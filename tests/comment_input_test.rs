//! In-place comment editor (plan §3.5, C7): the `c` action's per-LineKind and
//! per-gate behavior, the multi-line editable box at the anchor, the newline
//! chords + bracketed paste, and transactional save (add + edit). Stores are read
//! back through `strix::comments::load` — the same schema the human's TUI and the
//! agent's CLI share — and the TUI is driven with `press`-style key events (and
//! the `on_paste` seam), asserting on the persisted JSON and `dump_frame`.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{git, init_repo, init_repo_with_diverged_branches, write};
use strix::app::{App, DiffMode, FlashKind, ViewMode};
use strix::comments::{Branch, Comment, Scope, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use strix::terminal::dump_frame;
use tempfile::TempDir;

const W: u16 = 100;
const H: u16 = 24;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn code(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn enter() -> KeyEvent {
    KeyEvent::from(KeyCode::Enter)
}

fn shift_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
}

fn alt_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
}

fn ctrl_j() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)
}

fn dump(app: &App) -> String {
    dump_frame(app, W, H).unwrap()
}

fn dump_hw(app: &App, w: u16, h: u16) -> String {
    dump_frame(app, w, h).unwrap()
}

fn review(repo: &Path, range: &str) -> App {
    App::for_review(repo.to_path_buf(), &Config::default(), range).unwrap()
}

fn strix_dir(repo: &Path) -> PathBuf {
    repo.join(".git").join("strix")
}

/// Type a string into the app one key at a time (each char a plain key event).
fn typ(app: &mut App, s: &str) {
    for ch in s.chars() {
        app.on_key(key(ch));
    }
}

fn comments_of(repo: &Path, branch: &str) -> Vec<Comment> {
    let store = strix::comments::load(&strix_dir(repo)).unwrap();
    store
        .branches
        .get(branch)
        .map(|b| b.comments.clone())
        .unwrap_or_default()
}

fn store_text(repo: &Path) -> String {
    std::fs::read_to_string(strix_dir(repo).join("comments.json")).unwrap()
}

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

fn human(id: u64, file: &str, side: Side, line: usize, text: &str, ctx: &str) -> Comment {
    Comment {
        // These tests seed a `strix diff` range review; the empty range value
        // matches any active range (an unscoped placeholder).
        scope: Scope::Range {
            range: String::new(),
        },
        id,
        source: Source::Human,
        file: file.to_string(),
        side,
        line,
        text: text.to_string(),
        context: Some(ctx.to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
        base: None,
        stale: false,
    }
}

/// The 0-based frame row of the first line containing `needle`.
fn row_of(frame: &str, needle: &str) -> usize {
    frame
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("frame missing {needle:?}:\n{frame}"))
}

/// A repo whose `feature` branch edits the middle line of a 5-line file, so the
/// diff carries context lines above and below a deletion+addition pair.
fn context_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    write(p, "code.txt", "one\ntwo\nthree\nfour\nfive\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add code"]);
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "code.txt", "one\ntwo\nTHREE\nfour\nfive\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "edit line 3"]);
    dir
}

/// A repo whose `feature` branch adds a 40-line file (single file in range).
fn tall_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
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

/// A repo whose `feature` branch adds two files, for cross-file routing tests.
fn two_file_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "one.txt", "alpha\n");
    write(p, "two.txt", "beta\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add two"]);
    dir
}

/// Focus the diff pane and move the cursor to row `n` (from the top).
fn cursor_to(app: &mut App, n: usize) {
    app.on_key(key('l'));
    app.on_key(key('g'));
    for _ in 0..n {
        app.on_key(key('j'));
    }
}

fn mouse(col: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

// --- `c` captures the right anchor per LineKind (add) ---

#[test]
fn c_on_code_rows_captures_side_line_and_context() {
    // Row layout for the context_repo diff: 0 hunk, 1 ctx "one", 2 ctx "two",
    // 3 del "three", 4 add "THREE", 5 ctx "four", 6 ctx "five".
    let cases = [
        (1usize, Side::New, 1usize, "one", "ctxnote"),
        (3, Side::Old, 3, "three", "delnote"),
        (4, Side::New, 3, "THREE", "addnote"),
    ];
    for (row, side, line, ctx, text) in cases {
        let repo = context_repo();
        let mut app = review(repo.path(), "main");
        let _ = dump(&app);
        cursor_to(&mut app, row);
        app.on_key(key('c'));
        assert!(app.editor_open(), "the editor opened on row {row}");
        typ(&mut app, text);
        app.on_key(enter());

        let comments = comments_of(repo.path(), "feature");
        let c = comments
            .iter()
            .find(|c| c.text == text)
            .unwrap_or_else(|| panic!("stored comment for row {row}: {comments:?}"));
        assert_eq!(c.side, side, "row {row} side");
        assert_eq!(c.line, line, "row {row} line");
        assert_eq!(c.context.as_deref(), Some(ctx), "row {row} context");
        assert_eq!(c.source, Source::Human, "authored as a human note");
        assert!(!c.orphaned, "a fresh anchor is not orphaned");
    }
}

#[test]
fn the_editor_box_renders_in_place_below_the_anchor() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('b')); // hide the Changes panel → full-width diff
    cursor_to(&mut app, 1); // ctx "one" (New line 1)
    app.on_key(key('c'));
    typ(&mut app, "inline");

    let frame = dump(&app);
    // The editor box opens directly under its anchor code line, with a title +
    // the live buffer in its body (no centered modal).
    let anchor = row_of(&frame, " one"); // the " one" context line
    let title = row_of(&frame, "✎ you — code.txt R1");
    let body = row_of(&frame, "inline");
    assert_eq!(title, anchor + 1, "editor opens below the anchor:\n{frame}");
    assert_eq!(body, anchor + 2, "the buffer is on the body row:\n{frame}");
    assert!(
        frame.lines().any(|l| l.contains('╭')),
        "the editor is a bordered box:\n{frame}"
    );
}

#[test]
fn c_on_a_hunk_row_flashes_and_opens_no_editor() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 0); // the @@ hunk header
    app.on_key(key('c'));
    assert!(!app.editor_open(), "no editor on a hunk row");
    let flash = app.flash.clone().expect("a flash");
    assert_eq!(flash.kind, FlashKind::Info);
    assert_eq!(flash.text, "can't comment here");
}

// --- Gates ---

#[test]
fn c_with_the_list_focused_flashes() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app); // default focus is the file list
    app.on_key(key('c'));
    assert!(!app.editor_open());
    let flash = app.flash.clone().expect("a flash");
    assert_eq!(flash.kind, FlashKind::Info);
    assert_eq!(flash.text, "focus the diff to comment");
}

#[test]
fn c_in_a_non_authoring_session_flashes() {
    // A fixed `feature..main` range: its head is `main`, but HEAD is `feature`,
    // so the reviewed head isn't checked out and authoring is off (plan §3.1.1).
    let repo = init_repo_with_diverged_branches();
    let mut app = review(repo.path(), "feature..main");
    let _ = dump(&app);
    app.on_key(key('c'));
    assert!(!app.editor_open());
    let flash = app.flash.clone().expect("a flash");
    assert_eq!(flash.kind, FlashKind::Info);
    assert_eq!(flash.text, "check out the reviewed branch to comment");
}

#[test]
fn c_on_an_agent_note_is_read_only() {
    let repo = init_repo_with_diverged_branches();
    let mut agent = human(1, "feature.txt", Side::New, 1, "agent said", "feature");
    agent.source = Source::Agent;
    seed_store(repo.path(), "feature", Some("main"), vec![agent]);
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key(']')); // cursor onto the agent comment row, diff focused
    app.on_key(key('c'));
    assert!(!app.editor_open(), "agent notes don't open the editor");
    let flash = app.flash.clone().expect("a flash");
    assert_eq!(flash.kind, FlashKind::Info);
    assert_eq!(flash.text, "agent note — read-only");
}

// --- Edit an existing human note ---

#[test]
fn c_edits_a_human_note_prefilled_and_updates_only_text() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![human(1, "feature.txt", Side::New, 1, "origtext", "feature")],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key(']')); // cursor onto the comment
    app.on_key(key('c'));
    assert!(app.editor_open(), "editing opens the editor");
    let frame = dump(&app);
    assert!(
        frame.contains("origtext"),
        "the editor is prefilled with the note text:\n{frame}"
    );

    // Cursor starts at the end of the prefilled text; append and save.
    typ(&mut app, "!");
    app.on_key(enter());

    let comments = comments_of(repo.path(), "feature");
    assert_eq!(comments.len(), 1, "the edit didn't add a second comment");
    let c = &comments[0];
    assert_eq!(c.id, 1, "same comment id");
    assert_eq!(c.text, "origtext!", "text updated");
    assert_eq!(c.side, Side::New, "side unchanged");
    assert_eq!(c.line, 1, "line unchanged");
    let flash = app.flash.clone().expect("a flash");
    assert_eq!(flash.text, "comment updated");
}

/// An edit whose text is unchanged writes nothing (extends the elision contract).
#[test]
fn a_no_op_edit_writes_nothing() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![human(
            1,
            "feature.txt",
            Side::New,
            1,
            "unchanged",
            "feature",
        )],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    let before = store_text(repo.path());
    app.on_key(key(']'));
    app.on_key(key('c'));
    assert!(app.editor_open());
    app.on_key(enter()); // save without changing anything

    assert!(!app.editor_open(), "the editor closed");
    assert_eq!(
        store_text(repo.path()),
        before,
        "a no-op edit leaves the store byte-stable"
    );
}

#[test]
fn editing_a_concurrently_removed_note_flashes_and_drops_the_edit() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![human(
            1,
            "feature.txt",
            Side::New,
            1,
            "gone soon",
            "feature",
        )],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key(']'));
    app.on_key(key('c'));
    assert!(app.editor_open());
    typ(&mut app, "edit");

    // A concurrent writer (e.g. an agent `rm`) empties the branch's set.
    seed_store(repo.path(), "feature", Some("main"), vec![]);

    app.on_key(enter());
    assert!(!app.editor_open(), "the editor closes");
    assert!(
        comments_of(repo.path(), "feature").is_empty(),
        "the edit was dropped — no resurrection"
    );
    let flash = app.flash.clone().expect("a flash");
    assert_eq!(flash.text, "comment was removed");
}

// --- Codex fix #1: an untouched edit never clobbers a concurrent change ---

#[test]
fn an_untouched_edit_does_not_clobber_a_concurrent_change() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![human(1, "feature.txt", Side::New, 1, "original", "feature")],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key(']')); // cursor onto the comment
    app.on_key(key('c')); // open the editor (original_text captured = "original")
    assert!(app.editor_open());

    // A concurrent writer changes the note's text; the watcher reload refreshes the
    // in-memory comment to "concurrent" while the editor buffer stays "original".
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![human(
            1,
            "feature.txt",
            Side::New,
            1,
            "concurrent",
            "feature",
        )],
    );
    app.reload();
    let before = store_text(repo.path());

    app.on_key(enter()); // save with NO local change

    assert!(!app.editor_open(), "the editor closed");
    let comments = comments_of(repo.path(), "feature");
    assert_eq!(comments.len(), 1);
    assert_eq!(
        comments[0].text, "concurrent",
        "an untouched editor never overwrites the concurrent change"
    );
    assert_eq!(store_text(repo.path()), before, "no write happened");
}

// --- Codex fix #2: the edit transaction only touches an existing record's text ---

#[test]
fn a_vanished_edit_does_not_recreate_the_removed_branch() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![human(1, "feature.txt", Side::New, 1, "doomed", "feature")],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key(']'));
    app.on_key(key('c'));
    typ(&mut app, "-edited"); // a real local change (not a no-op)

    // The whole branch entry is removed concurrently (an empty store, no branches).
    let empty = Store {
        version: 2,
        next_id: 1000,
        branches: BTreeMap::new(),
    };
    let dir = strix_dir(repo.path());
    std::fs::write(
        dir.join("comments.json"),
        serde_json::to_string_pretty(&empty).unwrap(),
    )
    .unwrap();
    let before = store_text(repo.path());

    app.on_key(enter()); // save → Vanished (branch gone): no write

    let store = strix::comments::load(&dir).unwrap();
    assert!(
        !store.branches.contains_key("feature"),
        "a Vanished edit does not resurrect the removed branch entry (no or_default)"
    );
    assert_eq!(
        store_text(repo.path()),
        before,
        "a Vanished edit persists nothing (no metadata-only branch)"
    );
    let flash = app.flash.clone().expect("a flash");
    assert_eq!(flash.text, "comment was removed");
}

#[test]
fn an_edit_changes_only_the_comment_text_not_branch_metadata() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![human(1, "feature.txt", Side::New, 1, "keep-me", "feature")],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    // Snapshot after session-open settled (range recorded, no re-anchor move).
    let before = strix::comments::load(&strix_dir(repo.path())).unwrap();

    app.on_key(key(']'));
    app.on_key(key('c'));
    typ(&mut app, "!"); // "keep-me" -> "keep-me!"
    app.on_key(enter());

    let after = strix::comments::load(&strix_dir(repo.path())).unwrap();
    let (a, b) = (&after.branches["feature"], &before.branches["feature"]);
    assert_eq!(
        a.active_range, b.active_range,
        "the edit left active_range untouched (no record_range on the edit path)"
    );
    assert_eq!(a.comments.len(), 1);
    assert_eq!(a.comments[0].text, "keep-me!", "the text updated");
    // Everything but the text is byte-identical.
    let (mut ac, mut bc) = (a.comments[0].clone(), b.comments[0].clone());
    ac.text = String::new();
    bc.text = String::new();
    assert_eq!(ac, bc, "only the comment's text field changed");
}

// --- Codex fix #3: a save after checkout keeps the current view's own set ---

#[test]
fn a_save_after_checkout_does_not_install_the_old_branch_set() {
    let repo = init_repo_with_diverged_branches(); // HEAD is `feature`
    let p = repo.path();
    let mut app = review(p, "main"); // authoring under `feature`
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "note on feature");

    // Checkout swings the active inbox to `main`; the reload refreshes the view's
    // own (empty) set.
    git(p, &["checkout", "-q", "main"]);
    app.reload();

    app.on_key(enter()); // persists under `feature`; must not install into the view

    assert_eq!(
        app.review_comment_count("feature.txt"),
        0,
        "the captured branch's set was not installed into the now-active view"
    );
    let store = strix::comments::load(&strix_dir(p)).unwrap();
    assert!(
        store
            .branches
            .get("feature")
            .is_some_and(|b| b.comments.iter().any(|c| c.text == "note on feature")),
        "the note still persisted under its authoring branch"
    );
}

// --- Multi-line: newline chords + wrapping ---

#[test]
fn shift_enter_ctrl_j_and_alt_enter_each_insert_a_newline() {
    for (label, newline) in [
        ("shift", shift_enter()),
        ("ctrlj", ctrl_j()),
        ("altenter", alt_enter()),
    ] {
        let repo = context_repo();
        let mut app = review(repo.path(), "main");
        let _ = dump(&app);
        cursor_to(&mut app, 1);
        app.on_key(key('c'));
        typ(&mut app, "line1");
        app.on_key(newline);
        typ(&mut app, "line2");
        app.on_key(enter()); // plain Enter saves

        let comments = comments_of(repo.path(), "feature");
        assert_eq!(comments.len(), 1, "{label}: one comment");
        assert_eq!(
            comments[0].text, "line1\nline2",
            "{label}: the newline chord split the note into two lines"
        );
    }
}

#[test]
fn a_long_line_wraps_across_body_rows() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    let long = format!("START{}END", "x".repeat(60));
    typ(&mut app, &long);

    let frame = dump(&app);
    let start = row_of(&frame, "START");
    let end = row_of(&frame, "END");
    assert!(
        end > start,
        "the long line wrapped onto a later body row (START {start}, END {end}):\n{frame}"
    );
}

// --- Bracketed paste inserts newlines (via the on_paste seam) ---

#[test]
fn paste_inserts_multi_line_text_at_the_caret() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "pre ");
    app.on_paste("pasted\ntwo\r\nlines"); // \r\n normalises to a newline too
    app.on_key(enter());

    let comments = comments_of(repo.path(), "feature");
    assert_eq!(comments.len(), 1);
    assert_eq!(
        comments[0].text, "pre pasted\ntwo\nlines",
        "the pasted newlines became real lines, not saves"
    );
}

#[test]
fn a_paste_with_no_editor_open_is_ignored() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_paste("stray paste"); // no editor open
    assert!(!app.editor_open());
    assert!(
        comments_of(repo.path(), "feature").is_empty(),
        "a stray paste never leaks into the store"
    );
}

// --- Typing: multibyte round-trip + editing keys ---

#[test]
fn multibyte_text_round_trips_and_renders() {
    // Combining diaeresis (a + U+0308), a CJK ideograph, and an emoji.
    let text = "a\u{308}字🎉";
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, text);
    app.on_key(enter());

    let comments = comments_of(repo.path(), "feature");
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].text, text, "the multibyte text stored exactly");

    let frame = dump(&app);
    assert!(
        frame.contains("字"),
        "the comment row renders the multibyte text:\n{frame}"
    );
}

#[test]
fn editing_keys_move_and_delete_by_char() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));

    typ(&mut app, "abc"); // "abc|"
    app.on_key(code(KeyCode::Home)); // "|abc"
    app.on_key(code(KeyCode::Right)); // "a|bc"
    app.on_key(key('Z')); // "aZ|bc"
    app.on_key(code(KeyCode::End)); // "aZbc|"
    app.on_key(code(KeyCode::Backspace)); // "aZb|"
    app.on_key(code(KeyCode::Left)); // "aZ|b"
    app.on_key(code(KeyCode::Delete)); // "aZ|"
    app.on_key(enter());

    let comments = comments_of(repo.path(), "feature");
    assert_eq!(comments.len(), 1);
    assert_eq!(
        comments[0].text, "aZ",
        "arrows/home/end/backspace/delete applied"
    );
}

#[test]
fn up_down_move_between_hard_lines_at_a_preferred_column() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    // Two hard lines: "hello" and "world"; caret at end of "world" (1, 5).
    typ(&mut app, "hello");
    app.on_key(shift_enter());
    typ(&mut app, "world");
    assert_eq!(app.editor_cursor(), Some((1, 5)));

    // Up keeps the preferred display column (5) on the first line.
    app.on_key(code(KeyCode::Up));
    assert_eq!(
        app.editor_cursor(),
        Some((0, 5)),
        "up lands at the same column"
    );
    // Home then Down retains column 0 down to the second line.
    app.on_key(code(KeyCode::Home));
    app.on_key(code(KeyCode::Down));
    assert_eq!(app.editor_cursor(), Some((1, 0)), "down retains column 0");
}

#[test]
fn backspace_at_line_start_merges_with_the_previous_line() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "foo");
    app.on_key(shift_enter());
    typ(&mut app, "bar");
    app.on_key(code(KeyCode::Home)); // caret at (1, 0)
    app.on_key(code(KeyCode::Backspace)); // joins the two lines
    assert_eq!(app.editor_buffer().as_deref(), Some("foobar"));
    assert_eq!(app.editor_cursor(), Some((0, 3)), "caret at the join point");
}

// --- Empty / cancel paths leave the store untouched ---

#[test]
fn whitespace_only_save_writes_nothing() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    let before = store_text(repo.path()); // session-open already recorded the range
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "   ");
    app.on_key(enter());

    assert!(!app.editor_open(), "the editor closed");
    assert!(
        comments_of(repo.path(), "feature").is_empty(),
        "no comment written"
    );
    assert_eq!(store_text(repo.path()), before, "the store is byte-stable");
}

#[test]
fn esc_discards_and_leaves_the_store_byte_stable() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    let before = store_text(repo.path());
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "discard me");
    app.on_key(code(KeyCode::Esc));

    assert!(!app.editor_open(), "Esc closed the editor");
    assert_eq!(store_text(repo.path()), before, "the store is untouched");
}

// --- Reload / checkout mid-typing must not dangle nor cross-save ---

#[test]
fn a_reload_mid_typing_keeps_the_anchor_and_editor() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1); // addition row "row 1" (New / line 1)
    app.on_key(key('c'));
    assert!(app.editor_open());

    typ(&mut app, "hel");
    app.reload(); // a watcher reload rebuilds the diff underneath the open editor
    assert!(app.editor_open(), "the editor survives a reload");
    typ(&mut app, "lo");
    app.on_key(enter());

    let comments = comments_of(repo.path(), "feature");
    assert_eq!(comments.len(), 1);
    let c = &comments[0];
    assert_eq!(c.text, "hello", "typing across the reload persisted");
    assert_eq!(c.side, Side::New, "the captured anchor side survived");
    assert_eq!(c.line, 1, "the captured anchor line survived");
    assert_eq!(c.context.as_deref(), Some("row 1"));
}

/// A checkout + reload mid-edit swings the current branch, but the save must land
/// under the branch the editor opened on (captured at open, plan §3.5).
#[test]
fn checkout_mid_edit_does_not_cross_save() {
    let repo = init_repo_with_diverged_branches(); // HEAD is `feature`
    let p = repo.path();
    let mut app = review(p, "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "authored on feature");

    // An external checkout to `main` + a watcher reload while the editor is open.
    git(p, &["checkout", "-q", "main"]);
    app.reload();

    app.on_key(enter());

    // The note landed under `feature` (the authoring branch), not `main`.
    let store = strix::comments::load(&strix_dir(p)).unwrap();
    let feature = store
        .branches
        .get("feature")
        .expect("the note landed under the authoring branch");
    assert_eq!(feature.comments.len(), 1);
    assert_eq!(feature.comments[0].text, "authored on feature");
    assert!(
        store
            .branches
            .get("main")
            .is_none_or(|b| b.comments.is_empty()),
        "nothing leaked into the branch checked out mid-edit"
    );
}

// --- Write failure keeps the editor + buffer ---

#[test]
fn a_failed_write_keeps_the_editor_open_with_the_buffer() {
    use std::os::unix::fs::PermissionsExt;

    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "keepme");

    // Make the store dir read-only so the atomic write can't create its temp file.
    let dir = strix_dir(repo.path());
    let mut perms = std::fs::metadata(&dir).unwrap().permissions();
    let original = perms.mode();
    perms.set_mode(0o555);
    std::fs::set_permissions(&dir, perms).unwrap();

    app.on_key(enter());

    // Restore perms before asserting so cleanup can proceed.
    let mut restore = std::fs::metadata(&dir).unwrap().permissions();
    restore.set_mode(original);
    std::fs::set_permissions(&dir, restore).unwrap();

    assert!(
        app.editor_open(),
        "the editor stays open after a failed write"
    );
    let flash = app.flash.clone().expect("an error flash");
    assert_eq!(flash.kind, FlashKind::Error);
    assert!(
        dump(&app).contains("keepme"),
        "the buffer is preserved for a retry"
    );
    assert!(
        comments_of(repo.path(), "feature").is_empty(),
        "nothing was written"
    );
}

// --- Ctrl-C still hard-quits from inside the editor ---

#[test]
fn ctrl_c_quits_from_inside_the_editor() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    assert!(app.editor_open());
    app.on_key(ctrl('c'));
    assert!(
        app.should_quit,
        "Ctrl-C hard-quits even with the editor open"
    );
}

// --- Blocked while editing: mode / history / file / pane keys insert ---

#[test]
fn mode_history_and_pane_keys_are_blocked_while_editing() {
    let repo = two_file_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    let mode = app.diff_mode;
    let file = app.active_diff_path();
    cursor_to(&mut app, 1);
    app.on_key(key('c'));

    // `d` (toggle side-by-side), `i` (history), `Tab` (switch pane), and `j`/`k`
    // (file/cursor nav) all route to the editor while it is open.
    app.on_key(key('d'));
    app.on_key(key('i'));
    app.on_key(code(KeyCode::Tab));
    app.on_key(key('j'));
    app.on_key(key('k'));

    assert!(app.editor_open(), "the editor is still open");
    assert_eq!(app.diff_mode, mode, "diff mode did not toggle");
    assert_eq!(app.view, ViewMode::Review, "history did not open");
    assert_eq!(app.active_diff_path(), file, "the file did not change");
    // The printable keys inserted; Tab (a non-char) was ignored.
    assert_eq!(
        app.editor_buffer().as_deref(),
        Some("dijk"),
        "d/i/j/k inserted as text, Tab was ignored"
    );
    assert_eq!(app.diff_mode, DiffMode::Unified);
}

// --- Click outside the editor saves, then routes the click ---

#[test]
fn click_outside_the_editor_saves_then_routes() {
    let repo = two_file_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app); // file 0 (one.txt) selected, list focused
    cursor_to(&mut app, 1); // focus diff, cursor onto one.txt's addition
    app.on_key(key('c'));
    typ(&mut app, "on one");

    // Click the file list row for two.txt (outside the editor).
    let list = app.review_list_area();
    app.on_mouse(mouse(
        list.x + 2,
        list.y + 1,
        MouseEventKind::Down(MouseButton::Left),
    ));

    // The click committed the editor (saved to one.txt) then routed to two.txt.
    assert!(!app.editor_open(), "the click closed the editor");
    let comments = comments_of(repo.path(), "feature");
    assert_eq!(comments.len(), 1, "the note saved on click-outside");
    assert_eq!(comments[0].file, "one.txt", "saved against its anchor file");
    assert_eq!(comments[0].text, "on one");
    assert_eq!(
        app.active_diff_path().as_deref(),
        Some("two.txt"),
        "the click then routed to the clicked file"
    );
}

/// A click *inside* the editor keeps it open (no save/route).
#[test]
fn click_inside_the_editor_keeps_editing() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('b')); // full-width diff
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "stay");

    let frame = dump(&app);
    let body = row_of(&frame, "stay") as u16;
    let diff = app.diff_area();
    app.on_mouse(mouse(
        diff.x + 3,
        body,
        MouseEventKind::Down(MouseButton::Left),
    ));

    assert!(app.editor_open(), "a click inside the editor keeps it open");
    assert!(
        comments_of(repo.path(), "feature").is_empty(),
        "no save happened on an inside click"
    );
}

// --- Act-and-reveal: an offscreen cursor reveals, no editor ---

#[test]
fn c_on_an_offscreen_cursor_reveals_without_opening() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l'));
    app.on_key(key('g'));
    app.on_key(key('j')); // cursor on row 1, near the top
    let cursor = app.review_cursor();

    // Wheel the viewport down until the cursor sits above it.
    let diff = app.diff_area();
    for _ in 0..12 {
        app.on_mouse(mouse(diff.x + 2, diff.y + 1, MouseEventKind::ScrollDown));
    }
    assert!(app.diff_scroll.get() > cursor, "the cursor is offscreen");

    app.on_key(key('c'));
    assert!(!app.editor_open(), "the first c only reveals — no editor");
    assert!(
        app.diff_scroll.get() <= cursor,
        "the cursor's row was scrolled back into view"
    );

    // Now that it's visible, c opens the editor on the code row.
    app.on_key(key('c'));
    assert!(app.editor_open(), "the second c opens the editor");
}

// --- Side-by-side: the editor renders in the anchor-side column, no panic ---

#[test]
fn the_editor_renders_in_side_by_side_without_panicking() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('d')); // side-by-side
    cursor_to(&mut app, 1); // ctx "one" (New side → the right column)
    app.on_key(key('c'));
    typ(&mut app, "sbs note");

    let frame = dump(&app);
    assert!(app.editor_open());
    assert!(
        frame.contains("sbs note"),
        "the editor body renders in side-by-side:\n{frame}"
    );
    // Narrow widths halve the pane; the box must degrade, never panic (mirrors the
    // comment-box narrow-width regression).
    for w in [40u16, 24, 16, 10, 6] {
        let _ = dump_hw(&app, w, 20);
    }
}

// --- A box taller than the viewport keeps the caret visible ---

#[test]
fn a_taller_than_viewport_editor_keeps_the_caret_visible() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump_hw(&app, W, 12); // set a short viewport
    app.on_key(key('b')); // full-width diff
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    let _ = dump_hw(&app, W, 12);
    // Paste 30 lines; the box is far taller than the ~8-row viewport.
    let mut body = String::new();
    for i in 1..=30 {
        body.push_str(&format!("L{i:02}\n"));
    }
    app.on_paste(body.trim_end());

    let frame = dump_hw(&app, W, 12);
    assert!(
        frame.contains("L30"),
        "the caret line (end of the paste) stays visible:\n{frame}"
    );
    assert!(
        !frame.contains("L01"),
        "the top of an over-tall editor scrolled off:\n{frame}"
    );
    assert!(app.editor_open(), "the editor is still open");
}
