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
filesystem error. The synchronous constructor initializes and verifies the marker
before returning. Async operations copy borrowed input into an owned blocking
task and retain descriptors until completion even if the awaiting task is
cancelled. This ownership copy is one allocation per put and is included in
instrumented benchmark observations.

## Format and immutable publication

Block IDs are `b` followed by 64 lowercase SHA256 hex digits. Files are stored
under the first two digest characters, for example `ab/bab...`. Put and get
payloads are limited to 128 MiB. A corrupt file length is checked before payload
allocation. Reads directly verify the digest; there is no provider RAM cache.

A new object is completely written to an exclusively created private staging
file before Linux `renameat2(RENAME_NOREPLACE)` or macOS
`renameatx_np(RENAME_EXCL)` publishes its final name. Unsupported no-replace
semantics fail closed; there is no replacing-rename fallback. A duplicate put
reads and checks both its complete bytes and digest before acknowledgment. Files
must be regular, effective-user-owned, mode 0600 and have one hardlink. Directory
and file opens are relative to owned descriptors and use `O_NOFOLLOW`; file
opens also use `O_NONBLOCK` before checking their type.

## OS writeback and persistence

The operating system flushes data and directory changes normally. The provider
never calls `sync_all`, `fsync` or `F_FULLFSYNC`, including during marker
initialization, new object publication and duplicate publication. `flush`
performs two authority checks in an owned blocking task; it does not request
stable-storage synchronization or drain concurrent PUT workers.

`durable()` always returns false. Acknowledgment means a complete immutable
object is visible through the filesystem, not that it has reached stable
storage. OS crashes or power loss can lose acknowledged blobs or their backing
marker, even when TiDB metadata has committed. This pairing does not provide
power-loss-safe writes.

The constructor's `persistent` argument asserts that the deployed backing is
available after owned shutdown and provider/process reopen while the OS and
storage remain alive. It changes the capability used for healthy runtime
retirement; it does not change writeback behavior. Set it false for ephemeral
backing. Independent-context reopen tests verify bytes and backing authority;
they do not establish recovery from a machine or device failure.

## Stable backing authority

The root contains `_mount-rs-backing-id-v1`, a 20-byte `MFB1` plus random UUID
marker. Independently opened contexts share this persisted identity. A root
directory lock serializes initialization. New and existing marker openers verify
the marker and root/parent identities before returning. The root and its
immediate parent must reside on the same filesystem. The caller provisions
ancestor directories; this provider does not create them. An empty caller-created root can
initialize, while a nonempty root missing its marker fails `ESTALE`.

Every operation verifies the marker content and its device/inode identity, and
reopens the physical root path before and after acknowledgment. An established
context fails `ESTALE` if its root or marker is removed/replaced, or the root
loses its private ownership/mode. A completely emptied former root is
indistinguishable from a new root; it receives a new UUID, which does not match
metadata bound to the former authority. Symlink aliases and foreign hardlinks
are rejected. The root is private to the operating user; another process with
the same credentials can still delete or alter data and is outside the storage
trust boundary. Content and authority checks detect ordinary corruption.

## Deployment and maintenance

All servers sharing TiDB metadata for one Drive must access the same underlying
block root and persisted marker. Independent server-local roots are different
authorities and cannot substitute for a shared blob store. A single-machine
multi-server filesystem comparison compares the combined storage paths; it does
not isolate SDK/HTTP/RustFS overhead or establish a multi-host production topology.

Deletion and reconciliation return `ENOTSUP` in this first version. Successful
shutdown does not imply garbage collection. Complete immutable objects left by
failed or cancelled metadata publication remain available for later scoped
reclamation. Temporary-file removal verifies its named regular-file inode before
unlink; a substituted or otherwise ambiguous name is retained.
