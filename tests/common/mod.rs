// Shared test helpers. Not every test binary uses every helper, so silence the
// dead-code lint that would otherwise fire per-crate.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use strix::app::App;
use strix::crossterm::event::{KeyCode, KeyEvent};
use tempfile::TempDir;

/// Press a plain character key on `app`, as if typed at the keyboard.
pub fn press(app: &mut App, ch: char) {
    app.on_key(KeyEvent::from(KeyCode::Char(ch)));
}

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

/// Run a git command in `dir` with extra environment (e.g. fixed commit dates),
/// asserting success.
pub fn git_env(dir: &Path, envs: &[(&str, &str)], args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let status = cmd.args(args).status().expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// `git init` on branch `main` with a deterministic identity (no signing).
fn setup_identity(path: &Path) {
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    git(path, &["config", "commit.gpgsign", "false"]);
}

/// Commit staged changes with a fixed author + committer date, so history walks
/// (which sort by commit time) are deterministic across runs.
fn commit_at(path: &Path, message: &str, date: &str) {
    git_env(
        path,
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
        &["commit", "-q", "-m", message],
    );
}

/// A fresh repository on branch `main` with one committed file and a
/// deterministic identity (no signing, fixed user).
pub fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-q", "-m", "init"]);
    dir
}

/// A repository with three linear commits and known content: `init` (adds
/// README), `add a` (adds a.txt), `edit readme` (appends a known line).
pub fn init_repo_with_history() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "README.md"]);
    commit_at(path, "init", "2021-01-01T00:00:00");
    write(path, "a.txt", "alpha\n");
    git(path, &["add", "a.txt"]);
    commit_at(path, "add a", "2021-01-02T00:00:00");
    write(path, "README.md", "# test\nsecond line\n");
    git(path, &["add", "README.md"]);
    commit_at(path, "edit readme", "2021-01-03T00:00:00");
    dir
}

/// A repository with a feature branch merged back into `main` (a real merge
/// commit with two parents), to exercise multi-parent walks and the rail graph.
pub fn init_repo_with_branches() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "."]);
    commit_at(path, "init", "2021-01-01T00:00:00");
    git(path, &["checkout", "-q", "-b", "feature"]);
    write(path, "feature.txt", "feature\n");
    git(path, &["add", "."]);
    commit_at(path, "add feature", "2021-01-02T00:00:00");
    git(path, &["checkout", "-q", "main"]);
    write(path, "main.txt", "main\n");
    git(path, &["add", "."]);
    commit_at(path, "add main file", "2021-01-03T00:00:00");
    git_env(
        path,
        &[
            ("GIT_AUTHOR_DATE", "2021-01-04T00:00:00"),
            ("GIT_COMMITTER_DATE", "2021-01-04T00:00:00"),
        ],
        &["merge", "--no-ff", "-q", "-m", "merge feature", "feature"],
    );
    dir
}

/// A repository with `main` and `feature` genuinely diverged from a common base:
/// after the shared `init` commit, `feature` adds two commits and `main` adds one
/// — no merge. `merge-base(main, feature)` is the `init` commit and differs from
/// both tips, so three-dot (`main...feature`) and two-dot ranges differ. `feature`
/// is the checked-out branch (HEAD), so `strix diff main` reviews what `feature`
/// adds.
///
/// Layout:
/// - `init`         (base, on both)      README.md
/// - `main`:  base → `main change`       README.md edited, main-only.txt added
/// - `feature`: base → `feat one` → `feat two`  feature.txt, feature2.txt, rename
pub fn init_repo_with_diverged_branches() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\nshared\n");
    write(path, "shared.txt", "alpha\nbeta\ngamma\n");
    git(path, &["add", "."]);
    commit_at(path, "init", "2021-01-01T00:00:00");

    // main advances past the base (but never merges feature).
    write(path, "main-only.txt", "main\n");
    write(path, "README.md", "# test\nshared\nmain edit\n");
    git(path, &["add", "."]);
    commit_at(path, "main change", "2021-01-02T00:00:00");

    // feature branches off the base and adds its own commits.
    git(path, &["checkout", "-q", "-b", "feature", "HEAD~1"]);
    write(path, "feature.txt", "feature\n");
    git(path, &["add", "."]);
    commit_at(path, "feat one", "2021-01-03T00:00:00");
    // A rename+modify (exercises -M) plus a second added file.
    git(path, &["mv", "shared.txt", "renamed.txt"]);
    write(path, "renamed.txt", "alpha\nbeta\ngamma\ndelta\n");
    write(path, "feature2.txt", "more\n");
    git(path, &["add", "."]);
    commit_at(path, "feat two", "2021-01-04T00:00:00");
    dir
}

/// A repository with an orphan `unrelated` branch: a second root with no shared
/// history, so `merge-base(main, unrelated)` fails. `main` is left checked out.
pub fn init_repo_with_orphan_branch() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "."]);
    commit_at(path, "init", "2021-01-01T00:00:00");

    git(path, &["checkout", "-q", "--orphan", "unrelated"]);
    git(path, &["rm", "-rfq", "--cached", "."]);
    write(path, "other.txt", "unrelated\n");
    git(path, &["add", "."]);
    commit_at(path, "orphan root", "2021-01-02T00:00:00");

    git(path, &["checkout", "-q", "main"]);
    dir
}

/// A repository with identity configured but no commits (unborn HEAD).
pub fn init_empty_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_identity(dir.path());
    dir
}

/// A repository whose latest commit ("add binary") introduces a file containing
/// NUL bytes, for binary-detection tests.
pub fn setup_for_binary() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "."]);
    commit_at(path, "init", "2021-01-01T00:00:00");
    write(path, "bin.dat", "a\0b\0c\n");
    git(path, &["add", "."]);
    commit_at(path, "add binary", "2021-01-02T00:00:00");
    dir
}
