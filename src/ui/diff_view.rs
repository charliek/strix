use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Focus};
use crate::git::{DiffLine, FileDiff, LineKind};
use crate::ui::theme::Theme;
use crate::ui::{centered_hint, panel_block};

/// Width of the line-number gutter: `oldd nnnn ` → 4 + 1 + 4 + 1.
const GUTTER_WIDTH: usize = 10;
/// Width of the change sign column: `+ ` / `- ` / `  `.
const SIGN_WIDTH: usize = 2;

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

    let dim = Style::new().fg(theme.dim);
    match &app.current_diff {
        None => centered_hint(frame, inner, "Select a file to view its diff", dim),
        Some(FileDiff::Binary) => centered_hint(frame, inner, "Binary file — no preview", dim),
        Some(FileDiff::Text(lines)) if lines.is_empty() => {
            centered_hint(frame, inner, "No changes to show", dim)
        }
        Some(FileDiff::Text(lines)) => render_lines(frame, inner, app, lines),
    }
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
        LineKind::Addition => ("+ ", theme.add, theme.add_bg),
        LineKind::Deletion => ("- ", theme.del, theme.del_bg),
        _ => ("  ", theme.fg, theme.bg),
    };
    let gutter = format!("{} {} ", gutter_num(line.old_no), gutter_num(line.new_no));
    // The sign and content are separate spans so syntax highlighting (M4) can
    // replace the content span with per-token spans while keeping the same
    // background.
    let content = fit_to_width(&line.text, body_width.saturating_sub(SIGN_WIDTH));

    Line::from(vec![
        Span::styled(gutter, Style::new().fg(theme.line_no)),
        Span::styled(sign, Style::new().fg(fg).bg(bg)),
        Span::styled(content, Style::new().fg(fg).bg(bg)),
    ])
}

fn gutter_num(no: Option<usize>) -> String {
    match no {
        Some(n) => format!("{n:>4}"),
        None => "    ".to_string(),
    }
}

/// Expand tabs, drop control characters (so file content can't inject escape
/// sequences), truncate to `width` display columns, and pad to fill the width so
/// the line's background spans the full row.
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
