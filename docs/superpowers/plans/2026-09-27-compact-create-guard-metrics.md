# Compact Create Guard Metrics Implementation Plan

> **For agentic workers:** Use subagent-driven-development to execute the independent controls and consumer contract work. Root owns production wiring and runtime leases.

**Goal:** Attribute prepared fresh-file rejection to each actual guard predicate before changing storage or retry policy.

**Architecture:** Append six fixed rows to the existing opt-in atomic core profile recorder. Observe the production fresh-create branch after successful path resolution, counting all three overlapping predicates before returning its existing outcome. Existing generic SDK, CLI and NAPI exports carry these rows.

**Tech Stack:** Rust, existing core profile atomics, SQLite public filesystem controls, Node 24 consumer controls.

## Global Constraints

- Base `f25ddaeef29c89e0589988f37c3d41e7ebe546fb`; branch `codex/compact-create-guard-metrics` in the existing causal checkout.
- Preserve the exact 108-row prefix and append six rows in the order below. Core bank and exact consumers become 114 rows; the projector retains its six additional accepted labels and becomes 120. The separate storage diagnostics bank stays at 85.
- Preserve path resolution, namespace changes, revision checks, retries, flushing, uncertain-commit behavior, response semantics and all three guard predicates.
- No dependencies, dynamic labels, paths, drive IDs, payloads, per-request allocations or clocks in the guard observer. Disabled profiling retains the existing recorder behavior.
- Guard evaluations may repeat after confirmed CAS losses. They are not final request outcomes or provider attempts. Passing this guard does not establish durable commit.
- Path resolution errors occur before evaluation. `path_present` refers to the resolved `entry.node` with final symlinks followed, not merely lexical directory-entry existence. A passed guard can still encounter inode overflow or later namespace/timestamp errors.
- Observe the mutation after existing batch remapping against its accumulated candidate. Same-revision fresh creates have already been remapped to the candidate's `next_inode`; allocation mismatch does not report their original captured inode. The eight direct-apply combinations qualify the guard itself, not batch remapping.
- The observed helper is shared by compact and legacy whole-file mutations. Counters are layout-independent even when the causal experiment targets compact storage.
- Predicate counters overlap. At quiescence `evaluated = passed + conflict`; each predicate count is at most `conflict`, but their sum may exceed it.
- Runtime tests and native runs use root-owned bounded commands, frozen source and independent review. Preserve the original checkout's protected 15 paths.
- Existing authorization covers this scoped metrics addition. Execute continuously; do not introduce another approval step.

## File Map and Contract

| File | Responsibility |
| --- | --- |
| `src/diagnostics/profile.rs` | Append the six concrete Event variants and fixed names. |
| `filesystems/mount-rs-chunked/src/causal_metrics.rs` | Stack-only `observe_create_guard(bool, bool, bool)`. |
| `filesystems/mount-rs-chunked/src/lib.rs` | Call the observer at the actual fresh-create guard, and register its private test module. |
| `filesystems/mount-rs-chunked/tests/compact_snapshot_revision.rs` | Assert guard deltas in existing full-content/reopen fresh-create and real-peer controls. |
| `filesystems/mount-rs-chunked/src/create_guard_metrics_tests.rs` | Exercise all eight predicate combinations through actual `apply_whole_file_mutation`. |
| `tests/filesystem_causal_profile_allocations.rs` | Expand the warmed primitive allocation contract from 61 to 67 causal events and 108 to 114 total rows. |
| `benchmarks/storage/owned-layout-metrics{,.test}.mjs` | Expand projector acceptance contract without losing its six extra labels. |
| `benchmarks/storage/owned-backing-pilot{,.test}.mjs` | Expand exact pilot profile contract and independent fixture. |
| `scripts/verify-owned-backing-pilot{,.test}.mjs` | Expand exact verifier contract and independent fixture. |
| `.github/workflows/ci.yml` | Execute the new ignored production guard control with profiling enabled and retain its log. |
| `docs/bottleneck-metrics.md` | Define rows and evidence limits; record actual measured predicate counts after native runs. |

Append these names in order, each with calls and units incremented by one and elapsed_ns zero:

```text
filesystem.mutation.create_guard.evaluated
filesystem.mutation.create_guard.passed
filesystem.mutation.create_guard.conflict
filesystem.mutation.create_guard.revision_mismatch
filesystem.mutation.create_guard.allocation_mismatch
filesystem.mutation.create_guard.path_present
```

## Task 1: Establish semantic RED

- [x] Add independent string-name assertions to both existing public controls. Fresh/no-peer expects deltas `[1,1,0,0,0,0]`; real peer selected-inode update expects `[1,0,1,1,0,0]`. Assert both calls and units after full-content EOF and fresh reopen prerequisites.
- [x] Add a private actual-production control for all eight boolean combinations. Construct an absent or present path and vary current revision and next_inode. Check `Committed` only for all-false, otherwise `Conflict`, and check candidate namespace unchanged on rejection. Use string metric lookup so the missing rows yield a semantic failure.
- [x] Execute the public ignored controls before production wiring. Accept only a missing-row assertion after real filesystem prerequisites, not compilation failure or fixture timeout.

```sh
MOUNT_RS_PROFILE_IO=1 MOUNT_RS_TRACE_STORAGE=0 MOUNT_RS_TRACE_REQUESTS=0 \
CARGO_TARGET_DIR=/private/tmp/mount-rs-causal-metrics-target-20260927 \
./scripts/cargo-shared test -p mount-rs-chunked --test compact_snapshot_revision \
--locked --offline -- --ignored --test-threads=1 --nocapture
```

## Task 2: Wire fixed observations

- [x] Append `FilesystemMutationCreateGuardEvaluated`, `Passed`, `Conflict`, `RevisionMismatch`, `AllocationMismatch`, `PathPresent` Event variants with the fixed names above.
- [x] Add this observer, using those concrete Event variants:

```rust
pub(super) fn observe_create_guard(revision_mismatch: bool, allocation_mismatch: bool, path_present: bool) {
    profile::add(Event::FilesystemMutationCreateGuardEvaluated, 1);
    let conflict = revision_mismatch || allocation_mismatch || path_present;
    profile::add(if conflict { Event::FilesystemMutationCreateGuardConflict } else { Event::FilesystemMutationCreateGuardPassed }, 1);
    if revision_mismatch { profile::add(Event::FilesystemMutationCreateGuardRevisionMismatch, 1); }
    if allocation_mismatch { profile::add(Event::FilesystemMutationCreateGuardAllocationMismatch, 1); }
    if path_present { profile::add(Event::FilesystemMutationCreateGuardPathPresent, 1); }
}
```

- [x] After the existing successful `walk` in `apply_whole_file_mutation`, compute the three pure predicates, observe them, and use their unchanged OR to return `Conflict`.
- [x] Append the same names in all three consumers and independent fixtures. Preserve unknown, duplicate, missing-row and prefix rejection controls.
- [x] Expand the warmed allocation control's concrete list to 67 events, total snapshot to 114, and observed delta rows to 67.

## Task 3: Verify and measure

- [x] Run public controls GREEN, actual-production eight-combination control, causal helper controls, warmed allocation gate and Node contract controls in isolated processes.
- [x] Run core/chunked libs, compact inode, concurrent CAS, concurrent inode, touched formatting and strict Clippy. Add an explicit profiling-enabled CI invocation for the new private control and retained log.

```sh
MOUNT_RS_PROFILE_IO=1 MOUNT_RS_TRACE_STORAGE=0 MOUNT_RS_TRACE_REQUESTS=0 \
CARGO_TARGET_DIR=/private/tmp/mount-rs-causal-metrics-target-20260927 \
./scripts/cargo-shared test -p mount-rs-chunked --lib --locked --offline -- \
--ignored --exact create_guard_metrics_tests::fresh_create_guard_records_overlapping_predicates \
--test-threads=1 --nocapture
```

- [x] Build a fresh native artifact, retain its exact hash and source join, and run the unchanged bounded SQLite workload (400 iterations, concurrency 64, 4096-byte payload, 65536-byte chunk, 1000 logical operations/sec floor). Preserve every actual result including floor failures. Confirm 400 full readbacks and all guard counter relationships before interpreting predicate attribution.
- [x] Obtain an independent source/contract review and an independent actual measurement audit. Both have no remaining material findings; the actual audit seals all 13 final gates, 119 Node controls, four native SQLite arms and the protected source checks.
- [ ] Commit only scoped files after passing local gates, create and attach a draft PR stacked on PR31, and report pending CI accurately. Retain the publication receipt outside the source plan after publication.

## Interpretation Limits

These counters reveal which guard predicates reject a candidate, not the unique writer or full request timeline. SQL statement counts, pager writes, logical operations and physical device IOPS remain distinct. Native local SQLite measurements do not qualify transport, distributed cache, mounted clients or the full production topology. No storage format or guard relaxation belongs to this addition.
