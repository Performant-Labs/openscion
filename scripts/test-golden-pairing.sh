#!/usr/bin/env bash
# Tests scripts/check-golden-pairing.sh against a throwaway repository
# inside this worktree (never /tmp: the janitor cleans outside): a base
# commit and head commits that prove the pairing's three rules - a
# contract-doc change alone passes; a golden change without the doc's
# version line fails; a golden change WITH the version line moved
# passes. Run from the repository root; CI runs it in the lint job.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d "$PWD/.golden-pairing-fixture.XXXXXX")
trap 'rm -rf "$work"' EXIT

git -C "$work" init -q -b fixture
git -C "$work" config user.email t@example.com
git -C "$work" config user.name t

# The contract doc and the golden, at the table's own paths: the other
# rows' files do not exist here, so their diffs are empty and only the
# archive pair is exercised.
mkdir -p "$work/docs" "$work/crates/scion-server/tests"
cat >"$work/docs/archive-format.md" <<'EOD'
# The Scion Archive Format

Version 1 is this document: the layout and the verify steps.
EOD
echo pinned >"$work/crates/scion-server/tests/archive_v1.golden"
git -C "$work" add -A
git -C "$work" commit -qm base
base_sha=$(git -C "$work" rev-parse HEAD)

run_check() { # base head -> exit status of the real script on the row
    # GITHUB_HEAD_REF unset: on a release/* branch the check exits 0 at
    # once, and the rules under test would never run.
    ( cd "$work" && env -u GITHUB_HEAD_REF "$here/check-golden-pairing.sh" "$1" "$2" )
}

fail() { echo "test-golden-pairing: $1" >&2; exit 1; }

# (a) a doc-only change passes.
printf 'Version 1 is this document: the layout, the verify steps, and\nthe 0700 rule a new --out directory is made with.\n' >"$work/docs/archive-format.md"
git -C "$work" add -A
git -C "$work" commit -qm doc-only
if ! run_check HEAD^ HEAD; then fail "(a) a doc-only change must pass"; fi

# (b) a golden change without the version line fails.
echo repinned >"$work/crates/scion-server/tests/archive_v1.golden"
git -C "$work" add -A
git -C "$work" commit -qm golden-only
if run_check HEAD^ HEAD; then fail "(b) a golden change without the version line must fail"; fi

# (c) a golden change WITH the version line moved, in ONE commit from
# the base: the pairing's whole point. The branch restarts at the base
# commit, so the range base..head holds both changes together.
git -C "$work" checkout -qb c "$base_sha"
printf 'Version 2 is this document: the layout and the verify steps.\n' >"$work/docs/archive-format.md"
echo repinned-2 >"$work/crates/scion-server/tests/archive_v1.golden"
git -C "$work" add -A
git -C "$work" commit -qm golden-and-version
if ! run_check "$base_sha" HEAD; then fail "(c) a golden change with the version line moved must pass"; fi

# (d) a describe-rule change whose doc diff outgrows a pipe buffer: the
# proto and a long new section of the protocol doc together. The doc's
# diff is read whole before it is searched; a reader that quit at the
# first match would leave the diff's writer with a broken pipe, and
# pipefail would fail the check on a pull request that pairs.
git -C "$work" checkout -qb d "$base_sha"
mkdir -p "$work/proto"
echo 'syntax = "proto3";' >"$work/proto/scion.proto"
printf '# Scion protocol v1\n' >"$work/docs/protocol-v1.md"
git -C "$work" add -A
git -C "$work" commit -qm describe-base
echo 'service Added {}' >>"$work/proto/scion.proto"
for i in $(seq 4000); do echo "The added service's line $i is described here."; done >>"$work/docs/protocol-v1.md"
git -C "$work" add -A
git -C "$work" commit -qm describe-long-doc
if ! run_check HEAD^ HEAD; then fail "(d) a proto change with a long doc change must pass"; fi

echo "test-golden-pairing: ok"
