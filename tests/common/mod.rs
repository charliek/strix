// Shared test helpers. Not every test binary uses every helper, so silence the
// dead-code lint that would otherwise fire per-crate.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use strix::app::{App, DiffWindow, FileId, RowTarget};
use strix::comments::{Branch, Comment, Store};
use strix::config::Config;
use strix::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use strix::git::Section;
use strix::terminal::dump_frame;
use tempfile::TempDir;

/// Press a plain character key on `app`, as if typed at the keyboard.
pub fn press(app: &mut App, ch: char) {
    app.on_key(KeyEvent::from(KeyCode::Char(ch)));
}

// --- Key / mouse event builders ---------------------------------------------
// `key` builds the event without applying it (vs [`press`], which applies).

pub fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

pub fn enter() -> KeyEvent {
    KeyEvent::from(KeyCode::Enter)
}

pub fn esc() -> KeyEvent {
    KeyEvent::from(KeyCode::Esc)
}

pub fn tab() -> KeyEvent {
    KeyEvent::from(KeyCode::Tab)
}

pub fn mouse(col: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

pub fn click(col: u16, row: u16) -> MouseEvent {
    mouse(col, row, MouseEventKind::Down(MouseButton::Left))
}

pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// --- Frame rendering ---------------------------------------------------------

/// Render one frame to text at `w`×`h`. Suites with a fixed viewport keep a
/// local `fn dump(app: &App) -> String` that calls this with their own `W`/`H`.
pub fn dump(app: &App, w: u16, h: u16) -> String {
    dump_frame(app, w, h).unwrap()
}

/// The 0-indexed row in `frame` (as produced by [`dump`]) containing `needle`,
/// or panics with the frame content if not found.
pub fn row_of(frame: &str, needle: &str) -> usize {
    frame
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("frame missing {needle:?}:\n{frame}"))
}

// --- Selection polling --------------------------------------------------------

/// Press `j` up to `max_tries` times until `app`'s active diff path is `path`,
/// or panic. Suites keep a local zero-arg `select_file`/`select_status_file`
/// wrapper pinning their own `W`/`H`/retry count.
pub fn select_file(app: &mut App, path: &str, w: u16, h: u16, max_tries: u32) {
    let _ = dump(app, w, h);
    for _ in 0..max_tries {
        if app.active_diff_path().as_deref() == Some(path) {
            return;
        }
        app.on_key(key('j'));
    }
    panic!("{path} never became the selected file");
}

/// As [`select_file`], for suites whose failure message calls out the status
/// (working-tree) list rather than a review file list.
pub fn select_status_file(app: &mut App, path: &str, w: u16, h: u16, max_tries: u32) {
    let _ = dump(app, w, h);
    for _ in 0..max_tries {
        if app.active_diff_path().as_deref() == Some(path) {
            return;
        }
        app.on_key(key('j'));
    }
    panic!("{path} never became the selected status file");
}

// --- Review-mode app construction --------------------------------------------

/// A review-mode `App` over `repo` at `range`, default config.
pub fn review(repo: &Path, range: &str) -> App {
    App::for_review(repo.to_path_buf(), &Config::default(), range).unwrap()
}

/// As [`review`], also returning the backing repo (some suites build the repo
/// inline rather than taking it as a parameter).
pub fn review_app(range: &str) -> (TempDir, App) {
    let repo = init_repo_with_diverged_branches();
    let app = App::for_review(repo.path().to_path_buf(), &Config::default(), range).unwrap();
    (repo, app)
}

// --- Comment store I/O --------------------------------------------------------

/// The `strix` comments directory under `repo`'s `.git`.
pub fn strix_dir(repo: &Path) -> PathBuf {
    repo.join(".git").join("strix")
}

/// The raw JSON text of `repo`'s comment store.
pub fn store_text(repo: &Path) -> String {
    std::fs::read_to_string(strix_dir(repo).join("comments.json")).unwrap()
}

/// Write a comment store for `repo` with a single branch entry (`branch`,
/// `range`, `comments`), `next_id` 1000.
pub fn seed_store(repo: &Path, branch: &str, range: Option<&str>, comments: Vec<Comment>) {
    let mut branches = BTreeMap::new();
    branches.insert(
        branch.to_string(),
        Branch {
            active_range: range.map(str::to_string),
            comments,
        },
    );
    let store = Store {
        version: 2,
        next_id: 1000,
        branches,
    };
    let dir = strix_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    let json = serde_json::to_string_pretty(&store).unwrap();
    std::fs::write(dir.join("comments.json"), json).unwrap();
}

// --- Styled-cell assertions -------------------------------------------------
//
// `dump_frame` serialises only the glyphs, dropping every colour, so a test that
// needs to check a border colour, a selection highlight, or the stale-dim accent
// has to reach into the rendered `Buffer` and read per-cell styles. These shared
// helpers do that against the same `TestBackend` render path `dump_frame` uses.

/// Render one frame to a `Buffer` for per-cell style assertions.
pub fn render_buffer(app: &App, width: u16, height: u16) -> Buffer {
    strix::terminal::render_to_buffer(app, width, height).unwrap()
}

/// The glyph at cell `(x, y)`, or `""` when out of bounds.
pub fn cell_symbol(buf: &Buffer, x: u16, y: u16) -> String {
    buf.cell((x, y))
        .map(|c| c.symbol().to_string())
        .unwrap_or_default()
}

/// The foreground colour at cell `(x, y)`, if any.
pub fn cell_fg(buf: &Buffer, x: u16, y: u16) -> Option<Color> {
    buf.cell((x, y)).map(|c| c.fg)
}

/// The background colour at cell `(x, y)`, if any.
pub fn cell_bg(buf: &Buffer, x: u16, y: u16) -> Option<Color> {
    buf.cell((x, y)).map(|c| c.bg)
}

/// Whether any cell in buffer row `y` carries foreground colour `fg`.
pub fn row_has_fg(buf: &Buffer, y: u16, fg: Color) -> bool {
    let area = buf.area;
    (area.x..area.x + area.width).any(|x| buf.cell((x, y)).map(|c| c.fg) == Some(fg))
}

/// Whether any cell in buffer row `y` carries background colour `bg`.
pub fn row_has_bg(buf: &Buffer, y: u16, bg: Color) -> bool {
    let area = buf.area;
    (area.x..area.x + area.width).any(|x| buf.cell((x, y)).map(|c| c.bg) == Some(bg))
}

/// Run a git command in `dir`, asserting success.
pub fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// Write a file (creating parent directories) inside `dir`.
pub fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// Run a git command in `dir` with extra environment (e.g. fixed commit dates),
/// asserting success.
pub fn git_env(dir: &Path, envs: &[(&str, &str)], args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let status = cmd.args(args).status().expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// `git init` on branch `main` with a deterministic identity (no signing).
fn setup_identity(path: &Path) {
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    git(path, &["config", "commit.gpgsign", "false"]);
}

/// Commit staged changes with a fixed author + committer date, so history walks
/// (which sort by commit time) are deterministic across runs.
fn commit_at(path: &Path, message: &str, date: &str) {
    git_env(
        path,
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
        &["commit", "-q", "-m", message],
    );
}

/// A fresh repository on branch `main` with one committed file and a
/// deterministic identity (no signing, fixed user).
pub fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-q", "-m", "init"]);
    dir
}

/// A repository with three linear commits and known content: `init` (adds
/// README), `add a` (adds a.txt), `edit readme` (appends a known line).
pub fn init_repo_with_history() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "README.md"]);
    commit_at(path, "init", "2021-01-01T00:00:00");
    write(path, "a.txt", "alpha\n");
    git(path, &["add", "a.txt"]);
    commit_at(path, "add a", "2021-01-02T00:00:00");
    write(path, "README.md", "# test\nsecond line\n");
    git(path, &["add", "README.md"]);
    commit_at(path, "edit readme", "2021-01-03T00:00:00");
    dir
}

/// A repository with a feature branch merged back into `main` (a real merge
/// commit with two parents), to exercise multi-parent walks and the rail graph.
pub fn init_repo_with_branches() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "."]);
    commit_at(path, "init", "2021-01-01T00:00:00");
    git(path, &["checkout", "-q", "-b", "feature"]);
    write(path, "feature.txt", "feature\n");
    git(path, &["add", "."]);
    commit_at(path, "add feature", "2021-01-02T00:00:00");
    git(path, &["checkout", "-q", "main"]);
    write(path, "main.txt", "main\n");
    git(path, &["add", "."]);
    commit_at(path, "add main file", "2021-01-03T00:00:00");
    git_env(
        path,
        &[
            ("GIT_AUTHOR_DATE", "2021-01-04T00:00:00"),
            ("GIT_COMMITTER_DATE", "2021-01-04T00:00:00"),
        ],
        &["merge", "--no-ff", "-q", "-m", "merge feature", "feature"],
    );
    dir
}

/// A repository with `main` and `feature` genuinely diverged from a common base:
/// after the shared `init` commit, `feature` adds two commits and `main` adds one
/// — no merge. `merge-base(main, feature)` is the `init` commit and differs from
/// both tips, so three-dot (`main...feature`) and two-dot ranges differ. `feature`
/// is the checked-out branch (HEAD), so `strix diff main` reviews what `feature`
/// adds.
///
/// Layout:
/// - `init`         (base, on both)      README.md
/// - `main`:  base → `main change`       README.md edited, main-only.txt added
/// - `feature`: base → `feat one` → `feat two`  feature.txt, feature2.txt, rename
pub fn init_repo_with_diverged_branches() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\nshared\n");
    write(path, "shared.txt", "alpha\nbeta\ngamma\n");
    git(path, &["add", "."]);
    commit_at(path, "init", "2021-01-01T00:00:00");

    // main advances past the base (but never merges feature).
    write(path, "main-only.txt", "main\n");
    write(path, "README.md", "# test\nshared\nmain edit\n");
    git(path, &["add", "."]);
    commit_at(path, "main change", "2021-01-02T00:00:00");

    // feature branches off the base and adds its own commits.
    git(path, &["checkout", "-q", "-b", "feature", "HEAD~1"]);
    write(path, "feature.txt", "feature\n");
    git(path, &["add", "."]);
    commit_at(path, "feat one", "2021-01-03T00:00:00");
    // A rename+modify (exercises -M) plus a second added file.
    git(path, &["mv", "shared.txt", "renamed.txt"]);
    write(path, "renamed.txt", "alpha\nbeta\ngamma\ndelta\n");
    write(path, "feature2.txt", "more\n");
    git(path, &["add", "."]);
    commit_at(path, "feat two", "2021-01-04T00:00:00");
    dir
}

/// A repository with an orphan `unrelated` branch: a second root with no shared
/// history, so `merge-base(main, unrelated)` fails. `main` is left checked out.
pub fn init_repo_with_orphan_branch() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "."]);
    commit_at(path, "init", "2021-01-01T00:00:00");

    git(path, &["checkout", "-q", "--orphan", "unrelated"]);
    git(path, &["rm", "-rfq", "--cached", "."]);
    write(path, "other.txt", "unrelated\n");
    git(path, &["add", "."]);
    commit_at(path, "orphan root", "2021-01-02T00:00:00");

    git(path, &["checkout", "-q", "main"]);
    dir
}

/// A primary checkout plus a linked worktree checked out on a distinct branch
/// (`side`). Both `TempDir`s must stay alive for the test; the linked worktree
/// lives at [`WorktreeRepo::worktree`], outside the main working tree, so its
/// `.git` is a file pointing back to the shared common dir — the property that
/// makes both checkouts resolve the same comments store.
pub struct WorktreeRepo {
    pub main: TempDir,
    /// Parent of the linked worktree (kept alive to hold the directory).
    pub worktrees: TempDir,
}

impl WorktreeRepo {
    /// The linked worktree's working-tree root.
    pub fn worktree(&self) -> PathBuf {
        self.worktrees.path().join("wt")
    }
}

/// Build a [`WorktreeRepo`]: a one-commit `main` repo with a linked worktree on
/// branch `side`, added via `git worktree add`.
pub fn init_repo_with_worktree() -> WorktreeRepo {
    let main = init_repo();
    let worktrees = tempfile::tempdir().expect("tempdir");
    let wt = worktrees.path().join("wt");
    git(
        main.path(),
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    WorktreeRepo { main, worktrees }
}

/// A repository with identity configured but no commits (unborn HEAD).
pub fn init_empty_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_identity(dir.path());
    dir
}

/// A repository whose latest commit ("add binary") introduces a file containing
/// NUL bytes, for binary-detection tests.
pub fn setup_for_binary() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    setup_identity(path);
    write(path, "README.md", "# test\n");
    git(path, &["add", "."]);
    commit_at(path, "init", "2021-01-01T00:00:00");
    write(path, "bin.dat", "a\0b\0c\n");
    git(path, &["add", "."]);
    commit_at(path, "add binary", "2021-01-02T00:00:00");
    dir
}

// --- More repo fixtures -------------------------------------------------------

/// A repository with a `feature` branch adding a 40-line `big.txt` not present
/// on the base, so a range diff against it runs well past a typical viewport.
pub fn tall_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["checkout", "-qb", "feature"]);
    let mut content = String::new();
    for i in 1..=40 {
        content.push_str(&format!("row {i}\n"));
    }
    write(p, "big.txt", &content);
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add big"]);
    dir
}

/// A repository with one commit changing a single line of `file.txt`
/// (`OLD` -> `NEW`), on a `feature` branch off the base.
pub fn modified_line_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    write(p, "file.txt", "line1\nOLD\nline3\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "base"]);
    git(p, &["checkout", "-qb", "feature"]);
    write(p, "file.txt", "line1\nNEW\nline3\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "change"]);
    dir
}

/// A repository with a pure rename (`orig.txt` -> `renamed.txt`, no content
/// change) committed on a `feature` branch off the base.
pub fn pure_rename_repo() -> TempDir {
    let dir = init_repo();
    let p = dir.path();
    write(p, "orig.txt", "unchanged\ncontent\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add orig"]);
    git(p, &["checkout", "-qb", "feature"]);
    git(p, &["mv", "orig.txt", "renamed.txt"]);
    git(p, &["commit", "-qm", "pure rename"]);
    dir
}

/// A repository with three modified files (`a.txt`/`b.txt`/`c.txt`, each
/// `two` -> `TWO` on the middle line), for multi-file window/stream tests.
pub fn three_modified_files() -> TempDir {
    let repo = init_repo();
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(repo.path(), name, "one\ntwo\nthree\n");
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "files"]);
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(repo.path(), name, "one\nTWO\nthree\n");
    }
    repo
}

/// Write `contents` to `rel` in `dir` and commit it with `msg`.
pub fn commit_file(dir: &Path, rel: &str, contents: &str, msg: &str) {
    write(dir, rel, contents);
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", msg]);
}

/// `repo`'s current `HEAD` commit OID.
pub fn head_oid(repo: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    assert!(
        out.status.success(),
        "git rev-parse HEAD failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// --- Cross-file scroll / strip-mouse shared fixtures --------------------------
//
// `cross_file_scroll_test.rs` and `strip_mouse_test.rs` both drive the same
// continuous diff-pane stream at a shared 120-column viewport; these nine
// helpers were byte-identical between the two suites.

/// The fixed pane width both suites render at.
const STREAM_W: u16 = 120;

/// A working-tree status file's stream identity.
pub fn unstaged(path: &str) -> FileId {
    FileId::Status {
        section: Section::Unstaged,
        path: path.to_string(),
    }
}

/// An index status file's stream identity — the *other* entry a path modified
/// both in the index and in the working tree occupies.
pub fn staged(path: &str) -> FileId {
    FileId::Status {
        section: Section::Staged,
        path: path.to_string(),
    }
}

/// Cursor's resolved [`RowTarget`] at the anchor's current layout, if any.
pub fn cursor_target(app: &App) -> Option<RowTarget> {
    let w = app.diff_area().width;
    let idx = app.review_cursor();
    app.diff_layout(w).get(idx).map(|r| r.target)
}

/// A status repo with two small modified files (`b.txt`, `c.txt`), short
/// enough that both fit in a typical window at once.
pub fn short_status_repo() -> TempDir {
    let repo = init_repo();
    write(repo.path(), "b.txt", "one\ntwo\n");
    write(repo.path(), "c.txt", "x\ny\n");
    repo
}

/// A cross-file-scroll / wrap config with both flags explicit.
pub fn config(cross_file: bool, wrap: bool) -> Config {
    Config {
        cross_file_scroll: Some(cross_file),
        wrap_lines: Some(wrap),
        ..Config::default()
    }
}

/// A status-mode `App` over `repo` with `cfg`.
pub fn app_for(repo: &TempDir, cfg: Config) -> App {
    App::with_config(repo.path().to_path_buf(), &cfg).unwrap()
}

/// Force the diff window to prepare at `app`'s current pane size.
pub fn prepare_window(app: &mut App) {
    let area = app.diff_area();
    app.ensure_diff_window(area.width, area.height);
}

/// `app`'s current diff window at its current pane size.
pub fn window_of(app: &App) -> DiffWindow {
    let area = app.diff_area();
    app.diff_window(area.width, area.height)
}

/// Build `app_for(repo, cfg)` and render one frame at [`STREAM_W`]×`h`, so the
/// diff window is prepared before the test drives it further.
pub fn rendered_app(repo: &TempDir, cfg: Config, h: u16) -> App {
    let app = app_for(repo, cfg);
    dump_frame(&app, STREAM_W, h).unwrap();
    app
}

/// Move the status-list selection to `index` via `j`/`k`, then render one
/// frame at [`STREAM_W`]×`h`.
pub fn select(app: &mut App, index: usize, h: u16) {
    while app.selected < index {
        press(app, 'j');
    }
    while app.selected > index {
        press(app, 'k');
    }
    dump_frame(app, STREAM_W, h).unwrap();
}

/// The currently selected file's path, or `""` if none.
pub fn selected_path(app: &App) -> String {
    app.selected_file()
        .map(|(_, e)| e.path.clone())
        .unwrap_or_default()
}

/// The title text rendered on the row just above `area` in `buf`.
pub fn pane_title(buf: &Buffer, area: Rect) -> String {
    (area.x..area.x + area.width)
        .map(|x| {
            buf.cell((x, area.y - 1))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        })
        .collect()
}
