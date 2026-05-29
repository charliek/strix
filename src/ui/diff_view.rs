use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::ui::{panel_block, vertical_center};

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

    // The diff itself arrives in M3; until a file is selected, show a hint.
    if selected.is_none() {
        let hint = Paragraph::new("Select a file to view its diff")
            .style(Style::new().fg(theme.dim))
            .alignment(Alignment::Center);
        frame.render_widget(hint, vertical_center(inner, 1));
    }
}
