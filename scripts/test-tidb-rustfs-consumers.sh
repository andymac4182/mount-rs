#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"

for command_name in node; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "test-tidb-rustfs-consumers.sh: $command_name is required" >&2
    exit 2
  fi
done
: "${MOUNT_RS_TIDB_URL:?MOUNT_RS_TIDB_URL must be supplied by test-tidb.sh}"
: "${R2_ENDPOINT:?R2_ENDPOINT must be supplied by the RustFS harness}"
: "${R2_BUCKET:?R2_BUCKET must be supplied by the RustFS harness}"
: "${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID must be supplied by the RustFS harness}"
: "${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY must be supplied by the RustFS harness}"
: "${RUSTFS_ENDPOINT:?RUSTFS_ENDPOINT must be supplied by the RustFS harness}"
: "${RUSTFS_BUCKET:?RUSTFS_BUCKET must be supplied by the RustFS harness}"
: "${RUSTFS_REGION:?RUSTFS_REGION must be supplied by the RustFS harness}"
: "${RUSTFS_ACCESS_KEY_ID:?RUSTFS_ACCESS_KEY_ID must be supplied by the RustFS harness}"
: "${RUSTFS_SECRET_ACCESS_KEY:?RUSTFS_SECRET_ACCESS_KEY must be supplied by the RustFS harness}"

base_run_id=${MOUNT_RS_TIDB_CONSUMER_RUN_ID:-tidb-rustfs-$(date +%s)-$$}
case "${MOUNT_RS_TIDB_EXPECT_PERSISTED:-0}" in
  0) phase_run_id="$base_run_id-seed" ;;
  1) phase_run_id="$base_run_id-reopen" ;;
  *)
    echo "test-tidb-rustfs-consumers.sh: MOUNT_RS_TIDB_EXPECT_PERSISTED must be 0 or 1" >&2
    exit 2
    ;;
esac
export MOUNT_RS_PROVIDER_MATRIX_RUN_ID="$phase_run_id"

"$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-tidb \
  --test chunked_rustfs -- --ignored --nocapture
if [ "${MOUNT_RS_TIDB_EXPECT_PERSISTED:-0}" = 0 ]; then
  # A RustFS block put that succeeds and then loses its reply. The test owns
  # its own prefix and volume key, so the seed phase alone covers it.
  "$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-tidb \
    --test lost_block_ack_rustfs -- --ignored --nocapture
fi

# Host a real SQLite database file on a FUSE mount whose metadata is this
# TiDB, with TiDB blocks and then with RustFS blocks. The test is Linux-only
# and fails, instead of skipping, when /dev/fuse or fusermount3 is missing.
# It runs before and after the TiDB component restarts.
if [ "$(uname -s)" = Linux ]; then
  MOUNT_RS_RUN_NATIVE_TIDB_SQLITE=1 \
    "$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-core \
      --test native_tidb_sqlite -- --ignored --nocapture --test-threads=1
else
  echo "TIDB_SQLITE_HOSTING_SKIP reason=linux-fuse-required host=$(uname -s)" >&2
fi

node "$repo_dir/tests/provider_matrix/node-sdk.mjs"
node "$repo_dir/tests/provider_matrix/cli.mjs"
node "$repo_dir/tests/provider_matrix/tidb-rustfs-soak.mjs"

MOUNT_RS_NODE_CLI_NATIVE_INTEGRATION=1 \
MOUNT_RS_NODE_CLI_TIDB_RUSTFS_NATIVE=1 \
MOUNT_RS_NODE_CLI_NATIVE_REQUIRED=1 \
  node "$repo_dir/examples/node-cli/native-integration.mjs"
