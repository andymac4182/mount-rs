# Compact local metadata instrumentation

The user asked to add metrics and logging before further architecture changes.
Existing service, backend, process and amplification measurements remain in
place. This follow-on measurement change exposes local work previously hidden
inside inclusive filesystem spans.

## Scope

- Preserve the first 114 core rows; append four fixed rows documented in
  `docs/bottleneck-metrics.md`. Storage inventory remains 85 rows.
- Time compact namespace materialization, write/unlink batch candidate copies
  and structural delta capture with existing stack spans and O(1) node lengths.
- Count expected physical guards only for successful delta captures.
- Record the existing filesystem snapshot span in the compact branch, including
  pending atime folding. Preserve existing validation and mutation ordering.
- Reuse the existing opt-in bounded slow logger; no per-file labels, request
  log allocation, new traversal, format, dependency or durability change.
- Update closed benchmark inventories together. Historical 114-row snapshots
  remain partial evidence and cannot satisfy exact current pilot qualification.

## Verification

The persisted SQLite create control reproduced the missing measurement after
full bytes, fresh reopen and EOF checks: 11 controls passed, then the compact
control failed because `filesystem.snapshot_nodes` recorded zero create calls.
Receipt: `/private/tmp/mount-rs-current-create-causal-20260927-2xwb8plr/receipt.json`.

Run that real operation control after instrumentation; extend the warmed actual
recorder allocation gate to all 71 appended causal rows and preserve the old
prefix. Verify consumer precision, absent historical rows and exact inventory
rejection. Run touched Rust regressions, formatting and strict Clippy. Build a
fresh native addon and use unchanged lifecycle dimensions with profiling on
and off. Retain source identity, full payload verification and original floor
failures. Single runs establish descriptive observations, not causal speedups.
