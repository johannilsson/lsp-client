//! kotlin-lsp (JetBrains' IntelliJ-based Kotlin language server).
//!
//! Its project import is not part of the protocol: it announces an import with
//! `intellij/workspaceImportStatus`, reports the outcome with
//! `intellij/workspaceImportState`, and when a folder has more than one build tool
//! it asks which to use with `window/showMessageRequest`. It also announces its
//! own log file in a `window/logMessage` ("Log file: <path>"). The notifications are
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
            // "Log file: <path>", sent once at startup.
            Some("window/logMessage") => {
                if let Some(path) = msg["params"]["message"].as_str().and_then(|m| m.strip_prefix("Log file: ")) {
                    facts.server_log = Some(path.trim().to_owned());
                }
            }
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
                    let server_log = facts.server_log.clone();
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
                                server_log.as_deref(),
                            )
                        })
                        .collect();
                }
            }
            _ => {}
        }
    }

    /// The server links to declarations with `command:jetbrains.navigateToLocation?
    /// ["<uri>",line,col]` (percent-encoded), which a command line cannot follow:
    /// show the place as `text (path:line:col)` instead.
    fn rewrite_hover(&self, markdown: String) -> String {
        const LINK: &str = "](command:jetbrains.navigateToLocation?";
        let mut out = String::with_capacity(markdown.len());
        let mut rest = markdown.as_str();
        while let Some(i) = rest.find(LINK) {
            let (Some(open), Some(close)) = (rest[..i].rfind('['), rest[i..].find(')').map(|j| i + j)) else {
                break;
            };
            let args = &rest[i + LINK.len()..close];
            let place = match serde_json::from_str::<Value>(&percent_decode(args)) {
                Ok(Value::Array(a)) => match (
                    a.first().and_then(|v| v.as_str()),
                    a.get(1).and_then(|v| v.as_u64()),
                    a.get(2).and_then(|v| v.as_u64()),
                ) {
                    (Some(uri), Some(line), Some(col)) => {
                        Some(format!(" ({}:{}:{})", uri.strip_prefix("file://").unwrap_or(uri), line + 1, col + 1))
                    }
                    _ => None,
                },
                _ => None,
            };
            let Some(place) = place else {
                // Not the shape we know: leave it for the generic clean-up.
                out.push_str(&rest[..close + 1]);
                rest = &rest[close + 1..];
                continue;
            };
            out.push_str(&rest[..open]);
            out.push_str(&rest[open + 1..i]);
            out.push_str(&place);
            rest = &rest[close + 1..];
        }
        out.push_str(rest);
        out
    }

    /// On Android the server also lists every declaration from the project's own
    /// compiled classes jar, under `build/intermediates/`. Those are the same
    /// declarations as in the source, so only the source ones are shown.
    fn keep_workspace_symbol(&self, symbol: &Value) -> bool {
        let uri = symbol["location"]["uri"].as_str().unwrap_or("");
        !(uri.starts_with("jar:") && uri.contains("/build/intermediates/"))
    }

    /// Compose names UI functions in UpperCase on purpose, and Android Studio does
    /// not report it, but the server warns `Function name 'Foo' should start with a
    /// lowercase letter` (inspection `FunctionName`) for every one. Leave out those
    /// on `@Composable` functions; every other diagnostic is kept.
    fn keep_diagnostic(&self, diagnostic: &Value, source: Option<&str>) -> bool {
        if diagnostic["code"].as_str() != Some("FunctionName") {
            return true;
        }
        let (Some(source), Some(line), Some(col)) = (
            source,
            diagnostic["range"]["start"]["line"].as_u64(),
            diagnostic["range"]["start"]["character"].as_u64(),
        ) else {
            return true;
        };
        !is_composable_declaration(source, line as usize, col as usize)
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

/// Decode `%XX` escapes (as in a URI query string).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True if `text` contains a `@Composable` annotation (plain or qualified,
/// e.g. `@androidx.compose.runtime.Composable`), but not `@ComposableTarget`
/// and the like.
fn mentions_composable(text: &str) -> bool {
    const NAME: &str = "Composable";
    text.match_indices(NAME).any(|(i, _)| {
        let before = &text[..i];
        let after = text[i + NAME.len()..].chars().next();
        let boundary = after.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let annotated = before.contains('@') && (before.ends_with('@') || before.ends_with('.'));
        boundary && annotated
    })
}

/// Whether the declaration starting at 0-based `line`/`col` of `source` is
/// annotated `@Composable`: on the same line before `col`, or in the
/// annotation lines directly above it.  Annotations can span lines
/// (`@OptIn(\n Foo::class\n)`), so the walk upwards continues until a blank
/// line, the end of a previous declaration or block (`{`, `}`, `;`) or the end
/// of a comment, for at most 12 lines.
fn is_composable_declaration(source: &str, line: usize, col: usize) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    let Some(&decl) = lines.get(line) else {
        return false;
    };
    let prefix_end = decl.char_indices().nth(col).map_or(decl.len(), |(i, _)| i);
    if mentions_composable(&decl[..prefix_end]) {
        return true;
    }
    for above in lines[..line].iter().rev().take(12) {
        let t = above.trim();
        if t.is_empty() || t.ends_with(['{', '}', ';']) || t.ends_with("*/") || t.starts_with("//") {
            return false;
        }
        if mentions_composable(t) {
            return true;
        }
    }
    false
}

/// Human-readable explanation of why a folder was not imported.  `status` and
/// `message` come from `intellij/workspaceImportState` (`FAILED` or
/// `BLOCKED`); for blocked folders `message` is the reason id and
/// `candidates` are the build tools the server found.
fn describe_import_problem(
    status: &str,
    folder: &str,
    message: &str,
    candidates: &[String],
    server_log: Option<&str>,
) -> String {
    let path = folder.strip_prefix("file://").unwrap_or(folder);
    if status != "BLOCKED" {
        let log = match server_log {
            Some(log) => format!("server log: {log}"),
            None => "see the server log (--verbose prints its path)".to_string(),
        };
        return format!("project import failed for {path}: {message}. Results will be incomplete; {log}");
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
            None,
        );
        assert!(amb.contains("/p/amb/") && amb.contains("gradle, maven") && amb.contains("--build-tool"), "{amb}");
        let none = describe_import_problem("BLOCKED", "file:///p/x/", "noBuildSystemFound", &[], None);
        assert!(none.contains("no supported build system"), "{none}");
        // Unknown reasons are shown as they are rather than hidden.
        let other = describe_import_problem("BLOCKED", "file:///p/x/", "somethingNew", &[], None);
        assert!(other.contains("somethingNew"), "{other}");
    }

    #[test]
    fn failed_imports_point_at_the_server_log() {
        let f = describe_import_problem("FAILED", "file:///p/app/", "Gradle sync failed", &[], None);
        assert!(f.contains("/p/app/") && f.contains("Gradle sync failed") && f.contains("server log"), "{f}");
    }

    #[test]
    fn failed_imports_name_the_log_file_when_the_server_announced_one() {
        let f = describe_import_problem("FAILED", "file:///p/app/", "Gradle sync failed", &[], Some("/tmp/x/server.log"));
        assert!(f.ends_with("server log: /tmp/x/server.log"), "{f}");
    }

    fn log_message(m: &str) -> Value {
        json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"message": m, "type": 3}})
    }

    #[test]
    fn the_server_log_path_is_taken_from_its_log_message() {
        let mut kt = KotlinLsp::default();
        let mut facts = ServerFacts::default();
        kt.on_message(&log_message("Process started with ServerLaunchConfig(...)"), &mut facts);
        assert_eq!(facts.server_log, None);
        kt.on_message(&log_message("Log file: /tmp/idea-system1/system/log/intellij-server.log"), &mut facts);
        assert_eq!(facts.server_log.as_deref(), Some("/tmp/idea-system1/system/log/intellij-server.log"));
    }

    #[test]
    fn the_log_path_reaches_the_failure_message() {
        let mut kt = KotlinLsp::default();
        let mut facts = ServerFacts::default();
        kt.on_message(&log_message("Log file: /tmp/x/server.log"), &mut facts);
        kt.on_message(&state("FINISHED", "FAILED", "Gradle sync failed"), &mut facts);
        assert!(facts.import.failures[0].ends_with("server log: /tmp/x/server.log"), "{:?}", facts.import.failures);
    }

    #[test]
    fn another_server_announcing_a_log_file_is_not_taken_at_its_word() {
        let mut facts = ServerFacts::default();
        let mut generic = servers::generic();
        generic.on_message(&log_message("Log file: /tmp/elsewhere.log"), &mut facts);
        assert_eq!(facts.server_log, None);
    }

    #[test]
    fn navigate_links_in_hover_text_become_a_place() {
        let md = "From class: [`a.B`](command:jetbrains.navigateToLocation?%5B%22file%3A%2F%2F%2Fx%2FB.java%22%2C5%2C19%5D)\nmore";
        assert_eq!(KotlinLsp::default().rewrite_hover(md.to_string()), "From class: `a.B` (/x/B.java:6:20)\nmore");
    }

    #[test]
    fn other_links_in_hover_text_are_left_alone() {
        let kt = KotlinLsp::default();
        for md in [
            "[run](command:foo?x) and [docs](https://example.com)",
            // The command is this server's, but the arguments are not what we know.
            "[x](command:jetbrains.navigateToLocation?%5B1%5D)",
        ] {
            assert_eq!(kt.rewrite_hover(md.to_string()), md);
        }
    }

    #[test]
    fn symbols_from_the_projects_own_class_jar_are_dropped() {
        let kt = KotlinLsp::default();
        let sym = |uri: &str| json!({"name": "f", "kind": 12, "location": {"uri": uri}});
        assert!(kt.keep_workspace_symbol(&sym("file:///p/app/src/A.kt")));
        assert!(kt.keep_workspace_symbol(&sym("jar:///root/.gradle/caches/lib/classes.jar!/B.class")));
        assert!(!kt.keep_workspace_symbol(&sym("jar:///p/app/build/intermediates/x/classes.jar!/AKt.class")));
    }

    fn function_name_warning(line: u64, col: u64) -> Value {
        json!({"code": "FunctionName", "severity": 2,
               "message": "Function name 'Foo' should start with a lowercase letter",
               "range": {"start": {"line": line, "character": col}, "end": {"line": line, "character": col + 3}}})
    }

    /// Whether the warning on the `Foo` in `src` is kept.
    fn warning_kept(src: &str) -> bool {
        let line = src.lines().position(|l| l.contains("fun Foo")).unwrap();
        let col = src.lines().nth(line).unwrap().find("Foo").unwrap() as u64;
        KotlinLsp::default().keep_diagnostic(&function_name_warning(line as u64, col), Some(src))
    }

    #[test]
    fn composable_function_names_are_not_reported() {
        for src in [
            "@Composable fun Foo() {}",
            "@Composable\nfun Foo() {}",
            "@Composable\n@OptIn(ExperimentalFoo::class)\nfun Foo() {}",
            "@OptIn(\n    ExperimentalFoo::class,\n)\n@Composable\nfun Foo() {}",
            "@androidx.compose.runtime.Composable\nprivate fun Foo() {}",
            "@Composable()\ninternal fun Foo() {}",
        ] {
            assert!(!warning_kept(src), "{src}");
        }
    }

    #[test]
    fn other_function_name_warnings_are_kept() {
        for src in [
            "fun Foo() {}",
            // The annotation belongs to the previous function.
            "@Composable\nfun a() {\n}\nfun Foo() {}",
            // Similar names are not @Composable.
            "@ComposableTarget(\"x\")\nfun Foo() {}",
            "// @Composable\nfun Foo() {}",
        ] {
            assert!(warning_kept(src), "{src}");
        }
    }

    #[test]
    fn other_diagnostics_are_kept_and_so_is_everything_without_the_source() {
        let kt = KotlinLsp::default();
        let other = json!({"code": "SomethingElse", "severity": 2, "message": "unused",
                           "range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 7}}});
        assert!(kt.keep_diagnostic(&other, Some("@Composable\nfun Foo() {}\n")));
        // Without the source we cannot tell, so nothing is hidden.
        assert!(kt.keep_diagnostic(&function_name_warning(1, 4), None));
    }

    #[test]
    fn another_server_keeps_everything() {
        let generic = servers::generic();
        let src = "@Composable\nfun Foo() {}\n";
        assert!(generic.keep_diagnostic(&function_name_warning(1, 4), Some(src)));
        assert!(generic.keep_workspace_symbol(&json!({"location": {"uri": "jar:///p/build/intermediates/x.jar!/A.class"}})));
        let md = "[a.B](command:jetbrains.navigateToLocation?%5B%22file%3A%2F%2F%2Fx%22%2C1%2C2%5D)";
        assert_eq!(generic.rewrite_hover(md.to_string()), md);
    }

    #[test]
    fn the_server_is_recognised_by_its_name() {
        assert!(is_kotlin_lsp(Some("IntelliJ Language Server by JetBrains")));
        assert!(!is_kotlin_lsp(Some("rust-analyzer")));
        assert!(!is_kotlin_lsp(None));
    }
}
