#!/usr/bin/env bash
# Bump strix's release version.
#
# strix is a single-crate package: the canonical version is `[package].version`
# in the root `Cargo.toml`. This script bumps that field and regenerates
# `Cargo.lock` so the `strix` lockfile entry matches.
#
# The sed pattern allows variable whitespace around the `=`, so it matches both
# vanilla cargo output (`version = "0.1.0"`) and hand-aligned column layouts. It
# replaces with a single space (the cargo default); cargo doesn't care either way.
#
# Contract (see cc-plugins release-workflows references/update-version/README.md):
#   - one arg: semver string, no `v` prefix
#   - idempotent
#   - no network (--offline)
#   - verifies its own work
#   - does not `git add` (the release skill stages + commits)

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <X.Y.Z>   e.g. $0 0.0.1" >&2
  exit 2
fi
V="$1"

if [[ ! "$V" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]; then
  echo "error: '$V' is not semver (X.Y.Z or X.Y.Z-suffix)" >&2
  exit 2
fi

# 1. Bump [package].version. Variable whitespace around `=` so both vanilla
#    cargo (`version = "0.1.0"`) and column-aligned layouts match.
sed -i.bak -E 's/^version[[:space:]]*=[[:space:]]*"[^"]+"/version = "'"$V"'"/' Cargo.toml
rm -f Cargo.toml.bak

# 2. Verify Cargo.toml saw the bump. A silent sed no-match is the most common
#    failure mode here; catch it before blaming the lockfile.
if ! grep -q "^version = \"$V\"" Cargo.toml; then
  echo "error: Cargo.toml's [package].version did not update to $V." >&2
  echo "       The sed pattern matches \`version (whitespace) = (whitespace) \"<value>\"\` at column 0." >&2
  echo "       If your manifest has a different shape, adjust the sed pattern in this script." >&2
  exit 1
fi

# 3. Regenerate Cargo.lock so the strix entry matches. --offline is safe: we're
#    only changing an internal version string, not the dependency tree.
cargo update --workspace --offline >/dev/null

# 4. Verify the lockfile saw the bump.
if ! grep -q "^version = \"$V\"" Cargo.lock; then
  echo "error: Cargo.lock did not update to $V" >&2
  exit 1
fi

echo "Bumped Cargo.toml + Cargo.lock to $V"
