//! Mouse on strip rows (plan 007 §3.3c–e): a click on a strip row — a row
//! belonging to a following file in the continuous cross-file stream, not the
//! anchor — is **pure cursor placement**. It pins a divergent cursor at the
//! clicked target and moves nothing else: no flip, no reveal, no selection or
//! title change, no scroll. (This replaces plan 006 C5's click-to-select.)
//!
//! On top of that placement sit two acts: a click on a strip box's `[x]` deletes
//! that note, and a second click within the double-click window converges on the
//! cursor's file and acts there — the editor on a code row or an own note, a
//! read-only flash on an agent note, the flip alone on a file header.
//!
//! Anchor-row clicks are untouched throughout (they delegate to the pre-C5 path),
//! and a stale hit map is still inert.

mod common;

use std::time::Instant;

use common::{
    click, config, git, head_oid, init_repo, init_repo_with_diverged_branches,
    init_repo_with_history, mouse, ms, pane_title, prepare_window, press, render_buffer,
    rendered_app, seed_store, select, selected_path, short_status_repo, staged, store_text,
    unstaged, window_of, write,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use strix::app::{App, FileId, Focus, HistoryFocus, LayoutRow, RowTarget, ViewMode};
use strix::comments::{Comment, Scope, Side, Source};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent, MouseEventKind};
use strix::terminal::dump_frame;
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 24;

// --- fixtures ---------------------------------------------------------------

/// Two short untracked files: at width 120 both fit in one viewport, so `b.txt`
/// renders as a strip below the `a.txt` anchor from the first prepared window on
/// — the state a strip click needs, with the scroll offset pinned at 0 so "zero
/// view movement" is checkable by inspection.
fn two_short_files() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "a.txt", "a one\na two\n");
    write(repo.path(), "b.txt", "b one\nb two\nb three\n");
    repo
}

/// A status repo with two tall untracked files (`a.txt` 60 lines, `b.txt` 40
/// lines) — deep enough that either can lead the strip, so the boundary only
/// comes into view once the anchor is scrolled to its end.
fn handoff_status_repo() -> TempDir {
    let repo = init_repo();
    let a: String = (0..60).map(|i| format!("alpha {i}\n")).collect();
    let b: String = (0..40).map(|i| format!("beta {i}\n")).collect();
    write(repo.path(), "a.txt", &a);
    write(repo.path(), "b.txt", &b);
    repo
}

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

/// One path listed **twice** — staged, then modified again in the working tree —
/// so its two stream rows draw the same net diff and render the same comment box
/// twice: once in the anchor, once in the strip.
fn dup_path_repo() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "dup.txt", "one\ntwo\n");
    git(repo.path(), &["add", "dup.txt"]);
    write(repo.path(), "dup.txt", "one\ntwo\nthree\n");
    repo
}

/// A worktree note anchored by context, so the status comment sync keeps it.
fn note(id: u64, file: &str, line: usize, context: &str, base: &str, source: Source) -> Comment {
    Comment {
        scope: Scope::WorkTree,
        id,
        source,
        file: file.to_string(),
        side: Side::New,
        line,
        text: "seeded note".to_string(),
        context: Some(context.to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
        base: Some(base.to_string()),
        stale: false,
    }
}

/// A status app over `repo` with cross-file scroll on, one frame rendered and
/// the window prepared. Focus is left on the file list: a strip click has to
/// focus the diff pane itself.
fn status_app(repo: &TempDir) -> App {
    let mut app = rendered_app(repo, config(true, false), H);
    prepare_window(&mut app);
    dump_frame(&app, W, H).unwrap();
    app
}

/// Park the stream at `(file index, offset)` with the window prepared and one
/// frame rendered — the window hit map a click resolves against is recorded by
/// the renderer, so a test must re-render after moving the scroll offset
/// (mirrors the real event loop: render, then handle the next input).
fn park(app: &mut App, index: usize, offset: usize, h: u16) {
    select(app, index, h);
    app.diff_scroll.set(offset);
    prepare_window(app);
    dump_frame(app, W, h).unwrap();
}

/// A review parked one row before its second file's header, with a comment
/// seeded on that second file — so the note's box is a strip box.
fn review_with_strip_comment_box() -> (TempDir, App) {
    let repo = init_repo_with_diverged_branches();
    let mut app = App::for_review(repo.path().to_path_buf(), &config(true, false), "main").unwrap();
    let h = 10;
    dump_frame(&app, W, h).unwrap();
    let second = app.review_files()[1].path.clone();
    seed_store(
        repo.path(),
        "feature",
        Some("main"),
        vec![Comment {
            scope: Scope::Range {
                range: "main".to_string(),
            },
            id: 1,
            source: Source::Human,
            file: second,
            side: Side::New,
            line: 1,
            text: "a note on the neighbour".to_string(),
            context: None,
            orphaned: false,
            created_at: 1_700_000_000,
            base: None,
            stale: false,
        }],
    );
    app.reload();
    dump_frame(&app, W, h).unwrap();

    let anchor_rows = app.diff_row_count();
    app.diff_scroll.set(anchor_rows.saturating_sub(1));
    prepare_window(&mut app);
    dump_frame(&app, W, h).unwrap();
    (repo, app)
}

// --- window-row lookup ------------------------------------------------------

/// One row the window draws: its screen position and the identity the renderer
/// records for it in the hit map.
struct WindowRow {
    y: u16,
    anchor: bool,
    file: FileId,
    target: RowTarget,
}

/// Every row of the current window in screen order — the same walk the renderer
/// does when it records the hit map, so a click at `row.y` resolves to `row`.
fn window_rows(app: &App) -> Vec<WindowRow> {
    let diff = app.diff_area();
    let window = window_of(app);
    let layout = app.diff_layout(diff.width);
    let mut out = Vec::new();
    let mut y = diff.y;
    for segment in &window.segments {
        let rows: &[LayoutRow] = match &segment.section {
            None => &layout[segment.row_range.clone()],
            Some(section) => &section.rows[segment.row_range.clone()],
        };
        for row in rows {
            if let Some(file) = segment.id.clone() {
                out.push(WindowRow {
                    y,
                    anchor: segment.is_anchor(),
                    file,
                    target: row.target,
                });
            }
            y += 1;
        }
    }
    out
}

/// The first strip row matching `pred`, panicking with the window's shape when
/// there is none.
fn strip_row(app: &App, what: &str, mut pred: impl FnMut(&WindowRow) -> bool) -> WindowRow {
    window_rows(app)
        .into_iter()
        .find(|row| !row.anchor && pred(row))
        .unwrap_or_else(|| panic!("no strip row matching {what} in the current window"))
}

/// The first strip *code* row (a hunk header counts as `Code` too, so skip the
/// header row and take the row after it).
fn strip_code_row(app: &App) -> WindowRow {
    let mut seen_hunk = false;
    strip_row(app, "a code line", |row| match row.target {
        RowTarget::Code(_) => {
            let take = seen_hunk;
            seen_hunk = true;
            take
        }
        _ => false,
    })
}

fn strip_header_row(app: &App) -> WindowRow {
    strip_row(app, "a file header", |row| {
        row.target == RowTarget::FileHeader
    })
}

fn strip_box_row(app: &App, id: u64) -> WindowRow {
    strip_row(app, "a comment box", |row| {
        row.target == RowTarget::Comment(id) || row.target == RowTarget::Orphan(id)
    })
}

/// Whether any cell of buffer row `y` inside the diff pane carries `bg` (the
/// file list paints its own selection background, which must not count).
fn diff_row_has_bg(buf: &Buffer, area: Rect, y: u16, bg: Color) -> bool {
    (area.x..area.x + area.width).any(|x| buf.cell((x, y)).map(|c| c.bg) == Some(bg))
}

// --- (c) a strip click places the cursor and moves nothing ------------------

#[test]
fn a_strip_code_row_click_places_a_divergent_cursor_status() {
    let repo = handoff_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let a_rows = app.diff_row_count();
    park(&mut app, 0, a_rows - 2, H);

    let before_selected = app.selected;
    let before_scroll = app.diff_scroll.get();
    let before_title = pane_title(&render_buffer(&app, W, H), app.diff_area());
    let row = strip_code_row(&app);
    assert_eq!(row.file, unstaged("b.txt"));

    app.on_mouse(click(app.diff_area().x + 2, row.y));

    assert_eq!(
        app.cursor_address().map(|a| (a.file, a.target)),
        Some((row.file, row.target)),
        "the cursor addresses the clicked strip row"
    );
    assert!(app.cursor_divergent());
    assert_eq!(app.selected, before_selected, "no selection change");
    assert_eq!(selected_path(&app), "a.txt");
    assert_eq!(app.diff_scroll.get(), before_scroll, "zero view movement");
    let buf = render_buffer(&app, W, H);
    assert_eq!(
        pane_title(&buf, app.diff_area()),
        before_title,
        "the title still names the anchor"
    );
    assert!(
        diff_row_has_bg(&buf, app.diff_area(), row.y, app.theme.selection_bg),
        "the clicked strip row carries the cursor highlight"
    );
    assert_eq!(
        app.focus,
        Focus::Diff,
        "a strip click focuses the diff pane, which placement requires"
    );
}

#[test]
fn a_strip_code_row_click_places_a_divergent_cursor_review() {
    let repo = handoff_review_repo();
    let mut app = App::for_review(repo.path().to_path_buf(), &config(true, false), "main").unwrap();
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.review_files().len(), 2);
    let a_rows = app.diff_row_count();
    app.diff_scroll.set(a_rows - 2);
    prepare_window(&mut app);
    dump_frame(&app, W, H).unwrap();

    let before_selected = app.review_selected();
    let before_scroll = app.diff_scroll.get();
    let row = strip_code_row(&app);

    app.on_mouse(click(app.diff_area().x + 2, row.y));

    assert_eq!(
        app.cursor_address().map(|a| (a.file, a.target)),
        Some((row.file, row.target))
    );
    assert!(app.cursor_divergent());
    assert_eq!(
        app.review_selected(),
        before_selected,
        "no selection change"
    );
    assert_eq!(app.active_diff_path().as_deref(), Some("a.txt"));
    assert_eq!(app.diff_scroll.get(), before_scroll, "zero view movement");
    let buf = render_buffer(&app, W, H);
    assert!(pane_title(&buf, app.diff_area()).contains("a.txt"));
    assert!(diff_row_has_bg(
        &buf,
        app.diff_area(),
        row.y,
        app.theme.selection_bg
    ));
}

#[test]
fn a_strip_file_header_click_places_the_cursor_on_the_header() {
    let repo = two_short_files();
    let mut app = status_app(&repo);
    let before_scroll = app.diff_scroll.get();
    let row = strip_header_row(&app);
    assert_eq!(row.file, unstaged("b.txt"));

    app.on_mouse(click(app.diff_area().x + 2, row.y));

    assert_eq!(
        app.cursor_address().map(|a| (a.file, a.target)),
        Some((unstaged("b.txt"), RowTarget::FileHeader))
    );
    assert_eq!(app.selected, 0, "nothing else moves");
    assert_eq!(selected_path(&app), "a.txt");
    assert_eq!(app.diff_scroll.get(), before_scroll);
}

#[test]
fn a_strip_box_click_places_the_cursor_without_deleting_or_editing() {
    let (_repo, mut app) = review_with_strip_comment_box();
    let before_selected = app.review_selected();
    let before_scroll = app.diff_scroll.get();
    let row = strip_box_row(&app, 1);

    // Anywhere on the box but its `[x]`: the pane's left edge.
    app.on_mouse(click(app.diff_area().x + 2, row.y));

    assert!(
        app.active_comment(1).is_some(),
        "clicking the box body never deletes it"
    );
    assert!(!app.editor_open(), "no editor opens from a single click");
    assert_eq!(
        app.cursor_address().map(|a| (a.file, a.target)),
        Some((row.file, row.target)),
        "the cursor rests on the box's target"
    );
    assert_eq!(app.review_selected(), before_selected, "no flip");
    assert_eq!(app.diff_scroll.get(), before_scroll);
}

// --- (d) the strip `[x]` deletes -------------------------------------------

#[test]
fn a_strip_x_click_deletes_the_note_status() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(
        repo.path(),
        "main",
        None,
        vec![note(1, "b.txt", 1, "b one", &base, Source::Human)],
    );
    let mut app = status_app(&repo);
    assert_eq!(app.status_comment_count("b.txt"), 1);
    let close = app.comment_close_rect(1).expect("the strip box's [x] rect");
    let box_row = strip_box_row(&app, 1);
    assert_eq!(close.y, box_row.y, "the recorded rect is the strip box's");

    app.on_mouse(click(close.x, close.y));

    assert_eq!(app.status_comment_count("b.txt"), 0, "the note is gone");
    assert_eq!(app.selected, 0, "no selection change");
    assert_eq!(selected_path(&app), "a.txt");
    assert!(
        !dump_frame(&app, W, H).unwrap().contains("seeded note"),
        "the stream re-renders without the box"
    );
}

#[test]
fn a_strip_x_click_deletes_the_note_review() {
    let (_repo, mut app) = review_with_strip_comment_box();
    let before_selected = app.review_selected();
    let close = app.comment_close_rect(1).expect("the strip box's [x] rect");
    assert_eq!(close.y, strip_box_row(&app, 1).y);

    app.on_mouse(click(close.x, close.y));

    assert!(app.active_comment(1).is_none(), "the note is gone");
    assert_eq!(
        app.review_selected(),
        before_selected,
        "no selection change"
    );
    assert!(!dump_frame(&app, W, 10)
        .unwrap()
        .contains("a note on the neighbour"));
}

#[test]
fn a_dup_path_double_render_keeps_the_anchors_x_and_drops_the_strips() {
    let repo = dup_path_repo();
    let base = head_oid(repo.path());
    seed_store(
        repo.path(),
        "main",
        None,
        vec![note(1, "dup.txt", 1, "one", &base, Source::Human)],
    );
    let mut app = status_app(&repo);

    // The same comment id is drawn twice: once in the anchor's section, once in
    // the strip's — the collision anchor precedence resolves (plan 007 §3.3d).
    let rows = window_rows(&app);
    let anchor_box = rows
        .iter()
        .find(|row| row.anchor && row.target == RowTarget::Comment(1))
        .expect("the anchor draws the box");
    let strip_box = rows
        .iter()
        .find(|row| !row.anchor && row.target == RowTarget::Comment(1))
        .expect("the strip draws the same box");
    assert_eq!(anchor_box.file, staged("dup.txt"));
    assert_eq!(strip_box.file, unstaged("dup.txt"), "two rows, one path");
    let close = app.comment_close_rect(1).expect("one recorded rect");
    assert_eq!(
        close.y, anchor_box.y,
        "the anchor's rect won; the strip copy was dropped"
    );

    // A click at the strip copy's own `[x]` column is therefore not a delete —
    // it falls through to placement.
    app.on_mouse(click(close.x, strip_box.y));
    assert_eq!(
        app.status_comment_count("dup.txt"),
        1,
        "the strip copy's [x] isn't clickable this frame"
    );
    assert_eq!(
        app.cursor_address().map(|a| (a.file, a.target)),
        Some((strip_box.file.clone(), RowTarget::Comment(1)))
    );

    // The anchor's `[x]`, at its own rect, still deletes.
    app.on_mouse(click(close.x, close.y));
    assert_eq!(app.status_comment_count("dup.txt"), 0);
}

#[test]
fn deleting_the_box_under_a_divergent_cursor_brings_the_cursor_home() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(
        repo.path(),
        "main",
        None,
        vec![note(1, "b.txt", 1, "b one", &base, Source::Human)],
    );
    let mut app = status_app(&repo);
    let close = app.comment_close_rect(1).expect("the strip box's [x] rect");

    // Place the cursor on the box, then delete the box out from under it.
    app.on_mouse(click(app.diff_area().x + 2, close.y));
    assert_eq!(
        app.cursor_address().map(|a| a.target),
        Some(RowTarget::Comment(1))
    );
    assert!(app.cursor_divergent(), "the box is the strip file's");
    app.on_mouse(click(close.x, close.y));

    assert_eq!(app.status_comment_count("b.txt"), 0);
    assert!(
        !app.cursor_divergent(),
        "a target that no longer resolves is never left dangling"
    );
    assert_eq!(
        app.cursor_address().map(|a| a.file),
        Some(unstaged("a.txt")),
        "it came home to the anchor's top, not to a foreign target"
    );
    dump_frame(&app, W, H).unwrap();
}

// --- side-by-side: the blank sibling column --------------------------------

#[test]
fn a_side_by_side_blank_sibling_column_click_matches_the_anchor_equivalent() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(
        repo.path(),
        "main",
        None,
        vec![note(1, "b.txt", 1, "b one", &base, Source::Human)],
    );
    let cfg = Config {
        diff_mode: Some("side-by-side".to_string()),
        ..config(true, false)
    };
    let mut app = rendered_app(&repo, cfg, H);
    prepare_window(&mut app);
    dump_frame(&app, W, H).unwrap();

    // The note is on the New side, so its box lives in the right column and the
    // left column of that row is blank.
    let close = app.comment_close_rect(1).expect("the strip box's [x] rect");
    let diff = app.diff_area();
    assert!(
        close.x > diff.x + diff.width / 2,
        "the box is in the right-hand column"
    );
    let blank_x = diff.x + 2;

    // Placement still happens on the row's target — exactly what a click on an
    // anchor box's blank sibling column does — but the row is no double-click
    // candidate there, so a fast pair never opens the editor.
    let t = Instant::now();
    app.on_mouse_at(click(blank_x, close.y), t);
    assert_eq!(
        app.cursor_address().map(|a| (a.file, a.target)),
        Some((unstaged("b.txt"), RowTarget::Comment(1)))
    );
    assert_eq!(app.status_comment_count("b.txt"), 1, "and never deletes");
    app.on_mouse_at(click(blank_x, close.y), t + ms(120));
    assert!(
        !app.editor_open(),
        "the blank sibling column can't pair into a double-click"
    );
    assert_eq!(app.selected, 0, "and never flips");

    // In the box's own column the same pair does pair.
    let t = Instant::now();
    app.on_mouse_at(click(close.x - 6, close.y), t);
    app.on_mouse_at(click(close.x - 6, close.y), t + ms(120));
    assert!(app.editor_open(), "the box's own column pairs");
}

// --- (e) double-click -------------------------------------------------------

#[test]
fn a_strip_code_row_double_click_opens_the_editor_on_that_line() {
    let repo = two_short_files();
    let mut app = status_app(&repo);
    let row = strip_code_row(&app);
    let RowTarget::Code(_) = row.target else {
        panic!("expected a code row")
    };
    let x = app.diff_area().x + 2;

    let t = Instant::now();
    app.on_mouse_at(click(x, row.y), t);
    assert!(!app.editor_open(), "the first click only places the cursor");
    assert_eq!(app.selected, 0, "and never flips");

    app.on_mouse_at(click(x, row.y), t + ms(150));
    assert!(app.editor_open(), "the second click converges and opens");
    assert_eq!(selected_path(&app), "b.txt", "converged on the strip file");
    assert!(!app.cursor_divergent());

    for ch in "hello".chars() {
        press(&mut app, ch);
    }
    app.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.status_comment_count("b.txt"), 1);
    let store = store_text(repo.path());
    assert!(store.contains("\"file\": \"b.txt\""));
    assert!(
        store.contains("\"context\": \"b one\""),
        "anchored on the clicked line, not the same index in the anchor file:\n{store}"
    );
}

#[test]
fn a_strip_double_click_on_an_own_note_edits_it() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(
        repo.path(),
        "main",
        None,
        vec![note(1, "b.txt", 1, "b one", &base, Source::Human)],
    );
    let mut app = status_app(&repo);
    let row = strip_box_row(&app, 1);
    let x = app.diff_area().x + 2;

    let t = Instant::now();
    app.on_mouse_at(click(x, row.y), t);
    app.on_mouse_at(click(x, row.y), t + ms(150));

    assert!(app.editor_open(), "the own note opens for editing");
    assert_eq!(app.editor_buffer().as_deref(), Some("seeded note"));
    assert_eq!(selected_path(&app), "b.txt", "converged on the note's file");
}

#[test]
fn a_strip_double_click_on_an_agent_note_flashes_without_flipping() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(
        repo.path(),
        "main",
        None,
        vec![note(1, "b.txt", 1, "b one", &base, Source::Agent)],
    );
    let mut app = status_app(&repo);
    let before_scroll = app.diff_scroll.get();
    let row = strip_box_row(&app, 1);
    let x = app.diff_area().x + 2;

    let t = Instant::now();
    app.on_mouse_at(click(x, row.y), t);
    app.on_mouse_at(click(x, row.y), t + ms(150));

    assert!(!app.editor_open(), "an agent note is read-only");
    assert_eq!(
        app.flash.as_ref().map(|f| f.text.as_str()),
        Some("agent note — read-only")
    );
    assert_eq!(app.selected, 0, "eligibility before the flip: no flip");
    assert_eq!(selected_path(&app), "a.txt");
    assert_eq!(app.diff_scroll.get(), before_scroll, "the view is unmoved");
    assert!(app.cursor_divergent(), "the cursor still names the note");
}

#[test]
fn a_strip_file_header_double_click_only_converges() {
    let repo = two_short_files();
    let mut app = status_app(&repo);
    let row = strip_header_row(&app);
    let x = app.diff_area().x + 2;

    let t = Instant::now();
    app.on_mouse_at(click(x, row.y), t);
    assert_eq!(app.selected, 0);

    app.on_mouse_at(click(x, row.y), t + ms(150));

    assert_eq!(selected_path(&app), "b.txt", "the flip is the whole act");
    assert!(!app.editor_open(), "a header opens nothing");
    assert!(!app.cursor_divergent());
    assert_eq!(
        app.cursor_address().map(|a| a.file),
        Some(unstaged("b.txt"))
    );
}

#[test]
fn a_slow_second_strip_click_is_just_another_placement() {
    let repo = two_short_files();
    let mut app = status_app(&repo);
    let first = strip_code_row(&app);
    let x = app.diff_area().x + 2;

    let t = Instant::now();
    app.on_mouse_at(click(x, first.y), t);
    app.on_mouse_at(click(x, first.y), t + ms(600));

    assert!(!app.editor_open(), "past the double-click window");
    assert_eq!(app.selected, 0, "no flip");
    assert_eq!(
        app.cursor_address().map(|a| (a.file, a.target)),
        Some((first.file, first.target)),
        "the cursor was placed, twice"
    );
}

#[test]
fn a_strip_pair_drained_without_a_redraw_still_pairs() {
    let repo = two_short_files();
    let mut app = status_app(&repo);
    let row = strip_code_row(&app);
    let x = app.diff_area().x + 2;

    // No `dump_frame` between the two: both clicks resolve against the same
    // recorded map, which placement leaves valid (nothing in the epoch moved).
    let t = Instant::now();
    app.on_mouse_at(click(x, row.y), t);
    app.on_mouse_at(click(x, row.y), t + ms(150));

    assert!(
        app.editor_open(),
        "the pair fires without an intervening frame"
    );
    assert_eq!(selected_path(&app), "b.txt");
}

#[test]
fn a_strip_pair_with_an_intervening_redraw_still_pairs() {
    let repo = two_short_files();
    let mut app = status_app(&repo);
    let row = strip_code_row(&app);
    let x = app.diff_area().x + 2;

    let t = Instant::now();
    app.on_mouse_at(click(x, row.y), t);
    dump_frame(&app, W, H).unwrap();
    app.on_mouse_at(click(x, row.y), t + ms(150));

    assert!(app.editor_open());
    assert_eq!(selected_path(&app), "b.txt");
}

// --- kept regressions -------------------------------------------------------

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
    assert!(!app.cursor_divergent(), "and no cursor placed");
}

#[test]
fn a_click_drained_after_a_same_batch_flip_falls_through_to_the_anchor_path() {
    let repo = handoff_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let a_rows = app.diff_row_count();
    // One render at the boundary: b.txt's strip is visible, and the window hit
    // map is recorded for *this* frame (a.txt anchor, offset `a_rows - 2`).
    park(&mut app, 0, a_rows - 2, H);

    let y = strip_code_row(&app).y;
    let x = app.diff_area().x + 2;

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
    // `window_hit_at` trusted the stale map, this would place the cursor at
    // that row's now-meaningless target; the epoch guard must instead treat the
    // map as absent and fall through to the anchor path — a plain Status click
    // on the diff pane only focuses the pane.
    app.on_mouse(click(x, y));

    assert_eq!(
        app.selected, 1,
        "the click must not re-resolve against the stale strip mapping"
    );
    assert!(!app.cursor_divergent(), "and places no divergent cursor");
    assert_eq!(
        app.review_cursor(),
        0,
        "the anchor path never moves the cursor on a plain Status click"
    );
    assert_eq!(
        app.focus,
        Focus::Diff,
        "the click still lands in the diff pane"
    );
}

#[test]
fn an_anchor_row_click_is_unaffected_by_the_window_hit_map() {
    let repo = handoff_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    // The anchor's own top rows — never a strip row.
    let diff = app.diff_area();
    app.on_mouse(click(diff.x + 2, diff.y + 1));

    // Status's plain single click on the diff pane only focuses (unchanged
    // pre-C5 behaviour); it does not move the cursor or the selection.
    assert_eq!(app.selected, 0, "no strip placement kicked in");
    assert_eq!(selected_path(&app), "a.txt");
    assert!(!app.cursor_divergent());
}

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
