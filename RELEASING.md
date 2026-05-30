# Releasing strix

The general release framework is `cc-plugins:release-workflows`; this file
documents what's specific to this repo.

## TL;DR

    /release-workflows:release v0.0.1

That's it. Everything else is automatic.

## What happens

1. **`release-workflows:release`** (LLM, local):
   - Verifies branch (`main`) + clean tree + `ci-success` green on HEAD
   - Asks/confirms version
   - Drafts a CHANGELOG entry from `git log v<previous>..HEAD`, commits as
     `docs(changelog): vX.Y.Z entry`
   - Runs `scripts/release/update-version.sh X.Y.Z` → bumps `Cargo.toml` + `Cargo.lock`
   - Commits as `chore(version): bump to X.Y.Z`
   - Tags `vX.Y.Z` (annotated) on the version commit
   - `git push --follow-tags` (admin bypasses the ruleset's `ci-success` rule)

2. **`release.yml`** (CI, on tag push `v*`):
   - **version-check** — tag matches `Cargo.toml`'s `[package].version`
   - **ci-gate** — polls `ci-success` green on the tagged commit
   - **create-release** — extracts this version's `CHANGELOG.md` section → `gh release create`
   - **build** (matrix, native runners) — `cargo build` + tarball for each of
     `darwin/amd64`, `darwin/arm64`, `linux/amd64`, `linux/arm64`; `cargo deb`
     for the two linux targets; uploads `strix_<os>_<arch>.tar.gz` and
     `strix_<version>_<arch>.deb` to the Release
   - **homebrew** — renders `scripts/release/strix.rb.tmpl` with the four tarball
     sha256s and pushes `Formula/strix.rb` to `charliek/homebrew-tap`
   - **apt-dispatch** — fires a `repository_dispatch` (`event_type=publish`,
     `package=strix`) at `charliek/apt-charliek`, which collects the new `.deb`s
     and republishes its apt index

The maintainer runs step 1; everything else is automated.

## Version files this repo owns

`scripts/release/update-version.sh` bumps:

- `Cargo.toml` — `[package].version`, the canonical version (strix is a single
  crate, not a workspace)
- `Cargo.lock` — regenerated via `cargo update --workspace --offline` so the
  `strix` entry matches

NOT bumped:

- `pyproject.toml` (`strix-docs`) — docs-site tooling for MkDocs, not a release
  artifact; its version is unrelated to the binary's

## Snapshot / dev versioning

Not used; main between releases shows the last released version. The working tree
starts at `0.0.0` as a pre-release sentinel until the first `v0.0.1`. For a build
identity beyond "last released" (e.g. `--version` diagnostics), derive it at build
time from `git describe --tags --dirty` rather than snapshotting the source tree.

## Credentials — one App, no PATs

strix uses the shared **release-bot GitHub App (App ID `3902108`)** as the single
credential for every cross-repo push, replacing the per-repo `HOMEBREW_TAP_TOKEN`
and `APT_DISPATCH_TOKEN` fine-grained PATs that prox/roost still use. The App is
installed on **strix**, **charliek/homebrew-tap**, and **charliek/apt-charliek**;
`release.yml` mints a short-lived, single-repo-scoped installation token in CI via
`actions/create-github-app-token` (`owner: charliek`, `repositories: <target>`)
for the formula push and the apt dispatch. The App has Contents: read+write, which
covers both pushing a commit to the tap and calling the `dispatches` endpoint.

### Secrets

| Secret | Purpose | Required? |
|---|---|---|
| `RELEASE_BOT_APP_ID` | release-bot App ID (`3902108`) | required |
| `RELEASE_BOT_APP_KEY` | App private key (`.pem`) | required |

No PATs. If `RELEASE_BOT_APP_ID` is unset, the `homebrew` and `apt-dispatch` jobs
log a warning and skip rather than fail the release.

## Branch protection

`main` is protected by a ruleset with `required_status_checks=['ci-success']` and
two bypass actors:

- release-bot App (App ID `3902108`, type `Integration`) — for any future
  CI-driven push back to this repo
- Admin role (id `5`, type `RepositoryRole`) — lets `/release-workflows:release`
  push the changelog + version commits + tag (which have no `ci-success` yet at
  push time)

Ruleset id: `17067528`. Inspect or edit at <https://github.com/charliek/strix/rules>.

`charliek/homebrew-tap` and `charliek/apt-charliek` have unprotected `main`
branches, so the App needs only to be *installed* on them (Contents:write); no
ruleset/bypass wiring is required there.

## When things break

| Symptom | Cause | Fix |
|---|---|---|
| `git push` rejected: "Required status check ci-success" | Pusher not in ruleset bypass | Confirm both the App and the admin role are in `bypass_actors` (see github-app.md) |
| `create-github-app-token` → `no access to <repo>` | App not installed on that repo | Install the App on the missing repo, then re-run |
| `update-version.sh` not found | Convention not adopted | Run `/release-workflows:setup` |
| Tag pushed, `version-check` fails | Tagged a commit that didn't run `update-version.sh` | Re-bump locally + cut a fresh patch tag (don't move an existing tag) |
| `homebrew`/`apt-dispatch` warned + skipped | `RELEASE_BOT_APP_ID` unset | Set the App secrets; re-run the release jobs |
| apt repo doesn't show the new version | `strix` missing from apt-charliek `packages.yaml`, or the dispatch didn't fire | Confirm the `strix` entry exists and re-run the `apt-dispatch` job |

## Adopting the convention (for new contributors)

The release pipeline is defined by `cc-plugins`'
[`release-workflows/references/convention.md`](https://github.com/charliek/cc-plugins/blob/main/plugins/release-workflows/references/convention.md).
It is the contract everything in `scripts/release/` and
`.github/workflows/release.yml` is written against.

## Notes for this repo

- strix is the first **Rust** consumer of release-workflows (prox/roost use
  GoReleaser) and the first to use the App as a unified cross-repo credential —
  the `build`/`homebrew` jobs and the App-token variants of `apt-dispatch` are
  hand-written here and are a candidate to fold back into the plugin's templates.
- The `.deb` declares `Depends: git` because strix shells out to `git` at
  runtime (status + stage/unstage/reset); the Homebrew formula `depends_on "git"`
  for the same reason.
