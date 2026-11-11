# serve-http operator runbook

How to run `mount-rs serve-http` in production. Scope: the v1 launch
configuration, which is plain `serve-http` with `remote_sessions: false` and no
`forwarding` block.

- API contract for consumers: [`transports/mount-rs-http/README.md`](../transports/mount-rs-http/README.md)
  and [`transports/mount-rs-http/openapi.yaml`](../transports/mount-rs-http/openapi.yaml).
- Config file basics and a local demo: [`apps/mount-rs-cli/README.md`](../apps/mount-rs-cli/README.md#unmounted-http-service).

## What the process is

One process serves one or more named drives over cleartext HTTP/1.1 on a
loopback address. Each drive has one static bearer token. The process holds
the exclusive writer lease of every split-store drive it serves, so **exactly
one instance may serve a given drive**. There is no active-active mode at
launch; a second instance pointed at the same storage fails to start.

The process does not terminate TLS, does not listen on a non-loopback address,
and does not write an access log. A reverse proxy in front of it provides all
three.

## Start

```sh
MOUNT_RS_FILES_TOKEN=... mount-rs serve-http --config /etc/mount-rs/http.json
```

`serve-http` accepts only `--config <path>`. Check a config without starting
anything with `mount-rs validate-config --config <path>`. It prints
`valid config: <path>` and exits 0, or prints the offending key and exits 2.
It does not open storage and does not read the token variables.

Startup order:

1. Parse and validate the config. Any error exits with code 2.
2. Read each drive's token from its environment variable.
3. Install the SIGINT/SIGTERM handler.
4. Open each drive in config order. A split-store drive acquires its writer
   lease here. If any drive fails to open, the drives already opened are shut
   down again and the process exits with code 1.
5. Bind the listener.
6. Print the readiness line.

### Readiness line

On success the process prints two lines to stdout and flushes:

```text
http listening at http://127.0.0.1:8080 (drives: files, scratch)
Press Ctrl-C to stop.
```

The first line is printed only after every drive is open and the socket is
bound. With `port: 0` it is the only place the chosen port appears, so set a
fixed port in production. Use `/readyz` for readiness checks rather than
parsing stdout.

## Configuration reference

The config is a JSON file with `"version": 1` and an `http` object. Unknown
keys are rejected. Relative paths resolve against the config file's directory.

### `http` keys

| Key | Type | Default | Valid values | Notes |
| --- | --- | --- | --- | --- |
| `host` | string | `127.0.0.1` | `127.0.0.1`, `::1`, `[::1]`, `localhost`, or any loopback IP | Non-loopback hosts are rejected at config validation and again at bind. |
| `port` | integer | `0` | 0-65535 | `0` picks an ephemeral port. Set a fixed port in production. |
| `max_request_bytes` | integer | `8388608` (8 MiB) | >= 1 | Largest `PUT` body and JSON body. Larger requests get 413. This is also the largest file a client can upload. |
| `max_buffered_upload_bytes` | integer | 4 x `max_request_bytes` | >= `max_request_bytes` (a smaller value is a config error, exit 2) | Upload bytes the server buffers across all in-flight `PUT`s. A `PUT` reserves its `Content-Length` (or `max_request_bytes` without one) before its body is read; uploads beyond the budget wait in arrival order. |
| `max_response_bytes` | integer | `8388608` (8 MiB) | >= 1 | Largest directory listing (uncompressed JSON). Larger listings get 413. Does not limit file downloads. |
| `max_directory_entries` | integer | `4096` | >= 1 | Largest directory that can be listed. Larger directories get 413. |
| `read_chunk_bytes` | integer | `65536` | >= 1 | Read size per chunk when streaming a download. |
| `request_timeout_ms` | integer | `30000` | >= 1 | Limit for one request up to its response headers, including receiving the body and waiting for upload budget; exceeding it returns 504. Also the header-read and idle keep-alive timeout (the connection is closed without a response) and the time a download may make no progress before it is cut. |
| `drain_timeout_ms` | integer | `35000` | >= 1 | How long shutdown waits for in-flight requests before aborting them. Keep it above `request_timeout_ms`. |
| `max_connections` | integer | `256` | >= 1 | Concurrent TCP connections. Excess connections are accepted and closed immediately with no HTTP response. |
| `tcp_nodelay` | boolean | `true` | | Disables Nagle on accepted connections. |
| `http2_prior_knowledge` | boolean | `false` | | `true` makes the listener cleartext HTTP/2 only (no HTTP/1.1). Leave `false` at launch; the reference proxy configs below speak HTTP/1.1 upstream. With HTTP/2 a stalled download releases its file handle but its connection is not dropped. |
| `drives` | array | required | at least one drive | See below. |
| `cache_capacity` | object | none | `{"ram_bytes": n, "disk_bytes": n}`, both required, >= 0 | Validation only: startup is refused when the sum of the drives' configured block-cache and disk-cache sizes exceeds these numbers. |
| `disk_cache_budget` | object | none | `{"workers": 1-512, "fill_copies": 1-512}`, both required | Shared disk-cache worker budget for split-store drives that configure a disk cache. |
| `tidb_metadata_pool_limit` | integer | none | >= 1 | Shares one TiDB connection pool of this size between drives that use the same TiDB connection variable. Only for TiDB metadata. |
| `cache_report_interval_seconds` | integer | none | >= 1 | Writes a `mount_cache_stats` JSON line per drive to stderr at this interval. Only drives with a block or disk cache report. |
| `remote_sessions` | boolean | `false` | | Experimental. Must stay `false` for the launch contract. |
| `forwarding` | object | none | | Experimental. Must be absent for the launch contract. |

### `http.drives[]` keys

| Key | Type | Required | Valid values |
| --- | --- | --- | --- |
| `id` | string | yes | 1-64 characters from `A-Z a-z 0-9 - _ .`; not `.` or `..`; unique in the file. It appears in URLs. |
| `token` | object | yes | Exactly `{"env": "NAME"}`. The token itself cannot be written in the file. |
| `driver` | object | yes | A `memory`, `host`, `sqlite` or `splitstore` driver object, in the same shape as native mount configs. `remote` is rejected. |

`memory` drives lose all data on restart. Do not serve one in production.

Durability of an acknowledged `PUT` depends on the drive. Split-store drives on
SQLite acknowledge after the data is durable, unless the storage object sets
`writeback`, in which case a `200` is returned before the data is durable. Do
not enable `writeback` on a drive whose consumers rely on the documented
"durable before 200".

The checked-in `apps/mount-rs-cli/examples/config-http.json` is a demo: it has
`port: 0` and a `memory` drive. It sets every limit to its default value
explicitly (`max_request_bytes` 8388608, `max_buffered_upload_bytes` 33554432,
`drain_timeout_ms` 35000, `request_timeout_ms` 30000).

### Example

```json
{
  "version": 1,
  "http": {
    "host": "127.0.0.1",
    "port": 8080,
    "max_request_bytes": 8388608,
    "request_timeout_ms": 30000,
    "drain_timeout_ms": 35000,
    "max_connections": 256,
    "drives": [
      {
        "id": "files",
        "token": {"env": "MOUNT_RS_FILES_TOKEN"},
        "driver": {
          "kind": "splitstore",
          "storage": {
            "metadata": {"kind": "sqlite", "path": "/var/lib/mount-rs/files-metadata.db"},
            "blocks": {"kind": "sqlite", "path": "/var/lib/mount-rs/files-blocks.db"},
            "owner": "files-http-1"
          }
        }
      }
    ]
  }
}
```

## Tokens

### Provisioning

- One token per drive, supplied in the environment variable named by
  `drives[].token.env`. Generate at least 32 random bytes, for example
  `openssl rand -base64 32 | tr '+/' '-_' | tr -d '='`.
- A token must be non-empty and must not contain whitespace. An invalid token
  exits with code 2; a missing variable exits with code 1. Neither message
  contains the token.
- The token is read once, at startup. It is never printed, never written to
  the config, and never returned by the API.
- Two drives may be given the same token. `GET /v1/drives` then lists both for
  that token. Use distinct tokens unless one consumer owns both drives.
- A token grants full read and write access to its drive. There are no
  read-only tokens.
- Deliver the variables through the service manager's secret mechanism (for
  systemd, `EnvironmentFile=` with mode 0600, or `LoadCredential=`). Do not put
  tokens on the command line.

### Rotation

**Rotation requires a restart.** There is no reload signal and a drive accepts
only one token at a time.

1. Give the new token to the consumers, who must be able to switch on a 401.
2. Update the environment source.
3. Restart the service (SIGTERM, wait for exit, start).
4. From the moment the new process is ready, the old token returns 401.

During the restart the API is unavailable (the proxy returns 502), and
there is no window in which both tokens work. Schedule rotations accordingly.
The reverse proxy must not cache or log the `Authorization` header.

## Reverse proxy

The listener is cleartext and loopback-only. A proxy is mandatory for any
client that is not on the same host.

Requirements:

| Requirement | Why |
| --- | --- |
| Terminate TLS at the proxy | The server has no TLS. |
| Run in the same network namespace as the server (same host, or same pod as a sidecar) | The server binds loopback only. A proxy on another host or in another container network cannot reach it. |
| Forward the request path unmodified | The server validates percent-encoding itself. Do not let the proxy decode, normalise or merge slashes in the upstream URI. |
| Forward `Authorization`, `Range`, `Accept-Encoding` unchanged; do not cache | Authentication is per request. Responses depend on the token. |
| Request body cap >= `max_request_bytes` | A smaller cap rejects uploads the server would accept. Equal is best: then the proxy rejects oversize uploads without opening an upstream connection. |
| Buffer request bodies before connecting upstream | The server's request timeout covers receiving the body. Buffering keeps slow clients from consuming it and from holding upload memory budget. |
| Do not buffer or cap response bodies | Downloads have no size limit and are streamed. |
| Upstream read/send timeout slightly above `request_timeout_ms` | The server answers 504 itself at `request_timeout_ms`; the proxy should not give up first. |
| Upstream keep-alive idle timeout shorter than `request_timeout_ms` | The server closes a keep-alive connection that has been idle for `request_timeout_ms`. A proxy that reuses such a connection at that moment gets a reset. |
| Upstream connections < `max_connections` | Connections beyond the limit are closed without a response, which the proxy reports as 502. |
| Do not retry non-idempotent requests upstream | A retried `rename` or non-recursive `mkdir` can fail spuriously. Safest is no automatic retry at all; consumers have retry rules. |
| Write an access log without the `Authorization` header | The server has no access log. |
| Keep `/readyz` and `/healthz` reachable by the load balancer only | They are unauthenticated. |

Errors generated by the proxy do not have the API's JSON error body. Consumers
are told to expect that.

### nginx

```nginx
upstream mount_rs_http {
    server 127.0.0.1:8080;
    keepalive 64;                 # idle upstream connections kept, well below max_connections
    keepalive_timeout 20s;        # shorter than request_timeout_ms (30 s)
}

server {
    listen 443 ssl;
    http2 on;
    server_name files.example.com;
    ssl_certificate     /etc/ssl/files.example.com.fullchain.pem;
    ssl_certificate_key /etc/ssl/files.example.com.key;

    client_max_body_size 8m;      # equals max_request_bytes (8388608)
    client_body_timeout 30s;

    location / {
        proxy_pass http://mount_rs_http;   # no URI part: the request URI is forwarded unmodified
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_set_header Host $host;

        proxy_request_buffering on;        # receive the upload before connecting upstream
        proxy_buffering off;               # stream downloads
        proxy_max_temp_file_size 0;

        proxy_connect_timeout 2s;
        proxy_send_timeout 35s;            # above request_timeout_ms
        proxy_read_timeout 35s;

        proxy_next_upstream off;           # never replay a request
    }

    location = /healthz { allow 10.0.0.0/8; deny all; proxy_pass http://mount_rs_http; }
    location = /readyz  { allow 10.0.0.0/8; deny all; proxy_pass http://mount_rs_http; }
}
```

### Caddy

```caddyfile
files.example.com {
    request_body {
        max_size 8MiB
    }

    @external_probe {
        path /healthz /readyz
        not remote_ip 10.0.0.0/8
    }
    respond @external_probe 403

    reverse_proxy 127.0.0.1:8080 {
        flush_interval -1
        lb_try_duration 0s
        transport http {
            versions 1.1
            dial_timeout 2s
            response_header_timeout 35s
            keepalive 20s
            keepalive_idle_conns 64
        }
    }
}
```

Test either config against a staging instance before production. In particular
check that `GET /v1/drives/<id>/fs/a%2Fb` reaches the server unchanged and
returns the server's `400`, and that an 8 MiB upload succeeds.

## Health checks

| Probe | Use for | 200 | 503 |
| --- | --- | --- | --- |
| `GET /healthz` | Liveness (restart decisions) | `{"status":"ok"}`. The process accepts and answers HTTP. It checks no drive. | Only for a probe in flight while the process starts draining (standard error body). After that the connection is refused. |
| `GET /readyz` | Readiness (traffic decisions) and alerting | `{"status":"ready","drives":N}`. No configured drive has reported a failure. | `{"status":"not_ready","drives":N,"failed":M}`. `M` drives have failed closed. With no drive registered: `drives` 0, `failed` 0. While draining: the standard error body instead. |

Both are unauthenticated, accept `GET` and `HEAD`, and send
`Cache-Control: no-store`. The bodies contain no drive ids.

What a `/readyz` 503 means: at least one drive has failed closed. A drive
fails closed when it can no longer prove it holds its writer lease (a renewal
failed or its outcome is unknown, typically because the metadata store was
unreachable or too slow). From
then on every request to that drive returns `503 ESTALE`. **The drive does not
recover by itself; the process must be restarted.** The other drives keep
serving. With one drive per process, a 503 from `/readyz` means the instance
is serving nothing useful.

Do not use `/readyz` as a liveness probe if an automatic restart is not what
you want; see [Failure modes](#failure-modes-and-recovery) for when a restart
helps.

Limits of `/readyz`, stated plainly:

- It performs no I/O. It reports state the drives already hold in memory.
- **Only split-store drives report health.** A `sqlite`-kind, `host` or
  `memory` drive never reports unhealthy; `/readyz` stays 200 for it whatever
  happens to its storage.
- A split-store failure is noticed only after an operation or the background
  lease-renewal worker has recorded it. A storage outage on an idle drive
  shows up when the next renewal fails, not at once.
- `503 ESTALE` is also returned, rarely, for a stale-handle race on a healthy
  drive. One `503 ESTALE` in the proxy log with `/readyz` at 200 is that race;
  the client retries. `503 ESTALE` on every request with `/readyz` at 503 is a
  failed drive.

## Shutdown

SIGINT and SIGTERM (Unix; Ctrl-C on Windows) start a graceful shutdown:

1. The listener stops accepting connections. New connections are refused.
2. Idle keep-alive connections are closed without a response.
3. An upload whose body is still being received is answered
   `503 server is shutting down` with `Connection: close` and changes nothing
   (PUT is atomic). A download in progress can be cut short; the client sees a
   body shorter than `Content-Length` and retries. Other requests in progress
   get up to `drain_timeout_ms` to finish.
4. Every drive is shut down in reverse config order. This flushes the storage
   and releases the writer lease.
5. The process prints `http stopped` to stdout and exits with code 0.

If connections are still open when `drain_timeout_ms` passes (for example a
client that has stopped reading a download), they are aborted. Drive shutdown
**still runs**, so the writer lease is released. The process then prints
`mount-rs: HTTP server close timed out` to stderr, does not print
`http stopped`, and exits with code 1.

Worst-case stop time is `drain_timeout_ms`, plus up to 5 seconds for aborted
requests to release their file handles, plus drive shutdown. Drive shutdown
has no time bound of its own.

Operational consequences:

- Give the service manager a stop timeout above that worst case (for systemd,
  `TimeoutStopSec=60` with the defaults) and let it fall back to SIGKILL
  afterwards, because drive shutdown against unreachable storage can hang.
- A second SIGTERM does not speed the shutdown up.
- SIGKILL skips step 4. A split-store drive's writer lease then stays held
  until it expires; see [Lease takeover wait](#lease-takeover-wait).
- Take the instance out of the load balancer before sending SIGTERM so that
  steps 2 and 3 affect no client.
- With `remote_sessions` or `forwarding` enabled (outside the launch
  contract) a failed drain does **not** release leases.
- A signal that arrives during startup is honoured: drives already opened are
  shut down and the process exits with code 1 without printing the readiness
  line.

### systemd unit

```ini
[Service]
ExecStart=/usr/local/bin/mount-rs serve-http --config /etc/mount-rs/http.json
EnvironmentFile=/etc/mount-rs/tokens.env
KillSignal=SIGTERM
TimeoutStopSec=60
Restart=on-failure
RestartSec=10
LimitNOFILE=65536
```

## Exit codes

| Code | Meaning | Typical stderr |
| --- | --- | --- |
| 0 | Clean stop after a signal. `http stopped` was printed. | |
| 1 | Runtime failure: missing token variable (`missing environment variable NAME`), drive failed to open (storage unreachable; lease held: `EAGAIN: resource temporarily unavailable, acquire writer`; corrupt block), bind failure (port in use), signal during startup, listener exited unexpectedly, drain deadline exceeded (`HTTP server close timed out`), drive shutdown failed. | `mount-rs: <message>` |
| 2 | Usage or configuration error: bad arguments, invalid or unknown config key, non-loopback host, invalid drive id, token with whitespace, config without an `http` section. | `mount-rs: <message>` then `try --help` |

Error messages go to stderr with the prefix `mount-rs: `. Secrets in messages
are redacted. The message is wrapped in ANSI colour escapes even when stderr
is not a terminal; set `NO_COLOR=1` if your log pipeline does not strip them.

`Restart=on-failure` is appropriate: code 1 can be transient (lease not yet
expired), code 2 never is. Add `RestartPreventExitStatus=2` to stop restart
loops on a bad config.

## Observability

State of the default build, without qualification:

| Signal | Available? |
| --- | --- |
| Lifecycle lines on stdout (`http listening at ...`, `http stopped`) | Yes |
| Fatal error on stderr at exit | Yes |
| `mount_cache_stats` JSON lines on stderr | Only with `cache_report_interval_seconds`, only for drives with a cache |
| `{"event":"http_accept_failed","total":N}` JSON line on stderr | Yes, each time the count of failed `accept` calls rises |
| Per-request access log | **No** |
| Metrics endpoint (Prometheus or other) | **No** |
| Request ids, trace headers in responses | **No** |
| Per-request error log (which request got a 500 and why) | **No** |

The error detail a client receives is the POSIX code only; the underlying
storage message is not logged anywhere in the default build.

With the CLI built with `--features observability` and run with
`MOUNT_RS_TELEMETRY=1`, the server records one `http.request` span, one
completion event and the `mount_rs.operations`, `mount_rs.errors` and
`mount_rs.operation.duration_ms` metrics per request, with an outcome and an
error class of `http_client_error` or `http_server_error`. Paths, tokens and
drive ids are not recorded. The CLI does not install an exporter: without a
subscriber and exporter provided by an embedding application these signals go
nowhere. See [`observability.md`](observability.md).

In practice, at launch, **the reverse proxy is the source of request
telemetry**. Configure its access log (status, method, path, upstream time,
bytes, without `Authorization`) and export its metrics.

### What to alert on

| Alert | Source | Meaning | First action |
| --- | --- | --- | --- |
| `/readyz` returns 503 for more than 1 minute | Load balancer or blackbox probe | A drive has failed closed | Check storage, then restart |
| `/healthz` fails or times out | Probe | Process down or hung | Check the service manager; restart |
| Process exits non-zero, or restart loop | Service manager | Startup failure | Read stderr; see exit codes |
| 503 rate from upstream above baseline | Proxy log | Drive failed closed, contention, or draining | Check `/readyz` |
| Any 507 | Proxy log | Storage full or over quota | Free or add space |
| 500 rate above baseline | Proxy log | Storage errors | Check storage health and disk |
| 504 rate above baseline | Proxy log | Slow storage, or upload memory budget saturated | Check storage latency and upload concurrency |
| 502 from the proxy | Proxy log | Connection limit reached, process down, or keep-alive race | Check connection count against `max_connections` and the proxy keep-alive timeout |
| `http_accept_failed` line on stderr | Process stderr | The process could not accept connections, usually out of file descriptors (`EMFILE`/`ENFILE`). It keeps retrying with a 10 ms to 1 s back-off; clients are turned away meanwhile | Raise `LimitNOFILE`; look for leaked connections |
| `HTTP server close timed out` on stderr at stop | Process stderr | The drain deadline passed and requests were aborted | Usually a stalled client; check that `drain_timeout_ms` exceeds `request_timeout_ms` |
| 401 rate spike | Proxy log | Rotated token not picked up, or probing | Check with the consumer |
| 413 rate above baseline | Proxy log | Consumers hitting the upload or listing limit | Talk to the consumer; consider the limits |
| Free disk on the storage volume below threshold | Host | Leads to 507 | Add space |
| Open connections above 80% of `max_connections` | Proxy or host | Approaching refusal | Raise the limit or reduce proxy keep-alive |

## Failure modes and recovery

| Symptom | Cause | Recovery |
| --- | --- | --- |
| All requests to one drive return `503 ESTALE`; `/readyz` is 503 | The drive failed closed: its writer lease could not be renewed | Fix the storage or its connectivity. Restart the process with SIGTERM. Check `/readyz`. Data that was acknowledged is intact; clients were told to retry. |
| `503` with `EBADF` | The drive's filesystem was shut down under the server | Restart. |
| `503` with `EAGAIN`/`EBUSY` and `Retry-After: 1` | Transient contention in storage | None if brief. Sustained: check storage latency. |
| Start fails at once with code 1 and `EAGAIN: resource temporarily unavailable, acquire writer` | Another process holds the drive's writer lease, or a killed process's lease has not expired | Make sure no other instance runs. The process does not wait for the lease; let the supervisor restart it with back-off until the lease expires. See below. |
| Start fails with code 1 and a bind error | Port in use | Stop the other listener or change `port`. |
| Start fails with code 1 and a hash or integrity message | Stored block content does not match its hash | Do not serve. Restore from backup. |
| Start fails with code 1 and `missing environment variable` | Token variable not set | Fix the environment source. |
| `HTTP listener exited unexpectedly`, exit 1 | The accept task ended | Restart. Accept errors, including `EMFILE`/`ENFILE`, are retried with back-off and logged as `http_accept_failed`; they do not cause this. |
| `http_accept_failed` lines, clients time out connecting | Out of file descriptors | Raise `LimitNOFILE`; the listener recovers by itself when descriptors are free. |
| Downloads cut off after `request_timeout_ms` | The client (or the proxy, if it buffers to a slow client) stopped reading | Expected: a download that makes no progress for `request_timeout_ms` is dropped to free its connection and file handle. |
| 507 on writes | Storage full | Free space or grow the volume. No restart needed. |
| Clients get connection resets or the proxy logs 502 | `max_connections` reached, or keep-alive idle race | Lower proxy keep-alive count and idle time, or raise `max_connections` and `LimitNOFILE`. |
| Uploads return 504 under load | Upload memory budget saturated or slow storage | Raise `max_buffered_upload_bytes` if memory allows; reduce upload concurrency at the proxy. |
| A directory returns `413 EOVERFLOW` | More than `max_directory_entries` entries, or listing above `max_response_bytes` | Raise the limits (needs restart) or have the consumer restructure. |
| A file starting with `.mount-rs-upload.` exists in a `host` drive's directory | The process was killed during a `PUT` on a `host` drive | It stays on disk, hidden from the API, and is removed when its otherwise-empty directory is deleted through the API. Safe to delete by hand while no upload to that directory is in flight. Only `host` drives create such files. |

### Lease takeover wait

A split-store drive is protected by an exclusive writer lease stored with its
metadata. A clean shutdown releases it. After a crash, SIGKILL, power loss or
host failure the lease stays held until it expires, and **a new process cannot
open the drive before then**. The new process does not wait: it fails at
startup with `EAGAIN: resource temporarily unavailable, acquire writer` and
exit code 1, every time, until the lease has expired. Setting the same `owner`
in the config does not shorten this.

The lease lifetime is the drive's `lease_ttl_ms`. Its default depends on how
the drive is configured:

| Drive config | Default lease lifetime |
| --- | --- |
| `splitstore` with a structured `storage` object | 240 seconds |
| `splitstore` with the legacy `database` + `blocks` pair | 30 seconds |

The lease is renewed in the background while the process runs, so the wait
after a kill is up to one full lifetime.

- Let the service manager retry with back-off (`Restart=on-failure`,
  `RestartSec=10`). With the 240-second default expect about 25 failed starts
  before the one that succeeds.
- Do not delete or edit the lease by hand.
- A shorter `lease_ttl_ms` shortens recovery but leaves less headroom for slow
  storage; a lease that cannot be renewed in time fails the drive closed.
- Drives of kind `sqlite` (single-file snapshot) and `host` have no lease and
  reopen immediately. They also have no protection against two processes
  serving the same files; never run two.

## Capacity

- **Connections.** At most `max_connections` (256). Each uses a file
  descriptor; each open download or upload uses more inside the storage layer.
  Set `LimitNOFILE` well above `max_connections`. Excess connections are closed
  without a response, so keep the proxy's upstream pool below the limit.
- **Upload memory.** Uploads are buffered whole before publication. The
  buffered total is bounded by `max_buffered_upload_bytes` (default
  4 x 8 MiB = 32 MiB), not by `max_connections` x `max_request_bytes`. Peak
  memory is roughly twice the budget, because the drive copies the data while
  publishing it; plan for about 64 MiB with the defaults. Uploads beyond the
  budget wait in arrival order, and fail with 504 if the wait exceeds the
  request timeout. Concurrent
  full-size uploads beyond `max_buffered_upload_bytes / max_request_bytes`
  (four by default) therefore queue. Raise the budget if consumers upload many
  large files in parallel.
- **Listing memory.** A listing is built in memory, up to `max_response_bytes`
  per request, and is not covered by the upload budget. Gzip compression runs
  on at most four listings at a time; waiting requests keep their listing in
  memory. Worst case is roughly `max_connections` x `max_response_bytes`. Lower
  `max_response_bytes` or `max_directory_entries` if consumers list large
  directories concurrently.
- **Download memory.** About two chunks of `read_chunk_bytes` per active
  download.
- **Throughput.** One process, one writer per drive. Capacity is added by
  putting drives in separate processes, not by adding replicas of one drive.
- **Storage caches.** Split-store block and disk caches are sized per drive in
  the driver config. `cache_capacity` makes startup fail if their sum exceeds a
  stated budget.
- **Largest object.** `max_request_bytes`. Raising it raises per-upload memory
  and requires the proxy cap and the upload budget to follow.

## Backup and restore (SQLite-backed drives)

There is no backup command in `mount-rs` and no qualified online backup
procedure. The supported procedure is a cold copy.

A split-store drive on SQLite consists of two databases: the metadata database
and the block database. They are one unit. A metadata database restored with
an older or newer block database produces missing-block or hash-mismatch
errors.

Backup:

1. Stop the service with SIGTERM and wait for exit code 0. This flushes the
   storage and releases the lease.
2. Copy both database files together, plus any `-wal` and `-shm` files that
   exist next to them. A filesystem or volume snapshot taken while the process
   is stopped is equivalent.
3. Start the service.

Restore:

1. Stop the service.
2. Replace both databases with the pair from the same backup. Remove leftover
   `-wal` and `-shm` files that do not belong to the restored pair.
3. Start the service and check `/readyz`, then read a known file.

Notes:

- Writes acknowledged after the backup are lost on restore. Tell consumers the
  restore point.
- A backup taken from a running process, or a copy made after a crash, carries
  a live lease. Starting from it waits for the lease to expire first.
- Keep the database files on local disk. SQLite files on NFS or SMB are not
  supported.
- A `sqlite` kind drive is a single database file; the same cold-copy rule
  applies.
- Drives on other providers (TiDB, PGlite, FoundationDB, object stores) are
  backed up with those systems' tools; see the provider runbooks in `docs/`.

## Pre-launch checklist

Configuration

- [ ] `host` is loopback and `port` is fixed (not `0`).
- [ ] `remote_sessions` is `false` or absent; no `forwarding` block;
      `http2_prior_knowledge` is `false`.
- [ ] No `memory` drive.
- [ ] `drain_timeout_ms` > `request_timeout_ms`.
- [ ] Twice `max_buffered_upload_bytes` fits the memory limit of the process.
- [ ] No drive sets `writeback`, unless its consumers accept non-durable `200`s.
- [ ] `mount-rs validate-config --config <path>` passes.
- [ ] Storage paths are on local, durable disk with monitored free space.

Secrets

- [ ] Each drive has its own random token, delivered through the secret
      mechanism, not the command line or the config file.
- [ ] Consumers know that rotation needs a restart and that the old token
      stops working at once.

Proxy

- [ ] TLS terminates at the proxy; the proxy shares the server's network
      namespace.
- [ ] Body cap equals `max_request_bytes`; request buffering on; response
      buffering off.
- [ ] Upstream timeouts above `request_timeout_ms`; upstream keep-alive idle
      below it; upstream pool below `max_connections`.
- [ ] No automatic upstream retries.
- [ ] Request URI forwarded unmodified (`a%2Fb` test returns the server's 400).
- [ ] Access log on, `Authorization` not logged; `/healthz` and `/readyz` not
      public.

Process

- [ ] Exactly one instance per drive.
- [ ] Stop signal SIGTERM; stop timeout above `drain_timeout_ms` + 5 s +
      storage flush, with SIGKILL as the fallback.
- [ ] Restart on failure with a delay of several seconds; no restart on exit
      code 2.
- [ ] `LimitNOFILE` raised.
- [ ] Liveness on `/healthz`, readiness on `/readyz`.

Verification on the production host

- [ ] Start, see the readiness line, `/readyz` returns 200.
- [ ] Through the proxy: `PUT`, `GET`, ranged `GET`, list, rename, delete.
- [ ] Wrong token and unknown drive both return 401.
- [ ] An upload one byte over the limit is rejected with 413.
- [ ] SIGTERM exits 0 with `http stopped`; restart succeeds immediately and
      the uploaded file is still there.
- [ ] SIGKILL, then observe restarts fail with `EAGAIN ... acquire writer`
      until the lease expires, and then succeed.
- [ ] Cold backup and restore rehearsed once.

Operations

- [ ] Alerts from the table above are wired to the proxy log, the probes and
      the process stderr (`http_accept_failed`).
- [ ] Consumers have the API guide and `openapi.yaml`, including the limits
      and the "not supported at launch" list.
