# Usage

```bash
strix              # open the repository containing the current directory
strix path/to/repo # open a specific repository
```

strix opens on the alternate screen and restores your terminal on exit (even if
it panics).

## The layout

```
 strix  my-repo                                       main
╭ Changes ───────────────────╮╭ pending · HEAD→worktree ─────────────╮
│ Staged                     ││  src/app.rs                          │
│   M src/app.rs             ││ @@ -12,6 +12,7 @@                     │
│ Changes                    ││  12  pub struct App {                │
│   M src/ui/mod.rs          ││  13      pub repo_path: PathBuf,      │
│ ? notes.txt                ││ +14      pub focus: Focus,            │
╰────────────────────────────╯╰──────────────────────────────────────╯
 j/k move   space stage   d diff mode   q quit
```

- **Changes (left).** Two sections: **Staged** files on top, **Changes**
  (unstaged) and untracked files below. Each row shows a status marker
  (`M` modified, `A` added, `D` deleted, `?` untracked) coloured by state.
- **Diff (right).** The selected file's diff, syntax-highlighted, in **unified**
  or **side-by-side** mode. The line-number gutter shown above (`12`, `13`, …)
  is on by default; press `n` to hide it, or set `line_numbers = false` — see
  [Configuration](../guides/configuration.md).

## A typical session

1. Launch `strix` in your repo. The first changed file is selected and its diff
   is shown.
2. Move through files with `j`/`k` (or the mouse).
3. Press `space` to stage or unstage the selected file. It moves between the
   **Changes** and **Staged** sections.
4. Press `d` to flip between unified and side-by-side diffs.
5. Press `x` to discard a file's changes (you'll be asked to confirm).
6. Press `c` on a diff line (or double-click it) to leave yourself a note —
   see [Comments](#comments) below.
7. Commit with `git commit` in another pane — strix intentionally doesn't create
   commits.

## Comments

Both changesets strix reviews take inline comments: the working tree (bare
`strix`) and a branch review (`strix diff <base>`). Each renders as a
bordered box directly below its anchored line — `● you — <file> R<line>` for
your own notes, `● agent — …` for an agent's — in the theme's `comment`
accent (see [Theming](../guides/theming.md#custom-themes)). The file list
shows a `● n` badge on any file with comments.

The diff pane has a per-row **cursor** (`j`/`k` move it, `g`/`G` jump to the
first/last row, `Ctrl-d`/`Ctrl-u` half-page) — it renders with the selection
colour only while the diff pane has focus, and clicking a row moves it there
too (the scroll wheel never moves the cursor).

- **`c`**, or **double-clicking** a code line, opens an in-place editor there:
  an empty box for a new comment, or your existing comment if the line
  already has one. Double-clicking an existing box edits it the same way. An
  agent-authored box flashes `agent note — read-only` instead — the TUI edits
  human notes only.
- The editor is **multi-line**. `Enter` saves; `Esc` discards (reverts an
  edit in place, or cancels a new comment without saving it). A newline is
  `Shift+Enter` — with `Ctrl-J` and `Alt+Enter` as equally-supported
  fallbacks, since Shift+Enter isn't reliably reported by every terminal.
  Pasting multi-line text inserts real newlines, not one per keypress. The
  box expands as you type and the view scrolls to keep the caret visible.
  While editing, keys go to the editor *first*, so `c`/`x`/`]`/etc. all type
  literally instead of firing their usual action.
- **`X`**, or clicking a box's **`[x]`**, deletes the comment under the
  cursor — no confirmation. **`x` never deletes a comment**, in either view:
  it's `Action::Discard` (the Changes-pane discard key), and it's a silent
  no-op when the diff-pane cursor sits on a comment/orphan row instead.
- **`]` / `[`** jump to the next/previous comment. With none in view, they
  flash instead of moving.
- Every comment-acting key follows **act-and-reveal**: on an offscreen
  cursor, the first press only scrolls it into view; a second press acts —
  nothing is ever added to or deleted from a row you can't see.

Comments persist immediately to `.git/strix/comments.json` on every add,
edit, or delete — a separate file from, and unrelated to, the `config.toml`
write-back that `t`/`d`/`n` do (see
[Configuration](../guides/configuration.md#runtime-changes-persist)); nothing
about comments ever touches `config.toml`. An agent (or another `strix`
instance, in another checkout) reads and edits that same inbox via
`strix comment list|add|rm|clear --json` — see
[CLI](../reference/cli.md#strix-comment) for the full contract.

### On uncommitted work

In bare `strix`, `c` (or a double-click) comments on the **Status diff
pane**, which always shows the selected file's net `HEAD`-vs-worktree
change, labeled `pending · HEAD→worktree` (see the mockup at the top of this
page). A conflicted file has no clean anchor to comment on; binary and
submodule files have no code lines to anchor to either.

These comments track the working tree rather than a fixed diff:

- Staging or unstaging a file with no further content change leaves its
  comments alone — the net diff is unchanged.
- Editing the anchored line while `HEAD` stays put marks the comment
  **stale** (dimmed) instead of deleting it — it clears on its own once the
  content matches again, or once a commit lands.
- **Committing** the change a comment anchors to sweeps it out of the inbox
  automatically — nothing to do by hand.
- An unrelated edit, an unrelated commit, or the line merely scrolling out of
  the rendered diff never touches a comment.

### On a reviewed range

See [Leaving review comments](#leaving-review-comments) below — the box,
editor, and keys are identical to the working-tree case above. What differs
is **committed-state-only** review semantics and how orphans surface across
a whole file list.

## History view

Press `i` (or `2`) to switch to the **History view**; `Esc`, `i`, or `1` returns
to the staging view. The left column changes shape:

```
╭ Committed Changes ─────────╮╭ Commit a1b2c3d ──────────────────────╮
│ ● a1b2c3d Add history view ││ commit a1b2c3d4e5f6…                  │
│   M src/app.rs             ││ Author  …                             │
│   A src/git/history.rs     ││ Date    2026-05-30 14:02 +0000        │
│   M src/ui/mod.rs          ││                                       │
├ Graph ─────────────────────┤│     Add history view                  │
│ ● a1b2c3d Add history view ││                                       │
│ │ 9f8e7d6 Fix diff scroll  ││  3 files changed, +120 −14            │
│ ● 7c6b5a4 Docs install     ││   M src/app.rs   +40 −2               │
╰────────────────────────────╯╰───────────────────────────────────────╯
 j/k move   tab pane   d split   b hide   i/esc status
```

- **Graph (bottom-left).** Commit log of the current branch (HEAD ancestry,
  including merges), with a colored branch/merge rail. Move with `j`/`k`, click
  a row to select it.
- **Committed Changes (top-left).** The selected commit followed by its changed
  files. The commit row (`●`) is selectable; the right pane swaps between
  commit details and a file diff based on what you pick.
- **Right pane.** Commit details (full hash, author, date, message, per-file
  stat) when the `●` row is selected; the file's diff vs the commit's first
  parent when a file is selected — same renderer as the status view.
- **Layout.** The vertical split bar resizes the left column vs the diff; the
  horizontal one resizes Committed Changes vs Graph. Both are drag-to-resize.
- **`b`** collapses the entire left column the same way it does in the status
  view, leaving the diff (or commit details) full-width.

## Reviewing a branch

```bash
strix diff main            # review the current branch against main
strix diff v1.2.0...HEAD   # review HEAD against v1.2.0's merge base
```

`strix diff <RANGE>` opens a **read-only** review session in place of the
staging view — the changeset is a branch against its base rather than the
working tree:

```text
 strix  my-repo · main…HEAD                                                
╭ Changes ───────────────────╮╭ Diff · unified ──────────────────────╮
│ M src/app.rs       +40 −2  ││  src/app.rs                          │
│ A src/git/review.rs +180   ││ @@ -12,6 +12,7 @@                     │
│ M src/ui/mod.rs    +12 −3  ││  12  pub struct App {                │
│                             ││ +14      pub focus: Focus,            │
╰────────────────────────────╯╰──────────────────────────────────────╯
 j/k move   tab pane   d split   n line #s   t theme   b hide   i history   q quit
```

`RANGE` is one of:

| Form      | Meaning                              |
|-----------|---------------------------------------|
| `BASE`    | `merge-base(BASE, HEAD)..HEAD` — the common case, e.g. `strix diff main` |
| `A...B`   | `merge-base(A, B)..B`                 |
| `A..B`    | `A..B` literally, no merge-base       |

An empty side means `HEAD`, matching `git`: `main..` is `main..HEAD`, `...feat`
is `HEAD...feat`. The bare-`BASE` and `A...B` forms use the merge base — the
same "what does this branch add" (three-dot, GitHub-PR) semantics `git diff
main...HEAD` and a GitHub pull request diff use — not a direct two-sided
comparison.

A few things behave differently here than in the staging view:

- **Committed state only.** A range compares two commits, so uncommitted
  changes on the reviewed branch never appear — commit them, or use the
  regular status view (`strix`, no subcommand) to see the working tree.
- **Read-only for staging.** Staging keys (`space`, `Enter`, `s`, `u`, `x`,
  and clicking a file's status marker) do nothing: no modal, no index change.
  Comment deletion here is `X`, a separate action from `x` — see below.
- **Live updates.** As new commits land on the reviewed branch, the file list
  and the currently open diff refresh automatically, the same auto-refresh
  path the staging view uses.
- **Navigation.** `i` opens the History view, same as in the staging view;
  `1` returns to the review session (also from History); `2` opens History as
  well (not a toggle); `Esc` does **not** exit the review session — quit with
  `q`.

An unresolvable range (unknown revision, an operand that is not a commit —
e.g. a blob — or no merge base between the two sides) fails before the TUI
opens: strix exits non-zero and prints a message
naming the offending operand. See [CLI](../reference/cli.md) for the full
grammar, the merge-base caveat, and exit behavior.

### Leaving review comments

A review session takes the same box, in-place editor, mouse, and key
bindings as [Comments](#comments) above — `c`/double-click to add or edit,
`X`/`[x]` to delete, `]`/`[` to navigate — anchored to this range's diff
instead of the working tree. What's different here:

**Orphans reach further.** A comment whose anchored line moved is re-anchored
automatically when the surrounding text still matches nearby; one whose line
was edited (or that drifted too far to match honestly) is marked
**orphaned** instead of silently relocated. Orphans on a file still in the
range show in a `⚠`-marked block at the top of that file's diff — even if the
diff itself is binary or has no textual lines to anchor to. Orphans on a file
that dropped out of the range entirely (renamed away, or no longer part of
the diff) can't be shown next to anything, so they're rolled into a footer
counter instead: `⚠ N orphaned — strix comment list`.

**Authoring requires reviewing your checked-out branch.** Comments are tied to
the branch you're actually on: open a session with `strix diff main` while
that branch is checked out, and `c`/double-click work normally. If the
reviewed head isn't your current `HEAD` (say, you `git checkout`d elsewhere
mid-review, or you're comparing two other refs), the session renders
comment-free and `c` flashes `check out the reviewed branch to comment` —
this keeps the human TUI inbox and the agent CLI inbox (below) provably the
same set.

**Committed state only, again.** Removing a comment is the signal that its
issue is resolved, so an agent addressing your notes commits its fix first,
then removes the comment — the review only ever shows committed code, so a
comment removed before its fix lands would vanish while the problem is still
on screen. (A worktree comment, above, works the other way — a commit sweeps
it automatically.)

## Inspecting a frame

For debugging or scripting, `--dump-frame` renders one frame to stdout as text
and exits:

```bash
strix --dump-frame --width 120 --height 40
```

See [Keybindings](keybindings.md) for the complete key map and
[CLI](../reference/cli.md) for all flags.
