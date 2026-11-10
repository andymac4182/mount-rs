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

## Fourth pass

Each lock the third pass kept was either replaced or put through an
experiment, and the two items the third pass left open were done. Numbers are
p50 / p99 / p99.9 in ms, 16 clients and 2000 files unless stated; "before" is
`688e8cc3`.

| Was | Now | Measured |
| --- | --- | --- |
| Remote whole-file write: open, write and close over HTTP, each through the handle path and its own publication | One `PUT` carrying the bytes and an `x-mount-write-file` path header, which the owner runs as `write_file` on the batched path. `RemoteFs` uses it when the server's capabilities advertise `write_file` and the file fits one request, and falls back to the three calls otherwise. | Remote write 0.768 / 0.964 / 1.045 (20.8k ops/s) to 0.285 / 0.500 / 0.568 (54.6k ops/s); one client p50 0.157 to 0.059. |
| State mutex taken by every open and close that overlapped any publication. Every publication raised the handle sequence, so 1 to 3% of opens and closes fell back to the mutex and queued there behind each other and the publisher. | A publication raises the sequence only if it removes an inode or takes its last link, which is all an open handle can care about. `NodeMap::differences` finds that out from the changed inodes alone (shared subtrees are skipped by pointer). | Contended acquisitions, 4 s runs with a background writer: direct all operations 614 to 736 of 62k to 68k (4 to 7 µs each) to 0 of 64k; remote all operations 1876 to 1952 of 90k (15 µs each) to 0 of 86k; remote mixed 65 to 87 of 2.8k to 0 of 2.7k. Latency moved inside run-to-run noise, as expected from waits that small. |
| Block cache index, and the object-store provider's cache index of the same shape: `RwLock`, write-locked by every put | A plain mutex plus a lock-free queue of entries to file. A put queues its entry and tries the index once; if another thread is in it, that thread files the entry on its way out. Puts never wait. Misses, deletes and statistics still take the index and file anything queued first. | Before, 7 to 10% of puts waited 1.5 to 2.5 µs. Direct writes 0.124 / 0.206 / 0.244 to 0.123 / 0.200 / 0.229, reads unchanged at 6.7M ops/s. |
| Disk cache index mutex held across every eviction's `unlink` | Eviction under the mutex is the index update alone; the files are removed after it is released. A refill of a resident block no longer unlinks at all (the rename replaces the file). | 4000 fills into a 64-entry cache from 16 tasks: total wait 253 to 303 ms to 12 to 17 ms, mean hold 61 to 65 µs to 6 to 7 µs, worst wait 1.4 to 15.8 ms to 0.06 to 0.19 ms. Fill latency itself is unchanged (2.5 ms p50): it is worker admission and file I/O. |
| SQLite metadata: every publication encoded the whole namespace and rewrote the row holding it. Measured split at 1 MB of namespace: 1.0 ms encoding, 0.5 ms in SQLite; both grow with the volume. | A publication that follows the previous one appends one row with the inodes it changed (`mount_rs_namespace_changes`); the namespace is the stored document plus those rows. See below. | One writer, by volume size: 100 files 0.402 / 0.551 / 0.835 to 0.269 / 0.496 / 1.020; 2000 files 1.392 / 1.662 / 3.260 to 0.279 / 0.439 / 1.089; 8000 files 6.380 / 7.186 / 11.714 to 0.283 / 0.413 / 0.549; 20000 files 17.6 / 22.8 / 31.3 to 0.293 / 0.734 / 8.779. Sixteen writers: 2000 files 1.793 / 4.006 / 5.858 to 0.448 / 1.091 / 3.097; 8000 files 14.3 / 16.8 / 22.0 to 0.517 / 1.184 / 10.67. |
| SQLite blocks: every put its own durable transaction under the connection mutex. With 16 writers 27 to 64% of acquisitions of that mutex waited, 0.4 to 0.7 ms each, on an async worker thread. | Group commit. A put that finds another in progress queues its bytes and waits its turn on a `TurnGate`; the put whose turn it is commits everything queued in one transaction. An uncontended put is unchanged. | SQLite metadata and blocks, 16 writers: 5.326 / 12.638 / 17.864 (2770 ops/s) to 1.874 / 3.457 / 5.221 (8287 ops/s). In the all-operations mix, writes 7.6 / 12.3 / 18.5 to 2.5 / 5.4 / 11.5. |

### SQLite change rows

The change log keeps the existing single-document form as the form at rest:

- The document is rewritten (a "fold") when the rows have grown to its size
  (never below 64 KiB), so a fold's cost is spread over at least as many bytes
  of changes. A change that is more than a quarter of the document is stored
  as a document; small volumes therefore never have rows.
- A row carries the revision it makes current. The metadata row keeps the
  document's revision: updating it too rewrites the row with the document in
  it, which is what the first version of this did and why it still took
  2.6 ms at 8000 files.
- `publish`, `load` and `load_if_changed` understand rows. Every other
  operation on the store folds first and then sees the plain document, so
  delegated mode, bound mode, inode mode, versions and the migrations between
  them are unchanged. Acquiring the writer lease folds, so only the lease
  holder ever adds rows.
- While rows exist, a trigger keeps the versioning schema version above what
  a build without this change accepts, so such a build refuses the database
  instead of reading a document that is behind. A fold lowers it again, and this build folds whenever
  it opens the database; a database in the folded form, which includes every
  small volume, opens in older builds as before. A second trigger aborts any
  update of the document while rows exist, which is what stops an older
  build that already had the database open from publishing over them.
- Rows that do not count up from the document's revision are refused.

What it costs: the publication at which a fold happens pays for the whole
document. At 8000 files (4 MB) that is one publication in 4096, and it
normally takes 7 ms (4 ms to encode, 3 ms in SQLite), 25 ms when the database
file has to grow. That sets the maximum of those runs (25 to 30 ms), not the
p99.9.

In about a quarter of the 4 s runs at 8000 files the maximum is 50 to 380 ms
instead, which the previous build never showed (its maximum was 11 to 17 ms,
with every write at 6 ms). Timing the steps of each slow publication put all
of it in SQLite's commit: of the fold's transaction (7 of 13 slow
publications), in its `UPDATE` (2), or of the first small row insert after a
fold (4). Encoding, the diff and clearing the rows took their usual
few milliseconds every time. So it is the durable flush of an occasional
multi-megabyte write, sometimes landing on the next commit, and it was not
taken further than that. One earlier cluster of second-long row inserts (two
consecutive runs, up to 2.4 s) did not recur in 27 later runs at that size.
Runs at 2000 files, which fold just as often with a 1 MB document, stayed
under 15 ms in 8 of 8.

### Experiments that kept a lock

| Lock | Experiment | Result |
| --- | --- | --- |
| SQLite block connection mutex, for reads | A second, read-only connection for `get`, so a read does not queue behind a commit. | Worse. All-operations mix: read p99 1.03 to 12.1 ms, write p50 2.4 to 5.8 ms. In rollback-journal mode a reader and a committing writer exclude each other in SQLite itself, and its busy handler sleeps in millisecond steps where the mutex hands over in microseconds. Reverted. Only WAL mode would let readers pass a commit, and the journal mode is part of the provider's durability contract. |
| SQLite metadata connection mutex | Timed the wait in `publish`. | 0.0 to 0.1 µs mean over 8000 publications: publications are already serialized by the batch runner, and nothing else uses that connection on the session path. |
| State mutex, for the batch runner and publications | Counted contended acquisitions by call site after the change above. | None in any workload (direct and remote; writes, all operations, mixed with a background writer): 0 of 57k to 98k acquisitions per 4 s run. It is taken and not waited for, so replacing it would change no measurement here. |
| Block cache index, for misses and deletes | They need an answer from the index, so they cannot hand their work to whoever holds it the way a put now does. | Left as a mutex: 460 acquisitions in 3 s of the read workload against 20 million reads. |

### Two things that were looked into and are not from this code

- `rejected_range_closes_the_handle_opened_for_representation_metadata`
  (`transports/mount-rs-http`) timed out in three full test runs and never
  when its package was tested alone. Timing marks in the test put the whole
  5 s between entering the test and `reqwest::Client::new()` returning; no
  request had been sent. The cause is feature unification: the package asks
  reqwest for `rustls-tls` (bundled roots, about 100 µs to build a client),
  but a `--workspace` build also turns on `rustls-tls-native-roots`, and the
  client then loads the macOS keychain trust store. Running the workspace-built
  test binary by itself measured 0.39, 0.59, 1.4, 1.6 and 5.2 s for that one
  call, and the last failed the test the same way. The test built its client
  inside its 5 s deadline; it now builds it before. The same cost is paid
  once per `RemoteFs` (one client per mount, never per operation) in any
  binary that links the native root store, which the CLI does.
- Some 4 s benchmark runs show one stall of 6 to 9 ms that hits every
  operation in flight at once (it appears as the maximum of read, stat,
  readdir and open together). The build from the previous commit shows it at
  the same rate, with and without a writer, and it was absent earlier on the
  same binaries: it comes from the machine (Docker and another agent were
  running), not from a lock.

## Fifth pass

The fourth pass left four locks in place on the strength of counts and
reasoning. This pass replaced each of them and measured the result, removed
the fold from the SQLite write path, and explained the two anomalies. Numbers
are p50 / p99 / p99.9 in ms; "before" is `d1654401`.

There is now no mutex or reader-writer lock on the session path in these
crates. What took their place is one of three things, all in
`mount-rs-core`: a `TurnGate` (asynchronous turn, lock-free queue of
waiters) where callers genuinely have to wait for each other; a `TurnCell`
(one compare-and-swap, spin then yield) where they are already serialized
and a mutex was taken without ever being waited for; and a lock-free queue
of work that whoever has the turn carries out, where the caller needs no
answer.

| Was | Now | Measured |
| --- | --- | --- |
| Block cache index mutex, taken by every miss (three times), delete and statistics call | No lock. Every cached block is published in the lock-free table, so a lookup that finds nothing there is a miss without consulting the index (a full probe window now evicts its least recently used block instead of leaving the new one unpublished). Counters are atomics. A completed miss queues its entry as a put does. One thread at a time has the index's turn, taken with a compare-and-swap and never waited for; it files at most 4 queued entries per turn. Delete and clear hold the I/O gate exclusively, which keeps out every other thread that takes the turn. | 8 threads reading blocks that are almost never cached (`profile_concurrent_misses`): 121k to 2.3M reads/s; 0.052 / 0.247 / 0.346 to 0.0026 / 0.023 / 0.030. Hits unchanged (remote read 0.187 / 0.312 / 0.405 to 0.187 / 0.310 / 0.376). |
| In-flight fill table: `ArcSwap` slots, one allocation and two `ArcSwap` stores per miss | A fixed table of counted slots matched by hash; joining or leaving a fill is one atomic update. | Part of the row above: `arc_swap`'s store (`Debt::pay_all`, which visits every thread's debt list) was 64% of the miss path's CPU, four stores per miss; this removes two of them. |
| Disk cache index mutex | No lock. The second half of a fill (indexing the written file) is pushed on a lock-free stack and applied by whoever has the turn; admitting a fill takes the turn itself. | 4000 fills into a 64-entry cache from 16 tasks: total wait 13 to 18 ms to 5 to 7 ms, put p99 3.4 to 4.6 ms to 3.0 to 3.2 ms. Worst single wait rose from 0.07 to 0.08 ms to 0.10 to 0.39 ms, since a waiter now yields. |
| State mutex (`StateCell`) | `TurnCell`. | No change, as the count of contended acquisitions (zero) predicted: remote write 0.275 / 0.496 / 0.575 to 0.275 / 0.496 / 0.567, direct write 0.120 / 0.199 / 0.247 to 0.118 / 0.211 / 0.281, and open and mixed with a background writer within run-to-run spread. |
| SQLite connection mutex, blocks: a read arriving during a commit blocked its worker thread on the mutex until the commit finished. With four server threads, reads waiting there stopped metadata requests that needed no block at all. | Reads take the same asynchronous turn as puts, so they wait without holding a thread. A read leaves its request in a queue, and whoever has the turn answers every queued read before it starts a commit and after it finishes one; a read is woken as soon as it is answered (`TurnGate::lock_unless`). The connection itself is a `TurnCell`: nothing waits for it. | SQLite blocks, no RAM cache, all five operations, 16 clients: stat 0.33 / 0.89 / 1.2 to 0.11 / 0.25 / 0.33; readdir 0.41 / 0.95 / 1.3 to 0.15 / 0.27 / 0.35; open 0.72 / 1.31 / 1.7 to 0.21 / 0.36 / 0.48; write 1.39 / 2.3 / 5.0 to 1.42 / 2.0 / 2.7; read 0.60 / 1.50 / 2.3 to 0.71 / 1.09 / 1.5; 4490 to 6005 ops/s per operation. Read p50 is higher because 33% more commits are in progress for a read to wait out. |
| SQLite connection mutex, metadata | `TurnCell`. | Wait was already 0.0 to 0.1 µs mean. |
| SQLite change rows folded into the document every 4096 publications | A per-inode overlay that is never folded while files are rewritten in place. See below. | One writer, 8000 files: maximum 26 to 32 ms in every run (and 1.9 s in one of eight) to 1.8 to 9 ms; 0.281 / 0.436 / 1.0 to 0.302 / 0.440 / 0.87. 20000 files: maximum 56 to 107 ms to 5 to 9 ms; p99.9 1.2 to 1.7 to 0.75 to 0.83. 16 writers, 2000 files: 0.46 / 1.14 / 2.9 at 27.5k ops/s to 0.68 / 1.55 / 2.8 at 19.7k ops/s (the cost; see below). |
| Object-store provider's cache index mutex, `MemoryMetadataStore`'s `RwLock` | `TurnCell`. | Not waited for before or after. |
| `RemoteFs` built its HTTP client with the default root store for every server | For a plain-HTTP server (loopback only, and redirects are off) the root store is not loaded. | Client construction in a release build that links the system root store: 131 to 163 ms to 8 µs. An HTTPS mount still pays it once. |

Default configuration (memory metadata and blocks), remote, all five
operations, 16 clients, for reference: read 0.185 / 0.319 / 0.394 to
0.181 / 0.308 / 0.368; stat 0.190 / 0.328 / 0.414 to 0.187 / 0.317 / 0.370;
readdir 0.218 / 0.365 / 0.448 to 0.211 / 0.347 / 0.406; open
0.371 / 0.555 / 0.666 to 0.365 / 0.532 / 0.604; write 0.225 / 0.399 / 0.504
to 0.220 / 0.382 / 0.458.

### SQLite overlay

The fourth pass appended one row per publication and rewrote the document
when the rows had grown to its size. That rewrite ran on a publication, cost
26 ms at 8000 files and 56 to 107 ms at 20000, and grows with the volume.
The overlay replaces the rows:

- `mount_rs_namespace_nodes` holds one row per changed inode (empty for a
  removed one), and a publication upserts the inodes it changed. A row
  replaces the inode's earlier row, so the overlay has at most as many rows
  as inodes were touched and never needs folding for a volume whose files
  are rewritten in place.
- `mount_rs_namespace_changes` holds a single header row: the namespace's
  scalar fields and the revision the overlay makes current. Its presence is
  what the fences of the fourth pass key on, unchanged: the schema version
  trigger that makes older builds refuse the database, and the trigger that
  aborts an update of the document underneath the overlay.
- A publication is one transaction: the fenced conditional insert of the
  header decides it, exactly as the conditional update does for a document.
- Only removed inodes accumulate. The overlay is folded into the document
  when it holds more than twice the namespace's inodes (and more than 4096
  rows). The publication keeps an upper bound on the row count and counts
  the rows only when the bound says there is no room.
- Everything else is as before: `publish`, `load` and `load_if_changed`
  understand the overlay, every other operation folds first, acquiring the
  writer lease folds, and a change more than a quarter of the document's
  size is stored as a document, so small volumes stay in the plain form.
- An overlay that is not exactly one header, for a revision after the
  document's, over a stored document is refused.

What it costs: a publication now writes one b-tree leaf per changed inode,
where an appended row was one page whatever it held. A single writer changes
one inode per publication and pays 20 µs (p50 0.281 to 0.302). Sixteen
writers batch sixteen inodes into a publication, which touches about fifteen
leaves: `pwrite` time per commit roughly tripled in the profile, and
throughput fell from 27.5k to 19.7k writes/s with p50 0.46 to 0.68. The
append-and-fold form bought that throughput by deferring the page writes
into one sequential rewrite, which is the stall this removes; batching the
upserts instead only moves the same leaf writes into a periodic stall whose
size grows with the volume. The tail is the target here, so the steady cost
was taken.

### The two anomalies, explained

- The HTTP integration test that timed out in full runs: see the fourth
  pass. It was `reqwest::Client::new()` loading the system root store, which
  a `--workspace` build links through feature unification.
- The 6 to 10 ms stall that some 4 s runs show on every operation at once is
  the machine. A separate single-threaded busy loop, with no benchmark
  running, was descheduled for 5.6 ms in one of six 4 s runs (0.04 to
  0.26 ms in the others). Both builds show it at the same rate.

### What an attempt showed should stay

| Tried | Result |
| --- | --- |
| A second, read-only SQLite connection for block reads (fourth pass) | Worse: read p99 1.03 to 12.1 ms. Reverted; the turn-served reads above are what replaced it. |
| Bounding how many queued entries one thread files into the block cache index per turn: 32, 8, 4 | 32: p99 0.007, p99.9 0.155 ms. 8: 0.038 / 0.046. 4: 0.023 / 0.030, and the highest throughput. 4 was kept; unbounded let one reader file for the other seven indefinitely. |
| `TurnCell` for the SQLite connection without routing reads through the turn first | Not built: a reader would spin for the length of a commit (over a millisecond). The cell is only used where the callers are already serialized. |

## Sixth pass

The fifth pass left three things: mutexes it called off the path without
trying to remove them, the 16-writer cost of the SQLite overlay, and the
rest of the workspace. Numbers are p50 / p99 / p99.9 in ms.

### Mutexes the fifth pass kept by argument

| Was | Now | Result |
| --- | --- | --- |
| `HttpServer::close_outcome` (`Mutex<Option<CloseOutcome>>`) | `OnceLock`: the first close records the outcome, later ones read it. | Tests pass; no lock. |
| `HttpServer::task` (`tokio::sync::Mutex`) | `TurnLock`, an asynchronous mutex over `TurnGate` added to `mount-rs-core` (lock-free waiter queue, `lock`, `try_lock`, `lock_owned`, `get_mut`). | Tests pass. |
| `HandleState` in `mount-rs-host` (`Arc<Mutex<_>>`, locked on a blocking thread) | `Arc<TurnLock<_>>`, taken with `lock_owned` before the blocking call, so a second caller on one handle waits without holding a blocking thread. | Host tests pass. |
| `SqliteStore`'s `Mutex<Connection>` | `TurnLock<Connection>`. | Tests pass. |
| SQLite I/O diagnostics (`Mutex<BTreeMap>`, registry `Mutex<Vec>`) | `TurnCell`. | Tests pass. |

### SQLite rows, third form: a log that is cleaned and never folded

Three forms were built and measured on the direct surface (SQLite metadata,
memory blocks, whole-file writes):

| Form | 1 writer, 8000 files | 1 writer, 20000 files | 16 writers, 2000 files |
| --- | --- | --- | --- |
| Appended rows, folded into the document when they reach its size (fourth pass) | 0.281 / 0.422 / 0.73, max 26 | 0.294 / 0.452 / 1.29, max 55 | 0.47 / 1.36 / 3.2, max 11, 26.8k/s |
| Per-inode overlay (fifth pass) | 0.305 / 0.474 / 1.09, max 3 | 0.328 / 0.498 / 1.19, max 8 | 0.66 / 1.53 / 2.3, max 9, 19.6k/s |
| Cleaned log (this pass) | 0.301 / 0.533 / 1.38, max 8 | 0.293 / 0.528 / 0.82, max 3 | 0.51 / 1.20 / 2.1, max 5, 26.0k/s |

The overlay wrote a b-tree leaf per changed inode, which is what cost the
16-writer case a quarter of its throughput. The cleaned log goes back to one
appended row per publication (`mount_rs_namespace_changes`; the
`mount_rs_namespace_nodes` table is gone) and removes the fold a different
way, the way a log-structured store cleans segments (`changes.rs`):

- An entry is garbage once a newer row sets the same inode. A row that is
  all garbage is dead; dead rows are deleted sixteen at a time, in the
  transaction of the publication that finds sixteen.
- A row that is at least half garbage (by bytes) is cleaned: the next
  publication stores its remaining inodes again, which kills the row. A
  publication may clean as many bytes as it and its predecessors changed, so
  a publication writes at most about twice what it changed and the rows hold
  at most about twice the inodes they cover.
- A large inode that never changes is therefore written again only after as
  much has been written around it. An earlier version of this pass carried a
  fixed number of entries forward from the oldest row instead, and rewrote
  the benchmark's 650 KB directory node every 65 publications (p99 0.75 to
  2.0 ms for one writer); that version was dropped.
- A removed inode keeps an empty entry so it stays removed. For an inode the
  document never had, the entry is forgotten as soon as no other stored row
  mentions the inode, so creating and removing files leaves nothing behind
  (6000 create-and-remove publications: the row count does not grow).
- The fences are those of the fourth pass. In addition the conditional
  insert requires the document to be the one the rows were counted against;
  if another connection folded them in the meantime the publication stores a
  whole document instead.
- The document is rewritten only if the rows exceed eight times its size
  plus 1 MiB, which cleaning keeps them from reaching; and by anything that
  reads the namespace column, as before.

Against the original (`d1654401`): one writer has the same median, p99
0.45 to 0.53 (a delete of sixteen rows rides on one publication in
sixteen), and the maximum falls from 26 to 55 ms, growing with the volume,
to 3 to 8 ms. Sixteen writers: 27.5k to 26.0k writes/s (noise is about
3%), p99 1.23 to 1.20, p99.9 2.9 to 2.1. Sweeping the cleaning threshold
(half, three quarters, never) and the delete batch (4, 16, 64) moved the
16-writer figure by no more than the noise, so the remaining 5% is the
deletes and encoding entries one at a time, not the cleaning.

### The rest of the workspace

The earlier passes covered the session path. This one went through every
other crate in the workspace (not the vendored `quinn-proto` and
`tunnel-transport`, which are third-party code) and replaced each `Mutex`
and `RwLock` outside test code that could be replaced: about 135, with
`TurnCell` (short sections that never block), `TurnLock` (asynchronous
mutexes), an atomic, or `OnceLock`.

| Crates | Replaced | Notes |
| --- | --- | --- |
| `mount-rs-napi`, `mount-rs-sqlite-vfs` | 52 | The VFS lifecycle state taken twice per VFS callback is one atomic word. The NFS clock is a `OnceLock` and an atomic maximum. |
| `mount-rs-nfs`, `mount-rs-9p` | 35 | A 9P pending reply was a tokio mutex around a value only tested for presence; it is an atomic flag, which also removes a copy of every reply. |
| `mount-rs-s3`, `mount-rs-webdav`, `mount-rs-fuse`, `mount-rs-auto`, `mount-rs-fskit` | 22 | The loaded-transports set is an atomic bit mask. |
| `mount-rs-kv`, `mount-rs-memfs`, `mount-rs-persist`, `mount-rs-versioned`, `mount-rs-blob-cache`, `mount-rs-fault-injection` | 20 | `mount-rs-persist`'s hand-written save gate (a mutex, a waiter queue, a future and a guard, about 120 lines) is a `TurnLock`. |
| `mount-rs-pglite`, `mount-rs-slatedb`, `mount-rs-cli`, `mount-rs-service` | 11 | The service catalog's connection pool was a `Vec<Mutex<Connection>>` locked on a blocking thread, so a request that drew a busy connection held a blocking thread while it waited. Each connection is now a `TurnLock` taken before the request moves to a blocking thread. The catalog's snapshot cache was held across the row read and the decode of a document of up to 8 MiB; it is now held only to compare and to store. |

`TurnCell` changed to make this possible. It abandoned itself whenever its
guard was released on a panicking thread, so a `Drop` that ran during an
unwind and took a cell killed the cell. It now does what a mutex does: only
a panic that starts while the cell is held abandons it. `lock_recovered`
gives the value regardless, for state that is valid whatever its holder was
doing, and `get_mut` gives it through exclusive access.

The blob cache's disk reads were the one place this found a lock that cost
latency. `LocalCache::disk_io` put every disk operation of the cache in one
line, reads included. A read now takes no disk lock: a file is published by
rename and checked against its recorded size and digest, and the index
generation is compared again after the read, so a read that raced a
replacement or an eviction is discarded. Only the removal of a file found to
be bad waits with the writers. Eight threads reading 256 blocks of 4 KiB
that are too large for the RAM tier (`profile_disk_reads`): 32.5k to 116k
reads/s; p50 0.030 to 0.067 ms (the reads now overlap in the filesystem),
p99 5.3 to 0.11 ms, p99.9 16 to 22 ms to 0.13 to 0.20 ms.

A first sweep kept eight locks on the argument that they could not be
replaced. Each was then tried:

| Lock | Replaced with | Result |
| --- | --- | --- |
| `path_lock` in the NFS v3/v4 and 9P sessions, `CachedBlockStore::write_gate` in the blob cache (`tokio::sync::RwLock<()>`) | `DrainGate`, the shared/exclusive gate the session path already uses: readers enter with one atomic update and overlap as before. | Tests pass. |
| `HistoricalFileHandle::state` in `mount-rs-versioned` (`tokio::sync::RwLock`) | A `DrainGate` for admission and two atomics for the cursor and the closed flag. | Tests pass. |
| `MemoryFs::inner`, the whole memfs tree (`Mutex`) | `TurnCell`. It was kept at first because some sections copy data. | 8 threads, open/read/close and stat on 64 files (`profile_contended_reads_and_stats`): 293k to 680k ops/s; p50 10.5 to 4.1 µs, p99 166 to 84 µs, p99.9 265 to 158 µs. The same with one write in eight. |
| `StorageVolume::state` in `mount-rs-sqlite-vfs` (`Mutex`, held across provider round trips) | `TurnLock`, waited for on the bridge's executor like any provider future, so a waiting VFS callback parks instead of spinning. | Tests pass. |
| `LocalCache::disk_io` in the blob cache (`Mutex<()>`, writers) | `TurnLock` taken with the new `lock_blocking`, which parks the calling thread on the gate's lock-free waiter queue. | Tests pass; disk reads unchanged at 118k/s. |
| `WebdavSession::locks` (`Arc<Mutex<DavLockTable>>`, a public field) | `Arc<TurnCell<DavLockTable>>`. The field's type changes; the one user outside the crate is its own test. | Tests pass, including the one that panics while holding the table. |

The entries that first stayed here were then tried as well:

| Lock | What replaced it | How it was checked |
| --- | --- | --- |
| `CLIENT_NETWORK` `Mutex` + `Condvar` in `mount-rs-foundationdb` | Two `TurnLock`s taken with `lock_blocking`. One holds the state. The caller stopping the network holds the other from `Stopping` to `Stopped`, and other terminal callers queue on it instead of waiting on a condition variable. The state is set to `Failed` before the native boot, so a panic there still fails closed without lock poisoning. | Type-checked and linted with `--features foundationdb --lib`. Not run: there is no native client or cluster on this machine. |
| `vendor/tunnel-transport`: pool writer, log limiter keys, fairness counters, peer identity slot, driver, pin watcher, dial gates, draining set, connection table | `TurnLock` for the locks held across an await, `TurnCell` for the rest, and `ArcSwap` for the identity slot, whose readers no longer take anything. The crate now depends on `mount-rs-core`. | Its own test suite passes. |
| `vendor/quinn-proto`: `TokenMemoryCache`, `BloomTokenLog`, the qlog stream handle | A compare-and-swap cell local to the vendored copy (`src/turn.rs`), so the manifest and every lockfile are unchanged. Recorded as a second change in `UPSTREAM_PATCH.md`. | The crate's own tests on a copy outside the workspace, with `qlog` and `bloom` on: 303 unit and 3 doc tests pass. |

No `Mutex`, `RwLock` or `Condvar` is left in the workspace or under
`vendor/` outside tests, examples (which include the mutex baseline the
benchmarks compare against) and the SQLite benchmark's statement counter.

Checked beyond the workspace gates: the Redis hint and lease test against a
local Redis server, and the Node binding's test files run one at a time
(50 of 54 pass; two need `MOUNTX_SOURCE`, two shell out to a `pnpm` shim
that is broken on this machine). Not exercised here: PGlite, Redis over
TLS, FoundationDB, kernel NFS/9P/FUSE mounts, and the Linux-only FUSE code,
which is only type-checked.

## Kept, with what was measured

No mutex or reader-writer lock remains on the session path in these crates
(`mount-rs-core`, `mount-rs-chunked`, `mount-rs-cluster`, `mount-rs-remote`,
`mount-rs-http`'s request path, and the SQLite, object-store and memory
providers). What remains is exclusion that is still needed, in a form that
is not a lock, and locks that are off the path:

| What | Where | Why it stays |
| --- | --- | --- |
| `TurnCell` (compare-and-swap, never waited for in any measured workload) | runtime state; SQLite connections; block cache, disk cache and object-store cache indexes; `MemoryMetadataStore` | Each guards a multi-field update that must be one step. Their callers are serialized before they arrive (batch runner, `TurnGate`, exclusive I/O gate), or carry out each other's queued work instead of waiting. |
| `TurnGate` on the SQLite block connection | puts, reads, deletes | A SQLite connection runs one statement at a time and a commit is a durable flush. Waiters hold no thread, puts share a commit, and reads are answered by the holder. |
| Session budget semaphore, HTTP `connection_permits`, disk cache worker and fill semaphores | session creation, connection accept, disk cache fills | Admission limits; not per request on the session path. |
| hyper header-read timer, reqwest request timer, hyper-util connection pool, tokio's timer wheel | one registration per request on each side | Inside third-party crates. About 1% of active samples in the remote write profile. |

## Still open

1. **A publication of the SQLite metadata store deletes superseded rows**
   (sixth pass), sixteen at a time. That is the one part of its cost that is
   not an append, and it shows as p99 0.45 to 0.53 ms for a single writer.
2. **The blob cache still serializes its disk writes** (admissions,
   evictions and invalidations take turns on `LocalCache::disk_io`). Reads
   were taken off it in the sixth pass.
3. **A directory is one node**, so creating or removing a file rewrites its
   parent's whole entry list in the change row (and in memory). At 64 entries
   that is about 4 KB per create; it is what the populate phase of the SQLite
   benchmark spends its time on.
4. **The batch runner yields at least eight times per batch** to let other
   writers' requests gather. That costs a single writer a few microseconds and
   was left alone: it is what coalesces publications against a slow provider,
   and nothing here measures one.
5. **HTTP/2 on one connection** runs at about half the HTTP/1.1 throughput
   because every stream shares h2's connection mutex. The CLI uses HTTP/1.1.
6. **Directory entries are scanned linearly** in `walk`; fine at 64 entries,
   visible in profiles for very large directories.
7. **`read_parallelism` is still 1.** A 4 KiB read touches one block, so it does
   not affect the target above; multi-block reads against a remote object store
   would benefit.
8. **Per-operation statistics** in the object-store provider are shared atomic
   counters written on every block read. They are not locks, but they are the
   next shared cache lines on that provider's hit path.
9. **Per-session and per-handle gates have one stripe.** Many cores working
   one session's path operations, or one shared handle, still pass that
   counter between them. Striping them costs two kilobytes per gate.
10. **A cache hit still writes the block's own hit counter**, so cores reading
   the same block share that line. Different blocks share nothing.
11. **Strict-atime reads were not benchmarked**; the benchmark has no switch
   for the policy. A test covers that they skip the gate and the mutex.
