use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem};
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::git::{Change, FileEntry, Section, Status};
use crate::ui::theme::Theme;
use crate::ui::{centered_hint, panel_block};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.focus == Focus::Staging;
    let block = panel_block(" Changes ", focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.set_staging_area(inner);

    if app.status.is_clean() {
        centered_hint(
            frame,
            inner,
            "✓ working tree clean",
            Style::new().fg(theme.staged),
        );
        return;
    }

    let items = build_items(&app.status, theme);
    let selected_item = file_item_rows(&app.status).get(app.selected).copied();

    let highlight = if focused {
        Style::new()
            .bg(theme.selection_bg)
            .fg(theme.selection_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().bg(theme.selection_bg)
    };
    let list = List::new(items).highlight_style(highlight);

    let mut state = app.staging_state();
    state.select(selected_item);
    frame.render_stateful_widget(list, inner, &mut state);
}

/// The list rows: a section header followed by its files, staged then unstaged.
fn build_items(status: &Status, theme: &Theme) -> Vec<ListItem<'static>> {
    let mut items = Vec::new();
    if !status.staged.is_empty() {
        items.push(section_header("Staged", status.staged.len(), theme));
        for entry in &status.staged {
            items.push(file_item(entry, Section::Staged, theme));
        }
    }
    if !status.unstaged.is_empty() {
        items.push(section_header("Changes", status.unstaged.len(), theme));
        for entry in &status.unstaged {
            items.push(file_item(entry, Section::Unstaged, theme));
        }
    }
    items
}

/// The list-item index of each selectable file, by selection index. Mirrors the
/// row order produced by [`build_items`] so mouse clicks map to the right file.
pub fn file_item_rows(status: &Status) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut item = 0;
    if !status.staged.is_empty() {
        item += 1; // "Staged" header
        for _ in &status.staged {
            rows.push(item);
            item += 1;
        }
    }
    if !status.unstaged.is_empty() {
        item += 1; // "Changes" header
        for _ in &status.unstaged {
            rows.push(item);
            item += 1;
        }
    }
    rows
}

fn section_header(label: &str, count: usize, theme: &Theme) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        format!(" {label} ({count})"),
        Style::new().fg(theme.dim).add_modifier(Modifier::BOLD),
    )))
}

fn file_item(entry: &FileEntry, section: Section, theme: &Theme) -> ListItem<'static> {
    let color = marker_color(section, entry.change, theme);
    ListItem::new(Line::from(vec![
        Span::styled(
            format!("  {} ", entry.change.marker()),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(entry.display_path(), Style::new().fg(theme.fg)),
    ]))
}

fn marker_color(section: Section, change: Change, theme: &Theme) -> Color {
    match section {
        Section::Staged => theme.staged,
        Section::Unstaged if change == Change::Untracked => theme.untracked,
        Section::Unstaged => theme.unstaged,
    }
}
