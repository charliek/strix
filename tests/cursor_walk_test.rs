//! The cursor address (plan 007 §3.3a/b/i/j): the diff cursor names a *file*
//! plus one of that file's own [`RowTarget`]s, so it can rest on a strip row —
//! a file below the anchor in the prepared window — without its target being
//! reinterpreted against the anchor's layout.
//!
//! A cursor whose file is the anchor is *converged* (everything the app does
//! today produces one, and these tests pin that behaviour as unchanged); one
//! that names another file is *divergent* and lives under the invariant of
//! §3.3(b): the diff pane focused, the file in the prepared window, the target
//! still resolving — or the cursor drops back to `None`, both fields at once.
//!
//! The address seam is driven two ways here: directly through
//! `App::place_cursor` (the validated setter), and — from the `the walk` section
//! down — by the keyboard walk of §3.3(f), which moves on the flattened target
//! stream from wherever the cursor is and pulls the viewport after it through
//! §3.3(h)'s window-aware reveal.

mod common;

use std::ops::Range;
use std::time::Instant;

use common::{
    app_for, click, commit_file, config, ctrl, diff_row_has_bg, dump, git, head_oid, init_repo,
    pane_title, prepare_window, press, render_buffer, seed_store, staged, strix_dir, tab, unstaged,
    window_of, write,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use strix::app::{App, CursorAddress, FileId, Modal, RowTarget};
use strix::comments::{self, Comment, Scope, Side, Source};
use strix::crossterm::event::MouseEventKind;
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 24;

// --- fixtures ---------------------------------------------------------------

/// Two short untracked files: at width 120 both fit in one viewport, so `b.txt`
/// is a strip below the `a.txt` anchor from the first prepared window on.
///
/// Rows per file, cross-file on: the file header, the `@@` hunk row, then one row
/// per added line. The header is one row (the band) for `a.txt`, the stream's
/// first file, and two (rule + band) for `b.txt` — so `a.txt` is 4 rows and
/// `b.txt` is 6 (plan 008 §3.5).
fn two_short_files() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "a.txt", "a one\na two\n");
    write(repo.path(), "b.txt", "b one\nb two\nb three\n");
    repo
}

/// Two 40-line files — 42 rows for `a.txt` and 43 for `b.txt`, whose header
/// carries a rule row — so the strip only appears once the viewport overruns the
/// anchor (the state divergence actually requires) and the stream is deep enough
/// below it for a wheel flip to renormalize.
fn two_tall_files() -> TempDir {
    let repo = init_repo();
    let tall: String = (0..40).map(|i| format!("alpha {i}\n")).collect();
    write(repo.path(), "a.txt", &tall);
    let other: String = (0..40).map(|i| format!("beta {i}\n")).collect();
    write(repo.path(), "b.txt", &other);
    repo
}

/// One path listed **twice**: staged, then modified again in the working tree.
/// The two rows share a path and differ only in [`FileId`], which is exactly
/// what an address has to tell apart.
fn dup_path_repo() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "dup.txt", "one\ntwo\n");
    git(repo.path(), &["add", "dup.txt"]);
    write(repo.path(), "dup.txt", "one\ntwo\nthree\n");
    repo
}

// --- helpers ----------------------------------------------------------------

fn address(file: FileId, target: RowTarget) -> CursorAddress {
    CursorAddress { file, target }
}

/// A status app with cross-file scroll on, one frame rendered and its window
/// prepared, and the **diff pane focused** (`l`) — the cursor highlight and
/// divergence both require diff focus.
fn diff_focused_app(repo: &TempDir) -> App {
    let mut app = app_for(repo, config(true, false));
    dump(&app, W, H);
    prepare_window(&mut app);
    press(&mut app, 'l');
    app
}

fn wheel(app: &mut App, kind: MouseEventKind) {
    let diff = app.diff_area();
    app.on_mouse(common::mouse(diff.x + 2, diff.y + 2, kind));
}

fn wheel_down(app: &mut App) {
    wheel(app, MouseEventKind::ScrollDown);
}

fn wheel_up(app: &mut App) {
    wheel(app, MouseEventKind::ScrollUp);
}

/// Wheel down until the window shows a strip below the anchor, i.e. until the
/// viewport overruns the anchor's own rows. Panics rather than looping forever.
fn wheel_to_boundary(app: &mut App) {
    for _ in 0..100 {
        if window_of(app).segments.len() > 1 {
            return;
        }
        wheel_down(app);
    }
    panic!("the strip never came into view");
}

/// Wheel far enough down that the anchor renormalizes onto the next file.
fn wheel_past_the_boundary(app: &mut App) {
    for _ in 0..40 {
        wheel_down(app);
    }
    assert_eq!(common::selected_path(app), "b.txt", "the anchor flipped");
}

/// Park `app` with a divergent cursor on `b.txt`'s second code row, the state
/// every §3.3(b) trigger is measured against.
fn diverged_on_b(repo: &TempDir) -> App {
    let mut app = diff_focused_app(repo);
    wheel_to_boundary(&mut app);
    let placed = app.place_cursor(address(unstaged("b.txt"), RowTarget::Code(2)));
    assert!(
        placed,
        "b.txt is in the prepared window, so the cursor lands"
    );
    assert!(
        app.cursor_divergent(),
        "the cursor names a file below the anchor"
    );
    app
}

/// The text of buffer row `y` inside the diff pane.
fn diff_row_text(buf: &Buffer, area: Rect, y: u16) -> String {
    (area.x..area.x + area.width)
        .map(|x| {
            buf.cell((x, y))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// The screen row the cursor's window span starts on.
fn cursor_screen_row(app: &App) -> u16 {
    let span: Range<usize> = app
        .cursor_window_span()
        .expect("the cursor is in the window");
    app.diff_area().y + span.start as u16
}

/// A worktree comment on `a.txt`'s first added line, anchored by context so the
/// sync keeps it.
fn note_on_a(base: &str) -> Comment {
    Comment {
        scope: Scope::WorkTree,
        id: 1,
        source: Source::Human,
        file: "a.txt".to_string(),
        side: Side::New,
        line: 1,
        text: "seeded".to_string(),
        context: Some("a one".to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
        base: Some(base.to_string()),
        stale: false,
    }
}

// --- the divergent cursor renders ------------------------------------------

#[test]
fn a_divergent_cursor_highlights_its_own_strip_row() {
    let repo = two_short_files();
    let mut app = diff_focused_app(&repo);

    assert!(app.place_cursor(address(unstaged("b.txt"), RowTarget::Code(2))));

    // Resolution: b.txt is not the stream's first file, so its own rows are
    // [rule, band, hunk, Code(1), Code(2), Code(3)] and `Code(2)` is its row 4;
    // the anchor draws 4 rows above it.
    assert_eq!(
        app.cursor_highlight_span(),
        Some((unstaged("b.txt"), 4..5)),
        "the highlight is (file, per-file span)"
    );
    assert_eq!(app.cursor_window_span(), Some(8..9));

    let area = app.diff_area();
    let buf = render_buffer(&app, W, H);
    let sel = app.theme.selection_bg;
    let row = cursor_screen_row(&app);
    assert!(
        diff_row_has_bg(&buf, area, row, sel),
        "the strip row under the cursor is highlighted:\n{}",
        dump(&app, W, H)
    );
    assert!(
        diff_row_text(&buf, area, row).contains("b two"),
        "and it is the row the address names, not a neighbour"
    );
    // The anchor keeps no highlight of its own: the cursor is elsewhere.
    for y in area.y..area.y + 4 {
        assert!(
            !diff_row_has_bg(&buf, area, y, sel),
            "anchor row {y} must not be highlighted while the cursor is divergent"
        );
    }
    // The anchor-domain observables stay honest about having no cursor.
    assert_eq!(app.review_cursor_highlight(), None);
}

#[test]
fn an_anchor_cursor_highlights_exactly_as_before() {
    let repo = two_short_files();
    let mut app = diff_focused_app(&repo);
    press(&mut app, 'j'); // header → hunk row

    assert!(!app.cursor_divergent());
    assert_eq!(app.review_cursor_highlight(), Some(1..2));
    assert_eq!(
        app.cursor_highlight_span(),
        Some((unstaged("a.txt"), 1..2)),
        "the same span, qualified by the anchor's identity"
    );

    let area = app.diff_area();
    let buf = render_buffer(&app, W, H);
    assert!(diff_row_has_bg(
        &buf,
        area,
        area.y + 1,
        app.theme.selection_bg
    ));
}

// --- the corollaries --------------------------------------------------------

#[test]
fn divergence_implies_the_anchor_sits_at_its_hard_edge() {
    let repo = two_tall_files();
    let app = diverged_on_b(&repo);

    // Strip rows exist only where the window overruns the anchor, so a divergent
    // cursor implies the anchor scroll is pinned at its own maximum — which is
    // why `diff_max_scroll` and friends keep their anchor-domain meaning
    // unchanged through B1 (plan 007 §3.3b, first corollary).
    assert!(
        app.diff_scroll.get() >= app.diff_max_scroll(),
        "offset {} vs max {}",
        app.diff_scroll.get(),
        app.diff_max_scroll()
    );
    assert_eq!(
        common::selected_path(&app),
        "a.txt",
        "the anchor did not move"
    );
}

#[test]
fn a_generation_bump_that_re_prepares_the_file_keeps_the_cursor() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(repo.path(), "main", None, vec![note_on_a(&base)]);
    let mut app = diff_focused_app(&repo);
    let placed = address(unstaged("b.txt"), RowTarget::Code(2));
    assert!(app.place_cursor(placed.clone()));
    let generation = app.stream_generation();

    // A comment mutation on the *anchor* file: it bumps `stream_generation`
    // (retiring every cached section) and the same event's `ensure` re-prepares
    // the window — so the address is re-validated, not discarded (plan 007
    // §3.3b, second corollary).
    let close = app.comment_close_rect(1).expect("the note's [x] rect");
    app.on_mouse_at(click(close.x, close.y), Instant::now());

    assert_eq!(app.stream_generation() - generation, 1, "one bump");
    assert!(
        app.status_comment_count("a.txt") == 0,
        "the note was deleted"
    );
    assert!(app.cursor_divergent(), "the cursor survived the bump");
    assert_eq!(app.cursor_address(), Some(placed));
    assert!(
        app.cursor_window_span().is_some(),
        "and it still has a screen row"
    );
}

#[test]
fn a_generation_bump_that_drops_the_target_clears_the_cursor() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(repo.path(), "main", None, vec![note_on_a(&base)]);
    let mut app = diff_focused_app(&repo);
    assert!(app.place_cursor(address(unstaged("b.txt"), RowTarget::Code(3))));

    // b.txt loses the line the cursor names; the next bump rebuilds its section
    // from the new content, where `Code(3)` no longer exists.
    write(repo.path(), "b.txt", "b one\n");
    let close = app.comment_close_rect(1).expect("the note's [x] rect");
    app.on_mouse_at(click(close.x, close.y), Instant::now());

    assert!(
        !app.cursor_divergent(),
        "a target that no longer resolves is never left dangling"
    );
    assert_eq!(
        app.cursor_address().map(|a| a.file),
        Some(unstaged("a.txt")),
        "it reset to the anchor's top, not to a foreign target"
    );
    assert_eq!(app.review_cursor(), 0);
}

// --- the §3.3(b) trigger sweep ---------------------------------------------

/// A named event that must end divergence.
type Trigger = (&'static str, fn(&mut App));

#[test]
fn every_normalization_trigger_clears_divergence() {
    // One case per trigger in plan 007 §3.3(b). Each starts from the same parked
    // divergent state and asserts only that the cursor came home — the trigger's
    // own behaviour is pinned by its own suite. Refresh / reload / relist are
    // deliberately absent: there the sweep decides per file (plan 003 §3.3), and
    // the keep/drop matrix below is what pins them.
    let triggers: &[Trigger] = &[
        ("resize", |app| app.on_resize(W + 10, H)),
        ("wrap toggle (w)", |app| press(app, 'w')),
        ("line-number toggle (n)", |app| press(app, 'n')),
        ("diff-mode toggle (d)", |app| press(app, 'd')),
        ("cross-file toggle (f)", |app| press(app, 'f')),
        ("view change (i, there and back)", |app| {
            press(app, 'i');
            press(app, 'i');
        }),
        ("gg", |app| press(app, 'g')),
        ("G", |app| press(app, 'G')),
        ("list click", |app| {
            let list = app.staging_area();
            app.on_mouse(click(list.x + 2, list.y + 1));
        }),
        ("focus change (tab)", |app| app.on_key(tab())),
        ("wheel flip", wheel_past_the_boundary),
    ];

    for (name, trigger) in triggers {
        let repo = two_tall_files();
        let mut app = diverged_on_b(&repo);

        trigger(&mut app);

        assert!(
            !app.cursor_divergent(),
            "{name} must clear the divergent cursor"
        );
    }
}

#[test]
fn a_reload_on_an_unmoved_range_keeps_the_cursor() {
    let repo = common::init_repo_with_diverged_branches();
    let mut app = App::for_review(repo.path().to_path_buf(), &config(true, false), "main").unwrap();
    dump(&app, W, H);
    prepare_window(&mut app);
    press(&mut app, 'l'); // focus the review diff
    let file = FileId::Review {
        path: "feature2.txt".to_string(),
    };
    let placed = address(file, RowTarget::FileHeader);
    assert!(
        app.place_cursor(placed.clone()),
        "the second review file is in the prepared window"
    );

    app.reload();

    assert_eq!(
        app.cursor_address(),
        Some(placed),
        "the range never moved, so the sweep has nothing to drop the cursor for"
    );
    assert!(app.cursor_divergent());
}

// --- the refresh keep/drop matrix (plan 003 §3.3) ---------------------------
//
// A refresh is not on the sweep's trigger list above: `normalize_cursor` compares
// the rebuilt section against what the address last resolved against, so an
// agent's save only costs the reader the cursor when it touched the file the
// cursor is in. History already behaved this way; these pin Status and Review.

/// A short anchor, a tall strip file for the cursor to park in, and a third file
/// the window never reaches — so a change to `c.txt` is provably off-window.
fn short_anchor_tall_strip() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "a.txt", "a one\na two\n");
    write(repo.path(), "b.txt", &beta(40, None));
    write(repo.path(), "c.txt", "c one\n");
    repo
}

/// `count` `beta N` lines, with line `edit` (if any) rewritten so the file's diff
/// differs without changing its row count.
fn beta(count: usize, edit: Option<usize>) -> String {
    (0..count)
        .map(|i| {
            if Some(i) == edit {
                format!("beta {i} edited\n")
            } else {
                format!("beta {i}\n")
            }
        })
        .collect()
}

fn review_file(path: &str) -> FileId {
    FileId::Review {
        path: path.to_string(),
    }
}

/// Whether the prepared window draws any of `file`'s rows — the precondition a
/// divergent address lives under, read from the outside.
fn window_holds(app: &App, file: &FileId) -> bool {
    window_of(app)
        .segments
        .iter()
        .any(|segment| segment.id.as_ref() == Some(file))
}

/// A review of `main` over `repo`, diff-focused, with the cursor parked on
/// `b.txt`'s second code row.
fn review_diverged_on_b(repo: &TempDir) -> App {
    let mut app = App::for_review(repo.path().to_path_buf(), &config(true, false), "main").unwrap();
    dump(&app, W, H);
    prepare_window(&mut app);
    press(&mut app, 'l');
    assert!(
        app.place_cursor(address(review_file("b.txt"), RowTarget::Code(2))),
        "the second review file is in the prepared window"
    );
    assert!(app.cursor_divergent());
    app
}

/// A review range of `a.txt`, `b.txt` (content given by `b`), `c.txt`, each its
/// own commit on `feature`.
fn review_repo_with_b(b: &str) -> TempDir {
    let repo = init_repo();
    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    commit_file(repo.path(), "a.txt", "a one\na two\n", "add a");
    commit_file(repo.path(), "b.txt", b, "add b");
    commit_file(repo.path(), "c.txt", "c one\n", "add c");
    repo
}

/// A review range whose files are a short anchor, a tall second file, and a third
/// the window never reaches.
fn tall_review_repo() -> TempDir {
    review_repo_with_b(&beta(40, None))
}

/// The same range with all three files short, so the whole stream fits one
/// viewport and `c.txt` has a strip file above it.
fn short_review_repo() -> TempDir {
    review_repo_with_b("b one\n")
}

#[test]
fn a_status_reload_that_changes_nothing_keeps_the_divergent_cursor() {
    let repo = two_tall_files();
    let mut app = diverged_on_b(&repo);
    let before = app.cursor_address();

    app.reload();

    assert_eq!(
        app.cursor_address(),
        before,
        "nothing on disk moved, so the rebuilt section matches the outgoing one"
    );
    assert!(app.cursor_divergent());
}

#[test]
fn a_status_reload_after_the_cursors_file_changed_drops_it() {
    let repo = two_tall_files();
    let mut app = diverged_on_b(&repo);

    write(repo.path(), "b.txt", &beta(40, Some(3)));
    app.reload();

    assert!(
        !app.cursor_divergent(),
        "a rebuilt `Code(2)` names a different line, so the address cannot hold"
    );
}

#[test]
fn a_status_reload_after_the_cursors_file_was_staged_drops_it() {
    let repo = two_tall_files();
    let mut app = diverged_on_b(&repo);

    git(repo.path(), &["add", "b.txt"]);
    app.reload();

    assert!(
        !app.cursor_divergent(),
        "the same path in the index is a different `FileId`, so nothing resolves"
    );
}

#[test]
fn a_status_reload_after_an_off_window_file_changed_keeps_it() {
    let repo = short_anchor_tall_strip();
    let mut app = diverged_on_b(&repo);
    let before = app.cursor_address();
    assert!(
        !window_holds(&app, &unstaged("c.txt")),
        "the tall strip fills the viewport, so c.txt is off-window"
    );

    write(repo.path(), "c.txt", "c one\nc two\n");
    app.reload();

    assert!(
        window_holds(&app, &unstaged("b.txt")),
        "the cursor's file is still drawn — survival can't be read from an \
         address that simply left the window"
    );
    assert_eq!(
        app.cursor_address(),
        before,
        "an unrelated save costs nothing"
    );
}

#[test]
fn a_status_reload_that_grows_a_strip_file_above_the_cursor_drops_it() {
    let repo = three_short_files();
    let mut app = diff_focused_app(&repo);
    assert!(
        app.place_cursor(address(unstaged("c.txt"), RowTarget::Code(1))),
        "the whole stream fits one viewport"
    );

    // b.txt sits between the anchor and the cursor's file, so growing it pushes
    // c.txt past the viewport. Plan 003 §4: that eviction is accepted — the row
    // the cursor names is genuinely no longer on screen.
    write(repo.path(), "b.txt", &beta(40, None));
    app.reload();

    assert!(!window_holds(&app, &unstaged("c.txt")));
    assert!(!app.cursor_divergent());
}

#[test]
fn a_review_reload_that_changes_nothing_keeps_the_divergent_cursor() {
    let repo = tall_review_repo();
    let mut app = review_diverged_on_b(&repo);
    let before = app.cursor_address();

    // The common watcher event during an agent run: a worktree save, which can't
    // move a committed range.
    write(repo.path(), "scratch.txt", "work in progress\n");
    app.reload();

    assert_eq!(app.cursor_address(), before);
    assert!(app.cursor_divergent());
}

#[test]
fn a_review_reload_after_the_cursors_file_changed_drops_it() {
    let repo = tall_review_repo();
    let mut app = review_diverged_on_b(&repo);

    commit_file(repo.path(), "b.txt", &beta(40, Some(3)), "edit b");
    app.reload();

    assert!(
        !app.cursor_divergent(),
        "the range moved and rebuilt b.txt's section from new content"
    );
}

#[test]
fn a_review_reload_after_an_off_window_file_changed_keeps_it() {
    let repo = tall_review_repo();
    let mut app = review_diverged_on_b(&repo);
    let before = app.cursor_address();
    assert!(
        !window_holds(&app, &review_file("c.txt")),
        "the tall strip fills the viewport, so c.txt is off-window"
    );

    commit_file(repo.path(), "c.txt", "c one\nc two\n", "edit c");
    app.reload();

    assert!(
        window_holds(&app, &review_file("b.txt")),
        "the cursor's file is still drawn"
    );
    assert_eq!(
        app.cursor_address(),
        before,
        "a relist that left b.txt's diff alone keeps the cursor in it"
    );
}

#[test]
fn a_review_reload_that_grows_a_strip_file_above_the_cursor_drops_it() {
    let repo = short_review_repo();
    let mut app = App::for_review(repo.path().to_path_buf(), &config(true, false), "main").unwrap();
    dump(&app, W, H);
    prepare_window(&mut app);
    press(&mut app, 'l');
    assert!(
        app.place_cursor(address(review_file("c.txt"), RowTarget::Code(1))),
        "the whole range fits one viewport"
    );

    commit_file(repo.path(), "b.txt", &beta(40, None), "grow b");
    app.reload();

    assert!(!window_holds(&app, &review_file("c.txt")));
    assert!(!app.cursor_divergent());
}

#[test]
fn a_wheel_flip_resets_the_cursor_rather_than_inheriting_the_address() {
    let repo = two_tall_files();
    let mut app = diverged_on_b(&repo);

    wheel_past_the_boundary(&mut app);

    // The flip lands on the very file the cursor was addressing, so "not
    // divergent" alone would be satisfied by simply keeping the address. 006's
    // contract is stronger: a wheel flip carries no cursor at all.
    assert!(!app.cursor_divergent());
    assert_eq!(
        app.review_cursor(),
        0,
        "the arriving anchor starts at its own first row"
    );
}

#[test]
fn scrolling_the_file_out_of_the_window_clears_divergence() {
    let repo = two_tall_files();
    let mut app = diverged_on_b(&repo);

    // Wheel back up until the anchor's own rows fill the viewport again: the
    // cursor's file is no longer in the window, so the address cannot hold.
    for _ in 0..10 {
        wheel_up(&mut app);
        if window_of(&app).segments.len() == 1 {
            break;
        }
    }

    assert_eq!(window_of(&app).segments.len(), 1, "the strip is gone");
    assert!(
        !app.cursor_divergent(),
        "movement that evicts the file from the window ends divergence"
    );
    assert_eq!(common::selected_path(&app), "a.txt", "no flip happened");
}

// --- exact identity, no staged/unstaged fallback ----------------------------

#[test]
fn a_dup_path_address_resolves_to_its_own_section() {
    let repo = dup_path_repo();
    let mut app = diff_focused_app(&repo);
    assert_eq!(common::selected_path(&app), "dup.txt");

    // The same path is listed twice; the staged row is the anchor, the working-
    // tree row is the strip below it. Only the `FileId` tells them apart — and
    // both rows draw the *same* net HEAD→worktree diff, so a path-keyed lookup
    // would happily resolve the address against the anchor's copy.
    let placed = address(unstaged("dup.txt"), RowTarget::Code(1));
    assert!(app.place_cursor(placed.clone()));
    assert_eq!(
        app.cursor_highlight_span().map(|(file, _)| file),
        Some(unstaged("dup.txt"))
    );

    let anchor_rows = window_of(&app).segments[0].rows();
    let span = app.cursor_window_span().expect("a window row");
    assert!(
        span.start >= anchor_rows,
        "the highlight is on the strip copy (window row {}), not the anchor's \
         identically-pathed row",
        span.start
    );

    let area = app.diff_area();
    let buf = render_buffer(&app, W, H);
    let sel = app.theme.selection_bg;
    assert!(diff_row_has_bg(&buf, area, cursor_screen_row(&app), sel));
    for y in area.y..area.y + anchor_rows as u16 {
        assert!(
            !diff_row_has_bg(&buf, area, y, sel),
            "the anchor's copy of dup.txt must stay unhighlighted"
        );
    }
}

#[test]
fn the_sibling_section_above_the_anchor_is_not_addressable() {
    let repo = dup_path_repo();
    let mut app = app_for(&repo, config(true, false));
    dump(&app, W, H);
    press(&mut app, 'j'); // list-focused: anchor the working-tree row instead
    press(&mut app, 'l'); // then focus the diff
    assert_eq!(common::selected_path(&app), "dup.txt");
    assert_eq!(
        window_of(&app).segments.len(),
        1,
        "the working-tree row is the last file; nothing follows it"
    );

    // The staged row is *above* the anchor, so it is not in the window — and
    // with an exact lookup there is no fallback that could resolve it to the
    // anchor's own row.
    assert!(!app.place_cursor(address(staged("dup.txt"), RowTarget::Code(1))));
    assert!(!app.cursor_divergent());
}

#[test]
fn a_list_focused_place_cursor_is_rejected() {
    let repo = two_short_files();
    let mut app = app_for(&repo, config(true, false));
    dump(&app, W, H);
    prepare_window(&mut app);
    assert!(!app.diff_focused(), "the file list has focus at startup");

    // Divergence requires the diff pane to be focused — leaving it clears the
    // cursor (§3.3b), so placing one from a list-focused state would install a
    // state the very next sweep undoes.
    assert!(!app.place_cursor(address(unstaged("b.txt"), RowTarget::Code(2))));
    assert!(!app.cursor_divergent());

    press(&mut app, 'l');
    assert!(
        app.place_cursor(address(unstaged("b.txt"), RowTarget::Code(2))),
        "the same address lands once the diff is focused"
    );
}

#[test]
fn an_address_for_a_file_outside_the_window_is_rejected() {
    let repo = two_short_files();
    let mut app = diff_focused_app(&repo);

    assert!(
        !app.place_cursor(address(unstaged("nope.txt"), RowTarget::FileHeader)),
        "a file that isn't in the stream at all"
    );
    assert!(
        !app.place_cursor(address(staged("b.txt"), RowTarget::FileHeader)),
        "the right path in the wrong section is a different file"
    );
    assert!(
        !app.place_cursor(address(unstaged("b.txt"), RowTarget::Code(99))),
        "a target that isn't in the file's rows"
    );
    assert!(!app.cursor_divergent());
}

// --- cache pinning ----------------------------------------------------------

#[test]
fn the_cursor_file_section_survives_an_lru_pressure_ensure() {
    // Forty tiny files: wheeling the whole stream and back caches far more
    // sections than the budget keeps, so the next `ensure` really evicts.
    let repo = init_repo();
    for i in 0..40 {
        write(repo.path(), &format!("f{i:02}.txt"), "one\ntwo\n");
    }
    let mut app = diff_focused_app(&repo);
    for _ in 0..400 {
        wheel_down(&mut app);
    }
    for _ in 0..400 {
        wheel_up(&mut app);
    }
    assert_eq!(common::selected_path(&app), "f00.txt", "back at the top");

    let placed = address(unstaged("f01.txt"), RowTarget::Code(1));
    assert!(app.place_cursor(placed.clone()));
    prepare_window(&mut app);

    assert_eq!(
        app.cursor_address(),
        Some(placed),
        "the cursor's section is pinned against the eviction pass, so its \
         address still resolves"
    );
    assert!(app.cursor_window_span().is_some());
}

// --- the converged case is untouched ---------------------------------------

#[test]
fn a_same_path_section_change_carries_the_converged_cursor_over() {
    // One path, two stream rows: staging moves the *same* file between them, and
    // both rows draw the same net HEAD→worktree diff. The cursor's target still
    // means the same row after the move, so the address is re-pointed at the row
    // the selection landed on rather than left naming the row it left (which
    // would read as divergent and be swept to the top).
    let repo = dup_path_repo();
    let mut app = app_for(&repo, config(true, false));
    dump(&app, W, H);
    press(&mut app, 'l');
    press(&mut app, 'j');
    press(&mut app, 'j'); // onto the first added line of the staged row
    assert_eq!(
        app.cursor_address(),
        Some(address(staged("dup.txt"), RowTarget::Code(1)))
    );

    app.on_key(tab()); // focus the list…
    press(&mut app, 'j'); // …and select the same path's working-tree row

    assert_eq!(common::selected_path(&app), "dup.txt");
    assert_eq!(
        app.cursor_address(),
        Some(address(unstaged("dup.txt"), RowTarget::Code(1))),
        "the target survived; only the section it is addressed in changed"
    );
    assert!(!app.cursor_divergent());

    // And the reverse move, back up into the staged row.
    press(&mut app, 'k');
    assert_eq!(
        app.cursor_address(),
        Some(address(staged("dup.txt"), RowTarget::Code(1))),
        "the same rebinding going the other way"
    );
    assert!(!app.cursor_divergent());
}

#[test]
fn a_section_change_onto_a_different_path_still_resets_the_cursor() {
    // The rebinding is same-path only: a real file change is a new layout, so
    // the cursor goes back to the top exactly as it always did.
    let repo = init_repo();
    write(repo.path(), "a.txt", "a one\na two\n");
    git(repo.path(), &["add", "a.txt"]);
    write(repo.path(), "b.txt", "b one\nb two\nb three\n");
    let mut app = app_for(&repo, config(true, false));
    dump(&app, W, H);
    press(&mut app, 'l');
    press(&mut app, 'j');
    press(&mut app, 'j');
    assert_eq!(
        app.cursor_address(),
        Some(address(staged("a.txt"), RowTarget::Code(1)))
    );

    app.on_key(tab());
    press(&mut app, 'j'); // the unstaged b.txt — a different file

    assert_eq!(
        app.cursor_address(),
        Some(address(unstaged("b.txt"), RowTarget::FileHeader)),
        "a new file starts at its own first row"
    );
}

#[test]
fn an_anchor_cursor_is_not_swept_by_the_layout_toggles() {
    let repo = two_short_files();
    let mut app = diff_focused_app(&repo);
    press(&mut app, 'j');
    press(&mut app, 'j'); // onto the first added line
    let before = app.cursor_address();
    assert_eq!(
        before,
        Some(address(unstaged("a.txt"), RowTarget::Code(1))),
        "an anchor cursor on a code row"
    );

    press(&mut app, 'n'); // line numbers: a layout-key change, not a cursor reset
    assert_eq!(app.cursor_address(), before, "the anchor cursor is kept");
    press(&mut app, 'w');
    assert_eq!(app.cursor_address(), before, "wrap likewise");
    app.on_resize(W, H);
    assert_eq!(app.cursor_address(), before, "and a resize");
}

// --- the walk (plan 007 §3.3f/h) --------------------------------------------

/// Three short files: the whole stream fits one viewport, so the cursor can walk
/// A→B→C without the anchor ever having to move.
fn three_short_files() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "a.txt", "a one\na two\n");
    write(repo.path(), "b.txt", "b one\n");
    write(repo.path(), "c.txt", "c one\n");
    repo
}

/// The diff pane's body glyphs, row by row.
fn body(app: &App) -> Vec<String> {
    let area = app.diff_area();
    let buf = render_buffer(app, W, H);
    (area.y..area.y + area.height)
        .map(|y| diff_row_text(&buf, area, y))
        .collect()
}

/// The pane's border title.
fn title(app: &App) -> String {
    let buf = render_buffer(app, W, H);
    pane_title(&buf, app.diff_area())
}

/// §3.2f's continuity rule for a *keyboard* step: after moving down by `step`
/// rows every row still on screen holds the content it did before. Glyphs only —
/// the walk moves the cursor highlight as well as the view, so the styling of the
/// row the cursor left (and the one it arrived on) legitimately changes.
fn assert_shift_down(before: &[String], after: &[String], step: usize) {
    for y in 0..before.len().saturating_sub(step) {
        assert_eq!(
            after[y],
            before[y + step],
            "body row {y} after the step should be row {} from before it",
            y + step
        );
    }
}

/// The mirror image, for an upward step.
fn assert_shift_up(before: &[String], after: &[String], step: usize) {
    for y in 0..before.len().saturating_sub(step) {
        assert_eq!(
            after[y + step],
            before[y],
            "body row {} after the step should be row {y} from before it",
            y + step
        );
    }
}

#[test]
fn the_cursor_walks_through_two_files_while_the_first_stays_anchored() {
    let repo = three_short_files();
    let mut app = diff_focused_app(&repo);
    let rows = app.diff_row_count(); // a.txt: header, hunk, two code rows
    let before = body(&app);

    // Off the end of a.txt: the next press is the boundary, and every press after
    // it walks the *cursor* alone — the stream already fits the viewport, so
    // there is nothing for the anchor to follow (plan 007 §3.3f). Nothing scrolls
    // either, so the highlight advances a target at a time, right through the
    // boundary — one screen row per press across a.txt's one-row targets, and the
    // press that crosses into b.txt lands on its header's first row.
    for press_no in 1..=rows {
        press(&mut app, 'j');
        assert_eq!(
            cursor_screen_row(&app),
            app.diff_area().y + press_no as u16,
            "press {press_no}: one row down"
        );
    }
    assert_eq!(
        app.cursor_address(),
        Some(address(unstaged("b.txt"), RowTarget::FileHeader))
    );
    assert_eq!(common::selected_path(&app), "a.txt");

    // b.txt is not the stream's first file, so its header is two rows under one
    // target (plan 008 §3.5): stepping off it lands past *both*, and crossing the
    // file takes one press fewer than it has rows.
    let b_rows = window_of(&app).segments[1].rows();
    for press_no in 1..b_rows {
        press(&mut app, 'j');
        assert_eq!(
            cursor_screen_row(&app),
            app.diff_area().y + (rows + press_no + 1) as u16,
            "press {press_no} of the second boundary"
        );
    }
    assert_eq!(
        app.cursor_address(),
        Some(address(unstaged("c.txt"), RowTarget::FileHeader)),
        "the walk carried on into the third file"
    );
    assert_eq!(
        common::selected_path(&app),
        "a.txt",
        "with a.txt anchored the whole way"
    );
    assert!(title(&app).contains("a.txt"), "{}", title(&app));
    assert_eq!(app.diff_scroll.get(), 0, "and nothing scrolled");
    assert_eq!(body(&app), before, "the same rows, start to finish");
}

#[test]
fn a_walk_across_a_boundary_scrolls_a_row_per_press_and_flips_at_the_threshold() {
    let repo = two_tall_files();
    let mut app = diff_focused_app(&repo);
    let v = app.diff_area().height as usize;
    let r_a = app.diff_row_count();
    press(&mut app, 'G'); // a.txt's last stop, revealed at its hard edge
    assert_eq!(app.diff_scroll.get(), r_a - v);
    let mut before = body(&app);

    // Each press walks one stop into b.txt and pulls the view down: the boundary
    // stays on screen (a.txt's tail above, b.txt's head below) instead of the
    // whole viewport teleporting. The *first* press pulls two rows, not one —
    // b.txt's header is a two-row target since plan 008 §3.5, and revealing a
    // target reveals its whole span, exactly as landing on a comment box does.
    for press_no in 1..v {
        press(&mut app, 'j');
        let after = body(&app);
        assert_shift_down(&before, &after, if press_no == 1 { 2 } else { 1 });
        if press_no == 1 {
            // The boundary frame: a.txt's tail still fills the pane, with b.txt's
            // rule and band arriving together and the band — the row the cursor
            // sits on — on the bottom row.
            assert!(
                after[v - 1].contains("b.txt"),
                "the arriving band: {}",
                after[v - 1]
            );
            assert!(
                after[v - 2].chars().all(|c| c == '─'),
                "its rule row above it: {}",
                after[v - 2]
            );
            assert!(
                after[v - 3].contains("alpha"),
                "the departed tail above that: {}",
                after[v - 3]
            );
        }
        before = after;
        assert_eq!(app.selected, 0, "press {press_no}: the anchor stays put");
        assert!(
            title(&app).contains("a.txt"),
            "press {press_no}: {}",
            title(&app)
        );
        assert_eq!(app.diff_scroll.get(), r_a - v + press_no + 1);
        assert_eq!(
            app.cursor_address().map(|a| a.file),
            Some(unstaged("b.txt")),
            "press {press_no}: the cursor is in the file below"
        );
        // The target's *last* row rides the bottom edge — which on press 1 is the
        // band of a two-row header whose rule sits the row above it.
        assert_eq!(
            app.cursor_window_span()
                .expect("the cursor is in the window")
                .end,
            v,
            "press {press_no}: riding the bottom edge"
        );
    }

    // Press V is the first whose reveal puts the top past R_anchor — 006's strict
    // hysteresis, the same threshold the wheel flips at. It comes one press
    // earlier than it did with a one-row header, because the two-row arrival
    // spent an extra row on press 1.
    press(&mut app, 'j');
    let after = body(&app);
    assert_shift_down(&before, &after, 1);
    assert_eq!(app.selected, 1, "the anchor flipped on press {v}");
    assert!(title(&app).contains("b.txt"), "{}", title(&app));
    assert_eq!(app.diff_scroll.get(), 1, "(B, 1): one row past the top");
    // The flip is cursor-preserving: the walk carried on from where it was, and
    // did not restart at the arriving file's first row the way a wheel flip does.
    assert!(!app.cursor_divergent(), "the address converged on the flip");
    assert_eq!(
        app.review_cursor(),
        v,
        "the cursor kept walking rather than resetting to row 0"
    );
}

#[test]
fn k_walks_back_down_stream_then_flips_on_the_stop_above_the_anchor() {
    let repo = two_tall_files();
    let mut app = diff_focused_app(&repo);
    let v = app.diff_area().height as usize;
    let r_a = app.diff_row_count();
    press(&mut app, 'j'); // off the header so the cursor has somewhere to come back to
    press(&mut app, 'G');
    for _ in 0..=v {
        press(&mut app, 'j'); // walk into b.txt and past the flip threshold
    }
    assert_eq!(app.selected, 1);

    // Back up through b.txt: ordinary in-file moves, no flip until the cursor
    // reaches b.txt's own first stop.
    for _ in 0..v {
        press(&mut app, 'k');
        assert_eq!(app.selected, 1, "still walking inside b.txt");
    }
    assert_eq!(app.review_cursor(), 0, "at b.txt's first stop");
    assert_eq!(app.diff_scroll.get(), 0);
    let before = body(&app);

    press(&mut app, 'k');
    // Previous files cannot render below the anchor, so this one press is the
    // whole flip: anchor, selection and title move, the cursor converges on
    // a.txt's last target, and the view moves by exactly the one row that target
    // occupies (§3.3f's pinned asymmetry).
    assert_eq!(app.selected, 0);
    assert!(title(&app).contains("a.txt"), "{}", title(&app));
    assert!(!app.cursor_divergent());
    assert_eq!(app.review_cursor(), r_a - 1, "a.txt's last target");
    assert_eq!(app.diff_scroll.get(), r_a - 1, "top-aligned on it");
    assert_shift_up(&before, &body(&app), 1);
}

#[test]
fn the_wheel_boundary_state_and_the_walk_share_one_threshold() {
    // Plan 007 §3.3(j): wheel to exactly `(A, R_A)` — the position where b.txt's
    // header leads the pane but the title still reads a.txt — and then walk. The
    // walk must read that same position as "not past the top": it steps the cursor
    // into b.txt without moving the view or the anchor.
    let repo = two_tall_files();
    let mut app = diff_focused_app(&repo);
    let v = app.diff_area().height as usize;
    let r_a = app.diff_row_count();
    press(&mut app, 'G');
    app.wheel_scroll_window((r_a - app.diff_scroll.get()) as i64);
    assert_eq!((app.selected, app.diff_scroll.get()), (0, r_a), "(A, R_A)");
    assert!(title(&app).contains("a.txt"), "{}", title(&app));
    let parked = body(&app);

    press(&mut app, 'j');
    assert_eq!(
        app.cursor_address(),
        Some(address(unstaged("b.txt"), RowTarget::FileHeader)),
        "the cursor entered b.txt"
    );
    assert_eq!(
        (app.selected, app.diff_scroll.get()),
        (0, r_a),
        "and nothing moved: the cursor's row is already the top one"
    );
    assert_eq!(body(&app), parked, "frame-identical, highlight aside");

    // From here the cursor walks down the visible strip with the view still, and
    // the anchor changes hands only when the reveal has to push past R_anchor.
    // One press fewer than with a one-row header: the walk left b.txt's header
    // for its row 2, not its row 1, because the header's two rows are one target.
    for press_no in 2..v {
        press(&mut app, 'j');
        assert_eq!(app.selected, 0, "press {press_no}");
        assert_eq!(
            app.diff_scroll.get(),
            r_a,
            "press {press_no}: no scroll while the cursor still fits"
        );
        assert!(app.cursor_divergent(), "press {press_no}");
    }

    // The cursor has reached the bottom row of the viewport; the next press is
    // the first whose reveal has to push the top past `R_A`.
    press(&mut app, 'j');
    assert_eq!(app.selected, 1, "the anchor flipped one row past the top");
    assert_eq!(app.diff_scroll.get(), 1, "(B, 1)");
    assert!(title(&app).contains("b.txt"), "{}", title(&app));
}

#[test]
fn a_non_flipping_wheel_keeps_divergence_and_a_flipping_one_resets_it() {
    let repo = two_tall_files();
    let mut app = diverged_on_b(&repo);
    let address_before = app.cursor_address();

    // A tick that only deepens the strip leaves the address alone: its file is
    // still in the prepared window, so the invariant holds.
    wheel_down(&mut app);
    assert!(
        app.cursor_divergent(),
        "a non-flipping tick does not touch the cursor"
    );
    assert_eq!(app.cursor_address(), address_before);

    // A tick that hands the anchor over does reset it — 006's contract, and the
    // firewall between mouse scrolling and the keyboard's address (§3.3b).
    wheel_past_the_boundary(&mut app);
    assert!(!app.cursor_divergent());
    assert_eq!(app.review_cursor(), 0);
}

#[test]
fn ctrl_d_spends_its_whole_residual_across_the_boundary_and_ctrl_u_returns_it() {
    let repo = two_tall_files();
    let mut app = diff_focused_app(&repo);
    let half = (app.diff_area().height as usize / 2).max(1);
    let r_a = app.diff_row_count();

    // Half a page at a time down a.txt, until one press has to spend its residual
    // in b.txt: the walk crosses with the leftover rows intact rather than
    // stopping at the boundary for a second press (plan 007 §3.3f).
    let mut flat = 0usize;
    while flat + half < r_a {
        app.on_key(ctrl('d'));
        flat += half;
        assert!(!app.cursor_divergent(), "still inside a.txt at row {flat}");
        assert_eq!(app.review_cursor(), flat);
    }
    let landed = flat + half;
    app.on_key(ctrl('d'));
    assert!(app.cursor_divergent(), "the residual carried into b.txt");
    let expected = window_of(&app).segments[1]
        .section
        .as_ref()
        .expect("b.txt's section")
        .rows[landed - r_a]
        .target;
    assert_eq!(
        app.cursor_address(),
        Some(address(unstaged("b.txt"), expected)),
        "exactly {half} rows past row {flat} in the flattened stream"
    );
    assert_eq!(app.selected, 0, "without the anchor having to follow");

    app.on_key(ctrl('u'));
    assert!(
        !app.cursor_divergent(),
        "and straight back over the boundary"
    );
    assert_eq!(app.review_cursor(), flat, "the same residual, in reverse");
}

/// One 40-line file with a five-line note anchored to its tenth line, so the
/// comment box is a single target spanning several physical rows well below the
/// top of the layout.
fn boxed_repo() -> TempDir {
    let repo = init_repo();
    let text: String = (0..40).map(|i| format!("line {i}\n")).collect();
    write(repo.path(), "a.txt", &text);
    let base = head_oid(repo.path());
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
            line: 10,
            text: "note one\nnote two\nnote three\nnote four\nnote five".to_string(),
            context: Some("line 9".to_string()),
            orphaned: false,
            created_at: 1_700_000_000,
            base: Some(base),
            stale: false,
        }],
    );
    repo
}

#[test]
fn a_half_page_into_a_comment_box_resumes_from_the_boxs_first_row() {
    // The cursor is target-granular: a comment box is ONE stop, however many
    // physical rows it draws, so the walk canonicalises any landing inside it to
    // the box's `span.start`. A half page down into the middle of a box therefore
    // does not remember which of its rows it arrived on, and the half page back up
    // is measured from the box's first row — landing *above* where the pair
    // started. That is deliberate, and it is byte-identical to the pre-walk
    // arithmetic this replaced (`(start + step).max(end)` down, `start - step` up):
    // the walk generalised those two expressions across files without touching
    // their granularity.
    let repo = boxed_repo();
    let mut app = diff_focused_app(&repo);
    let width = app.diff_area().width;
    let half = (app.diff_area().height as usize / 2).max(1);
    let boxed = RowTarget::Comment(1);
    let box_start = app
        .diff_layout(width)
        .iter()
        .position(|row| row.target == boxed)
        .expect("the seeded note's box is in the layout");
    let box_rows = app.diff_layout(width)[box_start..]
        .iter()
        .take_while(|row| row.target == boxed)
        .count();
    assert!(
        box_rows >= 5,
        "a box of {box_rows} rows is tall enough to land inside"
    );

    // Park two rows short of a half page above the box, so the step lands *inside*
    // it rather than on its first row — the only way the canonicalisation shows.
    let parked = box_start + 2 - half;
    assert!(parked > 0, "the box sits far enough down the layout");
    for _ in 0..parked {
        press(&mut app, 'j');
    }
    let before = app.cursor_address();
    assert_eq!(app.review_cursor(), parked);
    assert_ne!(
        before.as_ref().map(|a| a.target),
        Some(RowTarget::Comment(1)),
        "and it starts on a plain code row"
    );

    app.on_key(ctrl('d'));
    assert_eq!(
        app.cursor_address(),
        Some(address(unstaged("a.txt"), boxed)),
        "a half page lands on the box — physical row {}, its third of {box_rows}",
        parked + half
    );
    assert_eq!(
        app.review_cursor(),
        box_start,
        "and the cursor reads as the whole box, from its first row"
    );

    app.on_key(ctrl('u'));
    assert_eq!(
        app.review_cursor(),
        box_start - half,
        "the way back is measured from the box's first row, not from the row the \
         step happened to land on"
    );
    assert_ne!(
        app.cursor_address(),
        before,
        "so the pair does not round-trip while the box is taller than one row"
    );
    assert_eq!(
        app.review_cursor() + 2,
        parked,
        "it lands exactly the box's own two rows higher"
    );
}

// --- the (file, target) oracle ----------------------------------------------
//
// The walk is *defined* on the flattened target stream, so the reference model is
// that stream written out: one entry per physical row of the whole file list,
// plus the two scroll rules (§3.3h's stream reveal for a divergent destination,
// today's anchor-domain clamp for a converged one). The model is written in
// representation-independent flat coordinates — a single row index into the whole
// stream — so agreeing with the app's `(anchor, offset)` pair is a real
// constraint, not a restatement of the implementation.

/// A stream whose four entries cover the awkward cases in one fixture: a path
/// listed twice (staged *and* modified, two entries that differ only by
/// [`FileId`]) and a binary file whose header is its whole section.
fn oracle_repo() -> TempDir {
    let repo = dup_path_repo();
    write(repo.path(), "bin.dat", "a\0b\0c\n"); // NUL bytes → a binary diff
    write(repo.path(), "z.txt", "z one\nz two\n");
    repo
}

/// The flattened stream: per-file row counts, and the `(file, target)` every
/// physical row belongs to. Measured by selecting each file in turn — a file's
/// layout when selected *is* its section (006 C2's parity guarantee).
fn flatten(repo: &TempDir, h: u16) -> (Vec<usize>, Vec<(FileId, RowTarget)>) {
    let mut app = app_for(repo, config(true, false));
    dump(&app, W, h);
    let mut rows = Vec::new();
    let mut flat = Vec::new();
    for index in 0..app.status.total() {
        common::select(&mut app, index, h);
        let id = app.active_file_id().expect("a selected file");
        let layout = app.diff_layout(app.diff_area().width);
        rows.push(layout.len());
        flat.extend(layout.iter().map(|row| (id.clone(), row.target)));
    }
    (rows, flat)
}

/// The reference model: where the cursor and the viewport are, in flat rows.
struct Oracle {
    rows: Vec<usize>,
    flat: Vec<(FileId, RowTarget)>,
    viewport: usize,
    anchor: usize,
    top: usize,
    cursor: usize,
}

impl Oracle {
    fn new(rows: Vec<usize>, flat: Vec<(FileId, RowTarget)>, viewport: usize) -> Self {
        Oracle {
            rows,
            flat,
            viewport,
            anchor: 0,
            top: 0,
            cursor: 0,
        }
    }

    fn total(&self) -> usize {
        self.flat.len()
    }

    fn start_of(&self, file: usize) -> usize {
        self.rows[..file].iter().sum()
    }

    fn file_of(&self, row: usize) -> usize {
        let mut base = 0;
        for (index, rows) in self.rows.iter().enumerate() {
            base += rows;
            if row < base {
                return index;
            }
        }
        self.rows.len() - 1
    }

    /// The run of rows sharing the cursor row's `(file, target)` — the model's
    /// half of `target_span`.
    fn span(&self, row: usize) -> Range<usize> {
        let key = &self.flat[row];
        let mut start = row;
        while start > 0 && self.flat[start - 1] == *key {
            start -= 1;
        }
        let mut end = row + 1;
        while end < self.total() && self.flat[end] == *key {
            end += 1;
        }
        start..end
    }

    /// Where a reveal wants the top, given the destination's `[start, end)`.
    /// `top` and `span` must be in the same domain — flat rows for the stream
    /// reveal, the anchor file's own rows for the anchor-domain one.
    fn reveal(&self, top: i64, span: &Range<i64>) -> i64 {
        let viewport = self.viewport as i64;
        if span.start < top || span.end - span.start >= viewport {
            span.start
        } else if span.end > top + viewport {
            span.end - viewport
        } else {
            top
        }
    }

    fn step(&mut self, down: bool, step: usize) {
        // Both directions measure from the cursor target's `span.start`, never
        // from the physical row the last step happened to land on: the cursor is
        // target-granular — a comment box is one stop however many rows it draws —
        // so a landing inside a multi-row target canonicalises to its first row.
        // That is the deliberate contract (and the pre-walk arithmetic verbatim),
        // not a shortcut in the model; see
        // `a_half_page_into_a_comment_box_resumes_from_the_boxs_first_row`.
        let span = self.span(self.cursor);
        // The tall-target rule (plan 006 §3.5), consulted before any crossing: a
        // *converged* cursor whose target already reaches its file's edge scrolls
        // inside the anchor rather than stepping off rows the user hasn't seen,
        // unless the anchor is at its own hard edge already. Two-row file headers
        // (plan 008 §3.5) are the first target that can outgrow the viewport
        // without a comment box, so the model has to carry the rule now: at V = 1
        // a `j` on a header-only file's header scrolls onto its band first.
        if self.file_of(self.cursor) == self.anchor {
            let file = self.anchor;
            let base = self.start_of(file);
            let stuck = if down {
                span.end - base >= self.rows[file]
            } else {
                span.start == base
            };
            let max_top = self.rows[file].saturating_sub(self.viewport);
            let offset = (self.top - base).min(max_top);
            let at_hard_edge = if down { offset >= max_top } else { offset == 0 };
            if stuck && !at_hard_edge {
                self.top = base
                    + if down {
                        (offset + step).min(max_top)
                    } else {
                        offset.saturating_sub(step)
                    };
                return;
            }
        }
        let destination = if down {
            (span.start + step).max(span.end).min(self.total() - 1)
        } else {
            span.start.saturating_sub(step)
        };
        self.cursor = destination;
        let span = self.span(destination);
        let file = self.file_of(destination);
        if file == self.anchor {
            // Converged: today's anchor-domain reveal, clamped to the anchor's own
            // last page (§3.3h keeps this verbatim).
            let base = self.start_of(file);
            let local = (span.start - base) as i64..(span.end - base) as i64;
            let new_top = self.reveal((self.top - base) as i64, &local).max(0) as usize;
            let max_top = self.rows[file].saturating_sub(self.viewport);
            self.top = base + new_top.min(max_top);
            return;
        }
        // Divergent (or above the anchor): the stream reveal, then the
        // renormalization + end-of-stream clamp the settle does.
        let raw = self.reveal(self.top as i64, &(span.start as i64..span.end as i64));
        let mut anchor = self.anchor;
        while raw > self.start_of(anchor) as i64 + self.rows[anchor] as i64
            && anchor + 1 < self.rows.len()
        {
            anchor += 1;
        }
        while raw < self.start_of(anchor) as i64 && anchor > 0 {
            anchor -= 1;
        }
        let top = raw.clamp(0, self.total().saturating_sub(self.viewport) as i64) as usize;
        while top < self.start_of(anchor) && anchor > 0 {
            anchor -= 1;
        }
        self.anchor = anchor;
        self.top = top;
    }

    /// Assert `app` is where the model says it is.
    fn check(&self, app: &App, what: &str) {
        let (file, target) = self.flat[self.cursor].clone();
        assert_eq!(
            app.cursor_address(),
            Some(address(file, target)),
            "{what}: cursor address"
        );
        assert_eq!(app.selected, self.anchor, "{what}: anchor");
        assert_eq!(
            self.start_of(app.selected) + app.diff_scroll.get(),
            self.top,
            "{what}: flat top (anchor {}, offset {})",
            app.selected,
            app.diff_scroll.get()
        );
    }
}

/// The `reveal`'s top for a *converged* destination is computed in the anchor's
/// own coordinates; this test drives the model and the app side by side.
fn walk_against_the_oracle(repo: &TempDir, h: u16, keys: &[char], label: &str) {
    let (rows, flat) = flatten(repo, h);
    let mut oracle = Oracle::new(rows, flat, (h - 4) as usize);
    let mut app = app_for(repo, config(true, false));
    dump(&app, W, h);
    press(&mut app, 'l');
    dump(&app, W, h);
    let half = ((h - 4) as usize / 2).max(1);
    oracle.check(&app, &format!("{label}: before any press"));

    for (index, key) in keys.iter().enumerate() {
        // One match for both sides: the app's key and the model's `(down, step)`
        // can't drift apart, and an unknown key can't quietly become a Ctrl-u.
        let (down, step) = match key {
            'j' => {
                press(&mut app, 'j');
                (true, 1)
            }
            'k' => {
                press(&mut app, 'k');
                (false, 1)
            }
            'd' => {
                app.on_key(ctrl('d'));
                (true, half)
            }
            'u' => {
                app.on_key(ctrl('u'));
                (false, half)
            }
            other => panic!("unknown key {other}"),
        };
        dump(&app, W, h);
        oracle.step(down, step);
        oracle.check(&app, &format!("{label}: after press {} ({key})", index + 1));
    }
}

#[test]
fn every_short_key_sequence_matches_the_flattened_oracle() {
    let repo = oracle_repo();
    let keys = ['j', 'k', 'd', 'u'];
    // Exhaustive to length three, at a viewport the stream is deeper than (so
    // boundaries, flips and the end clamp are all in range) and at V = 1.
    for h in [8u16, 5] {
        for &a in &keys {
            walk_against_the_oracle(&repo, h, &[a], &format!("h={h} [{a}]"));
            for &b in &keys {
                walk_against_the_oracle(&repo, h, &[a, b], &format!("h={h} [{a}{b}]"));
                for &c in &keys {
                    walk_against_the_oracle(&repo, h, &[a, b, c], &format!("h={h} [{a}{b}{c}]"));
                }
            }
        }
    }
}

#[test]
fn a_long_random_walk_matches_the_flattened_oracle() {
    let repo = oracle_repo();
    // V = 1, a viewport the stream is deeper than, and one deeper than the whole
    // stream (where nothing ever scrolls and every step is pure cursor movement).
    for h in [5u16, 8, 40] {
        let mut seed = 0x9e37_79b9u32;
        let keys: Vec<char> = (0..300)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ['j', 'k', 'd', 'u'][((seed >> 8) % 4) as usize]
            })
            .collect();
        walk_against_the_oracle(&repo, h, &keys, &format!("h={h} random"));
    }
}

#[test]
fn a_reveal_triggered_flip_keeps_the_very_same_address() {
    // At the end of the stream a walk press cannot move the cursor — the residual
    // is discarded — but it still reveals, and here that reveal renormalizes past
    // the boundary. So the flip is cursor-preserving in the literal sense: the
    // same `(file, target)` before and after, only converged (plan 007 §3.3h).
    let repo = two_tall_files();
    let mut app = diff_focused_app(&repo);
    let v = app.diff_area().height as usize;
    let r_a = app.diff_row_count();
    app.wheel_scroll_window((r_a - v + 1) as i64); // b.txt's first row enters the window
    let target = {
        let window = window_of(&app);
        let strip = window.segments.last().expect("a strip segment");
        assert!(!strip.is_anchor(), "b.txt is below the anchor");
        strip
            .section
            .as_ref()
            .expect("its prepared section")
            .rows
            .last()
            .expect("b.txt has rows")
            .target
    };
    assert!(
        app.place_cursor(address(unstaged("b.txt"), target)),
        "b.txt's last target — in the window's file, below its drawn rows"
    );
    let before = app.cursor_address();
    assert!(title(&app).contains("a.txt"), "{}", title(&app));

    press(&mut app, 'j');
    assert_eq!(
        app.cursor_address(),
        before,
        "the address survived the flip untouched"
    );
    assert_eq!(app.selected, 1, "which happened: the anchor moved");
    assert!(!app.cursor_divergent(), "so the address is converged now");
    assert!(title(&app).contains("b.txt"), "{}", title(&app));
}

// --- flip-then-act convergence (plan 007 §3.3g) -----------------------------

/// A worktree comment on `b.txt`'s first added line — a box in the *strip*, the
/// only place a divergent cursor can rest on one.
fn note_on_b(base: &str) -> Comment {
    Comment {
        id: 2,
        file: "b.txt".to_string(),
        line: 1,
        context: Some("b one".to_string()),
        ..note_on_a(base)
    }
}

/// The comments recorded for the repo's branch.
fn stored_notes(repo: &TempDir) -> Vec<Comment> {
    let store = comments::load(&strix_dir(repo.path())).expect("the store parses");
    store
        .branches
        .get("main")
        .map(|branch| branch.comments.clone())
        .unwrap_or_default()
}

/// Park `app` with a divergent cursor on the comment box `note_on_b` places in
/// the strip.
fn diverged_on_bs_note(repo: &TempDir) -> App {
    let mut app = diff_focused_app(repo);
    let placed = app.place_cursor(address(unstaged("b.txt"), RowTarget::Comment(2)));
    assert!(placed, "the note's box is one of b.txt's own rows");
    app
}

#[test]
fn space_stages_the_file_the_cursor_walked_into() {
    let repo = two_short_files();
    let mut app = diverged_on_b(&repo);
    assert_eq!(common::selected_path(&app), "a.txt", "anchored on a.txt");

    press(&mut app, ' ');

    assert!(
        app.status.staged.iter().any(|entry| entry.path == "b.txt"),
        "the cursor's file was staged"
    );
    assert!(
        app.status.staged.iter().all(|entry| entry.path != "a.txt"),
        "the anchor's was not"
    );
    assert_eq!(
        common::selected_path(&app),
        "b.txt",
        "and the selection followed the cursor there"
    );
    assert!(!app.cursor_divergent(), "converged");
    assert!(title(&app).contains("b.txt"), "{}", title(&app));
}

#[test]
fn s_and_u_both_follow_the_cursor_into_the_strip() {
    let repo = two_short_files();
    let mut app = diverged_on_b(&repo);

    press(&mut app, 's');
    assert!(app.status.staged.iter().any(|entry| entry.path == "b.txt"));

    // The cursor converged on b.txt with the stage, so `u` acts on it too.
    press(&mut app, 'u');
    assert!(app.status.staged.is_empty());
    assert!(app
        .status
        .unstaged
        .iter()
        .any(|entry| entry.path == "b.txt"));
}

#[test]
fn staging_with_a_converged_cursor_is_unchanged() {
    let repo = two_short_files();
    let mut app = diff_focused_app(&repo);
    press(&mut app, 'j'); // move inside the anchor: still converged
    assert!(!app.cursor_divergent());

    press(&mut app, ' ');

    assert!(
        app.status.staged.iter().any(|entry| entry.path == "a.txt"),
        "the selected file, exactly as before the walk existed"
    );
    assert_eq!(common::selected_path(&app), "a.txt");
}

#[test]
fn a_list_focused_action_after_a_walk_uses_the_list_selection() {
    let repo = two_short_files();
    let mut app = diverged_on_b(&repo);

    app.on_key(tab()); // focus leaves the diff — §3.3b sweeps the divergence
    assert!(!app.cursor_divergent());
    press(&mut app, ' ');

    assert!(
        app.status.staged.iter().any(|entry| entry.path == "a.txt"),
        "the list selection acts, not the file the cursor had walked into"
    );
}

#[test]
fn c_on_a_divergent_code_row_comments_on_that_file_at_that_line() {
    // The mis-anchor regression: before convergence, `c` paired the cursor's row
    // index with the *anchor's* path and lines.
    let repo = two_short_files();
    let mut app = diverged_on_b(&repo); // b.txt's "b two" row

    press(&mut app, 'c');
    assert!(app.editor_open(), "the editor opened");
    assert!(!app.cursor_divergent(), "and only ever opens converged");
    assert_eq!(common::selected_path(&app), "b.txt");

    for ch in "walked here".chars() {
        press(&mut app, ch);
    }
    app.on_key(common::enter());

    let stored = stored_notes(&repo);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].text, "walked here");
    assert_eq!(
        stored[0].file, "b.txt",
        "the walked-into file, not the anchor"
    );
    assert_eq!(stored[0].line, 2, "at the line the cursor stood on");
    assert_eq!(stored[0].context.as_deref(), Some("b two"));
}

#[test]
fn c_on_an_ineligible_divergent_row_flashes_without_flipping() {
    let repo = two_short_files();
    let mut app = diff_focused_app(&repo);
    let before = body(&app);

    // b.txt's file header and its `@@` row (a `Code` target `anchor_for_line`
    // refuses): both flash today, so both must flash *without* the view first
    // reorienting onto b.txt (plan 007 §3.3g step 3).
    for target in [RowTarget::FileHeader, RowTarget::Code(0)] {
        assert!(app.place_cursor(address(unstaged("b.txt"), target)));
        press(&mut app, 'c');

        assert_eq!(
            app.flash.as_ref().map(|flash| flash.text.as_str()),
            Some("can't comment here"),
            "{target:?}"
        );
        assert!(!app.editor_open(), "{target:?}");
        assert!(
            app.cursor_divergent(),
            "no flip for an ineligible row: {target:?}"
        );
        assert_eq!(common::selected_path(&app), "a.txt", "{target:?}");
        assert_eq!(app.diff_scroll.get(), 0, "{target:?}");
        assert_eq!(body(&app), before, "nor any view movement: {target:?}");
    }
}

#[test]
fn c_on_a_divergent_agent_note_flashes_without_flipping() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    let agent = Comment {
        source: Source::Agent,
        ..note_on_b(&base)
    };
    seed_store(repo.path(), "main", None, vec![agent]);
    let mut app = diverged_on_bs_note(&repo);
    let before = body(&app);

    press(&mut app, 'c');

    assert_eq!(
        app.flash.as_ref().map(|flash| flash.text.as_str()),
        Some("agent note — read-only")
    );
    assert!(!app.editor_open());
    assert!(
        app.cursor_divergent(),
        "a read-only note never flips the view"
    );
    assert_eq!(common::selected_path(&app), "a.txt");
    assert_eq!(body(&app), before);
}

#[test]
fn discard_prompts_for_the_file_under_the_cursor_and_discards_it() {
    let repo = two_short_files();
    let mut app = diverged_on_b(&repo);

    press(&mut app, 'x');
    match app.modal.as_ref() {
        Some(Modal::ConfirmDiscard { path, .. }) => assert_eq!(path, "b.txt"),
        other => panic!("expected a discard prompt for b.txt, got {other:?}"),
    }
    assert_eq!(
        common::selected_path(&app),
        "b.txt",
        "the view converged before prompting"
    );

    press(&mut app, 'y');
    assert!(
        !repo.path().join("b.txt").exists(),
        "the cursor's file was discarded"
    );
    assert!(repo.path().join("a.txt").exists(), "the anchor's was not");
}

#[test]
fn the_discard_gate_reads_the_divergent_cursor_target() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(repo.path(), "main", None, vec![note_on_b(&base)]);
    let mut app = diverged_on_bs_note(&repo);
    let before = body(&app);

    press(&mut app, 'x');

    assert!(
        app.modal.is_none(),
        "`x` on a comment row is inert wherever that row lives (plan 007 §5-B3)"
    );
    assert!(app.cursor_divergent(), "and inert means nothing moved");
    assert_eq!(common::selected_path(&app), "a.txt");
    assert_eq!(body(&app), before);
    assert_eq!(app.status_comment_count("b.txt"), 1, "nor was it deleted");
}

#[test]
fn keyboard_delete_converges_on_the_comment_it_removes() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(repo.path(), "main", None, vec![note_on_b(&base)]);
    let mut app = diverged_on_bs_note(&repo);

    press(&mut app, 'X');

    assert_eq!(app.status_comment_count("b.txt"), 0, "the note is gone");
    assert!(stored_notes(&repo).is_empty(), "and gone from the store");
    assert_eq!(
        common::selected_path(&app),
        "b.txt",
        "the view converged onto the file it deleted from"
    );
    assert!(!app.cursor_divergent());
}

#[test]
fn an_offscreen_divergent_cursor_reveals_before_it_acts() {
    // Today's two-press rule, extended to an address whose file is in the window
    // but whose row is below the rows that file draws.
    let repo = two_tall_files();
    let mut app = diff_focused_app(&repo);
    let v = app.diff_area().height as usize;
    let r_a = app.diff_row_count();
    app.wheel_scroll_window((r_a - v + 1) as i64); // b.txt's first row enters the window
    let target = {
        let window = window_of(&app);
        let strip = window.segments.last().expect("a strip segment");
        assert!(!strip.is_anchor(), "b.txt is below the anchor");
        strip
            .section
            .as_ref()
            .expect("its prepared section")
            .rows
            .last()
            .expect("b.txt has rows")
            .target
    };
    assert!(app.place_cursor(address(unstaged("b.txt"), target)));
    assert!(
        app.cursor_window_span().is_none(),
        "the address names a drawn file but an undrawn row"
    );

    press(&mut app, ' ');
    assert!(
        app.status.staged.is_empty(),
        "the first press only reveals — it must not act on a row off screen"
    );
    assert!(
        app.cursor_window_span().is_some(),
        "and the row it names is on screen now"
    );

    press(&mut app, ' ');
    assert!(
        app.status.staged.iter().any(|entry| entry.path == "b.txt"),
        "the second press acts, on the cursor's file"
    );
}

#[test]
fn the_comment_cycle_is_untouched_by_divergence() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(
        repo.path(),
        "main",
        None,
        vec![note_on_a(&base), note_on_b(&base)],
    );
    let mut app = diverged_on_b(&repo);

    // `]` selects and places its own cursor rather than converging on the current
    // one, so it lands on the first ordered comment either way (§3.3g).
    press(&mut app, ']');

    assert_eq!(common::selected_path(&app), "a.txt");
    assert_eq!(common::cursor_target(&app), Some(RowTarget::Comment(1)));
    assert!(!app.cursor_divergent(), "and leaves a converged cursor");
}

#[test]
fn a_rebuild_whose_content_changed_under_the_cursor_clears_it() {
    let repo = two_short_files();
    let base = head_oid(repo.path());
    seed_store(repo.path(), "main", None, vec![note_on_a(&base)]);
    let mut app = diff_focused_app(&repo);
    assert!(app.place_cursor(address(unstaged("b.txt"), RowTarget::Code(2))));

    // b.txt changes on disk with no refresh in between: its rebuilt rows still
    // *have* a `Code(2)`, but it denotes a different line now. One event — the
    // delete on a.txt — bumps the generation and rebuilds b.txt's section from
    // the new bytes, so resolving is not enough to keep the address alive.
    write(repo.path(), "b.txt", "b one\nb two changed\nb three\n");
    let close = app.comment_close_rect(1).expect("the note's [x] rect");
    app.on_mouse_at(click(close.x, close.y), Instant::now());

    assert!(
        !app.cursor_divergent(),
        "a target that resolves against different content is not the same target"
    );
    assert_eq!(
        app.cursor_address().map(|a| a.file),
        Some(unstaged("a.txt")),
        "it reset to the anchor's top"
    );
}

/// A worktree note on the first added line of `two_tall_files`' `b.txt`, with a
/// body long enough that its box is taller than the viewport.
fn tall_note_on_b(base: &str, source: Source) -> Comment {
    let text: Vec<String> = (0..20).map(|i| format!("note line {i}")).collect();
    Comment {
        id: 2,
        source,
        file: "b.txt".to_string(),
        line: 1,
        text: text.join("\n"),
        context: Some("beta 0".to_string()),
        ..note_on_a(base)
    }
}

/// `two_tall_files` parked with the cursor on `b.txt`'s note box where the
/// viewport bottom cuts through it: the box's first row is drawn, its tail is
/// not.
fn diverged_on_a_clipped_box(repo: &TempDir) -> App {
    let mut app = diff_focused_app(repo);
    let v = app.diff_area().height as usize;
    let r_a = app.diff_row_count();
    // a.txt's tail plus b.txt's first five rows: its two header rows (rule and
    // band — b.txt is not the stream's first file), `@@`, the first added line,
    // then the box's opening row.
    app.wheel_scroll_window((r_a - v + 5) as i64);
    assert!(app.place_cursor(address(unstaged("b.txt"), RowTarget::Comment(2))));
    assert!(
        app.cursor_window_span().is_none(),
        "the box is clipped by the viewport bottom"
    );
    app
}

#[test]
fn a_bottom_clipped_divergent_box_is_visible_enough_to_act_on() {
    let repo = two_tall_files();
    let base = head_oid(repo.path());
    seed_store(
        repo.path(),
        "main",
        None,
        vec![tall_note_on_b(&base, Source::Human)],
    );
    let mut app = diverged_on_a_clipped_box(&repo);

    press(&mut app, ' ');

    assert!(
        app.status.staged.iter().any(|entry| entry.path == "b.txt"),
        "the first press acts: a clipped tail does not make the row unseen"
    );
}

#[test]
fn c_on_a_bottom_clipped_divergent_agent_note_flashes_without_moving_anything() {
    // The prohibited sequence, pinned: reading the clipped box as off-screen sent
    // `c` down the reveal-only path, whose reveal renormalizes past the boundary —
    // reorienting the view for an action that was never eligible.
    let repo = two_tall_files();
    let base = head_oid(repo.path());
    seed_store(
        repo.path(),
        "main",
        None,
        vec![tall_note_on_b(&base, Source::Agent)],
    );
    let mut app = diverged_on_a_clipped_box(&repo);
    let before = body(&app);
    let offset = app.diff_scroll.get();

    press(&mut app, 'c');

    assert_eq!(
        app.flash.as_ref().map(|flash| flash.text.as_str()),
        Some("agent note — read-only")
    );
    assert!(!app.editor_open());
    assert!(app.cursor_divergent(), "no flip");
    assert_eq!(common::selected_path(&app), "a.txt");
    assert_eq!(app.diff_scroll.get(), offset, "no scroll");
    assert_eq!(body(&app), before, "no view movement at all");
}
