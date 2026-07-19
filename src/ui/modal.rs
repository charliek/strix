use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, Modal};
use crate::git::Change;
use crate::ui::theme::Theme;
use crate::ui::{centered_rect, char_width};

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
        Modal::CommentInput {
            buffer,
            cursor,
            editing,
            ..
        } => render_comment_input(frame, app, buffer, *cursor, editing.is_some()),
    }
}

/// The single-line comment editor: a centred popup with the input line (a block
/// cursor via reversed video), then a hint row. The buffer scrolls horizontally
/// so the cursor stays visible, unicode-width aware.
fn render_comment_input(frame: &mut Frame, app: &App, buffer: &str, cursor: usize, editing: bool) {
    let theme = &app.theme;
    let title = if editing {
        " Edit comment "
    } else {
        " Comment "
    };

    let area = centered_rect(frame.area(), 60, 5);
    frame.render_widget(Clear, area);
    let block = popup_block(title, theme.border_focused, theme.bg);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [input_area, _gap, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let input = input_line(buffer, cursor, input_area.width as usize, theme);
    frame.render_widget(Paragraph::new(input), input_area);

    let footer = Line::from(vec![
        Span::styled(" enter ", key_style(theme.footer_key)),
        Span::styled("save  ", Style::new().fg(theme.dim)),
        Span::styled(" esc ", key_style(theme.footer_key)),
        Span::styled("cancel", Style::new().fg(theme.dim)),
    ]);
    frame.render_widget(Paragraph::new(footer), footer_area);
}

/// Lay out the input buffer within `width` columns, scrolled so the cursor cell
/// (char index `cursor`, or a trailing blank when at the end) is visible, and
/// painting that cell with reversed video as the block cursor.
fn input_line(buffer: &str, cursor: usize, width: usize, theme: &Theme) -> Line<'static> {
    if width == 0 {
        return Line::from(String::new());
    }
    let chars: Vec<char> = buffer.chars().collect();
    let char_w = |i: usize| chars.get(i).map(|&c| char_width(c)).unwrap_or(1);

    // Scroll: walk left from the cursor, keeping its cell plus the run before it
    // within `width`, so the visible window always ends at (or after) the cursor.
    let cursor_w = if cursor < chars.len() {
        char_w(cursor)
    } else {
        1
    };
    let mut start = cursor;
    let mut used = cursor_w;
    while start > 0 {
        let w = char_w(start - 1);
        if used + w > width {
            break;
        }
        used += w;
        start -= 1;
    }

    let base = Style::new().fg(theme.fg).bg(theme.bg);
    let cursor_style = base.add_modifier(Modifier::REVERSED);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut col = 0;
    let mut i = start;
    while i < chars.len() {
        let w = char_w(i);
        if col + w > width {
            break;
        }
        let style = if i == cursor { cursor_style } else { base };
        spans.push(Span::styled(chars[i].to_string(), style));
        col += w;
        i += 1;
    }
    // Cursor at the end has no char under it: draw a reversed blank.
    if cursor >= chars.len() && col < width {
        spans.push(Span::styled(" ".to_string(), cursor_style));
        col += 1;
    }
    if col < width {
        spans.push(Span::styled(" ".repeat(width - col), base));
    }
    Line::from(spans)
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
        section("Review comments"),
        binding("] / [", "next / prev comment"),
        binding("c", "add / edit comment"),
        binding("x", "delete comment under cursor"),
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
