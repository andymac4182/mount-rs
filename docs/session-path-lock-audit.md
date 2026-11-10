# Session-path lock audit

Every lock a read or metadata call crosses on the remote session path
(`RemoteFs` → HTTP → `VolumeSessions` → `OwnedHandleSession` → `ChunkedFs` →
block cache → block store), what was done about it, and what is left.

Scope: an exclusive, non-delegated owner (`OwnershipMode::Exclusive`, no
directory delegation), which is what the session server runs. Concurrent-writer
and delegated mounts still take the operation gate for reads.

Measure with `crates/mount-rs-remote/examples/session_latency.rs`
(`BENCH_SURFACE=direct` for the owner alone, `remote` for the HTTP/1.1 path).
Numbers below are release builds on a 10-core Apple Silicon laptop, 2000
files, 4 KiB reads, 16 closed-loop clients unless stated, p50/p99/p99.9 in ms.

## Result

| Path | Before this audit | After |
| --- | --- | --- |
| Direct read | 0.018 / 0.038 / 0.055, 855k ops/s | 0.001 / 0.002 / 0.003, 4.4M ops/s |
| Direct stat | 0.004 / 0.007 / 0.018 | 0.004 / 0.007 / 0.008, 1.0M ops/s |
| Direct readdir (64 entries) | 0.006 / 0.007 / 0.024 | 0.006 / 0.010 / 0.012 |
| Direct open+close | 0.002 / 0.030 / 2.2 | 0.002 / 0.006 / 1.8 |
| Remote read | 0.20 / 0.37 / 0.53 | 0.20 / 0.36 / 0.49 |
| Remote stat | 0.20 / 0.37 / 0.50 | 0.20 / 0.36 / 0.43 |
| Remote read, 6000 files, concurrent writer | 0.21 / ~0.8 / ~1.6 | 0.21 / 0.39–0.48 / 0.5–1.6 |

The owner is no longer what the remote path waits on: one remote client sees
0.056 / 0.118 / 0.154 ms, and the server gives 70k ops/s on two threads and no
more on eight. What remains at 16 clients is loopback syscalls and task
wake-ups in hyper and reqwest.

## Removed from the read path

| Lock | Where | Replacement |
| --- | --- | --- |
| Operation gate (`ChunkedInner::gate`, async mutex) | every read, stat, readdir, open | Not taken in exclusive mode; reads use the committed snapshot. |
| State mutex (`ChunkedInner::state`) | snapshot fetch on every read/stat/readdir; fence check per session call | `StateCell`: an `ArcSwap<Namespace>` mirror refreshed when a state guard is released, plus a `locked_reads` flag that sends readers back to the mutex while the filesystem is failed, closed, or has uncommitted per-inode overlays. |
| Lease mutex (`ChunkedInner::lease`) | lease validation and `checked_owner_fence` on every session call | `lease_cached_until_ms` and `lease_fence` atomics; the mutex is only taken once the cached deadline has passed. |
| Lifecycle `tokio::sync::RwLock` | shared by every read and write | `DrainGate`: one atomic increment/decrement while no transition is running. A tokio `RwLock` locks its waiter mutex on every read-guard release; with all cores releasing the same lock this was the largest contention point left (direct reads went from 2.0M to 4.4M ops/s, p99.9 from 1.0 ms to 0.003 ms). |
| Per-handle gate (`ChunkedHandle::gate`, was a mutex, then a tokio `RwLock`) | every handle read | `DrainGate`; positional reads share it, cursor moves, writes and close drain it. |
| Handle state mutex (`ChunkedHandle::state`) | every handle call | `HandleState` atomics (`position`, `closed`); writers of either hold the handle gate exclusively. |
| Block cache state (`CachedBlockStore::state`, async mutex) | every cached block read, with an LRU re-link and a `BlockId` clone | `std::sync::RwLock` taken shared on a hit. A hit stores its use sequence in the entry; eviction re-links stale entries before choosing a victim, so the order is still exact LRU. Hit/miss counters are atomics. |
| Handle table (`OwnedHandleSession::state`, mutex) | every handle call's lookup | `std::sync::RwLock`, shared for lookups. |

Related changes that are not locks but sat on the same path: `BlockStore::read_into`
(no full-block copy per read), binary search over extents, namespace clones and
drops moved off the async workers for large namespaces, and the lease gate held
across publication so a heartbeat cannot replace the fencing token mid-publish.

## Kept, with reasons

| Lock | Hot path? | Why it stays |
| --- | --- | --- |
| State mutex for open/close (`open_refs`, fd numbers) | open and close, a few dozen ns each | Publication reads the open-reference map under this mutex to decide which unlinked inodes survive. It saturates near 1.2–2M open+close/s on one owner; the HTTP path delivers about 40k/s. See "Open references" below for the lock-free design if that ever matters. |
| State mutex under `AtimePolicy::Strict` | reads (record atime) and stats (overlay) | Strict atime is a write on every read by definition. The SDK runtime default is `NoAtime`, which skips both. |
| Operation gate, mutation queue, lease gate | writes only | They order mutations and publications. Readers no longer wait on them. |
| Cache `io_gate` (tokio `RwLock`) and per-block fill mutex | cache misses, put, flush, delete | Misses are bounded by backing I/O, and the gate is what stops a deleted block being repopulated. Hits take neither. |
| `VolumeSessions::state` (`std::sync::RwLock`) | one shared read per request to resolve the session | Shared; written only on session create, expiry and close. |
| Per-handle entry (`tokio::sync::RwLock`) and `path_operations` in `OwnedHandleSession` | every session call | Scoped to one handle or one client session, so only that client's own concurrent calls meet there. They do have tokio's release mutex; in a 3 s profile of the remote path it was 3 samples out of roughly 7000. Moving `DrainGate` into a shared crate would remove it. |
| `RemoteHandle::state` (tokio `RwLock`, client side) | every remote handle call | Same shape and cost as the per-handle entry above. |
| Session budget semaphore, HTTP `connection_permits` | session creation, connection accept | Not per request. |
| hyper header-read timer, reqwest request timer | one timer registration per request on each side | Slow-request and deadline protection. They account for most of the remaining mutex waits in the remote profile (tokio's timer wheel) along with hyper-util's connection pool. |
| `MemoryBlockStore` `RwLock` | test and benchmark provider | Reads share it. |

## Still open

1. **Mutations are O(namespace).** Each mutation deep-clones the namespace and
   publishes all of it. Readers no longer block on that, but writes at 6000+
   files cost milliseconds and are the source of the residual read p99.9 when a
   writer is active. This is the largest remaining cost and needs a persistent
   map for `Namespace::nodes` plus incremental publication.
2. **Open references.** To take open/close off the state mutex: keep the
   reference counts in a concurrent map, and bracket publication's read of that
   map with a sequence counter (odd while publishing). An open increments its
   count, then re-reads the counter; if it moved, it falls back to the locked
   path, which must release through the same reaping path as close.
3. **HTTP/2 on one connection** runs at about half the HTTP/1.1 throughput
   because every stream shares h2's connection mutex. The CLI uses HTTP/1.1.
4. **Directory entries are scanned linearly** in `walk`; fine at 64 entries,
   visible in profiles for very large directories.
5. **`read_parallelism` is still 1.** A 4 KiB read touches one block, so it does
   not affect the target above; multi-block reads against a remote object store
   would benefit.
