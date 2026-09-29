# Inode extent scaling with a fixed hot working set

This diagnostic varies file layout size while keeping the accessed blocks fixed.
It helps distinguish whole-inode metadata work from blob payload amplification.
The production storage format and durability settings are unchanged.

## Measured results

Six release runs used SQLite WAL with FULL synchronization, 100 active QUIC
clients, 100 separate Drives, one Partition and ten coordinator replicas in one
process. Each Drive held one file. Clients performed aligned, pseudorandom
4 KiB overwrites at depth one in the first 32 blocks. File sizes were 32, 128
and 256 blocks; the order was **32 → 128 → 256 → 256 → 128 → 32**. Each run
had one second of warmup and 30 nominal measured seconds; rates include drain.
The runtime had 16 workers. Existing audit logging remained enabled and request,
service and storage tracing were disabled in every arm.

| Extents per file | Writes/sec, both runs | Inode bytes returned and decoded/write | Inode bytes serialized/write | Process CPU µs/write | End RSS MiB |
| --- | --- | --- | --- | --- | --- |
| 32 | 7,361 / 5,814 | 8,860 | 4,430 | 953 / 982 | 464 / 464 |
| 128 | 5,859 / 5,418 | 33,630 | 16,815 | 1,008 / 1,016 | 651 / 655 |
| 256 | 5,695 / 4,707 | 66,678 | 33,339 | 1,139 / 1,142 | 907 / 905 |

Returned bytes and decoded bytes observe the **same metadata at two boundaries**;
do not add those counters. Each overwrite loads/decodes two complete inode
documents and serializes one. SQL notifications remain exactly 17 per write.
The CPU cost per acknowledgment is consistent within each extent pair and rises
with layout size. Larger files also change resident state and SQLite page layout.

Successful SDK blob input stays **1× the 4 KiB payload**, with **zero old blob
reads** in all six arms. Confirmed native SQLite VFS writes stay **7.80–7.96×
payload**, with **2.018–2.020 sync calls/write** and two successful provider
commits/write. Client QUIC UDP traffic stays near 4,365 bytes/write.
Confirmed native VFS reads grow from roughly 13.6× payload at 32 extents to
22.2× at 128 and 34.2× at 256. This is file-callback read amplification;
cached file reads may not reach the backing device.

This points to two separate improvement seams:

- Publish bounded extent changes without returning, decoding and serializing
  the entire inode for each small overwrite. Preserve fresh guards, conflict
  semantics and exact reopen results.
- Reduce durable commit cost only with a proven publication contract. The
  existing two commits establish blob durability before publishing its inode
  reference; eliminating one requires an atomicity/crash-safety design.

These runs do not establish that a chunk-compression change addresses either
seam. They do not compare compression algorithms or unaligned writes.

Throughput varied materially with run order: the two 32-extent runs differ by
21%. Native sync wall time also varied. The balanced order provides diagnostic
evidence, not a pure metadata causal percentage or a steady-state capacity claim.
Inclusive provider/VFS timings overlap and cannot be summed as exclusive CPU.

## Fixture change and verification

The test-only `MOUNT_RS_REMOTE_SATURATION_HOT_BLOCKS` selector limits both warmup
and measured lane positions. Omitting it preserves the existing full-file range.
Strict parsing rejects nondecimal, non-Unicode, overflow, zero, out-of-file and
insufficient-lane settings before opening a provider. Population and both stored
and fresh-reader verification still cover every byte of the full file and EOF.
The expected cold suffix is checked after every warmup/measured stage.

All **11 owned gates** passed: 76 ordinary test cases (including four new pure
controls), one actual SQLite cold-tail corruption control, six measured load
cases, a release build, strict fixture Clippy and formatting. The corruption
control changes a real cold byte and requires both stored and fresh readers to
reject it. Every load run verified all 100 full files and persisted MRC5 backing
authority. All 2,000 provider connections reported WAL/FULL; terminal native
file/context gauges were zero. All gates retained unchanged source pins, reaped
children, absent process groups and complete, hash-matched logs.

The baseline Rust load case passed. A separate semantic check rejected the old
fixture because it lacked the requested hot-prefix control and artifact fields.
That baseline is not included in the six performance arms.

[report.json](report.json) retains exact observations, source/binary identities,
receipt/log/artifact hashes, SQL categories, native VFS rows, CPU/RSS/network
observations, counter scopes and qualification limits. Raw artifacts and the
bounded parent remain in the referenced private evidence directory. The report
binds the measured base commit plus the frozen dirty fixture hash; it does not
pretend the binary was built from a later publication commit.

## Reproduce the fixture arguments

Use a clean diagnostic environment and an absolute Cargo target isolated to the
checkout. The command below reproduces an arm's fixture arguments. The retained
runs additionally used a fixed owned parent with a 300-second deadline, source
freezes, log limits and process containment; plain Cargo supplies none of those
ownership guarantees. Repeat with block counts `32 128 256 256 128 32`, retaining
each output separately. These are small diagnostic runs, with a 10 GiB launch
floor, and do not alter the full-target 64 GiB floor.

```sh
CARGO_TARGET_DIR=/absolute/isolated-target \
MOUNT_RS_PROFILE_IO=1 \
MOUNT_RS_TRACE_REQUESTS=0 MOUNT_RS_TRACE_SERVICE=0 MOUNT_RS_TRACE_STORAGE=0 \
MOUNT_RS_REMOTE_SATURATION_PROVIDER=sqlite \
MOUNT_RS_REMOTE_SATURATION_CLIENTS=100 MOUNT_RS_REMOTE_SATURATION_ACTIVE_CLIENTS=100 \
MOUNT_RS_REMOTE_SATURATION_SERVERS=10 MOUNT_RS_REMOTE_SATURATION_SEPARATE_DRIVES=1 \
MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES=1 MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES=0 \
MOUNT_RS_REMOTE_SATURATION_PRESEED=0 MOUNT_RS_REMOTE_SATURATION_PROVISION_DRIVES=1 \
MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY=1 MOUNT_RS_REMOTE_SATURATION_RUNTIME_WORKERS=16 \
MOUNT_RS_REMOTE_SATURATION_SQLITE_JOURNAL=WAL MOUNT_RS_REMOTE_SATURATION_HOT_BLOCKS=32 \
MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS=32 MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=1 \
MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=write MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=0 \
MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS=1 MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=30 \
MOUNT_RS_REMOTE_TIDB_SATURATION_REQUEST_TIMEOUT_SECONDS=10 \
MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT=/absolute/private-output/extent32a.json \
./scripts/cargo-shared test --release --locked --offline -p mount-rs-service \
  --features resource-profiling --test quic_tidb_saturation -- \
  --ignored --exact actual_tidb_100_clients_10_servers_saturation --test-threads=1 --nocapture
```

The exact corruption control is
`hot_working_set_oracles_reject_corrupted_cold_tail` in the same test target;
it requires the `PROVIDER=sqlite` and `COMPACT_INODE_UPDATES=1` settings above.

## Limits and the next scale seam

This is a local macOS diagnostic with test wire helpers and synthetic
authorization, bypassing the production remote provider and native mounts.
It covers aligned writes at depth one, not reads, mixed/churn patterns, ten real
server processes, 10,000 clients or ten million files. Allocation churn was not
measured. Larger initial databases, faster accumulation of immutable blob history,
warmup/checkpoint state and host variation remain confounders.

VFS counts describe native SQLite callbacks, not syscalls or physical SSD IOPS.
Metadata and blobs share physical SQLite files; VFS roles cannot attribute every
write exclusively to one API. Process OS accounting is separate; no host device
was selected. End SQL/VFS counters are totals since a drained reset, not an
end-minus-begin delta. Terminal counters also include shutdown/drop before fresh
verification. These results establish no crash/power-loss, native-mount, hosted
CI or full production-capacity qualification.

Source review also found that both the process fixture and CLI eagerly open all
catalog Drives. After populated reopen, the full target retains ten million
file-layout nodes per process and 100 million fleetwide. SQLite would retain
20,000 provider connections per process; TiDB shares a bounded endpoint pool.
The next scale seam is bounded lazy runtime activation after catalog
authorization, preserving any-server routing and protecting live handles and
uncertain publications from eviction. Static Drive sharding would violate the
existing all-server/all-Drive route check. These are source-derived counts, not
a measured RSS-cap breach; the 24 GiB cap is per process.
