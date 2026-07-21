//! Side-by-side word-diff emphasis (plan §3.7, C9): a modified pair's changed
//! character spans paint with `theme.add_emph`/`del_emph` instead of the flat
//! `add_bg`/`del_bg` wash. Also covers the empty-column filler shading (plan
//! §3.2, C1): the column opposite a *pure* add/del tints with
//! `theme.add_gutter`/`del_gutter` instead of flat `theme.bg` — a separate
//! feature reached only when one side is absent, never overlapping the
//! emphasis path above (active only when both sides are present). Colours
//! aren't visible in `dump_frame`'s glyph-only text, so every assertion here
//! reads the rendered `Buffer` via the shared `TestBackend` colour helpers
//! (`render_buffer`/`row_has_bg`/`cell_bg`).

mod common;

use common::{cell_bg, cell_symbol, git, init_repo, press, render_buffer, row_has_bg, write};
use strix::app::App;
use tempfile::TempDir;

const W: u16 = 100;
const H: u16 = 24;

fn dump(app: &App) -> String {
    dump_at(app, W, H)
}

fn dump_at(app: &App, w: u16, h: u16) -> String {
    strix::terminal::dump_frame(app, w, h).unwrap()
}

/// The 0-based screen row containing `needle`, panicking if there is none —
/// `dump_frame` and `render_buffer` share the same render pass, so a text row
/// index is also the `Buffer` row index for colour assertions.
fn row_of(frame: &str, needle: &str) -> u16 {
    frame
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("frame missing {needle:?}:\n{frame}")) as u16
}

/// A repo with `name` committed as `before`, then edited (uncommitted) to
/// `after` — a single unstaged modification, so it's the sole (and thus
/// auto-selected) entry in the Status Changes list.
fn modified_file_repo(name: &str, before: &str, after: &str) -> TempDir {
    let repo = init_repo();
    let path = repo.path();
    write(path, name, before);
    git(path, &["add", name]);
    git(path, &["commit", "-q", "-m", "add file"]);
    write(path, name, after);
    repo
}

/// A `code.txt`-named [`modified_file_repo`], for tests that don't care about
/// syntax highlighting.
fn modified_repo(before: &str, after: &str) -> TempDir {
    modified_file_repo("code.txt", before, after)
}

/// `App::new` on `repo`, switched to side-by-side (word emphasis is an SBS-only
/// feature, plan §3.7).
fn sbs_app(repo: &std::path::Path) -> App {
    let mut app = App::new(repo.to_path_buf()).unwrap();
    press(&mut app, 'd');
    app
}

#[test]
fn a_modified_pair_emphasizes_changed_chars_and_keeps_base_bg_elsewhere() {
    let repo = modified_repo(
        "context line unchanged\nlet value = compute_something();\n",
        "context line unchanged\nlet value = compute_other();\n",
    );
    let app = sbs_app(repo.path());
    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let theme = &app.theme;

    // The changed word ("something" → "other") is emphasized on both sides...
    let changed = row_of(&frame, "compute_other");
    assert!(
        row_has_bg(&buf, changed, theme.del_emph),
        "old side shows del_emph on the changed span:\n{frame}"
    );
    assert!(
        row_has_bg(&buf, changed, theme.add_emph),
        "new side shows add_emph on the changed span:\n{frame}"
    );
    // ...while the shared prefix/suffix ("let value = compute_", "();") keeps
    // the flat wash.
    assert!(
        row_has_bg(&buf, changed, theme.del_bg),
        "old side keeps del_bg on its unchanged chars:\n{frame}"
    );
    assert!(
        row_has_bg(&buf, changed, theme.add_bg),
        "new side keeps add_bg on its unchanged chars:\n{frame}"
    );

    // The unmodified context line above it never carries emphasis.
    let context = row_of(&frame, "context line unchanged");
    assert!(
        !row_has_bg(&buf, context, theme.add_emph) && !row_has_bg(&buf, context, theme.del_emph),
        "an unchanged line has no emphasis:\n{frame}"
    );
}

/// Shared body for `a_pure_addition_has_no_emphasis` / `a_pure_deletion_has_no_emphasis`:
/// a pure add or pure delete keeps the flat colour wash and never carries word-diff
/// emphasis (there's no other side to diff against).
fn assert_pure_change_has_no_emphasis(before: &str, after: &str, needle: &str, is_addition: bool) {
    let repo = modified_repo(before, after);
    let app = sbs_app(repo.path());
    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let theme = &app.theme;
    let (bg, emph) = if is_addition {
        (theme.add_bg, theme.add_emph)
    } else {
        (theme.del_bg, theme.del_emph)
    };

    let row = row_of(&frame, needle);
    assert!(
        row_has_bg(&buf, row, bg),
        "pure change keeps the flat colour wash:\n{frame}"
    );
    assert!(
        !row_has_bg(&buf, row, emph),
        "pure change (no other side to diff against) has no emphasis:\n{frame}"
    );
}

#[test]
fn a_pure_addition_has_no_emphasis() {
    assert_pure_change_has_no_emphasis(
        "context\n",
        "context\nadded_only_line\n",
        "added_only_line",
        true,
    );
}

#[test]
fn a_pure_deletion_has_no_emphasis() {
    assert_pure_change_has_no_emphasis("context\nto_delete\n", "context\n", "to_delete", false);
}

// --- empty-column filler shading (plan §3.2, C1) ---
//
// Strictly separate from word-diff emphasis above: these tint the *empty*
// column opposite a pure add/del (one side `None`), never reached by the
// emphasis path (only active when both sides are `Some`).

/// The side-by-side left/right column widths for a diff pane of `width`,
/// mirroring `sbs_columns` (private to `src/app.rs`).
fn sbs_columns(width: u16) -> (u16, u16) {
    let left = (width.saturating_sub(1)) / 2;
    let right = width.saturating_sub(left + 1);
    (left, right)
}

#[test]
fn sbs_empty_old_column_opposite_a_pure_addition_is_add_gutter() {
    let repo = modified_repo("context\n", "context\nadded_only_line\n");
    let app = sbs_app(repo.path());
    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let theme = &app.theme;

    let row = row_of(&frame, "added_only_line");
    let area = app.diff_area();
    assert_eq!(
        cell_bg(&buf, area.x, row),
        Some(theme.add_gutter),
        "the empty old column opposite a pure addition is tinted add_gutter:\n{frame}"
    );
}

#[test]
fn sbs_empty_new_column_opposite_a_pure_deletion_is_del_gutter() {
    let repo = modified_repo("context\nto_delete_line\n", "context\n");
    let app = sbs_app(repo.path());
    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let theme = &app.theme;

    let row = row_of(&frame, "to_delete_line");
    let area = app.diff_area();
    let (left, _) = sbs_columns(area.width);
    let new_col_x = area.x + left + 1; // past the left column + the `│` divider
    assert_eq!(
        cell_bg(&buf, new_col_x, row),
        Some(theme.del_gutter),
        "the empty new column opposite a pure deletion is tinted del_gutter:\n{frame}"
    );
}

#[test]
fn sbs_modified_pair_and_context_row_carry_no_gutter_tint() {
    let repo = modified_repo(
        "context line unchanged\nlet value = compute_something();\n",
        "context line unchanged\nlet value = compute_other();\n",
    );
    let app = sbs_app(repo.path());
    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let theme = &app.theme;

    let modified = row_of(&frame, "compute_other");
    assert!(
        !row_has_bg(&buf, modified, theme.add_gutter)
            && !row_has_bg(&buf, modified, theme.del_gutter),
        "a modified pair (both sides present) never carries the empty-column tint:\n{frame}"
    );

    let context = row_of(&frame, "context line unchanged");
    assert!(
        !row_has_bg(&buf, context, theme.add_gutter)
            && !row_has_bg(&buf, context, theme.del_gutter),
        "a context row never carries the empty-column tint:\n{frame}"
    );
}

#[test]
fn a_below_threshold_zipped_pair_has_no_emphasis() {
    // Two lines with zero shared characters: `flush_pairs` still zips this lone
    // deletion against the lone addition that follows it (plan §2), but the
    // similarity ratio is far below the threshold, so it renders as a plain
    // add+del rather than lighting up as if one edited the other.
    let repo = modified_repo("context\nabcdefghijklmnop\n", "context\nqrstuvwxyz123456\n");
    let app = sbs_app(repo.path());
    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let theme = &app.theme;

    let row = row_of(&frame, "abcdefghijklmnop");
    assert!(row_has_bg(&buf, row, theme.del_bg));
    assert!(row_has_bg(&buf, row, theme.add_bg));
    assert!(
        !row_has_bg(&buf, row, theme.del_emph) && !row_has_bg(&buf, row, theme.add_emph),
        "an unrelated zipped pair below the similarity threshold has no emphasis:\n{frame}"
    );
}

#[test]
fn a_whitespace_only_edit_is_emphasized() {
    // One extra space inserted mid-line: a real change, not a no-op.
    let repo = modified_repo("context\nlet x = 1;\n", "context\nlet  x = 1;\n");
    let app = sbs_app(repo.path());
    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let theme = &app.theme;

    let row = row_of(&frame, "let  x = 1;");
    assert!(
        row_has_bg(&buf, row, theme.add_emph),
        "an inserted space is a real change and gets emphasized:\n{frame}"
    );
}

#[test]
fn a_selected_modified_pair_shows_selection_not_emphasis() {
    let repo = modified_repo(
        "context line unchanged\nlet value = compute_something();\n",
        "context line unchanged\nlet value = compute_other();\n",
    );
    let mut app = sbs_app(repo.path());
    press(&mut app, 'l'); // focus the diff pane (Action::FocusDiff)

    // Walk the cursor down until it lands on the modified pair's row (its
    // exact position among the hunk/context rows above it isn't pinned here).
    let mut on_target = false;
    for _ in 0..20 {
        let frame = dump(&app);
        let buf = render_buffer(&app, W, H);
        if let Some(y) = frame.lines().position(|l| l.contains("compute_other")) {
            if row_has_bg(&buf, y as u16, app.theme.selection_bg) {
                on_target = true;
                break;
            }
        }
        press(&mut app, 'j');
    }
    assert!(on_target, "cursor never reached the modified pair's row");

    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let row = row_of(&frame, "compute_other");
    assert!(
        row_has_bg(&buf, row, app.theme.selection_bg),
        "the selected row shows selection_bg:\n{frame}"
    );
    assert!(
        !row_has_bg(&buf, row, app.theme.add_emph) && !row_has_bg(&buf, row, app.theme.del_emph),
        "the cursor overrides word-diff emphasis, not the other way round:\n{frame}"
    );
}

// --- codex regressions: `highlighted_content`'s per-char loop ---

/// A width-2 (CJK) char that doesn't fit the remaining column, immediately
/// followed by a differently-highlighted syntax token (a `//` comment),
/// pinned a rendering bug: only the *inner* char loop broke on the char that
/// didn't fit, so the *outer* token loop moved on to the comment token and
/// rendered one of its chars into the leftover column — using a `char_idx`
/// that was never advanced over the skipped wide char, so `char_bg` looked up
/// the wrong offset. The fix makes the first non-fitting char stop the whole
/// line (a labeled break out of the token loop too).
///
/// `code.rs` at a 5-column-wide diff pane (`sbs_columns(5) == (2, 2)`, line
/// numbers off) gives each side exactly 2 content columns. `y`/`x` differ at
/// char 0 (a real one-char edit, so the pair is emphasized); the CJK filler
/// at char 1 needs 2 columns but only 1 remains after the changed char, so it
/// — and everything after it, including the `// note` comment — must be cut,
/// leaving one column of plain padding.
#[test]
fn wide_char_truncation_does_not_leak_the_next_token_or_desync_emphasis() {
    let repo = modified_file_repo("code.rs", "y字 // note\n", "x字 // note\n");
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    press(&mut app, 'b'); // hide the Changes panel: the diff pane gets the full width
    press(&mut app, 'd'); // side-by-side
    press(&mut app, 'n'); // line numbers off: content width == the raw column width

    let frame = dump_at(&app, 7, 10); // inner diff width 5 -> sbs_columns == (2, 2)
    let buf = render_buffer(&app, 7, 10);
    let area = app.diff_area();
    assert_eq!(
        area.width, 5,
        "the probed geometry this test's math depends on"
    );
    let y = area.y + 1; // the hunk header is the row above the paired code line
    let theme = &app.theme;

    // Old side: the changed char renders correctly emphasized...
    assert_eq!(cell_symbol(&buf, area.x, y), "y");
    assert_eq!(cell_bg(&buf, area.x, y), Some(theme.del_emph));
    // ...and the truncated CJK filler leaves plain padding, not a leaked '/'.
    assert_eq!(cell_symbol(&buf, area.x + 1, y), " ");
    assert_eq!(cell_bg(&buf, area.x + 1, y), Some(theme.del_bg));

    // New side: same shape.
    assert_eq!(cell_symbol(&buf, area.x + 3, y), "x");
    assert_eq!(cell_bg(&buf, area.x + 3, y), Some(theme.add_emph));
    assert_eq!(cell_symbol(&buf, area.x + 4, y), " ");
    assert_eq!(cell_bg(&buf, area.x + 4, y), Some(theme.add_bg));

    // Belt and braces: the comment token's text never appears anywhere in the
    // rendered frame (it would if a later token leaked past the truncation).
    assert!(
        !frame.contains("note") && !frame.contains("//"),
        "the comment token must not render past the truncated wide char:\n{frame}"
    );
}

/// `cafe` → `café` (NFD: a combining acute accent `\u{0301}` after a plain
/// `e`, so only the zero-width mark is the changed char) pinned a second bug:
/// the bg-change span break didn't special-case zero-width chars, so the
/// combining mark could split off into its own zero-width span — a terminal
/// only combines a mark with its base character when both are written in the
/// same string, so the isolated mark span rendered incorrectly (silently
/// dropped/overwritten), losing the visible change entirely. The fix glues a
/// zero-width char onto the current chunk unconditionally.
#[test]
fn a_combining_mark_stays_glued_to_its_base_char_and_is_not_dropped() {
    let repo = modified_file_repo("note.txt", "cafe\n", "cafe\u{0301}\n");
    let app = sbs_app(repo.path());
    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let area = app.diff_area();
    let y = area.y + 1;

    // The whole grapheme cluster survives in one cell — not dropped, not split
    // into its own zero-width cell that the trailing padding could overwrite.
    let cluster = (0..W)
        .map(|x| cell_symbol(&buf, x, y))
        .find(|s| s.contains('\u{0301}'));
    assert_eq!(
        cluster.as_deref(),
        Some("e\u{0301}"),
        "the base char and its combining mark render as one cell:\n{frame}"
    );
    assert!(
        frame.contains("cafe\u{0301}"),
        "the accented word is visible, not silently dropped:\n{frame}"
    );
}
