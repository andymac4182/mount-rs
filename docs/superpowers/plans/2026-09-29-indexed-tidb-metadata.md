# Indexed TiDB Metadata Implementation Plan

> **For agentic workers:** Use subagent-driven-development for isolated tasks,
> with root-owned Cargo, Git, fixture and integration execution.

**Goal:** Ship normalized TiDB membership/directory storage and bounded coherent
selected reads and direct-root file Open, retaining durability and concurrency.

**Architecture:** Keep complete logical compact snapshots for graph audits and
existing structural validators. Add scoped authority/file/entry receipts for
point reads; normalize TiDB persistence behind explicit format and capability
boundaries. Adapt filesystem/SDK dispatch without changing local mounts or wire.

**Tech Stack:** Rust 1.95, mysql_async 0.37.1, existing TiDB/PD/TiKV provider,
serde and SHA-256 for name indexing.

## Global constraints

- Stop capacity benchmarks; correctness/query-shape/fault checks are gates.
- Root owns Cargo/Git/runtime/fixture/configuration/ledger operations.
- Use `scripts/cargo-shared`, explicit shared temporary target and locked/offline
  builds where possible. Preserve unrelated work and all retained evidence.
- No fabricated one-member anchor or empty-directory complete-proof receipt.
- Exact name equality, original enumeration order, atomic mutations, physical
  CAS, guard-first selected writes, fresh post-wait authority, durable ACK and
  unknown-commit non-replay are mandatory.
- Other providers retain existing behavior through Unsupported point defaults.

## Task 1: Scoped core contracts

Files: new `src/storage/compact/indexed.rs`, `src/storage/compact.rs`,
`src/storage.rs`, focused core tests.

- [x] Add small authority and directory-header DTOs, with validation and exact
  projection/reconstruction of complete logical snapshots using actual members.
- [x] Add `CompactPointReadCapability`, borrowed file expectation and scoped
  file/root-entry receipts. Receipts expose generation first; missing/malformed
  selected outcomes are deferred until it is checked. Root-entry receipt binds
  exact requested name/candidate IDs and typed directory/header/file groups.
- [x] Add optional `MetadataStore::compact_point_read_capability`,
  `read_compact_file`, and `read_compact_root_entry` defaults returning Enotsup.
- [x] Test backing/authority precedence, sorted actual members/counts, all
  physical identity and body contradictions, missing groups, generation changes,
  root/header/entry scope and unchanged file comparison. Observe RED before
  implementing; run core gates; obtain independent contract review.

## Task 2: TiDB normalized persistence

Files: new `providers/mount-rs-tidb/src/compact/indexed.rs`, existing `compact.rs`,
`storage.rs`, provider tests; dependency declaration for serde if needed.

- [x] Add/validate members and dentries schemas including exact indexed columns;
  keep guard physical identity columns. Introduce strict authority/directory
  codecs and tagged enrollment, rejecting mixed/old physical formats.
- [x] Write codec/directory-plan tests first. Plans retain old ordinals for
  surviving order, append new names, delete only removed names, use exact names
  within hash buckets, and explicitly reconstruct reordered Full directories.
- [x] Reconstruct full snapshots/affected guards under one existing transaction;
  validate exact keyspace and all counts/references before returning them.
- [x] Publish membership/dentry diffs and small headers atomically; perform
  complete preflight before DML. Preserve rollback and unknown-COMMIT behavior.
- [x] Implement one-statement joined file and root-entry point reads, typed NULL
  groups, verified autocommit, authority-first decoding and scoped receipts.
- [x] Selected publication reads small authority + selected membership after its
  guard lock and validates the same per-file transition without member ranges.
- [x] Run focused provider/actual-TiDB correctness and fault controls; review the
  complete codec/SQL transaction diff before integration acceptance.

## Task 3: SDK and filesystem dispatch

Files: `crates/mount-rs-sdk/src/stores.rs`,
`filesystems/mount-rs-chunked/src/lib.rs`, new scoped-read module, focused tests.

- [x] Forward point capability/methods through the erased SDK adapter with
  existing finite span attribution.
- [x] Prefer point file reads for cached regular files; check generation before
  absence/body handling; retain existing admission checks and orphan overlay.
- [x] Extract one shared selected-read admission helper to avoid duplicating
  incarnation/revision/equal-identity body/local-revision logic.
- [x] Use one point root-entry request for eligible existing ordinary Open.
  Verify actual selected link/root-header against audited cache and admit its
  file under the captured local revision; retain all excluded-path fallbacks.
- [x] Test capable dispatch, Unsupported fallback, changed-generation/missing
  rows, same-generation contradiction, pending local revisions, sparse/truncate
  and open-unlinked behavior. Run affected core/filesystem/SDK/provider suites.

## Task 4: Ship through PR

- [x] Run formatting, strict affected Clippy and focused suites, plus owned actual
  TiDB integration/query-shape/fault controls. No throughput trial is requested.
- [x] Obtain independent whole-change review and resolve material findings.
- [ ] Commit/push `codex/tidb-indexed-metadata`; open/attach a PR based on PR33,
  with exact test results and source/proof/platform qualification limits.
- [ ] Inspect required CI and stack state; merge only when required gates are
  proved. Existing restricted raw-log diagnosis is not bypassed.


## Verification evidence

- Affected core/chunked/TiDB/SDK libraries and integration targets: 567 passed,
  zero failed, 140 explicitly ignored (native/external/profiling opt-ins).
- Strict Clippy: all targets of core, chunked, TiDB, SDK and NAPI passed.
- Native seven-family presence controls: 8 passed. Closed JS presence/comparison
  controls: 110 passed. Shell syntax and patch whitespace checks passed.
- Core canonical authority and complete-file comparison passed its allocator
  positive-control contract with zero requested allocations in the measured
  comparison window. SQL buffers and owned fallback remain allocating paths.
- Actual TiDB gates cover normalized point SQL, full audits, ordering, guard
  contention, rollback, lost COMMIT, packet limits, schema rejection and retained
  markers. A separate actual TiDB + RustFS SDK test checks disjoint peer writes,
  every byte, append/rename/truncate, open-unlinked state, fresh reopen and cleanup.
- Independent review identified authority strictness/inode binding and retained
  marker routing gaps; regressions reproduced failures and fixes were reviewed.
- Capacity/throughput benchmarks remain stopped. No updated production capacity,
  physical IOPS, formal proof or native platform acceptance is inferred.
