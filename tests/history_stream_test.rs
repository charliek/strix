//! The History view's cross-file stream (plan 009): the selected commit's files
//! are the stream, the commit (`●`) details row sits outside it, section
//! identities carry the commit's OID, the wheel crosses file boundaries and the
//! committed-changes list follows the anchor.
//!
//! Crossing never changes the selected commit: the last file's last row and the
//! first file's first row are hard edges, and only picking `●` in the list gets
//! back to the commit details.

mod common;

use common::{
    cell_bg, cell_symbol, click, commit_at, config, ctrl, dump, git, git_env, history_select_row,
    init_repo_with_multi_file_commit, key, mouse, prepare_window, press, render_buffer,
    rendered_app, row_of, window_of, write,
};
use strix::app::{App, FileId, HeaderPart, HistoryFocus, RowContent, RowTarget, ViewMode};
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
