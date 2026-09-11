//! Commit history: walking the current branch (HEAD ancestry, including merges),
//! listing a commit's changed files, and diffing a file at a commit against its
//! first parent.
//!
//! Splits work the way the rest of the git layer does (see CLAUDE.md): the commit
//! walk, commit metadata, and refs come from **gix** (object/ref discovery); the
//! per-commit changed-file *list* comes from two `git diff-tree` passes
//! (`--name-status` joined with `--numstat` by path, the same
//! ergonomics-driven CLI fallback `status` uses); and diff *content* reuses the
//! in-process `similar` path over blob bytes.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::git::{FileDiff, Repo};

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
///
/// The declared order is also the badge order (`Repo::history_key` sorts by it),
/// so named refs read before the `HEAD` marker on a graph row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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

/// Everything [`Repo::history`] and its badges depend on. Two equal keys mean a
/// re-walk would return the same commits with the same labels, so a refresh can
/// skip it (plan 003 §3.4).
///
/// `refs/replace` and grafts are deliberately absent: unsupported here, and they
/// self-heal on the next commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryKey {
    pub head: gix::ObjectId,
    /// Sorted, so two reads compare element-wise — gix's ref iteration order is
    /// not guaranteed stable. The sort runs over the whole record, so refs
    /// sharing a name can't order ambiguously, and it leads with the kind
    /// because this vector is also what badges the graph.
    pub refs: Vec<RefLabel>,
    /// The shallow boundary, `None` in a full clone. Deepening a shallow clone
    /// lengthens the walk without moving a single ref.
    pub shallow: Option<Vec<gix::ObjectId>>,
}

/// Sort by `(kind, name, target)`, kind first, so named refs read before the
/// `HEAD` marker on a graph row and two reads compare element-wise regardless
/// of gix's ref iteration order.
fn sort_ref_labels(refs: &mut [RefLabel]) {
    refs.sort_by(|a, b| (&a.kind, &a.name, &a.target).cmp(&(&b.kind, &b.name, &b.target)));
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

        self.history_walk_count
            .set(self.history_walk_count.get() + 1);
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

    /// Read the current [`HistoryKey`] — HEAD, the sorted refs, and the shallow
    /// boundary — resolving HEAD once and reusing it for both the key and its
    /// synthetic `HEAD` ref label.
    ///
    /// Errors on an unborn HEAD or an unreadable `shallow` file; the caller
    /// treats that as "no key" and walks unconditionally.
    pub fn history_key(&self) -> Result<HistoryKey> {
        let head = self.gix().head_id().context("no commits yet")?.detach();
        let mut refs = self.local_branch_labels()?;
        refs.push(RefLabel {
            name: "HEAD".to_string(),
            target: head,
            kind: RefKind::Head,
        });
        sort_ref_labels(&mut refs);
        let shallow = self
            .gix()
            .shallow_commits()
            .context("reading the shallow boundary")?
            .map(|boundary| boundary.iter().copied().collect());
        Ok(HistoryKey {
            head,
            refs,
            shallow,
        })
    }

    /// Refs pointing into history, for graph badges. Current-branch scope only
    /// shows labels whose target is in the walked set; the renderer filters.
    /// Sorted by `(kind, name, target)` so badge order agrees with
    /// [`Repo::history_key`] regardless of gix's ref iteration order.
    pub fn ref_labels(&self) -> Result<Vec<RefLabel>> {
        let mut out = self.local_branch_labels()?;
        if let Ok(head) = self.gix().head_id() {
            out.push(RefLabel {
                name: "HEAD".to_string(),
                target: head.detach(),
                kind: RefKind::Head,
            });
        }
        sort_ref_labels(&mut out);
        Ok(out)
    }

    /// Local-branch ref labels, in gix's (unordered) iteration order — the part
    /// [`Repo::ref_labels`] and [`Repo::history_key`] share, each adding its own
    /// already-resolved `HEAD` entry.
    fn local_branch_labels(&self) -> Result<Vec<RefLabel>> {
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
        Ok(out)
    }

    /// The files changed in `commit` relative to its first parent (root commit:
    /// relative to the empty tree). See [`Repo::diff_tree_files`].
    pub fn commit_files(&self, commit: &CommitInfo) -> Result<Vec<CommitFile>> {
        let id = commit.id.to_string();
        match commit.first_parent() {
            Some(parent) => self.diff_tree_files(&[&parent.to_string(), &id]),
            None => self.diff_tree_files(&["--root", &id]),
        }
    }

    /// The files that differ between the trees `revs` name — `[base, head]`, or
    /// `["--root", commit]` for a parentless commit — with +/- counts. Shared by
    /// the history and review listings.
    ///
    /// Two `git diff-tree` passes joined by path: `--name-status` for the change
    /// kind (rename source included) and `--numstat` for line counts (a `-` count
    /// marks a binary change). No in-process diffing while listing: a branch
    /// range can span hundreds of files, and a commit's list is rebuilt on every
    /// selection.
    pub(crate) fn diff_tree_files(&self, revs: &[&str]) -> Result<Vec<CommitFile>> {
        let name_status = self.diff_tree(revs, "--name-status")?;
        let numstat = self.diff_tree(revs, "--numstat")?;
        let stats = parse_numstat(&numstat);
        Ok(parse_name_status(&name_status)
            .into_iter()
            .map(|(change, path, orig_path)| CommitFile {
                stat: stats.get(&path).copied().unwrap_or_default(),
                path,
                orig_path,
                change,
            })
            .collect())
    }

    fn diff_tree(&self, revs: &[&str], format: &str) -> Result<Vec<u8>> {
        let mut args = vec!["diff-tree", "--no-commit-id", "-r", "-M", "-z", format];
        args.extend_from_slice(revs);
        self.run(&args)
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
