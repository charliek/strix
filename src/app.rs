use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use ratatui::widgets::ListState;
use syntect::parsing::SyntaxReference;

use crate::config::Config;
use crate::git::{
    Change, CommitFile, CommitInfo, DiffLine, FileDiff, FileEntry, LineKind, RefLabel, Repo,
    Section, Status,
};
use crate::graph::{self, GraphRow};
use crate::keys::{Action, Keymap};
use crate::ui::theme::Theme;

/// A path-based git mutation (stage / unstage); lets the select → run → refresh
/// flow be shared via `run_on_selected`.
type GitOp = fn(&Repo, &str) -> anyhow::Result<()>;

/// Columns at the start of a staging row (the change marker) where a click
/// toggles staging rather than only selecting.
const MARKER_ZONE: u16 = 4;
/// Lines scrolled per mouse-wheel notch in the diff pane.
const SCROLL_STEP: u16 = 3;
/// Default width (columns) of the Changes panel. It is a fixed width, not a
/// percentage, so widening the terminal grows the diff rather than this panel.
const DEFAULT_CHANGES_WIDTH: u16 = 32;
/// Minimum columns each pane keeps when the split bar is dragged, so neither
/// the Changes list nor the diff can be collapsed to nothing.
const MIN_CHANGES_WIDTH: u16 = 16;
const MIN_DIFF_WIDTH: u16 = 24;
/// History view: default + minimum heights (rows) for the two stacked left
/// sub-panes, and how many commits to load per page.
const DEFAULT_COMMITTED_HEIGHT: u16 = 12;
const MIN_COMMITTED_HEIGHT: u16 = 4;
const MIN_GRAPH_HEIGHT: u16 = 4;
const HISTORY_PAGE: usize = 500;

/// Which pane currently receives keyboard input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Staging,
    Diff,
}

/// How the diff pane renders a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffMode {
    Unified,
    SideBySide,
}

/// Which top-level view is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Status,
    History,
}

/// Which sub-pane of the history view receives keyboard input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryFocus {
    Graph,
    CommittedChanges,
    Diff,
}

/// One syntax-highlighted line: `(foreground colour, text)` segments, shared
/// (`Rc`) so the per-file cache can hand out cheap clones each frame.
type HighlightedLine = Rc<[(Color, String)]>;

/// A side-by-side row, referencing diff lines by index into the file's
/// `Vec<DiffLine>`. Indices (not borrows) let the row layout be computed once
/// per file and cached on `App` without a self-referential borrow.
#[derive(Clone, Copy)]
pub enum SbsRow {
    Hunk(usize),
    Pair {
        left: Option<usize>,
        right: Option<usize>,
    },
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
    /// The keybinding help overlay.
    Help,
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
    /// Whether the left Changes panel is visible. When hidden, the diff pane
    /// fills the body and focus is forced to the diff (see `toggle_changes`).
    pub show_changes: bool,
    /// Width (columns) of the Changes panel, adjusted by dragging the split bar.
    /// Fixed rather than proportional, so a wider terminal feeds the diff.
    pub changes_width: u16,
    /// True while the split bar is held with the left mouse button.
    dragging_divider: bool,
    /// True while the mouse hovers the split bar (free movement, no button),
    /// used to highlight it and request a resize cursor.
    hovering_divider: bool,
    pub modal: Option<Modal>,
    pub theme: Theme,
    pub should_quit: bool,
    /// A transient error from the last action, shown until the next keypress.
    pub last_error: Option<String>,

    /// Cached diff for the selected file; recomputed only when the selection
    /// changes (see `sync_diff`).
    pub current_diff: Option<FileDiff>,
    diff_key: Option<(Section, String)>,
    /// Set when an external refresh should recompute the open file's diff even
    /// though its `(section, path)` is unchanged (its content may have changed).
    /// Unlike navigating to a new file, this preserves the scroll position.
    diff_dirty: bool,
    pub diff_mode: DiffMode,
    pub diff_scroll: u16,
    /// Inner height + total content rows of the diff pane from the last render,
    /// so scrolling can clamp to the content in either mode. Interior-mutable
    /// because rendering takes `&App`.
    diff_viewport: Cell<u16>,
    diff_content_rows: Cell<u16>,
    /// Per-file caches that make scrolling cheap: syntax-highlighted lines keyed
    /// by their (sanitised) text, and the side-by-side row layout. Both are
    /// cleared whenever `sync_diff` recomputes `current_diff`, so they never
    /// outlive the file they describe. Interior-mutable because rendering, which
    /// fills them, takes `&App`.
    highlight_cache: RefCell<HashMap<String, HighlightedLine>>,
    sbs_rows: RefCell<Option<Vec<SbsRow>>>,

    /// Persisted so the staging list's scroll offset survives between frames
    /// and can be read for mouse hit-testing. The pane rects are recorded
    /// during rendering for the same reason.
    staging_state: RefCell<ListState>,
    staging_area: Cell<Rect>,
    diff_area: Cell<Rect>,
    /// Body rect and split-bar column from the last render, for hit-testing a
    /// drag on the divider. Recorded during rendering, like the pane rects.
    body_area: Cell<Rect>,
    divider_x: Cell<u16>,

    // --- History view ---
    pub view: ViewMode,
    history_focus: HistoryFocus,
    commits: Vec<CommitInfo>,
    refs: Vec<RefLabel>,
    graph_rows: Vec<GraphRow>,
    /// True once a walk returned fewer commits than requested — no more to load.
    history_loaded_all: bool,
    selected_commit: usize,
    commit_files: Vec<CommitFile>,
    /// Row in the top "Committed Changes" list: 0 is the commit (`●`) row,
    /// `1..=commit_files.len()` index into `commit_files`.
    committed_row: usize,
    history_diff: Option<FileDiff>,
    history_diff_key: Option<(gix::ObjectId, String)>,
    /// Height (rows) of the top "Committed Changes" sub-pane; the Graph fills the
    /// rest. Adjusted by dragging the horizontal divider. Mirrors `changes_width`.
    committed_height: u16,
    dragging_hdivider: bool,
    hovering_hdivider: bool,
    committed_area: Cell<Rect>,
    graph_area: Cell<Rect>,
    /// The left column's body rect and the horizontal divider row, recorded
    /// during rendering for hit-testing a drag (mirrors `body_area`/`divider_x`).
    left_col_area: Cell<Rect>,
    hdivider_y: Cell<u16>,
    committed_state: RefCell<ListState>,
    graph_state: RefCell<ListState>,

    keymap: Keymap,
}

impl App {
    pub fn new(repo_path: PathBuf) -> anyhow::Result<Self> {
        Self::with_config(repo_path, &Config::default())
    }

    pub fn with_config(repo_path: PathBuf, config: &Config) -> anyhow::Result<Self> {
        let repo = Repo::open(&repo_path)?;
        let status = repo.status()?;
        let theme = Theme::load(
            config.theme.as_deref().unwrap_or("tokyo-night"),
            crate::config::config_dir().as_deref(),
        );
        let mut app = App {
            repo,
            status,
            selected: 0,
            focus: Focus::Staging,
            show_changes: true,
            changes_width: DEFAULT_CHANGES_WIDTH,
            dragging_divider: false,
            hovering_divider: false,
            modal: None,
            theme,
            should_quit: false,
            last_error: None,
            current_diff: None,
            diff_key: None,
            diff_dirty: false,
            diff_mode: config.diff_mode(),
            diff_scroll: 0,
            diff_viewport: Cell::new(0),
            diff_content_rows: Cell::new(0),
            highlight_cache: RefCell::new(HashMap::new()),
            sbs_rows: RefCell::new(None),
            staging_state: RefCell::new(ListState::default()),
            staging_area: Cell::new(Rect::default()),
            diff_area: Cell::new(Rect::default()),
            body_area: Cell::new(Rect::default()),
            divider_x: Cell::new(0),
            view: ViewMode::Status,
            history_focus: HistoryFocus::Graph,
            commits: Vec::new(),
            refs: Vec::new(),
            graph_rows: Vec::new(),
            history_loaded_all: false,
            selected_commit: 0,
            commit_files: Vec::new(),
            committed_row: 0,
            history_diff: None,
            history_diff_key: None,
            committed_height: DEFAULT_COMMITTED_HEIGHT,
            dragging_hdivider: false,
            hovering_hdivider: false,
            committed_area: Cell::new(Rect::default()),
            graph_area: Cell::new(Rect::default()),
            left_col_area: Cell::new(Rect::default()),
            hdivider_y: Cell::new(0),
            committed_state: RefCell::new(ListState::default()),
            graph_state: RefCell::new(ListState::default()),
            keymap: Keymap::from_config(config.keys.as_ref()),
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

    /// Re-read status from disk, keeping the cursor on the same file (matched by
    /// section and path) when it survives, and forcing the open diff to
    /// recompute — its content may have changed in place even if its path did not.
    pub fn refresh(&mut self) {
        let previous = self.selected_section_path();
        match self.repo.status() {
            Ok(status) => {
                self.status = status;
                match previous.and_then(|(section, path)| self.index_of(section, &path)) {
                    Some(index) => self.selected = index,
                    None => self.clamp_selection(),
                }
                // The open file's content may have changed in place; mark the
                // diff dirty so `sync_diff` recomputes it (but, unlike a file
                // change, keeps the scroll position).
                self.diff_dirty = true;
            }
            Err(err) => tracing::warn!("status refresh failed: {err:#}"),
        }
    }

    /// Re-read status and recompute the diff in one step. Used by the file
    /// watcher, whose path has no trailing `sync_diff` like `on_key`/`on_mouse`.
    pub fn reload(&mut self) {
        self.refresh();
        self.sync_diff();
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits, even with a modal open.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }
        self.last_error = None;

        if self.modal.is_some() {
            self.on_key_modal(key);
        } else if key.code == KeyCode::Esc {
            // Esc leaves the history view; it is not in the keymap (so the modal's
            // own Esc handling stays first). A no-op in the status view.
            if self.view == ViewMode::History {
                self.exit_history();
            }
        } else if let Some(action) = self.keymap.action(key) {
            self.dispatch(action);
        }

        // A handled key may have moved the selection or changed status; keep the
        // active view's cached diff in sync.
        self.sync_active();
    }

    /// Interpret an action in context: navigation keys move the file cursor in
    /// the staging pane but scroll the diff pane; staging ops act on the
    /// selected file regardless of focus.
    fn dispatch(&mut self, action: Action) {
        // View-agnostic actions are handled the same in either view.
        match action {
            Action::Quit => {
                self.should_quit = true;
                return;
            }
            Action::Help => {
                self.modal = Some(Modal::Help);
                return;
            }
            Action::ToggleDiffMode => {
                self.toggle_diff_mode();
                return;
            }
            Action::Refresh => {
                self.refresh_active();
                return;
            }
            Action::ToggleHistory => {
                self.toggle_history();
                return;
            }
            Action::ShowStatus => {
                if self.view == ViewMode::History {
                    self.exit_history();
                }
                return;
            }
            Action::ShowHistory => {
                if self.view == ViewMode::Status {
                    self.enter_history();
                }
                return;
            }
            _ => {}
        }
        match self.view {
            ViewMode::Status => self.dispatch_status(action),
            ViewMode::History => self.dispatch_history(action),
        }
    }

    /// Interpret a navigation/staging action in the status view: navigation keys
    /// move the file cursor in the staging pane but scroll the diff pane; staging
    /// ops act on the selected file regardless of focus.
    fn dispatch_status(&mut self, action: Action) {
        match action {
            Action::SwitchPane => {
                if self.show_changes {
                    self.toggle_focus();
                } else {
                    self.reveal_changes(); // Tab reveals a hidden panel and lands in it.
                }
            }
            Action::ToggleChanges => self.toggle_changes(),
            // Focusing a hidden panel reveals it first.
            Action::FocusStaging => self.reveal_changes(),
            Action::FocusDiff => self.focus = Focus::Diff,
            Action::Down => match self.focus {
                Focus::Staging => self.select_next(),
                Focus::Diff => self.scroll_diff(true, 1),
            },
            Action::Up => match self.focus {
                Focus::Staging => self.select_prev(),
                Focus::Diff => self.scroll_diff(false, 1),
            },
            Action::Top => match self.focus {
                Focus::Staging => self.selected = 0,
                Focus::Diff => self.diff_scroll = 0,
            },
            Action::Bottom => match self.focus {
                Focus::Staging => self.selected = self.status.total().saturating_sub(1),
                Focus::Diff => self.diff_scroll = self.diff_max_scroll(),
            },
            // Ctrl-D/U page the diff pane regardless of which pane is focused.
            Action::HalfPageDown => self.scroll_diff(true, self.half_page()),
            Action::HalfPageUp => self.scroll_diff(false, self.half_page()),
            Action::ToggleStage => self.toggle_stage(),
            Action::Stage => self.stage_selected(),
            Action::Unstage => self.unstage_selected(),
            Action::Discard => self.request_discard(),
            // Handled in `dispatch`.
            Action::Quit
            | Action::Help
            | Action::Refresh
            | Action::ToggleDiffMode
            | Action::ToggleHistory
            | Action::ShowStatus
            | Action::ShowHistory => {}
        }
    }

    /// Interpret a navigation action in the history view: it acts on whichever
    /// sub-pane (Graph / Committed changes / Diff) currently has focus. The view
    /// is read-only, so staging ops do nothing.
    fn dispatch_history(&mut self, action: Action) {
        match action {
            Action::SwitchPane => self.cycle_history_focus(),
            Action::FocusStaging => self.history_focus_left(),
            Action::FocusDiff => self.history_focus_right(),
            Action::Down => self.history_move(true),
            Action::Up => self.history_move(false),
            Action::Top => self.history_to_edge(false),
            Action::Bottom => self.history_to_edge(true),
            Action::HalfPageDown => self.scroll_diff(true, self.half_page()),
            Action::HalfPageUp => self.scroll_diff(false, self.half_page()),
            // Read-only view: the changes toggle and staging ops do nothing.
            Action::ToggleChanges
            | Action::ToggleStage
            | Action::Stage
            | Action::Unstage
            | Action::Discard => {}
            // Handled in `dispatch`.
            Action::Quit
            | Action::Help
            | Action::Refresh
            | Action::ToggleDiffMode
            | Action::ToggleHistory
            | Action::ShowStatus
            | Action::ShowHistory => {}
        }
    }

    fn half_page(&self) -> u16 {
        (self.diff_viewport.get() / 2).max(1)
    }

    // --- History view: enter / exit / load ---

    fn toggle_history(&mut self) {
        match self.view {
            ViewMode::Status => self.enter_history(),
            ViewMode::History => self.exit_history(),
        }
    }

    fn enter_history(&mut self) {
        if self.commits.is_empty() {
            self.load_history();
        }
        self.view = ViewMode::History;
        self.history_focus = HistoryFocus::Graph;
        self.selected_commit = 0;
        self.committed_row = 0;
        self.diff_scroll = 0;
        self.load_commit_files();
        self.sync_history_diff();
    }

    fn exit_history(&mut self) {
        self.view = ViewMode::Status;
        self.focus = Focus::Staging;
        self.diff_scroll = 0;
        // Drop the per-file render caches: the status diff describes a different
        // file than the history view left behind.
        self.highlight_cache.borrow_mut().clear();
        *self.sbs_rows.borrow_mut() = None;
        self.sync_diff();
    }

    /// Load (or reload) the commit walk + refs + graph layout, leaving `commits`
    /// empty on an empty repo or error (the UI renders an empty-state hint).
    fn load_history(&mut self) {
        match self.repo.history(HISTORY_PAGE) {
            Ok(commits) => {
                self.history_loaded_all = commits.len() < HISTORY_PAGE;
                self.commits = commits;
            }
            Err(err) => {
                tracing::warn!("history walk failed: {err:#}");
                self.commits.clear();
                self.history_loaded_all = true;
            }
        }
        self.refs = self.repo.ref_labels().unwrap_or_default();
        self.graph_rows = graph::layout(&self.commits, &self.refs);
    }

    /// Load the selected commit's changed-file list, resetting the top-pane
    /// selection to the commit (`●`) row.
    fn load_commit_files(&mut self) {
        self.committed_row = 0;
        self.committed_state.borrow_mut().select(None);
        let Some(commit) = self.commits.get(self.selected_commit) else {
            self.commit_files.clear();
            return;
        };
        self.commit_files = self.repo.commit_files(commit).unwrap_or_else(|err| {
            tracing::warn!("listing commit files failed: {err:#}");
            Vec::new()
        });
    }

    /// Pull in the next page of history when the Graph selection reaches the end
    /// of what's loaded, preserving the selected commit.
    fn load_more_history(&mut self) {
        if self.history_loaded_all {
            return;
        }
        let want = self.commits.len() + HISTORY_PAGE;
        match self.repo.history(want) {
            Ok(commits) => {
                self.history_loaded_all = commits.len() < want;
                self.commits = commits;
                self.graph_rows = graph::layout(&self.commits, &self.refs);
            }
            Err(err) => {
                tracing::warn!("loading more history failed: {err:#}");
                self.history_loaded_all = true;
            }
        }
    }

    // --- History view: navigation ---

    fn cycle_history_focus(&mut self) {
        self.history_focus = match self.history_focus {
            HistoryFocus::Graph => HistoryFocus::CommittedChanges,
            HistoryFocus::CommittedChanges => HistoryFocus::Diff,
            HistoryFocus::Diff => HistoryFocus::Graph,
        };
    }

    fn history_focus_left(&mut self) {
        self.history_focus = match self.history_focus {
            HistoryFocus::Diff => HistoryFocus::CommittedChanges,
            HistoryFocus::CommittedChanges | HistoryFocus::Graph => HistoryFocus::Graph,
        };
    }

    fn history_focus_right(&mut self) {
        self.history_focus = match self.history_focus {
            HistoryFocus::Graph => HistoryFocus::CommittedChanges,
            HistoryFocus::CommittedChanges | HistoryFocus::Diff => HistoryFocus::Diff,
        };
    }

    fn history_move(&mut self, down: bool) {
        match self.history_focus {
            HistoryFocus::Graph => {
                if down {
                    self.select_commit_next();
                } else {
                    self.select_commit_prev();
                }
            }
            HistoryFocus::CommittedChanges => {
                if down {
                    self.committed_row = (self.committed_row + 1).min(self.commit_files.len());
                } else {
                    self.committed_row = self.committed_row.saturating_sub(1);
                }
            }
            HistoryFocus::Diff => self.scroll_diff(down, 1),
        }
    }

    fn history_to_edge(&mut self, bottom: bool) {
        match self.history_focus {
            HistoryFocus::Graph => {
                self.selected_commit = if bottom {
                    self.commits.len().saturating_sub(1)
                } else {
                    0
                };
                self.load_commit_files();
            }
            HistoryFocus::CommittedChanges => {
                self.committed_row = if bottom { self.commit_files.len() } else { 0 };
            }
            HistoryFocus::Diff => {
                self.diff_scroll = if bottom { self.diff_max_scroll() } else { 0 };
            }
        }
    }

    fn select_commit_next(&mut self) {
        if self.selected_commit + 1 >= self.commits.len() {
            self.load_more_history();
        }
        self.selected_commit =
            (self.selected_commit + 1).min(self.commits.len().saturating_sub(1));
        self.load_commit_files();
    }

    fn select_commit_prev(&mut self) {
        self.selected_commit = self.selected_commit.saturating_sub(1);
        self.load_commit_files();
    }

    /// Handle a mouse event; returns whether the frame should be redrawn.
    pub fn on_mouse(&mut self, event: MouseEvent) -> bool {
        // A modal captures all input, including the mouse.
        if self.modal.is_some() {
            return false;
        }
        let pos = Position {
            x: event.column,
            y: event.row,
        };

        // Free movement (no button held) only updates the hover affordance: it
        // must not clear the error toast or recompute the diff, and it redraws
        // only when the highlighted state actually changes.
        if let MouseEventKind::Moved = event.kind {
            let was = self.hovering_divider || self.hovering_hdivider;
            self.hovering_divider = self.on_divider(pos);
            self.hovering_hdivider = self.on_hdivider(pos);
            return (self.hovering_divider || self.hovering_hdivider) != was;
        }

        self.last_error = None;
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Grabbing a split bar starts a resize instead of a pane click.
                if self.on_divider(pos) {
                    self.dragging_divider = true;
                } else if self.on_hdivider(pos) {
                    self.dragging_hdivider = true;
                } else {
                    self.on_click(pos);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_divider => {
                self.resize_changes(pos)
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_hdivider => {
                self.resize_committed(pos)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging_divider = false;
                self.dragging_hdivider = false;
            }
            MouseEventKind::ScrollDown => self.on_scroll(pos, true),
            MouseEventKind::ScrollUp => self.on_scroll(pos, false),
            _ => {}
        }
        self.sync_active();
        true
    }

    /// Which pane (if any) a screen position falls in.
    fn pane_at(&self, pos: Position) -> Option<Focus> {
        if self.diff_area.get().contains(pos) {
            Some(Focus::Diff)
        } else if self.staging_area.get().contains(pos) {
            Some(Focus::Staging)
        } else {
            None
        }
    }

    fn on_click(&mut self, pos: Position) {
        if self.view == ViewMode::History {
            self.history_click(pos);
            return;
        }
        match self.pane_at(pos) {
            Some(Focus::Diff) => self.focus = Focus::Diff,
            Some(Focus::Staging) => {
                self.focus = Focus::Staging;
                if let Some(selection) = self.file_at(pos) {
                    self.selected = selection;
                    // Clicking the change marker (not just the name) toggles staging.
                    if pos.x < self.staging_area.get().x + MARKER_ZONE {
                        self.toggle_stage();
                    }
                }
            }
            None => {}
        }
    }

    /// Route a click in the history view to its sub-pane: the Graph selects a
    /// commit, the Committed Changes list selects the commit row or a file, the
    /// diff pane just takes focus.
    fn history_click(&mut self, pos: Position) {
        let graph = self.graph_area.get();
        let committed = self.committed_area.get();
        if graph.contains(pos) {
            self.history_focus = HistoryFocus::Graph;
            let row = self.graph_state.borrow().offset() + (pos.y - graph.y) as usize;
            if row < self.commits.len() {
                self.selected_commit = row;
                self.load_commit_files();
            }
        } else if committed.contains(pos) {
            self.history_focus = HistoryFocus::CommittedChanges;
            let row = self.committed_state.borrow().offset() + (pos.y - committed.y) as usize;
            // Row 0 is the commit (●) row; rows below index into the file list.
            self.committed_row = row.min(self.commit_files.len());
        } else if self.diff_area.get().contains(pos) {
            self.history_focus = HistoryFocus::Diff;
        }
    }

    /// Whether a position lands on the split bar — its two border columns —
    /// while the Changes panel is shown.
    fn on_divider(&self, pos: Position) -> bool {
        if !self.show_changes {
            return false;
        }
        let body = self.body_area.get();
        let dx = self.divider_x.get();
        let on_body_row = pos.y >= body.y && pos.y < body.y.saturating_add(body.height);
        on_body_row && (pos.x == dx || pos.x.saturating_add(1) == dx)
    }

    /// Whether a position lands on the history view's horizontal split bar — its
    /// two border rows — within the left column.
    fn on_hdivider(&self, pos: Position) -> bool {
        if self.view != ViewMode::History {
            return false;
        }
        let left = self.left_col_area.get();
        let dy = self.hdivider_y.get();
        let on_left_col = pos.x >= left.x && pos.x < left.x.saturating_add(left.width);
        on_left_col && (pos.y == dy || pos.y.saturating_add(1) == dy)
    }

    /// Whether either split bar should show its active affordance (highlight +
    /// resize pointer): the mouse hovers it, or a drag is in progress.
    pub fn divider_engaged(&self) -> bool {
        self.hovering_divider
            || self.dragging_divider
            || self.hovering_hdivider
            || self.dragging_hdivider
    }

    /// Move the split bar so the Changes panel's right edge follows the cursor,
    /// clamped so both panes keep a usable width.
    fn resize_changes(&mut self, pos: Position) {
        let body = self.body_area.get();
        self.changes_width = pos.x.saturating_sub(body.x);
        self.changes_width = self.changes_pane_width(body.width);
    }

    /// Move the horizontal split bar so the Committed Changes sub-pane's bottom
    /// edge follows the cursor, clamped so both sub-panes keep a usable height.
    fn resize_committed(&mut self, pos: Position) {
        let left = self.left_col_area.get();
        self.committed_height = pos.y.saturating_sub(left.y);
        self.committed_height = self.committed_pane_height(left.height);
    }

    fn on_scroll(&mut self, pos: Position, down: bool) {
        if self.view == ViewMode::History {
            self.history_scroll(pos, down);
            return;
        }
        match self.pane_at(pos) {
            Some(Focus::Diff) => self.scroll_diff(down, SCROLL_STEP),
            Some(Focus::Staging) if down => self.select_next(),
            Some(Focus::Staging) => self.select_prev(),
            None => {}
        }
    }

    fn history_scroll(&mut self, pos: Position, down: bool) {
        if self.graph_area.get().contains(pos) {
            self.history_focus = HistoryFocus::Graph;
            if down {
                self.select_commit_next();
            } else {
                self.select_commit_prev();
            }
        } else if self.committed_area.get().contains(pos) {
            self.history_focus = HistoryFocus::CommittedChanges;
            self.history_move(down);
        } else if self.diff_area.get().contains(pos) {
            self.scroll_diff(down, SCROLL_STEP);
        }
    }

    fn scroll_diff(&mut self, down: bool, step: u16) {
        // Clamp to the current content first: a same-file refresh may have shrunk
        // the diff below the preserved offset, and scrolling up must not stay
        // stuck past the new end (metrics are fresh here, post-render).
        let max = self.diff_max_scroll();
        let current = self.diff_scroll.min(max);
        self.diff_scroll = if down {
            (current + step).min(max)
        } else {
            current.saturating_sub(step)
        };
    }

    /// The selection index of the file at a screen position in the staging pane,
    /// using the list's last-rendered scroll offset.
    fn file_at(&self, pos: Position) -> Option<usize> {
        let area = self.staging_area.get();
        if !area.contains(pos) {
            return None;
        }
        let item = self.staging_state.borrow().offset() + (pos.y - area.y) as usize;
        crate::ui::staging::selection_at(&self.status, item)
    }

    fn on_key_modal(&mut self, key: KeyEvent) {
        if matches!(self.modal, Some(Modal::Help)) {
            self.modal = None; // any key dismisses the help overlay
            return;
        }
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
            self.last_error = Some(format!("{action} failed: {err}"));
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

    fn toggle_changes(&mut self) {
        if self.show_changes {
            // Hiding forces focus to the Diff, the only visible pane — this is
            // the "hidden ⇒ focus Diff" invariant.
            self.show_changes = false;
            self.focus = Focus::Diff;
        } else {
            self.reveal_changes();
        }
    }

    /// Reveal the Changes panel and focus it — the single home for the reveal
    /// semantics shared by the toggle key, Tab, and `h` when the panel is hidden.
    fn reveal_changes(&mut self) {
        self.show_changes = true;
        self.focus = Focus::Staging;
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

    /// The flattened selection index of `path`, preferring `section` but falling
    /// back to the other one (a file can move between staged/unstaged); `None`
    /// if it's no longer listed. Mirrors the staged-first order of `selected_file`.
    fn index_of(&self, section: Section, path: &str) -> Option<usize> {
        let staged = &self.status.staged;
        let in_staged = || staged.iter().position(|e| e.path == path);
        let in_unstaged = || {
            self.status
                .unstaged
                .iter()
                .position(|e| e.path == path)
                .map(|i| staged.len() + i)
        };
        match section {
            Section::Staged => in_staged().or_else(in_unstaged),
            Section::Unstaged => in_unstaged().or_else(in_staged),
        }
    }

    /// Recompute the cached diff when the selected file changes, or when an
    /// external refresh marked it dirty. Navigating to a different file resets
    /// the scroll; a same-file content refresh keeps it.
    fn sync_diff(&mut self) {
        let key = self
            .selected_file()
            .map(|(section, entry)| (section, entry.path.clone()));
        let file_changed = key != self.diff_key;
        if !file_changed && !self.diff_dirty {
            return;
        }
        self.diff_dirty = false;
        // Compute into a local first so the immutable borrow of the file list
        // (and repo) is released before assigning the cached fields.
        let diff = self
            .selected_file()
            .map(|(section, entry)| self.repo.diff(section, entry));
        // A same-file refresh that produced an identical diff is a no-op: keep
        // the warm highlight / SBS caches and the scroll untouched, so a watcher
        // firing on unrelated activity doesn't churn or disturb the view.
        if !file_changed && diff == self.current_diff {
            return;
        }
        self.current_diff = diff;
        self.diff_key = key;
        if file_changed {
            // Only a different file starts at the top; refreshing the open file
            // in place must not yank the view back up while the user scrolls.
            self.diff_scroll = 0;
        }
        // The cached highlights / side-by-side layout describe the previous
        // diff; drop them so the new one is recomputed lazily on next render.
        self.highlight_cache.borrow_mut().clear();
        *self.sbs_rows.borrow_mut() = None;
    }

    /// Keep whichever view is active in sync after an input event.
    fn sync_active(&mut self) {
        match self.view {
            ViewMode::Status => self.sync_diff(),
            ViewMode::History => self.sync_history_diff(),
        }
    }

    /// Re-read the active view's data: status re-reads the working tree; history
    /// re-walks commits (keeping the cursor on the same commit by oid) and
    /// reloads its file list.
    fn refresh_active(&mut self) {
        match self.view {
            ViewMode::Status => self.refresh(),
            ViewMode::History => {
                let current = self.commits.get(self.selected_commit).map(|c| c.id);
                self.load_history();
                self.selected_commit = current
                    .and_then(|id| self.commits.iter().position(|c| c.id == id))
                    .unwrap_or(0);
                self.load_commit_files();
                self.sync_history_diff();
            }
        }
    }

    /// Recompute the history diff for the selected commit + top-pane row. The
    /// commit (`●`) row shows details instead of a file diff, so it clears the
    /// cached diff. Indexing is guarded so an empty repo never panics.
    fn sync_history_diff(&mut self) {
        if self.view != ViewMode::History {
            return;
        }
        let Some(commit) = self.commits.get(self.selected_commit) else {
            self.history_diff = None;
            self.history_diff_key = None;
            return;
        };
        // Row 0 is the commit itself: the right pane shows details, no file diff.
        if self.committed_row == 0 {
            if self.history_diff_key.is_some() {
                self.history_diff = None;
                self.history_diff_key = None;
                self.diff_scroll = 0;
                self.highlight_cache.borrow_mut().clear();
                *self.sbs_rows.borrow_mut() = None;
            }
            return;
        }
        let Some(file) = self.commit_files.get(self.committed_row - 1) else {
            return;
        };
        let key = Some((commit.id, file.path.clone()));
        if key == self.history_diff_key {
            return;
        }
        self.history_diff = Some(self.repo.commit_file_diff(commit, file));
        self.history_diff_key = key;
        self.diff_scroll = 0;
        self.highlight_cache.borrow_mut().clear();
        *self.sbs_rows.borrow_mut() = None;
    }

    /// Syntax-highlight one already-sanitised line, memoised per file so
    /// scrolling reuses the result instead of re-parsing through syntect on
    /// every frame. Single-line highlighting carries no cross-line state (see
    /// `ui::syntax`), so the line text is a sound key; the cache is cleared when
    /// the selected file changes (`sync_diff`).
    pub fn highlight(
        &self,
        syntax: &SyntaxReference,
        theme_name: &str,
        text: &str,
    ) -> HighlightedLine {
        if let Some(hit) = self.highlight_cache.borrow().get(text) {
            return Rc::clone(hit);
        }
        let computed: HighlightedLine =
            crate::ui::syntax::highlight_line(syntax, theme_name, text).into();
        self.highlight_cache
            .borrow_mut()
            .insert(text.to_string(), Rc::clone(&computed));
        computed
    }

    /// The side-by-side row layout for the current diff, computed once per file
    /// and cached. Rows reference `lines` by index.
    pub fn sbs_rows(&self, lines: &[DiffLine]) -> Ref<'_, Vec<SbsRow>> {
        if self.sbs_rows.borrow().is_none() {
            *self.sbs_rows.borrow_mut() = Some(side_by_side_rows(lines));
        }
        Ref::map(self.sbs_rows.borrow(), |cached| {
            cached.as_ref().expect("filled above")
        })
    }

    // --- History view: accessors for rendering + mouse ---

    /// The diff the diff pane should render, by view: the status view's selected
    /// file, or the history view's selected commit file.
    pub fn active_diff(&self) -> Option<&FileDiff> {
        match self.view {
            ViewMode::Status => self.current_diff.as_ref(),
            ViewMode::History => self.history_diff.as_ref(),
        }
    }

    /// The path backing `active_diff`, for the diff title and syntax lookup.
    pub fn active_diff_path(&self) -> Option<String> {
        match self.view {
            ViewMode::Status => self.selected_file().map(|(_, entry)| entry.path.clone()),
            ViewMode::History => self
                .commit_files
                .get(self.committed_row.checked_sub(1)?)
                .map(|file| file.path.clone()),
        }
    }

    /// Whether the history view should show commit details (the `●` row is
    /// selected) rather than a file diff in the right pane.
    pub fn history_shows_details(&self) -> bool {
        self.view == ViewMode::History && self.committed_row == 0
    }

    /// Whether the diff pane is the focused pane, in either view.
    pub fn diff_focused(&self) -> bool {
        match self.view {
            ViewMode::Status => self.focus == Focus::Diff,
            ViewMode::History => self.history_focus == HistoryFocus::Diff,
        }
    }

    pub fn history_focus(&self) -> HistoryFocus {
        self.history_focus
    }

    pub fn commits(&self) -> &[CommitInfo] {
        &self.commits
    }

    pub fn graph_rows(&self) -> &[GraphRow] {
        &self.graph_rows
    }

    pub fn selected_commit(&self) -> usize {
        self.selected_commit
    }

    pub fn selected_commit_info(&self) -> Option<&CommitInfo> {
        self.commits.get(self.selected_commit)
    }

    pub fn history_files(&self) -> &[CommitFile] {
        &self.commit_files
    }

    pub fn committed_row(&self) -> usize {
        self.committed_row
    }

    pub fn committed_state_mut(&self) -> std::cell::RefMut<'_, ListState> {
        self.committed_state.borrow_mut()
    }

    pub fn graph_state_mut(&self) -> std::cell::RefMut<'_, ListState> {
        self.graph_state.borrow_mut()
    }

    pub fn set_committed_area(&self, area: Rect) {
        self.committed_area.set(area);
    }

    pub fn set_graph_area(&self, area: Rect) {
        self.graph_area.set(area);
    }

    /// Record the left column's body rect and the horizontal divider row for this
    /// frame, so a drag on it can be hit-tested (mirrors `set_split_geometry`).
    pub fn set_hsplit_geometry(&self, left: Rect, hdivider_y: u16) {
        self.left_col_area.set(left);
        self.hdivider_y.set(hdivider_y);
    }

    /// The "Committed Changes" sub-pane height clamped so both it and the Graph
    /// keep a usable height. Shared by the layout and the drag handler.
    pub fn committed_pane_height(&self, left_height: u16) -> u16 {
        let max = left_height
            .saturating_sub(MIN_GRAPH_HEIGHT)
            .max(MIN_COMMITTED_HEIGHT);
        self.committed_height.clamp(MIN_COMMITTED_HEIGHT, max)
    }

    /// Whether the horizontal divider shows its active affordance.
    pub fn hdivider_engaged(&self) -> bool {
        self.hovering_hdivider || self.dragging_hdivider
    }

    /// Largest valid diff scroll offset, from the last render's content rows
    /// and viewport height.
    pub fn diff_max_scroll(&self) -> u16 {
        self.diff_content_rows
            .get()
            .saturating_sub(self.diff_viewport.get())
    }

    /// Record the diff pane's inner height and the total rows the current mode
    /// renders (called while rendering), so scrolling clamps to the content.
    pub fn set_diff_metrics(&self, viewport: u16, content_rows: u16) {
        self.diff_viewport.set(viewport);
        self.diff_content_rows.set(content_rows);
    }

    /// The persisted staging list state; rendering borrows it so the scroll
    /// offset is available for mouse hit-testing.
    pub fn staging_state_mut(&self) -> std::cell::RefMut<'_, ListState> {
        self.staging_state.borrow_mut()
    }

    pub fn set_staging_area(&self, area: Rect) {
        self.staging_area.set(area);
    }

    pub fn set_diff_area(&self, area: Rect) {
        self.diff_area.set(area);
    }

    /// Record the body rect and split-bar column for this frame, so a drag on
    /// the divider can be hit-tested against where it was actually drawn.
    pub fn set_split_geometry(&self, body: Rect, divider_x: u16) {
        self.body_area.set(body);
        self.divider_x.set(divider_x);
    }

    /// The Changes panel width clamped to a usable range for the given body
    /// width, keeping a minimum for both panes. Shared by the layout and the
    /// drag handler so they can't disagree on the split.
    pub fn changes_pane_width(&self, body_width: u16) -> u16 {
        let max = body_width
            .saturating_sub(MIN_DIFF_WIDTH)
            .max(MIN_CHANGES_WIDTH);
        self.changes_width.clamp(MIN_CHANGES_WIDTH, max)
    }

    pub fn staging_area(&self) -> Rect {
        self.staging_area.get()
    }

    pub fn diff_area(&self) -> Rect {
        self.diff_area.get()
    }

    fn toggle_diff_mode(&mut self) {
        self.diff_mode = match self.diff_mode {
            DiffMode::Unified => DiffMode::SideBySide,
            DiffMode::SideBySide => DiffMode::Unified,
        };
        self.diff_scroll = 0;
    }
}

/// Pair the unified diff lines into side-by-side rows (by index): context lines
/// appear on both sides; a run of deletions is zipped against the following run
/// of additions, padding the shorter side with blanks.
fn side_by_side_rows(lines: &[DiffLine]) -> Vec<SbsRow> {
    let mut rows = Vec::new();
    let mut deletions: Vec<usize> = Vec::new();
    let mut additions: Vec<usize> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        match line.kind {
            LineKind::Deletion => deletions.push(i),
            LineKind::Addition => additions.push(i),
            LineKind::Context => {
                flush_pairs(&mut rows, &mut deletions, &mut additions);
                rows.push(SbsRow::Pair {
                    left: Some(i),
                    right: Some(i),
                });
            }
            LineKind::Hunk => {
                flush_pairs(&mut rows, &mut deletions, &mut additions);
                rows.push(SbsRow::Hunk(i));
            }
        }
    }
    flush_pairs(&mut rows, &mut deletions, &mut additions);
    rows
}

fn flush_pairs(rows: &mut Vec<SbsRow>, deletions: &mut Vec<usize>, additions: &mut Vec<usize>) {
    for i in 0..deletions.len().max(additions.len()) {
        rows.push(SbsRow::Pair {
            left: deletions.get(i).copied(),
            right: additions.get(i).copied(),
        });
    }
    deletions.clear();
    additions.clear();
}
