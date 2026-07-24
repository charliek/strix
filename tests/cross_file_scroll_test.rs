//! Cross-file scroll (plan 005 §3.4): scrolling past the end of one file's diff
//! crosses into the next file's diff, past the top into the previous one, in the
//! Status and Review views. Off by default; the History view is excluded.
//!
//! The wheel/keyboard paths are exercised with synthetic events against the same
//! `TestBackend` render path a real session uses. A hop is an ordinary selection
//! change plus a placement: down lands the arriving diff at its top, up at its
//! bottom (parked at the `usize::MAX` sentinel every reader clamps).

mod common;

use common::{init_repo, init_repo_with_diverged_branches, press, write};
use strix::app::App;
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use strix::terminal::dump_frame;
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 24;

// --- construction ----------------------------------------------------------

fn config(cross_file: bool, wrap: bool) -> Config {
    Config {
        cross_file_scroll: Some(cross_file),
        wrap_lines: Some(wrap),
        ..Config::default()
    }
}

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

/// Two short untracked files, so every diff has `max_scroll == 0`.
fn short_status_repo() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "b.txt", "one\ntwo\n");
    write(repo.path(), "c.txt", "x\ny\n");
    repo
}

fn app_for(repo: &TempDir, cfg: Config) -> App {
    App::with_config(repo.path().to_path_buf(), &cfg).unwrap()
}

// --- event helpers ---------------------------------------------------------

fn mouse(col: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn wheel_down(app: &mut App) {
    let d = app.diff_area();
    app.on_mouse(mouse(d.x + 2, d.y + 2, MouseEventKind::ScrollDown));
}

fn wheel_up(app: &mut App) {
    let d = app.diff_area();
    app.on_mouse(mouse(d.x + 2, d.y + 2, MouseEventKind::ScrollUp));
}

fn mouse_move(app: &mut App) {
    let d = app.diff_area();
    app.on_mouse(mouse(d.x + 2, d.y + 2, MouseEventKind::Moved));
}

fn ctrl(app: &mut App, ch: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
}

/// Wheel down until the offset is clamped at the bottom, leaving the arming
/// un-armed (every tick here is below the edge, so it scrolls, never records).
/// A no-op when the diff already fits (`max_scroll == 0`).
fn wheel_to_bottom(app: &mut App) {
    for _ in 0..200 {
        let max = app.diff_max_scroll();
        if app.diff_scroll.get().min(max) >= max {
            break;
        }
        wheel_down(app);
    }
}

fn selected_path(app: &App) -> String {
    app.selected_file()
        .map(|(_, e)| e.path.clone())
        .unwrap_or_default()
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

// --- wheel: Status ---------------------------------------------------------

#[test]
fn wheel_down_at_bottom_arms_then_hops_to_next_file_top() {
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.selected, 0);
    assert_eq!(selected_path(&app), "a.txt", "tall file first");

    wheel_to_bottom(&mut app);
    assert!(
        app.diff_max_scroll() > 0,
        "a.txt is taller than the viewport"
    );

    // The tick that first arrives at the edge only records it — it must not hop.
    wheel_down(&mut app);
    assert_eq!(app.selected, 0, "arriving-at-edge tick arms, does not hop");

    // A subsequent tick at the recorded edge crosses into the next file, landing
    // at its top.
    wheel_down(&mut app);
    assert_eq!(app.selected, 1, "hopped to the next file");
    assert_eq!(selected_path(&app), "b.txt");
    assert_eq!(app.diff_scroll.get(), 0, "landed at the top");
    assert_eq!(app.review_cursor(), 0, "cursor on the first row");
}

#[test]
fn wheel_up_at_top_hops_to_previous_file_bottom() {
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    // Move the selection to b.txt (a short file), then render for its metrics.
    press(&mut app, 'j');
    assert_eq!(selected_path(&app), "b.txt");
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.diff_scroll.get(), 0, "a fresh file starts at the top");

    // At the top edge: the first up tick arms, the second crosses back.
    wheel_up(&mut app);
    assert_eq!(app.selected, 1, "arming tick does not hop");
    wheel_up(&mut app);
    assert_eq!(app.selected, 0, "hopped back to the previous file");
    assert_eq!(selected_path(&app), "a.txt");
    assert_eq!(
        app.diff_scroll.get(),
        usize::MAX,
        "up hop parks at the bottom sentinel"
    );

    // Render clamps the sentinel to the real max, and the cursor sits on the last
    // physical row.
    dump_frame(&app, W, H).unwrap();
    let max = app.diff_max_scroll();
    assert!(max > 0);
    assert_eq!(
        app.diff_scroll.get().min(max),
        max,
        "clamps to the bottom on read"
    );
    assert_eq!(
        app.review_cursor(),
        app.diff_row_count() - 1,
        "cursor on the last row"
    );
}

#[test]
fn short_diff_arms_then_hops_without_cascading() {
    let repo = short_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.diff_max_scroll(), 0, "a short diff fits the viewport");
    assert_eq!(app.selected, 0);

    // One tick against a max==0 diff only arms — it must not cascade across files.
    wheel_down(&mut app);
    assert_eq!(app.selected, 0, "a single tick arms, no fling");
    // The next tick crosses exactly one boundary.
    wheel_down(&mut app);
    assert_eq!(app.selected, 1, "the second tick hops one file");
    assert_eq!(app.diff_scroll.get(), 0);
}

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

#[test]
fn last_file_wheel_down_clamps_no_wraparound() {
    let repo = short_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    // Move to the last file.
    let last = app.status.total() - 1;
    for _ in 0..last {
        press(&mut app, 'j');
    }
    assert_eq!(app.selected, last);
    dump_frame(&app, W, H).unwrap();

    // Arm + attempt to cross past the end: it clamps, no wraparound.
    wheel_down(&mut app);
    wheel_down(&mut app);
    assert_eq!(app.selected, last, "no hop past the last file");
}

// --- keyboard: Status ------------------------------------------------------

#[test]
fn keyboard_j_at_hard_edge_crosses() {
    let repo = short_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l'); // focus the diff
    press(&mut app, 'G'); // cursor to the last row (a short diff → at the hard edge)
    assert_eq!(app.selected, 0);

    press(&mut app, 'j');
    assert_eq!(app.selected, 1, "j at the hard edge crosses immediately");
    assert_eq!(app.diff_scroll.get(), 0, "landed at the top");
    assert_eq!(app.review_cursor(), 0);
}

#[test]
fn keyboard_k_at_top_crosses_landing_bottom() {
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'j'); // select b.txt
    assert_eq!(selected_path(&app), "b.txt");
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l'); // focus the diff (cursor at the top)

    press(&mut app, 'k');
    assert_eq!(app.selected, 0, "k at the top crosses to the previous file");
    assert_eq!(selected_path(&app), "a.txt");
    assert_eq!(
        app.diff_scroll.get(),
        usize::MAX,
        "an up hop lands at the bottom"
    );
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.review_cursor(), app.diff_row_count() - 1);
}

#[test]
fn first_file_keyboard_up_clamps() {
    let repo = short_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l');
    press(&mut app, 'k'); // at the first file, top edge
    assert_eq!(app.selected, 0, "no wraparound off the first file");
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
        "no hop while there is more of the line to see"
    );
    assert!(
        app.diff_scroll.get() > before,
        "the step scrolled within the tall target"
    );

    // Keep stepping: once the viewport reaches the hard edge, the next step hops.
    for _ in 0..500 {
        if app.selected != 0 {
            break;
        }
        press(&mut app, 'j');
    }
    assert_eq!(app.selected, 1, "crosses once the hard edge is reached");
    assert_eq!(
        app.diff_scroll.get(),
        0,
        "the arriving file lands at the top"
    );
}

#[test]
fn empty_binary_diff_keyboard_crossing() {
    let repo = init_repo();
    write(repo.path(), "bin.dat", "a\0b\0c\n"); // NUL bytes → a binary diff
    write(repo.path(), "z.txt", "text\n");
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    assert_eq!(selected_path(&app), "bin.dat");
    assert_eq!(app.diff_row_count(), 0, "a binary diff has no code rows");

    press(&mut app, 'l'); // focus the diff
    press(&mut app, 'j'); // an empty diff is an immediate boundary
    assert_eq!(app.selected, 1, "crossed off the empty diff");
    assert_eq!(selected_path(&app), "z.txt");
}

// --- refresh + editing safety ----------------------------------------------

#[test]
fn reload_never_hops() {
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    wheel_to_bottom(&mut app);
    wheel_down(&mut app); // arm at the bottom edge (no hop yet)
    assert_eq!(app.selected, 0);

    app.reload(); // a watcher-style refresh must never cross a boundary
    assert_eq!(app.selected, 0, "a refresh never hops");
}

#[test]
fn reload_clears_the_wheel_arm() {
    // FIX 2: a reload landing between two ticks must not leave the arm set, or the
    // next tick would hop instantly. The tick after a reload only re-arms.
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    wheel_to_bottom(&mut app);
    wheel_down(&mut app); // arm
    assert_eq!(app.selected, 0);

    app.reload();
    wheel_down(&mut app); // arm was cleared → this only re-arms
    assert_eq!(
        app.selected, 0,
        "the tick after a reload re-arms, does not hop"
    );
    wheel_down(&mut app); // now armed → hop
    assert_eq!(app.selected, 1, "the following tick crosses");
}

#[test]
fn an_unrelated_event_clears_the_wheel_arm() {
    // FIX 3: only consecutive wheel ticks over the diff keep the arm. A mouse move
    // (or a resize) between two ticks clears it, so the next tick re-arms.
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    wheel_to_bottom(&mut app);

    wheel_down(&mut app); // arm
    mouse_move(&mut app); // an unrelated event clears the arm
    wheel_down(&mut app); // re-arms, does not hop
    assert_eq!(app.selected, 0, "a mouse move between ticks breaks the arm");

    // A resize likewise clears it.
    wheel_down(&mut app); // now armed → would hop
    assert_eq!(app.selected, 1, "consecutive ticks still cross");
    dump_frame(&app, W, H).unwrap();
    wheel_to_bottom(&mut app);
    wheel_down(&mut app); // arm on the new file
    app.on_resize(); // clears the arm
    wheel_down(&mut app);
    assert_eq!(app.selected, 1, "a resize between ticks breaks the arm");
}

#[test]
fn queued_wheel_ticks_do_not_skip_through_a_tall_destination() {
    // FIX 1: after a hop the destination diff is synced but the render-time metrics
    // still describe the source; the event loop drains queued ticks before the next
    // redraw. Landing on a tall destination, those queued ticks must scroll within
    // it, not fling straight through to the file beyond.
    let repo = init_repo();
    write(repo.path(), "s.txt", "one\ntwo\n"); // short source (index 0)
    let tall: String = (0..60).map(|i| format!("line {i}\n")).collect();
    write(repo.path(), "t.txt", &tall); // tall destination (index 1)
    write(repo.path(), "u.txt", "x\ny\n"); // a file beyond (index 2)
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    assert_eq!(selected_path(&app), "s.txt");

    wheel_down(&mut app); // arm at the short source's edge (max == 0)
                          // A whole batch drained before any redraw: the first tick hops into t.txt, the
                          // rest must scroll within it — never reach u.txt.
    wheel_down(&mut app);
    wheel_down(&mut app);
    wheel_down(&mut app);
    assert_eq!(
        selected_path(&app),
        "t.txt",
        "landed in the tall destination"
    );
    assert_eq!(app.selected, 1, "did not skip through to the file beyond");
}

// --- keyboard: list-focused half page (FIX 4) ------------------------------

#[test]
fn list_focused_ctrl_d_at_bottom_crosses() {
    let repo = multi_status_repo(); // a.txt (tall) is index 0, staging focused
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.selected, 0);

    // Half-page down through the tall diff with the file list focused (the default).
    for _ in 0..200 {
        let max = app.diff_max_scroll();
        if app.diff_scroll.get().min(max) >= max {
            break;
        }
        ctrl(&mut app, 'd');
        assert_eq!(app.selected, 0, "must not cross before reaching the bottom");
    }
    ctrl(&mut app, 'd'); // pinned at the bottom → cross
    assert_eq!(app.selected, 1, "list-focused ctrl-d at the bottom crosses");
    assert_eq!(
        app.diff_scroll.get(),
        0,
        "the arriving file lands at the top"
    );
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
fn review_list_focused_ctrl_d_crosses() {
    let (_repo, mut app) = review_app(true); // list-focused by default
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.review_selected(), 0);

    // The review diffs are short (already at the bottom), so a half-page press is
    // pinned at the edge and crosses.
    ctrl(&mut app, 'd');
    assert_eq!(
        app.review_selected(),
        1,
        "list-focused ctrl-d crosses in review"
    );
    assert_eq!(app.diff_scroll.get(), 0);
}

#[test]
fn no_hop_while_editor_open() {
    let repo = short_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l'); // focus the diff
    press(&mut app, 'j'); // move onto a code row (off the hunk header)
    press(&mut app, 'c'); // open the in-place editor
    assert!(app.editor_open(), "the editor is open");
    dump_frame(&app, W, H).unwrap();

    wheel_down(&mut app);
    wheel_down(&mut app);
    assert_eq!(app.selected, 0, "scrolling while editing never hops");
    assert!(app.editor_open(), "the editor stays open");
}

// --- staged↔unstaged same-path hop -----------------------------------------

#[test]
fn staged_unstaged_same_path_hop_lands_top() {
    // A file that is both staged (a first change) and unstaged (a later change)
    // appears in both sections, so a down hop crosses within the same path — the
    // diff cache hits, but the placement still lands it at the top.
    let repo = init_repo();
    write(repo.path(), "dup.txt", "one\ntwo\n");
    common::git(repo.path(), &["add", "dup.txt"]);
    write(repo.path(), "dup.txt", "one\ntwo\nthree\n");
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.status.total(), 2, "dup.txt appears staged and unstaged");
    assert_eq!(app.selected, 0);
    let path0 = selected_path(&app);

    // Scroll to the bottom of the current diff, then arm + hop.
    wheel_to_bottom(&mut app);
    wheel_down(&mut app);
    wheel_down(&mut app);
    assert_eq!(app.selected, 1, "hopped to the same path's other section");
    assert_eq!(selected_path(&app), path0, "same path, different section");
    assert_eq!(app.diff_scroll.get(), 0, "still lands at the top");

    // No stale placement leaks: a later up hop lands cleanly at the bottom.
    dump_frame(&app, W, H).unwrap();
    wheel_up(&mut app);
    wheel_up(&mut app);
    assert_eq!(app.selected, 0);
    assert_eq!(
        app.diff_scroll.get(),
        usize::MAX,
        "a fresh up hop, not stale"
    );
}

// --- laziness --------------------------------------------------------------

#[test]
fn hop_computes_only_the_destination_diff() {
    let repo = multi_status_repo();
    let mut app = app_for(&repo, config(true, false));
    dump_frame(&app, W, H).unwrap();
    wheel_to_bottom(&mut app);
    wheel_down(&mut app); // arm

    let before = app.diff_compute_count();
    wheel_down(&mut app); // hop to exactly one neighbour
    let after = app.diff_compute_count();
    assert_eq!(app.selected, 1, "hopped");
    assert_eq!(
        after - before,
        1,
        "only the destination file's diff computed"
    );
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
fn review_wheel_down_hops_to_next_file_top() {
    let (_repo, mut app) = review_app(true);
    dump_frame(&app, W, H).unwrap();
    assert!(app.review_files().len() >= 2, "a multi-file review");
    assert_eq!(app.review_selected(), 0);

    wheel_to_bottom(&mut app);
    wheel_down(&mut app); // arm
    assert_eq!(app.review_selected(), 0, "arming tick does not hop");
    wheel_down(&mut app); // hop
    assert_eq!(app.review_selected(), 1, "crossed to the next review file");
    assert_eq!(app.diff_scroll.get(), 0, "landed at the top");
}

#[test]
fn review_wheel_up_hops_to_previous_file_bottom() {
    let (_repo, mut app) = review_app(true);
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'j'); // move the list selection to the second file
    assert_eq!(app.review_selected(), 1);
    dump_frame(&app, W, H).unwrap();

    wheel_up(&mut app); // arm at the top
    assert_eq!(app.review_selected(), 1);
    wheel_up(&mut app); // hop back
    assert_eq!(
        app.review_selected(),
        0,
        "crossed back to the previous file"
    );
    assert_eq!(
        app.diff_scroll.get(),
        usize::MAX,
        "an up hop lands at the bottom"
    );
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
