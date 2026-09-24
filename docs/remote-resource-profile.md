# Remote drive CPU, memory and network profile

Measured 2026-09-24 on an M4 Pro laptop. Release builds, 100 QUIC clients,
10 service instances in one process, 4 KiB operations, 32 blocks per file,
100 files, depth 1 per client, 3 seconds warmup and 15 seconds measurement
including drain. Audit logging is enabled. These are warm-cache diagnostics,
not disk saturation or independent host results.
The baseline uses production paths at `fddad518` with the diagnostic resource
instrumentation; the final variant includes the immutable snapshot and harness
changes in this report.

## Implemented read refactor

Concurrent MRC2 reads formerly cloned the entire namespace to access one file.
They now retain an immutable `Arc<Namespace>` and borrow that inode's layout.
Capturing a live inode's metadata allocates nothing. Legacy, delegated and
orphan read paths retain their existing owned layout behavior. Both metadata
freshness checks remain, as do sticky failure, backing and permission checks.

The load harness also reuses its expected read buffer and compares response
bytes directly instead of constructing another JSON array. Measurements
include this verification; the combined result is not solely a production
filesystem improvement.

| SQLite read variant | Reads/s | Rust allocations/read | Requested allocation bytes/read | User CPU us/read | System CPU us/read |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline, allocator disabled | 6,322 | unavailable | unavailable | 394.84 | 632.13 |
| Baseline, sharded allocator | 6,338 | 3,981.18 | 1,346,787 | 408.86 | 638.10 |
| Copy only selected layout | 6,398 | 224.19 | 904,268 | 275.06 | 668.40 |
| Immutable snapshot and reusable oracle | 6,354 | 185.14 | 765,076 | 252.84 | 643.10 |

The combined change cuts allocations 95.35%, requested byte volume 43.19%,
user CPU 38.16% and total CPU 14.42% against the matched instrumented baseline.
Throughput is essentially unchanged. These are individual runs, not confidence
intervals. System CPU remains the largest measured component. The selected
layout intermediate isolates the main filesystem allocation improvement.

Source and evidence: `filesystems/mount-rs-chunked/src/lib.rs`,
`crates/mount-rs-service/tests/quic_tidb_saturation.rs`, and the JSON reports in
`benchmarks/remote-resource-profile-20260924/`.

## Remaining amplification

Final SQLite reads do one 4 KiB block GET and two conditional metadata checks;
unchanged checks avoid returning the whole namespace. The catalog is still
loaded and validated for each request. Aggregated stage timings include waits
and overlapping tasks and must not be interpreted as exclusive CPU time.

Each read receives approximately 15,049 QUIC UDP payload bytes in 11.22
datagrams and sends 311 bytes in 4.53 datagrams. No QUIC loss or congestion
events were recorded. Numeric JSON byte arrays expand the 4 KiB payload to
about 3.67 times its size before IP/UDP headers. A new QUIC stream and generic
JSON trees per RPC also remain. Sender and receiver totals are not added to
double-count a transfer.

Final SQLite writes complete at 278.5/s and request about 5.43 MB of Rust
allocations per successful 4 KiB write (20,394 allocations). They publish
1.712 whole namespaces per success and serialize 766,317 namespace bytes per
success. This is still the main structural write amplification. Retries also
write immutable blocks; the 1.712 block PUTs per success are not disk IOPS.

## Memory and profiler limits

A separate final read run records 17.66 MB live Rust allocations, 15.80 MB
SQLite C heap and 370.41 MB resident memory. SQLite's lifetime heap peak is
19.06 MB. The remaining RSS is not attributed by these counters: retained
allocator pages, stacks, mappings and other foreign allocations require
additional attribution. Reduced churn does not establish reduced RSS.

An immutable snapshot can retain an entire old namespace while a slow read
finishes. Several readers spanning publications can retain several versions;
the last owner may perform destruction. A race test proves the old version
stays valid through the read and is released afterwards.

The test binary's allocator wraps `System` with 128 thread-assigned counter
shards. Counters cover clients, servers, verification and background tasks.
Foreign C allocations are excluded and SQLite is reported separately via its
read-only allocator counters. Allocation byte volume counts requests,
including reallocations, rather than unique data or live memory. Cross-thread
frees use signed counters; summed snapshots can overlap background activity.
Allocator-disabled reports use null counts. An initial shared-counter
profiler badly distorted throughput and was excluded from comparisons.

`getrusage` measures whole-process CPU and faults; it does not attribute CPU to
individual layers. Linux block counts and Darwin block counts do not establish
physical SSD IOPS. Owned macOS stack samples show `stat`, `sendmsg`, JSON
processing and allocation among active stacks, alongside many waiting threads.
Collapsed sample counts are not exclusive CPU percentages. Hardware counter
recordings failed with a kperf lock error or timeout; retained status files
are diagnostic failure evidence, not usable hardware profiles.

## TiDB datastore attribution

The final TiDB v8.5.7 single-node diagnostic completes 23,136 reads at
1,537/s, with zero failures, fresh visibility verification and clean shutdown.
The Rust client/service process uses 398.2 us user plus 633.4 us system CPU
per read and makes 290.0 Rust allocations/read. No matched pre-refactor TiDB
resource run was taken, so this does not establish a TiDB throughput change.

Datastore counters span 15.839 seconds (a slightly wider observer window than
the service stage), including background work and observer probes:

| Component | CPU seconds | Average CPU cores over observer window | End resident MB |
| --- | ---: | ---: | ---: |
| PD | 5.95 | 0.376 | 151.6 |
| TiDB | 29.08 | 1.836 | 754.7 |
| TiKV | 11.00 | 0.695 | 2,986.7 |

There are 69,412 SELECT statements, approximately three per successful read,
and approximately three TiDB-to-TiKV requests per read. TiKV reports about
4,342 GET bytes plus 201 iterator bytes per read, retaining the previous
covering-index improvement. Observed cgroup block reads are zero across the
three containers; PD and TiKV record 43 and 48 background block writes.
These are warm Linux VM observations, not physical Mac SSD IOPS.

TiDB container traffic averages 5,311 received and 6,950 sent bytes per read;
TiKV receives 2,091 and sends 4,607. These counters include SQL, inter-service
traffic and probes. Do not add both sides of the same transfer to claim unique
network volume. End Go heap allocations are 61.1 MB for PD and 259.8 MB for
TiDB. Missing TiKV allocator gauges are unavailable, not zero.

The default durable topology was rejected before startup because Docker has
8.32 GB while the harness requires 10 GiB. This measured topology uses one PD,
one TiKV and one TiDB instance and provides no replicated durability evidence.
Raw service and datastore reports are `tidb-final.json` and
`tidb-datastore-metrics.json` in the evidence directory.

## Native FoundationDB attribution

The Linux ARM64 native FoundationDB read lane completes 101,887 reads at
6,786.8/s with zero failures, all 100 files freshly verified, and terminal
network shutdown passing. Provider and delegation contracts also pass.
The new resource module builds and runs on Linux as well as macOS.

Whole client/service process CPU is 475.40 us user plus 147.59 us system per
read, averaging 4.228 cores. It records 229.68 allocations and 35 reallocations
per read, requesting 771,620 bytes/read. RSS moves from 337.51 to 352.90 MB;
the lifetime peak is 638.38 MB. Foreign FoundationDB C allocator memory is
unavailable to the Rust allocator wrapper.

QUIC receives 15,049.73 bytes/read and sends 218.82 bytes/read, with no loss
or congestion events. Native datastore counters report about 2.978 KV reads
and 4,111.81 returned bytes/read. Counters have export lag and background work;
the exact filesystem profile records two conditional manifest checks and one
block GET, with no namespace copy, return or serialization events for reads.

Native process CPU gauges are 0.106 and 0.429 cores at the boundaries; RSS
gauges are 221.28 and 372.55 MB. Network gauges are rates exported at those
boundaries, not measured stage averages. Linux VM disk rates are about
0.133 reads/s and 86.812 writes/s, including background work; neither proves
physical host SSD performance. The single-node topology provides no replicated
durability evidence.

This lane runs the Rust process inside Linux Docker, while SQLite and TiDB
client/service processes above run on macOS. The much lower system CPU is
evidence that the OS/runtime path matters, but these runs cannot isolate that
effect from backend and topology differences or rank datastore performance.
Raw reports are `foundationdb-read.json` and
`foundationdb-datastore-counters.json` in the evidence directory.

## Structural next steps

1. Split metadata into independently revisioned inode and directory records.
   Updating one file's size and block references should compare that inode's
   revision, not the whole drive's namespace. Creation, deletion and rename
   need transactions across the affected records; same-file writes still
   require coordination. Persisted format versioning, migration and provider
   capability checks must precede activation. Do not simply remove the global
   revision check: that would permit lost updates with the current document.
2. Introduce typed I/O messages and a negotiated byte payload encoding, retaining
   existing protocol compatibility. Avoid numeric `Value` arrays and generic
   intermediate deserialization trees. Preserve timeout and uncertain-write
   behavior: a transport fallback must never silently replay a write.
3. Add caller-owned read buffers and reusable framing slots through the block,
   filesystem and transport interfaces. Measure boxed async futures and QUIC
   stream creation before changing concurrency management.
4. Reuse validated immutable catalog snapshots with a fresh revision and backing
   identity check. Authorization revocation must stay fresh; a TTL cache would
   change the security contract.

Only the read snapshot and harness refactors above are implemented in this
change. Per-inode publication and the protocol/buffer changes remain follow-up
work. Near-zero allocations for steady-state data I/O is a target, not a claim
about the current 185 allocations per complete verified read.

## Reproduction

Validation for this checkpoint: workspace formatting and strict Clippy pass
with `mount-rs-service/allocation-profiling`; the workspace all-targets suite
passes 1,238 tests with 101 environment/diagnostic tests ignored across 153
targets. The chunked library's dedicated run including its ignored read-copy
oracle passes all 57 tests. Five datastore observer tests pass. Actual SQLite,
single-node TiDB and native FoundationDB runs verify all 100 files without
workload, verification or cleanup errors. Existing bounded formal proofs were
not rerun or extended for this snapshot refactor; these results establish
local regression and provider behavior, not cross-host or power-loss durability.

Use `scripts/bench-remote-resources.sh sqlite` or `tidb`, setting
`MOUNT_RS_RESOURCE_OUTPUT_DIR` to a retained writable directory and
`MOUNT_RS_TRACE_ALLOCATIONS=1` for allocation counters. Set
`MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=read` for reads only. Builds and timed
workloads must run serially. The default TiDB harness requires capacity for a
durable topology; `MOUNT_RS_TIDB_TOPOLOGY=single` is explicitly diagnostic.
FoundationDB's native harness forwards the optional resource/allocation
features and retains CPU/memory/network gauges as point-in-time observations,
not integrated counters. PgLite's existing concurrent-client protocol failure
prevents a qualified 100-client service comparison; see
`remote-io-amplification.md` for prior datastore qualification boundaries.
