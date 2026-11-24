use serde_json::{json, Value};
use std::path::PathBuf;

/// Metadata stored on disk so that subsequent CLI invocations can find a
/// running daemon without the user supplying connection details each time.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub workspace: String,
    pub server: String,
    pub language_id: String,
    pub pid: u32,
    pub socket: String,
    pub started_at: u64, // Unix timestamp seconds
    pub initialized: bool,
    /// The server's own log file, if it announced one (kotlin-lsp does).
    pub server_log: Option<String>,
    /// Where the daemon writes the server's stderr (see [`stderr_path`]); `None`
    /// when it is not captured (`--verbose` shows it on the terminal instead).
    pub server_stderr: Option<String>,
    /// Name of the server profile in use (see `servers`); empty in session files
    /// written before profiles existed.
    pub profile: String,
    /// lsp-client version that started the daemon; empty in session files
    /// written before this field existed.
    pub version: String,
    /// Identifies the build of the binary that started the daemon (see
    /// [`build_id`]); empty in session files written before this field existed.
    pub build: String,
}

/// Identifies this build of lsp-client: the modification time of the running
/// executable. The version number stays the same across rebuilds and
/// `cargo install` from a newer checkout; the executable's time does not.
/// Empty if it cannot be read.
pub fn build_id() -> String {
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()))
        .unwrap_or_default()
}

/// Stable, deterministic hash of a workspace path used as the session filename.
/// We roll our own so we don't need a crypto dependency — collision resistance
/// across different paths on the same machine is all we need.
fn workspace_hash(path: &str) -> String {
    let mut h: u64 = 5381;
    for b in path.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u64);
    }
    format!("{h:016x}")
}

fn sessions_dir() -> PathBuf {
    let cache = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache")
        });
    cache.join("lsp-client").join("sessions")
}

pub fn session_path(workspace: &str) -> PathBuf {
    sessions_dir().join(format!("{}.json", workspace_hash(workspace)))
}

/// Where the daemon for `workspace` writes its language server's stderr, next to
/// the session file. Known from the workspace alone, so `session start` can read
/// it when the daemon died before it wrote the session file.
pub fn stderr_path(workspace: &str) -> PathBuf {
    sessions_dir().join(format!("{}.stderr", workspace_hash(workspace)))
}

impl SessionInfo {
    pub fn load(workspace: &str) -> Option<Self> {
        let text = std::fs::read_to_string(session_path(workspace)).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        Some(Self {
            workspace: v["workspace"].as_str()?.to_owned(),
            server: v["server"].as_str()?.to_owned(),
            language_id: v["language_id"].as_str().unwrap_or("").to_owned(),
            pid: v["pid"].as_u64()? as u32,
            socket: v["socket"].as_str()?.to_owned(),
            started_at: v["started_at"].as_u64().unwrap_or(0),
            initialized: v["initialized"].as_bool().unwrap_or(false),
            server_log: v["server_log"].as_str().map(str::to_owned),
            server_stderr: v["server_stderr"].as_str().map(str::to_owned),
            profile: v["profile"].as_str().unwrap_or("").to_owned(),
            version: v["version"].as_str().unwrap_or("").to_owned(),
            build: v["build"].as_str().unwrap_or("").to_owned(),
        })
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = session_path(&self.workspace);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let v = json!({
            "workspace":   self.workspace,
            "server":      self.server,
            "language_id": self.language_id,
            "pid":         self.pid,
            "socket":      self.socket,
            "started_at":  self.started_at,
            "initialized": self.initialized,
            "server_log":  self.server_log,
            "server_stderr": self.server_stderr,
            "profile":     self.profile,
            "version":     self.version,
            "build":       self.build,
        });
        std::fs::write(path, v.to_string())
    }

    pub fn delete(workspace: &str) {
        let _ = std::fs::remove_file(session_path(workspace));
    }

    /// A warning if the daemon was started by a different lsp-client build than
    /// this one: it may not understand newer requests, and would answer them
    /// with an error that is easy to miss.
    pub fn version_mismatch_warning(&self) -> Option<String> {
        self.mismatch_warning(env!("CARGO_PKG_VERSION"), &build_id())
    }

    fn mismatch_warning(&self, ours: &str, our_build: &str) -> Option<String> {
        let fix = "run `lsp-client session stop` so the next command starts a matching one.";
        if self.version != ours {
            let theirs = if self.version.is_empty() { "an older version" } else { &self.version };
            return Some(format!("Warning: the daemon was started by lsp-client {theirs}, this is {ours}: {fix}"));
        }
        // Same version number: compare builds, unless either side cannot tell.
        if !self.build.is_empty() && !our_build.is_empty() && self.build != our_build {
            return Some(format!(
                "Warning: the daemon was started by a different build of lsp-client {ours}: {fix}"
            ));
        }
        None
    }

    /// Returns true if the daemon process is still running.
    pub fn is_alive(&self) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &self.pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

pub fn unix_timestamp() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(version: &str) -> SessionInfo {
        SessionInfo {
            workspace: "/w".into(),
            server: "s".into(),
            language_id: "x".into(),
            pid: 1,
            socket: "/s".into(),
            started_at: 0,
            initialized: true,
            server_log: None,
            server_stderr: None,
            profile: String::new(),
            version: version.into(),
            build: "100.000000000".into(),
        }
    }

    #[test]
    fn same_version_and_build_does_not_warn() {
        assert!(info("1.0.0").mismatch_warning("1.0.0", "100.000000000").is_none());
    }

    #[test]
    fn other_or_missing_version_warns() {
        let w = info("0.0.1").mismatch_warning("1.0.0", "100.000000000").unwrap();
        assert!(w.contains("0.0.1") && w.contains("this is 1.0.0") && w.contains("session stop"));
        assert!(info("").mismatch_warning("1.0.0", "100.000000000").unwrap().contains("an older version"));
    }

    #[test]
    fn same_version_from_another_build_warns() {
        let w = info("1.0.0").mismatch_warning("1.0.0", "200.000000000").unwrap();
        assert!(w.contains("different build of lsp-client 1.0.0"), "{w}");
    }

    #[test]
    fn an_unknown_build_on_either_side_does_not_warn() {
        assert!(info("1.0.0").mismatch_warning("1.0.0", "").is_none());
        let mut old = info("1.0.0");
        old.build.clear();
        assert!(old.mismatch_warning("1.0.0", "200.000000000").is_none());
    }

    #[test]
    fn this_build_has_an_id() {
        assert!(!build_id().is_empty());
    }
}
