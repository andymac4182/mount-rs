#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"
cd "$repo_dir"
provider=${1:-sqlite}
case "$provider" in
  sqlite|pglite|tidb|foundationdb) ;;
  *) echo "usage: $0 [sqlite|pglite|tidb|foundationdb]" >&2; exit 2 ;;
esac
: "${MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR:?supply a retained output directory}"
mkdir -p "$MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR"
output_dir=$(CDPATH= cd -- "$MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR" && pwd)
export MOUNT_RS_REMOTE_SATURATION_PROVIDER="$provider"
export MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS=32
export MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=15
export MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS=3
export MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=0

if [ "$provider" = foundationdb ]; then
  export MOUNT_RS_FOUNDATIONDB_BENCH_OUTPUT_DIR="$output_dir"
  exec sh "$repo_dir/scripts/bench-remote-foundationdb.sh"
fi
if [ "$provider" = tidb ]; then
  # The owned cluster harness supplies the endpoint to this same script.
  if [ "${MOUNT_RS_REMOTE_COMPARISON_EXISTING_TIDB:-0}" != 1 ]; then
    export MOUNT_RS_REMOTE_COMPARISON_EXISTING_TIDB=1
    export MOUNT_RS_REMOTE_COMPARISON_CONSUMER="$repo_dir/scripts/bench-remote-providers.sh"
    export MOUNT_RS_TIDB_COMPOSITION_COMMAND='exec sh "$MOUNT_RS_REMOTE_COMPARISON_CONSUMER" tidb'
    exec sh "$repo_dir/scripts/test-tidb.sh"
  fi
  : "${MOUNT_RS_TIDB_URL:?owned TiDB harness must supply endpoint}"
fi

owned_dir=""
server_pid=""
cleanup() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  if [ -n "$owned_dir" ]; then rm -rf -- "$owned_dir"; fi
}
trap cleanup EXIT INT TERM
if [ "$provider" = pglite ]; then
  owned_dir=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-pglite-comparison.XXXXXX")
  port=$(node -e 'const net=require("net");const s=net.createServer();s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
  PGLITE_PORT="$port" PGLITE_DATA_DIR="$owned_dir/data" PGLITE_MAX_CONNECTIONS=32 \
    node "$repo_dir/tests/pglite/server.mjs" > "$output_dir/pglite-server.log" 2>&1 &
  server_pid=$!
  ready=0
  for attempt in $(seq 1 30); do
    if ! kill -0 "$server_pid" 2>/dev/null; then break; fi
    if rg -q PGLITE_READY "$output_dir/pglite-server.log"; then ready=1; break; fi
    sleep 1
  done
  if [ "$ready" -ne 1 ]; then echo "owned PGlite server failed readiness; see retained log" >&2; exit 1; fi
  export PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable"
  pglite_version=$(node -p 'require("./tests/pglite/node_modules/@electric-sql/pglite/package.json").version')
  socket_version=$(node -p 'require("./tests/pglite/node_modules/@electric-sql/pglite-socket/package.json").version')
  export MOUNT_RS_PGLITE_BENCH_IDENTITY="owned PGlite $pglite_version, socket $socket_version, persistent data directory"
fi

MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=read \
MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=1,2,4,8,16 \
MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT="$output_dir/$provider-read.json" \
  "$repo_dir/scripts/cargo-shared" test --release --locked -p mount-rs-service \
    --test quic_tidb_saturation -- --ignored --nocapture --test-threads=1
MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=read,write \
MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=1,2 \
MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT="$output_dir/$provider-read-write.json" \
  "$repo_dir/scripts/cargo-shared" test --release --locked -p mount-rs-service \
    --test quic_tidb_saturation -- --ignored --nocapture --test-threads=1
