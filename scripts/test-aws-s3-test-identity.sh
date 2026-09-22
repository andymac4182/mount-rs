#!/bin/sh
set -eu

# Credential-free regression cases for the AWS caller-account binding helper.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
validator=$repo_dir/scripts/validate-aws-s3-test-identity.sh

run_validator() {
  case_name=$1
  expected=123456789012
  caller=123456789012

  case "$case_name" in
    valid) ;;
    mismatch) caller=210987654321 ;;
    missing_expected) expected= ;;
    missing_caller) caller= ;;
    malformed_expected) expected=12345678901x ;;
    malformed_caller) caller=12345678901x ;;
    *)
      echo "unknown test case: $case_name" >&2
      return 2
      ;;
  esac

  env -i \
    PATH="$PATH" \
    AWS_S3_TEST_EXPECTED_ACCOUNT_ID="$expected" \
    AWS_S3_TEST_CALLER_ACCOUNT_ID="$caller" \
    "$validator"
}

fail() {
  echo "AWS_S3_TEST_IDENTITY_TEST_FAILED $*" >&2
  exit 1
}

output=$(run_validator valid 2>&1) || fail valid_rejected
case "$output" in
  *"AWS_S3_TEST_IDENTITY_PASS account=123456789012"*) ;;
  *) fail valid_missing_pass_marker ;;
esac

expect_blocked() {
  case_name=$1
  expected_marker=$2
  if output=$(run_validator "$case_name" 2>&1); then
    fail "${case_name}_unexpected_pass"
  fi
  case "$output" in
    *"AWS_S3_TEST_IDENTITY_BLOCKED $expected_marker"*) ;;
    *) fail "${case_name}_wrong_marker" ;;
  esac
}

expect_blocked mismatch caller_account_mismatch
expect_blocked missing_expected missing_expected_account_id
expect_blocked missing_caller missing_caller_account_id
expect_blocked malformed_expected invalid_expected_account_id_shape
expect_blocked malformed_caller invalid_caller_account_id_shape

echo "AWS_S3_TEST_IDENTITY_TEST_PASS cases=6"
