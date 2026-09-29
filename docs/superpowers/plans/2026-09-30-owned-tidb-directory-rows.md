# Owned TiDB Directory Rows Implementation Plan

> Agentic workers use the existing subagent workflow with disjoint file ownership.
> Root owns Cargo, fixture execution, measurements and publication.

**Goal:** Remove the extra complete directory-name copy while preserving fresh
validation and physical write planning.

**Architecture:** Consume SQL row names into logical entries. Retain ordinal
vectors for structural write planning and share the planner through borrowed
entry views.

**Tools:** Rust 1.95, `scripts/cargo-shared`, pinned actual TiDB 8.5.7.

## Constraints

- Keep complete fresh row validation and the existing core transaction validator.
- Preserve generic insertion, reorder, rename and unlink behavior.
- Preserve packet/workload/allocation ceilings and all process deadlines.
- No schema, protocol, dependency or core publication ownership changes.
- Use `CARGO_TARGET_DIR=/private/tmp/mount-rs-server-cache-budget-cargo-target`.

## Task 1: Move validated names and retain ordinals

Files: provider `src/compact/indexed.rs` and root-owned `src/compact.rs`.

- [x] Add a failing materialization control requiring retained String buffers
  with exact attributes, names and listing order; keep old inputs alive.
- [x] Root runs `./scripts/cargo-shared test --locked --release -p mount-rs-tidb
  --lib storage::compact::indexed::tests::materialize_directory_preserves_name_buffers_attributes_and_order
  -- --exact`; inspect
  the actual selected test name so filtering cannot qualify zero tests.
- [x] Change materialization to consume `Vec<StoredEntry>` and move names after
  complete physical validation.
- [x] Add `DirectoryPlan::new_from_materialized(ordinals: &[u64], old:
  &[DirectoryEntry], next_ordinal: u64, next: &[DirectoryEntry])`. Reject length
  mismatch and share the original planner using borrowed views.
- [x] Consume directory-row maps at the four provider callers. In structural
  publication collect ordinal vectors first; derive old names from `current`.
- [x] Run all enabled provider tests and actual compact controls, including
  equal-count substitutions, same-version parent corruption, lock waits,
  ordinal/name boundaries, rollback and unknown COMMIT.

## Task 2: Close selected-read test ownership

File: `tests/compact_selected_read_allocations.rs`.

- [x] Retain existing random exact-key ownership and retired actors.
- [x] Delete dentries, guards, members, inodes, metadata, blocks and block
  authority by bound volume key; require `SELECT COUNT(*)` returns `Some(0)`.
- [x] Run both actual ignored allocation controls with unchanged bounds and
  fresh full/byte oracles. Do not replay uncertain operations.
- [ ] Resolve the existing 256-call ceiling failure. Both control and candidate
  fail; phase counters isolate complete-root Stat as the sibling-scaled path.

## Task 3: Measure and deliver

- [x] Build source-bound control and candidate release artifacts; retain exact
  digests, identical instrumentation and shared fixture identity.
- [x] Run the existing eight-trial paired method and unchanged fresh oracles,
  deadlines and exact metadata cleanup.
- [x] Inspect allocation, requested bytes, returned rows and mixed latency;
  keep losses visible and exclude physical IOPS/cluster claims.
- [x] Run strict all-target provider Clippy, formatting and patch checks.
- [x] Refine wide string-slice lookup keys to thin borrowed String keys and
  repeat the qualified pairing. Final F1000 calls 8,111→7,111 and requested
  bytes 804,603→738,267; mean/median latency rose about 4.2%, p95 fell 4.1%.
- [x] Save the independently reviewed local report and complete evidence at
  `docs/benchmarks/tidb-directory-ownership-20260930/`.
- [x] Independently review final source/results and prepare the tested source and
  source-bound reports for PR 34.
- [ ] Continue exact-head CI/formal and production-capacity qualification before
  merging the aggregate goal.
