# TiDB + RustFS native benchmark — 2026-09-30

This report retains the `fa70c17` observation. The subsequent `24aabef` run is
reported in [the borrowed-root benchmark](../tidb-rustfs-borrowed-root-20260930/README.md).

## Result

The current revision completes the 10-server correctness control, but **these
measurements demonstrate no throughput improvement**. All-active reads reached
594–651 completed file cycles/s and overwrites 207–216 cycles/s across the cluster.
Append/truncate was substantially slower than the historical observation.

| Pattern | Historical 6a60518 cycles/s | Current fa70c17 cycles/s | Observed change |
| --- | ---: | ---: | ---: |
| Sequential read | 627.91 | 594.10 | −5.38% |
| Random read | 656.04 | 651.01 | −0.77% |
| Sequential overwrite | 227.81 | 216.12 | −5.13% |
| Random overwrite | 235.60 | 206.80 | −12.22% |
| Mixed read/write | 342.71 | 313.75 | −8.45% |
| Hot file | 285.37 | 208.04 | −27.10% |
| Append/truncate | 279.14 | 111.02 | −60.23% |
| Create/rename/unlink | 54.98 | 50.83 | −7.54% |

These are **cluster totals**, not rates per drive or physical disk IOPS. A read
or overwrite cycle is Open → one 4 KiB read/write → Close; handle Stat is omitted.
Append/truncate alternates appending one block with truncating to one block.
Churn is create → close → rename → unlink. Mixed alternates reads and writes;
hot-file traffic reads every fourth cycle and writes the others, selecting one
file within each drive for nine of every ten cycles. Each client acts sequentially
on its own drive; this is not ten clients contending on one shared hot file.

The comparison uses two independently qualified local runs. The baseline is
historical, not a balanced paired experiment: dates, cache history, several code
changes and evolving file state differ. Each cell requests five active seconds.
Percentages describe those observations; they do not isolate the effect of the
latest optimization or establish saturation capacity.

## Workload and correctness

Actual durable local TiDB metadata plus RustFS strict blob writes, using indexed
MRC5 and lazy SDK construction over loopback QUIC: **10 servers, 10 clients,
10 drives, 5 partitions, 1,000 files per drive**. All eight patterns ran with one
active client and nine idle clients, then with all ten clients active. Blob RAM,
disk and peer cache tiers were not configured. Authentication uses a signed local
ES256 fixture. No OS mount, external OIDC issuer or cross-host network was tested.

The qualified current run completed all 16 cells and acknowledged all 100,826
cumulative requests, with zero failed or uncertain outcomes. That request total
includes untimed phases and must not be divided by cell duration. Fresh independent
initial and final oracles read every file's complete payload: 10,000 files,
62,832,640 initial bytes and 62,865,408 final bytes. Both settled. Cross-node checks
verified 100 payload pairs / 409,600 bytes; partition, sibling-drive and revocation
checks each denied ten unauthorized accesses. All ten workers drained, closed,
exited successfully and were reaped. The owned RustFS container stopped; source
pins and all eight original fixture containers remained unchanged.

The first current-source attempt failed during untimed setup after a worker
reported `resource coverage stale or invalid`. Ten final Open outcomes became
uncertain; no timed cell or full oracle completed. Its data is retained and the
unknown operations were never replayed. The exact rejected timestamp was not
retained, so the initiating cause remains unresolved. The qualified run used a
fresh independent namespace and owned RustFS fixture with the same guards.

This qualifies a bounded control with **10 clients**, not the intended production
10,000 clients / 10,000 drives / 5,000 partitions. No resource or performance
threshold was raised to obtain these results.

## SQL amplification and waits

| All-active cycle | Historical SQL calls / returned rows | Current SQL calls / returned rows |
| --- | ---: | ---: |
| Sequential or random read | 3 / 3 | 3 / 3 |
| Sequential or random overwrite | 6 / 5 | 6 / 5 |
| Create/rename/unlink | 44 / 10,131.30 | 44 / 7,069.02 |

Churn returned about 30.23% fewer rows per completed cycle in this observation,
while its 44 classified SQL calls/cycle persisted. Reads performed one logical
SDK block get of 4 KiB and one backing verification per cycle. Overwrites performed
one logical SDK block put of 4 KiB, two backing verifications and one commit per
cycle. These are client-observed classified submissions and returned rows; they
exclude affected-row counts and do not count TiKV physical work or every wire
statement. Churn state varies across the short historical runs, so this row
comparison does not isolate a production change.

| All-active operation await | Historical mean | Current mean |
| --- | ---: | ---: |
| Sequential overwrite SDK put | 17.937 ms | 18.954 ms |
| Sequential overwrite TiDB commit | 4.645 ms | 5.003 ms |
| Append/truncate SDK put | 17.521 ms | 98.724 ms |
| Append/truncate TiDB commit | 4.864 ms | 23.505 ms |

The current append/truncate cell used 1.562 seconds of aggregate worker CPU and
0.250 seconds of controller CPU over 5.134 active wall seconds. Its much larger
awaits locate a useful next investigation, but do not establish whether RustFS,
TiDB, host scheduling or another stage caused the slowdown. Await intervals include
driver/network work, and nested counters overlap. Do not sum them as exclusive
CPU or path percentages. No SQL errors or cancellations occurred in qualified
cells. QUIC reported no lost packets or congestion events in those cells.

## Allocations and resources

The separate
[paired selected-read comparison](../tidb-materialized-read-20260930/README.md)
includes Open → handle Stat → Read → Close with TiDB providing both metadata and
blocks. At 1,000 files, removing guard JSON encoding/reparsing reduced allocation
requests **1.99%** and requested bytes **20.67%**. It demonstrated no latency win.
The original 256-allocation gate still fails: Stat averages 8,656 allocation
requests and 2,006 returned inode rows are observed over the whole cycle. The
native RustFS run has allocation profiling disabled; its throughput samples and
the paired allocation samples are separate experiments.

Current native controller peak RSS was 141,934,592 bytes; worker peaks were about
80 MB each. The sum of individual lifetime peaks was 943,456,256 bytes, **not a
simultaneous fleet peak**. Minimum observed host free space was 78,531,444,736 bytes,
above the unchanged 64 GiB floor. Minimum observed Docker guest available memory
was 2,908,278,784 bytes. CPU observations include metric observers and background
work, and RSS endpoints do not measure per-cell heap allocation.

The owned RustFS guest cgroup recorded 27,592 reads / 301,776,896 bytes and 65,060
writes / 335,597,568 bytes over its entire 404.029-second native capture window.
The historical capture recorded 251 reads / 2,048,000 bytes and 73,557 writes /
361,713,664 bytes over 342.149 seconds. These include setup, oracles, background
work and writeback, with different cache histories. They are neither per-pattern
IOPS nor host flash IOPS. No contemporaneous TiKV physical IOPS were captured;
this benchmark cannot compare total datastore amplification against the laptop's
advertised 100,000 IOPS.

## Mostly-idle observations

One active client, nine connected idle clients; cluster cycles/s:

| Pattern | Current cycles/s |
| --- | ---: |
| Sequential read | 143.32 |
| Random read | 149.98 |
| Sequential overwrite | 53.74 |
| Random overwrite | 53.54 |
| Mixed read/write | 78.65 |
| Hot file | 49.52 |
| Append/truncate | 64.70 |
| Create/rename/unlink | 12.86 |

## Evidence

The measured production revision is `fa70c174cc050b68863477abd38947e097264fac`;
the historical revision is `6a60518d5601f11e5923d725675772f941a5e18b`.
The current native release archive pins 570 source/build inputs. Its exact binary
built successfully and the native inventory contained 176 tests; 170 pure controls
passed with six explicit opt-ins ignored. Native worker/controller opt-ins were
subsequently invoked by the owned runtime harness. The retained release artifact,
source closure, raw metrics, fresh oracle receipts and process outcomes were
independently checked. The extractor's 704 active boundary snapshots and all
published counter deltas were independently verified with no mismatches.

- [summary.json](summary.json): exact rates, logical amplification, operation means,
  CPU, resources and comparison identities.
- [observations.json.gz](observations.json.gz): fixed public whitelist of counters,
  histograms, endpoints and raw artifact hashes; excludes credentials and paths.
- [qualification.json](qualification.json): release/build/process witnesses,
  successful control, retained failed attempt and qualification limits.

Metrics required by this control were complete; broader coverage is incomplete
(physical IOPS, HTTP attempts and direct raw object-store instrumentation are
unavailable). Exact-revision CI also passed its durable TiDB/RustFS and Windows
jobs in both push and PR runs. Other CI gates remain failed or cancelled. Full
production-scale qualification, the remaining CI throughput floors and complete
formal verification are still outstanding. This report does not qualify merging
the broader performance work.
