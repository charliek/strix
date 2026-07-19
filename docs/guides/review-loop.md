# Review loop

The review loop is strix's core workflow for agent-written code: a human
reviews a branch and leaves inline comments on the diff; an agent reads those
comments, fixes each one, commits, and removes it. The diff itself is the
channel — no PR round-trip, no pasting line numbers into a chat window. This
page walks the loop end to end; the mechanics of each half are specified in
[Usage](../getting-started/usage.md#leaving-review-comments) and the
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
| `c` | Comment on the cursor's line (`Enter` saves, `Esc` cancels). On one of your own comments, edits it. |
| `]` / `[` | Jump to the next / previous comment, cycling across all files. |
| `x` | Delete the comment under the cursor. |

Each comment renders as its own row directly below the line it anchors to,
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

1. **`strix comment list --json`** is the inbox for the checked-out branch.
   Anchors are `file` + `side`/`line`; when line numbers have drifted, the
   stored `context` (the anchored line's exact text) is authoritative.
2. For each human comment: fix the code, **commit first, then
   `strix comment rm <id>`**. Removal is the completion signal, and the
   review shows committed state only — a comment removed before its fix lands
   would vanish while the problem is still on screen. An agent not authorized
   to commit leaves the comments in place and reports instead.
3. **`orphaned: true` → report, don't guess.** The agent leaves orphans
   untouched and surfaces them to the human.
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
