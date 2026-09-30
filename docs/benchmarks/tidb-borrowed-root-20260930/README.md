# TiDB borrowed root read comparison — 2026-09-30

## Result

Borrowing complete membership and directory rows reduces Rust allocation
requests and requested bytes while preserving the complete-root Stat audit.
At 1,000 files, the measured cycle uses **45.22% fewer allocation requests** and
**67.54% fewer requested bytes**. The cycle still allocates approximately 5,090
times and fails the original 256-call limit.

| Files in directory | Metric | Owned materialized A | Borrowed B | Change |
| --- | --- | ---: | ---: | ---: |
| 128 | Mean allocation requests/cycle | 2,163 | 1,603 | −25.89% |
| 128 | Mean requested bytes/cycle | 200,351 | 136,181 | −32.03% |
| 128 | Median cycle latency | 10.709 ms | 10.773 ms | +0.60% |
| 128 | Mean cycle latency | 10.828 ms | 10.986 ms | +1.46% |
| 128 | Nearest-rank p95 latency | 12.563 ms | 13.357 ms | +6.32% |
| 1,000 | Mean allocation requests/cycle | 9,292.19 | 5,090.19 | −45.22% |
| 1,000 | Mean requested bytes/cycle | 674,644 | 218,959 | −67.54% |
| 1,000 | Median cycle latency | 14.216 ms | 14.217 ms | +0.0037% |
| 1,000 | Mean cycle latency | 16.030 ms | 14.470 ms | −9.73% |
| 1,000 | Nearest-rank p95 latency | 22.547 ms | 17.582 ms | −22.02% |

These are descriptive pooled statistics, with 128 timed samples per role and
directory size across four independent process trials. The 1,000-file median
is effectively unchanged: 14.2161455 → 14.216666 ms. Its observed p95 is lower;
the control's 88.43 ms maximum also affects its mean. Trial medians vary, and
128-file latency is worse in this observation. **This does not establish a
general speed or throughput improvement.** No confidence intervals are claimed.

## Workload and comparison

Actual durable local TiDB 8.5.7 supplies **both metadata and blocks**. This is
one filesystem/provider cycle, using indexed MRC5 with compact inode updates,
not an OS mount, QUIC, server cluster or RustFS workload. The target contains
`selected-read`; the other files share its immutable block. Root directory order
is reversed to exercise order preservation.

Each sample runs the unchanged Open → handle Stat → Read → Close cycle and
checks the target inode and bytes at offset zero. Four untimed warmups precede
32 timed windows per directory size. Eight process trials run in **ABBABAAB**
order. Both roles use the same core, private driver fork, dependency resolution
and benchmark source. The sole changed production input disables the
`borrowed::try_root` caller for A; B uses the borrowed path. The measured-source
manifests bind 628 inputs, including the fork and both lockfiles, and the copied
release binaries are hashed. Restored and post-run candidate manifests match.

Allocation requests include current-thread Rust driver/runtime activity;
foreign threads and TiDB allocation are excluded. Requested bytes are not RSS
or retained heap. Latency includes the allocation meter and phase counters.
Setup, warmups, diagnostic snapshots/deltas, sample storage, fresh oracles,
cleanup and reporting occur outside the measured windows. Execution receipts
record 300-second child deadlines, a 64 MiB capture limit per file, a 64 GiB host
free-space floor, reaping and absent process groups without resource failures.

## SQL rows and remaining allocations

Every one of the 512 timed cycles has the same successful observed counts in
both roles, with zero diagnostic errors, cancellations or in-flight operations:

| Provider-observed operation / cycle | 128 files | 1,000 files |
| --- | ---: | ---: |
| Inode read statements / returned rows | 7 / 262 | 7 / 2,006 |
| Metadata read statements / returned rows | 1 / 1 | 1 / 1 |
| Block read statements / returned rows | 3 / 3 | 3 / 3 |
| Rollbacks | 1 | 1 |
| Pool checkouts | 8 | 8 |
| Session configurations | 0 | 0 |

The complete-root audit still reads all members and selected-parent entries;
inode rows are `2F + 6` for this fixture. This change reduces ownership and
reconstruction costs, **not SQL row amplification**. These counts are not the
complete MySQL protocol statement count or TiKV/RocksDB physical IOPS.

At 1,000 files, Stat falls from 8,656 to **4,454 allocation requests** and from
607,752 to **152,068 requested bytes**. It remains 87.50% of candidate cycle
allocations. Isolated packet-decoder and core-cursor controls report zero
allocations after their stated setup/decoding boundaries; the complete provider
and filesystem cycle still allocate. Open, Read and Close contribute about 636
calls even before Stat.

The original single-cycle controls retain their workload and **256-call limit**
and are reported separately. Both sizes fail in both roles after fresh oracles
and cleanup: A records 2,164 / 9,296 calls and B records 1,604 / 5,094 calls for
128 / 1,000 files. These observations are excluded from the pooled statistics.

## Correctness and qualification limits

All 16 paired cases and four original-control cases reach fresh independent
complete metadata audits, comparing the anchor, root guard and directory order
with the seed, and checking the target inode and content. The oracle does not
check EOF, every file guard or every file's complete payload. Actors close
before exact generated-key cleanup and verified zero rows in all seven owned
metadata/block row families. Failed earlier operations retain their diagnostic
data and are not replayed.

The complete membership/root-entry contract remains in place, including fresh
transaction validation and explicit rollback before a checked result. The
allocation ceiling, formal completion, CI performance floors, native
TiDB/RustFS throughput and 10,000-client capacity remain separate gates.

See [summary.json](summary.json) for exact cycle/phase statistics,
[paired-results.json.gz](paired-results.json.gz) for retained samples and
execution receipts, and [qualification.json](qualification.json) for source
and validation provenance. The earlier
[materialized comparison](../tidb-materialized-read-20260930/README.md) is a
separate historical experiment; the A values here are the fresh paired control.
