//! kotlin-lsp (JetBrains' IntelliJ-based Kotlin language server).
//!
//! Its project import is not part of the protocol: it announces an import with
//! `intellij/workspaceImportStatus`, reports the outcome with
//! `intellij/workspaceImportState`, and when a folder has more than one build tool
//! it asks which to use with `window/showMessageRequest`. The notifications are
//! undocumented and were read off the server's own messages (tested with
//! 271.2085.0), so everything that depends on them is here, and only runs when
//! this profile is in use.

use super::{ServerFacts, ServerProfile};
use serde_json::Value;

/// True if `serverInfo.name` from `initialize` is kotlin-lsp's
/// ("IntelliJ Language Server by JetBrains").
pub fn is_kotlin_lsp(server_info_name: Option<&str>) -> bool {
    server_info_name.is_some_and(|n| n.contains("IntelliJ Language Server"))
}

#[derive(Default)]
pub struct KotlinLsp {
    /// Folders the server could not import and why, from the latest
    /// `intellij/workspaceImportStatus` (`blockedFolders`). Its outcome message
    /// names the reason; the build tools it found (`candidates`) are only here.
    blocked: Vec<Value>,
}

impl ServerProfile for KotlinLsp {
    fn name(&self) -> &'static str {
        "kotlin-lsp"
    }

    fn on_message(&mut self, msg: &Value, facts: &mut ServerFacts) {
        match msg.get("method").and_then(|m| m.as_str()) {
            // "I am going to import": the import can start several seconds after
            // `initialized` and may report no `$/progress` at all, so from now on
            // being idle also means the import has finished.
            Some("intellij/workspaceImportStatus") => {
                if !facts.import.finished {
                    facts.import.expected = true;
                }
                if let Some(blocked) = msg["params"]["blockedFolders"].as_array() {
                    self.blocked = blocked.clone();
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
                        .filter(|f| matches!(f["status"].as_str(), Some("FAILED" | "BLOCKED")))
                        .map(|f| {
                            let folder = f["folderUri"].as_str().unwrap_or("?");
                            let candidates: Vec<String> = self
                                .blocked
                                .iter()
                                .find(|b| b["folderUri"].as_str() == Some(folder))
                                .and_then(|b| b["candidates"].as_array())
                                .into_iter()
                                .flatten()
                                .filter_map(|c| c.as_str().map(str::to_owned))
                                .collect();
                            describe_import_problem(
                                f["status"].as_str().unwrap_or("FAILED"),
                                folder,
                                f["message"].as_str().unwrap_or("import failed"),
                                &candidates,
                            )
                        })
                        .collect();
                }
            }
            _ => {}
        }
    }

    /// With more than one build tool in a folder the server asks "Use Gradle / Use
    /// Maven / Configure…". `--build-tool gradle` answers by pressing the button whose
    /// title contains that word; without it the question is dismissed. The server
    /// remembers the answer for the workspace.
    fn answer_request(&mut self, method: &str, params: &Value, build_tool: Option<&str>) -> Option<Value> {
        if method != "window/showMessageRequest" {
            return None;
        }
        let want = build_tool.map(str::to_lowercase).filter(|t| !t.is_empty())?;
        params["actions"]
            .as_array()?
            .iter()
            .find(|a| a["title"].as_str().is_some_and(|t| t.to_lowercase().contains(&want)))
            .cloned()
    }
}

/// Human-readable explanation of why a folder was not imported.  `status` and
/// `message` come from `intellij/workspaceImportState` (`FAILED` or
/// `BLOCKED`); for blocked folders `message` is the reason id and
/// `candidates` are the build tools the server found.
fn describe_import_problem(status: &str, folder: &str, message: &str, candidates: &[String]) -> String {
    let path = folder.strip_prefix("file://").unwrap_or(folder);
    if status != "BLOCKED" {
        return format!(
            "project import failed for {path}: {message}. Results will be incomplete; see the server log (--verbose prints its path)."
        );
    }
    let why = match message {
        "ambiguousBuildSystem" => format!(
            "more than one build tool found ({}); choose one with --build-tool or `build_tool` in .lsp-client.toml",
            candidates.join(", ")
        ),
        "noBuildSystemFound" => "no supported build system found in the project root".to_string(),
        "noProjectInRoot" => "no project found in the root folder".to_string(),
        "noProjectAtConfiguredPath" => "no project at the configured path".to_string(),
        "cannotResolveConfiguredPath" | "cannotResolveFolderPath" => "the folder path could not be resolved".to_string(),
        "unknownBuildTool" => "unknown build tool".to_string(),
        other => other.to_string(),
    };
    format!("project not imported for {path}: {why}. Results will be limited to file-level queries.")
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers;
    use serde_json::json;

    fn state(phase: &str, status: &str, message: &str) -> Value {
        json!({"jsonrpc": "2.0", "method": "intellij/workspaceImportState", "params": {
            "phase": phase,
            "folders": [{"folderUri": "file:///p/", "message": message, "status": status}],
        }})
    }

    fn buttons() -> Value {
        json!({"actions": [{"title": "Use Gradle"}, {"title": "Use Maven"}, {"title": "Configure…"}]})
    }

    #[test]
    fn a_failed_import_is_recorded_when_it_finishes() {
        let mut kt = KotlinLsp::default();
        let mut facts = ServerFacts::default();
        kt.on_message(&json!({"method": "intellij/workspaceImportStatus", "params": {}}), &mut facts);
        assert!(facts.import.expected && !facts.import.finished);
        kt.on_message(&state("FINISHED", "FAILED", "Gradle sync failed"), &mut facts);
        assert!(facts.import.finished);
        assert_eq!(facts.import.failures.len(), 1);
        assert!(facts.import.failures[0].contains("Gradle sync failed"), "{:?}", facts.import.failures);
    }

    #[test]
    fn a_successful_import_leaves_no_failures() {
        let mut kt = KotlinLsp::default();
        let mut facts = ServerFacts::default();
        kt.on_message(&state("FINISHED", "SUCCESS", ""), &mut facts);
        assert!(facts.import.finished && facts.import.failures.is_empty());
    }

    #[test]
    fn a_blocked_folder_is_explained_with_the_candidates_the_server_found() {
        let mut kt = KotlinLsp::default();
        let mut facts = ServerFacts::default();
        kt.on_message(
            &json!({"method": "intellij/workspaceImportStatus", "params": {"blockedFolders": [
                {"folderUri": "file:///p/", "reason": "ambiguousBuildSystem", "candidates": ["gradle", "maven"]}
            ]}}),
            &mut facts,
        );
        kt.on_message(&state("FINISHED", "BLOCKED", "ambiguousBuildSystem"), &mut facts);
        let text = &facts.import.failures[0];
        assert!(text.contains("/p/") && text.contains("gradle, maven") && text.contains("--build-tool"), "{text}");
    }

    #[test]
    fn other_messages_are_ignored() {
        let mut kt = KotlinLsp::default();
        let mut facts = ServerFacts::default();
        kt.on_message(&json!({"method": "window/logMessage", "params": {"message": "hi"}}), &mut facts);
        kt.on_message(&json!({"id": 3, "result": null}), &mut facts);
        assert!(!facts.import.expected);
    }

    #[test]
    fn another_server_sending_the_same_methods_is_not_affected() {
        // The point of profiles: a server that is not kotlin-lsp, whatever it sends.
        let mut facts = ServerFacts::default();
        let mut generic = servers::generic();
        generic.on_message(&state("FINISHED", "FAILED", "x"), &mut facts);
        generic.on_message(&json!({"method": "intellij/workspaceImportStatus", "params": {}}), &mut facts);
        assert!(!facts.import.expected && !facts.import.finished && facts.import.failures.is_empty());
        // ... and its questions are not answered either.
        let answer = generic.answer_request("window/showMessageRequest", &buttons(), Some("gradle"));
        assert_eq!(answer, None);
    }

    #[test]
    fn the_build_tool_answers_the_matching_button() {
        let mut kt = KotlinLsp::default();
        assert_eq!(
            kt.answer_request("window/showMessageRequest", &buttons(), Some("maven")),
            Some(json!({"title": "Use Maven"}))
        );
        // Case-insensitive.
        assert_eq!(
            kt.answer_request("window/showMessageRequest", &buttons(), Some("Gradle")),
            Some(json!({"title": "Use Gradle"}))
        );
    }

    #[test]
    fn a_question_without_a_matching_choice_is_left_to_the_default() {
        let mut kt = KotlinLsp::default();
        for tool in [None, Some("bazel"), Some("")] {
            assert_eq!(kt.answer_request("window/showMessageRequest", &buttons(), tool), None, "{tool:?}");
        }
        // Other requests are not this profile's business.
        assert_eq!(kt.answer_request("workspace/configuration", &json!({"items": [{}]}), Some("gradle")), None);
    }

    #[test]
    fn blocked_folders_are_explained() {
        let amb = describe_import_problem(
            "BLOCKED",
            "file:///p/amb/",
            "ambiguousBuildSystem",
            &["gradle".to_string(), "maven".to_string()],
        );
        assert!(amb.contains("/p/amb/") && amb.contains("gradle, maven") && amb.contains("--build-tool"), "{amb}");
        let none = describe_import_problem("BLOCKED", "file:///p/x/", "noBuildSystemFound", &[]);
        assert!(none.contains("no supported build system"), "{none}");
        // Unknown reasons are shown as they are rather than hidden.
        let other = describe_import_problem("BLOCKED", "file:///p/x/", "somethingNew", &[]);
        assert!(other.contains("somethingNew"), "{other}");
    }

    #[test]
    fn failed_imports_point_at_the_server_log() {
        let f = describe_import_problem("FAILED", "file:///p/app/", "Gradle sync failed", &[]);
        assert!(f.contains("/p/app/") && f.contains("Gradle sync failed") && f.contains("server log"), "{f}");
    }

    #[test]
    fn the_server_is_recognised_by_its_name() {
        assert!(is_kotlin_lsp(Some("IntelliJ Language Server by JetBrains")));
        assert!(!is_kotlin_lsp(Some("rust-analyzer")));
        assert!(!is_kotlin_lsp(None));
    }
}
