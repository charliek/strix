use std::cell::{Cell, Ref, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use ratatui::widgets::ListState;
use similar::{ChangeTag, TextDiff};
use syntect::parsing::SyntaxReference;

use crate::comments::{self, Comment, FileFacts, Scope, Side, Source};
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
/// How close in time two identical left-clicks must fall to count as a
/// double-click (plan §3.6). Semantic (same [`HitTarget`]), not pixel-based.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);
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

/// Where a click on a physical [`LayoutRow`] lands. Recorded per row so a later
/// commit (C8) can turn a click into an action without re-deriving geometry:
/// `Code` is a code/hunk line; `Body(id)` is anywhere on comment `id`'s box;
/// `Close(id)` is the box's `[x]` close cell — resolved by C8 against the finer
/// [`DiffPaneState`] `x_rects` rect, since a whole-row `hit` can't split the
/// title row's `[x]` from its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitRegion {
    Code,
    Close(u64),
    Body(u64),
}

/// The *semantic* region a click landed on, the part of a [`HitTarget`] that
/// distinguishes one double-click candidate from another (plan §3.6): a specific
/// code line (by diff-line index), a comment box, or its `[x]` close cell. Two
/// clicks are a double-click only when their whole `HitTarget`s (region included)
/// are equal, so adjacent rows or a different box never false-fire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClickRegion {
    Code(usize),
    Comment(u64),
    Close(u64),
}

/// What a left-click resolved to on the diff pane — the semantic double-click
/// key (plan §3.6). Built by [`App::hit_target`] for a click position; two
/// `Down(Left)`s are a double-click when their `HitTarget`s are equal within
/// [`DOUBLE_CLICK_WINDOW`]. `generation` is the layout-rebuild counter (a resize,
/// mode toggle, or comment mutation bumps it), so any relayout between the two
/// clicks — including a scroll that moved rows — makes the equality fail; `view`
/// and `file` guard against a view switch or a file change producing the same
/// row index. Only produced for the cursor-bearing views' (Status/Review) diff
/// rows: a click in the file list, the marker zone, or History yields `None`, so
/// those never open the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HitTarget {
    generation: u64,
    view: ViewMode,
    file: Option<String>,
    region: ClickRegion,
}

/// What a physical [`LayoutRow`] draws. Code rows carry diff-line indices (a
/// unified line, a side-by-side hunk header, or a side-by-side pair); a comment
/// box expands to several `Box` rows sharing one [`RowTarget`].
pub enum RowContent {
    /// A unified diff line (index into the file's `Vec<DiffLine>`).
    Line(usize),
    /// A side-by-side hunk header (index into `Vec<DiffLine>`), spanning both columns.
    Hunk(usize),
    /// A side-by-side pair of diff lines (either side may be blank).
    Pair {
        left: Option<usize>,
        right: Option<usize>,
        /// Word-diff emphasis for a genuinely modified pair (plan §3.7):
        /// `None` for an unchanged context pair, a pure addition/deletion, or a
        /// zipped pair whose two sides are too dissimilar to be a real edit of
        /// one another. Computed once when the layout is built (`pair_emphasis`),
        /// not per render.
        emphasis: Option<PairEmphasis>,
    },
    /// One physical row of a comment box.
    Box(BoxRow),
    /// One physical row of the in-place comment editor (plan §3.5).
    Editor(EditorPart),
}

/// Which physical part of the in-place editor box a row draws. The editor mirrors
/// a saved comment box (title / body / bottom) but is editable and caret-bearing.
pub enum EditorPart {
    /// The top border, carrying the editor title (`✎ you — <file> R<line>`).
    Title(String),
    /// A wrapped body display row; `caret` is the caret's display column within
    /// the content area when the caret falls on this row.
    Body { text: String, caret: Option<usize> },
    /// The bottom border.
    Bottom,
}

/// The render payload for one physical row of a comment box: the comment id (for
/// the `[x]` rect), whether the note has drifted (`stale` → a dim accent), and
/// which part of the box this row draws. The `● you`/`⚠ orphan` marker lives in
/// the pre-formatted title text, so it isn't repeated here.
pub struct BoxRow {
    pub id: u64,
    pub stale: bool,
    pub part: BoxPart,
}

/// Which physical part of a comment box a row draws.
pub enum BoxPart {
    /// The top border, carrying the title text (`● you — <file> R<line>`); the
    /// renderer truncates it to the box width and appends the right-aligned `[x]`.
    Title(String),
    /// A body line, already word-wrapped to the box's inner width.
    Body(String),
    /// The bottom border.
    Bottom,
}

/// One physical row of the diff pane's layout: the logical [`RowTarget`] it
/// belongs to, its 0-based offset within that target (`subrow`), the side column
/// a side-by-side box occupies (`None` for unified, full-width, and code rows),
/// the click [`HitRegion`], and the render `content`. A code line is exactly one
/// `LayoutRow`; a comment box is N rows sharing one `target`. The layout is
/// cached width-keyed (see [`App::diff_layout`]), so a resize rebuilds it while
/// preserving the logical targets.
pub struct LayoutRow {
    pub target: RowTarget,
    pub subrow: u16,
    pub side: Option<Side>,
    pub hit: HitRegion,
    pub content: RowContent,
}

/// The cached physical layout plus the `(width, mode)` it was built for, so a
/// resize or a diff-mode toggle rebuilds it (the logical `RowTarget`s survive).
struct CachedLayout {
    width: u16,
    mode: DiffMode,
    rows: Vec<LayoutRow>,
}

/// How a comment box is placed when building its rows: full-width (unified, or an
/// orphan block) at the given width, or into one side-by-side column (the side is
/// taken from the comment's own anchor side).
#[derive(Clone, Copy)]
enum BoxPlacement {
    Unified(usize),
    Sbs { left_w: usize, right_w: usize },
}

impl BoxPlacement {
    /// The row `side` (a side-by-side column, or `None` for full-width) and box
    /// width for a box anchored on `side`.
    fn column_for(self, side: Side) -> (Option<Side>, usize) {
        match self {
            BoxPlacement::Unified(w) => (None, w),
            BoxPlacement::Sbs { left_w, right_w } => match side {
                Side::Old => (Some(Side::Old), left_w),
                Side::New => (Some(Side::New), right_w),
            },
        }
    }
}

/// A side-by-side code row before comment boxes are interleaved: a hunk header or
/// a paired line (either side may be blank).
enum SbsCode {
    Hunk(usize),
    Pair {
        left: Option<usize>,
        right: Option<usize>,
    },
}

/// The *logical* selectable unit the diff cursor addresses, decoupled from the
/// physical row it renders on. `Code` carries the diff-line index (unique to the
/// row within the current mode: unified uses the line index, side-by-side uses
/// a pair's present side or the hunk index); `Comment`/`Orphan` carry the
/// comment id. Addressing a `RowTarget` rather than a physical row index is what
/// lets a later commit expand one comment into a multi-row box (or an in-place
/// editor) without touching cursor/scroll logic: the cursor still names a target,
/// the width-keyed layout still maps physical rows → targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowTarget {
    Code(usize),
    Comment(u64),
    Orphan(u64),
    /// The in-place editor box (plan §3.5). Only ever one at a time; keys route to
    /// it before the keymap, so the file cursor never navigates onto it.
    Editor,
}

/// The in-place comment editor's state (plan §3.5): a multi-line buffer plus the
/// caret, and the authoring identity captured *at open* so a checkout / watcher
/// reload mid-edit can't cross-save. The editor renders as a box at the anchor,
/// its position recomputed from `anchor` each layout build (never a captured row
/// index), and Enter persists exactly these fields.
///
/// The caret is `(line, col_char)` over the buffer's **hard lines** (`\n`
/// separated); `preferred_col` is the display column up/down try to keep. Wrapping
/// is display-only, so the caret never addresses a wrapped sub-row.
#[derive(Clone, Debug)]
struct CommentEdit {
    /// The note text, hard lines separated by `\n`.
    buffer: String,
    /// The comment's text captured at open (empty for a new comment). The save's
    /// no-op check compares `buffer` against *this*, not the current live comment —
    /// so an untouched editor writes nothing even if a concurrent writer changed the
    /// note underneath, never clobbering that change (plan §3.5).
    original_text: String,
    /// Caret as `(hard-line index, char index within that line)`.
    cursor: (usize, usize),
    /// The display column up/down aim to preserve; recomputed on any horizontal
    /// move or edit, retained across a run of up/down (plan §3.5).
    preferred_col: usize,
    /// The anchor captured at open — the box is placed by re-resolving this each
    /// frame, and a new comment persists it verbatim.
    anchor: CommentAnchor,
    /// How a *new* comment is scoped, captured at open.
    scope: Scope,
    /// The inbox key the save lands under, captured at open (checkout can't move it).
    branch_key: String,
    /// `Some(id)` when editing an existing human note (updates text only), `None`
    /// for a new comment.
    editing_id: Option<u64>,
    /// The baseline HEAD stamped on a *new* worktree comment, captured at open.
    base: Option<String>,
}

/// One editing operation on the [`CommentEdit`] buffer, so a single method covers
/// every editor key. All movement/edit indices are char-based; byte offsets are
/// derived only at the mutation site so multibyte text is never split.
enum EditOp {
    Insert(char),
    Newline,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

impl CommentEdit {
    /// A fresh editor for a new comment on `anchor`, caret at the empty start.
    fn new_comment(anchor: CommentAnchor, plan: SubmitPlan) -> Self {
        CommentEdit {
            buffer: String::new(),
            original_text: String::new(),
            cursor: (0, 0),
            preferred_col: 0,
            anchor,
            scope: plan.scope,
            branch_key: plan.branch,
            editing_id: None,
            base: plan.base,
        }
    }

    /// An editor pre-filled to edit human note `id`, caret at the buffer end.
    fn edit(text: String, anchor: CommentAnchor, id: u64, plan: SubmitPlan) -> Self {
        let last = line_count(&text).saturating_sub(1);
        let col = char_count(line_str(&text, last));
        let preferred_col = display_col(line_str(&text, last), col);
        CommentEdit {
            original_text: text.clone(),
            buffer: text,
            cursor: (last, col),
            preferred_col,
            anchor,
            scope: plan.scope,
            branch_key: plan.branch,
            editing_id: Some(id),
            base: plan.base,
        }
    }

    /// Apply one editing operation to the buffer + caret. Pure (no width): wrapping
    /// is a render concern, so up/down navigate hard lines by the preferred display
    /// column only.
    fn apply(&mut self, op: EditOp) {
        let (l, c) = self.cursor;
        match op {
            EditOp::Insert(ch) => {
                let at = byte_of(&self.buffer, l, c);
                self.buffer.insert(at, ch);
                self.cursor = (l, c + 1);
                self.recompute_preferred();
            }
            EditOp::Newline => {
                let at = byte_of(&self.buffer, l, c);
                self.buffer.insert(at, '\n');
                self.cursor = (l + 1, 0);
                self.recompute_preferred();
            }
            EditOp::Backspace => {
                if c > 0 {
                    let start = byte_of(&self.buffer, l, c - 1);
                    let end = byte_of(&self.buffer, l, c);
                    self.buffer.replace_range(start..end, "");
                    self.cursor = (l, c - 1);
                } else if l > 0 {
                    let prev_len = char_count(line_str(&self.buffer, l - 1));
                    let ls = line_start_byte(&self.buffer, l);
                    self.buffer.replace_range(ls - 1..ls, ""); // drop the joining '\n'
                    self.cursor = (l - 1, prev_len);
                }
                self.recompute_preferred();
            }
            EditOp::Delete => {
                let len = char_count(line_str(&self.buffer, l));
                if c < len {
                    let start = byte_of(&self.buffer, l, c);
                    let end = byte_of(&self.buffer, l, c + 1);
                    self.buffer.replace_range(start..end, "");
                } else if l + 1 < line_count(&self.buffer) {
                    let nl = line_start_byte(&self.buffer, l) + line_str(&self.buffer, l).len();
                    self.buffer.replace_range(nl..nl + 1, ""); // drop the next '\n'
                }
                self.recompute_preferred();
            }
            EditOp::Left => {
                if c > 0 {
                    self.cursor = (l, c - 1);
                } else if l > 0 {
                    self.cursor = (l - 1, char_count(line_str(&self.buffer, l - 1)));
                }
                self.recompute_preferred();
            }
            EditOp::Right => {
                let len = char_count(line_str(&self.buffer, l));
                if c < len {
                    self.cursor = (l, c + 1);
                } else if l + 1 < line_count(&self.buffer) {
                    self.cursor = (l + 1, 0);
                }
                self.recompute_preferred();
            }
            EditOp::Home => {
                self.cursor = (l, 0);
                self.recompute_preferred();
            }
            EditOp::End => {
                self.cursor = (l, char_count(line_str(&self.buffer, l)));
                self.recompute_preferred();
            }
            // Up/Down move between hard lines, landing at the char nearest the
            // retained preferred display column (not recomputed here).
            EditOp::Up => {
                if l > 0 {
                    let col = col_at_display(line_str(&self.buffer, l - 1), self.preferred_col);
                    self.cursor = (l - 1, col);
                }
            }
            EditOp::Down => {
                if l + 1 < line_count(&self.buffer) {
                    let col = col_at_display(line_str(&self.buffer, l + 1), self.preferred_col);
                    self.cursor = (l + 1, col);
                }
            }
        }
    }

    /// Reset the preferred display column to the caret's current display offset
    /// within its hard line (unwrapped), after any non-vertical caret change.
    fn recompute_preferred(&mut self) {
        let (l, c) = self.cursor;
        self.preferred_col = display_col(line_str(&self.buffer, l), c);
    }
}

/// A diff pane's cursor + editor state, owned by each view that has a cursor.
/// The cursor names a [`RowTarget`] (logical), not a physical row: `None` is the
/// reset state (top of the layout), resolved to physical row 0 once the layout
/// exists — a file change or mode toggle resets before the new layout is built,
/// so the concrete target isn't yet known. Scroll/metrics/row caches stay
/// App-global (History shares them and has no cursor).
#[derive(Debug, Default)]
struct DiffPaneState {
    cursor: Option<RowTarget>,
    /// The in-place editor slot: `Some` while a comment is being authored/edited
    /// in this pane (plan §3.5). Keys route here before the keymap; the layout
    /// expands to show the editor box, recomputed from the edit's anchor.
    editing: Option<CommentEdit>,
    /// The `[x]` close-cell rect of each visible comment box, recorded during
    /// render (mirrors `App::divider_x`). Keyed by comment id. C8 hit-tests a
    /// click against these to delete the note; C6 only records them.
    x_rects: RefCell<HashMap<u64, Rect>>,
}

/// A comment's anchor, captured *by value* when the in-place editor opens so a
/// watcher reload that rebuilds the diff mid-typing can't dangle it. Save persists
/// exactly these fields; if the diff moved underneath, the comment simply
/// re-anchors (or orphans) honestly on the next pass (plan §3.4).
#[derive(Clone, Debug)]
pub struct CommentAnchor {
    pub file: String,
    pub side: Side,
    pub line: usize,
    pub context: Option<String>,
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
    /// The diff pane's cursor + editor state. The cursor names a [`RowTarget`]
    /// (logical), moved by j/k/g/G/ctrl-d/u while the diff pane has focus, reset
    /// on file change and mode toggle, clamped after a relist (plan §3.4).
    pane: DiffPaneState,
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
            pane: DiffPaneState::default(),
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
    /// Keyed by file *path* only, not `(Section, path)`: the Status pane shows
    /// one net HEAD→worktree diff per file (plan §0/§3.1), so a path that appears
    /// in both the staged and unstaged sections selects the same computed diff
    /// from either row — no recompute, no divergence.
    diff_key: Option<String>,
    /// Set when an external refresh should recompute the open file's diff even
    /// though its `(section, path)` is unchanged (its content may have changed).
    /// Unlike navigating to a new file, this preserves the scroll position.
    diff_dirty: bool,
    pub diff_mode: DiffMode,
    /// Whether the diff pane shows line-number gutters (unified's 10-char
    /// number gutter, SBS's per-column 5-char gutter). The sign column in
    /// unified mode is unaffected. Toggled with `n`; from `Config.line_numbers`.
    pub show_line_numbers: bool,
    /// The diff pane's scroll offset, in physical layout rows (a `usize` since a
    /// few long comment boxes can push a diff past `u16::MAX` rows).
    pub diff_scroll: usize,
    /// Inner height (terminal rows, `u16`) and total physical content rows
    /// (`usize`) of the diff pane from the last render, so scrolling can clamp to
    /// the content in either mode. Interior-mutable because rendering takes `&App`.
    diff_viewport: Cell<u16>,
    diff_content_rows: Cell<usize>,
    /// Per-file caches that make scrolling cheap: syntax-highlighted lines keyed
    /// by their (sanitised) text, and the diff pane's physical row layout. Both
    /// are cleared whenever `sync_diff` recomputes `current_diff`, so they never
    /// outlive the file they describe. Interior-mutable because rendering, which
    /// fills them, takes `&App`.
    highlight_cache: RefCell<HashMap<String, HighlightedLine>>,
    /// The diff pane's physical [`LayoutRow`] list (code rows interleaved with
    /// multi-row comment boxes), rebuilt when the pane width or diff mode changes,
    /// or on any comment/diff mutation. `None` until first built for the current
    /// diff. This is the concrete backing store behind the C1 cursor seam.
    layout: RefCell<Option<CachedLayout>>,
    /// A monotonically-increasing counter bumped every time the physical
    /// [`layout`] is rebuilt (a resize, a diff-mode toggle, or any comment/diff
    /// mutation via `invalidate_comment_rows`). It is the `generation` field of a
    /// [`HitTarget`]: a relayout between two clicks changes it, so a stale
    /// double-click can't fire against a layout that no longer matches. `Cell`
    /// because the rebuild happens on the render path's `&self` (plan §3.6).
    layout_generation: Cell<u64>,
    /// The last recognized single left-click, for semantic double-click detection
    /// (plan §3.6): its [`HitTarget`] and the instant it happened. A second click
    /// with an equal `HitTarget` within [`DOUBLE_CLICK_WINDOW`] is a double-click.
    /// Reset (`None`) after a recognized double-click, a consumed `[x]`, any drag
    /// or scroll, and any click that isn't a plain diff-row single click.
    last_click: Option<(Instant, HitTarget)>,

    /// The status view's worktree-comment inbox (the checked-out branch's
    /// `Scope::WorkTree` set) and its diff-pane cursor/editor. Status has no
    /// dedicated state struct, so these live on `App` beside the other status
    /// fields, mirroring `ReviewState.comments`/`.pane`. `status_branch_key` is the
    /// inbox key, recomputed on refresh so an external checkout swings the inbox.
    status_comments: Vec<Comment>,
    status_branch_key: String,
    status_pane: DiffPaneState,

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
        // The status view's worktree inbox lives under the checked-out branch key,
        // derived from *this* status snapshot so the inbox key, `head_oid`, and the
        // file lists are all one consistent read (an external checkout between two
        // separate git reads could otherwise cross-mutate branches).
        let status_branch_key = status.branch_key().unwrap_or_default();
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
            layout: RefCell::new(None),
            layout_generation: Cell::new(0),
            last_click: None,
            status_comments: Vec::new(),
            status_branch_key,
            status_pane: DiffPaneState::default(),
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
        // Load the status view's worktree inbox (re-anchor + sweep). A no-op in a
        // review session; recoverable on a corrupt store, exactly like the review
        // inbox above.
        app.sync_status_comments();
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
                // Recompute the inbox key from the *same* status snapshot just read
                // (never a separate git call, which an external checkout could race)
                // so an external checkout swings the inbox to the new branch's set;
                // then re-anchor + sweep it against that snapshot (a HEAD advance
                // sweeps landed notes; the sweep is write-elided, so it can't loop
                // the watcher).
                if let Some(key) = self.status.branch_key() {
                    self.status_branch_key = key;
                }
                self.sync_status_comments();
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
        // Any keyboard input breaks a pending double-click chain (plan §3.6): a
        // real double-click is two mouse clicks with no key between them, so this
        // never drops one, and it subsumes every keyboard scroll/nav (Ctrl-D, j/k)
        // that moves `diff_scroll` without rebuilding the layout. Cleared here at
        // the entry point rather than in the reveal/scroll helpers, which a single
        // click's own reveal also runs through.
        self.last_click = None;

        if self.modal.is_some() {
            self.on_key_modal(key);
        } else if self.editing() {
            // The in-place editor captures every key *before* the keymap (plan
            // §3.5): a typed `c`/`x`/`]` inserts rather than triggering its action.
            self.on_key_editor(key);
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
            // The diff pane is cursor-driven (like review): j/k move the logical
            // cursor with act-and-reveal, so `c` always has a target line and the
            // viewport follows. The staging pane keeps moving the file selection.
            Action::Down => match self.focus {
                Focus::Staging => self.select_next(),
                Focus::Diff => self.review_move_cursor(true, 1),
            },
            Action::Up => match self.focus {
                Focus::Staging => self.select_prev(),
                Focus::Diff => self.review_move_cursor(false, 1),
            },
            Action::Top => match self.focus {
                Focus::Staging => self.selected = 0,
                Focus::Diff => self.cursor_to_edge(false),
            },
            Action::Bottom => match self.focus {
                Focus::Staging => self.selected = self.status.total().saturating_sub(1),
                Focus::Diff => self.cursor_to_edge(true),
            },
            // Ctrl-D/U move the diff cursor a half page when the diff is focused
            // (act-and-reveal), else scroll the diff viewport (the file list is
            // focused — leave the cursor put).
            Action::HalfPageDown => match self.focus {
                Focus::Diff => self.review_move_cursor(true, self.half_page() as usize),
                Focus::Staging => self.scroll_diff(true, self.half_page()),
            },
            Action::HalfPageUp => match self.focus {
                Focus::Diff => self.review_move_cursor(false, self.half_page() as usize),
                Focus::Staging => self.scroll_diff(false, self.half_page()),
            },
            Action::ToggleStage => self.toggle_stage(),
            Action::Stage => self.stage_selected(),
            Action::Unstage => self.unstage_selected(),
            // `x` discards the selected file's changes — but stays inert (neither
            // discarding nor deleting) when the *diff pane is focused* and its
            // cursor rests on a comment/orphan row, so it can never be mistaken for
            // the deletion key (`X`/`Action::DeleteComment`, below). With the file
            // list focused, `x` discards the list-selected file regardless of where
            // the hidden diff cursor sits.
            Action::Discard => {
                if !self.diff_focused() || self.cursor_comment_id().is_none() {
                    self.request_discard();
                }
            }
            // Worktree comments on the net diff: `c` adds/edits under the cursor,
            // `]`/`[` cycle notes on the changed files, `X` deletes the one under
            // the cursor.
            Action::Comment => self.status_comment_action(),
            Action::NextComment => self.cycle_comment(true),
            Action::PrevComment => self.cycle_comment(false),
            Action::DeleteComment => self.delete_cursor_comment(),
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
            // Read-only view: staging ops, commenting, and comment navigation do
            // nothing.
            Action::ToggleStage
            | Action::Stage
            | Action::Unstage
            | Action::Discard
            | Action::Comment
            | Action::NextComment
            | Action::PrevComment
            | Action::DeleteComment => {}
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
            Action::NextComment => self.cycle_comment(true),
            Action::PrevComment => self.cycle_comment(false),
            // `c` adds a comment on the code row under the cursor, or edits the
            // human comment under it. `X` deletes the one under the cursor (diff
            // focus only); on a code row it's a silent no-op.
            Action::Comment => self.review_comment_action(),
            Action::DeleteComment => self.delete_cursor_comment(),
            // Staging ops, including `x`/Discard, stay inert in a read-only review
            // (milestone 6 had `x` double as comment-delete here; that overload is
            // gone — deletion is `X` only, so `x` on a comment row is inert too).
            Action::ToggleStage | Action::Stage | Action::Unstage | Action::Discard => {}
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
            ReviewFocus::Diff => self.cursor_to_edge(bottom),
        }
    }

    /// Move the diff cursor to the first or last physical row (g/G in the diff
    /// pane), then reveal it. Shared by the review and status views' diff panes.
    fn cursor_to_edge(&mut self, bottom: bool) {
        let count = self.review_row_count();
        let idx = if bottom { count.saturating_sub(1) } else { 0 };
        let target = self.review_target_at(idx);
        self.set_review_cursor(target);
        self.review_reveal_cursor();
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
                // `None` is the top-of-layout reset; the new file's layout doesn't
                // exist yet (it's built by the trailing `sync_active`).
                review.pane.cursor = None;
            }
        }
    }

    /// Move the diff cursor by `step` physical rows (clamped), then scroll the
    /// viewport so it stays visible ("act-and-reveal", plan §3.4). The cursor is
    /// re-pinned to the [`RowTarget`] at the new physical row. A multi-row comment
    /// box shares one target across N physical rows, so a downward step never
    /// stalls inside the current box: it starts past the box's own last row, which
    /// makes `j`/`k` cross a whole box in one step (plan §3.0).
    fn review_move_cursor(&mut self, down: bool, step: usize) {
        let count = self.review_row_count();
        if count == 0 {
            return;
        }
        let (start, end) = self
            .review_cursor_span()
            .map_or((0, 1), |span| (span.start, span.end));
        let next = if down {
            // `.max(end)` skips the rest of the current target's own rows, so a
            // one-row step off a box lands on the next distinct target.
            (start + step).max(end).min(count - 1)
        } else {
            start.saturating_sub(step)
        };
        let target = self.review_target_at(next);
        self.set_review_cursor(target);
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

    /// Scroll the diff viewport so the cursor target is visible: no-op when it
    /// already is, otherwise snap the top edge to bring it into view. The cursor
    /// target may span several physical rows (a comment box), so this reveals the
    /// whole `[start, end)` span — but a box taller than the viewport can't be
    /// fully shown, so it top-aligns to the box's first row rather than looping
    /// (plan §3.4).
    fn review_reveal_cursor(&mut self) {
        if self.active_pane().is_none() {
            return;
        }
        let viewport = self.diff_viewport.get() as usize;
        if viewport == 0 {
            return;
        }
        let Some(span) = self.review_cursor_span() else {
            return;
        };
        let (start, end) = (span.start, span.end);
        let count = self.review_row_count();
        let top = self.diff_scroll;
        let new_top = if start < top || end.saturating_sub(start) >= viewport {
            // Above the viewport, or taller than it: top-align the box's first row.
            start
        } else if end > top + viewport {
            // Below the viewport and it fits: pull the box's last row into view.
            end - viewport
        } else {
            top
        };
        // Never scroll past the last full page of content.
        let max_top = count.saturating_sub(viewport);
        self.diff_scroll = new_top.min(max_top);
    }

    /// Whether the diff cursor target's first row lies within the visible
    /// viewport, tested against the same clamped offset the renderer paints with
    /// (so a wheel scroll that pushed it offscreen reads as not-visible).
    fn review_cursor_visible(&self) -> bool {
        if self.active_pane().is_none() {
            return false;
        }
        let cursor = self.review_cursor();
        let viewport = self.diff_viewport.get() as usize;
        if viewport == 0 {
            return false;
        }
        let top = self.diff_scroll.min(self.diff_max_scroll());
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

    /// The pane inner width of the last render, the key the physical layout is
    /// built for. The cursor seam reads it so an input event handled before the
    /// next render sees the same width-keyed layout the last frame drew.
    fn diff_pane_width(&self) -> u16 {
        self.diff_area.get().width
    }

    /// The number of physical rows the active diff renders for the selected file
    /// (code rows plus every row of each comment box). The cursor indexes into
    /// this count; scroll metrics count the same rows.
    fn review_row_count(&self) -> usize {
        self.diff_layout(self.diff_pane_width()).len()
    }

    /// The logical [`RowTarget`] at physical row `index` in the layout, or `None`
    /// when the row list is shorter. The physical→logical half of the cursor
    /// seam; every cursor op reads through it, so one target can span several
    /// physical rows (a comment box) without changing cursor logic.
    fn review_target_at(&self, index: usize) -> Option<RowTarget> {
        self.diff_layout(self.diff_pane_width())
            .get(index)
            .map(|row| row.target)
    }

    /// The first physical row `target` occupies, or `None` when it isn't in the
    /// current layout (e.g. its comment was removed). The logical→physical half
    /// of the cursor seam.
    fn review_index_of(&self, target: RowTarget) -> Option<usize> {
        self.diff_layout(self.diff_pane_width())
            .iter()
            .position(|row| row.target == target)
    }

    /// The `[start, end)` physical-row span of the cursor's target: its first row
    /// through the last consecutive row that shares the target (a code line is one
    /// row; a comment box is N). Drives the full-box cursor highlight, the reveal,
    /// and the box-crossing move. `None` when nothing is selectable.
    fn review_cursor_span(&self) -> Option<Range<usize>> {
        let target = self.review_cursor_target()?;
        let layout = self.diff_layout(self.diff_pane_width());
        let start = layout.iter().position(|row| row.target == target)?;
        let len = layout[start..]
            .iter()
            .take_while(|row| row.target == target)
            .count();
        Some(start..start + len)
    }

    /// The target the cursor rests on: the pinned one, or (when unset after a
    /// reset) the target at the top of the current layout.
    fn review_cursor_target(&self) -> Option<RowTarget> {
        let pinned = self.active_pane()?.cursor;
        pinned.or_else(|| self.review_target_at(0))
    }

    /// Pin the diff cursor to `target` (the write-side mirror of the read seam);
    /// `None` resets it to the top of the layout. A no-op in a view with no cursor
    /// (History).
    fn set_review_cursor(&mut self, target: Option<RowTarget>) {
        if let Some(pane) = self.active_pane_mut() {
            pane.cursor = target;
        }
    }

    /// The active view's diff-pane cursor/editor state: the status view's own
    /// (`status_pane`) or the review session's (`review.pane`). History has no
    /// cursor, so `None`. This is what lets the cursor seam serve both
    /// cursor-bearing views from one implementation.
    fn active_pane(&self) -> Option<&DiffPaneState> {
        match self.view {
            ViewMode::Status => Some(&self.status_pane),
            ViewMode::Review => self.review.as_ref().map(|review| &review.pane),
            ViewMode::History => None,
        }
    }

    fn active_pane_mut(&mut self) -> Option<&mut DiffPaneState> {
        match self.view {
            ViewMode::Status => Some(&mut self.status_pane),
            ViewMode::Review => self.review.as_mut().map(|review| &mut review.pane),
            ViewMode::History => None,
        }
    }

    /// Clamp the diff cursor after a relist or a comment deletion shrinks the row
    /// list: keep the pinned target when it still resolves, else snap to the last
    /// physical row's target (top when the list is empty).
    fn clamp_review_cursor(&mut self) {
        let Some(pinned) = self.active_pane().and_then(|pane| pane.cursor) else {
            return; // unset already means "top"; nothing to clamp
        };
        if self.review_index_of(pinned).is_some() {
            return; // still resolves
        }
        // Snap to the last physical row's target (`None`/top when the list is
        // empty: `review_target_at` of an empty layout is `None`).
        let count = self.review_row_count();
        let target = self.review_target_at(count.saturating_sub(1));
        self.set_review_cursor(target);
    }

    /// The comment id under the cursor in the selected file, or `None` when the
    /// cursor rests on a code/hunk row (so `x` there is a silent no-op).
    fn cursor_comment_id(&self) -> Option<u64> {
        match self.review_cursor_target()? {
            RowTarget::Comment(id) | RowTarget::Orphan(id) => Some(id),
            RowTarget::Code(_) | RowTarget::Editor => None,
        }
    }

    /// The row index of comment `id` in the selected file's active row list, for
    /// placing the cursor after a jump. `None` when it isn't placed in this file.
    fn comment_row_index(&self, id: u64) -> Option<usize> {
        self.review_index_of(RowTarget::Comment(id))
            .or_else(|| self.review_index_of(RowTarget::Orphan(id)))
    }

    /// Every comment on a *listed* file, in file-list order then by anchor line
    /// (ties by id) — the cycle order for `]`/`[`. Comments on files no longer in
    /// the range are excluded (they're CLI-territory, plan §3.4).
    fn ordered_comment_ids(&self) -> Vec<u64> {
        let comments = self.active_comments();
        let mut out = Vec::new();
        for path in self.active_file_paths() {
            // Sort by (line, side, id): on a replaced line the pinned SBS layout
            // emits old-side comments before new-side (see
            // `side_by_side_rows_with_comments`), so old must rank before new here
            // to visit them in on-screen order.
            let mut ids: Vec<(usize, u8, u64)> = comments
                .iter()
                .filter(|c| c.file == path)
                .map(|c| (c.line, side_rank(c.side), c.id))
                .collect();
            ids.sort_unstable();
            out.extend(ids.into_iter().map(|(_, _, id)| id));
        }
        out
    }

    /// The changed-file paths of the active view, in list order: the review
    /// session's range files, or the status view's changed files (deduped by path,
    /// staged rows first — the same path-keyed model the net diff / badge use).
    fn active_file_paths(&self) -> Vec<String> {
        match self.view {
            ViewMode::Review => self
                .review
                .as_ref()
                .map(|review| review.files.iter().map(|f| f.path.clone()).collect())
                .unwrap_or_default(),
            ViewMode::Status => {
                let mut seen = std::collections::HashSet::new();
                let mut out = Vec::new();
                for entry in self.status.staged.iter().chain(self.status.unstaged.iter()) {
                    if seen.insert(entry.path.clone()) {
                        out.push(entry.path.clone());
                    }
                }
                out
            }
            ViewMode::History => Vec::new(),
        }
    }

    /// Jump to the next / previous review comment on a listed file, wrapping.
    /// Switches the selected file when the target lives elsewhere, focuses the
    /// diff pane, places the cursor on the comment's row, and reveals it. Zero
    /// comments on listed files → an Info flash.
    fn cycle_comment(&mut self, forward: bool) {
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
        // Switch selection to the target's file if it lives elsewhere, focus the
        // diff, and recompute that file's diff now so the row lookup + reveal see
        // it (the trailing `sync_active` would be too late for in-handler placement).
        if let Some(file) = self.active_comment(target).map(|c| c.file) {
            self.select_active_file_by_path(&file);
        }
        self.focus_active_diff();
        self.sync_active();
        if let Some(row) = self.comment_row_index(target) {
            let cursor = self.review_target_at(row);
            self.set_review_cursor(cursor);
        }
        self.review_reveal_cursor();
    }

    /// Focus the diff pane in whichever cursor-bearing view is active.
    fn focus_active_diff(&mut self) {
        match self.view {
            ViewMode::Status => self.focus = Focus::Diff,
            ViewMode::Review => self.set_review_focus(ReviewFocus::Diff),
            ViewMode::History => {}
        }
    }

    /// Select the changed file at `path` in the active view (for comment nav),
    /// leaving the selection put when the path isn't listed.
    fn select_active_file_by_path(&mut self, path: &str) {
        match self.view {
            ViewMode::Review => {
                if let Some(review) = self.review.as_mut() {
                    if let Some(idx) = review.files.iter().position(|f| f.path == path) {
                        review.selected = idx;
                    }
                }
            }
            // Prefer the staged row (matches the selection ordering / badge), else
            // the unstaged one — the path-keyed dedup resolves either to the same
            // net diff.
            ViewMode::Status => {
                if let Some(idx) = self.index_of(Section::Staged, path) {
                    self.selected = idx;
                }
            }
            ViewMode::History => {}
        }
    }

    /// Delete the comment under the diff-pane cursor (`Action::DeleteComment`,
    /// `X`) — Status (worktree) and Review (range) alike, via the shared
    /// cursor/comment-set seam (diff focus only). A code/hunk-row cursor is a
    /// silent no-op, matching how milestone 6 treated `x` on a non-comment row.
    /// Resolves the cursor to a comment id, then defers to
    /// [`App::delete_comment_id`] for the transactional delete.
    fn delete_cursor_comment(&mut self) {
        if !self.diff_focused() {
            return;
        }
        // Act-and-reveal: never delete a row the user can't see. A first `X` on an
        // offscreen cursor (after a wheel scroll) only scrolls it into view; a
        // second `X`, now that it's visible, deletes (finding 1, plan §3.4).
        if !self.reveal_cursor_before_acting() {
            return;
        }
        let Some(id) = self.cursor_comment_id() else {
            return; // code / hunk row: no-op
        };
        self.delete_comment_id(id);
    }

    /// Delete comment `id` from the active view's inbox transactionally (plan
    /// §3.1.5): mutate a fresh store read, and only on success replace the
    /// in-memory set + invalidate the row caches + clamp the cursor. Shared by the
    /// `X` key (via [`delete_cursor_comment`], which resolves the cursor's id) and
    /// the `[x]` mouse click (which names the id directly). `authoring_identity`
    /// yields the inbox key + gates a non-authoring review — where no comments load
    /// to click on in the first place.
    fn delete_comment_id(&mut self, id: u64) {
        let Some(identity) = self.authoring_identity() else {
            return;
        };
        let dir = self.repo.strix_dir();
        let branch = identity.branch;
        let result = comments::mutate(&dir, |store| {
            let entry = store.branches.get_mut(&branch)?;
            let pos = entry.comments.iter().position(|c| c.id == id)?;
            entry.comments.remove(pos);
            Some(entry.comments.clone())
        });
        match result {
            Ok(Some(set)) => {
                self.apply_active_comments(set);
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

    /// Handle `c` in the review view (diff focus): open the in-place editor to add
    /// a comment on the code row under the cursor, or to edit the human comment
    /// under it. Gates per plan §3.4: a non-authoring session, the file list, a
    /// hunk row, and an agent note each Info-flash instead of opening; an
    /// offscreen cursor only reveals (act-and-reveal), no editor.
    fn review_comment_action(&mut self) {
        let Some(review) = self.review.as_ref() else {
            return;
        };
        if !review.authoring {
            self.flash = Some(Flash::info("check out the reviewed branch to comment"));
            return;
        }
        if self.review_focus() != ReviewFocus::Diff {
            self.flash = Some(Flash::info("focus the diff to comment"));
            return;
        }
        // Never act on a row the user can't see: a first `c` on an offscreen
        // cursor only scrolls it into view (mirrors `x`, finding 1 / plan §3.4).
        if !self.reveal_cursor_before_acting() {
            return;
        }
        self.open_editor_at_cursor();
    }

    /// Open the in-place editor for the row under the (already-revealed) diff
    /// cursor: edit the human note under it, refuse an agent note, or anchor a new
    /// comment on a code row (a hunk header or unanchorable row flashes). Shared by
    /// the status and review comment actions once their per-view gates have run;
    /// `active_comment`/`cursor_code_anchor` resolve against whichever view is
    /// active, so `save_comment` scopes the note correctly.
    fn open_editor_at_cursor(&mut self) {
        // Capture the authoring identity *now*, at open: a watcher `reload()` plus
        // an external checkout can move the current branch/HEAD while the editor is
        // open, and the save must land where the note was authored. `None` means
        // the active view can't author (a non-authoring review / History) — no open.
        let Some(identity) = self.authoring_identity() else {
            return;
        };
        // A comment/orphan row: edit a human note, or refuse an agent note.
        if let Some(id) = self.cursor_comment_id() {
            match self.active_comment(id) {
                Some(comment) if comment.source == Source::Human => {
                    let anchor = CommentAnchor {
                        file: comment.file.clone(),
                        side: comment.side,
                        line: comment.line,
                        context: comment.context.clone(),
                    };
                    self.set_editor(CommentEdit::edit(
                        comment.text.clone(),
                        anchor,
                        id,
                        identity,
                    ));
                }
                Some(_) => self.flash = Some(Flash::info("agent note — read-only")),
                // Vanished between the row build and now (a concurrent rm): no-op.
                None => {}
            }
            return;
        }
        // A code row: anchor a new comment, unless it's a hunk header (or a
        // binary/submodule file with no text anchor).
        match self.cursor_code_anchor() {
            Some(anchor) => self.set_editor(CommentEdit::new_comment(anchor, identity)),
            None => self.flash = Some(Flash::info("can't comment here")),
        }
    }

    /// Install `edit` as the active pane's in-place editor, drop the cached row
    /// layout so it re-expands with the editor box, and reveal the caret. A no-op
    /// in a cursor-less view (History), where `authoring_identity` already
    /// returned `None` and this is never reached.
    fn set_editor(&mut self, edit: CommentEdit) {
        if let Some(pane) = self.active_pane_mut() {
            pane.editing = Some(edit);
        }
        self.invalidate_comment_rows();
        self.editor_reveal();
    }

    /// The authoring identity for a *new or edited* comment in the active view,
    /// captured when the editor opens (plan §3.5): the target inbox key, how a new
    /// comment is scoped, and its baseline HEAD. `None` when the active view can't
    /// author — a review whose head isn't checked out (invariant §3.1.1) or
    /// History. The status view always authors (the checked-out branch is the
    /// inbox), stamping the worktree baseline.
    fn authoring_identity(&self) -> Option<SubmitPlan> {
        match self.view {
            ViewMode::Status => Some(SubmitPlan {
                branch: self.status_branch_key.clone(),
                scope: Scope::WorkTree,
                base: self.status.head_oid.clone(),
            }),
            ViewMode::Review => match self.review.as_ref() {
                Some(review) if review.authoring => Some(SubmitPlan {
                    branch: review.branch_key.clone(),
                    scope: Scope::Range {
                        range: review.spec.input.clone(),
                    },
                    base: None,
                }),
                _ => None,
            },
            ViewMode::History => None,
        }
    }

    /// Handle `c` in the status view (diff focus): open the in-place editor to add
    /// a worktree comment on the net-diff code row under the cursor, or to edit the
    /// human comment under it. Mirrors `review_comment_action`: the file list, an
    /// offscreen cursor (reveal only), a conflicted/binary file, a hunk row, and an
    /// agent note each flash instead of opening. A worktree comment stamps its
    /// scope + baseline HEAD (captured at open by `authoring_identity`).
    fn status_comment_action(&mut self) {
        if self.focus != Focus::Diff {
            self.flash = Some(Flash::info("focus the diff to comment"));
            return;
        }
        // Never act on a row the user can't see: a first `c` on an offscreen
        // cursor only scrolls it into view (act-and-reveal).
        if !self.reveal_cursor_before_acting() {
            return;
        }
        // A conflicted file has no clean HEAD-vs-worktree anchor; binary and
        // submodule files yield no code anchor below (their diff isn't `Text`), so
        // they fall through to the "can't comment here" flash.
        if let Some((_, entry)) = self.selected_file() {
            if entry.change == Change::Conflicted {
                self.flash = Some(Flash::info("can't comment on a conflicted file"));
                return;
            }
        }
        self.open_editor_at_cursor();
    }

    /// The anchor for a new comment on the code row under the cursor, or `None`
    /// on a hunk header (or a row with no anchorable line). Per plan §3.4:
    /// Addition → New/`new_no`, Deletion → Old/`old_no`, Context → New/`new_no`;
    /// `context` is the line's text. In side-by-side a replaced-line pair anchors
    /// to its new side when present, else its old side.
    fn cursor_code_anchor(&self) -> Option<CommentAnchor> {
        // Resolve the cursor's target and the file path first (owned values), so
        // the nested cache reads don't overlap the diff-line borrow below. The
        // file-path source is per-view: review's selected range file, or the
        // status view's selected changed file (`active_diff_path`).
        let RowTarget::Code(li) = self.review_cursor_target()? else {
            return None;
        };
        let file = self.active_diff_path()?;
        let FileDiff::Text(lines) = self.active_diff()? else {
            return None;
        };
        // A hunk row maps to `Code(index)` too; `anchor_for_line` returns `None`
        // for it, so no explicit hunk guard is needed here.
        lines.get(li).and_then(|line| anchor_for_line(line, file))
    }

    /// The active pane's in-place editor, if open — the single read accessor every
    /// editor query goes through.
    fn editor(&self) -> Option<&CommentEdit> {
        self.active_pane().and_then(|pane| pane.editing.as_ref())
    }

    /// Whether the active pane has the in-place editor open. Keys route to the
    /// editor while this holds (before the keymap), so a mode/history/file-change
    /// key inserts as text instead of firing — the block-while-editing pin (§3.5).
    fn editing(&self) -> bool {
        self.editor().is_some()
    }

    /// Mutate the open editor, then drop the cached layout so the box re-expands and
    /// the caret row is recomputed on next build. A no-op (no invalidation) when the
    /// editor is closed — the single write path both `editor_edit` and the paste seam
    /// funnel through.
    fn with_editor(&mut self, f: impl FnOnce(&mut CommentEdit)) {
        let Some(edit) = self
            .active_pane_mut()
            .and_then(|pane| pane.editing.as_mut())
        else {
            return;
        };
        f(edit);
        self.invalidate_comment_rows();
    }

    /// Route a key to the in-place editor (plan §3.5), run before the keymap when
    /// `editing()`. Enter saves; Esc discards; a newline is Shift+Enter, Alt+Enter,
    /// or Ctrl-J; plain chars insert (including `c`/`x`/`]`); other Ctrl/Alt chords
    /// are ignored. Ctrl-C is already handled upstream (hard-quit, before routing).
    fn on_key_editor(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            // Newline chords: Shift+Enter is unreliable on terminals without
            // keyboard-enhancement, so Alt+Enter and Ctrl-J are equal fallbacks.
            KeyCode::Enter if shift || alt => self.editor_edit(EditOp::Newline),
            KeyCode::Char('j') if ctrl => self.editor_edit(EditOp::Newline),
            KeyCode::Enter => self.save_edit(),
            KeyCode::Esc => self.discard_edit(),
            KeyCode::Backspace if !ctrl && !alt => self.editor_edit(EditOp::Backspace),
            KeyCode::Delete if !ctrl && !alt => self.editor_edit(EditOp::Delete),
            KeyCode::Left if !ctrl && !alt => self.editor_edit(EditOp::Left),
            KeyCode::Right if !ctrl && !alt => self.editor_edit(EditOp::Right),
            KeyCode::Up if !ctrl && !alt => self.editor_edit(EditOp::Up),
            KeyCode::Down if !ctrl && !alt => self.editor_edit(EditOp::Down),
            KeyCode::Home if !ctrl && !alt => self.editor_edit(EditOp::Home),
            KeyCode::End if !ctrl && !alt => self.editor_edit(EditOp::End),
            KeyCode::Char(ch) if !ctrl && !alt => self.editor_edit(EditOp::Insert(ch)),
            _ => {} // other Ctrl/Alt-modified keys are ignored
        }
        // The box may have grown/shrunk or the caret moved; keep it in view.
        self.editor_reveal();
    }

    /// Apply one editor operation to the active pane's buffer.
    fn editor_edit(&mut self, op: EditOp) {
        self.with_editor(|edit| edit.apply(op));
    }

    /// Insert `text` (which may carry newlines) at the caret — the bracketed-paste
    /// seam. The event loop's `Event::Paste` handler calls this, and tests call it
    /// directly (dump-frame/press tests can't emit a real paste). A no-op when the
    /// editor is closed, so a stray paste never leaks into the diff.
    pub fn on_paste(&mut self, text: &str) {
        if !self.editing() {
            return;
        }
        self.flash = None;
        self.editor_insert_str(text);
        self.editor_reveal();
    }

    /// Insert a (possibly multi-line) string, normalising `\r\n`/`\r` to editor
    /// newlines so pasted line breaks become real lines rather than dropped
    /// control chars.
    fn editor_insert_str(&mut self, text: &str) {
        self.with_editor(|edit| {
            for ch in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
                if ch == '\n' {
                    edit.apply(EditOp::Newline);
                } else {
                    edit.apply(EditOp::Insert(ch));
                }
            }
        });
    }

    /// Scroll the diff so the editor caret row stays visible, rerun after each
    /// keystroke (the box grows as text is typed). A box taller than the viewport
    /// can't be shown whole, so this reveals the *caret* row rather than the whole
    /// box — which never loops (plan §3.5).
    fn editor_reveal(&mut self) {
        let Some(caret) = self.editor_caret_physical_row() else {
            return;
        };
        let viewport = self.diff_viewport.get() as usize;
        if viewport == 0 {
            return;
        }
        let count = self.review_row_count();
        let top = self.diff_scroll.min(self.diff_max_scroll());
        let new_top = if caret < top {
            caret
        } else if caret >= top + viewport {
            caret - viewport + 1
        } else {
            top
        };
        let max_top = count.saturating_sub(viewport);
        self.diff_scroll = new_top.min(max_top);
    }

    /// The physical layout row the editor caret sits on — the editor body row whose
    /// caret column is set. `None` when the editor is closed.
    fn editor_caret_physical_row(&self) -> Option<usize> {
        if !self.editing() {
            return None;
        }
        self.diff_layout(self.diff_pane_width())
            .iter()
            .position(|row| {
                matches!(
                    &row.content,
                    RowContent::Editor(EditorPart::Body { caret: Some(_), .. })
                )
            })
    }

    /// Save the editor (Enter). Empty/whitespace → cancel (no write, an edit keeps
    /// its original text); a no-op edit (text unchanged) → no write; otherwise
    /// persist via `save_comment` and, only on success, replace the in-memory set +
    /// close the editor. A failed write keeps the editor open with its buffer intact
    /// and flashes (plan §3.5).
    fn save_edit(&mut self) {
        let Some(edit) = self.editor() else {
            return;
        };
        let text = edit.buffer.clone();
        let original_text = edit.original_text.clone();
        let editing_id = edit.editing_id;
        let scope = edit.scope.clone();
        let branch = edit.branch_key.clone();
        let anchor = edit.anchor.clone();
        let base = edit.base.clone();
        // Empty / whitespace-only save is a cancel — no store write (deletion is
        // the separate `X` action, never an empty save).
        if text.trim().is_empty() {
            self.close_editor();
            return;
        }
        // A no-op edit writes nothing. Compare against the text captured *at open*,
        // never the current live comment: an untouched editor must not overwrite a
        // value a concurrent writer changed underneath (codex fix #1). A local edit
        // is still elided fresh-side in `save_comment` when the stored text already
        // matches the buffer.
        if editing_id.is_some() && text == original_text {
            self.close_editor();
            return;
        }
        match self.save_comment(&scope, &branch, &anchor, &text, editing_id, &base) {
            Ok(outcome) => {
                let (id, set, flash) = match outcome {
                    SubmitOutcome::Added { id, set } => {
                        (Some(id), set, Flash::info("comment added"))
                    }
                    SubmitOutcome::Updated { id, set } => {
                        (Some(id), set, Flash::info("comment updated"))
                    }
                    SubmitOutcome::Vanished { set } => {
                        (None, set, Flash::info("comment was removed"))
                    }
                };
                // Install the persisted set into the view *only* when the current
                // inbox is still the branch the editor authored against. A checkout +
                // reload mid-edit swings the active inbox; the note still lands under
                // its captured branch, but the now-active view keeps its own set —
                // it already reloaded — rather than showing the old branch's comments
                // (codex fix #3).
                let same_branch = self.active_branch_key().as_deref() == Some(branch.as_str());
                if same_branch {
                    self.apply_active_comments(set);
                }
                self.close_editor();
                if same_branch {
                    // Land the cursor on the saved note's box (whatever it renders as).
                    if let Some(id) = id {
                        let target = self
                            .comment_row_index(id)
                            .and_then(|row| self.review_target_at(row));
                        self.set_review_cursor(target);
                    }
                }
                self.flash = Some(flash);
            }
            Err(err) => {
                tracing::warn!("saving comment failed: {err:#}");
                // The editor stays open with its buffer intact so the user can
                // retry or Esc (plan §3.5).
                self.flash = Some(Flash::error(format!("comments: {err}")));
            }
        }
    }

    /// The inbox key the active view currently reads (its own comment set lives
    /// under it); `None` in History. Gates whether a save's returned set is applied
    /// to the view — after a checkout mid-edit the active inbox differs from the
    /// branch the editor authored against (codex fix #3).
    fn active_branch_key(&self) -> Option<String> {
        match self.view {
            ViewMode::Status => Some(self.status_branch_key.clone()),
            ViewMode::Review => self.review.as_ref().map(|r| r.branch_key.clone()),
            ViewMode::History => None,
        }
    }

    /// Discard the editor (Esc): revert an edit / cancel a new comment, no write.
    fn discard_edit(&mut self) {
        self.close_editor();
    }

    /// Close the editor: drop the edit slot, invalidate the row cache (its box is
    /// gone, or a saved box takes its place), and clamp the cursor to the new list.
    fn close_editor(&mut self) {
        if let Some(pane) = self.active_pane_mut() {
            pane.editing = None;
        }
        self.invalidate_comment_rows();
        self.clamp_review_cursor();
    }

    /// Persist a comment creation/edit transactionally (plan §3.5), factored out so
    /// the editor's save path is the single writer. The add and edit paths are
    /// deliberately **separate transactions** (codex fix #2):
    ///
    /// - **New comment** — the only path that may create the branch entry
    ///   (`or_default`) or record its `active_range`; fresh-read `mutate`, push with
    ///   the captured anchor/scope/base.
    /// - **Edit** — touches *only* the existing fresh record's `text`, run through
    ///   `mutate_if_changed`: it never `or_default`s (an edit can't resurrect a
    ///   removed branch) and never `record_range`s (no stale metadata restore). The
    ///   write is elided (nothing persisted) when the branch entry is gone, the
    ///   comment id vanished (→ `Vanished`), or the stored text already equals the
    ///   buffer — so a concurrent value is never clobbered by a no-op edit.
    fn save_comment(
        &self,
        scope: &Scope,
        branch: &str,
        anchor: &CommentAnchor,
        text: &str,
        editing_id: Option<u64>,
        base: &Option<String>,
    ) -> anyhow::Result<SubmitOutcome> {
        let dir = self.repo.strix_dir();
        match editing_id {
            None => comments::mutate(&dir, |store| {
                // `take_id` scans every branch, so mint before borrowing the entry
                // (which would conflict with the scan's shared borrow).
                let id = store.take_id();
                let created_at = comments::now_secs();
                let entry = store.branches.entry(branch.to_string()).or_default();
                // The session-open pass records a review range; do it defensively
                // here too (a worktree plan carries no range).
                if let Scope::Range { range } = scope {
                    record_range(entry, range);
                }
                entry.comments.push(Comment {
                    scope: scope.clone(),
                    id,
                    source: Source::Human,
                    file: anchor.file.clone(),
                    side: anchor.side,
                    line: anchor.line,
                    text: text.to_string(),
                    context: anchor.context.clone(),
                    orphaned: false,
                    created_at,
                    base: base.clone(),
                    stale: false,
                });
                SubmitOutcome::Added {
                    id,
                    set: entry.comments.clone(),
                }
            }),
            Some(id) => comments::mutate_if_changed(&dir, |store| {
                // No `or_default`: a removed branch entry stays removed (the edit
                // vanishes rather than resurrecting it with stale metadata).
                let Some(entry) = store.branches.get_mut(branch) else {
                    return (SubmitOutcome::Vanished { set: Vec::new() }, false);
                };
                match entry.comments.iter().position(|c| c.id == id) {
                    None => (
                        SubmitOutcome::Vanished {
                            set: entry.comments.clone(),
                        },
                        false,
                    ),
                    // Fresh text already matches the buffer: no write (protects a
                    // concurrent change from a stale-buffer clobber).
                    Some(pos) if entry.comments[pos].text == text => (
                        SubmitOutcome::Updated {
                            id,
                            set: entry.comments.clone(),
                        },
                        false,
                    ),
                    Some(pos) => {
                        entry.comments[pos].text = text.to_string();
                        (
                            SubmitOutcome::Updated {
                                id,
                                set: entry.comments.clone(),
                            },
                            true,
                        )
                    }
                }
            }),
        }
    }

    /// Whether the in-place editor is open (test accessor + dump-frame guard).
    pub fn editor_open(&self) -> bool {
        self.editing()
    }

    /// The editor's current buffer, or `None` when it is closed (test accessor).
    pub fn editor_buffer(&self) -> Option<String> {
        self.editor().map(|edit| edit.buffer.clone())
    }

    /// The editor caret as `(hard-line, char)`, or `None` when closed (test accessor).
    pub fn editor_cursor(&self) -> Option<(usize, usize)> {
        self.editor().map(|edit| edit.cursor)
    }

    /// The diff cursor's physical row index (into the active mode's row list);
    /// `0` outside a review session or when the cursor is at the top. The cursor
    /// itself names a [`RowTarget`]; this projects it onto the current layout.
    /// Exposed for tests and comment navigation.
    pub fn review_cursor(&self) -> usize {
        self.review_cursor_target()
            .and_then(|target| self.review_index_of(target))
            .unwrap_or(0)
    }

    /// The `[start, end)` physical-row span to highlight while rendering — `Some`
    /// only in a cursor-bearing view (status or review) with the diff pane focused
    /// (plan §3.4); `None` otherwise, so the highlight never shows while the file
    /// list is focused or in History. A comment box spans several rows, so the
    /// whole box is highlighted, not just its first row.
    pub fn review_cursor_highlight(&self) -> Option<Range<usize>> {
        // While the in-place editor is open its box shows a caret, not the box
        // selection highlight — so the anchor row isn't highlighted underneath it.
        if self.editing() {
            None
        } else if self.diff_focused() && self.active_pane().is_some() {
            self.review_cursor_span()
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
        self.last_click = None; // a view change resets the double-click tracker (§3.6)
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
        self.last_click = None; // a view change resets the double-click tracker (§3.6)
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

    /// Handle a mouse event; returns whether the frame should be redrawn. A thin
    /// wrapper over [`App::on_mouse_at`] that stamps the current instant — the seam
    /// tests drive with explicit instants so double-click timing needs no sleeps
    /// (plan §3.6). The event loop calls `on_mouse_at` directly with `Instant::now`.
    pub fn on_mouse(&mut self, event: MouseEvent) -> bool {
        self.on_mouse_at(event, Instant::now())
    }

    /// Break a pending double-click chain on a terminal resize (plan §3.6). The
    /// layout rebuilds on the next redraw (bumping the generation), but a second
    /// click queued at the same coordinates could be dispatched *before* that
    /// redraw and match the pre-resize target — so clear the tracker eagerly here.
    /// Called from the event loop's resize arm.
    pub fn on_resize(&mut self) {
        self.last_click = None;
    }

    /// Handle a mouse event at logical time `now` (the injectable double-click
    /// clock, plan §3.6). `now` matters only for `Down(Left)`; every other kind
    /// ignores it.
    pub fn on_mouse_at(&mut self, event: MouseEvent, now: Instant) -> bool {
        // A modal captures all input, including the mouse.
        if self.modal.is_some() {
            return false;
        }
        let pos = Position {
            x: event.column,
            y: event.row,
        };

        // Free movement (no button held) only updates the hover affordance: it
        // must not clear the error toast, recompute the diff, or touch the
        // double-click tracker (motion between two clicks must not break the
        // double), and it redraws only when the highlighted state actually changes.
        if let MouseEventKind::Moved = event.kind {
            let was = self.hovering_divider || self.hovering_hdivider;
            self.hovering_divider = self.on_divider(pos);
            self.hovering_hdivider = self.on_hdivider(pos);
            return (self.hovering_divider || self.hovering_hdivider) != was;
        }

        self.flash = None;
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.on_left_down(pos, now),
            // Any drag resets the double-click tracker (plan §3.6), then continues
            // an in-progress split-bar resize.
            MouseEventKind::Drag(MouseButton::Left) => {
                self.last_click = None;
                if self.dragging_divider {
                    self.resize_changes(pos);
                } else if self.dragging_hdivider {
                    self.resize_committed(pos);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging_divider = false;
                self.dragging_hdivider = false;
            }
            // A scroll resets the tracker: the rows shift under a fixed cursor, so
            // a click before and after a scroll must not read as a double (plan §3.6).
            MouseEventKind::ScrollDown => {
                self.last_click = None;
                self.on_scroll(pos, true);
            }
            MouseEventKind::ScrollUp => {
                self.last_click = None;
                self.on_scroll(pos, false);
            }
            _ => {}
        }
        self.sync_active();
        true
    }

    /// Handle a left-button press with double-click detection (plan §3.6). The
    /// editor-open interaction from C7 stays first (a click inside keeps it, a
    /// click outside commits then routes); a click on an `[x]` deletes that note
    /// (a single click, before any double-click logic); otherwise a single click
    /// routes as before, and a second identical click within the window opens the
    /// editor (a code line → new comment, a comment box → edit it).
    fn on_left_down(&mut self, pos: Position, now: Instant) {
        // Editor open (C7): a click inside keeps editing; a click outside commits
        // (save if non-empty, else cancel) then routes. A failed save keeps the
        // editor open and swallows the click. A click while editing is never a
        // double-click candidate, so the tracker is cleared either way.
        if self.editing() {
            if self.click_in_editor(pos) {
                self.last_click = None;
                return;
            }
            self.commit_editor_for_click();
            if self.editing() {
                self.last_click = None; // save failed; the editor kept the click
                return;
            }
        }

        let target = self.hit_target(pos);
        // The `[x]` close cell deletes its note on a single click, handled before
        // the double-click logic and resetting the tracker so the next click can't
        // false-fire against it (plan §3.6).
        if let Some(HitTarget {
            region: ClickRegion::Close(id),
            ..
        }) = target
        {
            self.delete_comment_id(id);
            self.last_click = None;
            return;
        }

        // Decide double-click *before* routing (routing has no bearing on the
        // semantic target), then always route the single click so today's
        // behaviour — focus, cursor move, marker-zone stage — is preserved.
        let double = target
            .as_ref()
            .is_some_and(|t| is_double_click(self.last_click.as_ref(), now, t));
        self.route_left_down(pos);
        if double {
            self.double_click_comment(pos);
            // A recognized double-click resets the tracker so a triple-click's
            // third press can't re-fire it (plan §3.6).
            self.last_click = None;
        } else {
            // Remember a plain diff-row single click as the next double's first
            // half; anything else (the file list, marker zone, outside) clears it.
            self.last_click = target.map(|t| (now, t));
        }
    }

    /// The physical diff row `pos` falls on, or `None` when `pos` is outside the
    /// diff pane. The click→row arithmetic shared by [`App::hit_target`] and
    /// [`App::place_diff_cursor`]; clamps to the same offset the renderer paints
    /// with (diff_view.rs clamps identically).
    fn diff_row_at(&self, pos: Position) -> Option<usize> {
        let diff = self.diff_area.get();
        if !diff.contains(pos) {
            return None;
        }
        let offset = self.diff_scroll.min(self.diff_max_scroll());
        Some(offset + (pos.y - diff.y) as usize)
    }

    /// Whether `pos.x` lands in the diff-pane column a row with column `side`
    /// occupies: the left column for `Some(Old)`, the right column (past the
    /// centre divider) for `Some(New)`, any column for a full-width `None` row.
    /// Shared by the editor click-test and the double-click hit-test so a click on
    /// a side-by-side box's blank sibling column, or the centre divider, isn't
    /// mistaken for the box itself. Callers ensure `pos` is inside the diff pane
    /// (so `pos.x >= diff.x`).
    fn in_side_column(&self, pos: Position, side: Option<Side>) -> bool {
        let diff = self.diff_area.get();
        let rel_x = (pos.x - diff.x) as usize;
        let (left_w, _) = sbs_columns(diff.width);
        match side {
            // Full-width (unified, or a full-width orphan block): any column hits.
            None => true,
            // The old column is `[0, left_w)`; the divider at `left_w` is neither.
            Some(Side::Old) => rel_x < left_w,
            // The new column starts just past the divider: `[left_w + 1, width)`.
            Some(Side::New) => rel_x > left_w,
        }
    }

    /// The semantic [`HitTarget`] a click position resolves to, or `None` when it
    /// isn't a double-click candidate: outside the diff pane, past its last row, on
    /// the in-place editor, or in a view with no diff cursor (History). Reuses the
    /// C1 physical→logical seam (`review_target_at`) for the row and C6's recorded
    /// `[x]` rects (`comment_close_rect`) to split a box's close cell from its body.
    fn hit_target(&self, pos: Position) -> Option<HitTarget> {
        self.active_pane()?;
        let row_idx = self.diff_row_at(pos)?;
        // Read the physical row's target *and* column side; a side-by-side box
        // spans only its own column, so the column bounds the box hit-test.
        let (target, side) = self
            .diff_layout(self.diff_pane_width())
            .get(row_idx)
            .map(|row| (row.target, row.side))?;
        let region = match target {
            RowTarget::Code(line) => ClickRegion::Code(line),
            RowTarget::Comment(id) | RowTarget::Orphan(id) => {
                // In side-by-side the box occupies only its anchor-side column: a
                // click on the blank sibling column or the centre divider is not on
                // the box, so it's no double-click target (mirrors `click_in_editor`).
                if !self.in_side_column(pos, side) {
                    return None;
                }
                // The `[x]` cell (recorded during render) takes precedence over the
                // rest of the box, so a click on it deletes rather than edits.
                if self.comment_close_rect(id).is_some_and(|r| r.contains(pos)) {
                    ClickRegion::Close(id)
                } else {
                    ClickRegion::Comment(id)
                }
            }
            // The in-place editor isn't a double-click target (its own click
            // handling ran above).
            RowTarget::Editor => return None,
        };
        Some(HitTarget {
            generation: self.layout_generation.get(),
            view: self.view,
            file: self.active_diff_path(),
            region,
        })
    }

    /// Open the in-place editor for a recognized double-click on a diff row (plan
    /// §3.6): place the cursor on the clicked row (Review's single-click routing
    /// already did; Status's did not), then run the view's comment action — which
    /// adds on a code line, edits a human note, or flashes an agent note read-only,
    /// exactly like the `c` key. A no-op in History (no cursor / no `hit_target`).
    fn double_click_comment(&mut self, pos: Position) {
        self.place_diff_cursor(pos);
        match self.view {
            ViewMode::Status => self.status_comment_action(),
            ViewMode::Review => self.review_comment_action(),
            ViewMode::History => {}
        }
    }

    /// Focus the diff pane and move its cursor onto the clicked physical row, so
    /// the editor anchors where the user double-clicked. A no-op for a click
    /// outside the diff or past its last row (the cursor stays put).
    fn place_diff_cursor(&mut self, pos: Position) {
        self.focus_active_diff();
        let Some(row) = self.diff_row_at(pos) else {
            return;
        };
        if row < self.review_row_count() {
            let target = self.review_target_at(row);
            self.set_review_cursor(target);
        }
    }

    /// Route a left-button press that isn't consumed by the editor: grab a split
    /// bar (starting a resize), else hit-test the pane. The pre-editor `Down(Left)`
    /// behaviour, factored out so the click-outside-saves path can reuse it.
    fn route_left_down(&mut self, pos: Position) {
        if self.on_divider(pos) {
            self.dragging_divider = true;
        } else if self.on_hdivider(pos) {
            self.dragging_hdivider = true;
        } else {
            self.on_click(pos);
        }
    }

    /// Whether `pos` lands within the open editor box, so a click there keeps
    /// editing rather than committing + routing. In side-by-side the editor occupies
    /// only its anchor's column, so a click in the *other* column is outside (codex
    /// fix #4): the row's `side` bounds the horizontal hit-test.
    fn click_in_editor(&self, pos: Position) -> bool {
        let Some(row_idx) = self.diff_row_at(pos) else {
            return false;
        };
        // Copy the row's target + side out so the layout borrow doesn't outlive it.
        let Some((target, side)) = self
            .diff_layout(self.diff_pane_width())
            .get(row_idx)
            .map(|row| (row.target, row.side))
        else {
            return false;
        };
        if target != RowTarget::Editor {
            return false;
        }
        // In side-by-side the editor occupies only its anchor's column, so a click
        // in the *other* column is outside it (codex fix #4).
        self.in_side_column(pos, side)
    }

    /// Commit the editor for a click that landed outside it: save when the buffer
    /// is non-empty, else cancel (plan §3.5). On a save-write failure the editor
    /// stays open (and the caller then skips routing the click).
    fn commit_editor_for_click(&mut self) {
        let empty = self
            .editor()
            .is_none_or(|edit| edit.buffer.trim().is_empty());
        if empty {
            self.close_editor();
        } else {
            self.save_edit();
        }
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
                    review.pane.cursor = None; // a new file starts at its first row
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
            let offset = self.diff_scroll.min(self.diff_max_scroll());
            let row = offset + (pos.y - diff.y) as usize;
            let count = self.review_row_count();
            if row < count {
                let target = self.review_target_at(row);
                self.set_review_cursor(target);
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
        // Wheel scroll while editing is allowed but only moves the diff (the editor
        // stays anchored); a scroll over the file list is ignored so the file can't
        // change mid-edit (plan §3.5).
        if self.editing() {
            if self.diff_area.get().contains(pos) {
                self.scroll_diff(down, SCROLL_STEP);
            }
            return;
        }
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
        // stuck past the new end (metrics are fresh here, post-render). The step is
        // a small terminal-row count (`u16`); the offset it moves is a `usize`.
        let step = step as usize;
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
        // Path only, not (section, path) — see the `diff_key` field doc.
        let key = self.selected_file().map(|(_, entry)| entry.path.clone());
        let file_changed = key != self.diff_key;
        if !file_changed && !self.diff_dirty {
            return;
        }
        self.diff_dirty = false;
        // Compute into a local first so the immutable borrow of the file list
        // (and repo) is released before assigning the cached fields.
        let diff = self
            .selected_file()
            .map(|(_, entry)| self.repo.file_diff_head_vs_worktree(entry));
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
            // The new file's layout doesn't exist yet; reset the diff cursor to its
            // top (`None`), resolved to row 0 by the next render.
            self.status_pane.cursor = None;
        }
        // The cached highlights / row layout describe the previous diff; drop
        // them so the new one is recomputed lazily on next render.
        self.highlight_cache.borrow_mut().clear();
        *self.layout.borrow_mut() = None;
        // An in-place refresh may have shrunk the row list under a pinned cursor
        // (e.g. an edit removed lines); clamp it to the new layout. A fresh file
        // already reset the cursor above, so clamp only the same-file case.
        if !file_changed {
            self.clamp_review_cursor();
        }
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
                self.apply_review_comments(set);
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
                self.apply_review_comments(set);
                self.invalidate_comment_rows();
                self.clear_comment_error();
            }
            Err(err) => {
                tracing::warn!("loading review comments failed: {err:#}");
                self.flash_comment_error(err);
            }
        }
    }

    /// Replace the active view's in-memory comment set from a branch entry's full
    /// set, keeping only the comments of the active view's scope (so a worktree
    /// comment never leaks into a review render, nor vice versa).
    fn apply_active_comments(&mut self, full: Vec<Comment>) {
        match self.view {
            ViewMode::Status => {
                self.status_comments = full.into_iter().filter(is_worktree_scope).collect();
            }
            ViewMode::Review => self.apply_review_comments(full),
            ViewMode::History => {}
        }
    }

    /// Replace `review.comments` from a branch entry's full set, keeping only the
    /// comments scoped to *this* review's exact range (codex-#5): a worktree
    /// comment, or a range comment from a different range, is filtered out.
    fn apply_review_comments(&mut self, full: Vec<Comment>) {
        if let Some(review) = self.review.as_mut() {
            let input = review.spec.input.clone();
            review.comments = full
                .into_iter()
                .filter(|c| is_review_scope(c, &input))
                .collect();
        }
    }

    /// Re-anchor the worktree inbox and apply the §3.2 lifecycle (sweep landed
    /// notes, flag drifted ones `stale`) for the checked-out branch, replacing the
    /// in-memory `status_comments` with the surviving worktree-scoped set. A no-op
    /// in a review session (the worktree surface belongs to the status view).
    ///
    /// The whole pass runs through [`comments::mutate_if_changed`], so a settled
    /// inbox writes nothing — which is what keeps a re-anchor from waking the
    /// store-dir watcher into a reload → re-anchor loop. A corrupt/unsupported
    /// store flashes once and leaves the prior set, exactly like the review inbox.
    fn sync_status_comments(&mut self) {
        // Worktree comments live only in a status session; a review session's inbox
        // is range-scoped and driven by the review lifecycle.
        if self.review.is_some() {
            return;
        }
        let dir = self.repo.strix_dir();
        let branch = self.status_branch_key.clone();
        let current_head = self.status.head_oid.clone();
        let repo = &self.repo;
        let status = &self.status;
        let result = comments::mutate_if_changed(&dir, |store| {
            let entry = store.branches.entry(branch.clone()).or_default();
            let changed =
                comments::sweep_worktree(&mut entry.comments, current_head.as_deref(), |comment| {
                    worktree_facts(repo, status, comment)
                });
            let set: Vec<Comment> = entry
                .comments
                .iter()
                .filter(|c| is_worktree_scope(c))
                .cloned()
                .collect();
            (set, changed)
        });
        match result {
            Ok(set) => {
                self.status_comments = set;
                self.invalidate_comment_rows();
                self.clear_comment_error();
            }
            Err(err) => {
                tracing::warn!("loading worktree comments failed: {err:#}");
                self.flash_comment_error(err);
            }
        }
    }

    /// How many worktree comments (anchored, stale, or orphaned) the status inbox
    /// holds for `file`. Drives the Changes list's `● n` badge; the count is
    /// path-keyed (a file listed in both the staged and unstaged sections is one
    /// target), matching the net-diff model.
    pub fn status_comment_count(&self, file: &str) -> usize {
        self.status_comments
            .iter()
            .filter(|c| c.file == file)
            .count()
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

    /// Drop the physical row layout so the next render rebuilds it (after any
    /// comment mutation or reload — a box appeared, vanished, or changed).
    fn invalidate_comment_rows(&self) {
        *self.layout.borrow_mut() = None;
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
        *self.layout.borrow_mut() = None;
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

    /// The diff pane's physical [`LayoutRow`] list at pane width `width`, computed
    /// once per `(diff, comments, mode, width)` and cached. Code lines map 1:1 to
    /// rows; each comment box expands to several rows sharing one `RowTarget`. A
    /// changed width or diff mode rebuilds it (preserving the logical targets).
    /// This is the single backing store read by both the cursor seam and the
    /// renderer.
    pub fn diff_layout(&self, width: u16) -> Ref<'_, Vec<LayoutRow>> {
        let stale = self
            .layout
            .borrow()
            .as_ref()
            .is_none_or(|cached| cached.width != width || cached.mode != self.diff_mode);
        if stale {
            let rows = self.build_layout(width);
            // Every actual rebuild bumps the layout generation, the double-click
            // `HitTarget`'s `generation` field (plan §3.6): a resize, mode toggle,
            // or comment mutation (via `invalidate_comment_rows` → `layout = None`)
            // all funnel through here, so a relayout between two clicks is caught.
            self.layout_generation.set(self.layout_generation.get() + 1);
            *self.layout.borrow_mut() = Some(CachedLayout {
                width,
                mode: self.diff_mode,
                rows,
            });
        }
        Ref::map(self.layout.borrow(), |cached| {
            &cached.as_ref().expect("filled above").rows
        })
    }

    /// Build the physical layout for the active diff at pane width `width`: the
    /// code rows for the current mode interleaved with comment boxes, or (for an
    /// empty/binary/no diff) just the orphan boxes. When the in-place editor is
    /// open its box is injected too — after the anchored code line for a new
    /// comment, or in place of the edited comment's box (plan §3.5).
    fn build_layout(&self, width: u16) -> Vec<LayoutRow> {
        let mut rows = match self.active_diff() {
            Some(FileDiff::Text(lines)) if !lines.is_empty() => {
                let (orphans, placements) = self.active_placements(lines);
                // A new-comment editor anchors after this diff-line index (re-resolved
                // from the anchor every build, never a captured row — plan §3.5).
                let editor_line = self.editor_new_anchor_line(lines);
                match self.diff_mode {
                    DiffMode::Unified => {
                        self.build_unified_layout(lines, &orphans, &placements, width, editor_line)
                    }
                    DiffMode::SideBySide => {
                        self.build_sbs_layout(lines, &orphans, &placements, width, editor_line)
                    }
                }
            }
            // Empty/binary/no diff: the only selectable rows are orphan boxes,
            // rendered full-width (there are no columns to anchor them into).
            _ => {
                let orphans = self.selected_file_orphans();
                let mut rows = Vec::new();
                for &id in &orphans {
                    self.push_comment_box(
                        &mut rows,
                        id,
                        true,
                        BoxPlacement::Unified(width as usize),
                    );
                }
                rows
            }
        };
        // Orphan fallback: an open editor that resolved to no row — its anchor no
        // longer maps to a diff line, or the edited comment vanished — renders as a
        // full-width block at the diff top (plan §3.5).
        if self.editing()
            && !rows
                .iter()
                .any(|r| matches!(r.content, RowContent::Editor(_)))
        {
            let mut block = Vec::new();
            self.push_editor_box(&mut block, BoxPlacement::Unified(width as usize));
            block.append(&mut rows);
            rows = block;
        }
        rows
    }

    /// The unified physical layout: an orphan block at the top, then each diff
    /// line followed by any comment boxes anchored to it (full-width), and the
    /// new-comment editor box after its anchor line when `editor_line` matches.
    fn build_unified_layout(
        &self,
        lines: &[DiffLine],
        orphans: &[u64],
        placements: &BTreeMap<usize, Vec<u64>>,
        width: u16,
        editor_line: Option<usize>,
    ) -> Vec<LayoutRow> {
        let place = BoxPlacement::Unified(width as usize);
        let mut rows = Vec::with_capacity(lines.len() + orphans.len());
        for &id in orphans {
            self.push_comment_box(&mut rows, id, true, place);
        }
        for index in 0..lines.len() {
            rows.push(LayoutRow {
                target: RowTarget::Code(index),
                subrow: 0,
                side: None,
                hit: HitRegion::Code,
                content: RowContent::Line(index),
            });
            for &id in comments_after(placements, index) {
                self.push_comment_box(&mut rows, id, false, place);
            }
            if editor_line == Some(index) {
                self.push_editor_box(&mut rows, place);
            }
        }
        rows
    }

    /// The side-by-side physical layout: the orphan block, then paired code rows,
    /// each followed by its comment boxes. A box occupies its comment's anchor-side
    /// column (old for a deletion, new for an addition/context); the other column
    /// renders as blank sibling rows so the two sides stay aligned (plan §3.4).
    fn build_sbs_layout(
        &self,
        lines: &[DiffLine],
        orphans: &[u64],
        placements: &BTreeMap<usize, Vec<u64>>,
        width: u16,
        editor_line: Option<usize>,
    ) -> Vec<LayoutRow> {
        let (left_w, right_w) = sbs_columns(width);
        let place = BoxPlacement::Sbs { left_w, right_w };
        let mut rows = Vec::new();
        for &id in orphans {
            // Orphan boxes have no live anchor side; keep them full-width at top.
            self.push_comment_box(&mut rows, id, true, BoxPlacement::Unified(width as usize));
        }
        for row in side_by_side_rows(lines) {
            match row {
                SbsCode::Hunk(i) => rows.push(LayoutRow {
                    target: RowTarget::Code(i),
                    subrow: 0,
                    side: None,
                    hit: HitRegion::Code,
                    content: RowContent::Hunk(i),
                }),
                SbsCode::Pair { left, right } => {
                    let target = RowTarget::Code(
                        right
                            .or(left)
                            .expect("a side-by-side pair always has a side"),
                    );
                    // A genuine modified pair is a zipped deletion + addition
                    // (distinct indices); a context pair repeats one line on
                    // both sides (`left == right`) and never gets word emphasis.
                    let emphasis = match (left, right) {
                        (Some(l), Some(r)) if l != r => {
                            pair_emphasis(&lines[l].text, &lines[r].text)
                        }
                        _ => None,
                    };
                    rows.push(LayoutRow {
                        target,
                        subrow: 0,
                        side: None,
                        hit: HitRegion::Code,
                        content: RowContent::Pair {
                            left,
                            right,
                            emphasis,
                        },
                    });
                    // Old-side comments (on the left index) emit before new-side
                    // (on the right), matching `ordered_comment_ids`. A context
                    // Pair is one diff line on both sides (left == right), so emit
                    // its comments once (they carry the new side themselves).
                    if let Some(l) = left {
                        for &id in comments_after(placements, l) {
                            self.push_comment_box(&mut rows, id, false, place);
                        }
                    }
                    if let Some(r) = right {
                        if Some(r) != left {
                            for &id in comments_after(placements, r) {
                                self.push_comment_box(&mut rows, id, false, place);
                            }
                        }
                    }
                    // A new-comment editor anchors to this pair when its diff-line
                    // index (the anchor's side) matches either side of the pair; it
                    // takes its own anchor-side column (chosen in `push_editor_box`).
                    if editor_line.is_some() && (editor_line == left || editor_line == right) {
                        self.push_editor_box(&mut rows, place);
                    }
                }
            }
        }
        rows
    }

    /// Expand comment `id` into its physical box rows (title / wrapped body /
    /// bottom border) and append them to `rows`, all sharing one `RowTarget`. In
    /// side-by-side placement the box takes its own comment's anchor-side column;
    /// the body is word-wrapped to the box's inner width now (the width is in the
    /// cache key). A comment that vanished between placement and here is skipped.
    fn push_comment_box(
        &self,
        rows: &mut Vec<LayoutRow>,
        id: u64,
        orphan: bool,
        placement: BoxPlacement,
    ) {
        let Some(comment) = self.active_comment(id) else {
            return;
        };
        // Editing this comment? Its box becomes the in-place editor, rendered where
        // the saved box would have been (plan §3.5).
        if self.editor_edits_comment(id) {
            self.push_editor_box(rows, placement);
            return;
        }
        let (side, box_w) = placement.column_for(comment.side);
        let target = if orphan {
            RowTarget::Orphan(id)
        } else {
            RowTarget::Comment(id)
        };
        let mut parts = vec![BoxPart::Title(box_title_text(&comment, orphan))];
        parts.extend(
            wrap_comment_body(&comment.text, box_body_width(box_w))
                .into_iter()
                .map(BoxPart::Body),
        );
        parts.push(BoxPart::Bottom);
        for (subrow, part) in parts.into_iter().enumerate() {
            rows.push(LayoutRow {
                target,
                subrow: subrow as u16,
                side,
                hit: HitRegion::Body(id),
                content: RowContent::Box(BoxRow {
                    id,
                    stale: comment.stale,
                    part,
                }),
            });
        }
    }

    /// Expand the in-place editor into its physical box rows (title / wrapped +
    /// caret-marked body / bottom) sharing [`RowTarget::Editor`] (plan §3.5). The
    /// box takes the anchor-side column in side-by-side placement; the body is
    /// wrapped and the caret located now (both feed the render and the reveal).
    fn push_editor_box(&self, rows: &mut Vec<LayoutRow>, placement: BoxPlacement) {
        let Some(edit) = self.editor() else {
            return;
        };
        let (side, box_w) = placement.column_for(edit.anchor.side);
        let view = editor_view(&edit.buffer, edit.cursor, box_body_width(box_w));
        let mut parts = vec![EditorPart::Title(editor_title_text(&edit.anchor))];
        for (i, text) in view.rows.into_iter().enumerate() {
            let caret = (i == view.caret_row).then_some(view.caret_col);
            parts.push(EditorPart::Body { text, caret });
        }
        parts.push(EditorPart::Bottom);
        for (subrow, part) in parts.into_iter().enumerate() {
            rows.push(LayoutRow {
                target: RowTarget::Editor,
                subrow: subrow as u16,
                side,
                hit: HitRegion::Code,
                content: RowContent::Editor(part),
            });
        }
    }

    /// The diff-line index a *new*-comment editor anchors after, re-resolved from
    /// the editor's stored anchor (never a captured row). `None` when the editor is
    /// closed, is editing an existing comment (its box carries the editor instead),
    /// or the anchor no longer maps to a line on the selected file — the last two
    /// route to the orphan-block fallback in `build_layout`.
    fn editor_new_anchor_line(&self, lines: &[DiffLine]) -> Option<usize> {
        let edit = self.editor()?;
        if edit.editing_id.is_some() {
            return None;
        }
        let anchor = &edit.anchor;
        if self.active_diff_path().as_deref() != Some(anchor.file.as_str()) {
            return None;
        }
        lines
            .iter()
            .position(|line| line_no(line, anchor.side) == Some(anchor.line))
    }

    /// Whether the open editor is editing comment `id` (so its box renders the
    /// editor in place of the saved note).
    fn editor_edits_comment(&self, id: u64) -> bool {
        self.editor()
            .is_some_and(|edit| edit.editing_id == Some(id))
    }

    /// The comment placements for the diff being rendered: the orphaned ids (for
    /// the top block) and a map of diff-line index → comment ids anchored just
    /// below it, both ordered by id. Empty outside a review session (comments are
    /// a Review-only feature in v1), so status/history rows are unchanged.
    fn active_placements(&self, lines: &[DiffLine]) -> (Vec<u64>, BTreeMap<usize, Vec<u64>>) {
        let empty = || (Vec::new(), BTreeMap::new());
        let path = match self.selected_comment_file() {
            Some(path) => path,
            None => return empty(),
        };
        comment_placements(lines, self.active_comments(), &path)
    }

    /// The comment set the active view renders and navigates: the status view's
    /// worktree inbox or the review session's range inbox. Both are already
    /// scope-filtered when populated, so a comment of the wrong scope never leaks
    /// into the other view. Empty in History.
    fn active_comments(&self) -> &[Comment] {
        match self.view {
            ViewMode::Status => &self.status_comments,
            ViewMode::Review => self
                .review
                .as_ref()
                .map(|review| review.comments.as_slice())
                .unwrap_or(&[]),
            ViewMode::History => &[],
        }
    }

    /// The path of the file whose comments the diff pane is showing — the same file
    /// `active_diff_path` backs, except History carries no comments (→ `None`).
    fn selected_comment_file(&self) -> Option<String> {
        match self.view {
            ViewMode::History => None,
            _ => self.active_diff_path(),
        }
    }

    /// The comment with `id` in the active view's inbox, for rendering a row.
    pub fn active_comment(&self, id: u64) -> Option<Comment> {
        self.active_comments().iter().find(|c| c.id == id).cloned()
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
        let Some(path) = self.selected_comment_file() else {
            return Vec::new();
        };
        let mut ids: Vec<u64> = self
            .active_comments()
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

    /// The label shown before the path in the diff pane's title. The Status pane
    /// shows the file's net pending change, so it reads `pending · HEAD→worktree`
    /// (plan §0); History and Review keep the plain `Diff` label.
    pub fn active_diff_title(&self) -> &'static str {
        match self.view {
            ViewMode::Status => "pending · HEAD→worktree",
            ViewMode::History | ViewMode::Review => "Diff",
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
    pub fn diff_max_scroll(&self) -> usize {
        self.diff_content_rows
            .get()
            .saturating_sub(self.diff_viewport.get() as usize)
    }

    /// Record the diff pane's inner height (`viewport`, a terminal-row count) and
    /// the total physical rows the current layout renders (called while
    /// rendering), so scrolling clamps to the content.
    pub fn set_diff_metrics(&self, viewport: u16, content_rows: usize) {
        self.diff_viewport.set(viewport);
        self.diff_content_rows.set(content_rows);
    }

    /// Record the `[x]` close-cell rects of the comment boxes drawn this frame, on
    /// the active view's pane (Status or Review; History has no cursor pane, so a
    /// no-op). C8 hit-tests a click against these to delete a note.
    pub fn set_x_rects(&self, rects: HashMap<u64, Rect>) {
        if let Some(pane) = self.active_pane() {
            *pane.x_rects.borrow_mut() = rects;
        }
    }

    /// The `[x]` close-cell rect of comment `id`'s box in the active pane, if it's
    /// currently rendered. Recorded during render; consumed by C8's click routing.
    pub fn comment_close_rect(&self, id: u64) -> Option<Rect> {
        self.active_pane()
            .and_then(|pane| pane.x_rects.borrow().get(&id).copied())
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
        // A mode change relayouts the whole diff; drop any half-formed double-click
        // so a click before it can't pair with one after (plan §3.6).
        self.last_click = None;
        // The two modes have different row lists (a `Code` index means a
        // different physical row), so the cursor doesn't carry over — reset to
        // the top (plan §3.4).
        self.set_review_cursor(None);
    }

    /// Flip the line-number gutter on/off. The gutter width is computed fresh
    /// each render from `show_line_numbers` (see `ui::diff_view`); the row
    /// `layout` is keyed by pane width and mode, not the gutter (a comment box
    /// spans the full pane / column regardless), and the `highlight_cache`'s spans
    /// cover the full line and are trimmed to width after lookup — so no cache
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
        let range_changed = entry.active_range.as_deref() != Some(spec.input.as_str());
        entry.active_range = Some(spec.input.clone());
        // Re-anchor only *this* range's comments (codex-#5): a worktree comment, or
        // a comment from a different range that happens to share the branch entry,
        // is never re-anchored against this range's diff. The diff for a file is
        // computed at most once, and only for files that carry a comment.
        let moved = comments::reanchor_scoped(
            &mut entry.comments,
            |c| is_review_scope(c, &spec.input),
            files,
            |file| repo.range_file_diff(spec, file),
        );
        (entry.comments.clone(), range_changed || moved)
    })
}

/// Whether `comment` belongs to the review surface for `range_input`: a
/// range-scoped comment whose recorded range matches. An empty recorded range —
/// a CLI note authored before any review, or a legacy placeholder — matches any
/// active range (it is unscoped). A worktree comment, or a range comment from a
/// *different* range, never matches, so it is neither shown nor re-anchored in
/// this session (codex-#5).
fn is_review_scope(comment: &Comment, range_input: &str) -> bool {
    matches!(&comment.scope, Scope::Range { range } if range.is_empty() || range == range_input)
}

/// Whether `comment` belongs to the status view's worktree surface. The
/// worktree-scoped counterpart to [`is_review_scope`], used to keep range
/// comments out of the status inbox.
fn is_worktree_scope(comment: &Comment) -> bool {
    matches!(comment.scope, Scope::WorkTree)
}

/// Whether a left-click on `target` at `now` completes a double-click of the
/// previous click `prev` (plan §3.6): `prev` exists, its [`HitTarget`] equals
/// `target`, and the two fall within [`DOUBLE_CLICK_WINDOW`]. Pure and
/// clock-injected (the caller passes `now`), so tests assert the timing boundary
/// with explicit instants instead of sleeping. `saturating_duration_since` guards
/// against a non-monotonic clock rather than panicking.
fn is_double_click(prev: Option<&(Instant, HitTarget)>, now: Instant, target: &HitTarget) -> bool {
    match prev {
        Some((then, prev_target)) => {
            prev_target == target && now.saturating_duration_since(*then) <= DOUBLE_CLICK_WINDOW
        }
        None => false,
    }
}

/// The inbox + scope decisions [`App::authoring_identity`] captures once, at
/// editor open: which branch entry to write, how a *new* comment is scoped, and
/// its baseline HEAD (worktree only). A `Scope::Range` plan also records its range
/// on the branch entry — derived from `scope`, so it isn't stored twice.
struct SubmitPlan {
    branch: String,
    scope: Scope,
    base: Option<String>,
}

/// Build the [`FileFacts`] the worktree sweep needs for one comment, from the
/// current repo + status (plan §3.2 / C2c). Resolves the comment's file to a
/// current [`FileEntry`] (following a rename by `orig_path`), computes its net
/// HEAD→worktree diff, and the baseline-blob `resolved_in_head` signal.
///
/// Resolution order — [`FileFacts::Gone`] (which sweeps *unconditionally*) is
/// returned **only** for an unambiguous worktree-local deletion, never for a
/// file that merely left the status list:
/// 1. listed in status under its path → `Present` (a `Change::Deleted` entry
///    yields the deletions-only net diff and keeps the note until the deletion is
///    committed — pending deletion is not a sweep signal);
/// 2. a staged rename whose source is the comment's file → `Present` with
///    `renamed_to`;
/// 3. not listed but the path entry still exists → `Present` with an empty net
///    diff (a clean file: a commit sweeps via `resolved_in_head && orphaned`, a
///    revert under an unchanged HEAD only marks it stale);
/// 4. not listed and the path entry is truly absent:
///    - HEAD unchanged (`base == head_oid`) → [`FileFacts::Gone`] (a plain
///      worktree-local delete → sweep, per the §3.2 matrix);
///    - HEAD moved → `Present` with an empty net diff, so the sweep gate decides:
///      a committed rename-away (`context ∉ HEAD:file` → `resolved_in_head =
///      false`) stays **stale/retained** — never a blind sweep — while a committed
///      Old-side deletion (`context ∈ base ∧ ∉ HEAD`) + orphaned sweeps.
///
/// `pub(crate)` so `comments_cli`'s headless `list`/`add` (C4) can run the exact
/// same sweep engine as this module's `sync_status_comments` — no second copy
/// of the resolution order above.
pub(crate) fn worktree_facts(repo: &Repo, status: &Status, comment: &Comment) -> FileFacts {
    let all = || status.staged.iter().chain(status.unstaged.iter());
    // Direct hit: the file is a listed change under its current path.
    if let Some(entry) = all().find(|e| e.path == comment.file) {
        return FileFacts::Present {
            diff: repo.file_diff_head_vs_worktree(entry),
            renamed_to: None,
            resolved_in_head: resolved_in_head(repo, comment, &entry.path),
        };
    }
    // Renamed away in the worktree (a staged rename whose source is the file).
    if let Some(entry) = all().find(|e| e.orig_path.as_deref() == Some(comment.file.as_str())) {
        return FileFacts::Present {
            diff: repo.file_diff_head_vs_worktree(entry),
            renamed_to: Some(entry.path.clone()),
            resolved_in_head: resolved_in_head(repo, comment, &entry.path),
        };
    }
    // Not a listed change, but the path entry still exists (a clean file): the
    // empty net diff + `resolved_in_head` drive the sweep/stale decision.
    if path_entry_exists(repo.workdir(), &comment.file) {
        return present_clean(repo, comment);
    }
    // Path entry truly absent. Only an unambiguous worktree-local deletion (HEAD
    // unchanged) sweeps; if HEAD moved, the file may have been renamed-and-committed
    // away with no `orig_path` surviving in status — defer to the sweep gate rather
    // than lose the note.
    if comment.base.as_deref() == status.head_oid.as_deref() {
        FileFacts::Gone
    } else {
        present_clean(repo, comment)
    }
}

/// `FileFacts::Present` for a file with no listed change (clean, or absent after a
/// HEAD move): a synthesized entry whose net diff is empty, plus the baseline-blob
/// `resolved_in_head` signal that lets the sweep gate distinguish a landed change
/// from a rename/drift.
fn present_clean(repo: &Repo, comment: &Comment) -> FileFacts {
    let entry = FileEntry {
        path: comment.file.clone(),
        orig_path: None,
        change: Change::Modified,
    };
    FileFacts::Present {
        diff: repo.file_diff_head_vs_worktree(&entry),
        renamed_to: None,
        resolved_in_head: resolved_in_head(repo, comment, &comment.file),
    }
}

/// Whether a path *entry* exists under `workdir` — using `symlink_metadata` so a
/// broken symlink (the link entry is present) counts as existing, and only a true
/// `NotFound` counts as absent. A transient stat error (permissions, races) is
/// treated as present, so it can never trigger a false worktree-deletion sweep.
fn path_entry_exists(workdir: &Path, path: &str) -> bool {
    match std::fs::symlink_metadata(workdir.join(path)) {
        Ok(_) => true,
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Whether the comment's anchored change has landed in HEAD, computed against the
/// baseline blob so a context anchor is never mistaken for a committed add/delete
/// (plan §3.2). For an added line (New side, text `T`): `T ∉ base:file ∧ T ∈
/// HEAD:file`; for a removed line (Old side): `T ∈ base:file ∧ T ∉ HEAD:file`.
/// Necessarily `false` while HEAD hasn't moved past `base` (same blob both sides),
/// or when `context`/`base` is unavailable. This is a whole-file membership test —
/// necessary but not sufficient; the engine also requires the comment to be
/// orphaned-after-reanchor before sweeping.
fn resolved_in_head(repo: &Repo, comment: &Comment, head_path: &str) -> bool {
    let (Some(base), Some(context)) = (comment.base.as_deref(), comment.context.as_deref()) else {
        return false;
    };
    let base_blob = repo.object_bytes(&format!("{base}:{}", comment.file));
    let head_blob = repo.object_bytes(&format!("HEAD:{head_path}"));
    let in_base = blob_contains_line(&base_blob, context);
    let in_head = blob_contains_line(&head_blob, context);
    match comment.side {
        Side::New => !in_base && in_head,
        Side::Old => in_base && !in_head,
    }
}

/// Whether `bytes` (a blob) contains a line whose text equals `text`. Compared
/// against `str::lines()`, matching how [`DiffLine::text`] is trim-end'd.
fn blob_contains_line(bytes: &[u8], text: &str) -> bool {
    String::from_utf8_lossy(bytes)
        .lines()
        .any(|line| line == text)
}

/// What a save did to the store; each variant carries the branch's resulting
/// comment set for the in-memory replace, and the created/edited id for cursor
/// placement.
enum SubmitOutcome {
    Added {
        id: u64,
        set: Vec<Comment>,
    },
    Updated {
        id: u64,
        set: Vec<Comment>,
    },
    /// The edited comment was removed concurrently (a rm between open and save):
    /// the edit is dropped and the caller flashes "comment was removed".
    Vanished {
        set: Vec<Comment>,
    },
}

/// The byte offset of char index `idx` in `s`, or `s.len()` when `idx` is at or
/// past the end — so an insert/replace never splits a multibyte char.
fn char_byte_index(s: &str, idx: usize) -> usize {
    s.char_indices()
        .nth(idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

/// The byte offset where hard line `line` begins in `buf` (0 for line 0). Each
/// preceding line contributes its bytes plus the `\n` that terminates it.
fn line_start_byte(buf: &str, line: usize) -> usize {
    buf.split('\n').take(line).map(|l| l.len() + 1).sum()
}

/// The text of hard line `line` in `buf` (without its `\n`), or `""` past the end.
fn line_str(buf: &str, line: usize) -> &str {
    buf.split('\n').nth(line).unwrap_or("")
}

/// The byte offset of caret `(line, col)` in `buf`, composing the two
/// char-boundary-safe helpers so an insert/replace never splits a multibyte char.
fn byte_of(buf: &str, line: usize, col: usize) -> usize {
    line_start_byte(buf, line) + char_byte_index(line_str(buf, line), col)
}

/// The number of hard lines in `buf` (always ≥ 1; a trailing `\n` yields a final
/// empty line, matching the editor's caret model).
fn line_count(buf: &str) -> usize {
    buf.split('\n').count()
}

/// The char count of `s` (the editor addresses hard lines by char index).
fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// One char's display width for the editor: a tab shows as 4 columns, control
/// chars as 0 (dropped on display), everything else via the shared `char_width`.
/// Matches the render-side sanitisation so caret columns line up with what's drawn.
fn editor_char_width(ch: char) -> usize {
    match ch {
        '\t' => 4,
        c if c.is_control() => 0,
        c => crate::ui::char_width(c),
    }
}

/// The display column of char index `col` within hard line `line` (unwrapped) —
/// the caret's horizontal offset, the source of the up/down preferred column.
fn display_col(line: &str, col: usize) -> usize {
    line.chars().take(col).map(editor_char_width).sum()
}

/// The char index on `line` nearest to (but not past) display column `target` —
/// the inverse of [`display_col`], for landing up/down at the preferred column.
fn col_at_display(line: &str, target: usize) -> usize {
    let mut used = 0;
    let mut col = 0;
    for ch in line.chars() {
        let w = editor_char_width(ch);
        if used + w > target {
            break;
        }
        used += w;
        col += 1;
    }
    col
}

/// Record `range` as the branch's reviewed range when it has none yet — a
/// defensive mirror of the session-open pass (`record_range_and_reanchor`), so a
/// comment authored before that ran still stamps the range.
fn record_range(entry: &mut comments::Branch, range: &str) {
    if entry.active_range.is_none() {
        entry.active_range = Some(range.to_string());
    }
}

/// The anchor for a new comment on a code diff line, or `None` for a hunk header
/// (or a line missing the relevant side number). Plan §3.4: Addition → New,
/// Deletion → Old, Context → New; the anchored line's text becomes `context`.
fn anchor_for_line(line: &DiffLine, file: String) -> Option<CommentAnchor> {
    let (side, number) = match line.kind {
        LineKind::Addition => (Side::New, line.new_no),
        LineKind::Deletion => (Side::Old, line.old_no),
        LineKind::Context => (Side::New, line.new_no),
        LineKind::Hunk => return None,
    };
    Some(CommentAnchor {
        file,
        side,
        line: number?,
        context: Some(line.text.clone()),
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

/// The left/right column widths for a side-by-side pane of inner width `width`:
/// a one-cell centre divider between two roughly equal columns. Shared by the
/// layout builder and the renderer so the two can't disagree on the split.
pub(crate) fn sbs_columns(width: u16) -> (usize, usize) {
    let w = width as usize;
    let left = w.saturating_sub(1) / 2;
    let right = w.saturating_sub(left + 1);
    (left, right)
}

/// The title text for a comment box: `● you — <file> R<line>` (`⚠` and the last
/// known file/line for an orphan). The renderer truncates it to the box width.
fn box_title_text(comment: &Comment, orphan: bool) -> String {
    let marker = if orphan { '⚠' } else { '●' };
    let who = match comment.source {
        Source::Human => "you",
        Source::Agent => "agent",
    };
    format!("{marker} {who} — {} R{}", comment.file, comment.line)
}

/// The title text for the in-place editor box: `✎ you — <file> R<line>`, the pencil
/// marking it as the active input (vs a settled `●` note). Truncated by the renderer.
fn editor_title_text(anchor: &CommentAnchor) -> String {
    format!("✎ you — {} R{}", anchor.file, anchor.line)
}

/// The editor buffer wrapped to a body content width, plus the caret's position.
struct EditorView {
    /// The body display rows (each hard line contributes ≥ 1 row).
    rows: Vec<String>,
    /// The row index (into `rows`) the caret sits on.
    caret_row: usize,
    /// The caret's display column within the content area of `rows[caret_row]`.
    caret_col: usize,
}

/// Lay the editor `buffer` out for a body of `width` display columns and locate the
/// caret `(hard-line, char)`. Hard lines split on `\n`; each is char-wrapped by
/// display width (tabs → 4 cols, control dropped, wide/combining via `char_width`).
/// The caret maps to the wrapped row + column it falls in; a caret exactly filling
/// a wrapped row rolls to a fresh row so it always has a drawable cell.
fn editor_view(buffer: &str, cursor: (usize, usize), width: usize) -> EditorView {
    let width = width.max(1);
    let (cline, ccol) = cursor;
    let mut rows: Vec<String> = Vec::new();
    let mut caret_row = 0;
    let mut caret_col = 0;
    for (li, hard) in buffer.split('\n').enumerate() {
        let base = rows.len();
        let wrapped = wrap_display_row(hard, width);
        if li == cline {
            let chars: Vec<char> = hard.chars().collect();
            let ccol = ccol.min(chars.len());
            // The caret's wrapped row = the last one starting at or before `ccol`.
            let mut r = 0;
            for (i, (_, start)) in wrapped.iter().enumerate() {
                if *start <= ccol {
                    r = i;
                } else {
                    break;
                }
            }
            let start = wrapped[r].1;
            let col: usize = chars[start..ccol]
                .iter()
                .map(|&c| editor_char_width(c))
                .sum();
            // Caret at the very end of a full wrapped row: roll to a fresh row so it
            // has a cell to draw (else it would land in column `width`, off the box).
            if col >= width && ccol == chars.len() {
                rows.extend(wrapped.into_iter().map(|(text, _)| text));
                caret_row = rows.len();
                caret_col = 0;
                rows.push(String::new());
                continue;
            }
            caret_row = base + r;
            caret_col = col;
        }
        rows.extend(wrapped.into_iter().map(|(text, _)| text));
    }
    EditorView {
        rows,
        caret_row,
        caret_col,
    }
}

/// Char-wrap one hard `line` into display rows of at most `width` columns, each
/// paired with the char index (into the hard line) where it begins — so the caret
/// can be mapped back onto a wrapped row. Always yields at least one (possibly
/// empty) row; a single char wider than `width` overflows its own row rather than
/// looping.
fn wrap_display_row(line: &str, width: usize) -> Vec<(String, usize)> {
    let mut rows: Vec<(String, usize)> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0;
    let mut cur_start = 0;
    for (ci, ch) in line.chars().enumerate() {
        let w = editor_char_width(ch);
        if cur_w + w > width && cur_w > 0 {
            rows.push((std::mem::take(&mut cur), cur_start));
            cur_w = 0;
            cur_start = ci;
        }
        // Tabs display as spaces and control chars vanish, matching the render pass.
        match ch {
            '\t' => cur.push_str("    "),
            c if c.is_control() => {}
            c => cur.push(c),
        }
        cur_w += w;
    }
    rows.push((cur, cur_start));
    rows
}

/// The word-wrap width for a box's body: the box's total width less its two
/// borders and a one-column pad on each side (`│ … │`), floored at 1.
fn box_body_width(box_width: usize) -> usize {
    box_width.saturating_sub(4).max(1)
}

/// Word-wrap comment `text` to `width` display columns (unicode/CJK-aware via
/// `char_width`), honouring embedded newlines as hard breaks (CLI notes may be
/// multi-line). Always returns at least one line, so an empty note still draws a
/// body row inside its box.
fn wrap_comment_body(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    // Normalise line endings, then wrap each hard line as its own paragraph.
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    for paragraph in normalised.split('\n') {
        wrap_paragraph(&sanitize_wrap(paragraph), width, &mut out);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Greedily wrap one paragraph (no embedded newlines) into `out`, breaking at
/// spaces and hard-splitting any single word longer than `width`.
fn wrap_paragraph(text: &str, width: usize, out: &mut Vec<String>) {
    let mut line = String::new();
    let mut line_w = 0;
    for word in text.split(' ') {
        let mut word = word;
        loop {
            let word_w: usize = word.chars().map(crate::ui::char_width).sum();
            let sep = usize::from(line_w > 0);
            if line_w + sep + word_w <= width {
                if sep == 1 {
                    line.push(' ');
                    line_w += 1;
                }
                line.push_str(word);
                line_w += word_w;
                break;
            }
            if line_w > 0 {
                // The word doesn't fit after the current line; flush and retry it
                // on a fresh line.
                out.push(std::mem::take(&mut line));
                line_w = 0;
                continue;
            }
            // The line is empty and the word still overflows: hard-split it.
            let (head, rest) = split_at_width(word, width);
            out.push(head.to_string());
            word = rest;
        }
    }
    out.push(line);
}

/// Split `s` at the first char boundary whose prefix exceeds `width` display
/// columns, returning `(prefix, rest)`. Always consumes at least one char, so a
/// wide char in a one-column box can't loop forever.
fn split_at_width(s: &str, width: usize) -> (&str, &str) {
    let mut used = 0;
    for (byte, ch) in s.char_indices() {
        let w = crate::ui::char_width(ch);
        if byte > 0 && used + w > width {
            return (&s[..byte], &s[byte..]);
        }
        used += w;
    }
    (s, "")
}

/// Sanitize one line for wrapping: expand tabs to spaces and drop other control
/// characters (so a note can't inject terminal escapes), mirroring the render
/// pass's `sanitize`.
fn sanitize_wrap(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Pair the unified diff lines into side-by-side code rows (by index): context
/// lines appear on both sides; a run of deletions is zipped against the following
/// run of additions, padding the shorter side with blanks. Comment boxes are
/// interleaved by [`App::build_sbs_layout`].
fn side_by_side_rows(lines: &[DiffLine]) -> Vec<SbsCode> {
    let mut rows = Vec::new();
    let mut deletions: Vec<usize> = Vec::new();
    let mut additions: Vec<usize> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        match line.kind {
            LineKind::Deletion => deletions.push(i),
            LineKind::Addition => additions.push(i),
            LineKind::Context => {
                flush_pairs(&mut rows, &mut deletions, &mut additions);
                rows.push(SbsCode::Pair {
                    left: Some(i),
                    right: Some(i),
                });
            }
            LineKind::Hunk => {
                flush_pairs(&mut rows, &mut deletions, &mut additions);
                rows.push(SbsCode::Hunk(i));
            }
        }
    }
    flush_pairs(&mut rows, &mut deletions, &mut additions);
    rows
}

fn flush_pairs(rows: &mut Vec<SbsCode>, deletions: &mut Vec<usize>, additions: &mut Vec<usize>) {
    for i in 0..deletions.len().max(additions.len()) {
        rows.push(SbsCode::Pair {
            left: deletions.get(i).copied(),
            right: additions.get(i).copied(),
        });
    }
    deletions.clear();
    additions.clear();
}

/// Per-side changed character ranges for a side-by-side modified pair's
/// word-diff emphasis (plan §3.7): offsets into each side's *sanitized* text
/// (the same sanitization `highlighted_content` applies at render time, so
/// these ranges line up with the rendered tokens). Computed once when the
/// layout is built (see `pair_emphasis`) and read by the renderer every
/// frame — the char diff itself is never recomputed per render, only on a
/// layout rebuild (width/mode/diff/comments change).
#[derive(Clone, Debug, Default)]
pub struct PairEmphasis {
    pub old_ranges: Vec<Range<usize>>,
    pub new_ranges: Vec<Range<usize>>,
}

/// Similarity ratio (0.0–1.0, from `similar`'s char-level diff) below which a
/// zipped `Pair` is treated as a pure add+del with no word emphasis. `flush_pairs`
/// zips a run of deletions against the *next* run of additions positionally, not
/// semantically (plan §2), so two adjacent-but-unrelated lines that merely
/// landed in the same zip shouldn't light up as if one were an edit of the
/// other. 0.6 requires most of the line to still match — comfortably above
/// "half changed" while still catching typical one- or few-word edits (whose
/// ratio is well above it) and rejecting a wholesale line rewrite.
const PAIR_SIMILARITY_THRESHOLD: f32 = 0.6;

/// The word-diff emphasis for a side-by-side pair's two lines (plan §3.7), or
/// `None` when the pair isn't a genuine edit of the same line. Sanitizes both
/// sides exactly as `highlighted_content` does (so char offsets align), then
/// diffs them char-by-char with `similar`; below `PAIR_SIMILARITY_THRESHOLD`
/// the pair is discarded (probably two unrelated lines that merely zipped
/// together — plan §2/§3.7). A whitespace-only edit still clears the
/// threshold (the two texts are otherwise identical) and is emphasized like
/// any other change.
fn pair_emphasis(old_text: &str, new_text: &str) -> Option<PairEmphasis> {
    let old_clean = crate::ui::diff_view::sanitize(old_text);
    let new_clean = crate::ui::diff_view::sanitize(new_text);
    let diff = TextDiff::from_chars(old_clean.as_str(), new_clean.as_str());
    if diff.ratio() < PAIR_SIMILARITY_THRESHOLD {
        return None;
    }
    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => {
                push_char(
                    &mut old_ranges,
                    change.old_index().expect("delete has an old index"),
                );
            }
            ChangeTag::Insert => {
                push_char(
                    &mut new_ranges,
                    change.new_index().expect("insert has a new index"),
                );
            }
            ChangeTag::Equal => {}
        }
    }
    if old_ranges.is_empty() && new_ranges.is_empty() {
        return None;
    }
    Some(PairEmphasis {
        old_ranges,
        new_ranges,
    })
}

/// Append char index `idx` to `ranges`, merging into the last range when it
/// directly extends it (`similar`'s per-side indices arrive in increasing
/// order, so a run of consecutive changed chars collapses to one range).
fn push_char(ranges: &mut Vec<Range<usize>>, idx: usize) {
    match ranges.last_mut() {
        Some(last) if last.end == idx => last.end += 1,
        _ => ranges.push(idx..idx + 1),
    }
}
