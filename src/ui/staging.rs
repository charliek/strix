use ratatui::layout::Rect;
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::ui::panel_block;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Staging;
    frame.render_widget(panel_block(" Changes ", focused, app), area);
}
