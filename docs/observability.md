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
when the relevant feature is enabled. These are deterministic unit/integration
checks; they do not prove that an external collector is reachable. A demo or
release claim of end-to-end export still requires a live collector-backed run
with the exact binary revision and OTLP endpoint recorded.
