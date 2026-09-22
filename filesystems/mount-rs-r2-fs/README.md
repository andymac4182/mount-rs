# R2 snapshot filesystem

`mount-rs-r2-fs` joins `R2Store` from the R2 provider with the generic
`PersistedFs` filesystem. Cloudflare R2 configuration and object access remain
in `mount-rs-r2`; filesystem operations remain in `mount-rs-persist`.

```rust
let filesystem = mount_rs_r2_fs::open_r2(config).await?;
```

`open_object_store()` accepts any existing `ObjectStore` implementation,
including S3-compatible clients and in-memory stores.
