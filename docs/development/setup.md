# Development setup

## Toolchain

strix pins Rust **1.96.0** in `rust-toolchain.toml` (and `.mise.toml`). With
`mise` or `rustup` the right toolchain installs on demand:

```bash
mise install        # or: rustup will auto-install from rust-toolchain.toml
```

`.cargo/config.toml` turns on the MSRV-aware resolver so cargo prefers
dependency versions compatible with the pinned toolchain.

## Common tasks

`make help` lists everything. The essentials:

| Command           | Description                                  |
|-------------------|----------------------------------------------|
| `make build`      | Debug build                                  |
| `make run`        | Run against the current directory            |
| `make check`      | `fmt --check` + `clippy -D warnings` + tests |
| `make test`       | Run the test suite                           |
| `make dump`       | Render one frame to stdout as text           |
| `make docs-serve` | Serve the docs locally (needs `uv`)          |

## Inspecting the UI

The TUI takes over the screen, so the fastest feedback loop in tests and headless
runs is `--dump-frame`:

```bash
cargo run -- --dump-frame --width 120 --height 40
```

It builds the app against the current repo, renders one frame to an in-memory
backend, and prints the cell grid as text.

## Logs

`STRIX_LOG=debug strix` raises verbosity; output goes to the log file
(`~/Library/Logs/strix/strix.log` on macOS), never to the terminal.

## CI

`.github/workflows/ci.yml` runs fmt, clippy, tests, and a release build on every
push to `main`. `.github/workflows/docs.yml` builds the mkdocs site and deploys
it to GitHub Pages when `docs/` changes.
