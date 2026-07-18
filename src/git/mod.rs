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
        self.gix.common_dir().join("strix")
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
