#!/bin/sh
# A disposable Redis Sentinel deployment for tests: one primary, two
# replicas and three sentinels (quorum 2), as six processes in one Docker
# container. Every process listens on its own port, published on the same
# port of 127.0.0.1, so the addresses the sentinels report are reachable
# from the host as well as from each other, on Linux and macOS alike.
#
#   redis-sentinel-fixture.sh start STATE_DIR      start it; write STATE_DIR/env
#   redis-sentinel-fixture.sh stop STATE_DIR       remove the container
#   redis-sentinel-fixture.sh primary STATE_DIR    print the primary's port
#   redis-sentinel-fixture.sh kill-primary STATE_DIR
#                                                  SIGKILL the primary; print its port
#   redis-sentinel-fixture.sh start-node STATE_DIR PORT
#                                                  start a killed server again
#   redis-sentinel-fixture.sh wait-failover STATE_DIR OLD_PORT
#                                                  wait for another primary; print its port
#   redis-sentinel-fixture.sh cli STATE_DIR PORT ARGUMENT...
#                                                  redis-cli against one server
#   redis-sentinel-fixture.sh run COMMAND...       start, run COMMAND with the
#                                                  env exported, stop
#
# STATE_DIR/env holds, one NAME=value per line:
#   MOUNT_RS_SENTINEL_CONTAINER   the container name
#   MOUNT_RS_SENTINEL_ADDRESSES   the sentinels, host:port separated by commas
#   MOUNT_RS_SENTINEL_SERVICE     the service name they monitor
#   MOUNT_RS_SENTINEL_DATA_PORTS  the Redis servers' ports, the first primary first
#
# Options, from the environment at `start`:
#   MOUNT_RS_REDIS_SENTINEL_IMAGE          the Redis image (default redis:8.4)
#   MOUNT_RS_SENTINEL_FIXTURE_PASSWORD     password of the Redis servers
#   MOUNT_RS_SENTINEL_FIXTURE_SENTINEL_PASSWORD
#                                          password of the sentinels
#   MOUNT_RS_SENTINEL_FIXTURE_TLS_DIR      directory with ca.pem, server.pem and
#                                          server.key: every listener uses TLS
#
# Docker missing or unable to start the container is an error, never a skip.
set -eu

SERVICE=mount-rs-test
IMAGE="${MOUNT_RS_REDIS_SENTINEL_IMAGE:-redis:8.4}"

die() {
  echo "redis-sentinel-fixture: $*" >&2
  exit 1
}

load() {
  state="$1"
  [ -f "$state/env" ] || die "no fixture state in $state"
  # shellcheck disable=SC1091
  . "$state/env"
  container="$MOUNT_RS_SENTINEL_CONTAINER"
  sentinel_ports="$(echo "$MOUNT_RS_SENTINEL_ADDRESSES" | tr ',' '\n' | sed 's/.*://' | tr '\n' ' ')"
  data_ports="$MOUNT_RS_SENTINEL_DATA_PORTS"
  password="${MOUNT_RS_SENTINEL_FIXTURE_PASSWORD_SAVED:-}"
  sentinel_password="${MOUNT_RS_SENTINEL_FIXTURE_SENTINEL_PASSWORD_SAVED:-}"
  tls="${MOUNT_RS_SENTINEL_FIXTURE_TLS_SAVED:-}"
}

# redis-cli inside the container. $1 is "data" or "sentinel", $2 the port.
cli() {
  cli_secret="$password"
  [ "$1" = sentinel ] && cli_secret="$sentinel_password"
  cli_port="$2"
  shift 2
  if [ -n "$tls" ]; then
    set -- redis-cli -h 127.0.0.1 -p "$cli_port" --tls --cacert /tls/ca.pem "$@"
  else
    set -- redis-cli -h 127.0.0.1 -p "$cli_port" "$@"
  fi
  if [ -n "$cli_secret" ]; then
    docker exec -e REDISCLI_AUTH="$cli_secret" "$container" "$@"
  else
    docker exec "$container" "$@"
  fi
}

# The primary's port according to the first sentinel that names one.
primary_port() {
  for port in $sentinel_ports; do
    named="$(cli sentinel "$port" SENTINEL GET-MASTER-ADDR-BY-NAME "$SERVICE" 2>/dev/null | sed -n 2p | tr -d '\r')"
    if [ -n "$named" ]; then
      echo "$named"
      return 0
    fi
  done
  return 1
}

# Write the configuration of the server on port $1; $2 is the primary's
# port, or empty for the primary itself.
data_config() {
  {
    if [ -n "$tls" ]; then
      echo "port 0"
      echo "tls-port $1"
      echo "tls-replication yes"
    else
      echo "port $1"
    fi
    tls_config
    echo "bind 0.0.0.0"
    echo "protected-mode no"
    echo "save \"\""
    echo "appendonly no"
    echo "dir /data/$1"
    echo "pidfile /data/$1/redis.pid"
    echo "logfile /data/$1/redis.log"
    echo "daemonize yes"
    echo "replica-announce-ip 127.0.0.1"
    echo "replica-announce-port $1"
    if [ -n "$password" ]; then
      echo "requirepass $password"
      echo "masterauth $password"
    fi
    [ -z "$2" ] || echo "replicaof 127.0.0.1 $2"
  }
}

tls_config() {
  [ -n "$tls" ] || return 0
  echo "tls-cert-file /tls/server.pem"
  echo "tls-key-file /tls/server.key"
  echo "tls-ca-cert-file /tls/ca.pem"
  echo "tls-auth-clients no"
}

sentinel_config() {
  {
    if [ -n "$tls" ]; then
      echo "port 0"
      echo "tls-port $1"
      echo "tls-replication yes"
    else
      echo "port $1"
    fi
    tls_config
    echo "bind 0.0.0.0"
    echo "protected-mode no"
    echo "dir /data/$1"
    echo "pidfile /data/$1/redis.pid"
    echo "logfile /data/$1/redis.log"
    echo "daemonize yes"
    echo "sentinel announce-ip 127.0.0.1"
    echo "sentinel announce-port $1"
    echo "sentinel monitor $SERVICE 127.0.0.1 $2 2"
    echo "sentinel down-after-milliseconds $SERVICE 2000"
    echo "sentinel failover-timeout $SERVICE 10000"
    echo "sentinel parallel-syncs $SERVICE 1"
    [ -z "$password" ] || echo "sentinel auth-pass $SERVICE $password"
    if [ -n "$sentinel_password" ]; then
      echo "requirepass $sentinel_password"
      echo "sentinel sentinel-pass $sentinel_password"
    fi
  }
}

# Put the configuration on stdin at /data/$1/$2 inside the container.
install_config() {
  docker exec -i "$container" sh -c "mkdir -p /data/$1 && cat > /data/$1/$2"
}

start() {
  state="$1"
  mkdir -p "$state"
  [ ! -f "$state/env" ] || die "$state already holds a fixture; stop it first"
  command -v docker >/dev/null 2>&1 || die "docker is required"
  command -v python3 >/dev/null 2>&1 || die "python3 is required"
  password="${MOUNT_RS_SENTINEL_FIXTURE_PASSWORD:-}"
  sentinel_password="${MOUNT_RS_SENTINEL_FIXTURE_SENTINEL_PASSWORD:-}"
  tls_dir="${MOUNT_RS_SENTINEL_FIXTURE_TLS_DIR:-}"
  tls=""
  [ -z "$tls_dir" ] || tls=1
  # Six ports nothing listens on, held open together so they are distinct.
  # Another process can take one between this check and Docker binding it,
  # so a container that cannot be started is tried again on other ports.
  attempt=0
  while :; do
    ports="$(python3 -c '
import socket
held = [socket.socket() for _ in range(6)]
for s in held:
    s.bind(("127.0.0.1", 0))
print(" ".join(str(s.getsockname()[1]) for s in held))
')"
    # shellcheck disable=SC2086
    set -- $ports
    data_ports="$1 $2 $3"
    sentinel_ports="$4 $5 $6"
    container="mount-rs-sentinel-$$-$(date +%s)-$1"
    publish=""
    for port in $ports; do
      publish="$publish -p 127.0.0.1:$port:$port"
    done
    # shellcheck disable=SC2086
    if failure="$(docker run -d --init --name "$container" \
      --label mount-rs-sentinel-fixture=1 $publish "$IMAGE" sleep 86400 2>&1 >/dev/null)"; then
      break
    fi
    # A container that failed to start still exists.
    docker rm -f -v "$container" >/dev/null 2>&1 || true
    attempt=$((attempt + 1))
    [ "$attempt" -lt 5 ] || die "cannot start $IMAGE: $failure"
  done
  {
    echo "MOUNT_RS_SENTINEL_CONTAINER=$container"
    echo "MOUNT_RS_SENTINEL_ADDRESSES=127.0.0.1:$4,127.0.0.1:$5,127.0.0.1:$6"
    echo "MOUNT_RS_SENTINEL_SERVICE=$SERVICE"
    echo "MOUNT_RS_SENTINEL_DATA_PORTS=\"$data_ports\""
    echo "MOUNT_RS_SENTINEL_FIXTURE_PASSWORD_SAVED='$password'"
    echo "MOUNT_RS_SENTINEL_FIXTURE_SENTINEL_PASSWORD_SAVED='$sentinel_password'"
    echo "MOUNT_RS_SENTINEL_FIXTURE_TLS_SAVED=$tls"
  } >"$state/env"
  if ! configure "$1" "$tls_dir"; then
    docker exec "$container" sh -c 'tail -n 40 /data/*/redis.log' >&2 || true
    stop "$state"
    die "the deployment did not come up"
  fi
}

# Start the six processes in the running container and wait for the
# sentinels to agree. $1 is the first primary's port, $2 the TLS directory.
configure() {
  first="$1"
  if [ -n "$tls" ]; then
    docker cp "$2/." "$container:/tls" >/dev/null || return 1
    docker exec "$container" chmod -R a+rX /tls || return 1
  fi
  for port in $data_ports; do
    if [ "$port" = "$first" ]; then
      data_config "$port" "" | install_config "$port" redis.conf
    else
      data_config "$port" "$first" | install_config "$port" redis.conf
    fi
    docker exec "$container" redis-server "/data/$port/redis.conf" >/dev/null || return 1
  done
  for port in $sentinel_ports; do
    sentinel_config "$port" "$first" | install_config "$port" sentinel.conf
    docker exec "$container" redis-server "/data/$port/sentinel.conf" --sentinel >/dev/null ||
      return 1
  done
  # Ready when every sentinel sees the primary, both replicas and the other
  # two sentinels, and agrees a failover could be authorised.
  attempts=0
  while :; do
    ready=0
    for port in $sentinel_ports; do
      info="$(cli sentinel "$port" SENTINEL MASTER "$SERVICE" 2>/dev/null | tr -d '\r' || true)"
      replicas="$(echo "$info" | sed -n '/^num-slaves$/{n;p;}')"
      others="$(echo "$info" | sed -n '/^num-other-sentinels$/{n;p;}')"
      quorum="$(cli sentinel "$port" SENTINEL CKQUORUM "$SERVICE" 2>/dev/null || true)"
      case "$quorum" in OK*) ;; *) continue ;; esac
      [ "$replicas" = 2 ] && [ "$others" = 2 ] && ready=$((ready + 1))
    done
    [ "$ready" = 3 ] && break
    attempts=$((attempts + 1))
    if [ "$attempts" -gt 300 ]; then
      echo "redis-sentinel-fixture: the sentinels did not agree within 60 s" >&2
      return 1
    fi
    sleep 0.2
  done
}

stop() {
  state="$1"
  [ -f "$state/env" ] || return 0
  load "$state"
  docker rm -f -v "$container" >/dev/null 2>&1 || true
  rm -f "$state/env"
}

command="${1:-}"
[ -n "$command" ] || die "usage: $0 start|stop|primary|kill-primary|start-node|wait-failover|cli|run ..."
shift
case "$command" in
  start)
    start "${1:?STATE_DIR}"
    ;;
  stop)
    stop "${1:?STATE_DIR}"
    ;;
  primary)
    load "${1:?STATE_DIR}"
    primary_port || die "no sentinel names a primary"
    ;;
  kill-primary)
    load "${1:?STATE_DIR}"
    port="$(primary_port)" || die "no sentinel names a primary"
    docker exec "$container" sh -c "kill -9 \$(cat /data/$port/redis.pid)"
    echo "$port"
    ;;
  start-node)
    load "${1:?STATE_DIR}"
    port="${2:?PORT}"
    docker exec "$container" redis-server "/data/$port/redis.conf" >/dev/null
    ;;
  wait-failover)
    load "${1:?STATE_DIR}"
    old="${2:?OLD_PORT}"
    attempts=0
    while :; do
      port="$(primary_port || true)"
      if [ -n "$port" ] && [ "$port" != "$old" ]; then
        role="$(cli data "$port" ROLE 2>/dev/null | sed -n 1p | tr -d '\r' || true)"
        if [ "$role" = master ]; then
          echo "$port"
          break
        fi
      fi
      attempts=$((attempts + 1))
      [ "$attempts" -le 300 ] || die "no new primary within 60 s of losing $old"
      sleep 0.2
    done
    ;;
  cli)
    load "${1:?STATE_DIR}"
    port="${2:?PORT}"
    shift 2
    kind=data
    case " $sentinel_ports " in *" $port "*) kind=sentinel ;; esac
    cli "$kind" "$port" "$@"
    ;;
  run)
    [ "$#" -gt 0 ] || die "run needs a command"
    state="$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-sentinel.XXXXXX")"
    trap 'stop "$state"; rm -rf "$state"' EXIT
    trap 'exit 130' INT TERM
    start "$state"
    set -a
    # shellcheck disable=SC1091
    . "$state/env"
    set +a
    MOUNT_RS_SENTINEL_FIXTURE="$state" "$@"
    ;;
  *)
    die "unknown command $command"
    ;;
esac
