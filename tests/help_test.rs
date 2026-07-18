mod common;

use common::init_repo;
use strix::app::{App, Flash, Modal};
use strix::crossterm::event::{KeyCode, KeyEvent};
use strix::terminal::dump_frame;

#[test]
fn question_mark_opens_and_any_key_closes_help() {
    let repo = init_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();

    app.on_key(KeyEvent::from(KeyCode::Char('?')));
    assert!(matches!(app.modal, Some(Modal::Help)));

    let out = dump_frame(&app, 100, 30).unwrap();
    assert!(out.contains("Help"), "help title");
    assert!(out.contains("stage / unstage"), "help lists staging keys");
    assert!(out.contains("side-by-side"), "help lists the view toggle");

    app.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(app.modal.is_none(), "any key dismisses help");
}

#[test]
fn error_renders_in_the_footer() {
    let repo = init_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.flash = Some(Flash::error("boom"));

    let out = dump_frame(&app, 100, 20).unwrap();
    assert!(out.contains("✗ boom"), "error toast shown in footer");
}

#[test]
fn the_next_keypress_clears_the_error() {
    let repo = init_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.flash = Some(Flash::error("boom"));

    app.on_key(KeyEvent::from(KeyCode::Char('j')));
    assert!(app.flash.is_none());
}
