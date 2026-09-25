#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"
cd "$repo_dir"

case "${1:-cluster}" in
  cluster)
    : "${MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR:?supply a retained output directory}"
    if [ -z "${MOUNT_RS_DATASTORE_METRICS_OUTPUT_DIR:-}" ]; then
      MOUNT_RS_DATASTORE_METRICS_OUTPUT_DIR="$MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR/tidb-metrics"
    fi
    mkdir -p "$MOUNT_RS_DATASTORE_METRICS_OUTPUT_DIR"
    export MOUNT_RS_DATASTORE_METRICS_OUTPUT_DIR
    export MOUNT_RS_DATASTORE_STAGE_OBSERVER="$repo_dir/scripts/observe-tidb-stage.py"
    export MOUNT_RS_TIDB_DIRECT_BASELINE_OUTPUT="${MOUNT_RS_TIDB_DIRECT_BASELINE_OUTPUT:-$MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR/tidb-direct-baseline.json}"
    export MOUNT_RS_PROFILE_IO=1
    export MOUNT_RS_TIDB_OBSERVED_SCRIPT="$repo_dir/scripts/bench-remote-tidb-observed.sh"
    export MOUNT_RS_TIDB_COMPOSITION_COMMAND='exec sh "$MOUNT_RS_TIDB_OBSERVED_SCRIPT" existing'
    echo "TIDB_OBSERVED_OUTPUT_DIR $MOUNT_RS_DATASTORE_METRICS_OUTPUT_DIR" >&2
    exec sh "$repo_dir/scripts/test-tidb.sh"
    ;;
  existing)
    : "${MOUNT_RS_TIDB_URL:?owned disposable TiDB URL required}"
    : "${MOUNT_RS_TIDB_RUN_ID:?owned test-tidb.sh run identity required}"
    : "${MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR:?supply a retained output directory}"
    : "${MOUNT_RS_DATASTORE_STAGE_OBSERVER:?observer required}"
    : "${MOUNT_RS_DATASTORE_METRICS_OUTPUT_DIR:?retained metrics output directory required}"
    mkdir -p "$MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR" "$MOUNT_RS_DATASTORE_METRICS_OUTPUT_DIR"
    "$repo_dir/scripts/cargo-shared" test --release --offline --locked -p mount-rs-tidb \
      --test direct_io_baseline -- --ignored --nocapture --test-threads=1
    export MOUNT_RS_REMOTE_COMPARISON_EXISTING_TIDB=1
    exec sh "$repo_dir/scripts/bench-remote-providers.sh" tidb
    ;;
  *)
    echo "usage: $0 [cluster|existing]" >&2
    exit 2
    ;;
esac
