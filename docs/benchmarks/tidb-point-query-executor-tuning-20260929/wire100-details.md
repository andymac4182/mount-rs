# TiDB FILE query hints: matched Wire100 pair

Candidate2fef followed by preserved baseline801ef, qualified through a separate recovery addendum. The original suite remains false after its compiler-directory identity predicate failed; no workload was replayed. Numbers cover the whole 100-client cluster.

| Mode | Baseline logical ops/s | Candidate logical ops/s | Observed change |
|---|---:|---:|---:|
| read | 2146.37 | 2441.09 | +13.73% |
| write | 191.60 | 212.27 | +10.79% |
| mixed | 375.89 | 376.23 | +0.09% |

## Latency histogram upper bounds

| Mode / operation | Baseline p50 / p95 / p99 ms upper | Candidate p50 / p95 / p99 ms upper |
|---|---:|---:|
| read / read | 65.536 / 131.072 / 131.072 | 65.536 / 131.072 / 131.072 |
| write / write | 524.288 / 1048.576 / 1048.576 | 524.288 / 1048.576 / 1048.576 |
| mixed / read | 8.192 / 65.536 / 65.536 | 8.192 / 65.536 / 65.536 |
| mixed / write | 524.288 / 1048.576 / 1048.576 | 524.288 / 1048.576 / 1048.576 |

These are power-of-two bucket upper bounds for one run each, not exact or pooled percentiles.

## Allocation and SQL work

| Mode / arm | Rust alloc/op | Rust B/op | TiDB Go B/op | TiDB Go heap objects/op | Named SQL calls/op | SQL inode read µs/op | Pool checkout µs/op |
|---|---:|---:|---:|---:|---:|---:|---:|
| read / baseline | 344.33 | 63545.34 | 632802.01 | 6528.85 | 2.0017 | 45820.59 | 14.65 |
| read / current | 343.98 | 63466.26 | 407269.28 | 4887.97 | 2.0025 | 40415.05 | 12.59 |
| write / baseline | 903.30 | 231814.91 | 669726.65 | 6906.82 | 5.0000 | 7105.49 | 3.09 |
| write / current | 904.77 | 231977.71 | 561854.52 | 6113.30 | 5.0000 | 7723.29 | 3.67 |
| mixed / baseline | 621.07 | 148009.81 | 647258.79 | 6708.01 | 3.4992 | 9209.57 | 2.29 |
| mixed / current | 621.20 | 147978.68 | 478254.18 | 5475.73 | 3.5000 | 8502.67 | 2.50 |

TiDB Go counters cover the wider observer window, including background work. SQL and pool times are inclusive wall spans; they overlap and must remain separate.

## RPC and backing put work

| Mode / arm | TiDB requests/op | TiKV gRPC/op | TiKV commands/op | Block put calls/op | Block put calls/write | Block put B/write | Inclusive put await µs/write |
|---|---:|---:|---:|---:|---:|---:|---:|
| read / baseline | 6.0064 | 6.0760 | 4.7670 | 0.0000 | unavailable | unavailable | unavailable |
| read / current | 6.0066 | 5.9572 | 4.8292 | 0.0000 | unavailable | unavailable | unavailable |
| write / baseline | 7.0646 | 6.9932 | 6.6566 | 1.0000 | 1.0000 | 4096.0000 | 443396.7379 |
| write / current | 7.0737 | 7.0275 | 7.0036 | 1.0000 | 1.0000 | 4096.0000 | 395726.4907 |
| mixed / baseline | 6.5530 | 6.4741 | 6.2531 | 0.4997 | 1.0000 | 4096.0000 | 445589.6507 |
| mixed / current | 6.5347 | 6.7392 | 6.4824 | 0.5000 | 1.0000 | 4096.0000 | 437321.8441 |

PUT call/payload deltas are neutral observations. Per-write values use acknowledged write successes; read-only stages have no write denominator. A lower successful payload amount is not classified as an improvement.

The unchanged wire harness has no RustFS HTTP connector or daemon network snapshot. Block put await time includes client/backing work and possible retries; pure network time is unavailable.

## Qualification and interpretation

Both named workloads and both exact owned-scope cleanups passed. Release compiler identity and frozen baseline execution identity remain separate in the companion JSON.

Descriptive wins and losses compare this one fresh pair only. Equal percentile buckets can conceal latency changes within a bucket. Raw counter totals, intervals, success denominators, request classes, interface counters, source hashes and all semantic gates are retained in JSON.

## Measurement limits

- The original suite remains unqualified after a compiler-directory Git observation mismatch. This report is qualified only through the separate immutable recovery addendum and root-observed terminal invocation; no workload was replayed.
- Go/RPC totals trust hashed qualified observer summaries and reset/coverage gates; this extractor does not independently recompute all deltas from raw begin/end snapshots.
- Diagnostic retained TiDB/RustFS fixture; process stages and wider datastore capture windows are separate boundaries.
- Rust System allocator counters exclude foreign allocator activity and measure process work inside each stage; profiling affects throughput.
- RSS endpoints and lifetime peaks are gauges. They are not per-operation allocation or stage peak RSS measurements.
- Linux VM cgroup and RocksDB counters are not cluster physical IOPS, physical SSD operations, or RustFS backend I/O.
- This fresh 100 active-client followup does not establish 10,000-client capacity, production topology, or crash/power-loss durability.
- Wire byte/EOF oracles and exact seven-family/blob cleanup qualify the owned fixtures only.
- Only one fresh candidate/control pair is measured; launch-order, warm-cache and background work remain confounders. Descriptive wins/losses are not statistical claims.
- Go allocation counters measure all TiDB process work over wider observer intervals. Heap objects, mallocs and tiny allocations stay separate; no alias summation or operator/peak-memory inference.
- RustFS PUT uses the SDK block-adapter inclusive await span. HTTP attempt counts, pure socket-network time and RustFS daemon bytes are unavailable in this unchanged wire harness.
- TiDB/TiKV/PD eth0 and client-side QUIC counters are separate interfaces and windows; never sum them as exclusive network bytes or infer backing RustFS traffic.
