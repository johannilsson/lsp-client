mod daemon;
mod format;
mod session;
mod session_file;
mod transport;

use clap::{Parser, Subcommand};
use format::*;
use session::LspSession;
use session_file::{unix_timestamp, SessionInfo};
use transport::Transport;

/// LSP client CLI — connects to a language server and queries it.
///
/// Connects over TCP by default. Use `start` to launch a persistent daemon
/// that pays the server startup cost only once; subsequent calls auto-connect.
#[derive(Parser)]
#[command(name = "lsp-client", version)]
struct Cli {
    /// LSP server host (TCP mode)
    #[arg(long, default_value = "127.0.0.1", global = true)]
    host: String,

    /// LSP server port (TCP mode)
    #[arg(long, default_value = "9999", global = true)]
    port: u16,

    /// Project root directory (defaults to current working directory)
    #[arg(long, global = true)]
    root: Option<String>,

    /// Language ID sent to the server in textDocument/didOpen (e.g. kotlin, swift, rust)
    #[arg(long, global = true)]
    language_id: Option<String>,

    /// Server binary to launch (e.g. kotlin-lsp, sourcekit-lsp, rust-analyzer)
    #[arg(long, global = true)]
    server: Option<String>,

    /// Force use of a session daemon even if none is auto-detected
    #[arg(long, global = true)]
    stdio: bool,

    /// Do not pass --stdio to the server process (e.g. sourcekit-lsp uses stdio by default)
    #[arg(long, global = true)]
    no_server_stdio_flag: bool,

    /// Maximum time to wait for a response (e.g. 10s, 2m). Default: no timeout.
    #[arg(long, global = true, value_parser = parse_duration)]
    timeout: Option<std::time::Duration>,

    /// Output raw LSP result as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Enable verbose debug logging to stderr
    #[arg(long, short, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Get type/doc info at a position
    Hover {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
    },
    /// Jump to definition
    Definition {
        file: String,
        line: u32,
        col: u32,
    },
    /// Find all references
    References {
        file: String,
        line: u32,
        col: u32,
    },
    /// List all symbols in a file
    Symbols { file: String },
    /// Search symbols across the workspace
    WorkspaceSymbols {
        query: String,
        /// Open this file first to trigger workspace indexing
        #[arg(long)]
        file: Option<String>,
        /// Retry up to N times if result is empty (waits 1s between attempts)
        #[arg(long, default_value = "10")]
        retries: u32,
    },
    /// Get errors and warnings for a file
    Diagnostics { file: String },
    /// Get completions at a position
    Completion {
        file: String,
        line: u32,
        col: u32,
    },
    /// Get function signature help at a position
    SignatureHelp {
        file: String,
        line: u32,
        col: u32,
    },
    /// Get available code actions at a position (quick fixes, refactors)
    CodeAction {
        file: String,
        line: u32,
        col: u32,
    },
    /// Rename a symbol across the workspace
    Rename {
        file: String,
        line: u32,
        col: u32,
        new_name: String,
    },
    /// Get semantic tokens for a file
    SemanticTokens { file: String },
    /// List capabilities reported by the server
    Capabilities,
    /// Get inlay hints for a file or line range
    InlayHints {
        file: String,
        /// First line to include hints for (1-based, default: 1)
        #[arg(long, default_value = "1")]
        start_line: u32,
        /// Last line to include hints for (1-based, default: end of file)
        #[arg(long)]
        end_line: Option<u32>,
    },

    // ------------------------------------------------------------------
    // Session management
    // ------------------------------------------------------------------

    /// Start a persistent LSP session daemon for this workspace.
    ///
    /// The daemon owns the server process and exposes a Unix socket so that
    /// subsequent calls avoid the server startup cost.  Uses --root as the
    /// workspace and --server as the binary to launch.
    Start {
        /// Seconds of inactivity before the daemon stops itself (0 = never)
        #[arg(long, default_value = "300")]
        idle_timeout: u64,
    },

    /// Show status of the session daemon for this workspace.
    Status,

    /// Stop the running LSP session daemon for this workspace.
    Stop,

    /// Internal: run as the daemon process (spawned by `start`).
    #[command(hide = true)]
    DaemonRun {
        #[arg(long, default_value = "300")]
        idle_timeout: u64,
    },
}

fn main() {
    let cli = Cli::parse();

    let root = cli
        .root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap().to_string_lossy().into_owned());

    // ---- Session management commands (no LSP session needed) ---------------

    match &cli.command {
        Command::Start { idle_timeout } => {
            match start_daemon(&cli, &root, *idle_timeout) {
                Ok(()) => eprintln!("lsp-client daemon started for {root}"),
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Command::Status => {
            match SessionInfo::load(&root) {
                Some(info) if info.is_alive() => {
                    let uptime =
                        format_uptime(unix_timestamp().saturating_sub(info.started_at));
                    if cli.json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "status":      "running",
                                "pid":         info.pid,
                                "socket":      info.socket,
                                "server":      info.server,
                                "language_id": info.language_id,
                                "workspace":   info.workspace,
                                "uptime":      uptime,
                            })
                        );
                    } else {
                        println!("status:      running");
                        println!("pid:         {}", info.pid);
                        println!("socket:      {}", info.socket);
                        println!("server:      {}", info.server);
                        println!("language_id: {}", info.language_id);
                        println!("uptime:      {uptime}");
                    }
                }
                Some(_) => {
                    if cli.json {
                        println!("{}", serde_json::json!({"status": "dead"}));
                    } else {
                        eprintln!("status:      dead (stale session)");
                    }
                    std::process::exit(1);
                }
                None => {
                    if cli.json {
                        println!("{}", serde_json::json!({"status": "not running"}));
                    } else {
                        eprintln!("status:      not running");
                    }
                    std::process::exit(1);
                }
            }
            return;
        }
        Command::Stop => {
            match SessionInfo::load(&root) {
                Some(info) if info.is_alive() => {
                    eprintln!("Stopping lsp-client daemon (PID {})...", info.pid);
                    // Ask the daemon to stop cleanly via its control message.
                    // Fall back to SIGTERM if we can't connect.
                    if send_stop_to_daemon(&info.socket, cli.verbose).is_err() {
                        let _ = std::process::Command::new("kill")
                            .arg(info.pid.to_string())
                            .status();
                    }
                    // Wait for session file to disappear (daemon cleaned up).
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_secs(10);
                    while session_file::session_path(&root).exists() {
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    // If still there, remove it ourselves.
                    SessionInfo::delete(&root);
                    eprintln!("Done.");
                }
                Some(_) => {
                    SessionInfo::delete(&root);
                    eprintln!("Removed stale session for {root}.");
                }
                None => {
                    eprintln!("No active daemon session for {root}.");
                }
            }
            return;
        }
        Command::DaemonRun { idle_timeout } => {
            let server_args: &[&str] =
                if cli.no_server_stdio_flag { &[] } else { &["--stdio"] };
            let server = cli.server.as_deref().unwrap_or_else(|| {
                eprintln!("Error: --server is required");
                std::process::exit(1);
            });
            let language_id = cli.language_id.as_deref().unwrap_or_else(|| {
                eprintln!("Error: --language-id is required");
                std::process::exit(1);
            });
            if let Err(e) = daemon::run_daemon(
                &root, server, server_args, *idle_timeout, cli.verbose, language_id,
            ) {
                eprintln!("daemon error: {e}");
                std::process::exit(1);
            }
            return;
        }
        _ => {}
    }

    // ---- Build transport ----------------------------------------------------

    // Load session info once — used for both auto-detect and language_id fallback.
    let session_info = SessionInfo::load(&root);
    let use_daemon =
        cli.stdio || session_info.as_ref().map(|i| i.is_alive()).unwrap_or(false);

    let transport = if use_daemon {
        connect_or_start_daemon(&cli, &root)
    } else {
        let server_bin = cli.server.as_deref().unwrap_or_else(|| {
            eprintln!("Error: --server is required (no active daemon found for {root})");
            std::process::exit(1);
        });
        Transport::tcp_with_autostart(&cli.host, cli.port, server_bin, cli.verbose)
    };

    let transport = match transport {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    if let Some(dur) = cli.timeout {
        transport.set_read_timeout(Some(dur));
    }

    // Resolve language ID: explicit flag > session file > error.
    let effective_language_id: String = cli.language_id.clone()
        .or_else(|| session_info.as_ref().map(|i| i.language_id.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| {
            eprintln!("Error: --language-id is required (or start a daemon first with --language-id)");
            std::process::exit(1);
        });

    let mut session = LspSession::new(transport, cli.verbose, cli.timeout);

    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let init_resp = session.initialize(&root)?;

        let needs_open = !matches!(
            cli.command,
            Command::WorkspaceSymbols { file: None, .. } | Command::Capabilities
        );

        if needs_open {
            let file = match &cli.command {
                Command::Hover { file, .. }
                | Command::Definition { file, .. }
                | Command::References { file, .. }
                | Command::Symbols { file }
                | Command::Diagnostics { file }
                | Command::Completion { file, .. }
                | Command::SignatureHelp { file, .. }
                | Command::CodeAction { file, .. }
                | Command::Rename { file, .. }
                | Command::SemanticTokens { file }
                | Command::InlayHints { file, .. } => file.clone(),
                Command::Capabilities => unreachable!(),
                Command::WorkspaceSymbols { file: Some(f), .. } => f.clone(),
                Command::WorkspaceSymbols { file: None, .. } => unreachable!(),
                Command::Start { .. }
                | Command::Status
                | Command::Stop
                | Command::DaemonRun { .. } => unreachable!(),
            };
            let abs = abs_path(&file);
            session.did_open(&abs, &effective_language_id)?;
        }

        match &cli.command {
            Command::Hover { file, line, col } => {
                let abs = abs_path(file);
                let resp = session.hover(&abs, line - 1, col - 1)?;
                if cli.json { print_json(&resp) } else { println!("{}", format_hover(&resp)) }
            }
            Command::Definition { file, line, col } => {
                let abs = abs_path(file);
                let resp = session.definition(&abs, line - 1, col - 1)?;
                if cli.json { print_json(&resp) } else { println!("{}", format_definition(&resp)) }
            }
            Command::References { file, line, col } => {
                let abs = abs_path(file);
                let resp = session.references(&abs, line - 1, col - 1)?;
                if cli.json { print_json(&resp) } else { println!("{}", format_references(&resp)) }
            }
            Command::Symbols { file } => {
                let abs = abs_path(file);
                let resp = session.document_symbols(&abs)?;
                if cli.json { print_json(&resp) } else { println!("{}", format_symbols(&resp)) }
            }
            Command::WorkspaceSymbols { query, retries, .. } => {
                let mut resp = session.workspace_symbols(query)?;
                for _ in 0..*retries {
                    let is_empty =
                        resp["result"].as_array().map(|a| a.is_empty()).unwrap_or(true);
                    if !is_empty {
                        break;
                    }
                    if cli.verbose {
                        eprintln!("[DEBUG] empty result, retrying...");
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    resp = session.workspace_symbols(query)?;
                }
                if cli.json {
                    print_json(&resp)
                } else {
                    println!("{}", format_workspace_symbols(&resp))
                }
            }
            Command::Diagnostics { file } => {
                let abs = abs_path(file);
                let resp = session.diagnostics(&abs)?;
                if cli.json { print_json(&resp) } else { println!("{}", format_diagnostics(&resp)) }
            }
            Command::Completion { file, line, col } => {
                let abs = abs_path(file);
                let resp = session.completion(&abs, line - 1, col - 1)?;
                if cli.json { print_json(&resp) } else { println!("{}", format_completion(&resp)) }
            }
            Command::Capabilities => {
                let caps = &init_resp["result"]["capabilities"];
                if cli.json {
                    print_json(&init_resp)
                } else {
                    println!("{}", format_capabilities(caps))
                }
            }
            Command::SignatureHelp { file, line, col } => {
                let abs = abs_path(file);
                let resp = session.signature_help(&abs, line - 1, col - 1)?;
                if cli.json {
                    print_json(&resp)
                } else {
                    println!("{}", format_signature_help(&resp))
                }
            }
            Command::CodeAction { file, line, col } => {
                let abs = abs_path(file);
                let resp = session.code_action(&abs, line - 1, col - 1)?;
                if cli.json {
                    print_json(&resp)
                } else {
                    println!("{}", format_code_actions(&resp))
                }
            }
            Command::Rename { file, line, col, new_name } => {
                let abs = abs_path(file);
                let resp = session.rename(&abs, line - 1, col - 1, new_name)?;
                if cli.json { print_json(&resp) } else { println!("{}", format_rename(&resp)) }
            }
            Command::SemanticTokens { file } => {
                let abs = abs_path(file);
                let resp = session.semantic_tokens(&abs)?;
                if cli.json {
                    print_json(&resp)
                } else {
                    println!("{}", format_semantic_tokens(&resp))
                }
            }
            Command::InlayHints { file, start_line, end_line } => {
                let abs = abs_path(file);
                let end = end_line.unwrap_or_else(|| {
                    std::fs::read_to_string(&abs)
                        .map(|s| s.lines().count() as u32)
                        .unwrap_or(u32::MAX / 2)
                });
                let resp = session.inlay_hints(&abs, start_line - 1, end)?;
                if cli.json {
                    print_json(&resp)
                } else {
                    println!("{}", format_inlay_hints(&resp))
                }
            }
            Command::Start { .. }
            | Command::Status
            | Command::Stop
            | Command::DaemonRun { .. } => unreachable!(),
        }

        Ok(())
    })();

    session.shutdown();

    if let Err(e) = result {
        if cli.json {
            println!("{{\"ok\":false,\"error\":{}}}", serde_json::json!(e.to_string()));
        } else {
            eprintln!("LSP error: {e}");
        }
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Daemon helpers
// ---------------------------------------------------------------------------

/// Connect to an existing daemon for `root`, or auto-start one if none is
/// running.  Falls back to a direct stdio spawn if starting the daemon fails
/// (e.g., on non-Unix platforms).
#[cfg(unix)]
fn connect_or_start_daemon(cli: &Cli, root: &str) -> transport::Result<Transport> {
    // Try an existing live session first.
    if let Some(info) = SessionInfo::load(root) {
        if info.is_alive() {
            if cli.verbose {
                eprintln!("[DEBUG] connecting to daemon at {}", info.socket);
            }
            return Transport::unix_socket(&info.socket);
        }
        // Stale entry — clean up and fall through to auto-start.
        if cli.verbose {
            eprintln!("[DEBUG] removing stale daemon session");
        }
        SessionInfo::delete(root);
    }

    // Auto-start a new daemon and wait for it to signal readiness.
    if cli.verbose {
        eprintln!("[DEBUG] auto-starting daemon for {root}");
    }
    start_daemon(cli, root, 300).map_err(|e| format!("failed to start daemon: {e}"))?;

    match SessionInfo::load(root) {
        Some(info) => Transport::unix_socket(&info.socket),
        None => Err("daemon started but session file not found".into()),
    }
}

#[cfg(not(unix))]
fn connect_or_start_daemon(cli: &Cli, root: &str) -> transport::Result<Transport> {
    let _ = root;
    // No Unix socket support — fall back to spawning directly.
    let server = cli.server.as_deref().ok_or("--server is required")?;
    let args: &[&str] = if cli.no_server_stdio_flag { &[] } else { &["--stdio"] };
    Transport::stdio(server, args)
}

/// Spawn `lsp-client daemon-run` as a background process and block until the
/// session file appears (daemon has finished initialising), or 30 s elapse.
fn start_daemon(
    cli: &Cli,
    root: &str,
    idle_timeout: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Stdio;

    let server = cli.server.as_deref().ok_or("--server is required to start a daemon")?;
    let language_id =
        cli.language_id.as_deref().ok_or("--language-id is required to start a daemon")?;

    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);

    // Global flags must come before the subcommand.
    cmd.arg("--root").arg(root);
    cmd.arg("--server").arg(server);
    cmd.arg("--language-id").arg(language_id);
    if cli.no_server_stdio_flag {
        cmd.arg("--no-server-stdio-flag");
    }
    if cli.verbose {
        cmd.arg("--verbose");
    }
    cmd.arg("daemon-run");
    cmd.arg("--idle-timeout").arg(idle_timeout.to_string());

    // Daemon's stdin/stdout are irrelevant; stderr visible only in verbose mode.
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(if cli.verbose { Stdio::inherit() } else { Stdio::null() });

    cmd.spawn()?;

    // Poll for the session file to appear — this means the daemon has
    // successfully initialised the LSP server and is ready to accept clients.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if SessionInfo::load(root).map(|i| i.initialized).unwrap_or(false) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "daemon did not become ready within 30 s (check '{} --verbose start')",
                exe.display()
            )
            .into());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Connect to the daemon socket and send the internal `lsp-client/stop`
/// notification to ask it to shut down gracefully.
#[cfg(unix)]
fn send_stop_to_daemon(
    socket_path: &str,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)?;
    // We bypass LspSession here and write a raw notification directly.
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method":  "lsp-client/stop",
        "params":  {},
    })
    .to_string();
    write!(stream, "Content-Length: {}\r\n\r\n", body.len())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;
    if verbose {
        eprintln!("[DEBUG] sent stop notification to daemon");
    }
    Ok(())
}

#[cfg(not(unix))]
fn send_stop_to_daemon(_: &str, _: bool) -> Result<(), Box<dyn std::error::Error>> {
    Err("Unix sockets not available on this platform".into())
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn abs_path(file: &str) -> String {
    std::fs::canonicalize(file)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_owned())
}

fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    if let Some(n) = s.strip_suffix('s') {
        n.parse::<u64>().map(std::time::Duration::from_secs).map_err(|e| e.to_string())
    } else if let Some(n) = s.strip_suffix('m') {
        n.parse::<u64>()
            .map(|m| std::time::Duration::from_secs(m * 60))
            .map_err(|e| e.to_string())
    } else if let Some(n) = s.strip_suffix('h') {
        n.parse::<u64>()
            .map(|h| std::time::Duration::from_secs(h * 3600))
            .map_err(|e| e.to_string())
    } else {
        s.parse::<u64>()
            .map(std::time::Duration::from_secs)
            .map_err(|_| format!("invalid duration '{s}': use e.g. 10s, 2m, 1h"))
    }
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn print_json(resp: &serde_json::Value) {
    let out = serde_json::json!({
        "ok":     resp.get("error").is_none(),
        "result": resp.get("result"),
        "error":  resp.get("error"),
    });
    println!("{}", out);
}
