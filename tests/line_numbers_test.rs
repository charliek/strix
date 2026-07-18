//! Line-number gutter toggle (`n` / `Action::ToggleLineNumbers`, plan §3.4,
//! C3 test list): gutter presence in unified + side-by-side, all three views,
//! the `line_numbers = false` startup default, modal precedence, remapping,
//! and narrow-terminal safety.

mod common;

use std::collections::HashMap;

use common::{init_repo, init_repo_with_diverged_branches, init_repo_with_history, write};
use strix::app::{App, DiffMode};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use strix::keys::{Action, Keymap};
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 30;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn dump(app: &App) -> String {
    strix::terminal::dump_frame(app, W, H).unwrap()
}

/// The column (char index within its row) of the first line containing
/// `needle`, for measuring how far the gutter pushes content over.
fn col_of(out: &str, needle: &str) -> usize {
    for line in out.lines() {
        if let Some(idx) = line.find(needle) {
            return idx;
        }
    }
    panic!("{needle:?} not found in:\n{out}");
}

// --- Config default ---

#[test]
fn config_line_numbers_defaults_true_and_is_overridable() {
    assert!(Config::default().line_numbers());
    let config = Config {
        line_numbers: Some(false),
        ..Config::default()
    };
    assert!(!config.line_numbers());
}

#[test]
fn line_numbers_false_in_config_starts_hidden() {
    let repo = init_repo();
    let config = Config {
        line_numbers: Some(false),
        ..Config::default()
    };
    let app = App::with_config(repo.path().to_path_buf(), &config).unwrap();
    assert!(!app.show_line_numbers);
}

#[test]
fn default_config_starts_with_numbers_shown() {
    let repo = init_repo();
    let app = App::new(repo.path().to_path_buf()).unwrap();
    assert!(app.show_line_numbers);
}

// --- Unified / side-by-side gutter width (Status view) ---

#[test]
fn unified_numbers_on_shows_gutter_off_hides_it() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nADDED_LINE\n");

    let mut app = App::new(path.to_path_buf()).unwrap();
    assert!(app.show_line_numbers, "on by default");
    let on = dump(&app);
    let col_on = col_of(&on, "ADDED_LINE");

    app.on_key(key('n'));
    assert!(!app.show_line_numbers);
    let off = dump(&app);
    let col_off = col_of(&off, "ADDED_LINE");

    assert!(
        off.contains("+ ADDED_LINE"),
        "sign column stays when numbers are hidden:\n{off}"
    );
    assert_eq!(
        col_on - col_off,
        10,
        "hiding numbers frees exactly the 10-char unified gutter (no drift \
         between gutter emission and content width)\non:\n{on}\noff:\n{off}"
    );
}

#[test]
fn unified_hidden_gutter_width_goes_to_content() {
    let repo = init_repo();
    let path = repo.path();
    // A line longer than any terminal width: the visible prefix length IS the
    // content width, so freeing the gutter must surface exactly 10 more chars.
    write(path, "README.md", &format!("# test\n{}\n", "X".repeat(300)));

    let mut app = App::new(path.to_path_buf()).unwrap();
    let on = dump(&app);
    app.on_key(key('n'));
    let off = dump(&app);

    let xs = |frame: &str| {
        frame
            .lines()
            .map(|l| l.chars().filter(|&c| c == 'X').count())
            .max()
            .unwrap_or(0)
    };
    assert_eq!(
        xs(&off) - xs(&on),
        10,
        "the freed unified gutter becomes visible content\non:\n{on}\noff:\n{off}"
    );
}

#[test]
fn side_by_side_numbers_on_shows_gutter_off_hides_it() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nADDED_LINE\n");

    let mut app = App::new(path.to_path_buf()).unwrap();
    app.on_key(key('d'));
    assert_eq!(app.diff_mode, DiffMode::SideBySide);

    let on = dump(&app);
    let col_on = col_of(&on, "ADDED_LINE");

    app.on_key(key('n'));
    let off = dump(&app);
    let col_off = col_of(&off, "ADDED_LINE");

    assert_eq!(
        col_on - col_off,
        5,
        "hiding numbers frees exactly the 5-char per-column SBS gutter\non:\n{on}\noff:\n{off}"
    );
}

#[test]
fn sbs_hidden_gutter_width_goes_to_content() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", &format!("# test\n{}\n", "X".repeat(300)));

    let mut app = App::new(path.to_path_buf()).unwrap();
    app.on_key(key('d'));
    let on = dump(&app);
    app.on_key(key('n'));
    let off = dump(&app);

    // The addition renders only in the new (right) column, so the max per-row
    // X count is that column's content width; freeing its gutter adds 5.
    let xs = |frame: &str| {
        frame
            .lines()
            .map(|l| l.chars().filter(|&c| c == 'X').count())
            .max()
            .unwrap_or(0)
    };
    assert_eq!(
        xs(&off) - xs(&on),
        5,
        "the freed SBS gutter becomes visible content\non:\n{on}\noff:\n{off}"
    );
}

// --- Narrow terminal ---

#[test]
fn narrow_terminal_does_not_panic_or_underflow() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nADDED_LINE\n");

    let mut app = App::new(path.to_path_buf()).unwrap();
    // Hide the Changes panel so the diff pane gets the (narrow) full width.
    app.on_key(key('b'));

    for numbers in [true, false] {
        if app.show_line_numbers != numbers {
            app.on_key(key('n'));
        }
        let _ = strix::terminal::dump_frame(&app, 20, 10).unwrap();
        app.on_key(key('d')); // side-by-side under the same narrow width
        let _ = strix::terminal::dump_frame(&app, 20, 10).unwrap();
        app.on_key(key('d')); // back to unified
        let _ = strix::terminal::dump_frame(&app, 5, 5).unwrap();
    }
}

// --- All three views ---

#[test]
fn works_in_status_view() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nADDED_LINE\n");
    let mut app = App::new(path.to_path_buf()).unwrap();

    let col_on = col_of(&dump(&app), "ADDED_LINE");
    app.on_key(key('n'));
    let col_off = col_of(&dump(&app), "ADDED_LINE");
    assert_eq!(col_on - col_off, 10);
}

#[test]
fn works_in_history_view() {
    let repo = init_repo_with_history();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.on_key(key('i')); // enter history
    let _ = dump(&app); // record geometry
    app.on_key(KeyEvent::from(KeyCode::Tab)); // graph -> committed changes
    app.on_key(key('j')); // step onto the newest commit's changed file

    // The newest commit ("edit readme") adds this line.
    let col_on = col_of(&dump(&app), "second line");
    app.on_key(key('n'));
    let col_off = col_of(&dump(&app), "second line");
    assert_eq!(col_on - col_off, 10, "history view respects the toggle");
}

fn review_app(range: &str) -> (TempDir, App) {
    let repo = init_repo_with_diverged_branches();
    let app = App::for_review(repo.path().to_path_buf(), &Config::default(), range).unwrap();
    (repo, app)
}

#[test]
fn works_in_review_view() {
    let (_repo, mut app) = review_app("main");
    let _ = dump(&app); // record list geometry

    // Step through the list until the renamed file (a rename+modify whose
    // added "delta" line is unique across the whole frame — unlike
    // "feature", it can't collide with a filename in the list column) is
    // selected.
    let mut guard = 0;
    while app.active_diff_path().as_deref() != Some("renamed.txt") {
        app.on_key(key('j'));
        guard += 1;
        assert!(guard <= app.review_files().len(), "renamed.txt not listed");
    }

    let col_on = col_of(&dump(&app), "delta");
    app.on_key(key('n'));
    let col_off = col_of(&dump(&app), "delta");
    assert_eq!(col_on - col_off, 10, "review view respects the toggle");
}

// --- Modal precedence ---

#[test]
fn n_dismisses_confirm_discard_without_toggling_then_toggles_normally() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nDIRTY\n");
    let mut app = App::new(path.to_path_buf()).unwrap();
    assert!(app.show_line_numbers);

    app.on_key(key('x')); // open ConfirmDiscard
    assert!(app.modal.is_some());

    app.on_key(key('n')); // modal consumes 'n' first: "no"
    assert!(app.modal.is_none(), "the modal is dismissed");
    assert!(
        app.show_line_numbers,
        "line numbers must be unchanged while a modal captured the key"
    );

    app.on_key(key('n')); // no modal now: 'n' toggles
    assert!(!app.show_line_numbers);
}

// --- Remappable via [keys] ---

#[test]
fn toggle_line_numbers_is_remappable() {
    let mut overrides = HashMap::new();
    overrides.insert(
        "toggle-line-numbers".to_string(),
        vec!["ctrl-l".to_string()],
    );
    let keymap = Keymap::from_config(Some(&overrides));

    assert_eq!(
        keymap.action(ctrl('l')),
        Some(Action::ToggleLineNumbers),
        "new chord bound"
    );
    assert_eq!(
        keymap.action(key('n')),
        None,
        "default 'n' was replaced by the override"
    );
    assert_eq!(
        keymap.action(key('j')),
        Some(Action::Down),
        "unlisted actions keep their defaults"
    );
}
