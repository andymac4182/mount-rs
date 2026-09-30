# TiDB + RustFS after borrowed-root reads — 2026-09-30

## Result

The measured revision is `24aabef70726f1bf05cda0f713d69094803736d2`.
The bounded correctness control passes, but this run **does not demonstrate a
throughput improvement**. All-active reads completed 386–467 file cycles/s and
overwrites 131–156 cycles/s across the cluster. Allocation reductions measured
separately have not translated into an observed native throughput win.

| All-active pattern | Earlier fa70c17 cycles/s | Current 24aabef cycles/s | Observed change |
| --- | ---: | ---: | ---: |
| Sequential read | 594.10 | 386.39 | −34.96% |
| Random read | 651.01 | 467.45 | −28.20% |
| Sequential overwrite | 216.12 | 131.15 | −39.32% |
| Random overwrite | 206.80 | 155.69 | −24.71% |
| Mixed read/write | 313.75 | 232.09 | −26.03% |
| Hot file | 208.04 | 160.35 | −22.92% |
| Append/truncate | 111.02 | 196.34 | +76.86% |
| Create/rename/unlink | 50.83 | 32.62 | −35.83% |

These are **whole-cluster totals**, not rates per drive or physical disk IOPS.
Read and overwrite cycles are Open → one 4 KiB I/O → Close. Native cycles omit
handle Stat. Append/truncate alternates appending a block and truncating to one
block. Churn creates an empty file, closes, renames and unlinks it. Mixed
alternates reads and writes. Hot-file traffic reads every fourth cycle and writes
the others, choosing one file within each drive for nine of every ten cycles.

The earlier run is historical, not a balanced paired experiment. Source, date,
cache history and evolving file state differ. Each cell requests only five active
seconds. Percentages describe these observations; they do not isolate a causal
regression or establish saturation capacity. The earlier complete report is
[retained separately](../tidb-rustfs-native-20260930/README.md).

## Workload and correctness

Actual durable local **TiDB 8.5.7 metadata plus RustFS 1.0.0 strict blob writes**,
indexed MRC5, lazy SDK construction and loopback QUIC: **10 servers, 10 clients,
10 drives, 5 partitions and 1,000 files per drive**. Every client has its own
drive and submits one cycle at a time. All eight patterns ran with one active
client and nine idle clients, then all ten clients active.

Each drive initially contains 990 × 4 KiB files, nine × 128 KiB files and one
1 MiB file. Timed reads and writes use 4 KiB. Distributed RAM, disk and peer blob
cache tiers are not configured; the generic object-store adapter cache is
present. A signed local ES256 authentication fixture is used. This does not
exercise OS mounting, an external OIDC issuer or a cross-host network.

All 16 cells completed. The 90,251 cumulative requests were acknowledged with
zero failed or uncertain outcomes; that total includes untimed work. Independent
fresh initial and final oracles each verified all 10,000 files and their complete
payloads: 62,832,640 initial bytes and 62,865,408 final bytes. Cross-node checks
verified 100 payload pairs / 409,600 bytes. Partition, sibling-drive and
revocation checks each denied ten unauthorized accesses. All ten workers drained,
closed, exited successfully and were reaped. The owned RustFS container stopped;
source pins and all eight original fixture containers remained unchanged. Data
is retained; dataset absence is not claimed.

This qualifies ten clients, not the production target of 10,000 clients,
10,000 drives and 5,000 partitions. The static scratch grant-index and evidence
sharding patches have not been integrated and are not included in this benchmark.

## Amplification and waits

| All-active cycle | Classified SQL calls | Known SELECT returned rows |
| --- | ---: | ---: |
| Sequential or random read | 3 | 3 |
| Sequential or random overwrite | 6 | 5 |
| Create/rename/unlink | 44 | 7,057.17 |

Read/write counts match the earlier run. Churn still reads thousands of inode
rows per completed namespace cycle. Counts are client-observed submissions and
successful known-count SELECT results; affected rows, server-scanned rows, every
wire statement and TiKV physical work are not measured by these counters.

| All-active operation await | Earlier fa70c17 mean | Current 24aabef mean |
| --- | ---: | ---: |
| Sequential read SDK get | 1.644 ms | 2.442 ms |
| Random read SDK get | 1.082 ms | 1.692 ms |
| Sequential overwrite SDK put | 18.954 ms | 36.606 ms |
| Sequential overwrite TiDB commit | 5.003 ms | 10.167 ms |
| Random overwrite SDK put | 21.477 ms | 32.526 ms |
| Random overwrite TiDB commit | 5.650 ms | 6.998 ms |
| Append/truncate SDK put | 98.724 ms | 34.040 ms |
| Append/truncate TiDB commit | 23.505 ms | 8.685 ms |

Pool checkout averaged about 1.6–2.4 microseconds in current all-active cells,
including any lazy connection/session work; queue-only wait is unavailable.
Sequential overwrite consumed 2.092 aggregate worker CPU seconds and 0.358
controller CPU seconds during about five active wall seconds. Worker/controller
CPU counters exclude TiDB, TiKV, RustFS and the collector. These observations
point to blob and transaction awaits for investigation; they do not establish
which backend or scheduling stage caused the slowdown. Nested await counters
overlap and must not be summed as exclusive CPU or path percentages.

The new extraction retains observed RustFS HTTP dispatch and body counters by
data/probe role and method. Each backing verification emits two marker GETs:
one on the probe adapter and one on the data adapter, each returning 20 bytes.

| All-active cycle | Observed marker GETs | Other observed blob requests |
| --- | ---: | --- |
| Sequential or random read | 2 | Payload GETs on adapter cache misses |
| Sequential or random overwrite | 4 | 1 PUT |
| Append/truncate | 4 | About 0.5 PUT per alternating cycle |
| Create/rename/unlink | 10 | No logical payload request |

Sequential/random reads averaged 2.888 / 2.720 total GETs per cycle, including
marker requests and cache misses. The data adapter's counters mix marker and
payload requests. These are partial service observations, not a complete HTTP
attempt or wire trace. PUT offered bytes do not prove upload or commit; header
responses do not prove EOF or durability. The legacy broader HTTP coverage flag
remains incomplete. Generic adapter cache gauges are retained separately from
distributed cache configuration. Repeated marker checks and namespace SQL are
concrete remaining amplification; this report does not justify dropping their
correctness checks.

## Allocations and physical I/O

The separate [balanced packet-classification comparison](../tidb-packet-classification-20260930/README.md)
uses TiDB for both metadata and blobs and includes Open → handle Stat → Read →
Close. At 1,000 files, the packet guard reduced allocation requests by **83.30%**
(5,090 → 850) and requested bytes by **46.56%**. Median latency rose 3.08% and p95
rose 33.43%; it demonstrates an allocation improvement, not a latency win. Its
unchanged 256-allocation limit still fails. Complete-root Stat continues to
enumerate directory membership, so returned rows still grow with file count.
Allocation profiling is disabled in this native RustFS run.

Current controller peak RSS was 146,636,800 bytes; individual worker peaks were
89–91 MB. Their summed lifetime peaks were 1,049,149,440 bytes, **not a simultaneous
fleet peak**. Minimum observed host free space was 71,074,230,272 bytes, above the
unchanged 64 GiB floor.

The owned RustFS guest cgroup recorded **39,969 reads / 458,772,480 bytes** and
**69,190 writes / 335,417,344 bytes** during its 456.125-second capture. The earlier
capture recorded 27,592 reads and 65,060 writes over 404.029 seconds. These windows
include setup, fresh oracles, background work and writeback. They are not
per-pattern physical IOPS or host SSD IOPS. Contemporaneous TiKV physical I/O is
not extracted, so total datastore amplification and the laptop's advertised
100,000 IOPS remain unqualified.

## Mostly-idle observations

One active client and nine connected idle clients; whole-cluster cycles/s:

| Pattern | Current cycles/s |
| --- | ---: |
| Sequential read | 108.47 |
| Random read | 124.88 |
| Sequential overwrite | 45.84 |
| Random overwrite | 39.31 |
| Mixed read/write | 65.35 |
| Hot file | 55.60 |
| Append/truncate | 52.35 |
| Create/rename/unlink | 10.17 |

## Larger controls and qualification

Three attempts at 20 clients / 20 drives / 10 partitions / 20,000 files produced
**no timed throughput result**:

1. Docker Engine's initial info request timed out before fixture creation.
2. A container-statistics observation exceeded its unchanged five-second deadline
   during setup. The native processes were retired. A later exact-identity check
   confirmed the owned RustFS container was already stopped. Unknown writes were
   not replayed and data is retained.
3. A reviewed observer-only derivative used Docker's one-shot statistics query,
   preserving all resource and deadline limits. It refused to create a fixture
  because guest available memory was 2,253,705,216 bytes (2.10 GiB), below the
  existing 3 GiB startup requirement. Original fixture health checks passed.

Retained guest samples show available memory falling from 4.04 to 2.40 GiB
during the successful ten-client run, with anonymous memory increasing 1.92 GiB.
The test RustFS container peaked at 0.576 GiB, so its usage alone does not account
for that decline. The captures do not identify which VM process owns the growth
or establish that memory pressure caused the earlier statistics timeout.

The measured immutable release archive pins 632 source/build inputs. Its locked
build and 178-test inventory passed; 172 pure controls passed with six explicit
opt-ins ignored. Runtime worker/controller opt-ins then ran in the owned harness.
The retained extractor validates source/artifact hashes, identities, full fresh
oracles, settled outcomes and all active counter arithmetic before publication.
Independent arithmetic review checked 704 active frames, 1,054,962 scalar values
and 3,104 histogram bounds with zero mismatches or negative deltas. CPU, RSS,
cgroup and all 16 historical comparison calculations also matched.

- [summary.json](summary.json): exact rates, amplification, awaits and resources.
- [observations.json.gz](observations.json.gz): public whitelist of observations
  and evidence hashes; excludes credentials and private absolute paths.
- [qualification.json](qualification.json): build, process and failed-control
  provenance, fixed limits and remaining gaps.

PR #34 remains open with failing CI gates. Complete formal verification and
production-scale qualification remain outstanding. This benchmark does not
qualify merging the broader performance work.
