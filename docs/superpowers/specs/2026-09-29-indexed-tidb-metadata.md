# Indexed TiDB metadata

Approved direction: the user requested the SQL structure refactor now, stopping
capacity measurements and shipping a substantial improvement. This implements
the normalized membership/directory design discussed on 29 September. Focused
correctness, fault and integration tests remain required.

## Outcome

Common selected file reads transfer one small authority record and one file
record. An ordinary root-child file Open uses one coherent point lookup of
authority, root header, exact directory entry and file. Neither operation reads
the complete membership list or directory entries. Structural publications
persist only changed membership and directory rows instead of replacing large
JSON arrays.

The existing local mounting interfaces, negotiated remote transport, backing
selection, single-Partition mounts and per-Drive authorization remain in use.
This first implementation targets TiDB; other providers use their existing
compact contracts through unsupported capability defaults.

## Physical representation

- A distinct tagged authority envelope contains backing, generation, root,
  next inode, defaults, chunker configuration and member count, without member
  IDs. Existing MRC5 mode/backing fencing stays authoritative. An old physical
  envelope is rejected rather than silently converted.
- `mount_rs_tidb_compact_members`: `(volume_key, inode)` primary key.
- Existing guards retain physical incarnation/epoch/revision. File, symlink and
  special bodies remain complete. A distinct directory payload contains stats,
  entry count and the next append ordinal, without entries.
- `mount_rs_tidb_compact_dentries`: `(volume_key, parent, ordinal)` primary key;
  a nonunique `(volume_key, parent, name_hash)` index; full names in binary long
  values and child inode IDs. Name hashes narrow the lookup; exact binary names
  determine equality. Existing names have no new length limit.

Creation appends an ordinal; unlink preserves surviving ordinals; root rename
removes the source and appends its new name. General Full mutations preserve the
requested directory order, using an explicit directory rewrite only when a
reorder cannot retain existing ordinals. No hash collision becomes name equality.

## Shared scoped contracts

Add an optional point-read capability and explicit scoped receipts. Authority
excludes membership; directory headers exclude entries. Selected membership and
guard identity/body come from one provider statement snapshot. A scoped receipt
does not certify a complete graph or become `VerifiedCompactRoot`.

The caller checks the observed structural generation before interpreting a
missing selected row. A generation change triggers one coherent full snapshot
installation and retry. Equal-generation contradictions in a selected link,
header or physical identity fail closed. Unchanged file certification retains
complete body comparison with the borrowed expectation; cached bodies alone do
not grant freshness.

An eligible Open is an ordinary existing direct-root regular file with read or
read/write flags, without create/exclusive/truncate/append or an extra path guard.
Nested paths, symlinks, directories, missing paths and other shapes retain the
complete logical fallback. Point entry checks compare the actual name/inode to
the audited cached topology and recheck local revision before admission.

## Full audit and publication

Full snapshots enumerate real members, guards and dentries in one repeatable-read
transaction, reconstruct the actual complete logical CompactAnchor and directory
arrays, then run the existing graph validators. Reject extra/missing guards,
foreign parent/child references, duplicate names or ordinals, invalid bounds,
malformed payload tags and header count/order disagreement.

Initially retain the current structural authority lock and shared delta
validators. Reconstruct actual members and affected directories in that locked
transaction, validate the immutable delta, preflight every encoded value/parameter
against packet limits before DML, and atomically persist changed rows plus guard
and authority headers. Full scope retains complete keyspace/phantom checks.
Selected file publication locks its guard first, reads fresh authority and
membership after any wait, validates physical CAS and updates only that inode.
It never obtains the structural authority lock.

Keep immutable blobs durable before publishing their references and wait for
metadata COMMIT before acknowledgment. Proven conflicts remain retryable;
unknown commits retain their identity/resources and are never automatically
replayed. Last-link targeted unlink retains the nlink=0 member/guard tombstone and
its layout; full refresh preserves open detached handles.

## Gates

- Scoped core constructor/comparator tests for authority precedence, physical
  identity, body equality, missing rows and generation changes.
- Codec/directory diff controls for ordering, append ordinals, long names and
  collisions; bounded serialized authority/directory headers.
- Actual TiDB SQL proxy controls: one selected point statement and one eligible
  Open statement; no member/directory range queries on those paths.
- Actual full audit and publication controls: corrupted membership/dentries,
  stale CAS, guard waits, packet rejection before mutation, cancellation and lost
  COMMIT acknowledgment without replay; fresh reopen validates exact data.
- Filesystem/SDK capability dispatch and fallback tests, including concurrent
  structure changes, sparse files, truncation and open-unlinked handles.
- Touched formatting, strict Clippy and focused affected suites, independent
  review, then a concrete PR with truthful CI/proof limits. Capacity benchmark
  runs are deferred by the user's instruction.
