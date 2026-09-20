//! CLI-local provider erasure and construction.
//!
//! The chunked filesystem is intentionally generic over concrete providers.
//! The CLI only needs a small erased forwarding layer so a JSON-selected
//! metadata provider and block provider can be composed without changing the
//! core crate or adding a second dependency framework.

use std::sync::Arc;

use async_trait::async_trait;
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{Result, backend_error};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::time::Duration;

use crate::config::{EnvReference, SplitStorageConfig, StorageProvider};

#[derive(Clone)]
pub(crate) struct ErasedMetadataStore(Arc<dyn MetadataStore>);

#[async_trait]
impl MetadataStore for ErasedMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable()
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        self.0.load().await
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        self.0.acquire_writer(owner, ttl).await
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        self.0.renew_writer(lease, ttl).await
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        self.0.release_writer(lease).await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        self.0.publish(expected_revision, lease, namespace).await
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }
}

#[derive(Clone)]
pub(crate) struct ErasedBlockStore(Arc<dyn BlockStore>);

#[async_trait]
impl BlockStore for ErasedBlockStore {
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
}

#[derive(Clone)]
pub(crate) enum ProviderResource {
    PgliteMetadata(PgliteMetadataStore),
    PgliteBlocks(PgliteBlockStore),
}

impl ProviderResource {
    async fn close(&self) -> Result<()> {
        match self {
            Self::PgliteMetadata(store) => store.close().await,
            Self::PgliteBlocks(store) => store.close().await,
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
            match resource.close().await {
                Err(error) if first_error.is_none() => first_error = Some(error),
                _ => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

pub(crate) struct OpenStorage {
    pub metadata: ErasedMetadataStore,
    pub blocks: ErasedBlockStore,
    pub resources: StorageResources,
}

pub(crate) async fn open_storage(config: &SplitStorageConfig) -> Result<OpenStorage> {
    let (metadata, mut metadata_resources) = open_metadata(&config.metadata).await?;
    let (blocks, mut block_resources) = match open_blocks(&config.blocks).await {
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
    Ok(OpenStorage {
        metadata: ErasedMetadataStore(metadata),
        blocks: ErasedBlockStore(blocks),
        resources: StorageResources {
            resources: metadata_resources,
        },
    })
}

async fn open_metadata(
    provider: &StorageProvider,
) -> Result<(Arc<dyn MetadataStore>, Vec<ProviderResource>)> {
    match provider {
        StorageProvider::Memory => Ok((Arc::new(MemoryMetadataStore::new()), Vec::new())),
        StorageProvider::Sqlite { path } => {
            Ok((Arc::new(SqliteMetadataStore::open(path)?), Vec::new()))
        }
        StorageProvider::Pglite {
            connection,
            volume_key,
            durable,
        } => {
            let store = PgliteMetadataStore::connect_with_options(
                &resolve_env(connection)?,
                PgliteStorageOptions::new(volume_key.clone()).with_durable(*durable),
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                vec![ProviderResource::PgliteMetadata(store)],
            ))
        }
        StorageProvider::R2 { .. } => Err(backend_error(
            "metadata provider 'r2' is unsupported; R2 is block-only",
        )),
    }
}

async fn open_blocks(
    provider: &StorageProvider,
) -> Result<(Arc<dyn BlockStore>, Vec<ProviderResource>)> {
    match provider {
        StorageProvider::Memory => Ok((Arc::new(MemoryBlockStore::new()), Vec::new())),
        StorageProvider::Sqlite { path } => {
            Ok((Arc::new(SqliteBlockStore::open(path)?), Vec::new()))
        }
        StorageProvider::Pglite {
            connection,
            volume_key,
            durable,
        } => {
            let store = PgliteBlockStore::connect_with_options(
                &resolve_env(connection)?,
                PgliteStorageOptions::new(volume_key.clone()).with_durable(*durable),
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                vec![ProviderResource::PgliteBlocks(store)],
            ))
        }
        StorageProvider::R2 {
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
                access_key_id: resolve_env(access_key_id)?,
                secret_access_key: resolve_env(secret_access_key)?,
                state_key: prefix.clone(),
            };
            let store = R2BlockStore::new(config.build_store()?, prefix.clone(), *durable)?;
            Ok((Arc::new(store), Vec::new()))
        }
    }
}

fn resolve_env(reference: &EnvReference) -> Result<String> {
    std::env::var(&reference.name)
        .map_err(|_| backend_error(format!("missing environment variable {}", reference.name)))
}
