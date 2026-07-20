//! Inline comment box rendering (plan §3.4, C6): the bordered multi-row box, its
//! title + `[x]`, side-by-side column placement, body wrapping, the full-box
//! cursor highlight, box-crossing navigation, the taller-than-viewport reveal,
//! the narrow-width `[x]` guarantee, and the stale-dim accent. Layout is asserted
//! via `dump_frame` text; colours via the shared `TestBackend` buffer helpers
//! (dump-frame drops styles).

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{
    cell_symbol, git, init_repo, init_repo_with_diverged_branches, render_buffer, row_has_bg,
    row_has_fg, write,
};
use strix::app::App;
use strix::comments::{Branch, Comment, Scope, Side, Source, Store};
use strix::config::Config;
use strix::crossterm::event::{KeyCode, KeyEvent};
use tempfile::TempDir;

const W: u16 = 100;
const H: u16 = 24;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn dump(app: &App) -> String {
    strix::terminal::dump_frame(app, W, H).unwrap()
}

fn dump_hw(app: &App, w: u16, h: u16) -> String {
    strix::terminal::dump_frame(app, w, h).unwrap()
}

fn strix_dir(repo: &Path) -> PathBuf {
    repo.join(".git").join("strix")
}

/// A comment anchored to `line` on `side`, with `ctx` the anchored line's text
/// (it must match, or the session-open re-anchor pass orphans the note).
#[allow(clippy::too_many_arguments)]
fn note(
    id: u64,
    source: Source,
    file: &str,
    side: Side,
    line: usize,
    ctx: &str,
    text: &str,
) -> Comment {
    Comment {
        scope: Scope::Range {
            range: String::new(),
        },
        id,
        source,
        file: file.to_string(),
        side,
        line,
        text: text.to_string(),
        context: Some(ctx.to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
        base: None,
        stale: false,
    }
}

fn stale(mut c: Comment) -> Comment {
    c.stale = true;
    c
}

fn seed(repo: &Path, branch: &str, comments: Vec<Comment>) {
    let mut branches = BTreeMap::new();
    branches.insert(
        branch.to_string(),
        Branch {
            active_range: Some("main".to_string()),
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

fn review(repo: &Path, range: &str) -> App {
    App::for_review(repo.to_path_buf(), &Config::default(), range).unwrap()
}

fn select_file(app: &mut App, path: &str) {
    let _ = dump(app);
    for _ in 0..30 {
        if app.active_diff_path().as_deref() == Some(path) {
            return;
        }
        app.on_key(key('j'));
    }
    panic!("{path} never became the selected file");
}

fn row_of(frame: &str, needle: &str) -> usize {
    frame
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("frame missing {needle:?}:\n{frame}"))
}

/// A repo whose `feature` branch adds a 40-line file (a single file in range).
fn tall_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["checkout", "-qb", "feature"]);
    let mut content = String::new();
    for i in 1..=40 {
        content.push_str(&format!("row {i}\n"));
    }
    write(p, "big.txt", &content);
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add big"]);
    dir
}

/// A repo whose `feature` branch replaces `file.txt`'s middle line (OLD → NEW),
/// so the diff is one deletion (old side) paired with one addition (new side).
fn modified_line_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    write(p, "file.txt", "line1\nOLD\nline3\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "base"]);
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "file.txt", "line1\nNEW\nline3\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "change"]);
    dir
}

// --- Unified box shape + border colour ---

#[test]
fn unified_box_renders_after_the_anchor_with_a_bordered_frame() {
    let repo = init_repo_with_diverged_branches();
    seed(
        repo.path(),
        "feature",
        vec![note(
            1,
            Source::Human,
            "feature.txt",
            Side::New,
            1,
            "feature",
            "hoist it out",
        )],
    );
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "feature.txt");
    app.on_key(key('b')); // hide the Changes panel → full-width diff, no badge noise
    let frame = dump(&app);

    // A titled top border, a body row, a bottom border — directly below the anchor.
    let anchor = row_of(&frame, "+ feature");
    let title = row_of(&frame, "● you — feature.txt R1");
    let body = row_of(&frame, "hoist it out");
    assert_eq!(title, anchor + 1, "box opens below the anchor:\n{frame}");
    assert_eq!(body, anchor + 2, "body follows the title:\n{frame}");
    assert!(frame.lines().any(|l| l.contains('╭') && l.contains("[x]")));
    assert!(frame.lines().any(|l| l.starts_with('╰') || l.contains('╰')));

    // The border row is drawn in the comment accent (dump-frame drops this).
    let buf = render_buffer(&app, W, H);
    assert!(
        row_has_fg(&buf, title as u16, app.theme.comment),
        "the box border uses the comment accent"
    );
}

// --- Side-by-side: box in the anchor's column, blanks in the other ---

#[test]
fn sbs_boxes_occupy_the_anchor_side_column_with_blank_siblings() {
    let repo = modified_line_repo();
    seed(
        repo.path(),
        "feature",
        vec![
            note(1, Source::Human, "file.txt", Side::Old, 2, "OLD", "on old"),
            note(2, Source::Human, "file.txt", Side::New, 2, "NEW", "on new"),
        ],
    );
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "file.txt");
    app.on_key(key('b')); // full-width diff, so column math is off diff_area alone
    app.on_key(key('d')); // side-by-side

    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let inner = app.diff_area();
    let left = (inner.width - 1) / 2; // matches sbs_columns(inner.width)

    // Old-side box: its own left border sits at column 0 of the diff inner; the
    // right column of that row is blank.
    let old_body = row_of(&frame, "on old") as u16;
    assert_eq!(
        cell_symbol(&buf, inner.x, old_body),
        "│",
        "old box in the left column"
    );
    for x in (inner.x + left + 1)..(inner.x + inner.width) {
        assert_eq!(
            cell_symbol(&buf, x, old_body),
            " ",
            "old-side row's right column is blank"
        );
    }

    // New-side box: the left column is blank; the box's left border sits just past
    // the centre divider.
    let new_body = row_of(&frame, "on new") as u16;
    assert_eq!(
        cell_symbol(&buf, inner.x, new_body),
        " ",
        "new-side row's left column is blank"
    );
    assert_eq!(
        cell_symbol(&buf, inner.x + left + 1, new_body),
        "│",
        "new box in the right column"
    );

    // Old emits above new (old-side comments before new-side).
    assert!(row_of(&frame, "on old") < row_of(&frame, "on new"));
}

// --- Long body wraps across rows ---

#[test]
fn a_long_body_wraps_across_multiple_rows() {
    let repo = tall_repo();
    seed(
        repo.path(),
        "feature",
        vec![note(
            1,
            Source::Human,
            "big.txt",
            Side::New,
            1,
            "row 1",
            "ALPHA this note is long enough that it must wrap onto several body rows before OMEGA",
        )],
    );
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "big.txt");
    // Keep the Changes panel (a narrower diff pane), so the ~85-column note wraps.
    let frame = dump(&app);
    let first = row_of(&frame, "ALPHA");
    let last = row_of(&frame, "OMEGA");
    assert!(
        last > first,
        "the body wrapped onto later rows (ALPHA on {first}, OMEGA on {last}):\n{frame}"
    );
}

// --- Narrow width keeps [x] visible even when the title truncates ---

#[test]
fn narrow_width_truncates_the_title_but_keeps_the_close_affordance() {
    let repo = init_repo();
    let p = repo.path();
    write(p, "a-rather-long-nested-file-name.txt", "content\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "base"]);
    git(p, &["checkout", "-qb", "feature"]);
    write(
        p,
        "a-rather-long-nested-file-name.txt",
        "content\nedited line\n",
    );
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "edit"]);
    seed(
        p,
        "feature",
        vec![note(
            1,
            Source::Human,
            "a-rather-long-nested-file-name.txt",
            Side::New,
            2,
            "edited line",
            "x",
        )],
    );
    let mut app = review(p, "main");
    select_file(&mut app, "a-rather-long-nested-file-name.txt");
    app.on_key(key('d')); // side-by-side → a narrow (~half-width) box column
    let frame = dump(&app);
    assert!(
        frame.contains("[x]"),
        "the close affordance stays visible:\n{frame}"
    );
    assert!(
        frame.contains('…'),
        "the title truncates in the narrow column:\n{frame}"
    );
}

/// Regression: an extremely narrow diff pane drives a box inner width below
/// `│ x │`; the body must degrade to a bare border column, never underflow
/// `box_w - 4` (a subtract-with-overflow panic that crashed a narrow terminal).
#[test]
fn an_extremely_narrow_pane_renders_a_box_without_panicking() {
    let repo = init_repo();
    let p = repo.path();
    write(p, "f.txt", "content\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "base"]);
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "f.txt", "content\nedited line\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "edit"]);
    seed(
        p,
        "feature",
        vec![note(
            1,
            Source::Human,
            "f.txt",
            Side::New,
            2,
            "a note long enough to force wrapping in a tiny column",
            "x",
        )],
    );
    let mut app = review(p, "main");
    select_file(&mut app, "f.txt");
    app.on_key(key('d')); // side-by-side halves the already-tiny pane
                          // Each of these previously panicked for box widths of 2–3; dump_frame must
                          // succeed (degraded, but never crash) at every size.
    for w in [24u16, 20, 16, 12, 10, 8, 6, 4] {
        let _ = dump_hw(&app, w, 14);
    }
}

// --- Selected box highlights every one of its rows ---

#[test]
fn a_selected_box_highlights_all_of_its_rows() {
    let repo = init_repo_with_diverged_branches();
    seed(
        repo.path(),
        "feature",
        // Two hard lines → a box that is title + 2 body rows + bottom = 4 rows.
        vec![note(
            1,
            Source::Human,
            "feature.txt",
            Side::New,
            1,
            "feature",
            "lineone\nlinetwo",
        )],
    );
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "feature.txt");
    app.on_key(key('b')); // hide changes → diff focused, no selected-file bg elsewhere
    app.on_key(key(']')); // land the cursor on the box

    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);
    let sel = app.theme.selection_bg;
    let title = row_of(&frame, "● you — feature.txt R1") as u16;
    for dy in 0..4 {
        assert!(
            row_has_bg(&buf, title + dy, sel),
            "box row {} is highlighted:\n{frame}",
            title + dy
        );
    }
    assert!(
        !row_has_bg(&buf, title - 1, sel),
        "the anchor code row above the box is not highlighted:\n{frame}"
    );
}

// --- j crosses a whole box in one step ---

#[test]
fn j_crosses_a_box_in_a_single_step() {
    let repo = tall_repo();
    seed(
        repo.path(),
        "feature",
        vec![note(
            1,
            Source::Human,
            "big.txt",
            Side::New,
            1,
            "row 1",
            "shortnote",
        )],
    );
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "big.txt");
    app.on_key(key('l')); // focus the diff; cursor at the top (hunk row)

    app.on_key(key('j'));
    assert_eq!(app.review_cursor(), 1, "onto the +row 1 line");
    app.on_key(key('j'));
    assert_eq!(
        app.review_cursor(),
        2,
        "onto the box (its first physical row)"
    );
    app.on_key(key('j'));
    // The box spans physical rows 2..5 (title/body/bottom); one j lands on the
    // next code row past it, never inside it.
    assert_eq!(app.review_cursor(), 5, "one step crossed the whole box");
}

// --- A box taller than the viewport reveals its top, doesn't loop ---

#[test]
fn a_box_taller_than_the_viewport_reveals_its_top() {
    let repo = tall_repo();
    let mut body = String::new();
    for i in 1..=30 {
        body.push_str(&format!("L{i:02}\n"));
    }
    seed(
        repo.path(),
        "feature",
        vec![note(
            1,
            Source::Human,
            "big.txt",
            Side::New,
            1,
            "row 1",
            body.trim_end(),
        )],
    );
    let mut app = review(repo.path(), "main");
    let _ = dump_hw(&app, W, 12);
    app.on_key(key(']')); // land + reveal the box (taller than the ~8-row viewport)
    let frame = dump_hw(&app, W, 12);
    assert!(frame.contains("L01"), "the box top is revealed:\n{frame}");
    assert!(
        !frame.contains("L30"),
        "a box taller than the viewport can't show its whole self:\n{frame}"
    );
    // The reveal top-aligns the box (its first row == the scroll offset).
    assert_eq!(app.diff_scroll, app.review_cursor());
}

// --- Stale notes render dim ---

#[test]
fn a_stale_note_renders_dim_while_a_fresh_one_uses_the_accent() {
    let repo = init_repo_with_diverged_branches();
    seed(
        repo.path(),
        "feature",
        vec![
            note(
                1,
                Source::Human,
                "feature.txt",
                Side::New,
                1,
                "feature",
                "freshbody",
            ),
            stale(note(
                2,
                Source::Human,
                "feature.txt",
                Side::New,
                1,
                "feature",
                "stalebody",
            )),
        ],
    );
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "feature.txt");
    app.on_key(key('b')); // full-width boxes → border spans the whole row
    let frame = dump(&app);
    let buf = render_buffer(&app, W, H);

    let fresh_title = (row_of(&frame, "freshbody") - 1) as u16;
    let stale_title = (row_of(&frame, "stalebody") - 1) as u16;
    assert!(
        row_has_fg(&buf, fresh_title, app.theme.comment),
        "the fresh box uses the comment accent"
    );
    assert!(
        row_has_fg(&buf, stale_title, app.theme.dim)
            && !row_has_fg(&buf, stale_title, app.theme.comment),
        "the stale box renders dim, not in the comment accent"
    );
}

// --- Multiple comments on one line stack as consecutive boxes ---

#[test]
fn two_comments_on_one_line_stack_ordered_by_id() {
    let repo = init_repo_with_diverged_branches();
    seed(
        repo.path(),
        "feature",
        vec![
            note(
                1,
                Source::Human,
                "feature.txt",
                Side::New,
                1,
                "feature",
                "firstbox",
            ),
            note(
                2,
                Source::Human,
                "feature.txt",
                Side::New,
                1,
                "feature",
                "secondbox",
            ),
        ],
    );
    let mut app = review(repo.path(), "main");
    select_file(&mut app, "feature.txt");
    let frame = dump(&app);
    let first = row_of(&frame, "firstbox");
    let second = row_of(&frame, "secondbox");
    assert!(
        second > first + 1,
        "the two boxes stack (a border sits between them):\n{frame}"
    );
}
