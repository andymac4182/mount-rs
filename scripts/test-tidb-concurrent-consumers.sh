#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"
cd "$repo_dir"
: "${MOUNT_RS_TIDB_URL:?supply an actual disposable TiDB endpoint}"

"$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-tidb \
  --test concurrent --test concurrent_load -- --ignored --nocapture --test-threads=1

if [ "${MOUNT_RS_TIDB_NAPI:-0}" = 1 ]; then
  node "$repo_dir/bindings/mount-rs-napi/test/tidb-concurrent.mjs"
fi

if [ "${MOUNT_RS_CLI_NATIVE_TIDB_TWO_PROCESS:-0}" = 1 ]; then
  if [ "$(uname -s)" != Darwin ] || [ "${MOUNT_RS_CLI_NATIVE_NFS:-0}" != 1 ]; then
    echo "TiDB native acceptance requires macOS and MOUNT_RS_CLI_NATIVE_NFS=1" >&2
    exit 2
  fi
  set --
  if [ "${MOUNT_RS_CLI_NATIVE_RUSTFS_DISPOSABLE:-0}" != 1 ]; then
    set -- --skip cli_two_process_tidb_metadata_rustfs_blocks_stays_coherent_under_load_and_reopens
  fi
  "$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-cli \
    --test native_two_process_tidb -- --ignored --nocapture --test-threads=1 "$@"
fi
