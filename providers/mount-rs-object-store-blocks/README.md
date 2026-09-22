# mount-rs object-store blocks

This crate implements immutable byte blocks over any
`Arc<dyn object_store::ObjectStore>`. Create an `ObjectStoreBlockStore` with
`ObjectStoreBlockStore::new(store, prefix, durable)`. The caller declares whether
the chosen backend is durable; the adapter does not infer that from the client
type.

The adapter validates its prefix and block IDs, uses conditional creation for
content-addressed blocks, tracks bounded diagnostics, and reconciles only
objects inside its configured prefix. AWS S3 and R2 own their client setup in
their own provider crates. Filesystem composition and namespace metadata live
outside this crate.
