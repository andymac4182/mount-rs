# Filesystem blocks implementation plan

**Goal:** Compare the existing TiDB metadata path with persistent host filesystem blobs using normal OS writeback through the existing QUIC workload.

**Architecture:** Add a compiled block-only provider with immutable content identities and persisted authority, expose it through SDK/CLI configuration, and extend the native benchmark's owned-blob selector and direct oracle. Keep existing storage providers and workload geometry intact.

**Global constraints:** Linux/macOS, same-host shared root; no forced file/directory/device synchronization; durable=false with explicit persistent capability; no symlink traversal or root replacement; no implicit cleanup; original eight containers/six volumes preserved; 64 GiB host-free floor; bounded evidence/deadlines; no automatic replay of unknown outcomes. Root applies source edits and executes every check; delegated workers prepare source-only patches and reviews.

## 1. Provider

- [ ] Apply the manifest, separable integration tests and ENOTSUP scaffold for `providers/mount-rs-filesystem-blocks`.
- [ ] Run the real provider controls against the scaffold and confirm missing behavior fails. Use `scripts/cargo-shared test -p mount-rs-filesystem-blocks --offline -- --test-threads=1` under the bounded command owner.
- [ ] Implement `FilesystemBlockStore::open(root, persistent)` and the BlockStore methods using descriptor-relative Unix operations, SHA-256 sharding, private staging, atomic non-overwriting publication, normal OS writeback and stable marker verification.
- [ ] Run provider controls and strict Clippy; fix concrete failures before integration.

## 2. SDK and CLI

- [ ] Add block-only selection controls first. Check an independent TiDB/Filesystem composition, reopen and exact bytes.
- [ ] Add `StoreConfig::Filesystem { root, persistent }`, context construction, CLI parser and runtime/cache mappings. Reject metadata selection and preserve existing provider behavior.
- [ ] Run formatting, SDK/CLI focused tests and strict all-target Clippy through `scripts/cargo-shared` with the isolated shared target.

## 3. Native admission and oracle

- [ ] Add pure owned-root selection/admission controls first, including missing identity and symlink/foreign root rejection.
- [ ] Extend `remote_blocks` and production-target setup/preparation/oracle to use owner-created per-Drive filesystem roots; retain every geometry, content, scope and process-settlement control.
- [ ] Retain exact provider/source labels and mark unavailable object-store HTTP/physical-I/O observations unavailable. No synthetic zero coverage.
- [ ] Build and inventory the exact immutable instrumented native executable; run its ordinary controls before runtime admission.

## 4. Actual comparison and publication

- [ ] Review and pin a bounded filesystem owner and supervisor. Check source/root/protected-backend identity, projected disk/evidence budgets and cleanup ownership before invocation.
- [ ] Run all sixteen cells plus initial/final every-byte oracles, cross-server and scope/revocation controls. Collect SQL, blob, CPU, allocation, RSS and QUIC observations; settle all processes and confirm backend preservation.
- [ ] Extract a bounded public projection and independently verify exact counts/quotients/correctness identities. Publish the matched RustFS/filesystem results with storage-topology and physical-I/O limits.
- [ ] Commit/push scoped changes and report, update the authorized PR, verify fresh CI/head and append actual evidence to the active goal ledger. Keep the larger production goal active until its remaining gates pass.
