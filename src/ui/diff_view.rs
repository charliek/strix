use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use syntect::parsing::SyntaxReference;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Focus};
use crate::git::{DiffLine, FileDiff, LineKind};
use crate::ui::syntax::{highlight_line, syntax_for};
use crate::ui::theme::Theme;
use crate::ui::{centered_hint, panel_block};

/// Width of the line-number gutter: `oldd nnnn ` → 4 + 1 + 4 + 1.
const GUTTER_WIDTH: usize = 10;
/// Width of the change-sign column: `+ ` / `- ` / `  `.
const SIGN_WIDTH: usize = 2;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.focus == Focus::Diff;
    let selected = app.selected_file();

    let title = match selected {
        Some((_, entry)) => format!(" Diff · {} ", entry.path),
        None => " Diff ".to_string(),
    };
    let block = panel_block(&title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.set_diff_viewport(inner.height);

    let dim = Style::new().fg(theme.dim);
    match &app.current_diff {
        None => centered_hint(frame, inner, "Select a file to view its diff", dim),
        Some(FileDiff::Binary) => centered_hint(frame, inner, "Binary file — no preview", dim),
        Some(FileDiff::Text(lines)) if lines.is_empty() => {
            centered_hint(frame, inner, "No changes to show", dim)
        }
        Some(FileDiff::Text(lines)) => {
            let path = selected.map(|(_, entry)| entry.path.as_str()).unwrap_or("");
            render_lines(frame, inner, app, lines, syntax_for(path));
        }
    }
}

fn render_lines(
    frame: &mut Frame,
    inner: Rect,
    app: &App,
    lines: &[DiffLine],
    syntax: &SyntaxReference,
) {
    let theme = &app.theme;
    let offset = app.diff_scroll.min(app.diff_max_scroll()) as usize;
    let body_width = (inner.width as usize).saturating_sub(GUTTER_WIDTH);

    let rows: Vec<Line> = lines
        .iter()
        .skip(offset)
        .take(inner.height as usize)
        .map(|line| render_line(line, theme, syntax, body_width))
        .collect();

    frame.render_widget(Paragraph::new(rows), inner);
}

fn render_line(
    line: &DiffLine,
    theme: &Theme,
    syntax: &SyntaxReference,
    body_width: usize,
) -> Line<'static> {
    if line.kind == LineKind::Hunk {
        return Line::from(Span::styled(
            line.text.clone(),
            Style::new().fg(theme.hunk).add_modifier(Modifier::BOLD),
        ));
    }

    let (sign, sign_fg, bg) = match line.kind {
        LineKind::Addition => ("+ ", theme.add, theme.add_bg),
        LineKind::Deletion => ("- ", theme.del, theme.del_bg),
        _ => ("  ", theme.fg, theme.bg),
    };
    let gutter = format!("{} {} ", gutter_num(line.old_no), gutter_num(line.new_no));

    let mut spans = vec![
        Span::styled(gutter, Style::new().fg(theme.line_no)),
        Span::styled(sign, Style::new().fg(sign_fg).bg(bg)),
    ];
    spans.extend(highlighted_content(
        syntax,
        &line.text,
        body_width.saturating_sub(SIGN_WIDTH),
        bg,
    ));
    Line::from(spans)
}

/// Syntax-highlight `text` into spans (each token's colour over the line's
/// background), expanding tabs, dropping control chars, and padding to `width`
/// so the background fills the row.
fn highlighted_content(
    syntax: &SyntaxReference,
    text: &str,
    width: usize,
    bg: Color,
) -> Vec<Span<'static>> {
    let clean = sanitize(text);
    let mut spans = Vec::new();
    let mut used = 0;
    for (color, piece) in highlight_line(syntax, &clean) {
        if used >= width {
            break;
        }
        let mut chunk = String::new();
        for ch in piece.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + ch_width > width {
                break;
            }
            chunk.push(ch);
            used += ch_width;
        }
        if !chunk.is_empty() {
            spans.push(Span::styled(chunk, Style::new().fg(color).bg(bg)));
        }
    }
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), Style::new().bg(bg)));
    }
    spans
}

fn sanitize(text: &str) -> String {
    text.replace('\t', "    ")
        .chars()
        .filter(|ch| !ch.is_control())
        .collect()
}

fn gutter_num(no: Option<usize>) -> String {
    match no {
        Some(n) => format!("{n:>4}"),
        None => "    ".to_string(),
    }
}
