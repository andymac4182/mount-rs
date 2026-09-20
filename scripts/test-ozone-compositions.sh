#!/bin/sh
set -eu

# This wrapper is deliberately opt-in. It owns only the local PGlite process
# and SQLite metadata file; scripts/test-ozone.sh owns the actual Ozone
# container, bucket, S3 credentials, prefix, and Ozone cleanup.
if [ "${MOUNT_RS_OZONE_COMPOSITIONS:-0}" != "1" ]; then
  echo "Set MOUNT_RS_OZONE_COMPOSITIONS=1 to run the explicit Ozone composition gate." >&2
  exit 2
fi

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
startup_timeout=${MOUNT_RS_OZONE_PGLITE_STARTUP_TIMEOUT_SECONDS:-60}
shutdown_timeout=${MOUNT_RS_OZONE_PGLITE_SHUTDOWN_TIMEOUT_SECONDS:-30}

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

validate_positive_timeout "$startup_timeout" MOUNT_RS_OZONE_PGLITE_STARTUP_TIMEOUT_SECONDS
validate_positive_timeout "$shutdown_timeout" MOUNT_RS_OZONE_PGLITE_SHUTDOWN_TIMEOUT_SECONDS

for command_name in cargo grep mktemp node rm sed sleep; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Ozone composition gate requires $command_name" >&2
    exit 2
  fi
done

server_file="$repo_dir/tests/pglite/server.mjs"
if [ ! -f "$server_file" ]; then
  echo "Ozone composition gate requires tests/pglite/server.mjs" >&2
  exit 2
fi
if [ ! -d "$repo_dir/tests/pglite/node_modules/@electric-sql/pglite" ] \
  || [ ! -d "$repo_dir/tests/pglite/node_modules/@electric-sql/pglite-socket" ]; then
  echo "Ozone composition gate requires installed tests/pglite dependencies; run pnpm --dir tests/pglite install --frozen-lockfile" >&2
  exit 2
fi

temp_root=${TMPDIR:-/tmp}
if [ "$temp_root" = "/" ]; then
  temp_pattern="/mount-rs-ozone-compositions.XXXXXX"
  run_dir_prefix="/mount-rs-ozone-compositions."
else
  temp_root=${temp_root%/}
  temp_pattern="$temp_root/mount-rs-ozone-compositions.XXXXXX"
  run_dir_prefix="$temp_root/mount-rs-ozone-compositions."
fi
run_dir=$(mktemp -d "$temp_pattern")
if [ ! -d "$run_dir" ] || [ -L "$run_dir" ]; then
  echo "Could not create a safe Ozone composition run directory" >&2
  exit 2
fi
case "$run_dir" in
  "$run_dir_prefix"??????) ;;
  *)
    echo "Refusing an unexpected Ozone composition run directory: $run_dir" >&2
    exit 2
    ;;
esac
printf '%s\n' "mount-rs-ozone-compositions" >"$run_dir/.mount-rs-ozone-compositions-owned"

pglite_pid=""
pglite_data_dir="$run_dir/pglite-data"
pglite_log="$run_dir/pglite.log"
mkdir "$pglite_data_dir"

cleanup_run_dir() {
  [ -d "$run_dir" ] || return 0
  [ ! -L "$run_dir" ] || return 1
  case "$run_dir" in
    "$run_dir_prefix"??????) ;;
    *) return 1 ;;
  esac
  [ -f "$run_dir/.mount-rs-ozone-compositions-owned" ] || return 1
  [ ! -L "$run_dir/.mount-rs-ozone-compositions-owned" ] || return 1
  rm -rf "$run_dir"
  [ ! -e "$run_dir" ] && [ ! -L "$run_dir" ]
}

stop_pglite() {
  if [ -z "$pglite_pid" ]; then
    return 0
  fi
  if kill -0 "$pglite_pid" 2>/dev/null; then
    kill -TERM "$pglite_pid" 2>/dev/null || true
    ticks=0
    while kill -0 "$pglite_pid" 2>/dev/null; do
      if [ "$ticks" -ge "$shutdown_timeout" ]; then
        kill -KILL "$pglite_pid" 2>/dev/null || true
        break
      fi
      sleep 1
      ticks=$((ticks + 1))
    done
  fi
  if wait "$pglite_pid" 2>/dev/null; then
    wait_status=0
  else
    wait_status=$?
  fi
  pglite_pid=""
  if [ "$wait_status" -ne 0 ] && [ "$wait_status" -ne 143 ]; then
    echo "PGlite composition server exited with status $wait_status" >&2
    return 1
  fi
  return 0
}

cleanup() {
  exit_code=$1
  trap - EXIT INT TERM
  cleanup_ok=1
  if ! stop_pglite; then
    cleanup_ok=0
  fi
  if ! cleanup_run_dir; then
    echo "OZONE_COMPOSITION_CLEANUP_INCOMPLETE data=$run_dir" >&2
    cleanup_ok=0
  fi
  if [ "$cleanup_ok" -eq 1 ]; then
    echo "OZONE_COMPOSITION_CLEANUP_PASS" >&2
  elif [ "$exit_code" -eq 0 ]; then
    exit_code=1
  fi
  exit "$exit_code"
}
trap 'cleanup "$?"' EXIT INT TERM

pglite_port=$(node --input-type=module -e '
  import net from "node:net";
  const server = net.createServer();
  server.listen(0, "127.0.0.1", () => {
    console.log(server.address().port);
    server.close();
  });
')
case "$pglite_port" in
  ''|*[!0-9]*)
    echo "Could not reserve a loopback PGlite port" >&2
    exit 2
    ;;
esac

PGLITE_PORT="$pglite_port" \
PGLITE_MAX_CONNECTIONS=8 \
PGLITE_DATA_DIR="$pglite_data_dir" \
  node "$server_file" >"$pglite_log" 2>&1 &
pglite_pid=$!

ticks=0
while :; do
  if ! kill -0 "$pglite_pid" 2>/dev/null; then
    sed -n '1,160p' "$pglite_log" >&2 || true
    echo "PGlite composition server exited before readiness" >&2
    exit 1
  fi
  if grep -q '^PGLITE_READY ' "$pglite_log" 2>/dev/null; then
    break
  fi
  if [ "$ticks" -ge "$startup_timeout" ]; then
    sed -n '1,160p' "$pglite_log" >&2 || true
    echo "Timed out waiting for PGlite composition readiness" >&2
    exit 1
  fi
  sleep 1
  ticks=$((ticks + 1))
done

export PGLITE_PORT="$pglite_port"
export PGLITE_DATA_DIR="$pglite_data_dir"
export PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$pglite_port/postgres?sslmode=disable"
export OZONE_SQLITE_METADATA_FILE="$run_dir/sqlite-metadata.db"
export MOUNT_RS_OZONE_COMPOSITIONS=1
export MOUNT_RS_OZONE_COMPOSITION_HARNESS=1
echo "OZONE_COMPOSITION_PGLITE_READY endpoint=127.0.0.1:$pglite_port data=$pglite_data_dir"

if "$repo_dir/scripts/test-ozone.sh"; then
  ozone_status=0
else
  ozone_status=$?
fi
exit "$ozone_status"
