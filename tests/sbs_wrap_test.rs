//! Side-by-side line wrap (plan §3.3, C4). A pair's two cells wrap independently
//! within their column; the pair occupies `max(left_rows, right_rows)` physical
//! rows sharing one `RowTarget`. The pinned two-blank distinction: an *absent*
//! side renders `filler_bg`, an *exhausted* side (its line ran out of segments
//! before the taller side) renders blank in that line's own add/del/context
//! background. Word-diff emphasis (absolute char offsets) still lands on the
//! right chars on every subrow. Colours aren't in `dump_frame`'s glyph text, so
//! colour assertions read the rendered `Buffer` (`render_buffer`/`cell_bg`/
//! `row_has_bg`), mirroring `split_view_emph_test`.

mod common;

use common::{cell_bg, cell_symbol, git, init_repo, press, render_buffer, row_has_bg, write};
use strix::app::{App, RowContent, RowTarget};
use strix::crossterm::event::{KeyCode, KeyEvent};
use tempfile::TempDir;

const W: u16 = 100;
const H: u16 = 30;

/// One inspected diff-layout row: its index, subrow, and (for a pair) each side's
/// line index plus whether that subrow draws a segment (`Some(true)`), is an
/// exhausted blank (`Some(false)`), or is an absent side (`None`).
#[derive(Debug, Clone)]
struct RowInfo {
    index: usize,
    subrow: usize,
    target: RowTarget,
    is_pair: bool,
    left: Option<(usize, bool)>,
    right: Option<(usize, bool)>,
    box_side_new: bool,
}

/// A repo with `code.txt` committed as `before`, then edited (uncommitted) to
/// `after` — the sole auto-selected Status entry (mirrors split_view_emph).
fn modified_repo(before: &str, after: &str) -> TempDir {
    let repo = init_repo();
    let path = repo.path();
    write(path, "code.txt", before);
    git(path, &["add", "code.txt"]);
    git(path, &["commit", "-q", "-m", "add file"]);
    write(path, "code.txt", after);
    repo
}

/// A repo with a >10000-line file whose tail line is edited into a long line, so
/// its diff carries 5-digit line numbers (a 6-col gutter) and a wrapping line.
fn five_digit_repo() -> TempDir {
    let repo = init_repo();
    let p = repo.path();
    let mut base = String::new();
    for i in 1..=10005 {
        base.push_str(&format!("line {i}\n"));
    }
    write(p, "big.txt", &base);
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add big.txt"]);
    let mut edited = String::new();
    for i in 1..=10004 {
        edited.push_str(&format!("line {i}\n"));
    }
    edited.push_str(&"z".repeat(120));
    edited.push('\n');
    write(p, "big.txt", &edited);
    repo
}

/// Status app in side-by-side mode with wrap on and the diff focused.
fn sbs_wrap_app(repo: &std::path::Path) -> App {
    let mut app = App::new(repo.to_path_buf()).unwrap();
    press(&mut app, 'd'); // side-by-side
    press(&mut app, 'w'); // wrap on
    press(&mut app, 'l'); // focus the diff pane
    app
}

/// Collect the current diff layout into owned `RowInfo`s (so the `Ref` is dropped
/// before any buffer read).
fn rows(app: &App) -> Vec<RowInfo> {
    let w = app.diff_area().width;
    app.diff_layout(w)
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let (is_pair, left, right) = match &row.content {
                RowContent::Pair { left, right, .. } => (
                    true,
                    left.as_ref().map(|c| (c.line, c.seg.is_some())),
                    right.as_ref().map(|c| (c.line, c.seg.is_some())),
                ),
                _ => (false, None, None),
            };
            let box_side_new = matches!(&row.content, RowContent::Box(_))
                && row.side == Some(strix::comments::Side::New);
            RowInfo {
                index,
                subrow: row.subrow,
                target: row.target,
                is_pair,
                left,
                right,
                box_side_new,
            }
        })
        .collect()
}

/// The subrows of the one modified pair (a pair whose two sides name different
/// diff lines), in layout order.
fn modified_pair(app: &App) -> Vec<RowInfo> {
    rows(app)
        .into_iter()
        .filter(|r| match (r.left, r.right) {
            (Some((l, _)), Some((rr, _))) => l != rr,
            _ => false,
        })
        .collect()
}

#[test]
fn an_exhausted_side_of_a_taller_pair_keeps_its_line_background() {
    // A rewrite whose NEW side is much longer than the OLD: the new side wraps
    // into several subrows while the old side is exhausted after the first.
    let repo = modified_repo(
        "keep this context line\nlet total = sum();\n",
        "keep this context line\nlet total = accumulate_every_single_value_across_the_whole_list();\n",
    );
    let app = sbs_wrap_app(repo.path());
    let buf = render_buffer(&app, W, H);
    let area = app.diff_area();
    let del_bg = app.theme.del_bg;
    let filler = app.theme.filler_bg;

    let pair = modified_pair(&app);
    assert!(
        pair.len() >= 2,
        "the new side wrapped into several subrows: {pair:?}"
    );
    // First subrow: both sides present.
    assert_eq!(pair[0].subrow, 0);
    assert_eq!(
        pair[0].left.map(|(_, s)| s),
        Some(true),
        "old present on subrow 0"
    );
    assert_eq!(
        pair[0].right.map(|(_, s)| s),
        Some(true),
        "new present on subrow 0"
    );
    // A later subrow where the old side is exhausted (blank) but the new isn't.
    let exhausted = pair
        .iter()
        .find(|r| {
            r.left == Some((r.left.unwrap().0, false)) && r.right.map(|(_, s)| s) == Some(true)
        })
        .expect("an exhausted-old subrow");
    let y = area.y + exhausted.index as u16;
    // A content cell in the left (old) column carries the OLD line's own bg
    // (del_bg) — the exhausted-vs-absent distinction — never the neutral filler.
    let x = area.x + 8; // inside the left column's content area
    assert_eq!(
        cell_bg(&buf, x, y),
        Some(del_bg),
        "exhausted old side keeps its del_bg, not filler"
    );
    assert_ne!(cell_bg(&buf, x, y), Some(filler), "exhausted != absent");
}

#[test]
fn an_absent_side_of_a_wrapped_pure_addition_stays_filler() {
    // A pure addition (new lines with no paired deletion): the OLD column is
    // absent on every subrow and must stay filler_bg, including continuations.
    let long = "added ".repeat(20); // ~120 cols, wraps in a column
    let repo = modified_repo("context\n", &format!("context\n{long}\n"));
    let app = sbs_wrap_app(repo.path());
    let buf = render_buffer(&app, W, H);
    let area = app.diff_area();
    let filler = app.theme.filler_bg;

    // The pure addition is a pair with an absent (None) left side that wraps.
    let added: Vec<RowInfo> = rows(&app)
        .into_iter()
        .filter(|r| r.is_pair && r.left.is_none() && r.right.map(|(_, s)| s) == Some(true))
        .collect();
    assert!(added.len() >= 2, "the addition wrapped: {added:?}");
    let x = area.x + 8; // left column content
    for r in &added {
        let y = area.y + r.index as u16;
        assert_eq!(
            cell_bg(&buf, x, y),
            Some(filler),
            "absent old side is filler on subrow {}",
            r.subrow
        );
    }
}

#[test]
fn word_emphasis_lands_on_a_continuation_subrow_on_both_sides() {
    // Two long lines differing only in the final word, so the change wraps onto a
    // continuation subrow. Emphasis (absolute char offsets) must still paint the
    // changed chars there, on both sides.
    let old = "alpha beta gamma delta epsilon zeta eta theta iota kappa OLDWORD";
    let new = "alpha beta gamma delta epsilon zeta eta theta iota kappa NEWWORD";
    let repo = modified_repo(&format!("{old}\n"), &format!("{new}\n"));
    let app = sbs_wrap_app(repo.path());
    let frame = strix::terminal::dump_frame(&app, W, H).unwrap();
    let buf = render_buffer(&app, W, H);
    let area = app.diff_area();
    let del_emph = app.theme.del_emph;
    let add_emph = app.theme.add_emph;

    let pair = modified_pair(&app);
    assert!(pair.len() >= 2, "both sides wrapped: {pair:?}");
    // The last subrow (carrying the trailing changed word) is a continuation.
    let last = pair.last().unwrap();
    assert!(last.subrow > 0, "the change is on a continuation subrow");
    let y = area.y + last.index as u16;
    assert!(
        row_has_bg(&buf, y, del_emph),
        "old side emphasizes the changed word on the continuation:\n{frame}"
    );
    assert!(
        row_has_bg(&buf, y, add_emph),
        "new side emphasizes the changed word on the continuation:\n{frame}"
    );
}

#[test]
fn wide_chars_at_a_narrow_sbs_column_boundary_lose_no_content() {
    // A pure addition of 40 double-width chars: in a narrow column each wraps,
    // and a wide char that doesn't fit a subrow's tail moves whole to the next.
    let wide = "界".repeat(40);
    let repo = modified_repo("context\n", &format!("context\n{wide}\n"));
    let app = sbs_wrap_app(repo.path());
    let buf = render_buffer(&app, W, H); // render first to fix the pane geometry
    let area = app.diff_area();

    // Layout-level: the added line's segments are contiguous and cover [0, 40).
    let added: Vec<RowInfo> = rows(&app)
        .into_iter()
        .filter(|r| r.is_pair && r.left.is_none() && r.right.map(|(_, s)| s) == Some(true))
        .collect();
    assert!(added.len() >= 2, "the wide line wrapped: {added:?}");
    let target = added[0].target;
    let w = area.width;
    let layout = app.diff_layout(w);
    let mut cursor = 0usize;
    let mut covered = 0usize;
    for row in layout.iter() {
        if let RowContent::Pair { right: Some(c), .. } = &row.content {
            if RowTarget::Code(c.line) == target {
                if let Some(seg) = c.seg {
                    assert_eq!(seg.start_char, cursor, "segments contiguous: {seg:?}");
                    covered += seg.end_char - seg.start_char;
                    cursor = seg.end_char;
                }
            }
        }
    }
    drop(layout);
    assert_eq!(covered, 40, "every wide char is placed in some segment");

    // Render-level: all 40 glyphs are actually drawn (none clipped at an edge).
    let mut count = 0;
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if cell_symbol(&buf, x, y) == "界" {
                count += 1;
            }
        }
    }
    assert_eq!(count, 40, "no wide char clipped at a column boundary");
}

#[test]
fn j_crosses_a_wrapped_pair_in_one_step_and_highlights_the_whole_pair() {
    let old = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
    let new = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda XX";
    // A trailing unchanged line gives `j` a target to cross onto past the pair.
    let repo = modified_repo(
        &format!("ctx\n{old}\ntail\n"),
        &format!("ctx\n{new}\ntail\n"),
    );
    let mut app = sbs_wrap_app(repo.path());
    let _ = render_buffer(&app, W, H);

    let pair = modified_pair(&app);
    assert!(pair.len() >= 2, "the pair wrapped: {pair:?}");
    let target = pair[0].target;

    // Move the cursor down onto the wrapped pair.
    let mut guard = 0;
    while cursor_target(&app) != Some(target) {
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        guard += 1;
        assert!(guard < 40, "never reached the wrapped pair");
    }
    // The highlight spans every physical subrow of the pair.
    let span = app.review_cursor_highlight().expect("a highlight span");
    assert_eq!(
        span.end - span.start,
        pair.len(),
        "the whole wrapped pair highlights as one unit"
    );
    // One more `j` crosses the entire wrapped pair in a single step.
    let last_index = pair.last().unwrap().index;
    app.on_key(KeyEvent::from(KeyCode::Char('j')));
    assert_ne!(cursor_target(&app), Some(target), "one `j` left the pair");
    assert!(
        app.review_cursor() > last_index,
        "the cursor advanced past every subrow of the wrapped pair"
    );
}

#[test]
fn a_comment_box_on_a_wrapped_sbs_line_lands_after_the_last_subrow() {
    let long = "value ".repeat(20);
    let repo = modified_repo("ctx\n", &format!("ctx\n{long}\n"));
    let mut app = sbs_wrap_app(repo.path());
    let _ = render_buffer(&app, W, H);

    // Move onto the wrapped addition and add a comment.
    let added: Vec<RowInfo> = rows(&app)
        .into_iter()
        .filter(|r| r.is_pair && r.left.is_none() && r.right.map(|(_, s)| s) == Some(true))
        .collect();
    let target = added[0].target;
    let mut guard = 0;
    while cursor_target(&app) != Some(target) {
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        guard += 1;
        assert!(guard < 40, "never reached the addition");
    }
    app.on_key(KeyEvent::from(KeyCode::Char('c')));
    for ch in "note".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(ch)));
    }
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let _ = render_buffer(&app, W, H);

    // The addition's Pair subrows are contiguous; the box rows follow the last
    // one, in the new-side column.
    let all = rows(&app);
    let pair_positions: Vec<usize> = all
        .iter()
        .filter(|r| r.is_pair && r.target == target)
        .map(|r| r.index)
        .collect();
    assert!(pair_positions.len() >= 2, "the line stayed wrapped");
    let last = *pair_positions.last().unwrap();
    assert_eq!(
        pair_positions,
        (pair_positions[0]..=last).collect::<Vec<_>>(),
        "the pair's subrows are contiguous"
    );
    // The next row is a comment box in the new-side column.
    let after = &all[last + 1];
    assert!(!after.is_pair, "a box, not another pair subrow, follows");
    assert!(
        after.box_side_new,
        "the box occupies the anchor (new) column"
    );
}

#[test]
fn five_digit_line_numbers_do_not_clip_wrapped_sbs_content() {
    // A >10000-line file edited at the tail into a long line: 5-digit numbers and
    // wrapping, the same clip hazard fixed for unified now covered for sbs.
    let repo = init_repo();
    let p = repo.path();
    let mut base = String::new();
    for i in 1..=10005 {
        base.push_str(&format!("line {i}\n"));
    }
    write(p, "big.txt", &base);
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add big.txt"]);
    let mut edited = String::new();
    for i in 1..=10004 {
        edited.push_str(&format!("line {i}\n"));
    }
    edited.push_str(&"z".repeat(120));
    edited.push('\n');
    write(p, "big.txt", &edited);

    let app = sbs_wrap_app(p);
    let frame = strix::terminal::dump_frame(&app, W, H).unwrap();
    let area = app.diff_area();
    assert!(
        frame.contains("10005"),
        "a 5-digit line number is shown:\n{frame}"
    );

    // The 120-char new line's segments cover it fully (contiguous, no gap).
    let added: Vec<RowInfo> = rows(&app)
        .into_iter()
        .filter(|r| r.is_pair && r.right.map(|(_, s)| s) == Some(true) && r.subrow == 0)
        .collect();
    // Find the tail (long) addition target: the pair with the most subrows.
    let all = rows(&app);
    let target = all
        .iter()
        .filter(|r| r.is_pair)
        .max_by_key(|r| {
            all.iter()
                .filter(|x| x.is_pair && x.target == r.target)
                .count()
        })
        .map(|r| r.target)
        .expect("a pair");
    let _ = added;
    let w = area.width;
    let layout = app.diff_layout(w);
    let mut cursor = 0usize;
    let mut covered = 0usize;
    for row in layout.iter() {
        if let RowContent::Pair { right: Some(c), .. } = &row.content {
            if RowTarget::Code(c.line) == target {
                if let Some(seg) = c.seg {
                    covered += seg.end_char - seg.start_char;
                    cursor = seg.end_char;
                }
            }
        }
    }
    let _ = cursor;
    drop(layout);
    assert_eq!(
        covered, 120,
        "the wrapped tail line loses no content under a 5-digit gutter"
    );

    // Render-level: every `z` is drawn.
    let buf = render_buffer(&app, W, H);
    let mut count = 0;
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if cell_symbol(&buf, x, y) == "z" {
                count += 1;
            }
        }
    }
    assert_eq!(
        count, 120,
        "no wrapped content clipped by the wider sbs gutter"
    );
}

#[test]
fn a_narrow_cell_gutter_never_pushes_the_divider_off_left_w() {
    // 5-digit numbers → a 6-col gutter, rendered at a pane so narrow the left
    // cell is only ~5 cols. The gutter must clip to the cell, never overrun and
    // shift the centre divider past left_w (BUG 1).
    let repo = five_digit_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    press(&mut app, 'd'); // side-by-side
    press(&mut app, 'b'); // hide the changes panel so the diff spans the pane
    press(&mut app, 'w'); // wrap
    let narrow = 14u16;
    let buf = render_buffer(&app, narrow, H);
    let area = app.diff_area();
    let inner = area.width as usize;
    let left_w = (inner - 1) / 2;
    // The bug is only reachable when the cell is narrower than its 6-col gutter.
    assert!(
        left_w < 6,
        "the left cell ({left_w}) must be narrower than the 6-col gutter to test the fix"
    );

    // Every visible side-by-side Pair row draws its divider at exactly left_w:
    // the left cell emitted exactly left_w columns despite its wider gutter.
    let pair_rows: Vec<usize> = {
        let layout = app.diff_layout(area.width);
        layout
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                matches!(r.content, RowContent::Pair { .. }) && *i < area.height as usize
            })
            .map(|(i, _)| i)
            .collect()
    };
    assert!(!pair_rows.is_empty(), "some pair rows are visible");
    for i in pair_rows {
        let y = area.y + i as u16;
        assert_eq!(
            cell_symbol(&buf, area.x + left_w as u16, y),
            "│",
            "the divider stays at left_w={left_w} on row {i}"
        );
    }
}

#[test]
fn a_zero_content_width_cell_is_one_subrow_not_one_per_char() {
    // A pane so narrow the present cell's gutter eats its whole width (content
    // width 0): a non-empty line must be exactly ONE subrow, not one blank row
    // per char (BUG 2 — wrap_segments' ≥1-char floor vs a 0-col render).
    let repo = modified_repo("ctx\n", "ctx\nabcd\n");
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    press(&mut app, 'd'); // side-by-side
    press(&mut app, 'b'); // hide the changes panel
    press(&mut app, 'w'); // wrap
    let _ = render_buffer(&app, 13, H);
    let area = app.diff_area();
    let inner = area.width as usize;
    let right_w = inner - (inner - 1) / 2 - 1;
    // 4-digit numbers here → a 5-col gutter; the right cell must be that narrow so
    // its content width is 0 (the addition renders on the right side).
    assert_eq!(
        right_w, 5,
        "the right cell must equal the 5-col gutter for a 0 content width"
    );

    // The pure addition ("abcd") is a single subrow — its 4 chars did not explode
    // into 4 blank rows.
    let added: Vec<RowInfo> = rows(&app)
        .into_iter()
        .filter(|r| r.is_pair && r.left.is_none() && r.right.is_some())
        .collect();
    assert_eq!(added.len(), 1, "one subrow, not one per char: {added:?}");
    assert_eq!(added[0].subrow, 0);

    // And nothing of the line is drawn (content width really is 0), so no glyph
    // leaked past the gutter into the next cell.
    let frame = strix::terminal::dump_frame(&app, 13, H).unwrap();
    let added_row = area.y + added[0].index as u16;
    let line = frame.lines().nth(added_row as usize).unwrap_or("");
    assert!(
        !line.contains("abcd") && !line.contains('a'),
        "a 0-width cell draws no content: {line:?}"
    );
}

/// The logical target the diff cursor addresses, read back through the layout.
fn cursor_target(app: &App) -> Option<RowTarget> {
    let w = app.diff_area().width;
    let idx = app.review_cursor();
    app.diff_layout(w).get(idx).map(|r| r.target)
}
