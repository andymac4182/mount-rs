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
