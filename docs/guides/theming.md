# Theming

strix ships several themes and lets you add your own. A theme controls both the
UI chrome (borders, selection, status colours) and the diff colours, and selects
a syntax-highlighting theme for code.

## Choosing a theme

In `~/.config/strix/config.toml`:

```toml
theme = "tokyo-night"
```

Or per-invocation:

```bash
strix --theme gruvbox
```

Or at runtime: press `t` to cycle to the next theme. The footer flashes the
**canonical** name of the theme it switched to (e.g. `tokyonight` resolves and
flashes as `tokyo-night`), and the choice is written back to `config.toml` —
see [Configuration](configuration.md#runtime-changes-persist). `t` cycles
through the built-in presets first, in the order listed below, then your
custom `themes/*.toml` files sorted alphabetically by file stem.

## Built-in themes

| Name          | Description                          |
|---------------|--------------------------------------|
| `tokyo-night` | Dark, blue-accented (the default)    |
| `dark`        | Neutral dark                         |
| `light`       | Neutral light                        |
| `catppuccin`  | Catppuccin Mocha                     |
| `gruvbox`     | Gruvbox dark                         |

## Custom themes

Drop a `.toml` file in `~/.config/strix/themes/` and reference it by file stem
(`my-theme.toml` → `theme = "my-theme"`). Colours are hex strings:

```toml
base = "tokyo-night"            # preset to start from (default: tokyo-night)
syntax = "base16-ocean.dark"    # bundled syntect theme for code highlighting

[colors]
bg = "#1a1b26"
fg = "#a9b1d6"
dim = "#565f89"
border = "#292e42"
border_focused = "#7aa2f7"
staged = "#9ece6a"
unstaged = "#e0af68"
untracked = "#7dcfff"
selection_bg = "#283457"
add = "#9ece6a"
add_bg = "#202c26"
add_emph = "#2e5c3a"
add_gutter = "#1a241f"
del = "#f7768e"
del_bg = "#312027"
del_emph = "#642a3c"
del_gutter = "#281c21"
hunk = "#7dcfff"
comment = "#bb9af7"
```

`comment` colours the comment boxes (`● you`/`● agent`) and the file-list
`● n` badge — see [Comments](../getting-started/usage.md#comments).

`add_emph`/`del_emph` are the side-by-side **word-diff emphasis** colours: on
a genuinely modified pair (both sides present, similar enough to be an edit
of one another), the changed span within the line is painted in `add_emph`
(new side) / `del_emph` (old side) instead of the flat `add_bg`/`del_bg` wash
— brighter and more saturated, so the edited substring reads distinctly. A
pure addition/deletion, an unchanged context pair, or a pair too dissimilar
to be a real edit gets no emphasis (just the base background). Both are
optional in a custom theme file; an omitted one falls back to the `base`
preset's value like every other colour.

`add_gutter`/`del_gutter` colour the **empty column** in side-by-side mode
when its opposite has a pure addition or deletion (no partner line, so
nothing to zip it against): the empty half of the row opposite an Addition
gets `add_gutter`, opposite a Deletion gets `del_gutter` — a subtle tint
instead of the flat pane background, so the row still reads as "one side
changed" at a glance. A context pair (unchanged on both sides) and a modified
pair (both sides present) never get this tint, on either side. Both are
optional in a custom theme file; an omitted one falls back to the `base`
preset's value like every other colour.

Any colour you omit falls back to the `base` preset's value, so a partial theme
is fine.

!!! note
    A custom theme file whose stem matches a built-in preset name **or one of
    its aliases** (e.g. `themes/dark.toml`, or `themes/mocha.toml` — `mocha`
    is an alias for `catppuccin`) is unreachable: the built-in preset always
    wins, both when resolving `theme = "…"` and when cycling with `t`. Pick a
    stem that isn't a preset name or alias.

!!! note
    Themes use 24-bit colour. On a terminal without truecolor the palette
    degrades to the nearest 256-colour approximation.
