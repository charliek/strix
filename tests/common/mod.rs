// Shared test helpers. Not every test binary uses every helper, so silence the
// dead-code lint that would otherwise fire per-crate.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// Run a git command in `dir`, asserting success.
pub fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// Write a file (creating parent directories) inside `dir`.
pub fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// A fresh repository on branch `main` with one committed file and a
/// deterministic identity (no signing, fixed user).
pub fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    git(path, &["config", "commit.gpgsign", "false"]);
    write(path, "README.md", "# test\n");
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-q", "-m", "init"]);
    dir
}
