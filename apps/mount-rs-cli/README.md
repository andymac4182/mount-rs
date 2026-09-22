# mount-rs CLI

mount-rs is the native command-line front end for this repository. Its
default is intentionally close to the pinned mountx CLI: an in-memory
filesystem with a README, a request watcher, and one native mount chosen by
the auto facade.

The command accepts one positional mountpoint:

mount-rs [mountpoint] [options]
mount-rs mount [mountpoint] [options]
mount-rs probe
mount-rs validate-config --config PATH
mount-rs sdk-self-test [--config PATH] [--reopen]

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

Structured storage has independent metadata and blocks providers. Each
provider is strict and supports memory, sqlite, pglite, tidb, and
FoundationDB; r2 is supported for blocks only because the current integration
exposes no R2 metadata store. PGlite and TiDB connection URLs plus R2
credentials are environment references such as
{"env":"R2_SECRET_ACCESS_KEY"}, never plaintext values. FoundationDB uses a
resolved cluster_file path and requires the explicit `lease_authority` setting.
Use `"persisted-single-authority"` only for an owned single-authority/test
path. Production consumers should use `"shared-provider"` together with an
`authority_prefix`, and grant the CLI read-only access to that authority
record; the authority service alone may publish provider time. Enable the
CLI's foundationdb feature on a supported native target; the default CLI
remains portable and fails closed when that native provider is selected
without the feature. See
examples/config-pglite-r2.json for the block-only R2 shape,
examples/config-tidb-rustfs.json for the TiDB metadata/RustFS block shape, and
examples/config-foundationdb-rustfs.json for FoundationDB metadata with
RustFS-compatible blocks.

TiDB metadata can be composed with RustFS or another S3-compatible endpoint
through the `r2` block provider. The TiDB `connection` and `volume_key` are
passed to the provider, and `durable` remains an explicit caller assertion;
the URL alone does not prove replicated TiKV durability. The opt-in provider
matrix uses this shape for configuration, shutdown/reopen, partial-write,
truncate, and owned-prefix cleanup checks when live TiDB and RustFS
credentials are available.

Explicit command-line flags override only the config fields they name.
Unspecified flags retain config values. Relative config paths are resolved
relative to the config file. An omitted splitstore owner gets a
process-and-instance-specific writer-fence owner; explicit owners are
validated before provider construction. Splitstore also accepts the optional
`lease_ttl_ms` field, in milliseconds; it defaults to 30 seconds. Set it
explicitly for remote providers when the observed operation latency requires
more lease headroom, while keeping stale-writer recovery within the
deployment's recovery objective.

## Unmounted HTTP service

Run the HTTP service with a dedicated configuration file:

    mount-rs serve-http --config apps/mount-rs-cli/examples/config-http.json

An HTTP config has the same top-level `version` as native configs and an
`http` object instead of native mount fields. The `http.drives` array is
required and must contain at least one named drive. Every drive has an `id`, a
bearer-token environment reference, and one of the existing `driver` shapes
(`memory`, `host`, `sqlite`, or `splitstore`). Relative paths are resolved
relative to the HTTP config file, just as they are for native configs. The
service defaults to loopback (`127.0.0.1`), an ephemeral port (`0`), bounded
request/chunk sizes, 256 active connections, a 30-second request timeout, and
a five-second shutdown drain. Directory listings are capped at 4,096 entries
and 8 MiB by default; all values can be overridden in the `http`
object. Only loopback hosts are accepted; remote customers must terminate TLS
and authenticate at a reverse proxy before forwarding to this listener.

The token value is read from the named environment variable only when the
service starts and is never printed or stored in the config. Each token is
authorized only for its configured drive. The service is unmounted: it does
not create a FUSE, 9P, or NFS mount, and it does not add a distributed cache.
The readiness line reports the selected address and drive IDs. Ctrl-C closes
HTTP connections within the configured drain bound and then shuts down the
configured providers.

See `examples/config-http.json` for a memory drive and a durable SQLite drive
with isolated tokens.

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
curl -H 'Authorization: Bearer demo-memory' \
  -X PUT --data-binary 'hello from mount-rs' \
  http://127.0.0.1:PORT/v1/drives/memory/fs/hello.txt
curl -H 'Authorization: Bearer demo-memory' \
  http://127.0.0.1:PORT/v1/drives/memory/fs/hello.txt
curl -H 'Authorization: Bearer demo-sqlite' \
  -X PUT --data-binary 'durable split-store data' \
  http://127.0.0.1:PORT/v1/drives/sqlite/fs/persisted.txt
```

The SQLite drive uses independent metadata and block databases under the
example's `state/` directory. Stop and restart the service, then GET the same
`persisted.txt` path to verify reopen persistence; the memory drive is expected
to reset. The CLI creates missing SQLite parent directories, and generated
demo state should be removed when finished.

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
`/proc/self/mounts` after exit. macOS has a separate opt-in NFS lifecycle
check. With the built-in `/sbin/mount_nfs` available and the terminal/process
already allowed by its ownership and Privacy policy, run:

    MOUNT_RS_CLI_NATIVE_NFS=1 cargo test -p mount-rs-cli --test native_lifecycle -- --ignored --nocapture

The macOS check writes an extension-free config file for the actual binary,
mounts the configured host driver through native NFS, writes and reopens
bytes, sends SIGINT, verifies the unmount, then remounts the same config and
backing directory in a fresh child to verify persistence before removing the
test file. It is ignored in ordinary test runs, but explicitly running the
ignored test without `MOUNT_RS_CLI_NATIVE_NFS=1` fails rather than silently
passing. The test never invokes `sudo`, installs a helper, or changes host
configuration. This host-backed lifecycle pass does not satisfy the separate
SQLite native-NFS acceptance requirement. The SQLite case must use an explicit
driver.uid/gid pair and record the SQLite version, journal mode, synchronous
setting, locking/recovery results, and cleanup evidence. The host-backed case
keeps the NFS server's backing-directory ownership aligned with the
unprivileged macOS kernel client.

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
