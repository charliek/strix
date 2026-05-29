mod common;

use common::{git, init_repo, write};
use strix::app::{App, Focus};
use strix::crossterm::event::{KeyCode, KeyEvent};
use strix::terminal::dump_frame;

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
