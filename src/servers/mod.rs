//! Server profiles.
//!
//! lsp-client works with any language server, so what it does for every server
//! stays at the protocol level. Behaviour that only one server needs (its own
//! custom notifications, its own questions, how its output should be tidied up)
//! lives in a profile in this module and runs only while that server is in use.
//! Everything else sees a server through the [`ServerProfile`] hooks, which do
//! nothing unless a profile overrides them.
//!
//! A profile is chosen once the server has answered `initialize`:
//! - `server_profile` in `.lsp-client.toml` or `--server-profile` picks one by
//!   name (`generic` turns every profile off), otherwise
//! - the server's own `serverInfo.name` is matched against the known profiles,
//! - and anything unknown gets the generic profile.
//!
//! To support another server's quirks, add a module next to `kotlin_lsp`, list it
//! in [`KNOWN`], [`by_name`] and [`detect`], and keep its code out of everything
//! else.

use serde_json::Value;

mod kotlin_lsp;

/// Names accepted by `--server-profile` / `server_profile`, besides `auto`.
pub const KNOWN: &[&str] = &["generic", "kotlin-lsp"];

/// Project import state, for servers that report it. Not part of the protocol, so
/// only a profile ever fills it in; for every other server it stays empty.
#[derive(Default)]
pub struct ImportState {
    /// The server announced a project import.
    pub expected: bool,
    /// The server reported the import as finished (successfully or not).
    pub finished: bool,
    /// One message per folder that was not imported, from the last finished import.
    pub failures: Vec<String>,
}

impl ImportState {
    /// For `lsp-client/status`: `"none"` if the server never announced an import
    /// (most servers), `"running"` while one is in progress, `"problem"` if the last
    /// one left a folder not imported, otherwise `"ok"`.
    pub fn state(&self) -> &'static str {
        if !self.expected {
            "none"
        } else if !self.finished {
            "running"
        } else if !self.failures.is_empty() {
            "problem"
        } else {
            "ok"
        }
    }
}

/// What a profile can record about the server for the daemon to use.
#[derive(Default)]
pub struct ServerFacts {
    pub import: ImportState,
    /// The server's own log file, if it announces one.
    pub server_log: Option<String>,
    /// The server asked which build tool to use (kotlin-lsp's "Use Gradle / Use
    /// Maven"), so `--build-tool` had something to answer.
    pub build_tool_asked: bool,
}

/// Hooks a profile can override. Every hook has a do-nothing default, so the
/// generic profile is the trait with no overrides.
pub trait ServerProfile: Send {
    /// Name shown by `session status` and accepted by `--server-profile`.
    fn name(&self) -> &'static str;

    /// A message the server sent: a notification, or a request that the daemon
    /// answers by itself. Called for every one, with its method unchanged, so a
    /// profile must ignore what it does not know.
    fn on_message(&mut self, _msg: &Value, _facts: &mut ServerFacts) {}

    /// The answer to a request from the server, if this profile has one. `None`
    /// leaves it to the daemon's default (`null`, or the shape the protocol needs).
    /// `build_tool` is `--build-tool`, for servers that ask which one to use.
    fn answer_request(&mut self, _method: &str, _params: &Value, _build_tool: Option<&str>) -> Option<Value> {
        None
    }

    /// The arguments that make the server listen on `host:port` instead of speaking
    /// over stdio, for `--tcp --server <binary>`, which starts the server when nothing
    /// is listening. `None` if the profile does not know how to start its server that
    /// way (then lsp-client says so, rather than guessing flags).
    fn tcp_start_args(&self, _host: &str, _port: u16) -> Option<Vec<String>> {
        None
    }

    /// Tidy the markdown of a hover answer, before the generic clean-up (which turns
    /// any remaining editor `command:` link into its text).
    fn rewrite_hover(&self, markdown: String) -> String {
        markdown
    }

    /// Whether to show a symbol from `workspace/symbol`.
    fn keep_workspace_symbol(&self, _symbol: &Value) -> bool {
        true
    }

    /// Whether to show a diagnostic. `source` is the text of the file, if it could be
    /// read; without it a profile that needs the text keeps the diagnostic.
    fn keep_diagnostic(&self, _diagnostic: &Value, _source: Option<&str>) -> bool {
        true
    }

    /// Notes to print (on stderr) after the diagnostics of a file, e.g. what a known
    /// error usually means with this server. One line each.
    fn diagnostics_notes(&self, _diagnostics: &[&Value]) -> Vec<String> {
        Vec::new()
    }
}

struct Generic;

impl ServerProfile for Generic {
    fn name(&self) -> &'static str {
        "generic"
    }
}

/// The profile that does nothing special.
pub fn generic() -> Box<dyn ServerProfile> {
    Box::new(Generic)
}

/// The profile called `name` (one of [`KNOWN`]).
pub fn by_name(name: &str) -> Option<Box<dyn ServerProfile>> {
    match name {
        "generic" | "none" => Some(generic()),
        "kotlin-lsp" => Some(Box::new(kotlin_lsp::KotlinLsp::default())),
        _ => None,
    }
}

/// The profile for a server that called itself `server_info_name` in `initialize`.
fn detect(server_info_name: Option<&str>) -> Box<dyn ServerProfile> {
    if kotlin_lsp::is_kotlin_lsp(server_info_name) {
        return Box::new(kotlin_lsp::KotlinLsp::default());
    }
    generic()
}

/// The profile for a server that has not started yet, so there is no `serverInfo`:
/// the one asked for by name, or (`None` or `auto`) the one that matches the
/// server's binary name. Used where the profile is needed before `initialize`, such
/// as starting a server in TCP mode.
pub fn for_binary(requested: Option<&str>, server_bin: &str) -> Result<Box<dyn ServerProfile>, String> {
    match requested {
        None | Some("auto") if kotlin_lsp::is_kotlin_lsp_binary(server_bin) => Ok(Box::new(kotlin_lsp::KotlinLsp::default())),
        None | Some("auto") => Ok(generic()),
        Some(name) => select(Some(name), None),
    }
}

/// Choose the profile: the one asked for by name, or (`None` or `auto`) the one that
/// matches the server's `serverInfo.name`. An unknown name is an error.
pub fn select(requested: Option<&str>, server_info_name: Option<&str>) -> Result<Box<dyn ServerProfile>, String> {
    match requested {
        None | Some("auto") => Ok(detect(server_info_name)),
        Some(name) => by_name(name).ok_or_else(|| {
            format!("unknown server profile '{name}' (known: auto, {})", KNOWN.join(", "))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KOTLIN_NAME: &str = "IntelliJ Language Server by JetBrains";

    fn chosen(requested: Option<&str>, server: Option<&str>) -> &'static str {
        select(requested, server).unwrap().name()
    }

    #[test]
    fn the_profile_follows_what_the_server_calls_itself() {
        assert_eq!(chosen(None, Some(KOTLIN_NAME)), "kotlin-lsp");
        assert_eq!(chosen(Some("auto"), Some(KOTLIN_NAME)), "kotlin-lsp");
        assert_eq!(chosen(None, Some("rust-analyzer")), "generic");
        // pyright, for one, sends no serverInfo.
        assert_eq!(chosen(None, None), "generic");
    }

    #[test]
    fn a_profile_can_be_forced_or_turned_off() {
        assert_eq!(chosen(Some("kotlin-lsp"), None), "kotlin-lsp");
        assert_eq!(chosen(Some("generic"), Some(KOTLIN_NAME)), "generic");
        assert_eq!(chosen(Some("none"), Some(KOTLIN_NAME)), "generic");
    }

    #[test]
    fn an_unknown_profile_is_an_error_that_lists_the_known_ones() {
        let err = select(Some("bogus"), None).err().unwrap();
        assert!(err.contains("bogus") && err.contains("kotlin-lsp") && err.contains("generic"), "{err}");
    }

    #[test]
    fn import_state_follows_what_a_profile_records() {
        let mut st = ImportState::default();
        assert_eq!(st.state(), "none", "a server that never announces an import");
        st.expected = true;
        assert_eq!(st.state(), "running");
        st.finished = true;
        assert_eq!(st.state(), "ok");
        st.failures.push("project not imported for /p/".into());
        assert_eq!(st.state(), "problem");
    }

    #[test]
    fn a_server_that_has_not_started_is_recognised_by_its_binary_name() {
        for bin in ["kotlin-lsp", "/opt/kotlin-lsp/bin/intellij-server", "kotlin-lsp.sh"] {
            assert_eq!(for_binary(None, bin).unwrap().name(), "kotlin-lsp", "{bin}");
        }
        assert_eq!(for_binary(None, "rust-analyzer").unwrap().name(), "generic");
        // A profile asked for by name wins over the binary name, either way.
        assert_eq!(for_binary(Some("generic"), "kotlin-lsp").unwrap().name(), "generic");
        assert_eq!(for_binary(Some("kotlin-lsp"), "my-wrapper").unwrap().name(), "kotlin-lsp");
        assert!(for_binary(Some("bogus"), "x").is_err());
    }

    #[test]
    fn only_a_profile_that_knows_the_flags_can_start_a_server_in_tcp_mode() {
        assert_eq!(generic().tcp_start_args("127.0.0.1", 9999), None);
        assert_eq!(
            by_name("kotlin-lsp").unwrap().tcp_start_args("127.0.0.1", 9999),
            Some(vec!["--socket".to_string(), "127.0.0.1:9999".to_string(), "--multi-client".to_string()])
        );
    }

    #[test]
    fn every_known_name_resolves() {
        for name in KNOWN {
            assert_eq!(by_name(name).unwrap().name(), *name);
        }
    }
}
