# TiDB contexts and storage diagnostic profiles

`Filesystem::split` and the private TiDB `connect` constructors keep their existing ownership. An application serving many Drives can construct `StorageContext::new(max_tidb_connections)` and pass it to `Filesystem::split_with_context` or `split_with_context_and_block_decorator`. The default context limit is 16 sessions for each exact TiDB connection string. Credentials, database, TLS and session options are part of that identity; contexts do not share resources across server instances. Equivalent differently spelled URLs may have separate pools.

The server owns the context across all partitions. Stop admissions, close transports, shut down all filesystems, then call `StorageContext::close`. Filesystem shutdown and failed opens do not disconnect sibling Drives. Closing the context is permanent. Its first poll initiates closure for every pool before waiting for checked-out sessions to drain. Cancelling that wait cannot leave later pools accepting work; calling `close` again resumes waiting for completion. Provider stores keep an Arc to the context's pool state until their futures finish. The low-level `TidbPoolContext::metadata` and `blocks` constructors offer the same ownership contract without the SDK.

Each context initializes the metadata schema and block schema once, with failed or cancelled initialization remaining retryable. Every metadata volume still initializes its own exact key independently. Every new pool session uses the existing pessimistic transaction, repeatable-read and autocommit verification. The provider continues to return ambiguous commit failures without replay.

The remote service configuration accepts `tidb_pool_max_connections` (positive integer, default 16). The saturation harness accepts `MOUNT_RS_REMOTE_SATURATION_TIDB_POOL_MAX` (default 16, range 1–1024), constructs one context per server across all Drives/partitions, and records the bound in its artifact. Private provisioning and fresh verification retain independent connections.

## Inode startup and Node API

`inodeUpdates: true` is available in `createChunkedDriver` with `concurrentWrites: true` and no `ownershipMode`. Startup can recognize a peer's completed inode enrollment only by independently reading exact inode authority, verifying the existing block marker without creating one, and validating a coherent snapshot. A previously observed backing must match. Missing markers, changed backings, malformed snapshots and ambiguous publication acknowledgements remain failures. Non-opt-in clients remain fenced.

## Explicit benchmark profiles

`benchmarks/storage/runner.mjs` accepts `--layout legacy|inode` and `--workload lifecycle|steady-overwrite`; defaults remain `legacy` and `lifecycle`. Inode layout requires split storage. Both labels are recorded in artifact configuration and successful size results. FoundationDB inode layout selects revision CAS and omits the incompatible shared-provider authority prefix.

The lifecycle profile measures new-file write, full-byte read and unlink (three operations). The steady profile precreates one file and opens one handle per worker, then measures interior overwrite and full-file read (two operations). Each lane encodes a positive, nonrepeating generation in up to seven payload bytes; setup is generation zero. The encoding supports every JavaScript safe-integer generation, and the oracle checks the full file including both unchanged boundary bytes. Tiny payloads cap total iterations before setup (255 for one byte, 65,535 for two bytes), because any worker may receive every iteration. File creation, handle open/close and unlink are excluded from steady wall time; final cleanup remains checked. Operations with unresolved timeouts are not replayed on their handles.

The existing W26 Ozone floor remains 1,000 IOPS with the same lifecycle workload. Its validator rejects inode and steady-overwrite artifacts. These profiles expose different structural publication costs and are not interchangeable qualification evidence.

The focused local TiDB fixture is a single PD/TiKV/TiDB v8.5.7 topology in a VM below the replicated 10 GiB floor. Passing startup, lifecycle and bounded-session tests is diagnostic correctness evidence; it does not establish replicated durability, cross-host capacity, or the expanded production target. Paired provider measurements and full-scale qualification are tracked separately.
