# Keybindings

These are the defaults. Every binding is remappable — see
[Configuration](../guides/configuration.md).

## Global

| Key            | Action                                  |
|----------------|-----------------------------------------|
| `q`, `Ctrl-c`  | Quit                                    |
| `?`            | Toggle the help overlay                 |
| `r`            | Refresh status from disk                |
| `Tab`          | Switch focus between Changes and Diff   |
| `Esc`          | Close an overlay / cancel a dialog      |

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
| `[`, `]`        | Previous / next changed file                      |
| `h`, `←`        | Focus the Changes pane                            |

## Mouse

| Gesture                       | Action                              |
|-------------------------------|-------------------------------------|
| Click a file                  | Select it (and show its diff)       |
| Click a file's status marker  | Toggle stage / unstage              |
| Click a pane                  | Focus that pane                     |
| Scroll wheel                  | Scroll the pane under the cursor    |
