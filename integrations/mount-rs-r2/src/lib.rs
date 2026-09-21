//! Cloudflare R2 integration.
//!
//! R2 speaks the S3 API, so the implementation uses the official
//! `object_store` S3 client. The filesystem snapshot is one object, making the
//! integration deterministic while the port's filesystem semantics are being
//! differential-tested. The object key is configurable so multiple virtual
//! filesystems can share a bucket safely.

mod blocks;

pub use blocks::R2BlockStore;

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use mount_rs_core::{FsError, Result, backend_error};
use mount_rs_persist::{LoadedSnapshot, PersistedFs, StateStore, snapshot_conflict};
use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey, S3ConditionalPut};
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, PutMode, PutOptions, PutPayload, UpdateVersion};

#[derive(Clone)]
pub struct R2Config {
    pub endpoint: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub state_key: String,
}

/// Configuration for the first-class AWS S3 block provider.
///
/// Credentials are deliberately resolved by `object_store` from the standard
/// AWS environment and workload identity inputs. This keeps long-lived keys
/// out of configuration and supports web identity, ECS, and EC2 role
/// credentials in production deployments. S3-compatible endpoints belong to
/// [`R2Config`] instead and are rejected here.
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

    pub fn build_store(&self) -> Result<Arc<dyn ObjectStore>> {
        self.validate()?;
        let builder = AmazonS3Builder::from_env()
            .with_bucket_name(&self.bucket)
            .with_region(&self.region)
            .with_virtual_hosted_style_request(true)
            .with_conditional_put(S3ConditionalPut::ETagMatch);
        if builder
            .get_config_value(&AmazonS3ConfigKey::Endpoint)
            .is_some()
        {
            return Err(FsError::backend(
                "AWS S3 provider rejects custom endpoints; use the R2 provider for S3-compatible endpoints",
            ));
        }
        if builder
            .get_config_value(&AmazonS3ConfigKey::SkipSignature)
            .as_deref()
            == Some("true")
        {
            return Err(FsError::backend("AWS S3 provider requires signed requests"));
        }
        Ok(Arc::new(builder.build().map_err(backend_error)?))
    }
}

/// The non-secret identity of the configured object-store service.
///
/// This is suitable for acceptance logs. It deliberately contains neither
/// access-key material nor the full endpoint URL, because an endpoint may
/// include a path that is only meaningful to the client configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct R2ServiceIdentity {
    pub endpoint_authority: String,
    pub bucket: String,
}

impl fmt::Debug for R2Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("R2Config")
            .field(
                "endpoint_authority",
                &debug_endpoint_authority(&self.endpoint),
            )
            .field("bucket", &self.bucket)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field("state_key", &self.state_key)
            .finish()
    }
}

fn debug_endpoint_authority(endpoint: &str) -> String {
    let Some((scheme, remainder)) = endpoint.split_once("://") else {
        return "<redacted>".to_owned();
    };
    if !matches!(scheme, "http" | "https") {
        return "<redacted>".to_owned();
    }
    let authority = remainder.split('/').next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains('@')
        || endpoint.contains('?')
        || endpoint.contains('#')
    {
        "<redacted>".to_owned()
    } else {
        authority.to_owned()
    }
}

impl R2Config {
    pub fn from_env() -> Result<Self> {
        let required = |name: &str| {
            let value =
                std::env::var(name).map_err(|_| FsError::backend(format!("missing {name}")))?;
            if value.trim().is_empty() {
                return Err(FsError::backend(format!("missing {name}")));
            }
            Ok(value)
        };
        let config = Self {
            endpoint: required("R2_ENDPOINT")?,
            bucket: required("R2_BUCKET")?,
            access_key_id: required("R2_ACCESS_KEY_ID")?,
            secret_access_key: required("R2_SECRET_ACCESS_KEY")?,
            state_key: std::env::var("R2_STATE_KEY")
                .unwrap_or_else(|_| "mount-rs/state.json".to_owned()),
        };
        config.validate()?;
        Ok(config)
    }

    /// Validate the complete configuration before constructing a client.
    ///
    /// This is intentionally stricter than relying on `object_store` to reject
    /// malformed input: a bad endpoint, bucket, or state key must fail before
    /// any request can be made, and an empty credential must never be treated
    /// as a configured live R2 gate.
    pub fn validate(&self) -> Result<()> {
        validate_endpoint(&self.endpoint)?;
        validate_bucket(&self.bucket)?;
        validate_secret("access_key_id", &self.access_key_id)?;
        validate_secret("secret_access_key", &self.secret_access_key)?;
        validate_object_key("state_key", &self.state_key)?;
        Ok(())
    }

    /// Return only the service identity that is safe to include in evidence.
    pub fn service_identity(&self) -> Result<R2ServiceIdentity> {
        self.validate()?;
        let endpoint_authority = endpoint_authority(&self.endpoint)?;
        Ok(R2ServiceIdentity {
            endpoint_authority: endpoint_authority.to_owned(),
            bucket: self.bucket.clone(),
        })
    }

    pub fn build_store(&self) -> Result<Arc<dyn ObjectStore>> {
        self.validate()?;
        // `with_url` is a URL *parser* for a small set of AWS/R2 URL shapes;
        // it rejects ordinary HTTP endpoints used by local S3-compatible test
        // servers and custom R2 gateways. `with_endpoint` keeps the configured
        // endpoint verbatim and, with path-style requests, produces
        // `<endpoint>/<bucket>/<key>` for both Cloudflare R2 and S3-compatible
        // services. R2 uses `auto` as its signing region.
        let endpoint = self.endpoint.trim_end_matches('/');
        let bucket_suffix = format!("/{}", self.bucket);
        let endpoint = endpoint.strip_suffix(&bucket_suffix).unwrap_or(endpoint);
        let mut builder = AmazonS3Builder::new()
            .with_endpoint(endpoint)
            .with_bucket_name(&self.bucket)
            .with_access_key_id(&self.access_key_id)
            .with_secret_access_key(&self.secret_access_key)
            .with_region("auto")
            .with_virtual_hosted_style_request(false)
            .with_conditional_put(S3ConditionalPut::ETagMatch);
        if endpoint.starts_with("http://") {
            builder = builder.with_allow_http(true);
        }
        let store = builder.build().map_err(backend_error)?;
        Ok(Arc::new(store))
    }
}

fn validate_endpoint(endpoint: &str) -> Result<()> {
    let Some((scheme, remainder)) = endpoint.split_once("://") else {
        return Err(invalid_config("endpoint", "must use http:// or https://"));
    };
    if !matches!(scheme, "http" | "https")
        || endpoint.chars().any(|character| {
            character == '\0'
                || character.is_ascii_whitespace()
                || character == '?'
                || character == '#'
        })
        || endpoint_authority(endpoint)
            .ok()
            .is_none_or(|authority| authority.is_empty() || authority.contains('@'))
    {
        return Err(invalid_config(
            "endpoint",
            "must be an absolute HTTP(S) endpoint without credentials, query, or fragment",
        ));
    }
    // Keep this check separate from the authority extraction above so a
    // malformed endpoint cannot be accepted merely because it has a path.
    if remainder.starts_with('/') || remainder.starts_with('@') {
        return Err(invalid_config(
            "endpoint",
            "must include a host authority before any path",
        ));
    }
    Ok(())
}

fn endpoint_authority(endpoint: &str) -> Result<&str> {
    let Some((_, remainder)) = endpoint.split_once("://") else {
        return Err(invalid_config("endpoint", "must use http:// or https://"));
    };
    Ok(remainder.split('/').next().unwrap_or_default())
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
        return Err(invalid_config(
            "bucket",
            "must be a 3-63 character DNS-compatible bucket name",
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

fn validate_secret(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err(invalid_config(field, "must be non-empty"));
    }
    Ok(())
}

fn validate_object_key(field: &str, key: &str) -> Result<()> {
    if key.is_empty() || key.starts_with('/') || key.contains('\0') {
        return Err(invalid_config(field, "must be a relative object key"));
    }
    if key
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid_config(field, "contains an unsafe path component"));
    }
    Ok(())
}

fn invalid_config(field: &str, message: &str) -> FsError {
    FsError::backend(format!("invalid R2 {field}: {message}"))
}

#[derive(Clone)]
pub struct R2Store {
    store: Arc<dyn ObjectStore>,
    state_key: ObjectPath,
}

fn encode_version(version: UpdateVersion) -> Result<String> {
    if version.e_tag.is_none() && version.version.is_none() {
        return Err(backend_error(
            "R2 object store did not return a conditional-write version",
        ));
    }
    // ETags and object-store version IDs cannot contain NUL, so this keeps the
    // opaque pair lossless without adding a serialization dependency.
    Ok(format!(
        "{}\0{}",
        version.e_tag.unwrap_or_default(),
        version.version.unwrap_or_default()
    ))
}

fn decode_version(encoded: &str) -> Result<UpdateVersion> {
    let (e_tag, version) = encoded
        .split_once('\0')
        .ok_or_else(|| backend_error("invalid R2 conditional-write version"))?;
    let e_tag = (!e_tag.is_empty()).then(|| e_tag.to_owned());
    let version = (!version.is_empty()).then(|| version.to_owned());
    if e_tag.is_none() && version.is_none() {
        return Err(backend_error("invalid R2 conditional-write version"));
    }
    Ok(UpdateVersion { e_tag, version })
}

impl R2Store {
    pub fn new(store: Arc<dyn ObjectStore>, state_key: impl Into<String>) -> Self {
        Self {
            store,
            state_key: ObjectPath::from(state_key.into()),
        }
    }

    pub fn from_config(config: &R2Config) -> Result<Self> {
        Ok(Self::new(config.build_store()?, config.state_key.clone()))
    }

    pub async fn delete_snapshot(&self) -> Result<()> {
        match self.store.delete(&self.state_key).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(backend_error(error)),
        }
    }
}

#[async_trait]
impl StateStore for R2Store {
    async fn load(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.load_versioned().await?.snapshot)
    }

    async fn load_versioned(&self) -> Result<LoadedSnapshot> {
        match self.store.get(&self.state_key).await {
            Ok(result) => {
                let version = encode_version(UpdateVersion {
                    e_tag: result.meta.e_tag.clone(),
                    version: result.meta.version.clone(),
                })?;
                let snapshot = result.bytes().await.map_err(backend_error)?.to_vec();
                Ok(LoadedSnapshot {
                    snapshot: Some(snapshot),
                    version,
                })
            }
            Err(object_store::Error::NotFound { .. }) => Ok(LoadedSnapshot {
                snapshot: None,
                version: String::new(),
            }),
            Err(error) => Err(backend_error(error)),
        }
    }

    async fn save(&self, snapshot: Vec<u8>) -> Result<()> {
        self.store
            .put(&self.state_key, PutPayload::from(snapshot))
            .await
            .map_err(backend_error)?;
        Ok(())
    }

    async fn save_versioned(&self, snapshot: Vec<u8>, expected_version: &str) -> Result<String> {
        let mode = if expected_version.is_empty() {
            PutMode::Create
        } else {
            PutMode::Update(decode_version(expected_version)?)
        };
        let result = match self
            .store
            .put_opts(
                &self.state_key,
                PutPayload::from(snapshot),
                PutOptions {
                    mode,
                    ..Default::default()
                },
            )
            .await
        {
            Ok(result) => result,
            Err(object_store::Error::AlreadyExists { .. })
            | Err(object_store::Error::Precondition { .. }) => {
                return Err(snapshot_conflict("R2"));
            }
            Err(error) => return Err(backend_error(error)),
        };
        encode_version(UpdateVersion {
            e_tag: result.e_tag,
            version: result.version,
        })
    }
}

pub type R2Fs = PersistedFs<R2Store>;

pub async fn open_r2(config: R2Config) -> Result<R2Fs> {
    PersistedFs::open(R2Store::from_config(&config)?).await
}

pub async fn open_object_store(
    store: Arc<dyn ObjectStore>,
    state_key: impl Into<String>,
) -> Result<R2Fs> {
    PersistedFs::open(R2Store::new(store, state_key)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::FsDriver;
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::thread;

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match Future::poll(future.as_mut(), &mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => thread::yield_now(),
            }
        }
    }

    #[test]
    fn builder_accepts_rootless_http_s3_compatible_endpoints() {
        for endpoint in [
            "http://127.0.0.1:9000/",
            "https://account-id.r2.cloudflarestorage.com/",
            "https://account-id.r2.cloudflarestorage.com/mount-rs-tests/",
        ] {
            let config = R2Config {
                endpoint: endpoint.to_owned(),
                bucket: "mount-rs-tests".to_owned(),
                access_key_id: "test-access".to_owned(),
                secret_access_key: "test-secret".to_owned(),
                state_key: "state.json".to_owned(),
            };
            assert!(config.build_store().is_ok(), "endpoint: {endpoint}");
        }
    }

    #[test]
    fn configuration_validation_rejects_unsafe_live_inputs_before_client_build() {
        let cases = [
            (
                "ftp://account-id.r2.cloudflarestorage.com",
                "mount-rs-tests",
                "state.json",
            ),
            (
                "https://account-id.r2.cloudflarestorage.com?debug=1",
                "mount-rs-tests",
                "state.json",
            ),
            (
                "https://account-id.r2.cloudflarestorage.com",
                "bad_bucket",
                "state.json",
            ),
            (
                "https://account-id.r2.cloudflarestorage.com",
                "mount-rs-tests",
                "../state.json",
            ),
            (
                "https://account-id.r2.cloudflarestorage.com",
                "mount-rs-tests",
                "state//snapshot.json",
            ),
        ];
        for (endpoint, bucket, state_key) in cases {
            let config = R2Config {
                endpoint: endpoint.to_owned(),
                bucket: bucket.to_owned(),
                access_key_id: "test-access".to_owned(),
                secret_access_key: "test-secret".to_owned(),
                state_key: state_key.to_owned(),
            };
            assert!(
                config.validate().is_err(),
                "accepted unsafe config: {config:?}"
            );
            assert!(
                config.build_store().is_err(),
                "built unsafe config: {config:?}"
            );
        }
    }

    #[test]
    fn configuration_validation_rejects_blank_credentials() {
        for (access_key_id, secret_access_key) in [("", "test-secret"), ("test-access", " ")] {
            let config = R2Config {
                endpoint: "https://account-id.r2.cloudflarestorage.com".to_owned(),
                bucket: "mount-rs-tests".to_owned(),
                access_key_id: access_key_id.to_owned(),
                secret_access_key: secret_access_key.to_owned(),
                state_key: "state.json".to_owned(),
            };
            assert!(config.validate().is_err());
            assert!(config.build_store().is_err());
        }
    }

    #[test]
    fn aws_s3_configuration_validates_bucket_and_region() {
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
    fn debug_output_redacts_credentials() {
        let config = R2Config {
            endpoint: "https://account-id.r2.cloudflarestorage.com".to_owned(),
            bucket: "mount-rs-tests".to_owned(),
            access_key_id: "access-key-sentinel".to_owned(),
            secret_access_key: "secret-key-sentinel".to_owned(),
            state_key: "state.json".to_owned(),
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("access-key-sentinel"));
        assert!(!debug.contains("secret-key-sentinel"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn service_identity_is_safe_for_acceptance_evidence() {
        let config = R2Config {
            endpoint: "https://account-id.r2.cloudflarestorage.com/mount-rs-tests".to_owned(),
            bucket: "mount-rs-tests".to_owned(),
            access_key_id: "access-key-sentinel".to_owned(),
            secret_access_key: "secret-key-sentinel".to_owned(),
            state_key: "state.json".to_owned(),
        };
        let identity = config.service_identity().unwrap();
        assert_eq!(
            identity.endpoint_authority,
            "account-id.r2.cloudflarestorage.com"
        );
        assert_eq!(identity.bucket, "mount-rs-tests");
        let evidence = format!("{identity:?}");
        assert!(!evidence.contains("access-key-sentinel"));
        assert!(!evidence.contains("secret-key-sentinel"));
    }

    #[test]
    fn endpoint_embedded_credentials_are_rejected() {
        let config = R2Config {
            endpoint: "https://user:password@account-id.r2.cloudflarestorage.com".to_owned(),
            bucket: "mount-rs-tests".to_owned(),
            access_key_id: "access-key".to_owned(),
            secret_access_key: "secret-key".to_owned(),
            state_key: "state.json".to_owned(),
        };
        assert!(config.validate().is_err());
        assert!(config.build_store().is_err());
        let debug = format!("{config:?}");
        assert!(!debug.contains("user:password"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn in_memory_object_store_reopens_the_serialized_filesystem() {
        let object_store: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        let store = R2Store::new(object_store, "tests/reopen/state.json");
        let first = block_on(PersistedFs::open(store.clone())).unwrap();
        let handle = block_on(first.open("/durable", "w", 0o640)).unwrap();
        block_on(handle.write(b"r2", Some(0))).unwrap();
        block_on(handle.close()).unwrap();
        drop(handle);
        drop(first);

        let reopened = block_on(PersistedFs::open(store)).unwrap();
        let handle = block_on(reopened.open("/durable", "r", 0)).unwrap();
        let mut bytes = [0_u8; 2];
        assert_eq!(block_on(handle.read(&mut bytes, Some(0))).unwrap(), 2);
        assert_eq!(&bytes, b"r2");
    }
}
