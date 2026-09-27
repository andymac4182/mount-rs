# SQLite split-store providers

`SqliteMetadataStore` and `SqliteBlockStore` keep metadata and immutable file
blocks independently selectable. Their existing `open(path)` constructors
preserve the database's journal mode and set `synchronous=FULL` (`2`). A fresh
file therefore uses DELETE; reopening an existing WAL file retains WAL.

Select WAL explicitly at construction:

```rust
use mount_rs_sqlite::{SqliteBlockStore, SqliteJournalMode, SqliteStorageOptions};

let blocks = SqliteBlockStore::open_with_options(
    "state/blocks.sqlite",
    SqliteStorageOptions { journal_mode: SqliteJournalMode::Wal },
)?;
```

The metadata constructor accepts the same options. The SDK reexports these
options and accepts `StoreConfig::SqliteWithOptions { path, options }`, while
existing `StoreConfig::Sqlite { path }` callers retain their behavior.

WAL requires a qualified local Linux or macOS file on the same host. Empty,
`:memory:` and `file:` URI paths are refused. Existing physical-file,
single-hard-link, local-filesystem, mount-root and auxiliary-path authority
guards still apply; canonical symlinks are supported. Both persisted roles
are validated before changing a shared file's journal mode. WAL is persistent
and can create `-wal` and `-shm` sidecars: retain SQLite's complete live file
set and use SQLite-aware backup procedures.

FULL synchronization and the existing SQLite checkpoint policy remain in
force. Blocks commit durably before the separate metadata publication commit.
The option adds no per-request pragmas and does not alter the legacy whole
snapshot SQLite driver or in-memory providers. The retained small performance
fixture is not production-capacity or power-loss durability qualification.
