# strix

A focused, polished TUI for staging changes and viewing diffs without leaving
the terminal. Named after *Strix*, the genus of owls known for sharp vision and
quiet observation.

strix is built for one workflow done well: review a changeset and stage it. It
deliberately leaves commit creation, branch management, and remote operations to
`git` itself, and spends its effort on the parts a terminal usually does poorly —
a clean two-pane layout, syntax-highlighted diffs, real mouse support, and
themes that match a modern editor.

!!! note "Status"
    strix is in active development toward its MVP. See the
    [Project Spec](spec.md) for the full scope and non-goals.

## What it looks like

- **Left — Changes.** Staged files on top, unstaged + untracked below. Stage,
  unstage, and reset files; jump between them by keyboard or mouse.
- **Right — Diff.** A syntax-highlighted diff of the selected file, in unified or
  side-by-side mode, with smooth scrolling.

## Highlights

- Syntax highlighting on diffs (powered by syntect)
- Light, dark, and popular preset themes (Catppuccin, Tokyo Night, Gruvbox)
- Mouse support: click to select, click to stage, scroll diffs
- Customizable keybindings
- File-level reset with confirmation
- Fast: pure-Rust git reads via gitoxide, cached diff rendering

## Get started

- [Installation](getting-started/installation.md) — build from source
- [Usage](getting-started/usage.md) — the two-pane workflow
- [Keybindings](getting-started/keybindings.md) — the full key map
- [Theming](guides/theming.md) and [Configuration](guides/configuration.md)
- [Architecture](reference/architecture.md) — how it's put together
