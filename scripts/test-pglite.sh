#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
server_dir="$repo_dir/tests/pglite"
port=$(node -e 'const net=require("net"); const s=net.createServer(); s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
log_file=$(mktemp "${TMPDIR:-/tmp}/mount-rs-pglite.XXXXXX")

cleanup() {
  if [ "${server_pid:-}" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

PGLITE_PORT="$port" node "$server_dir/server.mjs" >"$log_file" 2>&1 &
server_pid=$!
ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if grep -q PGLITE_READY "$log_file"; then ready=1; break; fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  sed -n '1,160p' "$log_file"
  exit 1
fi

MOUNT_RS_REQUIRE_PGLITE=1 \
PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  cargo test --locked -p mount-rs-core --test backend_parity pglite_matches_the_same_contract_when_a_socket_is_configured -- --ignored --nocapture

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  cargo test --locked -p mount-rs-pglite configured_pglite_state_survives_reconnect -- --ignored --nocapture

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  node "$repo_dir/integrations/mount-rs-napi/test/pglite.mjs"

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  node "$repo_dir/integrations/mount-rs-napi/test/factories.mjs"

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  cargo test --locked -p mount-rs-core --test fuse_backends fuse_pglite_operations_survive_connection_reopen -- --ignored --nocapture

if [ -n "${MOUNTX_SOURCE:-}" ]; then
  PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
    node "$repo_dir/scripts/check-trace-parity.mjs"
fi
