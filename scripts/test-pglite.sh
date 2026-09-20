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

if [ "${MOUNT_RS_PGLITE_TEST_SCOPE:-}" = "benchmark" ]; then
  PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  MOUNT_RS_PGLITE_DURABLE=0 \
    node "$repo_dir/benchmarks/storage/runner.mjs" --smoke \
      --providers mount-rs-pglite,mount-rs-split-pglite \
      --output "$repo_dir/artifacts/storage-pglite-smoke.json"
  exit 0
fi

if [ "${MOUNT_RS_PGLITE_TEST_SCOPE:-}" = "native-fuse" ]; then
  PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
    cargo test --locked -p mount-rs-core --test native_fuse_backends mounted_pglite_persists_through_connection_reopen -- --ignored --nocapture
  MOUNT_RS_RUN_NATIVE_PGLITE_SQLITE=1 \
  PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
    cargo test --locked -p mount-rs-core --test native_pglite_sqlite -- --ignored --nocapture
  exit 0
fi

# Exercise bounded socket-slot cleanup against a real in-process PGlite server.
node "$repo_dir/integrations/mount-rs-pglite/test/server_slot_release.mjs"

# Verify detach failure remains rejected and cannot release a bounded slot.
node --unhandled-rejections=strict \
  "$repo_dir/integrations/mount-rs-pglite/test/server_cleanup_failure.mjs"

MOUNT_RS_REQUIRE_PGLITE=1 \
PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  cargo test --locked -p mount-rs-core --test backend_parity pglite_matches_the_same_contract_when_a_socket_is_configured -- --ignored --nocapture

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  cargo test --locked -p mount-rs-pglite configured_pglite_state_survives_reconnect -- --ignored --nocapture

cargo test --locked -p mount-rs-pglite readiness_handshake_does_not_consume_bounded_connection_slot -- --ignored --nocapture

cargo test --locked -p mount-rs-pglite split_stores_enforce_durability_fencing_cas_and_immutable_blocks -- --ignored --nocapture
cargo test --locked -p mount-rs-pglite close_is_shared_cancellation_safe -- --ignored --nocapture

MOUNT_RS_RUN_PGLITE_SERVER_LIFECYCLE=1 \
  cargo test --locked -p mount-rs-core --test pglite_server_lifecycle -- --ignored --nocapture

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  cargo test --locked -p mount-rs-core --test split_store pglite_metadata_and_blocks_compose_independently -- --ignored --nocapture

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  node "$repo_dir/integrations/mount-rs-napi/test/pglite.mjs"

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  node "$repo_dir/integrations/mount-rs-napi/test/factories.mjs"

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  node "$repo_dir/integrations/mount-rs-napi/test/chunked.mjs"

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
  cargo test --locked -p mount-rs-core --test fuse_backends fuse_pglite_operations_survive_connection_reopen -- --ignored --nocapture

# Run the provider matrix while this isolated PGlite server is alive. The
# matrix reports missing R2 credentials as skips and scopes any live objects
# to a per-run prefix; it never treats RustFS or local object storage as R2.
provider_matrix_url="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable"
provider_matrix_run_id="pglite-$port"
PGLITE_DATABASE_URL="$provider_matrix_url" \
MOUNT_RS_PROVIDER_MATRIX_RUN_ID="$provider_matrix_run_id" \
  cargo run --quiet --manifest-path "$repo_dir/tests/provider_matrix/Cargo.toml" --locked
PGLITE_DATABASE_URL="$provider_matrix_url" \
MOUNT_RS_PROVIDER_MATRIX_RUN_ID="$provider_matrix_run_id" \
  node "$repo_dir/tests/provider_matrix/node-sdk.mjs"
PGLITE_DATABASE_URL="$provider_matrix_url" \
MOUNT_RS_PROVIDER_MATRIX_RUN_ID="$provider_matrix_run_id" \
  node "$repo_dir/tests/provider_matrix/cli.mjs"

if [ -n "${MOUNTX_SOURCE:-}" ]; then
  PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
    pnpm --dir "$repo_dir/tests/upstream" test
  PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
    node "$repo_dir/scripts/check-trace-parity.mjs"
fi

if [ -n "${R2_ENDPOINT:-}" ] && [ -n "${R2_BUCKET:-}" ] && \
   [ -n "${R2_ACCESS_KEY_ID:-}" ] && [ -n "${R2_SECRET_ACCESS_KEY:-}" ]; then
  PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
    cargo test --locked -p mount-rs-core --test split_store live_r2_blocks_with_independent_pglite_metadata -- --ignored --nocapture
fi
