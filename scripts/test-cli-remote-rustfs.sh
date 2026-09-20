#!/bin/sh
set -eu

# Run the real CLI HTTP/provider gate after scripts/test-rustfs.sh has started
# its test-owned PGlite server and RustFS endpoint. This script deliberately
# accepts only the harness's loopback RustFS endpoint and never prints any
# credential value.
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"

if ! command -v cargo >/dev/null 2>&1; then
  echo "CLI remote test requires cargo" >&2
  exit 2
fi

: "${PGLITE_DATABASE_URL:?PGLITE_DATABASE_URL must be exported by the remote harness}"
: "${R2_ENDPOINT:?R2_ENDPOINT must be exported by the remote harness}"
: "${R2_BUCKET:?R2_BUCKET must be exported by the remote harness}"
: "${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID must be exported by the remote harness}"
: "${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY must be exported by the remote harness}"
: "${RUSTFS_TEST_PREFIX:?RUSTFS_TEST_PREFIX must be exported by scripts/test-rustfs.sh}"

case "$R2_ENDPOINT" in
  http://127.0.0.1:*|http://localhost:*) ;;
  *)
    echo "CLI remote test requires a loopback RustFS R2_ENDPOINT" >&2
    exit 2
    ;;
esac

# The standard RustFS harness gives each run a unique scoped prefix and owns
# cleanup for everything below it. Keep the CLI's default namespace inside
# that scope; callers can override it when composing a larger harness.
export MOUNT_RS_CLI_REMOTE_PREFIX="${MOUNT_RS_CLI_REMOTE_PREFIX:-${RUSTFS_TEST_PREFIX%/}/cli-remote}"

case "$(uname -s 2>/dev/null || printf unknown)" in
  Darwin|Linux|FreeBSD|NetBSD|OpenBSD)
    test_name=remote_cli_http_durable_reopen_after_graceful_shutdown
    test_mode=unix-graceful-reopen
    ;;
  *)
    test_name=remote_cli_http_roundtrip_forced_cleanup
    test_mode=portable-forced-cleanup
    ;;
esac

echo "CLI_REMOTE_HTTP_START mode=$test_mode"
output_file=$(mktemp "${TMPDIR:-/tmp}/mount-rs-cli-remote.XXXXXX")
cleanup() {
  rm -f "$output_file"
}
trap cleanup EXIT INT TERM

if cargo test \
  --manifest-path "$repo_dir/Cargo.toml" \
  --locked \
  -p mount-rs-cli \
  --test http_remote \
  -- \
  "$test_name" --exact --ignored --nocapture >"$output_file" 2>&1; then
  test_status=0
else
  test_status=$?
fi
cat "$output_file"
if [ "$test_status" -ne 0 ]; then
  echo "CLI remote HTTP test failed: $test_name" >&2
  exit "$test_status"
fi
if ! grep -Fq "test $test_name ... ok" "$output_file"; then
  echo "CLI remote HTTP invocation did not execute the expected test: $test_name" >&2
  exit 1
fi
echo "CLI_REMOTE_HTTP_PASS mode=$test_mode"
