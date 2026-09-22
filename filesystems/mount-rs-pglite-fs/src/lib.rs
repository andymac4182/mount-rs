//! Snapshot-backed PGlite filesystem composition.
//!
//! The PGlite provider owns the PostgreSQL-wire client; `mount-rs-persist`
//! owns the filesystem semantics. Owners can retain a store handle to close
//! its connection deterministically during shutdown.

use mount_rs_core::Result;
use mount_rs_persist::PersistedFs;
use mount_rs_pglite::PgliteStore;

pub type PgliteFs = PersistedFs<PgliteStore>;

/// Open a filesystem using the default snapshot key.
pub async fn connect_pglite(connection_string: &str) -> Result<PgliteFs> {
    let (filesystem, _store) = connect_pglite_with_store(connection_string, "mount-rs").await?;
    Ok(filesystem)
}

/// Open a filesystem with a named snapshot key.
pub async fn connect_pglite_with_key(
    connection_string: &str,
    state_key: impl Into<String>,
) -> Result<PgliteFs> {
    let (filesystem, _store) = connect_pglite_with_store(connection_string, state_key).await?;
    Ok(filesystem)
}

/// Open a filesystem and return the provider handle for explicit shutdown.
pub async fn connect_pglite_with_store(
    connection_string: &str,
    state_key: impl Into<String>,
) -> Result<(PgliteFs, PgliteStore)> {
    let store = PgliteStore::connect_with_key(connection_string, state_key).await?;
    let filesystem = PersistedFs::open(store.clone()).await?;
    Ok((filesystem, store))
}
