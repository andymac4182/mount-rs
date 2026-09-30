# Local QUIC service metrics

## Scope

Add an opt-in local Rust observer to the existing QUIC server. Ordinary bind
constructors keep diagnostics disabled. The explicit diagnostic constructor
enables observation only in an `io-profiling` build; default builds retain the
same callable API and return no observer. No endpoint, protocol, authentication,
admission capacity, retry, acknowledgement or storage behavior changes.

The observer counts fixed-label service spans with terminal outcomes,
cancellation, inclusive elapsed time and logarithmic latency histograms. RAII
tracks application handshakes and requests, including response submission and
session cleanup. Snapshots expose activity sequence envelopes so concurrent
observations cannot be called quiescent just because one sampled gauge was zero.

Accepted Quinn connections receive monotonic local IDs in a bounded registry.
On task completion, final observed transport counters are folded into retired
totals and the registry's strong connection reference is removed. Active and
retired observations are separate. UDP bytes/datagrams/I/O calls, frame counts,
path counters and current path gauges retain their actual Quinn meanings.
Unaccepted or failed TLS connection traffic, packets after retirement, physical
network headers, storage I/O and peer receipt are outside this collector.

Counter saturation and incomplete registry coverage invalidate completeness.
Snapshots are serial observations, not an atomic cut across Quinn and service
atomics. Profiling snapshots may allocate; disabled hot paths add no diagnostic
allocation, mutex or clock read. Async future frame-size changes remain a
separate measurement, not a zero-allocation claim.

## Implementation and validation

1. Read the existing service and runner source contracts. Create a new loopback
   test before implementation. Add forwarding-only public API scaffolding with
   no recorder and capture actual behavioral RED: the requested observer is
   absent while the same QUIC server still binds. Freeze that source and log.
2. Add the fixed recorder, RAII lifetimes and bounded active/retired registry;
   make the same observation assertion GREEN.
3. Add synchronized loopback tests for held authentication and request work,
   successful and denied dispatch, malformed requests, cancellation, connection
   retirement and default-disabled operation. Use handshakes/notifications for
   causality, not timing thresholds.
4. Verify focused default and profiling service tests, strict scoped Clippy,
   touched formatting and diff checks through the existing shared Cargo target.
   Serialize Cargo/native/load commands with the root's explicit lease.
5. Preserve protected dirty files and earlier evidence. Freeze only this slice's
   source hashes, patch, exact command receipts and report. Request independent
   review before acceptance; do not stage or commit from this task.

## Export contract

`RemoteServer::bind_with_diagnostics(..., options, limits, enabled)` is an
additional constructor. `RemoteServer::diagnostics()` returns a cloneable local
observer when configured. The observer remains valid after `close()` so cleanup
and retired observations can be captured. Serializable snapshots retain exact
Rust integers, fixed labels, schema/coverage, active gauges, activity sequence
before/after, observer wall interval and active/retired transport data. The
runner must preserve integer precision in any later JavaScript consumer.

Service dispatch duration includes authorization, catalog checks and provider
work in the existing dispatcher. It does not isolate the provider or catalog
substeps; separate storage metrics describe those nested boundaries. Inclusive
spans must not be summed as CPU time or used as a disjoint latency decomposition.
No drive, partition, bearer, payload, URL, path or error text is added to labels
or observer logs.
