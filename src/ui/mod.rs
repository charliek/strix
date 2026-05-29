pub mod diff_view;
pub mod modal;
pub mod staging;
pub mod syntax;
pub mod theme;

use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
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

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(body);
    staging::render(frame, left, app);
    diff_view::render(frame, right, app);

    render_footer(frame, footer, app);

    // Overlays draw last, on top of everything.
    modal::render(frame, app);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    // Fill the bar, then draw left + right text on top (transparent spans), so
    // the right-aligned branch never clobbers the left title.
    frame.render_widget(Block::new().style(Style::new().bg(theme.header_bg)), area);

    let left = Line::from(vec![
        Span::styled(
            " strix ",
            Style::new()
                .fg(theme.header_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.repo_name(), Style::new().fg(theme.dim)),
    ]);
    frame.render_widget(Paragraph::new(left), area);

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

    let mut spans = Vec::new();
    for (key, label) in [
        (" j/k ", "move  "),
        (" space ", "stage  "),
        (" d ", "split  "),
        (" ? ", "help  "),
        (" q ", "quit"),
    ] {
        spans.push(Span::styled(key, key_style));
        spans.push(Span::styled(label, label_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(theme.footer_bg)),
        area,
    );
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
