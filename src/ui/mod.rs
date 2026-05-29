pub mod diff_view;
pub mod staging;
pub mod theme;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;

/// Top-level render: header / body / footer, with the body split into the
/// staging pane (left) and diff pane (right).
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let theme = &app.theme;

    frame.render_widget(
        Block::new().style(Style::new().bg(theme.bg).fg(theme.fg)),
        area,
    );

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);

    render_header(frame, rows[0], app);

    let cols =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).split(rows[1]);
    staging::render(frame, cols[0], app);
    diff_view::render(frame, cols[1], app);

    render_footer(frame, rows[2], app);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let line = Line::from(vec![
        Span::styled(
            " strix ",
            Style::new()
                .fg(theme.header_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.repo_name(), Style::new().fg(theme.dim)),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::new().bg(theme.header_bg)),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut spans = Vec::new();
    for (key, label) in [("q", "quit"), ("Tab", "switch pane")] {
        spans.push(Span::styled(
            format!(" {key} "),
            Style::new()
                .fg(theme.footer_key)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{label}  "),
            Style::new().fg(theme.footer_fg),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(theme.footer_bg)),
        area,
    );
}

/// A bordered panel with focus-aware border + title colours. Shared by the
/// staging and diff panes so focus styling stays consistent.
pub fn panel_block<'a>(title: &'a str, focused: bool, app: &App) -> Block<'a> {
    let theme = &app.theme;
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

/// A one-line `Rect` vertically centred within `area`, for empty-state hints.
pub fn centered_line(area: Rect) -> Rect {
    if area.height == 0 {
        return area;
    }
    Rect {
        x: area.x,
        y: area.y + area.height / 2,
        width: area.width,
        height: 1,
    }
}
