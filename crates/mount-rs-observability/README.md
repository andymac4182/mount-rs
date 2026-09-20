# mount-rs-observability

`mount-rs-observability` is the opt-in instrumentation seam for mount-rs
applications. It deliberately sits above `mount-rs-core`: the core contract
does not depend on tracing, OpenTelemetry SDKs, exporters, or subscribers.

The default feature set contains only the lightweight `tracing` facade and is
disabled until an application constructs an enabled [`Telemetry`] value. A
disabled value returns the original futures without allocating spans or
updating counters. Applications own subscriber and exporter setup; the
`otlp` feature provides an explicit `install_otlp` helper for applications
that want the standard OTLP/HTTP traces, metrics, and logs pipeline.

Paths, provider owners, bearer tokens, block IDs, file contents, and backend
error messages are not emitted. Operation spans carry only static operation
names and bounded path shape metadata. Error events carry the finite
`mount-rs-core::ErrorCode` value, never the error's display text.

See `docs/observability.md` in the repository for the signal schema, setup,
shutdown, and evidence boundary.
