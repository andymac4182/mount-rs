# mount-rs-http

`mount-rs-http` serves named mount-rs drives over HTTP. A drive is a
filesystem with a stable id and its own bearer token. The server is started
with `mount-rs serve-http --config <path>`.

This file is the guide for services that call the API.

- Machine-readable contract: [`openapi.yaml`](openapi.yaml) (OpenAPI 3.1).
- Running the server: [`docs/serve-http-runbook.md`](../../docs/serve-http-runbook.md).
- Configuration file and local demo: [`apps/mount-rs-cli/README.md`](../../apps/mount-rs-cli/README.md#unmounted-http-service).

The v1 launch contract covers plain `serve-http` with `remote_sessions: false`
and no forwarding.

## Quick start

The server listens on a loopback address. In production you reach it through
the operator's TLS reverse proxy. The examples use two shell variables:

```sh
BASE=http://127.0.0.1:8080        # or https://files.example.com
TOKEN=demo-memory                 # the token of the drive named "memory"
```

Every example below is accepted by the server as written, in this order, on an
empty drive named `memory`.

Probes (no token):

```sh
curl -i "$BASE/healthz"           # 200 {"status":"ok"}
curl -i "$BASE/readyz"            # 200 {"status":"ready","drives":2}
```

Which drives does this token open?

```sh
curl -H "Authorization: Bearer $TOKEN" "$BASE/v1/drives"
# [{"id":"memory"}]
```

Create a directory, including missing parents:

```sh
curl -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"path":"/reports/2026","recursive":true}' \
  "$BASE/v1/drives/memory/ops/mkdir"
# {"path":"/reports/2026","created":"/reports"}
```

Write a file (create or replace):

```sh
curl -H "Authorization: Bearer $TOKEN" -X PUT --data-binary 'hello from mount-rs' \
  "$BASE/v1/drives/memory/fs/reports/2026/hello.txt"
# {"path":"/reports/2026/hello.txt","bytes":19}
```

To upload a local file use `--data-binary @local-file` or `-T local-file`.

Read it, read a byte range, and read only the headers:

```sh
curl -H "Authorization: Bearer $TOKEN" \
  "$BASE/v1/drives/memory/fs/reports/2026/hello.txt"
curl -H "Authorization: Bearer $TOKEN" -H 'Range: bytes=0-4' \
  "$BASE/v1/drives/memory/fs/reports/2026/hello.txt"      # 206, "hello"
curl -I -H "Authorization: Bearer $TOKEN" \
  "$BASE/v1/drives/memory/fs/reports/2026/hello.txt"      # Content-Length: 19
```

List a directory (optionally gzip-compressed), and list the root:

```sh
curl --compressed -H "Authorization: Bearer $TOKEN" \
  "$BASE/v1/drives/memory/entries/reports/2026"
# [{"name":"hello.txt","parent_path":"/reports/2026","file_type":"File"}]
curl -H "Authorization: Bearer $TOKEN" "$BASE/v1/drives/memory/entries"
```

`GET` on the `fs` route of a directory returns JSON metadata, not bytes:

```sh
curl -H "Authorization: Bearer $TOKEN" "$BASE/v1/drives/memory/fs/reports/2026"
# {"path":"/reports/2026","stats":{...},"entries":[...]}
```

Metadata without reading content (`stat` follows symlinks, `lstat` does not):

```sh
curl -H "Authorization: Bearer $TOKEN" \
  "$BASE/v1/drives/memory/stat/reports/2026/hello.txt"
# {"dev":0,"ino":4,"mode":33206,"nlink":1,"uid":501,"gid":20,"rdev":0,"size":19,...}
curl -H "Authorization: Bearer $TOKEN" \
  "$BASE/v1/drives/memory/lstat/reports/2026/hello.txt"
curl -H "Authorization: Bearer $TOKEN" "$BASE/v1/drives/memory/statfs"
# {"filesystem_type":16914836,"block_size":4096,"blocks":1048576,...}
curl -i -H "Authorization: Bearer $TOKEN" \
  "$BASE/v1/drives/memory/readlink/reports/2026/hello.txt"
# 400 {"error":{"code":"EINVAL","message":"invalid argument"}}: not a symbolic link
```

Rename, truncate, flush:

```sh
curl -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"from":"/reports/2026/hello.txt","to":"/reports/2026/greeting.txt"}' \
  "$BASE/v1/drives/memory/ops/rename"                     # {"ok":true}
curl -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"path":"/reports/2026/greeting.txt","length":5}' \
  "$BASE/v1/drives/memory/ops/truncate"                   # {"ok":true}
curl -i -H "Authorization: Bearer $TOKEN" -X POST \
  "$BASE/v1/drives/memory/ops/sync"                       # 204
```

Delete a file, then the now-empty directory:

```sh
curl -i -H "Authorization: Bearer $TOKEN" -X DELETE \
  "$BASE/v1/drives/memory/fs/reports/2026/greeting.txt"   # 204
curl -i -H "Authorization: Bearer $TOKEN" -X DELETE \
  "$BASE/v1/drives/memory/fs/reports/2026"                # 204
```

## Routes

| Method | Path | Purpose | Success |
| --- | --- | --- | --- |
| `GET`, `HEAD` | `/healthz` | Liveness. No token. | 200 `{"status":"ok"}` |
| `GET`, `HEAD` | `/readyz` | Readiness. No token. | 200 `{"status":"ready","drives":N}` |
| `GET`, `HEAD` | `/v1/drives` | Drive ids the token opens | 200 `[{"id":"..."}]` |
| `GET`, `HEAD` | `/v1/drives/{id}/fs/{path}` | File bytes; JSON metadata for a directory | 200, 206 |
| `PUT` | `/v1/drives/{id}/fs/{path}` | Create or replace a file | 200 `{"path":"...","bytes":N}` |
| `DELETE` | `/v1/drives/{id}/fs/{path}` | Remove a file, symlink or empty directory | 204 |
| `GET`, `HEAD` | `/v1/drives/{id}/entries/{path}` | List a directory | 200 `[{"name","parent_path","file_type"}]` |
| `GET`, `HEAD` | `/v1/drives/{id}/stat/{path}` | Metadata, following symlinks | 200 `Stats` object |
| `GET`, `HEAD` | `/v1/drives/{id}/lstat/{path}` | Metadata of the link itself | 200 `Stats` object |
| `GET`, `HEAD` | `/v1/drives/{id}/statfs/{path}` | Filesystem figures | 200 `StatsFs` object |
| `GET`, `HEAD` | `/v1/drives/{id}/readlink/{path}` | Symbolic link target | 200 JSON string |
| `POST` | `/v1/drives/{id}/ops/mkdir` | `{"path","recursive"?,"mode"?}` | 200 `{"path","created"}` |
| `POST` | `/v1/drives/{id}/ops/rename` | `{"from","to"}` | 200 `{"ok":true}` |
| `POST` | `/v1/drives/{id}/ops/truncate` | `{"path","length"}` | 200 `{"ok":true}` |
| `POST` | `/v1/drives/{id}/ops/sync` | No body | 204 |

`{path}` may contain several `/`-separated segments. Leave it out
(`/v1/drives/{id}/fs`, `/v1/drives/{id}/entries`, `/v1/drives/{id}/stat`,
`/v1/drives/{id}/statfs`) to address the drive root.

## Authentication

Send `Authorization: Bearer <token>` on every `/v1` request. Each drive has one
static token, provisioned by the operator. The scheme name is case-insensitive;
the token is compared exactly.

- A missing header, a malformed header, a wrong token and an unknown drive id
  all return the same `401` with `WWW-Authenticate: Bearer`. You cannot tell
  from the response whether a drive exists.
- A token for drive A is rejected with `401` on drive B.
- Tokens change only when the operator restarts the service. After a rotation
  the old token returns `401`; that is the signal to reload your credential.

## Paths

In the URL, each path segment is percent-decoded once and must be valid UTF-8.
Percent-encode spaces, `?`, `#`, `%` and non-ASCII bytes. The query string is
ignored. In JSON bodies (`mkdir`, `rename`, `truncate`) paths are absolute
(`/a/b`) and are not percent-encoded.

The server rejects these with `400 EINVAL`, before it looks at the token:

| Rejected | Example |
| --- | --- |
| Empty segment (a single trailing `/` is allowed) | `/fs/a//b` |
| `.` or `..` segment, also when percent-encoded | `/fs/%2e%2e/x` |
| Encoded `/`, `\` or NUL inside a segment | `/fs/a%2Fb`, `/fs/a%5Cb`, `/fs/a%00` |
| Bytes that are not UTF-8 after decoding | `/fs/%ff` |
| A segment starting with the reserved prefix `.mount-rs-upload.` (every drive; such names are also never listed) | `/fs/.mount-rs-upload.x` |
| JSON path not starting with `/`, or containing `\` | `{"path":"a/b"}` |

Paths are case-sensitive. A file name cannot contain `/` or `\` through this
API.

## Status codes and errors

Every error response from the server has this JSON body. The one exception is
the `/readyz` not-ready body (see [Health](#health)).

```json
{"error": {"code": "ENOENT", "message": "no such file or directory"}}
```

Branch on the HTTP status and on `code`. `message` is for people and can
change. Errors generated by the reverse proxy (its own 413, 502, 504 and so on)
do not have this body.

| Status | `code` | Meaning | Retry? |
| --- | --- | --- | --- |
| 400 | `EINVAL` | Invalid path, invalid JSON, body sent to `sync`, argument rejected by the filesystem | No |
| 400 | `EINVAL` | `readlink` on something that is not a symbolic link; rename of a directory into itself | No |
| 400 | `EXDEV`, `EMLINK`, `ELOOP`, `ENXIO` | Operation not possible on this path | No |
| 400 | `EIO` | The request body could not be read (client aborted the upload) | Yes, resend |
| 401 | `EACCES` | Missing or wrong token, or unknown drive | No; fix the credential |
| 403 | `EACCES`, `EPERM`, `EROFS` | The filesystem denies the operation, or the drive is read-only | No |
| 404 | `ENOENT`, `ENOTDIR` | No such file or directory; a parent is not a directory; listing a file; renaming a directory onto a file; unknown route | No |
| 405 | `ENOTSUP` | Method not supported on this route; see `Allow` | No |
| 406 | `ENOTSUP` | `Accept-Encoding` refuses both `gzip` and `identity` (listings only) | No |
| 409 | `EEXIST`, `ENOTEMPTY`, `EISDIR` | Target exists, directory not empty, or the path is a directory | No |
| 413 | `EFBIG` | Request body larger than the limit; connection closed | No |
| 413 | `EOVERFLOW` | Directory has too many entries, or its listing is too large; connection closed | No |
| 414 | `ENAMETOOLONG` | Name or path too long for the filesystem | No |
| 416 | `EINVAL` | Bad or unsatisfiable `Range`; `Content-Range: bytes */<size>` | No |
| 500 | `EIO`, others | Storage or server failure; outcome of a mutating request is unknown | Idempotent requests, with back-off |
| 501 | `ENOTSUP`, `ENOSYS` | The drive's filesystem does not support the operation | No |
| 503 | `ESTALE`, `EBADF` | The drive is unavailable, usually because it failed closed and needs an operator restart. Files are not gone. Rarely a transient stale-handle race on a healthy drive. No `Retry-After` | Yes, with back-off |
| 503 | `EAGAIN`, `EBUSY` | Transient contention; `Retry-After: 1` | Yes, after `Retry-After` |
| 503 | `EIO` | `server is shutting down`; connection closed | Yes, on a new connection |
| 504 | `ETIMEDOUT` | Server-side request timeout; connection closed; outcome of a mutating request is unknown | Idempotent requests |
| 507 | `ENOSPC`, `EDQUOT` | Storage full or over quota | Not until space is freed |

Other things a client sees:

- **Connection closed with no response.** The server is at its connection
  limit, or it closed an idle keep-alive connection. Retry on a new connection
  with back-off. Through a proxy this usually appears as a 502.
- **Body shorter than `Content-Length` on a download.** The server could not
  finish reading the file, or was shut down. Retry, optionally with `Range`
  from the last byte received.
- **A `404` never means a drive is temporarily unavailable.** An unavailable
  drive answers `503`. A `404` on a file means the file does not exist.

The server checks a request in this order: shutting down (503), path syntax
(400), route (404), drive and token (401), declared body size (413), method
(405), then the operation. A `405` on a drive route is only returned to an
authenticated caller. An unknown route is `404` with or without a token.

## Semantics

### Reading

`GET /v1/drives/{id}/fs/{path}` returns different things for different file
types. Check `Content-Type`, or `HEAD` first.

| `path` is | `Content-Type` | Body |
| --- | --- | --- |
| Regular file (symlinks are followed) | `application/octet-stream` | The bytes. `Accept-Ranges: bytes`. |
| Directory | `application/json` | `{"path","stats","entries"}`. `Range` is ignored. |
| Device, FIFO, socket | `application/json` | `{"path","stats"}` |

`Range` supports one range: `bytes=a-b`, `bytes=a-` or `bytes=-n`. The response
is `206` with `Content-Range: bytes a-b/size`. A malformed range, a unit other
than `bytes`, more than one range, a start at or past the end of the file, or
any range on an empty file returns `416`. The server does not fall back to a
full `200` response.

**Downloads are not snapshot-isolated.** A `GET` that overlaps a `PUT`,
`truncate` or `DELETE` of the same file can return a mixture of old and new
bytes, or a body shorter than `Content-Length`. If you need a consistent copy,
check that the received length equals `Content-Length`, and re-read when the
file may have been replaced meanwhile (compare `size` and `mtime_ms` from
`stat` before and after the download). Writers that publish under a new name
and never modify a published file avoid the problem.

A client that stops reading a download for longer than the request timeout is
disconnected.

`HEAD` returns the headers `GET` would return and no body, including for error
responses.

### Writing with PUT

- **Atomic.** The server receives the whole body, then publishes it in one
  step. Readers see the old content or the new content, never a prefix. A
  request that ends while the body is still being received (disconnect, `413`,
  timeout, shutdown) changes nothing and leaves no partial file. A request
  cut off during the single publication step is either fully published or not
  published at all.
- **Lost response means unknown outcome.** After a `504`, a `500` or a dropped
  connection on `PUT`, the file has either the old content or the complete new
  content. Read it back, or send the `PUT` again, before assuming either.
- **Durable before `200`.** A `200` means the content is durable. You do not
  need to call `sync` after a successful `PUT`. Two operator choices change
  this: a drive on memory storage is volatile, and a split-store drive with
  `writeback` enabled acknowledges before the data is durable. Ask the
  operator if you are unsure.
- **Overwrite.** `PUT` always replaces an existing file. There is no
  create-only mode, no `If-Match`, no `If-None-Match`.
- **Concurrent PUTs.** Two overlapping `PUT`s to one path leave exactly one of
  the two bodies, never a mix. Which one is not defined. If you need
  coordination between writers, do it in your service.
- **Parents.** The parent directory must exist (`404 ENOENT` otherwise). Use
  `mkdir` with `recursive: true` first.
- **Target is a directory** (including the drive root). `409 EISDIR`.
- **Size.** One request body, at most the server's request limit (8 MiB by
  default). There is no ranged, resumable or multipart upload. **Files larger
  than the limit are not supported**; split them into several files in your
  service, or ask the operator to raise the limit.
- **Send `Content-Length`.** A declared length above the limit is rejected
  with `413` before the body is read. The server also bounds the total upload
  bytes it buffers and admits uploads in arrival order; an upload without
  `Content-Length` reserves the full limit and waits longer under load.
- **Time.** The whole upload, including waiting for buffer space, must finish
  within the request timeout (30 s by default), or the server answers `504`.
- **Response.** `{"path":"/a/b","bytes":N}` where `N` is the body length.
  Any request `Content-Type` is accepted and ignored. It is not stored.

On drives backed by a host directory, a replace creates a new inode (existing
hard links keep the old content, and the old file's owner and group are not
carried over), and a `PUT` through a symbolic link is written in place and is
not atomic.

### Deleting

`DELETE` removes one file, one symbolic link (not its target), or one **empty**
directory. It is not recursive: a non-empty directory returns `409 ENOTEMPTY`.
To remove a tree, list it and delete bottom-up. The drive root cannot be
deleted; the server answers `400 EINVAL`.

### mkdir

`{"path":"/a/b"}` creates one directory; the parent must exist and the path
must not (`409 EEXIST`). `{"path":"/a/b","recursive":true}` creates missing
parents and succeeds if the directory already exists. `created` is the first
directory that was created, or `null`. Optional `mode` is the permission bits
as a decimal integer (493 is `0o755`); only the low nine bits are used.
Unknown JSON properties are ignored.

### rename

`{"from":"/a","to":"/b"}` follows POSIX `rename` semantics as implemented by
the drive's filesystem:

- The move is one step. No reader sees the entry under both names or neither.
- An existing file at `to` is replaced.
- A directory can replace only an empty directory (`409 ENOTEMPTY`).
- A file cannot replace a directory (`409 EISDIR`). A directory cannot replace
  a file (`404 ENOTDIR`).
- A directory cannot be moved into itself (`400 EINVAL`).
- `from` and the parent of `to` must exist (`404 ENOENT`). A rename cannot
  cross drives.

`PUT` to a temporary name followed by `rename` is a valid way to publish under
a final name, but is not required: `PUT` is already atomic.

### truncate

`{"path":"/a","length":N}` sets the size of an existing file to exactly `N`
bytes, cutting it or extending it with zero bytes.

### sync

`POST .../ops/sync` with no body flushes the drive. When it returns `204`,
every change acknowledged before the call is durable. `PUT` does not need it.
Call it after `mkdir`, `rename`, `truncate` or `DELETE` if those must survive a
crash of the server host.

### Metadata

`stat` and `lstat` return the `Stats` object of a path without reading its
content: `size`, `mode` (decimal `st_mode`; `mode & 61440 == 16384` is a
directory, `== 32768` a regular file, `== 40960` a symbolic link), `mtime_ms`
and the other POSIX fields. `stat` follows symbolic links; `lstat` describes
the link itself. `HEAD` on either is a cheap existence check. `statfs` returns
block and file counts as the drive's filesystem reports them; for split-store
and memory drives these are nominal figures, not free space in the backing
storage. `readlink` returns a link target as a JSON string and `400 EINVAL`
for anything that is not a symbolic link.

### Listings

`GET .../entries/{path}` returns an array of
`{"name","parent_path","file_type"}`. `file_type` is one of `File`,
`Directory`, `Symlink`, `BlockDevice`, `CharacterDevice`, `Fifo`, `Socket`.
The order is not guaranteed. `.` and `..` are not included.

A listing is complete or it fails. A directory with more entries than the
limit (4096 by default), or whose JSON is larger than the response limit
(8 MiB by default), returns `413 EOVERFLOW`. There is no pagination. Keep
directories below the limit.

With `Accept-Encoding: gzip`, listings of 1 KiB or more are returned with
`Content-Encoding: gzip`. Without the header the response is plain JSON. All
listing responses carry `Vary: Accept-Encoding`. File contents and the `fs`
route are never compressed.

## Retries and idempotency

| Request | Idempotent | Retrying after a lost response |
| --- | --- | --- |
| `GET`, `HEAD` | Yes | Safe |
| `PUT` | Yes | Safe. It can overwrite a newer write by another client |
| `DELETE` | Effect yes, status no | A `404` on the retry means the first attempt succeeded |
| `mkdir`, `recursive: true` | Yes | Safe |
| `mkdir`, non-recursive | No | `409 EEXIST` on the retry means the first attempt succeeded |
| `rename` | No | `404 ENOENT` on the retry: check whether `to` exists |
| `truncate` | Yes | Safe |
| `sync` | Yes | Safe |

Rules:

- Retry `503` and connection failures with exponential back-off and jitter.
  Honour `Retry-After` when present.
- After a `504`, a `500` or a dropped connection, the outcome of a mutating
  call is unknown. A `PUT` was either fully published or not at all, never
  partially: read the file back, or resend the `PUT`. For the other calls use
  the table above.
- Treat every `503` as retryable with back-off, whatever its code. A drive
  that stays at `503 ESTALE` needs the operator.
- Do not retry `4xx` other than a `400 EIO` body-read failure.
- `413`, `503 shutting down` and `504` close the connection. Open a new one.
- Keep idle connections for less than the server's request timeout (30 s by
  default), or be ready to retry a request that hits a connection the server
  has just closed.

## Health

| Route | 200 | 503 |
| --- | --- | --- |
| `/healthz` | `{"status":"ok"}`: the process serves HTTP | Only while shutting down (standard error body) |
| `/readyz` | `{"status":"ready","drives":N}`: no drive has reported a failure | `{"status":"not_ready","drives":N,"failed":M}`: a drive has failed closed or none is configured. While shutting down: the standard error body |

Both are unauthenticated, accept `GET` and `HEAD`, and send
`Cache-Control: no-store` and `X-Content-Type-Options: nosniff`. They never
reveal drive ids. They do not probe an external dependency on each call.

## Limits

Defaults; the operator can change them.

| Limit | Default | When exceeded |
| --- | --- | --- |
| Request body (`PUT` and JSON) | 8 MiB | `413 EFBIG`, connection closed |
| Directory entries per listing | 4096 | `413 EOVERFLOW`, connection closed |
| Listing response (uncompressed JSON) | 8 MiB | `413 EOVERFLOW`, connection closed |
| Request time | 30 s | `504 ETIMEDOUT`, connection closed |
| Idle keep-alive, time to send request headers | 30 s | Connection closed without a response |
| Download stalled by the client | 30 s without progress | Connection closed |
| Concurrent connections | 256 | New connections closed without a response |
| Buffered upload bytes, all requests | 4 x request body limit | `PUT` waits in arrival order; counts against request time |
| Download size | None | |
| Drive id | 1-64 of `A-Z a-z 0-9 - _ .` | |

## Not supported at launch

- Files larger than the request body limit; ranged, resumable or multipart
  upload; append.
- Pagination of listings.
- `ETag`, `If-Match`, `If-None-Match`, `If-Modified-Since`, create-exclusive.
- Recursive delete; copy; `chmod`, `chown`, `utimes`; creating symbolic or
  hard links.
- More than one byte range per request.
- TLS or a non-loopback listener in the server itself. Both are the reverse
  proxy's job.
- CORS headers and browser clients.
- Per-user identity or scopes. A token grants full read and write access to
  its drive.
- Token rotation without a restart.
- Change notifications, locking, and cross-request transactions.
- Snapshot-isolated downloads.
- Names beginning with `.mount-rs-upload.`.

The session and forwarding routes enabled by `remote_sessions` or `forwarding`
are experimental and are not part of the v1 launch contract.

## Embedding the crate

The crate exposes `HttpServer`, `HttpServerOptions`, `DriveRegistry` and
`DriveConfig`. A drive is an id, a token and an `Arc<dyn FsDriver>`; the
transport does not mount a filesystem and does not implement one.
`HttpServer::start` refuses any host that is not loopback (`127.0.0.1`, `::1`,
`localhost`). The `observability` feature records one `http.request` span and
completion event per request through an application-supplied `Telemetry`
handle; `observability-otlp` also reads the W3C trace context headers. Both are
off by default.

```text
./scripts/cargo-shared test -p mount-rs-http --locked
./scripts/cargo-shared clippy -p mount-rs-http --locked --all-targets -- -D warnings
```
