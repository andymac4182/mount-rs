# Cache stage metrics qualification — 2026-09-27

This slice measures the existing distributed cache path before changing its
locking or storage format. It appends six storage stages and two hit-byte events,
preserving the previous indexes: **91 storage rows /136 core rows**.

## What the new rows explain

| Stage | Boundary |
| --- | --- |
| Miss admission | Waiting for the existing distributed miss semaphore |
| Singleflight | Waiting for the existing per-block flight mutex |
| RAM lookup | Existing probes, including misses and the returned payload copy |
| Disk lookup | Existing bounded permit/worker/read/verification helper |
| Peer connection lock | Waiting for the existing per-peer connection slot |
| Peer establishment | Existing QUIC/TLS connection and authenticated identity checks |

Each row retains calls, success/error/cancellation, in-flight work, returned
bytes and 32 latency buckets. RAM/disk hit counters distinguish an empty hit
(one call, zero bytes) from an absent hit. Queue and connection bytes are zero.
Durations are inclusive wall time; nested and parallel work overlaps.

The [numeric report](report.json) contains the actual 64 cache and 26 peer
observations, their phase labels and the selected source/receipt hashes.
Pending observations are absolute snapshots; other observations are deltas.
These controlled fixtures deliberately hold responses and use UDP blackholes.
Their timings are not production latency samples.

## Validation

All **17 local gates** passed against unchanged selected inputs, including the
Cargo wrapper's sourced environment script: **468 distinct inputs** across Rust
and consumer manifests. The gates executed **129 passing Rust tests** and
**176 passing Node consumer tests**, plus the same three regression selectors
again. **37 modeled parent controls** passed. Formatting and strict Clippy
passed for core/cache and CLI/NAPI surfaces. Counts describe executions, not
unique tests or release qualification.

The cases verify 100 cold coalesced readers with one peer GET and zero backing
GETs; warm RAM/disk and empty hits; dropped admission, flight, disk and connection
waiters; disk timeout with successful peer fallback; cold/reused mTLS connections;
rejected partition and certificate identity; and overlapping blackhole PUT/GET.
Full-byte behavior oracles precede metric assertions. The signed CLI case also
verifies QUIC/WebSocket writes and clean SQLite reopen; seven named diagnostic
unit tests verify the exporter and unchanged record bounds.

Old 85-row storage reports retain supplied observations and mark the six missing
rows unavailable. Old 134-row core reports remain partial in general projection
and fail current exact qualification. Addon audited instrumentation remains
78 rows without FoundationDB /85 with it; declaring the cache family does not
claim the addon uses it. Native default-feature unit tests passed (52 tests,
six ignored); native FoundationDB feature-on runtime was not run locally.

## Allocations and logging

Warmed public recorder gates measured **zero allocation calls** for 13 selected
storage rows and 89 selected core events. These windows exclude initialization,
snapshots, printing, payloads and async state. Complete warm RAM reads in the
unchanged 4KiB debug microbenchmark still report about **two allocations and
4,200 allocated bytes per read**. Counts are rounded to three decimals.

The first measured profiled RAM hit initializes the 136-row core recorder.
Its 3,264-byte metric bank on this 64-bit target explains the extra approximately
0.033 bytes per read over 100,000 reads. Environment-flag initialization can
also allocate; the rounded output cannot establish the exact setup count.
There is no whole-path zero-allocation or controlled speedup claim.

A separate trace-on gate observed a fixed cache slow record. The parent rejects
absent/malformed/private labels, durations below 100ms, and more than 16 records
or an oversized line. Earlier slow stages may consume the shared log budget.
The actual local run emitted a disk error record; it need not be the first
logged stage on a busy host. Opt-in stderr writes can extend an existing mutex
hold, so allocation and timing controls keep tracing off.

## Enable and collect

Build the CLI with `--features io-profiling`, set `MOUNT_RS_PROFILE_IO=1` before
constructing stores, and optionally set `MOUNT_RS_TRACE_STORAGE=1` for bounded
slow logs. The CLI emits cumulative banks in its best-effort bounded shutdown
`service_diagnostics` record. There is no new live metrics endpoint. Use a
lossless integer JSON parser for its raw uint64 values.

See the [metrics guide](../../bottleneck-metrics.md#distributed-cache-lookup-and-peer-connection-stages)
and [CLI export contract](../../bottleneck-metrics.md#public-cli-diagnostic-export).

Five exact metrics/allocation/logging cases are now wired into Remote Drives CI.
Their hosted execution and merge remain separate from these local results.
This report does not qualify physical IOPS, backend saturation, power loss,
production reconnect contention, complete formal coverage or the full
10-server/10,000-client production workload. Discovery, peer request admission
and stream I/O, incoming peers and WebSocket transport still need stage timing.
