//! Snapshot storage contract shared by persisted filesystems and providers.

use async_trait::async_trait;

use crate::{ErrorCode, FsError, Result};

/// A snapshot and the backend version that was observed with it.
///
/// The version is intentionally opaque. SQLite and PGlite use a decimal
/// revision, while object stores use their conditional-write version (usually
/// an ETag). An empty version means that the store does not provide optimistic
/// concurrency control.
#[derive(Debug, Default)]
pub struct LoadedSnapshot {
    pub snapshot: Option<Vec<u8>>,
    pub version: String,
}

/// A durable byte store for one serialized filesystem snapshot.
#[async_trait]
pub trait StateStore: Send + Sync {
    async fn load(&self) -> Result<Option<Vec<u8>>>;

    /// Load the snapshot together with an opaque version token.
    ///
    /// Existing stores that only implement `load` continue to work, but their
    /// version token is empty and concurrent independent filesystem instances
    /// cannot be protected from a stale whole-snapshot write. Stores with a
    /// native revision or conditional-write primitive should override this
    /// method and `save_versioned` together.
    async fn load_versioned(&self) -> Result<LoadedSnapshot> {
        Ok(LoadedSnapshot {
            snapshot: self.load().await?,
            version: String::new(),
        })
    }

    /// Atomically replace the snapshot and do not resolve successfully until
    /// the replacement is durable in the backend. On error, the previously
    /// committed snapshot must remain readable so a later `persist` can retry.
    async fn save(&self, snapshot: Vec<u8>) -> Result<()>;

    /// Save a snapshot only if `expected_version` is still current, returning
    /// the version of the committed snapshot. The default preserves the
    /// original `StateStore` contract for stores without version support.
    async fn save_versioned(&self, snapshot: Vec<u8>, expected_version: &str) -> Result<String> {
        self.save(snapshot).await?;
        Ok(expected_version.to_owned())
    }
}

/// Return the retryable error used when another filesystem instance committed
/// a newer snapshot first. The stale instance must be reopened or explicitly
/// reconciled before it can write again.
pub fn snapshot_conflict(backend: &str) -> FsError {
    FsError::new(ErrorCode::Eagain).with_message(format!(
        "{backend} snapshot changed concurrently; reopen before retrying"
    ))
}
