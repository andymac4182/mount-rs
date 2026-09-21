#!/bin/sh
set -eu

# Run the real TiDB/PD/TiKV topology while the existing RustFS harness owns a
# loopback S3-compatible block service. The TiDB harness performs the direct
# provider contract and invokes the ChunkedFs composition twice: once before
# the TiDB component restart and once after it, using a scoped fixture.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"

temp_root=${TMPDIR:-/tmp}
if [ "$temp_root" != "/" ]; then
  temp_root=${temp_root%/}
fi
run_token=$(date +%s)-$$

topology=${MOUNT_RS_TIDB_TOPOLOGY:-durable}
case "$topology" in
  durable|single) ;;
  *)
    echo "test-tidb-rustfs.sh: MOUNT_RS_TIDB_TOPOLOGY must be durable or single" >&2
    exit 2
    ;;
esac

reopen_after_seed=${MOUNT_RS_TIDB_CHUNKED_RUSTFS_REOPEN:-0}
case "$reopen_after_seed" in
  0|1) ;;
  *)
    echo "test-tidb-rustfs.sh: MOUNT_RS_TIDB_CHUNKED_RUSTFS_REOPEN must be 0 or 1" >&2
    exit 2
    ;;
esac
if [ "$topology" = durable ] && [ "$reopen_after_seed" = 1 ]; then
  echo "test-tidb-rustfs.sh: durable topology needs REOPEN=0 so the fixture survives the TiDB restart phase" >&2
  exit 2
fi

fixture=${MOUNT_RS_TIDB_CHUNKED_RUSTFS_FIXTURE:-$temp_root/mount-rs-tidb-rustfs-$run_token.fixture}
if [ -L "$fixture" ]; then
  echo "test-tidb-rustfs.sh: refusing a symlink fixture path: $fixture" >&2
  exit 2
fi

export MOUNT_RS_TIDB_TOPOLOGY="$topology"
export MOUNT_RS_TIDB_CHUNKED_RUSTFS=1
export MOUNT_RS_TIDB_CHUNKED_RUSTFS_REOPEN="$reopen_after_seed"
export MOUNT_RS_TIDB_RUSTFS_PREFIX="${MOUNT_RS_TIDB_RUSTFS_PREFIX:-mount-rs-tidb-rustfs/$run_token}"
export MOUNT_RS_TIDB_CHUNKED_VOLUME_KEY="${MOUNT_RS_TIDB_CHUNKED_VOLUME_KEY:-mount-rs-tidb-rustfs-$run_token}"
export MOUNT_RS_TIDB_CHUNKED_RUSTFS_FIXTURE="$fixture"
export MOUNT_RS_TIDB_COMPOSITION_COMMAND="${MOUNT_RS_TIDB_COMPOSITION_COMMAND:-$repo_dir/scripts/cargo-shared test --locked -p mount-rs-tidb --test chunked_rustfs -- --ignored --nocapture}"
export RUSTFS_COMBO_NAME="${RUSTFS_COMBO_NAME:-tidb-metadata-rustfs}"
export RUSTFS_COMBO_TIMEOUT_SECONDS="${RUSTFS_COMBO_TIMEOUT_SECONDS:-1800}"
export RUSTFS_COMBO_COMMAND="${RUSTFS_COMBO_COMMAND:-sh $repo_dir/scripts/test-tidb.sh}"

exec sh "$repo_dir/scripts/test-rustfs.sh"
