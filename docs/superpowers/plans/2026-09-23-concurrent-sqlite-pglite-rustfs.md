# Concurrent SQLite, PGlite, and RustFS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give independent writable NFS mounts a safe revision-CAS path using local SQLite, one PGlite server, and RustFS blocks paired with a CAS metadata provider.

**Architecture:** The provider-neutral `ChunkedFs` already reloads an authoritative namespace and replays only proven CAS misses. Each SQL metadata provider persists an opt-in mode marker and a legacy lease fence sentinel, then atomically publishes a revision. RustFS gets a block-only crate over the existing object-block adapter.

**Tech Stack:** Rust, rusqlite, tokio-postgres, object_store, macOS NFSv3, Python SQLite adversarial tests.

The observed results, including failed load variants and qualification limits,
are recorded in [the qualification packet](../../concurrent-provider-qualification-2026-09-23.md).

## Global constraints

- Base branch: freshly fetched `origin/main` at `8a629287104fcdd6d4c334378eccfcab005817ec`.
- Preserve current FoundationDB `revision-cas` configuration and the default exclusive lease mode.
- SQLite files used as backing must be local and durable; direct cross-host SQLite file access is outside scope.
- Two CLIs/hosts using PGlite must reach one socket server and the same block backing.
- RustFS is block-only; metadata CAS comes from FoundationDB or PGlite.
- Legacy SQLite/PGlite snapshot facades remain single-view.
- No online block reclamation or atime publication in concurrent mode.
- Use `./scripts/cargo-shared` for ordinary Cargo commands; avoid worktree-local `target/`.

---

### Task 1: SQLite split metadata revision CAS

**Files:** `providers/mount-rs-sqlite/src/storage.rs` and its provider tests.

**Interfaces:** `prepare_concurrent_mode() -> Result<()>`; `publish_if_revision(expected_revision: u64, namespace: Namespace) -> Result<u64>`.

- [x] Add a provider test that opens two independent SQLite metadata handles, checks the default `ENOTSUP`, and checks an old lease acquisition cannot succeed after an atomic mode conversion.
- [x] Run the focused test red with `./scripts/cargo-shared test --locked -p mount-rs-sqlite concurrent -- --nocapture` (or temporarily omit `--locked` while the new crate updates `Cargo.lock`).
- [x] Add an `MRC1` marker column using checked schema migration and atomically set marker and `fence=9223372036854775807` only on a fresh row.
- [x] Add a conditional revision/namespace update requiring marker, sentinel, no owner, and exact old revision; read the failed state to distinguish known `EAGAIN` from corruption.
- [x] Verify independent conflicts, reopen, legacy lease fencing, in-memory rejection, schema migration, and provider Clippy.

### Task 2: PGlite split metadata revision CAS

**Files:** `providers/mount-rs-pglite/src/storage.rs` and its provider tests.

**Interfaces:** The same `MetadataStore` methods as Task 1, scoped by `volume_key`.

- [x] Add red tests for two separate PostgreSQL-wire connections to the same PGlite volume, mode conversion, and CAS conflict.
- [x] Persist the `MRC1` marker and max-fence sentinel with one conditional row update; reject lease operations in the prepared mode.
- [x] Publish revision and namespace in one conditional SQL statement, returning `EAGAIN` only when the revision changed and marker/sentinel remain valid.
- [x] Run the tests against one disposable server with a connection limit above the two-CLI demand; test restart from a disposable durable data directory.

### Task 3: Named RustFS block provider and frontend selection

**Files:** `providers/mount-rs-rustfs/`, root `Cargo.toml`, `Cargo.lock`, `crates/mount-rs-sdk/src/{options,providers,filesystem}.rs`, `apps/mount-rs-cli/src/{config,runtime}.rs`, `bindings/mount-rs-napi/src/lib.rs`, `bindings/mount-rs-napi/index.d.ts`, examples and focused config tests.

**Interfaces:** `RustFsConfig` and `RustFsBlockStore` use the shared object-block adapter; SDK `StoreConfig::RustFs` and CLI `storage.blocks.kind: "rustfs"` construct it. The SDK/CLI/Node gates enable the two new SQL CAS modes only with supported block pairings.

- [x] Add red config and factory tests for accepted SQLite/SQLite, PGlite/PGlite, PGlite/RustFS, FoundationDB/RustFS pairings and rejected memory or mixed local SQLite blocks.
- [x] Add the RustFS block crate without metadata selection; validate endpoint and credentials using existing environment-reference policy.
- [x] Broaden each frontend gate without changing default exclusive mode or existing FoundationDB JSON.
- [x] Run scoped SDK, CLI, Node type/runtime, RustFS contract, format, and strict Clippy checks.

### Task 4: Independent native mount and provider load qualification

**Files:** CLI native tests and disposable test scripts; avoid the live Finder demo mount.

- [x] Start two fresh SQLite splitstore CLIs over the same local metadata/block files and separate test-owned NFS mountpoints. Exercise create, disjoint writes, rename, unlink, retained handles, shutdown, and fresh reopen.
- [x] Repeat through one PGlite server; record its server version, connection limit, persistence policy, and any multiplexer errors.
- [x] Repeat FoundationDB CAS metadata with a RustFS bucket/prefix using two independent CLI/NFS mounts.
- [x] Repeat PGlite CAS metadata with RustFS blocks using two independent CLI/NFS mounts; the disposable same-host run passed 160 acknowledged calls and fresh reopen.
- [x] Load each qualified pairing with bounded parallel lifecycle operations and record acknowledged count, failures, and elapsed time. Measure SQLite block/metadata growth and preserve evidence of failed variants.
- [x] Distinguish two-process same-host evidence from physical cross-host evidence.

### Task 5: SQLite database files hosted inside NFS mounts

**Files:** Disposable Python matrix and relevant native NFS tests/scripts.

- [x] Run DELETE, TRUNCATE, PERSIST, and WAL journal modes on a single-host NFS view with competing SQLite clients, FULL sync, checkpoint/reopen, and crash interruption; record WAL's fallback to DELETE.
- [x] Try two independent NFS views and document exactly where local lock behavior or stale caching makes this unsupported; avoid enabling `--sqlite-single-host` with shared views.
- [x] Run `PRAGMA integrity_check`, compare final committed rows, and record `SQLITE_BUSY`, I/O, journal and recovery outcomes for every attempted mode.

### Task 6: Verification and pull request

**Files:** Architecture and macOS usage docs plus test evidence; only scoped changed files are staged.

- [x] Review diff and plan coverage; update docs with supported combinations and failure boundaries.
- [x] Run `./scripts/cargo-shared fmt --all -- --check`, relevant strict Clippy, focused tests, provider harnesses, and native macOS acceptance with full exit codes.
- [ ] Check `git status`, staged names, and `git diff --check`; commit the scoped changes.
- [ ] Re-fetch `origin/main`, rebase the branch, rerun affected checks, push, and create a reviewable PR. Keep integration status separate from local behavior.
