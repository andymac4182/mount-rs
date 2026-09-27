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

## Locate the bottleneck in one measured phase

Use completed workload operations and the workload's own elapsed time for the
application rate. Keep preparation, observation, persistence verification and
teardown outside that denominator. Compare the same workload, durability,
concurrency and payload with profiling enabled and disabled before attributing
an improvement to a storage change.

| Signal | Calculation or comparison | What to investigate next |
| --- | --- | --- |
| SQL amplification | Recorded SQL-family calls / completed workload operations | Repeated namespace/inode queries, retries and transaction setup; these are statement calls, not physical IOPS |
| Blob read amplification | Raw block-read GETs / verified application reads; keep conflict verification and migration separate | Cache misses, repeated block fetches and read-before-write behavior |
| Byte amplification | Known returned or copied bytes / verified payload bytes at the same boundary | Whole-document reads, oversized chunks and buffer copies; unknown metadata bytes remain unavailable |
| Pool contention | Pool-checkout latency buckets against transaction and SQL-family buckets | Connection limits, lazy setup and contention; checkout includes setup work |
| Service contention | Admission, request and dispatch timing against SDK/backend timing | Stream admission, filesystem gates and work queued before storage |
| CPU cost | Aligned process CPU delta / completed operations, alongside digest/copy input bytes | Hashing, copies, encoding and metadata processing; nested wall spans are not exclusive CPU measurements |
| Memory cost | Aligned current RSS and allocation churn against phase boundaries | Retained handles, caches and temporary buffers; lifetime peaks do not establish simultaneous fleet memory |
| Physical storage activity | Available operation deltas / their own observed interval | A stable explicitly selected device or owned daemon counter; missing operations cannot be derived from byte totals |

Retain incomplete observations and original floor failures. A high adapter-call
ratio locates work above the backend; it does not establish the backend's device
amplification. A long inclusive span locates elapsed time; separate CPU and
resource observations are needed to distinguish computation from waiting.

Native storage diagnostics use `mount-rs.storage-diagnostics.v3`, with exact
decimal strings for counters. The closed row registry and the instrumented-row
coverage are separate: reserving a row does not prove that a provider records it.
The object-store API sub-schema remains `mount-rs.object-store-api.v1`.

Registered split R2 and RustFS block stores have separate `r2` and `rustfs`
families. Each exposes logical calls/cache hits, ten raw adapter rows, upload
leader/follower claims and seven local work rows. Measurement fields identify
the corresponding provider scope. Equal instance IDs in different families do
not identify the same store. Weak registrations retire after the last handle
drops; an instance that disappears across a phase leaves that phase incomplete.
Marker verification and qualification probes remain outside these blob banks.

The benchmark retains RustFS deltas and emits `rustfs_local_work` separately in
its existing `MOUNT_RS_STORAGE_PHASE` summaries. A required raw workload proof
selects the explicit `rustfs` family and rejects an absent or empty registry.
Older observations without that family do not establish zero RustFS traffic.
This export reuses the existing recorders; constructor registration and JSON
publication can allocate. It adds no new per-operation recording sites.

## Follow the HTTP parity fixture startup

`scripts/check-http-parity.mjs` emits JSON records with schema
`mount-rs.http-fixture-progress.v1`. Seven parent-observed events distinguish
launch requested, Cargo child spawned, first complete stdout line or stderr
chunk, valid
readiness received, the existing watchdog expired and the Cargo child exited.
Exit remains observable after readiness. Elapsed milliseconds use the parent
monotonic clock. Payloads, arguments, environment values, paths and arbitrary
error or signal text are excluded.

The application also writes `mount-rs.http-oracle-startup.v1` records at eight
fixed boundaries: async main body entry, S3 create start/completion, WebDAV
create start/completion, WebDAV listen start/completion and readiness publication.
Their elapsed milliseconds start inside the application. The parent relays only
validated, bounded records from the existing stderr handler; its original error
collection remains intact. The application PID is self-reported and can differ
from the Cargo PID. This is not an OS identity check.

Cargo runs with `--quiet`, so compilation and Tokio initialization before the
async main body remain unobserved. The launcher uses `scripts/cargo-shared run
--locked`; CI prebuilds the same example with the same wrapper and default
profile. This removes a source-visible target mismatch. It does not establish
that the mismatch caused the earlier Intel timeout. The startup watchdog remains
120 seconds. Logging adds no retry or change to readiness parsing.

The parent observer, application observer and child-record relay each permit at
most 16 records of 512 UTF-8 bytes including the newline. The relay rejects
oversized, malformed and unknown records without forwarding raw stderr. Clock
failures produce a fixed unavailable reason; sink errors or partial writes stop
publication with no retry. These observations preserve the fixture's outcome and
child cleanup policy. Pure controls and Rust helper tests validate the logging
contract; an actual hosted fixture run is still needed for startup evidence.
Publication uses a synchronous stderr write: byte and attempt caps do not bound
sink latency, and logging can delay parent event handling. A failed sink's fixed
reason remains in the helper receipt; the launcher does not consume that receipt.

## Follow the PGlite fixture startup

Direct execution of `tests/pglite/server.mjs` writes
`mount-rs.pglite-startup.v1` records. Library imports keep reporting disabled
before diagnostic clock or sink calls. Fifteen fixed stages locate module body
entry, the two ordered pinned package imports, configuration, cleanup shim
installation, engine creation, socket construction/start and readiness
publication. They use the Node process PID and elapsed monotonic milliseconds
from the first observed stage. Earlier Node startup remains unobserved.

The logger allows 16 attempts and 512 UTF-8 bytes per record, reserving one
attempt for observer unavailability. It excludes paths, connection settings,
credentials and arbitrary errors. Clock and sink failures stop reporting;
synchronous output has no sink-latency guarantee. The helper's local receipt
retains publication failures, but the server does not consume it.

The existing readiness line, 30-second Rust helper deadline, process outcome and
cleanup stay in place. No Cargo process runs inside this Node fixture. A last
checkpoint can locate a startup stall; it cannot prove its cause or successful
cleanup. Pure import/operation controls do not start the engine or socket.

## Follow the S3 provider network test

The existing N-API S3 NodeFs/SQLite network concurrency test emits
`network_test_progress ` records with schema `mount-rs.network-test-progress.v1`.
Each case publishes at startup, at completion and on a cooperative five-second
timer. Its first rejected request publishes immediately and freezes the receipt;
late settlements cannot rewrite the pending counts. Non-request failures publish
at completion. Logging performs no server or provider calls.

Eight fixed rows distinguish concurrent and streamed PUT/GET fetches from their
response-body promises. Each row counts issued, fulfilled, rejected and in-flight
client promises. A fulfilled fetch can still have a failing HTTP status; these
counters do not establish server acceptance, HTTP success or backend IOPS.
The existing status, complete-byte equality and native request-stat assertions
remain the success checks. Default concurrency remains 32 and request timeouts
remain 15 seconds.
Set `MOUNT_RS_S3_NETWORK_PROGRESS=0` for a disabled control; it returns before
added resource reads, clocks, publication and scheduling.

CPU fields are Node process deltas in microseconds. Current RSS includes the
embedded native addon; peak RSS is the OS process-lifetime peak, including before
the case baseline. These observations are non-atomic and do not prove application
drain. CPU or memory of a separate backend service is outside this process scope.
Resource, clock, timer and sink failures retain a fixed unavailable reason and
preserve the test outcome. A stopped sink cannot publish its own failure; the
local receipt still retains it.

Each record is bounded to 4096 bytes including prefix/newline; each case allows
at most 16 publication attempts. The observer stops publishing on sink errors or
backpressure and does not retry them. Labels and failure categories are closed;
records contain no object paths, bodies or arbitrary error strings. This logger
is test-only and allocates at publication boundaries. It does not establish
allocation-free request processing or resolve a timeout by itself.

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

The production-target controller also forwards `resource_progress ` records
using `mount-rs.resource-progress.v1`, from its existing 100 ms process sampler.
Each record has 29 fixed fields and a 2 KiB limit including prefix and newline.
Publication follows the existing five-second progress cadence and phase
boundaries; it adds no process sampler or device query. An ordinary disabled
publication path returns before added sample reads, cloning, clocks or
serialization.

| Resource scalar | Meaning |
| --- | --- |
| `cpu_user_us`, `cpu_system_us` | Cumulative CPU microseconds since the sampler baseline |
| `rss_current_bytes` | Current process RSS |
| `rss_lifetime_peak_bytes` | OS peak over the whole process lifetime, including before the sampler baseline |
| `rss_peak_bytes` | Existing maximum of observed current RSS and OS lifetime peak |
| `minimum_host_free_bytes` | Minimum observed filesystem availability at `/System/Volumes/Data` on macOS or `/` elsewhere |
| `block_inputs`, `block_outputs` | Raw `getrusage` block accounting counts; these are neither bytes nor physical IOPS |
| `process_disk_read_bytes`, `process_disk_write_bytes` | Null, with `not_captured_by_sampler`; this sampler leaves process disk accounting disabled |

The envelope binds the controller and owned process PID, worker index, verified
source revision, source digest, binary digest and current phase. Nullable
`generation_context` identifies an accepted startup or readiness context at
publication; the resource sample itself does not carry generation identity.
Counters remain cumulative across replica reopen generations. Digests pinned
by the log filter establish consistency within the stream; separately joined
source and binary artifacts establish the build identity.

`observed_unix_ms` is the sampler's wall timestamp after capture;
`published_unix_ms` is the later projection time. `sample_interval_ms=100` is
the configured interval, rather than a measured sampling gap. The existing
ten-second freshness rule remains in force. The baseline has no recorded
timestamp, so these records do not directly report average CPU cores or prove
continuous 100 ms coverage. Separate process lifetime peaks must not be summed
as a simultaneous fleet peak.

Before resource coverage is expected during early worker configuration, an
absent file produces no resource record. Once coverage is expected, missing,
malformed, stale, foreign-PID or failed observations produce a fixed
unavailable reason and null sample measurements. Historical values from a
failed sample are not projected as current. Unavailable records and output
failures leave publication incomplete. `terminal_sample=true` identifies a
terminal capture; it does not alone establish sampler join, application drain
or successful cleanup.

During cleanup, the controller accepts a worker's fresh terminal sample at an
owned polling or reap boundary. It then retains that observation as historical
coverage, instead of rereading accepted terminal or retired workers as current
samples after later cleanup work. CPU/RSS coverage ends at the sampler's
terminal capture; earlier observation failures remain incomplete. The new
resource-file reader requires a regular file, rejects symlinks and special
files, and bounds reads to 16 KiB before projection.

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
The filter additionally checks the resource schema, process associations,
phase and source identity, lossless integer ranges and cumulative counter
regressions. Its summary schema is `mount-rs.startup-log-filter.v2`, with a
`resource_records` count. Unavailable resource records remain printable while
failing the filter. Raw sampler errors, paths and nested process receipts are
excluded from these public records.

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

## Separate blob-local work from storage waits

The native storage benchmark exports an optional `local_work` sibling beside
object-store API counters, using `mount-rs.object-store-local.v1`. Its seven
fixed rows identify work inside one object-store block-store instance:

| Row | What is timed | Byte observations |
| --- | --- | --- |
| `sha256.digest` | SHA256 computation | Input length; 32 bytes after a completed digest |
| `block_id.encode` | Encoding the digest as a block ID | 32 input bytes; 65 output bytes on completion |
| `copy.upload_payload` | Creating the upload payload buffer | Actual copied bytes |
| `copy.cache_insert` | Copying an admitted payload into the local cache | Actual copied bytes |
| `copy.return_vec` | Copying a returned cache, cold-read or migration payload | Actual copied bytes |
| `cache.lock_acquire` | Waiting to acquire the cache mutex | No payload bytes; excludes lock hold time |
| `put.follower_wait` | Waiting for another upload of the same block | No payload bytes |

The bank retains a total in-flight gauge. Each row retains calls,
success/error/cancellation outcomes, inclusive elapsed nanoseconds and 32
latency buckets. Input bytes count entered
work; output bytes count completed work. A successful digest remains a success
if its later integrity comparison rejects the block. Follower leader-errors and
caller cancellations have separate outcomes.

`measurement.r2_local` states the scope, byte units, latency meaning and exclusions. The bank construction and export costs are documented separately. Missing, disabled,
invalid, reset, saturated or active-at-boundary local observations stay
unavailable or incomplete; they are not measured zero. Local validation is
separate from the existing raw blob API qualification checks. Snapshots read
relaxed counters separately and are non-atomic; an observed zero in-flight gauge
does not prove application drain. Phase reports
retain cumulative maximum latency at both boundaries; subtracting maxima does
not produce a phase maximum.

The existing `MOUNT_RS_STORAGE_PHASE` summary includes seven fixed local totals
and observed/unavailable instance counts. Full phase JSON retains the rows and
metadata. Enable collection with `MOUNT_RS_PROFILE_IO=1` before native addon
construction, and compare with the same workload with profiling disabled.

Fixed recorder updates and the fixed `LocalState`/`RawState` snapshots allocate
nothing. Enabling the bank adds state to the existing per-store allocation.
The public `ObjectStoreBlockStore::stats()` error-class map, NAPI JSON exports
and full observations can allocate. These statements do not claim
allocation-free file operations. Cache lock hold time, cache bookkeeping and
other allocation sites remain outside these seven timed rows.
The adapter applies no compression. Copy bytes are local buffer traffic, not
wire bytes, and these timers are not exclusive CPU time or physical IOPS.
The public CLI shutdown envelope still reports raw object-store instance
statistics unavailable; this local bank is exported through the native storage
benchmark path.

## Observe owned backing resources

The owned-backing pilot samples only its configured container identities. It
requests streaming Engine stats and selects the first complete JSON object,
requires the observed full container ID, and retains the existing image,
ownership, start and restart checks. Missing identity makes capture incomplete.
Missing counters remain unavailable and can prevent interval qualification;
expected identities and zero counters are never substituted.

Capture runs in parallel across at most 16 owned containers, with inspection and
stats requests serial within each container. Replies remain in the configured
order. Selecting the first streamed frame avoids the non-streaming API's forced
second sample. Success waits for the exact response, request and assigned socket
to close before releasing that container's request slot. Inspection, first-frame
delivery or retirement can still exceed the unchanged two-second hook deadline;
late or unretired requests remain incomplete.

The one-MiB response cap applies to chunks yielded by the adapter, including any
coalesced trailing frame. An overflow chunk rejected by the adapter has
unavailable byte accounting; these are not wire bytes. The retained stats digest
covers only the selected first-frame prefix through its closing brace, including
leading whitespace. Ordinary version and inspection responses retain their
whole-body EOF semantics.

Fixed diagnostics count received headers, first frames, retired streamed
iterators and consumed trailing bytes. Pending header, body and retirement
stages report bounded counts and ages; successful captures retain their stage
timestamps. They contain no raw headers or arbitrary errors. Missing retirement
never becomes a fabricated terminal observation.

The enclosing hook records wall and process CPU cost once. Summed per-request
costs include overlapping windows and must not be treated as exclusive time.
The workload timer is separate from observation time; backing counter windows
can include observation and idle activity. Compare their retained timestamps
before interpreting a rate.

Cgroup block byte counters accept the documented read/write spellings. Missing
block operation counters remain unavailable, even when byte counters exist.
Container block accounting, process disk bytes and shared host device counters
are different observations; none establishes physical NAND IOPS.

## Preserve the first cache RSS failure

The native cache qualification controller retains the first fatal RSS
observation using `mount-rs.cache-rss-failure.v1`. Its 15 fixed fields identify
the owned run, candidate PID, role, generation and server index, sampling site,
typed failure category and retirement result. The timestamp is null when that
sampling path has no existing common-clock observation; recording adds no clock
query. The resource sequence names the last published worker frame rather than
inventing a frame for the failed sample. Records exclude paths, launch arguments,
arbitrary error text and command output.

An unavailable sample excused by the existing fresh reap of the same owned
child leaves the failure latch empty. An unexcused failure is retained before
discarding a candidate or rebuilding the roster. Later cleanup failures and
successful samples cannot replace it. Controller supervision retains its own
first record in `controller.json`; it preserves the existing polling and reap
rules.

The worker includes its record in `resource_evidence()` and attempts one
`rss-first-failure.json` write, bounded to 2 KiB. Separate retention status is
`not_attempted`, `written` or `write_failed`. A diagnostic write failure preserves
the original RSS failure and cannot permit qualification. Healthy samples add
no file write or extra sampling. Existing resource frame schemas, caps and
deadlines remain unchanged.

CI copies the optional worker file only after a bounded regular-file read,
rejecting symlinks, special files, duplicate or unknown fields, invalid scalar
types and enum values. An absent record is not a healthy-sampling assertion.
This diagnostic locates a failed observation; it does not establish resource
coverage, drain or successful cleanup.

The outer supervisor also rechecks the same retained worker `Child` with
WNOWAIT when its typed unavailable RSS sample could be caused by an intervening
exit. Confirmed exit returns to the original cleanup receipt, process group and
reap checks. It adds no RSS sample or fabricated zero. An already-fatal controller
sample, running worker, cap violation, poll error or other read/clock error
remains a failure. This closes a source-proven retirement race; it does not
identify the cause of an older hosted failure.

## Layout comparison metric retention

`projectOwnedLayoutPhaseMetrics` retains one original 4096-byte workload phase
as `mount-rs.owned-layout-phase-metrics.v1`. It validates the original RustFS
raw diagnostic contract and emits closed names and exact decimal counters:

- Provider and TiDB operations: calls, outcomes, inclusive time, latency buckets,
  known payload bytes, returned SQL rows and row-observation counts.
- RustFS instances: logical/cache counts, raw blob operations, upload/read bytes,
  claim counts and optional digest, encoding, copy, lock and follower-wait work.
- Process CPU work and observer CPU cost, context switches and fixed memory
  endpoints when the original process measurements are available.
- Known forwarding box calls/requested future bytes and fixed core events such
  as namespace serialization, old-chunk reads and conflicts when available.

Missing optional measurements remain unavailable. Forwarding box counts cover
one instrumented allocation site; total allocations remain unavailable. Memory
is an endpoint, and nested elapsed times overlap. Blob calls are adapter calls,
with internal HTTP retries unobserved. These counters do not establish physical
device IOPS, SQL wire bytes or exclusive CPU time.

The projection rejects accessors, sparse arrays, malformed counters and evidence
above its bounds. It retains up to 64 stable registered instances, with an 8 MiB
private input bound and a 2 MiB closed output bound; required evidence is never
truncated into an accepted observation. Controls use modeled evidence and open
no native addon or storage backend.

## Slow-operation logging

Set `MOUNT_RS_TRACE_STORAGE=1` for storage spans or
`MOUNT_RS_TRACE_SERVICE=1` for explicitly enabled service observers. Each recorder
emits at most 16 slow records for operations taking at least 100 ms. Records use
fixed operation/outcome labels and numeric measurements; tokens, paths, SQL,
payloads and caller-provided labels are excluded. Logging can allocate and block
on stderr, so use counters without slow logs for the throughput control.

## Read the profile

For `benchmarks/storage/runner.mjs --layout compact`, the artifact must include
a validated `layoutSelection.persistedReceipt` before timed I/O starts. The
five fields report MRC5, an opaque backing identity, a decimal u64 structural
generation and successful verification against the same opened block handle.
The inspector queries persisted provider state; it does not infer format from
constructor flags. `null` means validated non-MRC5, and proves no exact legacy
format. Unsupported, malformed and failed queries fail the compact run and
keep cleanup; an unsettled query defers provider shutdown. Inspection is outside
the workload numerator and timed interval, allocates its receipt, and is neither
an atomic snapshot nor a crash durability test. Existing legacy owned pilot
results remain legacy evidence.

Compare each family's phase counts and known bytes with acknowledged workload
operations. High SDK attempts indicate retries or repeated filesystem work;
high SQL calls per SDK operation indicate provider amplification. Catalog query
bytes expose repeated whole-document reads even when decode work is cached.
TiDB pool checkout includes waiting, lazy connection creation and session setup;
it does not isolate queue contention. Compare it with session configuration,
schema initialization and metadata-open counts before attributing the time to
contention. High
blob API calls relative to logical puts can reveal verification or conflict work.
QUIC UDP traffic above application payload includes framing and retransmission;
the retired-connection scope still applies.

Compare CPU, RSS and device deltas from the same interval, with an adjacent idle
control. Observer time is reported separately from workload time, while process
CPU includes it. Do not subtract cumulative maxima or use shared host activity
as exact provider IOPS. A profile identifies where to investigate; a performance
win needs a controlled before/after workload with matching correctness checks.

### Authentication stages

Enabled QUIC observers retain seven fixed rows for `CatalogAuthenticator`,
inside the inclusive `auth.authenticate` span:

| Row | Work measured |
| --- | --- |
| `auth.decode` | Token and header decoding, including claimed issuer extraction |
| `auth.catalog` | Current catalog load and requested partition lookup |
| `auth.cache.wait` | Awaiting the existing authentication cache mutex |
| `auth.policy.select` | Each policy candidate's eligible-grant scan, configuration and freshness checks |
| `auth.key.fetch` | Each actual key-source fetch, including failed fetches |
| `auth.jwt.verify` | Each actual JWT verification, including a failed verification before refresh |
| `auth.grant.authorize` | Verified principal expiry and allowed-drive evaluation |

Skipped policy candidates complete selection successfully; that is not an
authentication success. Failed key fetches remain errors when the existing
negative cache retains them. The original cache locking, refresh rules and
authorization decisions are unchanged. Cache pruning, identity construction
and other residual work remain in the outer span. Fetch time does not separate
DNS, connection setup, HTTP and key parsing.

The caller's task-local observer is active while its authentication future is
polled and is restored across yields and cancellation. Both initial QUIC
authentication and renewal use the unchanged 30-second deadline. A dropped
substage records cancellation; an outer deadline records timeout. Custom
authenticators and unscoped calls do not supply these catalog substage counts.
The current WebSocket path has no scoped authentication observer.

Standalone observer helper controls measure no new heap objects or requested
bytes with a preconstructed observer and tracing disabled. They do not measure
the real authenticator's boxed future size, JWT/catalog allocations or observer
construction. Additional future state can increase the existing future's
requested allocation size. Spans finish and release their observer references
before later stages; diagnostic snapshots and slow logs remain outside that
allocation claim.

### Scale-test receipt overhead

The production-target harness checks persisted MRC5 metadata and the selected
block backing during initialization, each server generation and fresh oracle
passes. These checks belong to setup and verification, outside the active I/O
numerator. For a complete run with `D` drives, the source schedules `23D`
receipt checks. Previously each TiDB receipt opened two private provider pools,
or `46D` pool constructions over the run; that arithmetic does not count live
connections or prove an observed completed run.

Receipts now open fresh provider handles through the caller's `StorageContext`.
The initializer and server retain their own contexts, and each fresh oracle
still creates and closes an independent context. Every receipt rereads persisted
mode, verifies the selected block backing and awaits handle cleanup. This
removes the receipt's private pools without caching layout authority. Actual
TiDB startup savings require a matched live measurement; local SQLite and
cleanup controls establish correctness, not TiDB throughput.
