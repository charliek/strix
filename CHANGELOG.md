# Changelog

All notable changes to strix are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each release below is headed by a `## vX.Y.Z` entry added by
`/release-workflows:release`; `release.yml` turns that section into the GitHub
Release notes.

## v0.0.3 — 2026-07-20

The review-workflow track: strix becomes the review surface for agent-written
code. Everything merged since v0.0.2 (milestones 5–8).

### Added
- **Branch / range review** — `strix diff <base>` opens a read-only review of a
  changeset against its merge-base (three-dot default), live-updating as the
  branch tip moves.
- **Review comments** — leave inline comments on a reviewed diff in the TUI; an
  agent reads and addresses them via `strix comment list|add|rm|clear|gc --json`
  (branch-scoped inbox in `<common_dir>/strix/comments.json`, ±10-line honest
  re-anchoring). Ships the `strix-review` agent skill + `strix skill path`.
- **Working-tree comments** — comment on *uncommitted* code in bare `strix`.
  Comments anchor to the net `HEAD→worktree` diff and follow a baseline-OID
  lifecycle: a note clears itself when its fix is committed, and goes `stale`
  (surfaced, not deleted) if the line drifts under an unchanged `HEAD`.
- **Rich comment UI** — inline bordered comment **boxes** with an in-place
  multi-line **editor** (newline via Shift+Enter, Ctrl-J, or Alt+Enter;
  bracketed paste), **double-click** to add/edit, `[x]` to delete, and a
  dedicated `X` delete key. Side-by-side **word-level diff emphasis** highlights
  the changed spans of a modified pair (`add_emph`/`del_emph`, themable).
- **CLI** — `strix comment --scope worktree|range|all` (+ `--range`), defaulting
  to worktree in a dirty repo, with a headless re-anchor/sweep so `list` isn't
  stale.
- Line-numbers toggle (`n`), runtime theme cycling (`t`), and config write-back
  for `t`/`d`/`n` (preserves comments/unknown keys, never clobbers invalid config).

### Changed
- The **Status diff pane** now always shows the selected file's net
  `HEAD→worktree` change (labeled `pending · HEAD→worktree`) instead of the
  section-scoped HEAD-vs-index / index-vs-worktree diff, so comments render on
  the diff they anchor to. The Changes list keeps its staged/unstaged sections;
  the pane is identical to before for any file that is only staged or only
  unstaged.
- The comments store is now **version 2** (adds per-comment `scope`/`base`/`stale`).
  A version-1 store from earlier builds is backed up to `comments.json.v1.bak`
  and reset on first load — **prior comments are dropped** (a one-time upgrade).
- `x` no longer deletes a comment (it stays a staging key); use `X`.

### Docs
- Full docs + in-app Help overlay coverage for the review track: branch review,
  the human/agent comment loops (committed range and pre-commit worktree),
  keybindings, the `strix comment` CLI + JSON contract, theming
  (`add_emph`/`del_emph`), and the row-model / net-worktree / sweep architecture.

### CI
- `pull_request` trigger broadened so stacked PRs (non-`main` bases) also get CI.

## v0.0.2 — 2026-05-31

### Added
- **History view.** Toggleable second view (`i` / `1` / `2`, `Esc` to leave)
  with a colored branch/merge rail graph of the current branch on the bottom
  left, the selected commit's changed files on the top left, and either commit
  details (`git show`-style) or the file's diff vs its first parent on the
  right. The vertical split bar resizes the left column vs the diff; a new
  draggable horizontal divider resizes the file list vs the graph. `b`
  collapses the entire left column, just as it does in the status view.

### Docs
- Homebrew + apt install methods documented.
- History view coverage added across keybindings, usage, index, README,
  configuration, and architecture.
- Retired the original planning doc (`docs/spec.md`); folded its still-relevant
  non-goals and performance targets into `docs/reference/architecture.md`.

### CI
- Bumped `create-github-app-token` v2 → v3 (Node 24).

## v0.0.1 — 2026-05-30

First release. strix is a focused, polished terminal UI for the two git
operations done most often: staging changes and viewing diffs.

### Added
- Staging panel — stage, unstage, and discard changes; repository status read via
  `git status --porcelain`.
- Diff viewing — unified and side-by-side modes with synced scrolling and syntax
  highlighting.
- First-class mouse *and* keyboard control, including a draggable Changes/Diff
  split bar and a key to collapse the Changes panel for a full-width diff.
- Themes and configurable keybindings.
- Auto-refresh — the view updates live on filesystem and git changes.
- Help overlay and error toasts.

<!-- New release sections are inserted above this line. -->
