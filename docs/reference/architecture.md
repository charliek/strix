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
| `config`      | Config read (`toml`) and write-back (`toml_edit`); theme loading; keybinding/mouse dispatch tables. |

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
is read-only — staging actions are a no-op — and the file watcher refreshes it
like the other views, guarded so a worktree-only change (no new commit on
either side of the range) skips re-listing.

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

Reviewing a branch against its base (`strix diff <base>`) is explicitly **in
scope** — it's the foundation for strix becoming the review surface for
agent-written code (inline comments and an agent skill build on it next), not
a future-phase item.

Keeping the surface area narrow otherwise is the point: strix earns its place
by doing "review a changeset and stage it," "browse history," and now
"review a branch against its base" very well, not by covering everything
`git` does.

## Testing

- **Render tests** build an `App`, call `dump_frame`, and assert on the text
  grid — fast, deterministic, no terminal required.
- **Git tests** create a temp repo (`git init` in a `tempfile::tempdir`),
  exercise the git layer, and assert on the results.
