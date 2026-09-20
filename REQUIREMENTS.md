# Delivery requirements

This file supplements the active porting goal and preserves subsequent user
requirements. `PORTING_STATUS.md` records evidence, not a reduction of scope.
User-selected architectural inspiration is tracked in [REFERENCES.md](REFERENCES.md),
including agentfs, Archil, and Tensorlake; mountx remains the compatibility oracle.

## Current completion gates

- Complete the Rust port of the pinned `pithings/mountx` source with behavioral
  parity tests, separate core/integration crates, minimal dependencies, and
  napi-rs Node bindings.
- Verify memfs, SQLite, Cloudflare R2, and real PGlite integrations. Local
  object-store tests do not replace authenticated live R2 verification.
- Verify supported macOS and Linux configurations, including native operations
  and the Node package. Commit and push validated work chunks to `origin/main`.
- **Safely host SQLite database files on mount-rs filesystems.** This is distinct
  from using SQLite as mount-rs's persistence backend. It is a required delivery
  gate, not established by backend tests or ordinary file round trips.
- **Separate metadata storage from byte/block storage.** Drivers must compose
  independent stores, including configurations where metadata and bytes use
  different backing services. The current whole-filesystem JSON snapshot is
  transitional and does not satisfy this architecture requirement.
- **Implement fixed-size chunking initially behind an extensible interface.**
  Persist algorithm/version and parameter identification. Additional algorithms
  are future extensions; the user selected fixed-size for initial delivery.

### Metadata, block storage, and chunking acceptance

- Keep metadata-store, block-store, chunking, and filesystem orchestration
  responsibilities explicit. Provider integrations remain separate crates;
  core contracts must not import a cloud SDK or a database implementation.
- Metadata represents namespace/inodes, attributes, links, file lengths,
  ordered chunk/extents, and revisions. Bytes reside in independently selected
  block storage, not in metadata snapshots disguised as a second interface.
- Exercise mixed configurations, including in-memory stores, SQLite/PGlite
  metadata with remote R2 blocks, and local alternatives that run without
  credentials. Report tested combinations rather than assuming all pairings.
- Define atomic publication across the two stores: referenced blocks must be
  available and durable to the promised level before metadata acknowledges a
  commit. Failed uploads, metadata conflicts, retries, crashes, and abandoned
  blocks must have explicit recovery and reclamation semantics.
- Preserve file/handle behavior across chunk boundaries: arbitrary offset
  reads/writes, append, partial overwrite, truncate/grow, sparse/zero-filled
  ranges, concurrent handles, links, and reopen. Test boundary sizes and
  randomized operations against the TypeScript oracle.
- Persist chunker version and parameters so existing files remain readable
  when defaults change. Verify determinism, bounded chunk sizes, reassembly,
  empty files, and rules for future algorithm switching/migration. Selecting an algorithm
  must not silently reinterpret existing data.
- Run SQLite-hosting acceptance through this split-store architecture as well
  as memory. Backend snapshot tests from the transitional implementation do
  not prove the final architecture's transaction or durability guarantees.

This requirement does not by itself bring user-visible copy-on-write snapshots
or clones into current scope; those remain the separate future requirement.

### SQLite hosting acceptance

Run the real SQLite engine against paths inside actual mounts, with evidence
for every supported platform/transport/backend combination. Record the tested
SQLite version, journal mode, synchronous setting, and storage configuration.

- Transaction commit, rollback, reopen, schema changes, and binary data must
  preserve contents; `PRAGMA integrity_check` must return `ok` after each
  recovery scenario.
- Multiple connections and separate processes must exercise readers, competing
  writers, lock contention, busy errors/timeouts, and connection termination.
  Verify filesystem lock semantics; do not substitute an in-process test mutex.
- Test rollback-journal and WAL modes, including journal/WAL/SHM lifecycle,
  checkpointing, file extension/truncation, and any required shared mappings.
  A mode that cannot be supported safely must fail explicitly and remain a
  documented acceptance gap, rather than silently changing modes.
- Verify synchronization ordering and durability under the declared storage
  guarantees: committed data survives filesystem-service restart and backend
  reopen; interrupted writes, failed synchronization, and process crashes do
  not produce silent corruption or falsely acknowledged durable commits.
- Separate SQLite-process failure from mount-service failure and backend
  failure. Test recovery with fault injection and abrupt termination, not only
  graceful closes. Do not equate process-restart tests with power-loss proof.
- Define and enforce the writer/ownership model for persistent and remote
  backends. Concurrent mounts must not silently corrupt one database; any
  single-mount restriction must be enforced and clearly reported. Snapshot CAS
  alone is not proof of SQLite-compatible locking or transaction durability.
- Memfs must preserve transaction/locking correctness while explicitly reporting
  its volatile nature. Do not claim crash-persistent storage for memfs.

Until these gates have evidence, SQLite hosting is **not verified safe**.

## Future requirement: copy-on-write

Track copy-on-write as future work, not a current feature or a requirement to
implement during this port unless the user explicitly brings it into scope.

The future design must define snapshot/clone semantics, isolation between views,
atomic publication, crash consistency, synchronization with live writers,
shared-data reclamation, and backend support. It must preserve the filesystem
contract and SQLite-hosting guarantees. Snapshot serialization or full-snapshot
conditional replacement is not itself copy-on-write support. The API and storage
granularity remain design decisions; no implementation or compatibility claim
is made yet.
