//! kotlin-lsp (JetBrains' IntelliJ-based Kotlin language server).
//!
//! Its project import is not part of the protocol: it announces an import with
//! `intellij/workspaceImportStatus` and reports the outcome with
//! `intellij/workspaceImportState`. Both are undocumented and were read off the
//! server's own messages (tested with 271.2085.0), so everything that depends on
//! them is here, and only runs when this profile is in use.

use super::{ServerFacts, ServerProfile};
use serde_json::Value;

/// True if `serverInfo.name` from `initialize` is kotlin-lsp's
/// ("IntelliJ Language Server by JetBrains").
pub fn is_kotlin_lsp(server_info_name: Option<&str>) -> bool {
    server_info_name.is_some_and(|n| n.contains("IntelliJ Language Server"))
}

pub struct KotlinLsp;

impl ServerProfile for KotlinLsp {
    fn name(&self) -> &'static str {
        "kotlin-lsp"
    }

    fn on_message(&self, msg: &Value, facts: &mut ServerFacts) {
        match msg.get("method").and_then(|m| m.as_str()) {
            // "I am going to import": the import can start several seconds after
            // `initialized` and may report no `$/progress` at all, so from now on
            // being idle also means the import has finished.
            Some("intellij/workspaceImportStatus") => {
                if !facts.import.finished {
                    facts.import.expected = true;
                }
            }
            // "The import is at <phase>, and this is how each folder went."
            Some("intellij/workspaceImportState") => {
                let params = &msg["params"];
                facts.import.expected = true;
                facts.import.finished = params["phase"].as_str() == Some("FINISHED");
                if facts.import.finished {
                    facts.import.failures = params["folders"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|f| f["status"].as_str() == Some("FAILED"))
                        .map(|f| {
                            format!(
                                "{}: {}",
                                f["folderUri"].as_str().unwrap_or("?"),
                                f["message"].as_str().unwrap_or("import failed")
                            )
                        })
                        .collect();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers;
    use serde_json::json;

    fn state(phase: &str, status: &str) -> Value {
        json!({"jsonrpc": "2.0", "method": "intellij/workspaceImportState", "params": {
            "phase": phase,
            "folders": [{"folderUri": "file:///p/", "message": "Gradle sync failed", "status": status}],
        }})
    }

    #[test]
    fn a_failed_import_is_recorded_when_it_finishes() {
        let mut facts = ServerFacts::default();
        KotlinLsp.on_message(&json!({"method": "intellij/workspaceImportStatus", "params": {}}), &mut facts);
        assert!(facts.import.expected && !facts.import.finished);
        KotlinLsp.on_message(&state("FINISHED", "FAILED"), &mut facts);
        assert!(facts.import.finished);
        assert_eq!(facts.import.failures, vec!["file:///p/: Gradle sync failed".to_string()]);
    }

    #[test]
    fn a_successful_import_leaves_no_failures() {
        let mut facts = ServerFacts::default();
        KotlinLsp.on_message(&state("FINISHED", "SUCCESS"), &mut facts);
        assert!(facts.import.finished && facts.import.failures.is_empty());
    }

    #[test]
    fn other_messages_are_ignored() {
        let mut facts = ServerFacts::default();
        KotlinLsp.on_message(&json!({"method": "window/logMessage", "params": {"message": "hi"}}), &mut facts);
        KotlinLsp.on_message(&json!({"id": 3, "result": null}), &mut facts);
        assert!(!facts.import.expected);
    }

    #[test]
    fn another_server_sending_the_same_methods_is_not_affected() {
        // The point of profiles: a server that is not kotlin-lsp, whatever it sends.
        let mut facts = ServerFacts::default();
        let generic = servers::generic();
        generic.on_message(&state("FINISHED", "FAILED"), &mut facts);
        generic.on_message(&json!({"method": "intellij/workspaceImportStatus", "params": {}}), &mut facts);
        assert!(!facts.import.expected && !facts.import.finished && facts.import.failures.is_empty());
    }

    #[test]
    fn the_server_is_recognised_by_its_name() {
        assert!(is_kotlin_lsp(Some("IntelliJ Language Server by JetBrains")));
        assert!(!is_kotlin_lsp(Some("rust-analyzer")));
        assert!(!is_kotlin_lsp(None));
    }
}
