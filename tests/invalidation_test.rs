//! Invalidation granularity (plan 007 §3.1): every top-level mutation bumps
//! `stream_generation` exactly once, and the in-place editor bumps it not at all.
//!
//! These are *exact-delta* tests. A bump retires every cached section, so an
//! over-eager one is invisible in the rendered frame but recomputes the whole
//! prepared window — which is why the assertions are equalities, not `>`.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use common::{git, init_repo, init_repo_with_diverged_branches, press, write};
use strix::app::App;
use strix::comments::{Branch, Comment, Scope, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent};
use strix::terminal::dump_frame;
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 30;

/// Cross-file scrolling on: the stream (and therefore section invalidation) only
/// exists with strips enabled.
fn config() -> Config {
    Config {
        cross_file_scroll: Some(true),
        line_numbers: Some(true),
        ..Config::default()
    }
}

/// A status app on `repo` with one frame rendered, so the pane geometry the
/// layouts are keyed by is known.
fn app_for(repo: &TempDir) -> App {
    let app = App::with_config(repo.path().to_path_buf(), &config()).unwrap();
    dump_frame(&app, W, H).unwrap();
    app
}

fn review_app(repo: &TempDir, range: &str) -> App {
    let app = App::for_review(repo.path().to_path_buf(), &config(), range).unwrap();
    dump_frame(&app, W, H).unwrap();
    app
}

/// Fill and assemble the window — the event-path/render-path pair, so every
/// visible neighbour has a prepared section before the measurement starts.
fn window(app: &mut App) -> usize {
    let area = app.diff_area();
    app.ensure_diff_window(area.width, area.height);
    app.diff_window(area.width, area.height).segments.len()
}

/// Three committed files, each modified in the working tree.
fn three_modified_files() -> TempDir {
    let repo = init_repo();
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(repo.path(), name, "one\ntwo\nthree\n");
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "files"]);
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(repo.path(), name, "one\nTWO\nthree\n");
    }
    repo
}

fn head_oid(repo: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Seed `comments.json` directly — the schema the TUI and the `strix comment`
/// CLI share.
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
    let dir = repo.join(".git").join("strix");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("comments.json"),
        serde_json::to_string_pretty(&store).unwrap(),
    )
    .unwrap();
}

/// A range comment on the reviewed branch's inbox, anchored by context so the
/// re-anchor pass can follow it when the line moves.
fn range_comment(line: usize, text: &str, context: &str) -> Comment {
    Comment {
        scope: Scope::Range {
            range: "main".to_string(),
        },
        id: 1,
        source: Source::Human,
        file: "renamed.txt".to_string(),
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

/// The reviewed branch's stored comments, read back off disk.
fn stored_comments(repo: &Path) -> Vec<Comment> {
    strix::comments::load(&repo.join(".git").join("strix"))
        .unwrap()
        .branches
        .get("feature")
        .map(|b| b.comments.clone())
        .unwrap_or_default()
}

/// Add a line above `renamed.txt`'s "delta" and commit it on `feature`, so the
/// review range moves *and* every anchor below the insertion shifts by one.
fn commit_a_line_above_delta(repo: &Path) {
    write(repo, "renamed.txt", "zero\nalpha\nbeta\ngamma\ndelta\n");
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "feat three"]);
}

/// A worktree comment anchored on `a.txt`'s modified line, with the baseline
/// HEAD and the context the re-anchor pass matches on (so the sweep keeps it).
fn worktree_comment(base: &str) -> Comment {
    Comment {
        scope: Scope::WorkTree,
        id: 1,
        source: Source::Human,
        file: "a.txt".to_string(),
        side: Side::New,
        line: 2,
        text: "seeded".to_string(),
        context: Some("TWO".to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
        base: Some(base.to_string()),
        stale: false,
    }
}

/// Focus the diff and put the cursor on the first code line (header, hunk
/// header, then the line itself).
fn cursor_to_first_code_line(app: &mut App) {
    press(app, 'l');
    press(app, 'j');
    press(app, 'j');
}

// --- Status refresh / reload ------------------------------------------------

#[test]
fn a_status_reload_bumps_the_stream_exactly_once() {
    let repo = three_modified_files();
    let mut app = app_for(&repo);
    assert!(window(&mut app) > 1, "the viewport shows strips");
    let generation = app.stream_generation();

    app.reload();

    assert_eq!(
        app.stream_generation() - generation,
        1,
        "reload delegates to refresh, which owns the cycle's single bump"
    );
}

#[test]
fn a_status_reload_with_changed_comments_bumps_exactly_once() {
    let repo = three_modified_files();
    let mut app = app_for(&repo);
    window(&mut app);
    assert_eq!(app.status_comment_count("a.txt"), 0);
    // An agent writes a note through the CLI between ticks: the reload's comment
    // sync installs a genuinely different set.
    seed(
        repo.path(),
        "main",
        None,
        vec![worktree_comment(&head_oid(repo.path()))],
    );
    let generation = app.stream_generation();

    app.reload();

    assert_eq!(
        app.status_comment_count("a.txt"),
        1,
        "the reload picked the new note up"
    );
    assert_eq!(
        app.stream_generation() - generation,
        1,
        "the comment sync rides the refresh's bump instead of adding its own"
    );
}

#[test]
fn a_failed_status_refresh_still_retires_the_stream() {
    let repo = three_modified_files();
    let mut app = app_for(&repo);
    window(&mut app);
    let generation = app.stream_generation();

    // Pull the working tree out from under `git status`, so the snapshot read
    // fails and the previous one is kept.
    std::fs::remove_dir_all(repo.path()).unwrap();
    app.refresh();

    assert_eq!(
        app.stream_generation() - generation,
        1,
        "a failed snapshot must not leave sections built from the old one alive"
    );
}

// --- Review refresh ---------------------------------------------------------

#[test]
fn a_review_reload_with_an_unchanged_range_does_not_bump() {
    let repo = init_repo_with_diverged_branches();
    let mut app = review_app(&repo, "main");
    window(&mut app);
    let generation = app.stream_generation();

    app.reload();

    // Zero, not one: a review's sections are computed from committed tips, so an
    // unchanged range with an unchanged inbox has nothing to invalidate. This is
    // exactly what the churn guard exists for — the common watcher event during
    // an agent run is a worktree save, which can't move a committed range.
    assert_eq!(
        app.stream_generation(),
        generation,
        "the churn guard kept every section"
    );
}

#[test]
fn a_review_reload_after_the_range_moves_bumps_exactly_once() {
    let repo = init_repo_with_diverged_branches();
    let mut app = review_app(&repo, "main");
    window(&mut app);
    let files = app.stream_file_count();
    let generation = app.stream_generation();

    write(repo.path(), "later.txt", "later\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "feat three"]);
    app.reload();

    assert_eq!(
        app.stream_file_count(),
        files + 1,
        "the range moved and the list was rebuilt"
    );
    assert_eq!(
        app.stream_generation() - generation,
        1,
        "the relist owns the bump; the store re-read and re-anchor elide theirs"
    );
}

#[test]
fn a_range_move_that_also_re_anchors_comments_bumps_exactly_once() {
    let repo = init_repo_with_diverged_branches();
    seed(
        repo.path(),
        "feature",
        Some("main"),
        vec![range_comment(4, "on delta", "delta")],
    );
    let mut app = review_app(&repo, "main");
    window(&mut app);
    let generation = app.stream_generation();

    commit_a_line_above_delta(repo.path());
    app.reload();

    // The re-anchor pass ran and *moved* the anchor — the case that used to add a
    // second bump on top of the relist's.
    assert_eq!(
        stored_comments(repo.path())
            .first()
            .expect("the note survived")
            .line,
        5,
        "the inserted line pushed the anchor down"
    );
    assert_eq!(
        app.stream_generation() - generation,
        1,
        "one relist, one bump — the re-anchor's invalidation folds into it"
    );
}

#[test]
fn a_range_move_racing_an_external_comment_edit_bumps_exactly_once() {
    let repo = init_repo_with_diverged_branches();
    seed(
        repo.path(),
        "feature",
        Some("main"),
        vec![range_comment(4, "on delta", "delta")],
    );
    let mut app = review_app(&repo, "main");
    window(&mut app);
    let generation = app.stream_generation();

    // The worst case: an agent edits the note through the CLI *and* commits, so
    // one tick carries a store change, a relist, and a re-anchor — three mutations
    // that used to bump three times.
    seed(
        repo.path(),
        "feature",
        Some("main"),
        vec![range_comment(4, "edited externally", "delta")],
    );
    commit_a_line_above_delta(repo.path());
    app.reload();

    let stored = stored_comments(repo.path());
    let note = stored.first().expect("the note survived");
    assert_eq!(note.text, "edited externally", "the external edit was read");
    assert_eq!(note.line, 5, "and the anchor still followed the insertion");
    assert_eq!(
        app.stream_generation() - generation,
        1,
        "the relist still owns the cycle's single bump"
    );
}

// --- Comment mutations ------------------------------------------------------

#[test]
fn saving_a_comment_bumps_the_stream_exactly_once() {
    let repo = three_modified_files();
    let mut app = app_for(&repo);
    window(&mut app);
    cursor_to_first_code_line(&mut app);
    let generation = app.stream_generation();

    press(&mut app, 'c');
    assert!(app.editor_open(), "the editor opened on a code row");
    for ch in "note".chars() {
        press(&mut app, ch);
    }
    app.on_key(KeyEvent::from(KeyCode::Enter));

    assert_eq!(app.status_comment_count("a.txt"), 1, "the note was stored");
    assert_eq!(
        app.stream_generation() - generation,
        1,
        "open + four keystrokes + save is one mutation, one bump"
    );
}

#[test]
fn deleting_a_comment_bumps_the_stream_exactly_once() {
    let repo = three_modified_files();
    let mut app = app_for(&repo);
    window(&mut app);
    cursor_to_first_code_line(&mut app);
    press(&mut app, 'c');
    for ch in "note".chars() {
        press(&mut app, ch);
    }
    // The save lands the cursor on the new note's box, so `X` deletes it.
    app.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.status_comment_count("a.txt"), 1);
    let generation = app.stream_generation();

    press(&mut app, 'X');

    assert_eq!(app.status_comment_count("a.txt"), 0, "the note was deleted");
    assert_eq!(app.stream_generation() - generation, 1);
}

// --- The editor -------------------------------------------------------------

#[test]
fn opening_typing_and_discarding_the_editor_never_bumps_the_stream() {
    let repo = three_modified_files();
    let mut app = app_for(&repo);
    window(&mut app);
    cursor_to_first_code_line(&mut app);
    let generation = app.stream_generation();

    press(&mut app, 'c');
    assert!(app.editor_open());
    for ch in "hello".chars() {
        press(&mut app, ch);
    }
    app.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(!app.editor_open());

    assert_eq!(
        app.stream_generation(),
        generation,
        "the editor renders in the anchor's rows only — no section is stale"
    );
}

#[test]
fn typing_in_the_editor_does_not_recompute_neighbour_sections() {
    let repo = three_modified_files();
    let mut app = app_for(&repo);
    assert!(window(&mut app) > 1, "the viewport shows a strip");
    cursor_to_first_code_line(&mut app);
    let computed = app.diff_compute_count();

    press(&mut app, 'c');
    for ch in "hello".chars() {
        press(&mut app, ch);
    }
    app.on_key(KeyEvent::from(KeyCode::Esc));
    // The strips return once the editor closes; every one of them must come back
    // from the cache rather than being recomputed per typed character.
    assert!(window(&mut app) > 1);

    assert_eq!(
        app.diff_compute_count(),
        computed,
        "no neighbour's diff was recomputed across the whole editing session"
    );
}
