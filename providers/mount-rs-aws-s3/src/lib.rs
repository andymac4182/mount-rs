//! AWS S3 client and immutable-block provider.
//!
//! Credentials come from the standard AWS environment and workload identity
//! sources. This provider accepts AWS buckets and regions; S3-compatible
//! endpoints are configured through the R2 provider.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::storage::{BlockId, BlockReconcileReport, BlockStore};
use mount_rs_core::{FsError, Result, backend_error};
use mount_rs_object_store_blocks::{ObjectStoreBlockStore, ObjectStoreBlockStoreStats};
use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey, S3ConditionalPut};
use object_store::client::ClientConfigKey;
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
        self.validate()?;
        let builder = AmazonS3Builder::from_env()
            .with_bucket_name(&self.bucket)
            .with_region(&self.region)
            .with_virtual_hosted_style_request(true)
            .with_retry(self.retry_config())
            .with_conditional_put(S3ConditionalPut::ETagMatch);
        validate_aws_builder(&builder)?;
        Ok(Arc::new(builder.build().map_err(backend_error)?))
    }
}

/// Immutable blocks in one AWS S3 prefix.
///
/// The shared object-store adapter enforces block identities, conditional
/// creation, and scoped reconciliation. This wrapper owns the AWS client
/// construction and keeps it independent of the R2 provider.
#[derive(Clone)]
pub struct AwsS3BlockStore(ObjectStoreBlockStore);

impl AwsS3BlockStore {
    /// Wrap an existing client with an explicitly declared durability level.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        prefix: impl Into<String>,
        durable: bool,
    ) -> Result<Self> {
        Ok(Self(ObjectStoreBlockStore::new(store, prefix, durable)?))
    }

    /// Build durable blocks using this provider's signed AWS S3 client.
    pub fn from_config(config: &AwsS3Config, prefix: impl Into<String>) -> Result<Self> {
        Self::new(config.build_store()?, prefix, true)
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
