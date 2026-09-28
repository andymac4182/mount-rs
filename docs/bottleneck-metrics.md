# Finding remote-drive bottlenecks

The [fixed-working-set extent comparison](benchmarks/extent-scaling-20260928/README.md)
separates whole-inode metadata growth from blob input and native SQLite write
amplification. Six balanced WAL/FULL runs keep the accessed 32 blocks fixed while
varying file layout size, retaining full-file stored and fresh-reader checks.

The [persistent SQLite cache fault measurements](benchmarks/sqlite-cache-failure-metrics-20260928/README.md)
join actual backing GETs, SQL statements, pager activity and VFS callbacks across
14 cache phases. They also retain a disk-admission rejection that returned full
bytes while dropping the optional fill. This separates cache admission and peer
faults from backing-store I/O, with fresh payload/EOF and authority checks.

The [SQLite journal comparison](benchmarks/sqlite-journal-diagnostic-20260928/README.md)
adds a test-only owned-file DELETE/WAL selector and verifies actual FULL settings
on all provider connections. It retains database/WAL/SHM size gauges and a
separate terminal close/drop process-I/O interval before fresh verification.
Three repetitions per mode measured median writes of 1,641 versus 10,459 IOPS,
with unchanged chunk/metadata format, 17 statement notifications and two commits
per write. The result identifies local journal/commit cost and retains deferred
work and environment limits.

For WAL, SQLite pager writes count pages submitted to the log; checkpoint
database rewrites are outside that counter. Earlier worker-report wording that
the pager counter excludes WAL was imprecise. Compare pager activity with
separate process/device observations without equating them to physical writes.

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
| Distributed blob cache | Miss admission/singleflight waits, RAM/disk lookups and hit bytes, outbound peer connection lock/establishment | Lookup times include misses; disk timing includes the bounded helper, and connection stages preserve the existing serialization |
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

### Process resource observations

Phase reports now retain the resource counters already sampled at each boundary.
`resource_measurement.schema` is `mount-rs.process-resources.v1`. No new Node
sampler calls are added. The current core, storage and service row inventories are
136, 110 and 22 respectively; the cache and client additions are described below.

| Report field | Meaning |
| --- | --- |
| `resources_work.minor_page_faults` / `major_page_faults` | Process fault-counter deltas between the end of the first snapshot and start of the second. |
| `resources_work.filesystem_input_operations` / `filesystem_output_operations` | Process `getrusage` block-accounting deltas; distinct from syscall counts, device IOPS and datastore daemon I/O. |
| `resources_observer` | The same four counters during the two boundary snapshots, kept separately from workload observations. |
| `memory_start_bytes` / `memory_end_bytes` | Fixed RSS, heap and external-memory endpoint gauges. |
| `memory_delta_bytes` | Signed end-minus-start gauges; a decrease is valid. These do not measure allocation churn or a phase peak. |
| `lifetime_peak_rss_bytes` | Before/after process lifetime high-water marks, converting Node's KiB values to exact decimal bytes. These are never subtracted as a phase peak. |

The added cumulative rows require matching Linux or macOS endpoint platforms.
Windows libuv fault/I/O fields have different support or meanings and remain
unavailable for this POSIX measurement. Missing, negative, fractional, unsafe
or reset counters also remain unavailable per row; observed zero is retained.
The public projector accepts exact measurement metadata and fixed fields only,
checks signed memory changes against their endpoints, and retains old CPU and
end-memory evidence when historical reports lack the additions. These units
follow [Node's process resource API](https://nodejs.org/docs/latest-v24.x/api/process.html#processresourceusage)
and its [bundled Windows libuv implementation](https://github.com/nodejs/node/blob/v24.18.0/deps/uv/src/win/util.c).

The QUIC saturation runner now enables its existing `os_io` observer at the two
workload boundaries when `MOUNT_RS_PROFILE_IO=1`. Its periodic process sampler
continues to leave disk observations disabled. The observer retains own-process
disk accounting, process identity, elapsed interval and observation cost. Device
operation/byte counters require one explicit `MOUNT_RS_PROFILE_BLOCK_DEVICE`
(Linux) or `MOUNT_RS_PROFILE_IOREGISTRY_ENTRY_ID` (macOS) selection; an unset
selector stays `unselected`. Device totals include other processes and kernel
writeback. They do not establish which datastore generated the activity.

The controls cover workload/observer separation, exact integer conversion,
unavailable/reset cases, historical absence, and private-field exclusion. The
real Mac gate observes its own PID and start token with no selected device.
CI runs the Node controls on its existing platforms and the exact OS gate on
Unix, retaining its output even on failure and rejecting a zero-case pass.

Client QUIC setup and directory/discovery lookup now have the fixed rows below.
Incoming peer establishment remains a timing gap.
Client send/receive and peer quota/open/send/receive/GET stages are described
below. Cache lookup histograms include misses; isolated warm phases are needed
to qualify RAM or disk hit latency. Peer send backpressure/error and stream-open
error remain unqualified by the current peer fixture.

The [retained resource report](benchmarks/process-resource-metrics-20260927/report.json)
contains two serial 400-lifecycle compact SQLite runs: 800 verified full reads
and 2,400 acknowledged write/read/delete operations altogether. The local
public NAPI workload measured 1,914.46 operations/sec with profiling and
1,947.38 with profiling disabled, both meeting the unchanged 1,000-operation
floor. Two arms do not isolate observer overhead or establish a speedup.
The disabled arm has no resource/native phase snapshot and retains that absence.

The profiled arm recorded 22,525 SQLite statement calls, 2,112 pager writes,
1,072 minor and two major faults, 268,475/332,648 microseconds of user/system
CPU, and an RSS endpoint increase of 15,794,176 bytes. Authority-path checks
took 90.16 ms across 3,670 calls; Full scans took 77.79 ms across 83,387 guards,
including their nested 55.66 ms decode work. These inclusive spans locate
repeated metadata work. CPU, memory, pager and statement observations have
separate scopes; the process block-accounting deltas of zero do not establish
zero disk activity. No device IOPS, total allocations, remote transport or
full production-capacity claim follows from these local runs.

## Compact metadata work

The compact path has separate observations for local work before provider I/O:

| Fixed row | Timed work / units |
| --- | --- |
| `filesystem.snapshot_nodes` | A filesystem Full snapshot, including compact namespace materialization and pending atime folding; attempted input nodes. |
| `compact.namespace.materialize_nodes` | Guard validation, namespace construction and graph validation inside `CompactSnapshot::namespace`; attempted input guards. |
| `filesystem.mutation.candidate_clone_nodes` | The actual namespace copy for each considered write or unlink in a batch; input nodes copied. |
| `compact.structure.delta_capture_nodes` | Structural delta validation and construction; attempted candidate nodes. |
| `compact.structure.expected_guard_nodes` | Successful delta captures and their expected physical guard counts; count only, with no elapsed time. |

The four new rows append to the unchanged 114-row core prefix, producing 118
core rows and leaving the 85 storage rows unchanged. Node units use existing
collection lengths; observing them adds no graph traversal or namespace copy.
Failed validation still contributes attempted materialization/capture work.
Expected guard units are recorded only after capture succeeds.

These timings overlap: delta capture includes a call to namespace
materialization, and a filesystem snapshot includes materialization too.
Compare their calls, units and inclusive latency with provider spans. Do not
sum them as exclusive CPU time. Large node counts per acknowledged operation
identify repeated local work; they are not allocation counts or device IOPS.
Historical measurements lack these new rows. The general benchmark projector
retains that absence, while exact current pilot qualification requires all
118 rows. The six optional `compact.capture.*` and `compact.create.*` projector
labels have no Rust producers and do not supply these observations.

The [compact create preparation comparison](benchmarks/compact-create-preparation-20260927/README.md)
uses these rows to isolate preparation from publication. With 128 siblings,
guarded missing-path preparation removes its 129-row Full scan and namespace
snapshot while increasing selected reads from one to three. Total guard bytes
decoded through acknowledgment fall from 284,682 to 200,582; authority checks
and read transaction begins each increase by one. Full publication scans remain
unchanged. This local controlled result identifies less scan/decode work, with
explicit race, corruption and unreferenced-block controls; it does not establish
fewer SQL statements, physical IOPS or production throughput.

All timed rows use the existing opt-in recorder and fixed slow-log format
`MOUNT_RS_PROFILE_SLOW event=... elapsed_us=... units=...`. With storage tracing
enabled, operations taking at least 100 ms can emit at most 16 such records per
process. Paths, storage keys and payloads are excluded. Keep tracing disabled
for throughput and warmed recorder allocation controls.

### Filesystem refresh classification qualification

The [retained causal expectation report](benchmarks/filesystem-causal-expectation-20260928/report.json)
reproduces the exact profiled filesystem CI command locally: eleven cases passed,
and the SQLite compact lifecycle case failed because create observed one
`filesystem.refresh.path_structure` call while its assertion expected zero.
Guarded missing-path preparation now resolves its parent through `inode_path_view`.
The corrected uncontended-create expectation is `[1, 1, 1, 1, 0, 0]` in the order
replace probe, create capture, batch capture, path structure, read before, read
after. Other phase expectations remain unchanged. Six fixed numeric phase lines
are emitted only after full payload, EOF and fresh SQLite reopen checks.

Final local qualification executes all twelve named causal cases, the real
client stream metric case and five compact-create race cases: eighteen Rust
test executions, five owned gates and sixty-six modeled parent controls, with
strict chunked/SQLite all-target Clippy and formatting. All 505 source pins
agree across the gates and current source. The owned CI parent now warms and
executes this complete suite, rejecting missing, failed or zero-case results.
This changes the test expectation and its evidence, with no production or
metric-registry change. Hosted raw logs were not inspected; the local reproduction
does not establish a unique cause for the separate hosted failures. Exact new
source still needs hosted qualification.

## Locate the bottleneck in one measured phase

### QUIC setup and cache discovery

| Row | Scope | Terminal meaning |
| --- | --- | --- |
| `client.quic.connection_setup` | TLS/config and endpoint construction through actual QUIC connection, failure classification and negotiated ALPN validation. | Success returns a transport; all setup errors, including its internal deadline, are errors. Caller drop is cancellation. Credentials, hello authentication and WebSocket fallback are excluded. |
| `blob_cache.discovery.locate` | Exactly the discovery await inside the existing peer deadline, before deduplication, filtering and hedged peer requests. | Empty and fallback lists are successful lookup results; trait errors are errors. The outer peer deadline and caller drop cancel this span. Success does not prove directory health. |

These append at storage ordinals 108 and 109, preserving all previous ordinals.
The bank has 110 rows and 23 units families; the core Event bank stays at 136.
Both stages expose calls, success/error/cancellation, in-flight gauges, inclusive
wall time and 32 latency buckets. Their bytes and SQL returned rows are unavailable.
Their timings overlap other work and cannot be added as exclusive CPU time.
Discovery has no backing-read or peer-query units; those are separate counters.

Use `MOUNT_RS_PROFILE_IO=1` before constructing the client or cache. Read
`mount_rs_core::diagnostics::storage::snapshot()` in that same process. Set
`MOUNT_RS_TRACE_STORAGE=1` for fixed-label slow records at 100 ms or longer,
sharing the existing limit of 16 records per process. Keep tracing disabled for
throughput/allocation measurements. Logs exclude credentials, storage keys,
paths, peer IDs and payloads. Native addon declarations include these rows,
but its audited source coverage remains unchanged at 78/85: declared zero rows
do not establish observed client setup or discovery. A server CLI shutdown
record cannot see a separate client's counters.

The isolated qualification commands are `scripts/test-remote-failures.py
clientsetupmetrics` and `scripts/test-remote-failures.py discoverymetrics`.
Both require an explicit checkout-isolated `CARGO_TARGET_DIR` and use the fixed
owned parent. The first uses real local QUIC; the second exercises the public
cache path with controlled discovery and peer adapters. These are local observer
controls, not production-capacity or Redis/network-directory benchmarks.

### Client QUIC stream acquisition

`client.quic.open_bi` appends at storage ordinal 91. The existing 91-row prefix
is unchanged. Enable `MOUNT_RS_PROFILE_IO=1` before constructing the client and
read `mount_rs_core::diagnostics::storage::snapshot()` in that same process.
The shared helper observes the existing `open_bi` await in control exchanges,
including authentication and renewal, binary reads and binary writes.

Calls count stream acquisition invocations. Success means that local send and
receive stream handles were acquired. It does not establish that a request was
sent or acknowledged. Error preserves the existing transport-error mapping;
dropping a pending acquisition records cancellation, including when the outer
request deadline cancels it. Inclusive wall time, outcomes, in-flight gauges
and the existing 32 latency buckets help identify stream-credit waits. Bytes
and returned-row counts are unavailable at this boundary. Acquisition ends
before the existing transaction guard begins. Dropping a private transport
future before acquisition retains its connection, while the public client still
closes the connection on its request timeout or transport failure. Closure after
an uncertain acquired transaction is preserved. Wire format, authorization and
deadlines are unchanged.

The [retained client stream report](benchmarks/client-quic-stream-metrics-20260928/report.json)
contains an actual missing-metric RED followed by five measured real QUIC phases:
three pending cancellations, three successful reused acquisitions, a successful
acquisition followed by a malformed response, three closed-connection errors,
and one acquired control request cancelled while its response is held. Full
65,543-byte read/write equality, fields and EOF are checked. The fixture uses
private transports and generated TLS certificates; it bypasses public OIDC and
ALPN negotiation. Existing signed remote and configured CLI gates separately
cover those public paths and negotiated WebSocket/lost-reply behavior.

All four fixture endpoints are explicitly closed and bounded `wait_idle` calls
observe idle connections before metric assertions. The fixed legacy marker
`endpoints_drained=true` describes that fixture observation; it does not prove
application drain, joined drivers or released UDP sockets.

Final local qualification has 13 owned gates, 120 Rust test executions,
181 pure Node tests and 65 modeled parent controls, including strict Clippy and
formatting. Each bank retains unchanged source inputs: 505 Rust pins and 93
Node pins, with a 530-file union. The earlier failed consumer gate is retained
separately; two stale row-count assertions were corrected in its JavaScript test.
The warmed public recorder covers 14 selected storage rows with zero allocation
calls. In that stream-only slice, boxed trait futures were 608/464/464 bytes
for exchange/read/write in both debug arm64 Rust 1.95 arms. This is an object
size observation; whole-client allocation counts remain unmeasured.

The Node and native exporters declare the row and its `client_quic` units family.
Historical 91-row records retain null client measurements, and strict current
pilots reject a missing row. NAPI still audits 78 default or 85 FoundationDB
storage producers; its client family remains unavailable because the addon does
not call this remote client. Server CLI shutdown exports are process-local and
cannot observe counters in a separate client process. A dedicated shutdown
export for client CLI mounts remains open; library and owned load-runner process
snapshots can observe this row.

### Client and peer QUIC request stages

The [retained transport report](benchmarks/transport-stage-metrics-20260928/report.json)
qualifies eight appended storage rows, preserving the original 92-row prefix.
That slice produced 100 rows and 20 distinct units families. The current bank,
including the WebSocket stages below, has 110 rows and 23 families.

| Fixed row | Timed boundary | Successful bytes |
| --- | --- | --- |
| `client.quic.request_send` | Existing control/binary encoding, write and FIN submission | Unavailable |
| `client.quic.response_receive` | Existing frame/result decode and EOF validation | Unavailable |
| `blob_cache.peer.request_byte_admission_wait` | Existing outbound transfer-byte permit await | Unavailable |
| `blob_cache.peer.open_bi` | Existing peer stream acquisition await | Unavailable |
| `blob_cache.peer.request_send` | Existing chunk write and FIN submission | Plaintext header plus body submitted |
| `blob_cache.peer.response_receive` | Status and bounded body read, EOF/status validation and existing owner wrap | Plaintext status byte plus body |
| `blob_cache.peer.get` | Inclusive `get` or `get_shared`, including existing return conversion | Logical returned payload length |
| `blob_cache.peer.get_miss` | Successful `None` classification marker | Unavailable |

Send success means local submission to Quinn. It does not establish peer
acknowledgment or backing durability. Receive success means the existing
transport validation completed. A valid remote application error can therefore
be a successful receive. GET includes the existing Vec conversion only for
`get`; `get_shared` retains its existing Bytes result. Empty `Some` is a hit;
only `None` emits the miss marker. Miss duration measures marker work, not a
network request. Nested GET and transport durations overlap.

Caller drop records cancellation. The existing peer deadline drops an active
inner stage as cancelled and returns a GET error. These counters preserve the
existing error mapping, security checks, deadlines, connection roles and
uncertain acquired client transaction closure. They add no framing pass,
spawned task or explicit Box. RPC allocation counts remain unmeasured.

Actual missing-row RED fixtures precede the producers. Final qualification runs
20 owned gates: 172 Rust test executions, 204 Node tests and 77 modeled parent
controls, including strict Clippy and formatting. Real client controls include
stream-credit waits, held response cancellation, delayed/blocked sends, stopped
streams, malformed data and extra response bytes after a valid frame. Real peer
controls include byte quota/deadline, stream credits, response pending, full and
empty payloads, misses, `get_shared`, reuse, mTLS and partition rejection.
Peer send success is qualified; peer send pending/error and stream-open error
remain open. Client send controls do not qualify peer send backpressure.

With profiling and tracing enabled, the real peer quota timeout emits
`request_byte_admission_wait` cancelled and inclusive `get` error records at
about two seconds. The shared 100 ms threshold and 16-record limit remain.
The validator accepts the six new fixed peer labels and rejects private or
unknown labels, invalid durations and over-budget records. The actual file reader
preserves raw bytes; recognized records must be complete LF frames. Real-file
controls reject truncation, CR/CRLF, alternate separators and invalid UTF-8.
Slow logging adds
observer work; keep it off for allocation and throughput measurements.

The warmed recorder measures zero alloc/alloc_zeroed/realloc calls across
22 selected rows and 66 success/error/drop lifecycles. This excludes setup,
snapshots, output and complete RPCs. Existing boxed client futures grow from
608/464/464 to 648/544/544 bytes for exchange/read/write in the debug arm64
Rust 1.95 build: increases of 40/80/80 bytes. No throughput improvement or
whole-RPC allocation claim follows from these instrumentation tests.

Both real fixtures complete cleanup checks before metric assertions. Client
`wait_idle` follows explicit closure of all 16 endpoints and observes idle
connections; it does not prove driver joins or UDP release. The peer fixture
observes weak cache owners gone and both actual UDP addresses rebound.
The owned parent independently observes reap, absent process group and EOF
before removing its private fixtures.

JS and NAPI exports declare stage-specific bytes. The addon still audits 78
storage producers by default or 85 with FoundationDB; its transport families
remain unavailable. Literal historical 92-row records retain missing rows and
histograms as null/unavailable. Strict current validators reject missing rows
and the historical payload-only descriptor on a current 110-row inventory.
Server CLI shutdown banks remain process-local; a dedicated client CLI shutdown
export and incoming peer setup timings remain open.

### WebSocket client and service observations

Enable exactly `MOUNT_RS_PROFILE_IO=1` before the first client recorder use and
read `mount_rs_core::diagnostics::storage::snapshot()` in that client process.
The eight rows append at storage indexes 100 through 107, preserving the original
100-row prefix:

| Fixed row | Timed boundary |
| --- | --- |
| `client.websocket.tcp_connect` | Existing TCP connect await |
| `client.websocket.tls_handshake` | Existing TLS connect await |
| `client.websocket.upgrade` | HTTP upgrade and subprotocol validation |
| `client.websocket.socket_lock_wait` | Existing serialized socket acquisition and closed/shutdown checks |
| `client.websocket.request_encode` | Existing binary request encoding |
| `client.websocket.request_send` | Header/body/terminator sink submission |
| `client.websocket.response_receive` | Existing envelope reception and protocol validation |
| `client.websocket.response_decode` | Result decoding, output copy and trailing-data validation |

Calls are stage invocations with success, error and cancellation outcomes,
in-flight gauges and 32 latency buckets. Byte and returned-row observations are
zero because these quantities are unavailable at this seam. Durations are
inclusive overlapping wall time: they do not isolate CPU or network time and
must not be summed as exclusive work. Local send success does not establish peer
acknowledgment; a valid remote error can be successfully received and decoded.
Existing deadlines, socket serialization and uncertain-write handling remain.

For service observations, compile `mount-rs-cli` with `--features io-profiling`
and run `serve-remote` with exactly `MOUNT_RS_PROFILE_IO=1` and a configured
WebSocket listener. Embedders instead explicitly call
`WebSocketServer::bind_with_diagnostics(..., true)` in an `io-profiling` service
build and retain `server.diagnostics()`. Ordinary constructors remain disabled,
including when the environment flag is set. The application-only snapshot schema
is `mount-rs.service-websocket.v1`; its shared 22-row service bank includes
`handshake.websocket_upgrade`, scoped hello/renewal authentication, request,
admission, dispatch, encode, submit and actual session cleanup. Authenticated idle
sockets have no open request span. Preserve the activity, saturation and quiescence envelope;
it does not measure passive traffic or prove application drain.

The WebSocket snapshot has no QUIC transport subtree. TCP/TLS wire bytes,
WebSocket frame counts, peer acknowledgment, process CPU and physical device
IOPS are explicitly unavailable. NAPI declares the eight client rows and the
`client_websocket` family but does not invoke that client: addon source coverage
remains 78 rows by default or 85 with FoundationDB. Historical 100-row receipts
remain incomplete under strict current validation and are never padded.

Optional slow logs require `MOUNT_RS_TRACE_STORAGE=1` for client stages or
`MOUNT_RS_TRACE_SERVICE=1` for the enabled service observer. The existing 100 ms
threshold permits at most 16 storage records per process or 16 service records
per observer. Formats are `MOUNT_RS_STORAGE_SLOW operation=... outcome=... elapsed_us=...`
and `MOUNT_RS_SERVICE_SLOW operation=... outcome=... elapsed_us=...`. Labels are
fixed; tokens, issuers, audiences, partitions, drives, paths, addresses, payloads
and raw errors are excluded. Leave tracing off for throughput controls.

After actual service, runtime, storage-context and cache cleanup, the CLI emits
a bounded `transport=websocket` shutdown record alongside its QUIC record, as
described in the [public CLI export](#public-cli-diagnostic-export). Server
records cannot observe a separate client process. This section describes the
source contract; it does not report runtime qualification.

### Distributed cache lookup and peer connection stages

The [public CLI diagnostic export](#public-cli-diagnostic-export) describes how
to build the profiled server and capture its bounded shutdown record.

The cache slice appends six storage rows at indexes 85 through 90 and two core
hit counters at 134 and 135. The existing 85 storage and 134 core rows retain
their offsets. This cache slice produced 91 storage rows; client stream acquisition
brought that slice to 92 storage and 136 core rows; the request-stage slice
above produced 100 storage rows; the WebSocket stages produced 108 rows; setup/discovery bring the current bank to
110.

| Fixed storage row | Measured boundary | Successful bytes |
| --- | --- | --- |
| `blob_cache.miss.admission_wait` | Existing distributed miss semaphore await only | 0 |
| `blob_cache.miss.singleflight_wait` | Existing per-block flight mutex await only | 0 |
| `blob_cache.ram.lookup` | Existing synchronous RAM probes, including misses and owner/follower rechecks | Returned payload length on Some, otherwise 0 |
| `blob_cache.disk.lookup` | Existing bounded disk helper: permit/worker queueing, read and verification | Returned payload length on Some, otherwise 0 |
| `blob_cache.peer.connection_lock_wait` | Existing per-peer connection slot mutex await only | 0 |
| `blob_cache.peer.connection_establish` | Existing outbound connect, TLS establishment, authenticated peer-ID check and slot installation | 0 |

Wait rows finish immediately on acquisition. They exclude flight-map
bookkeeping and the lifetime of acquired guards. A pending caller drop records
cancellation. RAM and successful disk misses are successful zero-byte lookups;
internal cache/join/read failures already collapsed into None remain successful
disk misses. The disk helper's outer deadline is an explicit error, while
caller abandonment is cancelled. Timed-out or dropped blocking disk work can
continue after the observer settles; helper latency does not establish worker
completion, device failure, physical IOPS or durability.

Known-peer and partition checks retain their current positions. A live reused
connection has a lock row and no establishment row. Establishment covers
explicit connect/handshake/identity errors; the existing outer request timeout
or caller drop records cancelled establishment. The connection mutex remains
held through establishment. These client boundaries exclude stream creation,
send/receive and remote server execution. Their bytes and SQL row observations
are zero.

`blob_cache.ram.hit_bytes` and `blob_cache.disk.hit_bytes` count only Some
results. Calls count hits and units count returned payload bytes; an empty hit
is one call with zero units. They do not measure allocation churn or QUIC wire
bytes. Mixed lookup histograms include misses, so use isolated warm RAM and
disk phases for hit latency.

The existing storage recorder supplies terminal outcomes, in-flight gauges,
inclusive nanoseconds and 32 latency buckets. Its existing 100 ms slow threshold
and limit of 16 records per process cover these rows automatically. With tracing
enabled, completion can write diagnostic stderr while the existing connection guard is
held; use trace-off controls for clean latency/allocation baselines. No peer,
block, key, path, token or raw error enters a metric label or slow record.

The ignored exact `cachemetrics` and `peermetrics` parent gates isolate processes
with profiling enabled and storage/request traces disabled. They qualify full binary
bytes, counted backing GETs and actual peer attempts before checking stage
deltas. Controls cover 100 cold coalesced reads, RAM/disk and empty hits, admission,
singleflight and disk cancellation, the existing disk deadline's healthy peer
fallback, real cold/reused mTLS, held connection mutex cancellation, an owned
blackhole PUT/GET overlap, and rejected identity/no admission. Fixed-label
`MOUNT_RS_CACHE_STAGE` JSON preserves actual phase deltas and pending gauges
before assertions; unavailable labels remain unavailable.

`storagealloc` exercises all six new operations through actual warmed public
success/error/drop primitives; `corealloc` adds both hit events to the warmed
Span/add/drop window. The controls count alloc, alloc_zeroed and realloc calls,
excluding initialization, snapshots, diagnostic output, returned payloads and
async work. Separate `cacheprofileoff`/`cacheprofileon` processes preserve the
existing RAM-hit ready-future benchmark. Finished wait/RAM spans leave their
lexical blocks before later work or the ready future; cold async future objects
can still grow to retain pending observers. These controls do not imply that
whole cache/transport futures or file operations allocate zero.

Historical observations with 85 storage and 134 core rows lack the cache additions.
Projectors retain those missing rows as unavailable/null; exact current
qualification requires the new bank. Native-addon storage declarations contain
the cache family, but that addon has no blob-cache dependency: its audited
instrumented-operation counts remain 78 feature-off and 85 with FoundationDB.
Cache rows therefore remain unavailable in native-addon coverage.

### Hosted peer reconnect diagnostics

The owned failure annotation for `peerreconnect` now adds
`last_sampled_progress_marker` and `progress_sample_window`. These contain only
authored phase labels or fixed `invalid`/`unobserved` states. The observer reads
the existing first/last 2,048-byte stdout windows, keeps their cut boundaries
separate, and accepts complete newline-delimited records. A cut record or an
unknown tail-line start cannot supply a marker. Stderr has separate ordering
and does not supply peer progress.

The marker identifies the last retained observation, rather than the last
executed or completed phase. Middle and boundary records can be unavailable.
A `before_*` marker records intent; `after_public_get` establishes byte equality,
and the subsequent amplification checks establish backing/cache outcomes.
Incoming-owner observation does not establish TLS readiness. Cleanup success
precedes the final amplification and stage assertions. The test now emits fixed
markers around those assertions so their observations can appear in the tail.

The [diagnostic report](benchmarks/peer-reconnect-progress-20260927/report.json)
joins 48 Rust test executions, 63 modeled parent controls, formatting and strict
cache/remote Clippy. The real successful reconnect supplies
`after_amplification_assertions` from the last window and emits no fatal
diagnostic. Failure classification, process ownership, deadlines and output
caps retain their existing contracts. This change gathers evidence for the
hosted failure; it does not establish its cause or a throughput improvement.

### Stopped-peer UDP fixture release

Real no-connection probes reproduced the restart fixture's immediate-bind
assumption failing for both a bare Quinn endpoint and the public peer transport.
Both later rebound sockets received the complete binary datagram from the
expected sender. The restart fixture now retries only `AddrInUse`, yields for
one millisecond, and retains the first successfully bound socket within one
fixed three-second deadline. Other bind errors return immediately. A real
held socket exercises exhaustion of that deadline.

The [UDP release report](benchmarks/stopped-peer-udp-release-20260927/report.json)
joins the intended baseline failure with 49 final Rust test executions, 64
modeled parent controls, formatting and strict cache/remote Clippy. The
authenticated reconnect still checks complete bytes, peer reuse, partition
isolation and zero backing reads. Fixture directories remain until observed
cache-owner release; earlier failure or cancellation leaves them for the owned
parent's cleanup. These observations establish fixture address reuse. They do
not establish a synchronous production shutdown contract, identify the earlier
hosted failure's cause or measure throughput.

### Authority checks and refresh reasons

The authority/refresh slice appended sixteen rows to the 118-row prefix, for
134 core rows, while storage then remained 85 rows. The cache slice appends to
those unchanged prefixes, producing 136 core and 91 storage rows. Client stream
acquisition subsequently appended storage row 91, producing 92 rows. The
request-stage slice produced 100 storage rows; WebSocket stages subsequently
produced 108 rows; setup/discovery bring the current bank to 110.
The authority/refresh observations split the
repeated compact metadata work seen in the latest lifecycle measurement:

| Fixed row | Scope / units |
| --- | --- |
| `sqlite.compact.authority_query` | Authority SELECT and extraction; no units. |
| `sqlite.compact.authority_path` | Existing physical-file, pathname and local-filesystem qualification; no units. |
| `sqlite.compact.anchor_query_bytes` | Anchor SELECT and extraction; successfully returned JSON bytes. |
| `sqlite.compact.anchor_decode_bytes` | Anchor decoding and generation/backing comparison; attempted JSON bytes. |
| `sqlite.compact.guard_selected_rows` / `guard_full_rows` | Entire selected/full cursor scan, decoding and map insertion; rows yielded before decoding, including malformed rows. |
| `sqlite.compact.guard_selected_decode_bytes` / `guard_full_decode_bytes` | Existing guard body decode/validation after string extraction; attempted JSON bytes. |
| `sqlite.compact.read_lock_wait` / `read_begin` | Read-side connection lock acquisition / deferred transaction construction; no units. |
| `filesystem.refresh.replace_probe` | Existing-file structure probe before whole-file replacement. |
| `filesystem.refresh.create_capture` | Whole-file preparation capture, including an existing-file fallback. |
| `filesystem.refresh.batch_capture` | The capture for each actual mutation batch attempt. |
| `filesystem.refresh.path_structure` | Structure refresh before each path-resolution attempt. |
| `filesystem.refresh.read_before` / `read_after` | Handle refresh immediately before / after block I/O, including EOF reads. |

Filesystem reason rows have no units and end immediately after their existing
refresh await. The existing `filesystem.inode_path_guard` counts traversed
guards. These rows describe particular callers; they are not an exhaustive
partition of every metadata operation. Guard scans include their decode rows,
and filesystem refreshes include provider work. Keep these inclusive times
separate instead of summing them as exclusive CPU time. Started scopes record
attempted work even when validation fails or an await is cancelled.

The same opt-in recorder and bounded fixed-field slow logs cover these rows.
No storage key, path, token, file contents or raw error is included. Historical
118-row measurements lack these stages and cannot satisfy the current exact
pilot inventory; the general projector preserves their absence.

The isolated CI control runs real SQLite selected and Full reads, checks the
persisted bodies, and checks fail-closed malformed-anchor and malformed-body
attempts. The public filesystem control verifies complete bytes through fresh
SQLite connections and counts both refreshes on a data read and on EOF. The
warmed allocation control covers actual recorder primitives; it excludes
initialization, snapshots, provider allocations and diagnostic output.

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

This section records the earlier snapshot-revision experiment and its controls
at that measurement head. The current fresh-create behavior and subsequent
measurements are recorded in the final section below.

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

The library also provides an additive `mount-rs.startup.v2` contract for lazy
construction through `Startup::new_lazy`. This prepares an observer API; the CLI
continues to use the eager v1 producer until its runtime-pool integration is
wired. V1 retains its original 21 fields. V2 adds `construction_mode="lazy"`,
`max_active_drives` and `construction_plans`. Capacity is an explicitly present
null before `plan_lazy` sets positive capacity and immutable catalog totals;
the plan can be set only once. `construction_plan` counts completed immutable
plans, and registration counts factories registered from those plans. A complete
Ready record requires planned, constructed and registered drive counts to match,
with zero startup opens. Provider activation and eviction timings belong in the
dedicated runtime observer bank after startup. Neither a v2 record nor a capacity
field configures or activates a pool. The Rust parser and public log filter accept
both versions with their exact field sets and reject private or unknown fields.

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

### Public CLI diagnostic export

The ordinary server constructors keep the service observers disabled. Service
embedders can explicitly use `RemoteServer::bind_with_diagnostics(..., true)` or
`WebSocketServer::bind_with_diagnostics(..., true)` in an `io-profiling` build,
retain `server.diagnostics()`, and call its `snapshot()`
at controlled boundaries. This is a local API, with no diagnostic network
endpoint.

The public `serve-remote` CLI selects its QUIC and configured WebSocket observers
only when its default-off `io-profiling` feature is compiled and `MOUNT_RS_PROFILE_IO` is
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
stderr line per enabled service observer, prefixed with
`service_diagnostics `. Its envelope schema is
`mount-rs.cli-service-diagnostics.v2`, with the server PID and
`capture_context=shutdown`. The closed transport labels select the nested
snapshot: `transport=quic` uses the unchanged `mount-rs.service-quic.v1`, and
`transport=websocket` uses `mount-rs.service-websocket.v1`. Added
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

Each service snapshot is cumulative over its listener lifetime, with no phase
export or diagnostic endpoint. Preserve the QUIC snapshot's completeness,
saturation, activity, quiescence and registry-gap fields when interpreting it; endpoint
closure does not establish application drain. Registry memory and capture cost
are bounded by the configured `max_connections`. Profiling and any slow-log
overhead remain part of the instrumented process measurements. The WebSocket
snapshot retains application observations and explicitly unavailable transport
quantities, as described [above](#websocket-client-and-service-observations).

The process section exports the existing typed storage and core profile
snapshots only when their recorders are enabled. Disabled banks have
`available=false`, `reason=observer_disabled` and no snapshot; they are not
reported as measured zero. Its scope is `process_cumulative`, with
`capture_atomic=false`, `application_drain_proven=false` and inclusive,
overlapping wall durations. Global banks survive provider retirement without
retaining stores or connections. They do not attribute totals to a particular
drive or provider instance, and zero in-flight gauges do not prove drain.

Storage retains all 110 ordered rows, outcomes, known successful bytes by
operation (payload or peer plaintext envelope as documented above),
returned rows and row-observation availability, global/per-row in-flight
gauges, 32 latency buckets and forwarding-box provenance. Coverage labels are
derived from the producer registry: 47 `sdk.*` erased-method rows, 18 `tidb.*`
source-instrumented adapter rows, 12 NAPI `metadata.*`/`blocks.*` forwarding
rows that this CLI does not use, and one `pglite.client_lock_wait` row selected
only by that provider. The seven FoundationDB, thirteen cache, four client QUIC
and eight client WebSocket rows are also declared in the bank. The additional
`client_websocket` coverage family is marked
`declared_not_cli_server_source_instrumented`; these declarations do not
establish their use or instrumentation in every process. Zero TiDB rows do not prove TiDB use or complete
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

Enabled QUIC and WebSocket observers retain seven fixed rows for
`CatalogAuthenticator`, inside the inclusive `auth.authenticate` span:

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
WebSocket hello and renewal also scope these stages: hello authentication retains
its existing 30-second timeout, and renewal remains inside the existing dispatch timeout.

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

The opt-in core profile contract retains its original 47 names and appends 67
fixed causal rows, for 114 total. The latest six rows preserve the preceding
108-row prefix. This is separate from the unchanged 85-row
storage operation bank. Each core row retains decimal-string `calls`,
`elapsed_ns` and event-specific `units` in the Node consumer; use a lossless
parser for raw Rust JSON integers above 2^53. The phase metrics projector can
retain a valid partial profile without inventing absent rows. Its allowed-label
set also preserves six existing optional compact capture/COW labels (120 allowed
labels); this compatibility allowance is separate from the 114-row bank. The
exact backing pilot and verifier require all 114 rows; an old 47- or 108-row snapshot is incomplete,
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
| `filesystem.mutation.create_guard.{evaluated,passed,conflict,revision_mismatch,allocation_mismatch,path_present}` | Count one completed fresh-create guard evaluation, its pass/conflict, and every true overlapping predicate. Calls and units each increment by one; elapsed time is zero. These rows cover the shared legacy/compact whole-file helper after successful path resolution. |

At a quiescent snapshot, create-guard `evaluated = passed + conflict`; each
predicate count is at most `conflict`, but their sum may exceed it. The rows
observe the post-remap mutation against the batch's accumulated candidate:
same-revision fresh creates already use its current `next_inode`, so an
allocation mismatch does not report the originally captured inode. A confirmed
CAS loss can cause another evaluation; these are distinct from final request
outcomes and provider attempts.

Path resolution errors precede evaluation. `path_present` describes the resolved
final node with symlinks followed, rather than lexical directory-entry existence.
Passing this guard still permits later inode-overflow, namespace, timestamp or
publication failures; it does not establish a committed write. The private
production control exercises all eight predicate combinations, unresolved-parent
exclusion, and passed-guard inode overflow; the public controls additionally
exercise the actual batch pipeline and full readback after reopening.

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
Unix guard predicate control retains `filesystem-create-guard-metrics.log`; the
public revision controls retain `compact-snapshot-revision.log`. A separate
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

## Fresh-create guard attribution: local SQLite, 2026-09-27

The six guard counters were measured with a fresh native addon, SHA-256
`ab744ea8ae4b4ddcc1743db832c539539ec3cdee357dff9f04f451b5b9284fb9`,
built from 435 frozen source inputs. Each runtime also pins its 449 source
inputs and reports that exact staged addon. The unchanged lifecycle control
uses 400 iterations, concurrency 64, 4096-byte payloads, 65536-byte chunks and
a 1000 logical operations/sec floor. Every arm completed 400 full verified
readbacks and acknowledged 1200 write/read/delete operations without timeouts,
late operations or cleanup failures.

| Local native SQLite arm | Logical operations/sec | Original floor |
| --- | ---: | --- |
| Compact, profiling enabled | 1031.70 | Passed |
| Compact, profiling disabled | 1030.96 | Passed |
| Legacy, profiling enabled | 1514.75 | Passed |
| Legacy, profiling disabled | 1369.13 | Passed |

These are single arms. They establish neither a causal speedup nor isolated
observer overhead. The earlier compact/profile 947.80 floor failure remains
retained. Profiling-disabled counters are unavailable, and shutdown diagnostics
remain incomplete. Create, workload and cleanup diagnostic phases are complete
and quiescent in the profiling-enabled arms, with 114 core and 85 storage rows.

| Workload observation | Compact/profile | Legacy/profile |
| --- | ---: | ---: |
| Fresh-create guard evaluations | 400 | 400 |
| Guard passed / conflict | 7 / 393 | 37 / 363 |
| Revision mismatch | 393 | 363 |
| Allocation mismatch, after existing batch remapping | 393 | 283 |
| Resolved path present | 0 | 0 |
| Whole-file replay gate holds | 393 | 0 |
| Metadata snapshot / publication calls | 833 / 420 | 0 / 783 |
| Inode body returns / bytes returned | 245004 / 118759807 | 0 / 0 |
| Whole namespace bytes serialized | 0 | 20661225 |
| Initial block PUT calls / attempted chunk input bytes | 400 / 1638400 | 763 / 3125248 |
| SQLite statements | 28378 | 4235 |
| SQLite pager writes | 4653 | 11262 |

Every compact guard conflict has both the revision and allocation predicates
set; their equal counts each exhaust the 393 conflict observations. The zero
path-present count rules out that predicate for these evaluated creates. In
legacy, 80 conflict evaluations have revision mismatch without allocation
mismatch. These are overlapping guard observations, not evidence of which
writer caused each change. A revision mismatch also prevents the existing
same-revision inode remapping before this guard runs.

Compact request-conflict and replay-hold counts are each 393, and its 40 batch
attempts considered 800 requests: 27 successful publication attempts considered
407, and 13 no-publication attempts considered 393. No batch attempt recorded a
provider CAS conflict. The 833 snapshots comprise 400 preparation captures,
393 replay captures and 40 batch captures; 420 publications comprise 393 replay
publications and 27 batch publications. These relationships agree with the
controlled workload and the reviewed source; counters alone are not a complete
per-request timeline.

The next optimization target is fresh-create rebasing against a validated
current batch candidate, followed by reducing full metadata captures and guard
scans. The compact workload still returns approximately 118.8 MB of inode
metadata for 1.64 MB of acknowledged write payload. Its block PUT input is
already one times that payload. Retain fresh path/parent, allocation, chunker,
authority, flush and uncertain-commit checks when evaluating a rebase change.
The legacy reentry path records its additional block writes under the immediate
`initial` caller reason; zero lower-level `fallback` PUT rows do not establish
that whole-file fallback did no work.

SQL statements and pager writes are backing-store observations with different
units from physical device IOPS. These local native controls do not qualify
mounted clients, transports, distributed caching or the production topology.
The new guard matrix and public controls, causal helpers, warmed allocation
recording and slow-log check passed alongside the focused storage regressions:
245 Rust tests and 119 Node consumer controls, formatting and strict Clippy.
The parallel compact-install test fixture now includes a unique sequence in its
private directory name after an observed clock-name collision; directory
creation remains exclusive. The warmed zero-allocation claim covers recorder
primitives, excluding complete filesystem operations, snapshots and logging.

Retained joined evidence: `/private/tmp/mount-rs-create-guard-actual-summary-20260927-kva9g3x1/summary.json`,
SHA-256 `a6d572aff2921b05ffbd36233ddbb2ba9923059bd9d037de33f0400c9ffd091b`.

## Current create rebase and local-work observations, 2026-09-27

Concurrent fresh creates now resolve their current path and directory authority,
compare the prepared chunker with current defaults, and rebind only their
ephemeral revision/inode before the existing apply guard. Each accumulated batch
candidate and confirmed uncommitted CAS retry is checked again. A changed
chunker takes full replay; an occupied path retains its guard conflict. Current
symlink targets and defaults apply. Durable publication and uncertain-commit
handling remain unchanged. Public SQLite controls verify complete persisted
bytes and EOF through fresh connections for peer allocation, current symlink
targets, occupied paths and rejected current parents.

The fresh native addon is SHA-256
`3689d34d2754bd7afa01cdd29c2dc8ca545024ac5c675fb2d72eaab61b717d8a`,
built from 437 frozen inputs; each native arm pins 451 runtime inputs. The
installed addon and the protected original checkout remain untouched. Workload
dimensions remain 400 lifecycles, concurrency 64, 4096-byte payloads,
65536-byte chunks and the original 1000 logical operations/sec floor.

| Initial local SQLite arm | Logical operations/sec | Original floor |
| --- | ---: | --- |
| Compact, profiling enabled | 1888.08 | Passed |
| Compact, profiling disabled | 1819.69 | Passed |
| Legacy, profiling enabled | 1294.21 | Passed |
| Legacy, profiling disabled | **826.53** | **Failed: `IOPS_TARGET_NOT_MET`** |

The floor failure prompted an additional profiling-disabled layout sequence:
legacy 1287.33, compact 1791.76, compact 1798.09, legacy 1475.18 operations/sec.
All four follow-up arms passed the unchanged floor. Retain the initial failure:
these short measurements expose substantial legacy variability and do not
establish a causal before/after speedup or isolated observer overhead. Layout
selection also changes the benchmark's ownership options: compact uses
concurrent inode updates, while legacy uses its existing exclusive default.
The comparison therefore includes those configured implementation differences.

All eight arms verified 400 complete read payloads through the native read-to-EOF
loop and acknowledged 1200 write/read/delete operations each: 3200 verified
readbacks and 9600 logical operations total. No workload errors, timeouts, late
operations or cleanup failures occurred. The original throughput floor failure
is separate from those successful operations. Creation, workload and cleanup
diagnostics in both profiled arms are complete and quiescent; shutdown
diagnostics remain incomplete after SQLite connections close.

| Profiled compact observation | Calls | Node units | Inclusive elapsed |
| --- | ---: | ---: | ---: |
| Filesystem Full snapshot | 436 | 76059 | 10.54 ms |
| Compact namespace materialization | 944 | 167966 | 24.06 ms |
| Write/unlink candidate copy | 800 | 173016 | 11.04 ms |
| Structural delta capture | 36 | 8324 | 3.63 ms |
| Successful expected physical guard set | 36 | 7924 | Count only |

The compact workload recorded zero fresh-create guard conflicts and zero
whole-file replay holds. Its 36 successful batch attempts considered 800
write/unlink request units and returned 800 committed replies, with no recorded
batch CAS conflict. The 436 backend snapshots comprise 400 preparations and 36
batch captures. Initial block PUT input remains 400 calls / 1638400 bytes.
Full captures and guard work still return 87183 inode bodies / 44693540 known
bytes, alongside 22540 SQLite statements and 2130 pager writes. These counters
locate remaining repeated work, without proving a per-request causal timeline.
Materialization appears inside other measured spans; do not add their durations
as exclusive costs.

Focused verification retained 261 Rust test executions (six helper cases also
execute in the library suite), four default ignored rows, 126 Node consumer
passes, formatting and strict Clippy. The actual warmed recorder allocation
gate covers all 71 causal events with zero added allocations, excluding whole
filesystem operations, initialization, snapshots and logging. The new bounded
Kani harness is wired but its execution remains pending. These local API runs
provide no physical device IOPS, total allocator counts, mounted-client,
cross-host, distributed-cache or production-topology qualification.

Retained native evidence:
`/private/tmp/mount-rs-compact-work-native-actual-20260927-gkiukj4u/summary.json`,
SHA-256 `c1d62ed0f9be73577fe3c34742cf56d37931a102c0b7afc71c1cf4ccf35bc225`.
Focused gate evidence:
`/private/tmp/mount-rs-compact-work-actual-gates-20260927-nbyw3h7i/summary.json`,
SHA-256 `7373a752d0a8cdc11f7f65a1d7a881ed97c36d4cfce74f99b221befe3811755a`.

## Refresh-stage observations, 2026-09-27

The sixteen additional fixed rows were measured with a freshly built native
addon, SHA-256
`4ce499fbb023cc8e482373528ac61073faa7776f807cc8f05e4d06e541bd8d0d`.
Its 437 build inputs match the build's before/after hashes and current source;
each arm pins 451 runtime inputs and that exact addon. The installed addon and
protected original checkout remain untouched.

Two serial compact SQLite lifecycle arms used the same 400 iterations,
concurrency 64, 4096-byte payloads, 65536-byte chunks and 1000 logical
operations/sec floor. Profiling enabled recorded 1992.29 operations/sec;
profiling disabled recorded 1233.89. Both passed the floor and each verified
400 complete read payloads through the native read-to-EOF loop, with 1200
acknowledged operations and no workload error, timeout or cleanup failure.
These single arms have uncontrolled host/cache activity and do not isolate
observer overhead or establish a causal speedup. Earlier failures remain
retained.

| Profiled stage | Calls | Units | Inclusive time |
| --- | ---: | ---: | ---: |
| Authority SELECT/extraction | 3670 | — | 30.59 ms |
| Physical pathname/local-filesystem checks | 3670 | — | 90.27 ms |
| Anchor SELECT/extraction | 3670 | 4006771 bytes | 5.38 ms |
| Anchor decode/authority comparison | 3670 | 4006771 bytes | 9.08 ms |
| Selected guard scan | 3200 | 3200 rows | 13.50 ms |
| Selected body decode, nested in scan | 3200 | 6063550 bytes | 4.81 ms |
| Full guard scan | 470 | 83239 rows | 79.21 ms |
| Full body decode, nested in scan | 83239 | 38272681 bytes | 56.27 ms |
| Read connection lock acquisition | 3635 | — | 0.09 ms |
| Read deferred transaction construction | 3635 | — | 0.93 ms |
| Replace probe refresh | 400 | — | 34.46 ms |
| Whole-file preparation refresh | 400 | — | 179.99 ms |
| Mutation batch capture refresh | 35 | — | 18.45 ms |
| Path structure refresh | 400 | — | 37.44 ms |
| Handle refresh before block I/O | 800 | — | 38.28 ms |
| Handle refresh after block I/O | 800 | — | 37.99 ms |

Counts reconcile at the actual boundaries: 3670 authority checks are 3200
selected loads + 435 snapshots + 35 publications. Read lock/begin observations
cover the 3635 loads/snapshots; 470 Full scans cover snapshots plus publications.
The selected loads comprise 400 create probes, 400 open structure checks,
800 traversed root/file guards and 1600 data/EOF freshness checks. The 435
snapshots comprise 400 preparations and 35 batch captures. Native reads call
the handle once for data and once for EOF, explaining 800 before and 800 after
refreshes for 400 application reads.

The workload returned 86439 inode bodies / 44336231 bytes and executed 22525
SQLite statements across both stores, with 2082 pager page writes. Initial
block PUT input was 400 calls / 1638400 bytes, with zero fresh-create guard
conflicts or whole-file replay holds. Process CPU was 272284 user and 324471
system microseconds; end RSS was 94273536 bytes. Those process observations
include other work and do not assign CPU to these stages.

Repeated physical-path qualification and Full guard scanning are measurable
targets for the next investigation. Preserve authority/fencing, path identity,
coherent capture and uncertain-commit checks when testing a reduction. These
inclusive, partly nested spans cannot be summed as exclusive CPU or end-to-end
latency. SQL calls and pager writes remain distinct from physical device IOPS.
Creation, workload and cleanup diagnostics are complete and quiescent; shutdown
diagnostics remain incomplete after SQLite closes.

The focused gates passed 354 Rust test executions and 143 Node controls,
formatting, strict Clippy, and the actual fixed slow-log check. The warmed
allocation gate covers all 87 appended recorder events with zero added
allocations, excluding initialization, snapshots, provider work, futures and
logging. Independent source review corrected the Unix-only CI condition and
found no remaining source-contract issue. Hosted CI and full production
qualification remain separate gates.

The fixed numerical [report](benchmarks/refresh-stage-metrics-20260927/report.json)
retains the summaries and limitations. Private joined evidence:
`/private/tmp/mount-rs-refresh-stage-actual-20260927-v7387ycj/summary.json`,
SHA-256 `e02926da6ee2a89adbc51059e853c89cdb8d422497612a64f3b8dc39273ba774`.
Focused gates:
`/private/tmp/mount-rs-refresh-metrics-gates-20260927-fwlkkb_w/summary.json`,
SHA-256 `ba46eccc7911cd7ff2c1d727bad83ab388025cf6788c9c5b68fceca0bf3368e0`.

## Saturation storage-bank export and measured SQLite leads

The small `quic_tidb_saturation` runner exports `storage_profile` at its drained
measured boundaries. With `MOUNT_RS_PROFILE_IO=1`, it includes fixed operation
identities, raw before/after snapshots and checked deltas. Nonzero boundary
gauges or unreconciled terminal counters produce incomplete observations;
disabled recording exports null snapshots rather than zero-cost observations.

Coverage labels explicitly identify its direct test-wire caller, absent peer
cache and service observer, and synthetic authentication. It does not exercise
the production client's `client.quic.*` spans. Snapshots and JSON export happen
at stage boundaries and are outside the warmed allocation-free recorder claim.

The [measured SQLite diagnostic](benchmarks/sqlite-causal-diagnostic-20260928/README.md)
separates logical blob payload, provider statements/pager work, process disk
accounting and whole-disk driver counters. It finds 1x blob payload amplification,
full inode extent-list metadata publication, and a source-supported blocking
SQLite worker-ceiling hypothesis. None of those accounting layers establishes
physical SSD IOPS.

## SQLite transaction diagnostics

With `MOUNT_RS_PROFILE_IO=1` before opening provider stores,
`mount_rs_sqlite::sqlite_io_diagnostics` additionally exports for registered
provider `Database` connections (the service catalog and legacy `SqliteStore`
are outside this registry):

- Actual `journal_mode`, `synchronous`, locking mode, busy timeout, fullfsync,
  checkpoint fullfsync, WAL autocheckpoint and cache settings. These are read
  queries at the registry phase boundary, not changes to storage policy.
- Nine fixed SQL categories with SQLite PROFILE notification counts, approximate
  elapsed nanoseconds, maxima, and 32 logarithmic microsecond buckets.
- Monotonic connection mutex acquisition wall time and acquisition errors.
- Monotonic provider connection mutex hold time, recorded by a stack guard
  before unlock. It includes SQL, SQLite busy handling, commit and Rust work
  inside the guard. Direct registry observer locks are excluded.
- Monotonic `BEGIN IMMEDIATE` call wall time and errors for block PUT and the
  shared MRC5 compact helper. It includes preparation, locking/recovery and busy
  handling; it is not an isolated busy-wait measurement. Existing timeouts,
  retries and returned errors are preserved.
- Monotonic `Transaction::commit` call wall time and errors for block PUT and
  the shared MRC5 compact transaction helper. This includes automatic checkpoint
  work and error-path transaction Drop rollback before the call returns.

SQLite PROFILE uses the bundled SQLite VFS wall clock with approximately 1 ms
resolution. A zero duration means below that resolution; notifications include
unsuccessful execution and cursor reset/finalization. PROFILE does not include
Rust lock acquisition, statement preparation or post-PROFILE WAL callbacks.
Statement starts and PROFILE notifications can differ, including for triggers.
The first-token SQL classifier is unchanged; leading-space or semicolon-only
keywords can retain the `OTHER` label. No SQL text, values or file paths are
exported. Invalid durations and overflow flags make a timing sample incomplete.
The fixed callback counters do not acquire Rust locks or allocate Rust objects;
whole provider calls, JSON snapshots and SQLite allocations remain distinct.

Lock acquisition, hold, SQL PROFILE, provider BEGIN/COMMIT and SDK spans overlap.
Do not add their totals as exclusive CPU time or distinct I/O. Connection
acquisition timing excludes hold time and SQLite busy waiting. BEGIN and commit
counters cover the specified helper calls, not every SQLite transaction in the
workspace. Successful instrumented acquisitions have one completed hold after
their guard drops; poisoned acquisitions have an error and no successful hold.
The guard and fixed counters add no Rust heap allocation per call; this is not a
whole-provider allocation claim. Disabled diagnostics evaluate no added hot-path
clocks.

WAL connections additionally expose current log frames, checkpointed frames and
their difference through an observer `PRAGMA main.wal_checkpoint(NOOP)`. These
are sequential gauges shared by connections to one database: do not sum them or
subtract them across log resets. They are not checkpoint counts, duration or
frames copied by the last checkpoint. Non-WAL connections report `not_wal`;
busy/unavailable probes retain that status. The pinned SQLite implementation
accepts NOOP through the PRAGMA but rejects its negative mode in the `_v2` API.
It skips the checkpoint lock and backfill, but can read/initialize/recover WAL
state and invalidate cached headers. This observer is not stat-only and can
perturb subsequent work. Registry collection wall time is reported separately;
it includes sequential observer locking/queries and excludes final outer JSON
serialization. It is not workload time or exclusively the WAL probe's time.

Drain the workload before sampling. A resetting snapshot returns prior totals,
runs observer queries under callback suppression, then clears counters **after**
the observer queries. The subsequent end totals are already the stage counts;
do not subtract the returned begin totals. Non-reset snapshots leave counters
cumulative. Their pager sample precedes their observer queries, so a later
non-reset sample can include prior observer pager activity. Match positive unique
connection IDs at both endpoints and reject missing IDs, errors, invalid timings,
overflows and incomplete workload boundaries. These sequential observations do
not establish global background-task quiescence.

The raw Rust saturation artifact and raw N-API snapshot retain these fields.
The JavaScript benchmark projector validates and retains configuration, SQL
profiles, lock acquisition/hold and provider BEGIN/COMMIT counters. It rejects
invalid durations, overflow, resets, unknown labels, changed configuration,
inconsistent histograms and unavailable WAL gauges. Cumulative maxima retain
start/end values with exact phase maxima marked unavailable; maxima are never
subtracted. WAL gauges and observer durations retain endpoint observations.
Newly opened connections have no observed initial configuration/WAL gauge and
explicit incomplete boundary-gauge coverage, even when their counters started
at zero. Raw counters stay decimal u64 strings through the N-API projector.

The [30-second write controls](benchmarks/sqlite-wait-backlog-diagnostic-20260928/README.md)
retain four fresh DELETE/WAL runs with full stored/fresh payload checks. They
separate monotonic BEGIN/hold/commit observations from one invalid SQL PROFILE
duration, preserve observer costs and report WAL backlog without claiming
checkpoint work or physical flash amplification. Qualification requiring both
configuration/WAL boundaries must check each projected connection's
`boundary_gauges_complete`, including connections opened during a phase.

### Native SQLite file I/O diagnostics

The opt-in provider observer also selects a bounded process-lifetime,
nondefault wrapper around the native SQLite VFS before opening a connection.
Its neutral context never borrows a provider connection or its counters, so
external SQLite connections selecting its globally registered name can safely
outlive a provider. The native default VFS,
open flags, locking, shared memory, mapped reads and return codes are preserved.
`vfs_observed` checks the actual selected main pager VFS at the drained observer
boundary: a URI that selects a different VFS cannot be reported as observed.
The service catalog and legacy `SqliteStore` use their normal native defaults
and remain outside this wrapper scope. All invocations selecting the observed
wrapper, including external connections, contribute to its process bank.

The top-level `sqlite.vfs` bank has one fixed process-wide set of 15 rows:
`main_database`, `main_journal`, `wal`, `temporary` and `other`, each with native
`read`, `write` and `sync` invocations. It records monotonic call-return wall
time, errors, histograms, requested bytes and bytes confirmed only when the
native method returns `SQLITE_OK`. A short read is an error with unknown actual
partial bytes; the underlying method still supplies SQLite's zero filling.
Sync byte fields are zero. Counts are VFS method invocations, not operating
system syscall counts, physical SSD IOPS or proof of durable storage.

Mapped reads and native shared-memory file operations can bypass these ordinary
file callbacks. Directory syncs, file deletion/truncation and other native VFS
operations are outside these 15 rows. Sync flags are forwarded unchanged;
one `xSync` invocation does not establish how many `fsync` or `F_FULLFSYNC`
operations occurred. These omissions stay explicit when interpreting amplification.

The bank retains file open/close outcomes and close-time I/O after the live
connection registry empties. Lifecycle counters also include sidecar files
opened or closed by observer queries, as its separate lifecycle scope states.
Live file, live provider-marker and registered-VFS counts are gauges;
they retain start/end observations and are not reset with cumulative counters.
Process-lifetime VFS registrations remain alive after provider markers drop;
they are bounded and are not allocated once per Drive or connection.
Drain callbacks before sampling or resetting. An active callback/window or a
detected reset race prevents complete VFS attribution. Observer query suppression
is scoped to the synchronous observer's calling thread, so it excludes observer
I/O without suppressing work on unrelated threads. Registry observer elapsed time covers its existing
per-connection collection; the global bank export is collected afterward.

Checkpoint signals additionally bracket an observed **backfill copy window**.
In pinned SQLite 3.51.3, `CKPT_START` follows the initial WAL sync and iterator/
lock work; `CKPT_DONE` precedes final database truncate/sync and backfill
publication. DONE can follow a copy error or arrive without START after an
initial sync failure. Paired durations, unmatched signals, aborted-at-close
windows and pending windows are recorded separately. They do not count successful
checkpoints, all checkpoint attempts, copied WAL frames or total checkpoint time.
The WAL iterator can copy a page once despite multiple frames for that page.

JavaScript validates this global bank independently of connection attribution.
It retains qualified VFS deltas after connection retirement while keeping full
native/connection attribution incomplete. Invalid or missing banks retain only
bounded partial evidence. Exact counters and histograms are subtracted; maxima
and live gauges retain endpoints. The callback observer adds fixed counters
and clocks, with no per-call Rust allocation or logging by source inspection.
Registration allocates its bounded context once, and the wrapper enlarges
SQLite's native per-file allocation. Shared atomics, clocks, native operations
and snapshot serialization have their own costs; no allocation or disabled-
observer overhead comparison was measured for this wrapper.

### Controlled runtime-worker experiment

The ignored saturation fixture keeps its default 16-worker Tokio runtime. The
strict test-only selector `MOUNT_RS_REMOTE_SATURATION_RUNTIME_WORKERS` accepts
`16`, `32` or `64`; other values fail before providers are opened. Each stage and
final artifact record `runtime.worker_threads` from the actual Tokio runtime.
This is the configured pool size, not a busy-worker measurement. Client depth,
dataset, drivers and storage durability policy remain separate selectors.

Use serial runs with fresh disposable backing state and identical selectors,
including a repeated 16-worker run to detect drift. Settle child containment and
fresh backing verification before starting the next run. The pure selector and
ignored `runtime_worker_selector_reaches_observed_tokio_pool` control verify the
parser and actual spawned-task execution. Small single-process results do not
qualify the 10-server, 10,000-client production topology.
