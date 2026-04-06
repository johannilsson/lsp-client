// Integration tests for lsp-client using rust-analyzer on a small fixture project.
//
// Run with:
//   cargo test --test integration -- --test-threads=1
//
// The daemon is started once and reused across tests. Running with more than
// one thread risks contention on the daemon's single-connection accept loop.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::OnceLock;

const BINARY: &str = env!("CARGO_BIN_EXE_lsp-client");

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-project")
}

fn fixture_file(name: &str) -> String {
    fixture_root()
        .join("src")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn lsp_cmd() -> Command {
    let mut cmd = Command::new(BINARY);
    cmd.arg("--server")
        .arg("rust-analyzer")
        .arg("--no-server-stdio-flag")
        .arg("--language-id")
        .arg("rust")
        .arg("--root")
        .arg(fixture_root());
    cmd
}

// ---------------------------------------------------------------------------
// Daemon lifecycle
// ---------------------------------------------------------------------------

static DAEMON_READY: OnceLock<()> = OnceLock::new();

fn setup() {
    DAEMON_READY.get_or_init(|| {
        let running = lsp_cmd()
            .args(["session", "status"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !running {
            let out = lsp_cmd()
                .args(["session", "start", "--wait"])
                .output()
                .expect("failed to run session start");
            assert!(
                out.status.success(),
                "daemon failed to start:\n{}",
                String::from_utf8_lossy(&out.stderr),
            );
        }
        // Pre-warm: open lib.rs and wait for rust-analyzer to finish analyzing it.
        // Without this, the first definition/hover query on a cold daemon may get
        // empty results because rust-analyzer hasn't indexed the file yet.
        let _ = lsp_cmd()
            .args(["--wait-for-index", "query", "symbols", &fixture_file("lib.rs")])
            .output();
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn run(args: &[&str]) -> Output {
    setup();
    lsp_cmd()
        .args(args)
        .output()
        .expect("failed to run lsp-client")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "expected success\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// Fixture source positions (1-based):
//
//   lib.rs line  1, col  8  →  `add`   in `pub fn add(...)`
//   lib.rs line  5, col 12  →  `Point` in `pub struct Point`
//   lib.rs line 12, col  9  →  `Point` in `        Point { x, y }` (constructor use)

#[test]
fn test_symbols() {
    let out = run(&["query", "symbols", &fixture_file("lib.rs")]);
    assert_ok(&out);
    let s = stdout(&out);
    assert!(s.contains("add"), "expected 'add': {s}");
    assert!(s.contains("Point"), "expected 'Point': {s}");
    assert!(s.contains("new"), "expected 'new': {s}");
    assert!(s.contains("distance"), "expected 'distance': {s}");
}

#[test]
fn test_hover() {
    // Hover over `add` at line 1, col 8.
    let out = run(&["query", "hover", &fixture_file("lib.rs"), "1", "8"]);
    assert_ok(&out);
    let s = stdout(&out);
    assert!(s.contains("i32"), "expected type info with 'i32': {s}");
}

#[test]
fn test_definition() {
    // `Point` used at line 12, col 9 should resolve to the struct at line 5.
    let out = run(&["query", "definition", &fixture_file("lib.rs"), "12", "9"]);
    assert_ok(&out);
    let s = stdout(&out);
    assert!(s.contains("lib.rs:5:"), "expected definition at lib.rs:5:, got: {s}");
}

#[test]
fn test_diagnostics_clean() {
    let out = run(&["query", "diagnostics", &fixture_file("lib.rs")]);
    assert_ok(&out);
    let s = stdout(&out);
    assert_eq!(s.trim(), "No diagnostics.", "expected no diagnostics: {s}");
}

#[test]
fn test_capabilities() {
    let out = run(&["capabilities"]);
    assert_ok(&out);
    let s = stdout(&out);
    assert!(s.contains("hover"), "expected hover capability: {s}");
    assert!(s.contains("definition"), "expected definition capability: {s}");
}

#[test]
fn test_json_flag() {
    let out = run(&["--json", "query", "symbols", &fixture_file("lib.rs")]);
    assert_ok(&out);
    let s = stdout(&out);
    let json: serde_json::Value =
        serde_json::from_str(&s).unwrap_or_else(|e| panic!("invalid JSON: {e}\noutput: {s}"));
    assert_eq!(json["ok"], serde_json::json!(true), "expected ok=true: {s}");
    assert!(json["result"].is_array(), "expected result array: {s}");
}
