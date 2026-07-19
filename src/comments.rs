//! Review comments: the on-disk model, its JSON store, and the re-anchor engine.
//!
//! Comments live in `<common_dir>/strix/comments.json` (see [`Repo::strix_dir`]),
//! keyed by branch. The store is the shared inbox between a human leaving notes
//! in the TUI and an agent reading them via `strix comment list --json`. This
//! module owns the data contract only; the TUI and CLI wiring live in later
//! commits.
//!
//! [`Repo::strix_dir`]: crate::git::Repo::strix_dir

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::git::{CommitFile, DiffLine, FileDiff};

// The store schema version this build reads and writes. A store found with a
// higher version is refused for both read and write (no silent downgrade).
const STORE_VERSION: u32 = 1;
const STORE_FILE: &str = "comments.json";
// Content-match re-anchoring only relocates a comment within this many lines of
// its stored position; a farther match orphans instead of faking an "addressed"
// signal by silently teleporting the note (plan §3.2).
const REANCHOR_WINDOW: usize = 10;
// A detached-HEAD branch key is a full SHA-1 commit hex.
const COMMIT_HEX_LEN: usize = 40;

/// Who authored a comment. Humans leave notes in the TUI; agents annotate via
/// the CLI (which can only ever author `agent` notes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Human,
    Agent,
}

/// Which side of the diff a comment anchors to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Old,
    New,
}

/// A single review comment anchored to a line of a file's diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Comment {
    pub id: u64,
    pub source: Source,
    /// The file's new-side path (`CommitFile::path`); a later rename orphans it.
    pub file: String,
    pub side: Side,
    /// 1-based line number on `side`.
    pub line: usize,
    pub text: String,
    /// The anchored line's text at authoring time. `None` means "context
    /// unavailable" and always orphans on drift — it never content-matches.
    pub context: Option<String>,
    pub orphaned: bool,
    /// Unix epoch seconds.
    pub created_at: u64,
}

/// One branch's inbox: the last reviewed range and its comments.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    /// The last `strix diff` argument recorded on this branch, or `None` until
    /// a review session has run.
    pub range: Option<String>,
    pub comments: Vec<Comment>,
}

/// The whole comments store: a global id counter plus per-branch inboxes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub version: u32,
    pub next_id: u64,
    pub branches: BTreeMap<String, Branch>,
}

impl Default for Store {
    fn default() -> Store {
        Store {
            version: STORE_VERSION,
            next_id: 1,
            branches: BTreeMap::new(),
        }
    }
}

impl Store {
    /// Mint the next comment id, advancing the global counter.
    ///
    /// `next_id` is a hint, not a source of truth: a hand-edited store can carry
    /// a counter at or below an existing id. Minting `max(next_id, max_id + 1)`
    /// keeps ids unique regardless of how the file was last written.
    pub fn take_id(&mut self) -> u64 {
        let max_existing = self
            .branches
            .values()
            .flat_map(|branch| &branch.comments)
            .map(|comment| comment.id)
            .max()
            .unwrap_or(0);
        let id = self.next_id.max(max_existing + 1);
        self.next_id = id + 1;
        id
    }
}

/// Load the store from `dir/comments.json`.
///
/// A missing *or zero-byte* file is a valid empty store. A file that fails to
/// parse returns an error and is left untouched. A store whose `version` is
/// newer than this build understands is refused (read *and*, by extension,
/// write — every mutation reads first) with a clear message.
pub fn load(dir: &Path) -> Result<Store> {
    let path = dir.join(STORE_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Store::default()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    if bytes.is_empty() {
        return Ok(Store::default());
    }
    let store: Store =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    if store.version > STORE_VERSION {
        anyhow::bail!(
            "{} is comments store version {}, but this strix understands only \
             version {}; refusing to read or write it",
            path.display(),
            store.version,
            STORE_VERSION
        );
    }
    Ok(store)
}

/// Apply `f` to a freshly-read store and persist the result atomically.
///
/// Every mutation is read-modify-write against the current on-disk state, so an
/// agent's concurrent `rm` is observed before we overwrite. A failed read (parse
/// error, unsupported version) aborts *before* any write — the file is never
/// clobbered.
pub fn mutate<F, T>(dir: &Path, f: F) -> Result<T>
where
    F: FnOnce(&mut Store) -> T,
{
    let mut store = load(dir)?;
    let out = f(&mut store);
    save(dir, &store)?;
    Ok(out)
}

/// The write-eliding cousin of [`mutate`]: read the store, apply `f`, and persist
/// **only when `f`'s returned flag is `true`**. `f` returns `(value, changed)`.
///
/// This is what keeps a re-anchor pass that changed nothing from rewriting the
/// file — and so from waking the store-dir watcher into a reload → re-anchor →
/// write loop (plan §3.2). The fresh read still enforces the never-clobber and
/// version guards before any write, exactly like [`mutate`].
pub fn mutate_if_changed<F, T>(dir: &Path, f: F) -> Result<T>
where
    F: FnOnce(&mut Store) -> (T, bool),
{
    let mut store = load(dir)?;
    let (out, changed) = f(&mut store);
    if changed {
        save(dir, &store)?;
    }
    Ok(out)
}

/// Write `store` to `dir/comments.json` as pretty-printed JSON (agent-readable),
/// atomically via [`crate::config::write_atomic`]. Creates `dir` if missing.
///
/// Private on purpose: it writes whatever it is handed, bypassing the
/// never-clobber and version guards. Every external write goes through
/// [`mutate`], which fresh-reads and enforces those guards first.
fn save(dir: &Path, store: &Store) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(STORE_FILE);
    let mut json = serde_json::to_string_pretty(store).context("serializing comments store")?;
    json.push('\n');
    crate::config::write_atomic(dir, &path, &json)
}

/// What a GC pass removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GcResult {
    /// The branch keys whose inbox was dropped.
    pub removed_branches: Vec<String>,
    /// Total comments dropped across those keys.
    pub removed_comments: usize,
}

impl GcResult {
    pub fn is_empty(&self) -> bool {
        self.removed_branches.is_empty()
    }
}

/// Drop dead inboxes: branch keys whose ref is gone, and detached (commit-hex)
/// keys whose commit no longer resolves. `live_branches` is the current set of
/// local branch short names; `commit_exists` reports whether a 40-hex commit key
/// still resolves. Each dropped set is logged with its comment count (a branch
/// rename recovery aid). Returns what was removed.
///
/// Live-branch membership is checked *before* the commit-hex shape test, so a
/// real branch that happens to be named as 40 hex chars is kept unconditionally
/// while it is alive — never misclassified as a stale detached key.
pub fn gc<F>(store: &mut Store, live_branches: &HashSet<String>, commit_exists: F) -> GcResult
where
    F: Fn(&str) -> bool,
{
    let mut result = GcResult::default();
    store.branches.retain(|key, branch| {
        let keep = if live_branches.contains(key) {
            true
        } else if is_commit_hex(key) {
            commit_exists(key)
        } else {
            false
        };
        if !keep {
            tracing::info!(
                branch = %key,
                comments = branch.comments.len(),
                "gc: dropping comments for a branch/commit that no longer exists"
            );
            result.removed_comments += branch.comments.len();
            result.removed_branches.push(key.clone());
        }
        keep
    });
    result
}

fn is_commit_hex(key: &str) -> bool {
    key.len() == COMMIT_HEX_LEN && key.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Re-anchor every comment against the review's current diff, in place, and
/// report whether anything changed (so the caller can elide a no-op write).
///
/// `files` is the review's changed-file list; `diff_for` computes a file's
/// `FileDiff` and is invoked at most once per file that carries a comment
/// (results are cached). The algorithm is exactly plan §3.2.
pub fn reanchor<F>(comments: &mut [Comment], files: &[CommitFile], mut diff_for: F) -> bool
where
    F: FnMut(&CommitFile) -> FileDiff,
{
    let mut cache: BTreeMap<String, FileDiff> = BTreeMap::new();
    let mut changed = false;
    for comment in comments.iter_mut() {
        let before = (comment.line, comment.orphaned);
        match files.iter().find(|f| f.path == comment.file) {
            None => comment.orphaned = true,
            Some(file) => {
                let diff = cache
                    .entry(comment.file.clone())
                    .or_insert_with(|| diff_for(file));
                reanchor_one(comment, diff);
            }
        }
        if before != (comment.line, comment.orphaned) {
            changed = true;
        }
    }
    changed
}

fn reanchor_one(comment: &mut Comment, diff: &FileDiff) {
    let lines = match diff {
        FileDiff::Binary => {
            comment.orphaned = true;
            return;
        }
        FileDiff::Text(lines) => lines,
    };

    let line_no = |line: &DiffLine| match comment.side {
        Side::Old => line.old_no,
        Side::New => line.new_no,
    };

    // (3) Exact hit: same side, same line number, and stored context matches the
    // line's text. `Some(text) == context` is false whenever context is `None`,
    // so an unavailable-context comment can never exact-match here.
    let exact = lines.iter().any(|line| {
        line_no(line) == Some(comment.line) && Some(&line.text) == comment.context.as_ref()
    });
    if exact {
        comment.orphaned = false;
        return;
    }

    // (4) Content match — only when context is available. Same-side lines whose
    // text equals the stored context, within ±REANCHOR_WINDOW of the stored
    // line: closest wins, ties broken toward the smaller line number.
    if let Some(context) = &comment.context {
        let best = lines
            .iter()
            .filter(|line| &line.text == context)
            .filter_map(line_no)
            .filter(|n| n.abs_diff(comment.line) <= REANCHOR_WINDOW)
            .min_by_key(|&n| (n.abs_diff(comment.line), n));
        if let Some(n) = best {
            comment.line = n;
            comment.orphaned = false;
            return;
        }
    }

    // (5) No match: orphan, keeping the stored line for display.
    comment.orphaned = true;
}
