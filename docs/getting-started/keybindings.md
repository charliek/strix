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
| `n`            | Toggle line numbers in the diff gutter (persists) |
| `w`            | Toggle hard line wrapping in the diff pane (persists) |
| `f`            | Toggle cross-file scroll — scroll past a diff's edge into the next / previous file (persists) |
| `t`            | Cycle the theme (presets, then your custom themes; persists) |
| `m`            | Show / hide the menu bar (persists)     |
| `b`            | Show / hide the left panel              |
| `i`            | Toggle the History view                 |
| `1`, `2`       | Switch to the Status / History view (or the review session / History, in a `strix diff` session) |
| `Esc`          | Close an overlay; leave History view    |

`n`, `w`, `f`, `t`, and `m` work in every view (Status, History, and the review
view), though cross-file scroll (`f`) only crosses boundaries in Status and
Review — the History view is excluded. While the discard-confirmation modal is
open, `n` is consumed as "no"
(dismiss the modal) rather than toggling line numbers; the global binding
resumes once the modal closes.

## Menu bar

The header shows two dropdown menus, `View` and `Theme`, whenever the menu
bar is visible (`m` toggles it, on by default — see
[Configuration](../guides/configuration.md)). `View` holds diff mode
(unified / side-by-side), line numbers, line wrap, cross-file scroll, the
changes panel, and a
Status/History switcher; the current view's item is checked so the row that
would just switch to itself still shows state, not a dead control. `Theme` lists the built-in presets and
any custom `themes/*.toml` files, with the active one marked.

**Opening a menu is mouse-first**: click a title (`View` or `Theme`) to open
its dropdown. There's no keyboard chord to *open* one — every setting a menu
exposes already has its own direct key (`d`, `n`, `w`, `f`, `b`, `t`, `1`, `2`). Once a
menu is open, it's fully keyboard-navigable:

| Key                | Action                                            |
|--------------------|----------------------------------------------------|
| `←`/`→`, `Tab`/`Shift+Tab` | Switch to the other top-level menu             |
| `↑`/`↓`            | Move the highlight (skips separators, wraps at the ends) |
| `Enter`, `Space`   | Activate the highlighted item and close the menu  |
| `Esc`, `m`         | Close the menu without changing anything          |

Clicking an item activates it the same way; clicking a separator or anywhere
in the dropdown that isn't an item is consumed but does nothing; clicking
another title switches menus; clicking outside the dropdown closes it and
still routes the click to whatever's underneath. While a menu is open, moving
the mouse (no click needed) over another title slides the dropdown to follow
it, and moving over an item in the open dropdown moves the highlight — the
same effect as arrowing to it.

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
| `j`, `↓`        | Move the cursor down (scrolls to follow it)       |
| `k`, `↑`        | Move the cursor up                                |
| `Ctrl-d`, `Ctrl-u` | Move the cursor a half page down / up          |
| `g`, `G`        | Jump to top / bottom of the diff                  |
| `d`             | Toggle unified / side-by-side mode                |
| `w`             | Toggle hard line wrapping (long lines wrap instead of truncating) |
| `f`             | Toggle cross-file scroll (scroll past a diff's edge into the next / previous file) |
| `c`             | Add a comment on the cursor's line, or edit the comment under it |
| `X`             | Delete the comment under the cursor (no confirmation) |
| `]`, `[`        | Jump to the next / previous comment               |
| `h`, `←`        | Focus the Changes pane                            |

### Comments (working tree)

The diff pane's cursor above also addresses comments — worktree notes on the
selected file's net `HEAD`-vs-worktree change (the `pending · HEAD→worktree`
pane; see [Usage](usage.md#on-uncommitted-work)). `c` and `X` follow
**act-and-reveal**: an offscreen cursor's first press only scrolls it into
view, the second acts. `c` on the file list, a hunk-header row, or a
conflicted/binary/submodule file does nothing but flash a hint;
double-clicking a code line or an existing comment box works the same as
pressing `c`. See [Comments](usage.md#comments) for the box, the multi-line
editor, and the lifecycle (stale, swept on commit).

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

## Review view

Opened with `strix diff <RANGE>` (see [Usage](usage.md#reviewing-a-branch));
there is no in-app way to enter it from the staging view. The left column is a
flat file list (no Staged/Changes sections) beside the shared diff pane.

| Key             | Action                                                           |
|-----------------|-------------------------------------------------------------------|
| `Tab`           | Switch focus: file list ↔ diff                                    |
| `h`, `l`, `←`, `→` | Focus the file list / the diff                                 |
| `j`, `k`, `↓`, `↑` | Move in the focused pane: file selection, or the diff cursor   |
| `g`, `G`        | Jump to the first / last item in the focused pane                 |
| `Ctrl-d`, `Ctrl-u` | Move the diff cursor a half page                               |
| `d`             | Toggle unified / side-by-side                                     |
| `c`             | Add a comment on the code row under the cursor, or edit the comment on a comment row (multi-line editor, `Enter` saves, `Esc` cancels — see below) |
| `X`             | Delete the comment under the cursor (no confirmation)              |
| `]`, `[`        | Jump to the next / previous comment (listed files only, wraps)     |
| `b`             | Show / hide the file list                                         |
| `i`             | Open the History view                                             |
| `1`             | Return to the review session (from History)                       |
| `2`             | Open the History view                                             |

The view is read-only for staging: `space`, `Enter`, `s`, `u`, `x`, and
clicking a file's status marker do nothing — no modal, no index change. `x`
does **not** delete a comment here either — that's `X` (`Action::DeleteComment`,
its own action) — `x` is a silent no-op on a comment/orphan row and inert
everywhere else in this read-only view. `Esc` does not exit the session
(unlike History); quit with `q`.

### Review comments

The diff pane's cursor (`j`/`k`/`g`/`G`/`Ctrl-d`/`Ctrl-u` above) can rest on a
comment row, not just a code line; it only renders (with the selection
colour) while the diff pane has focus. `c` and `X` act on whatever row the
cursor is on, and both follow **act-and-reveal**: if the cursor is currently
scrolled offscreen, the key only scrolls it into view — a second press is
needed to actually act, so nothing is ever added to or deleted from a row
you can't see. `c` on the file list, a hunk-header row, or an
agent-authored comment does nothing but flash a hint instead of opening the
editor; double-clicking a code line or an existing comment box opens the
editor the same way `c` does, and clicking a box's `[x]` deletes it directly
(see [Mouse](#mouse)). `]`/`[` only cycle comments on files still in the
review's file list; comments on files that dropped out of the range are
orphaned and only reachable via `strix comment list` (see
[CLI](../reference/cli.md)).

Adding or editing a comment additionally requires that the range you're
reviewing has your checked-out branch as its head (the common
`strix diff main` case) — otherwise `c` flashes instead of opening the
editor. See [Usage](usage.md#leaving-review-comments) for the full comment
model (orphans, persistence, the agent-facing CLI).

`c`, `X`, `]`, and `[` share their default chords with no other action, and
each is independently remappable — see
[Configuration](../guides/configuration.md#keybindings). Unlike milestone 6,
`x`/`discard` and comment deletion are fully separate actions now, so
remapping one never affects the other.

## Mouse

| Gesture                       | Action                                          |
|-------------------------------|-------------------------------------------------|
| Click a menu title (`View`/`Theme`) | Open its dropdown, or switch to it if another is already open (see [Menu bar](#menu-bar)) |
| Click a file                  | Select it (and show its diff)                   |
| Click a file's status marker  | Toggle stage / unstage (staging view only; a no-op in the read-only Review view) |
| Click a pane                  | Focus that pane                                 |
| Click a commit in the graph   | Select it (and show its details)                |
| Click a row in the diff pane (Status or Review) | Focus the diff and move the cursor there (a comment box is selected, not opened — double-click or press `c` to edit it) |
| Double-click a code line in the diff pane | Open the in-place editor there (add a comment); excludes the marker zone and the file list |
| Double-click a comment box    | Edit it (an agent note flashes read-only instead) |
| Click a comment box's `[x]`   | Delete that comment, no confirmation             |
| Drag the vertical split bar   | Resize the left column vs the diff              |
| Drag the horizontal split bar | Resize Committed Changes vs Graph (History view)|
| Scroll wheel                  | Scroll the pane under the cursor; in the diff pane this moves the viewport only — the cursor stays put |
| Trackpad horizontal scroll    | Shift the diff's code content sideways to read long lines (gutters, sign column, hunk headers, and comment boxes stay put); works in every view's diff pane, and only when line wrap is off |

Horizontal scrolling is a trackpad gesture only — there is no keybinding and
nothing is persisted. It relies on the terminal emitting horizontal scroll
events; some terminals (including macOS Terminal.app) never do, so the gesture
does nothing there. It is inert while line wrap (`w`) is on, since wrapped lines
have nothing to scroll to.

Double-click detection is semantic, not pixel-based: two clicks resolving to
the same target (same file, same code line or comment box) within 500 ms
count as a double-click. A drag, a scroll, or a layout change (resize, mode
toggle, a comment added/edited/deleted) resets the tracker, so a stray click
afterward is never mistaken for the second half of a double-click.

## In-place comment editor

While the editor is open (after `c` or a double-click — see
[Usage](usage.md#comments)), keys go to the editor first — these are
**fixed**, not part of the remappable `[keys]` keymap, so they apply
regardless of any keybinding overrides:

| Key                              | Action                                    |
|-----------------------------------|--------------------------------------------|
| `Enter`                           | Save                                      |
| `Esc`                             | Discard (revert an edit, or cancel a new comment) |
| `Shift+Enter`, `Ctrl-J`, `Alt+Enter` | Insert a newline (three equivalent chords — see below) |
| `←` `→` `↑` `↓`, `Home`, `End`, `Backspace`, `Delete` | Move / edit within the buffer |
| any other character               | Inserted literally, including `c`, `x`, `]`, `[`, and any other action's normal key |

`Shift+Enter` alone is unreliable on terminals without a keyboard-enhancement
protocol, so `Ctrl-J` and `Alt+Enter` are equally-supported fallbacks — use
whichever your terminal delivers. A bracketed paste containing newlines
inserts them as real line breaks rather than one `Enter` per line. Ctrl/Alt
combinations other than the newline chords above are ignored; `Ctrl-C` still
quits immediately, even while editing.
