//! Snapshot-backed SQLite filesystem composition.
//!
//! The SQLite provider owns database access; `mount-rs-persist` owns the
//! filesystem semantics. This crate joins them for filesystem consumers.

use std::path::Path;

use mount_rs_core::Result;
use mount_rs_persist::PersistedFs;
use mount_rs_sqlite::SqliteStore;

pub type SqliteFs = PersistedFs<SqliteStore>;

/// Open a filesystem backed by a SQLite database file.
pub async fn open_sqlite(path: impl AsRef<Path>) -> Result<SqliteFs> {
    PersistedFs::open(SqliteStore::open(path)?).await
}

/// Open a volatile SQLite-backed filesystem for tests and local use.
pub async fn open_sqlite_memory() -> Result<SqliteFs> {
    PersistedFs::open(SqliteStore::in_memory()?).await
}
