mod config;
mod daemon;
mod format;
mod session;
mod session_file;
mod transport;

use clap::{Parser, Subcommand};
use format::*;
use session::{IdleResult, LspSession};
use session_file::{unix_timestamp, SessionInfo};
use transport::Transport;

/// LSP client CLI — connects to a language server and queries it.
///
/// Place a `.lsp-client.toml` in your project root to set `server`,
/// `language-id`, and other defaults so you don't need to repeat flags.
/// Run `session start` once to launch a persistent daemon; subsequent calls
/// auto-connect without any extra flags.
#[derive(Parser)]
#[command(name = "lsp-client", version)]
struct Cli {
    /// LSP server host (TCP mode)
    #[arg(long, default_value = "127.0.0.1", global = true)]
    host: String,

    /// LSP server port (TCP mode)
    #[arg(long, default_value = "9999", global = true)]
    port: u16,

    /// Project root directory (defaults to .lsp-client.toml location or CWD)
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
    daemon: bool,

    /// Deprecated: use --daemon
    #[arg(long, global = true, hide = true)]
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

    /// Wait for the server to finish indexing before querying (daemon: sends
    /// waitIdle; direct: uses a 5-minute timeout instead of the default 60s)
    #[arg(long, global = true)]
    wait_for_index: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run an LSP query against a file or workspace
    Query {
        #[command(subcommand)]
        command: QueryCommand,
    },
    /// Manage the persistent LSP session daemon
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// List capabilities reported by the server
    Capabilities,
}

#[derive(Subcommand)]
enum QueryCommand {
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
    /// Combined hover + definition + references + diagnostics for AI context
    Context {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Start a persistent LSP session daemon for this workspace.
    ///
    /// The daemon owns the server process and exposes a Unix socket so that
    /// subsequent calls avoid the server startup cost.  Uses --root as the
    /// workspace and --server as the binary to launch.
    Start {
        /// Seconds of inactivity before the daemon stops itself (0 = never)
        #[arg(long, default_value = "300")]
        idle_timeout: u64,
        /// Block until the server reports indexing complete before returning
        #[arg(long)]
        wait: bool,
        /// Open this file to trigger full workspace indexing while waiting
        #[arg(long)]
        wait_file: Option<String>,
    },

    /// Show status of the session daemon for this workspace.
    Status,

    /// Stop the running LSP session daemon for this workspace.
    Stop,

    /// Wait until the LSP server has finished indexing (no pending progress).
    ///
    /// Useful in scripts: `lsp-client session wait-ready && lsp-client query hover ...`
    WaitReady {
        /// Maximum time to wait (default: 60s)
        #[arg(long, default_value = "60s", value_parser = parse_duration)]
        wait_timeout: std::time::Duration,
    },

    /// Internal: run as the daemon process (spawned by `session start`).
    #[command(hide = true)]
    DaemonRun {
        #[arg(long, default_value = "300")]
        idle_timeout: u64,
    },
}

// ---------------------------------------------------------------------------
// Effective configuration (CLI args merged with .lsp-client.toml)
// ---------------------------------------------------------------------------

struct EffectiveConfig {
    root: String,
    server: Option<String>,
    language_id: Option<String>,
    no_server_stdio_flag: bool,
    timeout: Option<std::time::Duration>,
}

fn build_effective_config(cli: &Cli) -> EffectiveConfig {
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let search_start = cli.root.as_deref().unwrap_or(&cwd);

    let (config_dir, file_config) = config::find_config(search_start)
        .map(|(path, cfg)| {
            let dir = path.parent().unwrap().to_string_lossy().into_owned();
            (Some(dir), cfg)
        })
        .unwrap_or((None, config::FileConfig::default()));

    // CLI > config file `root` field > directory containing config file > CWD
    let root = cli
        .root
        .clone()
        .or_else(|| file_config.root.clone())
        .or(config_dir)
        .unwrap_or(cwd);

    let timeout = cli.timeout.or_else(|| {
        file_config
            .timeout
            .as_deref()
            .and_then(|s| parse_duration(s).ok())
    });

    EffectiveConfig {
        root,
        server: cli.server.clone().or(file_config.server),
        language_id: cli.language_id.clone().or(file_config.language_id),
        no_server_stdio_flag: cli.no_server_stdio_flag
            || file_config.no_server_stdio_flag.unwrap_or(false),
        timeout,
    }
}

fn main() {
    let cli = Cli::parse();
    let effective = build_effective_config(&cli);
    let root = &effective.root;

    // ---- Session management commands (no LSP session needed) ---------------

    if let Command::Session { command } = &cli.command {
        match command {
            SessionCommand::Start { idle_timeout, wait, wait_file } => {
                match start_daemon(&effective, cli.verbose, *idle_timeout) {
                    Ok(()) => eprintln!("lsp-client daemon started for {root}"),
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                }
                if *wait {
                    if let Some(info) = SessionInfo::load(root) {
                        let lang = effective
                            .language_id
                            .as_deref()
                            .unwrap_or(&info.language_id);
                        if let Err(e) = wait_for_daemon_ready(
                            &info.socket,
                            wait_file.as_deref(),
                            lang,
                            cli.verbose,
                            effective.timeout,
                        ) {
                            eprintln!("Warning: wait-for-idle failed: {e}");
                        } else {
                            eprintln!("lsp-client daemon ready (indexing complete) for {root}");
                        }
                    }
                }
                return;
            }
            SessionCommand::Status => {
                match SessionInfo::load(root) {
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
            SessionCommand::Stop => {
                match SessionInfo::load(root) {
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
                        while session_file::session_path(root).exists() {
                            if std::time::Instant::now() >= deadline {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                        // If still there, remove it ourselves.
                        SessionInfo::delete(root);
                        eprintln!("Done.");
                    }
                    Some(_) => {
                        SessionInfo::delete(root);
                        eprintln!("Removed stale session for {root}.");
                    }
                    None => {
                        eprintln!("No active daemon session for {root}.");
                    }
                }
                return;
            }
            SessionCommand::DaemonRun { idle_timeout } => {
                let server_args: &[&str] =
                    if effective.no_server_stdio_flag { &[] } else { &["--stdio"] };
                let server = effective.server.as_deref().unwrap_or_else(|| {
                    eprintln!("Error: --server is required");
                    std::process::exit(1);
                });
                let language_id = effective.language_id.as_deref().unwrap_or_else(|| {
                    eprintln!("Error: --language-id is required");
                    std::process::exit(1);
                });
                if let Err(e) = daemon::run_daemon(
                    root, server, server_args, *idle_timeout, cli.verbose, language_id,
                ) {
                    eprintln!("daemon error: {e}");
                    std::process::exit(1);
                }
                return;
            }
            SessionCommand::WaitReady { .. } => {} // handled below via LSP session
        }
    }

    // ---- Build transport ----------------------------------------------------

    // Load session info once — used for both auto-detect and language_id fallback.
    let session_info = SessionInfo::load(root);

    // `capabilities --server <bin>` means "query this specific server directly" —
    // bypass any running daemon so we don't return a different server's capabilities.
    let caps_direct =
        matches!(cli.command, Command::Capabilities) && effective.server.is_some();

    // Use daemon if:
    //   - explicitly requested (--daemon / deprecated --stdio)
    //   - a live daemon is already running for this root
    //   - a server binary is configured (auto-start)
    // …but not when we're doing a direct capabilities probe.
    let use_daemon = !caps_direct
        && (cli.daemon
            || cli.stdio
            || session_info.as_ref().map(|i| i.is_alive()).unwrap_or(false)
            || effective.server.is_some());

    let transport = if use_daemon {
        connect_or_start_daemon(&effective, cli.verbose, 300)
    } else {
        let server_bin = effective.server.as_deref().unwrap_or_else(|| {
            eprintln!("Error: --server is required (no active daemon found for {root})");
            std::process::exit(1);
        });
        if caps_direct {
            // Spawn the server directly over stdio for capabilities introspection.
            let args: &[&str] = if effective.no_server_stdio_flag { &[] } else { &["--stdio"] };
            Transport::stdio(server_bin, args)
        } else {
            Transport::tcp_with_autostart(&cli.host, cli.port, server_bin, cli.verbose)
        }
    };

    let transport = match transport {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    if let Some(dur) = effective.timeout {
        transport.set_read_timeout(Some(dur));
    }

    // Resolve language ID: explicit flag > session file > error.
    // WaitReady does not open files, so it doesn't require a language ID.
    let effective_language_id: String = effective
        .language_id
        .clone()
        .or_else(|| {
            session_info
                .as_ref()
                .map(|i| i.language_id.clone())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| {
            if matches!(
                cli.command,
                Command::Session { command: SessionCommand::WaitReady { .. } }
            ) {
                String::new()
            } else {
                eprintln!(
                    "Error: --language-id is required (or start a daemon first with --language-id)"
                );
                std::process::exit(1);
            }
        });

    let mut session = LspSession::new(transport, cli.verbose, effective.timeout);

    // --wait-for-index: use a longer timeout in direct TCP mode; in daemon mode
    // we send lsp-client/waitIdle after did_open (below).
    let did_open_timeout = if cli.wait_for_index && !use_daemon {
        Some(std::time::Duration::from_secs(300))
    } else {
        effective.timeout
    };

    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        // In daemon mode (not Capabilities): skip initialize — the daemon has
        // the cached result and re-sending it wastes a round trip.
        let init_resp = if use_daemon && !matches!(cli.command, Command::Capabilities) {
            serde_json::Value::Null
        } else {
            session.initialize(root)?
        };

        // WaitReady: send lsp-client/waitIdle and exit.
        if let Command::Session { command: SessionCommand::WaitReady { wait_timeout } } =
            &cli.command
        {
            if use_daemon {
                // Set a generous read timeout so we can wait a long time.
                session.transport.set_read_timeout(Some(*wait_timeout));
                session.wait_idle_request()?;
            } else {
                session.wait_for_index(*wait_timeout);
                if matches!(session.last_idle_result, Some(IdleResult::TimedOut)) {
                    eprintln!("Timed out waiting for server to become idle.");
                    std::process::exit(2);
                }
            }
            return Ok(());
        }

        let needs_open = !matches!(
            cli.command,
            Command::Query { command: QueryCommand::WorkspaceSymbols { file: None, .. } }
                | Command::Capabilities
                | Command::Session { command: SessionCommand::WaitReady { .. } }
        );

        if needs_open {
            let file = match &cli.command {
                Command::Query { command } => match command {
                    QueryCommand::Hover { file, .. }
                    | QueryCommand::Definition { file, .. }
                    | QueryCommand::References { file, .. }
                    | QueryCommand::Symbols { file }
                    | QueryCommand::Diagnostics { file }
                    | QueryCommand::Completion { file, .. }
                    | QueryCommand::SignatureHelp { file, .. }
                    | QueryCommand::CodeAction { file, .. }
                    | QueryCommand::Rename { file, .. }
                    | QueryCommand::SemanticTokens { file }
                    | QueryCommand::InlayHints { file, .. }
                    | QueryCommand::Context { file, .. } => file.clone(),
                    QueryCommand::WorkspaceSymbols { file: Some(f), .. } => f.clone(),
                    QueryCommand::WorkspaceSymbols { file: None, .. } => unreachable!(),
                },
                _ => unreachable!(),
            };
            let abs = abs_path(&file);
            // Temporarily override timeout for did_open if --wait-for-index is set.
            let saved_timeout = session.timeout;
            session.timeout = did_open_timeout;
            session.ensure_current(&abs, &effective_language_id)?;
            session.timeout = saved_timeout;
            // wait_for_idle (called inside did_open) sets transport timeout to
            // session.timeout; restore to the original configured value.
            if cli.wait_for_index && !use_daemon {
                session.transport.set_read_timeout(saved_timeout);
            }

            // In daemon mode with --wait-for-index: send waitIdle so the daemon
            // blocks until fully indexed before we query.
            if cli.wait_for_index && use_daemon {
                let wait_dur =
                    effective.timeout.unwrap_or(std::time::Duration::from_secs(300));
                session.transport.set_read_timeout(Some(wait_dur));
                session.wait_idle_request()?;
                // Restore the configured timeout for subsequent queries.
                session.transport.set_read_timeout(effective.timeout);
            }
        }

        match &cli.command {
            Command::Query { command } => match command {
                QueryCommand::Hover { file, line, col } => {
                    let abs = abs_path(file);
                    let resp = session.hover(&abs, line - 1, col - 1)?;
                    if cli.json { print_json(&resp) } else { println!("{}", format_hover(&resp)) }
                }
                QueryCommand::Definition { file, line, col } => {
                    let abs = abs_path(file);
                    let resp = session.definition(&abs, line - 1, col - 1)?;
                    if cli.json { print_json(&resp) } else { println!("{}", format_definition(&resp)) }
                }
                QueryCommand::References { file, line, col } => {
                    let abs = abs_path(file);
                    let resp = session.references(&abs, line - 1, col - 1)?;
                    let is_empty =
                        resp["result"].as_array().map(|a| a.is_empty()).unwrap_or(true);
                    // If empty and we know the server was still indexing, signal that
                    // rather than silently returning nothing.
                    if is_empty
                        && !use_daemon
                        && matches!(session.last_idle_result, Some(IdleResult::TimedOut))
                    {
                        if cli.json {
                            println!(
                                "{}",
                                serde_json::json!({"ok": false, "error": "server not ready", "indexing": true})
                            );
                        } else {
                            eprintln!(
                                "server not ready: still indexing (use --wait-for-index to wait)"
                            );
                        }
                        std::process::exit(2);
                    }
                    if cli.json { print_json(&resp) } else { println!("{}", format_references(&resp)) }
                }
                QueryCommand::Symbols { file } => {
                    let abs = abs_path(file);
                    let resp = session.document_symbols(&abs)?;
                    if cli.json { print_json(&resp) } else { println!("{}", format_symbols(&resp)) }
                }
                QueryCommand::WorkspaceSymbols { query, retries, .. } => {
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
                QueryCommand::Diagnostics { file } => {
                    let abs = abs_path(file);
                    let resp = session.diagnostics(&abs)?;
                    if cli.json { print_json(&resp) } else { println!("{}", format_diagnostics(&resp)) }
                }
                QueryCommand::Completion { file, line, col } => {
                    let abs = abs_path(file);
                    let resp = session.completion(&abs, line - 1, col - 1)?;
                    if cli.json { print_json(&resp) } else { println!("{}", format_completion(&resp)) }
                }
                QueryCommand::SignatureHelp { file, line, col } => {
                    let abs = abs_path(file);
                    let resp = session.signature_help(&abs, line - 1, col - 1)?;
                    if cli.json {
                        print_json(&resp)
                    } else {
                        println!("{}", format_signature_help(&resp))
                    }
                }
                QueryCommand::CodeAction { file, line, col } => {
                    let abs = abs_path(file);
                    let resp = session.code_action(&abs, line - 1, col - 1)?;
                    if cli.json {
                        print_json(&resp)
                    } else {
                        println!("{}", format_code_actions(&resp))
                    }
                }
                QueryCommand::Rename { file, line, col, new_name } => {
                    let abs = abs_path(file);
                    let resp = session.rename(&abs, line - 1, col - 1, new_name)?;
                    if cli.json { print_json(&resp) } else { println!("{}", format_rename(&resp)) }
                }
                QueryCommand::SemanticTokens { file } => {
                    let abs = abs_path(file);
                    let resp = session.semantic_tokens(&abs)?;
                    if cli.json {
                        print_json(&resp)
                    } else {
                        println!("{}", format_semantic_tokens(&resp))
                    }
                }
                QueryCommand::InlayHints { file, start_line, end_line } => {
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
                QueryCommand::Context { file, line, col } => {
                    let abs = abs_path(file);
                    let line0 = line - 1;
                    let col0 = col - 1;
                    let hover = session.hover(&abs, line0, col0).unwrap_or(serde_json::Value::Null);
                    let def = session.definition(&abs, line0, col0).unwrap_or(serde_json::Value::Null);
                    let refs = session.references(&abs, line0, col0).unwrap_or(serde_json::Value::Null);
                    let diag = session.diagnostics(&abs).unwrap_or(serde_json::Value::Null);
                    if cli.json {
                        println!("{}", serde_json::json!({
                            "hover": hover,
                            "definition": def,
                            "references": refs,
                            "diagnostics": diag,
                        }));
                    } else {
                        print!("{}", format_context(&hover, &def, &refs, &diag));
                    }
                }
            },
            Command::Capabilities => {
                let caps = &init_resp["result"]["capabilities"];
                if cli.json {
                    print_json(&init_resp)
                } else {
                    println!("{}", format_capabilities(caps))
                }
            }
            Command::Session { .. } => unreachable!(),
        }

        Ok(())
    })();

    // In daemon mode: just drop the session (EOF → daemon loops back to accept).
    // In direct mode: send shutdown + exit cleanly.
    if !use_daemon {
        session.shutdown();
    }

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
fn connect_or_start_daemon(
    effective: &EffectiveConfig,
    verbose: bool,
    idle_timeout: u64,
) -> transport::Result<Transport> {
    let root = &effective.root;
    // Try an existing live session first.
    if let Some(info) = SessionInfo::load(root) {
        if info.is_alive() {
            if verbose {
                eprintln!("[DEBUG] connecting to daemon at {}", info.socket);
            }
            return Transport::unix_socket(&info.socket);
        }
        // Stale entry — clean up and fall through to auto-start.
        if verbose {
            eprintln!("[DEBUG] removing stale daemon session");
        }
        SessionInfo::delete(root);
    }

    // Auto-start a new daemon and wait for it to signal readiness.
    if verbose {
        eprintln!("[DEBUG] auto-starting daemon for {root}");
    }
    start_daemon(effective, verbose, idle_timeout)
        .map_err(|e| format!("failed to start daemon: {e}"))?;

    match SessionInfo::load(root) {
        Some(info) => Transport::unix_socket(&info.socket),
        None => Err("daemon started but session file not found".into()),
    }
}

#[cfg(not(unix))]
fn connect_or_start_daemon(
    effective: &EffectiveConfig,
    _verbose: bool,
    _idle_timeout: u64,
) -> transport::Result<Transport> {
    let server = effective.server.as_deref().ok_or("--server is required")?;
    let args: &[&str] = if effective.no_server_stdio_flag { &[] } else { &["--stdio"] };
    Transport::stdio(server, args)
}

/// Spawn `lsp-client session daemon-run` as a background process and block
/// until the session file appears (daemon has finished initialising), or 30 s
/// elapse.
fn start_daemon(
    effective: &EffectiveConfig,
    verbose: bool,
    idle_timeout: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Stdio;

    let server = effective
        .server
        .as_deref()
        .ok_or("--server is required to start a daemon")?;
    let language_id = effective
        .language_id
        .as_deref()
        .ok_or("--language-id is required to start a daemon")?;

    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);

    // Global flags must come before the subcommand.
    cmd.arg("--root").arg(&effective.root);
    cmd.arg("--server").arg(server);
    cmd.arg("--language-id").arg(language_id);
    if effective.no_server_stdio_flag {
        cmd.arg("--no-server-stdio-flag");
    }
    if verbose {
        cmd.arg("--verbose");
    }
    cmd.arg("session");
    cmd.arg("daemon-run");
    cmd.arg("--idle-timeout").arg(idle_timeout.to_string());

    // Daemon's stdin/stdout are irrelevant; stderr visible only in verbose mode.
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(if verbose { Stdio::inherit() } else { Stdio::null() });

    cmd.spawn()?;

    // Poll for the session file to appear — this means the daemon has
    // successfully initialised the LSP server and is ready to accept clients.
    let root = &effective.root;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if SessionInfo::load(root).map(|i| i.initialized).unwrap_or(false) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "daemon did not become ready within 30 s (check '{} --verbose session start')",
                exe.display()
            )
            .into());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Connect to the daemon, optionally open a file to trigger indexing, then
/// send `lsp-client/waitIdle` to block until the server is fully idle.
#[cfg(unix)]
fn wait_for_daemon_ready(
    socket: &str,
    wait_file: Option<&str>,
    language_id: &str,
    verbose: bool,
    timeout: Option<std::time::Duration>,
) -> Result<(), Box<dyn std::error::Error>> {
    let transport = Transport::unix_socket(socket)?;
    if let Some(dur) = timeout.or(Some(std::time::Duration::from_secs(300))) {
        transport.set_read_timeout(Some(dur));
    }
    let mut session = LspSession::new(transport, verbose, timeout);
    // No initialize needed — daemon has the cached result.
    if let Some(f) = wait_file {
        let abs = abs_path(f);
        session.did_open(&abs, language_id)?;
    }
    session.wait_idle_request()?;
    Ok(())
}

#[cfg(not(unix))]
fn wait_for_daemon_ready(
    _socket: &str,
    _wait_file: Option<&str>,
    _language_id: &str,
    _verbose: bool,
    _timeout: Option<std::time::Duration>,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("Unix sockets not available on this platform".into())
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
