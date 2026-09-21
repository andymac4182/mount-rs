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
    BlockId, BlockReconcileReport, BlockStore, LoadedMetadata, MetadataStore, Namespace,
    WriterLease,
};
use mount_rs_core::{FsError, MemoryFs, backend_error};
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
use mount_rs_host::{HostFs, HostFsOptions};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_r2::{AwsS3Config, R2BlockStore, R2Config};
use mount_rs_sqlite::{SqliteBlockStore, SqliteFs, SqliteMetadataStore, open_sqlite};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};

pub use mount_rs_auto::{
    AutoMount, AutoMountError, AutoMountOptions, AutoProbe, AutoTransport, Transport,
    TransportProbe, probe_transports,
};
pub use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, Loopback, MemoryOptions, MkdirOptions,
    OpenFlags, Result, Stats, StatsFs,
};
pub use mount_rs_host::HostFsOptions as HostOptions;
#[cfg(feature = "observability")]
pub use mount_rs_observability::{
    Telemetry, TelemetryConfig, global as global_telemetry, set_global as set_global_telemetry,
};

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
    Tidb {
        connection: String,
        volume_key: String,
        durable: bool,
    },
    FoundationDb {
        cluster_file: PathBuf,
        volume_key: String,
        durable: bool,
        lease_authority: FoundationDbLeaseAuthority,
    },
    R2 {
        endpoint: String,
        bucket: String,
        prefix: String,
        access_key_id: String,
        secret_access_key: String,
        durable: bool,
    },
    /// AWS S3 block storage using the standard AWS workload credential chain.
    /// The provider is block-only; metadata remains an independent store.
    AwsS3 {
        bucket: String,
        region: String,
        prefix: String,
        durable: bool,
    },
}

/// Lease authority choices exposed by consumer configuration.
///
/// The persisted choice is intentionally named as a single-authority mode:
/// it is suitable for an owned test cluster or one trusted writer authority.
/// Production consumers should select [`Self::SharedProvider`] with the
/// authority prefix published by a protected shared time authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundationDbLeaseAuthority {
    PersistedSingleAuthority,
    SharedProvider { authority_prefix: String },
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

    /// Return a driver decorated with optional application-owned telemetry.
    ///
    /// The decorator is feature-gated so the default SDK build has no
    /// observability dependency or runtime work. It only instruments this
    /// returned driver view; filesystem construction, shutdown, and the
    /// existing [`Self::driver`] behavior remain unchanged.
    #[cfg(feature = "observability")]
    pub fn driver_with_telemetry(&self, telemetry: Telemetry) -> Arc<dyn FsDriver> {
        mount_rs_observability::InstrumentedDriver::from_arc(self.driver(), telemetry).into_arc()
    }

    /// Return a driver decorated with the process-wide application telemetry.
    ///
    /// Applications can install exporters and then call
    /// [`set_global_telemetry`] during startup. The default global handle is
    /// disabled, so this method is also a no-op at runtime until the
    /// application explicitly enables telemetry.
    #[cfg(feature = "observability")]
    pub fn observed_driver(&self) -> Arc<dyn FsDriver> {
        self.driver_with_telemetry(global_telemetry())
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
struct ErasedMetadataStore {
    inner: Arc<dyn MetadataStore>,
    #[cfg(feature = "observability")]
    telemetry: Telemetry,
}

impl ErasedMetadataStore {
    fn new(inner: Arc<dyn MetadataStore>) -> Self {
        Self {
            inner,
            #[cfg(feature = "observability")]
            telemetry: mount_rs_observability::global(),
        }
    }
}

#[async_trait]
impl MetadataStore for ErasedMetadataStore {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs("provider.metadata", "load", None, self.inner.load())
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.load().await
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "lease.acquire",
                    None,
                    self.inner.acquire_writer(owner, ttl),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.acquire_writer(owner, ttl).await
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "lease.renew",
                    None,
                    self.inner.renew_writer(lease, ttl),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.renew_writer(lease, ttl).await
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "lease.release",
                    None,
                    self.inner.release_writer(lease),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.release_writer(lease).await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs(
                    "provider.metadata",
                    "publish",
                    None,
                    self.inner.publish(expected_revision, lease, namespace),
                )
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner
            .publish(expected_revision, lease, namespace)
            .await
    }

    async fn flush(&self) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs("provider.metadata", "flush", None, self.inner.flush())
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.flush().await
    }
}

#[derive(Clone)]
struct ErasedBlockStore {
    inner: Arc<dyn BlockStore>,
    #[cfg(feature = "observability")]
    telemetry: Telemetry,
}

impl ErasedBlockStore {
    fn new(inner: Arc<dyn BlockStore>) -> Self {
        Self {
            inner,
            #[cfg(feature = "observability")]
            telemetry: mount_rs_observability::global(),
        }
    }
}

#[async_trait]
impl BlockStore for ErasedBlockStore {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        #[cfg(feature = "observability")]
        {
            let count = bytes.len() as u64;
            let result = self
                .telemetry
                .observe_fs("provider.blocks", "put", None, self.inner.put(bytes))
                .await?;
            self.telemetry.record_bytes("write", count);
            return Ok(result);
        }
        #[cfg(not(feature = "observability"))]
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        #[cfg(feature = "observability")]
        {
            let result = self
                .telemetry
                .observe_fs("provider.blocks", "get", None, self.inner.get(id))
                .await?;
            self.telemetry.record_bytes("read", result.len() as u64);
            return Ok(result);
        }
        #[cfg(not(feature = "observability"))]
        self.inner.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs("provider.blocks", "flush", None, self.inner.flush())
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        #[cfg(feature = "observability")]
        {
            return self
                .telemetry
                .observe_fs("provider.blocks", "delete", None, self.inner.delete(id))
                .await;
        }
        #[cfg(not(feature = "observability"))]
        self.inner.delete(id).await
    }

    async fn reconcile(
        &self,
        live: &std::collections::BTreeSet<BlockId>,
        grace: Duration,
    ) -> Result<BlockReconcileReport> {
        #[cfg(feature = "observability")]
        {
            let report = self
                .telemetry
                .observe_fs(
                    "provider.blocks",
                    "reconcile",
                    None,
                    self.inner.reconcile(live, grace),
                )
                .await?;
            self.telemetry.record_reconcile(&report);
            return Ok(report);
        }
        #[cfg(not(feature = "observability"))]
        self.inner.reconcile(live, grace).await
    }
}

#[derive(Clone)]
enum ProviderResource {
    PgliteMetadata(PgliteMetadataStore),
    PgliteBlocks(PgliteBlockStore),
    TidbMetadata(TidbMetadataStore),
    TidbBlocks(TidbBlockStore),
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
        metadata: ErasedMetadataStore::new(metadata),
        blocks: ErasedBlockStore::new(blocks),
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
        StoreConfig::Tidb {
            connection,
            volume_key,
            durable,
        } => {
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
                Err(FsError::enotsup(
                    "FoundationDB SDK support (enable the foundationdb feature on a supported native target)",
                ))
            }
        }
        StoreConfig::R2 { .. } | StoreConfig::AwsS3 { .. } => Err(backend_error(
            "metadata providers 'r2' and 'aws-s3' are unsupported; object-store providers are block-only",
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
        StoreConfig::Tidb {
            connection,
            volume_key,
            durable,
        } => {
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
                Err(FsError::enotsup(
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
            let store = R2BlockStore::new(config.build_store()?, prefix.clone(), *durable)?;
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
            let store = R2BlockStore::new(config.build_store()?, prefix.clone(), *durable)?;
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
    };
    FoundationDbStorage::connect(cluster_file, options)
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

    #[cfg(feature = "observability")]
    #[tokio::test]
    async fn opt_in_driver_telemetry_preserves_sdk_roundtrip() {
        let filesystem = Filesystem::memory(MemoryOptions::default());
        let telemetry = Telemetry::new(TelemetryConfig::enabled("sdk-test"));
        let view = Loopback::from_arc(filesystem.driver_with_telemetry(telemetry.clone()));
        view.write_file("/observed.txt", b"observed").await.unwrap();
        assert_eq!(view.read_file("/observed.txt").await.unwrap(), b"observed");
        assert!(telemetry.snapshot().operations > 0);
        filesystem.shutdown().await.unwrap();
    }
}
