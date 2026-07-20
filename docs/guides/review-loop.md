# Review loop

The review loop is strix's core workflow for agent-written code: a human
leaves inline comments on a diff; an agent reads those comments, fixes each
one, and clears it. The diff itself is the channel — no PR round-trip, no
pasting line numbers into a chat window. There are two variants of the same
loop, distinguished by **scope**:

- **Committed range** (`strix diff <base>`) — a human reviews a branch
  against its base; clearing a comment is a deliberate signal, so it happens
  *after* the fix is committed.
- **Uncommitted work** (bare `strix`) — a human comments on the working tree
  *before* anything is committed, the most valuable review moment. Landing
  the fix in a commit clears the comment automatically — no separate removal
  step.

This page walks the committed-range loop first, then the working-tree
variant; the mechanics of each half are specified in
[Comments](../getting-started/usage.md#comments) and the
[CLI reference](../reference/cli.md#strix-comment).

## The human side

With the branch you're reviewing checked out, open it against its base:

```bash
strix diff main
```

The diff pane has a per-row cursor — `j`/`k` to move, `g`/`G` for first/last,
`Ctrl-d`/`Ctrl-u` half-page, or click a row. Then:

| Key | Action |
|-----|--------|
| `c`, or a double-click | Comment on the cursor's line, in a multi-line in-place editor (`Enter` saves, `Esc` cancels, `Shift+Enter`/`Ctrl-J`/`Alt+Enter` for a newline). On one of your own comments, edits it. |
| `]` / `[` | Jump to the next / previous comment, cycling across all files. |
| `X`, or clicking a box's `[x]` | Delete the comment under the cursor. |

Each comment renders as a bordered box directly below the line it anchors to,
and persists immediately to the store (below). When the pass is done, tell
the agent — with the skill installed, a plain **"address my strix comments"**
is enough.

!!! note
    Authoring requires that the reviewed head is your checked-out `HEAD` —
    the `strix diff main` case. Reviewing two other refs renders comment-free
    and `c` flashes a hint instead. This keeps your TUI inbox and the agent's
    CLI inbox provably the same set.

**Orphans.** A comment whose line merely moved (the branch gained commits)
re-anchors automatically when the same text is found within ten lines. One
whose line was *edited*, or that drifted too far to match honestly, is marked
**orphaned** rather than silently relocated — it renders in a `⚠`-marked
block (or the footer counter, if its file left the range) and waits for a
human decision. An orphan often means the note was already addressed; it is
never evidence by itself.

## Commenting on uncommitted work

The same box, editor, and keys work in bare `strix` (no `strix diff`) —
comment directly on whatever's staged or unstaged, before any of it is
committed. These are `scope: "worktree"` comments, anchored to the file's net
`HEAD`-vs-worktree change rather than a committed range:

```bash
strix   # bare — the Status view, not a review session
```

Leave a note with `c` or a double-click, same as above, then hand off to the
agent the same way: **"address my strix comments"**. The clearing rule is the
mirror image of the committed-range case:

- Fixing the code and **committing it** sweeps the comment out of the inbox
  automatically — the agent doesn't need to `rm` anything.
- If the agent isn't going to commit (or you'd rather not wait), `rm <id>`
  clears it directly — harmless here, since your strix session already shows
  the uncommitted fix as soon as it refreshes.

A worktree comment can also go **stale** (rendered dimmed): its anchored line
drifted while `HEAD` stayed put — you or the agent edited it again without
committing. A stale note isn't wrong, just out of date; it clears once the
content matches again or a commit lands. Treat it like an orphan — don't
guess, ask if unclear.

## The agent side

### Installing the skill

The bundled `strix-review` skill teaches an agent the contract below. Three
routes:

| Route | Command |
|-------|---------|
| [skills.sh](https://skills.sh) — Claude Code, GitHub Copilot, OpenCode, and many other agents | `npx skills add charliek/strix` |
| Claude Code native plugin | `/plugin marketplace add charliek/strix`, then `/plugin install strix@strix` |
| Any other agent | `strix skill path` writes the skill file to disk and prints its absolute path — point the agent at it |

`strix skill path` rewrites the file on every invocation, so it always
matches the installed binary — see
[`strix skill`](../reference/cli.md#strix-skill).

### What the skill teaches

1. **`strix comment list --json`** is the inbox for the checked-out branch —
   defaulting to `--scope worktree` when the repo is dirty, else the active
   reviewed range; the skill passes `--scope` explicitly when it matters.
   Anchors are `file` + `side`/`line`; when line numbers have drifted, the
   stored `context` (the anchored line's exact text) is authoritative.
2. Clearing depends on `scope`: a **`"range"`** comment — fix the code,
   **commit first, then `strix comment rm <id>`**. Removal is the completion
   signal, and the review shows committed state only — a comment removed
   before its fix lands would vanish while the problem is still on screen. A
   **`"worktree"`** comment — fix the code and commit as usual; that commit
   sweeps the comment on its own, or `rm <id>` clears it immediately with no
   commit needed. An agent not authorized to commit leaves range comments in
   place and reports instead (a worktree comment can still be `rm`'d).
3. **`orphaned: true` (either scope) or `stale: true` (worktree only) →
   report, don't guess.** The agent leaves both untouched and surfaces them
   to the human.
4. The agent can leave its own notes with `strix comment add` — always
   `source: "agent"`. It can never author or edit a human note; the CLI has
   no way to, and the skill forbids editing the store file by hand.

Your open strix session refreshes as the agent works: each `rm` removes the
comment row, each commit updates the diff.

## Where comments live

The store is a single JSON file at `<common_dir>/strix/comments.json` —
`.git/strix/comments.json` in a normal checkout. Comments are keyed by
branch, so each branch has its own inbox, and every checkout of the
repository shares one file: a linked worktree's `git_dir` is private, but its
*common* dir is the primary checkout's `.git`, so an agent working in a
worktree reads and writes the same store (against its own branch key).
Nothing ever leaves your machine.

!!! note "Upgrading from an older strix: a one-time comment reset"
    Worktree comments needed a store format change (`version` 2). The first
    time this version of strix (or `strix comment`) reads an older
    (`version` 1) store, it copies the file aside to
    `comments.json.v1.bak` and starts a fresh, empty store — any comments
    left by an older strix are **not carried forward**. This is a one-time,
    one-way reset (not a bug): back-compatibility with the old format wasn't
    worth the complexity for what's a disposable inbox, and your old
    comments aren't gone — they're sitting in the `.bak` file if you need to
    recover one by hand. Nothing about this touches comments you leave from
    here on.

## Sharp edges

- **Concurrent writers are last-writer-wins.** Every mutation re-reads the
  file before writing, but there is no cross-process lock: if the TUI and an
  agent write in the same instant, one whole-file write wins. A deletion can
  be resurrected, and two simultaneous `add`s can lose one comment. Accepted
  for a two-party (one human, one agent) workflow; the store stays parseable
  either way. Details in the
  [CLI reference](../reference/cli.md#concurrent-writers).
- **Commits from another checkout don't auto-refresh** your TUI — press `r`.
  (Store changes do refresh live, including from linked worktrees.)
- **Branch renames strand an inbox** under the old name until
  `strix comment gc` drops it (each drop is logged; recover by hand from the
  JSON if needed).
