#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
: "${MOUNT_RS_TIDB_URL:?owned TiDB harness endpoint required}"
export MOUNT_RS_PROFILE_IO=1
export MOUNT_RS_REMOTE_SATURATION_PROVIDER=tidb
export MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS=32
export MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS=3
export MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=0

: "${MOUNT_RS_TIDB_INDEX_GREEN_OUTPUT_DIR:?retained output directory required}"
mkdir -p "$MOUNT_RS_TIDB_INDEX_GREEN_OUTPUT_DIR"

./scripts/cargo-shared test --release --offline --locked -p mount-rs-tidb \
  --lib actual_tidb_unchanged_revision_uses_covering_index -- --ignored --nocapture

MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=read \
MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=1 \
MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=15 \
MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT="$MOUNT_RS_TIDB_INDEX_GREEN_OUTPUT_DIR/read.json" \
  ./scripts/cargo-shared test --release --offline --locked --features io-profiling \
    -p mount-rs-service --test quic_tidb_saturation \
    actual_tidb_100_clients_10_servers_saturation -- --ignored --nocapture --test-threads=1
