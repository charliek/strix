//! The History view's cross-file stream (plan 009): the selected commit's files
//! are the stream, the commit (`●`) details row sits outside it, section
//! identities carry the commit's OID, the wheel crosses file boundaries and the
//! committed-changes list follows the anchor.
//!
//! Crossing never changes the selected commit: the last file's last row and the
//! first file's first row are hard edges, and only picking `●` in the list gets
//! back to the commit details.

mod common;

use std::time::Instant;

use common::{
    cell_bg, cell_symbol, click, commit_at, config, ctrl, diff_row_has_bg, dump, esc, git, git_env,
    history_select_row, init_repo_with_multi_file_commit, key, mouse, ms, prepare_window, press,
    render_buffer, rendered_app, row_of, seed_store, strip_header_row, strip_row, tab, window_of,
    write,
};
use strix::app::{
    App, CursorAddress, FileId, HeaderPart, HistoryFocus, LayoutRow, RowContent, RowTarget,
    ViewMode,
};
use strix::comments::{Comment, Scope, Side, Source};
use strix::crossterm::event::MouseEventKind;
use strix::git::{FileDiff, LineKind};
use tempfile::TempDir;

const W: u16 = 120;
/// Tall enough that the whole five-file stream is prepared at once.
const H: u16 = 30;
/// A 10-row viewport: the five-file stream is three times deeper, so the wheel
/// has boundaries it can actually reach past the end-of-stream clamp.
const SHORT_H: u16 = 14;

/// HEAD's file list, in the order `git diff-tree` reports it.
const HEAD_FILES: [&str; 5] = ["a.txt", "b.txt", "blob.bin", "c.txt", "moved.txt"];

// --- construction ----------------------------------------------------------

/// An `App` on `repo`, already in the History view with one frame rendered, so
/// the graph / list / diff geometry every layout is keyed by exists.
fn history_app(repo: &TempDir, cross_file: bool, h: u16) -> App {
    let mut app = rendered_app(repo, config(cross_file, false), h);
    press(&mut app, 'i');
    let _ = dump(&app, W, h);
    app
}

/// The multi-file fixture with an `a.txt` far taller than any viewport here:
/// HEAD rewrites all 60 of its lines and edits one line of `b.txt`, so the
/// anchor alone fills the pane and its strip stays unprepared.
fn tall_head_file_repo() -> TempDir {
    let repo = init_repo_with_multi_file_commit();
    let path = repo.path();
    let tall: String = (0..60).map(|i| format!("alpha {i}\n")).collect();
    write(path, "a.txt", &tall);
    write(path, "b.txt", "beta one\nbeta again\nbeta three\n");
    git(path, &["add", "."]);
    commit_at(path, "grow a", "2021-01-04T00:00:00");
    repo
}

/// The stream identity of `path` in the commit at walk index `commit`. Takes the
/// app because `gix::ObjectId` — the type `FileId::History` carries — is not
/// nameable from outside the crate.
fn hist(app: &App, commit: usize, path: &str) -> FileId {
    FileId::History {
        commit: app.commits()[commit].id,
        path: path.to_string(),
    }
}

// --- event helpers ---------------------------------------------------------

/// A wheel tick at `(x, y)` — the real event path, so the trailing `sync_active`
/// runs exactly as it does under a user's finger.
fn wheel_at(app: &mut App, x: u16, y: u16, down: bool) {
    let kind = if down {
        MouseEventKind::ScrollDown
    } else {
        MouseEventKind::ScrollUp
    };
    app.on_mouse(mouse(x, y, kind));
}

/// A wheel tick over the diff pane.
fn wheel(app: &mut App, down: bool) {
    let area = app.diff_area();
    wheel_at(app, area.x + 2, area.y + 1, down);
}

/// A wheel tick over the frame row containing `needle` in the left column, which
/// is how the Graph and the committed-changes list are addressed here (neither
/// records a public geometry accessor).
fn wheel_over(app: &mut App, needle: &str, h: u16, down: bool) {
    let frame = dump(app, W, h);
    let y = row_of(&frame, needle) as u16;
    wheel_at(app, 2, y, down);
}

/// Park the stream at `(committed row, offset)` with the window prepared — the
/// state invariant every wheel tick starts from.
fn park(app: &mut App, row: usize, offset: usize, h: u16) {
    history_select_row(app, row, W, h);
    app.diff_scroll.set(offset);
    prepare_window(app);
}

/// The diff pane's body rows as glyphs, top to bottom.
fn body(app: &App, h: u16) -> Vec<String> {
    let buf = render_buffer(app, W, h);
    let area = app.diff_area();
    (area.y..area.y + area.height)
        .map(|y| {
            (area.x..area.x + area.width)
                .map(|x| cell_symbol(&buf, x, y))
                .collect()
        })
        .collect()
}

/// The window's segment identities, in draw order.
fn window_ids(app: &App) -> Vec<Option<FileId>> {
    window_of(app)
        .segments
        .iter()
        .map(|segment| segment.id.clone())
        .collect()
}

/// Whether the frame paints a file-header band anywhere (the chip background is
/// unique to it).
fn paints_a_band(app: &App, h: u16) -> bool {
    let buf = render_buffer(app, W, h);
    (0..h).any(|y| (0..W).any(|x| cell_bg(&buf, x, y) == Some(app.theme.file_header_chip_bg)))
}

// --- A1: the stream and the details row ------------------------------------

#[test]
fn the_stream_is_the_selected_commits_files_and_the_details_row_is_outside_it() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, H);

    // A fresh app rests on the commit `●` row, which has no anchor at all.
    assert!(app.history_shows_details());
    assert_eq!(window_ids(&app), vec![None], "one segment, no identity");
    assert_eq!(app.cached_section_count(), 0, "nothing was prepared for it");
    assert!(!paints_a_band(&app, H), "the details pane draws no band");

    history_select_row(&mut app, 1, W, H);
    prepare_window(&mut app);
    let ids = window_ids(&app);
    assert!(ids.len() > 1, "row 1 anchors a real stream");
    let expected: Vec<Option<FileId>> = HEAD_FILES
        .iter()
        .take(ids.len())
        .map(|path| Some(hist(&app, 0, path)))
        .collect();
    assert_eq!(
        ids, expected,
        "History ids for the commit's files, in order"
    );
    assert!(paints_a_band(&app, H), "a file row draws the header band");

    // The anchor is the stream's first file, so it leads with the band alone and
    // its next row is already code.
    let width = app.diff_area().width;
    {
        let rows = app.diff_layout(width);
        assert!(
            matches!(
                &rows[0].content,
                RowContent::FileHeader(header) if header.part == HeaderPart::Band
            ),
            "row 0 is the band, not {:?}",
            rows[0].target
        );
        assert!(
            matches!(rows[1].target, RowTarget::Code(_)),
            "row 1 is code: {:?}",
            rows[1].target
        );
    }
    // The file below it is led by a rule row (plan 008 §3.5).
    let win = window_of(&app);
    let section = win.segments[1]
        .section
        .as_ref()
        .expect("a strip segment owns its section");
    let parts: Vec<HeaderPart> = section.rows[..2]
        .iter()
        .map(|row| match &row.content {
            RowContent::FileHeader(header) => header.part,
            _ => panic!("expected a header row at {:?}", row.target),
        })
        .collect();
    assert_eq!(parts, vec![HeaderPart::Rule, HeaderPart::Band]);
    drop(win);

    // Back to `●`: the anchor is gone again, and nothing is computed for it.
    let computes = app.diff_compute_count();
    history_select_row(&mut app, 0, W, H);
    assert_eq!(window_ids(&app), vec![None]);
    assert_eq!(
        app.diff_compute_count(),
        computes,
        "the details row computes no diff"
    );
}

// --- A2: the wheel handoff --------------------------------------------------

#[test]
fn a_wheel_crossing_moves_the_anchor_and_the_list_follows() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, SHORT_H);
    history_select_row(&mut app, 1, W, SHORT_H);
    let r_a = app.diff_row_count();

    // Warm every section the crossing can reach, so what follows measures the
    // flip alone rather than the strip's ordinary laziness.
    for _ in 0..20 {
        wheel(&mut app, true);
    }
    park(&mut app, 1, r_a - 1, SHORT_H);

    let computes = app.diff_compute_count();
    wheel(&mut app, true);
    assert_eq!(app.committed_row(), 2, "the list follows the anchor");
    assert_eq!(app.active_diff_path().as_deref(), Some("b.txt"));
    assert_eq!(
        app.diff_scroll.get(),
        r_a - 1 + 3 - r_a,
        "the offset is rebased on the arriving file, not reset"
    );
    assert_eq!(
        app.diff_compute_count(),
        computes,
        "the flip installs the prepared section, recomputing nothing"
    );

    // `(a.txt, R_a)` reached going down and `(b.txt, 0)` reached going up are the
    // same picture; only the anchor differs (plan 006 §3.2c).
    park(&mut app, 1, r_a - 1, SHORT_H);
    app.wheel_scroll_window(1);
    assert_eq!(
        (app.committed_row(), app.diff_scroll.get()),
        (1, r_a),
        "the boundary is a legal resting state for a.txt"
    );
    let down = body(&app, SHORT_H);

    park(&mut app, 2, 1, SHORT_H);
    app.wheel_scroll_window(-1);
    assert_eq!(
        (app.committed_row(), app.diff_scroll.get()),
        (2, 0),
        "hysteresis holds at (b.txt, 0)"
    );
    let up = body(&app, SHORT_H);
    assert_eq!(down, up, "the same rows, glyph for glyph");

    // One more up tick shows a.txt's content, so the anchor flips back.
    app.wheel_scroll_window(-1);
    assert_eq!(
        (app.committed_row(), app.diff_scroll.get()),
        (1, r_a - 1),
        "one row of a.txt is showing"
    );
}

// --- A3: hard edges ---------------------------------------------------------

#[test]
fn the_stream_stops_at_the_commits_first_and_last_rows() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, SHORT_H);
    history_select_row(&mut app, 1, W, SHORT_H);
    let commit = app.selected_commit();

    for _ in 0..40 {
        wheel(&mut app, true);
    }
    let frame = dump(&app, W, SHORT_H);
    assert_eq!(
        app.diff_scroll.get(),
        app.diff_scroll_limit(),
        "pinned at the last row the stream offers"
    );
    assert!(
        frame.contains("keep four"),
        "the last file's last row is on screen:\n{frame}"
    );
    assert_eq!(
        app.selected_commit(),
        commit,
        "crossing never changes the commit"
    );
    // One more tick changes nothing: the stream's end is a hard edge, not a
    // handoff into the next commit.
    let bottom = (app.committed_row(), app.diff_scroll.get());
    wheel(&mut app, true);
    assert_eq!((app.committed_row(), app.diff_scroll.get()), bottom);

    history_select_row(&mut app, 1, W, SHORT_H);
    for _ in 0..10 {
        wheel(&mut app, false);
    }
    assert_eq!(
        app.committed_row(),
        1,
        "up stops at the first file, never at the ● row"
    );
    assert_eq!(app.diff_scroll.get(), 0);
    assert!(!app.history_shows_details());
}

// --- A4: sections are keyed on the commit -----------------------------------

#[test]
fn changing_the_commit_re_scopes_the_stream_to_its_own_files() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, H);
    history_select_row(&mut app, 1, W, H);
    prepare_window(&mut app);
    let head = app.commits()[0].id;
    assert!(window_ids(&app)
        .iter()
        .all(|id| matches!(id, Some(FileId::History { commit, .. }) if *commit == head)));

    // Back to the Graph, one commit older, then its first file.
    press(&mut app, 'h');
    assert_eq!(app.history_focus(), HistoryFocus::Graph);
    app.on_key(key('j'));
    let older = app.commits()[app.selected_commit()].id;
    assert_ne!(older, head);
    history_select_row(&mut app, 1, W, H);
    prepare_window(&mut app);

    assert!(
        window_ids(&app)
            .iter()
            .all(|id| matches!(id, Some(FileId::History { commit, .. }) if *commit == older)),
        "every segment belongs to the newly selected commit"
    );
    let frame = dump(&app, W, H);
    assert!(
        !frame.contains("alpha edited"),
        "no row of HEAD's a.txt survives the re-scope:\n{frame}"
    );
    assert!(frame.contains("alpha two"), "the older content is shown");
}

// --- A5: laziness -----------------------------------------------------------

#[test]
fn a_tall_anchor_computes_no_neighbour_until_the_wheel_reaches_it() {
    let repo = tall_head_file_repo();
    let mut app = history_app(&repo, true, H);
    history_select_row(&mut app, 1, W, H);
    prepare_window(&mut app);

    assert_eq!(app.active_diff_path().as_deref(), Some("a.txt"));
    assert_eq!(
        app.diff_compute_count(),
        1,
        "only the anchor's own diff was read"
    );
    assert_eq!(app.cached_section_count(), 0, "no strip section yet");

    let mut ticks = 0;
    while app.diff_compute_count() == 1 && ticks < 60 {
        wheel(&mut app, true);
        ticks += 1;
    }
    assert_eq!(
        app.diff_compute_count(),
        2,
        "b.txt is computed once, when the viewport reaches it"
    );
    assert_eq!(
        app.active_diff_path().as_deref(),
        Some("a.txt"),
        "and the anchor has not moved yet"
    );
}

// --- A10: the Graph and the list keep their own wheel model ------------------

#[test]
fn the_graph_and_the_list_keep_their_wheel_behaviour() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, H);
    history_select_row(&mut app, 2, W, H);
    assert_eq!(app.selected_commit(), 0);

    // Over the Graph a tick still selects the next commit, resetting to `●`.
    wheel_over(&mut app, "HEAD", H, true);
    assert_eq!(app.history_focus(), HistoryFocus::Graph);
    assert_eq!(app.selected_commit(), 1);
    assert_eq!(app.committed_row(), 0, "a new commit starts at its details");

    // Over the committed-changes list a tick still moves the row.
    wheel_over(&mut app, "A a.txt", H, true);
    assert_eq!(app.history_focus(), HistoryFocus::CommittedChanges);
    assert_eq!(app.committed_row(), 1);
}

// --- A11: cross-file scroll off ---------------------------------------------

#[test]
fn with_cross_file_off_history_clamps_per_file_and_draws_no_band() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, false, SHORT_H);
    history_select_row(&mut app, 1, W, SHORT_H);

    assert!(!paints_a_band(&app, SHORT_H), "no header band with `f` off");
    {
        let rows = app.diff_layout(app.diff_area().width);
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.content, RowContent::FileHeader(_))),
            "and no header rows in the layout"
        );
    }
    assert_eq!(window_ids(&app).len(), 1, "a single anchor segment");

    for _ in 0..10 {
        wheel(&mut app, true);
    }
    assert_eq!(
        app.committed_row(),
        1,
        "the wheel clamps at the file's edge"
    );
    assert_eq!(app.diff_scroll.get(), app.diff_max_scroll());
}

// --- A12: refresh -----------------------------------------------------------

#[test]
fn a_refresh_that_re_finds_the_commit_keeps_its_list_scroll_and_sections() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, SHORT_H);
    history_select_row(&mut app, 2, W, SHORT_H);
    wheel(&mut app, true);

    let before = (
        app.selected_commit(),
        app.committed_row(),
        app.diff_scroll.get(),
        app.diff_compute_count(),
        app.stream_generation(),
    );
    // The list and the prepared sections themselves, not just the counters: a
    // reinstall that happened to produce an identical list, or a silently
    // emptied section cache, would pass on the counters alone (codex review
    // finding).
    let files: Vec<String> = app.history_files().iter().map(|f| f.path.clone()).collect();
    let sections = app.cached_section_count();
    assert!(sections > 0, "the strip prepared at least one section");
    app.reload();
    assert_eq!(
        (
            app.selected_commit(),
            app.committed_row(),
            app.diff_scroll.get(),
            app.diff_compute_count(),
            app.stream_generation(),
        ),
        before,
        "an immutable commit's file list is not reinstalled"
    );
    assert_eq!(
        app.history_files()
            .iter()
            .map(|f| f.path.clone())
            .collect::<Vec<_>>(),
        files,
        "the same file list survives"
    );
    assert_eq!(
        app.cached_section_count(),
        sections,
        "the prepared sections survive"
    );
    // The window still renders that commit's rows after the reload.
    let frame = common::dump(&app, W, SHORT_H);
    assert!(frame.contains("b.txt"), "frame:\n{frame}");

    // When the commit is gone after the re-walk, the list is reinstalled for
    // commit 0 — back to its `●` row, one generation bump.
    git(repo.path(), &["reset", "--hard", "HEAD~1"]);
    let generation = app.stream_generation();
    app.reload();
    assert_eq!(app.selected_commit(), 0);
    assert_eq!(app.committed_row(), 0, "back to the details row");
    assert_eq!(app.stream_generation(), generation + 1);
}

// --- A17: the details pane never inherits a strip's hit map -----------------

#[test]
fn a_click_on_the_details_pane_after_a_strip_only_focuses() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, H);
    history_select_row(&mut app, 1, W, H);
    let strip_y = app.diff_area().y + app.diff_row_count() as u16 + 2;

    // Step back up to `●`: nothing in the hit map's epoch (layout generation,
    // stream generation, view, offset, pane area) moved, so only the details
    // renderer clearing the map keeps the click off `strip_click`.
    app.on_key(key('k'));
    assert!(app.history_shows_details());
    let _ = dump(&app, W, H);

    app.on_mouse(click(app.diff_area().x + 3, strip_y));
    assert_eq!(app.history_focus(), HistoryFocus::Diff);
    assert_eq!(app.cursor_address(), None);
}

// --- A19: the hidden panel --------------------------------------------------

#[test]
fn with_the_panel_hidden_the_row_follows_the_anchor_and_reappears_selected() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, SHORT_H);
    history_select_row(&mut app, 1, W, SHORT_H);

    press(&mut app, 'b');
    let _ = dump(&app, W, SHORT_H);
    assert_eq!(app.history_focus(), HistoryFocus::Diff);
    assert_eq!(app.committed_row(), 1);

    for _ in 0..6 {
        wheel(&mut app, true);
    }
    let row = app.committed_row();
    assert!(row > 1, "the row followed the anchor invisibly");

    press(&mut app, 'b');
    let frame = dump(&app, W, SHORT_H);
    assert_eq!(
        app.committed_row(),
        row,
        "the reveal keeps the followed row"
    );
    assert_eq!(
        app.history_focus(),
        HistoryFocus::Graph,
        "and lands in the Graph, as it always has"
    );
    assert!(frame.contains("Committed Changes"), "frame:\n{frame}");
}

// --- A20: an empty commit ---------------------------------------------------

#[test]
fn an_empty_commit_has_no_stream_at_all() {
    let repo = init_repo_with_multi_file_commit();
    git_env(
        repo.path(),
        &[
            ("GIT_AUTHOR_DATE", "2021-01-05T00:00:00"),
            ("GIT_COMMITTER_DATE", "2021-01-05T00:00:00"),
        ],
        &["commit", "-q", "--allow-empty", "-m", "empty"],
    );
    let mut app = history_app(&repo, true, H);

    assert_eq!(app.stream_file_count(), 0);
    assert!(app.history_shows_details());
    assert_eq!(window_ids(&app), vec![None]);

    history_select_row(&mut app, 0, W, H);
    app.on_key(key('j'));
    assert_eq!(app.committed_row(), 0, "there is no row to step onto");
    assert_eq!(app.diff_compute_count(), 0, "and no diff to compute");

    // Neither the wheel nor the scroll keys have anything to act on.
    wheel(&mut app, true);
    wheel(&mut app, false);
    for k in ['j', 'k', 'g', 'G'] {
        app.on_key(key(k));
    }
    app.on_key(ctrl('d'));
    app.on_key(ctrl('u'));
    assert_eq!(app.committed_row(), 0);
    assert_eq!(app.diff_compute_count(), 0);
}

// --- A21: the root commit ---------------------------------------------------

#[test]
fn the_root_commits_files_stream_against_the_empty_tree() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, H);

    // Graph focus is the default on entry; `G` walks to the root.
    app.on_key(key('G'));
    let root = app.commits().len() - 1;
    assert_eq!(app.selected_commit(), root);

    history_select_row(&mut app, 1, W, H);
    prepare_window(&mut app);
    assert_eq!(
        window_ids(&app),
        vec![
            Some(hist(&app, root, "README.md")),
            Some(hist(&app, root, "keep.txt")),
        ]
    );
    assert!(paints_a_band(&app, H), "the root commit's files get bands");
}

// --- A22: a rename ----------------------------------------------------------

#[test]
fn a_renamed_file_is_keyed_on_its_new_path_and_bands_the_old_one() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, H);
    history_select_row(&mut app, 5, W, H);

    assert_eq!(app.active_file_id(), Some(hist(&app, 0, "moved.txt")));
    let frame = dump(&app, W, H);
    assert!(
        frame.contains("keep.txt → moved.txt"),
        "the band reads old → new:\n{frame}"
    );
    let Some(FileDiff::Text(lines)) = app.active_diff() else {
        panic!("the rename carries a text diff");
    };
    assert!(
        lines
            .iter()
            .any(|line| line.kind == LineKind::Addition && line.text.contains("keep edited")),
        "the section is the one-line edit: {lines:?}"
    );
}

// --- A23: a binary file -----------------------------------------------------

#[test]
fn a_binary_file_is_a_header_only_section_the_stream_crosses() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, SHORT_H);
    history_select_row(&mut app, 3, W, SHORT_H);
    assert_eq!(app.active_diff_path().as_deref(), Some("blob.bin"));
    {
        let rows = app.diff_layout(app.diff_area().width);
        assert_eq!(rows.len(), 2, "rule + band, no content rows");
        assert!(rows
            .iter()
            .all(|row| matches!(row.content, RowContent::FileHeader(_))));
    }

    // Crossing does not stall on it: from b.txt the wheel walks straight past.
    history_select_row(&mut app, 2, W, SHORT_H);
    for _ in 0..12 {
        wheel(&mut app, true);
    }
    assert!(
        app.committed_row() > 3,
        "the stream crossed the binary file, row {}",
        app.committed_row()
    );

    // With `f` off the same row is the plain binary hint it always was.
    let mut app = history_app(&repo, false, SHORT_H);
    history_select_row(&mut app, 3, W, SHORT_H);
    let frame = dump(&app, W, SHORT_H);
    assert!(
        frame.contains("Binary file — no preview"),
        "frame:\n{frame}"
    );
}

// --- A25: the generation matrix ---------------------------------------------

#[test]
fn the_stream_generation_advances_once_per_file_list_installation() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = rendered_app(&repo, config(true, false), H);
    let _ = dump(&app, W, H);

    let generation = app.stream_generation();
    press(&mut app, 'i');
    assert_eq!(
        app.stream_generation(),
        generation + 1,
        "entering History installs the selected commit's list"
    );
    let _ = dump(&app, W, H);

    // `j` in the Graph: a new commit, one bump.
    let generation = app.stream_generation();
    app.on_key(key('j'));
    assert_eq!(app.selected_commit(), 1);
    assert_eq!(app.stream_generation(), generation + 1);

    // `j` at the Graph's last row: the commit does not change, but the list is
    // reinstalled (and the row reset), so it still bumps exactly once — the
    // `load_more_history` it goes through adds none of its own.
    app.on_key(key('G'));
    let generation = app.stream_generation();
    app.on_key(key('j'));
    assert_eq!(app.selected_commit(), app.commits().len() - 1);
    assert_eq!(app.stream_generation(), generation + 1);

    // Clicking the already-selected Graph row reloads it: one bump.
    app.on_key(key('g'));
    assert_eq!(app.selected_commit(), 0);
    let frame = dump(&app, W, H);
    let y = row_of(&frame, "HEAD") as u16;
    let generation = app.stream_generation();
    app.on_mouse(click(2, y));
    assert_eq!(app.selected_commit(), 0);
    assert_eq!(app.stream_generation(), generation + 1);

    // A refresh that re-finds the commit, selecting rows, and scrolling all bump
    // nothing.
    let generation = app.stream_generation();
    app.reload();
    assert_eq!(app.stream_generation(), generation, "same-commit refresh");
    history_select_row(&mut app, 1, W, H);
    assert_eq!(app.stream_generation(), generation, "a list selection");
    for _ in 0..8 {
        wheel(&mut app, true);
    }
    assert_eq!(app.stream_generation(), generation, "scrolling / crossing");

    // Leaving History re-scopes the stream back to the home view: one bump.
    app.on_key(key('i'));
    assert_eq!(app.view, ViewMode::Status);
    assert_eq!(app.stream_generation(), generation + 1);
}

// --- C2: History's diff cursor ----------------------------------------------
//
// `history_pane` gives the History diff pane the same `DiffPaneState` Status and
// Review own, so the whole cursor seam — placement, the walk, divergence, the
// highlight — starts working there by construction. The commit `●` row is the
// one exception: it is a paragraph, not a diff, so its keys stay a plain scroll.

/// Two tall files in HEAD (`a.txt` and `b.txt` rewritten to 60 lines each), so
/// either can lead the window and a half page has room to cross between them.
fn two_tall_files_repo() -> TempDir {
    let repo = init_repo_with_multi_file_commit();
    let path = repo.path();
    let alpha: String = (0..60).map(|i| format!("alpha {i}\n")).collect();
    let beta: String = (0..60).map(|i| format!("beta {i}\n")).collect();
    write(path, "a.txt", &alpha);
    write(path, "b.txt", &beta);
    git(path, &["add", "."]);
    commit_at(path, "grow a and b", "2021-01-04T00:00:00");
    repo
}

/// HEAD edits one line of `a.txt` and rewrites `b.txt` to 60 lines, so a walk
/// that steps off the short first file stays inside the second one long enough
/// for the anchor to follow it across.
fn tall_second_file_repo() -> TempDir {
    let repo = init_repo_with_multi_file_commit();
    let path = repo.path();
    let beta: String = (0..60).map(|i| format!("beta {i}\n")).collect();
    write(path, "a.txt", "alpha one\nalpha again\nalpha three\n");
    write(path, "b.txt", &beta);
    git(path, &["add", "."]);
    commit_at(path, "grow b", "2021-01-04T00:00:00");
    repo
}

/// A HEAD whose commit message is far taller than any viewport here, so the
/// details paragraph has somewhere to scroll.
fn long_message_repo() -> TempDir {
    let repo = init_repo_with_multi_file_commit();
    let path = repo.path();
    write(path, "a.txt", "alpha one\nalpha again\nalpha three\n");
    git(path, &["add", "."]);
    let message: String = (0..40).map(|i| format!("message line {i}\n")).collect();
    commit_at(path, &message, "2021-01-04T00:00:00");
    repo
}

/// Focus the History diff pane from wherever focus is now.
fn focus_diff(app: &mut App, h: u16) {
    for _ in 0..3 {
        if app.diff_focused() {
            break;
        }
        app.on_key(key('l'));
    }
    assert!(app.diff_focused(), "the diff pane never took focus");
    let _ = dump(app, W, h);
}

/// Press `ch` until the committed-changes row reaches `want`, rendering between
/// presses as the event loop does.
fn press_until_row(app: &mut App, ch: char, want: usize, h: u16) {
    for _ in 0..400 {
        app.on_key(key(ch));
        let _ = dump(app, W, h);
        if app.committed_row() == want {
            return;
        }
    }
    panic!("committed row {want} was never reached by `{ch}`");
}

/// Walk the cursor down until it leaves the anchor, returning the file it landed
/// in. Panics rather than looping forever when the walk never diverges.
fn walk_until_divergent(app: &mut App, h: u16) -> FileId {
    for _ in 0..400 {
        app.on_key(key('j'));
        let _ = dump(app, W, h);
        if app.cursor_divergent() {
            return app.cursor_address().expect("a divergent cursor").file;
        }
    }
    panic!("the walk never left the anchor");
}

// --- A6: the keyboard walk --------------------------------------------------

#[test]
fn the_history_diff_pane_walks_the_cursor_across_the_commits_files() {
    let repo = tall_second_file_repo();
    let mut app = history_app(&repo, true, SHORT_H);
    history_select_row(&mut app, 1, W, SHORT_H);
    focus_diff(&mut app, SHORT_H);
    let a_rows = app.diff_row_count();
    let b_id = hist(&app, 0, "b.txt");

    // `G`/`g` are the *current file's* edges, exactly as in Review — not the
    // stream's (plan 009 §3.1).
    app.on_key(key('G'));
    assert_eq!(app.review_cursor(), a_rows - 1, "a.txt's last target");
    assert_eq!(app.committed_row(), 1, "and the anchor has not moved");
    assert!(!app.cursor_divergent());

    // One more step walks off a.txt's end onto b.txt's header: divergent, with
    // the anchor and the list still on a.txt.
    app.on_key(key('j'));
    let _ = dump(&app, W, SHORT_H);
    assert_eq!(
        app.cursor_address(),
        Some(CursorAddress {
            file: b_id.clone(),
            target: RowTarget::FileHeader,
        }),
        "the walk stepped onto the next file's header"
    );
    assert!(app.cursor_divergent());
    assert_eq!(app.committed_row(), 1, "the anchor did not follow yet");
    assert_eq!(app.active_diff_path().as_deref(), Some("a.txt"));

    // Keep walking: the reveal eventually renormalizes past the boundary, and the
    // list follows the anchor.
    press_until_row(&mut app, 'j', 2, SHORT_H);
    assert!(!app.cursor_divergent(), "the flip converged the cursor");
    assert_eq!(app.active_diff_path().as_deref(), Some("b.txt"));
    assert_eq!(
        app.cursor_address().map(|address| address.file),
        Some(b_id),
        "still the file the cursor walked into"
    );

    // `g` is b.txt's own first target now, not the stream's.
    app.on_key(key('g'));
    assert_eq!(app.review_cursor(), 0);
    assert_eq!(app.committed_row(), 2, "g never leaves the current file");

    // And `k` walks back the other way.
    press_until_row(&mut app, 'k', 1, SHORT_H);
    assert_eq!(app.active_diff_path().as_deref(), Some("a.txt"));
    assert!(!app.cursor_divergent());
}

// --- A7: clicks -------------------------------------------------------------

#[test]
fn a_click_in_the_history_diff_pane_places_the_cursor() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, false, H);

    // (a) The `●` details row has no rows to address, so a click only focuses.
    let area = app.diff_area();
    app.on_mouse(click(area.x + 2, area.y + 1));
    assert_eq!(app.history_focus(), HistoryFocus::Diff);
    assert_eq!(app.cursor_address(), None, "and grows no cursor");

    // (b) On a file row the click lands on the row under the pointer, exactly as
    // `review_click` does.
    history_select_row(&mut app, 1, W, H);
    let area = app.diff_area();
    app.on_mouse(click(area.x + 2, area.y + 2));
    assert_eq!(app.history_focus(), HistoryFocus::Diff);
    assert_eq!(
        app.cursor_address().map(|address| address.file),
        Some(hist(&app, 0, "a.txt"))
    );
    assert_eq!(app.review_cursor(), 2, "the third row of the anchor");
}

#[test]
fn a_history_strip_click_places_a_divergent_cursor_and_a_double_click_converges() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, SHORT_H);
    history_select_row(&mut app, 1, W, SHORT_H);
    prepare_window(&mut app);
    let _ = dump(&app, W, SHORT_H);
    let x = app.diff_area().x + 2;

    // A single click on a strip code row is pure placement: the cursor diverges
    // onto b.txt and nothing else moves.
    let code_y = strip_row(&app, "a strip code row", |row| {
        matches!(row.target, RowTarget::Code(_))
    })
    .y;
    let before = (app.committed_row(), app.diff_scroll.get());
    app.on_mouse(click(x, code_y));
    assert_eq!(app.history_focus(), HistoryFocus::Diff);
    assert!(
        app.cursor_divergent(),
        "the click placed a divergent cursor"
    );
    assert_eq!(
        app.cursor_address().map(|address| address.file),
        Some(hist(&app, 0, "b.txt"))
    );
    assert_eq!(
        (app.committed_row(), app.diff_scroll.get()),
        before,
        "and moved the view not at all"
    );

    // A double-click on a strip *code* row authors nothing: History has no
    // comments, so the second click is inert past the placement.
    let t = Instant::now();
    app.on_mouse_at(click(x, code_y), t);
    app.on_mouse_at(click(x, code_y), t + ms(150));
    assert!(!app.editor_open(), "History never opens the editor");

    // A double-click on the strip's file *header* converges: the anchor flips and
    // the committed-changes list follows it.
    let _ = dump(&app, W, SHORT_H);
    let header_y = strip_header_row(&app).y;
    let t = Instant::now();
    app.on_mouse_at(click(x, header_y), t);
    app.on_mouse_at(click(x, header_y), t + ms(150));
    assert_eq!(app.committed_row(), 2, "the list followed the flip");
    assert_eq!(app.active_diff_path().as_deref(), Some("b.txt"));
    assert!(!app.cursor_divergent());
}

// --- A8: the cursor highlight -----------------------------------------------

#[test]
fn the_history_cursor_row_is_painted_only_while_the_diff_is_focused() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, H);
    history_select_row(&mut app, 1, W, H);
    focus_diff(&mut app, H);
    let sel = app.theme.selection_bg;

    // Step off the band onto a code row: the header carries its own styling and
    // never goes through `mark_cursor_row`.
    app.on_key(key('j'));
    let _ = dump(&app, W, H);
    let span = app.cursor_window_span().expect("the cursor is on screen");
    let y = app.diff_area().y + span.start as u16;
    assert!(
        diff_row_has_bg(&render_buffer(&app, W, H), app.diff_area(), y, sel),
        "the cursor row carries the selection background while the diff is focused"
    );

    app.on_key(key('h')); // back to the committed-changes list
    let _ = dump(&app, W, H);
    assert!(
        !diff_row_has_bg(&render_buffer(&app, W, H), app.diff_area(), y, sel),
        "and loses it as soon as a list pane is focused"
    );
}

// --- A9: the half page ------------------------------------------------------

#[test]
fn a_history_half_page_scrolls_with_a_list_focused_and_walks_with_the_diff() {
    let repo = two_tall_files_repo();
    let mut app = history_app(&repo, true, H);
    history_select_row(&mut app, 1, W, H);
    prepare_window(&mut app);
    let half = (app.diff_area().height / 2).max(1) as usize;
    let a_rows = app.diff_row_count();
    assert!(a_rows > 2 * half, "a.txt is taller than a page");

    // The committed-changes list has no cursor: Ctrl-d is a viewport tick.
    app.on_key(ctrl('d'));
    assert_eq!(app.diff_scroll.get(), half);
    assert_eq!(app.committed_row(), 1, "well inside the tall anchor");
    assert_eq!(
        app.cursor_address().map(|address| address.target),
        Some(RowTarget::FileHeader),
        "and moved no cursor"
    );

    // Parked at a.txt's last row, the same tick renormalizes into b.txt.
    app.diff_scroll.set(a_rows - 1);
    prepare_window(&mut app);
    let _ = dump(&app, W, H);
    app.on_key(ctrl('d'));
    assert_eq!(app.committed_row(), 2, "the tick crossed the boundary");
    assert_eq!(app.active_diff_path().as_deref(), Some("b.txt"));

    // With the diff focused it is the cursor's half page instead, and the list
    // stays where it is while the walk is still inside the anchor.
    history_select_row(&mut app, 1, W, H);
    focus_diff(&mut app, H);
    assert_eq!(app.review_cursor(), 0, "back on the band");
    app.on_key(ctrl('d'));
    let _ = dump(&app, W, H);
    assert_eq!(app.review_cursor(), half, "the cursor moved a half page");
    assert_eq!(app.committed_row(), 1, "and the list stayed put");
}

// --- A13: no comments -------------------------------------------------------

#[test]
fn the_history_diff_pane_authors_no_comments() {
    let repo = init_repo_with_multi_file_commit();
    // A worktree comment on the very file the stream anchors on. History's
    // comment set is empty by construction, so none of it may reach the layout.
    seed_store(
        repo.path(),
        "main",
        None,
        vec![Comment {
            scope: Scope::WorkTree,
            id: 1,
            source: Source::Human,
            file: "a.txt".to_string(),
            side: Side::New,
            line: 1,
            text: "a note the history view must not draw".to_string(),
            context: None,
            orphaned: false,
            created_at: 1_700_000_000,
            base: None,
            stale: false,
        }],
    );
    let mut app = history_app(&repo, true, H);
    history_select_row(&mut app, 1, W, H);
    focus_diff(&mut app, H);
    app.on_key(key('j')); // a code row

    press(&mut app, 'c');
    assert!(!app.editor_open(), "`c` is inert in History");

    // A code-row double-click is idempotent with the single click: it places the
    // same cursor and stops.
    let area = app.diff_area();
    let t = Instant::now();
    app.on_mouse_at(click(area.x + 2, area.y + 2), t);
    let placed = app.cursor_address();
    app.on_mouse_at(click(area.x + 2, area.y + 2), t + ms(150));
    assert!(!app.editor_open());
    assert_eq!(
        app.cursor_address(),
        placed,
        "the second click changed nothing"
    );

    // And no comment reaches the layout — in the anchor *or* the strip, as a
    // box or as an orphan block. Seeded first, so this proves History's empty
    // comment set rather than passing for want of any comment at all.
    let width = app.diff_area().width;
    let boxes = |rows: &[LayoutRow]| {
        rows.iter()
            .any(|row| matches!(row.target, RowTarget::Comment(_) | RowTarget::Orphan(_)))
    };
    assert!(!boxes(&app.diff_layout(width)), "the anchor draws none");
    let win = window_of(&app);
    for segment in win.segments.iter().filter_map(|s| s.section.as_ref()) {
        assert!(!boxes(&segment.rows), "no strip section draws one either");
    }
}

// --- A14: leaving and re-entering -------------------------------------------

#[test]
fn leaving_history_keeps_the_home_cursor_and_re_entry_resets_its_own() {
    let repo = init_repo_with_multi_file_commit();
    write(
        repo.path(),
        "a.txt",
        "alpha one\nalpha edited\nalpha three\nalpha four\n",
    );
    let mut app = rendered_app(&repo, config(true, false), H);

    // Park a converged cursor in the status pane.
    press(&mut app, 'l');
    press(&mut app, 'j');
    let _ = dump(&app, W, H);
    let home = app.cursor_address().expect("a status cursor");
    let home_row = app.review_cursor();
    assert!(home_row > 0, "the status cursor moved off the top");

    press(&mut app, 'i');
    let _ = dump(&app, W, H);
    assert_eq!(app.view, ViewMode::History);
    history_select_row(&mut app, 1, W, H);
    focus_diff(&mut app, H);
    app.on_key(key('j'));
    assert!(app.cursor_address().is_some(), "History has its own cursor");

    // Esc home: the status cursor is exactly where it was left.
    app.on_key(esc());
    let _ = dump(&app, W, H);
    assert_eq!(app.view, ViewMode::Status);
    assert_eq!(app.cursor_address(), Some(home));
    assert_eq!(app.review_cursor(), home_row);

    // Re-entry lands on `●` with no cursor at all, and the first file row gives
    // the implicit one: that file's first target.
    press(&mut app, 'i');
    let _ = dump(&app, W, H);
    assert_eq!(app.committed_row(), 0);
    assert_eq!(
        app.cursor_address(),
        None,
        "the details row addresses nothing"
    );
    history_select_row(&mut app, 1, W, H);
    assert_eq!(
        app.cursor_address(),
        Some(CursorAddress {
            file: hist(&app, 0, "a.txt"),
            target: RowTarget::FileHeader,
        }),
        "the implicit cursor is the arriving file's first target"
    );
}

// --- A16: the details row keeps its paragraph scrolling ---------------------

#[test]
fn the_details_row_keeps_its_plain_scroll_keys() {
    let repo = long_message_repo();
    let mut app = history_app(&repo, true, SHORT_H);
    assert!(app.history_shows_details());
    focus_diff(&mut app, SHORT_H);
    let half = (app.diff_area().height / 2).max(1) as usize;
    assert!(
        app.diff_max_scroll() > half,
        "the commit message is taller than a page"
    );

    app.on_key(key('j'));
    assert_eq!(app.diff_scroll.get(), 1, "j scrolls the paragraph by a row");
    app.on_key(key('G'));
    assert_eq!(app.diff_scroll.get(), app.diff_max_scroll());
    app.on_key(key('g'));
    assert_eq!(app.diff_scroll.get(), 0);
    app.on_key(ctrl('d'));
    assert_eq!(app.diff_scroll.get(), half, "and Ctrl-d by a half page");
    assert_eq!(app.cursor_address(), None, "never growing a cursor");

    // Same again with the left column hidden, where Diff is the only focus.
    app.on_key(key('g'));
    press(&mut app, 'b');
    let _ = dump(&app, W, SHORT_H);
    assert_eq!(app.history_focus(), HistoryFocus::Diff);
    app.on_key(key('j'));
    assert_eq!(app.diff_scroll.get(), 1);
    app.on_key(key('G'));
    assert_eq!(app.diff_scroll.get(), app.diff_max_scroll());
    assert_eq!(app.cursor_address(), None);
}

// --- A18: resize ------------------------------------------------------------

#[test]
fn a_resize_re_prepares_the_history_window_and_drops_a_divergent_cursor() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, SHORT_H);
    history_select_row(&mut app, 1, W, SHORT_H);
    focus_diff(&mut app, SHORT_H);
    walk_until_divergent(&mut app, SHORT_H);

    const NARROW_W: u16 = 90;
    app.on_resize(NARROW_W, SHORT_H);
    let _ = dump(&app, NARROW_W, SHORT_H);
    assert!(
        !app.cursor_divergent(),
        "the resize dropped the divergent cursor"
    );
    assert!(
        window_of(&app).segments.len() > 1,
        "and re-prepared the strip at the new width"
    );

    // The same with the left column hidden: the diff pane fills the body and the
    // resize still has geometry to derive.
    press(&mut app, 'b');
    let _ = dump(&app, NARROW_W, SHORT_H);
    app.on_resize(W, SHORT_H);
    let frame = dump(&app, W, SHORT_H);
    assert!(frame.contains("a.txt"), "frame:\n{frame}");
}

// --- A19 (cursor): the hidden panel -----------------------------------------

#[test]
fn with_the_panel_hidden_a_strip_double_click_converges_and_the_reveal_drops_divergence() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, SHORT_H);
    history_select_row(&mut app, 1, W, SHORT_H);
    press(&mut app, 'b'); // hide the left column; Diff is the only focus left
    let _ = dump(&app, W, SHORT_H); // the pane grew: re-record its geometry first
    prepare_window(&mut app);
    let _ = dump(&app, W, SHORT_H);

    let x = app.diff_area().x + 2;
    let header_y = strip_header_row(&app).y;
    let t = Instant::now();
    app.on_mouse_at(click(x, header_y), t);
    app.on_mouse_at(click(x, header_y), t + ms(150));
    assert_eq!(
        app.committed_row(),
        2,
        "the invisible list followed the flip"
    );
    assert_eq!(app.active_diff_path().as_deref(), Some("b.txt"));

    // Diverge again, then reveal the panel: the reveal lands in the Graph, so the
    // sweep drops the address that only the focused diff could hold.
    let _ = dump(&app, W, SHORT_H);
    walk_until_divergent(&mut app, SHORT_H);
    let row = app.committed_row();
    press(&mut app, 'b');
    let frame = dump(&app, W, SHORT_H);
    assert_eq!(app.history_focus(), HistoryFocus::Graph);
    assert_eq!(app.committed_row(), row, "the reveal keeps the row");
    assert!(!app.cursor_divergent(), "and drops the divergent cursor");
    assert!(frame.contains("Committed Changes"), "frame:\n{frame}");
}

// --- A24: divergence lifetime -----------------------------------------------

#[test]
fn a_divergent_history_cursor_survives_a_refresh_and_dies_on_focus_loss() {
    let repo = init_repo_with_multi_file_commit();
    let mut app = history_app(&repo, true, H);
    history_select_row(&mut app, 1, W, H);
    focus_diff(&mut app, H);
    let file = walk_until_divergent(&mut app, H);

    // A watcher tick that re-finds the same commit keeps its list, so the address
    // still resolves and stays put.
    app.reload();
    let _ = dump(&app, W, H);
    assert!(app.cursor_divergent(), "a same-commit refresh keeps it");
    assert_eq!(app.cursor_address().map(|a| a.file), Some(file));

    // Tab out of the diff pane: the sweep drops it.
    app.on_key(tab());
    let _ = dump(&app, W, H);
    assert!(!app.diff_focused());
    assert!(!app.cursor_divergent(), "leaving the diff pane drops it");

    // And a Graph click, which reloads the commit's list outright.
    focus_diff(&mut app, H);
    walk_until_divergent(&mut app, H);
    let frame = dump(&app, W, H);
    let y = row_of(&frame, "HEAD") as u16;
    app.on_mouse(click(2, y));
    let _ = dump(&app, W, H);
    assert!(!app.cursor_divergent());
    assert_eq!(app.cursor_address(), None, "back on the `●` row");
}
