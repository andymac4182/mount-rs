# serve-http operator runbook

How to run `mount-rs serve-http` in production. Everything up to
[Clusters](#clusters) describes one server with `remote_sessions: false` and
no `forwarding` block, and applies to each server of a cluster too. The
[Clusters](#clusters) chapter covers remote sessions, forwarding between
servers and drive failover.

- API contract for consumers: [`transports/mount-rs-http/README.md`](../transports/mount-rs-http/README.md)
  and [`transports/mount-rs-http/openapi.yaml`](../transports/mount-rs-http/openapi.yaml).
- Config file basics and a local demo: [`apps/mount-rs-cli/README.md`](../apps/mount-rs-cli/README.md#unmounted-http-service).

## What the process is

One process serves one or more named drives over cleartext HTTP/1.1, on a
loopback address unless configured otherwise. Each drive has its own
bearer token. The process holds
the exclusive writer lease of every split-store drive it serves, so **exactly
one instance may serve a given drive**. There is no active-active mode at
launch; a second instance pointed at the same storage fails to start.
(Several servers can stand in for one another for the same drive, one at a
time: see [Clusters](#clusters).)

The process does not terminate TLS, and by default it listens only on a
loopback address and writes no access log. A reverse proxy in front of it
provides the first two; the process's own access log is opt-in (see
[Access log](#access-log)). Where the network up to the listener is private (a
container network, or a VPC behind a load balancer that terminates TLS),
`allow_non_loopback` lets it listen on that network directly; see
[Listening beyond loopback](#listening-beyond-loopback).

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
2. Read each drive's token from its environment variable or its token
   file.
3. Install the SIGINT/SIGTERM handler.
4. Open each drive in config order. A split-store drive acquires its writer
   lease here. If any drive fails to open, the drives already opened are shut
   down again and the process exits with code 1.
5. Bind the listener.
6. Print the readiness line.

### In a container

Each CLI preview release also publishes an image for `linux/amd64` and
`linux/arm64` to `ghcr.io/andymac4182/mount-rs:<version>`, for example
`0.1.0-cli-preview.1` for the tag `v0.1.0-cli-preview.1`, with a provenance
attestation (`gh attestation verify oci://ghcr.io/andymac4182/mount-rs:<version> --repo andymac4182/mount-rs`).
`docker build -t mount-rs .` at the repository root builds the same image.

- The entrypoint is `mount-rs`; pass the subcommand, as in
  `serve-http --config /etc/mount-rs/http.json`.
- It runs as uid and gid 10001. `/var/lib/mount-rs` is a volume owned by that
  user: keep SQLite files and other state there, and give absolute paths in
  the config, since relative paths resolve against the config file's
  directory.
- By default the HTTP listener binds loopback only, so nothing outside the
  container's network namespace can reach it. Either run the TLS proxy in
  the same namespace (a sidecar container in the same Kubernetes pod, or
  `docker run --network container:<mount-rs container>`), or set
  `"host": "0.0.0.0"` with `"allow_non_loopback": true` and publish the port
  (`-p 8080:8080`, a Kubernetes Service, a load balancer target group) on a
  network you trust; see [Listening beyond loopback](#listening-beyond-loopback).
  The forwarding listener (`forwarding.bind`) may bind `0.0.0.0` for peers on
  other hosts.
- Stopping the container sends SIGTERM, which drains as described under
  [Shutdown](#shutdown). Give it a stop timeout above `drain_timeout_ms` +
  5 s + `drive_shutdown_timeout_ms` (`docker stop -t 75`, or
  `terminationGracePeriodSeconds: 75` in Kubernetes, with the defaults).
- Probe `/readyz` and `/healthz` from the sidecar or with an HTTP probe sent
  through it.

```sh
docker run -d --name mount-rs \
  -e MOUNT_RS_FILES_TOKEN=... \
  -v /etc/mount-rs/http.json:/etc/mount-rs/http.json:ro \
  -v mount-rs-data:/var/lib/mount-rs \
  ghcr.io/andymac4182/mount-rs:<version> serve-http --config /etc/mount-rs/http.json
```

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
| `host` | string | `127.0.0.1` | `127.0.0.1`, `::1`, `[::1]`, `localhost`, or any loopback IP; any address with `allow_non_loopback` | Without `allow_non_loopback`, non-loopback hosts are rejected at config validation and again at bind. |
| `allow_non_loopback` | boolean | `false` | | Lets `host` be a non-loopback address such as `0.0.0.0`. The listener stays cleartext. See [Listening beyond loopback](#listening-beyond-loopback). |
| `port` | integer | `0` | 0-65535 | `0` picks an ephemeral port. Set a fixed port in production. |
| `max_request_bytes` | integer | `8388608` (8 MiB) | >= 1 | Largest body received whole in memory: JSON bodies, session writes, and file uploads up to this size. A file upload above it is streamed to storage instead (see `max_upload_bytes`). Other larger requests get 413. |
| `max_upload_bytes` | integer | `5368709120` (5 GiB) | >= `max_request_bytes` | Largest file a `PUT` may upload. Larger uploads get 413 and leave the target unchanged. Uploads above `max_request_bytes`, or without a `Content-Length` that grow past it, are stored as they arrive and published once complete: a split-store drive stores the blocks and publishes the file in one step; other drives write a hidden temporary file in up to 8 MiB writes and rename it over the target. |
| `max_buffered_upload_bytes` | integer | 4 x `max_request_bytes` | >= `max_request_bytes` (a smaller value is a config error, exit 2) | Upload bytes the server holds in memory across all in-flight `PUT`s. A `PUT` reserves its `Content-Length` (or `max_request_bytes` without one) before its body is read; a streamed upload reserves one write buffer (the smaller of 8 MiB and `max_request_bytes`). Uploads beyond the budget wait in arrival order, up to `request_timeout_ms`. |
| `max_response_bytes` | integer | `33554432` (32 MiB) | >= 1 | Largest directory listing, or page of one (uncompressed JSON). Larger listings get 413; a consumer can then list in pages with `?limit=`. Does not limit file downloads. |
| `max_directory_entries` | integer | `100000` | >= 1 | Largest directory that can be listed, in one response or in pages, and the largest page `limit`. Larger directories get 413. An entry is about 70 bytes of JSON with short names, so 100,000 entries is about 7 MB (0.3 MB gzipped from `/entries`). |
| `read_chunk_bytes` | integer | `65536` | >= 1 | Read size per chunk when streaming a download. |
| `request_timeout_ms` | integer | `30000` | >= 1 | Limit for one request up to its response headers, including receiving the body and waiting for upload budget; exceeding it returns 504. A streamed upload (above `max_request_bytes`, or without a `Content-Length`) has no overall limit: instead each piece of its body, each write to the drive and the final publication must happen within this time, or it gets 504 and leaves the target unchanged. Also the header-read and idle keep-alive timeout (the connection is closed without a response) and the time a download may make no progress before it is cut. |
| `drain_timeout_ms` | integer | `35000` | >= 1 | How long shutdown waits for in-flight requests before aborting them. Keep it above `request_timeout_ms`. |
| `drive_shutdown_timeout_ms` | integer | `30000` | >= 1 | How long shutdown gives the drives, all together, to flush and release their writer leases once the listener has stopped. When it passes the process exits with code 1 without waiting further. See [Shutdown](#shutdown). |
| `max_connections` | integer | `256` | >= 1 | Concurrent TCP connections. Excess connections are accepted and closed immediately with no HTTP response. |
| `access_log` | boolean | `false` | | Writes one `http_request` JSON line per request to stderr. No path, query, token or header is written. See [Access log](#access-log). |
| `metrics` | object | none | `{"listen": "IP:PORT"}`, optionally `"allow_non_loopback": true` | Serves Prometheus text at `GET /metrics` on a listener of its own, without a token. `listen` must be a loopback address unless `allow_non_loopback` is `true`. Off unless set. See [Metrics](#metrics). |
| `token_reload_seconds` | integer | `30` | 0-3600 | How often the token files of the drives are looked at for a change. `0` turns the timer off, leaving SIGHUP as the only trigger. Only valid when a drive's token is a file. See [Rotation](#rotation). |
| `tcp_nodelay` | boolean | `true` | | Disables Nagle on accepted connections. |
| `http2_prior_knowledge` | boolean | `false` | | `true` makes the listener cleartext HTTP/2 only (no HTTP/1.1). Leave `false` at launch; the reference proxy configs below speak HTTP/1.1 upstream. With HTTP/2 a stalled download releases its file handle but its connection is not dropped. |
| `drives` | array | required | at least one drive; `[]` only with `forwarding` | See below. |
| `cache_capacity` | object | none | `{"ram_bytes": n, "disk_bytes": n}`, both required, >= 0 | Validation only: startup is refused when the sum of the drives' configured block-cache and disk-cache sizes exceeds these numbers. |
| `disk_cache_budget` | object | none | `{"workers": 1-512, "fill_copies": 1-512}`, both required | Shared disk-cache worker budget for split-store drives that configure a disk cache. |
| `tidb_metadata_pool_limit` | integer | none | >= 1 | Shares one TiDB connection pool of this size between drives that use the same TiDB connection variable. Only for TiDB metadata. |
| `cache_report_interval_seconds` | integer | none | >= 1 | Writes a `mount_cache_stats` JSON line per drive to stderr at this interval. Only drives with a block or disk cache report. |
| `remote_sessions` | boolean | `false` | | Serves the session and handle routes. See [Cluster configuration reference](#cluster-configuration-reference). |
| `sessions` | object | defaults | | Session limits; only with `remote_sessions`. Same reference. |
| `forwarding` | object | none | | Makes the server part of a cluster. Same reference. |

### `http.drives[]` keys

| Key | Type | Required | Valid values |
| --- | --- | --- | --- |
| `id` | string | yes | 1-64 characters from `A-Z a-z 0-9 - _ .`; not `.` or `..`; unique in the file. It appears in URLs. |
| `token` | object | yes | Exactly `{"env": "NAME"}` or exactly `{"file": "PATH"}`. The token itself cannot be written in the config. `env` names a variable read once at startup. `file` names a file that lists the drive's tokens and is read again while the server runs; a relative path is taken from the config file's directory. With `remote_sessions` every token in the file must have the form that option requires. See [Tokens](#tokens) and, for `remote_sessions` or `forwarding`, [Token rotation in a cluster](#token-rotation-in-a-cluster). |
| `driver` | object | yes | A `memory`, `host`, `sqlite` or `splitstore` driver object, in the same shape as native mount configs. `remote` is rejected. |

`memory` drives lose all data on restart. Do not serve one in production.

Durability of an acknowledged `PUT` depends on the drive. Split-store drives on
SQLite acknowledge after the data is durable, unless the storage object sets
`writeback`, in which case a `200` is returned before the data is durable. Do
not enable `writeback` on a drive whose consumers rely on the documented
"durable before 200".

The checked-in `apps/mount-rs-cli/examples/config-http.json` is a demo: it has
`port: 0` and a `memory` drive. It sets every limit to its default value
explicitly (`max_request_bytes` 8388608, `max_upload_bytes` 5368709120,
`max_buffered_upload_bytes` 33554432, `drain_timeout_ms` 35000,
`request_timeout_ms` 30000).

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

- Each drive gets its token from one of two places:
  - `drives[].token.env`: an environment variable, read once at startup.
    Changing it needs a restart.
  - `drives[].token.file`: a file, read at startup and again while the
    server runs. This is what makes [rotation](#rotation) without a restart
    possible.
- Generate at least 32 random bytes, for example
  `openssl rand -base64 32 | tr '+/' '-_' | tr -d '='`.
- A token must be non-empty and must not contain whitespace. An invalid token
  exits with code 2; a missing variable exits with code 1. Neither message
  contains the token.
- A token file lists one token per line: the current token, and optionally
  the one before it. Blank lines and white space around a token are
  ignored. More than two tokens, no token, a token with white space inside
  it, a file over 64 KiB or one that is not UTF-8 is not valid. At startup a
  file that is missing or not valid exits with code 2 and a message that
  names the drive, the file and the line, never its contents.
- A token is never printed, never written to the config, and never returned
  by the API.
- Two drives may be given the same token. `GET /v1/drives` then lists both for
  that token. Use distinct tokens unless one consumer owns both drives.
- A token grants full read and write access to its drive. There are no
  read-only tokens.
- Deliver the variables through the service manager's secret mechanism (for
  systemd, `EnvironmentFile=` with mode 0600, or `LoadCredential=`). Do not put
  tokens on the command line.
- A token file must be readable by the service user and by no one else
  (mode 0600, or a mounted secret). The server does not check its
  permissions.

### Rotation

A drive whose token is a file is rotated **without a restart**, with a
period in which the old and the new token both work:

1. Write the new token as the first line of the file and keep the old one
   as the second. Replace the file by writing a new one beside it and
   renaming it into place, so that the server never reads half a file.
2. The server picks the file up within `token_reload_seconds` (30 by
   default), or at once on `SIGHUP`. From then on both tokens are accepted.
3. Move the consumers to the new token.
4. Remove the old token's line. Once that file is picked up the old token
   gets the same `401` as any wrong token.

Every reload writes one line to stderr:

```json
{"event":"http_token_reload","drive":"files","outcome":"reloaded","tokens":2,"error":null}
```

`outcome` is `reloaded` (the accepted tokens changed), `unchanged` (the file
lists the tokens already accepted) or `refused`. `tokens` is how many tokens
the drive accepts after the reload. `error` says why a file was refused and
never quotes it. Check for `reloaded` with the count you expect before you
tell consumers to switch, and again before you rely on the old token being
gone.

What to know:

- **A file that is not valid changes nothing.** A file that cannot be read,
  or that does not list one or two valid tokens, is refused whole: the
  tokens accepted before stay accepted and one `refused` line is written.
  The timer reports a bad file once, not at every look; `SIGHUP` always
  reports. Deleting the file does not lock anyone out and does not revoke
  anything.
- **To revoke a token at once**, write a file with only the new token and
  send `SIGHUP`. There is no overlap and consumers holding the old token
  get `401` from that moment.
- A request is checked against the tokens in force when it arrives. A
  request already under way when its token is dropped finishes.
- With a token file, `SIGHUP` reloads every token file and the process
  keeps running. **A server with no token file does not handle `SIGHUP`:**
  the signal ends the process, as it always has.
- A mounted Kubernetes Secret is updated by the kubelet by renaming, so it
  is picked up by the timer with no signal.
- `token.file` works with `remote_sessions` and `forwarding` too: the file
  routes, the session routes and the owner of a drive in a cluster all read
  the one set of tokens a reload replaces. What changes there (which
  servers hold the file, what happens to open sessions, the stricter token
  form) is in [Token rotation in a cluster](#token-rotation-in-a-cluster).

A drive whose token is an environment variable **rotates by restart**:

1. Give the new token to the consumers, who must be able to switch on a 401.
2. Update the environment source.
3. Restart the service (SIGTERM, wait for exit, start).
4. From the moment the new process is ready, the old token returns 401.

During that restart the API is unavailable (the proxy returns 502), and
there is no window in which both tokens work.

The reverse proxy must not cache or log the `Authorization` header.

## Reverse proxy

The listener is cleartext and, by default, loopback-only. A proxy is
mandatory for any client that is not on the same host, unless the listener is
opened to a private network as described below.

Requirements:

| Requirement | Why |
| --- | --- |
| Terminate TLS at the proxy | The server has no TLS. |
| Run in the same network namespace as the server (same host, or same pod as a sidecar) | The server binds loopback only. A proxy on another host or in another container network cannot reach it. |
| Forward the request path unmodified | The server validates percent-encoding itself. Do not let the proxy decode, normalise or merge slashes in the upstream URI. |
| Forward `Authorization`, `Range`, `If-Range`, `If-Match`, `If-None-Match`, `Accept-Encoding`, `X-Request-Id` and the response's `ETag` and `X-Request-Id` unchanged; do not cache | Authentication is per request. Responses depend on the token. Conditional writes rely on the tags reaching both ways intact. |
| Request body cap >= `max_upload_bytes` | A smaller cap rejects uploads the server would accept. Equal is best: then the proxy rejects oversize uploads without opening an upstream connection. |
| Stream request bodies upstream; do not buffer them | Large uploads are streamed to storage as they arrive. A proxy that buffers a multi-gigabyte upload to its own disk first doubles the disk traffic and delays the upload's start. The server gives up on an upload that stalls for `request_timeout_ms`, so a slow client cannot hold it forever. |
| Do not buffer or cap response bodies | Downloads have no size limit and are streamed. |
| Upstream read/send timeout slightly above `request_timeout_ms` | The server answers 504 itself at `request_timeout_ms`; the proxy should not give up first. |
| Upstream keep-alive idle timeout shorter than `request_timeout_ms` | The server closes a keep-alive connection that has been idle for `request_timeout_ms`. A proxy that reuses such a connection at that moment gets a reset. |
| Upstream connections < `max_connections` | Connections beyond the limit are closed without a response, which the proxy reports as 502. |
| Do not retry non-idempotent requests upstream | A retried `rename` or non-recursive `mkdir` can fail spuriously. Safest is no automatic retry at all; consumers have retry rules. |
| Write an access log without the `Authorization` header | The server's own access log is off by default and, when on, records route templates rather than paths. The proxy's log is where paths and client addresses are. |
| Keep `/readyz` and `/healthz` reachable by the load balancer only | They are unauthenticated. |

Errors generated by the proxy do not have the API's JSON error body. Consumers
are told to expect that.

### Listening beyond loopback

With `"allow_non_loopback": true`, `host` may be any address, for example
`0.0.0.0` to listen on every interface. Use it when the network between the
clients, or the load balancer that terminates TLS, and the listener is
private: a container or pod network, or instances in a VPC behind an AWS ALB
whose target group uses HTTP. The listener still has no TLS: bearer tokens
and file contents cross that network unencrypted, and anything that can reach
the port can try tokens. Then:

- Restrict who can reach the port: a security group that admits only the
  load balancer, a Kubernetes NetworkPolicy, or a port published only on a
  private interface.
- The load balancer takes the proxy's place, so the requirements above apply
  to it: forward the path and `Authorization`, `Range` and conditional headers
  unchanged, do not cache, keep its body cap at least `max_upload_bytes`, its
  idle timeout below `request_timeout_ms` (an ALB's default of 60 s is above
  the default 30 s: lower the ALB's or raise `request_timeout_ms`), and
  point its health check at `/readyz`.
- Every server of a forwarding cluster can then be a target of the same load
  balancer directly.

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

    client_max_body_size 5g;      # equals max_upload_bytes (5368709120)
    client_body_timeout 30s;      # between two reads of the body, as the server's own

    location / {
        proxy_pass http://mount_rs_http;   # no URI part: the request URI is forwarded unmodified
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_set_header Host $host;

        proxy_request_buffering off;       # stream uploads; the server streams them to storage
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
        max_size 5GiB
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
returns the server's `400`, and that an upload larger than `max_request_bytes`
(say 100 MiB) succeeds and reads back whole.

## Health checks

| Probe | Use for | 200 | 503 |
| --- | --- | --- | --- |
| `GET /healthz` | Liveness (restart decisions) | `{"status":"ok"}`. The process accepts and answers HTTP. It checks no drive. | Only for a probe in flight while the process starts draining (standard error body). After that the connection is refused. |
| `GET /readyz` | Readiness (traffic decisions) and alerting | `{"status":"ready","drives":N}`. No configured drive has reported a failure. | `{"status":"not_ready","drives":N,"failed":M}`. `M` drives cannot serve: they have failed closed, or their storage is unreachable. With no drive registered: `drives` 0, `failed` 0. While draining: the standard error body instead. |

Both are unauthenticated, accept `GET` and `HEAD`, and send
`Cache-Control: no-store`. The bodies contain no drive ids.

What a `/readyz` 503 means: at least one drive cannot serve. What that is,
and whether it clears by itself, depends on the drive's kind:

| Drive kind | Not ready when | Noticed | Clears by itself? |
| --- | --- | --- | --- |
| `splitstore` | The drive has failed closed: it can no longer prove it holds its writer lease (a renewal failed or its outcome is unknown, typically because the metadata store was unreachable or too slow). Every request to it then returns `503 ESTALE`. | By the lease-renewal worker in the background, or by the operation that failed | **No. The process must be restarted.** |
| `host` | The root directory cannot be opened: removed, renamed, unmounted, permission lost, or on a mount that has stopped answering | By a check every 2 seconds, off the request path | Yes, at the first check after the directory is back |
| `sqlite` | The database file cannot be opened as a regular file (removed, renamed, its volume unmounted or not answering), **or** the most recent save to it failed (disk full, I/O error, the database changed underneath) | The file: by a check every 2 seconds. A failed save: at once | Yes: at the first check after the file is back, and at the first save that succeeds (any later write, or `POST .../ops/sync`) |
| `memory` | Never. It has no storage that can be lost; its content lives and dies with the process. | | |

The storage check of a `host` or `sqlite` drive opens the directory or the
file and nothing more. One that has not returned after 5 seconds counts as a
failure for as long as it stays out, which is how a hung network mount shows
up. Each change is written to stderr as one line:

```json
{"event":"http_drive_storage","drive":"files","reachable":false,"error":"entity not found","check_ms":0}
```

`drive` is the drive id, `reachable` what the check now finds, `error` the
kind of failure (`null` when reachable) and `check_ms` how long the check
took. A check that took longer than 5 seconds writes a line when it returns
even if what it found is unchanged. Nothing is written while storage stays
as it was.

The check is about readiness only. Requests to a drive that is not ready
are still given to it and get whatever its storage answers (typically `404`
or `500` from a `host` drive whose root is gone); a `sqlite` drive keeps
answering reads from memory. The other drives keep serving. With one drive
per process, a 503 from `/readyz` means the instance is serving nothing
useful.

Do not use `/readyz` as a liveness probe if an automatic restart is not what
you want; see [Failure modes](#failure-modes-and-recovery) for when a restart
helps.

Limits of `/readyz`, stated plainly:

- The probe itself performs no I/O. It reports what the drives and their
  storage checks already hold in memory, so it answers at once even when
  storage does not.
- A split-store failure is noticed only after an operation or the background
  lease-renewal worker has recorded it. A storage outage on an idle drive
  shows up when the next renewal fails, not at once.
- The `host` and `sqlite` checks prove that the directory or file can be
  opened, not that it can be written: a full disk or a read-only remount is
  not noticed by the check. A `sqlite` drive reports it at its next failed
  save; a `host` drive does not report it, and its writes answer `507` or
  `403`.
- A `sqlite` database file that is deleted while the process runs is
  reported, but the process may still hold the deleted file open, and then
  saves to it appear to succeed. Stop the process and restore the file
  rather than waiting for readiness to return.
- A split-store drive whose providers are local files (`sqlite` metadata or
  blocks, `host-file` blocks) is covered by its lease renewal like any
  other split-store drive, not by a file check.
- `503 ESTALE` is also returned, rarely, for a stale-handle race on a healthy
  drive. One `503 ESTALE` in the proxy log with `/readyz` at 200 is that race;
  the client retries. `503 ESTALE` on every request with `/readyz` at 503 is a
  failed drive.

## Shutdown

SIGINT and SIGTERM on Unix start a graceful shutdown. On Windows, Ctrl-C,
Ctrl-Break, closing the console window and system shutdown do; Windows
ends the process about 5 seconds after the last two, so they stop within
4 seconds whatever `drain_timeout_ms` says (see
[Windows console close and system shutdown](#windows-console-close-and-system-shutdown)),
and `taskkill /F` is never graceful. The
platforms each release is tested on are in
[`support-matrix.md`](support-matrix.md). A graceful shutdown:

1. The listener stops accepting connections. New connections are refused.
2. Idle keep-alive connections are closed without a response.
3. An upload whose body is still being received is answered
   `503 server is shutting down` with `Connection: close` and changes nothing
   (PUT is atomic). A download in progress can be cut short; the client sees a
   body shorter than `Content-Length` and retries. Other requests in progress
   get up to `drain_timeout_ms` to finish.
4. Every drive is shut down in reverse config order. This flushes the storage
   and releases the writer lease. The drives have `drive_shutdown_timeout_ms`
   for this, all together.
5. The process prints `http stopped` to stdout and exits with code 0.

If connections are still open when `drain_timeout_ms` passes (for example a
client that has stopped reading a download), they are aborted. Drive shutdown
**still runs**, so the writer lease is released. The process then prints
`mount-rs: HTTP server close timed out` to stderr, does not print
`http stopped`, and exits with code 1.

If the drives have not all shut down when `drive_shutdown_timeout_ms` passes
(storage that has stopped answering, or a flush that needs longer than the
bound), the process stops waiting. It prints, for example,

```text
mount-rs: drive shutdown timed out after 30000 ms: drive 'files' did not finish and 'scratch' not attempted; a writer lease not released stays held until it expires, and writeback data not yet flushed may be lost
```

to stderr, does not print `http stopped`, and exits with code 1 at once. It
does not claim anything that did not happen:

- The drive named as not finished was abandoned mid-shutdown, and the
  drives named as not attempted (those before it in the config) were never
  shut down. Their writer leases were **not** released; the next start
  waits for them as after a SIGKILL (see
  [Lease takeover wait](#lease-takeover-wait)). Drives listed after the
  stuck one in the config had already been shut down.
- Every write that was acknowledged as durable is still durable: a `200`
  for a `PUT` is sent only after the data is stored, and the timeout
  changes nothing that is stored. The exception is the one that always
  applies to an unclean stop: a drive with `writeback` may lose
  acknowledged data it had not yet flushed.
- Nothing is cleaned up on the way out. The process exits without running
  the remaining teardown, because that teardown can block on the same
  storage. This is the same state as after a SIGKILL at that moment, and
  the next start recovers from it in the same way.

Worst-case stop time is therefore bounded: `drain_timeout_ms`, plus up to 5
seconds for aborted requests to release their file handles, plus
`drive_shutdown_timeout_ms` (plus up to 2 seconds to flush the access log
when it is enabled). With the defaults that is 70 seconds. A stop with no
stalled client and healthy storage does not wait for any of these.

### Windows console close and system shutdown

Windows ends a console process about **5 seconds** after it closes the
process's console window or shuts the system down, whether or not the
process has finished. Those two stops therefore do not use the limits
above. The whole stop gets 4 seconds instead: the drain gets up to 2 of
them, and the drives get what is left (at least 1 second), each capped by
`drain_timeout_ms` and `drive_shutdown_timeout_ms` when those are lower.
The last second is for the process to exit. The console handler does not
return to Windows until the process exits, which would otherwise let
Windows end the process at once.

- An idle server stops as after Ctrl-C: `http stopped`, exit code 0, writer
  lease released.
- A request still in progress after 2 seconds is abandoned. The process
  prints `mount-rs: drain cut short after 2000 ms: Windows ends the process
  about 5 s after a console close or system shutdown`, releases the drives
  and exits with code 1 at once. An upload that had not finished changes
  nothing.
- Drives that need longer than what is left to shut down are left as
  described above for `drive_shutdown_timeout_ms`; their leases stay held
  until they expire.

Ctrl-C and Ctrl-Break have no such limit and get the full drain. Stop a
server that must finish long transfers with one of those. User logoff is
not a stop.

What is tested: the Windows end-to-end job (`serve-http-e2e (windows-2025)`)
closes the server's console window the way its close button does, by
posting `WM_CLOSE` to it, once with the server idle and once with an upload
stalled half-way under a 60-second drain limit. Both times it checks the
exit code, that the process exited within 5 seconds of the close, that an
immediate restart on the same storage starts (so the writer lease was
released), and that data written before the close is intact. The console
is one of the classic console host (conhost): the job sets the runner's
user to it, because the runner otherwise hands new consoles to a
pseudo-console terminal whose window ignores `WM_CLOSE`. Closing a Windows
Terminal tab that runs the server is not tested. System
shutdown is not tested: it is only delivered when Windows really shuts
down. A unit test checks that a shutdown event takes the same path as a
console close and that logoff is not a stop, so only the delivery of the
event by Windows is untested.

Operational consequences:

- Give the service manager a stop timeout above that worst case (for systemd,
  `TimeoutStopSec=75` with the defaults) and let it fall back to SIGKILL
  afterwards. The process now ends by itself when storage hangs; SIGKILL
  remains the backstop.
- Size `drive_shutdown_timeout_ms` for the slowest legitimate flush, not the
  typical one. Cutting a healthy but slow shutdown short leaves the lease
  held, and the replacement then waits out the lease instead of starting at
  once. Raise it for drives with `writeback` or large unflushed caches.
- With several drives in one process, a drive that hangs keeps the drives
  before it in the config from being shut down within the bound. One drive
  per process avoids that.
- A second SIGTERM does not speed the shutdown up.
- SIGKILL skips step 4. A split-store drive's writer lease then stays held
  until it expires; see [Lease takeover wait](#lease-takeover-wait).
- Take the instance out of the load balancer before sending SIGTERM so that
  steps 2 and 3 affect no client.
- The same holds with `remote_sessions` or `forwarding` enabled: a failed
  drain still withdraws the Redis route, closes sessions and releases leases.
- A signal that arrives during startup is honoured: drives already opened are
  shut down and the process exits with code 1 without printing the readiness
  line.

### systemd unit

```ini
[Service]
ExecStart=/usr/local/bin/mount-rs serve-http --config /etc/mount-rs/http.json
EnvironmentFile=/etc/mount-rs/tokens.env
KillSignal=SIGTERM
TimeoutStopSec=75
Restart=on-failure
RestartSec=10
LimitNOFILE=65536
```

## Exit codes

| Code | Meaning | Typical stderr |
| --- | --- | --- |
| 0 | Clean stop after a signal. `http stopped` was printed. | |
| 1 | Runtime failure: missing token variable (`missing environment variable NAME`), drive failed to open (storage unreachable; lease held: `EAGAIN: resource temporarily unavailable, acquire writer`; corrupt block), bind failure (port in use), signal during startup, listener exited unexpectedly, drain deadline exceeded (`HTTP server close timed out`), drive shutdown failed, drive shutdown deadline exceeded (`drive shutdown timed out after N ms: ...`). | `mount-rs: <message>` |
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
| `{"event":"http_token_reload",...}` JSON line on stderr | Yes, each time a token file is reloaded or refused (see [Rotation](#rotation)) |
| `{"event":"http_drive_storage",...}` JSON line on stderr | Yes, each time the storage of a `host` or `sqlite` drive becomes unreachable or reachable again (see [Health checks](#health-checks)) |
| Per-request access log (`http_request` JSON lines on stderr) | Only with `"access_log": true`; off by default (see [Access log](#access-log)) |
| Metrics endpoint (Prometheus text) | Only with `http.metrics`; off by default, on its own loopback listener (see [Metrics](#metrics)) |
| `{"event":"http_metrics_listening","address":...}` JSON line on stderr | Once at startup, with `http.metrics` |
| Request ids in responses | Yes: every response carries `X-Request-Id` (see [Request ids](#request-ids)) |
| Trace headers in responses | **No** |
| Per-request error log (which request got a 500 and why) | Which request: yes, with the access log (status and request id). Why: **no** |

### Request ids

Every response the process produces carries `X-Request-Id`. A request that
sends exactly one `X-Request-Id` of 1 to 128 characters from
`A-Z a-z 0-9 . _ -` keeps its value; any other request is given a generated
id of 32 hexadecimal characters. A value outside that set is never echoed
or forwarded. Nothing needs configuring.

- Let the proxy pass the client's `X-Request-Id` through, or set one itself
  (nginx: `proxy_set_header X-Request-Id $request_id;`), and log the
  response's `X-Request-Id` in the proxy access log. That joins the proxy's
  line and the client's report.
- With `forwarding`, the server that receives a request sends the same id
  to the owner, so both servers name the request alike.
- A response without the header came from the proxy, or is the bare `400`
  the HTTP parser sends for a request that is not valid HTTP.

### Access log

`"access_log": true` in the `http` object makes the process write one JSON
line to stderr for every request, when the request is over:

```json
{"event":"http_request","unix_ms":1794439474775,"request_id":"3f9c1d2e4b5a69788796a5b4c3d2e1f0","method":"PUT","route":"/v1/drives/{id}/fs/{path}","drive":"files","status":200,"duration_ms":12.345,"bytes_in":19,"bytes_out":44}
```

| Field | Meaning |
| --- | --- |
| `event` | Always `http_request`. |
| `unix_ms` | When the line was recorded, in milliseconds since the Unix epoch. |
| `request_id` | The response's `X-Request-Id`. |
| `method` | The request method. |
| `route` | The route's template as in the API guide's route table, for example `/v1/drives/{id}/fs/{path}` (also for the drive root) or `/v1/drives/{id}/ops/rename`. `unmatched` for a path that is no route (404), `invalid` for a path rejected before routing (400). With `remote_sessions` or `forwarding`, also `/v1/drives/{id}/sessions` and `/v1/drives/{id}/sessions/handles`; on any other server those paths are `unmatched`. **Never the path.** |
| `drive` | The drive id, only when the request named a drive of this config, whether or not its token was right. Absent otherwise: an id that is not configured is client text and is not written. With `remote_sessions` or `forwarding` the field is absent on every line for a request the server's own listener received; the owner of a drive writes it on the line for a request another server forwarded to it. |
| `status` | The response status. `null` when there was no response: the client disconnected first, or shutdown ran out of drain time. |
| `duration_ms` | From the request's arrival until its response body was sent in full or abandoned. For a download this includes the time the client took to read it. |
| `bytes_in` | Request body bytes the server read. |
| `bytes_out` | Response body bytes handed to the connection. `0` for `HEAD`; less than the file size for a download that was cut short. |

What is never written: the request path, the query string, the bearer token,
any request header, the client address, and the storage error behind a 500.
That is why the log can be shipped without redaction, and why it does not
replace the proxy's log, which has the path and the client.

It is **off by default**. The process has never written request data, the
proxy in front of it already keeps an access log, and stderr is where the
alerts above look for `mount-rs:` and `http_accept_failed` lines: an upgrade
should not start sending a line per request there unasked. Turn it on when
you want the server's own view: status and timing as the server saw them, a
line for requests the proxy never got an answer to, and the request id to
join the two.

Operational notes:

- Lines are written by one background thread from a queue of 8,192 lines. If
  stderr is not being read (a blocked pipe, a full journal), requests are
  never delayed: lines are dropped instead, and
  `{"event":"http_access_log_dropped","total":N}` reports the running count
  once writing resumes.
- A request costs a few small allocations and a queue push when the log is on,
  and one branch when it is off.
- At shutdown the lines of the last requests are written before the drives
  are closed (the process waits up to 2 seconds for stderr).
- A line is written when the response is finished, so lines are ordered by
  completion, not arrival.
- With `forwarding`, the ingress and the owner each write a line, with the
  same `request_id`.

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

The reverse proxy remains the source of paths and client addresses, and of
what clients experienced: the server cannot count a request that never
reached it. Configure the proxy's access log (status, method, path,
upstream time, bytes, `X-Request-Id`, without `Authorization`) and export
its metrics. The server's access log adds the server's own status and
timing per request, and the server's metrics add them up.

### Metrics

With `http.metrics` the process serves its own counters in the Prometheus
text format. It is off unless configured:

```json
"metrics": {"listen": "127.0.0.1:9464"}
```

- The metrics have **their own listener**. `GET /metrics` on that address
  answers; every other path there is `404`, and the API listener has no
  `/metrics` route. The reverse proxy that publishes the API therefore
  cannot publish the metrics by accident.
- **No token is asked for.** That is why `listen` must be a loopback
  address: anything else is a config error (exit 2) unless
  `"allow_non_loopback": true` is set beside it. Set that only where the
  network to the port admits the scraper and nothing else (a security group
  or NetworkPolicy), or scrape through a sidecar on loopback.
- `listen` is an IP address and a port, not a host name. An address already
  in use exits with code 1 before any drive is opened.
- When the listener is up, stderr gets
  `{"event":"http_metrics_listening","address":"127.0.0.1:9464"}`.
- The listener serves at most 8 connections at once and closes any more
  without an answer. It stops with the process, after the drives are
  released.

| Series | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `mount_rs_http_requests_total` | counter | `route`, `method`, `status` | Requests finished. `route` is the route template the access log uses (`/v1/drives/{id}/fs/{path}`, `/readyz`, `unmatched`, `invalid`; with `remote_sessions` or `forwarding` also `/v1/drives/{id}/sessions` and `/v1/drives/{id}/sessions/handles`). `method` is `GET`, `HEAD`, `PUT`, `POST`, `DELETE`, `OPTIONS`, `PATCH` or `other`. `status` is the class: `1xx` to `5xx`, or `none` for a request that ended before a response (the client went away, or a shutdown ran out of drain time). |
| `mount_rs_http_request_duration_seconds` | histogram | `route` | From a request's arrival until its response body was sent or abandoned: a download's time includes the client reading it. Buckets from 5 ms to 300 s. |
| `mount_rs_http_requests_in_flight` | gauge | | Requests that have arrived and are not finished. |
| `mount_rs_http_request_body_bytes_total` | counter | | Request body bytes read. |
| `mount_rs_http_response_body_bytes_total` | counter | | Response body bytes handed to connections. |
| `mount_rs_http_forwarded_requests_total` | counter | | Requests this server received from another server of the cluster, which forwarded them here. They are in `mount_rs_http_requests_total` as well. Always 0 on a server that owns no drive of a forwarding cluster. See [Probes, logs and metrics in a cluster](#probes-logs-and-metrics-in-a-cluster). |
| `mount_rs_http_drive_ready` | gauge | `drive` | 1 while the drive is ready as `/readyz` counts it, 0 while it is not (see [Health checks](#health-checks)). `drive` is the configured drive id. With `forwarding`, a server has a series for each drive it owns at the moment of the scrape and none for any other. |
| `mount_rs_http_access_log_dropped_lines_total` | counter | | Access-log lines dropped because stderr was not being read. Present only with `"access_log": true`. |
| `process_start_time_seconds` | gauge | | When the process started counting, as Unix time. A change means a restart, and every counter began again at zero. |
| `mount_rs_build_info` | gauge | `version` | Always 1; `version` is the version of the binary. |

What the labels can hold is fixed by the server: a route template, a method
from the list, a status class, a configured drive id, the version. **No
path, query, header, client address or token is ever in the output**, and a
client cannot add a series by inventing paths or methods.

Limits:

- Request counts and durations are **not split by drive**. Split the
  proxy's log by the drive id in the path for that.
- A request is counted when it is finished, so a long download appears in
  `requests_in_flight` first and in the counters when it ends.
- Scrapes are not counted: they do not pass through the API listener.
- Requests refused before they are read (connections over
  `max_connections`, a connection that sends no valid request) are not
  counted. `http_accept_failed` is still a stderr line only.
- With `remote_sessions` or `forwarding`, each server counts what it did
  itself, so a forwarded request is counted on two servers. What each
  counts, and how to add the servers up, is in
  [Probes, logs and metrics in a cluster](#probes-logs-and-metrics-in-a-cluster).

Example scrape config and the queries behind the alerts below:

```yaml
scrape_configs:
  - job_name: mount-rs-http
    static_configs:
      - targets: ["127.0.0.1:9464"]
```

```promql
# Share of requests answered 5xx, over 5 minutes
sum(rate(mount_rs_http_requests_total{status="5xx"}[5m]))
  / sum(rate(mount_rs_http_requests_total[5m]))

# 99th percentile of metadata requests
histogram_quantile(0.99, sum by (le) (
  rate(mount_rs_http_request_duration_seconds_bucket{route="/v1/drives/{id}/stat/{path}"}[5m])))

# A drive that is not ready
mount_rs_http_drive_ready == 0
```


### What to alert on

| Alert | Source | Meaning | First action |
| --- | --- | --- | --- |
| `/readyz` returns 503 for more than 1 minute | Load balancer or blackbox probe | A split-store drive has failed closed, or the storage of a `host` or `sqlite` drive is unreachable | Check storage. Restart for a split-store drive; a `host` or `sqlite` drive is ready again when its storage is |
| `http_drive_storage` line with `"reachable":false` on stderr | Process stderr | The root directory of a `host` drive or the database file of a `sqlite` drive cannot be opened | Check the mount or volume named by the drive's config |
| `/healthz` fails or times out | Probe | Process down or hung | Check the service manager; restart |
| Process exits non-zero, or restart loop | Service manager | Startup failure | Read stderr; see exit codes |
| 503 rate from upstream above baseline | Proxy log | Drive failed closed, contention, or draining | Check `/readyz` |
| Any 507 | Proxy log | Storage full or over quota | Free or add space |
| 500 rate above baseline | Proxy log | Storage errors | Check storage health and disk |
| 504 rate above baseline | Proxy log | Slow storage, or upload memory budget saturated | Check storage latency and upload concurrency |
| 502 from the proxy | Proxy log | Connection limit reached, process down, or keep-alive race | Check connection count against `max_connections` and the proxy keep-alive timeout |
| `http_access_log_dropped` line on stderr | Process stderr | Access-log lines were dropped because stderr was not being read fast enough | Check the log collector; requests were not affected |
| `http_accept_failed` line on stderr | Process stderr | The process could not accept connections, usually out of file descriptors (`EMFILE`/`ENFILE`). It keeps retrying with a 10 ms to 1 s back-off; clients are turned away meanwhile | Raise `LimitNOFILE`; look for leaked connections |
| `drive shutdown timed out` on stderr at stop | Process stderr | A drive did not flush and release its lease within `drive_shutdown_timeout_ms`; the lease stays held until it expires | Check the storage of the drive named in the message; expect the restart to wait for the lease; raise the bound if the storage was only slow |
| `HTTP server close timed out` on stderr at stop | Process stderr | The drain deadline passed and requests were aborted | Usually a stalled client; check that `drain_timeout_ms` exceeds `request_timeout_ms` |
| 401 rate spike | Proxy log | Rotated token not picked up, or probing | Check with the consumer |
| `mount_rs_http_drive_ready == 0` for more than 1 minute | Server metrics | The same as `/readyz` 503, but naming the drive | As for `/readyz` |
| 5xx share of `mount_rs_http_requests_total` above the objective | Server metrics | Storage errors, timeouts or a failed drive | See [Failure modes and recovery](#failure-modes-and-recovery) |
| `mount_rs_http_access_log_dropped_lines_total` rising | Server metrics | stderr is not being read fast enough; access-log lines are being lost | Fix the log collector |
| `process_start_time_seconds` changed | Server metrics | The process restarted | Expected after a deploy; otherwise find out why |
| `http_token_reload` line with `"outcome":"refused"` on stderr | Process stderr | A token file is missing, unreadable or not valid. The drive still accepts the tokens it accepted before, so a rotation that was meant to happen has not | Fix the file named in `error`; send `SIGHUP` or wait for the timer; look for `"outcome":"reloaded"` |
| 413 rate above baseline | Proxy log | Consumers hitting the upload or listing limit | Talk to the consumer; consider the limits |
| Free disk on the storage volume below threshold | Host | Leads to 507 | Add space |
| Open connections above 80% of `max_connections` | Proxy or host | Approaching refusal | Raise the limit or reduce proxy keep-alive |

## Failure modes and recovery

| Symptom | Cause | Recovery |
| --- | --- | --- |
| All requests to one drive return `503 ESTALE`; `/readyz` is 503 | The drive failed closed: its writer lease could not be renewed | Fix the storage or its connectivity. Restart the process with SIGTERM. Check `/readyz`. Data that was acknowledged is intact; clients were told to retry. |
| `/readyz` is 503 and stderr has `http_drive_storage` with `"reachable":false` | The root of a `host` drive or the database file of a `sqlite` drive is gone or not answering | Restore the mount, directory or file. No restart is needed: `/readyz` returns to 200 within a few seconds and a `"reachable":true` line follows. |
| `/readyz` is 503 for a `sqlite` drive whose file is there | Its last save failed; writes to it answered `500` or `507` | Free space or fix the volume, then `POST /v1/drives/<id>/ops/sync`: a save that succeeds makes the drive ready. Until then what was written since the failure is in memory only. |
| `503` with `EBADF` | The drive's filesystem was shut down under the server | Restart. |
| `503` with `EAGAIN`/`EBUSY` and `Retry-After: 1` | Transient contention in storage | None if brief. Sustained: check storage latency. |
| Start fails at once with code 1 and `EAGAIN: resource temporarily unavailable, acquire writer` | Another process holds the drive's writer lease, or a killed process's lease has not expired | Make sure no other instance runs. The process does not wait for the lease; let the supervisor restart it with back-off until the lease expires. See below. |
| Start fails with code 1 and a bind error | Port in use | Stop the other listener or change `port`. |
| Start fails with code 1 and a hash or integrity message | Stored block content does not match its hash | Do not serve. Restore from backup. |
| Start fails with code 1 and `missing environment variable` | Token variable not set | Fix the environment source. |
| Start fails with code 2 and `invalid HTTP drive '<id>': ... token file` | The drive's token file is missing, unreadable or does not list one or two valid tokens | Fix the file. The message names the line at fault. |
| A consumer's new token gets 401 after a rotation | The file was refused, or has not been looked at yet | Read the last `http_token_reload` line for the drive. Fix the file if it was refused; send `SIGHUP`. |
| `HTTP listener exited unexpectedly`, exit 1 | The accept task ended | Restart. Accept errors, including `EMFILE`/`ENFILE`, are retried with back-off and logged as `http_accept_failed`; they do not cause this. |
| `http_accept_failed` lines, clients time out connecting | Out of file descriptors | Raise `LimitNOFILE`; the listener recovers by itself when descriptors are free. |
| Stop ends with code 1 and `drive shutdown timed out after N ms: drive 'X' did not finish` | The drive's storage did not answer in time while flushing or releasing the lease | Fix the storage. The lease of that drive, and of any drive named as not attempted, stays held until it expires, so restarts fail with `EAGAIN ... acquire writer` until then. Acknowledged durable writes are intact. |
| Downloads cut off after `request_timeout_ms` | The client (or the proxy, if it buffers to a slow client) stopped reading | Expected: a download that makes no progress for `request_timeout_ms` is dropped to free its connection and file handle. |
| 507 on writes | Storage full | Free space or grow the volume. No restart needed. |
| Clients get connection resets or the proxy logs 502 | `max_connections` reached, or keep-alive idle race | Lower proxy keep-alive count and idle time, or raise `max_connections` and `LimitNOFILE`. |
| Uploads return 504 under load | Upload memory budget saturated or slow storage | Raise `max_buffered_upload_bytes` if memory allows; reduce upload concurrency at the proxy. |
| A directory returns `413 EOVERFLOW` | More than `max_directory_entries` entries, or listing above `max_response_bytes` | Above `max_response_bytes` only: have the consumer list in pages (`?limit=N`). Above `max_directory_entries`: pagination does not help, because every page enumerates the whole directory; raise the limit (needs restart, and memory for one full enumeration per listing request in flight) or have the consumer restructure. |
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
- **Upload memory.** Uploads up to `max_request_bytes` are buffered whole
  before publication; larger ones hold one write buffer of up to 8 MiB. The
  total is bounded by `max_buffered_upload_bytes` (default 4 x 8 MiB =
  32 MiB), not by `max_connections` x `max_request_bytes`. Peak memory is
  roughly twice the budget, because the drive copies the data while
  publishing it; plan for about 64 MiB with the defaults. Uploads beyond the
  budget wait in arrival order, and fail with 504 if the wait exceeds the
  request timeout. With the defaults, four uploads at a time are received;
  raise the budget if consumers upload many files in parallel.
- **Large uploads.** A split-store drive stores a streamed upload's blocks as
  they arrive and publishes the file once, so throughput and memory do not
  grow with the file. Measured on a SQLite split-store drive on one Mac,
  1 GiB and 2 GiB uploads ran at 190-210 MiB/s with under 65 MB peak server
  memory with 64 KiB chunks (previously 45-95 MiB/s and up to 300 MB, falling
  as the file grew), and at 275 MiB/s with 1 MiB chunks. Larger chunks are
  still faster for drives that hold mostly large files, because each block
  costs a put. Other drives write a streamed upload through a file handle to
  a temporary file. Block reconciliation keeps an in-progress upload's
  blocks; an abandoned upload's blocks are reclaimed like any other
  unreferenced block.
- **Listing memory.** A listing is built in memory, up to `max_response_bytes`
  per request, and is not covered by the upload budget. Gzip compression runs
  on at most four listings at a time; waiting requests keep their listing in
  memory. Worst case is roughly `max_connections` x `max_response_bytes`. Lower
  `max_response_bytes` or `max_directory_entries` if consumers list large
  directories concurrently. Measured on a SQLite split-store drive, listing a
  100,000-entry directory took 16 ms and produced 6.8 MB of JSON; the entries
  and their JSON are held together while the response is built, so budget
  about 15-20 MB for each such listing in flight. A paginated listing
  (`?limit=N`) sends less but does not cost less: each page enumerates the
  whole directory and keeps its entries in memory while it picks the page,
  so reading a directory of E entries in pages of N costs E/N enumerations.
  Pagination is for bounding response size, not server work.
- **Download memory.** About two chunks of `read_chunk_bytes` per active
  download.
- **Throughput.** One process, one writer per drive. Capacity is added by
  putting drives in separate processes, not by adding replicas of one drive.
- **Forwarding clusters.** Put drives on different owners to add write
  capacity, and put any number of servers behind an ordinary load balancer as
  ingresses. Each server needs the TLS proxy described above, which the load
  balancer targets, or `allow_non_loopback` on a private network so the load
  balancer can target the servers directly. Each
  server's peer UDP port (its membership `address`) must be reachable from
  every other server. The membership file may list up to 1,024 servers. Every
  ingress keeps `forwarding.peer_connections` (default 4) QUIC connections to
  each owner it forwards to, so in a full mesh of N servers each server holds
  up to 4 x N outbound and 4 x N inbound peer connections. Each connection can
  buffer up to 8 MiB of request or response body in flight, so size memory for
  the busiest owner: with 100 ingresses all streaming large files to one owner,
  that is up to 400 connections. Raise `peer_connections` on hosts with many
  cores if forwarded throughput to one owner is the limit.
- **Storage caches.** Split-store block and disk caches are sized per drive in
  the driver config. `cache_capacity` makes startup fail if their sum exceeds a
  stated budget.
- **Largest object.** `max_upload_bytes` (5 GiB by default). Raising it needs
  the proxy's body cap to follow; it does not raise memory use.

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

## Clusters

Everything above describes one process. This chapter is for running several
`serve-http` processes as one service: remote sessions, forwarding between
servers, and drive failover. What a client sees is in the API guide
([Sessions and handles](../transports/mount-rs-http/README.md#sessions-and-handles),
[Clusters](../transports/mount-rs-http/README.md#clusters)) and in
[`openapi-cluster.yaml`](../transports/mount-rs-http/openapi-cluster.yaml).
The design of failover, with its states and timing bounds, is in
[`serve-http-failover.md`](serve-http-failover.md).

### Topology

| Role | Config | What it does |
| --- | --- | --- |
| Owner | `forwarding.owner: true`, drives in `http.drives` | Opens its drives, holds their writer leases, serves them on its public listener, and accepts forwarded requests for them from the other servers on a UDP port (HTTP/3 over QUIC, mutual TLS). It also forwards requests for drives it does not own. |
| Ingress | `forwarding` with `owner` absent or `false`, `"drives": []` | Holds no drive. Forwards every drive request to the owner. |
| Failover candidate | An owner with a `forwarding.failover` object | Several candidates list the same drive; one of them owns it at a time and another takes it when that one stops. A candidate that does not own a drive forwards requests for it, like an ingress. |

```
 clients ── TLS ──▶ load balancer / proxy ──▶ any server's public listener (HTTP/1.1, cleartext)
                                                   │ drive owned here: served locally
                                                   │ drive owned elsewhere:
                                                   ▼
                                         owner's peer port (UDP, HTTP/3, mutual TLS)

 every server ──▶ Redis        (which server owns which drive; a hint, never an authority)
 owners      ──▶ storage       (metadata store and block store; the writer lease lives here)
```

- Any server may receive any request. The load balancer needs no knowledge
  of which server owns which drive. Sending a client to the owner directly
  saves the forwarding hop, and keeps that drive available to it while Redis
  is down.
- **One owner per drive at a time.** That is enforced by the drive's writer
  lease in its metadata store, not by Redis and not by configuration.
  Without `failover`, exactly one server may list a drive; a second one
  fails to start with `EAGAIN ... acquire writer`.
- All drives of a server with `forwarding.owner` or `remote_sessions` must
  be `splitstore` drives.
- Remote sessions (`remote_sessions: true`) are independent of forwarding:
  a single server may enable them. In a cluster, set it on the owners. An
  ingress forwards session requests whatever its own setting.
- TLS for clients is still the proxy's job, and everything in
  [Reverse proxy](#reverse-proxy) applies to each server's public listener.

Ports to open: each server's public `port` to the proxy; each owner's peer
`address` (UDP) to every other server; Redis and storage from the servers.

### Cluster configuration reference

Every key the parser accepts, with its default and range. An unknown key or
a value out of range is refused at startup (exit code 2), and
`mount-rs validate-config` checks all of it without opening anything.

`http.remote_sessions` and `http.sessions`:

| Key | Type | Default | Range | Meaning |
| --- | --- | --- | --- | --- |
| `remote_sessions` | boolean | `false` | | Serve the session and handle routes on every drive. Requires every drive to be `splitstore` and every drive token to be 32-256 characters from `A-Z a-z 0-9 - . _ ~ + / =`, whether it comes from a variable or from a token file. |
| `sessions` | object | all defaults | | Only valid with `remote_sessions: true`. |
| `sessions.max_sessions` | integer | `4096` | 1-65536 | Sessions the process admits across all drives. |
| `sessions.max_sessions_per_drive` | integer | `128` | 1-4096 | Sessions one drive admits. A session needs room in both. |
| `sessions.max_handles_per_session` | integer | `128` | 1-4096 | Open handles one session may hold. |
| `sessions.idle_timeout_seconds` | integer | `60` | 1-3600 | A session no request has named for this long is closed with its handles. Reported to clients as `idle_timeout_ms`. |

`http.forwarding`:

| Key | Type | Default | Range | Meaning |
| --- | --- | --- | --- | --- |
| `peers` | path | required | | The membership file. See [Membership file](#membership-file). |
| `certificate` | path | required | | This server's peer certificate (PEM). Read once at startup. |
| `private_key` | path | required | | Its private key (PEM). Read once at startup. |
| `ca` | path | required | | CA certificates (PEM) that peer certificates must chain to. Read once at startup. |
| `redis_url` | `{"env": "NAME"}` | one of the two is required | | The environment variable holding the URL of a single Redis server. A literal URL is refused. Cannot be combined with `redis_sentinel`. |
| `redis_sentinel` | object | one of the two is required | | A Redis Sentinel deployment instead of a single server. See [Redis Sentinel](#redis-sentinel). Cannot be combined with `redis_url`. |
| `redis_ca` | path | system roots | | CA certificates (PEM) trusted for a TLS Redis server, and for the sentinels, instead of the system roots. Read once at startup. |
| `allow_plaintext_redis` | boolean | `false` | | Permit an unencrypted connection to a Redis server, or a sentinel, on a host other than this one. |
| `namespace` | string | required | 1-256 characters from `A-Z a-z 0-9 - _ .` | Prefix of this cluster's Redis keys. The same on every server of a cluster; different for clusters sharing a Redis. |
| `bind` | UDP socket address | required | | Local address of the socket this server makes peer connections from. Use port `0`. (An owner *listens* on the `address` of its membership record, not here.) |
| `owner` | boolean | `false` | | Own and serve the drives in `http.drives`, and accept forwarded requests for them. Requires `splitstore` drives. Without it `drives` must be `[]`. |
| `refresh_active_routes` | boolean | `true` | | Re-read the routes of recently used drives before their one-second cache entry expires, so requests do not wait on Redis. About one Redis read per active drive per second. |
| `route_refresh_idle_ttls` | integer | `5` | 1-60 | Stop refreshing a drive's route after this many route lifetimes (15 s each) without a request for it. |
| `peer_connections` | integer | `4` | 1-16 | QUIC connections this server keeps to each owner it forwards to. |
| `membership_reload_seconds` | integer | `30` | 0-3600 | How often the membership file is checked for a change. `0`: only on `SIGHUP`. |
| `max_members` | integer | the larger of 64 and the number in the file at startup | 1-1024 | Most servers a membership reload may install. Connection limits are sized from it at startup; raising it needs a restart. |
| `failover` | object | none | | Makes this owner a failover candidate for every drive it lists. Requires `owner: true`, at least one drive, exclusive ownership (no `concurrent_writes`, no shared `ownership_mode`) and metadata storage other than `memory`. `{}` takes every default. |

`http.forwarding.failover`:

| Key | Type | Default | Range | Meaning |
| --- | --- | --- | --- | --- |
| `poll_interval_ms` | integer | `1000` | 100-10000 | How often a standby reads the drive's route and an owner checks its lease. |
| `takeover_stagger_ms` | integer | `2000` | 0-60000 | Extra wait per rank before a standby tries to take a drive nobody serves, so the candidates do not all try at once. |
| `fail_back` | boolean | `false` | | Hand a drive back to a better-ranked candidate that has returned. Costs a drain and the drive's sessions each time. |
| `fail_back_after_seconds` | integer | `60` | 10-86400 | How long that candidate must have ranked first without interruption before the hand-back. |

Two values are fixed in the binary and matter for the timings below: a
route in Redis lives **15 seconds** and is refreshed every 5; a server
caches a route for 1 second. The time a crashed owner's drive stays
unavailable is governed by the drive's own `driver.storage.lease_ttl_ms`
(default 240 s for a structured `storage` object), see
[Placement and failover](#placement-and-failover).

Restrictions on other keys when a cluster feature is on:

- With `remote_sessions`, every line of a `token.file` must be a token in
  the form above. A file with one that is not stops startup (exit 2), and
  at a reload is refused whole like any other file that is not valid.
- `drives` may be `[]` only with `forwarding`.

### Worked example: three servers with failover

[`apps/mount-rs-cli/examples/cluster/`](../apps/mount-rs-cli/examples/cluster/)
holds a complete set: `node-a.json` and `node-b.json` are failover
candidates for one drive, `shared`, on TiDB metadata and S3 blocks;
`node-c.json` is an ingress; `peers.json` is the membership file all three
use. A test runs `mount-rs validate-config` on each config and parses the
membership file with the cluster's own parser
(`the_cluster_example_configs_are_valid`). The addresses, names and pins in
them are placeholders.

`node-a.json` (`node-b.json` differs in `host`, `bind`, the certificate
paths and the lease label `owner`):

```json
{
  "version": 1,
  "http": {
    "host": "10.0.1.11",
    "allow_non_loopback": true,
    "port": 8080,
    "remote_sessions": true,
    "sessions": {
      "max_sessions": 4096,
      "max_sessions_per_drive": 128,
      "max_handles_per_session": 128,
      "idle_timeout_seconds": 60
    },
    "forwarding": {
      "peers": "peers.json",
      "certificate": "/etc/mount-rs/tls/node-a.pem",
      "private_key": "/etc/mount-rs/tls/node-a.key",
      "ca": "/etc/mount-rs/tls/cluster-ca.pem",
      "redis_url": {"env": "MOUNT_RS_CLUSTER_REDIS_URL"},
      "redis_ca": "/etc/mount-rs/tls/redis-ca.pem",
      "namespace": "files-prod",
      "bind": "10.0.1.11:7443",
      "owner": true,
      "membership_reload_seconds": 30,
      "failover": {
        "poll_interval_ms": 1000,
        "takeover_stagger_ms": 2000,
        "fail_back": false
      }
    },
    "drives": [
      {
        "id": "shared",
        "token": {"env": "MOUNT_RS_SHARED_TOKEN"},
        "driver": {
          "kind": "splitstore",
          "storage": {
            "metadata": {
              "kind": "tidb",
              "connection": {"env": "MOUNT_RS_TIDB_URL"},
              "volume_key": "files-prod-shared",
              "durable": true
            },
            "blocks": {
              "kind": "aws-s3",
              "bucket": "example-private-bucket",
              "region": "ap-southeast-2",
              "prefix": "mount-rs/files-prod/shared",
              "durable": true
            },
            "chunk_size_bytes": 65536,
            "lease_ttl_ms": 20000,
            "owner": "node-a"
          }
        }
      }
    ]
  }
}
```

`node-c.json`, the ingress:

```json
{
  "version": 1,
  "http": {
    "host": "10.0.1.13",
    "allow_non_loopback": true,
    "port": 8080,
    "forwarding": {
      "peers": "peers.json",
      "certificate": "/etc/mount-rs/tls/node-c.pem",
      "private_key": "/etc/mount-rs/tls/node-c.key",
      "ca": "/etc/mount-rs/tls/cluster-ca.pem",
      "redis_url": {"env": "MOUNT_RS_CLUSTER_REDIS_URL"},
      "redis_ca": "/etc/mount-rs/tls/redis-ca.pem",
      "namespace": "files-prod",
      "bind": "10.0.1.13:7443",
      "owner": false,
      "membership_reload_seconds": 30
    },
    "drives": []
  }
}
```

What must be identical on the two candidates, and is **not checked by the
server**: the drive `id`, the token, and the whole storage description
(`metadata` connection and `volume_key`, `blocks` bucket and `prefix`,
`chunk_size_bytes`). Two candidates that describe different storage for the
same drive id would each serve their own copy in turn. The `owner` label
inside `storage` may differ; it only labels the lease.

Start order does not matter. Each server needs Redis reachable to start.
Check each with `/readyz` (200 on all three; `drives` is 1 on the node that
took the drive and 0 on the others) and, on stderr of the node that took the
drive:

```json
{"drive":"shared","event":"drive_failover","fence":7,"state":"acquired"}
{"drive":"shared","event":"drive_failover","fence":7,"state":"published"}
```

`lease_ttl_ms: 20000` is what makes a crash recover in about 25 seconds
instead of about four minutes; see below for what it costs.

### Membership file

`forwarding.peers` names a JSON array with one record per server in the
cluster, ingresses included. Every server should have the same file. It is
the root of trust between servers: protect and distribute it as you would a
CA certificate.

```json
[
  {
    "node": "node-a",
    "boot": "2026-11-18-1",
    "address": "10.0.1.11:7443",
    "tls_server_name": "node-a.files.internal",
    "spki_sha256": "64 hexadecimal digits"
  }
]
```

| Field | Meaning |
| --- | --- |
| `node` | The server's name, unique in the file. Must equal the node in the URI SAN of its certificate, `urn:agent-tunnel:peer:<node>`. |
| `boot` | An identifier for this deployment of the server. It is published in the server's routes, and a route is trusted only if it matches the file. |
| `address` | The UDP socket address an owner listens on for forwarded requests. Not port 0. An ingress does not listen but still needs a well-formed address. |
| `tls_server_name` | The DNS name the other servers verify in this server's certificate. |
| `spki_sha256` | SHA-256 of the server's public key (DER `SubjectPublicKeyInfo`), 64 hexadecimal digits: the *pin*. |

`node`, `boot` and `tls_server_name` are 1-256 characters from
`A-Z a-z 0-9 - _ .`. Unknown fields are rejected. At most 1024 records, and
at most `max_members`. The pin of a key:

```sh
openssl pkey -in node.key -pubout -outform DER | openssl dgst -sha256 -hex
```

**Reload.** A running server reads the file again on `SIGHUP` and every
`membership_reload_seconds` if its contents changed. The new file is
validated whole first; a file that cannot be read or parsed, breaks a rule,
exceeds `max_members`, or would cut the server itself off is refused, the
old membership stays in force, and one line says why:

```text
mount-rs: membership reload refused, keeping the current 3 servers: membership record 2 has an invalid `address`
mount-rs: membership reload: now 4 servers from /etc/mount-rs/peers.json; added ["node-d"], removed [], changed []
```

Replace the file by writing a new one beside it and renaming it over the
old one. What an accepted reload does:

| Change | Effect |
| --- | --- |
| Server added | Accepted, and forwarded to, from its next connection. |
| Server removed | Refused from then on. Its open connections are closed at once; a forwarded request in flight on one fails with `502` (outcome unknown). Stop the server or move its drives first. |
| Server's pin changed | Connections made under the old key are closed at once; only the new key is accepted. |
| Unchanged records | Connections kept. |

An owner refuses a file that changes its own `boot`, `address` or
`tls_server_name`; any server refuses a file that has no record matching its
own node and key. Changing a server's own record needs a restart of that
server.

**Adding a server.** Issue its certificate, add its record, distribute the
file, `SIGHUP` every running server (or wait one interval) and check each
for the `added` line, then start the new server. **Removing a server.**
Stop it (or move its drives), remove its record, distribute, `SIGHUP`.

### Certificates and key rotation

Each server has one certificate and key for the server-to-server
connection, issued by a CA all servers trust (`forwarding.ca`). The
certificate must have:

- a URI SAN `urn:agent-tunnel:peer:<node>` with the server's `node`;
- a DNS SAN equal to its `tls_server_name`;
- extended key usage `clientAuth` and `serverAuth`, key usage
  `digitalSignature`.

A peer is accepted only if its certificate chains to `ca`, names a `node`
in the membership file, and has exactly that record's public key. Redis
never supplies an address, a name or a key.

A server reads its certificate, key and CA once, at startup. Rotation:

- **New certificate, same key** (a renewal): the pin does not change.
  Install the certificate and restart that server. Nothing else changes.
- **New key**:
  1. Issue the new certificate and key; compute the new pin.
  2. Stop the server. Without failover its drives are unavailable from
     here to step 5; with failover another candidate takes them.
  3. Install the new certificate and key; change its `spki_sha256` in the
     membership file.
  4. Distribute the file; `SIGHUP` every other server or wait one interval.
  5. Start the server.

**The limit to plan for: one record holds one pin.** There is no period in
which a server's old and new keys are both accepted, so a key rotation
always includes a restart of that server and an interval, between the other
servers reloading and that server restarting, in which it is cut off. If
the new file reaches the others while the server still runs with its old
key, they stop accepting it, and it refuses the file itself and logs that;
it recovers when restarted with the new key. Rotating the **CA** means
restarting every server, since `ca` is read at startup; put both the old
and the new CA certificate in the `ca` file for the transition.

### Redis

Redis holds one small record per drive (which server owns it) and, with
failover, one sorted set per drive (which candidates are alive). It carries
no file data and grants no ownership. Details of URLs, TLS, ACL users and
error messages are under
["The Redis connection"](../apps/mount-rs-cli/README.md#the-redis-connection)
in the CLI README. For operation:

| Requirement | Detail |
| --- | --- |
| One primary | With `redis_url` each server connects to the single address in its URL and reconnects to that same address. With `redis_sentinel` it finds the primary through the sentinels and finds the new one after a failover; see [Redis Sentinel](#redis-sentinel). **Redis Cluster is not supported.** |
| TLS | `rediss://` for any Redis not on the same host. The certificate must chain to the system roots or `redis_ca` and name the host in the URL. Verification cannot be turned off. Client certificates are not supported. |
| Plaintext | `redis://` is accepted only to loopback, unless `allow_plaintext_redis: true`. |
| Authentication | In the URL. This ACL is enough without failover, and is the one the TLS test uses: `~mount-rs:* +eval +getrange +get +set +del +pexpire +strlen +ping +client\|setinfo`. Failover also runs `TIME`, `ZADD`, `ZCARD`, `ZSCORE`, `ZRANGE`, `ZREM` and `ZREMRANGEBYSCORE` on keys under the same prefix; add `+time +zadd +zcard +zscore +zrange +zrem +zremrangebyscore`. That longer rule is read from the code and is **not** covered by a test: failover is tested with the default user. |
| Persistence | Not needed. A Redis that comes back empty is refilled by the owners within seconds. |
| Reachable at startup | A server connects, logs in and pings within 5 seconds before it reports listening. It cannot start while Redis is unreachable (exit code 1). |
| Namespace | Keys are `mount-rs:<namespace>:...`. Give each cluster its own. |

What a Redis outage does (measured with real servers, Redis cut off for 45
seconds, by `apps/mount-rs-cli/tests/redis_connection.rs`):

- An owner keeps serving requests that arrive at it for its own drives.
- Requests that need forwarding get `503` not-dispatched within about two
  seconds, for as long as the outage lasts. Nothing is queued.
- No server exits. Owners re-register once Redis answers (0.3 to 5 seconds
  in the test) and forwarding resumes.
- With failover, standbys take nothing while they cannot read Redis. If an
  owner dies during the outage, its drive stays unavailable until Redis is
  back.

High availability: use [Redis Sentinel](#redis-sentinel), which is tested.
The alternative is one stable name or address that always reaches the
current primary (a managed service's primary endpoint, or a virtual address
your tooling moves) in `redis_url`. **Not tested:** that alternative across
a failover, Redis Cluster (not supported), a proxy in front of Redis, a
`redis_url` that still resolves to a demoted read-only replica, Unix
sockets, and a certificate whose name does not match.

### Redis Sentinel

A single Redis is a single point of failure for route lookups and for
failover: while it is down, owners keep serving their own listeners, but
nothing is forwarded and no standby takes a drive. A Sentinel deployment
(one primary, one or more replicas, three or more sentinels) removes it.

```json
"forwarding": {
  "redis_sentinel": {
    "sentinels": ["10.0.2.11:26379", "10.0.2.12:26379", "10.0.2.13:26379"],
    "service": "mount-rs",
    "password": {"env": "MOUNT_RS_REDIS_PASSWORD"},
    "sentinel_password": {"env": "MOUNT_RS_SENTINEL_PASSWORD"}
  },
  "redis_ca": "/etc/mount-rs/tls/redis-ca.pem"
}
```

| `redis_sentinel` key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `sentinels` | array of `"host:port"` | required | 1 to 16 sentinels, each listed once. List all of them: a server asks them in turn and needs one that answers. An IPv6 address is written `"[2001:db8::1]:26379"`. The list is read at startup. |
| `service` | string | required | The name the sentinels monitor the primary under (`sentinel monitor <service> ...`). Letters, digits, `.`, `-`, `_`. |
| `tls` | boolean | `true` | TLS to the sentinels **and** to the Redis servers. There is one setting for both. |
| `username`, `password` | string, `{"env": "NAME"}` | none | Credentials for the Redis servers (`requirepass`, or an ACL user). `username` requires `password`. |
| `sentinel_username`, `sentinel_password` | string, `{"env": "NAME"}` | none | Credentials for the sentinels, which have their own (`requirepass` in the sentinel configuration). Not taken from `password`: set both if they are the same. |
| `database` | integer | `0` | The database on the Redis servers. |

Rules, checked at startup (exit status 2 for a configuration that breaks
one):

- `redis_url` and `redis_sentinel` cannot both be set, and one must be.
- With `tls: false`, every sentinel must be on this host (`localhost` or a
  loopback address) unless `allow_plaintext_redis` is `true`. The same rule
  is applied to the address a sentinel names for the primary, when it is
  named: a plaintext primary on another host is refused.
- `redis_ca` requires `tls` to be on. It is the only trust root for the
  sentinels and the Redis servers alike.
- A password is named by its environment variable and never written.

Deploying:

1. Run Redis as one primary and at least one replica, and at least three
   sentinels on separate hosts, as the Redis documentation describes. No
   persistence is needed.
2. **The address the sentinels report must be reachable from every
   mount-rs server**, and with TLS the Redis certificates must be valid for
   it. Sentinels report IP addresses unless `sentinel resolve-hostnames`
   and `sentinel announce-hostnames` are on; behind NAT or in containers
   set `replica-announce-ip`/`replica-announce-port` on the servers and
   `sentinel announce-ip`/`announce-port` on the sentinels. Client
   certificates are not supported (`tls-auth-clients no`).
3. Give every mount-rs server the same `redis_sentinel` and `namespace`.
4. Keep the Redis hosts' clocks in step with each other (NTP). Route and
   announcement lifetimes are measured on the primary's clock.
5. Optional, from Redis itself and not tested here:
   `min-replicas-to-write 1` on the Redis servers makes a primary that has
   lost its replicas refuse writes, which shortens the time a cut-off old
   primary accepts publications nobody will read. It needs two replicas to
   survive the loss of one server.

An ACL user for the Redis servers needs `+role` in addition to the rule
under Redis above, and one for the sentinels needs
`+sentinel|get-master-addr-by-name +client|setinfo`. Both are read from the
code and **not** covered by a test: the tests use the default user with a
password.

At startup a server asks the sentinels for the primary, connects and logs
in to it, and requires it to report the `master` role, before it reports
listening. It cannot start while no sentinel answers, while the service
name is unknown to them, or while no primary is reachable (exit status 1;
the message starts with `forwarding Redis Sentinel` and never contains a
password).

**What a failover of the Redis primary does** (measured with real servers
by `apps/mount-rs-cli/tests/redis_sentinel.rs`; the figures and the
reasoning are under
["Redis Sentinel"](serve-http-failover.md#redis-sentinel) in the failover
design):

- No server exits and nothing needs restarting. Owners keep serving
  requests that arrive at them for their own drives throughout.
- From the loss of the primary until the sentinels have promoted a replica
  and the servers have found it, requests that need forwarding get `503`
  not-dispatched. In the test that was 3 to 7 seconds with
  `down-after-milliseconds 2000`; a client repeating one forwarded read
  went at most 4 seconds without an answer. Your `down-after-milliseconds`
  sets most of it.
- Servers find the new primary by themselves: on the first failed command,
  or within about a second for a server that only reads. They never use a
  replica.
- **The promoted replica may be missing the last publications**, because
  Redis replicates asynchronously. A missing route is published again
  within one refresh interval (5 seconds; 0.6 to 3 seconds in the test).
  An older route that the promoted replica still held is replaced within
  the same interval; requests forwarded to the server it named in between
  are refused or fail, and are never served by a server that does not hold
  the drive's lease.
- With `failover`, no drive changes owner because of a Redis failover: the
  lease is in storage. A standby may log `lease_held` once if it read "no
  route" in the gap.
- The old primary, when it returns, is made a replica by the sentinels and
  loses whatever it accepted while cut off. Servers do not go back to it.

**Limits.** Redis Cluster is not supported. The sentinel list is fixed at
startup: after replacing sentinels, update the configuration and restart
the servers one at a time. Not tested: a network partition inside the
Redis deployment (as opposed to a killed primary), Redis hosts with
different clocks, an ACL user, sentinels reporting host names, and a
managed service's Sentinel-compatible endpoint.

### Placement and failover

**Without `failover`** placement is the configuration: a drive is served by
the one owner that lists it, from startup until that process stops. If the
process stops, the drive is unavailable (`503` not-dispatched through the
other servers) until it is started again; after a crash the restart also
waits for the lease ([Lease takeover wait](#lease-takeover-wait)). An owner
that loses a drive's lease exits with code 1.

**With `failover`** on two or more owners that list the same drive:

- Each candidate announces itself in Redis. Candidates are ranked per drive
  by a fixed hash of drive id and node name; the best-ranked live candidate
  tries first, the others `takeover_stagger_ms` later per rank. Rank only
  orders the attempts. Who owns the drive is decided by who acquires the
  writer lease in storage.
- A candidate opens a drive only after reading that no route is published
  for it. A candidate that does not own a drive does not open it.
- A server with `failover` does not exit when a lease is held elsewhere or
  lost: it becomes, or stays, a standby for that drive.

Time until the drive is served again (`L` = `lease_ttl_ms` of the drive,
`P` = `poll_interval_ms`, `S` = `takeover_stagger_ms`, `k` = rank of the
taking node, 0 for the first):

| Event | Bound | With the defaults | With `lease_ttl_ms: 20000` |
| --- | --- | --- | --- |
| Owner crashes (SIGKILL, host or power loss) | `max(15 s + k x S, L) + 5P +` open | about 245 s | about 25 s; measured 20.9 to 21.3 s |
| Owner stops cleanly (SIGTERM, deploy) | `P + k x S + 5P +` open | 1 to 2 s | the same; measured 0.1 to 1.0 s |

Clients see `503` not-dispatched in that time, and `502` outcome-unknown
for requests that were in flight on the old owner or were forwarded to it
while its route was still published (up to 15 seconds after a crash). An
ingress follows the new owner within about a second of its route appearing.
Every session and open handle on the old owner is lost.

Choosing `lease_ttl_ms`: it bounds crash recovery from below, and it is
also how long a live owner may fail to reach its metadata store before it
must stop serving. Too short, and a slow metadata store makes healthy owners
give up their drives. 20 seconds is what the tests run with.

`fail_back` is off by default because every move costs a drain and the
drive's sessions. Without it, after a rolling restart the drives stay on
whichever candidates took them.

Tested with real processes, each behind its own Redis proxy
(`apps/mount-rs-cli/tests/failover.rs`): an owner cut off from Redis keeps
its drive and the standby takes nothing; an owner killed during a Redis
outage is replaced once Redis is back; fail-back; writes across a killed
owner are complete or absent and never lost once acknowledged; an owner
paused past its lease gives the drive up when it resumes; and a release
that storage does not answer is abandoned so the lease expires.

Each state change is a JSON line on stderr with `"event":"drive_failover"`;
the states are listed in
[`serve-http-failover.md`](serve-http-failover.md#startup-shutdown-and-logs).
Alert on `open_failed` and `release_failed`, and on `lease_held` or
`route_unpublished` that lasts.

### Sessions in operation

- A session and its handles exist only in the memory of the owner. They do
  not survive a restart of the owner or a move of the drive. Clients get
  `403 EACCES` and must create a new session; `mount-rs mount` with a
  remote drive does **not** do this by itself (see the CLI README).
- Each owner applies the `sessions` limits of its own config. A drive that
  moves to a candidate with different limits gets those.
- Every session request carries the drive's bearer token and is checked
  against the tokens the drive accepts when the request arrives. A session
  belongs to the token it was opened with and does not survive that
  token: see [Token rotation in a cluster](#token-rotation-in-a-cluster).
- A session and each open handle hold memory and storage state on the
  owner. Raise the limits with the process's memory in mind.
- Shutdown closes the listener, drains for up to `drain_timeout_ms`, closes
  every session and handle, withdraws the routes and releases the leases.
  That still happens when the drain runs out of time; the exit code is
  then 1.

### Probes, logs and metrics in a cluster

- `/healthz` and `/readyz` are answered by the server that receives them,
  never forwarded. `/readyz` counts only the drives that server owns at that
  moment: an ingress, and a standby, are ready with `"drives": 0`. A drive
  the server owns that has failed makes it not ready. **Readiness does not
  check Redis, the other servers, or that any owner exists for any drive.**
  To monitor a drive end to end, request something on it through the
  cluster (for example `GET /v1/drives/<id>/statfs`) and alert on anything
  but `200`.
- `GET /v1/drives` lists only drives the answering server owns.
- With `access_log`, the `drive` field is absent on the lines a server
  with `remote_sessions` or `forwarding` writes for requests its own
  listener received. The owner's line for a forwarded request has it.
- With `http.metrics`, **each server counts the requests it handled, each
  once**:

  | Server | Counts in `mount_rs_http_requests_total` | `mount_rs_http_forwarded_requests_total` | `mount_rs_http_drive_ready` |
  | --- | --- | --- | --- |
  | Single server with `remote_sessions` | Every request its listener received, file routes and session routes | 0 | Every configured drive |
  | Ingress (`forwarding`, owns nothing) | Every request its listener received, with the status it gave the client: the owner's answer, or its own `400`, `502` or `503` when it did not or could not forward | 0 | No series |
  | Owner (`forwarding.owner`) | Every request its listener received, and every request another server forwarded to it and it dispatched to the drive | The forwarded ones only | The drives it owns at the scrape. With `failover`, a standby has none |

  So a request that is forwarded is counted **twice in the cluster**: once
  by the server the client reached and once by the owner. The same holds
  for the duration histogram, the byte counters and
  `mount_rs_http_requests_in_flight`. To count each client request once
  over the whole cluster, subtract the forwarded ones:

  ```promql
  sum(rate(mount_rs_http_requests_total[5m]))
    - sum(rate(mount_rs_http_forwarded_requests_total[5m]))
  ```

  `mount_rs_http_forwarded_requests_total` has no labels, so this works for
  the total only; per route or status, use the ingress servers' series for
  what clients were answered and the owners' for what the drives did.
- The owner's duration for a forwarded request starts when the request
  reaches it and leaves out the hop from the ingress. The ingress's
  duration is what the client waited.
- A forwarded request that the owner refuses before it reaches the drive
  (it no longer owns the drive, or the route is stale: the ingress turns
  these into `503`) is counted by the ingress and not by the owner.
- The session routes have their own route templates,
  `/v1/drives/{id}/sessions` and `/v1/drives/{id}/sessions/handles`, on
  every server with `remote_sessions` or `forwarding`.
- A drive nobody owns has no `mount_rs_http_drive_ready` series anywhere.
  Alert on the end-to-end probe above, or on
  `absent(mount_rs_http_drive_ready{drive="<id>"})` over all candidates.

### Token rotation in a cluster

A drive's token is checked by **the server that owns the drive**, against
the tokens that server accepts at that moment. An ingress has no drive and
no token: it forwards the request with its `Authorization` header and the
owner decides. So a token file belongs on every server that lists the
drive, which with `failover` is every candidate, and
[Rotation](#rotation) is done on each of them:

1. Put the new token first and the old one second in the file **on every
   server that lists the drive**.
2. Wait for each of those servers to write
   `{"event":"http_token_reload",...,"outcome":"reloaded","tokens":2}`.
   A standby reloads its file like an owner, so that it serves the drive
   with the current tokens the moment it takes it over.
3. Move the consumers to the new token.
4. Remove the old token's line on every server, and wait for
   `"tokens":1` from each.

What to know:

- **Do not move consumers before every candidate has reloaded.** Until
  then the new token works or gets `401` depending on which candidate owns
  the drive, and a failover in that window changes the answer.
- A forwarded request is checked when it reaches the owner. There is no
  second check at the ingress and no window in which the ingress and the
  owner disagree.
- With `remote_sessions` every token of the file must be 32-256 characters
  from `A-Z a-z 0-9 - . _ ~ + / =`. A file with a token that is not is
  refused whole, so the file routes and the session routes always accept
  the same tokens.
- **Open sessions belong to the token that opened them.** During the
  overlap a session opened with the old token keeps working with the old
  token; the new token does not reach it (`403 EACCES`, as for a session
  that does not exist) and opens sessions of its own. From the reload that
  drops the old token, every request with it gets `401`, session and
  handle requests included, exactly as a token that was never valid.
  Nothing can then reach the sessions it opened: the server closes each,
  with its handles, once it has been idle for
  `sessions.idle_timeout_seconds` (60 by default), and until then it still
  counts towards `max_sessions_per_drive` and `max_sessions`. A rotation
  closes no session of a token that stays.
- A client moves to the new token by opening a new session with it.
  `mount-rs mount` with a remote drive reads its token once: remount it
  with the new token during the overlap. A mount left on a dropped token
  fails every operation with `EACCES`.
- To revoke at once, write a file with only the new token on every server
  that lists the drive and send each `SIGHUP`.
- A drive whose token is an environment variable still rotates by
  restarting every server that lists it, with no overlap.

### Upgrading a cluster

The server-to-server protocol has a version (currently 1), named on every
forwarded request and in each drive's route record. A server refuses a peer
whose version it does not speak instead of misreading it: the client gets
`503` with `x-mount-dispatch: not-dispatched`, and the server writes a
`peer_protocol_refused` line on stderr. See
[`serve-http-upgrade.md`](serve-http-upgrade.md#clusters) for the rule, the
rolling upgrade procedure and the full-stop procedure.

What is tested is one pair of builds: the current commit in a cluster with
`main` at `031050cc`, in both arrangements (CI job `cluster-mixed-release`:
forwarding, sessions, membership reload and failover hand-off between the
two). No pair of published releases has been run together. The policy is
that two adjacent releases may share a cluster for the length of an
upgrade, one server at a time; any other pair is upgraded by stopping every
server and starting every server on the new release.

With failover, a rolling restart *of one release* (for a config change or
a host reboot) is supported and is what the clean-stop row above measures:
stop one server with SIGTERM, wait for its drives to be `published` on
another, start it again, move on. A rolling upgrade is the same with the
binary replaced in between.

### Cluster failure modes

| Symptom | Cause | Recovery |
| --- | --- | --- |
| `503` with `x-mount-dispatch: not-dispatched` for one drive | No route for it: its owner is down, is starting, or cannot reach Redis | Find the owner (`drive_failover` lines, or which server lists the drive). Start it, or fix its Redis connection. With failover, wait for the bound above and look for `lease_held` / `open_failed` on the standbys. |
| The same for every drive through one server | That server cannot reach Redis | Fix the connection; no restart needed. Meanwhile route clients to the owners. |
| The same for a drive id nobody configured | Expected: a cluster cannot tell an unknown drive from one that is down | Fix the client. |
| `502` with `x-mount-dispatch: unknown` in bursts | An owner crashed or was removed from the membership with requests in flight; or its route outlived it (up to 15 s) | None for the server. Clients re-read or repeat with preconditions. |
| `502` on every request through one new server | The owners have not loaded a membership file that lists it | Distribute the file and `SIGHUP` them. |
| Start fails, code 1, `forwarding Redis Sentinel: no primary for service ...` | No sentinel answered (address, CA, `sentinel_password`), the service name is not the one the sentinels monitor, or the primary they name is unreachable or refuses the login (`password`, an address not reachable from this server) | The end of the message says which. A deployment in the middle of a failover has no primary for a few seconds; start again. |
| Start fails, code 1, naming Redis (`rediss://host:port`) | Redis unreachable, wrong CA, wrong or missing password | Fix as the message says. The URL's credentials are never printed. |
| Start fails, code 2, `config.http.forwarding...` | A key out of range, `failover` without `owner`, `memory` metadata with failover, a non-`splitstore` drive | Fix the config; `validate-config` reports the same. |
| `membership reload refused` on stderr | The new file is invalid or would cut this server off | Fix the file. The old membership is still in force. |
| An owner without failover exits with code 1 while running | It lost a drive's lease, or another server registered the drive | Find out why two servers claim the drive before restarting. |
| `503` with `x-mount-dispatch: not-dispatched` for a drive whose owner is up, and `peer_protocol_refused` on stderr of the forwarding server or the owner | The two servers run releases that share no version of the server-to-server protocol | Finish the upgrade, or roll the newer server back. The line names the peer, what it accepts or offered, and what this server speaks. Nothing was run. |
| `drive_failover` `lease_held` that never ends | The previous owner is alive and still holds the lease but cannot publish its route (cut off from Redis) | Fix that owner's Redis connection, or stop it so the lease is released. |

### Known limits

Stated plainly. None of these is hidden behind a feature flag.

1. **Nothing checks that failover candidates describe the same storage.**
   Two candidates whose configs give the same drive id different metadata
   or block storage will each serve their own copy in turn, silently. Keep
   the drive definitions in one place and generate the configs from it.
   (`memory` metadata is refused with failover for this reason; other kinds
   cannot be compared by the server.)
2. **A token file is per server.** Rotation without a restart needs
   `token.file` and the same change made on every server that lists the
   drive; nothing distributes the file or checks that the candidates
   agree. Sessions do not move to the new token: clients open new ones
   (see [Token rotation in a cluster](#token-rotation-in-a-cluster)).
3. **One pin per server.** A key rotation needs that server restarted and
   leaves it cut off between the others' reload and its restart. A CA
   rotation needs every server restarted.
4. **Mixed-release clusters are tested for one pair of builds only**: the
   current commit with `main` at `031050cc`. A rolling upgrade between
   published releases has not been run; rehearse it, or stop every server.
   A peer of an incompatible protocol version is refused with `503`, not
   misread.
5. **Redis high availability means Redis Sentinel.** A failover of the
   primary is tested with a killed primary and three sentinels; a
   partition inside the Redis deployment is not. Redis Cluster is not
   supported. During a Redis failover forwarded requests get `503` for as
   long as the sentinels take, and for up to 5 seconds after it a route can
   be missing or name the previous owner; see
   [Redis Sentinel](#redis-sentinel).
6. **The route lifetime is fixed at 15 seconds.** For that long after an
   owner crashes, forwarded requests for its drives wait on the dead owner
   (up to 30 seconds each) and then get `502`. Clients need their own
   timeouts.
7. **Crash recovery cannot be faster than the drive's lease.** About 245
   seconds with the default `lease_ttl_ms`.
8. **Sessions and handles do not move.** They are lost when their owner
   stops or the drive moves, and requests in flight are not replayed.
9. **Readiness is local.** `/readyz` is 200 on a server that cannot reach
   Redis or any owner.
10. **An unknown drive is `503`, not `401`,** on a forwarding server, and
    the route lookup happens before the token is checked: a caller without
    a valid token can learn whether a drive id currently has an owner.
11. **An upload being forwarded when the ingress shuts down is not cut
    short.** It runs until it finishes or the drain timeout aborts it. (An
    upload to a drive the server owns is refused at once, as on a single
    server.)
12. **A forwarded response never closes the client's connection** where a
    single server would send `Connection: close` (a `413` for an oversized
    listing).
13. **The session routes answer `500` for most filesystem errors**
    (`EEXIST`, `ENOTEMPTY`, `EISDIR`, ...), where the file routes use
    `409`, `400` and so on. Clients branch on `code`.
14. **Metrics are per server and not split by drive.** A forwarded request
    is counted by the ingress and by the owner, and the access log's
    `drive` field is absent on a listener's own lines (see above).
15. **Clocks.** Candidates and the metadata store must agree to within a
    second or two (NTP is enough). Beyond the 5-second renewal margin a
    deposed owner can answer *reads* from its last committed state after a
    successor has taken the drive. Writes are refused throughout. See
    "Clocks" in [`serve-http-failover.md`](serve-http-failover.md#clocks).
16. **A candidate removed from the membership file while it runs keeps its
    drives**, which are then unreachable through the other servers. Stop a
    server before removing it.
17. **With `fail_back`, a drive can be handed to a node that has just
    died**, and is then unavailable for about 15 seconds.
18. **Not exercised by any test:** a network partition of the
    server-to-server (peer) link, as opposed to a server cut off from
    Redis, which is tested; failover with a restricted Redis ACL user;
    more than a handful of servers; and `http2_prior_knowledge` together with cluster
    features. The rest of what failover leaves out is listed under "Not
    handled" in [`serve-http-failover.md`](serve-http-failover.md#not-handled).

## Pre-launch checklist

Run the end-to-end probe against the build you will deploy. It starts the
binary on a scratch split-store drive and checks auth, path handling, the
upload and listing limits, parallel reads and writes, SIGTERM, restart and
the single-writer lease. CI runs it on every change.

```sh
python3 scripts/serve-http-e2e.py ./mount-rs "$(mktemp -d)"
```

Configuration

- [ ] `host` is loopback, or `allow_non_loopback` is set and only the load
  balancer or proxy can reach the port; `port` is fixed (not `0`).
- [ ] `http2_prior_knowledge` is `false`.
- [ ] `remote_sessions` and `forwarding` are absent, or the
      [Clusters](#clusters) chapter has been followed and its
      [Known limits](#known-limits) accepted.
- [ ] No `memory` drive.
- [ ] `drain_timeout_ms` > `request_timeout_ms`.
- [ ] `drive_shutdown_timeout_ms` covers the slowest flush the drives need.
- [ ] Twice `max_buffered_upload_bytes` fits the memory limit of the process.
- [ ] No drive sets `writeback`, unless its consumers accept non-durable `200`s.
- [ ] `mount-rs validate-config --config <path>` passes.
- [ ] Storage paths are on local, durable disk with monitored free space.

Secrets

- [ ] Each drive has its own random token, delivered through the secret
      mechanism, not the command line or the config file.
- [ ] Drives that must rotate without a restart take their token from a
      file (`token.file`) that only the service user can read. In a
      cluster, every server that lists the drive has the file.
- [ ] Consumers know how a rotation reaches them: with a token file, a
      period in which both tokens work and then `401` for the old one; with
      an environment variable, a restart after which the old token stops
      working at once.

Proxy

- [ ] TLS terminates at the proxy; the proxy shares the server's network
      namespace.
- [ ] Body cap equals `max_upload_bytes`; request and response buffering off.
- [ ] Upstream timeouts above `request_timeout_ms`; upstream keep-alive idle
      below it; upstream pool below `max_connections`.
- [ ] No automatic upstream retries.
- [ ] Request URI forwarded unmodified (`a%2Fb` test returns the server's 400).
- [ ] Access log on, `Authorization` not logged, `X-Request-Id` logged;
      `/healthz` and `/readyz` not public.

Process

- [ ] Exactly one instance per drive, or every instance that lists the
      drive is a failover candidate with identical storage for it.
- [ ] Stop signal SIGTERM; stop timeout above `drain_timeout_ms` + 5 s +
      `drive_shutdown_timeout_ms`, with SIGKILL as the fallback.
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
