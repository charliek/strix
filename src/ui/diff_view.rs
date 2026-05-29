use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use syntect::parsing::SyntaxReference;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, DiffMode, Focus};
use crate::git::{DiffLine, FileDiff, LineKind};
use crate::ui::syntax::{highlight_line, syntax_for};
use crate::ui::theme::Theme;
use crate::ui::{centered_hint, panel_block};

/// Unified gutter: `oldd nnnn ` → 4 + 1 + 4 + 1.
const GUTTER_WIDTH: usize = 10;
/// Unified change-sign column: `+ ` / `- ` / `  `.
const SIGN_WIDTH: usize = 2;
/// Side-by-side per-column gutter: `nnnn ` → 4 + 1.
const SBS_GUTTER: usize = 5;

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
    app.set_diff_area(inner);

    let lines = match &app.current_diff {
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

    let path = selected.map(|(_, entry)| entry.path.as_str()).unwrap_or("");
    let syntax = syntax_for(path);
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
    let body_width = (inner.width as usize).saturating_sub(GUTTER_WIDTH);

    let rows: Vec<Line> = lines
        .iter()
        .skip(offset)
        .take(inner.height as usize)
        .map(|line| unified_line(line, theme, syntax, body_width))
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

fn unified_line(
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
    let gutter = format!("{} {} ", gutter_num(line.old_no), gutter_num(line.new_no));

    let mut spans = vec![
        Span::styled(gutter, Style::new().fg(theme.line_no)),
        Span::styled(sign, Style::new().fg(sign_fg).bg(bg)),
    ];
    spans.extend(highlighted_content(
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
    let rows = side_by_side_rows(lines);
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
        .map(|row| side_by_side_line(row, theme, syntax, left_w as usize, right_w as usize))
        .collect();
    frame.render_widget(Paragraph::new(out), inner);
}

/// A side-by-side row: a full-width hunk header, or an aligned old/new pair.
enum SbsRow<'a> {
    Hunk(&'a DiffLine),
    Pair {
        left: Option<&'a DiffLine>,
        right: Option<&'a DiffLine>,
    },
}

#[derive(Clone, Copy)]
enum Side {
    Old,
    New,
}

/// Pair the unified diff lines into side-by-side rows: context lines appear on
/// both sides; a run of deletions is zipped against the following run of
/// additions, padding the shorter side with blanks.
fn side_by_side_rows(lines: &[DiffLine]) -> Vec<SbsRow<'_>> {
    let mut rows = Vec::new();
    let mut deletions: Vec<&DiffLine> = Vec::new();
    let mut additions: Vec<&DiffLine> = Vec::new();

    for line in lines {
        match line.kind {
            LineKind::Deletion => deletions.push(line),
            LineKind::Addition => additions.push(line),
            LineKind::Context => {
                flush_pairs(&mut rows, &mut deletions, &mut additions);
                rows.push(SbsRow::Pair {
                    left: Some(line),
                    right: Some(line),
                });
            }
            LineKind::Hunk => {
                flush_pairs(&mut rows, &mut deletions, &mut additions);
                rows.push(SbsRow::Hunk(line));
            }
        }
    }
    flush_pairs(&mut rows, &mut deletions, &mut additions);
    rows
}

fn flush_pairs<'a>(
    rows: &mut Vec<SbsRow<'a>>,
    deletions: &mut Vec<&'a DiffLine>,
    additions: &mut Vec<&'a DiffLine>,
) {
    for i in 0..deletions.len().max(additions.len()) {
        rows.push(SbsRow::Pair {
            left: deletions.get(i).copied(),
            right: additions.get(i).copied(),
        });
    }
    deletions.clear();
    additions.clear();
}

fn side_by_side_line(
    row: &SbsRow,
    theme: &Theme,
    syntax: &SyntaxReference,
    left_w: usize,
    right_w: usize,
) -> Line<'static> {
    match row {
        SbsRow::Hunk(line) => hunk_line(line, theme),
        SbsRow::Pair { left, right } => {
            let mut spans = cell(*left, Side::Old, theme, syntax, left_w);
            spans.push(Span::styled("│", Style::new().fg(theme.border)));
            spans.extend(cell(*right, Side::New, theme, syntax, right_w));
            Line::from(spans)
        }
    }
}

fn cell(
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
    let gutter = format!("{} ", gutter_num(number));
    let mut spans = vec![Span::styled(gutter, Style::new().fg(theme.line_no))];
    spans.extend(highlighted_content(
        syntax,
        &theme.syntax_theme,
        &line.text,
        width.saturating_sub(SBS_GUTTER),
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
    syntax: &SyntaxReference,
    theme_name: &str,
    text: &str,
    width: usize,
    bg: Color,
) -> Vec<Span<'static>> {
    let clean = sanitize(text);
    let mut spans = Vec::new();
    let mut used = 0;
    for (color, piece) in highlight_line(syntax, theme_name, &clean) {
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
