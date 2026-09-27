# SQLite journal comparison — 2026-09-28

This compares **DELETE and WAL with FULL durability** on disposable local SQLite
Drives. Production defaults are unchanged. The [report](report.json) retains
measurements, source/binary bindings, receipts and excluded preliminary runs.

## Workload and result

Six fresh runs used the same release binary, 16 Tokio workers, 100 continuously
active QUIC clients and 10 coordinators in one process. There were 100 separate
Drives in one Partition, each with one 128 KiB file and random aligned 4 KiB I/O
at depth one. Metadata and blobs share each Drive's database file: **100 database
files, 2,000 provider connections**. Authentication was synthetic; the peer blob
cache was absent. Request audit logging stayed enabled.

Order was DELETE, WAL, WAL, DELETE, DELETE, WAL. Each read, write and mixed phase
had a one-second warmup and a three-second measured interval including drain.
These are descriptive results from three repetitions per mode.

| Application operations/sec | DELETE median (range) | WAL median (range) |
| --- | ---: | ---: |
| Read | 21,658 (20,603–21,840) | 21,365 (21,160–21,525) |
| Write | 1,641 (1,271–1,672) | 10,459 (8,857–10,615) |
| Mixed | 3,555 (2,663–3,557) | 14,775 (9,111–14,783) |

WAL's median write rate was **6.37 times** DELETE's. Median inclusive provider
commit time per acknowledged write fell from **5.377 ms to 0.435 ms**, across
two commit calls. Process CPU per write fell from 3.718 ms to 0.915 ms. The
connection mutex wait remained below 1.1 microseconds per write in both arms.
These locate substantial journal/commit cost in this workload; inclusive timers
overlap and cannot be summed as exclusive CPU time.

Both arms still issued **17 SQL statement notifications and two provider commit
calls per write**, with 1x successful SDK block input bytes. Pure reads issued
11 statement notifications and returned 1x SDK payload bytes. Chunking and the
metadata format were identical between arms.

Median process-accounted write bytes during the pure-write phase fell from
**43.00x to 15.89x** acknowledged payload bytes. This is OS process accounting,
not physical SSD write amplification. Selected host-driver counters include
parent log output, other processes and kernel work.

## Deferred work and tradeoffs

All actual connections retained `synchronous=2`, normal locking, a 5,000 ms busy
timeout, 1,000-page automatic checkpoint setting, 4 KiB pages and the same cache
size. `fullfsync` and `checkpoint_fullfsync` were both zero. FULL WAL syncs the
log at commit, and checkpoint work remains necessary; SQLite also checkpoints
on the final connection close. See [SQLite WAL documentation](https://sqlite.org/wal.html).
This experiment does not qualify power-loss recovery.

After the last measured phase, server close and filesystem shutdown/drop were
measured separately, before fresh verification opened any provider. WAL runs
recorded **73.15–89.62 MB** of process write accounting during **0.446–0.823 s**
cleanup intervals; DELETE recorded 0.41–0.57 MB during 0.414–0.530 s. These totals
include background and observer work following setup, warmups and all phases.
They cannot be attributed exactly to checkpointing or to timed write ACKs.

The 100 WAL files retained about 413 MB of logical size at write/mixed endpoints,
then all WAL/SHM sidecars disappeared after provider drops. Database size grew
during close. These are sequential logical/allocation gauges, not physical
writes or complete disk footprint: transient rollback journals are omitted.
Actual checkpoint counts and durations were not instrumented. The third WAL
mixed run had materially higher provider-commit time than SQL PROFILE time;
post-PROFILE work inside commit is a follow-up target, without attributing that
gap solely to checkpointing.

The experiment establishes a useful journaling option for this local workload.
Longer runs, checkpoint behavior and production configuration require further
qualification. WAL requires local shared-memory coordination and serializes
writers within each database; [SQLite's WAL constraints](https://sqlite.org/wal.html)
apply when choosing deployment topology.

## Qualification and reproduction

The fixture first provisions each Drive through the SDK, drops its providers,
then converts the existing owned file using fixed DELETE/WAL PRAGMAs. It rejects
missing, unowned, unprovisioned, symlinked and hard-linked backings. Actual mode
and FULL settings are verified after conversion, on all 2,000 reopened provider
connections, and with identical connection IDs at every measured endpoint.

The real conversion control tests both modes across two SDK replicas, full
payloads, MRC5 backing identity, stored bytes, fresh reads and final resource
closure. All six load runs verified all 100 files through stored and fresh
provider oracles: **635,632 measured acknowledgments, zero stage errors**.
Final validation passed 72 regular tests, the exclusive conversion control,
both resource/default-feature Clippy scopes, formatting and release build.
Twelve owned gates passed with matching 505-file repository source maps and
the same release executable across all measurements. Independent parsing also
matched raw logged stages and duplicate artifacts byte for byte.

Source was measured with three owned Rust changes over `44871e37`, rather than
as a clean execution of the publication commit. Report source hashes must match
the final committed blobs. Release binary SHA256:
`57277d95784a7ffed2b7a68eb07b30cc6f620b90a13a2a7e830f6a1221932640`.

Use `scripts/cargo-shared` with a checkout-specific target cache. The existing
ignored `actual_tidb_100_clients_10_servers_saturation` test accepts these
selectors for the exact SQLite fixture:

```text
MOUNT_RS_PROFILE_IO=1
MOUNT_RS_TRACE_STORAGE=0
MOUNT_RS_REMOTE_SATURATION_PROVIDER=sqlite
MOUNT_RS_REMOTE_SATURATION_RUNTIME_WORKERS=16
MOUNT_RS_REMOTE_SATURATION_SQLITE_JOURNAL=DELETE or WAL
MOUNT_RS_REMOTE_SATURATION_CLIENTS=100
MOUNT_RS_REMOTE_SATURATION_ACTIVE_CLIENTS=100
MOUNT_RS_REMOTE_SATURATION_SERVERS=10
MOUNT_RS_REMOTE_SATURATION_SEPARATE_DRIVES=1
MOUNT_RS_REMOTE_SATURATION_PROVISION_DRIVES=1
MOUNT_RS_REMOTE_SATURATION_PRESEED=0
MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES=0
MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES=1
MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY=1
MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS=32
MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=1
MOUNT_RS_REMOTE_TIDB_SATURATION_MODES=read,write
MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED=1
MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS=1
MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=3
MOUNT_RS_REMOTE_TIDB_SATURATION_REQUEST_TIMEOUT_SECONDS=10
```

Select an actual host device separately for host counters; an unset selection
remains unavailable. Clear inherited remote/datastore observer selectors, use
unique output paths and preserve bounded process containment and full logs.

```sh
./scripts/cargo-shared test --release --locked --offline -p mount-rs-service \
  --features resource-profiling --test quic_tidb_saturation -- \
  --ignored --exact actual_tidb_100_clients_10_servers_saturation \
  --test-threads=1 --nocapture
```

A preliminary regular-test run exposed an existing sampler-control race:
unrelated SQLite frees made a shared-baseline-plus-8-MiB threshold unreachable.
The control now observes a known live allocation with one two-second deadline
and RAII cleanup. The contained timeout, semantic journal RED and preliminary
successful control are retained and excluded from final qualification. The
timeout's postterminal KILL EPERM remains explicitly unresolved, although its
child was reaped, group absent, pipes at EOF and fixtures removed.

These runs do not qualify 10 independent server hosts, 10,000 clients, 5,000
Partitions or 10 million files. Retained JSON observer banks contribute to RSS;
allocation churn and off-CPU attribution were not measured in this binary.
