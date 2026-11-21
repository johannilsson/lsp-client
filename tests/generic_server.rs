// Tests with servers that are not rust-analyzer or kotlin-lsp.
//
// lsp-client works with any language server, so behaviour added for one server
// must not change how the others are treated. These tests use a tiny fake server
// (tests/fixtures/fake_lsp.py, needs python3) and a server that dies at startup.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_lsp-client");

fn python3_available() -> bool {
    Command::new("python3").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn fake_server() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_lsp.py")
}

/// A workspace directory that stops its daemon and deletes itself on drop.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lsp-client-generic-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        Workspace(dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BINARY)
            .args(["--root", self.0.to_str().unwrap()])
            .args(args)
            .output()
            .expect("failed to run lsp-client")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = self.run(&["session", "stop"]);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn server_that_never_ends_a_progress_token_is_still_ready() {
    if !python3_available() {
        eprintln!("skipped: python3 not available");
        return;
    }
    let ws = Workspace::new("stuck-token");
    let server = fake_server();
    let started = Instant::now();
    // A long timeout: the wait must not use all of it just because a token stays open.
    let out = ws.run(&[
        "--timeout", "5m", "--server", server.to_str().unwrap(), "--no-server-stdio-flag",
        "--language-id", "plain", "session", "start", "--wait",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(started.elapsed() < Duration::from_secs(30), "took {:?}", started.elapsed());

    // Queries work, and say nothing about project imports: this server has none.
    let file = ws.0.join("a.txt");
    let hover = ws.run(&["query", "hover", file.to_str().unwrap(), "1", "1"]);
    assert!(String::from_utf8_lossy(&hover.stdout).contains("hover from the fake server"));
    assert!(!String::from_utf8_lossy(&hover.stderr).contains("Warning"), "stderr: {}", String::from_utf8_lossy(&hover.stderr));
    let status = ws.run(&["session", "status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("import:      n/a"));
}

#[test]
fn server_that_exits_at_startup_fails_quickly_even_with_a_long_timeout() {
    let ws = Workspace::new("dead-server");
    let started = Instant::now();
    let out = ws.run(&[
        "--timeout", "5m", "--server", "false", "--language-id", "plain", "session", "start", "--wait",
    ]);
    assert_ne!(out.status.code(), Some(0));
    assert!(started.elapsed() < Duration::from_secs(30), "took {:?}", started.elapsed());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("exited"), "stderr: {err}");
}
