# Changelog

All notable changes to strix are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each release below is headed by a `## vX.Y.Z` entry added by
`/release-workflows:release`; `release.yml` turns that section into the GitHub
Release notes.

## v0.0.1 — 2026-05-30

First release. strix is a focused, polished terminal UI for the two git
operations done most often: staging changes and viewing diffs.

### Added
- Staging panel — stage, unstage, and discard changes; repository status read via
  `git status --porcelain`.
- Diff viewing — unified and side-by-side modes with synced scrolling and syntax
  highlighting.
- First-class mouse *and* keyboard control, including a draggable Changes/Diff
  split bar and a key to collapse the Changes panel for a full-width diff.
- Themes and configurable keybindings.
- Auto-refresh — the view updates live on filesystem and git changes.
- Help overlay and error toasts.

<!-- New release sections are inserted above this line. -->
