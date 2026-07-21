//! Header menu bar scaffold (issue #5, C3): the `View`/`Theme` labels render at
//! deterministic columns right after the brand, `m` toggles them without
//! shifting the body, and a narrow terminal keeps the labels while dropping the
//! branch. Dropdowns / mouse / keyboard nav are C4 and not exercised here.

mod common;

use common::{cell_symbol, init_repo, press, render_buffer, write};
use strix::app::App;
use strix::terminal::dump_frame;

const W: u16 = 120;
const H: u16 = 24;

// Column geometry is fully determined by the brand + label widths, so the tests
// derive the expected columns rather than eyeballing them:
//   brand `" strix "`      → 7 cols  → [0, 7)
//   `" View ▾ "`           → 8 cols  → [7, 15)   ('V' at 8, caret at 13)
//   `" Theme ▾ "`          → 9 cols  → [15, 24)  ('T' at 16, caret at 22)
//   context (repo name …)  → from col 24
const BRAND_W: u16 = 7;
const VIEW_CELL_W: u16 = 8;
const VIEW_LABEL_X: u16 = BRAND_W + 1; // leading space, then 'V'
const VIEW_CARET_X: u16 = BRAND_W + 1 + 4 + 1; // brand + space + "View" + space
const THEME_LABEL_X: u16 = BRAND_W + VIEW_CELL_W + 1; // after the View cell, then 'T'
const THEME_CARET_X: u16 = BRAND_W + VIEW_CELL_W + 1 + 5 + 1; // + "Theme" + space
const CONTEXT_X: u16 = BRAND_W + VIEW_CELL_W + 9; // after both cells

fn clean_app() -> (tempfile::TempDir, App) {
    let repo = init_repo();
    let app = App::new(repo.path().to_path_buf()).unwrap();
    (repo, app)
}

/// A status app with an untracked file, so the body has real content whose row
/// positions we can prove are unmoved by the menu bar.
fn app_with_change() -> (tempfile::TempDir, App) {
    let repo = init_repo();
    write(repo.path(), "extra.txt", "hello\n");
    let app = App::new(repo.path().to_path_buf()).unwrap();
    (repo, app)
}

#[test]
fn labels_render_at_deterministic_columns() {
    let (_repo, app) = clean_app();
    assert!(app.show_menu_bar, "menu bar is on by default");
    let buf = render_buffer(&app, W, H);

    assert_eq!(cell_symbol(&buf, VIEW_LABEL_X, 0), "V", "View label column");
    assert_eq!(cell_symbol(&buf, VIEW_LABEL_X + 3, 0), "w", "end of View");
    assert_eq!(cell_symbol(&buf, VIEW_CARET_X, 0), "▾", "View caret");
    assert_eq!(
        cell_symbol(&buf, THEME_LABEL_X, 0),
        "T",
        "Theme label column"
    );
    assert_eq!(cell_symbol(&buf, THEME_LABEL_X + 4, 0), "e", "end of Theme");
    assert_eq!(cell_symbol(&buf, THEME_CARET_X, 0), "▾", "Theme caret");

    // The context (repo name) begins immediately after the labels.
    let name = app.repo_name();
    assert_eq!(
        cell_symbol(&buf, CONTEXT_X, 0),
        name.chars().next().unwrap().to_string(),
        "repo name starts right after the labels"
    );

    let header = dump_frame(&app, W, H).unwrap();
    let row0 = header.lines().next().unwrap();
    assert!(row0.contains("View ▾"), "row0: {row0:?}");
    assert!(row0.contains("Theme ▾"), "row0: {row0:?}");
}

#[test]
fn m_hides_the_labels_and_keeps_the_context() {
    let (_repo, mut app) = clean_app();
    let name = app.repo_name();

    let shown = dump_frame(&app, W, H).unwrap();
    let shown0 = shown.lines().next().unwrap();
    assert!(shown0.contains("View ▾") && shown0.contains("Theme ▾"));
    assert!(shown0.contains("main"), "branch shown");
    assert!(shown0.contains(&name), "repo name shown");

    press(&mut app, 'm');
    assert!(!app.show_menu_bar, "m hid the menu bar");

    let hidden = dump_frame(&app, W, H).unwrap();
    let hidden0 = hidden.lines().next().unwrap();
    assert!(!hidden0.contains("View ▾"), "labels gone: {hidden0:?}");
    assert!(!hidden0.contains("Theme ▾"), "labels gone: {hidden0:?}");
    assert!(hidden0.contains("main"), "branch still shown");
    assert!(hidden0.contains(&name), "repo name still shown");
    // With the bar hidden the brand sits at the left and the context follows it
    // directly, exactly as before the menu bar existed.
    let buf = render_buffer(&app, W, H);
    assert_eq!(cell_symbol(&buf, 1, 0), "s", "brand at the left edge");
    assert_eq!(
        cell_symbol(&buf, BRAND_W, 0),
        name.chars().next().unwrap().to_string(),
        "context follows the brand with no labels between"
    );
}

#[test]
fn toggling_the_menu_bar_never_shifts_the_body() {
    let (_repo, mut app) = app_with_change();

    let with_bar = dump_frame(&app, 100, H).unwrap();
    press(&mut app, 'm');
    let without_bar = dump_frame(&app, 100, H).unwrap();

    // Only the header row differs; every body + footer row is byte-identical.
    let body_with: Vec<&str> = with_bar.lines().skip(1).collect();
    let body_without: Vec<&str> = without_bar.lines().skip(1).collect();
    assert_eq!(
        body_with, body_without,
        "body must not move when the bar toggles"
    );
}

#[test]
fn narrow_width_keeps_labels_and_drops_the_branch() {
    // 28 cols fully fits the brand + both labels (24 cols) but leaves too little
    // for a branch, so the branch is suppressed and the labels stay hit-accurate.
    const NARROW: u16 = 28;
    let (_repo, mut app) = app_with_change();

    let out = dump_frame(&app, NARROW, H).unwrap();
    let row0 = out.lines().next().unwrap();
    assert!(row0.contains("View ▾"), "labels kept when narrow: {row0:?}");
    assert!(
        row0.contains("Theme ▾"),
        "labels kept when narrow: {row0:?}"
    );
    assert!(
        !row0.contains("main"),
        "branch suppressed when narrow: {row0:?}"
    );

    // The body is still unmoved versus the menu-bar-off narrow header.
    press(&mut app, 'm');
    let off = dump_frame(&app, NARROW, H).unwrap();
    let body_on: Vec<&str> = out.lines().skip(1).collect();
    let body_off: Vec<&str> = off.lines().skip(1).collect();
    assert_eq!(body_on, body_off, "narrow body must not move either");
}
