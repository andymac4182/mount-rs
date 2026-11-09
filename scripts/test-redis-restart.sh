#!/bin/sh
# Isolated, nonpersistent Redis restart. Does not restart any existing service.
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
./scripts/cargo-shared test -p mount-rs-cluster --features http3 --lib --locked --no-run
run_dir=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-redis-restart.XXXXXX")
cid=
test_pid=
watchdog=
cleanup() {
    if [ -n "$watchdog" ]; then kill "$watchdog" 2>/dev/null || true; fi
    if [ -n "$test_pid" ]; then
        kill "$test_pid" 2>/dev/null || true
        wait "$test_pid" 2>/dev/null || true
    fi
    if [ -n "$cid" ]; then docker rm -f "$cid" >/dev/null 2>&1 || true; fi
    rm -rf "$run_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM
# Docker reallocates an automatically published host port on stop/start. Use
# an explicit ephemeral port so the restart preserves the service endpoint.
host_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
cid="$(basename "$run_dir")"
docker run -d --name "$cid" --network bridge -p "127.0.0.1:$host_port:6379" redis:8.4.0-alpine redis-server --save '' --appendonly no >/dev/null
binding=$(docker port "$cid" 6379/tcp)
export MOUNT_RS_REDIS_TEST_URL="redis://$binding/"
export MOUNT_RS_REDIS_RESTART_MARKERS="$run_dir"
attempt=0
until docker exec "$cid" redis-cli ping >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 100 ] || { echo 'Redis startup timed out' >&2; exit 1; }
    sleep 0.1
done
./scripts/cargo-shared test -p mount-rs-cluster --features http3 --lib --locked registration_recovers_after_server_restart -- --ignored --nocapture &
test_pid=$!
(sleep 60; kill "$test_pid" 2>/dev/null || true) &
watchdog=$!
attempt=0
until [ -f "$run_dir/ready" ]; do
    kill -0 "$test_pid" 2>/dev/null || { echo 'Test exited before readiness' >&2; exit 1; }
    attempt=$((attempt + 1))
    [ "$attempt" -lt 200 ] || { echo 'Registration readiness timed out' >&2; exit 1; }
    sleep 0.1
done
docker exec "$cid" redis-cli SET mount-rs-restart-sentinel original >/dev/null
docker stop --time 1 "$cid" >/dev/null
sleep 1
docker start "$cid" >/dev/null
[ "$(docker port "$cid" 6379/tcp)" = "$binding" ] || { echo 'Redis endpoint changed' >&2; exit 1; }
attempt=0
until docker exec "$cid" redis-cli ping >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 100 ] || { echo 'Redis restart readiness timed out' >&2; exit 1; }
    sleep 0.1
done
[ -z "$(docker exec "$cid" redis-cli --raw GET mount-rs-restart-sentinel)" ] || { echo 'Redis unexpectedly retained volatile state' >&2; exit 1; }
echo 'REDIS_RESTART_FIXTURE endpoint_preserved=true volatile_state_lost=true'
touch "$run_dir/restarted"
wait "$test_pid"
test_pid=
