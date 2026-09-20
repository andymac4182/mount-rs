#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
rustfs_image="rustfs/rustfs:1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff"
rustfs_bucket="mount-rs-rustfs-test"
rustfs_access_key="mount-rs-rustfs-test"
rustfs_secret_key="mount-rs-rustfs-test-secret"
rustfs_region="us-east-1"

bounded_docker_command_for_timeout() {
  bounded_timeout=$1
  action_name=$2
  shift 2
  case "$bounded_timeout" in
    ''|*[!0-9]*)
      echo "RustFS Docker action timeout must be a non-negative integer" >&2
      return 2
      ;;
  esac
  python3 "$repo_dir/scripts/rustfs-bounded-docker.py" \
    "$bounded_timeout" \
    "$action_name" \
    "$@"
}

bounded_docker_command() {
  bounded_timeout=${RUSTFS_DOCKER_ACTION_TIMEOUT_SECONDS:-30}
  bounded_docker_command_for_timeout "$bounded_timeout" "$@"
}

bounded_docker_startup_command() {
  bounded_timeout=${RUSTFS_DOCKER_STARTUP_TIMEOUT_SECONDS:-120}
  bounded_docker_command_for_timeout "$bounded_timeout" "$@"
}

if ! command -v docker >/dev/null 2>&1; then
  echo "RustFS test requires Docker; Docker was not found. Install/start it outside this script." >&2
  exit 2
fi
for command_name in cargo curl node mktemp python3; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "RustFS test requires $command_name" >&2
    exit 2
  fi
done

if bounded_docker_startup_command "daemon-info" docker info >/dev/null 2>&1; then
  :
else
  echo "RustFS test requires an already-running Docker daemon; this script will not start one." >&2
  exit 2
fi

if bounded_docker_startup_command "image-inspect" docker image inspect "$rustfs_image" >/dev/null 2>&1; then
  :
else
  image_inspect_status=$?
  if [ "$image_inspect_status" -eq 124 ] || [ "$image_inspect_status" -eq 125 ]; then
    echo "Could not boundedly inspect the pinned RustFS image" >&2
    exit 2
  fi
  if ! bounded_docker_startup_command "image-pull" docker pull "$rustfs_image" >/dev/null 2>&1; then
    echo "Could not pull the pinned RustFS image" >&2
    exit 2
  fi
fi

temp_root=${TMPDIR:-/tmp}
if [ "$temp_root" != "/" ]; then
  temp_root=${temp_root%/}
fi
run_dir=""
data_dir=""
fixture_file=""
pglite_pid=""
pglite_data_dir=""
pglite_log=""
combo_pid=""
combo_pid_file=""
container_name="mount-rs-rustfs-$$-$(date +%s)"
cleanup_container_name="${container_name}-cleanup"
ownership_label="mount-rs-rustfs-test"

inspect_owned_container() {
  inspect_output=""
  if inspect_output=$(bounded_docker_command "inspect-service" docker container inspect \
    --format '{{.Name}}|{{index .Config.Labels "com.mount-rs.rustfs-test"}}|{{index .Config.Labels "com.mount-rs.rustfs-test-run"}}' \
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
      echo "Could not inspect test container $container_name: $inspect_output" >&2
      return 2
      ;;
  esac
}

inspect_owned_cleanup_container() {
  inspect_output=""
  if inspect_output=$(bounded_docker_command "inspect-cleanup" docker container inspect \
    --format '{{.Name}}|{{index .Config.Labels "com.mount-rs.rustfs-test"}}|{{index .Config.Labels "com.mount-rs.rustfs-test-run"}}|{{index .Config.Labels "com.mount-rs.rustfs-test-purpose"}}' \
    "$cleanup_container_name" 2>&1); then
    expected="/$cleanup_container_name|$ownership_label|$container_name|cleanup"
    if [ "$inspect_output" != "$expected" ]; then
      echo "Refusing to manage cleanup container with unexpected ownership: $inspect_output" >&2
      return 3
    fi
    return 0
  fi
  case "$inspect_output" in
    *"No such container"*|*"No such object"*) return 1 ;;
    *)
      echo "Could not inspect RustFS cleanup container $cleanup_container_name: $inspect_output" >&2
      return 2
      ;;
  esac
}

validate_run_dir_for_cleanup() {
  [ -n "$run_dir" ] || return 1
  [ -d "$run_dir" ] || return 1
  [ ! -L "$run_dir" ] || return 1
  case "$run_dir" in
    "$temp_root"/mount-rs-rustfs.??????) ;;
    *) return 1 ;;
  esac
  [ -f "$run_dir/.mount-rs-rustfs-owned" ] || return 1
  [ ! -L "$run_dir/.mount-rs-rustfs-owned" ] || return 1
  [ "$(sed -n '1p' "$run_dir/.mount-rs-rustfs-owned" 2>/dev/null)" = "$container_name" ] || return 1
}

validate_data_dir_for_cleanup() {
  validate_run_dir_for_cleanup || return 1
  [ "$data_dir" = "$run_dir/data" ] || return 1
  [ -d "$data_dir" ] || return 1
  [ ! -L "$data_dir" ] || return 1
}

remove_owned_run_dir() {
  if ! validate_run_dir_for_cleanup; then
    echo "Refusing recursive cleanup of unvalidated RustFS temp path: ${run_dir:-<unset>}" >&2
    return 1
  fi
  if ! rm -rf "$run_dir"; then
    echo "Could not remove owned RustFS temp path; preserving it: $run_dir" >&2
    return 1
  fi
  if [ -e "$run_dir" ] || [ -L "$run_dir" ]; then
    echo "Could not remove owned RustFS temp path; preserving it: $run_dir" >&2
    return 1
  fi
  return 0
}

bounded_remove_container() {
  if inspect_owned_container; then
    :
  else
    inspect_status=$?
    if [ "$inspect_status" -eq 1 ]; then
      return 0
    fi
    return 1
  fi
  if bounded_docker_command "remove-service" docker container rm --force "$container_name" >/dev/null 2>&1; then
    remove_status=0
  else
    remove_status=$?
  fi
  if [ "$remove_status" -eq 124 ]; then
    echo "Timed out removing test-owned RustFS container $container_name" >&2
  elif [ "$remove_status" -eq 125 ]; then
    echo "Could not reap RustFS container removal process $container_name" >&2
  elif [ "$remove_status" -ne 0 ]; then
    echo "RustFS container removal command failed with status $remove_status: $container_name" >&2
  fi

  if inspect_owned_container; then
    echo "RustFS container still exists after bounded removal: $container_name" >&2
    return 1
  else
    inspect_status=$?
  fi
  if [ "$inspect_status" -ne 1 ]; then
    echo "Could not verify removal of RustFS container $container_name" >&2
    return 1
  fi
  if [ "$remove_status" -eq 125 ]; then
    return 1
  fi
  return 0
}

bounded_remove_cleanup_container() {
  if inspect_owned_cleanup_container; then
    :
  else
    inspect_status=$?
    if [ "$inspect_status" -eq 1 ]; then
      return 0
    fi
    return 1
  fi
  if bounded_docker_command "remove-cleanup" docker container rm --force "$cleanup_container_name" >/dev/null 2>&1; then
    remove_status=0
  else
    remove_status=$?
  fi
  if [ "$remove_status" -eq 124 ]; then
    echo "Timed out removing RustFS cleanup container $cleanup_container_name" >&2
  elif [ "$remove_status" -eq 125 ]; then
    echo "Could not reap RustFS cleanup-container removal process $cleanup_container_name" >&2
  elif [ "$remove_status" -ne 0 ]; then
    echo "RustFS cleanup-container removal command failed with status $remove_status: $cleanup_container_name" >&2
  fi

  if inspect_owned_cleanup_container; then
    echo "RustFS cleanup container still exists after bounded removal: $cleanup_container_name" >&2
    return 1
  else
    inspect_status=$?
  fi
  if [ "$inspect_status" -ne 1 ]; then
    echo "Could not verify removal of RustFS cleanup container $cleanup_container_name" >&2
    return 1
  fi
  if [ "$remove_status" -eq 125 ]; then
    return 1
  fi
  return 0
}

remove_owned_data_contents() {
  if ! validate_data_dir_for_cleanup; then
    echo "Refusing container cleanup for unvalidated RustFS data path: ${data_dir:-<unset>}" >&2
    return 1
  fi
  if inspect_owned_container; then
    echo "Refusing data cleanup while the RustFS service container still exists: $container_name" >&2
    return 1
  else
    inspect_status=$?
    if [ "$inspect_status" -ne 1 ]; then
      return 1
    fi
  fi
  if inspect_owned_cleanup_container; then
    echo "RustFS cleanup container name is already in use: $cleanup_container_name" >&2
    return 1
  else
    inspect_status=$?
    if [ "$inspect_status" -ne 1 ]; then
      return 1
    fi
  fi

  # RustFS may leave root-owned metadata in the test bind mount. Use the
  # already pinned image as a narrowly mounted root helper; never chmod the
  # host tree and never remove the run directory until this helper is gone.
  if bounded_docker_command "create-cleanup" docker container create \
    --name "$cleanup_container_name" \
    --label "com.mount-rs.rustfs-test=$ownership_label" \
    --label "com.mount-rs.rustfs-test-run=$container_name" \
    --label "com.mount-rs.rustfs-test-purpose=cleanup" \
    --network none \
    --read-only \
    --user 0:0 \
    --mount "type=bind,src=$data_dir,dst=/cleanup" \
    --entrypoint /bin/sh \
    "$rustfs_image" \
    -c 'find /cleanup -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +' >/dev/null 2>&1; then
    create_status=0
  else
    create_status=$?
    if inspect_owned_cleanup_container; then
      if ! bounded_remove_cleanup_container; then
        echo "Could not safely remove leaked RustFS cleanup helper; preserving: $run_dir" >&2
        return 1
      fi
    else
      inspect_status=$?
      if [ "$inspect_status" -ne 1 ]; then
        echo "Could not determine whether RustFS cleanup helper was created; preserving: $run_dir" >&2
        return 1
      fi
    fi
    echo "Could not create RustFS data cleanup helper with status $create_status; preserving: $run_dir" >&2
    return 1
  fi

  cleanup_status=0
  if bounded_docker_command "start-cleanup" docker container start --attach "$cleanup_container_name" >/dev/null 2>&1; then
    cleanup_status=0
  else
    cleanup_status=$?
  fi

  if ! bounded_remove_cleanup_container; then
    echo "Could not verify removal of RustFS data cleanup helper; preserving: $run_dir" >&2
    return 1
  fi
  if [ "$cleanup_status" -ne 0 ]; then
    echo "RustFS data cleanup helper failed with status $cleanup_status; preserving: $run_dir" >&2
    return 1
  fi

  remaining_data=""
  if ! remaining_data=$(find "$data_dir" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null); then
    echo "Could not verify RustFS data cleanup; preserving: $run_dir" >&2
    return 1
  fi
  if [ -n "$remaining_data" ]; then
    echo "RustFS data cleanup left entries; preserving: $run_dir" >&2
    return 1
  fi
  return 0
}

bounded_wait_for_child() {
  bounded_wait_pid=$1
  bounded_wait_seconds=$2
  bounded_wait_alarm_triggered=0
  trap 'bounded_wait_alarm_triggered=1' ALRM
  (sleep "$bounded_wait_seconds"; kill -ALRM "$$" 2>/dev/null) &
  bounded_wait_alarm_pid=$!
  if wait "$bounded_wait_pid" 2>/dev/null; then
    bounded_wait_status=0
  else
    bounded_wait_status=$?
  fi
  kill -KILL "$bounded_wait_alarm_pid" 2>/dev/null || true
  wait "$bounded_wait_alarm_pid" 2>/dev/null || true
  trap - ALRM
  if [ "$bounded_wait_alarm_triggered" -eq 1 ]; then
    return 124
  fi
  return "$bounded_wait_status"
}

stop_pglite_server() {
  if [ -z "$pglite_pid" ]; then
    return 0
  fi
  if kill -0 "$pglite_pid" 2>/dev/null; then
    kill -TERM "$pglite_pid" 2>/dev/null || true
    pglite_ticks=0
    while kill -0 "$pglite_pid" 2>/dev/null; do
      if [ "$pglite_ticks" -ge 20 ]; then
        kill -KILL "$pglite_pid" 2>/dev/null || true
        break
      fi
      sleep 1
      pglite_ticks=$((pglite_ticks + 1))
    done
  fi
  if bounded_wait_for_child "$pglite_pid" 2; then
    pglite_wait_status=0
  else
    pglite_wait_status=$?
  fi
  if [ "$pglite_wait_status" -eq 124 ]; then
    echo "Could not stop test-owned PGlite server process $pglite_pid" >&2
    return 1
  fi
  pglite_pid=""
  return 0
}

combo_group_signal() {
  combo_signal=$1
  [ -f "$combo_pid_file" ] || return 0
  combo_group_pid=$(sed -n '1p' "$combo_pid_file" 2>/dev/null || true)
  case "$combo_group_pid" in
    ''|*[!0-9]*) return 1 ;;
  esac
  python3 - "$combo_group_pid" "$combo_signal" <<'PY'
import os
import sys

try:
    os.killpg(int(sys.argv[1]), int(sys.argv[2]))
except (PermissionError, ProcessLookupError):
    pass
PY
}

combo_group_exists() {
  [ -f "$combo_pid_file" ] || return 1
  combo_group_pid=$(sed -n '1p' "$combo_pid_file" 2>/dev/null || true)
  case "$combo_group_pid" in
    ''|*[!0-9]*) return 2 ;;
  esac
  python3 - "$combo_group_pid" <<'PY' >/dev/null 2>&1
import os
import subprocess
import sys

pgid = int(sys.argv[1])
try:
    os.killpg(pgid, 0)
except ProcessLookupError:
    raise SystemExit(1)
except PermissionError:
    try:
        result = subprocess.run(
            ["ps", "-eo", "pgid="],
            capture_output=True,
            text=True,
            check=False,
            timeout=1.0,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise SystemExit(0)
    if result.returncode != 0:
        raise SystemExit(0)
    raise SystemExit(0 if any(line.strip() == str(pgid) for line in result.stdout.splitlines()) else 1)
raise SystemExit(0)
PY
}

stop_combo_command() {
  if [ -z "$combo_pid" ] && [ ! -f "$combo_pid_file" ]; then
    return 0
  fi
  if [ -n "$combo_pid" ] && kill -0 "$combo_pid" 2>/dev/null; then
    kill -TERM "$combo_pid" 2>/dev/null || true
    combo_ticks=0
    while kill -0 "$combo_pid" 2>/dev/null; do
      if [ "$combo_ticks" -ge 10 ]; then
        combo_group_signal 9 || true
        kill -KILL "$combo_pid" 2>/dev/null || true
        break
      fi
      sleep 1
      combo_ticks=$((combo_ticks + 1))
    done
  fi
  if [ -n "$combo_pid" ]; then
    wait "$combo_pid" 2>/dev/null || true
  fi
  if combo_group_exists; then
    combo_group_status=0
  else
    combo_group_status=$?
  fi
  if [ "$combo_group_status" -eq 2 ]; then
    echo "Could not validate RustFS combo process-group identity" >&2
    return 1
  fi
  if [ "$combo_group_status" -eq 0 ]; then
    combo_group_signal 9 || true
    combo_ticks=0
    while :; do
      if combo_group_exists; then
        combo_group_status=0
      else
        combo_group_status=$?
      fi
      if [ "$combo_group_status" -eq 1 ]; then
        break
      fi
      if [ "$combo_group_status" -eq 2 ]; then
        echo "Could not validate RustFS combo process-group identity" >&2
        return 1
      fi
      if [ "$combo_ticks" -ge 10 ]; then
        echo "RustFS combo process group remains after bounded cleanup" >&2
        return 1
      fi
      sleep 1
      combo_ticks=$((combo_ticks + 1))
    done
  fi
  if [ -n "$combo_pid" ] && kill -0 "$combo_pid" 2>/dev/null; then
    echo "Could not stop RustFS combo command process $combo_pid" >&2
    return 1
  fi
  combo_pid=""
  combo_pid_file=""
  return 0
}

cleanup() {
  exit_code=$?
  trap - EXIT INT TERM
  if [ "${MOUNT_RS_RUSTFS_KEEP:-0}" = "1" ]; then
    if ! stop_combo_command; then
      echo "RUSTFS_CLEANUP_INCOMPLETE combo_pid=$combo_pid data=${run_dir:-unknown}" >&2
      if [ "$exit_code" -eq 0 ]; then
        exit_code=1
      fi
    fi
    if ! stop_pglite_server; then
      echo "RUSTFS_CLEANUP_INCOMPLETE pglite_pid=$pglite_pid data=${run_dir:-unknown}" >&2
      if [ "$exit_code" -eq 0 ]; then
        exit_code=1
      fi
    fi
    echo "RUSTFS_KEEP=1 container=$container_name endpoint=${rustfs_endpoint:-unknown} data=${data_dir:-unknown}" >&2
    exit "$exit_code"
  fi

  container_cleanup_ok=1
  if ! stop_combo_command; then
    container_cleanup_ok=0
  fi
  if inspect_owned_container; then
    if ! bounded_remove_container; then
      container_cleanup_ok=0
    fi
  else
    inspect_status=$?
    if [ "$inspect_status" -ne 1 ]; then
      container_cleanup_ok=0
    fi
  fi

  if ! stop_pglite_server; then
    container_cleanup_ok=0
  fi

  if [ "$container_cleanup_ok" -eq 1 ]; then
    if ! remove_owned_data_contents; then
      container_cleanup_ok=0
    fi
  fi
  if [ "$container_cleanup_ok" -eq 1 ]; then
    if ! remove_owned_run_dir; then
      container_cleanup_ok=0
    fi
  fi
  if [ "$container_cleanup_ok" -ne 1 ]; then
    echo "RUSTFS_CLEANUP_INCOMPLETE container=$container_name data=${run_dir:-unknown}" >&2
    if [ "$exit_code" -eq 0 ]; then
      exit_code=1
    fi
  fi
  exit "$exit_code"
}
trap cleanup EXIT INT TERM

run_dir=$(mktemp -d "$temp_root/mount-rs-rustfs.XXXXXX")
data_dir="$run_dir/data"
fixture_file="$run_dir/restart-fixture"
mkdir "$data_dir"
# The official image runs as UID 10001. This directory is test-owned and
# disposable, so a mode-only grant avoids changing ownership of user paths.
chmod 0777 "$data_dir"
printf '%s\n' "$container_name" > "$run_dir/.mount-rs-rustfs-owned"

if inspect_owned_container; then
  echo "RustFS test container name is already in use: $container_name" >&2
  exit 2
else
  inspect_status=$?
  if [ "$inspect_status" -ne 1 ]; then
    echo "Cannot safely reserve RustFS test container name: $container_name" >&2
    exit 2
  fi
fi

if [ ! -f "$repo_dir/tests/pglite/server.mjs" ]; then
  echo "RustFS split-provider test requires tests/pglite/server.mjs" >&2
  exit 2
fi
pglite_data_dir="$run_dir/pglite-data"
pglite_log="$run_dir/pglite.log"
mkdir "$pglite_data_dir"
pglite_port=$(node --input-type=module -e '
  import net from "node:net";
  const server = net.createServer();
  server.listen(0, "127.0.0.1", () => {
    console.log(server.address().port);
    server.close();
  });
')
PGLITE_PORT="$pglite_port" \
PGLITE_MAX_CONNECTIONS=8 \
PGLITE_DATA_DIR="$pglite_data_dir" \
node "$repo_dir/tests/pglite/server.mjs" >"$pglite_log" 2>&1 &
pglite_pid=$!
pglite_ticks=0
while :; do
  if grep -q '^PGLITE_READY ' "$pglite_log" 2>/dev/null; then
    break
  fi
  if ! kill -0 "$pglite_pid" 2>/dev/null; then
    cat "$pglite_log" >&2 || true
    echo "PGlite server exited before readiness" >&2
    exit 1
  fi
  if [ "$pglite_ticks" -ge 60 ]; then
    cat "$pglite_log" >&2 || true
    echo "Timed out waiting for PGlite readiness" >&2
    exit 1
  fi
  sleep 1
  pglite_ticks=$((pglite_ticks + 1))
done
export PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$pglite_port/postgres?sslmode=disable"
export RUSTFS_SQLITE_METADATA_FILE="$run_dir/sqlite-metadata.db"
echo "PGLITE_READY endpoint=127.0.0.1:$pglite_port"

if bounded_docker_startup_command "run-service" docker run --detach \
  --name "$container_name" \
  --label "com.mount-rs.rustfs-test=$ownership_label" \
  --label "com.mount-rs.rustfs-test-run=$container_name" \
  --mount "type=bind,src=$data_dir,dst=/data" \
  --publish 127.0.0.1::9000 \
  --env "RUSTFS_ACCESS_KEY=$rustfs_access_key" \
  --env "RUSTFS_SECRET_KEY=$rustfs_secret_key" \
  --env "RUSTFS_REGION=$rustfs_region" \
  --env RUSTFS_ADDRESS=:9000 \
  --env RUSTFS_CONSOLE_ENABLE=false \
  "$rustfs_image" /data >/dev/null 2>&1; then
  :
else
  docker_run_status=$?
  echo "Could not start RustFS container with status $docker_run_status" >&2
  exit 1
fi

refresh_endpoint() {
  rustfs_port=""
  candidate_port=""
  previous_port=""
  port_ticks=0
  while :; do
    port_output=""
    if port_output=$(bounded_docker_command "port" docker port "$container_name" 9000/tcp 2>/dev/null); then
      :
    else
      port_status=$?
      if [ "$port_status" -eq 124 ] || [ "$port_status" -eq 125 ]; then
        echo "Could not boundedly query the RustFS test port" >&2
        return 1
      fi
    fi
    candidate_port=$(printf '%s\n' "$port_output" \
      | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -n 1)
    if [ -n "$candidate_port" ] && [ "$candidate_port" = "$previous_port" ]; then
      rustfs_port="$candidate_port"
      rustfs_endpoint="http://127.0.0.1:$rustfs_port"
      export R2_ENDPOINT="$rustfs_endpoint"
      return 0
    fi
    previous_port="$candidate_port"
    if [ "$port_ticks" -ge 20 ]; then
      bounded_docker_command "logs" docker logs --tail 160 "$container_name" >&2 || true
      echo "RustFS did not publish its test port" >&2
      return 1
    fi
    sleep 1
    port_ticks=$((port_ticks + 1))
  done
}

refresh_endpoint

container_is_running() {
  running_output=""
  if running_output=$(bounded_docker_command "inspect-running" docker inspect \
    --format '{{.State.Running}}' "$container_name" 2>/dev/null); then
    [ "$running_output" = "true" ]
    return $?
  else
    running_status=$?
  fi
  if [ "$running_status" -eq 124 ] || [ "$running_status" -eq 125 ]; then
    echo "Could not boundedly inspect RustFS container state" >&2
    return 2
  fi
  echo "Could not verify RustFS container state (inspect status $running_status)" >&2
  return 2
}

wait_for_ready() {
  ready_ticks=0
  while :; do
    if curl --fail --silent --show-error --max-time 2 "$rustfs_endpoint/health" >/dev/null 2>&1; then
      return 0
    fi
    if container_is_running; then
      :
    else
      inspect_status=$?
      bounded_docker_command "logs" docker logs --tail 160 "$container_name" >&2 || true
      if [ "$inspect_status" -eq 2 ]; then
        echo "Could not verify RustFS container readiness state" >&2
      else
        echo "RustFS container exited before readiness" >&2
      fi
      return 1
    fi
    if [ "$ready_ticks" -ge 60 ]; then
      bounded_docker_command "logs" docker logs --tail 160 "$container_name" >&2 || true
      echo "Timed out waiting for RustFS readiness at $rustfs_endpoint/health" >&2
      return 1
    fi
    sleep 1
    ready_ticks=$((ready_ticks + 1))
  done
}

wait_for_ready
echo "RUSTFS_HEALTHY endpoint=$rustfs_endpoint image=$rustfs_image"

bounded_docker_action() {
  action_name=$1
  shift
  if bounded_docker_command "$action_name" "$@" >/dev/null 2>&1; then
    return 0
  else
    action_status=$?
  fi
  if [ "$action_status" -eq 124 ]; then
    echo "Timed out during RustFS $action_name action" >&2
  elif [ "$action_status" -eq 125 ]; then
    echo "Could not reap RustFS $action_name action process" >&2
  else
    echo "RustFS $action_name action failed with status $action_status" >&2
  fi
  return 1
}

export R2_ENDPOINT="$rustfs_endpoint"
export R2_BUCKET="$rustfs_bucket"
export R2_ACCESS_KEY_ID="$rustfs_access_key"
export R2_SECRET_ACCESS_KEY="$rustfs_secret_key"
export RUSTFS_REGION="$rustfs_region"
export RUSTFS_TEST_PREFIX="mount-rs-rustfs/$(date +%s)-$$"
export RUSTFS_COMBO_PREFIX="$RUSTFS_TEST_PREFIX/combo"
export RUSTFS_FIXTURE_FILE="$fixture_file"
export RUSTFS_RUN_DIR="$run_dir"
combo_pid_file="$run_dir/combo.pid"
export RUSTFS_ENDPOINT="$rustfs_endpoint"
export RUSTFS_BUCKET="$rustfs_bucket"
export RUSTFS_ACCESS_KEY_ID="$rustfs_access_key"
export RUSTFS_SECRET_ACCESS_KEY="$rustfs_secret_key"
export RUSTFS_HARNESS_CONTAINER="$container_name"
export RUSTFS_HARNESS_IMAGE="$rustfs_image"

bootstrap_bucket() {
  bucket_log="$run_dir/bucket-bootstrap.log"
  bucket_ticks=0
  while :; do
    if node "$repo_dir/tests/rustfs/create-bucket.mjs" >"$bucket_log" 2>&1; then
      cat "$bucket_log"
      return 0
    fi
    if container_is_running; then
      :
    else
      inspect_status=$?
      cat "$bucket_log" >&2 || true
      bounded_docker_command "logs" docker logs --tail 160 "$container_name" >&2 || true
      if [ "$inspect_status" -eq 2 ]; then
        echo "Could not verify RustFS container storage state" >&2
      else
        echo "RustFS container exited before storage readiness" >&2
      fi
      return 1
    fi
    refresh_endpoint
    # /health can precede storage-quorum readiness; keep the signed probe
    # bounded to roughly 90 seconds without allowing an unbounded retry.
    if [ "$bucket_ticks" -ge 30 ]; then
      cat "$bucket_log" >&2 || true
      bounded_docker_command "logs" docker logs --tail 160 "$container_name" >&2 || true
      echo "Timed out waiting for RustFS storage readiness" >&2
      return 1
    fi
    sleep 1
    bucket_ticks=$((bucket_ticks + 1))
  done
}

bootstrap_bucket
echo "RUSTFS_READY endpoint=$rustfs_endpoint image=$rustfs_image"

cargo test \
  --manifest-path "$repo_dir/tests/rustfs/Cargo.toml" \
  --locked \
  -- "real_rustfs_block_contract" --exact --test-threads=1 --nocapture

cargo test \
  --manifest-path "$repo_dir/tests/rustfs/Cargo.toml" \
  --locked \
  -- "real_rustfs_sqlite_metadata_round_trip" --exact --test-threads=1 --nocapture

cargo test \
  --manifest-path "$repo_dir/tests/rustfs/Cargo.toml" \
  --locked \
  -- "real_rustfs_pglite_metadata_round_trip" --exact --test-threads=1 --nocapture

cargo test \
  --manifest-path "$repo_dir/tests/rustfs/Cargo.toml" \
  --locked \
  -- "real_rustfs_block_benchmark" --exact --test-threads=1 --nocapture

node "$repo_dir/tests/rustfs/napi-factories.mjs"

echo "RUSTFS_SQLITE_VFS_START"
cargo test \
  --manifest-path "$repo_dir/Cargo.toml" \
  --locked -p mount-rs-sqlite-vfs --features remote-harness \
  --test remote_storage_bridge -- \
  remote_pglite_rustfs_sqlite_vfs --exact --ignored --nocapture
echo "RUSTFS_SQLITE_VFS_PASS"

run_combo_command() {
  if [ -z "${RUSTFS_COMBO_COMMAND:-}" ]; then
    return 0
  fi
  combo_name=${RUSTFS_COMBO_NAME:-external}
  combo_timeout=${RUSTFS_COMBO_TIMEOUT_SECONDS:-900}
  case "$combo_timeout" in
    ''|*[!0-9]*)
      echo "RUSTFS_COMBO_TIMEOUT_SECONDS must be a non-negative integer" >&2
      return 2
      ;;
  esac
  echo "RUSTFS_COMBO_START name=$combo_name timeout=${combo_timeout}s"
  # rustfs-combo-runner.py owns the timeout and the child process-group
  # cleanup. Do not add a second watchdog here: killing the Python supervisor
  # at the same deadline can interrupt its SIGTERM handler before it has
  # terminated and verified the command's process group.
  python3 "$repo_dir/scripts/rustfs-combo-runner.py" \
    "$combo_timeout" \
    "$repo_dir" \
    "$combo_pid_file" \
    "$combo_name" \
    "$RUSTFS_COMBO_COMMAND" &
  combo_pid=$!
  if wait "$combo_pid"; then
    combo_pid=""
    combo_pid_file=""
    return 0
  else
    combo_status=$?
    # On an abnormal runner exit, leave combo_pid_file available to the EXIT
    # cleanup. The runner removes it only after it has verified the process
    # group is gone; otherwise cleanup must finish that verification before
    # the run directory can be removed.
    combo_pid=""
    return "$combo_status"
  fi
}

run_combo_command

RUSTFS_VFS_RESTART_PHASE=prepare cargo test \
  --manifest-path "$repo_dir/Cargo.toml" \
  --locked -p mount-rs-sqlite-vfs --features remote-harness \
  --test remote_storage_bridge -- \
  remote_vfs_survives_rustfs_restart --exact --ignored --nocapture

bounded_docker_action "stop" docker stop --time=5 "$container_name"
if container_is_running; then
  echo "RustFS container remained running after fault injection" >&2
  exit 1
else
  inspect_status=$?
  if [ "$inspect_status" -eq 2 ]; then
    echo "Could not verify RustFS container stopped after fault injection" >&2
    exit 1
  fi
fi
if curl --fail --silent --show-error --max-time 2 "$rustfs_endpoint/health" >/dev/null 2>&1; then
  echo "RustFS health endpoint remained reachable during fault injection" >&2
  exit 1
fi
echo "RUSTFS_FAULT_WINDOW_PASS container=$container_name"

bounded_docker_action "start" docker start "$container_name"
refresh_endpoint
wait_for_ready
bootstrap_bucket
echo "RUSTFS_FAULT_RECOVERY_PASS endpoint=$rustfs_endpoint"
echo "RUSTFS_RESTART_READY endpoint=$rustfs_endpoint"

RUSTFS_VFS_RESTART_PHASE=reopen cargo test \
  --manifest-path "$repo_dir/Cargo.toml" \
  --locked -p mount-rs-sqlite-vfs --features remote-harness \
  --test remote_storage_bridge -- \
  remote_vfs_survives_rustfs_restart --exact --ignored --nocapture

cargo test \
  --manifest-path "$repo_dir/tests/rustfs/Cargo.toml" \
  --locked \
  -- "real_rustfs_reopen_after_service_restart" --exact --test-threads=1 --nocapture

echo "RUSTFS_INTEGRATION_PASS endpoint=$rustfs_endpoint bucket=$rustfs_bucket prefix=$RUSTFS_TEST_PREFIX"
