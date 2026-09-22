//! Snapshot-backed filesystem over an S3-compatible object store.
//!
//! `R2Config` selects a Cloudflare R2 client, while `open_object_store` accepts
//! any `ObjectStore` implementation. The provider owns object-store access;
//! `mount-rs-persist` owns the filesystem semantics. This crate gives the R2
//! snapshot filesystem its own filesystem import path.

use std::sync::Arc;

use mount_rs_core::Result;
use mount_rs_persist::PersistedFs;
pub use mount_rs_r2::R2Config;
use mount_rs_r2::R2Store;
use object_store::ObjectStore;

pub type R2Fs = PersistedFs<R2Store>;

/// Open a filesystem using a configured R2 bucket and snapshot key.
pub async fn open_r2(config: R2Config) -> Result<R2Fs> {
    PersistedFs::open(R2Store::from_config(&config)?).await
}

/// Open a filesystem using an existing object-store client.
pub async fn open_object_store(
    store: Arc<dyn ObjectStore>,
    state_key: impl Into<String>,
) -> Result<R2Fs> {
    PersistedFs::open(R2Store::new(store, state_key)).await
}
