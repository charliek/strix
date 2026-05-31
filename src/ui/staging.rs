use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem};
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::git::{Change, FileEntry, Section, Status};
use crate::ui::theme::Theme;
use crate::ui::{centered_hint, panel_block, selection_style};

/// One row of the staging list, in display order.
enum Row<'a> {
    Header {
        label: &'static str,
        count: usize,
    },
    File {
        selection: usize,
        section: Section,
        entry: &'a FileEntry,
    },
}

/// The staging list rows in order: each section header followed by its files,
/// staged then unstaged. The single source of truth for the list layout, shared
/// by rendering and mouse hit-testing so the two can't disagree on row order.
fn rows(status: &Status) -> Vec<Row<'_>> {
    let mut rows = Vec::new();
    let mut selection = 0;
    for (label, section, entries) in [
        ("Staged", Section::Staged, &status.staged),
        ("Changes", Section::Unstaged, &status.unstaged),
    ] {
        if entries.is_empty() {
            continue;
        }
        rows.push(Row::Header {
            label,
            count: entries.len(),
        });
        for entry in entries {
            rows.push(Row::File {
                selection,
                section,
                entry,
            });
            selection += 1;
        }
    }
    rows
}

/// The file selection index at a list-item position (None for a header row).
pub fn selection_at(status: &Status, item: usize) -> Option<usize> {
    match rows(status).get(item) {
        Some(Row::File { selection, .. }) => Some(*selection),
        _ => None,
    }
}

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

    let mut items = Vec::new();
    let mut selected_item = None;
    for (index, row) in rows(&app.status).into_iter().enumerate() {
        match row {
            Row::Header { label, count } => items.push(section_header(label, count, theme)),
            Row::File {
                selection,
                section,
                entry,
            } => {
                if selection == app.selected {
                    selected_item = Some(index);
                }
                items.push(file_item(entry, section, theme));
            }
        }
    }

    let highlight = if focused {
        Style::new()
            .bg(theme.selection_bg)
            .fg(theme.selection_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().bg(theme.selection_bg)
    };
    let list = List::new(items).highlight_style(highlight);

    let mut state = app.staging_state_mut();
    state.select(selected_item);
    frame.render_stateful_widget(list, inner, &mut state);
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
