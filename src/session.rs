use crate::transport::{Result, Transport};
use serde_json::{json, Value};
use std::path::Path;

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
        }))
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
