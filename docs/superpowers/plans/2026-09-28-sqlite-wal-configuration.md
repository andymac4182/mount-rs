# Explicit SQLite WAL Configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox syntax for tracking.

**Goal:** Make the measured WAL improvement available through the normal split-store provider configuration while preserving durable block-before-metadata publication.

**Architecture:** SQLite metadata and block providers accept a typed construction option after existing schema and authority validation. An additive SDK option variant preserves path-only Rust callers. Normal CLI and remote Drive JSON use the same SQLite provider with an optional `journal_mode`; N-API exposes `journalMode`.

**Tech Stack:** Rust 1.95, rusqlite 0.39.0 / bundled SQLite 3.51.3, existing CLI/SDK/N-API adapters, Node 24.18.0.

## Global Constraints

- Implements the approved remote-production performance specification using the retained native VFS DELETE/WAL measurements; no transport, format, acknowledgment or durability-policy changes.
- Default `Preserve` executes no journal-mode assignment, including an existing WAL database. Explicit `Wal` requires a qualified local Linux/macOS file and keeps `synchronous=FULL` (`2`).
- Existing block durability before metadata publication remains mandatory; two commits remain separate.
- No per-request pragmas, checkpoint-policy change, authority restamping, automatic downgrade or ignored options.
- Empty, `:memory:` and `file:` paths are rejected for explicit WAL before opening or path rebasing. Preserve-mode behavior remains unchanged.
- Existing requested/canonical physical identity, single-hard-link, local filesystem/mount-root and auxiliary-path checks apply. Safe canonical symlinks remain supported.
- Validate selected and any present opposite-role persisted authority before journal mutation. Refusal must preserve journal mode and authority bytes.
- Use `scripts/cargo-shared`, the existing dedicated `/private/tmp/mount-rs-causal-metrics-target-20260927` cache and bounded owned execution. Never edit/build the protected sibling checkout.
- Full production capacity and power-loss durability remain separate, unqualified gates; the full runner's 64 GiB disk reserve remains unchanged.

## Task 1: Provider options and normal adapter wiring

**Files:**
- Modify `providers/mount-rs-sqlite/src/lib.rs` and `storage.rs`; a focused `journal_options.rs` module is permitted for the construction-only policy and controls.
- Modify `crates/mount-rs-sdk/src/{options,providers,filesystem,lib}.rs`.
- Modify `apps/mount-rs-cli/src/{config,runtime,server_cache,remote}.rs`.
- Modify `bindings/mount-rs-napi/src/lib.rs` and its public `index.d.ts`.
- Extend focused provider/config/runtime/N-API tests and the existing signed configured remote compact test.

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SqliteJournalMode {
    #[default]
    Preserve,
    Wal,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SqliteStorageOptions {
    pub journal_mode: SqliteJournalMode,
}
// Both providers retain open(path) unchanged and add:
pub fn open_with_options(path: impl AsRef<Path>, options: SqliteStorageOptions) -> Result<Self>;
// Add, rather than changing the existing path-only variant:
StoreConfig::SqliteWithOptions { path: PathBuf, options: SqliteStorageOptions }
StorageProvider::SqliteWithOptions { path: PathBuf, options: SqliteStorageOptions }
```

CLI accepts exactly `"preserve"` or `"wal"` at optional SQLite-only `journal_mode`. Omission selects the old path-only variant. N-API accepts the same values at `journalMode`; reject it on other providers before any store is opened. SDK pairing/cache-integrity/SQLite transport checks recognize both variants.

Construction order: reject explicit-WAL special path; open with existing default initialization; run selected-role `from_database`; inspect any opposite-role authority without enrollment/restamping; acquire connection mutex, require autocommit, validate existing local backing guards, execute fixed `PRAGMA main.journal_mode=WAL` and require returned `wal`; require FULL `2`; recheck physical identity; return the provider. Preserve exits before all journal policy work.

- [x] Add a real behavioral RED: valid normal split-store JSON with both roles `journal_mode:"wal"` must parse and reach the real runtime. The old strict parser must reject the field. Capture exact named failure before implementation.
- [x] Implement the typed provider option and additive adapter wiring above, with fixed labels/errors and no new request-path work.
- [x] Provider controls: fresh default DELETE/FULL, WAL/FULL for both roles, same and separate files, exact bytes after fresh reopen, default reopen retains WAL, memory/URI refusal, hard-link refusal, canonical symlink success, copied/mismatched auxiliary authority refusal without journal mutation, and opposite-role authority refusal.
- [x] Adapter controls: closed values/types and non-SQLite rejection, both SDK variants accepted by existing pairing rules, raw special paths refused before CLI/catalog rebasing, invalid N-API options refused before first open.
- [x] Extend the signed configured compact Drive fixture to select WAL through catalog JSON and retain existing QUIC/WebSocket/full-payload/EOF/MRC5/clean-restart checks. Observe actual mode/FULL, not the requested string alone.
- [x] Root runs scoped provider/SDK/CLI/N-API tests, exact signed CLI fixture, strict touched-surface Clippy and formatting after source freeze. Preserve RED receipts and all final logs; reject zero-case passes.

## Task 2: Evidence, review and publication

**Files:** provider and CLI/N-API README sections; an explicit normal-provider example; retained report/goal ledger and existing PR #33.

- [x] Document the normal option, same-host local filesystem constraint, defaults, persistent WAL sidecars, existing checkpoint policy and FULL synchronization. Do not equate the prior small fixture with production throughput or power-loss qualification.
- [x] Independently review frozen diff for source compatibility, backing authority, path rebasing, unsupported backend validation, default behavior and test coverage.
- [ ] Join tested source pins to committed bytes; verify all 15 protected sibling files unchanged; commit only owned changes and push to the existing PR branch. Preserve prior PR body and attach PR #33.
- [ ] Capture one exact-head CI metadata snapshot. Keep the goal ACTIVE while full-capacity/backend/cache/fallback/formal/native/CI/merge requirements remain unproven.

## Execution status

This plan continues the already-authorized performance work. Root owns bounded execution and publication; one implementation worker owns the source changes, with independent read-only review. Existing source/audit evidence fixes the contract above; no new user decision is required for these implementation mechanics.

## Local validation result

Root ran the final frozen source through SQLite (120 passed, 8 ignored), SDK (35 passed, 8 ignored), CLI (109 passed), Rust N-API (53 passed, 6 ignored), the exact signed QUIC/WebSocket compact restart fixture (1 passed), strict touched-surface Clippy and workspace formatting. Two additional opposite-authority regressions first failed, then passed after existing primary-key and delegated-authority validators were reused. Independent source reviewers found no remaining actionable issue. The new options are not native-JavaScript conversion qualification; production capacity and power-loss qualification remain open. Publication and exact-head CI remain separate gates.
