use std::path::PathBuf;

use strix::app::App;
use strix::crossterm::event::{KeyCode, KeyEvent};
use strix::terminal::dump_frame;

fn rendered(width: u16, height: u16) -> String {
    let app = App::new(PathBuf::from("/tmp/strix-demo")).expect("app");
    dump_frame(&app, width, height).expect("dump_frame")
}

#[test]
fn renders_header_panels_and_footer() {
    let out = rendered(120, 40);
    assert!(out.contains("strix"), "header shows the app name");
    assert!(out.contains("strix-demo"), "header shows the repo name");
    assert!(out.contains("Changes"), "left panel title present");
    assert!(out.contains("Diff"), "right panel title present");
    assert!(out.contains("quit"), "footer shows key hints");
}

#[test]
fn quits_on_q() {
    let mut app = App::new(PathBuf::from(".")).expect("app");
    assert!(!app.should_quit);
    app.on_key(KeyEvent::from(KeyCode::Char('q')));
    assert!(app.should_quit);
}

#[test]
fn tab_toggles_focus() {
    use strix::app::Focus;
    let mut app = App::new(PathBuf::from(".")).expect("app");
    assert_eq!(app.focus, Focus::Staging);
    app.on_key(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Diff);
    app.on_key(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Staging);
}
