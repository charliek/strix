use std::cell::Cell;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::git::{Change, FileDiff, FileEntry, Repo, Section, Status};
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

    /// Cached diff for the selected file; recomputed only when the selection
    /// changes (see `sync_diff`).
    pub current_diff: Option<FileDiff>,
    diff_key: Option<(Section, String)>,
    pub diff_scroll: u16,
    /// Inner height of the diff pane from the last render, so scrolling can
    /// clamp to the content. Interior-mutable because rendering takes `&App`.
    diff_viewport: Cell<u16>,
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
            current_diff: None,
            diff_key: None,
            diff_scroll: 0,
            diff_viewport: Cell::new(0),
        };
        app.clamp_selection();
        app.sync_diff();
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
        } else {
            match key.code {
                KeyCode::Char('q') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),
                KeyCode::Char('r') => self.refresh(),
                _ => match self.focus {
                    Focus::Staging => self.on_key_staging(key),
                    Focus::Diff => self.on_key_diff(key),
                },
            }
        }

        // A handled key may have moved the selection or changed status; keep the
        // cached diff in sync.
        self.sync_diff();
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
        let max = self.diff_max_scroll();
        let page = (self.diff_viewport.get() / 2).max(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('d') if ctrl => self.diff_scroll = (self.diff_scroll + page).min(max),
            KeyCode::Char('u') if ctrl => self.diff_scroll = self.diff_scroll.saturating_sub(page),
            KeyCode::Char('j') | KeyCode::Down => {
                self.diff_scroll = (self.diff_scroll + 1).min(max)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.diff_scroll = self.diff_scroll.saturating_sub(1)
            }
            KeyCode::Char('g') | KeyCode::Home => self.diff_scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.diff_scroll = max,
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Staging,
            _ => {}
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

    /// Recompute the cached diff when the selected file changes, resetting scroll.
    fn sync_diff(&mut self) {
        let key = self
            .selected_file()
            .map(|(section, entry)| (section, entry.path.clone()));
        if key == self.diff_key {
            return;
        }
        // Compute into a local first so the immutable borrow of the file list
        // (and repo) is released before assigning the cached fields.
        let diff = self
            .selected_file()
            .map(|(section, entry)| self.repo.diff(section, entry));
        self.current_diff = diff;
        self.diff_key = key;
        self.diff_scroll = 0;
    }

    fn diff_len(&self) -> u16 {
        match &self.current_diff {
            Some(FileDiff::Text(lines)) => lines.len().try_into().unwrap_or(u16::MAX),
            _ => 0,
        }
    }

    /// Largest valid diff scroll offset, given the last-rendered viewport height.
    pub fn diff_max_scroll(&self) -> u16 {
        self.diff_len().saturating_sub(self.diff_viewport.get())
    }

    /// Record the diff pane's inner height (called while rendering).
    pub fn set_diff_viewport(&self, height: u16) {
        self.diff_viewport.set(height);
    }
}
