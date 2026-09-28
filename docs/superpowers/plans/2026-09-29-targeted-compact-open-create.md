# Targeted compact Open(create)

## Objective

Remove complete persisted-guard reads from clean root-child exclusive regular-file
creation through the existing filesystem Open path. The production-target workload
uses this path (`/{name}`, `wx+`); its namespace population is currently incomplete
at 100 drives with 1,000 files each. Preserve the complete production target and
measure the same workload after the change.

## Contract

- Compact mode remains explicitly enrolled. No format, transport, authorization,
  backing identity, durability, or mounting configuration changes.
- Capture a `FileCreate` intent from a validated structural witness, cached graph,
  and a freshly selected parent. Verify parent body and physical identity against
  the cached pair before capture. Keep complete candidate graph validation.
- Allocate exactly one new regular inode, advance allocation and generation once,
  append one absent name, and change only the parent and new inode. Defaults and
  unrelated cached nodes remain unchanged.
- The transaction locks existing authority and reads the current anchor and affected
  parent. Require exact parent identity **and body** before overwriting it. A valid
  changed parent with an unchanged identity is corruption, not permission to publish.
- Acknowledged receipts contain exactly the parent and new inode. Install through a
  distinct path that retains unrelated selected body/physical identity pairs and
  pending atimes, and reprojects logical revisions into the new generation.
- Proven conflicts require a coherent complete refresh before recapture. Unknown
  publication, invalid receipts, flush failures, or cancellation after publication
  starts poison the coordinator and cannot replay the create.
- Other Open shapes retain their existing paths. Complete reopen and Full structural
  operations continue auditing complete persisted membership and bodies. A targeted
  create intentionally does not audit unrelated persisted guards.

## Verification

- [x] Existing-API behavioral RED: valid altered parent permissions and renamed
  preexisting entry with unchanged identity were both accepted before the fix.
- [x] Filesystem behavioral RED: an ordinary clean `wx+` loaded one Full snapshot
  after real handle I/O, duplicate rejection, shutdown and fresh byte verification.
- [x] Core constructor and exact transaction parent-body validation.
- [x] Filesystem dispatch and preserving receipt installation.
- [x] Core positive and malformed/stale/overflow controls.
- [x] Real SQLite sibling-count, selected-overlay, atime, structural conflict,
  uncertain commit, cancellation, and fresh reopen controls.
- [x] Actual TiDB FileCreate lost COMMIT acknowledgment, exact fresh snapshot,
  one transaction/commit and no replay, with explicit proxy cleanup.
- [ ] Touched formatting, strict Clippy, compact and broader chunked regressions,
  independent source review, clean committed source and end-to-end control.
- [ ] Same-geometry TiDB/RustFS measurement, complete counters and fresh oracles.

Local gates passed 31 compact core tests, 39 compact filesystem integration tests,
and 321 tests across the complete chunked/SQLite targets. The broader run retained
31 ignored cases; those are not counted as passed. Formatting, strict Clippy for
core/chunked/TiDB, and compilation of the actual TiDB test targets passed.
Independent source review accepted the scoped change. The actual owned TiDB
compact and ambiguous-commit suites passed all 22 cases with I/O profiling enabled.
The original eight-container fixture passed identity and health checks before and
after execution. These qualify the provider contracts and query-count controls;
committed-source end-to-end checks and performance qualification remain separate
gates.

## Evidence limits

The intended clean-create guard read count changes from two complete guard sets
to two parent guards: one selected preparation read and one locked publication
read. This is a source-level prediction until actual provider measurements prove
it. Anchor membership, directory entry arrays, cached candidate cloning and graph
validation still grow with namespace size; this change does not establish zero
allocations, device saturation, physical IOPS, or 10,000-client capacity.

Root owns Cargo, fixtures, runtime settlement, Git and the private evidence ledger.
Source workers own disjoint core, filesystem and TiDB test files. The immutable
bounded runner retains the previous ownership, source-freeze, deadline and output
limits. The full thread goal remains active until the original qualification and
delivery requirements are met.
