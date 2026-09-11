use std::time::Duration;

use strix::comments::{self, Branch};
use strix::git::Repo;
use strix::watch;
use tempfile::tempdir;

mod common;

/// End-to-end: a real file change under the watched root produces a signal.
/// Timing-based (FS watcher + debounce) but with a generous timeout, so a
/// single write should always be observed well within it.
#[test]
fn watcher_signals_on_a_file_change() {
    let dir = tempdir().expect("tempdir");
    let rx = watch::spawn(dir.path().to_path_buf(), Vec::new()).expect("spawn watcher");
    // Let the watch register before touching files.
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(dir.path().join("hello.txt"), "hi").expect("write");

    common::expect_signal(&rx, "a file change");
}

/// A commit made in a *linked* worktree updates refs / the reflog under the
/// shared common dir — outside the linked worktree's working tree — so only the
/// extra common-dir watch (not the recursive workdir watch) can catch it. This
/// is the linked-worktree gap C2c closes.
#[test]
fn watcher_signals_on_a_linked_worktree_commit() {
    let repos = common::init_repo_with_worktree();
    let wt = repos.worktree();
    let repo = Repo::open(&wt).expect("open linked worktree");

    // Exactly what `run()` passes to the watcher: the working tree plus any
    // outside-workdir state root (the common dir, for a linked worktree).
    let extra = repo.watch_roots();
    assert!(
        !extra.is_empty(),
        "a linked worktree must add an outside-workdir watch root (the common dir)"
    );

    let rx = watch::spawn(repo.workdir().to_path_buf(), extra).expect("spawn watcher");
    // Let the recursive watches register before committing.
    std::thread::sleep(Duration::from_millis(300));

    // Commit on the linked worktree's branch (`side`): writes refs/heads/side and
    // the per-worktree reflog under the common dir, none of it under the wt root.
    common::write(&wt, "feature.txt", "hello\n");
    common::git(&wt, &["add", "feature.txt"]);
    common::git(&wt, &["commit", "-q", "-m", "wt commit"]);

    common::expect_signal(&rx, "a linked-worktree commit");
}

/// A primary checkout keeps all its state under `.git` inside the working tree,
/// so the recursive workdir watch already covers it — no extra roots are needed
/// (the behavior C2c must preserve).
#[test]
fn primary_checkout_needs_no_extra_watch_roots() {
    let repo_dir = common::init_repo();
    let repo = Repo::open(repo_dir.path()).expect("open primary checkout");
    let extra = repo.watch_roots();
    assert!(
        extra.is_empty(),
        "a primary checkout's state lives under the watched workdir: {extra:?}"
    );
}

// --- Coverage per change class (plan 003 §3.1, C1) ---------------------------
//
// Each test prepares its precondition before spawning, spawns, waits for the
// watch to register, drains any setup noise until quiet, performs exactly one
// mutation, and asserts the signal arrives. Timing-generous (5s) but never
// racing: the drain means the awaited signal can only be the mutation's own.

#[test]
fn watcher_signals_on_a_worktree_edit_of_a_tracked_file() {
    let repo = common::init_repo(); // README.md is already tracked.
    let rx = common::spawn_and_drain(repo.path());

    common::write(repo.path(), "README.md", "# test\nedited\n");

    common::expect_signal(&rx, "a worktree edit");
}

#[test]
fn watcher_signals_on_git_add() {
    let repo = common::init_repo();
    // Precondition: an unstaged change already present before the watcher spawns.
    common::write(repo.path(), "README.md", "# test\nunstaged\n");
    let rx = common::spawn_and_drain(repo.path());

    common::git(repo.path(), &["add", "README.md"]);

    common::expect_signal(&rx, "`git add`");
}

#[test]
fn watcher_signals_on_git_commit() {
    let repo = common::init_repo();
    // Precondition: a staged change already present before the watcher spawns.
    common::write(repo.path(), "README.md", "# test\nstaged\n");
    common::git(repo.path(), &["add", "README.md"]);
    let rx = common::spawn_and_drain(repo.path());

    common::git(repo.path(), &["commit", "-q", "-m", "edit readme"]);

    common::expect_signal(&rx, "`git commit`");
}

#[test]
fn watcher_signals_on_a_head_write_from_checkout_b() {
    let repo = common::init_repo();
    let rx = common::spawn_and_drain(repo.path());

    common::git(repo.path(), &["checkout", "-q", "-b", "other"]);

    common::expect_signal(&rx, "`git checkout -b other`");
}

#[test]
fn watcher_signals_on_a_comment_store_write() {
    let repo = common::init_repo();
    let handle = Repo::open(repo.path()).expect("open repo");
    let dir = handle.strix_dir();
    // Precondition: a valid v2 store already on disk before the watcher spawns.
    comments::mutate(&dir, |store| {
        store.branches.insert(
            "main".to_string(),
            Branch {
                active_range: None,
                comments: Vec::new(),
            },
        );
    })
    .expect("seed comment store");

    let rx = common::spawn_and_drain(repo.path());

    // The real atomic path: tmp file + rename over the existing store, exactly
    // as the agent-facing `strix comment` CLI and the TUI's own writes do it.
    comments::mutate(&dir, |store| {
        store.next_id += 1;
    })
    .expect("mutate comment store");

    common::expect_signal(&rx, "a comment-store write");
}
