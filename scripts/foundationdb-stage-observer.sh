#!/bin/sh
# Called outside timed work; status probes add background logical read traffic.
set -eu
phase=${1:?phase required}
mode=${2:?mode required}
queue_depth=${3:?queue depth required}
stage_id=${4:?stage ID required}
successes=${5:?success count required}
failures=${6:?failure count required}
case "$phase" in begin|end) ;; *) exit 2 ;; esac
case "$stage_id" in ''|*[!a-zA-Z0-9_-]*) echo 'invalid stage ID' >&2; exit 2 ;; esac
: "${MOUNT_RS_FOUNDATIONDB_COUNTER_DIR:?counter directory required}"
: "${MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE:?owned cluster file required}"
mkdir -p "$MOUNT_RS_FOUNDATIONDB_COUNTER_DIR"
base="$MOUNT_RS_FOUNDATIONDB_COUNTER_DIR/$stage_id.$phase"
printf '%s\n' "$mode $queue_depth $successes $failures" > "$base.stage-info"
date +%s%N > "$base.start-ns"
/fdb/fdbcli -C "$MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE" --exec 'status json' > "$base.status.json"
cat /proc/diskstats > "$base.diskstats"
parent_exe=$(readlink "/proc/$PPID/exe" || true)
case "$parent_exe" in
  /tmp/mount-rs-foundationdb-target/*/deps/quic_tidb_saturation-*|/tmp/mount-rs-foundationdb-target/*/deps/block_datastore_saturation-*)
    cat "/proc/$PPID/io" > "$base.client-io"
    printf '%s\n' 'owned-native-integration-test' > "$base.client-io-scope"
    ;;
  *) printf '%s\n' 'unavailable-unrecognized-parent' > "$base.client-io-scope" ;;
esac
date +%s%N > "$base.finish-ns"
