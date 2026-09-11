//! Changing the split — hiding or revealing the Changes panel (`b`), or
//! dragging the divider — re-keys every prepared section by width before any
//! frame records the new geometry (issue 28). The first frame after the change
//! must still draw a full strip, and a divergent cursor must never swallow the
//! next `j`: hiding and dragging keep the cursor (its file is still in the
//! window), revealing drops it (focus leaves the diff pane).
//!
//! Each toggle test parks a divergent cursor on a strip file, hides the panel,
//! and requires a full first frame, the kept cursor, and a moving first `j`; it
//! then re-diverges while hidden and requires a full first frame, the drop, and
//! a moving first `j` of the reveal. The `press` → `dump` → `j` order is
//! load-bearing: the dump records the retoggled width before the first step
//! reads it, and the window is inspected under *that* width — sections
//! prepared for the old one do not count.

mod common;

use common::{
    app_for, config, dump, history_select_row, init_repo, init_repo_with_diverged_branches,
    init_repo_with_multi_file_commit, mouse, press, rendered_app, selected_path, strip_row, tab,
    window_of, write,
};
use std::time::Instant;
use strix::app::{App, CursorAddress, FileId, RowTarget};
use strix::crossterm::event::{MouseButton, MouseEventKind};
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 14;

fn address(file: FileId, target: RowTarget) -> CursorAddress {
    CursorAddress { file, target }
}

/// The first strip code row's address plus its file's path (the frame needle).
/// Read off the prepared window so no test depends on the file-list order.
fn strip_code_address(app: &App) -> (CursorAddress, String) {
    let row = strip_row(app, "a strip code row", |row| {
        matches!(row.target, RowTarget::Code(_))
    });
    let path = row.file.path().to_string();
    (address(row.file.clone(), row.target), path)
}

fn wheel_down(app: &mut App) {
    let diff = app.diff_area();
    app.on_mouse(mouse(diff.x + 2, diff.y + 2, MouseEventKind::ScrollDown));
}

fn assert_first_j_moves(app: &mut App, what: &str) {
    let before = app.cursor_address();
    press(app, 'j');
    assert_ne!(
        before,
        app.cursor_address(),
        "the first j after {what} moves"
    );
}

/// Render the first frame after a split change and require the window it drew
/// to reach past the anchor: the strip sections must already be prepared under
/// the width that frame recorded, not left for a later event to repair.
fn assert_first_frame_full(app: &App, what: &str) {
    let frame = dump(app, W, H);
    assert!(
        window_of(app).segments.len() > 1,
        "the first frame after {what} draws a full strip:\n{frame}"
    );
}

/// Wheel until the prepared window draws a strip below the anchor.
fn wheel_to_boundary(app: &mut App) {
    for _ in 0..100 {
        if window_of(app).segments.len() > 1 {
            return;
        }
        wheel_down(app);
    }
    panic!("the strip never came into view");
}

// --- Status ---------------------------------------------------------------

/// Two short untracked files: `b.txt` draws as a strip below the `a.txt`
/// anchor from the first prepared window on.
fn two_short_files() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "a.txt", "a one\na two\n");
    write(repo.path(), "b.txt", "b one\nb two\nb three\n");
    repo
}

#[test]
fn status_hide_and_reveal_keep_the_first_j_moving() {
    let repo = two_short_files();
    let mut app = app_for(&repo, config(true, false));
    let _ = dump(&app, W, H);
    press(&mut app, 'l');
    assert!(app.diff_focused(), "the diff pane has focus");
    assert_eq!(selected_path(&app), "a.txt", "a.txt anchors the stream");
    wheel_to_boundary(&mut app);
    let (strip, _) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the window, so the cursor lands"
    );
    assert!(
        app.cursor_divergent(),
        "the cursor names the file below the anchor"
    );

    press(&mut app, 'b');
    assert_first_frame_full(&app, "hiding");
    assert_eq!(
        app.cursor_address(),
        Some(strip),
        "the strip row is still in the wider window, so the cursor stays"
    );
    assert_first_j_moves(&mut app, "hiding");

    let _ = dump(&app, W, H);
    wheel_to_boundary(&mut app);
    let (strip, _) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the hidden-width window"
    );
    assert!(app.cursor_divergent(), "diverged again while hidden");
    press(&mut app, 'b');
    assert_first_frame_full(&app, "revealing");
    assert!(
        !app.cursor_divergent(),
        "focus leaves the diff pane on reveal, so the cursor drops"
    );
    assert_first_j_moves(&mut app, "revealing");
}

// --- Review ---------------------------------------------------------------

/// A rendered review with the diff focused and the cursor parked on a strip
/// row below the anchor, returned with that row's address.
fn diverged_review() -> (TempDir, App, CursorAddress) {
    let repo = init_repo_with_diverged_branches();
    let mut app = App::for_review(
        repo.path().to_path_buf(),
        &config(true, false),
        "main...feature",
    )
    .unwrap();
    let _ = dump(&app, W, H);
    app.on_key(tab());
    assert!(app.diff_focused(), "the diff pane has focus");
    assert!(app.review_files().len() >= 2, "a multi-file review");
    assert_eq!(
        app.review_selected(),
        0,
        "the first file anchors the stream"
    );
    wheel_to_boundary(&mut app);
    let (strip, _) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the window, so the cursor lands"
    );
    assert!(
        app.cursor_divergent(),
        "the cursor names the file below the anchor"
    );
    (repo, app, strip)
}

#[test]
fn review_hide_and_reveal_keep_the_first_j_moving() {
    let (_repo, mut app, strip) = diverged_review();

    press(&mut app, 'b');
    assert_first_frame_full(&app, "hiding");
    assert_eq!(
        app.cursor_address(),
        Some(strip),
        "the strip row is still in the wider window, so the cursor stays"
    );
    assert_first_j_moves(&mut app, "hiding");

    let _ = dump(&app, W, H);
    wheel_to_boundary(&mut app);
    let (strip, _) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the hidden-width window"
    );
    assert!(app.cursor_divergent(), "diverged again while hidden");
    press(&mut app, 'b');
    assert_first_frame_full(&app, "revealing");
    assert!(
        !app.cursor_divergent(),
        "focus leaves the diff pane on reveal, so the cursor drops"
    );
    assert_first_j_moves(&mut app, "revealing");
}

// --- History ---------------------------------------------------------------

#[test]
fn history_hide_and_reveal_keep_the_first_j_moving() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = rendered_app(&repo, config(true, false), H);
    press(&mut app, 'i');
    let _ = dump(&app, W, H);
    // Row 1 is the first file diff; row 0 is the commit details paragraph.
    history_select_row(&mut app, 1, W, H);
    app.on_key(tab());
    assert!(app.diff_focused(), "the diff pane has focus");
    wheel_to_boundary(&mut app);
    let (strip, needle) = strip_code_address(&app);
    assert_eq!(needle, "b.txt", "the first strip file of the fixture");
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the window, so the cursor lands"
    );
    assert!(
        app.cursor_divergent(),
        "the cursor names the file below the anchor"
    );

    press(&mut app, 'b');
    assert_first_frame_full(&app, "hiding");
    assert_eq!(
        app.cursor_address(),
        Some(strip),
        "the strip row is still in the wider window, so the cursor stays"
    );
    assert_first_j_moves(&mut app, "hiding");

    let _ = dump(&app, W, H);
    wheel_to_boundary(&mut app);
    let (strip, _) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the hidden-width window"
    );
    assert!(app.cursor_divergent(), "diverged again while hidden");
    press(&mut app, 'b');
    assert_first_frame_full(&app, "revealing");
    assert!(
        !app.cursor_divergent(),
        "focus leaves the diff pane on reveal, so the cursor drops"
    );
    assert_first_j_moves(&mut app, "revealing");
}

// --- Divider drag -----------------------------------------------------------

/// A drag re-keys the sections by width exactly like a toggle, but between two
/// mouse events rather than around a key: every dragged frame must still draw
/// a full strip, and the cursor — whose file is still in the window — survives.
#[test]
fn dragging_the_divider_keeps_the_strip_full_and_the_cursor_moving() {
    let (_repo, mut app, strip) = diverged_review();
    let diff = app.diff_area();
    let (x, y) = (diff.x - 1, diff.y + 2);
    let t = Instant::now();
    app.on_mouse_at(mouse(x, y, MouseEventKind::Down(MouseButton::Left)), t);
    app.on_mouse_at(mouse(x + 12, y, MouseEventKind::Drag(MouseButton::Left)), t);
    let narrower = app.diff_area().width;
    assert_first_frame_full(&app, "dragging");
    assert!(
        app.diff_area().width < narrower,
        "the drag narrowed the diff pane"
    );
    assert_eq!(
        app.cursor_address(),
        Some(strip),
        "the strip row is still in the narrower window, so the cursor stays"
    );
    app.on_mouse_at(mouse(x + 12, y, MouseEventKind::Up(MouseButton::Left)), t);
    assert_first_frame_full(&app, "releasing");
    assert_first_j_moves(&mut app, "dragging");
}
