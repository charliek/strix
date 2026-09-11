mod common;

use common::{git, init_repo, write};
use strix::app::{App, Focus};
use strix::crossterm::event::{KeyCode, KeyEvent};
use strix::git::FileDiff;
use strix::terminal::dump_frame;

const W: u16 = 120;
const H: u16 = 30;

/// The current diff's text joined into one string, for content assertions.
fn diff_text(app: &App) -> String {
    match &app.current_diff {
        Some(FileDiff::Text(lines)) => lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[test]
fn reload_recomputes_the_open_diff_after_an_in_place_edit() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "file.txt", "original\n");
    git(path, &["add", "file.txt"]);
    git(path, &["commit", "-q", "-m", "add file"]);

    // One unstaged modification; it's the only change, so it's selected.
    write(path, "file.txt", "edited-one\n");
    let mut app = App::new(path.to_path_buf()).expect("app");
    assert!(
        diff_text(&app).contains("edited-one"),
        "diff shows the first edit"
    );

    // Edit again in place — same path, same section — so the diff key is
    // unchanged. Only forcing a recompute on reload picks this up.
    write(path, "file.txt", "edited-two\n");
    app.reload();
    let text = diff_text(&app);
    assert!(
        text.contains("edited-two"),
        "reload picks up the in-place edit"
    );
    assert!(!text.contains("edited-one"), "the stale diff is gone");
}

#[test]
fn refresh_keeps_the_cursor_on_the_same_file_when_the_list_shifts() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "m.txt", "1\n");
    let mut app = App::new(path.to_path_buf()).expect("app");
    assert_eq!(
        app.selected_file().map(|(_, e)| e.path.clone()).as_deref(),
        Some("m.txt"),
    );

    // A new untracked file appears ahead of it in the list; an index-based
    // cursor would now point at the wrong file.
    write(path, "a.txt", "1\n");
    app.reload();

    assert_eq!(
        app.selected_file().map(|(_, e)| e.path.clone()).as_deref(),
        Some("m.txt"),
        "the cursor follows the file by path, not by index"
    );
}

#[test]
fn reload_keeps_the_scroll_position_for_the_same_file() {
    let repo = init_repo();
    let path = repo.path();
    let lines: String = (0..40).map(|i| format!("line {i}\n")).collect();
    write(path, "big.txt", &lines);
    git(path, &["add", "big.txt"]);
    git(path, &["commit", "-q", "-m", "add big"]);
    // Modify every line so the diff is taller than the viewport.
    let edited: String = (0..40).map(|i| format!("line {i} edited\n")).collect();
    write(path, "big.txt", &edited);

    let mut app = App::new(path.to_path_buf()).expect("app");
    app.focus = Focus::Diff;
    // Render once to record the diff viewport, so scrolling can advance.
    let _ = dump_frame(&app, 80, 24).expect("dump_frame");
    // The diff pane is cursor-driven: jump the cursor to the last row (`G`), whose
    // act-and-reveal scrolls the viewport down past the top.
    app.on_key(KeyEvent::from(KeyCode::Char('G')));
    let scrolled = app.diff_scroll.get();
    assert!(scrolled > 0, "scrolled down into the diff");

    // An external in-place edit (as the watcher would trigger) reloads the same
    // file — the scroll must not jump back to the top.
    let edited_again: String = (0..40)
        .map(|i| format!("line {i} edited again\n"))
        .collect();
    write(path, "big.txt", &edited_again);
    app.reload();

    assert_eq!(
        app.diff_scroll.get(),
        scrolled,
        "reloading the open file keeps the scroll position"
    );
}

// --- C5 (issue #37): reload cost bounds --------------------------------------
//
// Before/after deltas around one isolated `reload()`, following the U2
// convention (`tests/history_stream_test.rs`) — construction and the first
// render already read, so absolute counts are brittle.

/// The git-layer counters Review's churn guard is judged by.
fn git_counts(app: &App) -> (u64, u64, u64) {
    (
        app.repo.subprocess_count(),
        app.repo.object_read_count(),
        app.repo.spec_diff_count(),
    )
}

#[test]
fn a_review_reload_on_an_unmoved_range_computes_no_diffs() {
    let (_repo, mut app) = common::review_app("main");
    let _ = common::dump(&app, W, H);
    common::prepare_window(&mut app);
    let (sub, obj, spec) = git_counts(&app);
    let compute = app.diff_compute_count();

    // Nothing on disk moved: the resolved (base, head) is unchanged, so the
    // churn guard in `refresh_review` returns before relisting or touching any
    // file's diff.
    app.reload();

    assert_eq!(
        app.repo.spec_diff_count() - spec,
        0,
        "the churn guard keeps the list and every cached diff"
    );
    assert_eq!(
        app.repo.object_read_count() - obj,
        0,
        "no diff recomputed means no blob read either"
    );
    assert!(
        app.repo.subprocess_count() - sub <= 1,
        "at most the range re-resolution's own probe, no relist"
    );
    assert_eq!(
        app.diff_compute_count() - compute,
        0,
        "nor is any diff rebuilt in-process from a warm blob (review finding)"
    );
}

#[test]
fn a_status_reload_on_an_identical_snapshot_recomputes_only_the_window() {
    let repo = common::three_modified_files();
    let mut app = common::app_for(&repo, common::config(true, false));
    let _ = common::dump(&app, W, H);
    common::prepare_window(&mut app);

    // The window's own file set, not `cached_section_count()` (which also
    // counts stale/off-window LRU entries): the anchor segment plus every strip
    // segment that actually holds a prepared section.
    let window = common::window_of(&app);
    let file_count = window
        .segments
        .iter()
        .filter(|segment| segment.is_anchor() || segment.section.is_some())
        .count();
    assert!(
        file_count > 1,
        "the three-file fixture fills the anchor plus at least one strip segment"
    );
    let ids_before: Vec<_> = window.segments.iter().map(|s| s.id.clone()).collect();

    let sub = app.repo.subprocess_count();
    let compute = app.diff_compute_count();

    // Nothing on disk changed, but Status's per-tick cost is deliberately "every
    // window section recomputed" (plan §9 leaves scoping this to future work):
    // the refresh bumps `stream_generation` once, retiring every cached section,
    // and `reload`'s `sync_active` re-prepares exactly the window that was open.
    app.reload();

    assert_eq!(
        app.repo.subprocess_count() - sub,
        1,
        "exactly the one `git status` read"
    );
    assert_eq!(
        app.diff_compute_count() - compute,
        file_count as u64,
        "every section the prepared window holds is rebuilt, nothing more"
    );

    let ids_after: Vec<_> = common::window_of(&app)
        .segments
        .iter()
        .map(|s| s.id.clone())
        .collect();
    assert_eq!(
        ids_after, ids_before,
        "the same files occupy the window, in the same order, after reload"
    );
}
