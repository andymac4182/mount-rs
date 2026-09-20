# Mount-free SQLite VFS design

`integrations/mount-rs-sqlite-vfs/` is a separate SQLite hosting boundary for
environments where mounting a filesystem is unavailable or undesirable. It
contains both the synchronous rollback-journal VFS API and the concrete
`StorageBackend<M, B, E>` bridge to mount-rs metadata and immutable-block
providers. It is not a conversion layer that makes an async `FsDriver` SQLite
safe by blocking on futures.

Workspace registration is intentionally owned by the main integration owner
and is now present in the root workspace. This crate does not modify the root
manifest or lockfile as part of its scoped implementation.

## Synchronous contract

The public `Backend`/`VfsFile` traits require a backend to provide:

- positioned reads, complete positioned writes, truncation, and file size;
- `sync(data_only)` that does not report success before the declared data or
  metadata durability barrier;
- rollback-journal lock transitions (`SHARED`, `RESERVED`, `PENDING`, and
  `EXCLUSIVE`) and reserved-lock probing visible across processes;
- delete, access, path normalization, and temporary-name operations.

The traits are synchronous and object-safe. An in-process mutex is only a
state guard; it is not a cross-process SQLite lock. A provider used in a
deployment must supply a real fenced lock authority and a real durability
barrier. The VFS therefore rejects or fails closed on missing capabilities
instead of advertising arbitrary async storage as SQLite-compatible.

## Implemented storage bridge

`StorageBackend<M, B, E>` is the implemented mount-rs bridge. It stores each
SQLite-visible file as a synthetic regular file in a mount-rs `Namespace`:

1. file bytes are split into the configured fixed-size chunks;
2. each chunk is written to the immutable `BlockStore`;
3. the block store is flushed before any metadata reference is published;
4. the complete namespace is published through the fenced
   `MetadataStore::publish(expected_revision, lease, namespace)` operation;
5. metadata is flushed before `sync` returns.

This is a page/file publication boundary with immutable blocks. It is not a
power-loss claim, a WAL implementation, or a substitute for a provider's
transaction and fencing guarantees. Unreachable blocks from a failed
publication remain subject to the block-store coordinator's normal garbage
collection policy.

`StorageOptions::require_durable` defaults to true and requires both providers
to report `durable() == true`. `volatile_for_tests` is explicit and is only a
functional engine-test mode; it must not be used to claim crash durability.
The bridge is generic over providers so the dependency surface stays small:

| Metadata provider | Block provider | Status |
| --- | --- | --- |
| `MemoryMetadataStore` | `MemoryBlockStore` | Implemented volatile engine gate; not durable |
| `SqliteMetadataStore` | `SqliteBlockStore` | Implemented durable local engine gate; reopen and integrity test covered |
| `PgliteMetadataStore` | `R2BlockStore` against RustFS | Standard RustFS harness passes real SQLite transactions, client reconnect, RustFS stop/start, and graceful PGlite process restart with exact ledger/integrity checks; abrupt-loss and power-loss qualification remain open |
| `SqliteMetadataStore` | `R2BlockStore` | API-compatible shape; VFS-specific live restart/fault qualification remains open |
| `PgliteMetadataStore` | `PgliteBlockStore` | API-compatible shape only; requires a real external runtime/executor and live restart/fault evidence |

The external-provider rows are intentionally not presented as production
acceptance. They need provider-specific checks for lease fencing, block
visibility, restart ordering, network failure, and durability.

## Conservative lock ownership

Rollback-journal correctness is currently implemented conservatively as
exclusive volume ownership:

- the first main-database `SHARED` acquisition obtains the provider's fenced
  writer lease and reloads the file under that authority;
- the same per-open handle token retains that lease through `RESERVED`,
  `PENDING`, and `EXCLUSIVE`, and releases it only at `unlock(NONE)` or close;
- every main-file read/write/truncate/sync and lock transition renews and
  checks the exact handle token; a volume-wide `writer_active` bit never
  authorizes another connection's write;
- journal handles are associated with the active main owner and cannot write
  or sync without that owner being renewed;
- a lease renewal, publication, flush, or release failure marks the bridge
  failed closed. It never fabricates `EXCLUSIVE` after authority is lost;
- because a Shared reader owns the volume, a second connection may receive
  `SQLITE_BUSY` while opening or acquiring `SHARED`. This deliberately
  sacrifices reader concurrency until a real cross-connection reader table and
  writer-exclusion protocol is implemented.

SQLite may issue a header probe before its first `xLock` callback. The bridge
allows only the bytes cached at `xOpen` for that bootstrap probe; the first
lock callback reloads under the fenced lease, and subsequent transaction reads
require the exact owner. The cached probe is therefore not treated as pager
locking or reader isolation.

The local mutex serializes bridge state, but the provider lease is the
cross-process authority. `StorageBackend` tests cover an unrelated handle
write rejection, stale-reader-to-writer promotion, and a separate process
against durable SQLite metadata/blocks. No implementation reports an
exclusive lock while another bridge owner is active.

## VFS lifecycle and unsupported modes

The VFS is rollback-journal only. It advertises no shared-memory or mmap
capabilities, rejects WAL open requests and `PRAGMA journal_mode=WAL`, and
returns errors from `xShmMap`/`xShmLock`. `require_full_sync` defaults to true
and rejects weaker synchronous pragmas. The crate makes no atomic-sector,
power-safe-overwrite, or power-loss recovery claim.

Registrations reject duplicate active names. Explicit `SqliteVfs::close()`
returns `Busy` while a callback or file handle is active, then unregisters a
quiescent VFS and detaches its backend/provider resources. Backend destruction
runs outside the registry lock, allowing reentrant destructors. Closing is
idempotent; the name can then be reused. A small backend-free callback
allocation remains for the process lifetime so pointers obtained before
unregistration never dangle. Use bounded stable registrations, not one per
request: closing releases backend resources but does not reclaim tombstones.
`VfsConnection` exposes a borrowed connection and closure-scoped mutable access.
File leases also protect name-opened or extracted connections independently
of the wrapper; wrapper ownership alone is not the lifetime safety boundary.

The `InlineExecutor` parks the calling thread on a real future wake rather
than busy-looping a noop waker. The optional `tokio-executor` feature exposes
`TokioExecutor`: outside Tokio it uses the supplied handle directly, and from
a Tokio multi-thread worker it uses `block_in_place` before entering the
handle. A current-thread Tokio runtime is rejected with a diagnostic panic
because a synchronous SQLite callback cannot block that runtime without a
deadlock; callers must use a multi-thread runtime or a dedicated runtime
thread. Providers that need a reactor, such as PGlite, must use this adapter
or another executor with equivalent fail-closed runtime behavior.

The optional `remote-harness` feature adds an ignored integration hook in
`tests/remote_storage_bridge.rs`. It composes `PgliteMetadataStore` with
`R2BlockStore` and runs SQLite through the storage bridge; no host-directory
backend and no `RUSTFS_SQLITE_METADATA_FILE` are used by that test. The hook
is deliberately opt-in and must fail when its service variables are missing,
so an ordinary package test cannot be mistaken for remote evidence. The
existing `scripts/test-rustfs.sh` supplies the contract:

- `PGLITE_DATABASE_URL` for the external PGlite PostgreSQL-wire server;
- `R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`, and
  `R2_SECRET_ACCESS_KEY` for the loopback RustFS S3-compatible endpoint;
- `RUSTFS_TEST_PREFIX` for a run-scoped object prefix.

The bounded harness command is:

```text
RUSTFS_COMBO_NAME=sqlite-vfs-remote \
RUSTFS_COMBO_COMMAND='cargo test -p mount-rs-sqlite-vfs --locked --features remote-harness --test remote_storage_bridge -- --ignored --exact remote_pglite_rustfs_sqlite_vfs --nocapture --test-threads=1' \
sh scripts/test-rustfs.sh
```

The test marks the PGlite provider durable only because the harness launches
its configured persistent data directory; that is an explicit provider
assertion, not proof of power-loss durability. The remote hook proves an
actual network-backed SQLite round trip and provider reconnect, but does not
close the crash, fencing, network-fault, or power-loss gates below.

The built-in `HostDirectory` is a native-directory reference backend for Unix
and Windows. It uses ordinary files, durable file-handle sync, and three
sidecar lock files per main database so independent processes observe
rollback-journal lock contention. The sidecars are deliberately separate from
SQLite's database lock-byte ranges: they make this VFS's own processes agree,
but they are not interoperable with a native SQLite Win32 VFS or an unrelated
VFS using the same database path. It is a reference implementation and test
oracle, not evidence that a remote, object-store, NFS, or existing mount-rs
backend has the same guarantees.

### Windows comparison boundary

The Windows implementation was checked against SQLite's official `os_win.c`
policy. `xSync` flushes the file handle for both normal and full sync requests,
matching `FlushFileBuffers`; transient delete failures caused by sharing,
locking, antivirus, or indexing are retried with a bounded linear backoff.
SQLite's native Win32 `xDelete` treats `syncDir` as unused, so this backend also
ignores that request on Windows and does not claim directory-entry durability.
On Unix, `syncDir=true` opens and syncs the containing directory before
returning. Neither path is a power-loss test or a guarantee for a filesystem
that does not honor its host sync primitive.

The Windows lock calls use `LockFileEx`/`UnlockFileEx` on the VFS sidecars and
fail closed on contention. This is sufficient for the backend's own
cross-process lock tests, but it is intentionally not a claim of native
SQLite Win32 lock-byte interoperability. A Windows cross-target build and
native test run remain required before accepting that platform.

## Verified and unchecked gates

`tests/sqlite_engine.rs` runs the bundled SQLite engine through `HostDirectory`
and covers the complete nine-cell `DELETE`/`TRUNCATE`/`PERSIST` ×
`NORMAL`/`FULL`/`EXTRA` rollback-journal matrix. Each cell asserts the effective
`PRAGMA journal_mode` and `PRAGMA synchronous` values after configuration and
after each transaction phase, records an exact ordered binary row ledger across
commit and rollback, checks the journal/WAL/SHM artifacts, runs
`PRAGMA integrity_check`, and reopens the database for a second ledger,
configuration, artifact, and integrity check. The same suite also covers
binary data, WAL rejection, duplicate-registration and name-based lifecycle
behavior, two-connection contention, child-process contention, and an injected
sync failure.

`tests/storage_bridge.rs` runs the complete nine-cell rollback matrix through
each of the volatile memory and durable SQLite metadata/block provider pairs
(18 cells). Memory reopen reuses the same volatile stores; durable reopen uses
fresh providers over persisted files. It covers fixed
chunk publication, durable reopen/integrity, exact per-handle authorization,
stale-reader promotion rejection, separate-process durable lease checking, and
faults injected before block-barrier completion and metadata publication. The
unit suite also verifies that `InlineExecutor` completes a future that yields
`Pending` and wakes its parked thread.

With `tokio-executor`, the unit suite additionally drives a yielding future
from a multi-thread Tokio runtime through `TokioExecutor`. The
`remote-harness` test is ignored by default and is only evidence when run
against the actual PGlite and RustFS services under the contract above.

These are component and local-provider gates, not full SQLite hosting
acceptance. The following remain unchecked and must be recorded before any
broader claim:

- live R2 and PGlite combinations beyond the opt-in round-trip/reconnect hook,
  including lease fencing and failure behavior;
- multi-process lease fencing across provider instances and restart after
  expired leases for every remote provider;
- injected block, metadata-publication, network, and process-crash faults;
- actual power-loss/restart durability on each target filesystem and storage
  service;
- WAL/shared-memory support, which remains intentionally unsupported;
- concurrent-reader locking, which remains intentionally sacrificed by the
  conservative exclusive-volume policy.

The bridge API is versioned by `STORAGE_BRIDGE_VERSION` (currently `1`). Any
change to chunk layout, lease ownership, publication ordering, lock policy, or
durability claims requires a corresponding versioned review and fresh engine,
multi-process, restart, and fault evidence.
