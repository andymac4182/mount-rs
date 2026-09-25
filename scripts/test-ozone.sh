#!/bin/sh
set -eu

# Apache Ozone 2.2.1 is released under Apache License 2.0. These
# architecture-specific GHCR digests are published by the official
# apache/ozone-docker project; do not replace them with a floating tag.
# Digest source: https://github.com/apache/ozone-docker/pkgs/container/ozone
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/foundationdb-napi-fixture-env.sh"
. "$repo_dir/scripts/cargo-shared-env.sh"
ozone_image_repository="ghcr.io/apache/ozone"
ozone_version="2.2.1"
ozone_license="Apache-2.0"
ozone_bucket="mount-rs-ozone-test"
ozone_access_key="mount-rs-ozone-test"
ozone_secret_key="mount-rs-ozone-test-secret"
ozone_region="us-east-1"
ozone_startup_timeout=${MOUNT_RS_OZONE_STARTUP_TIMEOUT_SECONDS:-180}
ozone_action_timeout=${MOUNT_RS_OZONE_ACTION_TIMEOUT_SECONDS:-30}
ozone_client_timeout=${MOUNT_RS_OZONE_CLIENT_TIMEOUT_SECONDS:-10}
ozone_stop_timeout=${MOUNT_RS_OZONE_STOP_TIMEOUT_SECONDS:-30}
ozone_test_timeout=${MOUNT_RS_OZONE_TEST_TIMEOUT_SECONDS:-600}
ozone_composition_timeout=${MOUNT_RS_OZONE_COMPOSITION_TIMEOUT_SECONDS:-1200}
ozone_composition_stop_grace=${MOUNT_RS_OZONE_COMPOSITION_STOP_GRACE_SECONDS:-30}
ozone_publish_host=127.0.0.1
if [ "${MOUNT_RS_OZONE_FOUNDATIONDB_COMPOSITION:-0}" = "1" ]; then
  if [ "${MOUNT_RS_FOUNDATIONDB_NAPI:-0}" = "1" ]; then
    validate_foundationdb_napi_fixture_provider ozone
  fi
  # The FoundationDB child runs in a Docker client container and reaches the
  # host-published gateway through host.docker.internal. Keep the default
  # contract loopback-only, but make this explicit opt-in composition
  # reachable from the Docker bridge on Linux as well as Docker Desktop.
  ozone_publish_host=0.0.0.0
fi

validate_positive_timeout() {
  timeout_value=$1
  timeout_name=$2
  case "$timeout_value" in
    ''|*[!0-9]*)
      echo "$timeout_name must be a positive integer" >&2
      exit 2
      ;;
  esac
  if [ "$timeout_value" -le 0 ]; then
    echo "$timeout_name must be a positive integer" >&2
    exit 2
  fi
}

validate_positive_timeout "$ozone_startup_timeout" MOUNT_RS_OZONE_STARTUP_TIMEOUT_SECONDS
validate_positive_timeout "$ozone_action_timeout" MOUNT_RS_OZONE_ACTION_TIMEOUT_SECONDS
validate_positive_timeout "$ozone_client_timeout" MOUNT_RS_OZONE_CLIENT_TIMEOUT_SECONDS
validate_positive_timeout "$ozone_stop_timeout" MOUNT_RS_OZONE_STOP_TIMEOUT_SECONDS
validate_positive_timeout "$ozone_test_timeout" MOUNT_RS_OZONE_TEST_TIMEOUT_SECONDS
validate_positive_timeout "$ozone_composition_timeout" MOUNT_RS_OZONE_COMPOSITION_TIMEOUT_SECONDS
validate_positive_timeout "$ozone_composition_stop_grace" MOUNT_RS_OZONE_COMPOSITION_STOP_GRACE_SECONDS

bounded_docker_command_for_timeout() {
  bounded_timeout=$1
  action_name=$2
  shift 2
  case "$bounded_timeout" in
    ''|*[!0-9]*)
      echo "Ozone Docker action timeout must be a non-negative integer" >&2
      return 2
      ;;
  esac
  python3 "$repo_dir/scripts/rustfs-bounded-docker.py" \
    "$bounded_timeout" \
    "$action_name" \
    "$@"
}

bounded_docker_command() {
  bounded_docker_command_for_timeout "$ozone_action_timeout" "$@"
}

bounded_docker_startup_command() {
  bounded_docker_command_for_timeout "$ozone_startup_timeout" "$@"
}

bounded_process_command_for_timeout() {
  bounded_timeout=$1
  action_name=$2
  shift 2
  python3 "$repo_dir/scripts/rustfs-bounded-docker.py" \
    "$bounded_timeout" \
    "$action_name" \
    "$@"
}

begin_startup_budget() {
  ozone_startup_deadline=$(($(date +%s) + ozone_startup_timeout))
}

startup_budget_expired() {
  [ "$(date +%s)" -ge "$ozone_startup_deadline" ]
}

begin_stop_budget() {
  ozone_stop_deadline=$(($(date +%s) + ozone_stop_timeout))
}

stop_budget_expired() {
  [ "$(date +%s)" -ge "$ozone_stop_deadline" ]
}

if ! command -v docker >/dev/null 2>&1; then
  echo "Apache Ozone test requires Docker; Docker was not found." >&2
  exit 2
fi
for command_name in cargo curl date mktemp node python3 rm sed sleep; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Apache Ozone test requires $command_name" >&2
    exit 2
  fi
done

docker_arch=""
if docker_arch=$(bounded_docker_startup_command "daemon-info" docker info --format '{{.Architecture}}'); then
  :
else
  echo "Apache Ozone test requires an already-running Docker daemon." >&2
  exit 2
fi
case "$docker_arch" in
  amd64|x86_64)
    ozone_platform="linux/amd64"
    ozone_digest="sha256:88cf042bc3b810a66a85ab3fcd7b1558a44bb9d914a28d64e30338ac6780b9a6"
    ;;
  arm64|aarch64)
    ozone_platform="linux/arm64"
    ozone_digest="sha256:c7ba6ee740323de7da970d8b8ea373d43c42fe22d31d72077092f5511c5d83ed"
    ;;
  *)
    echo "Unsupported Docker architecture for Apache Ozone 2.2.1: $docker_arch" >&2
    exit 2
    ;;
esac
ozone_image="$ozone_image_repository:$ozone_version-all-in-one@$ozone_digest"

if bounded_docker_startup_command "image-inspect" docker image inspect "$ozone_image" >/dev/null 2>&1; then
  :
else
  image_inspect_status=$?
  if [ "$image_inspect_status" -eq 124 ] || [ "$image_inspect_status" -eq 125 ]; then
    echo "Could not boundedly inspect the pinned Apache Ozone image" >&2
    exit 2
  fi
  if ! bounded_docker_startup_command "image-pull" docker pull --platform "$ozone_platform" "$ozone_image" >/dev/null 2>&1; then
    echo "Could not pull the pinned Apache Ozone image" >&2
    exit 2
  fi
fi

temp_root=${TMPDIR:-/tmp}
if [ "$temp_root" = "/" ]; then
  temp_pattern="/mount-rs-ozone.XXXXXX"
  run_dir_prefix="/mount-rs-ozone."
else
  temp_root=${temp_root%/}
  temp_pattern="$temp_root/mount-rs-ozone.XXXXXX"
  run_dir_prefix="$temp_root/mount-rs-ozone."
fi
run_dir=""
fixture_file=""
container_name=""
ownership_label="mount-rs-ozone-test"
ozone_startup_deadline=""
ozone_stop_deadline=""

inspect_owned_container() {
  inspect_output=""
  if inspect_output=$(bounded_docker_command "inspect-service" docker container inspect \
    --format '{{.Name}}|{{index .Config.Labels "com.mount-rs.ozone-test"}}|{{index .Config.Labels "com.mount-rs.ozone-test-run"}}' \
    "$container_name" 2>&1); then
    expected="/$container_name|$ownership_label|$container_name"
    if [ "$inspect_output" != "$expected" ]; then
      echo "Refusing to manage container with unexpected ownership: $inspect_output" >&2
      return 3
    fi
    return 0
  fi
  case "$inspect_output" in
    *"No such container"*|*"No such object"*) return 1 ;;
    *)
      echo "Could not inspect Apache Ozone test container $container_name: $inspect_output" >&2
      return 2
      ;;
  esac
}

show_logs() {
  bounded_docker_command "logs" docker logs --tail 160 "$container_name" >&2 || true
}

validate_run_dir() {
  [ -n "$run_dir" ] || return 1
  [ -d "$run_dir" ] || return 1
  [ ! -L "$run_dir" ] || return 1
  case "$run_dir" in
    "$run_dir_prefix"??????) ;;
    *) return 1 ;;
  esac
  [ -f "$run_dir/.mount-rs-ozone-owned" ] || return 1
  [ ! -L "$run_dir/.mount-rs-ozone-owned" ] || return 1
  [ "$(sed -n '1p' "$run_dir/.mount-rs-ozone-owned" 2>/dev/null)" = "$container_name" ] || return 1
}

remove_owned_run_dir() {
  if ! validate_run_dir; then
    echo "Refusing cleanup of unvalidated Apache Ozone temp path: ${run_dir:-<unset>}" >&2
    return 1
  fi
  if ! rm -rf "$run_dir"; then
    echo "Could not remove Apache Ozone temp path; preserving it: $run_dir" >&2
    return 1
  fi
  if [ -e "$run_dir" ] || [ -L "$run_dir" ]; then
    echo "Could not remove Apache Ozone temp path; preserving it: $run_dir" >&2
    return 1
  fi
  return 0
}

bounded_remove_container() {
  if inspect_owned_container; then
    :
  else
    inspect_status=$?
    [ "$inspect_status" -eq 1 ] && return 0
    return 1
  fi

  if bounded_docker_command "remove-service" docker container rm --force --volumes "$container_name" >/dev/null 2>&1; then
    remove_status=0
  else
    remove_status=$?
  fi
  if [ "$remove_status" -eq 124 ]; then
    echo "Timed out removing Apache Ozone container $container_name" >&2
  elif [ "$remove_status" -eq 125 ]; then
    echo "Could not reap Apache Ozone container removal process $container_name" >&2
  elif [ "$remove_status" -ne 0 ]; then
    echo "Apache Ozone container removal failed with status $remove_status" >&2
  fi

  if inspect_owned_container; then
    echo "Apache Ozone container still exists after bounded removal: $container_name" >&2
    return 1
  else
    inspect_status=$?
  fi
  if [ "$inspect_status" -ne 1 ]; then
    echo "Could not verify Apache Ozone container removal" >&2
    return 1
  fi
  [ "$remove_status" -ne 125 ]
}

cleanup() {
  exit_code=$1
  trap - EXIT INT TERM
  if [ "${MOUNT_RS_OZONE_KEEP:-0}" = "1" ]; then
    echo "OZONE_KEEP=1 container=$container_name endpoint=${ozone_endpoint:-unknown} data=${run_dir:-unknown}" >&2
    exit "$exit_code"
  fi

  cleanup_ok=1
  if inspect_owned_container; then
    if ! bounded_remove_container; then
      cleanup_ok=0
    fi
  else
    inspect_status=$?
    if [ "$inspect_status" -ne 1 ]; then
      cleanup_ok=0
    fi
  fi
  if [ "$cleanup_ok" -eq 1 ] && ! remove_owned_run_dir; then
    cleanup_ok=0
  fi
  if [ "$cleanup_ok" -ne 1 ]; then
    echo "OZONE_CLEANUP_INCOMPLETE container=$container_name data=${run_dir:-unknown}" >&2
    [ "$exit_code" -ne 0 ] || exit_code=1
  else
    echo "OZONE_CLEANUP_PASS container=$container_name" >&2
  fi
  exit "$exit_code"
}

run_dir=$(mktemp -d "$temp_pattern")
run_id=${run_dir##*/}
container_name="mount-rs-ozone-$run_id"
fixture_file="$run_dir/restart-fixture"
printf '%s\n' "$container_name" >"$run_dir/.mount-rs-ozone-owned"
trap 'cleanup "$?"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if inspect_owned_container; then
  echo "Apache Ozone test container name is already in use: $container_name" >&2
  exit 2
else
  inspect_status=$?
  if [ "$inspect_status" -ne 1 ]; then
    echo "Cannot safely reserve Apache Ozone test container name: $container_name" >&2
    exit 2
  fi
fi

begin_startup_budget
if bounded_docker_startup_command "run-service" docker run --detach --platform "$ozone_platform" \
  --name "$container_name" \
  --label "com.mount-rs.ozone-test=$ownership_label" \
  --label "com.mount-rs.ozone-test-run=$container_name" \
  --publish "$ozone_publish_host"::9878 \
  "$ozone_image" >/dev/null 2>&1; then
  :
else
  run_status=$?
  show_logs
  echo "Could not start Apache Ozone container with status $run_status" >&2
  exit 1
fi

refresh_endpoint() {
  ozone_port=""
  port_ticks=0
  while :; do
    if startup_budget_expired; then
      show_logs
      echo "Timed out waiting for Apache Ozone S3 Gateway port" >&2
      return 1
    fi
    port_output=""
    if port_output=$(bounded_docker_command "port" docker port "$container_name" 9878/tcp 2>&1); then
      :
    else
      port_status=$?
      if [ "$port_status" -eq 124 ] || [ "$port_status" -eq 125 ]; then
        echo "Could not boundedly query the Apache Ozone S3 Gateway port" >&2
        return 1
      fi
    fi
    ozone_port=$(printf '%s\n' "$port_output" \
      | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | sed -n '1p')
    if [ -n "$ozone_port" ]; then
      ozone_endpoint="http://127.0.0.1:$ozone_port"
      export R2_ENDPOINT="$ozone_endpoint"
      return 0
    fi
    sleep 1
    port_ticks=$((port_ticks + 1))
  done
}

container_is_running() {
  running_output=""
  if running_output=$(bounded_docker_command "inspect-running" docker inspect \
    --format '{{.State.Running}}' "$container_name" 2>/dev/null); then
    [ "$running_output" = "true" ]
    return $?
  else
    running_status=$?
  fi
  echo "Could not verify Apache Ozone container state (inspect status $running_status)" >&2
  return 2
}

wait_for_gateway() {
  gateway_ticks=0
  while :; do
    http_code=$(curl --silent --show-error --connect-timeout 1 --max-time 2 \
      -o /dev/null -w '%{http_code}' "$ozone_endpoint/" 2>/dev/null || true)
    if [ "$http_code" != "000" ]; then
      return 0
    fi
    if startup_budget_expired; then
      show_logs
      echo "Timed out waiting for Apache Ozone S3 Gateway at $ozone_endpoint" >&2
      return 1
    fi
    if container_is_running; then
      :
    else
      inspect_status=$?
      show_logs
      echo "Apache Ozone container exited before S3 Gateway readiness (inspect status $inspect_status)" >&2
      return 1
    fi
    if startup_budget_expired; then
      show_logs
      echo "Timed out waiting for Apache Ozone S3 Gateway at $ozone_endpoint" >&2
      return 1
    fi
    sleep 1
    gateway_ticks=$((gateway_ticks + 1))
  done
}

wait_for_stopped() {
  stopped_ticks=0
  while :; do
    if container_is_running; then
      if stop_budget_expired; then
        echo "Apache Ozone container remained running after stop" >&2
        return 1
      fi
    else
      inspect_status=$?
      [ "$inspect_status" -eq 1 ] && return 0
      echo "Could not verify Apache Ozone container stopped" >&2
      return 1
    fi
    if stop_budget_expired; then
      echo "Timed out waiting for Apache Ozone container to stop" >&2
      return 1
    fi
    sleep 1
    stopped_ticks=$((stopped_ticks + 1))
  done
}

bootstrap_bucket() {
  bucket_ticks=0
  bucket_log="$run_dir/bucket-bootstrap.log"
  while :; do
    if startup_budget_expired; then
      cat "$bucket_log" >&2 || true
      show_logs
      echo "Timed out waiting for Apache Ozone S3 bucket readiness" >&2
      return 1
    fi
    if bounded_process_command_for_timeout "$ozone_client_timeout" "ozone-bucket-bootstrap" \
      node "$repo_dir/tests/ozone/create-bucket.mjs" >"$bucket_log" 2>&1; then
      cat "$bucket_log"
      return 0
    fi
    bucket_status=$?
    if [ "$bucket_status" -eq 124 ] || [ "$bucket_status" -eq 125 ]; then
      cat "$bucket_log" >&2 || true
      echo "Apache Ozone bucket bootstrap exceeded its bounded client timeout" >&2
      return 1
    fi
    if startup_budget_expired; then
      cat "$bucket_log" >&2 || true
      show_logs
      echo "Timed out waiting for Apache Ozone S3 bucket readiness" >&2
      return 1
    fi
    if container_is_running; then
      :
    else
      cat "$bucket_log" >&2 || true
      show_logs
      echo "Apache Ozone container exited before S3 bucket readiness" >&2
      return 1
    fi
    if startup_budget_expired; then
      cat "$bucket_log" >&2 || true
      show_logs
      echo "Timed out waiting for Apache Ozone S3 bucket readiness" >&2
      return 1
    fi
    sleep 1
    bucket_ticks=$((bucket_ticks + 1))
  done
}

bounded_docker_action() {
  action_name=$1
  shift
  if bounded_docker_command "$action_name" "$@" >/dev/null 2>&1; then
    return 0
  else
    action_status=$?
  fi
  if [ "$action_status" -eq 124 ]; then
    echo "Timed out during Apache Ozone $action_name action" >&2
  elif [ "$action_status" -eq 125 ]; then
    echo "Could not reap Apache Ozone $action_name action process" >&2
  else
    echo "Apache Ozone $action_name action failed with status $action_status" >&2
  fi
  show_logs
  return 1
}

bounded_cargo_test() {
  test_name=$1
  shift
  if bounded_process_command_for_timeout "$ozone_test_timeout" "ozone-cargo-$test_name" \
    cargo test \
      --manifest-path "$repo_dir/tests/ozone/Cargo.toml" \
      --locked \
      -- "$test_name" --exact --test-threads=1 --nocapture "$@"; then
    return 0
  else
    test_status=$?
  fi
  if [ "$test_status" -eq 124 ] || [ "$test_status" -eq 125 ]; then
    echo "Apache Ozone cargo test exceeded its bounded timeout: $test_name" >&2
  else
    echo "Apache Ozone cargo test failed with status $test_status: $test_name" >&2
  fi
  return "$test_status"
}

bounded_ignored_cargo_test() {
  test_name=$1
  shift
  if bounded_process_command_for_timeout "$ozone_test_timeout" "ozone-cargo-$test_name" \
    cargo test \
      --manifest-path "$repo_dir/tests/ozone/Cargo.toml" \
      --locked \
      -- "$test_name" --exact --ignored --test-threads=1 --nocapture "$@"; then
    return 0
  else
    test_status=$?
  fi
  if [ "$test_status" -eq 124 ] || [ "$test_status" -eq 125 ]; then
    echo "Apache Ozone ignored cargo test exceeded its bounded timeout: $test_name" >&2
  else
    echo "Apache Ozone ignored cargo test failed with status $test_status: $test_name" >&2
  fi
  return "$test_status"
}

bounded_cli_ignored_cargo_test() {
  test_name=$1
  if bounded_process_command_for_timeout "$ozone_test_timeout" "ozone-cli-$test_name" \
    cargo test \
      --manifest-path "$repo_dir/apps/mount-rs-cli/Cargo.toml" \
      --locked \
      --test cli \
      -- "$test_name" --exact --ignored --test-threads=1 --nocapture; then
    return 0
  else
    test_status=$?
  fi
  if [ "$test_status" -eq 124 ] || [ "$test_status" -eq 125 ]; then
    echo "Apache Ozone Rust CLI test exceeded its bounded timeout: $test_name" >&2
  else
    echo "Apache Ozone Rust CLI test failed with status $test_status: $test_name" >&2
  fi
  return "$test_status"
}

bounded_cli_remote_http_test() {
  if bounded_process_command_for_timeout "$ozone_test_timeout" "ozone-cli-http-remote" \
    sh "$repo_dir/scripts/test-cli-remote-ozone.sh"; then
    return 0
  else
    test_status=$?
  fi
  if [ "$test_status" -eq 124 ] || [ "$test_status" -eq 125 ]; then
    echo "Apache Ozone remote HTTP CLI test exceeded its bounded timeout" >&2
  else
    echo "Apache Ozone remote HTTP CLI test failed with status $test_status" >&2
  fi
  return "$test_status"
}

bounded_node_test() {
  test_name=$1
  test_script=$2
  shift 2
  if bounded_process_command_for_timeout "$ozone_test_timeout" "ozone-node-$test_name" \
    node "$test_script" "$@"; then
    return 0
  else
    test_status=$?
  fi
  if [ "$test_status" -eq 124 ] || [ "$test_status" -eq 125 ]; then
    echo "Apache Ozone Node test exceeded its bounded timeout: $test_name" >&2
  else
    echo "Apache Ozone Node test failed with status $test_status: $test_name" >&2
  fi
  return "$test_status"
}

bounded_composition_script() {
  composition_name=$1
  composition_command=$2
  composition_pid_file="$run_dir/$composition_name.pid"
  if RUSTFS_COMBO_GROUP_STOP_SECONDS="$ozone_composition_stop_grace" \
    python3 "$repo_dir/scripts/rustfs-combo-runner.py" \
      "$ozone_composition_timeout" \
      "$repo_dir" \
      "$composition_pid_file" \
      "$composition_name" \
      "$composition_command"; then
    return 0
  else
    composition_status=$?
  fi
  if [ "$composition_status" -eq 124 ] || [ "$composition_status" -eq 125 ]; then
    echo "Apache Ozone composition exceeded its bounded timeout or cleanup grace: $composition_name" >&2
  else
    echo "Apache Ozone composition failed with status $composition_status: $composition_name" >&2
  fi
  return "$composition_status"
}

refresh_endpoint
export R2_BUCKET="$ozone_bucket"
export R2_ACCESS_KEY_ID="$ozone_access_key"
export R2_SECRET_ACCESS_KEY="$ozone_secret_key"
export OZONE_REGION="$ozone_region"
export OZONE_TEST_PREFIX="mount-rs-ozone/$run_id"
export OZONE_FIXTURE_FILE="$fixture_file"
export OZONE_IMAGE="$ozone_image"

wait_for_gateway
echo "OZONE_HEALTHY endpoint=$ozone_endpoint release=$ozone_version digest=$ozone_digest license=$ozone_license"
bootstrap_bucket
echo "OZONE_READY endpoint=$ozone_endpoint image=$ozone_image"

bounded_cargo_test "real_ozone_block_contract"

begin_stop_budget
bounded_docker_action "stop" docker stop --time=10 "$container_name" || {
  echo "Could not stop Apache Ozone container" >&2
  exit 1
}
wait_for_stopped
echo "OZONE_FAULT_WINDOW_PASS container=$container_name"

bounded_cargo_test "real_ozone_gateway_failure_is_bounded"

begin_startup_budget
bounded_docker_action "start" docker start "$container_name" || {
  echo "Could not restart Apache Ozone container" >&2
  exit 1
}
refresh_endpoint
wait_for_gateway
bootstrap_bucket
echo "OZONE_RESTART_READY endpoint=$ozone_endpoint"

bounded_cargo_test "real_ozone_reopen_after_service_restart"

if [ "${MOUNT_RS_OZONE_COMPOSITIONS:-0}" = "1" ]; then
  : "${MOUNT_RS_OZONE_COMPOSITION_HARNESS:?MOUNT_RS_OZONE_COMPOSITION_HARNESS must be set by scripts/test-ozone-compositions.sh}"
  : "${PGLITE_DATABASE_URL:?PGLITE_DATABASE_URL must be supplied by the Ozone composition harness}"
  : "${PGLITE_DATA_DIR:?PGLITE_DATA_DIR must be supplied by the Ozone composition harness}"
  : "${OZONE_SQLITE_METADATA_FILE:?OZONE_SQLITE_METADATA_FILE must be supplied by the Ozone composition harness}"
  export MOUNT_RS_OZONE_CHUNKED_SQLITE=1
  export MOUNT_RS_OZONE_CHUNKED_PGLITE=1
  bounded_ignored_cargo_test "chunked_composition::real_ozone_sqlite_chunked_composition"
  bounded_ignored_cargo_test "chunked_composition::real_ozone_pglite_chunked_composition"
  if [ "${MOUNT_RS_OZONE_NODE_COMPOSITION:-0}" = "1" ]; then
    export MOUNT_RS_PROVIDER_MATRIX_RUN_ID="$run_id"
    export RUSTFS_REGION="$ozone_region"
    bounded_cli_ignored_cargo_test "actual_binary_runs_live_ozone_split_provider_self_test"
    bounded_node_test "provider-matrix" "$repo_dir/tests/provider_matrix/node-sdk.mjs"
    bounded_node_test "cli" "$repo_dir/tests/ozone/node-cli.mjs"
    export MOUNT_RS_CLI_REMOTE_PREFIX="$OZONE_TEST_PREFIX/cli-http"
    bounded_cli_remote_http_test
  fi
fi

if [ "${MOUNT_RS_OZONE_IOPS:-0}" = "1" ]; then
  : "${MOUNT_RS_OZONE_COMPOSITIONS:?MOUNT_RS_OZONE_IOPS requires the Ozone composition harness}"
  iops_output=${MOUNT_RS_OZONE_IOPS_OUTPUT:-$run_dir/ozone-iops.json}
  iops_size_mib=${MOUNT_RS_OZONE_IOPS_SIZE_MIB:-1}
  iops_payload_bytes=${MOUNT_RS_OZONE_IOPS_PAYLOAD_BYTES:-4096}
  iops_iterations=${MOUNT_RS_OZONE_IOPS_ITERATIONS:-400}
  iops_concurrency=${MOUNT_RS_OZONE_IOPS_CONCURRENCY:-64}
  iops_minimum=${MOUNT_RS_OZONE_IOPS_MIN:-1000}
  iops_providers=${MOUNT_RS_OZONE_IOPS_PROVIDERS:-mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2}
  if [ "$iops_payload_bytes" != "4096" ] || [ "$iops_iterations" != "400" ] || [ "$iops_concurrency" != "64" ]; then
    echo "Ozone IOPS qualification requires payload=4096 iterations=400 concurrency=64" >&2
    exit 2
  fi
  case "$iops_minimum" in
    ''|*[!0-9]*|0)
      echo "Ozone IOPS minimum must be a positive integer" >&2
      exit 2
      ;;
  esac
  if [ "$iops_minimum" -lt 1000 ]; then
    echo "Ozone IOPS qualification minimum must be at least 1000" >&2
    exit 2
  fi
  export MOUNT_RS_PGLITE_DATABASE_URL="$PGLITE_DATABASE_URL"
  export MOUNT_RS_PGLITE_DURABLE=1
  export MOUNT_RS_R2_ENDPOINT="$R2_ENDPOINT"
  export MOUNT_RS_R2_BUCKET="$R2_BUCKET"
  export MOUNT_RS_R2_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID"
  export MOUNT_RS_R2_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY"
  export MOUNT_RS_R2_DURABLE=1
  bounded_node_test "iops" "$repo_dir/benchmarks/storage/runner.mjs" \
    --providers "$iops_providers" \
    --sizes "$iops_size_mib" \
    --payload-bytes "$iops_payload_bytes" \
    --iterations "$iops_iterations" \
    --concurrency "$iops_concurrency" \
    --min-iops "$iops_minimum" \
    --require-configured \
    --network-context "ozone-ci" \
    --output "$iops_output"
  node "$repo_dir/scripts/verify-w26-ozone-iops-artifact.mjs" \
    --output "$iops_output" \
    --providers "$iops_providers" \
    --minimum-iops "$iops_minimum"
  echo "OZONE_IOPS_PASS providers=$iops_providers target=$iops_minimum output=$iops_output"
fi

if [ "${MOUNT_RS_OZONE_TIDB_COMPOSITION:-0}" = "1" ]; then
  # test-tidb.sh owns the actual TiDB/TiKV/PD topology and restart sequence.
  # This process keeps the real Ozone gateway alive and supplies its scoped
  # S3 endpoint, so the child test composes independent TiDB metadata with
  # Ozone immutable blocks rather than silently switching to RustFS.
  export MOUNT_RS_TIDB_CHUNKED_RUSTFS=1
  export MOUNT_RS_TIDB_RUSTFS_PREFIX="$OZONE_TEST_PREFIX/tidb-blocks"
  export MOUNT_RS_TIDB_CHUNKED_VOLUME_KEY="$OZONE_TEST_PREFIX/tidb-metadata"
  export MOUNT_RS_TIDB_CHUNKED_RUSTFS_FIXTURE="$run_dir/tidb-chunked.fixture"
  export MOUNT_RS_TIDB_CHUNKED_RUSTFS_REOPEN=0
  export MOUNT_RS_TIDB_COMPOSITION_COMMAND="${MOUNT_RS_TIDB_COMPOSITION_COMMAND:-cargo test --locked -p mount-rs-tidb --test chunked_rustfs -- --ignored --nocapture}"
  bounded_composition_script "ozone-tidb-composition" "sh scripts/test-tidb.sh"
fi

if [ "${MOUNT_RS_OZONE_FOUNDATIONDB_COMPOSITION:-0}" = "1" ]; then
  # The FoundationDB harness builds its real client in an image that carries
  # the matching native library. It rewrites this loopback endpoint to the
  # Docker host gateway and owns its own cluster/network cleanup.
  export RUSTFS_COMBO_PREFIX="$OZONE_TEST_PREFIX/foundationdb-blocks"
  export MOUNT_RS_FOUNDATIONDB_TEST_PREFIX="$RUSTFS_COMBO_PREFIX"
  if [ "${MOUNT_RS_FOUNDATIONDB_NAPI:-0}" = "1" ]; then
    configure_foundationdb_napi_fixture ozone "$ozone_region"
    echo "FOUNDATIONDB_NAPI_EXTERNAL_BLOCK_FIXTURE service=ozone adapter=$MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER region=$RUSTFS_REGION endpoint=$R2_ENDPOINT"
  fi
  # The composition supervisor gives the child cleanup trap a longer grace
  # than the short Docker-action watchdog before it reaps a stuck process.
  bounded_composition_script "ozone-foundationdb-composition" "sh scripts/test-foundationdb.sh"
fi

echo "OZONE_INTEGRATION_PASS endpoint=$ozone_endpoint bucket=$ozone_bucket prefix=$OZONE_TEST_PREFIX"
