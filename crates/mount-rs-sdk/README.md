# mount-rs Rust SDK

`mount-rs-sdk` is the public Rust consumer facade. It keeps filesystem
construction and provider lifecycle management out of applications and the
configuration-driven CLI while leaving each backend in its own integration
crate.

The CLI is an SDK consumer: `mount-rs-cli` creates a `Filesystem` through this
crate and only owns argument/configuration parsing plus transport lifecycle.

The SDK source is organized by responsibility: `options.rs` defines public
provider choices, `filesystem.rs` owns construction and shutdown,
`providers.rs` opens built-in stores and closes their resources, and
`stores.rs` adapts those stores to the common contract with optional telemetry.
`lib.rs` keeps the existing root API available. Applications implementing their
own metadata and block stores can compose them directly with
`mount-rs-chunked::ChunkedFs::open`; the SDK's built-in `StoreConfig` choices
remain a closed list. Direct composition also leaves provider-specific resource
closure with the caller after `ChunkedFs::shutdown()` releases its writer lease.

```rust
use mount_rs_core::Loopback;
use mount_rs_sdk::{Filesystem, MemoryOptions};

# #[tokio::main]
# async fn main() -> mount_rs_core::Result<()> {
let filesystem = Filesystem::memory(MemoryOptions::default());
let view = Loopback::from_arc(filesystem.driver());
view.write_file("/hello.txt", b"hello from Rust").await?;
assert_eq!(view.read_file("/hello.txt").await?, b"hello from Rust");
filesystem.shutdown().await?;
# Ok(())
# }
```

Use SplitOptions when metadata and byte storage need different providers.
The SDK owns the provider handles and closes PGlite connections after the
chunked filesystem has released its writer lease.

FoundationDB is an opt-in native feature:

~~~toml
mount-rs-sdk = { version = "0.1", features = ["foundationdb"] }
~~~

StoreConfig::FoundationDb opens the configured cluster file and requires an
explicit `FoundationDbLeaseAuthority` choice. Use
`PersistedSingleAuthority` only for an owned single-authority/test cluster;
independent production writers should use `SharedProvider` with the protected
authority prefix published by a separate authority service. The persisted
choice does not provide that shared time authority.
The default SDK build remains portable and returns ENOTSUP if a FoundationDB
store is selected without the native feature or on an unsupported target.

For opt-in `SplitOptions::with_concurrent_writes(true)`, choose SQLite,
PGlite, or FoundationDB revision-CAS metadata. SQLite metadata and block
files must be durable local files on the same host, with the same paths in
every process; do not place either database on NFS or another network file
system. PGlite needs one reachable server endpoint shared by all processes or
hosts. R2, RustFS, AWS S3, PGlite, and FoundationDB can supply immutable
blocks shared by remote writers. Local SQLite blocks are accepted only with
local SQLite metadata; memory blocks are rejected for this mode.

`StoreConfig::RustFs` is the separately named RustFS block provider. Supply
its endpoint, signing region, bucket, prefix, and injected credentials; it
does not provide the metadata revision authority. Concurrent mode retains
tombstones and failed publication blocks until a distributed pin and safe
reclamation protocol exists, so monitor storage growth on long-running
volumes.

TiDB TLS is also opt-in so the default graph remains small and portable:

~~~toml
mount-rs-sdk = { version = "0.1", features = ["rustls"] }
~~~

Use a TiDB connection URL with `require_ssl=true` for a TLS-required endpoint.
The deployment remains responsible for the URL, certificate roots, secret
injection and a credentialed handshake test; compiling the feature alone is
not provider or production acceptance.

## Optional observability

Enable the `observability` feature when the application wants the SDK to
decorate a returned driver with the separate `mount-rs-observability` crate:

```toml
mount-rs-sdk = { version = "0.1", features = ["observability"] }
```

Call `driver_with_telemetry` with an application-owned handle, or install a
handle with `set_global_telemetry` and call `observed_driver`. The default
handle is disabled and exporter setup remains owned by the application.

Explicit `exclusive` and `shared` ownership modes are available for chunked split stores. Exclusive mode enables deferred publication with durable synchronization; omitted modes preserve legacy behavior. See [mount ownership contracts and configuration](../../docs/mount-ownership.md).

For directory ownership, use `.with_checkout_path("/tenant")` with durable supported metadata and shared blocks. This selects Shared and claims the directory before filesystem construction returns. Direct clients can instead open Shared without a path, then call `checkout_scope`, `delegation_status` and `checkin_scope`. File bytes require a grant, and checkin requires closed handles.

Existing initialized volumes must stop all mounts before `Filesystem::enroll_directory_ownership(options, expected_revision)`. `directory_ownership_state` inspects persisted grants; `recover_directory_ownership(options, root_inode, expected_fence)` explicitly retires a crashed owner. A newer grant invalidates an older recovery fence. Native ownership transfer requires unmount/remount; initial qualification is Linux FUSE.
## TiDB and local block caching

`StoreConfig::Tidb { connection, volume_key, durable }` selects the TiDB
provider for metadata or blocks. TiDB TLS support is compiled in; supply the
appropriate TLS options in the connection URL for your endpoint. The SDK
releases the writer lease before disconnecting its TiDB pools.

Use `Filesystem::split_with_runtime(options, policies)` to opt into
`SplitRuntimeOptions { atime_policy: AtimePolicy::NoAtime,
block_cache: Some(BlockCacheOptions { max_bytes, max_entries }) }`.
Both cache limits must be positive. `filesystem.block_cache_stats().await`
reports hits, misses, evictions and occupancy. The cache is process-local RAM,
scoped to that backing store, populated after backend writes and disposable.
It neither adds durability nor substitutes for metadata/block flushes. Noatime
and uncached blocks are the defaults through `Filesystem::split`: reads do not
update access timestamps. Set `atime_policy: AtimePolicy::Strict` explicitly if
synchronous access-time updates are required; with TiDB this adds metadata
publication to file reads and directory listings.

TiDB plus `StoreConfig::Memory` blocks is ephemeral: reconnecting a new process
cannot recover those file bytes. SQLite blocks are a local-file-backed option;
RustFS uses the existing R2/S3-compatible block configuration. Raw local object
store benchmarks do not establish a durable SDK local-directory block backend.

**Mount ownership:** this driver still acquires an exclusive volume-wide
writer lease for its lifetime. Independent SDK instances cannot concurrently
open the same volume. The new cache does not establish distributed metadata
coherence or multi-writer support. Multiple mounts per volume are an active
production requirement, not a verified capability of these options.

Real-service consumer test (ignored by default):

```sh
MOUNT_RS_TIDB_URL='mysql://...' \
  ./scripts/cargo-shared test -p mount-rs-sdk --test tidb --locked -- --ignored --nocapture
```

Set `MOUNT_RS_SDK_RUSTFS=1` only inside the repository's isolated RustFS harness
to include RustFS reopen/cache checks. The default covers memory and local
SQLite blocks; it creates and deletes process/time-unique TiDB metadata rows.

### Experimental owner serving window

The SDK `SplitRuntimeOptions.read_lease_cache_window` (CLI storage field
`read_lease_cache_window_ms`) defaults to 60 seconds, paired with
`SplitRuntimeOptions.lease_ttl` (CLI `lease_ttl_ms`) defaulting to 240 seconds.
Background renewal runs every 30 seconds. A zero window explicitly restores
per-operation renewal. Nonzero windows must be at least 20ms and at most one
quarter of the configured lease. Resident reads use the owner serving window.
After a crash, takeover may wait roughly four minutes for the authoritative
lease to expire; route TTL does not shorten that ownership lease.
Metadata publications also reuse the acknowledged token while that window is
valid, avoiding a separate renewal transaction before each publication. Every
publication still validates owner, fence, expiry and revision at the provider;
publication success does not extend the local serving window.
This requires managed provider clocks without abrupt forward expiry jumps or
out-of-band early revocation. It requires a Tokio runtime and exclusive
ownership; the `split_with_*` runtime constructors reject concurrent writes and
shared ownership. `read_parallelism` must be 1. Renewal failures or unknown outcomes
fail the owner closed. This is an experimental owner-service optimization, not
an implementation of multi-server discovery or multi-client cache coherence.

The block provider `{"kind":"local","path":"/existing/owned/block-directory"}`
uses immutable content-addressed files with file and directory synchronization
on Unix. Provision the directory durably before use. It must be application-owned;
local disk contents are not automatically replicated to another owner server.

### Sharing TiDB connections across volumes

A server can own one `TidbStorePool::new(url, max_connections)` and select
`StoreConfig::TidbShared { pool: pool.clone(), volume_key, durable }` for each
volume's metadata and/or blocks. Volume keys remain independent. The shared pool
has a hard connection cap, zero minimum idle count and a 30-second idle retention
window. Its equality is owner identity; debug output does not expose credentials.

Call `Filesystem::shutdown()` for every volume before `pool.close().await`.
Filesystem shutdown releases that volume's lease but does not disconnect the
shared pool. Explicit pool close affects every volume created from that owner.
Independent `StoreConfig::Tidb` configurations keep their existing per-store
pool lifetime. Sharing is explicit; no credentials are used as global cache keys.

### Persistent disk cache below RAM

Use `Filesystem::split_with_disk_cache(split, runtime, DiskCacheOptions { root,
scope, max_bytes, max_entries })` to opt into owner-local disk content caching.
The existing `runtime.block_cache` remains an independent RAM tier above it.
The root must already exist and be application-owned. Scope is a non-secret
identity for the backing volume **and generation**; change it when backing data
is recreated or a provider can reuse old block IDs. In particular, a newly
created memory backing store needs a new scope. Do not share one scope between
different volumes or independent cache instances.

`disk_cache_stats().await?` returns the optional disk-tier counters;
`clear_disk_cache().await?` clears disk copies only. The existing
`clear_block_cache().await` continues to clear RAM only. To audit fully cold
application caches, quiesce requests and clear both tiers. Clearing RAM alone
allows measurement of disk hits. These methods do not clear OS caches.

Shutdown releases provider resources and the writer lease. Drop all filesystem
and driver/handle clones afterward to release the disk scope's exclusive file
lock. Disk persistence is acceleration, not authoritative durability: provider
writes and flushes still determine acknowledgment. Cache invalidation is synced
before authoritative deletion. See the [host cache documentation](../../integrations/mount-rs-host/README.md)
for failure semantics, supported platforms and per-cache resource limits.

For multiple cached volumes in one server, create a `DiskCacheBudget` once and
pass clones to `Filesystem::split_with_disk_cache_budget`. It shares disk worker
and temporary fill-copy admission across volumes. Existing
`split_with_disk_cache` keeps private admission per volume. Cache capacity and
identity remain per volume; shutting down one volume does not close the shared
budget or other volumes. This does not bound resident bytes across all volumes.

For opt-in server-wide local mutation admission, create one `LocalWriteBudget`
and clone it into each `StoreConfig::LocalWithWriteBudget { path, budget }`.
The budget limits active blocking puts/deletes across volumes; reads bypass it.
Queued requests are not bounded by this API. `StoreConfig::Local` retains its
existing unrestricted behavior. Budget equality uses shared owner identity.

### Explicit shared S3 clients

Applications hosting several volumes with the same S3 endpoint, bucket and
credentials may create a `mount_rs_r2::SharedR2Client::from_config(&config)`
once, then clone it into `StoreConfig::R2Shared { client, prefix, durable }`.
Each volume keeps its own validated block prefix and caller-declared durability.
Client equality compares shared instance identity, and Debug redacts its
configuration. There is no global credential-keyed client registry. The final
client/store reference owns the connection-pool lifetime. This is block-only;
it cannot be selected as a metadata provider. Existing `StoreConfig::R2`
continues constructing independent clients. A local RustFS A/B showed no consistent latency improvement; sharing is not
a recommended latency default. See the read-audit client-sharing results.

Opening a split filesystem with `blocks: StoreConfig::Memory` creates a new, empty authoritative block store. If the committed namespace already references file blocks, SDK startup returns ESTALE after acquiring and releasing the writer lease. A warm cache cannot establish that the new memory store contains the original data. Directories, empty files and entirely sparse files can reopen because they have no stored extents. Preserve the original authoritative contents or deliberately initialize a fresh volume to recover from process loss.


When reopening TiDB metadata with local-file or S3-compatible blocks, the SDK
reads one referenced block directly from the authoritative provider and checks
that it covers the sampled extent. A populated disk cache cannot mask an empty
replacement root or bucket prefix. Missing or short data returns `ESTALE`;
other provider failures retain their error codes. Reopen also renews the writer
lease before serving. The probe adds at most one block GET at startup. A store
with partial data can pass this sample, so operators still need to preserve
the volume's storage identity across owner replacements.
