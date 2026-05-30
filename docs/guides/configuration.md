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
toggle_changes   = ["b"]
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
(e.g. `ctrl-d`).

The in-app help overlay (`?`) and the footer hints list the **default** keys,
not any you've remapped here.

See [Keybindings](../getting-started/keybindings.md) for the defaults and
[Theming](theming.md) for theme files.
