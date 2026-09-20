# SQLite VFS WAL plan

Status: implementation plan for the WAL capability in
`integrations/mount-rs-sqlite-vfs`. Rollback-journal behavior remains the
compatibility baseline and must continue to pass unchanged.

## Contract being implemented

SQLite WAL is a three-file protocol:

* `X` is the main database.
* `X-wal` contains committed frames that have not yet been checkpointed into
  `X`.
* `X-shm` is the wal-index shared-memory file. It is a coordination/cache
  structure, not durable database state, and SQLite may recreate it from a
  valid WAL after a crash.

SQLite's official requirements are:

* the VFS file methods must provide version-2 shared-memory callbacks:
  `xShmMap`, `xShmLock`, `xShmBarrier`, and `xShmUnmap`;
* `xShmMap` must return stable mapped regions of the requested size, extending
  the shared-memory object only when requested;
* `xShmLock` has eight lock slots (`SQLITE_SHM_NLOCK == 8`) and accepts only
  lock/unlock combined with shared or exclusive mode. A connection may move
  between unlocked and shared or between unlocked and exclusive, but not
  directly between shared and exclusive;
* `xShmBarrier` must publish shared-memory writes before another connection
  observes the wal-index; and
* `xShmUnmap` must release the connection's mapping and delete the shared
  memory only when SQLite has determined that the last client may remove it.

WAL still uses the main-file locking protocol. In WAL mode, connections hold
the main-file SHARED lock while attached, writers serialize on the WAL write
lock, checkpoints serialize on the checkpoint lock, recovery takes the
recovery/read locks, and read transactions hold one WAL read lock. A writer
must not reset or overwrite the WAL while a reader still references frames
past the checkpoint boundary.

The durability ordering is the SQLite ordering, not a provider shortcut:

1. flush the WAL (`xSync` on `X-wal`);
2. copy valid frames into `X` during checkpoint;
3. flush `X` (`xSync` on the main database); and
4. reset/delete WAL and SHM only after the reader and main-file lock gates
   allow it.

Recovery scans WAL frames, validates checksums, reconstructs the wal-index, and
must hold the recovery-exclusive SHM locks while rebuilding shared state.

References: [WAL](https://www.sqlite.org/wal.html),
[WAL file format](https://www.sqlite.org/walformat.html),
[VFS I/O methods](https://sqlite.org/c3ref/io_methods.html),
[SHM lock flags](https://sqlite.org/c3ref/c_shm_exclusive.html),
[SHM lock count](https://sqlite.org/c3ref/c_shm_nlock.html),
[Unix VFS source](https://sqlite.org/src/file/src/os_unix.c), and
[Windows VFS source](https://sqlite.org/src/file/src/os_win.c).

## Current boundary and capability gates

The existing synchronous `Backend`/`VfsFile` contract provides durable file
operations and rollback lock levels. It does not provide shared-memory
regions, SHM lock slots, or a distributed coordination primitive. The existing
`StorageBackend` additionally provides a fenced volume-wide writer lease,
immutable block puts, metadata revision publication, and provider flush
barriers. Its provider traits do not provide atomic shared-memory-page and
lock updates.

WAL is therefore opt-in and capability-negotiated. A requested scope must be
no broader than the backend's declared capability; otherwise VFS creation and
the WAL PRAGMA path fail explicitly with an unsupported-capability error.
There is no process-local implementation hidden behind a host/distributed
claim.

The capability levels are:

| Scope | Meaning | Initial implementation |
| --- | --- | --- |
| Disabled | Rollback journal only; WAL request is rejected | default for compatibility |
| ProcessLocal | Shared memory and SHM locks are shared by connections using the same backend/VFS process state; WAL bytes may still be durable if the providers are durable | `StorageBackend`, with an explicit process-local request |
| HostLocal | File-backed SHM and OS-visible lock coordination work between processes on one host and one supported local filesystem | `HostDirectory` |
| Distributed | A provider supplies shared-memory/lock semantics across hosts, with fencing and recovery | not enabled by this change |

`StorageBackend` reports provider durability separately from coordination
scope. Memory providers are volatile evidence only. Durable SQLite metadata and
block providers can prove WAL bytes and namespace recovery after a clean close
or reopen, but their current generic provider traits still do not prove
cross-process SHM. PGlite, FoundationDB, TiDB, and object-store combinations
must not be advertised as distributed WAL merely because their metadata or
block writes are durable.

## Shared-memory architecture

The VFS file boundary will expose four backend-owned operations corresponding
to SQLite's SHM callbacks. Each main-database file owns one per-connection SHM
handle; handles for the same normalized database identity refer to one shared
region and one lock table at the declared scope.

* `xShmMap` validates non-negative region/page values, keeps the region size
  consistent for the database, returns a stable pointer for an existing region,
  and returns a null pointer without extending when SQLite asks for a missing
  read-only region. Extending creates a zeroed region and is the only path that
  grows the SHM object.
* `xShmLock` validates the SQLite flag combinations and range `0..8`, performs
  all-or-nothing range acquisition, records the exact mode held by the
  connection, and maps conflicts to `SQLITE_BUSY`. Unlocking requires the same
  shared/exclusive mode used for acquisition.
* `xShmBarrier` uses a sequentially consistent memory fence. It never performs
  provider I/O.
* `xShmUnmap` releases this connection's mappings and locks. Host-local delete
  is attempted only after the last SHM connection and SQLite's delete flag;
  the HostDirectory implementation uses a separate deadman-switch (DMS)
  shared lock to coordinate that last-client decision across processes.
  Process-local state is retained until the backend volume state is dropped so
  pointers cannot outlive their owner during a callback.

For `HostDirectory`, `X-shm` is a real file mapped with shared read/write
pages. SHM lock slots use OS-visible per-slot lock objects plus a DMS lock so
the existing rollback sidecar locks and WAL locks do not accidentally collapse
into one whole-file lock or delete `X-shm` while another process is mapped.
Unix uses shared file mappings and interrupt-safe native locking; Windows uses
file mappings and the Win32 lock/handle primitives used by the native SQLite
VFS. The implementation will not claim network-filesystem WAL.

For `StorageBackend`, process-local regions are stable boxed pages behind a
shared volume map. The region is intentionally not treated as cross-process
memory. On reopen, a new region starts empty and SQLite's WAL recovery rebuilds
it from the durable `X-wal` bytes.

## StorageBackend ordering and lock integration

WAL main-database readers must not retain the current rollback writer lease
merely to read a snapshot. A WAL reader loads/refreshes the main and WAL bytes
at the SHM read-lock boundary and keeps its SQLite snapshot locally while the
writer lease remains available to a concurrent writer. A WAL writer or
checkpoint acquires the provider writer lease through the SHM write/checkpoint
lock before modifying provider-backed bytes.

The existing publication sequence remains the durability boundary:

1. put all immutable WAL or database blocks;
2. flush the block provider;
3. publish the new namespace under the fenced metadata lease; and
4. flush the metadata provider.

The VFS must not report `xSync` success after a failed block or metadata
barrier. A failed WAL sync, SHM map, SHM lock, or provider lease transition
fails closed; it does not silently fall back to rollback or expose a partial
publication. Existing failed-state behavior remains in force after a provider
failure until the backend is explicitly rebuilt/reopened.

The current generic provider contract is sufficient for process-local WAL
because the local SHM lock table can serialize the SQLite engine while the
provider lease serializes durable publication. It is not sufficient for
distributed SHM. A future distributed coordinator must add, at minimum:

* a fenced, atomic read/write/checkpoint/recovery lock service with the eight
  SQLite lock slots and lease expiry/recovery semantics;
* provider-addressable SHM pages or an equivalent shared-memory API with
  conditional page publication and versioning;
* immutable WAL-frame storage with a confirmed-upload barrier before metadata
  references become visible;
* a metadata CAS/transaction that coordinates WAL generation, frame range,
  salts, checkpoint backfill, and reader marks; and
* recovery that can rebuild the wal-index after any client or coordinator
  failure without allowing an expired owner to publish.

Until that contract exists, remote/object-store-backed pairs are explicitly
unsupported for distributed WAL. Durable object uploads alone are not shared
memory, and a process-local wal-index would permit corruption if another
process or host opened the same database.

## Verification gates

The implementation is accepted only when all of these hold:

1. Existing rollback tests, including the DELETE/TRUNCATE/PERSIST ×
   NORMAL/FULL/EXTRA matrix, pass unchanged for HostDirectory and the
   StorageBackend memory/durable SQLite pairs.
2. Unsupported backends/scopes reject `PRAGMA journal_mode=WAL` explicitly;
   no test treats a process-local region as distributed evidence.
3. Host-local WAL tests assert effective `journal_mode=WAL`, synchronous
   NORMAL and FULL, WAL/SHM artifacts, exact row ledgers, integrity checks,
   and clean close/reopen persistence.
4. Two connections exercise a long reader snapshot while a writer commits,
   then verify the reader's old view and the next reader's new view. A child
   process exercises the same-host SHM and lock path for HostDirectory.
5. PASSIVE/FULL/TRUNCATE checkpoint paths are covered both with a blocking
   reader and after the reader closes. WAL reset/deletion is checked only
   after the correct lock boundary.
6. Reopen tests cover a clean WAL close and recovery from retained WAL/SHM
   state, followed by `integrity_check` and the exact ledger.
7. Fault tests cover WAL sync, SHM map/lock, checkpoint publication, and
   provider barrier failures. Every failure is observable and later reopen
   either recovers valid committed state or reports a fail-closed error.
8. Durable provider tests distinguish confirmed SQLite/provider durability from
   volatile memory evidence. Remote-service tests remain ignored unless a real
   service is present; ignored tests are not acceptance passes.
9. Windows-specific host mapping, lock, close, and cleanup assertions remain
   enabled and are not replaced by leak-tolerant cleanup or Unix-only claims.

The final report will identify the supported scope for each backend, list the
exact executed tests, and call out any unverified platform or live-service
boundary.
