#!/bin/sh
set -eu

# Run the real FoundationDB provider gate without installing a native client on
# the host. When called by scripts/test-rustfs.sh as a combo command, the RustFS
# endpoint is passed through to a separate Rust client container.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fdb_image=${MOUNT_RS_FOUNDATIONDB_IMAGE:-foundationdb/foundationdb:7.4.7@sha256:7f1ce47f7f636351540423144a583141c255a4b314147972855e5388060f7677}
rust_image=${MOUNT_RS_FOUNDATIONDB_RUST_IMAGE:-rust:1.95-bookworm}
node_image=${MOUNT_RS_FOUNDATIONDB_NODE_IMAGE:-node:24-bookworm}
run_napi=0
if [ "${MOUNT_RS_FOUNDATIONDB_NAPI:-0}" = "1" ]; then
  run_napi=1
fi
run_native_cli=0
if [ "${MOUNT_RS_FOUNDATIONDB_NATIVE_CLI:-0}" = "1" ]; then
  run_native_cli=1
fi
run_id="$(date +%s)-$$"
network="mount-rs-foundationdb-net-$run_id"
server="mount-rs-foundationdb-server-$run_id"
client_container=""
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

cleanup() {
  exit_status=$?
  trap - EXIT INT TERM
  if [ -n "$client_container" ] && docker container inspect "$client_container" >/dev/null 2>&1; then
    docker rm --force "$client_container" >/dev/null 2>&1 || cleanup_status=1
  fi
  if [ "${MOUNT_RS_FOUNDATIONDB_KEEP:-0}" = "1" ]; then
    echo "FOUNDATIONDB_KEEP=1 server=$server network=$network data=$run_dir" >&2
    exit "$exit_status"
  fi
  if [ "$owns_server" -eq 1 ] && docker container inspect "$server" >/dev/null 2>&1; then
    docker rm --force "$server" >/dev/null 2>&1 || cleanup_status=1
  fi
  if [ "$owns_network" -eq 1 ] && docker network inspect "$network" >/dev/null 2>&1; then
    docker network rm "$network" >/dev/null 2>&1 || cleanup_status=1
  fi
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
  amd64|x86_64) docker_platform=linux/amd64 ;;
  arm64|aarch64) docker_platform=linux/arm64 ;;
  *) echo "Unsupported Docker architecture for FoundationDB gate: $docker_arch" >&2; exit 2 ;;
esac

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
  owns_server=0
  owns_network=0
fi

docker pull --platform "$docker_platform" "$fdb_image" >/dev/null
docker pull --platform "$docker_platform" "$rust_image" >/dev/null
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
  docker network create "$network" >/dev/null
  docker run --detach --platform "$docker_platform" \
    --name "$server" --hostname fdb --network "$network" \
    --env FDB_NETWORKING_MODE=container --env FDB_PORT=4500 \
    --env FDB_CLUSTER_FILE=/var/fdb/fdb.cluster \
    --entrypoint /var/fdb/scripts/fdb_single.bash \
    "$fdb_image" >/dev/null
fi

wait_for_foundationdb() {
  ticks=0
  while :; do
    if docker exec "$server" fdbcli --exec 'status json' >"$run_dir/status.json" 2>"$run_dir/status.err"; then
      return 0
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

if [ "$external_mode" -eq 0 ]; then
  docker cp "$server:/var/fdb/fdb.cluster" "$run_dir/fdb.cluster"
  docker cp "$server:/usr/lib/libfdb_c.so" "$run_dir/libfdb_c.so"
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

if [ -n "${R2_ENDPOINT:-}" ]; then
  if [ "$external_mode" -eq 1 ]; then
    echo "FoundationDB + RustFS service-restart gate requires an owned FoundationDB server; refusing external mode" >&2
    exit 2
  fi
  case "$R2_ENDPOINT" in
    http://127.0.0.1:*) rustfs_endpoint="http://host.docker.internal${R2_ENDPOINT#http://127.0.0.1}" ;;
    http://localhost:*) rustfs_endpoint="http://host.docker.internal${R2_ENDPOINT#http://localhost}" ;;
    http://host.docker.internal:*) rustfs_endpoint="$R2_ENDPOINT" ;;
    *) echo "Refusing non-local RustFS endpoint in composed gate: $R2_ENDPOINT" >&2; exit 2 ;;
  esac
  test_manifest=tests/foundationdb/Cargo.toml
  # The first composed client proves the split ChunkedFs path and the provider
  # contract against the same real cluster. A second client runs after the
  # owned FoundationDB container is restarted below.
  test_command="cargo check --locked -p mount-rs-sdk -p mount-rs-cli -p mount-rs-napi --features mount-rs-sdk/foundationdb,mount-rs-cli/foundationdb,mount-rs-napi/foundationdb && cargo test --manifest-path tests/foundationdb/Cargo.toml --locked --lib foundationdb_rustfs_chunked_composition -- --exact --nocapture && cargo test --manifest-path integrations/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test foundationdb -- --nocapture"
  test_prefix=${RUSTFS_COMBO_PREFIX:?RUSTFS_COMBO_PREFIX must be set for the composed gate}
  : "${R2_BUCKET:?R2_BUCKET must be set for the composed gate}"
  : "${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID must be set for the composed gate}"
  : "${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY must be set for the composed gate}"
else
  rustfs_endpoint=""
  test_manifest=integrations/mount-rs-foundationdb/Cargo.toml
  test_command="cargo check --locked -p mount-rs-sdk -p mount-rs-cli -p mount-rs-napi --features mount-rs-sdk/foundationdb,mount-rs-cli/foundationdb,mount-rs-napi/foundationdb && cargo test --manifest-path integrations/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test foundationdb -- --nocapture"
  if [ "$external_mode" -eq 1 ]; then
    test_prefix=${MOUNT_RS_FOUNDATIONDB_TEST_PREFIX:-mount-rs/foundationdb-external/$run_id}
  else
    test_prefix=""
  fi
fi

if [ "$run_native_cli" -eq 1 ] && [ -z "$rustfs_endpoint" ]; then
  echo "MOUNT_RS_FOUNDATIONDB_NATIVE_CLI requires the composed RustFS lane" >&2
  exit 2
fi
authority_prefix=""
if [ "$run_native_cli" -eq 1 ] || [ "$run_napi" -eq 1 ]; then
  authority_prefix=${MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX:-}
  if [ -z "$authority_prefix" ]; then
    authority_prefix="$test_prefix/lease-authority"
  fi
  test_command="${test_command} && cargo test --manifest-path integrations/mount-rs-foundationdb/Cargo.toml --locked --features foundationdb --test foundationdb publish_foundationdb_authority_for_consumers -- --exact --nocapture && MOUNT_RS_CLI_NATIVE_FOUNDATIONDB=1 MOUNT_RS_CLI_FOUNDATIONDB_SHARED_PROVIDER=1 cargo test --locked -p mount-rs-cli --features foundationdb --test native_lifecycle cli_foundationdb_rustfs_config_binary_mounts_and_reopens -- --ignored --exact --nocapture"
fi
native_mount_args=""
if [ "$run_native_cli" -eq 1 ]; then
  native_mount_args="--device /dev/fuse --cap-add SYS_ADMIN"
fi

napi_build_prefix=""
if [ "$run_napi" -eq 1 ]; then
  # Build the feature-enabled N-API artifact in the same pinned client image
  # that supplied libfdb_c. The source remains read-only; only the explicitly
  # owned run directory receives the temporary .node copy.
  napi_build_prefix='cargo build --locked --release -p mount-rs-napi --features foundationdb && test -f /tmp/mount-rs-foundationdb-target/release/libmount_rs_napi.so && cp /tmp/mount-rs-foundationdb-target/release/libmount_rs_napi.so /fdb/mount-rs.linux-x64-gnu.node && '
  test_command="${napi_build_prefix}${test_command}"
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
if [ -n "$rustfs_endpoint" ]; then
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
    --env "RUSTFS_COMBO_PREFIX=$test_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_TEST_PREFIX=$test_prefix" \
    --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX=$authority_prefix" \
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

if [ "$run_napi" -eq 1 ]; then
  if [ -n "$rustfs_endpoint" ]; then
    docker run --rm \
      --platform "$docker_platform" \
      --network "$network" \
      --add-host host.docker.internal:host-gateway \
      --volume "$repo_dir:/workspace:ro" \
      --volume "$run_dir:/fdb:ro" \
      --workdir /workspace \
      --env "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/fdb/fdb.cluster" \
      --env LD_LIBRARY_PATH=/fdb \
      --env NAPI_RS_NATIVE_LIBRARY_PATH=/fdb/mount-rs.linux-x64-gnu.node \
      --env MOUNT_RS_NAPI_FOUNDATIONDB=1 \
      --env MOUNT_RS_NAPI_FOUNDATIONDB_SHARED_PROVIDER=1 \
      --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX=$authority_prefix" \
      --env "R2_ENDPOINT=$rustfs_endpoint" \
      --env R2_BUCKET \
      --env R2_ACCESS_KEY_ID \
      --env R2_SECRET_ACCESS_KEY \
      "$node_image" node integrations/mount-rs-napi/test/foundationdb.mjs
  else
    docker run --rm \
      --platform "$docker_platform" \
      --network "$network" \
      --volume "$repo_dir:/workspace:ro" \
      --volume "$run_dir:/fdb:ro" \
      --workdir /workspace \
      --env "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/fdb/fdb.cluster" \
      --env LD_LIBRARY_PATH=/fdb \
      --env NAPI_RS_NATIVE_LIBRARY_PATH=/fdb/mount-rs.linux-x64-gnu.node \
      --env MOUNT_RS_NAPI_FOUNDATIONDB=1 \
      --env MOUNT_RS_NAPI_FOUNDATIONDB_SHARED_PROVIDER=1 \
      --env "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX=$authority_prefix" \
      "$node_image" node integrations/mount-rs-napi/test/foundationdb.mjs
  fi
  echo "FOUNDATIONDB_NAPI_PASS image=$node_image"
fi

if [ -n "$rustfs_endpoint" ]; then
  restart_timeout=${MOUNT_RS_FOUNDATIONDB_RESTART_TIMEOUT_SECONDS:-120}
  case "$restart_timeout" in
    ''|*[!0-9]*)
      echo "MOUNT_RS_FOUNDATIONDB_RESTART_TIMEOUT_SECONDS must be a non-negative integer" >&2
      exit 2
      ;;
  esac
  if ! python3 "$repo_dir/scripts/rustfs-bounded-docker.py" "$restart_timeout" \
    foundationdb-service-restart docker restart "$server" >/dev/null; then
    echo "Could not restart the owned FoundationDB service container" >&2
    exit 1
  fi
  wait_for_foundationdb
  echo "FOUNDATIONDB_SERVICE_RESTART_READY server=$server"

  restart_test_command="cargo test --manifest-path tests/foundationdb/Cargo.toml --locked --lib foundationdb_rustfs_chunked_restart_reopen -- --exact --nocapture"
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
  echo "FOUNDATIONDB_TEST_PASS manifests=$test_manifest+integrations/mount-rs-foundationdb/Cargo.toml platform=$docker_platform service_restart=pass"
else
  echo "FOUNDATIONDB_TEST_PASS manifest=$test_manifest platform=$docker_platform"
fi
