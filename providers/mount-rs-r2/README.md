# mount-rs R2 provider

This crate owns the Cloudflare R2 and S3-compatible client configuration,
versioned snapshot storage, and the R2 immutable block facade. `R2Config`
validates an endpoint, bucket, credentials, and snapshot key before
`build_store` creates a signed client. `R2Store` implements the core snapshot
contract. `R2BlockStore::from_config` builds durable blocks under a selected
prefix; `R2BlockStore::new` accepts an existing client and an explicit
durability declaration.

The shared `mount-rs-object-store-blocks` crate implements block publication
and reconciliation. The `mount-rs-r2-fs` filesystem crate composes `R2Store`
with `PersistedFs`. The older `R2Fs`, `open_r2`, and `open_object_store` exports
remain here as compatibility entry points.
