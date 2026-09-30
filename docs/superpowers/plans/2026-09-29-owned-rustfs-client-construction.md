# Owned RustFS Client Construction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task by task. Root owns all Cargo, native runtime, ledger and Git mutations; scoped authors do not run those operations.

**Goal:** Move measured synchronous RustFS client construction into bounded,
retained workers and preserve shutdown authority and cancellation behavior.

**Architecture:** A provider-owned construction context retains actual blocking
handles and one-consumer products. SDK context and CLI shutdown own its seal
and drain separately from backing authority. Native prefix initialization uses
the same retained context as its later SDK open.

**Tech Stack:** Rust 1.95, Tokio blocking tasks and notifications, existing
ConstructionObserver/ConstructionResource, object_store 0.12.

## Global constraints

- Follow `../specs/2026-09-29-owned-rustfs-client-construction-design.md`.
- No backing operation runs inside a construction worker; preserve all current
  signing, TLS, retry, prefix and durability options and all four client roles.
- Use `CARGO_TARGET_DIR=/private/tmp/mount-rs-public-compact-selection-cargo-target`,
  `CARGO_BUILD_JOBS=1`, and `./scripts/cargo-shared`; no checkout target directory.
- Keep the native geometry, freshness gate, deadlines and resource caps unchanged.
- Preserve unrelated source and private fixture credentials; no raw CI log access.

### Task 1: provider owner and deterministic lifecycle controls

**Files:**

- Create `providers/mount-rs-rustfs/src/client_construction.rs`.
- Create `providers/mount-rs-rustfs/src/client_construction_tests.rs`.
- Modify `providers/mount-rs-rustfs/src/lib.rs` and `Cargo.toml` only for exports,
  module wiring and Tokio `rt,sync,time` features.

**Interfaces:**

```rust
pub struct RustFsConstructionContext;
pub struct OwnedPrefixProbe;
impl RustFsConstructionContext {
    pub fn new(max_builds: usize) -> Result<Self>;
    pub fn seal_admission(&self) -> Result<()>;
    pub async fn close(&self) -> Result<()>;
    pub async fn block_store(&self, config: RustFsConfig, prefix: String,
        durable: bool, observer: Option<&dyn ConstructionObserver>)
        -> Result<RustFsBlockStore>;
    pub async fn owned_prefix_probe(&self, config: RustFsConfig, prefix: String,
        observer: Option<&dyn ConstructionObserver>) -> Result<OwnedPrefixProbe>;
}
impl OwnedPrefixProbe {
    pub async fn observe_owned_prefix_absence(&self) -> Result<bool>;
}
```

- [x] Observe the actual existing SDK constructor behavioral RED: the exact
  `providers::rustfs_construction_async_tests::rustfs_context_construction_yields_before_returning_without_backing_requests`
  test fails its assertion after successful construction and cleanup.
- [x] Write deterministic controls before implementing the new owner. Test
  factories return `RustFsBlockStore::new(Arc::new(InMemory::new()), "unit", false)`
  through the actual production reserve/spawn/join/claim path. Hold the factory
  with a Condvar; notify its entry before testing cancellation or heartbeat.
- [x] Implement the two closed recipes, retained handle, fixed owner waker,
  pending registration, single result transfer, actual disposal, capacity wait,
  synchronous seal, all-ticket drain and sticky quarantine from the design.
- [x] Run `./scripts/cargo-shared test -p mount-rs-rustfs --lib --locked --offline
  client_construction_tests:: -- --test-threads=1` under the owned runner. Verify
  every named control is observed, then run all existing provider tests.
- [x] Independently review the immutable diff and actual RED/GREEN evidence.

### Task 2: SDK context ownership

**Files:** Modify `crates/mount-rs-sdk/src/providers.rs`.

**Interfaces:** Consume Task 1. Produce:

```rust
pub fn seal_client_builds(&self) -> Result<()>;
pub async fn close_client_builds(&self) -> Result<()>;
pub async fn prepare_rustfs_owned_prefix_probe(&self, config: RustFsConfig,
    prefix: String, observer: Option<&dyn ConstructionObserver>)
    -> Result<OwnedPrefixProbe>;
```

- [x] Add a `RustFsConstructionContext::new(8)?` field to StorageContext.
- [x] Route only `open_blocks(..., Some(context), observer)` RustFS construction
  through `context.rustfs.block_store(config, prefix.clone(), *durable, observer).await?`.
- [x] Seal before direct close; poll constructor drain alongside every existing
  TiDB close before yielding. Collect errors without skipping any future.
- [x] Replace the baseline scheduling diagnostic with deterministic owner tests
  and an SDK wiring/control test; do not promise first-poll Pending for a worker
  that already finished. Preserve existing outer journal uncertainty controls.
- [x] Run `./scripts/cargo-shared test -p mount-rs-sdk --all-targets --locked
  --offline` and verify constructor, context close and journal tests by name.
- [x] Independently review the SDK diff and close-order evidence.

### Task 3: CLI shutdown and native prefix ownership

**Files:** Modify `apps/mount-rs-cli/src/remote_runtime.rs`,
`apps/mount-rs-cli/src/remote_runtime/lifecycle_tests.rs` and
`apps/mount-rs-cli/tests/ten_process_cache_support/cold_retirement.rs`.

**Interfaces:** Consume SDK constructor-only seal/drain and the provider probe.

- [x] Add a failing lifecycle control where pool/factory cleanup fails while a
  constructor remains held. Prove admission seal is synchronous and constructor
  drain completes before the existing authority barrier stops dependent closes.
- [x] Add `ClosePhase::ClientBuilds` after Factories. First request calls
  `context.seal_client_builds()`; this phase awaits only `close_client_builds()`.
- [x] Retain StorageContext before native prefix preparation and reuse it in
  `open_sdk`. The prepared probe runs the existing exact-prefix LIST afterward;
  a present namespace starts no probe worker. Keep fresh oracle contexts separate.
- [x] Retain a separate `RustFsConstructionContext::new(1)` for the fresh raw
  provider constructor and a fresh StorageContext for the public SDK oracle;
  pass its context explicitly to `open_sdk` at both call sites.
- [x] Run CLI lifecycle/provider tests and `ten_process_cache` unit controls with
  `local-oidc-fixture,io-profiling`; check named tests and real process cleanup.
- [x] Run touched-surface strict Clippy and `cargo fmt --all -- --check`.
- [x] Independently review the complete source diff, lockfile and protected files.

### Task 4: unchanged native qualification and delivery

**Files:** Retained external evidence and scoped source from Tasks 1–3.

- [ ] Rebuild actual native test/CLI binaries, bind their depinfo and source hashes.
- [ ] Review a new immutable launcher, refresh only the exact existing eight
  fixture services' read-only health, then launch within the existing freshness
  bound. Select `ten_process_tidb_rustfs_cold_retirement_qualification` unchanged.
- [ ] Inspect the first failure or complete receipt. Verify actual worker joins,
  complete source-bound observations, fresh oracle bytes and cleanup; record any
  incomplete credential cleanup truthfully.
- [ ] Append evidence to the owned goal ledger, independently review, then commit
  and push scoped verified source to the existing PR branch. No completion claim
  for the full goal until subsequent scale, cache, fallback and E2E gates pass.

## Recorded source verification

- Provider: 40 library tests and 12 integration tests passed; one platform
  construction profile remains ignored. All 17 owner lifecycle controls passed.
- SDK: 83 library tests and seven integration tests passed. Existing live-provider
  tests remain ignored; cancellation at public RustFS admission preserves the
  canonical outer journal uncertainty after positive constructor drainage.
- CLI: 182 library tests passed. Native controller: 88 controls passed, four
  fixture-dependent cases ignored. This has not executed the live qualification.
- Strict touched-surface Clippy and workspace formatting checks passed. The
  lockfile adds only the existing futures development dependency for the actual
  held ObjectStore destructor control.
- Two independent source reviewers verified the immutable 13-file packet and
  closed the earlier integration and lifecycle proof gaps without new findings.
- The original SDK synchronous-construction assertion and original generic probe
  error assertion have recorded RED outcomes. Newly added owner fault controls
  have GREEN outcomes; they have not been replayed individually against an owner
  implementation that did not exist in the baseline.
- Live startup freshness, physical IOPS, production capacity, native mounted
  end-to-end behavior, remaining fault matrices and current CI remain separate
  qualification gates. The full goal remains active.
