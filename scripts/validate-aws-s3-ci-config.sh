#!/bin/sh
set -eu

# Validate the protected GitHub environment contract before the OIDC action
# runs. This is intentionally secret-safe: it checks only shape and presence,
# never prints the role ARN or any credential value, and makes no AWS calls.

fail() {
  echo "AWS_S3_CI_CONFIG_BLOCKED $*" >&2
  exit 2
}

[ -n "${AWS_S3_TEST_BUCKET:-}" ] || fail "missing_bucket"
[ -n "${AWS_S3_TEST_REGION:-}" ] || fail "missing_region"
[ -n "${AWS_S3_TEST_VERSIONING_STATUS:-}" ] || fail "missing_versioning_status"
[ -n "${AWS_S3_CI_ROLE_ARN:-}" ] || fail "missing_role_arn"
[ -n "${MOUNT_RS_AWS_S3_TEST_PREFIX:-}" ] || fail "missing_test_prefix"

case "$AWS_S3_TEST_BUCKET" in
  ''|*[!a-z0-9.-]*) fail "invalid_bucket_shape" ;;
esac
case "$AWS_S3_TEST_REGION" in
  ''|*[!A-Za-z0-9.-]*) fail "invalid_region_shape" ;;
esac
case "$AWS_S3_TEST_VERSIONING_STATUS" in
  None|Enabled|Suspended) ;;
  *) fail "invalid_versioning_status" ;;
esac
case "$MOUNT_RS_AWS_S3_TEST_PREFIX" in
  mount-rs-tests/aws-s3/ci/?*) ;;
  *) fail "unsafe_test_prefix" ;;
esac
case "$MOUNT_RS_AWS_S3_TEST_PREFIX" in
  *//*|*/../*|*/..|*/./*|*/.) fail "unsafe_test_prefix" ;;
esac

case "$AWS_S3_CI_ROLE_ARN" in
  arn:aws:iam::????????????:role/*) ;;
  *) fail "invalid_role_arn_shape" ;;
esac
case "$AWS_S3_CI_ROLE_ARN" in
  *[!A-Za-z0-9:/_.+=,@-]*) fail "invalid_role_arn_characters" ;;
esac
role_account=${AWS_S3_CI_ROLE_ARN#arn:aws:iam::}
role_account=${role_account%%:role/*}
case "$role_account" in
  ''|*[!0-9]*) fail "invalid_role_account_shape" ;;
esac
[ "${#role_account}" -eq 12 ] || fail "invalid_role_account_length"

[ "${AWS_EC2_METADATA_DISABLED:-}" = "true" ] ||
  fail "AWS_EC2_METADATA_DISABLED_must_be_true"

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
  [ -z "$endpoint_value" ] || fail "endpoint_override_detected"
done

if [ -n "${AWS_ACCESS_KEY_ID:-}${AWS_SECRET_ACCESS_KEY:-}${AWS_SESSION_TOKEN:-}" ]; then
  fail "preconfigured_credentials_detected"
fi

echo "AWS_S3_CI_CONFIG_PASS bucket=$AWS_S3_TEST_BUCKET region=$AWS_S3_TEST_REGION versioning=$AWS_S3_TEST_VERSIONING_STATUS role=redacted"
