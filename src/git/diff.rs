//! Diff computation. Blob contents come from gix (HEAD via `HEAD:path`, the
//! index via `:path`) and the working tree from disk; the line diff is computed
//! in-process with [`similar`], producing a structured model that drives both
//! unified and (later) side-by-side rendering.

use gix::bstr::BStr;
use similar::{ChangeTag, DiffOp, TextDiff};

use crate::git::{Change, FileEntry, Repo, Section};

const CONTEXT_LINES: usize = 3;
const BINARY_SCAN_BYTES: usize = 8000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
    Hunk,
}

/// One rendered diff row: a context/added/removed line, or a `@@` hunk header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileDiff {
    Text(Vec<DiffLine>),
    Binary,
}

impl Repo {
    /// Compute the diff for `entry` as shown in `section`: staged files diff
    /// HEAD against the index; unstaged files diff the index (or nothing, for
    /// an untracked file) against the working tree.
    pub fn diff(&self, section: Section, entry: &FileEntry) -> FileDiff {
        let (old, new) = self.diff_sides(section, entry);
        if is_binary(&old) || is_binary(&new) {
            return FileDiff::Binary;
        }
        let old = String::from_utf8_lossy(&old);
        let new = String::from_utf8_lossy(&new);
        FileDiff::Text(diff_lines(&old, &new))
    }

    fn diff_sides(&self, section: Section, entry: &FileEntry) -> (Vec<u8>, Vec<u8>) {
        match section {
            Section::Staged => {
                let head_path = entry.orig_path.as_deref().unwrap_or(&entry.path);
                (
                    self.object_bytes(&format!("HEAD:{head_path}")),
                    self.object_bytes(&format!(":{}", entry.path)),
                )
            }
            Section::Unstaged => match entry.change {
                Change::Untracked => (Vec::new(), self.worktree_bytes(&entry.path)),
                Change::Deleted => (self.object_bytes(&format!(":{}", entry.path)), Vec::new()),
                _ => (
                    self.object_bytes(&format!(":{}", entry.path)),
                    self.worktree_bytes(&entry.path),
                ),
            },
        }
    }

    /// Bytes of an object addressed by a revspec (`HEAD:path`, `:path`), or
    /// empty if it doesn't resolve — e.g. a newly added file has no HEAD blob.
    fn object_bytes(&self, spec: &str) -> Vec<u8> {
        self.gix()
            .rev_parse_single(BStr::new(spec))
            .ok()
            .and_then(|id| id.object().ok())
            .map(|object| object.detach().data)
            .unwrap_or_default()
    }

    fn worktree_bytes(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.workdir().join(path)).unwrap_or_default()
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SCAN_BYTES).any(|&b| b == 0)
}

fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    for group in diff.grouped_ops(CONTEXT_LINES) {
        lines.push(hunk_header(&group));
        for op in &group {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => LineKind::Context,
                    ChangeTag::Delete => LineKind::Deletion,
                    ChangeTag::Insert => LineKind::Addition,
                };
                lines.push(DiffLine {
                    kind,
                    old_no: change.old_index().map(|i| i + 1),
                    new_no: change.new_index().map(|i| i + 1),
                    text: change.value().trim_end_matches(['\n', '\r']).to_string(),
                });
            }
        }
    }
    lines
}

fn hunk_header(group: &[DiffOp]) -> DiffLine {
    let old = span(group, DiffOp::old_range);
    let new = span(group, DiffOp::new_range);
    DiffLine {
        kind: LineKind::Hunk,
        old_no: None,
        new_no: None,
        text: format!(
            "@@ -{},{} +{},{} @@",
            old.start + 1,
            old.end - old.start,
            new.start + 1,
            new.end - new.start
        ),
    }
}

/// The combined start..end range of a hunk on one side, in a single pass.
fn span(group: &[DiffOp], side: fn(&DiffOp) -> std::ops::Range<usize>) -> std::ops::Range<usize> {
    group
        .iter()
        .map(side)
        .reduce(|acc, r| acc.start.min(r.start)..acc.end.max(r.end))
        .unwrap_or(0..0)
}
