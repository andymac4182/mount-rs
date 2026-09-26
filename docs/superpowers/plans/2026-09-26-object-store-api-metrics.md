# Object-store API metrics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Measure actual object-store method and body invocations, coalesced put claims, outcomes and bytes through native v2 and phase-aware Node artifacts.

**Architecture:** A fixed `Option<Box<RawState>>` bank belongs to the existing shared per-store stats state. Optional stack spans time actual adapter awaits and close exactly once on completion/drop. Native export serializes exact decimal counters and Node checks endpoint and delta invariants before any opt-in workload completeness gate.

**Tech Stack:** Existing Rust atomics/object_store/Tokio, serde_json/N-API, Node 24.

## Global Constraints

- Scope follows `/private/tmp/mount-rs-object-store-api-metrics-task-brief-20260926.md` SHA256 `11fe61db94dc809547cf90501c07bb17c9553e8b0209344d14a655614b51ad3d` and its root decisions; those supersede inline-bank/live-load/global-recorder appendix wording.
- Sole serialized source/Cargo/native lease; only `scripts/cargo-shared` and `CARGO_TARGET_DIR=/private/tmp/mount-rs-public-compact-selection-cargo-target`. No external service/network/fixture calls, new dependency, protocol/storage/cache/retry/acknowledgment/floor changes, commits or pushes.
- Preserve protected11 exactly and exclude frozen runner17/root plan and `docs/remote-client-scaling.md`, `docs/remote-production-qualification.md`, core diagnostics/profile.
- `raw_api=None` means disabled/unavailable; enabled construction alone allocates one boxed bank. No added disabled timer, per-operation allocation, forwarding box or heap counter updates. Existing provider futures/preparation/cache and logical timers remain; disclose future-size/startup/snapshot costs.
- Fixed names: `put_opts.block_create`, `get.block_read`, `body_read.block_read`, `get.conflict_verify`, `body_read.conflict_verify`, `get.migration`, `body_read.migration`, `head.direct_delete`, `delete.direct`, `delete.reconcile`.
- Raw schema `mount-rs.object-store-api.v1`, scope `one_object_store_block_store_instance`; native schema `mount-rs.storage-diagnostics.v2`. Counts/time/bytes and instance IDs export exact decimal strings; no private or dynamic labels.
- Method/body calls and claims start when polled, close once on completion/drop; terminal outcome/histogram/bytes precede in-flight decrement. Raw NotFound delete remains error even if logical reconcile succeeds. Lost-reply error confirms zero bytes and causes no replay. Surviving followers report error on cancelled leader; only dropped followers report cancellation.
- Latency maximum is cumulative and never subtracted. Exact phase maximum is unavailable. Saturation/overflow is explicitly incomplete. Quiescence is an external precondition and snapshots are not multi-atomic transactions.
- Exclude all backing-marker/concurrent-prefix/qualification/preflight calls (also through `blocks.store`), uninstrumented reconcile list/stream, internal HTTP/client retries, physical IOPS. Native registry covers live registered split R2 stores only.
- Preserve failures with bounded whitelisted sanitized before/after observations; workload identity is stable/nonempty for known split R2 provider IDs. Setup may open stores; shutdown disappearance stays incomplete but throughput gate requires workload phases.

### Task 1: Observable export RED and held fake API oracle

**Files:** Modify `bindings/mount-rs-napi/src/lib.rs`; create `providers/mount-rs-object-store-blocks/src/raw_metrics_tests.rs`.

- [x] Add isolated ignored test constructing `build_block_store` with existing R2 configuration, no I/O, then `assert!(snapshot["r2"]["instances"][0]["raw_api"].is_object())`.
- [x] Run `MOUNT_RS_PROFILE_IO=1 CARGO_TARGET_DIR=/private/tmp/mount-rs-public-compact-selection-cargo-target scripts/cargo-shared test --locked -p mount-rs-napi tests::live_r2_raw_diagnostics_require_exact_fields -- --ignored --exact`; preserve expected missing-field assertion, source and executable hashes.
- [x] Add independent held fake ObjectStore counts and deterministic first-poll follower establishment. Assert one raw put, N logical puts, N-1 followers, conflict get/body bytes, body fault after successful get, committed-lost-reply/no replay, cancellation, direct/migration/delete and reconcile NotFound semantics.

### Task 2: Fixed per-store raw bank and export

**Files:** Modify object-store `lib.rs`; create focused `raw_metrics.rs`; modify N-API `lib.rs`.

**Interfaces:** `ObjectStoreBlockStoreStats.raw_api: Option<RawApiSnapshot>`; rows carry calls/success/error/cancelled/elapsed_ns/latency_max_ns/attempted_bytes/confirmed_bytes/returned_bytes/32 buckets. Snapshot includes in_flight, pending_claims, fixed claim outcomes, saturated flag and fixed schema/scope.

- [x] Implement optional borrowed stack `RawSpan` and `ClaimSpan`; use `fetch_update` with `checked_add`, sticky saturation and fixed rows. Prepare payload/path before timer. Wrap existing awaits without forwarding boxes or semantic changes.
- [x] Export raw rows/claims with fixed privacy/scope/byte/latency coverage metadata and native v2 strings.
- [x] Run isolated enabled export GREEN and isolated disabled public-constructor test; focused fake tests and meaningful allocator positive/control tests. Record exact command handles/results.

### Task 3: Node phase validation and opt-in gate

**Files:** Modify `benchmarks/storage/diagnostics.mjs`, `benchmarks/storage/test.mjs`, `scripts/verify-w26-ozone-iops-artifact.mjs` and existing focused verifier tests.

- [x] Write Node RED fixtures for v2/raw success plus missing/malformed/nondecimal/reset/duplicate/dropped/new-work instances/fixed order/outcome/histogram/nonzero in-flight/saturation failures, and opt-in artifact missing workload evidence.
- [x] Retain schema/provenance/coverage, exact additive deltas and cumulative start/end maxima; `finishPhase(name)` enforces phase-aware identities. Keep bounded known-only integer/name/metadata observations on failure with truncation indicator.
- [x] Gate every expected size/result for known split R2 IDs, validating metadata, stable nonempty instances, raw endpoints/delta invariants and quiescence independently of complete=true. Keep non-R2/default behavior and 1000 ops/s floor.
- [x] Run Node 24 storage/verifier tests and applicable typecheck.

### Task 4: Verification and immutable review handoff

**Files:** Owned plan and `docs/remote-io-amplification.md`; evidence under `/private/tmp/mount-rs-object-store-api-metrics-evidence-20260926`.

- [x] Run scoped formatting, object-store/R2/touched N-API focused tests and strict all-target Clippy through sole target; no duplicate commands while a handle is active.
- [x] Document scope, byte/max semantics, unavailable fields and allocations. Freeze final owned hashes/diff, all RED/GREEN receipts/logs, source/protected continuity and cleanup/resource evidence.
- [ ] Root independent SPEC and QUALITY acceptance remains pending at frozen handoff. The owner supplies report/diff/evidence, then fixes/reverifies review findings until accepted and explicitly releases the sole lease with no remaining process handles. No commit/push; capacity/formal/cache/denied-FDB-RED/merge gates remain open.

## Verified handoff checkpoint

- Actual behavioral NAPI JSON RED failed for missing enabled `raw_api` at original HEAD `8f725872`; the source/binary receipt is retained. The later missing-lifetime helper compilation error and invalid missing-ID test failure are retained separately and are not relabeled as causal REDs.
- Bound HEAD transitioned once to `ff30ddc90d2014c24e98d723a71844726df60861` after root committed accepted unrelated runner18; protected11 remained exact. Final object-store lib47 pass/1 ignored, NAPI default39 pass/3 ignored and isolated export1 pass, R2 unit13 and existing mock-HTTP3 pass. Isolated real-constructor enabled/disabled controls pass.
- Strict scoped all-target Clippy, touched rustfmt/diff checks and Node24 NAPI static typecheck pass. Node synthetic RED/GREEN, final syntax, full JS capture-mock suite and exact actual native v2 JSON consumed by Node phase/gate pass. Actual encoding control constructs/registers R2 and performs zero backing calls; nonzero backing semantics use independent held fake stores.
- Observational control reports one enabled-bank startup allocation for3368 bytes, positive allocator1, enabled/disabled recorder update0, fixed raw snapshot0. RawSpan32 bytes/ClaimSpan24 bytes; before/after future frame growth is unavailable and may affect disabled future frames. Full provider operations and native/Node JSON are outside this zero-update-allocation statement.
- Immutable evidence/report/diff/source manifests live in `/private/tmp/mount-rs-object-store-api-metrics-evidence-20260926`. No commits/pushes, external services, live workloads, dependency or semantic/performance-floor changes by this owner. Final acceptance and all original capacity/formal/cache/denied-FDB-RED/merge gates remain open.
