#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
rustfs_image="rustfs/rustfs:1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff"
rustfs_bucket="mount-rs-rustfs-test"
rustfs_access_key="mount-rs-rustfs-test"
rustfs_secret_key="mount-rs-rustfs-test-secret"
rustfs_region="us-east-1"

if ! command -v docker >/dev/null 2>&1; then
  echo "RustFS test requires Docker; Docker was not found. Install/start it outside this script." >&2
  exit 2
fi
if ! docker info >/dev/null 2>&1; then
  echo "RustFS test requires an already-running Docker daemon; this script will not start one." >&2
  exit 2
fi
for command_name in cargo curl node mktemp; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "RustFS test requires $command_name" >&2
    exit 2
  fi
done

if ! docker image inspect "$rustfs_image" >/dev/null 2>&1; then
  docker pull "$rustfs_image"
fi

temp_root=${TMPDIR:-/tmp}
if [ "$temp_root" != "/" ]; then
  temp_root=${temp_root%/}
fi
run_dir=""
data_dir=""
fixture_file=""
container_name="mount-rs-rustfs-$$-$(date +%s)"
ownership_label="mount-rs-rustfs-test"

inspect_owned_container() {
  inspect_output=""
  if inspect_output=$(docker container inspect \
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
  docker container rm --force "$container_name" >/dev/null 2>&1 &
  remove_pid=$!
  remove_ticks=0
  while kill -0 "$remove_pid" 2>/dev/null; do
    if [ "$remove_ticks" -ge 20 ]; then
      kill -TERM "$remove_pid" 2>/dev/null || true
      wait "$remove_pid" 2>/dev/null || true
      echo "Timed out removing test-owned RustFS container $container_name" >&2
      break
    fi
    sleep 1
    remove_ticks=$((remove_ticks + 1))
  done
  wait "$remove_pid" 2>/dev/null || true

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
  return 0
}

cleanup() {
  exit_code=$?
  trap - EXIT INT TERM
  if [ "${MOUNT_RS_RUSTFS_KEEP:-0}" = "1" ]; then
    echo "RUSTFS_KEEP=1 container=$container_name endpoint=${rustfs_endpoint:-unknown} data=${data_dir:-unknown}" >&2
    exit "$exit_code"
  fi

  container_cleanup_ok=1
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

docker run --detach \
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
  "$rustfs_image" /data >/dev/null

refresh_endpoint() {
  rustfs_port=""
  candidate_port=""
  previous_port=""
  port_ticks=0
  while :; do
    candidate_port=$(docker port "$container_name" 9000/tcp 2>/dev/null \
      | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -n 1)
    if [ -n "$candidate_port" ] && [ "$candidate_port" = "$previous_port" ]; then
      rustfs_port="$candidate_port"
      rustfs_endpoint="http://127.0.0.1:$rustfs_port"
      export R2_ENDPOINT="$rustfs_endpoint"
      return 0
    fi
    previous_port="$candidate_port"
    if [ "$port_ticks" -ge 20 ]; then
      docker logs --tail 160 "$container_name" >&2 || true
      echo "RustFS did not publish its test port" >&2
      return 1
    fi
    sleep 1
    port_ticks=$((port_ticks + 1))
  done
}

refresh_endpoint

wait_for_ready() {
  ready_ticks=0
  while :; do
    if curl --fail --silent --show-error --max-time 2 "$rustfs_endpoint/health" >/dev/null 2>&1; then
      return 0
    fi
    if ! docker inspect --format '{{.State.Running}}' "$container_name" 2>/dev/null \
      | grep -q '^true$'; then
      docker logs --tail 160 "$container_name" >&2 || true
      echo "RustFS container exited before readiness" >&2
      return 1
    fi
    if [ "$ready_ticks" -ge 60 ]; then
      docker logs --tail 160 "$container_name" >&2 || true
      echo "Timed out waiting for RustFS readiness at $rustfs_endpoint/health" >&2
      return 1
    fi
    sleep 1
    ready_ticks=$((ready_ticks + 1))
  done
}

wait_for_ready
echo "RUSTFS_HEALTHY endpoint=$rustfs_endpoint image=$rustfs_image"

export R2_ENDPOINT="$rustfs_endpoint"
export R2_BUCKET="$rustfs_bucket"
export R2_ACCESS_KEY_ID="$rustfs_access_key"
export R2_SECRET_ACCESS_KEY="$rustfs_secret_key"
export RUSTFS_REGION="$rustfs_region"
export RUSTFS_TEST_PREFIX="mount-rs-rustfs/$(date +%s)-$$"
export RUSTFS_FIXTURE_FILE="$fixture_file"

bootstrap_bucket() {
  bucket_log="$run_dir/bucket-bootstrap.log"
  bucket_ticks=0
  while :; do
    if node "$repo_dir/tests/rustfs/create-bucket.mjs" >"$bucket_log" 2>&1; then
      cat "$bucket_log"
      return 0
    fi
    if ! docker inspect --format '{{.State.Running}}' "$container_name" 2>/dev/null \
      | grep -q '^true$'; then
      cat "$bucket_log" >&2 || true
      docker logs --tail 160 "$container_name" >&2 || true
      echo "RustFS container exited before storage readiness" >&2
      return 1
    fi
    refresh_endpoint
    # /health can precede storage-quorum readiness; keep the signed probe
    # bounded to roughly 90 seconds without allowing an unbounded retry.
    if [ "$bucket_ticks" -ge 30 ]; then
      cat "$bucket_log" >&2 || true
      docker logs --tail 160 "$container_name" >&2 || true
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

docker restart "$container_name" >/dev/null
refresh_endpoint
wait_for_ready
bootstrap_bucket
echo "RUSTFS_RESTART_READY endpoint=$rustfs_endpoint"

cargo test \
  --manifest-path "$repo_dir/tests/rustfs/Cargo.toml" \
  --locked \
  -- "real_rustfs_reopen_after_service_restart" --exact --test-threads=1 --nocapture

echo "RUSTFS_INTEGRATION_PASS endpoint=$rustfs_endpoint bucket=$rustfs_bucket prefix=$RUSTFS_TEST_PREFIX"
