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

## Build from source

strix needs Rust 1.96.0 (pinned in `rust-toolchain.toml`; `mise install` or
rustup will install it automatically).

```bash
git clone https://github.com/charliek/strix
cd strix
cargo build --release      # → target/release/strix
```

## Usage

```bash
strix              # open the repository in the current directory
strix path/to/repo # open a specific repository
```

- **Left pane** — staged files (top) and unstaged + untracked files (bottom).
  Move files between sections to stage/unstage; reset a file back to HEAD.
- **Right pane** — a syntax-highlighted diff of the selected file, in unified or
  side-by-side mode.

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
