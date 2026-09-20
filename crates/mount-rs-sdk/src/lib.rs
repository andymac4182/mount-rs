//! Public Rust consumer API for mount-rs.
//!
//! Backend implementations remain separate integration crates. This facade
//! gives applications and the Rust CLI one stable place to construct a
//! filesystem, compose independent metadata/block providers, access the
//! shared [`mount_rs_core::FsDriver`] contract, and finish provider cleanup.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{FsError, MemoryFs, backend_error};
use mount_rs_host::{HostFs, HostFsOptions};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_sqlite::{SqliteBlockStore, SqliteFs, SqliteMetadataStore, open_sqlite};

pub use mount_rs_auto::{
    AutoMount, AutoMountError, AutoMountOptions, AutoProbe, AutoTransport, Transport,
    TransportProbe, probe_transports,
};
pub use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, Loopback, MemoryOptions, MkdirOptions,
    OpenFlags, Result, Stats, StatsFs,
};
pub use mount_rs_host::HostFsOptions as HostOptions;

/// A provider selected for the metadata or immutable block side of a
/// split-store filesystem.
///
/// Credentials are passed as values by the application. Configuration files
/// and CLIs should resolve environment references before constructing this
/// value so the SDK never needs to know about a configuration-file schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreConfig {
    Memory,
    Sqlite {
        path: PathBuf,
    },
    Pglite {
        connection: String,
        volume_key: String,
        durable: bool,
    },
    R2 {
        endpoint: String,
        bucket: String,
        prefix: String,
        access_key_id: String,
        secret_access_key: String,
        durable: bool,
    },
}

/// Options for a filesystem with independent metadata and block providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitOptions {
    pub metadata: StoreConfig,
    pub blocks: StoreConfig,
    pub chunk_size_bytes: usize,
    pub owner: String,
    pub uid: u32,
    pub gid: u32,
    pub umask: u32,
}

impl SplitOptions {
    /// Create a volatile split store using the in-process memory providers.
    pub fn memory(owner: impl Into<String>, chunk_size_bytes: usize) -> Self {
        Self {
            metadata: StoreConfig::Memory,
            blocks: StoreConfig::Memory,
            chunk_size_bytes,
            owner: owner.into(),
            uid: 0,
            gid: 0,
            umask: 0,
        }
    }

    pub fn with_identity(mut self, uid: u32, gid: u32, umask: u32) -> Self {
        self.uid = uid;
        self.gid = gid;
        self.umask = umask;
        self
    }
}

/// Identifies the top-level filesystem construction used by an SDK consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemKind {
    Memory,
    Host,
    Sqlite,
    SplitStore,
}

/// A Rust SDK filesystem and its provider cleanup lifecycle.
pub struct Filesystem {
    inner: FilesystemInner,
}

enum FilesystemInner {
    Memory(MemoryFs),
    Host(HostFs),
    Sqlite(SqliteFs),
    Split(
        ChunkedFs<ErasedMetadataStore, ErasedBlockStore>,
        StorageResources,
    ),
}

impl Filesystem {
    /// Construct a volatile in-memory filesystem.
    pub fn memory(options: MemoryOptions) -> Self {
        Self {
            inner: FilesystemInner::Memory(MemoryFs::new(options)),
        }
    }

    /// Construct a host-backed filesystem rooted at `root`.
    pub fn host(root: impl AsRef<Path>, options: HostFsOptions) -> Self {
        Self {
            inner: FilesystemInner::Host(HostFs::with_options(root, options)),
        }
    }

    /// Open a durable SQLite-backed filesystem.
    pub async fn sqlite(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            inner: FilesystemInner::Sqlite(open_sqlite(path).await?),
        })
    }

    /// Open a filesystem composed from independent metadata and block stores.
    pub async fn split(options: SplitOptions) -> Result<Self> {
        if options.chunk_size_bytes == 0 {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("chunk_size_bytes must be greater than zero"));
        }
        let chunk_options = ChunkedOptions::fixed(options.owner, options.chunk_size_bytes)?
            .with_identity(options.uid, options.gid, options.umask);
        let opened = open_storage(&options.metadata, &options.blocks).await?;
        let resources = opened.resources.clone();
        match ChunkedFs::open(opened.metadata, opened.blocks, chunk_options).await {
            Ok(driver) => Ok(Self {
                inner: FilesystemInner::Split(driver, resources),
            }),
            Err(error) => {
                let _ = resources.close().await;
                Err(error)
            }
        }
    }

    /// Return the shared driver contract used by transports and loopback
    /// clients. The returned handle remains valid until this filesystem is
    /// shut down.
    pub fn driver(&self) -> Arc<dyn FsDriver> {
        match &self.inner {
            FilesystemInner::Memory(driver) => Arc::new(driver.clone()),
            FilesystemInner::Host(driver) => Arc::new(driver.clone()),
            FilesystemInner::Sqlite(driver) => Arc::new(driver.clone()),
            FilesystemInner::Split(driver, _) => Arc::new(driver.clone()),
        }
    }

    pub const fn kind(&self) -> FilesystemKind {
        match &self.inner {
            FilesystemInner::Memory(_) => FilesystemKind::Memory,
            FilesystemInner::Host(_) => FilesystemKind::Host,
            FilesystemInner::Sqlite(_) => FilesystemKind::Sqlite,
            FilesystemInner::Split(_, _) => FilesystemKind::SplitStore,
        }
    }

    /// Release writer leases and provider resources in the safe order.
    pub async fn shutdown(&self) -> Result<()> {
        match &self.inner {
            FilesystemInner::Split(driver, resources) => {
                let driver_result = driver.shutdown().await;
                let resources_result = resources.close().await;
                driver_result.and(resources_result)
            }
            FilesystemInner::Memory(_) | FilesystemInner::Host(_) | FilesystemInner::Sqlite(_) => {
                Ok(())
            }
        }
    }
}

impl Clone for Filesystem {
    fn clone(&self) -> Self {
        let inner = match &self.inner {
            FilesystemInner::Memory(driver) => FilesystemInner::Memory(driver.clone()),
            FilesystemInner::Host(driver) => FilesystemInner::Host(driver.clone()),
            FilesystemInner::Sqlite(driver) => FilesystemInner::Sqlite(driver.clone()),
            FilesystemInner::Split(driver, resources) => {
                FilesystemInner::Split(driver.clone(), resources.clone())
            }
        };
        Self { inner }
    }
}

#[derive(Clone)]
struct ErasedMetadataStore(Arc<dyn MetadataStore>);

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
struct ErasedBlockStore(Arc<dyn BlockStore>);

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
enum ProviderResource {
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
struct StorageResources {
    resources: Vec<ProviderResource>,
}

impl StorageResources {
    async fn close(&self) -> Result<()> {
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

struct OpenStorage {
    metadata: ErasedMetadataStore,
    blocks: ErasedBlockStore,
    resources: StorageResources,
}

async fn open_storage(metadata: &StoreConfig, blocks: &StoreConfig) -> Result<OpenStorage> {
    let (metadata, mut metadata_resources) = open_metadata(metadata).await?;
    let (blocks, mut block_resources) = match open_blocks(blocks).await {
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
    provider: &StoreConfig,
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
        StoreConfig::R2 { .. } => Err(backend_error(
            "metadata provider 'r2' is unsupported; R2 is block-only",
        )),
    }
}

async fn open_blocks(
    provider: &StoreConfig,
) -> Result<(Arc<dyn BlockStore>, Vec<ProviderResource>)> {
    match provider {
        StoreConfig::Memory => Ok((Arc::new(MemoryBlockStore::new()), Vec::new())),
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
            let store = R2BlockStore::new(config.build_store()?, prefix.clone(), *durable)?;
            Ok((Arc::new(store), Vec::new()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_sdk_roundtrip_uses_public_driver() {
        let filesystem = Filesystem::memory(MemoryOptions::default());
        let view = Loopback::from_arc(filesystem.driver());
        view.write_file("/sdk.txt", b"rust sdk").await.unwrap();
        assert_eq!(view.read_file("/sdk.txt").await.unwrap(), b"rust sdk");
        assert_eq!(filesystem.kind(), FilesystemKind::Memory);
        filesystem.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn split_sdk_roundtrip_uses_independent_memory_stores() {
        let filesystem = Filesystem::split(
            SplitOptions::memory("sdk-test-owner", 4).with_identity(1000, 1000, 0),
        )
        .await
        .unwrap();
        let view = Loopback::from_arc(filesystem.driver());
        view.write_file("/split.txt", b"split sdk").await.unwrap();
        assert_eq!(view.read_file("/split.txt").await.unwrap(), b"split sdk");
        assert_eq!(filesystem.kind(), FilesystemKind::SplitStore);
        filesystem.shutdown().await.unwrap();
    }
}
