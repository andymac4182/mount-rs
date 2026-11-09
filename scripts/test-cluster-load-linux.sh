#!/bin/sh
set -eu

# Native ARM64 Linux transport comparison, using an already-built Linux example.
# This is a Docker VM / loopback diagnostic, not multi-host acceptance.
: "${MOUNT_RS_LINUX_TARGET_DIR:?set the dedicated Linux Cargo target directory}"
: "${MOUNT_RS_LINUX_RESULTS_DIR:?set a results directory for this run}"
image=${MOUNT_RS_LINUX_IMAGE:-rust:1.95.0}
case "${AUDIT_BLOCKS:-memory}" in
  memory|local-fsync) ;;
  *) echo 'Linux isolated runner supports AUDIT_BLOCKS=memory or local-fsync' >&2; exit 2 ;;
esac
operations=${AUDIT_READ_OPERATIONS:-"stat readdir file"}
seen_operations=" "
for selected_operation in $operations; do
  case "$selected_operation" in
    stat|stat-json|statfs|stat-symlink|lstat|readlink|readdir|file|range) ;;
    *) echo 'AUDIT_READ_OPERATIONS accepts stat stat-json statfs stat-symlink lstat readlink readdir file range only' >&2; exit 2 ;;
  esac
  case "$seen_operations" in
    *" $selected_operation "*) echo 'duplicate read operation' >&2; exit 2 ;;
  esac
  seen_operations="$seen_operations$selected_operation "
done
[ "$seen_operations" != " " ] || { echo 'select at least one read operation' >&2; exit 2; }
case "$MOUNT_RS_LINUX_TARGET_DIR:$MOUNT_RS_LINUX_RESULTS_DIR" in
  /*:/*) ;;
  *) echo 'target and results directories must be absolute' >&2; exit 2 ;;
esac
[ -f "$MOUNT_RS_LINUX_TARGET_DIR/release/examples/cluster_load" ] || {
  echo 'build the Linux cluster_load example before running this script' >&2; exit 2;
}
platform=$(docker image inspect "$image" --format '{{.Os}}/{{.Architecture}}')
[ "$platform" = linux/arm64 ] || { echo 'requires an existing native linux/arm64 image' >&2; exit 2; }
host_arch=$(docker info --format '{{.Architecture}}')
case "$host_arch" in aarch64|arm64) ;; *) echo 'requires an ARM64 Docker engine, not emulation' >&2; exit 2 ;; esac
image_id=$(docker image inspect "$image" --format '{{.Id}}')
mkdir -p "$MOUNT_RS_LINUX_RESULTS_DIR"
for filename in linux-arm64-stat.json linux-arm64-readdir.json linux-arm64-file.json \
  linux-arm64-stat-checkpoint.json linux-arm64-readdir-checkpoint.json linux-arm64-file-checkpoint.json \
  linux-environment.json linux-image-id.txt linux-binary-sha256.txt \
  linux-stat-failure.json linux-readdir-failure.json linux-file-failure.json \
  linux-stat-udp-before.txt linux-stat-udp-after.txt \
  linux-readdir-udp-before.txt linux-readdir-udp-after.txt \
  linux-file-udp-before.txt linux-file-udp-after.txt \
  linux-arm64-range.json linux-arm64-range-checkpoint.json linux-range-failure.json \
  linux-range-udp-before.txt linux-range-udp-after.txt; do
  [ ! -e "$MOUNT_RS_LINUX_RESULTS_DIR/$filename" ] || {
    echo 'refusing to overwrite existing comparison results' >&2; exit 2;
  }
done
for selected_operation in $operations; do
  for filename in "linux-arm64-$selected_operation.json" "linux-arm64-$selected_operation-checkpoint.json" \
    "linux-$selected_operation-failure.json" "linux-$selected_operation-udp-before.txt" "linux-$selected_operation-udp-after.txt"; do
    [ ! -e "$MOUNT_RS_LINUX_RESULTS_DIR/$filename" ] || {
      echo 'refusing to overwrite existing comparison results' >&2; exit 2;
    }
  done
done
run_dir=$(mktemp -d)
registry_id=
cleanup() {
  if [ -n "$registry_id" ] && [ -n "${operation:-}" ] && [ ! -e "$MOUNT_RS_LINUX_RESULTS_DIR/linux-$operation-udp-after.txt" ]; then
    docker exec "$registry_id" cat /proc/net/snmp > "$MOUNT_RS_LINUX_RESULTS_DIR/linux-$operation-udp-after.txt" 2>/dev/null || true
  fi
  for cid in "$run_dir"/*.cid; do
    if [ -f "$cid" ]; then
      benchmark_id=$(cat "$cid")
      if [ -n "$benchmark_id" ]; then docker rm -f "$benchmark_id" >/dev/null 2>&1 || true; fi
      rm -f "$cid"
    fi
  done
  rmdir "$run_dir" 2>/dev/null || true
  if [ -n "$registry_id" ]; then docker rm -f "$registry_id" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
docker info --format '{"cpus":{{.NCPU}},"memory_bytes":{{.MemTotal}},"architecture":{{json .Architecture}}}' > "$MOUNT_RS_LINUX_RESULTS_DIR/linux-environment.json"
printf '%s\n' "$image_id" > "$MOUNT_RS_LINUX_RESULTS_DIR/linux-image-id.txt"
shasum -a 256 "$MOUNT_RS_LINUX_TARGET_DIR/release/examples/cluster_load" > "$MOUNT_RS_LINUX_RESULTS_DIR/linux-binary-sha256.txt"
case "${AUDIT_FORCE_UDP_BUFFERS:-0}" in
  0) set -- ;;
  1) set -- --cap-add NET_ADMIN ;;
  *) echo 'AUDIT_FORCE_UDP_BUFFERS must be 0 or 1' >&2; exit 2 ;;
esac
registry_id=$(docker run -d --network none redis:8.4.0-alpine redis-server --save '' --appendonly no)
for operation in $operations; do
  docker exec "$registry_id" cat /proc/net/snmp > "$MOUNT_RS_LINUX_RESULTS_DIR/linux-$operation-udp-before.txt"
  if docker run --rm "$@" --cidfile "$run_dir/$operation.cid" --network "container:$registry_id" \
    -e MOUNT_RS_REDIS_TEST_URL=redis://127.0.0.1:6379/ \
    -e AUDIT_SERVERS="${AUDIT_SERVERS:-3}" \
    -e AUDIT_SESSION_RPC="${AUDIT_SESSION_RPC:-0}" \
    -e AUDIT_CLIENT_THREADS="${AUDIT_CLIENT_THREADS:-${TOKIO_WORKER_THREADS:-4}}" \
    -e AUDIT_HEADER_READS="${AUDIT_HEADER_READS:-0}" \
    -e AUDIT_ROUTE_RENEWAL="${AUDIT_ROUTE_RENEWAL:-0}" \
    -e AUDIT_HTTP2="${AUDIT_HTTP2:-0}" \
    -e AUDIT_DIRECT_INGRESS="${AUDIT_DIRECT_INGRESS:-0}" \
    -e AUDIT_DIRECT_CLIENT_THREADS \
    -e AUDIT_DIRECT_PUBLIC="${AUDIT_DIRECT_PUBLIC:-0}" \
    -e AUDIT_DIRECT_MIXED="${AUDIT_DIRECT_MIXED:-0}" \
    -e AUDIT_LOCAL_WRITE_LIMIT="${AUDIT_LOCAL_WRITE_LIMIT:-}" \
    -e AUDIT_BLOCKS="${AUDIT_BLOCKS:-memory}" \
    -e AUDIT_CLIENT_LEVELS="${AUDIT_CLIENT_LEVELS:-100}" \
    -e AUDIT_SAMPLES="${AUDIT_SAMPLES:-20}" \
    -e AUDIT_RANGE_OFFSET="${AUDIT_RANGE_OFFSET:-0}" \
    -e AUDIT_RANGE_BYTES="${AUDIT_RANGE_BYTES:-4096}" \
    -e AUDIT_STORAGE_CHUNK_BYTES="${AUDIT_STORAGE_CHUNK_BYTES:-65536}" \
    -e AUDIT_DIRECTORY_ENTRIES="${AUDIT_DIRECTORY_ENTRIES:-0}" \
    -e AUDIT_PAYLOAD_BYTES="${AUDIT_PAYLOAD_BYTES:-131123}" \
    -e AUDIT_SERVER_THREADS="${AUDIT_SERVER_THREADS:-4}" \
    -e TOKIO_WORKER_THREADS="${TOKIO_WORKER_THREADS:-4}" \
    -e AUDIT_FORCE_UDP_BUFFERS="${AUDIT_FORCE_UDP_BUFFERS:-0}" \
    -e AUDIT_TCP_NODELAY="${AUDIT_TCP_NODELAY:-1}" \
    -e AUDIT_READ_PARALLELISM="${AUDIT_READ_PARALLELISM:-1}" \
    -e AUDIT_READ_CHUNK_BYTES="${AUDIT_READ_CHUNK_BYTES:-65536}" \
    -e AUDIT_WRITE_OPERATION="${AUDIT_WRITE_OPERATION:-file}" \
    -e AUDIT_WRITE_CONCURRENCY="${AUDIT_WRITE_CONCURRENCY:-100}" \
    -e AUDIT_DISK_CACHE="${AUDIT_DISK_CACHE:-0}" \
    -e AUDIT_SHARED_DISK_BUDGET="${AUDIT_SHARED_DISK_BUDGET:-0}" \
    -e AUDIT_ATIME="${AUDIT_ATIME:-noatime}" \
    -e AUDIT_SCHEDULER_PROBE="${AUDIT_SCHEDULER_PROBE:-0}" \
    -e AUDIT_PEER_POOLS="${AUDIT_PEER_POOLS:-1}" \
    -e AUDIT_INGRESS_WRITE_LIMIT="${AUDIT_INGRESS_WRITE_LIMIT:-}" \
    -e AUDIT_INGRESS_WRITE_QUEUE="${AUDIT_INGRESS_WRITE_QUEUE:-100}" \
    -e AUDIT_SEPARATE_READ_PEERS="${AUDIT_SEPARATE_READ_PEERS:-0}" \
    -e AUDIT_BLOCKING_THREADS="${AUDIT_BLOCKING_THREADS:-}" \
    -e AUDIT_UDP_BUFFER_BYTES="${AUDIT_UDP_BUFFER_BYTES:-4194304}" \
    -e AUDIT_UDP_RECV_BUFFER_BYTES="${AUDIT_UDP_RECV_BUFFER_BYTES:-${AUDIT_UDP_BUFFER_BYTES:-4194304}}" \
    -e AUDIT_UDP_SEND_BUFFER_BYTES="${AUDIT_UDP_SEND_BUFFER_BYTES:-${AUDIT_UDP_BUFFER_BYTES:-4194304}}" \
    -e AUDIT_ROUTE_CACHE_TTL_MS="${AUDIT_ROUTE_CACHE_TTL_MS:-1000}" \
    -e AUDIT_SHARED_READ_VOLUME="${AUDIT_SHARED_READ_VOLUME:-0}" \
    -e AUDIT_LISTING_GZIP="${AUDIT_LISTING_GZIP:-0}" \
    -e AUDIT_READ_OPERATION="$operation" \
    -e AUDIT_CHECKPOINT_PATH="/audit-results/linux-arm64-$operation-checkpoint.json" \
    -v "$MOUNT_RS_LINUX_TARGET_DIR:/audit-target:ro" \
    -v "$MOUNT_RS_LINUX_RESULTS_DIR:/audit-results" \
    "$image_id" /audit-target/release/examples/cluster_load \
    > "$MOUNT_RS_LINUX_RESULTS_DIR/linux-arm64-$operation.json"; then
    :
  else
    benchmark_status=$?
    printf '{"complete":false,"operation":"%s","exit_code":%s,"checkpoint":"linux-arm64-%s-checkpoint.json","note":"Checkpoint may be absent if startup failed; do not treat partial phases as a complete run."}\n' \
      "$operation" "$benchmark_status" "$operation" > "$MOUNT_RS_LINUX_RESULTS_DIR/linux-$operation-failure.json"
    exit "$benchmark_status"
  fi
  docker exec "$registry_id" cat /proc/net/snmp > "$MOUNT_RS_LINUX_RESULTS_DIR/linux-$operation-udp-after.txt"
done
