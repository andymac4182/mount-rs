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
| FoundationDB adapter | Transaction creation, closure attempts, point reads, selected-key reads, range pages, commit and explicit retry recovery | Client dispatch and closure attempts; range pages are distinct from returned keys, wire RPCs and device I/O |
| Object-store block adapter | Actual get/put/head/delete invocations and body reads, reason, outcomes, claim leader/follower counters | Adapter API calls and known body bytes; internal HTTP retries and reconciliation listing remain unavailable |
| QUIC server | TLS/application handshake, authentication, admission, request/read/dispatch/encode/submit/cleanup, latency buckets and gauges | Inclusive application spans; submission to Quinn does not establish peer acknowledgment |
| QUIC authentication | Token decode, catalog load, key-cache wait, policy selection, key fetch, JWT verification and grant authorization | Inclusive stage times within the existing handshake/renewal deadline; key fetch is only recorded when requested |
| Accepted QUIC connections | UDP bytes/datagrams/I/O calls, frame counters, path observations, retained retired-connection totals | Accepted connection lifetime through session retirement; excludes refused/failed TLS connections and subsequent transport traffic |
| Process | CPU, RSS, optional Rust allocation churn and process disk accounting | Includes observer/background work; process disk bytes are not physical operation counts |
| Selected host device | Optional block/driver operation and byte counters | Shared device activity, including other processes; requires stable device identity and is not NAND I/O |

Fixed storage rows have terminal outcomes, in-flight gauges, inclusive elapsed
time and 32 logarithmic latency buckets. Returned-row observations distinguish
known zero rows from an unavailable row count. Nonzero values in nested families
must not be added as unique application operations or exclusive CPU time.

For slow-operation logs, set `MOUNT_RS_TRACE_STORAGE=1` with profiling before
constructing stores. Operations taking at least 100 ms emit
`MOUNT_RS_STORAGE_SLOW` with the fixed operation label, outcome and elapsed
microseconds. Logs contain no storage keys, paths or raw errors. Leave tracing
off for the allocation and throughput baseline; writing diagnostic output adds
observer work.

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

Unix CI requests a per-platform `owned-layout-controls` artifact containing TAP
output from the comparison controls, including on failure. The pipeline retains
the test exit status and existing deadlines. Verify the uploaded artifact before
using its test names or counts; an empty check-API response supplies neither.

## Compact snapshot revision experiment

A public SQLite regression reproduced a prepared create reporting one conflict
and entering whole-file replay without any competing writer. Installing a
validated, unchanged compact snapshot advanced the local revision. The installer
now preserves that revision when the full anchor and physical identities match,
after the existing checks establish exact effective inode-body equality. It
still installs the fresh Full capture and folds selected-inode caches.

The isolated create now records zero conflicts/replays and one committed reply.
A second public control pauses its blob PUT, commits a real peer inode update
without changing the anchor, and verifies that the prepared create still
conflicts and replays. Both controls verify complete bytes and EOF after fresh
SQLite connections reopen. Additional controls cover selected-cache folding,
revision exhaustion without partial state changes, and advanced identities with
unchanged bodies. This is a correctness fix; it does not remove Full captures.

The same local native API workload was run before and after the fix: 400 fresh
file write/read/delete lifecycles, concurrency 64, 4,096-byte payloads,
65,536-byte chunks and an unchanged 1,000 logical operations/sec floor. Each
arm verified all 400 reads and completed all 1,200 logical operations without
timeouts or cleanup failures. Fresh native artifacts were independently hashed
and joined to their captured Rust sources; the installed addon was untouched.

| Layout / profiling | Baseline operations/sec | Candidate operations/sec |
| --- | ---: | ---: |
| Compact / enabled | 1,015.45 | **947.80 — floor failed** |
| Compact / disabled | 1,049.38 | 1,042.37 |
| Legacy / enabled | 1,275.45 | 1,126.73 |
| Legacy / disabled | 1,518.08 | 1,399.35 |

These are single controls in the same execution order, with uncontrolled host
and OS cache activity. They establish neither a causal speedup nor an isolated
instrumentation overhead estimate. Retain the candidate's profiling-enabled
failure. The unchanged legacy path also varied between the measurements.

| Profiled compact workload counter | Baseline | Candidate |
| --- | ---: | ---: |
| Conflicting prepared requests / whole-file replays | 400 / 400 | 393 / 393 |
| Full metadata captures / publications | 838 / 419 | 831 / 419 |
| SQLite statement calls, both stores | 28,399 | 28,357 |
| Inode bodies returned | 244,906 | 243,801 |
| Known inode-body bytes returned | 118,507,084 | 118,101,202 |
| Initial block PUT input calls / bytes | 400 / 1,638,400 | 400 / 1,638,400 |

The remaining work is visible: most prepared creates still leave the batch and
take the serial replay path, while Full captures and structural publications
materialize complete guard sets. Selected inode reads also load and validate
the anchor. The conflict counter does not distinguish its revision, allocation
or path predicate, so attributing every conflict to one predicate needs another
observation. The next comparison should also separate fresh file creation from
updates to existing files. Compression or chunk sizing is not yet established
as the limiting factor.

Creation, workload and cleanup diagnostics were complete and quiescent in the
profiled arms. Shutdown diagnostics remained incomplete because their SQLite
connections had closed; the original diagnostic issue is retained. These runs
provide no mounted-path, server-fleet, physical device IOPS, total native
allocation or 10,000-client qualification.

## Retained example: FoundationDB and Ozone

The [Ozone FoundationDB job](https://github.com/andymac4182/mount-rs/actions/runs/36286931056/job/108529427341)
retained a legacy-layout run with 400 lifecycle iterations, concurrency 64,
4,096-byte payloads and 65,536-byte chunks. Its verified artifact
`10920877514` contains matching standalone and log-embedded benchmark JSON.
All 400 writes, reads and deletes succeeded; 1,200 logical operations over
2,389.66 ms produced **502.16 IOPS**, below the unchanged 1,000 floor.
The recorded failure is `IOPS_TARGET_NOT_MET`, with zero timeouts or cleanup
failures in the workload summary.

| Workload observation | Retained value |
| --- | --- |
| Write latency, median / p95 / p99 | 345.96 / 429.26 / 431.06 ms |
| Read latency, median / p95 / p99 | 33.07 / 155.38 / 174.08 ms |
| Filesystem gate waits | 2,968 calls; 93.85 s summed inclusive time |
| Blob upload follower waits | 688 calls; 15.60 s summed inclusive time |
| Namespace serialization | 15,337,494 bytes across 650 publications |
| Block put / get adapter calls | 707 / 400 |
| Object create calls / conditional conflicts | 19 / 18 |
| Read cache hits | 400 |
| Node process CPU, user / system | 538,983 / 177,420 microseconds |
| Node process RSS at workload end | 120,762,368 bytes |
| Dynamic provider forwarding futures | 2,771 requests; 255,160 requested object bytes |

These counters expose contention and repeated work at different boundaries.
The concurrent wait sums exceed workload wall time and cannot be added or
treated as exclusive time shares. The 707 adapter puts are not 707 physical
uploads; the cached reads do not measure backing-store read saturation.

This artifact supplies no physical operation counts, HTTP wire attempts,
total allocator counts, FoundationDB transaction/RPC breakdown or container CPU
intervals. Its declared native hash has no independently verified source/build
join. It therefore identifies this job's threshold failure and descriptive
hotspots, while a matched comparison is still needed to isolate their cost.
The forwarding-future counter covers one specific boxing site; requested object
bytes are separate from allocator traffic, retained memory and lifetime peaks.

## Latest instrumented Ozone observations

The [PR CI run](https://github.com/andymac4182/mount-rs/actions/runs/36295144716)
retained FoundationDB artifact `10924420071` and TiDB artifact `10924080720`.
Their archive digests and all four log/JSON member digests were independently
checked. Both benchmarks used the legacy layout, 400 lifecycle iterations,
concurrency 64, 4,096-byte payloads and 65,536-byte chunks. Each completed
400 writes, reads and deletes and verified all 400 read payloads, with zero
workload timeouts or cleanup failures. Both retained `IOPS_TARGET_NOT_MET`
against the unchanged 1,000 floor.

| Workload observation | FoundationDB/Ozone | TiDB/Ozone |
| --- | --- | --- |
| Logical operations / measured wall time | 1,200 / 2,815.36 ms | 1,200 / 2,884.30 ms |
| Lifecycle operations/sec | 426.23 | 416.05 |
| Write latency, median / p95 / p99 | 395.81 / 461.31 / 462.24 ms | 380.92 / 818.97 / 820.07 ms |
| Read latency, median / p95 / p99 | 66.63 / 158.24 / 162.09 ms | 15.66 / 30.21 / 30.42 ms |
| Filesystem gate waits, calls / inclusive time | 3,113 / 97.83 s | 3,059 / 98.73 s |
| Block put calls / inclusive time | 752 / 21.34 s | 737 / 27.27 s |
| Upload follower waits, calls / inclusive time | 728 / 20.44 s | 721 / 26.51 s |
| Namespace serialized bytes / publications | 16,410,569 / 743 | 15,863,601 / 705 |
| Block create calls / conditional conflicts | 24 / 23 | 16 / 15 |
| Verified read cache hits | 400 | 400 |
| Workload process CPU, user / system | 665,078 / 172,360 microseconds | 242,270 / 81,320 microseconds |
| Workload process RSS at end | 122,839,040 bytes | 101,818,368 bytes |

The new FoundationDB rows recorded 743 successful transaction creations,
743 closure attempts, 743 commits and 2,972 point gets. Selected-key reads,
range pages and explicit `on_error` recovery were zero in this workload.
Those observations separate provider attempts from block adapter work;
they do not establish the absence of client-internal retries or wire RPCs.
The hosted log also retained passing results for the 31 transaction-driver
controls, the retry-policy control and both NAPI snapshot/coverage controls.

The 752 and 737 block put calls respectively carried 3,080,192 and 3,018,752
bytes: exactly 4,096 bytes per call. This workload therefore did not pad new
4 KiB files into 64 KiB stored blocks. Its block-adapter write byte ratios were
1.88 and 1.8425 against the 400 acknowledged payloads; these are separate from
the actual object-create and conflict-verification counters. Both runs confirmed
one new object create and served all normal reads from the adapter cache.
They do not exercise cold backing reads or demonstrate backing-store saturation.

Gate and follower waits identify caller contention to investigate next. Their
concurrent, nested totals overlap and cannot be added into exclusive time shares.
A matched layout comparison is needed to isolate the cost of namespace
publication and preparation work. A backing-store saturation experiment also
needs distinct payloads and an explicitly observed cache condition.

These are observations from this CI run, rather than a clean source/binary
qualification or backend ranking. The FoundationDB benchmark could not verify
its container checkout revision; TiDB recorded PR checkout revision
`f9b9b5e160a06b2a2f98a231a63de9bfa45e87e6` with one dirty entry. Native files
were hashed, but their exact source/build joins remain unverified. The workload
metric phases were complete; both shutdown phases retained incomplete native
observations after the object-store instance closed. Native/JavaScript allocator
counts, server CPU and device operation counts remain unavailable.

## Independently check retained comparison records

Using Node 24, run the offline verifier against the two retained outputs, the four original
handoff files, the exact selected native file and the source checkout:

```sh
node scripts/verify-owned-layout-comparison.mjs \
  --projection /private/owned/comparison.json \
  --originals /private/owned/originals.json \
  --fixtures /private/owned/fixtures.json \
  --controller /private/owned/controller.json \
  --engine /private/owned/engine.json \
  --build /private/owned/build.json \
  --native /private/owned/mount-rs.node \
  --checkout /absolute/source/checkout
```

The Linux/macOS CLI reads bounded regular files and rechecks all 35 file
identities, nanosecond timestamps and canonical paths before replay. JSON inputs
require current-user ownership, mode 0600 and a mode 0700 parent. Keep the
originals private: they can contain configuration and raw benchmark errors.
Read checks observe boundaries; they do not prove interval immutability or an
unconditional filesystem deadline. The verifier dispatches no native code,
benchmark, backend request or subprocess.

The closed `mount-rs.owned-layout-independent-check.v1` report separates:

| Field | Meaning |
| --- | --- |
| `status` | Retained records are consistent, incomplete or rejected; exit 0 means consistent only |
| `floor_qualified` | Original workload floor checks passed; a sole floor failure retains its original failed status |
| `comparable`, `safe_to_continue` | Recomputed four-arm consistency and continuation safety, separate from the floor |
| `native_uncertainty` | Retained uncertainty remains sticky; absent evidence is not a zero counter |
| `hosted_qualified` | Always false: actual owner capacity and final teardown evidence are still missing |

Each receipt digest is checked against its actual bytes and its parsed value.
The 28 source hashes cover reviewed runtime/controller seams, rather than the
full native build dependency closure. Build flags are retained declarations;
the verifier does not independently observe a clean build. It reconstructs
selected counter intervals, preserving null totals and separate partial totals.
Discarded inspect/stats bodies cannot be independently rehashed. ABBA accounting
remains descriptive; this check does not establish a causal speedup or physical
IOPS. Unix CI retains the verifier controls in the existing TAP artifact.

## Comparison receipt producers

`scripts/owned-layout-handoff.mjs` exposes five pure receipt APIs for supplied
controller observations, native/source bytes, Engine capacity, resource
observations and final owner cleanup. Each returns private receipt JSON and a
closed public summary containing status, digest, size and counts. Preserve
`private_json` privately because it can contain endpoints and resource identities.
The public `hosted_qualified` field remains false; actual owner execution,
native builds and joined lifecycle verification are pending.

`scripts/owned_layout_process.py` supplies the pure supervision and
retain-before-release logic. Its exact 19-field receipts preserve raw child
return codes, observed supervisor signals, timeouts and unavailable wait/group
observations, even after later settlement. Modeled controls exercise injected
process, group and clock operations. The receipt writer creates an exclusive
mode 0600 file in a canonical, current-user-owned mode 0700 directory. It checks
the bounded bytes, parent and file identities, and closes both owned file
descriptors before successful publication. Failed publication retains any
private partial file, blocks PID-file release and requires a new exclusive path
for retry. These checks observe publication boundaries; they do not establish
crash durability or interval immutability. Integration with the actual owner
helpers remains pending.

Unix CI retains the Node receipt controls with `owned-layout-controls.log` and
the Python models with `owned-layout-process-controls.log`. The writer's 21
filesystem controls are retained in `owned-layout-process-writer-controls.log`;
the supervisor has 35 modeled controls. Test success covers these controls;
owner teardown and workload qualification require actual runs.

## Native diagnostic schemas

Native storage diagnostics use `mount-rs.storage-diagnostics.v3`, with exact
decimal strings for counters. The closed row registry and the instrumented-row
coverage are separate: reserving a row does not prove that a provider records it.
The object-store API sub-schema remains `mount-rs.object-store-api.v1`.

### FoundationDB attempts

The expanded bank appends these seven rows to the existing 78 rows:

| Row | Boundary | Known successful bytes |
| --- | --- | --- |
| `foundationdb.transaction.create` | Native transaction creation | None recorded |
| `foundationdb.transaction.closure_attempt` | Each invocation of the transaction closure | None recorded |
| `foundationdb.read.get` | Point-read dispatch and wait | Returned value length; absent value is known zero |
| `foundationdb.read.get_key` | Selected-key dispatch and wait | Returned key length |
| `foundationdb.read.get_range_page` | Each dispatched range page and wait | Returned key and value lengths |
| `foundationdb.transaction.commit` | Commit dispatch and wait | None recorded |
| `foundationdb.transaction.on_error` | Explicit retry recovery and backoff wait | None recorded |

`measurement.foundationdb_coverage` distinguishes audited source wiring from
feature-disabled or unsupported builds. Its site counts describe six transaction
runners, two point-read helpers, one selected-key helper and three range
consumers. They are source counts, rather than observed attempts. A feature-off
bank reserves the seven rows but reports the FoundationDB families as
unavailable. Old 78-row snapshots remain incomplete under the 85-row contract.

Compare closure attempts with successful transaction creations to expose
retries, and inspect commit and `on_error` latency separately. These are nested,
inclusive wall spans. They do not report internal client retries, wire RPCs,
wire bytes, server execution time or physical IOPS. Returned-row observations
remain SQL-only. Cancellation records an abandoned observed wait; it does not
establish native operation settlement, rollback or durability after a commit
error.

The explicit allocation gate measures the warmed global core `Span` lifecycle
for all seven rows, with tracing off. It excludes first initialization and
native client futures. Its integration test is intentionally ignored by ordinary
suites and is selected explicitly in observability CI with
`--ignored --exact warmed_core_spans_record_without_added_allocations` and
`MOUNT_RS_PROFILE_IO=1 MOUNT_RS_TRACE_STORAGE=0`. CI retains its output in the
`storage-allocation-controls` artifact; a default ignored result does not
qualify this gate.

### Object-store registrations

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


## Filesystem causal profile rows

The opt-in core profile contract retains its original 47 names and appends 61
fixed causal rows, for 108 total. This is separate from the unchanged 85-row
storage operation bank. Each core row retains decimal-string `calls`,
`elapsed_ns` and event-specific `units` in the Node consumer; use a lossless
parser for raw Rust JSON integers above 2^53. The phase metrics projector can
retain a valid partial profile without inventing absent rows. Its allowed-label
set also preserves six existing optional compact capture/COW labels (114 allowed
labels); this compatibility allowance is separate from the 108-row bank. The
exact backing pilot and verifier require all 108 rows; an old 47-row snapshot is incomplete,
rather than evidence of zero causal traffic. Unknown labels are excluded from
the pilot's closed projection, while unknown or duplicate projected labels fail
verification. Explicit new zero rows are valid observations.

| Fixed row group | Meaning and limits |
| --- | --- |
| `filesystem.block_put.{initial,fallback,retry_rewrite,chunker_reprepare}` and each `.success`, `.error`, `.cancelled` | One span per dispatched block put; unsuffixed units are attempted chunk input bytes. Terminal calls partition dispatches. Success units are the known input length on successful return; error/cancelled units are zero. These are neither application payload nor wire/physical storage bytes. A returned block ID does not prove durability. |
| `filesystem.gate_wait.<kind>`, `filesystem.gate_hold.<kind>`, `filesystem.gate_wait.<kind>.cancelled` | Kinds: `read`, `metadata`, `write_prepare`, `write_commit`, `write_fallback`, `whole_file_replay`, `mutation_batch`, `maintenance`. Wait includes acquisition cancellation; hold starts only on acquisition and ends on guard drop; units are zero. The unchanged old aggregate covers its original sites, so its sum need not equal the classified rows. |
| `filesystem.gate_phase.{refresh,recovery,block_rewrite,publication,cas_backoff}` | Constructed only while borrowing an acquired operation gate. Nested phases are inclusive and do not measure work outside the gate. |
| `filesystem.mutation.{enqueue_requests,dequeue_requests,queue_wait_requests,coalescing_yields,attempt_requests}` | Request/yield units are actual considered counts; queue wait runs until dequeue or queue cancellation. Coalescing observes its actual adaptive window/yields. An attempt includes only open-reply candidates considered in that iteration. It is not a CAS dispatch count. |
| `filesystem.mutation.attempt.{success,conflict,no_publication,error,cancelled}` | One terminal outcome per attempt; `no_publication` includes candidates producing no namespace change and does not imply a dispatched CAS. |
| `filesystem.mutation.request.{committed,conflict,cancelled,receiver_closed,error,reply_sent}` | Committed/conflict/cancelled/error are terminal request observations. Delivery rows classify observed sends or closed-receiver skips; dropping a request before either boundary records no delivery. Successful channel send does not prove consumption or application acknowledgement. A receiver lost after known commit retains the committed observation. |

Cancellation records the Rust observer future/guard drop once; it does not
prove rollback or native operation settlement. Use acknowledged workload
operations as the independent denominator. Gate, rewrite, publication and
provider spans overlap and may nest or run concurrently. Their summed elapsed
time is inclusive wall time, not exclusive CPU, throughput attribution or
physical device IOPS. Attempted/completed chunk bytes are separate numerators;
divide by the same phase's verified/acknowledged payload bytes only when that
independent denominator exists.

### Observer gates and tracing cost

The dedicated public-filesystem integration target exercises deterministic
provider counts, pending futures, CAS outcomes and manual-clock controls with
profiling enabled. Its tests are ignored by default: reporting ignored tests
is not execution evidence. Run the entire dedicated target with `--ignored
--test-threads=1 --nocapture` and inspect every executed name and the terminal
result. The separate core allocator gate uses `--ignored --exact
warmed_causal_profile_rows_record_without_added_allocations`; it measures warmed
recorder primitives outside filesystem futures, executor work and snapshots.
The older storage-span allocation gate qualifies the separate storage recorder,
not these new core rows. None of these controls qualifies backend throughput.

The dedicated target executes 12 cases on Linux/macOS and 11 portable cases on
Windows; the additional Unix case uses SQLite compact inodes and verifies
payload after reopening. A separate exact helper lifecycle control checks
once-only terminal partitions and canceled observers.

The observability CI job adds explicit opt-in commands and retains their
complete `filesystem-causal-metrics.log` and
`filesystem-causal-profile-allocations.log`, together with the existing
`storage-diagnostic-allocations.log`, using an always-upload artifact. Helper
controls retain `filesystem-causal-helpers.log`. A separate
trace-enabled smoke runs `causal_profile_slow_span_emits_fixed_stderr` and retains
`filesystem-causal-profile-slow.log`; CI requires its fixed slow-record line.
`pipefail` preserves the test failure through `tee`. Hosted coverage requires the actual
uploaded logs and matching source/build identity, not a workflow definition.

Optional causal slow records reuse `MOUNT_RS_TRACE_STORAGE` and the fixed
`MOUNT_RS_PROFILE_SLOW` marker: a 100 ms threshold and a 16-record process budget
bound emitted records. Fixed labels and numeric counters exclude identifiers,
payloads and error bodies. Tracing still adds observer work; compare tracing-off
and tracing-on evidence separately. Sampling/log bounds do not make the observer
free, and the warmed allocation gate does not include formatting or output.
The retained Ozone floor failures and their source/build uncertainty above
remain unchanged; these rows do not establish their cause or a speedup.
