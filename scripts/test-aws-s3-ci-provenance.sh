#!/bin/sh
set -eu

# Credential-free regression tests for the hosted AWS S3 provenance binding.
# The fixture contains only synthetic GitHub metadata and toolchain strings.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
validator=$repo_dir/scripts/validate-aws-s3-ci-provenance.sh
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-aws-s3-provenance.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
provenance=$tmp_dir/provenance.txt

source_sha=0123456789abcdef0123456789abcdef01234567

write_fixture() {
  case_name=$1
  printf '%s\n' \
    "source_sha=$source_sha" \
    'source_subject=synthetic provenance fixture' \
    'rustc=rustc 1.95.0 (synthetic)' \
    'cargo=cargo 1.95.0 (synthetic)' \
    'ruby=ruby 3.4.0 (synthetic)' \
    'repository=andymac4182/mount-rs' \
    'workflow=Live AWS S3' \
    'ref=refs/heads/main' \
    'event=push' \
    'run_id=123456789' \
    'run_attempt=1' \
    'working_tree=clean' > "$provenance"

  case "$case_name" in
    valid) ;;
    source_sha_mismatch) sed -i.bak 's/^source_sha=.*/source_sha=abcdefabcdefabcdefabcdefabcdefabcdefabcd/' "$provenance"; rm -f "$provenance.bak" ;;
    dirty_tree) sed -i.bak 's/^working_tree=.*/working_tree=modified/' "$provenance"; rm -f "$provenance.bak" ;;
    non_main_ref) sed -i.bak 's#^ref=.*#ref=refs/heads/feature#' "$provenance"; rm -f "$provenance.bak" ;;
    duplicate_source_sha) printf '%s\n' "source_sha=$source_sha" >> "$provenance" ;;
    *)
      echo "unknown test case: $case_name" >&2
      return 2
      ;;
  esac
}

run_validator() {
  write_fixture "$1"
  env -i \
    PATH="$PATH" \
    GITHUB_SHA="$source_sha" \
    GITHUB_REPOSITORY=andymac4182/mount-rs \
    GITHUB_WORKFLOW='Live AWS S3' \
    GITHUB_REF=refs/heads/main \
    GITHUB_EVENT_NAME=push \
    "$validator" "$provenance"
}

fail() {
  echo "AWS_S3_CI_PROVENANCE_TEST_FAILED $*" >&2
  exit 1
}

output=$(run_validator valid 2>&1) || fail valid_case_rejected
case "$output" in
  *"AWS_S3_CI_PROVENANCE_PASS"*"working_tree=clean"*) ;;
  *) fail valid_case_missing_pass_marker ;;
esac

expect_blocked() {
  case_name=$1
  expected=$2
  if output=$(run_validator "$case_name" 2>&1); then
    fail "${case_name}_unexpected_pass"
  fi
  case "$output" in
    *"AWS_S3_CI_PROVENANCE_BLOCKED $expected"*) ;;
    *) fail "${case_name}_wrong_marker" ;;
  esac
}

expect_blocked source_sha_mismatch source_sha_mismatch
expect_blocked dirty_tree working_tree_not_clean
expect_blocked non_main_ref ref_mismatch
expect_blocked duplicate_source_sha missing_or_duplicate_source_sha

echo "AWS_S3_CI_PROVENANCE_TEST_PASS cases=5"
