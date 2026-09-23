# `@mount-rs/core` native structural-driver acceptance

The N-API surface accepts a structural JavaScript `FsDriver` in the server
factories and in `mount()`. `createDriver()` is the explicit adapter when a
caller wants a native `Filesystem` value for other APIs. The native mount
binding retains the plain structural object for the full mount lifecycle; it
does not take ownership of the caller's driver or call its `shutdown()`.

`test/structural-native.mjs` covers both boundaries:

- a portable adapter smoke test performs read/write I/O through
  `createDriver()`;
- malformed structural input is rejected before a host mount is attempted;
- an opt-in macOS/Linux test passes the plain structural object directly to
  `mount()`, reads a seeded file through the kernel mount, writes and reads it
  back, verifies callbacks were reached, and checks unmount cleanup.

The structural adapter bridges both string open modes and decoded numeric
`open_flags`, so native host writes are part of this acceptance check rather
than a portable-only capability. The test also verifies that the bytes written
through the mount are visible through the original backing driver after
unmount.

The host-mount portion is deliberately opt-in because it requires a working
native transport and host mount privileges. The ordinary check is safe and
rootless:

```sh
node bindings/mount-rs-napi/test/structural-native.mjs
```

To exercise the real lifecycle on macOS or Linux:

```sh
MOUNT_RS_NAPI_STRUCTURAL_NATIVE_MOUNT=1 \
  node bindings/mount-rs-napi/test/structural-native.mjs
```

On Linux, the CI acceptance path installs `fuse3`, verifies `/dev/fuse` and
`fusermount3`, then forces the probed `fuse` transport. The equivalent local
command is:

```sh
MOUNT_RS_NAPI_STRUCTURAL_NATIVE_MOUNT=1 \
MOUNT_RS_NAPI_STRUCTURAL_TRANSPORT=fuse \
  node bindings/mount-rs-napi/test/structural-native.mjs
```

`auto` uses the platform probe. A named transport can be selected when the
host is configured for it:

```sh
MOUNT_RS_NAPI_STRUCTURAL_NATIVE_MOUNT=1 \
MOUNT_RS_NAPI_STRUCTURAL_TRANSPORT=nfs \
  node bindings/mount-rs-napi/test/structural-native.mjs
```

Linux may use `fuse`, `9p`, or `nfs`; macOS currently uses its native NFS
client. The test refuses an explicitly requested transport whose probe says it
is unavailable and includes the probe's prerequisite reason in the failure.
If mount or teardown fails, the mountpoint is printed and is not recursively
removed, so an active host mount is never hidden by cleanup.

For a live shared NFSv3 view, pass `{ transport: "nfs", nfsSharedView: true }`
to `mount(filesystem, mountpoint, options)`. With `transport: "auto"`, the shared
view setting selects NFS. Explicit FUSE or 9P requests and the local-only
`nfsSqliteSingleHost` lock profile are rejected. This enables guarded server reads
and mutations and disables the native client's metadata and name caches (`noac`
and `nonegnamecache` on macOS; `noac` and `lookupcache=none` on Linux). The
backend must support identity-guarded reads and mutations; unsupported drivers fail
before the mount starts. A direct `createNfsServer(filesystem, { sharedView:
true })` selects the same server policy. Callers mounting that server through
an external NFS client must configure those client cache options themselves.

## FoundationDB chunked provider

The chunked Node factory accepts a foundationdb provider when the N-API crate
is built with its opt-in native feature:

    cargo check -p mount-rs-napi --features foundationdb

Build the local Node addon with that feature using the package script:

    MOUNT_RS_NAPI_FEATURES=foundationdb pnpm --dir bindings/mount-rs-napi build:debug

On macOS, supply a compatible native FoundationDB client library through
`FDB_CLIENT_LIB_PATH` for linking and `DYLD_LIBRARY_PATH` if its runtime path
is not already in the dynamic loader search path.

Use `uri` for the cluster-file path and `key` for the volume prefix. Two
independent clients that write the same volume use FoundationDB metadata with
`leaseAuthority: "revision-cas"`, `concurrentWrites: true`, and a shared durable
block provider such as FoundationDB or R2. Both clients must use the same
metadata volume key and block store. For example:

    await createChunkedDriver({
      metadata: { kind: "foundationdb", uri: clusterFile, key: "files", leaseAuthority: "revision-cas", durable: true },
      blocks: { kind: "foundationdb", uri: clusterFile, key: "file-blocks", leaseAuthority: "revision-cas", durable: true },
      chunkSize: 4096,
      concurrentWrites: true,
    })

`concurrentWrites: true` also accepts SQLite, PGlite or TiDB metadata. For SQLite,
both Node processes must run on one host and use the same durable local
metadata and block file paths; network file systems are unsupported. PGlite
clients must reach one shared socket server and use the same volume keys.
TiDB clients must reach the same TiDB metadata volume and shared block backing;
TiDB metadata can be paired with TiDB or RustFS blocks across independent clients.
The separately named `rustfs` block backend takes `endpoint`, `bucket`,
`region`, `key` (block prefix), `accessKeyId`, and `secretAccessKey`. Pair it
with PGlite, TiDB or FoundationDB metadata for a shared cross-host backing design;
physical cross-host mounts have not yet been verified.
RustFS alone has no metadata revision authority. The endpoint must be an
HTTP(S) authority with at most one trailing slash; an omitted `durable`
defaults to false and `durable: true` is a caller assertion about the service.
Memory blocks are rejected,
and SQLite blocks are accepted only with SQLite metadata on one host.

The volume records its writer mode; keep all clients on the same mode.
Concurrent mode is experimental: block reconciliation is disabled, and
deleted file tombstones and superseded blocks remain stored until a safe
distributed reclamation protocol is available. Size long-lived volumes with
that retained data in mind.
Exclusive-writer volumes use `leaseAuthority: "persisted-single-authority"` in
an owned test cluster, or `leaseAuthority: "shared-provider"` with a protected
`authorityPrefix` in a deployed cluster. In shared-provider mode, each worker
needs read-only access to the provider-time record and only the authority
service may publish it. The addon does not create that credential boundary.
The provider retains the process-scoped FoundationDB client network until its
filesystem handles are dropped.
At the Node application's terminal process boundary, after all FoundationDB
filesystems have completed `shutdown()`, call the synchronous
`shutdownFoundationdbClientNetwork()` export. It stops and joins the native
client network; `EBUSY` means a provider handle is still live and the stop can
be retried after closing it. A feature-off addon exposes the same function and
returns `ENOTSUP`.

The live Node gate is intentionally opt-in:

    MOUNT_RS_NAPI_FOUNDATIONDB=1 \
    MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE=/path/to/fdb.cluster \
      node bindings/mount-rs-napi/test/foundationdb.mjs

If R2_ENDPOINT, R2_BUCKET, R2_ACCESS_KEY_ID, and R2_SECRET_ACCESS_KEY are
present, the gate composes FoundationDB metadata with R2-compatible blocks;
otherwise it exercises durable FoundationDB metadata and block stores, including
readback after reopening a new filesystem instance.
The native feature build requires the host FoundationDB client library for
linking, and a live cluster is required for the runtime gate. The repository
CI lane builds that artifact from the pinned FoundationDB client image and
runs this test in a Node 24 container on the same Docker network. That hosted
lane publishes a shared authority sample first and selects `shared-provider`
with the published `authorityPrefix`; local invocations default to the
explicit persisted single-authority/test mode.

## Explicit immutable-block reconciliation

Chunked N-API filesystems that have an enumerable block provider expose
`filesystem.reconcileBlocks(graceMs)`. The call is an explicit operator or
maintenance action: it renews the filesystem writer lease, derives live block
roots from the committed namespace and open-unlinked handles, and asks the
provider to delete only valid, aged, unreferenced objects in its configured
prefix. The returned `{ scanned, protected, recent, deleted }` report is
bounded to that provider scope.

`graceMs` must be a positive integer. The grace period is a safety boundary for
in-flight or ambiguous metadata publication; it is not a retention policy and
reconciliation is never run implicitly by `shutdown()`. Providers without a
scoped object enumerator return `ENOTSUP` and must be reconciled by their own
provider-native process if they support that operation. Customers must schedule
the action with the Ozone/operator control plane, retain enough history to meet
their recovery policy, and monitor the report and provider space pressure.

Directory listings that cross a remote HTTP boundary can use
`filesystem.readdirBounded(path, maxEntries)`. The bound is enforced by the
driver before the result is returned; providers without a provider-side
enumeration limit fail closed with `ENOTSUP`. An unstorage store may opt into
this path with `getKeysBounded(prefix, maxKeys)`, which must return no more than
`maxKeys + 1` keys so the adapter can distinguish a complete listing from an
overflow signal. The legacy `getKeys` callback alone is intentionally not used
for bounded remote listings.

Structural JavaScript `FsDriver` providers can opt into the same WebDAV-safe
path with `readdirBounded(path, maxEntries)`. `createDriver` forwards the
ceiling to that callback and rejects a callback result larger than the ceiling
as `EOVERFLOW`; omitting the optional callback retains the explicit
`ENOTSUP`/`501` capability boundary. The provider callback must enforce the
bound before materializing and returning its listing.

## Optional observability

The native crate has an opt-in `observability` feature that decorates every
native, structural-JavaScript, memory-factory, and unstorage filesystem driver
at the Node application boundary. The feature is disabled by default and does
not change the exported API or add exporter setup to the default build.

```sh
MOUNT_RS_TELEMETRY=1 node-gyp-build bindings/mount-rs-napi
```

When the feature is enabled, set `MOUNT_RS_TELEMETRY=1` before loading the
module to activate the bounded operation spans and local metrics. A Rust host
embedding this crate may install an application-owned telemetry handle before
constructing a filesystem. Exporter/provider lifecycle remains owned by that
host; Node operation failures are never made dependent on telemetry export.

For a TLS-required TiDB endpoint, build the addon with the `rustls` feature so
the TiDB client TLS implementation is included:

```sh
MOUNT_RS_NAPI_FEATURES=rustls pnpm --dir bindings/mount-rs-napi build
```

This is a build capability, not live TLS/provider acceptance. The deployment
still owns the TiDB URL, CA/certificate policy, secret injection, rotation and
the credentialed handshake test.
