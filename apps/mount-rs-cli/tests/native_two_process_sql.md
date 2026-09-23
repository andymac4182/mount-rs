# Native macOS SQLite split-store qualification

This packet uses **local SQLite metadata and block databases as mount-rs
backing**, served through one or two CLI processes and native NFS views. It
also has separate negative cases for backing databases hosted **on** NFS.
SQLite application files **inside** an NFS view have their own adversarial
packet at [`tests/sqlite_nfs_adversarial.md`](../../../tests/sqlite_nfs_adversarial.md).

Each case creates unique test-owned paths under `TMPDIR`, pins the built CLI
there, verifies exact mount entries, sends SIGINT only to children it started,
and removes its own directory after unmount. Set `TMPDIR` to a fresh, empty
disposable directory. The native tests are ignored unless their two opt-in
variables are both set to `1`.

## Supported local backing cases

```sh
probe_root=$(mktemp -d /private/tmp/mount-rs-cli-sqlite-test-XXXXXXXX)
TMPDIR="$probe_root" \
MOUNT_RS_CLI_NATIVE_NFS=1 \
MOUNT_RS_CLI_NATIVE_SQLITE_TWO_PROCESS=1 \
CARGO_TARGET_DIR=/private/tmp/mount-rs-cargo-target \
  ./scripts/cargo-shared test --locked -p mount-rs-cli \
  --test native_two_process_sql \
  two_cli_processes_share_sqlite_backing_and_reopen \
  -- --ignored --nocapture
rmdir "$probe_root"
```

The two-process case runs local DELETE backing by default. Add
`MOUNT_RS_CLI_NATIVE_SQLITE_WAL=1` to its command for a separate diagnostic
WAL run; WAL is not part of the native CI safety gate. Its two CLIs have
independent NFS listeners and access the same SQLite file paths.
It checks bidirectional create visibility, simultaneous disjoint 4 KiB writes
to one file, rename/unlink, 2 × 12 file create/rename/unlink lifecycles,
exact data after fresh process reopen, clean unmount, and both backing
databases' `PRAGMA integrity_check`.

For two views in **one** CLI, use a fresh `probe_root` and replace the test
selector and opt-in variable with:

```sh
MOUNT_RS_CLI_NATIVE_SQLITE_ONE_PROCESS=1
one_cli_process_serves_two_writable_sqlite_views
```

That case uses CLI `--also-mountpoint` with local DELETE backing. It checks
the same namespace/range/lifecycle operations, then clean unmount and fresh
reopen.

The two-process case also probes a SQLite **application** database through
both NFS views: A holds an uncommitted `BEGIN IMMEDIATE` update, B attempts
`BEGIN IMMEDIATE` with a short timeout. B rolls back if it gets the lock. The
result is diagnostic only; the test checks the rolled-back value and
`integrity_check`, and prints `SQLITE_INNER_NFS_LOCK=blocked` or
`lock_bypassed`. A blocked attempt does not certify general NFS locking.

Set `MOUNT_RS_CLI_NATIVE_SQLITE_INNER_MATRIX=1` for a separate, bounded
adversarial run inside those **two independent CLI mounts**. It replaces the
short inner lock probe in the DELETE backing case with SQLite application
DELETE, TRUNCATE, PERSIST, and WAL capability and process kill/reopen checks,
plus two workers × eight transactions in DELETE and available WAL. The
fixture checks exact committed payloads and `integrity_check`, then records
two-view create/delete visibility and conflicting locks. SQLite may select
DELETE when WAL is requested through NFS; that fallback is reported as
unsupported. A two-view lock bypass is a diagnostic failure of that SQLite
application topology, and the contender rolls back without writing. Each
JSON report is printed as `SQLITE_INNER_NFS_MATRIX`. The application matrix
reports its failure count and exit status while the provider acceptance
continues to exact reopen and backing integrity checks. This two-CLI
application topology is unsupported; journal/recovery/load failures are
recorded as such. The Python fixture retries removal of its unique NFS
directory for bounded deferred unlink races. If it remains after that wait,
it reports `owned_cleanup=deferred_until_backing_disposal`; the native test
then cleanly unmounts and removes the entire disposable backing. The test
fails if the fixture cannot emit a summary or its exit disagrees with that
summary. SQLite failure reports include `sqlite_errorcode` and
`sqlite_errorname` when Python receives a SQLite error, including a failed
load worker's error. Set `MOUNT_RS_CLI_NATIVE_SQLITE_INNER_TRACE=1` with the
matrix flag to capture bounded CLI `--verbose` lines for failed operations
and `.nfs` rename activity inside the fixture's unique directory. The native
test prints up to the last 80 matching lines per CLI after clean unmount.
See the standalone
[adversarial packet](../../../tests/sqlite_nfs_adversarial.md) for the
supported single-view profile and its limits.

## Backing path boundary

For concurrent SQLite backing, the CLI rejects metadata or block database
paths inside any requested view before opening the providers. The guard
checks existing symlinks and missing path components, then creates an
unmounted view directory and checks the actual filesystem identity again.
It follows dangling backing symlinks into missing view paths, including an
intermediate directory link; unrelated outside links stay valid and cycles
fail closed.
On case-insensitive macOS volumes this catches absent `mnt` versus
`MNT/metadata.sqlite`, composed versus decomposed Unicode names, and
`ß` versus `ss` aliases. A view that is already mounted returns `EBUSY`
without detaching it. A rejected request may leave a new empty view
directory; it does not create either backing database or install the
concurrent metadata marker. Unrelated sibling paths, including distinct
Unicode names, remain valid.

## Unsupported NFS backing cases

Run either selector below with a fresh test root, `MOUNT_RS_CLI_NATIVE_NFS=1`,
and its listed opt-in variable. These cases start a disposable memory-backed
native NFS view to host test-owned SQLite backing files. The metadata selector
hosts metadata on NFS and keeps blocks local so its rejection remains
specific to metadata after the block preflight runs first.

| Selector | Opt-in variable | Required result |
| --- | --- | --- |
| `sqlite_backing_files_on_nfs_are_unsupported` | `MOUNT_RS_CLI_NATIVE_SQLITE_NFS_BACKING=1` | Two independent concurrent SQLite CLIs reject NFS metadata before either mounts; backing integrity stays `ok`. |
| `sqlite_blocks_on_nfs_with_local_metadata_are_unsupported` | `MOUNT_RS_CLI_NATIVE_SQLITE_NFS_BLOCKS=1` | A concurrent SQLite CLI with local metadata and NFS blocks rejects before mount; the local metadata row stays fresh (`revision=0`, no namespace/owner or `MRC1` marker). |

For example:

```sh
probe_root=$(mktemp -d /private/tmp/mount-rs-cli-sqlite-test-XXXXXXXX)
TMPDIR="$probe_root" \
MOUNT_RS_CLI_NATIVE_NFS=1 \
MOUNT_RS_CLI_NATIVE_SQLITE_NFS_BLOCKS=1 \
CARGO_TARGET_DIR=/private/tmp/mount-rs-cargo-target \
  ./scripts/cargo-shared test --locked -p mount-rs-cli \
  --test native_two_process_sql \
  sqlite_blocks_on_nfs_with_local_metadata_are_unsupported \
  -- --ignored --nocapture
rmdir "$probe_root"
```

## Observed macOS runs, 2026-09-23

The disposable two-process acceptance passed for local DELETE and opt-in WAL
backing: 2 × 12 lifecycle loads took 988 and 861 ms, and backing integrity
was `ok` after clean stop and again after reopen. The inner SQLite
application lock attempt was `blocked` in both runs. The one-CLI/two-view
local DELETE case passed its 2 × 12 load in 1,376 ms, with exact data,
integrity, and fresh reopen.

The full inner SQLite packet was run three times against two independent
DELETE-backed CLI mounts. Each mount-rs file lifecycle load finished before
the application packet (the final 2 × 12 load took 859 ms). The SQLite
application packet consistently failed four cases: DELETE, TRUNCATE, and
PERSIST each reported `database is locked` after killing an uncommitted
writer, and one DELETE load worker reported a disk I/O error at commit. WAL
selected DELETE and was reported unsupported. The second view observed a
synced create/delete and a competing `BEGIN IMMEDIATE` was blocked in this
run, but those observations do not certify two-view locks. After a ten-second
owned cleanup retry, three `.nfs.*` deferred names remained; the native test
then cleanly unmounted, reopened, verified both backing databases had
`integrity_check=ok`, and disposed of the test-owned backing and mounts.
These are SQLite application failures in the shared-view `soft,nolocks` NFS
profile, not failures of the local SQLite storage provider's acknowledged
file operations.

Before the local-filesystem guard, the NFS metadata+blocks negative setup
allowed both CLIs to mount. Simultaneous first filesystem writes gave
A=`EIO` and B=`Ok(())`; both backing databases still passed integrity.
After the metadata guard, both CLIs rejected before NFS mount with
`concurrent SQLite metadata requires a local filesystem; NFS/network-backed
databases are unsupported`, and the backing databases remained valid.
The final executable metadata selector isolates that guard with NFS metadata
and local blocks; the historical both-on-NFS write result above remains a
separate diagnostic observation.

The mixed local-metadata/NFS-block negative was red before the block guard:
the CLI mounted, and fresh local metadata was promoted to `MRC1`, revision
1, with a namespace. The first run after the provider guard remained red
because the SDK's erased block-store wrapper used the trait's default
preparation method and bypassed the provider check. After the wrapper
forwarded preparation, the native negative passed: the CLI rejected before
mount with `concurrent SQLite blocks require a local filesystem;
NFS/network-backed databases are unsupported`. The local metadata row stayed
at revision 0, fence 0, no namespace/owner, and no `MRC1` marker; both
database integrity checks were `ok`.

After this forwarding fix, the supported two-CLI local-backing case passed
again in DELETE and WAL (2 × 12 loads in 969 and 828 ms, with clean reopen and
backing integrity `ok`). The one-CLI/two-view DELETE case passed again (2 ×
12 load in 1,561 ms, with clean reopen and backing integrity `ok`). All exact
temporary mounts were removed; the independent Finder demo mount was left
alone.
After the pre-provider mountpoint path guard changed, fresh DELETE reruns
passed for both cases: two separate CLIs completed 2 × 12 lifecycles in
1,798 ms, and one CLI with two writable views completed them in 3,342 ms.
Fresh reopen and both backing databases' integrity checks stayed `ok`. The
separate-CLI inner SQLite lock attempt was blocked; this is a diagnostic
observation only.

The local same-host SQLite backing case does not establish sharing those
files across hosts over NFS/SMB. The currently bundled SQLite is 3.46.0;
the WAL-reset bug blocks a WAL safety claim until that library is upgraded.
SQLite's own guidance distinguishes network filesystems and WAL:
[SQLite over a network](https://www.sqlite.org/useovernet.html),
[SQLite WAL](https://www.sqlite.org/wal.html).
