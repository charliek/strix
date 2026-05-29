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

## Inspecting a frame

For debugging or scripting, `--dump-frame` renders one frame to stdout as text
and exits:

```bash
strix --dump-frame --width 120 --height 40
```

See [Keybindings](keybindings.md) for the complete key map and
[CLI](../reference/cli.md) for all flags.
