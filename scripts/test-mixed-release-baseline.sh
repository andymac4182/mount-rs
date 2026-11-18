#!/usr/bin/env bash
# Tests scripts/mixed-release-baseline.sh against a throwaway repository.
set -euo pipefail

script="$(cd "$(dirname "$0")" && pwd)/mixed-release-baseline.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

git init -q .
git config user.email test@example.invalid
git config user.name test
commit() {
  git commit -q --allow-empty -m "$1"
  git rev-parse HEAD
}
expect() {
  local what="$1" want="$2" got="$3"
  if [ "$got" != "$want" ]; then
    echo "FAIL: $what: expected '$want', got '$got'" >&2
    exit 1
  fi
}

old="$(commit old)"
git tag v0.1.0-cli-preview
floor="$(commit floor)"
after="$(commit after)"

# A release older than the floor is not a candidate.
expect "no release contains the floor" \
  "$floor floor commit (no release contains it yet)" "$("$script" "$floor")"

git tag v0.3.0-cli-preview
# A tag on the commit under test is skipped: a release is not its own baseline.
expect "the commit under test is the only release" \
  "$floor floor commit (no release contains it yet)" "$("$script" "$floor")"

next="$(commit next)"
expect "the newest release" "$after release v0.3.0-cli-preview" "$("$script" "$floor")"

git tag v0.10.0-cli-preview.2
git tag v0.9.0-cli-preview
git tag unrelated-tag
last="$(commit last)"
# Ordered by version, not by name: 0.10 is newer than 0.9 and 0.3.
expect "version order" "$next release v0.10.0-cli-preview.2" "$("$script" "$floor")"
expect "explicit commit under test" "$after release v0.3.0-cli-preview" "$("$script" "$floor" "$next")"

if "$script" 0000000000000000000000000000000000000001 >/dev/null 2>&1; then
  echo "FAIL: a floor that is not in the clone was accepted" >&2
  exit 1
fi
: "$old" "$last"
echo "MIXED_RELEASE_BASELINE_TEST_PASS"
