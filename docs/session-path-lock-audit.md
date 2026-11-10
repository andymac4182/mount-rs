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

A third pass took each lock the second one had kept and either removed it or
measured it, and went after write latency, which the first two had not
measured at all. A whole-file write went from 1.06 ms to 0.010 ms with one
writer and from 31 ms to 0.12 ms with sixteen; the tables are under
"Removed in the third pass".

## `DrainGate`

`mount_rs_core::drain_gate::DrainGate` replaces every reader-writer lock on
this path whose shared side is hot. A tokio `RwLock` locks its waiter mutex
whenever a read guard is released, so a lock every operation shares becomes a
mutex every core contends on; this was the largest contention point left once
the operation gate was gone. `DrainGate` admits shared holders with one atomic
increment and releases them with one decrement. It has no lock inside it: a
transition takes a turn at a lock-free queue (`TurnGate`), raises a counter
that holds new shared entries back, and waits for the holders to leave.
Transitions are first-come-first-served, take priority over new shared
entries, and both futures are cancellation-safe. It has no runtime dependency.

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

## Removed in the third pass

The second pass left a list of locks marked "kept". Each was then either
removed or measured. Measurements used temporary counters at every remaining
acquisition site (try-lock first, time the wait on failure) under 16 and 64
clients running writes alone and all five operations together, on both
surfaces.

| Was | Now |
| --- | --- |
| Operation gate and lease gate: an async mutex with its waiters behind a mutex | `mount_rs_core::turn_gate::TurnGate`: one atomic flag and a lock-free waiter queue. Taking a free gate is one compare-and-swap; release hands the gate to the oldest waiter. Cancellation-safe. |
| `DrainGate`'s internal mutex, taken by every exclusive entry and exit (three per whole-file write through the cache's flush gate; 1.1 to 1.6 million acquisitions in a 3 s run) | Gone. Transitions queue on a `TurnGate`; the draining holder's waker sits in an `AtomicWaker`; waiting shared entries sit in a lock-free queue. |
| State mutex taken once per whole-file write to read the namespace and revision (422k acquisitions, 4656 contended in a 3 s, 16-writer run) | Write preparation reads the published namespace and a revision mirror without it (50k acquisitions, none contended, same run). |
| Publication scanning all 1024 open-reference slots (128 KiB of cache lines) and all 1024 pending-atime slots while holding the state mutex. This was the 16 µs mean wait other state-mutex users saw on the remote surface. | A bitmap marks the reference slots in use and a flag marks a non-empty atime table, so a publication visits only what is there. |
| `IngressForwarder`'s route-lookup mutex | `Coalesce`: a slot table of in-flight lookups; requests for one volume share a `TurnGate`. |
| `Mutex<Option<JoinHandle>>` in the session and registration supervisors | `JoinSlot`: the handle in an atomic slot, joiners ordered by a `TurnGate`. |
| Service session identity (`RwLock`), telemetry global (`RwLock`), remote client handle state and renewal lock (mutexes) | Atomic swaps, a `DrainGate`, a `TurnGate`. |
| Disk cache: one mutex around each whole fill, including two SHA-256 passes, the file write, the rename and an unlink that usually found nothing. 4000 fills from 16 readers waited 2639 ms in total for it (9.6 ms worst), and fills ran one at a time. | The mutex covers two short index updates per fill: reserve the space, then index the file. Hashing, writing and the rename happen between them. Same run: 15 ms total wait, 0.09 ms worst, 4000 fills in 450 ms instead of 890 ms. Its I/O and put/flush tokio locks are `DrainGate`s. |

Two things found on the way were not locks but cost more than any of them:

- **Every mutation copied the whole namespace three times.** `Namespace::nodes`
  was a `BTreeMap`, and a mutation cloned it to edit, cloned it again to hand
  to the provider, and the provider cloned it to keep. It is now `NodeMap`, a
  persistent 64-way radix tree: a clone is one reference count, an edit copies
  the few branches above the inode, and the serialized form is unchanged. The
  hand-offs to another thread that surrounded those copies are gone with them.
- **Whole-file writers starved and replayed.** The writer whose request started
  the batch runner kept the role for as long as others kept writing, so one
  writer finished a single write in the whole run (the multi-second maximum in
  the baseline). The role now passes to a task after the first batch. And a
  request prepared at an older revision was reported as a conflict and redone
  through open, write and close; with an exclusive owner every newer revision
  is its own, and the path, inode and layout checks already decide whether the
  request still applies, so it is rebased instead.

An attempt that was reverted: the first lock-free `DrainGate` and `TurnGate`
kept their wakers in `ArcSwapOption`. Every store to an `ArcSwap` walks the
debt list of every thread that has used one, and that showed up as 10% of
server CPU on remote writes and cut direct open+close from 2.4M to 0.85M
ops/s. `AtomicWaker` has no such cost.

Whole-file writes, 8 bytes each, memory providers, before (`f3f98e5d`) and
after; p50 / p99 / p99.9 in ms:

| Workload | Before | ops/s | After | ops/s |
| --- | --- | --- | --- | --- |
| direct, 1 writer, 2000 files | 1.057 / 1.149 / 1.589 | 941 | 0.010 / 0.013 / 0.019 | 102,443 |
| direct, 16 writers, 2000 files | 31.3 / 34.4 / 36.8 (max 4006) | 484 | 0.117 / 0.202 / 0.243 (max 0.45) | 131,858 |
| direct, 1 writer, 20000 files | 14.3 / 15.6 / 23.2 | 70 | 0.024 / 0.027 / 0.033 | 40,629 |
| direct, 16 writers, 20000 files | 384 / 415 / 4180 | 42 | 0.245 / 0.454 / 0.601 | 59,223 |
| remote, 16 writers, 2000 files | 27.2 / 36.7 / 40.4 | 578 | 0.768 / 0.964 / 1.045 | 20,828 |

Reads were re-measured and did not move outside run-to-run noise: direct
read 6.7M to 6.8M ops/s, stat 0.94M to 1.05M, readdir 0.64M to 0.94M (p99
0.010 to 0.006 ms), open+close 1.9M to 2.8M across repeated runs of either
build. Remote mixed p99.9 went from 0.71 to 0.43 ms for read and from 1.44 to
0.74 ms for open.

## Kept, with what was measured

| Lock | Taken by | Measured | Why it stays |
| --- | --- | --- | --- |
| State mutex (`StateCell::mutex`) | the batch runner and publications; handle writes and closes of written handles; readers only while the filesystem is failed, closed or has uncommitted per-inode overlays; opens and closes only for an inode that is retained or raced a publication | Direct, 16 clients, all operations: 44k acquisitions in 3 s for 1.3M operations, 1.7% contended. Remote, same mix: 187k acquisitions, 5.1% contended, before the publication scans above were removed. | It makes a mutation's multi-field update (namespace, revision, orphans, references, failure) one step. Everything a read-only operation or a write's preparation needs from it is mirrored in atomics. What still contends on the remote surface is the handle path that remote whole-file writes use (see below). |
| Block cache index (`RwLock`) | misses, puts, deletes, eviction; never hits | One write acquisition per put. With 16 writers, 7 to 10% of them wait, 1.5 to 2.5 µs each: about 0.2 µs per write on average. Read acquisitions: 460 in 3 s. | Eviction order and the byte budget span every entry. Replacing it means a different eviction policy, not a different lock; the measured cost does not justify that. The object-store provider's cache index has the same shape, behind network puts. |
| `MemoryMetadataStore` (`RwLock`) | lease and revision transitions take it exclusively | Never contended in any run: publications are already serialized by the operation gate. | Each is a check and an update of several fields that must be one linearizable step; that is the provider's contract. |
| SQLite providers' connection mutex | metadata and block I/O on SQLite | Not measured: not reachable with the memory providers the benchmark uses. | A SQLite connection is not usable from two threads at once. |
| Disk cache index mutex (`mount-rs-host`) | fills, deletes, clears; never hits | 7 µs mean hold, 0.09 ms worst wait under 16 concurrent fills. | Eviction and the byte budget, as for the block cache. File I/O no longer happens under it. |
| Session budget semaphore, HTTP `connection_permits`, disk cache worker and fill semaphores | session creation, connection accept, disk cache fills | | Admission limits; not per request on the session path. |
| hyper header-read timer, reqwest request timer, hyper-util connection pool, tokio's timer wheel | one registration per request on each side | About 1% of active samples in the remote write profile. | Inside third-party crates. They are where the remaining mutex waits in the remote profile come from. |

## Still open

1. **Remote whole-file writes take three round trips.** `RemoteFs` has no
   whole-file write operation, so `write_file` is open, write and close over
   HTTP, and on the owner each of those goes through the handle path and its
   own publication rather than the batch runner. That is why a remote write
   costs 0.77 ms against 0.19 ms for a remote read. A single session operation
   carrying the bytes would put remote writes on the batched path. It is a
   wire-protocol addition and was left for its own change.
2. **Providers still receive the whole namespace.** Copies inside the process
   are now O(1), but `MetadataStore::publish` takes a full `Namespace`, and a
   provider that serializes it does work proportional to the volume on every
   publication. The memory provider used above does not. The SQLite provider
   encodes the namespace as one JSON document per publication: a single
   writer's p50 is 0.45 ms at 100 files, 1.45 ms at 2000 and 6.5 ms at 8000
   (`BENCH_METADATA=sqlite`), almost all of it in `serde_json`. This pass still
   helped it (2.63 to 1.51 ms with one writer at 2000 files, 185 to 2.3 ms with
   sixteen, because they now share publications), but writes on a large volume
   stay above 10 ms until that provider stores changed inodes only. `NodeMap`
   makes the change set cheap to find: two versions share every subtree that
   did not change, so a walk can skip them by pointer.
3. **The batch runner yields at least eight times per batch** to let other
   writers' requests gather. That costs a single writer a few microseconds and
   was left alone: it is what coalesces publications against a slow provider,
   and nothing here measures one.
4. **HTTP/2 on one connection** runs at about half the HTTP/1.1 throughput
   because every stream shares h2's connection mutex. The CLI uses HTTP/1.1.
5. **Directory entries are scanned linearly** in `walk`; fine at 64 entries,
   visible in profiles for very large directories.
6. **`read_parallelism` is still 1.** A 4 KiB read touches one block, so it does
   not affect the target above; multi-block reads against a remote object store
   would benefit.
7. **Per-operation statistics** in the object-store provider are shared atomic
   counters written on every block read. They are not locks, but they are the
   next shared cache lines on that provider's hit path.
8. **Per-session and per-handle gates have one stripe.** Many cores working
   one session's path operations, or one shared handle, still pass that
   counter between them. Striping them costs two kilobytes per gate.
9. **A cache hit still writes the block's own hit counter**, so cores reading
   the same block share that line. Different blocks share nothing.
10. **Strict-atime reads were not benchmarked**; the benchmark has no switch
   for the policy. A test covers that they skip the gate and the mutex.
