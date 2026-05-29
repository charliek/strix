use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Focus};
use crate::git::{DiffLine, FileDiff, LineKind};
use crate::ui::theme::Theme;
use crate::ui::{panel_block, vertical_center};

/// Width of the line-number gutter: `oldd nnnn ` → 4 + 1 + 4 + 1.
const GUTTER_WIDTH: usize = 10;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.focus == Focus::Diff;

    let title = match app.selected_file() {
        Some((_, entry)) => format!(" Diff · {} ", entry.path),
        None => " Diff ".to_string(),
    };
    let block = panel_block(&title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.set_diff_viewport(inner.height);

    match &app.current_diff {
        None => hint(frame, inner, theme, "Select a file to view its diff"),
        Some(FileDiff::Binary) => hint(frame, inner, theme, "Binary file — no preview"),
        Some(FileDiff::Text(lines)) if lines.is_empty() => {
            hint(frame, inner, theme, "No changes to show")
        }
        Some(FileDiff::Text(lines)) => render_lines(frame, inner, app, lines),
    }
}

fn hint(frame: &mut Frame, inner: Rect, theme: &Theme, text: &str) {
    let hint = Paragraph::new(text)
        .style(Style::new().fg(theme.dim))
        .alignment(Alignment::Center);
    frame.render_widget(hint, vertical_center(inner, 1));
}

fn render_lines(frame: &mut Frame, inner: Rect, app: &App, lines: &[DiffLine]) {
    let theme = &app.theme;
    let offset = app.diff_scroll.min(app.diff_max_scroll()) as usize;
    let body_width = (inner.width as usize).saturating_sub(GUTTER_WIDTH);

    let rows: Vec<Line> = lines
        .iter()
        .skip(offset)
        .take(inner.height as usize)
        .map(|line| render_line(line, theme, body_width))
        .collect();

    frame.render_widget(Paragraph::new(rows), inner);
}

fn render_line(line: &DiffLine, theme: &Theme, body_width: usize) -> Line<'static> {
    if line.kind == LineKind::Hunk {
        return Line::from(Span::styled(
            line.text.clone(),
            Style::new().fg(theme.hunk).add_modifier(Modifier::BOLD),
        ));
    }

    let (sign, fg, bg) = match line.kind {
        LineKind::Addition => ('+', theme.add, theme.add_bg),
        LineKind::Deletion => ('-', theme.del, theme.del_bg),
        _ => (' ', theme.fg, theme.bg),
    };
    let gutter = format!("{} {} ", gutter_num(line.old_no), gutter_num(line.new_no));
    let body = fit_to_width(&format!("{sign} {}", line.text), body_width);

    Line::from(vec![
        Span::styled(gutter, Style::new().fg(theme.line_no)),
        Span::styled(body, Style::new().fg(fg).bg(bg)),
    ])
}

fn gutter_num(no: Option<usize>) -> String {
    match no {
        Some(n) => format!("{n:>4}"),
        None => "    ".to_string(),
    }
}

/// Expand tabs, drop control characters (so file content can't inject escape
/// sequences), truncate to `width` display columns, and pad to fill the row so
/// the line's background spans the full width.
fn fit_to_width(text: &str, width: usize) -> String {
    let expanded = text.replace('\t', "    ");
    let mut out = String::with_capacity(width);
    let mut used = 0;
    for ch in expanded.chars() {
        if ch.is_control() {
            continue;
        }
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    for _ in used..width {
        out.push(' ');
    }
    out
}
