# Architecture

strix is a single binary crate, split into a library (`src/lib.rs`) and a thin
binary (`src/main.rs`). Keeping the logic in the library makes it testable from
`tests/*_test.rs` and from the `--dump-frame` path without driving a real
terminal.

## State and the event loop

A single `App` struct holds all state: the repository, the staged / unstaged /
untracked file lists, the current selection and focus, the diff mode, and the
active theme. The loop is deliberately simple:

1. Render the current state to the screen.
2. Block for an input event (key, mouse, resize).
3. Dispatch it to `App` — which updates state.
4. Repeat.

Rendering is a pure function of state (`ui::draw`); it never mutates `App`. That
separation is what lets `dump_frame` render any state to an in-memory buffer for
assertions.

## Modules

| Module        | Responsibility                                                    |
|---------------|-------------------------------------------------------------------|
| `app`         | State + input dispatch (`on_key`, `on_mouse`); per-view dispatch. |
| `terminal`    | Setup/teardown, event loop, panic-safe restore, `dump_frame`.     |
| `ui`          | Rendering: layout, the panes, theme, syntax highlighting.         |
| `ui/review.rs`| Review view rendering: the flat changed-file list beside the shared diff pane. |
| `git`         | Repository access: status, history, diffs, mutations.             |
| `git/review.rs` | Branch-range review: `resolve_range` (gix `merge_base`), file listing via paired `git diff-tree` name-status/numstat runs, and lazy per-file diffs. |
| `graph`       | Pure rail-graph lane layout for the history view.                 |
| `config`      | Config read (`toml`) and write-back (`toml_edit`). |
| `keys`        | The configurable keymap: `Action`, default chords, `[keys]` overrides; dispatch itself lives in `App::on_key`/`App::on_mouse`. |
| `comments`    | Review-comment model, JSON store I/O, and the re-anchor engine (pure; see [comments store](#comments-store)). |
| `comments_cli`| `strix comment list\|add\|rm\|clear\|gc` — the agent-facing CLI over the same store. |

## Git layer

| Operation                     | Mechanism                                              |
|-------------------------------|--------------------------------------------------------|
| Status (staged/unstaged/untracked) | Shell out to `git status --porcelain=v2 --branch -z`, parsed in `git/status.rs`. gix *can* compute status, but its iterator API is far less ergonomic than the stable porcelain format for a read that runs once per refresh, not on the hot path. |
| Blob contents (HEAD / index / worktree) | gix object database + the working tree.        |
| Commit walk + refs (history view) | gix `rev_walk` from HEAD (full DAG), commit objects decoded only. |
| Per-commit changed files      | `git diff-tree -z --name-status` (parsed in `git/history.rs`).  |
| Branch-range resolution (review view) | gix `merge_base`, `rev_parse_single` + tag-peeling (`git/review.rs`). |
| Branch-range file listing (review view) | Two `git diff-tree -z` runs (`--name-status`, `--numstat`) joined by path — no in-process diffing while listing a range that can span hundreds of files. |
| Diff computation              | [`similar`](https://github.com/mitsuhiko/similar) over blob bytes, producing structured hunks; computed lazily, per selected file. |
| Stage / unstage / reset       | Shell out to `git` (`add`, `restore --staged`, `restore`).      |

Object reads (blobs, refs, merge-base) stay on the pure-Rust gix path for
speed; status and the range file listing shell out to `git` because the
porcelain formats they need are far more ergonomic than the equivalent gix
iterator APIs for a read that isn't on the diff hot path; the three mutations
shell out because it is reliable and sidesteps gix's less-mature index-write
porcelain. Those shell-outs are confined to `git/ops.rs` (mutations),
`git/status.rs`, and `git/review.rs`/`git/history.rs` (listings).

### Comments store

Review comments (`src/comments.rs`) live in a single JSON file at
`<common_dir>/strix/comments.json` — `Repo::strix_dir()` resolves
`gix.common_dir()`, not `git_dir()`, so a linked worktree (whose `git_dir` is
private to that checkout) and the primary checkout share exactly one file,
keyed by branch. Every mutation (TUI or CLI) is a read-modify-write: load a
fresh copy of the store, apply the change, and write atomically via the same
sibling-temp-file-then-rename helper `config::write_atomic` uses for
`config.toml` (`config.rs`, promoted `pub(crate)` and generalized to take a
target filename rather than hardcoding `config.toml`). A store that fails to
parse, or whose `version` field is newer than this build understands, is
never written to — every mutation reads first, so the never-clobber guarantee
holds for both reads and writes. A missing or zero-byte file is a valid empty
store.

**GC** runs at TUI startup (`App::build`, right after `Repo::open` and before
comments load — best-effort, never fatal) and via `strix comment gc`: it
drops inboxes keyed to a branch whose ref is gone, and detached (commit-hex)
inboxes whose commit no longer resolves, logging each drop.

**Re-anchoring** (`comments::reanchor`, a pure function over a comment slice
and the range's per-file diffs) runs on review-session open, on every
`refresh_review` (a store re-read is the *first* statement, ahead of the OID
churn guard, so an agent's `rm` shows up even when nothing else changed), and
in the CLI's `list`/`add` paths. For each comment: an exact line-number +
line-text match wins outright; failing that, a same-side line whose text
matches the stored `context` **within ±10 lines** of the stored line
re-anchors to it (closest wins, ties toward the smaller line); anything
farther, or with no match at all, is marked `orphaned` rather than silently
relocated — a `context` of `None` ("unavailable" at authoring time) always
orphans on any drift instead of content-matching. A re-anchor pass that
changes nothing skips its write, which is what keeps a live TUI's watcher
from looping (reload → re-anchor → write → reload → …).

The watcher (`watch.rs`) recursively watches the workdir as usual, plus
`Repo::strix_dir()` as an extra root — necessary because in a linked
worktree the shared store lives *outside* the watched workdir entirely, so
without that extra root an agent's CLI writes there would never trigger a
live refresh.

## Diff model

A diff is a `FileDiff` of hunks; each hunk is a list of lines tagged
`Add` / `Delete` / `Context` with their old and new line numbers. This one model
drives both unified rendering (a single column) and side-by-side rendering (old
on the left, new on the right, with blank padding to keep the two columns
aligned). Syntax highlighting is layered on top: syntect tokenizes the file
content, and the token colours are composited with the add/delete backgrounds.

The history view feeds the *same* `FileDiff` model from a different source — a
commit blob and its first-parent blob, looked up by revspec — so the diff pane
serves both views unchanged. The right pane swaps between that diff and a
`git show`-style commit-details paragraph based on the selection.

## History view

The history view is a toggleable second view (`i` / `1` / `2`) alongside the
status view. `app::ViewMode` selects which is active and splits input dispatch
(`dispatch_status` / `dispatch_history`); the diff pane and its caches are
shared via an `active_diff()` indirection. The left column splits into the
selected commit's file list and a commit-graph log, separated by a draggable
horizontal divider that mirrors the existing vertical one.

The rail graph is a **pure** transform (`src/graph.rs`): an ordered commit list
plus refs in, one render row per commit out, assigning each commit a lane and
emitting Unicode rail glyphs (`● │ ╮ ╯ ╭ ╰ ─`) connecting it to its parents.
The walk is bounded (500 commits per page, decode-only) and tree/blob reads
happen lazily for the selected commit only.

## Review view

`strix diff <RANGE>` opens a third `app::ViewMode` (`Review`) alongside
`Status` and `History`, resolved once at startup (`git/review.rs`) so a bad
range fails before the terminal takes over. It reuses the shared diff pane and
its caches; the file list is a flat `ui/review.rs` widget (no
staged/unstaged sections, since a committed range has no such split). The view
is read-only for staging — those actions are a no-op — and the file watcher
refreshes it like the other views, guarded so a worktree-only change (no new
commit on either side of the range) skips re-listing.

**Cursor and row model.** `ReviewState.cursor` is an index into the active
diff mode's *row list*, not a raw line index — a row can be a diff line or an
injected review-comment/orphan row. Unified rendering, previously a strict
1:1 map from `DiffLine`s to screen rows, became row-list-driven for this:
`URow::{Line(usize), Comment(u64), Orphan(u64)}`, cached in `unified_rows`
next to the pre-existing `SbsRow` cache side-by-side rendering already used
(extended with its own `Comment`/`Orphan` variants). Rows carry comment
*ids*, not vector indexes, so a mid-session deletion can't leave a row
pointing at the wrong comment. The cursor renders with the selection colour
only while the diff pane has focus, is clamped to the (possibly-shrunk) row
count after every relist, and resets to row 0 on a file or mode change — with
one deliberate exception: `]`/`[` (comment navigation) switch the file first,
then place the cursor on the target comment's row rather than row 0. Every
cursor-acting key (`c`, `x`) first scrolls an offscreen cursor into view and
stops there ("act-and-reveal") rather than acting on a row the user can't
see; a mouse click in the diff resolves the clicked screen row to a row index
the same way and moves the cursor there, while the scroll wheel only moves
the viewport.

## Configuration write-back

Three explicit in-app actions — cycling the theme (`t`), toggling diff mode
(`d`), and toggling line numbers (`n`) — write their new value back to
`config.toml` via `toml_edit`, which preserves comments, unrelated keys, and
formatting elsewhere in the file. If the existing file fails to parse, the
write is skipped entirely (the in-app change still applies for the running
session) rather than clobbering a user's broken-but-recoverable config.
Reads stay on `toml`/serde; only the write path uses `toml_edit`.

## Performance

Targets (small to mid-size repos, up to ~2M LOC):

| Metric                                  | Target              |
|-----------------------------------------|---------------------|
| Startup                                 | < 100 ms            |
| Diff render (typical file)              | < 50 ms             |
| Scrolling / pane switching              | No frame drops      |
| Resident memory                         | Tens of MB, not hundreds |

How they're met:

- Rendered diffs are cached per `(file, mode)` and invalidated when the file or
  diff mode changes.
- Syntax highlighting runs over the visible window plus a small buffer, not the
  whole file.
- The history walk decodes commit objects only (no trees or blobs) and is
  bounded to a page (500 commits); per-commit trees and blobs load lazily for
  the selected commit.

## Non-goals

strix deliberately leaves these to `git` itself, to other tools, or to a future
phase:

- Commit creation (use `git commit`)
- Branch management (create / switch / delete branches)
- Merge conflict resolution
- Stashing
- Remote operations (push, pull, fetch)
- Hunk-level or line-level staging (file-level only)
- A daemon or long-running session: review comments are a plain JSON file on
  disk (`.git/strix/comments.json`), read and written by whichever process
  (TUI or `strix comment`) happens to run, never a background service.

Reviewing a branch against its base (`strix diff <base>`) is explicitly **in
scope** — it's the foundation for strix becoming the review surface for
agent-written code. Inline review comments (`c`/`x`/`]`/`[` in a review
session, plus `strix comment` for agents — see [Review view](#review-view)
and [comments store](#comments-store) above) build directly on it; an
agent-facing skill is the next piece on that track, not a future-phase item.

Keeping the surface area narrow otherwise is the point: strix earns its place
by doing "review a changeset and stage it," "browse history," and now
"review a branch against its base, with inline comments" very well, not by
covering everything `git` does.

## Testing

- **Render tests** build an `App`, call `dump_frame`, and assert on the text
  grid — fast, deterministic, no terminal required.
- **Git tests** create a temp repo (`git init` in a `tempfile::tempdir`),
  exercise the git layer, and assert on the results.
