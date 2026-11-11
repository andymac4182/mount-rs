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
by two in-house types: `TurnCell` (a waiter spins, then yields; only for
short sections) and `TurnLock` (a waiter queues; may be held across an
await). Every field of either type was audited, and each one that guarded
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

Fixed keys mean a client can no longer grow a counter map: undefined NFS and
MOUNT procedure numbers are counted as `unknown`, and WebDAV counts at most
64 distinct method names before using `OTHER`.

**Every lock that remains** was then re-read, access site by access site,
by two independent reviews told to look for a replacement first. They found
seven more, which are in the table above. Each of the rest is kept for one
of four reasons:

| Reason it must stay | Fields |
|---------------------|--------|
| Several values change together and must agree | 9P: lock table, session state, in-flight tags. NFS: handle table, directory snapshots, exclusive creates, retained and provisional handles, NFSv4 state. WebDAV lock table. Chunked runtime state, memory filesystem, key-value state and open files, memory metadata, object-store block cache, blob-cache local index, fault injector, session handles, SQLite VFS WAL state, Node path lock, Node 9P server state and fid table, peer turnover counts, refusal log windows, QUIC token caches |
| A collection that concurrent callers insert into and remove from, where a check and the change must be one step | Connection tables (9P, NFS, WebDAV, S3 shutdowns, peer server), live-mount registries, mount records, blob-cache flights and pending writes, CLI drive metrics, Node callback list, S3 conditional-put locks, peer dial locks and draining list, SQLite VFS registry and WAL regions, SQLite trace registry, 9P and S3 assertion logs, WebDAV method counts |
| Held across an await on purpose, so a second caller waits for the first to finish or one exchange owns a connection | Unmount, release, adoption, listen and lifecycle gates (9P, NFS, WebDAV, HTTP, Node); FUSE and NFS reply writers; close gates; save gate; versioned view state; SlateDB state; SQLite VFS volume state; Redis and peer connections; policy cache; single-flight and per-key gates; CLI worker and task handles; Node FUSE session, response body streams and delegated mounts; FoundationDB network start and stop; peer pool writer |
| A resource that is not safe to share | SQLite connections (provider, storage, catalog); the PGlite storage client, whose transactions need exclusive use; host file handle |

Three were judged removable in form but are kept, because removing them
changes behaviour:

- **Blob-cache `flush_gate`**: without it two flushes could fill the cache
  out of order, so an older write of a block could land after a newer one.
- **PGlite state-store client**: without it statements would be pipelined
  on one socket to a server that runs one statement at a time.
- **9P and S3 assertion logs**: the only lock-free form copies the whole
  list on every append.

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
