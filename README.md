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

Two panes, mouse + keyboard, syntax-highlighted diffs, themeable (cycle at
runtime with `t`), toggleable line numbers (`n`) — plus a read-only
**branch review** mode (`strix diff main`) for reviewing a branch against its
base, GitHub-PR style, with **inline review comments** (`c`) an agent can
read via `strix comment list --json` and remove via `strix comment rm`. Built in Rust on
[ratatui](https://github.com/ratatui/ratatui),
[gitoxide](https://github.com/GitoxideLabs/gitoxide), and
[syntect](https://github.com/trishume/syntect).

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
strix diff main     # review the current branch against main (read-only)
```

- **Left pane** — staged files (top) and unstaged + untracked files (bottom).
  Move files between sections to stage/unstage; reset a file back to HEAD.
- **Right pane** — a syntax-highlighted diff of the selected file, in unified or
  side-by-side mode.

Press `i` for the **History view**: a branch/merge rail graph of the current
branch on the left, commit details or file diffs (vs the commit's first parent)
on the right. `Esc` or `i` returns to staging.

`strix diff <RANGE>` opens a read-only review of a range. The bare `BASE`
and `A...B` forms compare against the merge base — the same "what does this
branch add" comparison a GitHub pull request shows — while `A..B` compares
the two revisions directly. See
[Usage](docs/getting-started/usage.md#reviewing-a-branch) for the
range grammar and [keybindings](docs/getting-started/keybindings.md) for the
full key set.

## Agent review loop

The review comments close a loop: you review a branch with `strix diff main`,
press `c` to leave inline notes on the diff, then tell your agent to "address
my strix comments". The agent reads the inbox with `strix comment list --json`,
fixes each note, commits, and removes it — resolved comments vanish from your
open review as it works. The bundled `strix-review` skill teaches an agent
that contract. Install it via [skills.sh](https://skills.sh), which targets
Claude Code, GitHub Copilot, OpenCode, and many other agents:

```bash
npx skills add charliek/strix
```

For Claude Code, a native plugin is also available:

```
/plugin marketplace add charliek/strix
/plugin install strix@strix
```

Any other agent can be pointed at the skill file directly: `strix skill path`
writes the bundled skill to disk and prints its absolute path. See the
[review loop guide](docs/guides/review-loop.md) for the full workflow.

## Documentation

The full site lives under `docs/` and builds with `mkdocs-material`
(`make docs-serve` → http://127.0.0.1:7071):

- [Installation](docs/getting-started/installation.md)
- [Usage](docs/getting-started/usage.md) and [Keybindings](docs/getting-started/keybindings.md)
- [Review loop](docs/guides/review-loop.md) — the human-comments → agent-fixes workflow
- [Theming](docs/guides/theming.md) and [Configuration](docs/guides/configuration.md)
- [CLI reference](docs/reference/cli.md) and [Architecture](docs/reference/architecture.md)

`CLAUDE.md` at the repo root captures the project conventions.

## License

MIT — see [LICENSE](LICENSE).
