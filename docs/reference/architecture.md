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
| `app`         | State + input dispatch (`on_key`, `on_mouse`).                     |
| `terminal`    | Setup/teardown, event loop, panic-safe restore, `dump_frame`.     |
| `ui`          | Rendering: layout, the two panes, theme, syntax highlighting.     |
| `git`         | Repository access: status, diffs, mutations.                      |
| `config`      | Config + theme loading; keybinding/mouse dispatch tables.         |

## Git layer

| Operation                     | Mechanism                                              |
|-------------------------------|--------------------------------------------------------|
| Status (staged/unstaged/untracked) | [gix](https://github.com/GitoxideLabs/gitoxide) — pure-Rust reads. |
| Blob contents (HEAD / index / worktree) | gix object database + the working tree.        |
| Diff computation              | [`similar`](https://github.com/mitsuhiko/similar) over blob bytes, producing structured hunks. |
| Stage / unstage / reset       | Shell out to `git` (`add`, `restore --staged`, `restore`). |

Reads stay on the pure-Rust path for speed; the three mutations shell out to
`git`, which the [spec](../spec.md) explicitly sanctions as a fallback — it is
reliable and sidesteps gix's less-mature index-write porcelain. Those shell-outs
are confined to `git/ops.rs`.

## Diff model

A diff is a `FileDiff` of hunks; each hunk is a list of lines tagged
`Add` / `Delete` / `Context` with their old and new line numbers. This one model
drives both unified rendering (a single column) and side-by-side rendering (old
on the left, new on the right, with blank padding to keep the two columns
aligned). Syntax highlighting is layered on top: syntect tokenizes the file
content, and the token colours are composited with the add/delete backgrounds.

## Performance

- Rendered diffs are cached per `(file, mode)` and invalidated when the file or
  diff mode changes.
- Syntax highlighting runs over the visible window plus a small buffer, not the
  whole file.
- Targets: startup < 100 ms and diff render < 50 ms on mid-size repos, with no
  frame drops while scrolling.

## Testing

- **Render tests** build an `App`, call `dump_frame`, and assert on the text
  grid — fast, deterministic, no terminal required.
- **Git tests** create a temp repo (`git init` in a `tempfile::tempdir`),
  exercise the git layer, and assert on the results.
