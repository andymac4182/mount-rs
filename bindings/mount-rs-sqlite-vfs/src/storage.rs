//! Bridge from mount-rs's fenced metadata/immutable-block contracts to the
//! synchronous SQLite VFS boundary.
//!
//! The bridge is intentionally generic over the provider pair.  That keeps
//! the crate's dependency surface small while allowing the same code to be
//! instantiated with memory stores for volatile tests, durable SQLite stores,
//! R2 blocks plus a fenced metadata store, or PGlite metadata/blocks.  The
//! provider pair is still responsible for its own durability declaration.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::os::raw::{c_int, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
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
                state: mount_rs_core::turn_gate::TurnLock::new(VolumeState::default()),
                next_handle: AtomicU64::new(1),
                wal_regions: ArcSwap::default(),
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
            .then(|| self.volume.wal_connection(name))
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
    state: mount_rs_core::turn_gate::TurnLock<VolumeState>,
    next_handle: AtomicU64,
    /// One region per database name. Entries are added on a database's
    /// first WAL connection and never removed, so a connection normally
    /// finds its region with one load.
    wal_regions: ArcSwap<HashMap<Vec<u8>, Arc<ProcessWalRegion>>>,
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

/// Number of SQLite WAL-index lock slots; each has one byte in
/// `ProcessWalRegion::slots`.
const SHM_SLOTS: usize = ffi::SQLITE_SHM_NLOCK as usize;
const _: () = assert!(SHM_SLOTS * 8 <= u64::BITS as usize);

/// Slot byte meaning "one connection holds this slot exclusively". Any other
/// value is the number of connections holding the slot shared.
const SHM_SLOT_EXCLUSIVE: u64 = 0xFF;

/// Most shared holders one slot can count. The byte after it is the
/// exclusive marker, so a shared request beyond this many holders is
/// answered busy, which SQLite retries, rather than corrupting the count.
const SHM_SLOT_MAX_SHARED: u64 = 0xFE;

fn shm_slot(word: u64, index: usize) -> u64 {
    (word >> (index * 8)) & 0xFF
}

/// One zeroed page of WAL-index memory. SQLite reads and writes it through
/// the raw pointer `shm_map` hands out; the bytes are atomics only so that
/// the pointer may be written through while the page is shared.
struct WalPage {
    bytes: Box<[AtomicU8]>,
    #[cfg(test)]
    live: Arc<AtomicUsize>,
}

impl WalPage {
    fn pointer(&self) -> *mut c_void {
        self.bytes.as_ptr().cast_mut().cast::<c_void>()
    }
}

#[cfg(test)]
impl Drop for WalPage {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

struct ProcessWalRegion {
    /// Page size of the WAL index, zero until the first `map` sets it.
    region_size: AtomicUsize,
    /// The mapped pages. The list only grows, and a page stays at its
    /// address until the region is dropped, so a pointer handed to SQLite
    /// stays valid for as long as any connection holds the region.
    pages: ArcSwap<Vec<Arc<WalPage>>>,
    /// One byte per lock slot, slot `i` in bits `8 * i ..`. A lock or unlock
    /// call changes every slot of its range in one step, so no other
    /// connection sees a range half taken.
    slots: AtomicU64,
    #[cfg(test)]
    live_pages: Arc<AtomicUsize>,
}

/// The region of `database_name`, created on first use. Callers racing on a
/// new name each offer a region; one is published and all of them receive
/// that one, so every connection to a database shares its WAL-index memory.
fn wal_region(
    regions: &ArcSwap<HashMap<Vec<u8>, Arc<ProcessWalRegion>>>,
    database_name: Vec<u8>,
) -> Arc<ProcessWalRegion> {
    if let Some(region) = regions.load().get(&database_name) {
        return Arc::clone(region);
    }
    let fresh = Arc::new(ProcessWalRegion::new());
    loop {
        let current = regions.load_full();
        if let Some(region) = current.get(&database_name) {
            return Arc::clone(region);
        }
        let mut next = HashMap::clone(&current);
        next.insert(database_name.clone(), Arc::clone(&fresh));
        let previous = regions.compare_and_swap(&current, Arc::new(next));
        if Arc::ptr_eq(&previous, &current) {
            return fresh;
        }
    }
}

struct ProcessWalConnection {
    region: Arc<ProcessWalRegion>,
    /// What this connection holds in each slot. Only this connection adds
    /// to or removes its own holds, so this array alone answers "do I
    /// already hold this slot".
    held: [ProcessShmLockMode; SHM_SLOTS],
}

impl ProcessWalRegion {
    fn new() -> Self {
        Self {
            region_size: AtomicUsize::new(0),
            pages: ArcSwap::default(),
            slots: AtomicU64::new(0),
            #[cfg(test)]
            live_pages: Arc::default(),
        }
    }

    fn new_page(&self, page_size: usize) -> Arc<WalPage> {
        #[cfg(test)]
        self.live_pages.fetch_add(1, Ordering::SeqCst);
        Arc::new(WalPage {
            bytes: (0..page_size).map(|_| AtomicU8::new(0)).collect(),
            #[cfg(test)]
            live: Arc::clone(&self.live_pages),
        })
    }
}

impl ProcessWalConnection {
    fn new(region: Arc<ProcessWalRegion>) -> Self {
        Self {
            region,
            held: [ProcessShmLockMode::None; SHM_SLOTS],
        }
    }

    fn map(
        &mut self,
        page: usize,
        page_size: usize,
        extend: bool,
    ) -> Result<*mut c_void, VfsError> {
        if page_size == 0 {
            return Err(VfsError::InvalidInput("WAL shared-memory region is empty"));
        }
        // The first caller sets the size; everyone else must agree with it.
        if let Err(existing) = self.region.region_size.compare_exchange(
            0,
            page_size,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) && existing != page_size
        {
            return Err(VfsError::InvalidInput(
                "WAL shared-memory region size changed",
            ));
        }
        if let Some(existing) = self.region.pages.load().get(page) {
            return Ok(existing.pointer());
        }
        if !extend {
            return Ok(ptr::null_mut());
        }
        // Offer a longer list. If another connection publishes first, the
        // pages it added are the ones everybody uses and the unused pages
        // allocated here are freed when `fresh` is dropped.
        let mut fresh: Vec<Arc<WalPage>> = Vec::new();
        loop {
            let current = self.region.pages.load_full();
            if let Some(existing) = current.get(page) {
                return Ok(existing.pointer());
            }
            let missing = page + 1 - current.len();
            while fresh.len() < missing {
                fresh.push(self.region.new_page(page_size));
            }
            let mut next = Vec::with_capacity(page + 1);
            next.extend(current.iter().cloned());
            next.extend(fresh[..missing].iter().cloned());
            let pointer = next[page].pointer();
            let previous = self.region.pages.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return Ok(pointer);
            }
        }
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
            self.release(offset..end);
            return Ok(());
        }

        let mut current = self.region.slots.load(Ordering::SeqCst);
        loop {
            // Judge the whole range against one value of the word, then
            // install the whole range with one compare-and-swap. A failed
            // swap means some slot changed, so the range is judged again
            // against the new value; nothing was taken that needs undoing.
            let mut next = current;
            for index in offset..end {
                let slot = shm_slot(current, index);
                let held = self.held[index];
                match mode {
                    ProcessShmLockMode::Shared => {
                        if held == ProcessShmLockMode::Exclusive {
                            return Err(VfsError::InvalidInput(
                                "WAL shared lock cannot replace an exclusive lock",
                            ));
                        }
                        if held == ProcessShmLockMode::Shared {
                            continue;
                        }
                        if slot == SHM_SLOT_EXCLUSIVE || slot == SHM_SLOT_MAX_SHARED {
                            return Err(VfsError::Busy);
                        }
                        next += 1 << (index * 8);
                    }
                    ProcessShmLockMode::Exclusive => {
                        if held == ProcessShmLockMode::Exclusive {
                            continue;
                        }
                        if held == ProcessShmLockMode::Shared {
                            // This connection is one of the holders counted.
                            if slot > 1 {
                                return Err(VfsError::Busy);
                            }
                            return Err(VfsError::InvalidInput(
                                "WAL exclusive lock cannot replace a shared lock",
                            ));
                        }
                        if slot != 0 {
                            return Err(VfsError::Busy);
                        }
                        next |= SHM_SLOT_EXCLUSIVE << (index * 8);
                    }
                    ProcessShmLockMode::None => unreachable!(),
                }
            }
            match self.region.slots.compare_exchange_weak(
                current,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(changed) => current = changed,
            }
        }
        for index in offset..end {
            self.held[index] = mode;
        }
        Ok(())
    }

    /// Give up whatever this connection holds in `range`. Each held slot's
    /// byte is at least what is subtracted for it, a shared holder's count
    /// of one or the whole exclusive marker, so one subtraction releases
    /// the range in one step and never borrows from a neighbouring slot.
    fn release(&mut self, range: std::ops::Range<usize>) {
        let mut holds = 0_u64;
        for index in range {
            holds |= match self.held[index] {
                ProcessShmLockMode::Shared => 1,
                ProcessShmLockMode::Exclusive => SHM_SLOT_EXCLUSIVE,
                ProcessShmLockMode::None => 0,
            } << (index * 8);
            self.held[index] = ProcessShmLockMode::None;
        }
        if holds != 0 {
            self.region.slots.fetch_sub(holds, Ordering::SeqCst);
        }
    }

    fn barrier(&mut self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }

    fn unmap(&mut self) -> Result<(), VfsError> {
        self.release(0..SHM_SLOTS);
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

    fn wal_connection(&self, database_name: &[u8]) -> Result<ProcessWalConnection, VfsError> {
        let database_name = database_name
            .strip_suffix(b"-wal")
            .unwrap_or(database_name)
            .to_vec();
        Ok(ProcessWalConnection::new(wal_region(
            &self.wal_regions,
            database_name,
        )))
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

    /// The volume state, one caller at a time. The state is held across the
    /// provider round trips it orders (lease, load, commit), so a caller that
    /// finds it taken waits on the executor, parked, like any other provider
    /// future.
    fn lock_state(
        &self,
    ) -> Result<mount_rs_core::turn_gate::TurnLockGuard<'_, VolumeState>, VfsError> {
        Ok(self.executor.block_on(self.state.lock()))
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
        nodes: BTreeMap::from([(1, root)]).into(),
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

    /// Connections racing to open the WAL index of one new database all
    /// share one region, and all map the same memory for a page.
    #[test]
    fn racing_wal_connections_share_one_region_and_one_page() {
        const THREADS: usize = 8;
        for round in 0..300_u32 {
            let regions: ArcSwap<HashMap<Vec<u8>, Arc<ProcessWalRegion>>> = ArcSwap::default();
            let barrier = std::sync::Barrier::new(THREADS);
            let mapped: Vec<(usize, usize)> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..THREADS)
                    .map(|thread| {
                        let (regions, barrier) = (&regions, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            let name = format!("db-{}", thread % 2).into_bytes();
                            let region = wal_region(regions, name);
                            let mut connection = ProcessWalConnection::new(Arc::clone(&region));
                            let page = connection.map(0, 32 * 1024, true).unwrap();
                            (thread % 2, page as usize)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .collect()
            });
            assert_eq!(regions.load().len(), 2, "round {round}");
            for database in 0..2 {
                let pages: std::collections::BTreeSet<usize> = mapped
                    .iter()
                    .filter(|(name, _)| *name == database)
                    .map(|(_, page)| *page)
                    .collect();
                assert_eq!(pages.len(), 1, "one WAL-index page per database");
            }
        }
    }

    const SHARED: c_int = ffi::SQLITE_SHM_SHARED;
    const EXCLUSIVE: c_int = ffi::SQLITE_SHM_EXCLUSIVE;
    const LOCK: c_int = ffi::SQLITE_SHM_LOCK;
    const UNLOCK: c_int = ffi::SQLITE_SHM_UNLOCK;

    /// The lock table written the obvious way: who holds each slot.
    #[derive(Default)]
    struct ModelSlots {
        shared: [std::collections::BTreeSet<usize>; SHM_SLOTS],
        exclusive: [Option<usize>; SHM_SLOTS],
    }

    #[derive(Debug, Eq, PartialEq)]
    enum Outcome {
        Granted,
        Busy,
        Invalid,
    }

    fn outcome(result: Result<(), VfsError>) -> Outcome {
        match result {
            Ok(()) => Outcome::Granted,
            Err(VfsError::Busy) => Outcome::Busy,
            Err(VfsError::InvalidInput(_)) => Outcome::Invalid,
            Err(other) => panic!("unexpected WAL lock error: {other:?}"),
        }
    }

    impl ModelSlots {
        /// What a lock table guarded by one lock answers, slot by slot in
        /// index order, changing nothing unless the whole range is granted.
        fn apply(&mut self, owner: usize, offset: usize, number: usize, flags: c_int) -> Outcome {
            let range = offset..offset + number;
            let shared = flags & SHARED != 0;
            if flags & LOCK == 0 {
                for index in range.clone() {
                    let holds_shared = self.shared[index].contains(&owner);
                    let holds_exclusive = self.exclusive[index] == Some(owner);
                    if (shared && holds_exclusive) || (!shared && holds_shared) {
                        return Outcome::Invalid;
                    }
                }
                for index in range {
                    if shared {
                        self.shared[index].remove(&owner);
                    } else if self.exclusive[index] == Some(owner) {
                        self.exclusive[index] = None;
                    }
                }
                return Outcome::Granted;
            }
            for index in range.clone() {
                match self.exclusive[index] {
                    Some(holder) if holder != owner => return Outcome::Busy,
                    Some(_) if shared => return Outcome::Invalid,
                    Some(_) => continue,
                    None => {}
                }
                if !shared {
                    if self.shared[index].iter().any(|holder| *holder != owner) {
                        return Outcome::Busy;
                    }
                    if self.shared[index].contains(&owner) {
                        return Outcome::Invalid;
                    }
                }
            }
            for index in range {
                if shared {
                    self.shared[index].insert(owner);
                } else {
                    self.exclusive[index] = Some(owner);
                }
            }
            Outcome::Granted
        }

        fn drop_owner(&mut self, owner: usize) {
            for index in 0..SHM_SLOTS {
                self.shared[index].remove(&owner);
                if self.exclusive[index] == Some(owner) {
                    self.exclusive[index] = None;
                }
            }
        }

        fn word(&self) -> u64 {
            (0..SHM_SLOTS).fold(0, |word, index| {
                let slot = match self.exclusive[index] {
                    Some(_) => SHM_SLOT_EXCLUSIVE,
                    None => self.shared[index].len() as u64,
                };
                word | slot << (index * 8)
            })
        }
    }

    struct Random(u64);

    impl Random {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound as u64) as usize
        }

        fn lock_call(&mut self) -> (usize, usize, c_int) {
            let offset = self.below(SHM_SLOTS);
            let number = 1 + self.below((SHM_SLOTS - offset).min(3));
            let action = if self.below(2) == 0 { LOCK } else { UNLOCK };
            let mode = if self.below(2) == 0 {
                SHARED
            } else {
                EXCLUSIVE
            };
            (offset, number, action | mode)
        }
    }

    /// With no races, every answer and the whole slot word match the lock
    /// table written with sets of holders, including the refusals.
    #[test]
    fn wal_slot_word_answers_like_a_table_of_holders() {
        const CONNECTIONS: usize = 5;
        let region = Arc::new(ProcessWalRegion::new());
        let mut connections: Vec<_> = (0..CONNECTIONS)
            .map(|_| ProcessWalConnection::new(Arc::clone(&region)))
            .collect();
        let mut model = ModelSlots::default();
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        let mut seen = [0_usize; 3];
        for step in 0..200_000 {
            let owner = random.below(CONNECTIONS);
            if random.below(40) == 0 {
                connections[owner].unmap().unwrap();
                model.drop_owner(owner);
            } else {
                let (offset, number, flags) = random.lock_call();
                let expected = model.apply(owner, offset, number, flags);
                let actual = outcome(connections[owner].lock(offset, number, flags));
                assert_eq!(
                    actual, expected,
                    "step {step}: {offset}+{number} {flags:#x}"
                );
                seen[expected as usize] += 1;
            }
            assert_eq!(
                region.slots.load(Ordering::SeqCst),
                model.word(),
                "step {step}"
            );
        }
        assert!(seen.iter().all(|count| *count > 1_000), "{seen:?}");
        drop(connections);
        assert_eq!(region.slots.load(Ordering::SeqCst), 0);
    }

    /// Connections on different threads lock and unlock random ranges. A
    /// table of who holds what, kept under a test mutex, is told of a hold
    /// only after it is granted and of a release before it happens, so two
    /// holds that conflict in the table were both held for real.
    #[test]
    fn racing_wal_locks_keep_exclusive_holders_alone() {
        use std::sync::Mutex;

        const THREADS: usize = 8;
        const STEPS: usize = 40_000;

        struct Table {
            held: ModelSlots,
            /// Lock attempts ever announced per slot, by any connection.
            attempts: [u64; SHM_SLOTS],
            /// Attempts announced and not yet finished or released.
            pending: [std::collections::BTreeSet<usize>; SHM_SLOTS],
        }

        let region = Arc::new(ProcessWalRegion::new());
        let table = Mutex::new(Table {
            held: ModelSlots::default(),
            attempts: [0; SHM_SLOTS],
            pending: Default::default(),
        });
        let barrier = std::sync::Barrier::new(THREADS);
        let totals: Vec<[usize; 3]> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|thread| {
                    let (region, table, barrier) = (&region, &table, &barrier);
                    scope.spawn(move || {
                        let mut connection = ProcessWalConnection::new(Arc::clone(region));
                        let mut random =
                            Random(0x2545_f491_4f6c_dd1d_u64.wrapping_mul(thread as u64 + 1));
                        let mut seen = [0_usize; 3];
                        barrier.wait();
                        for _ in 0..STEPS {
                            if random.below(60) == 0 {
                                table.lock().unwrap().held.drop_owner(thread);
                                connection.unmap().unwrap();
                                for pending in &mut table.lock().unwrap().pending {
                                    pending.remove(&thread);
                                }
                                continue;
                            }
                            let (offset, number, flags) = random.lock_call();
                            let range = offset..offset + number;
                            let shared = flags & SHARED != 0;
                            let before = connection.held;
                            let own_other_mode = range.clone().any(|index| {
                                before[index]
                                    == if shared {
                                        ProcessShmLockMode::Exclusive
                                    } else {
                                        ProcessShmLockMode::Shared
                                    }
                            });
                            if flags & LOCK == 0 {
                                // An unlock depends only on this connection.
                                if !own_other_mode {
                                    let mut table = table.lock().unwrap();
                                    assert_eq!(
                                        table.held.apply(thread, offset, number, flags),
                                        Outcome::Granted
                                    );
                                }
                                let actual = outcome(connection.lock(offset, number, flags));
                                if own_other_mode {
                                    assert_eq!(actual, Outcome::Invalid);
                                    assert_eq!(connection.held, before);
                                } else {
                                    assert_eq!(actual, Outcome::Granted);
                                    let mut table = table.lock().unwrap();
                                    for index in range {
                                        table.pending[index].remove(&thread);
                                    }
                                }
                                seen[actual as usize] += 1;
                                continue;
                            }
                            // Announce the attempt, noting who else might
                            // be in the way now and how many have ever tried.
                            let (rivals_before, attempts_before) = {
                                let mut table = table.lock().unwrap();
                                let rivals = range
                                    .clone()
                                    .any(|index| table.pending[index].iter().any(|o| *o != thread));
                                let attempts = table.attempts;
                                for index in range.clone() {
                                    table.attempts[index] += 1;
                                    table.pending[index].insert(thread);
                                }
                                (rivals, attempts)
                            };
                            let actual = outcome(connection.lock(offset, number, flags));
                            let mut table = table.lock().unwrap();
                            match actual {
                                Outcome::Granted => {
                                    assert!(!own_other_mode);
                                    // The table grants it too: nobody it
                                    // knows of holds a conflicting slot.
                                    assert_eq!(
                                        table.held.apply(thread, offset, number, flags),
                                        Outcome::Granted,
                                        "a granted lock overlaps a conflicting holder"
                                    );
                                    for index in range.clone() {
                                        if !shared {
                                            assert_eq!(table.held.exclusive[index], Some(thread));
                                            assert!(table.held.shared[index].is_empty());
                                        }
                                    }
                                }
                                Outcome::Busy => {
                                    // Busy needs another connection that
                                    // was on these slots during the call.
                                    let rivals_since = range.clone().any(|index| {
                                        table.attempts[index] != attempts_before[index] + 1
                                    });
                                    assert!(
                                        rivals_before || rivals_since,
                                        "busy with no other connection on the range"
                                    );
                                }
                                Outcome::Invalid => assert!(own_other_mode),
                            }
                            if actual != Outcome::Granted {
                                // All or nothing: a refusal took no slot.
                                assert_eq!(connection.held, before);
                                for index in range {
                                    if before[index] == ProcessShmLockMode::None {
                                        table.pending[index].remove(&thread);
                                    }
                                }
                            }
                            seen[actual as usize] += 1;
                        }
                        table.lock().unwrap().held.drop_owner(thread);
                        drop(connection);
                        seen
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        // Every hold was given back exactly once: no count is left over.
        assert_eq!(region.slots.load(Ordering::SeqCst), 0);
        assert_eq!(table.lock().unwrap().held.word(), 0);
        for kind in 0..3 {
            let total: usize = totals.iter().map(|seen| seen[kind]).sum();
            assert!(total > 1_000, "outcome {kind} seen {total} times");
        }
    }

    /// A slot counts at most 254 shared holders; the next is told busy and
    /// the count stays exact.
    #[test]
    fn wal_slot_refuses_shared_holders_beyond_its_count() {
        let region = Arc::new(ProcessWalRegion::new());
        let mut holders: Vec<_> = (0..SHM_SLOT_MAX_SHARED)
            .map(|_| {
                let mut connection = ProcessWalConnection::new(Arc::clone(&region));
                connection.lock(3, 2, LOCK | SHARED).unwrap();
                connection
            })
            .collect();
        let mut extra = ProcessWalConnection::new(Arc::clone(&region));
        assert!(matches!(
            extra.lock(4, 1, LOCK | SHARED),
            Err(VfsError::Busy)
        ));
        assert!(matches!(
            extra.lock(4, 1, LOCK | EXCLUSIVE),
            Err(VfsError::Busy)
        ));
        assert_eq!(shm_slot(region.slots.load(Ordering::SeqCst), 4), 254);
        assert_eq!(shm_slot(region.slots.load(Ordering::SeqCst), 5), 0);
        holders.pop();
        extra.lock(4, 1, LOCK | SHARED).unwrap();
        drop(holders);
        assert_eq!(region.slots.load(Ordering::SeqCst), 1 << (4 * 8));
        drop(extra);
        assert_eq!(region.slots.load(Ordering::SeqCst), 0);
    }

    /// Threads racing to map the same page index of one region all get the
    /// same address, earlier pages keep theirs, and the pages the losers
    /// allocated are freed, as are all pages once the region is dropped.
    #[test]
    fn racing_wal_maps_share_each_page_and_free_the_spares() {
        const THREADS: usize = 8;
        const PAGE: usize = 4096;
        for round in 0..300_usize {
            let region = Arc::new(ProcessWalRegion::new());
            let live = Arc::clone(&region.live_pages);
            let target = round % 4;
            let barrier = std::sync::Barrier::new(THREADS);
            let mapped: Vec<[usize; 2]> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..THREADS)
                    .map(|thread| {
                        let (region, barrier) = (&region, &barrier);
                        scope.spawn(move || {
                            let mut connection = ProcessWalConnection::new(Arc::clone(region));
                            barrier.wait();
                            // Half the threads ask for the page after.
                            let page = target + thread % 2;
                            let first = connection.map(page, PAGE, true).unwrap();
                            assert!(!first.is_null());
                            let again = connection.map(page, PAGE, false).unwrap();
                            assert_eq!(first, again);
                            let lower = connection.map(target, PAGE, false).unwrap();
                            assert!(!lower.is_null());
                            assert!(connection.map(page, PAGE * 2, true).is_err());
                            [
                                lower as usize,
                                if page == target { 0 } else { first as usize },
                            ]
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .collect()
            });
            for column in 0..2 {
                let addresses: std::collections::BTreeSet<usize> = mapped
                    .iter()
                    .map(|pair| pair[column])
                    .filter(|address| *address != 0)
                    .collect();
                assert_eq!(addresses.len(), 1, "round {round}: one address per page");
            }
            let pages = region.pages.load_full();
            assert_eq!(pages.len(), target + 2, "round {round}");
            let distinct: std::collections::BTreeSet<usize> =
                pages.iter().map(|page| page.pointer() as usize).collect();
            assert_eq!(distinct.len(), pages.len());
            let mut reader = ProcessWalConnection::new(Arc::clone(&region));
            assert!(reader.map(target + 2, PAGE, false).unwrap().is_null());
            // Only the published pages are still allocated.
            assert_eq!(live.load(Ordering::SeqCst), pages.len(), "round {round}");
            drop((pages, reader, region));
            assert_eq!(live.load(Ordering::SeqCst), 0, "round {round}");
        }
    }

    /// A mapped page starts zeroed and one connection's writes through its
    /// pointer are what another connection reads through its own.
    #[test]
    fn wal_page_is_zeroed_and_shared_between_connections() {
        let region = Arc::new(ProcessWalRegion::new());
        let mut writer = ProcessWalConnection::new(Arc::clone(&region));
        let mut reader = ProcessWalConnection::new(Arc::clone(&region));
        let written = writer.map(1, 512, true).unwrap().cast::<u8>();
        let read = reader.map(1, 512, false).unwrap().cast::<u8>();
        // SAFETY: both pointers address the 512-byte page the region keeps
        // alive, and only this thread touches it.
        unsafe {
            assert!((0..512).all(|at| read.add(at).read() == 0));
            written.add(511).write(9);
            assert_eq!(read.add(511).read(), 9);
        }
    }

    /// Metadata whose next publish stops on entry, before it reaches the
    /// provider, until the test lets it go on.
    #[derive(Clone)]
    struct ParkingMetadata {
        inner: mount_rs_memory::MemoryMetadataStore,
        gate: Arc<ParkGate>,
    }

    #[derive(Default)]
    struct ParkGate {
        /// (armed, parked, released)
        steps: std::sync::Mutex<(bool, bool, bool)>,
        changed: std::sync::Condvar,
    }

    impl ParkingMetadata {
        fn park_next_publish(&self) {
            *self.gate.steps.lock().unwrap() = (true, false, false);
        }

        fn wait_until_parked(&self) {
            let mut steps = self.gate.steps.lock().unwrap();
            while !steps.1 {
                steps = self.gate.changed.wait(steps).unwrap();
            }
        }

        fn release(&self) {
            self.gate.steps.lock().unwrap().2 = true;
            self.gate.changed.notify_all();
        }
    }

    #[async_trait::async_trait]
    impl MetadataStore for ParkingMetadata {
        fn durable(&self) -> bool {
            false
        }

        async fn load(&self) -> mount_rs_core::Result<LoadedMetadata> {
            self.inner.load().await
        }

        async fn acquire_writer(
            &self,
            owner: &str,
            ttl: Duration,
        ) -> mount_rs_core::Result<WriterLease> {
            self.inner.acquire_writer(owner, ttl).await
        }

        async fn renew_writer(
            &self,
            lease: &WriterLease,
            ttl: Duration,
        ) -> mount_rs_core::Result<WriterLease> {
            self.inner.renew_writer(lease, ttl).await
        }

        async fn release_writer(&self, lease: &WriterLease) -> mount_rs_core::Result<()> {
            self.inner.release_writer(lease).await
        }

        async fn publish(
            &self,
            expected_revision: u64,
            lease: &WriterLease,
            namespace: Namespace,
        ) -> mount_rs_core::Result<u64> {
            {
                // The bridge calls this from a blocking callback, so the
                // thread itself waits.
                let mut steps = self.gate.steps.lock().unwrap();
                if steps.0 {
                    steps.0 = false;
                    steps.1 = true;
                    self.gate.changed.notify_all();
                    while !steps.2 {
                        steps = self.gate.changed.wait(steps).unwrap();
                    }
                }
            }
            self.inner
                .publish(expected_revision, lease, namespace)
                .await
        }

        async fn flush(&self) -> mount_rs_core::Result<()> {
            self.inner.flush().await
        }
    }

    /// The volume state is held across a publication. A commit has read
    /// the revision it publishes from and is on its way to the provider
    /// when another connection deletes a file (SQLite removes a journal
    /// through whichever connection gets there). The delete must wait and
    /// publish from the commit's revision; publishing from the same one,
    /// one of the two is refused and the volume fails closed for good.
    #[test]
    fn a_delete_waits_for_a_commit_that_is_publishing() {
        let metadata = ParkingMetadata {
            inner: mount_rs_memory::MemoryMetadataStore::new(),
            gate: Arc::default(),
        };
        let backend = StorageBackend::new(
            metadata.clone(),
            mount_rs_memory::MemoryBlockStore::new(),
            StorageOptions::volatile_for_tests("race", 4096).unwrap(),
        )
        .unwrap();
        let volume = &backend.volume;
        let owner = volume.next_handle_id().unwrap();
        volume.acquire_owner(owner).unwrap();
        volume.commit_file(b"/db", b"one", owner).unwrap();
        volume
            .commit_file(b"/db-journal", b"journal", owner)
            .unwrap();

        metadata.park_next_publish();
        std::thread::scope(|threads| {
            let commit = threads.spawn(|| volume.commit_file(b"/db", b"two", owner));
            metadata.wait_until_parked();
            let delete = threads.spawn(|| volume.delete_file(b"/db-journal"));
            // Unheld, the delete is done within this wait; held, it is
            // parked on the state and the wait only runs out.
            let waited = std::time::Instant::now();
            while !delete.is_finished() && waited.elapsed() < Duration::from_millis(100) {
                std::thread::sleep(Duration::from_millis(1));
            }
            metadata.release();
            commit
                .join()
                .unwrap()
                .expect("the commit was refused: a delete published from its revision");
            delete
                .join()
                .unwrap()
                .expect("the delete was refused: it published from a stale revision");
        });
        volume
            .ensure_healthy()
            .expect("a commit and a delete that overlapped failed the volume");
        assert_eq!(volume.load_file(b"/db").unwrap(), b"two");
        assert!(!volume.file_exists(b"/db-journal").unwrap());
        volume.release_owner(owner).unwrap();
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
