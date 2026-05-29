//! Mutating git operations. These shell out to `git` (via [`Repo::run`]) rather
//! than gix, per the spec's sanctioned CLI fallback — they're rare, user-driven,
//! and `git`'s porcelain handles every edge case correctly.

use anyhow::Result;

use crate::git::{Change, Repo};

impl Repo {
    /// Stage a path (`git add`). Works for modified, new, and deleted files.
    pub fn stage(&self, path: &str) -> Result<()> {
        self.run(&["add", "--", path])?;
        Ok(())
    }

    /// Unstage a path, resetting its index entry to HEAD (`git restore --staged`).
    pub fn unstage(&self, path: &str) -> Result<()> {
        self.run(&["restore", "--staged", "--", path])?;
        Ok(())
    }

    /// Discard a file's changes. For tracked files this resets both the index
    /// and the working tree to HEAD; an untracked file is deleted and a
    /// staged-new file is removed entirely. Destructive — callers confirm first.
    pub fn discard(&self, path: &str, change: Change) -> Result<()> {
        match change {
            Change::Untracked => {
                self.run(&["clean", "-f", "-d", "--", path])?;
            }
            Change::Added => {
                self.run(&["rm", "-f", "--", path])?;
            }
            _ => {
                self.run(&[
                    "restore",
                    "--source=HEAD",
                    "--staged",
                    "--worktree",
                    "--",
                    path,
                ])?;
            }
        }
        Ok(())
    }
}
