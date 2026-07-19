---
name: strix-review
description: "Use when a human has left inline review comments on a strix diff and you need to act on them — e.g. the user says they left review comments or notes in strix, asks you to \"address my review comments\", \"check strix comments\", or \"go through the strix review\", or the request mentions the `strix comment` CLI or a strix review loop. This skill teaches the branch-scoped inbox contract: read the notes with `strix comment list --json`, fix each one, commit, then remove it with `strix comment rm <id>` to signal completion. Do NOT use it for operating the strix TUI itself (staging changes, viewing diffs, keybindings), for general git or GitHub PR review that does not involve strix comments, or for unrelated meanings of \"strix\"."
---

# strix review comments

A human left you inline comments in strix — either on a reviewed branch
(`strix diff <base>`) or directly on **uncommitted code** in bare `strix`.
Either way, the notes are a **branch-scoped inbox** on disk at
`.git/strix/comments.json` (technically `<common_dir>/strix/comments.json`, so
linked worktrees share it). Your job: read the inbox, address each comment, and
clear it to mark it done. **How you clear it depends on its scope:**

- **`scope: "range"`** (a committed review) — remove it with `strix comment rm
  <id>` **after** committing the fix. Removal is the completion signal: it
  must lag the commit (see Step 2).
- **`scope: "worktree"`** (uncommitted code) — landing the fix in a commit
  **sweeps the comment automatically**, no `rm` needed. You can also `rm` it
  directly with no commit at all; removal here is just tidying, not a
  completion signal, since the human's own strix session already shows an
  uncommitted fix as soon as they refresh.

Every `strix comment` action operates on the inbox for the **current `HEAD`**,
keyed by the checked-out branch's short name — or, when `HEAD` is detached, the
full commit hex; or, on an unborn branch, that branch's name. There is no
`--branch` flag.

**Run `strix comment list` from the exact repo state the human reviewed — do not
switch branches, check out anything, or move `HEAD` first.** The inbox follows
`HEAD`, so checking out a different branch (or detaching `HEAD`) points you at a
*different* inbox and you'll read the wrong comments (or none). If you need to
operate on another repo, pass its path (`strix comment <PATH> list`) rather than
changing your own checkout.

## Step 1 — read the inbox

```bash
strix comment list --json
```

With no `--scope`, `list` picks whichever surface you're most likely working
on: `worktree` if the repo has uncommitted changes, else the branch's last
reviewed `range`. Pass `--scope worktree`, `--scope range`, or `--scope all` to
be explicit instead — `list` re-anchors/sweeps the requested scope first, so
the result is never stale even in a fresh, headless run.

The envelope:

```json
{
  "branch": "feature-x",
  "range": "main",
  "comments": [ { …Comment… }, … ]
}
```

- `branch` — the branch key these comments belong to (the checked-out branch).
- `range` — the last `strix diff` argument recorded for this branch, or `null`
  if no review session has run yet. (Independent of which scope you asked for.)
- `comments` — the notes to work through, filtered to the requested scope.

Each **Comment**:

| Field | Type | Meaning |
|---|---|---|
| `id` | integer | Unique, store-global (crosses scopes). You pass this to `rm`. |
| `source` | `"human"` \| `"agent"` | `human` = a person's note (your work list). `agent` = one you added. |
| `scope` | `"worktree"` \| `"range"` | Which surface the comment lives on — see the top of this doc for how each clears. |
| `range` | string | Only present when `scope` is `"range"`: the reviewed range spec (e.g. `"main"`). |
| `file` | string | The file's new-side path. |
| `side` | `"old"` \| `"new"` | Which side of the diff `line` refers to. |
| `line` | integer | 1-based line number on that side. |
| `text` | string | The comment body (may contain newlines). |
| `context` | string \| `null` | The anchored line's text captured when the note was written. `null` means it was unavailable. |
| `orphaned` | boolean | `true` if strix could no longer match the anchor on its last pass — see below. |
| `base` | string \| `null` | Worktree comments only: the `HEAD` commit recorded when the note was written. |
| `stale` | boolean | Worktree comments only: `true` if the anchored line drifted while `HEAD` stayed the same — see below. |
| `created_at` | integer | Unix epoch seconds. |

Work the `source: "human"` comments. Ignore your own `agent` notes unless asked.

## Step 2 — address each comment, then clear it

For each human comment:

1. **Locate the code.** Open `file` and go to `line` on the given `side`. If the
   line numbers have drifted since the review (the branch moved on), **trust
   `context`** — it is the exact text of the line the human anchored to. Search
   for that text to find the real location; the number is only a hint.
2. **Fix it** in the code.
3. **Clear it, per its `scope`:**

   - **`"range"`** — **commit the fix first**, then remove the comment:

     ```bash
     strix comment rm <id>
     ```

     Order matters. The human's review shows **committed state only** — if you
     `rm` before committing, the note disappears from their screen while the
     problem is still visible in the diff, which reads as "resolved" when it
     isn't. Commit, *then* remove.

   - **`"worktree"`** — just commit the fix as you normally would; that commit
     **sweeps the comment out of the inbox on its own** (the same rule strix's
     live TUI session uses), no `rm` required. If you'd rather not wait for a
     commit — or aren't going to make one — `strix comment rm <id>` clears it
     immediately. Unlike range scope, an early `rm` here is harmless: the
     human's strix session already shows your uncommitted fix as soon as they
     refresh, so removal is just tidying, not the signal that the fix landed.

**If you are not authorized to commit:** for a `"range"` comment, do not remove
anything — leave it in place and report which fixes are staged/ready, so the
human can commit and clear the inbox afterward. For a `"worktree"` comment you
may still `rm` it once the fix is made, since clearing it never requires a
commit.

## Orphaned and stale comments

An `"orphaned": true` comment is one strix could no longer anchor — usually the
line it pointed at was edited or the file was renamed. A `"stale": true`
comment (worktree scope only) means the anchored line drifted while `HEAD`
stayed the same — it clears on its own once the content matches again or the
change is committed. Treat both the same way: **do not guess** what either
meant, and do not delete it. It may already be addressed. Report it to the
human by id, file, and text, and leave it untouched unless they tell you what
to do.

## Optional — leave your own notes

You can annotate the diff back to the human (e.g. flag a follow-up, ask a
question):

```bash
strix comment add --file <path> --new-line <N> --text "…"
# or --old-line <N> for the base side

# Defaults to --scope worktree. To annotate a committed review instead:
strix comment add --file <path> --new-line <N> --text "…" --scope range [--range <RANGE>]
# --range defaults to the branch's last-reviewed range; give one explicitly if
# none has been recorded yet (add errors if neither is available).
```

Comments you add are **always `source: "agent"`** — the CLI has no way to author
a human note, and you must never try to. Do not edit or fake human comments, and
do not hand-edit `comments.json`; go through the CLI.

## Finish

When done, give the human a **per-comment summary**: for each id, what you did
(fixed and committed / fixed and `rm`'d without committing (worktree only) /
reported as an orphan or stale / left for them to commit). By default their
open strix session refreshes automatically — on your commits and on the store
changes from your `rm`s — so cleared comments vanish from their view as you go
(a swept worktree comment vanishes on the commit alone). Auto-refresh is on by
default but configurable; if the human turned `auto_refresh` off, they press
`r` to pull in your changes.

## Command reference

The path (a repo other than the current directory) comes **before** the action:
`strix comment [PATH] <action>`. Add `--json` to any action for machine output.

| Command | Does |
|---|---|
| `strix comment list [--scope worktree\|range\|all] --json` | Read the current branch's inbox (defaults per the repo's dirty state; re-anchors/sweeps first). |
| `strix comment add --file F (--old-line N \| --new-line N) --text S [--scope worktree\|range] [--range R]` | Add an agent note (defaults to `--scope worktree`). |
| `strix comment rm <id>` | Remove one comment, in either scope — the completion signal for a `"range"` comment, just tidying for a `"worktree"` one. |
| `strix comment clear --scope worktree\|range\|all` (or `--all`) | Remove every comment in that scope. A scope is required — `clear` never wipes everything implicitly. |
| `strix comment gc` | Drop inboxes for branches/commits that no longer exist. |
