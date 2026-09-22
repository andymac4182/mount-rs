//! Bridge from mount-rs's fenced metadata/immutable-block contracts to the
//! synchronous SQLite VFS boundary.
//!
//! The bridge is intentionally generic over the provider pair.  That keeps
//! the crate's dependency surface small while allowing the same code to be
//! instantiated with memory stores for volatile tests, durable SQLite stores,
//! R2 blocks plus a fenced metadata store, or PGlite metadata/blocks.  The
//! provider pair is still responsible for its own durability declaration.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::os::raw::{c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mount_rs_core::chunking::ChunkerConfig;
use mount_rs_core::storage::{
    BlockExtent, BlockStore, DirectoryEntry, FileLayout, InodeId, LoadedMetadata, MetadataStore,
    NAMESPACE_FORMAT_VERSION, Namespace, NodeData, NodeMetadata, WriterLease,
};
use mount_rs_core::types::{S_IFDIR, S_IFREG, Stats, now_ms};
use mount_rs_core::{ErrorCode, FsError};
use rusqlite::ffi;

use crate::{
    AccessMode, Backend, FileKind, LockLevel, OpenOptions, VfsError, VfsFile, WalCapability,
    WalScope,
};

/// Version of the storage bridge state/layout contract.
pub const STORAGE_BRIDGE_VERSION: u32 = 1;

/// A runtime-neutral way to invoke an async provider from a synchronous VFS
/// callback. The default executor is suitable for the existing memory and
/// SQLite providers, whose async trait methods complete without an external
/// reactor. Providers that need a reactor must supply an executor such as
/// `TokioExecutor` (when the optional Tokio feature is enabled) and follow
/// that executor's runtime requirements.
pub trait BlockingExecutor: Clone + Send + Sync + 'static {
    fn block_on<F: Future>(&self, future: F) -> F::Output;
}

/// Small executor for provider futures that do not require an I/O reactor.
#[derive(Clone, Copy, Debug, Default)]
pub struct InlineExecutor;

impl BlockingExecutor for InlineExecutor {
    fn block_on<F: Future>(&self, future: F) -> F::Output {
        let waker = std::task::Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut context = std::task::Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match Future::poll(future.as_mut(), &mut context) {
                std::task::Poll::Ready(value) => return value,
                std::task::Poll::Pending => std::thread::park(),
            }
        }
    }
}

struct ThreadWake(std::thread::Thread);

impl std::task::Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Tokio adapter for providers whose futures need a Tokio I/O reactor.
///
/// Calls made outside Tokio use [`tokio::runtime::Handle::block_on`]. Calls
/// made from a Tokio multi-thread worker use `block_in_place` before entering
/// the handle, so the worker is handed off rather than deadlocking itself.
/// Calling a VFS callback from a current-thread Tokio runtime is unsupported
/// and fails fast with a diagnostic panic: that runtime has no worker it can
/// hand off while the synchronous SQLite callback waits.
#[cfg(feature = "tokio-executor")]
#[derive(Clone, Debug)]
pub struct TokioExecutor {
    handle: tokio::runtime::Handle,
}

#[cfg(feature = "tokio-executor")]
impl TokioExecutor {
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self { handle }
    }

    pub fn current() -> Result<Self, VfsError> {
        tokio::runtime::Handle::try_current()
            .map(Self::new)
            .map_err(|_| VfsError::Unsupported("TokioExecutor requires a running Tokio runtime"))
    }

    pub fn handle(&self) -> &tokio::runtime::Handle {
        &self.handle
    }
}

#[cfg(feature = "tokio-executor")]
impl BlockingExecutor for TokioExecutor {
    fn block_on<F: Future>(&self, future: F) -> F::Output {
        if let Ok(current) = tokio::runtime::Handle::try_current() {
            if matches!(
                current.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::CurrentThread
            ) {
                panic!(
                    "TokioExecutor cannot run a synchronous SQLite callback from a current-thread Tokio runtime; use a multi-thread runtime or a dedicated runtime thread"
                );
            }
            tokio::task::block_in_place(|| self.handle.block_on(future))
        } else {
            self.handle.block_on(future)
        }
    }
}

/// Controls whether volatile providers may be used for engine tests.
///
/// The fields remain public for configuration interoperability, so callers can
/// construct or mutate this value directly. `StorageBackend::with_executor`
/// validates all invariants again before it inspects either provider.
#[derive(Clone, Debug)]
pub struct StorageOptions {
    /// Provider-owned lease owner. It must be unique for one live process.
    pub owner: String,
    pub lease_ttl: Duration,
    /// Fixed publication chunk size. Existing published layouts retain this
    /// value; changing it creates a new bridge volume, not a reinterpretation.
    pub chunk_size: usize,
    /// Require both providers to report `durable() == true`.
    pub require_durable: bool,
}

impl StorageOptions {
    pub fn new(owner: impl Into<String>, chunk_size: usize) -> Result<Self, VfsError> {
        let options = Self {
            owner: owner.into(),
            lease_ttl: Duration::from_secs(30),
            chunk_size,
            require_durable: true,
        };
        options.validate()?;
        Ok(options)
    }

    fn validate(&self) -> Result<(), VfsError> {
        if self.owner.is_empty() {
            return Err(VfsError::InvalidInput("storage owner must not be empty"));
        }
        if self.lease_ttl.is_zero() {
            return Err(VfsError::InvalidInput("storage lease TTL must be positive"));
        }
        if self.chunk_size == 0 {
            return Err(VfsError::InvalidInput(
                "storage chunk size must be positive",
            ));
        }
        Ok(())
    }

    pub fn volatile_for_tests(
        owner: impl Into<String>,
        chunk_size: usize,
    ) -> Result<Self, VfsError> {
        let mut options = Self::new(owner, chunk_size)?;
        options.require_durable = false;
        Ok(options)
    }

    pub fn with_lease_ttl(mut self, lease_ttl: Duration) -> Result<Self, VfsError> {
        self.lease_ttl = lease_ttl;
        self.validate()?;
        Ok(self)
    }
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            owner: format!("mount-rs-sqlite-vfs-{}", std::process::id()),
            lease_ttl: Duration::from_secs(30),
            chunk_size: 4096,
            require_durable: true,
        }
    }
}

/// A SQLite VFS backend backed by one fenced metadata provider and one
/// immutable block provider.
pub struct StorageBackend<M, B, E = InlineExecutor>
where
    M: MetadataStore,
    B: BlockStore,
    E: BlockingExecutor,
{
    volume: Arc<StorageVolume<M, B, E>>,
}

impl<M, B> StorageBackend<M, B, InlineExecutor>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    /// Build the bridge used by memory and synchronous SQLite providers.
    pub fn new(metadata: M, blocks: B, options: StorageOptions) -> Result<Self, VfsError> {
        Self::with_executor(metadata, blocks, options, InlineExecutor)
    }
}

impl<M, B, E> StorageBackend<M, B, E>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
    E: BlockingExecutor,
{
    pub fn with_executor(
        metadata: M,
        blocks: B,
        options: StorageOptions,
        executor: E,
    ) -> Result<Self, VfsError> {
        options.validate()?;
        if options.require_durable && (!metadata.durable() || !blocks.durable()) {
            return Err(VfsError::Unsupported(
                "SQLite storage bridge requires durable metadata and block providers",
            ));
        }
        Ok(Self {
            volume: Arc::new(StorageVolume {
                metadata: Arc::new(metadata),
                blocks: Arc::new(blocks),
                executor,
                options,
                state: Mutex::new(VolumeState::default()),
                next_handle: AtomicU64::new(1),
                wal_regions: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn storage_bridge_version(&self) -> u32 {
        STORAGE_BRIDGE_VERSION
    }

    pub fn provider_is_durable(&self) -> bool {
        self.volume.metadata.durable() && self.volume.blocks.durable()
    }
}

impl<M, B, E> Clone for StorageBackend<M, B, E>
where
    M: MetadataStore,
    B: BlockStore,
    E: BlockingExecutor,
{
    fn clone(&self) -> Self {
        Self {
            volume: Arc::clone(&self.volume),
        }
    }
}

impl<M, B, E> Backend for StorageBackend<M, B, E>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
    E: BlockingExecutor,
{
    fn wal_capability(&self) -> WalCapability {
        WalCapability {
            scope: WalScope::ProcessLocal,
            durable: self.provider_is_durable(),
        }
    }

    fn open(&self, name: &[u8], options: OpenOptions) -> Result<Box<dyn VfsFile>, VfsError> {
        if options.wal_scope != WalScope::Disabled && options.wal_scope != WalScope::ProcessLocal {
            return Err(VfsError::Unsupported(
                "storage bridge only supports process-local WAL",
            ));
        }
        if !options.create && !self.volume.file_exists(name)? {
            return Err(VfsError::NotFound);
        }
        let bytes = self.volume.load_file(name)?;
        let handle_id = self.volume.next_handle_id()?;
        let is_wal_file = name.ends_with(b"-wal");
        let wal_connection = (options.wal_scope == WalScope::ProcessLocal
            && options.kind == FileKind::MainDatabase)
            .then(|| self.volume.wal_connection(name, handle_id))
            .transpose()?;
        Ok(Box::new(StorageFile {
            volume: Arc::clone(&self.volume),
            name: name.to_vec(),
            kind: options.kind,
            wal_scope: options.wal_scope,
            is_wal_file,
            read_only: options.read_only,
            delete_on_close: options.delete_on_close,
            bytes,
            dirty: false,
            level: LockLevel::None,
            handle_id,
            associated_owner: None,
            owns_volume: false,
            can_write: false,
            wal_connection,
        }))
    }

    fn delete(&self, name: &[u8], _sync_dir: bool) -> Result<(), VfsError> {
        self.volume.delete_file(name)
    }

    fn access(&self, name: &[u8], _mode: AccessMode) -> Result<bool, VfsError> {
        self.volume.file_exists(name)
    }

    fn full_pathname(&self, name: &[u8]) -> Result<Vec<u8>, VfsError> {
        if name.is_empty() || name.contains(&0) {
            return Err(VfsError::InvalidInput(
                "storage path must be non-empty and NUL-free",
            ));
        }
        if name.first() == Some(&b'/') {
            Ok(name.to_vec())
        } else {
            let mut path = Vec::with_capacity(name.len() + 1);
            path.push(b'/');
            path.extend_from_slice(name);
            Ok(path)
        }
    }

    fn randomness(&self, output: &mut [u8]) -> Result<(), VfsError> {
        crate::fill_randomness(output)
    }

    fn temporary_name(&self) -> Result<Vec<u8>, VfsError> {
        self.volume.temporary_name()
    }
}

struct StorageVolume<M, B, E>
where
    M: MetadataStore,
    B: BlockStore,
    E: BlockingExecutor,
{
    metadata: Arc<M>,
    blocks: Arc<B>,
    executor: E,
    options: StorageOptions,
    state: Mutex<VolumeState>,
    next_handle: AtomicU64,
    wal_regions: Mutex<HashMap<Vec<u8>, Arc<ProcessWalRegion>>>,
}

#[derive(Default)]
struct VolumeState {
    writer: Option<WriterState>,
    failed: Option<String>,
}

struct WriterState {
    handle_id: u64,
    lease: WriterLease,
    revision: u64,
    namespace: Option<Namespace>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessShmLockMode {
    None,
    Shared,
    Exclusive,
}

#[derive(Default)]
struct ProcessShmSlot {
    shared: BTreeSet<u64>,
    exclusive: Option<u64>,
}

struct ProcessWalState {
    region_size: Option<usize>,
    regions: Vec<Box<[u8]>>,
    slots: [ProcessShmSlot; ffi::SQLITE_SHM_NLOCK as usize],
}

struct ProcessWalRegion {
    state: Mutex<ProcessWalState>,
}

struct ProcessWalConnection {
    region: Arc<ProcessWalRegion>,
    id: u64,
    held: [ProcessShmLockMode; ffi::SQLITE_SHM_NLOCK as usize],
}

impl ProcessWalRegion {
    fn new() -> Self {
        Self {
            state: Mutex::new(ProcessWalState {
                region_size: None,
                regions: Vec::new(),
                slots: std::array::from_fn(|_| ProcessShmSlot::default()),
            }),
        }
    }
}

impl ProcessWalConnection {
    fn new(region: Arc<ProcessWalRegion>, id: u64) -> Self {
        Self {
            region,
            id,
            held: [ProcessShmLockMode::None; ffi::SQLITE_SHM_NLOCK as usize],
        }
    }

    fn map(
        &mut self,
        page: usize,
        page_size: usize,
        extend: bool,
    ) -> Result<*mut c_void, VfsError> {
        let mut state = self
            .region
            .state
            .lock()
            .map_err(|_| VfsError::Other("process-local WAL state lock poisoned".to_owned()))?;
        if page_size == 0 {
            return Err(VfsError::InvalidInput("WAL shared-memory region is empty"));
        }
        if let Some(existing) = state.region_size
            && existing != page_size
        {
            return Err(VfsError::InvalidInput(
                "WAL shared-memory region size changed",
            ));
        }
        state.region_size = Some(page_size);
        if page >= state.regions.len() {
            if !extend {
                return Ok(ptr::null_mut());
            }
            state
                .regions
                .resize_with(page + 1, || vec![0_u8; page_size].into_boxed_slice());
        }
        Ok(state.regions[page].as_mut_ptr().cast::<c_void>())
    }

    fn lock(&mut self, offset: usize, number: usize, flags: c_int) -> Result<(), VfsError> {
        let locking = flags & ffi::SQLITE_SHM_LOCK != 0;
        let mode = if flags & ffi::SQLITE_SHM_SHARED != 0 {
            ProcessShmLockMode::Shared
        } else {
            ProcessShmLockMode::Exclusive
        };
        let end = offset
            .checked_add(number)
            .ok_or(VfsError::InvalidInput("WAL lock range overflows"))?;
        if end > self.held.len() {
            return Err(VfsError::InvalidInput("WAL lock range is outside SQLite"));
        }
        let mut state = self
            .region
            .state
            .lock()
            .map_err(|_| VfsError::Other("process-local WAL state lock poisoned".to_owned()))?;
        if !locking {
            for index in offset..end {
                if self.held[index] != mode {
                    if self.held[index] == ProcessShmLockMode::None {
                        continue;
                    }
                    return Err(VfsError::InvalidInput(
                        "WAL lock unlock mode does not match the held mode",
                    ));
                }
            }
            for index in offset..end {
                match mode {
                    ProcessShmLockMode::Shared => {
                        state.slots[index].shared.remove(&self.id);
                    }
                    ProcessShmLockMode::Exclusive => {
                        if state.slots[index].exclusive == Some(self.id) {
                            state.slots[index].exclusive = None;
                        }
                    }
                    ProcessShmLockMode::None => {}
                }
                self.held[index] = ProcessShmLockMode::None;
            }
            return Ok(());
        }

        for index in offset..end {
            let slot = &state.slots[index];
            match mode {
                ProcessShmLockMode::Shared => {
                    if slot.exclusive.is_some() && slot.exclusive != Some(self.id) {
                        return Err(VfsError::Busy);
                    }
                    if slot.exclusive == Some(self.id) {
                        return Err(VfsError::InvalidInput(
                            "WAL shared lock cannot replace an exclusive lock",
                        ));
                    }
                }
                ProcessShmLockMode::Exclusive => {
                    if slot.exclusive.is_some() && slot.exclusive != Some(self.id) {
                        return Err(VfsError::Busy);
                    }
                    if slot.exclusive == Some(self.id) {
                        continue;
                    }
                    if slot.shared.iter().any(|owner| *owner != self.id) {
                        return Err(VfsError::Busy);
                    }
                    if slot.shared.contains(&self.id) {
                        return Err(VfsError::InvalidInput(
                            "WAL exclusive lock cannot replace a shared lock",
                        ));
                    }
                }
                ProcessShmLockMode::None => unreachable!(),
            }
        }
        for index in offset..end {
            match mode {
                ProcessShmLockMode::Shared => {
                    state.slots[index].shared.insert(self.id);
                }
                ProcessShmLockMode::Exclusive => {
                    state.slots[index].exclusive = Some(self.id);
                }
                ProcessShmLockMode::None => unreachable!(),
            }
            self.held[index] = mode;
        }
        Ok(())
    }

    fn barrier(&mut self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }

    fn unmap(&mut self) -> Result<(), VfsError> {
        let mut state = self
            .region
            .state
            .lock()
            .map_err(|_| VfsError::Other("process-local WAL state lock poisoned".to_owned()))?;
        for index in 0..self.held.len() {
            match self.held[index] {
                ProcessShmLockMode::Shared => {
                    state.slots[index].shared.remove(&self.id);
                }
                ProcessShmLockMode::Exclusive => {
                    if state.slots[index].exclusive == Some(self.id) {
                        state.slots[index].exclusive = None;
                    }
                }
                ProcessShmLockMode::None => {}
            }
            self.held[index] = ProcessShmLockMode::None;
        }
        Ok(())
    }
}

impl Drop for ProcessWalConnection {
    fn drop(&mut self) {
        let _ = self.unmap();
    }
}

impl<M, B, E> StorageVolume<M, B, E>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
    E: BlockingExecutor,
{
    fn load_metadata(&self) -> Result<LoadedMetadata, VfsError> {
        let loaded = self
            .executor
            .block_on(self.metadata.load())
            .map_err(storage_error)?;
        loaded.validate().map_err(storage_error)?;
        Ok(loaded)
    }

    fn load_file(&self, name: &[u8]) -> Result<Vec<u8>, VfsError> {
        self.ensure_healthy()?;
        let loaded = self.load_metadata()?;
        let Some(namespace) = loaded.namespace else {
            return Ok(Vec::new());
        };
        let entry_name = encoded_name(name);
        let root = namespace.nodes.get(&namespace.root).ok_or(VfsError::Other(
            "published storage namespace has no root".to_owned(),
        ))?;
        let inode = match &root.data {
            NodeData::Directory { entries } => entries
                .iter()
                .find(|entry| entry.name == entry_name)
                .map(|entry| entry.inode),
            _ => {
                return Err(VfsError::Other(
                    "published storage root is not a directory".to_owned(),
                ));
            }
        };
        let Some(inode) = inode else {
            return Ok(Vec::new());
        };
        let node = namespace.nodes.get(&inode).ok_or(VfsError::Other(
            "published storage entry is missing".to_owned(),
        ))?;
        let NodeData::File(layout) = &node.data else {
            return Err(VfsError::Other(
                "published storage entry is not a file".to_owned(),
            ));
        };
        let size = usize::try_from(node.stats.size)
            .map_err(|_| VfsError::InvalidInput("published SQLite file is too large"))?;
        let mut bytes = vec![0_u8; size];
        for extent in &layout.extents {
            let block = self
                .executor
                .block_on(self.blocks.get(&extent.block))
                .map_err(storage_error)?;
            let source_start = usize::try_from(extent.block_offset)
                .map_err(|_| VfsError::Other("block offset overflows usize".to_owned()))?;
            let destination_start = usize::try_from(extent.file_offset)
                .map_err(|_| VfsError::Other("file offset overflows usize".to_owned()))?;
            let length = usize::try_from(extent.length)
                .map_err(|_| VfsError::Other("block extent length overflows usize".to_owned()))?;
            let source_end = source_start
                .checked_add(length)
                .ok_or(VfsError::Other("block extent overflows source".to_owned()))?;
            let destination_end = destination_start
                .checked_add(length)
                .ok_or(VfsError::Other(
                    "block extent overflows destination".to_owned(),
                ))?;
            if source_end > block.len() || destination_end > bytes.len() {
                return Err(VfsError::Other(
                    "published block extent is out of bounds".to_owned(),
                ));
            }
            bytes[destination_start..destination_end]
                .copy_from_slice(&block[source_start..source_end]);
        }
        Ok(bytes)
    }

    fn file_exists(&self, name: &[u8]) -> Result<bool, VfsError> {
        self.ensure_healthy()?;
        let loaded = self.load_metadata()?;
        let Some(namespace) = loaded.namespace else {
            return Ok(false);
        };
        let Some(root) = namespace.nodes.get(&namespace.root) else {
            return Err(VfsError::Other(
                "published storage namespace has no root".to_owned(),
            ));
        };
        let NodeData::Directory { entries } = &root.data else {
            return Err(VfsError::Other(
                "published storage root is not a directory".to_owned(),
            ));
        };
        Ok(entries.iter().any(|entry| entry.name == encoded_name(name)))
    }

    fn next_handle_id(&self) -> Result<u64, VfsError> {
        self.next_handle
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| VfsError::InvalidInput("storage file handle id overflow"))
    }

    fn wal_connection(
        &self,
        database_name: &[u8],
        handle_id: u64,
    ) -> Result<ProcessWalConnection, VfsError> {
        let database_name = database_name
            .strip_suffix(b"-wal")
            .unwrap_or(database_name)
            .to_vec();
        let mut regions = self
            .wal_regions
            .lock()
            .map_err(|_| VfsError::Other("storage WAL registry lock poisoned".to_owned()))?;
        let region = regions
            .entry(database_name)
            .or_insert_with(|| Arc::new(ProcessWalRegion::new()));
        Ok(ProcessWalConnection::new(Arc::clone(region), handle_id))
    }

    fn ensure_healthy_state(state: &VolumeState) -> Result<(), VfsError> {
        if let Some(failure) = &state.failed {
            return Err(VfsError::Other(format!(
                "storage bridge is failed closed: {failure}"
            )));
        }
        Ok(())
    }

    fn ensure_healthy(&self) -> Result<(), VfsError> {
        let state = self.lock_state()?;
        Self::ensure_healthy_state(&state)
    }

    fn acquire_owner(&self, handle_id: u64) -> Result<(), VfsError> {
        let mut state = self.lock_state()?;
        Self::ensure_healthy_state(&state)?;
        if state.writer.is_some() {
            return Err(VfsError::Busy);
        }

        let lease = self
            .executor
            .block_on(
                self.metadata
                    .acquire_writer(&self.options.owner, self.options.lease_ttl),
            )
            .map_err(storage_error)?;
        let loaded = match self.load_metadata() {
            Ok(loaded) => loaded,
            Err(error) => {
                if let Err(release_error) =
                    self.executor.block_on(self.metadata.release_writer(&lease))
                {
                    let release_error = storage_error(release_error);
                    state.failed = Some(release_error.to_string());
                }
                return Err(error);
            }
        };
        state.writer = Some(WriterState {
            handle_id,
            lease,
            revision: loaded.revision,
            namespace: loaded.namespace,
        });
        Ok(())
    }

    fn ensure_owner(&self, handle_id: u64) -> Result<(), VfsError> {
        let mut state = self.lock_state()?;
        self.ensure_owner_locked(&mut state, handle_id)
    }

    fn ensure_owner_locked(&self, state: &mut VolumeState, handle_id: u64) -> Result<(), VfsError> {
        Self::ensure_healthy_state(state)?;
        let Some(writer) = state.writer.as_ref() else {
            return Err(VfsError::Busy);
        };
        if writer.handle_id != handle_id {
            return Err(VfsError::Busy);
        }
        let lease = writer.lease.clone();
        let renewed = match self
            .executor
            .block_on(self.metadata.renew_writer(&lease, self.options.lease_ttl))
        {
            Ok(renewed) => renewed,
            Err(error) => {
                let error = storage_error(error);
                state.failed = Some(error.to_string());
                return Err(error);
            }
        };
        let writer = state.writer.as_mut().ok_or(VfsError::Busy)?;
        writer.lease = renewed;
        Ok(())
    }

    fn release_owner(&self, handle_id: u64) -> Result<(), VfsError> {
        let mut state = self.lock_state()?;
        Self::ensure_healthy_state(&state)?;
        let Some(writer) = state.writer.as_ref() else {
            return Ok(());
        };
        if writer.handle_id != handle_id {
            return Err(VfsError::Busy);
        }
        let lease = writer.lease.clone();
        match self.executor.block_on(self.metadata.release_writer(&lease)) {
            Ok(()) => {
                state.writer = None;
                Ok(())
            }
            Err(error) => {
                let error = storage_error(error);
                state.failed = Some(error.to_string());
                Err(error)
            }
        }
    }

    fn current_owner(&self) -> Result<Option<u64>, VfsError> {
        let state = self.lock_state()?;
        Self::ensure_healthy_state(&state)?;
        Ok(state.writer.as_ref().map(|writer| writer.handle_id))
    }

    fn commit_file(&self, name: &[u8], bytes: &[u8], handle_id: u64) -> Result<(), VfsError> {
        self.ensure_owner(handle_id)?;
        let mut state = self.lock_state()?;
        let result = self.commit_file_locked(&mut state, name, bytes, handle_id);
        if let Err(error) = &result {
            state.failed = Some(error.to_string());
        }
        result
    }

    fn commit_file_locked(
        &self,
        state: &mut VolumeState,
        name: &[u8],
        bytes: &[u8],
        handle_id: u64,
    ) -> Result<(), VfsError> {
        Self::ensure_healthy_state(state)?;
        if state.writer.as_ref().map(|writer| writer.handle_id) != Some(handle_id) {
            return Err(VfsError::Busy);
        }
        self.ensure_owner_locked(state, handle_id)?;
        let chunk_size = self.options.chunk_size;
        let mut block_extents = Vec::new();
        for (index, chunk) in bytes.chunks(chunk_size).enumerate() {
            let block = self
                .executor
                .block_on(self.blocks.put(chunk))
                .map_err(storage_error)?;
            block_extents.push(BlockExtent {
                file_offset: u64::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_mul(chunk_size as u64))
                    .ok_or(VfsError::InvalidInput("SQLite file offset overflows u64"))?,
                block,
                block_offset: 0,
                length: u64::try_from(chunk.len())
                    .map_err(|_| VfsError::InvalidInput("SQLite block length overflows u64"))?,
            });
        }
        self.executor
            .block_on(self.blocks.flush())
            .map_err(storage_error)?;

        self.ensure_owner_locked(state, handle_id)?;
        let (revision, lease, mut namespace) = {
            let writer = state.writer.as_ref().ok_or(VfsError::Busy)?;
            (
                writer.revision,
                writer.lease.clone(),
                writer
                    .namespace
                    .clone()
                    .unwrap_or_else(|| empty_namespace(chunk_size)),
            )
        };
        upsert_file(
            &mut namespace,
            name,
            bytes.len() as u64,
            block_extents,
            chunk_size,
        )?;
        namespace.validate().map_err(storage_error)?;
        let next_revision = self
            .executor
            .block_on(self.metadata.publish(revision, &lease, namespace.clone()))
            .map_err(storage_error)?;
        self.executor
            .block_on(self.metadata.flush())
            .map_err(storage_error)?;
        let writer = state.writer.as_mut().ok_or(VfsError::Busy)?;
        writer.revision = next_revision;
        writer.namespace = Some(namespace);
        Ok(())
    }

    fn delete_file(&self, name: &[u8]) -> Result<(), VfsError> {
        let (handle_id, temporary_owner) = match self.current_owner()? {
            Some(handle_id) => {
                self.ensure_owner(handle_id)?;
                (handle_id, false)
            }
            None => {
                let handle_id = self.next_handle_id()?;
                self.acquire_owner(handle_id)?;
                (handle_id, true)
            }
        };
        let result = self.delete_file_locked(name, handle_id);
        let release_result = if temporary_owner {
            self.release_owner(handle_id)
        } else {
            Ok(())
        };
        result.and(release_result)
    }

    fn delete_file_locked(&self, name: &[u8], handle_id: u64) -> Result<(), VfsError> {
        let mut state = self.lock_state()?;
        let result = self.delete_file_state_locked(&mut state, name, handle_id);
        if let Err(error) = &result {
            state.failed = Some(error.to_string());
        }
        result
    }

    fn delete_file_state_locked(
        &self,
        state: &mut VolumeState,
        name: &[u8],
        handle_id: u64,
    ) -> Result<(), VfsError> {
        Self::ensure_healthy_state(state)?;
        if state.writer.as_ref().map(|writer| writer.handle_id) != Some(handle_id) {
            return Err(VfsError::Busy);
        }
        let Some(mut namespace) = state
            .writer
            .as_ref()
            .and_then(|writer| writer.namespace.clone())
        else {
            return Ok(());
        };
        let entry_name = encoded_name(name);
        let root = namespace
            .nodes
            .get_mut(&namespace.root)
            .ok_or(VfsError::Other(
                "published storage namespace has no root".to_owned(),
            ))?;
        let NodeData::Directory { entries } = &mut root.data else {
            return Err(VfsError::Other(
                "published storage root is not a directory".to_owned(),
            ));
        };
        let Some(position) = entries.iter().position(|entry| entry.name == entry_name) else {
            return Ok(());
        };
        let inode = entries.remove(position).inode;
        namespace.nodes.remove(&inode);
        namespace.validate().map_err(storage_error)?;
        self.ensure_owner_locked(state, handle_id)?;
        let (revision, lease) = {
            let writer = state.writer.as_ref().ok_or(VfsError::Busy)?;
            (writer.revision, writer.lease.clone())
        };
        let next_revision = self
            .executor
            .block_on(self.metadata.publish(revision, &lease, namespace.clone()))
            .map_err(storage_error)?;
        self.executor
            .block_on(self.metadata.flush())
            .map_err(storage_error)?;
        let writer = state.writer.as_mut().ok_or(VfsError::Busy)?;
        writer.revision = next_revision;
        writer.namespace = Some(namespace);
        Ok(())
    }

    fn check_reserved(&self) -> Result<bool, VfsError> {
        if let Some(handle_id) = self.current_owner()? {
            self.ensure_owner(handle_id)?;
            return Ok(true);
        }
        let probe = self.next_handle_id()?;
        match self.acquire_owner(probe) {
            Ok(()) => {
                self.release_owner(probe)?;
                Ok(false)
            }
            Err(VfsError::Busy) => Ok(true),
            Err(error) => Err(error),
        }
    }

    fn temporary_name(&self) -> Result<Vec<u8>, VfsError> {
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(format!("/mount-rs-sqlite-tmp-{}-{id}", std::process::id()).into_bytes())
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, VolumeState>, VfsError> {
        self.state
            .lock()
            .map_err(|_| VfsError::Other("storage bridge state lock poisoned".to_owned()))
    }
}

struct StorageFile<M, B, E>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
    E: BlockingExecutor,
{
    volume: Arc<StorageVolume<M, B, E>>,
    name: Vec<u8>,
    kind: FileKind,
    wal_scope: WalScope,
    is_wal_file: bool,
    read_only: bool,
    delete_on_close: bool,
    bytes: Vec<u8>,
    dirty: bool,
    level: LockLevel,
    handle_id: u64,
    associated_owner: Option<u64>,
    owns_volume: bool,
    can_write: bool,
    wal_connection: Option<ProcessWalConnection>,
}

impl<M, B, E> Drop for StorageFile<M, B, E>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
    E: BlockingExecutor,
{
    fn drop(&mut self) {
        if let Some(connection) = self.wal_connection.as_mut() {
            let _ = connection.unmap();
        }
        if self.delete_on_close {
            let _ = self.volume.delete_file(&self.name);
        }
        if self.owns_volume {
            let _ = self.volume.release_owner(self.handle_id);
            self.owns_volume = false;
        }
    }
}

impl<M, B, E> StorageFile<M, B, E>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
    E: BlockingExecutor,
{
    fn ensure_writable(&self) -> Result<(), VfsError> {
        if self.read_only {
            Err(VfsError::Other(
                "SQLite storage file is read-only".to_owned(),
            ))
        } else {
            Ok(())
        }
    }

    fn needs_volume_owner(&self) -> bool {
        matches!(
            self.kind,
            FileKind::MainDatabase
                | FileKind::MainJournal
                | FileKind::TempJournal
                | FileKind::SubJournal
                | FileKind::MasterJournal
        ) || self.is_wal_file
    }

    fn process_local_wal(&self) -> bool {
        self.wal_scope == WalScope::ProcessLocal && self.wal_connection.is_some()
    }

    fn ensure_process_wal_owner(&mut self) -> Result<u64, VfsError> {
        if let Some(owner) = self.associated_owner {
            self.volume.ensure_owner(owner)?;
            return Ok(owner);
        }
        if let Some(owner) = self.volume.current_owner()? {
            self.volume.ensure_owner(owner)?;
            self.associated_owner = Some(owner);
            return Ok(owner);
        }
        self.volume.acquire_owner(self.handle_id)?;
        self.owns_volume = true;
        self.associated_owner = Some(self.handle_id);
        Ok(self.handle_id)
    }

    fn ensure_associated_owner(&mut self) -> Result<u64, VfsError> {
        if self.wal_scope == WalScope::ProcessLocal
            && self.kind != FileKind::MainDatabase
            && self.needs_volume_owner()
        {
            return self.ensure_process_wal_owner();
        }
        let owner = match self.associated_owner {
            Some(owner) => owner,
            None => {
                let owner = match self.volume.current_owner()? {
                    Some(owner) => owner,
                    None => return Err(VfsError::Busy),
                };
                self.associated_owner = Some(owner);
                owner
            }
        };
        if let Err(error) = self.volume.ensure_owner(owner) {
            self.associated_owner = None;
            return Err(error);
        }
        Ok(owner)
    }

    fn ensure_main_owner(&self) -> Result<(), VfsError> {
        if self.kind == FileKind::MainDatabase && self.owns_volume {
            self.volume.ensure_owner(self.handle_id)
        } else {
            Err(VfsError::Busy)
        }
    }

    fn ensure_main_lock_owner(&mut self) -> Result<(), VfsError> {
        if self.kind != FileKind::MainDatabase {
            return Err(VfsError::Busy);
        }
        if self.process_local_wal() {
            if self.owns_volume {
                self.volume.ensure_owner(self.handle_id)?;
            }
            Ok(())
        } else {
            self.ensure_main_owner()
        }
    }

    fn ensure_main_write_authority(&mut self) -> Result<u64, VfsError> {
        if self.kind != FileKind::MainDatabase || !self.can_write {
            return Err(VfsError::Busy);
        }
        if self.process_local_wal() {
            if self.owns_volume {
                self.volume.ensure_owner(self.handle_id)?;
                return Ok(self.handle_id);
            }
            if let Some(owner) = self.volume.current_owner()? {
                self.volume.ensure_owner(owner)?;
                self.associated_owner = Some(owner);
                return Ok(owner);
            }
            self.volume.acquire_owner(self.handle_id)?;
            self.owns_volume = true;
            return Ok(self.handle_id);
        }
        if self.owns_volume {
            self.volume.ensure_owner(self.handle_id)?;
            Ok(self.handle_id)
        } else {
            Err(VfsError::Busy)
        }
    }

    fn ensure_main_shared(&mut self) -> Result<(), VfsError> {
        if self.kind != FileKind::MainDatabase {
            return Err(VfsError::Busy);
        }
        if self.process_local_wal() {
            self.bytes = self.volume.load_file(&self.name)?;
            self.level = LockLevel::Shared;
            return Ok(());
        }
        if self.owns_volume {
            self.volume.ensure_owner(self.handle_id)?;
            return Ok(());
        }
        self.volume.acquire_owner(self.handle_id)?;
        self.owns_volume = true;
        match self.volume.load_file(&self.name) {
            Ok(bytes) => {
                self.bytes = bytes;
                self.level = LockLevel::Shared;
                Ok(())
            }
            Err(error) => {
                let _ = self.volume.release_owner(self.handle_id);
                self.owns_volume = false;
                Err(error)
            }
        }
    }

    fn publish_wal_write(&mut self) -> Result<(), VfsError> {
        if !self.is_wal_file || !self.dirty {
            return Ok(());
        }
        let owner = self.ensure_associated_owner()?;
        self.volume.commit_file(&self.name, &self.bytes, owner)?;
        self.dirty = false;
        Ok(())
    }
}

impl<M, B, E> VfsFile for StorageFile<M, B, E>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
    E: BlockingExecutor,
{
    fn shm_map(
        &mut self,
        page: usize,
        page_size: usize,
        extend: bool,
    ) -> Result<*mut c_void, VfsError> {
        if self.wal_scope != WalScope::ProcessLocal {
            return Err(VfsError::Unsupported("process-local WAL shared memory"));
        }
        self.wal_connection
            .as_mut()
            .ok_or(VfsError::Unsupported(
                "WAL shared memory is main-database only",
            ))?
            .map(page, page_size, extend)
    }

    fn shm_lock(&mut self, offset: usize, number: usize, flags: c_int) -> Result<(), VfsError> {
        if self.wal_scope != WalScope::ProcessLocal {
            return Err(VfsError::Unsupported("process-local WAL locks"));
        }
        self.wal_connection
            .as_mut()
            .ok_or(VfsError::Unsupported("WAL locks are main-database only"))?
            .lock(offset, number, flags)
    }

    fn shm_barrier(&mut self) {
        if let Some(connection) = self.wal_connection.as_mut() {
            connection.barrier();
        }
    }

    fn shm_unmap(&mut self, _delete: bool) -> Result<(), VfsError> {
        if let Some(connection) = self.wal_connection.as_mut() {
            connection.unmap()
        } else {
            Ok(())
        }
    }

    fn read_at(&mut self, output: &mut [u8], offset: u64) -> Result<usize, VfsError> {
        if self.is_wal_file && !self.dirty {
            self.bytes = self.volume.load_file(&self.name)?;
        }
        if self.kind == FileKind::MainDatabase {
            // SQLite may probe the header before its first xLock callback.  The
            // bytes cached by xOpen are used only for that bootstrap probe;
            // lock acquisition below reloads under the provider lease before
            // any transaction-visible read is allowed.
            if self.process_local_wal() {
                if self.level >= LockLevel::Shared {
                    self.volume.ensure_healthy()?;
                }
            } else if self.owns_volume || self.level >= LockLevel::Shared {
                self.ensure_main_owner()?;
            }
        } else if self.needs_volume_owner() {
            self.ensure_associated_owner()?;
        }
        let offset = usize::try_from(offset)
            .map_err(|_| VfsError::InvalidInput("SQLite read offset overflows usize"))?;
        if offset >= self.bytes.len() {
            return Ok(0);
        }
        let count = output.len().min(self.bytes.len() - offset);
        output[..count].copy_from_slice(&self.bytes[offset..offset + count]);
        Ok(count)
    }

    fn write_at(&mut self, input: &[u8], offset: u64) -> Result<(), VfsError> {
        self.ensure_writable()?;
        if self.kind == FileKind::MainDatabase {
            let _ = self.ensure_main_write_authority()?;
        } else if self.needs_volume_owner() {
            self.ensure_associated_owner()?;
        }
        let offset = usize::try_from(offset)
            .map_err(|_| VfsError::InvalidInput("SQLite write offset overflows usize"))?;
        let end = offset
            .checked_add(input.len())
            .ok_or(VfsError::InvalidInput("SQLite write range overflows usize"))?;
        if end > self.bytes.len() {
            self.bytes.resize(end, 0);
        }
        self.bytes[offset..end].copy_from_slice(input);
        self.dirty = true;
        self.publish_wal_write()?;
        Ok(())
    }

    fn truncate(&mut self, size: u64) -> Result<(), VfsError> {
        self.ensure_writable()?;
        if self.kind == FileKind::MainDatabase {
            let _ = self.ensure_main_write_authority()?;
        } else if self.needs_volume_owner() {
            self.ensure_associated_owner()?;
        }
        let size = usize::try_from(size)
            .map_err(|_| VfsError::InvalidInput("SQLite truncate size overflows usize"))?;
        self.bytes.resize(size, 0);
        self.dirty = true;
        self.publish_wal_write()?;
        Ok(())
    }

    fn sync(&mut self, _data_only: bool) -> Result<(), VfsError> {
        self.ensure_writable()?;
        let owner = if self.kind == FileKind::MainDatabase {
            Some(self.ensure_main_write_authority()?)
        } else if self.needs_volume_owner() {
            Some(self.ensure_associated_owner()?)
        } else {
            None
        };
        if let Some(owner) = owner
            && self.dirty
        {
            self.volume.commit_file(&self.name, &self.bytes, owner)?;
            self.dirty = false;
        }
        Ok(())
    }

    fn size(&mut self) -> Result<u64, VfsError> {
        if self.kind == FileKind::MainDatabase {
            if self.level >= LockLevel::Shared && !self.process_local_wal() {
                self.ensure_main_owner()?;
            }
        } else if self.needs_volume_owner() && self.level >= LockLevel::Shared {
            self.ensure_associated_owner()?;
        }
        u64::try_from(self.bytes.len())
            .map_err(|_| VfsError::InvalidInput("SQLite file size overflows u64"))
    }

    fn lock(&mut self, level: LockLevel) -> Result<(), VfsError> {
        if level >= LockLevel::Shared && self.level < LockLevel::Shared {
            if self.kind == FileKind::MainDatabase {
                self.ensure_main_shared()?;
            } else if self.needs_volume_owner() {
                self.ensure_associated_owner()?;
            }
            self.level = LockLevel::Shared;
        } else if level >= LockLevel::Shared && self.needs_volume_owner() {
            if self.kind == FileKind::MainDatabase {
                if !self.process_local_wal() {
                    self.ensure_main_owner()?;
                }
            } else {
                self.ensure_associated_owner()?;
            }
        }
        if level >= LockLevel::Reserved && self.level < LockLevel::Reserved {
            if self.kind == FileKind::MainDatabase {
                self.ensure_main_lock_owner()?;
                self.can_write = true;
            } else if self.needs_volume_owner() {
                self.ensure_associated_owner()?;
            }
            self.level = LockLevel::Reserved;
        }
        if level >= LockLevel::Pending {
            if self.kind == FileKind::MainDatabase {
                self.ensure_main_lock_owner()?;
            } else if self.needs_volume_owner() {
                self.ensure_associated_owner()?;
            }
            self.level = LockLevel::Pending;
        }
        if level >= LockLevel::Exclusive {
            if self.kind == FileKind::MainDatabase {
                self.ensure_main_lock_owner()?;
            } else if self.needs_volume_owner() {
                self.ensure_associated_owner()?;
            }
            self.level = LockLevel::Exclusive;
        }
        Ok(())
    }

    fn unlock(&mut self, level: LockLevel) -> Result<(), VfsError> {
        if self.kind == FileKind::MainDatabase {
            if level < LockLevel::Shared && self.owns_volume {
                self.volume.release_owner(self.handle_id)?;
                self.owns_volume = false;
            }
            self.can_write = level >= LockLevel::Reserved;
        }
        self.level = level;
        Ok(())
    }

    fn check_reserved_lock(&mut self) -> Result<bool, VfsError> {
        self.volume.check_reserved()
    }
}

fn empty_namespace(chunk_size: usize) -> Namespace {
    let root = NodeMetadata {
        stats: stats(1, S_IFDIR | 0o755, 2, 0),
        data: NodeData::Directory {
            entries: Vec::new(),
        },
    };
    Namespace {
        format_version: NAMESPACE_FORMAT_VERSION,
        root: 1,
        next_inode: 2,
        default_uid: 0,
        default_gid: 0,
        umask: 0,
        default_chunker: chunker_config(chunk_size),
        nodes: BTreeMap::from([(1, root)]),
    }
}

fn upsert_file(
    namespace: &mut Namespace,
    name: &[u8],
    size: u64,
    extents: Vec<BlockExtent>,
    chunk_size: usize,
) -> Result<(), VfsError> {
    let entry_name = encoded_name(name);
    let root_id = namespace.root;
    let existing = {
        let root = namespace.nodes.get(&root_id).ok_or(VfsError::Other(
            "published storage namespace has no root".to_owned(),
        ))?;
        let NodeData::Directory { entries } = &root.data else {
            return Err(VfsError::Other(
                "published storage root is not a directory".to_owned(),
            ));
        };
        entries
            .iter()
            .find(|entry| entry.name == entry_name)
            .map(|entry| entry.inode)
    };
    let inode = if let Some(inode) = existing {
        inode
    } else {
        let inode = namespace.next_inode;
        namespace.next_inode = namespace
            .next_inode
            .checked_add(1)
            .ok_or(VfsError::InvalidInput("storage inode counter overflow"))?;
        let root = namespace.nodes.get_mut(&root_id).ok_or(VfsError::Other(
            "published storage namespace has no root".to_owned(),
        ))?;
        let NodeData::Directory { entries } = &mut root.data else {
            return Err(VfsError::Other(
                "published storage root is not a directory".to_owned(),
            ));
        };
        entries.push(DirectoryEntry {
            name: entry_name,
            inode,
        });
        inode
    };
    namespace.nodes.insert(
        inode,
        NodeMetadata {
            stats: stats(inode, S_IFREG | 0o600, 1, size),
            data: NodeData::File(FileLayout {
                chunker: chunker_config(chunk_size),
                extents,
            }),
        },
    );
    Ok(())
}

fn chunker_config(chunk_size: usize) -> ChunkerConfig {
    ChunkerConfig {
        algorithm: "fixed-size".to_owned(),
        version: 1,
        parameters: BTreeMap::from([("chunk_size".to_owned(), chunk_size as u64)]),
    }
}

fn stats(inode: InodeId, mode: u32, nlink: u64, size: u64) -> Stats {
    let now = now_ms();
    Stats {
        dev: 1,
        ino: inode,
        mode,
        nlink,
        uid: 0,
        gid: 0,
        rdev: 0,
        size,
        blksize: 4096,
        blocks: size.div_ceil(512),
        atime_ms: now,
        mtime_ms: now,
        ctime_ms: now,
        birthtime_ms: now,
    }
}

fn encoded_name(name: &[u8]) -> String {
    let mut output = String::with_capacity(1 + name.len() * 2);
    output.push('f');
    for byte in name {
        output.push(char::from_digit((byte >> 4) as u32, 16).expect("hex digit"));
        output.push(char::from_digit((byte & 0x0f) as u32, 16).expect("hex digit"));
    }
    output
}

fn storage_error(error: FsError) -> VfsError {
    match error.code {
        ErrorCode::Eagain | ErrorCode::Ebusy => VfsError::Busy,
        ErrorCode::Enoent => VfsError::NotFound,
        ErrorCode::Enotsup => VfsError::Unsupported("provider capability"),
        _ => VfsError::Other(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct WakeOnce(bool);

    impl Future for WakeOnce {
        type Output = u8;

        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
            if self.0 {
                Poll::Ready(7)
            } else {
                self.0 = true;
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    #[test]
    fn inline_executor_parks_until_a_real_wake() {
        assert_eq!(InlineExecutor.block_on(WakeOnce(false)), 7);
    }

    #[cfg(feature = "tokio-executor")]
    #[test]
    fn tokio_executor_reenters_a_multi_thread_runtime_without_deadlock() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .expect("multi-thread Tokio runtime");

        runtime.block_on(async {
            let executor = TokioExecutor::current().expect("current Tokio runtime");
            assert_eq!(
                executor.block_on(async {
                    tokio::task::yield_now().await;
                    11
                }),
                11
            );
        });
    }
}
