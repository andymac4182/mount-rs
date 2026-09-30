# TiDB + filesystem after removing duplicate flush synchronization

The durable filesystem block provider completed all 16 native workload cells at source `bf6062cb4fc0b3dcbfde4532bba1b8c46266ab2f`. Aggregate active sequential reads reached **621.863 complete cycles/s** and sequential overwrites **77.026 cycles/s**. Replacing RustFS has not demonstrated a write-speed improvement on this macOS/APFS laptop.

## Configuration

Ten server processes and ten real loopback QUIC clients, ten Drives across five Partitions, 1,000 files per Drive. TiDB 8.5.7 retains metadata; all servers share one private APFS block root. Each initial corpus contains 10,000 files and 62,832,640 bytes (per Drive: 990 × 4 KiB, nine × 128 KiB, one × 1 MiB). Active basic I/O is 4 KiB, depth one per client. Mostly-idle mode has one active client; all-active has ten. Each pattern runs nominally five seconds and includes draining final requests.

Rust 1.95.0, optimized release with allocation, I/O and resource profiling plus SDK runtime; no incremental compilation or debug assertions. The immutable executable binds 646 build sources. Its ordinary checks passed 177 tests with six native entry points ignored; the live controller was then run explicitly. The filesystem provider has no adapter RAM cache. Kernel caching is present and unisolated; the composed RAM/disk/QUIC peer cache is unconfigured.

## Complete cluster cycles per second

A cycle includes Open, I/O and Close: three acknowledged RPCs for basic reads/overwrites and four for churn. Rates are aggregate cluster logical cycles, not per-Drive or physical IOPS. Exact rational denominators and counts are in `summary.json`.

| Pattern | Mostly idle | All active |
|---|---:|---:|
| sequential_read | 136.039 | 621.863 |
| random_read | 138.180 | 583.189 |
| sequential_overwrite | 29.194 | 77.026 |
| random_overwrite | 29.965 | 77.325 |
| mixed | 43.095 | 135.225 |
| hot_file | 37.719 | 89.753 |
| append_truncate | 42.728 | 121.684 |
| churn | 13.093 | 59.800 |

## Inclusive storage span means (ms)

These worker spans overlap and cannot be summed into an exclusive latency budget. Bucket percentile bounds are retained in the observations. PUT includes descriptor/authority checks, hashing, blocking dispatch, file synchronization, atomic publication, directory synchronization and macOS device barriers. There is no isolated SSD service-time measurement.

| All-active pattern | GET | PUT | Flush | TiDB COMMIT |
|---|---:|---:|---:|---:|
| sequential_read | 3.381 | — | — | — |
| random_read | 3.714 | — | — | — |
| sequential_overwrite | — | 68.328 | 1.578 | 32.638 |
| random_overwrite | — | 69.155 | 1.563 | 32.474 |
| mixed | 2.041 | 68.597 | 1.656 | 27.024 |
| hot_file | 2.532 | 78.017 | 1.793 | 31.475 |
| append_truncate | — | 61.267 | 1.836 | 24.924 |
| churn | — | — | 2.109 | 11.653 |

The code change removes a repeated root-directory sync from `flush`. Every acknowledged PUT already completes its required file, directory and final device barriers; flush still runs two authority checks in its owned blocking task. Novel, duplicate and racing PUT durability is unchanged.

The [earlier matched pair](../tidb-filesystem-paired-20260930/README.md) measured filesystem sequential overwrite at 78.068 cycles/s, PUT at 68.093 ms, COMMIT at 24.691 ms and flush around 10.375 ms. This run measures 77.026 cycles/s, PUT 68.328 ms, COMMIT 32.638 ms and flush 1.578 ms. The lower observed flush time did not translate into a clear total throughput gain. These are ordered runs with source/host/background differences, so they do not isolate the causal gain from this change.

That earlier RustFS reference reached 209.548 sequential overwrite cycles/s and 673.811 sequential read cycles/s. It remains historical evidence at source `7f4da840b815b24cc65ab37c93d7146a43f72775`, and is explicitly rejected as a current matched control by the verifier. No fresh backend speedup ratio is claimed.

## Amplification and resources

Reads remain three classified SQL submissions and three known SELECT rows per cycle; overwrites five SQL submissions, four SELECT rows, one COMMIT and one block PUT. These classified calls exclude separate BEGIN/control/PREPARE wire commands and do not measure scanned rows or TiKV operations. Active churn requires 44 submissions and 7,074.2 returned rows/cycle. Blob replacement does not remove this namespace cost.

Active worker Rust allocation plus reallocation requests/cycle were 890.190 for sequential reads, 914.367 for random reads, 3,056.463 for sequential overwrites and 3,030.508 for random overwrites. Observer/background work is included, foreign C allocators excluded; these quotients do not isolate metadata allocations. Largest server lifetime peak RSS was 78,233,600 bytes (74.61 MiB), controller 95,404,032 bytes (90.98 MiB); these are not a simultaneous fleet peak. Exact CPU, allocation, SQL, UDP and span observations are retained for all 16 cells. Active worker QUIC loss counters were zero; network saturation is unproven.

`host-block-observations.json` retains same-boot, stable-roster host driver endpoints over approximately 511.9 seconds, wider than the 476.454-second native run and including setup, oracles and background activity. One reported driver recorded 5,592,720 read operations / 24,867,209,216 bytes and 2,502,981 write operations / 20,315,942,912 bytes; its read/write error deltas were zero. Drivers are not aggregated. These host block-layer events cannot be attributed to the filesystem provider, TiDB/TiKV, individual workload cells or physical NVMe IOPS, and do not establish device saturation.

## Correctness and qualification

All 16 cells passed. Initial and final fresh content/size/EOF/membership checks each covered all 10,000 files. The run verified 100 cross-server pairs (409,600 bytes), ten Partition denials, ten sibling denials and ten revocation denials. All 89,918 attempted requests were acknowledged; failed and uncertain counts were zero. All ten workers retired cleanly. Final bytes were 62,869,504, following the acknowledged write ledger.

The external supervisor qualified the filesystem run: exit zero, unchanged pinned sources, reaped owner, absent process group, closed pipes and no abort/forced stop. The 64 GiB free-disk floor remained unchanged, sampled at 100 ms with 119.728 ms maximum observed gap; minimum observed free space was 69,474,414,592 bytes. The private backing root and data are retained.

Two fresh RustFS controls used the same executable but stopped at the disk floor during setup, before timed workload cells. Their elapsed times were 120.194 and 40.794 seconds, with minimum free space 68,542,513,152 and 67,585,380,352 bytes. Both owners were reaped without forced stop. Subsequent exact process/container checks found no owned native processes remaining and the owned fixtures stopped with data retained; this does not retroactively prove historical child reaping. Both attempts are excluded from performance results. Regenerable task compilation archives were reclaimed between attempts; neither retained datasets nor the original eight containers/six volumes were deleted. Further live testing awaits stable disk headroom or a disposable host.

Local source checks for the flush change passed 31 provider tests, 28 filesystem-filtered SDK/CLI checks, strict Clippy and formatting. The report verifier checked all 16 current filesystem cells and separately the 16 historical RustFS cells. Three negative controls rejected an allocation delta corruption, incomplete full content oracle, and use of the historical RustFS result as a current matched pair.

Linux runtime, real crash/power-loss recovery, mounted OS traffic, cross-host storage authority, full production scale and final CI/merge remain separate qualifications. At benchmark source `bf6062cb`, the refreshed CI rollup had 78 successful, 14 skipped, five cancelled and one failed check (macOS Node 9P shared-lock timeout). The source candidate has not been executed or presented as a verified fix.

## Deployment implications and next measurements

TiDB continues to own metadata. Every server accessing a Drive must share its authoritative block root and persisted backing marker. Independent local SSDs require shared storage or replication/recovery before replacing S3; a peer cache alone does not provide backing durability.

The next useful measurements are internal blocking-queue/hash/file/shard/root/device timing and a Linux filesystem run, followed by durable publication batching if those spans support it. Current data localizes substantial time in PUT and COMMIT without proving intrinsic TiDB contention or SSD saturation. Ten depth-one clients and short cells do not qualify the 10,000-client / 10,000-Drive / 5,000-Partition target.

## Artifacts

- `filesystem-observations.json.gz`: complete allowlisted current projection and frame/source/oracle bindings, with a deterministic gzip round trip.
- `summary.json`: all 16 current cells, exact rates, resources and explicitly historical RustFS throughput references.
- `qualification.json`: source/build/runtime/extraction/verification bindings and both excluded control stops.
- `verification.json`, `verify.py`, `arithmetic-verifier.py`, `extract-filesystem.py`: actual executed verification result and source. Raw-evidence controls require the hash-pinned private captures/input specification.
- `host-block-observations.json`: whole-host, per-driver endpoints and scoped deltas; no workload physical IOPS claim.

Credentials, raw fixture identities and absolute private paths are excluded from the public data. The earlier pair and its qualified artifacts remain unchanged.
