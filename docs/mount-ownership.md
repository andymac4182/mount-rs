# Mount ownership modes

Ownership describes one filesystem coordinator, not a hostname. Several local processes or containers can share its mount. Separate coordinators on one host still need shared mode or exclusive ownership transfer.

## Exclusive

Explicit exclusive mode acquires the existing provider-enforced volume writer lease and enables writeback. Mutations become visible in the coordinator's local namespace immediately. Publication and block flushes are deferred until file fsync/datasync, directory fsync, syncfs or orderly shutdown. Native FUSE also synchronizes through its default FLUSH handler. Immutable block puts still occur during writes; this optimization batches namespace publication and durability barriers, not byte uploads or dirty page coalescing.

Synchronization flushes new blocks before atomically publishing their references under the lease and persisted revision. The metadata durability barrier completes before sync reports success. A stale owner, failed publication or canceled required barrier fails closed. Garbage collection drains pending publication before reclaiming blocks. Local mutation generations remain independent of the last persisted revision so overlapping writes retain conflict detection.

Unsynchronized updates can be lost if the mount process crashes. There is no new periodic background flush: applications must use their required sync boundaries, and consumers must call shutdown before disposing of the filesystem. Volatile providers remain volatile even with successful sync.

## Shared

Explicit shared mode uses persisted directory checkout authority (MRC3). One coordinator owns one directory and its descendants; independent clients can own disjoint directories in the same volume. The provider rejects overlapping claims, writes outside the owned subtree, cross-boundary rename/hardlink, stale tokens and namespace header or inode allocator abuse. File data access, including read-only SQLite opens, requires ownership. Namespace discovery remains available before checkout.

Shared publishes immediately using revision CAS and full provider-side namespace delta validation. It does not batch metadata or provide distributed POSIX file locks. Processes using one mount use local locks; SQLite databases must stay wholly inside one owned directory. Checkout does not expire by client clocks. Checkin refuses open handles, completes durability barriers and releases the exact fence. Crashed owners require explicit recovery with the observed fence; recovery cannot retire a newer owner. Grants and retry receipts are bounded to 4096 identities; offline protocol migration is required to compact receipts; a receipt compactor is not currently provided. FoundationDB also enforces its 100 KB value limit, which can reject authority state earlier with EFBIG. Existing grants remain fenced when capacity is exhausted.

Native handoff requires unmounting the old mount before checkout by a fresh mount. This retires its kernel caches. N-API ownership controls refuse active or uncertain native mounts. A failed/canceled native creation retains an uncertainty marker; establish kernel retirement and use offline expected-fence recovery before opening a fresh coordinator. Initial native qualification targets Linux FUSE; shared CLI automatic selection uses FUSE and rejects NFS/9p. No live cache revocation or macOS FSKit qualification is claimed. SQLite metadata/block files are same-host only; remote clients need appropriate shared providers. Distributed shared GC remains unsupported.

The legacy `concurrent_writes: true` / `concurrentWrites: true` option without explicit ownership mode continues to use MRC2 revision CAS without directory grants. Existing initialized volumes require explicit offline enrollment after all old mounts stop. MRC3 persists a distinct protocol marker so old Legacy/MRC2 publication paths fail closed.

## Configuration

Rust SDK:

```rust
use mount_rs_sdk::{Filesystem, OwnershipMode, SplitOptions};
let options = SplitOptions::memory("unique-client-owner", 4096)
    .with_ownership_mode(OwnershipMode::Exclusive);
let filesystem = Filesystem::split(options).await?;
// Use filesystem.driver(); synchronize as required.
filesystem.shutdown().await?;
```

Structured Rust and Node CLI storage configuration:

```json
{
  "version": 1,
  "driver": {
    "kind": "splitstore",
    "storage": {
      "ownership_mode": "exclusive",
      "metadata": { "kind": "sqlite", "path": "metadata.sqlite" },
      "blocks": { "kind": "sqlite", "path": "blocks.sqlite" }
    }
  }
}
```

For a delegated native mount, choose `ownership_mode: "shared"` and `checkout_path: "/tenant"`. The directory must exist. N-API accepts `ownershipMode: 'exclusive' | 'shared'` and `checkoutPath`; direct SDK clients can use `checkout_scope` / `checkin_scope` / `delegation_status` after opening Shared. The SDK also provides offline enrollment, state inspection and expected-fence recovery.
An omitted ownership mode preserves existing write-through behavior and the legacy concurrent_writes/concurrentWrites option. When both declarative CLI/N-API options are supplied they must agree. Rust builders use their usual last-setter semantics. SDK/core users can use with_writeback(false) after choosing Exclusive for comparison or write-through compatibility; writeback plus shared operation is rejected before opening providers.

## Qualification and performance

NFSv3 currently synchronizes every WRITE and replies FILE_SYNC, so it does not benefit from deferred write batching. The main performance targets here are the direct driver and exclusive Linux FUSE. macOS automatic NFS mounts retain that synchronization cost.

Run `benchmarks/ownership/run.sh` for repeatable release-mode comparisons including sync costs and provider operation counts. Native Linux qualification is the ignored `native_exclusive_sqlite` integration test with `MOUNT_RS_RUN_NATIVE_FUSE=1`. Benchmark results and the native environment are recorded separately; local provider measurements do not establish remote latency or power-loss guarantees. macOS FSKit, network-provider SQLite durability and live mounted handoff require separate qualification.

See [validation evidence](mount-ownership-validation.md) and [measured results](../benchmarks/ownership/RESULTS.md).
