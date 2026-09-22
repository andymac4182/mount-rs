# SQLite snapshot filesystem

`mount-rs-sqlite-fs` joins `SqliteStore` from the SQLite provider with the
generic `PersistedFs` filesystem. Database access remains in
`mount-rs-sqlite`; filesystem operations remain in `mount-rs-persist`.

```rust
let filesystem = mount_rs_sqlite_fs::open_sqlite("state.db").await?;
```

Use `open_sqlite_memory()` for a volatile SQLite database.
