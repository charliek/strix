# Architecture

strix is a single binary crate, split into a library (`src/lib.rs`) and a thin
binary (`src/main.rs`). Keeping the logic in the library makes it testable from
`tests/*_test.rs` and from the `--dump-frame` path without driving a real
terminal.

## State and the event loop

A single `App` struct holds all state: the repository, the staged / unstaged /
untracked file lists, the current selection and focus, the diff mode, and the
active theme. The loop is deliberately simple:

1. Render the current state to the screen.
2. Block for an input event (key, mouse, resize).
3. Dispatch it to `App` — which updates state.
4. Repeat.

Rendering is a pure function of state (`ui::draw`); it never mutates `App`. That
separation is what lets `dump_frame` render any state to an in-memory buffer for
assertions.

**Keyboard-enhancement protocol.** `terminal.rs` probes
`supports_keyboard_enhancement()` once at startup, while still on the normal
screen and before mouse capture (the query can block briefly on a terminal
that ignores it, so the round-trip happens before the alt-screen would
otherwise sit frozen). On a capable terminal it pushes crossterm's
`DISAMBIGUATE_ESCAPE_CODES` flag, recording success in an `AtomicBool` only
after the push write itself succeeds — so `restore()` never pops a flag that
was never pushed. `restore()` attempts every teardown step (pop enhancement,
leave alt screen, disable raw mode, disable mouse capture) and surfaces the
first error rather than short-circuiting, so one failing step can't strand a
later one; the panic hook and setup-failure cleanup share the same restore
path. This is what makes Shift+Enter arrive as a distinct event instead of a
plain `Enter`, reaching
[the in-place editor's newline chord](#diff-pane-rows-cursor-and-comments) —
on an incapable terminal nothing is pushed, and the editor's Ctrl-J/Alt+Enter
fallbacks still work either way.

## Modules

| Module        | Responsibility                                                    |
|---------------|-------------------------------------------------------------------|
| `app`         | State + input dispatch (`on_key`, `on_mouse`); per-view dispatch. |
| `terminal`    | Setup/teardown, event loop, panic-safe restore, `dump_frame`.     |
| `ui`          | Rendering: layout, the panes, theme, syntax highlighting.         |
| `ui/review.rs`| Review view rendering: the flat changed-file list beside the shared diff pane. |
| `ui/menu.rs`  | Pure menu-bar geometry + rendering: label layout, the dropdown overlay (see [Menu bar](#menu-bar)). |
| `git`         | Repository access: status, history, diffs, mutations.             |
| `git/review.rs` | Branch-range review: `resolve_range` (gix `merge_base`), file listing via paired `git diff-tree` name-status/numstat runs, and lazy per-file diffs. |
| `graph`       | Pure rail-graph lane layout for the history view.                 |
| `config`      | Config read (`toml`) and write-back (`toml_edit`). |
| `keys`        | The configurable keymap: `Action`, default chords, `[keys]` overrides; dispatch itself lives in `App::on_key`/`App::on_mouse`. |
| `comments`    | Comment model (worktree + range scopes), JSON store I/O, and the pure re-anchor/sweep engines (see [comments store](#comments-store)). |
| `comments_cli`| `strix comment list\|add\|rm\|clear\|gc --scope …` — the agent-facing CLI over the same store, sharing the worktree sweep engine with the TUI. |
| `skill`       | `strix skill path` — materializes the bundled `strix-review` skill (see [skill distribution](#skill-distribution)). |

## Git layer

| Operation                     | Mechanism                                              |
|-------------------------------|--------------------------------------------------------|
| Status (staged/unstaged/untracked) | Shell out to `git status --porcelain=v2 --branch -z`, parsed in `git/status.rs`. gix *can* compute status, but its iterator API is far less ergonomic than the stable porcelain format for a read that runs once per refresh, not on the hot path. |
| Blob contents (HEAD / index / worktree) | gix object database + the working tree.        |
| Commit walk + refs (history view) | gix `rev_walk` from HEAD (full DAG), commit objects decoded only. |
| Per-commit changed files      | `git diff-tree -z --name-status` (parsed in `git/history.rs`).  |
| Branch-range resolution (review view) | gix `merge_base`, `rev_parse_single` + tag-peeling (`git/review.rs`). |
| Branch-range file listing (review view) | Two `git diff-tree -z` runs (`--name-status`, `--numstat`) joined by path — no in-process diffing while listing a range that can span hundreds of files. |
| Diff computation              | [`similar`](https://github.com/mitsuhiko/similar) over blob bytes, producing structured hunks; computed lazily, per selected file. |
| Stage / unstage / reset       | Shell out to `git` (`add`, `restore --staged`, `restore`).      |

Object reads (blobs, refs, merge-base) stay on the pure-Rust gix path for
speed; status and the range file listing shell out to `git` because the
porcelain formats they need are far more ergonomic than the equivalent gix
iterator APIs for a read that isn't on the diff hot path; the three mutations
shell out because it is reliable and sidesteps gix's less-mature index-write
porcelain. Those shell-outs are confined to `git/ops.rs` (mutations),
`git/status.rs`, and `git/review.rs`/`git/history.rs` (listings).

### Comments store

Comments (`src/comments.rs`) live in a single JSON file at
`<common_dir>/strix/comments.json` — `Repo::strix_dir()` resolves
`gix.common_dir()`, not `git_dir()`, so a linked worktree (whose `git_dir` is
private to that checkout) and the primary checkout share exactly one file,
keyed by branch. Every mutation (TUI or CLI) is a read-modify-write: load a
fresh copy of the store, apply the change, and write atomically via the same
sibling-temp-file-then-rename helper `config::write_atomic` uses for
`config.toml` (`config.rs`, promoted `pub(crate)` and generalized to take a
target filename rather than hardcoding `config.toml`). A store that fails to
parse, or whose `version` field is newer than this build understands, is
never written to — every mutation reads first, so the never-clobber guarantee
holds for both reads and writes. A missing or zero-byte file is a valid empty
store.

**Scope.** Every `Comment` carries a `Scope`: `WorkTree` (anchored to the net
`HEAD`-vs-worktree diff — see [Status view](#status-view-net-worktree-diff-and-comments)
below) or `Range { range }` (anchored to a committed `strix diff <range>`
review). `Scope` is serialized flat and additively — `"scope":"worktree"`, or
`"scope":"range","range":"main"` — so no pre-existing field was renamed for
it (see [CLI](cli.md#strix-comment)). A worktree comment additionally carries
`base` (the `HEAD` commit hex at authoring time; omitted, not `null`, for a
range comment) and always carries `stale` (`bool`, meaningful — set when the
line drifts under an unchanged `HEAD` — for worktree comments only, always
`false` otherwise); see the lifecycle below.

**Store versioning.** `STORE_VERSION` is 2 (bumped from milestone 6's 1 to add
`Scope`). `load` decodes a minimal `{version, next_id}` envelope first — a
full v2 decode requires the v2 `Comment` shape, so a real v1 file with
comments would otherwise fail that parse and wrongly hit the never-clobber
path — then routes on `version` alone: `2` parses normally; `1` copies the
raw bytes aside to `comments.json.v1.bak` (an identical existing backup is a
no-op; a different one is kept, and these bytes go to
`comments.json.v1.bak.1`, `.2`, …) and returns an **empty** v2 store,
carrying the old `next_id` forward so a freshly minted id can't collide with
one still referenced from the backup; this is a deliberate, one-time reset —
backwards compatibility with v1 was explicitly out of scope for the milestone
that introduced `Scope` (see the [review loop guide](../guides/review-loop.md)
for the user-facing note). Unparseable JSON still errors and preserves the
file (never-clobber kept); `version > 2` still refuses read and write.

**GC** runs at TUI startup (`App::build`, right after `Repo::open` and before
comments load — best-effort, never fatal) and via `strix comment gc`: it
drops inboxes keyed to a branch whose ref is gone, and detached (commit-hex)
inboxes whose commit no longer resolves, logging each drop — both scopes on a
dropped branch key go together, not just one.

**Re-anchoring** (`comments::reanchor`/`reanchor_scoped`, pure functions over
a comment slice and per-file diffs) runs on review-session open, on every
`refresh_review`/Status `refresh()` (a store re-read is the *first*
statement, ahead of the OID churn guard, so an agent's `rm` shows up even
when nothing else changed), and in the CLI's `list`/`add` paths. Re-anchoring
is **scope-filtered**: Status re-anchors only `Scope::WorkTree` comments
against the net worktree diff; Review re-anchors only `Scope::Range`
comments whose `range` matches the resolved review's *exact* range — a
comment is never re-anchored against, or orphaned by, the wrong view's diff.
For each comment: an exact line-number + line-text match wins outright;
failing that, a same-side line whose text matches the stored `context`
**within ±10 lines** of the stored line re-anchors to it (closest wins, ties
toward the smaller line); anything farther, or with no match at all, is
marked `orphaned` rather than silently relocated — a `context` of `None`
("unavailable" at authoring time) always orphans on any drift instead of
content-matching. A re-anchor pass that changes nothing skips its write,
which is what keeps a live TUI's watcher from looping (reload → re-anchor →
write → reload → …).

**Worktree comment lifecycle.** A worktree comment's `base` OID plus the
current `Status` drive a small state machine (`comments::sweep_worktree`,
fed per-comment `FileFacts` from `App::worktree_facts` — the same function
`comments_cli`'s headless sweep calls, so the TUI and CLI run one lifecycle
engine, not two): committing the exact change a comment anchors to (`HEAD`
moves past `base` and the comment is orphaned-after-re-anchor against the net
diff) sweeps it from the inbox; editing the anchored line in place while
`HEAD` is unchanged marks it `stale` (surfaced with a dimmed accent, never
auto-deleted); staging/unstaging with no content change, an unrelated edit,
an unrelated commit, or the line merely scrolling out of the rendered diff
never touch it; a comment's file vanishing from the worktree with `HEAD`
unchanged sweeps unconditionally. The sweep runs through `mutate_if_changed`,
so a pass that finds nothing to do never writes (and never re-triggers the
watcher).

The watcher (`watch.rs`) recursively watches the workdir plus
`Repo::watch_roots()` — the common dir, this checkout's private git dir, and
`strix_dir()`, each dropped if it's already covered by the recursive workdir
watch or nested under another root already kept. In a primary checkout every
candidate lives under `workdir/.git`, so the result is empty (the workdir
watch already covers it); in a linked worktree the private git dir and the
shared common dir both live outside `workdir` (its `.git` is a file pointing
elsewhere), so the result is the common dir alone — which subsumes both the
per-worktree git dir and the store — letting a commit or a `strix comment`
write from another linked worktree wake this session.

### Skill distribution

The `strix-review` agent skill lives in the repo at
`skills/strix-review/SKILL.md` — the single source of truth, from which
skills.sh and the Claude Code plugin manifests (`.claude-plugin/`) install
directly — and is *also* embedded into the binary at compile time
(`include_str!` in `src/skill.rs`). `strix skill path` materializes that
embedded copy on demand under the user's data directory and prints the
absolute path, overwriting on every invocation so the on-disk file can never
drift from the binary that ships it. This is what lets any agent, plugin
system or not, be pointed at a current copy of the skill with one command —
see [`strix skill`](cli.md#strix-skill).

## Diff model

A diff is a `FileDiff` of hunks; each hunk is a list of lines tagged
`Add` / `Delete` / `Context` with their old and new line numbers. This one model
drives both unified rendering (a single column) and side-by-side rendering (old
on the left, new on the right, with blank padding to keep the two columns
aligned). Syntax highlighting is layered on top: syntect tokenizes the file
content, and the token colours are composited with the add/delete backgrounds.

The history view feeds the *same* `FileDiff` model from a different source — a
commit blob and its first-parent blob, looked up by revspec — so the diff pane
serves both views unchanged. The right pane swaps between that diff and a
`git show`-style commit-details paragraph based on the selection.

**Side-by-side word emphasis.** A zipped SBS `Pair` with both sides present is
a candidate for word-level emphasis: a char/word-level `similar` diff over the
two sides' *sanitized* text (the same sanitize `highlighted_content` already
applies, so offsets align) locates the changed spans, computed once when the
row layout is built and cached with it, not per frame. Below a similarity
threshold the pair is treated as a plain add+del instead — dissimilar lines
never get emphasis painted over them, and a pure addition/deletion (no
opposite-side partner) never does either. The renderer intersects the changed
spans with `highlighted_content`'s syntax-token spans and paints the
intersection in `theme.add_emph`/`del_emph` — a brighter, more saturated pair
than the flat `add_bg`/`del_bg` wash — instead of the line's base background;
selecting the row still repaints every span with `selection_bg`, so the
cursor highlight intentionally overrides emphasis.

**Side-by-side filler shading.** A pure addition or deletion has no partner
line, so its opposite column renders empty; `cell()` paints that empty column
`theme.add_gutter` when the row is a pure Addition or `theme.del_gutter` when
it's a pure Deletion, instead of the flat pane background — a subtle tint
that reads as "one side changed" without competing with the word-emphasis
colours above. A context pair and a modified pair never carry a gutter tint
on either side; an empty cell opposite anything else (there is no such case
today, but the fallback exists) stays `theme.bg`.

## Diff pane: rows, cursor, and comments

The diff pane is shared by Status, Review, and History, but only Status and
Review have a cursor (History reuses the rendering, not the input model). The
cursor addresses a **logical** `RowTarget` — `Code(diff_index)`,
`Comment(id)`, or `Orphan(id)` — never a raw row index. A separate, **physical**
layout (`Vec<LayoutRow>`, each row carrying its `RowTarget`, a `subrow` index,
an optional side-by-side column, a click `HitRegion`, and its render content)
is rebuilt from that target list for the current pane width and diff/comments
generation; a code line is exactly one `LayoutRow`, a comment box or the
in-place editor is several (border / title / body) sharing one target.
Consequences of the split: `j`/`k` move between *targets*, crossing a
multi-row box in a single step; scroll offset and content-height metrics
count physical rows; the cursor highlight spans every physical row of the
selected target; and a resize rebuilds the layout while preserving the
logical target the cursor was on. `DiffPaneState` (the cursor, the open
in-place editor if any, and the comment boxes' `[x]` click rects recorded
during render) is owned per view that *has* a cursor — Status and Review each
get their own; the scroll offset, the height metrics, and the row-layout
cache itself stay App-global, shared with History.

**Comment boxes.** A comment renders as a bordered, multi-row box directly
below its anchored line: a title row (`● you — <file> R<line>` or
`● agent — …`, `⚠` in place of `●` for an orphan, a right-aligned `[x]`), the
word-wrapped body, and a bottom border — placed after the line in unified
mode, and in the anchor's own column (the other column rendering blank) in
side-by-side. A `stale` worktree comment (see
[Comments store](#comments-store)) renders its accent and body dimmed rather
than in the theme's `comment` colour.

**The in-place editor** (`CommentEdit`, replacing milestone 6's centered
`Modal::CommentInput`) lives in `DiffPaneState.editing`: an editable box
rendered at the anchor, its position recomputed from that anchor *every
frame* rather than a captured row index, so a watcher reload that rebuilds
the diff mid-typing can't dangle it. It expands the diff around it, and the
view scrolls after every keystroke to keep the caret visible as the box
grows. While editing, keys route to the editor *before* the keymap, so any
plain character — including `c`, `x`, `]` — inserts literally instead of
firing its usual action; `Enter` saves, `Esc` discards, and a newline is
`Shift+Enter`, with `Ctrl-J`/`Alt+Enter` as equal fallbacks for terminals that
don't report Shift+Enter reliably (see
[Keybindings](../getting-started/keybindings.md#in-place-comment-editor)).
Bracketed paste is enabled in the terminal setup so a multi-line paste
inserts real newlines rather than one `Enter` per line. Submitting re-reads
the store fresh and, for an edit, updates only the comment's `text` on that
freshly-read record (so a concurrent re-anchor's location is never
overwritten by the stale captured anchor); an unchanged-text save is elided
as a no-op, matching the store's general write-elision discipline.

**Mouse.** A render pass resolves a click position to a semantic `HitTarget`
(the view, the file, and a region — a code line, a comment box, or its
`[x]`); two `Down(Left)` events whose `HitTarget`s are *equal* within 500 ms
are a double-click (`is_double_click`, driven through an injectable clock,
`on_mouse_at`, so tests assert the timing boundary with explicit instants
instead of sleeping). A double-click on a code line opens the editor there,
same as `c`; on an existing comment box, it edits that comment (an
agent-authored box flashes read-only instead); a single click on a box's
`[x]` deletes it immediately, ahead of the double-click check. The tracker
resets on any drag, scroll, resize, view/mode change, or comment
mutation — including a `[x]` click consuming its own click — so a
triple-click or a click just after a delete can't false-fire a second
double-click.

## History view

The history view is a toggleable second view (`i` / `1` / `2`) alongside the
status view. `app::ViewMode` selects which is active and splits input dispatch
(`dispatch_status` / `dispatch_history`); the diff pane and its caches are
shared via an `active_diff()` indirection. The left column splits into the
selected commit's file list and a commit-graph log, separated by a draggable
horizontal divider that mirrors the existing vertical one.

The rail graph is a **pure** transform (`src/graph.rs`): an ordered commit list
plus refs in, one render row per commit out, assigning each commit a lane and
emitting Unicode rail glyphs (`● │ ╮ ╯ ╭ ╰ ─`) connecting it to its parents.
The walk is bounded (500 commits per page, decode-only) and tree/blob reads
happen lazily for the selected commit only.

## Review view

`strix diff <RANGE>` opens a third `app::ViewMode` (`Review`) alongside
`Status` and `History`, resolved once at startup (`git/review.rs`) so a bad
range fails before the terminal takes over. It reuses the shared diff pane and
its caches; the file list is a flat `ui/review.rs` widget (no
staged/unstaged sections, since a committed range has no such split). The view
is read-only for staging — those actions are a no-op — and the file watcher
refreshes it like the other views, guarded so a worktree-only change (no new
commit on either side of the range) skips re-listing.

Bare `strix diff` (`RANGE` omitted) never resolves a range at all — `cli.rs`
routes a `None` range straight to `App::with_config`, the same construction
path the root `strix` command uses, which opens `Status`. The two ordered
`Option` positionals (`range`, `path`) mean a single trailing argument always
binds to `range`, not `path` — there is no path-only `strix diff PATH` form
(see [CLI](cli.md#strix-diff-range-path)).

**Cursor and comments.** Review's diff-pane cursor and its comments (range
scope) use the shared `RowTarget`/`LayoutRow`/`DiffPaneState` model described
in [Diff pane: rows, cursor, and comments](#diff-pane-rows-cursor-and-comments)
above — Status uses the same model for its worktree comments (see
[Status view](#status-view-net-worktree-diff-and-comments) below). The cursor
is clamped to the (possibly-shrunk) target list after every relist and resets
to the top on a file or mode change, with one deliberate exception: `]`/`[`
(comment navigation) switch the file first, then place the cursor on the
target comment rather than the top.

## Status view: net worktree diff and comments

The Status diff pane always shows the selected file's **net `HEAD`-vs-worktree
diff** (`Repo::file_diff_head_vs_worktree(&FileEntry)`, a sibling of `diff`/
`diff_sides` in `git/diff.rs`), labeled `pending · HEAD→worktree` — not the
per-section (staged xor unstaged) diff a milestone-6 session showed for the
selected file. Because strix stages whole files, not hunks, this is identical
to the old per-section diff for a file that's *only* staged or *only*
unstaged; it differs only for a file staged and then re-edited, where the
pane now shows the combined change (documented as a net-tree-diff caveat, not
a bug — see [Usage](../getting-started/usage.md#on-uncommitted-work)). A path
listed in both the Staged and Changes sections is one net diff, one comment
target, and one `● n` badge (`ui/staging.rs`, keyed by path, not by
`(section, path)`). A conflicted file has no clean net diff to comment on;
binary and submodule files have no code lines to anchor to; an unborn `HEAD`
treats the old side as empty (worktree content shows as additions only).

Comments on this pane are `Scope::WorkTree` — see
[the lifecycle in Comments store](#comments-store) above for how they track
edits, staging, and commits. `App::worktree_facts` computes the per-comment
`FileFacts` (`Present`/`Gone`, plus the `resolved_in_head` signal) the sweep
engine needs each refresh, resolving a renamed file by its `orig_path` so a
committed rename doesn't silently orphan or lose the note, and treating a
pending (uncommitted) deletion as retained rather than swept — only an
unambiguous worktree-local deletion under an unchanged `HEAD` sweeps
unconditionally.

## Menu bar

The `View`/`Theme` labels live inside the existing one-row header, not a new
row — `ui/menu.rs` holds the geometry/label helpers (`header_menu_layout`
lays the two labels left-to-right from a given start column; the dropdown
renderer draws the open box over `Clear`), shared by two consumers: the
header draw always paints the labels when `App::show_menu_bar` is set, and,
when a menu is open, the same layout function's rects are recorded so a
click can be matched back to a label. Like the rest of the UI, the render
pass is a pure function of `App`'s *logical* state and records only mouse
hit-rects back onto `App` through interior-mutability setters (`Cell`/
`RefCell`) — the same recorded-rect pattern the staging area, the split
divider, and the comment-box `[x]` cells already use. `App::open_menu: Option<OpenMenu>`
(`MenuId` — `View` or `Theme` — plus a highlighted-item index) is the only
open/closed state; `App::menu_items(MenuId)` builds each dropdown's rows
fresh on every call (radio/check markers reflecting current state, e.g. the
`View` menu's Status/History rows check whichever is active) rather than
caching them, so there's no stale-marker state to invalidate.

**Hit-testing.** Two recorded-rect maps back mouse input to menu structure:
`menu_title_rects` (one `Rect` per top-level label, cleared when the bar is
hidden) and a dropdown hit-map (the open box's bounds plus a
`(Option<MenuCommand>, Rect)` per row, cleared when the menu closes) —
both populated during render, not computed on click, so hit-testing never
duplicates the layout math. `on_key_menu` intercepts keys before the regular
keymap whenever a menu is open (Left/Right/Tab switch top-level menus,
Up/Down move the highlight and skip separators, Enter/Space activate then
close, anything else including `Esc` and the `ToggleMenuBar` chord closes
without acting); mouse routing checks a title rect first (open/switch), then
the open dropdown's hit-map (activate on an item, consume on a separator or
dead space, hover-slide to follow the pointer), then falls through to the
regular click handling only when nothing in the bar or dropdown was hit.
Opening a dropdown is mouse-only — there's no keyboard chord to open one,
since every setting a menu exposes already has its own direct key. A
`reload()` (config or watcher-triggered) clears `open_menu` unconditionally,
so a live re-render mid-navigation never leaves a dropdown pointing at rows
that no longer exist.

## Configuration write-back

Four explicit in-app actions — cycling the theme (`t`), toggling diff mode
(`d`), toggling line numbers (`n`), and toggling the menu bar (`m`) — write
their new value back to `config.toml` via `toml_edit`, which preserves
comments, unrelated keys, and formatting elsewhere in the file. Picking a
theme, diff mode, or line-numbers setting from the `View`/`Theme` dropdowns
persists the same way, through the same `persist_setting` call the key
dispatch uses — the menu is a second input surface over the same actions, not
a separate write path; the menu bar's own visibility has no menu item, only
the `m` chord. If the existing file fails to parse, the write is skipped
entirely (the in-app change still applies for the running session) rather
than clobbering a user's broken-but-recoverable config. Reads stay on
`toml`/serde; only the write path uses `toml_edit`.

## Performance

Targets (small to mid-size repos, up to ~2M LOC):

| Metric                                  | Target              |
|-----------------------------------------|---------------------|
| Startup                                 | < 100 ms            |
| Diff render (typical file)              | < 50 ms             |
| Scrolling / pane switching              | No frame drops      |
| Resident memory                         | Tens of MB, not hundreds |

How they're met:

- Rendered diffs are cached per `(file, mode)` and invalidated when the file or
  diff mode changes.
- Syntax highlighting runs over the visible window plus a small buffer, not the
  whole file.
- The history walk decodes commit objects only (no trees or blobs) and is
  bounded to a page (500 commits); per-commit trees and blobs load lazily for
  the selected commit.

## Non-goals

strix deliberately leaves these to `git` itself, to other tools, or to a future
phase:

- Commit creation (use `git commit`)
- Branch management (create / switch / delete branches)
- Merge conflict resolution
- Stashing
- Remote operations (push, pull, fetch)
- Hunk-level or line-level staging (file-level only)
- A daemon or long-running session: comments are a plain JSON file on disk
  (`.git/strix/comments.json`), read and written by whichever process (TUI or
  `strix comment`) happens to run, never a background service.
- Comments in the History view (still absent — only Status and Review carry
  them); the staged/unstaged distinction isn't itself commentable (a comment
  anchors to the net diff, not to a section).

Reviewing a branch against its base (`strix diff <base>`) is explicitly **in
scope** — it's the foundation for strix becoming the review surface for
agent-written code. Inline comments — on uncommitted work in the Status view
or in a `strix diff` review session (`c`/double-click to add or edit, `X`/
`[x]` to delete, `]`/`[` to navigate — never `x`, which stays the Changes-pane
discard key), plus `strix comment` for agents — build directly on it (see
[Diff pane: rows, cursor, and comments](#diff-pane-rows-cursor-and-comments),
[Status view](#status-view-net-worktree-diff-and-comments), and
[comments store](#comments-store) above), and the bundled `strix-review`
skill (see [skill distribution](#skill-distribution)) teaches agents both the
committed-range and the working-tree loop.

Keeping the surface area narrow otherwise is the point: strix earns its place
by doing "review a changeset and stage it," "browse history," and "review a
changeset — working tree or branch — with inline comments" very well, not by
covering everything `git` does.

## Testing

- **Render tests** build an `App`, call `dump_frame`, and assert on the text
  grid — fast, deterministic, no terminal required.
- **Colour assertions** use shared `tests/common` helpers (`cell_fg`/`cell_bg`/
  `row_has_fg`/`row_has_bg`) against the same `TestBackend` render path
  `dump_frame` uses, for cases text alone can't cover — the cursor highlight,
  a stale comment's dimmed accent, and split-view word emphasis.
- **Git tests** create a temp repo (`git init` in a `tempfile::tempdir`),
  exercise the git layer, and assert on the results.
- **Mouse tests** render a frame, then drive `on_mouse_at` with explicit
  `Instant`s so double-click timing is asserted deterministically, without
  sleeping.
