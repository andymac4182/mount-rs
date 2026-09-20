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

    mount-rs validate-config --config crates/mount-rs-cli/examples/config-memory.json
    mount-rs mount --config crates/mount-rs-cli/examples/config-splitstore.json

validate-config performs only JSON/schema and static option validation. It
does not open SQLite or PGlite, resolve credential values, construct an R2
client, or make a network request. Provider construction starts only after
the mount command has resolved the config.

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
provider is strict and supports memory, sqlite, and pglite; r2 is supported
for blocks only because the current integration exposes no R2 metadata store.
PGlite and R2 credentials are environment references such as
{"env":"R2_SECRET_ACCESS_KEY"}, never plaintext values. See
examples/config-pglite-r2.json for the block-only R2 shape.

Explicit command-line flags override only the config fields they name.
Unspecified flags retain config values. Relative config paths are resolved
relative to the config file. An omitted splitstore owner gets a
process-and-instance-specific writer-fence owner; explicit owners are
validated before provider construction.

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
