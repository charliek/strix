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
    Change, CommitFile, CommitInfo, CommitStat, DiffLine, FileDiff, FileEntry, LineKind, RefLabel,
    Repo, ReviewSpec, Section, Status,
};
use crate::graph::{self, GraphRow};
use crate::keys::{Action, Keymap};
use crate::ui::theme::Theme;
use crate::ui::MarkerTone;

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
/// Display columns shifted per trackpad horizontal-scroll notch (plan §3.5).
const HSCROLL_STEP: usize = 4;
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

/// A top-level menu in the header menu bar. Enumerated by the header renderer
/// (`ui::menu`) to draw the `View` / `Theme` labels; the dropdown open-state
/// reuses it as the menu identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuId {
    View,
    Theme,
}

/// The open dropdown menu, if any. `item` indexes the **full** row list from
/// [`App::menu_items`] — separators included — so Up/Down skip separators while
/// clicks resolve by rect.
///
/// Invariant: `open_menu` and [`App::editing`] are mutually exclusive. The key
/// ladder routes to at most one (modal → editing → menu → keymap), and a mouse
/// open commits any editor first, so the two states never coexist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenMenu {
    pub menu: MenuId,
    pub item: usize,
}

/// One row of a dropdown, built live from state so its marker reflects the
/// current setting. A `Separator` is a dim rule; an `Item` is an activatable row.
pub(crate) enum MenuRow {
    Separator,
    Item {
        label: String,
        marker: Marker,
        hint: Option<&'static str>,
        command: MenuCommand,
    },
}

/// A row's left-gutter marker: a radio (single-choice group) or a checkbox
/// (toggle), each rendered in a fixed-width cell so the labels align.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    Radio(bool),
    Check(bool),
}

/// What activating a dropdown row does. Resolved by [`App::activate_command`],
/// which reuses the same mutate+persist pairs as the keyboard actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuCommand {
    SetDiffMode(DiffMode),
    SetLineNumbers(bool),
    SetWrap(bool),
    SetCrossFileScroll(bool),
    GoHome,
    EnterHistory,
    SetTheme(String),
    ToggleChangesPanel,
}

/// The recorded hit-map for the open dropdown, mirroring the `x_rects`
/// interior-mutability pattern: the whole box's `bounds` plus one entry per
/// **visible** row. Re-recorded every render and cleared to `None` when no menu
/// is open, so a stale frame can't match a click.
pub(crate) struct DropdownHit {
    /// The menu this hit-map was recorded for; validated against `open_menu`
    /// before a click acts (defensive against a stale frame).
    pub menu: MenuId,
    /// The whole box (borders included); a click inside it is consumed.
    pub bounds: Rect,
    /// The full-list index of the first visible row (the scroll window's top),
    /// so a visible row's position maps back to its full-list index for hover.
    pub window_start: usize,
    /// One entry per visible row, top to bottom: its activation command (`None`
    /// for a separator/border) and screen rect.
    pub rows: Vec<(Option<MenuCommand>, Rect)>,
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

/// How far a comment-set change has to reach when invalidating cached rows
/// (plan 007 §3.1's exactly-once contract). A standalone mutation — an agent's
/// `add`/`rm` seen by a watcher reload, a save, a delete — is `Stream`: every
/// cached section carries its own file's boxes, so all of them retire. The same
/// work run *inside* a relist is `AnchorOnly`: the enclosing branch already
/// takes the cycle's single `stream_generation` bump once every piece of state
/// is installed, and a second bump there would retire the window twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommentInvalidation {
    Stream,
    AnchorOnly,
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
    /// A file header row, which only a *strip* click can land on: the anchor's
    /// hit-test rejects headers outright (they are no double-click candidate
    /// there), while a double-click on a strip header converges on that file —
    /// the flip is the whole act (plan 007 §3.3e).
    FileHeader,
}

/// What a left-click resolved to on the diff pane — the semantic double-click
/// key (plan §3.6). Built by [`App::hit_target`] for a click position; two
/// `Down(Left)`s are a double-click when their `HitTarget`s are equal within
/// [`DOUBLE_CLICK_WINDOW`]. `generation` is the layout-rebuild counter (a resize,
/// mode toggle, or comment mutation bumps it), so any relayout between the two
/// clicks — including a scroll that moved rows — makes the equality fail; `view`
/// and `file` guard against a view switch or a file change producing the same
/// row index. Only produced for a diff row: a click in the file list or the
/// marker zone yields `None`, so those never open the editor. History resolves
/// one like any other view, but its double-click arm is inert (no comments), so
/// there it only feeds the tracker.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HitTarget {
    generation: u64,
    view: ViewMode,
    file: Option<String>,
    region: ClickRegion,
}

/// A half-open char window `[start_char, end_char)` into a line's **sanitized**
/// text (the same coordinate space as word-diff emphasis ranges, so a segment
/// never needs its ranges remapped). One wrapped display row renders exactly one
/// `Seg`; with wrap off a line is a single full-width `Seg`. `end_char` is stored
/// (not re-derived) so the render primitive needs no second width pass (plan
/// §3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seg {
    pub start_char: usize,
    pub end_char: usize,
}

impl Seg {
    /// The whole line as one segment `[0, char_count)` — the wrap-off degenerate
    /// case shared by the layout builder and the side-by-side renderer.
    pub(crate) fn full(char_count: usize) -> Self {
        Seg {
            start_char: 0,
            end_char: char_count,
        }
    }
}

/// One side of a side-by-side [`RowContent::Pair`] subrow: the diff line this
/// column draws (`line`, an index into the file's `Vec<DiffLine>`) and the char
/// window for this subrow. `seg` is `Some` for a subrow the line actually
/// reaches, `None` once the line is exhausted while the *other* side still has
/// segments — an exhausted `None` renders blank in the line's own add/del/context
/// background, distinct from an absent side (a `None` `PairCell`) which renders
/// `filler_bg` (plan §3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairCell {
    pub line: usize,
    pub seg: Option<Seg>,
}

/// What a physical [`LayoutRow`] draws. Code rows carry diff-line indices (a
/// unified line, a side-by-side hunk header, or a side-by-side pair); a comment
/// box expands to several `Box` rows sharing one [`RowTarget`].
///
/// `Clone` so a prepared section's rows can be handed to a window segment (and,
/// in a later commit, installed into the active layout) without rebuilding them
/// — the heavy payload (`emphasis`) is already behind an `Rc` (plan 006 §3.3).
#[derive(Clone)]
pub enum RowContent {
    /// One display row of a unified diff line: the line index into the file's
    /// `Vec<DiffLine>` plus the char window this row renders. With wrap off the
    /// window is the whole line; with wrap on a long line emits several `Line`
    /// rows, one per [`Seg`], sharing a `RowTarget::Code` (plan §3.3).
    Line { line: usize, seg: Seg },
    /// A side-by-side hunk header (index into `Vec<DiffLine>`), spanning both columns.
    Hunk(usize),
    /// One display row of a side-by-side pair. Each side is either absent (no line
    /// there at all → the column renders `filler_bg`) or a [`PairCell`] naming the
    /// diff line and the char window this subrow draws; a `PairCell` whose `seg`
    /// is `None` is a side whose line ran out of segments before the taller side
    /// did — it renders blank in *that line's own* background, not `filler_bg`
    /// (the two-blank distinction, plan §3.3). With wrap off a pair is one row
    /// with full-line segments; with wrap on it is `max(left_rows, right_rows)`
    /// rows sharing the pair's `RowTarget::Code` with incrementing `subrow`.
    Pair {
        left: Option<PairCell>,
        right: Option<PairCell>,
        /// Word-diff emphasis for a genuinely modified pair (plan §3.7):
        /// `None` for an unchanged context pair, a pure addition/deletion, or a
        /// zipped pair whose two sides are too dissimilar to be a real edit of
        /// one another. Computed once when the layout is built (`pair_emphasis`),
        /// shared across the pair's subrows by `Rc` (its per-side ranges are
        /// absolute char offsets, so each subrow's [`Seg`] windows them directly).
        emphasis: Option<Rc<PairEmphasis>>,
    },
    /// One physical row of a comment box.
    Box(BoxRow),
    /// One physical row of the in-place comment editor (plan §3.5).
    Editor(EditorPart),
    /// One physical row of the file's header, present only with cross-file scroll
    /// on (plan 006 §3.1). The stream's first file leads with the band alone; every
    /// file below it gets a separating rule row above the band (plan 008 §3.5), so
    /// the header is one or two rows sharing a single [`RowTarget::FileHeader`].
    FileHeader(FileHeaderRow),
}

/// Which physical part of a two-row file header a row draws (plan 008 §3.5). Both
/// rows carry the same [`FileHeaderRow`] payload, differing only here — the same
/// shape a comment box uses ([`BoxPart`]), which is what makes the pair a single
/// cursor stop without any row-count arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderPart {
    /// The separator rule above a header, drawn for every file but the first.
    Rule,
    /// The header band itself: bar, marker, prefix, chip, counts.
    Band,
}

/// The render payload of a [`RowContent::FileHeader`] row: everything the band
/// draws, resolved once when the layout is built (plan 006 §3.1) so no frame
/// re-derives it. The marker's colour is named ([`MarkerTone`]) rather than
/// resolved, because a theme cycle does not rebuild the layout. A header's rule
/// and band rows carry identical payloads apart from `part` (plan 008 §3.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHeaderRow {
    pub marker: char,
    pub tone: MarkerTone,
    /// Which of the header's physical rows this is — stamped by [`header_rows`],
    /// which is the only place a payload becomes rows; the payload builders leave
    /// it `Band`.
    pub part: HeaderPart,
    /// Everything of the file's list label before the basename, drawn dim: the
    /// directory, and the whole old path for a rename. May be empty.
    pub prefix: String,
    /// The basename, drawn on the chip.
    pub name: String,
    pub stat: CommitStat,
}

/// Which physical part of the in-place editor box a row draws. The editor mirrors
/// a saved comment box (title / body / bottom) but is editable and caret-bearing.
#[derive(Clone)]
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
#[derive(Clone)]
pub struct BoxRow {
    pub id: u64,
    pub stale: bool,
    pub part: BoxPart,
}

/// Which physical part of a comment box a row draws.
#[derive(Clone)]
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
/// and the render `content`. A code line is exactly one `LayoutRow`; a comment
/// box is N rows sharing one `target`. The layout is cached width-keyed (see
/// [`App::diff_layout`]), so a resize rebuilds it while preserving the logical
/// targets. Clicks resolve through `ClickRegion`/`WindowHit`, not through the
/// row.
#[derive(Clone)]
pub struct LayoutRow {
    pub target: RowTarget,
    pub subrow: usize,
    pub side: Option<Side>,
    pub content: RowContent,
}

/// Everything a built layout depends on: a resize (`width`), a diff-mode toggle
/// (`mode`), a wrap toggle (`wrap`), a line-number toggle (`line_numbers`, which
/// changes the gutter width and hence the wrap content width), or a cross-file
/// toggle (`cross_file`, which adds the file header — plan 006 §3.1) each rebuild
/// it. Three of the five are `bool`, so they are named rather than positional.
///
/// The file's *stream index* is deliberately not a key input: it decides only
/// whether the header carries its rule row, it is not shared by the whole stream
/// the way these five are, and a section is only ever read back at the index it
/// was built for. The anchor's own index change is caught by
/// [`CachedLayout::first`] instead (plan 008 §3.5).
#[derive(Clone, Copy, PartialEq, Eq)]
struct LayoutKey {
    width: u16,
    mode: DiffMode,
    wrap: bool,
    line_numbers: bool,
    cross_file: bool,
}

/// The cached physical layout plus the inputs it was built for. The logical
/// `RowTarget`s survive a rebuild (plan §3.3).
struct CachedLayout {
    key: LayoutKey,
    /// Whether the file was the stream's first when these rows were built — the
    /// header's rule row hangs off it, and it is *not* part of [`LayoutKey`]
    /// because it is per-file rather than pane-global. A watcher tick that adds a
    /// file sorting above the anchor changes this without changing the path, the
    /// diff or the key, so `diff_layout` compares it on every read (plan 008 §3.5).
    first: bool,
    rows: Vec<LayoutRow>,
}

/// One file's comment placements: the orphan ids that lead its layout, and the
/// diff-line index → comment-ids map for the boxes anchored under its lines.
#[derive(Default)]
struct FilePlacements {
    orphans: Vec<u64>,
    anchored: BTreeMap<usize, Vec<u64>>,
}

/// Everything a layout build needs *about the file being built* — so the same
/// builders serve the selected file and any other file in the stream (plan 006
/// §3.3). `editor` is true only on the active file's build: a section never
/// carries the in-place editor.
struct LayoutInput<'a> {
    diff: Option<&'a FileDiff>,
    placements: FilePlacements,
    /// The file's header rows: empty with cross-file scroll off, one row (the
    /// band) for the stream's first file, two (rule then band) below it.
    header: Vec<LayoutRow>,
    editor: bool,
}

/// Which file of the current view's scroll stream a section belongs to. Status
/// keys on `(Section, path)` — a path that is both staged *and* modified is two
/// stream entries whose headers differ — while `diff_key` stays deliberately
/// path-only (see its field doc: one net HEAD→worktree *diff* serves both rows).
/// The two keys answer different questions; don't unify them (plan 006 §3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileId {
    Status {
        section: Section,
        path: String,
    },
    Review {
        path: String,
    },
    /// A file of one *commit* (plan 009 §3.3). The OID is part of the identity:
    /// History's stream is re-scoped whenever the selected commit changes, and
    /// carrying the commit is what keeps a cursor or a strip hit from another
    /// commit from resolving against the current one.
    History {
        commit: gix::ObjectId,
        /// The **new** path for a rename — `CommitFile.path`, the same field
        /// `history_diff_key` and `active_path` key on; `orig_path` stays inside
        /// the `CommitFile` the diff and the header band are built from.
        path: String,
    },
}

impl FileId {
    /// The file's path, whichever view it came from.
    pub fn path(&self) -> &str {
        match self {
            FileId::Status { path, .. }
            | FileId::Review { path }
            | FileId::History { path, .. } => path,
        }
    }
}

/// Where the diff cursor is: which file of the current view's stream it
/// addresses, and which of *that file's own* [`RowTarget`]s it rests on (plan
/// 007 §3.3a). Carrying the file alongside the target is what lets the cursor
/// stand on a strip row — a file below the anchor in the stream — without the
/// target being silently reinterpreted against the anchor's layout, which a bare
/// `RowTarget` would be.
///
/// An address whose `file` is the anchor is *converged* (every write site before
/// plan 007 produces one); anything else is *divergent* and lives under the
/// divergence invariant (§3.3b): its file must be in the prepared window with
/// the diff pane focused, or the cursor drops back to `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorAddress {
    pub file: FileId,
    pub target: RowTarget,
}

/// What happens to the diff cursor when the anchor flips under it
/// ([`App::flip_anchor`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FlipCursor {
    /// 006 §3.2d's contract: the arriving anchor starts with no cursor at all.
    /// The wheel's mode — and the firewall that keeps mouse scrolling from
    /// inheriting an address the keyboard walked to.
    Reset,
    /// Plan 007 §3.3h: the address survives the flip, captured before the
    /// selection moves and re-installed after it. The mode every flip a *cursor*
    /// movement triggered uses, so the row the user is pointing at is still the
    /// row they are pointing at once the title has changed.
    Keep,
}

/// One file's prepared contribution to the stream: the diff computed for it and
/// the physical rows built from that diff. Named `FileSection` because `Section`
/// alone is the staged/unstaged enum (`git::Section`).
pub struct FileSection {
    pub diff: FileDiff,
    pub rows: Vec<LayoutRow>,
}

/// How many sections *not* needed by the current window the cache keeps around
/// (plan 006 §3.3). Window need comes first — a viewport of header-only files can
/// legitimately pin more than this — and the budget only bounds what is retained
/// for re-crossing.
const SECTION_BUDGET: usize = 32;

/// The bounded per-file section cache: a hand-rolled LRU over a `Vec` (no new
/// dependency, and the working set is tens of entries, so a linear scan beats
/// allocating a key per lookup). Each entry is tagged with the [`LayoutKey`] and
/// the `stream_generation` it was built at; a stale tag is discarded when the
/// entry is next touched, never in an eager sweep.
#[derive(Default)]
struct SectionCache {
    entries: Vec<SectionEntry>,
    /// Monotonic use stamp — the LRU order.
    clock: u64,
}

struct SectionEntry {
    id: FileId,
    key: LayoutKey,
    generation: u64,
    used: u64,
    section: Rc<FileSection>,
}

impl SectionCache {
    /// The live section for `id`, marked most-recently-used. A tag mismatch — a
    /// resize, a mode/wrap/number/cross toggle, or any `stream_generation` bump —
    /// drops the entry here and reports a miss.
    fn get(&mut self, id: &FileId, key: LayoutKey, generation: u64) -> Option<Rc<FileSection>> {
        let pos = self.entries.iter().position(|entry| entry.id == *id)?;
        if self.entries[pos].key != key || self.entries[pos].generation != generation {
            self.entries.swap_remove(pos);
            return None;
        }
        self.clock += 1;
        self.entries[pos].used = self.clock;
        Some(Rc::clone(&self.entries[pos].section))
    }

    fn insert(&mut self, id: FileId, key: LayoutKey, generation: u64, section: Rc<FileSection>) {
        self.entries.retain(|entry| entry.id != id);
        self.clock += 1;
        self.entries.push(SectionEntry {
            id,
            key,
            generation,
            used: self.clock,
            section,
        });
    }

    /// The section cached for `id` whatever tag it carries — including a stale one
    /// the next [`SectionCache::get`] would discard, which is what makes this the
    /// *outgoing* section right after a `stream_generation` bump. Read-only: no
    /// LRU stamp and no discard, so inspecting the cache cannot perturb it.
    fn cached(&self, id: &FileId) -> Option<Rc<FileSection>> {
        self.entries
            .iter()
            .find(|entry| entry.id == *id)
            .map(|entry| Rc::clone(&entry.section))
    }

    /// Evict least-recently-used entries past [`SECTION_BUDGET`], never one the
    /// current window still needs (`pinned`).
    fn evict(&mut self, pinned: &[FileId]) {
        while self.entries.len() > SECTION_BUDGET {
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| !pinned.contains(&entry.id))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(index, _)| index);
            match victim {
                Some(index) => drop(self.entries.swap_remove(index)),
                // Everything left is pinned: window need outranks the budget.
                None => break,
            }
        }
    }

    /// Whether any cached section belongs to `path` (in either status section) —
    /// what ties a file's highlight sub-map to its section's lifetime.
    fn holds_path(&self, path: &str) -> bool {
        self.entries.iter().any(|entry| entry.id.path() == path)
    }
}

/// One file's contribution to the rendered window: which file it is, and the
/// half-open span of *that file's own* layout rows this segment draws.
pub struct WindowSegment {
    /// The file's stream identity. `None` only where there is no stream entry —
    /// History's `●` details row, or an empty file list.
    pub id: Option<FileId>,
    pub path: String,
    /// The prepared section a *strip* segment draws from, owned (`Rc`) so a window
    /// outlives every borrow it was assembled from. `None` for the anchor segment,
    /// whose rows are the live `diff_layout` — the anchor is never read from the
    /// section cache (plan 006 §3.4).
    pub section: Option<Rc<FileSection>>,
    pub row_range: Range<usize>,
}

impl WindowSegment {
    /// Whether this is the anchor (selected-file) segment.
    pub fn is_anchor(&self) -> bool {
        self.section.is_none()
    }

    /// How many physical rows the segment draws.
    pub fn rows(&self) -> usize {
        self.row_range.len()
    }
}

/// The viewport-sized slice of the stream: the anchor segment from the current
/// scroll offset, then as many following files as fit. Assembly is read-only — a
/// file the cache doesn't hold ends the window as a shortfall rather than
/// computing anything (`ensure_diff_window` fills the cache on the event path).
pub struct DiffWindow {
    pub segments: Vec<WindowSegment>,
}

impl DiffWindow {
    /// Total physical rows the window draws (below the viewport height when the
    /// stream — or the prepared part of it — ran out).
    pub fn rows(&self) -> usize {
        self.segments.iter().map(WindowSegment::rows).sum()
    }
}

/// One row of the per-frame window hit map (plan 006 §3.6): which stream file a
/// screen row belongs to and which of that file's own [`RowTarget`]s it draws,
/// plus whether the row is the anchor segment's or a strip segment's. Recorded
/// by the renderer straight from the same [`DiffWindow`] segment list it draws,
/// mirroring the `x_rects`/`diff_area` interior-mutability pattern.
///
/// A click resolves against this instead of `diff_row_at`'s anchor-layout-only
/// arithmetic, which is what makes a strip row — inert since C3 — clickable: an
/// `is_anchor` hit still delegates to the unchanged `hit_target`/`diff_row_at`
/// path (only strip rows need the new handling), and a row in the shortfall
/// region (below the last row the window drew) has no entry at all.
#[derive(Clone)]
pub(crate) struct WindowHit {
    /// The row's stream identity — `None` only where the window has no stream
    /// entry at all (History's `●` details row, or an empty file list); always an
    /// anchor row.
    pub(crate) id: Option<FileId>,
    /// The target within `id`'s own layout this row draws.
    pub(crate) target: RowTarget,
    pub(crate) is_anchor: bool,
    /// The row's side-by-side column, or `None` for a full-width row — the same
    /// `LayoutRow.side` the anchor path reads through `in_side_column`, carried
    /// here so a click in a strip box's blank sibling column resolves like the
    /// anchor equivalent rather than as a click on the box (plan 007 §3.3c).
    pub(crate) side: Option<Side>,
}

/// The state signature a [`WindowHit`] map was recorded against. The event
/// loop drains a whole batch (wheel, resize, toggles, a refresh, a click)
/// before the next redraw, so a click can be processed several state changes
/// after the frame that recorded the map — a flip, a scroll, a relayout, a
/// relist, or a view change all shift which screen row means what. Unlike
/// `x_rects` (a stale comment rect is caught because the click still resolves
/// against the *current* layout before acting), a window-hit lookup drives
/// `strip_click` directly, so staleness has to be caught up front: any field
/// mismatch between record time and lookup time means "something happened
/// in between," and the map is treated as absent rather than trusted.
#[derive(Clone, Copy, PartialEq, Eq)]
struct WindowEpoch {
    layout_generation: u64,
    stream_generation: u64,
    view: ViewMode,
    /// The anchor-domain scroll offset — not just the flip-only
    /// `layout_generation` — so a plain same-file scroll (no flip, no
    /// relayout) is caught too.
    offset: usize,
    diff_area: Rect,
}

/// The current frame's window hit map, plus the [`WindowEpoch`] it was built
/// against (plan 006 §3.6). `Default` is the pre-first-render state: no
/// epoch, so the very first lookup (before anything has ever rendered) misses
/// cleanly rather than matching a placeholder.
#[derive(Default)]
struct WindowHitMap {
    epoch: Option<WindowEpoch>,
    rows: Vec<WindowHit>,
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
    /// The file's header (plan 006 §3.1) — one cursor stop, anchoring nothing:
    /// `c`, double-click-to-edit, and `x` are all no-ops on it. One physical row
    /// for the stream's first file, two (rule then band) for every file below it
    /// (plan 008 §3.5).
    FileHeader,
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

/// A diff pane's cursor + editor state, owned once per view (`status_pane`,
/// `review.pane`, `history_pane`). The cursor names a [`CursorAddress`] — a file
/// plus one of its logical [`RowTarget`]s, never a physical row: `None` is the
/// reset state (the anchor's first target), resolved once the layout exists — a
/// file change or mode toggle resets before the new layout is built, so the
/// concrete target isn't yet known. Scroll/metrics/row caches stay App-global,
/// shared by whichever view is showing.
///
/// The field is written **only** by `App::write_cursor` and the setters above it
/// (plan 007 §3.3a); nothing else assigns it.
#[derive(Debug, Default)]
struct DiffPaneState {
    cursor: Option<CursorAddress>,
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
    /// The section `diff_key`'s cached *layout* was built for. The diff itself is
    /// section-independent (see above), but the file-header row's marker and tone
    /// are not (plan 006 §3.1), so a same-path staged↔unstaged move keeps the diff
    /// and drops the layout.
    diff_section: Option<Section>,
    /// Set when an external refresh should recompute the open file's diff even
    /// though its `(section, path)` is unchanged (its content may have changed).
    /// Unlike navigating to a new file, this preserves the scroll position.
    diff_dirty: bool,
    pub diff_mode: DiffMode,
    /// Whether the diff pane shows line-number gutters (unified's 10-char
    /// number gutter, SBS's per-column 5-char gutter). The sign column in
    /// unified mode is unaffected. Toggled with `n`; from `Config.line_numbers`.
    pub show_line_numbers: bool,
    /// Whether the top menu bar (the `View`/`Theme` labels in the header) is
    /// shown. On by default; from `Config.menu_bar`, toggled with `m`.
    pub show_menu_bar: bool,
    /// Whether the diff pane hard-wraps long lines at the pane width. Off by
    /// default; from `Config.wrap_lines`, toggled with `w`. A wrap input to the
    /// physical layout, so a change rebuilds it (see [`App::diff_layout`]).
    pub wrap_lines: bool,
    /// Whether scrolling past a diff's edge crosses into the next / previous
    /// file's diff, in all three views (History's stream is the selected commit's
    /// files). Off by default; from `Config.cross_file_scroll`, toggled with `f`
    /// (plan §3.4).
    pub cross_file_scroll: bool,
    /// Horizontal scroll offset for code content, in display columns (plan §3.5).
    /// Applies only when wrap is off; shifts unified content and both side-by-side
    /// cells by the same amount, never the gutters / sign / hunk headers / comment
    /// boxes / editor. Clamped at read time to the longest code line, so a divider
    /// drag or `n` toggle needs no reset. Reset on file change, mode toggle, and
    /// wrap enable; preserved on a same-file refresh. Session-only: no key, no
    /// config, no menu, no persistence.
    pub diff_hscroll: usize,
    /// Bumped whenever the active diff *object* is (re)computed, so the lazily
    /// cached longest-code-line width invalidates exactly then (plan §3.5).
    diff_generation: Cell<u64>,
    /// Memoized longest sanitized *code*-line display width for the active diff
    /// (hunk headers excluded), keyed by `(diff_generation, view)` so it survives
    /// resizes / mode toggles but not a diff or view change. Computed lazily, only
    /// while horizontally scrolled; the h-scroll clamp reads it (`max_hscroll`).
    max_line_width: Cell<Option<((u64, ViewMode), usize)>>,
    /// Count of times [`App::active_max_line_width`] actually recomputed the
    /// memo (a cache miss), as opposed to returning the cached value. A
    /// test-only observable proving the per-diff longest-line scan runs once
    /// per `(diff_generation, view)`, not once per horizontal scroll or render
    /// (plan §3.7). Not otherwise read.
    max_line_width_compute_count: Cell<u64>,
    /// Count of per-file diff computations, bumped in `sync_diff` /
    /// `sync_review_diff`'s actual compute branches. A test-only observable
    /// proving a cross-file crossing computes exactly the destination file's diff
    /// (laziness, plan §3.4). Not otherwise read.
    diff_compute_count: Cell<u64>,
    /// The open dropdown, or `None` when no menu is open. Opening is mouse-first
    /// (click a title); keyboard nav drives an already-open menu. Mutually
    /// exclusive with editing (see [`OpenMenu`]).
    pub open_menu: Option<OpenMenu>,
    /// Each top-level title's clickable rect, recorded from the header layout
    /// every render (interior-mutable, mirroring `x_rects`). Cleared to empty
    /// when the bar is hidden, so a stale rect can't match after `m`.
    menu_title_rects: RefCell<Vec<(MenuId, Rect)>>,
    /// The open dropdown's hit-map (bounds + per-visible-row rects), recorded
    /// every render and cleared to `None` when no menu is open.
    menu_dropdown: RefCell<Option<DropdownHit>>,
    /// The diff pane's scroll offset, in physical layout rows (a `usize` since a
    /// few long comment boxes can push a diff past `u16::MAX` rows).
    /// Interior-mutable: a structural relayout (resize, wrap/line-number toggle)
    /// re-anchors it from the render path's `&self` to keep the top visible
    /// logical line put (plan §3.3, `diff_layout`).
    pub diff_scroll: Cell<usize>,
    /// Inner height (terminal rows, `u16`) and total physical content rows
    /// (`usize`) of the diff pane from the last render, so scrolling can clamp to
    /// the content in either mode. Interior-mutable because rendering takes `&App`.
    diff_viewport: Cell<u16>,
    diff_content_rows: Cell<usize>,
    /// Per-file sub-maps of syntax-highlighted lines, keyed by file *path* then by
    /// the line's sanitized text, so a strip row highlights with its own file's
    /// syntax instead of colliding with another file's identical text (plan 006
    /// §3.3). Path — not the full [`FileId`] — because a highlight depends only on
    /// (syntax, text), both path-derived: a path listed in both status sections
    /// shares one warm sub-map rather than keeping two. A sub-map lives as long as
    /// its file is the active one or holds a cached section
    /// (`prune_highlight_cache`).
    highlight_cache: RefCell<HashMap<String, HashMap<String, HighlightedLine>>>,
    /// Prepared sections for the files *around* the anchor — the stream's working
    /// set (plan 006 §3.3). Interior-mutable because window assembly runs on the
    /// render path's `&self`; it only ever reads what the event path prepared.
    sections: RefCell<SectionCache>,
    /// Bumped by anything that can change which files the stream holds, or what
    /// any file's diff or rows contain: a status snapshot replacement, a review
    /// relist, a commit's file-list installation in History, a comment mutation, a
    /// view change. Cached sections carry the value they were built at and are
    /// dropped on access once it moves. Layout-key changes need no bump — they
    /// invalidate by tag mismatch (plan 006 §3.3).
    stream_generation: Cell<u64>,
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
    /// The current frame's window hit map (plan 006 §3.6): one [`WindowHit`] per
    /// drawn row, top to bottom, recorded by the renderer alongside `x_rects`,
    /// tagged with the [`WindowEpoch`] it was recorded under so a click drained
    /// after a later state change (in the same input batch, no redraw between)
    /// can detect the staleness instead of acting on it. Empty whenever the
    /// window has nothing drawn (the early-return no-diff frame, and History's
    /// details pane) or, off cross-file scroll, holds only anchor rows — so a
    /// click always falls through to the unchanged path there.
    window_hits: RefCell<WindowHitMap>,

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
    /// Whether the last `commit_files` listing *failed* (as opposed to returning
    /// an honestly empty commit). A refresh keeps an immutable commit's list
    /// rather than re-listing it (plan 009 §3.4), which would otherwise strand a
    /// transient listing error: the empty list it left behind would survive every
    /// watcher tick until the user reselected the commit by hand.
    commit_files_failed: bool,
    /// Row in the top "Committed Changes" list: 0 is the commit (`●`) row,
    /// `1..=commit_files.len()` index into `commit_files`.
    committed_row: usize,
    /// History's own diff-pane cursor, the third `DiffPaneState` beside
    /// `status_pane` and `review.pane` (plan 009 §3.1). Its `editing` slot exists
    /// but can never open: History authors no comments, so nothing reaches
    /// `set_editor`.
    history_pane: DiffPaneState,
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
            diff_section: None,
            diff_dirty: false,
            diff_mode: config.diff_mode(),
            show_line_numbers: config.line_numbers(),
            show_menu_bar: config.menu_bar(),
            wrap_lines: config.wrap_lines(),
            cross_file_scroll: config.cross_file_scroll(),
            diff_hscroll: 0,
            diff_generation: Cell::new(0),
            max_line_width: Cell::new(None),
            max_line_width_compute_count: Cell::new(0),
            diff_compute_count: Cell::new(0),
            open_menu: None,
            menu_title_rects: RefCell::new(Vec::new()),
            menu_dropdown: RefCell::new(None),
            diff_scroll: Cell::new(0),
            diff_viewport: Cell::new(0),
            diff_content_rows: Cell::new(0),
            highlight_cache: RefCell::new(HashMap::new()),
            sections: RefCell::new(SectionCache::default()),
            stream_generation: Cell::new(0),
            layout: RefCell::new(None),
            layout_generation: Cell::new(0),
            last_click: None,
            window_hits: RefCell::new(WindowHitMap::default()),
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
            commit_files_failed: false,
            committed_row: 0,
            history_pane: DiffPaneState::default(),
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
        app.reanchor_review_comments(CommentInvalidation::Stream);
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
        self.file_at_index(self.selected)
    }

    /// The `(section, entry)` at flattened index `index` — staged entries first,
    /// then unstaged, the order the Changes panel lists and the stream crosses.
    pub fn file_at_index(&self, index: usize) -> Option<(Section, &FileEntry)> {
        let staged = &self.status.staged;
        if index < staged.len() {
            Some((Section::Staged, &staged[index]))
        } else {
            self.status
                .unstaged
                .get(index - staged.len())
                .map(|entry| (Section::Unstaged, entry))
        }
    }

    /// Re-read status from disk, keeping the cursor on the same file (matched by
    /// section and path) when it survives, and forcing the open diff to
    /// recompute — its content may have changed in place even if its path did not.
    pub fn refresh(&mut self) {
        // A re-read can renumber, re-section, or drop the file a divergent cursor
        // names; it snaps back to the anchor rather than chasing it (plan 007
        // §3.3b — the deliberate "watcher tick mid-walk" trade).
        self.clear_divergent_cursor();
        let previous = self.selected_section_path();
        match self.repo.status() {
            Ok(status) => {
                self.status = status;
                // A staging mutation reaches here *without* a `reload()`, and it
                // rewrites the files' diffs in place — so every cached section is
                // stale from this point (plan 006 §3.3).
                self.bump_stream_generation();
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
            Err(err) => {
                tracing::warn!("status refresh failed: {err:#}");
                // A failed snapshot is not a reason to trust the cached sections:
                // whatever made `git status` fail may already have rewritten the
                // files they were built from. Retire them (plan 007 §3.1).
                self.bump_stream_generation();
            }
        }
    }

    /// Re-read the active view's data and recompute its diff in one step. Used by
    /// the file watcher, whose path has no trailing `sync_active` like
    /// `on_key`/`on_mouse`. View-aware: it refreshes whichever view is showing.
    pub fn reload(&mut self) {
        // A watcher-driven reload can shrink the menu's row list (a theme file
        // vanished); drop any open dropdown rather than risk a stale `item`.
        self.open_menu = None;
        // No bump here: `refresh_active` reaches whichever view's refresh owns the
        // single invalidation for this cycle (plan 007 §3.1).
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
        } else if self.open_menu.is_some() {
            // An open dropdown captures keys before the keymap: arrows/tab
            // navigate, Enter/Space activate, every other key closes (plan §3.0).
            self.on_key_menu(key);
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
            Action::ToggleWrap => {
                self.toggle_wrap();
                self.persist_setting(Setting::WrapLines(self.wrap_lines));
                return;
            }
            Action::ToggleCrossFileScroll => {
                self.set_cross_file_scroll(!self.cross_file_scroll);
                self.persist_setting(Setting::CrossFileScroll(self.cross_file_scroll));
                return;
            }
            Action::CycleTheme => {
                self.cycle_theme();
                self.persist_setting(Setting::Theme(self.theme_name.clone()));
                return;
            }
            Action::ToggleMenuBar => {
                self.show_menu_bar = !self.show_menu_bar;
                if !self.show_menu_bar {
                    // Hiding the bar drops any open dropdown and clears the title
                    // rects synchronously, so a click queued behind this key (the
                    // event loop drains input before redraw) can't hit a stale rect.
                    self.open_menu = None;
                    self.menu_title_rects.borrow_mut().clear();
                }
                self.persist_setting(Setting::MenuBar(self.show_menu_bar));
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

    /// The view-aware behavior behind `b`/`Action::ToggleChanges` and the View
    /// menu's "Changes panel" row: which panel toggles depends on the active
    /// view. Shared so the key and the menu command can never diverge.
    fn toggle_changes_panel(&mut self) {
        match self.view {
            ViewMode::Status => self.toggle_changes(),
            ViewMode::History => self.toggle_history_panel(),
            ViewMode::Review => self.toggle_review_panel(),
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
            Action::ToggleChanges => self.toggle_changes_panel(),
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
                Focus::Staging => self.list_scroll_half_page(true),
            },
            Action::HalfPageUp => match self.focus {
                Focus::Diff => self.review_move_cursor(false, self.half_page() as usize),
                Focus::Staging => self.list_scroll_half_page(false),
            },
            Action::ToggleStage => self.toggle_stage(),
            Action::Stage => self.stage_selected(),
            Action::Unstage => self.unstage_selected(),
            // `x` discards the file under the cursor — but stays inert (neither
            // discarding nor deleting) when the *diff pane is focused* and its
            // cursor rests on a comment/orphan row, so it can never be mistaken for
            // the deletion key (`X`/`Action::DeleteComment`, below). The gate reads
            // the cursor's whole *address* (plan 007 §5-B3): a comment row in a
            // file the cursor walked into is as inert as one in the anchor. With
            // the file list focused, `x` discards the list-selected file regardless
            // of where the hidden diff cursor sits.
            Action::Discard => {
                if !self.diff_focused() || self.cursor_address_comment_id().is_none() {
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
            | Action::ToggleWrap
            | Action::ToggleCrossFileScroll
            | Action::CycleTheme
            | Action::ToggleMenuBar
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
            Action::ToggleChanges => self.toggle_changes_panel(),
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
            Action::HalfPageDown => self.history_half_page(true),
            Action::HalfPageUp => self.history_half_page(false),
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
            | Action::ToggleWrap
            | Action::ToggleCrossFileScroll
            | Action::CycleTheme
            | Action::ToggleMenuBar
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
            Action::ToggleChanges => self.toggle_changes_panel(),
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
            | Action::ToggleWrap
            | Action::ToggleCrossFileScroll
            | Action::CycleTheme
            | Action::ToggleMenuBar
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
            self.clear_divergent_cursor();
            self.prepare_post_toggle_window();
        } else {
            self.reveal_review_panel();
        }
    }

    fn reveal_review_panel(&mut self) {
        self.show_changes = true;
        self.set_review_focus(ReviewFocus::List);
        self.clear_divergent_cursor();
        self.prepare_post_toggle_window();
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
    /// pane), then reveal it. Shared by every view's diff pane — status, review
    /// and, on a file row, history.
    fn cursor_to_edge(&mut self, bottom: bool) {
        let count = self.review_row_count();
        let idx = if bottom { count.saturating_sub(1) } else { 0 };
        let target = self.review_target_at(idx);
        self.set_cursor_on_anchor(target);
        self.review_reveal_cursor();
    }

    /// Select review file `idx` and reset the diff cursor to its first row. A
    /// no-op selection (nav that lands on the same file, e.g. `k` at the top)
    /// leaves the cursor and scroll untouched — resetting only the cursor would
    /// strand it above the preserved viewport. The one caller that must *not*
    /// reset on a real change (comment navigation, which places the cursor on a
    /// specific row) moves `selected` itself and then places its own cursor.
    fn select_review_file(&mut self, idx: usize) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        if review.selected == idx {
            return;
        }
        review.selected = idx;
        // `None` is the top-of-layout reset; the new file's layout doesn't exist
        // yet (it's built by the trailing `sync_active`).
        self.set_cursor_on_anchor(None);
    }

    /// Move the diff cursor by `step` physical rows, then scroll the viewport so
    /// it stays visible ("act-and-reveal", plan §3.4).
    ///
    /// With a stream below the pane the movement is the **walk** (plan 007
    /// §3.3f); without one it is today's single-file clamp.
    fn review_move_cursor(&mut self, down: bool, step: usize) {
        if self.strip_anchor().is_some() {
            self.walk_cursor(down, step);
        } else {
            self.move_cursor_in_anchor(down, step);
        }
    }

    /// The pre-stream cursor move: `step` physical rows clamped to the anchor's
    /// own row list. The path taken with cross-file scroll off, while the in-place
    /// editor is open (the strip is collapsed for its duration), and in any view
    /// without a stream.
    ///
    /// A multi-row comment box shares one target across N physical rows, so a
    /// downward step never stalls inside the current box: it starts past the box's
    /// own last row, which makes `j`/`k` cross a whole box in one step (plan §3.0).
    fn move_cursor_in_anchor(&mut self, down: bool, step: usize) {
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
        self.set_cursor_on_anchor(target);
        self.review_reveal_cursor();
    }

    /// The keyboard walk (plan 007 §3.3f).
    ///
    /// Movement is defined on the **flattened target stream** — every stream
    /// file's physical rows concatenated in list order — and starts from wherever
    /// the *cursor* is, which may be a file below the anchor. A step that runs off
    /// the end of the cursor's file keeps its residual and spends it in the next
    /// one, so `j` walks a stop at a time through however many short files it
    /// meets while the anchor stays put, and Ctrl-d moves a full half page across
    /// boundaries instead of pausing at each. The residual is discarded only at
    /// the two ends of the stream, where there is nothing left to spend it on.
    ///
    /// The anchor follows only when the reveal below renormalizes past the
    /// boundary (`o > R_anchor`, 006's strict hysteresis — the same threshold the
    /// wheel flips at), except upward, where a destination *above* the anchor
    /// renormalizes on the spot: previous files cannot render below the anchor, so
    /// there is no divergent state to walk through (§3.3f's pinned asymmetry).
    fn walk_cursor(&mut self, down: bool, step: usize) {
        let width = self.diff_pane_width();
        let Some(address) = self.cursor_address() else {
            return; // no file, or a layout with no rows at all
        };
        let Some(index) = self.stream_index_of_exact(&address.file) else {
            return;
        };
        let Some(span) = self.address_file_span(&address) else {
            return;
        };
        // The tall-target rule (plan 006 §3.5), unchanged and checked before any
        // crossing: when the step cannot advance within the cursor's own file but
        // the viewport still has room to move, scroll inside the target rather
        // than stepping off content the user hasn't seen yet.
        //
        // Converged cursors only, because `at_hard_edge` is anchor-domain: a
        // divergent cursor implies the anchor is pinned at its *lower* edge
        // (§3.3b's first corollary) but says nothing about the upper one, and its
        // own file's rows are not the anchor's to measure anyway. That leaves the
        // rule firing exactly where it did before the walk existed.
        let stuck = self.address_is_anchor(&address)
            && if down {
                span.end >= self.stream_rows(index, width)
            } else {
                span.start == 0
            };
        if stuck && !self.at_hard_edge(down) {
            self.scroll_diff(down, step.min(u16::MAX as usize) as u16);
            return;
        }
        let Some((file, row)) = self.walk_step(index, &span, down, step, width) else {
            return;
        };
        let Some(destination) = self.stream_address_at(file, row, width) else {
            return;
        };
        let target = destination.target;
        if self.address_is_anchor(&destination) {
            self.set_cursor_on_anchor(Some(target));
            self.review_reveal_cursor();
            return;
        }
        // Reveal *before* pinning: walking into a file is what brings it into the
        // window, and `place_cursor` deliberately refuses an address the window
        // doesn't hold yet. The reveal may hand the anchor over to the very file
        // being walked into (always, going up), which converges the address.
        self.reveal_address(&destination);
        if self.address_is_anchor(&destination) {
            self.set_cursor_on_anchor(Some(target));
        } else {
            // A rejected placement — the window still doesn't hold the file
            // because a section vanished under the walk — leaves the cursor where
            // it was rather than pinning a dangling address.
            self.place_cursor(destination);
        }
    }

    /// Where `step` physical rows from `span` in stream file `index` lands, as
    /// `(file, row)` in the flattened stream. Files crossed on the way are
    /// prepared here — the walk's laziness trigger (plan 007 §3.4).
    fn walk_step(
        &mut self,
        index: usize,
        span: &Range<usize>,
        down: bool,
        step: usize,
        width: u16,
    ) -> Option<(usize, usize)> {
        let mut file = index;
        if down {
            // `.max(span.end)` skips the rest of a multi-row target, so one press
            // crosses a whole comment box.
            let mut row = (span.start + step).max(span.end);
            loop {
                let rows = self.stream_rows(file, width);
                if row < rows {
                    return Some((file, row));
                }
                if file + 1 >= self.stream_len() {
                    return Some((file, rows.checked_sub(1)?)); // last file: clamp
                }
                row -= rows;
                file += 1;
            }
        } else {
            let mut row = span.start as i64 - step as i64;
            while row < 0 {
                if file == 0 {
                    return Some((0, 0)); // first file: clamp
                }
                file -= 1;
                row += self.stream_rows(file, width) as i64;
            }
            Some((file, row as usize))
        }
    }

    /// The address physical `row` of stream file `index` names: that file's
    /// identity paired with the [`RowTarget`] its rows carry there — read from the
    /// live layout when it is the anchor (the one file whose rows can carry the
    /// in-place editor), from its prepared section otherwise.
    fn stream_address_at(&mut self, index: usize, row: usize, width: u16) -> Option<CursorAddress> {
        let file = self.stream_file_id(index)?;
        let target = if Some(index) == self.stream_position() {
            self.review_target_at(row)?
        } else {
            self.prepare_section(index, width)?.rows.get(row)?.target
        };
        Some(CursorAddress { file, target })
    }

    /// Half-page in review: the diff pane moves the cursor (act-and-reveal); the
    /// file list scrolls the diff viewport only, without touching the cursor
    /// (restores the pre-cursor behaviour when the list is focused, plan §3.3).
    fn review_half_page(&mut self, down: bool) {
        match self.review_focus() {
            ReviewFocus::Diff => self.review_move_cursor(down, self.half_page() as usize),
            ReviewFocus::List => self.list_scroll_half_page(down),
        }
    }

    /// A half-page scroll of the diff viewport while a *list* pane is focused
    /// (Status staging pane / Review list / History's Graph and committed
    /// changes). A list has no diff cursor to move, so with cross-file scroll on
    /// this is a plain wheel-sized tick in the stream domain (plan 006 §3.5):
    /// continuous through a boundary, no clamp-then-cross step, no cursor. With
    /// cross-file off, with no anchor, or while editing, it is the plain clamping
    /// scroll it always was.
    fn list_scroll_half_page(&mut self, down: bool) {
        let step = self.half_page();
        if self.strip_anchor().is_some() {
            self.wheel_scroll_window(if down {
                i64::from(step)
            } else {
                -i64::from(step)
            });
        } else {
            self.scroll_diff(down, step);
        }
    }

    /// Scroll the diff viewport so the cursor target is visible: no-op when it
    /// already is, otherwise snap the top edge to bring it into view. The cursor
    /// target may span several physical rows (a comment box), so this reveals the
    /// whole `[start, end)` span — but a box taller than the viewport can't be
    /// fully shown, so it top-aligns to the box's first row rather than looping
    /// (plan §3.4).
    fn review_reveal_cursor(&mut self) {
        if let Some(address) = self.divergent_address() {
            // A divergent cursor has no row in the anchor's layout at all, so the
            // anchor-domain arithmetic below would reveal the wrong rows (or
            // none): §3.3h's window-aware reveal owns this case.
            self.reveal_address(&address);
            return;
        }
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
        // Read in the extended domain (plan 006 §3.2e): clamping to the anchor's
        // own max would read a wheel-extended view as higher than it is and yank
        // it. The write below stays anchor-domain (never above `max_top`).
        let top = self.diff_scroll.get().min(self.diff_scroll_limit());
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
        self.diff_scroll.set(new_top.min(max_top));
    }

    /// Scroll so `address` is visible, measured over the whole **stream** rather
    /// than one file's rows (plan 007 §3.3h). Same rule as the anchor-domain
    /// reveal above — already visible is a no-op, above the viewport top-aligns,
    /// below pulls the last row in, a span taller than the viewport top-aligns —
    /// but the top it computes is an extended-domain offset, settled through the
    /// stream renormalizer. A top past the anchor's last row (`o > R_anchor`,
    /// 006's strict hysteresis and the same threshold the wheel flips at) hands
    /// the anchor over instead of clamping the address off screen; a negative one
    /// hands it back to a previous file.
    ///
    /// The flip that settling may trigger is cursor-PRESERVING: an address being
    /// revealed is an address the user is still pointing at. The wheel's resetting
    /// flip is 006's contract and stays exactly as it was.
    fn reveal_address(&mut self, address: &CursorAddress) {
        let Some(anchor) = self.strip_anchor() else {
            return;
        };
        let viewport = self.diff_viewport.get() as i64;
        if viewport == 0 {
            return;
        }
        let Some(span) = self.address_stream_span(address) else {
            return;
        };
        let width = self.diff_pane_width();
        let anchor_rows = self.diff_layout(width).len();
        let top = self.paint_offset(anchor_rows, viewport as usize) as i64;
        let new_top = if span.start < top || span.end - span.start >= viewport {
            span.start
        } else if span.end > top + viewport {
            span.end - viewport
        } else {
            top
        };
        self.settle_stream_offset(anchor, new_top, FlipCursor::Keep);
    }

    /// Whether the diff cursor target's first row lies within the visible
    /// viewport, tested against the same clamped offset the renderer paints with
    /// (so a wheel scroll that pushed it offscreen reads as not-visible).
    ///
    /// A *divergent* cursor has no row in the anchor's layout at all, so the
    /// anchor-domain test below would read it as row 0 and call an off-screen
    /// address visible; it is measured in window rows instead — the screen rows
    /// its file actually draws (plan 007 §3.3g). Same contract in both branches:
    /// the target's **first** row decides, so a comment box clipped by the
    /// viewport bottom is visible, not hidden.
    fn review_cursor_visible(&self) -> bool {
        if self.active_pane().is_none() {
            return false;
        }
        let viewport = self.diff_viewport.get() as usize;
        if viewport == 0 {
            return false;
        }
        if let Some(address) = self.divergent_address() {
            return self
                .address_window_placement(&address)
                .is_some_and(|(start, _)| start < viewport);
        }
        let cursor = self.review_cursor();
        let top = self.diff_scroll.get().min(self.diff_scroll_limit());
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

    /// Flip-then-act convergence (plan 007 §3.3g): hand the anchor — and with it
    /// the selection, the title, and every `selected_file()` read downstream — to
    /// the file the *cursor* names, so a diff-focused action acts on the row the
    /// user is pointing at rather than on whichever file the list happens to have
    /// selected. Returns whether the caller may go on to act.
    ///
    /// A converged cursor, and any action taken while the file list has focus, is
    /// today's path untouched: nothing moves and this reports `true` at once.
    ///
    /// Callers check their own eligibility *first* and skip this when the action
    /// would only flash or no-op on the resolved row (step 3) — an ineligible
    /// target must never reorient the view. The rest of the sequence lives here:
    /// validating the address (1), the two-press reveal gate (2), the
    /// cursor-preserving flip (4), and the revalidation the caller acts behind (5).
    fn converge_on_cursor(&mut self) -> bool {
        if !self.diff_focused() {
            return true;
        }
        let Some(address) = self.divergent_address() else {
            return true;
        };
        if !self.cursor_address_valid(&address) {
            // The window stopped holding the file between the last sweep and this
            // key. Drop the address rather than fall back to the anchor: acting on
            // a file other than the one under the cursor is the whole hazard.
            self.write_cursor(None);
            return false;
        }
        if !self.reveal_cursor_before_acting() {
            return false;
        }
        if !self.flip_to_address(&address) {
            return false;
        }
        debug_assert!(
            !self.cursor_divergent(),
            "convergence left the cursor pointing away from the anchor"
        );
        true
    }

    /// Step (4) of the convergence: make `address`'s file the anchor with the
    /// cursor still on it.
    ///
    /// The arriving file is installed at offset 0, which is the *minimal*
    /// reorientation available rather than an arbitrary one: `diff_scroll` counts
    /// from the new anchor's own first row, a divergent file always begins below
    /// the pane top, so every legal offset scrolls the view down and 0 scrolls it
    /// least. The trailing reveal is then the ordinary anchor-domain one — the
    /// address converged the moment the flip landed — and re-preparing the window
    /// is what settling would have done had this gone through it (it cannot: the
    /// end-of-stream clamp would hand a short last file straight back).
    fn flip_to_address(&mut self, address: &CursorAddress) -> bool {
        let Some(index) = self.stream_index_of_exact(&address.file) else {
            return false;
        };
        if !self.flip_anchor(index, 0, FlipCursor::Keep) {
            return false;
        }
        self.review_reveal_cursor();
        self.ensure_diff_window(self.diff_pane_width(), self.diff_viewport.get());
        true
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
        target_span(&self.diff_layout(self.diff_pane_width()), target)
    }

    /// The target the cursor rests on **in the anchor's layout**: the pinned
    /// address's target when it is the anchor's, or (when unset after a reset)
    /// the target at the top of the current layout.
    ///
    /// A *divergent* address has no anchor-domain answer at all — reinterpreting
    /// its target against the anchor's rows is exactly the confusion
    /// [`CursorAddress`] exists to prevent — so this reports `None` and the
    /// address-aware accessors below serve those callers (plan 007 §3.3j).
    fn review_cursor_target(&self) -> Option<RowTarget> {
        match self.active_pane()?.cursor.as_ref() {
            Some(address) => self.address_is_anchor(address).then_some(address.target),
            None => self.review_target_at(0),
        }
    }

    // --- The cursor write seam (plan 007 §3.3a) ---
    //
    // One setter family owns every mutation of `DiffPaneState.cursor`. Nothing
    // else assigns the field, so an address can never be half-written (a target
    // moved without the file it belongs to), and the divergence sweep below has
    // exactly one place to hook.

    /// The one place the cursor field is assigned. A no-op when there is no pane
    /// to write to (review without a session).
    fn write_cursor(&mut self, address: Option<CursorAddress>) {
        if let Some(pane) = self.active_pane_mut() {
            pane.cursor = address;
        }
    }

    /// Pin the diff cursor to `target` **in the anchor's own file** — the
    /// converged write every pre-007 seam does (`None` resets it to the top of
    /// the anchor's layout). Divergence, if any, ends here: the address is
    /// rebuilt around the current anchor rather than carried over.
    fn set_cursor_on_anchor(&mut self, target: Option<RowTarget>) {
        // `active_file_id` clones the path, so only ask for it once there *is* a
        // target to pair with: the bare `None` reset is the common call.
        let address = target.and_then(|target| {
            self.active_file_id()
                .map(|file| CursorAddress { file, target })
        });
        self.write_cursor(address);
    }

    /// Place the cursor at `address`, which may name a file other than the
    /// anchor (plan 007 §3.3a). Returns whether it was placed: the address has to
    /// satisfy the divergence invariant *now* — the diff pane focused, its file in
    /// the prepared window, its target resolving in that file's rows — so no
    /// caller can install a dangling highlight, a target that means nothing, or a
    /// divergence the next sweep would immediately undo. The keyboard walk and
    /// strip clicks are the production callers (both act on the focused diff);
    /// B1 drives it from tests.
    pub fn place_cursor(&mut self, address: CursorAddress) -> bool {
        if !self.diff_focused()
            || self.active_pane().is_none()
            || !self.cursor_address_valid(&address)
        {
            return false;
        }
        self.write_cursor(Some(address));
        true
    }

    /// Re-point a cursor that was converged on the stream row the selection just
    /// *left* at the row it landed on, keeping its target — the same-path
    /// staged↔unstaged move, where one file occupies two stream rows drawing the
    /// same net HEAD→worktree diff (see [`FileId`]). Without this the address
    /// would still name the row we left, read as divergent, and be swept to the
    /// top: a regression against the bare-`RowTarget` cursor, which simply
    /// survived the move (plan 007 §3.3a).
    ///
    /// Same path only, and only from `previous`: a genuinely divergent cursor on
    /// the *other* section's row is left alone (selecting that row converges it
    /// on its own), and a move to a different path resets through `sync_diff`'s
    /// file-changed branch as it always did.
    fn rebind_cursor_across_sections(&mut self, previous: Option<Section>) {
        let Some(anchor) = self.active_file_id() else {
            return;
        };
        let Some(address) = self.pinned_address() else {
            return;
        };
        let FileId::Status { section, path } = &address.file else {
            return;
        };
        if Some(*section) != previous || path != anchor.path() {
            return;
        }
        let target = address.target;
        self.write_cursor(Some(CursorAddress {
            file: anchor,
            target,
        }));
    }

    /// Drop a *divergent* cursor back to `None` — both fields, never a foreign
    /// target reinterpreted against the anchor (plan 007 §3.3b). An anchor cursor
    /// is untouched, which is what keeps every trigger below behaviour-identical
    /// for the converged case: refresh / reload / relist, resize, the `w`/`n`/`d`/
    /// `f` layout toggles, a view change, a list click.
    fn clear_divergent_cursor(&mut self) {
        if self.cursor_divergent() {
            self.write_cursor(None);
        }
    }

    /// Enforce the divergence invariant wherever the window is (re)prepared: a
    /// cursor may name a file other than the anchor only while the diff pane is
    /// focused and that file is still in the prepared window with its target
    /// resolving. Anything else drops it to `None`.
    ///
    /// This is also how a `stream_generation` bump re-validates the address
    /// (§3.3b's second corollary), structurally rather than by enumeration: the
    /// bump retires every cached section, so the address survives only if the
    /// same event's `ensure_diff_window` re-prepared its file *and* the target
    /// still resolves in the rebuilt rows.
    ///
    /// Resolving is not enough on its own, though: the file may have changed on
    /// disk since (an agent's edit, not yet seen by a refresh), and a rebuilt
    /// `Code(i)` denotes a *different line* while indexing just as happily. So the
    /// rebuilt diff is compared against `outgoing` — what the address last
    /// resolved against — and any difference drops it. That mirrors the anchor
    /// cursor, which survives a same-file refresh only because
    /// `recompute_status_diff` early-returns on an identical diff.
    fn normalize_cursor(&mut self, outgoing: Option<Rc<FileSection>>) {
        let Some(address) = self.divergent_address() else {
            return;
        };
        if !self.diff_focused() || !self.cursor_address_valid(&address) {
            self.write_cursor(None);
            return;
        }
        let Some(outgoing) = outgoing else {
            return;
        };
        let changed = self
            .address_section(&address.file)
            .is_none_or(|section| section.diff != outgoing.diff);
        if changed {
            self.write_cursor(None);
        }
    }

    // --- Address-aware resolution (plan 007 §3.3j) ---
    //
    // Separate accessors, never an overload of the anchor-domain ones above:
    // `review_index_of` / `review_target_at` / `comment_row_index` keep meaning
    // exactly what their anchor-context callers (`place_diff_cursor`,
    // `hit_target`, click routing) need.

    /// The pinned address, if the cursor is pinned at all.
    fn pinned_address(&self) -> Option<&CursorAddress> {
        self.active_pane()?.cursor.as_ref()
    }

    /// The pinned target when it is the anchor's — the read the pre-007 code
    /// spelled `pane.cursor` directly. `None` while unset *or* divergent.
    fn pinned_anchor_target(&self) -> Option<RowTarget> {
        let address = self.pinned_address()?;
        self.address_is_anchor(address).then_some(address.target)
    }

    /// Whether `address` names the anchor (the selected file).
    fn address_is_anchor(&self, address: &CursorAddress) -> bool {
        self.active_file_id().as_ref() == Some(&address.file)
    }

    /// The pinned address when it is divergent (names a file other than the
    /// anchor). Cloned: callers act on it while mutating the app — the ones that
    /// only ask *whether* read [`App::cursor_divergent`].
    fn divergent_address(&self) -> Option<CursorAddress> {
        let address = self.pinned_address()?;
        (!self.address_is_anchor(address)).then(|| address.clone())
    }

    /// `address`'s `[start, end)` span in *its own file's* row list: the live
    /// layout when it names the anchor (the one file whose rows can carry the
    /// in-place editor), its prepared section otherwise. `None` when the file
    /// isn't resolvable or the target isn't in its rows.
    fn address_file_span(&self, address: &CursorAddress) -> Option<Range<usize>> {
        if self.address_is_anchor(address) {
            return target_span(&self.diff_layout(self.diff_pane_width()), address.target);
        }
        let section = self.address_section(&address.file)?;
        target_span(&section.rows, address.target)
    }

    /// Where `address`'s target starts on screen and how many of its rows the
    /// window actually draws: `(first window row, drawn rows)`, window rows being
    /// screen rows counted from the top of the diff pane. `None` when the target's
    /// *first* row isn't drawn at all — its file isn't in the prepared window, or
    /// that row is among the ones the segment skips (the anchor's rows above the
    /// offset, or a strip tail past the viewport).
    ///
    /// A multi-row target straddling the viewport bottom is a legitimate answer
    /// with `drawn < span.len()`: the user can see it, which is all the visibility
    /// test of §3.3g step 2 asks. [`App::address_window_span`] is the stricter
    /// read, for callers that need the whole target on screen.
    fn address_window_placement(&self, address: &CursorAddress) -> Option<(usize, usize)> {
        let window = self.diff_window(self.diff_pane_width(), self.diff_viewport.get());
        let mut drawn = 0usize;
        for segment in &window.segments {
            if segment.id.as_ref() == Some(&address.file) {
                let span = self.address_file_span(address)?;
                // The segment draws `row_range` of its file's own rows; the span
                // has to start inside that to have a screen position at all.
                if span.start < segment.row_range.start || span.start >= segment.row_range.end {
                    return None;
                }
                let base = drawn + (span.start - segment.row_range.start);
                return Some((base, span.end.min(segment.row_range.end) - span.start));
            }
            drawn += segment.rows();
        }
        None
    }

    /// `address`'s span in window rows, when the window draws the target *whole*.
    /// `None` when any of it is clipped — the caller wanting the visible head of a
    /// clipped target reads [`App::address_window_placement`] instead.
    fn address_window_span(&self, address: &CursorAddress) -> Option<Range<usize>> {
        let span = self.address_file_span(address)?;
        let (base, drawn) = self.address_window_placement(address)?;
        (drawn == span.len()).then(|| base..base + span.len())
    }

    /// `address`'s span in the **offset domain**: physical rows counted from the
    /// anchor file's own row 0, which is where `diff_scroll` lives (plan 006
    /// §3.2a's extended domain, plus its mirror image above the anchor). This is
    /// what [`App::reveal_address`] does its arithmetic in — [`App::
    /// address_window_span`]'s screen rows move with the offset, so they cannot
    /// express where the offset should go.
    ///
    /// Signed, because a file *before* the anchor sits at negative offsets: the
    /// position the renormalizer turns back into a (previous file, offset) pair,
    /// which is exactly how the upward walk flips. Files between the anchor and
    /// the address are prepared on the way (§3.4).
    fn address_stream_span(&mut self, address: &CursorAddress) -> Option<Range<i64>> {
        let anchor = self.stream_position()?;
        let index = self.stream_index_of_exact(&address.file)?;
        let width = self.diff_pane_width();
        if index != anchor {
            // The address's own rows have to be readable before its span resolves;
            // a walk upward reaches files the window never prepared.
            self.prepare_section(index, width)?;
        }
        let span = self.address_file_span(address)?;
        let mut base = 0i64;
        if index >= anchor {
            for file in anchor..index {
                base += self.stream_rows(file, width) as i64;
            }
        } else {
            for file in index..anchor {
                base -= self.stream_rows(file, width) as i64;
            }
        }
        Some(base + span.start as i64..base + span.end as i64)
    }

    /// The prepared section `file` resolves through, by exact identity — never
    /// the anchor (whose rows are the live layout).
    fn address_section(&self, file: &FileId) -> Option<Rc<FileSection>> {
        let index = self.stream_index_of_exact(file)?;
        let key = self.layout_key(self.diff_pane_width());
        let (_, section) = self.prepared_section(index, key, self.stream_generation.get())?;
        Some(section)
    }

    /// Whether `address` still resolves: an anchor address needs only its target
    /// in the anchor's layout; a divergent one needs its file in the prepared
    /// window (which is what makes the boundary visible in the first place) and
    /// its target in that file's rows.
    fn cursor_address_valid(&self, address: &CursorAddress) -> bool {
        if self.address_is_anchor(address) {
            return self.review_index_of(address.target).is_some();
        }
        self.window_holds(&address.file) && self.address_file_span(address).is_some()
    }

    /// Whether the currently prepared window draws any of `file`'s rows.
    fn window_holds(&self, file: &FileId) -> bool {
        self.diff_window(self.diff_pane_width(), self.diff_viewport.get())
            .segments
            .iter()
            .any(|segment| segment.id.as_ref() == Some(file))
    }

    /// The active view's diff-pane cursor/editor state: the status view's own
    /// (`status_pane`), the review session's (`review.pane`), or the history
    /// view's (`history_pane`). `None` only in review without a session. This is
    /// what lets the cursor seam serve all three views from one implementation.
    fn active_pane(&self) -> Option<&DiffPaneState> {
        match self.view {
            ViewMode::Status => Some(&self.status_pane),
            ViewMode::Review => self.review.as_ref().map(|review| &review.pane),
            ViewMode::History => Some(&self.history_pane),
        }
    }

    fn active_pane_mut(&mut self) -> Option<&mut DiffPaneState> {
        match self.view {
            ViewMode::Status => Some(&mut self.status_pane),
            ViewMode::Review => self.review.as_mut().map(|review| &mut review.pane),
            ViewMode::History => Some(&mut self.history_pane),
        }
    }

    /// Clamp the diff cursor after a relist or a comment deletion shrinks the row
    /// list: keep the pinned target when it still resolves, else snap to the last
    /// physical row's target (top when the list is empty).
    fn clamp_review_cursor(&mut self) {
        // Anchor-domain only: an unset cursor already means "top", and a divergent
        // one is the sweep's business (`normalize_cursor`), not this clamp's — its
        // target indexes another file's rows entirely (plan 007 §3.3b).
        let Some(pinned) = self.pinned_anchor_target() else {
            return;
        };
        if self.review_index_of(pinned).is_some() {
            return; // still resolves
        }
        // Snap to the last physical row's target (`None`/top when the list is
        // empty: `review_target_at` of an empty layout is `None`).
        let count = self.review_row_count();
        let target = self.review_target_at(count.saturating_sub(1));
        self.set_cursor_on_anchor(target);
    }

    /// The comment id under the cursor in the selected file, or `None` when the
    /// cursor rests on a code/hunk row (so `x` there is a silent no-op).
    fn cursor_comment_id(&self) -> Option<u64> {
        target_comment_id(self.review_cursor_target()?)
    }

    /// The comment id under the cursor **wherever it points** — the divergence-
    /// aware counterpart of [`App::cursor_comment_id`], a separate accessor rather
    /// than an overload of it (plan 007 §3.3j). Read by the actions that converge
    /// before they act; `]`/`[` place their own cursor and keep the anchor-domain
    /// one.
    fn cursor_address_comment_id(&self) -> Option<u64> {
        target_comment_id(self.cursor_address()?.target)
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
            self.set_cursor_on_anchor(cursor);
        }
        self.review_reveal_cursor();
    }

    /// Focus the diff pane in whichever view is active.
    fn focus_active_diff(&mut self) {
        match self.view {
            ViewMode::Status => self.focus = Focus::Diff,
            ViewMode::Review => self.set_review_focus(ReviewFocus::Diff),
            ViewMode::History => self.history_focus = HistoryFocus::Diff,
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
        // Eligibility before the flip (plan 007 §3.3g step 3): a code/hunk row is
        // a silent no-op, and a no-op must not hand the anchor to another file.
        let Some(id) = self.cursor_address_comment_id() else {
            return; // code / hunk row: no-op
        };
        if !self.converge_on_cursor() {
            return;
        }
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
                if self.apply_active_comments(set) {
                    self.invalidate_comment_rows();
                }
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
        self.comment_at_cursor();
    }

    /// The shared tail of `c` in both views (plan 007 §3.3g steps 3–5): decide
    /// what the row under the cursor would do *before* anything moves, converge on
    /// its file only when the editor would really open, then open it there.
    fn comment_at_cursor(&mut self) {
        let opens = self
            .cursor_address()
            .is_some_and(|address| self.editor_opens_at(&address));
        if opens && !self.converge_on_cursor() {
            return;
        }
        match self.cursor_address() {
            // Re-resolved after the flip: the pinned address is the authority, not
            // the one the eligibility check read.
            Some(address) => self.open_editor_at(&address),
            // No addressable row at all (an empty diff): the same flash as ever.
            None => self.flash = Some(Flash::info("can't comment here")),
        }
    }

    /// Open the in-place editor for `address`, the row under the (already-revealed
    /// and converged) diff cursor: edit the human note there, refuse an agent
    /// note, or anchor a new comment on a code row (a hunk header or unanchorable
    /// row flashes). Shared by the status and review comment actions once their
    /// per-view gates have run; `active_comment`/`address_code_anchor` resolve
    /// against whichever view is active, so `save_comment` scopes the note
    /// correctly.
    fn open_editor_at(&mut self, address: &CursorAddress) {
        // Capture the authoring identity *now*, at open: a watcher `reload()` plus
        // an external checkout can move the current branch/HEAD while the editor is
        // open, and the save must land where the note was authored. `None` means
        // the active view can't author (a non-authoring review / History) — no open.
        let Some(identity) = self.authoring_identity() else {
            return;
        };
        // A comment/orphan row: edit a human note, or refuse an agent note.
        if let Some(id) = target_comment_id(address.target) {
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
        match self.address_code_anchor(address) {
            Some(anchor) => self.set_editor(CommentEdit::new_comment(anchor, identity)),
            None => self.flash = Some(Flash::info("can't comment here")),
        }
    }

    /// Install `edit` as the active pane's in-place editor, drop the cached row
    /// layout so it re-expands with the editor box, and reveal the caret. Never
    /// reached in History: `authoring_identity` returns `None` there, so the
    /// pane's `editing` slot exists but stays empty.
    fn set_editor(&mut self, edit: CommentEdit) {
        // The editor box renders inline in the *anchor's* rows, so it can only
        // ever open on a converged cursor — §3.3g's convergence runs before every
        // open, and this is the one choke point every open goes through.
        debug_assert!(
            !self.cursor_divergent(),
            "the in-place editor opened on a divergent cursor"
        );
        if let Some(pane) = self.active_pane_mut() {
            pane.editing = Some(edit);
        }
        self.relayout_comment_rows();
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
        // they fall through to the "can't comment here" flash. Decided on the
        // *cursor's* file and before any flip: a flash must not reorient the view
        // (plan 007 §3.3g step 3).
        if self.cursor_file_conflicted() {
            self.flash = Some(Flash::info("can't comment on a conflicted file"));
            return;
        }
        self.comment_at_cursor();
    }

    /// The anchor for a new comment on `address`'s code row, or `None` on a hunk
    /// header (or a row with no anchorable line). Per plan §3.4: Addition →
    /// New/`new_no`, Deletion → Old/`old_no`, Context → New/`new_no`; `context`
    /// is the line's text. In side-by-side a replaced-line pair anchors to its
    /// new side when present, else its old side.
    ///
    /// Address-aware (plan 007 §3.3j): the anchor's lines come from the live diff,
    /// a divergent file's from its prepared section, so the line read is always
    /// the line the user is pointing at — never the same index in another file,
    /// which is the mis-anchor the pre-007 cursor could produce.
    fn address_code_anchor(&self, address: &CursorAddress) -> Option<CommentAnchor> {
        let RowTarget::Code(li) = address.target else {
            return None;
        };
        let section = match self.address_is_anchor(address) {
            true => None,
            false => Some(self.address_section(&address.file)?),
        };
        let diff = match section.as_deref() {
            Some(section) => &section.diff,
            None => self.active_diff()?,
        };
        let FileDiff::Text(lines) = diff else {
            return None;
        };
        // A hunk row maps to `Code(index)` too; `anchor_for_line` returns `None`
        // for it, so no explicit hunk guard is needed here.
        lines
            .get(li)
            .and_then(|line| anchor_for_line(line, address.file.path().to_string()))
    }

    /// Whether `c` on `address`'s row would actually open the editor — the
    /// eligibility half of [`App::open_editor_at`], mirrored so that an agent
    /// note, a hunk header or a file-header row flashes *without* the view
    /// flipping to its file first (plan 007 §3.3g step 3).
    fn editor_opens_at(&self, address: &CursorAddress) -> bool {
        match target_comment_id(address.target) {
            Some(id) => self
                .active_comment(id)
                .is_some_and(|comment| comment.source == Source::Human),
            None => self.address_code_anchor(address).is_some(),
        }
    }

    /// Whether the file the cursor points at is conflicted — no clean
    /// HEAD-vs-worktree diff to hang a comment on. Divergence-aware, so `c`
    /// decides on the row it would act on rather than on the selected file.
    fn cursor_file_conflicted(&self) -> bool {
        let Some(address) = self.cursor_address() else {
            return false;
        };
        match &address.file {
            FileId::Status { section, path } => self
                .status_entry(*section, path)
                .is_some_and(|entry| entry.change == Change::Conflicted),
            FileId::Review { .. } | FileId::History { .. } => false,
        }
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
        self.relayout_comment_rows();
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
        let top = self.diff_scroll.get().min(self.diff_max_scroll());
        let new_top = if caret < top {
            caret
        } else if caret >= top + viewport {
            caret - viewport + 1
        } else {
            top
        };
        let max_top = count.saturating_sub(viewport);
        self.diff_scroll.set(new_top.min(max_top));
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
                // A changed set means neighbour sections are stale too;
                // `close_editor` below only rebuilds the anchor's rows.
                if same_branch && self.apply_active_comments(set) {
                    self.bump_stream_generation();
                }
                self.close_editor();
                if same_branch {
                    // Land the cursor on the saved note's box (whatever it renders as).
                    if let Some(id) = id {
                        let target = self
                            .comment_row_index(id)
                            .and_then(|row| self.review_target_at(row));
                        self.set_cursor_on_anchor(target);
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

    /// Close the editor: drop the edit slot, rebuild the anchor's rows (its box is
    /// gone, or a saved box takes its place), and clamp the cursor to the new list.
    /// Anchor-only — a save that actually changed the inbox bumps the stream in
    /// `save_edit`, and a discard changed nothing outside these rows.
    fn close_editor(&mut self) {
        if let Some(pane) = self.active_pane_mut() {
            pane.editing = None;
        }
        self.relayout_comment_rows();
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
    ///
    /// Anchor domain: a divergent cursor has no row in *this* layout, so it reads
    /// as `0` here — the address-aware readers ([`App::cursor_address`],
    /// [`App::cursor_window_span`]) are what resolve one (plan 007 §3.3j).
    pub fn review_cursor(&self) -> usize {
        self.review_cursor_target()
            .and_then(|target| self.review_index_of(target))
            .unwrap_or(0)
    }

    /// Whether the cursor highlight is painted at all: only with the diff pane
    /// focused (plan §3.4), so it never shows while a file list or the Graph is
    /// focused — and never while the in-place editor is open, whose box shows a
    /// caret instead (the row underneath it isn't highlighted either).
    fn cursor_highlight_visible(&self) -> bool {
        !self.editing() && self.diff_focused() && self.active_pane().is_some()
    }

    /// The `[start, end)` physical-row span to highlight while rendering, in the
    /// anchor's layout; `None` when the highlight isn't visible (see
    /// [`App::cursor_highlight_visible`]) or the cursor is divergent. A comment
    /// box spans several rows, so the whole box is highlighted, not just its
    /// first row.
    pub fn review_cursor_highlight(&self) -> Option<Range<usize>> {
        if !self.cursor_highlight_visible() {
            return None;
        }
        self.review_cursor_span()
    }

    /// The cursor highlight as `(file, span)`: which stream file to paint it in,
    /// and the `[start, end)` rows of its target in *that file's own* layout
    /// (plan 007 §3.3i). The renderer matches this per window segment, so a
    /// divergent cursor highlights its strip row while the anchor stays put; for
    /// an anchor cursor it is [`App::review_cursor_highlight`] plus the anchor's
    /// identity, and the same gates apply (nothing while the editor is open or a
    /// list pane is focused).
    pub fn cursor_highlight_span(&self) -> Option<(FileId, Range<usize>)> {
        if !self.cursor_highlight_visible() {
            return None;
        }
        let address = self.cursor_address()?;
        let span = self.address_file_span(&address)?;
        Some((address.file, span))
    }

    /// The address the cursor resolves to right now: the pinned one, or — when
    /// unset — the anchor's own first target (`None`'s meaning, §3.3a). Also the
    /// test/observability accessor for the divergence invariant.
    pub fn cursor_address(&self) -> Option<CursorAddress> {
        match self.pinned_address() {
            Some(address) => Some(address.clone()),
            None => Some(CursorAddress {
                file: self.active_file_id()?,
                target: self.review_target_at(0)?,
            }),
        }
    }

    /// Whether the cursor addresses a file other than the anchor.
    pub fn cursor_divergent(&self) -> bool {
        self.pinned_address()
            .is_some_and(|address| !self.address_is_anchor(address))
    }

    /// The cursor's span in *window* rows — screen rows from the top of the diff
    /// pane — or `None` when its file isn't in the prepared window (see
    /// [`App::address_window_span`]).
    pub fn cursor_window_span(&self) -> Option<Range<usize>> {
        let address = self.cursor_address()?;
        self.address_window_span(&address)
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
            self.clear_divergent_cursor();
            self.prepare_post_toggle_window();
        } else {
            self.reveal_history_panel();
        }
    }

    fn reveal_history_panel(&mut self) {
        self.show_changes = true;
        self.history_focus = HistoryFocus::Graph;
        self.clear_divergent_cursor();
        self.prepare_post_toggle_window();
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
        // Before the view changes, while the home pane is still the active one:
        // the sweep only ever inspects the *active* view's pane, so a divergent
        // address left behind would outlive its window until the user came back
        // (plan 007 §3.3b).
        self.clear_divergent_cursor();
        if self.commits.is_empty() {
            self.load_history();
        }
        self.view = ViewMode::History;
        // No bump here: the stream is re-scoped by `load_commit_files` below, which
        // installs the selected commit's file list (plan 009 §3.4).
        self.last_click = None; // a view change resets the double-click tracker (§3.6)
                                // Hidden left column ⇒ the Diff is the only visible pane to focus.
        self.history_focus = if self.show_changes {
            HistoryFocus::Graph
        } else {
            HistoryFocus::Diff
        };
        self.selected_commit = 0;
        self.committed_row = 0;
        self.diff_scroll.set(0);
        self.load_commit_files();
        self.sync_history_diff();
    }

    fn exit_history(&mut self) {
        // Before the view changes, while `history_pane` is still the active one:
        // a divergent address left there would outlive the stream it names (the
        // mirror of `enter_history`'s pre-switch clear, plan 009 §3.1). A
        // *converged* History cursor may stay — re-entry resets it through
        // `load_commit_files`.
        self.clear_divergent_cursor();
        // Return to the session's home view (status or review), not always status.
        self.view = self.home_view();
        // The stream is re-scoped; the home pane's cursor converges (plan 007
        // §3.3b). Belt to `enter_history`'s braces — a session can also start in
        // History, whose exit is the home view's first activation.
        self.clear_divergent_cursor();
        // The stream is the home view's file list now, not the commit's.
        self.bump_stream_generation();
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
    ///
    /// The single place that list — which *is* History's scroll stream — is
    /// installed, so every exit takes the same steps, including the no-commit and
    /// listing-error ones: reset the row and the cursor, install (or clear) the
    /// list, and bump `stream_generation` exactly once, retiring sections built
    /// against the commit being left. That makes the contract "one bump per
    /// file-list installation" rather than per commit *change*: a Graph re-click
    /// or an edge no-op reloads the same commit and bumps too, but it also
    /// returns to `●`, so nothing could have reached those sections anyway (plan
    /// 009 §3.4).
    fn load_commit_files(&mut self) {
        self.committed_row = 0;
        self.committed_state.borrow_mut().select(None);
        // The arriving list is a different stream: any address into the old one is
        // meaningless, divergent or not. `enter_history` switches the view before
        // calling this, so the write lands on `history_pane` (plan 009 §3.1).
        self.set_cursor_on_anchor(None);
        let listed = self
            .selected_commit_info()
            .map(|commit| self.repo.commit_files(commit));
        self.commit_files_failed = matches!(listed, Some(Err(_)));
        self.commit_files = match listed {
            Some(Ok(files)) => files,
            Some(Err(err)) => {
                tracing::warn!("listing commit files failed: {err:#}");
                Vec::new()
            }
            None => Vec::new(),
        };
        self.bump_stream_generation();
    }

    /// The one seam every *user* change of the committed-changes row goes through
    /// (row 0 is the commit `●` details row; rows below index into
    /// `commit_files`). The History analogue of [`App::select_review_file`]: a
    /// no-op selection leaves the pane alone, so re-selecting the current row
    /// never disturbs the scroll.
    fn select_committed_row(&mut self, row: usize) {
        let row = row.min(self.commit_files.len());
        if row == self.committed_row {
            return;
        }
        self.committed_row = row;
        // `None` is the top-of-layout reset; the arriving row's layout doesn't
        // exist yet (it's built by the trailing `sync_active`).
        self.set_cursor_on_anchor(None);
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
            HistoryFocus::CommittedChanges => self.select_committed_row(if down {
                self.committed_row + 1
            } else {
                self.committed_row.saturating_sub(1)
            }),
            // The `●` details row is a paragraph, not a diff: it has no cursor
            // rows and scrolls by `diff_scroll`, so there j/k stay a row scroll
            // (plan 009 §3.6).
            HistoryFocus::Diff if self.history_shows_details() => self.scroll_diff(down, 1),
            HistoryFocus::Diff => self.review_move_cursor(down, 1),
        }
    }

    /// A half page in the history view, by focused sub-pane — the mirror of
    /// `review_half_page`. With the Graph or the file list focused there is no
    /// cursor to move, so it is a viewport tick that crosses file boundaries when
    /// the stream is on; with the Diff focused it moves the cursor, except on the
    /// `●` details row, which keeps the paragraph scroll (plan 009 §3.6).
    fn history_half_page(&mut self, down: bool) {
        match self.history_focus {
            HistoryFocus::Diff if self.history_shows_details() => {
                self.scroll_diff(down, self.half_page())
            }
            HistoryFocus::Diff => self.review_move_cursor(down, self.half_page() as usize),
            HistoryFocus::Graph | HistoryFocus::CommittedChanges => {
                self.list_scroll_half_page(down)
            }
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
                self.select_committed_row(if bottom { self.commit_files.len() } else { 0 });
            }
            // On a file row g/G are the cursor's edges within the *current* file,
            // exactly as in Review; the `●` details paragraph keeps the viewport
            // edges it has no cursor for (plan 009 §3.6).
            HistoryFocus::Diff if self.history_shows_details() => {
                self.diff_scroll
                    .set(if bottom { self.diff_max_scroll() } else { 0 });
            }
            HistoryFocus::Diff => self.cursor_to_edge(bottom),
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
    pub fn on_resize(&mut self, cols: u16, rows: u16) {
        self.last_click = None;
        // A resize re-keys every section and re-cuts the window; the file a
        // divergent cursor names may not be in the new one, and its rows are
        // rebuilt regardless (plan 007 §3.3b).
        self.clear_divergent_cursor();
        // A resize changes the window's geometry: prepare the sections the new
        // viewport needs, on the event path (plan 006 §3.3). The recorded pane
        // rect is still the *pre-resize* one and its width keys both the layout
        // and the section cache, so preparing against it would tag every section
        // for a geometry the next frame no longer draws — a short window until
        // some later event happens to re-prepare. Derive the new pane instead.
        let (width, height) = self.diff_geometry_for(cols, rows);
        self.ensure_diff_window(width, height);
    }

    /// The recorded pane rect is still the *pre-toggle* one, so prepare against
    /// the derived post-toggle width instead (the stale width would tag every
    /// section for a geometry the next frame no longer draws).
    fn prepare_post_toggle_window(&mut self) {
        let body = self.body_area.get().width;
        if body == 0 {
            return;
        }
        let list = if self.show_changes {
            self.changes_pane_width(body)
        } else {
            0
        };
        self.ensure_diff_window(
            body.saturating_sub(list).saturating_sub(2),
            self.diff_viewport.get(),
        );
    }

    /// The diff pane's geometry for a terminal `cols` × `rows`, derived the way
    /// [`crate::ui::draw`] lays the Status and Review bodies out: the file list
    /// takes [`App::changes_pane_width`] off the left when shown, and the pane's
    /// block borders take a column on each side — so the width returned is exactly
    /// what the renderer passes to [`App::diff_layout`], which is what makes the
    /// sections prepared here match the key the next frame reads them under.
    ///
    /// The height is only a fill target, so the whole terminal height stands in
    /// for the pane's: overshooting prepares at most a section the frame won't
    /// draw, while undershooting would leave the shortfall this is here to
    /// prevent. History's diff pane sits in the same place, so the derivation
    /// serves it too.
    fn diff_geometry_for(&self, cols: u16, rows: u16) -> (u16, u16) {
        let list = if self.show_changes {
            self.changes_pane_width(cols)
        } else {
            0
        };
        (cols.saturating_sub(list).saturating_sub(2), rows)
    }

    /// Re-prepare the window for the pane's recorded geometry. The seam every
    /// change to the layout key ends with: sections are tagged with the key they
    /// were built for, so a wrap / line-number / diff-mode / cross-file toggle
    /// invalidates all of them at once, and the render path never computes. The
    /// toggles hold that invariant themselves rather than leaning on the trailing
    /// `sync_active` of whichever path dispatched them.
    fn reprepare_diff_window(&mut self) {
        let area = self.diff_area.get();
        self.ensure_diff_window(area.width, area.height);
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
            let divider_changed = (self.hovering_divider || self.hovering_hdivider) != was;
            // Hover slides an *already-open* dropdown (hunk behaviour); it never
            // opens one (opening is click-only). Redraw when either changes.
            let menu_changed = self.menu_hover(pos);
            return divider_changed || menu_changed;
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
            // Trackpad horizontal scroll shifts code content only, view-agnostic
            // (Status / Review / History), and — like vertical — clears the
            // double-click tracker.
            MouseEventKind::ScrollRight => {
                self.last_click = None;
                self.horizontal_scroll(pos, true);
            }
            MouseEventKind::ScrollLeft => {
                self.last_click = None;
                self.horizontal_scroll(pos, false);
            }
            _ => {}
        }
        self.sync_active();
        true
    }

    /// Shift the diff pane's code content horizontally by [`HSCROLL_STEP`] display
    /// columns (plan §3.5). A no-op unless the tick is over the diff pane and wrap
    /// is off (wrap and h-scroll are mutually exclusive). Clamped to the longest
    /// code line at write time to bound growth; the render re-clamps, so a divider
    /// drag or `n` toggle between events can't leave a stale over-scroll.
    fn horizontal_scroll(&mut self, pos: Position, right: bool) {
        // An open dropdown captures input over the pane beneath it (like keys and
        // clicks); a horizontal tick must not shift the diff under it (FIX 1). Wrap
        // and h-scroll are mutually exclusive, and the tick must be over the diff.
        if self.open_menu.is_some() || self.wrap_lines || !self.diff_area.get().contains(pos) {
            return;
        }
        let max = self.max_hscroll();
        let current = self.diff_hscroll.min(max);
        self.diff_hscroll = if right {
            (current + HSCROLL_STEP).min(max)
        } else {
            current.saturating_sub(HSCROLL_STEP)
        };
    }

    /// The horizontal-scroll offset the renderer actually shifts code by: the
    /// stored `diff_hscroll` clamped to the longest code line at *read* time
    /// (plan §3.5), and always 0 while wrap is on (the two are mutually exclusive).
    pub fn effective_hscroll(&self) -> usize {
        if self.wrap_lines {
            return 0;
        }
        self.diff_hscroll.min(self.max_hscroll())
    }

    /// The largest h-scroll offset that still leaves content on screen: the
    /// longest code line minus the visible content width (unified: pane − gutter −
    /// sign; SBS: the narrower column's content width). Computed from current
    /// geometry, so a divider drag or `n` toggle is reflected immediately (plan
    /// §3.5). Read by both the scroll event and the render clamp.
    fn max_hscroll(&self) -> usize {
        self.active_max_line_width()
            .saturating_sub(self.hscroll_content_width())
    }

    /// The visible content width h-scroll clamps against for the active mode.
    fn hscroll_content_width(&self) -> usize {
        let pane = self.diff_pane_width();
        let lines: &[DiffLine] = match self.active_diff() {
            Some(FileDiff::Text(lines)) => lines,
            _ => &[],
        };
        let number_width = crate::ui::diff_view::line_number_width(lines);
        match self.diff_mode {
            DiffMode::Unified => crate::ui::diff_view::unified_content_width(
                pane as usize,
                self.show_line_numbers,
                number_width,
            ),
            DiffMode::SideBySide => {
                let (left_w, right_w) = sbs_columns(pane);
                let left = crate::ui::diff_view::sbs_content_width(
                    left_w,
                    self.show_line_numbers,
                    number_width,
                );
                let right = crate::ui::diff_view::sbs_content_width(
                    right_w,
                    self.show_line_numbers,
                    number_width,
                );
                left.min(right)
            }
        }
    }

    /// The longest sanitized *code*-line display width in the active diff (hunk
    /// headers excluded — a wide hunk header must not extend the clamp, plan §3.5),
    /// memoized per `(diff_generation, view)` so it is recomputed exactly when the
    /// active diff object or the view changes, not on resize / mode / wrap.
    fn active_max_line_width(&self) -> usize {
        let key = (self.diff_generation.get(), self.view);
        if let Some((cached_key, width)) = self.max_line_width.get() {
            if cached_key == key {
                return width;
            }
        }
        let width = match self.active_diff() {
            Some(FileDiff::Text(lines)) => lines
                .iter()
                .filter(|line| line.kind != LineKind::Hunk)
                .map(|line| {
                    crate::ui::diff_view::sanitize(&line.text)
                        .chars()
                        .map(crate::ui::char_width)
                        .sum::<usize>()
                })
                .max()
                .unwrap_or(0),
            _ => 0,
        };
        self.max_line_width.set(Some((key, width)));
        self.max_line_width_compute_count
            .set(self.max_line_width_compute_count.get() + 1);
        width
    }

    /// Note that the active diff *object* changed, invalidating the memoized
    /// longest-code-line width (plan §3.5). Called at every diff (re)assignment.
    fn bump_diff_generation(&self) {
        self.diff_generation.set(self.diff_generation.get() + 1);
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

        // Menu hit-testing runs *after* the editor commit block (so a title click
        // while editing commits the editor first) and *before* the diff-pane
        // double-click logic. A fully-consumed menu click resets the double-click
        // tracker (like an `[x]` consumption); a click-away closes the menu but
        // falls through to route normally.
        if self.menu_click(pos) {
            self.last_click = None;
            return;
        }

        // A click on a strip row is handled entirely by `strip_click` (plan 007
        // §3.3c–e), resolved from the per-frame window hit map —
        // `diff_row_at`/`hit_target` only ever resolve rows inside the *anchor's*
        // own layout, so a strip row would otherwise be inert. An anchor-row hit
        // (or no hit at all: outside the diff pane, on History's details pane, or
        // in the shortfall region) falls through to the unchanged path below.
        if let Some(hit) = self.window_hit_at(pos) {
            if !hit.is_anchor {
                self.strip_click(&hit, pos, now);
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
        // Build the layout FIRST: a queued relayout earlier in this event batch
        // (a `w`/`n`/resize before the click, drained before any redraw) rebuilds
        // it here and re-anchors `diff_scroll`. Reading the offset only after that
        // — and clamping against the just-built row count, not the stale render-
        // time metric — keeps the layout, offset, and lookup one consistent
        // snapshot, so the click resolves against the rows the next frame paints.
        let rows = self.diff_layout(self.diff_pane_width()).len();
        let offset = self.paint_offset(rows, diff.height as usize);
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
    /// the in-place editor, or with no pane at all. Reuses the C1
    /// physical→logical seam (`review_target_at`) for the row and C6's recorded
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
            // handling ran above); neither is the file header.
            RowTarget::Editor | RowTarget::FileHeader => return None,
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
    /// exactly like the `c` key. History authors no comments, so there it is
    /// idempotent with the single click: it places the same cursor and stops.
    fn double_click_comment(&mut self, pos: Position) {
        self.place_diff_cursor(pos);
        match self.view {
            ViewMode::Status => self.status_comment_action(),
            ViewMode::Review => self.review_comment_action(),
            ViewMode::History => {}
        }
    }

    /// Focus the diff pane and move its cursor onto the clicked physical row —
    /// History's single click, and the double-click path, where it is what makes
    /// the editor anchor where the user clicked. A no-op for a click outside the
    /// diff or past its last row (the cursor stays put).
    fn place_diff_cursor(&mut self, pos: Position) {
        self.focus_active_diff();
        let Some(row) = self.diff_row_at(pos) else {
            return;
        };
        if row < self.review_row_count() {
            let target = self.review_target_at(row);
            self.set_cursor_on_anchor(target);
        }
    }

    /// Handle a left-click on a strip row — a row of a file below the anchor in
    /// the prepared window (plan 007 §3.3c–e).
    ///
    /// A single click is **pure cursor placement**: it pins a divergent
    /// [`CursorAddress`] at the clicked target and does nothing else — no flip,
    /// no reveal, no selection or title change, zero view movement. (Focusing the
    /// diff pane is not movement, and `place_cursor` requires it; an anchor click
    /// focuses the same way.) Because the frame stays put, the clicked row is
    /// still under the pointer for a second click, which is what makes the
    /// double-click of §3.3(e) pair naturally — the flip-and-reveal this used to
    /// do moved the row out from under it.
    ///
    /// The `[x]` close cell is checked first and is target-qualified: the clicked
    /// row must itself be that comment's box, in that box's column, with the
    /// recorded rect under the pointer — never "some rect happens to cover this
    /// coordinate". A dup-path Status file can render one comment in both
    /// sections; the renderer keeps the *anchor's* rect (§3.3d), so the strip
    /// copy's `[x]` simply isn't clickable that frame and the click places the
    /// cursor instead.
    fn strip_click(&mut self, hit: &WindowHit, pos: Position, now: Instant) {
        let Some(file) = hit.id.clone() else {
            self.last_click = None;
            return;
        };
        if let Some(id) = self.strip_close_click(hit, pos) {
            self.delete_comment_id(id);
            self.last_click = None;
            return;
        }
        let target = self.strip_hit_target(hit, pos);
        let double = target
            .as_ref()
            .is_some_and(|t| is_double_click(self.last_click.as_ref(), now, t));
        self.focus_active_diff();
        // A section that vanished between "the row was drawn" and "the click
        // resolved" (the epoch guard screens out most such staleness, but a
        // same-frame invalidation is conceivable) leaves the address invalid, and
        // placing is then a full no-op rather than a cursor pointing nowhere.
        if !self.place_cursor(CursorAddress {
            file,
            target: hit.target,
        }) {
            self.last_click = None;
            return;
        }
        if double {
            self.strip_double_click(hit.target);
            // Reset the tracker so a triple-click's third press can't re-fire.
            self.last_click = None;
        } else {
            self.last_click = target.map(|t| (now, t));
        }
    }

    /// The comment a strip click deletes: the id of the box its row draws, when
    /// the click also fell in that box's column and on that box's recorded `[x]`
    /// rect — the target-qualified, column-qualified test of §3.3(d).
    fn strip_close_click(&self, hit: &WindowHit, pos: Position) -> Option<u64> {
        let id = target_comment_id(hit.target)?;
        let hit_close = self.in_side_column(pos, hit.side)
            && self.comment_close_rect(id).is_some_and(|r| r.contains(pos));
        hit_close.then_some(id)
    }

    /// The double-click key for a strip-row click (plan 007 §3.3e) — the strip
    /// counterpart of [`App::hit_target`], which resolves against the anchor's
    /// layout and so can't see these rows. Same `HitTarget` shape, with two
    /// differences the strip domain forces: `file` is the *clicked* file's path
    /// (no flip happened, so `active_diff_path` still names the anchor), and a
    /// file header is a real region here rather than a rejection.
    ///
    /// `None` where the row can't pair into a double-click at all: the blank
    /// sibling column of a side-by-side box (mirroring the anchor hit-test), or
    /// the in-place editor.
    fn strip_hit_target(&self, hit: &WindowHit, pos: Position) -> Option<HitTarget> {
        let region = match hit.target {
            RowTarget::Code(line) => ClickRegion::Code(line),
            RowTarget::Comment(id) | RowTarget::Orphan(id) => {
                if !self.in_side_column(pos, hit.side) {
                    return None;
                }
                ClickRegion::Comment(id)
            }
            RowTarget::FileHeader => ClickRegion::FileHeader,
            RowTarget::Editor => return None,
        };
        Some(HitTarget {
            generation: self.layout_generation.get(),
            view: self.view,
            file: hit.id.as_ref().map(|id| id.path().to_string()),
            region,
        })
    }

    /// Act on a recognized double-click whose first press placed the cursor on a
    /// strip row (plan 007 §3.3e): converge-then-act through §3.3(g)'s machinery,
    /// which the cursor now addresses. A code row or a human note opens the editor
    /// exactly as `c` would; an agent note flashes read-only *without* flipping
    /// (eligibility before the flip); a file header only converges — selecting the
    /// file is the whole act there.
    fn strip_double_click(&mut self, target: RowTarget) {
        if target == RowTarget::FileHeader {
            self.converge_on_cursor();
            return;
        }
        match self.view {
            ViewMode::Status => self.status_comment_action(),
            ViewMode::Review => self.review_comment_action(),
            ViewMode::History => {}
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
    /// diff pane takes focus and moves its cursor to the clicked row.
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
            self.select_committed_row(row);
        } else if self.diff_area.get().contains(pos) {
            // The same focus-and-place the double-click path already uses. On the
            // `●` details row the layout is empty, so no row matches and the click
            // only focuses. A strip row never gets here — `on_left_down` routes
            // those to `strip_click`.
            self.place_diff_cursor(pos);
        }
    }

    /// Route a click in the review view to its sub-pane: the file List selects a
    /// row and focuses the list, the diff pane just takes focus. Staging is inert
    /// in review, so a click in the marker column only selects — it never stages.
    fn review_click(&mut self, pos: Position) {
        let list = self.review_list_area();
        if list.contains(pos) {
            let Some(review) = self.review.as_mut() else {
                return;
            };
            review.focus = ReviewFocus::List;
            let row = review.list_state.borrow().offset() + (pos.y - list.y) as usize;
            if row >= review.files.len() {
                return;
            }
            review.selected = row;
            // A new file starts at its first row; a list click also ends any
            // divergence, both by this reset and by leaving the diff pane
            // (plan 007 §3.3b).
            self.set_cursor_on_anchor(None);
        } else if let Some(row) = self.diff_row_at(pos) {
            // Focus the diff and move the cursor to the clicked row. A click below
            // the last row just focuses (no cursor move); wheel scroll never moves
            // the cursor (that path is `review_scroll`). `diff_row_at` resolves it
            // against the offset the renderer paints with, so a strip row maps past
            // the anchor's last row and hits nothing — inert until C5 (§3.6).
            self.set_review_focus(ReviewFocus::Diff);
            let count = self.review_row_count();
            if row < count {
                let target = self.review_target_at(row);
                self.set_cursor_on_anchor(target);
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
                // Scrolling while editing never crosses (plan §3.4).
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
            Some(Focus::Diff) => self.wheel_scroll_diff(down),
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
            self.wheel_scroll_diff(down);
        }
    }

    /// A wheel tick over the diff pane, in any view. With cross-file scroll off —
    /// or on History's `●` details row, which has no anchor — this is the plain
    /// per-file clamp it always was; with it on the tick is a signed delta in the
    /// extended stream domain (plan 006 §3.2a).
    fn wheel_scroll_diff(&mut self, down: bool) {
        if !self.cross_file_scroll {
            self.scroll_diff(down, SCROLL_STEP);
            return;
        }
        let step = i64::from(SCROLL_STEP);
        self.wheel_scroll_window(if down { step } else { -step });
    }

    /// Whether the diff viewport is pinned against its hard edge in the scroll
    /// direction (the clamped offset is `0` going up, or the maximum going down).
    /// An empty or short diff (max scroll `0`) is at *both* edges — an immediate
    /// boundary in either direction (plan §3.4). Metrics are from the last render,
    /// which is correct for the same-file same-layout case this guards.
    fn at_hard_edge(&self, down: bool) -> bool {
        let max = self.diff_max_scroll();
        let offset = self.diff_scroll.get().min(max);
        if down {
            offset >= max
        } else {
            offset == 0
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
            self.wheel_scroll_diff(down);
        }
    }

    fn scroll_diff(&mut self, down: bool, step: u16) {
        // Clamp to the current content first: a same-file refresh may have shrunk
        // the diff below the preserved offset, and scrolling up must not stay
        // stuck past the new end (metrics are fresh here, post-render). The step is
        // a small terminal-row count (`u16`); the offset it moves is a `usize`.
        let step = step as usize;
        let max = self.diff_max_scroll();
        let current = self.diff_scroll.get().min(max);
        self.diff_scroll.set(if down {
            (current + step).min(max)
        } else {
            current.saturating_sub(step)
        });
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
        // Converge before reading the section: which way the toggle goes is the
        // cursor's file's answer, not the previously selected file's (§3.3g).
        // `run_on_selected` converges too — a no-op once this one has.
        if !self.converge_on_cursor() {
            return;
        }
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

    /// Run a path-based git op on the selected file, then refresh. Staging is
    /// eligible on any file, so the convergence (§3.3g) is unconditional: with the
    /// cursor walked into a following file, space/`s`/`u` act on *that* file and
    /// the selection follows it there.
    fn run_on_selected(&mut self, action: &str, op: GitOp) {
        if !self.converge_on_cursor() {
            return;
        }
        let Some((_, path)) = self.selected_section_path() else {
            return;
        };
        self.after_mutation(action, op(&self.repo, &path));
    }

    /// Open the discard confirmation for the file under the cursor — which the
    /// convergence has just made the selected one (§3.3g; the caller's gate has
    /// already ruled out the ineligible comment-row case).
    fn request_discard(&mut self) {
        if !self.converge_on_cursor() {
            return;
        }
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
            self.clear_divergent_cursor();
            self.prepare_post_toggle_window();
        } else {
            self.reveal_changes();
        }
    }

    /// Reveal the Changes panel and focus it — the single home for the reveal
    /// semantics shared by the toggle key, Tab, and `h` when the panel is hidden.
    fn reveal_changes(&mut self) {
        self.show_changes = true;
        self.focus = Focus::Staging;
        self.clear_divergent_cursor();
        self.prepare_post_toggle_window();
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
    ///
    /// **Selection-only.** The fallback is what keeps the *selection* on a file
    /// the user just staged (its row moves sections under it), and it is wrong
    /// for anything that names one exact stream row — a strip click, a cursor
    /// address — which resolve through [`App::stream_index_of_exact`] instead
    /// (plan 007 §3.3a).
    fn index_of(&self, section: Section, path: &str) -> Option<usize> {
        let other = match section {
            Section::Staged => Section::Unstaged,
            Section::Unstaged => Section::Staged,
        };
        self.index_of_exact(section, path)
            .or_else(|| self.index_of_exact(other, path))
    }

    /// The flattened index of exactly `(section, path)`, no cross-section
    /// fallback — the staged list first, so an unstaged hit is offset by its
    /// length. The lookup [`App::stream_index_of_exact`] needs, and the half
    /// [`App::index_of`] adds its selection-survival fallback to.
    fn index_of_exact(&self, section: Section, path: &str) -> Option<usize> {
        let staged = &self.status.staged;
        match section {
            Section::Staged => staged.iter().position(|e| e.path == path),
            Section::Unstaged => self
                .status
                .unstaged
                .iter()
                .position(|e| e.path == path)
                .map(|i| staged.len() + i),
        }
    }

    /// Recompute the cached diff when the selected file changes, or when an
    /// external refresh marked it dirty. Navigating to a different file resets
    /// the scroll; a same-file content refresh keeps it.
    fn sync_diff(&mut self) {
        // Path only, not (section, path) — see the `diff_key` field doc.
        let key = self.selected_file().map(|(_, entry)| entry.path.clone());
        // The section is *not* part of the diff key, but it is part of the layout:
        // the file-header row's marker and tone read it (plan 006 §3.1). A
        // same-path staged↔unstaged move therefore drops the layout — and only the
        // layout, so the diff isn't recomputed and the highlight cache (same text,
        // same syntax) stays warm.
        let section = self.selected_file().map(|(section, _)| section);
        if section != self.diff_section {
            // The cursor's address named the row being left; carry a converged one
            // over to the row we landed on before `diff_section` moves under it
            // (plan 007 §3.3a).
            self.rebind_cursor_across_sections(self.diff_section);
            self.diff_section = section;
            *self.layout.borrow_mut() = None;
        }
        let file_changed = key != self.diff_key;
        if !file_changed && !self.diff_dirty {
            return;
        }
        self.diff_dirty = false;
        // Compute into a local first so the immutable borrow of the file list
        // (and repo) is released before assigning the cached fields. The compute
        // counter proves a cross-file crossing touches only the destination
        // file's diff (plan §3.4 laziness).
        let diff = self.selected_file().map(|(_, entry)| {
            self.diff_compute_count
                .set(self.diff_compute_count.get() + 1);
            self.repo.file_diff_head_vs_worktree(entry)
        });
        // A same-file refresh that produced an identical diff is a no-op: keep the
        // warm highlight / SBS caches and the scroll untouched, so a watcher firing
        // on unrelated activity doesn't churn or disturb the view.
        if !file_changed && diff == self.current_diff {
            return;
        }
        self.current_diff = diff;
        self.diff_key = key;
        // The diff object changed: invalidate the memoized longest-line width.
        self.bump_diff_generation();
        if file_changed {
            // Only a different file starts at the top; refreshing the open file in
            // place must not yank the view back up while scrolling.
            self.diff_scroll.set(0);
            // A new file starts unshifted; a same-file refresh keeps the h-scroll
            // (the read-time clamp handles a shrunken longest line — plan §3.5).
            self.diff_hscroll = 0;
            // The new file's layout doesn't exist yet; reset the diff cursor to its
            // top (`None`), resolved to row 0 by the render.
            self.set_cursor_on_anchor(None);
        }
        // The cached row layout describes the previous diff; drop it so the new one
        // is recomputed lazily on next render. Highlights are per-file (keyed by
        // path, then line text), so the departed file's map is simply pruned —
        // whatever the stream still holds stays warm (plan 006 §3.3).
        self.prune_highlight_cache();
        *self.layout.borrow_mut() = None;
        // An in-place refresh may have shrunk the row list under a pinned cursor
        // (e.g. an edit removed lines); clamp it to the new layout. A fresh file
        // already reset the cursor above.
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
        // Keep the visible window prepared: a selection change or a refresh can
        // leave a short anchor with an unfilled strip, and the render path may
        // never compute (plan 006 §3.3). A no-op with cross-file scroll off, with
        // no anchor, or before the first frame (the pane has no geometry yet).
        let area = self.diff_area.get();
        self.ensure_diff_window(area.width, area.height);
    }

    /// Re-read the active view's data: status re-reads the working tree; history
    /// re-walks commits (keeping the cursor on the same commit by oid) and
    /// reloads its file list.
    fn refresh_active(&mut self) {
        match self.view {
            ViewMode::Status => self.refresh(),
            ViewMode::History => {
                let current = self.selected_commit_info().map(|c| c.id);
                self.load_history();
                let found = current.and_then(|id| self.commits.iter().position(|c| c.id == id));
                match found {
                    // A commit's file list and diffs are immutable, so re-finding
                    // the same commit changes nothing but its index in the walk:
                    // the list, the row, the scroll and the cached sections all
                    // stay, and nothing bumps. That is what keeps a watcher tick
                    // on every worktree save from recomputing the strip under a
                    // reader (plan 009 §3.4).
                    Some(index) => {
                        self.selected_commit = index;
                        // Unless the last listing *failed*: an honestly empty
                        // commit stays empty, but a transient error would
                        // otherwise leave History showing no files for as long as
                        // the commit stayed selected (codex review finding).
                        if self.commit_files_failed {
                            self.load_commit_files();
                        }
                    }
                    None => {
                        self.selected_commit = 0;
                        self.load_commit_files();
                    }
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
        // Same trade as the status refresh: a re-resolve can relist the range out
        // from under a divergent cursor, so it snaps back (plan 007 §3.3b).
        self.clear_divergent_cursor();
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
        if let Some(review) = self.review.as_mut() {
            review.authoring = head_oid == Some(spec.head);
            review.branch_key = branch_key;
        }

        let moved = spec.base != old_base || spec.head != old_head;
        // A relist is one top-level mutation and takes one bump, at its end (plan
        // 007 §3.1). The store re-read and the re-anchor below are subordinate to
        // it, so inside a relist they only rebuild the anchor's rows — otherwise a
        // range move that also carries an inbox change would retire the window two
        // or three times. Reached without a relist (the churn-guarded watcher tick)
        // the store re-read is the top-level mutation and keeps its own bump.
        let how = if moved {
            CommentInvalidation::AnchorOnly
        } else {
            CommentInvalidation::Stream
        };

        // Re-read the store from disk (plan §3.2b) so an agent's `rm`/`add` — and
        // any new branch key above — is reflected even when the range OIDs are
        // unchanged. Cheap and write-free, so it can't drive a reload loop.
        let comments_changed = self.reload_review_comments(how);

        if !moved {
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
                // Bailing out before the relist's own bump: if the store re-read
                // above changed the set, its deferred invalidation is owed here or
                // the sections keep rendering the previous comments.
                if comments_changed {
                    self.bump_stream_generation();
                }
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
        self.reanchor_review_comments(CommentInvalidation::AnchorOnly);
        // Every section was computed against the old tips, and the re-anchored
        // boxes above may have moved within them (plan 006 §3.3). One bump, taken
        // once all of the new state is installed — nothing below prepares a
        // section, so no stale one can be re-tagged as live.
        self.bump_stream_generation();
        self.sync_review_diff();
        // The relist rebuilt the row list; keep the cursor's index but clamp it
        // to the new count (plan §3.4).
        self.clamp_review_cursor();
    }

    /// Re-read the comment inbox from disk and replace the in-memory set. Cheap,
    /// write-free (so it can't loop the store-dir watcher), and a no-op when
    /// comments are inactive. On a load error the prior set is kept and an error
    /// flashes at most once (a corrupt store must not spam on every reload).
    ///
    /// Returns whether the in-memory set actually changed, so a caller that
    /// passed `AnchorOnly` still knows a bump is owed if it bails out before
    /// taking its own.
    fn reload_review_comments(&mut self, how: CommentInvalidation) -> bool {
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
                self.invalidate_comments(how);
            }
            return cleared;
        }
        match comments::load(&dir) {
            Ok(store) => {
                let set = store
                    .branches
                    .get(&branch)
                    .map(|b| b.comments.clone())
                    .unwrap_or_default();
                // Elided when the store re-read produced the set already on screen:
                // the common watcher tick must not retire the prepared window.
                let changed = self.apply_review_comments(set);
                if changed {
                    self.invalidate_comments(how);
                }
                self.clear_comment_error();
                changed
            }
            Err(err) => {
                tracing::warn!("re-reading comments store failed: {err:#}");
                self.flash_comment_error(err);
                false
            }
        }
    }

    /// Record the range + run the write-elided re-anchor pass against the current
    /// review diff, replacing the in-memory set with the result. Runs on session
    /// open (plan §3.1.1 / §3.2) and the OID-changed refresh branch; inactive → a
    /// no-op. A store error keeps the prior set and flashes once, so a corrupt
    /// store opens comment-free rather than failing construction.
    fn reanchor_review_comments(&mut self, how: CommentInvalidation) {
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
                if self.apply_review_comments(set) {
                    self.invalidate_comments(how);
                }
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
    /// Returns whether the view's set actually changed, so callers can skip the
    /// invalidation when a reload/re-anchor produced the set already on screen.
    fn apply_active_comments(&mut self, full: Vec<Comment>) -> bool {
        match self.view {
            ViewMode::Status => {
                let set: Vec<Comment> = full.into_iter().filter(is_worktree_scope).collect();
                let changed = set != self.status_comments;
                self.status_comments = set;
                changed
            }
            ViewMode::Review => self.apply_review_comments(full),
            ViewMode::History => false,
        }
    }

    /// Replace `review.comments` from a branch entry's full set, keeping only the
    /// comments scoped to *this* review's exact range (codex-#5): a worktree
    /// comment, or a range comment from a different range, is filtered out.
    /// Returns whether the set actually changed (see [`apply_active_comments`]).
    fn apply_review_comments(&mut self, full: Vec<Comment>) -> bool {
        let Some(review) = self.review.as_mut() else {
            return false;
        };
        let input = review.spec.input.clone();
        let set: Vec<Comment> = full
            .into_iter()
            .filter(|c| is_review_scope(c, &input))
            .collect();
        let changed = set != review.comments;
        review.comments = set;
        changed
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
                // Anchor-only: this runs solely from `new` and from `refresh`, and
                // `refresh` already owns this cycle's single stream bump (plan 007
                // §3.1) — a second one here would retire the window twice per tick.
                self.relayout_comment_rows();
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

    /// Drop the *anchor's* physical row layout so the next render rebuilds it,
    /// leaving every cached neighbour section intact.
    ///
    /// Split from [`invalidate_comment_rows`] because the two have different
    /// blast radii (plan 007 §3.1). The in-place editor only ever renders in the
    /// anchor's rows, so its open/keystroke/close path belongs here: bumping the
    /// stream on every keypress would retire the whole prepared window — every
    /// neighbour section recomputed per typed character.
    fn relayout_comment_rows(&self) {
        *self.layout.borrow_mut() = None;
    }

    /// Drop the anchor's layout *and* retire every cached section: a comment set
    /// actually changed, and neighbours' sections carry *their* comment boxes
    /// (plan 006 §3.3). Only for real inbox mutations — a top-level
    /// refresh/reload/relist owns its own single bump instead (plan 007 §3.1).
    fn invalidate_comment_rows(&self) {
        self.relayout_comment_rows();
        self.bump_stream_generation();
    }

    /// Apply a changed comment set's invalidation at the reach the caller's
    /// context calls for — see [`CommentInvalidation`].
    fn invalidate_comments(&self, how: CommentInvalidation) {
        match how {
            CommentInvalidation::Stream => self.invalidate_comment_rows(),
            CommentInvalidation::AnchorOnly => self.relayout_comment_rows(),
        }
    }

    /// Invalidate every cached section. Anything that can change which files the
    /// stream holds, or what a file's diff or rows contain, lands here: a status
    /// snapshot replacement, a review relist, a commit's file-list installation in
    /// History, a comment mutation, a view change. Layout-key changes need no bump
    /// — a stale tag is caught on access (plan 006 §3.3).
    fn bump_stream_generation(&self) {
        self.stream_generation.set(self.stream_generation.get() + 1);
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
                self.bump_diff_generation();
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
        // Counts as a per-file diff computation (plan §3.4 laziness observable).
        self.diff_compute_count
            .set(self.diff_compute_count.get() + 1);
        let diff = self.repo.range_file_diff(&spec, &file);
        if let Some(review) = self.review.as_mut() {
            review.diff = Some(diff);
            review.diff_key = Some(key);
        }
        self.bump_diff_generation();
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
            if self.history_diff.is_some() {
                self.history_diff = None;
                self.history_diff_key = None;
                self.bump_diff_generation();
            }
            return;
        };
        // Row 0 is the commit itself: the right pane shows details, no file diff.
        if self.committed_row == 0 {
            if self.history_diff_key.is_some() {
                self.history_diff = None;
                self.history_diff_key = None;
                self.bump_diff_generation();
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
        // Counts as a per-file diff computation (plan §3.4 laziness observable),
        // as `sync_diff` and `sync_review_diff` count theirs: this is the anchor's
        // own diff, the one `compute_section` never sees.
        self.diff_compute_count
            .set(self.diff_compute_count.get() + 1);
        self.history_diff = Some(self.repo.commit_file_diff(commit, file));
        self.history_diff_key = key;
        self.bump_diff_generation();
        self.reset_diff_view();
    }

    /// Reset the diff pane to the top and drop the per-file render caches, which
    /// describe the diff being replaced. A different diff also starts unshifted
    /// (Review/History file changes route through here; Status resets h-scroll in
    /// `sync_diff`'s file-changed branch — plan §3.5).
    fn reset_diff_view(&mut self) {
        self.diff_scroll.set(0);
        self.diff_hscroll = 0;
        self.prune_highlight_cache();
        *self.layout.borrow_mut() = None;
    }

    /// Syntax-highlight one already-sanitised line of the *active* file, memoised
    /// per file so scrolling reuses the result instead of re-parsing through
    /// syntect on every frame.
    pub fn highlight(
        &self,
        syntax: &SyntaxReference,
        theme_name: &str,
        text: &str,
    ) -> HighlightedLine {
        self.highlight_for(
            self.active_path().unwrap_or_default(),
            syntax,
            theme_name,
            text,
        )
    }

    /// Syntax-highlight one already-sanitised line of `path`'s content, memoised
    /// in that file's own sub-map. Single-line highlighting carries no cross-line
    /// state (see `ui::syntax`), so the line text is a sound key *within* a file;
    /// across files it is not (identical text, different syntax), which is why the
    /// map is per-file (plan 006 §3.3).
    pub fn highlight_for(
        &self,
        path: &str,
        syntax: &SyntaxReference,
        theme_name: &str,
        text: &str,
    ) -> HighlightedLine {
        if let Some(hit) = self
            .highlight_cache
            .borrow()
            .get(path)
            .and_then(|lines| lines.get(text))
        {
            return Rc::clone(hit);
        }
        let computed: HighlightedLine =
            crate::ui::syntax::highlight_line(syntax, theme_name, text).into();
        self.highlight_cache
            .borrow_mut()
            .entry(path.to_string())
            .or_default()
            .insert(text.to_string(), Rc::clone(&computed));
        computed
    }

    /// Drop the highlight sub-maps of files the pane no longer holds — everything
    /// but the active file's and those with a live cached section. Tying sub-map
    /// lifetime to the section cache is what keeps a long browsing (or scrolling)
    /// session from growing the map without bound (plan 006 §3.3).
    fn prune_highlight_cache(&self) {
        let active = self.active_path();
        let sections = self.sections.borrow();
        self.highlight_cache
            .borrow_mut()
            .retain(|path, _| Some(path.as_str()) == active || sections.holds_path(path));
    }

    /// Everything a built layout depends on besides the file itself. Wrap and the
    /// line-number gutter are both wrap inputs (the gutter sets the content width
    /// a line wraps at), so a change in either invalidates alongside width and
    /// mode (plan §3.3); `cross_file` adds the header row (plan 006 §3.1). Cached
    /// sections carry the same key, so one toggle invalidates the whole stream.
    fn layout_key(&self, width: u16) -> LayoutKey {
        LayoutKey {
            width,
            mode: self.diff_mode,
            wrap: self.wrap_lines,
            line_numbers: self.show_line_numbers,
            cross_file: self.cross_file_scroll,
        }
    }

    /// The diff pane's physical [`LayoutRow`] list at pane width `width`, computed
    /// once per `(diff, comments, mode, width)` and cached. Code lines map 1:1 to
    /// rows; each comment box expands to several rows sharing one `RowTarget`. A
    /// changed width or diff mode rebuilds it (preserving the logical targets).
    /// This is the single backing store read by both the cursor seam and the
    /// renderer.
    pub fn diff_layout(&self, width: u16) -> Ref<'_, Vec<LayoutRow>> {
        let current = self.layout_key(width);
        let first = self.stream_position() == Some(0);
        let cached = self.layout.borrow().as_ref().map(|c| (c.key, c.first));
        let previous = cached.map(|(key, _)| key);
        // A stale `first` is a stale header (the rule row appears or goes) even
        // when every key input still matches, so it forces a rebuild too — and it
        // does so through the same branch, keeping the top-line anchoring below.
        if cached != Some((current, first)) {
            // Anchor the top visible logical line across a *structural* relayout —
            // a resize, a wrap toggle, or a line-number toggle — so the row the
            // user was reading stays at the top (plan §3.3). Skip it when the mode
            // changed (`toggle_diff_mode` resets scroll+cursor itself) and when
            // there was no prior layout (first build, or a comment mutation dropped
            // the cache to `None` — those preserve `diff_scroll` verbatim). A
            // cross-file toggle is skipped for its own reason: re-anchoring would
            // slide the arriving header row straight back off the top, so pressing
            // `f` at the top of a file would draw nothing (plan 006 §3.1).
            let anchor = match previous {
                Some(prev)
                    if prev.mode == current.mode && prev.cross_file == current.cross_file =>
                {
                    let top = self.diff_scroll.get().min(self.diff_max_scroll());
                    self.layout
                        .borrow()
                        .as_ref()
                        .and_then(|c| c.rows.get(top))
                        .map(|row| row.target)
                }
                _ => None,
            };
            let rows = self.build_layout(width);
            // Every actual rebuild bumps the layout generation, the double-click
            // `HitTarget`'s `generation` field (plan §3.6): a resize, mode toggle,
            // or comment mutation (via `invalidate_comment_rows` → `layout = None`)
            // all funnel through here, so a relayout between two clicks is caught.
            self.layout_generation.set(self.layout_generation.get() + 1);
            // Put the anchored target's first row back at the viewport top (0 if it
            // is gone). It may exceed the fresh content's max scroll (metrics update
            // after this call); every reader clamps `diff_scroll.min(max)`, so an
            // over-large value normalises on first use.
            if let Some(target) = anchor {
                let row = rows.iter().position(|r| r.target == target).unwrap_or(0);
                self.diff_scroll.set(row);
            }
            // An index-driven rebuild has no key change behind it, so nothing else
            // refreshes the scroll metrics the way a resize or a mode toggle does —
            // and the header just grew or lost a row. The event loop drains queued
            // input before redrawing, so a `j` in the same batch would otherwise
            // clamp against the pre-refresh row count and walk clean off the file.
            // Same reasoning, and same call, as `set_cross_file_scroll`.
            if previous == Some(current) {
                self.set_diff_metrics(self.diff_viewport.get(), rows.len());
            }
            *self.layout.borrow_mut() = Some(CachedLayout {
                key: current,
                first,
                rows,
            });
        }
        Ref::map(self.layout.borrow(), |cached| {
            &cached.as_ref().expect("filled above").rows
        })
    }

    /// Build the physical layout for the *active* file at pane width `width` —
    /// the selected-file path, expressed as one call through the file-parameterized
    /// seam so the selected file and a stream neighbour can never diverge.
    fn build_layout(&self, width: u16) -> Vec<LayoutRow> {
        self.build_file_layout(self.active_layout_input(), width)
    }

    /// The build inputs for the active file: its diff, its comment placements, its
    /// header rows, and the in-place editor (only ever the active file's).
    fn active_layout_input(&self) -> LayoutInput<'_> {
        let diff = self.active_diff();
        LayoutInput {
            diff,
            placements: self.file_placements(self.comment_path(), diff),
            header: self.file_header_rows(),
            editor: true,
        }
    }

    /// Build one file's physical layout at pane width `width`: the code rows for
    /// the current mode interleaved with that file's comment boxes, or (for an
    /// empty/binary/no diff) just its orphan boxes, led by its header rows. When
    /// the in-place editor is open *and* this is the active file, its box is
    /// injected too — after the anchored code line for a new comment, or in place
    /// of the edited comment's box (plan §3.5).
    ///
    /// Every input that varies per file arrives in `input`; everything read off
    /// `self` (mode, wrap, line-number toggle) is pane-global, so a neighbour's
    /// section is byte-identical to the layout that file gets when selected — the
    /// property a pixel-stable handoff needs (plan 006 §3.3).
    fn build_file_layout(&self, input: LayoutInput<'_>, width: u16) -> Vec<LayoutRow> {
        let mut rows = match input.diff {
            Some(FileDiff::Text(lines)) if !lines.is_empty() => {
                // A new-comment editor anchors after this diff-line index (re-resolved
                // from the anchor every build, never a captured row — plan §3.5).
                let editor_line = input
                    .editor
                    .then(|| self.editor_new_anchor_line(lines))
                    .flatten();
                match self.diff_mode {
                    DiffMode::Unified => {
                        self.build_unified_layout(&input, lines, width, editor_line)
                    }
                    DiffMode::SideBySide => {
                        self.build_sbs_layout(&input, lines, width, editor_line)
                    }
                }
            }
            // Empty/binary/no diff: the only selectable rows are orphan boxes,
            // rendered full-width (there are no columns to anchor them into).
            _ => {
                let mut rows = Vec::new();
                for &id in &input.placements.orphans {
                    self.push_comment_box(
                        &mut rows,
                        id,
                        true,
                        BoxPlacement::Unified(width as usize),
                        input.editor,
                    );
                }
                rows
            }
        };
        // Orphan fallback: an open editor that resolved to no row — its anchor no
        // longer maps to a diff line, or the edited comment vanished — renders as a
        // full-width block at the diff top (plan §3.5).
        if input.editor
            && self.editing()
            && !rows
                .iter()
                .any(|r| matches!(r.content, RowContent::Editor(_)))
        {
            let mut block = Vec::new();
            self.push_editor_box(&mut block, BoxPlacement::Unified(width as usize));
            block.append(&mut rows);
            rows = block;
        }
        // Ahead of the orphan block, in both modes (plan 006 §3.1).
        if !input.header.is_empty() {
            let mut header = input.header;
            header.append(&mut rows);
            rows = header;
        }
        rows
    }

    /// One file's comment placements, resolved exactly the way the active file's
    /// are: a non-empty text diff anchors boxes to lines, while an empty/binary one
    /// has no line to anchor to and shows only its orphan block. A `None` path —
    /// History's anchor, or nothing selected — has neither.
    fn file_placements(&self, path: Option<&str>, diff: Option<&FileDiff>) -> FilePlacements {
        let Some(path) = path else {
            return FilePlacements::default();
        };
        match diff {
            Some(FileDiff::Text(lines)) if !lines.is_empty() => {
                let (orphans, anchored) = comment_placements(lines, self.active_comments(), path);
                FilePlacements { orphans, anchored }
            }
            _ => FilePlacements {
                orphans: self.file_orphans(path),
                anchored: BTreeMap::new(),
            },
        }
    }

    /// The active file's header rows (plan 006 §3.1; none on History's `●` details
    /// row, which is outside the stream). Its whole payload — marker, display path,
    /// counts — is resolved once per layout build, so rendering never re-derives
    /// it. The stream's first file gets the band alone; below it the band is led by
    /// a rule row (plan 008 §3.5), which is why this reads the anchor's stream
    /// index.
    fn file_header_rows(&self) -> Vec<LayoutRow> {
        if !self.cross_file_scroll {
            return Vec::new();
        }
        let position = self.stream_position();
        let header = match self.view {
            ViewMode::Status => self
                .selected_file()
                .zip(self.active_diff())
                .map(|((section, entry), diff)| status_header(section, entry, diff)),
            ViewMode::Review => self
                .review_files()
                .get(self.review_selected())
                .map(commit_file_header),
            // Through `stream_position`, never `committed_row - 1`: the layout is
            // also built on the details row (a click hit-test does it), where the
            // subtraction would underflow (plan 009 §3.3).
            ViewMode::History => position
                .and_then(|index| self.commit_files.get(index))
                .map(commit_file_header),
        };
        header.map_or_else(Vec::new, |header| header_rows(header, position == Some(0)))
    }

    // --- The stream: identities, sections, window (plan 006 §3.3–3.4) ---

    /// How many files the current view's scroll stream holds. In History that is
    /// the selected commit's file list — the commit (`●`) details row is outside
    /// the stream (plan 009 §3.2).
    fn stream_len(&self) -> usize {
        match self.view {
            ViewMode::Status => self.status.total(),
            ViewMode::Review => self.review_files().len(),
            ViewMode::History => self.commit_files.len(),
        }
    }

    /// The anchor's index in the stream, or `None` when nothing is selected — in
    /// History, that includes the `●` details row (top-pane row 0), which has no
    /// anchor at all: no header rows, no strip, no crossing.
    fn stream_position(&self) -> Option<usize> {
        let index = match self.view {
            ViewMode::Status => self.selected,
            ViewMode::Review => self.review_selected(),
            ViewMode::History => self.committed_row.checked_sub(1)?,
        };
        (index < self.stream_len()).then_some(index)
    }

    /// The anchor's index when the pane has a *strip* to walk below it: cross-file
    /// scrolling on, and a stream the anchor sits in. The single gate both the
    /// event-path fill and the read-only assembly ask.
    fn strip_anchor(&self) -> Option<usize> {
        // Crossing and strips are both off while the in-place editor is open
        // (plan 006 §3.3): the anchor's layout carries the editor box, no section
        // ever does, and every clamp falls back to the anchor domain for the
        // duration — which is exactly what the editing wheel path expects.
        if !self.cross_file_scroll || self.editing() {
            return None;
        }
        self.stream_position()
    }

    /// The stream identity of the file at `index`.
    fn stream_file_id(&self, index: usize) -> Option<FileId> {
        match self.view {
            ViewMode::Status => {
                let (section, entry) = self.file_at_index(index)?;
                Some(FileId::Status {
                    section,
                    path: entry.path.clone(),
                })
            }
            ViewMode::Review => Some(FileId::Review {
                path: self.review_files().get(index)?.path.clone(),
            }),
            ViewMode::History => Some(FileId::History {
                commit: self.selected_commit_info()?.id,
                path: self.commit_files.get(index)?.path.clone(),
            }),
        }
    }

    /// The anchor file's stream identity.
    pub fn active_file_id(&self) -> Option<FileId> {
        self.stream_file_id(self.stream_position()?)
    }

    /// The stream index of **exactly** `id` — the reverse of `stream_file_id`,
    /// and what lets a strip hit or a cursor address (both identified by
    /// [`FileId`]) find the row they name.
    ///
    /// No staged/unstaged fallback, deliberately (plan 007 §3.3a): a path that is
    /// both staged and modified is *two* stream entries, and answering with the
    /// other one would resolve a cursor or a click against a file the user never
    /// pointed at. The fallback belongs to selection survival alone — see
    /// [`App::index_of`].
    fn stream_index_of_exact(&self, id: &FileId) -> Option<usize> {
        match id {
            FileId::Status { section, path } => self.index_of_exact(*section, path),
            FileId::Review { path } => self
                .review_files()
                .iter()
                .position(|file| file.path == *path),
            // The commit half of the identity is the History analogue of the
            // section-exact rule: a row belonging to a commit that is no longer
            // selected resolves to nothing rather than to the same path here.
            FileId::History { commit, path } => {
                if self.selected_commit_info()?.id != *commit {
                    return None;
                }
                self.commit_files.iter().position(|file| file.path == *path)
            }
        }
    }

    /// Compute one file's section: its diff plus the rows built from that diff at
    /// the current layout key. The only place a *non-selected* file's diff is read,
    /// and it runs on the event path (`ensure_diff_window`) — never during render.
    ///
    /// `index` is the file's stream position, which decides whether its header
    /// carries a rule row (plan 008 §3.5). The caller always knows it, so it is
    /// passed rather than looked up: `stream_index_of_exact` would answer the same
    /// question with a list walk, and the section is only ever read back at the
    /// index it was built for.
    fn compute_section(&self, id: &FileId, index: usize, width: u16) -> Option<FileSection> {
        let (diff, header) = match id {
            FileId::Status { section, path } => {
                let entry = self.status_entry(*section, path)?;
                let diff = self.repo.file_diff_head_vs_worktree(entry);
                let header = status_header(*section, entry, &diff);
                (diff, header)
            }
            FileId::Review { path } => {
                let review = self.review.as_ref()?;
                let file = review.files.iter().find(|file| file.path == *path)?;
                let diff = self.repo.range_file_diff(&review.spec, file);
                (diff, commit_file_header(file))
            }
            FileId::History { commit, path } => {
                let info = self.selected_commit_info()?;
                if info.id != *commit {
                    return None;
                }
                let file = self.commit_files.iter().find(|file| file.path == *path)?;
                let diff = self.repo.commit_file_diff(info, file);
                (diff, commit_file_header(file))
            }
        };
        // Counted only once the file resolved and its diff was actually read —
        // the laziness observable is "per-file diff computations" (plan §3.4).
        self.diff_compute_count
            .set(self.diff_compute_count.get() + 1);
        let input = LayoutInput {
            diff: Some(&diff),
            placements: self.file_placements(Some(id.path()), Some(&diff)),
            header: if self.cross_file_scroll {
                header_rows(header, index == 0)
            } else {
                Vec::new()
            },
            editor: false,
        };
        let rows = self.build_file_layout(input, width);
        Some(FileSection { diff, rows })
    }

    /// The working-tree entry for `path` in `section`, by identity rather than by
    /// index (a refresh can renumber the list under a queued window fill).
    fn status_entry(&self, section: Section, path: &str) -> Option<&FileEntry> {
        let list = match section {
            Section::Staged => &self.status.staged,
            Section::Unstaged => &self.status.unstaged,
        };
        list.iter().find(|entry| entry.path == path)
    }

    /// The anchor's contribution to a window `viewport` rows deep, drawn from
    /// scroll offset `offset`: the half-open span of its *own* layout rows,
    /// clamped to the rows it actually has. The one place the anchor's share of
    /// the window is decided, so the event-path fill and the render-path assembly
    /// can't disagree about where the strip starts.
    fn anchor_span(&self, width: u16, viewport: usize, offset: usize) -> Range<usize> {
        let rows = self.diff_layout(width).len();
        let offset = offset.min(rows);
        offset..offset + (rows - offset).min(viewport)
    }

    /// The stored offset read as a stream position. An offset *past* the anchor's
    /// last row is never a legal extended position — renormalization keeps
    /// `o <= R_anchor` (plan 006 §3.2a) and every flip sets an offset from a known
    /// layout — so it can only be what a shrunken relayout left behind, and it
    /// reads as the anchor-domain bottom, exactly as it did before the domain was
    /// extended. Defensive, not a protocol.
    fn stream_offset(&self, rows: usize, viewport: usize) -> usize {
        let stored = self.diff_scroll.get();
        if stored > rows {
            rows.saturating_sub(viewport)
        } else {
            stored
        }
    }

    /// The offset a frame (or a click hit-test against that frame) reads from:
    /// the stored offset normalized, then held to what the prepared stream can
    /// actually fill. Rows below the anchor's own last row belong to the strip —
    /// C3 leaves them click-inert, since every consumer resolves them against the
    /// anchor layout and finds nothing there (plan 006 §3.2e/§3.6).
    fn paint_offset(&self, rows: usize, viewport: usize) -> usize {
        self.stream_offset(rows, viewport)
            .min(self.stream_scroll_limit(rows, viewport))
    }

    /// The largest offset the extended domain allows (plan 006 §3.2a+b): the
    /// anchor's rows plus every **prepared** following section, less the viewport,
    /// so the viewport bottom can never pass the last row the stream offers. Walks
    /// prepared sections only — it never computes, which is what keeps it callable
    /// from the render path; `wheel_scroll_window` ensures first, then clamps
    /// against the filled window. With cross-file scroll off (or with no anchor,
    /// as on History's details row) the strip is empty and this *is* the
    /// anchor-content clamp.
    ///
    /// The walk stops once the result exceeds the anchor's own row count: no legal
    /// offset can reach past that (renormalization keeps `o <= R_anchor`), so
    /// every clamp site gets the same answer for a bounded amount of work.
    fn stream_scroll_limit(&self, anchor_rows: usize, viewport: usize) -> usize {
        let mut total = anchor_rows;
        if let Some(anchor) = self.strip_anchor() {
            let key = self.layout_key(self.diff_pane_width());
            let generation = self.stream_generation.get();
            let mut index = anchor + 1;
            while index < self.stream_len() && total < viewport.saturating_add(anchor_rows) {
                let Some((_, section)) = self.prepared_section(index, key, generation) else {
                    break;
                };
                total += section.rows.len();
                index += 1;
            }
        }
        total.saturating_sub(viewport)
    }

    /// [`App::stream_scroll_limit`] at the last render's metrics — the `limit` of
    /// the reader audit (plan 006 §3.2e), and the extended-domain counterpart of
    /// [`App::diff_max_scroll`].
    pub fn diff_scroll_limit(&self) -> usize {
        self.stream_scroll_limit(
            self.diff_content_rows.get(),
            self.diff_viewport.get() as usize,
        )
    }

    /// The section stream file `index` *already* has prepared, with its identity.
    /// The read-only counterpart of [`App::prepare_section`]: a miss is reported,
    /// never filled, which is what keeps every render-path reader (the window
    /// assembly, the scroll limit) free of repo reads.
    fn prepared_section(
        &self,
        index: usize,
        key: LayoutKey,
        generation: u64,
    ) -> Option<(FileId, Rc<FileSection>)> {
        let id = self.stream_file_id(index)?;
        let section = self.sections.borrow_mut().get(&id, key, generation)?;
        Some((id, section))
    }

    /// The live section for stream file `index`, computing (and caching) it on a
    /// miss. The event path's single compute seam — every laziness trigger goes
    /// through here, so `diff_compute_count` counts exactly the files the stream
    /// legitimately needed.
    fn prepare_section(&mut self, index: usize, width: u16) -> Option<Rc<FileSection>> {
        let id = self.stream_file_id(index)?;
        let key = self.layout_key(width);
        let generation = self.stream_generation.get();
        if let Some(section) = self.sections.borrow_mut().get(&id, key, generation) {
            return Some(section);
        }
        let section = Rc::new(self.compute_section(&id, index, width)?);
        self.sections
            .borrow_mut()
            .insert(id, key, generation, Rc::clone(&section));
        Some(section)
    }

    /// How many physical rows stream file `index` has. The anchor's rows are the
    /// live layout (never the cache — it is the one file whose layout can carry the
    /// in-place editor); every other file's come from its section.
    fn stream_rows(&mut self, index: usize, width: u16) -> usize {
        if Some(index) == self.stream_position() {
            return self.diff_layout(width).len();
        }
        self.prepare_section(index, width)
            .map_or(0, |section| section.rows.len())
    }

    /// How many rows a window anchored at `(index, offset)` can actually draw,
    /// capped at `viewport`, preparing the following sections it needs on the way
    /// (laziness trigger (a)). A result below `viewport` is the end-of-stream
    /// shortfall the clamp backs off by (plan 006 §3.2b).
    fn window_rows(&mut self, index: usize, offset: usize, width: u16, viewport: usize) -> usize {
        let rows = self.stream_rows(index, width);
        let mut filled = rows.saturating_sub(offset).min(viewport);
        let mut next = index + 1;
        while filled < viewport && next < self.stream_len() {
            let Some(section) = self.prepare_section(next, width) else {
                break;
            };
            filled = (filled + section.rows.len()).min(viewport);
            next += 1;
        }
        filled
    }

    /// Prepare the sections the window at the **current** scroll offset needs, on
    /// the event path — repo reads never happen during render (plan 006 §3.3).
    ///
    /// The laziness contract's trigger (a): a following file is computed only when
    /// the viewport actually reaches past the anchor's last row (`o + V > R`) and a
    /// next file exists. Scrolling anywhere inside one large file computes nothing.
    /// Triggers (b)/(c) — the renormalization an up- or fling-delta needs — belong
    /// to the scroll domain and land with it.
    ///
    /// Sections beyond what the window pins are LRU-trimmed to [`SECTION_BUDGET`],
    /// and each file's highlight sub-map goes with its section.
    pub fn ensure_diff_window(&mut self, width: u16, height: u16) {
        if self.editing() {
            // The editor collapses the strip, so nothing below the anchor is
            // addressable while it is open (plan 007 §3.3b).
            self.clear_divergent_cursor();
            return;
        }
        let Some(anchor) = self.strip_anchor() else {
            self.clear_divergent_cursor();
            return;
        };
        // What a divergent address currently resolves against, read before the
        // preparation below discards a stale entry and rebuilds it. The sweep
        // compares the two (plan 007 §3.3b's second corollary): a rebuilt section
        // can resolve the same target index against *different* content.
        let outgoing = self
            .divergent_address()
            .and_then(|address| self.sections.borrow().cached(&address.file));
        let viewport = height as usize;
        let rows = self.diff_layout(width).len();
        let offset = self.stream_offset(rows, viewport);
        let mut filled = self.anchor_span(width, viewport, offset).len();
        let mut index = anchor + 1;
        while filled < viewport && index < self.stream_len() {
            let Some(section) = self.prepare_section(index, width) else {
                break;
            };
            filled += section.rows.len();
            index += 1;
        }
        // Everything the window touches — the anchor plus the strip just walked.
        let mut pinned: Vec<FileId> = (anchor..index)
            .filter_map(|file| self.stream_file_id(file))
            .collect();
        // A divergent cursor resolves *through* its file's section, so that
        // section outranks the LRU budget for as long as the cursor names it
        // (plan 007 §3.3b). It is normally in the walk above already; pinning it
        // explicitly is what keeps the address from being invalidated by cache
        // pressure rather than by a real state change.
        if let Some(address) = self.divergent_address() {
            if !pinned.contains(&address.file) {
                pinned.push(address.file);
            }
        }
        self.sections.borrow_mut().evict(&pinned);
        self.prune_highlight_cache();
        // Last: the window the address is validated against is the one just
        // prepared and trimmed.
        self.normalize_cursor(outgoing);
    }

    /// The window to render at `width` × `height`: the anchor's rows from the
    /// current offset, then each following file's prepared section until the
    /// viewport is full or the stream ends. Read-only — a file the cache doesn't
    /// hold ends the window as a shortfall rather than computing anything, so a
    /// frame can never block on a repo read. History's `●` details row has no
    /// anchor at all, so it is always the single `id == None` segment.
    pub fn diff_window(&self, width: u16, height: u16) -> DiffWindow {
        let viewport = height as usize;
        let rows = self.diff_layout(width).len();
        // The render-time clamp of the reader audit (plan 006 §3.2e): the stored
        // offset, normalized, then held to what the *prepared* stream can fill.
        let offset = self.paint_offset(rows, viewport);
        let row_range = self.anchor_span(width, viewport, offset);
        let mut filled = row_range.len();
        // The anchor segment is always present, even when it contributes no rows
        // (`o == R`, the position pixel-identical to the next file's own row 0):
        // segment 1 is the anchor by definition.
        let mut segments = vec![WindowSegment {
            id: self.active_file_id(),
            path: self.active_path().unwrap_or_default().to_string(),
            section: None,
            row_range,
        }];
        if let Some(anchor) = self.strip_anchor() {
            let key = self.layout_key(width);
            let generation = self.stream_generation.get();
            let mut index = anchor + 1;
            while filled < viewport && index < self.stream_len() {
                let Some((id, section)) = self.prepared_section(index, key, generation) else {
                    break;
                };
                let take = section.rows.len().min(viewport - filled);
                filled += take;
                segments.push(WindowSegment {
                    path: id.path().to_string(),
                    id: Some(id),
                    section: Some(section),
                    row_range: 0..take,
                });
                index += 1;
            }
        }
        DiffWindow { segments }
    }

    /// A wheel tick in the extended stream domain (plan 006 §3.2a–b) — the wheel
    /// entry whenever cross-file scroll is on and the pane isn't editing. `delta`
    /// is a signed physical-row count, applied to what the last frame painted and
    /// settled by [`App::settle_stream_offset`].
    ///
    /// `pub` for the same reason as [`App::diff_window`]: the renormalizer's
    /// property test drives exact deltas, which a synthetic wheel event (fixed at
    /// [`SCROLL_STEP`] rows) cannot express.
    pub fn wheel_scroll_window(&mut self, delta: i64) {
        let Some(anchor) = self.strip_anchor() else {
            self.scroll_diff(delta >= 0, SCROLL_STEP);
            return;
        };
        let width = self.diff_pane_width();
        let viewport = self.diff_viewport.get() as usize;
        if viewport == 0 {
            return;
        }
        // Build the layout before reading the offset: a relayout queued earlier in
        // this batch re-anchors `diff_scroll`, and the tick must move from the
        // settled value. Starting from `paint_offset` — what the last frame drew —
        // is what makes a tick continuous with the picture on screen.
        let anchor_rows = self.diff_layout(width).len();
        let offset = self.paint_offset(anchor_rows, viewport) as i64 + delta;
        // A wheel flip carries no cursor: 006 §3.2d's contract, and the firewall
        // that keeps mouse scrolling from inheriting a walked-to address.
        self.settle_stream_offset(anchor, offset, FlipCursor::Reset);
    }

    /// Install `offset` — a signed position in the stream domain, anchored on file
    /// `anchor` — as the pane's scroll state, renormalizing and clamping it into a
    /// legal `(anchor, offset)` pair first. The shared tail of every stream-domain
    /// move: the wheel tick above and §3.3h's window-aware reveal both land here,
    /// differing only in what a flip does to the cursor.
    ///
    /// Two steps, in order: **renormalize** the offset across file boundaries
    /// (`(B, o) ≡ (A, R_A + o)`), then **clamp** by whatever shortfall filling the
    /// window from there reveals, so the viewport bottom never passes the last row
    /// the stream offers. All arithmetic is `i64`: an upward move legitimately
    /// goes negative before renormalization moves the anchor, and a `usize` would
    /// wrap.
    fn settle_stream_offset(&mut self, anchor: usize, offset: i64, cursor: FlipCursor) {
        let width = self.diff_pane_width();
        let height = self.diff_viewport.get();
        let viewport = height as usize;
        let mut index = anchor;
        let mut offset = offset;

        // Renormalize. Each step moves the anchor one file and rebases the offset
        // on that file's row count, so the *rendered* top row never moves — the
        // identity is exact. Down-renormalization stops at the last file (an offset
        // past its end is what the clamp below eats); up-renormalization triggers
        // only at `o < 0`, which is what makes `(B, 0)` a legal resting state and
        // the boundary hysteresis directional (plan 006 §3.2c).
        loop {
            let rows = self.stream_rows(index, width) as i64;
            if offset > rows && index + 1 < self.stream_len() {
                offset -= rows;
                index += 1;
                continue;
            }
            if offset < 0 && index > 0 {
                index -= 1;
                offset += self.stream_rows(index, width) as i64;
                continue;
            }
            break;
        }

        // End-of-stream clamp, discovered by filling: back the offset off by
        // however many rows the window came up short, walking back into previous
        // files when one file's own rows can't absorb it. Each pass strictly
        // lowers the top position and the stream floors at `(first file, 0)`, so
        // this terminates.
        loop {
            if offset < 0 {
                if index == 0 {
                    offset = 0;
                    break;
                }
                index -= 1;
                offset += self.stream_rows(index, width) as i64;
                continue;
            }
            let filled = self.window_rows(index, offset as usize, width, viewport);
            let short = viewport - filled;
            if short == 0 {
                break;
            }
            if index == 0 && offset == 0 {
                // The whole stream is shorter than the viewport: floor at 0.
                break;
            }
            offset -= short as i64;
        }

        let offset = offset.max(0) as usize;
        if index == anchor {
            self.diff_scroll.set(offset);
        } else if !self.flip_anchor(index, offset, cursor) {
            // The destination's section vanished between the fill loop above
            // and here — stay on the current anchor rather than half-apply.
            return;
        }
        self.ensure_diff_window(width, height);
    }

    /// Make stream file `to` the anchor at `new_offset`, seeded from its prepared
    /// section — no recompute, no scroll reset, no placement token (plan 006
    /// §3.2d). `sync_diff`/`sync_review_diff` early-return afterwards because the
    /// diff key (and, for Status, the section) already match and nothing is dirty.
    /// The border title follows the selection, so the one-row-past-the-top handoff
    /// falls out of *when* this is called; `cursor` decides what the cursor does
    /// while it happens.
    ///
    /// Returns `false` — a no-op, nothing touched — if `to` has no prepared
    /// section under the current key/generation; callers ensure first, but
    /// must still check the return: a section can vanish between "ensure" and
    /// "flip" (a queued invalidation drained in between), and installing the
    /// caller's cursor/offset against whichever file is still selected would
    /// silently resolve against the wrong layout.
    fn flip_anchor(&mut self, to: usize, new_offset: usize, cursor: FlipCursor) -> bool {
        let width = self.diff_pane_width();
        let key = self.layout_key(width);
        let generation = self.stream_generation.get();
        let Some((id, section)) = self.prepared_section(to, key, generation) else {
            return false;
        };
        // §3.3h's capture rule: read the address BEFORE the selection moves. Every
        // seam below (`set_cursor_on_anchor` for Status, `select_review_file` for
        // Review) rewrites the cursor against whichever file is selected at the
        // time, so a kept address has to be lifted out first and put back after.
        let carried = match cursor {
            FlipCursor::Keep => self.pinned_address().cloned(),
            FlipCursor::Reset => None,
        };
        // Retire the file being left into the cache *before* the selection moves,
        // so scrolling back across the boundary re-reads it instead of recomputing
        // its diff (the oscillation case of the laziness contract).
        self.retire_anchor_section(key, generation);
        match &id {
            FileId::Status { section: sec, path } => {
                self.selected = to;
                self.current_diff = Some(section.diff.clone());
                self.diff_key = Some(path.clone());
                self.diff_section = Some(*sec);
                // A pending dirty flag is satisfied by the section: every status
                // snapshot replacement bumps `stream_generation`, so a section old
                // enough to predate the flag could not have been handed out here.
                self.diff_dirty = false;
                self.set_cursor_on_anchor(None);
            }
            FileId::Review { path } => {
                self.select_review_file(to);
                let diff_key = self
                    .review
                    .as_ref()
                    .map(|review| (review.spec.base, review.spec.head, path.clone()));
                if let (Some(review), Some(diff_key)) = (self.review.as_mut(), diff_key) {
                    review.diff = Some(section.diff.clone());
                    review.diff_key = Some(diff_key);
                }
            }
            FileId::History { commit, path } => {
                // The list follows the anchor: `committed_row` is one past the
                // stream index, row 0 being the commit `●` (plan 009 §3.5). The
                // renderer re-selects `committed_state` from it every frame, so
                // the list scrolls the arriving file into view by itself.
                self.select_committed_row(to + 1);
                self.history_diff = Some(section.diff.clone());
                self.history_diff_key = Some((*commit, path.clone()));
            }
        }
        // The arriving file's rows *are* its section's (C2 builds both through one
        // seam), so the layout is installed rather than rebuilt. The generation
        // bump keeps a double-click straddling the flip inert. `first` records the
        // index the section was *built* at — `to`, which is also the index the
        // selection now sits at — so the next `diff_layout` read agrees with it
        // instead of rebuilding these rows away (plan 008 §3.5).
        *self.layout.borrow_mut() = Some(CachedLayout {
            key,
            first: to == 0,
            rows: section.rows.clone(),
        });
        self.layout_generation.set(self.layout_generation.get() + 1);
        // The h-scroll offset is kept; its read-time clamp must now measure the
        // *new* file's longest line.
        self.bump_diff_generation();
        self.diff_scroll.set(new_offset);
        // Drained-batch rule: a tick queued behind this one, drained before any
        // redraw, must clamp against the new file's bounds.
        self.set_diff_metrics(self.diff_viewport.get(), section.rows.len());
        self.prune_highlight_cache();
        if cursor == FlipCursor::Keep {
            // Put the captured address back verbatim. If it names the arriving
            // file it is simply converged now (its target came from that file's
            // rows, which are the layout just installed); if it names one further
            // down it stays divergent, and the caller's trailing
            // `ensure_diff_window` re-validates it against the window the flip
            // produced — including dropping it if the flip left it *above* the
            // anchor, where nothing renders.
            self.write_cursor(carried);
        }
        true
    }

    /// Store the current anchor's diff + built rows as its stream section, so the
    /// file a flip leaves behind stays warm. Skipped when its layout was built for
    /// a different key, or at a stream index whose header differs from the one it
    /// holds now (either way the rows would be rebuilt on the next read anyway,
    /// and caching them would hand a stale header to the strip).
    fn retire_anchor_section(&mut self, key: LayoutKey, generation: u64) {
        let Some(id) = self.active_file_id() else {
            return;
        };
        let Some(diff) = self.active_diff().cloned() else {
            return;
        };
        let first = self.stream_position() == Some(0);
        let rows = match self.layout.borrow().as_ref() {
            Some(cached) if cached.key == key && cached.first == first => cached.rows.clone(),
            _ => return,
        };
        self.sections
            .borrow_mut()
            .insert(id, key, generation, Rc::new(FileSection { diff, rows }));
    }

    /// The unified physical layout: an orphan block at the top, then each diff
    /// line followed by any comment boxes anchored to it (full-width), and the
    /// new-comment editor box after its anchor line when `editor_line` matches.
    fn build_unified_layout(
        &self,
        input: &LayoutInput<'_>,
        lines: &[DiffLine],
        width: u16,
        editor_line: Option<usize>,
    ) -> Vec<LayoutRow> {
        let place = BoxPlacement::Unified(width as usize);
        // Wrap at the same content width the renderer draws into — derived from
        // *this file's* line-number column width, so a 5+-digit number can't clip a
        // wrapped segment (they must agree; both call `line_number_width(lines)`).
        let number_width = crate::ui::diff_view::line_number_width(lines);
        let content_w = crate::ui::diff_view::unified_content_width(
            width as usize,
            self.show_line_numbers,
            number_width,
        );
        let placements = &input.placements.anchored;
        let mut rows = Vec::with_capacity(lines.len() + input.placements.orphans.len());
        for &id in &input.placements.orphans {
            self.push_comment_box(&mut rows, id, true, place, input.editor);
        }
        for (index, line) in lines.iter().enumerate() {
            // One display row per wrapped segment, all sharing `Code(index)` with
            // an incrementing subrow — so the cursor span, single-step `j`, whole-
            // unit highlight and hit-testing treat a wrapped line as one unit. With
            // wrap off (or a hunk header, which never wraps) this is a single
            // full-width segment — the same code path (plan §3.3).
            for (subrow, seg) in self.unified_line_segments(line, content_w) {
                rows.push(LayoutRow {
                    target: RowTarget::Code(index),
                    subrow,
                    side: None,
                    content: RowContent::Line { line: index, seg },
                });
            }
            // Comment boxes and the editor insert after the anchored line's *last*
            // subrow (they are appended after the whole segment run above).
            for &id in comments_after(placements, index) {
                self.push_comment_box(&mut rows, id, false, place, input.editor);
            }
            if editor_line == Some(index) {
                self.push_editor_box(&mut rows, place);
            }
        }
        rows
    }

    /// The wrapped segments of one unified diff line, paired with their subrow
    /// index. With wrap off, or for a hunk header (which never wraps), this is a
    /// single full-width segment `[0, char_count)`; with wrap on it is the display-
    /// column hard wrap of the *sanitized* text at `content_w` (plan §3.3).
    fn unified_line_segments(&self, line: &DiffLine, content_w: usize) -> Vec<(usize, Seg)> {
        // Hunk headers never wrap; every other line wraps (or a single full-width
        // segment with wrap off) through the shared `line_segments`.
        let segs = if line.kind == LineKind::Hunk {
            let count = crate::ui::diff_view::sanitize(&line.text).chars().count();
            vec![Seg::full(count)]
        } else {
            self.line_segments(&line.text, content_w)
        };
        segs.into_iter().enumerate().collect()
    }

    /// The wrapped segments of one line's sanitized text at content width
    /// `content_w`: a display-column hard wrap with wrap on, a single full-width
    /// segment `[0, char_count)` with it off — one code path for both modes
    /// (plan §3.3). Shared by the unified and side-by-side layout builders.
    ///
    /// A `content_w` of 0 (a pane so narrow the gutter/sign eats the whole cell)
    /// collapses to exactly one degenerate segment: `wrap_segments` guarantees ≥1
    /// char per segment, which would otherwise explode a non-empty line into one
    /// blank row per char while the renderer draws no content. Pinning one subrow
    /// keeps the layout's row count equal to what the renderer draws (the width
    /// the layout wraps at is the width the renderer draws at — even at 0).
    fn line_segments(&self, text: &str, content_w: usize) -> Vec<Seg> {
        let clean = crate::ui::diff_view::sanitize(text);
        if self.wrap_lines && content_w > 0 {
            crate::ui::diff_view::wrap_segments(&clean, content_w)
        } else {
            vec![Seg::full(clean.chars().count())]
        }
    }

    /// The side-by-side physical layout: the orphan block, then paired code rows,
    /// each followed by its comment boxes. A box occupies its comment's anchor-side
    /// column (old for a deletion, new for an addition/context); the other column
    /// renders as blank sibling rows so the two sides stay aligned (plan §3.4).
    fn build_sbs_layout(
        &self,
        input: &LayoutInput<'_>,
        lines: &[DiffLine],
        width: u16,
        editor_line: Option<usize>,
    ) -> Vec<LayoutRow> {
        let (left_w, right_w) = sbs_columns(width);
        let place = BoxPlacement::Sbs { left_w, right_w };
        // Per-file number-column width feeds each column's content width, so a
        // wrapped segment fits exactly where it is drawn (matches the renderer,
        // which derives the same width from the same lines).
        let number_width = crate::ui::diff_view::line_number_width(lines);
        let left_content =
            crate::ui::diff_view::sbs_content_width(left_w, self.show_line_numbers, number_width);
        let right_content =
            crate::ui::diff_view::sbs_content_width(right_w, self.show_line_numbers, number_width);
        let placements = &input.placements.anchored;
        let mut rows = Vec::new();
        for &id in &input.placements.orphans {
            // Orphan boxes have no live anchor side; keep them full-width at top.
            self.push_comment_box(
                &mut rows,
                id,
                true,
                BoxPlacement::Unified(width as usize),
                input.editor,
            );
        }
        for row in side_by_side_rows(lines) {
            match row {
                SbsCode::Hunk(i) => rows.push(LayoutRow {
                    target: RowTarget::Code(i),
                    subrow: 0,
                    side: None,
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
                            pair_emphasis(&lines[l].text, &lines[r].text).map(Rc::new)
                        }
                        _ => None,
                    };
                    // Wrap each side independently within its column; the pair
                    // occupies `max(left_rows, right_rows)` subrows, all sharing
                    // one `RowTarget` (so cursor/j/highlight/hit-testing treat the
                    // whole pair as one unit). A side with no line contributes no
                    // segments; a side whose segments run out before the taller
                    // side draws blank in its own line background (plan §3.3).
                    let left_segs = left.map(|i| self.line_segments(&lines[i].text, left_content));
                    let right_segs =
                        right.map(|i| self.line_segments(&lines[i].text, right_content));
                    let height = left_segs
                        .as_ref()
                        .map_or(0, |s| s.len())
                        .max(right_segs.as_ref().map_or(0, |s| s.len()))
                        .max(1);
                    for subrow in 0..height {
                        let cell = |idx: Option<usize>, segs: &Option<Vec<Seg>>| {
                            idx.map(|line| PairCell {
                                line,
                                seg: segs.as_ref().and_then(|s| s.get(subrow).copied()),
                            })
                        };
                        rows.push(LayoutRow {
                            target,
                            subrow,
                            side: None,
                            content: RowContent::Pair {
                                left: cell(left, &left_segs),
                                right: cell(right, &right_segs),
                                emphasis: emphasis.clone(),
                            },
                        });
                    }
                    // Old-side comments (on the left index) emit before new-side
                    // (on the right), matching `ordered_comment_ids`. A context
                    // Pair is one diff line on both sides (left == right), so emit
                    // its comments once (they carry the new side themselves).
                    if let Some(l) = left {
                        for &id in comments_after(placements, l) {
                            self.push_comment_box(&mut rows, id, false, place, input.editor);
                        }
                    }
                    if let Some(r) = right {
                        if Some(r) != left {
                            for &id in comments_after(placements, r) {
                                self.push_comment_box(&mut rows, id, false, place, input.editor);
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
        editor: bool,
    ) {
        let Some(comment) = self.active_comment(id) else {
            return;
        };
        // Editing this comment? Its box becomes the in-place editor, rendered where
        // the saved box would have been (plan §3.5) — never in a non-active file's
        // section, which carries no editor at all (plan 006 §3.3).
        if editor && self.editor_edits_comment(id) {
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
                subrow,
                side,
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
                subrow,
                side,
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
    fn comment_path(&self) -> Option<&str> {
        match self.view {
            ViewMode::History => None,
            _ => self.active_path(),
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

    /// The orphaned comment ids on `file`, ordered by id — the block any file's
    /// layout leads with, selected or not. For a file whose diff is empty or binary
    /// (no lines to anchor to) it is the *only* place they can appear, so the block
    /// renders regardless of diff kind (finding 2). Always empty outside an active
    /// review session in the Review view.
    fn file_orphans(&self, file: &str) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .active_comments()
            .iter()
            .filter(|c| c.orphaned && c.file == file)
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
        self.active_path().map(str::to_string)
    }

    /// The path backing `active_diff`, borrowed — the allocation-free form the
    /// per-line highlight lookup needs (`active_diff_path` clones for callers that
    /// keep it past a mutation).
    pub fn active_path(&self) -> Option<&str> {
        match self.view {
            ViewMode::Status => self.selected_file().map(|(_, entry)| entry.path.as_str()),
            ViewMode::History => self
                .commit_files
                .get(self.committed_row.checked_sub(1)?)
                .map(|file| file.path.as_str()),
            ViewMode::Review => self
                .review
                .as_ref()
                .and_then(|review| review.files.get(review.selected))
                .map(|file| file.path.as_str()),
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

    /// Count of per-file diff computations so far, in all three views. A test-only
    /// observable proving a cross-file crossing computes exactly the destination
    /// file's diff — nothing eager (plan §3.4 laziness).
    #[doc(hidden)]
    pub fn diff_compute_count(&self) -> u64 {
        self.diff_compute_count.get()
    }

    /// How many prepared sections the stream cache currently holds (live or
    /// stale-tagged — staleness is resolved on access). A test-only observable for
    /// the window-pinned capacity rule (plan 006 §3.3).
    #[doc(hidden)]
    pub fn cached_section_count(&self) -> usize {
        self.sections.borrow().entries.len()
    }

    /// The stream invalidation counter (plan 006 §3.3). A test-only observable
    /// proving a refresh / comment mutation retires every cached section.
    #[doc(hidden)]
    pub fn stream_generation(&self) -> u64 {
        self.stream_generation.get()
    }

    /// How many files the active view's scroll stream holds. A test-only
    /// observable (the window walks this list).
    #[doc(hidden)]
    pub fn stream_file_count(&self) -> usize {
        self.stream_len()
    }

    /// How many times the physical diff-pane layout has actually been
    /// rebuilt (`App::diff_layout`'s stale branch), as opposed to reused from
    /// cache. A test-only observable proving a repeated render at unchanged
    /// geometry is a cache hit, and that a width / wrap / line-numbers change
    /// bumps it by exactly one (plan §3.7).
    #[doc(hidden)]
    pub fn layout_generation(&self) -> u64 {
        self.layout_generation.get()
    }

    /// How many times [`App::active_max_line_width`] recomputed the longest
    /// code-line memo, as opposed to returning the cached value. A test-only
    /// observable proving the per-diff scan runs once per `(diff_generation,
    /// view)`, not once per horizontal scroll (plan §3.7).
    #[doc(hidden)]
    pub fn max_line_width_compute_count(&self) -> u64 {
        self.max_line_width_compute_count.get()
    }

    /// The number of physical rows the active diff's layout renders (needs a
    /// prior render so the pane width is known). A test-only observable so an
    /// up-crossing's landing can be pinned to the previous file's last row.
    #[doc(hidden)]
    pub fn diff_row_count(&self) -> usize {
        self.review_row_count()
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
    /// the active view's pane. C8 hit-tests a click against these to delete a note;
    /// History draws no boxes, so its map stays empty.
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

    /// Record this frame's window hit map (plan 006 §3.6): one [`WindowHit`]
    /// per drawn row, top to bottom — the same order `out` is built in, so
    /// index `k` here is screen row `diff_area.y + k`. Mirrors `set_x_rects`,
    /// plus the [`WindowEpoch`] snapshot `window_hit_at` validates a later
    /// lookup against — captured here, in the same render pass that just
    /// built `hits`, so it's exactly the state the rows describe.
    pub(crate) fn set_window_hits(&self, hits: Vec<WindowHit>) {
        *self.window_hits.borrow_mut() = WindowHitMap {
            epoch: Some(self.window_epoch()),
            rows: hits,
        };
    }

    /// The state signature a window hit map is valid for right now — see
    /// [`WindowEpoch`]. Read both when recording (during render) and when
    /// looking up (at click time); any field drifting between the two calls
    /// means the map predates something that happened since.
    fn window_epoch(&self) -> WindowEpoch {
        WindowEpoch {
            layout_generation: self.layout_generation.get(),
            stream_generation: self.stream_generation.get(),
            view: self.view,
            offset: self.diff_scroll.get(),
            diff_area: self.diff_area.get(),
        }
    }

    /// The window hit map entry for a screen position, or `None` outside the
    /// diff pane, past the last row the window drew (the shortfall region), or
    /// when the map predates a state change since its render — a click drained
    /// after a flip/scroll/relayout/relist/view-change in the same input batch
    /// falls through to the always-safe anchor-only path instead of acting on
    /// stale row associations (plan 006 §3.6, correctness review finding 1).
    fn window_hit_at(&self, pos: Position) -> Option<WindowHit> {
        let diff = self.diff_area.get();
        if !diff.contains(pos) {
            return None;
        }
        let map = self.window_hits.borrow();
        if map.epoch != Some(self.window_epoch()) {
            return None;
        }
        let row = (pos.y - diff.y) as usize;
        map.rows.get(row).cloned()
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
        self.diff_scroll.set(0);
        // A mode change re-lays out both columns from scratch; start unshifted
        // (the two modes have different content widths — plan §3.5).
        self.diff_hscroll = 0;
        // A mode change relayouts the whole diff; drop any half-formed double-click
        // so a click before it can't pair with one after (plan §3.6).
        self.last_click = None;
        // The two modes have different row lists (a `Code` index means a
        // different physical row), so the cursor doesn't carry over — reset to
        // the top (plan §3.4).
        self.set_cursor_on_anchor(None);
        self.reprepare_diff_window();
    }

    /// Flip the line-number gutter on/off. The gutter width feeds the content
    /// width a line wraps at, so the `layout` cache tracks `show_line_numbers`
    /// and rebuilds here (see [`App::diff_layout`]); that rebuild re-anchors the
    /// top visible logical line, so the view doesn't jump on `n` while wrap is on.
    /// The `highlight_cache`'s spans cover the full line and are windowed at
    /// render time, so it never needs invalidating.
    fn toggle_line_numbers(&mut self) {
        self.show_line_numbers = !self.show_line_numbers;
        // A layout-key change rebuilds every section, so a divergent address
        // can't be carried across it (plan 007 §3.3b).
        self.clear_divergent_cursor();
        self.reprepare_diff_window();
    }

    /// Flip hard line wrapping on/off. Wrap is a physical-layout input, so the
    /// next `diff_layout` rebuilds the row list and re-anchors the top visible
    /// logical line (plan §3.3). The cursor addresses a logical target and so
    /// survives untouched (unlike `toggle_diff_mode`, whose row indices change
    /// meaning).
    fn toggle_wrap(&mut self) {
        self.wrap_lines = !self.wrap_lines;
        // Enabling wrap resets the horizontal offset — the two are mutually
        // exclusive, and h-scroll is ignored while wrap is on (plan §3.5).
        if self.wrap_lines {
            self.diff_hscroll = 0;
        }
        // Wrap is a layout-key input: every section is rebuilt, so a divergent
        // address doesn't survive the toggle (plan 007 §3.3b).
        self.clear_divergent_cursor();
        self.reprepare_diff_window();
    }

    /// Flip cross-file scroll on/off — the single seam both the `f` key and the
    /// View-menu item go through, because turning it *off* retires the file header
    /// and two things depend on those rows existing:
    ///
    /// - a cursor pinned to `RowTarget::FileHeader` no longer resolves, and the
    ///   `(0, 1)` span fallback would make the next `j` skip physical row 0;
    /// - the row count changes by the header's height — one row for the stream's
    ///   first file, two for any other (plan 008 §3.5) — so the scroll metrics a
    ///   same-batch wheel tick or click clamps against are stale until the next
    ///   render (the same drained-batch rule [`App::flip_anchor`] follows).
    fn set_cross_file_scroll(&mut self, on: bool) {
        self.cross_file_scroll = on;
        if !on && self.pinned_anchor_target() == Some(RowTarget::FileHeader) {
            self.set_cursor_on_anchor(None);
        }
        // Turning the mode off retires the strip a divergent cursor lives on;
        // turning it on rebuilds every section under a new layout key. Either way
        // the address can't carry over (plan 007 §3.3b).
        self.clear_divergent_cursor();
        let width = self.diff_pane_width();
        let count = self.diff_layout(width).len();
        self.set_diff_metrics(self.diff_viewport.get(), count);
        if !on {
            // Turning the mode off retires the extended domain. The frame paints
            // the anchor's own bottom from here on, so *store* that: an extended
            // offset left behind would be resurrected — jumping back to a boundary
            // the user last saw before the toggle — the moment the mode is turned
            // on again (plan 006 §3.2e, the `max` clamp).
            let max = self.diff_max_scroll();
            if self.diff_scroll.get() > max {
                self.diff_scroll.set(max);
            }
        }
        self.reprepare_diff_window();
    }

    /// Advance to the next theme in `Theme::available` (presets then user themes),
    /// wrapping around. The available set is enumerated fresh here so a theme file
    /// added or removed since startup is honoured; a `theme_name` no longer in the
    /// set (its file was deleted) restarts at index 0. The highlight cache is keyed
    /// by line text only — its cached colours belong to the old theme — so it is
    /// cleared here, the verified staleness hazard. The flash shows the *resolved*
    /// canonical name, so it can never diverge from the theme now on screen.
    fn cycle_theme(&mut self) {
        // `cycle` picks the next name (resolving to confirm it loads); the shared
        // tail installs it. The resolved theme is rebuilt in `set_theme_by_name`
        // — a cheap second resolve that keeps the install path single-sourced.
        let (name, _) = Theme::cycle(&self.theme_name, self.config_dir.as_deref());
        self.set_theme_by_name(&name);
    }

    /// Resolve `name` against the config dir, install it as the active theme,
    /// clear the (now-stale) highlight cache, and flash the resolved canonical
    /// name. The shared install tail of `cycle_theme` and the Theme menu's
    /// `SetTheme` activation, so the two can't diverge. The flash shows the
    /// resolved name, never the requested one, so it can't name a theme other
    /// than the one on screen.
    fn set_theme_by_name(&mut self, name: &str) {
        let (name, theme) = Theme::resolve(name, self.config_dir.as_deref());
        self.theme = theme;
        self.theme_name = name.clone();
        self.highlight_cache.borrow_mut().clear();
        self.flash = Some(Flash::info(name));
    }

    // --- Header menu bar dropdowns (issue #5, plan §3.0) ----------------

    /// The rows of `menu`, built fresh from live state so every marker reflects
    /// the current setting. Shared by the dropdown renderer *and* by nav /
    /// activation, so the drawn menu and what a click does never drift.
    pub(crate) fn menu_items(&self, menu: MenuId) -> Vec<MenuRow> {
        match menu {
            MenuId::View => {
                let home = self.home_view();
                // Dynamic Home label: from a review session the first row reads
                // "Review" (checked when home), not a dead "Status" control.
                let home_label = if home == ViewMode::Review {
                    "Review"
                } else {
                    "Status"
                };
                vec![
                    MenuRow::Item {
                        label: "Unified".to_string(),
                        marker: Marker::Radio(self.diff_mode == DiffMode::Unified),
                        hint: Some("d"),
                        command: MenuCommand::SetDiffMode(DiffMode::Unified),
                    },
                    MenuRow::Item {
                        label: "Side by side".to_string(),
                        marker: Marker::Radio(self.diff_mode == DiffMode::SideBySide),
                        hint: None,
                        command: MenuCommand::SetDiffMode(DiffMode::SideBySide),
                    },
                    MenuRow::Separator,
                    MenuRow::Item {
                        label: "Line numbers".to_string(),
                        marker: Marker::Check(self.show_line_numbers),
                        hint: Some("n"),
                        command: MenuCommand::SetLineNumbers(!self.show_line_numbers),
                    },
                    MenuRow::Item {
                        label: "Wrap lines".to_string(),
                        marker: Marker::Check(self.wrap_lines),
                        hint: Some("w"),
                        command: MenuCommand::SetWrap(!self.wrap_lines),
                    },
                    MenuRow::Item {
                        label: "Cross-file scroll".to_string(),
                        marker: Marker::Check(self.cross_file_scroll),
                        hint: Some("f"),
                        command: MenuCommand::SetCrossFileScroll(!self.cross_file_scroll),
                    },
                    MenuRow::Separator,
                    MenuRow::Item {
                        label: "Changes panel".to_string(),
                        marker: Marker::Check(self.show_changes),
                        hint: Some("b"),
                        command: MenuCommand::ToggleChangesPanel,
                    },
                    MenuRow::Separator,
                    MenuRow::Item {
                        label: home_label.to_string(),
                        marker: Marker::Radio(self.view == home),
                        hint: Some("1"),
                        command: MenuCommand::GoHome,
                    },
                    MenuRow::Item {
                        label: "History".to_string(),
                        marker: Marker::Radio(self.view == ViewMode::History),
                        hint: Some("2"),
                        command: MenuCommand::EnterHistory,
                    },
                ]
            }
            MenuId::Theme => Theme::available(self.config_dir.as_deref())
                .into_iter()
                .map(|name| MenuRow::Item {
                    marker: Marker::Radio(name == self.theme_name),
                    label: name.clone(),
                    hint: None,
                    command: MenuCommand::SetTheme(name),
                })
                .collect(),
        }
    }

    /// The full-list index of `menu`'s first activatable row — a menu's opening
    /// highlight. Menus always have ≥1 item, so the `0` fallback is defensive.
    fn menu_first_selectable(&self, menu: MenuId) -> usize {
        self.menu_items(menu)
            .iter()
            .position(|row| matches!(row, MenuRow::Item { .. }))
            .unwrap_or(0)
    }

    /// Open `menu` with its first activatable row highlighted — the shared
    /// "reveal this dropdown" step behind a title click, a hover-slide, and a
    /// keyboard menu switch.
    fn open_menu_first(&mut self, menu: MenuId) {
        let item = self.menu_first_selectable(menu);
        self.open_menu = Some(OpenMenu { menu, item });
    }

    /// The full-list index one step from `item` in `menu`, skipping separators
    /// and wrapping. Clamped against a freshly built list so a shrunk menu can't
    /// index past the end.
    fn menu_step(&self, menu: MenuId, item: usize, down: bool) -> usize {
        let rows = self.menu_items(menu);
        let n = rows.len();
        if n == 0 {
            return 0;
        }
        let mut idx = item.min(n - 1);
        for _ in 0..n {
            idx = if down {
                (idx + 1) % n
            } else {
                (idx + n - 1) % n
            };
            if matches!(rows[idx], MenuRow::Item { .. }) {
                break;
            }
        }
        idx
    }

    /// Route a key while a dropdown is open (plan §3.0), run before the keymap.
    /// Left/Right/Tab switch menus, Up/Down move (skipping separators, wrapping),
    /// Enter/Space activate then close. Esc, a `ToggleMenuBar` chord, and every
    /// other key all just close the dropdown (consumed, never re-dispatched) — so
    /// no keymap-action lookup is needed to honour a remapped toggle chord.
    fn on_key_menu(&mut self, key: KeyEvent) {
        let Some(open) = self.open_menu else {
            return;
        };
        match key.code {
            KeyCode::Left | KeyCode::BackTab => self.open_menu_sibling(false),
            KeyCode::Right | KeyCode::Tab => self.open_menu_sibling(true),
            KeyCode::Up | KeyCode::Down => {
                let item = self.menu_step(open.menu, open.item, key.code == KeyCode::Down);
                self.open_menu = Some(OpenMenu {
                    menu: open.menu,
                    item,
                });
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.activate_menu_item(open.menu, open.item);
                self.open_menu = None;
            }
            _ => self.open_menu = None,
        }
    }

    /// Switch the open dropdown to the previous/next top-level menu, highlighting
    /// its first activatable row (Left/Right/Tab while open, and hover-slide).
    fn open_menu_sibling(&mut self, next: bool) {
        let Some(open) = self.open_menu else {
            return;
        };
        let menus = crate::ui::menu::MENUS;
        let n = menus.len();
        let Some(pos) = menus.iter().position(|&m| m == open.menu) else {
            return;
        };
        let target = if next {
            (pos + 1) % n
        } else {
            (pos + n - 1) % n
        };
        self.open_menu_first(menus[target]);
    }

    /// Activate the row at full-list index `item` in `menu` (a no-op on a
    /// separator or an out-of-range index — clamped against a fresh list).
    fn activate_menu_item(&mut self, menu: MenuId, item: usize) {
        let command = match self.menu_items(menu).into_iter().nth(item) {
            Some(MenuRow::Item { command, .. }) => command,
            _ => return,
        };
        self.activate_command(command);
    }

    /// Apply a menu command, reusing the shipped mutate+persist pairs. Settings
    /// persist (theme / diff-mode / line-numbers); view changes don't, matching
    /// the keyboard actions. A setting already at the chosen value is a no-op.
    fn activate_command(&mut self, command: MenuCommand) {
        match command {
            MenuCommand::SetDiffMode(mode) => {
                if self.diff_mode != mode {
                    self.toggle_diff_mode();
                    self.persist_setting(Setting::DiffMode(self.diff_mode));
                }
            }
            MenuCommand::SetLineNumbers(on) => {
                if self.show_line_numbers != on {
                    self.toggle_line_numbers();
                    self.persist_setting(Setting::LineNumbers(self.show_line_numbers));
                }
            }
            MenuCommand::SetWrap(on) => {
                if self.wrap_lines != on {
                    self.toggle_wrap();
                    self.persist_setting(Setting::WrapLines(self.wrap_lines));
                }
            }
            MenuCommand::SetCrossFileScroll(on) => {
                if self.cross_file_scroll != on {
                    self.set_cross_file_scroll(on);
                    self.persist_setting(Setting::CrossFileScroll(self.cross_file_scroll));
                }
            }
            MenuCommand::SetTheme(name) => {
                // No-op when already active (matching the diff-mode / line-numbers
                // guards): re-installing would needlessly persist, and re-resolving
                // a since-deleted custom theme would silently fall back + persist
                // the fallback over the user's choice.
                if name != self.theme_name {
                    self.set_theme_by_name(&name);
                    self.persist_setting(Setting::Theme(self.theme_name.clone()));
                }
            }
            MenuCommand::GoHome => self.go_home(),
            MenuCommand::EnterHistory => {
                if self.view != ViewMode::History {
                    self.enter_history();
                }
            }
            // Session-only, like the `b` key it mirrors: no persisted setting.
            MenuCommand::ToggleChangesPanel => self.toggle_changes_panel(),
        }
    }

    /// The top-level menu whose recorded title rect contains `pos`, if any.
    fn menu_title_at(&self, pos: Position) -> Option<MenuId> {
        self.menu_title_rects
            .borrow()
            .iter()
            .find(|(_, rect)| rect.contains(pos))
            .map(|(id, _)| *id)
    }

    /// Handle a left-click against the menu bar. Returns `true` when the click
    /// was fully consumed (no further routing): a title toggle, or a click inside
    /// the open dropdown. Returns `false` to fall through to normal routing —
    /// either no menu was involved, or a click-away that closed an open dropdown
    /// (which still routes to its target, mirroring the editor's click-outside).
    fn menu_click(&mut self, pos: Position) -> bool {
        // A hidden bar records no title rects (cleared on hide), but guard here
        // too so a rect stale between the hide and the next redraw can't match.
        if !self.show_menu_bar {
            return false;
        }
        // A click on a top-level title toggles its dropdown.
        if let Some(menu) = self.menu_title_at(pos) {
            match self.open_menu {
                Some(open) if open.menu == menu => self.open_menu = None,
                _ => self.open_menu_first(menu),
            }
            return true;
        }
        // Consult the last-drawn dropdown — what is actually on screen. A click
        // inside its bounds is consumed even if a queued hover/Esc already changed
        // `open_menu` before this frame was redrawn (input drains before redraw),
        // so it can't fall through to the body under the visible overlay. A row is
        // activated only when the recorded box still matches the open menu (a fresh
        // frame); a stale box just consumes and closes. `fresh` distinguishes them;
        // the inner `Option` is the clicked row's command (`None` = separator/border).
        let inside = {
            let hit = self.menu_dropdown.borrow();
            hit.as_ref().and_then(|d| {
                d.bounds.contains(pos).then(|| {
                    let fresh = self.open_menu.map(|open| open.menu) == Some(d.menu);
                    let cmd = d
                        .rows
                        .iter()
                        .find(|(_, rect)| rect.contains(pos))
                        .and_then(|(cmd, _)| cmd.clone());
                    (fresh, cmd)
                })
            })
        };
        match inside {
            Some((true, Some(command))) => {
                // Fresh box, activatable row: act then close.
                self.activate_command(command);
                self.open_menu = None;
                true
            }
            // Fresh box, separator/border: a no-op that stays open, still consumed.
            Some((true, None)) => true,
            // Stale box still on screen: consume + close, but don't act on rows we
            // can no longer trust.
            Some((false, _)) => {
                self.open_menu = None;
                true
            }
            None => {
                // Not inside any drawn box: close an open menu, then route the click.
                self.open_menu = None;
                false
            }
        }
    }

    /// Slide an already-open dropdown to follow the mouse (plan §3.0): over
    /// another title it switches the open menu; over an activatable row it moves
    /// the highlight. A no-op (and no open) when no menu is open — hover never
    /// opens a dropdown. Returns `true` when the open menu changed.
    fn menu_hover(&mut self, pos: Position) -> bool {
        let Some(open) = self.open_menu else {
            return false;
        };
        if let Some(menu) = self.menu_title_at(pos) {
            if menu != open.menu {
                self.open_menu_first(menu);
                return true;
            }
            return false;
        }
        let new_item = {
            let hit = self.menu_dropdown.borrow();
            hit.as_ref().and_then(|d| {
                if d.menu != open.menu || !d.bounds.contains(pos) {
                    return None;
                }
                // Only activatable rows take the highlight (skip separators), and
                // map the visible position back to its full-list index.
                d.rows.iter().enumerate().find_map(|(vis, (cmd, rect))| {
                    (cmd.is_some() && rect.contains(pos)).then_some(d.window_start + vis)
                })
            })
        };
        if let Some(item) = new_item {
            if item != open.item {
                self.open_menu = Some(OpenMenu {
                    menu: open.menu,
                    item,
                });
                return true;
            }
        }
        false
    }

    /// Record the top-level title rects for this frame (called from the header
    /// renderer). Cleared to empty when the bar is hidden.
    pub(crate) fn set_menu_title_rects(&self, rects: Vec<(MenuId, Rect)>) {
        *self.menu_title_rects.borrow_mut() = rects;
    }

    /// The recorded title rect of `menu`, for anchoring its dropdown.
    pub(crate) fn menu_title_rect(&self, menu: MenuId) -> Option<Rect> {
        self.menu_title_rects
            .borrow()
            .iter()
            .find(|(id, _)| *id == menu)
            .map(|(_, rect)| *rect)
    }

    /// Record the open dropdown's hit-map for this frame (called from the overlay
    /// renderer), or clear it when no menu is open.
    pub(crate) fn set_menu_dropdown(&self, hit: Option<DropdownHit>) {
        *self.menu_dropdown.borrow_mut() = hit;
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

/// The `[start, end)` run of `rows` sharing `target`: its first row through the
/// last consecutive row with the same target (a code line is one row, a comment
/// box is N). File-agnostic, so the anchor's live layout and any prepared
/// section resolve a target the same way (plan 007 §3.3j).
fn target_span(rows: &[LayoutRow], target: RowTarget) -> Option<Range<usize>> {
    let start = rows.iter().position(|row| row.target == target)?;
    let len = rows[start..]
        .iter()
        .take_while(|row| row.target == target)
        .count();
    Some(start..start + len)
}

/// The comment a [`RowTarget`] names, if it names one at all. File-agnostic, so
/// the anchor-domain [`App::cursor_comment_id`] and its address-aware
/// counterpart share one row-kind test.
fn target_comment_id(target: RowTarget) -> Option<u64> {
    match target {
        RowTarget::Comment(id) | RowTarget::Orphan(id) => Some(id),
        RowTarget::Code(_) | RowTarget::Editor | RowTarget::FileHeader => None,
    }
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

/// Split a file's list label into the dim prefix and the basename the chip
/// carries (plan 008 §3.2), from the `path`/`orig_path` fields rather than by
/// re-parsing a formatted label. `prefix + name` is exactly what `display_path`
/// returns, which is what keeps the band's label identical in content to the
/// file lists.
fn split_label(path: &str, orig_path: Option<&str>) -> (String, String) {
    // `git status` collapses a wholly-untracked directory to a `dir/` entry, which
    // has no basename to chip — the whole label goes on the chip instead.
    let (dir, name) = match path.rsplit_once('/') {
        Some((dir, name)) if !name.is_empty() => (format!("{dir}/"), name.to_string()),
        _ => (String::new(), path.to_string()),
    };
    match orig_path {
        Some(orig) => (format!("{orig} → {dir}"), name),
        None => (dir, name),
    }
}

/// The header payload for a working-tree file: its section-aware marker and tone,
/// its split display path (`old → new` for a rename), and the counts recounted
/// off the diff — a `FileEntry` carries no stats of its own (plan 006 §3.1).
fn status_header(section: Section, entry: &FileEntry, diff: &FileDiff) -> FileHeaderRow {
    let (prefix, name) = split_label(&entry.path, entry.orig_path.as_deref());
    FileHeaderRow {
        marker: entry.change.marker(),
        tone: MarkerTone::for_status(section, entry.change),
        part: HeaderPart::Band,
        prefix,
        name,
        stat: crate::git::diff::stat_of(diff),
    }
}

/// The header payload for a committed file — a reviewed range's or a single
/// commit's — whose `+a −d` come from git's numstat rather than a recount.
fn commit_file_header(file: &CommitFile) -> FileHeaderRow {
    let (prefix, name) = split_label(&file.path, file.orig_path.as_deref());
    FileHeaderRow {
        marker: file.change.marker(),
        tone: MarkerTone::for_change_kind(file.change),
        part: HeaderPart::Band,
        prefix,
        name,
        stat: file.stat,
    }
}

/// Wrap a header payload as the physical rows that lead a file's layout: the band
/// alone for the stream's `first` file, a separating rule above it for every file
/// below (plan 008 §3.5). Both rows share one [`RowTarget::FileHeader`] with an
/// incrementing `subrow`, so the header stays a single cursor stop.
fn header_rows(header: FileHeaderRow, first: bool) -> Vec<LayoutRow> {
    let row = |subrow: usize, part: HeaderPart| LayoutRow {
        target: RowTarget::FileHeader,
        subrow,
        side: None,
        content: RowContent::FileHeader(FileHeaderRow {
            part,
            ..header.clone()
        }),
    };
    if first {
        vec![row(0, HeaderPart::Band)]
    } else {
        vec![row(0, HeaderPart::Rule), row(1, HeaderPart::Band)]
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
/// (the same sanitization the renderer applies before slicing, so these ranges
/// line up with the rendered tokens on every subrow). Computed once when the
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
/// sides exactly as the renderer does (so char offsets align), then
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
