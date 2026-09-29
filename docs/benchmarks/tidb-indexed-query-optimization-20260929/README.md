# TiDB indexed query optimization benchmark — 2026-09-29

**Results are workload dependent.** With 1,000 files per Drive, SDK sequential-read cycles improve **2.58×** and random-read cycles **1.79×**. At 100 concurrent QUIC clients, read throughput regresses in both orders: **−30.3%** in the first pair and **−10.5%** in the reversed pair. Their descriptive weighted aggregate is **−21.0% reads, +5.8% writes and +7.5% mixed**. All ten runs pass correctness, shutdown and cleanup checks.

The repeated high-concurrency read loss coincides with TiDB HashJoin execution and increased server allocations. It remains a performance issue. This benchmark qualifies the measurements and their data checks; it does not qualify a general speed improvement or production capacity.

## Compared code and fixture

- Indexed baseline: `2dae7d413c1b20bb694698b50387ef3909000766`.
- Tuned query candidate: `548e7644d3677733576b775ccda8e450207723ee`.
- Candidate binds volume keys explicitly for point access, forces the existing exact-name directory index, and combines fresh authority/membership reads after the selected write guard lock. The stored representation and wire encoding are unchanged by this SQL follow-up.
- Both builds use identical committed harness/support/Cargo.lock bytes, Rust 1.95.0, release mode, one build job and allocation/resource profiling. Every run binds clean source, compiled revision, executable, observer and configuration hashes; builds finish before timing.
- Actual retained TiDB v8.5.7: one TiDB, three PD and three TiKV containers; RustFS 1.0.0 with durable blocks and persistent local storage. Docker VM: 14 CPUs and 16,745,295,872 B RAM. All eight container identities, images, resources and restart/OOM state remain stable across both suites.
- RustFS has a 2 GiB live memory limit. The older ready receipt declares 1 GiB; that recorded drift is retained explicitly, and the live limit is identical in every measured arm.

The [earlier indexed-layout comparison](../tidb-indexed-metadata-20260929/README.md) compares different revisions and executions. Its historical numbers remain unchanged.

## Workloads and units

**Wire:** ten service instances in one process and one shared Partition, with a separate Drive per active client. Ten or 100 clients, depth one, preopened handles, one file per client with 32 hot 4 KiB blocks, nominal five-second Read/Write/Mixed stages plus drain, 16 Tokio workers, binary QUIC v2 and audit logging. Authentication is synthetic. These are total cluster logical operations/s.

**SDK:** two Drives with five independent SDK/filesystem/pool contexts each, 1,000 files per Drive. Each pattern completes 1,000 cycles: Open, one acknowledged 4 KiB overwrite or verified Read plus EOF, then Close. Five patterns, 100 iterations per lane, one pair. SDK cycle rates include work that a preopened wire operation excludes.

Caches use the harness defaults. Read payloads can hit warm local SDK caches; all-file warmness and cache hit counts are unavailable. No peer cache, production remote-client mount, external OIDC or NAPI path is exercised. Setup, observer work, fresh oracles and cleanup are outside stage throughput. The maximum measured cohort is 100 active clients.

## Wire100: both pairs and descriptive aggregate

The original suite runs baseline then candidate. A separate fresh pair runs candidate then baseline after the read regression is found. This gives a descriptive A–B–B–A sequence with an intervening review gap. Equal source/fixture/resource identities do not establish equal cache, thermal or background state. Two runs per arm do not establish statistical significance or causality.

### Original pair

| Pattern | Baseline ops/s | Candidate ops/s | Change |
|---|---:|---:|---:|
| Read | 2,526.45 | 1,761.65 | -30.3% |
| Write | 212.62 | 203.58 | -4.3% |
| Mixed | 340.76 | 389.02 | +14.2% |

### Reversed pair

The candidate runs first in this pair; the columns retain baseline/candidate order for comparison.

| Pattern | Baseline ops/s | Candidate ops/s | Change |
|---|---:|---:|---:|
| Read | 2,237.65 | 2,002.92 | -10.5% |
| Write | 186.29 | 218.50 | +17.3% |
| Mixed | 343.75 | 347.00 | +0.9% |

### Four-run weighted aggregate

Throughput is summed acknowledgements divided by summed drain-inclusive elapsed. Counter ratios use summed raw counters divided by summed successes. Percentage changes are recomputed from those aggregates. Individual run ranges are retained.

| Pattern | Baseline ops/s (range) | Candidate ops/s (range) | Change |
|---|---:|---:|---:|
| Read | 2,381.99 (2,237.65–2,526.45) | 1,881.85 (1,761.65–2,002.92) | -21.0% |
| Write | 199.42 (186.29–212.62) | 211.07 (203.58–218.50) | +5.8% |
| Mixed | 342.21 (340.76–343.75) | 367.86 (347.00–389.02) | +7.5% |

Read p95 histogram upper bounds are 131.072 ms in all four runs. Write bounds are 1,048.576 ms for baseline and 524.288–1,048.576 ms for candidate. Mixed read bounds are 65.536 ms; mixed write bounds are 1,048.576–2,097.152 ms for baseline and 1,048.576 ms for candidate. These are per-run upper-bound ranges, with no pooled percentile estimate.

| Pattern | Rust allocations/op baseline → candidate | Allocated B/op | Process CPU µs/op | Selected data SQL calls/op |
|---|---:|---:|---:|---:|
| Read | 340.15 → 344.66 | 63,156 → 63,667 | 532.58 → 551.01 | 2.00 → 2.00 |
| Write | 922.16 → 904.18 | 235,718 → 232,001 | 1490.34 → 1431.86 | 5.00 → 4.00 |
| Mixed | 627.19 → 621.07 | 149,503 → 147,981 | 1000.87 → 1001.59 | 3.50 → 3.00 |

Rust counts cover the measured process pipeline and background, with foreign allocations excluded. Canonical unchanged metadata comparison has its separate zero-allocation correctness test; complete read/write paths still allocate. RSS endpoints and lifetime peaks are gauges rather than per-operation allocation counts or stage peaks.

## Read regression: evidence and remaining cause

In the first pair, inclusive inode-SQL time rises **39.122 → 55.874 ms/read**. Pool checkout is only **11.96 → 49.91 µs/read**; filesystem gate and service handle-lock waits remain below one microsecond. Catalog queue wait is separately **15.60 → 27.11 µs**. Across all four runs, inode-SQL spans are **41.499 → 52.334 ms/read**.

The exact retained executor counter `tidb_executor_expensive_total` shows four `HashJoinExec` events per candidate read in both runs. Baseline has two `IndexLookUpJoin` and two `MergeJoinExec` events/read. Prepared-cache misses are 240 → 9 in the original pair and 3 → 12 in the reversed pair, in baseline/candidate order. All four read captures have more than 99% prepared-cache hits. These counts do not establish compile time or a causal explanation for the regression.

| Read capture | Successes | Observer s | TiDB Go allocated B/success | Go allocation objects/success | GC cycles/window |
|---|---:|---:|---:|---:|---:|
| wire100-baseline | 12,679 | 6.033 | 291,185 | 2,997 | 12 |
| wire100-current | 8,957 | 6.366 | 634,365 | 7,125 | 26 |
| wire100-current-reverse | 10,111 | 6.142 | 634,336 | 7,124 | 30 |
| wire100-baseline-reverse | 11,239 | 6.455 | 263,698 | 2,803 | 9 |

These are whole TiDB process counters including background work, normalized by completed workload reads. They are not allocations attributed to individual queries. Exact raw before/after/delta totals, units, windows, seven-source identity/series coverage and zero resets are retained in [tidb-executors.json](tidb-executors.json). Event counts establish no GC pause time or executor duration; the observer captured no corresponding duration sums/buckets. The evidence makes TiDB join/executor overhead a leading suspect; it does not prove lock contention or CPU saturation.

## Write profile and amplification

Each preopened write now uses **four selected data SQL calls instead of five**; SDK overwrite cycles use **five instead of six**. Session queries and transaction BEGIN/COMMIT are separate counters. Actual TiDB controls also confirm one fewer traced statement in the combined write path; unknown COMMIT outcomes are never replayed.

In the four-run Wire100 aggregate, blob PUT spans are **421.38 → 394.45 ms/write**, while metadata publication is **23.59 → 22.77 ms/write**. SDK candidate PUT HTTP dispatch closely matches its 39 ms blob-PUT span, with 1,000 successful upload responses and zero transport errors per overwrite stage. This localizes the largest observed write wait to the upload request/response seam. RustFS server storage/disk timing is unmeasured.

Inclusive spans overlap: COMMIT is inside publication, marker GETs are inside backing verification, and HTTP dispatch is inside PUT. They must not be added as exclusive CPU or SQL-lock time.

Wire10 read TiDB→TiKV request totals remain near 6.009/op; Wire100 totals also stay near six. Cop shifts from about two/read to near zero, while Get shifts from four to six. TiKV storage-command totals exclude coprocessor work, so higher storage-command/Get-byte counters do not establish extra physical I/O. The exact client/server RPC classes and separate storage/coprocessor engine counters are retained in the two request-type reports.

Known returned wire metadata remains about **9,576 B (9.35 KiB) for each 4 KiB payload**, roughly **2.34×**. These are encoded provider buffers, excluding SQL protocol bytes. This follow-up reduces selected query work; it does not materially shrink those buffers.

## SDK1000 full-cycle results

| Pattern | Baseline cycles/s | Candidate cycles/s | Change |
|---|---:|---:|---:|
| Sequential read | 186.55 | 480.85 | +157.8% |
| Random read | 306.50 | 548.94 | +79.1% |
| Sequential overwrite | 122.95 | 145.17 | +18.1% |
| Random overwrite | 143.32 | 154.95 | +8.1% |
| Mixed | 243.74 | 246.55 | +1.2% |

Sequential-read p95 falls 65.475 → 30.838 ms and random-read p95 52.763 → 27.201 ms. Mixed p95 worsens 71.130 → 74.392 ms (+4.6%). Read Rust allocation counts rise about 1.1%; overwrite counts fall about 0.9%. Known metadata bytes change by less than 0.3%. The large read gains are observed in one pair; no repeatability or isolated causal claim follows.

Creating the 2,000-file dataset takes **194.623 → 191.949 seconds**, essentially unchanged. Both variants verify all 2,000 files from fresh contexts, every stored byte, EOF and exact root membership, then shut down and remove the complete owned scope.

## Wire10 ABBA

| Pattern | Baseline ops/s (range) | Candidate ops/s (range) | Change |
|---|---:|---:|---:|
| Read | 1,581.07 (1,557.30–1,604.84) | 1,656.65 (1,555.78–1,757.57) | +4.8% |
| Write | 165.70 (165.66–165.74) | 170.66 (166.10–175.21) | +3.0% |
| Mixed | 279.84 (279.21–280.47) | 282.24 (252.46–312.08) | +0.9% |

Read and mixed ranges overlap. Write ranges do not, but only two runs per arm are available. Read p95 upper bounds are 16.384 ms on both arms; write bounds are 131.072 ms. Metadata byte costs are unchanged; Rust read allocations rise slightly and write allocations fall.

## Qualification, retained evidence and reproduction

All **ten arms / 34 stages** pass source/binary bindings, logical correctness, fresh byte/EOF checks, shutdown and exact owned cleanup. All eight wire scopes have exact retired manifests; their metadata keys are verified absent across all seven SQL families and their owned blob prefixes are verified absent in RustFS. No automatic retry, operation replay or cleanup replay occurs. Docker identities/resources stay unchanged, with no concurrent Cargo/rustc during measurements.

The original suite receipt is SHA-256 `231237b12e335ee87aa6bd188a0cc9e2c2aa39a602b4bc3d0ec6b30835e15ce0`; reversed receipt is `5d234fbf098c557fffceab22bcfeda6407840ca997995e542b85d941fab32244`. Derived files below retain source/build/binary/owner/artifact/observer/configuration hashes and raw numerator/denominator evidence. Connection settings, credentials, private scopes and container IDs are omitted.

| Derived artifact | SHA-256 |
|---|---|
| [report.json](report.json) | `f0835a16f75181c8b850bd962f246c27e338eccf403feb113947156d929bc5f1` |
| [wire100-reverse.json](wire100-reverse.json) | `ba16ca26778c8d03b9f5fa475630a55532eb8883ab368a3d2095104839e4fdd7` |
| [wire100-combined.json](wire100-combined.json) | `cd458a3b54ef9755565d71af30947b30d0750f73380c076d6fd566dfcf6e22cf` |
| [request-types.json](request-types.json) | `e52875979486d6ab0175b6f4b06dca361cbd71407b70a9b9779d31c4dd065e79` |
| [wire100-reverse-request-types.json](wire100-reverse-request-types.json) | `12bbfe2db6017e0e2787a7bac74d937d331e324e2159a0bccfeb990129ebcd9b` |
| [tidb-executors.json](tidb-executors.json) | `2120a62de868881dc17055915cecdfe2267ce8f7c3281ad757b19c47ec024c14` |

Use the committed `quic_tidb_saturation` and `sdk_tidb_metadata_benchmark` harnesses with identical bytes in the compared checkouts. Build through `./scripts/cargo-shared`, separate targets per checkout, Rust 1.95.0, `--release --locked --features allocation-profiling -j1 --no-run`; compile and run with the actual checkout revision in `MOUNT_RS_SATURATION_SOURCE_REVISION` and `MOUNT_RS_INDEXED_BENCH_SOURCE_REVISION`. Use an explicitly owned durable TiDB/RustFS fixture and the [SDK reproduction settings](../tidb-indexed-metadata-20260929/README.md#reproduce-the-sdk-harness). Run emitted binaries serially, preserve complete profiles/fresh oracles/cleanup, and capture the datastore at matching boundaries. Reproduction on another machine requires fresh bindings and measurements.

Set the wire workload selectors below to reproduce the effective settings. Set both client counts to `10` for Wire10. Supply your owned connection settings, datastore observer and fresh absolute artifact/output paths separately. The exact ignored case is `actual_tidb_100_clients_10_servers_saturation`, including when client counts are overridden to ten.

```text
MOUNT_RS_PROFILE_IO=1
MOUNT_RS_TIDB_DURABLE=1
MOUNT_RS_RUSTFS_DURABLE=1
MOUNT_RS_REMOTE_SATURATION_PROVIDER=tidb
MOUNT_RS_REMOTE_SATURATION_BLOCK_PROVIDER=rustfs
MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES=0
MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES=1
MOUNT_RS_REMOTE_SATURATION_CLIENTS=100
MOUNT_RS_REMOTE_SATURATION_ACTIVE_CLIENTS=100
MOUNT_RS_REMOTE_SATURATION_SERVERS=10
MOUNT_RS_REMOTE_SATURATION_RUNTIME_WORKERS=16
MOUNT_RS_REMOTE_SATURATION_SETUP_CONCURRENCY=10
MOUNT_RS_REMOTE_SATURATION_TIDB_POOL_MAX=16
MOUNT_RS_REMOTE_SATURATION_CODEC=binary
MOUNT_RS_REMOTE_SATURATION_SEPARATE_DRIVES=1
MOUNT_RS_REMOTE_SATURATION_PROVISION_DRIVES=1
MOUNT_RS_REMOTE_SATURATION_PRESEED=0
MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=1
MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=read,write
MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=1
MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS=32
MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS=1
MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=5
MOUNT_RS_REMOTE_TIDB_SATURATION_REQUEST_TIMEOUT_SECONDS=30
MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY=1
MOUNT_RS_REMOTE_SATURATION_VERIFY_SECONDS=120
```

The SQL change separately passes 11 new actual TiDB point/write controls and seven existing concurrency/fencing/corruption/cancellation/lost-COMMIT regressions. Those controls are correctness evidence, independent of these timings. Later diagnostic-test count and Windows test-helper fixes do not rebind the measured candidate revision.

**Outstanding qualification:** 10,000 clients / 10,000 Drives / 5,000 Partitions / 1,000 files per Drive, independent production server processes, external OIDC and peer-cache paths, full formal qualification, and crash/power-loss durability. Linux VM, RocksDB and RPC counters do not measure physical Mac SSD IOPS or RustFS backend I/O. Current measurements leave the high-concurrency read regression and slow durable-write path unresolved.
