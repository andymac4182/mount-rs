#!/bin/sh
set -eu

# This harness deliberately uses the official PingCAP component images rather
# than a MySQL-compatible substitute or TiDB's unistore/mocktikv test mode.
# The default topology has three PD nodes and three TiKV nodes so the test's
# restart check exercises a Raft-backed cluster. All host-facing ports are
# loopback-only and ephemeral.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"
cd "$repo_dir"
if ! command -v docker >/dev/null 2>&1; then
  echo "test-tidb.sh: Docker is required" >&2
  exit 2
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "test-tidb.sh: curl is required for local readiness checks" >&2
  exit 2
fi
if ! docker info >/dev/null 2>&1; then
  echo "test-tidb.sh: Docker daemon is unavailable; actual TiDB/PD/TiKV acceptance cannot run" >&2
  exit 2
fi

startup_timeout_seconds=${MOUNT_RS_TIDB_STARTUP_TIMEOUT_SECONDS:-300}
case "$startup_timeout_seconds" in
  ''|*[!0-9]*)
    echo "test-tidb.sh: MOUNT_RS_TIDB_STARTUP_TIMEOUT_SECONDS must be a non-negative integer" >&2
    exit 2
    ;;
esac
if [ "$startup_timeout_seconds" -lt 30 ]; then
  echo "test-tidb.sh: MOUNT_RS_TIDB_STARTUP_TIMEOUT_SECONDS must be at least 30" >&2
  exit 2
fi

tikv_nofile_limit=${MOUNT_RS_TIDB_TIKV_NOFILE_LIMIT:-200000}
case "$tikv_nofile_limit" in
  ''|*[!0-9]*)
    echo "test-tidb.sh: MOUNT_RS_TIDB_TIKV_NOFILE_LIMIT must be a non-negative integer" >&2
    exit 2
    ;;
esac
if [ "$tikv_nofile_limit" -lt 123880 ]; then
  echo "test-tidb.sh: MOUNT_RS_TIDB_TIKV_NOFILE_LIMIT must be at least 123880 for TiKV v8.5.7" >&2
  exit 2
fi

topology=${MOUNT_RS_TIDB_TOPOLOGY:-durable}
case "$topology" in
  durable)
    pd_count=3
    tikv_count=3
    ;;
  single)
    pd_count=1
    tikv_count=1
    ;;
  *)
    echo "test-tidb.sh: MOUNT_RS_TIDB_TOPOLOGY must be durable or single" >&2
    exit 2
    ;;
esac

image_version=${MOUNT_RS_TIDB_VERSION:-v8.5.7}
case "$image_version" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "test-tidb.sh: MOUNT_RS_TIDB_VERSION must be a pinned vX.Y.Z tag" >&2
    exit 2
    ;;
esac

docker_platform=${MOUNT_RS_TIDB_DOCKER_PLATFORM:-}
if [ -z "$docker_platform" ]; then
  docker_arch=$(docker info --format '{{.Architecture}}')
  case "$docker_arch" in
    aarch64|arm64) docker_platform=linux/arm64 ;;
    amd64|x86_64) docker_platform=linux/amd64 ;;
    *)
      echo "test-tidb.sh: unsupported Docker architecture: $docker_arch" >&2
      exit 2
      ;;
  esac
fi

run_napi=0
if [ "${MOUNT_RS_TIDB_NAPI:-0}" = "1" ]; then
  run_napi=1
  if ! command -v node >/dev/null 2>&1; then
    echo "test-tidb.sh: MOUNT_RS_TIDB_NAPI=1 requires node" >&2
    exit 2
  fi
fi
case "$docker_platform" in
  linux/arm64|linux/amd64) ;;
  *)
    echo "test-tidb.sh: MOUNT_RS_TIDB_DOCKER_PLATFORM must be linux/arm64 or linux/amd64" >&2
    exit 2
    ;;
esac

docker_mem_bytes=$(docker info --format '{{.MemTotal}}')
docker_cpu_count=$(docker info --format '{{.NCPU}}')
case "$docker_mem_bytes" in
  ''|*[!0-9]*)
    echo "test-tidb.sh: Docker reported an invalid memory limit: $docker_mem_bytes" >&2
    exit 2
    ;;
esac
case "$docker_cpu_count" in
  ''|*[!0-9]*)
    echo "test-tidb.sh: Docker reported an invalid CPU count: $docker_cpu_count" >&2
    exit 2
    ;;
esac
allow_underprovisioned=${MOUNT_RS_TIDB_ALLOW_UNDERPROVISIONED:-0}
case "$allow_underprovisioned" in
  0|1) ;;
  *)
    echo "test-tidb.sh: MOUNT_RS_TIDB_ALLOW_UNDERPROVISIONED must be 0 or 1" >&2
    exit 2
    ;;
esac
durable_min_memory_bytes=10737418240
durable_min_cpu_count=4
capacity_issue=0
capacity_details=""
if [ "$topology" = durable ]; then
  if [ "$docker_mem_bytes" -lt "$durable_min_memory_bytes" ]; then
    capacity_issue=1
    capacity_details="${capacity_details} memory_bytes=$docker_mem_bytes minimum_memory_bytes=$durable_min_memory_bytes"
  fi
  if [ "$docker_cpu_count" -lt "$durable_min_cpu_count" ]; then
    capacity_issue=1
    capacity_details="${capacity_details} cpus=$docker_cpu_count minimum_cpus=$durable_min_cpu_count"
  fi
  if [ "$capacity_issue" -ne 0 ]; then
    if [ "$allow_underprovisioned" -ne 1 ]; then
      echo "test-tidb.sh: durable topology lacks required Docker capacity:$capacity_details (set MOUNT_RS_TIDB_ALLOW_UNDERPROVISIONED=1 only for a diagnostic attempt)" >&2
      exit 2
    fi
    echo "test-tidb.sh: warning: diagnostic underprovisioned durable run:$capacity_details" >&2
  fi
fi

temp_root=${TMPDIR:-/tmp}
if [ "$temp_root" != "/" ]; then
  temp_root=${temp_root%/}
fi
run_dir=$(mktemp -d "$temp_root/mount-rs-tidb.XXXXXX")
run_id=$(basename "$run_dir" | tr '.' '-')
network_name="mount-rs-tidb-net-$run_id"
resource_label="mount-rs.tidb.run=$run_id"
resource_value="$run_id"
pd_image="pingcap/pd:$image_version"
tikv_image="pingcap/tikv:$image_version"
tidb_image="pingcap/tidb:$image_version"
tikv_config="$repo_dir/tests/tidb/tikv-test.toml"
tidb_config="$repo_dir/tests/tidb/tidb-test.toml"
if [ ! -r "$tikv_config" ]; then
  echo "test-tidb.sh: missing readable TiKV test config: $tikv_config" >&2
  exit 2
fi
if [ ! -r "$tidb_config" ]; then
  echo "test-tidb.sh: missing readable TiDB test config: $tidb_config" >&2
  exit 2
fi
created_network=0
created_containers=""
created_volumes=""
tidb_container=""
tidb_sql_port=""
tidb_status_port=""
startup_deadline=""
phase_name="initialization"
network_subnet=""
network_prefix=""
pd_ips=""
tikv_ips=""
tidb_ip=""

redact_stream() {
  # No test URL is printed by this script. Keep diagnostics safe if a future
  # component happens to include a MySQL URL in an error message.
  sed -E 's#(mysql://[^:/@]+:)[^/@]+@#\1[redacted]@#g'
}

print_logs() {
  for container in $created_containers; do
    echo "--- Docker logs: $container ---" >&2
    docker logs --tail 60 "$container" 2>&1 | redact_stream >&2 || true
  done
}

print_diagnostics() {
  echo "--- TiDB harness diagnostics: phase=$phase_name ---" >&2
  echo "docker_capacity cpus=$docker_cpu_count mem_bytes=$docker_mem_bytes" >&2
  for container in $created_containers; do
    docker inspect --format \
      'container={{.Name}} state={{.State.Status}} exit={{.State.ExitCode}} error={{.State.Error}} ip={{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' \
      "$container" 2>&1 | redact_stream >&2 || true
  done
  if docker network inspect "$network_name" >/dev/null 2>&1; then
    docker network inspect --format 'network={{.Name}} subnet={{range .IPAM.Config}}{{.Subnet}}{{end}} containers={{json .Containers}}' \
      "$network_name" 2>&1 | redact_stream >&2 || true
  fi
  if [ -n "$created_containers" ]; then
    docker stats --no-stream --format 'stats={{.Name}} cpu={{.CPUPerc}} mem={{.MemUsage}} mem_percent={{.MemPerc}}' $created_containers 2>&1 | redact_stream >&2 || true
  fi
  print_logs
}

container_owned() {
  container_name=$1
  ownership=$(docker inspect --format '{{index .Config.Labels "mount-rs.tidb.run"}}' "$container_name" 2>/dev/null || true)
  [ "$ownership" = "$resource_value" ]
}

volume_owned() {
  volume_name=$1
  ownership=$(docker volume inspect --format '{{index .Labels "mount-rs.tidb.run"}}' "$volume_name" 2>/dev/null || true)
  [ "$ownership" = "$resource_value" ]
}

network_owned() {
  ownership=$(docker network inspect --format '{{index .Labels "mount-rs.tidb.run"}}' "$network_name" 2>/dev/null || true)
  [ "$ownership" = "$resource_value" ]
}

remove_run_dir() {
  case "$run_dir" in
    "$temp_root"/mount-rs-tidb.??????) ;;
    *)
      echo "test-tidb.sh: refusing to remove an unvalidated run directory: $run_dir" >&2
      return 1
      ;;
  esac
  if [ -d "$run_dir" ] && [ ! -L "$run_dir" ]; then
    rm -rf -- "$run_dir"
  fi
  [ ! -e "$run_dir" ] && [ ! -L "$run_dir" ]
}

cleanup() {
  exit_status=$?
  trap - EXIT INT TERM
  if [ "$exit_status" -ne 0 ]; then
    print_diagnostics
  fi
  set +e
  cleanup_status=0
  for container in $created_containers; do
    cleanup_attempt=1
    while [ "$cleanup_attempt" -le 10 ]; do
      if ! docker inspect "$container" >/dev/null 2>&1; then
        break
      fi
      if ! container_owned "$container"; then
        echo "test-tidb.sh: refusing to remove container without this run's ownership label: $container" >&2
        cleanup_status=1
        break
      fi
      docker rm --force "$container" >/dev/null 2>&1 || true
      cleanup_attempt=$((cleanup_attempt + 1))
      sleep 1
    done
    if docker inspect "$container" >/dev/null 2>&1; then
      echo "test-tidb.sh: container remained after bounded cleanup: $container" >&2
      cleanup_status=1
    fi
  done
  for volume in $created_volumes; do
    cleanup_attempt=1
    while [ "$cleanup_attempt" -le 10 ]; do
      if ! docker volume inspect "$volume" >/dev/null 2>&1; then
        break
      fi
      if ! volume_owned "$volume"; then
        echo "test-tidb.sh: refusing to remove volume without this run's ownership label: $volume" >&2
        cleanup_status=1
        break
      fi
      docker volume rm "$volume" >/dev/null 2>&1 || true
      cleanup_attempt=$((cleanup_attempt + 1))
      sleep 1
    done
    if docker volume inspect "$volume" >/dev/null 2>&1; then
      echo "test-tidb.sh: volume remained after bounded cleanup: $volume" >&2
      cleanup_status=1
    fi
  done
  if [ "$created_network" -eq 1 ]; then
    cleanup_attempt=1
    while [ "$cleanup_attempt" -le 10 ]; do
      if ! docker network inspect "$network_name" >/dev/null 2>&1; then
        break
      fi
      if ! network_owned; then
        echo "test-tidb.sh: refusing to remove network without this run's ownership label: $network_name" >&2
        cleanup_status=1
        break
      fi
      docker network rm "$network_name" >/dev/null 2>&1 || true
      cleanup_attempt=$((cleanup_attempt + 1))
      sleep 1
    done
    if docker network inspect "$network_name" >/dev/null 2>&1; then
      echo "test-tidb.sh: network remained after bounded cleanup: $network_name" >&2
      cleanup_status=1
    fi
  fi
  if ! remove_run_dir; then
    cleanup_status=1
  fi
  if [ "$cleanup_status" -ne 0 ] && [ "$exit_status" -eq 0 ]; then
    exit_status=1
  fi
  exit "$exit_status"
}

on_signal() {
  exit 130
}

trap cleanup EXIT
trap on_signal INT TERM

create_volume() {
  volume_name=$1
  docker volume create --label "$resource_label" "$volume_name" >/dev/null
  created_volumes="$created_volumes $volume_name"
}

create_network() {
  network_candidate_seed=$(printf '%s' "$run_id" | cksum | awk '{print $1 % 240 + 1}')
  network_candidate_attempt=0
  while [ "$network_candidate_attempt" -lt 240 ]; do
    network_candidate_octet=$(( (network_candidate_seed + network_candidate_attempt - 1) % 240 + 1 ))
    network_candidate_subnet="172.30.$network_candidate_octet.0/24"
    network_create_error="$run_dir/network-create.err"
    if docker network create --driver bridge --subnet "$network_candidate_subnet" \
      --label "$resource_label" "$network_name" > /dev/null 2>"$network_create_error"; then
      created_network=1
      break
    fi
    network_create_output=$(sed -n '1,4p' "$network_create_error" 2>/dev/null || true)
    case "$network_create_output" in
      *overlap*|*"address space"*)
        network_candidate_attempt=$((network_candidate_attempt + 1))
        ;;
      *)
        printf '%s\n' "$network_create_output" >&2
        echo "test-tidb.sh: could not create the isolated Docker network" >&2
        return 1
        ;;
    esac
  done
  if [ "$created_network" -ne 1 ]; then
    echo "test-tidb.sh: exhausted isolated Docker subnet candidates" >&2
    return 1
  fi
  network_subnet=$(docker network inspect --format '{{(index .IPAM.Config 0).Subnet}}' "$network_name")
  network_prefix=$(printf '%s\n' "$network_subnet" | awk -F'[./]' \
    'NF >= 5 && $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ {print $1 "." $2 "." $3}')
  network_prefix_bits=$(printf '%s\n' "$network_subnet" | awk -F/ 'NF == 2 {print $2}')
  case "$network_prefix_bits" in
    ''|*[!0-9]*)
      echo "test-tidb.sh: could not determine the IPv4 prefix for $network_name ($network_subnet)" >&2
      return 1
      ;;
  esac
  if [ -z "$network_prefix" ] || [ "$network_prefix_bits" -gt 24 ]; then
    echo "test-tidb.sh: Docker allocated an IPv4 subnet too small for stable service addresses: $network_subnet" >&2
    return 1
  fi

  pd_ip1="$network_prefix.10"
  pd_ip2="$network_prefix.11"
  pd_ip3="$network_prefix.12"
  tikv_ip1="$network_prefix.20"
  tikv_ip2="$network_prefix.21"
  tikv_ip3="$network_prefix.22"
  tidb_ip="$network_prefix.30"
  if [ "$pd_count" -eq 3 ]; then
    pd_ips="$pd_ip1:2379,$pd_ip2:2379,$pd_ip3:2379"
    pd_cluster="pd1=http://$pd_ip1:2380,pd2=http://$pd_ip2:2380,pd3=http://$pd_ip3:2380"
  else
    pd_ips="$pd_ip1:2379"
    pd_cluster="pd1=http://$pd_ip1:2380"
  fi
  pd_endpoints="$pd_ips"
  if [ "$tikv_count" -eq 3 ]; then
    tikv_ips="$tikv_ip1,$tikv_ip2,$tikv_ip3"
  else
    tikv_ips="$tikv_ip1"
  fi
  echo "TiDB network=$network_name subnet=$network_subnet service_ips=PD:$pd_ips TiKV:$tikv_ips TiDB:$tidb_ip" >&2
}

pd_ip_for_number() {
  case "$1" in
    1) printf '%s\n' "$pd_ip1" ;;
    2) printf '%s\n' "$pd_ip2" ;;
    3) printf '%s\n' "$pd_ip3" ;;
    *) return 1 ;;
  esac
}

tikv_ip_for_number() {
  case "$1" in
    1) printf '%s\n' "$tikv_ip1" ;;
    2) printf '%s\n' "$tikv_ip2" ;;
    3) printf '%s\n' "$tikv_ip3" ;;
    *) return 1 ;;
  esac
}

start_pd() {
  pd_number=$1
  pd_container=$2
  pd_volume=$3
  pd_ip=$(pd_ip_for_number "$pd_number")
  docker run --detach \
    --platform "$docker_platform" \
    --name "$pd_container" \
    --label "$resource_label" \
    --network "$network_name" \
    --ip "$pd_ip" \
    --network-alias "pd$pd_number" \
    --volume "$pd_volume:/data" \
    "$pd_image" \
    --name="pd$pd_number" \
    --client-urls=http://0.0.0.0:2379 \
    --peer-urls=http://0.0.0.0:2380 \
    --advertise-client-urls="http://$pd_ip:2379" \
    --advertise-peer-urls="http://$pd_ip:2380" \
    --initial-cluster="$pd_cluster" \
    --log-level=warn \
    --data-dir=/data \
    >/dev/null
  created_containers="$created_containers $pd_container"
}

start_tikv() {
  tikv_number=$1
  tikv_container=$2
  tikv_volume=$3
  tikv_ip=$(tikv_ip_for_number "$tikv_number")
  docker run --detach \
    --platform "$docker_platform" \
    --ulimit "nofile=$tikv_nofile_limit:$tikv_nofile_limit" \
    --name "$tikv_container" \
    --label "$resource_label" \
    --network "$network_name" \
    --ip "$tikv_ip" \
    --network-alias "tikv$tikv_number" \
    --volume "$tikv_volume:/data" \
    --volume "$tikv_config:/etc/tikv-test.toml:ro" \
    "$tikv_image" \
    --config=/etc/tikv-test.toml \
    --pd-endpoints="$pd_endpoints" \
    --addr=0.0.0.0:20160 \
    --advertise-addr="$tikv_ip:20160" \
    --status-addr=0.0.0.0:20180 \
    --data-dir=/data \
    >/dev/null
  created_containers="$created_containers $tikv_container"
}

wait_for_pd() {
  pd_container=$1
  pd_label=$2
  while [ "$(date +%s)" -lt "$startup_deadline" ]; do
    container_state=$(docker inspect --format '{{.State.Status}}' "$pd_container" 2>/dev/null || true)
    if [ "$container_state" != running ]; then
      echo "test-tidb.sh: $pd_label stopped before becoming ready" >&2
      return 1
    fi
    if docker exec "$pd_container" curl -fsS --max-time 2 \
      http://127.0.0.1:2379/pd/api/v1/health >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "test-tidb.sh: timed out after ${startup_timeout_seconds}s waiting for $pd_label" >&2
  return 1
}

wait_for_pd_quorum() {
  while [ "$(date +%s)" -lt "$startup_deadline" ]; do
    if pd_quorum_ready; then
      return 0
    fi
    sleep 1
  done
  echo "test-tidb.sh: timed out after ${startup_timeout_seconds}s waiting for PD quorum ($pd_count members)" >&2
  return 1
}

pd_quorum_ready() {
  members_json=$(docker exec "$pd1_container" curl -fsS --max-time 3 \
    http://127.0.0.1:2379/pd/api/v1/members 2>/dev/null || true)
  member_count=$(printf '%s' "$members_json" | awk '{sum += gsub(/"member_id"/, "")} END {print sum + 0}')
  [ "${member_count:-0}" -ge "$pd_count" ]
}

wait_for_tikv() {
  tikv_container=$1
  tikv_label=$2
  while [ "$(date +%s)" -lt "$startup_deadline" ]; do
    container_state=$(docker inspect --format '{{.State.Status}}' "$tikv_container" 2>/dev/null || true)
    if [ "$container_state" != running ]; then
      echo "test-tidb.sh: $tikv_label stopped before becoming ready" >&2
      return 1
    fi
    if docker exec "$tikv_container" curl -fsS --max-time 2 \
      http://127.0.0.1:20180/status >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "test-tidb.sh: timed out after ${startup_timeout_seconds}s waiting for $tikv_label" >&2
  return 1
}

wait_for_store_count() {
  while [ "$(date +%s)" -lt "$startup_deadline" ]; do
    stores_json=$(docker exec "$pd1_container" curl -fsS --max-time 2 \
      http://127.0.0.1:2379/pd/api/v1/stores 2>/dev/null || true)
    store_count=$(printf '%s' "$stores_json" | awk '{sum += gsub(/"state_name"[[:space:]]*:[[:space:]]*"Up"/, "")} END {print sum + 0}')
    if [ "${store_count:-0}" -ge "$tikv_count" ]; then
      return 0
    fi
    sleep 1
  done
  echo "test-tidb.sh: timed out after ${startup_timeout_seconds}s waiting for $tikv_count TiKV stores" >&2
  return 1
}

wait_for_tidb() {
  pd_quorum_failures=0
  while [ "$(date +%s)" -lt "$startup_deadline" ]; do
    container_state=$(docker inspect --format '{{.State.Status}}' "$tidb_container" 2>/dev/null || true)
    if [ "$container_state" != running ]; then
      echo "test-tidb.sh: TiDB stopped before its status endpoint became ready" >&2
      return 1
    fi
    if pd_quorum_ready; then
      pd_quorum_failures=0
    else
      pd_quorum_failures=$((pd_quorum_failures + 1))
      if [ "$pd_quorum_failures" -ge 20 ]; then
        echo "test-tidb.sh: PD quorum was unavailable for 20 readiness checks while waiting for TiDB" >&2
        return 1
      fi
    fi
    if curl -fsS --connect-timeout 1 --max-time 5 \
      "http://127.0.0.1:$tidb_status_port/status" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "test-tidb.sh: timed out after ${startup_timeout_seconds}s waiting for TiDB SQL/status endpoints" >&2
  return 1
}

start_tidb() {
  tidb_container_name=$1
  docker run --detach \
    --platform "$docker_platform" \
    --name "$tidb_container_name" \
    --label "$resource_label" \
    --network "$network_name" \
    --ip "$tidb_ip" \
    --network-alias tidb \
    --publish 127.0.0.1::4000/tcp \
    --publish 127.0.0.1::10080/tcp \
    --volume "$tidb_config:/etc/tidb-test.toml:ro" \
    "$tidb_image" \
    --config=/etc/tidb-test.toml \
    --store=tikv \
    --path="$pd_endpoints" \
    --host=0.0.0.0 \
    --advertise-address="$tidb_ip" \
    -P=4000 \
    -L=warn \
    --status=10080 \
    >/dev/null
  created_containers="$created_containers $tidb_container_name"
  tidb_container="$tidb_container_name"
  tidb_sql_port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "4000/tcp") 0).HostPort}}' "$tidb_container")
  tidb_status_port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "10080/tcp") 0).HostPort}}' "$tidb_container")
}

begin_phase() {
  phase_name=$1
  startup_deadline=$(($(date +%s) + startup_timeout_seconds))
  echo "TiDB phase=$phase_name timeout=${startup_timeout_seconds}s" >&2
}

run_direct_provider_test() {
  persistence_expectation=$1
  # Each top-level ignored test initializes the shared provider schema. Keep
  # those DDL transactions serial while preserving the tests' own explicit
  # concurrency coverage; parallel schema creation can leave TiDB's DDL
  # owner/recovery path unable to expose the status endpoint after restart.
  MOUNT_RS_TIDB_URL="$tidb_url" \
  MOUNT_RS_TIDB_TEST_VOLUME_KEY="$volume_key" \
  MOUNT_RS_TIDB_EXPECT_PERSISTED="$persistence_expectation" \
    "$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-tidb --test tidb -- --ignored --nocapture --test-threads=1
}

run_provider_test() {
  persistence_expectation=$1
  # A composition test must not replace direct TiDB identity, schema,
  # fencing, ambiguous-commit, or reconnect evidence for the metadata
  # provider itself.
  if run_direct_provider_test "$persistence_expectation"; then
    :
  else
    direct_status=$?
    echo "test-tidb.sh: direct provider contract failed before composition" >&2
    return "$direct_status"
  fi
  if [ -n "${MOUNT_RS_TIDB_COMPOSITION_COMMAND:-}" ]; then
    # The caller owns the block service and supplies a complete, explicit
    # command for a split-provider composition. Keep the TiDB URL and the
    # restart expectation in the child environment without putting either
    # credentials or URLs into the command string.
    MOUNT_RS_TIDB_URL="$tidb_url" \
    MOUNT_RS_TIDB_TEST_VOLUME_KEY="$volume_key" \
    MOUNT_RS_TIDB_EXPECT_PERSISTED="$persistence_expectation" \
      sh -c "$MOUNT_RS_TIDB_COMPOSITION_COMMAND"
  fi
}

run_node_provider_test() {
  if [ "$run_napi" -ne 1 ]; then
    return 0
  fi
  node_prefix=${MOUNT_RS_TIDB_NODE_PREFIX:-${MOUNT_RS_TIDB_RUSTFS_PREFIX:-mount-rs-tidb/$run_id}/napi}
  MOUNT_RS_TIDB_NAPI=1 \
  MOUNT_RS_TIDB_URL="$tidb_url" \
  MOUNT_RS_TIDB_NODE_PREFIX="$node_prefix" \
    node "$repo_dir/integrations/mount-rs-napi/test/tidb.mjs"
}

run_ambiguous_commit_test() {
  MOUNT_RS_TIDB_URL="$tidb_url" \
    "$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-tidb --test ambiguous_commit -- --ignored --nocapture
}

echo "Starting actual TiDB/TiKV test topology=$topology version=$image_version platform=$docker_platform docker_cpus=$docker_cpu_count docker_mem_bytes=$docker_mem_bytes" >&2
docker pull --platform "$docker_platform" "$pd_image" >/dev/null
docker pull --platform "$docker_platform" "$tikv_image" >/dev/null
docker pull --platform "$docker_platform" "$tidb_image" >/dev/null

for image in "$pd_image" "$tikv_image" "$tidb_image"; do
  image_platform=$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$image")
  if [ "$image_platform" != "$docker_platform" ]; then
    echo "test-tidb.sh: $image resolved to $image_platform, expected $docker_platform" >&2
    exit 1
  fi
done

begin_phase "network allocation"
create_network

begin_phase "PD startup"
pd_number=1
while [ "$pd_number" -le "$pd_count" ]; do
  pd1_container="mount-rs-tidb-$run_id-pd$pd_number"
  pd1_volume="mount-rs-tidb-$run_id-pd$pd_number-data"
  create_volume "$pd1_volume"
  start_pd "$pd_number" "$pd1_container" "$pd1_volume"
  pd_number=$((pd_number + 1))
done

pd1_container="mount-rs-tidb-$run_id-pd1"
begin_phase "PD readiness"
pd_number=1
while [ "$pd_number" -le "$pd_count" ]; do
  wait_for_pd "mount-rs-tidb-$run_id-pd$pd_number" "PD$pd_number"
  pd_number=$((pd_number + 1))
done
wait_for_pd_quorum

phase_name="TiKV startup"
begin_phase "$phase_name"
tikv_number=1
while [ "$tikv_number" -le "$tikv_count" ]; do
  tikv1_container="mount-rs-tidb-$run_id-tikv$tikv_number"
  tikv1_volume="mount-rs-tidb-$run_id-tikv$tikv_number-data"
  create_volume "$tikv1_volume"
  start_tikv "$tikv_number" "$tikv1_container" "$tikv1_volume"
  tikv_number=$((tikv_number + 1))
done

begin_phase "TiKV readiness"
tikv_number=1
while [ "$tikv_number" -le "$tikv_count" ]; do
  wait_for_tikv "mount-rs-tidb-$run_id-tikv$tikv_number" "TiKV$tikv_number"
  tikv_number=$((tikv_number + 1))
done
wait_for_store_count

begin_phase "TiDB startup"
tidb_container="mount-rs-tidb-$run_id-tidb"
# TiDB's force-init-stats gate can withhold the SQL/status service while it
# rebuilds optimizer statistics after a frontend restart. W08 validates the
# provider and replicated-store durability, not optimizer warm-up; keep
# readiness independent of that optional startup phase.
start_tidb "$tidb_container"
begin_phase "TiDB readiness"
wait_for_tidb

# This URL is intentionally kept in the process environment only. The Rust
# test performs the TiDB identity query before opening provider schemas.
tidb_url="mysql://root@127.0.0.1:$tidb_sql_port/test"
volume_key="mount-rs-tidb-$run_id"

set +e
run_provider_test 0
provider_status=$?
set -e
if [ "$provider_status" -ne 0 ]; then
  exit "$provider_status"
fi
run_node_provider_test
if [ "$topology" = durable ]; then
  # Confirm data remains available through PD quorum recovery, a TiDB frontend
  # restart, and a TiKV store restart. This is still a test-cluster check, not
  # a power-loss or host-fsync guarantee; the provider's durable flag remains
  # caller-owned.
  begin_phase "TiDB restart readiness"
  # Keep the PD/TiKV-backed state while giving the frontend a fresh process and
  # listener lifecycle. In-place Docker restart can leave TiDB v8.5.7 blocked
  # in DDL/domain bootstrap after its graceful shutdown has completed.
  docker stop --time 30 "$tidb_container" >/dev/null
  docker rm "$tidb_container" >/dev/null
  start_tidb "$tidb_container"
  tidb_url="mysql://root@127.0.0.1:$tidb_sql_port/test"
  wait_for_tidb
  begin_phase "TiKV restart readiness"
  docker restart "mount-rs-tidb-$run_id-tikv1" >/dev/null
  wait_for_tikv "mount-rs-tidb-$run_id-tikv1" "TiKV1 after restart"
  wait_for_store_count
  begin_phase "PD restart readiness"
  docker restart "$pd1_container" >/dev/null
  wait_for_pd "$pd1_container" "PD1 after restart"
  wait_for_pd_quorum
  wait_for_tidb
  set +e
  run_provider_test 1
  provider_status=$?
  set -e
  if [ "$provider_status" -ne 0 ]; then
    exit "$provider_status"
  fi
fi

# The failure-injection lane deliberately drops a client connection after
# TiDB has processed COMMIT. Keep it after the durable restart/reopen gate so
# an intentionally unknown client outcome cannot contaminate the frontend
# startup check; it still exercises the provider's no-replay contract before
# the owned topology is cleaned up.
run_ambiguous_commit_test

if [ "$topology" = durable ] && [ "$capacity_issue" -eq 0 ]; then
  evidence_class="durable-multinode-restart"
elif [ "$topology" = durable ]; then
  evidence_class="diagnostic-underprovisioned-not-durable-acceptance"
else
  evidence_class="single-node-smoke-not-replicated-acceptance"
fi
echo "TIDB_ACCEPTANCE evidence=$evidence_class topology=$topology version=$image_version platform=$docker_platform cpus=$docker_cpu_count mem_bytes=$docker_mem_bytes ambiguous_commit=pass" >&2
