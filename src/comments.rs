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
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::git::{CommitFile, DiffLine, FileDiff};

// The store schema version this build reads and writes. A higher version is
// refused for read and write (no silent downgrade); a version-1 (milestone-6)
// store is backed up once and reset to empty on load (see [`load`]).
const STORE_VERSION: u32 = 2;
const STORE_FILE: &str = "comments.json";
// Where [`load`] copies a version-1 store aside before resetting it to empty v2.
const STORE_BACKUP_V1: &str = "comments.json.v1.bak";
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

/// Which review surface a comment belongs to. Serialized *flat* and additively
/// into [`Comment`] via `#[serde(flatten)]` (plan §3.3): the internally-tagged
/// `scope` key carries the discriminant, and a range comment additionally carries
/// its `range` spec as a sibling key.
///
/// - [`Scope::WorkTree`] → `"scope":"worktree"` (no `range` key)
/// - [`Scope::Range`]    → `"scope":"range","range":"<spec>"`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum Scope {
    /// A comment on the uncommitted working tree (net HEAD-vs-worktree diff).
    #[serde(rename = "worktree")]
    WorkTree,
    /// A comment on a committed range review; `range` is the `strix diff` spec.
    Range { range: String },
}

/// A single review comment anchored to a line of a file's diff.
///
/// `scope` is flattened into the object as the pinned, additive JSON contract
/// (plan §3.3): a worktree comment serializes `{"scope":"worktree", …}` (no
/// `range` key), a range comment `{"scope":"range","range":"main", …}`. Every
/// pre-existing field keeps its name and shape, so milestone-6 skill parsers
/// keep working.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Comment {
    /// Which review surface this comment lives on (flattened; see [`Scope`]).
    #[serde(flatten)]
    pub scope: Scope,
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
    /// The HEAD commit OID (40-char lowercase hex) captured when a *worktree*
    /// comment was authored — its stable baseline. Range comments don't carry it
    /// (`None`, omitted from JSON). C2c compares it against the current HEAD to
    /// drive sweep/stale; C2a only lands the field so the schema is stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Set when the anchored line's content drifted under a still-current HEAD:
    /// surfaced with a dim marker, never auto-deleted. C2c drives it; here it
    /// defaults false and is read by serde's derive, so it is not dead code.
    #[serde(default)]
    pub stale: bool,
}

/// One branch's inbox: the active reviewed range and its comments.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    /// The last `strix diff` range recorded on this branch — the *active range*,
    /// the source for range-scoped operations — or `None` until a review session
    /// has run.
    pub active_range: Option<String>,
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
/// parse returns an error and is left untouched (never-clobber). Version handling
/// (plan §3.0):
///
/// - version 2 (current) → parse normally.
/// - version 1 (milestone-6) → back the file up once to `comments.json.v1.bak`,
///   then return an empty v2 store. The old comments are intentionally dropped
///   (backwards compatibility is not required); this is *not* an error. If the
///   backup can't be written the error surfaces and the v1 file is left intact —
///   we never reset without a backup.
/// - version > 2 → refused for read (and, since every mutation reads first,
///   write) with a clear message.
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
    // Decode only the version envelope first. A full `Store` parse requires the v2
    // `Comment` shape (the flattened `scope` tag), so a *real* v1 file with
    // comments would fail that parse and hit the never-clobber path — the upgrade
    // has to route on `version` alone, never on a successful v2 decode.
    let envelope: StoreEnvelope =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    match envelope.version {
        v if v == STORE_VERSION => {
            serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
        }
        1 => {
            backup_legacy_store(dir, &path, &bytes)?;
            // Carry the old id counter into the empty v2 store so an id minted
            // after the reset can't collide with one still referenced from the
            // backup (a stale `rm <id>` would otherwise delete an unrelated new
            // comment).
            Ok(Store {
                next_id: envelope.next_id.max(1),
                ..Store::default()
            })
        }
        v if v > STORE_VERSION => anyhow::bail!(
            "{} is comments store version {}, but this strix understands only \
             version {}; refusing to read or write it",
            path.display(),
            v,
            STORE_VERSION
        ),
        other => anyhow::bail!(
            "{} is comments store version {}, which this strix does not recognize; \
             refusing to read or write it",
            path.display(),
            other
        ),
    }
}

/// The minimal shape [`load`] decodes first, to route on `version` without
/// committing to the current `Comment` schema (so a v1 file with comments still
/// migrates instead of tripping the never-clobber guard).
#[derive(Deserialize)]
struct StoreEnvelope {
    version: u32,
    #[serde(default)]
    next_id: u64,
}

/// Copy a version-1 store aside before the v2 upgrade resets it. The original
/// bytes are written verbatim (they parsed as JSON, so they are valid UTF-8). An
/// existing backup is **never destroyed**: an identical one makes this an
/// idempotent no-op, and a *differing* one is kept while these bytes go to the
/// first free numbered sibling (`comments.json.v1.bak.1`, `.2`, …). Uses the same
/// atomic write as every other store write.
fn backup_legacy_store(dir: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    let contents = std::str::from_utf8(bytes)
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    let Some(backup) = free_backup_path(dir, bytes)? else {
        return Ok(()); // an identical backup already exists — nothing to do
    };
    crate::config::write_atomic(dir, &backup, contents)
        .with_context(|| format!("backing up {} to {}", path.display(), backup.display()))?;
    tracing::info!(
        backup = %backup.display(),
        "comments store upgraded v1 → v2; previous comments backed up and reset"
    );
    Ok(())
}

/// Where to back a v1 store up to: the base `comments.json.v1.bak` if free, else
/// the first numbered sibling that is free — or `None` when an existing backup
/// already holds these exact bytes (so a backup is never duplicated nor a
/// differing one clobbered).
fn free_backup_path(dir: &Path, bytes: &[u8]) -> Result<Option<PathBuf>> {
    const MAX_BACKUPS: u32 = 100;
    for n in 0..MAX_BACKUPS {
        let candidate = if n == 0 {
            dir.join(STORE_BACKUP_V1)
        } else {
            dir.join(format!("{STORE_BACKUP_V1}.{n}"))
        };
        match std::fs::read(&candidate) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Some(candidate)),
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", candidate.display()))
            }
            Ok(existing) if existing == bytes => return Ok(None), // already backed up
            Ok(_) => continue,                                    // differs — keep it, try next
        }
    }
    anyhow::bail!(
        "refusing to reset the v1 comments store: {} already holds {} differing backups",
        dir.display(),
        MAX_BACKUPS
    )
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

/// The current Unix time in whole seconds (a comment's `created_at`). A clock
/// before the epoch (unreachable in practice) yields `0` rather than panicking.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Re-anchor every comment against the review's current diff, in place, and
/// report whether anything changed (so the caller can elide a no-op write).
///
/// `files` is the review's changed-file list; `diff_for` computes a file's
/// `FileDiff` and is invoked at most once per file that carries a comment
/// (results are cached). The algorithm is exactly plan §3.2.
pub fn reanchor<F>(comments: &mut [Comment], files: &[CommitFile], diff_for: F) -> bool
where
    F: FnMut(&CommitFile) -> FileDiff,
{
    reanchor_scoped(comments, |_| true, files, diff_for)
}

/// Like [`reanchor`], but re-anchors only the comments for which `selected`
/// returns `true`, leaving every other comment untouched.
///
/// This lets a caller re-anchor a single scope's comments — the worktree's, or
/// one exact range's — against that scope's own diff without disturbing the other
/// scope (plan §3.2/§3.3). Since a store's comments interleave scopes in one
/// `Vec`, selection is by predicate rather than a contiguous sub-slice. The
/// ±`REANCHOR_WINDOW` content-match and the per-file diff caching are identical
/// to [`reanchor`]; there is deliberately **no** sweep/stale logic here (that
/// lands in C2c).
pub fn reanchor_scoped<S, F>(
    comments: &mut [Comment],
    selected: S,
    files: &[CommitFile],
    mut diff_for: F,
) -> bool
where
    S: Fn(&Comment) -> bool,
    F: FnMut(&CommitFile) -> FileDiff,
{
    let mut cache: BTreeMap<String, FileDiff> = BTreeMap::new();
    let mut changed = false;
    for comment in comments.iter_mut() {
        if !selected(comment) {
            continue;
        }
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
