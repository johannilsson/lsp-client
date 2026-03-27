mod format;
mod session;
mod transport;

use clap::{Parser, Subcommand};
use format::*;
use session::LspSession;
use transport::Transport;

/// LSP client CLI — connects to a language server and queries it.
///
/// Connects over TCP by default (with auto-start of kotlin-lsp if not running).
/// Use --stdio to spawn the server as a child process instead.
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

    /// Language ID sent to the server in textDocument/didOpen
    #[arg(long, default_value = "kotlin", global = true)]
    language_id: String,

    /// Server binary to launch (auto-start or stdio)
    #[arg(long, default_value = "kotlin-lsp", global = true)]
    server: String,

    /// Use stdio transport: spawn the server as a child process
    #[arg(long, global = true)]
    stdio: bool,

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
    WorkspaceSymbols { query: String },
    /// Get errors and warnings for a file
    Diagnostics { file: String },
    /// Get completions at a position
    Completion {
        file: String,
        line: u32,
        col: u32,
    },
}

fn main() {
    let cli = Cli::parse();

    let root = cli
        .root
        .unwrap_or_else(|| std::env::current_dir().unwrap().to_string_lossy().into_owned());

    let transport = if cli.stdio {
        Transport::stdio(&cli.server, &[])
    } else {
        Transport::tcp_with_autostart(&cli.host, cli.port, &cli.server, cli.verbose)
    };

    let transport = match transport {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    let mut session = LspSession::new(transport, cli.verbose);

    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        session.initialize(&root)?;

        let needs_open = !matches!(cli.command, Command::WorkspaceSymbols { .. });

        if needs_open {
            let file = match &cli.command {
                Command::Hover { file, .. }
                | Command::Definition { file, .. }
                | Command::References { file, .. }
                | Command::Symbols { file }
                | Command::Diagnostics { file }
                | Command::Completion { file, .. } => file.clone(),
                Command::WorkspaceSymbols { .. } => unreachable!(),
            };
            let abs = abs_path(&file);
            session.did_open(&abs, &cli.language_id)?;
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
            Command::WorkspaceSymbols { query } => {
                let resp = session.workspace_symbols(query)?;
                if cli.json { print_json(&resp) } else { println!("{}", format_workspace_symbols(&resp)) }
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

fn abs_path(file: &str) -> String {
    std::fs::canonicalize(file)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_owned())
}

fn print_json(resp: &serde_json::Value) {
    let out = serde_json::json!({
        "ok": resp.get("error").is_none(),
        "result": resp.get("result"),
        "error": resp.get("error"),
    });
    println!("{}", out);
}
