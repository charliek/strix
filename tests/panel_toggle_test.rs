//! Plan 002 U1: hiding or revealing the Changes panel (`b`) must clear a
//! divergent diff cursor, and the first `j` after the toggle must move over a
//! full frame — one test per view (Status, Review, History).
//!
//! Each test parks a divergent cursor on a strip file, hides the panel, and
//! requires the clear plus a moving first `j`; it then re-diverges while
//! hidden and requires the same of the reveal. The `press` → `dump` → `j`
//! order is load-bearing: the dump records the retoggled width before the
//! first step reads it.

mod common;

use common::{
    app_for, config, dump, history_select_row, init_repo, init_repo_with_diverged_branches,
    init_repo_with_multi_file_commit, mouse, press, rendered_app, selected_path, strip_row, tab,
    window_of, write,
};
use strix::app::{App, CursorAddress, FileId, RowTarget};
use strix::crossterm::event::MouseEventKind;
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

fn assert_strip_drawn(app: &App, needle: &str, what: &str) -> String {
    let frame = dump(app, W, H);
    assert!(
        frame.contains(needle),
        "the first frame after {what} still draws the strip file:\n{frame}"
    );
    frame
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
    let (strip, needle) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the window, so the cursor lands"
    );
    assert!(
        app.cursor_divergent(),
        "the cursor names the file below the anchor"
    );

    press(&mut app, 'b');
    let _ = dump(&app, W, H);
    assert!(
        !app.cursor_divergent(),
        "hiding the panel clears the divergent cursor"
    );
    assert_first_j_moves(&mut app, "hiding");
    assert_strip_drawn(&app, needle.as_str(), "hiding");

    let _ = dump(&app, W, H);
    wheel_to_boundary(&mut app);
    let (strip, _) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the hidden-width window"
    );
    assert!(app.cursor_divergent(), "diverged again while hidden");
    press(&mut app, 'b');
    let _ = dump(&app, W, H);
    assert!(
        !app.cursor_divergent(),
        "revealing the panel clears the divergent cursor"
    );
    assert_first_j_moves(&mut app, "revealing");
    assert_strip_drawn(&app, needle.as_str(), "revealing");
}

// --- Review ---------------------------------------------------------------

#[test]
fn review_hide_and_reveal_keep_the_first_j_moving() {
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
    let (strip, needle) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the window, so the cursor lands"
    );
    assert!(
        app.cursor_divergent(),
        "the cursor names the file below the anchor"
    );

    press(&mut app, 'b');
    let _ = dump(&app, W, H);
    assert!(
        !app.cursor_divergent(),
        "hiding the panel clears the divergent cursor"
    );
    assert_first_j_moves(&mut app, "hiding");
    assert_strip_drawn(&app, needle.as_str(), "hiding");

    let _ = dump(&app, W, H);
    wheel_to_boundary(&mut app);
    let (strip, _) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the hidden-width window"
    );
    assert!(app.cursor_divergent(), "diverged again while hidden");
    press(&mut app, 'b');
    let _ = dump(&app, W, H);
    assert!(
        !app.cursor_divergent(),
        "revealing the panel clears the divergent cursor"
    );
    assert_first_j_moves(&mut app, "revealing");
    assert_strip_drawn(&app, needle.as_str(), "revealing");
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
    let _ = dump(&app, W, H);
    assert!(
        !app.cursor_divergent(),
        "hiding the panel clears the divergent cursor"
    );
    assert_first_j_moves(&mut app, "hiding");
    assert_strip_drawn(&app, needle.as_str(), "hiding");

    let _ = dump(&app, W, H);
    wheel_to_boundary(&mut app);
    let (strip, _) = strip_code_address(&app);
    assert!(
        app.place_cursor(strip.clone()),
        "the strip row is in the hidden-width window"
    );
    assert!(app.cursor_divergent(), "diverged again while hidden");
    press(&mut app, 'b');
    let _ = dump(&app, W, H);
    assert!(
        !app.cursor_divergent(),
        "revealing the panel clears the divergent cursor"
    );
    assert_first_j_moves(&mut app, "revealing");
    assert_strip_drawn(&app, needle.as_str(), "revealing");
}
