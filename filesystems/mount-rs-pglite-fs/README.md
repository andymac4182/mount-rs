# PGlite snapshot filesystem

`mount-rs-pglite-fs` joins `PgliteStore` from the PGlite provider with the
generic `PersistedFs` filesystem. PostgreSQL-wire access remains in
`mount-rs-pglite`; filesystem operations remain in `mount-rs-persist`.

```rust
let filesystem = mount_rs_pglite_fs::connect_pglite(connection_string).await?;
```

Use `connect_pglite_with_store()` when the owner needs a store handle for
explicit connection shutdown.
