#!/bin/sh
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
. "$repo_dir/scripts/cargo-shared-env.sh"
provider=${1:-sqlite}
case "$provider" in sqlite|tidb) ;; *) echo 'usage: bench-remote-resources.sh [sqlite|tidb]' >&2; exit 2 ;; esac
: "${MOUNT_RS_RESOURCE_OUTPUT_DIR:?retained output directory required}"
mkdir -p "$MOUNT_RS_RESOURCE_OUTPUT_DIR"
export MOUNT_RS_PROFILE_IO=1
export MOUNT_RS_REMOTE_SATURATION_PROVIDER="$provider"
export MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS="${MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS:-32}"
export MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS="${MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS:-15}"
export MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS="${MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS:-3}"
export MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=0
export MOUNT_RS_REMOTE_TIDB_SATURATION_MODES="${MOUNT_RS_REMOTE_TIDB_SATURATION_MODES:-read,write}"
export MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS="${MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS:-1}"
export MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT="$MOUNT_RS_RESOURCE_OUTPUT_DIR/$provider.json"
if [ "$provider" = tidb ] && [ -z "${MOUNT_RS_TIDB_URL:-}" ]; then
  export MOUNT_RS_DATASTORE_STAGE_OBSERVER="$repo_dir/scripts/observe-tidb-stage.py"
  export MOUNT_RS_DATASTORE_METRICS_OUTPUT_DIR="$MOUNT_RS_RESOURCE_OUTPUT_DIR/datastore"
  export MOUNT_RS_TIDB_RESOURCE_CONSUMER="$repo_dir/scripts/bench-remote-resources.sh"
  export MOUNT_RS_TIDB_COMPOSITION_COMMAND='exec sh "$MOUNT_RS_TIDB_RESOURCE_CONSUMER" tidb'
  exec sh "$repo_dir/scripts/test-tidb.sh"
fi
feature=resource-profiling
if [ "${MOUNT_RS_TRACE_ALLOCATIONS:-0}" = 1 ]; then feature=allocation-profiling; fi
exec "$repo_dir/scripts/cargo-shared" test --release --offline --locked \
  --features "$feature" -p mount-rs-service --test quic_tidb_saturation \
  actual_tidb_100_clients_10_servers_saturation -- --ignored --nocapture --test-threads=1
