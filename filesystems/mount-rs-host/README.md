# Host filesystem and local block storage

`LocalBlockStore` is durable, SHA-256-addressed backing storage. It syncs successful
puts before acknowledgment. `DiskCachedBlockStore` is a separate, optional cache
for any immutable `BlockStore`, including providers with opaque block IDs.

## Persistent content cache (library layer)

```rust,ignore
let disk = DiskCachedBlockStore::open(
    backing,                // Arc<B>, B: BlockStore
    cache_root,             // existing, application-owned directory
    "volume-42-generation-7", // stable, non-secret backing identity
    1024 * 1024 * 1024,      // encoded bytes, including entry headers
    16_384,                 // entry bound
).await?;
```

Place the optional RAM cache above this wrapper. Share one instance through
clones/Arc; a second independent owner of the same scope is rejected by a file
lock. Different volumes or backing generations require distinct scopes. A scope
must not be reused for a backing store that can return different bytes under an
old block ID. SDK `split_with_disk_cache` and CLI structured-storage `disk_cache` configuration
now expose this layer. Production cluster lifecycle and aggregate resource
budgeting still require qualification.

Successful puts reach the authoritative provider before entering the cache.
Flush reaches that provider and propagates failure. Cache entry writes use an
atomic rename without fsync: losing a cached copy after a crash is harmless.
Reopened entries verify format, key, length and SHA-256 payload checksum before
use. Corrupt entries are removed and fetched again. Cache I/O failures disable
cache reads/admission until reopen; they do not turn a failed backing write into
success. Worker failure or poisoned state still returns an error.

Eviction and `clear()` remove cache copies only. Authoritative reclamation must
use the wrapper's `delete()`, which invalidates and syncs the cache directory
**before** starting backing deletion. Failed invalidation prevents backing
deletion, and cancelled backing deletion cannot leave a cache hit. Reclamation
outside the wrapper requires an equivalent invalidation/generation policy.

Indexed encoded bytes and entries are bounded. Oversized blocks bypass cache
admission. Reopen prunes excess entries and removes interrupted temporary files;
LRU order is rebuilt, not preserved. Four blocking workers and four bounded
admission copies are allowed per cache by default. Cache probes open an entry
under short index admission, then read and verify it concurrently outside the
index mutex. Completion checks publication identity before updating recency or
removing corrupt data. Insertions and evictions still serialize; backing cache
misses can overlap. Default limits are per-cache, not a server-wide
memory or I/O budget. `stats()` reports hits, misses, corruption, I/O failures,
eviction, admission rejection, residency, disabled state and aggregate timing.

An opened Unix file remains allocated after eviction unlinks it until its read
worker closes it. Reserve temporary disk headroom beyond indexed residency for
up to one maximum encoded entry per admitted worker, plus insertion temporary
files. This storage is worker-bounded; indexed residency is not a strict bound
on instantaneous physical disk use. Worker and invalidation guards remain owned
by blocking reads after caller cancellation, so delete/clear cannot race them.

This implementation currently requires Unix directory synchronization. It is
not a sandbox against a hostile process modifying the application-owned cache
directory. Unit/integration tests are not power-loss recovery qualification, and
local RustFS tier measurements are recorded in the
[read audit](../../benchmarks/read-audit/AUDIT_STATUS.md). They do not meet the
all-reads-under-10ms target or qualify AWS performance.

Disk read diagnostics expose `read_probes`, `read_queue_nanoseconds` and
`read_service_nanoseconds`. Queue time covers worker admission, blocking dispatch
and initial index-mutex admission, after acquiring the invalidation guard. Service time
covers the cache lookup, file read, checksum validation and LRU update, including
completion-mutex waiting and miss handling. Neither includes authoritative
backing reads or cache fills. Lock wait/hold counters measure only index critical
sections; each probe now has separate opening and completion sections.
Completed blocking probes count even if their async caller was cancelled after
dispatch. Stats snapshots do not count as probes. These are cumulative elapsed
times, not CPU times or latency percentiles; overlapping work must not be added
as end-to-end latency. The cluster audit exports per-phase counter deltas.

Fill diagnostics expose `fill_attempts`, `fill_wait_and_copy_nanoseconds` and
`fill_service_nanoseconds`. Attempts include puts and miss fills, even when
admission is rejected or caching is disabled. Preparation includes admission,
copying, worker dispatch and mutex waiting; service includes checksum, eviction
and insertion filesystem operations. Completed blocking attempts are counted
even if their async caller was cancelled. Authoritative backing I/O is excluded.

To share disk admission across volumes, construct one `DiskCacheBudget::new(
workers, fill_copies)` and pass clones to `DiskCachedBlockStore::open_with_budget`.
The budget includes directory opening and disk workers. Fill admission happens
before copying and remains held through insertion; each temporary fill copy is
at most 16 MiB. Worker permits remain held after async caller cancellation until
the blocking closure actually finishes. Existing `open` creates a private 4/4
budget. Shared admission does not bound total resident cache bytes, authoritative
provider buffers, returned read buffers, or unrelated runtime blocking tasks.

`LocalBlockStore::open_with_write_budget(path, budget)` optionally shares a
`LocalWriteBudget` across volumes. Construct one budget per server/storage domain
and clone it into each store. Limits are 1..1024 active mutations. Puts acquire
before copying their payload; puts and deletes retain permits inside their
blocking workers through completion, including after request cancellation.
Reads bypass this budget. This does not bound waiting requests or provide a
separate read thread pool. Ordinary `open` remains unrestricted; no production
limit has yet been selected or qualified by load tests.
