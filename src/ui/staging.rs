use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::git::{Change, FileEntry, Section};
use crate::ui::theme::Theme;
use crate::ui::{panel_block, vertical_center};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.focus == Focus::Staging;
    let block = panel_block(" Changes ", focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.status.is_clean() {
        let hint = Paragraph::new("✓ working tree clean")
            .style(Style::new().fg(theme.staged))
            .alignment(Alignment::Center);
        frame.render_widget(hint, vertical_center(inner, 1));
        return;
    }

    // Build the list with section headers interleaved, tracking which item
    // index each selectable file lands on so the cursor never sits on a header.
    let mut items: Vec<ListItem> = Vec::new();
    let mut file_rows: Vec<usize> = Vec::new();

    if !app.status.staged.is_empty() {
        items.push(section_header("Staged", app.status.staged.len(), theme));
        for entry in &app.status.staged {
            file_rows.push(items.len());
            items.push(file_item(entry, Section::Staged, theme));
        }
    }
    if !app.status.unstaged.is_empty() {
        items.push(section_header("Changes", app.status.unstaged.len(), theme));
        for entry in &app.status.unstaged {
            file_rows.push(items.len());
            items.push(file_item(entry, Section::Unstaged, theme));
        }
    }

    let mut state = ListState::default();
    state.select(file_rows.get(app.selected).copied());

    let highlight = if focused {
        Style::new()
            .bg(theme.selection_bg)
            .fg(theme.selection_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().bg(theme.selection_bg)
    };
    let list = List::new(items).highlight_style(highlight);
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
