//! `strix comment <list|add|rm|clear|gc>` — the agent-facing CLI over the
//! comments store (plan §3.3).
//!
//! Discipline, uniform across actions: machine JSON only on stdout (`--json`),
//! diagnostics/warnings only on stderr, and any failure exits non-zero with a
//! stderr message (no JSON error envelope). Every action operates on the current
//! HEAD branch key; a corrupt store fails the action non-zero and leaves the file
//! untouched (the store's fresh-read-before-write guard, [`comments::mutate`]).

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Result};
use serde_json::json;

use crate::cli::CommentAction;
use crate::comments::{self, Comment, Side, Source};
use crate::git::{DiffLine, FileDiff, Repo};

/// Dispatch a `strix comment` action against the repository at `repo_path`.
pub fn run(repo_path: &Path, action: &CommentAction) -> Result<()> {
    let repo = Repo::open(repo_path)?;
    let dir = repo.strix_dir();
    let branch = repo.head_branch_key()?;
    match action {
        CommentAction::List { json } => list(&repo, &dir, &branch, *json),
        CommentAction::Add {
            file,
            old_line,
            new_line,
            text,
            json,
        } => add(
            &repo, &dir, &branch, file, *old_line, *new_line, text, *json,
        ),
        CommentAction::Rm { id, json } => rm(&dir, &branch, *id, *json),
        CommentAction::Clear { json } => clear(&dir, &branch, *json),
        CommentAction::Gc { json } => gc(&repo, &dir, *json),
    }
}

fn list(repo: &Repo, dir: &Path, branch: &str, json_out: bool) -> Result<()> {
    // A corrupt/unsupported store fails here (non-zero) before any write.
    let store = comments::load(dir)?;
    let entry = store.branches.get(branch);
    let range = entry.and_then(|b| b.range.clone());
    let mut comments = entry.map(|b| b.comments.clone()).unwrap_or_default();

    // Re-anchor first, best-effort (plan §3.2c).
    reanchor_pass(repo, dir, branch, range.as_deref(), &mut comments);

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
    branch: &str,
    file: &str,
    old_line: Option<usize>,
    new_line: Option<usize>,
    text: &str,
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

    // Fresh read for the stored range (also fails fast on a corrupt store).
    let store = comments::load(dir)?;
    let range = store.branches.get(branch).and_then(|b| b.range.clone());

    // Re-anchor the branch's existing comments too — the pass runs on `add` as
    // well as `list` (plan §3.2c), so an agent annotating also refreshes the set.
    let mut existing = store
        .branches
        .get(branch)
        .map(|b| b.comments.clone())
        .unwrap_or_default();
    reanchor_pass(repo, dir, branch, range.as_deref(), &mut existing);

    // The new comment's anchor is resolved against the stored range: line found →
    // context captured, not orphaned; range resolves but the line is absent (file
    // gone/binary/no such line) → orphaned honestly with no context; range
    // unresolvable or absent → context null and *not* orphaned (unknown, not
    // orphaned — plan §3.2/§3.3).
    let (context, orphaned) = match range.as_deref() {
        None => (None, false),
        Some(range_str) => match capture_context(repo, range_str, file, side, line) {
            Ok(Some(text)) => (Some(text), false),
            Ok(None) => (None, true),
            Err(_) => (None, false),
        },
    };

    let created_at = comments::now_secs();
    let comment = comments::mutate(dir, |store| {
        let id = store.take_id();
        let comment = Comment {
            id,
            source: Source::Agent,
            file: file.to_string(),
            side,
            line,
            text: text.to_string(),
            context,
            orphaned,
            created_at,
        };
        store
            .branches
            .entry(branch.to_string())
            .or_default()
            .comments
            .push(comment.clone());
        comment
    })?;

    if json_out {
        print_json(&json!({ "comment": comment }));
    } else {
        println!("{}", comment.id);
    }
    Ok(())
}

fn rm(dir: &Path, branch: &str, id: u64, json_out: bool) -> Result<()> {
    // Detect a missing id from a fresh read *before* mutating: an unknown id must
    // surface "not found on branch <key>", not a spurious write error under an
    // unwritable store, and must not touch the file at all (plan §3.3).
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

fn clear(dir: &Path, branch: &str, json_out: bool) -> Result<()> {
    let cleared = comments::mutate(dir, |store| match store.branches.get_mut(branch) {
        Some(entry) => {
            let n = entry.comments.len();
            entry.comments.clear();
            n
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
    // comments (plan §3.1).
    live.insert(repo.head_branch_key()?);
    let result = comments::mutate(dir, |store| {
        comments::gc(store, &live, |key| repo.commit_exists(key))
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

/// The shared best-effort re-anchor pass for `list` and `add` (plan §3.2c): when
/// the stored `range` resolves, re-anchor `comments` in place and persist if
/// anything changed. A range that no longer resolves serves the persisted state
/// (warning on stderr); a persist failure never fails the caller (warning only).
fn reanchor_pass(
    repo: &Repo,
    dir: &Path,
    branch: &str,
    range: Option<&str>,
    comments: &mut [Comment],
) {
    let Some(range_str) = range else { return };
    match reanchor(repo, range_str, comments) {
        Ok(true) => {
            if let Err(err) = persist_reanchor(dir, branch, comments) {
                eprintln!("strix: warning: could not persist re-anchored comments: {err:#}");
            }
        }
        Ok(false) => {}
        Err(err) => {
            eprintln!("strix: warning: could not re-anchor against range '{range_str}': {err:#}");
        }
    }
}

/// Run the re-anchor pass against the resolved `range`, returning whether any
/// comment moved or orphaned. Errors only when the range can't be resolved.
fn reanchor(repo: &Repo, range: &str, comments: &mut [Comment]) -> Result<bool> {
    let spec = repo.resolve_range(range)?;
    let files = repo.range_files(&spec)?;
    Ok(comments::reanchor(comments, &files, |file| {
        repo.range_file_diff(&spec, file)
    }))
}

/// Persist re-anchored comments for `branch` — a fresh read-modify-write, so a
/// concurrent agent edit is read before we overwrite this branch's set.
fn persist_reanchor(dir: &Path, branch: &str, comments: &[Comment]) -> Result<()> {
    comments::mutate(dir, |store| {
        if let Some(entry) = store.branches.get_mut(branch) {
            entry.comments = comments.to_vec();
        }
    })
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
    let spec = repo.resolve_range(range)?;
    let files = repo.range_files(&spec)?;
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

/// The plain-text table: `⚠` marks orphans, columns aligned. An empty set prints
/// nothing (exit 0).
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
        let first = comment.text.lines().next().unwrap_or("");
        println!("{mark} {:>id_w$}  {:<loc_w$}  {first}", comment.id, loc,);
    }
}

fn location(comment: &Comment) -> String {
    let side = match comment.side {
        Side::Old => "old",
        Side::New => "new",
    };
    format!("{}:{side}:{}", comment.file, comment.line)
}
