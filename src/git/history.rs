//! Commit history: walking the current branch (HEAD ancestry, including merges),
//! listing a commit's changed files, and diffing a file at a commit against its
//! first parent.
//!
//! Splits work the way the rest of the git layer does (see CLAUDE.md): the commit
//! walk, commit metadata, and refs come from **gix** (object/ref discovery); the
//! per-commit changed-file *list* comes from `git diff-tree` (the same
//! ergonomics-driven CLI fallback `status` uses); and diff *content* + line stats
//! reuse the in-process `similar` path over blob bytes.

use anyhow::{Context, Result};

use crate::git::diff::{diff_lines, is_binary};
use crate::git::{FileDiff, LineKind, Repo};

fn bstr_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// How a file changed in a commit, relative to its first parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
    TypeChange,
}

impl ChangeKind {
    /// The single-character marker shown next to the file (matches `git::Change`).
    pub fn marker(self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Deleted => 'D',
            ChangeKind::Modified => 'M',
            ChangeKind::Renamed => 'R',
            ChangeKind::Copied => 'C',
            ChangeKind::TypeChange => 'T',
        }
    }

    fn from_status(code: &str) -> Option<ChangeKind> {
        match code.chars().next()? {
            'A' => Some(ChangeKind::Added),
            'D' => Some(ChangeKind::Deleted),
            'M' => Some(ChangeKind::Modified),
            'R' => Some(ChangeKind::Renamed),
            'C' => Some(ChangeKind::Copied),
            'T' => Some(ChangeKind::TypeChange),
            _ => None,
        }
    }
}

/// One commit in the log, with the metadata the history view needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: gix::ObjectId,
    pub short: String,
    pub summary: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub author_seconds: i64,
    pub author_offset: i32,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_seconds: i64,
    pub committer_offset: i32,
    /// Parent oids; `parents[0]` is the first parent (the merge "mainline").
    pub parents: Vec<gix::ObjectId>,
    pub tree: gix::ObjectId,
}

impl CommitInfo {
    pub fn first_parent(&self) -> Option<&gix::ObjectId> {
        self.parents.first()
    }
}

/// Line counts for a single file's change in a commit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitStat {
    pub added: usize,
    pub deleted: usize,
    pub binary: bool,
}

/// A file changed in a commit, relative to its first parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFile {
    pub path: String,
    /// Rename/copy source path (for `R`/`C`), else `None`.
    pub orig_path: Option<String>,
    pub change: ChangeKind,
    pub stat: CommitStat,
}

impl CommitFile {
    /// How the file is labelled in the list (rename shows `orig → path`).
    pub fn display_path(&self) -> String {
        match &self.orig_path {
            Some(orig) => format!("{orig} → {}", self.path),
            None => self.path.clone(),
        }
    }
}

/// The kind of ref pointing at a commit, for graph labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    LocalBranch,
    RemoteBranch,
    Tag,
    Head,
}

/// A ref pointing at a commit (used to badge graph rows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefLabel {
    pub name: String,
    pub target: gix::ObjectId,
    pub kind: RefKind,
}

impl Repo {
    /// Walk the current branch's history (HEAD ancestry, full DAG so merges and
    /// their merged-in commits appear), newest first, up to `limit` commits.
    ///
    /// Errors on an unborn HEAD (empty repo); the caller treats that as "no
    /// history". The walk decodes commit objects only — no trees or blobs — so it
    /// stays fast on deep history.
    pub fn history(&self, limit: usize) -> Result<Vec<CommitInfo>> {
        use gix::revision::walk::Sorting;
        use gix::traverse::commit::simple::CommitTimeOrder;

        let head = self.gix().head_id().context("no commits yet")?;
        let walk = self
            .gix()
            .rev_walk(std::iter::once(head.detach()))
            // Full topology (not first-parent) so the rail graph can show merges.
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
            .all()
            .context("walking commit history")?;

        let mut out = Vec::with_capacity(limit.min(4096));
        for info in walk {
            let info = info.context("reading a commit during the walk")?;
            let commit = info.object().context("loading a commit object")?;
            out.push(decode_commit(&commit)?);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Refs pointing into history, for graph badges. Current-branch scope only
    /// shows labels whose target is in the walked set; the renderer filters.
    pub fn ref_labels(&self) -> Result<Vec<RefLabel>> {
        let mut out = Vec::new();
        let refs = self.gix().references().context("opening refs")?;
        for branch in refs.local_branches().context("listing local branches")? {
            let branch = match branch {
                Ok(branch) => branch,
                Err(_) => continue,
            };
            let name = bstr_string(branch.name().shorten());
            out.push(RefLabel {
                name,
                target: branch.id().detach(),
                kind: RefKind::LocalBranch,
            });
        }
        if let Ok(head) = self.gix().head_id() {
            out.push(RefLabel {
                name: "HEAD".to_string(),
                target: head.detach(),
                kind: RefKind::Head,
            });
        }
        Ok(out)
    }

    /// The files changed in `commit` relative to its first parent (root commit:
    /// relative to the empty tree). Listed via `git diff-tree`; line stats are
    /// computed in-process.
    pub fn commit_files(&self, commit: &CommitInfo) -> Result<Vec<CommitFile>> {
        let id = commit.id.to_string();
        let stdout = match commit.first_parent() {
            Some(parent) => self.run(&[
                "diff-tree",
                "--no-commit-id",
                "-r",
                "-M",
                "-z",
                "--name-status",
                &parent.to_string(),
                &id,
            ])?,
            None => self.run(&[
                "diff-tree",
                "--no-commit-id",
                "--root",
                "-r",
                "-M",
                "-z",
                "--name-status",
                &id,
            ])?,
        };

        let mut files = Vec::new();
        for (change, path, orig_path) in parse_name_status(&stdout) {
            let (old_spec, new_spec) = self.diff_specs(commit, &path, orig_path.as_deref(), change);
            let stat = stat_of(&self.file_diff_from_specs(&old_spec, &new_spec));
            files.push(CommitFile {
                path,
                orig_path,
                change,
                stat,
            });
        }
        Ok(files)
    }

    /// The diff for one of a commit's files, against its first parent. Reuses the
    /// in-process `similar` path over blob bytes.
    pub fn commit_file_diff(&self, commit: &CommitInfo, file: &CommitFile) -> FileDiff {
        let (old_spec, new_spec) =
            self.diff_specs(commit, &file.path, file.orig_path.as_deref(), file.change);
        self.file_diff_from_specs(&old_spec, &new_spec)
    }

    /// The `<rev>:<path>` specs (old, new) for a file's change. Empty string means
    /// "no side" (an addition's old / a deletion's new) and resolves to no bytes.
    fn diff_specs(
        &self,
        commit: &CommitInfo,
        path: &str,
        orig_path: Option<&str>,
        change: ChangeKind,
    ) -> (String, String) {
        let parent = commit.first_parent();
        let old_path = orig_path.unwrap_or(path);
        let old = match (change, parent) {
            (ChangeKind::Added, _) | (_, None) => String::new(),
            (_, Some(p)) => format!("{p}:{old_path}"),
        };
        let new = match change {
            ChangeKind::Deleted => String::new(),
            _ => format!("{}:{path}", commit.id),
        };
        (old, new)
    }

    fn file_diff_from_specs(&self, old_spec: &str, new_spec: &str) -> FileDiff {
        let old = self.object_bytes(old_spec);
        let new = self.object_bytes(new_spec);
        if is_binary(&old) || is_binary(&new) {
            return FileDiff::Binary;
        }
        FileDiff::Text(diff_lines(
            &String::from_utf8_lossy(&old),
            &String::from_utf8_lossy(&new),
        ))
    }
}

/// The +/- line counts for an already-computed diff (so the file list and the
/// detailed view never diff the same blobs twice).
fn stat_of(diff: &FileDiff) -> CommitStat {
    match diff {
        FileDiff::Binary => CommitStat {
            binary: true,
            ..CommitStat::default()
        },
        FileDiff::Text(lines) => {
            let mut stat = CommitStat::default();
            for line in lines {
                match line.kind {
                    LineKind::Addition => stat.added += 1,
                    LineKind::Deletion => stat.deleted += 1,
                    _ => {}
                }
            }
            stat
        }
    }
}

fn decode_commit(commit: &gix::Commit<'_>) -> Result<CommitInfo> {
    let id = commit.id().detach();
    let short = commit
        .short_id()
        .map(|prefix| prefix.to_string())
        .unwrap_or_else(|_| id.to_string()[..7.min(id.to_string().len())].to_string());
    // gix keeps the title's trailing newline; trim it for clean one-line display.
    let summary = commit
        .message()
        .map(|m| bstr_string(m.title).trim_end().to_string())
        .unwrap_or_default();
    let message = bstr_string(commit.message_raw_sloppy());
    let author = commit.author().context("reading commit author")?;
    let committer = commit.committer().context("reading commit committer")?;
    // `SignatureRef::time` is the raw git time string; `.time()` parses it.
    let author_time = author.time().unwrap_or_default();
    let committer_time = committer.time().unwrap_or_default();
    let parents = commit.parent_ids().map(|id| id.detach()).collect();
    let tree = commit.tree_id().context("reading commit tree")?.detach();

    Ok(CommitInfo {
        id,
        short,
        summary,
        message,
        author_name: bstr_string(author.name),
        author_email: bstr_string(author.email),
        author_seconds: author_time.seconds,
        author_offset: author_time.offset,
        committer_name: bstr_string(committer.name),
        committer_email: bstr_string(committer.email),
        committer_seconds: committer_time.seconds,
        committer_offset: committer_time.offset,
        parents,
        tree,
    })
}

/// Parse `git diff-tree -z --name-status` output: NUL-separated fields where a
/// status token is followed by one path (or two, `orig` then `new`, for R/C).
fn parse_name_status(bytes: &[u8]) -> Vec<(ChangeKind, String, Option<String>)> {
    let mut out = Vec::new();
    let mut fields = bytes.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(raw_status) = fields.next() {
        let status = String::from_utf8_lossy(raw_status);
        let Some(change) = ChangeKind::from_status(&status) else {
            // Unknown status: still consume its path(s) so parsing stays aligned.
            let _ = fields.next();
            continue;
        };
        let renamed = matches!(change, ChangeKind::Renamed | ChangeKind::Copied);
        if renamed {
            let Some(orig) = fields.next() else { break };
            let Some(path) = fields.next() else { break };
            out.push((
                change,
                String::from_utf8_lossy(path).into_owned(),
                Some(String::from_utf8_lossy(orig).into_owned()),
            ));
        } else {
            let Some(path) = fields.next() else { break };
            out.push((change, String::from_utf8_lossy(path).into_owned(), None));
        }
    }
    out
}
