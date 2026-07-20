use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
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
        Modal::Help => render_help(frame, app),
    }
}

fn render_help(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let lines = help_lines(app);
    let height = lines.len() as u16 + 2;
    let area = centered_rect(frame.area(), 54, height);

    frame.render_widget(Clear, area);
    let block = popup_block(" Help ", theme.border_focused, theme.bg);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn help_lines(app: &App) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let section = |label: &str| {
        Line::from(Span::styled(
            format!(" {label}"),
            Style::new().fg(theme.title).add_modifier(Modifier::BOLD),
        ))
    };
    let binding = |keys: &str, desc: &str| {
        Line::from(vec![
            Span::styled(format!("   {keys:<12}"), key_style(theme.footer_key)),
            Span::styled(desc.to_string(), Style::new().fg(theme.fg)),
        ])
    };
    vec![
        section("Navigation"),
        binding("j / k", "move selection / scroll diff"),
        binding("g / G", "top / bottom"),
        binding("Tab", "switch pane"),
        binding("h / l", "focus staging / diff"),
        binding("Ctrl-d / u", "scroll diff half page"),
        Line::raw(""),
        section("Staging"),
        binding("space", "stage / unstage selected"),
        binding("s / u", "stage / unstage"),
        binding("x", "discard changes (confirm)"),
        binding("r", "refresh"),
        Line::raw(""),
        section("Comments"),
        binding("c", "add / edit comment (or dbl-click)"),
        binding("X", "delete comment (or click [x])"),
        binding("] / [", "next / prev comment"),
        binding("enter / esc", "save / cancel (while editing)"),
        binding("shift+enter", "newline (also ctrl-j, alt+enter)"),
        Line::raw(""),
        section("View"),
        binding("d", "toggle side-by-side"),
        binding("n", "toggle line numbers"),
        binding("t", "cycle theme"),
        binding("b", "show / hide changes panel"),
        binding("i", "toggle history"),
        binding("1 / 2", "home / history"),
        Line::raw(""),
        binding("any key", "close help"),
        binding("q", "quit"),
    ]
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

    let block = popup_block(title, theme.del, theme.bg);

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

fn key_style(fg: Color) -> Style {
    Style::new().fg(fg).add_modifier(Modifier::BOLD)
}

/// A rounded popup block with a solid background and a coloured border + title.
fn popup_block(title: &str, border: Color, bg: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Span::styled(title.to_string(), key_style(border)))
        .style(Style::new().bg(bg))
}
