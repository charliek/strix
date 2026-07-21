//! Header menu bar tests (issue #5). C3 scaffold: the `View`/`Theme` labels
//! render at deterministic columns right after the brand, `m` toggles them
//! without shifting the body, and a narrow terminal keeps the labels while
//! dropping the branch. C4 dropdowns: open-state, rendering, mouse, and keyboard
//! navigation/activation are exercised in the section below.

mod common;

use std::fs;
use std::path::Path;

use common::{
    cell_symbol, init_repo, init_repo_with_diverged_branches, press, render_buffer, write,
};
use strix::app::{App, DiffMode, FlashKind, Focus, MenuId, OpenMenu};
use strix::config::Config;
use strix::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use strix::terminal::dump_frame;

const W: u16 = 120;
const H: u16 = 24;

// --- C4 dropdown helpers ----------------------------------------------------

fn mouse(col: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn click(app: &mut App, col: u16, row: u16) {
    app.on_mouse(mouse(col, row, MouseEventKind::Down(MouseButton::Left)));
}

fn moved(app: &mut App, col: u16, row: u16) {
    app.on_mouse(mouse(col, row, MouseEventKind::Moved));
}

fn key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::from(code));
}

/// Render (so title rects are recorded), click a top-level title to open its
/// dropdown, then render again so the dropdown hit-map is recorded — the
/// mouse-first open path a real session takes.
fn open_menu(app: &mut App, title_x: u16) {
    dump_frame(app, W, H).unwrap();
    click(app, title_x, 0);
    dump_frame(app, W, H).unwrap();
}

fn app_with_dir(dir: &Path) -> (tempfile::TempDir, App) {
    let repo = init_repo();
    let app = App::new(repo.path().to_path_buf())
        .unwrap()
        .with_config_dir(Some(dir.to_path_buf()));
    (repo, app)
}

fn read_config(dir: &Path) -> String {
    fs::read_to_string(dir.join("config.toml")).unwrap_or_default()
}

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

// --- C4: dropdowns (open-state, rendering, mouse, keyboard, activation) ------
//
// Geometry is deterministic (see the C3 constants above). The View dropdown
// anchors under its title column (x=7) with a 1-column border, so its content
// starts at x=8; a marker's filled dot sits at x=9 (` ● …`). Content rows begin
// at y=2 (y=1 is the box's top border). The full View row list is:
//   0 Unified · 1 Side by side · 2 sep · 3 Line numbers · 4 sep · 5 Home · 6 History

#[test]
fn open_view_menu_renders_rows_with_state_markers() {
    let (_repo, mut app) = clean_app();
    open_menu(&mut app, VIEW_LABEL_X);
    assert_eq!(app.open_menu.map(|o| o.menu), Some(MenuId::View));

    let out = dump_frame(&app, W, H).unwrap();
    for label in ["Unified", "Side by side", "Line numbers", "History"] {
        assert!(out.contains(label), "View row {label:?} missing:\n{out}");
    }

    // Each row reads " <3-col marker> <label>", so the marker field is columns
    // [inner_x+1, inner_x+4) = [9, 12): the radio dot is centred at x=10, and the
    // checkbox brackets span 9..12. Default: Unified active, line numbers on.
    let buf = render_buffer(&app, W, H);
    assert_eq!(cell_symbol(&buf, 10, 2), "●", "Unified radio filled");
    assert_eq!(cell_symbol(&buf, 9, 5), "[", "checkbox opens");
    assert_eq!(cell_symbol(&buf, 10, 5), "x", "line numbers checked");
}

#[test]
fn view_menu_markers_track_current_state() {
    let (_repo, mut app) = clean_app();
    press(&mut app, 'd'); // -> side-by-side
    press(&mut app, 'n'); // line numbers off
    open_menu(&mut app, VIEW_LABEL_X);

    let buf = render_buffer(&app, W, H);
    assert_ne!(
        cell_symbol(&buf, 10, 2),
        "●",
        "Unified no longer the active radio"
    );
    assert_eq!(cell_symbol(&buf, 10, 3), "●", "Side by side is now active");
    assert_eq!(cell_symbol(&buf, 10, 5), " ", "checkbox now unchecked");
}

#[test]
fn open_theme_menu_lists_themes_with_the_active_one_marked() {
    let (_repo, mut app) = clean_app();
    open_menu(&mut app, THEME_LABEL_X);
    assert_eq!(app.open_menu.map(|o| o.menu), Some(MenuId::Theme));

    let out = dump_frame(&app, W, H).unwrap();
    for name in ["tokyo-night", "dark", "light", "catppuccin", "gruvbox"] {
        assert!(out.contains(name), "theme {name:?} missing:\n{out}");
    }
    // tokyo-night is the default active theme → filled radio on row 0. The Theme
    // box anchors at x=15, so its marker dot sits at x=17.
    let buf = render_buffer(&app, W, H);
    assert_eq!(cell_symbol(&buf, 18, 2), "●", "active theme radio filled");
}

#[test]
fn keyboard_up_down_skips_separators_and_wraps() {
    let (_repo, mut app) = clean_app();
    open_menu(&mut app, VIEW_LABEL_X);
    assert_eq!(app.open_menu.unwrap().item, 0, "opens on the first item");

    key(&mut app, KeyCode::Down);
    assert_eq!(app.open_menu.unwrap().item, 1, "Side by side");
    key(&mut app, KeyCode::Down);
    assert_eq!(
        app.open_menu.unwrap().item,
        3,
        "skips separator 2 to Line numbers"
    );
    key(&mut app, KeyCode::Down);
    assert_eq!(app.open_menu.unwrap().item, 5, "skips separator 4 to Home");
    key(&mut app, KeyCode::Down);
    assert_eq!(app.open_menu.unwrap().item, 6, "History");
    key(&mut app, KeyCode::Down);
    assert_eq!(app.open_menu.unwrap().item, 0, "wraps to the top");
    key(&mut app, KeyCode::Up);
    assert_eq!(app.open_menu.unwrap().item, 6, "wraps back to the bottom");
}

#[test]
fn keyboard_left_right_tab_switch_between_menus() {
    let (_repo, mut app) = clean_app();
    open_menu(&mut app, VIEW_LABEL_X);

    key(&mut app, KeyCode::Right);
    assert_eq!(app.open_menu.map(|o| o.menu), Some(MenuId::Theme));
    key(&mut app, KeyCode::Left);
    assert_eq!(app.open_menu.map(|o| o.menu), Some(MenuId::View));
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.open_menu.map(|o| o.menu), Some(MenuId::Theme));
    key(&mut app, KeyCode::BackTab);
    assert_eq!(app.open_menu.map(|o| o.menu), Some(MenuId::View));
    assert_eq!(
        app.open_menu.unwrap().item,
        0,
        "a switch lands on the first item"
    );
}

#[test]
fn enter_activates_the_highlighted_row_then_closes() {
    let (_repo, mut app) = clean_app();
    open_menu(&mut app, VIEW_LABEL_X);
    key(&mut app, KeyCode::Down); // Side by side
    key(&mut app, KeyCode::Enter);
    assert!(app.open_menu.is_none(), "menu closed after Enter");
    assert_eq!(app.diff_mode, DiffMode::SideBySide);
}

#[test]
fn esc_closes_and_m_closes_without_toggling_the_bar() {
    let (_repo, mut app) = clean_app();
    open_menu(&mut app, VIEW_LABEL_X);
    key(&mut app, KeyCode::Esc);
    assert!(app.open_menu.is_none(), "Esc closed the menu");

    open_menu(&mut app, VIEW_LABEL_X);
    press(&mut app, 'm'); // the ToggleMenuBar chord, resolved while open, only closes
    assert!(app.open_menu.is_none(), "m closed the menu");
    assert!(
        app.show_menu_bar,
        "m did not also hide the bar while a menu was open"
    );
}

#[test]
fn an_unrelated_key_closes_the_menu_and_is_consumed() {
    let (_repo, mut app) = clean_app();
    open_menu(&mut app, VIEW_LABEL_X);
    press(&mut app, 'q'); // would quit if it fell through to the keymap
    assert!(app.open_menu.is_none(), "an unknown key closes the menu");
    assert!(!app.should_quit, "the key was consumed, not re-dispatched");
}

#[test]
fn clicking_titles_opens_closes_and_switches() {
    let (_repo, mut app) = clean_app();
    dump_frame(&app, W, H).unwrap();

    click(&mut app, VIEW_LABEL_X, 0);
    assert_eq!(
        app.open_menu.map(|o| o.menu),
        Some(MenuId::View),
        "click opens"
    );
    click(&mut app, VIEW_LABEL_X, 0);
    assert!(
        app.open_menu.is_none(),
        "clicking the same title again closes"
    );

    click(&mut app, VIEW_LABEL_X, 0);
    dump_frame(&app, W, H).unwrap();
    click(&mut app, THEME_LABEL_X, 0);
    assert_eq!(
        app.open_menu.map(|o| o.menu),
        Some(MenuId::Theme),
        "another title switches"
    );
}

#[test]
fn clicking_an_item_applies_persists_and_flips_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let (_repo, mut app) = app_with_dir(dir.path());
    open_menu(&mut app, VIEW_LABEL_X);

    click(&mut app, 12, 3); // the "Side by side" row (y=3)
    assert!(app.open_menu.is_none(), "activating closes the menu");
    assert_eq!(app.diff_mode, DiffMode::SideBySide);
    assert!(
        read_config(dir.path()).contains("side-by-side"),
        "setting persisted"
    );

    // Reopening reflects the new state: Side by side is now the filled radio.
    open_menu(&mut app, VIEW_LABEL_X);
    let buf = render_buffer(&app, W, H);
    assert_eq!(cell_symbol(&buf, 10, 3), "●", "Side by side now active");
    assert_ne!(cell_symbol(&buf, 10, 2), "●", "Unified no longer active");
}

#[test]
fn clicking_a_separator_is_a_no_op_that_keeps_the_menu_open() {
    let (_repo, mut app) = clean_app();
    open_menu(&mut app, VIEW_LABEL_X);
    click(&mut app, 12, 4); // the separator at y=4
    assert_eq!(
        app.open_menu.map(|o| o.menu),
        Some(MenuId::View),
        "a separator click stays open"
    );
    assert_eq!(app.diff_mode, DiffMode::Unified, "nothing was activated");
}

#[test]
fn clicking_inside_the_box_off_any_row_is_consumed() {
    let (_repo, mut app) = app_with_change();
    let before = app.selected;
    open_menu(&mut app, VIEW_LABEL_X);
    // The box's bottom border (y=9) is inside its bounds but on no content row.
    click(&mut app, 15, 9);
    assert!(
        app.open_menu.is_some(),
        "an in-box click is consumed (a fall-through would have closed the menu)"
    );
    assert_eq!(
        app.selected, before,
        "the body under the box was not actioned"
    );
}

#[test]
fn clicking_outside_closes_the_menu_and_routes_the_click() {
    let (_repo, mut app) = app_with_change();
    open_menu(&mut app, VIEW_LABEL_X);
    let diff = app.diff_area();
    click(&mut app, diff.x + 2, diff.y + 2);
    assert!(app.open_menu.is_none(), "a click away closes the menu");
    assert_eq!(
        app.focus,
        Focus::Diff,
        "the click still routed to the diff pane"
    );
}

#[test]
fn hover_slides_an_open_menu_but_never_opens_a_closed_one() {
    let (_repo, mut app) = clean_app();
    dump_frame(&app, W, H).unwrap();
    moved(&mut app, THEME_LABEL_X, 0);
    assert!(app.open_menu.is_none(), "hover never opens a closed menu");

    open_menu(&mut app, VIEW_LABEL_X);
    moved(&mut app, THEME_LABEL_X, 0);
    assert_eq!(
        app.open_menu.map(|o| o.menu),
        Some(MenuId::Theme),
        "hovering another title slides the open menu across"
    );
}

#[test]
fn a_stale_title_rect_click_after_hiding_opens_nothing() {
    let (_repo, mut app) = clean_app();
    dump_frame(&app, W, H).unwrap(); // records the title rects
    press(&mut app, 'm'); // hides the bar, clearing open-state + title rects
    assert!(!app.show_menu_bar);
    click(&mut app, VIEW_LABEL_X, 0); // click the now-stale View column
    assert!(
        app.open_menu.is_none(),
        "no dropdown opens once the bar is hidden"
    );
}

#[test]
fn a_persist_failure_while_activating_surfaces_a_flash() {
    let repo = init_repo();
    let base = tempfile::tempdir().unwrap();
    let blocker = base.path().join("blocker");
    fs::write(&blocker, "not a directory").unwrap();
    let dir = blocker.join("strix"); // unwritable: its parent is a regular file
    let mut app = App::new(repo.path().to_path_buf())
        .unwrap()
        .with_config_dir(Some(dir));

    open_menu(&mut app, VIEW_LABEL_X);
    click(&mut app, 12, 3); // Side by side
    assert_eq!(
        app.diff_mode,
        DiffMode::SideBySide,
        "the in-app change stands"
    );
    match &app.flash {
        Some(flash) => {
            assert_eq!(flash.kind, FlashKind::Info);
            assert!(
                flash.text.starts_with("couldn't save setting"),
                "unexpected flash: {}",
                flash.text
            );
        }
        None => panic!("expected a persist-failure flash"),
    }
}

#[test]
fn reload_while_a_menu_is_open_drops_it_without_panic() {
    let (_repo, mut app) = clean_app();
    open_menu(&mut app, VIEW_LABEL_X);
    app.reload();
    assert!(app.open_menu.is_none(), "reload dropped the open menu");
    dump_frame(&app, W, H).unwrap(); // must not panic

    // Render defensiveness: an out-of-range `item` (a menu that shrank between
    // frames) is indexed via `.get`/clamp, never a panicking index.
    app.open_menu = Some(OpenMenu {
        menu: MenuId::Theme,
        item: 999,
    });
    dump_frame(&app, W, H).unwrap();
}

#[test]
fn review_session_view_menu_home_row_reads_review_and_is_checked() {
    let repo = init_repo_with_diverged_branches();
    let mut app = App::for_review(repo.path().to_path_buf(), &Config::default(), "main").unwrap();
    open_menu(&mut app, VIEW_LABEL_X);

    let out = dump_frame(&app, W, H).unwrap();
    assert!(
        out.contains("Review"),
        "the home row reads 'Review':\n{out}"
    );
    // The Home row is full-index 5 → y=7; in a review session it is checked
    // (view == home == Review), so its radio is filled.
    let buf = render_buffer(&app, W, H);
    assert_eq!(
        cell_symbol(&buf, 10, 7),
        "●",
        "the Review home row is checked"
    );
}

#[test]
fn selecting_the_already_active_theme_is_a_no_op_and_does_not_persist() {
    // codex C4 finding: re-installing the active theme would re-persist, and a
    // since-deleted custom theme would resolve to the fallback and persist it.
    let dir = tempfile::tempdir().unwrap();
    let (_repo, mut app) = app_with_dir(dir.path());
    assert_eq!(app.theme_name, "tokyo-night", "default active theme");

    open_menu(&mut app, THEME_LABEL_X);
    click(&mut app, 18, 2); // the active tokyo-night row (row 0)
    assert!(app.open_menu.is_none(), "activating still closes the menu");
    assert_eq!(app.theme_name, "tokyo-night", "theme unchanged");
    assert!(
        read_config(dir.path()).is_empty(),
        "re-selecting the active theme must not persist"
    );
}

#[test]
fn a_click_in_a_stale_dropdown_box_is_consumed_not_routed() {
    // codex C4 finding: input drains before redraw, so a queued hover can switch
    // open_menu while the previous dropdown is still on screen; a click inside the
    // visible box must be consumed, not fall through to the body under it.
    let (_repo, mut app) = app_with_change();
    press(&mut app, 'l'); // focus the diff pane
    assert_eq!(app.focus, Focus::Diff);

    open_menu(&mut app, VIEW_LABEL_X); // View open + View box recorded on screen
    moved(&mut app, THEME_LABEL_X, 0); // queued hover -> open_menu = Theme, no redraw
    assert_eq!(app.open_menu.map(|o| o.menu), Some(MenuId::Theme));

    // The recorded (still-drawn) dropdown is View; a click inside it is consumed.
    // (12, 3) is the "Side by side" row of the *View* box, so a bug that resolved
    // the click against the mismatched open menu would flip diff_mode.
    let theme_before = app.theme_name.clone();
    click(&mut app, 12, 3);
    assert!(
        app.open_menu.is_none(),
        "the stale-box click closed the menu"
    );
    assert_eq!(
        app.focus,
        Focus::Diff,
        "the staging pane under the stale box was not actioned"
    );
    assert_eq!(
        app.diff_mode,
        DiffMode::Unified,
        "no command fired against the mismatched menu/rect"
    );
    assert_eq!(app.theme_name, theme_before, "theme unchanged");
}
