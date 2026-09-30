# QUIC setup and cache discovery observations

Two fixed rows complete the outgoing QUIC setup and cache discovery timing gaps:

| Row | Measured boundary |
| --- | --- |
| `client.quic.connection_setup` | Existing TLS/config/endpoint construction, QUIC connect, failure classification and ALPN validation. Excludes credentials, hello and WebSocket fallback. |
| `blob_cache.discovery.locate` | Existing locate await inside the peer deadline. Ends before filtering, peer requests or hedging. Empty/fallback peer lists are successful results, not directory health evidence. |

The storage bank grows from 108 to 110 rows with 23 units families. Existing ordinals and the 136-row core bank are preserved. Native addon source coverage remains 78 default / 85 FoundationDB; declarations do not establish observed client/cache work.

## Local qualification

All 16 owned gates passed on the same frozen sources: actual QUIC setup controls, discovery component controls, core diagnostics, warmed recorder allocations, Node consumer controls, typed CLI diagnostics, native Rust exporter tests, signed CLI compact selection/reopen, cache/client ordinary suites, existing WebSocket/cache stages, three strict Clippy gates and formatting. Every gate retained unchanged inputs, child reap, absent process group and pipe EOF. Three expected semantic REDs are retained separately.

- QUIC: one negotiated success, fatal TLS-name and ALPN errors, caller cancellation after an actual initial UDP datagram, and an internal deadline error. Pending cancellation shows one in-flight setup; all terminal gauges settle.
- Discovery: nonempty/empty/error results, three warm RAM bypasses, caller cancellation, identical-key retry with one miss permit, and existing peer deadline fallback. Full 4,096-byte binary payloads and exact discovery/peer/backing call counts are checked. Ownership barriers precede directory removal.
- The warmed public recorder measured **zero allocation calls across 32 selected operation rows (96 Span invocations)**, including both new rows. This excludes setup, snapshots, logging, async bodies and payload allocation.
- Consumer controls preserve exact large integers, independent historical 108-row descriptors and unavailable measurements. There are **242 pure Node cases** and **104 modeled parent controls**, separate from Rust/runtime tests.

[report.json](report.json) retains source and receipt/log hashes, actual stage outcomes and timings, pending gauges and per-gate case counts. Timings include fixture holds; they are observer validation, not deployment latency or throughput comparisons.

## Enable collection

Set `MOUNT_RS_PROFILE_IO=1` before constructing client/cache objects and take `mount_rs_core::diagnostics::storage::snapshot()` in that process. Calls, outcomes, inclusive wall time, 32 latency buckets and in-flight gauges distinguish waiting from completed work. Bytes and SQL returned rows are unavailable for these two stages.

Set `MOUNT_RS_TRACE_STORAGE=1` for the existing fixed-label slow records at 100 ms or longer, limited to 16 per process. Records exclude tokens, paths, keys, peers and payloads. Leave tracing disabled for throughput/allocation comparisons. Service stage collection additionally requires its `io-profiling` feature and explicit observer selection; see [the bottleneck guide](../../bottleneck-metrics.md).

Reproduce the two exact controls from the repository root with a target directory isolated to that checkout:

```sh
CARGO_TARGET_DIR=/absolute/isolated-target python3 -B scripts/test-remote-failures.py clientsetupmetrics
CARGO_TARGET_DIR=/absolute/isolated-target python3 -B scripts/test-remote-failures.py discoverymetrics
```

The owned parent supplies profiling/trace settings, enforces each named test, bounds execution and retains private raw evidence. CI calls the same selectors.

## Qualification limits

This is a local macOS observer qualification. Discovery is a controlled adapter fixture; real Redis/directory-network performance is unmeasured. Client setup tests use generated local TLS and omit public OIDC; the separate signed CLI gate covers authenticated compact selection and clean reopen. Incoming peer setup and dedicated client CLI shutdown export remain gaps. Inclusive spans cannot be summed as exclusive CPU/network time. These results establish no whole-client zero-allocation, physical IOPS, crash/power-loss, native-mount, cross-host, hosted CI or full production-capacity claim.
