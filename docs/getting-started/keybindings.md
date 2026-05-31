# Keybindings

These are the defaults. Every binding is remappable — see
[Configuration](../guides/configuration.md).

## Global

| Key            | Action                                  |
|----------------|-----------------------------------------|
| `q`, `Ctrl-c`  | Quit                                    |
| `?`            | Toggle the help overlay                 |
| `r`            | Refresh status from disk                |
| `Tab`          | Switch focus between panes              |
| `b`            | Show / hide the left panel              |
| `i`            | Toggle the History view                 |
| `1`, `2`       | Switch to the Status / History view     |
| `Esc`          | Close an overlay; leave History view    |

## Changes pane

| Key             | Action                                            |
|-----------------|---------------------------------------------------|
| `j`, `↓`        | Next file                                         |
| `k`, `↑`        | Previous file                                     |
| `g`, `G`        | First / last file                                 |
| `Enter`, `Space`| Toggle stage / unstage of the selected file       |
| `s`             | Stage the selected file                           |
| `u`             | Unstage the selected file                         |
| `x`             | Discard changes to the file (asks to confirm)     |
| `l`, `→`        | Focus the Diff pane                               |

## Diff pane

| Key             | Action                                            |
|-----------------|---------------------------------------------------|
| `j`, `↓`        | Scroll down one line                              |
| `k`, `↑`        | Scroll up one line                                |
| `Ctrl-d`, `Ctrl-u` | Scroll half a page down / up                   |
| `g`, `G`        | Jump to top / bottom of the diff                  |
| `d`             | Toggle unified / side-by-side mode                |
| `h`, `←`        | Focus the Changes pane                            |

## History view

Open with `i` (or `2`); leave with `Esc`, `i`, or `1`. The left column splits
into **Committed Changes** (the selected commit's `●` row + its files) on top
and **Graph** (the commit log with a colored branch/merge rail) on the bottom,
separated by a draggable horizontal divider. The right pane shows commit
details when the `●` row is selected, or the file's diff (vs the commit's
first parent) when a file is selected.

| Key             | Action                                                           |
|-----------------|------------------------------------------------------------------|
| `Tab`           | Cycle focus: Graph → Committed Changes → Diff                    |
| `h`, `l`, `←`, `→` | Step focus left / right across the three panes               |
| `j`, `k`, `↓`, `↑` | Move in the focused pane (commit / file / diff scroll)        |
| `g`, `G`        | Jump to the first / last item in the focused pane                |
| `Ctrl-d`, `Ctrl-u` | Scroll the diff (or details) a half page                      |
| `d`             | Toggle unified / side-by-side for file diffs                     |
| `b`             | Show / hide the left column (Graph + Committed Changes)          |

The view is read-only: staging keys (`space`, `s`, `u`, `x`) do nothing. The
graph is walked from HEAD ancestry (including merges) up to 500 commits per
page; scrolling past the end loads the next page.

## Mouse

| Gesture                       | Action                                          |
|-------------------------------|-------------------------------------------------|
| Click a file                  | Select it (and show its diff)                   |
| Click a file's status marker  | Toggle stage / unstage                          |
| Click a pane                  | Focus that pane                                 |
| Click a commit in the graph   | Select it (and show its details)                |
| Drag the vertical split bar   | Resize the left column vs the diff              |
| Drag the horizontal split bar | Resize Committed Changes vs Graph (History view)|
| Scroll wheel                  | Scroll the pane under the cursor                |
