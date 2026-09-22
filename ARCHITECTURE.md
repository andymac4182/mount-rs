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
| FoundationDB | Yes | Yes | Explicit caller assertion, volatile by default; native client and lease authority are opt-in |

Metadata and blocks can use different providers or independent database files.
Metadata contains inode attributes, directory entries and block references, not
file bytes. Fixed-size chunk configuration is persisted in each file layout;
changing runtime defaults does not reinterpret existing files. The chunker
interface is extensible, but additional algorithms are not implemented yet.

## Write ordering and ownership

1. Acquire a provider-clock writer lease with a monotonic fence.
2. Write new immutable blocks, then complete the block-store barrier.
3. Publish the new metadata revision with both revision CAS and the current
   writer fence; complete the metadata barrier before acknowledging success.

The driver serializes operations within one instance. Expired leases can be
reacquired only if no intervening owner fenced the instance and the metadata
revision is unchanged. Ownership loss fails closed. A metadata publication or
barrier failure can leave an uncertain commit; the caller receives an error,
the instance stops accepting operations, and reopening loads authoritative
metadata. An error is not a promise that a transaction never reached storage.

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

This evidence does not establish power-loss behavior, distributed multi-host
locking, native macOS SQLite hosting, SQLite safety over every transport, or
live R2 hosting. Do not infer those guarantees from userspace driver tests.
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
