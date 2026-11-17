# mount-rs CLI

mount-rs is the native command-line front end for this repository. Its
default is intentionally close to the pinned mountx CLI: an in-memory
filesystem with a README, a request watcher, and one native mount chosen by
the auto facade. On macOS NFS, the same process can serve more than one
mountpoint from that opened filesystem.

The command accepts one positional mountpoint:

mount-rs [mountpoint] [options]
mount-rs mount [mountpoint] [options]
mount-rs probe
mount-rs validate-config --config PATH
mount-rs sdk-self-test [--config PATH] [--reopen]

For multiple writable views in one CLI, supply a primary mountpoint and
repeat `--also-mountpoint` for each additional empty, user-owned directory:

```sh
./scripts/cargo-shared run --locked -p mount-rs-cli -- \
  mount /absolute/path/to/view-a --also-mountpoint /absolute/path/to/view-b \
  --transport nfs --driver memory
```

This opens one driver and serves both native NFS paths until Ctrl-C unmounts
them. Open each path in Finder with **Shift-Command-G**; macOS NFS uses
`nobrowse`, so Finder does not show separate volumes. The
[macOS guide](../../docs/macos-usage.md#two-writable-views-of-one-filesystem)
also gives a FoundationDB config for two *independent* CLI processes sharing
one fresh volume. The final guarded-source macOS native two-CLI test passed
1/1 on 2026-09-23. The FoundationDB
concurrent mode is experimental while distributed handle pins and a capacity
protocol remain unfinished.

The shared NFSv3 profile requires a filesystem that checks an opaque file
handle's original inode during both namespace reads and handle-derived
mutations. Guarded stat, lookup, readdir and readlink use one backend snapshot;
a directory handle's old path must not list or look up entries from a
replacement directory after a remote rename. The CLI checks both guarded-read
and guarded-mutation capabilities at startup. `MemoryFs` and `ChunkedFs`
support these guarded operations and stable inode IDs. An old handle can rebind
to a renamed path when the server discovers the
same inode under its new name. `--driver host` and the legacy `--driver sqlite`
snapshot facade are single-view NFS choices; `--also-mountpoint` with either
fails at startup with an identity-guard error. The ordinary SQLite-backed
`splitstore` driver uses `ChunkedFs` and can serve several views in one CLI.
For a shared view, `--transport auto` selects NFS when its client is usable;
explicit FUSE or 9P selection fails. `--sqlite-single-host` uses local SQLite
locking and cannot be combined with shared NFS views.

For a TLS-required TiDB connection in structured configuration, build the CLI
with `--features rustls` and use a connection environment variable whose URL
includes `require_ssl=true`. The URL and credentials remain environment
references; the feature build does not replace the required credentialed
provider handshake and deployment certificate checks.

## Optional observability

Build the CLI with `--features observability` to enable the application-owned
`mount-rs-observability` facade at the CLI and HTTP boundaries. The feature is
off by default and does not add exporter SDKs to the ordinary binary. Set
`MOUNT_RS_TELEMETRY=1` before running the feature-enabled binary to activate
bounded local spans, metrics, and structured events; configure an exporter and
subscriber in the embedding application when OTLP export is required. See
`docs/observability.md` for the signal contract and shutdown requirements.

--help, --version, and probe do not create a driver or a mount. The probe
output describes the current host's FUSE, 9P, and NFS prerequisites and the
auto preference. Naming a transport skips that probe and does not fall back
after a mount error. Ctrl-C calls the selected mount's actual unmount
operation before the process exits.

The built-in driver choices are:

- --driver memory (default): volatile memory, seeded with README.md unless
  --empty is supplied.
- --driver host --root PATH: a rooted host filesystem; the default root is
  the current directory. Host paths are kept below the configured root by the
  host integration.
- --driver sqlite --database PATH: the durable SQLite snapshot integration.
  In JSON configuration, optional driver.uid and driver.gid set the virtual
  root ownership used by the mounted filesystem.
- --driver splitstore: independent metadata and immutable block stores. With
  no paths it is volatile memory; with both --database METADATA and --blocks
  BLOCKS it uses two durable SQLite databases.

## Versioned JSON configuration

Use --config PATH to select the same transport, lifecycle flags, and driver
settings from a versioned JSON file:

    mount-rs validate-config --config apps/mount-rs-cli/examples/config-memory.json
    mount-rs mount --config apps/mount-rs-cli/examples/config-splitstore.json

validate-config performs only JSON/schema and static option validation. It
does not open SQLite or PGlite, resolve credential values, construct an R2
client, or make a network request. Provider construction starts only after
the mount command has resolved the config.

Two CLI processes cannot use an ordinary exclusive-writer volume
concurrently. The experimental opt-in `driver.storage.concurrent_writes: true`
mode uses revision-CAS metadata and blocks available to every process. Each
CLI receives its own mountpoint and uses identical provider keys and paths.
The current metadata marker does not bind a block-backing identity. If two
writers use different SQLite block files or RustFS bucket/prefix settings,
both can acknowledge files into one namespace while each sees missing chunks
from the other's backing. Keep the shared block configuration identical for
every writer and stop a misconfigured writer before publication.
SQLite metadata and block databases support only processes on the same host
using the same local disk files; do not place those files on NFS or SMB.
PGlite needs one reachable socket server shared by every CLI or host.
FoundationDB requires `lease_authority: "revision-cas"`; the SQLite and
PGlite metadata configurations need no FoundationDB lease setting. RustFS
supplies shared immutable blocks when paired with one of those metadata
providers; it has no metadata revision authority of its own. Local SQLite
blocks are accepted only with local SQLite metadata, and memory blocks are
rejected. Existing exclusive volumes with a legacy writer lease or fence
require offline migration. Cross-host physical acceptance is still pending.
Stop and upgrade older writers
before activation because they do not recognize the concurrent-mode marker.
If a shared cluster still serves older writers,
isolate those clients at the deployment, network, or tenant level where that
control is available.

Concurrent mode retains detached file tombstones for open handles in other
processes, and blocks staged for failed or conflicted publications. Neither
has a built-in retention cap while distributed handle pins and safe block
reclamation are unfinished; backing storage can grow without bound during
use. This capacity protocol is required before the mode can be qualified for
long-running shared mounts.
Shared NFSv3 sessions also retain a backend descriptor and opaque handle entry
for each regular file visited until server close. `max_handles` is a soft
table target and does not bound pinned entries, so a long-running mount can
grow its process resources as well.

FoundationDB [watches](https://apple.github.io/foundationdb/developer-guide.html#watches)
could later wake a process to refresh its namespace after another process
publishes a revision. The current concurrent design refreshes on filesystem
operations; watches are not yet wired into mount-rs. Revision checks in
FoundationDB transactions control write conflicts, independent of any watch.
The NFS session's `shared_concurrent_view` setting enables the shared handle
identity checks and implies WCC omission. Its separate
`omit_wcc_attributes` setting can also omit WCC in an ordinary view; the CLI
sets both in shared views and omits
optional before/after weak-cache-consistency attributes because another writer
can commit before a reply. NFSv4 requests fail closed in the shared profile.
The CLI adds `noac` and `nonegnamecache` on macOS NFS for shared views, but
positive entries and file data may still be cached; Finder may need a
directory refresh. A watch cannot directly invalidate that kernel cache.

`sdk-self-test` is the mount-free Rust CLI example and integration entry
point. It constructs the selected filesystem through the public
`mount-rs-sdk::Filesystem` facade, writes and reads a binary file through the
shared `FsDriver` contract, synchronizes it, and cleans it up. With
`--config PATH --reopen`, it shuts down the first SDK filesystem, opens the
configured durable provider again, verifies the persisted bytes, and then
removes the test file. The default command uses memfs; a structured SQLite
split-store config is a portable durable example:

```sh
cargo run --locked -p mount-rs-cli -- sdk-self-test
cargo run --locked -p mount-rs-cli -- \
  sdk-self-test --config apps/mount-rs-cli/examples/config-splitstore.json --reopen
```

The Node counterpart is `examples/node-cli/index.mjs --sdk-self-test`; both
commands are exercised by the CLI integration tests and provider matrix. For
first-class AWS S3 blocks, start from
`examples/config-sqlite-aws-s3.json`: keep metadata independent, set the real
bucket and region, and provide short-lived AWS workload credentials through
the environment or deployment identity. The AWS provider deliberately does
not accept an endpoint or long-lived secret fields; use the `r2` provider for
S3-compatible endpoints.

The top-level version is currently 1. The driver object is discriminated and
strict: memory accepts only kind; host accepts kind and root; sqlite accepts
kind, database, and an optional uid/gid pair; and splitstore accepts either the
legacy database plus blocks pair or one structured storage object. Structured
splitstore.storage must not be combined with root, database, or blocks.

For writable SQLite hosting through the native NFS transport, configure the
virtual root owner explicitly:

    {
      "version": 1,
      "transport": "nfs",
      "sqlite_single_host": true,
      "driver": {
        "kind": "sqlite",
        "database": "./state.sqlite",
        "uid": 501,
        "gid": 20
      }
    }

uid and gid must be unsigned 32-bit values and must be supplied together.
They are filesystem metadata, not a request to change the host SQLite file's
ownership and not a way to forge NFS client credentials. Before the native
mount, the CLI reads virtual "/" through FsDriver and calls FsDriver::chown
only when its persisted metadata differs. PersistedFs stores that root change
in the SQLite snapshot, so reopening the same database retains the configured
owner. A different explicit pair is an intentional persisted root-owner
migration.

When uid/gid are omitted, the CLI never infers that a 0:0 root is new and
never changes persisted ownership implicitly. A writable mount whose persisted
root does not match the effective identity fails and asks for an explicit
uid/gid migration. A read-only mount with any ownership mismatch also fails
without mutating the database; use a matching pair for read-only access.
This policy only addresses the virtual filesystem root; SQLite journal/WAL and
transaction safety still require the native hosting acceptance tests below.

Structured storage has independent metadata and block providers. Each
provider is strict and supports memory, sqlite, pglite, tidb, and
FoundationDB; R2, RustFS, and AWS S3 are block-only. PGlite and TiDB
connection URLs plus R2 and RustFS credentials are environment references such as
{"env":"R2_SECRET_ACCESS_KEY"}, never plaintext values. FoundationDB uses a
resolved cluster_file path and requires the explicit `lease_authority` setting.
Use `"persisted-single-authority"` only for an owned single-authority/test
path. Production consumers should use `"shared-provider"` together with an
`authority_prefix`, and grant the CLI read-only access to that authority
record for the exclusive-writer mode; the authority service alone may publish
provider time. Fresh concurrent-writer FoundationDB metadata uses
`"revision-cas"` instead and requires `concurrent_writes: true` in the
structured storage config. Enable the CLI's foundationdb feature on a
supported native target; the default CLI
remains portable and fails closed when that native provider is selected
without the feature. See
examples/config-pglite-r2.json for the block-only R2 shape,
examples/config-tidb-rustfs.json for the TiDB metadata/RustFS block shape, and
examples/config-foundationdb-rustfs.json for FoundationDB metadata with
RustFS blocks. For opt-in concurrent mode, see
examples/config-sqlite-concurrent.json,
examples/config-pglite-rustfs-concurrent.json, and
examples/config-foundationdb-rustfs-concurrent.json.

TiDB metadata can be composed with RustFS through the `rustfs` block provider.
The TiDB `connection` and `volume_key` are
passed to the provider, and `durable` remains an explicit caller assertion;
the URL alone does not prove replicated TiKV durability. The opt-in provider
matrix uses this shape for configuration, shutdown/reopen, partial-write,
truncate, and owned-prefix cleanup checks when live TiDB and RustFS
credentials are available.
RustFS endpoint URLs must contain only the HTTP(S) authority and may have one
trailing slash; provider paths and buckets go in their separate fields.
Omitting RustFS `durable` defaults to false. Set it to true only when the
configured RustFS service is expected to retain completed block writes.

Explicit command-line flags override only the config fields they name.
Unspecified flags retain config values. Relative config paths are resolved
relative to the config file. An omitted splitstore owner gets a
process-and-instance-specific writer-fence owner; explicit owners are
validated before provider construction. Splitstore also accepts the optional
`lease_ttl_ms` field, in milliseconds. Exclusive ownership defaults to 240
seconds (see the owner serving window below); concurrent writes reject it. Set it
explicitly for remote providers when the observed operation latency requires
more lease headroom, while keeping stale-writer recovery within the
deployment's recovery objective.

## Unmounted HTTP service

`serve-http` serves named drives over HTTP without creating a FUSE, 9P or NFS
mount.

    mount-rs serve-http --config apps/mount-rs-cli/examples/config-http.json

The full documentation is in three places:

- API guide for consumers: [`transports/mount-rs-http/README.md`](../../transports/mount-rs-http/README.md)
- OpenAPI 3.1 contract: [`transports/mount-rs-http/openapi.yaml`](../../transports/mount-rs-http/openapi.yaml),
  checked against the server by `transports/mount-rs-http/tests/openapi_conformance.rs`
- Operator runbook (config reference, proxy, health, shutdown, recovery):
  [`docs/serve-http-runbook.md`](../../docs/serve-http-runbook.md)

- Sessions, forwarding and failover: the contract is
  [`transports/mount-rs-http/openapi-cluster.yaml`](../../transports/mount-rs-http/openapi-cluster.yaml),
  checked against real servers by `apps/mount-rs-cli/tests/cluster_contract.rs`;
  the operator chapter, with a config reference for every key, a worked
  three-server example ([`examples/cluster/`](examples/cluster/)) and the
  known limits, is ["Clusters" in the runbook](../../docs/serve-http-runbook.md#clusters).

The file routes (v1) answer the same on a plain server, with
`remote_sessions`, and on every server of a forwarding cluster; the
specified differences are listed in the API guide's "Clusters" section.

### Configuration

An HTTP config has the same top-level `version` as native configs and an
`http` object instead of native mount fields. `serve-http` accepts only
`--config <path>`. A config with an `http` object is rejected by the native
mount commands. Unknown keys are rejected. Relative paths resolve against the
config file's directory.

`http.drives` is required and must contain at least one drive. Each drive has:

- `id`: 1-64 characters from `A-Z a-z 0-9 - _ .`, unique, used in URLs;
- `token`: `{"env": "NAME"}`, the environment variable that holds the drive's
  bearer token, or `{"file": "PATH"}`, a file that lists it (see
  [Tokens](#tokens));
- `driver`: a `memory`, `host`, `sqlite` or `splitstore` driver object in the
  same shape as native configs. `remote` drivers are rejected.

Listener and limit keys, all optional:

| Key | Default | Meaning |
| --- | --- | --- |
| `host` | `127.0.0.1` | Loopback hosts only (`127.0.0.1`, `::1`, `localhost`), unless `allow_non_loopback` is set. |
| `allow_non_loopback` | `false` | Lets `host` be any address, such as `0.0.0.0` in a container or behind a load balancer on a private network. The listener stays cleartext. |
| `port` | `0` | `0` selects an ephemeral port, printed in the readiness line. |
| `max_request_bytes` | 8388608 | Largest body held whole in memory: JSON bodies, session writes, and file uploads up to this size. |
| `max_upload_bytes` | 5368709120 | Largest file a `PUT` may upload. Uploads above `max_request_bytes` stream to a temporary file and are renamed into place. Must be >= `max_request_bytes`. |
| `max_buffered_upload_bytes` | 4 x `max_request_bytes` | Total upload bytes buffered across requests; uploads beyond it wait. Must be >= `max_request_bytes`. |
| `max_response_bytes` | 33554432 | Largest directory listing. |
| `max_directory_entries` | 100000 | Largest listable directory. |
| `read_chunk_bytes` | 65536 | Download read size. |
| `request_timeout_ms` | 30000 | Per-request, header-read, idle keep-alive and stalled-download timeout. |
| `drain_timeout_ms` | 35000 | Shutdown drain. Keep it above `request_timeout_ms`. |
| `drive_shutdown_timeout_ms` | 30000 | Longest the drives may take, together, to shut down after the listener has stopped; past it the process exits `1` without waiting. |
| `max_connections` | 256 | Concurrent connections; excess connections are closed. |
| `access_log` | `false` | `true` writes one `http_request` JSON line per request to stderr: request id, method, route template, status, duration and byte counts; never the path or the token. |
| `tcp_nodelay` | `true` | Set `false` to restore TCP packet coalescing. |
| `http2_prior_knowledge` | `false` | `true` makes the listener cleartext HTTP/2 only; the proxy must then send the HTTP/2 preface upstream. |

Numeric values must be positive integers. The runbook lists the remaining keys
(`cache_capacity`, `disk_cache_budget`, `tidb_metadata_pool_limit`,
`cache_report_interval_seconds`, `remote_sessions`, `forwarding`).

Split storage accepts `read_parallelism` only as `1` (the default): block reads
within a file read are sequential. Parallel block fetches are not supported.

### Tokens

A token is never printed and never stored in the config. It must be
non-empty and contain no whitespace. Each token is valid only for its own
drive; a request for an unknown drive gets the same `401` as a wrong token.

With `{"env": "NAME"}` the token is read from that environment variable once,
when the service starts. Changing it requires a restart.

With `{"file": "PATH"}` the token is read from a file, at startup and again
while the service runs, so it can be rotated without a restart. The file
lists one token per line: the current one and, optionally, the one before
it. Both are accepted, which gives clients time to move. A relative path is
taken from the config file's directory. The file is read again:

- when the process receives `SIGHUP` (Unix), and
- every `http.token_reload_seconds` (default `30`, allowed `0`-`3600`), if
  its contents have changed. `0` turns the timer off.

A file that cannot be read, or that does not list one or two valid tokens,
is refused whole and the tokens in force stay as they were. Each reload
writes one `{"event":"http_token_reload","drive":...,"outcome":"reloaded"|"unchanged"|"refused","tokens":N,"error":...}`
line to stderr, which never contains a token. At startup such a file is an
error.

To rotate: put the new token on the first line and keep the old one on the
second; move the clients; then remove the old line. Replace the file by
renaming a new one into place.

`token.file` is refused together with `http.remote_sessions` or
`http.forwarding`: those authorize against the token read at startup. A
server with no token file does not handle `SIGHUP` for tokens.

### Metrics

`http.metrics` turns on a second listener that serves Prometheus text at
`GET /metrics`, without a token:

```json
"metrics": {"listen": "127.0.0.1:9464"}
```

`listen` is an IP address and port and must be loopback unless
`"allow_non_loopback": true` is set beside it. The API listener never serves
`/metrics`. The series are requests by route template, method and status
class (`mount_rs_http_requests_total`), a duration histogram by route
template (`mount_rs_http_request_duration_seconds`), requests in flight,
request and response body bytes, readiness per drive
(`mount_rs_http_drive_ready{drive=...}`), dropped access-log lines,
`process_start_time_seconds` and `mount_rs_build_info{version=...}`. No path
or token appears in them. The
[runbook](../../docs/serve-http-runbook.md#metrics) has the full table.

### Network exposure

The listener is cleartext HTTP/1.1 on loopback. It has no TLS. Remote clients
must go through a TLS reverse proxy that runs in the same network namespace.
The runbook has the proxy requirements and reference nginx and Caddy
configurations. Where the network up to the listener is private, such as a
container network or a VPC behind a load balancer that terminates TLS,
`"allow_non_loopback": true` lets `host` be a non-loopback address instead.

### Lifecycle

After every drive is open and the socket is bound, the process prints:

    http listening at http://127.0.0.1:PORT (drives: memory, sqlite)
    Press Ctrl-C to stop.

`GET /healthz` reports that the process is alive. `GET /readyz` returns `200`
with `{"status":"ready","drives":N}` and `503` with
`{"status":"not_ready","drives":N,"failed":M}` when a drive cannot serve: a
split-store drive that has failed closed, a `host` drive whose root
directory cannot be opened, or a `sqlite` drive whose database file cannot
be opened or whose last save failed. The storage of `host` and `sqlite`
drives is checked every two seconds in the background. A `memory` drive has
nothing to check.

Ctrl-C and, on Unix, SIGTERM start a graceful shutdown: the listener stops
accepting, in-flight requests get up to `drain_timeout_ms`, the drives are
shut down (which releases split-store writer leases), and the process prints
`http stopped` and exits `0`. If the drain deadline passes, in-flight requests
are aborted, the drives are still shut down, and the process exits `1` with
`HTTP server close timed out` on stderr and no `http stopped` line. The
drives in turn have `drive_shutdown_timeout_ms` to shut down; if storage does
not answer in that time the process exits `1` at once with
`drive shutdown timed out after N ms: ...` naming the drives it left, whose
writer leases then stay held until they expire.

Exit codes: `0` clean stop; `1` runtime failure (missing token variable, drive
could not be opened, bind failure, signal during startup, listener failure,
drain deadline exceeded, drive shutdown deadline exceeded); `2` usage or
configuration error.

A split-store drive is served by exactly one process. After an unclean exit
(SIGKILL, crash) its writer lease remains until it expires (`lease_ttl_ms`,
240 seconds by default for a structured `storage` object). Until then a
restart fails at once with `EAGAIN: resource temporarily unavailable, acquire
writer` and exit code `1`; it does not wait, so restart it with back-off.

### Write semantics

`PUT /v1/drives/{id}/fs/{path}` is atomic and durable before `200`: the body
is received in full and published in one step; a failed or aborted upload
leaves the previous content unchanged and no partial file. (A `memory` drive
is volatile, and a split-store drive with `writeback` acknowledges before the
data is durable.) One `PUT` carries
one file of at most `max_upload_bytes` (5 GiB by default). Uploads larger than
`max_request_bytes` are streamed to a hidden temporary file as they arrive and
renamed over the target once complete, with the same guarantee; each piece of
such an upload must arrive within `request_timeout_ms` of the last.
Files carry an `ETag`, derived from the content on split-store drives, and
`GET`, `HEAD`, `PUT` and `DELETE` honour `If-Match` and `If-None-Match`:
`If-None-Match: *` creates only, and `If-Match` with the tag you read replaces
only that version.
`DELETE` is not recursive. See the API guide for the status codes, retry rules
and limits.

See `examples/config-http.json` for a memory drive and a durable SQLite drive
with isolated tokens.

A container image for `linux/amd64` and `linux/arm64` is published with each
CLI preview release as `ghcr.io/andymac4182/mount-rs:<version>`, and the
repository's `Dockerfile` builds it locally. See the runbook's
[container section](../../docs/serve-http-runbook.md#in-a-container).

### Quick local demo

The checked-in example is runnable on macOS, Linux, and Windows terminals
with `curl` (PowerShell users can use `Invoke-WebRequest` for the same routes):

```sh
MOUNT_RS_MEMORY_TOKEN=demo-memory \
MOUNT_RS_SQLITE_TOKEN=demo-sqlite \
  cargo run --locked -p mount-rs-cli -- serve-http \
  --config apps/mount-rs-cli/examples/config-http.json
```

In a second terminal, use the address printed by the service:

```sh
curl http://127.0.0.1:PORT/readyz
curl -H 'Authorization: Bearer demo-memory' \
  -X PUT --data-binary 'hello from mount-rs' \
  http://127.0.0.1:PORT/v1/drives/memory/fs/hello.txt
curl -H 'Authorization: Bearer demo-memory' \
  http://127.0.0.1:PORT/v1/drives/memory/fs/hello.txt
curl -H 'Authorization: Bearer demo-memory' \
  http://127.0.0.1:PORT/v1/drives/memory/entries
curl -H 'Authorization: Bearer demo-memory' \
  http://127.0.0.1:PORT/v1/drives/memory/stat/hello.txt
curl -H 'Authorization: Bearer demo-sqlite' \
  -X PUT --data-binary 'durable split-store data' \
  http://127.0.0.1:PORT/v1/drives/sqlite/fs/persisted.txt
```

The SQLite drive uses independent metadata and block databases under the
example's `state/` directory. Stop the service with Ctrl-C, start it again,
then GET the same `persisted.txt` path to verify reopen persistence; the memory
drive is expected to reset. The CLI creates missing SQLite parent directories,
and generated demo state should be removed when finished.

For a SQLite database hosted through this process's loopback NFS server, add
`--sqlite-single-host` with `--transport nfs` or `--transport auto`. The flag
selects the NFSv3 single-host profile: a hard mount and local-only locking
(`locallocks` on macOS, `local_lock=all` on Linux). It is deliberately scoped
to one host and one client; it is not distributed locking, cross-host
coordination, or a power-loss durability guarantee. The profile is an NFS
setting only: `--transport fuse` and `--transport 9p` reject it, and `auto`
does not force NFS—if auto selects FUSE or 9P, no NFS profile is applied. Use
`--transport nfs` when NFS is required.

This option does not guarantee SQLite WAL support. Treat WAL as unsupported
unless the target host and filesystem have been independently verified; use
SQLite's DELETE journal mode for the portable single-host NFS path.

The CLI never uses a native mount in its ordinary parser, help, version, probe,
or watcher tests. Those tests verify the protocol-free portions only; they do
not claim a native kernel mount on either platform.

## Native prerequisites

On Linux, automatic selection prefers FUSE, then 9P, then NFS. Rootless FUSE
needs /dev/fuse and fusermount3 (or fusermount) available on PATH. Native 9P
and Linux NFS mounting normally require CAP_SYS_ADMIN/root and the
corresponding kernel/client support. A named transport is attempted once, so
an unavailable named transport is reported rather than silently replaced.

On macOS, automatic selection prefers NFS. The process must be allowed to use
the native NFS client and an unprivileged user must own the mountpoint.
Privacy/Full Disk Access policy can also reject a mount or unmount; grant the
terminal/process the required permission and retry. macFUSE is a different
protocol and is not treated as this crate's FUSE transport.

The transport probe is an availability explanation, not a native mount test.
Run a real mount explicitly on a disposable, user-owned mountpoint only when
the host prerequisites are installed.

## Native lifecycle acceptance

The native FUSE subprocess check is intentionally ignored in ordinary tests;
it is not a wire-test claim. On Linux, with `/dev/fuse` and the required
mount capability available, run:

    MOUNT_RS_CLI_NATIVE_FUSE=1 cargo test -p mount-rs-cli --test native_lifecycle -- --ignored --nocapture

That test starts the actual `mount-rs` binary, waits for the kernel mount,
sends SIGINT, checks the CLI's `unmounted` report, and checks
`/proc/self/mounts` after exit. macOS has separate opt-in NFS lifecycle
checks. With the built-in `/sbin/mount_nfs` available and the terminal/process
already allowed by its ownership and Privacy policy, run the host-driver case:

    MOUNT_RS_CLI_NATIVE_NFS=1 ./scripts/cargo-shared test --locked -p mount-rs-cli \
      --test native_lifecycle cli_nfs_config_binary_persists_bytes_and_cleans_up_on_sigint \
      -- --ignored --exact --nocapture

This check writes an extension-free config file for the actual binary,
mounts the configured host driver through native NFS, writes and reopens
bytes, sends SIGINT, verifies the unmount, then remounts the same config and
backing directory in a fresh child to verify persistence before removing the
test file. The memory-provider case uses the same native mount lifecycle,
writes and reads bytes, then verifies that a fresh CLI process starts empty:

    MOUNT_RS_CLI_NATIVE_NFS=1 ./scripts/cargo-shared test --locked -p mount-rs-cli \
      --test native_lifecycle cli_nfs_memory_config_binary_mounts_io_and_resets_on_restart \
      -- --ignored --exact --nocapture

For PGlite metadata and blocks, the harness starts a disposable local PGlite
server, runs the native NFS test across two CLI processes while the server
stays alive, and stops the server afterward:

    MOUNT_RS_PGLITE_TEST_SCOPE=native-nfs ./scripts/test-pglite.sh

The harness clears any inherited `PGLITE_DATA_DIR` for its own in-memory
server. To verify that an ambient data directory remains untouched through
the native scope, run `./scripts/test-pglite-isolation.sh`.

Each case owns a unique mountpoint and config file and cleans up only those
paths. The native tests are ignored in ordinary runs; running one directly
without `MOUNT_RS_CLI_NATIVE_NFS=1` fails rather than silently passing. They
never invoke `sudo`, install a helper, or change host configuration. The
host-backed and memory passes do not satisfy the separate SQLite native-NFS
acceptance requirement. Run that case separately:

    MOUNT_RS_CLI_NATIVE_NFS=1 ./scripts/cargo-shared test --locked -p mount-rs-cli \
      --test native_lifecycle cli_nfs_sqlite_config_binary_hosts_sqlite_and_reopens \
      -- --ignored --exact --nocapture

The SQLite case uses an explicit driver.uid/gid pair and records the SQLite
version, journal mode, synchronous setting, locking/recovery results, and
cleanup evidence. The host-backed case keeps
the NFS server's backing-directory ownership aligned with the unprivileged
macOS kernel client.

For the opt-in FoundationDB metadata/RustFS block composition, build the CLI
with the native feature and provide the disposable cluster/client plus RustFS
environment:

    MOUNT_RS_CLI_NATIVE_FOUNDATIONDB=1 \
    MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/path/to/fdb.cluster \
    R2_ENDPOINT=http://127.0.0.1:9878 \
    R2_BUCKET=mount-rs-rustfs \
    R2_ACCESS_KEY_ID=... R2_SECRET_ACCESS_KEY=... \
    cargo test -p mount-rs-cli --features foundationdb \
      --test native_lifecycle \
      cli_foundationdb_rustfs_config_binary_mounts_and_reopens -- \
      --ignored --exact --nocapture

Linux selects FUSE and macOS selects native NFS. The test runs the actual
config-driven CLI twice, writes and reopens bytes through FoundationDB
metadata plus RustFS-compatible blocks, and verifies each native unmount. It
is separate from the portable CLI suite and does not pass without a matching
FoundationDB client library and live services.
The hosted Linux gate first publishes a provider-time sample from a separate
authority process and runs this CLI case with `shared-provider` plus a
read-only authority prefix; local runs default to the explicit persisted
single-authority/test mode unless `MOUNT_RS_CLI_FOUNDATIONDB_SHARED_PROVIDER=1`
and `MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX` are set.

For shared native views, `native_multi_mount` checks one CLI managing two
memory views and, with `--features foundationdb`, two FoundationDB views that
reopen cleanly. `native_two_process_foundationdb` checks two simultaneously
mounted independent CLIs, bidirectional create/read, concurrent writes,
rename, unlink and a third CLI reopen. Both FoundationDB cases require an
owned disposable loopback cluster, native `libfdb_c.dylib` and explicit test
opt-ins; they do not qualify a remote host or long-running block reclamation.
The final native two-process case passed 1/1 on 2026-09-23 after the
identity-guard and teardown fixes. The one-CLI dual FoundationDB and dual
memory native cases also passed once each on that source.

## macOS three-way shared visibility boundary

There is currently no valid acceptance command for mounting one mount-rs
backing location concurrently through macOS NFS, FUSE, and FSKit. The
capability-boundary check is rootless and non-mutating:

    cargo test --locked -p mount-rs-core --test native_shared_visibility

It records the only currently available macOS native path (NFS), verifies that
the mount-rs FUSE API returns `UnsupportedPlatform` before touching a
mountpoint, and records that FSKit now has a path-backed worker lifecycle, but
the target is still unsigned and has no containing app, activation, or
mounted-volume host. macFUSE is not interchangeable with this repository's Linux FUSE
protocol, and the current CLI/auto transport set has no FSKit mount authority.
The test therefore must not be read as three-way shared-write evidence.

If this gate is later expanded into the real acceptance, it must first acquire
one exclusive per-run lock, create one disposable backing location, and use
three distinct sibling mountpoints. Each writer must close or synchronize its
file before another mount reads the committed bytes; a provider lock or
single-writer lease must be proven safe for the three independent workers
before the test shares a database path. Cleanup must unmount each exact path
with a bounded deadline, verify that it is no longer mounted, remove only the
empty mountpoint directories, and preserve the backing artifacts when any
mount remains. Missing FUSE/FSKit capability is an unsupported result, never a
fallback to NFS or a passing shared-visibility claim.

Explicit `exclusive` and `shared` ownership modes are available for chunked split stores. Exclusive mode enables deferred publication with durable synchronization. Explicit shared mode uses MRC3 directory ownership and requires a `checkout_path` before the CLI exposes a native mount:

```json
{
  "version": 1,
  "driver": {
    "kind": "splitstore",
    "storage": {
      "ownership_mode": "shared",
      "checkout_path": "/project",
      "metadata": { "kind": "sqlite", "path": "metadata.sqlite" },
      "blocks": { "kind": "sqlite", "path": "blocks.sqlite" }
    }
  }
}
```

`checkout_path` names a canonical absolute path inside the virtual filesystem,
not a host mountpoint. It is valid only with explicit shared ownership. The SDK
claims that subtree during filesystem opening, before the native transport starts;
a conflicting claim fails the mount. This mode currently requires Linux FUSE; `auto` requires a usable FUSE client.
NFS, 9p, multiple mountpoints, and the NFS SQLite profile are rejected until
cache handoff is qualified. SQLite provider files must still live outside the
mountpoint and remain subject to their same-host constraints.

Omitted ownership modes preserve legacy behavior. In particular,
`concurrent_writes: true` alone selects MRC2 optimistic revision CAS and retains
its existing shared-provider and NFS policy. Supplying both options requires
`concurrent_writes` to agree with the selected ownership mode. See
[mount ownership contracts and configuration](../../docs/mount-ownership.md).

Directory ownership has separate offline administration commands:

```sh
mount-rs enroll-directory-ownership --config shared.json --expected-revision 7
mount-rs directory-ownership-status --config shared.json
mount-rs recover-directory-ownership --config shared.json --root-inode 42 --expected-fence 9
```

Enrollment requires an initialized namespace at the exact expected revision.
Stop all older mounts before enrollment and keep them stopped until every client
supports MRC3. Status prints JSON with the persisted grants, including root inode IDs and
fences, without mounting. Recovery retires the exact observed crashed owner's
fence; take the root inode and fence from status. Unmount the old native view
before recovery and remount afterwards to invalidate its kernel caches.
These commands open provider administration handles without acquiring a subtree
checkout or starting a native transport. Recovery does not synchronize another
process's pending work. Legacy MRC2 migration commands do not accept explicit
MRC3 shared configurations.

## Split-store tuning and HTTP cluster sessions

### TiDB metadata and explicit cache policy

Structured split-store configuration accepts TiDB through an environment
reference, and supports the opt-in process-local block cache and noatime policy:

```json
{
  "version": 1,
  "driver": {
    "kind": "splitstore",
    "storage": {
      "metadata": {
        "kind": "tidb",
        "connection": {"env": "MOUNT_RS_TIDB_URL"},
        "volume_key": "example-volume",
        "durable": false
      },
      "blocks": {"kind": "memory"},
      "atime": "noatime",
      "block_cache": {"max_bytes": 16777216, "max_entries": 4096}
    }
  }
}
```

This example is deliberately ephemeral; memory blocks are lost on restart.
Replace `blocks` with a configured SQLite or R2/RustFS provider for persistence.
`durable` is a deployment assertion, not an automatically inferred guarantee.
Omit `atime` and `block_cache` for noatime/uncached defaults. Reads do not update
access timestamps. Set `"atime": "strict"` explicitly to retain synchronous
access-time updates, which add TiDB metadata publication to reads/listings. Cache
limits must be positive; unknown options and inline connection secrets fail
static validation. The same storage object works inside a `serve-http` drive.

The current SDK still holds a single writer lease per volume. These options
**do not yet allow independent mounts to open the same volume concurrently**;
distributed metadata coherence and multi-mount acceptance remain outstanding.

### Experimental owner serving window

The SDK `SplitRuntimeOptions.read_lease_cache_window` (CLI storage field
`read_lease_cache_window_ms`) defaults to 60000ms, paired with an authoritative
`lease_ttl_ms` default of 240000ms. With a shorter explicit `lease_ttl_ms` the
default window is a quarter of the lease (zero below 80ms). Background renewals run every 30000ms.
Zero explicitly restores per-operation renewal. A nonzero serving window must
be at least 20ms and at most one quarter of `lease_ttl_ms`. The serving window,
`atime`, `block_cache`, `disk_cache` and `verify_backing_blocks` apply only to
exclusive ownership; they are rejected with `concurrent_writes` or shared
`ownership_mode`. After an owner crash,
takeover may wait roughly four minutes; Redis route expiry does not shorten it.
This requires managed provider clocks without abrupt forward expiry jumps or
out-of-band early revocation. It requires a Tokio runtime. Renewal failures or unknown outcomes
fail the owner closed. This is an experimental owner-service optimization, not
an implementation of multi-server discovery or multi-client cache coherence.

The block provider `{"kind":"local","path":"/existing/owned/block-directory"}`
uses immutable content-addressed files with file and directory synchronization
on Unix. Provision the directory durably before use. It must be application-owned;
local disk contents are not automatically replicated to another owner server.

### Uncached FUSE mounts

Use `--transport fuse --fuse-uncached`, or set `"transport": "fuse"` and
`"fuse_uncached": true` in a version-1 native mount config, to request zero
kernel metadata TTLs and direct file I/O. The resolved transport must explicitly
be FUSE; auto/NFS/9P are rejected rather than silently using another cache policy.
This bypasses kernel caching, not the owner's backing-store cache. Existing
mount defaults are unchanged. This option does not add a remote driver or prove
multi-mount coherence; native shared-writer acceptance remains outstanding.

### Verifying restored content before serving

Set `"verify_backing_blocks": true` inside `driver.storage` to verify a restored split volume before opening it to clients. The owner reads every distinct referenced block directly from the authoritative provider and checks that it contains all byte ranges committed in metadata. A missing or short block fails startup with `ESTALE`, even when its contents are present in the local cache. The scan reads the entire referenced block set and can substantially increase startup time and RustFS GET traffic. Its checks cover availability and length. Detecting same-length corruption requires content integrity checks from the provider or a trusted digest.

### Persistent disk content cache

Structured split storage can opt into a disk tier below the optional RAM cache:

```json
"disk_cache": {
  "root": "./cache",
  "scope": "volume-42-generation-7",
  "max_bytes": 1073741824,
  "max_entries": 16384
}
```

Place this object beside `metadata`, `blocks` and `block_cache` inside `storage`.
The root is resolved relative to the configuration file and must already exist.
Each volume needs an explicit non-secret scope identifying its backing generation.
Change it if backing data is recreated or block IDs can be reused, including when
starting a fresh memory backing store. The same scope cannot be owned by two
independent disk-cache instances. This setting works for local split-store mounts
and `serve-http` drives, including the drives of a cluster owner. Native
remote-mount acceptance (the `remote` driver, below) remains incomplete; this
cache setting does not supply that lifecycle.

Limits cover encoded cache bytes (including 80-byte entry headers) and entry
count. `max_bytes` must be at least 80 and `max_entries` positive. Omission keeps
disk caching disabled. This is currently Unix-only and preserves authoritative
provider durability. Per-server aggregate limits and RustFS performance
qualification remain outstanding. See the SDK documentation for separate RAM and
disk cache counters and clearing controls.

For a multi-drive HTTP server, an optional `http.disk_cache_budget` object shares
disk-cache admission across all disk-cached drives in that process:

```json
"disk_cache_budget": { "workers": 8, "fill_copies": 4 }
```

Both fields are required integers from 1 through 512. This example is a resource
limit, not a recommended performance setting. Omitting the object preserves
private four-worker/four-copy admission per cached volume. Shared admission also
covers cache opening; shutdown of one volume does not close the budget. It does
not limit aggregate resident disk/RAM capacity, provider/request buffers or
unrelated runtime blocking work. Each admitted fill copy is at most 16 MiB.
Tune against the intended concurrency: measured four-slot budgets improved some
workloads and worsened others. Non-disk-cached drives do not use this budget.

Optionally set `http.cache_capacity` to reject cache overcommit before providers
open. Both non-negative integer fields are required:

```json
"cache_capacity": { "ram_bytes": 536870912, "disk_bytes": 17179869184 }
```

The parser sums every drive's configured `block_cache.max_bytes` and
`disk_cache.max_bytes` independently, rejecting overflow or totals above these
limits. Disabled caches reserve zero. This is static admission of configured
maxima, not a shared LRU or physical disk quota. It does not account for metadata,
allocator overhead, transient encoded files, returned read buffers or provider
buffers; allow headroom for those separately. Omission preserves existing
configuration behavior. SDK callers managing volumes dynamically must enforce
their own capacity reservations.

## Remote volume client (experimental)

A mount client can use an authenticated public server without Redis, TiDB or
block-store connection settings:

```json
{
  "version": 1,
  "driver": {
    "kind": "remote",
    "server": "https://mount.example",
    "volume": "drive-1",
    "token": { "env": "MOUNT_CLIENT_TOKEN" },
    "timeout_ms": 30000
  }
}
```

Run `mount-rs mount /path/to/mount --config remote.json`. The credential is read
from the named environment variable, never from an argument or literal config
field. The server must expose the authenticated session protocol; the existing
`serve-http` file endpoint alone is insufficient. HTTPS is required except for
loopback testing. Redirects and automatic request replay are disabled.

`mount-rs sdk-self-test --config remote.json` checks public-endpoint read/write
and session cleanup without a native mount. Remote `--reopen` qualification is
not supported by this command. FUSE requests uncached kernel access for remote
volumes. NFS/9p cache coherence and native remote mounts remain unqualified.
The CLI owns a keepalive worker using the server-advertised idle timeout and
joins it before session shutdown. Failed refreshes stop without recreating the
session: after the server that owned the drive stops, or the drive fails over
to another server, the mount's session is gone and the mount must be started
again. The server side of this protocol (`http.remote_sessions`, below) has a
contract and tests of its own; what is experimental here is the mount client.
Do not use this client mode as production acceptance evidence.

### Remote sessions on a configured HTTP server

For split-store drives, `http.remote_sessions: true` enables the session and
handle API used by the remote mount CLI on the real `serve-http` command.
It defaults to false. All drives in that service must be split-store drivers.
Each configured drive token must satisfy the session credential format: 32–256
ASCII letters/digits or `-._~+/=`; use a high-entropy secret from its environment
reference. The drive token authorizes both file and session routes for that volume.

```json
{
  "version": 1,
  "http": {
    "host": "127.0.0.1",
    "port": 8080,
    "remote_sessions": true,
    "drives": [{
      "id": "volume",
      "token": {"env": "MOUNT_VOLUME_TOKEN"},
      "driver": {"kind": "splitstore", "storage": {
        "metadata": {"kind": "memory"},
        "blocks": {"kind": "memory"},
        "chunk_size_bytes": 4096
      }}
    }]
  }
}
```

The memory example is volatile. Existing split-store provider configuration can
select TiDB and local or object-store blocks.

Session limits are set in the optional `http.sessions` object. Every key is
optional, and a key left out keeps its default. Unknown keys and values
outside the range are rejected at startup, as is an `http.sessions` object
when `remote_sessions` is not `true`.

| Key | Default | Range | Meaning |
| --- | --- | --- | --- |
| `max_sessions` | 4096 | 1–65536 | Sessions the whole process admits, across every drive. |
| `max_sessions_per_drive` | 128 | 1–4096 | Sessions one drive admits. |
| `max_handles_per_session` | 128 | 1–4096 | Open handles one session may hold. |
| `idle_timeout_seconds` | 60 | 1–3600 | A session unused for this long is expired and its handles are closed. The server advertises the value and the remote CLI schedules its keepalive from it. |

```json
"sessions": {
  "max_sessions": 8192,
  "max_sessions_per_drive": 512,
  "max_handles_per_session": 256,
  "idle_timeout_seconds": 120
}
```

A session is admitted only when both the process budget and its drive's
limit have room, so the smaller of the two is what binds. Each session and
each open handle holds memory and, for a handle, backend state: raise the
limits with the process's memory in mind. On a forwarding cluster, each
owner applies the limits from its own config to the drives it owns.

The remote CLI owns keepalive. The command supervises
expiry and closes listeners/sessions before shutting down SDK owners. Session
drain uses `drain_timeout_ms`. A drain that fails still withdraws the Redis
route, closes the sessions and releases each drive's writer lease, so a
replacement process can take over at once; the exit status is 1. Each drive's
shutdown waits for the operations it admitted and fences the rest.

Multiple writable clients can use the same volume through any configured ingress. Clients need the public HTTPS endpoint and bearer credential. Servers use Redis for volume discovery and mutually authenticated HTTP/3 to forward requests to the owner.

For an ingress with no local drives, set `http.drives` to `[]` and configure `http.forwarding` as follows.

```json
{
  "peers": "peers.json",
  "certificate": "ingress.pem",
  "private_key": "ingress.key",
  "ca": "ca.pem",
  "redis_url": {"env": "MOUNT_RS_REDIS_URL"},
  "namespace": "mount-production",
  "bind": "0.0.0.0:0"
}
```

Distribute a trusted membership file to the servers. Each server uses its own certificate and key. Use the same Redis namespace across servers serving the cluster.

#### The Redis connection

`http.forwarding.redis_url` names an environment variable; the variable
holds the URL, so a password never appears in the config file. Each server
connects to the one endpoint in that URL.

| URL | Connection |
| --- | --- |
| `rediss://[user:password@]host[:port][/db]` | TLS. Use this for any Redis that is not on the same host. |
| `redis://[user:password@]host[:port][/db]` | Unencrypted. Allowed only to this host unless `allow_plaintext_redis` is set. |
| `unix:///path/to/redis.sock` | A local Unix socket. Accepted, but not covered by a test. |

The port defaults to 6379 and the database to 0. Anything else is refused at
startup, as is a URL that cannot be parsed.

| `http.forwarding` key | Default | Meaning |
| --- | --- | --- |
| `redis_url` | required | `{"env": "NAME"}`: the variable that holds the URL. |
| `redis_ca` | system roots | Path to a PEM file of CA certificates, relative to the config file. When set, only these certificates are trusted for the Redis server, and the URL must be `rediss://`. |
| `allow_plaintext_redis` | `false` | Set to `true` to permit `redis://` to a host other than this one. |

**TLS.** A `rediss://` server must present a certificate that chains to a
trusted root and that names the host or IP address written in the URL. The
roots are the operating system's unless `redis_ca` is set. Verification
cannot be turned off: a URL ending in `#insecure` is refused. `redis_ca` is
read once at startup, so replacing the CA needs a restart. Client
certificates are not supported; a Redis that requires them
(`tls-auth-clients yes`) cannot be used.

**Authentication.** Put the credentials in the URL: `rediss://:password@host`
for the default user, `rediss://user:password@host` for an ACL user.
Percent-encode reserved characters in either (`@` is `%40`, `/` is `%2F`,
`:` is `%3A`). The login is repeated on every reconnect. The registry keeps
one key per drive, `mount-rs:<namespace>:route:<drive>`, and this ACL rule is
enough (it is the one the TLS test uses):

```text
ACL SETUSER registry on >password ~mount-rs:* +eval +getrange +get +set +del +pexpire +strlen +ping +client|setinfo
```

**Plaintext.** `redis://` sends the password and the routes unencrypted.
It is accepted without further configuration only when the host is
`localhost` or a loopback address (`127.0.0.0/8`, `::1`). For any other
host, including a name that happens to resolve to this machine, the server
refuses to start (exit status 2) unless `allow_plaintext_redis` is `true`.
Set that only where the network between the server and Redis is itself
trusted or encrypted.

**Startup.** Before a server reports that it is listening it connects,
completes the handshake and the login, and runs one `PING`, all within five
seconds. A wrong CA, a wrong password, or a missing password on a server
that requires one is therefore a startup error (exit status 1) that says
which it was. A server cannot start while Redis is unreachable.

**Secrets.** The URL is never printed. Errors and logs name the endpoint as
scheme, host and port only (`rediss://redis.internal:6380`), with no user,
password, database or query, and text returned by the Redis client is
stripped of the password before it is reported.

#### When Redis is unavailable

Redis holds only the routes: which server owns which drive. It grants no
ownership and carries no file data. What an outage does, as measured by
`apps/mount-rs-cli/tests/redis_connection.rs` with real servers and Redis
cut off for 45 seconds (three route lifetimes), both by refusing connections
and by silently dropping them:

- **An owner keeps serving its own drives.** A request that arrives at the
  server that owns the drive does not use Redis. Reads and writes succeeded
  throughout.
- **Requests that must be forwarded fail with 503.** A request for a drive
  owned by another server needs a route. Cached routes last one second;
  after that each such request is answered with 503 within about two
  seconds, and nothing is queued. Clients should retry, or be sent to the
  owner.
- **Routes expire.** An owner refreshes its routes every five seconds and a
  route lives for 15 seconds, so an outage longer than that leaves Redis
  with no routes. A Redis that restarts empty is the same case; Redis
  persistence is not needed.
- **Recovery needs no restart.** An owner retries for as long as the outage
  lasts, at most two seconds apart, and re-registers each drive once Redis
  answers, after checking that it still holds the drive's writer lease.
  Routes were back 0.3 to 5 seconds after Redis returned, and forwarding
  resumed on the next request after that.
- **A connection that goes silent is replaced.** If three commands in a row
  time out on a connection, the next one opens a new connection, so a
  connection whose packets are being dropped does not hold a server back
  once Redis is reachable again.

An owner still stops, with exit status 1, if it loses the drive's writer
lease or finds that another server has registered the drive: those are
ownership changes, not Redis outages.

#### Running Redis highly available

A server connects to the single endpoint in its URL and reconnects to that
same endpoint, resolving the name again each time. Redis Sentinel and Redis
Cluster addresses are not supported: there is no Sentinel discovery and no
cluster redirection in the client as built.

To make Redis highly available, put it behind one stable name or address
that always reaches the current primary: a managed service's primary
endpoint, or a virtual address moved by your failover tooling. After a
failover the servers reconnect, and owners re-register any routes the new
primary does not have, exactly as after an outage. Forwarded requests fail
with 503 in between.

What is tested: one Redis 8.4, with TLS and without, stopped or cut off for
longer than the route lifetime and then restored, and every connection
killed by the server. What is not tested: a replicated pair failing over,
Sentinel, Redis Cluster, a proxy such as Envoy or HAProxy in front of Redis,
and reconnecting to an endpoint that still resolves to a demoted, read-only
replica.

#### The membership file

`http.forwarding.peers` names the membership file: a JSON array with one
record for every server in the cluster, ingresses included. Every server
should have the same file.

```json
[
  {
    "node": "owner-a",
    "boot": "owner-a-2026-11",
    "address": "10.0.1.11:7443",
    "tls_server_name": "owner-a.mount.internal",
    "spki_sha256": "3f0c…64 hexadecimal digits…9b1e"
  },
  {
    "node": "ingress-1",
    "boot": "ingress-1-2026-11",
    "address": "10.0.1.21:7443",
    "tls_server_name": "ingress-1.mount.internal",
    "spki_sha256": "a47d…64 hexadecimal digits…02c5"
  }
]
```

| Field | Meaning |
| --- | --- |
| `node` | The server's name, unique in the file. It must equal the node in the server certificate's URI SAN, `urn:agent-tunnel:peer:<node>`. |
| `boot` | An identifier for this deployment of the server. An owner publishes it in its routes, and a route is trusted only if it matches. |
| `address` | The UDP socket address an owner listens on for peer requests. It must not use port 0. An ingress never listens, but still needs a well-formed address. |
| `tls_server_name` | The name other servers verify in the server's certificate. |
| `spki_sha256` | The SHA-256 of the server's public key (DER `SubjectPublicKeyInfo`), as 64 hexadecimal digits. |

`node`, `boot` and `tls_server_name` are 1–256 characters from
`A-Z a-z 0-9 - _ .`. Unknown fields are rejected, and the file may list at
most 1,024 servers. Compute a pin from a private key with:

```sh
openssl pkey -in node.key -pubout -outform DER | openssl dgst -sha256 -hex
```

A peer is trusted only if its certificate chains to `ca`, names a `node` in
the file, and has that record's public key. Redis never supplies an address
or a key.

#### Reloading the membership file

A running server reads the membership file again, without a restart:

- when it receives `SIGHUP` (Unix), and
- every `http.forwarding.membership_reload_seconds` (default `30`, allowed
  `0`–`3600`), if the file's contents have changed. `0` turns the timer off,
  leaving `SIGHUP` as the only trigger.

A server with no `forwarding` block does not handle `SIGHUP`, unless a
drive's token is a file (see [Tokens](#tokens)).

The new file is checked in full before anything changes. If it cannot be
read or parsed, breaks a rule above, lists more servers than
`max_members`, or would cut this server off, the server keeps the
membership it had, keeps serving, and writes one line to stderr:

```text
mount-rs: membership reload refused, keeping the current 3 servers: membership record 2 has an invalid `address`
```

It does not log the same unchanged bad file again on each timer tick; a
`SIGHUP` always logs the result. An accepted reload logs
`mount-rs: membership reload: now 4 servers from /etc/mount-rs/peers.json; added ["owner-c"], removed [], changed []`.
Replace the file in one step (write a new file beside it and rename it over
the old one) so that a timer tick never reads a half-written file. A
half-written file is refused, not applied, and the complete file is picked
up on the next tick.

A server refuses a file that would stop it authenticating: one with no
record matching its own certificate's node and key. An owner also refuses a
file that changes its own `boot`, `address` or `tls_server_name`, since its
peer listener is bound to that address and its published routes carry those
values. Restart the server to change its own record.

Once a server has accepted a reload:

- A server **added** to the file is accepted and can be forwarded to from
  its next connection.
- A server **removed** from the file is refused on new connections and is
  no longer forwarded to. Connections already open with it, in either
  direction, are closed at once. A forwarded request in flight on one
  fails: the client gets a 502, or a cut-off body if the response had
  already started. A write caught this way may or may not have been
  applied, and is not retried. Stop a server, or move its drives, before
  removing it.
- A server whose **key changed** has the connections made with its old key
  closed at once, and must present the new key from then on. An old key
  does not keep working on a connection that was already open.
- Servers whose records did not change keep their connections.

`http.forwarding.max_members` (allowed `1`–`1024`) is the most servers a
reload may install. Each server sizes its peer connection limits from it at
startup, and they cannot grow while it runs. It defaults to 64, or to the
number of servers in the file at startup if that is larger. Startup fails
if the file lists more than `max_members`. To grow a cluster past the
value, raise it and restart each server, one at a time.

**Adding a server**

1. Issue the new server a certificate from the cluster CA with the URI SAN
   `urn:agent-tunnel:peer:<node>` and compute its pin.
2. Add its record to the membership file and distribute the file to every
   running server.
3. Send each running server `SIGHUP`, or wait one reload interval. Check
   stderr on each for the `added` line.
4. Start the new server with the same file. If it is started before the
   others have reloaded, requests for a new owner's drives get 503 and
   requests through a new ingress get 502 until they do; nothing needs
   restarting either way.

**Removing a server**

1. Stop the server, or move its drives to another owner.
2. Remove its record from the membership file, distribute it, and send each
   running server `SIGHUP` or wait one reload interval.

**Rotating a server's certificate**

A server reads its own certificate and key once, at startup, so the server
being rotated is restarted; no other server is.

- If the new certificate keeps the same key, its pin is unchanged. Install
  the certificate and restart that server. The membership file does not
  change.
- If the key changes:
  1. Issue the new certificate and key and compute the new pin. Leave the
     old ones in place for now.
  2. Stop the server. Its drives are unavailable until step 5.
  3. Install the new certificate and key on it, and change its
     `spki_sha256` in the membership file.
  4. Distribute the file and send every other server `SIGHUP`, or wait one
     reload interval. They close any connection made under the old key.
  5. Start the server with the new file.

  If the file reaches the other servers while the rotated server is still
  running with its old key, they stop accepting it and stop forwarding to
  it, and it refuses the file itself (its own record no longer matches its
  key) and logs that. It recovers when it is restarted with the new key.
  One record holds one key, so there is no period in which both keys are
  accepted.

On a volume owner, set `http.forwarding.owner` to `true`, supply the owner's certificate and key, and configure its assigned drives. The owner listens for peer requests at its membership address. The forwarding `bind` setting controls the outbound QUIC endpoint. Enable `http.remote_sessions` on the owner for remote mount clients. Ownership assignment is operator-configured unless failover is enabled (below). Serve the public endpoint through HTTPS termination for bearer credentials.

#### Drive failover

By default each owner opens the drives in its own `http.drives` at startup
and owns them until it stops; if it dies, they are unavailable until it is
restarted. Add a `failover` object to `http.forwarding` on two or more
owners that list the same drive to have the drive move by itself:

```json
"forwarding": {
  "owner": true,
  "failover": {}
}
```

Every drive of such a server is then a drive it is a *candidate* for. One
candidate holds the drive open and publishes its route; the others are
standbys that forward to it. When the route disappears (the owner crashed,
was partitioned or stopped), a standby opens the drive, which succeeds only
once the storage writer lease can be acquired, and then publishes itself.
The lease stays the only authority: Redis decides nothing about who may
write. Design, invariants and limits:
[`docs/serve-http-failover.md`](../../docs/serve-http-failover.md).

| Key | Default | Range | Meaning |
| --- | --- | --- | --- |
| `poll_interval_ms` | `1000` | `100`–`10000` | How often a standby reads the route and an owner checks its lease. |
| `takeover_stagger_ms` | `2000` | `0`–`60000` | A standby ranked `k` among the live candidates (rendezvous hash of drive id and node id) waits `k` times this before trying to take a drive. |
| `fail_back` | `false` | boolean | Hand a drive back to a better-ranked candidate that has returned. Off, a drive stays where it is until its owner stops. |
| `fail_back_after_seconds` | `60` | `10`–`86400` | With `fail_back`: how long that candidate must have ranked first without interruption. |

Unknown keys are rejected. `failover` requires `owner: true`, at least one
drive, and `splitstore` drives with exclusive ownership whose metadata is
not `memory`.

Requirements and behaviour:

- All candidates for a drive must configure the **same storage** (metadata
  store and volume, block store) and the same token and `remote_sessions`
  setting. Nothing checks this: candidates pointed at different storage
  would each serve their own copy in turn.
- Recovery after a crash takes at most
  `max(15 s + rank x takeover_stagger_ms, lease_ttl_ms) + 5 x poll_interval_ms`
  plus the time to open the drive: about four minutes with the default
  240-second lease, about 25 seconds with `lease_ttl_ms: 20000`. A clean
  stop (SIGTERM) hands over in a second or two.
- While no owner is published, requests get `503` with
  `x-mount-dispatch: not-dispatched` and `Retry-After: 1`. Requests in
  flight on the failed owner get `502` with `x-mount-dispatch: unknown` and
  are not replayed. Remote sessions and open handles on the failed owner are
  lost: the new owner answers `403 EACCES` and the client must open a new
  session.
- A candidate does not exit when a drive's lease is held elsewhere or the
  drive cannot be opened; it stays a standby and reports each change as a
  `drive_failover` JSON line on stderr.
- A Redis outage moves nothing: standbys wait, owners keep their leases and
  keep serving requests that reach them directly.

`scripts/serve-http-cluster-e2e.py` starts two owners on loopback against a
disposable Redis and checks the cluster as a round-robin load balancer would
use it: every drive through every ingress, read-after-write across ingresses,
uploads at and over the size limit, sessions carried by either ingress, an
owner stopping and restarting, and a membership file reloaded on the timer
and on `SIGHUP` to add and then remove an ingress, and a drive with two
failover candidates whose owner is killed and then stopped cleanly. CI runs it. A request whose cached route names
an owner with no live peer connection re-reads the route before dialing, so a
cleanly stopped owner's drive answers 503 at once rather than after the QUIC
handshake timeout.

`http.forwarding.refresh_active_routes` rechecks recently used routes in Redis before their one-second cache lifetime expires, so requests to an active drive don't wait for a Redis lookup each time the cached route expires. This option defaults to `true`; set it to `false` to look routes up only when a request finds the cached one expired. On a GitHub Actions runner with 100 drives across three nodes, refresh lowered forwarded metadata reads from a p99 of 0.80–0.88 ms to about 0.72 ms, at the cost of about one Redis read per active route per second. Each refresh verifies the exact registration and stops after `http.forwarding.route_refresh_idle_ttls` route lifetimes without use (default `5`, allowed `1`–`60`). The route lifetime and its hard expiry are unchanged by this setting. Owners still check the route and their current fencing generation before serving forwarded requests.

Forwarded requests have no overall time limit once connected: an upload or
download through an ingress may take as long as it keeps moving, provided
each piece arrives within 30 seconds of the last. File uploads are forwarded
up to `max_upload_bytes`; other request bodies up to `max_request_bytes`.

`http.forwarding.peer_connections` (default `4`, allowed `1`–`16`) sets how
many QUIC connections an ingress keeps to each owner. Each connection has its
own UDP socket, and requests take them in turn. One QUIC connection holds a
lock while it sends, so it tops out at a few hundred MB/s; in a 5-core Linux
container, forwarded 1 MiB reads went from 219 MB/s with one connection to
524 MB/s with four and 584 MB/s with eight. The membership file may list up to
1,024 servers. Each server sizes its peer connection limits from
`max_members`: an ingress may hold one pooled connection per member per
`peer_connections`, and an owner accepts up to 16 connections for each member
it has room for.

For many TiDB volumes on an owner, `http.tidb_metadata_pool_limit` sets a shared SQL connection limit for volumes that use the same connection environment variable name. The 100-volume local benchmark used 16.

Session JSON control requests and write bodies also respect `max_request_bytes`,
including chunked uploads. Oversized requests are rejected before mutation.
The session advertises the smaller of that limit and its 64KiB protocol I/O cap;
the current remote client honors smaller advertised limits and may return short
reads/writes accordingly. Very small limits can also reject control operations
whose JSON exceeds the configured bound.


Set `http.cache_report_interval_seconds` to a positive integer to have
`serve-http` write periodic JSON records to stderr for drives with a configured
content cache. Reporting defaults to off. Each `mount_cache_stats` record
contains the configured drive ID and separate `ram` and `disk` snapshots.
An absent tier is JSON null. Hits, misses, and evictions are cumulative for
the open drive. Use differences between successive snapshots from the same
process to measure a workload interval. A RAM miss can be a disk hit. With
both tiers enabled, the disk misses identify lookups that fell through the
local caches. Resident entries and bytes describe each tier at sampling time.

The RAM snapshot also reports completed and failed gets to its next storage tier, bytes returned by those gets, and their cumulative duration in nanoseconds. For a workload interval with completed gets, divide the duration delta by the completed-get delta for the mean duration. A configured disk cache is the next tier below RAM, so these counters can include disk hits.

## Remote Drive provider

`mount-rs mount --config <path>` accepts a remote provider that selects one Partition and mounts several granted Drives through the existing native adapters. `serve-remote --config <path>` starts the authenticated QUIC service; `catalog-apply --config <path>` updates its persistent metadata definitions and grants using a revision check. Tokens come from a file or an argv command. See [Remote Drives](../../docs/remote-drives.md) for setup, examples and qualification boundaries.
