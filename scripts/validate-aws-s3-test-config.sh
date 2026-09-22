#!/bin/sh
set -eu

# Validate the local/hosted AWS S3 harness inputs before it invokes the AWS CLI.
# This is deliberately credential-safe: it checks only presence and shape, never
# prints credential values, and makes no network calls.

fail() {
  echo "AWS_S3_TEST_CONFIG_BLOCKED $*" >&2
  exit 2
}

bucket=${AWS_S3_TEST_BUCKET:-}
region=${AWS_S3_TEST_REGION:-}
prefix=${MOUNT_RS_AWS_S3_TEST_PREFIX:-}
role_arn=${AWS_S3_TEST_ROLE_ARN:-}
expected_account=${AWS_S3_TEST_EXPECTED_ACCOUNT_ID:-}

[ -n "$bucket" ] || fail missing_bucket
[ -n "$region" ] || fail missing_region
[ -n "$prefix" ] || fail missing_prefix

case "$bucket" in
  ''|*[!a-z0-9.-]*) fail invalid_bucket_shape ;;
esac
[ "${#bucket}" -ge 3 ] || fail invalid_bucket_length
[ "${#bucket}" -le 63 ] || fail invalid_bucket_length
case "$bucket" in
  [!a-z0-9]*|*[!a-z0-9]|*..*) fail invalid_bucket_shape ;;
esac

case "$region" in
  ''|*[!A-Za-z0-9.-]*) fail invalid_region_shape ;;
  [!A-Za-z0-9]*|*[!A-Za-z0-9]) fail invalid_region_edge ;;
esac

case "$prefix" in
  mount-rs-tests/aws-s3/*) ;;
  *) fail unsafe_prefix_root ;;
esac
case "$prefix" in
  ''|mount-rs-tests/aws-s3/|*/|*[!A-Za-z0-9_./-]*|*//*|*/../*|*/..|*/./*|*/.)
    fail unsafe_prefix
    ;;
esac
[ "${#prefix}" -le 1024 ] || fail prefix_too_long

for endpoint_variable in \
  AWS_ENDPOINT \
  AWS_ENDPOINT_URL \
  AWS_ENDPOINT_URL_S3 \
  AWS_ENDPOINT_URL_STS \
  AWS_S3_ENDPOINT
do
  case "$endpoint_variable" in
    AWS_ENDPOINT) endpoint_value=${AWS_ENDPOINT:-} ;;
    AWS_ENDPOINT_URL) endpoint_value=${AWS_ENDPOINT_URL:-} ;;
    AWS_ENDPOINT_URL_S3) endpoint_value=${AWS_ENDPOINT_URL_S3:-} ;;
    AWS_ENDPOINT_URL_STS) endpoint_value=${AWS_ENDPOINT_URL_STS:-} ;;
    AWS_S3_ENDPOINT) endpoint_value=${AWS_S3_ENDPOINT:-} ;;
  esac
  [ -z "$endpoint_value" ] || fail endpoint_override_detected
done

access_key=${AWS_ACCESS_KEY_ID:-}
secret_key=${AWS_SECRET_ACCESS_KEY:-}
session_token=${AWS_SESSION_TOKEN:-${AWS_SECURITY_TOKEN:-}}
profile=${AWS_PROFILE:-${AWS_DEFAULT_PROFILE:-}}

if [ -n "$access_key" ] && [ -z "$secret_key" ]; then
  fail incomplete_access_credentials
fi
if [ -z "$access_key" ] && [ -n "$secret_key" ]; then
  fail incomplete_access_credentials
fi
if [ -n "$session_token" ] && { [ -z "$access_key" ] || [ -z "$secret_key" ]; }; then
  fail incomplete_session_credentials
fi
if [ -n "$profile" ] && { [ -n "$access_key" ] || [ -n "$secret_key" ] || [ -n "$session_token" ]; }; then
  fail ambiguous_credential_sources
fi

if [ -n "$role_arn" ]; then
  case "$role_arn" in
    arn:aws:iam::????????????:role/*) ;;
    *) fail invalid_role_arn_shape ;;
  esac
  case "$role_arn" in
    *[!A-Za-z0-9:/_.+=,@-]*) fail invalid_role_arn_characters ;;
  esac

  [ -n "$expected_account" ] || fail missing_expected_account_id
  role_account=${role_arn#arn:aws:iam::}
  role_account=${role_account%%:role/*}
  case "$role_account" in
    ''|*[!0-9]*) fail invalid_role_account_shape ;;
  esac
  [ "${#role_account}" -eq 12 ] || fail invalid_role_account_length
  case "$expected_account" in
    ''|*[!0-9]*) fail invalid_expected_account_id_shape ;;
  esac
  [ "${#expected_account}" -eq 12 ] || fail invalid_expected_account_id_length
  [ "$role_account" = "$expected_account" ] || fail role_account_mismatch
elif [ -n "$expected_account" ]; then
  case "$expected_account" in
    ''|*[!0-9]*) fail invalid_expected_account_id_shape ;;
  esac
  [ "${#expected_account}" -eq 12 ] || fail invalid_expected_account_id_length
fi

if [ -n "$role_arn" ]; then
  account="$expected_account"
else
  account=unbound
fi
echo "AWS_S3_TEST_CONFIG_PASS bucket=$bucket region=$region account=$account role=redacted"
