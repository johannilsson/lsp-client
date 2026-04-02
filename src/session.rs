use crate::transport::{Result, Transport};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct LspSession {
    transport: Transport,
    next_id: u64,
    verbose: bool,
}

fn file_uri(path: &str) -> String {
    let p = Path::new(path).canonicalize().unwrap_or_else(|_| Path::new(path).to_path_buf());
    format!("file://{}", p.display())
}

impl LspSession {
    pub fn new(transport: Transport, verbose: bool) -> Self {
        Self { transport, next_id: 1, verbose }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        if self.verbose {
            eprintln!("[DEBUG] >>> {method} (id={id})");
        }

        self.transport.send_raw(&msg.to_string())?;

        loop {
            let response = self.transport.reader.read_message()?;

            // Server-initiated request: has both "method" and "id" — ack it
            if response.get("method").is_some() && response.get("id").is_some() {
                if self.verbose {
                    eprintln!(
                        "[DEBUG] <<< server request: {} (id={})",
                        response["method"], response["id"]
                    );
                }
                let ack = json!({
                    "jsonrpc": "2.0",
                    "id": response["id"],
                    "result": null,
                });
                self.transport.send_raw(&ack.to_string())?;
                continue;
            }

            // Notification: has "method" but no "id" — skip
            if response.get("method").is_some() {
                if self.verbose {
                    eprintln!("[DEBUG] <<< notification: {}", response["method"]);
                }
                continue;
            }

            // Our response
            if response.get("id") == Some(&json!(id)) {
                return Ok(response);
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.transport.send_raw(&msg.to_string())
    }

    pub fn initialize(&mut self, root: &str) -> Result<Value> {
        let root_uri = file_uri(root);
        let basename = Path::new(root).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let resp = self.request("initialize", json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "hover": {"contentFormat": ["plaintext", "markdown"]},
                    "completion": {"completionItem": {"snippetSupport": false}},
                    "definition": {},
                    "references": {},
                    "documentSymbol": {},
                    "diagnostic": {},
                    "formatting": {},
                    "signatureHelp": {
                        "signatureInformation": {"parameterInformation": {"labelOffsetSupport": true}},
                    },
                    "codeAction": {
                        "codeActionLiteralSupport": {"codeActionKind": {"valueSet": ["quickfix", "source.organizeImports"]}},
                    },
                    "rename": {},
                    "semanticTokens": {
                        "requests": {"full": true, "range": true},
                        "tokenTypes": [],
                        "tokenModifiers": [],
                        "formats": ["relative"],
                    },
                    "inlayHint": {},
                },
                "workspace": {
                    "symbol": {},
                },
            },
            "workspaceFolders": [{"uri": root_uri, "name": basename}],
        }))?;
        self.notify("initialized", json!({}))?;
        Ok(resp)
    }

    pub fn did_open(&mut self, file_path: &str, language_id: &str) -> Result<()> {
        let text = std::fs::read_to_string(file_path)
            .map_err(|e| format!("cannot read {file_path}: {e}"))?;
        self.notify("textDocument/didOpen", json!({
            "textDocument": {
                "uri": file_uri(file_path),
                "languageId": language_id,
                "version": 1,
                "text": text,
            }
        }))?;
        // Wait for server to finish indexing before we query.
        // Only possible in TCP mode — stdio transport doesn't support read timeouts.
        if self.transport.supports_timeout() {
            self.wait_for_idle(Duration::from_secs(60));
        }
        Ok(())
    }

    /// Read and discard incoming messages until the server goes quiet and all
    /// $/progress tokens have completed, or until `max_wait` elapses.
    ///
    /// This is necessary because servers like kotlin-lsp index asynchronously
    /// after didOpen and return empty results if queried before indexing finishes.
    fn wait_for_idle(&mut self, max_wait: Duration) {
        let mut pending: HashSet<serde_json::Value> = HashSet::new();
        let deadline = Instant::now() + max_wait;

        // Use a short read timeout so we can detect when the server goes quiet.
        self.transport.set_read_timeout(Some(Duration::from_millis(500)));

        loop {
            if Instant::now() >= deadline {
                break;
            }

            match self.transport.reader.try_read_message() {
                Ok(Some(msg)) => {
                    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");

                    // Ack any server-initiated requests (e.g. window/workDoneProgress/create)
                    if method == "window/workDoneProgress/create" {
                        if let Some(token) = msg["params"].get("token") {
                            pending.insert(token.clone());
                        }
                        if let Some(id) = msg.get("id") {
                            let ack = json!({"jsonrpc":"2.0","id":id,"result":null});
                            let _ = self.transport.send_raw(&ack.to_string());
                        }
                        if self.verbose {
                            eprintln!("[DEBUG] <<< progress token registered");
                        }
                        continue;
                    }

                    // Track $/progress begin/end
                    if method == "$/progress" {
                        let token = &msg["params"]["token"];
                        let kind = msg["params"]["value"]["kind"].as_str().unwrap_or("");
                        match kind {
                            "begin" => { pending.insert(token.clone()); }
                            "end" => { pending.remove(token); }
                            _ => {}
                        }
                        if self.verbose {
                            let title = msg["params"]["value"]["title"].as_str()
                                .or_else(|| msg["params"]["value"]["message"].as_str())
                                .unwrap_or("");
                            eprintln!("[DEBUG] <<< $/progress {kind} {title} (pending: {})", pending.len());
                        }
                        // Keep waiting if there are still active tokens
                        continue;
                    }

                    if self.verbose && !method.is_empty() {
                        eprintln!("[DEBUG] <<< notification: {method}");
                    }
                }
                // Timeout — no message arrived within 500ms
                Ok(None) | Err(_) => {
                    if pending.is_empty() {
                        // Server is quiet and nothing is pending — we're ready
                        break;
                    }
                    // Still waiting on progress tokens — keep going
                }
            }
        }

        // Restore the normal read timeout
        self.transport.set_read_timeout(Some(Duration::from_secs(60)));
    }

    /// Notify the server that a file's content has changed.
    ///
    /// Use this instead of `did_open` when the file has already been opened in
    /// this session and you want to sync an edit so diagnostics stay fresh.
    pub fn did_change(&mut self, file_path: &str) -> Result<()> {
        let text = std::fs::read_to_string(file_path)
            .map_err(|e| format!("cannot read {file_path}: {e}"))?;
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {
                    "uri":     file_uri(file_path),
                    "version": 2,
                },
                "contentChanges": [{"text": text}],
            }),
        )?;
        if self.transport.supports_timeout() {
            self.wait_for_idle(Duration::from_secs(60));
        }
        Ok(())
    }

    pub fn hover(&mut self, file_path: &str, line: u32, col: u32) -> Result<Value> {
        self.request("textDocument/hover", json!({
            "textDocument": {"uri": file_uri(file_path)},
            "position": {"line": line, "character": col},
        }))
    }

    pub fn definition(&mut self, file_path: &str, line: u32, col: u32) -> Result<Value> {
        self.request("textDocument/definition", json!({
            "textDocument": {"uri": file_uri(file_path)},
            "position": {"line": line, "character": col},
        }))
    }

    pub fn references(&mut self, file_path: &str, line: u32, col: u32) -> Result<Value> {
        self.request("textDocument/references", json!({
            "textDocument": {"uri": file_uri(file_path)},
            "position": {"line": line, "character": col},
            "context": {"includeDeclaration": true},
        }))
    }

    pub fn document_symbols(&mut self, file_path: &str) -> Result<Value> {
        self.request("textDocument/documentSymbol", json!({
            "textDocument": {"uri": file_uri(file_path)},
        }))
    }

    pub fn workspace_symbols(&mut self, query: &str) -> Result<Value> {
        self.request("workspace/symbol", json!({"query": query}))
    }

    pub fn diagnostics(&mut self, file_path: &str) -> Result<Value> {
        self.request("textDocument/diagnostic", json!({
            "textDocument": {"uri": file_uri(file_path)},
        }))
    }

    pub fn completion(&mut self, file_path: &str, line: u32, col: u32) -> Result<Value> {
        self.request("textDocument/completion", json!({
            "textDocument": {"uri": file_uri(file_path)},
            "position": {"line": line, "character": col},
        }))
    }

    pub fn signature_help(&mut self, file_path: &str, line: u32, col: u32) -> Result<Value> {
        self.request("textDocument/signatureHelp", json!({
            "textDocument": {"uri": file_uri(file_path)},
            "position": {"line": line, "character": col},
        }))
    }

    pub fn code_action(&mut self, file_path: &str, line: u32, col: u32) -> Result<Value> {
        // Use a zero-width range at the given position
        let pos = json!({"line": line, "character": col});
        self.request("textDocument/codeAction", json!({
            "textDocument": {"uri": file_uri(file_path)},
            "range": {"start": pos, "end": pos},
            "context": {"diagnostics": []},
        }))
    }

    pub fn rename(&mut self, file_path: &str, line: u32, col: u32, new_name: &str) -> Result<Value> {
        self.request("textDocument/rename", json!({
            "textDocument": {"uri": file_uri(file_path)},
            "position": {"line": line, "character": col},
            "newName": new_name,
        }))
    }

    pub fn semantic_tokens(&mut self, file_path: &str) -> Result<Value> {
        self.request("textDocument/semanticTokens/full", json!({
            "textDocument": {"uri": file_uri(file_path)},
        }))
    }

    pub fn inlay_hints(&mut self, file_path: &str, start_line: u32, end_line: u32) -> Result<Value> {
        self.request("textDocument/inlayHint", json!({
            "textDocument": {"uri": file_uri(file_path)},
            "range": {
                "start": {"line": start_line, "character": 0},
                "end":   {"line": end_line,   "character": 0},
            },
        }))
    }

    pub fn shutdown(mut self) {
        let _ = self.request("shutdown", json!({}));
        let _ = self.notify("exit", json!({}));
    }
}
