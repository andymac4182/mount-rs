# Rivet Actors source review

## Review basis

This is a source-level review of the pinned Rivet Actors repository, not a
README-only review or a claim that Rivet is a drop-in FoundationDB backend.

- Repository: <https://github.com/rivet-dev/actors>
- Reviewed commit: [`78336a1a0ee33bb45e6b15e89963cbe91713353b`](https://github.com/rivet-dev/actors/commit/78336a1a0ee33bb45e6b15e89963cbe91713353b)
- Commit date: `2026-09-18T15:40:41-07:00`
- Commit subject: `fix(depot): import BucketId in the inline compaction workflow tests`
- Review method: detached checkout of that exact SHA, followed by inspection of
  the VFS, commit, transaction, driver, recovery, and Cargo-manifest sources.

The repository is licensed under the Apache License 2.0. The exact snapshot's
root `LICENSE` is Apache-2.0; the root `Cargo.toml` declares
`license = "Apache-2.0"`, and the relevant package manifests inherit
`license.workspace = true`. This review copied no source code, and no Rivet
code, dependency, or license text was added to mount-rs. The license finding is
provenance for this review, not a decision to reuse implementation code.

## The FoundationDB boundary

Rivet's relevant storage abstraction is **UniversalDB (UDB)**. It is not a
direct FoundationDB Rust client integration.

The exact snapshot contains these manifest references:

- `Cargo.toml` — `foundationdb-tuple = "0.9.1"`
- `engine/packages/pegboard/Cargo.toml` — `foundationdb-tuple.workspace = true`
- `engine/packages/universaldb/Cargo.toml` — `foundationdb-tuple.workspace = true`

There is no direct `foundationdb` Rust client dependency in the reviewed
manifests. The UDB implementation has a Postgres driver under
`engine/packages/universaldb/src/driver/postgres/` and a RocksDB driver under
`engine/packages/universaldb/src/driver/rocksdb/`. `foundationdb-tuple` is tuple
encoding support; it does not make the Postgres/RocksDB UDB implementation a
direct FoundationDB client.

Consequently, the observations below are transaction, fencing, VFS, and
recovery design lessons. They are not evidence for FoundationDB-specific
semantics, native-client setup, or FoundationDB transaction limits.

## Inspected paths and findings

### SQLite VFS and transport

- `engine/packages/depot-client/src/vfs.rs`
  - `io_sync` waits for `flush_dirty_pages()` and the backend commit result
    before returning. A successful SQLite sync therefore cannot be reported
    before the backend's durability acknowledgement.
  - Commit staging uses a bounded threshold (`COMMIT_STAGE_THRESHOLD_PAGES =
    320`). A staged commit is not visible until finalization; a failed whole
    commit remains invisible, and an abandoned attempt can be reclaimed when
    the same transaction id is reopened.
  - The VFS has an explicit lost-ack test,
    `lost_commit_response_fails_later_on_head_fence_mismatch`. A commit can be
    applied while its response is lost; a later retry is then rejected by the
    head fence and the VFS becomes fatal. An unapplied transport failure is
    separately allowed to retry from the same head.
  - `xLock`, `xUnlock`, and `xCheckReservedLock` are intentional no-ops only
    under the documented invariant of one actor process per actor id,
    `locking_mode=EXCLUSIVE`, and one SQLite connection. This is not a general
    multi-client SQLite locking strategy.
  - `xSync`/`xClose` surface commit errors rather than hiding them in a later
    asynchronous callback.
- `engine/packages/depot-client-embedded/src/lib.rs`
  - `EmbeddedDepotSqliteTransport` wraps `Arc<depot::conveyer::Db>` and routes
    normal and staged commit operations to the Depot/UDB backend.

### Atomic publish and bounded staging

- `engine/packages/depot/src/conveyer/commit/apply.rs`
  - `Db::commit_with_options` opens a UDB transaction, checks the expected
    branch head, writes deltas/PIDX/head/commit/versionstamp/quota state, and
    only updates process-local publication/compaction state after the UDB
    commit is durable.
- `engine/packages/depot/src/conveyer/commit/publish.rs`
  - Single-shot and staged paths share the same publish sequence. The
    transaction writes delta chunks, PIDX, head, versionstamped commit/VTX
    state, and quota together.
- `engine/packages/depot/src/conveyer/commit/stage.rs`
  - Stage begin allocates `head + 1` and can reclaim an abandoned stage at the
    same transaction id. Each segment is re-fenced against the current head
    and generation. Finalization validates the expected generation, transaction
    id, and staged segment list before making the commit visible.

These paths support the useful invariant that data publication is atomic and
bounded, but they do not remove the need to design mount-rs chunk limits and
FoundationDB transaction budgets explicitly.

### UDB transaction, retry, and fencing behavior

- `engine/packages/universaldb/src/database.rs`
  - `Database::txn` is an automatic retry wrapper around the driver. Attempts,
    throttling, and the final commit are handled by the wrapper rather than by
    each metadata call site.
  - System wall-clock time is used for metrics such as
    `LAST_TX_COMPLETED_EPOCH_MS`; this is not a lease oracle.
- `engine/packages/universaldb/src/error.rs`
  - The error model distinguishes `NotCommitted`, `TransactionTooOld`, and
    `MaxRetriesReached`. Retryability and maybe-committed state are explicit
    parts of the driver contract.
- `engine/packages/universaldb/src/driver/postgres/database.rs`
  - The custom Postgres UDB schema includes `kv`, `udb_lease` with epoch and
    durable-version fields, `udb_version_seq`, and
    `udb_applied(client_node_id, client_seq, commit_version)` for failover
    deduplication.
  - Deduplication rows are retained long enough for resends before garbage
    collection. This is a UDB/Postgres mechanism, not FoundationDB commit
    behavior.
- `engine/packages/universaldb/src/driver/postgres/commit.rs`
  - A stable `(client_node_id, client_seq)` key is reused across NATS/leader
    failover resends, providing an at-most-once apply key.
  - After its bounded submit attempts are exhausted, this driver reports
    `NotCommitted`. That outcome policy must not be copied into mount-rs without
    reconciling it with mount-rs's explicit fail-closed handling for an
    applied-but-unacknowledged metadata transaction.
- `engine/packages/universaldb/src/driver/postgres/transport.rs`
  - The transport distinguishes `Committed { commit_version }` from
    `Conflict` and carries the dedup key used by the commit path.
- `engine/packages/universaldb/src/driver/postgres/resolver/mod.rs`
  - A dedup hit returns the previous commit version without applying the
    request twice. The resolver applies winners, records `udb_applied`, and
    advances `udb_lease.durable_version` with an epoch check in one SQL
    transaction. A failed epoch update rolls back and returns `LostLease`,
    fencing a zombie leader.
- `engine/packages/universaldb/src/driver/rocksdb/`
  - The alternate local UDB driver confirms that the public abstraction is
    deliberately broader than FoundationDB and that backend-specific retry and
    durability behavior must remain explicit.

The reusable ideas are stable operation identity, durable deduplication, and a
server-side epoch fence. The backend-specific retry and maybe-committed rules
must be re-derived for FoundationDB rather than inferred from UDB.

### Recovery scope

- `engine/packages/depot/src/recovery.rs`
  - `scan_hot_shard_history_corruption` is a bounded, snapshot-oriented check.
    It follows current pointers and selected shard versions, checks commit and
    PIDX ownership, and does not scan all retained/staged history, invoke
    SQLite, or use the normal read path.
  - Missing head/commit metadata and malformed selected shard or commit rows
    produce errors. Recovery therefore fails closed on the metadata needed to
    establish a safe view.

## Mount-rs implications

1. Keep metadata operations non-idempotent unless they have a stable operation
   identity and an explicit reconciliation protocol. An applied-but-lost
   FoundationDB acknowledgement must not be replayed as a fresh acquire,
   renew, release, or publish; returning a fail-closed I/O error is safer than
   converting the ambiguity into an ordinary conflict.
2. Keep retries for content-addressed block writes separate from metadata
   publication. A duplicate block write can be safe when the key and bytes are
   identical; a lease or head mutation cannot be treated the same way.
3. Do not adopt Rivet's no-op SQLite lock methods for mount-rs. The invariant
   that makes them safe is narrower than mount-rs's multi-client and
   cross-process use case.
4. Preserve bounded chunk/manifest transactions and make the visibility point
   explicit. A restart must see either the old manifest or the complete new
   manifest, never a partially published chunk set.
5. Keep recovery bounded and fail closed on missing or malformed manifest,
   generation, or fencing metadata. Rivet's recovery shape is a useful limit on
   recovery work, not proof of mount-rs backend correctness.

## Non-reuse statement

No Rivet source code was copied, translated, vendored, or linked into
mount-rs. No Rivet crate was added as a dependency. This file records findings
from the pinned source snapshot only.
