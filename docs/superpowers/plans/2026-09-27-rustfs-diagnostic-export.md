# Expose the existing RustFS blob diagnostics

**Goal:** Observe logical blob calls, raw adapter calls and bytes, upload claims,
hashing, copies and waits for the explicit RustFS N-API block provider. This
closes an export gap before a controlled legacy/MRC5 comparison.

## Contract

- Keep the existing R2 registry, measurement fields and legacy pilot/checker
  unchanged. Preserve the canonical R2 qualification guard.
- Add a separate weak RustFS registry only for successful `blocks.kind=rustfs`
  constructors while profiling is enabled. Snapshot publication may allocate;
  registration adds no per-operation work. Existing recorder allocation claims
  remain scoped to fixed bank updates and snapshots, not JSON or provider I/O.
- Native schema remains `mount-rs.storage-diagnostics.v3`; the `rustfs` family
  and `measurement.rustfs`, `rustfs_api`, `rustfs_local` are additive. Both
  providers use the existing object-store stats, raw/local row schemas and exact
  decimal counters. RustFS measurement scope identifies registered split RustFS
  instances and excludes marker, qualification, reconciliation listing,
  unregistered factories and client internal retry work as applicable.
- The benchmark retains separate family identities and counters. Old snapshots
  without RustFS remain unchanged. If RustFS is present at either endpoint,
  missing/malformed/reset/nonquiescent or retired instances remain incomplete.
  A required RustFS workload proof rejects absent or empty RustFS registries.
- Optional local evidence cannot repair missing raw evidence. Family routing is
  a closed `r2|rustfs` choice; no arbitrary labels, paths, configuration or errors
  enter safe observations or phase logs. No cross-family counter aggregation.
- Inclusive durations are not exclusive CPU. Raw adapter calls are not HTTP
  retries or physical device operations; physical IOPS remain unavailable.

## Ownership and validation

- Native worker owns only `bindings/mount-rs-napi/src/lib.rs`: new registry,
  shared scalar projection, fixed RustFS metadata and inert Rust unit controls.
- Root owns `benchmarks/storage/diagnostics.mjs`, its existing test harness,
  metric documentation and this plan. The HTTP worker owns its three scripts.
- Preserve protected eleven dirty files and Cargo.lock. No backend, Engine,
  native addon load/build, global inventory or full capacity run in this slice.
- Native source changes are authorized now. Cargo uses a later explicit root
  lease, `scripts/cargo-shared`, jobs=1, locked graph and isolated `/private/tmp`
  target. Never run Rust checks concurrently with another Cargo owner.

### Native controls

- [x] Add isolated enabled and disabled constructor/export controls first;
  retain intended assertion RED against old production source.
- [x] Require zero backend calls, exact raw/local row order/claims, all zero
  initial gauges, exact counter projection above 2^53, privacy sentinel absence,
  independent R2 identity and weak retirement after drop.
- [x] Implement the separate registry/export, then run targeted GREEN, package
  library controls, touched formatting and strict package library/test Clippy.

### Benchmark controls

- [x] Add RustFS snapshots to the existing pure harness and retain assertion RED.
- [x] Implement separate deltas, sanitized observations, workload validation and
  summary logs with `r2` remaining the default for existing callers.
- [x] Check exact byte/call deltas, family isolation, empty/missing registries,
  invalid family, reset/saturation/in-flight/retirement, metadata provenance,
  optional local unavailability, and secret redaction.
- [x] Run the existing benchmark harness and bounded pilot/observer/checker
  controls under Node 24 with the explicit inert capture selection.

### Delivery

- [ ] Freeze exact source and retained gate hashes for independent review.
- [ ] Commit/push the accepted files and update the existing draft PR.

This slice provides inert local export and consumer controls. It does not prove
live RustFS safety, a compiled addon identity, benchmark throughput, physical
IOPS, the 10,000-client target or the broader goal's completion.

## Local validation checkpoint

Both new isolated native controls first failed at the intended missing RustFS
family assertions. Final enabled/disabled RustFS and existing R2 controls each
passed one selected test. The N-API library suite passed 43 tests with six
isolated controls ignored; strict library/test Clippy and scoped formatting
passed. An initial JSON macro recursion compile error was repaired by extracting
the fixed RustFS API measurement helper; its failed source/log is retained.

Node 24.18.0 controls passed ten diagnostic groups, the complete existing
benchmark harness and 158 backing observer/pilot/checker tests. Independent
review found that a directly supplied processed RustFS phase could omit its
logical measurement metadata. Added rejection controls reproduced that gap;
the RustFS validator now requires that metadata before accepting raw evidence.
An actual Rust unit constructor snapshot also passed the Node consumer's raw
and local validators with zero workload activity. That join validates export
shape, rather than live backend traffic or throughput.

The local Cargo build includes the eleven pre-existing protected dirty files;
their hashes and Cargo.lock remained unchanged. It is not a clean hosted build
receipt. No addon build/load or backing service execution occurred in this slice.
CI selects the two new isolated native controls separately with profiling on/off.
