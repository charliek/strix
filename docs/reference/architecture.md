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
| `ui`          | Rendering: layout, the two panes, theme, syntax highlighting.     |
| `git`         | Repository access: status, history, diffs, mutations.             |
| `graph`       | Pure rail-graph lane layout for the history view.                 |
| `config`      | Config + theme loading; keybinding/mouse dispatch tables.         |

## Git layer

| Operation                     | Mechanism                                              |
|-------------------------------|--------------------------------------------------------|
| Status (staged/unstaged/untracked) | [gix](https://github.com/GitoxideLabs/gitoxide) — pure-Rust reads. |
| Blob contents (HEAD / index / worktree) | gix object database + the working tree.        |
| Commit walk + refs (history view) | gix `rev_walk` from HEAD (full DAG), commit objects decoded only. |
| Per-commit changed files      | `git diff-tree -z --name-status` (parsed in `git/history.rs`).  |
| Diff computation              | [`similar`](https://github.com/mitsuhiko/similar) over blob bytes, producing structured hunks. |
| Stage / unstage / reset       | Shell out to `git` (`add`, `restore --staged`, `restore`).      |

Reads stay on the pure-Rust path for speed; the three mutations shell out to
`git` because it is reliable and sidesteps gix's less-mature index-write
porcelain. Those shell-outs are confined to `git/ops.rs`.

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

Keeping the surface area narrow is the point: strix earns its place by doing
"review a changeset and stage it" — and now "browse history" — very well, not by
covering everything `git` does.

## Testing

- **Render tests** build an `App`, call `dump_frame`, and assert on the text
  grid — fast, deterministic, no terminal required.
- **Git tests** create a temp repo (`git init` in a `tempfile::tempdir`),
  exercise the git layer, and assert on the results.
