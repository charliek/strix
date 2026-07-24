//! Logical/physical row model (plan §3.0, C1). The diff cursor addresses a
//! logical `RowTarget`, not a physical row index; these tests pin the
//! behavior-preserving invariants that lets a later commit expand a target
//! across several physical rows: History round-trips, mode toggles, empty and
//! binary diffs, the hidden Changes pane, a wheel-scrolled-offscreen cursor, a
//! file change, and a width change all keep the cursor coherent.
//!
//! The harness mirrors the other review-view suites: build an `App` via
//! `App::for_review`, `dump_frame` to record geometry, drive `on_key`/`on_mouse`,
//! and assert on the exposed cursor state + rendered text grid.

mod common;

use std::path::Path;

use common::{git, init_repo, init_repo_with_diverged_branches, write};
use strix::app::{App, ViewMode};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use tempfile::TempDir;

const W: u16 = 100;
const H: u16 = 24;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn dump_at(app: &App, w: u16, h: u16) -> String {
    strix::terminal::dump_frame(app, w, h).unwrap()
}

fn dump(app: &App) -> String {
    dump_at(app, W, H)
}

fn review(repo: &Path, range: &str) -> App {
    App::for_review(repo.to_path_buf(), &Config::default(), range).unwrap()
}

fn mouse(col: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// Move the review selection until `path`'s diff is showing.
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

/// A repo whose `feature` branch adds a 40-line file (a single file in range,
/// so the diff has plenty of selectable code rows).
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

/// A repo whose `feature` branch adds a binary file (NUL bytes), so its range
/// diff is `FileDiff::Binary` with no selectable rows.
fn binary_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "blob.bin", "a\0b\0c\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add binary"]);
    dir
}

/// A repo whose `feature` branch renames a file with no content change, so the
/// file is listed in the range but its text diff is empty (no code rows).
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

// --- Mode toggle: the cursor resets and still moves in both modes ---

#[test]
fn mode_toggle_resets_the_cursor_and_moves_in_both_modes() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l')); // focus the diff pane
    app.on_key(key('j'));
    app.on_key(key('j'));
    assert_eq!(app.review_cursor(), 2, "j moves the unified cursor");

    app.on_key(key('d')); // toggle to side-by-side
    assert_eq!(
        app.review_cursor(),
        0,
        "a mode toggle resets the cursor to the top"
    );
    app.on_key(key('j'));
    assert_eq!(app.review_cursor(), 1, "j moves the side-by-side cursor");

    app.on_key(key('d')); // back to unified
    assert_eq!(app.review_cursor(), 0, "toggling back resets again");
}

// --- History round-trip preserves the review cursor's logical target ---

#[test]
fn history_round_trip_preserves_the_review_cursor() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l'));
    for _ in 0..3 {
        app.on_key(key('j'));
    }
    let before = app.review_cursor();
    assert_eq!(before, 3);

    app.on_key(key('i')); // enter history
    assert_eq!(app.view, ViewMode::History);
    let _ = dump(&app);
    app.on_key(key('i')); // back to review
    assert_eq!(app.view, ViewMode::Review);
    let _ = dump(&app);

    assert_eq!(
        app.review_cursor(),
        before,
        "the review cursor's target survives a history round-trip"
    );
}

// --- Empty diff: a listed file with no code rows has no selectable rows ---

#[test]
fn empty_diff_has_no_selectable_rows() {
    let repo = pure_rename_repo();
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "renamed.txt");
    app.on_key(key('l')); // focus the diff
    assert_eq!(app.review_cursor(), 0);
    app.on_key(key('j')); // nothing to move onto
    assert_eq!(app.review_cursor(), 0, "j is a no-op on an empty diff");
    app.on_key(key('G'));
    assert_eq!(app.review_cursor(), 0, "G is a no-op on an empty diff");
    let _ = dump(&app); // must not panic
}

// --- Binary diff: no selectable rows, renders the hint, doesn't panic ---

#[test]
fn binary_diff_has_no_selectable_rows() {
    let repo = binary_repo();
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "blob.bin");
    app.on_key(key('l'));
    assert_eq!(app.review_cursor(), 0);
    app.on_key(key('j'));
    assert_eq!(app.review_cursor(), 0, "j is a no-op on a binary diff");
    let frame = dump(&app);
    assert!(
        frame.contains("Binary file"),
        "the binary diff renders its hint:\n{frame}"
    );
}

// --- Hidden Changes pane: the cursor still moves ---

#[test]
fn cursor_moves_with_the_changes_pane_hidden() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('b')); // hide the file list; focus forced to the diff
    assert!(!app.show_changes);
    assert!(app.diff_focused());
    let _ = dump(&app);

    app.on_key(key('j'));
    app.on_key(key('j'));
    assert_eq!(
        app.review_cursor(),
        2,
        "j moves the cursor with the list hidden"
    );
}

// --- A wheel scroll pushes the cursor offscreen without moving its target ---

#[test]
fn a_wheel_scroll_leaves_the_cursor_target_then_a_move_reveals_it() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l'));
    app.on_key(key('j')); // cursor on physical row 1
    let cursor = app.review_cursor();
    assert_eq!(cursor, 1);

    // Wheel the viewport down until the cursor's row sits above it.
    let diff = app.diff_area();
    for _ in 0..12 {
        app.on_mouse(mouse(diff.x + 2, diff.y + 1, MouseEventKind::ScrollDown));
    }
    assert!(
        app.diff_scroll.get() > cursor,
        "the wheel pushed the cursor offscreen"
    );
    assert_eq!(
        app.review_cursor(),
        cursor,
        "the wheel scroll leaves the cursor target put"
    );

    // Moving the cursor reveals it (act-and-reveal): its row returns to view.
    app.on_key(key('j'));
    assert_eq!(app.review_cursor(), 2, "the move advanced the cursor");
    assert!(
        app.diff_scroll.get() <= app.review_cursor(),
        "the move scrolled the cursor's row back into view"
    );
}

// --- File change resets the cursor to the new file's top ---

#[test]
fn changing_the_selected_file_resets_the_cursor_to_the_top() {
    let repo = init_repo_with_diverged_branches();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('l')); // focus the diff
    app.on_key(key('j'));
    app.on_key(key('j'));
    assert!(app.review_cursor() > 0, "the cursor moved off the top");

    app.on_key(key('h')); // focus the file list
    app.on_key(key('j')); // select the next file
    assert_eq!(
        app.review_cursor(),
        0,
        "a new file starts at the top of its layout"
    );
}

// --- A width change preserves the logical cursor target ---

#[test]
fn a_width_change_preserves_the_logical_cursor_target() {
    let repo = tall_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump_at(&app, 100, 24);
    app.on_key(key('l'));
    for _ in 0..5 {
        app.on_key(key('j'));
    }
    let before = app.review_cursor();
    assert_eq!(before, 5);

    // Re-render at different widths: the layout rebuilds against the new width,
    // but the cursor's logical target (the same code line) is preserved, so its
    // projected physical row is unchanged.
    let _ = dump_at(&app, 60, 24);
    assert_eq!(
        app.review_cursor(),
        before,
        "the cursor holds its target across a narrower width"
    );
    let _ = dump_at(&app, 140, 30);
    assert_eq!(
        app.review_cursor(),
        before,
        "the cursor holds its target across a wider width"
    );
}
