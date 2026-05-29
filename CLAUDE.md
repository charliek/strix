# strix — Project Conventions

## Direction (read first)

strix is a focused, polished terminal UI for the two git operations done most
often: **staging changes** and **viewing diffs**. It is *not* a full git client
— no commit creation, branch management, merge-conflict resolution, stashing, or
remote operations in the MVP (see `docs/spec.md` for the full scope and the
explicit non-goals).

**North star.** Visual quality matching a modern editor's diff view (Zed/Cursor),
with first-class mouse *and* keyboard support, fast enough to feel instant on
mid-size repos. When a choice is ambiguous, optimize for *polish and the review
experience* over feature breadth.

## Branch policy

`main` is the primary branch. Work proceeds in milestone branches
(`milestone/<n>-<slug>`). Each milestone is reviewed with `/simplify` before it
merges. **We do not open PRs**: merge the milestone branch into `main` locally
and push `main`. CI (`.github/workflows/ci.yml`) runs on the push and is
informational. Docs deploy to GitHub Pages on pushes that touch `docs/`.

## Architecture

A single binary crate, split into a library (`src/lib.rs`) and a thin binary
(`src/main.rs`) so all logic is testable from `tests/*_test.rs` and from the
`--dump-frame` path without a real terminal.

- `app.rs` — the `App` struct holds all state (repo, file lists, selection,
  focus, diff mode, theme). The event loop reads an input event, calls
  `App::on_key` / `App::on_mouse`, then redraws from the updated state. State is
  the single source of truth; rendering is a pure function of it.
- `terminal.rs` — terminal setup/teardown (raw mode, alternate screen, mouse
  capture), the event loop, a panic hook that restores the terminal, and
  `dump_frame` (renders one frame to text via ratatui's `TestBackend`).
- `ui/` — rendering only, never mutates state. `mod.rs` lays out header / body /
  footer; `staging.rs` and `diff_view.rs` render the two panes; `theme.rs` is the
  colour palette every widget reads from (no hard-coded colours elsewhere).
- `git/` — repository access (see below).
- `config.rs`, `input/` — config + keybinding/mouse dispatch (later milestones).

## Git integration

- **Reads** (status, refs, blob contents) use **gix** (gitoxide) — pure-Rust, no
  subprocess on the hot path. This is the spec's primary choice.
- **Diffs** are computed in-process with the **`similar`** crate over the HEAD /
  index / worktree blob bytes, producing a structured model that drives both
  unified and side-by-side rendering.
- **Mutations** (stage / unstage / reset) **shell out to `git`** (`git add`,
  `git restore --staged`, `git restore` / `git checkout`). This is the
  CLI fallback the spec explicitly sanctions (`docs/spec.md`): it is rock-solid
  and avoids gix's less-mature index-write porcelain. Keep these shell-outs
  confined to `git/ops.rs` and documented there; a future iteration can move them
  to pure gix once that porcelain matures.

## Library preferences

Prefer pure-Rust crates. Current set: `ratatui` + `crossterm` (TUI), `gix`
(git reads), `similar` (diff), `syntect` with `default-fancy` (pure-Rust regex
syntax highlighting), `serde` + `toml` (config/themes), `directories` (paths),
`clap` (CLI), `anyhow`/`thiserror` (errors), `tracing` (+ file appender) logging.
Adding a dependency: name the constraint, prefer pure-Rust, keep wrappers small.

## Toolchain

Rust **1.96.0**, pinned in `rust-toolchain.toml` and `.mise.toml`. `.cargo/config.toml`
enables the MSRV-aware resolver (`incompatible-rust-versions = "fallback"`).

> Note: the project initially pinned 1.85.0 (mirroring roost) but the mid-2026
> ecosystem — gix 0.84, etc. — needs a newer compiler, so strix tracks current
> stable. `mise install` / rustup will fetch 1.96.0 automatically.

## Style

- Flat module layout, concrete types until duplication forces an interface. No
  `Manager`, `Coordinator`, `Service`, `Helper` — name things for what they are.
- Errors are returned, not logged-and-swallowed. Log at the boundary that handles
  them.
- Tests live in `tests/*_test.rs`. Two kinds: `TestBackend` render assertions
  (build an `App`, `dump_frame`, assert on the text grid) and temp-repo
  integration tests for the git layer (`git init` a `tempfile::tempdir`, exercise
  the ops, assert).
- Default to no comments. Add one only when the *why* is non-obvious — a hidden
  constraint, a workaround, a tricky invariant. Don't restate what the code says.
- No `// TODO` in committed code. Do it, file it, or leave a `// XXX:` for a known
  dead-end.

## Troubleshooting

- **Logs**: `~/Library/Logs/strix/strix.log` (macOS), `$XDG_STATE_HOME/strix/strix.log`
  (Linux). Set `STRIX_LOG=debug` (same syntax as `RUST_LOG`) to raise verbosity.
  The TUI can't log to stdout, so the file appender is the way to see what
  happened.
- **See a frame without a terminal**: `cargo run -- --dump-frame [--width W --height H]`
  renders one frame against the current repo and prints it as text. This is the
  primary way to inspect the UI in tests and headless runs.
- **Build**: `cargo build`; `make check` runs fmt + clippy + tests. Docs:
  `make docs-serve` (needs `uv`).
