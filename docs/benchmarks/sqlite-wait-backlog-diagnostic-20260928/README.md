# SQLite wait and WAL backlog diagnostic — 2026-09-28

[Machine-readable evidence](report.json) retains all four fresh runs, provider
timings, OS accounting, file gauges and separate terminal receipts. The new
metrics preserve existing journal, busy/retry and durability policies.

## Scope

100 active QUIC clients, 10 coordinators **in one process**, 100 separate Drives,
one Partition, one 128 KiB file per Drive, random aligned 4 KiB writes, depth 1,
16 Tokio workers. There are 100 SQLite database files and 2,000 provider
connections: metadata/block roles across 10 replicas. WAL has additional
WAL/SHM sidecars. This fixture uses test wire helpers, synthetic authentication
and full request audit logging; it bypasses the production remote provider and
OS mount. There is no peer blob cache.

Each run has one second of warmup and a nominal 30-second write interval; rates
include drain. Order is DELETE → WAL → WAL → DELETE. These are descriptive
interval averages from two repetitions per mode, without per-second throughput
buckets or a statistical/production saturation claim.

## Results

| Run | Write IOPS | Commit ms / ACK | Mutex hold ms / ACK | Process CPU µs / ACK | Process-accounted write bytes / payload |
| --- | ---: | ---: | ---: | ---: | ---: |
| DELETE 1 | 1,085.25 | 9.805 | 14.347 | 3,689 | 44.16× |
| WAL 1 | 7,464.16 | 1.082 | 1.841 | 926 | 16.46× |
| WAL 2 | 7,362.90 | 1.057 | 1.899 | 934 | 16.49× |
| DELETE 2 | 1,210.43 | 8.659 | 12.829 | 3,814 | 44.24× |

Median write throughput is **1,147.84 IOPS DELETE / 7,413.53 IOPS WAL**, a
descriptive 6.46× ratio. Successful SDK block input is 1× payload in both modes;
each acknowledged write has 17 SQL starts and two observed BEGIN/commit calls.
The application payload, pager, process accounting and whole-host device layers
have different units. Process bytes are not physical flash amplification.

Mutex acquisition totals are 0.53–1.13 µs per ACK. Observed BEGIN call totals are
76–91 µs per ACK, including both transactions. The larger measured intervals
are inside the acquired connection and commit calls. These inclusive wall
timers overlap; do not add them or identify them as exclusive CPU/fsync time.

Connection holds occupy 15.53–15.57 aggregate wall-worker equivalents in DELETE
and 13.75–13.98 in WAL, against 16 configured runtime workers. Process CPU is
4.00–4.62 cores DELETE and 6.88–6.91 WAL. The provider performs synchronous
SQLite work in async driver methods. Together these observations support
investigating runtime threads blocked inside storage calls; they do not prove
that one queue or syscall accounts for all remaining time. An off-CPU trace and
VFS write/sync attribution are still needed.

The coarse latency histogram's p99 upper bounds are 0.52–1.05 seconds DELETE
and 0.262 seconds WAL. They are bucket bounds, not exact percentiles. End RSS is
479–484 MB and includes retained JSON observer banks; Rust allocation profiling
was not enabled in this binary.

## Observer and checkpoint limits

Registry collection takes 39–45 ms per endpoint, outside the stage's workload
clock. It observes every provider connection sequentially. The WAL NOOP probe
does no backfill but can read/initialize/recover WAL state and invalidate cached
headers; it is not a stat-only observation or an overhead-free control.

WAL endpoint uncheckpointed-frame ranges are 0–998 and 2–995 across the 2,000
connection observations. These gauges duplicate the same 100 physical databases
and must not be summed or differenced across WAL resets. Actual checkpoint
counts, copy duration and sync calls remain uninstrumented. Terminal close/drop
is accounted separately and includes background/observer work, not solely
checkpointing or work attributable to measured ACKs.

**One SQL PROFILE duration is unavailable:** DELETE 1, connection 902, ROLLBACK,
one invalid notification among 332 completions. The raw callback counters and
histogram are retained. That category's derived duration and full SQL PROFILE
timing qualification are unavailable for this run. Throughput/payload checks
and the monotonic BEGIN, hold and commit timers remain independently valid.
The JavaScript projector now rejects complete timing claims when any invalid
duration is present and retains sanitized endpoint observations.

## Verification and reproduction

All **517,333 acknowledged writes** have zero stage errors. Each run verified
all 100 stored files and full payloads through fresh drivers, persisted MRC5
backing identity, all 2,000 actual configurations and stable positive connection
IDs. Parent processes exited, reaped their owned children, observed absent
groups and EOF, and removed owned fixtures without unknown lifecycle state or
log overflow. The partial SQL PROFILE measurement above is not presented as
a fully qualified timing bank.

The real contention control first failed on the missing BEGIN metric, then
passed with failed-BEGIN noncommit, a known held interval, unchanged positive
WAL backlog under NOOP observation, reset behavior, fresh payloads and empty
final registry. Focused JavaScript RED controls exposed both omitted fields and
invalid duration completeness. Normal SQLite, service-fixture and N-API unit
suites, four exclusive diagnostic controls, real journal conversion, full
JavaScript benchmark unit tests, touched SQLite/service Clippy and formatting
passed. The JavaScript unit suite uses the capture fixture, not a live addon.

The runs used owned Rust changes over `1c04cc7d`, not clean executions of their
publication commit. All 505 repository source pins must match final committed
blobs. Source closure SHA256:
`8975c6ae8996aa57b00c0eb137e0345626fd9937b2469283f25c34b2e12ac47c`.
Release executable SHA256:
`d65c39878d653b9371e7feec81c265869c189d5e0856b6fc6f3caf4caee97639`.

Use the selectors and command in the
[short journal experiment](../sqlite-journal-diagnostic-20260928/README.md),
changing only these workload selectors:

```text
MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=write
MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=0
MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=30
```

Use fresh owned fixtures, a selected host device, complete retained logs and
bounded process containment. The private diagnostic parent checks 10 GiB free
before this small fixture and caps each log at 256 MiB. This does not lower the
64 GiB reserve required by the separate full production qualification runner.
No 10-host, 10,000-client, 5,000-Partition, 10-million-file, native-mount or
power-loss qualification is claimed.
