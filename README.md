# strix

A focused, polished TUI for staging changes and viewing diffs without leaving
the terminal. Named after *Strix*, the genus of owls known for sharp vision and
quiet observation. It bridges the gap between heavy GUI source-control views
(Cursor, Zed) and existing terminal tools (LazyGit, tig) that can feel cluttered
or mouse-unfriendly.

```
 strix  my-repo                                       main
╭ Changes ───────────────────╮╭ Diff · unified ──────────────────────╮
│ Staged                     ││  src/app.rs                          │
│   M src/app.rs             ││ @@ -12,6 +12,7 @@                     │
│ Changes                    ││  12  pub struct App {                │
│   M src/ui/mod.rs          ││  13      pub repo_path: PathBuf,      │
│ ? notes.txt                ││ +14      pub focus: Focus,            │
╰────────────────────────────╯╰──────────────────────────────────────╯
 j/k move   space stage   d diff mode   q quit
```

Two panes, mouse + keyboard, syntax-highlighted diffs, themeable. Built in Rust
on [ratatui](https://github.com/ratatui/ratatui),
[gitoxide](https://github.com/GitoxideLabs/gitoxide), and
[syntect](https://github.com/trishume/syntect).

> **Status:** active development toward the MVP. See
> [`docs/spec.md`](docs/spec.md) for scope.

## Installation

### macOS (Homebrew)

```bash
brew install charliek/tap/strix
```

### Linux (apt)

```bash
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://apt.stridelabs.ai/pubkey.gpg | \
  sudo tee /etc/apt/keyrings/apt-charliek.gpg > /dev/null
echo 'deb [signed-by=/etc/apt/keyrings/apt-charliek.gpg] https://apt.stridelabs.ai noble main' | \
  sudo tee /etc/apt/sources.list.d/apt-charliek.list
sudo apt update && sudo apt install strix
```

Direct `.deb` downloads and building from source are in the
[installation guide](docs/getting-started/installation.md). strix shells out to
`git`, so it needs `git` on your `PATH`.

## Usage

```bash
strix              # open the repository in the current directory
strix path/to/repo # open a specific repository
```

- **Left pane** — staged files (top) and unstaged + untracked files (bottom).
  Move files between sections to stage/unstage; reset a file back to HEAD.
- **Right pane** — a syntax-highlighted diff of the selected file, in unified or
  side-by-side mode.

Press `i` for the **History view**: a branch/merge rail graph of the current
branch on the left, commit details or file diffs (vs the commit's first parent)
on the right. `Esc` or `i` returns to staging.

See the [keybindings](docs/getting-started/keybindings.md) for the full set.

## Documentation

The full site lives under `docs/` and builds with `mkdocs-material`
(`make docs-serve` → http://127.0.0.1:7071):

- [Installation](docs/getting-started/installation.md)
- [Usage](docs/getting-started/usage.md) and [Keybindings](docs/getting-started/keybindings.md)
- [Theming](docs/guides/theming.md) and [Configuration](docs/guides/configuration.md)
- [Architecture](docs/reference/architecture.md)

`CLAUDE.md` at the repo root captures the project conventions.

## License

MIT — see [LICENSE](LICENSE).
