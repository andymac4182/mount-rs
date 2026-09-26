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

The ordinary server constructors keep the new observer disabled. Service
embedders can explicitly use `RemoteServer::bind_with_diagnostics(..., true)` in
an `io-profiling` build, retain `server.diagnostics()`, and call its `snapshot()`
at controlled boundaries. This is a local API, with no diagnostic network
endpoint.

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
