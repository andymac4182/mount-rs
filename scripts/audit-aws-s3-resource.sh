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
region=${AWS_S3_AUDIT_REGION:-${AWS_REGION:-${AWS_DEFAULT_REGION:-}}}
: "${region:?AWS_S3_AUDIT_REGION or AWS_REGION must name the bucket region}"
expected_prefix=${AWS_S3_AUDIT_LIFECYCLE_PREFIX:-mount-rs-tests/}
expected_expiration_days=${AWS_S3_AUDIT_EXPIRATION_DAYS:-7}
expected_abort_days=${AWS_S3_AUDIT_ABORT_MULTIPART_DAYS:-1}
expected_sse=${AWS_S3_AUDIT_EXPECTED_SSE:-AES256}

case "$AWS_S3_AUDIT_BUCKET" in
  ''|*[!a-z0-9.-]*)
    echo "AWS_S3_AUDIT_BUCKET contains an invalid bucket name" >&2
    exit 2
    ;;
esac
case "$region" in
  ''|*[!A-Za-z0-9.-]*)
    echo "AWS_S3_AUDIT_REGION contains an invalid AWS region" >&2
    exit 2
    ;;
esac
case "$expected_prefix" in
  ''|/*|*//*|*/../*|*/..|*/./*|*/.)
    echo "AWS_S3_AUDIT_LIFECYCLE_PREFIX contains an unsafe path" >&2
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

read_value() {
  aws_call s3api "$@" --bucket "$AWS_S3_AUDIT_BUCKET" --region "$region" --output text
}

normalize_bool() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

caller_arn=$(aws_call sts get-caller-identity --region "$region" --query Arn --output text) ||
  fail "caller_identity_unavailable"

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

echo "AWS_S3_RESOURCE_AUDIT_PASS bucket=$AWS_S3_AUDIT_BUCKET region=$region caller=$caller_arn ownership=$ownership sse=$sse versioning=${versioning:-None} lifecycle_prefix=$expected_prefix expiration_days=$expiration_days abort_multipart_days=$abort_days"
