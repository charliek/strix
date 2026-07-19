//! Worktree comments in the status view (plan §3.1/§3.2, C3): authoring on the
//! net HEAD→worktree diff in bare `strix`, navigation, the re-anchor + sweep
//! lifecycle wired into refresh, and scope isolation from range comments.
//!
//! Stores are seeded by writing `comments.json` directly (the schema the human's
//! TUI and the agent's CLI share); tests then drive an `App` and assert on
//! `dump_frame` output and the exposed comment state. Nothing touches the real
//! store — every repo is a `tempfile::tempdir`.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{git, init_empty_repo, init_repo, init_repo_with_diverged_branches, write};
use strix::app::App;
use strix::comments::{self, Branch, Comment, Scope, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent};

const W: u16 = 110;
const H: u16 = 30;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn enter() -> KeyEvent {
    KeyEvent::from(KeyCode::Enter)
}

fn dump(app: &App) -> String {
    strix::terminal::dump_frame(app, W, H).unwrap()
}

fn strix_dir(repo: &Path) -> PathBuf {
    repo.join(".git").join("strix")
}

fn load_store(repo: &Path) -> Store {
    comments::load(&strix_dir(repo)).unwrap()
}

/// The current HEAD commit oid (the baseline a worktree comment stamps).
fn head_oid(repo: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A worktree comment with a baseline HEAD (as C3 records it), anchored on the
/// New side by default with a captured context line.
fn wt(id: u64, file: &str, line: usize, text: &str, context: &str, base: &str) -> Comment {
    Comment {
        scope: Scope::WorkTree,
        id,
        source: Source::Human,
        file: file.to_string(),
        side: Side::New,
        line,
        text: text.to_string(),
        context: Some(context.to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
        base: Some(base.to_string()),
        stale: false,
    }
}

/// A committed-range comment (the `strix diff` surface), for scope-isolation tests.
fn range_comment(
    id: u64,
    file: &str,
    line: usize,
    text: &str,
    context: &str,
    range: &str,
) -> Comment {
    Comment {
        scope: Scope::Range {
            range: range.to_string(),
        },
        id,
        source: Source::Human,
        file: file.to_string(),
        side: Side::New,
        line,
        text: text.to_string(),
        context: Some(context.to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
        base: None,
        stale: false,
    }
}

/// Write a store for `branch` with the given comments (and an optional active
/// range). Pretty JSON with no trailing newline, so a later strix write is
/// byte-detectable.
fn seed(repo: &Path, branch: &str, range: Option<&str>, comments: Vec<Comment>) {
    let mut branches = BTreeMap::new();
    branches.insert(
        branch.to_string(),
        Branch {
            active_range: range.map(str::to_string),
            comments,
        },
    );
    let store = Store {
        version: 2,
        next_id: 1000,
        branches,
    };
    let dir = strix_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("comments.json"),
        serde_json::to_string_pretty(&store).unwrap(),
    )
    .unwrap();
}

/// The 0-based frame row of the first line containing `needle`.
fn row_of(frame: &str, needle: &str) -> usize {
    frame
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("frame missing {needle:?}:\n{frame}"))
}

/// Move the status file selection until `path` is the selected file.
fn select_status_file(app: &mut App, path: &str) {
    let _ = dump(app);
    for _ in 0..40 {
        if app.active_diff_path().as_deref() == Some(path) {
            return;
        }
        app.on_key(key('j'));
    }
    panic!("{path} never became the selected status file");
}

/// The worktree comments the status view currently holds for `branch`.
fn worktree_comments(repo: &Path, branch: &str) -> Vec<Comment> {
    load_store(repo)
        .branches
        .get(branch)
        .map(|b| {
            b.comments
                .iter()
                .filter(|c| matches!(c.scope, Scope::WorkTree))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

// --- Authoring on the net worktree diff ---

#[test]
fn c_in_bare_strix_leaves_a_worktree_comment() {
    let repo = init_repo();
    write(repo.path(), "new.txt", "target line\n");
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    select_status_file(&mut app, "new.txt");

    app.on_key(key('l')); // focus the diff
    app.on_key(key('j')); // cursor: hunk header -> the first addition
    app.on_key(key('c')); // open the comment editor on "target line"
    for ch in "looks off".chars() {
        app.on_key(key(ch));
    }
    app.on_key(enter());

    let frame = dump(&app);
    assert!(
        frame.contains("● you looks off"),
        "the worktree comment renders:\n{frame}"
    );
    let content = row_of(&frame, "target line");
    let note = row_of(&frame, "● you looks off");
    assert_eq!(note, content + 1, "one row below its anchor:\n{frame}");

    // Stored as a `WorkTree` comment stamped with the current HEAD baseline.
    let stored = worktree_comments(repo.path(), "main");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].text, "looks off");
    assert_eq!(
        stored[0].base.as_deref(),
        Some(head_oid(repo.path()).as_str())
    );
}

#[test]
fn comment_badge_counts_worktree_comments_per_file() {
    let repo = init_repo();
    write(repo.path(), "new.txt", "target\n");
    seed(
        repo.path(),
        "main",
        None,
        vec![wt(
            1,
            "new.txt",
            1,
            "note",
            "target",
            &head_oid(repo.path()),
        )],
    );
    let app = App::new(repo.path().to_path_buf()).unwrap();
    assert_eq!(app.status_comment_count("new.txt"), 1);
    assert!(
        dump(&app).contains("● 1"),
        "the Changes list shows a per-file badge"
    );
}

// --- Navigation ---

#[test]
fn bracket_keys_navigate_worktree_comments() {
    let repo = init_repo();
    write(repo.path(), "a.txt", "aaa\n");
    write(repo.path(), "b.txt", "bbb\n");
    let base = head_oid(repo.path());
    seed(
        repo.path(),
        "main",
        None,
        vec![
            wt(1, "a.txt", 1, "on a", "aaa", &base),
            wt(2, "b.txt", 1, "on b", "bbb", &base),
        ],
    );
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let _ = dump(&app);

    app.on_key(key(']')); // jump to the first comment
    let first = app.active_diff_path().clone();
    assert!(dump(&app).contains("on a") || dump(&app).contains("on b"));

    app.on_key(key(']')); // to the next comment (other file)
    let second = app.active_diff_path().clone();
    assert_ne!(first, second, "]/[ cross files to the next comment");

    app.on_key(key('[')); // back to the first
    assert_eq!(
        app.active_diff_path(),
        first,
        "[ returns to the prior comment"
    );
}

// --- Agent add / rm reflects on reload ---

#[test]
fn agent_add_and_rm_reflect_on_reload() {
    let repo = init_repo();
    write(repo.path(), "new.txt", "target\n");
    let base = head_oid(repo.path());
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    select_status_file(&mut app, "new.txt");
    assert_eq!(
        app.status_comment_count("new.txt"),
        0,
        "starts comment-free"
    );

    // An agent adds a worktree comment out-of-band, then the watcher reloads.
    seed(
        repo.path(),
        "main",
        None,
        vec![wt(1, "new.txt", 1, "agent flagged this", "target", &base)],
    );
    app.reload();
    select_status_file(&mut app, "new.txt");
    assert_eq!(app.status_comment_count("new.txt"), 1);
    assert!(
        dump(&app).contains("agent flagged this"),
        "the agent's note appears on reload"
    );

    // The agent removes it; the reload drops the row.
    seed(repo.path(), "main", None, vec![]);
    app.reload();
    assert_eq!(app.status_comment_count("new.txt"), 0);
    assert!(
        !dump(&app).contains("agent flagged this"),
        "the removed note is gone after reload"
    );
}

// --- Lifecycle: commit sweeps, in-place edit staleifies ---

#[test]
fn a_commit_that_lands_the_target_sweeps_the_comment() {
    let repo = init_repo();
    write(repo.path(), "feature.rs", "target line\n");
    let base = head_oid(repo.path());
    seed(
        repo.path(),
        "main",
        None,
        vec![wt(1, "feature.rs", 1, "review me", "target line", &base)],
    );
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    assert_eq!(app.status_comment_count("feature.rs"), 1, "loaded on open");

    // Commit exactly the commented addition: HEAD advances past the baseline and
    // the change resolves into it, so the note is swept.
    git(repo.path(), &["add", "feature.rs"]);
    git(repo.path(), &["commit", "-qm", "land feature"]);
    app.reload();

    assert_eq!(
        app.status_comment_count("feature.rs"),
        0,
        "the committed target is swept"
    );
    assert!(
        worktree_comments(repo.path(), "main").is_empty(),
        "and removed from the store"
    );
}

#[test]
fn an_in_place_edit_marks_the_comment_stale_not_deleted() {
    let repo = init_repo();
    write(repo.path(), "new.txt", "target\n");
    let base = head_oid(repo.path());
    seed(
        repo.path(),
        "main",
        None,
        vec![wt(1, "new.txt", 1, "look here", "target", &base)],
    );
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    assert_eq!(app.status_comment_count("new.txt"), 1);

    // Rewrite the anchored line in place (no commit → HEAD unchanged): the note
    // drifts but must be surfaced (stale), never deleted.
    write(repo.path(), "new.txt", "changed\n");
    app.reload();

    let stored = worktree_comments(repo.path(), "main");
    assert_eq!(stored.len(), 1, "an in-place edit is staled, never swept");
    assert!(stored[0].stale, "drift under an unchanged HEAD marks stale");
    select_status_file(&mut app, "new.txt");
    assert!(
        dump(&app).contains("look here"),
        "the stale note is still surfaced"
    );
}

// --- Scope isolation ---

#[test]
fn a_range_comment_never_shows_in_status() {
    let repo = init_repo();
    write(repo.path(), "README.md", "# test\nedited\n"); // an uncommitted change
    let base = head_oid(repo.path());
    // The same branch entry carries both a range comment and a worktree comment.
    seed(
        repo.path(),
        "main",
        Some("main"),
        vec![
            range_comment(1, "README.md", 2, "range note", "edited", "main"),
            wt(2, "README.md", 2, "worktree note", "edited", &base),
        ],
    );
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    select_status_file(&mut app, "README.md");
    let frame = dump(&app);

    assert!(
        frame.contains("worktree note"),
        "the worktree comment shows in status:\n{frame}"
    );
    assert!(
        !frame.contains("range note"),
        "the range comment is hidden in status:\n{frame}"
    );
    assert_eq!(
        app.status_comment_count("README.md"),
        1,
        "the badge counts only worktree comments"
    );

    // Status must not have re-anchored the range comment (a different scope's diff).
    let range = load_store(repo.path()).branches["main"].comments[0].clone();
    assert!(
        matches!(range.scope, Scope::Range { .. }) && !range.orphaned && range.line == 2,
        "the range comment is untouched by the worktree sweep"
    );
}

#[test]
fn a_worktree_comment_never_shows_in_a_review() {
    // HEAD is `feature`; review `main` (feature is checked out, so authoring is on).
    let repo = init_repo_with_diverged_branches();
    let base = head_oid(repo.path());
    seed(
        repo.path(),
        "feature",
        Some("main"),
        vec![
            range_comment(1, "feature.txt", 1, "range note", "feature", "main"),
            wt(2, "feature.txt", 1, "worktree note", "feature", &base),
        ],
    );
    let app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();

    assert_eq!(
        app.review_comment_count("feature.txt"),
        1,
        "the review sees only its range comment"
    );
    assert!(app.review_comment(1).is_some(), "the range comment loads");
    assert!(
        app.review_comment(2).is_none(),
        "the worktree comment is hidden in the review"
    );

    // The worktree comment must not have been re-anchored against the range diff.
    let stored = worktree_comments(repo.path(), "feature");
    assert_eq!(stored.len(), 1);
    assert!(
        !stored[0].orphaned && stored[0].line == 1,
        "the review left the worktree comment untouched"
    );
}

// --- Un-commentable files flash ---

#[test]
fn commenting_on_a_binary_file_flashes() {
    let repo = init_repo();
    std::fs::write(repo.path().join("blob.dat"), [0u8, 1, 2, 0, 255, 0]).unwrap();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    select_status_file(&mut app, "blob.dat");
    app.on_key(key('l')); // focus the diff
    app.on_key(key('c'));
    assert!(
        app.flash.is_some(),
        "commenting on a binary file flashes instead of anchoring"
    );
    assert_eq!(app.status_comment_count("blob.dat"), 0, "nothing anchored");
}

#[test]
fn commenting_on_a_conflicted_file_flashes() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "conflict.txt", "base\n");
    git(path, &["add", "."]);
    git(path, &["commit", "-qm", "base"]);
    git(path, &["checkout", "-qb", "other"]);
    write(path, "conflict.txt", "other side\n");
    git(path, &["commit", "-aqm", "other"]);
    git(path, &["checkout", "-q", "main"]);
    write(path, "conflict.txt", "main side\n");
    git(path, &["commit", "-aqm", "main"]);
    // The merge conflicts (non-zero exit) — run it directly so the test helper's
    // success assertion doesn't fire.
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["merge", "other"])
        .output()
        .expect("git merge");

    let mut app = App::new(path.to_path_buf()).unwrap();
    select_status_file(&mut app, "conflict.txt");
    app.on_key(key('l'));
    app.on_key(key('c'));
    assert!(
        app.flash.is_some(),
        "commenting on a conflicted file flashes instead of anchoring"
    );
    assert_eq!(app.status_comment_count("conflict.txt"), 0);
}

// --- No re-anchor / watcher loop on a settled worktree store ---

#[test]
fn a_settled_worktree_store_is_not_rewritten_on_reload() {
    let repo = init_repo();
    write(repo.path(), "new.txt", "target\n");
    seed(
        repo.path(),
        "main",
        None,
        vec![wt(
            1,
            "new.txt",
            1,
            "note",
            "target",
            &head_oid(repo.path()),
        )],
    );
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let settled = std::fs::read(strix_dir(repo.path()).join("comments.json")).unwrap();
    app.reload();
    app.reload();
    assert_eq!(
        std::fs::read(strix_dir(repo.path()).join("comments.json")).unwrap(),
        settled,
        "a settled worktree store is never rewritten (no re-anchor loop)"
    );
}

// --- FIX A: `worktree_facts` never blind-sweeps a file that left the status list ---

#[test]
fn a_committed_rename_goes_stale_not_swept() {
    // A tracked file with a worktree comment is renamed-and-committed away. In the
    // new HEAD there is no `orig_path` in status and the old path is gone from disk;
    // the pre-fix code classified that as `Gone` and swept the note (comment loss).
    // The note must be RETAINED (stale) — the commit-sweep gate fires only when the
    // anchored content actually resolves in HEAD.
    let repo = init_repo();
    let p = repo.path();
    write(p, "old.rs", "target line\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add old"]);
    let base = head_oid(p);
    seed(
        p,
        "main",
        None,
        vec![wt(1, "old.rs", 1, "review me", "target line", &base)],
    );

    git(p, &["mv", "old.rs", "new.rs"]);
    git(p, &["commit", "-qm", "rename"]);
    let _app = App::new(p.to_path_buf()).unwrap(); // the sweep runs on open

    let stored = worktree_comments(p, "main");
    assert_eq!(stored.len(), 1, "a committed rename never sweeps the note");
    assert!(stored[0].stale, "the surviving note is surfaced as stale");
}

#[cfg(unix)]
#[test]
fn a_broken_symlink_path_is_not_read_as_a_deletion() {
    use std::os::unix::fs::symlink;
    // A committed symlink whose target doesn't exist: the tree is clean (git sees
    // no change) but the path resolves to nothing. `Path::exists()` follows the
    // link and reports absent → the pre-fix code would sweep; `symlink_metadata`
    // sees the link entry and keeps the note. HEAD is unchanged, so the only reason
    // to sweep would be a false "file absent".
    let repo = init_repo();
    let p = repo.path();
    symlink("does-not-exist", p.join("link.txt")).unwrap();
    git(p, &["add", "link.txt"]);
    git(p, &["commit", "-qm", "add broken symlink"]);
    let base = head_oid(p);
    seed(
        p,
        "main",
        None,
        vec![wt(1, "link.txt", 1, "note", "whatever", &base)],
    );

    let _app = App::new(p.to_path_buf()).unwrap();
    assert_eq!(
        worktree_comments(p, "main").len(),
        1,
        "a broken-symlink path must not read as a worktree deletion"
    );
}

// --- FIX B: inbox key + baseline from one atomic Status snapshot ---

#[test]
fn unborn_branch_author_stamps_no_baseline_and_keys_by_branch_name() {
    // On an unborn HEAD, porcelain reports `# branch.head main` + `# branch.oid
    // (initial)`. The inbox keys by the branch name (`main`), and authoring stamps
    // no baseline OID (not the literal "(initial)").
    let repo = init_empty_repo();
    write(repo.path(), "first.txt", "hello\n");
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    select_status_file(&mut app, "first.txt");
    app.on_key(key('l'));
    app.on_key(key('j'));
    app.on_key(key('c'));
    for ch in "unborn note".chars() {
        app.on_key(key(ch));
    }
    app.on_key(enter());

    let store = load_store(repo.path());
    let branch = store
        .branches
        .get("main")
        .expect("keyed by the unborn branch name");
    assert_eq!(branch.comments.len(), 1);
    assert!(
        branch.comments[0].base.is_none(),
        "an unborn HEAD stamps no baseline"
    );
}

// --- FIX C: submit persists to the branch the editor opened on ---

#[test]
fn modal_submit_persists_to_the_branch_it_opened_on() {
    // Open the editor on `main`, then an external checkout to `other` + a watcher
    // reload swings the current inbox — the submit must still land under `main`,
    // where the note was authored, not the branch checked out mid-edit.
    let repo = init_repo();
    let p = repo.path();
    git(p, &["branch", "other"]); // a second branch at main
    write(p, "a.txt", "target\n");
    let mut app = App::new(p.to_path_buf()).unwrap();
    select_status_file(&mut app, "a.txt");
    app.on_key(key('l'));
    app.on_key(key('j'));
    app.on_key(key('c')); // open the editor while on `main`
    for ch in "authored on main".chars() {
        app.on_key(key(ch));
    }

    git(p, &["checkout", "-q", "other"]);
    app.reload(); // the watcher path: modals gate input, not reloads
    assert_eq!(
        app.status.branch.as_deref(),
        Some("other"),
        "the reload swung the current branch to `other`"
    );

    app.on_key(enter()); // submit

    let store = load_store(p);
    let main = store
        .branches
        .get("main")
        .expect("the note landed under the authoring branch");
    assert_eq!(main.comments.len(), 1);
    assert_eq!(main.comments[0].text, "authored on main");
    assert!(
        store
            .branches
            .get("other")
            .is_none_or(|b| b.comments.is_empty()),
        "nothing leaked into the branch checked out mid-edit"
    );
}
