# SQLite write timing and runtime-worker control — 2026-09-28

## Result

Increasing Tokio workers did not improve writes in this sequence. The earlier
constant-latency worker-ceiling estimate is insufficient: SQL write and commit
wall times grew substantially with the larger pools. The next experiment should
isolate SQLite journal/filesystem behavior while retaining FULL durability.
These observations do not identify individual fsyncs or a physical SSD limit.

| Runtime workers | Read IOPS | Write IOPS | Mixed IOPS | Two commit calls, ms/write | Connection acquisition, µs/write |
| --- | ---: | ---: | ---: | ---: | ---: |
| 16 | 20,458 | 1,341 | 3,508 | 7.27 | 0.82 |
| 32 | 21,237 | 1,062 | 2,052 | 18.14 | 0.95 |
| 64 | 21,114 | 745 | 1,507 | 53.33 | 1.06 |
| 16 repeat | 20,405 | 1,800 | 3,700 | 4.96 | 1.06 |

Each configuration has one 3-second measured stage per mode, preceded by 1-second
warmup. The repeated 16-worker write result is **34.18% higher** than the initial
16-worker result. Both bracket the slower 32/64-worker observations, but this is
not a confidence interval, isolated instrumentation-overhead measurement or
stable causal estimate of changing worker count. Run order was 16 → 32 → 64 → 16,
serially, with fresh disposable state and the same executable and source closure.

## Where time is observed

For pure writes, the measured `Transaction::commit` wrappers account for
**60–65%** of the overlapping SDK block PUT plus inode-publication wall time.
SQLite PROFILE observes INSERT plus UPDATE at **3.68 / 10.86 / 31.06 / 2.97 ms
per write**, in the same run order. Connection mutex acquisition stays around
**0.8–1.1 µs per write**. These are overlapping wall-time observations; they must
not be added as exclusive CPU time or physical I/O cost. Actual worker occupancy
and off-CPU stacks were not traced.

Every write still has exactly **17 provider SQL statements, two measured durable
transaction commit calls and 1× SDK blob payload**. Process OS write accounting
remains **42.88–43.15×** application payload; pager work is about **7.14–7.18
written pages per write**. Whole selected disk0 driver write counts range from
about **13,158 to 30,326 operations/sec**, including parent audit-log writes and
other host traffic. None of these counters is NAND IOPS or proof of journal/APFS
attribution. Reads retain 1× blob payload and 11 provider SQL statements per ACK.

The actual settings were observed on all 2,000 provider connections at both
boundaries: **DELETE journal, synchronous=FULL (2), normal locking, 5,000 ms busy
timeout, fullfsync=0, checkpoint_fullfsync=0, WAL autocheckpoint=1,000 and
cache_size=-2,000**. All were autocommit at those sampling points. Settings were
read, not assigned by the exporter. No WAL throughput comparison or power-loss
qualification is supplied by these measurements.

## Instrumentation and oracle coverage

The opt-in SQLite callback now uses fixed atomic category counters instead of a
Rust mutex/map. It captures SQLite PROFILE count/time/max/histograms and invalid
or overflowing counters, with no Rust allocation or lock in that callback's
source. This property was reviewed; no allocation trace was run. PROFILE is an
approximate millisecond-resolution VFS wall interval. Notifications can include
unsuccessful SQL execution or cursor finalization and exclude preparation and
post-PROFILE WAL callbacks. Zero means below that resolution.

Monotonic timers separately observe provider connection-lock acquisition and
`Transaction::commit` return for block PUT and the shared MRC5 compact transaction
helper. The latter includes automatic checkpoint and error-path Drop rollback.
The actual provider commit error-counter branch was not forced in this slice;
the failed-COMMIT oracle below exercises raw SQL PROFILE and rollback.

Snapshots suppress their own SQL/profile callbacks, capture pager totals before
configuration queries, and reset after all observer work. End values after the
begin reset are stage totals, not end-minus-begin. Match IDs and require complete
workload boundaries. Sequential snapshots do not prove all background tasks are
quiescent. Non-reset cumulative pager observations can include previous observer
queries.

JSON snapshots allocate and are retained in reports. RSS includes prior exported
snapshots and is not workload-only memory; the expanded diagnostic export has a
visible memory cost. Registered provider `Database` connections are covered;
the service catalog and legacy `SqliteStore` are excluded. Raw Rust/N-API exports
retain new fields, while the current JavaScript transformed `connectionDelta`
projector omits these additions.

Final local gates:

- 107 regular SQLite provider controls and 67 regular saturation controls.
- Three exclusive ignored SQLite controls: actual config/reset/privacy and
  observer exclusion; legacy pruning/counts; a held-reader 40 ms failed COMMIT,
  rollback, later successful commit and fresh-connection exact-byte verification.
  File-backed DELETE/WAL and memory configuration observations are exercised.
- The ignored runtime control verifies real spawned tasks on default/explicit
  16, 32 and 64-worker pools, constructing and dropping each pool serially.
- Four release workload runs, each with full payload and fresh backing verification,
  100 matching MRC5 Drive receipts, storage terminal/gauge reconciliation and
  source/runtime/binary receipts. All 100 active clients' files were verified.
- Two strict scoped Clippy gates, formatting check and fresh release build.

All 14 accepted owned gates finished with child reap, process-group absence,
pipe EOF, unchanged source and removed disposable fixtures. The intended RED
failed because configuration was missing. Two preliminary contention tests used
semicolon/leading-space COMMIT text that the existing first-token classifier
labels OTHER; final oracles use the provider's standalone COMMIT spelling.
All three failed receipts are retained and excluded from qualification. The
production classifier and storage/error/durability behavior remain unchanged.

## Scope and identity

Fixture: 100 all-active QUIC connections, ten independent coordinators **in one
process**, 100 separate Drives, one Partition, one 128 KiB file per client,
32 × 4 KiB extents, random warm reads/writes, depth one and synthetic auth.
This bypasses the production remote-client implementation; peer cache and real
OIDC are unexercised. Ongoing client-session liveness was not independently
probed. This does not qualify 10 servers, 10,000 clients, 10,000 Drives,
5,000 Partitions or ten million files.

The executable was built from the measured owned Rust changes over
`a54845dc6cab48f5c6315ac5f25a0ddd47a4fa5d`; all 505 repository build-closure files
matched across accepted gates and the four runs. Raw receipts have 506 pins,
including the private parent. The JSON report contains hashes and raw aggregate
histograms, process/device identity observations, exact gates and exclusions.
The build closure excludes report/docs files and the private parent.

- Executable SHA256: `117ae788d81286eb70db3c1f9e8cab4e97c5bb9666874193ca58d3b56a5ad596`
- Repository closure SHA256: `82321ac5ddb2532d69b731b3c0e5f1f16ff211fe279b68b3740310ee1d98e4c2`
- [Full report](report.json)
- [Metrics guide](../../bottleneck-metrics.md)

Next: controlled DELETE versus WAL with FULL durability, repeated matched runs,
file-role VFS write/sync timings and extent-count scaling. Keep the original
production runner's 64 GiB free-disk floor; a qualified large host is still
needed for the full target. CI and PR merge remain separate gates.
