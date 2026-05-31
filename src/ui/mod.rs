pub mod diff_view;
pub mod history;
pub mod modal;
pub mod staging;
pub mod syntax;
pub mod theme;

use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, ViewMode};
use crate::ui::theme::Theme;

/// Top-level render: header / body / footer, with the body split into the
/// staging pane (left) and diff pane (right).
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let theme = &app.theme;

    frame.render_widget(
        Block::new().style(Style::new().bg(theme.bg).fg(theme.fg)),
        area,
    );

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header, app);

    match app.view {
        ViewMode::Status => draw_status_body(frame, body, app),
        ViewMode::History => history::render(frame, body, app),
    }

    render_footer(frame, footer, app);

    // Overlays draw last, on top of everything.
    modal::render(frame, app);
}

/// The status view's body: the Changes panel (fixed width) beside the diff, or
/// the full-width diff when the panel is hidden.
fn draw_status_body(frame: &mut Frame, body: Rect, app: &App) {
    let theme = &app.theme;
    if app.show_changes {
        // The Changes panel is a fixed width; the diff takes the rest, so a
        // wider terminal feeds the diff. Drag the split bar to resize (see
        // `App::resize_changes`).
        let width = app.changes_pane_width(body.width);
        let [left, right] =
            Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(body);
        staging::render(frame, left, app);
        diff_view::render(frame, right, app);
        app.set_split_geometry(body, right.x);
        if app.divider_engaged() {
            highlight_divider(frame, body, right.x, theme);
        }
    } else {
        // Clear the stale staging rect so mouse hit-testing (`pane_at`) can't
        // match where the panel used to be; give the whole body to the diff.
        app.set_staging_area(Rect::default());
        diff_view::render(frame, body, app);
    }
}

/// Tint the split bar — the two adjacent pane borders at `divider_x` — with the
/// focus accent, so it reads as draggable while hovered or being dragged.
pub(crate) fn highlight_divider(frame: &mut Frame, body: Rect, divider_x: u16, theme: &Theme) {
    let style = Style::new()
        .fg(theme.border_focused)
        .add_modifier(Modifier::BOLD);
    let buf = frame.buffer_mut();
    let bottom = body.y.saturating_add(body.height);
    for y in body.y..bottom {
        for x in [divider_x.saturating_sub(1), divider_x] {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(style);
            }
        }
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    // Fill the bar, then draw left + right text on top (transparent spans), so
    // the right-aligned branch never clobbers the left title.
    frame.render_widget(Block::new().style(Style::new().bg(theme.header_bg)), area);

    let mut left_spans = vec![
        Span::styled(
            " strix ",
            Style::new()
                .fg(theme.header_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.repo_name(), Style::new().fg(theme.dim)),
    ];
    if app.view == ViewMode::History {
        left_spans.push(Span::styled(" · history", Style::new().fg(theme.title)));
    }
    frame.render_widget(Paragraph::new(Line::from(left_spans)), area);

    if let Some(branch) = app.status.head_label() {
        let right = Line::from(Span::styled(
            format!("{branch} "),
            Style::new().fg(theme.title),
        ));
        frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
    }
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;

    // A failed action shows a transient error in place of the key hints.
    if let Some(error) = &app.last_error {
        let line = Line::from(Span::styled(
            format!(" ✗ {error}"),
            Style::new().fg(theme.del).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(
            Paragraph::new(line).style(Style::new().bg(theme.footer_bg)),
            area,
        );
        return;
    }

    let key_style = Style::new()
        .fg(theme.footer_key)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::new().fg(theme.footer_fg);

    let hints: Vec<(&str, &str)> = match app.view {
        ViewMode::Status => {
            // The toggle's label tracks what the key will do next.
            let changes_label = if app.show_changes {
                "hide  "
            } else {
                "changes  "
            };
            vec![
                (" j/k ", "move  "),
                (" space ", "stage  "),
                (" d ", "split  "),
                (" b ", changes_label),
                (" y ", "history  "),
                (" ? ", "help  "),
                (" q ", "quit"),
            ]
        }
        ViewMode::History => vec![
            (" j/k ", "move  "),
            (" tab ", "pane  "),
            (" d ", "split  "),
            (" y/esc ", "status  "),
            (" ? ", "help  "),
            (" q ", "quit"),
        ],
    };
    let mut spans = Vec::new();
    for (key, label) in hints {
        spans.push(Span::styled(key, key_style));
        spans.push(Span::styled(label, label_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(theme.footer_bg)),
        area,
    );
}

/// The highlight style for a selected list row: the selection background, plus
/// the selection foreground in bold when its pane is focused. Shared by the
/// staging and history list panes so selection styling stays consistent.
pub fn selection_style(focused: bool, theme: &Theme) -> Style {
    if focused {
        Style::new()
            .bg(theme.selection_bg)
            .fg(theme.selection_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().bg(theme.selection_bg)
    }
}

/// A bordered panel with focus-aware border + title colours. Shared by the
/// staging and diff panes (and future overlays) so focus styling stays
/// consistent.
pub fn panel_block<'a>(title: &'a str, focused: bool, theme: &Theme) -> Block<'a> {
    let border_color = if focused {
        theme.border_focused
    } else {
        theme.border
    };
    let title_color = if focused { theme.title } else { theme.dim };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border_color))
        .title(Span::styled(
            title,
            Style::new().fg(title_color).add_modifier(Modifier::BOLD),
        ))
}

/// A sub-rect of `area` that is `height` rows tall and vertically centred,
/// spanning the full width. Used to place empty-state hints.
pub fn vertical_center(area: Rect, height: u16) -> Rect {
    let [rect] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    rect
}

/// Draw a centred single-line hint in the middle of `area`, for empty states.
pub fn centered_hint(frame: &mut Frame, area: Rect, text: &str, style: Style) {
    frame.render_widget(
        Paragraph::new(text)
            .style(style)
            .alignment(Alignment::Center),
        vertical_center(area, 1),
    );
}

/// A `width`×`height` rect centred within `area` (clamped to `area`), for modal
/// popups.
pub fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let row = vertical_center(area, height.min(area.height));
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}
