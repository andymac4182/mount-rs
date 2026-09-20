#!/bin/sh
set -eu

# Run the ignored Rust integration tests against the real AWS S3 service.
# This harness never creates IAM users, access keys, buckets, or policies. It
# uses credentials already supplied by the caller or exported by an AWS CLI
# profile, and it cleans only its unique prefix in the configured test bucket.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if [ "${MOUNT_RS_RUN_AWS_S3:-0}" != "1" ]; then
  echo "AWS_S3_TEST_SKIPPED reason=opt_in_required"
  exit 0
fi

if ! command -v aws >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
  echo "AWS S3 test requires the AWS CLI and cargo" >&2
  exit 2
fi

: "${AWS_S3_TEST_BUCKET:?AWS_S3_TEST_BUCKET must name the dedicated private AWS S3 test bucket}"

region=${AWS_S3_TEST_REGION:-${AWS_REGION:-${AWS_DEFAULT_REGION:-ap-southeast-2}}}
prefix=${MOUNT_RS_AWS_S3_TEST_PREFIX:-mount-rs-tests/aws-s3/$(date -u +%Y%m%dT%H%M%SZ)-$$}

case "${AWS_S3_TEST_BUCKET}" in
  ''|*[!a-z0-9.-]*)
    echo "AWS_S3_TEST_BUCKET contains an invalid bucket name" >&2
    exit 2
    ;;
esac
case "$region" in
  ''|*[!A-Za-z0-9.-]*)
    echo "AWS_S3_TEST_REGION contains an invalid AWS region" >&2
    exit 2
    ;;
esac
case "$prefix" in
  mount-rs-tests/aws-s3/*) ;;
  *)
    echo "MOUNT_RS_AWS_S3_TEST_PREFIX must remain below mount-rs-tests/aws-s3/" >&2
    exit 2
    ;;
esac
case "$prefix" in
  ''|*[!A-Za-z0-9_./-]*|*//*|*/../*|*/..|*/./*|*/.)
    echo "MOUNT_RS_AWS_S3_TEST_PREFIX contains an unsafe path" >&2
    exit 2
    ;;
esac

# A local or S3-compatible endpoint would make a green result non-AWS
# evidence. The Rust client also omits an endpoint, but reject CLI endpoint
# overrides so the preflight and the test cannot disagree.
if [ -n "${AWS_ENDPOINT:-}" ] || [ -n "${AWS_ENDPOINT_URL:-}" ] || \
  [ -n "${AWS_ENDPOINT_URL_S3:-}" ] || [ -n "${AWS_S3_ENDPOINT:-}" ]; then
  echo "AWS S3 test refuses custom endpoints; unset AWS endpoint overrides" >&2
  exit 2
fi

credential_file=$(mktemp "${TMPDIR:-/tmp}/mount-rs-aws-s3-credentials.XXXXXX")
fixture_file=$(mktemp "${TMPDIR:-/tmp}/mount-rs-aws-s3-restart.XXXXXX")
cleanup_required=0
cleanup_done=0

cleanup() {
  cleanup_status=0
  if [ "$cleanup_done" -eq 1 ]; then
    return 0
  fi
  cleanup_done=1

  if [ "$cleanup_required" -eq 1 ]; then
    if ! AWS_EC2_METADATA_DISABLED=true aws s3 rm \
      "s3://${AWS_S3_TEST_BUCKET}/$prefix/" \
      --recursive --region "$region" >/dev/null 2>&1; then
      echo "AWS_S3_CLEANUP_FAILED bucket=${AWS_S3_TEST_BUCKET} prefix=$prefix" >&2
      cleanup_status=1
    fi

    remaining=$(AWS_EC2_METADATA_DISABLED=true aws s3api list-objects-v2 \
      --bucket "${AWS_S3_TEST_BUCKET}" \
      --prefix "$prefix/" \
      --max-keys 1 \
      --region "$region" \
      --query 'length(Contents || `[]`)' \
      --output text 2>/dev/null) || {
        echo "AWS_S3_CLEANUP_VERIFY_FAILED bucket=${AWS_S3_TEST_BUCKET} prefix=$prefix" >&2
        cleanup_status=1
        remaining=unknown
      }
    case "$remaining" in
      0) ;;
      *)
        echo "AWS_S3_CLEANUP_INCOMPLETE bucket=${AWS_S3_TEST_BUCKET} prefix=$prefix count=$remaining" >&2
        cleanup_status=1
        ;;
    esac
  fi

  rm -f "$credential_file" "$fixture_file"
  return "$cleanup_status"
}

on_exit() {
  test_status=$?
  cleanup_status=0
  cleanup || cleanup_status=$?
  if [ "$test_status" -eq 0 ] && [ "$cleanup_status" -eq 0 ] && [ "$cleanup_required" -eq 1 ]; then
    echo "AWS_S3_TEST_PASS bucket=${AWS_S3_TEST_BUCKET} region=$region prefix=$prefix"
  fi
  if [ "$test_status" -ne 0 ]; then
    exit "$test_status"
  fi
  exit "$cleanup_status"
}

trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# If a profile is selected and the caller did not already provide credentials,
# use the AWS CLI's normal credential chain to obtain temporary credentials.
# Parse only the three expected credential variables; never print the file.
if [ -z "${AWS_ACCESS_KEY_ID:-}" ] || [ -z "${AWS_SECRET_ACCESS_KEY:-}" ]; then
  unset AWS_SESSION_TOKEN
  if [ -n "${AWS_PROFILE:-}" ]; then
    aws configure export-credentials \
      --profile "${AWS_PROFILE}" \
      --format env-no-export >"$credential_file"
  else
    aws configure export-credentials \
      --format env-no-export >"$credential_file"
  fi
  while IFS='=' read -r name value; do
    case "$name" in
      AWS_ACCESS_KEY_ID|AWS_SECRET_ACCESS_KEY|AWS_SESSION_TOKEN)
        export "$name=$value"
        ;;
      '') ;;
      *)
        echo "AWS credential export returned an unexpected variable" >&2
        exit 2
        ;;
    esac
  done <"$credential_file"
fi
rm -f "$credential_file"

: "${AWS_ACCESS_KEY_ID:?AWS credentials must provide AWS_ACCESS_KEY_ID or an active AWS profile}"
: "${AWS_SECRET_ACCESS_KEY:?AWS credentials must provide AWS_SECRET_ACCESS_KEY or an active AWS profile}"
export AWS_DEFAULT_REGION="$region"
export AWS_EC2_METADATA_DISABLED=true
export AWS_S3_TEST_REGION="$region"
export AWS_S3_TEST_PREFIX="$prefix"
export AWS_S3_RESTART_FIXTURE="$fixture_file"

AWS_EC2_METADATA_DISABLED=true aws sts get-caller-identity \
  --region "$region" >/dev/null
AWS_EC2_METADATA_DISABLED=true aws s3api head-bucket \
  --bucket "${AWS_S3_TEST_BUCKET}" \
  --region "$region" >/dev/null

existing=$(AWS_EC2_METADATA_DISABLED=true aws s3api list-objects-v2 \
  --bucket "${AWS_S3_TEST_BUCKET}" \
  --prefix "$prefix/" \
  --max-keys 1 \
  --region "$region" \
  --query 'length(Contents || `[]`)' \
  --output text)
case "$existing" in
  0) ;;
  *)
    echo "Refusing a non-empty AWS S3 test prefix: bucket=${AWS_S3_TEST_BUCKET} prefix=$prefix count=$existing" >&2
    exit 2
    ;;
esac
cleanup_required=1

echo "AWS_S3_TEST_START bucket=${AWS_S3_TEST_BUCKET} region=$region prefix=$prefix"

cargo test \
  --manifest-path "$repo_dir/tests/aws/Cargo.toml" \
  --locked \
  --lib \
  actual_aws_s3_block_and_composed_filesystem \
  -- \
  --exact --ignored --nocapture

cargo test \
  --manifest-path "$repo_dir/tests/aws/Cargo.toml" \
  --locked \
  --lib \
  actual_aws_s3_reopen_after_process_restart \
  -- \
  --exact --ignored --nocapture
