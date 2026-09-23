//! Cloudflare R2 immutable blocks.
//!
//! The generic object-store adapter owns immutable publication and scoped
//! reconciliation. This facade owns the R2 client construction and keeps the
//! legacy R2BlockStore API available to callers.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::Result;
use mount_rs_core::storage::{BlockId, BlockReconcileReport, BlockStore};
use mount_rs_object_store_blocks::{ObjectStoreBlockStore, probe_configured_concurrent_prefix};
use object_store::ObjectStore;

pub use mount_rs_object_store_blocks::{
    ObjectStoreBlockStoreErrorClass as R2BlockStoreErrorClass,
    ObjectStoreBlockStoreStats as R2BlockStoreStats,
};

/// Immutable blocks in one R2 or S3-compatible object-store prefix.
#[derive(Clone)]
pub struct R2BlockStore(ObjectStoreBlockStore, Option<Arc<dyn ObjectStore>>);

impl R2BlockStore {
    /// Wrap an existing client with an explicitly declared durability level.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        prefix: impl Into<String>,
        durable: bool,
    ) -> Result<Self> {
        Ok(Self(
            ObjectStoreBlockStore::new(store, prefix, durable)?,
            None,
        ))
    }

    /// Build durable blocks using this provider's S3-compatible R2 client.
    pub fn from_config(config: &crate::R2Config, prefix: impl Into<String>) -> Result<Self> {
        Self::from_config_with_durable(config, prefix, true)
    }

    /// Build blocks with a validated signed client and caller-declared
    /// durability. Concurrent startup probes the configured service.
    pub fn from_config_with_durable(
        config: &crate::R2Config,
        prefix: impl Into<String>,
        durable: bool,
    ) -> Result<Self> {
        let mut blocks = Self::new(config.build_store()?, prefix, durable)?;
        blocks.1 = Some(config.build_probe_store()?);
        Ok(blocks)
    }

    pub fn prefix(&self) -> &str {
        self.0.prefix()
    }

    pub fn stats(&self) -> R2BlockStoreStats {
        self.0.stats()
    }
}

#[async_trait]
impl BlockStore for R2BlockStore {
    fn durable(&self) -> bool {
        self.0.durable()
    }

    async fn prepare_concurrent_mode(&self) -> Result<()> {
        match &self.1 {
            Some(probe) => probe_configured_concurrent_prefix(probe.as_ref(), self.prefix()).await,
            None => self.0.prepare_concurrent_mode().await,
        }
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.0.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.0.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.0.delete(id).await
    }

    async fn reconcile(
        &self,
        live: &BTreeSet<BlockId>,
        grace: Duration,
    ) -> Result<BlockReconcileReport> {
        self.0.reconcile(live, grace).await
    }
}
