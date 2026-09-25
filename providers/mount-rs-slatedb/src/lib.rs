//! SlateDB-backed flat key-value storage for [`mount_rs_kv::KeyValueFs`].
//!
//! SlateDB owns its database path within an object store. Only one writer may
//! open that path at a time. This adapter acknowledges mutations after the
//! SlateDB write handle reaches its object-store durability barrier.

use async_trait::async_trait;
use mount_rs_core::storage::{LoadedMetadata, MetadataStore, Namespace, WriterLease};
use mount_rs_core::{ErrorCode, FsError, Result as FsResult, backend_error};
use mount_rs_kv::KeyValueStore;
use mount_rs_rustfs::RustFsConfig;
use serde::{Deserialize, Serialize};
use slatedb::object_store::ObjectStore;
use slatedb::object_store::aws::{AmazonS3Builder, S3ConditionalPut};
use slatedb::{Db, Error};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct SlateDbStore {
    db: Arc<Db>,
}

impl SlateDbStore {
    pub async fn open(path: &str, objects: Arc<dyn ObjectStore>) -> Result<Self, Error> {
        Ok(Self {
            db: Arc::new(Db::open(path, objects).await?),
        })
    }

    pub async fn close(&self) -> Result<(), Error> {
        self.db.close().await
    }
}

#[async_trait]
impl KeyValueStore for SlateDbStore {
    type Error = Error;

    async fn has_item(&self, key: &str) -> Result<bool, Error> {
        Ok(self.db.get(key.as_bytes()).await?.is_some())
    }

    async fn get_item_raw(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        Ok(self
            .db
            .get(key.as_bytes())
            .await?
            .map(|value| value.to_vec()))
    }

    async fn set_item_raw(&self, key: &str, value: Vec<u8>) -> Result<(), Error> {
        self.db
            .put(key.as_bytes(), value)
            .await?
            .await_durable()
            .await
    }

    async fn remove_item(&self, key: &str) -> Result<(), Error> {
        self.db.delete(key.as_bytes()).await?.await_durable().await
    }

    async fn get_keys(&self, prefix: &str) -> Result<Vec<String>, Error> {
        let mut iterator = self.db.scan_prefix(prefix.as_bytes(), ..).await?;
        let mut keys = Vec::new();
        while let Some(item) = iterator.next().await? {
            keys.push(String::from_utf8_lossy(&item.key).into_owned());
        }
        Ok(keys)
    }

    async fn get_keys_bounded(
        &self,
        prefix: &str,
        max_keys: usize,
    ) -> Result<Option<Vec<String>>, Error> {
        let mut iterator = self.db.scan_prefix(prefix.as_bytes(), ..).await?;
        let mut keys = Vec::new();
        while keys.len() <= max_keys {
            let Some(item) = iterator.next().await? else {
                break;
            };
            keys.push(String::from_utf8_lossy(&item.key).into_owned());
        }
        Ok(Some(keys))
    }
}

const METADATA_KEY: &[u8] = b"mount-rs/metadata/v1";

/// Build SlateDB's object-store client for the same validated RustFS service
/// used by the mount-rs immutable block provider. SlateDB and that provider
/// currently depend on different object_store versions, so they need separate
/// clients and non-overlapping key prefixes.
pub fn rustfs_object_store(config: &RustFsConfig) -> FsResult<Arc<dyn ObjectStore>> {
    config.validate()?;
    let mut builder = AmazonS3Builder::new()
        .with_endpoint(config.endpoint.trim_end_matches('/'))
        .with_bucket_name(&config.bucket)
        .with_region(&config.region)
        .with_access_key_id(&config.access_key_id)
        .with_secret_access_key(&config.secret_access_key)
        .with_virtual_hosted_style_request(false)
        .with_conditional_put(S3ConditionalPut::ETagMatch);
    if config.endpoint.starts_with("http://") {
        builder = builder.with_allow_http(true);
    }
    Ok(Arc::new(builder.build().map_err(backend_error)?))
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct PersistentMetadata {
    revision: u64,
    namespace: Option<Namespace>,
    last_fence: u64,
}

struct MetadataState {
    persisted: PersistentMetadata,
    lease: Option<WriterLease>,
    failed: bool,
}

/// Single-writer split-store metadata backed by a SlateDB database path.
/// A reopened SlateDB writer fences the previous writer; each new mount
/// increments the persisted lease fence. MRC2 and delegated modes are not
/// offered by this provider.
#[derive(Clone)]
pub struct SlateDbMetadataStore {
    db: Arc<Db>,
    state: Arc<Mutex<MetadataState>>,
    durable: bool,
}

impl SlateDbMetadataStore {
    pub async fn open(path: &str, objects: Arc<dyn ObjectStore>) -> FsResult<Self> {
        Self::open_with_durable(path, objects, false).await
    }

    pub async fn open_with_durable(
        path: &str,
        objects: Arc<dyn ObjectStore>,
        durable: bool,
    ) -> FsResult<Self> {
        let db = Db::open(path, objects).await.map_err(backend_error)?;
        let persisted = match db.get(METADATA_KEY).await.map_err(backend_error)? {
            Some(bytes) => {
                serde_json::from_slice::<PersistentMetadata>(&bytes).map_err(backend_error)?
            }
            None => PersistentMetadata::default(),
        };
        LoadedMetadata {
            revision: persisted.revision,
            namespace: persisted.namespace.clone(),
        }
        .validate()?;
        Ok(Self {
            db: Arc::new(db),
            state: Arc::new(Mutex::new(MetadataState {
                persisted,
                lease: None,
                failed: false,
            })),
            durable,
        })
    }

    pub async fn close(&self) -> FsResult<()> {
        self.db.close().await.map_err(backend_error)
    }

    async fn persist(&self, next: &PersistentMetadata) -> FsResult<()> {
        let bytes = serde_json::to_vec(next).map_err(backend_error)?;
        self.db
            .put(METADATA_KEY, bytes)
            .await
            .map_err(backend_error)?
            .await_durable()
            .await
            .map_err(backend_error)
    }
}

fn now_ms() -> FsResult<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(backend_error)?;
    u64::try_from(elapsed.as_millis()).map_err(backend_error)
}

fn lease_expiry(ttl: Duration) -> FsResult<u64> {
    let millis = u64::try_from(ttl.as_millis()).map_err(backend_error)?;
    if millis == 0 {
        return Err(FsError::new(ErrorCode::Einval));
    }
    now_ms()?
        .checked_add(millis)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))
}

fn check_lease(current: &Option<WriterLease>, supplied: &WriterLease) -> FsResult<()> {
    let Some(active) = current else {
        return Err(FsError::new(ErrorCode::Estale));
    };
    if active != supplied || now_ms()? >= active.expires_at_ms {
        return Err(FsError::new(ErrorCode::Estale));
    }
    Ok(())
}

fn check_healthy(state: &MetadataState) -> FsResult<()> {
    if state.failed {
        return Err(FsError::new(ErrorCode::Eio)
            .with_message("SlateDB metadata has an ambiguous write; reopen before retrying"));
    }
    Ok(())
}

#[async_trait]
impl MetadataStore for SlateDbMetadataStore {
    fn durable(&self) -> bool {
        self.durable
    }

    async fn load(&self) -> FsResult<LoadedMetadata> {
        let state = self.state.lock().await;
        check_healthy(&state)?;
        Ok(LoadedMetadata {
            revision: state.persisted.revision,
            namespace: state.persisted.namespace.clone(),
        })
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> FsResult<WriterLease> {
        if owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let expires_at_ms = lease_expiry(ttl)?;
        let now = now_ms()?;
        let mut state = self.state.lock().await;
        check_healthy(&state)?;
        if state
            .lease
            .as_ref()
            .is_some_and(|lease| now < lease.expires_at_ms)
        {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let mut next = state.persisted.clone();
        next.last_fence = next
            .last_fence
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        if let Err(error) = self.persist(&next).await {
            state.failed = true;
            return Err(error);
        }
        let lease = WriterLease {
            owner: owner.to_owned(),
            fence: next.last_fence,
            expires_at_ms,
        };
        state.persisted = next;
        state.lease = Some(lease.clone());
        Ok(lease)
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> FsResult<WriterLease> {
        let expires_at_ms = lease_expiry(ttl)?;
        let mut state = self.state.lock().await;
        check_healthy(&state)?;
        check_lease(&state.lease, lease)?;
        let renewed = WriterLease {
            expires_at_ms,
            ..lease.clone()
        };
        state.lease = Some(renewed.clone());
        Ok(renewed)
    }

    async fn release_writer(&self, lease: &WriterLease) -> FsResult<()> {
        let mut state = self.state.lock().await;
        check_healthy(&state)?;
        check_lease(&state.lease, lease)?;
        state.lease = None;
        Ok(())
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> FsResult<u64> {
        namespace.validate()?;
        let mut state = self.state.lock().await;
        check_healthy(&state)?;
        check_lease(&state.lease, lease)?;
        if state.persisted.revision != expected_revision {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let mut next = state.persisted.clone();
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        next.namespace = Some(namespace);
        if let Err(error) = self.persist(&next).await {
            state.failed = true;
            return Err(error);
        }
        state.persisted = next;
        Ok(state.persisted.revision)
    }

    async fn flush(&self) -> FsResult<()> {
        self.db.flush().await.map_err(backend_error)
    }
}
