mod common;

use common::{git, init_repo, write};
use strix::app::{App, Focus};
use strix::crossterm::event::{KeyCode, KeyEvent};
use strix::git::FileDiff;
use strix::terminal::dump_frame;

/// The current diff's text joined into one string, for content assertions.
fn diff_text(app: &App) -> String {
    match &app.current_diff {
        Some(FileDiff::Text(lines)) => lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[test]
fn reload_recomputes_the_open_diff_after_an_in_place_edit() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "file.txt", "original\n");
    git(path, &["add", "file.txt"]);
    git(path, &["commit", "-q", "-m", "add file"]);

    // One unstaged modification; it's the only change, so it's selected.
    write(path, "file.txt", "edited-one\n");
    let mut app = App::new(path.to_path_buf()).expect("app");
    assert!(
        diff_text(&app).contains("edited-one"),
        "diff shows the first edit"
    );

    // Edit again in place — same path, same section — so the diff key is
    // unchanged. Only forcing a recompute on reload picks this up.
    write(path, "file.txt", "edited-two\n");
    app.reload();
    let text = diff_text(&app);
    assert!(
        text.contains("edited-two"),
        "reload picks up the in-place edit"
    );
    assert!(!text.contains("edited-one"), "the stale diff is gone");
}

#[test]
fn refresh_keeps_the_cursor_on_the_same_file_when_the_list_shifts() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "m.txt", "1\n");
    let mut app = App::new(path.to_path_buf()).expect("app");
    assert_eq!(
        app.selected_file().map(|(_, e)| e.path.clone()).as_deref(),
        Some("m.txt"),
    );

    // A new untracked file appears ahead of it in the list; an index-based
    // cursor would now point at the wrong file.
    write(path, "a.txt", "1\n");
    app.reload();

    assert_eq!(
        app.selected_file().map(|(_, e)| e.path.clone()).as_deref(),
        Some("m.txt"),
        "the cursor follows the file by path, not by index"
    );
}

#[test]
fn reload_keeps_the_scroll_position_for_the_same_file() {
    let repo = init_repo();
    let path = repo.path();
    let lines: String = (0..40).map(|i| format!("line {i}\n")).collect();
    write(path, "big.txt", &lines);
    git(path, &["add", "big.txt"]);
    git(path, &["commit", "-q", "-m", "add big"]);
    // Modify every line so the diff is taller than the viewport.
    let edited: String = (0..40).map(|i| format!("line {i} edited\n")).collect();
    write(path, "big.txt", &edited);

    let mut app = App::new(path.to_path_buf()).expect("app");
    app.focus = Focus::Diff;
    // Render once to record the diff viewport, so scrolling can advance.
    let _ = dump_frame(&app, 80, 24).expect("dump_frame");
    for _ in 0..5 {
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
    }
    let scrolled = app.diff_scroll;
    assert!(scrolled > 0, "scrolled down into the diff");

    // An external in-place edit (as the watcher would trigger) reloads the same
    // file — the scroll must not jump back to the top.
    let edited_again: String = (0..40)
        .map(|i| format!("line {i} edited again\n"))
        .collect();
    write(path, "big.txt", &edited_again);
    app.reload();

    assert_eq!(
        app.diff_scroll, scrolled,
        "reloading the open file keeps the scroll position"
    );
}
