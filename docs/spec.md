# strix

A focused, polished TUI for staging changes and viewing diffs without leaving the terminal. Named after *Strix*, the genus of owls known for sharp vision and quiet observation. Built to bridge the gap between heavy GUI tools (Cursor, Zed) and existing terminal options (LazyGit, tig) that feel cluttered or mouse-unfriendly.

## Problem

Current workflow involves bouncing between Claude Code in the terminal and a GUI editor (Cursor or Zed) just to view diffs properly. Existing terminal diff tools have rough edges:

- **LazyGit**: Powerful but visually busy, weak mouse support, steep learning curve
- **tig**: Functional but dated UI, limited theming, no proper side-by-side view
- **git diff** (CLI): No interactivity, no syntax highlighting by default, painful for reviewing larger changesets

The goal is a clean, fast, visually polished TUI focused on the two operations done most often: **staging changes** and **viewing diffs**.

## MVP Scope

### Core layout

A two-pane layout inspired by Cursor's source control view:

- **Left pane (staging panel)**
  - Top section: staged files
  - Bottom section: unstaged + untracked files
  - Move files between sections (stage / unstage)
  - Reset individual files back to HEAD
- **Right pane (diff viewer)**
  - Toggle between **unified** and **side-by-side** diff modes
  - Syntax highlighting for the language of the file being viewed
  - Smooth scrolling, with synced scroll across both panels in side-by-side mode

### Required features

- **Syntax highlighting** on diffs (matching Zed/Cursor quality bar)
- **Theme support**: light, dark, and popular presets (Catppuccin, Tokyo Night, Gruvbox, etc.)
- **Mouse support**: click to select files, click to toggle stage/unstage, scroll diffs
- **Keyboard support** with **customizable keybindings**
- **File reset**: discard changes to a file (with confirmation)
- **Smooth scrolling** through file lists and diff content
- **Quick file navigation**: jump between changed files via keyboard

### Out of scope for MVP

- Commit creation (use `git commit` directly for now)
- Branch management
- Merge conflict resolution
- Stashing
- Remote operations (push, pull, fetch)
- Hunk-level or line-level staging (file-level only for MVP)

## Future Phases

### Phase 2: History view — *shipped*

A separate view (toggleable, not crammed into the main layout) for browsing
commit history. Press `i` (or `2`) to enter; `Esc` or `1` returns to staging.

- Commit log of the current branch (HEAD ancestry, including merges), drawn
  with a colored Unicode branch/merge rail graph
- Drill into a commit to see its changed files; commit row shows full details
- Reuses the same diff viewer for commit-vs-first-parent file diffs

Possible follow-ups: multi-tip walk (all branches), rename tracking on the
file list, and incremental graph layout on load-more.

### Phase 3: File browser

Browse the working tree, not just changed files. Useful for opening files for context while reviewing a diff.

### Phase 4: Performance work

Target large repos (10M+ LOC, monorepos). Background indexing, incremental diff computation, smarter caching.

## Performance Targets

- **Initial scope**: small to mid-size repos (up to ~2M LOC, mature codebases with deep history)
- **Startup time**: < 100ms on typical repos
- **Diff render**: < 50ms for typical file diffs
- **No frame drops** during scroll or pane switching
- **Memory**: stay reasonable on the order of tens of MB, not hundreds

## Tech Stack

### Language: Rust

Chosen for performance, single-binary distribution, strong TUI ecosystem, and the maturity of git libraries available. Go was a strong second choice but Rust's ecosystem fits this problem better.

### Core dependencies

- **[ratatui](https://github.com/ratatui/ratatui)**: TUI framework. Successor to tui-rs, actively maintained, rich widget set, good mouse support.
- **[gitoxide / gix](https://github.com/GitoxideLabs/gitoxide)**: Pure-Rust git implementation. Fast, no shelling out to the `git` CLI, gives direct read access to the object database.
- **[syntect](https://github.com/trishume/syntect)** or **[tree-sitter](https://github.com/tree-sitter/tree-sitter)**: Syntax highlighting. Syntect uses Sublime Text grammars (broad coverage), tree-sitter is more accurate but requires per-language grammars. Evaluate both during prototyping.
- **[crossterm](https://github.com/crossterm-rs/crossterm)**: Cross-platform terminal backend (already used by ratatui). Handles mouse events, key events, terminal sizing.

### Config & theming

- **[serde](https://serde.rs/)** + **toml**: User config file (keybinds, theme selection, default diff mode)
- Themes shipped as TOML or JSON, with the ability to import existing color schemes (e.g., Base16, Catppuccin palette files)

### Project structure

```
prism/
├── src/
│   ├── main.rs              # Entry point, event loop
│   ├── app.rs               # App state, global state machine
│   ├── git/
│   │   ├── mod.rs           # gitoxide wrapper
│   │   ├── status.rs        # Status (staged/unstaged/untracked)
│   │   ├── diff.rs          # Diff computation
│   │   └── ops.rs           # Stage, unstage, reset
│   ├── ui/
│   │   ├── mod.rs           # Top-level render
│   │   ├── staging.rs       # Left panel
│   │   ├── diff_view.rs     # Right panel (unified + side-by-side)
│   │   ├── syntax.rs        # Syntax highlighting integration
│   │   └── theme.rs         # Theme loading + application
│   ├── input/
│   │   ├── keybinds.rs      # Keybinding config + dispatch
│   │   └── mouse.rs         # Mouse event handling
│   └── config.rs            # Config file loading
├── themes/                  # Bundled themes
├── Cargo.toml
└── README.md
```

## Architecture Notes

### State management

A single `App` struct holds global state: current repo, staging state, selected file, current view mode (unified vs side-by-side), active theme. Event loop pattern:

1. Read input (keyboard or mouse event)
2. Dispatch to handler based on focused pane
3. Update state
4. Re-render

### Git integration

Use `gitoxide` to:

- Walk the index for staged file list
- Walk the working tree + diff against index for unstaged changes
- Compute file diffs (blob vs blob, blob vs working tree)
- Apply staging operations (write to index)
- Reset working tree files to HEAD content

Fall back to shelling out to `git` CLI only for operations gitoxide doesn't yet support. Document any such fallbacks clearly.

### Rendering performance

- Cache rendered diff output per file until file or diff mode changes
- Lazy-load syntax highlighting (only highlight visible portion + small buffer)
- Use ratatui's built-in scrolling primitives where possible

## Design Mockup

Before implementation, build a React mockup of the UI to nail down:

- Pane proportions and spacing
- Color usage for staged vs unstaged vs untracked
- Diff line styling (additions, deletions, context, hunk headers)
- Header / footer / status bar content
- Side-by-side gutter design

Screenshots of the mockup should be included in this doc or a sibling `design.md` for reference during ratatui implementation.

## Existing Tools (Market Scan)

For context, here's what already exists in this space:

| Tool | Strengths | Weaknesses |
|------|-----------|------------|
| **LazyGit** | Powerful, mature, popular | Cluttered UI, weak mouse, steep learning curve |
| **tig** | Stable, lightweight, fast | Dated look, no proper side-by-side, limited theming |
| **gitui** | Rust-based, fast, modern | Less polished than target, limited mouse |
| **delta** (CLI) | Beautiful diff rendering, syntax highlighting | Not a TUI, no staging workflow |
| **GitUI in Helix/Zed/Cursor** | Best-in-class diff visualization | Requires leaving the terminal |

The gap this tool fills: **polished, opinionated TUI focused narrowly on staging + diff viewing**, with proper mouse support and visual quality matching modern editors.

## Feasibility

### Risks

- **Syntax highlighting performance**: tree-sitter integration in a TUI context is non-trivial. Mitigation: start with syntect, evaluate tree-sitter in a later iteration.
- **gitoxide maturity**: while production-ready for many operations, some areas (e.g., complex merge scenarios) are still in development. MVP scope is simple enough to avoid most rough edges.
- **Side-by-side diff layout**: making two scrollable panes stay in sync with proper alignment for added/removed lines is the trickiest UI problem. Worth prototyping early.
- **Terminal mouse limitations**: not all terminals report mouse events identically. Test on Ghostty (primary), iTerm2, Alacritty, WezTerm, and standard terminals.

### Estimated effort (rough)

- **Phase 1 MVP**: 3-6 weeks of evening/weekend work
  - Week 1: Project scaffolding, gitoxide integration, basic status display
  - Week 2: Staging operations, file selection, basic unified diff
  - Week 3: Side-by-side diff, syntax highlighting integration
  - Week 4: Theme system, mouse support, keybinding config
  - Week 5-6: Polish, edge cases, testing across terminals

## Implementation Roadmap

1. **Scaffold project** with cargo, ratatui hello-world, crossterm event loop
2. **Wire up gitoxide** to read repo status (staged/unstaged/untracked file lists)
3. **Build staging panel** with selection, two sections, basic keyboard navigation
4. **Implement stage/unstage/reset** operations via gitoxide
5. **Add unified diff view** with basic line coloring (no syntax highlighting yet)
6. **Integrate syntect** for syntax-highlighted diffs
7. **Add side-by-side mode** with synced scrolling
8. **Mouse support** for selection and stage toggling
9. **Theme system** with TOML-based theme files
10. **Keybinding config** via TOML
11. **Polish pass**: scrolling smoothness, status bar, help overlay
12. **Cross-terminal testing** and bug fixes

## Open Questions

- Single binary distribution: how to handle theme files? Bundle a few defaults, allow user themes via `~/.config/strix/themes/`?
- Should the tool support being launched against a specific repo path, or always use CWD?
- How to handle very large diffs (e.g., a single file with 10k+ line changes)? Pagination? Truncation with a "show all" toggle?

## Success Criteria

The tool succeeds if:

1. It replaces the current workflow of jumping to Cursor/Zed just to view diffs
2. Staging operations feel faster than the equivalent in LazyGit or a GUI
3. Diff rendering quality is good enough that there's no urge to open another tool to "really see" a change
4. It's enjoyable enough to use that it becomes the default git review tool
