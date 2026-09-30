# Paired grant-index benchmark — 2026-09-30

## Results

Eight alternating reference/indexed pairs at each catalog size, using a release
build on the local macOS host. Each scale has one Drive per grant, two Drives per
Partition, and two scoped candidates. Queries rotate the first, middle and last
Drive. Values below are medians of process CPU time per completed synthetic
authorization/audit cycle across the eight windows.

| Total grants | Partitions | Full traversal, µs/cycle | Indexed, µs/cycle |
| --- | ---: | ---: | ---: |
| 10 | 5 | 0.463 | 0.482 |
| 100 | 50 | 1.248 | 0.559 |
| 1,000 | 500 | 7.563 | 0.698 |
| 10,000 | 5,000 | 69.399 | 0.754 |

At 10,000 grants, the observed CPU cost is about **92× lower**. Median paired
wall-time speedup is 92.98×. At ten grants, CPU cost rises about 4%; the additional
cache lookup has a small-catalog cost. These are batch-average cycle costs;
individual request latency percentiles were not measured.

Both warmed implementations report **zero allocation requests and zero requested
bytes** in the separate allocation windows. The index improves traversal CPU
cost; this experiment demonstrates no additional warmed allocation reduction.

## Cold preparation

Each pair starts with a new cache and measures one actual index preparation
separately. The catalog snapshot has already been constructed and validated.

| Grants | Median preparation, ms | Allocation requests | Cumulative requested bytes |
| --- | ---: | ---: | ---: |
| 10 | 0.00310 | 43 | 4,044 |
| 100 | 0.01354 | 410 | 38,368 |
| 1,000 | 0.13488 | 4,084 | 383,784 |
| 10,000 | 1.55415 | 40,835 | 3,844,696 |

Cold timing includes allocation-meter overhead. Requested bytes are cumulative
allocation/reallocation traffic, not retained index size or RSS. Index destruction
is outside the window. Reusing the same immutable snapshot amortizes preparation;
changed catalog snapshots require fresh preparation.

## What was measured

Both paths execute policy matching, authorization, an identical preallocated
writer mutex, a 4 KiB stack-buffered audit serializer and terminating newline.
The indexed path includes actual warmed cache selection and Arc reuse. The
reference calls public full-map authorization and the exact original audit
predicate through the same buffering path. Every cycle checks permission and
complete audit bytes. Additional oracles check grant order, current claims,
Read/Write union and empty-condition behavior.

The indexed audit retains two test-only candidate-visit observations per cycle.
The reference omits such instrumentation to avoid introducing 10,000 artificial
TLS increments. Both paths include identical output assertions and periodic
budget checks. Timing and allocation windows are separate. CPU is `RUSAGE_SELF`
for the single-test process, not a thread or fleet measurement.

The complete benchmark finishes in 1.021 seconds. All 635 pinned Rust/Cargo source
inputs and the 7,852,864-byte binary match before and after. The external parent
retains the 64 GiB host floor, bounded captures and 55-second deadline; the test
also has a separate cooperative 55-second budget. The child is reaped and its
process group absent. Final shipping-feature regression tests and strict
all-target Clippy pass.

Catalog loading, JWT verification, dispatcher freshness/wait paths, real audit
OS I/O, transport, storage and SQLite foreign allocation are excluded. The fresh
uniquely owned catalog fallback retains full traversal. This does not qualify
10,000 concurrent clients or establish TiDB/RustFS throughput improvement.

- [summary.json](summary.json): derived costs, ranges, source and capture hashes.
- [observations.json](observations.json): all 32 paired rows, calibration and cold measurements.
- [source-inputs.json](source-inputs.json): relative source paths, sizes and hashes.
- [qualification.json](qualification.json): observed negative controls, regression and lint receipts, and the independent 3,440-check arithmetic audit.
- [verify-summary.py](verify-summary.py): reproduce the public arithmetic audit with observation/summary filenames and their SHA-256 values.
- [Earlier observation](observations-first.json): retained run before two test-only Clippy fixes; its [summary](summary-first.json) records the earlier source identity.
- Source test: `request_metadata::prepared_catalog::paired_benchmark::grant_index_paired_release_cpu_and_allocations`.

Reproduce through `scripts/cargo-shared test --locked --release -p mount-rs-service
--features sdk-runtime,allocation-profiling --lib -- --ignored --exact` followed by
the full test name and `--test-threads=1 --nocapture`. Run with I/O/storage/request/
service tracing disabled, outside competing build or benchmark workloads.
