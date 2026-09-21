#!/bin/sh
set -eu

# Read-only audit for the exact bucket-policy contract emitted by
# infra/aws-s3-production.yaml. This script never changes AWS resources and
# never prints role ARNs or policy contents.

if ! command -v aws >/dev/null 2>&1; then
  echo "AWS CLI is required" >&2
  exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required" >&2
  exit 2
fi

: "${AWS_S3_AUDIT_BUCKET:?AWS_S3_AUDIT_BUCKET must name the bucket to audit}"
: "${AWS_S3_AUDIT_REGION:?AWS_S3_AUDIT_REGION must name the bucket region}"
: "${AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID:?AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID must name the expected caller account}"
: "${AWS_S3_AUDIT_RUNTIME_ROLE_ARN:?AWS_S3_AUDIT_RUNTIME_ROLE_ARN must name the reviewed runtime role}"
: "${AWS_S3_AUDIT_MAINTENANCE_ROLE_ARN:?AWS_S3_AUDIT_MAINTENANCE_ROLE_ARN must name the reviewed maintenance role}"
: "${AWS_S3_AUDIT_OWNED_PREFIX:?AWS_S3_AUDIT_OWNED_PREFIX must name the owned object prefix}"

fail_input() {
  echo "AWS_S3_BUCKET_POLICY_AUDIT_FAILED $*" >&2
  exit 2
}

case "$AWS_S3_AUDIT_BUCKET" in
  ''|*[!a-z0-9.-]*) fail_input invalid_bucket_shape ;;
esac
[ "${#AWS_S3_AUDIT_BUCKET}" -ge 3 ] || fail_input invalid_bucket_length
[ "${#AWS_S3_AUDIT_BUCKET}" -le 63 ] || fail_input invalid_bucket_length
case "$AWS_S3_AUDIT_BUCKET" in
  [!a-z0-9]*|*[!a-z0-9]|*..*) fail_input invalid_bucket_shape ;;
esac
case "$AWS_S3_AUDIT_REGION" in
  ''|*[!A-Za-z0-9.-]*|[!A-Za-z0-9]*|*[!A-Za-z0-9])
    fail_input invalid_region_shape
    ;;
esac
case "$AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID" in
  ''|*[!0-9]*) fail_input invalid_account_shape ;;
esac
[ "${#AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID}" -eq 12 ] ||
  fail_input invalid_account_length
for role_arn in "$AWS_S3_AUDIT_RUNTIME_ROLE_ARN" "$AWS_S3_AUDIT_MAINTENANCE_ROLE_ARN"; do
  case "$role_arn" in
    arn:aws:iam::????????????:role/*) ;;
    *) fail_input invalid_role_arn_shape ;;
  esac
  case "$role_arn" in
    *[!A-Za-z0-9:/_.+=,@-]*) fail_input invalid_role_arn_characters ;;
  esac
done
[ "$AWS_S3_AUDIT_RUNTIME_ROLE_ARN" != "$AWS_S3_AUDIT_MAINTENANCE_ROLE_ARN" ] ||
  fail_input roles_must_differ
case "$AWS_S3_AUDIT_OWNED_PREFIX" in
  ''|/*|*//*|*/../*|*/..|*/./*|*/.|*[!A-Za-z0-9._/-]*)
    fail_input invalid_owned_prefix
    ;;
esac

fail() {
  echo "AWS_S3_BUCKET_POLICY_AUDIT_FAILED $*" >&2
  exit 1
}

aws_call() {
  AWS_EC2_METADATA_DISABLED=true aws "$@"
}

if [ -n "${AWS_ENDPOINT:-}${AWS_ENDPOINT_URL:-}${AWS_ENDPOINT_URL_S3:-}${AWS_ENDPOINT_URL_STS:-}${AWS_S3_ENDPOINT:-}" ]; then
  fail endpoint_override_detected
fi
profile_name=${AWS_PROFILE:-${AWS_DEFAULT_PROFILE:-default}}
configured_endpoint=$(aws configure get endpoint_url --profile "$profile_name" 2>/dev/null || true)
configured_services=$(aws configure get services --profile "$profile_name" 2>/dev/null || true)
configured_s3_endpoint=$(aws configure get services.s3.endpoint_url --profile "$profile_name" 2>/dev/null || true)
configured_sts_endpoint=$(aws configure get services.sts.endpoint_url --profile "$profile_name" 2>/dev/null || true)
if [ -n "$configured_endpoint$configured_services$configured_s3_endpoint$configured_sts_endpoint" ]; then
  fail profile_endpoint_override_detected
fi

caller_account=$(aws_call sts get-caller-identity --region "$AWS_S3_AUDIT_REGION" --query Account --output text) ||
  fail caller_identity_unavailable
[ "$caller_account" = "$AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID" ] ||
  fail unexpected_caller_account

policy_raw=$(aws_call s3api get-bucket-policy --bucket "$AWS_S3_AUDIT_BUCKET" \
  --query Policy --output text 2>/dev/null) || fail bucket_policy_unavailable
policy_json=$(printf '%s' "$policy_raw" | jq -cer 'if type == "string" then fromjson else . end' 2>/dev/null) ||
  fail bucket_policy_invalid_json

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bucket_arn="arn:aws:s3:::$AWS_S3_AUDIT_BUCKET"
printf '%s' "$policy_json" | jq -e -f "$repo_dir/scripts/check-aws-s3-bucket-policy.jq" \
  --arg bucket_arn "$bucket_arn" \
  --arg runtime_role "$AWS_S3_AUDIT_RUNTIME_ROLE_ARN" \
  --arg maintenance_role "$AWS_S3_AUDIT_MAINTENANCE_ROLE_ARN" \
  --arg owned_prefix "$AWS_S3_AUDIT_OWNED_PREFIX" >/dev/null ||
  fail bucket_policy_contract_mismatch

echo "AWS_S3_BUCKET_POLICY_AUDIT_PASS bucket=$AWS_S3_AUDIT_BUCKET region=$AWS_S3_AUDIT_REGION expected_account=$AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID owned_prefix=$AWS_S3_AUDIT_OWNED_PREFIX runtime_role=redacted maintenance_role=redacted statements=5"
