//! Review view rendering: a flat list of the files that differ across the range
//! (no section headers) beside the shared diff pane. The list reuses the
//! committed-files pattern from the history view — a change marker, the display
//! path, and `+n −m` line stats — and the vertical split bar is shared wholesale
//! with the status view.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem};
use ratatui::Frame;

use crate::app::App;
use crate::ui::{centered_hint, diff_view, file_stat_spans, panel_block, selection_style};

pub fn render(frame: &mut Frame, body: Rect, app: &App) {
    let theme = &app.theme;

    // `b` collapses the file list, like the status view. Hidden: the diff pane
    // fills the body; clear the stale geometry so mouse hit-testing can't match
    // where the list used to be.
    if !app.show_changes {
        app.set_review_list_area(Rect::default());
        diff_view::render(frame, body, app);
        return;
    }

    // The list is a fixed width (shared with the status view); the diff takes the
    // rest. Reuse the vertical split bar wholesale.
    let width = app.changes_pane_width(body.width);
    let [left, right] =
        Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(body);
    app.set_split_geometry(body, right.x);

    render_files(frame, left, app);
    diff_view::render(frame, right, app);

    if app.divider_engaged() {
        super::highlight_divider(frame, body, right.x, theme);
    }
}

fn render_files(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.review_list_focused();
    let block = panel_block(" Changes ", focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.set_review_list_area(inner);

    let files = app.review_files();
    if files.is_empty() {
        centered_hint(
            frame,
            inner,
            "No differences in range",
            Style::new().fg(theme.dim),
        );
        return;
    }

    let items: Vec<ListItem> = files
        .iter()
        .map(|file| {
            let mut spans = file_stat_spans(file, theme);
            // Review-only `● n` comment badge (count includes orphans).
            let count = app.review_comment_count(&file.path);
            if count > 0 {
                spans.push(Span::styled(
                    format!("  ● {count}"),
                    Style::new().fg(theme.comment),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items).highlight_style(selection_style(focused, theme));
    let mut state = app.review_list_state_mut();
    state.select(Some(app.review_selected()));
    frame.render_stateful_widget(list, inner, &mut state);
}
