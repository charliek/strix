mod common;

use common::{init_repo, write};
use strix::app::App;
use strix::crossterm::event::{KeyCode, KeyEvent};
use strix::git::{Change, Repo};

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

#[test]
fn stage_then_unstage_via_repo() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nchange\n");

    let r = Repo::open(path).unwrap();
    r.stage("README.md").unwrap();
    let status = r.status().unwrap();
    assert!(status.staged.iter().any(|e| e.path == "README.md"));
    assert!(status.unstaged.is_empty());

    r.unstage("README.md").unwrap();
    let status = r.status().unwrap();
    assert!(status.staged.is_empty());
    assert!(status.unstaged.iter().any(|e| e.path == "README.md"));
}

#[test]
fn discard_modified_resets_to_head() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nDIRTY\n");

    let r = Repo::open(path).unwrap();
    r.discard("README.md", Change::Modified).unwrap();

    assert_eq!(
        std::fs::read_to_string(path.join("README.md")).unwrap(),
        "# test\n"
    );
    assert!(r.status().unwrap().is_clean());
}

#[test]
fn discard_untracked_deletes_the_file() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "junk.txt", "x\n");

    let r = Repo::open(path).unwrap();
    r.discard("junk.txt", Change::Untracked).unwrap();

    assert!(!path.join("junk.txt").exists());
    assert!(r.status().unwrap().is_clean());
}

#[test]
fn space_toggles_stage_through_app() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "new.txt", "hi\n");

    let mut app = App::new(path.to_path_buf()).unwrap();
    assert_eq!(app.status.total(), 1);

    app.on_key(key(' ')); // stage the untracked file
    assert!(app.status.staged.iter().any(|e| e.path == "new.txt"));
    assert!(app.status.unstaged.is_empty());

    app.on_key(key(' ')); // unstage it again
    assert!(app.status.staged.is_empty());
    assert!(app.status.unstaged.iter().any(|e| e.path == "new.txt"));
}

#[test]
fn discard_requires_confirmation_then_resets() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nDIRTY\n");

    let mut app = App::new(path.to_path_buf()).unwrap();

    app.on_key(key('x')); // request discard -> modal appears
    assert!(app.modal.is_some());
    assert!(!app.status.is_clean(), "nothing changed yet");

    app.on_key(key('y')); // confirm
    assert!(app.modal.is_none());
    assert!(app.status.is_clean());
}

#[test]
fn discard_can_be_cancelled() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nDIRTY\n");

    let mut app = App::new(path.to_path_buf()).unwrap();
    app.on_key(key('x'));
    assert!(app.modal.is_some());
    app.on_key(KeyEvent::from(KeyCode::Esc)); // cancel
    assert!(app.modal.is_none());
    assert!(!app.status.is_clean(), "changes remain after cancel");
}

#[test]
fn discard_modal_renders_as_popup() {
    let repo = init_repo();
    write(repo.path(), "README.md", "# test\nDIRTY\n");

    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.on_key(key('x'));

    let out = strix::terminal::dump_frame(&app, 100, 24).unwrap();
    assert!(out.contains("Discard"), "popup title");
    assert!(out.contains("README.md"), "popup names the file");
    assert!(out.contains("confirm"), "popup shows the confirm hint");
}
