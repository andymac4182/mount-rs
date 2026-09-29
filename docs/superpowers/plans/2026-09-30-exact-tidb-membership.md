# Exact TiDB Membership Implementation Plan

Root owns source, shared Cargo and owned fixture execution. Independent agents
review invariants and diagnostics in disjoint files.

**Goal:** Reduce returned membership rows while proving the same complete fresh
set and measuring whole-publication latency and Rust allocation traffic.

**Architecture:** A constant three-parameter BETWEEN statement proves exact
contiguous-set equality under the existing authority lock. Sparse layouts and
mismatches retain enumeration. Parent proofs and publication validation remain.
The earlier IN-list candidate was rejected after a measured F1000 regression.

## Completed evidence

- [x] An actual old-query transfer control failed as expected before implementation.
- [x] The first candidate passed all 54 actual compact controls.
- [x] Eight alternating paired trials measured both implementations with 4
  warmups/32 samples per 4-file and 1,000-file case; each created state had a fresh
  complete oracle before reset. The IN-list regression is retained.
- [x] Audit all production diagnostic submissions; align Rust/JS declarations
  and native goldens; add source-drift and stale-inventory controls.

## Constant-range qualification

- [x] Require positive contiguous signed IDs; test gaps, duplicate/invalid IDs,
  signed overflow, exact count theorem and conservative packet refusal.
- [x] Borrow the sealed base only after fresh exact equality; enumerate every
  Full audit and every fallback, preserving error precedence.
- [x] Run fresh enabled TiDB/NAPI tests and strict all-target Clippy.
- [x] Run actual compact corruption, lock-wait, rollback, lost-ACK, packet and
  both dense/sparse equal-count substitution controls.
- [x] Independently review exact source and actual receipts.
- [x] Repeat paired latency/allocation/returned-row measurements with the same
  fixed membership schedule and unchanged deadlines.
- [x] Prepare the qualified report, source-bound raw samples and local gate
  receipts for PR 34.
- [ ] Qualify hosted CI, full formal and the production target before merge.

Use Rust 1.95 and `scripts/cargo-shared` with
`CARGO_TARGET_DIR=/private/tmp/mount-rs-server-cache-budget-cargo-target`.
Actual tests require the existing pinned disposable TiDB configuration and a
unique owned prefix; keep execution serial with explicit whole-process bounds.
Microbenchmark timing excludes seed, pure capture, proxy probe, snapshots,
Full resets and cleanup. Allocation instrumentation includes current-thread
Rust driver/runtime requests and excludes server/foreign-thread allocations.
This is not a blob, QUIC, sustained-throughput or physical-IOPS qualification.
