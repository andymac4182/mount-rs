# Remote client scaling

Target: 10 servers and 10,000 clients, mostly one Drive per sandbox, with both mostly idle connections and continuously active clients. Separate Drive results and shared Drive controls must be interpreted separately.

## Method

The runner uses production TLS QUIC sessions, dispatcher authorization and SDK storage drivers. All measurements enable opt-in per-inode publication (`inode_updates: true`). Each active client keeps an independent 128 KiB file and issues random 4 KiB reads or overwrites, with one outstanding operation. Read bytes are checked during measurement. Writes update an expected byte ledger; storage is reopened after all original coordinators shut down.

Each stage has three seconds of warmup and fifteen seconds of measurement. Throughput includes draining in-flight requests; p99 values are power-of-two microsecond upper bounds. Audit logging and opt-in allocation/I/O profiling are enabled. Setup admission is bounded to ten clients at once. These are single observations, not confidence intervals.

The shared Drive control prepublishes empty file definitions through the provider's public bound publication API. It verifies every stored file against its ledger using a coherent inode snapshot and actual blob reads, then samples up to 64 fresh SDK reads. This avoids quadratic verification from reopening a large namespace for every file. An actual TiDB negative oracle test confirms that a wrong ledger fails verification. Ordinary online create results and the earlier verification deadline failure are retained separately.

The separate Drive fixture eagerly opens every Drive on every server, matching CLI startup. Each sandbox has its own grant and token claim; setup checks that another sandbox's Drive returns `EACCES`. Every active file is checked through a fresh SDK driver. Inactive clients issue a root stat on their own registered Drive. Catalog definitions are compact fixture descriptors rather than complete CLI configuration documents; catalog byte costs therefore understate larger production definitions. The fixture exercises grants, but does not exercise definition-change fencing. Authentication itself is synthetic; signed OIDC correctness is covered by the existing integration tests, and external issuer capacity is outside this test.

All coordinators and clients run in one Rust process on an M4 Pro Mac with 48 GB RAM. Actual TiDB 8.5.7, PD and TiKV run in a local Linux ARM64 Docker VM with 14 CPUs and 8.32 GB RAM, using the explicitly selected single-node topology. This does not establish replicated durability, cross-host network behavior or production horizontal capacity.

## Shared Drive control

| Connected clients | Active clients | Servers | Reads/s | Writes/s | Read p99 upper | Write p99 upper |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 2,500 | 2,500 | 10 | 2,204 | 789 | 2.10 s | 4.19 s |
| 5,000 | 5,000 | 10 | 2,147 | 732 | 4.19 s | 8.39 s |
| 10,000 | 10,000 | 10 | 2,073 | 751 | 8.39 s | 16.78 s |
| 10,000 | 100 | 10 | 2,111 | 797 | 65.5 ms | 262 ms |

All rows passed their byte oracle and had zero inode CAS conflicts. The all-active 10,000-client run took 1,175 seconds, including 1,046 seconds of client setup. This is a contention control, not the requested production Drive layout.

At 1,000 active clients, measured read gate wait averaged 444 ms per operation versus 453 ms of service dispatch: approximately 98% was gate wait. Independent backend inode publication eliminates revision conflicts but the per-coordinator filesystem gate still queues shared Drive work. The write publication gate does not emit the same wait event; an exact write queue percentage is not measured.

At 10,000 active clients, process CPU averaged 2.85 cores for reads and 1.43 for writes. RSS at stage end was 2.92 GB and 3.39 GB respectively, including clients, servers and allocator retention. Rust allocations remained about 435 per read and 693 per write. These are process totals, not isolated server allocation counts.

TiDB executed approximately 3 SQL statements per read and 10 per write. TiKV cgroup counters recorded about 2.95 VM block writes and 25.2 KiB written per successful 4 KiB overwrite. Counters include background work and observer windows. Warm reads mostly hit cached storage. VM cgroup I/O is not the physical Mac SSD's IOPS, so these results do not test a 100,000-IOPS SSD claim.

## Separate Drives

| Sandbox Drives / clients | Driver replicas | Reads/s | Writes/s | Read p99 upper | Write p99 upper |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 10 | 100 | 1,869 | 683 | 8.19 ms | 32.8 ms |
| 25 | 250 | 2,831 | 1,062 | 16.4 ms | 65.5 ms |
| 50 | 500 | 3,222 | 1,432 | 32.8 ms | 65.5 ms |
| 100 | 1,000 | 3,074 | 1,742 | 131 ms | 131 ms |
| 250 | 2,500 | 1,883 | 1,407 | 1.05 s | 1.05 s |

All rows used ten coordinators, all clients active, and passed fresh SDK byte verification with zero inode CAS conflicts. The first four rows initialized coordinators sequentially; the 250-Drive row provisioned volumes first and started coordinators concurrently. Startup is excluded from throughput in both cases.

The mostly idle separate-Drive case also passed: 250 Drives replicated on ten coordinators, 250 connected clients, and ten active clients measured 1,323 reads/s and 314 writes/s, with p99 upper bounds of 16.4 ms and 262 ms. All 250 clients checked authorization to their own Drive; all ten active files passed fresh-driver byte verification. These are wire clients exercising registered Drives, not 250 native OS mounts. The mostly idle 10,000-Drive case remains untested because the initialization ramp failed first.

The 100-Drive write result is approximately 2.27 times the shared-Drive 100-client control's 766 writes/s. Read gate wait remained around a microsecond in the separate-Drive cases. The backend can publish independent files concurrently; shared filesystem scheduling no longer dominates these requests.

Catalog processing now grows with Drive count. Each request loaded and decoded 2,037 bytes at ten Drives, 19,407 at 100, and 49,107 at 250. Process allocations grew from 551 per read / 798 per write at ten Drives to 4,236 / 4,482 at 250. At 250, read authorization averaged 23.9 ms per request, including 11.4 ms of catalog pool wait. These durations include waiting and must not be summed as CPU time.

Each TiDB metadata and blob provider constructs a private pool. At 100 Drives, a stage snapshot observed 3,188 TiDB client sessions; at 250, both stage boundaries observed 7,750. TiDB RSS reached about 3.8 GB at 250 Drives. The 250-Drive datastore windows consumed approximately 3.18 / 3.92 TiDB CPU cores and 0.82 / 1.08 TiKV cores for reads / writes. SQL counts remained 3 / 10 per successful operation. Whole-catalog processing and per-Drive pools are additional amplification surfaces even after per-inode publication removes revision conflicts.

Concurrent initialization of unprovisioned new Drives failed with `ESTALE` at ten and 250 Drives. The provider correctly fences obsolete namespace APIs after MRC4 enrollment; startup does not yet recover when another initializer wins that transition. This is an outstanding startup defect. Preprovisioning is an explicit benchmark condition, not a production fix.

The provisioned 500-Drive point failed before timed I/O, including two repeats in fresh clusters. Sanitized provider errors report connections closing during connect and inode snapshot reads. Shutdown succeeded. The captured TiDB container state was running, exit zero, and not OOM-killed. Startup boundary snapshots show post-cleanup memory, not peak sessions or peak RSS; they cannot establish a root cause. This is a local harness boundary, not evidence that production TiDB itself is limited to 500 Drives. The ramp stops at failure, so larger separate-Drive points are untested.

At 250 active Drives, each 4 KiB read received about 15,083 QUIC UDP bytes and each write transmitted about 15,130: roughly 3.7 times the application payload. JSON byte arrays account for much of the expansion. Client counters recorded about twelve receive operations per read and eleven transmit operations per write, with zero packet loss or congestion events in these windows. This is observable network and syscall amplification; it does not show link saturation.

The complete catalog is capped at 8 MiB across all Partitions, Drives, grants and issuer policies. A real 10,000-Drive configuration must also be checked against that limit; compact fixture definitions do not qualify production document sizes.

## Next changes supported by these measurements

1. Share bounded datastore pools across Drive contexts on each server, with service-owned pool shutdown. Retain per-volume keys, backing authority and verified pessimistic sessions. Closing one Drive must not disconnect other Drives' storage.
2. Remove repeated full-catalog decoding and validation. A revision-checked immutable snapshot cache or targeted authorization records must preserve per-operation revocation, catalog file identity checks, fail-closed metadata errors and handle invalidation on revision change.
3. Make concurrent first-time MRC4 initialization recognize a peer's exact verified authority without weakening legacy fencing or replaying ambiguous mutations.
4. Use an explicitly versioned binary representation for I/O bytes to reduce JSON expansion and parsing allocation. This is separate from datastore CAS improvements.
5. Narrow shared-Drive filesystem scheduling after validating read/structure ordering. This helps the contention control and genuinely shared production Drives; it is not the principal gate in the separate-Drive results.

The 10,000-client shared-Drive connection result is established locally. The requested 10,000 separately mounted sandbox Drives on ten servers is **not yet qualified**.

## Reproduce

```sh
CARGO_TARGET_DIR=/private/tmp/mount-rs-client-scaling-target \
MOUNT_RS_TIDB_TOPOLOGY=single \
MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES=1 \
MOUNT_RS_REMOTE_SATURATION_SEPARATE_DRIVES=1 \
MOUNT_RS_REMOTE_SATURATION_PROVISION_DRIVES=1 \
MOUNT_RS_REMOTE_SATURATION_CONNECTION_LIMIT=1024 \
MOUNT_RS_REMOTE_SATURATION_SETUP_CONCURRENCY=10 \
MOUNT_RS_REMOTE_SCALING_SERVERS=10 \
MOUNT_RS_REMOTE_SCALING_CLIENTS=10,25,50,100,250,500,1000,2500,5000,10000 \
MOUNT_RS_TRACE_ALLOCATIONS=1 \
MOUNT_RS_TIDB_COMPOSITION_COMMAND='exec sh scripts/bench-remote-scaling.sh tidb /private/tmp/separate-drive-results' \
sh scripts/test-tidb.sh
```

`MOUNT_RS_REMOTE_SATURATION_PROVISION_DRIVES=1` opens and closes each new Drive before concurrent coordinator startup. Provisioning time is recorded separately. Without it, concurrent first-time initialization currently fails with `ESTALE`; this is retained as a startup defect rather than a capacity result.

For mostly idle clients, set `MOUNT_RS_REMOTE_SATURATION_ACTIVE_CLIENTS=100` and start at 100 connected clients. For the shared Drive control, unset `MOUNT_RS_REMOTE_SATURATION_SEPARATE_DRIVES` and set `MOUNT_RS_REMOTE_SATURATION_PRESEED=1` and `MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY=1`.

`scripts/summarize-remote-scaling.py` accepts a results directory and `--output summary.json`. It rejects failed verification and zero-success/failed stages from ranked results. Canonical artifacts, selected datastore counters and failed workloads are retained in [the evidence directory](benchmarks/remote-client-scaling-20260924/). Raw metric series are omitted from committed evidence; compact profiles record their source SHA-256.

## Regression gates

The full workspace suite passed 1,256 tests with zero failures and 104 ignored tests across 155 targets. Strict workspace Clippy, allocation-profiled service/CLI Clippy, formatting, shell syntax and six datastore counter-parser tests passed. Default and configured connection admission tests cover overload refusal and slot recovery. The final actual TiDB fixture also passed its two terminal ambiguous-commit tests. Exact commands and scope limits are recorded in [verification.json](benchmarks/remote-client-scaling-20260924/verification.json). No new formal proof or native Linux/FDB scaling result is claimed for this harness change.
