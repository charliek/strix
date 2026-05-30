//! Background filesystem watcher for auto-refresh. A debouncer coalesces bursts
//! of FS events under the repo root into infrequent signals; the main event
//! loop turns each into one `git status` refresh (see `App::reload`). The
//! watcher thread owns the debouncer (so the watch stays alive) and only sends
//! `()` — it never touches the `!Send` `App`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult};

/// How long FS-event activity must settle before a refresh signal is sent.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// Start watching `root` recursively on a background thread; returns a receiver
/// that yields `()` whenever a relevant change settles. Errors if the watch
/// can't be established, so the caller can fall back to manual refresh.
pub fn spawn(root: PathBuf) -> Result<Receiver<()>> {
    // Establish the watch on this thread so a setup failure (e.g. inotify
    // limits) surfaces to the caller rather than dying silently in a thread.
    let (debounce_tx, debounce_rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(DEBOUNCE, debounce_tx).context("create file watcher")?;
    debouncer
        .watcher()
        .watch(&root, RecursiveMode::Recursive)
        .with_context(|| format!("watch {}", root.display()))?;

    let (signal_tx, signal_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        // Hold the debouncer for the thread's lifetime; dropping it ends the watch.
        let _debouncer = debouncer;
        for batch in debounce_rx {
            let relevant = match batch {
                Ok(events) => events.iter().any(|e| is_relevant(&e.path)),
                // A watch error (e.g. a queue overflow) means we may have missed
                // changes — refresh to be safe.
                Err(_) => true,
            };
            if relevant && signal_tx.send(()).is_err() {
                break; // the main loop is gone; stop forwarding
            }
        }
    });
    Ok(signal_rx)
}

/// Whether a changed path should trigger a refresh. Git object-store churn
/// (`.git/objects/…`) is ignored as pure noise; the rest of `.git` (index,
/// HEAD, refs) and the whole worktree are relevant, so external
/// stage/commit/branch-switch are caught. Bursts are coalesced by the debounce.
fn is_relevant(path: &Path) -> bool {
    let mut comps = path.components();
    while let Some(comp) = comps.next() {
        if comp.as_os_str() == OsStr::new(".git") {
            return comps
                .next()
                .is_none_or(|next| next.as_os_str() != OsStr::new("objects"));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::is_relevant;
    use std::path::Path;

    #[test]
    fn ignores_git_object_churn_but_keeps_state_and_worktree() {
        assert!(!is_relevant(Path::new("/repo/.git/objects/ab/cdef")));
        assert!(is_relevant(Path::new("/repo/.git/index")));
        assert!(is_relevant(Path::new("/repo/.git/HEAD")));
        assert!(is_relevant(Path::new("/repo/.git/refs/heads/main")));
        assert!(is_relevant(Path::new("/repo/src/main.rs")));
    }
}
