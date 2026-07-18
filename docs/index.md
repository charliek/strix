# strix

A focused, polished TUI for staging changes and viewing diffs without leaving
the terminal. Named after *Strix*, the genus of owls known for sharp vision and
quiet observation.

strix is built for one workflow done well: review a changeset and stage it. A
changeset can be the working tree (staging) or a branch against its base
(`strix diff <base>`, read-only) — it deliberately leaves commit creation,
branch management, and remote operations to `git` itself, and spends its
effort on the parts a terminal usually does poorly — a clean two-pane layout,
syntax-highlighted diffs, real mouse support, and themes that match a modern
editor. strix is becoming the review surface for agent-written code; inline
comments and an agent-facing skill build on top of branch review. See
[Non-goals](reference/architecture.md#non-goals) for the full list of what strix
intentionally doesn't do.

## What it looks like

- **Left — Changes.** Staged files on top, unstaged + untracked below. Stage,
  unstage, and reset files; jump between them by keyboard or mouse.
- **Right — Diff.** A syntax-highlighted diff of the selected file, in unified or
  side-by-side mode, with smooth scrolling.

## Highlights

- Syntax highlighting on diffs (powered by syntect)
- **Branch review** (`strix diff main`): a read-only, GitHub-PR-style diff of a
  branch against its merge base
- Light, dark, and popular preset themes (Catppuccin, Tokyo Night, Gruvbox),
  cyclable at runtime with `t` and persisted
- Toggleable line numbers (`n`)
- Mouse support: click to select, click to stage, scroll diffs
- Customizable keybindings
- File-level reset with confirmation
- Toggleable **History view** with a colored branch/merge rail graph and
  commit-vs-parent diffs (press `i`)
- Fast: pure-Rust git reads via gitoxide, cached diff rendering

## Get started

- [Installation](getting-started/installation.md) — Homebrew, apt, or build from source
- [Usage](getting-started/usage.md) — the two-pane workflow
- [Keybindings](getting-started/keybindings.md) — the full key map
- [Theming](guides/theming.md) and [Configuration](guides/configuration.md)
- [Architecture](reference/architecture.md) — how it's put together
