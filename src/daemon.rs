//! Long-lived daemon that owns the stdio pipes to an LSP server and exposes a
//! Unix domain socket for short-lived CLI invocations to connect to.
//!
//! Architecture:
//!
//!   lsp-client hover ...
//!         ↓ unix socket
//!     lsp-client daemon         ← this module, long-lived
//!         ↓ stdio pipes
//!     rust-analyzer / ...
//!
//! The daemon performs the initialize/initialized handshake once and caches
//! the server's capabilities.  Each CLI invocation connects to the socket,
//! receives the cached init response, makes its LSP calls, and disconnects —
//! the server process never sees the disconnect.
//!
//! Each socket client gets its own thread, but the conversation with the
//! server stays sequential: a client holds the server lock for one message at
//! a time (a request and its response, or a notification and the settle after
//! it), so every response goes back to the client that asked.  Control
//! messages that do not need the server (`lsp-client/status`,
//! `lsp-client/stop`) are answered without waiting for that lock.

use crate::servers::{self, ServerFacts, ServerProfile};
use crate::session_file::{unix_timestamp, SessionInfo};
use crate::transport::MessageReader;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
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
    /// Tracks URIs the server has opened and their current document version.
    /// Used to convert a repeat textDocument/didOpen into textDocument/didChange
    /// so the server sees updated content without a protocol violation.
    opened_versions: HashMap<String, u32>,
    /// `$/progress` tokens that have begun (or been created) but not ended.
    /// Kept across calls: servers such as kotlin-lsp run a Gradle import that
    /// can stay silent for minutes while its token is still open.
    pending_progress: HashSet<Value>,
    /// Server-specific behaviour (see `servers`). Generic until the server has
    /// answered `initialize` and told us what it is.
    profile: Box<dyn ServerProfile>,
    /// What the profile has learned about the server, e.g. that it is importing a
    /// project. Stays empty for servers whose profile knows nothing to record.
    facts: ServerFacts,
    /// Notifications that arrived before the profile was chosen, to hand to it once
    /// it is. `None` once that has happened.
    early: Option<Vec<Value>>,
    /// Answer for the server's "which build tool?" question (`--build-tool`), for the
    /// profiles whose server asks one.
    build_tool: Option<String>,
    /// Copy of the import state for `lsp-client/status`, which must be answered
    /// while another client holds the server lock.
    status: Arc<Mutex<StatusSnapshot>>,
}

/// What `lsp-client/status` reports about the import, kept up to date by
/// [`DaemonCore::publish_status`].
#[derive(Default, Clone)]
struct StatusSnapshot {
    import_state: &'static str,
    import_problems: Vec<String>,
}

/// Lock a mutex even if a thread panicked while holding it: the daemon's state
/// stays usable, and one failed client must not stop the others.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Answer to a server-initiated request that no profile answered.
/// `workspace/configuration` needs one entry per requested item; everything else
/// (`window/workDoneProgress/create`, `client/registerCapability`, ...) takes `null`.
fn server_request_result(method: &str, params: &Value) -> Value {
    match method {
        "workspace/configuration" => {
            let n = params["items"].as_array().map_or(0, |a| a.len());
            Value::Array(vec![Value::Null; n])
        }
        _ => Value::Null,
    }
}

impl DaemonCore {
    fn send_raw(&mut self, msg: &Value) -> io::Result<()> {
        send_framed(self.server_writer.as_mut(), msg)
    }

    /// Switch to `profile` (chosen after `initialize`), and let it see the
    /// notifications the server sent before we knew which profile to use.
    fn use_profile(&mut self, profile: Box<dyn ServerProfile>) {
        self.profile = profile;
        for msg in self.early.take().unwrap_or_default() {
            self.profile.on_message(&msg, &mut self.facts);
        }
        self.publish_status();
    }

    fn publish_status(&self) {
        *lock(&self.status) = StatusSnapshot {
            import_state: self.facts.import.state(),
            import_problems: self.facts.import.failures.clone(),
        };
    }

    /// Notes for `lsp-client/waitIdle` that are not failures.
    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        // --build-tool only answers a question; if an import finished without one,
        // the server chose by itself, possibly from an answer it remembered (#38).
        if let Some(tool) = &self.build_tool
            && self.facts.import.finished
            && !self.facts.build_tool_asked
        {
            notes.push(format!(
                "--build-tool {tool} was not used: the server did not ask which build tool to import with. \
                 It found only one, or it is reusing the answer it remembered for this workspace."
            ));
        }
        notes
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
            if self.observe(&resp) {
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

    /// Handle a server-initiated message (request or notification): ack
    /// requests and track `$/progress` state.  Returns `true` if `msg` was a
    /// server-initiated message, `false` if it is a response to one of ours.
    fn observe(&mut self, msg: &Value) -> bool {
        let Some(method) = msg.get("method").and_then(|v| v.as_str()) else {
            return false;
        };
        if let Some(early) = &mut self.early
            && msg.get("id").is_none()
        {
            early.push(msg.clone());
        }
        self.profile.on_message(msg, &mut self.facts);
        self.publish_status();
        match method {
            // A created token only counts once it begins: kotlin-lsp creates
            // tokens and then cancels the create request without using them.
            "window/workDoneProgress/create" => {}
            "$/progress" => {
                let token = &msg["params"]["token"];
                let kind = msg["params"]["value"]["kind"].as_str().unwrap_or("");
                match kind {
                    "begin" => {
                        self.pending_progress.insert(token.clone());
                    }
                    "end" => {
                        self.pending_progress.remove(token);
                    }
                    _ => {}
                }
                if self.verbose {
                    let v = &msg["params"]["value"];
                    let text = v["title"].as_str().or_else(|| v["message"].as_str()).unwrap_or("");
                    let pct = v["percentage"].as_u64().map(|p| format!(" {p}%")).unwrap_or_default();
                    eprintln!(
                        "[DAEMON] $/progress {kind}{pct} {text} (pending: {})",
                        self.pending_progress.len()
                    );
                }
            }
            _ => {
                if self.verbose {
                    let params = msg["params"].to_string();
                    let cut = params.char_indices().nth(400).map_or(params.len(), |(i, _)| i);
                    eprintln!("[DAEMON] <<< {method} {}", &params[..cut]);
                }
            }
        }
        if let Some(id) = msg.get("id") {
            let result = self
                .profile
                .answer_request(method, &msg["params"], self.build_tool.as_deref())
                .unwrap_or_else(|| server_request_result(method, &msg["params"]));
            if self.verbose && method == "window/showMessageRequest" {
                eprintln!("[DAEMON] answering {method} with {result}");
            }
            let ack = json!({"jsonrpc":"2.0","id":id,"result":result});
            let _ = self.send_raw(&ack);
        }
        true
    }

    /// Drain server messages until the server is idle, or `max_wait` elapses.
    ///
    /// `strict == false` ("settle"): return once the server has been quiet for
    /// 500 ms with no open progress tokens, or quiet for 2 s even with open
    /// tokens.  Used after didOpen/didChange so queries are not held up for the
    /// whole of a long background import.
    ///
    /// `strict == true`: return only once every progress token has ended and
    /// the server has been quiet for 500 ms, or, if it leaves a token open, quiet
    /// for 2 s, unless it announced a project import that is still running.
    /// Used by `lsp-client/waitIdle`.
    ///
    /// Returns `true` if idle was reached, `false` on timeout.
    fn wait_for_idle(&mut self, max_wait: Duration, strict: bool) -> bool {
        let deadline = Instant::now() + max_wait;
        let mut last_activity = Instant::now();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match self.server_rx.recv_timeout(remaining.min(Duration::from_millis(500))) {
                Ok(msg) => {
                    last_activity = Instant::now();
                    self.observe(&msg);
                }
                Err(RecvTimeoutError::Timeout) => {
                    let quiet = last_activity.elapsed();
                    let import_running = self.facts.import.expected && !self.facts.import.finished;
                    let idle = self.pending_progress.is_empty()
                        && !import_running
                        && quiet >= Duration::from_millis(500);
                    // Some servers never end a progress token, so a server that has gone
                    // quiet for 2 s counts as done even with one still open.  The one
                    // exception is a project import the server announced and has not
                    // finished (kotlin-lsp's Gradle sync): that is waited for, however
                    // quiet it is, until `max_wait`.
                    let quiet_done = quiet >= Duration::from_secs(2) && !(strict && import_running);
                    if idle || quiet_done {
                        return true;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-connection handler
// ---------------------------------------------------------------------------

/// State shared by the accept loop and the client threads.
struct Shared {
    core: Mutex<DaemonCore>,
    /// Same as the core's, reachable without the server lock.
    status: Arc<Mutex<StatusSnapshot>>,
    init_result: Value,
    verbose: bool,
    /// Set by `lsp-client/stop`, or when the server is gone.
    stop: AtomicBool,
    /// The server stopped answering: skip the graceful shutdown.
    server_dead: AtomicBool,
    /// Connected clients; the idle timeout only runs while there are none.
    active: AtomicUsize,
    last_activity: Mutex<Instant>,
}

impl Shared {
    fn touch(&self) {
        *lock(&self.last_activity) = Instant::now();
    }

    fn server_gone(&self) {
        self.server_dead.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Handle one connected CLI client until it disconnects.  Asks the daemon to
/// stop (through `shared`) on `lsp-client/stop`, or if the server is gone.
fn handle_connection(stream: UnixStream, shared: &Shared) {
    // Not from the core: that would wait for the server lock.
    let verbose = shared.verbose;
    let mut reader = match stream.try_clone() {
        Ok(s) => MessageReader::new(s),
        Err(e) => {
            if verbose {
                eprintln!("[DAEMON] failed to clone socket: {e}");
            }
            return;
        }
    };
    let mut writer = stream;

    loop {
        let msg = match reader.read_message() {
            Ok(m) => m,
            Err(_) => return, // client disconnected
        };
        shared.touch();

        let method = msg.get("method").and_then(|v| v.as_str());
        let has_id = msg.get("id").is_some();

        if verbose
            && let Some(m) = method {
                eprintln!("[DAEMON] client >>> {m}");
            }

        match method {
            // ------------------------------------------------------------------
            // Intercepted lifecycle messages (no server lock needed)
            // ------------------------------------------------------------------
            Some("initialize") => {
                // Return the cached init result so the client's LspSession works
                // without modification.
                let resp =
                    json!({"jsonrpc":"2.0","id":msg["id"],"result":shared.init_result});
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
                return;
            }

            // ------------------------------------------------------------------
            // Internal control messages
            // ------------------------------------------------------------------
            Some("lsp-client/stop") => {
                // Signal to the main loop that the daemon should shut down.
                shared.stop.store(true, Ordering::SeqCst);
                return;
            }
            Some("lsp-client/status") => {
                // Messages the server sent while no client was connected are
                // still queued: if nobody holds the server, take them into
                // account first.  If somebody does, it is reading them already.
                if let Ok(mut core) = shared.core.try_lock() {
                    while let Ok(queued) = core.server_rx.try_recv() {
                        core.observe(&queued);
                    }
                }
                let status = lock(&shared.status).clone();
                let result = json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "build": crate::session_file::build_id(),
                    "import": {
                        "state": status.import_state,
                        "problems": status.import_problems,
                    },
                });
                let resp = json!({"jsonrpc":"2.0","id":msg["id"],"result":result});
                let _ = send_framed(&mut writer, &resp);
            }
            Some("lsp-client/waitIdle") => {
                // Block until the server finishes all in-progress work, then
                // respond so the caller knows it's safe to query.
                let max_wait = msg["params"]["timeoutMs"]
                    .as_u64()
                    .map(Duration::from_millis)
                    .unwrap_or(Duration::from_secs(300));
                let mut core = lock(&shared.core);
                let idle = core.wait_for_idle(max_wait, true);
                let result = json!({
                    "status": if idle { "idle" } else { "timeout" },
                    "importFailures": core.facts.import.failures,
                    "notes": core.notes(),
                });
                drop(core);
                let resp = json!({"jsonrpc":"2.0","id":msg["id"],"result":result});
                let _ = send_framed(&mut writer, &resp);
            }

            // ------------------------------------------------------------------
            // Notifications (no id): forward to server, wait for it to settle.
            // ------------------------------------------------------------------
            Some(_) if !has_id => {
                let mut core = lock(&shared.core);
                let forwarded = rewrite_repeat_open(&mut core, msg);
                if let Err(e) = core.send_raw(&forwarded) {
                    if core.verbose {
                        eprintln!("[DAEMON] write to server failed: {e}");
                    }
                    shared.server_gone();
                    return;
                }
                // Block until the server finishes any background work triggered
                // by this notification (e.g., indexing after didOpen/didChange).
                // The client is already waiting for its next response, so this
                // delay is invisible to it.
                core.wait_for_idle(Duration::from_secs(60), false);
            }

            // ------------------------------------------------------------------
            // Requests: proxy to server, route response back.  The lock is held
            // until the response arrives, so it can only be this client's.
            // ------------------------------------------------------------------
            _ => {
                let mut core = lock(&shared.core);
                if let Err(e) = core.send_raw(&msg) {
                    if core.verbose {
                        eprintln!("[DAEMON] write to server failed: {e}");
                    }
                    shared.server_gone();
                    return;
                }
                let req_id = msg.get("id").cloned().unwrap_or(Value::Null);
                let response = loop {
                    let server_msg = match core.server_rx.recv() {
                        Ok(m) => m,
                        Err(_) => {
                            // server reader thread died
                            shared.server_gone();
                            return;
                        }
                    };

                    // Server-initiated messages are acked/tracked internally,
                    // never forwarded to the CLI client.
                    if core.observe(&server_msg) {
                        continue;
                    }

                    if server_msg.get("id") == Some(&req_id) {
                        break server_msg;
                    }
                };
                drop(core);
                let _ = send_framed(&mut writer, &response);
            }
        }
    }
}

/// Convert a repeat textDocument/didOpen into textDocument/didChange.
/// Each CLI invocation sends didOpen with current file content, but the LSP
/// spec forbids opening a document that is already open.  By rewriting to
/// didChange after the first open, the server sees correct content updates
/// without a protocol violation.
fn rewrite_repeat_open(core: &mut DaemonCore, msg: Value) -> Value {
    if msg.get("method").and_then(|v| v.as_str()) != Some("textDocument/didOpen") {
        return msg;
    }
    let uri = msg["params"]["textDocument"]["uri"].as_str().unwrap_or("").to_string();
    let Some(version) = core.opened_versions.get_mut(&uri) else {
        core.opened_versions.insert(uri, 1);
        return msg;
    };
    *version += 1;
    let new_version = *version;
    let text = msg["params"]["textDocument"]["text"].as_str().unwrap_or("").to_string();
    if core.verbose {
        eprintln!("[DAEMON] rewriting didOpen → didChange v{new_version} for {uri}");
    }
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": {"uri": uri, "version": new_version},
            "contentChanges": [{"text": text}]
        }
    })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Settings that only matter to the daemon process.
#[derive(Default)]
pub struct DaemonOptions {
    /// Answer for the server's "which build tool?" question (`--build-tool`).
    pub build_tool: Option<String>,
    /// `initializationOptions` for `initialize` (`[initialization_options]` in the config file).
    pub initialization_options: Option<Value>,
    /// Server profile asked for by name (`--server-profile`); `None` means auto.
    pub profile: Option<String>,
}

/// Run the daemon process.  This function blocks until the daemon shuts down
/// (idle timeout, server crash, or explicit stop request).
pub fn run_daemon(
    workspace: &str,
    server_bin: &str,
    server_args: &[&str],
    idle_secs: u64,
    verbose: bool,
    language_id: &str,
    options: DaemonOptions,
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
        while let Ok(msg) = reader.read_message() {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    let mut core = DaemonCore {
        server_writer: Box::new(server_stdin),
        server_rx: rx,
        next_id: 1,
        verbose,
        opened_versions: HashMap::new(),
        pending_progress: HashSet::new(),
        profile: servers::generic(),
        facts: ServerFacts::default(),
        early: Some(Vec::new()),
        build_tool: options.build_tool.clone(),
        status: Arc::new(Mutex::new(StatusSnapshot::default())),
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

    let mut init_params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "hover":      {"contentFormat": ["plaintext", "markdown"]},
                    "completion": {"completionItem": {"snippetSupport": false}},
                    "definition": {},
                    "implementation": {},
                    "typeDefinition": {},
                    "callHierarchy": {},
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
                // Without this, servers (e.g. rust-analyzer) send no $/progress,
                // and wait_for_idle cannot tell when indexing has finished.
                "window": {"workDoneProgress": true},
            },
            "workspaceFolders": [{"uri": root_uri, "name": basename}],
    });
    if let Some(opts) = &options.initialization_options {
        init_params["initializationOptions"] = opts.clone();
    }
    let init_resp = core.request("initialize", init_params)?;

    // Now the server has said what it is, choose its profile (see `servers`).
    let server_name = init_resp["result"]["serverInfo"]["name"].as_str();
    core.use_profile(servers::select(options.profile.as_deref(), server_name)?);
    if verbose {
        eprintln!("[DAEMON] server profile: {}", core.profile.name());
    }

    core.notify("initialized", json!({}))?;
    core.wait_for_idle(Duration::from_secs(60), false);

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
        server_log: core.facts.server_log.clone(),
        profile: core.profile.name().to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        build: crate::session_file::build_id(),
    };
    info.save()?;

    eprintln!(
        "[DAEMON] ready — listening on {socket_path} (idle timeout: {}s)",
        idle_secs
    );

    // ---- Main accept loop ---------------------------------------------------

    let shared = Arc::new(Shared {
        status: core.status.clone(),
        core: Mutex::new(core),
        init_result,
        verbose,
        stop: AtomicBool::new(false),
        server_dead: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        last_activity: Mutex::new(Instant::now()),
    });

    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                // The listener is non-blocking (for the idle-timeout loop), but accepted
                // sockets inherit that flag on Unix.  Set back to blocking so that
                // read_message() in handle_connection works correctly.
                let _ = stream.set_nonblocking(false);
                shared.touch();
                shared.active.fetch_add(1, Ordering::SeqCst);
                if verbose {
                    eprintln!("[DAEMON] client connected");
                }
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    handle_connection(stream, &shared);
                    shared.touch();
                    shared.active.fetch_sub(1, Ordering::SeqCst);
                    if verbose {
                        eprintln!("[DAEMON] client disconnected");
                    }
                });
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let idle = shared.active.load(Ordering::SeqCst) == 0
                    && lock(&shared.last_activity).elapsed().as_secs() >= idle_secs;
                if idle_secs > 0 && idle {
                    eprintln!("[DAEMON] idle timeout — shutting down");
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("[DAEMON] accept error: {e}");
                break;
            }
        }
    }

    // ---- Cleanup ------------------------------------------------------------

    // Remove the session file first, so `session stop` sees the daemon go at
    // once even if another client is still in the middle of a query.
    SessionInfo::delete(&canonical_root);
    let _ = std::fs::remove_file(&socket_path);

    // Gracefully shut down the LSP server after an idle timeout (as before, not
    // after a stop request or a dead server).  Another client may still hold the
    // server: give it a few seconds, then just kill the server.
    if !shared.stop.load(Ordering::SeqCst) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(mut core) = shared.core.try_lock() {
                let _ = core.request("shutdown", json!({}));
                let _ = core.notify("exit", json!({}));
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // Give the server a moment to exit before we drop the pipes.
    std::thread::sleep(Duration::from_millis(500));
    let _ = child.kill();
    let _ = child.wait();

    eprintln!("[DAEMON] stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core_for_test() -> DaemonCore {
        let (_tx, rx) = mpsc::channel();
        DaemonCore {
            server_writer: Box::new(io::sink()),
            server_rx: rx,
            next_id: 1,
            verbose: false,
            opened_versions: HashMap::new(),
            pending_progress: HashSet::new(),
            profile: servers::generic(),
            facts: ServerFacts::default(),
            early: Some(Vec::new()),
            build_tool: None,
            status: Arc::new(Mutex::new(StatusSnapshot::default())),
        }
    }

    fn failed_import() -> Value {
        json!({"jsonrpc": "2.0", "method": "intellij/workspaceImportState", "params": {
            "phase": "FINISHED",
            "folders": [{"folderUri": "file:///p/", "message": "Gradle sync failed", "status": "FAILED"}],
        }})
    }

    #[test]
    fn what_arrives_before_the_profile_is_known_is_replayed_to_it() {
        let mut core = core_for_test();
        // Sent while `initialize` is still being answered: nobody knows the server yet.
        core.observe(&failed_import());
        assert!(core.facts.import.failures.is_empty());
        core.use_profile(servers::by_name("kotlin-lsp").unwrap());
        assert_eq!(core.facts.import.failures.len(), 1);
        assert!(core.early.is_none());
    }

    #[test]
    fn nothing_reaches_the_facts_of_a_server_without_a_profile() {
        let mut core = core_for_test();
        core.observe(&failed_import());
        core.use_profile(servers::generic());
        core.observe(&failed_import());
        assert!(!core.facts.import.expected && core.facts.import.failures.is_empty());
    }

    #[test]
    fn the_default_answers_to_server_requests() {
        // `workspace/configuration` needs one entry per requested item.
        let cfg = json!({"items": [{}, {}]});
        assert_eq!(server_request_result("workspace/configuration", &cfg), json!([null, null]));
        for method in ["window/workDoneProgress/create", "client/registerCapability", "window/showMessageRequest"] {
            assert_eq!(server_request_result(method, &json!({})), Value::Null, "{method}");
        }
    }

    #[test]
    fn an_unused_build_tool_gets_a_note_once_the_import_finished() {
        let mut core = core_for_test();
        core.build_tool = Some("maven".into());
        assert!(core.notes().is_empty(), "no import yet");
        core.facts.import.expected = true;
        core.facts.import.finished = true;
        let notes = core.notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("--build-tool maven was not used"), "{}", notes[0]);
        core.facts.build_tool_asked = true;
        assert!(core.notes().is_empty());
        core.build_tool = None;
        core.facts.build_tool_asked = false;
        assert!(core.notes().is_empty());
    }

    #[test]
    fn requests_from_the_server_are_not_replayed() {
        // Only notifications are kept; a request has an id and is answered on the spot.
        let mut core = core_for_test();
        core.observe(&json!({"jsonrpc": "2.0", "id": 7, "method": "intellij/workspaceImportState", "params": {}}));
        assert_eq!(core.early.as_ref().map(Vec::len), Some(0));
    }
}
