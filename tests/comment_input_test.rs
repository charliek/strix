//! Review-comment input modal (plan §3.4, C5): the `c` action's per-LineKind
//! and per-gate behavior, the single-line editor, and transactional submit
//! (add + edit). Stores are read back through `strix::comments::load` — the same
//! schema the human's TUI and the agent's CLI share — and the TUI is driven with
//! `press`-style key events, asserting on the persisted JSON and `dump_frame`.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{git, init_repo, init_repo_with_diverged_branches, write};
use strix::app::{App, FlashKind, Modal};
use strix::comments::{Branch, Comment, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
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

fn dump(app: &App) -> String {
    dump_frame(app, W, H).unwrap()
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

fn modal_open(app: &App) -> bool {
    matches!(app.modal, Some(Modal::CommentInput { .. }))
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
            range: range.map(str::to_string),
            comments,
        },
    );
    let store = Store {
        version: 1,
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
        id,
        source: Source::Human,
        file: file.to_string(),
        side,
        line,
        text: text.to_string(),
        context: Some(ctx.to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
    }
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

/// Focus the diff pane and move the cursor to row `n` (from the top).
fn cursor_to(app: &mut App, n: usize) {
    app.on_key(key('l'));
    app.on_key(key('g'));
    for _ in 0..n {
        app.on_key(key('j'));
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
        assert!(modal_open(&app), "the modal opened on row {row}");
        typ(&mut app, text);
        app.on_key(code(KeyCode::Enter));

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
fn c_on_a_hunk_row_flashes_and_opens_no_modal() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 0); // the @@ hunk header
    app.on_key(key('c'));
    assert!(!modal_open(&app), "no editor on a hunk row");
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
    assert!(!modal_open(&app));
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
    assert!(!modal_open(&app));
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
    assert!(!modal_open(&app), "agent notes don't open the editor");
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
    assert!(modal_open(&app), "editing opens the modal");
    let frame = dump(&app);
    assert!(
        frame.contains("origtext"),
        "the editor is prefilled with the note text:\n{frame}"
    );

    // Cursor starts at the end of the prefilled text; append and save.
    typ(&mut app, "!");
    app.on_key(code(KeyCode::Enter));

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
    assert!(modal_open(&app));
    typ(&mut app, "edit");

    // A concurrent writer (e.g. an agent `rm`) empties the branch's set.
    seed_store(repo.path(), "feature", Some("main"), vec![]);

    app.on_key(code(KeyCode::Enter));
    assert!(!modal_open(&app), "the modal closes");
    assert!(
        comments_of(repo.path(), "feature").is_empty(),
        "the edit was dropped — no resurrection"
    );
    let flash = app.flash.clone().expect("a flash");
    assert_eq!(flash.text, "comment was removed");
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
    app.on_key(code(KeyCode::Enter));

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
    app.on_key(code(KeyCode::Enter));

    let comments = comments_of(repo.path(), "feature");
    assert_eq!(comments.len(), 1);
    assert_eq!(
        comments[0].text, "aZ",
        "arrows/home/end/backspace/delete applied"
    );
}

// --- Empty / cancel paths leave the store untouched ---

#[test]
fn whitespace_only_submit_writes_nothing() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    let before = store_text(repo.path()); // session-open already recorded the range
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "   ");
    app.on_key(code(KeyCode::Enter));

    assert!(!modal_open(&app), "the modal closed");
    assert!(
        comments_of(repo.path(), "feature").is_empty(),
        "no comment written"
    );
    assert_eq!(store_text(repo.path()), before, "the store is byte-stable");
}

#[test]
fn esc_cancels_and_leaves_the_store_byte_stable() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    let before = store_text(repo.path());
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    typ(&mut app, "discard me");
    app.on_key(code(KeyCode::Esc));

    assert!(!modal_open(&app), "Esc closed the modal");
    assert_eq!(store_text(repo.path()), before, "the store is untouched");
}

// --- Refresh mid-typing must not dangle the anchor ---

#[test]
fn a_reload_mid_typing_does_not_panic_and_submit_keeps_the_anchor() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1); // addition row "row 1" (New / line 1)
    app.on_key(key('c'));
    assert!(modal_open(&app));

    typ(&mut app, "hel");
    app.reload(); // a watcher reload rebuilds the diff underneath the open modal
    assert!(modal_open(&app), "the modal survives a reload");
    typ(&mut app, "lo");
    app.on_key(code(KeyCode::Enter));

    let comments = comments_of(repo.path(), "feature");
    assert_eq!(comments.len(), 1);
    let c = &comments[0];
    assert_eq!(c.text, "hello", "typing across the reload persisted");
    assert_eq!(c.side, Side::New, "the captured anchor side survived");
    assert_eq!(c.line, 1, "the captured anchor line survived");
    assert_eq!(c.context.as_deref(), Some("row 1"));
}

// --- Write failure keeps the modal + buffer ---

#[test]
fn a_failed_write_keeps_the_modal_open_with_the_buffer() {
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

    app.on_key(code(KeyCode::Enter));

    // Restore perms before asserting so cleanup can proceed.
    let mut restore = std::fs::metadata(&dir).unwrap().permissions();
    restore.set_mode(original);
    std::fs::set_permissions(&dir, restore).unwrap();

    assert!(
        modal_open(&app),
        "the modal stays open after a failed write"
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

// --- Ctrl-C still hard-quits from inside the modal ---

#[test]
fn ctrl_c_quits_from_inside_the_modal() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    assert!(modal_open(&app));
    app.on_key(ctrl('c'));
    assert!(
        app.should_quit,
        "Ctrl-C hard-quits even with the modal open"
    );
}

// --- Act-and-reveal: an offscreen cursor reveals, no modal ---

fn mouse(col: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

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
    assert!(app.diff_scroll as usize > cursor, "the cursor is offscreen");

    app.on_key(key('c'));
    assert!(!modal_open(&app), "the first c only reveals — no editor");
    assert!(
        (app.diff_scroll as usize) <= cursor,
        "the cursor's row was scrolled back into view"
    );

    // Now that it's visible, c opens the editor on the code row.
    app.on_key(key('c'));
    assert!(modal_open(&app), "the second c opens the editor");
}

// --- Horizontal scroll keeps the cursor region visible ---

#[test]
fn a_long_buffer_scrolls_to_show_the_cursor_region() {
    let repo = context_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    cursor_to(&mut app, 1);
    app.on_key(key('c'));
    let long = format!("START{}END", "x".repeat(60));
    typ(&mut app, &long);

    let frame = dump(&app);
    assert!(
        frame.contains("END"),
        "the tail (cursor region) is visible:\n{frame}"
    );
    assert!(
        !frame.contains("START"),
        "the head scrolled off the left edge:\n{frame}"
    );
}
