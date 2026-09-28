# TiDB/RustFS: 100-drive namespace diagnostic

The latest completed steady workload measurement remains the
[guarded autocommit control](../tidb-rustfs-autocommit-20260928/README.md):
ten servers, ten active clients, ten Drives, five Partitions and 100 files per
Drive, using a release build at `52c1cdee`. Its whole-fleet logical cycle rates
were 737/s sequential reads, 819/s random reads, 201/s sequential overwrites,
194/s random overwrites and 331/s mixed operations. A read or overwrite cycle
includes one 4 KiB data operation, open and close.

This new diagnostic used clean commit `f94c1918` in a **debug build**, with
ten QUIC server processes, 100 clients, 100 separate Drives, 50 Partitions and
1,000 files per Drive. It used TiDB metadata and RustFS/S3 blobs through the SDK
runtime on local loopback, with signed local ES256 fixture authentication.
The parent had a 600-second bound. Namespace creation completed; payload
population was still running when the bound terminated the process.

## Completed setup measurements

| Measurement | Observed value | Scope |
| --- | ---: | --- |
| Empty Drive initialization | 98.159 seconds | All 100 Drives; phase journal clock |
| Namespace creation | 242.625 seconds | 100,000 files; includes first lazy runtime activation |
| Acknowledged file creates | 100,000 | Completed namespace phase |
| Create and close acknowledgments | 200,000 | Completed namespace phase |
| Returned inode rows | 200,100 | Adapter counter; includes cold root reads |
| Compact capture node visits | 50,150,000 | 100,000 capture calls |
| Expected guard nodes | 100,000 | One observed parent guard per create |
| Metadata JSON bytes returned or serialized | 5,659,454,304 bytes | Sum of four application counters |
| Worker process CPU | 1,140.354429 CPU-seconds | Ten workers during namespace phase |
| Maximum sampled process-group RSS | 2,020,704,256 bytes | Whole diagnostic; excludes Docker backend processes |
| Completed steady workload stages | 0 | Payload population did not finish |
| Fresh backend byte-oracle passes | 0 | Whole diagnostic incomplete |

The metadata byte total consists of 442,702,504 anchor bytes returned,
221,710,200 anchor bytes serialized, 3,305,924,100 inode bytes returned and
1,689,117,500 inode bytes serialized. It measures application JSON work.
It does not measure wire bytes, Raft replication, blob bytes or device I/O.
An anchor and inode observation can originate from one joined SQL statement;
adding those event counts would overstate SQL requests.

The compact create path now selects affected inode rows, but substantial
namespace-sized work remains: 50.15 million capture node visits and 5.66 GB of
metadata JSON for this creation phase. These observed counters support further
work on namespace traversal and metadata representation. They do not establish
an allocation count: allocation profiling was disabled, and the zero candidate
clone counter does not cover this create path.

The namespace phase recorded 100,000 transaction commits with
4,166.770834920 seconds of cumulative commit span, and 200,310 pool checkouts
with 6.888861994 seconds of cumulative checkout span. These are inclusive async
spans across concurrent workers. They overlap other spans and cannot be summed
into exclusive CPU time or elapsed phase time. They do not isolate TiDB lock
contention or the RustFS throughput ceiling.

## Saved backing-server metrics

Exact offline parsing of the before/after Prometheus bodies completed for TiDB
and each of the three TiKV members. All selected families were available at
both endpoints, with unchanged captured role identities. The finite selected
coverage was:

| Role | Selected metric family | Matched full-label coverage |
| --- | --- | ---: |
| TiDB | Query duration | 15 histogram pairs |
| TiDB | TiKV client request duration | 56 histogram pairs |
| TiDB | TiKV client backoff duration | 13 histogram pairs |
| TiDB | Pessimistic retry values | 1 histogram pair |
| Each TiKV member | Scheduler stages | 860 counter samples |
| Each TiKV member | Storage commands | 43 counter samples |
| Each TiKV member | Command keys read | 43 histogram pairs |
| Each TiKV member | Command keys written | 43 histogram pairs |

Each selected family had zero new endpoint-only samples and zero dropped
samples. Coverage is complete for these selected endpoint label sets; selection
remains partial across all backing-server metrics. Histogram counts measure
observations, key sums measure keys, and duration sums measure cumulative
observed seconds. The scheduler-stage increment paths remain unverified.
The parser uses exact arithmetic on served tokens; exporter rounding remains.

These captures enclose initialization, creation, partial payload writes,
observers, background activity and forced termination. They are separate
non-atomic endpoint captures. This report supplies no backing metric rates,
per-phase amplification ratio, pure lock-wait duration or physical disk IOPS.
The captured metric roles do not provide a RustFS I/O or CPU measurement.

### Exporter process CPU

| Captured role | Whole exporter process CPU increment |
| --- | ---: |
| TiDB | 519.14 CPU-seconds |
| TiKV 1 | 327 CPU-seconds |
| TiKV 2 | 156 CPU-seconds |
| TiKV 3 | 161 CPU-seconds |

Twenty-two offline CPU controls and all four bounded role derivations passed.
Each role had one finite CPU counter series with complete selected coverage.
The mandatory process-start gauge guard matched full label sets and served
values, and the external captured role identity was unchanged. The increments
are exact as served; exporter rounding remains.

These process-counter windows enclose initialization, namespace creation,
partial payload population, observers and background work. They differ from
the namespace-only window of the 1,140.354429 worker CPU-seconds above.
No causal CPU comparison, CPU utilization, aggregate CPU total, per-phase
amplification, physical IOPS or metric rate is supplied. RustFS process CPU
remains unmeasured.

## Termination and qualification limits

The owned run ended after 597.223832625 seconds with signal termination
(`-15`). The receipt confirms original-child reap, observed process-group
absence, pipe EOF and unchanged watched source. It retains an unresolved
post-terminal KILL error (`EPERM`) and an unpassed named case. The completed
namespace counter subset is independently checked; the whole run did not pass.
Graceful shutdown, cleanup of backing test data and fresh reopen durability
were not qualified by this run.

The RSS figure is a sampled sum of available live process-group members, with
21 unavailable member observations and a maximum sampling interval of about
151 ms. It is not an OS lifetime peak or total laptop/backend memory use.

The [sanitized observations](observations.json) retain exact counter values,
finite metric coverage and source hashes. No steady I/O rate at 100 Drives,
10,000-client capacity, cross-host capacity, allocation count, physical NVMe
IOPS, native mounted CLI end-to-end behavior or power-loss durability is
established by this diagnostic. Native mounted CLI end-to-end qualification
remains outstanding. The smaller release control and this debug setup run have
different scopes and cannot be compared as a steady-throughput improvement.
