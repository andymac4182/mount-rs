#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
mountx_source=${MOUNTX_SOURCE:-}

if [ -z "$mountx_source" ]; then
  echo "MOUNTX_SOURCE must point to a checkout of pithings/mountx" >&2
  exit 2
fi

cd "$repo_dir"

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked

MOUNTX_SOURCE="$mountx_source" node scripts/check-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-edge-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-flags-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-trace-parity.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-fuse-inodes.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-fuse-init.mjs
MOUNTX_SOURCE="$mountx_source" node scripts/check-fuse-session.mjs

pnpm --dir integrations/mount-rs-napi install --frozen-lockfile
pnpm --dir integrations/mount-rs-napi build
MOUNTX_SOURCE="$mountx_source" pnpm --dir integrations/mount-rs-napi test

pnpm --dir tests/pglite install --frozen-lockfile
MOUNTX_SOURCE="$mountx_source" ./scripts/test-pglite.sh

if [ -n "${R2_ENDPOINT:-}" ] && \
  [ -n "${R2_BUCKET:-}" ] && \
  [ -n "${R2_ACCESS_KEY_ID:-}" ] && \
  [ -n "${R2_SECRET_ACCESS_KEY:-}" ]; then
  MOUNT_RS_REQUIRE_R2=1 \
    cargo test -p mount-rs-core --test backend_parity cloudflare_r2_matches_the_same_contract_when_configured -- --ignored --nocapture
else
  echo "Skipping live Cloudflare R2 parity: R2 credentials are not configured."
fi
