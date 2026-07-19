---
name: strix-review
description: "Use when a human has left inline review comments on a strix diff and you need to act on them — e.g. the user says they left review comments or notes in strix, asks you to \"address my review comments\", \"check strix comments\", or \"go through the strix review\", or the request mentions the `strix comment` CLI or a strix review loop. This skill teaches the branch-scoped inbox contract: read the notes with `strix comment list --json`, fix each one, commit, then remove it with `strix comment rm <id>` to signal completion. Do NOT use it for operating the strix TUI itself (staging changes, viewing diffs, keybindings), for general git or GitHub PR review that does not involve strix comments, or for unrelated meanings of \"strix\"."
---

# strix review comments

A human reviewed a branch in strix (`strix diff <base>`) and left inline
comments on the diff. Those comments are a **branch-scoped inbox** on disk at
`.git/strix/comments.json` (technically `<common_dir>/strix/comments.json`, so
linked worktrees share it). Your job: read the inbox, address each comment, and
**remove it to mark it done**. Removal is the completion signal — a comment that
is gone is a comment that is handled.

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
  if no review session has run yet.
- `comments` — the notes to work through.

Each **Comment**:

| Field | Type | Meaning |
|---|---|---|
| `id` | integer | Unique, store-global. You pass this to `rm`. |
| `source` | `"human"` \| `"agent"` | `human` = a person's note (your work list). `agent` = one you added. |
| `file` | string | The file's new-side path. |
| `side` | `"old"` \| `"new"` | Which side of the diff `line` refers to. |
| `line` | integer | 1-based line number on that side. |
| `text` | string | The comment body (may contain newlines). |
| `context` | string \| `null` | The anchored line's text captured when the note was written. `null` means it was unavailable. |
| `orphaned` | boolean | `true` if strix could no longer match the anchor on its last pass — see below. |
| `created_at` | integer | Unix epoch seconds. |

Work the `source: "human"` comments. Ignore your own `agent` notes unless asked.

## Step 2 — address each comment, then remove it

For each human comment:

1. **Locate the code.** Open `file` and go to `line` on the given `side`. If the
   line numbers have drifted since the review (the branch moved on), **trust
   `context`** — it is the exact text of the line the human anchored to. Search
   for that text to find the real location; the number is only a hint.
2. **Fix it** in the code.
3. **Commit the fix first**, then remove the comment:

   ```bash
   strix comment rm <id>
   ```

   Order matters. The human's review shows **committed state only** — if you
   `rm` before committing, the note disappears from their screen while the
   problem is still visible in the diff, which reads as "resolved" when it
   isn't. Commit, *then* remove.

**If you are not authorized to commit:** do not remove anything. Leave every
comment in place and report which fixes are staged/ready, so the human can
commit and you (or they) can clear the inbox afterward.

## Orphaned comments

An `"orphaned": true` comment is one strix could no longer anchor — usually the
line it pointed at was edited or the file was renamed. **Do not guess** what it
meant, and do not delete it. It may already be addressed. Report it to the human
by id, file, and text, and leave it untouched unless they tell you what to do.

## Optional — leave your own notes

You can annotate the diff back to the human (e.g. flag a follow-up, ask a
question):

```bash
strix comment add --file <path> --new-line <N> --text "…"
# or --old-line <N> for the base side
```

Comments you add are **always `source: "agent"`** — the CLI has no way to author
a human note, and you must never try to. Do not edit or fake human comments, and
do not hand-edit `comments.json`; go through the CLI.

## Finish

When done, give the human a **per-comment summary**: for each id, what you did
(fixed and committed / reported as possibly-stale orphan / left for them to
commit). By default their open strix session refreshes automatically — on your
commits and on the store changes from your `rm`s — so removed comments vanish
from their diff as you go. (Auto-refresh is on by default but configurable; if
the human turned `auto_refresh` off, they press `r` to pull in your changes.)

## Command reference

The path (a repo other than the current directory) comes **before** the action:
`strix comment [PATH] <action>`. Add `--json` to any action for machine output.

| Command | Does |
|---|---|
| `strix comment list --json` | Read the current branch's inbox. |
| `strix comment add --file F (--old-line N \| --new-line N) --text S` | Add an agent note anchored to a line. |
| `strix comment rm <id>` | Remove one comment (the completion signal). |
| `strix comment clear` | Remove every comment on the branch. |
| `strix comment gc` | Drop inboxes for branches/commits that no longer exist. |
