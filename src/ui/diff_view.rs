use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use syntect::parsing::SyntaxReference;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, DiffMode, SbsRow};
use crate::git::{DiffLine, FileDiff, LineKind};
use crate::ui::syntax::syntax_for;
use crate::ui::theme::Theme;
use crate::ui::{centered_hint, panel_block};

/// Unified gutter: `oldd nnnn ` → 4 + 1 + 4 + 1.
const GUTTER_WIDTH: usize = 10;
/// Unified change-sign column: `+ ` / `- ` / `  `.
const SIGN_WIDTH: usize = 2;
/// Side-by-side per-column gutter: `nnnn ` → 4 + 1.
const SBS_GUTTER: usize = 5;

/// The unified gutter's width for the current `show_line_numbers` setting —
/// `GUTTER_WIDTH` when shown, `0` when hidden (the sign column is unaffected
/// and always renders). The single source of truth for both emitting the
/// gutter and sizing the remaining content width, so they can't drift.
fn unified_gutter_width(show_line_numbers: bool) -> usize {
    if show_line_numbers {
        GUTTER_WIDTH
    } else {
        0
    }
}

/// The side-by-side per-column gutter's width for the current
/// `show_line_numbers` setting — mirrors `unified_gutter_width`.
fn sbs_gutter_width(show_line_numbers: bool) -> usize {
    if show_line_numbers {
        SBS_GUTTER
    } else {
        0
    }
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.diff_focused();
    let path = app.active_diff_path();

    let title = match &path {
        Some(path) => format!(" Diff · {path} "),
        None => " Diff ".to_string(),
    };
    let block = panel_block(&title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.set_diff_area(inner);

    let lines = match app.active_diff() {
        Some(FileDiff::Text(lines)) if !lines.is_empty() => lines,
        other => {
            let message = match other {
                Some(FileDiff::Binary) => "Binary file — no preview",
                Some(_) => "No changes to show",
                None => "Select a file to view its diff",
            };
            app.set_diff_metrics(inner.height, 0);
            centered_hint(frame, inner, message, Style::new().fg(theme.dim));
            return;
        }
    };

    let syntax = syntax_for(path.as_deref().unwrap_or(""));
    match app.diff_mode {
        DiffMode::Unified => render_unified(frame, inner, app, lines, syntax),
        DiffMode::SideBySide => render_side_by_side(frame, inner, app, lines, syntax),
    }
}

fn render_unified(
    frame: &mut Frame,
    inner: Rect,
    app: &App,
    lines: &[DiffLine],
    syntax: &SyntaxReference,
) {
    app.set_diff_metrics(inner.height, clamp_u16(lines.len()));
    let theme = &app.theme;
    let offset = app.diff_scroll.min(app.diff_max_scroll()) as usize;
    let body_width =
        (inner.width as usize).saturating_sub(unified_gutter_width(app.show_line_numbers));

    let rows: Vec<Line> = lines
        .iter()
        .skip(offset)
        .take(inner.height as usize)
        .map(|line| unified_line(app, line, theme, syntax, body_width))
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

fn unified_line(
    app: &App,
    line: &DiffLine,
    theme: &Theme,
    syntax: &SyntaxReference,
    body_width: usize,
) -> Line<'static> {
    if line.kind == LineKind::Hunk {
        return hunk_line(line, theme);
    }

    let (sign, sign_fg, bg) = match line.kind {
        LineKind::Addition => ("+ ", theme.add, theme.add_bg),
        LineKind::Deletion => ("- ", theme.del, theme.del_bg),
        _ => ("  ", theme.fg, theme.bg),
    };

    let mut spans = Vec::new();
    if app.show_line_numbers {
        let gutter = format!("{} {} ", gutter_num(line.old_no), gutter_num(line.new_no));
        spans.push(Span::styled(gutter, Style::new().fg(theme.line_no)));
    }
    spans.push(Span::styled(sign, Style::new().fg(sign_fg).bg(bg)));
    spans.extend(highlighted_content(
        app,
        syntax,
        &theme.syntax_theme,
        &line.text,
        body_width.saturating_sub(SIGN_WIDTH),
        bg,
    ));
    Line::from(spans)
}

fn render_side_by_side(
    frame: &mut Frame,
    inner: Rect,
    app: &App,
    lines: &[DiffLine],
    syntax: &SyntaxReference,
) {
    let rows = app.sbs_rows(lines);
    app.set_diff_metrics(inner.height, clamp_u16(rows.len()));
    let theme = &app.theme;
    let offset = app.diff_scroll.min(app.diff_max_scroll()) as usize;
    // left column │ right column, the divider taking one cell.
    let left_w = inner.width.saturating_sub(1) / 2;
    let right_w = inner.width.saturating_sub(left_w + 1);

    let out: Vec<Line> = rows
        .iter()
        .skip(offset)
        .take(inner.height as usize)
        .map(|row| {
            side_by_side_line(
                app,
                row,
                lines,
                theme,
                syntax,
                left_w as usize,
                right_w as usize,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(out), inner);
}

#[derive(Clone, Copy)]
enum Side {
    Old,
    New,
}

fn side_by_side_line(
    app: &App,
    row: &SbsRow,
    lines: &[DiffLine],
    theme: &Theme,
    syntax: &SyntaxReference,
    left_w: usize,
    right_w: usize,
) -> Line<'static> {
    match row {
        SbsRow::Hunk(i) => hunk_line(&lines[*i], theme),
        SbsRow::Pair { left, right } => {
            let mut spans = cell(
                app,
                left.map(|i| &lines[i]),
                Side::Old,
                theme,
                syntax,
                left_w,
            );
            spans.push(Span::styled("│", Style::new().fg(theme.border)));
            spans.extend(cell(
                app,
                right.map(|i| &lines[i]),
                Side::New,
                theme,
                syntax,
                right_w,
            ));
            Line::from(spans)
        }
    }
}

fn cell(
    app: &App,
    line: Option<&DiffLine>,
    side: Side,
    theme: &Theme,
    syntax: &SyntaxReference,
    width: usize,
) -> Vec<Span<'static>> {
    let Some(line) = line else {
        return vec![Span::styled(" ".repeat(width), Style::new().bg(theme.bg))];
    };
    let (number, active_kind, active_bg) = match side {
        Side::Old => (line.old_no, LineKind::Deletion, theme.del_bg),
        Side::New => (line.new_no, LineKind::Addition, theme.add_bg),
    };
    let bg = if line.kind == active_kind {
        active_bg
    } else {
        theme.bg
    };
    let gutter_w = sbs_gutter_width(app.show_line_numbers);
    let mut spans = Vec::new();
    if app.show_line_numbers {
        let gutter = format!("{} ", gutter_num(number));
        spans.push(Span::styled(gutter, Style::new().fg(theme.line_no)));
    }
    spans.extend(highlighted_content(
        app,
        syntax,
        &theme.syntax_theme,
        &line.text,
        width.saturating_sub(gutter_w),
        bg,
    ));
    spans
}

fn hunk_line(line: &DiffLine, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        line.text.clone(),
        Style::new().fg(theme.hunk).add_modifier(Modifier::BOLD),
    ))
}

/// Syntax-highlight `text` into spans (each token's colour over the line's
/// background), expanding tabs, dropping control chars, and padding to `width`
/// so the background fills the row.
fn highlighted_content(
    app: &App,
    syntax: &SyntaxReference,
    theme_name: &str,
    text: &str,
    width: usize,
    bg: Color,
) -> Vec<Span<'static>> {
    let clean = sanitize(text);
    let mut spans = Vec::new();
    let mut used = 0;
    for (color, piece) in app.highlight(syntax, theme_name, &clean).iter() {
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
            spans.push(Span::styled(chunk, Style::new().fg(*color).bg(bg)));
        }
    }
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), Style::new().bg(bg)));
    }
    spans
}

/// Normalise a line for display in one pass: expand tabs and drop control
/// characters so file content can't inject terminal escape sequences.
fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn gutter_num(no: Option<usize>) -> String {
    match no {
        Some(n) => format!("{n:>4}"),
        None => "    ".to_string(),
    }
}

fn clamp_u16(value: usize) -> u16 {
    value.min(u16::MAX as usize) as u16
}
