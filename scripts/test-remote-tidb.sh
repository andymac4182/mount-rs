#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"
cd "$repo_dir"

case "${1:-cluster}" in
  existing)
    : "${MOUNT_RS_TIDB_URL:?supply an actual disposable TiDB endpoint}"
    exec "$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-service \
      --test quic_tidb_load -- --ignored --nocapture --test-threads=1
    ;;
  cluster)
    # Reuse the owned, pinned PD/TiKV/TiDB topology and its cleanup/restart
    # checks. The endpoint remains in the child environment, never command text.
    export MOUNT_RS_REMOTE_TIDB_CONSUMER_SCRIPT="$repo_dir/scripts/test-remote-tidb.sh"
    export MOUNT_RS_TIDB_COMPOSITION_COMMAND='exec sh "$MOUNT_RS_REMOTE_TIDB_CONSUMER_SCRIPT" existing'
    exec sh "$repo_dir/scripts/test-tidb.sh"
    ;;
  *)
    echo "usage: $0 [cluster|existing]" >&2
    exit 2
    ;;
esac
