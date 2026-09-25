# Independent inode publication

## Approved scope

The user approved independently revisioned inode writes and transactional
directory changes, then requested implementation. This extends the existing
split storage provider and preserves the local mounting API on Linux and macOS.

Ordinary writes to different existing files must not contend on a shared
namespace revision or serialize the whole namespace. A new explicitly selected
MRC4 metadata mode fences existing Legacy, MRC1, MRC2 and MRC3 publication paths.
Existing modes retain their contracts. `inode_updates` defaults to false in
storage configuration; enabling it selects concurrent inode publication and
cannot be combined with delegated directory checkout or writeback.

## Storage representation and authority

The existing namespace document remains the structural base. Every inode has a
complete authoritative node record and revision, including revision-zero records
at enrollment. There is no fallback to a stale base node when a record is missing.
The CAS identity is `(structural_generation, inode_revision)`. Ordinary writes
increment only the selected inode revision; structural publication increments
the generation and resets all authoritative inode records atomically.

Explicit enrollment converts initialized same-backing MRC2 metadata into MRC4
in one transaction after checking the expected revision, authority, physical
identity where applicable, absent lease/grants and valid fencing state. Existing
MRC4 volumes verify their backing read-only. Enrollment does not recreate a
missing backing marker. Old generic metadata reads and publication APIs reject
MRC4 so they cannot expose stale base layouts or overwrite authoritative nodes.

The additive `MetadataStore` API in `src/storage.rs` supplies inspection,
enrollment, coherent full snapshots, selected inode reads, conditional selected
reads, inode CAS and structural publication. Unsupported providers fail closed.
The existing `ConcurrentModeState` enum remains source compatible.

## Transactions

An inode publication checks exact authority, generation and inode revision,
validates a regular file update, then changes only that node record. Identity,
permissions, links and other structural attributes are unchanged; layout, size,
blocks, mtime and ctime may change. New blocks are flushed before publication.
Known non-commit conflicts alone return EAGAIN. Ambiguous acknowledgements remain
errors and never trigger automatic write replay.

Structural publication verifies the complete old inode revision map and its
membership against a coherent snapshot before replacing the namespace and all
inode records. This first implementation deliberately folds every node during
structural operations. A concurrent inode write can therefore cause a structural
retry, but two unrelated inode writes cannot cause each other's revision loss.

SQLite uses an immediate transaction and retains physical file checks. TiDB and
PgLite lock the selected inode before a fresh authority read for ordinary writes;
structural publication locks the root and all node guards. Structural transactions
rewrite all guard generations, preventing read skew without a shared write or
exclusive root lock on ordinary writes. FoundationDB uses non-snapshot authority
and selected inode reads, and a conflict-protected complete range for structural
publication. Backend byte limits and durability barriers continue to apply.

## Runtime

The chunked filesystem stores the structural generation, complete snapshot
version map and independently cached selected nodes. Handle reads and writes
refresh the selected inode rather than cloning or decoding the whole namespace.
Reads retain both freshness checks and fail-closed behavior. Same-file conflicts
reload and reconstruct the original write, preserving append and partial chunk
semantics. Unlinked open handles retain private orphan state as before.

Path, directory and structural operations materialize a fresh complete snapshot.
Reconciliation must never enumerate only stale base references. Unsupported
versioning/reconciliation combinations are rejected rather than risk deleting
live blocks. SDK type erasure forwards every method and storage configuration
persists the explicit option inside the existing drive definition.

## Acceptance and measurement

- Different-inode CAS operations from the same starting generation both succeed;
  the structural generation does not change and no namespace is serialized.
- Two writes using the same inode version have one winner and one proven conflict.
- A stale structural snapshot cannot fold away a committed inode update or a new
  node record. Missing, malformed and mismatched records fail closed.
- Reopen, separate coordinators, append, partial writes, rename, unlink/orphans,
  backing mismatch, old publishers and ambiguous commit behavior retain correctness.
- Actual 100-client/10-server SQLite and TiDB workloads measure writes, metadata
  byte amplification, conflicts, CPU, allocations and backend counters. Native
  FoundationDB verifies the native path. PgLite qualification continues to state
  its known concurrent socket protocol limitation.
- Builds and timed workloads run serially. Formatting, strict Clippy and focused
  and workspace regressions precede the final checkpoint. Local evidence does not
  establish replicated, cross-host or power-loss durability.

## Already running older readers

Checking MRC4 in newly implemented generic APIs is insufficient: historical
conditional readers skipped mode checks and returned early on an unchanged
namespace revision. Enrollment therefore advances structural generation and
atomically stores a strict `{ "format": "MRC4", "namespace": ... }` envelope
with complete guards/tokens. The changed revision invalidates old caches, and
plain Namespace decoding fails. All structural publications retain the
envelope. This also prevents silent stale reads by mode-blind direct readers.
