//! The stream's section machinery (plan 006 §3.3–3.4): file-parameterized
//! layouts, the bounded `Rc<FileSection>` cache with its window-pinned capacity
//! and `stream_generation` invalidation, and the `DiffWindow` the renderer
//! consumes in C3.
//!
//! The tests drive `ensure_diff_window` (the event-path fill) and `diff_window`
//! (the read-only assembly) directly, exactly the way the wheel path does since
//! C3 wired them into the event loop.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{
    init_repo, init_repo_with_diverged_branches, init_repo_with_history, press, strix_dir,
    three_modified_files, write,
};
use strix::app::{App, DiffWindow, FileId, RowContent, RowTarget};
use strix::comments::{Branch, Comment, Scope, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent};
use strix::terminal::dump_frame;
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 30;

fn config(wrap: bool) -> Config {
    Config {
        cross_file_scroll: Some(true),
        wrap_lines: Some(wrap),
        line_numbers: Some(true),
        ..Config::default()
    }
}

/// An app on `repo` with one frame already rendered, so the diff pane's geometry
/// (the width every layout is keyed by) is known.
fn app_for(repo: &TempDir, wrap: bool) -> App {
    app_sized(repo, wrap, H)
}

/// The same, rendered into a taller terminal — a viewport deep enough to need
/// more sections at once than the LRU budget keeps.
fn app_sized(repo: &TempDir, wrap: bool, height: u16) -> App {
    let app = App::with_config(repo.path().to_path_buf(), &config(wrap)).unwrap();
    dump_frame(&app, W, height).unwrap();
    app
}

fn review_app(repo: &TempDir) -> App {
    let app = App::for_review(repo.path().to_path_buf(), &config(false), "main").unwrap();
    dump_frame(&app, W, H).unwrap();
    app
}

/// Prepare the sections the pane-sized window needs, then assemble it — the
/// event-path/render-path pair, at the pane's own geometry.
fn window(app: &mut App) -> DiffWindow {
    let area = app.diff_area();
    app.ensure_diff_window(area.width, area.height);
    app.diff_window(area.width, area.height)
}

/// The same pair at an explicit viewport height (the pane's width is what the
/// layout cache is keyed by, so that one always comes from the last render).
fn window_of_height(app: &mut App, height: u16) -> DiffWindow {
    let width = app.diff_area().width;
    app.ensure_diff_window(width, height);
    app.diff_window(width, height)
}

/// Three small untracked files, so every section is a handful of rows.
fn three_small_files() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "a.txt", "a1\na2\n");
    write(repo.path(), "b.txt", "b1\nb2\nb3\n");
    write(repo.path(), "c.txt", "c1\n");
    repo
}

/// Seed a review comment store, the schema the TUI and the `strix comment` CLI
/// share (mirrors `comment_view_test`).
fn seed_store(repo: &Path, branch: &str, range: &str, comments: Vec<Comment>) {
    let mut branches = BTreeMap::new();
    branches.insert(
        branch.to_string(),
        Branch {
            active_range: Some(range.to_string()),
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

fn comment_on(id: u64, file: &str, line: usize, text: &str) -> Comment {
    Comment {
        scope: Scope::Range {
            range: "main".to_string(),
        },
        id,
        source: Source::Human,
        file: file.to_string(),
        side: Side::New,
        line,
        text: text.to_string(),
        context: None,
        orphaned: false,
        created_at: 1_700_000_000,
        base: None,
        stale: false,
    }
}

// --- window fill ------------------------------------------------------------

#[test]
fn the_window_fills_the_viewport_with_the_following_files() {
    let repo = three_small_files();
    let mut app = app_for(&repo, false);
    let anchor_rows = app.diff_row_count();

    let win = window(&mut app);
    assert_eq!(win.segments.len(), 3, "all three files fit the viewport");

    let anchor = &win.segments[0];
    assert!(anchor.is_anchor(), "segment 1 is the anchor");
    assert!(
        anchor.section.is_none(),
        "the anchor is read from the live layout, never the cache"
    );
    assert_eq!(anchor.id, app.active_file_id());
    assert_eq!(anchor.path, "a.txt");
    assert_eq!(anchor.row_range, 0..anchor_rows);

    for segment in &win.segments[1..] {
        let section = segment.section.as_ref().expect("a strip owns its section");
        assert_eq!(
            section.rows[0].target,
            RowTarget::FileHeader,
            "{} leads with its own header",
            segment.path
        );
        assert!(matches!(section.rows[0].content, RowContent::FileHeader(_)));
        assert_eq!(
            segment.row_range,
            0..section.rows.len(),
            "a fully-visible strip draws its whole section"
        );
        assert_eq!(segment.id.as_ref().map(FileId::path), Some(&*segment.path));
    }
    assert_eq!(win.segments[1].path, "b.txt");
    assert_eq!(win.segments[2].path, "c.txt");
    assert!(
        win.rows() < app.diff_area().height as usize,
        "the stream ran out before the viewport did — a shortfall, not a wrap"
    );
}

#[test]
fn the_window_stops_at_the_viewport_edge_mid_file() {
    let repo = three_small_files();
    let mut app = app_for(&repo, false);
    let anchor_rows = app.diff_row_count();
    let height = anchor_rows + 2;

    let win = window_of_height(&mut app, height as u16);
    assert_eq!(win.segments.len(), 2, "only the first neighbour is reached");
    assert_eq!(win.rows(), height, "the window fills the viewport exactly");
    assert_eq!(
        win.segments[1].row_range,
        0..2,
        "the last segment is cut at the viewport edge"
    );
    assert_eq!(
        app.cached_section_count(),
        1,
        "nothing beyond the viewport was computed"
    );
}

#[test]
fn the_history_details_row_is_the_single_anchor_segment() {
    let repo = init_repo_with_history();
    let mut app = app_for(&repo, false);
    press(&mut app, 'i');
    dump_frame(&app, W, H).unwrap();

    // History enters on the commit ● row, which is outside the stream (plan
    // 009 §3.2); the file rows below it are the stream proper.
    let win = window(&mut app);
    assert_eq!(win.segments.len(), 1, "the details row has no strip");
    assert!(win.segments[0].is_anchor());
    assert_eq!(win.segments[0].id, None, "and no stream identity");
    assert_eq!(app.cached_section_count(), 0);
}

// --- laziness ---------------------------------------------------------------

#[test]
fn scrolling_inside_one_file_computes_no_neighbour() {
    let repo = init_repo();
    let tall: String = (0..200).map(|i| format!("line {i}\n")).collect();
    write(repo.path(), "a_tall.txt", &tall);
    write(repo.path(), "b_next.txt", "next\n");
    let mut app = app_for(&repo, false);

    let area = app.diff_area();
    let before = app.diff_compute_count();
    app.ensure_diff_window(area.width, area.height);
    assert_eq!(
        app.diff_compute_count(),
        before,
        "o + V ≤ R: the neighbour is never touched"
    );
    assert_eq!(app.cached_section_count(), 0);

    // Deep inside the file, still short of its end.
    let rows = app.diff_row_count();
    app.diff_scroll.set(rows - area.height as usize - 1);
    app.ensure_diff_window(area.width, area.height);
    assert_eq!(app.diff_compute_count(), before, "still no neighbour");
    let win = app.diff_window(area.width, area.height);
    assert_eq!(win.segments.len(), 1, "the anchor alone fills the viewport");

    // One row further and the viewport reaches past the anchor's last row —
    // trigger (a) fires and exactly one neighbour is computed.
    app.diff_scroll.set(rows - area.height as usize + 1);
    app.ensure_diff_window(area.width, area.height);
    assert_eq!(
        app.diff_compute_count(),
        before + 1,
        "exactly the one file the window now needs"
    );
    let win = app.diff_window(area.width, area.height);
    assert_eq!(win.segments.len(), 2);
    assert_eq!(win.segments[1].path, "b_next.txt");
}

#[test]
fn a_prepared_section_is_reused_on_the_next_pass() {
    let repo = three_small_files();
    let mut app = app_for(&repo, false);
    window(&mut app);
    let computed = app.diff_compute_count();

    window(&mut app);
    assert_eq!(
        app.diff_compute_count(),
        computed,
        "re-filling the same window hits the cache"
    );
}

// --- capacity ---------------------------------------------------------------

/// 40 untracked binary files: each section is exactly its header, so one viewport
/// legitimately needs more sections than the LRU budget (32). The stream is 79
/// rows — the first file's header is a lone band, every other file's is a rule
/// plus a band (plan 008 §3.5) — so [`OVER_BUDGET_H`] is what it takes to still
/// show all 40 at once.
fn forty_binary_files() -> TempDir {
    let repo = init_repo();
    for i in 0..40 {
        write(repo.path(), &format!("f{i:02}.dat"), "a\0b\n");
    }
    repo
}

/// A frame tall enough for [`forty_binary_files`]' whole 79-row stream, so the
/// window still pins more sections than the 32-entry budget. A 50-row frame did
/// that while every header was one row; with two-row headers it reaches only 22
/// files, and the over-budget pinning this fixture exists to test stops being
/// exercised at all.
const OVER_BUDGET_H: u16 = 84;

#[test]
fn the_window_pins_more_sections_than_the_lru_budget() {
    let repo = forty_binary_files();
    let mut app = app_sized(&repo, false, OVER_BUDGET_H);
    assert!(
        app.diff_area().height as usize >= 79,
        "the fixture needs a viewport deeper than the whole stream"
    );
    let win = window(&mut app);

    assert_eq!(app.stream_file_count(), 40);
    assert_eq!(
        win.segments.len(),
        40,
        "every header-only file fits this viewport"
    );
    assert_eq!(
        app.cached_section_count(),
        39,
        "every section the window needs survives the budget of 32"
    );
    for segment in &win.segments[1..] {
        let section = segment.section.as_ref().expect("a strip owns its section");
        assert_eq!(
            section.rows.len(),
            2,
            "a binary file below the stream's first is header-only: rule + band"
        );
        assert_eq!(section.rows[0].target, RowTarget::FileHeader);
        assert_eq!(section.rows[1].target, RowTarget::FileHeader);
    }
}

#[test]
fn sections_the_window_no_longer_needs_are_lru_evicted() {
    let repo = forty_binary_files();
    let mut app = app_sized(&repo, false, OVER_BUDGET_H);
    window(&mut app);
    assert_eq!(app.cached_section_count(), 39);

    // Anchor on the last file: the window needs no strip at all, so only the
    // budget applies and the 7 least-recently-used entries go.
    for _ in 0..39 {
        press(&mut app, 'j');
    }
    window(&mut app);
    assert_eq!(app.cached_section_count(), 32, "trimmed to the LRU budget");

    // Back to the top: since C3 every selection change re-fills the window on the
    // event path (`sync_active`), so the evicted tail is recomputed during the
    // walk itself — by the time the window is assembled it needs nothing more,
    // and every file the viewport shows is pinned again.
    for _ in 0..39 {
        press(&mut app, 'k');
    }
    let computed = app.diff_compute_count();
    window(&mut app);
    assert_eq!(
        app.diff_compute_count(),
        computed,
        "the walk back already prepared everything the window needs"
    );
    assert_eq!(app.cached_section_count(), 39);
}

// --- invalidation -----------------------------------------------------------

#[test]
fn a_staging_mutation_retires_every_cached_section() {
    let repo = three_modified_files();
    let mut app = app_for(&repo, false);
    let win = window(&mut app);
    assert_eq!(win.segments.len(), 3);
    assert_eq!(app.cached_section_count(), 2);
    let generation = app.stream_generation();
    let computed = app.diff_compute_count();

    press(&mut app, 's'); // stage the selected file (refresh, no reload)
    assert!(
        app.stream_generation() > generation,
        "the status snapshot was replaced"
    );

    window(&mut app);
    assert!(
        app.diff_compute_count() > computed,
        "the stale sections were discarded and rebuilt"
    );
    assert_eq!(
        app.cached_section_count(),
        2,
        "the stale entries were replaced, not accumulated"
    );
}

#[test]
fn a_comment_mutation_retires_every_cached_section() {
    let repo = three_modified_files();
    let mut app = app_for(&repo, false);
    window(&mut app);
    let generation = app.stream_generation();
    let computed = app.diff_compute_count();

    press(&mut app, 'l'); // focus the diff; the cursor starts on the header row
    press(&mut app, 'j'); // the hunk header
    press(&mut app, 'j'); // the first code line
    press(&mut app, 'c');
    assert!(app.editor_open(), "the editor opened on a code row");
    for ch in "note".chars() {
        press(&mut app, ch);
    }
    app.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(!app.editor_open());
    assert_eq!(app.status_comment_count("a.txt"), 1, "the note was stored");
    assert!(
        app.stream_generation() > generation,
        "a comment mutation invalidates the stream"
    );

    window(&mut app);
    assert!(
        app.diff_compute_count() > computed,
        "the neighbours' sections — which carry their own comment boxes — rebuilt"
    );
}

#[test]
fn a_layout_toggle_invalidates_by_tag_without_a_generation_bump() {
    let repo = three_small_files();
    let mut app = app_for(&repo, false);
    window(&mut app);
    let generation = app.stream_generation();
    let computed = app.diff_compute_count();

    press(&mut app, 'd'); // side-by-side: a different layout key
    dump_frame(&app, W, H).unwrap();
    assert_eq!(
        app.stream_generation(),
        generation,
        "a layout-key change needs no bump"
    );
    window(&mut app);
    assert!(
        app.diff_compute_count() > computed,
        "the tagged sections no longer match and were rebuilt"
    );
}

// --- per-file layout inputs -------------------------------------------------

#[test]
fn each_file_wraps_at_its_own_line_number_gutter() {
    // `b_big.txt` has 5-digit line numbers (a 12-column gutter); the anchor has
    // 4-digit ones (10 columns). Its last line is sized to fit on one row at the
    // anchor's gutter and to need two at its own — so the section's row count is
    // only right if the layout was built with *its* number width.
    let repo = init_repo();
    write(repo.path(), "a_small.txt", "one\ntwo\n");
    let app = app_for(&repo, true);
    let pane = app.diff_area().width as usize;
    let content_five = pane - (2 * 5 + 2) - 2;
    let long = "x".repeat(content_five + 1);
    let mut body: String = (0..10_000).map(|_| "y\n".to_string()).collect();
    body.push_str(&long);
    body.push('\n');
    write(repo.path(), "b_big.txt", &body);

    let mut app = app_for(&repo, true);
    let win = window(&mut app);
    let section = win.segments[1]
        .section
        .as_ref()
        .expect("the big file is a strip");
    // rule + band + hunk header + 10 000 one-row lines + the two-row long line
    // (`b_big.txt` sorts second, so its header carries a rule row).
    assert_eq!(
        section.rows.len(),
        2 + 1 + 10_000 + 2,
        "the long line wrapped at the big file's own content width"
    );

    // And the section is exactly the layout that file gets when selected — the
    // parity a pixel-stable handoff needs.
    drop(win);
    press(&mut app, 'j');
    assert_eq!(app.active_path(), Some("b_big.txt"));
    assert_eq!(
        app.diff_row_count(),
        2 + 1 + 10_000 + 2,
        "the standalone layout matches the section row for row"
    );
}

#[test]
fn a_neighbours_comment_box_is_in_its_section() {
    let repo = init_repo_with_diverged_branches();
    let mut app = review_app(&repo);
    let second = app.review_files()[1].path.clone();
    seed_store(
        repo.path(),
        "feature",
        "main",
        vec![comment_on(1, &second, 1, "a note on the neighbour")],
    );
    // Reload picks the seeded store up without moving the range.
    app.reload();
    dump_frame(&app, W, H).unwrap();

    let win = window(&mut app);
    let segment = win
        .segments
        .iter()
        .find(|segment| segment.path == second)
        .expect("the neighbour is in the window");
    let section = segment.section.as_ref().expect("a strip owns its section");
    let boxes = section
        .rows
        .iter()
        .filter(|row| matches!(row.content, RowContent::Box(_)))
        .count();
    assert!(boxes >= 3, "the neighbour's comment box is in its section");
    let rows = section.rows.len();

    drop(win);
    press(&mut app, 'j');
    assert_eq!(app.active_path(), Some(second.as_str()));
    assert_eq!(
        app.diff_row_count(),
        rows,
        "the section has row-count parity with the file's standalone layout"
    );
}

#[test]
fn a_section_never_carries_the_in_place_editor() {
    let repo = three_modified_files();
    let mut app = app_for(&repo, false);
    window(&mut app);

    press(&mut app, 'l');
    press(&mut app, 'j');
    press(&mut app, 'j');
    press(&mut app, 'c');
    assert!(app.editor_open());
    // Editing freezes the fill (crossing is disabled while editing), and the
    // sections already prepared hold no editor rows.
    let area = app.diff_area();
    app.ensure_diff_window(area.width, area.height);
    let win = app.diff_window(area.width, area.height);
    for segment in win.segments.iter().filter(|s| !s.is_anchor()) {
        let section = segment.section.as_ref().unwrap();
        assert!(
            !section
                .rows
                .iter()
                .any(|row| matches!(row.content, RowContent::Editor(_))),
            "{} carries no editor row",
            segment.path
        );
    }
}
