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
| Chunked block cache per-block fill turn (`DrainGate` that never had a shared holder) | `TurnGate`; the exclusion stays, the unused shared half goes |

Fixed keys mean a client can no longer grow a counter map: undefined NFS and
MOUNT procedure numbers are counted as `unknown`, and WebDAV counts at most
64 distinct method names before using `OTHER`.

**Every lock that remains** was then re-read, access site by access site,
by two independent reviews told to look for a replacement first, and the
merged result was swept a third time, including every `TurnGate` and
`DrainGate` field. What they found removable is in the table above,
including four that were first kept as a matter of judgment. Each lock left
is listed here with the reason it must stay:

- **S**: several values change together and must agree.
- **C**: a collection where a check and the change must be one step.
- **A**: held across an await or a blocking call on purpose, so a second
  caller waits for the first or one exchange owns a connection.
- **R**: a resource that is not safe to share.

| Crate | Lock | Why | Evidence |
|-------|------|-----|----------|
| apps/mount-rs-cli | `OwnerService.task`, `RemoteRuntime.worker` | A | Held across the join, so a second close waits and a cancelled close leaves the handle to abort or retry |
| bindings/mount-rs-napi | `FuseSession.inner` | A | Session used through `&mut` across each request, in order |
| bindings/mount-rs-napi | `HandleState.close_gate` | A | Held across the JavaScript close call |
| bindings/mount-rs-napi | `native_delegation_mounts` | A | Held across mount and unmount; two fields change around them |
| bindings/mount-rs-napi | 9P fid table (`Standalone`), `P9Server.state` | S | Fid table; server and serve task set and taken as a pair with the closed check |
| bindings/mount-rs-napi | `S3BodyStream.inner`, `WebdavBodyStream.inner` | A | Held across the next-chunk read so two pulls cannot advance the stream twice |
| bindings/mount-rs-napi | `WebdavServer.lifecycle` | A | Serialises listen against close |
| bindings/mount-rs-napi | `NativePathLock` state (three handles to one cell) | S | The body of a reader/writer path lock |
| bindings/mount-rs-sqlite-vfs | `REGISTRY` | C | Name check, SQLite registration and both maps are one step |
| bindings/mount-rs-sqlite-vfs | volume `state` | A | Held across provider round trips |
| bindings/mount-rs-sqlite-vfs | `wal_regions`, `ProcessWalRegion.state` | C, S | One region per database; lock ranges checked then applied |
| crates/mount-rs-blob-cache | `RedisDiscovery.connection`, peer `connections` | A | One exchange owns the socket; one dial per peer |
| crates/mount-rs-blob-cache | `LocalCache.state` | S | Index, byte accounting and scope counts |
| crates/mount-rs-blob-cache | `LocalCache.disk_io` | A | Held across blocking file writes against the index re-check |
| crates/mount-rs-blob-cache | `flights` and each flight gate | C, A | Get-or-insert; one fetch per key |
| crates/mount-rs-blob-cache | `pending` | C | Pushed by puts, taken by flush, filtered by delete |
| crates/mount-rs-fault-injection | `FaultInjector.state` | S | Plan, counters and event list advance per decision |
| crates/mount-rs-service | `CatalogAuthenticator.cache` | A | Held across the key fetch to allow one refresh per policy |
| crates/mount-rs-service | catalog `connections`, `SessionHandles.state` | R, S | SQLite connection; id, closed flag, revision and handle map |
| filesystems/mount-rs-chunked | `StateCell.mutex` | S | Namespace, revisions, open references; reads already use an atomic mirror |
| filesystems/mount-rs-chunked | block cache `Index` turn | S | Entries, order and byte total; arrivals already queue lock-free |
| filesystems/mount-rs-host | `HostHandle.state` | A | Guard moves into the blocking worker; one operation at a time on the file cursor |
| filesystems/mount-rs-host | disk cache `Shared` turn | S | Disk index; commits already queue lock-free |
| filesystems/mount-rs-kv | `Inner.state`, each `OpenFile` | S | Attribute map, directory set, open files; file data, references and dirty flag |
| filesystems/mount-rs-memfs | `MemoryFs.inner` | S | Inode table and quota |
| filesystems/mount-rs-persist | `SaveGate.lock` | A | Held across the save so an older save cannot overwrite a newer one |
| filesystems/mount-rs-versioned | `ViewLifetime.state` | A | Held across the pin renewal |
| providers/mount-rs-foundationdb | `CLIENT_NETWORK`, `CLIENT_NETWORK_STOPPING` | A | Held across the one-time native start and the thread join |
| providers/mount-rs-memory | `state` | S | Revision, namespace, lease and pins |
| providers/mount-rs-object-store-blocks | cache `state` | S | Entries, recency and byte total; hits are already lock-free |
| providers/mount-rs-pglite | storage `client` | R | Transactions need exclusive use of the one connection |
| providers/mount-rs-slatedb | `state` | A | Held across the persisted write |
| providers/mount-rs-sqlite | both `connection`s | R | SQLite connection |
| transports/mount-rs-9p | `P9LockTable.state`, `SessionInner.state` | S | Byte-range locks per path; fid table and users against the generation |
| transports/mount-rs-9p | `SessionInner.inflight`, server `connections` | C | Duplicate-tag check and insert; insert rechecks closed |
| transports/mount-rs-9p | `unmount_lock`, `release`, `adoption` | A | Unmount and rollback; server close and task join; the client-set comparison around the mount command |
| transports/mount-rs-fskit | `handles` | C | Handle table |
| transports/mount-rs-fuse | reply `writer` | A | Held across the frame write so replies do not interleave. The public `from_owned_fd` path writes a frame in a loop of partial writes, so one write call is not one frame |
| transports/mount-rs-http | `HttpServer.task` | A | Held across the join so the outcome is cached with the handle |
| transports/mount-rs-nfs | `FileHandleTable.state`, `DirectorySnapshots.state`, `exclusive_creates`, `provisional`, NFSv4 `state` | S | Tables whose maps, order lists and counters must agree |
| transports/mount-rs-nfs | `clients`, `mounts`, `by_id` | C | Inserted and removed by concurrent connections and calls |
| transports/mount-rs-nfs | `listen_lock`, native `unmount_lock`, connection reply writer | A | Bind against close; unmount; record frames must not interleave |
| transports/mount-rs-s3 | conditional-put `locks` | C | Get-or-insert and remove-if-unused |
| transports/mount-rs-s3 | per-key conditional-put gate | A | Held for the whole check-then-write |
| transports/mount-rs-webdav | `close_gate`, `lifecycle` | A | Held across close; listen against close |
| transports/mount-rs-webdav | `ConnectionRegistry.tasks`, `methods` | C | Spawn and insert together so abort-all cannot miss one; the key cap check and insert |
| transports/mount-rs-webdav | `locks` (lock table) | S | Locks, issued tokens and order, with expiry on read |
| vendor/tunnel-transport | `ConnectionTurnover.counts`, `RefusalLogLimiter.keys` | S | Paired counters; per-key window |
| vendor/tunnel-transport | `dial_locks`, `draining`, server `connections` | C | Check-then-insert with a bound |
| vendor/tunnel-transport | per-destination dial gate, `PublishedPool.writer` | A, S | One dial per destination; directory and slot writes must not interleave |
| vendor/quinn-proto | `TokenMemoryCache`, `BloomTokenLog`, `QlogStream` | S, R | Map with its order list; two filters with their period; a writer used through `&mut` |

The gates, each held across an await on purpose (**A**). "Shared" callers
overlap each other; an "exclusive" caller waits for them.

| Crate | Gate | Type | Evidence |
|-------|------|------|----------|
| crates/mount-rs-cluster | `Slot.gate` | `DrainGate` | Positional reads and stats share; cursor moves, writes and close wait for them. A pending open holds it so a session close cannot count the handle as closed |
| crates/mount-rs-cluster | `path_operations` | `DrainGate` | Path calls share across the driver call; close waits for them. The closing flag only stops new entry |
| crates/mount-rs-cluster | `JoinSlot.turn` | `TurnGate` | One joiner across the task join; a second shutdown waits, or inherits the handle if the first is cancelled |
| crates/mount-rs-cluster | coalesce `Entry.turn` | `TurnGate` | One registry lookup per volume; waiters re-check the route cache |
| crates/mount-rs-remote-client | `renew_lock` | `TurnGate` | One renewal across the credential fetch and exchange, with the expiry re-checked inside |
| crates/mount-rs-remote-client, crates/mount-rs-remote | `RemoteHandle.gate` | `DrainGate` | Calls share across the round trip; close (and, on the server, cursor reads and mutations) wait for them |
| crates/mount-rs-blob-cache | `write_gate` | `DrainGate` | Flush waits for in-flight puts, or a put could have its dirty mark cleared while its block is unflushed |
| filesystems/mount-rs-versioned | handle `admission` | `DrainGate` | Close waits for in-flight reads before releasing the view pin; cursor reads serialise the position |
| filesystems/mount-rs-chunked | cache `io_gate`, `write_gate` | `DrainGate` | Delete and clear wait for misses and puts so nothing re-caches an evicted block; flush waits for in-flight puts |
| filesystems/mount-rs-chunked | cache `Fill.turn` | `TurnGate` | One backing read per block; the others find it cached |
| filesystems/mount-rs-chunked | `PendingAtimes.gate` | `DrainGate` | A slot is two words; the sweep frees it only when no recorder is inside. Never awaited |
| filesystems/mount-rs-chunked | `gate`, `lease_gate` | `TurnGate` | One namespace change across the lease check and publish; one lease renewal or publication, so the heartbeat cannot swap the token in between |
| filesystems/mount-rs-chunked | `lifecycle` | `DrainGate` | Block I/O runs shared without `gate`; shutdown, syncfs, checkout and reconcile wait for it |
| filesystems/mount-rs-chunked | `ChunkedHandle.gate` | `DrainGate` | Positional reads share; cursor, mutation and close are exclusive |
| filesystems/mount-rs-host | disk cache `io`, `writes` | `DrainGate` | Delete and clear wait for gets and puts running on blocking threads; flush waits for in-flight puts |
| transports/mount-rs-nfs | `path_lock` | `DrainGate` | Ordinary calls share; remove, rename, client expiry and destroy wait so an unlinked path cannot be rebound |
| transports/mount-rs-9p | `path_lock` | `DrainGate` | Path requests share; rename waits so fid paths are rewritten with no walk in flight |
| providers/mount-rs-sqlite | `PutGroup.turn` | `TurnGate` | Group commit: one commit at a time, with queued puts and reads answered by the turn holder |

One gap from the replacements: the S3 connection shutdown list and the Node
open-handle callback list are now push-only. They hold a weak pointer per
connection or handle and are freed only when the server or driver is
dropped, so a very long-lived server grows them slowly.

**Spinning locks held for too long** (found by the audit, not fixed):

| Where | What is held under a `TurnCell` |
|-------|---------------------------------|
| `providers/mount-rs-sqlite/src/storage.rs` connection | fsync and SQLite's busy wait. Flush, verify and metadata calls can contend, so a waiter spins for the length of a disk sync |
| Blob cache local store | An eviction scan over every entry |
| Memory filesystem snapshot | Serialising the whole filesystem |
| Key-value open file | Whole-file copies |
| NFS handle table | `forget` and `remap` full scans, and a deep copy for READDIR |
| `crates/mount-rs-service/src/auth.rs` policy cache (`TurnLock`) | An OIDC fetch, so every request queues behind one slow fetch |

The first is the one that matters: it should become a `TurnLock` or run on
a blocking thread.
