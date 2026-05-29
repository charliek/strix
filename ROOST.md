# Using roost to debug strix (notes + feedback)

You asked me to try driving/inspecting strix with the [roost](https://github.com/charliek/roost)
app and to jot down what I found plus any feedback to make roost better. Here it
is — written from the overnight build of strix.

## TL;DR

- The current Rust CLI, **`roostctl`** (`crates/roost-cli`), is a genuinely great
  **headless TUI test harness**: `tab open` → `tab send` → `wait` → `tab dump` /
  `screenshot`. It's exactly the loop I'd want to verify a TUI like strix in a
  *real* terminal (libghostty-vt), with real colours and real key/mouse encoding.
- I built it in ~2s (`cargo build -p roost-cli` — pure Rust, no GTK/ghostty) and
  mapped the command surface, but I **couldn't do a live run tonight**: no Roost
  UI was running (only a stale `roost.sock`), and I wasn't going to launch a GUI
  window while you were asleep.
- A couple of rough edges (stale root binaries pointing at the wrong socket;
  discoverability of `roostctl`) are written up under *Feedback* below.

## How I verified strix's TUI without roost

Because I was headless, strix grew its own terminal-free verification loop, which
roost would *complement* rather than replace:

- **`strix --dump-frame [--width W --height H]`** renders one frame to stdout as
  text via ratatui's `TestBackend`. I used this constantly to "see" the layout,
  the diff gutters, the side-by-side divider, the help overlay, and theme runs —
  no terminal required.
- **`tests/*_test.rs`** assert on that text grid (`dump_frame`) and on the git
  layer against temp repos.

What this *can't* see — and where roost shines — is real truecolor rendering, the
libghostty-vt cell grid, alt-screen enter/leave, and real mouse/key encoding.

## The workflow I'd use (once a Roost UI is running)

`roostctl` auto-detects the running UI's socket. The loop to drive strix:

```bash
RC=../roost/target/debug/roostctl      # or build: cargo build -p roost-cli

PID=$($RC project list --json | jq -r '.[0].id')        # or: project create
TAB=$($RC tab open --project-id "$PID" --cwd "$PWD" --cols 120 --rows 40)

$RC tab send --tab "$TAB" --bytes 'strix\n'             # run strix in the tab's shell
$RC wait --tab "$TAB" --text 'Changes'                  # block until strix paints

$RC tab dump --tab "$TAB"                               # viewport as text (content assertions)
$RC screenshot --out /tmp/strix.png                     # PNG WITH COLOURS (theme/syntax check)

$RC tab send --tab "$TAB" --bytes 'jjd'                 # navigate + toggle side-by-side
$RC tab dump --tab "$TAB"                               # re-check
$RC tab send --tab "$TAB" --bytes '\x1b[<0;40;5M\x1b[<0;40;5m'  # an SGR mouse click
$RC tab send --tab "$TAB" --bytes 'q'                   # quit
$RC tab close --tab "$TAB"
```

`tab dump` is essentially my `--dump-frame`, but produced by a real VT — so it
cross-checks that strix's escape output (styles, alt-screen, cursor) actually
renders the way TestBackend predicts. `screenshot` is the piece TestBackend can
never give: did the Tokyo Night / Catppuccin / Gruvbox palette and the syntect
token colours actually land on screen.

### strix-specific things worth checking in roost when it's up

- Truecolor for all five themes (`strix --theme catppuccin`, etc.) — strix uses
  24-bit `Color::Rgb`; confirm libghostty-vt renders them, and that they degrade
  sanely on a 256-colour profile.
- Syntax-highlighted add/delete lines: token colours composited over the
  green/red backgrounds.
- Side-by-side mode (`d`): the `│` divider alignment and per-column line numbers
  at a real terminal width.
- Mouse: click-to-select, marker-click-to-stage, wheel-scroll — strix enables SGR
  mouse capture, so `tab send` of `\x1b[<…M/m` sequences exercises the real path.
- The panic hook: strix restores the terminal (leaves alt-screen, disables raw
  mode + mouse) on panic — worth confirming a forced panic doesn't wedge the tab.

## Feedback (to make roost better)

1. **The root `./roost` and `./roost-cli` binaries are stale and misleading.**
   `./roost-cli` (the committed Go build) dials
   `~/Library/Application Support/Roost/roost.sock`, but the current Rust/Swift
   Roost listens at `~/Library/Caches/Roost/roost.sock` — so it can never connect,
   and its `--help` lacks `tab open/send/dump`, `screenshot`, `wait`. I went down a
   blind alley with it before finding that the real CLI is `crates/roost-cli` →
   `roostctl`. Since these root binaries are slated for removal anyway
   (`plans/GODELETE.md`), deleting them (or `.gitignore`-ing them like the build
   output) would stop them shadowing `roostctl`.

2. **Make "build just the CLI" obvious.** `cargo build -p roost-cli` produces a
   working `roostctl` in ~2s with **no** GTK/ghostty toolchain — perfect for
   scripts/agents that only need to talk to a running UI. The README's build
   section leads with the full UI build; a one-liner "just the CLI: `cargo build
   -p roost-cli`" would save that discovery.

3. **`tab dump` deserves top billing as a TUI-testing primitive.** It's the
   single most useful command for verifying *any* TUI headlessly. A one-shot
   convenience — e.g. `roostctl run --cwd DIR --cmd "strix" --wait-text Changes
   --dump` that opens a tab, runs the command, waits, dumps, and closes — would
   turn "snapshot a TUI" into a single call. (Today it's ~5 commands + capturing
   the tab id.)

4. **A `tab click` helper would help mouse-heavy TUIs.** strix leans on the mouse
   (click to select/stage, wheel to scroll). I *can* hand-roll SGR sequences
   through `tab send --bytes '\x1b[<0;X;YM…'`, but a `roostctl tab click --x --y
   [--button] [--scroll up|down]` that emits the right encoding would make mouse
   paths first-class in tests.

5. **Stale-socket UX is already good** — `identify` probes the connection and says
   "no Roost UI is running" rather than hanging on a dead `roost.sock`. Minor:
   cleaning up an orphaned socket file on next launch (if it doesn't already)
   would keep `ls` tidy.

## What I actually ran tonight

```text
$ cargo build -p roost-cli          # 1.76s, pure Rust
$ ./target/debug/roostctl --help    # mapped the command surface
$ ./target/debug/roostctl identify  # -> "no Roost UI is running" (stale socket)
```

Net: roost's `roostctl` is the right tool to give strix a real-terminal test
pass; I've left the exact recipe above so it's a copy-paste away once a Roost
window is open.
