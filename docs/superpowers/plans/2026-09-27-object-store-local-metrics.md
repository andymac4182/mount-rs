# Object-store local bottleneck metrics implementation plan

> **For agentic workers:** Use executing-plans to implement each task with its recorded RED/GREEN evidence and independent source review. Root owns commits and publication.

**Goal:** Attribute object-store block hashing, ID encoding, payload/cache/return copies, cache acquisition and coalesced-upload waits without changing storage behavior.

**Architecture:** Add seven fixed local rows in a `LocalState` inside the existing opt-in boxed `RawState`. Export a separate optional `local_work` snapshot and `measurement.r2_local`; local availability and validation are independent of the existing raw API qualification contract. Recorder updates and fixed snapshots allocate nothing; disabled local spans do not read a clock.

**Tech stack:** Rust atomics, stack spans and existing SHA256/object-store/watch implementations; existing NAPI JSON exports; pure Node 24 diagnostics and projection controls.

## Global constraints

- Native schema remains `mount-rs.storage-diagnostics.v3`.
- Raw schema remains `mount-rs.object-store-api.v1`; ten raw rows, eight claim counters and `measurement.r2_api` are unchanged.
- Local schema is `mount-rs.object-store-local.v1`; scope is `one_object_store_block_store_instance`.
- Ordered local rows are `sha256.digest`, `block_id.encode`, `copy.upload_payload`, `copy.cache_insert`, `copy.return_vec`, `cache.lock_acquire`, `put.follower_wait`.
- Each row has `calls`, `success`, `error`, `cancelled`, `elapsed_ns`, `latency_max_ns`, `input_bytes`, `output_bytes`, and `latency_log2_us[32]`.
- `input_bytes` counts entered work; `output_bytes` counts completed work. Digest output is 32 bytes, encoded ID output is 65 bytes, copies count the actual copied length, and both wait rows have zero bytes.
- Local saturation and in-flight gauges are independent of raw API saturation and in-flight gauges.
- Durations are inclusive wall nanoseconds. Mutex duration ends on acquisition result. Cumulative maxima are retained, never subtracted; exact phase maximum is unavailable.
- A successful digest remains successful if a subsequent integrity comparison rejects the block. A surviving follower receiving leader failure is an error; a dropped follower span is cancelled.
- Missing or disabled local snapshots are unavailable, never zero. Invalid optional local evidence does not alter raw qualification gates.
- No codec, hash/copy/claim algorithm, SDK, backing schema, workflow, floor, capacity or trace changes. The adapter performs no compression; physical IOPS and global allocation counts remain unavailable.
- Root explicitly releases the startup Cargo/source lease before any Rust edits or Cargo execution. No native/backing runtime is authorized by this plan.

## Exact owned files

- `providers/mount-rs-object-store-blocks/src/lib.rs`: local spans at real hash/copy/lock/wait boundaries; optional public stats sibling.
- `providers/mount-rs-object-store-blocks/src/raw_metrics.rs`: independent local fixed bank/snapshot/span within the current enabled allocation.
- `providers/mount-rs-object-store-blocks/src/raw_metrics_tests.rs`: enabled, disabled, byte-count, cancellation and allocation controls.
- `bindings/mount-rs-napi/src/lib.rs`: local snapshot/metadata JSON and tests in its existing module.
- `benchmarks/storage/diagnostics.mjs`: optional local validators, phase deltas and safe projections.
- `benchmarks/storage/test.mjs`: pure diagnostics controls using the existing capture guard.
- `benchmarks/storage/owned-backing-pilot.mjs`: bounded local evidence projection.
- `benchmarks/storage/owned-backing-pilot.test.mjs`: local projection/privacy controls.
- This plan file: execution evidence and contract limits.

### Task 1: Node optional local contract

**Interfaces:** Export `OBJECT_STORE_LOCAL_NAMES` and `OBJECT_STORE_LOCAL_MEASUREMENT`. Native instances consume `local_work: null | {schema, scope, saturated, in_flight, entries}`. Observed local deltas produce `{status: "observed", complete: true, schema, scope, in_flight_start, in_flight_end, entries}`; unavailable/invalid values produce fixed `status`, `complete: false`, and fixed issues with no trusted private fields.

Processed consumers call `validateLocalPhaseDiagnostics(local, measurement)` with the phase's `measurement.r2_local`; valid-looking rows without exact local metadata cannot be observed. This validation remains separate from raw qualification.

- [x] Add a pure control with two valid local snapshots and assert three calls, exact input/output bytes, histogram subtraction and retained start/end maxima:

```js
const delta = deltaNativeSnapshots(withLocal(2), withLocal(5))
const local = delta.r2.instances[0].local_work
assert.equal(local.status, "observed")
assert.equal(local.entries[0].calls, "3")
assert.equal(local.entries[0].input_bytes, "12288")
assert.equal(local.entries[0].output_bytes, "96")
assert.equal(local.entries[0].latency_max_ns_start, "1000")
assert.equal(local.entries[0].latency_max_ns_end, "1000")
```

- [x] Run the existing benchmark test entry under Node 24 with `NAPI_RS_NATIVE_LIBRARY_PATH` set to the existing `benchmarks/storage/capture-native.cjs`; retain the assertion RED.
- [x] Implement separate seven-row validation/deltas. Compare existing measurement metadata with only `r2_local` excluded from raw equality; validate local metadata separately. Preserve raw ten-row validation exactly.
- [x] Add missing/null, availability change, invalid metadata, wrong/duplicate names, saturation, reset, nonzero boundary gauges, byte applicability, malformed histogram/maxima, and sanitized secret controls. Assert raw diagnostics remain complete when only optional local evidence is invalid.
- [x] Run the pure controls GREEN and both touched Node syntax checks; retain exact command/log/hash evidence.

### Task 2: Bounded pilot projection

**Interfaces:** Consume each phase instance's local delta from Task 1 and project `local_work` beside existing raw instance rows. Use only seven fixed names, canonical decimal counters, 32 histogram slots and fixed availability/issues. Keep the 16-instance cap and existing raw qualification fields unchanged.

- [x] Add a pilot projection control asserting local counters survive and unknown labels/private values do not survive; observe RED before production changes.
- [x] Implement fixed-label local projection with duplicate detection, explicit unavailable evidence and no changes to raw component completeness.
- [x] Run the pilot, entry, observer, transport and checker tests with the existing Node24/capture guard, and verify legacy raw fixtures remain valid.

### Task 3: Rust local bank and real boundaries, after lease release

**Interfaces:** `LocalSpan::new(Option<&LocalState>, Local, input_bytes)` records one entered span; `success(output_bytes)` and `result(&Result<_, _>, output_bytes)` finish once; drop records cancellation. `new_with_clock` evaluates its clock closure only for enabled state, allowing a positive clock control without a disabled-path read. `RawState` owns `LocalState`; public stats expose `Option<LocalWorkSnapshot>`.

- [x] Extend the held six-identical-upload control first: six digest/encoding rows, one payload/cache copy, five follower waits, one actual raw upload. Run RED before adding the bank or spans.
- [x] Add the fixed atomic local rows and independent saturation/in-flight accounting. Keep raw snapshot fields and counters unchanged.
- [x] Instrument SHA256 and ID encoding separately without changing their algorithms; instrument only actual payload/cache/return copies. Pass an optional bank into cache operations; end cache acquisition spans immediately after the mutex result. Wrap the follower await with a cancellation-safe span.
- [x] Add cold/hot/migration return-copy controls, integrity rejection with no cache/return copies, backend failure with no cache copy, cancelled leader/surviving and dropped followers, poisoned cache acquisition, unpolled future drop, local saturation and histogram/outcome reconciliation.
- [x] Extend the existing allocator positive control to local enabled/disabled updates and fixed snapshots. Assert disabled clock closure is never invoked. Preserve the distinction between recorder allocations and existing provider/future-frame allocations.
- [x] Run scoped provider tests through `./scripts/cargo-shared`, jobs1 and the explicitly leased isolated target; retain RED/GREEN and measured bank/span sizes. Run touched formatting and strict Clippy only after tests pass.

### Task 4: NAPI export and final review

**Interfaces:** Serialize optional local snapshot beside `raw_api`; recursive `stringify_counters` keeps exact decimal strings. Add static `measurement.r2_local` matching the Node constant. `storageDiagnostics(): string` and generated bindings remain unchanged.

- [x] Extend the isolated zero-service-call R2 constructor/export control first with local schema, seven fixed rows, enabled presence and exact string counters; retain RED.
- [x] Add local JSON serialization/measurement metadata and disabled-null export checks in the existing test module. Test a counter above JavaScript's safe integer range and exclusion of configuration/private values.
- [x] Run approved focused NAPI/provider tests and strict touched-surface Clippy, then the pure Node suite and syntax gates. Do not execute native backing qualification.
- [x] Verify raw ten-row names/claims/metadata snapshots are unchanged; protected eleven hashes and all qualification floors/gates are unchanged.
- [ ] Freeze exact owned-source and log hashes, request independent read-only review, and send root the reviewable batch. Root owns staging, commit, PR and any later runtime authorization.

## Qualification limits

New rows measure adapter-local work, not CPU-exclusive time, allocator totals, HTTP requests, wire bytes or physical device IOPS. Nested and concurrent wall durations overlap. Copy-byte counters measure actual adapter copies, not network traffic. Local counters use independent atomic reads and require quiescent phase boundaries. Existing backing-marker/preflight and unregistered Rust-factory export exclusions remain unchanged. No capacity, compression, cross-host durability or formal-proof result follows from these controls.

## Execution record

- Node optional-local delta regression first failed because local evidence was discarded; the nine-group diagnostics-only script then passed.
- Pilot projection regression first failed because the local sibling was discarded; the 34-test pilot suite then passed.
- Existing phase-log regression first failed because local totals were absent; the nine-group diagnostics-only script then passed with bounded seven-row totals and explicit unavailable instance counts.
- Full benchmark unit script passed under Node 24.18.0 with the explicit capture binding; the observer/transport/pilot/entry/checker suite passed all 140 tests. Four touched Node syntax gates and scoped diff check passed.
- Independent Node review found that processed logger/pilot phases initially checked local rows without measurement provenance. Missing/tampered metadata regressions failed before the fix, then passed with exact separate local metadata validation. Positive models now exercise all seven rows, copy/65-byte ID rules, follower error/cancellation, and valid-endpoint input/max resets.
- Root explicitly released the startup/cache Rust and Cargo lease. The existing warmed `/private/tmp/mount-rs-cache-rss-retirement-cargo-target-20260927` target is used through `./scripts/cargo-shared` with jobs1 and `--locked`.
- Actual held-six upload assertions failed first for the missing local bank, then for zero digest calls before boundary wiring. The control now observes six digests/encodings, one payload/cache copy, five follower waits and one independent backing write.
- Formatted-source provider units passed 52 tests (one isolated enablement control excluded). Per-step cold/hot/migration copies, legacy migration, integrity rejection, failed uploads, cancellation, poisoned cache fallback and a held real cache mutex are covered. The held mutex asserts an active local gauge before release, completed outcome/histogram and nonzero inclusive elapsed/max afterward, without an exact duration threshold.
- The allocation harness intentionally exposed 20 test error-string allocations in its first added error-path fixture. Replacing only that allocating setup with a generic unit error gives zero enabled/disabled recorder allocations and zero fixed raw/local snapshot allocations, with a positive heap control. Enabled bank construction remains one allocation; the complete RawState is 5624 bytes, its local sub-bank 2256 bytes and LocalSpan 32 bytes on this validation target. Existing provider, future, public stats and JSON allocations are outside that recorder claim.
- The disabled clock closure is called zero times, with an enabled one-call positive control. Independent local saturation leaves raw qualification flags unchanged.
- The actual NAPI constructor/export assertion failed before local serialization. Formatted-source NAPI default units passed 39 tests (four isolated profile controls excluded); isolated enabled and disabled R2 export controls each passed. Full local metadata matches the Node contract, seven rows serialize exact decimal strings, and actual local input/output/histogram values above the JavaScript safe integer range remain exact. Constructors make no backing service calls.
- The retained Rust JSON line passes the real pure Node local-delta validator with exact metadata and seven observed rows. This cross-language check reads a constructor-only export; it executes no addon or backing workload.
- Strict provider/NAPI/CLI all-target Clippy with warnings denied, touched Rust formatting checks and scoped diff checks passed. Final immutable review receipts are being completed. Root owns the fresh joined Node suite after the independent backing-observer source freeze. No local native/backing/controller qualification, throughput comparison or capacity result is claimed.
