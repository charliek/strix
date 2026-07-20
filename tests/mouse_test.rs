mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{git, init_repo, write};
use strix::app::{App, Focus};
use strix::comments::{Branch, Comment, Scope, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
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

/// Screen row of the file at selection `target`, using the rendered layout.
fn file_row(app: &App, target: usize) -> u16 {
    let area = app.staging_area();
    let offset = app.staging_state_mut().offset();
    let item = (0usize..)
        .find(|&i| strix::ui::staging::selection_at(&app.status, i) == Some(target))
        .unwrap();
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

// --- Double-click + `[x]` click (plan §3.6, C8) -----------------------------
//
// These drive the injectable double-click clock (`on_mouse_at` with explicit
// `Instant`s) so timing is deterministic — no sleeps. Positions come from the
// rendered frame + the recorded pane rects, exactly like the single-click tests.

const W: u16 = 120;
const H: u16 = 30;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn dump(app: &App) -> String {
    dump_frame(app, W, H).unwrap()
}

fn strix_dir(repo: &Path) -> PathBuf {
    repo.join(".git").join("strix")
}

/// The current HEAD oid, the baseline a worktree comment stamps (so the sweep
/// leaves the seeded note anchored rather than staling/dropping it).
fn head_oid(repo: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A worktree comment on the New side, anchored to `line` with a matching context.
fn wt(id: u64, file: &str, line: usize, text: &str, ctx: &str, base: &str) -> Comment {
    Comment {
        scope: Scope::WorkTree,
        id,
        source: Source::Human,
        file: file.to_string(),
        side: Side::New,
        line,
        text: text.to_string(),
        context: Some(ctx.to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
        base: Some(base.to_string()),
        stale: false,
    }
}

fn seed(repo: &Path, branch: &str, comments: Vec<Comment>) {
    let mut branches = BTreeMap::new();
    branches.insert(
        branch.to_string(),
        Branch {
            active_range: None,
            comments,
        },
    );
    let store = Store {
        version: 2,
        next_id: 1000,
        branches,
    };
    let dir = strix_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("comments.json"),
        serde_json::to_string_pretty(&store).unwrap(),
    )
    .unwrap();
}

/// The 0-based frame row (== screen y) of the first line containing `needle`.
fn row_of(frame: &str, needle: &str) -> usize {
    frame
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("frame missing {needle:?}:\n{frame}"))
}

/// Move the status file selection until `path` is the selected file.
fn select_status_file(app: &mut App, path: &str) {
    let _ = dump(app);
    for _ in 0..40 {
        if app.active_diff_path().as_deref() == Some(path) {
            return;
        }
        app.on_key(key('j'));
    }
    panic!("{path} never became the selected status file");
}

/// A status app on `file`'s net diff (an untracked file with `contents`), its
/// selection moved onto it and one frame rendered to populate the pane rects.
fn status_app(file: &str, contents: &str) -> (tempfile::TempDir, App) {
    let repo = init_repo();
    write(repo.path(), file, contents);
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    select_status_file(&mut app, file);
    let _ = dump(&app);
    (repo, app)
}

/// A screen position (col, row) on the diff-pane line containing `needle`.
fn code_pos(app: &App, needle: &str) -> (u16, u16) {
    let frame = dump(app);
    let y = row_of(&frame, needle) as u16;
    (app.diff_area().x + 5, y)
}

/// A review repo whose `feature` branch (checked out → authoring) adds `feat.txt`
/// with one distinctive line, so `strix diff main` reviews that single addition.
fn review_repo() -> tempfile::TempDir {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "feat.txt", "ZEBRA reviewed line\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add feat"]);
    dir
}

// --- Double-click a code line opens the editor (Status + Review) ---

#[test]
fn double_click_code_line_opens_the_editor_in_status() {
    let (_repo, mut app) = status_app("new.txt", "target line\nsecond line\n");
    let (x, y) = code_pos(&app, "target line");

    let t = Instant::now();
    app.on_mouse_at(click(x, y), t);
    assert!(!app.editor_open(), "a single click doesn't open the editor");

    app.on_mouse_at(click(x, y), t + ms(150));
    assert!(
        app.editor_open(),
        "a double-click on a code line opens the editor"
    );
    assert_eq!(
        app.editor_buffer().as_deref(),
        Some(""),
        "a new-comment editor starts empty"
    );
}

#[test]
fn double_click_code_line_opens_the_editor_in_review() {
    let repo = review_repo();
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    // The single file in range is auto-selected; render to place the pane rects.
    let _ = dump(&app);
    let (x, y) = code_pos(&app, "ZEBRA reviewed line");

    let t = Instant::now();
    app.on_mouse_at(click(x, y), t);
    app.on_mouse_at(click(x, y), t + ms(150));
    assert!(
        app.editor_open(),
        "a double-click on a code line opens the editor in review too"
    );
}

// --- Double-click a comment box edits it ---

#[test]
fn double_click_a_comment_box_edits_it() {
    let repo = init_repo();
    write(repo.path(), "new.txt", "target line\n");
    let base = head_oid(repo.path());
    seed(
        repo.path(),
        "main",
        vec![wt(1, "new.txt", 1, "please review", "target line", &base)],
    );
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    select_status_file(&mut app, "new.txt");
    let (x, y) = code_pos(&app, "please review"); // a box body row

    let t = Instant::now();
    app.on_mouse_at(click(x, y), t);
    app.on_mouse_at(click(x, y), t + ms(150));
    assert!(
        app.editor_open(),
        "a double-click on a box opens the editor"
    );
    assert_eq!(
        app.editor_buffer().as_deref(),
        Some("please review"),
        "editing a note pre-fills its text"
    );
}

// --- `[x]` click deletes exactly that comment (a single click) ---

#[test]
fn x_click_deletes_exactly_that_comment() {
    let repo = init_repo();
    write(repo.path(), "new.txt", "alpha\nbeta\n");
    let base = head_oid(repo.path());
    seed(
        repo.path(),
        "main",
        vec![
            wt(1, "new.txt", 1, "on alpha", "alpha", &base),
            wt(2, "new.txt", 2, "on beta", "beta", &base),
        ],
    );
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    select_status_file(&mut app, "new.txt");
    let _ = dump(&app); // record the `[x]` rects

    let close = app.comment_close_rect(1).expect("comment 1's [x] rect");
    app.on_mouse_at(click(close.x, close.y), Instant::now());

    assert!(
        app.active_comment(1).is_none(),
        "the clicked note is deleted"
    );
    assert!(app.active_comment(2).is_some(), "the other note survives");
    assert_eq!(
        app.status_comment_count("new.txt"),
        1,
        "exactly one deleted"
    );
    assert!(
        !app.editor_open(),
        "an `[x]` click deletes; it never opens the editor"
    );
}

// --- 500 ms window boundary ---

#[test]
fn double_click_fires_at_the_500ms_boundary() {
    let (_repo, mut app) = status_app("new.txt", "target line\n");
    let (x, y) = code_pos(&app, "target line");
    let t = Instant::now();
    app.on_mouse_at(click(x, y), t);
    app.on_mouse_at(click(x, y), t + ms(500)); // exactly the window → still a double
    assert!(
        app.editor_open(),
        "a second click at exactly 500ms is a double-click"
    );
}

#[test]
fn a_click_past_500ms_is_not_a_double_click() {
    let (_repo, mut app) = status_app("new.txt", "target line\n");
    let (x, y) = code_pos(&app, "target line");
    let t = Instant::now();
    app.on_mouse_at(click(x, y), t);
    app.on_mouse_at(click(x, y), t + ms(501)); // just past → not a double
    assert!(
        !app.editor_open(),
        "a second click past 500ms doesn't open the editor"
    );
}

// --- Triple-click doesn't re-fire ---

#[test]
fn triple_click_does_not_reopen_the_editor() {
    let (_repo, mut app) = status_app("new.txt", "target line\n");
    let (x, y) = code_pos(&app, "target line");
    let t = Instant::now();
    app.on_mouse_at(click(x, y), t);
    app.on_mouse_at(click(x, y), t + ms(100));
    assert!(app.editor_open(), "the double-click opened the editor");
    // The 3rd rapid press commits the (empty) editor and cannot itself re-fire the
    // double-click (the tracker was reset), so no editor remains.
    app.on_mouse_at(click(x, y), t + ms(200));
    assert!(
        !app.editor_open(),
        "the triple-click's 3rd press does not re-open the editor"
    );
}

// --- Adjacent-row false-positive ---

#[test]
fn clicking_an_adjacent_row_is_not_a_double_click() {
    let (_repo, mut app) = status_app("new.txt", "alpha line\nbeta line\n");
    let frame = dump(&app);
    let x = app.diff_area().x + 5;
    let y1 = row_of(&frame, "alpha line") as u16;
    let y2 = row_of(&frame, "beta line") as u16;
    let t = Instant::now();
    app.on_mouse_at(click(x, y1), t);
    app.on_mouse_at(click(x, y2), t + ms(100)); // a different code line
    assert!(
        !app.editor_open(),
        "two clicks on adjacent rows are not a double-click"
    );
}

// --- `[x]` consumption resets the tracker ---

#[test]
fn a_click_right_after_x_is_not_a_double_click() {
    let repo = init_repo();
    write(repo.path(), "new.txt", "target line\n");
    let base = head_oid(repo.path());
    seed(
        repo.path(),
        "main",
        vec![wt(1, "new.txt", 1, "note", "target line", &base)],
    );
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    select_status_file(&mut app, "new.txt");
    let frame = dump(&app);
    let x = app.diff_area().x + 5;
    let y = row_of(&frame, "target line") as u16;

    let t = Instant::now();
    app.on_mouse_at(click(x, y), t); // stores the code-line target
    let close = app.comment_close_rect(1).expect("[x] rect");
    app.on_mouse_at(click(close.x, close.y), t + ms(50)); // `[x]` consumes + resets
    app.on_mouse_at(click(x, y), t + ms(100)); // same code line, within the window
    assert!(
        !app.editor_open(),
        "a click right after an `[x]` deletion isn't a double-click"
    );
}

// --- Drag / scroll reset the tracker (both isolate the explicit reset: neither
//     rebuilds the layout, so the generation alone wouldn't catch them) ---

#[test]
fn a_drag_between_clicks_resets_the_tracker() {
    let (_repo, mut app) = status_app("new.txt", "target line\n");
    let (x, y) = code_pos(&app, "target line");
    let t = Instant::now();
    app.on_mouse_at(click(x, y), t);
    app.on_mouse_at(
        mouse(x, y, MouseEventKind::Drag(MouseButton::Left)),
        t + ms(50),
    );
    app.on_mouse_at(click(x, y), t + ms(100));
    assert!(
        !app.editor_open(),
        "a drag between two clicks resets the double-click tracker"
    );
}

#[test]
fn a_scroll_between_clicks_resets_the_tracker() {
    // A tall file so a wheel notch (SCROLL_STEP = 3 rows) has room to move.
    let mut content = String::new();
    for i in 0..40 {
        content.push_str(&format!("row{i:02} line\n"));
    }
    let (_repo, mut app) = status_app("big.txt", &content);
    let frame = dump(&app);
    let diff = app.diff_area();
    let x = diff.x + 5;
    let y = row_of(&frame, "row10 line") as u16;

    let t = Instant::now();
    app.on_mouse_at(click(x, y), t); // stores row10's code target
    app.on_mouse_at(mouse(x, y, MouseEventKind::ScrollDown), t + ms(50)); // scrolls 3, resets
                                                                          // After a 3-row scroll the same logical line sits 3 rows higher: without the
                                                                          // reset this would be an equal `HitTarget` (a scroll doesn't bump the layout
                                                                          // generation) and would false-fire.
    app.on_mouse_at(click(x, y - 3), t + ms(100));
    assert!(
        !app.editor_open(),
        "a scroll between two clicks resets the double-click tracker"
    );
}

// --- A layout-generation change makes an otherwise-identical click not a double ---

#[test]
fn a_layout_generation_change_resets_the_tracker() {
    let (_repo, mut app) = status_app("new.txt", "target line\nsecond line\n");
    let frame = dump_frame(&app, 120, 30).unwrap();
    let x = app.diff_area().x + 5;
    let y = row_of(&frame, "target line") as u16;

    let t = Instant::now();
    app.on_mouse_at(click(x, y), t); // stores (generation G, code line)
                                     // Re-render at a different width: the layout rebuilds, bumping the generation,
                                     // while the code line stays at the same row (unified, no boxes).
    let _ = dump_frame(&app, 100, 30).unwrap();
    app.on_mouse_at(click(x, y), t + ms(100));
    assert!(
        !app.editor_open(),
        "a layout-generation change makes the second click not a double"
    );
}

// --- Marker zone / file list are excluded ---

#[test]
fn double_click_in_the_marker_zone_does_not_open_the_editor() {
    let (_repo, mut app) = app_with_two_files();
    dump_frame(&app, W, H).unwrap();
    let area = app.staging_area();
    let row = file_row(&app, 0);
    let x = area.x + 1; // inside the marker zone

    let t = Instant::now();
    app.on_mouse_at(click(x, row), t);
    app.on_mouse_at(click(x, row), t + ms(100));
    assert!(
        !app.editor_open(),
        "double-clicking the marker zone never opens the editor"
    );
}

#[test]
fn double_click_in_the_file_list_does_not_open_the_editor() {
    let (_repo, mut app) = app_with_two_files();
    dump_frame(&app, W, H).unwrap();
    let area = app.staging_area();
    let row = file_row(&app, 1);
    let x = area.x + 8; // past the marker zone → a plain file-name click

    let t = Instant::now();
    app.on_mouse_at(click(x, row), t);
    app.on_mouse_at(click(x, row), t + ms(100));
    assert!(
        !app.editor_open(),
        "double-clicking the file list never opens the editor"
    );
    assert_eq!(app.selected, 1, "the click still selected the file");
}

// --- codex fix #1: SBS column bounds the box hit-test ---

#[test]
fn sbs_click_on_a_boxs_blank_sibling_column_is_not_a_double_click() {
    let repo = init_repo();
    let p = repo.path();
    write(p, "file.txt", "line1\nOLD\nline3\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "base"]);
    write(p, "file.txt", "line1\nNEW\nline3\n"); // an unstaged modification
    let base = head_oid(p);
    seed(
        p,
        "main",
        vec![wt(1, "file.txt", 2, "SIDEBOXNOTE", "NEW", &base)],
    );
    let mut app = App::new(p.to_path_buf()).unwrap();
    select_status_file(&mut app, "file.txt");
    app.on_key(key('d')); // side-by-side: the New-side box sits in the right column
    let frame = dump(&app);
    let diff = app.diff_area();
    let left_w = (diff.width - 1) / 2;
    let row = row_of(&frame, "SIDEBOXNOTE") as u16;

    // Double-click the blank LEFT column beside the box → `hit_target` is None there.
    let t = Instant::now();
    app.on_mouse_at(click(diff.x + 1, row), t);
    app.on_mouse_at(click(diff.x + 1, row), t + ms(150));
    assert!(
        !app.editor_open(),
        "double-clicking a box's blank sibling column doesn't open the editor"
    );

    // Positive control: the same box double-clicked in its own (right) column does.
    let rx = diff.x + left_w + 3;
    let t2 = t + ms(2000);
    app.on_mouse_at(click(rx, row), t2);
    app.on_mouse_at(click(rx, row), t2 + ms(150));
    assert!(
        app.editor_open(),
        "the box double-clicked in its own column opens the editor"
    );
    assert_eq!(app.editor_buffer().as_deref(), Some("SIDEBOXNOTE"));
}

// --- codex fix #2: a keyboard event between clicks breaks the chain ---

#[test]
fn a_key_press_between_clicks_breaks_the_double_click_chain() {
    let (_repo, mut app) = status_app("new.txt", "target line\nsecond line\n");
    let (x, y) = code_pos(&app, "target line");
    let t = Instant::now();
    app.on_mouse_at(click(x, y), t); // stores the code-line target, focuses the diff
    app.on_key(key('j')); // keyboard nav (subsumes Ctrl-D scroll) breaks the chain
    app.on_mouse_at(click(x, y), t + ms(100)); // same row, within the window
    assert!(
        !app.editor_open(),
        "a key press between two clicks isn't a double-click"
    );
}

// --- codex fix #3: a resize breaks the chain before the redraw relayouts ---

#[test]
fn a_resize_between_clicks_breaks_the_double_click_chain() {
    let (_repo, mut app) = status_app("new.txt", "target line\n");
    let (x, y) = code_pos(&app, "target line");
    let t = Instant::now();
    app.on_mouse_at(click(x, y), t); // stores the code-line target
    app.on_resize(); // the event loop's resize arm calls this before the redraw
    app.on_mouse_at(click(x, y), t + ms(100)); // same row/target, within the window
    assert!(
        !app.editor_open(),
        "a resize between two clicks (before the relayout) isn't a double-click"
    );
}
