# Bounded Drive Runtimes Implementation Plan

> **For agentic workers:** execute these coupled tasks in the current session with independent source and evidence review. Coordinate source freezes before each owned test gate.

**Goal:** Bound server runtime residency while preserving all catalog routes,
fresh authorization, handle lifetime and uncertain-publication ownership.

**Architecture:** Add a lifecycle pool at dispatcher lookup, keeping existing
filesystem drivers. Resolve storage construction once, open on authorized access,
pin requests and handles, and evict only healthy persistent MRC5 runtimes after
owned shutdown. Wire both CLI and the process qualification fixture to it.

**Tech Stack:** Rust 1.95, Tokio, existing SDK providers, SQLite and the existing
QUIC/WebSocket service.

## Global constraints

- Use `./scripts/cargo-shared` with an explicit checkout-isolated writable target.
- Preserve the protected sibling checkout and unrelated state.
- Full dimensions: ten servers, 10,000 clients/Drives, 5,000 Partitions,
  1,000 files/Drive. Full free-disk floor: 64 GiB. RSS cap: 24 GiB per process.
- No storage format, durable commit, local native-mount or authorization change.
- No raw CI log/artifact fetching while the earlier approval rejection is pending.
- No eviction of live, volatile, failed or uncertain owners; no dropped close
  future on timeout; no new worker generation after unproven drain.

## Task 1: SDK lifecycle health and identity observations

**Files:** `crates/mount-rs-sdk/src/filesystem.rs`,
`filesystems/mount-rs-chunked/src/lib.rs`,
`crates/mount-rs-service/tests/runtime_health.rs`,
`filesystems/mount-rs-chunked/src/runtime_health_tests.rs`.

**Interfaces:** `Filesystem::failed() -> bool` forwards sticky ChunkedFs failure;
`Filesystem::concurrent_backing_id() -> Option<ConcurrentBackingId>` forwards the
opened identity through a read-only ChunkedFs accessor. Other SDK driver kinds
report no chunked failure and no concurrent backing identity; this does not
qualify them for eviction.

The pool callback requires nonblocking observation. The concrete ChunkedFs
accessor now uses a sticky atomic latch, set before its three detailed failure
publishers wait for state and on observed state poison. Six actual-mutex
controls passed after a genuine blocking-accessor regression; healthy contention
does not quarantine an owner. This repairs the observation seam without
qualifying provider construction cleanup or fresh backing authority.

- [x] Write actual SQLite tests before the observations. Open MRC5, acknowledge
  a complete deterministic payload, inject a SQLite trigger rejecting inode
  updates, and require write failure plus sticky `failed()` before and after
  successful SDK shutdown. Remove the trigger and use a distinct fresh reader
  to verify old acknowledged bytes, EOF and the unchanged backing identity.
- [x] Retain the missing-API baseline failure separately from runtime tests.
- [x] Implement thin read-only observations without changing shutdown behavior.
- [x] Run the exact health test, SDK ordinary suite, touched strict Clippy and
  formatting under owned terminal receipts. Review the source and actual SQL
  failure/oracles before proceeding.

## Task 2: Bounded lifecycle pool and dispatcher pins

**Files:** create `crates/mount-rs-service/src/runtime_pool.rs` and
`crates/mount-rs-service/tests/lazy_drives.rs`; modify service `lib.rs` and
`dispatch.rs`.

**Interfaces:** `RuntimeFactory` opens `Arc<dyn ManagedDrive>`;
`ManagedDrive` returns an existing driver, failure/eviction eligibility/backing
identity and owned shutdown. `RuntimePool` has checked nonzero capacity,
singleflight slot activation, request/handle pins, snapshots and owned terminal
shutdown. `DriveDispatcher::register_lazy_definition` retains exact definitions
and a factory without opening. Eager registration stays available.

- [x] Write controlled behavioral cases for zero opens/denied or stale scope,
  shared cold open, canceled first waiter, bounded pinned/quarantine refusal,
  gated eviction/shutdown and changed backing identity.
- [x] Retain the failing controls, then implement the state machine with one
  bound for opening/ready/closing/quarantined owners and owned completion tasks.
- [x] Add request mutation cancellation guards and handle/pending-close pins.
  Quarantine failed closes while retaining their actual handles.
- [x] Factor current authorization checks so cold waits repeat complete fresh
  checks. Gate an actual factory open and revoke/redefine/expire identity while
  it waits; require zero backend invocation. Preserve existing catalog-revision
  and same-revision restoration controls.
- [ ] Qualify actual SQLite full bytes/EOF/backing after repeated eviction and
  sibling provider usability; retain before/after residency and native resource
  observations. Run service ordinary tests and strict Clippy, then independent
  lifecycle/security review.

Five concrete local SQLite pool controls now pass, including ten opens/nine
evictions, request/handle pins, failed publication quarantine and durable MRC4
ineligibility. Their old-owner observation is Weak runtime Arc retirement;
native SQLite connection/thread resource observations and canceled concrete
eviction/shutdown waiters are still unqualified. The checklist item remains
open until those observations and the broader final gates are complete.

The pool now has a separate explicitly enabled timing observer with eight fixed
stages. Controlled cancellation, error and post-unlock publication cases pass;
the warmed lease/handle allocation oracle covers both disabled and enabled
observers. This does not qualify concrete SDK construction cleanup or wire the
pool into the CLI/process fixture. Those remain required below.

## Task 3: CLI immutable plans and lifecycle wiring

**Files:** CLI `runtime.rs`, `remote.rs`, diagnostics and
`tests/configured_remote_compact.rs`.

Preparatory source now provides a resolved CLI plan and cached driver Arc,
explicit provider/Chunked construction ownership hooks, and an SDK per-attempt
journal. Observed SDK composition now retains the actual providers and authority
in the journal, preserves metadata-before-block cleanup and rejects provider
shutdown after failed or ambiguous authority. Ten SDK and fifteen provider/fixture
controls cover failed publication, retained ordinary owners and actual canceled
committed checkout. The SDK and Chunked journal share the full authority barrier.
The retained CLI factory still needs wiring before lazy activation is enabled.
AWS source selection still rereads environment settings, SlateDB pre-return
tasks remain unqualified, and CLI SQLite postconfiguration still needs ownership
registration before its await. The first item remains incomplete until those
seams are resolved; additive startup v2 support alone does not change CLI/process
startup.

- [ ] Add an owned resolved open plan so storage environment references, paths,
  endpoint and volume selection are frozen once at registration.
- [ ] Add a checked `max_active_drives` service setting (default 2,048). Register
  all factories without opening, preserve cache decorators and the common
  context, and emit truthful startup/pool observations.
- [ ] Close both listener/session sets before the owned pool drain, then context
  and cache. Failure must retain the pool owner and report unproven cleanup.
- [ ] Run signed configured CLI QUIC/WebSocket selection, denials, complete
  payload/EOF and clean restart with a small residency limit. Require unopened
  denied Drives and unchanged backing identity across allowed eviction. Run
  touched default/profiled ordinary checks and strict Clippy.

## Task 4: Real-process target wiring and delivery

**Files:** `tests/support/production_target/{backend,process,metrics,mod}.rs`,
relevant owned-runner/verifier controls and documentation.

- [ ] Freeze each backend StoreConfig at registration. Retain every catalog
  registration and inspect every persisted backing receipt without retaining
  every filesystem. Readiness explicitly records registered Drives and opened
  residents; update identity/diagnostic controls without weakening them.
- [ ] Replace timeout-canceled replica shutdown with waiting on the owned pool;
  retain the owner on unproven drain and reject new generations.
- [ ] Run the real ten-process controlled gate with all-server/all-Drive routes,
  live-handle pins, exact data/EOF and before/after resident bounds. This is
  prerequisite evidence, not a substitute for the unchanged full target.
- [ ] Continue the full idle/all-active scale run only when the declared host
  floors are met. Keep backend/native/formal/crash gates explicitly incomplete
  where the actual environment cannot qualify them.
- [ ] Independently join frozen sources, binaries, owned receipts, logs, data
  oracles and resource observations; publish the scoped report, run meaningful
  final gates, commit/push to PR33 and verify actual PR head/body/CI state.
- [ ] Merge only after required checks and the goal's completion audit prove
  the actual requested scope. Preserve draft status and active goal otherwise.
