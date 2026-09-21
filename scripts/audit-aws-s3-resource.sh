#!/bin/sh
set -eu

# Read-only AWS S3 resource-control audit for the W25 rollout checklist.
# This script never creates, updates, or deletes AWS resources. It checks the
# controls that make a bucket suitable for a mount-rs block plane; identity,
# metadata, recovery, and hosted-release qualification remain separate gates.

if ! command -v aws >/dev/null 2>&1; then
  echo "AWS CLI is required" >&2
  exit 2
fi

: "${AWS_S3_AUDIT_BUCKET:?AWS_S3_AUDIT_BUCKET must name the bucket to audit}"
: "${AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID:?AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID must name the expected caller account}"
region=${AWS_S3_AUDIT_REGION:-${AWS_REGION:-${AWS_DEFAULT_REGION:-}}}
: "${region:?AWS_S3_AUDIT_REGION or AWS_REGION must name the bucket region}"
expected_prefix=${AWS_S3_AUDIT_LIFECYCLE_PREFIX:-mount-rs-tests/}
expected_expiration_days=${AWS_S3_AUDIT_EXPIRATION_DAYS:-7}
expected_abort_days=${AWS_S3_AUDIT_ABORT_MULTIPART_DAYS:-1}
expected_sse=${AWS_S3_AUDIT_EXPECTED_SSE:-AES256}
expected_versioning=${AWS_S3_AUDIT_EXPECTED_VERSIONING_STATUS:-}

case "$AWS_S3_AUDIT_BUCKET" in
  ''|*[!a-z0-9.-]*)
    echo "AWS_S3_AUDIT_BUCKET contains an invalid bucket name" >&2
    exit 2
    ;;
esac
[ "${#AWS_S3_AUDIT_BUCKET}" -ge 3 ] || {
  echo "AWS_S3_AUDIT_BUCKET must contain 3-63 characters" >&2
  exit 2
}
[ "${#AWS_S3_AUDIT_BUCKET}" -le 63 ] || {
  echo "AWS_S3_AUDIT_BUCKET must contain 3-63 characters" >&2
  exit 2
}
case "$AWS_S3_AUDIT_BUCKET" in
  [!a-z0-9]*|*[!a-z0-9]|*..*)
    echo "AWS_S3_AUDIT_BUCKET must start/end alphanumerically without consecutive dots" >&2
    exit 2
    ;;
esac
case "$region" in
  ''|*[!A-Za-z0-9.-]*)
    echo "AWS_S3_AUDIT_REGION contains an invalid AWS region" >&2
    exit 2
    ;;
esac
case "$AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID" in
  ''|*[!0-9]*)
    echo "AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID must contain only digits" >&2
    exit 2
    ;;
esac
if [ "${#AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID}" -ne 12 ]; then
  echo "AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID must contain 12 digits" >&2
  exit 2
fi
case "$expected_prefix" in
  ''|/*|*//*|*/../*|*/..|*/./*|*/.)
    echo "AWS_S3_AUDIT_LIFECYCLE_PREFIX contains an unsafe path" >&2
    exit 2
    ;;
esac
case "$expected_versioning" in
  ''|None|Enabled|Suspended) ;;
  *)
    echo "AWS_S3_AUDIT_EXPECTED_VERSIONING_STATUS must be None, Enabled, or Suspended" >&2
    exit 2
    ;;
esac

aws_call() {
  AWS_EC2_METADATA_DISABLED=true aws "$@"
}

fail() {
  echo "AWS_S3_RESOURCE_AUDIT_FAILED $*" >&2
  exit 1
}

reject_endpoint_overrides() {
  if [ -n "${AWS_ENDPOINT:-}" ] || [ -n "${AWS_ENDPOINT_URL:-}" ] || \
    [ -n "${AWS_ENDPOINT_URL_S3:-}" ] || [ -n "${AWS_ENDPOINT_URL_STS:-}" ] || \
    [ -n "${AWS_S3_ENDPOINT:-}" ]; then
    echo "AWS_S3_RESOURCE_AUDIT_FAILED endpoint_override_detected" >&2
    exit 2
  fi

  profile_name=${AWS_PROFILE:-${AWS_DEFAULT_PROFILE:-default}}
  configured_endpoint=$(aws configure get endpoint_url --profile "$profile_name" 2>/dev/null || true)
  configured_services=$(aws configure get services --profile "$profile_name" 2>/dev/null || true)
  configured_s3_endpoint=$(aws configure get services.s3.endpoint_url --profile "$profile_name" 2>/dev/null || true)
  configured_sts_endpoint=$(aws configure get services.sts.endpoint_url --profile "$profile_name" 2>/dev/null || true)
  if [ -n "$configured_endpoint" ] || [ -n "$configured_services" ] || \
    [ -n "$configured_s3_endpoint" ] || [ -n "$configured_sts_endpoint" ]; then
    echo "AWS_S3_RESOURCE_AUDIT_FAILED profile_endpoint_override_detected" >&2
    exit 2
  fi
}

read_value() {
  aws_call s3api "$@" --bucket "$AWS_S3_AUDIT_BUCKET" --region "$region" --output text
}

normalize_bool() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

reject_endpoint_overrides

caller_account=$(aws_call sts get-caller-identity --region "$region" --query Account --output text) ||
  fail "caller_identity_unavailable"
[ "$caller_account" = "$AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID" ] ||
  fail "unexpected_caller_account expected=$AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID value=$caller_account"
caller_arn=$(aws_call sts get-caller-identity --region "$region" --query Arn --output text) ||
  fail "caller_identity_unavailable"

bucket_region=$(read_value get-bucket-location --query LocationConstraint) ||
  fail "bucket_location_unavailable"
case "$bucket_region" in
  None|none|'') bucket_region=us-east-1 ;;
esac
[ "$bucket_region" = "$region" ] ||
  fail "unexpected_bucket_region expected=$region value=$bucket_region"

for field in BlockPublicAcls IgnorePublicAcls BlockPublicPolicy RestrictPublicBuckets; do
  value=$(read_value get-public-access-block --query "PublicAccessBlockConfiguration.$field") ||
    fail "public_access_block_unavailable field=$field"
  [ "$(normalize_bool "$value")" = "true" ] ||
    fail "public_access_block_disabled field=$field value=$value"
done

ownership=$(read_value get-bucket-ownership-controls --query 'OwnershipControls.Rules[0].ObjectOwnership') ||
  fail "ownership_controls_unavailable"
[ "$ownership" = "BucketOwnerEnforced" ] ||
  fail "bucket_owner_enforced_required value=$ownership"

sse=$(read_value get-bucket-encryption \
  --query 'ServerSideEncryptionConfiguration.Rules[0].ApplyServerSideEncryptionByDefault.SSEAlgorithm') ||
  fail "default_encryption_unavailable"
[ "$sse" = "$expected_sse" ] ||
  fail "unexpected_default_encryption expected=$expected_sse value=$sse"

versioning=$(read_value get-bucket-versioning --query Status) ||
  fail "versioning_status_unavailable"

noncurrent_expiration_days=
case "$versioning" in
  Enabled|Suspended)
    noncurrent_expiration_days=$(read_value get-bucket-lifecycle-configuration \
      --query "Rules[?Status=='Enabled' && Prefix=='$expected_prefix'].NoncurrentVersionExpiration.NoncurrentDays | [0]") ||
      fail "noncurrent_lifecycle_unavailable"
    if [ "$noncurrent_expiration_days" = "None" ] ||
      [ -z "$noncurrent_expiration_days" ]; then
      noncurrent_expiration_days=$(read_value get-bucket-lifecycle-configuration \
        --query "Rules[?Status=='Enabled' && Filter.Prefix=='$expected_prefix'].NoncurrentVersionExpiration.NoncurrentDays | [0]") ||
        fail "noncurrent_lifecycle_unavailable"
    fi
    [ "$noncurrent_expiration_days" = "$expected_expiration_days" ] ||
      fail "unexpected_noncurrent_lifecycle_expiration expected=$expected_expiration_days value=$noncurrent_expiration_days"
    ;;
  None|"") ;;
  *) fail "invalid_versioning_status value=$versioning" ;;
esac
[ -z "$expected_versioning" ] || [ "$versioning" = "$expected_versioning" ] ||
  fail "unexpected_versioning_status expected=$expected_versioning value=$versioning"

expiration_days=$(read_value get-bucket-lifecycle-configuration \
  --query "Rules[?Status=='Enabled' && Prefix=='$expected_prefix'].Expiration.Days | [0]") ||
  fail "lifecycle_unavailable"
if [ "$expiration_days" = "None" ] || [ -z "$expiration_days" ]; then
  expiration_days=$(read_value get-bucket-lifecycle-configuration \
    --query "Rules[?Status=='Enabled' && Filter.Prefix=='$expected_prefix'].Expiration.Days | [0]") ||
    fail "lifecycle_unavailable"
fi
[ "$expiration_days" = "$expected_expiration_days" ] ||
  fail "unexpected_lifecycle_expiration expected=$expected_expiration_days value=$expiration_days"

abort_days=$(read_value get-bucket-lifecycle-configuration \
  --query "Rules[?Status=='Enabled' && AbortIncompleteMultipartUpload].AbortIncompleteMultipartUpload.DaysAfterInitiation | [0]") ||
  fail "multipart_lifecycle_unavailable"
[ "$abort_days" = "$expected_abort_days" ] ||
  fail "unexpected_multipart_abort expected=$expected_abort_days value=$abort_days"

echo "AWS_S3_RESOURCE_AUDIT_PASS bucket=$AWS_S3_AUDIT_BUCKET region=$region caller=$caller_arn ownership=$ownership sse=$sse versioning=${versioning:-None} expected_versioning=${expected_versioning:-not_enforced} lifecycle_prefix=$expected_prefix expiration_days=$expiration_days noncurrent_expiration_days=${noncurrent_expiration_days:-not_applicable} abort_multipart_days=$abort_days"
