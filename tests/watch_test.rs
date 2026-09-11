use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use strix::comments::{self, Branch, Comment, Scope, Side, Source};
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

// --- The read side must stay silent (plan 003 §3.2, C2) ----------------------

/// strix reading the repo must not wake strix. On Linux, inotify reports the
/// app's own `.git` reads back to it as `Access(Open)`, so before the kind
/// filter every refresh scheduled the next one and an idle session never
/// settled. This drives strix's whole read set — the calls one `App::reload`
/// makes across Status, History and Review, plus the comment store — and then
/// requires silence.
///
/// One theoretical false red: an inotify queue overflow during the read set is
/// forwarded as `Err` (we may have missed a real change) and would signal. It
/// takes thousands of unread events to provoke; this fixture is a handful of
/// files.
#[test]
fn strixs_own_reads_produce_no_signal() {
    // HEAD is `feature`, diverged from `main`, so `resolve_range("main")` is a
    // real three-dot range with files on both sides.
    let repo = common::init_repo_with_diverged_branches();
    let path = repo.path();
    let base = common::head_oid(path);
    // A modified tracked file, so `status()` has real work and the diff paths
    // are exercised rather than short-circuited on a clean tree.
    common::write(path, "README.md", "# test\nshared\nworking-tree edit\n");
    common::seed_store(
        path,
        "feature",
        Some("main"),
        vec![Comment {
            scope: Scope::WorkTree,
            id: 1,
            source: Source::Agent,
            file: "README.md".to_string(),
            side: Side::New,
            line: 3,
            text: "seeded before the watch".to_string(),
            context: Some("working-tree edit".to_string()),
            orphaned: false,
            created_at: 1_700_000_000,
            base: Some(base),
            stale: false,
        }],
    );

    let rx = common::spawn_and_drain(path);

    let handle = Repo::open(path).expect("open repo");
    let _status = handle.status().expect("status");
    let history = handle.history(500).expect("history");
    let _labels = handle.ref_labels().expect("ref labels");
    let head = history.first().expect("history has a HEAD commit");
    let _files = handle.commit_files(head).expect("commit files");
    let spec = handle.resolve_range("main").expect("resolve range");
    let _range = handle.range_files(&spec).expect("range files");
    let _store = comments::load(&handle.strix_dir()).expect("load comment store");

    match rx.recv_timeout(Duration::from_secs(2)) {
        Err(RecvTimeoutError::Timeout) => {}
        Ok(()) => panic!("strix's own reads produced a refresh signal"),
        Err(RecvTimeoutError::Disconnected) => {
            panic!("watch channel disconnected: the watcher thread died")
        }
    }

    // The silence above only counts if the watcher was still alive through it.
    common::write(
        path,
        "README.md",
        "# test\nshared\nedited after the reads\n",
    );
    common::expect_signal(&rx, "a worktree edit after the read set");
}
