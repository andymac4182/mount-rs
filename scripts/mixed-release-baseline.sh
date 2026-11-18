#!/usr/bin/env bash
# Prints the commit that CI job cluster-mixed-release runs against the commit
# under test, as "<sha> <what it is>".
#
# The baseline is the newest published CLI release that can be in a cluster
# with this commit: the highest `v*-cli-preview*` tag, by version, that
#   - descends from the floor commit (the first build the mixed-release test
#     ever passed with; releases older than it predate cluster forwarding as
#     tested here and are not candidates), and
#   - is not the commit under test itself (a release is tested against the
#     release before it, not against itself).
# With no such tag the baseline is the floor commit. So the baseline moves by
# itself when a release is cut; nothing in the workflow has to be edited.
#
# Usage: mixed-release-baseline.sh <floor-commit> [<commit-under-test>]
# Needs a clone with full history and tags (fetch-depth: 0).
set -euo pipefail

floor="${1:?usage: mixed-release-baseline.sh <floor-commit> [<commit-under-test>]}"
head_ref="${2:-HEAD}"

floor_sha="$(git rev-parse --verify --quiet "${floor}^{commit}")" || {
  echo "mixed-release-baseline: floor commit $floor is not in this clone (is it shallow?)" >&2
  exit 1
}
head_sha="$(git rev-parse --verify --quiet "${head_ref}^{commit}")" || {
  echo "mixed-release-baseline: $head_ref is not a commit" >&2
  exit 1
}

while IFS= read -r tag; do
  [ -n "$tag" ] || continue
  tag_sha="$(git rev-parse --verify --quiet "refs/tags/${tag}^{commit}")" || continue
  [ "$tag_sha" != "$head_sha" ] || continue
  git merge-base --is-ancestor "$floor_sha" "$tag_sha" || continue
  printf '%s release %s\n' "$tag_sha" "$tag"
  exit 0
done < <(git tag --list 'v*-cli-preview*' --sort=-version:refname)

printf '%s floor commit (no release contains it yet)\n' "$floor_sha"
