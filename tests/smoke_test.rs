mod common;

use common::{git, init_repo, write};
use strix::app::{App, Focus};
use strix::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use strix::terminal::dump_frame;

/// A left-button mouse event at `(x, y)` of the given kind.
fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

fn app_with_changes() -> (tempfile::TempDir, App) {
    let repo = init_repo();
    let path = repo.path();
    write(path, "staged.txt", "hi\n");
    git(path, &["add", "staged.txt"]);
    write(path, "untracked.txt", "yo\n");
    let app = App::new(path.to_path_buf()).expect("app");
    (repo, app)
}

#[test]
fn renders_status_against_repo() {
    let (_repo, app) = app_with_changes();
    let out = dump_frame(&app, 100, 30).expect("dump_frame");
    assert!(out.contains("strix"), "header shows the app name");
    assert!(out.contains("main"), "header shows the branch");
    assert!(out.contains("Staged"), "staged section header");
    assert!(out.contains("staged.txt"), "staged file listed");
    assert!(out.contains("untracked.txt"), "untracked file listed");
    assert!(out.contains("quit"), "footer shows key hints");
}

#[test]
fn clean_repo_shows_empty_state() {
    let repo = init_repo();
    let app = App::new(repo.path().to_path_buf()).expect("app");
    let out = dump_frame(&app, 100, 20).expect("dump_frame");
    assert!(out.contains("working tree clean"));
}

#[test]
fn quits_on_q() {
    let (_repo, mut app) = app_with_changes();
    assert!(!app.should_quit);
    app.on_key(KeyEvent::from(KeyCode::Char('q')));
    assert!(app.should_quit);
}

#[test]
fn tab_toggles_focus() {
    let (_repo, mut app) = app_with_changes();
    assert_eq!(app.focus, Focus::Staging);
    app.on_key(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Diff);
    app.on_key(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Staging);
}

#[test]
fn jk_moves_selection() {
    let (_repo, mut app) = app_with_changes();
    assert_eq!(app.selected, 0);
    app.on_key(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(app.selected, 1);
    app.on_key(KeyEvent::from(KeyCode::Char('k')));
    assert_eq!(app.selected, 0);
    // Can't move above the first entry.
    app.on_key(KeyEvent::from(KeyCode::Char('k')));
    assert_eq!(app.selected, 0);
}

#[test]
fn b_toggles_changes_panel_and_focus() {
    let (_repo, mut app) = app_with_changes();
    assert!(app.show_changes, "Changes panel visible by default");
    assert_eq!(app.focus, Focus::Staging);

    // Hide: focus is forced to the diff (the only visible pane).
    app.on_key(KeyEvent::from(KeyCode::Char('b')));
    assert!(!app.show_changes);
    assert_eq!(app.focus, Focus::Diff);

    // Show again: re-showing focuses the Changes panel.
    app.on_key(KeyEvent::from(KeyCode::Char('b')));
    assert!(app.show_changes);
    assert_eq!(app.focus, Focus::Staging);
}

#[test]
fn tab_reveals_a_hidden_changes_panel() {
    let (_repo, mut app) = app_with_changes();
    app.on_key(KeyEvent::from(KeyCode::Char('b'))); // hide
    assert!(!app.show_changes);

    app.on_key(KeyEvent::from(KeyCode::Tab));
    assert!(app.show_changes, "Tab reveals the hidden panel");
    assert_eq!(app.focus, Focus::Staging, "and lands in it");
}

#[test]
fn focusing_staging_reveals_a_hidden_changes_panel() {
    let (_repo, mut app) = app_with_changes();
    app.on_key(KeyEvent::from(KeyCode::Char('b'))); // hide
    assert!(!app.show_changes);

    app.on_key(KeyEvent::from(KeyCode::Char('h'))); // FocusStaging
    assert!(
        app.show_changes,
        "focusing staging reveals the hidden panel"
    );
    assert_eq!(app.focus, Focus::Staging);
}

#[test]
fn b_hides_changes_so_diff_fills_the_width() {
    let (_repo, mut app) = app_with_changes();

    // Shown by default: the diff title (`pending · HEAD→worktree`) sits to the
    // right of the Changes panel.
    let shown = dump_frame(&app, 100, 30).expect("dump_frame");
    assert!(shown.contains("Changes"), "Changes panel shown by default");
    let shown_top = shown.lines().nth(1).expect("body top border");
    let shown_diff_col = shown_top.find("pending").expect("diff title shown");
    assert!(
        shown_diff_col > 20,
        "Diff title starts past the Changes panel when shown (col {shown_diff_col})"
    );

    // After `b`: Changes gone, and the Diff border opens at column 0 and runs to
    // the last column — it owns the whole body width.
    app.on_key(KeyEvent::from(KeyCode::Char('b')));
    let hidden = dump_frame(&app, 100, 30).expect("dump_frame");
    assert!(!hidden.contains("Changes"), "Changes panel hidden after b");
    let hidden_top = hidden.lines().nth(1).expect("body top border");
    assert!(
        hidden_top.starts_with('╭') && hidden_top.ends_with('╮'),
        "Diff border spans the full width: {hidden_top:?}"
    );
    let hidden_diff_col = hidden_top.find("pending").expect("diff title hidden");
    assert!(
        hidden_diff_col < 8,
        "Diff title now sits at the left edge (col {hidden_diff_col})"
    );
}

#[test]
fn dragging_the_split_bar_resizes_the_changes_panel() {
    let (_repo, mut app) = app_with_changes();
    // Render once so the split geometry (body + divider column) is recorded.
    let _ = dump_frame(&app, 100, 30).expect("dump_frame");
    let divider = app.diff_area().x - 1; // the diff's left border = the split bar

    // Grab the bar and drag it right: the Changes panel widens to the cursor.
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider, 5));
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 5));
    assert_eq!(app.changes_width, 50);

    // The next frame reflects it: the divider follows the drag.
    let _ = dump_frame(&app, 100, 30).expect("dump_frame");
    assert_eq!(app.diff_area().x - 1, 50, "divider follows the drag");
}

#[test]
fn split_bar_drag_clamps_to_usable_widths() {
    let (_repo, mut app) = app_with_changes();
    let _ = dump_frame(&app, 100, 30).expect("dump_frame");
    let divider = app.diff_area().x - 1;

    // Drag far left: clamped so the Changes panel keeps its minimum (16).
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider, 5));
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 1, 5));
    assert_eq!(app.changes_width, 16);

    // Drag far right: clamped so the diff keeps its minimum (100 - 24 = 76).
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 99, 5));
    assert_eq!(app.changes_width, 76);
}

#[test]
fn split_bar_stops_resizing_after_mouse_up() {
    let (_repo, mut app) = app_with_changes();
    let _ = dump_frame(&app, 100, 30).expect("dump_frame");
    let divider = app.diff_area().x - 1;
    let start = app.changes_width;

    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider, 5));
    app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), divider, 5));
    // A drag after release is not a resize — the bar was let go.
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 60, 5));
    assert_eq!(app.changes_width, start);
}

#[test]
fn grabbing_the_split_bar_leaves_selection_and_focus_untouched() {
    let (_repo, mut app) = app_with_changes();
    let _ = dump_frame(&app, 100, 30).expect("dump_frame");
    let divider = app.diff_area().x - 1;

    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider, 5));
    assert_eq!(
        app.selected, 0,
        "grabbing the bar doesn't move the selection"
    );
    assert_eq!(
        app.focus,
        Focus::Staging,
        "grabbing the bar doesn't refocus"
    );
}

#[test]
fn hovering_the_split_bar_engages_the_resize_affordance() {
    let (_repo, mut app) = app_with_changes();
    let _ = dump_frame(&app, 100, 30).expect("dump_frame");
    let divider = app.diff_area().x - 1;

    assert!(!app.divider_engaged(), "idle: not engaged");

    // Hover onto the bar: engages, and asks for a redraw.
    let redraw = app.on_mouse(mouse(MouseEventKind::Moved, divider, 5));
    assert!(app.divider_engaged(), "hover engages the affordance");
    assert!(redraw, "the state change asks for a redraw");

    // Moving while still on the bar changes nothing visible — no redraw.
    let redraw = app.on_mouse(mouse(MouseEventKind::Moved, divider, 6));
    assert!(app.divider_engaged());
    assert!(!redraw, "no redraw while still hovering the bar");

    // Move off the bar: disengages, and asks for a redraw.
    let redraw = app.on_mouse(mouse(MouseEventKind::Moved, divider + 10, 5));
    assert!(!app.divider_engaged(), "moving away disengages");
    assert!(redraw, "leaving the bar asks for a redraw");
}

#[test]
fn dragging_keeps_the_divider_engaged_off_the_bar() {
    let (_repo, mut app) = app_with_changes();
    let _ = dump_frame(&app, 100, 30).expect("dump_frame");
    let divider = app.diff_area().x - 1;

    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider, 5));
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 70, 5));
    assert!(
        app.divider_engaged(),
        "still engaged mid-drag, cursor off the bar"
    );

    app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 70, 5));
    assert!(!app.divider_engaged(), "releasing ends the engagement");
}
