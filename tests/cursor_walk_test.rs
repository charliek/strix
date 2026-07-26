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
//! B1 has no keyboard walk yet, so divergence is constructed through
//! `App::place_cursor` (the same validated setter the walk and strip clicks
//! will use).

mod common;

use std::ops::Range;
use std::time::Instant;

use common::{
    app_for, click, config, dump, git, head_oid, init_repo, prepare_window, press, render_buffer,
    seed_store, tab, window_of, write,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use strix::app::{App, CursorAddress, FileId, RowTarget};
use strix::comments::{Comment, Scope, Side, Source};
use strix::crossterm::event::MouseEventKind;
use strix::git::Section;
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 24;

// --- fixtures ---------------------------------------------------------------

/// Two short untracked files: at width 120 both fit in one viewport, so `b.txt`
/// is a strip below the `a.txt` anchor from the first prepared window on.
///
/// Rows per file, cross-file on: a file header, the `@@` hunk row, then one row
/// per added line — `a.txt` is 4 rows, `b.txt` is 5.
fn two_short_files() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "a.txt", "a one\na two\n");
    write(repo.path(), "b.txt", "b one\nb two\nb three\n");
    repo
}

/// Two 40-line files (42 rows each), so the strip only appears once the
/// viewport overruns the anchor — the state divergence actually requires — and
/// the stream is deep enough below it for a wheel flip to renormalize.
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

fn unstaged(path: &str) -> FileId {
    FileId::Status {
        section: Section::Unstaged,
        path: path.to_string(),
    }
}

fn staged(path: &str) -> FileId {
    FileId::Status {
        section: Section::Staged,
        path: path.to_string(),
    }
}

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

/// Whether any cell of buffer row `y` **inside the diff pane** carries `bg`
/// (the file list draws its own selection background, which must not count).
fn diff_row_has_bg(buf: &Buffer, area: Rect, y: u16, bg: Color) -> bool {
    (area.x..area.x + area.width).any(|x| buf.cell((x, y)).map(|c| c.bg) == Some(bg))
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

    // Resolution: b.txt's own rows are [header, hunk, Code(1), Code(2), Code(3)],
    // so `Code(2)` is its row 3; the anchor draws 4 rows above it.
    assert_eq!(
        app.cursor_highlight_span(),
        Some((unstaged("b.txt"), 3..4)),
        "the highlight is (file, per-file span)"
    );
    assert_eq!(app.cursor_window_span(), Some(7..8));

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
    // own behaviour is pinned by its own suite.
    let triggers: &[Trigger] = &[
        ("refresh (r)", |app| press(app, 'r')),
        ("reload (watcher)", |app| app.reload()),
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
fn a_review_relist_clears_divergence() {
    let repo = common::init_repo_with_diverged_branches();
    let mut app = App::for_review(repo.path().to_path_buf(), &config(true, false), "main").unwrap();
    dump(&app, W, H);
    prepare_window(&mut app);
    press(&mut app, 'l'); // focus the review diff
    let file = FileId::Review {
        path: "feature2.txt".to_string(),
    };
    assert!(
        app.place_cursor(address(file, RowTarget::FileHeader)),
        "the second review file is in the prepared window"
    );

    app.reload();

    assert!(!app.cursor_divergent(), "a relist snaps the cursor home");
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
