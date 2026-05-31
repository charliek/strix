mod common;

use common::init_repo_with_history;
use strix::app::{App, HistoryFocus, ViewMode};
use strix::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use strix::git::{FileDiff, LineKind};
use tempfile::TempDir;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn esc() -> KeyEvent {
    KeyEvent::from(KeyCode::Esc)
}

fn tab() -> KeyEvent {
    KeyEvent::from(KeyCode::Tab)
}

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: strix::crossterm::event::KeyModifiers::empty(),
    }
}

const W: u16 = 100;
const H: u16 = 30;

fn dump(app: &App) -> String {
    strix::terminal::dump_frame(app, W, H).unwrap()
}

/// Build an `App` against a 3-commit repo. The returned `TempDir` must be held
/// for the `App`'s lifetime — dropping it deletes the repo and the commit walk
/// would then find nothing (mirrors the pattern in `mouse_test.rs`).
fn history_app() -> (TempDir, App) {
    let repo = init_repo_with_history();
    let app = App::new(repo.path().to_path_buf()).unwrap();
    (repo, app)
}

#[test]
fn y_enters_history_view() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    assert_eq!(app.view, ViewMode::History);
    let out = dump(&app);
    assert!(out.contains("Committed Changes"), "frame:\n{out}");
    assert!(out.contains("Graph"), "frame:\n{out}");
}

#[test]
fn esc_exits_to_status() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    assert_eq!(app.view, ViewMode::History);
    app.on_key(esc());
    assert_eq!(app.view, ViewMode::Status);
    let out = dump(&app);
    assert!(out.contains("Changes"), "status frame:\n{out}");
}

#[test]
fn number_keys_switch_views() {
    let (_repo, mut app) = history_app();
    app.on_key(key('2'));
    assert_eq!(app.view, ViewMode::History);
    app.on_key(key('1'));
    assert_eq!(app.view, ViewMode::Status);
}

#[test]
fn commit_row_shows_details() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    // Defaults to the commit row, so the right pane shows details.
    assert!(app.history_shows_details());
    let out = dump(&app);
    assert!(out.contains("Author"), "details frame:\n{out}");
    assert!(
        out.contains("file"),
        "details should summarise files:\n{out}"
    );
}

#[test]
fn selecting_a_file_shows_its_diff() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    // Render once so the graph/committed geometry is recorded.
    let _ = dump(&app);
    // Move focus to the committed-changes list and step onto the first file.
    app.on_key(tab());
    assert_eq!(app.history_focus(), HistoryFocus::CommittedChanges);
    app.on_key(key('j'));
    assert_eq!(app.committed_row(), 1);
    assert!(!app.history_shows_details());
    // The newest commit ("edit readme") adds this line to README.md; assert on
    // the diff model (robust) plus that the pane renders it.
    let diff = app.active_diff().expect("a file diff is active");
    let FileDiff::Text(lines) = diff else {
        panic!("expected a text diff, got {diff:?}");
    };
    assert!(
        lines
            .iter()
            .any(|l| l.kind == LineKind::Addition && l.text.contains("second line")),
        "diff should add the new README line: {lines:?}"
    );
}

#[test]
fn selecting_a_graph_commit_defaults_to_details() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    let _ = dump(&app);
    // Graph is focused by default; move down to an older commit.
    app.on_key(key('j'));
    assert_eq!(app.selected_commit(), 1);
    assert_eq!(
        app.committed_row(),
        0,
        "new commit defaults to its detail row"
    );
    assert!(app.history_shows_details());
}

#[test]
fn clicking_a_graph_row_selects_that_commit() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    let _ = dump(&app);
    // The graph pane sits below the committed pane (top height 12) inside the
    // body (which starts at row 1): its first row is around y = 1 + 12 + 1.
    let y = 1 + app.committed_height() + 1;
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, y));
    assert_eq!(app.history_focus(), HistoryFocus::Graph);
    assert_eq!(app.selected_commit(), 0);
}

#[test]
fn dragging_horizontal_divider_resizes_committed_pane() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    let _ = dump(&app);
    assert_eq!(app.committed_height(), 12, "default height");

    // The divider sits at the committed pane's bottom border, body.y + height.
    let dy = 1 + app.committed_height();
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, dy));
    // Drag it up to shrink the committed pane.
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 4, 8));
    app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4, 8));
    assert!(
        app.committed_height() < 12,
        "committed pane should shrink, got {}",
        app.committed_height()
    );
}

#[test]
fn horizontal_divider_drag_clamps_to_minimum() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    let _ = dump(&app);

    let dy = 1 + app.committed_height();
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, dy));
    // Drag far above the top: the height clamps to the minimum, never zero.
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 4, 0));
    app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4, 0));
    assert!(app.committed_height() >= 4, "clamped to a usable minimum");
}

#[test]
fn b_collapses_and_restores_the_history_left_panel() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    let before = dump(&app);
    assert!(
        before.contains("Committed Changes"),
        "panels shown:\n{before}"
    );
    assert!(before.contains("Graph"));

    // `b` hides the left column; focus moves to the diff (the only pane left).
    app.on_key(key('b'));
    assert!(!app.show_changes, "show_changes flipped off");
    assert_eq!(app.history_focus(), HistoryFocus::Diff);
    let hidden = dump(&app);
    assert!(!hidden.contains("Committed Changes"), "hidden:\n{hidden}");
    assert!(!hidden.contains("Graph"), "hidden:\n{hidden}");
    // The commit details (the right pane when the commit row is selected)
    // still render — just full-width now.
    assert!(
        hidden.contains("Author"),
        "details still rendered:\n{hidden}"
    );

    // `b` again reveals the panel and lands focus back in the Graph.
    app.on_key(key('b'));
    assert!(app.show_changes);
    assert_eq!(app.history_focus(), HistoryFocus::Graph);
    let restored = dump(&app);
    assert!(restored.contains("Committed Changes"));
    assert!(restored.contains("Graph"));
}

#[test]
fn empty_repo_history_renders_without_panic() {
    let dir = common::init_empty_repo();
    let mut app = App::new(dir.path().to_path_buf()).unwrap();
    app.on_key(key('i'));
    assert_eq!(app.view, ViewMode::History);
    let out = dump(&app);
    assert!(out.contains("No commits"), "empty-state frame:\n{out}");
}

#[test]
fn exiting_history_clears_horizontal_divider_state() {
    let (_repo, mut app) = history_app();
    app.on_key(key('i'));
    let _ = dump(&app);

    // Grab the hdivider, exit history without releasing.
    let dy = 1 + app.committed_height();
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, dy));
    app.on_key(esc());
    assert_eq!(app.view, ViewMode::Status);
    assert!(
        !app.divider_engaged(),
        "stale hdivider state survived exit_history"
    );
}

#[test]
fn exit_history_with_hidden_changes_focuses_diff() {
    let (_repo, mut app) = history_app();
    // Hide the Changes panel in status view first.
    app.on_key(key('b'));
    assert!(!app.show_changes);
    // Enter history, then Esc back to status.
    app.on_key(key('i'));
    app.on_key(esc());
    assert_eq!(app.view, ViewMode::Status);
    assert!(!app.show_changes, "Changes panel stays hidden");
    // Hidden-panel invariant: focus must be the only visible pane.
    assert_eq!(app.focus, strix::app::Focus::Diff);
}
