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
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::storage::{BlockStore, InodeModeState, MetadataStore};
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
    FoundationDbBlockAuthorityPolicy, FoundationDbLimits, FoundationDbSharedLeaseOracle,
    FoundationDbStorage, FoundationDbStorageOptions,
};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_rustfs::{
    OwnedPrefixProbe, RawBlockCacheBudget, RawBlockCacheBudgetSnapshot, RustFsBlockStore,
    RustFsConfig, RustFsConstructionContext,
};
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
    rustfs: RustFsConstructionContext,
    raw_cache_budget: RawBlockCacheBudget,
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
        Self::new_with_raw_cache_limits(max_tidb_connections, 64 * 1024 * 1024, 4096)
    }

    /// Configure raw block cache capacity shared by RustFS, R2 and AWS S3
    /// facades opened with this context. Zero in either limit disables their
    /// raw cache admission; the charged limit is not a process RSS cap.
    pub fn new_with_raw_cache_limits(
        max_tidb_connections: usize,
        max_charged_bytes: usize,
        max_entries: usize,
    ) -> Result<Self> {
        if max_tidb_connections == 0 {
            return Err(
                mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Einval)
                    .with_message("TiDB context pool maximum must be positive"),
            );
        }
        let raw_cache_budget = RawBlockCacheBudget::new(max_charged_bytes, max_entries);
        let rustfs = RustFsConstructionContext::new_with_cache_budget(8, raw_cache_budget.clone())?;
        Ok(Self {
            inner: Arc::new(std::sync::Mutex::new(ContextState::default())),
            max_tidb_connections,
            rustfs,
            raw_cache_budget,
        })
    }

    /// Immutable configured limits: charged bytes and retained entries.
    /// This observes configuration without locking or inspecting live usage.
    pub fn raw_cache_limits(&self) -> (usize, usize) {
        (
            self.raw_cache_budget.max_charged_bytes(),
            self.raw_cache_budget.max_entries(),
        )
    }

    /// Common adapter observations; this type is reexported through RustFS.
    pub fn raw_cache_budget_snapshot(&self) -> Option<RawBlockCacheBudgetSnapshot> {
        self.raw_cache_budget.snapshot()
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
    /// Inspect persisted compact layout authority through fresh provider handles.
    ///
    /// Provider opening may initialize schemas and metadata rows. Inspection
    /// does not enroll compact layout or repair backing authority. A compact
    /// marker is returned only after the selected block provider verifies its
    /// backing identity. `None` retains the metadata provider's noncompact
    /// result. Inspection releases its handles while context-owned pools remain
    /// usable until the caller explicitly closes this context.
    ///
    /// Cleanup is awaited on both successful and failed inspections. An
    /// inspection error takes precedence if cleanup also fails; a cleanup error
    /// rejects an otherwise successful inspection.
    pub async fn inspect_compact_layout(
        &self,
        metadata: &StoreConfig,
        blocks: &StoreConfig,
    ) -> Result<Option<InodeModeState>> {
        let opened = open_storage_in_context(metadata, blocks, None, Some(self)).await?;
        inspect_compact_layout_opened(opened).await
    }
    /// Inspect compact authority while retaining partial provider owners.
    ///
    /// Retain the observer and the actual inspection operation before polling
    /// this future, then seal and join that operation before closing this
    /// context. An observer alone does not keep a cancelled operation running
    /// or acknowledge cleanup. Close retained resources after an acknowledged
    /// result, and preserve them when cleanup fails or remains uncertain.
    ///
    /// Provider opening may initialize schemas and metadata rows. Inspection
    /// reads the selected compact authority and verifies its existing backing;
    /// it never opens a filesystem, enrolls compact layout or repairs markers.
    /// No block decorator participates in this authority inspection.
    /// An inspection error takes precedence over a cleanup error; the observer
    /// retains the cleanup result independently through its resource owner.
    pub async fn inspect_compact_layout_with_construction_observer(
        &self,
        metadata: &StoreConfig,
        blocks: &StoreConfig,
        observer: &dyn ConstructionObserver,
    ) -> Result<Option<InodeModeState>> {
        let opened = open_storage_in_context_with_observer(
            metadata,
            blocks,
            None,
            Some(self),
            Some(observer),
        )
        .await?;
        inspect_compact_layout_opened(opened).await
    }

    /// Reject new pure RustFS client builds without closing storage authority.
    pub fn seal_client_builds(&self) -> Result<()> {
        self.rustfs.seal_admission()
    }

    /// Seal and join retained pure RustFS client builds and release the context's
    /// cached signed clients. Prefix-specific provider facades remain independent.
    ///
    /// This does not close TiDB pools or acknowledge cleanup of an abandoned
    /// outer provider operation. Its construction journal remains authoritative.
    pub async fn close_client_builds(&self) -> Result<()> {
        self.rustfs.close().await
    }

    /// Prepare an owned prefix probe without issuing its LIST request.
    ///
    /// The context must remain open for admission. The caller separately awaits
    /// the returned probe's observation and retains its outer operation owner.
    pub async fn prepare_rustfs_owned_prefix_probe(
        &self,
        config: RustFsConfig,
        prefix: String,
        observer: Option<&dyn ConstructionObserver>,
    ) -> Result<OwnedPrefixProbe> {
        self.require_open()?;
        self.rustfs
            .owned_prefix_probe(config, prefix, observer)
            .await
    }

    pub async fn close(&self) -> Result<()> {
        let mut error = self.seal_client_builds().err();
        let contexts: Vec<_> = {
            let mut state = match self.inner.lock() {
                Ok(state) => state,
                Err(poisoned) => {
                    error.get_or_insert_with(|| backend_error("storage context lock poisoned"));
                    poisoned.into_inner()
                }
            };
            state.closed = true;
            state.tidb.values().cloned().collect()
        };
        let closing = contexts
            .iter()
            .map(|context| Some(Box::pin(context.close())))
            .collect();
        close_context_resources(self.close_client_builds(), closing, error).await
    }
}

async fn close_context_resources<C, P>(
    construction: C,
    mut closing: Vec<Option<std::pin::Pin<Box<P>>>>,
    mut error: Option<mount_rs_core::FsError>,
) -> Result<()>
where
    C: std::future::Future<Output = Result<()>>,
    P: std::future::Future<Output = Result<()>>,
{
    let mut construction = Some(Box::pin(construction));
    std::future::poll_fn(|cx| {
        // Poll both resource families before yielding, even after poison or
        // another close failure. Retained owners let a later close resume.
        if let Some(future) = &mut construction
            && let std::task::Poll::Ready(result) = future.as_mut().poll(cx)
        {
            if let Err(e) = result {
                error.get_or_insert(e);
            }
            construction = None;
        }
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
        if construction.is_some() || closing.iter().any(Option::is_some) {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(error.take().map_or(Ok(()), Err))
        }
    })
    .await
}

#[derive(Clone)]
enum ProviderResource {
    Constructor(Arc<dyn ConstructionResource>),
    MetadataKeepalive(Arc<dyn MetadataStore>),
    BlockKeepalive(Arc<dyn BlockStore>),
    #[cfg(all(test, unix))]
    CloseProbe(Arc<compact_layout_inspection_tests::CloseProbe>),
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
            Self::Constructor(owner) => owner.close().await,
            Self::MetadataKeepalive(store) => {
                let _ = store;
                Ok(())
            }
            Self::BlockKeepalive(store) => {
                let _ = store;
                Ok(())
            }
            #[cfg(all(test, unix))]
            Self::CloseProbe(probe) => probe.close().await,
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
    observed: Option<Arc<ObservedStorageResources>>,
}

impl StorageResources {
    pub(crate) fn is_observed(&self) -> bool {
        self.observed.is_some()
    }

    pub(crate) async fn close(&self) -> Result<()> {
        if let Some(observed) = &self.observed {
            return observed.close().await;
        }
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

struct ObservedProviderState {
    accepting: bool,
    uncertain: bool,
    closed: bool,
    next: usize,
    failure: Option<mount_rs_core::FsError>,
    resources: Vec<ProviderResource>,
}

/// One canonical owner for observed provider construction. Its forward order
/// preserves metadata-before-block cleanup inside the journal's reverse order.
struct ObservedStorageResources {
    state: std::sync::Mutex<ObservedProviderState>,
    closing: tokio::sync::Mutex<()>,
}

impl ObservedStorageResources {
    fn lock(&self) -> std::sync::MutexGuard<'_, ObservedProviderState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.uncertain = true;
                state
            }
        }
    }

    fn retain_provider(&self, resource: ProviderResource) {
        let mut state = self.lock();
        if !state.accepting || state.closed {
            state.uncertain = true;
        }
        // A late or poisoned registration remains owned even though cleanup
        // cannot acknowledge it. Never drop a constructor's actual owner.
        state.resources.push(resource);
    }
}

impl ConstructionObserver for ObservedStorageResources {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        self.retain_provider(ProviderResource::Constructor(resource));
    }
}

#[async_trait::async_trait]
impl ConstructionResource for ObservedStorageResources {
    async fn close(&self) -> Result<()> {
        let _closing = self.closing.lock().await;
        loop {
            let resource = {
                let state = self.lock();
                if let Some(error) = &state.failure {
                    return Err(error.clone());
                }
                if state.uncertain {
                    return Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio));
                }
                if state.accepting {
                    return Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Ebusy));
                }
                if state.closed {
                    return Ok(());
                }
                state.resources.get(state.next).cloned()
            };
            if let Some(resource) = resource {
                let result = resource.close().await;
                let mut state = self.lock();
                if let Err(error) = result {
                    state.failure = Some(error.clone());
                    state.uncertain = true;
                    return Err(error);
                }
                if state.uncertain {
                    return Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio));
                }
                state.next += 1;
            } else {
                let released = {
                    let mut state = self.lock();
                    if state.uncertain {
                        return Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio));
                    }
                    state.closed = true;
                    std::mem::take(&mut state.resources)
                };
                // Actual provider destructors run outside the ownership mutex.
                drop(released);
                if self.lock().uncertain {
                    return Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio));
                }
                return Ok(());
            }
        }
    }
}

/// Dropping an unsealed constructor makes even an empty group uncertain.
struct ProviderConstruction {
    resources: Arc<ObservedStorageResources>,
    completed: bool,
}

impl ProviderConstruction {
    fn new(observer: &dyn ConstructionObserver) -> Self {
        let resources = Arc::new(ObservedStorageResources {
            state: std::sync::Mutex::new(ObservedProviderState {
                accepting: true,
                uncertain: false,
                closed: false,
                next: 0,
                failure: None,
                resources: Vec::new(),
            }),
            closing: tokio::sync::Mutex::new(()),
        });
        // The outer application owner assumes this group before any provider
        // constructor is polled or can register a child owner.
        observer.retain(resources.clone());
        Self {
            resources,
            completed: false,
        }
    }

    fn retain_provider(&self, resource: ProviderResource) {
        self.resources.retain_provider(resource);
    }

    fn finish(mut self) -> Arc<ObservedStorageResources> {
        self.resources.lock().accepting = false;
        self.completed = true;
        self.resources.clone()
    }
}

impl ConstructionObserver for ProviderConstruction {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        self.resources.retain(resource);
    }
}

impl Drop for ProviderConstruction {
    fn drop(&mut self) {
        if !self.completed {
            let mut state = self.resources.lock();
            state.accepting = false;
            state.uncertain = true;
        }
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

async fn inspect_compact_layout_opened(opened: OpenStorage) -> Result<Option<InodeModeState>> {
    let inspected: Result<Option<InodeModeState>> = async {
        let mode = opened.metadata.compact_inode_mode_state().await?;
        if let Some(mode) = &mode {
            opened
                .blocks
                .verify_concurrent_backing(mode.backing)
                .await?;
        }
        Ok(mode)
    }
    .await;
    let closed = opened.close().await;
    inspected.and_then(|mode| closed.map(|()| mode))
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
    open_storage_in_context_with_observer(metadata, blocks, decorator, context, None).await
}

pub(crate) async fn open_storage_in_context_with_observer(
    metadata: &StoreConfig,
    blocks: &StoreConfig,
    decorator: Option<&dyn crate::filesystem::BlockStoreDecorator>,
    context: Option<&StorageContext>,
    observer: Option<&dyn ConstructionObserver>,
) -> Result<OpenStorage> {
    if let Some(context) = context {
        context.require_open()?;
    }
    let construction = observer.map(ProviderConstruction::new);
    let opened =
        open_storage_observed(metadata, blocks, decorator, context, construction.as_ref()).await;
    let observed = construction.map(ProviderConstruction::finish);
    opened.map(|mut opened| {
        opened.resources.observed = observed;
        opened
    })
}

async fn open_storage_observed(
    metadata: &StoreConfig,
    blocks: &StoreConfig,
    decorator: Option<&dyn crate::filesystem::BlockStoreDecorator>,
    context: Option<&StorageContext>,
    construction: Option<&ProviderConstruction>,
) -> Result<OpenStorage> {
    let observer = construction.map(|construction| construction as &dyn ConstructionObserver);
    let block_config = blocks;
    let (metadata, mut metadata_resources) =
        open_metadata(metadata, blocks, context, observer).await?;
    if let Some(construction) = construction {
        for resource in metadata_resources.drain(..) {
            construction.retain_provider(resource);
        }
        construction.retain_provider(ProviderResource::MetadataKeepalive(metadata.clone()));
    }
    let (blocks, mut block_resources) = match open_blocks(blocks, context, observer).await {
        Ok(opened) => opened,
        Err(error) => {
            if construction.is_none() {
                let resources = StorageResources {
                    resources: metadata_resources,
                    observed: None,
                };
                let _ = resources.close().await;
            }
            return Err(error);
        }
    };
    if let Some(construction) = construction {
        for resource in block_resources.drain(..) {
            construction.retain_provider(resource);
        }
        construction.retain_provider(ProviderResource::BlockKeepalive(blocks.clone()));
    }
    metadata_resources.append(&mut block_resources);
    let blocks = match decorator {
        Some(decorator) => match decorator.decorate(block_config, blocks) {
            Ok(blocks) => blocks,
            Err(error) => {
                if construction.is_none() {
                    let resources = StorageResources {
                        resources: metadata_resources,
                        observed: None,
                    };
                    let _ = resources.close().await;
                }
                return Err(error);
            }
        },
        None => blocks,
    };
    if decorator.is_some()
        && let Some(construction) = construction
    {
        construction.retain_provider(ProviderResource::BlockKeepalive(blocks.clone()));
    }
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
            observed: None,
        },
    })
}

async fn open_metadata(
    provider: &StoreConfig,
    _block_config: &StoreConfig,
    context: Option<&StorageContext>,
    observer: Option<&dyn ConstructionObserver>,
) -> Result<(Arc<dyn MetadataStore>, Vec<ProviderResource>)> {
    match provider {
        StoreConfig::Memory => Ok((Arc::new(MemoryMetadataStore::new()), Vec::new())),
        StoreConfig::Sqlite { path } => {
            Ok((Arc::new(SqliteMetadataStore::open(path)?), Vec::new()))
        }
        StoreConfig::SqliteWithOptions { path, options } => Ok((
            Arc::new(SqliteMetadataStore::open_with_options(path, *options)?),
            Vec::new(),
        )),
        StoreConfig::Pglite {
            connection,
            volume_key,
            durable,
        } => {
            let store = PgliteMetadataStore::connect_with_options_and_observer(
                connection,
                PgliteStorageOptions::new(volume_key.clone()).with_durable(*durable),
                observer,
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                if observer.is_none() {
                    vec![ProviderResource::PgliteMetadata(store)]
                } else {
                    Vec::new()
                },
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
            let store = TidbMetadataStore::connect_with_options_and_observer(
                connection,
                TidbStorageOptions::new(volume_key.clone()).with_durable(*durable),
                observer,
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                if observer.is_none() {
                    vec![ProviderResource::TidbMetadata(store)]
                } else {
                    Vec::new()
                },
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
                let storage = open_foundationdb_storage(
                    cluster_file,
                    volume_key,
                    *durable,
                    lease_authority,
                    foundationdb_metadata_policy(cluster_file, volume_key, _block_config),
                )?;
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
    observer: Option<&dyn ConstructionObserver>,
) -> Result<(Arc<dyn BlockStore>, Vec<ProviderResource>)> {
    match provider {
        StoreConfig::Memory => Ok((Arc::new(MemoryBlockStore::new()), Vec::new())),
        StoreConfig::SlateDb { .. } => Err(mount_rs_core::FsError::enotsup(
            "SlateDB is a metadata provider; use RustFS for immutable blocks",
        )),
        StoreConfig::Sqlite { path } => Ok((Arc::new(SqliteBlockStore::open(path)?), Vec::new())),
        StoreConfig::SqliteWithOptions { path, options } => Ok((
            Arc::new(SqliteBlockStore::open_with_options(path, *options)?),
            Vec::new(),
        )),
        StoreConfig::Pglite {
            connection,
            volume_key,
            durable,
        } => {
            let store = PgliteBlockStore::connect_with_options_and_observer(
                connection,
                PgliteStorageOptions::new(volume_key.clone()).with_durable(*durable),
                observer,
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                if observer.is_none() {
                    vec![ProviderResource::PgliteBlocks(store)]
                } else {
                    Vec::new()
                },
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
            let store = TidbBlockStore::connect_with_options_and_observer(
                connection,
                TidbStorageOptions::new(volume_key.clone()).with_durable(*durable),
                observer,
            )
            .await?;
            Ok((
                Arc::new(store.clone()),
                if observer.is_none() {
                    vec![ProviderResource::TidbBlocks(store)]
                } else {
                    Vec::new()
                },
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
                let storage = open_foundationdb_storage(
                    cluster_file,
                    volume_key,
                    *durable,
                    lease_authority,
                    FoundationDbBlockAuthorityPolicy::SameKeyspace,
                )?;
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
            let store = if let Some(context) = context {
                R2BlockStore::from_config_with_cache_budget(
                    &config,
                    prefix.clone(),
                    *durable,
                    context.raw_cache_budget.clone(),
                )?
            } else {
                R2BlockStore::from_config_with_durable(&config, prefix.clone(), *durable)?
            };
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
            let store = if let Some(context) = context {
                context
                    .rustfs
                    .block_store(config, prefix.clone(), *durable, observer)
                    .await?
            } else {
                RustFsBlockStore::from_config(&config, prefix.clone(), *durable)?
            };
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
            let store = if let Some(context) = context {
                AwsS3BlockStore::from_config_with_cache_budget(
                    &config,
                    prefix.clone(),
                    *durable,
                    context.raw_cache_budget.clone(),
                )?
            } else {
                AwsS3BlockStore::from_config_with_durable(&config, prefix.clone(), *durable)?
            };
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
fn foundationdb_metadata_policy(
    cluster: &Path,
    prefix: &str,
    blocks: &StoreConfig,
) -> FoundationDbBlockAuthorityPolicy {
    match blocks {
        StoreConfig::FoundationDb {
            cluster_file,
            volume_key,
            ..
        } if cluster_file.as_os_str() == cluster.as_os_str() && volume_key == prefix => {
            FoundationDbBlockAuthorityPolicy::SameKeyspace
        }
        _ => FoundationDbBlockAuthorityPolicy::ExternalBlockStore,
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
    policy: FoundationDbBlockAuthorityPolicy,
) -> Result<FoundationDbStorage> {
    let options = FoundationDbStorageOptions::new(volume_key)
        .with_durable(durable)
        .with_block_authority_policy(policy);
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

#[cfg(all(test, unix))]
#[path = "provider_construction_tests.rs"]
mod provider_construction_tests;

#[cfg(all(test, unix))]
mod compact_layout_inspection_tests {
    use super::*;
    use crate::{Filesystem, SplitOptions};
    use mount_rs_core::{ErrorCode, FsError};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct OwnedDirectory(std::path::PathBuf);
    impl OwnedDirectory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "mount-rs-compact-inspection-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::SeqCst),
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn store(&self, name: &str) -> StoreConfig {
            StoreConfig::Sqlite {
                path: self.0.join(format!("{name}.sqlite")),
            }
        }
    }
    impl Drop for OwnedDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn layout(
        context: &StorageContext,
        metadata: &StoreConfig,
        blocks: &StoreConfig,
        compact: bool,
    ) -> Filesystem {
        let mut options = SplitOptions::memory("compact-inspection", 4096)
            .with_concurrent_writes(true)
            .with_compact_inode_updates(compact);
        options.metadata = metadata.clone();
        options.blocks = blocks.clone();
        Filesystem::split_with_context(options, context)
            .await
            .unwrap()
    }

    pub(super) struct CloseProbe {
        calls: AtomicUsize,
        completed: AtomicBool,
        fail: bool,
    }
    impl CloseProbe {
        fn new(fail: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                completed: AtomicBool::new(false),
                fail,
            })
        }
        pub(super) async fn close(&self) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            self.completed.store(true, Ordering::SeqCst);
            if self.fail {
                Err(FsError::new(ErrorCode::Eio))
            } else {
                Ok(())
            }
        }
        fn assert_awaited(&self) {
            assert_eq!(self.calls.load(Ordering::SeqCst), 1);
            assert!(self.completed.load(Ordering::SeqCst));
        }
    }
    async fn opened_with_probes(
        context: &StorageContext,
        metadata: &StoreConfig,
        blocks: &StoreConfig,
        first_fails: bool,
    ) -> (OpenStorage, [Arc<CloseProbe>; 2]) {
        let mut opened = open_storage_in_context(metadata, blocks, None, Some(context))
            .await
            .unwrap();
        let probes = [CloseProbe::new(first_fails), CloseProbe::new(false)];
        for probe in &probes {
            opened
                .resources
                .resources
                .push(ProviderResource::CloseProbe(probe.clone()));
        }
        (opened, probes)
    }

    #[derive(Default)]
    struct InspectionObserver(std::sync::Mutex<Vec<Arc<dyn ConstructionResource>>>);
    impl ConstructionObserver for InspectionObserver {
        fn retain(&self, resource: Arc<dyn ConstructionResource>) {
            self.0.lock().unwrap().push(resource);
        }
    }
    impl InspectionObserver {
        fn group(&self) -> Arc<dyn ConstructionResource> {
            let resources = self.0.lock().unwrap();
            assert_eq!(
                resources.len(),
                1,
                "one canonical inspection provider group"
            );
            resources[0].clone()
        }
    }

    #[tokio::test]
    async fn observed_compact_inspection_preserves_same_and_split_sqlite_state() {
        for same_database in [true, false] {
            let owned = OwnedDirectory::new();
            let metadata = owned.store("metadata");
            let blocks = if same_database {
                metadata.clone()
            } else {
                owned.store("blocks")
            };
            let context = StorageContext::new(2).unwrap();
            let fs = layout(&context, &metadata, &blocks, true).await;
            let driver = fs.driver();
            driver.write_file("/sentinel", b"proof\0").await.unwrap();
            let StoreConfig::Sqlite { path } = &metadata else {
                unreachable!()
            };
            let raw = SqliteMetadataStore::open(path).unwrap();
            let mode = raw.compact_inode_mode_state().await.unwrap().unwrap();
            let before = raw.load_compact_snapshot(mode.backing).await.unwrap();
            let observer = InspectionObserver::default();
            assert_eq!(
                context
                    .inspect_compact_layout_with_construction_observer(
                        &metadata, &blocks, &observer,
                    )
                    .await
                    .unwrap(),
                Some(mode)
            );
            observer.group().close().await.unwrap();
            assert_eq!(raw.compact_inode_mode_state().await.unwrap(), Some(mode));
            assert_eq!(
                raw.load_compact_snapshot(mode.backing).await.unwrap(),
                before
            );
            let handle = driver.open("/sentinel", "r", 0).await.unwrap();
            let mut bytes = [0_u8; 8];
            assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 6);
            assert_eq!(&bytes[..6], b"proof\0");
            assert_eq!(handle.read(&mut bytes, Some(6)).await.unwrap(), 0);
            handle.close().await.unwrap();
            context.require_open().unwrap();
            driver
                .write_file("/after-inspection", b"usable")
                .await
                .unwrap();
            fs.shutdown().await.unwrap();
            drop(fs);
            context.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn observed_compact_inspection_does_not_enroll_a_noncompact_layout() {
        let owned = OwnedDirectory::new();
        let store = owned.store("noncompact");
        let context = StorageContext::new(2).unwrap();
        let fs = layout(&context, &store, &store, false).await;
        fs.shutdown().await.unwrap();
        drop(fs);
        let observer = InspectionObserver::default();
        assert_eq!(
            context
                .inspect_compact_layout_with_construction_observer(&store, &store, &observer)
                .await
                .unwrap(),
            None
        );
        observer.group().close().await.unwrap();
        let StoreConfig::Sqlite { path } = &store else {
            unreachable!()
        };
        assert_eq!(
            SqliteMetadataStore::open(path)
                .unwrap()
                .compact_inode_mode_state()
                .await
                .unwrap(),
            None
        );
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn observed_compact_inspection_rejects_foreign_backing_without_repair() {
        for established_foreign_marker in [false, true] {
            let owned = OwnedDirectory::new();
            let store = owned.store("layout");
            let wrong = owned.store("wrong-blocks");
            let context = StorageContext::new(2).unwrap();
            let fs = layout(&context, &store, &store, true).await;
            fs.shutdown().await.unwrap();
            drop(fs);
            let StoreConfig::Sqlite { path } = &store else {
                unreachable!()
            };
            let raw = SqliteMetadataStore::open(path).unwrap();
            let mode = raw.compact_inode_mode_state().await.unwrap().unwrap();
            let before = raw.load_compact_snapshot(mode.backing).await.unwrap();
            let StoreConfig::Sqlite { path } = &wrong else {
                unreachable!()
            };
            let foreign = SqliteBlockStore::open(path).unwrap();
            let foreign_id = if established_foreign_marker {
                Some(foreign.prepare_concurrent_backing().await.unwrap())
            } else {
                None
            };
            let observer = InspectionObserver::default();
            assert_eq!(
                context
                    .inspect_compact_layout_with_construction_observer(&store, &wrong, &observer)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Estale
            );
            // The inspection result alone does not certify resource cleanup.
            observer.group().close().await.unwrap();
            assert!(
                foreign
                    .verify_concurrent_backing(mode.backing)
                    .await
                    .is_err()
            );
            if let Some(foreign_id) = foreign_id {
                foreign.verify_concurrent_backing(foreign_id).await.unwrap();
            }
            assert_eq!(raw.compact_inode_mode_state().await.unwrap(), Some(mode));
            assert_eq!(
                raw.load_compact_snapshot(mode.backing).await.unwrap(),
                before
            );
            context.require_open().unwrap();
            context.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn observed_compact_inspection_closed_context_rejects_before_registration() {
        let owned = OwnedDirectory::new();
        let unopened = owned.store("must-not-open");
        let context = StorageContext::new(2).unwrap();
        context.close().await.unwrap();
        let observer = InspectionObserver::default();
        assert_eq!(
            context
                .inspect_compact_layout_with_construction_observer(&unopened, &unopened, &observer,)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert!(observer.0.lock().unwrap().is_empty());
        let StoreConfig::Sqlite { path } = &unopened else {
            unreachable!()
        };
        assert!(
            !path.exists(),
            "closed context must not open a selected provider"
        );
    }

    #[tokio::test]
    async fn observed_inspection_keeps_primary_failure_separate_from_cleanup_failure() {
        for compact in [true, false] {
            let owned = OwnedDirectory::new();
            let store = owned.store("layout");
            let context = StorageContext::new(2).unwrap();
            if compact {
                let fs = layout(&context, &store, &store, true).await;
                fs.shutdown().await.unwrap();
                drop(fs);
            }
            let metadata = if compact {
                store.clone()
            } else {
                StoreConfig::Memory
            };
            let blocks = if compact {
                store.clone()
            } else {
                StoreConfig::Memory
            };
            let observer = InspectionObserver::default();
            let opened = open_storage_in_context_with_observer(
                &metadata,
                &blocks,
                None,
                Some(&context),
                Some(&observer),
            )
            .await
            .unwrap();
            let group = opened.resources.observed.as_ref().unwrap().clone();
            let failed = CloseProbe::new(true);
            let later = CloseProbe::new(false);
            let weak_later = Arc::downgrade(&later);
            // Add controlled cleanup dependencies before inspection begins.
            // This is fixture setup, not late production registration.
            group
                .lock()
                .resources
                .push(ProviderResource::CloseProbe(failed.clone()));
            group
                .lock()
                .resources
                .push(ProviderResource::CloseProbe(later.clone()));
            drop(later);
            let inspection = inspect_compact_layout_opened(opened).await.unwrap_err();
            assert_eq!(
                inspection.code,
                if compact {
                    ErrorCode::Eio
                } else {
                    ErrorCode::Enotsup
                }
            );
            assert_eq!(
                observer.group().close().await.unwrap_err().code,
                ErrorCode::Eio
            );
            assert_eq!(
                observer.group().close().await.unwrap_err().code,
                ErrorCode::Eio
            );
            failed.assert_awaited();
            assert!(
                weak_later.upgrade().is_some(),
                "failed cleanup retains later owners"
            );
            assert_eq!(
                weak_later.upgrade().unwrap().calls.load(Ordering::SeqCst),
                0
            );
            // These SQLite/Memory fixtures contain no context-owned TiDB pool.
            // No successful observed-group drain is claimed on this negative path.
            assert!(context.inner.lock().unwrap().tidb.is_empty());
            context.close().await.unwrap();
        }
    }

    struct HeldInspectionCleanup {
        entered: tokio::sync::watch::Sender<bool>,
        release: tokio::sync::Semaphore,
        calls: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl ConstructionResource for HeldInspectionCleanup {
        async fn close(&self) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.send_replace(true);
            self.release
                .acquire()
                .await
                .map_err(|_| FsError::new(ErrorCode::Eio))?
                .forget();
            Ok(())
        }
    }

    #[tokio::test]
    async fn observed_inspection_owned_operation_survives_a_cancelled_cleanup_waiter() {
        let owned = OwnedDirectory::new();
        let store = owned.store("layout");
        let context = StorageContext::new(2).unwrap();
        let fs = layout(&context, &store, &store, true).await;
        fs.shutdown().await.unwrap();
        drop(fs);
        let observer = InspectionObserver::default();
        let opened = open_storage_in_context_with_observer(
            &store,
            &store,
            None,
            Some(&context),
            Some(&observer),
        )
        .await
        .unwrap();
        let group = opened.resources.observed.as_ref().unwrap().clone();
        let (entered, mut entering) = tokio::sync::watch::channel(false);
        let gate = Arc::new(HeldInspectionCleanup {
            entered,
            release: tokio::sync::Semaphore::new(0),
            calls: AtomicUsize::new(0),
        });
        group
            .lock()
            .resources
            .push(ProviderResource::Constructor(gate.clone()));
        // The actual operation handle is retained before any waiter is polled.
        let mut operation = tokio::spawn(inspect_compact_layout_opened(opened));
        tokio::time::timeout(std::time::Duration::from_secs(5), entering.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(*entering.borrow());
        let waiter = observer.group();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), waiter.close())
                .await
                .is_err()
        );
        assert!(!operation.is_finished());
        assert!(!group.lock().closed);
        assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
        context.require_open().unwrap();
        gate.release.add_permits(1);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut operation)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .is_some()
        );
        observer.group().close().await.unwrap();
        assert!(group.lock().closed);
        assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn compact_layout_inspection_observes_persisted_enrollment_and_later_generation() {
        let owned = OwnedDirectory::new();
        let store = owned.store("layout");
        let context = StorageContext::new(2).unwrap();
        let ordinary = layout(&context, &store, &store, false).await;
        ordinary.shutdown().await.unwrap();
        drop(ordinary);
        assert_eq!(
            context
                .inspect_compact_layout(&store, &store)
                .await
                .unwrap(),
            None
        );

        let compact = layout(&context, &store, &store, true).await;
        let first = context
            .inspect_compact_layout(&store, &store)
            .await
            .unwrap()
            .unwrap();
        compact
            .driver()
            .mkdir("/fresh", mount_rs_core::MkdirOptions::default())
            .await
            .unwrap();
        let later = context
            .inspect_compact_layout(&store, &store)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(later.backing, first.backing);
        assert!(later.structural_generation > first.structural_generation);
        compact.shutdown().await.unwrap();
        drop(compact);
        assert_eq!(
            context
                .inspect_compact_layout(&store, &store)
                .await
                .unwrap(),
            Some(later)
        );
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn compact_layout_inspection_rejects_missing_and_wrong_backing_without_repair() {
        let owned = OwnedDirectory::new();
        let store = owned.store("layout");
        let wrong = owned.store("wrong-blocks");
        let context = StorageContext::new(2).unwrap();
        let fs = layout(&context, &store, &store, true).await;
        fs.shutdown().await.unwrap();
        drop(fs);
        let mode = context
            .inspect_compact_layout(&store, &store)
            .await
            .unwrap()
            .unwrap();
        let StoreConfig::Sqlite { path } = &wrong else {
            unreachable!()
        };
        let foreign = SqliteBlockStore::open(path).unwrap();
        assert_eq!(
            context
                .inspect_compact_layout(&store, &wrong)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert!(
            foreign
                .verify_concurrent_backing(mode.backing)
                .await
                .is_err()
        );
        let foreign_backing = foreign.prepare_concurrent_backing().await.unwrap();
        assert_ne!(foreign_backing, mode.backing);
        assert_eq!(
            context
                .inspect_compact_layout(&store, &wrong)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        foreign
            .verify_concurrent_backing(foreign_backing)
            .await
            .unwrap();
        assert_eq!(
            context
                .inspect_compact_layout(&store, &store)
                .await
                .unwrap(),
            Some(mode)
        );
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn compact_layout_inspection_closed_clone_is_terminal_and_other_context_is_usable() {
        let owned = OwnedDirectory::new();
        let store = owned.store("layout");
        let context = StorageContext::new(2).unwrap();
        let fs = layout(&context, &store, &store, true).await;
        let mode = context
            .inspect_compact_layout(&store, &store)
            .await
            .unwrap();
        assert_eq!(
            context
                .inspect_compact_layout(&store, &store)
                .await
                .unwrap(),
            mode
        );
        fs.driver()
            .write_file("/caller-still-usable", b"receipt")
            .await
            .unwrap();
        fs.shutdown().await.unwrap();
        drop(fs);
        let closed = context.clone();
        context.close().await.unwrap();
        let unopened = owned.store("must-not-open");
        assert_eq!(
            closed
                .inspect_compact_layout(&unopened, &unopened)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        let StoreConfig::Sqlite { path } = unopened else {
            unreachable!()
        };
        assert!(
            !path.exists(),
            "terminal context must reject before provider opening"
        );
        let independent = StorageContext::new(2).unwrap();
        assert_eq!(
            independent
                .inspect_compact_layout(&store, &store)
                .await
                .unwrap()
                .unwrap()
                .backing,
            mode.unwrap().backing
        );
        independent.close().await.unwrap();
    }

    #[tokio::test]
    async fn compact_layout_inspection_awaits_cleanup_on_compact_and_noncompact_success() {
        for compact in [false, true] {
            let owned = OwnedDirectory::new();
            let store = owned.store("layout");
            let context = StorageContext::new(2).unwrap();
            let fs = layout(&context, &store, &store, compact).await;
            fs.shutdown().await.unwrap();
            drop(fs);
            let (opened, probes) = opened_with_probes(&context, &store, &store, false).await;
            assert_eq!(
                inspect_compact_layout_opened(opened)
                    .await
                    .unwrap()
                    .is_some(),
                compact
            );
            for probe in &probes {
                probe.assert_awaited();
            }
            context.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn compact_layout_inspection_awaits_cleanup_after_persisted_read_error() {
        let context = StorageContext::new(2).unwrap();
        let (opened, probes) =
            opened_with_probes(&context, &StoreConfig::Memory, &StoreConfig::Memory, false).await;
        assert_eq!(
            inspect_compact_layout_opened(opened)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        for probe in &probes {
            probe.assert_awaited();
        }
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn compact_layout_inspection_awaits_cleanup_after_backing_error() {
        let owned = OwnedDirectory::new();
        let store = owned.store("layout");
        let wrong = owned.store("wrong-blocks");
        let context = StorageContext::new(2).unwrap();
        let fs = layout(&context, &store, &store, true).await;
        fs.shutdown().await.unwrap();
        drop(fs);
        let (opened, probes) = opened_with_probes(&context, &store, &wrong, false).await;
        assert_eq!(
            inspect_compact_layout_opened(opened)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        for probe in &probes {
            probe.assert_awaited();
        }
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn compact_layout_inspection_cleanup_failure_rejects_success_and_closes_remaining_resources()
     {
        let owned = OwnedDirectory::new();
        let store = owned.store("layout");
        let context = StorageContext::new(2).unwrap();
        let fs = layout(&context, &store, &store, true).await;
        fs.shutdown().await.unwrap();
        drop(fs);
        let (opened, probes) = opened_with_probes(&context, &store, &store, true).await;
        assert_eq!(
            inspect_compact_layout_opened(opened)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        for probe in &probes {
            probe.assert_awaited();
        }
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn compact_layout_inspection_primary_error_survives_cleanup_failure() {
        let context = StorageContext::new(2).unwrap();
        let (opened, probes) =
            opened_with_probes(&context, &StoreConfig::Memory, &StoreConfig::Memory, true).await;
        assert_eq!(
            inspect_compact_layout_opened(opened)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        for probe in &probes {
            probe.assert_awaited();
        }
        context.close().await.unwrap();
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    #[cfg(all(
        feature = "foundationdb",
        any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
        )
    ))]
    #[test]
    fn foundationdb_policy_requires_exact_cluster_and_prefix_pair() {
        use FoundationDbBlockAuthorityPolicy::{ExternalBlockStore, SameKeyspace};
        let config = |cluster: &str, prefix: &str| StoreConfig::FoundationDb {
            cluster_file: cluster.into(),
            volume_key: prefix.into(),
            durable: true,
            lease_authority: FoundationDbLeaseAuthority::RevisionCas,
        };
        for (cluster, prefix, expected) in [
            ("/cluster", "volume", SameKeyspace),
            ("/other", "volume", ExternalBlockStore),
            ("/cluster", "other", ExternalBlockStore),
            ("/./cluster", "volume", ExternalBlockStore),
        ] {
            assert_eq!(
                foundationdb_metadata_policy(
                    Path::new("/cluster"),
                    "volume",
                    &config(cluster, prefix)
                ),
                expected
            );
        }
        assert_eq!(
            foundationdb_metadata_policy(Path::new("/cluster"), "volume", &StoreConfig::Memory),
            ExternalBlockStore
        );
    }
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

#[cfg(test)]
mod rustfs_construction_async_tests {
    use std::sync::{Arc, Mutex};

    use mount_rs_core::ErrorCode;
    use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
    use mount_rs_rustfs::RustFsConfig;

    use super::{StorageContext, StoreConfig, open_blocks};

    #[tokio::test]
    async fn public_context_raw_cache_budget_accepts_zero_and_validates_pool_capacity() {
        assert!(StorageContext::new_with_raw_cache_limits(0, 132, 8).is_err());
        for (bytes, entries) in [(0, 8), (1024, 0), (132, 8)] {
            let context = StorageContext::new_with_raw_cache_limits(1, bytes, entries).unwrap();
            assert_eq!(context.raw_cache_limits(), (bytes, entries));
            assert_eq!(context.raw_cache_budget.max_charged_bytes(), bytes);
            assert_eq!(context.raw_cache_budget.max_entries(), entries);
            let unused = context.raw_cache_budget_snapshot().unwrap();
            assert_eq!(
                (unused.entries, unused.payload_bytes, unused.charged_bytes),
                (0, 0, 0)
            );
            context.close().await.unwrap();
            assert_eq!(context.raw_cache_limits(), (bytes, entries));
        }
    }

    #[tokio::test]
    #[ignore = "root-owned local RustFS fixture and fresh owned cache-budget prefix required"]
    async fn public_context_raw_cache_budget_bounds_actual_rustfs_io() {
        fn private_variable(name: &str) -> String {
            std::env::var(name).unwrap_or_else(|_| {
                panic!("root-owned cache-budget fixture configuration required")
            })
        }
        let signed = RustFsConfig {
            endpoint: private_variable("MOUNT_RS_CACHE_BUDGET_RUSTFS_ENDPOINT"),
            bucket: private_variable("MOUNT_RS_CACHE_BUDGET_RUSTFS_BUCKET"),
            access_key_id: private_variable("MOUNT_RS_CACHE_BUDGET_RUSTFS_ACCESS_KEY_ID"),
            secret_access_key: private_variable("MOUNT_RS_CACHE_BUDGET_RUSTFS_SECRET_ACCESS_KEY"),
            region: private_variable("MOUNT_RS_CACHE_BUDGET_RUSTFS_REGION"),
        };
        assert!(
            signed.endpoint.starts_with("http://127.0.0.1:")
                || signed.endpoint.starts_with("http://localhost:"),
            "fixture must be local"
        );
        signed.validate().unwrap();
        let prefix = private_variable("MOUNT_RS_CACHE_BUDGET_RUSTFS_PREFIX");
        assert!(
            prefix.starts_with("mount-rs-cache-budget-"),
            "explicit owned test prefix required"
        );
        assert!(
            signed.observe_owned_prefix_absence(&prefix).await.unwrap(),
            "owned prefix must start empty"
        );
        let scoped = |suffix: &str| StoreConfig::RustFs {
            endpoint: signed.endpoint.clone(),
            bucket: signed.bucket.clone(),
            region: signed.region.clone(),
            prefix: format!("{prefix}/{suffix}/blocks"),
            access_key_id: signed.access_key_id.clone(),
            secret_access_key: signed.secret_access_key.clone(),
            durable: true,
        };
        let context = StorageContext::new_with_raw_cache_limits(1, 132, 8).unwrap();
        let (first, first_resources) = open_blocks(&scoped("drive-a"), Some(&context), None)
            .await
            .unwrap();
        let (second, second_resources) = open_blocks(&scoped("drive-b"), Some(&context), None)
            .await
            .unwrap();
        assert!(first_resources.is_empty());
        assert!(second_resources.is_empty());
        let first_id = first.put(b"same").await.unwrap();
        assert!(
            second
                .get(&first_id)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        let second_id = second.put(b"same").await.unwrap();
        assert_eq!(first_id, second_id);
        first.flush().await.unwrap();
        second.flush().await.unwrap();
        assert_eq!(first.get_for_migration(&first_id).await.unwrap(), b"same");
        assert_eq!(second.get_for_migration(&second_id).await.unwrap(), b"same");
        assert_eq!(first.get(&first_id).await.unwrap(), b"same");
        assert_eq!(second.get(&second_id).await.unwrap(), b"same");
        // This bank must account actual opened provider entries, not just the
        // unused configuration owner retained by an inert constructor scaffold.
        let occupied = context.raw_cache_budget_snapshot().unwrap();
        first.delete(&first_id).await.unwrap();
        second.delete(&second_id).await.unwrap();
        assert!(signed.observe_owned_prefix_absence(&prefix).await.unwrap());
        drop(first);
        drop(second);
        context.close().await.unwrap();
        assert_eq!(
            (
                occupied.entries,
                occupied.payload_bytes,
                occupied.charged_bytes
            ),
            (1, 4, 132),
            "actual RustFS I/O must occupy the common configured context budget"
        );
        let released = context.raw_cache_budget_snapshot().unwrap();
        assert_eq!(
            (
                released.entries,
                released.payload_bytes,
                released.charged_bytes
            ),
            (0, 0, 0)
        );
    }

    #[derive(Default)]
    struct Journal(Mutex<Vec<Arc<dyn ConstructionResource>>>);

    impl ConstructionObserver for Journal {
        fn retain(&self, resource: Arc<dyn ConstructionResource>) {
            self.0.lock().unwrap().push(resource);
        }
    }

    fn config(endpoint: String, durable: bool) -> StoreConfig {
        StoreConfig::RustFs {
            endpoint,
            bucket: "unit-bucket".into(),
            region: "us-east-1".into(),
            prefix: "unit-prefix/nested".into(),
            access_key_id: "unit-key".into(),
            secret_access_key: "unit-secret".into(),
            durable,
        }
    }

    fn probe_config(endpoint: String) -> RustFsConfig {
        RustFsConfig {
            endpoint,
            bucket: "unit-bucket".into(),
            region: "us-east-1".into(),
            access_key_id: "unit-key".into(),
            secret_access_key: "unit-secret".into(),
        }
    }

    #[tokio::test]
    async fn sealed_client_builds_reject_before_constructor_and_leave_context_usable() {
        let context = StorageContext::new(1).unwrap();
        let journal = Journal::default();
        let sql = "mysql://unused@127.0.0.1:1/unused";
        context.tidb(sql).unwrap();
        context.seal_client_builds().unwrap();
        // Invalid configuration would produce its original validation error if
        // the constructor ran instead of rejecting sealed admission.
        let result = open_blocks(
            &config("invalid".into(), false),
            Some(&context),
            Some(&journal),
        )
        .await;
        assert_eq!(result.err().unwrap().code, ErrorCode::Estale);
        assert!(journal.0.lock().unwrap().is_empty());
        context.close_client_builds().await.unwrap();
        context.require_open().unwrap();
        open_blocks(&StoreConfig::Memory, Some(&context), None)
            .await
            .unwrap();
        context.tidb(sql).unwrap();
        assert_eq!(context.inner.lock().unwrap().tidb.len(), 1);
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn context_rustfs_preserves_durability_and_retains_constructor_tickets_without_io() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let context = StorageContext::new(1).unwrap();
        let journal = Journal::default();
        for durable in [false, true] {
            let (store, resources) = open_blocks(
                &config(endpoint.clone(), durable),
                Some(&context),
                Some(&journal),
            )
            .await
            .unwrap();
            assert_eq!(store.durable(), durable);
            assert!(resources.is_empty());
            drop(store);
        }
        let tickets = journal.0.lock().unwrap().clone();
        assert_eq!(tickets.len(), 2);
        for ticket in tickets {
            ticket.close().await.unwrap();
        }
        context.close().await.unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    #[ignore = "requires MOUNT_RS_PROFILE_IO=1 and an isolated exact test process"]
    async fn public_context_fresh_inspections_reuse_four_native_rustfs_clients() {
        use mount_rs_core::diagnostics::object_store::{ClientRole, Observer};

        let observer = Observer::enabled();
        let before = observer.snapshot().expect("set MOUNT_RS_PROFILE_IO=1");
        let context = StorageContext::new(1).unwrap();
        let first = config("http://127.0.0.1:1".into(), false);
        let mut second = config("http://127.0.0.1:1".into(), true);
        let StoreConfig::RustFs { prefix, .. } = &mut second else {
            unreachable!()
        };
        *prefix = "another-drive/blocks".into();
        // Each in-memory SQLite metadata provider recognizes a fresh noncompact
        // layout. This public inspection opens/cleans up fresh providers without
        // a persisted backing marker or any remote request.
        let metadata = StoreConfig::Sqlite {
            path: ":memory:".into(),
        };
        assert!(
            context
                .inspect_compact_layout(&metadata, &first)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            context
                .inspect_compact_layout(&metadata, &second)
                .await
                .unwrap()
                .is_none()
        );
        let inspected = observer.snapshot().unwrap();
        context.close().await.unwrap();
        let closed = observer.snapshot().unwrap();

        assert_eq!(inspected.bundles.committed - before.bundles.committed, 2);
        assert_eq!(inspected.bundles.live, before.bundles.live);
        assert_eq!(inspected.cache.created - before.cache.created, 2);
        assert_eq!(inspected.cache.live, before.cache.live);
        for role in [
            ClientRole::PrimaryDataMixed,
            ClientRole::QualificationData,
            ClientRole::PrimaryProbeMixed,
            ClientRole::QualificationProbe,
        ] {
            let prior = before.clients[role.index()];
            let actual = inspected.clients[role.index()];
            assert_eq!(actual.build.started - prior.build.started, 1, "{role:?}");
            assert_eq!(
                actual.build.succeeded - prior.build.succeeded,
                1,
                "{role:?}"
            );
            assert_eq!(actual.constructed - prior.constructed, 1, "{role:?}");
            assert_eq!(closed.clients[role.index()].live, prior.live, "{role:?}");
            assert!(
                actual
                    .http
                    .iter()
                    .zip(prior.http)
                    .all(|(after, before)| { after.attempts_started == before.attempts_started }),
                "fresh unmarked inspection must issue no backing requests"
            );
        }
        assert!(!closed.saturated);
    }

    #[tokio::test]
    async fn context_rustfs_preserves_invalid_configuration_errors_and_observer_ownership() {
        let context = StorageContext::new(1).unwrap();
        let journal = Journal::default();
        for invalid_field in 0..5 {
            let mut options = config("http://127.0.0.1:1".into(), false);
            let StoreConfig::RustFs {
                endpoint,
                bucket,
                region,
                access_key_id,
                secret_access_key,
                ..
            } = &mut options
            else {
                unreachable!()
            };
            match invalid_field {
                0 => *endpoint = "invalid".into(),
                1 => bucket.clear(),
                2 => region.clear(),
                3 => access_key_id.clear(),
                _ => secret_access_key.clear(),
            }
            let original = super::RustFsBlockStore::from_config(
                &RustFsConfig {
                    endpoint: endpoint.clone(),
                    bucket: bucket.clone(),
                    region: region.clone(),
                    access_key_id: access_key_id.clone(),
                    secret_access_key: secret_access_key.clone(),
                },
                "unit-prefix/nested",
                false,
            )
            .err()
            .expect("invalid configuration must reject the original constructor");
            let result = open_blocks(&options, Some(&context), Some(&journal)).await;
            let retained = result.err().unwrap();
            assert_eq!(retained.code, original.code);
            assert_eq!(retained.to_string(), original.to_string());
        }
        let tickets = journal.0.lock().unwrap().clone();
        assert_eq!(tickets.len(), 5);
        for ticket in tickets {
            ticket.close().await.unwrap();
        }
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn owned_prefix_preparation_rejects_invalid_sealed_and_closed_without_list() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let context = StorageContext::new(1).unwrap();
        let journal = Journal::default();
        let invalid = context
            .prepare_rustfs_owned_prefix_probe(
                probe_config(endpoint.clone()),
                "../invalid".into(),
                Some(&journal),
            )
            .await;
        assert_eq!(invalid.err().unwrap().code, ErrorCode::Einval);
        assert!(journal.0.lock().unwrap().is_empty());
        let probe = context
            .prepare_rustfs_owned_prefix_probe(
                probe_config(endpoint.clone()),
                "owned/nested".into(),
                Some(&journal),
            )
            .await
            .unwrap();
        assert_eq!(journal.0.lock().unwrap().len(), 1);
        drop(probe);
        context.seal_client_builds().unwrap();
        let sealed = context
            .prepare_rustfs_owned_prefix_probe(
                probe_config(endpoint.clone()),
                "owned/nested".into(),
                Some(&journal),
            )
            .await;
        assert_eq!(sealed.err().unwrap().code, ErrorCode::Estale);
        context.close().await.unwrap();
        let closed = context
            .prepare_rustfs_owned_prefix_probe(
                probe_config(endpoint),
                "owned/nested".into(),
                Some(&journal),
            )
            .await;
        assert_eq!(closed.err().unwrap().code, ErrorCode::Estale);
        assert_eq!(journal.0.lock().unwrap().len(), 1);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn canceled_public_rustfs_admission_keeps_outer_journal_uncertain_after_client_drain() {
        use std::future::{Future, poll_fn};
        use std::sync::Condvar;
        use std::task::Poll;
        use std::time::Duration;

        use crate::{ConstructionJournal, Filesystem, SplitOptions};
        use mount_rs_rustfs::RustFsConstructionContext;

        const BOUND: Duration = Duration::from_secs(5);

        #[derive(Default)]
        struct HeldObserver {
            tickets: Journal,
            entered: tokio::sync::Notify,
            released: Mutex<bool>,
            changed: Condvar,
        }
        impl HeldObserver {
            fn release(&self) {
                *self.released.lock().unwrap() = true;
                self.changed.notify_all();
            }
        }
        impl ConstructionObserver for HeldObserver {
            fn retain(&self, resource: Arc<dyn ConstructionResource>) {
                self.tickets.retain(resource);
                self.entered.notify_one();
                // Finite safety bound protects failed assertions; it does not
                // establish admission ordering or a scheduling deadline.
                let (released, _) = self
                    .changed
                    .wait_timeout_while(self.released.lock().unwrap(), BOUND * 2, |released| {
                        !*released
                    })
                    .unwrap();
                let was_released = *released;
                drop(released);
                assert!(was_released, "held registration was not released");
            }
        }
        struct ReleaseOnDrop(Arc<HeldObserver>);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                self.0.release();
            }
        }
        #[derive(Default)]
        struct OuterObserver {
            journal: ConstructionJournal,
            groups: Journal,
        }
        impl ConstructionObserver for OuterObserver {
            fn retain(&self, resource: Arc<dyn ConstructionResource>) {
                self.journal.retain(resource.clone());
                self.groups.retain(resource);
            }
        }

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let mut context = StorageContext::new(1).unwrap();
        context.rustfs = RustFsConstructionContext::new(1).unwrap();
        let held = Arc::new(HeldObserver::default());
        let _release_on_drop = ReleaseOnDrop(held.clone());
        let owner = context.rustfs.clone();
        let held_observer = held.clone();
        let held_config = probe_config(endpoint.clone());
        let reservation = tokio::spawn(async move {
            owner
                .block_store(held_config, "reserved".into(), false, Some(&*held_observer))
                .await
        });
        tokio::time::timeout(BOUND, held.entered.notified())
            .await
            .unwrap();
        let tickets = held.tickets.0.lock().unwrap().clone();
        assert_eq!(tickets.len(), 1);

        let outer = OuterObserver::default();
        let attempt = outer.journal.begin().unwrap();
        let mut options = SplitOptions::memory("pending-rustfs-admission", 4096);
        options.blocks = config(endpoint, false);
        let mut opening = Box::pin(Filesystem::split_with_context_and_construction_observer(
            options, &context, &outer,
        ));
        assert!(
            poll_fn(|cx| Poll::Ready(opening.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(outer.journal.snapshot().opening);
        assert_eq!(outer.journal.snapshot().retained_resources, 1);
        drop(opening);
        drop(attempt);
        let groups = outer.groups.0.lock().unwrap().clone();
        assert_eq!(groups.len(), 1);
        assert!(outer.journal.snapshot().uncertain);
        assert!(outer.journal.close().await.is_err());
        assert!(groups[0].close().await.is_err());

        context.seal_client_builds().unwrap();
        let mut draining = Box::pin(context.close_client_builds());
        assert!(
            poll_fn(|cx| Poll::Ready(draining.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        held.release();
        let rejected = tokio::time::timeout(BOUND, reservation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rejected.err().unwrap().code, ErrorCode::Estale);
        tokio::time::timeout(BOUND, draining)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(BOUND, tickets[0].close())
            .await
            .unwrap()
            .unwrap();

        assert!(outer.journal.close().await.is_err());
        assert!(groups[0].close().await.is_err());
        let snapshot = outer.journal.snapshot();
        assert!(snapshot.uncertain);
        assert!(!snapshot.cleanup_complete);
        assert_eq!(snapshot.retained_resources, 1);
        context.require_open().unwrap();
        open_blocks(&StoreConfig::Memory, Some(&context), None)
            .await
            .unwrap();
        context.close().await.unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    async fn poisoned_context_close_still_seals_constructors_and_closes_retained_sql() {
        let context = StorageContext::new(1).unwrap();
        let sql = context.tidb("mysql://unused@127.0.0.1:1/unused").unwrap();
        let inner = context.inner.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = inner.lock().unwrap();
                panic!("poison SDK map for close control");
            })
            .join()
            .is_err()
        );
        assert!(context.close().await.is_err());
        assert_eq!(
            sql.metadata(mount_rs_tidb::TidbStorageOptions::new("closed"))
                .await
                .err()
                .unwrap()
                .code,
            ErrorCode::Estale,
        );
        let result = context
            .rustfs
            .block_store(probe_config("invalid".into()), "unit".into(), false, None)
            .await;
        assert_eq!(result.err().unwrap().code, ErrorCode::Estale);
        assert!(context.close().await.is_err());
    }
}

#[cfg(test)]
mod context_close_poll_tests {
    use std::future::{Future, poll_fn};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    use mount_rs_core::{ErrorCode, FsError, Result};

    use super::close_context_resources;

    #[derive(Clone, Default)]
    struct CloseControl {
        polls: Arc<AtomicUsize>,
        released: Arc<AtomicBool>,
        error: Option<ErrorCode>,
    }

    impl Future for CloseControl {
        type Output = Result<()>;

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            if self.released.load(Ordering::SeqCst) {
                Poll::Ready(self.error.map_or(Ok(()), |code| Err(FsError::new(code))))
            } else {
                // Tests explicitly poll again after release, so no timing or
                // scheduler behavior participates in these controls.
                Poll::Pending
            }
        }
    }

    #[tokio::test]
    async fn direct_close_polls_all_families_despite_initial_error_before_cancel_and_resume() {
        let constructor = CloseControl::default();
        let pools = [CloseControl::default(), CloseControl::default()];
        let mut closing = Box::pin(close_context_resources(
            constructor.clone(),
            pools
                .iter()
                .cloned()
                .map(|pool| Some(Box::pin(pool)))
                .collect(),
            Some(FsError::new(ErrorCode::Eio)),
        ));
        assert!(
            poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        for control in [&constructor, &pools[0], &pools[1]] {
            assert_eq!(control.polls.load(Ordering::SeqCst), 1);
        }
        drop(closing);
        // The resource owners outlive the canceled waiter. Recreate only its
        // waiting futures, as StorageContext does from its retained owners.
        for control in [&constructor, &pools[0], &pools[1]] {
            control.released.store(true, Ordering::SeqCst);
        }
        let resumed = close_context_resources(
            constructor.clone(),
            pools
                .iter()
                .cloned()
                .map(|pool| Some(Box::pin(pool)))
                .collect(),
            Some(FsError::new(ErrorCode::Eio)),
        )
        .await;
        assert_eq!(resumed.unwrap_err().code, ErrorCode::Eio);
        for control in [&constructor, &pools[0], &pools[1]] {
            assert_eq!(control.polls.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn direct_close_drains_pending_peers_after_constructor_and_pool_errors() {
        let constructor = CloseControl {
            error: Some(ErrorCode::Eio),
            ..CloseControl::default()
        };
        let failed_pool = CloseControl {
            error: Some(ErrorCode::Estale),
            ..CloseControl::default()
        };
        let held_pool = CloseControl::default();
        constructor.released.store(true, Ordering::SeqCst);
        failed_pool.released.store(true, Ordering::SeqCst);
        let mut closing = Box::pin(close_context_resources(
            constructor.clone(),
            vec![
                Some(Box::pin(failed_pool.clone())),
                Some(Box::pin(held_pool.clone())),
            ],
            None,
        ));
        assert!(
            poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        for control in [&constructor, &failed_pool, &held_pool] {
            assert_eq!(control.polls.load(Ordering::SeqCst), 1);
        }
        held_pool.released.store(true, Ordering::SeqCst);
        assert_eq!(closing.await.unwrap_err().code, ErrorCode::Eio);
        assert_eq!(held_pool.polls.load(Ordering::SeqCst), 2);
        assert_eq!(constructor.polls.load(Ordering::SeqCst), 1);
        assert_eq!(failed_pool.polls.load(Ordering::SeqCst), 1);
    }
}
