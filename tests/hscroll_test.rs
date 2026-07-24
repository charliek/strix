//! Trackpad horizontal scroll (plan §3.5, C6). A `ScrollLeft`/`ScrollRight`
//! gesture over the diff pane shifts *code content* sideways by a fixed step,
//! leaving the gutters, sign column, hunk headers, comment boxes, and editor
//! fixed. It is clamped to the longest code line at read time (hunk headers
//! excluded), reset on file/mode/wrap changes but preserved on a same-file
//! refresh, and inert while wrap is on. Colours aren't in `dump_frame`'s glyph
//! text, so emphasis checks read the rendered `Buffer`.

mod common;

use common::{cell_bg, cell_symbol, git, init_repo, press, render_buffer, write};
use strix::app::{App, DiffMode};
use strix::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use tempfile::TempDir;

fn enter(app: &mut App) {
    app.on_key(KeyEvent::from(KeyCode::Enter));
}

const W: u16 = 100;
const H: u16 = 30;

fn mouse(col: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn dump(app: &App) -> String {
    strix::terminal::dump_frame(app, W, H).unwrap()
}

/// A repo with `code.txt` committed as `before`, edited (uncommitted) to `after`
/// — the sole auto-selected Status entry.
fn modified_repo(before: &str, after: &str) -> TempDir {
    let repo = init_repo();
    let p = repo.path();
    write(p, "code.txt", before);
    git(p, &["add", "code.txt"]);
    git(p, &["commit", "-q", "-m", "add"]);
    write(p, "code.txt", after);
    repo
}

/// The screen row of the diff line whose rendered text contains `needle`.
fn row_of(app: &App, needle: &str) -> u16 {
    dump(app)
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("frame has no {needle:?}:\n{}", dump(app))) as u16
}

/// Scroll the diff pane horizontally (right = reveal rightward content).
fn hscroll(app: &mut App, right: bool) {
    let area = app.diff_area();
    let kind = if right {
        MouseEventKind::ScrollRight
    } else {
        MouseEventKind::ScrollLeft
    };
    app.on_mouse(mouse(area.x + 2, area.y + 2, kind));
}

/// A long positional line: char at absolute position `p` is the digit `p % 10`,
/// so a horizontal skip is directly readable from the first content column.
fn positional_line(len: usize) -> String {
    (0..len)
        .map(|i| char::from(b'0' + (i % 10) as u8))
        .collect()
}

// The unified content column: 4-digit gutter (10) + sign (2) = 12.
const UNIFIED_CONTENT_X: u16 = 12;

#[test]
fn scroll_right_and_left_shift_code_content_by_the_step() {
    let line = positional_line(200);
    let repo = modified_repo("context\n", &format!("context\n{line}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let row = row_of(&app, "0123456789");
    let area = app.diff_area();
    let cx = area.x + UNIFIED_CONTENT_X;

    let buf = render_buffer(&app, W, H);
    assert_eq!(
        cell_symbol(&buf, cx, row),
        "0",
        "unshifted: content starts at 0"
    );

    // One notch right skips 4 columns: the first content char becomes position 4.
    hscroll(&mut app, true);
    let buf = render_buffer(&app, W, H);
    assert_eq!(
        cell_symbol(&buf, cx, row),
        "4",
        "one notch right skips 4 columns"
    );

    hscroll(&mut app, true);
    let buf = render_buffer(&app, W, H);
    assert_eq!(cell_symbol(&buf, cx, row), "8", "two notches skip 8");

    // Left steps back by the same step.
    hscroll(&mut app, false);
    let buf = render_buffer(&app, W, H);
    assert_eq!(
        cell_symbol(&buf, cx, row),
        "4",
        "one notch left returns to 4"
    );
}

#[test]
fn the_gutter_sign_and_hunk_header_stay_fixed_while_code_shifts() {
    let line = positional_line(200);
    let repo = modified_repo("context\n", &format!("context\n{line}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let code_row = row_of(&app, "0123456789");
    let hunk_row = row_of(&app, "@@");
    let area = app.diff_area();

    let sign_x = area.x + 10; // the '+'/' ' sign column, just before content
    let hunk_x0 = area.x; // the '@' at the start of the hunk header
    let before = render_buffer(&app, W, H);
    assert_eq!(cell_symbol(&before, sign_x, code_row), "+", "addition sign");
    assert_eq!(cell_symbol(&before, hunk_x0, hunk_row), "@", "hunk header");
    // Capture the whole gutter+sign region of the code row.
    let gutter_before: String = (0..UNIFIED_CONTENT_X)
        .map(|dx| cell_symbol(&before, area.x + dx, code_row))
        .collect();
    let hunk_before = dump(&app)
        .lines()
        .nth(hunk_row as usize)
        .unwrap()
        .to_string();

    hscroll(&mut app, true);
    hscroll(&mut app, true);
    let after = render_buffer(&app, W, H);
    let gutter_after: String = (0..UNIFIED_CONTENT_X)
        .map(|dx| cell_symbol(&after, area.x + dx, code_row))
        .collect();
    let hunk_after = dump(&app)
        .lines()
        .nth(hunk_row as usize)
        .unwrap()
        .to_string();

    assert_eq!(gutter_before, gutter_after, "gutter + sign never shift");
    assert_eq!(hunk_before, hunk_after, "the hunk header never shifts");
    // The content did move (sanity that the test exercised a shift).
    assert_eq!(
        cell_symbol(&after, area.x + UNIFIED_CONTENT_X, code_row),
        "8"
    );
}

#[test]
fn scroll_clamps_to_the_longest_code_line() {
    let line = positional_line(60);
    let repo = modified_repo("context\n", &format!("context\n{line}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let _ = dump(&app);

    // Fling far past the end: the offset clamps and never exceeds the longest
    // code line minus the visible content width.
    for _ in 0..50 {
        hscroll(&mut app, true);
    }
    let clamped = app.diff_hscroll;
    assert!(clamped > 0, "some scroll happened");
    // The last content column shows the final char of the line ('9' at pos 59);
    // scrolling further does nothing.
    let before = app.diff_hscroll;
    hscroll(&mut app, true);
    assert_eq!(
        app.diff_hscroll, before,
        "cannot scroll past the longest line"
    );
}

#[test]
fn a_long_hunk_header_does_not_extend_the_clamp() {
    // All code lines fit the (narrow) content width, so the code-based max scroll
    // is 0. The hunk header is longer than the content width — if it wrongly
    // counted, the offset could advance; it must not (plan §3.5).
    let repo = modified_repo("aa\nbb\ncc\n", "aa\nbb\nXX\n"); // 2-char code lines
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    press(&mut app, 'b'); // hide the changes panel so the diff spans a tiny pane
    let narrow = 26u16; // inner ~24 → content ~12; hunk header "@@ -1,3 +1,3 @@" is 15
    let _ = strix::terminal::dump_frame(&app, narrow, H).unwrap();
    let area = app.diff_area();
    app.on_mouse(mouse(area.x + 2, area.y + 2, MouseEventKind::ScrollRight));
    assert_eq!(
        app.diff_hscroll, 0,
        "short code lines + a long hunk header leave nothing to scroll"
    );
}

#[test]
fn hscroll_resets_on_file_change_and_mode_toggle_but_survives_a_refresh() {
    let repo = init_repo();
    let p = repo.path();
    let line = positional_line(120);
    write(p, "a.txt", "one\n");
    write(p, "b.txt", "two\n");
    write(p, "a.txt", &format!("one\n{line}\n"));
    git(p, &["add", "b.txt"]); // both appear in the changes list
    let mut app = App::new(p.to_path_buf()).unwrap();
    let _ = dump(&app);

    // Select a.txt (the file with the long line) via the focused file list.
    press(&mut app, 'h'); // focus the changes list
    let mut guard = 0;
    while app.active_diff_path().as_deref() != Some("a.txt") {
        press(&mut app, 'j');
        guard += 1;
        assert!(guard < 10, "never reached a.txt");
    }
    let _ = dump(&app);
    hscroll(&mut app, true);
    hscroll(&mut app, true);
    assert!(app.diff_hscroll > 0, "a.txt is scrolled");

    // A same-file refresh preserves the offset (diff_dirty path).
    let before = app.diff_hscroll;
    app.reload();
    assert_eq!(
        app.diff_hscroll, before,
        "a same-file refresh keeps the offset"
    );

    // Toggling the diff mode resets it.
    press(&mut app, 'd');
    assert_eq!(app.diff_hscroll, 0, "a mode toggle resets the offset");
    assert_eq!(app.diff_mode, DiffMode::SideBySide);

    // Scroll again, then change the selected file: reset.
    hscroll(&mut app, true);
    assert!(app.diff_hscroll > 0);
    press(&mut app, 'h'); // focus the changes list
    press(&mut app, 'g'); // jump to the top of the list (the other file)
    assert_ne!(
        app.active_diff_path().as_deref(),
        Some("a.txt"),
        "selection moved to a different file"
    );
    assert_eq!(app.diff_hscroll, 0, "a file change resets the offset");
}

#[test]
fn enabling_wrap_resets_and_disables_hscroll() {
    let line = positional_line(200);
    let repo = modified_repo("context\n", &format!("context\n{line}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let row = row_of(&app, "0123456789");
    let area = app.diff_area();
    let cx = area.x + UNIFIED_CONTENT_X;

    hscroll(&mut app, true);
    hscroll(&mut app, true);
    assert!(app.diff_hscroll > 0);

    press(&mut app, 'w'); // enable wrap
    assert_eq!(app.diff_hscroll, 0, "enabling wrap reset the offset");
    let buf = render_buffer(&app, W, H);
    assert_eq!(
        cell_symbol(&buf, cx, row),
        "0",
        "content is back at the line start"
    );

    // While wrap is on, a horizontal scroll is a no-op.
    hscroll(&mut app, true);
    assert_eq!(app.diff_hscroll, 0, "hscroll ignored while wrap is on");
}

#[test]
fn an_open_dropdown_captures_horizontal_scroll() {
    let line = positional_line(200);
    let repo = modified_repo("context\n", &format!("context\n{line}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let _ = dump(&app); // records the menu title rects
                        // Open the View dropdown (mouse-first): click its title at x=8.
    app.on_mouse(mouse(8, 0, MouseEventKind::Down(MouseButton::Left)));
    let _ = dump(&app);
    assert!(app.open_menu.is_some(), "the View menu is open");

    // A horizontal scroll over the diff beneath the open dropdown is inert.
    let area = app.diff_area();
    app.on_mouse(mouse(area.x + 2, area.y + 2, MouseEventKind::ScrollRight));
    assert_eq!(
        app.diff_hscroll, 0,
        "an open dropdown captures the horizontal scroll"
    );
}

#[test]
fn a_scroll_outside_the_diff_area_is_ignored() {
    let line = positional_line(120);
    let repo = modified_repo("context\n", &format!("context\n{line}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let _ = dump(&app);
    let staging = app.staging_area();
    // A horizontal scroll over the changes panel does nothing to the diff.
    app.on_mouse(mouse(
        staging.x + 1,
        staging.y + 1,
        MouseEventKind::ScrollRight,
    ));
    assert_eq!(app.diff_hscroll, 0, "a scroll outside the diff is ignored");
}

#[test]
fn sbs_shifts_both_cells_and_keeps_the_divider_fixed() {
    let old = positional_line(120);
    let new = format!("{}X", positional_line(119)); // both long, differ at the end
    let repo = modified_repo(&format!("{old}\n"), &format!("{new}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    press(&mut app, 'd'); // side-by-side
    let _ = dump(&app);
    let area = app.diff_area();
    let inner = area.width as usize;
    let left_w = (inner - 1) / 2;
    let divider_x = area.x + left_w as u16;

    let buf = render_buffer(&app, W, H);
    // The divider column is the box-drawing bar; find a code row (has a '│').
    let code_row = (area.y..area.y + area.height)
        .find(|&y| cell_symbol(&buf, divider_x, y) == "│")
        .expect("a code row with a divider");
    // Left cell content starts after its 5-col gutter; right after the divider+gutter.
    let left_cx = area.x + 5;
    let right_cx = divider_x + 1 + 5;
    assert_eq!(cell_symbol(&buf, left_cx, code_row), "0", "old cell at 0");
    assert_eq!(cell_symbol(&buf, right_cx, code_row), "0", "new cell at 0");

    hscroll(&mut app, true);
    let buf = render_buffer(&app, W, H);
    assert_eq!(
        cell_symbol(&buf, left_cx, code_row),
        "4",
        "old cell shifted"
    );
    assert_eq!(
        cell_symbol(&buf, right_cx, code_row),
        "4",
        "new cell shifted"
    );
    assert_eq!(
        cell_symbol(&buf, divider_x, code_row),
        "│",
        "the divider never moves"
    );
}

#[test]
fn emphasis_stays_on_the_changed_chars_under_a_skip() {
    // A one-char edit near the START of a long line; scrolling keeps the emphasis
    // aligned to the (now shifted) changed column on both sides.
    let old = format!("ab{}", positional_line(118)); // change is at chars 0..2
    let new = format!("XY{}", positional_line(118));
    let repo = modified_repo(&format!("{old}\n"), &format!("{new}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    press(&mut app, 'd'); // side-by-side
    let _ = dump(&app);
    let del_emph = app.theme.del_emph;
    let add_emph = app.theme.add_emph;
    let area = app.diff_area();
    let inner = area.width as usize;
    let left_w = (inner - 1) / 2;
    let divider_x = area.x + left_w as u16;
    let buf = render_buffer(&app, W, H);
    let code_row = (area.y..area.y + area.height)
        .find(|&y| cell_symbol(&buf, divider_x, y) == "│")
        .expect("a modified pair row");
    let left_cx = area.x + 5;
    let right_cx = divider_x + 1 + 5;

    // Unshifted: the change sits in the first content cell on each side.
    assert_eq!(
        cell_bg(&buf, left_cx, code_row),
        Some(del_emph),
        "old emph at 0"
    );
    assert_eq!(
        cell_bg(&buf, right_cx, code_row),
        Some(add_emph),
        "new emph at 0"
    );

    // Scroll right by one notch (4 cols): the changed chars (0..2) scroll off, so
    // the first cell is now unchanged content — the emphasis moved with the chars,
    // it did not stick to a fixed screen column.
    hscroll(&mut app, true);
    let buf = render_buffer(&app, W, H);
    assert_ne!(
        cell_bg(&buf, left_cx, code_row),
        Some(del_emph),
        "old emphasis scrolled off with its chars"
    );
    assert_ne!(
        cell_bg(&buf, right_cx, code_row),
        Some(add_emph),
        "new emphasis scrolled off with its chars"
    );
}

#[test]
fn emphasis_far_into_a_line_aligns_after_scrolling_to_it() {
    // A change at chars 40..42 of a long line: after scrolling right so column 40
    // is the first visible content column, the emphasis lands exactly there.
    let mut old = positional_line(120);
    let mut new = positional_line(120);
    old.replace_range(40..42, "ab");
    new.replace_range(40..42, "XY");
    let repo = modified_repo(&format!("{old}\n"), &format!("{new}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    press(&mut app, 'd');
    let _ = dump(&app);
    let del_emph = app.theme.del_emph;
    let area = app.diff_area();
    let inner = area.width as usize;
    let left_w = (inner - 1) / 2;
    let divider_x = area.x + left_w as u16;
    let buf = render_buffer(&app, W, H);
    let code_row = (area.y..area.y + area.height)
        .find(|&y| cell_symbol(&buf, divider_x, y) == "│")
        .expect("a modified pair row");
    let left_cx = area.x + 5;

    // 10 notches × 4 = skip 40 columns → char 40 ('a') is the first content cell.
    for _ in 0..10 {
        hscroll(&mut app, true);
    }
    let buf = render_buffer(&app, W, H);
    assert_eq!(app.diff_hscroll, 40, "skipped exactly 40 columns");
    assert_eq!(cell_symbol(&buf, left_cx, code_row), "a", "char 40 is 'a'");
    assert_eq!(
        cell_bg(&buf, left_cx, code_row),
        Some(del_emph),
        "emphasis aligns to the changed char under the skip"
    );
}

#[test]
fn comment_box_rows_do_not_shift_while_code_does() {
    let line = positional_line(120);
    let repo = modified_repo("context\n", &format!("context\n{line}\n"));
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let _ = dump(&app);
    press(&mut app, 'l'); // focus the diff
    press(&mut app, 'G'); // cursor to the last code row (the long addition)
    press(&mut app, 'c'); // open the comment editor on that code line
    for ch in "hi".chars() {
        press(&mut app, ch);
    }
    enter(&mut app); // save the comment
    let _ = dump(&app);

    // The box must actually exist — fail loudly if commenting didn't create one.
    let frame = dump(&app);
    let box_row = frame
        .lines()
        .position(|l| l.contains('╭'))
        .unwrap_or_else(|| panic!("no comment box was created:\n{frame}")) as u16;
    let box_before = frame.lines().nth(box_row as usize).unwrap().to_string();

    hscroll(&mut app, true);
    hscroll(&mut app, true);
    let box_after = dump(&app)
        .lines()
        .nth(box_row as usize)
        .unwrap()
        .to_string();
    assert_eq!(box_before, box_after, "the comment box never shifts");
}

#[test]
fn history_diff_pane_scrolls_horizontally() {
    let repo = init_repo();
    let p = repo.path();
    let line = positional_line(120);
    write(p, "code.txt", &format!("{line}\n"));
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add long line"]);
    let mut app = App::new(p.to_path_buf()).unwrap();
    press(&mut app, 'i'); // enter History
    let _ = dump(&app);
    // Focus the committed-changes list, step onto the file row (below the `●`
    // commit row), then focus the diff pane.
    press(&mut app, 'l'); // focus committed changes
    press(&mut app, 'j'); // move off the commit row onto the file
    press(&mut app, 'l'); // focus the diff
    let mut guard = 0;
    while !dump(&app).contains("0123456789") {
        press(&mut app, 'j');
        guard += 1;
        assert!(
            guard < 10,
            "History diff never showed the long line:\n{}",
            dump(&app)
        );
    }
    assert!(
        app.active_diff_path().as_deref() == Some("code.txt"),
        "the committed file's diff is showing"
    );
    hscroll(&mut app, true);
    assert!(
        app.diff_hscroll > 0,
        "History's diff pane scrolls horizontally"
    );
}
