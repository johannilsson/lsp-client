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
        });
        std::fs::write(path, v.to_string())
    }

    pub fn delete(workspace: &str) {
        let _ = std::fs::remove_file(session_path(workspace));
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
