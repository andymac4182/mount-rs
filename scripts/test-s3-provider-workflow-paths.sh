#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
expected="      - 'transports/mount-rs-s3/**'"

for workflow in aws-s3.yml cloudflare-r2.yml; do
  path="$repo_dir/.github/workflows/$workflow"
  if ! grep -Fqx -- "$expected" "$path"; then
    echo "S3_PROVIDER_WORKFLOW_PATHS_FAILED workflow=$workflow" >&2
    exit 1
  fi
done

echo "S3_PROVIDER_WORKFLOW_PATHS_PASS workflows=2"
