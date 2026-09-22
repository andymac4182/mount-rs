# Code architecture

This page maps the source and shows where an extension belongs. For the storage
commit protocol and its acceptance limits, see [ARCHITECTURE.md](../ARCHITECTURE.md).

## Workspace map

| Location | Responsibility |
| --- | --- |
| `src/` (`mount-rs-core`) | `FsDriver`/`FileHandle`, paths, errors, types, `Loopback`, and the metadata, block, chunker and versioning contracts. Core has no runtime dependency on a concrete provider, filesystem, or transport. |
| `providers/` | One crate per backend: memory, SQLite, PGlite, TiDB, FoundationDB, Cloudflare R2 and AWS S3. The shared object-store block adapter has its own crate. |
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
   namespace; validate loaded namespaces. Use provider time or a qualified
   shared lease-time authority, never a requesting client's wall clock, and
   atomically check the expected revision, current fence and lease expiry when
   publishing. Namespace records contain attributes and block references, not
   file bytes.
3. For blocks, store immutable bytes under stable identities, reject an
   identity reused for different bytes, and make `flush` cover completed puts.
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
it through a transport.

### Transport

Add an adapter over `Arc<dyn FsDriver>` in `transports/`. Translate protocol
flags, errors and paths at that boundary. Gate operations on advertised
capabilities, preserve `syncfs` and handle-close semantics, and bound remotely
supplied bodies and directory
responses. Own listener or native-mount shutdown in the adapter, and close it
before the underlying filesystem. Run protocol tests without a kernel mount
where possible; use separate native tests for mount claims.

### Frontend

For a Rust consumer, use `mount-rs-sdk::Filesystem` for construction, expose
its `Arc<dyn FsDriver>` to a chosen transport or `Loopback`, and await
`Filesystem::shutdown()` after transports close. A new configuration-driven
frontend can keep parsing and presentation in its own crate, with provider
selection and cleanup in the SDK. Node bindings currently have their own
construction path, so a new provider exposed there also needs explicit N-API
mapping and shutdown coverage.

## Invariants to preserve

- `ChunkedFs::open` acquires a provider-enforced writer lease, loads and
  validates metadata, and initializes an empty namespace through fenced
  publication. Ownership loss and ambiguous publication/barrier failures stop
  that instance; reopen reads the provider's authoritative revision.
- Writes complete new immutable blocks and the block barrier before publishing
  their references with revision CAS and the writer fence. Metadata publication
  and its barrier complete before success is acknowledged. `syncfs` remains an
  explicit filesystem-wide barrier. The composed driver advertises durable
  writes only when **both** providers do.
- Namespace format version is currently `1`. Future versions fail with
  `ENOTSUP`; malformed supported data fails validation. Each file layout stores
  its chunker configuration, so changing a new-file default cannot reinterpret
  old extents. A new on-disk format needs an explicit compatibility/migration
  design.
- Coordinator shutdown releases the writer lease; consumer facades then close
  provider resources they own. Servers and native mounts close first.
  Unreferenced blocks can survive failed writes.
  Reclamation is an explicit, fenced maintenance operation, never implicit
  cleanup on shutdown.

## Provider crate versus S3-compatible service

`mount-rs-r2` and `mount-rs-aws-s3` are **outgoing** object-store providers.
They select Cloudflare R2 and AWS S3 clients respectively over a shared
immutable object-block adapter. `mount-rs-s3` is the opposite direction: an
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
