//! History view rendering: a two-part left column — the selected commit's files
//! (top) and the commit-graph log (bottom) split by a draggable horizontal
//! divider — beside the shared diff pane, which shows either the selected file's
//! diff or, when the commit row is selected, the commit's details.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{App, HistoryFocus};
use crate::git::{CommitFile, CommitInfo};
use crate::graph::GraphRow;
use crate::ui::theme::Theme;
use crate::ui::{
    centered_hint, change_color, diff_view, file_stat_spans, panel_block, selection_style,
};

pub fn render(frame: &mut Frame, body: Rect, app: &App) {
    let theme = &app.theme;

    // `b` collapses the left column, like the status view. Hidden: the diff
    // pane fills the body; clear the stale geometry so mouse hit-testing can't
    // match where the panels used to be.
    if !app.show_changes {
        app.set_committed_area(Rect::default());
        app.set_graph_area(Rect::default());
        app.set_hsplit_geometry(Rect::default(), 0);
        if app.history_shows_details() {
            render_details(frame, body, app);
        } else {
            diff_view::render(frame, body, app);
        }
        return;
    }

    // Left column is a fixed width (shared with the status view); the diff takes
    // the rest. Reuse the vertical split bar wholesale.
    let width = app.changes_pane_width(body.width);
    let [left, right] =
        Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(body);
    app.set_split_geometry(body, right.x);

    // Split the left column into the committed-changes pane (top) and the graph
    // (bottom), divided by the draggable horizontal bar.
    let top_h = app.committed_pane_height(left.height);
    let [top, bottom] =
        Layout::vertical([Constraint::Length(top_h), Constraint::Min(0)]).areas(left);
    app.set_hsplit_geometry(left, bottom.y);

    render_committed(frame, top, app);
    render_graph(frame, bottom, app);

    if app.history_shows_details() {
        render_details(frame, right, app);
    } else {
        diff_view::render(frame, right, app);
    }

    // Divider affordances last, so they tint the borders already drawn.
    if app.divider_engaged() {
        super::highlight_divider(frame, body, right.x, theme);
    }
    if app.hdivider_engaged() {
        highlight_hdivider(frame, left, bottom.y, theme);
    }
}

/// The top "Committed Changes" pane: the commit (`●`) row, then its files.
fn render_committed(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.history_focus() == HistoryFocus::CommittedChanges;
    let block = panel_block(" Committed Changes ", focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.set_committed_area(inner);

    let Some(commit) = app.selected_commit_info() else {
        centered_hint(frame, inner, "No commits yet", Style::new().fg(theme.dim));
        return;
    };

    let mut items = vec![commit_row(commit, theme)];
    for file in app.history_files() {
        items.push(file_row(file, theme));
    }

    let list = List::new(items).highlight_style(selection_style(focused, theme));
    let mut state = app.committed_state_mut();
    state.select(Some(app.committed_row()));
    frame.render_stateful_widget(list, inner, &mut state);
}

/// The bottom "Graph" pane: the commit log with rail glyphs + labels.
fn render_graph(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.history_focus() == HistoryFocus::Graph;
    let block = panel_block(" Graph ", focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.set_graph_area(inner);

    let commits = app.commits();
    if commits.is_empty() {
        centered_hint(frame, inner, "No commits yet", Style::new().fg(theme.dim));
        return;
    }

    let items: Vec<ListItem> = app
        .graph_rows()
        .iter()
        .map(|row| graph_row(row, commits, theme))
        .collect();

    let list = List::new(items).highlight_style(selection_style(focused, theme));
    let mut state = app.graph_state_mut();
    state.select(Some(app.selected_commit()));
    frame.render_stateful_widget(list, inner, &mut state);
}

/// The right pane when the commit row is selected: a `git show`-style summary.
fn render_details(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.history_focus() == HistoryFocus::Diff;
    let block = panel_block(" Commit ", focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.set_diff_area(inner);

    let Some(commit) = app.selected_commit_info() else {
        app.set_diff_metrics(inner.height, 0);
        centered_hint(
            frame,
            inner,
            "Select a commit to view its details",
            Style::new().fg(theme.dim),
        );
        return;
    };

    let value = Style::new().fg(theme.fg);
    let mut lines = vec![
        field(
            "commit ",
            &commit.id.to_string(),
            Style::new().fg(theme.title),
        ),
        field(
            "Author ",
            &format!("{} <{}>", commit.author_name, commit.author_email),
            value,
        ),
        field(
            "Date   ",
            &fmt_time(commit.author_seconds, commit.author_offset),
            value,
        ),
    ];
    // Only show the committer when it differs from the author.
    if commit.committer_name != commit.author_name || commit.committer_email != commit.author_email
    {
        lines.push(field(
            "Commit ",
            &format!("{} <{}>", commit.committer_name, commit.committer_email),
            value,
        ));
    }
    lines.push(Line::from(""));
    for line in commit.message.lines() {
        lines.push(Line::from(Span::styled(format!("    {line}"), value)));
    }
    lines.push(Line::from(""));
    lines.extend(stat_summary(app.history_files(), theme));

    app.set_diff_metrics(inner.height, lines.len());
    // `Paragraph::scroll` takes a `u16`; the commit-details pane is short, so
    // clamping the (now `usize`) offset can't lose anything real.
    let offset = app
        .diff_scroll
        .min(app.diff_max_scroll())
        .min(u16::MAX as usize) as u16;
    frame.render_widget(Paragraph::new(lines).scroll((offset, 0)), inner);
}

/// A `name: value` detail line, the name dimmed and the value in `val_style`.
fn field(name: &str, val: &str, val_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            name.to_string(),
            Style::new()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(val.to_string(), val_style),
    ])
}

/// The `N files changed, +A −D` line plus a per-file `M path +a −d` breakdown.
fn stat_summary(files: &[CommitFile], theme: &Theme) -> Vec<Line<'static>> {
    let added: usize = files.iter().map(|f| f.stat.added).sum();
    let deleted: usize = files.iter().map(|f| f.stat.deleted).sum();
    let mut lines = vec![Line::from(Span::styled(
        format!(
            " {} file{} changed, +{added} −{deleted}",
            files.len(),
            if files.len() == 1 { "" } else { "s" },
        ),
        Style::new().fg(theme.dim).add_modifier(Modifier::BOLD),
    ))];
    for file in files {
        lines.push(Line::from(file_stat_spans(file, theme)));
    }
    lines
}

fn commit_row(commit: &CommitInfo, theme: &Theme) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::styled(
            format!("● {} ", commit.short),
            Style::new().fg(theme.title).add_modifier(Modifier::BOLD),
        ),
        Span::styled(commit.summary.clone(), Style::new().fg(theme.fg)),
    ]))
}

fn file_row(file: &CommitFile, theme: &Theme) -> ListItem<'static> {
    let color = change_color(file.change, theme);
    ListItem::new(Line::from(vec![
        Span::styled(
            format!("  {} ", file.change.marker()),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(file.display_path(), Style::new().fg(theme.fg)),
    ]))
}

fn graph_row(row: &GraphRow, commits: &[CommitInfo], theme: &Theme) -> ListItem<'static> {
    let mut spans = Vec::new();
    // The rail, each lane in its lane colour.
    for cell in &row.cells {
        spans.push(Span::styled(
            cell.glyph.to_string(),
            Style::new().fg(theme.lane(cell.lane)),
        ));
    }
    spans.push(Span::raw(" "));

    if let Some(commit) = commits.get(row.commit) {
        spans.push(Span::styled(
            format!("{} ", commit.short),
            Style::new().fg(theme.dim),
        ));
        for label in &row.labels {
            spans.push(Span::styled(
                format!("{label} "),
                Style::new()
                    .fg(theme.untracked)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(
            commit.summary.clone(),
            Style::new().fg(theme.fg),
        ));
    }
    ListItem::new(Line::from(spans))
}

/// Tint the horizontal split bar — the two adjacent pane borders at `hdivider_y`
/// — across the left column, the horizontal analogue of `highlight_divider`.
fn highlight_hdivider(frame: &mut Frame, left: Rect, hdivider_y: u16, theme: &Theme) {
    let style = Style::new()
        .fg(theme.border_focused)
        .add_modifier(Modifier::BOLD);
    let buf = frame.buffer_mut();
    let right = left.x.saturating_add(left.width);
    for y in [hdivider_y.saturating_sub(1), hdivider_y] {
        for x in left.x..right {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(style);
            }
        }
    }
}

/// Format a git timestamp (`seconds` since the epoch, `offset` seconds east of
/// UTC) as `YYYY-MM-DD HH:MM ±HHMM` in that local offset. Self-contained civil
/// date math (no date crate); accurate for the proleptic Gregorian calendar.
fn fmt_time(seconds: i64, offset: i32) -> String {
    let local = seconds + offset as i64;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (hour, minute) = (secs / 3600, (secs % 3600) / 60);
    let (year, month, day) = civil_from_days(days);
    let (sign, off) = if offset < 0 {
        ('-', -offset)
    } else {
        ('+', offset)
    };
    let (oh, om) = (off / 3600, (off % 3600) / 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} {sign}{oh:02}{om:02}")
}

/// Convert a day count since 1970-01-01 to `(year, month, day)`. Howard
/// Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}
