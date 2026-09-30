# TiDB with filesystem blobs versus RustFS

The new durable filesystem block provider completed the same native workload as RustFS. On this macOS/APFS laptop it was slower for reads and markedly slower for durable writes. Removing HTTP did not yield a throughput improvement in this pair.

## Configuration

Both arms used the identical instrumented release executable at source `7f4da840b815b24cc65ab37c93d7146a43f72775`: Rust 1.95.0, optimization level 3, allocation/I/O/resource profiling and SDK runtime enabled, no incremental compilation or debug assertions. The archive binds all 646 build source files; 177 pure checks passed, six native runtime entry points were ignored during those checks. The live controller was then run explicitly once per backend.

Ten native server processes, ten real loopback QUIC clients, ten Drives across five Partitions, and 1,000 files per Drive. Each initial dataset has 10,000 files and 62,832,640 bytes: per Drive, 990 files of 4 KiB, nine of 128 KiB and one of 1 MiB. Active reads/overwrites use 4 KiB. One client is active in mostly-idle mode and ten in all-active mode, with depth one per client. Each backend completes eight patterns in both modes, nominally five seconds per cell; the denominator includes draining final requests.

TiDB 8.5.7 uses the preserved Docker fixture. RustFS 1.0.0 uses a fresh strict-policy fixture capped at two CPUs, 2 GiB and 512 PIDs. Filesystem blobs use a private local APFS root shared by all ten server processes, with descriptor identity checks, atomic publication and configured durable barriers. Filesystem runs first; RustFS runs second. Each has a separate namespace and full fresh initial/final content oracle.

RustFS retains its generic object-store adapter RAM cache; filesystem has no such adapter/cache, and kernel filesystem caching is present but unisolated. The composed RAM/disk/QUIC peer cache is unconfigured in both arms. HTTP, Docker VM traversal, RustFS processing and adapter caching change together; this is one ordered deployment comparison, without confidence intervals or isolated causal attribution.

## Complete cluster cycles per second

Open/Close are included; basic read/overwrite cycles contain three acknowledged RPCs and churn four. These are aggregate cluster rates, not per-Drive rates or physical IOPS. Exact counts and rational denominators are retained in the data.

| Pattern | Idle RustFS | Idle filesystem | Active RustFS | Active filesystem |
|---|---:|---:|---:|---:|
| sequential_read | 151.051 | 144.810 | 673.811 | 555.995 |
| random_read | 170.741 | 141.253 | 692.592 | 606.550 |
| sequential_overwrite | 56.501 | 28.027 | 209.548 | 78.068 |
| random_overwrite | 58.244 | 28.207 | 208.629 | 81.318 |
| mixed | 85.829 | 46.658 | 323.084 | 133.615 |
| hot_file | 69.478 | 35.566 | 227.638 | 99.626 |
| append_truncate | 69.693 | 37.523 | 241.251 | 117.512 |
| churn | 13.062 | 11.037 | 59.130 | 57.969 |

## Latency and amplification

Inclusive worker span means in milliseconds. Spans overlap and cannot be added into an exclusive latency budget; histogram percentile bounds are retained in `summary.json`.

| Active pattern | RustFS block span | Filesystem block span | RustFS COMMIT | Filesystem COMMIT |
|---|---:|---:|---:|---:|
| sequential_read | 1.316 | 3.460 | — | — |
| random_read | 0.855 | 3.437 | — | — |
| sequential_overwrite | 26.185 | 68.093 | 5.223 | 24.691 |
| random_overwrite | 26.133 | 64.254 | 5.331 | 24.643 |

Reads remain exactly three classified SQL submissions and three known SELECT rows per complete cycle in both arms. Overwrites remain five SQL submissions, four known SELECT rows, one COMMIT and one SDK block PUT. Changing blob storage did not reduce metadata SQL amplification. Churn remains 44 SQL submissions and about 7,070–7,073 known SELECT rows/cycle; that namespace cost remains a separate optimization seam.

Filesystem PUT includes descriptor walks, validation, hashing, an owned blocking task, file synchronization, atomic publication, directory synchronization and macOS device barriers (`F_FULLFSYNC` before publication and after directory synchronization). The 64–68 ms measured span localizes a substantial cost to this path, but there are no separate internal queue/hash/fsync spans. It is not a measured SSD service time. TiDB COMMIT was also slower in the filesystem arm; the runs share the host disk and background state, so these data do not prove intrinsic TiDB contention or backend saturation. No durability barrier was weakened for this comparison.

## CPU, allocations, memory and network

Worker totals include observation/background work; CPU excludes TiDB, Docker and collector processes. Allocations are System Rust allocator allocation plus reallocation requests, not metadata-only allocations.

| Active pattern | RustFS allocations/cycle | Filesystem allocations/cycle | RustFS CPU ms/cycle | Filesystem CPU ms/cycle |
|---|---:|---:|---:|---:|
| sequential_read | 1304.008 | 929.694 | 1.513 | 3.757 |
| random_read | 1245.817 | 892.209 | 1.439 | 3.798 |
| sequential_overwrite | 2316.458 | 2926.846 | 2.377 | 8.474 |
| random_overwrite | 2321.613 | 2925.837 | 2.485 | 8.372 |

Reads used fewer Rust allocation requests with filesystem; overwrites used more. Neither result establishes zero-allocation metadata or an isolated provider allocation cost.

| Process lifetime peak RSS | RustFS MiB | Filesystem MiB |
|---|---:|---:|
| Largest server | 79.484 | 74.250 |
| Controller | 145.188 | 89.891 |

These are lifetime process peaks, not simultaneous cluster peak memory or per-cell peaks. UDP datagrams, bytes, syscall counts and loss counters are retained per cell in both full projections. All active-cell worker QUIC loss counters are zero; that does not prove wire or host NIC saturation. Filesystem HTTP/cache/cgroup observations are explicitly `not_configured` with null counters, not measured zeros.

RustFS guest cgroup observations cover 382.343 seconds of the whole native benchmark, including setup, content oracles, background work and writeback: 227 read operations / 974,848 bytes and 62,275 write operations / 326,025,216 bytes. These guest block events are not host SSD or TiKV physical IOPS, and cannot be assigned to individual timed cells. No qualified host-device or TiKV physical IOPS measurement is available for either arm.

## Correctness and delivery

Both arms passed every workload cell, full initial/final payload/size/EOF/membership checks over all 10,000 files, 100 cross-server payload pairs, Partition and sibling scope denials, revocation checks and clean retirement of all ten workers. Failed and uncertain requests were zero. Final file contents follow each arm’s own acknowledged ledger; duration-based writes leave different final byte totals.

Both external supervisors qualified the run and settled all owned processes without abort, forced stop or automatic replay. They continuously sampled the 64 GiB host free-space floor. The original eight containers and six volumes were preserved. Filesystem data remains in its retained owned root; the control RustFS fixture was stopped with its data retained. The failed initial sandbox Docker-access preflight produced no filesystem dataset or workload and is excluded from performance evidence.

Local checks: 25 provider tests including permission, symlink, corruption, concurrency and injected barrier failures; 28 filesystem-filtered SDK/CLI checks; strict Clippy for provider/SDK/CLI and the native release target; formatting; 177 archived pure checks; both explicit native controller runs. Linux runtime, real process-crash/power-loss recovery, external issuer, mounted Linux/macOS filesystem traffic and final CI/merge are separate qualifications.

## Deployment tradeoffs and follow-up

The filesystem backend is a block-only option for split stores: TiDB still owns metadata. It removes the blob HTTP dependency and gives direct local file access. Each server must see the same authoritative bytes: this test shares one local root. Separate server SSDs require shared authoritative storage or replication/recovery before they can replace S3. The provider has no garbage collection; admitted and retained objects are not swept.

The measured write cost makes batching durable publication a useful next investigation, with acknowledgments still waiting for the required barriers. Internal blocking-queue/hash/file/directory/device timing and a Linux filesystem run are needed to isolate that cost. Simply replacing RustFS did not make this workload faster. Ten depth-one clients and short cells do not establish a saturation curve or the 10,000-client / 10,000-Drive / 5,000-Partition production target.

## Artifacts

- `summary.json`: all 32 cells, exact rational rate inputs, inclusive latency bounds, SQL/allocations/CPU/network tables and qualified correctness/resource summaries.
- `filesystem-observations.json.gz` and `rustfs-observations.json.gz`: complete allowlisted projections, metric frame bindings and provenance.
- `qualification.json`: actual build, local validation and runtime supervisor receipt hashes.
- `verify.py` and `verification.json`: the exact executed independent verifier and its successful receipt; all 32 cells checked and three deliberate corruptions rejected.
- `extract-filesystem.py` and `extract-rustfs.py`: the exact executed pinned allowlist extractors.

The extractors/verifier used retained private native evidence and a hash-pinned private input spec. Raw credentials, fixture identities and absolute private paths are excluded from committed artifacts. The full public projections and executable verification source are retained for review; running the raw-evidence controls requires the corresponding private captures.
