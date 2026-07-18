//! Branch-to-branch review: resolving a range spec into a concrete base/head
//! commit pair and listing the files (with line stats) that differ between them.
//!
//! Range grammar (see the plan §3.1): split on the first `...` else the first
//! `..`; an empty side means `HEAD`; a bare rev `BASE` means
//! `merge-base(BASE, HEAD)..HEAD` (three-dot / GitHub-PR semantics). Operands are
//! peeled through annotated tags to a commit.
//!
//! Like the rest of the git layer, the file *list* is read via `git diff-tree`
//! (`--name-status` for change kinds joined with `--numstat` for +/- counts) and
//! diff *content* is computed lazily, per selected file, over blob bytes. Listing
//! never diffs in-process: a branch range can span hundreds of files and the list
//! is rebuilt on every real refresh.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use gix::bstr::BStr;
use gix::ObjectId;

use crate::git::history::parse_name_status;
use crate::git::{ChangeKind, CommitFile, CommitStat, FileDiff, Repo};

/// A resolved review range: two concrete commits plus the strings used to build
/// and label it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSpec {
    /// The exact user input, re-used verbatim on refresh.
    pub input: String,
    /// The normalized header label (e.g. `main…HEAD`).
    pub display: String,
    pub base: ObjectId,
    pub head: ObjectId,
}

impl Repo {
    /// Resolve a range spec per the grammar above into a concrete `ReviewSpec`.
    ///
    /// Errors are contextual: they name the offending operand and distinguish an
    /// unknown revision, a revision that isn't a commit, and the absence of a
    /// merge base.
    pub fn resolve_range(&self, input: &str) -> Result<ReviewSpec> {
        if input.trim().is_empty() {
            return Err(anyhow!("empty revision range"));
        }

        // `...` is checked before `..` so `A...B` never parses as `A.`/`.B`.
        if let Some(idx) = input.find("...") {
            let a_op = operand(&input[..idx]);
            let b_op = operand(&input[idx + 3..]);
            let a = self.resolve_commit(a_op)?;
            let b = self.resolve_commit(b_op)?;
            let base = self.merge_base_of(a, b, a_op, b_op)?;
            return Ok(ReviewSpec {
                input: input.to_string(),
                display: format!("{a_op}…{b_op}"),
                base,
                head: b,
            });
        }

        if let Some(idx) = input.find("..") {
            let a_op = operand(&input[..idx]);
            let b_op = operand(&input[idx + 2..]);
            let base = self.resolve_commit(a_op)?;
            let head = self.resolve_commit(b_op)?;
            return Ok(ReviewSpec {
                input: input.to_string(),
                display: format!("{a_op}..{b_op}"),
                base,
                head,
            });
        }

        let single = input.trim();
        let a = self.resolve_commit(single)?;
        let head = self.resolve_commit("HEAD")?;
        let base = self.merge_base_of(a, head, single, "HEAD")?;
        Ok(ReviewSpec {
            input: input.to_string(),
            display: format!("{single}…HEAD"),
            base,
            head,
        })
    }

    /// The files that differ between `spec.base` and `spec.head`, with +/- counts.
    ///
    /// Two `git diff-tree` passes joined by path: `--name-status` for the change
    /// kind (rename source included) and `--numstat` for line counts (a `-` count
    /// marks a binary change). No in-process diffing while listing.
    pub fn range_files(&self, spec: &ReviewSpec) -> Result<Vec<CommitFile>> {
        let base = spec.base.to_string();
        let head = spec.head.to_string();
        let name_status = self.run(&[
            "diff-tree",
            "--no-commit-id",
            "-r",
            "-M",
            "-z",
            "--name-status",
            &base,
            &head,
        ])?;
        let numstat = self.run(&[
            "diff-tree",
            "--no-commit-id",
            "-r",
            "-M",
            "-z",
            "--numstat",
            &base,
            &head,
        ])?;
        let stats = parse_numstat(&numstat);

        let files = parse_name_status(&name_status)
            .into_iter()
            .map(|(change, path, orig_path)| {
                let stat = stats.get(&path).copied().unwrap_or_default();
                CommitFile {
                    path,
                    orig_path,
                    change,
                    stat,
                }
            })
            .collect();
        Ok(files)
    }

    /// The diff for one range file, computed lazily over blob bytes. Mirrors the
    /// spec-building in `history::diff_specs`: an addition has no base side, a
    /// deletion no head side, a rename reads the base blob at its old path.
    pub fn range_file_diff(&self, spec: &ReviewSpec, file: &CommitFile) -> FileDiff {
        let old_path = file.orig_path.as_deref().unwrap_or(&file.path);
        let old = match file.change {
            ChangeKind::Added => String::new(),
            _ => format!("{}:{old_path}", spec.base),
        };
        let new = match file.change {
            ChangeKind::Deleted => String::new(),
            _ => format!("{}:{}", spec.head, file.path),
        };
        self.file_diff_from_specs(&old, &new)
    }

    /// Resolve one operand to a commit oid, peeling annotated tags. Distinguishes
    /// an unknown revision from a resolvable non-commit.
    fn resolve_commit(&self, operand: &str) -> Result<ObjectId> {
        let id = self
            .gix()
            .rev_parse_single(BStr::new(operand))
            .map_err(|e| anyhow!("unknown revision '{operand}': {e}"))?;
        let object = id
            .object()
            .map_err(|e| anyhow!("couldn't read object for '{operand}': {e}"))?;
        let peeled = object
            .peel_tags_to_end()
            .map_err(|e| anyhow!("couldn't peel tags for '{operand}': {e}"))?;
        if peeled.kind != gix::object::Kind::Commit {
            return Err(anyhow!("'{operand}' is not a commit"));
        }
        Ok(peeled.id)
    }

    fn merge_base_of(&self, a: ObjectId, b: ObjectId, a_op: &str, b_op: &str) -> Result<ObjectId> {
        use gix::repository::merge_base::Error;
        self.gix()
            .merge_base(a, b)
            .map(|id| id.detach())
            .map_err(|e| match e {
                Error::NotFound { .. } => anyhow!("no merge base between '{a_op}' and '{b_op}'"),
                other => anyhow!("finding the merge base of '{a_op}' and '{b_op}': {other}"),
            })
    }
}

/// An empty range side means `HEAD` (git semantics: `main..` ≡ `main..HEAD`).
fn operand(side: &str) -> &str {
    if side.is_empty() {
        "HEAD"
    } else {
        side
    }
}

/// Parse `git diff-tree -z --numstat` into per-path stats keyed by the new path.
///
/// Records are NUL-separated `added\tdeleted\t<path>`; a `-` count marks a binary
/// change. For a rename/copy the path portion is empty and the two following
/// NUL fields are the old then new path (we key on the new path, matching
/// `CommitFile::path`).
fn parse_numstat(bytes: &[u8]) -> HashMap<String, CommitStat> {
    let mut out = HashMap::new();
    let mut fields = bytes.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(field) = fields.next() {
        let record = String::from_utf8_lossy(field);
        let mut parts = record.splitn(3, '\t');
        let added = parts.next().unwrap_or("");
        let deleted = parts.next().unwrap_or("");
        let path_part = parts.next().unwrap_or("");
        let stat = CommitStat {
            added: added.parse().unwrap_or(0),
            deleted: deleted.parse().unwrap_or(0),
            binary: added == "-" || deleted == "-",
        };
        let path = if path_part.is_empty() {
            // Rename/copy: consume old then new path; key on the new path.
            let _old = fields.next();
            match fields.next() {
                Some(new) => String::from_utf8_lossy(new).into_owned(),
                None => break,
            }
        } else {
            path_part.to_string()
        };
        out.insert(path, stat);
    }
    out
}
