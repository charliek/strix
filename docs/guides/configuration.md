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
cycle_theme      = ["t"]
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
```

`down`/`up` and `top`/`bottom` are context-aware: they move the file cursor in
the **Changes** pane and scroll the **Diff** pane, depending on which is focused.
Staging actions (`stage`, `discard`, …) act on the selected file from either
pane.

Chord syntax: a key name (`a`, `enter`, `space`, `tab`, `esc`, `up`, `left`,
`pageup`, …) optionally prefixed with `ctrl-`, `alt-`, or `shift-`
(e.g. `ctrl-d`). `toggle_line_numbers` also accepts the config-file names
`toggle-line-numbers` / `line-numbers`; `cycle_theme` also accepts
`cycle-theme` / `theme`.

The in-app help overlay (`?`) and the footer hints list the **default** keys,
not any you've remapped here.

## Runtime changes persist

Pressing `t` (cycle theme), `d` (toggle diff mode), or `n` (toggle line
numbers) writes the new value into `config.toml` immediately, so the choice
survives the next launch. Only these three explicit actions write anything —
`[keys]` and `auto_refresh` are never touched by strix itself.

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

See [Keybindings](../getting-started/keybindings.md) for the defaults and
[Theming](theming.md) for theme files.
