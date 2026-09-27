# SQLite causal diagnostic — 28 September 2026

The saturation runner now exports the current storage bank alongside the core
profile. Enabled stages retain before/after snapshots and checked deltas,
terminal outcomes, bytes, known returned rows, latency histograms and boundary
gauges. Disabled snapshots are unavailable, and pending or inconsistent
boundaries remain incomplete. These observations are sequential, not an atomic
snapshot of the server.

## Measured controls

Same fresh release executable, 100 established QUIC connections, ten coordinator
instances in one process, 100 separate Drives in one Partition, fixed 4 KiB
chunks and one 128 KiB file per active client. Each stage has a one-second warmup
and three-second timed workload. Empty Drives are provisioned before parallel
startup; files are created and populated through QUIC. Every acknowledged result
updates the expected ledger; every file is checked after fresh storage reopen,
including a separate stored namespace/blob oracle. Uncertain writes are never
replayed.

| Active clients | Recording | Random reads/sec | Random writes/sec | Mixed operations/sec |
|---:|---|---:|---:|---:|
| 100 | Enabled | 20,962 | 1,408 | 3,552 |
| 100 | Disabled at runtime | 21,517 | 1,800 | 3,267 |
| 1 of 100 | Enabled | 4,057 | 802 | 1,308 |

Each cell is one short observation. The disabled control uses the same
resource-profiling binary; it continues to collect process and Quinn boundary
counters. Different histories and host activity prevent an isolated overhead
percentage. The idle control populates only one file and does not repeatedly
probe inactive connection liveness. Audit logging remains enabled and its cost
is included.

## What the counters establish

- Pure reads and writes transfer exactly one blob payload per acknowledged
  4 KiB operation. Aligned overwrites read no old blob data and rewrite no
  extra chunks in this fixture.
- Reads execute **11 provider SQL statements per ACK**, including two selected
  inode loads. They return and decode **8,860 bytes of inode JSON per ACK**.
  The warm read stage has no observed provider pager misses or process disk
  bytes; its average process CPU use is **10.67 cores**.
- Writes execute **17 provider SQL statements per ACK**: twelve metadata
  statements and five block statements. They serialize **4,430 bytes of inode
  JSON**, including the complete 32-extent layout, for each 4 KiB overwrite.
- SQLite reports **7.14 page writes per write ACK**. Process disk accounting
  charges **176,327 bytes per ACK**, or **43.05× application payload**. These
  count different layers. Journal traffic, filesystem accounting and physical
  flash attribution require further measurements.
- The selected whole-disk driver reports about **23,733 writes/sec** during the
  write stage. This includes other host traffic and the controller's retained
  audit logs; it is not datastore-only or physical SSD IOPS.

SQLite performs synchronous mutex/statement/commit work inside the asynchronous
provider calls. In this run, SDK block put and inode publication take **5.466 ms**
and **5.211 ms** per ACK. Their sequential method wall time implies **15.04
occupied worker equivalents** and a 16-worker ceiling around **1,499 IOPS**,
close to the observed 1,408. This is a source-supported scheduling hypothesis,
not a commit-only timing or off-CPU trace. The two stages are sequential; other
nested/concurrent timing rows must not be added together.

The provider sets `synchronous=FULL` but does not set `journal_mode`. Actual
journal mode was not captured, so these results do not attribute amplification
to WAL or checkpoints.

## Next discriminating measurements

1. Capture actual journal/durability configuration and separate SQLite lock,
   statement and commit durations.
2. Compare 16, 32 and 64 runtime workers with the same queue depth and payload.
3. Measure database/journal/WAL write and sync operations by file role.
4. Compare 32, 128 and 256 extents under identical 4 KiB overwrites.

The blob path gives no reason to increase chunk size or add compression as the
first change in this fixture. Worker scheduling, persistence and full extent
metadata publication are the concrete leads.

## Verification and scope

The semantic export gate, **66 regular controls**, strict scoped Clippy and three
storage runs passed with frozen sources, full content checks and settled process
containment. The published [report](report.json) retains counters, source/binary
identity, accepted receipt hashes and excluded-launch evidence.

Three preliminary launches are excluded: an invalid private mode selector, a
concurrent fresh SQLite startup refusal, and the previous 4 MiB audit capture
limit. Each remains a failed receipt; none contributes throughput results. The
private diagnostic controller now retains at most 64 MiB per stream. The
repository's existing fault-gate limit remains unchanged.

This fixture uses synthetic authentication and a direct test wire. Production
client transport timings, the service stage observer, OIDC verification and
peer caching are explicitly unavailable. Client endpoint driver release before
fresh-store verification is unqualified; server shutdown awaits `wait_idle`,
and terminal process containment is proved.

This is not the ten-process, 10,000-client, 10,000-Drive, 5,000-Partition,
1,000-files-per-Drive qualification. Its unchanged 64 GiB disk reserve is unmet
on this host. Rust allocation tracing and physical SSD IOPS are also unqualified.
