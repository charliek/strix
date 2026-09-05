//! The diff pane's file-header row (plan 006 §3.1, restyled by plan 008 §3.3):
//! with cross-file scroll on, every Status/Review file's layout is led by a
//! one-row band carrying a tone-coloured accent bar, the change marker, the dim
//! directory prefix, the basename on its own chip, and the `+a −d` counts at the
//! right edge. The band's label is the file lists' label *split*, not reworded —
//! `prefix + name` is exactly `display_path()`. With cross-file scroll off the
//! row doesn't exist and every frame is what it always was.

mod common;

use std::time::{Duration, Instant};

use common::{
    cell_bg, cell_fg, click, git, init_repo, init_repo_with_diverged_branches,
    init_repo_with_history, key, mouse, press, write,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use strix::app::{App, FileHeaderRow, FlashKind, RowContent};
use strix::config::Config;
use strix::crossterm::event::MouseEventKind;
use strix::terminal::dump_frame;
use tempfile::TempDir;

const W: u16 = 120;
const H: u16 = 24;

fn config(cross_file: bool) -> Config {
    Config {
        cross_file_scroll: Some(cross_file),
        wrap_lines: Some(false),
        ..Config::default()
    }
}

fn app_for(repo: &TempDir, cross_file: bool) -> App {
    App::with_config(repo.path().to_path_buf(), &config(cross_file)).unwrap()
}

fn frame(app: &App) -> Vec<String> {
    dump_frame(app, W, H)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The diff pane's rows only — the frame lines cropped to `diff_area`'s columns,
/// so the Changes panel beside them never leaks into an assertion.
fn diff_rows(app: &App) -> Vec<String> {
    let lines = frame(app);
    let area = app.diff_area();
    (area.y..area.bottom())
        .map(|y| {
            lines[y as usize]
                .chars()
                .skip(area.x as usize)
                .take(area.width as usize)
                .collect()
        })
        .collect()
}

/// The diff pane's first content row — where the header band lands when
/// cross-file scroll is on.
fn top_row(app: &App) -> String {
    diff_rows(app).remove(0)
}

/// A repo whose sole Status entry is `code.txt`, modified so the diff is exactly
/// two additions and one deletion.
fn counted_repo() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "code.txt", "one\ntwo\nthree\n");
    git(repo.path(), &["add", "code.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "add code"]);
    write(repo.path(), "code.txt", "one\nTWO\nthree\nfour\n");
    repo
}

fn review_app(repo: &TempDir, cross_file: bool) -> App {
    App::for_review(repo.path().to_path_buf(), &config(cross_file), "main").unwrap()
}

/// `counted_repo`'s file, buried deep enough that its prefix can't fit a narrow
/// pane — the fixture for the truncation order (plan 008 §3.3).
fn deep_repo() -> TempDir {
    let repo = init_repo();
    let path = "src/very/deeply/nested/module/code.txt";
    write(repo.path(), path, "one\ntwo\nthree\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "add code"]);
    write(repo.path(), path, "one\nTWO\nthree\nfour\n");
    repo
}

/// The band's cells across the pane on row `y`, as `(symbol, fg, bg, bold)`.
fn band_cells(buf: &Buffer, area: Rect, y: u16) -> Vec<(String, Color, Color, bool)> {
    (area.x..area.right())
        .map(|x| {
            let cell = buf.cell((x, y)).expect("a band cell");
            (
                cell.symbol().to_string(),
                cell.fg,
                cell.bg,
                cell.modifier.contains(Modifier::BOLD),
            )
        })
        .collect()
}

/// The band split by background — `(prefix, chip, whole row)`. The chip is the
/// run of cells carrying `file_header_chip_bg`; the prefix is what sits between
/// the bar + marker (four columns) and it.
fn band_parts(app: &App, buf: &Buffer) -> (String, String, String) {
    let area = app.diff_area();
    let cells = band_cells(buf, area, area.y);
    let chip_bg = app.theme.file_header_chip_bg;
    let chip_start = cells
        .iter()
        .position(|c| c.2 == chip_bg)
        .unwrap_or(cells.len());
    let text = |slice: &[(String, Color, Color, bool)]| -> String {
        slice.iter().map(|c| c.0.as_str()).collect()
    };
    let chip: String = cells
        .iter()
        .filter(|c| c.2 == chip_bg)
        .map(|c| c.0.as_str())
        .collect();
    (
        text(&cells[chip_start.min(4)..chip_start]),
        chip,
        text(&cells),
    )
}

/// The anchor file's own header payload, read out of its layout.
fn header_payload(app: &App) -> FileHeaderRow {
    app.diff_layout(app.diff_area().width)
        .iter()
        .find_map(|row| match &row.content {
            RowContent::FileHeader(header) => Some(header.clone()),
            _ => None,
        })
        .expect("the anchor's header row")
}

// --- content ---------------------------------------------------------------

#[test]
fn status_header_leads_the_diff_with_marker_path_and_counts() {
    let repo = counted_repo();
    let app = app_for(&repo, true);
    let row = top_row(&app);
    assert!(
        row.contains("M  code.txt"),
        "marker + chipped path, got {row:?}"
    );
    assert!(row.contains("+2"), "two additions counted, got {row:?}");
    assert!(row.contains("−1"), "one deletion counted, got {row:?}");
}

#[test]
fn the_header_band_paints_the_full_pane_width() {
    let repo = counted_repo();
    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, W, H);
    let area = app.diff_area();
    let band = app.theme.file_header_bg;
    for x in [area.x, area.x + area.width / 2, area.right() - 1] {
        assert_eq!(
            cell_bg(&buf, x, area.y),
            Some(band),
            "the band runs the whole pane width at x={x}"
        );
    }
}

#[test]
fn status_binary_header_reads_binary() {
    let repo = init_repo();
    write(repo.path(), "bin.dat", "a\0b\0c\n");
    let app = app_for(&repo, true);
    let row = top_row(&app);
    assert!(row.contains("?  bin.dat"), "untracked marker, got {row:?}");
    assert!(row.contains("(binary)"), "no counts for a binary file");
}

#[test]
fn review_header_shows_the_range_files_stats() {
    let repo = init_repo_with_diverged_branches();
    let app = review_app(&repo, true);
    let files = app.review_files();
    assert_eq!(files[0].path, "feature.txt", "first review file");
    let row = top_row(&app);
    assert!(
        row.contains("A  feature.txt"),
        "marker + chipped path, got {row:?}"
    );
    // The review header reads `CommitFile.stat` — the numstat counts, not a
    // recount of the diff.
    let stat = files[0].stat;
    assert!(
        row.contains(&format!("+{}", stat.added)) && row.contains(&format!("−{}", stat.deleted)),
        "the listed stats ({stat:?}), got {row:?}"
    );
}

#[test]
fn review_header_shows_a_rename_as_old_to_new() {
    let repo = init_repo_with_diverged_branches();
    let mut app = review_app(&repo, true);
    let renamed = app
        .review_files()
        .iter()
        .position(|f| f.path == "renamed.txt")
        .expect("the fixture renames shared.txt");
    for _ in 0..renamed {
        press(&mut app, 'j');
    }
    let row = top_row(&app);
    assert!(
        row.contains("R shared.txt →  renamed.txt"),
        "the rename display path, old path dim, basename chipped, got {row:?}"
    );
}

#[test]
fn the_prefix_and_the_name_rebuild_the_display_path() {
    // The band splits the file lists' label rather than rewording it: for a plain
    // path, a nested one and a rename, `prefix + name` is exactly what
    // `display_path()` returns. This equivalence is what keeps the band's label
    // identical in content to the sidebar's.
    let repo = init_repo();
    write(repo.path(), "plain.txt", "one\n");
    write(repo.path(), "src/deep/keep.txt", "one\n");
    write(repo.path(), "src/deep/nested.txt", "one\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "base"]);
    write(repo.path(), "plain.txt", "one\ntwo\n");
    write(repo.path(), "src/deep/keep.txt", "one\ntwo\n");
    std::fs::create_dir_all(repo.path().join("moved")).unwrap();
    std::fs::rename(
        repo.path().join("src/deep/nested.txt"),
        repo.path().join("moved/renamed.txt"),
    )
    .unwrap();
    git(repo.path(), &["add", "-A"]);

    let mut app = app_for(&repo, true);
    dump_frame(&app, W, H).unwrap();
    let labels: Vec<String> = app
        .status
        .staged
        .iter()
        .chain(app.status.unstaged.iter())
        .map(|entry| entry.display_path())
        .collect();
    assert!(
        labels.iter().any(|l| l.contains(" → ")),
        "the fixture stages a rename, got {labels:?}"
    );
    for label in &labels {
        let header = header_payload(&app);
        assert_eq!(
            format!("{}{}", header.prefix, header.name),
            *label,
            "the split label rebuilds the list label"
        );
        press(&mut app, 'j');
        dump_frame(&app, W, H).unwrap();
    }
}

// --- band styling ----------------------------------------------------------

#[test]
fn an_accent_bar_leads_the_band_in_the_marker_tone() {
    let repo = counted_repo();
    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, W, H);
    let area = app.diff_area();
    assert_eq!(common::cell_symbol(&buf, area.x, area.y), "▌");
    assert_eq!(
        cell_fg(&buf, area.x, area.y),
        Some(app.theme.unstaged),
        "the bar takes the marker tone"
    );
    assert_eq!(
        cell_bg(&buf, area.x, area.y),
        Some(app.theme.file_header_bg)
    );
}

#[test]
fn the_basename_sits_on_a_bold_chip_in_the_marker_tone() {
    let repo = counted_repo();
    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, W, H);
    let area = app.diff_area();
    let chip: Vec<_> = band_cells(&buf, area, area.y)
        .into_iter()
        .filter(|c| c.2 == app.theme.file_header_chip_bg)
        .collect();
    assert_eq!(
        chip.iter().map(|c| c.0.as_str()).collect::<String>(),
        " code.txt ",
        "the chip is the padded basename"
    );
    for (symbol, fg, _, bold) in &chip {
        assert_eq!(*fg, app.theme.unstaged, "the marker tone on {symbol:?}");
        assert!(bold, "the chip is bold on {symbol:?}");
    }
}

#[test]
fn the_directory_prefix_is_dim_and_sits_outside_the_chip() {
    let repo = init_repo();
    write(repo.path(), "src/deep/code.txt", "one\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "base"]);
    write(repo.path(), "src/deep/code.txt", "one\ntwo\n");
    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, W, H);
    let (prefix, chip, _) = band_parts(&app, &buf);
    assert_eq!(prefix, "src/deep/", "the directory leads the basename");
    assert_eq!(chip, " code.txt ", "only the basename is chipped");
    let area = app.diff_area();
    assert_eq!(
        cell_fg(&buf, area.x + 4, area.y),
        Some(app.theme.dim),
        "the prefix is dim"
    );
}

#[test]
fn an_untracked_directory_chips_its_whole_label() {
    // `git status` collapses a wholly-untracked directory to `dir/` — there is no
    // basename to chip, so the label goes on the chip rather than leaving it blank.
    let repo = init_repo();
    write(repo.path(), "fresh/code.txt", "one\n");
    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, W, H);
    let (prefix, chip, _) = band_parts(&app, &buf);
    assert_eq!(prefix, "", "nothing is left over for the prefix");
    assert_eq!(chip, " fresh/ ");
}

#[test]
fn the_counts_end_one_cell_before_the_right_edge() {
    let repo = counted_repo();
    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, W, H);
    let area = app.diff_area();
    let (_, _, row) = band_parts(&app, &buf);
    assert!(row.ends_with("+2 −1 "), "one trailing cell, got {row:?}");
    assert_eq!(
        cell_fg(&buf, area.right() - 2, area.y),
        Some(app.theme.del),
        "the deletion count is the last glyph"
    );
    assert_eq!(
        cell_bg(&buf, area.right() - 1, area.y),
        Some(app.theme.file_header_bg),
        "the trailing cell is band, not chip"
    );
}

#[test]
fn a_directory_only_rename_dims_the_old_path_and_chips_the_basename() {
    let repo = init_repo();
    write(repo.path(), "a/x.txt", "one\ntwo\nthree\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "base"]);
    std::fs::create_dir_all(repo.path().join("b")).unwrap();
    std::fs::rename(repo.path().join("a/x.txt"), repo.path().join("b/x.txt")).unwrap();
    git(repo.path(), &["add", "-A"]);

    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, W, H);
    let (prefix, chip, _) = band_parts(&app, &buf);
    assert_eq!(
        prefix, "a/x.txt → b/",
        "the whole old path, then the new directory"
    );
    assert_eq!(chip, " x.txt ", "the basename alone is chipped");
}

// --- truncation order ------------------------------------------------------

#[test]
fn a_narrow_band_cuts_the_prefix_from_the_left() {
    let repo = deep_repo();
    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, 60, H);
    let (prefix, chip, row) = band_parts(&app, &buf);
    assert!(
        prefix.starts_with('…'),
        "the prefix is cut from the left, got {prefix:?}"
    );
    assert!(
        prefix.ends_with('/'),
        "the directory nearest the name survives, got {prefix:?}"
    );
    assert_eq!(chip, " code.txt ", "the chip outranks the prefix");
    assert!(
        row.ends_with("+2 −1 "),
        "the counts still end one cell in, got {row:?}"
    );
}

#[test]
fn a_band_too_narrow_for_the_counts_still_keeps_the_chip() {
    let repo = deep_repo();
    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, 34, H);
    let (prefix, chip, row) = band_parts(&app, &buf);
    assert_eq!(chip, " code.txt ", "the chip outranks the counts");
    assert_eq!(prefix, "", "the prefix went first");
    assert!(!row.contains("+2"), "the counts are dropped, got {row:?}");
    assert!(row.starts_with("▌ M  code.txt "), "got {row:?}");
}

#[test]
fn a_long_basename_is_truncated_inside_the_chip() {
    let repo = init_repo();
    write(repo.path(), &format!("{}.txt", "x".repeat(200)), "one\n");
    let app = app_for(&repo, true);
    let buf = common::render_buffer(&app, W, H);
    let area = app.diff_area();
    let (_, chip, row) = band_parts(&app, &buf);
    assert!(
        row.starts_with("▌ ? "),
        "the bar and marker outrank the chip, got {row:?}"
    );
    assert!(
        chip.ends_with('…'),
        "the cut lands inside the chip, got {chip:?}"
    );
    assert_eq!(
        chip.chars().count(),
        area.width as usize - 4,
        "the chip takes everything past the bar and marker"
    );
    for x in area.x + 4..area.right() {
        assert_eq!(
            cell_bg(&buf, x, area.y),
            Some(app.theme.file_header_chip_bg),
            "the chip keeps its own background at x={x}"
        );
    }
}

// --- geometry --------------------------------------------------------------

#[test]
fn the_header_is_one_row_and_the_code_follows_it() {
    let repo = counted_repo();
    let app = app_for(&repo, true);
    let rows = diff_rows(&app);
    assert!(rows[0].contains("code.txt"), "row 0 is the header");
    assert!(
        rows[1].contains("@@"),
        "the hunk header follows immediately, got {:?}",
        rows[1]
    );
}

#[test]
fn the_header_never_shifts_with_horizontal_scroll() {
    let repo = init_repo();
    let long: String = (0..200)
        .map(|i| char::from(b'0' + (i % 10) as u8))
        .collect();
    write(repo.path(), "code.txt", &format!("context\n{long}\n"));
    let mut app = app_for(&repo, true);
    let before = diff_rows(&app);
    let code_row = before
        .iter()
        .position(|l| l.contains("0123456789"))
        .expect("the long line is on screen");

    let area = app.diff_area();
    for _ in 0..3 {
        app.on_mouse(mouse(area.x + 2, area.y + 2, MouseEventKind::ScrollRight));
    }
    let after = diff_rows(&app);
    assert_ne!(
        before[code_row], after[code_row],
        "the code content shifted sideways"
    );
    assert_eq!(
        before[0], after[0],
        "the header row is never h-shifted (plan 006 §3.1)"
    );
}

#[test]
fn side_by_side_draws_the_header_full_width() {
    let repo = counted_repo();
    let mut app = app_for(&repo, true);
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'd'); // side-by-side
    let rows = diff_rows(&app);
    assert!(
        rows[0].contains("M  code.txt"),
        "the header row, side-by-side"
    );
    // Full-width means the band paints over the centre divider that every code
    // row below it draws.
    let divider_x = rows[1..]
        .iter()
        .find_map(|row| row.chars().position(|c| c == '│'))
        .expect("a side-by-side pair row has a divider");
    assert_ne!(
        rows[0].chars().nth(divider_x),
        Some('│'),
        "the header spans both columns"
    );
}

// --- cursor ----------------------------------------------------------------

#[test]
fn the_header_is_a_single_cursor_stop() {
    let repo = counted_repo();
    let mut app = app_for(&repo, true);
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l'); // focus the diff
    assert_eq!(app.review_cursor(), 0, "the cursor starts on the header");

    press(&mut app, 'j');
    assert_eq!(app.review_cursor(), 1, "one step lands on the hunk header");
    press(&mut app, 'j');
    assert_eq!(app.review_cursor(), 2, "the next code row");

    // Back up onto the header, then again: it is one stop, and the first file has
    // nowhere above it to cross to.
    press(&mut app, 'k');
    press(&mut app, 'k');
    assert_eq!(app.review_cursor(), 0, "the header is a single stop");
    press(&mut app, 'k');
    assert_eq!(app.review_cursor(), 0, "no row above the header");
}

#[test]
fn the_cursor_on_the_header_spares_the_chip() {
    // The chip is the header's identity, so the band paints its own cursor state
    // rather than taking the blanket `selection_bg` repaint (plan 008 §3.4).
    let repo = counted_repo();
    let mut app = app_for(&repo, true);
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l'); // focus the diff; the cursor starts on the header
    assert_eq!(app.review_cursor_highlight(), Some(0..1), "on the header");

    let buf = common::render_buffer(&app, W, H);
    let area = app.diff_area();
    let cells = band_cells(&buf, area, area.y);
    let chip: String = cells
        .iter()
        .filter(|c| c.2 == app.theme.file_header_chip_bg)
        .map(|c| c.0.as_str())
        .collect();
    assert_eq!(chip, " code.txt ", "the chip survives the cursor colour");
    for (i, (symbol, _, bg, _)) in cells.iter().enumerate() {
        if *bg == app.theme.file_header_chip_bg {
            continue;
        }
        assert_eq!(
            *bg, app.theme.selection_bg,
            "band cell {i} ({symbol:?}) takes the cursor colour"
        );
    }
}

#[test]
fn a_click_on_the_header_only_moves_the_cursor() {
    // Review is the view whose single click places the diff cursor.
    let repo = init_repo_with_diverged_branches();
    let mut app = review_app(&repo, true);
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l');
    press(&mut app, 'j');
    press(&mut app, 'j');
    assert_eq!(app.review_cursor(), 2);

    let area = app.diff_area();
    app.on_mouse(click(area.x + 4, area.y));
    assert_eq!(app.review_cursor(), 0, "the click moved the cursor");
    assert!(!app.editor_open(), "no comment anchors on the header");
    press(&mut app, 'c');
    assert!(!app.editor_open(), "`c` is a no-op on the header row");
}

#[test]
fn c_on_the_header_flashes_like_a_hunk_row() {
    // The header anchors no line, so it takes the same route a hunk header takes
    // (`comment_input_test::c_on_a_hunk_row_flashes_and_opens_no_editor`): the
    // "can't comment here" flash, not silence.
    let repo = counted_repo();
    let mut app = app_for(&repo, true);
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l'); // focus the diff; the cursor starts on the header
    press(&mut app, 'c');
    assert!(!app.editor_open(), "no editor on the header row");
    let flash = app.flash.clone().expect("a flash");
    assert_eq!(flash.kind, FlashKind::Info);
    assert_eq!(flash.text, "can't comment here");
}

#[test]
fn the_header_marker_follows_the_section_for_a_path_in_both() {
    // `dup.txt` is staged as an addition and then modified again, so it is listed
    // in both sections with a *different* marker and tone — while the diff (one
    // net HEAD→worktree diff per path) is the same object for either row.
    let repo = init_repo();
    write(repo.path(), "dup.txt", "one\ntwo\n");
    git(repo.path(), &["add", "dup.txt"]);
    write(repo.path(), "dup.txt", "one\ntwo\nthree\n");
    let mut app = app_for(&repo, true);
    dump_frame(&app, W, H).unwrap();
    assert_eq!(app.status.total(), 2, "dup.txt is staged and unstaged");

    let marker_x = app.diff_area().x + 2;
    let staged_row = top_row(&app);
    assert!(staged_row.contains("A  dup.txt"), "got {staged_row:?}");
    assert_eq!(
        cell_fg(
            &common::render_buffer(&app, W, H),
            marker_x,
            app.diff_area().y
        ),
        Some(app.theme.staged),
        "the staged tone"
    );
    let computed = app.diff_compute_count();

    press(&mut app, 'j'); // the same path's unstaged row
    let unstaged_row = top_row(&app);
    assert!(unstaged_row.contains("M  dup.txt"), "got {unstaged_row:?}");
    assert_eq!(
        cell_fg(
            &common::render_buffer(&app, W, H),
            marker_x,
            app.diff_area().y
        ),
        Some(app.theme.unstaged),
        "the unstaged tone — the cached layout was section-stale"
    );
    assert_eq!(
        app.diff_compute_count(),
        computed,
        "the diff is path-keyed: only the layout is rebuilt"
    );
}

// --- the file lists are untouched -------------------------------------------

#[test]
fn the_review_file_list_keeps_the_plain_stat_spans() {
    // The band stopped sharing `stat_spans` with the lists; the lists themselves
    // are unchanged — no chip, no accent bar, one space after the marker.
    let repo = init_repo_with_diverged_branches();
    let app = review_app(&repo, true);
    let out = dump_frame(&app, W, H).unwrap();
    assert!(
        out.contains("  A feature.txt  +1 −0"),
        "the list row is untouched, frame:\n{out}"
    );
    assert!(
        out.contains("  R shared.txt → renamed.txt"),
        "the rename row is untouched, frame:\n{out}"
    );

    let buf = common::render_buffer(&app, W, H);
    let area = app.diff_area();
    for y in 0..H {
        for x in 0..area.x {
            assert_ne!(
                cell_bg(&buf, x, y),
                Some(app.theme.file_header_chip_bg),
                "no chip outside the diff pane at ({x}, {y})"
            );
        }
    }
}

#[test]
fn the_history_file_breakdown_keeps_the_plain_stat_spans() {
    let repo = init_repo_with_history();
    let mut app = App::with_config(repo.path().to_path_buf(), &config(true)).unwrap();
    app.on_key(key('i'));
    let out = dump_frame(&app, W, H).unwrap();
    assert!(
        out.contains("  M README.md  +1 −0"),
        "the commit-detail breakdown is untouched, frame:\n{out}"
    );

    let buf = common::render_buffer(&app, W, H);
    for y in 0..H {
        for x in 0..W {
            assert_ne!(
                cell_bg(&buf, x, y),
                Some(app.theme.file_header_chip_bg),
                "History draws no header band at ({x}, {y})"
            );
        }
    }
}

// --- toggling off ----------------------------------------------------------

#[test]
fn toggling_f_off_releases_a_cursor_parked_on_the_header() {
    let repo = counted_repo();
    let mut app = app_for(&repo, true);
    dump_frame(&app, W, H).unwrap();
    press(&mut app, 'l'); // focus the diff
    press(&mut app, 'j'); // onto the hunk header
    press(&mut app, 'k'); // back onto the header row, now *pinned* there
    assert_eq!(app.review_cursor_highlight(), Some(0..1));

    press(&mut app, 'f'); // the header row no longer exists
    assert_eq!(
        app.review_cursor_highlight(),
        Some(0..1),
        "the cursor resolves against the header-less layout"
    );
    press(&mut app, 'j');
    assert_eq!(app.review_cursor(), 1, "one step off the top row, not two");
}

#[test]
fn toggling_f_refreshes_the_scroll_metrics_before_the_next_render() {
    // A wheel tick or click drained in the same event batch as the toggle clamps
    // against these metrics, and the row count just changed by one.
    let repo = init_repo();
    let long: String = (0..80).map(|i| format!("line {i}\n")).collect();
    write(repo.path(), "tall.txt", &long);
    let mut app = app_for(&repo, true);
    dump_frame(&app, W, H).unwrap();
    let with_header = app.diff_max_scroll();
    assert!(with_header > 0, "the fixture overflows the viewport");

    press(&mut app, 'f');
    assert_eq!(
        app.diff_max_scroll(),
        with_header - 1,
        "the retired header row is already out of the bounds"
    );
    press(&mut app, 'f');
    assert_eq!(app.diff_max_scroll(), with_header, "and back again");
}

#[test]
fn a_click_drained_with_the_toggle_lands_on_the_clicked_row() {
    // Review, so a single click places the diff cursor. Scrolled to the bottom,
    // `f` off, then a click on the last row — all before the next render.
    let repo = init_repo();
    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    let long: String = (0..80).map(|i| format!("line {i}\n")).collect();
    write(repo.path(), "tall.txt", &long);
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "tall"]);
    let mut app = review_app(&repo, true);
    dump_frame(&app, W, H).unwrap();

    let area = app.diff_area();
    for _ in 0..200 {
        if app.diff_scroll.get() >= app.diff_max_scroll() {
            break;
        }
        app.on_mouse(mouse(area.x + 2, area.y + 2, MouseEventKind::ScrollDown));
    }
    press(&mut app, 'f');
    app.on_mouse(click(area.x + 4, area.bottom() - 1));
    assert_eq!(
        app.review_cursor(),
        app.diff_row_count() - 1,
        "the click read the post-toggle bounds"
    );
}

#[test]
fn a_double_click_on_the_header_opens_no_editor() {
    let repo = counted_repo();
    let mut app = app_for(&repo, true);
    dump_frame(&app, W, H).unwrap();
    let area = app.diff_area();
    let (x, y) = (area.x + 4, area.y);

    let t = Instant::now();
    app.on_mouse_at(click(x, y), t);
    app.on_mouse_at(click(x, y), t + Duration::from_millis(150));
    assert!(
        !app.editor_open(),
        "the header row is not a double-click target"
    );
}

// --- toggling + the off mode -----------------------------------------------

#[test]
fn cross_file_off_has_no_header_row() {
    let repo = counted_repo();
    let app = app_for(&repo, false);
    let row = top_row(&app);
    assert!(row.contains("@@"), "the hunk header leads, got {row:?}");
    assert!(!row.contains("code.txt"), "no file-header band");
}

#[test]
fn cross_file_off_frames_are_unchanged_by_the_header_row() {
    // The on-frame is the off-frame shifted down by exactly the header row: the
    // header inserts a row and changes nothing else about the layout.
    let repo = counted_repo();
    let off = diff_rows(&app_for(&repo, false));
    let on = diff_rows(&app_for(&repo, true));
    for i in 0..off.len() - 1 {
        assert_eq!(
            on[i + 1],
            off[i],
            "content row {i} is identical, one row lower"
        );
    }
}

#[test]
fn toggling_f_rebuilds_the_layout_in_place() {
    let repo = counted_repo();
    let mut app = app_for(&repo, false);
    dump_frame(&app, W, H).unwrap();
    let rows_off = app.diff_row_count();
    assert!(!top_row(&app).contains("code.txt"));

    press(&mut app, 'f');
    assert!(
        top_row(&app).contains("M  code.txt"),
        "the header appears without a restart"
    );
    assert_eq!(
        app.diff_row_count(),
        rows_off + 1,
        "exactly one row was added"
    );

    press(&mut app, 'f');
    assert!(!top_row(&app).contains("code.txt"), "and disappears again");
    assert_eq!(app.diff_row_count(), rows_off);
}
