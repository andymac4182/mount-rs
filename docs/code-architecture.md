# Code architecture

This page maps the source and shows where an extension belongs. For the storage
commit protocol and its acceptance limits, see [ARCHITECTURE.md](../ARCHITECTURE.md).

## Workspace map

| Location | Responsibility |
| --- | --- |
| `src/` (`mount-rs-core`) | `FsDriver`/`FileHandle`, paths, errors, types, `Loopback`, and the metadata, block, chunker and versioning contracts. Core has no runtime dependency on a concrete provider, filesystem, or transport. |
| `providers/` | One crate per backend: memory, SQLite, PGlite, TiDB, FoundationDB, Cloudflare R2, RustFS and AWS S3. The shared object-store block adapter has its own crate. |
| `filesystems/` | One crate per `FsDriver` implementation or composition: memfs, chunked, host, KV, persisted snapshots, versioned views, and SQLite/PGlite/R2 snapshot facades. |
| `transports/` | One crate per protocol or native bridge: FUSE, 9P, NFS, WebDAV, incoming S3 gateway, HTTP, auto-mount and the macOS FSKit bridge. |
| `bindings/` | Node N-API, JavaScript virtual-filesystem adapters and the SQLite VFS bridge. |
| `crates/` | Rust SDK, observability wrapper and fault-injection test support. |
| `apps/` | Rust CLI and documentation site. |
| `tests/`, `examples/`, `benchmarks/` | Contract, parity, provider, native and live-service verification plus consumer examples and measurements. Some `tests/` crates are standalone gates outside the root Cargo workspace. |
| `docs/` and root Markdown files | Design, acceptance, rollout and operations records. |

The root `Cargo.toml` lists the active workspace members. The FSKit bridge and
standalone acceptance crates have their own manifests and lockfiles outside
that workspace. Adding a new provider, filesystem or transport should create a
crate in its corresponding group and update consumers, scripts and CI filters.

## Dependency direction

Representative runtime dependency edges point from a consumer to what it imports:

```text
Providers, filesystems and transports -> mount-rs-core contracts
ChunkedFs -> MetadataStore and BlockStore trait objects
Rust SDK and Node N-API -> concrete providers, filesystems and transports
Rust CLI -> Rust SDK and chosen transports
```

Each provider imports the core contracts and owns its database or cloud SDK.
`ChunkedFs` depends on the provider *traits*, not concrete stores. A transport
accepts an `FsDriver` and should not own a second filesystem model. The CLI
constructs filesystems through the SDK. The Node N-API crate currently composes
several concrete providers and transports directly; the FSKit bridge also
constructs some concrete filesystems for its native host. A provider exposed in
those frontends needs explicit selection and shutdown mapping as well as SDK
construction.
`mount-rs-rustfs` provides signed, path-style immutable blocks through the
shared `mount-rs-object-store-blocks` adapter. It has no `MetadataStore`;
concurrent mounts on different hosts pair it with FoundationDB or PGlite
revision-CAS metadata.
SQLite, PGlite and R2 providers retain their older `*Fs` factories for
existing callers. New filesystem consumers should import the matching
`mount-rs-*-fs` crate under `filesystems/`; those facades join a provider store
with the generic `PersistedFs` implementation.
Test-only dependencies can point in the opposite direction (core parity tests
use transports, and R2 tests use the S3 gateway). Keep storage SDKs, network
protocols, Node bindings and platform mount code out of core runtime dependencies;
see [DEPENDENCIES.md](../DEPENDENCIES.md).

## Rust import migration

| Previous import | New import | Reason |
| --- | --- | --- |
| `mount_rs_core::{MemoryFs, MemoryOptions}` | `mount_rs_memfs::{MemoryFs, MemoryOptions}` | Memfs now owns its implementation; core stays upstream of filesystems. |
| `mount_rs_r2::AwsS3Config` | `mount_rs_aws_s3::AwsS3Config` | AWS S3 has its own provider crate. Use `AwsS3BlockStore` for AWS blocks. |
| Provider `SqliteFs`, `PgliteFs`, `R2Fs` factories | `mount_rs_sqlite_fs`, `mount_rs_pglite_fs`, `mount_rs_r2_fs` | New filesystem construction belongs to the facade crates; the older provider entry points remain for existing callers. |

## Extension recipes

### Metadata or block provider

1. Add a focused crate under `providers/` and scope its constructor to one
   volume or object prefix. Implement `MetadataStore`, `BlockStore`, or both
   from `mount-rs-core::storage`. Keep provider-specific dependencies there.
2. For metadata, return an uninitialized state as revision zero with no
   namespace; validate loaded namespaces. In the default exclusive-writer
   mode, use provider time or a qualified shared lease-time authority, never
   a requesting client's wall clock. Atomically check the expected revision,
   current fence and lease expiry when publishing. A provider that supports
   concurrent writers must additionally implement `concurrent_mode_state`,
   `prepare_bound_concurrent_mode` and `publish_bound_if_revision`. The persisted
   `MRC2` mode binds every publication to the block provider's stable authority
   ID. Prepare the mode before opening the namespace, fence legacy lease
   operations, and reject conversion while an exclusive writer may exist.
   An existing `MRC1` volume requires explicit offline migration that verifies
   every referenced block before the metadata mode changes. A revision
   conflict must be a known non-commit (`EAGAIN`);
   an uncertain publication error must remain ambiguous and fail closed.
   SQLite performs the mode transition and revision CAS in immediate
   transactions on one local database file; PGlite uses atomic statements
   through one PostgreSQL-wire server that may use TCP or a Unix socket;
   FoundationDB publishes its metadata chunks and
   manifest in a transaction. A PGlite/PGlite split store opens metadata and
   block connections in each CLI, so two CLIs need at least four server slots.
   Namespace records contain attributes and block references, not file bytes.
3. For blocks, store immutable bytes under stable identities, reject an
   identity reused for different bytes, and make `flush` cover completed puts.
   Every block backing used with concurrent writers must implement
   `prepare_concurrent_backing` and read-only `verify_concurrent_backing`; the
   defaults reject participation. `ChunkedFs` reads the metadata mode first.
   An established `MRC2` volume verifies the existing block marker without
   creating one. A fresh volume claims the block ID, binds metadata to it,
   and verifies it before root publication. It verifies again after the block
   flush and before every metadata CAS. A rejected block path must leave
   metadata in its original writer mode. Forward these calls and the direct
   `get_for_migration` read through every type-erased, telemetry, fault and
   N-API block adapter.
   An arbitrary injected object-store client cannot establish sharedness;
   its adapter stays in the default rejecting path even if a caller declares
   durability. Named remote provider crates construct signed clients from
   validated configs and probe create/read access under their block prefix
   before the metadata mode changes. The reserved immutable object marker is
   `<prefix>/_mount-rs-backing-id-v2`; it is separate from content blocks and
   is excluded from reconciliation. Arbitrary S3-compatible endpoints need
   their own conditional-Create and read-back qualification.
   Concurrent writers must use the same block backing so a reference
   published by one writer can be read by the others. Memory blocks are
   rejected. Local SQLite blocks are allowed only with local SQLite metadata
   when every process uses the same file paths on one host; those database
   files must stay outside every mountpoint and off NFS/SMB. The SQLite block
   marker binds its ID to a Unix physical file stamp; other platforms reject
   concurrent SQLite blocks until they can establish the same invariant. PGlite,
   FoundationDB, R2, RustFS and AWS S3
   can provide blocks reachable by independent hosts. RustFS is block-only;
   pair it with PGlite or FoundationDB metadata for cross-host mounts.
   Deletion/reconciliation needs authoritative roots and an explicit grace
   period; it is not part of ordinary unlink or shutdown.
4. Make `durable()` reflect the actual configured store. Wire selectable
   providers into the SDK and any consumer frontend that exposes them. Add
   contract/provider-matrix tests; qualify remote services separately with
   live identity and failure evidence.

### Filesystem

Add a crate under `filesystems/` that implements `FsDriver` from core. Keep
filesystem state, handles and path semantics there. Accept provider traits
when persistence is needed. A composition should own its writer lifecycle and
expose an explicit shutdown path. Run loopback contract tests before mounting
it through a transport. To support several writable NFS views, implement
`FsDriver::guarded_read` and `FsDriver::guarded_mutation`, and advertise both
`supports_guarded_reads()` and `supports_guarded_mutations()`.
Guarded stat, lookup, readdir and readlink check an opaque handle's original
inode and return data from one state lock or namespace snapshot. A directory
can be renamed remotely and its old path replaced between handle resolution
and a path-only `lookup` or `readdir`; the read must validate the parent inode
at the read boundary, before binding returned entries to handles.
Check a handle's `dev:ino` identity and any observed directory entry inside
the same state lock or backend commit as the mutation, including every retry.
Return the committed inode and handle from creation/open; opening the path
again after commit can bind a replacement. Advertise `stable_inode_ids()` only
when deleted inode numbers cannot be reused during the driver's lifetime, so
an NFS handle kept before a remote rename can bind its new alias safely.
`MemoryFs` and `ChunkedFs` implement this contract. `HostFs` and the legacy
SQLite snapshot facade remain single-view NFS filesystems.

### Transport

Add an adapter over `Arc<dyn FsDriver>` in `transports/`. Translate protocol
flags, errors and paths at that boundary. Gate operations on advertised
capabilities, preserve `syncfs` and handle-close semantics, and bound remotely
supplied bodies and directory
responses. Own listener or native-mount shutdown in the adapter, and close it
before the underlying filesystem. Run protocol tests without a kernel mount
where possible; use separate native tests for mount claims. The shared NFSv3
profile binds opaque file handles to backend inode identity, verifies opened
handles, resolves renamed aliases, and sends handle-derived reads and
mutations through the driver's guarded API. The session's
`shared_concurrent_view` option enables those semantics and implies WCC
omission. The independent `omit_wcc_attributes` option can omit WCC in an
ordinary view. The CLI sets both for shared views because another server may
commit before the reply.
Automatic transport selection must choose NFS for a shared view; routing it
through FUSE or 9P would skip this shared NFS identity and cache profile.
NFSv4 requests fail closed in this profile.
The backing SQLite provider files must remain on local disk. An application
SQLite database stored *inside* two shared NFS views is a separate locking
problem: the [macOS adversarial packet](../tests/sqlite_nfs_adversarial.md)
observed a competing `BEGIN IMMEDIATE` lock bypass between views and a WAL
request falling back to DELETE. The one-host `sqlite_single_host()` NFS
profile is for one mount and cannot be combined with shared views.

### Frontend

For a Rust consumer, use `mount-rs-sdk::Filesystem` for construction, expose
its `Arc<dyn FsDriver>` to a chosen transport or `Loopback`, and await
`Filesystem::shutdown()` after transports close. A new configuration-driven
frontend can keep parsing and presentation in its own crate, with provider
selection and cleanup in the SDK. Node bindings currently have their own
construction path, so a new provider exposed there also needs explicit N-API
mapping and shutdown coverage.

## Invariants to preserve

- By default, `ChunkedFs::open` acquires a provider-enforced writer lease,
  loads and validates metadata, and initializes an empty namespace through
  fenced publication. Ownership loss stops that instance. The optional
  experimental concurrent mode opens without a long writer lease on SQLite,
  PGlite or FoundationDB metadata. It first checks the persisted writer mode
  and block authority. Fresh volumes enter bound `MRC2` and initialize a
  namespace through an ID-bound revision CAS. `MRC2` reopens verify the block
  marker without recreating it. `MRC1` volumes require the explicit
  `migrate-concurrent-backing` command while old mounts are stopped. Existing
  exclusive-writer state needs an offline migration. Memory
  metadata and the legacy SQLite/PGlite snapshot facades remain single-writer.
- Writes complete new immutable blocks and the block barrier before publishing
  their references. Exclusive mode checks both revision and writer fence;
  concurrent mode checks both backing ID and expected revision in the provider's atomic
  publication (SQLite transaction, PGlite update, or FoundationDB transaction
  containing metadata chunks and manifest). The latter
  reloads the authoritative namespace before operations and can rebuild an
  operation after a known `EAGAIN` conflict, within a bounded retry count.
  Ambiguous publication or barrier failures fail closed; they cannot be
  replayed as known non-commits. Local namespace changes and related side
  effects follow successful publication. Reopen reads the provider's
  authoritative revision.
- Metadata publication and its barrier complete before success is acknowledged.
  `syncfs` remains an explicit filesystem-wide barrier. The composed driver
  advertises durable writes only when **both** providers do. Concurrent mode
  disables automatic atime publication and block reconciliation until
  distributed open-handle pins and safe reclamation are available; it retains
  detached file tombstones for remote open handles. Detached tombstones and
  immutable blocks staged for failed or conflicted publications currently have
  no built-in retention cap, so storage can grow without bound. The mode is
  pending a distributed handle-pin and capacity protocol. Local macOS native
  cases have passed for two writable SQLite, PGlite and FoundationDB CLIs,
  and one CLI with two writable SQLite, PGlite, FoundationDB or memory mounts.
  PGlite's two-CLI case used TCP loopback to one engine. SQLite's DELETE and
  WAL backing smoke checks passed, but WAL is unqualified with the bundled
  SQLite 3.46 until its rare
  [WAL-reset bug](https://www.sqlite.org/wal.html#the_wal_reset_bug) is fixed.
  Cross-host physical acceptance remains outstanding.
- Shared NFS views require a driver that atomically guards handle identity
  during both namespace reads and mutations. A directory handle must identify
  its original parent while looking up or listing entries under one snapshot.
  Inode identities cannot be reused if opaque
  handles are to survive a remote rename before this server sees its new path.
  `MemoryFs` checks its locked state, and `ChunkedFs` checks an authoritative
  namespace snapshot for reads and every revision-CAS retry for mutations.
  Frontend wrappers must forward guarded-read, guarded-mutation and stable-inode
  capabilities and the corresponding calls.
- Namespace format version is currently `1`. Future versions fail with
  `ENOTSUP`; malformed supported data fails validation. Each file layout stores
  its chunker configuration, so changing a new-file default cannot reinterpret
  old extents. A new on-disk format needs an explicit compatibility/migration
  design.
- Coordinator shutdown releases the lease in exclusive mode. Concurrent mode
  has no lease to release. Consumer facades then close provider resources they
  own; servers and native mounts close first. Unreferenced blocks can survive
  failed writes. Reclamation is an explicit maintenance operation in exclusive
  mode, never implicit cleanup on shutdown, and is unsupported in concurrent
  mode until its distributed safety protocol exists.

## Provider crate versus S3-compatible service

`mount-rs-r2`, `mount-rs-rustfs` and `mount-rs-aws-s3` are **outgoing**
object-store block providers. They select Cloudflare R2, RustFS and AWS S3
clients respectively over a shared immutable object-block adapter.
`mount-rs-s3` is the opposite direction: an
**incoming** path-style HTTP gateway exposing one or more `FsDriver` values as
S3-compatible buckets. None of these crates by itself is an
operated, qualified S3-compatible service. Gateway rootless tests exercise
implemented HTTP/S3 behavior, while provider unit tests exercise adapter
behavior.
Qualification of a named service additionally needs live endpoint and bucket
identity, credentialed client tests, the selected metadata-provider pairing,
durability and failure evidence, and deployment controls such as TLS and
observability. See the [S3 gateway contract](../transports/mount-rs-s3/README.md)
and [AWS S3 rollout boundary](aws-s3-production-rollout.md).
