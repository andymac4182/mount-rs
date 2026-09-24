# Independent Inode Publication Implementation Plan

> **For agentic workers:** Use subagent-driven-development to implement these
> independent provider tasks, then review the combined branch. Builds and timed
> workloads are serialized by the root agent.

**Goal:** Publish ordinary file updates without whole-namespace CAS or encoding.

**Architecture:** Explicit MRC4 authority with complete independent node records.
Structural transactions fold all records against a coherent revision map. The
existing mount driver remains unchanged; the chunked implementation chooses the
new path through an explicit storage option.

**Tech Stack:** Rust, async-trait, SQLite/rusqlite, TiDB/mysql_async,
PgLite/PostgreSQL wire and native FoundationDB transactions.

## Global constraints

- Linux and macOS; no new local mounting interface.
- `inode_updates=false` by default; never mix MRC4 with delegated MRC3 or writeback.
- Preserve exact backing checks and flush-before-publication durability barriers.
- EAGAIN only for known non-commit; ambiguous errors never replay writes.
- Complete node guard for every inode; no stale base fallback.
- Run Cargo through `scripts/cargo-shared` with the isolated target directory.

## Task 1: Core contract and validation

File: `src/storage.rs`. Add the exact interfaces:

```rust
struct InodeVersion { structural_generation: u64, inode_revision: u64 }
struct LoadedInode { version: InodeVersion, node: NodeMetadata }
struct InodeMetadataSnapshot {
    structural_generation: u64,
    namespace: Namespace,
    inode_revisions: BTreeMap<InodeId, u64>,
}
struct InodeModeState { backing: ConcurrentBackingId, structural_generation: u64 }
```

- [x] Validate nonzero generation and exact namespace/version map keys.
- [x] Implement regular-file publication validation preserving all Stats fields
  except size, blocks, mtime and ctime.
- [x] Add default unsupported inspection/enrollment/load/CAS methods as specified
  in the design; `inode_mode_state` defaults to `Ok(None)` and conditional loads
  conservatively return the full selected record.
- [x] Test missing/extra revision keys, generation zero, changed inode identity,
  permissions, links and kind, invalid layouts and accepted content updates.
- [x] Run `cargo-shared test -p mount-rs-core --lib inode_ --locked --offline`.

## Task 2: Provider transactions

Files: `providers/mount-rs-{sqlite,tidb,pglite}/src/storage.rs`,
`providers/mount-rs-foundationdb/src/lib.rs` and their provider tests.

Implement the core methods separately for each backend:

```rust
inode_mode_state() -> Result<Option<InodeModeState>>
prepare_inode_mode(backing, expected_revision) -> Result<()>
load_inode_snapshot(backing) -> Result<InodeMetadataSnapshot>
load_inode(backing, inode) -> Result<LoadedInode>
load_inode_if_changed(backing, inode, Option<InodeVersion>)
    -> Result<Option<LoadedInode>>
publish_inode_if_version(backing, inode, expected, node) -> Result<InodeVersion>
publish_structure_if_versions(backing, expected_generation,
    &BTreeMap<InodeId, u64>, namespace) -> Result<u64>
```

- [x] Create complete node records on explicit same-backing MRC2 enrollment and
  atomically fence old publication and generic read paths. Advance enrollment
  generation and wrap the base namespace so historical mode-blind cached
  readers invalidate their revision and fail decoding; test exact old queries.
- [x] Implement selected inode reads that never decode the base namespace.
- [x] Implement selected record CAS; the namespace root is a read dependency,
  never an ordinary file-write key.
- [x] Implement structural complete-map checking and atomic generation/reset.
- [x] Test two independent inode writes at generation G both succeed and G stays
  unchanged; repeated same-inode token fails EAGAIN.
- [x] Test stale structure versions, malformed/missing guards, backing mismatch,
  old reads/writes and reopened effective file content.
- [x] Verify native/backend contract tests when the root grants the serial slot.

## Task 3: Chunked runtime

Files: `filesystems/mount-rs-chunked/src/lib.rs` and focused tests.

- [x] Add explicit `ChunkedOptions::with_inode_updates(bool)` and guarded open.
- [x] Track independently cached nodes/versions beside structural namespace.
- [x] Use selected inode CAS after immutable block preparation for ordinary
  handle writes; on a proven same-inode conflict reload/rewrite against winner.
- [x] Preserve both selected read freshness checks and orphan behavior.
- [x] Materialize coherent snapshots for path/structural operations; use complete
  version maps for publication. Reject unsafe reconciliation/version paths.
- [x] Test concurrent coordinators, same-file overlap, append, partial writes,
  structure races, orphan unlink and sticky authority failures.
- [x] Run chunked library/integration tests before the combined workspace gate.

## Task 4: SDK and drive configuration

Files: `crates/mount-rs-sdk/src/{options,filesystem,stores}.rs`,
`apps/mount-rs-cli/src/{config,runtime,remote}.rs`.

- [x] Add default-false storage and SplitOptions `inode_updates` setting and
  builder; forward through filesystem and every erased metadata method.
- [x] Validate delegated/writeback/unsupported providers before opening storage.
- [x] Persist the setting inside the existing drive `driver` configuration;
  no additional DriveDefinition field is needed.
- [x] Test configuration defaults, explicit opt-in, invalid combinations and
  metadata adapter forwarding.

## Task 5: Qualification and checkpoint

Files: saturation support, profiling scripts and benchmark report/evidence.

- [x] Add an explicit inode-mode benchmark switch with the old mode unchanged.
- [x] Run matched SQLite and TiDB write workloads serially, retaining correctness,
  per-inode serialization/commit counts, namespace bytes, CPU/allocations and
  backend counters. Compare with the committed profiling baseline.
- [x] Run native FoundationDB provider and workload qualification. State PgLite
  concurrent protocol limitations rather than ranking failed workloads.
- [x] Run workspace formatting, strict Clippy and all-target tests. Independently
  review structural race/authority/ambiguous-commit paths and fix findings.
- [x] Record exact passing gates and qualification limits, then commit the
  complete validated change without publishing or deploying it.
