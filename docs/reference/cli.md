# CLI

```
strix [OPTIONS] [PATH]
strix diff <RANGE> [PATH]
strix comment [PATH] <list|add|rm|clear|gc>
strix skill path [--json]
```

The root form opens the staging view. `strix diff <RANGE>` opens a read-only
review session comparing two commits instead — see
[Reviewing a branch](../getting-started/usage.md#reviewing-a-branch).
`strix comment` reads and edits the checked-out branch's comments inbox
without opening the TUI at all — the agent-facing surface for the notes a
human leaves either on uncommitted work in bare `strix` or in a review
session — see [`strix comment`](#strix-comment) below. `strix skill` manages
the bundled agent skill that teaches that workflow — see
[`strix skill`](#strix-skill).

## Root: `strix [PATH]`

| Argument | Description                                              |
|----------|----------------------------------------------------------|
| `PATH`   | Repository to open. Defaults to the current directory.   |

## `strix diff <RANGE> [PATH]`

| Argument | Description                                              |
|----------|----------------------------------------------------------|
| `RANGE`  | Required. The range to review — see grammar below.       |
| `PATH`   | Repository to open. Defaults to the current directory.   |

### RANGE grammar

`RANGE` is split on the first `...`, else the first `..` (`...` is checked
first, so `A...B` never parses as `A.`/`.B`). An empty side means `HEAD`,
matching `git`.

| Form    | Meaning                              | Example                          |
|---------|---------------------------------------|-----------------------------------|
| `BASE`  | `merge-base(BASE, HEAD)..HEAD`        | `strix diff main`                 |
| `A...B` | `merge-base(A, B)..B`                 | `strix diff v1.2.0...feat`        |
| `A..B`  | `A..B` literally, no merge-base       | `strix diff v1.2.0..v1.3.0`       |
| `main..` | `main..HEAD`                         | empty right side ⇒ `HEAD`         |
| `...feat` | `HEAD...feat`                       | empty left side ⇒ `HEAD`          |

The bare-`BASE` and `A...B` forms use the merge base — three-dot, GitHub-PR
semantics: "what has this branch added since it diverged," not a direct
two-sided comparison. Both operands are peeled through annotated tags to a
commit; a resolvable non-commit (e.g. a tree or blob) is rejected.

!!! note
    **Criss-cross merges** can have more than one valid merge base. strix uses
    gix's `merge_base`, which picks one of the best candidates; this can, in
    rare cases, differ from the base `git merge-base` itself would pick.

### `./diff` disambiguation

`diff` and `comment` (reserved subcommand names) take precedence over the
root `PATH` positional, so `strix diff` and `strix comment` are always the
subcommands — neither opens a directory literally named `diff` or `comment`.
To open such a directory, prefix it with a path segment: `strix ./diff`,
`strix ./comment`.

### Exit behavior

An unresolvable `RANGE` (unknown revision, a non-commit operand, or no merge
base between the two sides) fails before the TUI opens: strix exits non-zero
and prints a message to stderr naming the offending operand and the kind of
failure. A missing `RANGE` on `strix diff` is a clap usage error.

## `strix comment`

```text
strix comment [PATH] list [--scope worktree|range|all] [--json]
strix comment [PATH] add --file <FILE> (--old-line N | --new-line N) --text <TEXT> [--scope worktree|range] [--range <RANGE>] [--json]
strix comment [PATH] rm <ID> [--json]
strix comment [PATH] clear (--scope worktree|range|all | --all) [--json]
strix comment [PATH] gc [--json]
```

Reads and edits the comments inbox — the notes a human leaves either on
**uncommitted work** in bare `strix`, or on a `strix diff` session in the TUI
(see [Comments](../getting-started/usage.md#comments)) — without opening the
TUI. This is the agent-facing half of the loop: an agent runs `list --json`
to see what a human flagged, fixes each item, then clears it — how depends
on the comment's scope (see [Scopes](#scopes) below).

### Scopes

Every comment belongs to exactly one **scope**:

- **`worktree`** — anchored to the uncommitted working tree (the net
  `HEAD`-vs-worktree diff of a file, same as the Status diff pane). Landing
  the fix in a commit sweeps the comment out of the inbox automatically (see
  [Usage](../getting-started/usage.md#on-uncommitted-work)); `rm` also clears
  it directly, with no commit required.
- **`range`** — anchored to a committed `strix diff <RANGE>` review. Clearing
  one is a deliberate signal: `rm` it **after** committing the fix, never
  before (see [Usage](../getting-started/usage.md#on-a-reviewed-range)).

`list --scope` defaults to `worktree` when the repository has uncommitted
changes (the common agent case just after editing), else to the branch's
**active range** — the last range a `strix diff <RANGE>` session recorded for
this branch, kept as metadata on the branch's inbox entry. Pass `--scope`
explicitly (`worktree`, `range`, or `all`) to override the default. `add`
defaults to `--scope worktree`; a `--scope range` add uses `--range` if given,
else the branch's active range, and errors if neither is available (there is
no implicit empty range). `--scope all` is valid for `list`/`clear` only —
`add` always creates exactly one comment in exactly one scope. Every
`list`/`add` call re-anchors and sweeps the scope(s) it touches first
(write-elided when nothing changed), so the result is never stale even in a
fresh, headless run — see [`worktree_facts`](architecture.md#comments-store)
for the worktree lifecycle this reuses from the TUI.

`PATH` is the repository to operate on (defaults to the current directory)
and, notably, comes **before** the action — `strix comment ../other-repo
list`, not `strix comment list ../other-repo`. `comment` is a reserved
subcommand name like `diff`; to open a directory literally named `comment`,
prefix it: `strix ./comment`.

### Branch scoping

Every action operates on the **current `HEAD`'s inbox**, keyed by:

- the checked-out branch's short name (the common case);
- the full 40-character commit hex, if `HEAD` is detached;
- the unborn branch's name, if `HEAD` doesn't have a commit yet.

There is no `--branch` override — the inbox always follows what's checked
out, on purpose: it's what keeps the human's TUI inbox (gated on the review
head matching `HEAD`, see the usage guide) and this CLI's inbox provably the
same set. A linked worktree on its own branch resolves its own key against
the *same* store file — the store lives at
`<common_dir>/strix/comments.json`, shared by every checkout of the
repository (`common_dir`, not `git_dir`; see
[Architecture](architecture.md#git-layer)).

### Output discipline

- Machine-readable output is JSON **on stdout**, and only with `--json`;
  without it, `list` prints a plain aligned table (`⚠` marks an orphan) and
  the other actions print a short human-readable line.
- Diagnostics and warnings (e.g. a best-effort re-anchor pass that failed)
  go **to stderr**, never stdout — `--json` output is never interleaved with
  anything else.
- Failure is a **non-zero exit with a stderr message**; there is no JSON
  error envelope on any exit path, `--json` or not. A comments store that
  fails to parse, or whose `version` is newer than this build understands,
  fails the action *before any write* — the file is left untouched.

### Actions

| Action | Behavior | `--json` output |
|---|---|---|
| `list [--scope worktree\|range\|all]` | Re-anchors/sweeps the selected scope(s) first (best-effort — see below), then lists them. Defaults per [Scopes](#scopes). A branch with nothing in the selected scope returns an empty inbox, exit 0. | `{"branch": "<key>", "range": "<string>"\|null, "comments": [Comment, ...]}` |
| `add --file <FILE> (--old-line N \| --new-line N) --text <TEXT> [--scope worktree\|range] [--range <RANGE>]` | Adds a comment, always `source: "agent"` — there is no flag to author a human note; the CLI can't. Exactly one of `--old-line`/`--new-line` (both 1-based, ≥ 1); `--text` must be non-empty after trimming (its raw bytes, including newlines, are stored verbatim). Defaults to `--scope worktree`, stamping the current `HEAD` as the comment's baseline; `--scope range` needs `--range` or a stored active range, and errors if neither is available. `--scope all` is rejected. | `{"comment": Comment}` (plain: prints the new id) |
| `rm <ID>` | Removes one comment by id from the current branch — crosses scopes, since ids are store-global. An unknown id fails: `comment <ID> not found on branch <key>`. | `{"removed": Comment, "remaining": N}` |
| `clear (--scope worktree\|range\|all \| --all)` | Removes every comment in the given scope from the current branch. A scope (or `--all`) is **required** — a bare `clear` errors rather than wiping everything implicitly; passing both `--scope` and `--all` is also an error. | `{"cleared": N}` |
| `gc` | Drops inboxes for branches whose ref is gone and detached (commit-hex) keys whose commit no longer resolves — both scopes together, since a dropped branch key takes its whole entry. The current branch's own inbox is never dropped, even if `branch_names()` can't see it yet (a brand-new unborn branch). | `{"removed_branches": ["<key>", ...], "removed_comments": N}` |

**`Comment` JSON shape** (identical in `list` and `add`/`rm`'s embedded copy).
`scope` is additive and flat: a worktree comment carries no `range` key, a
range comment does (pinned, so existing parsers keep working — no field was
renamed or removed for this).

```json
{
  "id": 1,
  "scope": "worktree",
  "source": "human",
  "file": "src/app.rs",
  "side": "new",
  "line": 42,
  "text": "double-check this branch",
  "context": "    let x = compute();",
  "orphaned": false,
  "base": "a1b2c3d4e5f6…",
  "stale": false,
  "created_at": 1784430230
}
```

A range comment instead carries `"scope": "range", "range": "main"`; `base`
is **omitted** entirely (not `null`) since it's meaningless outside worktree
scope, while `stale` is still present (always `false` for a range comment):

```json
{ "id": 2, "scope": "range", "range": "main", "source": "agent", "file": "src/app.rs",
  "side": "old", "line": 10, "text": "…", "context": null, "orphaned": false,
  "stale": false, "created_at": 1784430300 }
```

| Field | Type | Notes |
|---|---|---|
| `id` | integer | Store-global, unique, crosses scopes. |
| `scope` | `"worktree"` \| `"range"` | Which surface the comment anchors to — see [Scopes](#scopes). |
| `range` | string | Present only when `scope` is `"range"`: the reviewed range spec (e.g. `"main"`). |
| `source` | `"human"` \| `"agent"` | Notes left in the TUI are `human`; everything `strix comment add` creates is `agent`. |
| `file` | string | The file's new-side path. A range comment orphans if the file is renamed; a worktree comment follows a *staged* rename via `orig_path`, and a committed rename marks it `stale`. |
| `side` | `"old"` \| `"new"` | Which side of the diff `line` refers to. |
| `line` | integer | 1-based; the last-known line even while `orphaned`. |
| `text` | string | The comment body, raw (may contain newlines). |
| `context` | string \| `null` | The anchored line's text captured at authoring time (re-anchoring never rewrites it); `null` means "unavailable" and always orphans on any drift instead of guessing. |
| `orphaned` | boolean | `true` when the anchor could no longer be matched on the last re-anchor/sweep pass. |
| `base` | string | Worktree comments only: the `HEAD` commit hex recorded when the note was written. **Omitted from the JSON entirely** (not `null`) for a range comment. |
| `stale` | boolean | Always present; meaningful for worktree comments only — `true` if the anchored line drifted while `HEAD` stayed the same (see [Usage](../getting-started/usage.md#on-uncommitted-work)). Always `false` for a range comment. |
| `created_at` | integer | Unix epoch seconds. |

### `add`'s context and orphan honesty

For `--scope worktree` (the default), `context`/`orphaned` reflect whether
`file`/`line` resolve in the net `HEAD`-vs-worktree diff at the moment of the
call: found → the line's text, `orphaned: false`; not found (gone, binary,
not part of the current change) → `context: null`, `orphaned: true`.

For `--scope range`, the result depends on whether the target range (`--range`,
or the branch's stored active range) resolves and whether the anchor is found
in it:

| Anchor resolves in the range | `context` | `orphaned` |
|---|---|---|
| file/line found | the line's text | `false` |
| file/line not found (gone, binary, out of range) | `null` | `true` — honest, not guessed |

The re-anchor/sweep pass also runs, best-effort, on `list` and `add` against
the branch's *existing* comments in the selected scope(s) before doing
anything else: it re-reads the store fresh and mutates that same read in
place (so a concurrent writer's change in between is never silently
discarded), then persists only if something actually changed — this is what
makes an agent's `rm` visible to a TUI session that re-reads on every
refresh, and vice versa. The worktree pass reuses the exact lifecycle engine
the live TUI runs (`worktree_facts`/`sweep_worktree`; see
[Architecture](architecture.md#comments-store)) — commit the anchored change
and it sweeps, edit it in place under an unchanged `HEAD` and it goes
`stale` rather than vanishing. A stored range that no longer resolves (e.g.
the base branch was deleted) serves the previously-persisted state and warns
on stderr rather than failing the action; a persist failure after a
successful re-anchor is the same — warn, don't fail the read.

### Concurrent writers

Every mutation is a fresh read-modify-write against the file on disk (so a
concurrent agent `rm` between two of your reads is picked up before you
overwrite), but the write itself is still last-writer-wins on the whole
file — there's no cross-process lock. In practice this means two rare races:
a delete can be resurrected by a concurrent writer that read the file before
the delete landed, and two concurrent `add`s that both read before either
writes can mint the same id and then lose one of the two writes entirely.
This is an accepted v1 tradeoff — a
per-store lock's stale-lock-on-crash failure mode was judged worse than a
millisecond race for a two-party (one human, one agent) workflow. The store
itself stays parseable and self-consistent either way.

## `strix skill`

```text
strix skill path [--json]
```

Manages the bundled `strix-review` agent skill — the document that teaches an
agent the review-comment loop (see the
[review loop guide](../guides/review-loop.md)). One action so far: `path`.

### `path`

Writes the skill embedded in this binary to

```text
<data_dir>/strix/skills/strix-review/SKILL.md
```

and prints that absolute path. The file is rewritten (atomically) on **every
invocation**, so the on-disk copy always matches the binary that printed the
path — after upgrading strix, the next `strix skill path` refreshes any stale
copy.

`data_dir` resolves as:

| Condition | `data_dir` |
|---|---|
| `$STRIX_DATA_DIR` set to a non-empty value | that value; a relative path is resolved against the current directory, so the printed path is always absolute |
| otherwise | the platform data directory — `~/Library/Application Support` on macOS, `$XDG_DATA_HOME` (default `~/.local/share`) on Linux |

An empty `$STRIX_DATA_DIR` counts as unset. Unlike the other subcommands,
`strix skill` takes no `[PATH]` and never touches a repository — it works
from any directory, inside a checkout or not.

With `--json`, stdout is `{"path": "…"}` instead of the bare path.

## Global options

Available on both the root command and `strix diff`.

| Option           | Description                                                       |
|------------------|-------------------------------------------------------------------|
| `--theme <NAME>` | Theme to use for this run (overrides the config file; never persisted). |
| `--dump-frame`   | Render one frame to stdout as text, then exit (debugging aid).    |
| `--width <N>`    | Terminal width for `--dump-frame` (default 120).                  |
| `--height <N>`   | Terminal height for `--dump-frame` (default 40).                  |
| `--version`      | Print the version and exit.                                       |
| `--help`         | Print help and exit.                                               |

## Environment

| Variable    | Description                                                          |
|-------------|----------------------------------------------------------------------|
| `STRIX_LOG` | Log verbosity, same syntax as `RUST_LOG` (e.g. `info`, `debug`, `strix=trace`). Logs are written to a file, never to the terminal. |
| `STRIX_DATA_DIR` | Overrides the data directory `strix skill path` materializes into (empty counts as unset; relative resolves against the current directory). |

## Logs

| Platform | Location                               |
|----------|----------------------------------------|
| macOS    | `~/Library/Logs/strix/strix.log`       |
| Linux    | `$XDG_STATE_HOME/strix/strix.log` (default `~/.local/state/strix/strix.log`) |
