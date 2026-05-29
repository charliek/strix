use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::ui::{centered_line, panel_block};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.focus == Focus::Diff;
    let block = panel_block(" Diff ", focused, app);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let hint = Paragraph::new("Select a file to view its diff")
        .style(Style::new().fg(theme.dim))
        .alignment(Alignment::Center);
    frame.render_widget(hint, centered_line(inner));
}
