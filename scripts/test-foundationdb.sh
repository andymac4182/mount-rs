#!/bin/sh
set -eu

# Run the real FoundationDB provider gate without installing a native client on
# the host. When called by scripts/test-rustfs.sh as a combo command, the RustFS
# endpoint is passed through to a separate Rust client container.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/foundationdb-napi-fixture-env.sh"
fdb_image_override=${MOUNT_RS_FOUNDATIONDB_IMAGE:-}
fdb_image=""
rust_image=${MOUNT_RS_FOUNDATIONDB_RUST_IMAGE:-rust:1.95-bookworm}
node_image=${MOUNT_RS_FOUNDATIONDB_NODE_IMAGE:-node:24-bookworm}
run_napi=0
if [ "${MOUNT_RS_FOUNDATIONDB_NAPI:-0}" = "1" ]; then
  run_napi=1
  sh "$repo_dir/scripts/test-foundationdb-napi-fixture-env.sh"
  if [ -n "${RUSTFS_ENDPOINT:-}" ]; then
    configure_foundationdb_napi_fixture rustfs
  else
    configure_foundationdb_napi_fixture generic
  fi
fi
run_iops=0
workload_profile=""
if [ "${MOUNT_RS_FOUNDATIONDB_WORKLOAD_PROFILE:-}" = "w07-bounded" ]; then
  workload_profile=w07-bounded
fi
if [ "${MOUNT_RS_FOUNDATIONDB_IOPS:-0}" = "1" ]; then
  run_iops=1
  if [ "$run_napi" -ne 1 ]; then
    echo "MOUNT_RS_FOUNDATIONDB_IOPS=1 requires MOUNT_RS_FOUNDATIONDB_NAPI=1" >&2
    exit 2
  fi
fi
run_service_benchmark=${MOUNT_RS_FOUNDATIONDB_SERVICE_BENCHMARK:-0}
case "$run_service_benchmark" in
  0|1) ;;
  *) echo "MOUNT_RS_FOUNDATIONDB_SERVICE_BENCHMARK must be 0 or 1" >&2; exit 2 ;;
esac
if [ "$run_service_benchmark" -eq 1 ]; then
  if [ -n "${RUSTFS_ENDPOINT:-}" ] || [ "$run_napi" -eq 1 ] || [ "${MOUNT_RS_FOUNDATIONDB_NATIVE_CLI:-0}" = "1" ] || [ -n "${MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE:-}" ]; then
    echo "The service benchmark requires an owned standalone FoundationDB cluster" >&2
    exit 2
  fi
  : "${MOUNT_RS_FOUNDATIONDB_BENCH_OUTPUT_DIR:?set a retained benchmark output directory}"
  mkdir -p "$MOUNT_RS_FOUNDATIONDB_BENCH_OUTPUT_DIR"
  benchmark_output_dir=$(CDPATH= cd -- "$MOUNT_RS_FOUNDATIONDB_BENCH_OUTPUT_DIR" && pwd)
  benchmark_prepare_only=${MOUNT_RS_FOUNDATIONDB_BENCH_PREPARE_ONLY:-0}
  case "$benchmark_prepare_only" in
    0|1) ;;
    *) echo "MOUNT_RS_FOUNDATIONDB_BENCH_PREPARE_ONLY must be 0 or 1" >&2; exit 2 ;;
  esac
  mkdir -p "$benchmark_output_dir/build-cache"
fi
run_native_cli=0
if [ "${MOUNT_RS_FOUNDATIONDB_NATIVE_CLI:-0}" = "1" ]; then
  run_native_cli=1
fi
run_id="$(date +%s)-$$"
network="mount-rs-foundationdb-net-$run_id"
server="mount-rs-foundationdb-server-$run_id"
server1="$server"
server2="mount-rs-foundationdb-server-$run_id-2"
server3="mount-rs-foundationdb-server-$run_id-3"
fdb_servers="$server"
fdb_volumes=""
client_container=""
authority_heartbeat_container=""
authority_heartbeat_log=""
probe_key="mount-rs/foundationdb/readiness/$run_id"
probe_value="ready"
probe_key_written=0
topology=${MOUNT_RS_FOUNDATIONDB_TOPOLOGY:-single}
case "$topology" in
  single) fdb_server_count=1 ;;
  durable) fdb_server_count=3 ;;
  *)
    echo "MOUNT_RS_FOUNDATIONDB_TOPOLOGY must be single or durable" >&2
    exit 2
    ;;
esac
soak_rounds=${MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS:-0}
case "$soak_rounds" in
  ''|*[!0-9]*)
    echo "MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS must be a non-negative integer" >&2
    exit 2
    ;;
esac
if [ "$soak_rounds" -gt 100 ]; then
  echo "MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS must not exceed 100" >&2
  exit 2
fi
if [ "$soak_rounds" -gt 0 ] && [ -z "${RUSTFS_ENDPOINT:-}" ]; then
  echo "MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS requires the composed RustFS lane" >&2
  exit 2
fi
provided_cluster_file=${MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE:-}
external_network=${MOUNT_RS_FOUNDATIONDB_NETWORK:-}
external_server=${MOUNT_RS_FOUNDATIONDB_SERVER_CONTAINER:-}
owns_server=1
owns_network=1
external_mode=0
temp_root=${TMPDIR:-/tmp}
case "$temp_root" in
  /) ;;
  *) temp_root=${temp_root%/} ;;
esac
run_dir=$(mktemp -d "$temp_root/mount-rs-foundationdb.XXXXXX")
cleanup_status=0
rustfs_network_connected=0

cleanup() {
  exit_status=$?
  trap - EXIT INT TERM
  if [ "$probe_key_written" -eq 1 ]; then
    docker exec "$server" fdbcli --exec "writemode on; clear $probe_key" >/dev/null 2>&1 || true
  fi
  if [ -n "$client_container" ] && docker container inspect "$client_container" >/dev/null 2>&1; then
    docker rm --force "$client_container" >/dev/null 2>&1 || cleanup_status=1
  fi
  if [ -n "$authority_heartbeat_container" ] \
    && docker container inspect "$authority_heartbeat_container" >/dev/null 2>&1; then
    docker rm --force "$authority_heartbeat_container" >/dev/null 2>&1 || cleanup_status=1
  fi
  if [ "${MOUNT_RS_FOUNDATIONDB_KEEP:-0}" = "1" ]; then
    echo "FOUNDATIONDB_KEEP=1 topology=$topology servers=$fdb_servers volumes=${fdb_volumes:-none} network=$network data=$run_dir" >&2
    exit "$exit_status"
  fi
  if [ "$owns_server" -eq 1 ]; then
    for fdb_server in $fdb_servers; do
      if docker container inspect "$fdb_server" >/dev/null 2>&1; then
        docker rm --force "$fdb_server" >/dev/null 2>&1 || cleanup_status=1
      fi
    done
  fi
  if [ "$rustfs_network_connected" -eq 1 ] && [ -n "${RUSTFS_HARNESS_CONTAINER:-}" ] \
    && docker network inspect "$network" >/dev/null 2>&1; then
    docker network disconnect --force "$network" "$RUSTFS_HARNESS_CONTAINER" >/dev/null 2>&1 || cleanup_status=1
  fi
  if [ "$owns_network" -eq 1 ] && docker network inspect "$network" >/dev/null 2>&1; then
    docker network rm "$network" >/dev/null 2>&1 || cleanup_status=1
  fi
  for fdb_volume in $fdb_volumes; do
    if docker volume inspect "$fdb_volume" >/dev/null 2>&1; then
      volume_owner=$(docker volume inspect --format '{{index .Labels "mount-rs.foundationdb.run"}}' "$fdb_volume" 2>/dev/null || true)
      if [ "$volume_owner" = "$run_id" ]; then
        docker volume rm "$fdb_volume" >/dev/null 2>&1 || cleanup_status=1
      else
        echo "Refusing cleanup of FoundationDB volume without this run's ownership label: $fdb_volume" >&2
        cleanup_status=1
      fi
    fi
  done
  case "$run_dir" in
    "$temp_root"/mount-rs-foundationdb.??????) rm -rf "$run_dir" || cleanup_status=1 ;;
    *) echo "Refusing cleanup of unexpected FoundationDB temp path: $run_dir" >&2; cleanup_status=1 ;;
  esac
  if [ "$cleanup_status" -ne 0 ] && [ "$exit_status" -eq 0 ]; then
    exit_status=1
  fi
  exit "$exit_status"
}
trap cleanup EXIT INT TERM

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  echo "FoundationDB test requires an already-running Docker daemon" >&2
  exit 2
fi

docker_arch=$(docker info --format '{{.Architecture}}')
case "$docker_arch" in
  amd64|x86_64)
    docker_platform=linux/amd64
    default_fdb_image='foundationdb/foundationdb:7.4.7@sha256:c13aed110fe17c6feb678f5dadc73dbcc2dbc42f2a3e952a877c116f63f052d3'
    ;;
  arm64|aarch64)
    docker_platform=linux/arm64
    default_fdb_image='foundationdb/foundationdb:7.4.7@sha256:25d9e2456ca923e70829fc07a15132c1a0c061474cd89826cdc4bbb45de870de'
    ;;
  *) echo "Unsupported Docker architecture for FoundationDB gate: $docker_arch" >&2; exit 2 ;;
esac
if [ -n "$fdb_image_override" ]; then
  fdb_image=$fdb_image_override
else
  # Use the official platform manifest rather than the multi-arch index. This
  # keeps older Docker Desktop engines from re-resolving the index during
  # container creation while preserving a digest pin for each architecture.
  fdb_image=$default_fdb_image
fi
resource_label="mount-rs.foundationdb.run=$run_id"
created_network=0
network_subnet=""
network_prefix=""
fdb_ip1=""
fdb_ip2=""
fdb_ip3=""
fdb_cluster_file_contents=""

create_foundationdb_network() {
  if [ "$topology" = "single" ] && [ "$run_service_benchmark" -eq 0 ]; then
    docker network create --label "$resource_label" "$network" >/dev/null
    created_network=1
    return 0
  fi

  network_candidate_seed=$(printf '%s' "$run_id" | cksum | awk '{print ($1 % 240) + 1}')
  network_candidate_attempt=0
  while [ "$network_candidate_attempt" -lt 240 ]; do
    network_candidate_octet=$(( (network_candidate_seed + network_candidate_attempt - 1) % 240 + 1 ))
    network_subnet="172.31.$network_candidate_octet.0/24"
    network_error="$run_dir/network-create.err"
    if docker network create --driver bridge --subnet "$network_subnet" \
      --label "$resource_label" "$network" > /dev/null 2>"$network_error"; then
      created_network=1
      break
    fi
    if grep -Eiq 'overlap|address space' "$network_error"; then
      network_candidate_attempt=$((network_candidate_attempt + 1))
      continue
    fi
    sed -n '1,6p' "$network_error" >&2 || true
    echo "Could not create the isolated FoundationDB network" >&2
    return 1
  done
  if [ "$created_network" -ne 1 ]; then
    echo "Exhausted isolated FoundationDB subnet candidates" >&2
    return 1
  fi

  network_prefix="172.31.$network_candidate_octet"
  fdb_ip1="$network_prefix.10"
  fdb_ip2="$network_prefix.11"
  fdb_ip3="$network_prefix.12"
  cluster_id=$(printf '%s' "$run_id" | tr -cd '[:alnum:]')
  if [ "$topology" = "single" ]; then
    fdb_cluster_file_contents="mount_rs:$cluster_id@$fdb_ip1:4500"
  else
    fdb_cluster_file_contents="mount_rs:$cluster_id@$fdb_ip1:4500,$fdb_ip2:4500,$fdb_ip3:4500"
  fi
}

if [ -n "$provided_cluster_file" ]; then
  if [ "${MOUNT_RS_FOUNDATIONDB_ALLOW_EXTERNAL_CLUSTER:-0}" != "1" ]; then
    echo "Refusing externally supplied FoundationDB cluster; set MOUNT_RS_FOUNDATIONDB_ALLOW_EXTERNAL_CLUSTER=1 explicitly" >&2
    exit 2
  fi
  if [ ! -f "$provided_cluster_file" ] || [ -L "$provided_cluster_file" ]; then
    echo "External FoundationDB cluster file must be a regular non-symlink file: $provided_cluster_file" >&2
    exit 2
  fi
  if [ -z "$external_network" ] || [ -z "$external_server" ]; then
    echo "External FoundationDB mode requires MOUNT_RS_FOUNDATIONDB_NETWORK and MOUNT_RS_FOUNDATIONDB_SERVER_CONTAINER" >&2
    exit 2
  fi
  if ! docker network inspect "$external_network" >/dev/null 2>&1; then
    echo "External FoundationDB network does not exist: $external_network" >&2
    exit 2
  fi
  if ! docker container inspect "$external_server" >/dev/null 2>&1; then
    echo "External FoundationDB server container does not exist: $external_server" >&2
    exit 2
  fi
  if [ -z "${MOUNT_RS_FOUNDATIONDB_TEST_PREFIX:-}" ] && [ -z "${RUSTFS_COMBO_PREFIX:-}" ]; then
    echo "External FoundationDB mode requires MOUNT_RS_FOUNDATIONDB_TEST_PREFIX or RUSTFS_COMBO_PREFIX" >&2
    exit 2
  fi
  external_mode=1
  network="$external_network"
  server="$external_server"
  fdb_servers="$server"
  owns_server=0
  owns_network=0
fi

docker pull --platform "$docker_platform" "$fdb_image" >/dev/null
if [ "$run_service_benchmark" -eq 1 ] && docker image inspect "$rust_image" >/dev/null 2>&1; then
  echo "FOUNDATIONDB_BENCH_CLIENT_IMAGE cached=$rust_image"
else
  docker pull --platform "$docker_platform" "$rust_image" >/dev/null
fi
if [ "$run_napi" -eq 1 ]; then
  docker pull --platform "$docker_platform" "$node_image" >/dev/null
fi
expected_fdb_image_id=$(docker image inspect --format '{{.Id}}' "$fdb_image")
if [ "$external_mode" -eq 1 ]; then
  external_fdb_image_id=$(docker inspect --format '{{.Image}}' "$server")
  if [ "$external_fdb_image_id" != "$expected_fdb_image_id" ]; then
    echo "External FoundationDB server/client image mismatch: server=$external_fdb_image_id client=$expected_fdb_image_id" >&2
    exit 2
  fi
  if ! docker network inspect --format '{{range $id, $container := .Containers}}{{println $container.Name}}{{end}}' "$network" \
    | grep -Fxq "$server"; then
    echo "External FoundationDB server is not attached to $network: $server" >&2
    exit 2
  fi
  cp "$provided_cluster_file" "$run_dir/fdb.cluster"
else
  create_foundationdb_network
  if [ "$topology" = "single" ] && [ "$run_service_benchmark" -eq 0 ]; then
    fdb_servers="$server"
    docker run --detach --platform "$docker_platform" \
      --name "$server" --hostname fdb --network "$network" \
      --env FDB_NETWORKING_MODE=container --env FDB_PORT=4500 \
      --env FDB_CLUSTER_FILE=/var/fdb/fdb.cluster \
      --entrypoint /var/fdb/scripts/fdb_single.bash \
      "$fdb_image" >/dev/null
  else
    fdb_servers=""
    fdb_number=1
    while [ "$fdb_number" -le "$fdb_server_count" ]; do
      case "$fdb_number" in
        1) fdb_server="$server1"; fdb_ip="$fdb_ip1" ;;
        2) fdb_server="$server2"; fdb_ip="$fdb_ip2" ;;
        3) fdb_server="$server3"; fdb_ip="$fdb_ip3" ;;
        *) echo "Unsupported FoundationDB server number: $fdb_number" >&2; exit 2 ;;
      esac
      fdb_volume="mount-rs-foundationdb-$run_id-fdb$fdb_number-data"
      docker volume create --label "$resource_label" "$fdb_volume" >/dev/null
      fdb_volumes="$fdb_volumes $fdb_volume"
      fdb_servers="$fdb_servers $fdb_server"
      docker run --detach --platform "$docker_platform" \
        --name "$fdb_server" --hostname "fdb$fdb_number" \
        --network "$network" --ip "$fdb_ip" --network-alias "fdb$fdb_number" \
        --volume "$fdb_volume:/var/fdb/data" \
        --env FDB_NETWORKING_MODE=container --env FDB_PORT=4500 \
        --env "FDB_PUBLIC_IP=$fdb_ip" \
        --env FDB_CLUSTER_FILE=/var/fdb/fdb.cluster \
        --env "FDB_CLUSTER_FILE_CONTENTS=$fdb_cluster_file_contents" \
        --entrypoint /var/fdb/scripts/fdb.bash \
        "$fdb_image" >/dev/null
      fdb_number=$((fdb_number + 1))
    done
  fi
fi

configure_durable_foundationdb() {
  redundancy=double
  if [ "$topology" = "single" ]; then redundancy=single; fi
  ticks=0
  while :; do
    all_running=1
    for fdb_server in $fdb_servers; do
      if ! docker inspect --format '{{.State.Running}}' "$fdb_server" 2>/dev/null | grep -q '^true$'; then
        all_running=0
        break
      fi
    done
    if [ "$all_running" -eq 1 ] && docker exec "$server" fdbcli --exec "configure new $redundancy ssd" >"$run_dir/configure.log" 2>&1; then
      echo "FOUNDATIONDB_CONFIGURED topology=$topology redundancy=$redundancy storage=ssd servers=$fdb_servers"
      return 0
    fi
    if [ "$all_running" -eq 1 ] && docker exec "$server" fdbcli --exec 'status json' >"$run_dir/status.json" 2>"$run_dir/status.err"; then
      echo "FOUNDATIONDB_CONFIGURED topology=$topology redundancy=$redundancy storage=ssd servers=$fdb_servers"
      return 0
    fi
    if [ "$ticks" -ge 90 ]; then
      docker logs --tail 160 "$server" >&2 || true
      cat "$run_dir/configure.log" >&2 || true
      cat "$run_dir/status.err" >&2 || true
      echo "Timed out configuring the durable FoundationDB cluster" >&2
      return 1
    fi
    sleep 1
    ticks=$((ticks + 1))
  done
}

if [ "$external_mode" -eq 0 ] && { [ "$topology" = "durable" ] || [ "$run_service_benchmark" -eq 1 ]; }; then
  configure_durable_foundationdb
fi

wait_for_foundationdb() {
  ticks=0
  while :; do
    if docker exec "$server" fdbcli --exec 'status json' >"$run_dir/status.json" 2>"$run_dir/status.err"; then
      probe_log="$run_dir/transaction-probe.log"
      probe_get="$run_dir/transaction-probe-get.log"
      if docker exec "$server" fdbcli --exec "writemode on; set $probe_key $probe_value" >"$probe_log" 2>&1 \
        && grep -Fq 'Committed' "$probe_log" \
        && docker exec "$server" fdbcli --exec "get $probe_key" >"$probe_get" 2>&1 \
        && grep -Fq "$probe_value" "$probe_get" \
        && docker exec "$server" fdbcli --exec "writemode on; clear $probe_key" >>"$probe_log" 2>&1 \
        && grep -Fq 'Committed' "$probe_log"; then
        probe_key_written=0
        echo "FOUNDATIONDB_TRANSACTION_READY server=$server"
        return 0
      fi
      probe_key_written=1
      echo "FoundationDB status is available but its transaction probe is not ready; retrying" >&2
      tail -40 "$probe_log" >&2 || true
      tail -40 "$probe_get" >&2 || true
    fi
    if [ "$ticks" -ge 90 ]; then
      docker logs --tail 160 "$server" >&2 || true
      cat "$run_dir/status.err" >&2 || true
      echo "Timed out waiting for the isolated FoundationDB cluster" >&2
      return 1
    fi
    if ! docker inspect --format '{{.State.Running}}' "$server" 2>/dev/null | grep -q '^true$'; then
      docker logs --tail 160 "$server" >&2 || true
      echo "FoundationDB server exited before readiness" >&2
      return 1
    fi
    sleep 1
    ticks=$((ticks + 1))
  done
}

wait_for_foundationdb

if [ "$run_service_benchmark" -eq 1 ]; then
  # The readiness transaction alone cannot distinguish the memory engine.
  # Verify actual configuration before any provider/client workloads begin.
  docker exec "$server" fdbcli --exec 'status json' >"$run_dir/benchmark-status.json"
  benchmark_identity=$(python3 "$repo_dir/scripts/foundationdb-benchmark-identity.py" \
    "$run_dir/benchmark-status.json" "$topology" "$fdb_image" \
    "$benchmark_output_dir/foundationdb-cluster-identity-$run_id.json")
  echo "FOUNDATIONDB_BENCH_SSD_VERIFIED $benchmark_identity"
fi

if [ "$external_mode" -eq 0 ]; then
  docker cp "$server:/var/fdb/fdb.cluster" "$run_dir/fdb.cluster"
  docker cp "$server:/usr/lib/libfdb_c.so" "$run_dir/libfdb_c.so"
  if [ "$run_service_benchmark" -eq 1 ]; then
    docker cp "$server:/usr/bin/fdbcli" "$run_dir/fdbcli"
  fi
  echo "FOUNDATIONDB_IMAGE_MATCH server=$expected_fdb_image_id client=$expected_fdb_image_id"
else
  client_container="mount-rs-foundationdb-client-$run_id"
  docker create --platform "$docker_platform" --name "$client_container" "$fdb_image" >/dev/null
  docker cp "$client_container:/usr/lib/libfdb_c.so" "$run_dir/libfdb_c.so"
  docker rm "$client_container" >/dev/null
  client_container=""
  echo "FOUNDATIONDB_IMAGE_MATCH server=$external_fdb_image_id client=$expected_fdb_image_id"
fi
export MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE="$run_dir/fdb.cluster"
echo "FOUNDATIONDB_READY platform=$docker_platform server=$server cluster=$run_dir/fdb.cluster"

if [ -n "${RUSTFS_ENDPOINT:-}" ]; then
  if [ "$external_mode" -eq 1 ]; then
    echo "FoundationDB + RustFS service-restart gate requires an owned FoundationDB server; refusing external mode" >&2
    exit 2
  fi
  rustfs_network_alias="mount-rs-rustfs"
  if [ -n "${RUSTFS_HARNESS_CONTAINER:-}" ]; then
    if ! docker container inspect "$RUSTFS_HARNESS_CONTAINER" >/dev/null 2>&1; then
      echo "RustFS harness container does not exist: $RUSTFS_HARNESS_CONTAINER" >&2
      exit 2
    fi
    if ! docker network connect --alias "$rustfs_network_alias" "$network" "$RUSTFS_HARNESS_CONTAINER"; then
      echo "Could not attach RustFS harness container to the FoundationDB client network" >&2
      exit 1
    fi
    rustfs_network_connected=1
    rustfs_endpoint="http://$rustfs_network_alias:9000"
    echo "FOUNDATIONDB_RUSTFS_NETWORK_READY alias=$rustfs_network_alias container=$RUSTFS_HARNESS_CONTAINER"
  else
    case "$RUSTFS_ENDPOINT" in
      http://127.0.0.1:*) rustfs_endpoint="http://host.docker.internal${RUSTFS_ENDPOINT#http://127.0.0.1}" ;;
      http://localhost:*) rustfs_endpoint="http://host.docker.internal${RUSTFS_ENDPOINT#http://localhost}" ;;
      http://host.docker.internal:*) rustfs_endpoint="$RUSTFS_ENDPOINT" ;;
      *) echo "Refusing non-local RustFS endpoint in composed gate: $RUSTFS_ENDPOINT" >&2; exit 2 ;;
    esac
  fi
  test_manifest=tests/foundationdb/Cargo.toml
  # The first composed client proves the split ChunkedFs path and the provider
  # contract against the same real cluster. A second client runs after the
  # owned FoundationDB container is restarted below.
  test_command="set -e; cargo check --locked -p mount-rs-sdk -p mount-rs-cli -p mount-rs-napi --features mount-rs-sdk/foundationdb,mount-rs-cli/foundationdb,mount-rs-napi/foundationdb && cargo test --manifest-path tests/foundationdb/Cargo.toml --locked --lib foundationdb_rustfs_chunked_composition -- --exact --nocapture && cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test foundationdb -- --nocapture && cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test inode -- --ignored --nocapture"
  test_prefix=${RUSTFS_COMBO_PREFIX:?RUSTFS_COMBO_PREFIX must be set for the composed gate}
  : "${RUSTFS_BUCKET:?RUSTFS_BUCKET must be set for the composed gate}"
  : "${RUSTFS_ACCESS_KEY_ID:?RUSTFS_ACCESS_KEY_ID must be set for the composed gate}"
  : "${RUSTFS_SECRET_ACCESS_KEY:?RUSTFS_SECRET_ACCESS_KEY must be set for the composed gate}"
  : "${RUSTFS_REGION:?RUSTFS_REGION must be set for the composed gate}"
else
  rustfs_endpoint=""
  test_manifest=providers/mount-rs-foundationdb/Cargo.toml
  test_command="set -e; cargo check --locked -p mount-rs-sdk -p mount-rs-cli -p mount-rs-napi --features mount-rs-sdk/foundationdb,mount-rs-cli/foundationdb,mount-rs-napi/foundationdb && cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test foundationdb -- --nocapture && cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test inode -- --ignored --nocapture"
  if [ "$external_mode" -eq 1 ]; then
    test_prefix=${MOUNT_RS_FOUNDATIONDB_TEST_PREFIX:-mount-rs/foundationdb-external/$run_id}
  else
    test_prefix="mount-rs/foundationdb/$run_id"
  fi
fi

if [ "$run_service_benchmark" -eq 1 ]; then
  benchmark_read_only=${MOUNT_RS_FOUNDATIONDB_READ_ONLY:-0}
  case "$benchmark_read_only" in
    0|1) ;;
    *) echo "MOUNT_RS_FOUNDATIONDB_READ_ONLY must be 0 or 1" >&2; exit 2 ;;
  esac
  benchmark_service_features=saturation-foundationdb,io-profiling
  if [ "${MOUNT_RS_RESOURCE_PROFILE:-0}" = "1" ]; then
    benchmark_service_features="$benchmark_service_features,resource-profiling"
  fi
  if [ "${MOUNT_RS_TRACE_ALLOCATIONS:-0}" = "1" ]; then
    benchmark_service_features="$benchmark_service_features,allocation-profiling"
  fi
  # Keep real provider contracts in this lane, while avoiding unrelated CLI
  # and N-API builds. Each benchmark invocation owns a separate test process.
  test_command="set -e; if ! cargo clippy --version >/dev/null 2>&1; then rustup component add clippy; fi; cargo clippy --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --all-targets --features foundationdb -- -D warnings && cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test foundationdb -- --nocapture && cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test delegation -- --ignored --nocapture && cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test inode -- --ignored --nocapture"
  if [ "${MOUNT_RS_FOUNDATIONDB_DIRECT_BASELINE:-0}" = "1" ]; then
    test_command="${test_command} && MOUNT_RS_FOUNDATIONDB_DIRECT_OUTPUT=/artifacts/foundationdb-direct-block.json cargo test --release --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test block_datastore_saturation -- --ignored --nocapture --test-threads=1"
  elif [ "$benchmark_prepare_only" -eq 1 ]; then
    test_command="${test_command} && cargo test --release --locked -p mount-rs-service --features $benchmark_service_features --test quic_tidb_saturation --no-run"
  elif [ -n "${MOUNT_RS_REMOTE_SCALING_SERVERS:-}" ]; then
    test_command="${test_command} && sh scripts/bench-remote-scaling.sh foundationdb /artifacts/scaling"
  else
    test_command="${test_command} && MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=read MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=${MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS:-1,2,4,8,16} MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT=/artifacts/foundationdb-read.json cargo test --release --locked -p mount-rs-service --features $benchmark_service_features --test quic_tidb_saturation -- --ignored --nocapture --test-threads=1"
    if [ "$benchmark_read_only" -eq 0 ]; then
      test_command="${test_command} && MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=read,write MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=${MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS:-1,2} MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT=/artifacts/foundationdb-read-write.json cargo test --release --locked -p mount-rs-service --features $benchmark_service_features --test quic_tidb_saturation -- --ignored --nocapture --test-threads=1"
    fi
  fi
fi

# The terminal network test runs in its own process after the ordinary
# provider contract, so stopping the one-shot native client cannot affect
# subsequent authority or application tests.
test_command="${test_command} && cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test network_shutdown terminal_shutdown_drains_the_native_network_and_rejects_new_connections -- --exact --nocapture"

if [ "$run_native_cli" -eq 1 ] && [ -z "$rustfs_endpoint" ]; then
  echo "MOUNT_RS_FOUNDATIONDB_NATIVE_CLI requires the composed RustFS lane" >&2
  exit 2
fi
authority_prefix=""
if [ -n "$rustfs_endpoint" ] || [ "$run_native_cli" -eq 1 ] || [ "$run_napi" -eq 1 ]; then
  if [ -z "$test_prefix" ]; then
    test_prefix="mount-rs/foundationdb/$run_id"
  fi
  authority_prefix=${MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX:-}
  if [ -z "$authority_prefix" ]; then
    authority_prefix="$test_prefix/lease-authority"
  fi
  if [ "$run_native_cli" -eq 1 ] || [ "$run_napi" -eq 1 ]; then
    test_command="${test_command} && cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test foundationdb publish_foundationdb_authority_for_consumers -- --exact --nocapture"
    if [ "$run_native_cli" -eq 1 ]; then
      test_command="${test_command} && MOUNT_RS_CLI_NATIVE_FOUNDATIONDB=1 MOUNT_RS_CLI_FOUNDATIONDB_SHARED_PROVIDER=1 cargo test --locked -p mount-rs-cli --features foundationdb --test native_lifecycle cli_foundationdb_rustfs_config_binary_mounts_and_reopens -- --ignored --exact --nocapture && echo FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse"
    fi
  fi
fi

start_authority_heartbeat() {
  authority_heartbeat_seconds=${MOUNT_RS_FOUNDATIONDB_AUTHORITY_HEARTBEAT_SECONDS:-1800}
  case "$authority_heartbeat_seconds" in
    ''|*[!0-9]*|0)
      echo "MOUNT_RS_FOUNDATIONDB_AUTHORITY_HEARTBEAT_SECONDS must be a positive integer" >&2
      exit 2
      ;;
  esac
  if [ "$authority_heartbeat_seconds" -gt 3600 ]; then
    echo "MOUNT_RS_FOUNDATIONDB_AUTHORITY_HEARTBEAT_SECONDS must not exceed 3600" >&2
    exit 2
  fi
  authority_heartbeat_container="mount-rs-foundationdb-authority-$run_id"
  authority_heartbeat_log="$run_dir/authority-heartbeat.log"
  docker run --detach --platform "$docker_platform" \
    --name "$authority_heartbeat_container" --label "$resource_label" \
    --network "$network" \
    --volume "$repo_dir:/workspace:ro" \
    --volume "$run_dir:/fdb:ro" \
    --workdir /workspace \
    --env "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/fdb/fdb.cluster" \
    --env LIBRARY_PATH=/fdb \
    --env LD_LIBRARY_PATH=/fdb \
    --env RUSTFLAGS=-Lnative=/fdb \
    --env CARGO_TARGET_DIR=/tmp/mount-rs-foundationdb-authority-target \
    --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX=$authority_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_HEARTBEAT_SECONDS=$authority_heartbeat_seconds" \
    "$rust_image" sh -c \
    'export PATH=/usr/local/cargo/bin:$PATH
     apt-get update -qq
     apt-get install -y -qq --no-install-recommends clang libclang-dev >/dev/null
     exec cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test foundationdb foundationdb_authority_heartbeat -- --ignored --exact --nocapture' \
    >/dev/null

  heartbeat_ticks=0
  while :; do
    docker logs "$authority_heartbeat_container" >"$authority_heartbeat_log" 2>&1 || true
    if grep -Fq 'FOUNDATIONDB_AUTHORITY_HEARTBEAT_READY' "$authority_heartbeat_log"; then
      echo "FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING container=$authority_heartbeat_container interval_seconds=30 max_forward_jump_seconds=120 duration_seconds=$authority_heartbeat_seconds"
      return 0
    fi
    if ! docker inspect --format '{{.State.Running}}' "$authority_heartbeat_container" 2>/dev/null | grep -q '^true$'; then
      cat "$authority_heartbeat_log" >&2 || true
      echo "FoundationDB authority heartbeat exited before readiness" >&2
      exit 1
    fi
    if [ "$heartbeat_ticks" -ge 300 ]; then
      cat "$authority_heartbeat_log" >&2 || true
      echo "Timed out waiting for the FoundationDB authority heartbeat" >&2
      exit 1
    fi
    sleep 1
    heartbeat_ticks=$((heartbeat_ticks + 1))
  done
}

check_authority_heartbeat() {
  [ -n "$authority_heartbeat_container" ] || return 0
  if docker inspect --format '{{.State.Running}}' "$authority_heartbeat_container" 2>/dev/null | grep -q '^true$'; then
    return 0
  fi
  docker logs "$authority_heartbeat_container" >&2 2>&1 || true
  echo "FoundationDB authority heartbeat is no longer running" >&2
  return 1
}

if [ -n "$authority_prefix" ]; then
  start_authority_heartbeat
fi

native_mount_args=""
if [ "$run_native_cli" -eq 1 ]; then
  native_mount_args="--device /dev/fuse --cap-add SYS_ADMIN --security-opt apparmor:unconfined"
fi

napi_build_prefix=""
if [ "$run_napi" -eq 1 ]; then
  # Build the feature-enabled N-API artifact in the same pinned client image
  # that supplied libfdb_c. The source remains read-only; only the explicitly
  # owned run directory receives the temporary .node copy.
  napi_build_prefix='cargo build --locked --release -p mount-rs-napi --features foundationdb && test -f /tmp/mount-rs-foundationdb-target/release/libmount_rs_napi.so && cp /tmp/mount-rs-foundationdb-target/release/libmount_rs_napi.so /fdb/mount-rs.linux-x64-gnu.node && '
  test_command="${napi_build_prefix}${test_command}"
fi
if [ "$soak_rounds" -gt 0 ]; then
  # Each round gets an independent FoundationDB volume prefix and RustFS block
  # prefix. The default composed lane deliberately defers cleanup until its
  # post-restart client; the repeated qualification rounds clean themselves so
  # a bounded soak cannot turn into unbounded test-bucket growth.
  soak_test_command='round=1
  while [ "$round" -le "$MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS" ]; do
    round_prefix="${RUSTFS_COMBO_PREFIX}/soak-${round}"
    round_authority_prefix="${round_prefix}/lease-authority"
    if ! env -u MOUNT_RS_FOUNDATIONDB_DEFER_CLEANUP \
      RUSTFS_COMBO_PREFIX="$round_prefix" \
      MOUNT_RS_FOUNDATIONDB_TEST_PREFIX="$round_prefix" \
      MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX="$round_authority_prefix" \
      cargo test --manifest-path tests/foundationdb/Cargo.toml --locked --lib foundationdb_rustfs_chunked_composition -- --exact --nocapture
    then
      echo "FOUNDATIONDB_SOAK_FAIL round=$round" >&2
      exit 1
    fi
    round=$((round + 1))
  done
  echo "FOUNDATIONDB_SOAK_PASS rounds=$MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS"'
  # Keep the loop in a command group. Appending a multiline while-loop
  # directly after && would let a preceding native CLI failure be masked by
  # the loop's final echo and incorrectly emit FOUNDATIONDB_TEST_PASS.
  test_command="${test_command} && { ${soak_test_command}; }"
fi
client_fdb_volume="$run_dir:/fdb:ro"
if [ "$run_napi" -eq 1 ]; then
  client_fdb_volume="$run_dir:/fdb"
fi

# The Rust image is the disposable client/build environment. clang/libclang
# are installed inside it for bindgen; no host package or native client is
# changed. The source tree is read-only and the target directory is ephemeral.
#
# Keep every Docker option as its own shell argument. In particular, repository
# and temporary paths may contain spaces. Secret credentials are propagated by
# environment variable name, never embedded in Docker's argv or the command
# string shown by process diagnostics. The test command is passed as a
# positional argument to the container shell for the same reason.
if [ "$run_service_benchmark" -eq 1 ]; then
  docker run --rm \
    --platform "$docker_platform" \
    --network "$network" \
    --volume "$repo_dir:/workspace:ro" \
    --volume "$client_fdb_volume" \
    --volume "$benchmark_output_dir:/artifacts" \
    --volume "$benchmark_output_dir/build-cache:/tmp/mount-rs-foundationdb-target" \
    --workdir /workspace \
    --env MOUNT_RS_REMOTE_SATURATION_PROVIDER=foundationdb \
    --env "MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES=${MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES:-0}" \
    --env "MOUNT_RS_REMOTE_SCALING_SERVERS=${MOUNT_RS_REMOTE_SCALING_SERVERS:-}" \
    --env "MOUNT_RS_REMOTE_SCALING_CLIENTS=${MOUNT_RS_REMOTE_SCALING_CLIENTS:-100}" \
    --env MOUNT_RS_REMOTE_SATURATION_ACTIVE_CLIENTS \
    --env "MOUNT_RS_REMOTE_SATURATION_CONNECTION_LIMIT=${MOUNT_RS_REMOTE_SATURATION_CONNECTION_LIMIT:-128}" \
    --env "MOUNT_RS_REMOTE_SATURATION_SETUP_CONCURRENCY=${MOUNT_RS_REMOTE_SATURATION_SETUP_CONCURRENCY:-100}" \
    --env "MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=${MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS:-1}" \
    --env MOUNT_RS_PROFILE_IO=1 \
    --env "MOUNT_RS_RESOURCE_PROFILE=${MOUNT_RS_RESOURCE_PROFILE:-0}" \
    --env "MOUNT_RS_TRACE_ALLOCATIONS=${MOUNT_RS_TRACE_ALLOCATIONS:-0}" \
    --env "MOUNT_RS_DATASTORE_STAGE_OBSERVER=${MOUNT_RS_DATASTORE_STAGE_OBSERVER:-}" \
    --env MOUNT_RS_FOUNDATIONDB_COUNTER_DIR=/artifacts/counters \
    --env CARGO_BUILD_JOBS=2 \
    --env "MOUNT_RS_FOUNDATIONDB_BENCH_IDENTITY=$benchmark_identity" \
    --env "MOUNT_RS_FOUNDATIONDB_TOPOLOGY=$topology" \
    --env MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=15 \
    --env MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS=3 \
    --env MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS=32 \
    --env MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=0 \
    --env "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/fdb/fdb.cluster" \
    --env "MOUNT_RS_FOUNDATIONDB_TEST_PREFIX=$test_prefix" \
    --env LIBRARY_PATH=/fdb \
    --env LD_LIBRARY_PATH=/fdb \
    --env RUSTFLAGS=-Lnative=/fdb \
    --env CARGO_TARGET_DIR=/tmp/mount-rs-foundationdb-target \
    "$rust_image" sh -c \
    'export PATH=/usr/local/cargo/bin:$PATH
     set -e
     if ! command -v clang >/dev/null 2>&1 || ! ldconfig -p | grep -q libclang; then
       apt-get update -qq
       apt-get install -y -qq --no-install-recommends clang libclang-dev >/dev/null
     fi
     exec sh -c "$1"' \
    mount-rs-foundationdb-service-benchmark "$test_command"
elif [ -n "$rustfs_endpoint" ]; then
  docker run --rm \
    $native_mount_args \
    --platform "$docker_platform" \
    --network "$network" \
    --add-host host.docker.internal:host-gateway \
    --volume "$repo_dir:/workspace:ro" \
    --volume "$client_fdb_volume" \
    --workdir /workspace \
    --env "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/fdb/fdb.cluster" \
    --env LIBRARY_PATH=/fdb \
    --env LD_LIBRARY_PATH=/fdb \
    --env RUSTFLAGS=-Lnative=/fdb \
    --env CARGO_TARGET_DIR=/tmp/mount-rs-foundationdb-target \
    --env "R2_ENDPOINT=$rustfs_endpoint" \
    --env R2_BUCKET \
    --env R2_ACCESS_KEY_ID \
    --env R2_SECRET_ACCESS_KEY \
    --env "RUSTFS_ENDPOINT=$rustfs_endpoint" \
    --env RUSTFS_BUCKET \
    --env RUSTFS_ACCESS_KEY_ID \
    --env RUSTFS_SECRET_ACCESS_KEY \
    --env RUSTFS_REGION \
    --env "RUSTFS_COMBO_PREFIX=$test_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_TEST_PREFIX=$test_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX=$authority_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS=$soak_rounds" \
    --env MOUNT_RS_FOUNDATIONDB_NATIVE_CLI \
    --env MOUNT_RS_FOUNDATIONDB_DEFER_CLEANUP=1 \
    "$rust_image" sh -c \
    'export PATH=/usr/local/cargo/bin:$PATH
     apt-get update -qq
     packages="clang libclang-dev curl"
     if [ "${MOUNT_RS_FOUNDATIONDB_NATIVE_CLI:-0}" = "1" ]; then
       packages="$packages fuse3"
     fi
     apt-get install -y -qq --no-install-recommends $packages >/dev/null
     endpoint_status=$(curl --silent --show-error --connect-timeout 2 --max-time 5 \
       -o /dev/null -w "%{http_code}" "$R2_ENDPOINT" || true)
     if [ "$endpoint_status" = "000" ]; then
       echo "FoundationDB client container could not reach the composed block endpoint" >&2
       exit 1
     fi
     echo "FOUNDATIONDB_BLOCK_ENDPOINT_REACHABLE status=$endpoint_status"
     exec sh -c "$1"' \
    mount-rs-foundationdb-client "$test_command"
else
  docker run --rm \
    $native_mount_args \
    --platform "$docker_platform" \
    --network "$network" \
    --add-host host.docker.internal:host-gateway \
    --volume "$repo_dir:/workspace:ro" \
    --volume "$client_fdb_volume" \
    --workdir /workspace \
    --env "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/fdb/fdb.cluster" \
    --env LIBRARY_PATH=/fdb \
    --env LD_LIBRARY_PATH=/fdb \
    --env RUSTFLAGS=-Lnative=/fdb \
    --env CARGO_TARGET_DIR=/tmp/mount-rs-foundationdb-target \
    --env "MOUNT_RS_FOUNDATIONDB_TEST_PREFIX=$test_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX=$authority_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_SOAK_ROUNDS=$soak_rounds" \
    --env MOUNT_RS_FOUNDATIONDB_NATIVE_CLI \
    "$rust_image" sh -c \
    'export PATH=/usr/local/cargo/bin:$PATH
     apt-get update -qq
     packages="clang libclang-dev"
     if [ "${MOUNT_RS_FOUNDATIONDB_NATIVE_CLI:-0}" = "1" ]; then
       packages="$packages fuse3"
     fi
     apt-get install -y -qq --no-install-recommends $packages >/dev/null
     exec sh -c "$1"' \
    mount-rs-foundationdb-client "$test_command"
fi

check_authority_heartbeat

if [ "$run_napi" -eq 1 ]; then
  check_authority_heartbeat
  # R2 can be a composed Ozone S3 gateway without being a RustFS service.
  # The Node client runs in its own container, so translate a host-published
  # loopback endpoint independently of the RustFS restart gate above.
  node_r2_endpoint=${R2_ENDPOINT:-}
  case "$node_r2_endpoint" in
    http://127.0.0.1:*) node_r2_endpoint="http://host.docker.internal${node_r2_endpoint#http://127.0.0.1}" ;;
    http://localhost:*) node_r2_endpoint="http://host.docker.internal${node_r2_endpoint#http://localhost}" ;;
  esac
  node_command='node bindings/mount-rs-napi/test/foundationdb.mjs'
  if [ "$run_iops" -eq 1 ]; then
    iops_size_mib=${MOUNT_RS_FOUNDATIONDB_IOPS_SIZE_MIB:-1}
    iops_payload_bytes=${MOUNT_RS_FOUNDATIONDB_IOPS_PAYLOAD_BYTES:-4096}
    iops_iterations=${MOUNT_RS_FOUNDATIONDB_IOPS_ITERATIONS:-400}
    iops_concurrency=${MOUNT_RS_FOUNDATIONDB_IOPS_CONCURRENCY:-64}
    iops_minimum=${MOUNT_RS_FOUNDATIONDB_IOPS_MIN:-1000}
    for iops_value in "$iops_size_mib" "$iops_payload_bytes" "$iops_iterations" "$iops_concurrency" "$iops_minimum"; do
      case "$iops_value" in
        ''|*[!0-9]*|0)
          echo "FoundationDB IOPS settings must be positive integers" >&2
          exit 2
        ;;
      esac
    done
    if [ "$iops_payload_bytes" != "4096" ] || [ "$iops_iterations" != "400" ] || [ "$iops_concurrency" != "64" ]; then
      echo "FoundationDB IOPS qualification requires payload=4096 iterations=400 concurrency=64" >&2
      exit 2
    fi
    if [ "$workload_profile" != "w07-bounded" ] && [ "$iops_minimum" -lt 1000 ]; then
      echo "FoundationDB IOPS qualification minimum must be at least 1000" >&2
      exit 2
    fi
    node_command="$node_command && node benchmarks/storage/runner.mjs --providers mount-rs-split-foundationdb-r2 --sizes $iops_size_mib --payload-bytes $iops_payload_bytes --iterations $iops_iterations --concurrency $iops_concurrency --min-iops $iops_minimum --require-configured --network-context ozone-ci --output /fdb/foundationdb-ozone-iops.json"
  fi
  napi_status=0
  if [ -n "$rustfs_endpoint" ]; then
    if foundationdb_napi_run_client "$rustfs_endpoint" \
      --platform "$docker_platform" \
      --network "$network" \
      --add-host host.docker.internal:host-gateway \
      --volume "$repo_dir:/workspace:ro" \
      --volume "$client_fdb_volume" \
      --workdir /workspace \
      --env "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/fdb/fdb.cluster" \
      --env LD_LIBRARY_PATH=/fdb \
      --env NAPI_RS_NATIVE_LIBRARY_PATH=/fdb/mount-rs.linux-x64-gnu.node \
      --env MOUNT_RS_PROFILE_IO \
      --env MOUNT_RS_NAPI_FOUNDATIONDB=1 \
      --env MOUNT_RS_NAPI_FOUNDATIONDB_SHARED_PROVIDER=1 \
      --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX=$authority_prefix" \
      --env "MOUNT_RS_FOUNDATIONDB_NODE_PREFIX=$test_prefix/napi" \
      --env "RUSTFS_ENDPOINT=$rustfs_endpoint" \
      --env RUSTFS_BUCKET \
      --env RUSTFS_ACCESS_KEY_ID \
      --env RUSTFS_SECRET_ACCESS_KEY \
      "$node_image" sh -c "$node_command"; then
      :
    else
      napi_status=$?
    fi
  else
    if foundationdb_napi_run_client "$node_r2_endpoint" \
      --platform "$docker_platform" \
      --network "$network" \
      --add-host host.docker.internal:host-gateway \
      --volume "$repo_dir:/workspace:ro" \
      --volume "$client_fdb_volume" \
      --workdir /workspace \
      --env "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/fdb/fdb.cluster" \
      --env LD_LIBRARY_PATH=/fdb \
      --env NAPI_RS_NATIVE_LIBRARY_PATH=/fdb/mount-rs.linux-x64-gnu.node \
      --env MOUNT_RS_PROFILE_IO \
      --env MOUNT_RS_NAPI_FOUNDATIONDB=1 \
      --env MOUNT_RS_NAPI_FOUNDATIONDB_SHARED_PROVIDER=1 \
      --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX=$authority_prefix" \
      --env "MOUNT_RS_FOUNDATIONDB_NODE_PREFIX=$test_prefix/napi" \
      "$node_image" sh -c "$node_command"; then
      :
    else
      napi_status=$?
    fi
  fi
  if [ "$run_iops" -eq 1 ]; then
    iops_output=${MOUNT_RS_FOUNDATIONDB_IOPS_OUTPUT:-$run_dir/foundationdb-ozone-iops.json}
    case "$iops_output" in
      /*) host_iops_output=$iops_output ;;
      *) host_iops_output="$repo_dir/$iops_output" ;;
    esac
    if [ -f "$run_dir/foundationdb-ozone-iops.json" ]; then
      mkdir -p "$(dirname "$host_iops_output")"
      cp "$run_dir/foundationdb-ozone-iops.json" "$host_iops_output"
      if [ "$napi_status" -eq 0 ] && [ "$workload_profile" != "w07-bounded" ]; then
        node "$repo_dir/scripts/verify-w26-ozone-iops-artifact.mjs" \
          --output "$host_iops_output" \
          --providers mount-rs-split-foundationdb-r2 \
          --minimum-iops "$iops_minimum"
        echo "FOUNDATIONDB_OZONE_IOPS_PASS provider=foundationdb-r2 target=$iops_minimum output=$host_iops_output"
      elif [ "$napi_status" -eq 0 ] && [ "$workload_profile" = "w07-bounded" ]; then
        node "$repo_dir/scripts/verify-w07-workload-artifact.mjs" \
          --output "$host_iops_output"
      fi
    else
      echo "FOUNDATIONDB_OZONE_IOPS_ARTIFACT_MISSING path=$run_dir/foundationdb-ozone-iops.json" >&2
      [ "$napi_status" -ne 0 ] || napi_status=1
    fi
  fi
  if [ "$napi_status" -ne 0 ]; then
    exit "$napi_status"
  fi
  check_authority_heartbeat
  echo "FOUNDATIONDB_NAPI_PASS image=$node_image"
fi

if [ -n "$rustfs_endpoint" ]; then
  check_authority_heartbeat
  restart_timeout=${MOUNT_RS_FOUNDATIONDB_RESTART_TIMEOUT_SECONDS:-120}
  case "$restart_timeout" in
    ''|*[!0-9]*)
      echo "MOUNT_RS_FOUNDATIONDB_RESTART_TIMEOUT_SECONDS must be a non-negative integer" >&2
      exit 2
      ;;
  esac
  restart_server="$server"
  if [ "$topology" = "durable" ]; then
    # Keep the three-coordinator majority available while one replicated
    # storage/transaction node is restarted.
    restart_server="$server2"
  fi
  if ! python3 "$repo_dir/scripts/rustfs-bounded-docker.py" "$restart_timeout" \
    foundationdb-service-restart docker restart "$restart_server" >/dev/null; then
    echo "Could not restart the owned FoundationDB service container" >&2
    exit 1
  fi
  wait_for_foundationdb
  echo "FOUNDATIONDB_SERVICE_RESTART_READY topology=$topology server=$restart_server"
  check_authority_heartbeat

  # The server-local fdbcli probe above can succeed while a separate client's
  # copied cluster descriptor is stale or its network transaction path is not
  # ready. Assert the exact descriptor and a transaction from another isolated
  # container before treating restart as a consumer-ready event.
  docker cp "$server:/var/fdb/fdb.cluster" "$run_dir/fdb.cluster.after-restart"
  if ! cmp -s "$run_dir/fdb.cluster" "$run_dir/fdb.cluster.after-restart"; then
    echo "FoundationDB cluster descriptor changed across restart; copied client descriptor is stale" >&2
    exit 1
  fi
  client_probe_log="$run_dir/restart-client-probe.log"
  probe_key_written=1
  if ! docker run --rm --platform "$docker_platform" --network "$network" \
    --volume "$run_dir:/fdb:ro" --entrypoint /usr/bin/fdbcli "$fdb_image" \
    -C /fdb/fdb.cluster --exec "writemode on; set $probe_key $probe_value" \
    >"$client_probe_log" 2>&1 || ! grep -Fq 'Committed' "$client_probe_log"; then
    cat "$client_probe_log" >&2
    echo "FoundationDB restarted server passed local probe but separate client transaction failed" >&2
    exit 1
  fi
  if ! docker exec "$server" fdbcli --exec "writemode on; clear $probe_key" >"$client_probe_log" 2>&1 \
    || ! grep -Fq 'Committed' "$client_probe_log"; then
    cat "$client_probe_log" >&2
    echo "FoundationDB could not clear this run's restart transaction probe" >&2
    exit 1
  fi
  probe_key_written=0
  echo "FOUNDATIONDB_RESTART_CLIENT_TRANSACTION_READY topology=$topology"

  restart_test_command="cargo test --manifest-path providers/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test foundationdb publish_foundationdb_authority_for_consumers -- --exact --nocapture && cargo test --manifest-path tests/foundationdb/Cargo.toml --locked --lib foundationdb_rustfs_chunked_restart_reopen -- --exact --nocapture"
  docker run --rm \
    --platform "$docker_platform" \
    --network "$network" \
    --add-host host.docker.internal:host-gateway \
    --volume "$repo_dir:/workspace:ro" \
    --volume "$run_dir:/fdb:ro" \
    --workdir /workspace \
    --env "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/fdb/fdb.cluster" \
    --env LIBRARY_PATH=/fdb \
    --env LD_LIBRARY_PATH=/fdb \
    --env RUSTFLAGS=-Lnative=/fdb \
    --env CARGO_TARGET_DIR=/tmp/mount-rs-foundationdb-target \
    --env "R2_ENDPOINT=$rustfs_endpoint" \
    --env R2_BUCKET \
    --env R2_ACCESS_KEY_ID \
    --env R2_SECRET_ACCESS_KEY \
    --env "RUSTFS_ENDPOINT=$rustfs_endpoint" \
    --env RUSTFS_BUCKET \
    --env RUSTFS_ACCESS_KEY_ID \
    --env RUSTFS_SECRET_ACCESS_KEY \
    --env RUSTFS_REGION \
    --env "RUSTFS_COMBO_PREFIX=$test_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_TEST_PREFIX=$test_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX=$authority_prefix" \
    "$rust_image" sh -c \
    'export PATH=/usr/local/cargo/bin:$PATH
     apt-get update -qq
     apt-get install -y -qq --no-install-recommends clang libclang-dev >/dev/null
     exec sh -c "$1"' \
    mount-rs-foundationdb-restart-client "$restart_test_command"
fi

if [ -n "$rustfs_endpoint" ]; then
  check_authority_heartbeat
  echo "FOUNDATIONDB_TEST_PASS topology=$topology manifests=$test_manifest+providers/mount-rs-foundationdb/Cargo.toml platform=$docker_platform service_restart=pass soak_rounds=$soak_rounds"
else
  echo "FOUNDATIONDB_TEST_PASS topology=$topology manifest=$test_manifest platform=$docker_platform soak_rounds=$soak_rounds"
fi
