#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"
server_dir="$repo_dir/tests/pglite"
port=$(node -e 'const net=require("net"); const s=net.createServer(); s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
log_file=$(mktemp "${TMPDIR:-/tmp}/mount-rs-pglite.XXXXXX")

cleanup() {
  if [ "${server_pid:-}" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -f -- "$log_file"
}
trap cleanup EXIT INT TERM

# This harness owns only an in-memory server. Other lifecycle runners set
# PGLITE_DATA_DIR explicitly for their own disposable persistent state.
(
  unset PGLITE_DATA_DIR
  PGLITE_PORT="$port" exec node "$server_dir/server.mjs"
) >"$log_file" 2>&1 &
server_pid=$!
ready=0
# 60 s: the server printed nothing within 20 s on a loaded macOS Intel
# runner, where starting PGlite's WebAssembly is slow.
waited=0
while [ "$waited" -lt 60 ]; do
  if grep -q PGLITE_READY "$log_file"; then ready=1; break; fi
  if ! kill -0 "$server_pid" 2>/dev/null; then break; fi
  sleep 1
  waited=$((waited + 1))
done
if [ "$ready" -ne 1 ]; then
  echo "PGlite server was not ready after ${waited}s; its output follows" >&2
  sed -n '1,160p' "$log_file" >&2
  exit 1
fi

url="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable"
cargo_shared="$repo_dir/scripts/cargo-shared"

# Every command below runs through `step`, with a limit in seconds and a
# name. A step that passes its limit is stopped, with everything it started,
# and the script fails saying which step it was and what the server logged.
# Before this a step that never finished (or a build nobody could see: the
# job this gate runs in was once cancelled at its own limit while a quiet
# `cargo build` was compiling) left only a job that had gone silent.
#
# The limits are several times what the steps take on the slowest hosted
# runner with nothing built, so they only end a step that is stuck.
# MOUNT_RS_PGLITE_GATE_LIMIT_SECONDS bounds the whole script as well: a step
# gets what is left of it when that is less than its own limit. A caller
# that is itself cut off after some time sets it a little below that, so
# that the step is named here first.
gate_limit=${MOUNT_RS_PGLITE_GATE_LIMIT_SECONDS:-5400}
gate_started=$(date +%s)

step() {
  step_limit=$1
  step_name=$2
  shift 2
  step_left=$((gate_limit - ($(date +%s) - gate_started)))
  if [ "$step_left" -lt 1 ]; then
    step_left=1
  fi
  if [ "$step_left" -lt "$step_limit" ]; then
    step_limit=$step_left
  fi
  echo "==> PGlite gate: $step_name (limit ${step_limit}s)" >&2
  step_status=0
  node "$repo_dir/scripts/run-bounded.mjs" "$step_limit" "$@" || step_status=$?
  if [ "$step_status" -eq 0 ]; then
    return 0
  fi
  if [ "$step_status" -eq 124 ]; then
    echo "PGlite gate: step '$step_name' did not finish within ${step_limit}s and was stopped" >&2
    echo "  (its own limit, or what was left of the gate's ${gate_limit}s)" >&2
  else
    echo "PGlite gate: step '$step_name' failed with status $step_status" >&2
  fi
  if kill -0 "$server_pid" 2>/dev/null; then
    echo "--- the PGlite server is still running; the end of its output ---" >&2
  else
    echo "--- the PGlite server has exited; the end of its output ---" >&2
  fi
  tail -n 200 "$log_file" >&2
  echo "--- end of the PGlite server's output ---" >&2
  exit "$step_status"
}

# A cargo test that may have to compile first, a Node script, and a build.
cargo_limit=1200
node_limit=600
build_limit=2400

# Focused per-inode CAS fixture; avoids the broad pgwire client fanout tests.
if [ "${MOUNT_RS_PGLITE_TEST_SCOPE:-}" = "inode" ]; then
  step "$cargo_limit" "per-inode CAS fixture" \
    env PGLITE_DATABASE_URL="$url" \
    "$cargo_shared" test --offline --locked -p mount-rs-pglite --lib \
      storage::tests::inode_guards_cas_fold_and_reopen -- --ignored --exact \
      --nocapture
  exit 0
fi

if [ "${MOUNT_RS_PGLITE_TEST_SCOPE:-}" = "benchmark" ]; then
  benchmark_providers="mount-rs-pglite,mount-rs-split-pglite"
  if [ "${MOUNT_RS_PGLITE_R2_BENCHMARK:-0}" = "1" ]; then
    benchmark_providers="$benchmark_providers,mount-rs-split-pglite-r2"
  fi
  step "$node_limit" "PGlite provider benchmark smoke" \
    env PGLITE_DATABASE_URL="$url" MOUNT_RS_PGLITE_DURABLE=0 \
    node "$repo_dir/benchmarks/storage/runner.mjs" --smoke --providers \
      "$benchmark_providers" --output \
      "$repo_dir/artifacts/storage-pglite-smoke.json"
  exit 0
fi

if [ "${MOUNT_RS_PGLITE_TEST_SCOPE:-}" = "native-fuse" ]; then
  step "$cargo_limit" "native FUSE mount of PGlite across a reopen" \
    env PGLITE_DATABASE_URL="$url" \
    "$cargo_shared" test --locked -p mount-rs-core --test native_fuse_backends \
      mounted_pglite_persists_through_connection_reopen -- --ignored \
      --nocapture
  step "$cargo_limit" "SQLite on a natively mounted PGlite volume" \
    env MOUNT_RS_RUN_NATIVE_PGLITE_SQLITE=1 PGLITE_DATABASE_URL="$url" \
    "$cargo_shared" test --locked -p mount-rs-core --test native_pglite_sqlite \
      -- --ignored --nocapture
  exit 0
fi

if [ "${MOUNT_RS_PGLITE_TEST_SCOPE:-}" = "native-nfs" ]; then
  if [ "$(uname -s)" != "Darwin" ]; then
    echo "native-nfs scope requires macOS" >&2
    exit 1
  fi
  step "$cargo_limit" "CLI native NFS mount of a PGlite config" \
    env MOUNT_RS_CLI_NATIVE_NFS=1 PGLITE_DATABASE_URL="$url" \
    "$cargo_shared" test --locked -p mount-rs-cli --test native_lifecycle \
      cli_nfs_pglite_config_binary_mounts_io_and_reopens -- --ignored --exact \
      --nocapture
  exit 0
fi

# Exercise bounded socket-slot cleanup against a real in-process PGlite server.
step "$node_limit" "server socket-slot release" \
  node "$repo_dir/providers/mount-rs-pglite/test/server_slot_release.mjs"

# Verify detach failure remains rejected and cannot release a bounded slot.
step "$node_limit" "server cleanup failure" \
  node --unhandled-rejections=strict \
    "$repo_dir/providers/mount-rs-pglite/test/server_cleanup_failure.mjs"

step "$cargo_limit" "backend parity contract" \
  env MOUNT_RS_REQUIRE_PGLITE=1 PGLITE_DATABASE_URL="$url" \
  "$cargo_shared" test --locked -p mount-rs-core --test backend_parity \
    pglite_matches_the_same_contract_when_a_socket_is_configured -- --ignored \
    --nocapture

step "$cargo_limit" "state survives a reconnect" \
  env PGLITE_DATABASE_URL="$url" \
  "$cargo_shared" test --locked -p mount-rs-pglite \
    configured_pglite_state_survives_reconnect -- --ignored --nocapture

step "$cargo_limit" "readiness handshake and connection slots" \
  "$cargo_shared" test --locked -p mount-rs-pglite \
    readiness_handshake_does_not_consume_bounded_connection_slot -- --ignored \
    --nocapture

step "$cargo_limit" "split-store durability, fencing and CAS" \
  "$cargo_shared" test --locked -p mount-rs-pglite \
    split_stores_enforce_durability_fencing_cas_and_immutable_blocks -- \
    --ignored --nocapture

step "$cargo_limit" "versioning across a reconnect" \
  env PGLITE_DATABASE_URL="$url" \
  "$cargo_shared" test --locked -p mount-rs-pglite --test versioned \
    pglite_versioning_snapshot_history_view_survives_reconnect -- --ignored \
    --nocapture

step "$cargo_limit" "SQLite VFS round trip" \
  env PGLITE_DATABASE_URL="$url" \
  "$cargo_shared" test --locked -p mount-rs-sqlite-vfs --features \
    pglite-harness --test pglite \
    pglite_sqlite_vfs_round_trip_and_fresh_provider_reconnect -- --ignored \
    --nocapture

step "$cargo_limit" "cancellation-safe close" \
  "$cargo_shared" test --locked -p mount-rs-pglite \
    close_is_shared_cancellation_safe -- --ignored --nocapture

step "$cargo_limit" "server lifecycle and restart recovery" \
  env MOUNT_RS_RUN_PGLITE_SERVER_LIFECYCLE=1 \
  "$cargo_shared" test --locked -p mount-rs-core --test \
    pglite_server_lifecycle -- --ignored --nocapture

step "$cargo_limit" "split-store composition" \
  env PGLITE_DATABASE_URL="$url" \
  "$cargo_shared" test --locked -p mount-rs-core --test split_store \
    pglite_metadata_and_blocks_compose_independently -- --ignored --nocapture

step "$node_limit" "N-API PGlite integration" \
  env PGLITE_DATABASE_URL="$url" \
  node "$repo_dir/bindings/mount-rs-napi/test/pglite.mjs"

step "$node_limit" "N-API factories" \
  env PGLITE_DATABASE_URL="$url" \
  node "$repo_dir/bindings/mount-rs-napi/test/factories.mjs"

step "$node_limit" "N-API chunked integration" \
  env PGLITE_DATABASE_URL="$url" \
  node "$repo_dir/bindings/mount-rs-napi/test/chunked.mjs"

step "$cargo_limit" "FUSE operations across a reopen" \
  env PGLITE_DATABASE_URL="$url" \
  "$cargo_shared" test --locked -p mount-rs-core --test fuse_backends \
    fuse_pglite_operations_survive_connection_reopen -- --ignored --nocapture

# Run the provider matrix while this isolated PGlite server is alive. The
# matrix reports missing R2 credentials as skips and scopes any live objects
# to a per-run prefix; it never treats RustFS or local object storage as R2.
provider_matrix_run_id="${MOUNT_RS_PROVIDER_MATRIX_RUN_ID:-pglite-$port}"
# Built first, and not quietly: `cargo run --quiet` below otherwise compiles
# the matrix's own dependency graph for minutes without a line of output.
step "$build_limit" "provider matrix: build the Rust SDK cases" \
  "$cargo_shared" build --manifest-path "$repo_dir/tests/provider_matrix/Cargo.toml" \
    --locked
step "$cargo_limit" "provider matrix: Rust SDK" \
  env PGLITE_DATABASE_URL="$url" MOUNT_RS_PROVIDER_MATRIX_RUN_ID="$provider_matrix_run_id" \
  "$cargo_shared" run --quiet --manifest-path \
    "$repo_dir/tests/provider_matrix/Cargo.toml" --locked
step "$node_limit" "provider matrix: Node SDK" \
  env PGLITE_DATABASE_URL="$url" MOUNT_RS_PROVIDER_MATRIX_RUN_ID="$provider_matrix_run_id" \
  node "$repo_dir/tests/provider_matrix/node-sdk.mjs"
# The CLI cases build this binary themselves, quietly; do it here where the
# compiler's progress is visible, so theirs finds it built.
step "$build_limit" "provider matrix: build the CLI" \
  "$cargo_shared" build --offline --locked -p mount-rs-cli
step "$build_limit" "provider matrix: CLI" \
  env PGLITE_DATABASE_URL="$url" MOUNT_RS_PROVIDER_MATRIX_RUN_ID="$provider_matrix_run_id" \
  node "$repo_dir/tests/provider_matrix/cli.mjs"

if [ -n "${MOUNTX_SOURCE:-}" ]; then
  step "$build_limit" "upstream conformance suite" \
    env PGLITE_DATABASE_URL="$url" \
    pnpm --dir "$repo_dir/tests/upstream" test
  step "$node_limit" "trace parity" \
    env PGLITE_DATABASE_URL="$url" \
    node "$repo_dir/scripts/check-trace-parity.mjs"
fi

if [ -n "${R2_ENDPOINT:-}" ] && [ -n "${R2_BUCKET:-}" ] && \
   [ -n "${R2_ACCESS_KEY_ID:-}" ] && [ -n "${R2_SECRET_ACCESS_KEY:-}" ]; then
  step "$cargo_limit" "live R2 blocks with PGlite metadata" \
    env PGLITE_DATABASE_URL="$url" \
    "$cargo_shared" test --locked -p mount-rs-core --test split_store \
      live_r2_blocks_with_independent_pglite_metadata -- --ignored --nocapture
fi
