# Configuration

strix reads `~/.config/strix/config.toml` (or
`$XDG_CONFIG_HOME/strix/config.toml`). Every field is optional; defaults apply
when the file or a field is missing.

```toml
# Theme: a built-in preset or the stem of a file in ~/.config/strix/themes/
theme = "tokyo-night"

# Default diff mode: "unified" or "side-by-side"
diff_mode = "unified"

# Spaces per tab when rendering diffs
tab_width = 4

# Show line numbers in the diff gutter
line_numbers = true
```

## Keybindings

Override any default under `[keys]`. The value is a list of key chords; an action
fires on any of them. Anything you don't list keeps its default.

```toml
[keys]
quit         = ["q", "ctrl-c"]
help         = ["?"]
refresh      = ["r"]
switch_pane  = ["tab"]
next_file    = ["j", "down"]
prev_file    = ["k", "up"]
stage        = ["s"]
unstage      = ["u"]
toggle_stage = ["space", "enter"]
discard      = ["x"]
diff_mode    = ["d"]
scroll_down  = ["j", "down"]
scroll_up    = ["k", "up"]
```

Chord syntax: a key name (`a`, `enter`, `space`, `tab`, `esc`, `up`, `left`,
`pageup`, …) optionally prefixed with `ctrl-`, `alt-`, or `shift-`
(e.g. `ctrl-d`).

See [Keybindings](../getting-started/keybindings.md) for the defaults and
[Theming](theming.md) for theme files.
