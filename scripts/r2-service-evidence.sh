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
if ! dirty_status=$(git -C "$repo_dir" status --porcelain); then
  printf '%s\n' "R2 service evidence could not read repository status" >&2
  exit 1
fi
if [ -n "$dirty_status" ]; then
  dirty_entries=$(printf '%s\n' "$dirty_status" | wc -l | tr -d ' ')
else
  dirty_entries=0
fi

if ! AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
  AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
  AWS_DEFAULT_REGION=auto \
  AWS_EC2_METADATA_DISABLED=true \
    aws s3api head-bucket \
      --bucket "$R2_BUCKET" \
      --endpoint-url "$endpoint" \
      >/dev/null 2>&1; then
  printf '%s\n' "R2 service evidence bucket probe failed" >&2
  exit 1
fi

printf 'R2_SERVICE=%s\n' 'cloudflare-r2'
printf 'R2_ENDPOINT_AUTHORITY=%s\n' "$authority"
printf 'R2_BUCKET=%s\n' "$R2_BUCKET"
printf 'R2_BUCKET_PROBE=%s\n' 'head-bucket-pass'
printf 'MOUNT_RS_REVISION=%s\n' "$revision"
printf 'MOUNT_RS_WORKTREE_DIRTY_ENTRIES=%s\n' "$dirty_entries"
