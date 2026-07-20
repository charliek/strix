//! Review-comment rendering + re-anchor wiring in the TUI (plan §3.4, C3).
//!
//! Stores are seeded by writing `comments.json` directly into the repo's
//! `<.git>/strix` dir before opening the review session (the schema the human's
//! TUI and the agent's CLI share). Tests then drive an `App` and assert on
//! `dump_frame` output and the exposed comment state.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{git, init_repo, init_repo_with_diverged_branches, write};
use strix::app::App;
use strix::comments::{Branch, Comment, Scope, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent};
use tempfile::TempDir;

const W: u16 = 100;
const H: u16 = 24;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn dump(app: &App) -> String {
    strix::terminal::dump_frame(app, W, H).unwrap()
}

/// The store dir for a normal (non-worktree) repo checkout.
fn strix_dir(repo: &Path) -> PathBuf {
    repo.join(".git").join("strix")
}

/// A comment builder with sensible defaults; override fields at the call site.
fn comment(id: u64, source: Source, file: &str, side: Side, line: usize, text: &str) -> Comment {
    Comment {
        // A `strix diff` range review; the range value is a C2a placeholder
        // (scope is not asserted here — C3 makes it exact).
        scope: Scope::Range {
            range: String::new(),
        },
        id,
        source,
        file: file.to_string(),
        side,
        line,
        text: text.to_string(),
        context: None,
        orphaned: false,
        created_at: 1_700_000_000,
        base: None,
        stale: false,
    }
}

fn with_context(mut c: Comment, context: &str) -> Comment {
    c.context = Some(context.to_string());
    c
}

fn orphaned(mut c: Comment) -> Comment {
    c.orphaned = true;
    c
}

/// Write a store under `branch` with `range` and `comments`. Pretty JSON with no
/// trailing newline, so a *later* strix write (which appends `\n`) is byte-detectable.
fn seed_store(repo: &Path, branch: &str, range: Option<&str>, comments: Vec<Comment>) {
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
    let json = serde_json::to_string_pretty(&store).unwrap();
    std::fs::write(dir.join("comments.json"), json).unwrap();
}

/// Rewrite the store file directly with an arbitrary comment set (simulates an
/// agent `strix comment` write while a TUI session is open).
fn rewrite_store(repo: &Path, branch: &str, range: Option<&str>, comments: Vec<Comment>) {
    seed_store(repo, branch, range, comments);
}

fn store_bytes(repo: &Path) -> Vec<u8> {
    std::fs::read(strix_dir(repo).join("comments.json")).unwrap()
}

/// Move the review selection until the given file's diff is showing.
fn select_file(app: &mut App, path: &str) {
    let _ = dump(app);
    for _ in 0..20 {
        if app.active_diff_path().as_deref() == Some(path) {
            return;
        }
        app.on_key(key('j'));
    }
    panic!("{path} never became the selected file");
}

/// The 0-based row of the first frame line containing `needle`.
fn row_of(frame: &str, needle: &str) -> usize {
    frame
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("frame missing {needle:?}:\n{frame}"))
}

// --- Anchored rows (unified + SBS) ---

#[test]
fn unified_comment_renders_one_row_below_its_anchor() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![with_context(
            comment(1, Source::Human, "feature.txt", Side::New, 1, "looks good"),
            "feature",
        )],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    select_file(&mut app, "feature.txt");

    let frame = dump(&app);
    // The box: a titled top border, then the word-wrapped body.
    assert!(
        frame.contains("● you — feature.txt R1"),
        "the box title names the author + anchor:\n{frame}"
    );
    assert!(
        frame.contains("looks good"),
        "the box body renders the note text:\n{frame}"
    );
    // The box's title row sits directly below the anchored `+ feature` line, with
    // the body on the next row.
    let content = row_of(&frame, "+ feature");
    let title = row_of(&frame, "● you — feature.txt R1");
    let body = row_of(&frame, "looks good");
    assert_eq!(
        title,
        content + 1,
        "the box opens one row below its anchor:\n{frame}"
    );
    assert_eq!(body, content + 2, "the body follows the title:\n{frame}");
}

#[test]
fn agent_comment_labels_the_source() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![with_context(
            comment(1, Source::Agent, "feature.txt", Side::New, 1, "auto note"),
            "feature",
        )],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    select_file(&mut app, "feature.txt");
    let frame = dump(&app);
    assert!(frame.contains("● agent — feature.txt R1"), "{frame}");
    assert!(frame.contains("auto note"), "{frame}");
}

/// A modified line yields a deletion+addition Pair in side-by-side; comments on
/// the old side must emit before the new side, each ordered by id.
fn modified_line_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    write(p, "file.txt", "line1\nOLD\nline3\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "base"]);
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "file.txt", "line1\nNEW\nline3\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "change"]);
    dir
}

#[test]
fn sbs_pair_emits_old_side_comment_before_new_side() {
    let repo = modified_line_repo();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![
            with_context(
                comment(1, Source::Human, "file.txt", Side::Old, 2, "on old"),
                "OLD",
            ),
            with_context(
                comment(2, Source::Human, "file.txt", Side::New, 2, "on new"),
                "NEW",
            ),
        ],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    // Both anchors resolved (not orphaned).
    assert!(!app.review_comment(1).unwrap().orphaned);
    assert!(!app.review_comment(2).unwrap().orphaned);
    app.on_key(key('d')); // switch to side-by-side
    let frame = dump(&app);
    let old = row_of(&frame, "on old");
    let new = row_of(&frame, "on new");
    assert!(
        old < new,
        "old-side comment box emits before new-side:\n{frame}"
    );
}

// --- Orphan block ---

#[test]
fn orphan_block_renders_at_the_top_of_the_diff() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![
            orphaned(with_context(
                comment(1, Source::Human, "feature.txt", Side::New, 99, "stale note"),
                "gone",
            )),
            with_context(
                comment(2, Source::Human, "feature.txt", Side::New, 1, "live note"),
                "feature",
            ),
        ],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    select_file(&mut app, "feature.txt");
    let frame = dump(&app);
    assert!(
        frame.contains("⚠ you — feature.txt R99") && frame.contains("stale note"),
        "orphan box renders (title + body):\n{frame}"
    );
    let orphan = row_of(&frame, "stale note");
    let live = row_of(&frame, "live note");
    assert!(
        orphan < live,
        "the orphan block sits above the anchored box:\n{frame}"
    );
}

// --- Badge ---

#[test]
fn file_list_shows_a_comment_badge() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![
            with_context(
                comment(1, Source::Human, "feature.txt", Side::New, 1, "a"),
                "feature",
            ),
            orphaned(comment(2, Source::Human, "feature.txt", Side::New, 5, "b")),
        ],
    );
    let app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    let frame = dump(&app);
    assert!(
        frame.contains("● 2"),
        "file-list badge counts all comments:\n{frame}"
    );
}

// --- Footer orphan counter (absent + binary files) ---

#[test]
fn footer_counts_orphans_on_absent_files() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        // A comment on a file that isn't in the range at all → orphaned, and no
        // block can show it, so it belongs in the footer counter.
        vec![orphaned(comment(
            1,
            Source::Human,
            "ghost.txt",
            Side::New,
            3,
            "lost",
        ))],
    );
    let app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    assert_eq!(app.orphan_footer_count(), 1);
    let frame = dump(&app);
    assert!(
        frame.contains("1 orphaned — strix comment list"),
        "footer surfaces the absent-file orphan:\n{frame}"
    );
}

/// A repo whose `feature` branch adds a binary file (NUL bytes) not on the base.
fn binary_file_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "blob.bin", "a\0b\0c\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add binary"]);
    dir
}

#[test]
fn binary_file_orphan_shows_in_its_block_not_the_footer() {
    let repo = binary_file_repo();
    // The file IS in the range, but its diff is binary — no lines to anchor to, so
    // re-anchor orphans the comment. Because the file is *listed*, its orphan is
    // reachable in the file's own top block (finding 2) and is therefore NOT
    // footer-counted (finding 3): the footer only carries orphans on absent files.
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![with_context(
            comment(1, Source::Human, "blob.bin", Side::New, 1, "on a binary"),
            "x",
        )],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    assert!(
        app.review_comment(1).unwrap().orphaned,
        "a binary-file comment orphans"
    );
    assert_eq!(
        app.orphan_footer_count(),
        0,
        "a listed binary file's orphan is not footer-counted"
    );
    select_file(&mut app, "blob.bin");
    let frame = dump(&app);
    assert!(
        frame.contains("on a binary"),
        "the binary file's orphan box renders in its block:\n{frame}"
    );
    assert!(
        !frame.contains("orphaned — strix comment list"),
        "and not in the footer:\n{frame}"
    );
}

/// A repo whose `feature` branch renames a file with no content change, so the
/// file is listed in the range but its text diff is empty (no lines to anchor).
fn pure_rename_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    write(p, "orig.txt", "unchanged\ncontent\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add orig"]);
    git(p, &["checkout", "-qb", "feature"]);
    git(p, &["mv", "orig.txt", "renamed.txt"]);
    git(p, &["commit", "-qm", "pure rename"]);
    dir
}

#[test]
fn empty_diff_orphan_shows_in_its_block_not_the_footer() {
    let repo = pure_rename_repo();
    // renamed.txt is listed (a pure rename) but its text diff is empty, so
    // re-anchor orphans the comment. As a listed file it shows in the file's own
    // orphan block (finding 2) — even though the empty-state hint also renders —
    // and is not footer-counted (finding 3).
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![with_context(
            comment(1, Source::Human, "renamed.txt", Side::New, 1, "on a rename"),
            "unchanged",
        )],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    assert!(
        app.review_comment(1).unwrap().orphaned,
        "an empty-diff file's comment orphans"
    );
    assert_eq!(
        app.orphan_footer_count(),
        0,
        "a listed empty-diff file's orphan is not footer-counted"
    );
    select_file(&mut app, "renamed.txt");
    let frame = dump(&app);
    assert!(
        frame.contains("on a rename"),
        "the empty-diff file's orphan box renders in its block:\n{frame}"
    );
}

// --- Scroll reaches the last (injected) comment row ---

fn tall_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    // big.txt is added on `feature` (not on the base), so the range diff is 40
    // additions and the last line's comment sits below the final rendered row.
    git(p, &["checkout", "-qb", "feature"]);
    let mut content = String::new();
    for i in 1..=40 {
        content.push_str(&format!("row {i}\n"));
    }
    write(p, "big.txt", &content);
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add big"]);
    dir
}

#[test]
fn scroll_to_bottom_reaches_a_comment_below_the_last_line() {
    let repo = tall_repo();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![with_context(
            comment(1, Source::Human, "big.txt", Side::New, 40, "final note"),
            "row 40",
        )],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    assert!(
        !app.review_comment(1).unwrap().orphaned,
        "anchored on the last line"
    );
    let _ = dump(&app);
    app.on_key(key('l')); // focus the diff
    app.on_key(key('G')); // scroll to the bottom
    let frame = dump(&app);
    assert!(
        frame.contains("final note"),
        "the metrics fix makes the injected box reachable:\n{frame}"
    );
}

// --- OID-change reload path ---

/// Commit `contents` for `rel` on the current branch and return nothing.
fn commit_file(dir: &Path, rel: &str, contents: &str, msg: &str) {
    write(dir, rel, contents);
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", msg]);
}

#[test]
fn a_commit_that_moves_the_anchor_line_follows_it() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![with_context(
            comment(1, Source::Human, "feature.txt", Side::New, 1, "feature"),
            "feature",
        )],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    assert_eq!(app.review_comment(1).unwrap().line, 1);

    // Prepend a line: "feature" moves to line 2 but is still present.
    commit_file(repo.path(), "feature.txt", "intro\nfeature\n", "prepend");
    app.reload();

    let c = app.review_comment(1).unwrap();
    assert!(!c.orphaned, "content-match re-anchors within the window");
    assert_eq!(c.line, 2, "the anchor followed the moved line");
}

#[test]
fn a_commit_that_edits_the_anchor_line_orphans_it() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![with_context(
            comment(1, Source::Human, "feature.txt", Side::New, 1, "feature"),
            "feature",
        )],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    select_file(&mut app, "feature.txt");
    assert!(!app.review_comment(1).unwrap().orphaned);

    // Rewrite the anchored line's text: no content match remains → orphan.
    commit_file(repo.path(), "feature.txt", "changed\n", "edit");
    app.reload();

    assert!(
        app.review_comment(1).unwrap().orphaned,
        "an edited line orphans"
    );
    select_file(&mut app, "feature.txt");
    assert!(
        dump(&app).contains("⚠ you — feature.txt"),
        "renders as an orphan box"
    );
}

// --- Agent rm appears on reload with OIDs unchanged ---

#[test]
fn agent_rm_disappears_on_reload_without_relisting() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![with_context(
            comment(1, Source::Human, "feature.txt", Side::New, 1, "remove me"),
            "feature",
        )],
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    select_file(&mut app, "feature.txt");
    assert!(dump(&app).contains("remove me"));
    let relists = app.review_relist_count();

    // The agent removes the comment (store rewritten, no commit → OIDs unchanged).
    rewrite_store(repo.path(), "feature", Some("main"), vec![]);
    app.reload();

    assert!(app.review_comment(1).is_none(), "reload re-reads the store");
    assert_eq!(
        app.review_relist_count(),
        relists,
        "an OID-unchanged reload doesn't relist (churn guard held)"
    );
    select_file(&mut app, "feature.txt");
    assert!(!dump(&app).contains("remove me"), "the comment row is gone");
}

// --- Re-anchor write elision (no reload loop) ---

#[test]
fn a_settled_store_is_not_rewritten_on_open_or_reload() {
    let repo = init_repo_with_diverged_branches();
    // Seed a store that's already settled: range == the review input and the
    // comment is already correctly anchored, so no re-anchor move is needed.
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![with_context(
            comment(1, Source::Human, "feature.txt", Side::New, 1, "stable"),
            "feature",
        )],
    );
    let before = store_bytes(repo.path());
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    assert_eq!(
        store_bytes(repo.path()),
        before,
        "session open elides the write when nothing changed"
    );
    app.reload();
    app.reload();
    assert_eq!(
        store_bytes(repo.path()),
        before,
        "reloads never rewrite a settled store (no re-anchor loop)"
    );
}

// --- Corrupt store is recoverable ---

#[test]
fn corrupt_store_opens_with_a_flash_and_no_comments() {
    let repo = init_repo_with_diverged_branches();
    let dir = strix_dir(repo.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("comments.json"), b"{ not json").unwrap();

    // Construction must not fail over a corrupt store.
    let app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    assert!(
        app.flash.is_some(),
        "a corrupt store flashes an error on open"
    );
    assert_eq!(
        app.review_comment_count("feature.txt"),
        0,
        "no comments loaded"
    );
    // Rendering the frame must not panic.
    let _ = dump(&app);
}

// --- Non-HEAD-head review is comment-free ---

#[test]
fn non_head_review_renders_comment_free() {
    // HEAD is `feature`; review a range whose head is `main` (≠ HEAD). Even though
    // the `feature` inbox has comments, the session must render none and not load
    // them (plan invariant §3.1.1).
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("feature..main"),
        vec![with_context(
            comment(
                1,
                Source::Human,
                "main-only.txt",
                Side::New,
                1,
                "should hide",
            ),
            "main",
        )],
    );
    let app = App::for_review(
        repo.path().to_path_buf(),
        &Config::default(),
        "feature..main",
    )
    .unwrap();
    assert_eq!(
        app.review_comment_count("main-only.txt"),
        0,
        "a non-HEAD-head review loads no comments"
    );
    let frame = dump(&app);
    assert!(
        !frame.contains("should hide"),
        "no comment rows render:\n{frame}"
    );
}

// --- External checkout re-computes the inbox identity (finding 1) ---

/// Write a store carrying two branch inboxes (both with `range: "main"`), so a
/// checkout between them can be observed switching the live inbox.
fn seed_two_branches(repo: &Path, a: (&str, Vec<Comment>), b: (&str, Vec<Comment>)) {
    let mut branches = BTreeMap::new();
    branches.insert(
        a.0.to_string(),
        Branch {
            active_range: Some("main".to_string()),
            comments: a.1,
        },
    );
    branches.insert(
        b.0.to_string(),
        Branch {
            active_range: Some("main".to_string()),
            comments: b.1,
        },
    );
    let store = Store {
        version: 2,
        next_id: 1000,
        branches,
    };
    let dir = strix_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    let json = serde_json::to_string_pretty(&store).unwrap();
    std::fs::write(dir.join("comments.json"), json).unwrap();
}

/// `main` (README only, never advanced) with two feature branches off it, each
/// adding its own file. `alpha` is left checked out.
fn two_feature_branches_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["checkout", "-qb", "alpha"]);
    write(p, "alpha.txt", "alpha\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "alpha work"]);
    git(p, &["checkout", "-q", "main"]);
    git(p, &["checkout", "-qb", "beta"]);
    write(p, "beta.txt", "beta\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "beta work"]);
    git(p, &["checkout", "-q", "alpha"]);
    dir
}

#[test]
fn checkout_switches_the_inbox_to_the_new_branch() {
    // `strix diff main` follows HEAD, so an external `git checkout` from alpha to
    // beta must swing the live inbox to beta's key — reading the new branch's
    // comments and dropping alpha's (finding 1).
    let repo = two_feature_branches_repo();
    seed_two_branches(
        repo.path(),
        (
            "alpha",
            vec![with_context(
                comment(1, Source::Human, "alpha.txt", Side::New, 1, "alpha note"),
                "alpha",
            )],
        ),
        (
            "beta",
            vec![with_context(
                comment(2, Source::Human, "beta.txt", Side::New, 1, "beta note"),
                "beta",
            )],
        ),
    );
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    assert!(app.review_comment(1).is_some(), "alpha's inbox loads");
    assert!(
        app.review_comment(2).is_none(),
        "beta's inbox is not read while on alpha"
    );
    select_file(&mut app, "alpha.txt");
    assert!(dump(&app).contains("alpha note"));

    // Externally check out beta while the TUI is open, then refresh.
    git(repo.path(), &["checkout", "-q", "beta"]);
    app.reload();

    assert!(
        app.review_comment(2).is_some(),
        "the reload switched to beta's inbox"
    );
    assert!(
        app.review_comment(1).is_none(),
        "alpha's comment is no longer in the live set"
    );
    select_file(&mut app, "beta.txt");
    let frame = dump(&app);
    assert!(
        frame.contains("beta note"),
        "beta's comment renders:\n{frame}"
    );
    assert!(
        !frame.contains("alpha note"),
        "alpha's comment is gone:\n{frame}"
    );
}

#[test]
fn checkout_off_the_reviewed_head_disables_authoring() {
    // A *fixed* range `main..feature` pins the reviewed head to `feature`. With
    // `feature` checked out, authoring is on and its comments render; checking out
    // `main` moves HEAD off the reviewed head, so authoring turns off and the
    // comments stop rendering (finding 1).
    let repo = init_repo_with_diverged_branches(); // HEAD = feature
    seed_store(
        repo.path(),
        "feature",
        Some("main..feature"),
        vec![with_context(
            comment(1, Source::Human, "feature.txt", Side::New, 1, "feature"),
            "feature",
        )],
    );
    let mut app = App::for_review(
        repo.path().to_path_buf(),
        &Config::default(),
        "main..feature",
    )
    .unwrap();
    assert!(
        app.review_comment(1).is_some(),
        "authoring on: comment loads"
    );
    select_file(&mut app, "feature.txt");
    assert!(dump(&app).contains("● you — feature.txt"));

    // Move HEAD off the reviewed head (`feature`).
    git(repo.path(), &["checkout", "-q", "main"]);
    app.reload();

    assert!(
        app.review_comment(1).is_none(),
        "authoring off: the live comment set is cleared"
    );
    let frame = dump(&app);
    assert!(
        !frame.contains("● you — feature.txt"),
        "no comment boxes render once authoring is off:\n{frame}"
    );
}
