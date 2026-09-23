//! RustFS S3-compatible immutable block provider.
//!
//! RustFS holds content-addressed blocks. A concurrent volume still needs a
//! metadata provider with revision CAS, such as FoundationDB or PGlite.

use std::collections::BTreeSet;
use std::fmt;
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::storage::{BlockId, BlockReconcileReport, BlockStore};
use mount_rs_core::{FsError, Result, backend_error};
use mount_rs_object_store_blocks::{
    ObjectStoreBlockStore, generate_private_qualification_prefix, prepare_configured_backing_id,
    probe_configured_concurrent_prefix, prove_two_configured_clients, verify_configured_backing_id,
};
use object_store::aws::{AmazonS3Builder, S3ConditionalPut};
use object_store::{ClientOptions, ObjectStore, RetryConfig};

pub use mount_rs_object_store_blocks::{
    ObjectStoreBlockStoreErrorClass as RustFsBlockStoreErrorClass,
    ObjectStoreBlockStoreStats as RustFsBlockStoreStats,
};

/// Signed RustFS S3 endpoint and bucket configuration.
#[derive(Clone)]
pub struct RustFsConfig {
    pub endpoint: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
}

impl fmt::Debug for RustFsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RustFsConfig")
            .field(
                "endpoint_authority",
                &debug_endpoint_authority(&self.endpoint),
            )
            .field("bucket", &self.bucket)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field("region", &self.region)
            .finish()
    }
}

impl RustFsConfig {
    /// Reject malformed or unsafe remote endpoints before any network request.
    pub fn validate(&self) -> Result<()> {
        validate_endpoint(&self.endpoint)?;
        validate_bucket(&self.bucket)?;
        validate_secret("access_key_id", &self.access_key_id)?;
        validate_secret("secret_access_key", &self.secret_access_key)?;
        validate_region(&self.region)?;
        Ok(())
    }

    /// Build a signed path-style client with conditional object creation.
    pub fn build_store(&self) -> Result<Arc<dyn ObjectStore>> {
        self.build_store_with_probe_limits(false)
    }

    fn build_probe_store(&self) -> Result<Arc<dyn ObjectStore>> {
        self.build_store_with_probe_limits(true)
    }

    fn build_store_with_probe_limits(&self, probe: bool) -> Result<Arc<dyn ObjectStore>> {
        self.validate()?;
        let endpoint = self.endpoint.trim_end_matches('/');
        let mut builder = AmazonS3Builder::new()
            .with_endpoint(endpoint)
            .with_bucket_name(&self.bucket)
            .with_access_key_id(&self.access_key_id)
            .with_secret_access_key(&self.secret_access_key)
            .with_region(&self.region)
            .with_virtual_hosted_style_request(false)
            .with_conditional_put(S3ConditionalPut::ETagMatch);
        if probe {
            // Limit only the startup probe; large data blocks keep the S3
            // client's normal request and retry budget.
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
        }
        if endpoint.starts_with("http://") {
            builder = builder.with_allow_http(true);
        }
        Ok(Arc::new(builder.build().map_err(backend_error)?))
    }
}

fn debug_endpoint_authority(endpoint: &str) -> String {
    let Some((scheme, remainder)) = endpoint.split_once("://") else {
        return "<redacted>".to_owned();
    };
    if !matches!(scheme, "http" | "https") || endpoint.contains('?') || endpoint.contains('#') {
        return "<redacted>".to_owned();
    }
    let authority = remainder.split('/').next().unwrap_or_default();
    if authority.contains('@') || authority_host(authority).is_none() {
        "<redacted>".to_owned()
    } else {
        format!("{scheme}://{authority}")
    }
}

fn authority_host(authority: &str) -> Option<&str> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, suffix) = bracketed.split_once(']')?;
        host.parse::<Ipv6Addr>().ok()?;
        if suffix.is_empty() || suffix.strip_prefix(':').is_some_and(valid_port) {
            return Some(host);
        }
        return None;
    }

    let (host, valid_suffix) = match authority.split_once(':') {
        Some((host, port)) => (host, valid_port(port)),
        None => (authority, true),
    };
    if host.is_empty()
        || !valid_suffix
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return None;
    }
    Some(host)
}

fn valid_port(port: &str) -> bool {
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|value| value != 0)
}

fn validate_endpoint(endpoint: &str) -> Result<()> {
    let Some((scheme, remainder)) = endpoint.split_once("://") else {
        return Err(invalid("endpoint", "must use http:// or https://"));
    };
    let authority = remainder.split('/').next().unwrap_or_default();
    let suffix = remainder.strip_prefix(authority).unwrap_or_default();
    if !matches!(scheme, "http" | "https")
        || authority.is_empty()
        || authority.contains('@')
        || authority_host(authority).is_none()
        || authority.chars().any(char::is_whitespace)
        || endpoint
            .chars()
            .any(|character| character == '\0' || character.is_whitespace())
        || endpoint.contains('?')
        || endpoint.contains('#')
        || (!suffix.is_empty() && suffix != "/")
    {
        return Err(invalid(
            "endpoint",
            "must be an HTTP(S) authority with optional trailing slash; paths, credentials, query, and fragment are forbidden",
        ));
    }
    if scheme == "http" && !is_local_http_authority(authority) {
        return Err(invalid(
            "endpoint",
            "HTTP is only allowed for loopback or the disposable Docker gateway",
        ));
    }
    Ok(())
}

fn is_local_http_authority(authority: &str) -> bool {
    matches!(
        authority_host(authority),
        Some("127.0.0.1" | "localhost" | "::1" | "host.docker.internal" | "mount-rs-rustfs")
    )
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
        return Err(invalid(
            "bucket",
            "must be a 3-63 character DNS-compatible bucket name",
        ));
    }
    Ok(())
}

fn validate_secret(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err(invalid(field, "must be non-empty"));
    }
    Ok(())
}

fn validate_region(region: &str) -> Result<()> {
    let bytes = region.as_bytes();
    if bytes.is_empty()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(invalid(
            "region",
            "must be a non-empty DNS-compatible region",
        ));
    }
    Ok(())
}

fn invalid(field: &str, message: &str) -> FsError {
    FsError::backend(format!("invalid RustFS {field}: {message}"))
}

/// Immutable blocks under one RustFS prefix.
///
/// Concurrent mode requires construction from signed configuration. Its
/// reusable, immutable probe occupies one retained object per prefix and is
/// deliberately outside normal block reconciliation.
#[derive(Clone)]
pub struct RustFsBlockStore {
    blocks: ObjectStoreBlockStore,
    configured_probe: Option<Arc<dyn ObjectStore>>,
    qualification_clients: Option<SignedQualificationClients>,
}

#[derive(Clone)]
struct SignedQualificationClients {
    first_data: Arc<dyn ObjectStore>,
    second_data: Arc<dyn ObjectStore>,
    first_probe: Arc<dyn ObjectStore>,
    second_probe: Arc<dyn ObjectStore>,
}

impl RustFsBlockStore {
    /// Wrap a client and declare whether its backing service is durable.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        prefix: impl Into<String>,
        durable: bool,
    ) -> Result<Self> {
        Ok(Self {
            blocks: ObjectStoreBlockStore::new(store, prefix, durable)?,
            configured_probe: None,
            qualification_clients: None,
        })
    }

    /// Build RustFS blocks with the caller's explicit durability assertion.
    pub fn from_config(
        config: &RustFsConfig,
        prefix: impl Into<String>,
        durable: bool,
    ) -> Result<Self> {
        let first_data = config.build_store()?;
        let second_data = config.build_store()?;
        let first_probe = config.build_probe_store()?;
        let second_probe = config.build_probe_store()?;
        let mut blocks = Self::new(first_data.clone(), prefix, durable)?;
        blocks.configured_probe = Some(first_probe.clone());
        blocks.qualification_clients = Some(SignedQualificationClients {
            first_data,
            second_data,
            first_probe,
            second_probe,
        });
        Ok(blocks)
    }

    pub fn prefix(&self) -> &str {
        self.blocks.prefix()
    }

    pub fn stats(&self) -> RustFsBlockStoreStats {
        self.blocks.stats()
    }
}

#[async_trait]
impl BlockStore for RustFsBlockStore {
    fn durable(&self) -> bool {
        self.blocks.durable()
    }

    async fn prepare_concurrent_backing(
        &self,
    ) -> Result<mount_rs_core::storage::ConcurrentBackingId> {
        let clients = self.qualification_clients.as_ref().ok_or_else(|| {
            FsError::new(mount_rs_core::ErrorCode::Enotsup)
                .with_syscall("prepare concurrent RustFS backing")
                .with_message("concurrent RustFS requires a validated signed configuration")
        })?;
        let probe = self.configured_probe.as_ref().ok_or_else(|| {
            FsError::new(mount_rs_core::ErrorCode::Enotsup)
                .with_syscall("prepare concurrent RustFS backing")
                .with_message("concurrent RustFS requires a validated signed configuration")
        })?;
        let private_prefix = generate_private_qualification_prefix(self.prefix())?;
        prove_two_configured_clients(
            clients.first_data.clone(),
            clients.second_data.clone(),
            clients.first_probe.clone(),
            clients.second_probe.clone(),
            &private_prefix,
        )
        .await?;
        probe_configured_concurrent_prefix(probe.as_ref(), self.prefix()).await?;
        prepare_configured_backing_id(probe.as_ref(), &self.blocks).await
    }

    async fn verify_concurrent_backing(
        &self,
        expected: mount_rs_core::storage::ConcurrentBackingId,
    ) -> Result<()> {
        let probe = self.configured_probe.as_ref().ok_or_else(|| {
            FsError::new(mount_rs_core::ErrorCode::Enotsup)
                .with_syscall("verify concurrent RustFS backing")
                .with_message("concurrent RustFS requires a validated signed configuration")
        })?;
        verify_configured_backing_id(probe.as_ref(), &self.blocks, expected).await
    }

    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.blocks.get_for_migration(id).await
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.blocks.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.blocks.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        self.blocks.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.blocks.delete(id).await
    }

    async fn reconcile(
        &self,
        live: &BTreeSet<BlockId>,
        grace: Duration,
    ) -> Result<BlockReconcileReport> {
        self.blocks.reconcile(live, grace).await
    }
}
