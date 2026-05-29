use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::theme::Theme;

/// Which pane currently receives keyboard input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Staging,
    Diff,
}

/// Global application state. A single `App` drives both rendering and input
/// dispatch: the event loop reads an event, calls [`App::on_key`], then redraws
/// from the updated state.
pub struct App {
    pub repo_path: PathBuf,
    pub focus: Focus,
    pub theme: Theme,
    pub should_quit: bool,
}

impl App {
    pub fn new(repo_path: PathBuf) -> anyhow::Result<Self> {
        Ok(App {
            repo_path,
            focus: Focus::Staging,
            theme: Theme::default(),
            should_quit: false,
        })
    }

    /// Short display name for the repository (its directory name).
    pub fn repo_name(&self) -> String {
        self.repo_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.repo_path.to_string_lossy().into_owned())
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Staging => Focus::Diff,
            Focus::Diff => Focus::Staging,
        };
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),
            _ => {}
        }
    }
}
