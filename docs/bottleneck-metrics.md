# Finding remote-drive bottlenecks

Enable `MOUNT_RS_PROFILE_IO=1` before constructing stores. The service benchmark
also needs its `io-profiling` feature; the production-target runner uses
`resource-profiling`, which includes it. Profiling is opt-in. Keep a disabled
control when measuring the cost of the observers.

## What the measurements mean

| Boundary | Observations | Interpretation |
| --- | --- | --- |
| Application workload | Acknowledged operations, errors, uncertain writes, active/idle time | The denominator for amplification and application operations/sec |
| Filesystem and catalog | Existing fixed core events, queue/gate time, query/decode document bytes, SQLite pager observations | Inclusive wall time and logical document traffic; pager activity is separate from device I/O |
| SDK adapters | All metadata and block methods, outcomes, cancellations, in-flight work, known block bytes | Calls through the erased adapters; metadata bytes remain unavailable |
| TiDB adapter | Pool checkout, session setup, schema/open, transaction begin/commit/explicit rollback, SQL families and known returned rows | Checkout includes lazy connection/setup work. SQL calls are distinct from MySQL commands, TiKV requests and device I/O |
| Object-store block adapter | Actual get/put/head/delete invocations and body reads, reason, outcomes, claim leader/follower counters | Adapter API calls and known body bytes; internal HTTP retries and reconciliation listing remain unavailable |
| QUIC server | TLS/application handshake, authentication, admission, request/read/dispatch/encode/submit/cleanup, latency buckets and gauges | Inclusive application spans; submission to Quinn does not establish peer acknowledgment |
| Accepted QUIC connections | UDP bytes/datagrams/I/O calls, frame counters, path observations, retained retired-connection totals | Accepted connection lifetime through session retirement; excludes refused/failed TLS connections and subsequent transport traffic |
| Process | CPU, RSS, optional Rust allocation churn and process disk accounting | Includes observer/background work; process disk bytes are not physical operation counts |
| Selected host device | Optional block/driver operation and byte counters | Shared device activity, including other processes; requires stable device identity and is not NAND I/O |

Fixed storage rows have terminal outcomes, in-flight gauges, inclusive elapsed
time and 32 logarithmic latency buckets. Returned-row observations distinguish
known zero rows from an unavailable row count. Nonzero values in nested families
must not be added as unique application operations or exclusive CPU time.

Native storage diagnostics use `mount-rs.storage-diagnostics.v3`, with exact
decimal strings for counters. The closed row registry and the instrumented-row
coverage are separate: reserving a row does not prove that a provider records it.
The object-store API sub-schema remains `mount-rs.object-store-api.v1`.

Rust allocation counters require the additional `allocation-profiling` test
feature. They count the Rust System allocator, exclude foreign C allocators,
and add atomic work to allocation paths. Resource-only builds report them as
unavailable. RSS and live allocation bytes are endpoint/lifetime gauges, rather
than counters to subtract as allocation churn.

## Collect a phase profile

The production-target runner writes controller and worker boundary receipts,
checked phase deltas and observer costs alongside workload results. It verifies
PID, replica generation, source/binary and storage identity before joining the
ten worker observations. Missing, reset, saturated, timed-out or nonquiescent
observations remain incomplete. Unconfigured cache or raw blob observers are
reported as unavailable, rather than zero traffic.

For a small instrumentation control on macOS or GNU Linux, choose a **new**
retained output directory:

```sh
env -u MOUNT_RS_TRACE_REQUESTS -u MOUNT_RS_TRACE_STORAGE \
  -u MOUNT_RS_TRACE_SERVICE -u MOUNT_RS_TARGET_INJECT \
  -u MOUNT_RS_PROFILE_BLOCK_DEVICE -u MOUNT_RS_PROFILE_IOREGISTRY_ENTRY_ID \
MOUNT_RS_PROFILE_IO=1 \
MOUNT_RS_TARGET_MODE=control \
MOUNT_RS_TARGET_PROVIDER=sqlite \
MOUNT_RS_TARGET_DRIVES=10 \
MOUNT_RS_TARGET_FILES=2 \
MOUNT_RS_TARGET_SECONDS=1 \
MOUNT_RS_TARGET_OUTPUT=/absolute/new/output-directory \
scripts/bench-remote-production-target.sh
```

This exercises ten independent servers and both idle and active client patterns.
It verifies collection and fresh content; its dimensions do not qualify the
10,000-client production target. Existing resource floors, deadlines and cleanup
requirements still apply. Use the full mode only with the required capacity.

## Follow startup while it is running

An `io-profiling` build with `MOUNT_RS_PROFILE_IO=1` also emits bounded
`startup_diagnostics ` records using `mount-rs.startup.v1`. The fixed stages are
configuration, catalog open/load/validation, TLS material, cache start, drive
configuration/open, backing receipt, drive registration, listener bind,
readiness and cleanup. Records contain stage attempts, terminal outcomes,
in-flight counts, elapsed time, planned drives, completed opens and registered
drives. They contain no drive names, file paths, tokens, issuer URLs or payloads.
Rows form a fixed registry; unused rows do not establish producer coverage. In
the CLI, catalog validation is included in catalog-load time, the backing-receipt
row is unused, optional cache setup includes the no-cache case, and listener-bind
time includes signal-handler preparation.

The observer records stage changes and publishes at startup, readiness, failure
and cleanup boundaries and every five seconds while an observed startup
operation is stalled. The production-target workers
also publish `startup-progress-gN.json` in their owned worker directories. The
controller forwards validated observations from the current worker PID and
generation. Running observations have a 15-second age limit and one second of
permitted clock skew; readiness records are historical observations. These are
progress observations, separate from the immutable phase
metric receipts. Startup records have `banks_captured=false`; they do not claim
to flush the storage banks or prove application drain.

After workload configuration is validated, the controller emits
`target_progress ` records using
`mount-rs.target-progress.v1`, identifying the workload dimensions, verified
source revision, current phase, observed free space, completed drive
initializations and successful initial client connections. Connection counts
advance after a successful connection and remain cumulative across replica
refresh; live transport state stays in the QUIC bank. A last progress record can
locate an interrupted
run, but it cannot establish why the process stopped or whether the phase
completed.

The hosted full-target job streams only closed, scalar diagnostic records through
`scripts/filter-startup-diagnostics.py`. The helper keeps a private raw log with
mode `0600`, bounds the retained log to 64 MiB and continues draining after a
filter or output failure. Raw runtime and worker logs are excluded from public
artifacts. The filter binds the controller's PID, source revision and exact
10-server/10,000-client/10,000-drive/5,000-partition/1,000-file workload. Missing
terminal records, malformed records or incomplete accounting fail the logging
gate. The original workload exit status remains the first failure reported;
filter success is not workload qualification. Worker startup completeness also
joins the existing target metric qualification gate, so a missing worker record
cannot qualify merely because it never reached the filter.

Readiness and observation completeness are separate. A service can report ready
while an observer failure leaves `accounting_complete=false`; valid incomplete
records can still be forwarded and the diagnostic gate fails. A forced kill can
leave only the last periodic record. Do not infer a zero error count, a completed open or a
successful cleanup from an absent terminal record.

The ordinary server constructors keep the new observer disabled. Service
embedders can explicitly use `RemoteServer::bind_with_diagnostics(..., true)` in
an `io-profiling` build, retain `server.diagnostics()`, and call its `snapshot()`
at controlled boundaries. This is a local API, with no diagnostic network
endpoint.

The public `serve-remote` CLI selects the QUIC observer only when its
default-off `io-profiling` feature is compiled and `MOUNT_RS_PROFILE_IO` is
exactly `1`. The feature forwards `mount-rs-service/io-profiling` only. It does
not add SDK or provider feature forwarding; the same environment value can
independently select their existing runtime banks. A CLI build without this
feature proves service-record absence, rather than absence of every observer
or its process overhead.

Build and run the public CLI using an isolated target directory:

```sh
CARGO_TARGET_DIR=/absolute/isolated-target \
  ./scripts/cargo-shared build --locked -p mount-rs-cli --features io-profiling
MOUNT_RS_PROFILE_IO=1 /absolute/isolated-target/debug/mount-rs \
  serve-remote --config /absolute/service.json
```

After QUIC, WebSocket, filesystem runtime, storage context and cache cleanup,
the CLI captures the local service and existing process banks and attempts one
stderr line prefixed with
`service_diagnostics `. Its envelope schema is
`mount-rs.cli-service-diagnostics.v2`, with the server PID, `transport=quic`,
`capture_context=shutdown` and the unchanged `mount-rs.service-quic.v1`
snapshot. `transport=quic` describes this nested service snapshot. Added
`process_diagnostics` banks cover instrumented work throughout the process,
including SDK work shared by QUIC and WebSocket. The whole combined prefix,
JSON and newline share one 1 MiB cap before output. Overflow or
serialization failure produces a fixed `diagnostic_incomplete` record instead
of partial snapshot JSON. Diagnostic serialization and output failures cannot
replace the service or cleanup outcome. Stderr output can block at the OS;
the outer process controller owns the deadline. Forced termination can leave
no final snapshot, which remains an evidence gap.

All snapshots contain raw JSON `uint64` numbers. Consumers need a lossless
integer JSON parser for values above `2^53`, including nanosecond timestamps;
ordinary JavaScript `JSON.parse` can lose precision. This service schema does
not use the native storage v3 decimal-string counter representation.

The service snapshot is cumulative over the QUIC server lifetime, with no phase
export or diagnostic endpoint. The WebSocket listener remains outside the
service observer's coverage. Preserve its completeness, saturation,
activity, quiescence and registry-gap fields when interpreting it; endpoint
closure does not establish application drain. Registry memory and capture cost
are bounded by the configured `max_connections`. Profiling and any slow-log
overhead remain part of the instrumented process measurements.

The process section exports the existing typed storage and core profile
snapshots only when their recorders are enabled. Disabled banks have
`available=false`, `reason=observer_disabled` and no snapshot; they are not
reported as measured zero. Its scope is `process_cumulative`, with
`capture_atomic=false`, `application_drain_proven=false` and inclusive,
overlapping wall durations. Global banks survive provider retirement without
retaining stores or connections. They do not attribute totals to a particular
drive or provider instance, and zero in-flight gauges do not prove drain.

Storage retains all 78 ordered rows, outcomes, known successful payload bytes,
returned rows and row-observation availability, global/per-row in-flight
gauges, 32 latency buckets and forwarding-box provenance. Coverage labels are
derived from the producer registry: 47 `sdk.*` erased-method rows, 18 `tidb.*`
source-instrumented adapter rows, 12 NAPI `metadata.*`/`blocks.*` forwarding
rows that this CLI does not use, and one `pglite.client_lock_wait` row selected
only by that provider. Zero TiDB rows do not prove TiDB use or complete
SQL/network coverage. Core profile rows retain fixed names, calls, elapsed
nanoseconds and event-specific units.

SDK block bytes describe successful known logical payloads; SDK metadata
payload bytes are unavailable at this seam. Zero `returned_row_observations`
means the returned-row count is unknown, not a measured empty result. A
reserved row does not establish that its provider was used.

This record explicitly marks raw object-store instance statistics, HTTP
attempts, physical device IOPS, process CPU/RSS and allocator churn unavailable.
It reads no extra provider handles or resource APIs. Aligned external resource
receipts remain separate. These exports do not establish raw blob activity,
full SQLite pager coverage, wire retries/bytes, backend server resources,
phase-aligned amplification, throughput or capacity.

The boundary observer reads only the current process for process disk bytes.
Host device observations additionally require an explicit selection:

- GNU Linux: `MOUNT_RS_PROFILE_BLOCK_DEVICE` is one name under `/sys/block`.
- macOS: `MOUNT_RS_PROFILE_IOREGISTRY_ENTRY_ID` is a known nonzero decimal
  IORegistry ID for one `IOBlockStorageDriver`.

The collector does not inventory devices or choose one automatically. A missing
selection remains unselected; an invalid identity, unsupported selection or
counter reset remains unavailable. Linux counts completed block operations;
macOS counts operations processed by the selected driver. Neither selection
proves that the device backs a remote database or blob service.

## Slow-operation logging

Set `MOUNT_RS_TRACE_STORAGE=1` for storage spans or
`MOUNT_RS_TRACE_SERVICE=1` for explicitly enabled service observers. Each recorder
emits at most 16 slow records for operations taking at least 100 ms. Records use
fixed operation/outcome labels and numeric measurements; tokens, paths, SQL,
payloads and caller-provided labels are excluded. Logging can allocate and block
on stderr, so use counters without slow logs for the throughput control.

## Read the profile

Compare each family's phase counts and known bytes with acknowledged workload
operations. High SDK attempts indicate retries or repeated filesystem work;
high SQL calls per SDK operation indicate provider amplification. Catalog query
bytes expose repeated whole-document reads even when decode work is cached.
High pool/gate time with low provider execution points to contention. High
blob API calls relative to logical puts can reveal verification or conflict work.
QUIC UDP traffic above application payload includes framing and retransmission;
the retired-connection scope still applies.

Compare CPU, RSS and device deltas from the same interval, with an adjacent idle
control. Observer time is reported separately from workload time, while process
CPU includes it. Do not subtract cumulative maxima or use shared host activity
as exact provider IOPS. A profile identifies where to investigate; a performance
win needs a controlled before/after workload with matching correctness checks.
