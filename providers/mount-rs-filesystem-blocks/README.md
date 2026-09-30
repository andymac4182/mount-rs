# Filesystem immutable blocks

This block-only provider composes with metadata providers such as TiDB. It
stores no namespace or Drive configuration. The caller supplies an existing,
effective-user-owned directory with mode 0700 and a physical path whose
components contain no symlinks. `open` never creates the root.

```rust,ignore
let blocks = FilesystemBlockStore::open("/srv/mount-rs/drive-42/blocks", true)?;
```

Linux and macOS are supported. Other targets return `ENOTSUP`. A Tokio runtime
is required for asynchronous `BlockStore` methods; an absent runtime returns a
filesystem error. The synchronous constructor initializes and syncs the marker
before returning. Async operations copy borrowed input into an owned blocking
task and retain descriptors until completion even if the awaiting task is
cancelled. This ownership copy is one allocation per put and is included in
instrumented benchmark observations.

## Format and immutable publication

Block IDs are `b` followed by 64 lowercase SHA256 hex digits. Files are stored
under the first two digest characters, for example `ab/bab...`. Put and get
payloads are limited to 128 MiB. A corrupt file length is checked before payload
allocation. Reads directly verify the digest; there is no provider RAM cache.

A new object is written to an exclusively created private staging file. Its
file barrier completes before Linux `renameat2(RENAME_NOREPLACE)` or macOS
`renameatx_np(RENAME_EXCL)` publishes its final name. Unsupported no-replace
semantics fail closed; there is no replacing-rename fallback. A duplicate put
reads and checks both its complete bytes and digest, then completes the winner's
file barrier and the shard/root directory barriers before acknowledgment. Files
must be regular, effective-user-owned, mode 0600 and have one hardlink. Directory
and file opens are relative to owned descriptors and use `O_NOFOLLOW`; file
opens also use `O_NONBLOCK` before checking their type.

Each put completes `sync_all` on the file, shard directory, and root directory.
macOS also completes `F_FULLFSYNC` on regular files before publication and once
more on the retained winning file descriptor after the directory syncs, so the
new directory entries precede the final device barrier. Barrier errors reach the
caller. `flush` rechecks authority and syncs the root; every prior successful
put has already completed its file and directory barriers. `durable` is the
caller assertion about the deployed filesystem and failure domain; setting it
false does not skip these barriers or enable weaker acknowledgment semantics.
Actual power-loss recovery and remote/network filesystem behavior need separate
qualification. This local benchmark does not prove either.

## Stable backing authority

The root contains `_mount-rs-backing-id-v1`, a 20-byte `MFB1` plus random UUID
marker. Independently opened contexts share this persisted identity. A root
directory lock serializes initialization; both new and existing marker openers
complete file, root-directory, immediate-parent-directory, and final device
barriers. The root and its immediate parent must reside on the same filesystem.
The caller must durably provision any newly created ancestor directories; this
provider does not create or sync an arbitrary path of ancestors. An empty caller-created root can
initialize, while a nonempty root missing its marker fails `ESTALE`.

Every operation verifies the marker content and its device/inode identity, and
reopens the physical root path before and after acknowledgment. An established
context fails `ESTALE` if its root or marker is removed/replaced, or the root
loses its private ownership/mode. A completely emptied former root is
indistinguishable from a new root; it receives a new UUID, which does not match
metadata bound to the former authority. Symlink aliases and foreign hardlinks
are rejected. The root is private to the operating user; another process with
the same credentials can still delete or alter data and is outside the durable
storage trust boundary. Content and authority checks detect ordinary corruption.

## Deployment and maintenance

All servers sharing TiDB metadata for one Drive must access the same underlying
block root and persisted marker. Independent server-local roots are different
authorities and cannot substitute for a shared blob store. A single-machine
multi-server filesystem comparison can isolate SDK/HTTP/RustFS overhead; it
does not establish a multi-host production deployment topology.

Deletion and reconciliation return `ENOTSUP` in this first version. Successful
shutdown does not imply garbage collection. Complete immutable objects left by
failed or cancelled metadata publication remain available for later scoped
reclamation. Temporary-file removal verifies its named regular-file inode before
unlink; a substituted or otherwise ambiguous name is retained.
