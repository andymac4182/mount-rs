# TiDB / RustFS packet-budget change: native comparison

This native pair confirms the selected-write SQL reduction. It did not show an all-active overwrite throughput or allocation improvement.

Before: `be510e4bd638504306eca37ec63f9379fdb6e8e9`. After: `9d4d09d51f7e20ae1bf73c548d6d109a98d4e64b`. Both fresh release builds use `allocation-profiling,io-profiling,resource-profiling,sdk-runtime`, Rust 1.95.0, optimization level 3, no debug assertions/debug info, and no incremental compilation. Only the TiDB compact writer and its query control differ in the archived 637-file source closure. The [provider change and safety controls](../tidb-packet-budget-projection-20260930/README.md) explain the operation being compared.

## Configuration and correctness

Ten native server processes on one macOS laptop, TiDB metadata and a fresh RustFS blob fixture per run; ten real QUIC clients, ten Drives, five Partitions and 1,000 files per Drive. Each initial dataset contains 10,000 files and 62,832,640 bytes. Mostly-idle keeps one client active; all-active keeps ten active, each with one request in flight. Sixteen nominal five-second cells run per build. Rates include final in-flight completion. Each Drive starts with 990 files of 4 KiB, nine of 128 KiB and one of 1 MiB; active read/overwrite payloads are 4 KiB. The eight patterns run in a fixed order. TiDB is 8.5.7; each run owns a fresh RustFS 1.0.0 fixture capped at two CPUs, 2 GiB memory and 512 PIDs, with strict bucket policy requested and authenticated policy preflight retained. This is a small local fixture, not an uncapped production backend.

Both runs passed complete initial/final payload, size, EOF and membership checks, 100 cross-server payload pairs, Partition/sibling scope denials, revocation checks and clean worker retirement. Failed and uncertain requests were zero. Their final acknowledged ledgers contain different byte totals because duration-based append/truncate execution completed different work: 62,861,312 bytes before and 62,865,408 after.

The generic object-store adapter cache is present; the composed RAM/disk/QUIC peer cache is unconfigured. Authentication uses the local fixture. External OIDC, mounted Linux/macOS filesystems, cross-host traffic and the 10,000-client target remain outside this benchmark.

## Whole-cluster throughput

Completed filesystem cycles/s, including Open/Close; values rounded to three decimals. A basic read or overwrite cycle contains three acknowledged RPCs; churn contains four. These are neither per-Drive rates nor physical IOPS. The companion data retains exact counts and rational rate inputs.

| Pattern | Idle before | Idle after | Active before | Active after |
|---|---:|---:|---:|---:|
| sequential_read | 98.273 | 127.278 | 396.376 | 590.972 |
| random_read | 128.452 | 150.683 | 525.149 | 637.134 |
| sequential_overwrite | 53.162 | 51.791 | 221.185 | 215.306 |
| random_overwrite | 48.726 | 52.517 | 213.443 | 205.181 |
| mixed | 79.532 | 79.452 | 336.862 | 288.036 |
| hot_file | 64.265 | 66.108 | 255.679 | 262.270 |
| append_truncate | 60.902 | 66.099 | 256.227 | 242.966 |
| churn | 12.330 | 12.306 | 53.232 | 54.480 |

All-active sequential/random overwrites were 2.66%/3.87% lower in this pair. Read rates increased despite unchanged read SQL. Both runs used matching geometry and instrumentation, but sequential run order, shared TiDB/background state, cache evolution and duration-dependent request counts remain confounders. Each point is one short cell; no confidence intervals or isolated causal throughput estimate are established.

## SQL amplification

Both overwrite patterns, in both traffic modes, produced the exact contract below.

| Per complete overwrite cycle | Before | After |
|---|---:|---:|
| Classified SQL submissions | 6 | 5 |
| Known returned SELECT rows | 5 | 4 |
| Standalone packet-cap SELECT | 1 | 0 |
| Classified inode-write submission | 1 | 1 |
| COMMIT | 1 | 1 |
| SDK blob PUT | 1 | 1 |

This removes 16.7% of classified SQL submissions per overwrite. The eliminated row contains a session variable; the result does not establish one fewer persistent TiKV read. The separate provider proxy control's total commands **8 to 7** include transaction controls and use a different denominator. Reads remain three SQL calls/three rows; structural churn remains 44 SQL calls and about 7,070 known returned rows per all-active cycle.

## Allocations and remaining spans

All-active worker-fleet System Rust allocator deltas divided by completed cycles:

| Pattern | Allocation/reallocation requests before | After | Allocated bytes before | After |
|---|---:|---:|---:|---:|
| sequential_overwrite | 2,292.434 | 2,295.941 | 453,611.739 | 452,346.140 |
| random_overwrite | 2,318.819 | 2,350.271 | 457,578.051 | 462,353.507 |

These process-wide windows include drivers, transport, observers and background activity and exclude foreign C allocators. They do not isolate metadata. This pair demonstrates no overwrite allocation reduction or zero-allocation claim. Live Rust bytes remain before/after gauges; RSS lifetime peaks are individual process peaks, not simultaneous fleet peaks.

| All-active overwrite inclusive mean span | Sequential before | After | Random before | After |
|---|---:|---:|---:|---:|
| Packet-cap SELECT, ms | 0.989 | absent | 0.997 | absent |
| Fresh authority/member query, ms | 2.308 | 2.704 | 2.366 | 2.688 |
| SDK blob PUT, ms | 18.413 | 19.187 | 19.041 | 20.406 |
| Transaction COMMIT, ms | 4.765 | 5.049 | 5.132 | 5.533 |

Mean local commit-gate waits were below 0.3 us in these overwrite cells. Small means do not rule out tail contention or contention with more clients per Drive. SQL, gate, SDK and HTTP spans overlap; they cannot be added as exclusive costs. SDK PUT spans do not distinguish RustFS CPU, network and persistence. RPC and provider percentiles are retained as log2 bucket bounds, not exact latencies or complete-cycle histograms.

The demonstrated improvement is one fewer SQL command per selected publication. Further throughput work should focus on the larger blob/publication spans and a scoped allocation profile. Whole-run RustFS guest I/O counters include setup, oracles, background activity and writeback; contemporary TiKV and physical Mac IOPS remain unavailable. The full ten-server / 10,000-client / 10,000-Drive / 5,000-Partition / 1,000-files-per-Drive target remains unqualified.

## Actual qualification and evidence

Both bounded native owners qualified: exit zero, no failed qualification labels, no retry/abort/forced stop or signals, all workers/controller/collector retired, pipes closed and source pins unchanged. Their total supervisor elapsed times were 387.414 s before and 407.134 s after. The original eight backend containers and six volumes retained their identities, configuration and lifecycle. The two new RustFS containers stopped; their data and unique TiDB rows were retained. A 64 GiB host-free floor was sampled throughout, with observed minima 76,784,365,568 and 75,331,112,960 bytes. These owner checks do not establish power-loss durability.

The independent offline verifier actually passed all 32 cells and exact count/rate, SQL, allocation, HTTP, CPU, QUIC, percentile-bound, source and correctness-roster joins. All three in-memory corruptions were rejected: a missing cell, a changed table allocation counter and a false allocation-profile flag. No unexpected SQL profiles were found.

Public extraction SHA-256: `d94225a9d87e15ca8ca61392e682086cbd8556606887e9a986fe5d6b4a4e320d`. The [summary](summary.json) retains every cell table, exact rational inputs and overwrite component spans. [Qualification](qualification.json) binds the actual supervisor, extractor, verifier and health receipt hashes. [Compressed observations](observations.json.gz) retain timed process counters and metrics, aggregate histograms, and hash-bound boundary, source and oracle rosters: 19,142,292 decompressed bytes, 1,372,598 compressed bytes, compressed SHA-256 `165e3983a804772e0569d4e5409788caaf00e46cae4239399d3d4d3ffddc5da9`. The projection contains no private paths, connection URLs, credentials or tokens.

To independently check the committed observations without runtime access, decompress them to a temporary JSON file, then run:

```sh
python3 -I -S -B docs/benchmarks/tidb-packet-paired-20260930/verify.py /absolute/path/to/observations.json d94225a9d87e15ca8ca61392e682086cbd8556606887e9a986fe5d6b4a4e320d --negative-controls
```

The checker reads only the supplied bounded regular JSON, verifies its exact hash and runs in-memory corruption controls; it does not run fixtures, import the workload/extractor, use network access or write files.

## Backend limits and the next comparison

The current overwrite profile localizes large inclusive spans to blob PUT (19–20 ms) and TiDB publication, including COMMIT (5–5.5 ms). It does not prove that either backend has reached its capacity limit. A depth-one, ten-client workload and a RustFS fixture capped at two CPUs cannot establish a production saturation curve. TiKV physical I/O and current per-cell RustFS CPU/stage attribution are incomplete.

A durable filesystem blob provider would make a useful matched control with TiDB unchanged. There is currently no plain-filesystem BlockStore exposed through StoreConfig; HostFs serves an entire host filesystem, while SQLite blocks are available as a different storage representation. A new provider must preserve immutable content identities, root isolation, file/parent-directory persistence barriers and stable backing authority. Simply using the generic local object-store adapter with its current no-op flush would not meet that contract.

Ten processes can compare a shared local directory on this laptop. Ten production machines would require a qualified shared filesystem or authoritative replication/ownership and recovery; separate local disks plus the current best-effort peer cache do not replace shared durable blob storage. A filesystem result would measure a different topology and cannot alone prove RustFS saturation.
