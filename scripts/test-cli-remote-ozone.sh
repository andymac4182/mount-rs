#!/bin/sh
set -eu

# Run the shipped HTTP CLI server against the real Apache Ozone gateway. The
# Ozone harness owns the gateway and supplies one unique object prefix; this
# runner owns only the HTTP test process and verifies that its prefix is empty
# before returning. It never prints credential values.
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"

for command_name in cargo grep mktemp node rm; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Ozone HTTP CLI test requires $command_name" >&2
    exit 2
  fi
done

: "${PGLITE_DATABASE_URL:?PGLITE_DATABASE_URL must be exported by the Ozone composition harness}"
: "${R2_ENDPOINT:?R2_ENDPOINT must be exported by the Ozone harness}"
: "${R2_BUCKET:?R2_BUCKET must be exported by the Ozone harness}"
: "${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID must be exported by the Ozone harness}"
: "${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY must be exported by the Ozone harness}"
: "${OZONE_TEST_PREFIX:?OZONE_TEST_PREFIX must be exported by the Ozone harness}"

case "$R2_ENDPOINT" in
  http://127.0.0.1:*|http://localhost:*) ;;
  *)
    echo "Ozone HTTP CLI test requires the loopback Ozone test endpoint" >&2
    exit 2
    ;;
esac

export MOUNT_RS_CLI_REMOTE_PREFIX="${MOUNT_RS_CLI_REMOTE_PREFIX:-${OZONE_TEST_PREFIX%/}/cli-http}"
case "$MOUNT_RS_CLI_REMOTE_PREFIX" in
  "$OZONE_TEST_PREFIX"/*) ;;
  *)
    echo "MOUNT_RS_CLI_REMOTE_PREFIX must remain below OZONE_TEST_PREFIX" >&2
    exit 2
    ;;
esac
case "$MOUNT_RS_CLI_REMOTE_PREFIX" in
  *//*|*/../*|*/..|*/./*|*/.)
    echo "MOUNT_RS_CLI_REMOTE_PREFIX contains an unsafe path" >&2
    exit 2
    ;;
esac

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

echo "OZONE_CLI_REMOTE_HTTP_START mode=$test_mode prefix=$MOUNT_RS_CLI_REMOTE_PREFIX"
output_file=$(mktemp "${TMPDIR:-/tmp}/mount-rs-ozone-cli-remote.XXXXXX")
cleanup() {
  exit_code=$1
  trap - EXIT INT TERM
  if node "$repo_dir/tests/ozone/cleanup-prefix.mjs"; then
    cleanup_status=0
  else
    cleanup_status=$?
  fi
  rm -f "$output_file"
  if [ "$cleanup_status" -ne 0 ] && [ "$exit_code" -eq 0 ]; then
    exit_code=$cleanup_status
  fi
  if [ "$cleanup_status" -ne 0 ]; then
    echo "Ozone HTTP CLI object-prefix cleanup failed: $MOUNT_RS_CLI_REMOTE_PREFIX" >&2
  fi
  exit "$exit_code"
}
trap 'cleanup "$?"' EXIT INT TERM

if "$repo_dir/scripts/cargo-shared" test \
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
if [ "$test_status" -eq 0 ] && ! grep -Fq "test $test_name ... ok" "$output_file"; then
  echo "Ozone HTTP CLI invocation did not execute the expected test: $test_name" >&2
  test_status=1
fi
if [ "$test_status" -ne 0 ]; then
  echo "Ozone HTTP CLI test failed: $test_name" >&2
  exit "$test_status"
fi
echo "OZONE_CLI_REMOTE_HTTP_PASS mode=$test_mode prefix=$MOUNT_RS_CLI_REMOTE_PREFIX"
