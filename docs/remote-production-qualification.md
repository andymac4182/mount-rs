# Remote production qualification

Status: in progress. The active goal covers secure WebSocket fallback, storage diagnosis, real distributed-cache failure tests, and ten-server separate-Drive scaling. WebSocket fallback is implemented, independently reviewed, and accepted. Storage, cache failure qualification, scaling, and final delivery remain in progress.

## Controlled TiDB baseline

Source: merged `cd92c7d4c4a46f5752c2f2af0d6983b6e4a3e39d`, with planning-only commit `c74ca180`. A preserved release executable was built with `resource-profiling` before functional edits.

Host hardware: 51,539,607,552 bytes RAM (48 GiB), 14 logical CPUs, and 14 physical CPUs, read using `sysctl`. Local Docker ARM64 VM: 14 CPUs, 8,318,976,000 bytes memory. The VM shares the host memory budget. One PD, one TiKV, one TiDB, version v8.5.7. This is a single-host diagnostic configuration; it does not establish replicated production capacity or power-loss durability.

Workload: ten server coordinators, ten clients, one separate Drive per client, inode updates enabled, 4 KiB reads and overwrites, depth one, 32 blocks per file, one-second warmup and three-second measured stages. Measurements include request drain. This baseline uses the load fixture authenticator with synthetic bearer tokens and actual per-Drive claim policies; it does not measure OIDC signature validation or production authentication capacity.

| Setup | Outcome |
| --- | --- |
| Virgin Drives, parallel server startup | Nine of ten coordinator initializations failed before I/O, primarily ESTALE. Cleanup succeeded; verification skipped. |
| Identical preprovisioned control | Byte verification and cleanup passed. Read 1,895.21 IOPS; write 680.13 IOPS; zero operation failures. |

The virgin setup failure is reproducible separately from steady-state throughput. The startup implementation can observe another coordinator transitioning metadata from MRC2 to inode mode between its authority inspection and enrollment. Deterministic regressions and an actual datastore rerun are required before accepting a fix.

### Datastore and process profile

The preprovisioned control had 310 TiDB client sessions at both measured stage boundaries. Current construction creates separate pools for every metadata and block role of every Drive on every server. Schema setup also repeats global DDL. Pool ownership and schema initialization need explicit lifetimes before sharing those resources.

| Counter | Read | Write |
| --- | ---: | ---: |
| Successful operations | 5,692 | 2,047 |
| TiDB statements per logical operation | 3 | 10 |
| TiDB CPU milliseconds per operation | 1.448 | 3.117 |
| TiKV VM block write operations per logical operation | 0.0016 | 3.8471 |
| TiKV VM block write bytes per logical operation | 4.32 | 20,692.10 |
| Process RSS at stage boundary | 43,040,768 bytes | 44,089,344 bytes |

The TiKV block counters are Linux VM cgroup I/O, not physical Mac SSD IOPS. Datastore counters include background traffic and observer-boundary overhead. New metric series appeared during the read stage, so coverage is marked incomplete there. These short diagnostic runs identify amplification; they are not stable capacity estimates. Rust allocation counts were not instrumented in this baseline.

Protocol traffic was approximately 4,237 received bytes per 4 KiB read and 4,262 transmitted bytes per 4 KiB write. Inode publication serialized approximately 4,462 bytes per write for a file containing 32 extents. Ordinary independent file updates lock their selected inode, not the global namespace revision.

Raw local artifacts, executable checksum, source identity, observer samples, and compact summary are retained under `/private/tmp/mount-rs-qualification-baseline`. Paired after-fix results must use the same topology, workload, oracle, and timing definitions.

## WebSocket fallback acceptance

Commits `180a4552` and `9c355020` implement verified TLS WebSocket transport with the `mount-rs.v2` subprotocol, bounded binary envelopes, and initial QUIC fallback only when no UDP response has arrived. Cancellation closes an incomplete transaction without replaying an uncertain write. Removed handles retain independently owned close tasks through cancellation and shutdown.

Focused client/service validation passed 103 tests with four ignored integration gates. The wire suite passed 12 tests, including a QUIC handshake that receives a server response and then stalls: it must not contact the WebSocket listener. Signed OIDC filesystem tests passed for QUIC, WebSocket, and fallback, including read-only, revocation, partition isolation, and fresh-store persistence.

The macOS native NFS end-to-end gate passed all three transport tests with `MOUNT_RS_REMOTE_NATIVE_NFS=1`: two Drive mounts, kernel file create/write/sync/read/rename, read-only denial, explicit unmount, and fresh SQLite persistence. The command ran as the ordinary user with approved host filesystem access; a filesystem-sandbox trial failed at mounting. No sudo or user identity change was used. Linux native mounting remains a CI gate.

Independent spec and quality re-review approved the implementation. Strict touched-surface Clippy and formatting passed. These results precede the storage changes; final branch validation must run again after integration.

## Original acceptance checklist

Subsequent sections record the completed bounded gates and their limits.
The ten-server production ramp and final branch delivery gates remain open.

- Deterministic startup regressions, bounded shared-pool lifetime and isolation tests, actual TiDB rerun, and controlled provider comparisons.
- Real authenticated QUIC cache peers with backing counters, peer loss/restart, stale discovery, corruption, disk pressure, and commit-before-placement.
- Ten-server ramps toward 10,000 concurrent clients, 10,000 Drives across 5,000 Partitions, and 1,000 files per Drive with varied read/write patterns, in mostly idle and all-active modes, with byte verification, explicit resource limits, and a signed OIDC authentication mode distinguished from synthetic data-path measurements.
- Independent reviews, touched and full validation, and a tested follow-up PR.

## Production workload target

The requested production workload has 10,000 clients using distinct Drives across 5,000 Partitions, with 1,000 files per Drive. A balanced fixture therefore uses two Drives per Partition and ten million files. Each connection remains bound to one Partition and one client's granted Drive; qualification must check denial of both other Partitions and the ungranted sibling Drive. The user confirmed 990 files of 4 KiB, nine of 128 KiB, and one of 1 MiB per Drive: 62.83 GB of unique logical payload across 10,000 Drives before metadata, logs, and replicas.

The existing single-Partition, one-file-per-Drive baseline does not establish this target. Further runs must declare file counts and sizes, distinguish namespace and payload setup from steady I/O, and exercise random/sequential reads and overwrites, mixed traffic, hot-file skew, append/truncate, and namespace churn. The original catalog baseline limited Partitions to 1,024. The bounded capacity change now admits 5,000 Partitions, 10,000 Drives and 10,000 grants; the accepted catalog shape tests below do not establish full workload capacity.

## Pre-change local provider diagnostic

The preserved NAPI addon at `180a4552` ran the same 400 create/read/unlink lifecycles with 64 workers and unique 4 KiB payloads against split local stores. Every provider completed 400 exact-byte reads and cleanup with zero errors. The ordinary-user host run used existing owned fixtures.

| Provider | Lifecycle IOPS | Measured seconds | Configuration |
| --- | ---: | ---: | --- |
| SQLite | 1,454.03 | 0.825 | Durable local files |
| PGlite | 1,662.54 | 0.722 | Persistent fixture, configured volatile acknowledgment |
| TiDB | 537.91 | 2.231 | Single Docker VM topology |
| FoundationDB | 40.95 | 29.301 | Native single process, 1 GiB limit, persisted single-authority lease |

These are short diagnostic measurements with different durability settings and execution environments, not a fair production ranking. All use legacy leased publication. FoundationDB opening took 0.136 seconds; the long delay was in the lifecycle workload. A prior 30-second bounded process stopped before completion and is retained as incomplete evidence. Controlled inode and steady-existing-file measurements must identify metadata contention and structural costs before attributing the difference.

## Storage implementation checkpoint

Commit `078eface` adds an explicit bounded TiDB pool context shared across a
server's Drives, retryable role schema initialization, and inode-startup
recovery that rechecks exact authority, backing markers, and snapshot validity.
It also forwards all seven inode metadata operations through NAPI and adds
explicit legacy/inode and lifecycle/steady benchmark profiles. The existing
1,000 IOPS legacy lifecycle gate remains unchanged.

Independent review identified two corrections, implemented in `50559c51`:
initiate terminal shutdown for every owned pool even when the caller cancels,
and encode distinguishable per-worker overwrite generations. Both have retained
failing and passing regressions. The live two-identity TiDB cancellation test
and a 256-worker SQLite byte-oracle run pass. Independent rereview accepted
those corrections. Paired performance measurements remain required;
the implementation checkpoint does not establish a throughput improvement.

The formal CI gate checks seven bounded production decisions: exact grant
scope and claims, handle admission, binary lengths before allocation, generic
control charges, readonly flags, read-grant mutation denial, and exact Drive
and revision handles. These proofs do not establish asynchronous cache
persistence, transport behavior, or full-system capacity.

## Paired measurement exposed pool reconnect amplification

The release control rebuilt from `50559c51` passed virgin ten-server startup,
exact-byte verification of all ten files, and cleanup. Its preprovisioned run
failed during read warmup with ten server-side EIO errors after 220 completed
reads; the identical repeat passed verification and cleanup. The first failure
is retained and unexplained. A passing repeat does not close that gap.

The passing runs were substantially slower than the earlier control:

| Trial | Read IOPS | Write IOPS | SQL queries/read | SQL queries/write |
| --- | ---: | ---: | ---: | ---: |
| Before, preprovisioned | 1,895.21 | 680.13 | 3 | 10 |
| `50559c51`, virgin | 322.11 | 162.21 | 30 | 56 |
| `50559c51`, preprovisioned repeat | 47.71 | 44.56 | 30 | 56 |

Stage-boundary client session gauges were zero in both passing new runs.
Source inspection establishes a reconnect amplification bug: the new context
sets pool minimum to zero, while the pinned mysql_async default inactive
connection TTL is zero. Its recycler retains only the minimum in this mode,
so every returned connection is closed. Subsequent checkouts reconnect and
repeat session initialization. This explains the new SQL amplification; it
does not establish the cause of the intermittent EIO. A bounded connection
retention correction, an actual connection-identity reuse regression, and
fresh matched controls are required before performance acceptance.

Compact measured evidence, including the failed trial, is retained in
[the pool churn artifact](benchmarks/remote-production-qualification-20260925/tidb-pool-churn.json).
These short, separated trials do not establish a precise throughput ratio:
the changing host and datastore state still require a fresh alternating
before/after comparison. They do establish that correctness review alone
missed a measurable regression in connection and SQL work.

## Bounded session retention verified

Correction `0015437d` retains lazily opened sessions up to the configured pool
maximum. Independent review accepted the fix. The live connection-identity
regression changed from four different IDs and 27 discarded sessions to one
reused ID and zero discards; bound, reconnect, cancellation, isolation, and
sibling-lifetime gates passed.

Two fresh alternating before/after release pairs used the same preprovisioned
fixture settings. All four passed every exact-byte oracle and cleanup, with
zero operation failures. A final corrected virgin-startup run also passed all
ten fresh file oracles and cleanup.

| Alternating trial | Before read IOPS | After read IOPS | Before write IOPS | After write IOPS |
| --- | ---: | ---: | ---: | ---: |
| 1 | 1,701.57 | 1,683.60 | 521.15 | 610.94 |
| 2 | 1,715.28 | 1,569.53 | 595.56 | 385.45 |

Both corrected controls retained **20 client sessions**, versus **310** in both
before controls. SQL work returned to **3 queries/read and 10 queries/write**.
The connection-count reduction and removed reconnect work are verified;
these short trials do not establish a steady throughput gain. The earlier
warmup EIO remains unexplained, although it did not recur in these five runs.
The exact failed UTC window yielded no TiDB container log entries.

[The session-retention artifact](benchmarks/remote-production-qualification-20260925/tidb-pool-retention.json)
retains source identity, configuration, operation counts, session gauges,
process resources, and all five control outcomes. This one-file, ten-client
fixture does not establish the 10,000-client production target.

## Distributed cache failure qualification

The cache suite passed 33 unit tests and six new integration tests using real
authenticated QUIC peers. It covers cold/warm reads, bounded disk eviction,
corruption fallback, unavailable peers, peer restart with disk retention, scope
isolation, cancellation cleanup, and successful/failed backing-store commits.
A fresh undecorated SQLite SDK reopened acknowledged data after cache shutdown.
The two live Redis directory tests also passed. Independent review accepted the
final tests at `97cef0ad`.

The 100-reader cold-miss test holds the actual peer response until every reader
has reached the pending state. Disabling the production singleflight lock in a
temporary mutation caused 100 peer requests instead of one before release; the
restored implementation passed. This distinguishes coalescing from warm hits.

In the small repeated-read fixture, 125 logical reads required two backing GETs
(56 bytes), compared with 125 GETs (3,500 bytes) without caching. This is a
98.4% request reduction for that fixture, not a prediction for uniformly unique
production reads. The flush barrier proves ordering, not power-loss durability.
The corruption test does not simulate operating-system ENOSPC. Redis and QUIC
failure coverage are separate; the combined cache fallback test uses fixed
discovery with an unavailable holder. Ten-server cache capacity remains pending.

## Local provider layout diagnostics

A fresh public NAPI comparison uses 64 workers, 400 iterations, unique 4 KiB
payloads, and full-byte read verification. Lifecycle includes create, read and
unlink; steady overwrites precreated files and reads them, with preparation and
cleanup excluded. These are different operation mixes and logical operation
counts. They do not measure physical SSD IOPS or QUIC throughput.

| Provider | Legacy lifecycle IOPS | Inode lifecycle IOPS | Legacy steady IOPS | Inode steady IOPS |
| --- | ---: | ---: | ---: | ---: |
| SQLite | 820.80 | 495.35 | 1,014.36 | 1,116.77 |
| PGlite (volatile) | 1,381.22 | 19.91 | 1,092.35 | 280.47 |
| TiDB (single node) | 477.64 | 23.96 | 334.39 | 119.52 |
| FoundationDB (single node) | 41.58 | Failed opening | 30.41 | Failed opening |

All completed cases verified 400 reads with zero operation errors and passed
cleanup. FoundationDB inode mode failed with ESTALE before any oracle operation
when metadata and blocks used separate key prefixes. Its inode authority check
currently reads a block marker under the metadata prefix; same-prefix tests do
not qualify this split-store arrangement. The failed case remains a contract
gap to resolve, including external blob backends.

PGlite inode methods use untyped SQL calls that prepare statements, and structural
publication rewrites all inode guards with individual INSERTs inside a serialized
transaction. TiDB also rewrites structural guards. These are concrete source
amplification candidates; the observed timings alone do not attribute all the
slowdown to either mechanism. No deadlock was established: progress artifacts
are persisted only at phase boundaries and all PGlite/TiDB iterations completed.

These short single trials have different durability settings and are diagnostic
comparisons, not a production ranking or a measured regression ratio.

[The provider layout artifact](benchmarks/remote-production-qualification-20260925/local-providers-after.json) retains all four configurations, successful counts and both failed FoundationDB cases.

A separate one-file native FoundationDB control confirms the prefix boundary:
same-prefix inode create/read/full-byte verification/unlink/shutdown passed;
otherwise-identical separate-prefix construction failed with the same ESTALE
before returning a driver. No authority marker was copied or altered.
[The prefix control artifact](benchmarks/remote-production-qualification-20260925/fdb-inode-prefix-control.json) retains both outcomes.

A small PostgreSQL wire probe verified eight iterations in each of eight
layout/workload/concurrency combinations. At one worker, inode steady work
produced 112 Parse, 80 Close and 272 Sync messages, versus 24 Parse, zero Close
and 24 Sync for legacy. Inode lifecycle produced 320 Parse, 240 Close and 800
Sync, versus 32 Parse, zero Close and 32 Sync. This confirms extra statement
protocol work; it does not attribute every slowdown or establish a throughput
gain. The proxy recorded message types only and affects timing.
[The wire-count artifact](benchmarks/remote-production-qualification-20260925/pglite-wire-control.json) retains all cases and failed diagnostic setup limitations.

## Production harness checkpoints

The balanced catalog accepts the requested 5,000 Partitions and 10,000 Drives
and grants within the unchanged 8 MiB document bound. Independent checkpoint
review accepted the count, resource and mixed-file oracle changes. These are
model and catalog gates, not a populated ten-million-file capacity result.

A release profile of 40 authoritative catalog reads at the target shape read
90,850,280 document bytes (2,271,257 per call), taking 28.711 ms wall and
27.656 ms process CPU. The ten-client shape took 2.182 ms wall with 2,222 bytes
per call. A separate debug allocation profile recorded about 14.725 Rust
allocations and 2,170.1 allocated bytes per call; SQLite's foreign heap is
excluded and recorded separately. No zero-allocation claim is supported.

A ten-listener signed-OIDC smoke passed ten own-Drive full-byte reads, ten
ungranted sibling denials and ten other-Partition authentication denials.
Negative probes require the exact authentication application close, so network
failures cannot count as denials. All ten legitimate connections remain held
through the sequential work. Independent review accepted this checkpoint.
The smoke uses shared volatile MemoryFs and an owned static JWK source; it
does not test discovery, TiDB population, simultaneous I/O or production capacity.

The catalog profile warms one read across an eight-connection pool. Initial
measured SQLite pager-counter drains can therefore include earlier setup and
CAS activity on other connections. Pager-write units do not establish writes
by the forty-read workload. Raw artifacts are retained; document-byte units
and process resource deltas have their stated measurement window. A future
matched warm profile must drain every connection and report cold setup separately.

### FoundationDB split-store correction and diagnostic performance

Commit `dba9629a` binds the block-authority policy atomically at first metadata
binding. Same-keyspace stores retain transactional marker verification; split
and external stores verify the actual configured block provider at existing
open/publication boundaries. External verification and metadata publication are
not one atomic cross-store transaction. Missing policy remains conservative.
The actual native SDK and public Node tests pass same-prefix, split-prefix and
external RustFS cases; canonical Cloudflare R2 remains runtime-unqualified.
Independent review accepted this bounded fix with no material code findings;
that approval does not extend to full production capacity or canonical R2.

The corrected split-prefix public Node provider completed both 400-iteration,
64-worker, 4KiB runs with 400 complete-byte checks, zero errors and namespace
cleanup/driver shutdown. Steady overwrite/read measured 70.49 logical operations
per second; create/read/delete measured 45.87. These are single short native
single-node diagnostic trials. Earlier split-prefix inode trials failed before
opening, so no before/after throughput improvement ratio is available. Neither
logical operation rate establishes physical SSD IOPS or production capacity.
Raw results are retained in `fdb-split-inode-after.json`.

### PGlite typed inode queries: bounded improvement and remaining work

Independent review accepted `c24e1513`: fifteen SQL calls now carry explicit
TEXT/INT8 parameter types; SQL text, locking, transactions, authority guards and
uncertain-outcome behavior are unchanged. Both eight-case wire experiments
passed all 64 complete-byte checks. Statement closes drop to zero; inode
lifecycle Sync counts fall from 800 to 320 and steady counts from 272 to 112
at one worker. Parse/SQL execution and the structural whole-guard rewrite remain.
Legacy message counts are unchanged.

Two alternating public Node 400-iteration/64-worker pairs per inode workload
completed all 3,200 complete-byte checks, with zero errors and cleanup:

| Workload | Before trial 1 | After trial 1 | Before trial 2 | After trial 2 |
| --- | ---: | ---: | ---: | ---: |
| Create/read/delete logical ops/s | 21.54 | 25.68 | 21.65 | 25.66 |
| Steady overwrite/read logical ops/s | 302.07 | 348.91 | 261.75 | 360.76 |

These are short configured-volatile PGlite diagnostics. They show a modest
repeatable lifecycle improvement and variable steady gains, not production
capacity or acceptance of the existing 1,000 lifecycle IOPS floor.
The recorded server-process disk-write byte deltas during whole lifecycle-run
windows were 138.2 MB (after trial 1), 139.9 MB (before trial 2) and 145.9 MB
(after trial 2). Those windows include setup, cleanup and background work;
they do not isolate physical SSD amplification. Short steady windows recorded
0 or 19.0 MB, so a zero delta cannot establish zero writes or durable publication.

The same-process kernel observer validates process identity and counter
monotonicity. Host `iostat` intervals exclude the initial since-boot sample,
but include all host traffic; the virtual FoundationDB image is not summed
with the physical device. Full results and explicit counter-window limits are
retained in `pglite-typed-paired.json` and both raw wire JSON artifacts.
No allocator or server CPU profile was performed in these paired runs.

### Actual TiDB population checkpoint: partial oracle and amplification

The first actual checkpoint used ten signed-OIDC clients, ten QUIC listeners,
ten TiDB Drives across five Partitions and 1,000 files per Drive. Each Drive
contains 990 × 4KiB, 9 × 128KiB and 1 × 1MiB files. The debug/resource-profiling
binary and frozen source patch are identified in
`task4-ten-drive-population-before.json`; this is one process/runtime and the
existing single-node diagnostic TiDB VM, not production capacity.

| Phase | Actual result | Wall seconds | TiDB executor statements |
| --- | --- | ---: | ---: |
| Namespace creation, ten workers | 10,000 acknowledged files | 546.219 | 5,245,000 |
| Payload population, ten workers | 10,000 acknowledged full writes / 62,832,640 bytes | 231.005 | 165,340 |
| Populated replica refresh | 100 independently reopened filesystems | 8.714 | Retained by stage |
| Fresh backend byte oracle, serial | 7,076 files / 44,294,144 bytes verified | Stopped at 600-second phase budget | Retained by stage |

**The checkpoint is unqualified.** No byte mismatch was reported, but the
oracle stopped before verifying all 10,000 files. The subsequent 100 server/Drive
routing probes were not reached. Ninety bounded protocol read/EOF requests
completed before the full oracle. Budgets are checked between operations;
protocol requests have a 30-second deadline. The 1,800-second work-body budget
does not bound preceding setup or synchronous observer commands.

Namespace creation published 5,015,000 node units, serialized 1,847,507,460
namespace bytes plus 1,811,255,630 inode bytes, and returned 5,426,533,270 inode
bytes. The phase recorded approximately 524.5 SQL statements per created file.
Primary client QUIC traffic was 4,464,336 transmitted and 3,327,861 received UDP
bytes, with zero reported packet loss. Async provider elapsed spans overlap
across workers and must not be summed as CPU time.

Payload writes serialized only 5,251,090 inode bytes and no namespace bodies,
but returned 4,262,086,520 inode bytes across 10,030,000 node-row units. Opening
each existing file currently invokes a complete inode snapshot and namespace
copy. This measured read amplification is separate from per-node structural
INSERT exchanges. The next fixes must preserve selected-file freshness and
structural authority while measuring these costs separately.

The process sampler recorded a 260,063,232-byte resident peak with no capture
errors; Rust allocations were not instrumented in this run. SQL counter-series
coverage is incomplete and no counter reset was recorded. TiKV namespace-stage
cgroup counters recorded 206,106 writes / 11,337,207,808 bytes in the VM; these
are not physical SSD IOPS. Whole-window host disk0 samples averaged 16,130.6
transfers/s and peaked at 53,330, including all host traffic. They cannot be
attributed to this datastore or combined with the virtual FoundationDB disk.

Independent source review found three cleanup defects: missing explicit shared
context closure, transition shutdown abandoning later replicas after an error,
and unbounded final listener cleanup. The original `cleanup_errors=0` therefore
does not prove complete drain. This first attempt, its exact source, logs and
all observer windows are retained under a distinct run ID; corrected attempts
must use new artifacts and receive a fresh review.

The harness correction in `b92d8280` passed a fresh bounded source review.
Cleanup borrows the replica vector, attempts all shutdowns with concurrent
deadlines, explicitly closes all shared contexts, and retains unresolved
listener tasks through terminal evidence. Scope-probe attempts are counted
before awaiting transport. Ten focused checkpoint tests and sixteen broader
production-scale tests passed (overlapping suites), with touched formatting and
strict Clippy. The cancellation test now counts actual future polling. The
initial RED was a missing-helper compilation failure, not behavioral proof of
the original lifecycle defect. This source acceptance does not qualify the
failed baseline or replace a corrected live run.

## Conditional inode read and path refresh

The frozen candidate adds an inode-specific conditional snapshot API and
forwards it through all four metadata providers, SDK erased stores and NAPI.
An unchanged result requires a fresh check of the exact MRC4 authority and
positive structural generation. Selected inode versions still require checks.
Existing nontruncating opens use the shared namespace view; structural
publication, directory enumeration and aggregate statistics retain full
snapshot refresh. Retry chains are bounded, changed structural identity forces
path resolution again, and selected metadata feeds file statistics.

The behavioral regression uses 128 existing files and twelve complete byte and
EOF oracles. Before the change, the correct reads required 36 full snapshots,
4,644 returned snapshot nodes, 24 namespace clones and 3,096 cloned nodes.
The frozen candidate passes those same oracles with zero repeated full
snapshots or namespace clones, 108 selected checks and three selected bodies.
Setup and the initial coherent load are outside this measured window. These
counts do not establish zero total allocations or constant directory lookup
CPU: the existing directory entry lookup remains linear.

Conditional hits trust correctly advanced generation and inode revision
versions. They do not audit payload tampering that preserves those tokens or
untouched corrupt records. Full snapshots and reopen retain full validation.
Actual block backing verification remains at the existing open/publication
boundaries; this change adds no metadata-local marker requirement for an
external block store.

Candidate source freeze SHA256:
`9ed07e6b4c69e1bcf3c75dcc5f8e5e1402410985a092c7b4aaa9db178ce1705a`.
Final provider, filesystem, SDK and NAPI checks and strict touched all-target
Clippy passed. Independent specification and quality review passed for the frozen source.
The matched ten-Drive measurement follows below. The earlier incomplete serial oracle
cannot supply a whole-oracle speedup denominator, and structural create/write
amplification remains a separate measured issue.


## Matched ten-Drive checkpoint after conditional refresh

Committed source `0107577f` passed the same debug/resource-profiling checkpoint:
ten actual signed clients, ten QUIC listeners, ten Drives across five
Partitions, and 1,000 mixed files per Drive. All 10,000 files / 62,832,640 bytes
passed the fresh backing byte oracle, all 100 server/Drive routes passed, and
all ten sibling-Drive and ten other-Partition denials passed. No uncertain
mutations, observer errors, cleanup errors or unresolved listener drains were
recorded. The corrected cleanup completed in 0.586 seconds. The original
failed attempt's cleanup counter remains insufficient evidence of full drain.

| Matched workload measure | Before | After |
|---|---:|---:|
| Namespace creations acknowledged | 10,000 | 10,000 |
| Namespace phase seconds | 546.219 | 554.318 |
| Payload writes acknowledged | 10,000 | 10,000 |
| Payload bytes acknowledged | 62,832,640 | 62,832,640 |
| Payload phase seconds | 231.005 | 29.854 |
| Payload returned inode node units | 10,030,000 | 20,000 |
| Payload returned inode bytes | 4,262,086,520 | 6,577,920 |
| Payload selected inode serialized bytes | 5,251,090 | 5,251,090 |
| Payload executor SQL statements | 165,340 | 145,340 |
| Payload main-process CPU seconds, user + system | 338.691 | 36.074 |
| Payload TiKV process CPU seconds | 258.000 | 46.000 |
| Payload RocksDB get-read bytes | 1,760,136,262 | 10,097,716 |
| Payload TiKV VM network transmit bytes | 8,128,468,089 | 388,626,637 |
| Payload TiKV VM cgroup write operations | 74,077 | 56,214 |
| Payload TiKV VM cgroup write bytes | 1,101,287,424 | 271,515,648 |
| Fresh serial byte oracle | 7,076 files at 600-second stop | 10,000 files in 180.982 seconds |

Payload elapsed time fell by about 87.1% in this single pair (231.005 to
29.854 seconds). The unfinished baseline cannot provide a whole-oracle speedup
ratio. Namespace creation is essentially unchanged, retaining its measured
full-replacement SQL and metadata amplification. This is a bounded point,
not a distribution of repeated throughput measurements or full production
capacity acceptance.

The process sampler captured 7,700 samples and a 261,128,192-byte resident
peak, with no capture failures. Allocation instrumentation was disabled;
allocation fields are null, not zero. The 100 populated namespace replicas
still contribute resident memory. SQL series coverage is incomplete, with no
recorded resets. Component CPU, engine, VM network and cgroup counters include
background traffic and different elapsed windows. They are not physical SSD
IOPS or proof that every changed byte arises from this code path. Host disk0
whole-window samples averaged 14,155.6 transfers/s and peaked at 26,538;
these include all host traffic and cannot be attributed to TiDB.

[Retained after artifact](benchmarks/remote-production-qualification-20260925/task4-ten-drive-population-after.json)
contains the terminal journal, ten datastore stage summaries, source/binary
identity and raw hashes. The exclusive raw directory also retains the exact
executable, source patch, helper, logs and observer windows. Before and after
use the same ten-worker depth-one population, serial oracle and 600-second
phase limits; corrected cleanup is an explicit second harness difference.
This remains one process on loopback with an 8.32 GB Docker TiDB fixture below
the 10 GiB qualification floor. The 10,000-client / 10,000-Drive /
5,000-Partition / 10-million-file target, workload patterns, cache capacity,
independent server processes, final platform/formal gates, current CI and PR
merge remain pending.

### Structural batching packet-limit prerequisite

Two actual TiDB 8.5.7 prerequisite tests passed before changing the singleton
insert loop. Fresh, reused and reconnected sessions retained the expected
packet cap under the provider's no-reset policy. A 65,537-byte parameter
succeeded with default client options and failed with the exact outbound
codec error under a 65,536-byte client cap. The negative control accepts
neither an arbitrary I/O error nor a large inbound result as evidence.

Both private and shared provider paths accepted a 131-node namespace with a
930-byte UTF-8 key under that client cap. Fresh reopen verified the exact
namespace, generation and guard revisions, plus all 4,096 persistent block
bytes. Exact-key cleanup passed. Touched package formatting and strict
integration-test Clippy passed. No production source changed in this step.

The pinned server/driver source and session observations support taking the
minimum of the transaction's session cap, any configured client cap and a
private batching budget, with full encoded-command sizing. No GLOBAL setting
was changed; a low server cap and manual reset/change-user were not tested.
This is a prerequisite, not proof of batching, rollback, throughput or
physical I/O improvement. The initial incorrect error-variant assertions and
their correction are retained as test-harness failures.

[Prerequisite artifact](benchmarks/remote-production-qualification-20260925/structural-packet-cap-prerequisite.json)
records source identity, observations and hashes of the retained raw evidence.

### Bounded structural INSERT batching correctness

The frozen TiDB and PGlite implementations replace singleton guard inserts
with at most 64 rows per statement under a conservative 256 KiB encoded
command budget. TiDB also respects the transaction's session and configured
client packet caps. Oversized rows select the existing singleton statement
before sending. Locks, complete membership/version checks, full replacement,
root publication and one final commit remain; a sent batch is never replayed
as smaller writes.

Actual behavioral regressions passed the complete namespace/revision/block
oracle before failing at 131 singleton INSERTs. The changed implementation
uses `[64, 64, 3]` in enrollment and two structural windows on both providers,
with the same persisted result. Row/byte boundaries, UTF-8 keys, oversized
singleton placement, late overflow, rollback, clean cancellation/reuse,
stale selected-write conflict and multi-batch lost-commit-ack controls passed
their scoped assertions. Independent public SDK reopen tests verified 130
files / 532,480 bytes and 130 EOF checks per provider, followed by public
unlinks, context closure and zero residual owned rows. Touched formatting and
strict Clippy passed. Independent source review found no material defect.
The supplemental actual TiDB ambiguity controls (2), proxy/pure controls (3),
concurrent inode cases (13), CAS cases (44) and delegated cases (9) passed
on the unchanged frozen source. One inode test remained ignored. Additional
design interleavings remain unqualified; these counts
do not imply complete execution of the planned matrix.

**PGlite SQL-error same-client recovery failed.** A real duplicate-key error
on batch two preserves old state on fresh reopen, but the original connection
cannot perform its next read. The preexisting typed singleton path reproduces
the failure directly and through the observer, with two `ReadyForQuery(E)`
responses to one exchange. Passing diagnostic assertions establish this
failure; they do not qualify reuse. Clean cancellation/reuse is a separate
passing case. A pristine upstream server control and a separately reviewed
protocol-server remedy remain open; this patch introduces no workaround.

[Frozen correctness artifact](benchmarks/remote-production-qualification-20260925/structural-bulk-correctness.json)
retains exact source/raw hashes and the failed gate. Fewer INSERT executions
do not establish lower physical SSD IOPS, throughput, zero allocations or
full production capacity. The full guard-row and root costs remain.


### Matched structural batching measurement

Attempt C at `d502e51c` completed the same ten-server, ten-Drive/five-Partition
checkpoint as accepted B: 10,000 online creations, 62,832,640 acknowledged
payload bytes, all 10,000 files verified through a fresh backend, 100 routes,
scoped denials, and zero cleanup errors or unresolved listener drains.

| Namespace population measure | B: singleton guards | C: bounded guard batches |
| --- | ---: | ---: |
| Elapsed seconds | 554.32 | 288.76 |
| TiDB SQL executions | 5,245,000 | 323,350 |
| Main-process CPU seconds | 865.62 | 631.27 |
| TiDB CPU seconds | 652.50 | 223.16 |
| TiKV CPU seconds | 515.00 | 470.00 |
| Namespace peak RSS bytes | 144,195,584 | 242,368,512 |
| Returned guard rows | 15,025,000 | 15,025,000 |
| Returned guard bytes | 5,426,533,270 | 5,426,533,270 |
| TiKV transmitted bytes | 14,536,741,730 | 14,644,030,885 |
| TiKV VM write operations | 216,071 | 179,668 |
| TiKV VM written bytes | 12,153,192,448 | 11,956,387,840 |
| TiKV VM read operations | 17,239 | 55,850 |
| TiKV VM read bytes | 848,674,816 | 2,095,304,704 |

Creation elapsed time decreased 47.9% and SQL executions decreased 93.8%
in this pair. Whole-root/guard serialization and returned rows are unchanged;
network bytes and VM written bytes barely changed. Namespace RSS increased
68.1%; whole-run RSS increased from 261,128,192 to 341,671,936 bytes. Rust
allocation profiling was disabled, so allocation counts remain unknown.
Payload population took 29.85 versus 30.51 seconds. These observations support
SQL execution savings and expose the remaining full structural rewrite costs.

The VM read increase is retained rather than attributed to batching without
controlled engine-cache/background experiments. The single pair uses debug
binaries, one process/loopback and a TiDB VM below the 10 GiB qualification
floor. Host and VM counters are not physical, datastore-attributed SSD IOPS;
no production capacity or variance-controlled throughput claim follows.

[Matched C artifact](benchmarks/remote-production-qualification-20260925/task4-ten-drive-population-bulk.json)
retains the executable/source identity, 43 raw hashes, ten datastore windows,
closed host observer, value/cleanup results and exact B/C arithmetic.
Independent evidence review passed for this bounded point and comparison.
