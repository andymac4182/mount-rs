#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
mountx_source=${MOUNTX_SOURCE:-}

if [ -z "$mountx_source" ]; then
  echo "MOUNTX_SOURCE must point to a checkout of pithings/mountx" >&2
  exit 2
fi

cd "$repo_dir"
. "$repo_dir/scripts/cargo-shared-env.sh"

# This script starts its own isolated PGlite server in the dedicated gate
# below. A caller's inherited URL must not make earlier suites connect to a
# stale or unrelated server before that lifecycle has been established.
unset PGLITE_DATABASE_URL

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked

# Standalone acceptance packets keep the W01 concurrency, provider-consumer,
# and SQLite reliability checks out of the root workspace while making them
# part of the normal gate. Provider rows that need PGlite/R2 run inside the
# isolated server lifecycle below.
cargo fmt --manifest-path tests/core_concurrency/Cargo.toml -- --check
cargo fmt --manifest-path tests/provider_matrix/Cargo.toml -- --check
cargo fmt --manifest-path tests/sqlite_matrix/Cargo.toml -- --check
MOUNTX_SOURCE="$mountx_source" node tests/core_concurrency/check.mjs
cargo test --manifest-path tests/sqlite_matrix/Cargo.toml --quiet --locked
cargo run --quiet --manifest-path tests/sqlite_matrix/Cargo.toml --locked

MOUNTX_SOURCE="$mountx_source" node scripts/check-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-edge-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-flags-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-auto-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-host-parity.mjs
# The dedicated PGlite gate below owns the lifecycle of its local server. Do
# not let a caller's inherited connection URL make this preflight try to
# connect before that server has been started.
env -u PGLITE_DATABASE_URL MOUNTX_SOURCE="$mountx_source" node scripts/check-trace-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-fuse-inodes.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-fuse-init.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-fuse-session.mjs

pnpm --dir integrations/mount-rs-napi install --frozen-lockfile
pnpm --dir integrations/mount-rs-napi build
MOUNTX_SOURCE="$mountx_source" pnpm --dir integrations/mount-rs-napi test
pnpm --dir integrations/mount-rs-virtual-fs install --frozen-lockfile --ignore-scripts
pnpm --dir integrations/mount-rs-virtual-fs test
pnpm --dir integrations/mount-rs-virtual-fs typecheck
node benchmarks/storage/test.mjs
MOUNTX_SOURCE="$mountx_source" node benchmarks/storage/runner.mjs --smoke --output artifacts/storage-smoke.json
pnpm --dir tests/upstream install --frozen-lockfile
pnpm --dir "$mountx_source" install --frozen-lockfile --ignore-scripts
MOUNTX_SOURCE="$mountx_source" pnpm --dir tests/upstream test
MOUNTX_SOURCE="$mountx_source" node scripts/check-kv-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-http-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/test-http-early-rejection.mjs

pnpm --dir tests/pglite install --frozen-lockfile
MOUNTX_SOURCE="$mountx_source" ./scripts/test-pglite.sh

if [ -n "${R2_ENDPOINT:-}" ] && \
  [ -n "${R2_BUCKET:-}" ] && \
  [ -n "${R2_ACCESS_KEY_ID:-}" ] && \
  [ -n "${R2_SECRET_ACCESS_KEY:-}" ]; then
  MOUNT_RS_REQUIRE_R2=1 \
    cargo test --locked -p mount-rs-core --test backend_parity cloudflare_r2_matches_the_same_contract_when_configured -- --ignored --nocapture
  MOUNT_RS_REQUIRE_R2=1 \
    cargo test --locked -p mount-rs-core --test backend_parity cloudflare_r2_rejects_concurrent_snapshot_publication_with_fresh_clients -- --ignored --nocapture
  cargo test --locked -p mount-rs-core --test split_store live_r2_blocks_with_independent_sqlite_metadata -- --ignored --nocapture
  MOUNT_RS_TRACE_R2=1 MOUNTX_SOURCE="$mountx_source" node scripts/check-trace-parity.mjs
  if [ "${MOUNT_RS_RUN_CLOUDFLARE_R2_CLI:-0}" = "1" ]; then
    sh scripts/test-cli-cloudflare-r2.sh
  fi
else
  echo "Skipping live Cloudflare R2 parity: R2 credentials are not configured."
fi
