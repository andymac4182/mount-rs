#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
runner="$repo_dir/scripts/rustfs-bounded-docker.py"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-rustfs-docker-timeout.XXXXXX")
hung_interpreter=$(command -v python3)

cleanup() {
  test_code=$?
  trap - EXIT INT TERM
  rmdir "$test_root" >/dev/null 2>&1 || true
  exit "$test_code"
}
trap cleanup EXIT INT TERM

# Invoke the interpreter directly with explicit arguments. On macOS, aliasing
# the interpreter binary to a basename of "docker" can dispatch the Docker
# CLI before the interpreter starts.
hung_code='import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)'
for action in create-cleanup inspect-service remove-service; do
  start_epoch=$(date +%s)
  set +e
  output=$(python3 "$runner" 1 "$action" "$hung_interpreter" -c "$hung_code" 2>&1)
  status=$?
  set -e
  elapsed=$(( $(date +%s) - start_epoch ))

  if [ "$status" -ne 124 ]; then
    echo "expected bounded timeout for $action, got $status" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
  case "$output" in
    *"RUSTFS_DOCKER_TIMEOUT action=$action"*) ;;
    *)
      echo "missing bounded timeout marker for $action" >&2
      printf '%s\n' "$output" >&2
      exit 1
      ;;
  esac
  case "$output" in
    *"RUSTFS_DOCKER_GROUP_CLEANUP_FAIL"*)
      echo "hung Docker shim was not reaped for $action" >&2
      printf '%s\n' "$output" >&2
      exit 1
      ;;
  esac
  if [ "$elapsed" -gt 8 ]; then
    echo "bounded Docker shim action exceeded cleanup deadline: $action (${elapsed}s)" >&2
    exit 1
  fi
done

echo "RUSTFS_BOUNDED_DOCKER_TIMEOUT_PASS actions=create-cleanup,inspect-service,remove-service"
