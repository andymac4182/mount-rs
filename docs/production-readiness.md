# Production readiness

Status of the work to make mount-rs safe to run in production: what was
fixed, what was checked and left alone, how each was verified, and what risk
remains. Scope was limited to production risks: data loss or corruption,
hangs, leaks, errors that make a caller retry forever or give up wrongly, and
anything that stops CI being a trustworthy gate.

## CI as a gate

Four jobs failed on every run of `main`, so no pull request could be judged
by its checks. #50 and #57 address them.

**These fixes are not yet proven.** #49 to #57 were merged on 2026-11-11
without waiting for their checks, at the owner's instruction. Before the
merge, #50's own runs had passed every job they reached except the ones
noted under "Flaky jobs" below and the Intel macOS job. Whether `main` is
green is decided by its CI run after these merges, not by this document.

| Job | Cause | Resolution |
|-----|-------|------------|
| `rust (windows-latest)` | Dead code in the cluster HTTP/3 test off Unix | Unix-only items gated |
| `Observability qualification (windows-latest)` | A delegated SQLite hand-off test needs concurrent local backing, which is Unix-only; then the disk cache report test, because the disk cache refuses to open off Unix | Quarantined on non-Unix, with the reason at each test |
| `node (macos-latest)`, `node (macos-15-intel)` | The provider matrix compiled the CLI inside a 120 s test case. On Intel macOS the enclosing step's 10-minute limit then expired during that build, and the NFS conformance suite's 3-minute fixture build also ran over | The CLI is built once before the timed cases; the step limit is 25 minutes; the fixture build limit is 20 minutes (#57) |
| `w26-ozone-evidence` | The TiDB job wrote its log into the checkout, and the evidence step refuses a dirty checkout | Log and artifact moved to the runner's temp directory |

The quarantines hide no behaviour that is supported on those platforms: the
delegated SQLite hand-off and the disk cache both return `ENOTSUP` off Unix
by design.

## Fixed

| Item | Failure mode | Change | Verification |
|------|--------------|--------|--------------|
| FUSE unmount hang (#49) | A helper process forked for unmount inherited the `/dev/fuse` descriptors. `exec` closes a file on the mount, which sends a flush the stopped session never answers, and the connection cannot abort while the child holds a descriptor. Unmount hung, as did the `native-fuse` CI job | Device descriptors are registered and closed in the child before `exec`; unmount and stop are explicit lifecycles | New unit tests; 150 of 150 native FUSE runs on 4 CPUs in a Linux container (it reproduced within 20 before) |
| 9P teardown (#49) | Teardown could run twice or be skipped when unmount and drop raced | One serialised, idempotent teardown | Unit tests; Linux container tests |
| Chunked owner lease (#49) | A fence could be read with another lease's deadline; a failed check-in left the scope draining | Fence and deadline published together; fallible steps run before the grant is dropped | New unit test |
| NFSv4 client restart (#49) | A restarted client's old state was dropped before the new instance was confirmed, against RFC 8881 §18.35 | The old instance is retired when `CREATE_SESSION` confirms the new one | New wire test |
| Node P9 and S3 server wrappers (#49) | `listen` and `close` could race and leak a server | Hand-over happens under the state lock; a failed close restores the server | Workspace tests only, no race test |
| Host disk cache fault (#51) | A fault in the cache's own index or data file failed `put` after the backing store had the block, and failed `get` instead of reading the backing store | A cache fault is a miss; `delete` still fails while the index is broken | New test that fails without the change |
| SQLite VFS lock level (#52) | After a failed upgrade from SHARED to EXCLUSIVE the file kept the reserved lock and reader gate while the handle recorded SHARED, so SQLite's unlock was dropped and other connections saw `SQLITE_BUSY` | The unlock after a failed upgrade reaches the file | New test that fails without the change |
| PGlite writer lease refusal (#53) | A concurrent-mode volume refused the legacy writer lease with `EAGAIN`, the answer for "wait for the holder", for a lease that can never be granted | Returns `EBUSY`, as SQLite does | Existing ignored test tightened to assert the code; run against the PGlite test server |
| NFS handle table (#54) | Every operation scanned a list of every handle to mark one as used, so operations slowed in proportion to the files ever seen | Recency kept in an ordered map | New test: 100,000 handles decoded once each took 362 s before and under 1 s after (debug build) |
| NFS and WebDAV accept loops (#55) | One failed `accept()` ended the listener; the mount stayed attached but the client's next reconnect hung | The failure is reported and the loop keeps accepting, pausing 10 ms to 1 s between failures | New tests drive the retry with injected `EMFILE`; real descriptor exhaustion was not exercised |
| PGlite and TiDB liveness (#56) | A connect that never completed, or a peer that vanished without closing, hung a statement and everything queued behind it | PGlite: 30 s connect timeout, 60 s keepalive idle. TiDB: 60 s TCP keepalive. Values in the connection string win | New unit tests for the defaults; a PGlite test connects through the new path |
| NFSv4 per-request session copy | Every `SEQUENCE` cloned the whole session state under the lock | Only the three fields the request needs are copied | Workspace tests; not benchmarked, the change only removes a clone |

Every Rust change was preceded by
`./scripts/cargo-shared test --workspace --all-targets --locked` and clippy
with `-D warnings` on macOS. #52 and #53 shared one such run, as did #54,
#55 and #56, with the changes together in the tree. The same run was
repeated on `main` after all of them were merged.

## Checked and left alone

| Item | Why no change |
|------|---------------|
| Blob cache dirty flag stuck after a failed put | The flag makes reads bypass the cache until a flush succeeds. Staying set after a failure costs speed, not correctness |
| Commit outcome: "possibly committed" against "definitely failed" | The chunked filesystem replays an operation only on `EAGAIN` and fails closed on anything else. Every provider was read for a path that returns `EAGAIN` after a commit that may have applied; none was found. SQLite, PGlite, FoundationDB, SlateDB and the memory provider derive `EAGAIN` only from explicit pre-commit comparisons or a known non-commit; TiDB routes every explicit commit through an "outcome unknown" error |
| Node delegated mount `creation_uncertain` | Deliberate: when native mount creation was cancelled or failed part-way, ownership is not released because a mount may exist. Two tests pin this |
| Node `Filesystem` admitting operations during shutdown | Operations admitted while shutdown runs reach the chunked filesystem, whose shutdown takes the operation gate and sets `closed`. They complete before it or get `EBADF` after it |
| Peer connection handed back while draining | Only reachable when a peer sends a planned HTTP/3 GOAWAY. mount-rs servers use the immediate-close `serve` and never send one. The code is vendored, and the obvious change would shut down the draining connection's in-flight streams |

## Scoped down

- The owner lease is still validated three times on a forwarded read; only
  the torn read of fence and deadline was fixed.
- The Node server wrappers were fixed in place, without a shared lifecycle
  type and without a test that races `listen` against `close`.
- NFSv4 keeps its parallel slot vectors.
- Writer-lease refusal codes still differ between providers for a
  concurrent-mode volume: SQLite and PGlite `EBUSY`, TiDB `EOVERFLOW`,
  FoundationDB `ENOTSUP`. All are permanent errors.

## Remaining risk

- **FUSE helper fix covers this crate's own children only.** A child process
  spawned by other code in the same process (an embedding application, a
  Node addon) still inherits the device descriptors and can hang the same
  way.
- **TiDB autocommit publishes** treat a server error whose message contains
  "write conflict" or "try again later" as a known non-commit. That rests on
  TiDB never using those words for an undetermined commit, which was not
  verified against TiDB itself.
- **SlateDB**: if a publish future is cancelled while persisting, the
  provider is not marked failed and its in-memory revision may lag a write
  that committed. Not verified, not fixed.
- **SQLite VFS**: if re-taking the shared lock fails with an I/O error after
  a refused exclusive lock, the lock files report PENDING without holding
  the shared lock.
- **Windows** has no disk cache and no delegated SQLite hand-off; tests for
  both are skipped there.
- **Flaky jobs seen during this work**: `windows-node` (a WebDAV step at its
  20 s limit) and `ozone-compositions` (an IOPS check). Both pass on rerun
  and neither was changed.
- **Linux-only behaviour** was verified in a container for FUSE and 9P only.
  The other changes were tested on macOS and by CI.

## Gaps found outside the audit

Found by a read-only survey after the audit items. The three rated high
were verified and fixed in part (#54, #55, #56); what is left of them is
listed first. The rest have not been verified further or started.

| Gap | Failure mode | Severity |
|-----|--------------|----------|
| NFS file-handle table has no default cap (`transports/mount-rs-nfs/src/handles.rs`) | A crawl over a large tree grows memory without bound. A default cap would return stale-handle errors for files a client may still have open, so it needs a decision | High |
| PGlite and TiDB statements have no timeout; PGlite does not reconnect | A server that is alive but stalled hangs one statement, and every operation queues behind it. A timeout would fail the mount closed when the backend is merely slow, so its value is a deployment decision | High |
| Exclusive (writeback) mode publishes namespace changes only on fsync, shutdown or reconcile | Creates and renames made without fsync are lost on SIGKILL, OOM or power loss | Medium |
| A remote mount rejects every operation after one transport error, and the CLI never checks its keepalive worker | A network blip leaves the mount attached and returning `EIO`, with no log or exit | Medium |
| `/readyz` ignores drive health | A load balancer keeps routing to a drive that has failed closed | Medium |
| The mount CLI wires no transport error hooks | A filesystem that failed closed shows up only as `EIO` to applications | Medium |
| The S3 server buffers up to 512 MiB per non-streamable request with no concurrency limit | A few concurrent oversized bodies exhaust memory | Low |

The survey did not examine 9P fid tables, the FUSE inode table, WebDAV lock
tables or the Node bindings in depth.

## Locks

Production code holds no `std::sync::Mutex` or `RwLock`. Exclusion is done
by in-house types:

- `TurnCell`: a waiter spins, then yields; only for short sections.
- `ParkCell`: `TurnCell` for sections that can be long. A waiter queues
  and parks its thread instead of spinning.
- `TurnLock`: a waiter queues; may be held across an await.
- `TurnGate`: the queue of a `TurnLock` with no value inside it.
- `DrainGate`: many shared holders or one exclusive holder, which waits
  for the shared ones to leave.
- Three one-off guards: `Turn` in the vendored `quinn-proto`, and the
  try-turn on the chunked block cache index and on the host disk cache.

Every field of every one of these was audited, and each one that guarded
nothing a cheaper primitive could not guard was removed.

| Removed | Replaced by |
|---------|-------------|
| Node binding: driver slot, file handle, ten server callbacks, key-value callbacks, reconcile and delegation slots | An atomic pointer swap |
| Node binding: the list of pending JavaScript calls, which grew by one entry per call and was never pruned | A generation counter that every pending call watches |
| Request and error counters in the NFS, 9P, S3 and WebDAV sessions and the SQLite statement trace | Atomic counters over a fixed set of keys |
| Task and shutdown handles taken once at close (NFS, S3, FUSE, PGlite, blob cache, peer transport) | An atomic take |
| 9P listen address, FUSE first transport error | A write-once cell |
| Catalog snapshot cache | An atomic pointer, replaced only by a newer revision |
| Key-value file handle position and closed flag | Atomics ordered by the entry lock already held |
| Persisted snapshot version, 9P serve task, WebDAV accept task | Moved inside the lock that was always held around them |
| NFS directory snapshot map and its eviction order (two cells that could disagree) | One cell |
| Node stream request holders (run once; the body stream is not `Sync`) | `TakeCell`, a take-once atomic pointer |
| Node S3 server slot, NFS `destroyed` flag | An atomic, with each side writing its own flag before reading the other's |
| Node shutdown attempt result and its waiter list | A watched value |
| NFS per-connection task list, which grew for the server's lifetime | A task set owned by the accept loop, which forgets each connection as it ends |
| S3 debug in-flight table | A flag carried by the request |
| 9P listener slot | An atomic take |
| 9P connection write half, locked only by its one writer task | Owned by that task, which hands it back when stopped |
| Peer identity install gate | A compare-and-swap on the slot it guarded |
| Peer listener counter registry, blob-cache active scope | An atomic pointer to a replaced-whole value |
| Node JavaScript call result and waker | A one-shot channel whose sender is taken once |
| Node shutdown state machine | A compare-and-swap between phases, with the callback in its own slot |
| 9P and S3 assertion logs | `AppendLog`, a push-only list read without a lock |
| Blob-cache flush gate | Nothing: the write gate already orders flushes, and cache fills are keyed by a content-checked id |
| PGlite state-store client | An atomic pointer; closing swaps it out and waits for the connection to end |
| S3 connection shutdown list, Node open-handle callback list | `AppendLog`; shutdown reads a snapshot, and one added after it is missed exactly as it was when a lock ordered the two |
| Live-mount registries (automatic transport, native NFS), SQLite trace registry, CLI drive metrics map | An atomic pointer to a list or map replaced whole |
| Authenticator policy cache, one lock held across every key fetch | One atomic snapshot per policy, read without waiting; a turn is taken only by a caller that must fetch |
| Chunked block cache per-block fill turn (`DrainGate` that never had a shared holder) | `TurnGate`; the exclusion stays, the unused shared half goes |
| SQLite storage connection (metadata and block stores), which was held through fsync and SQLite's busy wait on the caller's thread | A thread per database that owns the connection. Each operation is one job sent to it; jobs run one at a time in arrival order, and an async caller awaits the reply instead of blocking a runtime thread. A job that panics fails that call and every later one |
| Service session handle table, taken on every remote handle read and write | A fixed table of 1,024 atomic slots with an atomic id, count, closed flag and revision. An insert publishes then re-checks; close and a revision change set their flag then sweep, so each handle is closed exactly once |
| Blob cache: list of puts waiting for a flush | An immutable list behind an atomic pointer: a put prepends, flush takes the whole list, delete swaps in a filtered copy |
| Blob cache: map of fetches in flight | A slot table probed by key hash. The per-key gate stays; a full probe window gives an unshared fetch, which is safe because fills are keyed by a content-checked id |
| Blob cache: peer connection map, a queueing lock taken by every request | One atomic slot per peer, read without waiting; a gate is taken only to dial |
| Blob cache: scope registrations (were inside the index lock) | An atomic pointer to a map replaced whole |
| Cluster task join turn | A forwarder task publishes the join result once; joiners watch for it |
| CLI owner-service and remote-runtime task handles | The same forwarder: close waits on a watched result instead of holding a lock across the join |
| Tunnel transport: connection turnover counters | One packed atomic word |
| Tunnel transport: refusal log limiter | An atomic window per key, replaced by compare-and-swap |
| Tunnel transport: dial-gate map, draining list, server diagnostics map, published connection pool | An atomic pointer to a small, bounded collection replaced whole |
| Memory metadata store state, which serialised whole namespaces inside a spinning cell | An atomic snapshot replaced by compare-and-swap; the work is done outside and retried if another writer got in first |
| SQLite VFS: name registry, WAL region map | An atomic pointer to a map replaced whole, with retired entries kept on a push-only list |
| NFS: client table, mount table, exclusive-create table, provisional handles | Atomic maps replaced whole (changed per connection or mount, not per call) and per-entry compare-and-swap |
| NFS handle verifier and stable-inode flag | A plain field and an atomic |
| 9P: in-flight request table, taken twice per request | A two-level table of atomic slots indexed by tag |
| 9P: negotiated message size, generation and destroyed flag | Atomics, so a request no longer takes the session cell to read them |
| 9P: byte-range lock table | An atomic client counter and an atomic persistent map of per-path lock lists; a lock or unlock rewrites one path, not the map |
| 9P: server connection registry | An atomic map; an empty slot means closed, so an insert cannot land after close |
| 9P: unmount lock | Merged into the release lock that was always taken with it |
| WebDAV: method counters | 64 write-once slots with atomic counts; counting no longer allocates |
| WebDAV: connection task registry | A watched generation: each connection subscribes before it is spawned, and abort-all bumps it |
| WebDAV: lock table, swept on every write request | An atomic snapshot. Readers filter expired locks without writing; LOCK, refresh and UNLOCK are one compare-and-swap each |
| HTTP server task handle | A write-once listener with a supervisor that publishes the join result. This also fixed a bug: with port 0, two racing listens could report the port of the listener that was dropped |
| S3: conditional-put registry | An atomic map of entries with a use count; an entry is retired with a dead marker so a late user makes a new one |
| FSKit handle table | A plain map: the worker is already used through `&mut` |
| Node: 9P server state | An atomic slot; one closer at a time claims the listener and only it clears the slot |
| Node: WebDAV lifecycle lock | Removed; the closed flag moved into the transport's own lifecycle lock, which was already taken |
| Chunked block cache `io_gate`, which made every delete wait for all in-flight reads and puts | 64 delete counters, bumped inside the index turn. A fill or put records its counter before the backing call and is not cached if it moved |
| SQLite snapshot store connection, held through a durable commit on a runtime thread | The owner thread: load and save are each one job |
| SQLite block-store group-commit turn | A lock-free queue of puts drained by the owner thread into one transaction. Reads go on a priority lane that runs before the next commit. Puts are as fast as before and four-thread cached reads about 15% faster; under mixed load puts are about twice as fast and the slowest 1% of reads rise from about 0.1 ms to about 0.9 ms, because twice as many commits get through |
| Catalog connection pool, 8 SQLite connections each behind a lock | 8 owner threads, one per connection; each load or update is one job. Loads with 16 callers are about twice as fast (83k to 170k per second); one caller and updates are at parity |
| PGlite client, one lock across each transaction | An owner task that runs whole operations in arrival order; a caller that goes away cancels its transaction. At parity against a real server. Found a deadlock under the old lock: a durable publish called flush while holding it and never returned |
| FoundationDB client network start and stop locks | A lease counter and write-once cells (`NetworkGate`), with 13 race tests on fake start and stop actions. The native path is type-checked here but not run: this machine has no FoundationDB client library, so the live-cluster CI jobs are its first run |
| Blob cache Redis discovery connection | An owner task; the caller's deadline still covers queueing, connect and the exchange. At parity for one caller, faster for eight |
| FUSE reply writer | Nothing on the kernel device, where one write is one whole message. Parity, and 8% faster with 16 requests in flight; 10 of 10 native mount runs on Linux |
| NFS reply writer | A queue of framed replies and an atomic slot holding the socket: the call that takes the socket writes everything queued, up to 16 frames per write. One call at a time is at parity; small replies are 28% faster at depth 16 and 91% faster at depth 64 |
| Node FUSE session | An owner task fed in JavaScript call order. `handle()` is at parity one at a time and about 25% faster 16 at once |
| Node S3 and WebDAV body streams | A combining queue: a pull that finds the body idle runs in place; one that does not is queued and run in order. About 15% faster |
| kv state (attributes, directory set, open-file table, counters) | Atomic counters, an atomic snapshot of persistent maps, per-path attributes updated in place, and a bucketed open-file table. Eight callers: stat 30 times faster, open and close 12 times, rename 70 times. One caller: parity, except open and close 17% slower (1.75 to 2.05 microseconds) |
| Fault injector state (test support), a spinning cell held while cloning and sorting up to a million events | An atomic snapshot: the decision, its sequence number and its event are published in one compare-and-swap; the trace walks an immutable list |
| WebDAV response handle close gate | One atomic state, open, closing or closed. Each response has one owner, which closes it once and may retry, so nothing has to wait: a close that fails or is cancelled puts the state back to open, and a close that meets another in flight is refused instead of being told Ok |
| FUSE device stream-reply lock, which kept a reply written in pieces to a byte-stream descriptor together | Removed with the case it served. `from_owned_fd` refuses a descriptor that is not message framed (the kernel device, `SOCK_SEQPACKET`, `SOCK_DGRAM`), so every reply is one `write(2)`. Only this crate's tests used a byte stream; they now use message sockets |
| Node standalone 9P fid table (`new P9FidTable()`) | Thread confinement: an `Rc<RefCell>`. Every method on it is synchronous and runs on the JavaScript thread that owns it, and the type cannot be sent to another thread |
| Chunked block cache and host disk cache write gates, which made a flush wait for puts in flight and hold back new ones | Removed. They guarded no cache state. A flush covers every put acknowledged before it was called, which is the `BlockStore` contract and the backing store's to keep, as it is for an uncached store; the gates added a stronger promise nothing could rely on across stores, at the cost of stalling every put behind a flush |
| NFS retained-handle map | An atomic snapshot over a persistent map. Per-request lookups are a load; a racing insert is settled by one compare-and-swap, the loser closing its own descriptor and taking the winner's; each close is claimed by compare-and-swap on the entry, as the provisional-close table already did |
| NFS directory listing cache (`DirectorySnapshots`) | A fixed table of atomically swapped slots, one listing each, with a use stamp per slot; a new directory takes an empty slot or the least recently used one by compare-and-swap. At the default 64 directories, eight callers read 13 to 19 times faster (1.3 to 2.0 against 24 to 25 microseconds) and store about twice as fast; one caller reads at parity and stores 0.4 to 0.5 microseconds slower (2.2 against 1.8 for a cached directory), which is over the usual 100 nanosecond allowance and was accepted for the many-caller gain. Choosing a slot reads every stamp, so at 1,000 directories a store is slower than the lock was for one caller (5.0 against 2.1 microseconds) and for eight (about 130 against 41): keep `snapshot_cache` near its default. For callers storing one directory at the same moment the cache may briefly hold it twice; a read returns the newest, and each storer removes the older ones. Two persistent-map versions built earlier were 37 to 87% slower for one caller and were not used. Benchmark: `handles::tests::bench_directory_snapshots` |
| SQLite VFS process-local WAL-index state (lock slots and mapped pages) | The eight SQLite SHM lock slots are one atomic word, a byte each, so a lock over several slots is judged and installed by a single compare-and-swap with nothing to roll back; a connection's own holdings answer every "do I hold this" question. Mapped pages are an append-only published list: a caller that loses the race to add a page drops its own and takes the winner's. At most 254 shared holders per slot; the next is told busy |
| Cluster client handle gate (`mount-rs-remote`), which made a close wait on the client for the handle's calls | Removed as a duplicate. The owner's per-handle gate already orders cursor moves, writes and close and makes close wait for admitted calls, and the client gate could not do more, since a cancelled call released it while the request still ran on the owner. The client keeps an atomic open, closing or closed state; a second close is sent to the owner, not answered locally, and a failed or cancelled close leaves the handle open to retry. A close now waits on the wire, so cancelling one that is waiting cancels a sent request |
| Remote client handle gate (`mount-rs-remote-client`), which made a close wait on the client for the handle's calls | Removed. A call behind the close finds the handle gone on the server and gets `EBADF`; one ahead of it is ordered against the close by the provider's own handle, where that ordering has to hold anyway because the server closes handles in its own sweeps. Reading every provider handle for this found one that did not hold it: a key-value handle could acknowledge a write into a buffer its close had already dropped. That is fixed in the handle (it checks again under the file's lock) |
| Node `PathLock` state (readers, the writer, the gate and the ticket-ordered writer queue) | A combining owner: callers post reserve, acquire, cancel and release messages to a lock-free inbox, and whichever caller finds it idle applies them, its own inline when nothing is queued. Measured against the lock: one caller 36 against 30 ns for a read and 45 against 36 ns for a write; eight callers 320 ns against 14,000 to 19,000 for reads and 190 against 330 to 710 for writes. A mix of one write in eight was slower in two of three runs (175 against 62 to 68 ns). A reader now takes its place when `read()` is called, not when its future is first polled, which closes a race in which a writer polled first could gate a reader issued before it (16 in 20,000 rounds before, none after). Benchmark: `utilities::tests::bench_path_lock` |
| Blob cache write gate, which kept a flush from clearing the dirty mark over a put still inside the backing store | Three counters. A put counts itself in flight before it takes a generation number; a flush reads the generation, then the in-flight count, and records that generation as flushed only if nothing was in flight, so it never claims a put its barrier may not cover. A flush that overlaps a put leaves the store dirty for the next one. Flushes no longer wait for puts |
| Blob cache index (`LocalCache.state`) | One immutable value replaced by compare-and-swap, over a persistent hash trie, with recency stamps in atomics beside it so a hit writes nothing shared; only the winning attempt's files are unlinked. Measured against the lock on 1,024 resident blocks: a hit 24 against 48 ns for one caller and 0.5 against 23 microseconds for eight; insert with eviction 5.4 against 8.7 microseconds for one caller, and 120 to 126 against 96 to 97 for eight, the one case that is slower, because a losing attempt reruns its scan. Benchmark: `local::tests::profile_index` |
| Blob cache disk turn (`LocalCache.disk_io`), which made every disk insert, eviction and invalidation take turns | Narrowed, not removed: write-once file names, `{key}.{generation}`, so no two inserts touch the same name and inserts no longer take turns with each other. What is left is the `files` gate in the table below, which only an invalidation takes exclusively. An earlier attempt, a disk-writer thread, had measured slower and was reverted; this one does not: one inserter 0.23 ms median either way, eight inserters 5.4 to 5.9 thousand a second against 3.9. Caches in the old layout are adopted at startup; an older binary ignores the new names. Benchmark: `local::tests::profile_disk_inserts` |
| Chunked pending access times gate | One atomic slot per inode with an atomic time; the sweep closes a slot by compare-and-swap |

Fixed keys mean a client can no longer grow a counter map: undefined NFS and
MOUNT procedure numbers are counted as `unknown`, and WebDAV counts at most
64 distinct method names before using `OTHER`.

**Every lock that remains** has a written failure: the interleaving that
goes wrong if the lock is taken away or replaced by atomics, a
compare-and-swap or a copied snapshot. A proof pass read every access site
of every lock and had to produce either a removal plan or that
counterexample. The plans were then built and tested (the table above); the
counterexamples are below. A lock with no counterexample was removed.

`python3 scripts/lock-inventory.py` lists every lock field, static and
type alias in production code and fails if one is not named in a row for
its crate below, so a new lock cannot be added without a reason written
here. It also fails on any production line that names a standard-library
or `parking_lot` lock at all, whether or not the line is a field, so a
local variable or a renamed import is caught too. CI runs it. Production
code names no standard-library, `parking_lot` or `tokio` lock; every mention
of one is in test code.

That is a statement about lock types, not about everything that makes a
caller wait or excludes one. Outside it, and not in the table below:

- `tokio` semaphores used as budgets (in-flight requests, cache workers and
  fills, local write bytes), and `tokio` channels, `watch` and `Notify`,
  which keep their waiters behind an internal lock of their own.
- Try-only atomic claims that never wait: the combining queues (`Serial` in
  the Node binding, the chunked mutation queue's runner flag), the 9P and
  Node "already serving" flags, the NFS reply writer's idle slot, and the
  SQLite VFS lifecycle word.
- Two hand-written turns over a cache index (chunked block cache, host disk
  cache) and quinn's `Turn`, which are listed below under their own names.

**Kind** says what the lock is for:

- **S**: several values change together and must agree.
- **C**: a collection where a check and the change must be one step.
- **A**: held across an await or a blocking call on purpose, so a second
  caller waits for the first or one exchange owns a connection.
- **R**: a resource that is not safe to share.
- **D**: de-duplication only. Nothing breaks without it; the same work is
  done many times.

| Crate | Lock | Kind | What goes wrong without it |
|-------|------|------|----------------------------|
| bindings/mount-rs-napi | `HandleState.close_gate` | A | Held across the JavaScript close. With a flag, a second closer returns "closed" while the first close is still running; if that close rejects, the handle is open and the second caller was told otherwise |
| bindings/mount-rs-napi | `native_delegation_mounts` | A | A mount passes the "no active mounts" check and awaits the mount; a scope check-in sees an empty list and checks the directory in; the mount is then recorded: a live delegated mount over a directory that is not checked out |
| bindings/mount-rs-sqlite-vfs | `StorageVolume.state` | A | Held across provider round trips. A delete and a commit both publish from revision R; one is rejected and marks the volume failed: a healthy volume fails for good on a benign race |
| crates/mount-rs-blob-cache | flight `Entry.turn`, `PeerLink.dial` | D | Two misses on one block both go to peers and the backing store; two requests to one peer both dial |
| crates/mount-rs-blob-cache | `LocalCache.files` | A | Inserts hold it shared and an invalidation exclusively, so only invalidation waits. Without it an insert renames its file into place; an invalidation of that block finds nothing to remove and returns; the process dies before the insert indexes or removes the file: the next start adopts a cached copy of an invalidated block. A first version of this change had no gate and a review found exactly that; with the gate taken out of both invalidations, `an_invalidation_waits_for_an_insert_whose_file_is_not_yet_indexed` fails. Having the insert re-check an invalidation count after indexing closes the race in a running process but not the crash between rename and re-check, and a durable invalidation record would cost a synced write per invalidation |
| crates/mount-rs-cluster | coalesce `Entry.turn` | D | N requests on a cold route make N registry lookups |
| crates/mount-rs-cluster | `Slot.gate` | A | Two closes both pass the open check and both call `close()`. With a flag, close runs while a read is still inside the handle, and two cursor reads interleave on one cursor |
| crates/mount-rs-cluster | `path_operations` | A | A path call passes the closing check and is inside the driver; close finds no handles and returns; the server releases the volume lease while that call is still writing |
| crates/mount-rs-remote-client | `renew_lock` | A | Two renewals fetch tokens expiring at 100 and 200. The server applies them in one order and the client stores them in the other, so the client believes 200, stops renewing, and at 100 the server closes its handles |
| crates/mount-rs-service | `PolicySlot.refresh` | A | Two requests both see the last fetch as a minute old and both fetch, breaking "at most one key fetch a minute" before authentication. With a compare-and-swap the loser cannot wait, so a valid token signed by the rotated key is rejected |
| filesystems/mount-rs-chunked | `StateCell.mutex` | S | The handle-transition counter is read then added to. Two deciders both see it even and both add, leaving it even while both still decide; a lock-free open then takes a reference to an inode that unlink has just dropped (`ESTALE` on an open handle) |
| filesystems/mount-rs-chunked | block cache `Index` turn | S | With a retried compare-and-swap, a losing insert has already published its block to the lock-free front: a block served from cache after its delete |
| filesystems/mount-rs-chunked | `gate` | A | A write and a mutation batch both snapshot revision r. The batch publishes r+1; the write publishes expecting r, is rejected, and fails the volume closed: a healthy single owner poisons itself |
| filesystems/mount-rs-chunked | `lease_gate` | A | A publisher takes the cached lease token; the heartbeat renews it; the publish goes out with the old token, is rejected, and fails the volume closed |
| filesystems/mount-rs-chunked | `lifecycle` | A | A write holds it shared while uploading blocks. Without the drain, shutdown releases the lease and returns; the write then fails after its blocks are stored (orphan blocks), and reads still call the block store after shutdown returned |
| filesystems/mount-rs-chunked | `ChunkedHandle.gate` | A | Two cursor reads both load position p and both store p+n (the same bytes twice). Two closes both pass the closed check and both release the reference: the inode is reaped under an open handle |
| filesystems/mount-rs-chunked | cache `Fill.turn` | D | 21 concurrent misses on one block make 21 backing reads; a test asserts 1 |
| filesystems/mount-rs-host | disk cache `io` | A | A fill reserves space; a delete removes the entry and syncs the directory; the fill's rename lands; the backing delete completes; a crash before the stale file is unlinked: restart re-indexes a cached copy of a deleted block |
| filesystems/mount-rs-host | `HostHandle.state` | A | Two cursor reads both read the position, read the same range and both store. The kernel cursor cannot stand in on Windows. A close by swap returns while a worker still holds the descriptor |
| filesystems/mount-rs-host | disk cache `Shared` turn | S | With a retried compare-and-swap, a losing reservation has already removed a key from the read index; the index and the entry table disagree and `make_room` panics |
| filesystems/mount-rs-kv | each `OpenEntry.file` | S | Two cursor writes on one handle both load the position and both write at p. A copied snapshot would copy the whole file on every write. The same section joins a handle to the open file against the last close removing it, and clears the dirty mark with the copy it saves |
| filesystems/mount-rs-kv | each `OpenEntry.saving` | A | Two flushes of one file snapshot in order but reach the store out of order: the older save lands last and the file is marked clean. This was a bug; the gate is the fix |
| filesystems/mount-rs-memfs | `MemoryFs.inner` | S | Two cursor writes both load the position and write at p. Reserving the position first breaks short reads (the cursor ends past end of file and the next write leaves a hole). The node map and the quota counters also change in the same step |
| filesystems/mount-rs-persist | `SaveGate.lock` | A | Save 1 snapshots; save 2 snapshots a superset and writes; save 1's write lands last: an acknowledged write is gone after restart. A versioned store turns that into a conflict error on a write that already succeeded in memory |
| filesystems/mount-rs-versioned | `ViewLifetime.state` | A | Two begins both copy the pin and both renew it. The first rotates the pin; the second fails its exact-expiry check and closes a healthy view |
| filesystems/mount-rs-versioned | `HistoricalFileHandle.admission` | A | Two closes both see "open" and both release: the view pin is released under another open handle. A flag cannot stand in, because a close that fails at the provider leaves the handle open and a second closer must wait to learn which. Reads hold it shared, so close does not release the pin under a read that already passed the closed check |
| providers/mount-rs-object-store-blocks | cache `ObjectStoreBlockCache.state` | S | The section publishes to and retracts from the lock-free front. With compare-and-swap, A inserts X; B removes X and retracts nothing; A then publishes X: X is in the front but not the index and is served for ever |
| providers/mount-rs-slatedb | `SlateDbMetadataStore.state` | A | Held across the persisted write. A publish starts writing {rev 6, fence 3}; the lease expires and an acquire writes {rev 5, fence 4}; the publish lands second. Disk says fence 3, and after reopen fence 4 is issued twice |
| transports/mount-rs-9p | `MountState.release` | A | The monitor is inside `server.close`, awaiting client closes with the socket file still present; an unmount sees its own `server.close` return at once (the server is already marked closed) and removes the directory before the socket file is gone: `ENOTEMPTY`, and unmount reports failure for a mount that is gone. A failed release must also be retryable, so a write-once flag does not fit |
| transports/mount-rs-9p | `P9Server.adoption` | A | Held across the `mount` subprocess. Two mounts on one shared server both snapshot the client set and both run `mount`; each adopts the first connection not in its snapshot, so both adopt the same one. One mount then watches the other's kernel connection (its `active()`, `wait_closed` and monitor answer for the wrong mount) and one kernel connection has no monitor at all. A connection carries nothing that names its mount, so there is no cheaper way to pair them |
| transports/mount-rs-9p | `SessionInner.state` | S | Two opens of one fid both pass the "not open" check and both open a backend handle; the first is never closed. A copied snapshot copies an unbounded fid table per walk step |
| transports/mount-rs-9p | `path_lock` | A | A walk reads fid path `/a` and stats `/a/x`; a rename moves `/a` to `/b` and rewrites fid paths; the walk then stores `/a/x`: a fid naming a path that no longer exists |
| transports/mount-rs-nfs | `FileHandleTable.state` | S | One call inserts handle N unpinned; another call's limit check evicts N; the first then pins a missing entry: `ESTALE` on a file just looked up. That defeats split atomics, not one atomically published table, which was built and measured slower: see below |
| transports/mount-rs-nfs | NFSv4 `state` (`Nfs4Session`, `InFlightSlot`) | S | Two retransmissions of one (session, slot, sequence) both see nothing in flight and both run a non-idempotent operation. Two LOCK owners both find no conflict and both insert overlapping exclusive ranges |
| transports/mount-rs-nfs | `listen_lock` | A | Two closes: the second finds the accept task already taken, skips the connection drain and destroys the session, closing retained descriptors while the first close is still draining connections whose READ and WRITE hold no path lock. Two listens both read the address and both bind: `EADDRINUSE` on a fixed port, two listeners on port 0. Taken once per listen and once per close |
| transports/mount-rs-nfs | native `unmount_lock` | A | A forced unmount is in progress; a graceful one finishes and marks the mount gone; the forced one marks it mounted again: a retry runs `umount` on whatever now sits at that mount point |
| transports/mount-rs-nfs | `path_lock` (`SharedNfsState`, `Nfs3Session`, `Nfs4Session`) | A | LOOKUP stats P; REMOVE unlinks P; LOOKUP binds a handle to P. The next CREATE at P reuses the id, so the old handle addresses a different file |
| transports/mount-rs-s3 | per-key conditional-put gate (`ConditionalPutEntry`) | A | The driver has no compare-and-swap for `If-Match`. Two PUTs with the same `If-Match` both pass the check and both write: two conditional PUTs succeed |
| transports/mount-rs-webdav | `lifecycle` | A | The server can restart. A listen passes its checks and awaits bind; a close finds nothing and returns Ok; the listen stores its task: close returned Ok and the server is accepting |
| vendor/tunnel-transport | per-destination dial gate (`DialLocks`) | A | Two connects to one destination both miss the pool and both dial; the second insert replaces the first, which is then outside the pool and missed by shutdown. Inserting by compare-and-swap and closing the loser would fix that, but every racing caller would still dial and take a connection permit, so a reconnect burst turns into capacity errors |
| vendor/quinn-proto | `TokenMemoryCache` | S | A map with its order list. Two lock-free versions were built and measured slower: see below |
| vendor/quinn-proto | `BloomTokenLog`, `QlogStream` | S, R | Not compiled in this workspace (their features are off). A filter turnover racing an insert accepts a replayed token; the qlog writer is used through `&mut` |

**Kept because the replacement measured slower.** For each of these the
removal was built, tested and benchmarked against the lock, then reverted.
The benchmark is in the tree as an ignored test.

The rule: a replacement goes in if one caller is no slower than run-to-run
noise, or if one caller loses under 100 ns an operation and eight callers
gain. Both locks below fail it on both counts: one caller loses more than
100 ns and eight callers are slower too. The rule was set aside once, for
the NFS directory listing cache in the table of removed locks: one caller
loses 0.4 to 0.5 microseconds on a store, over the allowance, but eight
callers read 13 to 19 times faster, so the lock was removed and the cost is
recorded there. No lock is kept whose replacement won for eight callers.

| Lock | Replacement built | Measured | Benchmark |
|------|-------------------|----------|-----------|
| NFS `FileHandleTable.state` | One atomically published table of persistent maps, with a use stamp per entry and eviction inside the compare-and-swap loop | One caller doing bind-and-pin, decode, unpin on 4,096 handles: 0.73 to 0.93 microseconds under the lock, 2.5 to 3.3 without, since the pin and its release each copy a map path and publish a version. Binding a new name at the handle limit: about 1 microsecond against 70 to 76, since choosing the victim reads every stamp. Eight callers on the hot path measured 3 to 6 times faster without the lock in one session and a third slower in a repeat (102 to 109 microseconds against 75 to 76), so no gain for many callers is claimed. The one-caller loss is about 2 microseconds, far over the 100 nanosecond allowance | `handles::tests::bench_handle_table` |
| quinn `TokenMemoryCache` | Hashed buckets replaced by compare-and-swap, with `arc-swap` and with epoch reclamation | Repeated with nine rounds of each. The better of the two, epoch reclamation, against the lock: an evicting insert by one caller takes 371 to 375 ns against 128 to 169, a loss of over 200 ns where 100 is allowed; eight callers do 0.85 to 0.88 million evicting inserts a second against 2.7 to 3.1 million, and 6.4 to 6.7 million plain inserts against 7.3 to 11 million. Only a lookup is level (one caller) or about level (eight). The `arc-swap` version is slower than that on every operation. So the replacement loses for one caller and for eight | `token_memory_cache::tests::token_cache_bench` |

One cost showed up in every replacement that publishes with `arc-swap` on a
write path: a store takes about 50 nanoseconds for each thread in the
process that has used `arc-swap` (88 ns alone, about 500 ns with eight
other threads, 1.7 microseconds with 32), while a load stays near 10 ns.
An atomic snapshot is therefore a good trade where reads dominate and a
poor one where the lock's whole section is shorter than one publish.

The first round of this work kept eleven locks on an estimate that an
owner task or a snapshot would be slower. Measured, eight of them were not:
they are in the removed table above. The estimate was wrong most often
where the lock was held across slow work, because the replacement also
batches or stops callers queueing.

Three removals trade a little memory for the lock: the S3 connection
shutdown list, the Node open-handle callback list and the retired SQLite
VFS list are push-only. They hold a small entry per connection, handle or
registration and are freed when the server, driver or process ends.

**Behaviour that changed with the removals:**

- Chunked block cache: a delete no longer waits for in-flight reads and
  puts. A read racing a delete can return "not found" where it used to
  finish first. A block is never served from cache after its delete
  returns.
- Service session handles: ids are no longer consecutive, and at most
  1,024 are open per session, as before.
- 9P: clearing the in-flight table (on version negotiation, reset or
  destroy) visits the slots one by one, not in one step.
- WebDAV: the "lock table poisoned" error is gone. With a clock that runs
  backwards an expired lock can reappear until the next write.
- Node 9P server: while a close is running the getters still report the
  server.
- SQLite block store: a put that finds fewer puts queued than the last
  commit carried waits up to 250 microseconds on the owner thread for more
  to arrive. A read can overtake a put that has not been acknowledged.
- Fault injector: a trace read while outcomes are being recorded can show
  an outcome as pending or final.

**Bugs found under the old locks and fixed**, each with a test that failed
before:

- kv: an open racing the last close could land on a buffer the close had
  dropped; the next open built a second buffer and a write was lost.
- kv: two flushes of one file could reach the store out of order.
- HTTP: with port 0, two racing listens could report the dropped
  listener's port.
- 9P: an invalid lock type was accepted.
- Node 9P: a second listen on port 0 could bind a second server and
  replace the first.
- Node: overlapping calls from JavaScript did not run in call order. Four
  overlapping `next()` calls damaged an S3 body in 3 of 3 runs, and a
  LOOKUP issued after the MKDIR of the same name ran first in 45 of 3,000
  pairs. Calls now run strictly in call order.
- kv: an open could keep contents it read from the store before another
  buffer for the same path was saved, and then save over the newer write.
- kv: a close could drop a buffer while a later save of it was still in
  flight, so the next open read the store too early.
- PGlite: a durable publish deadlocked on its own flush.

Known and not fixed: kv path `truncate` can still modify an entry the last
close is detaching; a kv flush cancelled mid-write leaves the file marked
clean; a cancelled WebDAV `close()` detaches the accept task; FSKit sets
its shutting-down flag only after `shutdown()` returns; an unconditional
S3 PUT does not take the conditional-put gate; a blob-cache flush can
re-cache a block deleted during the flush (stale presence, not wrong
bytes).

**Sections that were too long for a spinning lock** are now `ParkCell`:
the same exclusion, with waiters parked in arrival order.

| Where | What is held |
|-------|--------------|
| Blob cache local store | An eviction scan over every entry |
| Memory filesystem state | Serialising the whole filesystem for a snapshot |
| Key-value state and open files | Whole-file copies |
| NFS handle table and directory snapshots | `forget` and `remap` full scans, and a deep copy for READDIR |
| NFSv4 state, NFS retained handles | Scans of every client, open and lock; spawning in a loop |
| 9P session state | A scan of every known path per walk step |
| Chunked state cell | Was also scanning the whole namespace in `reconcile_blocks` and folding over every orphan on each lock and unlock; the scan now runs outside the cell and the fold is a running count |
| SQLite VFS WAL region, Node path lock | Lock-range checks; waiter lists |

The host disk cache turn no longer unlinks files or syncs the directory
inside the turn: it forgets the keys, and the worker that holds `io`
unlinks and syncs afterwards.

The authenticator's policy cache was a `TurnLock` held across an OIDC key
fetch, so every request queued behind one slow issuer. Keys are now an
atomic snapshot per policy. A request whose keys are cached never waits; a
request that needs a fetch waits only for its own policy's.
