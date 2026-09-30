# Bounded server Drive runtimes

## Goal and evidence

Keep all catalog Drives available on every server while bounding opened runtime
state. The full qualification target remains ten server processes, 10,000 clients,
10,000 Drives, 5,000 Partitions and 1,000 files per Drive, including mostly idle
and continuously active patterns. The 64 GiB free-disk floor and per-process
24 GiB RSS cap remain unchanged.

At commit `477a5d81`, CLI startup and each process-fixture worker open every
catalog Drive. Post-population worker reopen materializes ten million file-layout
nodes per process and 100 million fleetwide. This source-derived replication is
a scaling cost, not a measured RSS breach. The fixed-hot-prefix measurements
separately identify whole-inode metadata/read amplification; this design does
not change the inode/chunk format or the two durable publication commits.

## Alternatives and choice

1. **Bounded lazy runtime activation:** retain all lightweight definitions and
   open on authorized access. This reduces duplicated resident state while
   retaining any-server routing. Cold latency and safe lifetime management are
   the costs. This is the chosen approach.
2. **Static Drive assignment:** a smaller startup footprint, but it violates the
   existing all-server/all-Drive route qualification and introduces routing and
   failover contracts. It does not satisfy the current goal.
3. **Eager open with a larger host:** retains the replicated state and provider
   connection amplification. It can provide capacity for measurement but does
   not fix this architecture seam.

The user has already authorized continuous performance/scaling implementation,
testing and PR delivery. Routine implementation follows that authorization;
there is no additional permission gate for this design.

## Interfaces and responsibilities

- `mount-rs-service::runtime_pool` owns a bounded set of lifecycle owners. A
  factory opens an owned runtime; a runtime returns its existing `FsDriver`,
  reports sticky failure and whether idle eviction is qualified, exposes its
  immutable backing identity when present, and shuts down its actual resources.
- `DriveDispatcher` retains all registrations. Eager registrations remain
  supported. Lazy registrations resolve through the pool after fresh catalog
  authorization and exact registered-definition validation. No `FsDriver`
  forwarding implementation is introduced.
- A request lease pins the exact runtime generation. Opened session handles
  and pending close tasks retain pins; handle operations clone their pins before
  releasing the session lock. Completed failed closes retain the actual handle
  under the quarantined owner.
- SDK lifecycle observations forward `ChunkedFs::failed()` and the opened
  immutable concurrent backing identity. They do not infer health from shutdown
  success or turn a cached identity into a fresh provider-authority check.
  Failure observation is nonblocking and sticky, including uncertainty published
  before the detailed state lock and observed state poisoning. Healthy state
  mutex contention does not report failure.
- CLI registration prepares immutable resolved construction plans, including
  endpoint/path/volume/environment references and cache decoration. Activation
  reuses these plans and the server-owned `StorageContext`. It does not resolve
  storage environment variables again on each reopen.
- The process fixture uses the same runtime pool, frozen resolved store
  configurations, all definitions and all backing receipts. Startup receipts
  distinguish registered Drives, inspected backing authorities and resident
  runtimes. No static assignment or reduced full-target dimensions are allowed.

## Pool state and admission

Opening, ready, closing and quarantined lifecycle owners consume one common
resident budget. Failed opens keep a nonretryable error record; an owned failed
open must finish partial-resource cleanup before its charge can be released.
Concurrent cold lookups share one owned open. Dropping a waiter never cancels
the open or shutdown task.

Only an idle, unpinned, healthy runtime qualified for persistent reopen may be
selected for least-recently-used eviction. Initial eligibility is restricted to
persistent MRC5 split storage. Memory and unqualified driver owners stay resident.
When all owners are pinned, opening, closing or quarantined, admission rejects
with a bounded error instead of forcibly evicting or exceeding the budget.

The first successful backing identity is retained for the registration's
lifetime. An activation with a changed identity must be quarantined and never
invoked. Runtime generations and pins cannot be confused after eviction.
Hot leases require no added heap allocation; actual provider/RPC allocation
counts remain separately unmeasured until an allocation control observes them.

## Authorization and uncertainty

Initially denied, cross-Partition or stale-definition requests invoke no factory.
After an activation or eviction wait, repeat the full fresh catalog, expiry,
policy, grant, permission and exact-definition checks immediately before backend
invocation. Revision equality alone is insufficient. Preserve current
same-revision denial/restoration behavior and close handles only under the
existing expiry/catalog-failure/revision rules.

Canceling an unfinished mutating request quarantines its runtime before its
request pin is released. Existing provider publication guards remain authoritative
about ambiguous outcomes. Idle eviction checks failure before and after awaited
shutdown. `ChunkedFs::shutdown()` may succeed for an already failed runtime;
that success must never release a quarantine charge or permit automatic reopen.
Failed close or shutdown retains the lifecycle owner, handle where applicable,
capacity charge and error state.

## Terminal ordering

Stop admission and close QUIC and WebSocket listeners/sessions first. Then join
the owned runtime-pool drain, close the shared storage context, and stop the
distributed cache. A timeout stops a waiter only. Unproven drain retains its
owned future/tombstone, reports failure and prohibits another worker generation.
No timeout may drop a filesystem shutdown future and start a replacement owner.

## Observations and qualification

Pool snapshots distinguish registered, resident, opening, ready, closing,
quarantined, pinned, open-success/error, eviction-success/error, wait and
capacity-rejection observations. They contain no credentials, paths or payloads.
Gauges are snapshots, not atomic fleet RSS or physical I/O measurements.

Required controls include zero opens on registration/denial; concurrent cold
singleflight and canceled waiters; revocation/redefinition/expiry while opening;
handle/write/pending-close pins; exact once close through cancellation; sticky
failure despite successful shutdown; failed/canceled shutdown ownership;
unchanged backing and complete bytes/EOF after real SQLite eviction/reopen;
all-Drive/all-server routing with a small resident bound; truthful readiness;
and continued usability of sibling context-backed providers during eviction.

Local controls are distinct from the full production load, independent hosts,
native Linux/macOS mounts, crash/power-loss behavior and symbolic verification.
Those gates remain open until direct evidence qualifies their actual scope.
