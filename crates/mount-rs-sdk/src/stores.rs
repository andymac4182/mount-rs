//! Type-erased provider adapters and optional operation telemetry.

use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "observability")]
use crate::Telemetry;
use async_trait::async_trait;
use mount_rs_core::Result;
use mount_rs_core::storage::{
    BlockId, BlockReconcileReport, BlockStore, LoadedMetadata, MetadataStore, Namespace,
    WriterLease,
};

#[derive(Clone)]
pub(crate) struct ErasedMetadataStore {
    inner: Arc<dyn MetadataStore>,
    #[cfg(feature = "observability")]
    telemetry: Telemetry,
}

impl ErasedMetadataStore {
    #[cfg(feature = "observability")]
    pub(crate) fn new(inner: Arc<dyn MetadataStore>, telemetry: Telemetry) -> Self {
        Self { inner, telemetry }
    }

    #[cfg(not(feature = "observability"))]
    pub(crate) fn new(inner: Arc<dyn MetadataStore>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl MetadataStore for ErasedMetadataStore {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    fn publish_includes_flush_barrier(&self) -> bool {
        self.inner.publish_includes_flush_barrier()
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs("provider.metadata", "load", None, self.inner.load())
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.load().await
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "lease.acquire",
                    None,
                    self.inner.acquire_writer(owner, ttl),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.acquire_writer(owner, ttl).await
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "lease.renew",
                    None,
                    self.inner.renew_writer(lease, ttl),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.renew_writer(lease, ttl).await
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "lease.release",
                    None,
                    self.inner.release_writer(lease),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.release_writer(lease).await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "publish",
                    None,
                    self.inner.publish(expected_revision, lease, namespace),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner
            .publish(expected_revision, lease, namespace)
            .await
    }

    async fn flush(&self) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs("provider.metadata", "flush", None, self.inner.flush())
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.flush().await
    }
}

#[derive(Clone)]
pub(crate) struct ErasedBlockStore {
    inner: Arc<dyn BlockStore>,
    #[cfg(feature = "observability")]
    telemetry: Telemetry,
}

impl ErasedBlockStore {
    #[cfg(feature = "observability")]
    pub(crate) fn new(inner: Arc<dyn BlockStore>, telemetry: Telemetry) -> Self {
        Self { inner, telemetry }
    }

    #[cfg(not(feature = "observability"))]
    pub(crate) fn new(inner: Arc<dyn BlockStore>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl BlockStore for ErasedBlockStore {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        #[cfg(feature = "observability")]
        {
            let count = bytes.len() as u64;
            let result = self
                .telemetry
                .observe_fs("provider.blocks", "put", None, self.inner.put(bytes))
                .await?;
            self.telemetry.record_bytes("write", count);
            return Ok(result);
        }
        #[cfg(not(feature = "observability"))]
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        #[cfg(feature = "observability")]
        {
            let result = self
                .telemetry
                .observe_fs("provider.blocks", "get", None, self.inner.get(id))
                .await?;
            self.telemetry.record_bytes("read", result.len() as u64);
            return Ok(result);
        }
        #[cfg(not(feature = "observability"))]
        self.inner.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs("provider.blocks", "flush", None, self.inner.flush())
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs("provider.blocks", "delete", None, self.inner.delete(id))
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.delete(id).await
    }

    async fn reconcile(
        &self,
        live: &std::collections::BTreeSet<BlockId>,
        grace: Duration,
    ) -> Result<BlockReconcileReport> {
        #[cfg(feature = "observability")]
        {
            let report = self
                .telemetry
                .observe_fs(
                    "provider.blocks",
                    "reconcile",
                    None,
                    self.inner.reconcile(live, grace),
                )
                .await?;
            self.telemetry.record_reconcile(&report);
            return Ok(report);
        }
        #[cfg(not(feature = "observability"))]
        self.inner.reconcile(live, grace).await
    }
}
