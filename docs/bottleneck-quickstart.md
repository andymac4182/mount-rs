# Capture remote service bottlenecks

Run from the repository root with a normal service configuration:

```sh
MOUNT_RS_PROFILE_IO=1 MOUNT_RS_DIAGNOSTIC_INTERVAL_MS=5000 \
  ./scripts/cargo-shared run --locked -p mount-rs-cli --features io-profiling \
  -- serve-remote --config /path/to/service.json
```

The interval is optional and accepts decimal milliseconds from 1,000 through
60,000. Without it, service snapshots are emitted at shutdown. Profiling must
be compiled and selected for periodic capture. Disabled profiling creates no
periodic timer and captures no banks.

Records go to stderr with the `service_diagnostics` prefix. Periodic records
carry `capture_context: "periodic"`, a capture sequence and Unix capture time;
QUIC and WebSocket records from the same tick share that metadata. Counters
are cumulative. Compare consecutive records from the same process and
transport; use sequence numbers for ordering because wall clocks can change.
Shutdown records keep their existing v2 shape.
Process storage/profile banks are shared across transports; do not add their
duplicate copies. Take acknowledged operation and payload totals from the client
workload, since response submission does not prove receipt by the client.

## Read the counters

| Observation | What to compare |
| --- | --- |
| Request and storage latency | Calls, outcomes, in-flight work and latency buckets across successive captures. |
| Queue and lock waits | Increasing wait time beside dispatch and provider time. |
| Metadata amplification | Metadata calls and decoded document bytes per acknowledged application operation. |
| Blob amplification | Backing requests and body bytes per acknowledged payload byte, alongside RAM/disk/peer hits. |
| CPU and memory | The benchmark runner's process resource observations beside these logical counters. |

Spans include nested work and must not be added as exclusive CPU time. Captures
are not atomic and do not prove application drain. Logical requests, SQLite
pager observations and process disk accounting are distinct from device IOPS.
Missing observations remain unavailable rather than becoming zero.

For bounded slow-operation records, also set `MOUNT_RS_TRACE_STORAGE=1`.
Storage operations taking at least 100 ms can emit `MOUNT_RS_STORAGE_SLOW` with
fixed operation/outcome labels and elapsed time. Records exclude storage keys,
paths, tokens and raw backend errors. Diagnostic serialization and output add
work; keep a profiling-disabled control when comparing throughput or allocations.

See [the metric catalog and retained measurements](bottleneck-metrics.md) for
provider coverage, units and qualification limits. The current eager CLI uses
startup open/cleanup observations. Runtime activation and eviction metrics
become available when an actual runtime pool is wired into that path.
