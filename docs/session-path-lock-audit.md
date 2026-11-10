# Session-path lock audit

Every lock a read or metadata call crosses on the remote session path
(`RemoteFs` → HTTP → `VolumeSessions` → `OwnedHandleSession` → `ChunkedFs` →
block cache → block store), what was done about it, and what is left.

Scope: an exclusive, non-delegated owner (`OwnershipMode::Exclusive`, no
directory delegation), which is what the session server runs. Concurrent-writer
and delegated mounts still take the operation gate for reads.

Measure with `crates/mount-rs-remote/examples/session_latency.rs`
(`BENCH_SURFACE=direct` for the owner alone, `remote` for the HTTP/1.1 path).

## Result

Release builds run back to back on a 10-core Apple Silicon laptop that was
also in interactive use: 2000 files, 4 KiB reads, 16 closed-loop clients,
p50 / p99 / p99.9 in ms. "Before" is the commit this audit started from.

| Path | Before | After |
| --- | --- | --- |
| Direct read | 0.018 / 0.038 / 0.053, 0.85M ops/s | 0.001 / 0.002 / 0.007, 2.9M ops/s |
| Direct stat | 0.004 / 0.013 / 0.036 | 0.004 / 0.007 / 0.017 |
| Direct readdir (64 entries) | 0.006 / 0.018 / 0.046 | 0.006 / 0.018 / 0.025 |
| Direct open+close | 0.003 / 0.050 / 4.26, 0.72M ops/s | 0.002 / 0.004 / 0.013, 1.56M ops/s |
| Remote read | 0.196 / 0.363 / 0.831 | 0.192 / 0.407 / 1.705 |
| Remote stat | 0.196 / 0.356 / 0.431 | 0.196 / 0.355 / 0.431 |
| Remote open+close | 0.375 / 0.592 / 0.835 | 0.377 / 0.588 / 0.788 |
| Remote read, 6000 files, concurrent writer | 0.219 / 0.498 / 1.078 | 0.211 / 0.415 / 0.618 |

The remote rows without a writer are the same within run-to-run noise (the
p99.9 of a 2 s run moves by a millisecond between identical builds). The owner
is not what that path waits on: one remote client sees 0.056 / 0.118 / 0.154
ms, and the server gives 70k ops/s on two threads and no more on eight. What
remains at 16 clients is loopback syscalls and task wake-ups in hyper and
reqwest.

A second pass removed the locks the first one had kept and the shared cache
lines behind them. Same machine and settings, block cache on, against the
commit that ended the first pass:

| Path | First pass | Second pass |
| --- | --- | --- |
| Direct read | 0.001 / 0.002 / 0.003, 4.3M ops/s | 0.001 / 0.001 / 0.001, 6.6M ops/s |
| Direct stat | 0.004 / 0.007 / 0.008 | 0.004 / 0.007 / 0.007 |
| Direct readdir (64 entries) | 0.006 / 0.009 / 0.011 | 0.006 / 0.007 / 0.025 |
| Direct open+close | 0.002 / 0.004 / 0.004, 1.73M ops/s | 0.002 / 0.002 / 0.003, 2.14M ops/s |
| Remote read | 0.183 / 0.304 / 0.394 | 0.181 / 0.298 / 0.350 |
| Remote stat | 0.186 / 0.310 / 0.367 | 0.188 / 0.313 / 0.369 |
| Remote open+close | 0.372 / 0.549 / 0.751 | 0.367 / 0.531 / 0.598 |

The direct benchmark runs its clients on four threads. One client does 1.86M
reads a second, so 6.6M is 89% of linear; before the second pass it was 52%.
The readdir p99.9 difference is within run-to-run noise.

## `DrainGate`

`mount_rs_core::drain_gate::DrainGate` replaces every reader-writer lock on
this path whose shared side is hot. A tokio `RwLock` locks its waiter mutex
whenever a read guard is released, so a lock every operation shares becomes a
mutex every core contends on; this was the largest contention point left once
the operation gate was gone. `DrainGate` admits shared holders with one atomic
increment and releases them with one decrement. Its internal mutex is touched
only when an exclusive transition is queued or running. Transitions are
first-come-first-served, take priority over new shared entries, and both
futures are cancellation-safe. It has no runtime dependency.

A counter every core increments is still one cache line passed between them.
`DrainGate<N>` spreads it over `N` stripes, one per thread; a transition waits
for every stripe to empty. The filesystem's lifecycle gate, which every
operation enters, uses sixteen. Per-handle and per-session gates keep one.

## Removed from the read and open/close paths

| Lock | Where | Replacement |
| --- | --- | --- |
| Operation gate (`ChunkedInner::gate`, async mutex) | every read, stat, readdir, open | Not taken in exclusive mode; reads use the committed snapshot. |
| State mutex (`ChunkedInner::state`) for snapshots | every read, stat, readdir; fence check per session call | `StateCell`: an `ArcSwap<Namespace>` mirror refreshed when a state guard is released, plus a `locked_reads` flag that sends readers back to the mutex while the filesystem is failed, closed, or has uncommitted per-inode overlays. |
| State mutex for open and close | open-reference counts and fd numbers | `FastRefs`, a fixed table of atomic words (inode and count packed together), and an atomic fd counter. An open counts its reference and then checks a sequence that is odd while a mutex holder is deciding inode lifetimes from the references; if it moved, the open settles under the mutex. Close does the same in reverse. An inode that is retained for an open handle uses the mutex (see the second pass). |
| Lease mutex (`ChunkedInner::lease`) | lease validation and `checked_owner_fence` on every session call | `lease_cached_until_ms` and `lease_fence` atomics; the mutex is only taken once the cached deadline has passed. |
| Lifecycle `tokio::sync::RwLock` | shared by every read and write | `DrainGate`. |
| Per-handle gate (`ChunkedHandle::gate`) | every handle call | `DrainGate`; positional reads share it, cursor moves, writes and close drain it. |
| Handle state mutex (`ChunkedHandle::state`) | every handle call | `HandleState` atomics; writers hold the handle gate exclusively. |
| Block cache index (`CachedBlockStore::state`, async mutex) | every cached block read, with an LRU re-link and a `BlockId` clone | `std::sync::RwLock` taken shared on a hit. A hit writes nothing that other blocks share: it records the current epoch in its own entry (once per epoch) and bumps a per-entry hit counter. Eviction re-files entries hit since they were linked. |
| Block cache I/O gate (tokio `RwLock`) | every miss, put and flush | `DrainGate`. |
| Block cache write gate (async mutex) | serialized every put behind every other put | `DrainGate`: puts share it, flush takes it exclusively. Puts to a remote object store now overlap, as they already did without the cache; a flush still covers every put acknowledged before it. |
| Object-store provider cache (`ObjectStoreBlockCache`, mutex) | every block read from the production block provider: cloned the whole block and scanned the LRU queue linearly under the mutex | `RwLock` shared on a hit, shared `Arc<[u8]>` payloads copied after unlocking, epoch recency as above, and a `read_into` that copies only the requested range. |
| Session handle table (`OwnedHandleSession::state`, mutex) | every handle call's lookup | `std::sync::RwLock`, shared for lookups; since replaced by atomic slots (second pass). |
| Session handle entry (tokio `RwLock<Option<handle>>`) and `path_operations` (tokio `RwLock`) | every session call | `DrainGate` per handle and per session; the handle sits in a `OnceLock`. |
| Remote client handle (`RemoteHandle::state`, tokio `RwLock`) | every remote handle call | `DrainGate` plus an atomic state. |

Related changes on the same path that are not locks: `BlockStore::read_into`
(no full-block copy per read), binary search over extents, namespace clones and
drops moved off the async workers for large namespaces, and the lease gate held
across publication so a heartbeat cannot replace the fencing token mid-publish.

One correctness fix came out of this work. Once opens stopped taking the
operation gate, an open could land between an unlink's preparation and its
publication, and the exclusive publish path did not retain the inode for it: a
later read on that handle returned `ESTALE`. Publication now retains every
inode that is open and absent from the namespace it installs.
`lock_free_opens_and_closes_stay_consistent_with_unlinks` covers it.

## Removed in the second pass

Several of these share one shape: a fixed table of atomic slots probed from a
hash, where a reader loads a slot, compares the key and uses what it guards
without taking a reference count. Writers that are already serialized publish
into the table and retract from it.

| Lock | Where | Replacement |
| --- | --- | --- |
| State mutex and operation gate under `AtimePolicy::Strict` | every read (record) and stat (overlay) | `PendingAtimes`, a table of atomic (inode, time) pairs. A read records without the gate or the mutex; publication folds the table in and forgets only the times the installed namespace carries. |
| State mutex for every open and close while any unlinked inode was held open | opens and closes of unrelated files | A 64-bit filter of retained inodes. Only an inode whose bit is set takes the mutex. The filter cannot be dropped altogether: an unlink marks its inode retained before the namespace that removes it is installed, and a lock-free close in that window would leave the inode retained for good. |
| Lease mutex (`ChunkedInner::lease`) | lease refresh, publication | `ArcSwapOption<WriterLease>`. |
| Mutation queue mutex | every mutation's enqueue, every batch | A lock-free queue, an atomic length for the capacity bound and an atomic runner flag. A runner that finds the queue empty releases the flag and looks once more. |
| Per-block fill mutex, registered under the cache index write lock | every cache miss | A slot table of in-flight fills; followers wait on the fill's `DrainGate`. A miss no longer write-locks the index, so cold reads do not stall hits. |
| Block cache index lock on hits | every cached read | A slot table in front of the index. The index lock is now taken by misses, puts, deletes and hits on the few entries whose probe window was full. |
| Object-store provider cache lock on hits | every block read from the production provider | The same table. |
| `inflight_puts` mutex | every put, twice | A slot table of uploads in progress. |
| `VolumeSessions::state` | every request | An immutable snapshot behind `ArcSwap`; create and remove install a modified copy. Admission and expiry decide with one compare-and-swap on a per-session word, so a request is admitted before the session is retired or refused. |
| `OwnedHandleSession` handle table lock | every handle call, open and close | Atomic slots indexed by handle id. Ids still only grow; an id whose slot is occupied is skipped. |
| `MemoryMetadataStore` mutex | every load and version lookup | `RwLock`; loads and lookups share it. |
| `MemoryBlockStore` lock | every block read without the cache | A hash table of atomically replaced immutable chains. |
| Delegation, pending-checkout and lease-worker mutexes | delegated mode, startup, shutdown | `ArcSwapOption`. |

Shared cache lines removed alongside, none of them locks: the namespace's
reference count (reads, stats, opens and closes now use an uncounted guard),
the lifecycle gate's counter (striped), the reference-count slots and the fd
counter (each on lines of their own), and the payload reference count a cache
hit used to take.

## Kept, with the reason each cannot go

| Lock | Taken by | Why it stays |
| --- | --- | --- |
| State mutex (`StateCell::mutex`) | mutations and publications; readers only while the filesystem is failed, closed or has uncommitted per-inode overlays; opens and closes only for an inode that is retained or raced a publication | It makes a mutation's multi-field update (namespace, revision, orphans, references, failure) one step. Everything a read-only operation needs from it is mirrored in atomics. |
| Operation gate and lease gate (async mutexes) | mutations, publications, lease renewal; reads only in concurrent-writer and delegated modes | Publication is a compare-and-swap of one revision against the metadata store: two publishers cannot both succeed, so they are ordered here instead of failing and retrying against the provider. Concurrent mutations are merged by the queue above, not run in parallel. The lease gate keeps a heartbeat from replacing the fencing token in the middle of a publication. |
| Block cache and provider cache index (`RwLock`) | misses, puts, deletes, eviction | Eviction order and the byte budget span every entry. Hits no longer take it. |
| `DrainGate`'s internal mutex | exclusive transitions only | It holds the queue of waiting transitions and their wakers. Shared entry and exit never touch it. |
| `MemoryMetadataStore` (`RwLock`) | lease and revision transitions take it exclusively | Each is a check and an update of several fields that must be one linearizable step; that is the provider's contract. |
| SQLite providers' connection mutex | metadata and block I/O on SQLite | A SQLite connection is not usable from two threads at once. |
| `DiskCachedBlockStore` (`mount-rs-host`) | only with the optional disk cache tier | Every operation is a blocking disk read behind a four-permit worker pool, which bounds it long before its index mutex does. Not on the session server's path. |
| Route-lookup mutex (`IngressForwarder`) | route cache misses only | It is the wait that lets concurrent requests for one volume share a single registry lookup, followed by network I/O. |
| Session budget semaphore, HTTP `connection_permits` | session creation, connection accept | Admission limits; not per request. |
| hyper header-read timer, reqwest request timer, hyper-util connection pool | one registration per request on each side | Inside third-party crates. They are where the remaining mutex waits in the remote profile come from. |

## Still open

1. **Mutations are O(namespace).** Each mutation deep-clones the namespace and
   publishes all of it. Readers no longer block on that, but writes at 6000+
   files cost milliseconds and are the source of the residual read p99.9 when a
   writer is active. This is the largest remaining cost and needs a persistent
   map for `Namespace::nodes` plus incremental publication.
2. **HTTP/2 on one connection** runs at about half the HTTP/1.1 throughput
   because every stream shares h2's connection mutex. The CLI uses HTTP/1.1.
3. **Directory entries are scanned linearly** in `walk`; fine at 64 entries,
   visible in profiles for very large directories.
4. **`read_parallelism` is still 1.** A 4 KiB read touches one block, so it does
   not affect the target above; multi-block reads against a remote object store
   would benefit.
5. **Per-operation statistics** in the object-store provider are shared atomic
   counters written on every block read. They are not locks, but they are the
   next shared cache lines on that provider's hit path.
6. **Per-session and per-handle gates have one stripe.** Many cores working
   one session's path operations, or one shared handle, still pass that
   counter between them. Striping them costs two kilobytes per gate.
7. **A cache hit still writes the block's own hit counter**, so cores reading
   the same block share that line. Different blocks share nothing.
8. **Strict-atime reads were not benchmarked**; the benchmark has no switch
   for the policy. A test covers that they skip the gate and the mutex.
