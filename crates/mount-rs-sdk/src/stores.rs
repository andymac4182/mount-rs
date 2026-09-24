//! Type-erased provider adapters and optional operation telemetry.

use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "observability")]
use crate::Telemetry;
use async_trait::async_trait;
use mount_rs_core::Result;
use mount_rs_core::storage::{
    BlockId, BlockReconcileReport, BlockStore, CheckoutRequest, ConcurrentBackingId,
    ConcurrentModeState, DelegatedCheckin, DelegatedPublish, DelegatedRecovery, DelegationState,
    DirectoryGrant, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::versioning::VolumeId;

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
    async fn delegation_state(&self) -> Result<Option<DelegationState>> {
        self.inner.delegation_state().await
    }

    async fn prepare_delegated_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        self.inner
            .prepare_delegated_mode(backing, expected_revision)
            .await
    }

    async fn checkout(&self, request: &CheckoutRequest) -> Result<DirectoryGrant> {
        self.inner.checkout(request).await
    }

    async fn publish_delegated(
        &self,
        request: &DelegatedPublish,
        namespace: Namespace,
    ) -> Result<u64> {
        self.inner.publish_delegated(request, namespace).await
    }

    async fn checkin(&self, request: &DelegatedCheckin) -> Result<()> {
        self.inner.checkin(request).await
    }

    async fn recover(&self, request: &DelegatedRecovery) -> Result<()> {
        self.inner.recover(request).await
    }

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

    async fn load_if_changed(&self, known_revision: u64) -> Result<Option<LoadedMetadata>> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "load",
                    None,
                    self.inner.load_if_changed(known_revision),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.load_if_changed(known_revision).await
    }

    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "concurrent.mode",
                    None,
                    self.inner.concurrent_mode_state(),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.concurrent_mode_state().await
    }

    async fn preflight_new_bound_mode(&self) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "concurrent.preflight_new",
                    None,
                    self.inner.preflight_new_bound_mode(),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.preflight_new_bound_mode().await
    }

    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "concurrent.prepare_bound",
                    None,
                    self.inner.prepare_bound_concurrent_mode(backing),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.prepare_bound_concurrent_mode(backing).await
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

    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "concurrent.publish_bound",
                    None,
                    self.inner
                        .publish_bound_if_revision(backing, expected_revision, namespace),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner
            .publish_bound_if_revision(backing, expected_revision, namespace)
            .await
    }

    async fn migrate_mrc1_to_bound_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "concurrent.migrate",
                    None,
                    self.inner
                        .migrate_mrc1_to_bound_mode(backing, expected_revision),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner
            .migrate_mrc1_to_bound_mode(backing, expected_revision)
            .await
    }

    async fn preflight_mrc1_to_bound_mode(&self, expected_revision: u64) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "concurrent.preflight_mrc1",
                    None,
                    self.inner.preflight_mrc1_to_bound_mode(expected_revision),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner
            .preflight_mrc1_to_bound_mode(expected_revision)
            .await
    }

    async fn preflight_trusted_unstamped_mrc1(
        &self,
        expected_revision: u64,
        expected_volume: VolumeId,
    ) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "concurrent.preflight_trusted_mrc1",
                    None,
                    self.inner
                        .preflight_trusted_unstamped_mrc1(expected_revision, expected_volume),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner
            .preflight_trusted_unstamped_mrc1(expected_revision, expected_volume)
            .await
    }

    async fn migrate_trusted_unstamped_mrc1(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        expected_volume: VolumeId,
    ) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "concurrent.migrate_trusted_mrc1",
                    None,
                    self.inner.migrate_trusted_unstamped_mrc1(
                        backing,
                        expected_revision,
                        expected_volume,
                    ),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner
            .migrate_trusted_unstamped_mrc1(backing, expected_revision, expected_volume)
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

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.blocks",
                    "concurrent.prepare_backing",
                    None,
                    self.inner.prepare_concurrent_backing(),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.prepare_concurrent_backing().await
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.blocks",
                    "concurrent.verify_backing",
                    None,
                    self.inner.verify_concurrent_backing(expected),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.verify_concurrent_backing(expected).await
    }

    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.blocks",
                    "concurrent.migration_read",
                    None,
                    self.inner.get_for_migration(id),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.get_for_migration(id).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::chunking::ChunkerConfig;
    use mount_rs_core::{ErrorCode, FsError};
    use mount_rs_sqlite::SqliteBlockStore;
    use std::collections::BTreeMap;

    struct IdentityProbeBlockStore(ConcurrentBackingId);

    #[async_trait]
    impl BlockStore for IdentityProbeBlockStore {
        fn durable(&self) -> bool {
            true
        }

        async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
            Ok(self.0)
        }

        async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
            if expected == self.0 {
                Ok(())
            } else {
                Err(FsError::new(ErrorCode::Estale))
            }
        }

        async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
            Ok(id.0.as_bytes().to_vec())
        }

        async fn put(&self, _bytes: &[u8]) -> Result<BlockId> {
            unreachable!()
        }

        async fn get(&self, _id: &BlockId) -> Result<Vec<u8>> {
            unreachable!()
        }

        async fn flush(&self) -> Result<()> {
            unreachable!()
        }

        async fn delete(&self, _id: &BlockId) -> Result<()> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn erased_blocks_forward_backing_authority_and_direct_migration_read() {
        let id = ConcurrentBackingId::from_bytes([0x91; 16]).unwrap();
        let inner = Arc::new(IdentityProbeBlockStore(id)) as Arc<dyn BlockStore>;
        #[cfg(feature = "observability")]
        let erased = ErasedBlockStore::new(inner, Telemetry::disabled());
        #[cfg(not(feature = "observability"))]
        let erased = ErasedBlockStore::new(inner);
        assert_eq!(erased.prepare_concurrent_backing().await.unwrap(), id);
        erased.verify_concurrent_backing(id).await.unwrap();
        let different = ConcurrentBackingId::from_bytes([0x92; 16]).unwrap();
        assert_eq!(
            erased
                .verify_concurrent_backing(different)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(
            erased
                .get_for_migration(&BlockId("direct".into()))
                .await
                .unwrap(),
            b"direct"
        );
    }

    struct IdentityProbeMetadataStore(ConcurrentBackingId);

    #[async_trait]
    impl MetadataStore for IdentityProbeMetadataStore {
        fn durable(&self) -> bool {
            true
        }

        async fn load(&self) -> Result<LoadedMetadata> {
            unreachable!()
        }

        async fn load_if_changed(&self, known_revision: u64) -> Result<Option<LoadedMetadata>> {
            match known_revision {
                7 => Ok(None),
                0 => Ok(Some(LoadedMetadata {
                    revision: 0,
                    namespace: None,
                })),
                _ => Err(FsError::new(ErrorCode::Eio)),
            }
        }

        async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
            Ok(ConcurrentModeState::Mrc2(self.0))
        }

        async fn preflight_new_bound_mode(&self) -> Result<()> {
            Ok(())
        }

        async fn preflight_mrc1_to_bound_mode(&self, expected_revision: u64) -> Result<()> {
            if expected_revision == 7 {
                Ok(())
            } else {
                Err(FsError::new(ErrorCode::Eagain))
            }
        }

        async fn preflight_trusted_unstamped_mrc1(
            &self,
            expected_revision: u64,
            expected_volume: VolumeId,
        ) -> Result<()> {
            self.preflight_mrc1_to_bound_mode(expected_revision).await?;
            if expected_volume.as_str() == "selected-volume" {
                Ok(())
            } else {
                Err(FsError::new(ErrorCode::Estale))
            }
        }

        async fn migrate_trusted_unstamped_mrc1(
            &self,
            backing: ConcurrentBackingId,
            expected_revision: u64,
            expected_volume: VolumeId,
        ) -> Result<()> {
            self.preflight_trusted_unstamped_mrc1(expected_revision, expected_volume)
                .await?;
            self.prepare_bound_concurrent_mode(backing).await
        }

        async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
            if backing == self.0 {
                Ok(())
            } else {
                Err(FsError::new(ErrorCode::Estale))
            }
        }

        async fn acquire_writer(&self, _owner: &str, _ttl: Duration) -> Result<WriterLease> {
            unreachable!()
        }

        async fn renew_writer(&self, _lease: &WriterLease, _ttl: Duration) -> Result<WriterLease> {
            unreachable!()
        }

        async fn release_writer(&self, _lease: &WriterLease) -> Result<()> {
            unreachable!()
        }

        async fn publish(
            &self,
            _expected_revision: u64,
            _lease: &WriterLease,
            _namespace: Namespace,
        ) -> Result<u64> {
            unreachable!()
        }

        async fn publish_bound_if_revision(
            &self,
            backing: ConcurrentBackingId,
            expected_revision: u64,
            _namespace: Namespace,
        ) -> Result<u64> {
            if backing == self.0 {
                Ok(expected_revision + 1)
            } else {
                Err(FsError::new(ErrorCode::Estale))
            }
        }

        async fn migrate_mrc1_to_bound_mode(
            &self,
            backing: ConcurrentBackingId,
            expected_revision: u64,
        ) -> Result<()> {
            if backing == self.0 && expected_revision == 7 {
                Ok(())
            } else {
                Err(FsError::new(ErrorCode::Estale))
            }
        }

        async fn flush(&self) -> Result<()> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn erased_metadata_forwards_conditional_load_and_provider_errors() {
        let id = ConcurrentBackingId::from_bytes([0x95; 16]).unwrap();
        let inner = Arc::new(IdentityProbeMetadataStore(id)) as Arc<dyn MetadataStore>;
        #[cfg(feature = "observability")]
        let telemetry = Telemetry::new(mount_rs_observability::TelemetryConfig::enabled(
            "conditional-load-test",
        ));
        #[cfg(feature = "observability")]
        let erased = ErasedMetadataStore::new(inner, telemetry.clone());
        #[cfg(not(feature = "observability"))]
        let erased = ErasedMetadataStore::new(inner);

        assert!(erased.load_if_changed(7).await.unwrap().is_none());
        let loaded = erased.load_if_changed(0).await.unwrap().unwrap();
        assert_eq!(loaded.revision, 0);
        assert!(loaded.namespace.is_none());
        assert_eq!(
            erased.load_if_changed(8).await.unwrap_err().code,
            ErrorCode::Eio
        );
        #[cfg(feature = "observability")]
        {
            assert_eq!(telemetry.snapshot().operations, 3);
            assert_eq!(telemetry.snapshot().errors, 1);
        }
    }

    #[tokio::test]
    async fn erased_metadata_forwards_bound_mode_cas_and_migration() {
        let id = ConcurrentBackingId::from_bytes([0x93; 16]).unwrap();
        let other = ConcurrentBackingId::from_bytes([0x94; 16]).unwrap();
        let inner = Arc::new(IdentityProbeMetadataStore(id)) as Arc<dyn MetadataStore>;
        #[cfg(feature = "observability")]
        let erased = ErasedMetadataStore::new(inner, Telemetry::disabled());
        #[cfg(not(feature = "observability"))]
        let erased = ErasedMetadataStore::new(inner);
        assert_eq!(
            erased.concurrent_mode_state().await.unwrap(),
            ConcurrentModeState::Mrc2(id)
        );
        erased.preflight_new_bound_mode().await.unwrap();
        erased.preflight_mrc1_to_bound_mode(7).await.unwrap();
        assert_eq!(
            erased
                .preflight_mrc1_to_bound_mode(8)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        let volume = VolumeId::new("selected-volume").unwrap();
        erased
            .preflight_trusted_unstamped_mrc1(7, volume.clone())
            .await
            .unwrap();
        erased
            .migrate_trusted_unstamped_mrc1(id, 7, volume.clone())
            .await
            .unwrap();
        assert_eq!(
            erased
                .migrate_trusted_unstamped_mrc1(other, 7, volume)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        erased.prepare_bound_concurrent_mode(id).await.unwrap();
        assert_eq!(
            erased
                .prepare_bound_concurrent_mode(other)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        let namespace = Namespace {
            format_version: 1,
            root: 1,
            next_inode: 2,
            default_uid: 0,
            default_gid: 0,
            umask: 0,
            default_chunker: ChunkerConfig {
                algorithm: "fixed-size".into(),
                version: 1,
                parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
            },
            nodes: BTreeMap::new(),
        };
        assert_eq!(
            erased
                .publish_bound_if_revision(id, 7, namespace.clone())
                .await
                .unwrap(),
            8
        );
        assert_eq!(
            erased
                .publish_bound_if_revision(other, 7, namespace)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        erased.migrate_mrc1_to_bound_mode(id, 7).await.unwrap();
        assert_eq!(
            erased
                .migrate_mrc1_to_bound_mode(other, 7)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
    }

    #[tokio::test]
    async fn erased_blocks_forward_concurrent_backing_failure() {
        let inner = Arc::new(SqliteBlockStore::open(":memory:").unwrap()) as Arc<dyn BlockStore>;
        #[cfg(feature = "observability")]
        let erased = ErasedBlockStore::new(inner, Telemetry::disabled());
        #[cfg(not(feature = "observability"))]
        let erased = ErasedBlockStore::new(inner);
        let error = erased
            .prepare_concurrent_backing()
            .await
            .expect_err("volatile SQLite block preflight must reach the provider");
        assert_eq!(error.code, ErrorCode::Enotsup);
    }
}
