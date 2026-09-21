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
external dependency is healthy. Use the provider-specific checks and
application-owned telemetry for those gates.

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
reach request handling.

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
cargo test -p mount-rs-http --locked
cargo clippy -p mount-rs-http --locked --all-targets -- -D warnings
```
