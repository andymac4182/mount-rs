# Storage bottleneck metrics implementation plan

> **For agentic workers:** Use subagent-driven-development with one source/Cargo/native owner and independent specification and quality review.

**Goal:** Make the actual public storage benchmark expose the counts, bytes, waits, latency distributions and outcomes needed to locate the lifecycle bottleneck.

**Architecture:** Reuse existing filesystem/provider profile counters and SQLite engine diagnostics. Add a bounded opt-in storage latency/outcome recorder, instrument the actual N-API dynamic provider adapters and relevant backend boundaries, and export read-only snapshots for quiescent benchmark phase reports. Emit bounded slow-operation records with fixed labels. All measurements retain their true logical, pager, inclusive-time or process scope.

**Tech Stack:** Rust, atomics, serde, N-API, Node 24, SQLite, existing TiDB and object-store clients.

## Global constraints

- Instrumentation is disabled unless `MOUNT_RS_PROFILE_IO=1` is set before native module initialization. No paths, object keys, SQL text/parameters, namespaces, credentials, tokens, or raw error messages appear in new metrics or logs.
- Histogram labels and storage are finite enums and fixed arrays; no additional allocation or blocking logging on the default disabled request path. Enabled recorder updates must not allocate per operation; snapshots may allocate at phase boundaries. The existing boxed async-trait ABI requires an additional forwarding future in the enabled N-API adapter. This opt-in observer cost is permitted and must expose exact allocation-site counts and requested object bytes, with enabled/disabled controls and an explicit distinction from global allocator counts. No zero-allocation claim applies to the enabled adapter.
- Count success, error and cancellation distinctly. Histogram bounds and inclusive duration semantics are explicit; nested/parallel durations cannot be summed as exclusive CPU time.
- Count logical provider calls separately from HTTP attempts, SQLite pager operations and physical device IOPS. Internal client retries must remain marked unavailable unless directly observed.
- Keep the existing 1,000 lifecycle operations/s gate, 400 iterations, 64 concurrency, 4 KiB payload and 64 KiB chunk controls unchanged. Diagnostics have no throughput claim and compare against a disabled control.
- Preserve Linux/macOS mounting, Partition/Drive authorization, backing authority, durable acknowledgment and uncertain-write nonreplay behavior.
- Preserve all eleven dirty protected files. In particular, do not edit `src/diagnostics/profile.rs` or `filesystems/mount-rs-chunked/src/lib.rs`; reuse their current counters through the public snapshot interface and retain their hashes.
- Serialize Cargo and timed loads. Use `scripts/cargo-shared` and the existing checkout-specific external target `/private/tmp/mount-rs-public-compact-selection-cargo-target`. No new target tree or external fixture restart/reset/purge.
- Full goal remains ten servers, 10,000 clients/Drives, 5,000 Partitions, 1,000 files per Drive in mostly-idle and all-active modes, original 600/1800/30 second limits, 64 GiB free host disk, 24 GiB per owned process and 10 GiB TiDB fixture floor. This diagnostic slice does not establish full capacity.

## Task 1: Actual-path storage diagnostics and benchmark phase logging

**Files:**
- Create `src/diagnostics/storage.rs` for the fixed-label recorder, snapshot/delta validation, optional bounded slow logging and tests.
- Modify `src/diagnostics.rs` to export the recorder.
- Modify `bindings/mount-rs-napi/src/lib.rs` (or focused adjacent diagnostics module), `index.d.ts` and focused native/Node tests to instrument `DynMetadataStore`/`DynBlockStore` and expose a read-only `storageDiagnostics` snapshot.
- Modify `benchmarks/storage/{runner.mjs,providers.mjs,test.mjs,README.md}` and a focused adjacent diagnostics helper/test if separation improves clarity.
- Modify `.github/workflows/ci.yml` only to enable the opt-in diagnostics on the existing Ozone lifecycle measurement jobs, preserving their commands, floors and workload values. Ensure benchmark artifact/verifier checks accept and retain the new diagnostics.
- Modify the narrow diagnostics environment pass-through in `scripts/test-foundationdb.sh` and equivalent wrappers if needed so containerized measurements receive the flag and failed benchmark JSON is copied out; preserve workload and gate behavior.
- Modify only the narrow central TiDB connection acquisition seam and object-store logical request/cache boundary when they are needed to distinguish backend waits and network requests. Preserve their error propagation, retries and durability behavior. Existing SQLite engine counters are reused rather than replaced.

**Interfaces:**
- Consume `mount_rs_core::diagnostics::profile::{snapshot,Span,Event}`, `mount_rs_sqlite::sqlite_io_diagnostics(false)` and existing provider operations.
- Produce a versioned native JSON snapshot and benchmark phase artifacts with explicit enabled/available flags, integer counts, elapsed time, fixed log2 latency buckets, bytes, outcomes and honest unavailable fields.
- Phase deltas validate stable names and monotonic counters. SQLite connections are matched by connection ID; missing/closed connections are reported as incomplete attribution rather than silently treated as zero.

- [ ] Write and run meaningful failing tests for real instrumented provider calls, positive byte counts, error/cancellation classification, delta counter reset rejection, default disabled behavior, bounded/log-safe samples, and benchmark phase attribution. A missing export alone is insufficient as the only RED.
- [ ] Run the complete ordinary focused Rust suite with profiling unset. Isolate any enabled GLOBAL-recorder exact-count probes from other provider tests using dedicated subprocess/filter execution or an injected recorder, and document their invocation. Default tests must not require an opt-in flag or introduce a parallel-count race.
- [ ] Implement the recorder with a disabled fast path and snapshot-only allocation. Instrument selected load/conditional load/snapshot/publish, block put/get/flush/backing verification and pool wait boundaries actually reached by N-API. Reuse existing core profile events so serialization, snapshot and gate work can be compared to backend calls.
- [ ] Add opt-in benchmark diagnostics initialized before loading the addon. Capture before/after create, measured workload, cleanup and shutdown; log one bounded summary per phase and retain detailed numeric deltas in JSON. Distinguish measured provider overhead from observer work, source and binary identity, and any incomplete phase.
- [ ] Enable the diagnostics through the existing three Ozone measurement job environments so CI retains numeric failure evidence, including when the unchanged floor is missed. Keep native builds and ordinary application requests disabled by default.
- [ ] Run current-source native SQLite byte/EOF/reopen lifecycle with diagnostics enabled and disabled, inspect nonzero metadata/blob/SQLite counters and reconcile declared operation counts. Include a negative native error and an actual cancellation control. No external Ozone/TiDB result may be claimed from an in-memory fake.
- [ ] Capture the native correctness control at explicit live-connection boundaries before shutdown and after reopen. A connection created and closed between snapshots cannot be declared fully attributed. Keep CPU/resource/wall endpoints outside all substantial snapshot work, including memory collection, and test observer overhead separation.
- [ ] Retain shared measurement metadata in benchmark JSON: inclusive duration scope, histogram intervals and terminal overflow, logical provider/cache scope, SQLite pager estimates, and explicitly unavailable physical IOPS/retries/TiDB waits/allocation counters. Forwarding allocation-site counters are an explicitly observed subset.
- [ ] Run touched formatting, strict Clippy, focused Rust and Node tests. Freeze exact owned sources, binary, commands/logs and JSON with SHA-256 identities and protected-file hashes. Report observed counters without inferring physical disk IOPS or promising the floor.
- [ ] Obtain independent specification and quality review, resolve important findings, then commit/push only reviewed task files and update draft PR #28 accurately.

## Follow-on measurements

After this reviewed slice, run serial matched legacy/MRC4/MRC5 diagnostics against available actual backends and the unchanged Ozone gate. Count server-side SQL/HTTP/device work separately where the current clients cannot observe physical attempts. Use that evidence to select a storage optimization and test the failed gate. Full population, cache composition, formal proofs, current CI and final merge remain required by the parent goal.
