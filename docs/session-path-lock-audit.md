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

## Removed from the read and open/close paths

| Lock | Where | Replacement |
| --- | --- | --- |
| Operation gate (`ChunkedInner::gate`, async mutex) | every read, stat, readdir, open | Not taken in exclusive mode; reads use the committed snapshot. |
| State mutex (`ChunkedInner::state`) for snapshots | every read, stat, readdir; fence check per session call | `StateCell`: an `ArcSwap<Namespace>` mirror refreshed when a state guard is released, plus a `locked_reads` flag that sends readers back to the mutex while the filesystem is failed, closed, or has uncommitted per-inode overlays. |
| State mutex for open and close | open-reference counts and fd numbers | `FastRefs`, a fixed table of atomic words (inode and count packed together), and an atomic fd counter. An open counts its reference and then checks a sequence that is odd while a mutex holder is deciding inode lifetimes from the references; if it moved, the open settles under the mutex. Close does the same in reverse. While any unlinked inode is retained for an open handle, opens and closes use the mutex. |
| Lease mutex (`ChunkedInner::lease`) | lease validation and `checked_owner_fence` on every session call | `lease_cached_until_ms` and `lease_fence` atomics; the mutex is only taken once the cached deadline has passed. |
| Lifecycle `tokio::sync::RwLock` | shared by every read and write | `DrainGate`. |
| Per-handle gate (`ChunkedHandle::gate`) | every handle call | `DrainGate`; positional reads share it, cursor moves, writes and close drain it. |
| Handle state mutex (`ChunkedHandle::state`) | every handle call | `HandleState` atomics; writers hold the handle gate exclusively. |
| Block cache index (`CachedBlockStore::state`, async mutex) | every cached block read, with an LRU re-link and a `BlockId` clone | `std::sync::RwLock` taken shared on a hit. A hit writes nothing that other blocks share: it records the current epoch in its own entry (once per epoch) and bumps a per-entry hit counter. Eviction re-files entries hit since they were linked. |
| Block cache I/O gate (tokio `RwLock`) | every miss, put and flush | `DrainGate`. |
| Block cache write gate (async mutex) | serialized every put behind every other put | `DrainGate`: puts share it, flush takes it exclusively. Puts to a remote object store now overlap, as they already did without the cache; a flush still covers every put acknowledged before it. |
| Object-store provider cache (`ObjectStoreBlockCache`, mutex) | every block read from the production block provider: cloned the whole block and scanned the LRU queue linearly under the mutex | `RwLock` shared on a hit, shared `Arc<[u8]>` payloads copied after unlocking, epoch recency as above, and a `read_into` that copies only the requested range. |
| Session handle table (`OwnedHandleSession::state`, mutex) | every handle call's lookup | `std::sync::RwLock`, shared for lookups. |
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

## Kept, with reasons

| Lock | Hot path? | Why it stays |
| --- | --- | --- |
| Block cache and provider cache index (`std::sync::RwLock`) | shared on every cached read | Shared acquisition does not exclude other readers. Removing it means a concurrent map; nothing on the read profile points at it. |
| State mutex under `AtimePolicy::Strict` | reads (record atime) and stats (overlay) | Strict atime is a write on every read by definition. The SDK runtime default is `NoAtime`, which skips both. |
| State mutex while an unlinked inode is held open | opens and closes | The lifetime of a retained inode is decided under the mutex; the lock-free path is disabled until the last such handle closes. |
| Operation gate, mutation queue, lease gate, publication | writes only | They order mutations and publications. Readers, opens and closes no longer wait on them. |
| Per-block fill mutex in the block cache | concurrent misses for one block | It is the wait that lets followers share the leader's backing read. Hits never touch it. |
| `VolumeSessions::state` (`std::sync::RwLock`) | one shared read per request to resolve the session | Shared; written only on session create, expiry and close. |
| Session budget semaphore, HTTP `connection_permits` | session creation, connection accept | Not per request. |
| hyper header-read timer, reqwest request timer, hyper-util connection pool | one registration per request on each side | Slow-request and deadline protection, and third-party. They are where the remaining mutex waits in the remote profile come from. |
| `DiskCachedBlockStore` (`mount-rs-host`) I/O gate and index mutex | only with the optional disk cache tier | Every operation there is a blocking disk read behind a four-permit worker semaphore. |
| SQLite providers' connection mutex | metadata and block I/O on SQLite | One connection is one writer by design. |
| `MemoryBlockStore` `RwLock` | test and benchmark provider | Reads share it. |

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
