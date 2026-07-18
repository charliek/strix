//! Review view (`ViewMode::Review`): layout/selection rendering, the empty-range
//! state, view transitions, staging inertness, and the view-aware reload path
//! with its OID churn guard (plan §3.3, C2 test list).

mod common;

use std::process::Command;

use common::{git, init_repo, init_repo_with_diverged_branches, write};
use strix::app::{App, Focus, ViewMode};
use strix::config::Config;
use strix::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use strix::git::FileDiff;
use tempfile::TempDir;

const W: u16 = 100;
const H: u16 = 24;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn esc() -> KeyEvent {
    KeyEvent::from(KeyCode::Esc)
}

fn tab() -> KeyEvent {
    KeyEvent::from(KeyCode::Tab)
}

fn space() -> KeyEvent {
    KeyEvent::from(KeyCode::Char(' '))
}

fn enter() -> KeyEvent {
    KeyEvent::from(KeyCode::Enter)
}

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

fn dump(app: &App) -> String {
    strix::terminal::dump_frame(app, W, H).unwrap()
}

/// A review session over `main…HEAD` on the diverged fixture (HEAD = feature).
/// Hold the `TempDir` for the `App`'s lifetime (dropping it deletes the repo).
fn review_app(range: &str) -> (TempDir, App) {
    let repo = init_repo_with_diverged_branches();
    let app = App::for_review(repo.path().to_path_buf(), &Config::default(), range).unwrap();
    (repo, app)
}

// --- Rendering ---

#[test]
fn review_session_starts_in_review_view() {
    let (_repo, app) = review_app("main");
    assert_eq!(app.view, ViewMode::Review);
    let out = dump(&app);
    // Header shows `repo · <range>`, and no status branch label.
    assert!(out.contains("main…HEAD"), "header shows the range:\n{out}");
    assert!(
        out.contains("feature.txt"),
        "the review list shows a changed file:\n{out}"
    );
}

#[test]
fn review_header_suppresses_the_branch_label() {
    let (_repo, app) = review_app("main");
    let out = dump(&app);
    // HEAD is the `feature` branch; a review header must not print it (the range
    // label already identifies HEAD, and a branch label would mislead).
    let header = out.lines().next().unwrap_or("");
    assert!(
        !header.contains("feature "),
        "header should not carry the status branch label: {header:?}"
    );
}

#[test]
fn review_list_shows_stats_and_first_file_diff() {
    let (_repo, app) = review_app("main");
    // The first file is selected, so its diff renders.
    assert!(app.review_list_focused());
    let path = app.active_diff_path().expect("a file is selected");
    let out = dump(&app);
    assert!(
        out.contains(&format!("Diff · {path}")),
        "diff pane titles the selected file:\n{out}"
    );
    // Stats column: at least one `+` count in the list.
    assert!(out.contains("+1"), "list shows +/- stats:\n{out}");
}

#[test]
fn empty_range_shows_no_differences() {
    // Same-tip range: base == head, so no files differ.
    let (_repo, app) = review_app("HEAD..HEAD");
    assert!(app.review_files().is_empty(), "no files in an empty range");
    let out = dump(&app);
    assert!(
        out.contains("No differences"),
        "empty range shows the no-differences state:\n{out}"
    );
    // With nothing selected, the diff pane shows its own empty hint.
    assert!(app.active_diff().is_none());
}

// --- Navigation / focus ---

#[test]
fn j_k_move_the_selection() {
    let (_repo, mut app) = review_app("main");
    let _ = dump(&app);
    assert_eq!(app.review_selected(), 0);
    app.on_key(key('j'));
    assert_eq!(app.review_selected(), 1);
    app.on_key(key('k'));
    assert_eq!(app.review_selected(), 0);
}

#[test]
fn tab_switches_between_list_and_diff() {
    let (_repo, mut app) = review_app("main");
    let _ = dump(&app);
    assert!(app.review_list_focused());
    app.on_key(tab());
    assert!(app.diff_focused());
    app.on_key(tab());
    assert!(app.review_list_focused());
}

#[test]
fn selecting_a_file_updates_the_diff() {
    let (_repo, mut app) = review_app("main");
    let _ = dump(&app);
    let first = app.active_diff_path().unwrap();
    app.on_key(key('j'));
    let second = app.active_diff_path().unwrap();
    assert_ne!(first, second, "selection change picks a different file");
}

#[test]
fn b_collapses_and_restores_the_list() {
    let (_repo, mut app) = review_app("main");
    let before = dump(&app);
    assert!(before.contains("feature.txt"));
    app.on_key(key('b'));
    assert!(!app.show_changes);
    assert!(
        app.diff_focused(),
        "hiding the list forces focus to the diff"
    );
    let hidden = dump(&app);
    assert!(!hidden.contains(" Changes "), "list hidden:\n{hidden}");
    app.on_key(key('b'));
    assert!(app.show_changes);
    assert!(app.review_list_focused());
}

// --- View transitions (plan §3.3 contract) ---

#[test]
fn i_toggles_history_and_back_to_review() {
    let (_repo, mut app) = review_app("main");
    app.on_key(key('i'));
    assert_eq!(app.view, ViewMode::History);
    app.on_key(key('i'));
    assert_eq!(app.view, ViewMode::Review, "`i` returns to the review home");
}

#[test]
fn esc_in_history_returns_to_review_home() {
    let (_repo, mut app) = review_app("main");
    app.on_key(key('2')); // enter history
    assert_eq!(app.view, ViewMode::History);
    app.on_key(esc());
    assert_eq!(app.view, ViewMode::Review);
}

#[test]
fn esc_in_review_is_a_no_op() {
    let (_repo, mut app) = review_app("main");
    assert_eq!(app.view, ViewMode::Review);
    app.on_key(esc());
    assert_eq!(
        app.view,
        ViewMode::Review,
        "Esc must not leave a review session"
    );
}

#[test]
fn one_returns_home_two_enters_history() {
    let (_repo, mut app) = review_app("main");
    app.on_key(key('2'));
    assert_eq!(app.view, ViewMode::History);
    app.on_key(key('1'));
    assert_eq!(app.view, ViewMode::Review, "`1` returns to the review home");
    // `1` while already home is a no-op.
    app.on_key(key('1'));
    assert_eq!(app.view, ViewMode::Review);
}

#[test]
fn status_session_cannot_enter_review() {
    let repo = init_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    for k in ['1', '2', 'i', '1'] {
        app.on_key(key(k));
        assert_ne!(
            app.view,
            ViewMode::Review,
            "a status session must never reach the review view (after {k:?})"
        );
    }
}

// --- Staging inertness (read-only view) ---

fn staged_paths(dir: &std::path::Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["diff", "--cached", "--name-only"])
        .output()
        .expect("spawn git");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// A review session over a repo with a DIRTY worktree: if a staging input ever
/// leaked through to the status handlers, there would be something to stage.
fn dirty_review_app() -> (TempDir, App) {
    let repo = init_repo_with_diverged_branches();
    write(repo.path(), "feature.txt", "uncommitted local edit\n");
    let app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    assert!(
        !app.status.unstaged.is_empty(),
        "fixture must have stageable changes for inertness to mean anything"
    );
    (repo, app)
}

#[test]
fn staging_keys_are_inert() {
    let (repo, mut app) = dirty_review_app();
    let before = staged_paths(repo.path());
    let _ = dump(&app);
    for k in [space(), enter(), key('s'), key('u'), key('x')] {
        app.on_key(k);
        assert!(app.modal.is_none(), "no modal opens in review");
        assert_eq!(
            staged_paths(repo.path()),
            before,
            "staging keys must not touch the index in review"
        );
    }
}

#[test]
fn marker_column_click_does_not_stage() {
    let (repo, mut app) = dirty_review_app();
    let before = staged_paths(repo.path());
    // Render first so the list geometry is recorded.
    let _ = dump(&app);
    let area = app.review_list_area();
    // Click the change-marker column of the first row (x within the marker zone).
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x + 1,
        area.y,
    ));
    assert!(
        app.modal.is_none(),
        "a marker click opens no modal in review"
    );
    assert_eq!(
        staged_paths(repo.path()),
        before,
        "a marker-column click must not stage in review"
    );
}

// --- Mouse (geometry comes from a prior render) ---

#[test]
fn click_selects_a_row_and_focuses_the_list() {
    let (_repo, mut app) = review_app("main");
    let _ = dump(&app);
    let area = app.review_list_area();
    // Second row (past the marker zone, so it only selects).
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x + 8,
        area.y + 1,
    ));
    assert!(app.review_list_focused());
    assert_eq!(app.review_selected(), 1);
}

#[test]
fn wheel_over_list_moves_selection_over_diff_scrolls() {
    let (_repo, mut app) = review_app("main");
    let _ = dump(&app);
    let list = app.review_list_area();
    app.on_mouse(mouse(MouseEventKind::ScrollDown, list.x + 4, list.y));
    assert_eq!(app.review_selected(), 1);
    app.on_mouse(mouse(MouseEventKind::ScrollUp, list.x + 4, list.y));
    assert_eq!(app.review_selected(), 0);

    let diff = app.diff_area();
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        diff.x + 2,
        diff.y,
    ));
    assert!(app.diff_focused(), "clicking the diff focuses it");
}

// --- View-aware reload + churn guard ---

/// Commit `contents` for `rel` on the current branch (feature/HEAD).
fn commit_file(dir: &std::path::Path, rel: &str, contents: &str, msg: &str) {
    write(dir, rel, contents);
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", msg]);
}

#[test]
fn reload_picks_up_a_new_commit_and_preserves_selection_by_path() {
    let (repo, mut app) = review_app("main");
    let _ = dump(&app);
    // Select feature.txt by path (whatever row it is).
    let target = "feature.txt";
    while app.active_diff_path().as_deref() != Some(target) {
        let before = app.review_selected();
        app.on_key(key('j'));
        assert_ne!(
            app.review_selected(),
            before,
            "feature.txt should be listed"
        );
    }
    let relists_before = app.review_relist_count();

    // A new commit on the reviewed branch edits feature.txt and adds a file.
    commit_file(
        repo.path(),
        "feature.txt",
        "feature\nmore feature\n",
        "extend feature",
    );
    commit_file(repo.path(), "brand-new.txt", "new\n", "add new file");

    app.reload();
    assert!(
        app.review_relist_count() > relists_before,
        "a moved range rebuilds the list"
    );
    // The new file appears.
    assert!(
        app.review_files().iter().any(|f| f.path == "brand-new.txt"),
        "the new commit's file appears in the list"
    );
    // Selection preserved by path.
    assert_eq!(
        app.active_diff_path().as_deref(),
        Some(target),
        "selection follows feature.txt across the reload"
    );
    // The selected file's diff reflects the new content.
    let diff = app.active_diff().expect("a diff is active");
    if let FileDiff::Text(lines) = diff {
        assert!(
            lines.iter().any(|l| l.text.contains("more feature")),
            "the selected file's diff updated to the new content: {lines:?}"
        );
    } else {
        panic!("expected a text diff");
    }
}

#[test]
fn oid_unchanged_reload_skips_relisting() {
    let (repo, mut app) = review_app("main");
    let _ = dump(&app);
    app.on_key(key('j')); // move selection off the default
    let selected = app.review_selected();
    let relists = app.review_relist_count();

    // A worktree save that changes no commit (the common watcher event).
    write(repo.path(), "scratch.txt", "not committed\n");
    app.reload();

    assert_eq!(
        app.review_relist_count(),
        relists,
        "an OID-unchanged reload must not rebuild the list (churn guard)"
    );
    assert_eq!(app.review_selected(), selected, "selection is retained");
}

#[test]
fn deleted_branch_flashes_and_keeps_the_stale_list() {
    // Review `main` from HEAD=feature, then delete `main`.
    let (repo, mut app) = review_app("main");
    let _ = dump(&app);
    let files_before: Vec<String> = app.review_files().iter().map(|f| f.path.clone()).collect();
    assert!(!files_before.is_empty());

    git(repo.path(), &["branch", "-D", "main"]);
    app.reload();

    assert!(
        app.flash.is_some(),
        "a failed re-resolution flashes an error"
    );
    let files_after: Vec<String> = app.review_files().iter().map(|f| f.path.clone()).collect();
    assert_eq!(files_after, files_before, "the stale list is retained");
}

#[test]
fn recovered_branch_clears_the_flash_and_updates_the_list() {
    let (repo, mut app) = review_app("main");
    let _ = dump(&app);
    let main_tip = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "main"])
            .current_dir(repo.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    git(repo.path(), &["branch", "-D", "main"]);
    app.reload();
    assert!(app.flash.is_some());

    // Recreate the base branch: the next reload must recover and stop flashing.
    git(repo.path(), &["branch", "main", main_tip.trim()]);
    app.reload();
    assert!(
        app.flash.is_none(),
        "a successful refresh clears the review flash"
    );
    assert!(!app.review_files().is_empty());
}

#[test]
fn history_reload_keeps_the_selected_file_row() {
    let (repo, mut app) = review_app("main");
    let _ = dump(&app);
    app.on_key(key('i'));
    app.on_key(tab()); // graph → committed pane
    app.on_key(key('j')); // commit row → first file
    let row = app.committed_row();
    assert!(row > 0, "a file row is selected");

    write(repo.path(), "scratch.txt", "worktree noise\n");
    app.reload();

    assert_eq!(
        app.committed_row(),
        row,
        "a watcher reload must not yank the cursor off the viewed file"
    );
}

#[test]
fn entering_history_with_hidden_panel_focuses_the_diff() {
    let (_repo, mut app) = review_app("main");
    let _ = dump(&app);
    app.on_key(key('b')); // hide the left panel
    app.on_key(key('i')); // enter history
    assert!(
        app.diff_focused(),
        "hidden left column ⇒ history focuses the only visible pane"
    );
    app.on_key(esc()); // back to review, panel still hidden
    assert!(
        app.diff_focused(),
        "hidden left column ⇒ review focus returns to the diff"
    );
}

#[test]
fn review_does_not_use_status_focus() {
    // The status `focus` field must not leak into review; review keeps its own.
    let (_repo, mut app) = review_app("main");
    let _ = dump(&app);
    app.on_key(tab());
    assert!(app.diff_focused());
    // `focus` (status pane) is irrelevant here; assert review focus drives it.
    assert_ne!(
        app.focus,
        Focus::Diff,
        "review focus is separate from status focus"
    );
}
