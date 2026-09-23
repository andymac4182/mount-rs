# mount-rs object-store blocks

This crate implements immutable byte blocks over any
`Arc<dyn object_store::ObjectStore>`. Create an `ObjectStoreBlockStore` with
`ObjectStoreBlockStore::new(store, prefix, durable)`. The caller declares whether
the chosen backend is durable; the adapter does not infer that from the client
type.

The generic injected adapter rejects `prepare_concurrent_backing` because an
arbitrary client and durability declaration cannot prove that two mounts use
the same service. Configured signed providers probe Create/read access, then
claim an immutable `_mount-rs-backing-id-v2` object under the block prefix.
It holds the `MRC2` header and the 16-byte backing authority ID. Established
mounts verify this marker directly without recreating it; block reconciliation
ignores it. RustFS uses a signed client. R2 requires an explicit live
independent-client conditional-Create/conflict/read-back qualification for its
configured endpoint and bucket before it can claim a concurrent backing.

The adapter validates its prefix and block IDs, uses conditional creation for
content-addressed blocks, tracks bounded diagnostics, and reconciles only
objects inside its configured prefix. AWS S3 and R2 own their client setup in
their own provider crates. Filesystem composition and namespace metadata live
outside this crate.
