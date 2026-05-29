use ratatui::layout::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, Modal};
use crate::git::Change;
use crate::ui::centered_rect;

/// Draw the active modal as a centred popup over the rest of the UI. A no-op
/// when no modal is open.
pub fn render(frame: &mut Frame, app: &App) {
    let Some(modal) = &app.modal else {
        return;
    };
    match modal {
        Modal::ConfirmDiscard { change, label, .. } => {
            render_confirm_discard(frame, app, *change, label)
        }
    }
}

fn render_confirm_discard(frame: &mut Frame, app: &App, change: Change, label: &str) {
    let theme = &app.theme;
    let (title, question) = if change == Change::Untracked {
        (" Delete file ", "Delete this untracked file?")
    } else {
        (" Discard changes ", "Discard all changes to this file?")
    };

    let area = centered_rect(frame.area(), 64, 7);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.del))
        .title(Span::styled(
            title,
            Style::new().fg(theme.del).add_modifier(Modifier::BOLD),
        ))
        .style(Style::new().bg(theme.bg));

    let lines = vec![
        Line::from(Span::styled(question, Style::new().fg(theme.fg))),
        Line::from(Span::styled(
            label.to_string(),
            Style::new().fg(theme.title),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" y ", key_style(theme.del)),
            Span::styled("confirm    ", Style::new().fg(theme.dim)),
            Span::styled(" n ", key_style(theme.footer_key)),
            Span::styled("cancel", Style::new().fg(theme.dim)),
        ]),
    ];

    let paragraph = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
}

fn key_style(fg: ratatui::style::Color) -> Style {
    Style::new().fg(fg).add_modifier(Modifier::BOLD)
}
