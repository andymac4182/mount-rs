# Concurrent SQLite, PGlite, and RustFS volumes

## Goal and supported topology

`ChunkedFs` is the shared writable filesystem. One CLI can expose several NFS
mountpoints from one driver. Independent CLIs need a metadata provider that can
atomically publish a namespace only when its expected revision remains current,
and all CLIs must read the same immutable block backing.

| Metadata | Blocks | Independent CLI scope |
| --- | --- | --- |
| Local SQLite file | The same local SQLite block file or a shared remote block provider | Same host only; files are on local storage, outside every mount |
| One PGlite socket server | The same PGlite volume or a shared remote block provider | Same or separate hosts connecting to that one server |
| FoundationDB revision CAS | The same RustFS bucket and prefix | Same or separate hosts reaching the same services |
| PGlite revision CAS | The same RustFS bucket and prefix | Same or separate hosts reaching the same services |

RustFS is an outgoing immutable block provider in its own crate, using its S3
API and the shared object-block adapter. It is not the metadata authority.
RustFS-only snapshot filesystems and the older SQLite/PGlite snapshot facades
remain single view because they keep private in-memory state and cannot merge
another writer's committed revision. Direct cross-host access to an SQLite
database file is outside this design. A future SQLite owner service would be a
separate provider and transport design.

## Provider contract and upgrade safety

Each new concurrent metadata provider implements the existing
`MetadataStore::prepare_concurrent_mode` and `publish_if_revision` methods.
Preparation is idempotent only for a matching prepared state. It may convert
only a fresh metadata row with no namespace, revision, lease history, or live
version records. Existing volumes require an offline migration. The provider
atomically stores an `MRC1` mode marker and sets the signed lease fence to
`i64::MAX`. Old SQLite and PGlite lease-acquire SQL already requires a fence
below that value, so an old writer cannot obtain a new lease after preparation.
New lease operations and lease-fenced publications also reject the marker.

`publish_if_revision` validates the namespace and updates the revision and
namespace in one conditional SQL statement requiring the mode marker, sentinel
fence, no lease owner, and the expected revision. A zero-row update returns
`EAGAIN` only after a read proves the mode is intact and the revision differs.
Unexpected state and database or socket failures remain ambiguous and fail
closed; they must not be replayed as a known noncommit. `ChunkedFs` reloads
before guarded reads and mutations and rebuilds an operation after a proven
revision conflict within its existing retry bound. It completes new blocks
before publishing their references.

The public SDK, CLI JSON, and Node binding accept concurrent SQLite metadata
only with durable local paths, and SQLite blocks only when metadata is SQLite.
Concurrent PGlite and FoundationDB metadata reject memory and local SQLite
blocks. The current FoundationDB concurrent config remains valid. Every CLI
must use matching metadata identity and block path, volume key, or bucket and
prefix. The current core contract does not persist a block-backing identity;
this is an operational prerequisite and an explicit follow-up protocol, not an
inferred guarantee from matching metadata alone.

## Native mount and SQLite file behavior

Shared views use the existing NFSv3 guarded inode identity profile. The CLI
selects that profile for `concurrent_writes` and `--also-mountpoint`. MacOS
clients need the tested no-attribute-cache and negative-name-cache settings so
Finder can refresh directory changes. `--sqlite-single-host` is a different
NFS client lock profile for SQLite database files *inside* a mount. It stays
incompatible with shared views: two NFS servers do not share its local lock
table, even if the mount-rs backing database is SQLite.

The SQLite provider begins with its existing rollback/DELETE journal and FULL
synchronous setting. WAL qualification requires a SQLite version containing
the 2026 WAL-reset fix. SQLite files hosted inside a mount are stressed
separately in DELETE, TRUNCATE, PERSIST, and WAL modes, with competing handles,
reopen, and crash recovery. The tests use disposable files and should record
where NFS semantics reject a mode rather than treating an expected failure as
a successful concurrent-volume result.

## Qualification and capacity limits

Provider tests cover a racing legacy lease acquisition, mode-marker mismatch,
known CAS loss, unexpected zero-row updates, load/reopen, and immutable blocks.
Native tests cover two independent CLIs, bidirectional file creation, disjoint
writes to one file, rename and unlink with retained handles, fresh reopen, and
actual Finder-visible NFS behavior where a Mac is available. Load tests count
acknowledged operations, conflicts, errors, elapsed time, stored blocks, and
metadata size. The test report separates one-CLI, two-CLI same-host, and
cross-host evidence. A network-accessible PGlite socket server may multiplex
clients, but the deployed server version and connection limit need validation.

Every concurrent provider retains detached file tombstones and staged
immutable blocks. Automatic reconciliation stays disabled because there is no
distributed open-handle pin or safe online collector. Load tests measure this
growth; bounded reclamation and persistent block-backing identity are separate
follow-up designs. A local test does not establish cross-host or production
readiness.
