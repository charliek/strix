//! Mouse on strip rows (plan 006 C5, §3.6): a click on a strip row — a row
//! belonging to a following file in the continuous cross-file stream, not the
//! anchor — selects that file through the same prepared-section path the
//! keyboard cross uses, places the cursor on the clicked target, and reveals.
//! No `[x]`-delete and no double-click-to-edit on strip rows in v1; anchor-row
//! clicks are untouched (they delegate to the pre-C5 path).

mod common;

use std::collections::BTreeMap;
use std::time::Instant;

use common::{
    click, config, git, init_repo, init_repo_with_diverged_branches, init_repo_with_history, mouse,
    ms, pane_title, prepare_window, render_buffer, rendered_app, select, selected_path,
    short_status_repo, window_of, write,
};
use strix::app::{App, BoxPart, Focus, HistoryFocus, RowContent, ViewMode};
use strix::comments::{Branch, Comment, Scope, Side, Source, Store};
use strix::crossterm::event::{KeyCode, KeyEvent, MouseEventKind};
use strix::terminal::dump_frame;
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 24;

// --- construction ------------------------------------------------------

/// A status repo with two tall untracked files (`a.txt` 60 lines, `b.txt` 40
/// lines) — deep enough that either can lead the strip.
fn handoff_status_repo() -> TempDir {
    let repo = init_repo();
    let a: String = (0..60).map(|i| format!("alpha {i}\n")).collect();
    let b: String = (0..40).map(|i| format!("beta {i}\n")).collect();
    write(repo.path(), "a.txt", &a);
    write(repo.path(), "b.txt", &b);
    repo
}

/// Two short untracked files whose whole stream (headers included) fits well
/// A review with two tall files (`a.txt`, `b.txt`) added on `feature`, mirroring
/// `handoff_status_repo` for the Review view.
fn handoff_review_repo() -> TempDir {
    let repo = init_repo();
    git(repo.path(), &["checkout", "-qb", "feature"]);
    let a: String = (0..60).map(|i| format!("alpha {i}\n")).collect();
    let b: String = (0..40).map(|i| format!("beta {i}\n")).collect();
    write(repo.path(), "a.txt", &a);
    write(repo.path(), "b.txt", &b);
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "add a and b"]);
    repo
}

// --- event helpers -------------------------------------------------------

/// Park the stream at `(file index, offset)` with the window prepared and one
/// frame rendered — the window hit map a click resolves against is recorded
/// by the renderer, so a test must re-render after moving the scroll offset
/// (mirrors the real event loop: render, then handle the next input).
fn park(app: &mut App, index: usize, offset: usize, h: u16) {
    select(app, index, h);
    app.diff_scroll.set(offset);
    prepare_window(app);
    dump_frame(app, W, h).unwrap();
}

/// Seed a one-comment review store on `file` (mirrors
/// `cross_file_scroll_test.rs`'s `seed_comment`).
fn seed_review_comment(repo: &std::path::Path, file: &str) {
    let mut branches = BTreeMap::new();
    branches.insert(
        "feature".to_string(),
        Branch {
            active_range: Some("main".to_string()),
            comments: vec![Comment {
                scope: Scope::Range {
                    range: "main".to_string(),
                },
                id: 1,
                source: Source::Human,
                file: file.to_string(),
                side: Side::New,
                line: 1,
                text: "a note on the neighbour".to_string(),
                context: None,
                orphaned: false,
                created_at: 1_700_000_000,
                base: None,
                stale: false,
            }],
        },
    );
    let store = Store {
        version: 2,
        next_id: 1000,
        branches,
    };
    let dir = repo.join(".git").join("strix");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("comments.json"),
        serde_json::to_string_pretty(&store).unwrap(),
    )
    .unwrap();
}

// --- strip CODE row: Status + Review ---------------------------------------

#[test]
fn click_on_a_strip_code_row_selects_the_file_and_places_the_cursor_status() {
    let repo = handoff_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let a_rows = app.diff_row_count();
    park(&mut app, 0, a_rows - 2, H);

    let window = window_of(&app);
    let seg0_rows = window.segments[0].rows();
    let seg1 = &window.segments[1];
    assert_eq!(seg1.path, "b.txt");
    let section = seg1.section.as_ref().unwrap();
    let row = &section.rows[seg1.row_range.clone()][1]; // skip the header (k=0)
    let target = row.target;
    let rel_y = seg0_rows + 1;

    let diff = app.diff_area();
    app.on_mouse(click(diff.x + 2, diff.y + rel_y as u16));

    assert_eq!(app.selected, 1, "the strip file became the anchor");
    assert_eq!(selected_path(&app), "b.txt");
    assert_eq!(
        app.review_cursor(),
        1,
        "the cursor lands on the clicked row's target"
    );
    assert_eq!(
        app.diff_layout(app.diff_area().width)[1].target,
        target,
        "same target as the row that was clicked"
    );
    let buf = render_buffer(&app, W, H);
    assert!(
        pane_title(&buf, app.diff_area()).contains("b.txt"),
        "the border title flips"
    );
    let viewport = app.diff_area().height as usize;
    let top = app.diff_scroll.get();
    assert!(
        top <= 1 && 1 < top + viewport,
        "the clicked line is visible after the reveal"
    );
}

#[test]
fn click_on_a_strip_code_row_selects_the_file_and_places_the_cursor_review() {
    let repo = handoff_review_repo();
    let mut app = App::for_review(repo.path().to_path_buf(), &config(true, false), "main").unwrap();
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.review_files().len(), 2);
    let a_rows = app.diff_row_count();
    app.diff_scroll.set(a_rows - 2);
    prepare_window(&mut app);
    dump_frame(&app, W, H).unwrap();

    let window = window_of(&app);
    let seg0_rows = window.segments[0].rows();
    let seg1 = &window.segments[1];
    assert_eq!(seg1.path, "b.txt");
    let rel_y = seg0_rows + 1;

    let diff = app.diff_area();
    app.on_mouse(click(diff.x + 2, diff.y + rel_y as u16));

    assert_eq!(app.review_selected(), 1, "the strip file became the anchor");
    assert_eq!(app.active_diff_path().as_deref(), Some("b.txt"));
    assert_eq!(app.review_cursor(), 1);
    let buf = render_buffer(&app, W, H);
    assert!(pane_title(&buf, app.diff_area()).contains("b.txt"));
}

// --- strip FileHeader row ----------------------------------------------

#[test]
fn click_on_a_strip_file_header_row_selects_the_file_with_the_cursor_on_the_header() {
    let repo = handoff_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let a_rows = app.diff_row_count();
    park(&mut app, 0, a_rows - 3, H);

    let window = window_of(&app);
    let seg0_rows = window.segments[0].rows();
    assert_eq!(seg0_rows, 3, "a.txt contributes exactly its tail 3 rows");
    let seg1 = &window.segments[1];
    let section = seg1.section.as_ref().unwrap();
    let header_row = &section.rows[seg1.row_range.clone()][0];
    assert!(
        matches!(header_row.content, RowContent::FileHeader(_)),
        "row 0 of a strip file is always its header"
    );

    let diff = app.diff_area();
    app.on_mouse(click(diff.x + 2, diff.y + seg0_rows as u16));

    assert_eq!(app.selected, 1);
    assert_eq!(selected_path(&app), "b.txt");
    assert_eq!(
        app.review_cursor(),
        0,
        "the cursor lands on the header, the file's first row"
    );
}

// --- strip COMMENT-BOX row -----------------------------------------------

/// Find the neighbour's comment box title row (the one carrying the `[x]`) in
/// the current window: its screen offset relative to the pane top (for the
/// click position), its physical row *within its own file's layout* (row_range
/// always starts at 0 for a strip segment, so this is `k` — what the cursor
/// resolves to once that file becomes the anchor), and the comment id.
fn find_strip_box_title(app: &App) -> (usize, usize, u64) {
    let window = window_of(app);
    let mut screen_row = 0usize;
    for segment in &window.segments {
        if !segment.is_anchor() {
            let section = segment.section.as_ref().unwrap();
            for (k, row) in section.rows[segment.row_range.clone()].iter().enumerate() {
                if let RowContent::Box(boxed) = &row.content {
                    if matches!(boxed.part, BoxPart::Title(_)) {
                        return (screen_row + k, k, boxed.id);
                    }
                }
            }
        }
        screen_row += segment.rows();
    }
    panic!("no strip comment box title row in the current window");
}

/// A review parked one row before its second file's header, with a comment
/// seeded on that second file — mirrors
/// `cross_file_scroll_test.rs::a_neighbours_comment_box_scrolls_through_without_a_jump`.
fn review_with_strip_comment_box() -> (TempDir, App) {
    let repo = init_repo_with_diverged_branches();
    let mut app = App::for_review(repo.path().to_path_buf(), &config(true, false), "main").unwrap();
    let h = 10;
    dump_frame(&app, W, h).unwrap();
    let second = app.review_files()[1].path.clone();
    seed_review_comment(repo.path(), &second);
    app.reload();
    dump_frame(&app, W, h).unwrap();

    let anchor_rows = app.diff_row_count();
    app.diff_scroll.set(anchor_rows.saturating_sub(1));
    prepare_window(&mut app);
    dump_frame(&app, W, h).unwrap();
    (repo, app)
}

#[test]
fn click_on_a_strip_comment_box_selects_the_file() {
    let (_repo, mut app) = review_with_strip_comment_box();
    let (screen_row, file_local_row, comment_id) = find_strip_box_title(&app);
    let before_selected = app.review_selected();

    let diff = app.diff_area();
    // Anywhere on the box's title row — column doesn't matter, the window hit
    // map keys on the row only (plan 006 §3.6).
    app.on_mouse(click(diff.x + 2, diff.y + screen_row as u16));

    assert_ne!(
        app.review_selected(),
        before_selected,
        "the neighbour became the anchor"
    );
    assert!(
        app.active_comment(comment_id).is_some(),
        "clicking the box body never deletes it"
    );
    assert!(!app.editor_open(), "no editor opens from a single click");
    assert_eq!(
        app.review_cursor(),
        file_local_row,
        "the cursor lands on the box's target (its first physical row)"
    );
}

#[test]
fn x_click_on_a_strip_box_does_not_delete_it() {
    let (_repo, mut app) = review_with_strip_comment_box();
    let (screen_row, _file_local_row, comment_id) = find_strip_box_title(&app);

    // An anchor box's `[x]` cell starts at `box_w - 4` from the box's left edge
    // (`box_title_spans`); clicking `box_w - 3` lands on its `x`. Full-width
    // since the box is unified.
    let diff = app.diff_area();
    let box_w = diff.width as usize;
    let close_x = diff.x + (box_w - 3) as u16;
    app.on_mouse(click(close_x, diff.y + screen_row as u16));

    assert!(
        app.active_comment(comment_id).is_some(),
        "the strip box's `[x]` isn't recorded, so this is just a select"
    );
    assert_eq!(app.review_selected(), 1, "the neighbour became the anchor");
}

// --- shortfall region: no-op --------------------------------------------

#[test]
fn click_in_the_shortfall_region_is_a_no_op() {
    let repo = short_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let window = window_of(&app);
    let viewport = app.diff_area().height as usize;
    assert!(
        window.rows() < viewport,
        "the whole stream is shorter than the viewport"
    );
    let before_path = selected_path(&app);
    let before_scroll = app.diff_scroll.get();

    let diff = app.diff_area();
    // Well past the last drawn row.
    app.on_mouse(click(diff.x + 2, diff.y + (viewport as u16 - 1)));

    assert_eq!(app.selected, 0, "no selection change");
    assert_eq!(selected_path(&app), before_path);
    assert_eq!(app.diff_scroll.get(), before_scroll);
}

// --- double-click starting on a strip row stays inert -----------------

#[test]
fn double_click_starting_on_a_strip_row_never_opens_the_editor() {
    let repo = handoff_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let a_rows = app.diff_row_count();
    park(&mut app, 0, a_rows - 2, H);

    let window = window_of(&app);
    let seg0_rows = window.segments[0].rows();
    let rel_y = seg0_rows + 1; // a strip code row of b.txt

    let diff = app.diff_area();
    let (x, y) = (diff.x + 2, diff.y + rel_y as u16);

    let t = Instant::now();
    app.on_mouse_at(click(x, y), t);
    assert!(!app.editor_open(), "the first click just selects");
    assert_eq!(app.selected, 1, "the flip happened");

    app.on_mouse_at(click(x, y), t + ms(150));
    assert!(
        !app.editor_open(),
        "a strip-originated click can never pair into a double-click"
    );
}

// --- a same-batch flip stales the recorded map (correctness review) --------

#[test]
fn a_click_drained_after_a_same_batch_flip_falls_through_to_the_anchor_path() {
    let repo = handoff_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let a_rows = app.diff_row_count();
    // One render at the boundary: b.txt's strip is visible, and the window hit
    // map is recorded for *this* frame (a.txt anchor, offset `a_rows - 2`).
    park(&mut app, 0, a_rows - 2, H);

    let window = window_of(&app);
    let seg0_rows = window.segments[0].rows();
    let rel_y = seg0_rows + 1; // the exact pixel a strip code row of b.txt occupied
    let diff = app.diff_area();
    let (x, y) = (diff.x + 2, diff.y + rel_y as u16);

    // Drain a wheel tick that flips the anchor to b.txt — *without* an
    // intervening render, so the recorded window hit map still describes the
    // pre-flip frame above (the event loop drains a whole batch before the
    // next redraw; a click can land right after a flip like this one).
    for _ in 0..10 {
        if app.selected != 0 {
            break;
        }
        app.on_mouse(mouse(x, y, MouseEventKind::ScrollDown));
    }
    assert_eq!(app.selected, 1, "the wheel tick alone crossed the boundary");

    // The click lands at the exact pixel that used to be a strip row. If
    // `window_hit_at` trusted the stale map, this would replay as a strip
    // click and pin the cursor to that row's now-meaningless target; the
    // epoch guard must instead treat the (stale) map as absent and fall
    // through to the anchor path — a plain Status click on the diff pane only
    // focuses the pane, leaving the cursor at its default (top) resolution.
    app.on_mouse(click(x, y));

    assert_eq!(
        app.selected, 1,
        "the click must not re-resolve against the stale strip mapping"
    );
    assert_eq!(
        app.review_cursor(),
        0,
        "the anchor path never moves the cursor on a plain Status click \
         (a stale strip hit would have pinned it to a non-zero row)"
    );
    assert_eq!(
        app.focus,
        Focus::Diff,
        "the click still lands in the diff pane"
    );
}

// --- anchor-row regression (cross-file scroll ON) -----------------------

#[test]
fn an_anchor_row_click_is_unaffected_by_the_window_hit_map() {
    let repo = handoff_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    // The anchor's own top rows — never a strip row.
    let diff = app.diff_area();
    app.on_mouse(click(diff.x + 2, diff.y + 1));

    // Status's plain single click on the diff pane only focuses (unchanged
    // pre-C5 behaviour); it does not move the cursor or the selection.
    assert_eq!(app.selected, 0, "no strip selection kicked in");
    assert_eq!(selected_path(&app), "a.txt");
}

// --- History: the map is absent / anchor-only, unaffected ---------------

#[test]
fn history_diff_click_is_unaffected_by_the_window_hit_map() {
    let repo = init_repo_with_history();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.on_key(KeyEvent::from(KeyCode::Char('i')));
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.view, ViewMode::History);

    let diff = app.diff_area();
    app.on_mouse(click(diff.x + 2, diff.y + 1));
    assert_eq!(app.history_focus(), HistoryFocus::Diff);
}
