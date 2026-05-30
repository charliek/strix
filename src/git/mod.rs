pub mod diff;
pub mod ops;
pub mod status;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

pub use diff::{DiffLine, FileDiff, LineKind};
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
