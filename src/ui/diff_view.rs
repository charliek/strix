use std::collections::HashMap;
use std::ops::Range;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use syntect::parsing::SyntaxReference;

use crate::app::{
    sbs_columns, App, BoxPart, BoxRow, EditorPart, LayoutRow, PairCell, PairEmphasis, RowContent,
    Seg,
};
use crate::comments::Side;
use crate::git::{DiffLine, FileDiff, LineKind};
use crate::ui::syntax::syntax_for;
use crate::ui::theme::Theme;
use crate::ui::{centered_hint, char_width, panel_block};

/// The minimum width of one line-number column (`nnnn`), so a ≤9999-line file
/// renders the classic 4-digit gutter unchanged; wider files widen it per-diff.
const NUM_MIN_WIDTH: usize = 4;
/// Unified change-sign column: `+ ` / `- ` / `  `.
const SIGN_WIDTH: usize = 2;

/// The decimal-digit width the line-number columns need for this diff: at least
/// [`NUM_MIN_WIDTH`] (so ≤9999-line files render exactly as before), widened to
/// fit the largest old/new line number. A fixed 4-wide column silently clips a
/// 5+-digit number, and under wrap that clip shortens the gutter and pushes every
/// wrapped segment out of alignment (losing content); sizing the gutter to the
/// actual numbers keeps the reserved and drawn widths equal. The single source of
/// truth read by both the layout's wrap width and the renderer's widths.
pub(crate) fn line_number_width(lines: &[DiffLine]) -> usize {
    let widest = lines
        .iter()
        .flat_map(|l| [l.old_no, l.new_no])
        .flatten()
        .max()
        .map_or(1, |n| n.checked_ilog10().unwrap_or(0) as usize + 1);
    widest.max(NUM_MIN_WIDTH)
}

/// The unified gutter's width for the current `show_line_numbers` setting and
/// per-diff `number_width`: two number columns plus their two trailing spaces
/// (`nnnn nnnn `) when shown, `0` when hidden (the sign column is unaffected and
/// always renders). The single source of truth for both emitting the gutter and
/// sizing the remaining content width, so they can't drift.
fn unified_gutter_width(show_line_numbers: bool, number_width: usize) -> usize {
    if show_line_numbers {
        2 * number_width + 2
    } else {
        0
    }
}

/// The side-by-side per-column gutter's width for the current `show_line_numbers`
/// setting and per-diff `number_width` (`nnnn ` → `number_width + 1`) — mirrors
/// `unified_gutter_width`, and carries the same 5-digit fix so both columns
/// reserve exactly what `gutter_num` draws.
fn sbs_gutter_width(show_line_numbers: bool, number_width: usize) -> usize {
    if show_line_numbers {
        number_width + 1
    } else {
        0
    }
}

/// The display-column width available for one side-by-side column's *content*:
/// the column minus its (toggleable) per-column line-number gutter. The single
/// source of truth for both a column's wrap width and its render width (plan §3.3).
pub(crate) fn sbs_content_width(
    column_width: usize,
    show_line_numbers: bool,
    number_width: usize,
) -> usize {
    column_width.saturating_sub(sbs_gutter_width(show_line_numbers, number_width))
}

/// The display-column width available for a unified line's *content* at pane
/// width `pane_width`: the pane minus the (toggleable) line-number gutter minus
/// the always-present change-sign column. `number_width` is this diff's per-diff
/// line-number column width ([`line_number_width`]). The single source of truth
/// for both the layout's wrap width and the renderer's content width, so a wrapped
/// segment always fits exactly where it is drawn (plan §3.3).
pub(crate) fn unified_content_width(
    pane_width: usize,
    show_line_numbers: bool,
    number_width: usize,
) -> usize {
    pane_width
        .saturating_sub(unified_gutter_width(show_line_numbers, number_width))
        .saturating_sub(SIGN_WIDTH)
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
    // Per-diff number-column width — matches what the layout builder wrapped at
    // (both derive it from the same lines), so gutter and content never drift.
    let number_width = line_number_width(lines);
    let content_width =
        unified_content_width(inner.width as usize, app.show_line_numbers, number_width);
    // Horizontal offset for code content only, clamped to the longest code line at
    // read time and always 0 while wrap is on (plan §3.5).
    let hskip = app.effective_hscroll();
    let offset = app.diff_scroll.get().min(app.diff_max_scroll());

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
            RowContent::Line { line: li, seg } => unified_line(
                app,
                &lines[*li],
                theme,
                syntax,
                content_width,
                number_width,
                *seg,
                row.subrow == 0,
                hskip,
            ),
            RowContent::Hunk(h) => hunk_line(&lines[*h], theme),
            RowContent::Pair {
                left,
                right,
                emphasis,
            } => sbs_pair_line(
                app,
                left.as_ref(),
                right.as_ref(),
                lines,
                theme,
                syntax,
                left_w,
                right_w,
                number_width,
                row.subrow == 0,
                hskip,
                emphasis.as_deref(),
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

/// One unified display row: the gutter + sign + the segment of the line's
/// content this row covers. `seg` is the char window (the whole line with wrap
/// off, one wrapped slice with it on); `first` is true only for a line's top
/// subrow. On continuation rows the gutter and sign columns render as equal-width
/// blanks styled like a row-zero gutter — gutter styling, never the line's
/// add/del background (plan §3.3) — while the content keeps the line background.
#[allow(clippy::too_many_arguments)]
fn unified_line(
    app: &App,
    line: &DiffLine,
    theme: &Theme,
    syntax: &SyntaxReference,
    content_width: usize,
    number_width: usize,
    seg: Seg,
    first: bool,
    hskip: usize,
) -> Line<'static> {
    if line.kind == LineKind::Hunk {
        return hunk_line(line, theme);
    }

    let (sign, sign_fg, bg) = match line.kind {
        LineKind::Addition => ("+ ", theme.add, theme.add_bg),
        LineKind::Deletion => ("- ", theme.del, theme.del_bg),
        _ => ("  ", theme.fg, theme.bg),
    };

    let gutter_style = Style::new().fg(theme.line_no);
    let mut spans = Vec::new();
    if first {
        if app.show_line_numbers {
            let gutter = format!(
                "{} {} ",
                gutter_num(line.old_no, number_width),
                gutter_num(line.new_no, number_width)
            );
            spans.push(Span::styled(gutter, gutter_style));
        }
        spans.push(Span::styled(sign, Style::new().fg(sign_fg).bg(bg)));
    } else {
        // Continuation row: blank gutter + blank sign, gutter-styled (no line bg).
        // The gutter blank must match the row-zero gutter's dynamic width exactly,
        // or wrapped content shifts on continuation rows.
        if app.show_line_numbers {
            let width = unified_gutter_width(true, number_width);
            spans.push(Span::styled(" ".repeat(width), gutter_style));
        }
        spans.push(Span::styled(" ".repeat(SIGN_WIDTH), gutter_style));
    }
    let highlighted = app.highlight(syntax, &theme.syntax_theme, &sanitize(&line.text));
    spans.extend(slice_spans(
        &highlighted,
        seg,
        // Horizontal scroll shifts the code content only (plan §3.5); the gutter
        // and sign above are emitted unshifted.
        hskip,
        content_width,
        bg,
        // Word-diff emphasis is a side-by-side feature only (plan §3.7).
        None,
    ));
    Line::from(spans)
}

/// One side-by-side display row: the old cell, the centre divider, the new cell.
/// Each side is a [`PairCell`] (or `None` for an absent side → `filler_bg`);
/// `first` is true only on the pair's top subrow, gating the gutter numbers.
/// `emphasis` carries the pair's word-diff changed-char ranges (plan §3.7),
/// absolute over each side's sanitized text, so every subrow's [`Seg`] windows
/// them correctly.
#[allow(clippy::too_many_arguments)]
fn sbs_pair_line(
    app: &App,
    left: Option<&PairCell>,
    right: Option<&PairCell>,
    lines: &[DiffLine],
    theme: &Theme,
    syntax: &SyntaxReference,
    left_w: usize,
    right_w: usize,
    number_width: usize,
    first: bool,
    hskip: usize,
    emphasis: Option<&PairEmphasis>,
) -> Line<'static> {
    // Both cells shift by the same horizontal offset; the centre divider between
    // them never moves (plan §3.5).
    let mut spans = cell(
        app,
        left,
        lines,
        Col::Old,
        theme,
        syntax,
        left_w,
        number_width,
        first,
        hskip,
        emphasis.map(|e| e.old_ranges.as_slice()),
    );
    spans.push(Span::styled("│", Style::new().fg(theme.border)));
    spans.extend(cell(
        app,
        right,
        lines,
        Col::New,
        theme,
        syntax,
        right_w,
        number_width,
        first,
        hskip,
        emphasis.map(|e| e.new_ranges.as_slice()),
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
    cell: Option<&PairCell>,
    lines: &[DiffLine],
    side: Col,
    theme: &Theme,
    syntax: &SyntaxReference,
    width: usize,
    number_width: usize,
    first: bool,
    hskip: usize,
    emph_ranges: Option<&[Range<usize>]>,
) -> Vec<Span<'static>> {
    // No line on this side at all: the whole column is neutral filler.
    let Some(cell) = cell else {
        return vec![Span::styled(
            " ".repeat(width),
            Style::new().bg(theme.filler_bg),
        )];
    };
    let line = &lines[cell.line];
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
    let gutter_w = sbs_gutter_width(app.show_line_numbers, number_width);
    let content_w = width.saturating_sub(gutter_w);
    let gutter_style = Style::new().fg(theme.line_no);
    let mut spans = Vec::new();
    if app.show_line_numbers {
        // The number shows on the top subrow only; continuation subrows draw a
        // blank gutter, gutter-styled — never the line's add/del background. In a
        // pane so narrow the gutter is wider than the cell, clip it to the cell
        // (gutter_w + content_w always sums to `width`), so a partially-visible
        // gutter can't overrun and push the centre divider off `left_w`.
        let emit = gutter_w.min(width);
        let text = if first {
            format!("{} ", gutter_num(number, number_width))
        } else {
            " ".repeat(gutter_w)
        };
        let clipped: String = text.chars().take(emit).collect();
        spans.push(Span::styled(clipped, gutter_style));
    }
    match cell.seg {
        // A subrow this line reaches: its char window, syntax-highlighted with
        // any word-diff emphasis (only ever on this side's changed line).
        Some(seg) => {
            let emphasis = emph_ranges
                .filter(|_| active)
                .map(|ranges| (ranges, emph_bg));
            let highlighted = app.highlight(syntax, &theme.syntax_theme, &sanitize(&line.text));
            spans.extend(slice_spans(
                &highlighted,
                seg,
                hskip,
                content_w,
                bg,
                emphasis,
            ));
        }
        // The line ran out of segments before the taller side did: blank content
        // in *this line's* own background (the two-blank distinction, plan §3.3).
        None => spans.push(Span::styled(" ".repeat(content_w), Style::new().bg(bg))),
    }
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

/// Split sanitized `text` into display-column-hard-wrapped segments, each a
/// `[start_char, end_char)` window into the same text, at content width `width`
/// (plan §3.2). Guarantees:
/// - **Progress**: `width` is floored to 1, and every segment holds ≥1 char, so
///   a pathological width can't loop or emit unbounded rows.
/// - **Wide chars stay whole**: a double-width char that would overflow the
///   current segment moves wholly to the next one (never split across a boundary).
/// - **Combining marks stay glued**: a zero-width char never *starts* a segment —
///   it is folded onto the preceding base char's segment, matching how the
///   renderer glues it (a boundary landing on a bare mark would strand it).
///
/// An empty line yields one empty segment `[0, 0)` so it still renders a blank row.
pub(crate) fn wrap_segments(text: &str, width: usize) -> Vec<Seg> {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return vec![Seg::full(0)];
    }
    let mut segs = Vec::new();
    let mut start = 0;
    let mut col = 0;
    let mut i = 0;
    while i < n {
        let w = char_width(chars[i]);
        // Would this char overflow the current segment? Close it first — but only
        // if the segment already holds a char, so a lone char wider than `width`
        // still makes progress (the ≥1-char floor).
        if col + w > width && i > start {
            segs.push(Seg {
                start_char: start,
                end_char: i,
            });
            start = i;
            col = 0;
        }
        col += w;
        i += 1;
        // Fold any trailing zero-width chars (combining marks/ZWJ) into this
        // segment so a boundary never lands on a bare mark.
        while i < n && char_width(chars[i]) == 0 {
            i += 1;
        }
    }
    segs.push(Seg {
        start_char: start,
        end_char: n,
    });
    segs
}

/// Emit the spans for one display row: the highlighted spans of a full line
/// windowed to the char range `seg`, after skipping `col_skip` further display
/// columns (horizontal scroll — C6; pass 0 to disable), fitted to `width`
/// display columns and padded so the background fills the row (plan §3.2).
///
/// `highlighted`'s chunks concatenate to the line's sanitized text, so `char_idx`
/// counts absolute sanitized chars — the same coordinate as `seg` and `emphasis`,
/// which is why emphasis ranges are never remapped. Pinned guarantees:
/// - A zero-width char glues onto the current chunk (never its own span), so a
///   combining mark renders in the same cell as its base.
/// - A wide char that would overflow `width` at the trailing edge is dropped and
///   the row padded to `width` (matching the pre-wrap truncation policy); a wide
///   char straddling the `col_skip` leading edge emits background pad for its
///   visible overhang, so a horizontal offset lands deterministically.
/// - Passing the caller's clean+highlight, the whole-line segment and `col_skip`
///   0 reproduces the old truncating renderer exactly.
fn slice_spans(
    highlighted: &[(Color, String)],
    seg: Seg,
    col_skip: usize,
    width: usize,
    bg: Color,
    emphasis: Option<(&[Range<usize>], Color)>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut used = 0; // display columns emitted into the visible window
    let mut skipped = 0; // display columns consumed by the `col_skip` lead-in
    let mut char_idx = 0; // absolute sanitized-char offset, aligned with `emphasis`
    'tokens: for (color, piece) in highlighted.iter() {
        if used >= width {
            break;
        }
        let mut chunk = String::new();
        let mut chunk_bg = bg;
        for ch in piece.chars() {
            // Before the segment window: swallow chars without emitting.
            if char_idx < seg.start_char {
                char_idx += 1;
                continue;
            }
            // Past the segment window: stop the whole line so a later, differently-
            // highlighted token can't render beyond the segment (and desync
            // `char_idx` from `emphasis`).
            if char_idx >= seg.end_char {
                if !chunk.is_empty() {
                    spans.push(Span::styled(chunk, Style::new().fg(*color).bg(chunk_bg)));
                }
                break 'tokens;
            }
            let ch_width = char_width(ch);
            // A zero-width char (combining mark / ZWJ) only renders glued to a base
            // in the same chunk. With a non-empty chunk it glues on, keeping the
            // cluster in one cell. With an *empty* chunk its base is off-window —
            // skipped by `col_skip`, or before the segment, or a truncated tail — so
            // it is dropped rather than stranded at the leading edge or on the gutter
            // (FIX 2). `char_idx` still advances so emphasis stays aligned.
            if ch_width == 0 {
                if !chunk.is_empty() {
                    chunk.push(ch);
                }
                char_idx += 1;
                continue;
            }
            // Horizontal lead-in: skip whole display columns before the visible
            // start. A wide char straddling the boundary emits background pad for
            // the columns that peek past it.
            if skipped < col_skip {
                let remaining = col_skip - skipped;
                if ch_width <= remaining {
                    skipped += ch_width;
                    char_idx += 1;
                    continue;
                }
                skipped = col_skip;
                let overhang = (ch_width - remaining).min(width - used);
                if overhang > 0 {
                    // If the cut char is emphasized, its overhang pad carries the
                    // emphasis background so a changed wide char keeps its highlight
                    // at the window edge (FIX 3).
                    let pad_bg = char_bg(char_idx, emphasis, bg);
                    spans.push(Span::styled(" ".repeat(overhang), Style::new().bg(pad_bg)));
                    used += overhang;
                }
                char_idx += 1;
                continue;
            }
            // A wide char that no longer fits ends the line (padded below).
            if used + ch_width > width {
                if !chunk.is_empty() {
                    spans.push(Span::styled(chunk, Style::new().fg(*color).bg(chunk_bg)));
                }
                break 'tokens;
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

/// One line-number column, right-aligned to this diff's `width`
/// ([`line_number_width`]) so a present number and a blank sibling occupy the
/// same span — the reserved gutter width and the drawn glyphs stay equal.
fn gutter_num(no: Option<usize>, width: usize) -> String {
    match no {
        Some(n) => format!("{n:>width$}"),
        None => " ".repeat(width),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The visible glyphs a run of spans emits, with the trailing background pad
    /// stripped, plus the run's total display width (pad included).
    fn rendered(spans: &[Span<'static>]) -> (String, usize) {
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let width: usize = text.chars().map(char_width).sum();
        (text.trim_end().to_string(), width)
    }

    fn one_span(text: &str) -> Vec<(Color, String)> {
        vec![(Color::Reset, text.to_string())]
    }

    fn seg(start: usize, end: usize) -> Seg {
        Seg {
            start_char: start,
            end_char: end,
        }
    }

    fn ctx(old_no: Option<usize>, new_no: Option<usize>) -> DiffLine {
        DiffLine {
            kind: LineKind::Context,
            old_no,
            new_no,
            text: String::new(),
        }
    }

    #[test]
    fn number_width_floors_at_four_and_widens_for_five_plus_digits() {
        // ≤9999-line files keep the classic 4-wide column (gutter stays 10).
        let small = [ctx(Some(1), Some(1)), ctx(Some(42), Some(9999))];
        assert_eq!(line_number_width(&small), 4);
        assert_eq!(unified_gutter_width(true, line_number_width(&small)), 10);

        // A 5-digit number widens the column to 5 (gutter 12), so the drawn gutter
        // matches the reserved one and wrapped content can't be clipped.
        let big = [ctx(Some(9998), Some(10000)), ctx(None, Some(10005))];
        assert_eq!(line_number_width(&big), 5);
        assert_eq!(unified_gutter_width(true, line_number_width(&big)), 12);

        // Empty / no-number lines fall back to the floor.
        assert_eq!(line_number_width(&[ctx(None, None)]), 4);
        assert_eq!(line_number_width(&[]), 4);
    }

    #[test]
    fn gutter_num_pads_to_the_dynamic_width() {
        assert_eq!(gutter_num(Some(7), 4), "   7");
        assert_eq!(gutter_num(Some(10005), 5), "10005");
        assert_eq!(gutter_num(None, 5), "     ");
    }

    #[test]
    fn wrap_splits_on_display_columns() {
        // "abcdef" at width 2 -> [0,2) [2,4) [4,6).
        let segs = wrap_segments("abcdef", 2);
        assert_eq!(segs, vec![seg(0, 2), seg(2, 4), seg(4, 6)]);
    }

    #[test]
    fn wrap_empty_line_is_one_empty_segment() {
        assert_eq!(wrap_segments("", 8), vec![seg(0, 0)]);
    }

    #[test]
    fn wrap_wide_char_moves_wholly_to_next_segment() {
        // "a世b" — '世' is 2 cols. Width 2: 'a'(1) fits; '世'(2) would overflow the
        // remaining 1 col, so it starts the next segment whole; then 'b'.
        let segs = wrap_segments("a世b", 2);
        assert_eq!(segs, vec![seg(0, 1), seg(1, 2), seg(2, 3)]);
        // Width 1: '世' alone is wider than the content — the ≥1-char floor still
        // makes progress rather than looping.
        let segs = wrap_segments("世世", 1);
        assert_eq!(segs, vec![seg(0, 1), seg(1, 2)]);
    }

    #[test]
    fn wrap_never_starts_a_segment_on_a_combining_mark() {
        // "e\u{301}" (e + combining acute) is one cell. A boundary must not fall
        // between the base and its mark, so both stay in the same segment.
        let text = "ae\u{301}b"; // a, e, ́, b  (chars 0..4, cols a=1 e=1 mark=0 b=1)
        let segs = wrap_segments(text, 2);
        // width 2: 'a'(1)+'e'(1) fill the row; the mark (col 0) folds onto 'e';
        // 'b' starts the next segment. So [0,3) then [3,4).
        assert_eq!(segs, vec![seg(0, 3), seg(3, 4)]);
        for s in &segs {
            let first = text.chars().nth(s.start_char).unwrap();
            assert_ne!(char_width(first), 0, "a segment never starts on a mark");
        }
    }

    #[test]
    fn wrap_tabs_are_expanded_before_wrapping() {
        // A tab sanitizes to four spaces; wrapping then sees eight display columns.
        let clean = sanitize("\ta");
        assert_eq!(clean, "    a");
        let segs = wrap_segments(&clean, 4);
        assert_eq!(segs, vec![seg(0, 4), seg(4, 5)]);
    }

    #[test]
    fn wrap_width_zero_still_progresses() {
        let segs = wrap_segments("abc", 0);
        assert_eq!(segs, vec![seg(0, 1), seg(1, 2), seg(2, 3)]);
    }

    #[test]
    fn slice_windows_the_segment_and_pads_to_width() {
        let hl = one_span("abcdef");
        // The middle segment [2,4) rendered into a width-4 row: "cd" + pad.
        let spans = slice_spans(&hl, seg(2, 4), 0, 4, Color::Reset, None);
        let (text, width) = rendered(&spans);
        assert_eq!(text, "cd");
        assert_eq!(width, 4, "row padded to the content width");
    }

    #[test]
    fn slice_wide_char_at_trailing_edge_is_dropped_and_padded() {
        // "a世" into width 2: 'a'(1) fits, '世'(2) overflows the last col, so it is
        // dropped and the row padded — the pre-wrap truncation policy.
        let hl = one_span("a世");
        let spans = slice_spans(&hl, seg(0, 2), 0, 2, Color::Reset, None);
        let (text, width) = rendered(&spans);
        assert_eq!(text, "a");
        assert_eq!(width, 2);
    }

    #[test]
    fn slice_glues_a_combining_mark_to_its_base() {
        let hl = one_span("e\u{301}x");
        let spans = slice_spans(&hl, seg(0, 3), 0, 8, Color::Reset, None);
        // The base and mark must share one span (one cell), never split apart.
        let combined = spans.iter().any(|s| s.content.contains("e\u{301}"));
        assert!(combined, "combining mark stays in the base's span");
    }

    #[test]
    fn slice_leading_wide_char_straddle_pads_its_overhang() {
        // col_skip=1 into "世ab": the first col of '世' is skipped, its second col
        // peeks past the boundary as one bg pad cell, then "ab".
        let hl = one_span("世ab");
        let spans = slice_spans(&hl, seg(0, 3), 1, 4, Color::Reset, None);
        let (text, width) = rendered(&spans);
        // The glyph itself is never re-emitted; its overhang shows as one leading
        // background pad cell, then "ab".
        assert!(
            !text.contains('世'),
            "the wide char's glyph is not re-emitted: {text:?}"
        );
        assert_eq!(text, " ab", "leading overhang pad + ab");
        assert_eq!(width, 4, "one overhang pad + ab + trailing pad");
    }

    #[test]
    fn slice_full_segment_reproduces_truncation() {
        // A whole-line segment with no skip and a tight width truncates exactly as
        // the old renderer: "abcdef" at width 3 -> "abc" padded to 3.
        let hl = one_span("abcdef");
        let full = seg(0, 6);
        let spans = slice_spans(&hl, full, 0, 3, Color::Reset, None);
        let (text, width) = rendered(&spans);
        assert_eq!(text, "abc");
        assert_eq!(width, 3);
    }

    #[test]
    fn slice_drops_a_combining_mark_stranded_by_the_skip() {
        // "ae\u{301}b": a(0), e(1), combining acute(2, w0), b(3). Skip 2 columns
        // (over 'a' and 'e'): the mark's base 'e' is gone, so the mark must be
        // dropped — not stranded at the leading edge — and 'b' is the first glyph.
        let hl = one_span("ae\u{301}b");
        let spans = slice_spans(&hl, seg(0, 4), 2, 8, Color::Reset, None);
        let (text, _) = rendered(&spans);
        assert!(
            !text.contains('\u{301}'),
            "stranded mark is dropped: {text:?}"
        );
        assert!(text.starts_with('b'), "content resumes at 'b': {text:?}");
    }

    #[test]
    fn slice_keeps_emphasis_aligned_after_dropping_a_stranded_mark() {
        // The dropped mark still advances `char_idx`, so an emphasis range on the
        // following char lands correctly.
        let hl = one_span("ae\u{301}b");
        let emph = Color::Red;
        // A one-element slice of ranges is exactly the emphasis shape here.
        #[allow(clippy::single_range_in_vec_init)]
        let ranges = [3..4]; // the 'b'
        let spans = slice_spans(&hl, seg(0, 4), 2, 8, Color::Reset, Some((&ranges, emph)));
        let b_span = spans
            .iter()
            .find(|s| s.content.contains('b'))
            .expect("a 'b' span");
        assert_eq!(b_span.style.bg, Some(emph), "emphasis aligned to 'b'");
    }

    #[test]
    fn slice_cut_wide_char_keeps_its_emphasis_on_the_overhang_pad() {
        // A skip landing mid-wide-char: its visible overhang pad carries the
        // emphasis background when the cut char is emphasized (FIX 3).
        let hl = one_span("世x");
        let emph = Color::Red;
        #[allow(clippy::single_range_in_vec_init)]
        let ranges = [0..1]; // the wide char is the changed char
        let spans = slice_spans(&hl, seg(0, 2), 1, 8, Color::Reset, Some((&ranges, emph)));
        let first = &spans[0];
        assert_eq!(
            first.content.as_ref(),
            " ",
            "the overhang is a background pad"
        );
        assert_eq!(first.style.bg, Some(emph), "the overhang pad is emphasized");
        // A non-emphasized cut wide char pads with the plain background.
        let plain = slice_spans(&hl, seg(0, 2), 1, 8, Color::Reset, None);
        assert_eq!(
            plain[0].style.bg,
            Some(Color::Reset),
            "no emphasis -> plain bg"
        );
    }
}
