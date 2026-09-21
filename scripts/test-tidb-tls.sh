#!/bin/sh

# Run the ignored TiDB provider contract against a TLS-required endpoint.
# Credentials stay in the caller's environment and are never printed.

set -eu

tls_url=${MOUNT_RS_TIDB_TLS_URL:-}
if [ -z "$tls_url" ]; then
  echo "test-tidb-tls.sh: MOUNT_RS_TIDB_TLS_URL must be set" >&2
  exit 2
fi

case "$tls_url" in
  mysql://*)
    ;;
  *)
    echo "test-tidb-tls.sh: TLS endpoint must use a mysql:// URL" >&2
    exit 2
    ;;
esac

case "$tls_url" in
  *\?*)
    query=${tls_url#*\?}
    ;;
  *)
    echo "test-tidb-tls.sh: TLS endpoint URL must include query parameters" >&2
    exit 2
    ;;
esac

# mysql_async defaults CA and hostname verification on when TLS is required.
# Require the explicit SSL switch and reject URL options that disable either
# verification or the built-in trust roots. Custom CA material must be wired
# through the deployment/runtime rather than weakening this gate in the URL.
query_tokens="&${query}&"
case "$query_tokens" in
  *"&require_ssl=true&"*)
    ;;
  *)
    echo "test-tidb-tls.sh: URL must contain require_ssl=true" >&2
    exit 2
    ;;
esac

for unsafe_option in verify_ca=false verify_identity=false built_in_roots=false; do
  case "$query_tokens" in
    *"&${unsafe_option}&"*)
      echo "test-tidb-tls.sh: URL must not contain ${unsafe_option}" >&2
      exit 2
      ;;
  esac
done

echo "TIDB_TLS_CONFIG_PASS require_ssl=true verify_ca=true verify_identity=true"

if [ "${MOUNT_RS_TIDB_TLS_VALIDATE_ONLY:-0}" = "1" ]; then
  echo "TIDB_TLS_CONFIG_ONLY_PASS"
  exit 0
fi

export MOUNT_RS_TIDB_URL="$tls_url"
if [ -n "${MOUNT_RS_TIDB_TLS_TEST_VOLUME_KEY:-}" ]; then
  export MOUNT_RS_TIDB_TEST_VOLUME_KEY="$MOUNT_RS_TIDB_TLS_TEST_VOLUME_KEY"
fi

./scripts/cargo-shared test --locked \
  -p mount-rs-tidb \
  --features rustls \
  --test tidb \
  -- --ignored --nocapture

echo "TIDB_TLS_ACCEPTANCE_PASS provider_contract=tidb tls=rustls"
