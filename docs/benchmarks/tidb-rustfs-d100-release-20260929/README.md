# TiDB/RustFS release run at 100 drives

This local release run used ten QUIC server processes, 100 connected clients,
100 separate Drives, 50 Partitions and 1,000 files per Drive. TiDB provided
filesystem metadata and RustFS provided immutable blobs. Authentication used
the signed local fixture. This was a Rust service harness, not an operating
system mounted CLI test or a cross-host deployment.

**The run failed during payload population.** It reached no sustained workload
stages and no fresh backend reopen oracles. The latest completed combined service
measurement is the [strict ten-drive control](../tidb-rustfs-strict-d10-20260929/README.md).

## Completed namespace phase

The ordinary open/create/close path acknowledged 100,000 empty file creates and
200,000 control RPCs. Its recorded phase duration was 146.806 seconds, including
boundary observers. Dividing those quantities gives approximately 681 creates
per second for this population phase; it is not sustained data I/O throughput.
The matched, quiescent namespace boundaries contained no data read/write RPCs.

| Metadata JSON counter | API bytes |
| --- | ---: |
| Anchor returned | 442,780,582 |
| Anchor serialized | 221,749,200 |
| Inode records returned | 3,305,924,100 |
| Inode records serialized | 1,689,117,500 |
| Total | 5,659,571,382 |

These are six distinct provider API counters; the two whole-namespace JSON
counters were zero. The total is about 56.6 KB of returned/serialized metadata
per empty file created. It measures growing membership and directory records,
not physical disk writes, allocator bytes, or TiKV replication traffic.
Frontend process CPU increased by 230.27958 seconds across the eleven captured
controller/worker processes during the namespace interval. It includes observer
and background work; overlapping async spans are not added to this CPU total.

## Incomplete payload and cleanup

The mixed population would require 628,326,400 payload bytes: per Drive,
990 files of 4 KiB, nine of 128 KiB and one of 1 MiB. Data blocks use explicit
file/block/generation identity plus seeded pseudorandom content.

The payload phase hit its inherited 600-second deadline after approximately
600.902 seconds. The enclosing work budget was 1,800 seconds. The controller
reported `inherited workload phase deadline; partial work incomplete`.
It retained 523,159 acknowledged RPCs and 100 uncertain requests across the
whole run. These counts are not completed payload or sustained-cycle rates.
The journal's completed-population byte total stayed zero because it was never
published; partial payload writes were not zero.

All ten workers exited with code 101 and reported an unproven replica drain and
an unclosed provider context. The parent reaped its child and confirmed process
group absence, EOF and unchanged source. Process settlement does not establish
successful provider cleanup or resolve uncertain writes. Cancellation can
quarantine an interrupted mutation; that fail-closed behavior remains required.

## Whole-window backend observations

Separate matched captures covered roughly 887 seconds, including background
time, setup, namespace creation, partial payload and cleanup. Cumulative Docker
CPU deltas were 708.00068 seconds for TiDB, 623.743075 seconds for RustFS, and
865.419845 seconds across the three TiKV containers. Both TiDB and RustFS had
two-CPU limits. These totals do not establish phase-exclusive CPU, peak
utilization, or the component limiting sustained throughput.

The captures matched all eight original container identities and all sixteen
before/after raw Docker bodies. Positive observed PIDs were stable across
captures; the original fixture receipt did not record PIDs. Four Prometheus
roles also matched with eight verified raw bodies. Docker disk-operation
counters were null for all eight roles. Guest block-byte accounting was present,
but no physical NVMe IOPS or per-phase backing amplification is established.

## Provenance and current limits

Measured source: `c65e78b0e25c2056f6824a385b05e3e5b9b50fc7`, clean release build.
Binary SHA256: `838bd08d50a8bc15f4071de0ee1cd8c64e07aa3bc32a900ed8ea8fb83110a7a3`.
Terminal SHA256: `95deb9c7e8279bd3e909ec7e51216b378d88cb4f04d6102fb26b8f9564870ad0`.
Failed owner receipt SHA256: `2b8160924aaa5a56c816dc53f755348736b12eab3d446e8c15566d07528baaad`.
Private offline observation SHA256: `d66fd36c0f6c48c3f946c2f3089e81e0269efbab9fbeb9f7a44466bcc6e4f29d`.
Its root execution passed the actual failed-run input and fifteen mutated
negative controls. It preserves sixteen unavailable timed cells and zero fresh
oracle passes; backend counter windows remain separate from namespace counts.

This run does not qualify sustained read/write performance at 100 Drives,
10,000-client capacity, mounted Linux/macOS behavior, external OIDC, graceful
cleanup, power-loss durability, or a causal speedup against the debug runs.
Subsequent cache and streamed-read changes are not measured by these results.
