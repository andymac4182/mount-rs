#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
runner="$repo_dir/scripts/rustfs-combo-runner.py"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-rustfs-combo.XXXXXX")
pid_file="$test_root/child.pid"
grandchild_pid_file="$test_root/grandchild.pid"

cleanup() {
  test_code=$?
  trap - EXIT INT TERM
  if [ -f "$pid_file" ]; then
    group_pid=$(sed -n '1p' "$pid_file" 2>/dev/null || true)
    case "$group_pid" in
      ''|*[!0-9]*) ;;
      *)
        python3 - "$group_pid" <<'PY' >/dev/null 2>&1 || true
import os
import signal
import sys

try:
    os.killpg(int(sys.argv[1]), signal.SIGKILL)
except ProcessLookupError:
    pass
PY
        ;;
    esac
  fi
  rm -rf "$test_root"
  exit "$test_code"
}
trap cleanup EXIT INT TERM

if ! command -v python3 >/dev/null 2>&1; then
  echo "RustFS combo timeout regression requires python3" >&2
  exit 2
fi

command=$(python3 - "$grandchild_pid_file" <<'PY'
import shlex
import sys

grandchild = (
    "import os, signal, time; "
    "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
    "open(os.environ['RUSTFS_COMBO_TEST_GRANDCHILD_PID'], 'w', encoding='ascii').write(str(os.getpid())); "
    "time.sleep(60)"
)
parent = (
    "import os, subprocess, sys, time; "
    f"subprocess.Popen([sys.executable, '-c', {grandchild!r}]); "
    "time.sleep(60)"
)
print("python3 -c " + shlex.quote(parent))
PY
)

set +e
output=$(RUSTFS_COMBO_TEST_GRANDCHILD_PID="$grandchild_pid_file" \
  python3 "$runner" 1 "$repo_dir" "$pid_file" timeout-regression "$command" 2>&1)
status=$?
set -e

if [ "$status" -ne 124 ]; then
  echo "expected timeout status 124, got $status" >&2
  printf '%s\n' "$output" >&2
  exit 1
fi
case "$output" in
  *"RUSTFS_COMBO_TIMEOUT name=timeout-regression"*) ;;
  *)
    echo "missing timeout marker" >&2
    printf '%s\n' "$output" >&2
    exit 1
    ;;
esac
case "$output" in
  *"RUSTFS_COMBO_PASS"*)
    echo "timeout path falsely reported PASS" >&2
    printf '%s\n' "$output" >&2
    exit 1
    ;;
esac

grandchild_pid=""
ticks=0
while [ ! -s "$grandchild_pid_file" ] && [ "$ticks" -lt 20 ]; do
  sleep 0.1
  ticks=$((ticks + 1))
done
grandchild_pid=$(sed -n '1p' "$grandchild_pid_file" 2>/dev/null || true)
case "$grandchild_pid" in
  ''|*[!0-9]*)
    echo "grandchild PID was not recorded" >&2
    exit 1
    ;;
esac

ticks=0
while kill -0 "$grandchild_pid" 2>/dev/null && [ "$ticks" -lt 20 ]; do
  sleep 0.1
  ticks=$((ticks + 1))
done
if kill -0 "$grandchild_pid" 2>/dev/null; then
  state=$(ps -p "$grandchild_pid" -o stat= 2>/dev/null | tr -d ' ' || true)
  case "$state" in
    Z*) ;;
    *)
      echo "grandchild remains alive: pid=$grandchild_pid state=$state" >&2
      exit 1
      ;;
  esac
fi

echo "RUSTFS_COMBO_TIMEOUT_REGRESSION_PASS status=$status grandchild=$grandchild_pid"
