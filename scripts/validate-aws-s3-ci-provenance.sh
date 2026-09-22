#!/bin/sh
set -eu

# Validate the non-secret provenance record before the live AWS S3 job can
# authenticate. This binds the evidence to the checked-out GitHub commit and
# main branch, and never prints or handles a credential value.

fail() {
  echo "AWS_S3_CI_PROVENANCE_BLOCKED $*" >&2
  exit 2
}

[ "$#" -eq 1 ] || fail "usage"
provenance=$1
[ -f "$provenance" ] || fail "missing_provenance_file"
[ -r "$provenance" ] || fail "unreadable_provenance_file"

value_for() {
  key=$1
  value=$(awk -v key="$key" '
    index($0, key "=") == 1 {
      if (found) exit 2
      found = 1
      print substr($0, length(key) + 2)
    }
    END {
      if (!found) exit 1
    }
  ' "$provenance" 2>/dev/null) || fail "missing_or_duplicate_$key"
  printf '%s' "$value"
}

require_nonempty() {
  [ -n "$2" ] || fail "missing_$1"
}

source_sha=$(value_for source_sha)
source_subject=$(value_for source_subject)
rustc=$(value_for rustc)
cargo=$(value_for cargo)
ruby=$(value_for ruby)
repository=$(value_for repository)
workflow=$(value_for workflow)
ref=$(value_for ref)
event=$(value_for event)
run_id=$(value_for run_id)
run_attempt=$(value_for run_attempt)
working_tree=$(value_for working_tree)

require_nonempty source_sha "$source_sha"
require_nonempty source_subject "$source_subject"
require_nonempty rustc "$rustc"
require_nonempty cargo "$cargo"
require_nonempty ruby "$ruby"
require_nonempty repository "$repository"
require_nonempty workflow "$workflow"
require_nonempty ref "$ref"
require_nonempty event "$event"
require_nonempty run_id "$run_id"
require_nonempty run_attempt "$run_attempt"
require_nonempty working_tree "$working_tree"

case "$source_sha" in
  *[!0-9a-f]*) fail "invalid_source_sha_shape" ;;
esac
[ "${#source_sha}" -eq 40 ] || fail "invalid_source_sha_length"
[ "$source_sha" = "${GITHUB_SHA:-}" ] || fail "source_sha_mismatch"

[ "$repository" = "${GITHUB_REPOSITORY:-}" ] || fail "repository_mismatch"
[ "$workflow" = "${GITHUB_WORKFLOW:-}" ] || fail "workflow_mismatch"
[ "$ref" = "${GITHUB_REF:-}" ] || fail "ref_mismatch"
[ "$event" = "${GITHUB_EVENT_NAME:-}" ] || fail "event_mismatch"

[ "$ref" = "refs/heads/main" ] || fail "unsupported_ref"
case "$event" in
  push|workflow_dispatch) ;;
  *) fail "unsupported_event" ;;
esac
case "$run_id" in
  ''|*[!0-9]*) fail "invalid_run_id" ;;
esac
case "$run_attempt" in
  ''|*[!0-9]*) fail "invalid_run_attempt" ;;
esac
[ "$working_tree" = clean ] || fail "working_tree_not_clean"

echo "AWS_S3_CI_PROVENANCE_PASS source_sha=$source_sha repository=$repository ref=$ref event=$event run_id=$run_id run_attempt=$run_attempt working_tree=clean"
