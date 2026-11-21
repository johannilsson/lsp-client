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

/// What a profile can record about the server for the daemon to use.
#[derive(Default)]
pub struct ServerFacts {
    pub import: ImportState,
}

/// Hooks a profile can override. Every hook has a do-nothing default, so the
/// generic profile is the trait with no overrides.
pub trait ServerProfile: Send {
    /// Name shown by `session status` and accepted by `--server-profile`.
    fn name(&self) -> &'static str;

    /// A message the server sent: a notification, or a request that the daemon
    /// answers by itself. Called for every one, with its method unchanged, so a
    /// profile must ignore what it does not know.
    fn on_message(&self, _msg: &Value, _facts: &mut ServerFacts) {}
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
        "kotlin-lsp" => Some(Box::new(kotlin_lsp::KotlinLsp)),
        _ => None,
    }
}

/// The profile for a server that called itself `server_info_name` in `initialize`.
fn detect(server_info_name: Option<&str>) -> Box<dyn ServerProfile> {
    if kotlin_lsp::is_kotlin_lsp(server_info_name) {
        return Box::new(kotlin_lsp::KotlinLsp);
    }
    generic()
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
    fn every_known_name_resolves() {
        for name in KNOWN {
            assert_eq!(by_name(name).unwrap().name(), *name);
        }
    }
}
