use std::path::{Path, PathBuf};

#[derive(serde::Deserialize, Default, Debug)]
pub struct FileConfig {
    pub server: Option<String>,
    pub language_id: Option<String>,
    pub no_server_stdio_flag: Option<bool>,
    /// Duration string, e.g. "30s", "2m"
    pub timeout: Option<String>,
    pub root: Option<String>,
}

/// Walk up from `start_dir` looking for `.lsp-client.toml`.
/// Returns the config file path and parsed config if found.
pub fn find_config(start_dir: &str) -> Option<(PathBuf, FileConfig)> {
    let mut dir = Path::new(start_dir);
    loop {
        let candidate = dir.join(".lsp-client.toml");
        if candidate.exists() {
            let contents = std::fs::read_to_string(&candidate).ok()?;
            let config: FileConfig = toml::from_str(&contents)
                .map_err(|e| {
                    eprintln!("Warning: failed to parse {}: {e}", candidate.display());
                })
                .ok()?;
            return Some((candidate, config));
        }
        dir = dir.parent()?;
    }
}
