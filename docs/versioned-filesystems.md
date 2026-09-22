# Versioned filesystems design checkpoint

Status: proposal for review. This document does not change the core, provider, N-API, SQLite-VFS, just-bash, or Mastra implementations. It defines the smallest correctness boundary those implementations should target.

## Decision summary

Versioning should be a logical layer above the existing split metadata/immutable-block design:

- A durable version is an immutable namespace manifest plus references to immutable blocks. The initial implementation may copy the namespace manifest for every version; it must not copy unchanged block bytes.
- A version ID is a stable, never-reused identity in one filesystem timeline. It is not a timestamp, a provider ETag, a block hash, or the current metadata revision.
- The mutable head is advanced with the existing lease/fence and expected-revision CAS discipline. A snapshot becomes visible only after every referenced block and its manifest are durably available.
- Historical reads and pinned/following read-only views are mount-independent. A mount, a future mount-free SQLite VFS, and future just-bash/Mastra adapters are transports over the same view API, not separate filesystem implementations.
- Restore is a new publication whose parent is the current head at the successful CAS, with a separate `restored_from` reference to the selected historical version. It preserves the target timeline's ancestry while recording the source of the restored contents. It never silently overwrites or deletes the current head. Fork creates a new filesystem identity from a retained version and leaves the source unchanged.
- A generic filesystem barrier is not SQLite application consistency. A SQLite-aware participant must quiesce/checkpoint the database before a snapshot can be advertised as application-consistent.
- Firecracker-style changed-block tracking, copy-on-write layers, delta manifests, and content-defined chunking are future optimizations. Correctness initially comes from complete logical manifests, fixed-size chunks, immutable blocks, and conservative garbage collection.

The design deliberately does not adopt Tensorlake's same-path last-writer-wins behavior for the core version publication path. A concurrent publication that loses its expected head or lease must return a conflict (`EAGAIN`/`ESTALE`-style), or use an explicit future merge policy.

## Existing contracts this extends

The current split design in [`src/storage.rs`](../src/storage.rs) already gives the required storage primitives:

- `Namespace` contains inode, directory, file-layout, sparse-gap, symlink, and metadata state; file bytes are referenced by `BlockId` and `BlockExtent`, not embedded in the namespace.
- `MetadataStore::publish(expected_revision, lease, namespace)` is an atomic metadata publication guarded by a revision and `WriterLease` fence.
- `BlockStore::put` writes immutable blocks, and `BlockStore::flush` is the durability barrier before metadata may reference them.
- `ChunkerConfig` is persisted per layout. Fixed-size chunking remains the initial format.
- [`filesystems/mount-rs-chunked/src/lib.rs`](../filesystems/mount-rs-chunked/src/lib.rs) already serializes operations with its gate, flushes blocks before publishing metadata, and fails closed when a metadata publication is uncertain.

The versioning extension must preserve those invariants. The provider's internal revision is a concurrency token; it is not itself a user-visible historical version because it may advance for metadata changes such as access-time updates and may not identify a complete block/manifest publication.

## What the external references establish

### Tensorlake filesystem behavior

The [Tensorlake filesystem introduction](https://docs.tensorlake.ai/filesystems/introduction), [core concepts](https://docs.tensorlake.ai/filesystems/core-concepts), [filesystem mounts](https://docs.tensorlake.ai/filesystems/filesystem-mounts), [read-only mounts](https://docs.tensorlake.ai/filesystems/read-only-mounts), [concurrent writes](https://docs.tensorlake.ai/filesystems/concurrent-writes), [session management](https://docs.tensorlake.ai/filesystems/manage-sessions), and [architecture](https://docs.tensorlake.ai/filesystems/architecture) provide a useful product vocabulary:

- recent autosaves support recovery, while named snapshots are permanent until deleted;
- a snapshot is an exact point-in-time state and can promote an already-durable autosave without duplicating bytes;
- historical reads select an explicit version; read-only views may either follow the current head or pin a permanent snapshot;
- a fork from a retained snapshot gets an independent timeline, while the source remains unchanged;
- content and metadata are separated, and historical content is fetched lazily by reference.

Those concepts fit mount-rs after translation to its existing block/namespace contracts. Tensorlake's documented concurrent-write rule—disjoint paths merge and same-path writes are ordered last-writer-wins—is not suitable as the core publication rule for a filesystem that must support SQLite or other application-level transactions. Mount-rs should expose a conflict instead of silently discarding a competing namespace publication.

### Changed bytes, zeroes, and sealed layers

The [Tensorlake Firecracker changed-bytes article](https://www.tensorlake.ai/blog/firecracker-disk-snapshots-o-changed-bytes) describes a different but relevant physical optimization: a paused VM seals an overlay, tracks dirty 4 KiB blocks and known-zero ranges, installs a fresh overlay, and uploads immutable content-addressed layers/manifests asynchronously. Snapshot work can then scale with changed bytes plus metadata rather than the full disk, with compaction controlling layer depth.

The useful lesson for mount-rs is to separate three facts that must not be conflated:

1. a logical cut/seal at which a namespace and its referenced blocks are fixed;
2. a local pause/quiescence boundary that makes a participant's state coherent; and
3. completion of remote durable publication.

The article's VM pause and WAL-recovery claims must not be generalized to a host-mounted SQLite database. A generic `FsDriver` does not own SQLite's transaction boundary, WAL, SHM, or journal protocol. COW layers remain a future physical optimization, not a requirement for the first correct versioning implementation.

## Proposed logical model

### Version identity and history

The conceptual public model is:

```text
VolumeId       = stable filesystem identity
VersionId      = (VolumeId, monotonic sequence) or an equivalent opaque encoding
VersionInfo    = { id, parent, restored_from?, forked_from?, sequence, kind,
                   manifest reference, created-at, durable state,
                   retention/pin state }
Head           = { version id, provider revision, publication identity }
```

The exact Rust/Node wire representation can be chosen during implementation, but the following are invariants:

- IDs are allocated only by the authoritative metadata store, are monotonically ordered within one timeline, and are never reused after deletion.
- The sequence gives user-visible ordering. It must not be derived from wall-clock time, because clocks can move and simultaneous providers can disagree.
- A manifest/content digest is useful as an integrity field and deduplication key, but it does not replace `VersionId`: two equal states may be distinct checkpoints, and ordering/retention must remain explicit.
- The provider revision/fence and a caller-supplied operation ID remain part of the CAS protocol. A retry after an ambiguous response must reconcile the operation ID rather than create an accidental duplicate or guess whether a commit occurred.
- A fork gets a new `VolumeId` and starts its own sequence. It may share immutable blocks and manifest objects with the source.
- `parent` is always the immediately previous head in the same volume timeline. An ordinary snapshot has the current head as parent. A restore also has the current head as parent, while `restored_from` identifies the retained historical version whose manifest/content was selected. This prevents a restore from making the timeline appear to jump backwards.
- A fork's initial record has a new volume identity and a `forked_from` source reference `(source_volume, source_version)`. That cross-volume provenance is informational and does not make the source version the target's same-volume `parent`.

An additive versioned metadata capability should conceptually provide:

```text
load_head() -> Head
load_version(VersionId) -> immutable manifest/VersionInfo
publish_version(expected_head, lease, operation_id, manifest, kind) -> VersionInfo
promote_snapshot(VersionId, label/retention) -> VersionInfo
list_versions(cursor/filter) -> VersionInfo...
pin_version(VersionId, pin identity/expiry)
unpin_version(pin identity)
delete_version(VersionId, expected head/retention checks)
```

This is an extension of `MetadataStore`, not a replacement that weakens existing providers. A provider may initially implement a complete namespace manifest per version. Visibility and durability are one explicit provider contract, not an assumption about a later `flush()` call:

- `Pending` publication records may be durable internal state that holds manifest/block references and an operation ID, but are excluded from `load_head()` and public history.
- An `Active` record and head advance become visible together in one authoritative metadata transaction, after referenced blocks and the manifest have passed the block-store barrier.
- `publish_version` must return success only after that active transaction has durably committed. It must not expose an active record and then rely on a separate post-commit `metadata.flush()` to make it durable. Providers that cannot make the active transaction durable as part of the operation must retain the record as `Pending` and atomically activate it only when they can satisfy this guarantee.
- If the active commit or its durable result is uncertain, the record may already be visible; the API returns an unknown outcome and recovery reconciles by operation ID. It is incorrect to promise that history remained invisible merely because the caller did not receive a response.

Thus a successfully returned version is both publicly visible and durable. An unsuccessful/ambiguous operation is either absent from public history or discoverable as one committed operation; pending records and their references remain GC roots until reconciled.

### Historical and read-only views

The mount-independent API should expose both direct historical operations and a view object:

- `read_at_version(version_id, path, offset, length)` and metadata/readdir equivalents resolve exactly one immutable manifest.
- A **pinned view** resolves one retained `VersionId`, rejects writes with read-only errors, does not acquire a writer lease, and does not publish atime or other incidental mutations. Opening it atomically validates the active version and creates a `ReadLease`/`ViewPin` record containing `(volume_id, version_id, view_id, owner/session, fence, expires_at)`. The view renews that lease before expiry and explicitly closes it; crash cleanup relies on expiry, not on a destructor running. Deletion checks the authoritative unexpired records in the same retention transaction and returns `EBUSY` while any pin is live.
- A **following view** resolves the current head according to an explicit refresh policy. It is read-only but not a stable historical snapshot. An open file handle must retain the version selected for that handle, or the API must explicitly document that each operation re-resolves; it must never switch silently halfway through one read.
- The view API has no mount path or OS-specific lifetime requirement. A mount can adapt it to a path; the future mount-free SQLite VFS, just-bash adapter, and Mastra adapter can consume it directly. Those consumers must still state their own transaction/consistency guarantees.

The view contract should distinguish `VersionId` selection errors (`ENOENT`/unknown version), retention conflicts (`EBUSY`/active pin), lease renewal/expiry, and read-only errors (`EROFS`). A direct historical read should hold a short-lived read lease or an equivalent provider read transaction across manifest and block resolution. It should not turn a missing historical block into an empty file or silently fall back to the current head.

### Snapshot, restore, and fork

`snapshot()` captures the exact logical state at a defined cut. If the selected current state is already a durable permanent snapshot, promotion may add a label/retention record without duplicating its namespace or blocks. Otherwise it publishes a new version.

`fork(version_id)` creates a new `VolumeId` whose initial head references the selected manifest. A manifest reference is qualified by a stable `BlockStoreId` identifying the physical/logical store namespace and its GC authority:

```text
BlockRef = (BlockStoreId, BlockId)
```

The target may retain the source's block references without copying only when the source and target use the same `BlockStoreId` and the same authority can see and GC that store. Otherwise the fork must copy every referenced block/layer into the target store, rewrite the manifest to the target `BlockStoreId`, and publish the target only after the copy is durable. During that copy, the source version and all in-flight target references are retention roots. The source history is not modified.

`restore(version_id, expected_current_head)` creates a new version on the existing volume whose manifest/content is selected from `version_id`, whose `parent` is `expected_current_head`, and whose `restored_from` is `version_id`. The old head and its history remain retained according to policy. The operation is conditional: if the current head changed, it fails with a conflict and leaves the live filesystem untouched. There is no in-place rewind that could invalidate already-open historical views or silently erase newer data.

## Durable atomic capture

The first implementation should use the following coordinator protocol. The protocol is intentionally conservative because the current metadata and block providers are separate stores and do not offer distributed two-phase commit.

1. Acquire the `ChunkedFs` operation gate and the provider writer lease/fence. For an external application participant, acquire its snapshot/quiescence token before observing the namespace.
2. Drain the filesystem's pending operations and establish a cut. Capture a canonical `Namespace`, its expected metadata revision/head, the parent version, and an idempotent snapshot operation ID. No writer may mutate the captured state while the cut is being published.
3. Write any new immutable blocks and the immutable manifest. Call `BlockStore::flush`; require both metadata and block providers to report durable capability for a durable snapshot. A volatile memory checkpoint must be labeled non-durable or return `ENOTSUP` when durable output was requested.
4. In one authoritative metadata transaction, validate the lease/fence and expected head, append/activate the version record, and advance the head (or promote an existing autosave). The transaction must contain enough operation identity to reconcile an ambiguous retry. A provider that needs a pending state writes it here but does not expose it as history/head.
5. `publish_version` must make the active transaction durable before returning success. There is no correctness gap in which an active history row is public while a later `metadata.flush()` is still required. If the active commit or its durable result is ambiguous, fail closed from the caller's perspective and reconcile by operation ID/head/history; never report a guessed success. The record may already be active after a successful commit even if the response was lost.
6. Release the application quiescence token and filesystem gate. If publication failed before active metadata visibility, its pending/unpublished blocks remain protected as operation roots until recovery or GC; they are never a partially readable version.

The invariant is **immutable data first, one authoritative metadata visibility point second, with durable activation before successful return**. It does not claim atomicity across arbitrary remote systems. A provider that cannot make the metadata visibility point durable must advertise that limitation; it must not promise a durable snapshot by relying on an upload returning successfully.

Cancellation follows the same boundary. Cancellation before the metadata commit may abandon the unpublished candidate. Cancellation after the commit must not roll it back; recovery resolves the operation from durable history. Restart loads only committed version records, and GC later handles candidates that are not reachable from a committed record.

## Dirty tracking, namespace changes, and future layers

Dirty physical blocks are not a complete filesystem change set. A rename, unlink, mode/owner/time change, hard-link change, directory mutation, file-size change, sparse hole, or symlink update can change the namespace without dirtying a data block. Conversely, a data block can be rewritten with identical bytes and need not create a new physical block.

The current `FileLayout` represents sparse zero regions as gaps between extents. Any future dirty/zero bitmap or overlay format must preserve at least:

- logical file size and every namespace mutation;
- whether a range is absent/sparse, explicitly zero, or backed by a block when that distinction affects accounting or future writes;
- block identity, offset, length, chunker format, and validation of non-overlapping extents.

The initial snapshot manifest therefore remains a complete canonical `Namespace` plus `BlockId` references. Its cost should be measured as metadata/manifest bytes plus newly written blocks, not advertised as “changed bytes only.” A future delta manifest may store changed extents, deletes, renames, and zero ranges against a parent, but then historical reads and GC must traverse the chain. Compaction can materialize a complete manifest, and only after a replacement is durable may old layers be removed. Layer depth, total referenced bytes, changed bytes, dirty backlog, and compaction debt should be observable separately.

## SQLite consistency boundary

There are two separate SQLite cases:

1. The mount-rs SQLite provider's own metadata/block databases can use SQLite transactions, `synchronous=FULL`, lease/revision checks, and a metadata transaction that atomically records the version/head. Blocks still need to be flushed before that transaction because they live in a separate store/database.
2. A user SQLite database stored through a mount, a future mount-free SQLite VFS, or an adapter is an application participant. The generic filesystem snapshot is not automatically an application-consistent database backup.

The versioning API should therefore have an optional participant/quiescence protocol with a shape like:

```text
prepare_snapshot(expected_scope) -> participant token
  // fence new writes, reach a transaction boundary, checkpoint/backup,
  // and make the relevant main/WAL/SHM/journal state durable
release_snapshot(token)
```

The SQLite VFS or database-aware caller owns the implementation and must identify the database files and journal mode. If no participant can prove a safe boundary, the result must be an ordinary filesystem snapshot with an explicit non-application-consistent guarantee, or the requested SQLite-consistent snapshot must fail with `EBUSY`/`ENOTSUP`. Restore must likewise stop/fence the database, replace the complete related state, and run the database's recovery/integrity procedure before reopening it.

This is why a VM pause/seal in the changed-bytes reference cannot be used as evidence that a live host-mounted SQLite database is safe. The planned separate mount-free SQLite VFS should consume the same historical/pinned view API and add its own database-aware boundary; that work is outside this design checkpoint.

## Concurrency and failure policy

The current in-process gate remains useful for serializing local filesystem operations, but it is not a distributed conflict policy. Every version publication, restore, fork target initialization, pin mutation, and destructive retention operation must validate:

- the expected head/revision;
- the unexpired writer lease and fencing token;
- an operation identity when the operation can be retried; and
- the provider's durability/transaction result.

If another writer advances the head, return a conflict and leave both histories intact. Do not silently apply a last-writer-wins namespace over a concurrent SQLite or application-level transaction. A later merge facility may make conflicts explicit by path or inode, but it is not part of the first versioning contract.

Failure tests must cover block upload failure, block flush failure, metadata CAS loss, active-transaction durability uncertainty, lease expiry, cancellation at every protocol boundary, process restart, and provider recovery. For each failure, the observable rule is either “no active version is visible” or “one committed version can be found and reconciled”; a committed version may already be visible after a lost response, but there must be no active version whose referenced blocks are not durable.

## Retention, pins, and block reclamation

Retention should distinguish recent autosave records from permanent snapshots. The policy can be count-, age-, or label-based, but it must be explicit and inspectable. Deleting a version removes it from the user-visible history only after retention checks; it does not immediately delete blocks.

Pins are leases/identities, not an informal promise that a caller will close eventually. A pin row contains a unique `view_id`, the volume and version, an owner/session identity, a fencing value, and an expiry. `open_pinned_view` creates it in the same transaction that validates the version; `renew_read_lease` extends it only while the owner/fence is valid; `close` removes it idempotently. Expired rows are reclaimable after provider time/lease validation, so process crashes do not leak liveness forever. A pinned view is a retention/read lease, not a writer lease and not permission to mutate metadata. Deletion of a pinned version returns a retention conflict while any unexpired pin exists.

Every manifest carries a `BlockStoreId` (or equivalent manifest-level store identity) with each `BlockId`. A `BlockStoreId` is equal only when the logical store namespace, access boundary, block format, and GC authority are the same; two providers pointing at the same backend with different prefixes are different stores. Block GC is performed per store identity and computes the union of references from all volumes sharing that store: current heads, retained versions, `forked_from`/source roots during copy, live pins, pending publications, unresolved operations, and every parent in a future delta chain. A cross-store fork must hold a source pin until the target manifest is active and durable.

It may call the existing `BlockStore::delete` only after the authoritative metadata retention transaction has removed the last qualified root, a restart/grace window has passed, and no in-flight publication or cross-store copy can still reference the block. A future layer compactor must publish the replacement manifest before dropping old layers. No provider may reclaim based only on dirty-block reports, an unqualified `BlockId`, or a stale per-volume cache.

## Provider and API work implied by this design

This is a review map, not an implementation assignment:

| Area | Required change | Initial correctness choice |
| --- | --- | --- |
| Core storage/API | Add version records/IDs, history listing, immutable load, snapshot publication, pins, fork, restore, and read-only historical views. | Full namespace manifests; existing immutable blocks and fixed-size chunking. |
| `ChunkedFs` coordinator | Add capture/quiescence boundaries and version-aware head publication while preserving lease/revision CAS. | Gate + lease + block flush + one durably activating metadata transaction. |
| SQLite provider | Version/history/pin tables and migrations; atomic version/head transaction; conservative recovery of ambiguous operations. | SQLite transaction for metadata; separate block flush before commit. |
| PGlite provider | Equivalent version/head/history durability and CAS behavior. | Do not infer durability beyond its configured persistence guarantee. |
| Memory provider | Volatile history useful for tests. | `durable = false`; explicit non-durable checkpoints only. |
| R2/block providers | Versioned metadata/index and reachability GC around immutable objects. | Conditional head publication; explicit `BlockStoreId`; no LWW mutable pointer. |
| Mount-free consumers | Consume the same historical/pinned view and direct FS API. | No path/mount assumption in the contract; adapter-specific consistency remains explicit. |
| Node/CLI | Expose version IDs, list/read/pin/snapshot/fork/restore/retention operations. | Preserve error identity and distinguish conflict, retention, read-only, and durability failures. |

The existing whole-snapshot persistence adapters are useful compatibility references but are not proof that the split provider path is version-safe. They should not silently define the new history model.

## Acceptance matrix for the first implementation

The design is ready to implement only when tests cover the following:

- two versions have stable ordered IDs; IDs survive restart and are never reused;
- old/current reads preserve bytes, namespace, metadata, sparse gaps, rename, delete, symlink, and mixed-provider layouts;
- a no-op snapshot promotes/reuses the durable state according to the documented policy without duplicating block bytes;
- pinned reads remain unchanged after head advancement; following reads change only at the documented refresh boundary; open handles do not switch versions mid-operation;
- restore is conditional, creates a new head with `parent=current_head` and `restored_from=selected_version`; fork leaves source history and bytes unchanged;
- pin open/renew/close and expiry are tested with a concrete `ReadLease` identity; active pins block deletion and GC;
- same-store forks share only an equal `BlockStoreId`; cross-store forks copy/rewrite refs and protect source roots until target activation;
- block flush, metadata CAS, active-transaction durability uncertainty, cancellation, lease expiry, and restart never expose a partially published version;
- memory, SQLite, PGlite, and remote block combinations report their real durability guarantees;
- SQLite application-consistency tests use an actual database participant in both rollback-journal and WAL modes; a generic arbitrary-FS snapshot is tested separately and is not labeled equivalent;
- capture pause/seal time, durable completion time, total state, changed bytes, manifest/layer depth, and dirty backlog are measured independently;
- the mount-free SQLite VFS and just-bash/Mastra adapters can target the same view API without requiring a mounted path (adapter implementations remain subsequent work).

Physical COW, changed-block bitmaps, delta manifests, and compaction are not acceptance gates for this first correct implementation. They are follow-on work only after the complete-manifest protocol and recovery tests pass.

## Bounded CrabBuild reference review

This appendix is intentionally limited to design patterns relevant to immutable version publication. Revisions were resolved from the public repositories on 2026-09-20 and are pinned here for review; mount-rs will not add their dependencies or copy their code.

| Candidate | Reviewed revision and relevant pattern | Decision for mount-rs |
| --- | --- | --- |
| [crab](https://github.com/crabbuild/crab/tree/f9495a2cf4ea0e9245cfc645f02c027ae37298c3) | Immutable chunks/manifests are made durable before mutable refs; manifest history validates generation/digest; publication uses authority/ref leases, CAS, release-after-commit, and explicit GC/doctor roots. | **Adopt** immutable-before-mutable ordering, digest validation, CAS head, fencing, and explicit GC roots. **Adapt** to `BlockId`, `Namespace`, provider flush, and `WriterLease`. **Reject** Git refs, pointer-file clones, protocol/dependency reuse, and content-defined chunking for the fixed-size first format. |
| [prolly](https://github.com/crabbuild/prolly/tree/6ee959eaed2625bf086ae0c4d1a2b5934f7e3872) | Immutable ordered-map roots, timestamp-free/content-derived version objects, format/config digests, structural patches, named roots, pruning before reachability GC, and stable cursors. | **Adopt** immutable manifest/root separation, format/config validation, structural-share/diff as future optimization, and prune-then-GC ordering. **Adapt** because a filesystem namespace includes inode/link/sparse/layout semantics and is not merely an ordered KV map. **Reject** treating one map root as a complete filesystem snapshot and adding Prolly/CDC dependencies now. |
| [silo](https://github.com/crabbuild/silo/tree/7f71a06b0560fb1ef85c3aa57bcac365dbe9d7be) | Immutable payloads and root/commit records sit behind a mutable branch ref; authority stamps fence writers; operation IDs reconcile ambiguous commits; historical reads use explicit commits; GC roots come from authoritative refs. | **Adopt** immutable version records, separate mutable head, operation identity, fencing, historical selection, and authoritative GC roots. **Adapt** to one volume timeline, existing leases, split metadata/blocks, and pin leases. **Reject** whole-file payload assumptions, implicit branch merges, and LWW publication. |
| [trail](https://github.com/crabbuild/trail/tree/9823ed7551a1c53bb1a2c1dc329d9faf30e6f460) | The changed-path ledger design distinguishes trusted scope from untrusted callbacks, requires a durable cursor/delivery fence or writer quiescence, records prepared/applied/published/acknowledged/aborted intents, fences snapshot/restore, and protects active views and in-flight roots during GC. | **Adopt** explicit cut/fence, operation state, recovery of unknown outcomes, active-view roots, and restore epoch/fencing ideas. **Adapt** the fence to `ChunkedFs` plus a database-aware SQLite participant. **Reject** using watcher/dirty paths as snapshot authority, importing a CDC ledger/schema, or treating callbacks as proof of namespace completeness. |

The common conclusion is narrow: immutable data must be validated and durable before an authoritative mutable head points at it, and retention/GC must follow authoritative roots. The projects do not justify a new dependency, CDC pipeline, content-defined chunker, or automatic merge policy in mount-rs.

## Review checkpoint

This proposal is bounded at the logical API, publication protocol, provider implications, and acceptance tests. The next review decision is whether to accept the version identity/history, restore ancestry, durable activation, read-lease, and cross-store GC invariants before assigning implementation work. Once main approves that checkpoint, the minimum implementation slice is the separate versioned capability in the memory and SQLite providers; it must not broaden into COW, CDC, adapter, or mount work without a new review.
