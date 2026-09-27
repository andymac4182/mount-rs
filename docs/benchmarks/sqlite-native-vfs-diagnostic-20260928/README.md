# SQLite native file I/O diagnostic — 2026-09-28

[Machine-readable evidence](report.json) retains all four planned fresh runs,
native file-role counters, checkpoint signals, provider timings, OS accounting
and terminal banks. The instrumentation preserves journal, locking, busy/retry
and acknowledgment policies.

## Workload

100 active QUIC clients, 10 coordinators **in one process**, 100 separate Drives,
one Partition, one 128 KiB file per Drive, random aligned 4 KiB writes, depth 1,
16 Tokio workers. Each of the 100 database files has metadata/block connections
across ten replicas: 2,000 live provider connections. The fixture uses test wire
helpers, synthetic authentication and full request audit logging, bypassing the
production remote provider and OS mount. It has no peer blob cache.

Each fresh run has one second of warmup and a nominal 30-second write interval;
rates include drain. Planned order: DELETE → WAL → WAL → DELETE. Two runs per
mode provide descriptive interval averages; there are no per-second throughput
buckets or statistical steady-saturation estimates.

## Results

| Run | Write ACK/s | Confirmed VFS write bytes / payload | xSync calls / ACK | Aggregate xSync wall ms / ACK | Provider commit ms / ACK |
| --- | ---: | ---: | ---: | ---: | ---: |
| DELETE 1 | 1,093.20 | 13.93× | 6.000 | 5.571 | 9.530 |
| WAL 1 | 7,136.24 | 7.92× | 2.020 | 0.972 | 1.175 |
| WAL 2 | 8,336.94 | 7.97× | 2.020 | 0.538 | 0.741 |
| DELETE 2 | 1,296.30 | 13.95× | 6.000 | 4.758 | 7.994 |

Median throughput is **1,194.75 ACK/s DELETE / 7,736.59 ACK/s WAL**, a descriptive
6.48× ratio. Keep the repeat variation: WAL 2 is about 17% faster than WAL 1.
Successful SDK block input is exactly 1× application payload in every run;
each ACK has 17 SQL starts and two observed provider BEGIN/commit calls.

DELETE has four journal syncs and two database syncs per ACK. Journal writes
alone occupy 2.86–3.52 aggregate ms per ACK. WAL has approximately two WAL syncs
per ACK, plus infrequent database/WAL syncs associated with checkpointing.
Native read/write/sync clocks and provider commit/hold clocks are inclusive;
they overlap and cannot be added as exclusive CPU or wait time. These results
support prioritizing durable commit frequency, native journal writes and sync
behavior for further tracing. They do not identify a particular syscall or
establish the effect of a future batching or storage-format change.

| Layer | DELETE | WAL | Unit |
| --- | ---: | ---: | --- |
| Successful SDK block input | 1.00× | 1.00× | Known submitted successful payload bytes / 4 KiB ACK payload |
| Confirmed VFS writes | 13.93–13.95× | 7.92–7.97× | Native xWrite requested bytes returning SQLITE_OK / payload |
| Process-accounted writes | 44.16–44.29× | 16.41–16.56× | macOS accounting for this observer PID / payload |

These layers have different units and exclusions. Their difference is not an
exact residual or physical flash amplification. VFS reads also show roughly
13.6–14.1× payload in successfully returned native bytes, covering all observed
SQLite reads. Mapped reads, shared-memory operations and other file/VFS methods
are excluded; this is not the application's logical read amplification.

DELETE records two journal `SQLITE_IOERR_SHORT_READ` returns per ACK, averaging
eight requested bytes per invocation. Application writes and full payload verification all
succeeded. The pattern is consistent with SQLite's tolerated end-of-journal
header probes; aggregate role counters do not establish every call's site or
offset. Error-call partial bytes remain unknown. Zero confirmed bytes for those
calls does not mean zero bytes were actually read.

## Checkpoints and close activity

WAL 1/2 record **1,412 / 1,687 matched checkpoint copy windows**. Aggregate copy
wall time is 21.61 / 13.73 µs per ACK. The separate database sync row contributes
268.57 / 81.46 µs per ACK. The START/DONE copy window excludes initial WAL sync,
iterator/lock work and final database truncate/sync; it does not measure total
checkpoint time, copied frames or successful checkpoints. No unmatched,
aborted or pending windows occurred in these four runs.

Each terminal global bank survives an empty provider registry, with one
permanent wrapper registration and zero live files/provider markers. Terminal
counts are cumulative since the last stage reset through server/filesystem
shutdown and drop, before stored/fresh verification. They are not an isolated
close delta or work attributable solely to measured ACKs. The dedicated native
control separately proves that final-close backfill stays observable.

## Partial SQL evidence and observer costs

DELETE 2 has one invalid approximate SQL PROFILE BEGIN duration: connection
904, 399 completions and 398 histogram observations. Across the process BEGIN
category there are 120,114 completions. That category's derived duration and
full SQL PROFILE qualification are unavailable; raw counters are retained.
Monotonic provider BEGIN/commit/hold and every VFS timing remain independently
valid. The JavaScript consumer preserves the valid global VFS bank when SQL
validation fails, while leaving the overall phase incomplete.

VFS callback instrumentation uses fixed atomics, clocks and thread-local
observer suppression, with no per-call Rust heap allocation or logging by
source inspection. Startup registration allocates once and enlarges SQLite's
native file structure. Shared-counter contention, native C allocations and
snapshot serialization still have costs. Allocation profiling and a matched
observer-disabled overhead comparison were **not measured** in these runs.
End RSS is 481–487 MB and includes retained observer JSON banks.

## Verification and reproduction

All **540,057 acknowledged writes** have zero application stage errors. Every
run verifies all 100 stored files and full bytes through fresh drivers,
persisted MRC5 backing identity, all 2,000 actual configurations and stable
positive connection IDs. Owned parents exited, reaped children, observed absent
groups and EOF, and removed owned fixtures without unknown lifecycle state or
log overflow. No planned control was excluded.

Native failure controls cover failed-but-closeable opens, unchanged errors and
arguments, short-read buffers, write/sync errors, observer suppression, native
method-table changes, checkpoint mismatches and reset rejection. A real
provider control covers DELETE/WAL roles, manual and final-close checkpoint
work, URI overrides, safe external connections outliving provider markers and
full payloads after reopen. Ordinary SQLite tests, strict Clippy, formatting,
prior exclusive diagnostic controls and JavaScript consumer gates are retained
separately; capture-fixture JavaScript checks do not establish a live addon.

Runs execute owned Rust changes over `ed9a8ce0`, rather than the eventual clean
publication commit. All **506 repository source pins** match the release build
and every run, and must match the final committed blobs. Source closure SHA256:
`2616253cb32d29aa1d5c9f040a6ef88ae762bff9cc8e873df21a1169658c387e`.
Release executable SHA256:
`6cc3ed7ef9bec5d2afe1d4382b2f1a79c49fb8521b30df78ced12ba15e09bfa3`.

Use the selectors and command in the
[short journal experiment](../sqlite-journal-diagnostic-20260928/README.md),
with `MOUNT_RS_PROFILE_IO=1` set before opening providers and these selectors:

```text
MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=write
MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=0
MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=30
```

Use fresh owned fixtures, complete retained logs and bounded process
containment. The small diagnostic checks 10 GiB free before launch; the full
production runner keeps its separate 64 GiB reserve. This report does not
qualify ten server processes/hosts, 10,000 clients/Drives, 5,000 Partitions,
ten million files, native mounting, physical SSD IOPS, power-loss behavior,
off-CPU stacks or formal correctness of the new VFS wrapper.
