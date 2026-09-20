#!/bin/sh
set -eu

# Run the configuration-driven HTTP CLI gate against the real Cloudflare R2
# S3 endpoint. This is intentionally separate from scripts/test-rustfs.sh:
# RustFS remains the deterministic local S3-compatible lane, while this gate
# proves the same CLI path against Cloudflare's hosted service.
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if ! command -v cargo >/dev/null 2>&1 || ! command -v node >/dev/null 2>&1; then
  echo "Cloudflare R2 CLI test requires cargo and node" >&2
  exit 2
fi
if ! command -v aws >/dev/null 2>&1; then
  echo "Cloudflare R2 CLI test requires the AWS CLI for prefix-scoped cleanup" >&2
  exit 2
fi

: "${R2_ENDPOINT:?R2_ENDPOINT must be exported for the Cloudflare R2 gate}"
: "${R2_BUCKET:?R2_BUCKET must be exported for the Cloudflare R2 gate}"
: "${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID must be exported for the Cloudflare R2 gate}"
: "${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY must be exported for the Cloudflare R2 gate}"

endpoint_without_slash=${R2_ENDPOINT%/}
endpoint_host=${endpoint_without_slash#https://}
case "$R2_ENDPOINT" in
  https://*) ;;
  *)
    echo "Cloudflare R2 CLI test requires an https://<account>.r2.cloudflarestorage.com endpoint" >&2
    exit 2
    ;;
esac
case "$endpoint_host" in
  ""|*/*|*\?*|*\#*|*:*)
    echo "Cloudflare R2 CLI test endpoint must not contain a path, query, fragment, or port" >&2
    exit 2
    ;;
esac
case "$endpoint_host" in
  *.r2.cloudflarestorage.com) ;;
  *)
    echo "Cloudflare R2 CLI test requires an https://<account>.r2.cloudflarestorage.com endpoint" >&2
    exit 2
    ;;
esac

prefix=${MOUNT_RS_CLI_REMOTE_PREFIX:-mount-rs/cli-cloudflare/$(date -u +%Y%m%dT%H%M%SZ)-$$}
case "$prefix" in
  mount-rs/cli-cloudflare/*) ;;
  *)
    echo "MOUNT_RS_CLI_REMOTE_PREFIX contains an unsafe path" >&2
    exit 2
    ;;
esac
case "$prefix" in
  *//*|*/../*|*/..|*/./*|*/.)
    echo "MOUNT_RS_CLI_REMOTE_PREFIX contains an unsafe path" >&2
    exit 2
    ;;
esac
export MOUNT_RS_CLI_REMOTE_PREFIX="$prefix"

port=$(node -e 'const net=require("net"); const s=net.createServer(); s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
pglite_log=$(mktemp "${TMPDIR:-/tmp}/mount-rs-pglite-cli-r2.XXXXXX")
output_file=$(mktemp "${TMPDIR:-/tmp}/mount-rs-cli-cloudflare-r2.XXXXXX")
pglite_data_dir=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-pglite-cli-r2-data.XXXXXX")
server_pid=""
cleanup_done=0

cleanup_r2() {
  AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
  AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
  AWS_DEFAULT_REGION=auto \
  AWS_EC2_METADATA_DISABLED=true \
    aws s3 rm "s3://$R2_BUCKET/$MOUNT_RS_CLI_REMOTE_PREFIX/" \
      --recursive --endpoint-url "$endpoint_without_slash" >/dev/null 2>&1 || {
        echo "CLOUDFLARE_R2_CLEANUP_FAILED prefix=$MOUNT_RS_CLI_REMOTE_PREFIX" >&2
        return 1
      }
  # Cloudflare R2 may omit AWS's optional KeyCount field while still
  # returning Contents. Count the returned objects instead of treating
  # a missing KeyCount as an empty prefix.
  remaining=$(AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
    AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
    AWS_DEFAULT_REGION=auto \
    AWS_EC2_METADATA_DISABLED=true \
      aws s3api list-objects-v2 \
        --bucket "$R2_BUCKET" \
        --prefix "$MOUNT_RS_CLI_REMOTE_PREFIX/" \
        --endpoint-url "$endpoint_without_slash" \
        --query 'length(Contents || `[]`)' --output text) || {
          echo "CLOUDFLARE_R2_CLEANUP_VERIFY_FAILED prefix=$MOUNT_RS_CLI_REMOTE_PREFIX" >&2
          return 1
        }
  case "$remaining" in
    0) ;;
    *)
      echo "CLOUDFLARE_R2_CLEANUP_INCOMPLETE prefix=$MOUNT_RS_CLI_REMOTE_PREFIX count=$remaining" >&2
      return 1
      ;;
  esac
}

stop_server() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
    server_pid=""
  fi
}

cleanup() {
  status=$?
  if [ "$cleanup_done" -eq 0 ]; then
    cleanup_done=1
    cleanup_r2 || status=1
    stop_server
    rm -f "$pglite_log" "$output_file"
    rm -rf "$pglite_data_dir"
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

PGLITE_PORT="$port" PGLITE_DATA_DIR="$pglite_data_dir" PGLITE_MAX_CONNECTIONS=8 \
  node "$repo_dir/tests/pglite/server.mjs" >"$pglite_log" 2>&1 &
server_pid=$!
ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60; do
  if grep -q PGLITE_READY "$pglite_log"; then ready=1; break; fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  sed -n '1,160p' "$pglite_log"
  echo "Cloudflare R2 CLI test PGlite server did not become ready" >&2
  exit 1
fi

export PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable"
export MOUNT_RS_CLI_REMOTE_ALLOW_EXTERNAL_ENDPOINT=1

echo "CLOUDFLARE_R2_CLI_START prefix=$MOUNT_RS_CLI_REMOTE_PREFIX"
if cargo test \
  --manifest-path "$repo_dir/Cargo.toml" \
  --locked \
  -p mount-rs-cli \
  --test http_remote \
  -- \
  remote_cli_http_durable_reopen_after_graceful_shutdown --exact --ignored --nocapture >"$output_file" 2>&1; then
  test_status=0
else
  test_status=$?
fi
cat "$output_file"
if [ "$test_status" -ne 0 ]; then
  echo "Cloudflare R2 CLI test failed" >&2
  exit "$test_status"
fi
if ! grep -Fq "test remote_cli_http_durable_reopen_after_graceful_shutdown ... ok" "$output_file"; then
  echo "Cloudflare R2 CLI invocation did not execute the expected test" >&2
  exit 1
fi

for block_prefix in pglite-r2-blocks sqlite-r2-blocks; do
  object_count=$(AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
    AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
    AWS_DEFAULT_REGION=auto \
    AWS_EC2_METADATA_DISABLED=true \
      aws s3api list-objects-v2 \
        --bucket "$R2_BUCKET" \
        --prefix "$MOUNT_RS_CLI_REMOTE_PREFIX/$block_prefix/" \
        --endpoint-url "$endpoint_without_slash" \
        --query 'length(Contents || `[]`)' --output text)
  case "$object_count" in
    ''|0)
      echo "Cloudflare R2 CLI test found no objects under $block_prefix" >&2
      exit 1
      ;;
  esac
done
echo "CLOUDFLARE_R2_CLI_PASS prefix=$MOUNT_RS_CLI_REMOTE_PREFIX"
