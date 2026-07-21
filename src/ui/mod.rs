pub mod diff_view;
pub mod history;
pub mod menu;
pub mod modal;
pub mod review;
pub mod staging;
pub mod syntax;
pub mod theme;

use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, FlashKind, ViewMode};
use crate::git::{ChangeKind, CommitFile};
use crate::ui::theme::Theme;

/// Top-level render: header / body / footer, with the body split into the
/// staging pane (left) and diff pane (right).
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let theme = &app.theme;

    frame.render_widget(
        Block::new().style(Style::new().bg(theme.bg).fg(theme.fg)),
        area,
    );

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header, app);

    match app.view {
        ViewMode::Status => draw_status_body(frame, body, app),
        ViewMode::History => history::render(frame, body, app),
        ViewMode::Review => review::render(frame, body, app),
    }

    render_footer(frame, footer, app);

    // Overlays draw last, on top of everything. Menus and modals are mutually
    // exclusive (a modal captures input; opening a menu is mouse-only and mouse
    // is ignored while a modal is open), so their draw order is immaterial.
    modal::render(frame, app);
    menu::render_dropdown(frame, app);
}

/// The status view's body: the Changes panel (fixed width) beside the diff, or
/// the full-width diff when the panel is hidden.
fn draw_status_body(frame: &mut Frame, body: Rect, app: &App) {
    let theme = &app.theme;
    if app.show_changes {
        // The Changes panel is a fixed width; the diff takes the rest, so a
        // wider terminal feeds the diff. Drag the split bar to resize (see
        // `App::resize_changes`).
        let width = app.changes_pane_width(body.width);
        let [left, right] =
            Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(body);
        staging::render(frame, left, app);
        diff_view::render(frame, right, app);
        app.set_split_geometry(body, right.x);
        if app.divider_engaged() {
            highlight_divider(frame, body, right.x, theme);
        }
    } else {
        // Clear the stale staging rect so mouse hit-testing (`pane_at`) can't
        // match where the panel used to be; give the whole body to the diff.
        app.set_staging_area(Rect::default());
        diff_view::render(frame, body, app);
    }
}

/// Tint the split bar — the two adjacent pane borders at `divider_x` — with the
/// focus accent, so it reads as draggable while hovered or being dragged.
pub(crate) fn highlight_divider(frame: &mut Frame, body: Rect, divider_x: u16, theme: &Theme) {
    let style = Style::new()
        .fg(theme.border_focused)
        .add_modifier(Modifier::BOLD);
    let buf = frame.buffer_mut();
    let bottom = body.y.saturating_add(body.height);
    for y in body.y..bottom {
        for x in [divider_x.saturating_sub(1), divider_x] {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(style);
            }
        }
    }
}

/// The brand text, shared by both header layouts.
const BRAND: &str = " strix ";

/// The brand's bold style, shared by both header layouts.
fn brand_style(theme: &Theme) -> Style {
    Style::new()
        .fg(theme.header_fg)
        .add_modifier(Modifier::BOLD)
}

/// The header's context spans: the repo name (dim) plus a per-view label (the
/// accent title) — `" · history"` in History, `" · <range>"` in Review. Shared
/// by both header layouts; the brand is prepended separately.
fn context_spans(app: &App, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(app.repo_name(), Style::new().fg(theme.dim))];
    match app.view {
        ViewMode::History => {
            spans.push(Span::styled(" · history", Style::new().fg(theme.title)));
        }
        ViewMode::Review => {
            if let Some(display) = app.review_display() {
                spans.push(Span::styled(
                    format!(" · {display}"),
                    Style::new().fg(theme.title),
                ));
            }
        }
        ViewMode::Status => {}
    }
    spans
}

/// The right-aligned branch / ahead-behind label, suppressed in a review session
/// (its range label already identifies HEAD, so a status branch would mislead —
/// e.g. for an `A...B` range). `None` when there is nothing to show.
fn branch_text(app: &App) -> Option<String> {
    if app.view == ViewMode::Review {
        return None;
    }
    app.status.head_label().map(|branch| format!("{branch} "))
}

/// Draw the right-aligned branch label into `rect`. The shared leaf of both
/// header paths — the plain path passes the full area, the menu path a sub-rect
/// sized to the branch.
fn render_branch(frame: &mut Frame, rect: Rect, branch: String, theme: &Theme) {
    let line = Line::from(Span::styled(branch, Style::new().fg(theme.title)));
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Right), rect);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    frame.render_widget(Block::new().style(Style::new().bg(theme.header_bg)), area);
    if app.show_menu_bar {
        render_header_with_menu(frame, area, app);
    } else {
        // Clear any stale title rects so a click can't match a label that is no
        // longer drawn (mirrors `set_staging_area(Rect::default())`).
        app.set_menu_title_rects(Vec::new());
        render_header_plain(frame, area, app);
    }
}

/// Today's header (no menu bar): the brand + context drawn as one left-aligned
/// paragraph over the full area, with the branch a separate right-aligned
/// paragraph on top. Kept byte-for-byte identical to the pre-menu-bar output so
/// `menu_bar = false` changes nothing.
fn render_header_plain(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut left_spans = vec![Span::styled(BRAND, brand_style(theme))];
    left_spans.extend(context_spans(app, theme));
    frame.render_widget(Paragraph::new(Line::from(left_spans)), area);

    if let Some(branch) = branch_text(app) {
        render_branch(frame, area, branch, theme);
    }
}

/// The menu-bar header: brand → `View`/`Theme` labels → context → branch, each
/// in its own non-overlapping sub-rect laid out in display columns. The labels
/// sit at fixed columns right after the brand (stable, hit-testable); context
/// yields first under width pressure (truncated with `…`, then emptied) so the
/// branch keeps its natural width, and the branch is suppressed only when
/// nothing is left for it.
fn render_header_with_menu(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;

    // Brand — always at the left edge.
    let brand_w = text_width(BRAND) as u16;
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(BRAND, brand_style(theme)))),
        sub_rect(area, area.x, brand_w),
    );

    // Menu labels — fixed columns right after the brand. On a pathologically
    // narrow terminal the trailing cells clip to nothing (drawn via the
    // intersection with `area`), and context + branch are dropped.
    let label_start = area.x.saturating_add(brand_w);
    let labels_w = menu::menus_width();
    // The open menu's title is highlighted (selection colours) so it reads as
    // active while its dropdown is showing; the rest use the plain title accent.
    let open = app.open_menu.map(|o| o.menu);
    let mut title_rects = Vec::new();
    for (id, rect) in menu::header_menu_layout(label_start, area.y) {
        let cell = rect.intersection(area);
        if cell.width == 0 {
            continue;
        }
        let style = if open == Some(id) {
            Style::new().bg(theme.selection_bg).fg(theme.selection_fg)
        } else {
            Style::new().fg(theme.title)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(menu::menu_cell(id), style))),
            cell,
        );
        // Record the actually-drawn (clamped) cell, so even a clipped label on a
        // pathologically narrow terminal still hit-tests where it is visible.
        title_rects.push((id, cell));
    }
    app.set_menu_title_rects(title_rects);

    let labels_end = label_start.saturating_add(labels_w);
    if labels_end >= area.right() {
        // Labels fill (or overflow) the row: nothing left for context/branch.
        return;
    }

    // Split the remainder into a left context rect and a right branch rect.
    // Reserve the branch at its natural width; context takes the rest (with a
    // 1-column gap) and yields first when the row is tight.
    let right_of_labels = area.right().saturating_sub(labels_end);
    let branch = branch_text(app);
    // Saturate the display-width→u16 cast: a pathologically long branch name
    // must clamp, never wrap (a wrap would mislay/suppress the branch).
    let branch_w = branch
        .as_deref()
        .map(|t| text_width(t).min(u16::MAX as usize) as u16)
        .unwrap_or(0);
    const MIN_BRANCH_W: u16 = 6;
    let (context_w, branch_w) = if branch_w == 0 {
        (right_of_labels, 0)
    } else if right_of_labels > branch_w.saturating_add(1) {
        (
            right_of_labels.saturating_sub(branch_w).saturating_sub(1),
            branch_w,
        )
    } else if right_of_labels >= MIN_BRANCH_W {
        // No room for both: keep the branch (truncated), drop context.
        (0, right_of_labels)
    } else {
        // Too little even for a minimal branch: give it all to context.
        (right_of_labels, 0)
    };

    if context_w > 0 {
        let spans = fit_spans(context_spans(app, theme), context_w as usize);
        if !spans.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(labels_end, area.y, context_w, area.height),
            );
        }
    }
    if branch_w > 0 {
        if let Some(branch) = branch {
            let rect = Rect::new(
                area.right().saturating_sub(branch_w),
                area.y,
                branch_w,
                area.height,
            );
            render_branch(frame, rect, branch, theme);
        }
    }
}

/// A single-row sub-rect of `area` starting at display column `x`, `width`
/// columns wide, clamped to `area`'s right edge.
fn sub_rect(area: Rect, x: u16, width: u16) -> Rect {
    let start = x.min(area.right());
    let end = x.saturating_add(width).min(area.right());
    Rect::new(start, area.y, end - start, area.height)
}

/// Fit a styled context line into `max` display columns, preserving each span's
/// colour. If the whole line fits it is returned unchanged; otherwise it is
/// truncated across span boundaries with a **single** trailing `…`, one column
/// reserved globally — so a span that exactly fills the width can't silently
/// drop the spans after it (e.g. a repo name eating the ` · history` label).
fn fit_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| text_width(&s.content)).sum();
    if total <= max {
        return spans;
    }
    if max == 0 {
        return Vec::new();
    }
    let budget = max - 1; // reserve one column for the single trailing ellipsis
    let mut out = Vec::with_capacity(spans.len() + 1);
    let mut used = 0;
    for span in spans {
        if used >= budget {
            break;
        }
        let mut text = String::new();
        for ch in span.content.chars() {
            let w = char_width(ch);
            if used + w > budget {
                break;
            }
            text.push(ch);
            used += w;
        }
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
    }
    let style = out.last().map(|s| s.style).unwrap_or_default();
    out.push(Span::styled("…", style));
    out
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;

    // A transient flash takes the footer in place of the key hints. Errors keep
    // the `✗` + bold `del` styling; info notices (e.g. a theme name after a cycle)
    // render plainly in `fg` with no marker, matching the house minimalism.
    if let Some(flash) = &app.flash {
        let line = match flash.kind {
            FlashKind::Error => Line::from(Span::styled(
                format!(" ✗ {}", flash.text),
                Style::new().fg(theme.del).add_modifier(Modifier::BOLD),
            )),
            FlashKind::Info => Line::from(Span::styled(
                format!(" {}", flash.text),
                Style::new().fg(theme.fg),
            )),
        };
        frame.render_widget(
            Paragraph::new(line).style(Style::new().bg(theme.footer_bg)),
            area,
        );
        return;
    }

    let key_style = Style::new()
        .fg(theme.footer_key)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::new().fg(theme.footer_fg);

    // The `b` toggle's label tracks what the key will do next.
    let changes_label = if app.show_changes {
        "hide  "
    } else {
        "changes  "
    };
    let hints: Vec<(&str, &str)> = match app.view {
        ViewMode::Status => {
            // Comment-navigation hints join the status footer (worktree comments on
            // the net diff); `c` adds/edits the note under the cursor, so it shows
            // only when the diff pane (where the cursor lives) is focused.
            let mut hints = vec![
                (" j/k ", "move  "),
                (" space ", "stage  "),
                (" ]/[ ", "notes  "),
            ];
            if app.diff_focused() {
                hints.push((" c ", "comment  "));
                hints.push((" X ", "delete  "));
            }
            hints.extend([
                (" d ", "split  "),
                (" n ", "line #s  "),
                (" t ", "theme  "),
                (" b ", changes_label),
                (" i ", "history  "),
                (" ? ", "help  "),
                (" q ", "quit"),
            ]);
            hints
        }
        ViewMode::History => vec![
            (" j/k ", "move  "),
            (" tab ", "pane  "),
            (" d ", "split  "),
            (" n ", "line #s  "),
            (" t ", "theme  "),
            (" b ", changes_label),
            (" i/esc ", "back  "),
            (" ? ", "help  "),
            (" q ", "quit"),
        ],
        ViewMode::Review => {
            // Comment-navigation hints join the review footer; `c` adds/edits and
            // `X` deletes the comment under the cursor, so both only show when the
            // diff pane (where the cursor lives) is focused.
            let mut hints = vec![(" j/k ", "move  "), (" ]/[ ", "notes  ")];
            if app.diff_focused() {
                hints.push((" c ", "comment  "));
                hints.push((" X ", "delete  "));
            }
            hints.extend([
                (" tab ", "pane  "),
                (" d ", "split  "),
                (" t ", "theme  "),
                (" b ", changes_label),
                (" i ", "history  "),
                (" ? ", "help  "),
                (" q ", "quit"),
            ]);
            hints
        }
    };
    let mut spans = Vec::new();
    // Review-only: orphaned comments that no diff block can show (files gone from
    // the range, or binary files) are surfaced here — `strix comment list` is the
    // only way to see/clear them (plan §3.4).
    if app.view == ViewMode::Review {
        let orphans = app.orphan_footer_count();
        if orphans > 0 {
            spans.push(Span::styled(
                format!(" ⚠ {orphans} orphaned — strix comment list  "),
                Style::new().fg(theme.del).add_modifier(Modifier::BOLD),
            ));
        }
    }
    for (key, label) in hints {
        spans.push(Span::styled(key, key_style));
        spans.push(Span::styled(label, label_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(theme.footer_bg)),
        area,
    );
}

/// The highlight style for a selected list row: the selection background, plus
/// the selection foreground in bold when its pane is focused. Shared by the
/// staging and history list panes so selection styling stays consistent.
pub fn selection_style(focused: bool, theme: &Theme) -> Style {
    if focused {
        Style::new()
            .bg(theme.selection_bg)
            .fg(theme.selection_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().bg(theme.selection_bg)
    }
}

/// A changed file's theme colour, keyed on its change kind. Shared by the review
/// file list and the history commit-detail summary.
pub(crate) fn change_color(change: ChangeKind, theme: &Theme) -> Color {
    match change {
        ChangeKind::Added | ChangeKind::Copied => theme.staged,
        ChangeKind::Deleted => theme.del,
        _ => theme.unstaged,
    }
}

/// The spans for one changed-file row: a bold change marker, the display path,
/// then `+a −d` line stats (or `(binary)`). Shared by the review file list and
/// the history commit-detail per-file breakdown so the row stays identical.
pub(crate) fn file_stat_spans(file: &CommitFile, theme: &Theme) -> Vec<Span<'static>> {
    let color = change_color(file.change, theme);
    let mut spans = vec![
        Span::styled(
            format!("  {} ", file.change.marker()),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(file.display_path(), Style::new().fg(theme.fg)),
    ];
    if file.stat.binary {
        spans.push(Span::styled(
            "  (binary)".to_string(),
            Style::new().fg(theme.dim),
        ));
    } else {
        spans.push(Span::styled(
            format!("  +{} ", file.stat.added),
            Style::new().fg(theme.add),
        ));
        spans.push(Span::styled(
            format!("−{}", file.stat.deleted),
            Style::new().fg(theme.del),
        ));
    }
    spans
}

/// A bordered panel with focus-aware border + title colours. Shared by the
/// staging and diff panes (and future overlays) so focus styling stays
/// consistent.
pub fn panel_block<'a>(title: &'a str, focused: bool, theme: &Theme) -> Block<'a> {
    let border_color = if focused {
        theme.border_focused
    } else {
        theme.border
    };
    let title_color = if focused { theme.title } else { theme.dim };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border_color))
        .title(Span::styled(
            title,
            Style::new().fg(title_color).add_modifier(Modifier::BOLD),
        ))
}

/// A sub-rect of `area` that is `height` rows tall and vertically centred,
/// spanning the full width. Used to place empty-state hints.
pub fn vertical_center(area: Rect, height: u16) -> Rect {
    let [rect] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    rect
}

/// Draw a centred single-line hint in the middle of `area`, for empty states.
pub fn centered_hint(frame: &mut Frame, area: Rect, text: &str, style: Style) {
    frame.render_widget(
        Paragraph::new(text)
            .style(style)
            .alignment(Alignment::Center),
        vertical_center(area, 1),
    );
}

/// A `width`×`height` rect centred within `area` (clamped to `area`), for modal
/// popups.
pub fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let row = vertical_center(area, height.min(area.height));
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}

/// The terminal-cell width of `ch` (0 for control/zero-width chars). The shared
/// helper every widget that lays text out column-by-column reads from.
pub(crate) fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// The terminal-cell width of `s` (the sum of its chars' display widths). The
/// menu bar lays labels out in display columns, so `·`/non-ASCII repo names
/// can't be measured by byte or char count.
pub(crate) fn text_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn fit_spans_keeps_a_line_that_fits() {
        let out = fit_spans(vec![Span::raw("abc"), Span::raw(" de")], 10);
        assert_eq!(joined(&out), "abc de");
    }

    #[test]
    fn fit_spans_ellipsizes_once_when_a_span_exactly_fills_the_width() {
        // The repo name exactly fills `max`; the trailing ` · history` label must
        // not be dropped silently — a single ellipsis marks the truncation.
        let out = fit_spans(vec![Span::raw("abcdefghij"), Span::raw(" · history")], 10);
        let text = joined(&out);
        assert!(
            text.ends_with('…'),
            "want a trailing ellipsis, got {text:?}"
        );
        assert_eq!(
            text.chars().filter(|&c| c == '…').count(),
            1,
            "one ellipsis"
        );
        assert!(text_width(&text) <= 10, "width {} > 10", text_width(&text));
    }
}
