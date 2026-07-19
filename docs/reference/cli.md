# CLI

```
strix [OPTIONS] [PATH]
strix diff <RANGE> [PATH]
strix comment [PATH] <list|add|rm|clear|gc>
```

The root form opens the staging view. `strix diff <RANGE>` opens a read-only
review session comparing two commits instead — see
[Reviewing a branch](../getting-started/usage.md#reviewing-a-branch).
`strix comment` reads and edits the review-comments inbox for the checked-out
branch without opening the TUI at all — the agent-facing surface for the
comments a human leaves in a review session — see
[`strix comment`](#strix-comment) below.

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
strix comment [PATH] list [--json]
strix comment [PATH] add --file <FILE> (--old-line N | --new-line N) --text <TEXT> [--json]
strix comment [PATH] rm <ID> [--json]
strix comment [PATH] clear [--json]
strix comment [PATH] gc [--json]
```

Reads and edits the review-comments inbox — the notes a human leaves on a
`strix diff` session in the TUI (see
[Leaving review comments](../getting-started/usage.md#leaving-review-comments))
— without opening the TUI. This is the agent-facing half of the loop: an
agent runs `list --json` to see what a human flagged, fixes each item,
commits, then `rm`s the comment to signal completion.

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
| `list` | Re-anchors the branch's comments against its stored range first (best-effort — see below), then lists them. A branch with no review session yet returns an empty inbox, exit 0. | `{"branch": "<key>", "range": "<string>"\|null, "comments": [Comment, ...]}` |
| `add --file <FILE> (--old-line N \| --new-line N) --text <TEXT>` | Adds a comment, always `source: "agent"` — there is no flag to author a human note; the CLI can't. Exactly one of `--old-line`/`--new-line` (both 1-based, ≥ 1); `--text` must be non-empty after trimming (its raw bytes, including newlines, are stored verbatim). Valid before any review session has run (the stored range is then `null`). | `{"comment": Comment}` (plain: prints the new id) |
| `rm <ID>` | Removes one comment by id from the current branch. An unknown id fails: `comment <ID> not found on branch <key>`. | `{"removed": Comment, "remaining": N}` |
| `clear` | Removes every comment on the current branch. | `{"cleared": N}` |
| `gc` | Drops inboxes for branches whose ref is gone and detached (commit-hex) keys whose commit no longer resolves; the current branch's own inbox is never dropped, even if `branch_names()` can't see it yet (a brand-new unborn branch). | `{"removed_branches": ["<key>", ...], "removed_comments": N}` |

**`Comment` JSON shape** (identical in `list` and `add`/`rm`'s embedded copy):

```json
{
  "id": 1,
  "source": "human",
  "file": "src/app.rs",
  "side": "new",
  "line": 42,
  "text": "double-check this branch",
  "context": "    let x = compute();",
  "orphaned": false,
  "created_at": 1784430230
}
```

| Field | Type | Notes |
|---|---|---|
| `id` | integer | Store-global, unique. |
| `source` | `"human"` \| `"agent"` | Notes left in the TUI are `human`; everything `strix comment add` creates is `agent`. |
| `file` | string | The file's new-side path (`CommitFile::path`); a rename orphans the comment (no rename-following). |
| `side` | `"old"` \| `"new"` | Which side of the diff `line` refers to. |
| `line` | integer | 1-based; the last-known line even while `orphaned`. |
| `text` | string | The comment body, raw (may contain newlines). |
| `context` | string \| `null` | The anchored line's text captured at authoring time (re-anchoring never rewrites it); `null` means "unavailable" and always orphans on any drift instead of guessing. |
| `orphaned` | boolean | `true` when the anchor could no longer be matched on the last re-anchor pass. |
| `created_at` | integer | Unix epoch seconds. |

### `add`'s context and orphan honesty

`add`'s `context`/`orphaned` result depends on whether the branch has a
stored range (i.e. a `strix diff` session has run at least once) and whether
the anchor resolves against it:

| Stored range | Anchor resolves | `context` | `orphaned` |
|---|---|---|---|
| none yet | — | `null` | `false` — unknown isn't the same as orphaned |
| resolves | file/line found | the line's text | `false` |
| resolves | file/line not found (gone, binary, out of range) | `null` | `true` — honest, not guessed |

The re-anchor pass also runs, best-effort, on `list` and `add` against the
branch's *existing* comments before doing anything else: it re-reads the
store fresh, re-anchors in place per the same algorithm the TUI uses (exact
line+text match, else a same-side content match within 10 lines of the
stored line, else orphan), and persists only if something actually changed —
this is what makes an agent's `rm` visible to a TUI session that re-reads on
every `refresh_review`, and vice versa. A stored range that no longer
resolves (e.g. the base branch was deleted) serves the previously-persisted
state and warns on stderr rather than failing the action; a persist failure
after a successful re-anchor is the same — warn, don't fail the read.

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

## Logs

| Platform | Location                               |
|----------|----------------------------------------|
| macOS    | `~/Library/Logs/strix/strix.log`       |
| Linux    | `$XDG_STATE_HOME/strix/strix.log` (default `~/.local/state/strix/strix.log`) |
