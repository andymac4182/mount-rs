#!/bin/sh
set -eu

# Own only this benchmark's disposable discovery service. Metadata/block
# services are selected by AUDIT_METADATA/AUDIT_BLOCKS in their own harnesses.
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
# Preserve host filesystem headroom alongside benchmark stderr. A successful
# workload alone does not establish that storage pressure was absent.
df -Pk "$repo_dir" >&2
registry_container="mount-rs-cluster-load-$$"
registry_id=
cleanup_registry() {
  if [ -n "$registry_id" ]; then
    docker rm -f "$registry_id" >/dev/null 2>&1 || true
  fi
  df -Pk "$repo_dir" >&2 || true
}
trap cleanup_registry EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
registry_id=$(docker run -d --name "$registry_container" -p 127.0.0.1::6379 \
  redis:8.4.0-alpine redis-server --save '' --appendonly no)
registry_binding=$(docker port "$registry_id" 6379/tcp)
export MOUNT_RS_REDIS_TEST_URL="redis://$registry_binding/"
./scripts/cargo-shared run -p mount-rs-cluster --features http3 \
  --example cluster_load --release --locked
