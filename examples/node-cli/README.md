# mount-rs Node SDK CLI example

This example uses the public `@mount-rs/core` Node SDK to select a Rust-backed
filesystem driver, mount it through the Rust transport facade, and access the
mounted path with ordinary Node `fs/promises` calls.

Validate arguments without loading native code or mounting:

```sh
node examples/node-cli/index.mjs \
  --driver memory \
  --transport nfs \
  --mountpoint /tmp/mount-rs-node-cli \
  --check
```

Run the SDK-backed driver example without a native mount. This is the portable
consumer smoke test used by the integration suite:

```sh
node examples/node-cli/index.mjs \
  --driver memory \
  --sdk-self-test
```

The Node CLI also accepts the same versioned provider configuration shape as
the Rust CLI. This opens the configured providers through the public N-API
SDK, writes and reads a file, shuts down, then recreates the driver and checks
the persisted readback:

```sh
PGLITE_DATABASE_URL='postgresql://postgres:postgres@127.0.0.1:5432/postgres?sslmode=disable' \
node examples/node-cli/index.mjs \
  --config /path/to/pglite-config.json \
  --sdk-self-test --reopen
```

For a live run, use a config whose `driver.storage` has PGlite metadata and
either PGlite or R2 blocks. The provider matrix generates this temporary
config during its PGlite run. Credentials remain environment references in
the JSON; the CLI passes their resolved values only to the public SDK factory.
The optional `driver.storage.lease_ttl_ms` field selects the writer-lease TTL
in milliseconds; it defaults to 30 seconds and should be chosen to cover the
deployment's observed provider latency without delaying stale-writer recovery.
Use `--check` to validate a config without loading native code or resolving
credentials.

For FoundationDB, the same config shape uses `cluster_file`, `volume_key`, and
an explicit `lease_authority` in a metadata or block store. The example calls
the SDK's synchronous `shutdownFoundationdbClientNetwork()` once at its
terminal boundary after shutting down every configured filesystem; a native
addon built without the `foundationdb` feature returns `ENOTSUP`.
Build the local addon with
`MOUNT_RS_NAPI_FEATURES=foundationdb pnpm --dir bindings/mount-rs-napi build:debug`
before running that config.

Structured storage accepts `driver.storage.ownership_mode: "exclusive"` or
`"shared"`. Explicit exclusive ownership enables fenced writeback, drained on
sync and orderly shutdown. Shared ownership enables the separate MRC3 directory
delegation protocol; supply `driver.storage.checkout_path: "/database"` to
claim the existing directory before mounting. Bootstrap database directories
on a new volume through a direct driver session checked out at `/`, then check
that root scope back in before assigning individual directories. A supplied
`concurrent_writes` must agree with the mode (`false` for exclusive, `true` for
shared). Omitting the mode preserves legacy write-through behavior and the
MRC2 `concurrent_writes` protocol. Existing volumes require explicit offline
MRC3 enrollment through the Rust SDK; a new empty store initializes directly.

Explicit shared native mounting requires Linux FUSE: `auto` selects `fuse`,
and `nfs`/`9p` are rejected. To hand off ownership, stop the SQLite application,
close all connections, unmount and complete driver shutdown/checkin before
creating a fresh mount. This CLI does not revoke caches on a live kernel mount.
Keep the database and its WAL, shared-memory and journal sidecars inside the
same owned directory. No timed takeover or distributed POSIX locks are implied.

For two independent writable CLIs, set `driver.storage.concurrent_writes: true`.
The example selects the NFS shared-view profile and accepts SQLite, PGlite,
or FoundationDB `revision-cas` metadata. SQLite metadata and blocks require
the same local disk paths on one host. Place both SQLite database files outside
the native mountpoint; the CLI rejects concurrent SQLite backing paths inside
the mountpoint before it opens a driver, including symlink aliases and paths
with missing components. On case-insensitive macOS filesystems, `mnt` and `MNT`
refer to one directory. Before opening a driver, the CLI creates the native
mountpoint and compares its filesystem identity with each existing SQLite
backing ancestor, including symlink targets. Under `--check`, it removes any
empty mountpoint directories it created for this probe. PGlite clients require
one reachable socket server.
RustFS is a separately named block-only provider; its JSON
configuration uses `kind: "rustfs"`, an endpoint, bucket, signing region,
prefix, and environment references for both credentials. Pair RustFS with
PGlite or FoundationDB metadata for hosts sharing one volume. RustFS
`durable` defaults to `false`; set it to `true` only when the service's
durability is assured.

Run the bounded self-test on a host with a usable native transport:

```sh
node examples/node-cli/index.mjs \
  --driver memory \
  --transport nfs \
  --mountpoint /tmp/mount-rs-node-cli \
  --self-test
```

Use `--driver host --root PATH` for a rooted host-backed view. `--transport`
accepts `auto`, `fuse`, `9p`, or `nfs`; a named transport is attempted once and
does not silently fall back. The repository checkout resolves the local N-API
addon at `bindings/mount-rs-napi/index.js`. A published install can resolve
`@mount-rs/core`, or override resolution with `MOUNT_RS_NAPI_PACKAGE`.

The CLI never accepts plaintext provider credentials in its JSON; configured
providers refer to environment variables and runtime use passes those values
directly to the public SDK. Native mounting is an explicit operation;
`--check` remains mount-free and does not resolve credentials, while the SDK
self-test exercises the public driver API without a transport. The integration
test runs both paths.

## Native Node CLI integration

The bounded native example launches this exact CLI as a separate process,
checks the host mount table, uses an independent Node process to read and write
through the mounted path, sends `SIGINT` so the CLI performs its own unmount,
and verifies the written bytes in the host-backed root after unmount:

```sh
MOUNT_RS_NODE_CLI_NATIVE_INTEGRATION=1 \
  node examples/node-cli/native-integration.mjs
```

It is deliberately opt-in. Without the environment variable it prints
`SKIP`, not `PASS`. On macOS it requires the repository's NFS client probe to
report usable and exercises native NFS. On Linux it requires the FUSE probe,
`/dev/fuse`, and a usable `fusermount3`/`fusermount` path and exercises native
FUSE. Other platforms and unavailable prerequisites are explicit skips; an
opted-in mount failure is a failure. This example uses only a temporary local
host driver, so it does not require provider credentials and does not claim
R2/PGlite coverage.

Build the local N-API addon first, or set `MOUNT_RS_NAPI_PACKAGE` to a package
that exports the same public SDK. The temporary directory is preserved when a
mount cannot be proven unmounted so it can be recovered manually.

Explicit `exclusive` and `shared` ownership modes are available for chunked split stores. Exclusive mode enables deferred publication with durable synchronization; omitted modes preserve legacy behavior. See [mount ownership contracts and configuration](../../docs/mount-ownership.md).
