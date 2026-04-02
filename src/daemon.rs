/// Long-lived daemon that owns the stdio pipes to an LSP server and exposes a
/// Unix domain socket for short-lived CLI invocations to connect to.
///
/// Architecture:
///
///   lsp-client hover ...
///         ↓ unix socket
///     lsp-client daemon         ← this module, long-lived
///         ↓ stdio pipes
///     rust-analyzer / ...
///
/// The daemon performs the initialize/initialized handshake once and caches
/// the server's capabilities.  Each CLI invocation connects to the socket,
/// receives the cached init response, makes its LSP calls, and disconnects —
/// the server process never sees the disconnect.
///
/// Request handling is sequential: the daemon handles one socket client at a
/// time.  This is correct for LSP (which is already sequential in practice)
/// and avoids needing an async runtime.

use crate::session_file::{unix_timestamp, SessionInfo};
use crate::transport::MessageReader;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// ---------------------------------------------------------------------------
// Framed write helper (Content-Length: N\r\n\r\n + body)
// ---------------------------------------------------------------------------

fn send_framed(writer: &mut dyn Write, value: &Value) -> io::Result<()> {
    let body = value.to_string();
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

// ---------------------------------------------------------------------------
// DaemonCore — owns the connection to the LSP server child process
// ---------------------------------------------------------------------------

struct DaemonCore {
    /// Write end of the server's stdin pipe.
    server_writer: Box<dyn Write + Send>,
    /// Messages arriving from the server's stdout (via a reader thread).
    server_rx: Receiver<Value>,
    next_id: u64,
    verbose: bool,
}

impl DaemonCore {
    fn send_raw(&mut self, msg: &Value) -> io::Result<()> {
        send_framed(self.server_writer.as_mut(), msg)
    }

    /// Send a JSON-RPC request to the server and return its response.
    /// Notifications and server-initiated requests that arrive while waiting
    /// are handled inline (acked or discarded).
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        self.send_raw(&msg).map_err(|e| format!("write to server: {e}"))?;
        if self.verbose {
            eprintln!("[DAEMON] >>> {method} (id={id})");
        }
        loop {
            let resp = self.server_rx.recv().map_err(|_| "server reader thread died")?;
            if let Some(m) = resp.get("method").and_then(|v| v.as_str()) {
                // Server-initiated request: ack it; notification: discard.
                if resp.get("id").is_some() {
                    if self.verbose {
                        eprintln!("[DAEMON] <<< server request: {m} — acking");
                    }
                    let ack = json!({"jsonrpc":"2.0","id":resp["id"],"result":null});
                    let _ = self.send_raw(&ack);
                } else if self.verbose {
                    eprintln!("[DAEMON] <<< notification: {m}");
                }
                continue;
            }
            if resp.get("id") == Some(&json!(id)) {
                if self.verbose {
                    eprintln!("[DAEMON] <<< response (id={id})");
                }
                return Ok(resp);
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> io::Result<()> {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":params});
        self.send_raw(&msg)
    }

    /// Drain server messages until the server goes quiet and all in-progress
    /// work tokens are complete.  Mirrors the logic in LspSession::wait_for_idle.
    fn wait_for_idle(&mut self) {
        let mut pending: HashSet<Value> = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut last_activity = Instant::now();

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let timeout = remaining.min(Duration::from_millis(500));
            match self.server_rx.recv_timeout(timeout) {
                Ok(msg) => {
                    last_activity = Instant::now();
                    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
                    match method {
                        "window/workDoneProgress/create" => {
                            if let Some(token) = msg["params"].get("token") {
                                pending.insert(token.clone());
                            }
                            if let Some(id) = msg.get("id") {
                                let ack = json!({"jsonrpc":"2.0","id":id,"result":null});
                                let _ = self.send_raw(&ack);
                            }
                            if self.verbose {
                                eprintln!(
                                    "[DAEMON] progress token registered (pending: {})",
                                    pending.len()
                                );
                            }
                        }
                        "$/progress" => {
                            let token = &msg["params"]["token"];
                            match msg["params"]["value"]["kind"].as_str().unwrap_or("") {
                                "begin" => {
                                    pending.insert(token.clone());
                                }
                                "end" => {
                                    pending.remove(token);
                                }
                                _ => {}
                            }
                            if self.verbose {
                                eprintln!(
                                    "[DAEMON] $/progress {} (pending: {})",
                                    msg["params"]["value"]["kind"]
                                        .as_str()
                                        .unwrap_or("?"),
                                    pending.len()
                                );
                            }
                        }
                        _ => {
                            if self.verbose && !method.is_empty() {
                                eprintln!("[DAEMON] notification: {method}");
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Break if server is quiet with no outstanding tokens, OR if tokens
                    // exist but the server has gone silent (server forgot to send "end").
                    if pending.is_empty() || last_activity.elapsed() >= Duration::from_secs(2) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-connection handler
// ---------------------------------------------------------------------------

/// Handle one connected CLI client.  Returns `true` if the daemon should stop
/// (server died or client sent the internal lsp-client/stop notification).
fn handle_connection(
    stream: UnixStream,
    core: &mut DaemonCore,
    init_result: &Value,
) -> bool {
    let mut reader = match stream.try_clone() {
        Ok(s) => MessageReader::new(s),
        Err(e) => {
            if core.verbose {
                eprintln!("[DAEMON] failed to clone socket: {e}");
            }
            return false;
        }
    };
    let mut writer = stream;

    loop {
        let msg = match reader.read_message() {
            Ok(m) => m,
            Err(_) => return false, // client disconnected
        };

        let method = msg.get("method").and_then(|v| v.as_str());
        let has_id = msg.get("id").is_some();

        if core.verbose {
            if let Some(m) = method {
                eprintln!("[DAEMON] client >>> {m}");
            }
        }

        match method {
            // ------------------------------------------------------------------
            // Intercepted lifecycle messages
            // ------------------------------------------------------------------
            Some("initialize") => {
                // Return the cached init result so the client's LspSession works
                // without modification.
                let resp =
                    json!({"jsonrpc":"2.0","id":msg["id"],"result":init_result});
                let _ = send_framed(&mut writer, &resp);
            }
            Some("initialized") => {
                // Already done — absorb silently (notification, no response).
            }
            Some("shutdown") => {
                // Keep the server alive; just satisfy the client's request.
                let resp = json!({"jsonrpc":"2.0","id":msg["id"],"result":null});
                let _ = send_framed(&mut writer, &resp);
            }
            Some("exit") => {
                // Client is done — close this connection, daemon keeps running.
                return false;
            }

            // ------------------------------------------------------------------
            // Internal control message
            // ------------------------------------------------------------------
            Some("lsp-client/stop") => {
                // Signal to the main loop that the daemon should shut down.
                return true;
            }

            // ------------------------------------------------------------------
            // Notifications (no id): forward to server, wait for it to settle.
            // ------------------------------------------------------------------
            Some(_) if !has_id => {
                if let Err(e) = core.send_raw(&msg) {
                    if core.verbose {
                        eprintln!("[DAEMON] write to server failed: {e}");
                    }
                    return true; // server dead
                }
                // Block until the server finishes any background work triggered
                // by this notification (e.g., indexing after didOpen/didChange).
                // The client is already waiting for its next response, so this
                // delay is invisible to it.
                core.wait_for_idle();
            }

            // ------------------------------------------------------------------
            // Requests: proxy to server, route response back.
            // ------------------------------------------------------------------
            _ => {
                if let Err(e) = core.send_raw(&msg) {
                    if core.verbose {
                        eprintln!("[DAEMON] write to server failed: {e}");
                    }
                    return true; // server dead
                }
                let req_id = msg.get("id").cloned().unwrap_or(Value::Null);
                loop {
                    let server_msg = match core.server_rx.recv() {
                        Ok(m) => m,
                        Err(_) => return true, // server reader thread died
                    };

                    if server_msg.get("method").is_some() {
                        // Server-initiated request: ack internally, never forward.
                        if server_msg.get("id").is_some() {
                            let ack = json!({
                                "jsonrpc": "2.0",
                                "id": server_msg["id"],
                                "result": null,
                            });
                            let _ = core.send_raw(&ack);
                        }
                        // Notifications from server while waiting: discard.
                        continue;
                    }

                    if server_msg.get("id") == Some(&req_id) {
                        let _ = send_framed(&mut writer, &server_msg);
                        break;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the daemon process.  This function blocks until the daemon shuts down
/// (idle timeout, server crash, or explicit stop request).
pub fn run_daemon(
    workspace: &str,
    server_bin: &str,
    server_args: &[&str],
    idle_secs: u64,
    verbose: bool,
    language_id: &str,
) -> Result<()> {
    // ---- Spawn the LSP server child process ---------------------------------

    let mut child: Child = Command::new(server_bin)
        .args(server_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                format!("'{server_bin}' not found in PATH")
            } else {
                format!("failed to spawn {server_bin}: {e}")
            }
        })?;

    let server_stdin = child.stdin.take().unwrap();
    let server_stdout = child.stdout.take().unwrap();

    // ---- Reader thread: server stdout → channel -----------------------------

    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        let mut reader = MessageReader::new(server_stdout);
        loop {
            match reader.read_message() {
                Ok(msg) => {
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut core = DaemonCore {
        server_writer: Box::new(server_stdin),
        server_rx: rx,
        next_id: 1,
        verbose,
    };

    // ---- LSP initialisation -------------------------------------------------

    let canonical_root = std::fs::canonicalize(workspace)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| workspace.to_owned());
    let root_uri = format!("file://{canonical_root}");
    let basename = std::path::Path::new(&canonical_root)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    if verbose {
        eprintln!("[DAEMON] initialising {server_bin} for {canonical_root}");
    }

    let init_resp = core.request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "hover":      {"contentFormat": ["plaintext", "markdown"]},
                    "completion": {"completionItem": {"snippetSupport": false}},
                    "definition": {},
                    "references": {},
                    "documentSymbol": {},
                    "diagnostic": {},
                    "formatting": {},
                    "signatureHelp": {
                        "signatureInformation": {
                            "parameterInformation": {"labelOffsetSupport": true}
                        },
                    },
                    "codeAction": {
                        "codeActionLiteralSupport": {
                            "codeActionKind": {
                                "valueSet": ["quickfix", "source.organizeImports"]
                            }
                        },
                    },
                    "rename": {},
                    "semanticTokens": {
                        "requests":       {"full": true, "range": true},
                        "tokenTypes":     [],
                        "tokenModifiers": [],
                        "formats":        ["relative"],
                    },
                    "inlayHint": {},
                },
                "workspace": {"symbol": {}},
            },
            "workspaceFolders": [{"uri": root_uri, "name": basename}],
        }),
    )?;

    core.notify("initialized", json!({}))?;
    core.wait_for_idle();

    let init_result = init_resp
        .get("result")
        .cloned()
        .unwrap_or(Value::Null);

    // ---- Unix domain socket -------------------------------------------------

    let socket_path = format!("/tmp/lsp-client-{}.sock", std::process::id());
    // Remove any leftover socket from a previous run with the same PID.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;
    listener.set_nonblocking(true)?;

    // ---- Write session file (signals to `start` that we are ready) ----------

    let info = SessionInfo {
        workspace: canonical_root.clone(),
        server: server_bin.to_owned(),
        language_id: language_id.to_owned(),
        pid: std::process::id(),
        socket: socket_path.clone(),
        started_at: unix_timestamp(),
        initialized: true,
    };
    info.save()?;

    eprintln!(
        "[DAEMON] ready — listening on {socket_path} (idle timeout: {}s)",
        idle_secs
    );

    // ---- Main accept loop ---------------------------------------------------

    let mut last_activity = Instant::now();
    let mut stop = false;

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // The listener is non-blocking (for the idle-timeout loop), but accepted
                // sockets inherit that flag on Unix.  Set back to blocking so that
                // read_message() in handle_connection works correctly.
                let _ = stream.set_nonblocking(false);
                last_activity = Instant::now();
                if verbose {
                    eprintln!("[DAEMON] client connected");
                }
                stop = handle_connection(stream, &mut core, &init_result);
                if verbose {
                    eprintln!("[DAEMON] client disconnected");
                }
                if stop {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if idle_secs > 0 && last_activity.elapsed().as_secs() >= idle_secs {
                    eprintln!("[DAEMON] idle timeout — shutting down");
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                eprintln!("[DAEMON] accept error: {e}");
                break;
            }
        }
    }

    // ---- Cleanup ------------------------------------------------------------

    SessionInfo::delete(&canonical_root);
    let _ = std::fs::remove_file(&socket_path);

    // Gracefully shut down the LSP server unless it already crashed.
    if !stop {
        let _ = core.request("shutdown", json!({}));
        let _ = core.notify("exit", json!({}));
    }

    // Give the server a moment to exit before we drop the pipes.
    std::thread::sleep(Duration::from_millis(500));
    let _ = child.kill();
    let _ = child.wait();

    eprintln!("[DAEMON] stopped");
    Ok(())
}
