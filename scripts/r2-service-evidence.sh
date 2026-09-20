#!/bin/sh
set -eu

# Print only non-secret service identity and revision evidence, then perform a
# read-only bucket probe. Credentials must be supplied by the caller's
# environment (for example, after a local Keychain lookup); they are never
# echoed or written by this script.
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

: "${R2_ENDPOINT:?R2_ENDPOINT must be exported for the R2 service-evidence gate}"
: "${R2_BUCKET:?R2_BUCKET must be exported for the R2 service-evidence gate}"
: "${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID must be exported for the R2 service-evidence gate}"
: "${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY must be exported for the R2 service-evidence gate}"

endpoint=${R2_ENDPOINT%/}
case "$endpoint" in
  https://*) authority=${endpoint#https://} ;;
  http://*) authority=${endpoint#http://} ;;
  *)
    echo "R2 service evidence requires an http:// or https:// endpoint" >&2
    exit 2
    ;;
esac
case "$authority" in
  ""|*/*|*\?*|*\#*|*@*)
    echo "R2 service evidence endpoint contains an unsafe authority" >&2
    exit 2
    ;;
esac
authority=${authority%%/*}
case "$authority" in
  ""|*@*)
    echo "R2 service evidence endpoint has no credential-free authority" >&2
    exit 2
    ;;
esac

if ! command -v aws >/dev/null 2>&1; then
  echo "R2 service evidence requires the AWS CLI for a read-only bucket probe" >&2
  exit 2
fi

revision=$(git -C "$repo_dir" rev-parse HEAD)
dirty_entries=$(git -C "$repo_dir" status --porcelain | wc -l | tr -d ' ')

if ! AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
  AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
  AWS_DEFAULT_REGION=auto \
  AWS_EC2_METADATA_DISABLED=true \
    aws s3api head-bucket \
      --bucket "$R2_BUCKET" \
      --endpoint-url "$endpoint" \
      >/dev/null 2>&1; then
  echo "R2 service evidence bucket probe failed" >&2
  exit 1
fi

echo "R2_SERVICE=cloudflare-r2"
echo "R2_ENDPOINT_AUTHORITY=$authority"
echo "R2_BUCKET=$R2_BUCKET"
echo "R2_BUCKET_PROBE=head-bucket-pass"
echo "MOUNT_RS_REVISION=$revision"
echo "MOUNT_RS_WORKTREE_DIRTY_ENTRIES=$dirty_entries"
