# Observability (W30)

`mount-rs-observability` is the optional instrumentation seam for the
filesystem, provider, HTTP, SDK, Node, and CLI boundaries. It is deliberately
outside `mount-rs-core`; the core crate and provider contracts do not acquire
tracing, OpenTelemetry, exporter, subscriber, or network dependencies.

## Opt in from an application

The facade is disabled by default. An application that only wants deterministic
local counters can enable the crate and create a handle:

```toml
mount-rs-observability = { path = "crates/mount-rs-observability" }
```

```rust
let telemetry = mount_rs_observability::Telemetry::new(
    mount_rs_observability::TelemetryConfig::enabled("demo-service"),
);
let driver = mount_rs_observability::InstrumentedDriver::from_arc(
    filesystem.driver(),
    telemetry,
).into_arc();
```

Applications own subscriber and exporter setup. For the built-in OTLP/HTTP
setup, enable the `otlp` feature and keep the returned guard alive until the
application has stopped serving:

```toml
mount-rs-observability = {
    path = "crates/mount-rs-observability",
    features = ["otlp", "http-propagation"],
}
```

```rust,no_run
use mount_rs_observability::{install_otlp, OtlpConfig, TelemetryConfig};

let guard = install_otlp(OtlpConfig {
    endpoint: "http://127.0.0.1:4318".into(),
    service_name: "mount-rs-demo".into(),
    ..OtlpConfig::default()
})?;
let telemetry = mount_rs_observability::Telemetry::new(
    TelemetryConfig::enabled("mount-rs-demo"),
);
mount_rs_observability::set_global(telemetry);
// Stop accepting work, then flush and shut down the three providers.
guard.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`OtlpConfig::endpoint` is the collector base URL. The built-in setup sends
traces, metrics, and logs to its `/v1/traces`, `/v1/metrics`, and `/v1/logs`
signal paths respectively. It uses the blocking OTLP HTTP client behind the
SDK's worker processors, so exporter threads do not require an ambient Tokio
runtime; applications that need a different runtime or client can own their
providers and call `set_global` instead.

Exporter failures are isolated from filesystem operations. `OtlpGuard::shutdown`
and `force_flush` return aggregated provider errors so the owning application
can report them without turning an in-flight read or write into an exporter
failure.

## Signal contract

The versioned schema is `mount-rs.telemetry.v1`. Operation spans use the
`mount_rs.operation` target and stable names such as `core.open`,
`core.handle.read`, `provider.blocks.put`, and `http.request`. They contain
only static boundary/operation names plus shape metadata:

- `path_rooted`: whether the input began with `/`;
- `path_depth`: capped at 16 segments by default;
- `path_size_bucket`: one of six fixed size buckets.

Operation completion events use the `mount_rs.event` target and the bounded
fields `boundary`, `operation`, `outcome`, `error_code`, and `duration_ms`.
Metrics use fixed instrument names (`mount_rs.operations`,
`mount_rs.errors`, `mount_rs.operation.duration_ms`,
`mount_rs.bytes.read`, and `mount_rs.bytes.written`) and bounded labels for
boundary, operation, outcome, and error code.

Paths, provider owners, bearer tokens, block IDs, file contents, backend error
messages, and arbitrary request headers are not emitted. HTTP trace context
helpers accept only the W3C propagation headers when `http-propagation` is
enabled.

For the HTTP transport, use `mount-rs-http` with its explicit
`observability-otlp` feature and pass the application-owned `Telemetry` handle
through `HttpServerOptions::with_telemetry`. That feature extracts the W3C
parent from request headers and attaches it to the request span; the ordinary
`observability` feature records a local request span without pulling in the
OTLP SDK.

## Disabled mode and boundaries

`Telemetry::disabled()` is the default. Disabled wrappers return the original
future without creating spans, formatting path metadata, or updating counters.
The `mount-rs-cli`, `mount-rs-http`, `mount-rs-sdk`, and `mount-rs-napi`
integrations keep their observability features off unless the application
explicitly enables them. The CLI feature reads `MOUNT_RS_TELEMETRY` and still
leaves exporter setup to the embedding application.

## Verification boundary

The crate tests cover disabled behavior, bounded path summaries, driver
round-trips, byte counters, configuration bounds, and W3C carrier round-trips
when the relevant feature is enabled. The `otlp_collector` integration test
also runs a loopback collector and verifies that all three signal paths receive
non-empty payloads without a raw path value. The `otlp_failure` test verifies
that setup, flush, and shutdown failures remain in the guard API. These tests
prove the local exporter/collector boundary, not reachability of an external
collector. A demo or release claim of external end-to-end export still
requires a live collector-backed run with the exact binary revision and OTLP
endpoint recorded.
