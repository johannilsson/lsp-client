// Integration tests against kotlin-lsp, for project-import handling.
//
// They need `kotlin-lsp` on PATH (and, for the Gradle case, network access to
// resolve the Kotlin plugin), and are skipped with a note when it is missing.
// Run with:
//   cargo test --test kotlin_lsp -- --nocapture
//
// Each test builds a tiny project in a fresh temp directory, so kotlin-lsp
// always does a first import (it remembers the model per path).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_lsp-client");

fn kotlin_lsp_available() -> bool {
    Command::new("kotlin-lsp")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A project directory that stops its daemon and deletes itself on drop.
struct Project(PathBuf);

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!("lsp-client-kt-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/Main.kt"), "fun main() { println(\"hi\") }\n").unwrap();
        for (path, content) in files {
            std::fs::write(dir.join(path), content).unwrap();
        }
        Project(dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BINARY)
            .args(["--root", self.0.to_str().unwrap(), "--timeout", "4m"])
            .args(args)
            .output()
            .expect("failed to run lsp-client")
    }

    fn start(&self, extra: &[&str]) -> Output {
        let mut args = vec!["--server", "kotlin-lsp", "--language-id", "kotlin"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["session", "start", "--wait"]);
        self.run(&args)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = self.run(&["session", "stop"]);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const GRADLE: &str = "plugins { kotlin(\"jvm\") version \"2.4.21\" }\nrepositories { mavenCentral() }\n";
const MAVEN: &str = "<project><modelVersion>4.0.0</modelVersion><groupId>x</groupId><artifactId>t</artifactId><version>1</version></project>\n";

#[test]
fn folder_without_a_build_file_is_reported() {
    if !kotlin_lsp_available() {
        eprintln!("skipped: kotlin-lsp not on PATH");
        return;
    }
    let p = Project::new("nobuild", &[]);
    let out = p.start(&[]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("no supported build system"), "stderr: {}", stderr(&out));
    // The daemon keeps running, so `wait-ready` gives the same answer.
    let again = p.run(&["session", "wait-ready"]);
    assert_eq!(again.status.code(), Some(3), "stderr: {}", stderr(&again));
}

#[test]
fn ambiguous_build_tool_names_the_candidates() {
    if !kotlin_lsp_available() {
        eprintln!("skipped: kotlin-lsp not on PATH");
        return;
    }
    let p = Project::new("ambiguous", &[("build.gradle.kts", GRADLE), ("pom.xml", MAVEN)]);
    let out = p.start(&[]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("gradle, maven") && err.contains("--build-tool"), "stderr: {err}");
}

#[test]
fn build_tool_option_resolves_the_ambiguity() {
    if !kotlin_lsp_available() {
        eprintln!("skipped: kotlin-lsp not on PATH");
        return;
    }
    let p = Project::new("choose", &[("build.gradle.kts", GRADLE), ("pom.xml", MAVEN)]);
    let out = p.start(&["--build-tool", "gradle"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let main = p.path().join("src/Main.kt");
    let symbols = p.run(&["query", "symbols", main.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&symbols.stdout).contains("main"), "stderr: {}", stderr(&symbols));
}
