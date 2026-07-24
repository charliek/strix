//! Unified line-wrap (plan §3.3, C3). A long diff line becomes several physical
//! display rows sharing one `RowTarget::Code` with incrementing subrows, so the
//! cursor, whole-unit highlight, click routing, and comment anchoring all treat
//! a wrapped line as one unit. Toggling wrap / line numbers and resizing keep the
//! top visible logical line and the cursor target put. Wrap off is the same code
//! path with a single full-width segment per line.
//!
//! Harness mirrors the other review-view suites: build via `App::for_review`,
//! `dump_frame` to record geometry, drive `on_key`/`on_mouse`, and read the
//! exposed cursor/layout/scroll state.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{cell_symbol, git, init_repo, render_buffer, write};
use strix::app::{App, RowContent, RowTarget};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

const W: u16 = 100;
const H: u16 = 30;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn dump(app: &App) -> String {
    strix::terminal::dump_frame(app, W, H).unwrap()
}

fn review(repo: &Path, range: &str) -> App {
    App::for_review(repo.to_path_buf(), &Config::default(), range).unwrap()
}

/// A repo whose `feature` branch adds a file with a mix of short lines and one
/// very long line (250 `x`s), so its unified diff has a line that must wrap at
/// any reasonable pane width.
fn long_line_repo() -> tempfile::TempDir {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["checkout", "-qb", "feature"]);
    let long: String = "x".repeat(250);
    let content = format!("short one\n{long}\nshort two\nshort three\n");
    write(p, "wrap.txt", &content);
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add wrap.txt"]);
    dir
}

/// A repo whose `feature` branch edits the last line of a >10000-line file into
/// a 250-char line, so the diff carries 5-digit line numbers *and* a wrapping
/// line — the case where a fixed 4-wide gutter would clip content under wrap.
fn five_digit_repo() -> tempfile::TempDir {
    let dir = init_repo();
    let p = dir.path();
    let mut base = String::new();
    for i in 1..=10005 {
        base.push_str(&format!("line {i}\n"));
    }
    write(p, "big.txt", &base);
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add big.txt"]);

    git(p, &["checkout", "-qb", "feature"]);
    let long = "z".repeat(250);
    let mut edited = String::new();
    for i in 1..=10004 {
        edited.push_str(&format!("line {i}\n"));
    }
    edited.push_str(&format!("{long}\n"));
    write(p, "big.txt", &edited);
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "edit tail"]);
    dir
}

/// The physical diff layout at the last-rendered pane width.
fn layout_targets(app: &App) -> Vec<(RowTarget, usize, bool)> {
    let w = app.diff_area().width;
    app.diff_layout(w)
        .iter()
        .map(|r| {
            (
                r.target,
                r.subrow,
                matches!(r.content, RowContent::Line { .. }),
            )
        })
        .collect()
}

/// The logical target the diff cursor addresses, read back through the layout
/// (`review_cursor` gives the target's first physical row).
fn cursor_target(app: &App) -> Option<RowTarget> {
    let w = app.diff_area().width;
    let idx = app.review_cursor();
    app.diff_layout(w).get(idx).map(|r| r.target)
}

/// The `(code_index, subrows)` runs of consecutive `Line` rows sharing a target.
fn code_runs(app: &App) -> Vec<(usize, Vec<usize>)> {
    let w = app.diff_area().width;
    let layout = app.diff_layout(w);
    let mut runs: Vec<(usize, Vec<usize>)> = Vec::new();
    for row in layout.iter() {
        if let RowContent::Line { line, .. } = row.content {
            match runs.last_mut() {
                Some((idx, subs)) if *idx == line => subs.push(row.subrow),
                _ => runs.push((line, vec![row.subrow])),
            }
        }
    }
    runs
}

/// Focus the review diff pane with the changes panel hidden (so the diff spans
/// nearly the whole width) and wrap enabled.
fn wrapped_review(repo: &Path) -> App {
    let mut app = review(repo, "main");
    let _ = dump(&app);
    app.on_key(key('b')); // hide the file list, focus the diff
    app.on_key(key('w')); // enable wrap
    let _ = dump(&app);
    app
}

#[test]
fn long_line_wraps_into_rows_sharing_target_with_incrementing_subrows() {
    let repo = long_line_repo();
    let app = wrapped_review(repo.path());

    let runs = code_runs(&app);
    // Exactly one code line (the 250-x line) wraps into several rows; the short
    // lines stay single-row.
    let wrapped: Vec<&(usize, Vec<usize>)> = runs.iter().filter(|(_, s)| s.len() > 1).collect();
    assert_eq!(wrapped.len(), 1, "only the long line wraps: {runs:?}");
    let (_, subrows) = wrapped[0];
    assert!(
        subrows.len() >= 3,
        "250 cols wrap into several rows: {subrows:?}"
    );
    let expected: Vec<usize> = (0..subrows.len()).collect();
    assert_eq!(*subrows, expected, "subrows increment from 0 with no gaps");
}

#[test]
fn five_digit_line_numbers_do_not_clip_wrapped_content() {
    let repo = five_digit_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('b')); // hide the file list, focus the diff
    app.on_key(key('w')); // enable wrap
    let _ = dump(&app);

    // The scenario is real: the gutter carries a 5-digit number.
    let frame = dump(&app);
    assert!(
        frame.contains("10005"),
        "5-digit line number is shown:\n{frame}"
    );

    // The 250-char line wraps into several rows.
    let long_run = code_runs(&app)
        .into_iter()
        .find(|(_, s)| s.len() > 1)
        .expect("the long line wraps");
    let subrows = long_run.1.len();
    assert!(subrows >= 2, "line wraps into several rows: {subrows}");

    // Layout-level: the wrapped segments are contiguous and cover the whole line
    // `[0, 250)` with no gap or overlap — the wrap width the gutter reserved is
    // the width content was cut at, so nothing falls between segments.
    let w = app.diff_area().width;
    let layout = app.diff_layout(w);
    let long_target = RowTarget::Code(long_run.0);
    let mut cursor = 0usize;
    let mut covered = 0usize;
    for row in layout.iter() {
        if let RowContent::Line { line, seg } = row.content {
            if RowTarget::Code(line) == long_target {
                assert_eq!(seg.start_char, cursor, "segments are contiguous: {seg:?}");
                assert!(seg.end_char > seg.start_char, "each segment advances");
                covered += seg.end_char - seg.start_char;
                cursor = seg.end_char;
            }
        }
    }
    drop(layout);
    assert_eq!(
        covered, 250,
        "the wrapped segments cover every char of the line"
    );

    // Render-level: every one of the 250 `z`s is actually drawn — a clipped
    // gutter would have swallowed a couple of columns per row (< 250).
    let area = app.diff_area();
    let buf = render_buffer(&app, W, H);
    let mut zs = 0;
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if cell_symbol(&buf, x, y) == "z" {
                zs += 1;
            }
        }
    }
    assert_eq!(zs, 250, "no wrapped content clipped by the wider gutter");
}

#[test]
fn wrap_off_is_a_single_full_width_segment_per_line() {
    let repo = long_line_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('b')); // focus the diff, wrap still off (default)
    let _ = dump(&app);

    // Every code line is exactly one row, subrow 0 — the wrap-off code path.
    for (_, subrows) in code_runs(&app) {
        assert_eq!(subrows, vec![0], "wrap off: one row per line");
    }
}

#[test]
fn continuation_rows_have_blank_gutters() {
    let repo = long_line_repo();
    let app = wrapped_review(repo.path());
    let area = app.diff_area();
    let buf = render_buffer(&app, W, H);

    // Find the long line's run and its first continuation row's layout index
    // (offset is 0 with the cursor at the top).
    let w = area.width;
    let layout = app.diff_layout(w);
    let mut first_index = None;
    let mut cont_index = None;
    for (i, row) in layout.iter().enumerate() {
        if let RowContent::Line { .. } = row.content {
            if row.subrow == 0 {
                first_index = Some(i);
            } else if cont_index.is_none() && first_index.is_some() {
                // The first subrow>0 that follows a subrow-0 of the *same* run.
                cont_index = Some(i);
                break;
            }
        }
    }
    drop(layout);
    let cont = cont_index.expect("a continuation row exists");

    // The gutter (10 cols) on a continuation row is all blanks — no line numbers.
    let y = area.y + cont as u16;
    for dx in 0..10u16 {
        assert_eq!(
            cell_symbol(&buf, area.x + dx, y),
            " ",
            "continuation gutter col {dx} is blank"
        );
    }
    // The line's first subrow *does* carry a number, proving the blank is the
    // continuation's doing, not a global gutter-off.
    let first = first_index.expect("a first row exists");
    let fy = area.y + first as u16;
    let gutter: String = (0..10u16)
        .map(|dx| cell_symbol(&buf, area.x + dx, fy))
        .collect();
    assert!(
        gutter.trim().chars().any(|c| c.is_ascii_digit()),
        "the first subrow carries a line number: {gutter:?}"
    );
}

#[test]
fn j_crosses_a_wrapped_line_in_one_step() {
    let repo = long_line_repo();
    let mut app = wrapped_review(repo.path());

    // Cursor starts on the hunk header (row 0). Step down onto the first short
    // line, then onto the long wrapped line, then one more `j` must land on the
    // *next* logical line — not on the long line's second subrow.
    let targets = layout_targets(&app);
    // Walk to the long-line target.
    let long_run = code_runs(&app)
        .into_iter()
        .find(|(_, s)| s.len() > 1)
        .expect("long run");
    let long_target = RowTarget::Code(long_run.0);

    // Move the cursor down until it reaches the long line.
    let mut guard = 0;
    while cursor_target(&app) != Some(long_target) {
        app.on_key(key('j'));
        guard += 1;
        assert!(guard < 50, "never reached the long line");
    }
    let at_long = app.review_cursor();
    // One more `j` crosses the whole wrapped unit in a single step.
    app.on_key(key('j'));
    let after = cursor_target(&app);
    assert_ne!(after, Some(long_target), "one `j` left the wrapped line");
    // And it advanced past *all* of the long line's physical rows.
    let long_rows: Vec<usize> = targets
        .iter()
        .enumerate()
        .filter(|(_, (t, _, _))| *t == long_target)
        .map(|(i, _)| i)
        .collect();
    let last_long_row = *long_rows.last().unwrap();
    assert!(
        app.review_cursor() > last_long_row,
        "cursor moved past the last subrow (was {at_long}, last long row {last_long_row})"
    );
}

#[test]
fn whole_wrapped_line_highlights_as_one_unit() {
    let repo = long_line_repo();
    let mut app = wrapped_review(repo.path());
    let long_run = code_runs(&app)
        .into_iter()
        .find(|(_, s)| s.len() > 1)
        .expect("long run");
    let long_target = RowTarget::Code(long_run.0);
    let mut guard = 0;
    while cursor_target(&app) != Some(long_target) {
        app.on_key(key('j'));
        guard += 1;
        assert!(guard < 50);
    }
    // The highlight span covers every physical row of the wrapped line.
    let span = app.review_cursor_highlight().expect("a highlight span");
    assert_eq!(
        span.end - span.start,
        long_run.1.len(),
        "the highlight spans all subrows of the wrapped line"
    );
}

#[test]
fn click_and_double_click_on_a_continuation_row_resolve_to_the_line_target() {
    let repo = long_line_repo();
    let mut app = wrapped_review(repo.path());
    let area = app.diff_area();

    let w = area.width;
    let (first_idx, cont_idx, target) = {
        let layout = app.diff_layout(w);
        let mut first = None;
        let mut cont = None;
        let mut tgt = None;
        for (i, row) in layout.iter().enumerate() {
            if let RowContent::Line { .. } = row.content {
                if row.subrow == 0 {
                    first = Some(i);
                    tgt = Some(row.target);
                } else if cont.is_none() && first.is_some() {
                    cont = Some(i);
                    break;
                }
            }
        }
        (first.unwrap(), cont.unwrap(), tgt.unwrap())
    };

    let _ = first_idx; // both subrows share the target; we drive the continuation
    let down = |row: usize| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 12,
        row: area.y + row as u16,
        modifiers: strix::crossterm::event::KeyModifiers::NONE,
    };

    // A single click on a continuation row lands the cursor on the whole line's
    // target (its first physical row) without opening the editor.
    let t0 = Instant::now();
    app.on_mouse_at(down(cont_idx), t0);
    assert_eq!(
        cursor_target(&app),
        Some(target),
        "single click hits the line"
    );
    assert!(!app.editor_open(), "a single click doesn't open the editor");

    // A double click on the same continuation row resolves to the same Code
    // target and opens the in-place editor anchored there.
    let t1 = t0 + Duration::from_secs(5); // far apart: a fresh first click
    app.on_mouse_at(down(cont_idx), t1);
    app.on_mouse_at(down(cont_idx), t1 + Duration::from_millis(40));
    assert!(
        app.editor_open(),
        "a double click on a continuation opens the editor"
    );
    assert_eq!(
        cursor_target(&app),
        Some(target),
        "the double-click editor anchors on the line target"
    );
}

#[test]
fn a_comment_box_on_a_wrapped_line_inserts_after_its_last_subrow() {
    let repo = long_line_repo();
    let mut app = wrapped_review(repo.path());
    let long_run = code_runs(&app)
        .into_iter()
        .find(|(_, s)| s.len() > 1)
        .expect("long run");
    let long_target = RowTarget::Code(long_run.0);
    let mut guard = 0;
    while cursor_target(&app) != Some(long_target) {
        app.on_key(key('j'));
        guard += 1;
        assert!(guard < 50);
    }
    // Open the editor on the wrapped line, type, and save.
    app.on_key(key('c'));
    for ch in "note".chars() {
        app.on_key(key(ch));
    }
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let _ = dump(&app);

    // The editor/box rows appear immediately after the long line's last subrow —
    // i.e. all of the long line's `Line` rows are contiguous, then a non-`Line`
    // row (the comment box) follows.
    let w = app.diff_area().width;
    let layout = app.diff_layout(w);
    let long_positions: Vec<usize> = layout
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r.content, RowContent::Line { line, .. } if RowTarget::Code(line) == long_target))
        .map(|(i, _)| i)
        .collect();
    let last = *long_positions.last().unwrap();
    // Contiguous run.
    assert_eq!(
        long_positions,
        (long_positions[0]..=last).collect::<Vec<_>>(),
        "the wrapped line's rows are contiguous"
    );
    // The row right after is not part of the long line (it's the comment box).
    assert!(
        !matches!(layout[last + 1].content, RowContent::Line { line, .. } if RowTarget::Code(line) == long_target),
        "the box inserts after the last subrow"
    );
    assert!(matches!(layout[last + 1].content, RowContent::Box(_)));
}

/// The logical line whose text sits at the top of the diff viewport.
fn top_line_index(app: &App) -> Option<usize> {
    let w = app.diff_area().width;
    let top = app.diff_scroll.get().min(app.diff_max_scroll());
    app.diff_layout(w).get(top).and_then(|r| match r.content {
        RowContent::Line { line, .. } => Some(line),
        _ => None,
    })
}

#[test]
fn toggling_wrap_preserves_the_top_line_and_the_cursor() {
    let repo = long_line_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('b')); // focus diff
                          // Scroll down a few code lines so the top isn't row 0.
    for _ in 0..2 {
        app.on_key(key('j'));
    }
    let _ = dump(&app);
    let top_before = top_line_index(&app);
    let cursor_before = cursor_target(&app);
    assert!(top_before.is_some());

    app.on_key(key('w')); // enable wrap — a structural relayout
    let _ = dump(&app);
    assert_eq!(
        top_line_index(&app),
        top_before,
        "the top logical line survives the wrap toggle"
    );
    assert_eq!(
        cursor_target(&app),
        cursor_before,
        "the cursor's logical target survives the wrap toggle"
    );
}

#[test]
fn toggling_line_numbers_while_wrapped_preserves_the_top_line() {
    let repo = long_line_repo();
    let mut app = wrapped_review(repo.path());
    for _ in 0..3 {
        app.on_key(key('j'));
    }
    let _ = dump(&app);
    let top_before = top_line_index(&app);
    let cursor_before = cursor_target(&app);

    app.on_key(key('n')); // gutter width change -> re-wrap -> re-anchor
    let _ = dump(&app);
    assert_eq!(top_line_index(&app), top_before, "top line survives `n`");
    assert_eq!(cursor_target(&app), cursor_before, "cursor survives `n`");
}

#[test]
fn a_width_resize_preserves_the_top_line_while_wrapped() {
    let repo = long_line_repo();
    let mut app = wrapped_review(repo.path());
    for _ in 0..3 {
        app.on_key(key('j'));
    }
    let _ = dump(&app);
    let top_before = top_line_index(&app);
    assert!(top_before.is_some());

    // Re-render narrower: the layout rewraps against the new width, but the top
    // logical line is re-anchored so it stays at the viewport top.
    let _ = strix::terminal::dump_frame(&app, 60, H).unwrap();
    assert_eq!(
        top_line_index(&app),
        top_before,
        "the top logical line survives a narrower resize"
    );
}

/// A repo whose `feature` branch adds a file that opens with 8 long (200-char)
/// lines then 30 short ones — so scrolling past the long block and then enabling
/// wrap makes the wrapped lines *above* the viewport top balloon the row count,
/// moving the re-anchored scroll offset by a lot.
fn leading_long_lines_repo() -> tempfile::TempDir {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["checkout", "-qb", "feature"]);
    let long = "A".repeat(200);
    let mut c = String::new();
    for i in 0..8 {
        c.push_str(&format!("{long}{i}\n"));
    }
    for i in 0..60 {
        c.push_str(&format!("short {i}\n"));
    }
    write(p, "big.txt", &c);
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add big.txt"]);
    dir
}

#[test]
fn a_click_batched_after_wrap_toggle_uses_the_reanchored_layout() {
    let repo = leading_long_lines_repo();
    // The event loop drains a burst of input before it redraws, so a `w` and a
    // click can arrive with no render between them. The click must resolve
    // against the layout+offset the toggle produced, not a pre-anchor snapshot.
    let click_at = |app: &mut App| {
        let area = app.diff_area();
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 8,
            row: area.y + 5,
            modifiers: strix::crossterm::event::KeyModifiers::NONE,
        });
    };

    // Reference: a render sits between `w` and the click, so everything is already
    // rebuilt/anchored when the click lands.
    let mut reference = review(repo.path(), "main");
    let _ = dump(&reference);
    reference.on_key(key('b')); // focus the diff
    for _ in 0..40 {
        reference.on_key(key('j')); // scroll into the short-line region
    }
    let _ = dump(&reference);
    let scroll_before = reference.diff_scroll.get();
    reference.on_key(key('w'));
    let _ = dump(&reference); // rebuild + re-anchor + fresh metrics
    assert_ne!(
        reference.diff_scroll.get(),
        scroll_before,
        "the wrapped leading lines moved the re-anchored offset (test is meaningful)"
    );
    click_at(&mut reference);
    let want = cursor_target(&reference);

    // Under test: the identical sequence, but NO render between `w` and the click.
    let mut batched = review(repo.path(), "main");
    let _ = dump(&batched);
    batched.on_key(key('b'));
    for _ in 0..40 {
        batched.on_key(key('j'));
    }
    let _ = dump(&batched);
    batched.on_key(key('w'));
    click_at(&mut batched); // no dump between the toggle and the click
    let got = cursor_target(&batched);

    assert_eq!(
        got, want,
        "the batched click resolves against the re-anchored layout, not a stale offset"
    );
}

#[test]
fn a_double_click_batched_after_wrap_hits_a_consistent_row() {
    // Sharper guard for the same bug: both halves of a batched double-click must
    // map the screen position through the *re-anchored* layout. If the first
    // (which triggers the rebuild) reads the pre-anchor offset while the second
    // reads the post-anchor one, their targets disagree, the double-click is
    // missed, and the editor never opens.
    let repo = leading_long_lines_repo();
    let mut app = review(repo.path(), "main");
    let _ = dump(&app);
    app.on_key(key('b')); // focus the diff
    for _ in 0..40 {
        app.on_key(key('j')); // scroll deep so diff_scroll > 0
    }
    let _ = dump(&app);
    assert!(
        app.diff_scroll.get() > 0,
        "the viewport is scrolled off the top"
    );

    app.on_key(key('w')); // enable wrap; re-anchor pending, not yet applied

    let area = app.diff_area();
    let ev = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 8,
        row: area.y + 2, // a short code line in the scrolled view
        modifiers: strix::crossterm::event::KeyModifiers::NONE,
    };
    let t = Instant::now();
    app.on_mouse_at(ev, t);
    app.on_mouse_at(ev, t + Duration::from_millis(40));
    assert!(
        app.editor_open(),
        "both clicks resolved to the same re-anchored row, so the double-click fired"
    );
}

// --- Config / persistence ----------------------------------------------------

fn app_with_dir(repo: &Path, dir: &Path) -> App {
    App::new(repo.to_path_buf())
        .unwrap()
        .with_config_dir(Some(dir.to_path_buf()))
}

#[test]
fn wrap_defaults_off_and_the_key_persists_it() {
    let repo = init_repo();
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with_dir(repo.path(), dir.path());
    assert!(!app.wrap_lines, "wrap is off by default");

    app.on_key(key('w'));
    assert!(app.wrap_lines, "`w` enabled wrap");
    let cfg = fs::read_to_string(dir.path().join("config.toml")).unwrap_or_default();
    assert!(cfg.contains("wrap_lines = true"), "persisted: {cfg:?}");

    app.on_key(key('w'));
    assert!(!app.wrap_lines, "`w` toggled back off");
    let cfg = fs::read_to_string(dir.path().join("config.toml")).unwrap_or_default();
    assert!(cfg.contains("wrap_lines = false"), "re-persisted: {cfg:?}");
}

#[test]
fn config_wrap_lines_true_starts_wrapped() {
    let repo = init_repo();
    let config = Config {
        wrap_lines: Some(true),
        ..Config::default()
    };
    let app = App::with_config(repo.path().to_path_buf(), &config).unwrap();
    assert!(app.wrap_lines, "wrap_lines = true starts on");
}
