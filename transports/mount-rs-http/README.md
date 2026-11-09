# mount-rs-http

`mount-rs-http` is a separately packaged HTTP/1.1 transport for named mount-rs
drives. Each drive keeps its own stable ID, bearer token, and
`Arc<dyn FsDriver>`. The transport does not mount a filesystem or provide a
second filesystem implementation.

## Routes

Unauthenticated `GET`/`HEAD /healthz` reports that the listener is alive.
Unauthenticated `GET`/`HEAD /readyz` reports `ready` and the configured drive
count when at least one drive is registered, and returns `503` with
`not_ready` for an empty registry. These are process/configuration probes only;
they do not claim that a remote provider, object store, TLS endpoint, or
external dependency is healthy. Responses are marked `Cache-Control: no-store`
and `X-Content-Type-Options: nosniff` for deployment-safe probing. Use the
provider-specific checks and application-owned telemetry for those gates.

All routes are versioned under `/v1`:

- `GET /v1/drives` requires a bearer token and lists only the drive IDs whose
  configured token matches; missing/invalid credentials return `401` and never
  reveal the complete registry.
- `GET`/`HEAD`/`PUT`/`DELETE /v1/drives/:id/fs/:path` provide metadata,
  bounded streaming reads, full replacement writes, and non-recursive removal.
- `GET`/`HEAD /v1/drives/:id/entries/:path` lists directory entries.
- `POST /v1/drives/:id/ops/mkdir`, `rename`, `truncate`, and `sync` expose the
  corresponding filesystem operations with bounded JSON bodies.

File reads support one `bytes=start-end` range and return `416` for invalid or
multi-range requests. Uploads and JSON requests are bounded by
`HttpServerOptions::max_request_bytes`; streamed reads use a bounded channel and
`read_chunk_bytes`. The listener also enforces `max_connections`, applies a
bounded header and request timeout, and closes excess connections before they
reach request handling. Directory listings are bounded by
`max_directory_entries` and `max_response_bytes`; oversized listings fail
closed before a response is sent.

Uploads coalesce transport fragments into at most 64 KiB
per filesystem write, flushing the remainder at end of body. This bounds added
buffering and avoids a durable write for each small network fragment. An
incomplete buffer is not visible to readers until it is flushed.

Accepted TCP connections enable `TCP_NODELAY` by default to avoid packet
coalescing delays on small streaming writes. Set `HttpServerOptions::tcp_nodelay`
to `false` to restore coalescing. This controls the public TCP listener, not
server-to-server HTTP/3.

`PUT` is a bounded streaming write, not an atomic publish. The current driver
contract has no portable temporary-file/atomic-publish operation, so a request
body that exceeds the limit or disconnects after writing can leave a partial
file; those paths return an error and never claim a successful write. A future
staging integration may add atomic publication where the backend can prove it.

Every drive route requires the exact `Authorization: Bearer <token>` value for
that drive. Unknown drives fail closed, traversal and encoded separators are
rejected before driver normalization, and tokens are private/redacted. The
listener accepts only loopback hosts (`127.0.0.1`, `::1`, or `localhost`) and
does not provide TLS. Customers exposing the service remotely must put it
behind a TLS/authenticated boundary and keep the mount-rs listener loopback
bound; non-loopback HTTP binds are rejected before socket creation.

Distributed caching, cache invalidation, and cross-process version coordination
are intentionally outside this primary slice.

## Local verification

This crate is a root workspace member and uses the shared lockfile:

```text
./scripts/cargo-shared test -p mount-rs-http --locked
./scripts/cargo-shared clippy -p mount-rs-http --locked --all-targets -- -D warnings
```

### Typed metadata reads

Authenticated `GET` or `HEAD /v1/drives/{id}/{operation}/{path}` supports:

| Operation | GET JSON result |
| --- | --- |
| `stat` | Core `Stats`, following symlinks |
| `lstat` | Core `Stats`, without following the final symlink |
| `statfs` | Core `StatsFs` |
| `readlink` | JSON string containing the symlink target |

These use the same drive authorization and path validation as file routes.
HEAD executes the metadata operation and omits the response body. Other methods
return 405. Filesystem errors retain the existing JSON error envelope. These
routes do not create persistent remote file handles or provide a mounted client.

Directory-entry responses (`GET /v1/drives/{drive}/entries/...`) support
negotiated gzip. Clients without an `Accept-Encoding` preference retain plain
JSON. Responses include `Vary: Accept-Encoding`; compressed responses advertise
their encoded Content-Length, including for HEAD. Quality values and explicit
identity/gzip exclusions are respected; rejecting both supported representations
returns 406. Listings below 1 KiB use identity when permitted. Compression uses
level 1 on blocking workers with four concurrent encoders per process; waiting
requests and their already-serialized listings are not memory admission bounded.
File contents and directory JSON returned by the `fs` route are unchanged.

File-response streaming uses one bounded ordered channel for data, errors and
EOF. This avoids the final-frame race in the dependency's separate data/error
channels. Shutdown can cancel an error send when the queue is full, and dropping
the receiver releases blocked producers. The channel does not snapshot a file
against mutations of the same inode; the driver retains that responsibility.
