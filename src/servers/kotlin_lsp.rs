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

/// True if `bin` (the `--server` value, possibly a path) is a kotlin-lsp launcher:
/// the Homebrew/standalone `kotlin-lsp`, its old `kotlin-lsp.sh`, or the
/// `intellij-server` binary inside the archive.
pub fn is_kotlin_lsp_binary(bin: &str) -> bool {
    let name = std::path::Path::new(bin).file_name().and_then(|n| n.to_str()).unwrap_or(bin);
    matches!(name, "kotlin-lsp" | "kotlin-lsp.sh" | "intellij-server")
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
            // "Use Gradle / Use Maven / Configure…" (answered in `answer_request`).
            Some("window/showMessageRequest") if is_build_tool_question(&msg["params"]) => {
                facts.build_tool_asked = true;
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
                    // The cause is in the server log, above its "sync failed" line.
                    let cause = server_log
                        .as_deref()
                        .and_then(|log| read_tail(log, 512 * 1024))
                        .and_then(|text| failure_cause(&text));
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
                                cause.as_deref(),
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

    /// kotlin-lsp does not run the build, so sources the build generates
    /// (`BuildConfig`, `R`) are missing until they exist on disk, and files
    /// generated while it runs are not picked up until a restart.
    fn diagnostics_notes(&self, diagnostics: &[&Value]) -> Vec<String> {
        let mut missing: Vec<&str> = Vec::new();
        for d in diagnostics {
            let msg = d["message"].as_str().unwrap_or("");
            for name in ["BuildConfig", "R"] {
                // K2 writes `Unresolved reference 'R'.`, K1 `Unresolved reference: R`.
                let unresolved = msg.contains(&format!("Unresolved reference '{name}'"))
                    || (msg.contains(&format!("Unresolved reference: {name}"))
                        && msg.trim_end_matches('.').ends_with(&format!(": {name}")));
                if unresolved && !missing.contains(&name) {
                    missing.push(name);
                }
            }
        }
        if missing.is_empty() {
            return Vec::new();
        }
        let names = missing.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(" and ");
        vec![format!(
            "{names} {} generated by the Android build, and kotlin-lsp does not run it. Generate the sources \
             (for example `./gradlew :app:generateDebugBuildConfig`, or a build), then restart the daemon \
             (`lsp-client session stop`); files generated while the server runs are not picked up.",
            if missing.len() == 1 { "is" } else { "are" }
        )]
    }

    /// kotlin-lsp serves TCP clients with `--socket host:port`, and keeps running for
    /// the next one with `--multi-client` (otherwise it exits when the client leaves).
    fn tcp_start_args(&self, host: &str, port: u16) -> Option<Vec<String>> {
        Some(vec!["--socket".to_string(), format!("{host}:{port}"), "--multi-client".to_string()])
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

/// Whether a `window/showMessageRequest` is the "which build tool?" question:
/// its buttons name Gradle or Maven.
fn is_build_tool_question(params: &Value) -> bool {
    params["actions"].as_array().is_some_and(|actions| {
        actions.iter().any(|a| {
            let t = a["title"].as_str().unwrap_or("").to_lowercase();
            t.contains("gradle") || t.contains("maven")
        })
    })
}

/// The last `max` bytes of the file at `path`, as text.
fn read_tail(path: &str, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(max))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The message of a log line, without IntelliJ's `date time [ms] LEVEL - logger - `
/// prefix (lines without it are returned as they are), and without the
/// `[IMPORT ERR]: ` / `[IMPORT STD]: ` that marks the Gradle build's own output.
fn log_message(line: &str) -> &str {
    let starts_with_date = line.len() > 10 && line.as_bytes()[..4].iter().all(u8::is_ascii_digit) && line.as_bytes()[4] == b'-';
    let mut msg = line;
    if starts_with_date && let Some((_, rest)) = line.split_once(" - ") {
        msg = rest.split_once(" - ").map_or(rest, |(_, msg)| msg);
    }
    for tag in ["[IMPORT ERR]:", "[IMPORT STD]:"] {
        msg = msg.trim_start().strip_prefix(tag).unwrap_or(msg);
    }
    msg.trim()
}

/// `message` from a `Caused by: some.pkg.SomeException: message` line, or the
/// line as it is.
fn exception_message(line: &str) -> &str {
    let line = line.strip_prefix("Caused by:").map_or(line, str::trim_start);
    match line.split_once(": ") {
        Some((class, msg)) if class.contains('.') && !class.contains(char::is_whitespace) => msg,
        _ => line,
    }
}

/// What says why the last failed import failed, from the end of the server log:
/// within that import (after the last `Importing with`, if any), above the last
/// `Unable to import` / `Gradle sync failed` line. For example `Compose Preview
/// failed for ':app' (variant 'release'): …` (#21) or `SDK location not found. …`.
/// `None` if nothing looks like a cause.
///
/// In order of preference: the first Kotlin compiler error (`e: file:…:3:25: …`,
/// which names the build file and line), the innermost `> ` line of Gradle's
/// `* What went wrong:` (the cause, with the failures that led to it above it),
/// the last `Caused by:` (the root of the exception chain), and then the first
/// line that matches one of a few patterns, the most specific pattern first. Gradle also
/// prints warnings it continues after, such as `Could not start the FSEvents
/// stream` from its file watching (#50); these are never the cause.
fn failure_cause(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let is_marker = |l: &str| l.contains("Unable to import") || l.contains("Gradle sync failed");
    let end = lines.iter().rposition(|l| is_marker(l))?;
    let start = lines[..end].iter().rposition(|l| l.contains("Importing with")).map_or(0, |i| i + 1);
    const NOISE: &[&str] = &["Could not start the FSEvents stream"];
    let msgs: Vec<&str> = lines[start..end]
        .iter()
        .map(|l| log_message(l))
        .filter(|m| !m.is_empty() && !is_marker(m) && !NOISE.iter().any(|n| m.contains(n)))
        .collect();

    let compiler_error = msgs.iter().find(|m| m.starts_with("e: ")).copied();
    let what_went_wrong = || msgs.iter().rposition(|m| *m == "* What went wrong:").and_then(|i| {
        let block: Vec<&str> = msgs[i + 1..].iter().copied().take_while(|m| !m.starts_with("* ")).collect();
        let innermost = block.iter().rev().find(|m| m.starts_with('>')).map(|m| m.trim_start_matches(['>', ' ']));
        innermost.or(block.first().copied())
    });
    let caused_by = || msgs.iter().rev().find(|m| m.starts_with("Caused by:")).map(|m| exception_message(m));
    const CAUSES: &[&str] = &["failed for", "error:", "Exception:", "Could not "];
    let pattern = || CAUSES.iter().find_map(|c| msgs.iter().find(|m| m.contains(c))).copied();

    compiler_error.or_else(what_went_wrong).or_else(caused_by).or_else(pattern).map(|m| {
        let cut = m.char_indices().nth(300).map_or(m.len(), |(i, _)| i);
        if cut < m.len() { format!("{}…", &m[..cut]) } else { m.to_owned() }
    })
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
    cause: Option<&str>,
) -> String {
    let path = folder.strip_prefix("file://").unwrap_or(folder);
    if status != "BLOCKED" {
        let log = match server_log {
            Some(log) => format!("server log: {log}"),
            None => "see the server log (--verbose prints its path)".to_string(),
        };
        let cause = cause.map(|c| format!(" (cause, from the server log: {c})")).unwrap_or_default();
        return format!("project import failed for {path}: {message}{cause}. Results will be incomplete; {log}");
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
            None,
        );
        assert!(amb.contains("/p/amb/") && amb.contains("gradle, maven") && amb.contains("--build-tool"), "{amb}");
        let none = describe_import_problem("BLOCKED", "file:///p/x/", "noBuildSystemFound", &[], None, None);
        assert!(none.contains("no supported build system"), "{none}");
        // Unknown reasons are shown as they are rather than hidden.
        let other = describe_import_problem("BLOCKED", "file:///p/x/", "somethingNew", &[], None, None);
        assert!(other.contains("somethingNew"), "{other}");
    }

    #[test]
    fn failed_imports_point_at_the_server_log() {
        let f = describe_import_problem("FAILED", "file:///p/app/", "Gradle sync failed", &[], None, None);
        assert!(f.contains("/p/app/") && f.contains("Gradle sync failed") && f.contains("server log"), "{f}");
    }

    #[test]
    fn failed_imports_name_the_log_file_when_the_server_announced_one() {
        let f = describe_import_problem("FAILED", "file:///p/app/", "Gradle sync failed", &[], Some("/tmp/x/server.log"), None);
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

    const SYNC_LOG: &str = "\
2026-11-21 11:00:00,000 [   1000]   INFO - #c.j.l.Server - Log file: /tmp/x/server.log
2026-11-21 11:00:01,000 [   2000]   INFO - #c.j.l.Import - Importing with Gradle
2026-11-21 11:00:05,000 [   6000]   WARN - #c.j.l.Android - Compose Preview failed for ':app' (variant 'release'): Unit tests are disabled for this variant
2026-11-21 11:00:05,100 [   6100]  ERROR - #c.j.l.Import - Unable to import a Gradle project
2026-11-21 11:00:05,200 [   6200]   INFO - #c.j.l.Import - Gradle sync failed
";

    #[test]
    fn the_cause_of_a_failed_sync_is_found_in_the_log() {
        assert_eq!(
            failure_cause(SYNC_LOG).as_deref(),
            Some("Compose Preview failed for ':app' (variant 'release'): Unit tests are disabled for this variant")
        );
        // Only the last import counts: a cause from an earlier one is not reused.
        let later = format!("{SYNC_LOG}2026-11-21 11:01:00,000 [  1000]   INFO - #i - Importing with Gradle\n2026-11-21 11:01:05,000 [  2000]   INFO - #i - Gradle sync failed\n");
        assert_eq!(failure_cause(&later), None);
        assert_eq!(failure_cause("nothing failed here\n"), None);
        // Lines without IntelliJ's prefix are taken as they are.
        assert_eq!(
            failure_cause("e: file:///p/build.gradle.kts:3:1: Unresolved reference: android\nGradle sync failed\n").as_deref(),
            Some("e: file:///p/build.gradle.kts:3:1: Unresolved reference: android")
        );
    }

    /// Trimmed from a real failed sync (kotlin-lsp 271.2085.0, an Android project
    /// without `local.properties`): Gradle's file-watching warning comes first.
    const GRADLE_LOG: &str = "\
2026-11-24 22:27:17,076 [   2868]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: Caught exception: Could not start the FSEvents stream: /p/klang
2026-11-24 22:27:17,100 [   2900]   INFO - #c.i.l.s.i.ImportExecutionKt - [IMPORT STD]: > Task :app:generateDebugBuildConfig
2026-11-24 22:27:17,551 [   3343]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: FAILURE: Build failed with an exception.
2026-11-24 22:27:17,551 [   3343]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: * What went wrong:
2026-11-24 22:27:17,551 [   3343]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: Failed to calculate the value of property 'bootClasspath'.
2026-11-24 22:27:17,551 [   3343]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: > Failed to calculate the value of property 'bootClasspath'.
2026-11-24 22:27:17,551 [   3343]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]:    > SDK location not found. Define a valid SDK location with an ANDROID_HOME environment variable.
2026-11-24 22:27:17,551 [   3343]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]:
2026-11-24 22:27:17,551 [   3343]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: * Try:
2026-11-24 22:27:17,551 [   3343]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: > Run with --info or --debug option to get more log output.
2026-11-24 22:27:17,551 [   3343]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: * Exception is:
2026-11-24 22:27:17,553 [   3345]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: Caused by: org.gradle.api.internal.provider.AbstractProperty$PropertyQueryException: Failed to calculate the value of property 'bootClasspath'.
2026-11-24 22:27:17,555 [   3347]   WARN - #c.i.l.s.i.ImportExecutionKt - [IMPORT ERR]: Caused by: com.android.builder.errors.EvalIssueException: SDK location not found (from the exception).
2026-11-24 22:27:17,585 [   3377]   WARN - #c.i.l.s.i.ImportExecutionKt - Unable to import a Gradle project: org.gradle.tooling.BuildActionFailureException: The supplied phased action failed with an exception.
com.jetbrains.ls.buildtool.api.WorkspaceImportException: Gradle sync failed
Caused by: com.android.builder.errors.EvalIssueException: SDK location not found (after the marker).
";

    #[test]
    fn gradles_own_warnings_are_not_taken_for_the_cause() {
        // The innermost line of `What went wrong`, not the FSEvents warning before it (#50).
        assert_eq!(
            failure_cause(GRADLE_LOG).as_deref(),
            Some("SDK location not found. Define a valid SDK location with an ANDROID_HOME environment variable.")
        );
        // Without `What went wrong`: the last `Caused by:` within the import, without its class.
        let no_summary: String = GRADLE_LOG.lines().filter(|l| !l.contains("[IMPORT ERR]: ") || l.contains("Caused by") || l.contains("FSEvents")).map(|l| format!("{l}\n")).collect();
        assert_eq!(failure_cause(&no_summary).as_deref(), Some("SDK location not found (from the exception)."));
        // A build script that does not compile: the compiler's error, which has the
        // file and line, rather than the first line of `What went wrong`.
        let script = "\
[IMPORT ERR]: Caught exception: Could not start the FSEvents stream: /p
[IMPORT ERR]: * What went wrong:
[IMPORT ERR]: Script compilation error:
[IMPORT ERR]:   Line 3: this is not valid kotlin
[IMPORT ERR]:                                   ^ Expecting an element
[IMPORT ERR]: 1 error
[IMPORT ERR]: e: file:///p/build.gradle.kts:3:25: Expecting an element
Unable to import a Gradle project
";
        assert_eq!(failure_cause(script).as_deref(), Some("e: file:///p/build.gradle.kts:3:25: Expecting an element"));
        // Only the warning: there is no cause to name.
        let only_noise: String = GRADLE_LOG.lines().filter(|l| l.contains("FSEvents") || l.contains("sync failed")).map(|l| format!("{l}\n")).collect();
        assert_eq!(failure_cause(&only_noise), None);
    }

    #[test]
    fn a_failed_import_warning_names_the_cause_from_the_server_log() {
        let dir = std::env::temp_dir().join(format!("lsp-client-kt-cause-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("server.log");
        std::fs::write(&log, SYNC_LOG).unwrap();
        let mut kt = KotlinLsp::default();
        let mut facts = ServerFacts::default();
        kt.on_message(&json!({"method": "window/logMessage", "params": {"message": format!("Log file: {}", log.display())}}), &mut facts);
        kt.on_message(&state("FINISHED", "FAILED", "Gradle sync failed"), &mut facts);
        let _ = std::fs::remove_dir_all(&dir);
        let f = &facts.import.failures[0];
        assert!(f.contains("(cause, from the server log: Compose Preview failed for ':app'"), "{f}");
    }

    #[test]
    fn the_build_tool_question_is_noticed() {
        let mut kt = KotlinLsp::default();
        let mut facts = ServerFacts::default();
        kt.on_message(&json!({"method": "window/showMessageRequest", "id": 3, "params": {"message": "Restart?", "actions": [{"title": "OK"}]}}), &mut facts);
        assert!(!facts.build_tool_asked);
        kt.on_message(&json!({"method": "window/showMessageRequest", "id": 4, "params": buttons()}), &mut facts);
        assert!(facts.build_tool_asked);
    }

    #[test]
    fn missing_generated_sources_get_a_note() {
        let kt = KotlinLsp::default();
        let k2 = json!({"message": "Unresolved reference 'BuildConfig'."});
        let k1 = json!({"message": "Unresolved reference: R"});
        let other = json!({"message": "Unresolved reference 'Rx'."});
        let notes = kt.diagnostics_notes(&[&k2, &k1, &other]);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("`BuildConfig` and `R` are generated"), "{}", notes[0]);
        assert!(kt.diagnostics_notes(&[&other]).is_empty());
        assert!(servers::generic().diagnostics_notes(&[&k2]).is_empty());
    }
}
