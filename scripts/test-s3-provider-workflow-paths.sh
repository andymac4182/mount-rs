#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

check_path() {
  workflow=$1
  pattern=$2
  path="$repo_dir/.github/workflows/$workflow"
  expected="      - '$pattern'"
  if ! grep -Fqx -- "$expected" "$path"; then
    echo "S3_PROVIDER_WORKFLOW_PATHS_FAILED workflow=$workflow pattern=$pattern" >&2
    exit 1
  fi
}

for pattern in \
  'transports/mount-rs-s3/**' \
  'apps/mount-rs-cli/**' \
  'src/**' \
  'providers/mount-rs-aws-s3/**' \
  'providers/mount-rs-object-store-blocks/**' \
  'providers/mount-rs-r2/**' \
  'filesystems/mount-rs-chunked/**' \
  'filesystems/mount-rs-r2-fs/**' \
  'filesystems/mount-rs-pglite-fs/**' \
  'filesystems/mount-rs-sqlite-fs/**'; do
  check_path aws-s3.yml "$pattern"
done

for pattern in \
  'transports/mount-rs-s3/**' \
  'apps/mount-rs-cli/**' \
  'bindings/**' \
  'filesystems/**' \
  'providers/**' \
  'src/**'; do
  check_path cloudflare-r2.yml "$pattern"
done

echo "S3_PROVIDER_WORKFLOW_PATHS_PASS workflows=2"
