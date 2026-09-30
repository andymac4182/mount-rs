# TiDB + RustFS: 100-client scale attempt

This is a **failed scale attempt**, alongside the completed [10-client benchmark](README.md). It does not qualify 100-client throughput or the production target.

## Source and workload

The run used ten actual server processes, 100 connected clients, 100 Drives across 50 Partitions, and 1,000 varied files per Drive. The planned initial corpus was 100,000 files / 628,326,400 bytes. All eight patterns in both activity modes, complete fresh initial/final byte oracles, and cross-node read-and-close checks remained required.

Measured checkout `9dcf0740d81142a96e22b9c41a6571c0f09fb2bb` contained only documentation changes since release build `6a60518d`. All 565 compiled source rows matched. A fresh Cargo no-run check reused the release binary (`fresh:true`); this was not a new compilation. Binary SHA256 was `f429f19580af557a4641e6fa6bb7f05542566363a3d5cfafa5a5ab141e57fc74`. The immutable archive and reviewed owner changed only the archive, manifest and HEAD pins; runtime deadlines and resource limits were unchanged.

## Observed result

| Boundary | Result |
|---|---:|
| Empty Drive initialization | 10.030 s |
| Worker setup | 0.902 s |
| Signed connections | 0.084 s |
| Create all 100,000 file names | 205.224 s |
| Completed timed cells | 0 / 16 |
| Completed fresh corpus oracle passes | 0 |
| Completed cross-node routes | 0 |
| Cumulative acknowledged RPCs | 542,825 |
| Cumulative uncertain RPCs at cancellation | 100 |
| Owner elapsed time, including settlement | 693.709 s |

The native controller ended `incomplete` during `online_payload`, with the exact error `host free disk below64GiB`. Its sampled minimum was **68,656,340,992 bytes (63.9412 GiB)**, 60.211 MiB below the 64 GiB reserve. This was a resource gate failure; no phase timeout or steady-state capacity limit was established. The source of the host disk-space decrease is not attributed by these counters.

Population's completion counter remained zero because the entire phase did not finish; this does not mean no payload was stored. Cumulative RPC acknowledgments include setup and partial population and must not be reported as timed logical cycles/s. Zero recorded failed RPCs does not resolve the 100 uncertain outcomes after cancellation. Full byte/EOF/membership correctness, cross-node and authorization workload checks remain unqualified for this attempt.

## Partial backend profile

These observations cover separate windows **during payload population**, before the failure. Read-only Docker statistics provided CPU, memory and network counters for the original owned TiDB/TiKV containers. Read-only `/sys/fs/cgroup/io.stat` observations provided actual Linux guest block operation counts for the three TiKV containers. Identity hashes, timestamps, raw counter values and derivations are retained in [scale-100-backend-observations.json](scale-100-backend-observations.json).

| Backend | Average CPU cores | Memory usage at window end, GiB | Receive, MB/s | Send, MB/s |
|---|---:|---:|---:|---:|
| TiDB | 1.340 | 1.408 | 2.564 | 4.812 |
| TiKV 1 | 0.241 | 2.741 | 0.637 | 0.174 |
| TiKV 2 | 0.654 | 2.720 | 1.683 | 2.055 |
| TiKV 3 | 0.267 | 2.567 | 0.546 | 0.132 |

Each CPU/network window lasted approximately 177.27 seconds, with slightly different per-container clocks. Memory is Docker's usage gauge including cache, not application RSS. CPU is consumed core-seconds divided by each observation window, not a throttle or saturation measurement. TiDB's configured CPU limit was two cores; this average alone does not prove that it was the bottleneck.

| Backend | Guest write operations/s | Guest read operations/s | Guest write MB/s |
|---|---:|---:|---:|
| TiKV 1 | 270.056 | 0.679 | 1.038 |
| TiKV 2 | 442.052 | 2.514 | 1.419 |
| TiKV 3 | 270.516 | 0.025 | 1.039 |

The guest I/O windows lasted 157.486–157.581 seconds. Their sum is approximately **983 guest write operations/s**, with differing clocks. It includes replication, compaction, logging, background activity and observer work. It is not a synchronized cluster sample, physical Mac SSD IOPS, a per-file amplification factor, or a RustFS measurement. Docker's empty serviced-operation arrays were unavailable, rather than zero; the separate cgroup counters supply the operation observations above.

Frontend observers recorded 89,325,172 audit log bytes for 542,825 events. The controller's lifetime RSS high-water mark was 109,477,888 bytes; the sum of individual controller/worker peaks was 985,169,920 bytes. Those process peaks were sampled separately and exclude Docker guests, so they are not simultaneous total RAM use. Rust allocator instrumentation was disabled; no allocation-free result is claimed.

## Settlement and evidence

All ten workers were reaped without forced termination, but exited 101 and did not report clean filesystem context closure. The native receipt retained eleven cleanup errors. The outer owner stopped the new RustFS container and collector, with no finish errors; the new RustFS container had no OOM kill or restart before stopping. These outer lifecycle observations do not establish clean native context shutdown.

Read-only before/after checks found all eight original fixture containers healthy with the same identities, mounts, ports and limits. No Docker control-plane mutation was applied to the original containers; the workload did persist owned metadata to the original TiDB fixture. Source pins remained equal, and the failed owned datasets were retained. No dataset absence or crash durability proof is claimed.

[scale-100-attempt.json](scale-100-attempt.json) retains the exact geometry, resource floor, terminal outcome, counters, source binding, worker settlement and receipt hashes. Owner receipt SHA256: `9121fd8fd7b4fb2dd7849d63adc924399a202697b4826808ac8f49f47629d371`. The completed 10-client comparison remains the only qualified throughput result for these changes in this report.
