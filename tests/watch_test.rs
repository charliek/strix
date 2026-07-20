use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

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

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => {}
        Err(RecvTimeoutError::Timeout) => panic!("watcher sent no signal within 5s"),
        Err(err) => panic!("watch channel error: {err}"),
    }
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

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => {}
        Err(RecvTimeoutError::Timeout) => {
            panic!("no signal for a linked-worktree commit within 5s")
        }
        Err(err) => panic!("watch channel error: {err}"),
    }
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
