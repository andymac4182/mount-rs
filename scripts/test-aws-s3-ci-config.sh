#!/bin/sh
set -eu

# Regression tests for the protected GitHub-environment preflight. The cases
# use synthetic values only and run the validator with a clean environment so
# a developer's local AWS credentials cannot affect the result.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
validator=$repo_dir/scripts/validate-aws-s3-ci-config.sh

run_validator() {
  case_name=$1
  bucket=mount-rs-integration-123456789012-ap-southeast-2
  region=ap-southeast-2
  versioning=None
  account=123456789012
  role=arn:aws:iam::123456789012:role/mount-rs-aws-s3-ci
  prefix=mount-rs-tests/aws-s3/ci/123-1
  endpoint=
  access_key=
  secret_key=
  profile=

  case "$case_name" in
    valid) ;;
    missing_bucket) bucket= ;;
    role_account_mismatch) role=arn:aws:iam::210987654321:role/mount-rs-aws-s3-ci ;;
    endpoint_override) endpoint=https://example.invalid ;;
    unsafe_prefix) prefix=mount-rs-tests/aws-s3/ci/../escape ;;
    static_credentials)
      access_key=AKIAEXAMPLE123456789
      secret_key=synthetic-secret-only
      ;;
    profile_override) profile=synthetic-profile ;;
    *)
      echo "unknown test case: $case_name" >&2
      return 2
      ;;
  esac

  env -i \
    PATH="$PATH" \
    AWS_S3_TEST_BUCKET="$bucket" \
    AWS_S3_TEST_REGION="$region" \
    AWS_S3_TEST_VERSIONING_STATUS="$versioning" \
    AWS_S3_CI_EXPECTED_ACCOUNT_ID="$account" \
    AWS_S3_CI_ROLE_ARN="$role" \
    MOUNT_RS_AWS_S3_TEST_PREFIX="$prefix" \
    AWS_EC2_METADATA_DISABLED=true \
    AWS_S3_ENDPOINT="$endpoint" \
    AWS_ACCESS_KEY_ID="$access_key" \
    AWS_SECRET_ACCESS_KEY="$secret_key" \
    AWS_PROFILE="$profile" \
    "$validator"
}

fail() {
  echo "AWS_S3_CI_CONFIG_TEST_FAILED $*" >&2
  exit 1
}

output=$(run_validator valid 2>&1) || fail "valid_case_rejected"
case "$output" in
  *"AWS_S3_CI_CONFIG_PASS"*"role=redacted"*) ;;
  *) fail "valid_case_missing_pass_marker" ;;
esac
case "$output" in
  *"123456789012:role/mount-rs-aws-s3-ci"*) fail "role_value_leaked" ;;
esac

expect_blocked() {
  case_name=$1
  expected=$2
  if output=$(run_validator "$case_name" 2>&1); then
    fail "${case_name}_unexpected_pass"
  fi
  case "$output" in
    *"AWS_S3_CI_CONFIG_BLOCKED $expected"*) ;;
    *) fail "${case_name}_wrong_marker" ;;
  esac
}

expect_blocked missing_bucket missing_bucket
expect_blocked role_account_mismatch role_account_mismatch
expect_blocked endpoint_override endpoint_override_detected
expect_blocked unsafe_prefix unsafe_test_prefix
expect_blocked static_credentials preconfigured_credentials_detected
expect_blocked profile_override profile_override_detected

echo "AWS_S3_CI_CONFIG_TEST_PASS cases=7"
