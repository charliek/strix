use std::cell::{Cell, Ref, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use ratatui::widgets::ListState;
use syntect::parsing::SyntaxReference;

use crate::comments::{self, Comment, Side};
use crate::config::{Config, Setting};
use crate::git::{
    Change, CommitFile, CommitInfo, DiffLine, FileDiff, FileEntry, LineKind, RefLabel, Repo,
    ReviewSpec, Section, Status,
};
use crate::graph::{self, GraphRow};
use crate::keys::{Action, Keymap};
use crate::ui::theme::Theme;

/// A path-based git mutation (stage / unstage); lets the select → run → refresh
/// flow be shared via `run_on_selected`.
type GitOp = fn(&Repo, &str) -> anyhow::Result<()>;

/// Drop comment inboxes for branches/commits that no longer exist, once at
/// startup (plan §3.1). Cheap and elided when nothing is stale: the store is
/// only opened when its file already exists, and rewritten only when a set is
/// actually dropped — so a clean open neither creates nor churns the file.
fn startup_comment_gc(repo: &Repo) -> anyhow::Result<()> {
    let dir = repo.strix_dir();
    if !dir.join("comments.json").exists() {
        return Ok(());
    }
    let mut live: std::collections::HashSet<String> = repo.branch_names()?.into_iter().collect();
    // The checked-out inbox is always live even without a ref (unborn HEAD) — GC
    // must never drop the current session's own comments (plan §3.1).
    live.insert(repo.head_branch_key()?);
    let commit_exists = |key: &str| repo.commit_exists(key);
    let mut peek = crate::comments::load(&dir)?;
    if crate::comments::gc(&mut peek, &live, commit_exists).is_empty() {
        return Ok(());
    }
    crate::comments::mutate(&dir, |store| {
        crate::comments::gc(store, &live, commit_exists);
    })?;
    Ok(())
}

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
    /// A branch-to-branch review session (`strix diff <range>`).
    Review,
}

/// Which sub-pane of the review view receives keyboard input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewFocus {
    /// The flat changed-file list.
    List,
    /// The diff pane.
    Diff,
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

/// A transient footer message shown until the next input. `Error` marks a failed
/// action (e.g. a stage that git rejected); `Info` a benign notice (e.g. the
/// theme name after a cycle). Same clear-on-next-input lifecycle for both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flash {
    pub text: String,
    pub kind: FlashKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashKind {
    Error,
    Info,
}

impl Flash {
    pub fn error(text: impl Into<String>) -> Self {
        Flash {
            text: text.into(),
            kind: FlashKind::Error,
        }
    }

    pub fn info(text: impl Into<String>) -> Self {
        Flash {
            text: text.into(),
            kind: FlashKind::Info,
        }
    }
}

/// A side-by-side row, referencing diff lines by index into the file's
/// `Vec<DiffLine>`. Indices (not borrows) let the row layout be computed once
/// per file and cached on `App` without a self-referential borrow. Comment rows
/// carry the comment *id* (not a vec index), so a concurrent removal can't shift
/// a row onto the wrong comment.
#[derive(Clone, Copy)]
pub enum SbsRow {
    Hunk(usize),
    Pair {
        left: Option<usize>,
        right: Option<usize>,
    },
    /// A full-width review comment anchored below its line (spans both columns).
    Comment(u64),
    /// A full-width orphaned review comment, shown in the top-of-diff block.
    Orphan(u64),
}

/// A unified-diff row: a diff line (by index), or a full-width review comment
/// anchored below its line, or an orphaned comment in the top-of-diff block.
/// Making unified row-driven (rather than a strict 1:1 lines→rows map) is what
/// lets comments inject rows; metrics count rows, so scrolling reaches them.
#[derive(Clone, Copy)]
pub enum URow {
    Line(usize),
    Comment(u64),
    Orphan(u64),
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

/// Review-session state: the resolved range, its changed-file list, and the
/// review view's own selection / focus / cached diff. Only what is the review
/// view's own lives here — the shared pane machinery (diff scroll + metrics,
/// highlight / side-by-side caches, changes-pane width) stays on [`App`], reused
/// across all three views.
struct ReviewState {
    /// The resolved range (its `input` is re-run verbatim on refresh).
    spec: ReviewSpec,
    /// The files that differ between `spec.base` and `spec.head`, in list order.
    files: Vec<CommitFile>,
    /// Row selected in `files` (0-based; meaningless when `files` is empty).
    selected: usize,
    /// The diff cursor: an index into the active mode's row list (unified or
    /// side-by-side), able to rest on an injected comment/orphan row. Moved by
    /// j/k/g/G/ctrl-d/u while the diff pane has focus, reset to 0 on file change
    /// and mode toggle, clamped to the row count after a relist (plan §3.4).
    cursor: usize,
    focus: ReviewFocus,
    /// The cached diff for the selected file and the `(base, head, path)` OID key
    /// it was computed for, so a moved range tip recomputes it.
    diff: Option<FileDiff>,
    diff_key: Option<(gix::ObjectId, gix::ObjectId, String)>,
    /// Bumped each time the file list is rebuilt by a refresh, so a test (and the
    /// churn guard's contract) can observe that an OID-unchanged reload skips it.
    relist_count: u64,
    /// This review's comment inbox (the checked-out branch's set), loaded on
    /// session open and refreshed by the store-dir watcher. Empty when comments
    /// are inactive (`authoring == false`).
    comments: Vec<Comment>,
    /// The branch key this inbox lives under (`Repo::head_branch_key`).
    branch_key: String,
    /// Whether comments are active for this session: `true` only when the review
    /// head OID == the checked-out HEAD OID (plan invariant §3.1.1). A review of a
    /// range whose head isn't HEAD renders comment-free and can't author.
    authoring: bool,
    /// The list's scroll offset, persisted between frames for mouse hit-testing.
    list_state: RefCell<ListState>,
    /// The file list's inner rect from the last render, for mouse hit-testing.
    list_area: Cell<Rect>,
}

impl ReviewState {
    fn new(spec: ReviewSpec, files: Vec<CommitFile>, branch_key: String, authoring: bool) -> Self {
        ReviewState {
            spec,
            files,
            selected: 0,
            cursor: 0,
            focus: ReviewFocus::List,
            diff: None,
            diff_key: None,
            relist_count: 0,
            comments: Vec::new(),
            branch_key,
            authoring,
            list_state: RefCell::new(ListState::default()),
            list_area: Cell::new(Rect::default()),
        }
    }

    /// The comment with `id`, if it's in this inbox.
    fn comment(&self, id: u64) -> Option<&Comment> {
        self.comments.iter().find(|c| c.id == id)
    }
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
    /// The canonical name of the active theme (from `Theme::resolve`), so the
    /// cycle can find the current position and the flash never names a theme
    /// other than the one on screen.
    pub theme_name: String,
    pub should_quit: bool,
    /// A transient message from the last action, shown until the next input.
    pub flash: Option<Flash>,

    /// Cached diff for the selected file; recomputed only when the selection
    /// changes (see `sync_diff`).
    pub current_diff: Option<FileDiff>,
    diff_key: Option<(Section, String)>,
    /// Set when an external refresh should recompute the open file's diff even
    /// though its `(section, path)` is unchanged (its content may have changed).
    /// Unlike navigating to a new file, this preserves the scroll position.
    diff_dirty: bool,
    pub diff_mode: DiffMode,
    /// Whether the diff pane shows line-number gutters (unified's 10-char
    /// number gutter, SBS's per-column 5-char gutter). The sign column in
    /// unified mode is unaffected. Toggled with `n`; from `Config.line_numbers`.
    pub show_line_numbers: bool,
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
    /// The unified row layout (diff lines interleaved with comment rows), cached
    /// beside `sbs_rows` and invalidated at the same points plus on any comment
    /// mutation. `None` until first built for the current diff.
    unified_rows: RefCell<Option<Vec<URow>>>,

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

    // --- Top-level view ---
    pub view: ViewMode,
    /// Present only in a review session (`strix diff <range>`); drives the
    /// `ViewMode::Review` view. `None` for a status session.
    review: Option<ReviewState>,

    // --- History view ---
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
    /// The real config dir in production (set via `with_config_dir` from the
    /// entrypoint), `None` in every existing test constructor. `None` makes
    /// `t`/`d`/`n` persistence a silent no-op — this is what keeps `cargo test`
    /// from ever touching the developer's real `~/.config/strix`, and lets
    /// theme resolution (`cycle_theme`) stay hermetic in tests too.
    config_dir: Option<PathBuf>,
}

impl App {
    pub fn new(repo_path: PathBuf) -> anyhow::Result<Self> {
        Self::with_config(repo_path, &Config::default())
    }

    pub fn with_config(repo_path: PathBuf, config: &Config) -> anyhow::Result<Self> {
        Self::build(repo_path, config, None)
    }

    /// Open a review session against `range` (`strix diff <range>`). The range is
    /// resolved here, before any terminal setup, so a bad range fails fast with a
    /// contextual error rather than a blank TUI.
    pub fn for_review(repo_path: PathBuf, config: &Config, range: &str) -> anyhow::Result<Self> {
        Self::build(repo_path, config, Some(range))
    }

    fn build(repo_path: PathBuf, config: &Config, range: Option<&str>) -> anyhow::Result<Self> {
        let repo = Repo::open(&repo_path)?;
        // Best-effort startup GC of dead-branch inboxes, right after the repo
        // opens and before anything else touches the comments store. A failure
        // (corrupt store, ref-read error) must never block opening the app.
        if let Err(err) = startup_comment_gc(&repo) {
            tracing::warn!("comments gc at startup failed: {err:#}");
        }
        let status = repo.status()?;
        let (theme_name, theme) = Theme::resolve(
            config.theme.as_deref().unwrap_or("tokyo-night"),
            crate::config::config_dir().as_deref(),
        );
        // A review session resolves its range up front (a bad range bubbles out).
        let review = match range {
            Some(range) => {
                let spec = repo.resolve_range(range)?;
                let files = repo.range_files(&spec)?;
                let branch_key = repo.head_branch_key()?;
                // Comments are active only when the reviewed head is the
                // checked-out HEAD (plan invariant §3.1.1): that makes the human's
                // TUI inbox and the agent's CLI inbox provably the same set.
                let head_oid = repo.gix().head_id().ok().map(|id| id.detach());
                let authoring = head_oid == Some(spec.head);
                Some(ReviewState::new(spec, files, branch_key, authoring))
            }
            None => None,
        };
        let view = if review.is_some() {
            ViewMode::Review
        } else {
            ViewMode::Status
        };
        let mut app = App {
            repo,
            status,
            view,
            review,
            selected: 0,
            focus: Focus::Staging,
            show_changes: true,
            changes_width: DEFAULT_CHANGES_WIDTH,
            dragging_divider: false,
            hovering_divider: false,
            modal: None,
            theme,
            theme_name,
            should_quit: false,
            flash: None,
            current_diff: None,
            diff_key: None,
            diff_dirty: false,
            diff_mode: config.diff_mode(),
            show_line_numbers: config.line_numbers(),
            diff_scroll: 0,
            diff_viewport: Cell::new(0),
            diff_content_rows: Cell::new(0),
            highlight_cache: RefCell::new(HashMap::new()),
            sbs_rows: RefCell::new(None),
            unified_rows: RefCell::new(None),
            staging_state: RefCell::new(ListState::default()),
            staging_area: Cell::new(Rect::default()),
            diff_area: Cell::new(Rect::default()),
            body_area: Cell::new(Rect::default()),
            divider_x: Cell::new(0),
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
            config_dir: None,
        };
        app.clamp_selection();
        // Load the review inbox (records the range + re-anchors, per §3.1.1). A
        // corrupt store is recoverable: it flashes and opens comment-free rather
        // than failing construction.
        app.reanchor_review_comments();
        app.sync_active();
        Ok(app)
    }

    /// Inject the config directory used for persisting settings (`t`/`d`/`n`)
    /// and for resolving themes on cycle. The real entrypoint (`lib::run`)
    /// sets this from `config::config_dir()`; every existing test constructor
    /// leaves it `None`, which makes persistence a silent no-op and keeps
    /// theme-cycle resolution hermetic.
    pub fn with_config_dir(mut self, config_dir: Option<PathBuf>) -> Self {
        self.config_dir = config_dir;
        // Re-resolve against the injected directory so startup resolution and
        // later cycling/persistence always read the same themes/ location.
        let (name, theme) = Theme::resolve(&self.theme_name, self.config_dir.as_deref());
        self.theme_name = name;
        self.theme = theme;
        self
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

    /// Re-read the active view's data and recompute its diff in one step. Used by
    /// the file watcher, whose path has no trailing `sync_active` like
    /// `on_key`/`on_mouse`. View-aware: it refreshes whichever view is showing.
    pub fn reload(&mut self) {
        self.refresh_active();
        self.sync_active();
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits, even with a modal open.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }
        self.flash = None;

        if self.modal.is_some() {
            self.on_key_modal(key);
        } else if key.code == KeyCode::Esc {
            // Esc leaves the history view; it is not in the keymap (so the modal's
            // own Esc handling stays first). A no-op in the status and review
            // views (review's Esc must not exit a session).
            match self.view {
                ViewMode::History => self.exit_history(),
                ViewMode::Status | ViewMode::Review => {}
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
                self.persist_setting(Setting::DiffMode(self.diff_mode));
                return;
            }
            Action::ToggleLineNumbers => {
                self.toggle_line_numbers();
                self.persist_setting(Setting::LineNumbers(self.show_line_numbers));
                return;
            }
            Action::CycleTheme => {
                self.cycle_theme();
                self.persist_setting(Setting::Theme(self.theme_name.clone()));
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
                // `1` returns to the session home (status or review); from history
                // it exits back to it, and is a no-op once already home.
                self.go_home();
                return;
            }
            Action::ShowHistory => {
                if self.view != ViewMode::History {
                    self.enter_history();
                }
                return;
            }
            _ => {}
        }
        match self.view {
            ViewMode::Status => self.dispatch_status(action),
            ViewMode::History => self.dispatch_history(action),
            ViewMode::Review => self.dispatch_review(action),
        }
    }

    /// The session's home view: Review for a `strix diff` session, else Status.
    /// History is a toggleable overlay on top of whichever home a session has.
    fn home_view(&self) -> ViewMode {
        if self.review.is_some() {
            ViewMode::Review
        } else {
            ViewMode::Status
        }
    }

    /// Return to the session home from history (a no-op if already home).
    fn go_home(&mut self) {
        if self.view == ViewMode::History {
            self.exit_history();
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
            // Comment navigation is a review-view feature; inert elsewhere.
            Action::NextComment | Action::PrevComment => {}
            // Handled in `dispatch`.
            Action::Quit
            | Action::Help
            | Action::Refresh
            | Action::ToggleDiffMode
            | Action::ToggleLineNumbers
            | Action::CycleTheme
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
            Action::SwitchPane => {
                if self.show_changes {
                    self.cycle_history_focus();
                } else {
                    self.reveal_history_panel(); // Tab reveals a hidden panel and lands in it.
                }
            }
            Action::ToggleChanges => self.toggle_history_panel(),
            Action::FocusStaging => {
                if self.show_changes {
                    self.history_focus_left();
                } else {
                    self.reveal_history_panel();
                }
            }
            Action::FocusDiff => self.history_focus_right(),
            Action::Down => self.history_move(true),
            Action::Up => self.history_move(false),
            Action::Top => self.history_to_edge(false),
            Action::Bottom => self.history_to_edge(true),
            Action::HalfPageDown => self.scroll_diff(true, self.half_page()),
            Action::HalfPageUp => self.scroll_diff(false, self.half_page()),
            // Read-only view: staging ops and comment navigation do nothing.
            Action::ToggleStage
            | Action::Stage
            | Action::Unstage
            | Action::Discard
            | Action::NextComment
            | Action::PrevComment => {}
            // Handled in `dispatch`.
            Action::Quit
            | Action::Help
            | Action::Refresh
            | Action::ToggleDiffMode
            | Action::ToggleLineNumbers
            | Action::CycleTheme
            | Action::ToggleHistory
            | Action::ShowStatus
            | Action::ShowHistory => {}
        }
    }

    /// Interpret a navigation action in the review view: it acts on whichever
    /// sub-pane (file List / Diff) has focus. The view is read-only, so staging
    /// ops do nothing (mirrors `dispatch_history`).
    fn dispatch_review(&mut self, action: Action) {
        match action {
            Action::SwitchPane => {
                if self.show_changes {
                    self.review_toggle_focus();
                } else {
                    self.reveal_review_panel(); // Tab reveals a hidden panel and lands in it.
                }
            }
            Action::ToggleChanges => self.toggle_review_panel(),
            Action::FocusStaging => {
                if self.show_changes {
                    self.set_review_focus(ReviewFocus::List);
                } else {
                    self.reveal_review_panel();
                }
            }
            Action::FocusDiff => self.set_review_focus(ReviewFocus::Diff),
            Action::Down => self.review_move(true),
            Action::Up => self.review_move(false),
            Action::Top => self.review_to_edge(false),
            Action::Bottom => self.review_to_edge(true),
            // Ctrl-d/u: with the diff focused, move the cursor by a half page (it
            // drives the viewport, so revealing it does the scrolling); with the
            // file list focused, scroll the viewport only, leaving the cursor put.
            Action::HalfPageDown => self.review_half_page(true),
            Action::HalfPageUp => self.review_half_page(false),
            Action::NextComment => self.review_cycle_comment(true),
            Action::PrevComment => self.review_cycle_comment(false),
            // `x` deletes the comment under the cursor (diff focus only); on a code
            // row it's a silent no-op. Other staging ops stay inert.
            Action::Discard => self.review_delete_comment(),
            Action::ToggleStage | Action::Stage | Action::Unstage => {}
            // Handled in `dispatch`.
            Action::Quit
            | Action::Help
            | Action::Refresh
            | Action::ToggleDiffMode
            | Action::ToggleLineNumbers
            | Action::CycleTheme
            | Action::ToggleHistory
            | Action::ShowStatus
            | Action::ShowHistory => {}
        }
    }

    /// Mirror of `toggle_changes` for the review view: hiding forces focus to the
    /// Diff (the only visible pane); revealing returns to the file List.
    fn toggle_review_panel(&mut self) {
        if self.show_changes {
            self.show_changes = false;
            self.set_review_focus(ReviewFocus::Diff);
        } else {
            self.reveal_review_panel();
        }
    }

    fn reveal_review_panel(&mut self) {
        self.show_changes = true;
        self.set_review_focus(ReviewFocus::List);
    }

    fn review_toggle_focus(&mut self) {
        let next = match self.review_focus() {
            ReviewFocus::List => ReviewFocus::Diff,
            ReviewFocus::Diff => ReviewFocus::List,
        };
        self.set_review_focus(next);
    }

    fn set_review_focus(&mut self, focus: ReviewFocus) {
        if let Some(review) = self.review.as_mut() {
            review.focus = focus;
        }
    }

    /// Move within the review view: the file List moves the selection (resetting
    /// the diff cursor to the new file's first row), the Diff moves the cursor.
    fn review_move(&mut self, down: bool) {
        match self.review_focus() {
            ReviewFocus::List => {
                let last = self.review_files().len().saturating_sub(1);
                let next = if down {
                    (self.review_selected() + 1).min(last)
                } else {
                    self.review_selected().saturating_sub(1)
                };
                self.select_review_file(next);
            }
            ReviewFocus::Diff => self.review_move_cursor(down, 1),
        }
    }

    fn review_to_edge(&mut self, bottom: bool) {
        match self.review_focus() {
            ReviewFocus::List => {
                let target = if bottom {
                    self.review_files().len().saturating_sub(1)
                } else {
                    0
                };
                self.select_review_file(target);
            }
            ReviewFocus::Diff => {
                let last = self.review_row_count().saturating_sub(1);
                if let Some(review) = self.review.as_mut() {
                    review.cursor = if bottom { last } else { 0 };
                }
                self.review_reveal_cursor();
            }
        }
    }

    /// Select review file `idx` and reset the diff cursor to its first row. A
    /// no-op selection (nav that lands on the same file, e.g. `k` at the top)
    /// leaves the cursor and scroll untouched — resetting only the cursor would
    /// strand it above the preserved viewport. The one caller that must *not*
    /// reset on a real change (comment navigation, which places the cursor on a
    /// specific row) sets `selected`/`cursor` directly.
    fn select_review_file(&mut self, idx: usize) {
        if let Some(review) = self.review.as_mut() {
            if review.selected != idx {
                review.selected = idx;
                review.cursor = 0;
            }
        }
    }

    /// Move the diff cursor by `step` rows (clamped), then scroll the viewport so
    /// it stays visible ("act-and-reveal", plan §3.4).
    fn review_move_cursor(&mut self, down: bool, step: usize) {
        let last = self.review_row_count().saturating_sub(1);
        if let Some(review) = self.review.as_mut() {
            review.cursor = if down {
                (review.cursor + step).min(last)
            } else {
                review.cursor.saturating_sub(step)
            };
        }
        self.review_reveal_cursor();
    }

    /// Half-page in review: the diff pane moves the cursor (act-and-reveal); the
    /// file list scrolls the diff viewport only, without touching the cursor
    /// (restores the pre-cursor behaviour when the list is focused, plan §3.3).
    fn review_half_page(&mut self, down: bool) {
        match self.review_focus() {
            ReviewFocus::Diff => self.review_move_cursor(down, self.half_page() as usize),
            ReviewFocus::List => self.scroll_diff(down, self.half_page()),
        }
    }

    /// Scroll the diff viewport so the cursor row is visible: no-op when it
    /// already is, otherwise snap the top edge to bring it just into view.
    fn review_reveal_cursor(&mut self) {
        let Some(cursor) = self.review.as_ref().map(|r| r.cursor) else {
            return;
        };
        let viewport = self.diff_viewport.get() as usize;
        if viewport == 0 {
            return;
        }
        let count = self.review_row_count();
        let top = self.diff_scroll as usize;
        let new_top = if cursor < top {
            cursor
        } else if cursor >= top + viewport {
            cursor + 1 - viewport
        } else {
            top
        };
        // Never scroll past the last full page of content.
        let max_top = count.saturating_sub(viewport);
        self.diff_scroll = new_top.min(max_top).min(u16::MAX as usize) as u16;
    }

    /// Whether the diff cursor row currently lies within the visible viewport,
    /// tested against the same clamped offset the renderer paints with (so a
    /// wheel scroll that pushed it offscreen reads as not-visible).
    fn review_cursor_visible(&self) -> bool {
        let Some(cursor) = self.review.as_ref().map(|r| r.cursor) else {
            return false;
        };
        let viewport = self.diff_viewport.get() as usize;
        if viewport == 0 {
            return false;
        }
        let top = self.diff_scroll.min(self.diff_max_scroll()) as usize;
        cursor >= top && cursor < top + viewport
    }

    /// Act-and-reveal gate (plan §3.4): when the cursor row is offscreen (e.g.
    /// after a wheel scroll), scroll it into view and return `false` so the
    /// caller only reveals — it must not act on a row the user can't see; when
    /// already visible, return `true` so the caller proceeds. `x` uses this to
    /// delete, and C5 reuses it for `c`.
    fn reveal_cursor_before_acting(&mut self) -> bool {
        if self.review_cursor_visible() {
            return true;
        }
        self.review_reveal_cursor();
        false
    }

    /// The number of rows the active diff mode renders for the selected file: the
    /// unified/side-by-side row list, or (for an empty/binary diff) the orphan
    /// block that renders in its place. The cursor indexes into this list.
    fn review_row_count(&self) -> usize {
        let Some(review) = self.review.as_ref() else {
            return 0;
        };
        match review.diff.as_ref() {
            Some(FileDiff::Text(lines)) if !lines.is_empty() => match self.diff_mode {
                DiffMode::Unified => self.unified_rows(lines).len(),
                DiffMode::SideBySide => self.sbs_rows(lines).len(),
            },
            // Empty/binary/no diff: the only selectable rows are orphan comments.
            _ => self.selected_file_orphans().len(),
        }
    }

    /// Clamp the diff cursor to the current row count (after a relist or a
    /// comment deletion shrinks the row list). Keeps the index otherwise.
    fn clamp_review_cursor(&mut self) {
        let last = self.review_row_count().saturating_sub(1);
        if let Some(review) = self.review.as_mut() {
            review.cursor = review.cursor.min(last);
        }
    }

    /// The comment id under the cursor in the selected file, or `None` when the
    /// cursor rests on a code/hunk row (so `x` there is a silent no-op).
    fn cursor_comment_id(&self) -> Option<u64> {
        let review = self.review.as_ref()?;
        let cursor = review.cursor;
        match review.diff.as_ref() {
            Some(FileDiff::Text(lines)) if !lines.is_empty() => match self.diff_mode {
                DiffMode::Unified => match self.unified_rows(lines).get(cursor)? {
                    URow::Comment(id) | URow::Orphan(id) => Some(*id),
                    URow::Line(_) => None,
                },
                DiffMode::SideBySide => match self.sbs_rows(lines).get(cursor)? {
                    SbsRow::Comment(id) | SbsRow::Orphan(id) => Some(*id),
                    SbsRow::Hunk(_) | SbsRow::Pair { .. } => None,
                },
            },
            // Empty/binary diff: every rendered row is an orphan comment.
            _ => self.selected_file_orphans().get(cursor).copied(),
        }
    }

    /// The row index of comment `id` in the selected file's active row list, for
    /// placing the cursor after a jump. `None` when it isn't placed in this file.
    fn comment_row_index(&self, id: u64) -> Option<usize> {
        let review = self.review.as_ref()?;
        match review.diff.as_ref() {
            Some(FileDiff::Text(lines)) if !lines.is_empty() => match self.diff_mode {
                DiffMode::Unified => self.unified_rows(lines).iter().position(
                    |row| matches!(row, URow::Comment(cid) | URow::Orphan(cid) if *cid == id),
                ),
                DiffMode::SideBySide => self.sbs_rows(lines).iter().position(
                    |row| matches!(row, SbsRow::Comment(cid) | SbsRow::Orphan(cid) if *cid == id),
                ),
            },
            _ => self
                .selected_file_orphans()
                .iter()
                .position(|&cid| cid == id),
        }
    }

    /// Every comment on a *listed* file, in file-list order then by anchor line
    /// (ties by id) — the cycle order for `]`/`[`. Comments on files no longer in
    /// the range are excluded (they're CLI-territory, plan §3.4).
    fn ordered_comment_ids(&self) -> Vec<u64> {
        let Some(review) = self.review.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for file in &review.files {
            // Sort by (line, side, id): on a replaced line the pinned SBS layout
            // emits old-side comments before new-side (see
            // `side_by_side_rows_with_comments`), so old must rank before new here
            // to visit them in on-screen order.
            let mut ids: Vec<(usize, u8, u64)> = review
                .comments
                .iter()
                .filter(|c| c.file == file.path)
                .map(|c| (c.line, side_rank(c.side), c.id))
                .collect();
            ids.sort_unstable();
            out.extend(ids.into_iter().map(|(_, _, id)| id));
        }
        out
    }

    /// Jump to the next / previous review comment on a listed file, wrapping.
    /// Switches the selected file when the target lives elsewhere, focuses the
    /// diff pane, places the cursor on the comment's row, and reveals it. Zero
    /// comments on listed files → an Info flash.
    fn review_cycle_comment(&mut self, forward: bool) {
        let order = self.ordered_comment_ids();
        if order.is_empty() {
            self.flash = Some(Flash::info("no comments"));
            return;
        }
        // Start from the comment under the cursor if there is one; otherwise the
        // ends (first for `]`, last for `[`).
        let target = match self
            .cursor_comment_id()
            .and_then(|id| order.iter().position(|&x| x == id))
        {
            Some(pos) => {
                let len = order.len();
                let next = if forward {
                    (pos + 1) % len
                } else {
                    (pos + len - 1) % len
                };
                order[next]
            }
            None if forward => order[0],
            None => order[order.len() - 1],
        };
        // Resolve the target's file and switch selection to it if needed.
        let idx = self.review.as_ref().and_then(|r| {
            let file = &r.comments.iter().find(|c| c.id == target)?.file;
            r.files.iter().position(|f| &f.path == file)
        });
        if let (Some(idx), Some(review)) = (idx, self.review.as_mut()) {
            review.selected = idx;
        }
        self.set_review_focus(ReviewFocus::Diff);
        // Recompute the (possibly new) file's diff now, so the row lookup and the
        // reveal both see it (the trailing `sync_active` would otherwise be too
        // late for this in-handler placement).
        self.sync_review_diff();
        if let Some(row) = self.comment_row_index(target) {
            if let Some(review) = self.review.as_mut() {
                review.cursor = row;
            }
        }
        self.review_reveal_cursor();
    }

    /// Delete the comment under the cursor (diff focus only). Transactional per
    /// plan §3.1.5: mutate a fresh store read, persist, and only on success
    /// replace the in-memory set + invalidate the row caches; a failure leaves
    /// everything unchanged and flashes. A code-row cursor is a silent no-op.
    fn review_delete_comment(&mut self) {
        if self.review_focus() != ReviewFocus::Diff {
            return;
        }
        // Act-and-reveal: never delete a row the user can't see. A first `x` on an
        // offscreen cursor (after a wheel scroll) only scrolls it into view; a
        // second `x`, now that it's visible, deletes (finding 1, plan §3.4).
        if !self.reveal_cursor_before_acting() {
            return;
        }
        let Some(id) = self.cursor_comment_id() else {
            return; // code / hunk row: no-op
        };
        let dir = self.repo.strix_dir();
        let branch = match self.review.as_ref() {
            Some(review) if review.authoring => review.branch_key.clone(),
            _ => return,
        };
        let result = comments::mutate(&dir, |store| {
            let entry = store.branches.get_mut(&branch)?;
            let pos = entry.comments.iter().position(|c| c.id == id)?;
            entry.comments.remove(pos);
            Some(entry.comments.clone())
        });
        match result {
            Ok(Some(set)) => {
                if let Some(review) = self.review.as_mut() {
                    review.comments = set;
                }
                self.invalidate_comment_rows();
                self.clamp_review_cursor();
                self.flash = Some(Flash::info("comment deleted"));
            }
            // The id vanished between our read and the mutate (a concurrent rm):
            // nothing to delete, and the next reload reconciles the set.
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("deleting comment failed: {err:#}");
                self.flash = Some(Flash::error(format!("comments: {err}")));
            }
        }
    }

    /// The diff cursor's row index (into the active mode's row list); `0` outside
    /// a review session. Exposed for tests and comment navigation.
    pub fn review_cursor(&self) -> usize {
        self.review.as_ref().map(|r| r.cursor).unwrap_or(0)
    }

    /// The cursor row to highlight while rendering — `Some(idx)` only in the
    /// review view with the diff pane focused (plan §3.4); `None` otherwise, so
    /// the highlight never shows while the file list is focused.
    pub fn review_cursor_highlight(&self) -> Option<usize> {
        if self.view == ViewMode::Review && self.diff_focused() {
            self.review.as_ref().map(|r| r.cursor)
        } else {
            None
        }
    }

    /// The review view's focused sub-pane (List when there is no review session).
    fn review_focus(&self) -> ReviewFocus {
        self.review
            .as_ref()
            .map(|review| review.focus)
            .unwrap_or(ReviewFocus::List)
    }

    /// Mirror of `toggle_changes` for the history view: hiding forces focus to
    /// the Diff (the only visible pane); revealing returns to the Graph (the
    /// history view's entry-default focus).
    fn toggle_history_panel(&mut self) {
        if self.show_changes {
            self.show_changes = false;
            self.history_focus = HistoryFocus::Diff;
        } else {
            self.reveal_history_panel();
        }
    }

    fn reveal_history_panel(&mut self) {
        self.show_changes = true;
        self.history_focus = HistoryFocus::Graph;
    }

    fn half_page(&self) -> u16 {
        (self.diff_viewport.get() / 2).max(1)
    }

    // --- History view: enter / exit / load ---

    fn toggle_history(&mut self) {
        match self.view {
            ViewMode::History => self.exit_history(),
            // From either home (status or review), `i` opens history.
            ViewMode::Status | ViewMode::Review => self.enter_history(),
        }
    }

    fn enter_history(&mut self) {
        if self.commits.is_empty() {
            self.load_history();
        }
        self.view = ViewMode::History;
        // Hidden left column ⇒ the Diff is the only visible pane to focus.
        self.history_focus = if self.show_changes {
            HistoryFocus::Graph
        } else {
            HistoryFocus::Diff
        };
        self.selected_commit = 0;
        self.committed_row = 0;
        self.diff_scroll = 0;
        self.load_commit_files();
        self.sync_history_diff();
    }

    fn exit_history(&mut self) {
        // Return to the session's home view (status or review), not always status.
        self.view = self.home_view();
        // Respect the hidden-panel invariant in whichever home we return to:
        // when the left panel is hidden, focus must be the only visible pane
        // (Diff), or keys would route to an invisible selection.
        self.focus = if self.show_changes {
            Focus::Staging
        } else {
            Focus::Diff
        };
        if !self.show_changes {
            self.set_review_focus(ReviewFocus::Diff);
        }
        // Clear any in-flight horizontal-divider drag/hover state so it can't
        // leak into the home view's mouse handling (the hdivider doesn't exist
        // outside history).
        self.dragging_hdivider = false;
        self.hovering_hdivider = false;
        // Drop the per-file render caches: the home view's diff describes a
        // different file than the history view left behind.
        self.reset_diff_view();
        self.sync_active();
    }

    /// Load (or reload) the commit walk + refs + graph layout, leaving `commits`
    /// empty on an empty repo or error (the UI renders an empty-state hint).
    /// Reloads walk at least as far as what's already paged in, so a refresh
    /// never silently truncates history the user scrolled to.
    fn load_history(&mut self) {
        let want = self.commits.len().max(HISTORY_PAGE);
        match self.repo.history(want) {
            Ok(commits) => {
                self.history_loaded_all = commits.len() < want;
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
        self.selected_commit = (self.selected_commit + 1).min(self.commits.len().saturating_sub(1));
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

        self.flash = None;
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
        // Route to the active view's own hit-testing. Review and history must
        // never fall through to the status branch below, whose `staging_area`
        // rect (and `toggle_stage`) would otherwise act on a stale click target.
        match self.view {
            ViewMode::History => {
                self.history_click(pos);
                return;
            }
            ViewMode::Review => {
                self.review_click(pos);
                return;
            }
            ViewMode::Status => {}
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

    /// Route a click in the review view to its sub-pane: the file List selects a
    /// row and focuses the list, the diff pane just takes focus. Staging is inert
    /// in review, so a click in the marker column only selects — it never stages.
    fn review_click(&mut self, pos: Position) {
        let list = self.review_list_area();
        if list.contains(pos) {
            let row = self
                .review
                .as_ref()
                .map(|review| review.list_state.borrow().offset() + (pos.y - list.y) as usize)
                .unwrap_or(0);
            if let Some(review) = self.review.as_mut() {
                review.focus = ReviewFocus::List;
                if row < review.files.len() {
                    review.selected = row;
                    review.cursor = 0; // a new file starts at its first row
                }
            }
        } else if self.diff_area.get().contains(pos) {
            // Focus the diff and move the cursor to the clicked row. A click below
            // the last row just focuses (no cursor move); wheel scroll never moves
            // the cursor (that path is `review_scroll`).
            self.set_review_focus(ReviewFocus::Diff);
            let diff = self.diff_area.get();
            // Hit-test against the same clamped offset the renderer paints with
            // (diff_view.rs clamps to diff_max_scroll); a raw diff_scroll would
            // desync clicks after content shrank at max scroll (finding 5).
            let offset = self.diff_scroll.min(self.diff_max_scroll()) as usize;
            let row = offset + (pos.y - diff.y) as usize;
            let count = self.review_row_count();
            if row < count {
                if let Some(review) = self.review.as_mut() {
                    review.cursor = row;
                }
            }
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
        if self.view != ViewMode::History || !self.show_changes {
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
        match self.view {
            ViewMode::History => {
                self.history_scroll(pos, down);
                return;
            }
            ViewMode::Review => {
                self.review_scroll(pos, down);
                return;
            }
            ViewMode::Status => {}
        }
        match self.pane_at(pos) {
            Some(Focus::Diff) => self.scroll_diff(down, SCROLL_STEP),
            Some(Focus::Staging) if down => self.select_next(),
            Some(Focus::Staging) => self.select_prev(),
            None => {}
        }
    }

    /// Route a wheel event in the review view: over the list it moves the
    /// selection, over the diff it scrolls the diff.
    fn review_scroll(&mut self, pos: Position, down: bool) {
        let list = self.review_list_area();
        if list.contains(pos) {
            self.set_review_focus(ReviewFocus::List);
            self.review_move(down);
        } else if self.diff_area.get().contains(pos) {
            self.scroll_diff(down, SCROLL_STEP);
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
            self.flash = Some(Flash::error(format!("{action} failed: {err}")));
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
        // The cached highlights / row layouts describe the previous diff; drop
        // them so the new one is recomputed lazily on next render.
        self.highlight_cache.borrow_mut().clear();
        *self.sbs_rows.borrow_mut() = None;
        *self.unified_rows.borrow_mut() = None;
    }

    /// Keep whichever view is active in sync after an input event.
    fn sync_active(&mut self) {
        match self.view {
            ViewMode::Status => self.sync_diff(),
            ViewMode::History => self.sync_history_diff(),
            ViewMode::Review => self.sync_review_diff(),
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
                let prev_row = self.committed_row;
                self.load_history();
                let found = current.and_then(|id| self.commits.iter().position(|c| c.id == id));
                self.selected_commit = found.unwrap_or(0);
                self.load_commit_files();
                // A watcher reload must not yank the cursor off the file being
                // viewed: keep the row when the same commit is still selected.
                if found.is_some() {
                    self.committed_row = prev_row.min(self.commit_files.len());
                }
                self.sync_history_diff();
            }
            ViewMode::Review => self.refresh_review(),
        }
    }

    /// Re-resolve the review range and rebuild its file list only when the range
    /// actually moved. The common watcher event during an agent run is a worktree
    /// save, which can't change a committed range: if the re-resolved (base, head)
    /// OIDs are unchanged, everything is kept (the churn guard). When they change,
    /// the list is rebuilt, the selection preserved by path (falling back to the
    /// nearest valid row), and the open diff recomputed via its OID-keyed cache.
    /// A resolution failure after startup (e.g. the branch was deleted) flashes an
    /// error and keeps the stale list; the next good refresh recovers.
    fn refresh_review(&mut self) {
        let Some(review) = self.review.as_ref() else {
            return;
        };
        let (old_base, old_head) = (review.spec.base, review.spec.head);
        let old_branch_key = review.branch_key.clone();
        // Re-resolve from the stored input by borrow (no clone): the range is only
        // re-listed if the resolved tips moved. Resolving up front also feeds the
        // inbox-identity recompute below.
        let spec = match self.repo.resolve_range(&review.spec.input) {
            Ok(spec) => spec,
            Err(err) => {
                tracing::warn!("re-resolving review range failed: {err:#}");
                self.flash = Some(Flash::error(format!("review: {err}")));
                return;
            }
        };

        // Recompute the inbox identity from fresh repo state (plan finding 1). An
        // external `git checkout` while the TUI is open moves HEAD, changing both
        // which branch's inbox to read (`branch_key`) and whether the reviewed head
        // is still checked out (`authoring`, invariant §3.1.1). Both were fixed at
        // construction; refreshing them *before* the store re-read below means we
        // read the new branch's set and gate authoring on the current head. For
        // `strix diff main` the reviewed head follows HEAD (authoring stays true,
        // the inbox just changes branch); for a fixed `A..B` the head is pinned, so
        // moving HEAD off it turns authoring off.
        let head_oid = self.repo.gix().head_id().ok().map(|id| id.detach());
        let branch_key = self
            .repo
            .head_branch_key()
            .unwrap_or_else(|_| old_branch_key.clone());
        let key_changed = branch_key != old_branch_key;
        if let Some(review) = self.review.as_mut() {
            review.authoring = head_oid == Some(spec.head);
            review.branch_key = branch_key;
        }
        if key_changed {
            // The inbox changed identity; drop cached comment rows so they rebuild
            // for the new branch's set.
            self.invalidate_comment_rows();
        }

        // Re-read the store from disk (plan §3.2b) so an agent's `rm`/`add` — and
        // any new branch key above — is reflected even when the range OIDs are
        // unchanged. Cheap and write-free, so it can't drive a reload loop.
        self.reload_review_comments();

        if spec.base == old_base && spec.head == old_head {
            // Range unchanged: keep the list, selection, scroll, and warm caches.
            // A store re-read above may still have dropped comment rows (agent
            // `rm`), so clamp the cursor to the possibly-shorter row list.
            self.clear_review_error();
            self.clamp_review_cursor();
            return;
        }

        // The range moved, so relisting is unavoidable; only now clone the prior
        // selection's path (to follow it) past the churn guard.
        let review = self
            .review
            .as_ref()
            .expect("review present after the churn guard");
        let prev_selected = review.selected;
        let prev_path = review
            .files
            .get(prev_selected)
            .map(|file| file.path.clone());
        // A transient listing failure must not store the new spec: doing so would
        // arm the churn guard against the very retry that could recover.
        let files = match self.repo.range_files(&spec) {
            Ok(files) => files,
            Err(err) => {
                tracing::warn!("listing review files failed: {err:#}");
                self.flash = Some(Flash::error(format!("review: {err}")));
                return;
            }
        };
        self.clear_review_error();
        let selected = prev_path
            .and_then(|path| files.iter().position(|file| file.path == path))
            .unwrap_or_else(|| prev_selected.min(files.len().saturating_sub(1)));

        if let Some(review) = self.review.as_mut() {
            review.spec = spec;
            review.selected = selected;
            review.files = files;
            review.relist_count += 1;
            // Force the open diff to recompute against the new tips.
            review.diff_key = None;
        }
        // The range moved, so a full re-anchor pass runs against the new diff
        // (write elided when nothing moved — plan §3.2b), updating the in-memory
        // set the row model reads.
        self.reanchor_review_comments();
        self.sync_review_diff();
        // The relist rebuilt the row list; keep the cursor's index but clamp it
        // to the new count (plan §3.4).
        self.clamp_review_cursor();
    }

    /// Re-read the comment inbox from disk and replace the in-memory set. Cheap,
    /// write-free (so it can't loop the store-dir watcher), and a no-op when
    /// comments are inactive. On a load error the prior set is kept and an error
    /// flashes at most once (a corrupt store must not spam on every reload).
    fn reload_review_comments(&mut self) {
        let dir = self.repo.strix_dir();
        let (active, branch) = match self.review.as_ref() {
            Some(review) if review.authoring => (true, review.branch_key.clone()),
            _ => (false, String::new()),
        };
        if !active {
            // Inactive means no review, or the reviewed head is no longer the
            // checked-out HEAD (a `git checkout` moved off it — finding 1). Drop
            // any previously-loaded set so a now-hidden inbox can't keep rendering
            // stale comments.
            let cleared = self.review.as_mut().is_some_and(|review| {
                let had = !review.comments.is_empty();
                review.comments.clear();
                had
            });
            if cleared {
                self.invalidate_comment_rows();
            }
            return;
        }
        match comments::load(&dir) {
            Ok(store) => {
                let set = store
                    .branches
                    .get(&branch)
                    .map(|b| b.comments.clone())
                    .unwrap_or_default();
                if let Some(review) = self.review.as_mut() {
                    review.comments = set;
                }
                self.invalidate_comment_rows();
                self.clear_comment_error();
            }
            Err(err) => {
                tracing::warn!("re-reading comments store failed: {err:#}");
                self.flash_comment_error(err);
            }
        }
    }

    /// Record the range + run the write-elided re-anchor pass against the current
    /// review diff, replacing the in-memory set with the result. Runs on session
    /// open (plan §3.1.1 / §3.2) and the OID-changed refresh branch; inactive → a
    /// no-op. A store error keeps the prior set and flashes once, so a corrupt
    /// store opens comment-free rather than failing construction.
    fn reanchor_review_comments(&mut self) {
        let dir = self.repo.strix_dir();
        let (branch, spec, files) = match self.review.as_ref() {
            Some(review) if review.authoring => (
                review.branch_key.clone(),
                review.spec.clone(),
                review.files.clone(),
            ),
            _ => return,
        };
        match record_range_and_reanchor(&self.repo, &dir, &branch, &spec, &files) {
            Ok(set) => {
                if let Some(review) = self.review.as_mut() {
                    review.comments = set;
                }
                self.invalidate_comment_rows();
                self.clear_comment_error();
            }
            Err(err) => {
                tracing::warn!("loading review comments failed: {err:#}");
                self.flash_comment_error(err);
            }
        }
    }

    /// Whether the footer currently shows an error flash whose text starts with
    /// `prefix` — used to de-dup a recurring store error and to clear it once the
    /// store reads cleanly again.
    fn has_error_flash(&self, prefix: &str) -> bool {
        self.flash
            .as_ref()
            .is_some_and(|flash| flash.kind == FlashKind::Error && flash.text.starts_with(prefix))
    }

    /// Flash a comment-store error at most once: if the footer already carries one,
    /// leave it (a corrupt store recurs on every watcher reload — don't spam).
    fn flash_comment_error(&mut self, err: anyhow::Error) {
        if !self.has_error_flash("comments: ") {
            self.flash = Some(Flash::error(format!("comments: {err}")));
        }
    }

    /// Clear a lingering comment-store error flash once the store reads cleanly
    /// again (mirrors `clear_review_error`).
    fn clear_comment_error(&mut self) {
        if self.has_error_flash("comments: ") {
            self.flash = None;
        }
    }

    /// Drop the comment-bearing row caches so the next render rebuilds them (after
    /// any comment mutation or reload).
    fn invalidate_comment_rows(&self) {
        *self.unified_rows.borrow_mut() = None;
        *self.sbs_rows.borrow_mut() = None;
    }

    /// A successful review refresh clears a lingering review failure flash, so a
    /// watcher-driven recovery doesn't keep shouting about a fixed problem.
    fn clear_review_error(&mut self) {
        if self.has_error_flash("review: ") {
            self.flash = None;
        }
    }

    /// Recompute the review diff for the selected file, keyed on
    /// `(base, head, path)` so a moved tip refreshes the same file's diff. Clears
    /// the cache when the range is empty (nothing selected).
    fn sync_review_diff(&mut self) {
        if self.view != ViewMode::Review {
            return;
        }
        let Some(review) = self.review.as_ref() else {
            return;
        };
        let Some(file) = review.files.get(review.selected) else {
            if review.diff.is_some() {
                if let Some(review) = self.review.as_mut() {
                    review.diff = None;
                    review.diff_key = None;
                }
                self.reset_diff_view();
            }
            return;
        };
        // Check the cache by borrow first (`ObjectId` is `Copy`), so an unchanged
        // selection — the common per-keypress case — allocates nothing.
        let cached = review.diff_key.as_ref().is_some_and(|(base, head, path)| {
            *base == review.spec.base && *head == review.spec.head && *path == file.path
        });
        if cached {
            return;
        }
        // Genuine miss: clone only what the recompute needs.
        let file = file.clone();
        let spec = review.spec.clone();
        let key = (spec.base, spec.head, file.path.clone());
        let diff = self.repo.range_file_diff(&spec, &file);
        if let Some(review) = self.review.as_mut() {
            review.diff = Some(diff);
            review.diff_key = Some(key);
        }
        self.reset_diff_view();
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
                self.reset_diff_view();
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
        self.reset_diff_view();
    }

    /// Reset the diff pane to the top and drop the per-file render caches, which
    /// describe the diff being replaced.
    fn reset_diff_view(&mut self) {
        self.diff_scroll = 0;
        self.highlight_cache.borrow_mut().clear();
        *self.sbs_rows.borrow_mut() = None;
        *self.unified_rows.borrow_mut() = None;
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
    /// and cached. Rows reference `lines` by index; review comments inject
    /// full-width rows below their anchor (and an orphan block at the top).
    pub fn sbs_rows(&self, lines: &[DiffLine]) -> Ref<'_, Vec<SbsRow>> {
        if self.sbs_rows.borrow().is_none() {
            let (orphans, placements) = self.active_placements(lines);
            *self.sbs_rows.borrow_mut() = Some(side_by_side_rows_with_comments(
                lines,
                &orphans,
                &placements,
            ));
        }
        Ref::map(self.sbs_rows.borrow(), |cached| {
            cached.as_ref().expect("filled above")
        })
    }

    /// The unified row layout for the current diff: diff lines interleaved with
    /// comment rows (and a top orphan block), computed once per (diff, comments)
    /// and cached beside `sbs_rows`.
    pub fn unified_rows(&self, lines: &[DiffLine]) -> Ref<'_, Vec<URow>> {
        if self.unified_rows.borrow().is_none() {
            let (orphans, placements) = self.active_placements(lines);
            *self.unified_rows.borrow_mut() =
                Some(unified_rows_with_comments(lines, &orphans, &placements));
        }
        Ref::map(self.unified_rows.borrow(), |cached| {
            cached.as_ref().expect("filled above")
        })
    }

    /// The comment placements for the diff being rendered: the orphaned ids (for
    /// the top block) and a map of diff-line index → comment ids anchored just
    /// below it, both ordered by id. Empty outside a review session (comments are
    /// a Review-only feature in v1), so status/history rows are unchanged.
    fn active_placements(&self, lines: &[DiffLine]) -> (Vec<u64>, BTreeMap<usize, Vec<u64>>) {
        let Some(review) = self.review.as_ref() else {
            return (Vec::new(), BTreeMap::new());
        };
        if self.view != ViewMode::Review {
            return (Vec::new(), BTreeMap::new());
        }
        let Some(path) = review.files.get(review.selected).map(|f| f.path.as_str()) else {
            return (Vec::new(), BTreeMap::new());
        };
        comment_placements(lines, &review.comments, path)
    }

    /// The comment with `id` in the active review inbox, for rendering a row.
    pub fn review_comment(&self, id: u64) -> Option<Comment> {
        self.review.as_ref().and_then(|r| r.comment(id).cloned())
    }

    /// How many comments (anchored or orphaned) the review inbox holds for `file`.
    /// Drives the file-list `● n` badge; always 0 outside an active review.
    pub fn review_comment_count(&self, file: &str) -> usize {
        self.review
            .as_ref()
            .map(|r| r.comments.iter().filter(|c| c.file == file).count())
            .unwrap_or(0)
    }

    /// The count of orphaned comments that no diff block can show: those on files
    /// that dropped out of the range entirely. A *listed* file's orphans — binary
    /// and empty-text files included — are reachable in that file's top orphan
    /// block when it's selected (finding 2) and counted in its badge, so they're
    /// excluded here. Deliberately no binary classification: the footer once used
    /// `CommitFile.stat.binary` (numstat), which can disagree with the NUL-byte
    /// classifier that render/re-anchor use (e.g. `.gitattributes`), double-counting
    /// or dropping an orphan (finding 3). Drives the footer's `⚠ N orphaned` notice.
    pub fn orphan_footer_count(&self) -> usize {
        let Some(review) = self.review.as_ref() else {
            return 0;
        };
        review
            .comments
            .iter()
            .filter(|c| c.orphaned)
            .filter(|c| !review.files.iter().any(|f| f.path == c.file))
            .count()
    }

    /// The orphaned comments on the currently-selected review file, ordered by id.
    /// These render in the diff pane's top orphan block; for a file whose diff is
    /// empty or binary (no lines to anchor to) it's the *only* place they can
    /// appear, so the block must render regardless of diff kind (finding 2).
    /// Always empty outside an active review session in the Review view.
    pub fn selected_file_orphans(&self) -> Vec<u64> {
        let Some(review) = self.review.as_ref() else {
            return Vec::new();
        };
        if self.view != ViewMode::Review {
            return Vec::new();
        }
        let Some(path) = review.files.get(review.selected).map(|f| f.path.as_str()) else {
            return Vec::new();
        };
        let mut ids: Vec<u64> = review
            .comments
            .iter()
            .filter(|c| c.orphaned && c.file == path)
            .map(|c| c.id)
            .collect();
        ids.sort_unstable();
        ids
    }

    // --- History view: accessors for rendering + mouse ---

    /// The diff the diff pane should render, by view: the status view's selected
    /// file, or the history view's selected commit file.
    pub fn active_diff(&self) -> Option<&FileDiff> {
        match self.view {
            ViewMode::Status => self.current_diff.as_ref(),
            ViewMode::History => self.history_diff.as_ref(),
            ViewMode::Review => self.review.as_ref().and_then(|review| review.diff.as_ref()),
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
            ViewMode::Review => self
                .review
                .as_ref()
                .and_then(|review| review.files.get(review.selected))
                .map(|file| file.path.clone()),
        }
    }

    /// Whether the history view should show commit details (the `●` row is
    /// selected) rather than a file diff in the right pane.
    pub fn history_shows_details(&self) -> bool {
        self.view == ViewMode::History && self.committed_row == 0
    }

    /// Whether the diff pane is the focused pane, in any view.
    pub fn diff_focused(&self) -> bool {
        match self.view {
            ViewMode::Status => self.focus == Focus::Diff,
            ViewMode::History => self.history_focus == HistoryFocus::Diff,
            ViewMode::Review => self.review_focus() == ReviewFocus::Diff,
        }
    }

    // --- Review view: accessors for rendering + mouse ---

    /// The review range's normalized display label (e.g. `main…HEAD`), for the
    /// header. `None` outside a review session.
    pub fn review_display(&self) -> Option<&str> {
        self.review
            .as_ref()
            .map(|review| review.spec.display.as_str())
    }

    /// The review file list, in display order.
    pub fn review_files(&self) -> &[CommitFile] {
        self.review
            .as_ref()
            .map(|review| review.files.as_slice())
            .unwrap_or(&[])
    }

    /// The selected row in the review file list.
    pub fn review_selected(&self) -> usize {
        self.review
            .as_ref()
            .map(|review| review.selected)
            .unwrap_or(0)
    }

    /// Whether the review file list is the focused pane.
    pub fn review_list_focused(&self) -> bool {
        self.review_focus() == ReviewFocus::List
    }

    /// How many times the review file list has been rebuilt by a refresh. Exposed
    /// so a test can confirm an OID-unchanged reload skips relisting (the churn
    /// guard) while a moved range does rebuild.
    pub fn review_relist_count(&self) -> u64 {
        self.review
            .as_ref()
            .map(|review| review.relist_count)
            .unwrap_or(0)
    }

    /// The review list's persisted `ListState`; rendering borrows it so the
    /// scroll offset is available for mouse hit-testing.
    pub fn review_list_state_mut(&self) -> std::cell::RefMut<'_, ListState> {
        self.review
            .as_ref()
            .expect("review_list_state_mut called outside a review session")
            .list_state
            .borrow_mut()
    }

    /// Record the review file list's inner rect for this frame, so a click or
    /// wheel event on it can be hit-tested (mirrors `set_staging_area`).
    pub fn set_review_list_area(&self, area: Rect) {
        if let Some(review) = self.review.as_ref() {
            review.list_area.set(area);
        }
    }

    /// The review file list's last-rendered inner rect (for mouse-hit tests).
    pub fn review_list_area(&self) -> Rect {
        self.review
            .as_ref()
            .map(|review| review.list_area.get())
            .unwrap_or_default()
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
    /// keep a usable height — and so the result never exceeds the available
    /// `left_height` on a very short terminal (where even both minimums won't
    /// fit, the top pane gets what's left and the graph collapses).
    pub fn committed_pane_height(&self, left_height: u16) -> u16 {
        let max = left_height
            .saturating_sub(MIN_GRAPH_HEIGHT)
            .max(MIN_COMMITTED_HEIGHT)
            .min(left_height);
        self.committed_height.clamp(MIN_COMMITTED_HEIGHT, max)
    }

    /// Whether the horizontal divider shows its active affordance.
    pub fn hdivider_engaged(&self) -> bool {
        self.hovering_hdivider || self.dragging_hdivider
    }

    /// Current height (rows) of the "Committed Changes" sub-pane. Exposed for
    /// tests that drag the horizontal divider.
    pub fn committed_height(&self) -> u16 {
        self.committed_height
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
        // The two modes have different row lists, so the cursor index doesn't
        // carry over — reset to the top (plan §3.4).
        if let Some(review) = self.review.as_mut() {
            review.cursor = 0;
        }
    }

    /// Flip the line-number gutter on/off. The gutter width is computed fresh
    /// each render from `show_line_numbers` (see `ui::diff_view`), and neither
    /// render cache (`highlight_cache`, `sbs_rows`) stores a width — the cached
    /// highlight spans cover the full line and are trimmed to width after the
    /// cache lookup, and `sbs_rows` holds only layout indices — so no cache
    /// needs invalidating here.
    fn toggle_line_numbers(&mut self) {
        self.show_line_numbers = !self.show_line_numbers;
    }

    /// Advance to the next theme in `Theme::available` (presets then user themes),
    /// wrapping around. The available set is enumerated fresh here so a theme file
    /// added or removed since startup is honoured; a `theme_name` no longer in the
    /// set (its file was deleted) restarts at index 0. The highlight cache is keyed
    /// by line text only — its cached colours belong to the old theme — so it is
    /// cleared here, the verified staleness hazard. The flash shows the *resolved*
    /// canonical name, so it can never diverge from the theme now on screen.
    fn cycle_theme(&mut self) {
        let (name, theme) = Theme::cycle(&self.theme_name, self.config_dir.as_deref());
        self.theme = theme;
        self.theme_name = name.clone();
        self.highlight_cache.borrow_mut().clear();
        self.flash = Some(Flash::info(name));
    }

    /// Write `setting` to `config.toml` when a config dir was injected
    /// (production only — see `config_dir`). A write failure is logged and
    /// surfaced as an Info-kind flash; the in-app change it follows always
    /// stands regardless of whether the write succeeded.
    fn persist_setting(&mut self, setting: Setting) {
        let Some(dir) = self.config_dir.clone() else {
            tracing::debug!("no config dir injected; not persisting setting");
            return;
        };
        if let Err(err) = crate::config::persist(&dir, setting) {
            tracing::warn!("couldn't save setting: {err:#}");
            self.flash = Some(Flash::info(format!("couldn't save setting: {err}")));
        }
    }
}

/// Record `spec.input` as the branch's reviewed range and re-anchor its comments
/// against the range diff, persisting **only when something changed** (the range
/// recording or a re-anchor move). Returns the branch's comments after the pass.
///
/// The write elision is what prevents a re-anchor → store write → watcher →
/// reload loop (plan §3.2): a pass that moves nothing and re-records the same
/// range writes nothing. A corrupt/unsupported store surfaces as an `Err` (the
/// caller flashes and keeps an empty/prior set — construction never fails).
fn record_range_and_reanchor(
    repo: &Repo,
    dir: &Path,
    branch: &str,
    spec: &ReviewSpec,
    files: &[CommitFile],
) -> anyhow::Result<Vec<Comment>> {
    comments::mutate_if_changed(dir, |store| {
        let entry = store.branches.entry(branch.to_string()).or_default();
        let range_changed = entry.range.as_deref() != Some(spec.input.as_str());
        entry.range = Some(spec.input.clone());
        // The diff for a file is computed at most once, and only for files that
        // carry a comment (the engine caches per file).
        let moved = comments::reanchor(&mut entry.comments, files, |file| {
            repo.range_file_diff(spec, file)
        });
        (entry.comments.clone(), range_changed || moved)
    })
}

/// Sort rank pinning old-side comments before new-side ones, matching the SBS
/// row layout (a replaced line emits its old-side comments first).
fn side_rank(side: Side) -> u8 {
    match side {
        Side::Old => 0,
        Side::New => 1,
    }
}

/// The diff-line number on `side`, used to match a comment to its anchor line.
fn line_no(line: &DiffLine, side: Side) -> Option<usize> {
    match side {
        Side::Old => line.old_no,
        Side::New => line.new_no,
    }
}

/// Resolve a file's comments into render placements: the orphaned ids (top block)
/// and a diff-line-index → comment-ids map (rows anchored just below the matched
/// line). A non-orphaned comment matches the first diff line whose `side` number
/// equals its `line`; one that somehow can't be placed falls into the orphan
/// block rather than being dropped. Both outputs are ordered by id.
fn comment_placements(
    lines: &[DiffLine],
    comments: &[Comment],
    file: &str,
) -> (Vec<u64>, BTreeMap<usize, Vec<u64>>) {
    let mut orphans: Vec<u64> = Vec::new();
    let mut placements: BTreeMap<usize, Vec<u64>> = BTreeMap::new();
    for comment in comments.iter().filter(|c| c.file == file) {
        if comment.orphaned {
            orphans.push(comment.id);
            continue;
        }
        match lines
            .iter()
            .position(|line| line_no(line, comment.side) == Some(comment.line))
        {
            Some(index) => placements.entry(index).or_default().push(comment.id),
            None => orphans.push(comment.id),
        }
    }
    orphans.sort_unstable();
    for ids in placements.values_mut() {
        ids.sort_unstable();
    }
    (orphans, placements)
}

/// The comment ids to emit directly after diff-line `index`, ordered by id.
fn comments_after(placements: &BTreeMap<usize, Vec<u64>>, index: usize) -> &[u64] {
    placements.get(&index).map(Vec::as_slice).unwrap_or(&[])
}

/// The unified row layout: an `⚠ orphaned` block at the top, then each diff line
/// followed by any comment rows anchored to it.
fn unified_rows_with_comments(
    lines: &[DiffLine],
    orphans: &[u64],
    placements: &BTreeMap<usize, Vec<u64>>,
) -> Vec<URow> {
    let mut rows = Vec::with_capacity(lines.len() + orphans.len());
    rows.extend(orphans.iter().map(|&id| URow::Orphan(id)));
    for index in 0..lines.len() {
        rows.push(URow::Line(index));
        rows.extend(
            comments_after(placements, index)
                .iter()
                .map(|&id| URow::Comment(id)),
        );
    }
    rows
}

/// The side-by-side row layout with comments: the orphan block, then the paired
/// rows, each followed by its old-side comments then new-side comments (a Pair
/// can hold both a deletion and an addition), each side ordered by id.
fn side_by_side_rows_with_comments(
    lines: &[DiffLine],
    orphans: &[u64],
    placements: &BTreeMap<usize, Vec<u64>>,
) -> Vec<SbsRow> {
    let base = side_by_side_rows(lines);
    let mut rows = Vec::with_capacity(base.len() + orphans.len());
    rows.extend(orphans.iter().map(|&id| SbsRow::Orphan(id)));
    for row in base {
        rows.push(row);
        if let SbsRow::Pair { left, right } = row {
            if let Some(l) = left {
                rows.extend(
                    comments_after(placements, l)
                        .iter()
                        .map(|&id| SbsRow::Comment(id)),
                );
            }
            // A context row is one diff line on both sides (left == right); emit
            // its comments once. A deletion+addition Pair has distinct indices, so
            // old-side (left) comments already emitted, then new-side (right).
            if let Some(r) = right {
                if Some(r) != left {
                    rows.extend(
                        comments_after(placements, r)
                            .iter()
                            .map(|&id| SbsRow::Comment(id)),
                    );
                }
            }
        }
    }
    rows
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
