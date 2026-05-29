use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::git::{Change, FileEntry, Repo, Section, Status};
use crate::ui::theme::Theme;

/// A path-based git mutation (stage / unstage); lets the select → run → refresh
/// flow be shared via `run_on_selected`.
type GitOp = fn(&Repo, &str) -> anyhow::Result<()>;

/// Which pane currently receives keyboard input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Staging,
    Diff,
}

/// A blocking overlay that captures input until dismissed.
#[derive(Clone, Debug)]
pub enum Modal {
    /// Confirm discarding a file's changes (or deleting an untracked file).
    ConfirmDiscard {
        path: String,
        change: Change,
        label: String,
    },
}

/// Global application state. A single `App` drives both rendering and input
/// dispatch: the event loop reads an event, calls [`App::on_key`], then redraws
/// from the updated state.
pub struct App {
    pub repo: Repo,
    pub status: Status,
    /// Index into the flattened file list (staged entries first, then unstaged).
    pub selected: usize,
    pub focus: Focus,
    pub modal: Option<Modal>,
    pub theme: Theme,
    pub should_quit: bool,
}

impl App {
    pub fn new(repo_path: PathBuf) -> anyhow::Result<Self> {
        let repo = Repo::open(&repo_path)?;
        let status = repo.status()?;
        let mut app = App {
            repo,
            status,
            selected: 0,
            focus: Focus::Staging,
            modal: None,
            theme: Theme::default(),
            should_quit: false,
        };
        app.clamp_selection();
        Ok(app)
    }

    /// Short display name for the repository (its working-tree directory name).
    pub fn repo_name(&self) -> String {
        let workdir = self.repo.workdir();
        workdir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| workdir.to_string_lossy().into_owned())
    }

    /// The file under the cursor, with the section it belongs to.
    pub fn selected_file(&self) -> Option<(Section, &FileEntry)> {
        let staged = &self.status.staged;
        if self.selected < staged.len() {
            Some((Section::Staged, &staged[self.selected]))
        } else {
            self.status
                .unstaged
                .get(self.selected - staged.len())
                .map(|entry| (Section::Unstaged, entry))
        }
    }

    /// Re-read status from disk, keeping the selection in bounds.
    pub fn refresh(&mut self) {
        match self.repo.status() {
            Ok(status) => {
                self.status = status;
                self.clamp_selection();
            }
            Err(err) => tracing::warn!("status refresh failed: {err:#}"),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits, even with a modal open.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }
        if self.modal.is_some() {
            self.on_key_modal(key);
            return;
        }

        // Global keys, regardless of focus.
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.toggle_focus();
                return;
            }
            KeyCode::Char('r') => {
                self.refresh();
                return;
            }
            _ => {}
        }

        match self.focus {
            Focus::Staging => self.on_key_staging(key),
            Focus::Diff => self.on_key_diff(key),
        }
    }

    fn on_key_staging(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.selected = self.status.total().saturating_sub(1)
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle_stage(),
            KeyCode::Char('s') => self.stage_selected(),
            KeyCode::Char('u') => self.unstage_selected(),
            KeyCode::Char('x') => self.request_discard(),
            KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Diff,
            _ => {}
        }
    }

    fn on_key_diff(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Char('h') | KeyCode::Left) {
            self.focus = Focus::Staging;
        }
    }

    fn on_key_modal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => self.confirm_modal(),
            KeyCode::Char('n') | KeyCode::Esc => self.modal = None,
            _ => {}
        }
    }

    /// Stage an unstaged file, or unstage a staged one.
    fn toggle_stage(&mut self) {
        let Some((section, _)) = self.selected_section_path() else {
            return;
        };
        let op: GitOp = match section {
            Section::Staged => Repo::unstage,
            Section::Unstaged => Repo::stage,
        };
        self.run_on_selected("toggle stage", op);
    }

    fn stage_selected(&mut self) {
        self.run_on_selected("stage", Repo::stage);
    }

    fn unstage_selected(&mut self) {
        self.run_on_selected("unstage", Repo::unstage);
    }

    /// Run a path-based git op on the selected file, then refresh.
    fn run_on_selected(&mut self, action: &str, op: GitOp) {
        let Some((_, path)) = self.selected_section_path() else {
            return;
        };
        self.after_mutation(action, op(&self.repo, &path));
    }

    /// Open the discard confirmation for the selected file.
    fn request_discard(&mut self) {
        self.modal = self
            .selected_file()
            .map(|(_, entry)| Modal::ConfirmDiscard {
                path: entry.path.clone(),
                change: entry.change,
                label: entry.display_path(),
            });
    }

    fn confirm_modal(&mut self) {
        if let Some(Modal::ConfirmDiscard { path, change, .. }) = self.modal.take() {
            let result = self.repo.discard(&path, change);
            self.after_mutation("discard", result);
        }
    }

    /// Log any failure, then refresh status so the UI reflects the result.
    fn after_mutation(&mut self, action: &str, result: anyhow::Result<()>) {
        if let Err(err) = result {
            tracing::warn!("{action} failed: {err:#}");
        }
        self.refresh();
    }

    fn selected_section_path(&self) -> Option<(Section, String)> {
        self.selected_file()
            .map(|(section, entry)| (section, entry.path.clone()))
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Staging => Focus::Diff,
            Focus::Diff => Focus::Staging,
        };
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1).min(self.status.total().saturating_sub(1));
    }

    fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.status.total().saturating_sub(1));
    }
}
