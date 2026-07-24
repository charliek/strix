# Changelog

All notable changes to strix are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each release below is headed by a `## vX.Y.Z` entry added by
`/release-workflows:release`; `release.yml` turns that section into the GitHub
Release notes.

## v0.0.6 — 2026-07-24

Milestone 10 — diff-review ergonomics: line wrapping, scrolling across files and
across long lines, a broader syntax set, and a more complete View menu. Merged
via #14 (plus the #13 filler follow-up).

### Added
- **Line wrapping** — long diff lines wrap instead of truncating, in both unified
  and side-by-side views. Off by default; toggle with `w` or the **View** menu
  (`wrap_lines`, persisted). A wrapped line stays one unit for the cursor,
  selection, clicks, and scroll-into-view; continuation rows carry a blank gutter.
- **Cross-file scroll** — scrolling past the bottom of one file's diff advances to
  the next file (and past the top to the previous), keeping the changes panel in
  sync. Off by default; toggle with `f` or the **View** menu (`cross_file_scroll`,
  persisted). Applies in the Status and Review surfaces; per-file diffs stay lazy.
- **Horizontal scroll** — trackpad left/right scrolls long lines sideways when
  wrapping is off; gutters and comment boxes stay put. (Terminals that don't emit
  horizontal wheel events simply won't trigger it.)
- **Wider syntax highlighting** — Kotlin, TypeScript / TSX, Swift, TOML,
  Dockerfile, Zig, Dart, Nix and more now highlight, via the `two-face` syntax
  set. Bare filenames like `Dockerfile` and aliases (`.mjs`/`.cjs`, `.tmpl`) are
  recognised too.
- **Changes panel** entry in the **View** menu — the panel toggle (`b`) is now
  also a menu row.

### Changed
- **Side-by-side empty-column filler** is neutralized (#13): the column opposite a
  pure addition / deletion renders as plain background again, dropping the tint
  introduced in v0.0.5.

## v0.0.5 — 2026-07-21

Milestone 9 — the top menu bar (completing the hunk-inspired review track) plus a
polish batch. Merged via #12.

### Added
- **Top menu bar** — a mouse-first menu bar integrated into the header with two
  dropdowns: **View** (unified / side-by-side, line numbers, Status / History)
  and **Theme** (pick from the available themes). Click a title to open; navigate
  an open menu with the arrows / Tab / Enter / Esc. Visible by default
  (`menu_bar = false` disables it), toggled with `m` (the remappable
  `toggle-menu-bar` action); menu changes persist to `config.toml`.
- **Native Shift+Enter** in the comment editor — strix now enables a terminal
  keyboard-enhancement protocol, so Shift+Enter inserts a newline on terminals
  that support it (Ctrl-J and Alt+Enter remain as fallbacks).
- **Side-by-side gutter tint** — in split view the empty column opposite a *pure*
  addition / deletion is now shaded (dim green / dim red) instead of flat
  background, so the changed region reads as anchored. New themable `add_gutter` /
  `del_gutter` colours (all presets + custom themes; unset inherits the preset).

### Changed
- **`strix diff` with no range** now opens the working-tree Status surface (the
  same view as bare `strix`). `strix diff <range> [path]` is unchanged; a lone
  positional is still always a range.

## v0.0.4 — 2026-07-20

Release-pipeline fix. **No functional changes to strix** — the binary is
identical to v0.0.3.

### CI
- `finalize-release` now passes `-R "${GITHUB_REPOSITORY}"` to `gh release edit`.
  The job has no checkout, so `gh` couldn't infer the repo (`fatal: not a git
  repository`) and exited 1 — which skipped the Homebrew formula update and the
  apt republish for v0.0.3. This release republishes both through the fixed
  pipeline (the v0.0.3 GitHub release itself, with its assets, shipped fine).

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
