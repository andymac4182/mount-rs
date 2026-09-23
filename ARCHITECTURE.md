# Storage composition and durability

`mount-rs-core` owns the filesystem, metadata, block and chunker contracts.
Integration crates implement providers; `mount-rs-chunked` composes them.
Transport crates and the napi-rs/CLI frontends consume the filesystem contract.

## Provider roles

| Provider | Metadata | Immutable blocks | Durability |
| --- | --- | --- | --- |
| Memory | Yes | Yes | Volatile |
| SQLite | Yes | Yes | Persistent file-backed database; memory databases are volatile |
| PGlite | Yes | Yes | Explicit caller assertion, volatile by default |
| R2 / S3-compatible endpoint | No | Yes | Explicit caller assertion for completed remote object writes; live service acceptance still required |
| AWS S3 | No | Yes | Explicit caller assertion for the object-store block adapter; actual AWS S3 acceptance remains separate |
| TiDB | Yes | Yes | Explicit caller assertion, volatile by default; depends on the TiDB/TiKV deployment |
| FoundationDB | Yes | Yes | Explicit caller assertion, volatile by default; native client and either lease authority or revision-CAS mode are opt-in |

Metadata and blocks can use different providers or independent database files.
Metadata contains inode attributes, directory entries and block references, not
file bytes. Fixed-size chunk configuration is persisted in each file layout;
changing runtime defaults does not reinterpret existing files. The chunker
interface is extensible, but additional algorithms are not implemented yet.

## Write ordering and coordination

Every chunked write finishes new immutable blocks and the block-store barrier
before publishing metadata that references them. The metadata publication and
its barrier complete before success is acknowledged. The driver serializes
operations within one instance; the metadata provider coordinates independent
instances.

The default exclusive-writer mode has three steps:

1. Acquire a provider-clock writer lease with a monotonic fence.
2. Write new immutable blocks, then complete the block-store barrier.
3. Publish the new metadata revision with both revision CAS and the current
   writer fence; complete the metadata barrier before acknowledging success.

Expired leases can be reacquired only if no intervening owner fenced the instance and the metadata
revision is unchanged. Ownership loss fails closed. A metadata publication or
barrier failure can leave an uncertain commit; the caller receives an error,
the instance stops accepting operations, and reopening loads authoritative
metadata. An error is not a promise that a transaction never reached storage.

The experimental, opt-in FoundationDB concurrent-writer mode lets two
coordinators open a fresh volume without a long writer lease. The provider
persists a write-mode marker and a legacy-key sentinel so older
exclusive-writer clients fail before claiming that volume. Each client reloads
the authoritative namespace before filesystem operations. A FoundationDB
transaction publishes metadata chunks and the manifest only if the expected
manifest revision still matches. A known `EAGAIN` conflict permits a bounded
retry after reloading and rebuilding the operation. An ambiguous storage
error fails closed and is never treated as a
known non-commit. Local namespace state changes only after publication is
acknowledged. The mode is available only on a fresh prefix: an existing lease
or fence key requires an explicit offline migration. All writers must use the
same shared block backing: independent memory or local SQLite block stores
cannot supply bytes referenced by another writer's metadata. The CLI rejects
`memory` and `sqlite` blocks in this mode. The final macOS native
two-process case passed 1/1 on the guarded source on 2026-09-23. One CLI
serving two FoundationDB mounts also passed 1/1. Cross-host behavior has not
been verified.

Concurrent mode does not yet have distributed open-file pins or safe block
reclamation. It keeps detached file tombstones for remote open handles,
disables automatic atime publication, and rejects block reconciliation until
those protocols exist. It also cannot collect immutable blocks staged for a
conflicted or failed publication. Neither tombstones nor staged blocks have a
built-in retention cap, so backing storage can grow without bound during use.
This mode remains experimental until a distributed handle-pin and capacity
management protocol is implemented and accepted. A FoundationDB key watch
could later wake an instance to refresh its namespace; revision transactions,
not watch notifications, control write conflicts.

## Shared native NFS views

One CLI can expose the same opened filesystem at several writable NFS
mountpoints with `--also-mountpoint`. Two independent CLIs can share a fresh
FoundationDB volume only with `concurrent_writes: true`, FoundationDB metadata
using `revision-cas`, and the same shared immutable block store. Those two
arrangements use the same NFSv3 identity safeguards.

An NFS file handle identifies an inode, not its last path. In a shared view,
the server checks the inode identity after opening a file. A directory handle
must also be checked when reading: another mount can rename its directory and
replace the remembered path before `LOOKUP` or `READDIR`, causing a path-only
read to return entries from a different inode. `FsDriver::guarded_read` checks
the original handle identity and gathers `STAT`, `LOOKUP`, `READDIR`, or
`READLINK` results within one backend snapshot. The filesystem advertises
this with `supports_guarded_reads()`.

A path mutation
must check its handle-derived parent and source identities within the same
backend commit as the mutation, including each retry after a metadata revision
conflict. `FsDriver::guarded_mutation` expresses that operation;
`supports_guarded_mutations()` is the explicit capability. `MemoryFs` checks
under its state lock, and `ChunkedFs` checks against an authoritative namespace
snapshot or its mutation transaction. Both also advertise
`stable_inode_ids()`: inode numbers are not reused, so a handle remembered
before a remote rename can be rebound when the renamed path is discovered.
If no live path still names the original inode, path operations return a stale
handle error. The CLI requires both guarded-read and guarded-mutation
capabilities before starting shared NFS views. It rejects filesystems without
those atomic identity guards, including `HostFs` and the legacy SQLite
snapshot facade. One CLI can still mount either of those filesystems at a
single path.

The `shared_concurrent_view` session setting enables these NFSv3 identity
checks and implies WCC omission. The separate `omit_wcc_attributes` setting
can also omit WCC in an ordinary view. The shared CLI profile omits optional
weak-cache-consistency
before/after attributes from mutation replies because another writer can commit
between this server's mutation and its reply. macOS mounts also request
`noac` and `nonegnamecache`. These options improve the next lookup, but a
kernel may retain positive directory entries or file pages; open descriptors
and a Finder directory can require a refresh after another mount writes.
NFSv4 requests against this shared profile fail closed.
Shared NFSv3 sessions keep descriptors and opaque handle entries for visited
regular files until server close; `max_handles` is a soft target that does not
bound pinned entries. Long-running shared mounts need a bounded release
protocol before they can be qualified for sustained capacity.

The combined filesystem advertises durable writes only when both providers do.
Memory plus persistent storage remains volatile. Provider durability assertions
must reflect the actual deployment, not merely an acknowledged network request.

Failed writes can leave unreferenced immutable blocks. Automatic garbage
collection and copy-on-write views are not implemented. Never delete blocks
that may be referenced by live metadata; block deletion is an explicit
maintenance operation, not part of ordinary overwrite/unlink.

## SQLite files hosted inside mounts

SQLite used as a provider is different from SQLite databases stored *inside*
the mounted filesystem. The latter additionally needs correct kernel locking,
journal/WAL/shared-memory behavior, barriers and recovery.

The verified native configuration is a single Linux FUSE mount with separate
file-backed SQLite metadata and block providers. Tests use the real SQLite
engine, FULL synchronization, DELETE/WAL, competing processes, killed SQLite
writers, database reopen and integrity checks. A separate child-service test
also verifies committed databases after graceful shutdown and SIGKILL, waiting
for the production writer lease to expire before restarting.

Separate native macOS NFS checks now cover a single-host SQLite file inside a
mount using DELETE journaling, FULL synchronization, competing SQLite handles,
rollback/integrity checks and fresh CLI reopen. Those checks do not establish
power-loss behavior, distributed multi-host SQLite locking, SQLite safety over
every transport, or live R2 hosting. Do not infer those guarantees from
userspace driver tests.
Linux FUSE also passes injected block-write, block-barrier, metadata-publication
and metadata-barrier failures in both journal modes: SQLite receives an error,
and fresh mounts recover valid database contents. These controlled provider
faults do not simulate host power loss. Further platform/provider acceptance
remains tracked in `PORTING_STATUS.md`.
Tests and their exact CI revisions are recorded there; the project goal remains
active until the full required acceptance matrix is satisfied.

Legacy `PersistedFs` factories serialize a whole-filesystem snapshot. They
remain for compatibility and differential testing; new independently configured
storage uses `ChunkedFs` / Node `createChunkedDriver`.
