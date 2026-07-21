//! The header menu bar (issue #5). C3 lays out and draws the two top-level
//! labels — `View` and `Theme` — inside the existing header row; the dropdowns,
//! open-state, and mouse/keyboard handling land in C4, which reuses
//! [`header_menu_layout`] to record each label's clickable rect. Pure geometry
//! + label text only: nothing here reads or mutates `App`.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, DropdownHit, Marker, MenuCommand, MenuId, MenuRow};
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

/// The fixed-width marker cell for a menu row (3 columns, so radio and check
/// rows align): a filled/empty radio dot centred in its field, or a checkbox.
fn marker_text(marker: Marker) -> &'static str {
    match marker {
        Marker::Radio(true) => " ● ",
        Marker::Radio(false) => "   ",
        Marker::Check(true) => "[x]",
        Marker::Check(false) => "[ ]",
    }
}

/// Draw the open dropdown as a bordered overlay under its title, and record its
/// hit-map for the mouse path. A no-op (clearing the hit-map) when no menu is
/// open. Pure: reads `App` (incl. `menu_items`) and records rects via setters,
/// never mutating state — the highlighted row is indexed defensively so an
/// out-of-range `item` (a menu that shrank between frames) can't panic.
pub(crate) fn render_dropdown(frame: &mut Frame, app: &App) {
    let Some(open) = app.open_menu else {
        app.set_menu_dropdown(None); // a stale hit-map must never match after close
        return;
    };
    let Some(anchor) = app.menu_title_rect(open.menu) else {
        // The bar is hidden or the title clipped away: nothing to anchor under.
        app.set_menu_dropdown(None);
        return;
    };

    let theme = &app.theme;
    let rows = app.menu_items(open.menu);
    let frame_area = frame.area();

    // Inner content width: the marker gutter + widest label, a 2-column gap, then
    // the widest right-aligned hint. Clamped so the bordered box fits the frame.
    const GUTTER: usize = 5; // " " + 3-col marker + " "
    const GAP: usize = 2;
    let mut max_left = 0usize;
    let mut max_right = 0usize;
    for row in &rows {
        if let MenuRow::Item { label, hint, .. } = row {
            max_left = max_left.max(GUTTER + text_width(label));
            if let Some(hint) = hint {
                max_right = max_right.max(text_width(hint) + 1); // hint + trailing space
            }
        }
    }
    let inner_w = (max_left + GAP + max_right)
        .max(1)
        .min(frame_area.width.saturating_sub(2) as usize);
    if inner_w == 0 {
        app.set_menu_dropdown(None);
        return;
    }

    // Vertical placement: the row below the header, scrolled to keep `item`
    // visible when the list is taller than the space beneath the bar.
    let box_top = anchor.y.saturating_add(1);
    let avail_h = frame_area.bottom().saturating_sub(box_top);
    if avail_h < 3 {
        // No room for even a one-row bordered box.
        app.set_menu_dropdown(None);
        return;
    }
    let max_visible = (avail_h - 2) as usize;
    let total = rows.len();
    let visible_count = total.min(max_visible);
    let item = open.item.min(total.saturating_sub(1));
    // `item < visible_count` subsumes the fits-entirely case (then `item <=
    // total-1 < visible_count`), so the else branch only runs when the list is
    // taller than the window and `total - visible_count` can't underflow.
    let window_start = if item < visible_count {
        0
    } else {
        (item + 1 - visible_count).min(total - visible_count)
    };

    // Horizontal placement: anchored under the title, shifted left to stay on
    // screen if the box would cross the right edge.
    let box_w = (inner_w as u16).saturating_add(2);
    let mut box_x = anchor.x;
    if box_x.saturating_add(box_w) > frame_area.right() {
        box_x = frame_area.right().saturating_sub(box_w);
    }
    box_x = box_x.max(frame_area.x);
    let box_rect = Rect::new(box_x, box_top, box_w, visible_count as u16 + 2);
    let inner_x = box_x + 1;

    frame.render_widget(Clear, box_rect);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme.border))
            .style(Style::new().bg(theme.bg)),
        box_rect,
    );

    let mut hit_rows: Vec<(Option<MenuCommand>, Rect)> = Vec::with_capacity(visible_count);
    for vis in 0..visible_count {
        let full_idx = window_start + vis;
        let row_rect = Rect::new(inner_x, box_top + 1 + vis as u16, inner_w as u16, 1);
        let highlighted = full_idx == open.item;
        match &rows[full_idx] {
            MenuRow::Separator => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        "─".repeat(inner_w),
                        Style::new().fg(theme.dim),
                    )))
                    .style(Style::new().bg(theme.bg)),
                    row_rect,
                );
                hit_rows.push((None, row_rect));
            }
            MenuRow::Item {
                label,
                marker,
                hint,
                command,
            } => {
                let left = format!(" {} {}", marker_text(*marker), label);
                let hint_str = hint.map(|h| format!("{h} ")).unwrap_or_default();
                let pad = inner_w
                    .saturating_sub(text_width(&left))
                    .saturating_sub(text_width(&hint_str));
                let row_bg = if highlighted {
                    theme.selection_bg
                } else {
                    theme.bg
                };
                let label_fg = if highlighted {
                    theme.selection_fg
                } else {
                    theme.fg
                };
                let mut label_style = Style::new().fg(label_fg);
                if highlighted {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                }
                let spans = vec![
                    Span::styled(left, label_style),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(hint_str, Style::new().fg(theme.dim)),
                ];
                frame.render_widget(
                    Paragraph::new(Line::from(spans)).style(Style::new().bg(row_bg)),
                    row_rect,
                );
                hit_rows.push((Some(command.clone()), row_rect));
            }
        }
    }

    app.set_menu_dropdown(Some(DropdownHit {
        menu: open.menu,
        bounds: box_rect,
        window_start,
        rows: hit_rows,
    }));
}
