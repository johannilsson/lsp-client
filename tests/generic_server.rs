// Tests with servers that are not rust-analyzer or kotlin-lsp.
//
// lsp-client works with any language server, so behaviour added for one server
// must not change how the others are treated. These tests use a tiny fake server
// (tests/fixtures/fake_lsp.py, needs python3) and a server that dies at startup.

use std::path::{Path, PathBuf};
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

/// A TCP port nothing is listening on.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn tcp_mode_does_not_start_a_server_it_does_not_know_how_to_start() {
    // `--tcp --server X` starts X when nothing is listening, which needs to know the
    // flags that make X listen on a port. Only kotlin-lsp's are known; for anything
    // else lsp-client must say so, not pass it kotlin-lsp's flags.
    let ws = Workspace::new("tcp-unknown");
    let port = free_port().to_string();
    let file = ws.0.join("a.txt");
    let started = Instant::now();
    let out = ws.run(&[
        "--tcp", "--port", &port, "--server", "python3", "--language-id", "plain",
        "query", "hover", file.to_str().unwrap(), "1", "1",
    ]);
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("does not know which flags") && err.contains("'python3'"), "stderr: {err}");
    assert!(!err.contains("kotlin-lsp"), "stderr: {err}");
    // It gave up at once; it did not start python3 and wait for it for 30 s.
    assert!(started.elapsed() < Duration::from_secs(15), "took {:?}", started.elapsed());
}

#[test]
fn tcp_mode_with_no_server_to_start_says_so() {
    let ws = Workspace::new("tcp-none");
    let port = free_port().to_string();
    let file = ws.0.join("a.txt");
    let out = ws.run(&["--tcp", "--port", &port, "--language-id", "plain", "query", "hover", file.to_str().unwrap(), "1", "1"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("No server listening"), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn an_unknown_profile_name_stops_tcp_mode_too() {
    let ws = Workspace::new("tcp-bad-profile");
    let port = free_port().to_string();
    let file = ws.0.join("a.txt");
    let out = ws.run(&[
        "--tcp", "--port", &port, "--server", "python3", "--server-profile", "bogus", "--language-id", "plain",
        "query", "hover", file.to_str().unwrap(), "1", "1",
    ]);
    assert_eq!(out.status.code(), Some(2));
}


#[test]
fn a_daemon_from_another_build_of_the_same_version_is_reported() {
    if !python3_available() {
        eprintln!("skipped: python3 not available");
        return;
    }
    let ws = Workspace::new("other-build");
    let out = ws.start_fake(&[], &[]);
    assert!(out.status.success(), "start failed: {}", stderr(&out));

    // The binary that started the daemon sees no mismatch.
    let same = ws.run(&["session", "status"]);
    assert!(!stderr(&same).contains("different build"), "{}", stderr(&same));

    // A copy has the same version but a newer modification time, as after a rebuild.
    let copy = std::env::temp_dir().join(format!("lsp-client-copy-{}", std::process::id()));
    std::fs::copy(BINARY, &copy).unwrap();
    let other = Command::new(&copy)
        .args(["--root", ws.0.to_str().unwrap(), "session", "status"])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&copy);
    assert!(
        stderr(&other).contains("different build of lsp-client") && stderr(&other).contains("session stop"),
        "{}",
        stderr(&other)
    );
}

#[test]
fn positions_can_be_given_as_file_line_col_or_with_symbol() {
    if !python3_available() {
        eprintln!("skipped: python3 not available");
        return;
    }
    let ws = Workspace::new("position-forms");
    std::fs::write(ws.0.join("a.txt"), "hello hello\n").unwrap();
    let out = ws.start_fake(&[], &[]);
    assert!(out.status.success(), "start failed: {}", stderr(&out));
    let file = ws.0.join("a.txt");
    let file = file.to_str().unwrap();

    let joined = ws.run(&["query", "hover", &format!("{file}:1:1")]);
    assert!(joined.status.success(), "{}", stderr(&joined));
    assert!(String::from_utf8_lossy(&joined.stdout).contains("hover from the fake server"));

    // The name is on the line twice: the first is used, and the note says so.
    let by_name = ws.run(&["query", "hover", &format!("{file}:1"), "--symbol", "hello"]);
    assert!(by_name.status.success(), "{}", stderr(&by_name));
    assert!(String::from_utf8_lossy(&by_name.stdout).contains("hover from the fake server"));
    assert!(stderr(&by_name).contains("2 times; using the first, at 1:1"), "{}", stderr(&by_name));

    let missing = ws.run(&["query", "hover", file, "1", "--symbol", "nope"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("`nope` is not on line 1"), "{}", stderr(&missing));
}

/// `query hover` on `line` of `file`, started in the background.
fn spawn_hover(ws: &Workspace, file: &Path, line: u32) -> std::process::Child {
    Command::new(BINARY)
        .args(["--root", ws.0.to_str().unwrap(), "query", "hover", file.to_str().unwrap(), &line.to_string(), "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run lsp-client")
}

#[test]
fn session_status_answers_while_another_client_runs_a_slow_query() {
    if !python3_available() {
        eprintln!("skipped: python3 not available");
        return;
    }
    let ws = Workspace::new("status-while-busy");
    let out = ws.start_fake(&[], &[("FAKE_LSP_HOVER_DELAY", "6")]);
    assert!(out.status.success(), "start failed: {}", stderr(&out));

    let file = ws.0.join("a.txt");
    let started = Instant::now();
    let hover = spawn_hover(&ws, &file, 1);
    std::thread::sleep(Duration::from_millis(500));

    let asked = Instant::now();
    let status = ws.run(&["session", "status"]);
    let took = asked.elapsed();
    assert!(status.status.success(), "{}", stderr(&status));
    assert!(String::from_utf8_lossy(&status.stdout).contains("import:      n/a"), "{}", String::from_utf8_lossy(&status.stdout));
    assert!(took < Duration::from_secs(3), "status waited for the query: {took:?}");

    let hover = hover.wait_with_output().unwrap();
    assert!(started.elapsed() >= Duration::from_secs(6), "the hover was not slow, so this proved nothing");
    assert!(String::from_utf8_lossy(&hover.stdout).contains("at line 1"), "{}", stderr(&hover));
}

#[test]
fn parallel_queries_each_get_their_own_answer() {
    if !python3_available() {
        eprintln!("skipped: python3 not available");
        return;
    }
    let ws = Workspace::new("parallel-queries");
    std::fs::write(ws.0.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let out = ws.start_fake(&[], &[("FAKE_LSP_HOVER_DELAY", "1")]);
    assert!(out.status.success(), "start failed: {}", stderr(&out));

    let file = ws.0.join("a.txt");
    let children: Vec<_> = (1..=3).map(|line| (line, spawn_hover(&ws, &file, line))).collect();
    for (line, child) in children {
        let o = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success(), "hover {line}: {}", stderr(&o));
        assert!(stdout.contains(&format!("at line {line}\n")) || stdout.trim_end().ends_with(&format!("at line {line}")), "hover {line} got: {stdout}");
    }
}
