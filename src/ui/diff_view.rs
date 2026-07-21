use std::collections::HashMap;
use std::ops::Range;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use syntect::parsing::SyntaxReference;

use crate::app::{
    sbs_columns, App, BoxPart, BoxRow, EditorPart, LayoutRow, PairEmphasis, RowContent,
};
use crate::comments::Side;
use crate::git::{DiffLine, FileDiff, LineKind};
use crate::ui::syntax::syntax_for;
use crate::ui::theme::Theme;
use crate::ui::{centered_hint, char_width, panel_block};

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

    let label = app.active_diff_title();
    let title = match &path {
        Some(path) => format!(" {label} · {path} "),
        None => format!(" {label} "),
    };
    let block = panel_block(&title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.set_diff_area(inner);

    // The `[start, end)` physical rows of the cursor target to paint with the
    // selection background — `None` outside a cursor-bearing view or while its
    // file list is focused (plan §3.4). A comment box spans several rows.
    let cursor = app.review_cursor_highlight();

    // The physical layout drives both the row count (metrics) and rendering; a
    // code line is one row, a comment box several. Built for this pane width, so a
    // resize rewraps its boxes.
    let layout = app.diff_layout(inner.width);
    app.set_diff_metrics(inner.height, layout.len());

    // The diff lines backing the code rows; empty for a no-text diff (the layout
    // then holds only orphan boxes).
    let lines: &[DiffLine] = match app.active_diff() {
        Some(FileDiff::Text(lines)) => lines,
        _ => &[],
    };
    let is_text = !lines.is_empty();

    // No diff and no orphan boxes: the plain centered hint, as before. Clear any
    // `[x]` rects a previous frame recorded so a stale click can't hit them.
    if !is_text && layout.is_empty() {
        app.set_x_rects(HashMap::new());
        centered_hint(
            frame,
            inner,
            no_diff_message(app),
            Style::new().fg(theme.dim),
        );
        return;
    }

    let syntax = syntax_for(path.as_deref().unwrap_or(""));
    let (left_w, right_w) = sbs_columns(inner.width);
    let body_width =
        (inner.width as usize).saturating_sub(unified_gutter_width(app.show_line_numbers));
    let offset = app.diff_scroll.min(app.diff_max_scroll());

    let mut out: Vec<Line> = Vec::new();
    let mut x_rects: HashMap<u64, Rect> = HashMap::new();
    for (i, row) in layout
        .iter()
        .enumerate()
        .skip(offset)
        .take(inner.height as usize)
    {
        let screen_y = inner.y + (i - offset) as u16;
        let line = match &row.content {
            RowContent::Line(li) => unified_line(app, &lines[*li], theme, syntax, body_width),
            RowContent::Hunk(h) => hunk_line(&lines[*h], theme),
            RowContent::Pair {
                left,
                right,
                emphasis,
            } => sbs_pair_line(
                app,
                *left,
                *right,
                lines,
                theme,
                syntax,
                left_w,
                right_w,
                emphasis.as_ref(),
            ),
            RowContent::Box(boxed) => box_row_line(
                row,
                boxed,
                theme,
                inner,
                left_w,
                right_w,
                screen_y,
                &mut x_rects,
            ),
            RowContent::Editor(part) => editor_row_line(row, part, theme, inner, left_w, right_w),
        };
        let in_cursor = cursor.as_ref().is_some_and(|span| span.contains(&i));
        out.push(mark_cursor_row(line, in_cursor, theme));
    }
    app.set_x_rects(x_rects);

    // A no-text diff (binary / empty) still surfaces its orphan boxes; the hint
    // follows them when there's vertical room (finding 2), exactly as the old
    // orphan block did.
    if !is_text && out.len() < inner.height as usize {
        out.push(Line::from(Span::styled(
            no_diff_message(app).to_string(),
            Style::new().fg(theme.dim),
        )));
    }
    frame.render_widget(Paragraph::new(out), inner);
}

/// The empty-state hint for a diff with no text lines to show.
fn no_diff_message(app: &App) -> &'static str {
    match app.active_diff() {
        Some(FileDiff::Binary) => "Binary file — no preview",
        Some(_) => "No changes to show",
        None => "Select a file to view its diff",
    }
}

/// When `is_cursor`, repaint every span's background with the selection colour
/// to mark a diff cursor row, keeping each span's foreground (syntax / box
/// colours) and modifiers; otherwise return the line untouched. Every physical
/// row of the selected target (a whole comment box) is marked, so the box
/// highlights as one unit (plan §3.4).
fn mark_cursor_row(line: Line<'static>, is_cursor: bool, theme: &Theme) -> Line<'static> {
    if !is_cursor {
        return line;
    }
    let spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| Span::styled(span.content, span.style.bg(theme.selection_bg)))
        .collect();
    Line::from(spans)
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
        // Word-diff emphasis is a side-by-side feature only (plan §3.7).
        None,
    ));
    Line::from(spans)
}

/// One side-by-side code row: the old cell, the centre divider, the new cell.
/// `emphasis` carries the pair's word-diff changed-char ranges (plan §3.7),
/// `None` unless this is a genuinely modified pair.
#[allow(clippy::too_many_arguments)]
fn sbs_pair_line(
    app: &App,
    left: Option<usize>,
    right: Option<usize>,
    lines: &[DiffLine],
    theme: &Theme,
    syntax: &SyntaxReference,
    left_w: usize,
    right_w: usize,
    emphasis: Option<&PairEmphasis>,
) -> Line<'static> {
    let old_empty_bg = if right.map(|i| lines[i].kind) == Some(LineKind::Addition) {
        theme.add_gutter
    } else {
        theme.bg
    };
    let new_empty_bg = if left.map(|i| lines[i].kind) == Some(LineKind::Deletion) {
        theme.del_gutter
    } else {
        theme.bg
    };
    let mut spans = cell(
        app,
        left.map(|i| &lines[i]),
        Col::Old,
        theme,
        syntax,
        left_w,
        emphasis.map(|e| e.old_ranges.as_slice()),
        old_empty_bg,
    );
    spans.push(Span::styled("│", Style::new().fg(theme.border)));
    spans.extend(cell(
        app,
        right.map(|i| &lines[i]),
        Col::New,
        theme,
        syntax,
        right_w,
        emphasis.map(|e| e.new_ranges.as_slice()),
        new_empty_bg,
    ));
    Line::from(spans)
}

/// Which column a side-by-side cell renders (old / new).
#[derive(Clone, Copy)]
enum Col {
    Old,
    New,
}

#[allow(clippy::too_many_arguments)]
fn cell(
    app: &App,
    line: Option<&DiffLine>,
    side: Col,
    theme: &Theme,
    syntax: &SyntaxReference,
    width: usize,
    emph_ranges: Option<&[Range<usize>]>,
    empty_bg: Color,
) -> Vec<Span<'static>> {
    let Some(line) = line else {
        return vec![Span::styled(" ".repeat(width), Style::new().bg(empty_bg))];
    };
    let (number, active_kind, active_bg, emph_bg) = match side {
        Col::Old => (
            line.old_no,
            LineKind::Deletion,
            theme.del_bg,
            theme.del_emph,
        ),
        Col::New => (
            line.new_no,
            LineKind::Addition,
            theme.add_bg,
            theme.add_emph,
        ),
    };
    let active = line.kind == active_kind;
    let bg = if active { active_bg } else { theme.bg };
    // Emphasis only ever applies to this side's changed line (the other side of
    // a modified pair never carries this side's ranges); guard on `active`
    // defensively even though `App::pair_emphasis` only ever sets ranges for a
    // real deletion/addition pairing.
    let emphasis = emph_ranges
        .filter(|_| active)
        .map(|ranges| (ranges, emph_bg));
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
        emphasis,
    ));
    spans
}

fn hunk_line(line: &DiffLine, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        line.text.clone(),
        Style::new().fg(theme.hunk).add_modifier(Modifier::BOLD),
    ))
}

/// Render one physical row of a comment box (plan §3.4). Unified boxes span the
/// full pane; side-by-side boxes occupy their comment's anchor-side column while
/// the other column renders equal-height blanks. The title row's `[x]` cell rect
/// is recorded into `x_rects` for C8's click handling. A drifted (`stale`) note
/// renders dim.
#[allow(clippy::too_many_arguments)]
fn box_row_line(
    row: &LayoutRow,
    boxed: &BoxRow,
    theme: &Theme,
    inner: Rect,
    left_w: usize,
    right_w: usize,
    screen_y: u16,
    x_rects: &mut HashMap<u64, Rect>,
) -> Line<'static> {
    let (box_x_offset, box_w) = match row.side {
        None => (0usize, inner.width as usize),
        Some(Side::Old) => (0usize, left_w),
        Some(Side::New) => (left_w + 1, right_w),
    };
    let accent = if boxed.stale {
        theme.dim
    } else {
        theme.comment
    };
    let border = Style::new().fg(accent);
    let title_style = Style::new().fg(accent).add_modifier(Modifier::BOLD);
    let body_style = Style::new().fg(if boxed.stale { theme.dim } else { theme.fg });

    let (box_spans, close_col) = match &boxed.part {
        BoxPart::Title(text) => box_title_spans(text, box_w, border, title_style),
        BoxPart::Body(text) => (box_body_spans(text, box_w, border, body_style), None),
        BoxPart::Bottom => (box_bottom_spans(box_w, border), None),
    };
    if let Some(col) = close_col {
        x_rects.insert(
            boxed.id,
            Rect {
                x: inner.x + (box_x_offset + col) as u16,
                y: screen_y,
                width: 3,
                height: 1,
            },
        );
    }

    frame_side_columns(box_spans, row.side, left_w, right_w, theme)
}

/// Wrap a box's own spans into its side-by-side column, padding the other column
/// with a divider + equal-height blank; a full-width (`None`) box passes through.
/// Shared by the comment-box and editor renderers.
fn frame_side_columns(
    box_spans: Vec<Span<'static>>,
    side: Option<Side>,
    left_w: usize,
    right_w: usize,
    theme: &Theme,
) -> Line<'static> {
    let divider = || Span::styled("│", Style::new().fg(theme.border));
    let blank = |w: usize| Span::styled(" ".repeat(w), Style::new().bg(theme.bg));
    let spans = match side {
        None => box_spans,
        Some(Side::Old) => {
            let mut spans = box_spans;
            spans.push(divider());
            spans.push(blank(right_w));
            spans
        }
        Some(Side::New) => {
            let mut spans = vec![blank(left_w), divider()];
            spans.extend(box_spans);
            spans
        }
    };
    Line::from(spans)
}

/// The box top border with the title and a right-aligned `[x]`:
/// `╭─ ● you — <file> R<line> ────[x]╮`. The title is truncated to fit, but the
/// `[x]` close affordance always stays visible (plan §3.4). Returns the spans and
/// the column of `[` within the box, so the caller can record its click rect.
fn box_title_spans(
    title: &str,
    box_w: usize,
    border: Style,
    accent: Style,
) -> (Vec<Span<'static>>, Option<usize>) {
    if box_w == 0 {
        return (Vec::new(), None);
    }
    // Too narrow for corners + `[x]`: fill with border, no close cell.
    if box_w < 5 {
        return (vec![Span::styled("─".repeat(box_w), border)], None);
    }
    let close_col = box_w - 4;
    // Narrow: no room for a title, but keep the framed `[x]`.
    if box_w < 8 {
        let spans = vec![
            Span::styled("╭", border),
            Span::styled("─".repeat(box_w - 5), border),
            Span::styled("[x]", accent),
            Span::styled("╮", border),
        ];
        return (spans, Some(close_col));
    }
    // Normal: `╭─ ` + title + ` ` + fill + `[x]` + `╮` totals `box_w`.
    let (title_fit, title_w) = fit_display(title, box_w - 8);
    let fill = box_w - 8 - title_w;
    let mut spans = vec![Span::styled("╭─ ", border)];
    if !title_fit.is_empty() {
        spans.push(Span::styled(title_fit, accent));
    }
    spans.push(Span::styled(format!(" {}", "─".repeat(fill)), border));
    spans.push(Span::styled("[x]", accent));
    spans.push(Span::styled("╮", border));
    (spans, Some(close_col))
}

/// A box body row: `│ <text padded> │`. The text was word-wrapped to the box's
/// inner width at layout time; it's fitted again here defensively.
fn box_body_spans(text: &str, box_w: usize, border: Style, body: Style) -> Vec<Span<'static>> {
    if box_w == 0 {
        return Vec::new();
    }
    // Below `│ x │` (2 borders + 2 padding) there is no room for content; degrade
    // to a bare border column rather than underflow `box_w - 4`.
    if box_w < 4 {
        return vec![Span::styled("│".repeat(box_w), border)];
    }
    let content_w = box_w - 4;
    let (fit, fit_w) = fit_display(text, content_w);
    let mid = format!(" {fit}{} ", " ".repeat(content_w - fit_w));
    vec![
        Span::styled("│", border),
        Span::styled(mid, body),
        Span::styled("│", border),
    ]
}

/// The box bottom border: `╰────╯`.
fn box_bottom_spans(box_w: usize, border: Style) -> Vec<Span<'static>> {
    if box_w == 0 {
        return Vec::new();
    }
    if box_w < 2 {
        return vec![Span::styled("─".repeat(box_w), border)];
    }
    vec![
        Span::styled("╰", border),
        Span::styled("─".repeat(box_w - 2), border),
        Span::styled("╯", border),
    ]
}

/// Render one physical row of the in-place editor box (plan §3.5). Mirrors
/// `box_row_line`'s side-column placement, but the accent is the focus colour (an
/// active input) and the body draws a reversed caret cell — no `[x]`, since the
/// editor is dismissed with Esc/Enter, not a click.
fn editor_row_line(
    row: &LayoutRow,
    part: &EditorPart,
    theme: &Theme,
    inner: Rect,
    left_w: usize,
    right_w: usize,
) -> Line<'static> {
    let box_w = match row.side {
        None => inner.width as usize,
        Some(Side::Old) => left_w,
        Some(Side::New) => right_w,
    };
    let accent = theme.border_focused;
    let border = Style::new().fg(accent);
    let title_style = Style::new().fg(accent).add_modifier(Modifier::BOLD);
    let body_style = Style::new().fg(theme.fg);
    let caret_style = body_style.add_modifier(Modifier::REVERSED);

    let box_spans = match part {
        EditorPart::Title(text) => editor_title_spans(text, box_w, border, title_style),
        EditorPart::Body { text, caret } => {
            editor_body_spans(text, *caret, box_w, border, body_style, caret_style)
        }
        EditorPart::Bottom => box_bottom_spans(box_w, border),
    };

    frame_side_columns(box_spans, row.side, left_w, right_w, theme)
}

/// The editor box top border with its title: `╭─ ✎ you — <file> R<line> ────╮`,
/// truncated to fit. Like `box_title_spans` but without the `[x]` close cell.
fn editor_title_spans(
    title: &str,
    box_w: usize,
    border: Style,
    accent: Style,
) -> Vec<Span<'static>> {
    if box_w == 0 {
        return Vec::new();
    }
    if box_w < 2 {
        return vec![Span::styled("─".repeat(box_w), border)];
    }
    if box_w < 5 {
        // Corners + fill; too narrow for a title.
        return vec![
            Span::styled("╭", border),
            Span::styled("─".repeat(box_w - 2), border),
            Span::styled("╮", border),
        ];
    }
    // `╭─ ` (3) + title + ` ` (1) + fill + `╮` (1) totals box_w.
    let (fit, w) = fit_display(title, box_w - 5);
    let fill = box_w - 5 - w;
    let mut spans = vec![Span::styled("╭─ ", border)];
    if !fit.is_empty() {
        spans.push(Span::styled(fit, accent));
    }
    spans.push(Span::styled(format!(" {}", "─".repeat(fill)), border));
    spans.push(Span::styled("╮", border));
    spans
}

/// An editor body row: `│ <text with caret> │`. `text` is already wrapped to the
/// content width; the caret cell (at display column `caret`) is drawn reversed, and
/// a caret past the text end reverses a trailing blank.
fn editor_body_spans(
    text: &str,
    caret: Option<usize>,
    box_w: usize,
    border: Style,
    body: Style,
    caret_style: Style,
) -> Vec<Span<'static>> {
    if box_w == 0 {
        return Vec::new();
    }
    // Below `│ x │` there is no room for content; degrade to a bare border column.
    if box_w < 4 {
        return vec![Span::styled("│".repeat(box_w), border)];
    }
    let content_w = box_w - 4;
    let mut content: Vec<Span<'static>> = Vec::new();
    let mut col = 0;
    for ch in text.chars() {
        let w = char_width(ch);
        if col + w > content_w {
            break;
        }
        let style = if caret == Some(col) {
            caret_style
        } else {
            body
        };
        content.push(Span::styled(ch.to_string(), style));
        col += w;
    }
    // The caret sits past the drawn text (empty line, or end of a short line): pad
    // up to it, then a reversed blank so it's visible.
    if let Some(cc) = caret {
        if cc >= col && cc < content_w {
            if cc > col {
                content.push(Span::styled(" ".repeat(cc - col), body));
                col = cc;
            }
            content.push(Span::styled(" ".to_string(), caret_style));
            col += 1;
        }
    }
    if col < content_w {
        content.push(Span::styled(" ".repeat(content_w - col), body));
    }
    let mut spans = vec![Span::styled("│", border), Span::styled(" ", body)];
    spans.extend(content);
    spans.push(Span::styled(" ", body));
    spans.push(Span::styled("│", border));
    spans
}

/// Fit `s` into `max` display columns (unicode-width aware), returning the
/// fitted string and its exact width, appending `…` when it doesn't fit.
pub(crate) fn fit_display(s: &str, max: usize) -> (String, usize) {
    let total: usize = s.chars().map(char_width).sum();
    if total <= max {
        return (s.to_string(), total);
    }
    if max == 0 {
        return (String::new(), 0);
    }
    let budget = max - 1; // one column for the ellipsis
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = char_width(ch);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    used += 1;
    (out, used)
}

/// Syntax-highlight `text` into spans (each token's colour over the line's
/// background), expanding tabs, dropping control chars, and padding to `width`
/// so the background fills the row. `emphasis`, when given, is a side-by-side
/// modified pair's changed-char ranges over this same sanitized text plus the
/// emphasis background to use for them (plan §3.7): each char's background is
/// decided against the ranges (`char_bg`), intersecting the word-diff with the
/// syntax-token spans below without touching foreground colour.
///
/// Two non-obvious invariants:
/// - **Truncation stops the whole line, not just the current token.** The first
///   char that doesn't fit exits the outer token loop too (a labeled break), so
///   a later, differently-highlighted token can't render past the cut — which
///   would otherwise also desync `char_idx` from `emphasis`'s ranges (nothing
///   advances it over the skipped remainder), corrupting every emphasis lookup
///   for the rest of the line.
/// - **A zero-width char (a combining mark/ZWJ) never forces a span break.** A
///   terminal only combines it with the preceding base character when both are
///   written in the same span/string; splitting it into its own span stranded
///   it in a zero-width cell that got overwritten by the trailing padding,
///   silently dropping the change it renders (e.g. an accent in a modified
///   pair). It's glued onto the current chunk regardless of its own `char_bg`
///   — the chunk keeps the base character's background rather than switching
///   for just the mark — so a word-diff range landing only on the mark itself
///   is not visually distinguished; this is the documented fallback over
///   splitting the cluster.
fn highlighted_content(
    app: &App,
    syntax: &SyntaxReference,
    theme_name: &str,
    text: &str,
    width: usize,
    bg: Color,
    emphasis: Option<(&[Range<usize>], Color)>,
) -> Vec<Span<'static>> {
    let clean = sanitize(text);
    let mut spans = Vec::new();
    let mut used = 0; // display columns consumed, for width truncation/padding
    let mut char_idx = 0; // char offset into `clean`, aligned with `emphasis`'s ranges
    'tokens: for (color, piece) in app.highlight(syntax, theme_name, &clean).iter() {
        if used >= width {
            break;
        }
        let mut chunk = String::new();
        let mut chunk_bg = bg;
        for ch in piece.chars() {
            let ch_width = char_width(ch);
            if used + ch_width > width {
                if !chunk.is_empty() {
                    spans.push(Span::styled(chunk, Style::new().fg(*color).bg(chunk_bg)));
                }
                break 'tokens;
            }
            if ch_width == 0 && !chunk.is_empty() {
                chunk.push(ch);
                char_idx += 1;
                continue;
            }
            let ch_bg = char_bg(char_idx, emphasis, bg);
            if !chunk.is_empty() && ch_bg != chunk_bg {
                spans.push(Span::styled(
                    std::mem::take(&mut chunk),
                    Style::new().fg(*color).bg(chunk_bg),
                ));
            }
            chunk_bg = ch_bg;
            chunk.push(ch);
            used += ch_width;
            char_idx += 1;
        }
        if !chunk.is_empty() {
            spans.push(Span::styled(chunk, Style::new().fg(*color).bg(chunk_bg)));
        }
    }
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), Style::new().bg(bg)));
    }
    spans
}

/// The background for sanitized-text char `idx`: `emph_bg` when `idx` falls in
/// one of `emphasis`'s changed-char ranges, else the line's base `bg` (plan
/// §3.7). The per-char decision is what lets a changed span's emphasis
/// intersect with the syntax-highlighted token spans above (each token can
/// straddle an emphasis-range boundary and split into an emphasized/base run).
fn char_bg(idx: usize, emphasis: Option<(&[Range<usize>], Color)>, bg: Color) -> Color {
    match emphasis {
        Some((ranges, emph_bg)) if ranges.iter().any(|r| r.contains(&idx)) => emph_bg,
        _ => bg,
    }
}

/// Normalise a line for display in one pass: expand tabs and drop control
/// characters so file content can't inject terminal escape sequences. `pub(crate)`
/// so `App::pair_emphasis` (plan §3.7) can diff the exact same text the renderer
/// shows, keeping its changed-char-range offsets aligned with these tokens.
pub(crate) fn sanitize(text: &str) -> String {
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
