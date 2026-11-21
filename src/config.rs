use std::path::{Path, PathBuf};

#[derive(serde::Deserialize, Default, Debug)]
pub struct FileConfig {
    pub server: Option<String>,
    pub language_id: Option<String>,
    pub no_server_stdio_flag: Option<bool>,
    /// Build tool to use when a project has more than one (e.g. "gradle" or "maven").
    pub build_tool: Option<String>,
    /// Sent to the server as `initializationOptions` in `initialize`, as given.
    /// Which keys exist is up to the server (kotlin-lsp: `indexDir`, `buildTools`, ...).
    pub initialization_options: Option<toml::Value>,
    /// Server profile: `auto` (the default), `generic`, or a name from `servers::KNOWN`.
    pub server_profile: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_options_become_json_unchanged() {
        let cfg: FileConfig = toml::from_str(
            r#"
            server = "kotlin-lsp"
            build_tool = "gradle"

            [initialization_options]
            indexDir = "/tmp/idx"
            phasedImport = true
            projects = [{ path = "app", tool = "gradle" }]

            [initialization_options.buildTools]
            "file:///p/" = "maven"
            "#,
        )
        .unwrap();
        let json = serde_json::to_value(cfg.initialization_options.unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "indexDir": "/tmp/idx",
                "phasedImport": true,
                "projects": [{"path": "app", "tool": "gradle"}],
                "buildTools": {"file:///p/": "maven"},
            })
        );
        assert_eq!(cfg.build_tool.as_deref(), Some("gradle"));
    }

    #[test]
    fn options_are_optional() {
        let cfg: FileConfig = toml::from_str("server = \"x\"").unwrap();
        assert!(cfg.initialization_options.is_none() && cfg.build_tool.is_none());
    }
}
