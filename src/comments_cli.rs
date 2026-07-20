//! `strix comment <list|add|rm|clear|gc>` — the agent-facing CLI over the
//! comments store (plan §3.3).
//!
//! Discipline, uniform across actions: machine JSON only on stdout (`--json`),
//! diagnostics/warnings only on stderr, and any failure exits non-zero with a
//! stderr message (no JSON error envelope). Every action operates on the current
//! HEAD branch key; a corrupt store fails the action non-zero and leaves the file
//! untouched (the store's fresh-read-before-write guard, [`comments::mutate`]).
//!
//! `list`/`add` are scope-aware (plan §3.3): a comment lives either on the
//! uncommitted working tree (`Scope::WorkTree`) or on a committed range
//! (`Scope::Range`), and each headless call first re-anchors/sweeps the
//! relevant scope so the result is never stale. The worktree pass reuses
//! [`crate::app::worktree_facts`] — the exact engine a live TUI session runs
//! (`App::sync_status_comments`, C3) — so there is one sweep engine, not two.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::app::worktree_facts;
use crate::cli::{CommentAction, ScopeArg};
use crate::comments::{self, Comment, Scope, Side, Source};
use crate::git::{Change, CommitFile, DiffLine, FileDiff, FileEntry, Repo, ReviewSpec, Status};

/// Dispatch a `strix comment` action against the repository at `repo_path`.
///
/// `rm`/`clear`/`gc` derive the inbox key with a single `head_branch_key()`
/// read — they never also read `Status`, so there's no second read to
/// disagree with. `list` and worktree `add` *do* read `Status` (for the
/// dirty-check default and the worktree sweep facts), so they derive the key
/// from that same snapshot instead (`status_branch_key`, codex-C4 #2): a
/// separate `head_branch_key()` call there would be a second, independent git
/// read that could momentarily disagree with `Status` under an external
/// checkout race, keying the inbox to one branch while sweeping with another
/// branch's facts.
pub fn run(repo_path: &Path, action: &CommentAction) -> Result<()> {
    let repo = Repo::open(repo_path)?;
    let dir = repo.strix_dir();
    match action {
        CommentAction::List { scope, json } => list(&repo, &dir, *scope, *json),
        CommentAction::Add {
            file,
            old_line,
            new_line,
            text,
            scope,
            range,
            json,
        } => add(
            &repo,
            &dir,
            file,
            *old_line,
            *new_line,
            text,
            *scope,
            range.as_deref(),
            *json,
        ),
        CommentAction::Rm { id, json } => {
            let branch = repo.head_branch_key()?;
            rm(&dir, &branch, *id, *json)
        }
        CommentAction::Clear { scope, all, json } => {
            let branch = repo.head_branch_key()?;
            clear(&dir, &branch, *scope, *all, *json)
        }
        CommentAction::Gc { json } => gc(&repo, &dir, *json),
    }
}

/// The inbox key implied by a `Status` snapshot, via [`Status::branch_key`] —
/// the branch name (normal or unborn) or, when `HEAD` is detached, the full
/// commit hex, exactly matching [`Repo::head_branch_key`]'s semantics but
/// derived from *this* read rather than a second one (codex-C4 #2). `None`
/// only if `git status` reported neither a branch name nor an oid, which
/// shouldn't happen in practice; surfaced as an error rather than silently
/// guessing a key.
fn status_branch_key(status: &Status) -> Result<String> {
    status
        .branch_key()
        .context("could not determine the checked-out branch from `git status`")
}

fn list(repo: &Repo, dir: &Path, scope: Option<ScopeArg>, json_out: bool) -> Result<()> {
    let status = repo.status()?;
    let branch = status_branch_key(&status)?;
    // A corrupt/unsupported store fails here (non-zero) before any write.
    let stored_range = comments::load(dir)?
        .branches
        .get(&branch)
        .and_then(|b| b.active_range.clone());

    // Default per plan §3.3: worktree when the repo is dirty (the common agent
    // case), else the branch's active reviewed range.
    let effective = scope.unwrap_or(if status.is_clean() {
        ScopeArg::Range
    } else {
        ScopeArg::Worktree
    });

    // Scope-filtered re-anchor + sweep first (write-elided), so a headless list
    // is never stale.
    if matches!(effective, ScopeArg::Worktree | ScopeArg::All) {
        sync_worktree_scope(repo, dir, &branch, &status)?;
    }
    if matches!(effective, ScopeArg::Range | ScopeArg::All) {
        if let Some(range) = &stored_range {
            sync_range_scope(repo, dir, &branch, range)?;
        }
    }

    // Re-read after the pass(es) above (each already persisted its own change,
    // if any) for the final, scope-filtered set.
    let store = comments::load(dir)?;
    let entry = store.branches.get(&branch);
    let range = entry.and_then(|b| b.active_range.clone());
    let comments: Vec<Comment> = entry
        .map(|b| b.comments.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| scope_matches(c, effective))
        .collect();

    if json_out {
        let value = json!({
            "branch": branch,
            "range": range,
            "comments": comments,
        });
        print_json(&value);
    } else {
        print_table(&comments);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add(
    repo: &Repo,
    dir: &Path,
    file: &str,
    old_line: Option<usize>,
    new_line: Option<usize>,
    text: &str,
    scope: Option<ScopeArg>,
    range: Option<&str>,
    json_out: bool,
) -> Result<()> {
    let (side, line) = match (old_line, new_line) {
        (Some(l), None) => (Side::Old, l),
        (None, Some(l)) => (Side::New, l),
        (Some(_), Some(_)) => bail!("provide exactly one of --old-line or --new-line, not both"),
        (None, None) => bail!("provide one of --old-line or --new-line to anchor the comment"),
    };
    if line < 1 {
        bail!("line numbers are 1-based; --old-line/--new-line must be >= 1");
    }
    if text.trim().is_empty() {
        bail!("--text must not be empty");
    }
    let scope = scope.unwrap_or(ScopeArg::Worktree);
    if scope == ScopeArg::All {
        bail!("--scope all is not valid for `add`; use worktree or range");
    }

    let comment = match scope {
        // Worktree needs `Status` (for the baseline OID + sweep facts), so its
        // inbox key is derived from that same snapshot (codex-C4 #2) rather
        // than a second, separate `head_branch_key()` read.
        ScopeArg::Worktree => {
            let status = repo.status()?;
            let branch = status_branch_key(&status)?;
            add_worktree(repo, dir, &branch, file, side, line, text, &status)?
        }
        // Range never touches `Status`, so a single `head_branch_key()` read
        // has nothing to disagree with.
        ScopeArg::Range => {
            let branch = repo.head_branch_key()?;
            add_range(repo, dir, &branch, file, side, line, text, range)?
        }
        ScopeArg::All => unreachable!("rejected above"),
    };

    if json_out {
        print_json(&json!({ "comment": comment }));
    } else {
        println!("{}", comment.id);
    }
    Ok(())
}

/// `add`'s worktree-scope path: re-anchor/sweep the branch's existing worktree
/// comments first (the same pass `list --scope worktree` runs), then push the
/// new comment stamped with the current HEAD as its baseline (plan §3.3).
/// Takes `status` from the caller (rather than reading it again) so the
/// branch key, the sweep facts, and the baseline OID all come from one
/// snapshot (codex-C4 #2).
#[allow(clippy::too_many_arguments)]
fn add_worktree(
    repo: &Repo,
    dir: &Path,
    branch: &str,
    file: &str,
    side: Side,
    line: usize,
    text: &str,
    status: &Status,
) -> Result<Comment> {
    sync_worktree_scope(repo, dir, branch, status)?;

    let context = capture_context_worktree(repo, status, file, side, line);
    let orphaned = context.is_none();
    let base = status.head_oid.clone();
    let created_at = comments::now_secs();

    comments::mutate(dir, |store| {
        let id = store.take_id();
        let entry = store.branches.entry(branch.to_string()).or_default();
        let comment = Comment {
            scope: Scope::WorkTree,
            id,
            source: Source::Agent,
            file: file.to_string(),
            side,
            line,
            text: text.to_string(),
            context,
            orphaned,
            created_at,
            base,
            stale: false,
        };
        entry.comments.push(comment.clone());
        comment
    })
}

/// `add`'s range-scope path: resolve the target range (`--range`, else the
/// branch's active range — an error if neither exists, so an invalid empty
/// range is never persisted), re-anchor the branch's existing range comments
/// against it, then push the new comment.
#[allow(clippy::too_many_arguments)]
fn add_range(
    repo: &Repo,
    dir: &Path,
    branch: &str,
    file: &str,
    side: Side,
    line: usize,
    text: &str,
    range: Option<&str>,
) -> Result<Comment> {
    let target = resolve_add_range(dir, branch, range)?;
    sync_range_scope(repo, dir, branch, &target)?;

    let (context, orphaned) = match capture_context(repo, &target, file, side, line) {
        Ok(Some(text)) => (Some(text), false),
        Ok(None) => (None, true),
        Err(_) => (None, false),
    };
    let created_at = comments::now_secs();

    comments::mutate(dir, |store| {
        let id = store.take_id();
        let entry = store.branches.entry(branch.to_string()).or_default();
        // The session-open pass (`strix diff`) records a review range; do it
        // defensively here too, mirroring `App::save_comment` — an `--range` given
        // explicitly with none stored yet becomes the active one.
        if entry.active_range.is_none() {
            entry.active_range = Some(target.clone());
        }
        let comment = Comment {
            scope: Scope::Range {
                range: target.clone(),
            },
            id,
            source: Source::Agent,
            file: file.to_string(),
            side,
            line,
            text: text.to_string(),
            context,
            orphaned,
            created_at,
            base: None,
            stale: false,
        };
        entry.comments.push(comment.clone());
        comment
    })
}

/// The target range for a `--scope range add`: `--range` when given, else the
/// branch's stored `active_range`. Errors when neither is available — a range
/// comment always needs a range to anchor against, so this never persists the
/// empty-range placeholder the milestone-6 CLI used to write (codex-C2a
/// finding #8).
fn resolve_add_range(dir: &Path, branch: &str, range: Option<&str>) -> Result<String> {
    if let Some(range) = range {
        return Ok(range.to_string());
    }
    let stored = comments::load(dir)?
        .branches
        .get(branch)
        .and_then(|b| b.active_range.clone());
    stored.context(
        "range scope needs a range: pass --range <RANGE>, or run `strix diff <RANGE>` \
         first to record this branch's active range",
    )
}

fn rm(dir: &Path, branch: &str, id: u64, json_out: bool) -> Result<()> {
    // Detect a missing id from a fresh read *before* mutating: an unknown id must
    // surface "not found on branch <key>", not a spurious write error under an
    // unwritable store, and must not touch the file at all (plan §3.3). Ids are
    // store-global, so this looks across every scope on the branch.
    let present = comments::load(dir)?
        .branches
        .get(branch)
        .is_some_and(|b| b.comments.iter().any(|c| c.id == id));
    if !present {
        bail!("comment {id} not found on branch {branch}");
    }
    let removed = comments::mutate(dir, |store| {
        let entry = store.branches.get_mut(branch)?;
        let pos = entry.comments.iter().position(|c| c.id == id)?;
        let comment = entry.comments.remove(pos);
        Some((comment, entry.comments.len()))
    })?;
    match removed {
        Some((comment, remaining)) => {
            if json_out {
                print_json(&json!({ "removed": comment, "remaining": remaining }));
            } else {
                println!("removed comment {id}");
            }
            Ok(())
        }
        None => bail!("comment {id} not found on branch {branch}"),
    }
}

/// `clear` never wipes everything implicitly (plan §3.3): exactly one of
/// `--scope` or `--all` is required, and combining both is a usage error.
fn clear(
    dir: &Path,
    branch: &str,
    scope: Option<ScopeArg>,
    all: bool,
    json_out: bool,
) -> Result<()> {
    let effective = match (scope, all) {
        (Some(_), true) => bail!("pass either --scope or --all, not both"),
        (Some(scope), false) => scope,
        (None, true) => ScopeArg::All,
        (None, false) => bail!(
            "clear requires a scope: pass --scope worktree|range|all, or --all — \
             a bare `clear` never wipes everything implicitly"
        ),
    };
    let cleared = comments::mutate(dir, |store| match store.branches.get_mut(branch) {
        Some(entry) => {
            let before = entry.comments.len();
            entry.comments.retain(|c| !scope_matches(c, effective));
            before - entry.comments.len()
        }
        None => 0,
    })?;
    if json_out {
        print_json(&json!({ "cleared": cleared }));
    } else {
        println!("cleared {cleared} comment(s)");
    }
    Ok(())
}

fn gc(repo: &Repo, dir: &Path, json_out: bool) -> Result<()> {
    let mut live: HashSet<String> = repo.branch_names()?.into_iter().collect();
    // The checked-out inbox is always live: an unborn HEAD names a branch that
    // `branch_names()` can't see yet (no ref), and a detached HEAD's commit key
    // is its own liveness — either way GC must never drop the current session's
    // comments (plan §3.1). Both scopes on a dropped branch key go together —
    // `comments::gc` retires the whole branch entry, not one scope of it.
    live.insert(repo.head_branch_key()?);
    let result = comments::mutate_if_changed(dir, |store| {
        let result = comments::gc(store, &live, |key| repo.commit_exists(key));
        let changed = !result.is_empty();
        (result, changed)
    })?;
    if json_out {
        print_json(&json!({
            "removed_branches": result.removed_branches,
            "removed_comments": result.removed_comments,
        }));
    } else {
        println!(
            "removed {} branch(es), {} comment(s)",
            result.removed_branches.len(),
            result.removed_comments
        );
    }
    Ok(())
}

/// Whether `comment` belongs to the requested CLI `scope` selector.
fn scope_matches(comment: &Comment, scope: ScopeArg) -> bool {
    match scope {
        ScopeArg::All => true,
        ScopeArg::Worktree => matches!(comment.scope, Scope::WorkTree),
        ScopeArg::Range => matches!(comment.scope, Scope::Range { .. }),
    }
}

/// Fresh-read the store, run `apply` against this branch's freshly loaded
/// comments `Vec` in place, and persist only when `apply` reports a change
/// (write-elided) — the shared shell behind [`sync_worktree_scope`] and
/// [`sync_range_scope`], which differ only in *which* engine they run over
/// that `Vec` (sweep vs. scoped re-anchor).
///
/// `apply` runs *inside* this one `mutate_if_changed` closure against the
/// freshly loaded store: earlier the CLI computed a re-anchor against an
/// already-stale clone and then unconditionally overwrote the freshly-read
/// entry with it, silently dropping any comment a concurrent writer had added
/// in between (codex-C2a finding #3). Mutating the freshly-loaded entry's
/// `Vec` in place, as done here, can't lose a concurrent write that way.
fn sync_scope(
    dir: &Path,
    branch: &str,
    apply: impl FnOnce(&mut Vec<Comment>) -> bool,
) -> Result<()> {
    comments::mutate_if_changed(dir, |store| {
        let entry = store.branches.entry(branch.to_string()).or_default();
        let changed = apply(&mut entry.comments);
        ((), changed)
    })
}

/// Apply the worktree lifecycle pass (re-anchor + sweep, plan §3.2) to
/// `branch`'s comments — the exact engine [`crate::app`]'s
/// `sync_status_comments` runs for a live TUI session (C3), via the shared
/// [`worktree_facts`], so a headless `list`/`add` sees the same lifecycle a
/// human's session would, not a second copy of it.
fn sync_worktree_scope(repo: &Repo, dir: &Path, branch: &str, status: &Status) -> Result<()> {
    sync_scope(dir, branch, |comments| {
        comments::sweep_worktree(comments, status.head_oid.as_deref(), |comment| {
            worktree_facts(repo, status, comment)
        })
    })
}

/// Re-anchor `branch`'s `Scope::Range` comments matching `target` against
/// `target`'s diff — the milestone-6 range re-anchor, scoped so a worktree
/// comment or a comment from a *different* stored range is never touched. A
/// `target` that fails to resolve warns on stderr and changes nothing
/// (matches the milestone-6 `list`/`add` behavior: an unreadable range never
/// fails the caller, since the caller may just be re-listing a stale/deleted
/// range).
fn sync_range_scope(repo: &Repo, dir: &Path, branch: &str, target: &str) -> Result<()> {
    sync_scope(dir, branch, |comments| {
        match resolve_range_files(repo, target) {
            Ok((spec, files)) => comments::reanchor_scoped(
                comments,
                |c| matches_range_scope(c, target),
                &files,
                |file| repo.range_file_diff(&spec, file),
            ),
            Err(err) => {
                eprintln!("strix: warning: could not re-anchor against range '{target}': {err:#}");
                false
            }
        }
    })
}

/// Whether `comment` is a range comment belonging to `target`: an empty
/// recorded range (a legacy/CLI placeholder predating scoped ranges) matches
/// any target — same leniency as `App`'s review-side re-anchor
/// (`is_review_scope`) — a worktree comment never matches.
fn matches_range_scope(comment: &Comment, target: &str) -> bool {
    matches!(&comment.scope, Scope::Range { range } if range.is_empty() || range == target)
}

fn resolve_range_files(repo: &Repo, range: &str) -> Result<(ReviewSpec, Vec<CommitFile>)> {
    let spec = repo.resolve_range(range)?;
    let files = repo.range_files(&spec)?;
    Ok((spec, files))
}

/// The anchored line's text at `line` on `side` in `file`'s **worktree** net
/// diff (HEAD→worktree, plan §3.1), or `None` when the file/line isn't present
/// there (deleted, untracked-but-absent, or simply not part of the current
/// diff). Mirrors [`capture_context`]'s range counterpart but over the
/// worktree surface; unlike the sweep engine's lifecycle pass, no
/// rename-following is needed — the caller names the file directly, and a
/// nonexistent path's HEAD/worktree bytes both resolve empty, yielding an
/// empty (not-found) diff on their own.
fn capture_context_worktree(
    repo: &Repo,
    status: &Status,
    file: &str,
    side: Side,
    line: usize,
) -> Option<String> {
    let entry = status
        .staged
        .iter()
        .chain(status.unstaged.iter())
        .find(|e| e.path == file)
        .cloned()
        .unwrap_or_else(|| FileEntry {
            path: file.to_string(),
            orig_path: None,
            change: Change::Modified,
        });
    let FileDiff::Text(lines) = repo.file_diff_head_vs_worktree(&entry) else {
        return None; // binary
    };
    lines
        .iter()
        .find(|dl| line_no(dl, side) == Some(line))
        .map(|dl| dl.text.clone())
}

/// The anchored line's text at `line` on `side` in the range's diff of `file`,
/// or `None` when the file/line isn't present (or the file is binary).
fn capture_context(
    repo: &Repo,
    range: &str,
    file: &str,
    side: Side,
    line: usize,
) -> Result<Option<String>> {
    let (spec, files) = resolve_range_files(repo, range)?;
    let Some(commit_file) = files.iter().find(|f| f.path == file) else {
        return Ok(None);
    };
    let FileDiff::Text(lines) = repo.range_file_diff(&spec, commit_file) else {
        return Ok(None);
    };
    let context = lines
        .iter()
        .find(|dl| line_no(dl, side) == Some(line))
        .map(|dl| dl.text.clone());
    Ok(context)
}

fn line_no(line: &DiffLine, side: Side) -> Option<usize> {
    match side {
        Side::Old => line.old_no,
        Side::New => line.new_no,
    }
}

/// Print a value as pretty JSON followed by a newline (agent- and human-readable).
fn print_json(value: &serde_json::Value) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("strix: warning: could not serialize JSON output: {err}"),
    }
}

/// The plain-text table: `⚠` marks orphans, `w`/`r` the scope, columns aligned.
/// An empty set prints nothing (exit 0).
fn print_table(comments: &[Comment]) {
    if comments.is_empty() {
        return;
    }
    let locs: Vec<String> = comments.iter().map(location).collect();
    let id_w = comments
        .iter()
        .map(|c| c.id.to_string().len())
        .max()
        .unwrap_or(1);
    let loc_w = locs.iter().map(String::len).max().unwrap_or(0);
    for (comment, loc) in comments.iter().zip(&locs) {
        let mark = if comment.orphaned { '⚠' } else { ' ' };
        let scope = match &comment.scope {
            Scope::WorkTree => 'w',
            Scope::Range { .. } => 'r',
        };
        let first = comment.text.lines().next().unwrap_or("");
        println!(
            "{mark} {scope} {:>id_w$}  {:<loc_w$}  {first}",
            comment.id, loc,
        );
    }
}

fn location(comment: &Comment) -> String {
    let side = match comment.side {
        Side::Old => "old",
        Side::New => "new",
    };
    format!("{}:{side}:{}", comment.file, comment.line)
}
