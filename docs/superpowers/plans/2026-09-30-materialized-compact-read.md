# Materialized Compact Read Implementation Plan

Root owns Cargo, measurements, fixture and delivery. The core worker exclusively
owns `src/storage/compact.rs` and its isolated integration controls. Root owns
TiDB provider wiring. Independent reviewers own no source.

- [x] Trace the actual Stat path and old comparator/fallback contract.
- [x] Record the materialized guard design under the existing allocation and
  metadata-refactor authorization.
- [x] Add isolated contract and allocation controls. Root runs the actual
  serialized baseline adapter and verifies a selected behavioral allocation RED;
  missing API compilation alone does not qualify that gate.
- [x] Implement exact typed match certification, retaining fallback and error
  semantics. Root runs zero-allocation and mutation controls.
- [x] Wire TiDB without changing SQL, physical validation or transaction order.
- [x] Run core/TiDB/chunked and affected enabled tests, actual compact faults,
  strict Clippy, formatting and whitespace checks.
- [x] Measure complete Open/Stat/Read/Close cycles with the existing 256-call
  ceiling and fresh oracles; retain any remaining failures.
- [x] Independently review source/results and update source-bound evidence.
- [ ] Resolve remaining allocation/CI/formal and native production qualification,
  deliver the changes to PR 34 and merge once its required gates pass.

Use Rust 1.95 and `scripts/cargo-shared` with
`CARGO_TARGET_DIR=/private/tmp/mount-rs-server-cache-budget-cargo-target`.

## Measured result

Eight source-bound paired trials at 128/1,000 files produced 128 timed samples
per role and size. At 1,000 files, mean allocation requests fell 1.99% and
requested bytes fell 20.67%; median latency increased 3.33%, with mixed mean/p95.
No latency or throughput win is established. Both original size controls still
fail the unchanged 256-call ceiling after fresh oracles and verified cleanup.
SQL remains seven inode reads returning 2,006 rows per 1,000-file cycle. Stat
accounts for 93.15% of the candidate's allocation requests.

The isolated typed comparator allocates nothing after row decoding. Complete
metadata operations still allocate. See
`docs/benchmarks/tidb-materialized-read-20260930/README.md` for qualification and
the remaining CI, formal, RustFS and production-scale gates.
