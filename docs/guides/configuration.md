# Configuration

strix reads `~/.config/strix/config.toml` (or
`$XDG_CONFIG_HOME/strix/config.toml`). Every field is optional; defaults apply
when the file or a field is missing.

```toml
# Theme: a built-in preset or the stem of a file in ~/.config/strix/themes/
theme = "tokyo-night"

# Default diff mode: "unified" or "side-by-side"
diff_mode = "unified"

# Auto-refresh: update the view live as the working tree / git state change.
# On by default; set false to disable the file watcher and refresh only with `r`.
auto_refresh = true

# Show line numbers in the diff gutter. On by default; toggle at runtime with `n`.
line_numbers = true

# Show the top menu bar (the `View`/`Theme` labels in the header). On by
# default; set false to start with it hidden, or toggle at runtime with `m`.
menu_bar = true

# Hard-wrap long diff lines at the pane width instead of truncating them. Off by
# default; toggle at runtime with `w`.
wrap_lines = false
```

## Keybindings

Override any default under `[keys]`. The value is a list of key chords; an action
fires on any of them. Anything you don't list keeps its default.

```toml
[keys]
quit             = ["q", "ctrl-c"]
help             = ["?"]
refresh          = ["r"]
switch_pane      = ["tab"]
toggle_diff_mode = ["d"]
toggle_line_numbers = ["n"]
toggle_wrap      = ["w"]
cycle_theme      = ["t"]
toggle_menu_bar  = ["m"]
toggle_changes   = ["b"]
toggle_history   = ["i"]
status_view      = ["1"]
history_view     = ["2"]
down             = ["j", "down"]
up               = ["k", "up"]
top              = ["g", "home"]
bottom           = ["G", "end"]
half_page_down   = ["ctrl-d"]
half_page_up     = ["ctrl-u"]
focus_staging    = ["h", "left"]
focus_diff       = ["l", "right"]
toggle_stage     = ["space", "enter"]
stage            = ["s"]
unstage          = ["u"]
discard          = ["x"]
comment          = ["c"]
next_comment     = ["]"]
prev_comment     = ["["]
delete_comment   = ["X"]
```

`down`/`up` and `top`/`bottom` are context-aware: they move the file cursor in
the **Changes** pane and scroll the **Diff** pane, depending on which is
focused. Staging actions (`stage`, `discard`, …) act on the selected file from
either pane. `comment`/`next_comment`/`prev_comment`/`delete_comment` act on
the diff-pane cursor in both the Status (working-tree) and Review diff panes —
`discard` and `delete_comment` are independent actions, so remapping one never
touches the other (unlike milestone 6, where deleting a comment was an
overload of `discard`/`x`).

Chord syntax: a key name (`a`, `enter`, `space`, `tab`, `esc`, `up`, `left`,
`pageup`, …) optionally prefixed with `ctrl-`, `alt-`, or `shift-`
(e.g. `ctrl-d`). `toggle_line_numbers` also accepts the config-file names
`toggle-line-numbers` / `line-numbers`; `toggle_wrap` also accepts
`toggle-wrap` / `wrap`; `cycle_theme` also accepts `cycle-theme` / `theme`;
`toggle_menu_bar` also accepts `toggle-menu-bar` / `menu-bar` / `menu`.

The in-app help overlay (`?`) and the footer hints list the **default** keys,
not any you've remapped here.

Assigning the same chord to two different actions in `[keys]` logs a warning
to the log file (default verbosity; see [Environment](../reference/cli.md#environment)
and [Logs](../reference/cli.md#logs)) rather than failing silently — the later
assignment in the table wins.

## Runtime changes persist

Pressing `t` (cycle theme), `d` (toggle diff mode), `n` (toggle line
numbers), `w` (toggle line wrap), or `m` (toggle menu bar) writes the new value
into `config.toml` immediately, so the choice survives the next launch. Only
these five explicit actions write anything — `[keys]` and `auto_refresh` are
never touched by strix itself. Picking diff mode, line numbers, wrap, or a theme
from the `View`/`Theme` menu bar dropdowns persists the same way as pressing its
equivalent key, since it's the same action under the hood; picking
Status/History from the `View` menu switches the view but doesn't persist,
matching `1`/`2`/`i`.

The write preserves everything else in the file: comments, unrelated keys and
tables, and their formatting. Only the one changed value's own formatting may
be normalized.

If `config.toml` exists but fails to parse, strix never overwrites it — the
in-app change still takes effect for the running session, but the save fails
and a footer notice reports it (`couldn't save setting: …`), leaving your
file byte-for-byte as it was so you can fix it by hand.

`--theme` alone (a one-off override for a single run) never writes to the
config file. Cycling the theme with `t` *from* a `--theme` override does
persist — the newly chosen theme is what gets saved.

Running multiple strix instances against the same config directory is
**last-writer-wins**: each write replaces the whole file, so if two instances
save around the same time, one save can be silently lost. There's no
cross-instance locking.

**Comments are not part of this.** Adding, editing, or deleting a comment —
on uncommitted work in bare `strix`, or in a review session (`c`, `X` — see
[Comments](../getting-started/usage.md#comments)) — writes to
`.git/strix/comments.json` immediately, the moment you act — there's no
"persists on next launch" delay like `t`/`d`/`n`, and nothing about it
touches `config.toml`.

See [Keybindings](../getting-started/keybindings.md) for the defaults and
[Theming](theming.md) for theme files.
