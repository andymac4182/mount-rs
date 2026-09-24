# Archil SQLite review and mount-rs support proposal

Review date: 2026-09-24. mount-rs revision: `db2cc4fb326c123fbc376bb522175befe2a0298f`.
Archil SDK source revision: `f8c8ae5920a4450cfec2b0fb8d48cc8f42247923`.
Published `@archildata/sqlite` package inspected: **0.0.16**, with peer `disk` **1.5.0**, Node >=22.
This is a source/documentation review and a local component test result, not a live Archil or mount-rs production qualification.

## Recommendation

**Scope correction:** The user's primary objective is ordinary local SQLite through filesystem mounts. See [the local mount and adapter review](archil-local-mount-sdk-review.md) for the primary recommendation. Archil supports that path without its SQL library. The service proposal below addresses only optional remote SQL access.

Build an optional managed SQLite service over mount-rs's existing VFS. Route multiple remote clients to one fenced execution owner per database; serialize writes and keep WAL shared memory within that owner. Start with rollback journals and FULL synchronization on a qualified persistent provider pair. Add process-local WAL only after lifetime ownership and takeover/recovery are tested.

Archil's publicly visible design does not require distributed WAL shared memory. Its SQLite package invokes ordinary Node SQLite remotely while its filesystem ownership layer excludes competing writable clients. That is a practical model to emulate. General-purpose simultaneous SQLite access through independent shared mounts is a separate, substantially harder support commitment.

## What Archil actually does

Sources: [SQLite docs](https://docs.archil.com/compute/sqlite), [SDK source](https://github.com/archil-data/archil-sdk/tree/f8c8ae5920a4450cfec2b0fb8d48cc8f42247923/packages/sqlite), [published package](https://www.npmjs.com/package/@archildata/sqlite/v/0.0.16).

The package is a small TypeScript adapter, not a SQLite fork or client-side database engine. `src/adapter.ts` constructs a Node runner using `node:sqlite` / `DatabaseSync`, and submits it via the top-level `disk.exec({ disks, command })` API. Each invocation opens SQLite, prepares statements with bound parameters, executes, and closes the connection. Remote values and results encode blobs and big integers explicitly. SQL template substitutions become `?` parameters.

* `createDatabase` creates parent directories and initializes the database while requesting a writable root mount queued for up to 5,000 ms.
* `getDatabase` returns a client handle without remotely checking existence; a missing file fails at query time.
* `read` requests a read-only mount and read-only SQLite connection. `write` requests writable ownership with a 5,000 ms mount queue.
* Nested writes check out the database's **parent directory**, covering creation/deletion of journals and other sidecars. Databases in the same directory consequently share this ownership contention boundary; root databases take root ownership.
* `transaction` sends all operations in one invocation. Writable transactions use `BEGIN IMMEDIATE`; read-only transactions use `BEGIN`. Failures attempt rollback and always close the connection.
* Query operations are deferred/claimable: collecting `.run()` operations into a transaction does not execute them separately. Tests cover this behavior and prevent reuse of already-started operations.
* The adapter does not explicitly set `journal_mode` or `synchronous`. Sidecar support is not evidence that cross-client WAL readers are safe. No distributed SHM protocol is visible in the package.

[Serverless compute](https://docs.archil.com/compute/serverless-execution) runs in colocated Linux microVMs. The `disk` SDK passes `readOnly`, `queueMs`, and `checkoutPaths` to the multi-disk execution API; its public tests verify request shaping. The CLI exposes remote execution, while the Linux `archil` client handles mounts, checkout/checkin, and cache controls. These client packages expose requests; they do not reveal the proprietary server's complete fencing or persistence implementation.

## The ownership and durability contracts

[Sharing disks](https://docs.archil.com/concepts/shared-disks) describes exclusive disk mounts, shared mounts with subtree delegations, and conditional server-side operations. A delegation excludes competing writes to its subtree, including object API writes. It is released by checkin or unmount. Orphaned delegations can be revoked/forced; the docs warn that in-progress writes from the former client can be lost.

[Application compatibility](https://docs.archil.com/details/support) explicitly limits `flock`/`fcntl` coordination to processes within one client. Across clients, ownership is provided by checkout/checkin. Conditional filesystem mutation support must not be confused with safely running SQLite's multi-file protocol from multiple owners.

[Consistency](https://docs.archil.com/details/consistency) and [architecture](https://docs.archil.com/details/architecture) promise successful fsync only after redundant multi-AZ persistence in the Archil storage system. Synchronization to an attached object store is eventual. Therefore their durable storage tier, rather than eventual S3 export, is the commit barrier. Client caches can lag; cache invalidation and expiry controls are explicit.

These are vendor contracts, not independently verified results. The reviewed public material does not establish how concurrent read-only invocations obtain SQLite-consistent snapshots while another client writes, how read-only WAL recovery works, or precisely how a timed-out invocation's commit outcome is reconciled. The compute docs also say a command may continue after an API timeout and describe trailing 128 KiB stdout/stderr limits. The adapter's result-on-stdout design needs bounded result sizes; no claim of unlimited query responses or exactly-once retry follows from it.

## mount-rs comparison

| Requirement | Current source boundary | Work needed |
| --- | --- | --- |
| Remote SQL API and TypeScript ergonomics | Rust VFS exists; Node filesystem/provider APIs are separate | Optional database service, Rust/TS clients, CLI, typed values and errors, parameter binding, batched transactions |
| Cross-client write exclusion | Fenced metadata writer lease, revision publication | Database identity/routing, bounded queue, owner epoch, lifetime ownership for WAL, takeover and stale-owner tests |
| Rollback journal storage | Conservative exclusive-volume locking; durable local SQLite pair | Qualify each production provider pair and service crash/retry boundary |
| WAL | StorageBackend ProcessLocal; HostDirectory HostLocal; Distributed reserved | Keep all live connections in one shared backend/process; reject broader scopes |
| Sidecar authority | VFS owns auxiliary files under its lease | Unified database/journal/WAL access policy; block raw concurrent mutations |
| Ordinary files alongside documents | VFS encodes full names as synthetic root entries | Namespace/path integration or explicit managed DB namespace plus backup/import/export |
| Durable acknowledgments | block put -> block flush -> fenced metadata publish -> metadata flush | Verify provider-specific guarantees and acknowledged commit ledger through failure/recovery |
| Performance and scale | whole-file Vec reads; every chunk submitted on file publication; WAL writes can publish eagerly | Positioned page cache, dirty extent updates, safe batching, bounded memory, benchmark small/large databases |
| Platform/support claim | Local component tests and retained historical native packets | Exact-revision hosted/provider/native matrix and explicit supported profiles |

Relevant source: `bindings/mount-rs-sqlite-vfs/src/lib.rs` (capability gates), `src/storage.rs` (bridge), `tests/sqlite_engine.rs`, `tests/storage_bridge.rs`, `tests/remote_storage_bridge.rs`, and `docs/sqlite-reliability-matrix.md`.

The encoded synthetic-root layout is not currently an ordinary `apps/app-1/main.sqlite` inode beside user Markdown files. Reusing that provider namespace directly for a workspace would need an explicit reviewed compatibility design. For an initial service, separate managed database volumes and a controlled SQLite backup/export path are viable; document that difference from Archil.

A provider transaction lease alone is insufficient for process-local WAL failover. Two independent owners must never maintain independent wal-index state against the same live database. Add an ownership coordinator that fences the complete connection/WAL lifetime, or first deliver rollback mode. Drain/close before transfer, and recover durable WAL under sole replacement ownership. Every storage publication must reject an expired epoch; a routing lock without storage fencing is insufficient.

The existing native NFS diagnostics document a competing writer acquiring `BEGIN IMMEDIATE` through a second view, plus shared two-CLI application failures. One-view rollback results do not qualify shared mounts. Preserve the CLI's unsupported topology rejection. [SQLite WAL](https://sqlite.org/wal.html) requires shared wal-index coordination; [SQLite over networks](https://sqlite.org/useovernet.html) explains why a server near the data is preferable to remote filesystem database access.

## Proposed delivery sequence

1. **Support contract:** Publish separate profiles for managed remote SQL, single-host native SQLite, and unsupported independent multi-host raw SQLite. Choose one persistent provider pair for initial production acceptance. Define database identity, maximum size, transaction/query limits, commit semantics, raw-file restrictions, and backup behavior.
2. **Minimal managed service:** Add a separate hosting crate/module and explicit CLI command. Use dedicated blocking SQLite workers and stable bounded VFS registrations. Provide create/get/query/transaction/status; typed null/number/int64/blob results; SQLite-enforced read-only execution; deadlines, cancellation and bounded queues/results. Use rollback journal + FULL initially. No serverless microVM infrastructure is required merely to serve SQL; microVM isolation is a separate need if arbitrary code execution is offered.
3. **Reliable retries and recovery:** Store an idempotency record and request hash atomically with mutations in the same database transaction. A lost response must reconcile the existing result instead of rerunning SQL. Test crash after commit/before response, queue timeout versus execution timeout, lease loss, stale owner publication, takeover, hot-journal recovery, and exact acknowledged ledgers.
4. **Concurrent reads/WAL:** Reuse one backend state per database owner, add connection pooling and bounded checkpoint policy, and qualify long readers, checkpoint/reset, owner death and recovery. Allow only ProcessLocal WAL for the storage bridge. Cross-host clients communicate with the owner over RPC; they do not directly share SHM. Multiple read replicas require a separate snapshot/replication protocol.
5. **Workspace integration and operations:** Decide between real namespace file integration and managed import/export. Database and sidecars must have one authority; backups must use SQLite backup or an explicitly quiescent protocol. Add check/status/backup/restore CLI support, ownership/queue/checkpoint telemetry, resource limits, provider-specific fault campaigns and supported-platform CI. Fix older rollback-only prose in the VFS design doc to reflect the current opt-in WAL implementation.
6. **Optimization after correctness:** Track page/chunk upload volume, metadata publication count, memory per database, queue delay and p95/p99 commit latency. Current whole-file publication needs dirty-page/extents work before claiming low-cost large-database hosting. Preserve block-before-metadata barriers during optimization.

## Verification performed

`./scripts/cargo-shared test -p mount-rs-sqlite-vfs --locked` on macOS exited 0: **5 unit + 16 engine + 16 storage-bridge tests** passed. This includes local rollback matrices, process-local WAL persistence, host-local cross-process WAL, locking, lifecycle, and selected barrier/publication failure tests. Child helper tests are included in those counts.

PGlite and remote harness features were not enabled; those binaries ran zero tests. No live Archil calls, remote provider restart campaign, native mount rerun, Linux/Windows execution, power-loss simulation, or performance qualification was performed. Existing native observations above are repository-recorded evidence, not fresh runs. No implementation was changed; the pre-existing modified formal-verification plan was preserved.
