#!/bin/sh
set -eu

# Full authenticated Cloudflare R2 acceptance. The workflow that calls this
# script is intentionally main-only and budget-gated; every test below uses a
# run-specific, test-owned prefix and verifies cleanup before returning.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"

: "${MOUNTX_SOURCE:?MOUNTX_SOURCE must point to the pinned mountx checkout}"
: "${R2_ENDPOINT:?R2_ENDPOINT must be exported for the Cloudflare R2 gate}"
: "${R2_BUCKET:?R2_BUCKET must be exported for the Cloudflare R2 gate}"
: "${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID must be exported for the Cloudflare R2 gate}"
: "${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY must be exported for the Cloudflare R2 gate}"

case "$R2_ENDPOINT" in
  https://*.r2.cloudflarestorage.com) ;;
  *)
    echo "Cloudflare R2 CI requires the canonical HTTPS S3 endpoint" >&2
    exit 2
    ;;
esac

if ! command -v aws >/dev/null 2>&1; then
  echo "Cloudflare R2 CI requires the AWS CLI for cleanup verification" >&2
  exit 2
fi

run_id=${MOUNT_RS_R2_CI_RUN_ID:-local-$(date -u +%Y%m%dT%H%M%SZ)-$$}
case "$run_id" in
  *[!A-Za-z0-9._-]*)
    echo "MOUNT_RS_R2_CI_RUN_ID contains unsafe characters" >&2
    exit 2
    ;;
esac

export MOUNT_RS_PROVIDER_MATRIX_RUN_ID="${MOUNT_RS_PROVIDER_MATRIX_RUN_ID:-ci-$run_id}"
export MOUNT_RS_CLI_REMOTE_PREFIX="${MOUNT_RS_CLI_REMOTE_PREFIX:-mount-rs/cli-cloudflare/$run_id}"
export MOUNT_RS_REQUIRE_R2=1

echo "CLOUDFLARE_R2_FULL_START run=$run_id"

echo "CLOUDFLARE_R2_RUST_BACKEND_START"
"$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-core --test backend_parity \
  'cloudflare_r2_' -- --ignored --nocapture
"$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-core --test split_store \
  live_r2_blocks_with_independent_sqlite_metadata -- --ignored --nocapture
echo "CLOUDFLARE_R2_RUST_BACKEND_PASS"

echo "CLOUDFLARE_R2_PGLITE_SDK_CLI_START"
mkdir -p "$repo_dir/artifacts"
MOUNTX_SOURCE="$MOUNTX_SOURCE" \
  sh "$repo_dir/scripts/test-pglite.sh"
MOUNT_RS_PGLITE_TEST_SCOPE=benchmark \
MOUNT_RS_PGLITE_R2_BENCHMARK=1 \
  sh "$repo_dir/scripts/test-pglite.sh"
echo "CLOUDFLARE_R2_PGLITE_SDK_CLI_PASS"

echo "CLOUDFLARE_R2_TRACE_START"
# The PGlite gate above already runs the complete five-seed trace across every
# local and PGlite backend. Keep the remote R2 trace bounded to one seed by
# default: the live SDK, CLI, block, benchmark and service-evidence gates above
# exercise the remaining remote paths, while this preserves an explicit live
# differential check without repeating the local matrix over paid HTTP.
r2_trace_seeds="${MOUNT_RS_TRACE_R2_SEEDS:-4182}"
MOUNT_RS_TRACE_R2=1 \
MOUNT_RS_TRACE_BACKENDS=r2 \
MOUNT_RS_TRACE_SEEDS="$r2_trace_seeds" \
  MOUNTX_SOURCE="$MOUNTX_SOURCE" \
  node "$repo_dir/scripts/check-trace-parity.mjs"
echo "CLOUDFLARE_R2_TRACE_PASS"

echo "CLOUDFLARE_R2_RUST_CLI_START"
MOUNT_RS_RUN_CLOUDFLARE_R2_CLI=1 \
  sh "$repo_dir/scripts/test-cli-cloudflare-r2.sh"
echo "CLOUDFLARE_R2_RUST_CLI_PASS"

echo "CLOUDFLARE_R2_NODE_SDK_START"
MOUNTX_SOURCE="$MOUNTX_SOURCE" \
  pnpm --dir "$repo_dir/integrations/mount-rs-napi" test
echo "CLOUDFLARE_R2_NODE_SDK_PASS"

echo "CLOUDFLARE_R2_SERVICE_EVIDENCE_START"
sh "$repo_dir/scripts/r2-service-evidence.sh"
echo "CLOUDFLARE_R2_SERVICE_EVIDENCE_PASS"

echo "CLOUDFLARE_R2_FULL_PASS run=$run_id"
