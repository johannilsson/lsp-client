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
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(BINARY)
            .args(["--root", self.0.to_str().unwrap()])
            .args(args)
            .envs(env.iter().copied())
            .output()
            .expect("failed to run lsp-client")
    }

    /// `session start --wait` with the fake server, and `extra` environment / flags.
    fn start_fake(&self, extra_args: &[&str], env: &[(&str, &str)]) -> Output {
        let server = fake_server();
        let mut args = vec![
            "--timeout", "60s", "--server", server.to_str().unwrap(), "--no-server-stdio-flag",
            "--language-id", "plain",
        ];
        args.extend_from_slice(extra_args);
        args.extend_from_slice(&["session", "start", "--wait"]);
        self.run_env(&args, env)
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
    assert!(String::from_utf8_lossy(&status.stdout).contains("profile:     generic"));
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

const KOTLIN_NAME: &str = "IntelliJ Language Server by JetBrains";

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn status_stdout(ws: &Workspace) -> String {
    String::from_utf8_lossy(&ws.run(&["session", "status"]).stdout).into_owned()
}

#[test]
fn another_server_sending_kotlin_lsps_messages_is_not_treated_as_kotlin_lsp() {
    if !python3_available() {
        eprintln!("skipped: python3 not available");
        return;
    }
    // It reports a failed import in kotlin-lsp's own words, but it is not kotlin-lsp.
    let ws = Workspace::new("not-kotlin");
    let out = ws.start_fake(
        &[],
        &[("FAKE_LSP_INTELLIJ", "1"), ("FAKE_LSP_SERVER_NAME", "some-other-server"), ("FAKE_LSP_LOG_FILE", "/tmp/fake.log")],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(!stderr(&out).contains("import failed"), "stderr: {}", stderr(&out));
    let status = status_stdout(&ws);
    assert!(status.contains("profile:     generic"));
    // Its "Log file:" message is not taken at its word either.
    assert!(!status.contains("server log:"), "{status}");
}

#[test]
fn a_server_that_calls_itself_kotlin_lsp_gets_the_kotlin_lsp_profile() {
    if !python3_available() {
        eprintln!("skipped: python3 not available");
        return;
    }
    let ws = Workspace::new("is-kotlin");
    let out = ws.start_fake(
        &[],
        &[("FAKE_LSP_INTELLIJ", "1"), ("FAKE_LSP_SERVER_NAME", KOTLIN_NAME), ("FAKE_LSP_LOG_FILE", "/tmp/fake.log")],
    );
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    // The log file was announced before `initialize` was answered, so this also
    // checks that what arrives before the profile is known reaches it.
    assert!(stderr(&out).contains("Gradle sync failed") && stderr(&out).contains("server log: /tmp/fake.log"), "stderr: {}", stderr(&out));
    let status = status_stdout(&ws);
    assert!(status.contains("profile:     kotlin-lsp") && status.contains("server log:  /tmp/fake.log"), "{status}");
}

#[test]
fn the_profile_can_be_forced_off_or_on() {
    if !python3_available() {
        eprintln!("skipped: python3 not available");
        return;
    }
    // Calls itself kotlin-lsp, but the profile is turned off.
    let off = Workspace::new("forced-off");
    let out = off.start_fake(&["--server-profile", "generic"], &[("FAKE_LSP_INTELLIJ", "1"), ("FAKE_LSP_SERVER_NAME", KOTLIN_NAME)]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(status_stdout(&off).contains("profile:     generic"));

    // Does not say what it is, but the profile is asked for by name.
    let on = Workspace::new("forced-on");
    let out = on.start_fake(&["--server-profile", "kotlin-lsp"], &[("FAKE_LSP_INTELLIJ", "1")]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(status_stdout(&on).contains("profile:     kotlin-lsp"));
}

#[test]
fn an_unknown_profile_name_is_rejected_before_anything_starts() {
    let ws = Workspace::new("bad-profile");
    let out = ws.run(&["--server-profile", "bogus", "session", "status"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("unknown server profile 'bogus'"), "stderr: {}", stderr(&out));
}

