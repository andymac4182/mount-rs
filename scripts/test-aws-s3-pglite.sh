#!/bin/sh
set -eu

# Run the AWS S3 block provider with an isolated external PGlite metadata
# server. The parent AWS harness owns the credentials, prefix claim, and
# version-aware cleanup; this script owns only the PGlite child and log file.
umask 077

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"
export AWS_EC2_METADATA_DISABLED=true

if [ "${MOUNT_RS_RUN_AWS_S3_PGLITE:-0}" != "1" ]; then
  echo "AWS_S3_PGLITE_TEST_SKIPPED reason=opt_in_required"
  exit 0
fi

: "${AWS_S3_TEST_BUCKET:?AWS_S3_TEST_BUCKET must be set by the AWS S3 harness}"
: "${AWS_S3_TEST_REGION:?AWS_S3_TEST_REGION must be set by the AWS S3 harness}"
: "${AWS_S3_TEST_PREFIX:?AWS_S3_TEST_PREFIX must be the child prefix owned by this run}"
: "${AWS_ACCESS_KEY_ID:?AWS_ACCESS_KEY_ID must be exported by the AWS S3 harness}"
: "${AWS_SECRET_ACCESS_KEY:?AWS_SECRET_ACCESS_KEY must be exported by the AWS S3 harness}"

case "$AWS_S3_TEST_PREFIX" in
  mount-rs-tests/aws-s3/*) ;;
  *)
    echo "AWS_S3_TEST_PREFIX must remain below mount-rs-tests/aws-s3/" >&2
    exit 2
    ;;
esac
case "$AWS_S3_TEST_PREFIX" in
  ''|*/|*[!A-Za-z0-9_./-]*|*//*|*/../*|*/..|*/./*|*/.)
    echo "AWS_S3_TEST_PREFIX contains an unsafe path" >&2
    exit 2
    ;;
esac

if ! command -v node >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
  echo "AWS S3 plus PGlite test requires Node.js and cargo" >&2
  exit 2
fi

server_dir="$repo_dir/tests/pglite"
port=$(node -e 'const net=require("net"); const s=net.createServer(); s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
log_file=$(mktemp "${TMPDIR:-/tmp}/mount-rs-aws-s3-pglite.XXXXXX")
server_pid=

cleanup() {
  if [ -n "${server_pid:-}" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -f "$log_file"
}
trap cleanup EXIT INT TERM

PGLITE_PORT="$port" node "$server_dir/server.mjs" >"$log_file" 2>&1 &
server_pid=$!
ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if grep -q PGLITE_READY "$log_file"; then
    ready=1
    break
  fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  sed -n '1,160p' "$log_file"
  echo "AWS_S3_PGLITE_TEST_BLOCKED reason=pglite_server_not_ready" >&2
  exit 1
fi

PGLITE_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:$port/postgres?sslmode=disable" \
AWS_DEFAULT_REGION="$AWS_S3_TEST_REGION" \
AWS_S3_TEST_BUCKET="$AWS_S3_TEST_BUCKET" \
AWS_S3_TEST_REGION="$AWS_S3_TEST_REGION" \
AWS_S3_TEST_PREFIX="$AWS_S3_TEST_PREFIX" \
CARGO_NET_OFFLINE=true \
  "$repo_dir/scripts/cargo-shared" test \
    --manifest-path "$repo_dir/Cargo.toml" \
    --locked \
    -p mount-rs-core \
    --test split_store \
    live_aws_s3_blocks_with_independent_pglite_metadata \
    -- --exact --ignored --nocapture

echo "AWS_S3_PGLITE_TEST_PASS prefix=$AWS_S3_TEST_PREFIX"
