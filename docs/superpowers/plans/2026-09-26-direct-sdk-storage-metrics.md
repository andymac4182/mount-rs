# Direct SDK storage metrics

Scope: core fixed recorder and every current erased SDK metadata/block forwarding method, including synchronous capability methods. Existing NAPI labels retain their identities. TiDB labels are reserved; this slice makes no TiDB coverage claim.

1. Run an isolated enabled behavioral RED through the actual erased SDK block adapter and a held local fake provider before adding production spans.
2. Add fixed sdk.metadata.<exact method> and sdk.blocks.<exact method> operations, row in-flight gauges, and optional successful returned-row observations. Keep ordinary success rows unavailable; known zero is an observed result.
3. Instrument existing forwarding bodies without new boxes, preserving optional Telemetry, profile events, arguments, results and errors. Successful block put input bytes and get/migration returned bytes are known logical payloads. Metadata payload bytes remain unavailable: zero counters here do not establish zero bytes.
4. Check held pending/error/success/cancel behavior, terminal gauges, checked counter deltas, reserved labels, and isolated enabled/disabled allocation scopes. Timers are inclusive and may overlap; existing async-trait futures can grow from inline span state even when disabled.
5. Serialize focused core/SDK tests, observability feature checks, strict scoped Clippy, and touched formatting through scripts/cargo-shared with the leased target directory. Capture commands, sources, binary hashes and failures in the separate evidence packet.
6. Freeze source/diff/evidence, verify protected11, and explicitly release Cargo for independent review.

No source changes outside the owned recorder, SDK adapters, this new plan, and focused new SDK test module. No provider, runner/exporter, dependency, backing I/O, services, workload, network, commit or staging actions. Full target/capacity, formal/cache and merge gates remain open. Direct worker/controller recorder export remains separately owned.
