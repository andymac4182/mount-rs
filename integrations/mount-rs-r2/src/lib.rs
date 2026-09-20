//! Cloudflare R2 integration.
//!
//! R2 speaks the S3 API, so the implementation uses the official
//! `object_store` S3 client. The filesystem snapshot is one object, making the
//! integration deterministic while the port's filesystem semantics are being
//! differential-tested. The object key is configurable so multiple virtual
//! filesystems can share a bucket safely.

mod blocks;

pub use blocks::R2BlockStore;

use std::sync::Arc;

use async_trait::async_trait;
use mount_rs_core::{FsError, Result, backend_error};
use mount_rs_persist::{LoadedSnapshot, PersistedFs, StateStore, snapshot_conflict};
use object_store::aws::{AmazonS3Builder, S3ConditionalPut};
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, PutMode, PutOptions, PutPayload, UpdateVersion};

#[derive(Debug, Clone)]
pub struct R2Config {
    pub endpoint: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub state_key: String,
}

impl R2Config {
    pub fn from_env() -> Result<Self> {
        let required = |name: &str| {
            std::env::var(name).map_err(|_| FsError::backend(format!("missing {name}")))
        };
        Ok(Self {
            endpoint: required("R2_ENDPOINT")?,
            bucket: required("R2_BUCKET")?,
            access_key_id: required("R2_ACCESS_KEY_ID")?,
            secret_access_key: required("R2_SECRET_ACCESS_KEY")?,
            state_key: std::env::var("R2_STATE_KEY")
                .unwrap_or_else(|_| "mount-rs/state.json".to_owned()),
        })
    }

    pub fn build_store(&self) -> Result<Arc<dyn ObjectStore>> {
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
