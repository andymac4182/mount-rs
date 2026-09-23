# mount-rs object-store blocks

This crate implements immutable byte blocks over any
`Arc<dyn object_store::ObjectStore>`. Create an `ObjectStoreBlockStore` with
`ObjectStoreBlockStore::new(store, prefix, durable)`. The caller declares whether
the chosen backend is durable; the adapter does not infer that from the client
type.

The generic injected adapter rejects `prepare_concurrent_mode` because an
arbitrary client and durability declaration cannot prove that two mounts use
the same service. Configured R2, AWS S3, and RustFS providers use the shared
`probe_configured_concurrent_prefix` before their metadata stores enable
concurrent publication. It conditionally creates and directly reads one stable
`_mount-rs-concurrent-probe-v1` object under the block prefix. That object is
ignored by block reconciliation and stays in the prefix until an operator
explicitly removes it.

The adapter validates its prefix and block IDs, uses conditional creation for
content-addressed blocks, tracks bounded diagnostics, and reconciles only
objects inside its configured prefix. AWS S3 and R2 own their client setup in
their own provider crates. Filesystem composition and namespace metadata live
outside this crate.
