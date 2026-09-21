#!/bin/sh
set -eu

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required" >&2
  exit 2
fi

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
filter=$repo_dir/scripts/check-aws-s3-bucket-policy.jq
bucket=mount-rs-integration-123456789012-ap-southeast-2
bucket_arn=arn:aws:s3:::$bucket
prefix=mount-rs/production/volume-a
runtime=arn:aws:iam::123456789012:role/mount-rs/runtime
maintenance=arn:aws:iam::123456789012:role/mount-rs/maintenance

policy=$(jq -n \
  --arg bucket "$bucket_arn" \
  --arg prefix "$prefix" \
  --arg runtime "$runtime" \
  --arg maintenance "$maintenance" '
  {
    Version: "2012-10-17",
    Statement: [
      {
        Sid: "DenyInsecureTransport",
        Effect: "Deny",
        Principal: "*",
        Action: "s3:*",
        Resource: [$bucket, ($bucket + "/*")],
        Condition: {Bool: {"aws:SecureTransport": "false"}}
      },
      {
        Sid: "RuntimeListOwnedPrefix",
        Effect: "Allow",
        Principal: {AWS: $runtime},
        Action: "s3:ListBucket",
        Resource: $bucket,
        Condition: {StringLike: {"s3:prefix": [$prefix, ($prefix + "/*")]}}
      },
      {
        Sid: "RuntimeObjectAccess",
        Effect: "Allow",
        Principal: {AWS: $runtime},
        Action: ["s3:AbortMultipartUpload", "s3:GetObject", "s3:PutObject"],
        Resource: ($bucket + "/" + $prefix + "/*")
      },
      {
        Sid: "MaintenanceListOwnedPrefix",
        Effect: "Allow",
        Principal: {AWS: $maintenance},
        Action: ["s3:ListBucket", "s3:ListBucketMultipartUploads", "s3:ListBucketVersions"],
        Resource: $bucket,
        Condition: {StringLike: {"s3:prefix": [$prefix, ($prefix + "/*")]}}
      },
      {
        Sid: "MaintenanceObjectAccess",
        Effect: "Allow",
        Principal: {AWS: $maintenance},
        Action: ["s3:AbortMultipartUpload", "s3:DeleteObject", "s3:DeleteObjectVersion", "s3:GetObject", "s3:GetObjectVersion", "s3:ListMultipartUploadParts", "s3:PutObject"],
        Resource: ($bucket + "/" + $prefix + "/*")
      }
    ]
  }')

check() {
  printf '%s' "$1" | jq -e -f "$filter" \
    --arg bucket_arn "$bucket_arn" \
    --arg runtime_role "$runtime" \
    --arg maintenance_role "$maintenance" \
    --arg owned_prefix "$prefix" >/dev/null
}

check "$policy"
tampered=$(printf '%s' "$policy" | jq '(.Statement[] | select(.Sid == "RuntimeObjectAccess").Action) = ["s3:GetObject"]')
if check "$tampered"; then
  echo "AWS_S3_BUCKET_POLICY_TEST_FAILED tampered_policy_accepted" >&2
  exit 1
fi

echo "AWS_S3_BUCKET_POLICY_TEST_PASS cases=2"
