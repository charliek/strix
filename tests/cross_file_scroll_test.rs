//! Cross-file scroll (plan 006): with `f` on, the diff pane is a *continuous
//! stream* — the anchor file's rows from the current offset, then the following
//! files' layouts, each led by its header (a band, preceded by a rule row for
//! every file but the stream's first). A wheel tick is a signed delta in
//! that extended domain (§3.2a), renormalized across boundaries, clamped so the
//! viewport bottom never passes the last row the stream offers (§3.2b), with the
//! border title following the anchor (§3.2c).
//!
//! The keyboard *walks* that same stream (plan 007 §3.3f, which replaced 006's
//! one-press teleport): `j` at the last stop steps onto the next file's first
//! target as a divergent cursor with the boundary still on screen, the anchor
//! following only when the reveal renormalizes past it; `k` above the anchor's
//! first stop flips immediately onto the previous file's last target. A
//! list-focused half page stays a plain continuous tick. Off by default.

mod common;

use common::{
    app_for, config, git, init_repo, init_repo_with_diverged_branches, mouse, pane_title,
    prepare_window, press, render_buffer, rendered_app, select, selected_path, short_status_repo,
    unstaged, window_of, write,
};
use std::collections::BTreeMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use strix::app::{App, FileId, RowContent, RowTarget};
use strix::comments::{Branch, Comment, Scope, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use strix::terminal::dump_frame;
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 24;
/// The diff pane's inner height at frame height `h` (borders + header + footer).
fn viewport_at(h: u16) -> usize {
    (h - 4) as usize
}

// --- construction ----------------------------------------------------------

/// A status repo (README committed) with three untracked files, listed in path
/// order: `a.txt` is tall (60 lines), `b.txt` and `c.txt` are short.
fn multi_status_repo() -> TempDir {
    let repo = init_repo();
    let long: String = (0..60).map(|i| format!("line {i}\n")).collect();
    write(repo.path(), "a.txt", &long);
    write(repo.path(), "b.txt", "one\ntwo\nthree\n");
    write(repo.path(), "c.txt", "x\ny\n");
    repo
}

/// A tall `a.txt` (62 rows) followed by a `b.txt` deep enough (43 rows — it is
/// not the stream's first file, so its header carries a rule row) that its header
/// can legitimately pass the top of a 20-row viewport — the fixture the handoff
/// frames are pinned against.
fn handoff_repo() -> TempDir {
    let repo = init_repo();
    let a: String = (0..60).map(|i| format!("alpha {i}\n")).collect();
    let b: String = (0..40).map(|i| format!("beta {i}\n")).collect();
    write(repo.path(), "a.txt", &a);
    write(repo.path(), "b.txt", &b);
    repo
}

/// A short `a.txt` (4 physical rows with the header) followed by a tall `b.txt`,
/// so the window always needs a strip section to fill the viewport — the fixture
/// every "is the next frame whole?" test needs.
fn short_then_tall_repo() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "a.txt", "one\ntwo\n");
    let b: String = (0..80).map(|i| format!("beta {i}\n")).collect();
    write(repo.path(), "b.txt", &b);
    repo
}

// --- event helpers ---------------------------------------------------------

fn wheel_down(app: &mut App) {
    let d = app.diff_area();
    app.on_mouse(mouse(d.x + 2, d.y + 2, MouseEventKind::ScrollDown));
}

fn wheel_up(app: &mut App) {
    let d = app.diff_area();
    app.on_mouse(mouse(d.x + 2, d.y + 2, MouseEventKind::ScrollUp));
}

fn ctrl(app: &mut App, ch: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
}

/// Wheel down until the offset stops moving (the end-of-stream clamp, or the
/// per-file clamp with cross-file scroll off).
fn wheel_to_bottom(app: &mut App) {
    for _ in 0..500 {
        let (before_file, before) = (app.selected, app.diff_scroll.get());
        wheel_down(app);
        if (app.selected, app.diff_scroll.get()) == (before_file, before) {
            break;
        }
    }
}

/// Park the stream at `(file index, offset)` with the window prepared, the state
/// invariant every wheel tick starts from.
fn park(app: &mut App, index: usize, offset: usize, h: u16) {
    select(app, index, h);
    app.diff_scroll.set(offset);
    prepare_window(app);
}

// --- frame helpers ---------------------------------------------------------

/// One rendered cell, with everything §3.2f's "content **and** styling" covers.
type Cell = (String, Color, Color, Modifier);

/// The diff pane's body rows, top to bottom, cell by cell.
fn pane_rows(buf: &Buffer, area: Rect) -> Vec<Vec<Cell>> {
    (area.y..area.y + area.height)
        .map(|y| {
            (area.x..area.x + area.width)
                .map(|x| {
                    let cell = buf.cell((x, y)).expect("cell inside the pane");
                    (cell.symbol().to_string(), cell.fg, cell.bg, cell.modifier)
                })
                .collect()
        })
        .collect()
}

/// The frame the pane draws right now, as (title, body rows).
fn frame(app: &App, h: u16) -> (String, Vec<Vec<Cell>>) {
    let buf = render_buffer(app, W, h);
    let area = app.diff_area();
    (pane_title(&buf, area), pane_rows(&buf, area))
}

/// §3.2f: after a downward tick of `step` rows the body has shifted by exactly
/// that much — every row that was on screen before and still is holds the same
/// cells, styling included.
fn assert_shift_down(before: &[Vec<Cell>], after: &[Vec<Cell>], step: usize) {
    for y in 0..before.len().saturating_sub(step) {
        assert_eq!(
            after[y],
            before[y + step],
            "body row {y} after the tick is row {} from before it",
            y + step
        );
    }
}

/// The mirror image for an upward tick.
fn assert_shift_up(before: &[Vec<Cell>], after: &[Vec<Cell>], step: usize) {
    for y in 0..before.len().saturating_sub(step) {
        assert_eq!(
            after[y + step],
            before[y],
            "body row {} after the tick is row {y} from before it",
            y + step
        );
    }
}

/// The glyphs of every body row, top to bottom. The walk moves the cursor
/// highlight as well as the view, so a frame-to-frame comparison across a
/// keypress has to compare *content* — §3.2f's full cell comparison stays the
/// wheel's, which never touches the cursor.
fn body_glyphs(rows: &[Vec<Cell>]) -> Vec<String> {
    rows.iter()
        .map(|row| row.iter().map(|cell| cell.0.as_str()).collect())
        .collect()
}

/// Glyph-only [`assert_shift_up`]: after an upward step of `step` rows every row
/// that was on screen before is `step` rows lower.
fn assert_text_shift_up(before: &[Vec<Cell>], after: &[Vec<Cell>], step: usize) {
    let (before, after) = (body_glyphs(before), body_glyphs(after));
    for y in 0..before.len().saturating_sub(step) {
        assert_eq!(
            after[y + step],
            before[y],
            "body row {} after the step is row {y} from before it",
            y + step
        );
    }
}

/// The per-file physical row counts of the whole status stream, in list order —
/// the flattened oracle's backing array. A file's layout when selected *is* its
/// section (C2's parity guarantee), so reading it by selection is exact.
fn stream_rows(app: &mut App, h: u16) -> Vec<usize> {
    let total = app.status.total();
    let mut rows = Vec::new();
    for index in 0..total {
        select(app, index, h);
        rows.push(app.diff_row_count());
    }
    select(app, 0, h);
    rows
}

/// Where the viewport's top row sits in the flattened stream: the anchor file's
/// start plus the anchor-relative offset. The identity `(B, o) ≡ (A, R_A + o)`
/// makes this single number the representation-independent state.
fn flat_top(app: &App, rows: &[usize]) -> usize {
    rows[..app.selected].iter().sum::<usize>() + app.diff_scroll.get()
}

// --- config: default + persistence -----------------------------------------

#[test]
fn cross_file_scroll_defaults_off() {
    let repo = init_repo();
    let app = app_for(&repo, Config::default());
    assert!(!app.cross_file_scroll, "off unless configured on");
}

#[test]
fn f_toggles_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo();
    let mut app = App::new(repo.path().to_path_buf())
        .unwrap()
        .with_config_dir(Some(dir.path().to_path_buf()));
    assert!(!app.cross_file_scroll);

    press(&mut app, 'f');
    assert!(app.cross_file_scroll, "`f` flips it on");
    let saved = std::fs::read_to_string(dir.path().join("config.toml")).unwrap_or_default();
    assert!(
        saved.contains("cross_file_scroll = true"),
        "persisted:\n{saved}"
    );

    press(&mut app, 'f');
    assert!(!app.cross_file_scroll, "`f` flips it back off");
}

// --- the renormalizer, against a flattened-stream oracle --------------------

#[test]
fn every_delta_from_every_position_matches_the_flattened_oracle() {
    // Four files of distinct heights, so every boundary combination differs.
    let repo = init_repo();
    write(repo.path(), "a.txt", "a1\na2\n"); // 4 rows
    write(
        repo.path(),
        "b.txt",
        &(0..12).map(|i| format!("b{i}\n")).collect::<String>(),
    );
    write(repo.path(), "c.txt", "c1\n"); // 3 rows
    write(
        repo.path(),
        "d.txt",
        &(0..7).map(|i| format!("d{i}\n")).collect::<String>(),
    );

    // Viewports from "one row" to "deeper than the whole stream": the end clamp,
    // the first-file floor, and the everything-fits floor are all in range.
    for h in [5u16, 9, 14, 40] {
        let mut app = rendered_app(&repo, config(true, false), h);
        let rows = stream_rows(&mut app, h);
        let total: usize = rows.iter().sum();
        let viewport = viewport_at(h);
        assert_eq!(app.diff_area().height as usize, viewport);
        let cap = total.saturating_sub(viewport);

        for (index, &file_rows) in rows.iter().enumerate() {
            // Offsets `0..=R` — `o == R` is the legal "next file's row 0 at the
            // top" position, not an overrun.
            for offset in 0..=file_rows {
                for delta in [-13i64, -7, -4, -3, -1, 1, 3, 4, 7, 13] {
                    park(&mut app, index, offset, h);
                    // A tick moves from what the frame *paints*, and the render
                    // clamp is anchor-relative (§3.2e): a park state left past the
                    // end of the stream by a jump normalizes against the rows
                    // below this file, not against the whole stream.
                    let file_start: usize = rows[..index].iter().sum();
                    let tail = total - file_start;
                    let painted = file_start + offset.min(tail.saturating_sub(viewport));
                    let expected = (painted as i64 + delta).clamp(0, cap as i64) as usize;

                    app.wheel_scroll_window(delta);

                    assert_eq!(
                        flat_top(&app, &rows),
                        expected,
                        "V={viewport} from (file {index}, o={offset}) delta {delta}"
                    );
                    assert!(
                        app.diff_scroll.get() <= rows[app.selected],
                        "the offset never overruns its anchor's rows"
                    );
                }
            }
        }
    }
}

#[test]
fn a_long_random_walk_stays_on_the_oracle() {
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    let total: usize = rows.iter().sum();
    let cap = total.saturating_sub(viewport_at(H));
    let mut position = 0usize;

    // A deterministic pseudo-random walk: sequences of deltas, not just single
    // ticks, so a renormalization that only holds from a fresh state would show.
    let mut seed = 0x9e3779b9u32;
    for _ in 0..400 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let delta = ((seed >> 8) % 21) as i64 - 10;
        if delta == 0 {
            continue;
        }
        position = (position as i64 + delta).clamp(0, cap as i64) as usize;
        app.wheel_scroll_window(delta);
        assert_eq!(flat_top(&app, &rows), position, "after delta {delta}");
    }
}

// --- the handoff, frame by frame -------------------------------------------

#[test]
fn the_title_swaps_exactly_one_row_past_the_top() {
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    let (r_a, step) = (rows[0], 1);

    // One row short of the boundary: a.txt's last row leads the viewport.
    park(&mut app, 0, r_a - 1, H);
    let (title, body) = frame(&app, H);
    assert!(
        title.contains("a.txt"),
        "title before the boundary: {title}"
    );

    // b.txt's header row reaches the top: still a.txt's frame — the header sits
    // *at* the top, not past it.
    app.wheel_scroll_window(step);
    let (title, next) = frame(&app, H);
    assert_eq!(app.selected, 0, "no flip while the header is at the top");
    assert_eq!(
        app.diff_scroll.get(),
        r_a,
        "(A, R_A): the boundary position"
    );
    assert!(title.contains("a.txt"), "title at the boundary: {title}");
    assert_shift_down(&body, &next, step as usize);
    let body = next;

    // One more row and the header has passed the top: the title swaps, and the
    // body still shifted by exactly the tick.
    app.wheel_scroll_window(step);
    let (title, next) = frame(&app, H);
    assert_eq!(app.selected, 1, "the anchor flipped");
    assert_eq!(app.diff_scroll.get(), 1, "(B, 1): one row past the top");
    assert!(title.contains("b.txt"), "title after the handoff: {title}");
    assert_shift_down(&body, &next, step as usize);

    // And the frames keep shifting by the tick past the handoff.
    let body = next;
    app.wheel_scroll_window(step);
    let (_, next) = frame(&app, H);
    assert_shift_down(&body, &next, step as usize);
}

#[test]
fn scrolling_up_rests_at_the_boundary_then_flips_on_prev_content() {
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    let r_a = rows[0];

    park(&mut app, 1, 3, H);
    let (title, body) = frame(&app, H);
    assert!(title.contains("b.txt"));

    // `(B, 0)` is a legal resting state: up-renormalization triggers only at
    // `o < 0`, so the title still reads b.txt with its header at the top.
    app.wheel_scroll_window(-3);
    let (title, next) = frame(&app, H);
    assert_eq!((app.selected, app.diff_scroll.get()), (1, 0), "(B, 0)");
    assert!(
        title.contains("b.txt"),
        "hysteresis holds at (B, 0): {title}"
    );
    assert_shift_up(&body, &next, 3);
    let body = next;

    // The first tick that shows a.txt's content flips the title — the up
    // direction has no header row to cross, so it is mechanically asymmetric.
    app.wheel_scroll_window(-1);
    let (title, next) = frame(&app, H);
    assert_eq!(
        (app.selected, app.diff_scroll.get()),
        (0, r_a - 1),
        "one row of a.txt is showing"
    );
    assert!(title.contains("a.txt"), "title after the up flip: {title}");
    assert_shift_up(&body, &next, 1);
}

#[test]
fn the_boundary_state_is_pixel_identical_from_both_directions() {
    // `(A, R_A)` reached going down and `(B, 0)` reached going up are the same
    // picture; only the title differs (plan §3.2c).
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);

    park(&mut app, 0, rows[0] - 1, H);
    app.wheel_scroll_window(1);
    let (down_title, down_body) = frame(&app, H);
    assert_eq!((app.selected, app.diff_scroll.get()), (0, rows[0]));

    park(&mut app, 1, 1, H);
    app.wheel_scroll_window(-1);
    let (up_title, up_body) = frame(&app, H);
    assert_eq!((app.selected, app.diff_scroll.get()), (1, 0));

    assert_eq!(down_body, up_body, "the same rows, cell for cell");
    assert!(down_title.contains("a.txt") && up_title.contains("b.txt"));
}

#[test]
fn side_by_side_hands_off_the_same_way() {
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    press(&mut app, 'd'); // side-by-side
    dump_frame(&app, W, H).unwrap();
    let rows = stream_rows(&mut app, H);

    park(&mut app, 0, rows[0] - 2, H);
    let (title, mut body) = frame(&app, H);
    assert!(title.contains("a.txt"));

    for expected_flip in [false, false, true] {
        app.wheel_scroll_window(1);
        let (title, next) = frame(&app, H);
        assert_shift_down(&body, &next, 1);
        assert_eq!(
            app.selected == 1,
            expected_flip,
            "side-by-side flips one row past the top too"
        );
        assert_eq!(title.contains("b.txt"), expected_flip, "title: {title}");
        if !expected_flip && app.diff_scroll.get() == rows[0] {
            // The boundary frame: b.txt's header leads the viewport — its rule row
            // then its band, both spanning the whole pane with no centre divider
            // splitting them (plan 006 §3.4, plan 008 §3.5).
            let rule: String = next[0].iter().map(|c| c.0.as_str()).collect();
            assert!(
                rule.trim_end().chars().all(|c| c == '─'),
                "the rule row: {rule}"
            );
            let header: String = next[1].iter().map(|c| c.0.as_str()).collect();
            assert!(header.contains("b.txt"), "the band row: {header}");
            assert!(!header.contains('│'), "full-width in SBS: {header}");
        }
        body = next;
    }
}

#[test]
fn a_strip_file_renders_exactly_as_it_does_when_selected() {
    // Mixed 4/5-digit line numbers: the strip segment sizes its gutter from its
    // own lines, so the same rows must come out identical either way.
    let repo = init_repo();
    let a: String = (0..60).map(|i| format!("alpha {i}\n")).collect();
    let b: String = (0..12_000).map(|i| format!("beta {i}\n")).collect();
    write(repo.path(), "a.txt", &a);
    write(repo.path(), "b.txt", &b);
    let cfg = Config {
        line_numbers: Some(true),
        ..config(true, false)
    };
    let mut app = rendered_app(&repo, cfg, H);
    let rows = stream_rows(&mut app, H);
    let viewport = viewport_at(H);

    // The boundary frame: a.txt's tail on top, b.txt's head below it.
    park(&mut app, 0, rows[0] - 2, H);
    let (_, boundary) = frame(&app, H);

    // The same rows of b.txt, drawn as the anchor from its own row 0.
    park(&mut app, 1, 0, H);
    let (_, standalone) = frame(&app, H);

    for y in 0..viewport - 2 {
        assert_eq!(
            boundary[y + 2],
            standalone[y],
            "strip row {y} of b.txt matches its standalone row (gutter width included)"
        );
    }
}

#[test]
fn a_neighbours_comment_box_scrolls_through_without_a_jump() {
    let repo = init_repo_with_diverged_branches();
    let mut app = App::for_review(repo.path().to_path_buf(), &config(true, false), "main").unwrap();
    // A 6-row viewport: the review's small diffs still make a stream deeper than
    // the pane, so the boundary is reachable.
    let h = 10;
    dump_frame(&app, W, h).unwrap();
    let second = app.review_files()[1].path.clone();
    seed_comment(repo.path(), &second);
    app.reload();
    dump_frame(&app, W, h).unwrap();

    // Park one row before the second file's header and step through it: a box in
    // the strip must shift like any other row.
    let anchor_rows = app.diff_row_count();
    app.diff_scroll.set(anchor_rows.saturating_sub(1));
    prepare_window(&mut app);
    assert_eq!(
        window_of(&app).rows(),
        app.diff_area().height as usize,
        "the window fills the pane, so the offset is not clamped away"
    );
    let (_, mut body) = frame(&app, h);

    let boxes = window_of(&app)
        .segments
        .iter()
        .filter(|segment| !segment.is_anchor())
        .flat_map(|segment| {
            let section = segment.section.as_ref().unwrap();
            section.rows[segment.row_range.clone()].to_vec()
        })
        .filter(|row| matches!(row.content, RowContent::Box(_)))
        .count();
    assert!(boxes > 0, "the neighbour's comment box is in the strip");

    for _ in 0..6 {
        app.wheel_scroll_window(1);
        let (_, next) = frame(&app, h);
        assert_shift_down(&body, &next, 1);
        body = next;
    }
}

/// Seed a one-comment review store on `file` — the schema the TUI and the
/// `strix comment` CLI share (mirrors `diff_window_test`).
fn seed_comment(repo: &std::path::Path, file: &str) {
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

// --- end of stream, floors, and hysteresis ---------------------------------

#[test]
fn the_viewport_bottom_pins_at_the_last_files_last_row() {
    let repo = multi_status_repo(); // a.txt tall, then two short files
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    let total: usize = rows.iter().sum();
    let viewport = viewport_at(H);

    wheel_to_bottom(&mut app);
    assert_eq!(
        flat_top(&app, &rows),
        total - viewport,
        "the bottom row of the window is the stream's last row"
    );
    assert_eq!(window_of(&app).rows(), viewport, "no blank overscroll");
}

#[test]
fn a_short_last_file_never_takes_the_title_by_wheel() {
    let repo = multi_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    assert!(
        rows[2] <= viewport_at(H),
        "c.txt is shorter than the viewport"
    );

    for _ in 0..80 {
        wheel_down(&mut app);
    }
    assert_ne!(
        selected_path(&app),
        "c.txt",
        "a short last file's header can never pass the top"
    );
    let buf = render_buffer(&app, W, H);
    let title = pane_title(&buf, app.diff_area());
    assert!(!title.contains("c.txt"), "title stayed put: {title}");
    // It is still visible — the stream just stops with its last row at the
    // viewport bottom.
    let window = window_of(&app);
    assert_eq!(
        window.segments.last().map(|s| s.path.as_str()),
        Some("c.txt")
    );
}

#[test]
fn the_first_file_floors_at_zero() {
    let repo = multi_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    park(&mut app, 0, 5, H);

    for _ in 0..10 {
        wheel_up(&mut app);
    }
    assert_eq!(app.selected, 0, "no wraparound off the first file");
    assert_eq!(app.diff_scroll.get(), 0, "floored at the top of the stream");
}

#[test]
fn a_stream_shorter_than_the_viewport_never_scrolls() {
    let repo = short_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    for _ in 0..10 {
        wheel_down(&mut app);
    }
    assert_eq!(app.selected, 0, "everything already fits");
    assert_eq!(app.diff_scroll.get(), 0);
}

#[test]
fn a_one_row_viewport_still_hands_off() {
    let repo = handoff_repo();
    let h = 5; // one body row
    let mut app = rendered_app(&repo, config(true, false), h);
    assert_eq!(app.diff_area().height, 1);
    let rows = stream_rows(&mut app, h);

    park(&mut app, 0, rows[0] - 1, h);
    app.wheel_scroll_window(1);
    assert_eq!((app.selected, app.diff_scroll.get()), (0, rows[0]));
    app.wheel_scroll_window(1);
    assert_eq!(
        (app.selected, app.diff_scroll.get()),
        (1, 1),
        "the handoff rule is the same at V == 1"
    );
    dump_frame(&app, W, h).unwrap();
}

// --- batches, metrics, and laziness ----------------------------------------

#[test]
fn a_fling_crosses_several_files_in_one_drained_batch() {
    // Twelve small files: a four-tick batch drained before any redraw must land
    // exactly where the deltas say — each flip refreshes the metrics the next
    // tick clamps against (plan §3.2d).
    let repo = init_repo();
    for i in 0..12 {
        write(repo.path(), &format!("f{i:02}.txt"), "one\n");
    }
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    let total: usize = rows.iter().sum();
    let viewport = viewport_at(H);
    assert!(total > viewport, "the stream is deeper than the viewport");
    select(&mut app, 0, H);

    let mut expected = 0usize;
    for _ in 0..4 {
        wheel_down(&mut app);
        expected = (expected + 3).min(total - viewport);
    }
    assert_eq!(flat_top(&app, &rows), expected, "no drift across the batch");
    assert!(app.selected > 0, "the fling crossed several files");
}

#[test]
fn scrolling_inside_one_file_computes_nothing() {
    let repo = init_repo();
    let tall: String = (0..300).map(|i| format!("line {i}\n")).collect();
    write(repo.path(), "a.txt", &tall);
    write(repo.path(), "b.txt", "next\n");
    let mut app = rendered_app(&repo, config(true, false), H);

    let before = app.diff_compute_count();
    for _ in 0..20 {
        wheel_down(&mut app);
    }
    assert!(app.diff_scroll.get() > 0, "the view moved");
    assert_eq!(
        app.diff_compute_count(),
        before,
        "o + V ≤ R: no neighbour is touched"
    );
}

#[test]
fn crossing_computes_only_what_the_window_needs_and_re_crossing_hits_the_cache() {
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);

    let before = app.diff_compute_count();
    park(&mut app, 0, rows[0] - 1, H);
    assert_eq!(
        app.diff_compute_count() - before,
        1,
        "the boundary window needs exactly the next file"
    );

    let computed = app.diff_compute_count();
    // Down across the boundary and back, twice: every file involved is cached.
    for _ in 0..3 {
        app.wheel_scroll_window(3);
        app.wheel_scroll_window(-3);
    }
    assert_eq!(app.selected, 0, "back where it started");
    assert_eq!(
        app.diff_compute_count(),
        computed,
        "oscillating across the boundary recomputes nothing"
    );
}

// --- staged↔unstaged same path ---------------------------------------------

#[test]
fn the_wheel_crosses_a_staged_unstaged_same_path_boundary() {
    // `dup.txt` is both staged and unstaged: two stream entries, one path, with
    // different header markers — the handoff must flip the marker.
    let repo = init_repo();
    let staged: String = (0..60).map(|i| format!("line {i}\n")).collect();
    write(repo.path(), "dup.txt", &staged);
    git(repo.path(), &["add", "dup.txt"]);
    write(
        repo.path(),
        "dup.txt",
        &format!("{staged}extra a\nextra b\n"),
    );
    let h = 10; // a 6-row viewport, so the second entry can lead it
    let mut app = rendered_app(&repo, config(true, false), h);
    assert_eq!(app.status.total(), 2, "staged and unstaged rows");
    let rows = stream_rows(&mut app, h);
    assert!(
        rows[1] > viewport_at(h),
        "the unstaged entry is deep enough"
    );

    park(&mut app, 0, rows[0] - 1, h);
    let staged_marker = anchor_header_marker(&app);
    app.wheel_scroll_window(2);
    assert_eq!(
        app.selected, 1,
        "crossed into the same path's other section"
    );
    assert_eq!(selected_path(&app), "dup.txt");
    assert_eq!(app.diff_scroll.get(), 1);
    let unstaged_marker = anchor_header_marker(&app);
    assert_ne!(
        staged_marker, unstaged_marker,
        "the header marker flips at the handoff"
    );
}

/// The marker char on the anchor file's own header row.
fn anchor_header_marker(app: &App) -> char {
    app.diff_layout(app.diff_area().width)
        .iter()
        .find_map(|row| match &row.content {
            RowContent::FileHeader(header) => Some(header.marker),
            _ => None,
        })
        .expect("the anchor's header row")
}

// --- geometry and layout-key changes ---------------------------------------

/// The frame the pane draws at an arbitrary size, as (pane rect, body rows).
fn frame_at(app: &App, w: u16, h: u16) -> (Rect, Vec<Vec<Cell>>) {
    let buf = render_buffer(app, w, h);
    let area = app.diff_area();
    (area, pane_rows(&buf, area))
}

/// A row is blank when every cell on it is a space.
fn blank(row: &[Cell]) -> bool {
    row.iter().all(|(sym, ..)| sym == " ")
}

#[test]
fn a_resize_prepares_the_window_for_the_new_geometry() {
    let repo = short_then_tall_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    prepare_window(&mut app);
    assert!(
        window_of(&app).segments.len() > 1,
        "the short anchor already streams its neighbour"
    );

    // Wider and narrower, taller and shorter: each resize changes the layout key,
    // so the sections the last geometry prepared no longer match.
    for (w, h) in [(160_u16, 30_u16), (90, 18), (200, 40)] {
        app.on_resize(w, h);
        // No intervening event: the very next frame must already be whole.
        let (area, rows) = frame_at(&app, w, h);
        let window = app.diff_window(area.width, area.height);
        assert_eq!(
            window.rows(),
            area.height as usize,
            "no shortfall at {w}x{h} — the window fills the viewport"
        );
        assert!(
            !blank(rows.last().expect("a body row")),
            "no blank tail at {w}x{h}"
        );
    }

    // The other half of the width derivation: with the Changes panel hidden the
    // diff pane spans the whole body.
    press(&mut app, 'b');
    assert!(!app.show_changes);
    app.on_resize(140, 26);
    let (area, rows) = frame_at(&app, 140, 26);
    assert_eq!(
        app.diff_window(area.width, area.height).rows(),
        area.height as usize,
        "no shortfall with the Changes panel hidden"
    );
    assert!(!blank(rows.last().expect("a body row")), "no blank tail");
}

#[test]
fn a_layout_key_toggle_re_prepares_the_window() {
    let repo = short_then_tall_repo();
    let viewport = viewport_at(H);

    // `w` (wrap), `n` (line numbers) and `d` (side-by-side) each change the layout
    // key; the strip must be whole on the very next frame, with no extra event.
    for key in ['w', 'n', 'd'] {
        let mut app = rendered_app(&repo, config(true, false), H);
        prepare_window(&mut app);
        assert_eq!(window_of(&app).rows(), viewport, "the strip starts whole");

        press(&mut app, key);
        let (area, rows) = frame_at(&app, W, H);
        let window = app.diff_window(area.width, area.height);
        assert_eq!(
            window.rows(),
            viewport,
            "`{key}` left the window short at the rebuilt key"
        );
        assert!(
            !blank(rows.last().expect("a body row")),
            "`{key}` blank tail"
        );
    }
}

#[test]
fn turning_f_on_prepares_the_strip_immediately() {
    let repo = short_then_tall_repo();
    let mut app = rendered_app(&repo, config(false, false), H);

    press(&mut app, 'f');
    assert!(app.cross_file_scroll);
    let (area, rows) = frame_at(&app, W, H);
    let window = app.diff_window(area.width, area.height);
    assert!(window.segments.len() > 1, "the strip appeared at once");
    assert_eq!(
        window.rows(),
        viewport_at(H),
        "turning `f` on fills the viewport on the next frame"
    );
    assert!(!blank(rows.last().expect("a body row")), "blank tail");
}

// --- refresh, mutation, and the toggle -------------------------------------

#[test]
fn a_reload_mid_window_re_derives_the_strip() {
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    park(&mut app, 0, rows[0], H); // b.txt's header leads the viewport
    let generation = app.stream_generation();

    write(repo.path(), "b.txt", "rewritten\n");
    app.reload();
    dump_frame(&app, W, H).unwrap();
    assert!(app.stream_generation() > generation, "the stream retired");

    prepare_window(&mut app);
    let window = window_of(&app);
    let strip = window.segments[1]
        .section
        .as_ref()
        .expect("b.txt is still the strip");
    assert!(
        strip.rows.len() < 43,
        "the strip re-derived from the rewritten file, not a stale section"
    );
}

#[test]
fn a_staging_mutation_mid_window_keeps_the_window_consistent() {
    let repo = init_repo();
    let a: String = (0..60).map(|i| format!("alpha {i}\n")).collect();
    write(repo.path(), "a.txt", &a);
    write(
        repo.path(),
        "b.txt",
        &(0..40).map(|i| format!("beta {i}\n")).collect::<String>(),
    );
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "files"]);
    write(repo.path(), "a.txt", &format!("{a}tail\n"));
    write(repo.path(), "b.txt", "rewritten\n");
    let mut app = rendered_app(&repo, config(true, false), H);
    let anchor_rows = app.diff_row_count();
    park(&mut app, 0, anchor_rows.saturating_sub(2), H);

    press(&mut app, 's'); // stage the anchor: the file list is rewritten
    dump_frame(&app, W, H).unwrap();

    let window = window_of(&app);
    let live: Vec<String> = (0..app.status.total())
        .filter_map(|i| app.file_at_index(i).map(|(_, e)| e.path.clone()))
        .collect();
    for segment in &window.segments {
        assert!(
            live.contains(&segment.path),
            "{} is a file the current stream holds",
            segment.path
        );
    }
    assert!(
        window.rows() <= app.diff_area().height as usize,
        "no stale overfill"
    );
}

#[test]
fn toggling_f_off_while_extended_normalizes_the_offset() {
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    park(&mut app, 0, rows[0], H); // extended: past the anchor's own max

    assert!(
        app.diff_scroll.get() > app.diff_max_scroll(),
        "the offset is in the extended domain"
    );
    press(&mut app, 'f');
    assert!(!app.cross_file_scroll);
    let bottom = app.diff_max_scroll();
    assert_eq!(
        app.diff_scroll.get(),
        bottom,
        "the stored offset is normalized to what the frame paints, not left extended"
    );
    dump_frame(&app, W, H).unwrap();

    let window = window_of(&app);
    assert_eq!(window.segments.len(), 1, "no strip with the mode off");
    assert_eq!(
        window.segments[0].row_range.start, bottom,
        "the extended offset reads as the per-file bottom"
    );
}

#[test]
fn toggling_f_back_on_resumes_from_what_was_painted() {
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);

    // Park at (a.txt, R_a) by wheel — b.txt's header leads the viewport.
    let rows = stream_rows(&mut app, H);
    park(&mut app, 0, rows[0] - 3, H);
    wheel_down(&mut app); // one SCROLL_STEP lands exactly on (a.txt, R_a)
    assert_eq!(app.selected, 0, "still anchored on a.txt");
    assert_eq!(app.diff_scroll.get(), rows[0], "parked at (a.txt, R_a)");
    assert!(
        app.diff_scroll.get() > app.diff_max_scroll(),
        "the offset is in the extended domain"
    );

    press(&mut app, 'f'); // off: the frame paints a.txt's bottom
    let bottom = app.diff_max_scroll();
    let off_frame = frame(&app, H);

    press(&mut app, 'f'); // on again
    assert!(app.cross_file_scroll);
    assert_eq!(
        app.diff_scroll.get(),
        bottom,
        "re-enabling continues from what was displayed, not the old extended offset"
    );
    let on_frame = frame(&app, H);
    assert_eq!(
        on_frame.0, off_frame.0,
        "the title still reads a.txt — no B-boundary resurrection"
    );
    assert!(
        on_frame.0.contains("a.txt"),
        "the anchor never moved: {}",
        on_frame.0
    );
}

// --- cross-file scroll off --------------------------------------------------

#[test]
fn disabled_clamps_and_never_hops() {
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(false, false));
    dump_frame(&app, W, H).unwrap();
    wheel_to_bottom(&mut app);
    let max = app.diff_max_scroll();

    for _ in 0..5 {
        wheel_down(&mut app);
    }
    assert_eq!(app.selected, 0, "disabled never crosses a boundary");
    assert_eq!(
        app.diff_scroll.get().min(max),
        max,
        "it clamps at the edge exactly as before"
    );
}

// --- keyboard: the walk (plan 007 §3.3f) -----------------------------------

/// The [`RowTarget`] the diff cursor names right now, resolved in the anchor's
/// layout (a converged cursor only — see [`cursor_at`] for the address).
fn cursor_target(app: &App) -> RowTarget {
    app.diff_layout(app.diff_area().width)[app.review_cursor()].target
}

/// The address the cursor names: which stream file, and which of its targets.
fn cursor_at(app: &App) -> (FileId, RowTarget) {
    let address = app.cursor_address().expect("the pane has a cursor");
    (address.file, address.target)
}

/// Press `j` (or `k`) until the anchor flips, returning how many presses it
/// took. Measured by the anchor's [`FileId`], which reads the same in either
/// view — and tells the two entries of a dup path apart. Panics rather than
/// looping forever.
fn press_until_flip(app: &mut App, key: char, w: u16, h: u16) -> usize {
    let from = app.active_file_id();
    for count in 1..=200 {
        press(app, key);
        dump_frame(app, w, h).unwrap();
        if app.active_file_id() != from {
            return count;
        }
    }
    panic!("the anchor never flipped");
}

/// The glyphs of body row `y` of the diff pane.
fn body_text(rows: &[Vec<Cell>], y: usize) -> String {
    rows[y].iter().map(|cell| cell.0.as_str()).collect()
}

/// Type `text` into the open comment editor.
fn typ(app: &mut App, text: &str) {
    for ch in text.chars() {
        press(app, ch);
    }
}

#[test]
fn j_at_the_last_stop_walks_onto_the_next_files_header() {
    let repo = short_status_repo(); // b.txt then c.txt, both short
    let mut app = rendered_app(&repo, config(true, false), H);
    press(&mut app, 'l'); // focus the diff
    press(&mut app, 'G'); // cursor to b.txt's last stop
    assert_eq!(app.selected, 0);
    let (title_before, body_before) = frame(&app, H);

    press(&mut app, 'j');
    // The whole stream already fits the viewport, so the walk moves the cursor
    // and nothing else: no scroll, no flip, no title change (§3.3f).
    assert_eq!(app.selected, 0, "the anchor stays where it was");
    assert_eq!(selected_path(&app), "b.txt");
    assert_eq!(app.diff_scroll.get(), 0);
    assert!(app.cursor_divergent(), "the address names the file below");
    assert_eq!(cursor_at(&app), (unstaged("c.txt"), RowTarget::FileHeader));

    let (title, body) = frame(&app, H);
    assert_eq!(title, title_before, "the title still reads b.txt: {title}");
    assert_eq!(
        body_glyphs(&body),
        body_glyphs(&body_before),
        "the same rows are on screen — only the highlight moved"
    );
    let first = body_text(&body, 0);
    assert!(first.contains("b.txt"), "the anchor still leads: {first}");
}

#[test]
fn k_above_the_anchors_first_stop_flips_onto_the_previous_files_last_target() {
    let repo = handoff_repo(); // a.txt is tall, b.txt deep enough to follow it
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    let prev_rows = rows[0];

    press(&mut app, 'j'); // select b.txt
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l'); // focus the diff — the cursor rests on b.txt's header
    assert_eq!(cursor_target(&app), RowTarget::FileHeader);
    assert_eq!(app.diff_scroll.get(), 0);

    press(&mut app, 'k');
    // Previous files cannot render below the anchor, so this direction flips on
    // the spot: the anchor, selection and title move to a.txt and the cursor is
    // converged on its last target, top-aligned (§3.3f's pinned asymmetry).
    assert_eq!(app.selected, 0, "the anchor moved with the cursor");
    assert_eq!(selected_path(&app), "a.txt");
    assert_eq!(
        app.diff_scroll.get(),
        prev_rows - 1,
        "top = the landing target's own span start"
    );
    assert_eq!(
        app.review_cursor(),
        prev_rows - 1,
        "the cursor is on the previous file's last target"
    );
    assert!(!app.cursor_divergent(), "and it converged there");

    let (title, body) = frame(&app, H);
    assert!(
        title.contains("a.txt"),
        "the title followed the anchor: {title}"
    );
    let top = body_text(&body, 0);
    assert!(
        top.contains("alpha 59"),
        "a.txt's last row leads the pane: {top}"
    );
    let rule = body_text(&body, 1);
    assert!(
        rule.trim_end().chars().all(|c| c == '─'),
        "the file just left is separated by its rule row: {rule}"
    );
    let below = body_text(&body, 2);
    assert!(
        below.contains("b.txt"),
        "and its band continues right below that: {below}"
    );
}

#[test]
fn the_k_flip_moves_the_view_by_exactly_one_row() {
    // The up flip is a *walk*, not a jump: one press, one row. Pinned as a buffer
    // comparison (plan 006 §3.2f's rules) rather than an offset assertion.
    let repo = handoff_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    press(&mut app, 'j'); // select b.txt: its header leads the pane
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l');
    let (_, before) = frame(&app, H);

    press(&mut app, 'k');
    let (_, after) = frame(&app, H);
    assert_text_shift_up(&before, &after, 1);
}

#[test]
fn first_file_keyboard_up_clamps() {
    let repo = short_status_repo();
    let mut app = rendered_app(&repo, config(true, false), H);
    press(&mut app, 'l');
    press(&mut app, 'k'); // at the first file, top edge
    assert_eq!(app.selected, 0, "no wraparound off the first file");
    assert_eq!(app.diff_scroll.get(), 0);
    assert_eq!(app.review_cursor(), 0);
}

#[test]
fn keyboard_j_and_k_with_cross_off_clamp() {
    let repo = short_status_repo();
    let mut app = rendered_app(&repo, config(false, false), H);
    press(&mut app, 'l');
    press(&mut app, 'G'); // the last stop of the first file
    let last = app.review_cursor();
    press(&mut app, 'j');
    assert_eq!(app.selected, 0, "cross-off never crosses downward");
    assert_eq!(
        app.review_cursor(),
        last,
        "the cursor clamps at the last row"
    );

    press(&mut app, 'g'); // back to the first stop
    assert_eq!(app.review_cursor(), 0);
    press(&mut app, 'k');
    assert_eq!(app.selected, 0, "cross-off never crosses upward");
    assert_eq!(app.review_cursor(), 0, "the cursor clamps at the first row");
    assert_eq!(app.diff_scroll.get(), 0);
}

#[test]
fn tall_wrapped_target_scrolls_internally_before_crossing() {
    let repo = init_repo();
    let long_line = "x".repeat(5000);
    write(repo.path(), "a.txt", &format!("{long_line}\n"));
    write(repo.path(), "b.txt", "short\n");
    let mut app = app_for(&repo, config(true, true)); // cross-file + wrap on
    dump_frame(&app, W, H).unwrap();
    assert_eq!(selected_path(&app), "a.txt");

    press(&mut app, 'l'); // focus the diff
    press(&mut app, 'G'); // cursor onto the (tall, wrapped) long line
    let before = app.diff_scroll.get();

    // The wrapped target is taller than the viewport: a step scrolls within it,
    // leaving the cursor (and the file) put.
    press(&mut app, 'j');
    assert_eq!(
        app.selected, 0,
        "no crossing while there is more of the line to see"
    );
    assert!(
        app.diff_scroll.get() > before,
        "the step scrolled within the tall target"
    );

    // Keep stepping: once the viewport reaches the hard edge, the next step walks
    // out of the file — onto b.txt's header, with a.txt still anchored.
    for _ in 0..500 {
        if app.cursor_divergent() {
            break;
        }
        press(&mut app, 'j');
    }
    assert_eq!(
        cursor_at(&app),
        (unstaged("b.txt"), RowTarget::FileHeader),
        "the walk leaves the tall target only once the whole line has been seen"
    );
    assert_eq!(app.selected, 0, "a.txt is still the anchor");
}

#[test]
fn an_up_walk_onto_a_multi_row_last_target_top_aligns_the_whole_box() {
    let repo = init_repo();
    write(repo.path(), "a.txt", "one\ntwo\n");
    write(repo.path(), "b.txt", "x\ny\n");
    let h = 10; // a 6-row viewport, shallower than the comment box below
    let mut app = rendered_app(&repo, config(true, false), h);

    // Anchor a five-line note to a.txt's last code row, so the box (several rows,
    // one target) becomes that file's last stop.
    press(&mut app, 'l');
    press(&mut app, 'G');
    press(&mut app, 'c');
    assert!(app.editor_open(), "the comment editor opened");
    for i in 0..5 {
        typ(&mut app, &format!("note {i}"));
        if i < 4 {
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        }
    }
    app.on_key(KeyEvent::from(KeyCode::Enter)); // save
    dump_frame(&app, W, h).unwrap();

    let v = viewport_at(h);
    let prev_rows = app.diff_row_count();
    let last_target = app.diff_layout(app.diff_area().width)[prev_rows - 1].target;
    let box_start = app
        .diff_layout(app.diff_area().width)
        .iter()
        .position(|row| row.target == last_target)
        .unwrap();
    assert!(
        prev_rows - box_start > 1,
        "the last target spans several physical rows"
    );

    press(&mut app, 'h'); // back to the file list
    press(&mut app, 'j'); // select b.txt
    dump_frame(&app, W, h).unwrap();
    press(&mut app, 'l'); // focus the diff — the cursor rests on b.txt's header

    press(&mut app, 'k');
    assert_eq!(app.selected, 0, "the up walk flipped back into a.txt");
    assert_eq!(
        app.diff_scroll.get(),
        box_start,
        "top = the landing target's span start, so the whole box is revealed \
         from its first row rather than clipped to its tail"
    );
    assert_eq!(
        cursor_target(&app),
        last_target,
        "the cursor names the whole box"
    );
    assert!(
        prev_rows - box_start >= v,
        "the box fills the viewport on its own"
    );
    let (_, body) = frame(&app, h);
    assert!(
        !body_text(&body, 0).contains("b.txt"),
        "the box leads the pane, not the file the cursor came from"
    );
}

#[test]
fn an_empty_file_is_a_one_stop_section_the_walk_steps_through() {
    let repo = init_repo();
    write(repo.path(), "a.txt", "one\n");
    write(repo.path(), "bin.dat", "a\0b\0c\n"); // NUL bytes → a binary diff
    write(repo.path(), "z.txt", "text\n");
    let mut app = rendered_app(&repo, config(true, false), H);
    assert_eq!(selected_path(&app), "a.txt");

    press(&mut app, 'l');
    press(&mut app, 'G'); // a.txt's last stop
    press(&mut app, 'j');
    // The three files together are shallower than the viewport, so the cursor
    // walks the whole stream with a.txt anchored throughout.
    assert_eq!(
        cursor_at(&app),
        (unstaged("bin.dat"), RowTarget::FileHeader)
    );
    assert_eq!(app.selected, 0, "the anchor never moved");
    assert_eq!(app.diff_scroll.get(), 0);

    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'j'); // a one-stop section: the next press steps clean off it
    assert_eq!(cursor_at(&app), (unstaged("z.txt"), RowTarget::FileHeader));

    // Symmetric upward: back onto the one-stop section, then off it again.
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'k');
    assert_eq!(
        cursor_at(&app),
        (unstaged("bin.dat"), RowTarget::FileHeader),
        "its only stop"
    );
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'k');
    assert_eq!(app.selected, 0);
    assert!(!app.cursor_divergent(), "back home on the anchor");
    assert_eq!(
        app.review_cursor(),
        app.diff_row_count() - 1,
        "the anchor's last target"
    );
}

#[test]
fn the_walk_crosses_a_staged_unstaged_same_path_boundary() {
    // `dup.txt` is both staged and unstaged: two stream entries, one path, with
    // different header markers. The address has to name the *entry*, not the path.
    let repo = init_repo();
    let staged_text: String = (0..60).map(|i| format!("line {i}\n")).collect();
    write(repo.path(), "dup.txt", &staged_text);
    git(repo.path(), &["add", "dup.txt"]);
    write(
        repo.path(),
        "dup.txt",
        &format!("{staged_text}extra a\nextra b\n"),
    );
    let mut app = rendered_app(&repo, config(true, false), H);
    assert_eq!(app.status.total(), 2, "staged and unstaged rows");
    let staged_marker = anchor_header_marker(&app);
    let staged_rows = app.diff_row_count();

    press(&mut app, 'l');
    press(&mut app, 'G'); // the staged entry's last stop
    press(&mut app, 'j');
    assert_eq!(
        cursor_at(&app),
        (unstaged("dup.txt"), RowTarget::FileHeader),
        "the walk stepped into the *working-tree* entry of the same path"
    );
    assert_eq!(app.selected, 0, "which the anchor has not followed yet");
    assert_eq!(
        anchor_header_marker(&app),
        staged_marker,
        "so the title's header marker is still the staged one"
    );

    // Walking on flips exactly when the reveal renormalizes past the boundary.
    // The step above already spent *two* rows, not one: the arriving header is a
    // two-row target and a reveal shows a target's whole span (plan 008 §3.5), so
    // the flip lands one press earlier than it did with a one-row header.
    let presses = press_until_flip(&mut app, 'j', W, H);
    assert_eq!(
        presses,
        viewport_at(H) - 1,
        "one press per row (the first two are already spent) until the top passes \
         R_anchor"
    );
    assert_eq!(selected_path(&app), "dup.txt");
    assert_eq!(app.diff_scroll.get(), 1, "(B, 1): one row past the top");
    assert_ne!(
        anchor_header_marker(&app),
        staged_marker,
        "the header marker flips with the anchor"
    );
    assert!(!app.cursor_divergent(), "the address converged on the flip");

    press_until_flip(&mut app, 'k', W, H);
    assert_eq!(app.selected, 0, "and back the other way");
    assert_eq!(anchor_header_marker(&app), staged_marker);
    assert_eq!(
        app.diff_scroll.get(),
        staged_rows - 1,
        "top-aligned on the staged entry's last target"
    );
}

#[test]
fn a_keypress_while_wheel_extended_snaps_back_to_the_cursor() {
    let repo = handoff_repo(); // a.txt is tall, b.txt deep enough to follow it
    let mut app = rendered_app(&repo, config(true, false), H);
    press(&mut app, 'l');
    for _ in 0..5 {
        press(&mut app, 'j'); // the cursor sits mid-file
    }
    let cursor = app.review_cursor();
    dump_frame(&app, W, H).unwrap();

    // Wheel into the strip without crossing: `o ∈ (max, R]` (§3.5).
    for _ in 0..16 {
        wheel_down(&mut app);
    }
    assert_eq!(app.selected, 0, "still anchored on a.txt");
    assert!(
        app.diff_scroll.get() > app.diff_max_scroll(),
        "the view is extended past the anchor's own bottom"
    );
    assert_eq!(
        app.review_cursor(),
        cursor,
        "the wheel never moved the cursor"
    );

    press(&mut app, 'j');
    assert_eq!(app.review_cursor(), cursor + 1, "the cursor stepped");
    assert_eq!(
        app.diff_scroll.get(),
        cursor + 1,
        "act-and-reveal snapped the viewport back to the cursor"
    );
}

#[test]
fn the_h_scroll_clamp_follows_a_walk_flip() {
    let repo = init_repo();
    // A wide, tall a.txt followed by a narrow, tall b.txt: tall enough on both
    // sides that the walk really renormalizes the anchor across the boundary.
    let wide: String = (0..60).map(|_| format!("{}\n", "x".repeat(400))).collect();
    write(repo.path(), "a.txt", &wide);
    write(
        repo.path(),
        "b.txt",
        &(0..40).map(|i| format!("beta {i}\n")).collect::<String>(),
    );
    let mut app = rendered_app(&repo, config(true, false), H);
    let d = app.diff_area();
    for _ in 0..4 {
        app.on_mouse(mouse(d.x + 2, d.y + 2, MouseEventKind::ScrollRight));
    }
    let shifted = app.diff_hscroll;
    assert!(shifted > 0, "a.txt is horizontally scrolled");
    assert_eq!(app.effective_hscroll(), shifted);

    press(&mut app, 'l');
    press(&mut app, 'G');
    press_until_flip(&mut app, 'j', W, H); // walk across the boundary
    assert_eq!(selected_path(&app), "b.txt");
    assert_eq!(
        app.diff_hscroll, shifted,
        "the offset itself is kept across the flip"
    );
    assert!(
        app.effective_hscroll() < shifted,
        "but it reads clamped against the arriving file's longest line"
    );

    press_until_flip(&mut app, 'k', W, H); // back to the wide file
    assert_eq!(
        app.effective_hscroll(),
        shifted,
        "walking back restores the full shift"
    );
}

// --- refresh + editing safety ----------------------------------------------

#[test]
fn reload_never_crosses() {
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    wheel_to_bottom(&mut app);
    let selected = app.selected;

    app.reload(); // a watcher-style refresh must never move the selection
    assert_eq!(app.selected, selected, "a refresh never crosses");
}

#[test]
fn no_crossing_while_the_editor_is_open() {
    let repo = short_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l'); // focus the diff
    press(&mut app, 'j'); // off the file-header row
    press(&mut app, 'j'); // off the hunk header, onto the first anchorable code row
    press(&mut app, 'c'); // open the in-place editor
    assert!(app.editor_open(), "the editor is open");
    dump_frame(&app, W, H).unwrap();

    wheel_down(&mut app);
    wheel_down(&mut app);
    assert_eq!(app.selected, 0, "scrolling while editing never crosses");
    assert!(app.editor_open(), "the editor stays open");
}

// --- keyboard: list-focused half page (continuous, §3.5) -------------------

#[test]
fn list_focused_ctrl_d_streams_through_the_boundary() {
    let repo = handoff_repo(); // a.txt tall, b.txt deep enough to lead the pane
    let mut app = rendered_app(&repo, config(true, false), H);
    let rows = stream_rows(&mut app, H);
    let half = (viewport_at(H) / 2).max(1);

    // Every press advances exactly a half page in the flattened stream — no
    // clamp-at-the-edge pause, no arming tick, and the cursor is never touched.
    for press_no in 1..=7 {
        ctrl(&mut app, 'd');
        assert_eq!(
            flat_top(&app, &rows),
            half * press_no,
            "press {press_no} advanced one half page"
        );
    }
    assert_eq!(app.selected, 1, "the anchor flipped at the boundary");
    assert_eq!(app.diff_scroll.get(), half * 7 - rows[0]);

    ctrl(&mut app, 'u');
    assert_eq!(
        flat_top(&app, &rows),
        half * 6,
        "and back, just as continuous"
    );
    assert_eq!(app.selected, 0, "the anchor flipped back");
}

#[test]
fn list_focused_half_page_disabled_clamps() {
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(false, false));
    dump_frame(&app, W, H).unwrap();
    for _ in 0..30 {
        ctrl(&mut app, 'd');
    }
    let max = app.diff_max_scroll();
    assert_eq!(
        app.selected, 0,
        "disabled list-focused ctrl-d never crosses"
    );
    assert_eq!(app.diff_scroll.get().min(max), max, "it clamps at the edge");
}

#[test]
fn review_list_focused_ctrl_d_streams_through_the_boundary() {
    let (_repo, mut app) = review_app(true);
    let h = 8; // a 4-row viewport: the review's small diffs still form a stream
    dump_frame(&app, W, h).unwrap();
    assert_eq!(app.review_selected(), 0);
    let first_rows = app.diff_row_count();
    let half = (viewport_at(h) / 2).max(1);

    ctrl(&mut app, 'd');
    assert_eq!((app.review_selected(), app.diff_scroll.get()), (0, half));
    ctrl(&mut app, 'd');
    assert_eq!(
        app.review_selected(),
        1,
        "list-focused ctrl-d streams across the boundary"
    );
    assert_eq!(app.diff_scroll.get(), 2 * half - first_rows);

    ctrl(&mut app, 'u');
    assert_eq!(app.review_selected(), 0, "and back");
    assert_eq!(app.diff_scroll.get(), half);
}

// --- Review view -----------------------------------------------------------

fn review_app(cross_file: bool) -> (TempDir, App) {
    let repo = init_repo_with_diverged_branches();
    let app = App::for_review(
        repo.path().to_path_buf(),
        &config(cross_file, false),
        "main",
    )
    .unwrap();
    (repo, app)
}

#[test]
fn review_wheel_streams_across_files_and_back() {
    let (_repo, mut app) = review_app(true);
    let h = 8; // a 4-row viewport: the review's small diffs still form a stream
    dump_frame(&app, W, h).unwrap();
    assert!(app.review_files().len() >= 2, "a multi-file review");
    let first_rows = app.diff_row_count();

    // Down past the first file's last row: the next file's header leads, then
    // one more tick flips the anchor to it.
    app.diff_scroll.set(first_rows - 1);
    prepare_window(&mut app);
    app.wheel_scroll_window(1);
    assert_eq!(app.review_selected(), 0, "the header is at the top");
    app.wheel_scroll_window(1);
    assert_eq!(app.review_selected(), 1, "crossed to the next review file");
    assert_eq!(app.diff_scroll.get(), 1, "one row past the top");

    // And straight back: `(B, 0)` first, then the previous file's tail.
    app.wheel_scroll_window(-1);
    assert_eq!((app.review_selected(), app.diff_scroll.get()), (1, 0));
    app.wheel_scroll_window(-1);
    assert_eq!(app.review_selected(), 0, "crossed back");
    assert_eq!(app.diff_scroll.get(), first_rows - 1);
    dump_frame(&app, W, h).unwrap();
}

#[test]
fn review_disabled_never_hops() {
    let (_repo, mut app) = review_app(false);
    dump_frame(&app, W, H).unwrap();
    wheel_to_bottom(&mut app);
    for _ in 0..5 {
        wheel_down(&mut app);
    }
    assert_eq!(app.review_selected(), 0, "disabled review never crosses");
}

#[test]
fn review_walks_both_directions() {
    let (_repo, mut app) = review_app(true);
    let h = 8; // a 4-row viewport, so the walk has a boundary to cross
    dump_frame(&app, W, h).unwrap();
    let v = viewport_at(h);
    let first_rows = app.diff_row_count();
    let first = app.review_files()[0].path.clone();
    let second = app.review_files()[1].path.clone();
    let second_id = FileId::Review {
        path: second.clone(),
    };

    press(&mut app, 'l'); // focus the diff
    press(&mut app, 'G'); // the first file's last stop
    press(&mut app, 'j');
    assert_eq!(
        cursor_at(&app),
        (second_id, RowTarget::FileHeader),
        "the walk stepped onto the next review file's header"
    );
    assert_eq!(
        app.review_selected(),
        0,
        "with the anchor still on the first"
    );
    let (title, body) = frame(&app, h);
    assert!(title.contains(&first), "the title has not flipped: {title}");
    assert!(
        body_text(&body, v - 1).contains(&second),
        "and the arriving header's band is the bottom row"
    );
    assert!(
        body_text(&body, v - 2).trim_end().chars().all(|c| c == '─'),
        "its rule row arrived with it: {}",
        body_text(&body, v - 2)
    );

    // The anchor follows once the reveal renormalizes past the boundary — one
    // press sooner than with a one-row header, because the step onto the header
    // revealed both of its rows at once (plan 008 §3.5).
    let presses = press_until_flip(&mut app, 'j', W, h);
    assert_eq!(presses, v - 1, "one press per row past the first stop");
    assert_eq!(app.review_selected(), 1);
    // Two rows past the top, not one: the press that flips walks clear of the
    // (short) second file onto the *third* file's header, and revealing that
    // two-row target pulls both of its rows in at once (plan 008 §3.5).
    assert_eq!(
        app.diff_scroll.get(),
        2,
        "past the top by the header it revealed"
    );
    let (title, _) = frame(&app, h);
    assert!(title.contains(&second), "the title flipped: {title}");

    // And back: `k` walks down-stream to the boundary, then flips on the stop
    // above the anchor's first.
    press_until_flip(&mut app, 'k', W, h);
    assert_eq!(app.review_selected(), 0, "and back the other way");
    assert_eq!(
        app.diff_scroll.get(),
        first_rows - 1,
        "top-aligned on the previous file's last target"
    );
    assert_eq!(app.review_cursor(), first_rows - 1);
    let (title, body) = frame(&app, h);
    assert!(
        title.contains(&first),
        "the title followed the anchor: {title}"
    );
    assert!(
        body_text(&body, 1).trim_end().chars().all(|c| c == '─'),
        "the file just left is separated by its rule row: {}",
        body_text(&body, 1)
    );
    assert!(
        body_text(&body, 2).contains(&second),
        "and its band continues below that: {}",
        body_text(&body, 2)
    );
}
