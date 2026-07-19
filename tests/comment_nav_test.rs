//! Review-comment cursor, navigation, deletion, and mouse behavior (plan §3.4,
//! C4). Stores are seeded by writing `comments.json` directly into the repo's
//! `<.git>/strix` dir before opening the review session (the schema the human's
//! TUI and the agent's CLI share); tests drive an `App` and assert on
//! `dump_frame` / buffer output and the exposed comment + cursor state.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{git, init_repo, init_repo_with_diverged_branches, write};
use ratatui::style::Color;
use strix::app::{App, FlashKind};
use strix::comments::{Branch, Comment, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use strix::terminal::{dump_frame, render_to_buffer};
use tempfile::TempDir;

const W: u16 = 100;
const H: u16 = 24;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
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

fn comment(id: u64, file: &str, side: Side, line: usize, text: &str, ctx: &str) -> Comment {
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

fn store_text(repo: &Path) -> String {
    std::fs::read_to_string(strix_dir(repo).join("comments.json")).unwrap()
}

/// Move the review selection (in the file list) until `path`'s diff is showing.
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

fn row_of(frame: &str, needle: &str) -> usize {
    frame
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("frame missing {needle:?}:\n{frame}"))
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

// --- Cursor movement ---

#[test]
fn j_k_move_the_cursor_and_g_edges_jump() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l')); // focus the diff pane
    assert_eq!(app.review_cursor(), 0);

    app.on_key(key('j'));
    app.on_key(key('j'));
    assert_eq!(app.review_cursor(), 2, "j moves the cursor down");
    app.on_key(key('k'));
    assert_eq!(app.review_cursor(), 1, "k moves it back up");

    app.on_key(key('G'));
    let bottom = app.review_cursor();
    assert!(bottom >= 40, "G jumps to the last row: {bottom}");
    app.on_key(key('g'));
    assert_eq!(app.review_cursor(), 0, "g jumps to the first row");
}

#[test]
fn cursor_clamps_at_the_last_row() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l'));
    for _ in 0..200 {
        app.on_key(key('j'));
    }
    let a = app.review_cursor();
    app.on_key(key('j'));
    assert_eq!(app.review_cursor(), a, "the cursor clamps at the last row");
}

#[test]
fn cursor_reaches_an_injected_comment_row_in_both_modes() {
    // G lands on the comment row appended below the final line; deleting from
    // there proves the cursor traversed onto the injected row (index-free).
    for sbs in [false, true] {
        let repo = tall_repo();
        seed_store(
            repo.path(),
            "feature",
            Some("main"),
            vec![comment(1, "big.txt", Side::New, 40, "tail note", "row 40")],
        );
        let mut app = review(repo.path(), "main");
        assert!(!app.review_comment(1).unwrap().orphaned);
        let _ = dump(&app);
        if sbs {
            app.on_key(key('d')); // side-by-side
        }
        app.on_key(key('l')); // focus diff
        app.on_key(key('G')); // cursor to the last (comment) row
        app.on_key(key('x')); // delete it
        assert!(
            app.review_comment(1).is_none(),
            "the cursor sat on the comment row (sbs={sbs})"
        );
    }
}

#[test]
fn far_cursor_move_reveals_the_row_act_and_reveal() {
    let repo = tall_repo();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![comment(1, "big.txt", Side::New, 40, "final note", "row 40")],
    );
    let mut app = review(repo.path(), "main");
    app.on_key(key('l'));
    assert!(
        !dump(&app).contains("final note"),
        "the last row starts offscreen"
    );
    for _ in 0..60 {
        app.on_key(key('j'));
    }
    assert!(app.diff_scroll > 0, "the viewport followed the cursor down");
    assert!(
        dump(&app).contains("● you final note"),
        "the cursor's row was revealed"
    );
}

#[test]
fn a_relist_that_shrinks_the_diff_clamps_the_cursor() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l'));
    app.on_key(key('G'));
    assert!(app.review_cursor() >= 40);

    // Replace the 40-line file with a single line and re-review: far fewer rows.
    write(repo.path(), "big.txt", "row 1\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "shrink"]);
    app.reload();

    assert!(
        app.review_cursor() <= 1,
        "the cursor clamped to the shorter row list: {}",
        app.review_cursor()
    );
}

// --- Cursor rendering only while the diff is focused ---

/// Whether any cell in buffer row `y` carries background `bg`.
fn row_has_bg(app: &App, y: usize, bg: Color) -> bool {
    let buffer = render_to_buffer(app, W, H).unwrap();
    (0..W).any(|x| buffer.cell((x, y as u16)).map(|c| c.bg) == Some(bg))
}

#[test]
fn cursor_row_shows_selection_bg_only_when_the_diff_is_focused() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![comment(
            1,
            "feature.txt",
            Side::New,
            1,
            "cursor mark",
            "feature",
        )],
    );
    let mut app = review(repo.path(), "main");
    let sel_bg = app.theme.selection_bg;
    select_file(&mut app, "feature.txt");
    app.on_key(key('l')); // focus the diff

    // Park the cursor on the comment row and find its screen row.
    app.on_key(key(']'));
    let frame = dump(&app);
    let row = row_of(&frame, "● you cursor mark");
    assert!(
        row_has_bg(&app, row, sel_bg),
        "the cursor row is painted with the selection background while focused:\n{frame}"
    );

    // Focus the file list: the cursor highlight must disappear from the diff.
    app.on_key(key('h'));
    let frame = dump(&app);
    let row = row_of(&frame, "● you cursor mark");
    assert!(
        !row_has_bg(&app, row, sel_bg),
        "no cursor highlight while the list is focused:\n{frame}"
    );
}

// --- Comment navigation (]/[) ---

/// Diverged repo with an anchored comment on each of two listed files.
fn two_commented_files() -> TempDir {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![
            comment(1, "feature.txt", Side::New, 1, "note one", "feature"),
            comment(2, "feature2.txt", Side::New, 1, "note two", "more"),
        ],
    );
    repo
}

#[test]
fn next_comment_wraps_across_files_and_focuses_the_diff() {
    let repo = two_commented_files();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);

    app.on_key(key(']'));
    let first = app.review_selected();
    assert!(!app.review_list_focused(), "] focuses the diff pane");

    app.on_key(key(']'));
    let second = app.review_selected();
    assert_ne!(first, second, "] advances to the comment on the other file");

    app.on_key(key(']'));
    assert_eq!(
        app.review_selected(),
        first,
        "] wraps back to the first file"
    );
}

#[test]
fn prev_comment_walks_backwards() {
    let repo = two_commented_files();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);

    app.on_key(key('['));
    let last = app.review_selected();
    assert!(!app.review_list_focused(), "[ focuses the diff pane");
    app.on_key(key('['));
    assert_ne!(last, app.review_selected(), "[ steps to the previous file");
    app.on_key(key('['));
    assert_eq!(app.review_selected(), last, "[ wraps around");
}

#[test]
fn comment_nav_with_no_comments_flashes() {
    let repo = init_repo_with_diverged_branches();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key(']'));
    let flash = app.flash.clone().expect("a flash is shown");
    assert_eq!(flash.kind, FlashKind::Info);
    assert_eq!(flash.text, "no comments");
}

// --- Delete (x) ---

#[test]
fn x_deletes_exactly_the_cursor_comment() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![
            comment(1, "feature.txt", Side::New, 1, "firstnote", "feature"),
            comment(2, "feature.txt", Side::New, 1, "secondnote", "feature"),
        ],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key(']')); // cursor onto the first comment (lowest id)
    app.on_key(key('x'));

    assert!(
        app.review_comment(1).is_none(),
        "the cursor comment is gone"
    );
    assert!(
        app.review_comment(2).is_some(),
        "the other comment survives"
    );
    let flash = app.flash.clone().expect("info flash");
    assert_eq!(flash.kind, FlashKind::Info);
    assert_eq!(flash.text, "comment deleted");

    let stored = store_text(repo.path());
    assert!(
        !stored.contains("firstnote"),
        "deleted from the store:\n{stored}"
    );
    assert!(stored.contains("secondnote"), "the other note persists");

    select_file(&mut app, "feature.txt");
    let frame = dump(&app);
    assert!(!frame.contains("firstnote"), "the row is gone:\n{frame}");
    assert!(frame.contains("secondnote"));
}

#[test]
fn x_on_a_code_row_is_a_silent_no_op() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![comment(
            1,
            "feature.txt",
            Side::New,
            1,
            "keep me",
            "feature",
        )],
    );
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "feature.txt");
    app.on_key(key('l')); // focus diff, cursor on a code (hunk) row
    app.on_key(key('j')); // still a code row (the `+ feature` line)
    let before = store_text(repo.path());

    app.on_key(key('x'));
    assert!(app.review_comment(1).is_some(), "no comment deleted");
    assert_eq!(store_text(repo.path()), before, "the store is byte-stable");
    assert!(app.flash.is_none(), "no flash on a code-row x");
}

#[test]
fn a_failed_delete_keeps_the_comment_and_flashes() {
    use std::os::unix::fs::PermissionsExt;

    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![comment(1, "feature.txt", Side::New, 1, "sticky", "feature")],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key(']')); // cursor onto the comment
    let before = store_text(repo.path());

    // Make the store dir read-only so the atomic write can't create its temp file.
    let dir = strix_dir(repo.path());
    let mut perms = std::fs::metadata(&dir).unwrap().permissions();
    let original = perms.mode();
    perms.set_mode(0o555);
    std::fs::set_permissions(&dir, perms).unwrap();

    app.on_key(key('x'));

    // Restore perms first so the assertions (and TempDir cleanup) can proceed.
    let mut restore = std::fs::metadata(&dir).unwrap().permissions();
    restore.set_mode(original);
    std::fs::set_permissions(&dir, restore).unwrap();

    assert!(app.review_comment(1).is_some(), "the comment is unchanged");
    assert_eq!(store_text(repo.path()), before, "the store is untouched");
    let flash = app.flash.clone().expect("error flash");
    assert_eq!(flash.kind, FlashKind::Error);
}

// --- Mouse ---

fn mouse(col: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn clicking_a_diff_row_focuses_the_pane_and_moves_the_cursor() {
    let repo = init_repo_with_diverged_branches();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![comment(
            1,
            "feature.txt",
            Side::New,
            1,
            "click target",
            "feature",
        )],
    );
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "feature.txt");
    app.on_key(key('l'));
    let frame = dump(&app);

    let screen_row = row_of(&frame, "● you click target") as u16;
    let diff = app.diff_area();
    app.on_mouse(mouse(
        diff.x + 3,
        screen_row,
        MouseEventKind::Down(MouseButton::Left),
    ));

    assert!(
        !app.review_list_focused(),
        "the click focuses the diff pane"
    );
    let expected = app.diff_scroll + (screen_row - diff.y);
    assert_eq!(
        app.review_cursor() as u16,
        expected,
        "the cursor jumps to the clicked row"
    );
}

#[test]
fn wheel_scroll_moves_the_viewport_but_not_the_cursor() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l'));
    assert_eq!(app.review_cursor(), 0);

    let diff = app.diff_area();
    app.on_mouse(mouse(diff.x + 2, diff.y + 1, MouseEventKind::ScrollDown));

    assert_eq!(app.review_cursor(), 0, "the wheel leaves the cursor alone");
    assert!(app.diff_scroll > 0, "the wheel scrolled the viewport");
}

#[test]
fn a_click_after_content_shrinks_hits_the_visually_clicked_row() {
    // Mouse hit-testing must use the same clamped offset the renderer paints
    // with; a raw diff_scroll desyncs clicks once the row list shrinks below it.
    // Deleting a comment row while scrolled to the bottom parks diff_scroll one
    // past the new max without recomputing the diff (no scroll reset).
    let repo = tall_repo();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![comment(1, "big.txt", Side::New, 40, "tail note", "row 40")],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l'));
    app.on_key(key('G')); // cursor onto the trailing comment row; diff_scroll == max
    assert!(app.diff_scroll > 0);

    app.on_key(key('x')); // delete it: the row list shrinks by one
    let frame = dump(&app);
    assert!(
        app.diff_scroll > app.diff_max_scroll(),
        "diff_scroll is parked one past the shrunken row list's max"
    );

    // Click the top visible row; it must resolve through the clamped offset.
    let diff = app.diff_area();
    app.on_mouse(mouse(
        diff.x + 2,
        diff.y,
        MouseEventKind::Down(MouseButton::Left),
    ));
    let clamped = app.diff_scroll.min(app.diff_max_scroll());
    assert_eq!(
        app.review_cursor() as u16,
        clamped,
        "the click hit the visually top row, not a phantom one row past it:\n{frame}"
    );
}

// --- Act-and-reveal gate (findings 1-3) ---

#[test]
fn x_reveals_an_offscreen_cursor_before_deleting() {
    // A wheel scroll can push the cursor row offscreen. The first `x` must only
    // reveal it (no delete); a second `x`, now that it's visible, deletes.
    let repo = tall_repo();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![comment(1, "big.txt", Side::New, 1, "topnote", "row 1")],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key(']')); // cursor onto the comment near the top, diff focused
    let cursor = app.review_cursor();

    // Wheel the viewport down until the cursor row sits above it.
    let diff = app.diff_area();
    for _ in 0..12 {
        app.on_mouse(mouse(diff.x + 2, diff.y + 1, MouseEventKind::ScrollDown));
    }
    assert!(
        app.diff_scroll as usize > cursor,
        "the cursor scrolled offscreen"
    );
    let before = store_text(repo.path());

    app.on_key(key('x'));
    assert!(
        app.review_comment(1).is_some(),
        "the first x reveals but must not delete an invisible row"
    );
    assert_eq!(
        store_text(repo.path()),
        before,
        "the store is untouched by the reveal"
    );
    assert!(
        dump(&app).contains("● you topnote"),
        "the cursor's row was scrolled into view"
    );

    app.on_key(key('x'));
    assert!(
        app.review_comment(1).is_none(),
        "the second x, on the now-visible row, deletes"
    );
}

#[test]
fn nav_onto_the_same_file_keeps_the_cursor() {
    // File-list nav that lands on the same file must not reset the cursor (which,
    // without a matching scroll reset, would strand it offscreen).
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l'));
    app.on_key(key('G')); // cursor + viewport to the bottom
    let cursor = app.review_cursor();
    let scroll = app.diff_scroll;
    assert!(cursor > 0 && scroll > 0);

    app.on_key(key('h')); // focus the file list
    app.on_key(key('j')); // down, but there is only one file → same selection
    assert_eq!(
        app.review_cursor(),
        cursor,
        "the cursor is not reset when the selection didn't change"
    );
    assert_eq!(app.diff_scroll, scroll, "the viewport is unchanged");
}

#[test]
fn half_page_moves_cursor_with_diff_focus_scrolls_with_list_focus() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);

    // Diff focused: ctrl-d moves the diff cursor.
    app.on_key(key('l'));
    assert_eq!(app.review_cursor(), 0);
    app.on_key(ctrl('d'));
    assert!(app.review_cursor() > 0, "ctrl-d moves the diff cursor");

    // List focused: ctrl-d scrolls the viewport only, leaving the cursor put.
    app.on_key(key('h'));
    let cursor = app.review_cursor();
    let scroll = app.diff_scroll;
    app.on_key(ctrl('d'));
    assert_eq!(
        app.review_cursor(),
        cursor,
        "the cursor is unmoved while the list is focused"
    );
    assert!(
        app.diff_scroll > scroll,
        "ctrl-d still scrolls the diff viewport"
    );
}

// --- Cycle ordering respects side (finding 4) ---

/// A repo whose `feature` branch replaces `f.txt`'s single line (alpha → beta),
/// so the diff is one deletion (old side) paired with one addition (new side).
fn replaced_line_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    write(p, "f.txt", "alpha\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add f"]);
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "f.txt", "beta\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "replace f"]);
    dir
}

#[test]
fn cycle_visits_a_replaced_line_in_old_then_new_visual_order() {
    // On a replaced line the SBS layout emits old-side comments (by id) before
    // new-side ones. With old ids 1 and 3 and new id 2, the visual order is
    // 1, 3, 2 — cycling must follow it, not the naive (line, id) order 1, 2, 3.
    let repo = replaced_line_repo();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![
            comment(1, "f.txt", Side::Old, 1, "aaa", "alpha"),
            comment(3, "f.txt", Side::Old, 1, "bbb", "alpha"),
            comment(2, "f.txt", Side::New, 1, "ccc", "beta"),
        ],
    );
    let mut app = review(repo.path(), "main");
    let sel_bg = app.theme.selection_bg;
    for id in [1, 3, 2] {
        assert!(
            !app.review_comment(id).unwrap().orphaned,
            "comment {id} anchors"
        );
    }
    let _ = dump(&app);

    for expected in ["aaa", "bbb", "ccc", "aaa"] {
        app.on_key(key(']'));
        let frame = dump(&app);
        let row = row_of(&frame, &format!("● you {expected}"));
        assert!(
            row_has_bg(&app, row, sel_bg),
            "] placed the cursor on {expected} in visual order:\n{frame}"
        );
    }
}
