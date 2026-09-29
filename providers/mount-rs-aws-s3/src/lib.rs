//! AWS S3 client and immutable-block provider.
//!
//! Credentials come from the standard AWS environment and workload identity
//! sources. This provider accepts AWS buckets and regions; S3-compatible
//! endpoints are configured through the R2 provider.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::storage::{BlockId, BlockReconcileReport, BlockStore, ConcurrentBackingId};
use mount_rs_core::{FsError, Result};
pub use mount_rs_object_store_blocks::RawBlockCacheBudget;
use mount_rs_object_store_blocks::{
    ObjectStoreBlockStore, ObjectStoreBlockStoreStats, prepare_configured_backing_id,
    probe_configured_concurrent_prefix, verify_configured_backing_id,
};
use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey, S3ConditionalPut};
use object_store::client::{ClientConfigKey, ClientOptions};
use object_store::{ObjectStore, RetryConfig};

/// Maximum number of internal object-store retry attempts for AWS S3.
pub const AWS_S3_MAX_RETRIES: usize = 5;
/// Maximum elapsed retry time for one AWS S3 request.
pub const AWS_S3_RETRY_TIMEOUT: Duration = Duration::from_secs(30);

/// AWS S3 configuration using environment or workload identity credentials.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AwsS3Config {
    pub bucket: String,
    pub region: String,
}

impl AwsS3Config {
    pub fn validate(&self) -> Result<()> {
        validate_bucket(&self.bucket)?;
        validate_aws_region(&self.region)?;
        Ok(())
    }

    /// Return the bounded retry policy used for AWS S3 requests.
    pub fn retry_config(&self) -> RetryConfig {
        RetryConfig {
            max_retries: AWS_S3_MAX_RETRIES,
            retry_timeout: AWS_S3_RETRY_TIMEOUT,
            ..RetryConfig::default()
        }
    }

    /// Build a signed AWS S3 client from the configured environment.
    pub fn build_store(&self) -> Result<Arc<dyn ObjectStore>> {
        self.build_store_with_probe_limits(false)
    }

    fn build_probe_store(&self) -> Result<Arc<dyn ObjectStore>> {
        self.build_store_with_probe_limits(true)
    }

    fn build_store_with_probe_limits(&self, probe: bool) -> Result<Arc<dyn ObjectStore>> {
        self.validate()?;
        let mut builder = AmazonS3Builder::from_env()
            .with_bucket_name(&self.bucket)
            .with_region(&self.region)
            .with_virtual_hosted_style_request(true)
            .with_conditional_put(S3ConditionalPut::ETagMatch);
        if probe {
            builder = builder
                .with_client_options(
                    ClientOptions::new()
                        .with_timeout(Duration::from_secs(8))
                        .with_connect_timeout(Duration::from_secs(3)),
                )
                .with_retry(RetryConfig {
                    max_retries: 1,
                    retry_timeout: Duration::from_secs(12),
                    ..RetryConfig::default()
                });
        } else {
            builder = builder.with_retry(self.retry_config());
        }
        validate_aws_builder(&builder)?;
        Ok(Arc::new(builder.build().map_err(|_| {
            FsError::backend("AWS S3 signed object-store client could not be built")
        })?))
    }
}

/// Immutable blocks in one AWS S3 prefix.
///
/// The shared object-store adapter enforces block identities, conditional
/// creation, and scoped reconciliation. This wrapper owns the AWS client
/// construction and keeps it independent of the R2 provider.
#[derive(Clone)]
pub struct AwsS3BlockStore(ObjectStoreBlockStore, Option<Arc<dyn ObjectStore>>);

impl AwsS3BlockStore {
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

    /// Wrap an existing client with an explicit raw-cache capacity owner.
    pub fn new_with_cache_budget(
        store: Arc<dyn ObjectStore>,
        prefix: impl Into<String>,
        durable: bool,
        budget: RawBlockCacheBudget,
    ) -> Result<Self> {
        Ok(Self(
            ObjectStoreBlockStore::new_with_cache_budget(store, prefix, durable, budget)?,
            None,
        ))
    }

    /// Build durable blocks using this provider's signed AWS S3 client.
    pub fn from_config(config: &AwsS3Config, prefix: impl Into<String>) -> Result<Self> {
        Self::from_config_with_durable(config, prefix, true)
    }

    /// Build blocks with a validated signed client and explicit durability.
    pub fn from_config_with_durable(
        config: &AwsS3Config,
        prefix: impl Into<String>,
        durable: bool,
    ) -> Result<Self> {
        let mut blocks = Self::new(config.build_store()?, prefix, durable)?;
        blocks.1 = Some(config.build_probe_store()?);
        Ok(blocks)
    }

    /// Build signed blocks with an explicit raw-cache capacity owner.
    pub fn from_config_with_cache_budget(
        config: &AwsS3Config,
        prefix: impl Into<String>,
        durable: bool,
        budget: RawBlockCacheBudget,
    ) -> Result<Self> {
        let mut blocks =
            Self::new_with_cache_budget(config.build_store()?, prefix, durable, budget)?;
        blocks.1 = Some(config.build_probe_store()?);
        Ok(blocks)
    }

    pub fn prefix(&self) -> &str {
        self.0.prefix()
    }

    pub fn stats(&self) -> ObjectStoreBlockStoreStats {
        self.0.stats()
    }
}

#[async_trait]
impl BlockStore for AwsS3BlockStore {
    fn durable(&self) -> bool {
        self.0.durable()
    }

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        match &self.1 {
            Some(probe) => {
                probe_configured_concurrent_prefix(probe.as_ref(), self.prefix()).await?;
                prepare_configured_backing_id(probe.as_ref(), &self.0).await
            }
            None => self.0.prepare_concurrent_backing().await,
        }
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
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

fn validate_bucket(bucket: &str) -> Result<()> {
    let valid_length = (3..=63).contains(&bucket.len());
    let valid_characters = bucket.bytes().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == b'.'
            || character == b'-'
    });
    let valid_edges = bucket
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && bucket
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric);
    if !valid_length || !valid_characters || !valid_edges || bucket.contains("..") {
        return Err(FsError::backend(
            "invalid bucket: must be a 3-63 character DNS-compatible bucket name",
        ));
    }
    Ok(())
}

fn validate_aws_region(region: &str) -> Result<()> {
    let bytes = region.as_bytes();
    let valid = !bytes.is_empty()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric);
    if !valid {
        return Err(FsError::backend(
            "invalid AWS S3 region: expected a non-empty DNS-compatible region",
        ));
    }
    Ok(())
}

fn validate_aws_builder(builder: &AmazonS3Builder) -> Result<()> {
    if builder
        .get_config_value(&AmazonS3ConfigKey::Endpoint)
        .is_some()
    {
        return Err(FsError::backend(
            "AWS S3 provider rejects custom endpoints; use the R2 provider for S3-compatible endpoints",
        ));
    }
    if builder
        .get_config_value(&AmazonS3ConfigKey::StsEndpoint)
        .is_some()
    {
        return Err(FsError::backend(
            "AWS S3 provider rejects custom STS endpoints; use the AWS STS endpoint",
        ));
    }
    if builder
        .get_config_value(&AmazonS3ConfigKey::SkipSignature)
        .as_deref()
        == Some("true")
    {
        return Err(FsError::backend("AWS S3 provider requires signed requests"));
    }
    if builder
        .get_config_value(&AmazonS3ConfigKey::Client(ClientConfigKey::AllowHttp))
        .as_deref()
        == Some("true")
    {
        return Err(FsError::backend(
            "AWS S3 provider rejects HTTP transport overrides",
        ));
    }
    if builder
        .get_config_value(&AmazonS3ConfigKey::Client(
            ClientConfigKey::AllowInvalidCertificates,
        ))
        .as_deref()
        == Some("true")
    {
        return Err(FsError::backend(
            "AWS S3 provider rejects invalid-certificate overrides",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::storage::{BlockStore, ConcurrentBackingId};
    use object_store::PutPayload;
    use object_store::memory::InMemory;
    use object_store::path::Path as ObjectPath;

    fn block_path(prefix: &str, id: &BlockId) -> ObjectPath {
        ObjectPath::from(format!("{prefix}/{}", id.0))
    }

    async fn assert_shared_budget_retains_one_facade(max_bytes: usize, max_entries: usize) {
        let backing = Arc::new(InMemory::new());
        let budget = RawBlockCacheBudget::new(max_bytes, max_entries);
        let first = AwsS3BlockStore::new_with_cache_budget(
            backing.clone(),
            "shared-budget/drive-a",
            false,
            budget.clone(),
        )
        .unwrap();
        let second = AwsS3BlockStore::new_with_cache_budget(
            backing.clone(),
            "shared-budget/drive-b",
            false,
            budget.clone(),
        )
        .unwrap();
        let first_id = first.put(b"abcd").await.unwrap();
        let second_id = second.put(b"wxyz").await.unwrap();
        first.flush().await.unwrap();
        second.flush().await.unwrap();
        for (store, id, expected) in [(&first, &first_id, b"abcd"), (&second, &second_id, b"wxyz")]
        {
            assert_eq!(
                backing
                    .get(&block_path(store.prefix(), id))
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
                    .as_ref(),
                expected
            );
        }
        // Remove only the actual backing objects. Reads now reveal which
        // independent facade retained a payload, without trusting counters.
        backing
            .delete(&block_path(first.prefix(), &first_id))
            .await
            .unwrap();
        backing
            .delete(&block_path(second.prefix(), &second_id))
            .await
            .unwrap();
        assert_eq!(first.get(&first_id).await.unwrap(), b"abcd");
        assert!(
            second
                .get(&second_id)
                .await
                .expect_err("second prefix must not retain an entry without shared credit")
                .is(mount_rs_core::ErrorCode::Enoent)
        );
        let occupied = budget.snapshot().unwrap();
        assert_eq!((occupied.entries, occupied.payload_bytes), (1, 4));
        assert!(occupied.charged_bytes <= max_bytes);
        drop(first);
        drop(second);
        assert_eq!(budget.snapshot().unwrap().entries, 0);
    }

    #[tokio::test]
    async fn public_aws_shared_budget_bounds_bytes_across_prefixed_facades() {
        assert_shared_budget_retains_one_facade(132, 8).await;
    }

    #[tokio::test]
    async fn public_aws_shared_budget_bounds_entries_across_prefixed_facades() {
        assert_shared_budget_retains_one_facade(64 * 1024, 1).await;
    }

    async fn assert_zero_budget_keeps_reads_authoritative(max_bytes: usize, max_entries: usize) {
        let backing = Arc::new(InMemory::new());
        let budget = RawBlockCacheBudget::new(max_bytes, max_entries);
        let store = AwsS3BlockStore::new_with_cache_budget(
            backing.clone(),
            "zero-budget/blocks",
            false,
            budget.clone(),
        )
        .unwrap();
        let id = store.put(b"backing authority").await.unwrap();
        store.flush().await.unwrap();
        assert_eq!(store.get(&id).await.unwrap(), b"backing authority");
        assert_eq!(
            store.get_for_migration(&id).await.unwrap(),
            b"backing authority"
        );
        backing
            .delete(&block_path(store.prefix(), &id))
            .await
            .unwrap();
        assert!(
            store
                .get(&id)
                .await
                .expect_err("zero in either limit must prevent a retained raw payload")
                .is(mount_rs_core::ErrorCode::Enoent)
        );
        let occupied = budget.snapshot().unwrap();
        assert_eq!((occupied.entries, occupied.charged_bytes), (0, 0));
    }

    #[tokio::test]
    async fn public_aws_shared_budget_zero_byte_limit_keeps_reads_authoritative() {
        assert_zero_budget_keeps_reads_authoritative(0, 8).await;
    }

    #[tokio::test]
    async fn public_aws_shared_budget_zero_entry_limit_keeps_reads_authoritative() {
        assert_zero_budget_keeps_reads_authoritative(64 * 1024, 0).await;
    }

    #[tokio::test]
    async fn public_aws_shared_budget_preserves_conditional_collision_and_migration_checks() {
        let backing = Arc::new(InMemory::new());
        let budget = RawBlockCacheBudget::new(0, 0);
        let published = AwsS3BlockStore::new_with_cache_budget(
            backing.clone(),
            "shared-budget/source",
            false,
            budget.clone(),
        )
        .unwrap();
        assert!(!published.durable());
        let body = b"immutable publication";
        let id = published.put(body).await.unwrap();
        published.flush().await.unwrap();
        assert_eq!(
            backing
                .get(&block_path(published.prefix(), &id))
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            body
        );
        let collision_prefix = "shared-budget/collision";
        let conflicting_body = b"another object at the content-addressed path";
        backing
            .put(
                &block_path(collision_prefix, &id),
                PutPayload::from(conflicting_body.to_vec()),
            )
            .await
            .unwrap();
        let conflicting = AwsS3BlockStore::new_with_cache_budget(
            backing.clone(),
            collision_prefix,
            false,
            budget.clone(),
        )
        .unwrap();
        assert!(
            conflicting
                .put(body)
                .await
                .unwrap_err()
                .is(mount_rs_core::ErrorCode::Eio)
        );
        assert!(
            conflicting
                .get(&id)
                .await
                .unwrap_err()
                .is(mount_rs_core::ErrorCode::Eio)
        );
        assert_eq!(
            backing
                .get(&block_path(collision_prefix, &id))
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            conflicting_body
        );
        backing
            .put(
                &block_path(published.prefix(), &id),
                PutPayload::from(conflicting_body.to_vec()),
            )
            .await
            .unwrap();
        assert!(
            published
                .get_for_migration(&id)
                .await
                .unwrap_err()
                .is(mount_rs_core::ErrorCode::Eio)
        );
        assert!(
            published
                .prepare_concurrent_backing()
                .await
                .unwrap_err()
                .is(mount_rs_core::ErrorCode::Enotsup)
        );
    }

    #[tokio::test]
    async fn public_aws_shared_budget_keeps_same_legacy_id_scoped_to_prefix_and_backing() {
        let backing = Arc::new(InMemory::new());
        let other_backing = Arc::new(InMemory::new());
        let budget = RawBlockCacheBudget::new(64 * 1024, 8);
        let first = AwsS3BlockStore::new_with_cache_budget(
            backing.clone(),
            "scoped/drive-a",
            false,
            budget.clone(),
        )
        .unwrap();
        let sibling = AwsS3BlockStore::new_with_cache_budget(
            backing.clone(),
            "scoped/drive-b",
            false,
            budget.clone(),
        )
        .unwrap();
        let other = AwsS3BlockStore::new_with_cache_budget(
            other_backing.clone(),
            "scoped/drive-a",
            false,
            budget.clone(),
        )
        .unwrap();
        let missing = AwsS3BlockStore::new_with_cache_budget(
            backing.clone(),
            "scoped/missing",
            false,
            budget,
        )
        .unwrap();
        let id = BlockId("b0123456789abcdef0123456789abcdef".to_owned());
        for (remote, store, body) in [
            (backing.as_ref(), &first, b"alpha".as_slice()),
            (backing.as_ref(), &sibling, b"bravo".as_slice()),
            (other_backing.as_ref(), &other, b"other".as_slice()),
        ] {
            remote
                .put(
                    &block_path(store.prefix(), &id),
                    PutPayload::from(body.to_vec()),
                )
                .await
                .unwrap();
            assert_eq!(store.get(&id).await.unwrap(), body);
        }
        assert_eq!(first.get(&id).await.unwrap(), b"alpha");
        assert_eq!(sibling.get(&id).await.unwrap(), b"bravo");
        assert_eq!(other.get(&id).await.unwrap(), b"other");
        assert!(
            missing
                .get(&id)
                .await
                .unwrap_err()
                .is(mount_rs_core::ErrorCode::Enoent)
        );
    }

    #[tokio::test]
    async fn configured_aws_wrapper_path_claims_a_stable_prefix_identity() {
        // This private simulated client tests wrapper forwarding only. It is
        // not evidence about an actual AWS S3 service or configured credentials.
        let shared = Arc::new(InMemory::new());
        let direct = AwsS3BlockStore::new(shared.clone(), "unit/blocks", true).unwrap();
        assert!(
            direct
                .prepare_concurrent_backing()
                .await
                .expect_err("unsigned constructor must remain unavailable")
                .is(mount_rs_core::ErrorCode::Enotsup)
        );
        let configured = AwsS3BlockStore(
            ObjectStoreBlockStore::new(shared.clone(), "unit/blocks", true).unwrap(),
            Some(shared.clone()),
        );
        let selected = configured.prepare_concurrent_backing().await.unwrap();
        let reopened = AwsS3BlockStore(
            ObjectStoreBlockStore::new(shared.clone(), "unit/blocks", true).unwrap(),
            Some(shared),
        );
        assert_eq!(
            reopened.prepare_concurrent_backing().await.unwrap(),
            selected
        );
        reopened.verify_concurrent_backing(selected).await.unwrap();
        let wrong = ConcurrentBackingId::from_bytes([0x55; 16]).unwrap();
        assert!(
            reopened
                .verify_concurrent_backing(wrong)
                .await
                .expect_err("different metadata binding must fail")
                .is(mount_rs_core::ErrorCode::Estale)
        );
    }

    #[test]
    fn validates_bucket_and_region() {
        let config = AwsS3Config {
            bucket: "mount-rs-production".to_owned(),
            region: "ap-southeast-2".to_owned(),
        };
        assert!(config.validate().is_ok());

        for (bucket, region) in [
            ("bad_bucket", "ap-southeast-2"),
            ("mount-rs-production", ""),
            ("mount-rs-production", " ap-southeast-2"),
            ("mount-rs-production", "ap_southeast_2"),
        ] {
            let config = AwsS3Config {
                bucket: bucket.to_owned(),
                region: region.to_owned(),
            };
            assert!(config.validate().is_err(), "accepted {bucket}/{region}");
        }
    }

    #[test]
    fn retry_policy_is_explicit_and_credential_safe() {
        let config = AwsS3Config {
            bucket: "mount-rs-production".to_owned(),
            region: "ap-southeast-2".to_owned(),
        };
        let retry = config.retry_config();
        assert_eq!(retry.max_retries, AWS_S3_MAX_RETRIES);
        assert_eq!(retry.retry_timeout, AWS_S3_RETRY_TIMEOUT);
        assert!(retry.retry_timeout < Duration::from_secs(5 * 60));
    }

    #[test]
    fn builder_rejects_endpoint_and_unsigned_request_overrides() {
        let endpoint = AmazonS3Builder::new().with_endpoint("http://localhost:9000");
        let error = validate_aws_builder(&endpoint).unwrap_err();
        assert!(error.to_string().contains("custom endpoints"));

        let sts_endpoint = AmazonS3Builder::new()
            .with_config(AmazonS3ConfigKey::StsEndpoint, "http://localhost:4566");
        let error = validate_aws_builder(&sts_endpoint).unwrap_err();
        assert!(error.to_string().contains("custom STS endpoints"));

        let unsigned = AmazonS3Builder::new().with_skip_signature(true);
        let error = validate_aws_builder(&unsigned).unwrap_err();
        assert!(error.to_string().contains("signed requests"));

        let allow_http = AmazonS3Builder::new().with_config(
            AmazonS3ConfigKey::Client(ClientConfigKey::AllowHttp),
            "true",
        );
        let error = validate_aws_builder(&allow_http).unwrap_err();
        assert!(error.to_string().contains("HTTP transport"));

        let invalid_certificates = AmazonS3Builder::new().with_config(
            AmazonS3ConfigKey::Client(ClientConfigKey::AllowInvalidCertificates),
            "true",
        );
        let error = validate_aws_builder(&invalid_certificates).unwrap_err();
        assert!(error.to_string().contains("invalid-certificate"));
    }
}
