pub mod diff;
pub mod history;
pub mod ops;
pub mod review;
pub mod status;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

pub use diff::{DiffLine, FileDiff, LineKind};
pub use history::{ChangeKind, CommitFile, CommitInfo, CommitStat, RefKind, RefLabel};
pub use review::ReviewSpec;
pub use status::{Change, FileEntry, Section, Status};

/// A git repository. Object reads (diffs, blobs) go through the in-process
/// [gix](https://github.com/GitoxideLabs/gitoxide) handle; status reads and
/// mutations shell out to `git` (see CLAUDE.md for the rationale).
pub struct Repo {
    gix: gix::Repository,
    workdir: PathBuf,
}

impl Repo {
    /// Open the repository containing `path` (walks upward to find `.git`).
    pub fn open(path: &Path) -> Result<Repo> {
        let gix = gix::discover(path)
            .with_context(|| format!("{} is not inside a git repository", path.display()))?;
        let workdir = gix
            .workdir()
            .map(Path::to_path_buf)
            .context("bare repositories are not supported")?;
        Ok(Repo { gix, workdir })
    }

    /// The working-tree root.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// The directory holding strix's per-repository state (`<common_dir>/strix`).
    ///
    /// `common_dir` (not `git_dir`) is deliberate: a linked worktree's `git_dir`
    /// is private to that worktree, but its `common_dir` points back at the main
    /// repository's `.git`, so every checkout — primary or linked — resolves to
    /// the *same* comments store.
    pub fn strix_dir(&self) -> PathBuf {
        self.common_dir().join("strix")
    }

    /// This checkout's private git-admin dir: `<workdir>/.git` for a primary
    /// checkout, or `<common_dir>/worktrees/<name>` for a linked worktree (whose
    /// per-worktree `HEAD`, `index`, and reflog live here).
    pub fn git_dir(&self) -> PathBuf {
        self.gix.git_dir().to_owned()
    }

    /// The shared common dir (`<main>/.git`): where `refs/*`, `HEAD`,
    /// `packed-refs`, and the comments store live for every linked checkout.
    pub fn common_dir(&self) -> PathBuf {
        self.gix.common_dir().to_owned()
    }

    /// Directories *outside* the working tree that must also be watched so a
    /// stage / commit / ref-advance in this checkout still wakes the TUI, with
    /// any candidate already covered by the recursive workdir watch — or nested
    /// under a broader root already in the result — dropped as redundant.
    ///
    /// In a linked worktree the private git dir and the shared common dir both
    /// lie outside `workdir` (its `.git` is a file pointing elsewhere), so the
    /// recursive workdir watch alone never sees them; the result is the common
    /// dir, which subsumes both the per-worktree git dir and the store. In a
    /// primary checkout every candidate is under `.git` inside `workdir`, so the
    /// result is empty.
    pub fn watch_roots(&self) -> Vec<PathBuf> {
        // Broadest-first, so a candidate nested under an already-kept root drops.
        let mut roots: Vec<PathBuf> = Vec::new();
        for root in [self.common_dir(), self.git_dir(), self.strix_dir()] {
            let covered =
                root.starts_with(&self.workdir) || roots.iter().any(|kept| root.starts_with(kept));
            if !covered {
                roots.push(root);
            }
        }
        roots
    }

    /// The key identifying the current comment inbox: the checked-out branch's
    /// short name (works for an unborn HEAD too, whose symbolic ref names a
    /// branch that has no commit yet), or — when HEAD is detached — the full
    /// commit hex.
    pub fn head_branch_key(&self) -> Result<String> {
        match self.gix.head_name().context("reading HEAD")? {
            Some(name) => Ok(String::from_utf8_lossy(name.shorten()).into_owned()),
            None => {
                let id = self.gix.head_id().context("resolving detached HEAD")?;
                Ok(id.detach().to_string())
            }
        }
    }

    /// The short names of every local branch (`refs/heads/*`), for GC.
    pub fn branch_names(&self) -> Result<Vec<String>> {
        let refs = self.gix.references().context("opening refs")?;
        let mut names = Vec::new();
        for branch in refs.local_branches().context("listing local branches")? {
            let branch = branch
                .map_err(anyhow::Error::from_boxed)
                .context("iterating local branches")?;
            names.push(String::from_utf8_lossy(branch.name().shorten()).into_owned());
        }
        Ok(names)
    }

    /// The underlying gitoxide handle, used for object reads.
    pub fn gix(&self) -> &gix::Repository {
        &self.gix
    }

    /// Whether `key` (a rev-spec, e.g. a detached-HEAD commit hex) still resolves
    /// to an object in this repository — the liveness test GC uses for
    /// commit-keyed inboxes.
    pub fn commit_exists(&self, key: &str) -> bool {
        self.gix.rev_parse_single(gix::bstr::BStr::new(key)).is_ok()
    }

    /// Read the staged / unstaged / untracked file lists and current branch.
    ///
    /// `--no-optional-locks` keeps this read from refreshing/rewriting
    /// `.git/index`, which would otherwise trip the auto-refresh file watcher
    /// and loop (status → index write → watch event → status …).
    pub fn status(&self) -> Result<Status> {
        let stdout = self.run(&[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
        ])?;
        Ok(status::parse(&stdout))
    }

    /// Run a `git` subcommand in the working directory, returning its stdout.
    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.workdir)
            .args(args)
            .output()
            .with_context(|| format!("failed to spawn `git {}`", args.join(" ")))?;
        if !output.status.success() {
            anyhow::bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    }
}
