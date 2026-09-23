# SQLite files inside a native NFS mount

This packet probes **SQLite database files inside a mount-rs NFS view**. The
storage provider behind that view is a separate pair of local SQLite metadata
and immutable-block databases. Provider concurrency is covered by other
tests; this packet exercises the SQLite application visible to the NFS client.

Run the host-filesystem control without a mount:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 tests/sqlite_nfs_adversarial.py \
  --self-test --workers 4 --transactions 24
```

For native macOS/Linux acceptance, build and run the existing ignored test
with the added adversarial phase:

```sh
CARGO_TARGET_DIR=/private/tmp/mount-rs-cargo-target \
  ./scripts/cargo-shared test --locked -p mount-rs-core \
  --test native_nfs_sqlite --no-run

MOUNT_RS_RUN_NATIVE_NFS_SQLITE=1 \
MOUNT_RS_SQLITE_NFS_ADVERSARIAL=1 \
CARGO_TARGET_DIR=/private/tmp/mount-rs-cargo-target \
  ./scripts/cargo-shared test --locked -p mount-rs-core \
  --test native_nfs_sqlite -- --ignored --nocapture
```

The native test uses disposable metadata/block files and a unique mountpoint.
It runs the original DELETE/WAL acceptance, then the matrix below. Its mount
cleanup verifies the exact mountpoint is no longer mounted before removing
the empty directory. The Python fixture removes only its unique directory
inside the mount.

The default native load is 2 workers × 8 transactions. Set
`MOUNT_RS_SQLITE_NFS_LOAD_WORKERS=4` and
`MOUNT_RS_SQLITE_NFS_LOAD_TRANSACTIONS=25` for a bounded 100-transaction
stress run. The Python runner accepts 1–8 workers and 1–100 transactions per
worker and checks every acknowledged row after fresh SQLite reopen.

| Probe | What it checks |
| --- | --- |
| DELETE, TRUNCATE, PERSIST, WAL | SQLite reports the requested journal mode. WAL fallback is recorded as unsupported. |
| Lock plus SIGKILL/reopen in all available modes | The killed writer explicitly selects the mode, spills dirty pages, and tests both committed and uncommitted recovery. A competing process cannot obtain an incompatible write lock; `integrity_check` and rows are checked after reopen. |
| 2 workers × 8 transactions in DELETE and available WAL | Simultaneous starts, distinct 4 KiB payloads, exact committed row count and contents, integrity, busy retries, throughput, and p95 commit time. |
| Local alias control | An ordinary local SQLite file blocks a competing `BEGIN IMMEDIATE` through a second connection. |
| Two-view name visibility | After an initial negative lookup, the second view must see a synced create and then a delete within a bounded 10-second probe. Delays are recorded in milliseconds. |

To examine two **unsupported** SQLite application views, set
`MOUNT_RS_SQLITE_NFS_SECOND_VIEW=1` with the native command. That test starts
two independent NFS server/listener lifecycles in one Rust process, both
sharing one `ChunkedFs` driver and its SQLite backing. To examine the default
soft NFSv3 mount with `nolocks` instead of
the single-host SQLite profile, set `MOUNT_RS_SQLITE_NFS_UNSAFE_NOLOCKS=1`.
Both probes use an owned file and attempt only a competing `BEGIN IMMEDIATE`
while another process holds an uncommitted update. If the contender gets a
lock, it rolls back without writing. The JSON result says `lock_bypassed`,
`blocked`, or `error`; a blocked result does **not** establish general NFS
locking reliability.

This packet does not test cross-host file locking, NLM, power loss, client
reboot, or a remote NFS server. SQLite itself warns that network filesystem
locking and `fsync` behavior vary, and WAL requires all processes to be on
one host: [SQLite over a network](https://www.sqlite.org/useovernet.html),
[WAL documentation](https://www.sqlite.org/wal.html). These probes are
diagnostic; the tested single-view profile uses one host kernel client with
`sqlite_single_host()`.

## Observed macOS run, 2026-09-23

The opt-in native packet ran against owned temporary paths on this Mac with
Python SQLite 3.53.4. DELETE, TRUNCATE, and PERSIST each passed process lock,
SIGKILL/reopen, data, and integrity checks. WAL requested through the NFS
view selected DELETE, so WAL was recorded as unsupported. The native 2 × 8
DELETE load committed all 16 exact 4 KiB rows at 14.082 transactions/second
with p95 commit time 72.066 ms and `integrity_check=ok`.

The two-view probe found a concrete lock failure: while the first NFS view
held an uncommitted SQLite `UPDATE`, the second NFS view acquired its own
`BEGIN IMMEDIATE` on the same file. The contender immediately rolled back;
the database remained intact. This demonstrates why the CLI rejects the
single-host SQLite NFS profile with shared mountpoints. In the same run, the
second view saw a synced create after 7.574 ms and a delete after 4.478 ms.
The default `nolocks` single-view diagnostic happened to block a competing
writer; that one observation does not establish safe locking.

The native mount test removed its exact temporary mounts and storage paths.
The existing Finder demo mount was not part of this test.

A second native run on one `sqlite_single_host()` NFS mount used 4 workers ×
25 DELETE transactions. All 100 acknowledged 4 KiB payloads matched after
reopen; SQLite reported `integrity_check=ok`, 11 busy lock retries,
22.11 transactions/second, and p95 commit time 111.175 ms. DELETE,
TRUNCATE, and PERSIST recovery checks passed; WAL again selected DELETE.
The exact temporary mount and storage paths were removed after verifying
the mount table contained no test entry.
