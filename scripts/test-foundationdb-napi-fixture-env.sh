#!/bin/sh
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/foundationdb-napi-fixture-env.sh"

failures=0
fail() {
  echo "FOUNDATIONDB_NAPI_FIXTURE_ENV_FAILED $*" >&2
  failures=$((failures + 1))
}

capture=$(mktemp)
env_capture=$(mktemp)
preflight_capture=$(mktemp)
fake_docker_dir=$(mktemp -d)
trap 'rm -f "$capture" "$env_capture" "$preflight_capture"; rm -rf "$fake_docker_dir"' EXIT INT TERM
docker() {
  printf '%s\n' "$@" > "$capture"
  env | grep -E '^R2_(BUCKET|ACCESS_KEY_ID|SECRET_ACCESS_KEY)=' > "$env_capture" || :
}

cat > "$fake_docker_dir/docker" <<'EOF'
#!/bin/sh
printf 'called\n' >> "$NAPI_FIXTURE_DOCKER_CALLS"
exit 1
EOF
chmod +x "$fake_docker_dir/docker"

unset MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER RUSTFS_REGION RUSTFS_ENDPOINT
R2_BUCKET=canonical-fixture
R2_ACCESS_KEY_ID=canonical-key
R2_SECRET_ACCESS_KEY=canonical-secret
export R2_BUCKET R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY
configure_foundationdb_napi_fixture generic
[ "$MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER" = r2 ] || fail canonical_default
foundationdb_napi_run_client https://canonical.example node:24 true
grep -Fxq 'MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER=r2' "$capture" || fail canonical_forwarding
grep -Fxq 'R2_ENDPOINT=https://canonical.example' "$capture" || fail canonical_endpoint_forwarding
grep -Fxq 'R2_BUCKET' "$capture" || fail canonical_bucket_argument
grep -Fxq 'R2_BUCKET=canonical-fixture' "$env_capture" || fail canonical_bucket_environment
grep -Fxq 'R2_ACCESS_KEY_ID' "$capture" || fail canonical_access_key_argument
grep -Fxq 'R2_SECRET_ACCESS_KEY' "$capture" || fail canonical_secret_argument
grep -Fxq 'R2_ACCESS_KEY_ID=canonical-key' "$env_capture" || fail canonical_access_key_environment
grep -Fxq 'R2_SECRET_ACCESS_KEY=canonical-secret' "$env_capture" || fail canonical_secret_environment
if grep -Fq 'canonical-key' "$capture" || grep -Fq 'canonical-secret' "$capture"; then fail canonical_credential_in_arguments; fi
[ "$R2_ACCESS_KEY_ID" = canonical-key ] && [ "$R2_SECRET_ACCESS_KEY" = canonical-secret ] || fail canonical_caller_environment_changed

unset MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER
RUSTFS_ENDPOINT=http://rustfs.local
RUSTFS_BUCKET=fixture
RUSTFS_ACCESS_KEY_ID=fixture-key
RUSTFS_SECRET_ACCESS_KEY=fixture-secret
RUSTFS_REGION=us-east-1
export RUSTFS_ENDPOINT RUSTFS_BUCKET RUSTFS_ACCESS_KEY_ID RUSTFS_SECRET_ACCESS_KEY RUSTFS_REGION
unset R2_ENDPOINT R2_BUCKET R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY
configure_foundationdb_napi_fixture rustfs
[ "$MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER" = rustfs ] || fail rustfs_selection
foundationdb_napi_run_client http://rustfs.local node:24 true
grep -Fxq 'MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER=rustfs' "$capture" || fail rustfs_forwarding
grep -Fxq 'RUSTFS_REGION=us-east-1' "$capture" || fail rustfs_region_forwarding
grep -Fxq 'R2_BUCKET' "$capture" || fail rustfs_bucket_argument
grep -Fxq 'R2_BUCKET=fixture' "$env_capture" || fail rustfs_bucket_mapping
grep -Fxq 'R2_ACCESS_KEY_ID' "$capture" || fail rustfs_access_key_argument
grep -Fxq 'R2_SECRET_ACCESS_KEY' "$capture" || fail rustfs_secret_argument
grep -Fxq 'R2_ACCESS_KEY_ID=fixture-key' "$env_capture" || fail rustfs_access_key_mapping
grep -Fxq 'R2_SECRET_ACCESS_KEY=fixture-secret' "$env_capture" || fail rustfs_secret_mapping
if grep -Fq 'fixture-key' "$capture" || grep -Fq 'fixture-secret' "$capture"; then fail rustfs_credential_in_arguments; fi
[ -z "${R2_ACCESS_KEY_ID+x}" ] && [ -z "${R2_SECRET_ACCESS_KEY+x}" ] || fail rustfs_caller_environment_changed

unset MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER RUSTFS_REGION RUSTFS_ENDPOINT
R2_ENDPOINT=http://ozone.local
R2_BUCKET=ozone-fixture
R2_ACCESS_KEY_ID=ozone-key
R2_SECRET_ACCESS_KEY=ozone-secret
export R2_ENDPOINT R2_BUCKET R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY
configure_foundationdb_napi_fixture ozone us-east-1
[ "$MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER" = rustfs ] || fail ozone_adapter_selection
[ "$RUSTFS_REGION" = us-east-1 ] || fail ozone_region_selection
[ -z "${RUSTFS_ENDPOINT:-}" ] || fail ozone_set_rustfs_endpoint
foundationdb_napi_run_client http://ozone.local node:24 true
grep -Fxq 'MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER=rustfs' "$capture" || fail ozone_forwarding
grep -Fxq 'RUSTFS_REGION=us-east-1' "$capture" || fail ozone_region_forwarding
grep -Fxq 'R2_BUCKET' "$capture" || fail ozone_bucket_argument
grep -Fxq 'R2_BUCKET=ozone-fixture' "$env_capture" || fail ozone_bucket_forwarding
grep -Fxq 'R2_ACCESS_KEY_ID' "$capture" || fail ozone_access_key_argument
grep -Fxq 'R2_SECRET_ACCESS_KEY' "$capture" || fail ozone_secret_argument
grep -Fxq 'R2_ACCESS_KEY_ID=ozone-key' "$env_capture" || fail ozone_access_key_environment
grep -Fxq 'R2_SECRET_ACCESS_KEY=ozone-secret' "$env_capture" || fail ozone_secret_environment
if grep -Fq 'ozone-key' "$capture" || grep -Fq 'ozone-secret' "$capture"; then fail ozone_credential_in_arguments; fi
[ "$R2_ACCESS_KEY_ID" = ozone-key ] && [ "$R2_SECRET_ACCESS_KEY" = ozone-secret ] || fail ozone_caller_environment_changed

if (MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER=unknown; export MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER; configure_foundationdb_napi_fixture generic) >/dev/null 2>&1; then
  fail invalid_selector_accepted
fi
if (MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER=r2; export MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER; configure_foundationdb_napi_fixture ozone us-east-1) >/dev/null 2>&1; then
  fail ozone_wrong_adapter_accepted
fi
if (RUSTFS_ENDPOINT=http://rustfs.local; MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER=r2; export RUSTFS_ENDPOINT MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER; configure_foundationdb_napi_fixture rustfs) >/dev/null 2>&1; then
  fail rustfs_wrong_adapter_accepted
fi
if (unset MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER RUSTFS_REGION; RUSTFS_ENDPOINT=http://rustfs.local; export RUSTFS_ENDPOINT; configure_foundationdb_napi_fixture rustfs) >/dev/null 2>&1; then
  fail missing_rustfs_region_accepted
fi
if (unset MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER RUSTFS_BUCKET; RUSTFS_ENDPOINT=http://rustfs.local; export RUSTFS_ENDPOINT; configure_foundationdb_napi_fixture rustfs) >/dev/null 2>&1; then
  fail incomplete_rustfs_fixture_accepted
fi
if (unset MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER R2_SECRET_ACCESS_KEY; configure_foundationdb_napi_fixture ozone us-east-1) >/dev/null 2>&1; then
  fail incomplete_ozone_fixture_accepted
fi

NAPI_FIXTURE_DOCKER_CALLS="$fake_docker_dir/calls"; export NAPI_FIXTURE_DOCKER_CALLS
preflight_status=0
if PATH="$fake_docker_dir:$PATH" MOUNT_RS_OZONE_FOUNDATIONDB_COMPOSITION=1 \
  MOUNT_RS_FOUNDATIONDB_NAPI=1 MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER=r2 \
  sh "$repo_dir/scripts/test-ozone.sh" > "$preflight_capture" 2>&1; then
  fail ozone_preflight_accepted_invalid_selector
else
  preflight_status=$?
fi
[ "$preflight_status" -eq 2 ] || fail ozone_preflight_exit_status
[ ! -e "$NAPI_FIXTURE_DOCKER_CALLS" ] || fail ozone_preflight_called_docker

[ "$failures" -eq 0 ] || exit 1
echo "FOUNDATIONDB_NAPI_FIXTURE_ENV_PASS cases=10"
