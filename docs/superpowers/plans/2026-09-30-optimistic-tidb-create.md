# Optimistic TiDB Create Implementation Plan

> **For agentic workers:** Use subagent-driven-development for independent
> test authoring and review; the root owns builds, fixture execution and Git.

**Goal:** Remove the common create's separate root proof transaction while retaining
complete fresh locked publication validation.

**Architecture:** Add an opt-in provider capability and an audited proposal
constructor. Successful cached preparation proceeds to the existing transaction;
cached errors and proven conflicts retain fresh preflight and Full refresh.

**Tech Stack:** Rust, async-trait, TiDB/MySQL, existing SQLite test provider,
source-bound TiDB + RustFS native qualification.

## Global constraints

- Use `scripts/cargo-shared`, Rust 1.95.0 and the explicit temporary target.
- Keep `validate_current`, transaction DML, packet checks and COMMIT semantics unchanged.
- Keep unknown outcomes/cancellation fail-closed and non-replayable.
- Preserve unsupported-provider and guarded-path fallbacks.
- Do not change metric inventories, thresholds or phase deadlines.

## Task 1: Audited proposal and opt-in routing

**Files:** `src/storage.rs`, `src/storage/compact.rs`,
`providers/mount-rs-tidb/src/storage.rs`, `crates/mount-rs-sdk/src/stores.rs`,
`filesystems/mount-rs-chunked/src/lib.rs`.

**Interfaces:** `MetadataStore::compact_optimistic_create_capability()` returns
`CompactOptimisticCreateCapability`; `CompactRootFileCreate::capture_audited`
accepts an opaque structure, expected parent identity, name, created file and
parent timestamps. It delegates the existing `capture_parent` validator.

- [x] Add RecordingMetadata regressions; observe all three fail on the old path.
- [x] Add default Unsupported / TiDB Supported capability and SDK forwarding.
- [x] Construct proposals only for absent unguarded root children.
- [x] Preserve fresh preflight for preparation errors and proven conflicts.
- [x] Verify clean path, same-generation refusal and lost acknowledgment controls.

## Task 2: Freshness and uncertainty regressions

**Files:** `src/storage/compact/tests.rs`,
`filesystems/mount-rs-chunked/tests/compact_inodes.rs`,
`providers/mount-rs-tidb/tests/support/indexed_compact.rs`, SDK adapter tests.

- [x] Add literal audited/loaded/verified proposal and full-snapshot parity oracle.
- [x] Add peer path changes, guarded errors and timestamp fallbacks.
- [x] Run cancellation/forged-receipt/lost-ack cases through both capability paths.
- [x] Author actual TiDB sibling name/hash and equal-count member substitution tests.
- [x] Run the final core, chunked, TiDB and SDK tests and strict Clippy.
- [x] Execute both new TiDB controls in a fresh owned prefix; retain exact scope
  and retirement evidence, and report dataset retention until absence is proven.

Commands (set the explicit target and Rust toolchain in the environment):

```sh
./scripts/cargo-shared test --locked --offline -p mount-rs-core -p mount-rs-chunked -p mount-rs-tidb -p mount-rs-sdk --lib --tests
./scripts/cargo-shared clippy --locked --offline -p mount-rs-core -p mount-rs-chunked -p mount-rs-tidb -p mount-rs-sdk --all-targets -- -D warnings
./scripts/cargo-shared fmt -p mount-rs-core -p mount-rs-chunked -p mount-rs-tidb -p mount-rs-sdk -- --check
```

## Task 3: Review, publication and performance qualification

- [x] Independently review production fences, SDK forwarding and persisted-row controls.
- [ ] Commit and push the tested change to PR #34; preserve earlier revision-bound results.
- [ ] Freeze a new native binary/source closure and compare matched population
  and churn profiles. Verify actual SQL/marker call reduction and full fresh oracles.
- [ ] Reattempt D100 only after the preceding correctness/performance gates pass.

No step above replaces the remaining distributed-cache/issuer/platform/formal
qualification, native cleanup/recovery work or the 10,000-client target.

## Validation evidence

Final tracked source manifest `ade66fb8a859d1dcfd8a91391d2b6c7aee8680f31542262ac42dcdce6df20a56`:
the broad command above passed 591 enabled tests across 49 suites; 153 opt-in
tests remained ignored. Strict Clippy covered all targets with SDK observability.
The retry-budget regression first reproduced EAGAIN on the old preparation
fallback, then passed with 126 proven publication noncommits and success on the
last attempt. Two independent source reviews accepted the final fences.

Both actual TiDB corruption controls passed in owned prefix
`optimistic-create-93aea53eb85f40bd83ee8df25984684c`. The tests require the fresh
locked SQL reads, refuse before DML/COMMIT, settle both providers and proxy, and
compare every raw table projection through an independent observer. Their
metadata is retained; dataset absence has not been proven. These controls do
not measure physical IOPS or establish production capacity.
