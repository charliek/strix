# Usage

```bash
strix              # open the repository containing the current directory
strix path/to/repo # open a specific repository
```

strix opens on the alternate screen and restores your terminal on exit (even if
it panics).

## The layout

```
 strix  my-repo                                       main
╭ Changes ───────────────────╮╭ Diff · unified ──────────────────────╮
│ Staged                     ││  src/app.rs                          │
│   M src/app.rs             ││ @@ -12,6 +12,7 @@                     │
│ Changes                    ││  12  pub struct App {                │
│   M src/ui/mod.rs          ││  13      pub repo_path: PathBuf,      │
│ ? notes.txt                ││ +14      pub focus: Focus,            │
╰────────────────────────────╯╰──────────────────────────────────────╯
 j/k move   space stage   d diff mode   q quit
```

- **Changes (left).** Two sections: **Staged** files on top, **Changes**
  (unstaged) and untracked files below. Each row shows a status marker
  (`M` modified, `A` added, `D` deleted, `?` untracked) coloured by state.
- **Diff (right).** The selected file's diff, syntax-highlighted, in **unified**
  or **side-by-side** mode.

## A typical session

1. Launch `strix` in your repo. The first changed file is selected and its diff
   is shown.
2. Move through files with `j`/`k` (or the mouse).
3. Press `space` to stage or unstage the selected file. It moves between the
   **Changes** and **Staged** sections.
4. Press `d` to flip between unified and side-by-side diffs.
5. Press `x` to discard a file's changes (you'll be asked to confirm).
6. Commit with `git commit` in another pane — strix intentionally doesn't create
   commits.

## History view

Press `i` (or `2`) to switch to the **History view**; `Esc`, `i`, or `1` returns
to the staging view. The left column changes shape:

```
╭ Committed Changes ─────────╮╭ Commit a1b2c3d ──────────────────────╮
│ ● a1b2c3d Add history view ││ commit a1b2c3d4e5f6…                  │
│   M src/app.rs             ││ Author  …                             │
│   A src/git/history.rs     ││ Date    2026-05-30 14:02 +0000        │
│   M src/ui/mod.rs          ││                                       │
├ Graph ─────────────────────┤│     Add history view                  │
│ ● a1b2c3d Add history view ││                                       │
│ │ 9f8e7d6 Fix diff scroll  ││  3 files changed, +120 −14            │
│ ● 7c6b5a4 Docs install     ││   M src/app.rs   +40 −2               │
╰────────────────────────────╯╰───────────────────────────────────────╯
 j/k move   tab pane   d split   b hide   i/esc status
```

- **Graph (bottom-left).** Commit log of the current branch (HEAD ancestry,
  including merges), with a colored branch/merge rail. Move with `j`/`k`, click
  a row to select it.
- **Committed Changes (top-left).** The selected commit followed by its changed
  files. The commit row (`●`) is selectable; the right pane swaps between
  commit details and a file diff based on what you pick.
- **Right pane.** Commit details (full hash, author, date, message, per-file
  stat) when the `●` row is selected; the file's diff vs the commit's first
  parent when a file is selected — same renderer as the status view.
- **Layout.** The vertical split bar resizes the left column vs the diff; the
  horizontal one resizes Committed Changes vs Graph. Both are drag-to-resize.
- **`b`** collapses the entire left column the same way it does in the status
  view, leaving the diff (or commit details) full-width.

## Inspecting a frame

For debugging or scripting, `--dump-frame` renders one frame to stdout as text
and exits:

```bash
strix --dump-frame --width 120 --height 40
```

See [Keybindings](keybindings.md) for the complete key map and
[CLI](../reference/cli.md) for all flags.
