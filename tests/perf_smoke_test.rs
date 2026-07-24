//! Perf gate (plan §3.7, C7): deterministic structural assertions are primary
//! — the repo convention is `TestBackend` render assertions and temp-repo
//! integration tests, not a timing harness — with one deliberately loose
//! wall-clock backstop at the end.
//!
//! All tests share one synthetic diff: a ~2,200-line file where roughly half
//! the lines change (several of those changed lines are 400+ columns), so a
//! single repo exercises the layout-rebuild, wrap, and horizontal-scroll caches
//! under a genuinely large diff rather than a handful of test lines.

mod common;

use std::time::{Duration, Instant};

use common::{git, init_repo, press, write};
use strix::app::App;
use strix::crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
use tempfile::TempDir;

const W: u16 = 100;
const H: u16 = 30;

fn dump(app: &App, width: u16, height: u16) -> String {
    strix::terminal::dump_frame(app, width, height).unwrap()
}

/// A repo with `big.txt` committed at ~2,200 lines, then edited (uncommitted)
/// so roughly half the lines change: every 100th changed line is stretched to
/// 420 columns (several 400+-column lines, per the plan's pinned parameters),
/// the rest of the changed lines just append a marker. The unchanged
/// (odd-indexed) lines stay as context, so the diff is a realistic mix of
/// context / add / delete rather than an all-changed file.
fn synthetic_diff_repo() -> TempDir {
    const LINES: usize = 2200;
    let repo = init_repo();
    let p = repo.path();

    let mut base = String::new();
    for i in 0..LINES {
        base.push_str(&format!(
            "line {i:04} base content padding padding padding\n"
        ));
    }
    write(p, "big.txt", &base);
    git(p, &["add", "big.txt"]);
    git(p, &["commit", "-q", "-m", "add big.txt"]);

    let mut modified = String::new();
    for i in 0..LINES {
        if i % 100 == 0 {
            // A 420-column line: several of these land in the diff (the
            // 400+-column case the plan pins).
            let long: String = (0..420)
                .map(|j| char::from(b'a' + ((i + j) % 26) as u8))
                .collect();
            modified.push_str(&long);
            modified.push('\n');
        } else if i % 2 == 0 {
            modified.push_str(&format!(
                "line {i:04} CHANGED content padding padding padding\n"
            ));
        } else {
            modified.push_str(&format!(
                "line {i:04} base content padding padding padding\n"
            ));
        }
    }
    write(p, "big.txt", &modified);
    repo
}

fn app_on_synthetic_diff() -> (TempDir, App) {
    let repo = synthetic_diff_repo();
    let app = App::new(repo.path().to_path_buf()).unwrap();
    (repo, app)
}

fn scroll_right(app: &mut App) {
    let area = app.diff_area();
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::ScrollRight,
        column: area.x + 2,
        row: area.y + 2,
        modifiers: KeyModifiers::NONE,
    });
}

// --- (a) unchanged state never bumps the layout generation ------------------

#[test]
fn repeated_render_of_unchanged_state_does_not_bump_layout_generation() {
    let (_repo, app) = app_on_synthetic_diff();
    let _ = dump(&app, W, H); // first render builds the layout (generation 0 -> 1)
    let baseline = app.layout_generation();
    assert!(baseline > 0, "the first render built a layout");

    for _ in 0..3 {
        let _ = dump(&app, W, H);
        assert_eq!(
            app.layout_generation(),
            baseline,
            "a render at unchanged width/mode/wrap/line-numbers is a cache hit"
        );
    }
}

// --- (b) width / wrap / line-numbers each bump the generation exactly once --

#[test]
fn a_width_change_bumps_the_generation_exactly_once() {
    let (_repo, app) = app_on_synthetic_diff();
    let _ = dump(&app, W, H);
    let before = app.layout_generation();

    let _ = dump(&app, W + 10, H);
    assert_eq!(
        app.layout_generation(),
        before + 1,
        "a width change rebuilds the layout exactly once"
    );

    // A further render at the same (new) width is a cache hit again.
    let _ = dump(&app, W + 10, H);
    assert_eq!(
        app.layout_generation(),
        before + 1,
        "a repeat render at the new width doesn't rebuild again"
    );
}

#[test]
fn a_wrap_toggle_bumps_the_generation_exactly_once() {
    let (_repo, mut app) = app_on_synthetic_diff();
    let _ = dump(&app, W, H);
    let before = app.layout_generation();

    press(&mut app, 'w'); // toggle_wrap: flips the field, doesn't rebuild itself
    let _ = dump(&app, W, H);
    assert_eq!(
        app.layout_generation(),
        before + 1,
        "enabling wrap rebuilds the layout exactly once"
    );

    let _ = dump(&app, W, H);
    assert_eq!(
        app.layout_generation(),
        before + 1,
        "a repeat render with wrap unchanged doesn't rebuild again"
    );
}

#[test]
fn a_line_numbers_toggle_bumps_the_generation_exactly_once() {
    let (_repo, mut app) = app_on_synthetic_diff();
    let _ = dump(&app, W, H);
    let before = app.layout_generation();

    press(&mut app, 'n'); // toggle_line_numbers: the gutter is a wrap input
    let _ = dump(&app, W, H);
    assert_eq!(
        app.layout_generation(),
        before + 1,
        "toggling line numbers rebuilds the layout exactly once (gutter width changed)"
    );

    let _ = dump(&app, W, H);
    assert_eq!(
        app.layout_generation(),
        before + 1,
        "a repeat render with line numbers unchanged doesn't rebuild again"
    );
}

// --- (c) the per-diff max-line-width memo computes once --------------------

#[test]
fn max_line_width_memo_computes_once_across_repeated_horizontal_scroll() {
    // Wrap off (the default) so horizontal scroll is live. Every render reads
    // the memo (`effective_hscroll` -> `max_hscroll` -> `active_max_line_width`),
    // so the first render alone already populates it once.
    let (_repo, mut app) = app_on_synthetic_diff();
    let _ = dump(&app, W, H);
    let after_first_render = app.max_line_width_compute_count();
    assert_eq!(
        after_first_render, 1,
        "the memo computes exactly once for the first render"
    );

    // Scroll horizontally twice (each scroll clamps against the memoized
    // width, and each subsequent render re-reads it) — the diff object never
    // changed, so the memo must not recompute.
    scroll_right(&mut app);
    let _ = dump(&app, W, H);
    scroll_right(&mut app);
    let _ = dump(&app, W, H);

    assert_eq!(
        app.max_line_width_compute_count(),
        after_first_render,
        "the same (diff_generation, view) key reuses the memoized width"
    );
}

// --- (d) wrap on at narrow widths renders sane row counts, no panic --------

#[test]
fn wrap_at_narrow_widths_renders_without_panicking() {
    let (_repo, mut app) = app_on_synthetic_diff();
    press(&mut app, 'w'); // enable wrap

    for (width, height) in [(80u16, 24u16), (200, 50), (12, 8)] {
        let frame = dump(&app, width, height);
        assert!(
            !frame.is_empty(),
            "a frame was rendered at {width}x{height}"
        );
        let rows = app.diff_row_count();
        // A sane bound either way: at least one row is laid out, and the
        // per-char floor (plan §3.2 — content width <= 1 caps the segment
        // count) keeps it from exploding into something absurd even at the
        // narrowest width, where the gutter+sign can eat the whole pane.
        assert!(rows > 0, "at least one row at {width}x{height}");
        assert!(
            rows < 50_000,
            "row count stayed bounded at {width}x{height}: {rows}"
        );
    }
}

// --- (e) wall-clock backstop (smoke, not a benchmark) -----------------------

#[test]
fn thirty_wrapped_frames_at_200x50_complete_in_under_ten_seconds() {
    // This is a loose smoke backstop, not a benchmark: 30 renders of a large
    // (~2,200-line, several 400+-column) synthetic diff, wrap on, at a
    // generously sized pane, must finish well inside 10s even on a slow debug
    // build / loaded CI runner. It exists to catch a gross regression (an
    // accidental per-frame O(n^2) or a lost cache), not to track performance
    // precisely — see the manual AC5 timing protocol (plan §3.7) for that.
    let (_repo, mut app) = app_on_synthetic_diff();
    press(&mut app, 'w'); // enable wrap
    let _ = dump(&app, 200, 50); // first render pays for the layout build

    let start = Instant::now();
    for _ in 0..30 {
        let _ = dump(&app, 200, 50);
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "30 frames took {elapsed:?}, expected well under the loose 10s backstop"
    );
}
