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
use notify::event::{AccessKind, AccessMode};
use notify::{
    Event, EventHandler, EventKind, RecommendedWatcher, RecursiveMode, Watcher, WatcherKind,
};
use notify_debouncer_mini::{new_debouncer_opt, Config, DebounceEventResult};

/// How long FS-event activity must settle before a refresh signal is sent.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// Start watching `root` recursively on a background thread; returns a receiver
/// that yields `()` whenever a relevant change settles. Errors if the primary
/// watch can't be established, so the caller can fall back to manual refresh.
///
/// `extra` roots are each created (so notify can bind a not-yet-existent store
/// dir) then watched best-effort (a failure logs and is skipped): in a *linked*
/// worktree the shared common dir (refs / HEAD / packed-refs + the comments
/// store) and the private git dir lie outside `root`, so watching them makes an
/// in-worktree commit / ref-advance — and an agent's CLI store write — refresh
/// the TUI there too (plan §2). Recursive dir watches (not watches on the
/// atomically-renamed ref files themselves) are what catch a ref advance
/// reliably; `.git/objects` churn is still filtered in [`is_relevant`].
pub fn spawn(root: PathBuf, extra: Vec<PathBuf>) -> Result<Receiver<()>> {
    // Establish the watch on this thread so a setup failure (e.g. inotify
    // limits) surfaces to the caller rather than dying silently in a thread.
    let (debounce_tx, debounce_rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer_opt::<_, FilteredWatcher>(
        Config::default().with_timeout(DEBOUNCE),
        debounce_tx,
    )
    .context("create file watcher")?;
    debouncer
        .watcher()
        .watch(&root, RecursiveMode::Recursive)
        .with_context(|| format!("watch {}", root.display()))?;
    // Object-store churn under each extra (git-admin / common) root is noise. The
    // `.git`-component check in `is_relevant` covers a common dir literally named
    // `.git`; also filter `<root>/objects` per extra root so a common dir that is
    // *not* named `.git` (e.g. a bare `repo.git` with linked worktrees) is covered
    // too. The primary checkout passes no extras, so its behavior is unchanged.
    let object_dirs: Vec<PathBuf> = extra.iter().map(|root| root.join("objects")).collect();
    for path in extra {
        let _ = std::fs::create_dir_all(&path);
        if let Err(err) = debouncer.watcher().watch(&path, RecursiveMode::Recursive) {
            tracing::warn!("watching {} failed: {err:#}", path.display());
        }
    }

    let (signal_tx, signal_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        // Hold the debouncer for the thread's lifetime; dropping it ends the watch.
        let _debouncer = debouncer;
        for batch in debounce_rx {
            let relevant = match batch {
                Ok(events) => events
                    .iter()
                    .any(|e| is_relevant(&e.path) && !under_object_dir(&e.path, &object_dirs)),
                // A watch error (e.g. a queue overflow) means we may have missed
                // changes — refresh to be safe.
                Err(_) => true,
            };
            if relevant {
                if signal_tx.send(()).is_err() {
                    break; // the main loop is gone; stop forwarding
                }
                tracing::trace!("refresh signal");
            }
        }
    });
    Ok(signal_rx)
}

/// Whether an event kind can mean a real change, and so should reach the
/// debouncer.
///
/// An allowlist rather than a denylist, because the noise is the *default* on
/// Linux: inotify reports strix's own `.git` reads back to it as
/// `Access(Open)`, and each refresh those trigger schedules the next, so an
/// idle repo never settles. Dropping non-mutating access loses nothing, because
/// noise and signal are disjoint kinds — `Access(Close(Write))` is the
/// writer-finished signal and is disjoint from reads, which never close as
/// write. `Any` and `Other` are unknown kinds, so they are treated as
/// possibly-real.
fn forwards(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Modify(_)
            | EventKind::Remove(_)
            | EventKind::Access(AccessKind::Close(AccessMode::Write))
            | EventKind::Any
            | EventKind::Other
    )
}

/// The platform's recommended watcher with [`forwards`] applied to every event
/// it raises. Filtering here — at the backend, before the debounce window — is
/// what stops read noise from restarting that window forever; a filter applied
/// after debouncing would still see the idle repo as "busy".
struct FilteredWatcher(RecommendedWatcher);

impl Watcher for FilteredWatcher {
    fn new<F: EventHandler>(mut handler: F, config: notify::Config) -> notify::Result<Self> {
        RecommendedWatcher::new(
            move |res: notify::Result<Event>| match res {
                Ok(event) if !forwards(&event.kind) => {}
                // An `Err` means the backend may have missed changes (e.g. a
                // queue overflow), so it is never filtered.
                res => handler.handle_event(res),
            },
            config,
        )
        .map(Self)
    }

    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        self.0.watch(path, recursive_mode)
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        self.0.unwatch(path)
    }

    fn configure(&mut self, option: notify::Config) -> notify::Result<bool> {
        self.0.configure(option)
    }

    fn kind() -> WatcherKind {
        RecommendedWatcher::kind()
    }
}

/// Whether `path` lies inside any watched root's `objects` store — object churn
/// under a common dir that isn't literally named `.git`, which the path-component
/// check in [`is_relevant`] would otherwise miss.
fn under_object_dir(path: &Path, object_dirs: &[PathBuf]) -> bool {
    object_dirs.iter().any(|dir| path.starts_with(dir))
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
    use super::{forwards, is_relevant};
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind,
        RenameMode,
    };
    use notify::EventKind;
    use std::path::Path;

    const MODES: [AccessMode; 5] = [
        AccessMode::Any,
        AccessMode::Execute,
        AccessMode::Read,
        AccessMode::Write,
        AccessMode::Other,
    ];
    const CREATES: [CreateKind; 4] = [
        CreateKind::Any,
        CreateKind::File,
        CreateKind::Folder,
        CreateKind::Other,
    ];
    const REMOVES: [RemoveKind; 4] = [
        RemoveKind::Any,
        RemoveKind::File,
        RemoveKind::Folder,
        RemoveKind::Other,
    ];
    const DATA_CHANGES: [DataChange; 4] = [
        DataChange::Any,
        DataChange::Size,
        DataChange::Content,
        DataChange::Other,
    ];
    const METADATA: [MetadataKind; 7] = [
        MetadataKind::Any,
        MetadataKind::AccessTime,
        MetadataKind::WriteTime,
        MetadataKind::Permissions,
        MetadataKind::Ownership,
        MetadataKind::Extended,
        MetadataKind::Other,
    ];
    const RENAMES: [RenameMode; 5] = [
        RenameMode::Any,
        RenameMode::To,
        RenameMode::From,
        RenameMode::Both,
        RenameMode::Other,
    ];

    /// Every `EventKind` variant (and every sub-kind of each), paired with
    /// whether it must reach the debouncer.
    fn table() -> Vec<(EventKind, bool)> {
        let mut rows = vec![
            (EventKind::Any, true),
            (EventKind::Other, true),
            (EventKind::Access(AccessKind::Any), false),
            (EventKind::Access(AccessKind::Read), false),
            (EventKind::Access(AccessKind::Other), false),
        ];
        for mode in MODES {
            rows.push((EventKind::Access(AccessKind::Open(mode)), false));
            // Only a close-after-*write* means a writer finished; every other
            // close is a read handle going away.
            rows.push((
                EventKind::Access(AccessKind::Close(mode)),
                mode == AccessMode::Write,
            ));
        }
        let mutating = CREATES
            .map(EventKind::Create)
            .into_iter()
            .chain(REMOVES.map(EventKind::Remove))
            .chain([ModifyKind::Any, ModifyKind::Other].map(EventKind::Modify))
            .chain(DATA_CHANGES.map(|data| EventKind::Modify(ModifyKind::Data(data))))
            .chain(METADATA.map(|meta| EventKind::Modify(ModifyKind::Metadata(meta))))
            .chain(RENAMES.map(|rename| EventKind::Modify(ModifyKind::Name(rename))));
        rows.extend(mutating.map(|kind| (kind, true)));
        rows
    }

    #[test]
    fn forwards_every_mutating_kind_and_drops_non_mutating_access() {
        for (kind, expected) in table() {
            assert_eq!(
                forwards(&kind),
                expected,
                "{kind:?} should {}be forwarded",
                if expected { "" } else { "not " }
            );
        }
    }

    /// The precise pair the filter exists for: a read's `Open` is dropped while
    /// a writer's `Close(Write)` — the only close inotify asks for — survives.
    #[test]
    fn drops_the_open_inotify_reports_for_our_own_reads() {
        assert!(!forwards(&EventKind::Access(AccessKind::Open(
            AccessMode::Any
        ))));
        assert!(forwards(&EventKind::Access(AccessKind::Close(
            AccessMode::Write
        ))));
    }

    #[test]
    fn ignores_git_object_churn_but_keeps_state_and_worktree() {
        assert!(!is_relevant(Path::new("/repo/.git/objects/ab/cdef")));
        assert!(is_relevant(Path::new("/repo/.git/index")));
        assert!(is_relevant(Path::new("/repo/.git/HEAD")));
        assert!(is_relevant(Path::new("/repo/.git/refs/heads/main")));
        assert!(is_relevant(Path::new("/repo/src/main.rs")));
        // The comments store under `.git/strix` must reach the TUI (agent writes).
        assert!(is_relevant(Path::new("/repo/.git/strix/comments.json")));
        // A linked worktree's ref / reflog updates live under the shared *common*
        // dir and its private git dir, outside the working tree — kept relevant,
        // while their object churn stays filtered.
        assert!(is_relevant(Path::new("/main/.git/refs/heads/side")));
        assert!(is_relevant(Path::new("/main/.git/packed-refs")));
        assert!(is_relevant(Path::new("/main/.git/worktrees/wt/logs/HEAD")));
        assert!(!is_relevant(Path::new(
            "/main/.git/objects/pack/pack-abc.idx"
        )));
    }
}
