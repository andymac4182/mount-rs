//! Built-in provider opening and resource cleanup.

#[cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
use std::path::Path;
use std::sync::Arc;

use mount_rs_aws_s3::{AwsS3BlockStore, AwsS3Config};
use mount_rs_core::storage::{BlockStore, MetadataStore};
use mount_rs_core::{Result, backend_error};
#[cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
use mount_rs_foundationdb::{
    FoundationDbLimits, FoundationDbSharedLeaseOracle, FoundationDbStorage,
    FoundationDbStorageOptions,
};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_rustfs::{RustFsBlockStore, RustFsConfig};
use mount_rs_slatedb::{SlateDbMetadataStore, rustfs_object_store};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbPoolContext, TidbStorageOptions};

#[cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
use crate::options::FoundationDbLeaseAuthority;
use crate::options::StoreConfig;
use crate::stores::{ErasedBlockStore, ErasedMetadataStore};

/// Server-owned storage resources. TiDB pools are bounded per exact connection
/// string (including credentials, database, TLS and session options). Equivalent
/// strings may use separate pools; different strings never share credentials.
/// Shut down every filesystem before explicitly closing the context.
#[derive(Clone)]
pub struct StorageContext {
    inner: Arc<std::sync::Mutex<ContextState>>,
    max_tidb_connections: usize,
}
#[derive(Default)]
struct ContextState {
    closed: bool,
    tidb: std::collections::HashMap<String, TidbPoolContext>,
}
impl Default for StorageContext {
    fn default() -> Self {
        Self::new(16).expect("positive default pool bound")
    }
}
impl StorageContext {
    pub fn new(max_tidb_connections: usize) -> Result<Self> {
        if max_tidb_connections == 0 {
            return Err(
                mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Einval)
                    .with_message("TiDB context pool maximum must be positive"),
            );
        }
        Ok(Self {
            inner: Arc::new(std::sync::Mutex::new(ContextState::default())),
            max_tidb_connections,
        })
    }
    fn require_open(&self) -> Result<()> {
        if self
            .inner
            .lock()
            .map_err(|_| backend_error("storage context lock poisoned"))?
            .closed
        {
            return Err(
                mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Estale)
                    .with_message("storage context is closed"),
            );
        }
        Ok(())
    }
    fn tidb(&self, connection: &str) -> Result<TidbPoolContext> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| backend_error("storage context lock poisoned"))?;
        if state.closed {
            return Err(mount_rs_core::FsError::new(
                mount_rs_core::ErrorCode::Estale,
            ));
        }
        if let Some(context) = state.tidb.get(connection) {
            return Ok(context.clone());
        }
        let context = TidbPoolContext::new(connection, self.max_tidb_connections)?;
        state.tidb.insert(connection.to_owned(), context.clone());
        Ok(context)
    }
    pub async fn close(&self) -> Result<()> {
        let contexts: Vec<_> = {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| backend_error("storage context lock poisoned"))?;
            state.closed = true;
            state.tidb.values().cloned().collect()
        };
        let mut closing: Vec<_> = contexts
            .iter()
            .map(|context| Some(Box::pin(context.close())))
            .collect();
        let mut error = None;
        std::future::poll_fn(|cx| {
            // Every close must be polled before yielding: each provider and
            // mysql_async pool then enters terminal closure, even if this
            // caller is cancelled while a checked-out connection drains.
            // Retaining contexts in the map lets a later close resume waiting.
            for close in &mut closing {
                if let Some(future) = close
                    && let std::task::Poll::Ready(result) = future.as_mut().poll(cx)
                {
                    if let Err(e) = result {
                        error.get_or_insert(e);
                    }
                    *close = None;
                }
            }
            if closing.iter().any(Option::is_some) {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(error.take().map_or(Ok(()), Err))
            }
        })
        .await
    }
}

#[derive(Clone)]
enum ProviderResource {
    PgliteMetadata(PgliteMetadataStore),
    PgliteBlocks(PgliteBlockStore),
    TidbMetadata(TidbMetadataStore),
    TidbBlocks(TidbBlockStore),
    SlateDbMetadata(SlateDbMetadataStore),
    #[cfg(all(
        feature = "foundationdb",
        any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
        )
    ))]
    FoundationDb(FoundationDbStorage),
}

impl ProviderResource {
    async fn close(&self) -> Result<()> {
        match self {
            Self::PgliteMetadata(store) => store.close().await,
            Self::PgliteBlocks(store) => store.close().await,
            Self::TidbMetadata(store) => store.close().await,
            Self::TidbBlocks(store) => store.close().await,
            Self::SlateDbMetadata(store) => store.close().await,
            #[cfg(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            ))]
            // FoundationDB storage owns the process-scoped network guard. It
            // must stay alive until the provider handles themselves are
            // dropped, so shutdown only releases the chunked lease here.
            Self::FoundationDb(storage) => {
                let _ = storage;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct StorageResources {
    resources: Vec<ProviderResource>,
}

impl StorageResources {
    pub(crate) async fn close(&self) -> Result<()> {
        let mut first_error = None;
        for resource in &self.resources {
            if let Err(error) = resource.close().await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

pub(crate) struct OpenStorage {
    pub(crate) metadata: ErasedMetadataStore,
    pub(crate) blocks: ErasedBlockStore,
    pub(crate) resources: StorageResources,
}

impl OpenStorage {
    pub(crate) async fn close(self) -> Result<()> {
        // The provider handles and FoundationDB network guard remain owned by
        // `self` until resource shutdown finishes on both result paths.
        self.resources.close().await
    }
}

pub(crate) async fn open_storage(
    metadata: &StoreConfig,
    blocks: &StoreConfig,
) -> Result<OpenStorage> {
    open_storage_decorated(metadata, blocks, None).await
}

pub(crate) async fn open_storage_decorated(
    metadata: &StoreConfig,
    blocks: &StoreConfig,
    decorator: Option<&dyn crate::filesystem::BlockStoreDecorator>,
) -> Result<OpenStorage> {
    open_storage_in_context(metadata, blocks, decorator, None).await
}

pub(crate) async fn open_storage_in_context(
    metadata: &StoreConfig,
    blocks: &StoreConfig,
    decorator: Option<&dyn crate::filesystem::BlockStoreDecorator>,
    context: Option<&StorageContext>,
) -> Result<OpenStorage> {
    if let Some(context) = context {
        context.require_open()?;
    }
    let block_config = blocks;
    let (metadata, mut metadata_resources) = open_metadata(metadata, context).await?;
    let (blocks, mut block_resources) = match open_blocks(blocks, context).await {
        Ok(opened) => opened,
        Err(error) => {
            let resources = StorageResources {
                resources: metadata_resources,
            };
            let _ = resources.close().await;
            return Err(error);
        }
    };
    metadata_resources.append(&mut block_resources);
    let blocks = match decorator {
        Some(decorator) => match decorator.decorate(block_config, blocks) {
            Ok(blocks) => blocks,
            Err(error) => {
                let resources = StorageResources {
                    resources: metadata_resources,
                };
                let _ = resources.close().await;
                return Err(error);
            }
        },
        None => blocks,
    };
    #[cfg(feature = "observability")]
    let (metadata, blocks) = {
        let telemetry = mount_rs_observability::global();
        (
            ErasedMetadataStore::new(metadata, telemetry.clone()),
            ErasedBlockStore::new(blocks, telemetry),
        )
    };
    #[cfg(not(feature = "observability"))]
    let (metadata, blocks) = (
        ErasedMetadataStore::new(metadata),
        ErasedBlockStore::new(blocks),
    );
    Ok(OpenStorage {
        metadata,
        blocks,
        resources: StorageResources {
            resources: metadata_resources,
        },
    })
}

async fn open_metadata(
    provider: &StoreConfig,
    context: Option<&StorageContext>,
) -> Result<(Arc<dyn MetadataStore>, Vec<ProviderResource>)> {
    match provider {
        StoreConfig::Memory => Ok((Arc::new(MemoryMetadataStore::new()), Vec::new())),
        StoreConfig::Sqlite { path } => {
            Ok((Arc::new(SqliteMetadataStore::open(path)?), Vec::new()))
        }
        StoreConfig::Pglite {
            connection,
            volume_key,
            durable,
        } => {
            let store = PgliteMetadataStore::connect_with_options(
                connection,
                PgliteStorageOptions::new(volume_key.clone()).with_durable(*durable),
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                vec![ProviderResource::PgliteMetadata(store)],
            ))
        }
        StoreConfig::Tidb {
            connection,
            volume_key,
            durable,
        } => {
            if let Some(context) = context {
                let store = context
                    .tidb(connection)?
                    .metadata(TidbStorageOptions::new(volume_key).with_durable(*durable))
                    .await?;
                return Ok((Arc::new(store), Vec::new()));
            }
            let store = TidbMetadataStore::connect_with_options(
                connection,
                TidbStorageOptions::new(volume_key.clone()).with_durable(*durable),
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                vec![ProviderResource::TidbMetadata(store)],
            ))
        }
        StoreConfig::SlateDb {
            endpoint,
            bucket,
            region,
            path,
            access_key_id,
            secret_access_key,
            durable,
        } => {
            let config = RustFsConfig {
                endpoint: endpoint.clone(),
                bucket: bucket.clone(),
                region: region.clone(),
                access_key_id: access_key_id.clone(),
                secret_access_key: secret_access_key.clone(),
            };
            let objects = rustfs_object_store(&config)?;
            let store = SlateDbMetadataStore::open_with_durable(path, objects, *durable).await?;
            Ok((
                Arc::new(store.clone()),
                vec![ProviderResource::SlateDbMetadata(store)],
            ))
        }
        StoreConfig::FoundationDb {
            cluster_file,
            volume_key,
            durable,
            lease_authority,
        } => {
            #[cfg(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            ))]
            {
                let storage =
                    open_foundationdb_storage(cluster_file, volume_key, *durable, lease_authority)?;
                let store = storage.metadata();
                Ok((
                    Arc::new(store),
                    vec![ProviderResource::FoundationDb(storage)],
                ))
            }
            #[cfg(not(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            )))]
            {
                let _ = (cluster_file, volume_key, durable, lease_authority);
                Err(mount_rs_core::FsError::enotsup(
                    "FoundationDB SDK support (enable the foundationdb feature on a supported native target)",
                ))
            }
        }
        StoreConfig::R2 { .. } | StoreConfig::RustFs { .. } | StoreConfig::AwsS3 { .. } => {
            Err(backend_error(
                "object-store metadata is unsupported; R2, RustFS, and AWS S3 are block-only",
            ))
        }
    }
}

async fn open_blocks(
    provider: &StoreConfig,
    context: Option<&StorageContext>,
) -> Result<(Arc<dyn BlockStore>, Vec<ProviderResource>)> {
    match provider {
        StoreConfig::Memory => Ok((Arc::new(MemoryBlockStore::new()), Vec::new())),
        StoreConfig::SlateDb { .. } => Err(mount_rs_core::FsError::enotsup(
            "SlateDB is a metadata provider; use RustFS for immutable blocks",
        )),
        StoreConfig::Sqlite { path } => Ok((Arc::new(SqliteBlockStore::open(path)?), Vec::new())),
        StoreConfig::Pglite {
            connection,
            volume_key,
            durable,
        } => {
            let store = PgliteBlockStore::connect_with_options(
                connection,
                PgliteStorageOptions::new(volume_key.clone()).with_durable(*durable),
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                vec![ProviderResource::PgliteBlocks(store)],
            ))
        }
        StoreConfig::Tidb {
            connection,
            volume_key,
            durable,
        } => {
            if let Some(context) = context {
                let store = context
                    .tidb(connection)?
                    .blocks(TidbStorageOptions::new(volume_key).with_durable(*durable))
                    .await?;
                return Ok((Arc::new(store), Vec::new()));
            }
            let store = TidbBlockStore::connect_with_options(
                connection,
                TidbStorageOptions::new(volume_key.clone()).with_durable(*durable),
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                vec![ProviderResource::TidbBlocks(store)],
            ))
        }
        StoreConfig::FoundationDb {
            cluster_file,
            volume_key,
            durable,
            lease_authority,
        } => {
            #[cfg(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            ))]
            {
                let storage =
                    open_foundationdb_storage(cluster_file, volume_key, *durable, lease_authority)?;
                let store = storage.blocks();
                Ok((
                    Arc::new(store),
                    vec![ProviderResource::FoundationDb(storage)],
                ))
            }
            #[cfg(not(all(
                feature = "foundationdb",
                any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "linux", target_arch = "aarch64"),
                    all(target_os = "macos", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64"),
                )
            )))]
            {
                let _ = (cluster_file, volume_key, durable, lease_authority);
                Err(mount_rs_core::FsError::enotsup(
                    "FoundationDB SDK support (enable the foundationdb feature on a supported native target)",
                ))
            }
        }
        StoreConfig::R2 {
            endpoint,
            bucket,
            prefix,
            access_key_id,
            secret_access_key,
            durable,
        } => {
            let config = R2Config {
                endpoint: endpoint.clone(),
                bucket: bucket.clone(),
                access_key_id: access_key_id.clone(),
                secret_access_key: secret_access_key.clone(),
                state_key: prefix.clone(),
            };
            let store = R2BlockStore::from_config_with_durable(&config, prefix.clone(), *durable)?;
            Ok((Arc::new(store), Vec::new()))
        }
        StoreConfig::RustFs {
            endpoint,
            bucket,
            region,
            prefix,
            access_key_id,
            secret_access_key,
            durable,
        } => {
            let config = RustFsConfig {
                endpoint: endpoint.clone(),
                bucket: bucket.clone(),
                region: region.clone(),
                access_key_id: access_key_id.clone(),
                secret_access_key: secret_access_key.clone(),
            };
            let store = RustFsBlockStore::from_config(&config, prefix.clone(), *durable)?;
            Ok((Arc::new(store), Vec::new()))
        }
        StoreConfig::AwsS3 {
            bucket,
            region,
            prefix,
            durable,
        } => {
            let config = AwsS3Config {
                bucket: bucket.clone(),
                region: region.clone(),
            };
            let store =
                AwsS3BlockStore::from_config_with_durable(&config, prefix.clone(), *durable)?;
            Ok((Arc::new(store), Vec::new()))
        }
    }
}

#[cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
fn open_foundationdb_storage(
    cluster_file: &Path,
    volume_key: &str,
    durable: bool,
    lease_authority: &FoundationDbLeaseAuthority,
) -> Result<FoundationDbStorage> {
    let options = FoundationDbStorageOptions::new(volume_key).with_durable(durable);
    let options = match lease_authority {
        FoundationDbLeaseAuthority::PersistedSingleAuthority => {
            options.with_persisted_lease_oracle()
        }
        FoundationDbLeaseAuthority::SharedProvider { authority_prefix } => {
            let oracle = FoundationDbSharedLeaseOracle::connect(
                cluster_file,
                authority_prefix,
                FoundationDbLimits::default(),
            )?;
            options.with_production_lease_oracle(oracle)
        }
        FoundationDbLeaseAuthority::RevisionCas => options.without_lease_oracle(),
    };
    FoundationDbStorage::connect(cluster_file, options)
}

#[cfg(test)]
mod context_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires actual TiDB and MOUNT_RS_TIDB_URL"]
    async fn actual_tidb_cancelled_close_fences_every_identity_and_can_resume() {
        use mysql_async::prelude::Queryable;
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let url = std::env::var("MOUNT_RS_TIDB_URL").unwrap();
        let second_url = format!(
            "{url}{}stmt_cache_size=31",
            if url.contains('?') { "&" } else { "?" }
        );
        let context = StorageContext::new(1).unwrap();
        context.tidb(&url).unwrap();
        context.tidb(&second_url).unwrap();
        // Match the stable map traversal in close, so the held pool is first.
        let pools: Vec<_> = context
            .inner
            .lock()
            .unwrap()
            .tidb
            .values()
            .cloned()
            .collect();
        assert_eq!(pools.len(), 2);
        let prefix = format!(
            "sdk-cancel-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let first_key = format!("{prefix}-held");
        let second_key = format!("{prefix}-idle");
        let held = pools[0]
            .metadata(TidbStorageOptions::new(&first_key))
            .await
            .unwrap();
        let idle = pools[1]
            .metadata(TidbStorageOptions::new(&second_key))
            .await
            .unwrap();
        let admin_pool = mysql_async::Pool::from_url(&url).unwrap();
        let mut admin = admin_pool.get_conn().await.unwrap();
        admin
            .query_drop("SET SESSION tidb_txn_mode='pessimistic'")
            .await
            .unwrap();
        let mut lock = admin
            .start_transaction(mysql_async::TxOpts::default())
            .await
            .unwrap();
        let _: Option<i64> = lock
            .exec_first(
                "SELECT revision FROM mount_rs_tidb_metadata WHERE volume_key=? FOR UPDATE",
                (&first_key,),
            )
            .await
            .unwrap();
        // This real provider transaction retains the pool's only connection
        // while the independent transaction holds its metadata row lock.
        let mut operation = Box::pin(held.acquire_writer("held", Duration::from_secs(30)));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut operation)
                .await
                .is_err()
        );
        let mut closing = Box::pin(context.close());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut closing)
                .await
                .is_err()
        );
        drop(closing);
        let checkouts_closed = [held.flush().await.is_err(), idle.flush().await.is_err()];
        let retained_contexts_closed = [
            pools[0]
                .metadata(TidbStorageOptions::new(&first_key))
                .await
                .is_err(),
            pools[1]
                .metadata(TidbStorageOptions::new(&second_key))
                .await
                .is_err(),
        ];
        lock.rollback().await.unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(5), operation).await;
        let resumed = tokio::time::timeout(Duration::from_secs(5), context.close()).await;
        for key in [&first_key, &second_key] {
            admin
                .exec_drop(
                    "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
                    (key,),
                )
                .await
                .unwrap();
        }
        drop(admin);
        admin_pool.disconnect().await.unwrap();
        assert!(
            completed.is_ok(),
            "held operation must release its connection"
        );
        assert!(
            resumed.is_ok_and(|result| result.is_ok()),
            "close completion must be resumable"
        );
        assert_eq!(
            checkouts_closed,
            [true, true],
            "cancellation left a pool accepting existing-store checkouts"
        );
        assert_eq!(
            retained_contexts_closed,
            [true, true],
            "cancellation left an already obtained provider context open"
        );
    }

    #[tokio::test]
    async fn contexts_isolate_credentials_database_options_and_server_lifetime() {
        let context = StorageContext::new(2).unwrap();
        assert!(StorageContext::new(0).is_err());
        for url in [
            "mysql://user:one@127.0.0.1/db",
            "mysql://user:two@127.0.0.1/db",
            "mysql://user:one@127.0.0.1/other",
            "mysql://user:one@127.0.0.1/db?stmt_cache_size=1",
        ] {
            context.tidb(url).unwrap();
            context.tidb(url).unwrap();
        }
        assert_eq!(context.inner.lock().unwrap().tidb.len(), 4);
        let independent = StorageContext::new(2).unwrap();
        assert_eq!(independent.inner.lock().unwrap().tidb.len(), 0);
        context.close().await.unwrap();
        assert!(context.tidb("mysql://user:one@127.0.0.1/db").is_err());
        assert!(context.require_open().is_err());
        assert!(independent.require_open().is_ok());
        independent.close().await.unwrap();
    }
}
