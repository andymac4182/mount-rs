#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ "$(uname -s)" != "Darwin" ]; then
  echo "PGlite native NFS isolation check requires macOS" >&2
  exit 1
fi

ambient_dir=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-pglite-ambient.XXXXXX")
sentinel="$ambient_dir/sentinel"
printf '%s' 'ambient PGlite state must stay untouched' >"$sentinel"

cleanup() {
  rm -f -- "$sentinel"
  if ! rmdir "$ambient_dir" 2>/dev/null; then
    echo "ambient PGlite test directory retained for inspection: $ambient_dir" >&2
  fi
}
trap cleanup EXIT INT TERM

PGLITE_DATA_DIR="$ambient_dir" MOUNT_RS_PGLITE_TEST_SCOPE=native-nfs \
  "$repo_dir/scripts/test-pglite.sh"

if [ "$(ls -A "$ambient_dir")" != "sentinel" ] || \
   [ "$(cat "$sentinel")" != 'ambient PGlite state must stay untouched' ]; then
  echo "PGlite native NFS scope changed the caller's PGLITE_DATA_DIR" >&2
  exit 1
fi

echo "PGLITE_AMBIENT_DATA_DIR_UNTOUCHED"
