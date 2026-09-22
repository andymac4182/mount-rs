#!/bin/sh
set -eu

# Credential-free regression cases for the local/hosted AWS S3 harness input
# validator. The fixtures are synthetic and never invoke aws, cargo, or a
# provider.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
validator=$repo_dir/scripts/validate-aws-s3-test-config.sh

run_validator() {
  case_name=$1
  bucket=mount-rs-integration-123456789012-ap-southeast-2
  region=ap-southeast-2
  prefix=mount-rs-tests/aws-s3/ci/123-1
  role=
  expected=123456789012
  access_key=
  secret_key=
  session_token=
  profile=
  endpoint=

  case "$case_name" in
    valid_profile) profile=synthetic-profile ;;
    valid_explicit)
      access_key=AKIAEXAMPLE123456789
      secret_key=synthetic-secret-only
      session_token=synthetic-session-only
      ;;
    valid_role)
      role=arn:aws:iam::123456789012:role/mount-rs-aws-s3-ci
      expected=123456789012
      profile=synthetic-profile
      ;;
    missing_expected_account) expected= ;;
    missing_expected_profile)
      expected=
      profile=synthetic-profile
      ;;
    role_account_mismatch)
      role=arn:aws:iam::210987654321:role/mount-rs-aws-s3-ci
      expected=123456789012
      ;;
    incomplete_access) access_key=synthetic-access-only ;;
    incomplete_session) session_token=synthetic-session-only ;;
    ambiguous_sources)
      access_key=AKIAEXAMPLE123456789
      secret_key=synthetic-secret-only
      profile=synthetic-profile
      ;;
    endpoint_override) endpoint=https://example.invalid ;;
    unsafe_prefix) prefix=mount-rs-tests/aws-s3/ci/../escape ;;
    *)
      echo "unknown test case: $case_name" >&2
      return 2
      ;;
  esac

  env -i \
    PATH="$PATH" \
    AWS_S3_TEST_BUCKET="$bucket" \
    AWS_S3_TEST_REGION="$region" \
    MOUNT_RS_AWS_S3_TEST_PREFIX="$prefix" \
    AWS_S3_TEST_ROLE_ARN="$role" \
    AWS_S3_TEST_EXPECTED_ACCOUNT_ID="$expected" \
    AWS_ACCESS_KEY_ID="$access_key" \
    AWS_SECRET_ACCESS_KEY="$secret_key" \
    AWS_SESSION_TOKEN="$session_token" \
    AWS_PROFILE="$profile" \
    AWS_S3_ENDPOINT="$endpoint" \
    "$validator"
}

fail() {
  echo "AWS_S3_TEST_CONFIG_TEST_FAILED $*" >&2
  exit 1
}

for valid_case in valid_profile valid_explicit valid_role; do
  output=$(run_validator "$valid_case" 2>&1) || fail "${valid_case}_rejected"
  case "$output" in
    *AWS_S3_TEST_CONFIG_PASS*role=redacted*) ;;
    *) fail "${valid_case}_missing_pass_marker" ;;
  esac
  case "$output" in
    *synthetic-*|*AKIAEXAMPLE*) fail "${valid_case}_credential_leak" ;;
  esac
done

expect_blocked() {
  case_name=$1
  expected_marker=$2
  if output=$(run_validator "$case_name" 2>&1); then
    fail "${case_name}_unexpected_pass"
  fi
  case "$output" in
    *"AWS_S3_TEST_CONFIG_BLOCKED $expected_marker"*) ;;
    *) fail "${case_name}_wrong_marker" ;;
  esac
  case "$output" in
    *synthetic-*|*AKIAEXAMPLE*) fail "${case_name}_credential_leak" ;;
  esac
}

expect_blocked missing_expected_account missing_expected_account_id
expect_blocked missing_expected_profile missing_expected_account_id
expect_blocked role_account_mismatch role_account_mismatch
expect_blocked incomplete_access incomplete_access_credentials
expect_blocked incomplete_session incomplete_session_credentials
expect_blocked ambiguous_sources ambiguous_credential_sources
expect_blocked endpoint_override endpoint_override_detected
expect_blocked unsafe_prefix unsafe_prefix

echo "AWS_S3_TEST_CONFIG_TEST_PASS cases=11"
