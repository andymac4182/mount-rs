#!/bin/sh
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
provider=${1:?provider required}
output=${2:?retained output directory required}
case "$provider" in sqlite|tidb|foundationdb) ;; *) exit 2 ;; esac
counts=${MOUNT_RS_REMOTE_SCALING_SERVERS:-1,2,4,8,16,32,64,100}
client_counts=${MOUNT_RS_REMOTE_SCALING_CLIENTS:-100}
# Validate the complete list before starting any expensive workload.
old_ifs=$IFS
IFS=,
set -- $counts
IFS=$old_ifs
for count do
  case "$count" in ''|*[!0-9]*) echo 'invalid server count' >&2; exit 2 ;; esac
  [ "$count" -ge 1 ] && [ "$count" -le 100 ] || exit 2
done
IFS=,
for clients in $client_counts; do
  case "$clients" in ''|*[!0-9]*) echo 'invalid client count' >&2; exit 2 ;; esac
  [ "$clients" -ge 1 ] && [ "$clients" -le 10000 ] || exit 2
  for count do [ "$count" -le "$clients" ] || exit 2; done
done
IFS=$old_ifs
mkdir -p "$output"
export MOUNT_RS_PROFILE_IO=1
export MOUNT_RS_REMOTE_SATURATION_PROVIDER="$provider"
export MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS=32
export MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=15
export MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS=3
export MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=0
export MOUNT_RS_REMOTE_SATURATION_VERIFY_SECONDS="${MOUNT_RS_REMOTE_SATURATION_VERIFY_SECONDS:-1200}"
export MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=read,write
export MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS="${MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS:-1}"
feature=resource-profiling
if [ "${MOUNT_RS_TRACE_ALLOCATIONS:-0}" = 1 ]; then feature=allocation-profiling; fi
if [ "$provider" = foundationdb ]; then feature="$feature,saturation-foundationdb"; fi
for count do
  IFS=,
  for clients in $client_counts; do
  IFS=$old_ifs
  export MOUNT_RS_REMOTE_SATURATION_SERVERS="$count"
  export MOUNT_RS_REMOTE_SATURATION_CLIENTS="$clients"
  active=${MOUNT_RS_REMOTE_SATURATION_ACTIVE_CLIENTS:-$clients}
  export MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT="$output/$provider-$count-$clients-$active.json"
  if [ "$provider" = foundationdb ]; then
    # The native harness already supplies its isolated Linux target and library.
    cargo test --release --locked -p mount-rs-service --features "$feature" --test quic_tidb_saturation actual_tidb_100_clients_10_servers_saturation -- --ignored --nocapture --test-threads=1
  else
    ./scripts/cargo-shared test --release --offline --locked -p mount-rs-service --features "$feature" --test quic_tidb_saturation actual_tidb_100_clients_10_servers_saturation -- --ignored --nocapture --test-threads=1
  fi
  done
done
