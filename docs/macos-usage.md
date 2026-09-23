# Use mount-rs on macOS

The storage provider and the native mount are separate parts of mount-rs.
`sdk-self-test`, `probe`, and the RustFS/FoundationDB Docker harnesses do
not leave a volume in macOS Finder. The CLI's native macOS path serves the
selected filesystem over its in-process NFSv3 server and mounts it with the
built-in `/sbin/mount_nfs` client. Keep the `mount-rs mount` process running
while using the mounted directory.

## Open a mount in Finder

Run from the repository root in one Terminal window:

```sh
demo_tmp_root="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
mount_dir="$(mktemp -d "$demo_tmp_root/mount-rs-finder.XXXXXX")"
printf 'Finder path: %s\n' "$mount_dir"
./scripts/cargo-shared run --locked -p mount-rs-cli -- \
  mount "$mount_dir" --transport nfs --driver memory
```

The temporary directory is empty, writable, and owned by the current user.
Wait for the CLI's `mounted nfs at ...` line. The default memory filesystem
contains `README.md`. In Finder, press **Shift-Command-G** (Go > Go to Folder),
paste the exact absolute path printed by the CLI, and press Return. A separate
Terminal window can check the same path with `ls -la /absolute/path/from-cli`
and confirm the live NFS entry with `mount`.

The macOS NFS client is configured with `nobrowse` by default. This hides
the mount as a separate GUI volume; Go to Folder still opens its exact path.
The mountpoint is the directory passed to the CLI, which may be in
`$TMPDIR`, not `/Volumes`. Leave that directory in any other Terminal window
before stopping the mount. Press **Ctrl-C** in the CLI window to
unmount. Wait for `unmounted` before removing the empty temporary directory.
Run `rmdir "$mount_dir"` once the NFS entry is gone. Memory filesystem
contents disappear when that CLI process ends.

If `probe` reports NFS as usable but the mount command fails, read the
mount error. On macOS the user must own the empty mountpoint. Privacy policy
may require Full Disk Access for the Terminal or process that runs the CLI.
The probe checks host prerequisites; the `mounted nfs at ...` line and a
mounted-path read are the runtime checks.

## Two writable views of one filesystem

For two views managed by **one CLI process**, make two distinct, empty,
user-owned directories and add a second NFS mountpoint:

```sh
demo_tmp_root="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
demo_dir="$(mktemp -d "$demo_tmp_root/mount-rs-two-views.XXXXXX")"
mkdir "$demo_dir/view-a" "$demo_dir/view-b"
./scripts/cargo-shared run --locked -p mount-rs-cli -- \
  mount "$demo_dir/view-a" --also-mountpoint "$demo_dir/view-b" \
  --transport nfs --driver memory
```

`--also-mountpoint` can be repeated. The CLI opens the driver once and
serves each mountpoint through a separate native NFS view. Both paths are
writable and refer to the same in-process filesystem state. Close or
synchronize a written file before checking it through another view. Keep
the CLI running while using either path; Ctrl-C unmounts every view before
the driver shuts down. In Finder, use **Shift-Command-G** with each exact
absolute path. Native NFS uses `nobrowse`, so Finder does not list these as
separate volumes. This one-process example uses memory; it can also use a
configured `splitstore` driver. Shared NFS views require atomic inode identity
guards for both reads and mutations, which `MemoryFs` and `ChunkedFs` provide.
The CLI checks both capabilities before starting a shared NFS mount and rejects
`--also-mountpoint` for `--driver host` and the legacy `--driver sqlite`
snapshot facade with a clear startup error; either still works for a single
mountpoint. An NFS file handle remains tied to its original inode after a
rename, and handle-derived changes check that identity inside the filesystem
mutation. Inode numbers are stable for the supported shared filesystems, so
an old handle can find the renamed path when this server learns its new name.
Directory `LOOKUP` and `READDIR` check the remembered parent inode during the
read: a path previously used by that directory may now name a different inode
after another mount renames it.
For shared mounts, `--transport auto` selects NFS when available; an explicit
FUSE or 9P selection fails. The `--sqlite-single-host` profile uses local
SQLite locking and cannot be combined with shared NFS views.

### Two CLIs using one local SQLite backing

On one Mac, two independent CLIs can use the same file-backed SQLite
metadata and block databases. Start with the
[concurrent SQLite/SQLite example](../apps/mount-rs-cli/examples/config-sqlite-concurrent.json):
set both database paths to absolute paths on local storage **outside** either
mount, use the same paths in both processes, and choose two distinct, empty,
user-owned mountpoints. The example enables `concurrent_writes` on a fresh
backing. Save the adjusted JSON as `/absolute/path/to/shared.json`, then run
these commands in separate Terminal windows:

```sh
./scripts/cargo-shared run --locked -p mount-rs-cli -- \
  mount --config /absolute/path/to/shared.json \
  --mountpoint /absolute/path/to/view-a
./scripts/cargo-shared run --locked -p mount-rs-cli -- \
  mount --config /absolute/path/to/shared.json \
  --mountpoint /absolute/path/to/view-b
```

Use `--also-mountpoint` with one CLI if both views can share one
process. This mode requires all writers to run on the same host; do not place
the provider databases on NFS or SMB. It does not use the single-mount
`sqlite_single_host` NFS profile. The CLI checks both SQLite paths against
the primary mountpoint and every `--also-mountpoint` before opening the driver.

The local native two-CLI test has passed with SQLite DELETE and WAL backing
smoke checks. WAL is not yet qualified: the bundled SQLite 3.46 is affected
by SQLite's rare [WAL-reset bug](https://www.sqlite.org/wal.html#the_wal_reset_bug),
which is fixed in 3.51.3 and selected backports. Use DELETE backing for now.
An SQLite *application database inside* two NFS mount views is a different
case: the [adversarial macOS probe](../tests/sqlite_nfs_adversarial.md)
observed a second view acquire `BEGIN IMMEDIATE` while the first held an
uncommitted update. That contender rolled back without writing. A WAL request
through the NFS view fell back to DELETE. These shared NFS views do not
provide safe SQLite application-file locking; SQLite also documents the
[network filesystem locking risk](https://www.sqlite.org/useovernet.html).

### Two CLIs using one PGlite server

Start one PostgreSQL-wire PGlite server and connect both CLIs to it using
the same `PGLITE_DATABASE_URL`, volume key, and block backing. The server can
listen on a Unix socket or TCP; the local native two-CLI test used TCP
loopback `127.0.0.1` to one engine. If both metadata and blocks use PGlite,
two CLIs open four provider connections, so configure `maxConnections` to at
least 4. The native test also checks that insufficient connection slots fail
closed. PGlite metadata may instead use a shared RustFS block store, as in
the [PGlite/RustFS concurrent example](../apps/mount-rs-cli/examples/config-pglite-rustfs-concurrent.json).
Set a fresh volume key, a durable `PGLITE_DATA_DIR` if persistence is needed,
the same provider settings in both CLIs, and different mountpoints. Local
two-CLI behavior has been exercised; a listener reached from another host
and physical cross-host mounts have not been verified.

### Two CLIs using FoundationDB

The following **experimental** two-process FoundationDB recipe passed the
final guarded-source macOS native two-CLI acceptance 1/1 on 2026-09-23. A
distributed handle-pin and capacity protocol is still pending. For
two independent CLI processes writing one FoundationDB volume, use
a *fresh, unused* FoundationDB `volume_key` and a cluster reachable by both
processes. Both processes must have the same provider configuration and a
different mountpoint. The concurrent configuration shape is:

```json
{
  "version": 1,
  "transport": "nfs",
  "driver": {
    "kind": "splitstore",
    "storage": {
      "concurrent_writes": true,
      "metadata": {
        "kind": "foundationdb",
        "cluster_file": "/absolute/path/to/fdb.cluster",
        "volume_key": "mount-rs/my-new-shared-volume",
        "durable": true,
        "lease_authority": "revision-cas"
      },
      "blocks": {
        "kind": "foundationdb",
        "cluster_file": "/absolute/path/to/fdb.cluster",
        "volume_key": "mount-rs/my-new-shared-volume",
        "durable": true,
        "lease_authority": "revision-cas"
      }
    }
  }
}
```

Save that JSON as `/absolute/path/to/shared.json`, then run the two
commands in separate Terminal windows with two empty, user-owned paths:

```sh
./scripts/cargo-shared run --locked -p mount-rs-cli \
  --features foundationdb -- mount --config /absolute/path/to/shared.json \
  --mountpoint /absolute/path/to/view-a
./scripts/cargo-shared run --locked -p mount-rs-cli \
  --features foundationdb -- mount --config /absolute/path/to/shared.json \
  --mountpoint /absolute/path/to/view-b
```

For two paths in **one CLI** backed by this same FoundationDB volume, use one
command with `--mountpoint /absolute/path/to/view-a` and
`--also-mountpoint /absolute/path/to/view-b`. The CLI opens one `ChunkedFs`
driver and starts two NFSv3 views. The two-CLI commands open separate drivers;
FoundationDB revision CAS coordinates their metadata publications.

This cross-process recipe requires matching macOS FoundationDB client
libraries, a live cluster, and the same shared block backing accessible to
both processes. The CLI rejects memory and local SQLite blocks with
FoundationDB metadata: they cannot serve references published by another
host. Local SQLite blocks are accepted in concurrent mode only alongside
SQLite metadata when both processes use the same local file paths. A second host
needs a reachable *shared* FoundationDB cluster and the same client,
configuration, and block backing. The local macOS test cluster does not verify
cross-host behavior. A prefix containing a legacy lease or fence key rejects
`concurrent_writes` with `EBUSY` and needs an offline migration before
concurrent mounts can use it.
Stop and upgrade any older mount-rs writers before activating this mode.
Older binaries do not recognize the concurrent-mode marker. If all writers
cannot be upgraded together, isolate old clients at the deployment, network,
or tenant level where that control is available before opening the new shared
volume.

Concurrent mode keeps detached file tombstones so remote open handles retain
their inode and blocks. It also leaves immutable blocks staged for conflicted
or failed publications. Without distributed open-handle pins and safe block
reclamation, neither category has a built-in retention cap; backing storage
can grow without bound during use. The mode remains experimental pending that
capacity protocol. Local native checks do not qualify long-running or
cross-host operation.
The shared NFSv3 session retains a backend descriptor and opaque handle entry
for each regular file it visits until server close. `max_handles` is a soft
target and does not bound those pinned entries, so process resource use can
also grow during a long-running mount.

Concurrent mounts refresh their namespace by loading metadata on filesystem
operations. The NFS session's `shared_concurrent_view` setting enables
identity-guarded NFSv3 behavior: the server verifies an opened file's inode,
and guarded stat, lookup, readdir and readlink inspect one namespace snapshot
using the original opaque handle identity. Handle-derived mutations check the
same identity within their commit. Shared mode implies WCC omission; the
separate `omit_wcc_attributes` setting can also omit WCC in an ordinary view.
The shared CLI profile omits optional
before/after weak-cache-consistency attributes from mutation replies because
another server may publish a newer revision before this reply. NFSv4
requests fail closed in this shared profile. For shared NFS views, the CLI adds
macOS options `noac` (disable attribute caching) and `nonegnamecache`
(disable negative name caching).
Positive directory entries and file data can still have weak NFS cache
coherence. Finder may need a directory refresh to display a newly created
file from the other process. Close or synchronize the writer's file before
expecting another mount to read its committed contents.

[FoundationDB key watches](https://apple.github.io/foundationdb/developer-guide.html#watches)
can signal that a volume revision changed so another client reloads its
namespace. A watch carries no file data or event history; clients still read
the current revision and namespace. The `revision-cas` transaction controls
conflicting writes, even when a notification is delayed or missed. Watches
are a planned signal optimization; mount-rs does not use them yet, and a
watch cannot directly invalidate the macOS NFS client's cache.

## Choose a storage backend

The CLI selects built-in filesystems with `--driver`. Structured JSON
`driver.kind: "splitstore"` composes independent metadata and block
providers. Each filesystem, provider, and transport lives in its own crate;
the [code architecture guide](code-architecture.md) shows the dependency
direction.

| Storage path | Direct check on macOS | Native Finder path |
| --- | --- | --- |
| In-memory `MemoryFs` | `./scripts/cargo-shared run --locked -p mount-rs-cli -- sdk-self-test` | Run the memory NFS command above. |
| SQLite `SqliteFs` or SQLite/SQLite split store | Use a file-backed config with `sdk-self-test --config PATH --reopen`. | Use the single-host NFS config below, or the local concurrent SQLite/SQLite recipe above. |
| PGlite metadata and blocks | `./scripts/test-pglite.sh` starts an isolated PostgreSQL-wire server and runs the provider checks. | `MOUNT_RS_PGLITE_TEST_SCOPE=native-nfs ./scripts/test-pglite.sh` runs a disposable native mount test; use one running PGlite server and a split-store config for an interactive mount. |
| RustFS S3-compatible blocks | `./scripts/test-rustfs.sh` starts a disposable RustFS service in Docker and tests the named `rustfs` block provider. | Keep a reachable RustFS service running; pair `storage.blocks.kind: "rustfs"` with PGlite or FoundationDB metadata for cross-host backing and mount that config over NFS. |
| FoundationDB metadata with RustFS blocks | `./scripts/test-foundationdb.sh` tests a disposable cluster with a Linux client in Docker; the composed lane is in [the FoundationDB test guide](../tests/foundationdb/README.md). | Supply a compatible macOS FoundationDB client and readable cluster file, a live cluster, and RustFS credentials; build the CLI with `--features foundationdb` and mount the composed config over NFS. |

These direct checks have different lifecycles. `sdk-self-test` exercises the
filesystem without a kernel mount; a `native-nfs` test creates a temporary
mount and unmounts it during the test, so it will usually be gone before Finder
can inspect it. Docker runs the RustFS and FoundationDB services in Linux
containers; a passing Docker provider check does not create a macOS volume.

### SQLite: a persistent Finder example

This example stores SQLite state outside the mounted view and sets the virtual
root owner to the current macOS identity:

```sh
demo_tmp_root="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
demo_dir="$(mktemp -d "$demo_tmp_root/mount-rs-sqlite.XXXXXX")"
mount_dir="$demo_dir/view"
mkdir "$mount_dir"
config="$demo_dir/mount.json"
cat >"$config" <<JSON
{
  "version": 1,
  "mountpoint": "$mount_dir",
  "transport": "nfs",
  "sqlite_single_host": true,
  "driver": {
    "kind": "sqlite",
    "database": "$demo_dir/state.sqlite",
    "uid": $(id -u),
    "gid": $(id -g)
  }
}
JSON
./scripts/cargo-shared run --locked -p mount-rs-cli -- \
  validate-config --config "$config"
./scripts/cargo-shared run --locked -p mount-rs-cli -- \
  mount --config "$config"
```

Open `$mount_dir` through Finder's Go to Folder while the second command
runs. The `sqlite_single_host` profile uses NFSv3, a hard mount, and
macOS-local locks for one host/client. SQLite files hosted *inside* this mount
use DELETE journaling; WAL and cross-host SQLite locking are outside this
profile. After Ctrl-C and `unmounted`, the SQLite backend database remains
at `$demo_dir/state.sqlite`. Keep that file if you want the mounted state on
the next run.

### PGlite, RustFS, and FoundationDB configurations

PGlite needs the `pglite-socket` server in another process. Install its pinned
fixture dependencies with
`pnpm --dir tests/pglite install --frozen-lockfile`, then follow the
[socket-server instructions](../tests/pglite/README.md). Set
`PGLITE_DATABASE_URL` to the server's Unix socket or TCP URL and use a structured
split-store config with `kind: "pglite"` in both provider roles. The CLI's
[native PGlite test](../apps/mount-rs-cli/tests/native_lifecycle.rs) constructs
that shape and checks write, read, shutdown, and reopen over NFS. A persistent
`PGLITE_DATA_DIR` is needed if the database must survive a PGlite server
restart; setting `durable: true` in a config is the caller's assertion about
the server, not an automatic durability check.

RustFS has its own `mount-rs-rustfs` crate and `rustfs` block-provider kind.
It supplies immutable blocks over signed, path-style S3-compatible requests;
it does not supply metadata revision CAS. The
[RustFS harness](../tests/rustfs/README.md) owns a temporary service and
bucket, then removes them on normal exit. For an interactive mount, keep a
service available at the configured `endpoint`, initialize its `bucket`,
set `RUSTFS_ACCESS_KEY_ID` and `RUSTFS_SECRET_ACCESS_KEY` in the CLI
environment, supply the endpoint's `region`, and give the filesystem its
own `prefix`. The endpoint URL has no path, apart from an optional trailing
slash. Omitting `durable` defaults to false; `durable: true` asserts that the
operator's RustFS service will retain completed writes. The disposable
single-node harness does not establish power-loss durability. Start from the
[PGlite/RustFS concurrent config](../apps/mount-rs-cli/examples/config-pglite-rustfs-concurrent.json)
or the FoundationDB/RustFS template below. Set `mountpoint` to an absolute,
user-owned empty directory and `transport` to `nfs`. Run
`validate-config --config PATH` to check the schema, then
`mount --config PATH` to open the providers and mount the view.
Configuration validation does not contact RustFS.

The [FoundationDB/RustFS config
template](../apps/mount-rs-cli/examples/config-foundationdb-rustfs.json)
selects exclusive-writer FoundationDB metadata and the named RustFS blocks.
The separate [concurrent FoundationDB/RustFS
template](../apps/mount-rs-cli/examples/config-foundationdb-rustfs-concurrent.json)
selects revision-CAS metadata and `concurrent_writes` for a fresh volume.
On macOS either
requires FoundationDB 7.4's matching `libfdb_c.dylib`, a cluster file
reachable by the host process, a live cluster, and a live RustFS endpoint.
Set the template's `cluster_file`, `endpoint`, `bucket`,
`mountpoint`, and `transport: "nfs"` for the local run. Enable the
`foundationdb` Cargo feature:

```sh
./scripts/cargo-shared run --locked -p mount-rs-cli \
  --features foundationdb -- validate-config --config /path/to/config.json
./scripts/cargo-shared run --locked -p mount-rs-cli \
  --features foundationdb -- mount --config /path/to/config.json
```

Use `lease_authority: "persisted-single-authority"` only for an owned
single-authority development cluster. A shared deployment using the existing
exclusive-writer mode requires the protected authority publisher and
`shared-provider` configuration described in the
[CLI guide](../apps/mount-rs-cli/README.md). A fresh concurrent-writer volume
uses `revision-cas` metadata with `concurrent_writes: true` instead. The Docker
FoundationDB harness supplies a Linux `libfdb_c.so` inside its client
container; it does
not install `libfdb_c.dylib` on macOS or leave a Finder mount.

## Local acceptance recorded on 2026-09-22 and 2026-09-23

On the macOS arm64 development host, the native CLI NFS cases for memory,
PGlite, and SQLite each passed their exact ignored test (1/1). The core
`native_nfs_hosts_real_sqlite_on_split_store` SQLite case also passed (1/1).
The ordinary `sh scripts/test-pglite.sh` gate exited 0, and
`./scripts/test-rustfs.sh` exited 0 with `RUSTFS_INTEGRATION_PASS` and
`RUSTFS_SDK_SQLITE_METADATA_PASS`. The FoundationDB standalone provider
gate emitted `FOUNDATIONDB_TEST_PASS` in a Linux arm64 Docker client.

The memory native case was
`cli_nfs_memory_config_binary_mounts_io_and_resets_on_restart`; the PGlite
case ran through
`MOUNT_RS_PGLITE_TEST_SCOPE=native-nfs ./scripts/test-pglite.sh`; the SQLite
CLI case was `cli_nfs_sqlite_config_binary_hosts_sqlite_and_reopens`.
These cases mount disposable paths and verify unmount during the test. The
RustFS gate uses a Docker Linux service and a macOS client, while the
standalone FoundationDB Docker gate uses a Linux client. Separately, an
isolated FoundationDB 7.4.7 cluster and matching arm64 `libfdb_c.dylib` ran
natively on this macOS host. The provider transaction contract and terminal
network shutdown regression each passed (1/1). A feature-enabled CLI
`sdk-self-test` using FoundationDB metadata exited cleanly after writing and
reading. The composed FoundationDB/RustFS native NFS case
`cli_foundationdb_rustfs_config_binary_mounts_and_reopens` passed (1/1): it
mounted, wrote and read through NFS, unmounted on SIGINT, and a fresh CLI
process reopened the data and unmounted cleanly. RustFS was a disposable
Docker service for this case; the FoundationDB server and client ran on
macOS. The native test mount was removed during the test, so an interactive
Finder view requires the long-running CLI command above.

The interactive memory command shown above was also run on this host. It
printed `mounted nfs at ...`, the host mount table recorded `nobrowse`,
the mounted `README.md` was read, Ctrl-C printed `unmounted`, and the
exact mount table entry disappeared. The empty temporary directory was
removed afterward. Finder's GUI was not part of that command check.

On 2026-09-23, the final guarded-source native `native_multi_mount` tests
passed 1/1 each for one CLI serving two writable memory views and one CLI
serving two writable FoundationDB views with reopen. The final
`native_two_process_foundationdb` test passed 1/1 on a fresh volume: two
independent mounted CLIs saw each other's creates, disjoint same-file writes,
rename and unlink, and a third CLI reopened committed bytes after clean
unmount. All three tests used the same pinned CLI dependency source, and no
test NFS mount remained afterward. These checks use separate processes on one
macOS host; they do not establish cross-host behavior or the distributed
handle-pin/capacity protocol.

The newer native CLI cases passed for one CLI serving two writable views and
for two independent writable CLIs on the same local SQLite backing. SQLite's
two-CLI case exercised DELETE and WAL backing, bounded load, disjoint writes,
rename/unlink and reopen; WAL remains unqualified with bundled SQLite 3.46
until the [WAL-reset bug](https://www.sqlite.org/wal.html#the_wal_reset_bug)
is fixed. The PGlite native suite passed its one-CLI/two-view and two-CLI
cases against one TCP-loopback PGlite engine, plus the insufficient-connection
fail-closed case. These are local mount checks. They do not verify a
cross-host PGlite listener or physical cross-host mounts.
