# CLI

```
strix [OPTIONS] [PATH]
strix diff <RANGE> [PATH]
```

The root form opens the staging view. `strix diff <RANGE>` opens a read-only
review session comparing two commits instead — see
[Reviewing a branch](../getting-started/usage.md#reviewing-a-branch).

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

`diff` (and future subcommands on this track) take precedence over the root
`PATH` positional, so `strix diff` is always the review subcommand — it does
not open a directory literally named `diff`. To open such a directory, prefix
it with a path segment: `strix ./diff`.

### Exit behavior

An unresolvable `RANGE` (unknown revision, a non-commit operand, or no merge
base between the two sides) fails before the TUI opens: strix exits non-zero
and prints a message to stderr naming the offending operand and the kind of
failure. A missing `RANGE` on `strix diff` is a clap usage error.

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
