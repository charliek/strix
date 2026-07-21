//! The header menu bar (issue #5). C3 lays out and draws the two top-level
//! labels — `View` and `Theme` — inside the existing header row; the dropdowns,
//! open-state, and mouse/keyboard handling land in C4, which reuses
//! [`header_menu_layout`] to record each label's clickable rect. Pure geometry
//! + label text only: nothing here reads or mutates `App`.

use ratatui::layout::Rect;

use crate::app::MenuId;
use crate::ui::text_width;

/// The top-level menus, in header left-to-right draw order.
pub(crate) const MENUS: [MenuId; 2] = [MenuId::View, MenuId::Theme];

/// The bare label for a top-level menu (no padding or caret).
pub(crate) fn menu_label(id: MenuId) -> &'static str {
    match id {
        MenuId::View => "View",
        MenuId::Theme => "Theme",
    }
}

/// The drawn cell for a top-level menu label — a leading and trailing space
/// around the label plus a `▾` caret, so it reads as a dropdown: `" View ▾ "`.
pub(crate) fn menu_cell(id: MenuId) -> String {
    format!(" {} ▾ ", menu_label(id))
}

/// The display-column width of a menu's drawn cell (see [`menu_cell`]).
pub(crate) fn menu_cell_width(id: MenuId) -> u16 {
    text_width(&menu_cell(id)) as u16
}

/// The total display-column width of all top-level labels laid consecutively.
pub(crate) fn menus_width() -> u16 {
    MENUS.iter().copied().map(menu_cell_width).sum()
}

/// Lay the top-level menu labels left-to-right starting at display column
/// `start_x` on header row `y`, returning each menu's id and the single-row
/// `Rect` its drawn cell occupies. Pure — it computes columns only and draws
/// nothing, so C3's label renderer and C4's hit-rect recorder share exactly one
/// geometry.
pub(crate) fn header_menu_layout(start_x: u16, y: u16) -> Vec<(MenuId, Rect)> {
    let mut x = start_x;
    let mut rects = Vec::with_capacity(MENUS.len());
    for id in MENUS {
        let w = menu_cell_width(id);
        rects.push((id, Rect::new(x, y, w, 1)));
        x = x.saturating_add(w);
    }
    rects
}
