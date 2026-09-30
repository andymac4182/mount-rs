# Persisted compact layout evidence

> **For agentic workers:** Use the existing executing-plans workflow, with separate native implementation and independent review. Root owns benchmark integration; the native worker owns `lib.rs` and declarations.

**Goal:** Require provider-backed MRC5 evidence before a benchmark can claim the compact layout, then use verified fresh reopen behavior to support controlled format comparisons.

**Architecture:** `Filesystem.inspectCompactLayout()` reads the already selected metadata handle and verifies the same block authority. It uses the existing unmounted delegation gate and a temporary filesystem clone. The generic benchmark validates the fixed receipt outside timed workloads and performs normal cleanup even when the receipt is unavailable or malformed.

**Tech stack:** Rust, existing N-API/SQLite and dynamic-store contracts, Node 24, existing storage benchmark.

## Constraints and evidence scope

- Existing eleven dirty files and Cargo.lock remain unchanged; no dependency, protocol, authorization or metadata format change.
- No new provider open, callback, registry or hot-path instrumentation.
- Inspector return is `JsCompactLayoutReceipt | null`: null means a recognized non-MRC5 mode, which can include legacy, MRC2 or MRC4. It never proves an exact legacy format. Unsupported providers and closed facades reject.
- Receipt has exactly `schema`, `marker`, `backingId`, `structuralGeneration`, `blockAuthorityVerified`. Schema is `mount-rs.compact-layout-receipt.v1`, marker is `MRC5`, backingId is a nonzero 32-character lowercase opaque hex identity, generation is a canonical decimal u64 string, authority flag is true.
- Identity is not a content hash. Query and verification are separate observations; this is not an atomic namespace snapshot or proof of crash/power-loss durability.
- Provider failures retain the fixed errno code and a fixed message, without raw SQL, URIs, credentials or arbitrary provider strings.
- Inspection/shutdown serialization and cancellation must release temporary ownership; post-shutdown inspection cannot issue backing work.
- Actual persisted SQLite tests own same-handle/reopen evidence. Forwarding fixtures own boundary/race/error behavior and are labeled modeled. Existing provider corruption tests own real marker damage; no new SQL mutation API is added.
- Existing lifecycle counts, timeouts, chunk/payload settings, full-byte checks, performance floors, resource floors, observer caps and cleanup policy stay unchanged.
- No current measured legacy result becomes MRC5 evidence. Existing owned pilot/checker remain legacy until a separately reviewed paired runner exists.

## Task 1 — native inspection seam

**Files:** `bindings/mount-rs-napi/src/lib.rs`, `bindings/mount-rs-napi/index.d.ts`.

- [x] Write failing native tests: actual Unix SQLite MRC5 receipt and fresh reopen/full bytes; null for recognized noncompact modes; unsupported facade and post-shutdown refusal; same-block verification failure and error redaction; blocked query/shutdown order and cancellation without retained handles.
- [x] Run bounded NAPI RED through `scripts/cargo-shared` with explicit isolated target, jobs1 and locked dependencies; retain actual output.
- [x] Add the fixed NAPI receipt and method. Acquire `unmounted_delegation_gate()`, clone via `delegated_filesystem()`, query `metadata_store().compact_inode_mode_state()`, return null for None, then verify `block_store().verify_concurrent_backing(state.backing)` before returning Some.
- [x] Run NAPI tests, strict touched-surface Clippy and touched formatting. Freeze source/log hashes and release source/Cargo leases before review. Working-tree evidence is not clean build qualification.

## Task 2 — benchmark and JavaScript evidence

**Files:** new `benchmarks/storage/compact-layout.mjs` and `.test.mjs`; `benchmarks/storage/runner.mjs`, `test.mjs`; `bindings/mount-rs-napi/postlude.cjs`; new `bindings/mount-rs-napi/test/compact-layout.mjs`; CI selection and profiling guide.

- [x] Write and observe a pure RED control that rejects missing/null/forged receipts for requested compact, requires exact fields and precise u64 generation, and preserves fixed redacted errors.
- [x] Add validation returning a fresh frozen fixed receipt, with no input extras or source objects retained. API absence, query failure and invalid data cannot become measured zero or constructor proof.
- [x] Wrap the new async method in the existing error postlude. Verify actual decoded native errno in the native integration test.
- [x] Inspect only after constructor acceptance and inside the existing path/resource cleanup region. Failed proof produces a failed benchmark result without issuing timed workload calls; existing cleanup still runs. Preserve selection evidence separately from stored-mode evidence.
- [x] Update the existing compact artifact control and add fake-provider cleanup/failure controls. These controls do not prove native storage or persistent markers.
- [x] Exercise native Unix SQLite with a payload crossing a 64KiB boundary, complete-byte equality before/after shutdown/reopen, stable backing identity and generation after reopen, sentinel unlink and a second fresh empty reopen. Windows owns unsupported inspection/construction controls only.
- [x] Run pure Node 24 controls, syntax/diff checks and declarations; select the native control in existing CI Node lanes and retain exact test/source/binary scope.
- [ ] Publish the independently reviewed frozen scope through the existing PR with protected hashes checked; keep the full goal active.

## Next qualification — separately gated

Once actual persisted/reopen inspection is verified, prepare a separate paired legacy/MRC5 runner with the same eight owned containers, source/addon identities and immutable workload settings, two fresh distinct namespaces and confirmed cleanup between arms. Keep the existing owned pilot and verifier unchanged. Counterbalance order across retained pairs before a performance-win claim; failed floors and unavailable backing counters stay failed/incomplete.

Full 10-server/10000-client qualification, external issuer/native mounting, complete formal proofs, cache failure qualification and physical backing IOPS remain independent requirements of the active goal.

## Local validation retained

- Rust: four inspector tests; 43 NAPI library tests passed with four existing ignored probes; strict package Clippy and scoped formatting passed.
- Node 24: 34 pure controls passed, existing benchmark harness and declarations passed. Actual macOS SQLite native control passed full bytes across a 64KiB boundary, explicit shutdown, fresh same-authority reopen and EOF, unlink and second empty reopen. The rebuilt native artifact was hashed before/after and observed in the exact module cache.
- Actual compact SQLite benchmark integration: two verified lifecycle iterations, six acknowledged operations, no failures or timeouts, normal cleanup, persisted five-field receipt retained. This smoke run is not throughput qualification.
- Release Cargo compilation succeeded; the launcher packaging step returned EPERM. The actual compiled dylib was installed with the equivalent bounded binary copy and its hash matched the loaded addon. This is working-tree evidence; a clean CI build and Windows execution remain separate gates.
- Independent review corrected the Windows legacy inspection expectation to ENOTSUP. Null remains a supported-provider non-MRC5 observation, not legacy certification.
- Pending inspection preserves the configured deadline failure and records deferred teardown. Later settlement after return does not schedule automatic cleanup.
