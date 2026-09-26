#!/bin/sh

# Select the adapter for the real external block fixture before starting the
# FoundationDB gate. An unqualified caller keeps the canonical R2 default.
validate_foundationdb_napi_fixture_provider() {
  fixture_kind=$1
  case "$fixture_kind" in
    generic) fixture_default=r2 ;;
    rustfs|ozone) fixture_default=rustfs ;;
    *) echo "Unknown FoundationDB N-API fixture: $fixture_kind" >&2; return 2 ;;
  esac
  fixture_provider=${MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER:-$fixture_default}
  case "$fixture_provider" in
    r2|rustfs) ;;
    *) echo "MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER must be r2 or rustfs" >&2; return 2 ;;
  esac
  if [ "$fixture_kind" = rustfs ] && [ "$fixture_provider" != rustfs ]; then
    echo "RustFS FoundationDB N-API fixture requires the qualified signed S3 adapter" >&2
    return 2
  fi
  if [ "$fixture_kind" = ozone ] && [ "$fixture_provider" != rustfs ]; then
    echo "Ozone FoundationDB N-API fixture requires the qualified signed S3 adapter" >&2
    return 2
  fi
}

configure_foundationdb_napi_fixture() {
  validate_foundationdb_napi_fixture_provider "$1" || return $?
  if [ "$fixture_kind" = rustfs ] && {
    [ -z "${RUSTFS_ENDPOINT:-}" ] || [ -z "${RUSTFS_BUCKET:-}" ] ||
    [ -z "${RUSTFS_ACCESS_KEY_ID:-}" ] || [ -z "${RUSTFS_SECRET_ACCESS_KEY:-}" ];
  }; then
    echo "RustFS FoundationDB N-API fixture requires endpoint, bucket, and credentials" >&2
    return 2
  fi
  if [ "$fixture_kind" = ozone ]; then
    RUSTFS_REGION=${2:-}
    export RUSTFS_REGION
  fi
  if [ "$fixture_provider" = rustfs ] && [ -z "${RUSTFS_REGION:-}" ]; then
    echo "RUSTFS_REGION is required for the signed S3 N-API fixture" >&2
    return 2
  fi
  if [ "$fixture_provider" = rustfs ] && [ "$fixture_kind" != rustfs ] && {
    [ -z "${R2_ENDPOINT:-}" ] || [ -z "${R2_BUCKET:-}" ] ||
    [ -z "${R2_ACCESS_KEY_ID:-}" ] || [ -z "${R2_SECRET_ACCESS_KEY:-}" ];
  }; then
    echo "Signed S3 FoundationDB N-API fixture requires R2 endpoint, bucket, and credentials" >&2
    return 2
  fi
  MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER=$fixture_provider
  export MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER
  foundationdb_napi_fixture_kind=$fixture_kind
}

# Both Node Docker branches use this boundary so the selected provider and
# region reach the helper that runs the actual full-payload inode oracle.
foundationdb_napi_run_client() {
  fixture_endpoint=$1
  shift
  (
    if [ "$foundationdb_napi_fixture_kind" = rustfs ]; then
      R2_BUCKET=$RUSTFS_BUCKET
      R2_ACCESS_KEY_ID=$RUSTFS_ACCESS_KEY_ID
      R2_SECRET_ACCESS_KEY=$RUSTFS_SECRET_ACCESS_KEY
    else
      R2_BUCKET=${R2_BUCKET:-}
      R2_ACCESS_KEY_ID=${R2_ACCESS_KEY_ID:-}
      R2_SECRET_ACCESS_KEY=${R2_SECRET_ACCESS_KEY:-}
    fi
    export R2_BUCKET R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY
    docker run --rm \
      --env "MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER=$MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER" \
      --env "RUSTFS_REGION=${RUSTFS_REGION:-}" \
      --env "R2_ENDPOINT=$fixture_endpoint" \
      --env R2_BUCKET \
      --env R2_ACCESS_KEY_ID \
      --env R2_SECRET_ACCESS_KEY \
      "$@"
  )
}
