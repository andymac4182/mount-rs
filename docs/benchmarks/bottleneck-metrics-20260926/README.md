# Measured storage performance, 2026-09-26

The opt-in [bottleneck metrics](../../bottleneck-metrics.md) produced these
observations at `6f31fc748032404548b968686402dec6c2b8cf00`. The
[exact summary](profile-summary.json) retains artifact digests, binary hashes,
source provenance, operation counts and measurement scopes.

## Application throughput

Each legacy public-NAPI run completed 400 write/read/delete cycles with full
read-byte verification, identical 4 KiB payloads and concurrency 64. Rates are
1,200 successful application operations divided by lifecycle wall time.
Lifecycle time includes scheduling and verification; per-read latency ends
before byte comparison. Setup and resource shutdown are separate.

| Metadata backend | PR operations/sec | Push operations/sec | 1,000 floor |
| --- | ---: | ---: | --- |
| SQLite | 1,441.73 | 1,017.58 | Both passed |
| PGlite | 1,151.03 | 673.07 | PR passed; push failed |
| TiDB | 396.83 | 243.48 | Both failed |
| FoundationDB | 637.05 | 461.89 | Both failed |

All eight passed the byte, operation, timeout and cleanup checks; failed rates
failed only the unchanged throughput floor. The
[PR run](https://github.com/andymac4182/mount-rs/actions/runs/36242093357) and
[push run](https://github.com/andymac4182/mount-rs/actions/runs/36242090386)
used distinct execution windows and fixture processes. The substantial variation
with the same respective native binaries prevents a stable backend ranking.
These results do not establish whole-CI success or production capacity.

## Measured publication costs

These are the PR workload windows. Publication calls count invoked publication
attempts, rather than separately observed durable commits. Serialization bytes
count serializer output. Timings are inclusive and can overlap with other spans.

| Backend | Publication calls | Serialized namespace bytes | Inclusive NAPI publication time | Node workload CPU |
| --- | ---: | ---: | ---: | ---: |
| SQLite | 642 | 15,198,339 | 621.14 ms | 326.34 ms |
| PGlite | 707 | 15,393,716 | 648.20 ms | 251.69 ms |
| TiDB | 634 | 14,734,711 | 2,543.88 ms | 423.66 ms |
| FoundationDB | 672 | 15,616,052 | 1,639.08 ms | 559.56 ms |

TiDB recorded 634 conditional autocommit namespace UPDATE calls: 2,476.37 ms
of inclusive SQL await/result-drain time and 1.37 ms of pool checkout time.
Serialization occurs before the SQL timer. Explicit transaction and SELECT-row
counters were zero in this window; they do not prove absence of internal server
transactions or reads. UPDATE affected rows and SQL wire bytes are unavailable.

SQLite recorded 642 UPDATE statements, 23,647 pager hits, zero pager misses
and 5,974 page writes. Its 4,096-byte pages imply a 24,469,504-byte pager estimate.
That estimate is separate from physical disk writes. FoundationDB driver phases
and PGlite execution internals remain unmeasured at these boundaries.

## Cache and provenance limits

All eight workloads recorded 400 R2/object-store adapter in-process cache hits
and zero ordinary raw GET/body calls. Each confirmed only one unique 4 KiB
block creation. Other conditional creates encountered existing content and
verified its body. This exercises cached reads and identical-content writes;
it does not qualify cold reads, unique-data saturation, distributed peer-cache
savings or HTTP retry counts.

The verified PR merge revision has the same committed tree as `6f31fc74`.
SQLite/PGlite PR artifacts report a clean checkout; TiDB reports one omitted
dirty entry. Their push artifacts identify `6f31fc74`, with SQLite/PGlite clean
and TiDB again reporting an omitted dirty entry. FoundationDB artifact source
revision remains unverified, although each job head and native binary hash are
retained. Source revision and binary identity are separate evidence.

Node CPU excludes separate database and blob-service processes. Existing TiDB container
CPU points were taken during failure cleanup without workload-aligned intervals.
Backing CPU, physical device IOPS and actual HTTP attempts remain unavailable.
Zero SDK rows are expected because this direct NAPI path bypasses SDK erased
adapters. It also bypasses the remote authorization catalog.

## Next comparison

Measure legacy and the existing public compact layout on the same qualified
fixture, retaining both results and the legacy floor. Compact changes concurrency
and publication semantics, so validate its persisted MRC5 marker separately.
Add workload-aligned backing-process observations and adjacent idle controls.
Cold unique-data traffic and the full production population need separate runs.
The current measurements identify concrete seams to investigate; they do not
yet justify a chunking or compression redesign.

## Remote catalog improvement

A separate [same-binary SQLite control](catalog-conditional-read.json) uses an
exact 10,000-Drive/5,000-Partition/10,000-grant catalog, 2,271,257 bytes long.
All eight pooled handles are warmed and observed before each 40-read window.
The full-row selector shares the production decoder and omits certificate
probes. Three serial pairs produced these ranges:

| Measurement | Full row | Conditional read |
| --- | ---: | ---: |
| Full-BLOB selects, profiling enabled | 40 | 0 |
| Logical returned document bytes | 90,850,280 | 0 |
| Wall time, profiling enabled | 19.5–21.9 ms | 3.1–3.4 ms |
| Wall time, profiling disabled | 20.4–22.1 ms | 1.5–1.7 ms |
| Rust allocation calls, disabled, loader thread only | 40 | 0 |
| Pager misses, profiling enabled | 22,210 | 5 |

Profiling-enabled pager collection itself allocates: 480 full-row versus 440
conditional Rust allocations per window. Disabled query/token/pager counters
are unavailable; their zero fields are not evidence of zero calls. SQLite C
allocations, asynchronous scheduling and end-to-end RPC throughput are outside
the allocation control. This does not establish globally allocation-free
metadata or device IOPS.

The local tests passed independent review: same-revision edits, invalid catalog
repairs, commit boundaries, CAS cancellation/rollback/lost result and generation
exhaustion. Signed QUIC tests revoke a grant on an already-open handle, observe
all eight real pool slots and confirm denial without further backend reads.
The deterministic cancellation test observes seven native reader handles blocked
while the writer owns the eighth, with all eight readers unfinished before release.

Certificates retain up to eight previous catalog snapshots until those handles
are revisited. This memory cost and the
[authority protocol](../../superpowers/plans/catalog-conditional-read.md) are
part of the trade-off. The control was run in the local combined dirty checkout;
clean committed CI is separate. Its catalog population is a definition shape,
not 10,000 active clients. The direct NAPI legacy rates above bypass this catalog.

Reproduce the paired control from the repository root:

```sh
MOUNT_RS_PROFILE_IO=1 scripts/cargo-shared test --locked \
  -p mount-rs-service --lib --features io-profiling \
  target_shape_paired_full_row_and_conditional_resource_windows \
  -- --ignored --nocapture
```

Run again with `MOUNT_RS_PROFILE_IO` unset for the disabled control.
