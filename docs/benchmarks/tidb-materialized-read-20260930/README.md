# TiDB materialized compact read comparison — 2026-09-30

## Result

Removing JSON encoding and reparsing of freshly decoded complete guards reduced
allocation requests and requested bytes. **This run demonstrates no latency or
throughput improvement.** The original 256-allocation cycle limit still fails.

| Files in directory | Metric | Serialized control | Materialized candidate | Change |
| --- | --- | ---: | ---: | ---: |
| 128 | Mean allocation requests/cycle | 2,200 | 2,163 | −1.68% |
| 128 | Mean requested bytes/cycle | 220,413 | 198,319 | −10.02% |
| 128 | Median cycle latency | 9.621 ms | 10.795 ms | +12.20% |
| 128 | Mean cycle latency | 9.850 ms | 11.316 ms | +14.88% |
| 128 | Nearest-rank p95 latency | 12.509 ms | 15.618 ms | +24.86% |
| 1,000 | Mean allocation requests/cycle | 9,481.19 | 9,292.19 | −1.99% |
| 1,000 | Mean requested bytes/cycle | 847,903 | 672,614 | −20.67% |
| 1,000 | Median cycle latency | 13.162 ms | 13.601 ms | +3.33% |
| 1,000 | Mean cycle latency | 14.664 ms | 14.764 ms | +0.68% |
| 1,000 | Nearest-rank p95 latency | 22.096 ms | 21.025 ms | −4.85% |

Descriptive pooled statistics: 128 timed samples per role and directory size,
spread over four independent process trials. They are not confidence intervals
or a capacity claim. Smaller-directory latency was worse in this observation;
larger-directory latency was mixed. Do not call this a speed improvement.

## What was measured

Actual durable local TiDB 8.5.7, with TiDB providing **both metadata and blocks**.
Default indexed MRC5 enrollment; compact inode updates enabled. This is a single
filesystem/provider cycle, not a server cluster, OS mount, QUIC or RustFS workload.
A target file contains `selected-read`; other seeded file layouts share its
immutable block. Directory order is reversed to exercise order preservation.

Each sample is the unchanged Open → handle Stat → Read → Close cycle. Every
sample checks the target inode and expected bytes at offset zero. Four untimed
warmups precede 32 timed windows for each 128/1,000-file case. Allocation requests
include the current Rust thread's driver/runtime activity; foreign threads and
TiDB allocation are excluded. Requested bytes are not RSS or retained heap.
The instrumented wall-clock latency includes that same cycle and phase counters.
Diagnostic snapshots/deltas, sample storage, reporting, fixture setup, warmup,
oracles and cleanup occur outside both windows.

Eight predeclared balanced trials ran in **ABBABAAB** order. A invokes the old
owned validation → JSON encode → streamed comparison. B invokes the new typed
factory. Both use owned directory names, thin borrowed name keys and the same
complete SQL audit. The core factory is compiled in both binaries but unused by
A. The sole changed production input between variants is TiDB `compact.rs`.
The build manifest pins all 558 tracked Rust/TOML inputs, lockfile and shared
Cargo wrapper inputs, plus the new core contract test. Both release binaries were
copied and hashed, and the candidate source was restored and checked before
execution. Each child had a 300-second external deadline and bounded termination;
all children exited and were reaped without timeout or forced kill. Capture size
limits were checked after execution. Reaping the direct child does not prove
absence of unrelated processes.

## SQL amplification and remaining allocation

All 256 timed samples at each size produced identical observed read counts in
both roles:

| Observed operation / cycle | 128 files | 1,000 files |
| --- | ---: | ---: |
| Inode read statements / returned rows | 7 / 262 | 7 / 2,006 |
| Metadata read statements / returned rows | 1 / 1 | 1 / 1 |
| Block read statements / returned rows | 3 / 3 | 3 / 3 |
| Rollbacks | 1 | 1 |
| Pool checkouts | 8 | 8 |
| Session configurations | 0 | 0 |

These are provider-observed SQL operations and returned rows. They are not TiKV
RocksDB physical IOPS or the complete MySQL protocol statement count. Process-wide
storage counters are observed in one isolated exact test process; every recorded
operation completed successfully with zero errors, cancellations or in-flight
operations. No database row reduction is established by this optimization.

At 1,000 files, the candidate Stat phase averages **8,656 allocation requests**,
93.15% of the cycle's 9,292.19. Stat still performs the fresh complete root and
membership audit. The typed exact-match factory itself performs zero Rust heap
allocations after decoding, as its isolated core controls verify; decoding,
physical validation and surrounding filesystem/provider work remain costly.

The original two controls ran separately, with their exact one-warmup,
one-measured-cycle workload and **256-call threshold unchanged**. Both variants
failed both size gates after fresh oracles and successful cleanup. Candidate
single observations were 2,164 calls at 128 files and 9,296 at 1,000 files. These
single observations are not pooled into the repeated benchmark statistics.

## Correctness and limits

Every case reopens independent providers and performs a fresh complete metadata
read, validates the snapshot, and compares the anchor, root guard and directory
order with the seed. It checks the target inode and expected content bytes. This
oracle does **not** check EOF, equality of every file guard, or every file's entire
payload. All filesystem/provider actors close before exact generated-key deletion
and explicit zero-row counts in all seven owned metadata/block row families.
All 16 paired cases and four original control cases reached these checks. Failed
or uncertain earlier setup/write paths are not replayed and retain their data.

The core differential controls compare the actual prior encoding/streaming path,
including invalid bodies, hint misses, physical identity, fresh anchor drift and
error precedence. Before implementation, the actual old adapter selected ten
controls and failed the three allocation expectations; the optimized factory
passed ten. Three additional parity controls were then added. Current-source
unit, lint and actual TiDB fault-control results are recorded in qualification.
Formal completion, remaining CI performance floors, native RustFS throughput and
10,000-client production capacity are separate outstanding gates.

See [summary.json](summary.json) for exact statistics and phase distributions,
[paired-results.json.gz](paired-results.json.gz) for all retained samples and
execution receipts, and [qualification.json](qualification.json) for source and
validation provenance. The earlier
[directory ownership publication comparison](../tidb-directory-ownership-20260930/README.md)
measures FileCreate and is a separate experiment; its samples are not pooled here.
