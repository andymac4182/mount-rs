#!/bin/sh
set -eu

# A real TLS handshake between the provider and TiDB, with no credentials and
# no network beyond loopback.
#
# The lane creates a throwaway certificate authority, starts a single-node
# TiDB/PD/TiKV whose server certificate is issued by that authority, and then:
#
#   1. runs scripts/test-tidb-tls.sh for real, so the provider contract runs
#      over a verified TLS session;
#   2. checks that the session is encrypted and that the provider refuses a
#      server it cannot verify: an untrusted authority, the public roots
#      alone, and a certificate issued for another host name. The same test
#      then turns on TiDB's require_secure_transport and checks that a
#      plaintext session is refused.
#
# Replication is not the subject here, so one PD and one TiKV are enough;
# scripts/test-tidb.sh covers the durable topology.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_dir/scripts/cargo-shared-env.sh"
cd "$repo_dir"

for command_name in docker curl openssl; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "test-tidb-tls-local.sh: $command_name is required" >&2
    exit 2
  fi
done
docker_arch=""
if ! docker_arch=$(docker info --format '{{.Architecture}}' 2>/dev/null); then
  echo "test-tidb-tls-local.sh: Docker daemon is not reachable; the TLS handshake lane cannot run" >&2
  exit 2
fi

startup_timeout_seconds=${MOUNT_RS_TIDB_STARTUP_TIMEOUT_SECONDS:-300}
case "$startup_timeout_seconds" in
  ''|*[!0-9]*)
    echo "test-tidb-tls-local.sh: MOUNT_RS_TIDB_STARTUP_TIMEOUT_SECONDS must be a non-negative integer" >&2
    exit 2
    ;;
esac

image_version=${MOUNT_RS_TIDB_VERSION:-v8.5.7}
case "$image_version" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "test-tidb-tls-local.sh: MOUNT_RS_TIDB_VERSION must be a pinned vX.Y.Z tag" >&2
    exit 2
    ;;
esac

docker_platform=${MOUNT_RS_TIDB_DOCKER_PLATFORM:-}
if [ -z "$docker_platform" ]; then
  case "$docker_arch" in
    aarch64|arm64) docker_platform=linux/arm64 ;;
    amd64|x86_64) docker_platform=linux/amd64 ;;
    *)
      echo "test-tidb-tls-local.sh: unsupported Docker architecture: $docker_arch" >&2
      exit 2
      ;;
  esac
fi

tikv_config="$repo_dir/tests/tidb/tikv-test.toml"
if [ ! -r "$tikv_config" ]; then
  echo "test-tidb-tls-local.sh: missing readable TiKV test config: $tikv_config" >&2
  exit 2
fi

temp_root=${TMPDIR:-/tmp}
if [ "$temp_root" != "/" ]; then
  temp_root=${temp_root%/}
fi
run_dir=$(mktemp -d "$temp_root/mount-rs-tidb-tls.XXXXXX")
case "$run_dir" in
  *[!A-Za-z0-9/._-]*)
    # The authority path travels in a connection URL query parameter.
    rmdir "$run_dir"
    echo "test-tidb-tls-local.sh: the temporary directory path must be URL-safe: set TMPDIR to a plain path" >&2
    exit 2
    ;;
esac
run_id=$(basename "$run_dir" | tr '.' '-')
network_name="mount-rs-tidb-tls-net-$run_id"
resource_label="mount-rs.tidb-tls.run=$run_id"
pd_container="mount-rs-tidb-tls-$run_id-pd"
tikv_container="mount-rs-tidb-tls-$run_id-tikv"
tidb_container="mount-rs-tidb-tls-$run_id-tidb"
pd_image="pingcap/pd:$image_version"
tikv_image="pingcap/tikv:$image_version"
tidb_image="pingcap/tidb:$image_version"
created_network=0
created_containers=""
phase_name="initialization"

owned() {
  [ "$(docker inspect --format '{{index .Config.Labels "mount-rs.tidb-tls.run"}}' "$1" 2>/dev/null || true)" = "$run_id" ]
}

cleanup() {
  exit_status=$?
  trap - EXIT INT TERM
  set +e
  if [ "$exit_status" -ne 0 ]; then
    echo "--- TiDB TLS lane diagnostics: phase=$phase_name ---" >&2
    for container in $created_containers; do
      echo "--- Docker logs: $container ---" >&2
      docker logs --tail 40 "$container" 2>&1 | grep -v 'loaded config' | sed -E 's#(mysql://[^:/@]+:)[^/@]+@#\1[redacted]@#g' >&2
    done
  fi
  cleanup_status=0
  for container in $created_containers; do
    if docker inspect "$container" >/dev/null 2>&1; then
      if owned "$container"; then
        docker rm --force --volumes "$container" >/dev/null 2>&1
      else
        echo "test-tidb-tls-local.sh: refusing to remove a container without this run's label: $container" >&2
        cleanup_status=1
      fi
    fi
    if docker inspect "$container" >/dev/null 2>&1; then
      echo "test-tidb-tls-local.sh: container remained after cleanup: $container" >&2
      cleanup_status=1
    fi
  done
  if [ "$created_network" -eq 1 ]; then
    network_owner=$(docker network inspect --format '{{index .Labels "mount-rs.tidb-tls.run"}}' "$network_name" 2>/dev/null)
    if [ "$network_owner" = "$run_id" ]; then
      docker network rm "$network_name" >/dev/null 2>&1
    fi
    if docker network inspect "$network_name" >/dev/null 2>&1; then
      echo "test-tidb-tls-local.sh: network remained after cleanup: $network_name" >&2
      cleanup_status=1
    fi
  fi
  case "$run_dir" in
    "$temp_root"/mount-rs-tidb-tls.??????)
      if [ -d "$run_dir" ] && [ ! -L "$run_dir" ]; then
        rm -rf -- "$run_dir"
      fi
      ;;
    *)
      echo "test-tidb-tls-local.sh: refusing to remove an unvalidated run directory: $run_dir" >&2
      cleanup_status=1
      ;;
  esac
  if [ "$cleanup_status" -ne 0 ] && [ "$exit_status" -eq 0 ]; then
    exit_status=1
  fi
  exit "$exit_status"
}

trap cleanup EXIT
trap 'exit 130' INT TERM

phase_name="certificate generation"
tls_dir="$run_dir/tls"
mkdir "$tls_dir"

# A certificate authority that exists only for this run. Its key never leaves
# the run directory and is deleted with it.
new_authority() {
  authority_name=$1
  cat > "$tls_dir/$authority_name.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = v3_ca
prompt = no
[dn]
CN = mount-rs throwaway $authority_name
[v3_ca]
basicConstraints = critical, CA:TRUE
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
EOF
  openssl req -x509 -newkey rsa:2048 -nodes -days 2 \
    -config "$tls_dir/$authority_name.cnf" \
    -keyout "$tls_dir/$authority_name-key.pem" \
    -out "$tls_dir/$authority_name.pem" >/dev/null 2>&1
}

new_authority ca
# A second, unrelated authority: a client that trusts only this one must
# refuse the server.
new_authority untrusted-ca

# The server certificate names `localhost` and nothing else, so a client that
# reaches the same server as 127.0.0.1 must refuse it.
cat > "$tls_dir/server.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
[dn]
CN = localhost
[v3_server]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost
EOF
openssl req -newkey rsa:2048 -nodes \
  -config "$tls_dir/server.cnf" \
  -keyout "$tls_dir/server-key.pem" \
  -out "$tls_dir/server.csr" >/dev/null 2>&1
openssl x509 -req -days 2 \
  -in "$tls_dir/server.csr" \
  -CA "$tls_dir/ca.pem" -CAkey "$tls_dir/ca-key.pem" -CAcreateserial \
  -extfile "$tls_dir/server.cnf" -extensions v3_server \
  -out "$tls_dir/server.pem" >/dev/null 2>&1
openssl verify -CAfile "$tls_dir/ca.pem" "$tls_dir/server.pem" >/dev/null
# The container's TiDB process must be able to read the mounted files.
chmod 0755 "$run_dir" "$tls_dir"
chmod 0644 "$tls_dir/ca.pem" "$tls_dir/server.pem" "$tls_dir/server-key.pem"

cat > "$run_dir/tidb-tls.toml" <<EOF
[performance]
force-init-stats = false

[security]
ssl-ca = "/tls/ca.pem"
ssl-cert = "/tls/server.pem"
ssl-key = "/tls/server-key.pem"
EOF

echo "Starting a TLS-enabled TiDB version=$image_version platform=$docker_platform" >&2
phase_name="image pull"
for image in "$pd_image" "$tikv_image" "$tidb_image"; do
  docker pull --platform "$docker_platform" "$image" >/dev/null
done

phase_name="network allocation"
docker network create --driver bridge --label "$resource_label" "$network_name" >/dev/null
created_network=1

deadline=$(($(date +%s) + startup_timeout_seconds))
wait_until() {
  wait_label=$1
  shift
  while [ "$(date +%s)" -lt "$deadline" ]; do
    for container in $created_containers; do
      if [ "$(docker inspect --format '{{.State.Status}}' "$container" 2>/dev/null || true)" != running ]; then
        echo "test-tidb-tls-local.sh: $container stopped while waiting for $wait_label" >&2
        return 1
      fi
    done
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "test-tidb-tls-local.sh: timed out after ${startup_timeout_seconds}s waiting for $wait_label" >&2
  return 1
}

phase_name="PD startup"
docker run --detach \
  --platform "$docker_platform" \
  --name "$pd_container" \
  --label "$resource_label" \
  --network "$network_name" \
  --network-alias pd1 \
  "$pd_image" \
  --name=pd1 \
  --client-urls=http://0.0.0.0:2379 \
  --peer-urls=http://0.0.0.0:2380 \
  --advertise-client-urls=http://pd1:2379 \
  --advertise-peer-urls=http://pd1:2380 \
  --initial-cluster=pd1=http://pd1:2380 \
  --log-level=warn \
  --data-dir=/data \
  >/dev/null
created_containers="$created_containers $pd_container"
wait_until "PD" docker exec "$pd_container" curl -fsS --max-time 2 http://127.0.0.1:2379/pd/api/v1/health

phase_name="TiKV startup"
docker run --detach \
  --platform "$docker_platform" \
  --ulimit nofile=200000:200000 \
  --name "$tikv_container" \
  --label "$resource_label" \
  --network "$network_name" \
  --network-alias tikv1 \
  --volume "$tikv_config:/etc/tikv-test.toml:ro" \
  "$tikv_image" \
  --config=/etc/tikv-test.toml \
  --pd-endpoints=pd1:2379 \
  --addr=0.0.0.0:20160 \
  --advertise-addr=tikv1:20160 \
  --status-addr=0.0.0.0:20180 \
  --data-dir=/data \
  >/dev/null
created_containers="$created_containers $tikv_container"
wait_until "TiKV" docker exec "$tikv_container" curl -fsS --max-time 2 http://127.0.0.1:20180/status

phase_name="TiDB startup"
docker run --detach \
  --platform "$docker_platform" \
  --name "$tidb_container" \
  --label "$resource_label" \
  --network "$network_name" \
  --network-alias tidb \
  --publish 127.0.0.1::4000/tcp \
  --publish 127.0.0.1::10080/tcp \
  --volume "$run_dir/tidb-tls.toml:/etc/tidb-tls.toml:ro" \
  --volume "$tls_dir:/tls:ro" \
  "$tidb_image" \
  --config=/etc/tidb-tls.toml \
  --store=tikv \
  --path=pd1:2379 \
  --host=0.0.0.0 \
  -P=4000 \
  -L=warn \
  --status=10080 \
  >/dev/null
created_containers="$created_containers $tidb_container"
sql_port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "4000/tcp") 0).HostPort}}' "$tidb_container")
status_port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "10080/tcp") 0).HostPort}}' "$tidb_container")
wait_until "TiDB" curl -fsS --connect-timeout 1 --max-time 5 "http://127.0.0.1:$status_port/status"

# The verification switches are spelled out so the guard in
# scripts/test-tidb-tls.sh sees them. `tls_ca_file` adds this run's authority
# to the trusted roots; it does not relax verification.
verified="require_ssl=true&verify_ca=true&verify_identity=true"
tls_url="mysql://root@localhost:$sql_port/test?$verified&tls_ca_file=$tls_dir/ca.pem"

phase_name="provider contract over TLS"
echo "TiDB TLS phase=$phase_name" >&2
MOUNT_RS_TIDB_TLS_URL="$tls_url" \
MOUNT_RS_TIDB_TLS_TEST_VOLUME_KEY="mount-rs-tidb-tls-$run_id" \
  "$repo_dir/scripts/test-tidb-tls.sh"

phase_name="handshake verification"
echo "TiDB TLS phase=$phase_name" >&2
MOUNT_RS_TIDB_TLS_URL="$tls_url" \
MOUNT_RS_TIDB_TLS_UNTRUSTED_URL="mysql://root@localhost:$sql_port/test?$verified&tls_ca_file=$tls_dir/untrusted-ca.pem" \
MOUNT_RS_TIDB_TLS_PUBLIC_ROOTS_URL="mysql://root@localhost:$sql_port/test?$verified" \
MOUNT_RS_TIDB_TLS_MISNAMED_URL="mysql://root@127.0.0.1:$sql_port/test?$verified&tls_ca_file=$tls_dir/ca.pem" \
MOUNT_RS_TIDB_TLS_PLAINTEXT_URL="mysql://root@localhost:$sql_port/test" \
  "$repo_dir/scripts/cargo-shared" test --locked -p mount-rs-tidb --features rustls --lib \
    actual_tidb_tls_handshake_is_verified \
    -- --ignored --nocapture --test-threads=1

echo "TIDB_TLS_HANDSHAKE_ACCEPTANCE version=$image_version platform=$docker_platform verified=pass untrusted_ca=rejected wrong_host=rejected plaintext=rejected" >&2
