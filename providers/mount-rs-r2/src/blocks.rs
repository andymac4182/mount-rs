//! Cloudflare R2 immutable blocks.
//!
//! The generic object-store adapter owns immutable publication and scoped
//! reconciliation. This facade owns the R2 client construction and keeps the
//! legacy R2BlockStore API available to callers.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::storage::{BlockId, BlockReconcileReport, BlockStore, ConcurrentBackingId};
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_object_store_blocks::{
    ObjectStoreBlockStore, generate_private_qualification_prefix, prepare_configured_backing_id,
    probe_configured_concurrent_prefix, prove_two_configured_clients, verify_configured_backing_id,
};
use object_store::ObjectStore;

pub use mount_rs_object_store_blocks::{
    ObjectStoreBlockStoreErrorClass as R2BlockStoreErrorClass,
    ObjectStoreBlockStoreStats as R2BlockStoreStats,
};

/// Evidence that two separately built signed clients claimed one marker on
/// the exact configured Cloudflare R2 service and bucket. This value is only
/// issued after a live conditional-Create race and direct read-back.
#[derive(Clone)]
pub struct R2ConcurrentQualification {
    endpoint: String,
    bucket: String,
}

impl R2ConcurrentQualification {
    /// Probe an actual canonical Cloudflare R2 endpoint. A custom
    /// S3-compatible gateway needs its own separately reviewed service gate.
    pub async fn prove_live_cloudflare_r2(config: &crate::R2Config) -> Result<Self> {
        let endpoint = canonical_cloudflare_r2_endpoint(config)?.to_owned();
        config.validate()?;
        let prefix = generate_private_qualification_prefix("mount-rs-concurrent-qualification-v2")?;
        let first_data = config.build_store()?;
        let second_data = config.build_store()?;
        let first_probe = config.build_probe_store()?;
        let second_probe = config.build_probe_store()?;
        prove_two_configured_clients(first_data, second_data, first_probe, second_probe, &prefix)
            .await?;
        Ok(Self {
            endpoint,
            bucket: config.bucket.clone(),
        })
    }
}

fn canonical_cloudflare_r2_endpoint(config: &crate::R2Config) -> Result<&str> {
    let Some(account) = config
        .endpoint
        .strip_prefix("https://")
        .and_then(|host| host.strip_suffix(".r2.cloudflarestorage.com"))
    else {
        return Err(unqualified_r2());
    };
    if account.len() != 32
        || !account
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(unqualified_r2());
    }
    Ok(&config.endpoint)
}

fn unqualified_r2() -> FsError {
    FsError::new(ErrorCode::Enotsup)
        .with_syscall("R2 concurrent backing")
        .with_message(
            "R2 concurrent backing requires a live-qualified canonical Cloudflare R2 service",
        )
}

/// Immutable blocks in one R2 or S3-compatible object-store prefix.
#[derive(Clone)]
pub struct R2BlockStore(
    ObjectStoreBlockStore,
    Option<Arc<dyn ObjectStore>>,
    Option<R2ConcurrentQualification>,
);

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
            None,
        ))
    }

    /// Build durable blocks using this provider's S3-compatible R2 client.
    /// Concurrent backing stays unavailable until an exact service and bucket
    /// have passed the separate live qualification gate.
    pub fn from_config(config: &crate::R2Config, prefix: impl Into<String>) -> Result<Self> {
        Self::from_config_with_durable(config, prefix, true)
    }

    /// Build blocks with a validated signed client and caller-declared
    /// durability. This ordinary constructor does not issue a concurrent
    /// backing qualification.
    pub fn from_config_with_durable(
        config: &crate::R2Config,
        prefix: impl Into<String>,
        durable: bool,
    ) -> Result<Self> {
        let mut blocks = Self::new(config.build_store()?, prefix, durable)?;
        blocks.1 = Some(config.build_probe_store()?);
        Ok(blocks)
    }

    /// Construct a concurrent-capable client only after the exact Cloudflare
    /// endpoint and bucket passed a live two-client Create/read-back gate.
    pub fn from_qualified_config(
        config: &crate::R2Config,
        prefix: impl Into<String>,
        qualification: &R2ConcurrentQualification,
    ) -> Result<Self> {
        Self::from_qualified_config_with_durable(config, prefix, true, qualification)
    }

    /// Build a qualified client with caller-declared durability.
    pub fn from_qualified_config_with_durable(
        config: &crate::R2Config,
        prefix: impl Into<String>,
        durable: bool,
        qualification: &R2ConcurrentQualification,
    ) -> Result<Self> {
        let endpoint = canonical_cloudflare_r2_endpoint(config)?;
        if endpoint != qualification.endpoint || config.bucket != qualification.bucket {
            return Err(FsError::new(ErrorCode::Estale)
                .with_message("R2 qualification belongs to another endpoint or bucket"));
        }
        let mut blocks = Self::from_config_with_durable(config, prefix, durable)?;
        blocks.2 = Some(qualification.clone());
        Ok(blocks)
    }

    fn require_qualification(&self) -> Result<()> {
        self.2.as_ref().map(|_| ()).ok_or_else(unqualified_r2)
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

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        self.require_qualification()?;
        match &self.1 {
            Some(probe) => {
                probe_configured_concurrent_prefix(probe.as_ref(), self.prefix()).await?;
                prepare_configured_backing_id(probe.as_ref(), &self.0).await
            }
            None => self.0.prepare_concurrent_backing().await,
        }
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        self.require_qualification()?;
        match &self.1 {
            Some(probe) => verify_configured_backing_id(probe.as_ref(), &self.0, expected).await,
            None => self.0.verify_concurrent_backing(expected).await,
        }
    }

    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.0.get_for_migration(id).await
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

#[cfg(test)]
mod qualification_tests {
    use super::*;
    use futures_util::stream::BoxStream;
    use mount_rs_core::ErrorCode;
    use object_store::memory::InMemory;
    use object_store::path::Path as ObjectPath;
    use object_store::{
        GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, PutMode,
        PutMultipartOptions, PutOptions, PutPayload, PutResult,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct SimulatedCreateStore {
        inner: Arc<InMemory>,
        marker_create_calls: AtomicUsize,
        non_atomic: bool,
    }

    impl std::fmt::Display for SimulatedCreateStore {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("SimulatedCreateStore")
        }
    }

    #[async_trait]
    impl ObjectStore for SimulatedCreateStore {
        async fn put_opts(
            &self,
            location: &ObjectPath,
            payload: PutPayload,
            options: PutOptions,
        ) -> object_store::Result<PutResult> {
            if location.as_ref().ends_with("/_mount-rs-backing-id-v2")
                && options.mode == PutMode::Create
            {
                self.marker_create_calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                if self.non_atomic {
                    return self.inner.put(location, payload).await;
                }
            }
            self.inner.put_opts(location, payload, options).await
        }

        async fn put_multipart_opts(
            &self,
            location: &ObjectPath,
            options: PutMultipartOptions,
        ) -> object_store::Result<Box<dyn MultipartUpload>> {
            self.inner.put_multipart_opts(location, options).await
        }

        async fn get_opts(
            &self,
            location: &ObjectPath,
            options: GetOptions,
        ) -> object_store::Result<GetResult> {
            self.inner.get_opts(location, options).await
        }

        async fn delete(&self, location: &ObjectPath) -> object_store::Result<()> {
            self.inner.delete(location).await
        }

        fn list(
            &self,
            prefix: Option<&ObjectPath>,
        ) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
            self.inner.list(prefix)
        }

        async fn list_with_delimiter(
            &self,
            prefix: Option<&ObjectPath>,
        ) -> object_store::Result<ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }

        async fn copy(&self, from: &ObjectPath, to: &ObjectPath) -> object_store::Result<()> {
            self.inner.copy(from, to).await
        }

        async fn copy_if_not_exists(
            &self,
            from: &ObjectPath,
            to: &ObjectPath,
        ) -> object_store::Result<()> {
            self.inner.copy_if_not_exists(from, to).await
        }
    }

    fn config(endpoint: &str, bucket: &str) -> crate::R2Config {
        crate::R2Config {
            endpoint: endpoint.to_owned(),
            bucket: bucket.to_owned(),
            access_key_id: "qualification-test-access".to_owned(),
            secret_access_key: "qualification-test-secret".to_owned(),
            state_key: "qualification/state.json".to_owned(),
        }
    }

    #[test]
    fn qualification_is_tied_to_the_exact_cloudflare_endpoint_and_bucket() {
        let canonical = format!("https://{}.r2.cloudflarestorage.com", "a".repeat(32));
        let qualified_config = config(&canonical, "qualified-bucket");
        let token = R2ConcurrentQualification {
            endpoint: canonical.clone(),
            bucket: qualified_config.bucket.clone(),
        };
        assert!(
            R2BlockStore::from_qualified_config(&qualified_config, "qualified/blocks", &token)
                .is_ok()
        );
        let another_bucket = config(&canonical, "other-bucket");
        assert!(
            R2BlockStore::from_qualified_config(&another_bucket, "qualified/blocks", &token)
                .err()
                .unwrap()
                .is(ErrorCode::Estale)
        );
        for gateway in [
            "https://gateway.example.com",
            "https://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.r2.cloudflarestorage.com:443",
            "https://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.r2.cloudflarestorage.com/path",
            "http://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.r2.cloudflarestorage.com",
        ] {
            assert!(
                R2BlockStore::from_qualified_config(
                    &config(gateway, "qualified-bucket"),
                    "qualified/blocks",
                    &token
                )
                .err()
                .unwrap()
                .is(ErrorCode::Enotsup)
            );
        }
    }

    #[tokio::test]
    async fn two_signed_client_gate_reads_one_winner_and_cleans_only_its_marker() {
        let backing = Arc::new(InMemory::new());
        let store = Arc::new(SimulatedCreateStore {
            inner: backing.clone(),
            marker_create_calls: AtomicUsize::new(0),
            non_atomic: false,
        });
        let prefix = generate_private_qualification_prefix("owned-qualification/blocks").unwrap();
        prove_two_configured_clients(
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            &prefix,
        )
        .await
        .unwrap();
        assert_eq!(store.marker_create_calls.load(Ordering::SeqCst), 2);
        let marker = prefix.marker_path();
        assert!(matches!(
            backing.get(&marker).await,
            Err(object_store::Error::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn qualification_requires_both_create_requests_to_overlap() {
        let backing = Arc::new(InMemory::new());
        let prefix =
            generate_private_qualification_prefix("sequential-qualification/blocks").unwrap();
        assert!(
            prove_two_configured_clients(
                backing.clone(),
                backing.clone(),
                backing.clone(),
                backing.clone(),
                &prefix,
            )
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup),
            "an immediate first completion must not qualify a sequential probe"
        );
    }

    #[tokio::test]
    async fn qualification_does_not_delete_a_preexisting_marker() {
        let backing = Arc::new(InMemory::new());
        let prefix =
            generate_private_qualification_prefix("preexisting-qualification/blocks").unwrap();
        let marker = prefix.marker_path();
        let seeded = b"another owner's marker".to_vec();
        backing
            .put(&marker, PutPayload::from(seeded.clone()))
            .await
            .unwrap();
        assert!(
            prove_two_configured_clients(
                backing.clone(),
                backing.clone(),
                backing.clone(),
                backing.clone(),
                &prefix,
            )
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup)
        );
        assert_eq!(
            backing.get(&marker).await.unwrap().bytes().await.unwrap(),
            seeded
        );
    }

    #[tokio::test]
    async fn qualification_rejects_a_service_that_overwrites_on_create() {
        let inner = Arc::new(InMemory::new());
        let store = Arc::new(SimulatedCreateStore {
            inner: inner.clone(),
            marker_create_calls: AtomicUsize::new(0),
            non_atomic: true,
        });
        let prefix =
            generate_private_qualification_prefix("non-atomic-qualification/blocks").unwrap();
        assert!(
            prove_two_configured_clients(
                store.clone(),
                store.clone(),
                store.clone(),
                store.clone(),
                &prefix,
            )
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup)
        );
        assert_eq!(store.marker_create_calls.load(Ordering::SeqCst), 2);
        let marker = prefix.marker_path();
        assert!(matches!(
            inner.get(&marker).await,
            Err(object_store::Error::NotFound { .. })
        ));
    }
}
