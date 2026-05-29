mod common;

use common::{git, init_repo, write};
use strix::app::{App, Focus};
use strix::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use strix::terminal::dump_frame;

fn mouse(col: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn click(col: u16, row: u16) -> MouseEvent {
    mouse(col, row, MouseEventKind::Down(MouseButton::Left))
}

/// A repo with one staged file (selection 0) and one untracked file (1).
fn app_with_two_files() -> (tempfile::TempDir, App) {
    let repo = init_repo();
    let path = repo.path();
    write(path, "staged.txt", "hi\n");
    git(path, &["add", "staged.txt"]);
    write(path, "untracked.txt", "yo\n");
    let app = App::new(path.to_path_buf()).unwrap();
    (repo, app)
}

/// Screen row of the file at selection `index`, using the rendered layout.
fn file_row(app: &App, index: usize) -> u16 {
    let area = app.staging_area();
    let offset = app.staging_state().offset();
    let item = strix::ui::staging::file_item_rows(&app.status)[index];
    area.y + (item - offset) as u16
}

#[test]
fn click_selects_a_file() {
    let (_repo, mut app) = app_with_two_files();
    dump_frame(&app, 120, 30).unwrap(); // populates pane rects + list offset
    let area = app.staging_area();
    let row = file_row(&app, 1);

    app.on_mouse(click(area.x + 8, row)); // past the marker zone → just select
    assert_eq!(app.selected, 1);
    assert_eq!(app.focus, Focus::Staging);
}

#[test]
fn click_marker_toggles_stage() {
    let (_repo, mut app) = app_with_two_files();
    dump_frame(&app, 120, 30).unwrap();
    let area = app.staging_area();
    let row = file_row(&app, 0); // staged.txt

    app.on_mouse(click(area.x + 1, row)); // marker zone → select + unstage
    assert!(app.status.staged.is_empty());
    assert!(app.status.unstaged.iter().any(|e| e.path == "staged.txt"));
}

#[test]
fn click_focuses_the_pane_under_the_cursor() {
    let (_repo, mut app) = app_with_two_files();
    dump_frame(&app, 120, 30).unwrap();

    let diff = app.diff_area();
    app.on_mouse(click(diff.x + 2, diff.y + 1));
    assert_eq!(app.focus, Focus::Diff);

    let staging = app.staging_area();
    app.on_mouse(click(staging.x + 8, staging.y + 1));
    assert_eq!(app.focus, Focus::Staging);
}

#[test]
fn wheel_over_staging_moves_selection() {
    let (_repo, mut app) = app_with_two_files();
    dump_frame(&app, 120, 30).unwrap();
    let area = app.staging_area();

    assert_eq!(app.selected, 0);
    app.on_mouse(mouse(area.x + 2, area.y + 1, MouseEventKind::ScrollDown));
    assert_eq!(app.selected, 1);
    app.on_mouse(mouse(area.x + 2, area.y + 1, MouseEventKind::ScrollUp));
    assert_eq!(app.selected, 0);
}
