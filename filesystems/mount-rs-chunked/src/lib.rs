//! Filesystem orchestration over independent metadata and immutable block stores.
//!
//! [`ChunkedFs`] deliberately keeps namespace records and file bytes in
//! separate providers. A write stores new immutable blocks and flushes them
//! before a coordinated metadata revision publishes references to those blocks.
//! The implementation is intentionally not a snapshot adapter and does not
//! provide copy-on-write views.

mod migration;
pub use migration::{migrate_mrc1_backing, migrate_trusted_unstamped_mrc1_backing};

use async_trait::async_trait;
use mount_rs_core::chunking::{Chunker, FixedSizeChunker, from_config};
use mount_rs_core::diagnostics::RequestTrace;
use mount_rs_core::diagnostics::profile::{self, Event, Span};
use mount_rs_core::driver::{
    FileHandle, FsDriver, GuardedDirectoryEntry, GuardedMutation, GuardedMutationResult,
    GuardedRead, GuardedReadResult, GuardedSetattr, ObservedEntry, PathGuard, PathIdentity,
};
use mount_rs_core::error::{ErrorCode, FsError, Result};
use mount_rs_core::handle::OpenFlags;
use mount_rs_core::path::{is_path_inside, normalize_path, split_path};
use mount_rs_core::storage::{
    BlockExtent, BlockReconcileReport, BlockStore, CheckoutRequest, ConcurrentBackingId,
    ConcurrentModeState, DelegatedCheckin, DelegatedPublish, DirectoryGrant, FileLayout, InodeId,
    InodeVersion, MetadataStore, NAMESPACE_FORMAT_VERSION, Namespace, NodeData, NodeMetadata,
    WriterLease,
};
use mount_rs_core::types::{
    Capabilities, DirEntry, FileType, MkdirOptions, S_IFDIR, S_IFMT, S_IFREG, Stats, StatsFs,
    now_ms,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BLOCK_SIZE: u64 = 4096;
const MAX_SYMLINK_DEPTH: usize = 40;
const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
const LEASE_RENEWAL_MARGIN: Duration = Duration::from_secs(5);
const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;
const MAX_PENDING_MUTATIONS: usize = 1024;
// Eight continuously active writers can exhaust 32 or 64 quick CAS attempts
// while other writers keep advancing the shared revision. The timed backoff
// below caps each delay at 20 ms, so 128 attempts bound retry waiting to at
// most about 2.6 seconds before reporting a known, uncommitted conflict.
const MAX_CONCURRENT_CAS_RETRIES: usize = 128;
const CAS_BACKOFF_INITIAL_MICROS: u64 = 250;
const CAS_BACKOFF_MAX_MICROS: u64 = 20_000;
// The W26 Ozone qualification uses 64 concurrent lifecycle workers. Start
// with the small fast-path window used by local providers, then extend it
// only while newly-prepared remote operations are still arriving. The total
// window remains bounded and scheduler-yield based rather than depending on
// provider-controlled time.
const MUTATION_BATCH_INITIAL_YIELD_ROUNDS: usize = 8;
const MUTATION_BATCH_MAX_YIELD_ROUNDS: usize = 64;
const MUTATION_BATCH_IDLE_YIELD_ROUNDS: usize = 2;
const MUTATION_BATCH_REQUEST_TARGET: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReacquiredFence {
    Next,
    Fenced,
    Overflow,
}

/// An expired writer may resume only with the immediately next fence token.
/// A skipped or wrapped token means some other ownership event may have
/// intervened, even when the provider returns the same owner name.
fn classify_reacquired_fence(prior: u64, acquired: u64) -> ReacquiredFence {
    match prior.checked_add(1) {
        None => ReacquiredFence::Overflow,
        Some(next) if acquired == next => ReacquiredFence::Next,
        Some(_) => ReacquiredFence::Fenced,
    }
}

#[cfg(kani)]
mod verification {
    use super::{ReacquiredFence, classify_reacquired_fence};

    /// The production lease-recovery guard accepts only a strict one-step
    /// increase over full-range fence tokens, never a wrap or skipped token.
    #[kani::proof]
    fn reacquired_writer_fence_is_exactly_next() {
        let prior: u64 = kani::any();
        let acquired: u64 = kani::any();
        let decision = classify_reacquired_fence(prior, acquired);
        let exactly_next = acquired > prior && acquired - prior == 1;

        assert_eq!(decision == ReacquiredFence::Next, exactly_next);
        assert_eq!(decision == ReacquiredFence::Overflow, prior == u64::MAX);
        kani::cover!(prior == u64::MAX && decision == ReacquiredFence::Overflow);
        kani::cover!(prior == 7 && acquired == 8 && decision == ReacquiredFence::Next);
        kani::cover!(prior == 7 && acquired == 9 && decision == ReacquiredFence::Fenced);
        kani::cover!(prior == 7 && acquired == 7 && decision == ReacquiredFence::Fenced);
    }
}

/// Runtime configuration for a newly-created namespace.
///
/// Coordination policy for independently mounted clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnershipMode {
    Exclusive,
    Shared,
}

/// The chunker's serialized configuration is stored in the namespace and in
/// every file layout. Existing files always use their persisted configuration;
/// changing this value only affects a newly-created namespace.
#[derive(Clone)]
pub struct ChunkedOptions {
    pub owner: String,
    pub lease_ttl: Duration,
    pub concurrent_writes: bool,
    /// Opt in to independently versioned inode publication.
    pub inode_updates: bool,
    pub writeback: bool,
    pub delegated: bool,
    pub checkout_path: Option<String>,
    pub chunker: Arc<dyn Chunker>,
    pub uid: u32,
    pub gid: u32,
    pub umask: u32,
    pub root_mode: u32,
}

impl ChunkedOptions {
    pub fn new(owner: impl Into<String>, chunker: Arc<dyn Chunker>) -> Self {
        Self {
            owner: owner.into(),
            lease_ttl: DEFAULT_LEASE_TTL,
            concurrent_writes: false,
            inode_updates: false,
            writeback: false,
            delegated: false,
            checkout_path: None,
            chunker,
            uid: 0,
            gid: 0,
            umask: 0,
            root_mode: 0o755,
        }
    }

    pub fn fixed(owner: impl Into<String>, chunk_size: usize) -> Result<Self> {
        Ok(Self::new(
            owner,
            Arc::new(FixedSizeChunker::new(chunk_size)?),
        ))
    }

    pub fn with_lease_ttl(mut self, lease_ttl: Duration) -> Self {
        self.lease_ttl = lease_ttl;
        self
    }

    pub fn with_concurrent_writes(mut self, concurrent_writes: bool) -> Self {
        self.concurrent_writes = concurrent_writes;
        self.delegated = false;
        self.checkout_path = None;
        self
    }

    pub fn with_inode_updates(mut self, enabled: bool) -> Self {
        self.inode_updates = enabled;
        if enabled {
            self.concurrent_writes = true;
        }
        self
    }

    pub fn with_writeback(mut self, writeback: bool) -> Self {
        self.writeback = writeback;
        self
    }

    pub fn with_ownership_mode(mut self, mode: OwnershipMode) -> Self {
        self.concurrent_writes = mode == OwnershipMode::Shared;
        self.writeback = mode == OwnershipMode::Exclusive;
        self.delegated = mode == OwnershipMode::Shared;
        if mode == OwnershipMode::Exclusive {
            self.checkout_path = None;
        }
        self
    }

    pub fn with_checkout_path(mut self, path: impl Into<String>) -> Self {
        self = self.with_ownership_mode(OwnershipMode::Shared);
        self.checkout_path = Some(path.into());
        self
    }

    pub fn with_identity(mut self, uid: u32, gid: u32, umask: u32) -> Self {
        self.uid = uid;
        self.gid = gid;
        self.umask = umask;
        self
    }

    pub fn with_root_mode(mut self, root_mode: u32) -> Self {
        self.root_mode = root_mode;
        self
    }
}

impl Default for ChunkedOptions {
    fn default() -> Self {
        static NEXT_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let sequence = NEXT_OWNER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self::fixed(
            format!("mount-rs-chunked-{}-{sequence}", std::process::id()),
            DEFAULT_CHUNK_SIZE,
        )
        .expect("the built-in chunk size is non-zero")
    }
}

type AsyncGateGuard<'a> = tokio::sync::MutexGuard<'a, ()>;

#[derive(Clone)]
struct AsyncGate {
    state: Arc<tokio::sync::Mutex<()>>,
}

impl AsyncGate {
    fn new() -> Self {
        Self {
            state: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    async fn lock(&self) -> AsyncGateGuard<'_> {
        self.state.lock().await
    }
}

struct RuntimeState {
    namespace: Arc<Namespace>,
    inode_revisions: BTreeMap<InodeId, u64>,
    selected_inodes: HashMap<InodeId, Arc<NodeMetadata>>,
    /// Local operation generation, incremented for every staged mutation.
    revision: u64,
    /// Provider CAS revision; local generations must never be used for fencing.
    persisted_revision: u64,
    pending_namespace: bool,
    next_fd: u64,
    open_refs: HashMap<InodeId, u64>,
    /// Read atime changes are visible immediately to this coordinator and are
    /// folded into the next fenced namespace publication. Keeping them out of
    /// the read acknowledgement path avoids a remote metadata commit for
    /// every small read while syncfs/fsync and graceful shutdown retain the
    /// durability boundary.
    pending_atime: HashMap<InodeId, i64>,
    /// Unlinked inodes remain available to existing handles but are never
    /// published. They are reclaimed on the last close or process restart.
    orphans: HashMap<InodeId, NodeMetadata>,
    failure: Option<FsError>,
    closed: bool,
}

enum ReadLayoutSnapshot {
    Selected(Arc<NodeMetadata>),
    SharedNamespace {
        namespace: Arc<Namespace>,
        inode: InodeId,
    },
    Owned(FileLayout),
}

impl ReadLayoutSnapshot {
    fn layout(&self) -> Result<&FileLayout> {
        match self {
            Self::Selected(node) => match &node.data {
                NodeData::File(layout) => Ok(layout),
                _ => Err(FsError::backend("captured inode is not a file")),
            },
            Self::Owned(layout) => Ok(layout),
            Self::SharedNamespace { namespace, inode } => {
                match namespace.nodes.get(inode).map(|node| &node.data) {
                    Some(NodeData::File(layout)) => Ok(layout),
                    _ => Err(FsError::backend("captured read layout is missing")),
                }
            }
        }
    }
}

fn retain_open_detached(state: &mut RuntimeState, next: &Namespace) {
    let detached: Vec<_> = state
        .open_refs
        .iter()
        .filter(|(inode, count)| **count > 0 && !next.nodes.contains_key(inode))
        .filter_map(|(inode, _)| {
            state
                .selected_inodes
                .get(inode)
                .map(|node| (**node).clone())
                .or_else(|| state.namespace.nodes.get(inode).cloned())
                .map(|node| (*inode, node))
        })
        .collect();
    for (inode, node) in detached {
        state.orphans.entry(inode).or_insert(node);
    }
}

enum MutationResult {
    WholeFile(WholeFileMutationResult),
    Unit,
}

enum WholeFileMutationResult {
    Committed,
    Conflict,
}

#[derive(Clone)]
struct WholeFileMutation {
    path: String,
    inode: InodeId,
    expected_revision: u64,
    new_inode: bool,
    original: Option<NodeMetadata>,
    layout: FileLayout,
    data_length: u64,
}

enum MutationRequest {
    WholeFile {
        mutation: Box<WholeFileMutation>,
        reply: tokio::sync::oneshot::Sender<Result<MutationResult>>,
        committed: Arc<AtomicBool>,
    },
    Unlink {
        path: String,
        reply: tokio::sync::oneshot::Sender<Result<MutationResult>>,
        committed: Arc<AtomicBool>,
    },
}

impl MutationRequest {
    fn mark_committed(&self) {
        match self {
            Self::WholeFile { committed, .. } | Self::Unlink { committed, .. } => {
                committed.store(true, Ordering::Release);
            }
        }
    }
}

struct MutationQueue {
    pending: Vec<MutationRequest>,
    running: bool,
}

impl MutationQueue {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            running: false,
        }
    }
}

struct MutationRunnerGuard<'a> {
    queue: &'a Mutex<MutationQueue>,
    active: bool,
}

/// Once a metadata publication starts, dropping its future leaves the commit
/// outcome unknown. Preserve that uncertainty even when the caller cancels
/// before the provider returns or the local state records its acknowledgement.
struct PublicationGuard<'a> {
    state: &'a Mutex<RuntimeState>,
    active: bool,
}

impl<'a> PublicationGuard<'a> {
    fn new(state: &'a Mutex<RuntimeState>) -> Self {
        Self {
            state,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for PublicationGuard<'_> {
    fn drop(&mut self) {
        if self.active
            && let Ok(mut state) = self.state.lock()
            && state.failure.is_none()
        {
            state.failure = Some(
                FsError::new(ErrorCode::Eio)
                    .with_syscall("metadata-publish")
                    .with_message("metadata publication canceled before its outcome was known"),
            );
        }
    }
}

/// Sending a committed batch response does not prove that its caller observed
/// it. A follower may be canceled after the one-shot send and before polling
/// its queued response; that dropped caller must preserve the uncertain result.
struct MutationAcknowledgementGuard<'a> {
    state: &'a Mutex<RuntimeState>,
    committed: Arc<AtomicBool>,
    observed: bool,
}

impl<'a> MutationAcknowledgementGuard<'a> {
    fn new(state: &'a Mutex<RuntimeState>, committed: Arc<AtomicBool>) -> Self {
        Self {
            state,
            committed,
            observed: false,
        }
    }

    fn disarm(&mut self) {
        self.observed = true;
    }
}

impl Drop for MutationAcknowledgementGuard<'_> {
    fn drop(&mut self) {
        if !self.observed
            && self.committed.load(Ordering::Acquire)
            && let Ok(mut state) = self.state.lock()
            && state.failure.is_none()
        {
            state.failure = Some(
                FsError::new(ErrorCode::Eio)
                    .with_syscall("mutation-batch")
                    .with_message("caller canceled before observing a committed mutation"),
            );
        }
    }
}

/// Marks a whole-file operation whose immutable block work is still being
/// prepared. The mutation runner uses this as a scheduling hint only: the
/// fenced metadata publication remains the sole commit boundary, and the
/// guard is released before the prepared request waits for its response.
struct MutationPreparationGuard {
    active: bool,
    counter: Arc<AtomicUsize>,
}

impl MutationPreparationGuard {
    fn release(mut self) {
        self.active = false;
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for MutationPreparationGuard {
    fn drop(&mut self) {
        if self.active {
            self.counter.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl<'a> MutationRunnerGuard<'a> {
    fn new(queue: &'a Mutex<MutationQueue>) -> Self {
        Self {
            queue,
            active: true,
        }
    }

    fn finish(&mut self) {
        self.active = false;
    }
}

impl Drop for MutationRunnerGuard<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        queue.running = false;
        let pending = std::mem::take(&mut queue.pending);
        drop(queue);
        let error = FsError::new(ErrorCode::Eio).with_message("mutation batch runner canceled");
        for request in pending {
            mutation_reply(request, Err(error.clone()));
        }
    }
}

/// A cooperative yield is enough to let concurrent remote block operations
/// make progress without requiring a runtime-spawned worker, which is
/// important because the core ChunkedFs tests deliberately run without a
/// Tokio runtime.
struct CooperativeYield {
    yielded: bool,
}

impl Future for CooperativeYield {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

async fn cooperative_yield() {
    CooperativeYield { yielded: false }.await;
}

/// A known CAS miss did not commit. Stagger independent coordinators with an
/// exponentially widening, bounded async delay before reloading the winner's
/// namespace. One executor turn alone cannot prevent hot workers on separate
/// threads from repeatedly racing the same next revision. This timer works
/// without a Tokio runtime, as required by the core and provider tests.
async fn concurrent_cas_backoff(attempt: usize, owner: &str) {
    let window = CAS_BACKOFF_INITIAL_MICROS << attempt.min(6);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| u64::from(duration.subsec_nanos()));
    let owner_salt = owner.bytes().fold(0xcbf29ce484222325_u64, |salt, byte| {
        (salt ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    let jitter = (owner_salt ^ tick ^ (attempt as u64).wrapping_mul(0x9e3779b97f4a7c15)) % window;
    let delay = Duration::from_micros((window + jitter).min(CAS_BACKOFF_MAX_MICROS));
    async_io::Timer::after(delay).await;
}

struct ChunkedInner<M, B>
where
    M: MetadataStore,
    B: BlockStore,
{
    metadata: Arc<M>,
    blocks: Arc<B>,
    concurrent_backing: Option<ConcurrentBackingId>,
    delegation: Mutex<Option<DirectoryGrant>>,
    delegation_generation: std::sync::atomic::AtomicU64,
    pending_checkout: Mutex<Option<CheckoutRequest>>,
    delegation_draining: AtomicBool,
    options: ChunkedOptions,
    gate: AsyncGate,
    lifecycle: tokio::sync::RwLock<()>,
    state: Mutex<RuntimeState>,
    lease: Mutex<Option<WriterLease>>,
    lease_gate: AsyncGate,
    lease_renewed: AtomicBool,
    preparing_mutations: Arc<AtomicUsize>,
    mutations: Mutex<MutationQueue>,
}

/// A filesystem driver composed from one metadata provider and one block
/// provider. The providers may be different implementations and may have
/// different durability characteristics.
pub struct ChunkedFs<M, B>
where
    M: MetadataStore,
    B: BlockStore,
{
    inner: Arc<ChunkedInner<M, B>>,
}

impl<M, B> Clone for ChunkedFs<M, B>
where
    M: MetadataStore,
    B: BlockStore,
{
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<M, B> ChunkedFs<M, B>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    async fn open_delegated(metadata: M, blocks: B, options: ChunkedOptions) -> Result<Self> {
        if !options.concurrent_writes || options.writeback {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("delegated ownership requires shared write-through mode"));
        }
        let mut delegated = metadata.delegation_state().await?;
        let backing = if let Some(state) = &delegated {
            blocks.verify_concurrent_backing(state.backing).await?;
            state.backing
        } else {
            if metadata.concurrent_mode_state().await? != ConcurrentModeState::Legacy {
                return Err(FsError::new(ErrorCode::Ebusy).with_message(
                    "delegated ownership requires explicit offline protocol migration",
                ));
            }
            let loaded = metadata.load().await?;
            loaded.validate()?;
            if loaded.revision != 0 || loaded.namespace.is_some() {
                return Err(FsError::new(ErrorCode::Ebusy).with_message(
                    "existing namespaces require explicit offline delegated enrollment",
                ));
            }
            // Establish block-provider support before initializing metadata. The final
            // delegated enrollment still follows durable Legacy root publication/release.
            let backing = blocks.prepare_concurrent_backing().await?;
            let lease = metadata
                .acquire_writer(&options.owner, options.lease_ttl)
                .await?;
            let initialized = async {
                let namespace = initial_namespace(&options)?;
                blocks.flush().await?;
                let revision = metadata.publish(0, &lease, namespace).await?;
                if !metadata.publish_includes_flush_barrier() {
                    metadata.flush().await?;
                }
                Ok::<_, FsError>(revision)
            }
            .await;
            let released = metadata.release_writer(&lease).await;
            let revision = initialized?;
            released?;
            metadata.prepare_delegated_mode(backing, revision).await?;
            blocks.verify_concurrent_backing(backing).await?;
            delegated = metadata.delegation_state().await?;
            backing
        };
        let loaded = metadata.load().await?;
        loaded.validate()?;
        let namespace = loaded
            .namespace
            .ok_or_else(|| FsError::backend("delegated namespace missing"))?;
        let authority = delegated.ok_or_else(|| FsError::backend("delegated authority missing"))?;
        if authority.backing != backing {
            return Err(FsError::new(ErrorCode::Estale));
        }
        authority.validate(&namespace)?;
        let checkout = options.checkout_path.clone();
        let fs = Self {
            inner: Arc::new(ChunkedInner {
                metadata: Arc::new(metadata),
                blocks: Arc::new(blocks),
                concurrent_backing: Some(backing),
                delegation: Mutex::new(None),
                delegation_generation: std::sync::atomic::AtomicU64::new(0),
                pending_checkout: Mutex::new(None),
                delegation_draining: AtomicBool::new(false),
                options,
                gate: AsyncGate::new(),
                lifecycle: tokio::sync::RwLock::new(()),
                state: Mutex::new(RuntimeState {
                    inode_revisions: BTreeMap::new(),
                    selected_inodes: HashMap::new(),
                    namespace: Arc::new(namespace),
                    revision: loaded.revision,
                    persisted_revision: loaded.revision,
                    pending_namespace: false,
                    next_fd: 3,
                    open_refs: HashMap::new(),
                    pending_atime: HashMap::new(),
                    orphans: HashMap::new(),
                    failure: None,
                    closed: false,
                }),
                lease: Mutex::new(None),
                lease_gate: AsyncGate::new(),
                lease_renewed: AtomicBool::new(false),
                preparing_mutations: Arc::new(AtomicUsize::new(0)),
                mutations: Mutex::new(MutationQueue::new()),
            }),
        };
        if let Some(path) = checkout {
            fs.checkout_scope(&path).await?;
        }
        Ok(fs)
    }

    fn local_grant(&self) -> Result<Option<DirectoryGrant>> {
        self.inner
            .delegation
            .lock()
            .map(|grant| grant.clone())
            .map_err(|_| FsError::backend("directory authority lock poisoned"))
    }

    async fn refresh_delegation(&self) -> Result<()> {
        let authority = self
            .inner
            .metadata
            .delegation_state()
            .await?
            .ok_or_else(|| self.fail_closed(FsError::new(ErrorCode::Estale)))?;
        if Some(authority.backing) != self.inner.concurrent_backing {
            return Err(self.fail_closed(FsError::new(ErrorCode::Estale)));
        }
        if let Some(local) = self.local_grant()? {
            let actual = authority
                .grants
                .get(&local.token.root)
                .filter(|grant| grant.token == local.token)
                .ok_or_else(|| {
                    self.fail_closed(
                        FsError::new(ErrorCode::Estale)
                            .with_message("directory authority was retired"),
                    )
                })?;
            *self
                .inner
                .delegation
                .lock()
                .map_err(|_| FsError::backend("directory authority lock poisoned"))? =
                Some(actual.clone());
        }
        Ok(())
    }

    /// Inspect this coordinator's current authority. Shared without a grant permits namespace discovery only.
    pub async fn delegation_status(&self) -> Result<Option<DirectoryGrant>> {
        if !self.inner.options.delegated {
            return Err(FsError::new(ErrorCode::Enotsup));
        }
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.refresh_concurrent_namespace().await?;
        self.local_grant()
    }

    /// Acquire one directory for direct-driver access. Native mounts require unmount/remount at handoff.
    pub async fn checkout_scope(&self, path: &str) -> Result<DirectoryGrant> {
        validate_checkout_path(path)?;
        if !self.inner.options.delegated {
            return Err(FsError::new(ErrorCode::Enotsup));
        }
        let _lifecycle = self.inner.lifecycle.write().await;
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        if self.local_grant()?.is_some() {
            return Err(
                FsError::new(ErrorCode::Ebusy).with_message("coordinator already owns a scope")
            );
        }
        self.refresh_concurrent_namespace().await?;
        let (namespace, _) = self.snapshot()?;
        let root = resolve(&namespace, &normalize_path(path), true, "checkout")?;
        let generation = self
            .inner
            .delegation_generation
            .load(Ordering::SeqCst)
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let sequence = NEXT_SESSION.fetch_add(1, Ordering::SeqCst);
        if sequence == u64::MAX {
            return Err(FsError::new(ErrorCode::Eoverflow));
        }
        use std::hash::BuildHasher;
        let nonce_a = std::collections::hash_map::RandomState::new().hash_one(sequence);
        let nonce_b = std::collections::hash_map::RandomState::new().hash_one(sequence);
        let owner = format!("{}-{nonce_a:016x}{nonce_b:016x}", self.inner.options.owner);
        let request = {
            let mut pending = self
                .inner
                .pending_checkout
                .lock()
                .map_err(|_| FsError::backend("checkout lock poisoned"))?;
            if let Some(request) = &*pending {
                if request.root != root {
                    return Err(FsError::new(ErrorCode::Ebusy)
                        .with_message("retry pending checkout with original scope"));
                }
                request.clone()
            } else {
                let request = CheckoutRequest {
                    backing: self
                        .inner
                        .concurrent_backing
                        .ok_or_else(|| FsError::new(ErrorCode::Estale))?,
                    root,
                    owner,
                };
                *pending = Some(request.clone());
                request
            }
        };
        let grant = match self.inner.metadata.checkout(&request).await {
            Ok(grant) => grant,
            Err(error) => {
                // A physical-authority check can report ESTALE after a committed claim.
                // Clear a denied request only after a fresh verified state proves it is inactive.
                let verified_stale_denial = if error.code == ErrorCode::Estale {
                    let verified = async {
                        self.inner
                            .blocks
                            .verify_concurrent_backing(request.backing)
                            .await?;
                        let authority = self
                            .inner
                            .metadata
                            .delegation_state()
                            .await?
                            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
                        Ok::<_, FsError>(
                            authority.backing == request.backing
                                && !authority
                                    .grants
                                    .values()
                                    .any(|grant| grant.token.owner == request.owner),
                        )
                    }
                    .await;
                    verified.unwrap_or(false)
                } else {
                    false
                };
                if verified_stale_denial
                    || matches!(
                        error.code,
                        ErrorCode::Ebusy
                            | ErrorCode::Einval
                            | ErrorCode::Enoent
                            | ErrorCode::Enotdir
                            | ErrorCode::Eacces
                            | ErrorCode::Enotsup
                            | ErrorCode::Eoverflow
                    )
                {
                    self.inner
                        .pending_checkout
                        .lock()
                        .map_err(|_| FsError::backend("checkout lock poisoned"))?
                        .take();
                }
                return Err(error);
            }
        };
        self.inner
            .pending_checkout
            .lock()
            .map_err(|_| FsError::backend("checkout lock poisoned"))?
            .take();
        *self
            .inner
            .delegation
            .lock()
            .map_err(|_| FsError::backend("directory authority lock poisoned"))? =
            Some(grant.clone());
        self.inner
            .delegation_generation
            .store(generation, Ordering::SeqCst);
        // Always reload after the claim: a disjoint owner may have published while checkout was pending.
        self.refresh_concurrent_namespace().await?;
        Ok(grant)
    }

    /// Drain file operations, reject open handles, flush and release the exact provider grant.
    pub async fn checkin_scope(&self) -> Result<()> {
        if !self.inner.options.delegated {
            return Err(FsError::new(ErrorCode::Enotsup));
        }
        let _lifecycle = self.inner.lifecycle.write().await;
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        let Some(grant) = self.local_grant()? else {
            return Ok(());
        };
        if !self.inner.delegation_draining.load(Ordering::SeqCst) {
            self.refresh_concurrent_namespace().await?;
        }
        {
            let state = self.lock_state()?;
            if !state.open_refs.is_empty() {
                return Err(
                    FsError::new(ErrorCode::Ebusy).with_message("close handles before checkin")
                );
            }
        }
        let generation = self
            .inner
            .delegation_generation
            .load(Ordering::SeqCst)
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        self.inner.delegation_draining.store(true, Ordering::SeqCst);
        let authority = self
            .inner
            .metadata
            .delegation_state()
            .await?
            .ok_or_else(|| self.fail_closed(FsError::new(ErrorCode::Estale)))?;
        let active = authority
            .grants
            .get(&grant.token.root)
            .is_some_and(|actual| actual.token == grant.token);
        if !active && !authority.retired.contains(&grant.token) {
            return Err(self.fail_closed(FsError::new(ErrorCode::Estale)));
        }
        if active {
            self.refresh_concurrent_namespace().await?;
            // A canceled last-close may have released its reference before its reap publication.
            // Complete that durable orphan cleanup before releasing authority.
            for attempt in 0..MAX_CONCURRENT_CAS_RETRIES {
                let (mut namespace, revision) = self.snapshot()?;
                let current = self
                    .local_grant()?
                    .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
                let mut changed = false;
                for inode in current.orphan_inodes {
                    changed |= namespace.nodes.remove(&inode).is_some();
                }
                if !changed {
                    break;
                }
                match self
                    .publish_namespace_durable(revision, namespace, false)
                    .await
                {
                    Ok(_) => break,
                    Err(error)
                        if error.code == ErrorCode::Eagain
                            && attempt + 1 < MAX_CONCURRENT_CAS_RETRIES =>
                    {
                        self.refresh_concurrent_namespace().await?;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        self.inner.blocks.flush().await?;
        self.inner.metadata.flush().await?;
        let loaded = self.inner.metadata.load().await?;
        loaded.validate()?;
        let expected_revision = loaded.revision;
        self.inner
            .metadata
            .checkin(&DelegatedCheckin {
                backing: self
                    .inner
                    .concurrent_backing
                    .ok_or_else(|| FsError::new(ErrorCode::Estale))?,
                token: grant.token,
                expected_revision,
            })
            .await?;
        self.inner
            .delegation_generation
            .store(generation, Ordering::SeqCst);
        *self
            .inner
            .delegation
            .lock()
            .map_err(|_| FsError::backend("directory authority lock poisoned"))? = None;
        let mut state = self.lock_state()?;
        state.orphans.clear();
        state.pending_atime.clear();
        state.namespace = Arc::new(
            loaded
                .namespace
                .ok_or_else(|| FsError::backend("delegated namespace missing"))?,
        );
        state.revision = expected_revision;
        state.persisted_revision = expected_revision;
        self.inner
            .delegation_draining
            .store(false, Ordering::SeqCst);
        Ok(())
    }

    fn require_inode_authority(&self, namespace: &Namespace, inode: InodeId) -> Result<()> {
        if !self.inner.options.delegated {
            return Ok(());
        }
        let grant = self.local_grant()?.ok_or_else(|| {
            FsError::new(ErrorCode::Eacces).with_message("checkout required for file access")
        })?;
        if grant.orphan_inodes.contains(&inode) {
            return Ok(());
        }
        let mut pending = vec![grant.token.root];
        let mut seen = BTreeSet::new();
        while let Some(current) = pending.pop() {
            if !seen.insert(current) {
                continue;
            }
            if current == inode {
                return Ok(());
            }
            if let Some(NodeMetadata {
                data: NodeData::Directory { entries },
                ..
            }) = namespace.nodes.get(&current)
            {
                pending.extend(entries.iter().map(|entry| entry.inode));
            }
        }
        Err(FsError::new(ErrorCode::Eacces).with_message("inode is outside checked-out directory"))
    }

    // Recovery is a read-only recognition of an already established authority.
    // In particular this never creates a missing block marker or replays a write.
    async fn verified_inode_startup(
        metadata: &M,
        blocks: &B,
        expected: Option<ConcurrentBackingId>,
    ) -> Result<
        Option<(
            ConcurrentBackingId,
            mount_rs_core::storage::InodeMetadataSnapshot,
        )>,
    > {
        let Some(mode) = metadata.inode_mode_state().await? else {
            return Ok(None);
        };
        if expected.is_some_and(|backing| backing != mode.backing) {
            return Err(FsError::new(ErrorCode::Estale));
        }
        blocks.verify_concurrent_backing(mode.backing).await?;
        let snapshot = metadata.load_inode_snapshot(mode.backing).await?;
        snapshot.validate()?;
        Ok(Some((mode.backing, snapshot)))
    }

    async fn startup_concurrent_mode(
        metadata: &M,
        blocks: &B,
        inode_updates: bool,
    ) -> Result<ConcurrentModeState> {
        match metadata.concurrent_mode_state().await {
            Ok(mode) => Ok(mode),
            Err(error) => {
                // Older provider inspectors may report EIO for MRC4. This is
                // a read, so independently verifying exact MRC4 is safe; no
                // mutating operation's ambiguous failure enters this path.
                if inode_updates
                    && let Some((backing, _)) =
                        Self::verified_inode_startup(metadata, blocks, None).await?
                {
                    return Ok(ConcurrentModeState::Mrc2(backing));
                }
                Err(error)
            }
        }
    }

    /// Open the current namespace. The default mode acquires a fenced writer
    /// lease; opt-in concurrent mode prepares the provider's revision-CAS
    /// protocol. An empty metadata store is initialized with an empty root
    /// directory through the selected publication path.
    pub async fn open(metadata: M, blocks: B, options: ChunkedOptions) -> Result<Self> {
        if let Some(path) = &options.checkout_path {
            validate_checkout_path(path)?;
            if !options.delegated {
                return Err(FsError::new(ErrorCode::Einval)
                    .with_message("checkout path requires delegated ownership"));
            }
        }
        if options.inode_updates
            && (!options.concurrent_writes || options.delegated || options.writeback)
        {
            return Err(FsError::new(ErrorCode::Einval).with_message(
                "inode updates require concurrent writes without delegated ownership or writeback",
            ));
        }
        if options.concurrent_writes && options.writeback {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("shared ownership cannot enable writeback"));
        }
        if options.delegated {
            return Self::open_delegated(metadata, blocks, options).await;
        }
        let metadata = Arc::new(metadata);
        let blocks = Arc::new(blocks);
        if options.concurrent_writes {
            // Inspect metadata before claiming a block authority. Established
            // MRC2 volumes must verify their persisted block marker read-only;
            // recreating a missing marker could bind an unrelated backing.
            let inode_mode = if options.inode_updates {
                metadata.inode_mode_state().await?
            } else {
                None
            };
            let mut expected_backing = inode_mode.as_ref().map(|mode| mode.backing);
            let backing_result = async {
                Ok(if let Some(mode) = &inode_mode {
                    blocks.verify_concurrent_backing(mode.backing).await?;
                    mode.backing
                } else {
                    match Self::startup_concurrent_mode(
                        metadata.as_ref(),
                        blocks.as_ref(),
                        options.inode_updates,
                    )
                    .await?
                    {
                        ConcurrentModeState::Mrc2(id) => {
                            expected_backing = Some(id);
                            blocks.verify_concurrent_backing(id).await?;
                            metadata.prepare_bound_concurrent_mode(id).await?;
                            id
                        }
                        ConcurrentModeState::Mrc1 => {
                            return Err(FsError::new(ErrorCode::Ebusy)
                                .with_syscall("migrate MRC1 backing")
                                .with_message(
                                    "stop old mounts and run migrate-concurrent-backing",
                                ));
                        }
                        ConcurrentModeState::Legacy => {
                            match metadata.preflight_new_bound_mode().await {
                                Ok(()) => {
                                    let id = blocks.prepare_concurrent_backing().await?;
                                    expected_backing = Some(id);
                                    metadata.prepare_bound_concurrent_mode(id).await?;
                                    blocks.verify_concurrent_backing(id).await?;
                                    id
                                }
                                Err(error) => {
                                    // A peer may enroll MRC2 after the Legacy inspection.
                                    // Reuse its authority only through the established
                                    // read-only block verification path.
                                    let ConcurrentModeState::Mrc2(id) =
                                        Self::startup_concurrent_mode(
                                            metadata.as_ref(),
                                            blocks.as_ref(),
                                            options.inode_updates,
                                        )
                                        .await?
                                    else {
                                        return Err(error);
                                    };
                                    expected_backing = Some(id);
                                    blocks.verify_concurrent_backing(id).await?;
                                    metadata.prepare_bound_concurrent_mode(id).await?;
                                    id
                                }
                            }
                        }
                    }
                })
            }
            .await;
            let backing = match backing_result {
                Ok(backing) => backing,
                Err(error)
                    if options.inode_updates
                        && matches!(error.code, ErrorCode::Estale | ErrorCode::Enotsup) =>
                {
                    Self::verified_inode_startup(
                        metadata.as_ref(),
                        blocks.as_ref(),
                        expected_backing,
                    )
                    .await?
                    .ok_or(error)?
                    .0
                }
                Err(error) => return Err(error),
            };
            // Two clients may initialize one fresh volume together. Only one
            // CAS publishes its root; the loser reloads the winner's root.
            for attempt in 0..MAX_CONCURRENT_CAS_RETRIES {
                let mut established_inode_mode = if options.inode_updates {
                    metadata.inode_mode_state().await?
                } else {
                    None
                };
                let loaded = if established_inode_mode.is_some() {
                    let snapshot = metadata.load_inode_snapshot(backing).await?;
                    mount_rs_core::storage::LoadedMetadata {
                        revision: snapshot.structural_generation,
                        namespace: Some(snapshot.namespace),
                    }
                } else {
                    match metadata.load().await {
                        Ok(loaded) => loaded,
                        Err(error) if options.inode_updates && error.code == ErrorCode::Estale => {
                            let (_, snapshot) = Self::verified_inode_startup(
                                metadata.as_ref(),
                                blocks.as_ref(),
                                Some(backing),
                            )
                            .await?
                            .ok_or(error)?;
                            established_inode_mode = metadata.inode_mode_state().await?;
                            mount_rs_core::storage::LoadedMetadata {
                                revision: snapshot.structural_generation,
                                namespace: Some(snapshot.namespace),
                            }
                        }
                        Err(error) => return Err(error),
                    }
                };
                loaded.validate()?;
                let (namespace, revision) = match loaded.namespace {
                    Some(namespace) => (namespace, loaded.revision),
                    None if loaded.revision == 0 => {
                        let namespace = initial_namespace(&options)?;
                        namespace.validate()?;
                        blocks.flush().await?;
                        blocks.verify_concurrent_backing(backing).await?;
                        match metadata
                            .publish_bound_if_revision(backing, 0, namespace.clone())
                            .await
                        {
                            Ok(revision) => {
                                if !metadata.publish_includes_flush_barrier() {
                                    metadata.flush().await?;
                                }
                                (namespace, revision)
                            }
                            Err(error) if error.code == ErrorCode::Eagain => {
                                concurrent_cas_backoff(attempt, &options.owner).await;
                                continue;
                            }
                            Err(error)
                                if options.inode_updates && error.code == ErrorCode::Estale =>
                            {
                                Self::verified_inode_startup(
                                    metadata.as_ref(),
                                    blocks.as_ref(),
                                    Some(backing),
                                )
                                .await?
                                .ok_or(error)?;
                                continue;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    None => {
                        return Err(FsError::backend(
                            "metadata revision has no corresponding namespace",
                        ));
                    }
                };
                let (namespace, revision, inode_revisions) = if options.inode_updates {
                    if established_inode_mode.is_none() {
                        match metadata.prepare_inode_mode(backing, revision).await {
                            Ok(()) => {}
                            Err(error) if error.code == ErrorCode::Eagain => {
                                continue;
                            }
                            Err(error) if error.code == ErrorCode::Estale => {
                                Self::verified_inode_startup(
                                    metadata.as_ref(),
                                    blocks.as_ref(),
                                    Some(backing),
                                )
                                .await?
                                .ok_or(error)?;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    let snapshot = metadata.load_inode_snapshot(backing).await?;
                    snapshot.validate()?;
                    (
                        snapshot.namespace,
                        snapshot.structural_generation,
                        snapshot.inode_revisions,
                    )
                } else {
                    (namespace, revision, BTreeMap::new())
                };
                return Ok(Self {
                    inner: Arc::new(ChunkedInner {
                        metadata,
                        blocks,
                        concurrent_backing: Some(backing),
                        delegation: Mutex::new(None),
                        delegation_generation: std::sync::atomic::AtomicU64::new(0),
                        pending_checkout: Mutex::new(None),
                        delegation_draining: AtomicBool::new(false),
                        options,
                        gate: AsyncGate::new(),
                        lifecycle: tokio::sync::RwLock::new(()),
                        state: Mutex::new(RuntimeState {
                            inode_revisions,
                            selected_inodes: HashMap::new(),
                            namespace: Arc::new(namespace),
                            revision,
                            persisted_revision: revision,
                            pending_namespace: false,
                            next_fd: 3,
                            open_refs: HashMap::new(),
                            pending_atime: HashMap::new(),
                            orphans: HashMap::new(),
                            failure: None,
                            closed: false,
                        }),
                        lease: Mutex::new(None),
                        lease_gate: AsyncGate::new(),
                        lease_renewed: AtomicBool::new(false),
                        preparing_mutations: Arc::new(AtomicUsize::new(0)),
                        mutations: Mutex::new(MutationQueue::new()),
                    }),
                });
            }
            return Err(FsError::new(ErrorCode::Eagain)
                .with_syscall("initialize concurrent metadata")
                .with_message("another writer repeatedly changed the new volume"));
        }
        let lease = metadata
            .acquire_writer(&options.owner, options.lease_ttl)
            .await?;
        let loaded = match metadata.load().await {
            Ok(loaded) => loaded,
            Err(error) => {
                let _ = metadata.release_writer(&lease).await;
                return Err(error);
            }
        };
        if let Err(error) = loaded.validate() {
            let _ = metadata.release_writer(&lease).await;
            return Err(error);
        }

        let namespace_result: Result<(Namespace, bool)> = match loaded.namespace {
            Some(namespace) => Ok((namespace, false)),
            None => {
                if loaded.revision != 0 {
                    Err(FsError::backend(
                        "metadata revision has no corresponding namespace",
                    ))
                } else {
                    // This is the transient, unpublished initial namespace;
                    // validate it directly rather than manufacturing an
                    // invalid LoadedMetadata { revision: 0, namespace: Some }
                    // value for the provider-level validator.
                    initial_namespace(&options).and_then(|namespace| {
                        namespace.validate()?;
                        Ok((namespace, true))
                    })
                }
            }
        };
        let (namespace, needs_initial_publish) = match namespace_result {
            Ok(value) => value,
            Err(error) => {
                let _ = metadata.release_writer(&lease).await;
                return Err(error);
            }
        };

        let filesystem = Self {
            inner: Arc::new(ChunkedInner {
                metadata,
                blocks,
                concurrent_backing: None,
                delegation: Mutex::new(None),
                delegation_generation: std::sync::atomic::AtomicU64::new(0),
                pending_checkout: Mutex::new(None),
                delegation_draining: AtomicBool::new(false),
                options,
                gate: AsyncGate::new(),
                lifecycle: tokio::sync::RwLock::new(()),
                state: Mutex::new(RuntimeState {
                    inode_revisions: BTreeMap::new(),
                    selected_inodes: HashMap::new(),
                    namespace: Arc::new(namespace.clone()),
                    revision: loaded.revision,
                    persisted_revision: loaded.revision,
                    pending_namespace: false,
                    next_fd: 3,
                    open_refs: HashMap::new(),
                    pending_atime: HashMap::new(),
                    orphans: HashMap::new(),
                    failure: None,
                    closed: false,
                }),
                lease: Mutex::new(Some(lease.clone())),
                lease_gate: AsyncGate::new(),
                lease_renewed: AtomicBool::new(false),
                preparing_mutations: Arc::new(AtomicUsize::new(0)),
                mutations: Mutex::new(MutationQueue::new()),
            }),
        };

        if needs_initial_publish
            && let Err(error) = filesystem
                .publish_namespace_durable(loaded.revision, namespace, false)
                .await
        {
            // publish_namespace may have renewed the lease before the
            // publication or metadata barrier failed. Release the exact
            // latest token, not the acquisition token.
            let latest = filesystem.lock_lease().ok().and_then(|lease| lease.clone());
            if let Some(latest) = latest {
                let _ = filesystem.inner.metadata.release_writer(&latest).await;
            }
            return Err(error);
        }
        Ok(filesystem)
    }

    /// Release the provider lease. Handles become unusable after shutdown;
    /// callers should close handles before shutting down the filesystem.
    pub async fn shutdown(&self) -> Result<()> {
        if self.inner.options.delegated {
            self.checkin_scope().await?;
            self.lock_state()?.closed = true;
            return Ok(());
        }
        // Optimistic block I/O deliberately runs outside the operation gate.
        // Take the lifecycle write lock first so an in-flight write can
        // reacquire the operation gate and publish before shutdown fences and
        // releases the writer lease. New optimistic operations are prevented
        // from starting while this writer is queued.
        let _lifecycle = self.inner.lifecycle.write().await;
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        // A failed publication fails the coordinator closed. Pending atime
        // state must not be published after that boundary: snapshot() will
        // deliberately return the original failure, and returning early here
        // would strand the provider lease until its TTL expires. The failed
        // instance is already unusable, so release the lease and preserve the
        // fail-closed state instead.
        let (already_closed, failed) = {
            let state = self.lock_state()?;
            (state.closed, state.failure.is_some())
        };
        if !already_closed && !failed {
            self.drain_writeback().await?;
            self.flush_pending_atime().await?;
        }
        // Provider I/O can outlive the lease TTL (for example, a bounded
        // remote R2 write). Refresh our own lease before releasing it so a
        // graceful shutdown is not reported as ESTALE merely because the
        // last operation was slow. `renew_lease` safely reacquires the next
        // fence when our lease expired without another owner publishing a
        // revision; a fenced instance still fails closed.
        let refresh = if !already_closed && self.lock_lease()?.is_some() {
            self.validate_lease().await
        } else {
            Ok(())
        };
        {
            let mut state = self.lock_state()?;
            state.closed = true;
        }
        // A release may be canceled before the provider applies it, or may
        // return an ambiguous error. Keep the exact token for a later retry
        // until the provider acknowledges release. A failed refresh does not
        // prevent an attempt to release a still-valid cached token.
        let lease = self.lock_lease()?.clone();
        let Some(lease) = lease else {
            return refresh;
        };
        match self.inner.metadata.release_writer(&lease).await {
            Ok(()) => {
                self.lock_lease()?.take();
                refresh
            }
            Err(error) => Err(self.fail_closed(with_context(error, "lease-release", None))),
        }
    }

    pub fn metadata_store(&self) -> Arc<M> {
        Arc::clone(&self.inner.metadata)
    }

    pub fn block_store(&self) -> Arc<B> {
        Arc::clone(&self.inner.blocks)
    }

    /// Reconcile aged, unreferenced immutable blocks in this provider scope.
    ///
    /// The writer lease and operation gate exclude concurrent publications in
    /// this filesystem. The grace period additionally protects blocks from a
    /// crashed or ambiguous publication in another process. Open unlinked
    /// files remain roots until their final handle closes.
    pub async fn reconcile_blocks(&self, grace: Duration) -> Result<BlockReconcileReport> {
        if self.inner.options.concurrent_writes {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("reconcile blocks")
                .with_message(
                    "concurrent writers need distributed open-handle pins before block reclamation",
                ));
        }
        if grace.is_zero() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("reconcile blocks")
                .with_message("reconciliation grace period must be positive"));
        }
        let _lifecycle = self.inner.lifecycle.write().await;
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.validate_lease().await?;
        self.drain_writeback().await?;
        let (namespace, _) = self.snapshot()?;
        let live = {
            let state = self.lock_state()?;
            let mut live = BTreeSet::new();
            collect_block_roots(&namespace, &mut live);
            for node in state.orphans.values() {
                collect_block_roots_from_node(node, &mut live);
            }
            live
        };
        self.inner.blocks.reconcile(&live, grace).await
    }

    pub fn failed(&self) -> bool {
        self.inner
            .state
            .lock()
            .map(|state| state.failure.is_some())
            .unwrap_or(true)
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, RuntimeState>> {
        self.inner
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("chunked state lock poisoned"))
    }

    fn lock_lease(&self) -> Result<MutexGuard<'_, Option<WriterLease>>> {
        self.inner
            .lease
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("lease lock poisoned"))
    }

    fn snapshot(&self) -> Result<(Namespace, u64)> {
        let state = self.lock_state()?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.closed {
            return Err(FsError::new(ErrorCode::Ebadf).with_message("filesystem is closed"));
        }
        let _profile = Span::new(Event::Snapshot).units(state.namespace.nodes.len() as u64);
        let mut namespace = state.namespace.as_ref().clone();
        // Full snapshots are reserved for namespace operations. Fold selected
        // inode acknowledgements into that candidate without copying the
        // namespace on ordinary handle reads or writes.
        for (inode, node) in &state.selected_inodes {
            if namespace.nodes.contains_key(inode) {
                namespace.nodes.insert(*inode, (**node).clone());
            }
        }
        for (inode, atime_ms) in &state.pending_atime {
            if let Some(node) = namespace.nodes.get_mut(inode) {
                node.stats.atime_ms = node.stats.atime_ms.max(*atime_ms);
            }
        }
        Ok((namespace, state.revision))
    }

    fn node_snapshot(
        &self,
        namespace: &Namespace,
        inode: InodeId,
        syscall: &str,
        path: &str,
    ) -> Result<(NodeMetadata, bool)> {
        self.require_inode_authority(namespace, inode)?;
        if let Some(node) = namespace.nodes.get(&inode) {
            return Ok((node.clone(), false));
        }
        let state = self.lock_state()?;
        state
            .orphans
            .get(&inode)
            .cloned()
            .map(|node| (node, true))
            .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, path))
    }

    fn fail_closed(&self, error: FsError) -> FsError {
        mount_rs_core::diagnostics::trace_failure("chunked", "fail_closed", &error);
        if let Ok(mut state) = self.inner.state.lock()
            && state.failure.is_none()
        {
            state.failure = Some(error.clone());
        }
        error
    }

    /// A concurrent client cannot trust its local namespace after another
    /// process commits. Refresh before each lookup or mutation. In-flight
    /// local operations may finish between the remote load and state lock;
    /// never replace a newer locally acknowledged revision with an older one.
    fn check_inode_runtime(&self) -> Result<()> {
        let state = self.lock_state()?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.closed {
            return Err(FsError::new(ErrorCode::Ebadf).with_message("filesystem is closed"));
        }
        Ok(())
    }

    async fn refresh_selected_inode(&self, inode: InodeId) -> Result<()> {
        self.check_inode_runtime()?;
        let backing = self
            .inner
            .concurrent_backing
            .ok_or_else(|| FsError::new(ErrorCode::Eio))?;
        // Match the existing concurrent read contract: metadata freshness
        // surrounds block I/O. Backing verification belongs to open and
        // publication; actual block reads use the existing provider path.
        // Avoid adding a block-authority transaction at each freshness check.
        let known = {
            let state = self.lock_state()?;
            if state.orphans.contains_key(&inode) {
                return Ok(());
            }
            if state.selected_inodes.contains_key(&inode) {
                state
                    .inode_revisions
                    .get(&inode)
                    .map(|revision| InodeVersion {
                        structural_generation: state.persisted_revision,
                        inode_revision: *revision,
                    })
            } else {
                None
            }
        };
        let loaded = match self
            .inner
            .metadata
            .load_inode_if_changed(backing, inode, known)
            .await
        {
            Ok(loaded) => loaded,
            Err(error) if error.code == ErrorCode::Estale || error.code == ErrorCode::Enoent => {
                self.refresh_concurrent_namespace().await?;
                if self.lock_state()?.orphans.contains_key(&inode) {
                    return Ok(());
                }
                return Err(error);
            }
            Err(error) => return Err(self.fail_closed(error)),
        };
        self.check_inode_runtime()?;
        if let Some(loaded) = loaded {
            if loaded.version.structural_generation != self.lock_state()?.persisted_revision {
                self.refresh_concurrent_namespace().await?;
                // The structural snapshot may have crossed another inode write.
                return Box::pin(self.refresh_selected_inode(inode)).await;
            }
            let mut state = self.lock_state()?;
            if state.inode_revisions.get(&inode) != Some(&loaded.version.inode_revision) {
                state.revision = state
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
            }
            state
                .inode_revisions
                .insert(inode, loaded.version.inode_revision);
            state.selected_inodes.insert(inode, Arc::new(loaded.node));
        }
        Ok(())
    }

    async fn publish_inode_structure(
        &self,
        generation: u64,
        namespace: Namespace,
        blocks_flushed: bool,
    ) -> Result<u64> {
        self.check_inode_runtime()?;
        namespace.validate()?;
        let (generation, revisions) = {
            let state = self.lock_state()?;
            if state.revision != generation {
                return Err(FsError::new(ErrorCode::Eagain));
            }
            (state.persisted_revision, state.inode_revisions.clone())
        };
        if !blocks_flushed {
            self.inner.blocks.flush().await?;
        }
        let backing = self
            .inner
            .concurrent_backing
            .ok_or_else(|| FsError::new(ErrorCode::Eio))?;
        self.inner
            .blocks
            .verify_concurrent_backing(backing)
            .await
            .map_err(|error| self.fail_closed(error))?;
        let mut publication = PublicationGuard::new(&self.inner.state);
        let next = match self
            .inner
            .metadata
            .publish_structure_if_versions(backing, generation, &revisions, namespace.clone())
            .await
        {
            Ok(next) => next,
            Err(error) if error.code == ErrorCode::Eagain => {
                publication.disarm();
                return Err(error);
            }
            Err(error) => return Err(self.fail_closed(error)),
        };
        if !self.inner.metadata.publish_includes_flush_barrier() {
            self.inner
                .metadata
                .flush()
                .await
                .map_err(|error| self.fail_closed(error))?;
        }
        self.check_inode_runtime()?;
        {
            let mut state = self.lock_state()?;
            retain_open_detached(&mut state, &namespace);
            state.namespace = Arc::new(namespace);
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
            state.persisted_revision = next;
            state.inode_revisions.clear();
            state.selected_inodes.clear();
        }
        publication.disarm();
        // Provider records may retain or reset revisions at structural publication.
        self.refresh_concurrent_namespace().await?;
        Ok(next)
    }

    async fn write_selected_inode(
        &self,
        inode: InodeId,
        path: &str,
        buffer: &[u8],
        position: u64,
        append: bool,
    ) -> Result<(usize, u64)> {
        for attempt in 0..MAX_CONCURRENT_CAS_RETRIES {
            let captured = {
                let _gate = self.inner.gate.lock().await;
                self.refresh_selected_inode(inode).await?;
                let state = self.lock_state()?;
                if state.orphans.contains_key(&inode) {
                    None
                } else {
                    let node = state
                        .selected_inodes
                        .get(&inode)
                        .cloned()
                        .ok_or_else(|| error_with_path(ErrorCode::Estale, "write", path))?;
                    let revision = *state
                        .inode_revisions
                        .get(&inode)
                        .ok_or_else(|| FsError::new(ErrorCode::Eio))?;
                    Some((
                        node,
                        InodeVersion {
                            structural_generation: state.persisted_revision,
                            inode_revision: revision,
                        },
                    ))
                }
            };
            let Some((original, expected)) = captured else {
                return self
                    .write_at_serial(inode, path, buffer, position, append)
                    .await;
            };
            let layout = match &original.data {
                NodeData::File(layout) => layout,
                NodeData::Directory { .. } => {
                    return Err(error_with_path(ErrorCode::Eisdir, "write", path));
                }
                NodeData::Special => return Err(error_with_path(ErrorCode::Enxio, "write", path)),
                NodeData::Symlink { .. } => {
                    return Err(error_with_path(ErrorCode::Eio, "write", path));
                }
            };
            let start = if append {
                original.stats.size
            } else {
                position
            };
            let length = u64::try_from(buffer.len())
                .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
            let end = start
                .checked_add(length)
                .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
            if buffer.is_empty() {
                return Ok((0, start));
            }
            let size = original.stats.size.max(end);
            let layout = rewrite_layout(
                &self.inner.blocks,
                layout,
                original.stats.size,
                start,
                buffer,
                size,
                path,
            )
            .await?;
            self.flush_mutation_blocks().await?;
            let mut node = (*original).clone();
            node.data = NodeData::File(layout);
            set_file_size(&mut node.stats, size);
            touch_modified(&mut node.stats, true)?;
            let _gate = self.inner.gate.lock().await;
            self.check_inode_runtime()?;
            let backing = self
                .inner
                .concurrent_backing
                .ok_or_else(|| FsError::new(ErrorCode::Eio))?;
            self.inner
                .blocks
                .verify_concurrent_backing(backing)
                .await
                .map_err(|error| self.fail_closed(error))?;
            let mut publication = PublicationGuard::new(&self.inner.state);
            let version = match self
                .inner
                .metadata
                .publish_inode_if_version(backing, inode, expected, node.clone())
                .await
            {
                Ok(version) => version,
                Err(error) if error.code == ErrorCode::Eagain => {
                    publication.disarm();
                    drop(_gate);
                    concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                    continue;
                }
                Err(error) => return Err(self.fail_closed(error)),
            };
            if !self.inner.metadata.publish_includes_flush_barrier() {
                self.inner
                    .metadata
                    .flush()
                    .await
                    .map_err(|error| self.fail_closed(error))?;
            }
            self.check_inode_runtime()?;
            {
                let mut state = self.lock_state()?;
                state.revision = state
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                state.inode_revisions.insert(inode, version.inode_revision);
                state.selected_inodes.insert(inode, Arc::new(node));
            }
            publication.disarm();
            return Ok((buffer.len(), end));
        }
        Err(FsError::new(ErrorCode::Eagain)
            .with_message("another writer repeatedly changed the inode"))
    }

    /// Caller holds the operation gate and has flushed immutable blocks.
    async fn publish_selected_node(
        &self,
        inode: InodeId,
        expected: InodeVersion,
        node: NodeMetadata,
    ) -> Result<()> {
        self.check_inode_runtime()?;
        let backing = self
            .inner
            .concurrent_backing
            .ok_or_else(|| FsError::new(ErrorCode::Eio))?;
        self.inner
            .blocks
            .verify_concurrent_backing(backing)
            .await
            .map_err(|error| self.fail_closed(error))?;
        let mut publication = PublicationGuard::new(&self.inner.state);
        let version = match self
            .inner
            .metadata
            .publish_inode_if_version(backing, inode, expected, node.clone())
            .await
        {
            Ok(version) => version,
            Err(error) if error.code == ErrorCode::Eagain => {
                publication.disarm();
                return Err(error);
            }
            Err(error) => return Err(self.fail_closed(error)),
        };
        if !self.inner.metadata.publish_includes_flush_barrier() {
            self.inner
                .metadata
                .flush()
                .await
                .map_err(|error| self.fail_closed(error))?;
        }
        self.check_inode_runtime()?;
        {
            let mut state = self.lock_state()?;
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
            state.inode_revisions.insert(inode, version.inode_revision);
            state.selected_inodes.insert(inode, Arc::new(node));
        }
        publication.disarm();
        Ok(())
    }

    async fn refresh_inode_structure(&self) -> Result<()> {
        self.check_inode_runtime()?;
        let mode = self
            .inner
            .metadata
            .inode_mode_state()
            .await
            .map_err(|error| self.fail_closed(error))?
            .ok_or_else(|| self.fail_closed(FsError::new(ErrorCode::Estale)))?;
        self.check_inode_runtime()?;
        if Some(mode.backing) != self.inner.concurrent_backing {
            return Err(self.fail_closed(FsError::new(ErrorCode::Estale)));
        }
        if mode.structural_generation != self.lock_state()?.persisted_revision {
            self.refresh_concurrent_namespace().await?;
        }
        Ok(())
    }

    /// Returns false for a missing path so creation can use a structural transaction.
    async fn replace_selected_file(&self, path: &str, data: &[u8]) -> Result<bool> {
        let size = u64::try_from(data.len())
            .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
        for attempt in 0..MAX_CONCURRENT_CAS_RETRIES {
            let captured = {
                let _gate = self.inner.gate.lock().await;
                self.refresh_inode_structure().await?;
                let (namespace, generation) = {
                    let state = self.lock_state()?;
                    (Arc::clone(&state.namespace), state.persisted_revision)
                };
                let entry = walk(&namespace, path, true, "open", 0)?;
                let Some(inode) = entry.node else {
                    return Ok(false);
                };
                match self.refresh_selected_inode(inode).await {
                    Ok(()) => {}
                    Err(error)
                        if error.code == ErrorCode::Estale || error.code == ErrorCode::Enoent =>
                    {
                        if self.lock_state()?.persisted_revision != generation {
                            continue;
                        }
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                }
                let state = self.lock_state()?;
                // A rename, unlink or recreation changes path meaning. Resolve
                // again against the structural winner before preparing bytes.
                if state.persisted_revision != generation {
                    None
                } else {
                    let node = state
                        .selected_inodes
                        .get(&inode)
                        .cloned()
                        .ok_or_else(|| error_with_path(ErrorCode::Estale, "write", path))?;
                    let revision = *state
                        .inode_revisions
                        .get(&inode)
                        .ok_or_else(|| FsError::new(ErrorCode::Eio))?;
                    Some((
                        inode,
                        node,
                        InodeVersion {
                            structural_generation: generation,
                            inode_revision: revision,
                        },
                    ))
                }
            };
            let Some((inode, original, expected)) = captured else {
                continue;
            };
            let chunker = match &original.data {
                NodeData::File(layout) => layout.chunker.clone(),
                NodeData::Directory { .. } => {
                    return Err(error_with_path(ErrorCode::Eisdir, "write", path));
                }
                NodeData::Special => return Err(error_with_path(ErrorCode::Enxio, "write", path)),
                NodeData::Symlink { .. } => {
                    return Err(error_with_path(ErrorCode::Eio, "write", path));
                }
            };
            let empty = FileLayout {
                chunker,
                extents: Vec::new(),
            };
            let layout = if data.is_empty() {
                empty
            } else {
                rewrite_layout(&self.inner.blocks, &empty, 0, 0, data, size, path).await?
            };
            self.inner.blocks.flush().await?;
            let mut node = (*original).clone();
            node.data = NodeData::File(layout);
            set_file_size(&mut node.stats, size);
            touch_modified(&mut node.stats, true)?;
            let _gate = self.inner.gate.lock().await;
            match self.publish_selected_node(inode, expected, node).await {
                Ok(()) => return Ok(true),
                Err(error) if error.code == ErrorCode::Eagain => {
                    drop(_gate);
                    concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(FsError::new(ErrorCode::Eagain)
            .with_message("another writer repeatedly changed the path or inode"))
    }

    async fn truncate_selected_inode(
        &self,
        selected: Option<InodeId>,
        path: &str,
        length: u64,
    ) -> Result<()> {
        let syscall = if selected.is_some() {
            "ftruncate"
        } else {
            "truncate"
        };
        for attempt in 0..MAX_CONCURRENT_CAS_RETRIES {
            let _gate = self.inner.gate.lock().await;
            let inode = if let Some(inode) = selected {
                inode
            } else {
                self.refresh_inode_structure().await?;
                let namespace = Arc::clone(&self.lock_state()?.namespace);
                resolve(&namespace, path, true, "truncate")?
            };
            let generation = self.lock_state()?.persisted_revision;
            match self.refresh_selected_inode(inode).await {
                Ok(()) => {}
                Err(error)
                    if selected.is_none()
                        && (error.code == ErrorCode::Estale || error.code == ErrorCode::Enoent) =>
                {
                    if self.lock_state()?.persisted_revision != generation {
                        continue;
                    }
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
            if selected.is_none() && self.lock_state()?.persisted_revision != generation {
                continue;
            }
            let (mut node, expected) = {
                let state = self.lock_state()?;
                if let Some(node) = state.orphans.get(&inode) {
                    (node.clone(), None)
                } else {
                    let node = state
                        .selected_inodes
                        .get(&inode)
                        .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, path))?;
                    let revision = *state
                        .inode_revisions
                        .get(&inode)
                        .ok_or_else(|| FsError::new(ErrorCode::Eio))?;
                    (
                        (**node).clone(),
                        Some(InodeVersion {
                            structural_generation: state.persisted_revision,
                            inode_revision: revision,
                        }),
                    )
                }
            };
            let layout = match &mut node.data {
                NodeData::File(layout) => layout,
                NodeData::Directory { .. } => {
                    return Err(error_with_path(ErrorCode::Eisdir, syscall, path));
                }
                NodeData::Special => {
                    return Err(error_with_path(
                        if selected.is_some() {
                            ErrorCode::Enxio
                        } else {
                            ErrorCode::Einval
                        },
                        syscall,
                        path,
                    ));
                }
                NodeData::Symlink { .. } => {
                    return Err(error_with_path(ErrorCode::Eio, syscall, path));
                }
            };
            if length < node.stats.size {
                trim_extents(&mut layout.extents, length)?;
            }
            set_file_size(&mut node.stats, length);
            touch_modified(&mut node.stats, true)?;
            let Some(expected) = expected else {
                self.lock_state()?.orphans.insert(inode, node);
                return Ok(());
            };
            self.inner.blocks.flush().await?;
            match self.publish_selected_node(inode, expected, node).await {
                Ok(()) => return Ok(()),
                Err(error) if error.code == ErrorCode::Eagain => {
                    drop(_gate);
                    concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(FsError::new(ErrorCode::Eagain)
            .with_message("another writer repeatedly changed the inode"))
    }

    async fn refresh_concurrent_namespace(&self) -> Result<()> {
        let _profile = Span::new(Event::Refresh);
        if self.inner.options.inode_updates {
            self.check_inode_runtime()?;
            let backing = self
                .inner
                .concurrent_backing
                .ok_or_else(|| FsError::new(ErrorCode::Eio))?;
            self.inner
                .blocks
                .verify_concurrent_backing(backing)
                .await
                .map_err(|error| self.fail_closed(error))?;
            let snapshot = self
                .inner
                .metadata
                .load_inode_snapshot(backing)
                .await
                .map_err(|error| self.fail_closed(error))?;
            snapshot
                .validate()
                .map_err(|error| self.fail_closed(error))?;
            self.check_inode_runtime()?;
            let mut state = self.lock_state()?;
            if snapshot.structural_generation >= state.persisted_revision {
                retain_open_detached(&mut state, &snapshot.namespace);
                state.namespace = Arc::new(snapshot.namespace);
                if state.persisted_revision != snapshot.structural_generation
                    || state.inode_revisions != snapshot.inode_revisions
                    || !state.selected_inodes.is_empty()
                {
                    state.revision = state
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                }
                state.persisted_revision = snapshot.structural_generation;
                state.inode_revisions = snapshot.inode_revisions;
                state.selected_inodes.clear();
            }
            return Ok(());
        }
        if self.inner.options.delegated {
            self.refresh_delegation().await?;
        }
        let known_revision = {
            let state = self.lock_state()?;
            if let Some(error) = &state.failure {
                return Err(error.clone());
            }
            if state.closed {
                return Err(FsError::new(ErrorCode::Ebadf).with_message("filesystem is closed"));
            }
            state.revision
        };
        let loaded = self
            .inner
            .metadata
            .load_if_changed(known_revision)
            .await
            .map_err(|error| self.fail_closed(with_context(error, "metadata-load", None)))?;
        let changed = match loaded {
            Some(loaded) => {
                loaded.validate().map_err(|error| {
                    self.fail_closed(with_context(error, "metadata-validate", None))
                })?;
                let namespace = loaded.namespace.ok_or_else(|| {
                    self.fail_closed(FsError::backend(
                        "concurrent metadata revision has no published namespace",
                    ))
                })?;
                {
                    profile::add(Event::Changed, namespace.nodes.len() as u64);
                    Some((namespace, loaded.revision))
                }
            }
            None => None,
        };
        let mut state = self.lock_state()?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.closed {
            return Err(FsError::new(ErrorCode::Ebadf).with_message("filesystem is closed"));
        }
        if let Some((namespace, revision)) = changed
            && revision > state.revision
        {
            // Remote rmdir can remove a directory inode entirely. A local
            // handle already opened on that inode must retain its metadata
            // until close, even though path lookup sees the new namespace.
            retain_open_detached(&mut state, &namespace);
            state.namespace = Arc::new(namespace);
            state.revision = revision;
            state.persisted_revision = revision;
            state.pending_atime.clear();
        }
        Ok(())
    }

    async fn renew_lease(&self) -> Result<WriterLease> {
        let _lease_gate = self.inner.lease_gate.lock().await;
        self.renew_lease_inner(false).await
    }

    async fn validate_lease(&self) -> Result<()> {
        if self.inner.options.delegated && self.inner.delegation_draining.load(Ordering::SeqCst) {
            return Err(
                FsError::new(ErrorCode::Ebusy).with_message("directory checkin is draining")
            );
        }
        if self.inner.options.concurrent_writes {
            return self.refresh_concurrent_namespace().await;
        }
        let _lease_gate = self.inner.lease_gate.lock().await;
        self.renew_lease_inner(true).await.map(|_| ())
    }

    async fn renew_lease_inner(&self, force: bool) -> Result<WriterLease> {
        let current = self
            .lock_lease()?
            .clone()
            .ok_or_else(|| FsError::new(ErrorCode::Ebadf).with_message("writer lease released"))?;
        // A provider renewal is a serialized metadata transaction. Avoid
        // paying that round trip for every read and optimistic block
        // operation while the cached lease still has a conservative safety
        // window. Publication still carries the provider-enforced owner,
        // fence and expiry predicates, so a clock-skewed or fenced lease
        // fails closed at the durable boundary.
        let margin = self.inner.options.lease_ttl.min(LEASE_RENEWAL_MARGIN);
        let margin_ms = u64::try_from(margin.as_millis()).unwrap_or(u64::MAX);
        let now = u64::try_from(now_ms()).unwrap_or(0);
        if !force
            && self.inner.lease_renewed.load(Ordering::Acquire)
            && current.expires_at_ms > now.saturating_add(margin_ms)
        {
            return Ok(current);
        }
        let renewed = match self
            .inner
            .metadata
            .renew_writer(&current, self.inner.options.lease_ttl)
            .await
        {
            Ok(renewed) => renewed,
            Err(error) if error.code == ErrorCode::Estale => {
                return self.reacquire_expired_lease(current).await;
            }
            Err(error) => {
                return Err(self.fail_closed(with_context(error, "lease-renew", None)));
            }
        };
        *self.lock_lease()? = Some(renewed.clone());
        self.inner.lease_renewed.store(true, Ordering::Release);
        Ok(renewed)
    }

    /// Recover only an expired lease that no other writer has fenced past.
    /// The provider's revision and fencing token are the recovery boundary:
    /// if either changed, this instance cannot safely continue using its
    /// in-memory namespace and is failed closed.
    async fn reacquire_expired_lease(&self, current: WriterLease) -> Result<WriterLease> {
        let acquired = match self
            .inner
            .metadata
            .acquire_writer(&self.inner.options.owner, self.inner.options.lease_ttl)
            .await
        {
            Ok(acquired) => acquired,
            Err(error) => {
                let error = if error.code == ErrorCode::Eagain {
                    FsError::new(ErrorCode::Estale)
                        .with_syscall("lease-acquire")
                        .with_message(error.to_string())
                } else {
                    with_context(error, "lease-acquire", None)
                };
                return Err(self.fail_closed(error));
            }
        };

        let fence_decision = classify_reacquired_fence(current.fence, acquired.fence);
        if fence_decision == ReacquiredFence::Overflow {
            let _ = self.inner.metadata.release_writer(&acquired).await;
            return Err(self.fail_closed(
                FsError::new(ErrorCode::Eoverflow)
                    .with_syscall("lease-acquire")
                    .with_message("writer lease fence overflow"),
            ));
        }
        if fence_decision != ReacquiredFence::Next || acquired.owner != self.inner.options.owner {
            let _ = self.inner.metadata.release_writer(&acquired).await;
            return Err(self.fail_closed(
                FsError::new(ErrorCode::Estale)
                    .with_syscall("lease-acquire")
                    .with_message("writer lease was fenced by another owner"),
            ));
        }

        let loaded = match self.inner.metadata.load().await {
            Ok(loaded) => loaded,
            Err(error) => {
                let _ = self.inner.metadata.release_writer(&acquired).await;
                return Err(self.fail_closed(with_context(error, "metadata-load", None)));
            }
        };
        if let Err(error) = loaded.validate() {
            let _ = self.inner.metadata.release_writer(&acquired).await;
            return Err(self.fail_closed(with_context(error, "metadata-validate", None)));
        }
        let state_result = match self.lock_state() {
            Ok(state) => {
                if let Some(error) = &state.failure {
                    Err(error.clone())
                } else if state.closed {
                    Err(FsError::new(ErrorCode::Ebadf)
                        .with_syscall("lease-acquire")
                        .with_message("filesystem is closed"))
                } else {
                    Ok(state.persisted_revision)
                }
            }
            Err(error) => Err(error),
        };
        let local_revision = match state_result {
            Ok(revision) => revision,
            Err(error) => {
                let _ = self.inner.metadata.release_writer(&acquired).await;
                return Err(self.fail_closed(error));
            }
        };
        if loaded.revision != local_revision {
            let _ = self.inner.metadata.release_writer(&acquired).await;
            return Err(self.fail_closed(
                FsError::new(ErrorCode::Eagain)
                    .with_syscall("lease-acquire")
                    .with_message("metadata changed while the writer lease was expired"),
            ));
        }
        if let Ok(mut lease) = self.lock_lease() {
            *lease = Some(acquired.clone());
            self.inner.lease_renewed.store(true, Ordering::Release);
        } else {
            let error = FsError::new(ErrorCode::Eio).with_message("lease lock poisoned");
            let _ = self.inner.metadata.release_writer(&acquired).await;
            return Err(self.fail_closed(error));
        }
        Ok(acquired)
    }

    async fn ensure_operation_lease(&self) -> Result<()> {
        if self.inner.options.delegated && self.inner.delegation_draining.load(Ordering::SeqCst) {
            return Err(
                FsError::new(ErrorCode::Ebusy).with_message("directory checkin is draining")
            );
        }
        if self.inner.options.concurrent_writes {
            return self.refresh_concurrent_namespace().await;
        }
        self.renew_lease().await.map(|_| ())
    }

    async fn flush_mutation_blocks(&self) -> Result<()> {
        if self.inner.options.writeback {
            return Ok(());
        }
        self.inner.blocks.flush().await
    }

    async fn publish_namespace(
        &self,
        expected_revision: u64,
        namespace: Namespace,
        blocks_flushed: bool,
    ) -> Result<u64> {
        if !self.inner.options.writeback {
            return self
                .publish_namespace_durable(expected_revision, namespace, blocks_flushed)
                .await;
        }
        let mut state = self.lock_state()?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.closed {
            return Err(FsError::new(ErrorCode::Ebadf));
        }
        if expected_revision != state.revision {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let generation = state.revision.checked_add(1).ok_or_else(|| {
            FsError::new(ErrorCode::Eio).with_message("local namespace generation overflow")
        })?;
        retain_open_detached(&mut state, &namespace);
        state.namespace = Arc::new(namespace);
        state.revision = generation;
        state.pending_namespace = true;
        state.pending_atime.clear();
        Ok(generation)
    }

    async fn drain_writeback(&self) -> Result<bool> {
        if !self.inner.options.writeback {
            return Ok(false);
        }
        let (namespace, generation) = self.snapshot()?;
        let persisted = {
            let state = self.lock_state()?;
            if !state.pending_namespace && state.pending_atime.is_empty() {
                return Ok(false);
            }
            state.persisted_revision
        };
        // Arm before block I/O: canceling a required barrier makes this owner unusable.
        let mut barrier = PublicationGuard::new(&self.inner.state);
        // Immutable block flushing does not publish references or require
        // writer authority. The durable path forces a provider lease check
        // after flushing, immediately before its fenced metadata publication.
        self.publish_namespace_durable(persisted, namespace, false)
            .await
            .map_err(|error| self.fail_closed(error))?;
        let mut state = self.lock_state()?;
        state.revision = generation;
        state.pending_namespace = false;
        barrier.disarm();
        Ok(true)
    }

    async fn publish_namespace_durable(
        &self,
        expected_revision: u64,
        namespace: Namespace,
        blocks_flushed: bool,
    ) -> Result<u64> {
        if self.inner.options.inode_updates {
            return self
                .publish_inode_structure(expected_revision, namespace, blocks_flushed)
                .await;
        }
        if self.inner.options.delegated {
            let grant = self
                .local_grant()?
                .ok_or_else(|| FsError::new(ErrorCode::Eacces).with_message("checkout required"))?;
            let mut authority = self
                .inner
                .metadata
                .delegation_state()
                .await?
                .ok_or_else(|| self.fail_closed(FsError::new(ErrorCode::Estale)))?;
            if authority
                .grants
                .get(&grant.token.root)
                .is_none_or(|actual| actual.token != grant.token)
            {
                return Err(self.fail_closed(FsError::new(ErrorCode::Estale)));
            }
            // Orphan provenance changes with publication revision. Verify that the
            // authority fetched above still corresponds to this candidate's revision,
            // including publications by disjoint owners not yet loaded locally.
            if let Some(latest) = self
                .inner
                .metadata
                .load_if_changed(expected_revision)
                .await?
            {
                latest.validate()?;
                if latest.revision != expected_revision {
                    return Err(FsError::new(ErrorCode::Eagain));
                }
            }
            let (old, local_revision) = self.snapshot()?;
            if local_revision != expected_revision {
                // A concurrent discovery refresh advanced local state while immutable I/O was pending.
                // Rebase the operation rather than classify a stale candidate as an outside edit.
                return Err(FsError::new(ErrorCode::Eagain));
            }
            // Refuse invalid driver deltas before publication; the provider repeats this check atomically.
            authority
                .authorize_publish(&old, &namespace, &grant.token)
                .map_err(|error| {
                    mount_rs_core::diagnostics::trace_failure(
                        "chunked",
                        "delegated_delta_denied",
                        &error,
                    );
                    FsError::new(ErrorCode::Eacces)
                        .with_message(format!("delegated namespace change denied: {error}"))
                })?;
        }
        let mut trace = RequestTrace::new("chunked", "publish_namespace");
        trace.stage(
            "publication",
            format_args!(
                "expected_revision={expected_revision} concurrent={} blocks_flushed={blocks_flushed}",
                self.inner.options.concurrent_writes
            ),
        );
        if !blocks_flushed {
            trace.stage("block_flush_start", format_args!(""));
            let result = self.inner.blocks.flush().await;
            trace.stage(
                "block_flush_end",
                format_args!(
                    "outcome={}",
                    result
                        .as_ref()
                        .map_or_else(|error| error.code.as_str(), |_| "ok")
                ),
            );
            result.map_err(|error| with_context(error, "block-flush", None))?;
        }
        if self.inner.options.concurrent_writes {
            let backing = self.inner.concurrent_backing.ok_or_else(|| {
                self.fail_closed(
                    FsError::new(ErrorCode::Eio)
                        .with_syscall("publish bound concurrent metadata")
                        .with_message("concurrent backing ID is missing from runtime state"),
                )
            })?;
            trace.stage(
                "backing_verify_start",
                format_args!("expected_revision={expected_revision}"),
            );
            let result = self.inner.blocks.verify_concurrent_backing(backing).await;
            trace.stage(
                "backing_verify_end",
                format_args!(
                    "expected_revision={expected_revision} outcome={}",
                    result
                        .as_ref()
                        .map_or_else(|error| error.code.as_str(), |_| "ok")
                ),
            );
            result
                .map_err(|error| self.fail_closed(with_context(error, "backing-verify", None)))?;
            let mut publication = PublicationGuard::new(&self.inner.state);
            trace.stage(
                "cas_start",
                format_args!("expected_revision={expected_revision}"),
            );
            let result = if self.inner.options.delegated {
                let grant = self.local_grant()?.ok_or_else(|| {
                    FsError::new(ErrorCode::Eacces).with_message("checkout required")
                })?;
                self.inner
                    .metadata
                    .publish_delegated(
                        &DelegatedPublish {
                            backing,
                            token: grant.token,
                            expected_revision,
                        },
                        namespace.clone(),
                    )
                    .await
            } else {
                self.inner
                    .metadata
                    .publish_bound_if_revision(backing, expected_revision, namespace.clone())
                    .await
            };
            trace.stage(
                "cas_end",
                format_args!(
                    "expected_revision={expected_revision} outcome={} revision={:?}",
                    result
                        .as_ref()
                        .map_or_else(|error| error.code.as_str(), |_| "ok"),
                    result.as_ref().ok()
                ),
            );
            let revision = match result {
                Ok(revision) => revision,
                Err(error) if error.code == ErrorCode::Eagain => {
                    profile::add(Event::PublishConflict, 0);
                    // This is a known non-commit. The caller may reload and
                    // reconstruct its original operation; local state stays
                    // unchanged until a successful acknowledgement.
                    publication.disarm();
                    return Err(with_context(error, "metadata-publish", None));
                }
                Err(error) => {
                    return Err(self.fail_closed(with_context(error, "metadata-publish", None)));
                }
            };
            if !self.inner.metadata.publish_includes_flush_barrier()
                && let Err(error) = self.inner.metadata.flush().await
            {
                return Err(self.fail_closed(with_context(error, "metadata-flush", None)));
            }
            let mut state = self.lock_state()?;
            if state.closed {
                drop(state);
                return Err(self.fail_closed(
                    FsError::new(ErrorCode::Ebadf)
                        .with_message("filesystem closed during concurrent publication"),
                ));
            }
            if revision > state.revision {
                // Only a successful CAS may move detached local nodes into
                // private handle state. A retryable conflict has no local
                // orphan side effect.
                retain_open_detached(&mut state, &namespace);
                state.namespace = Arc::new(namespace);
                state.revision = revision;
                state.persisted_revision = revision;
                state.pending_atime.clear();
            }
            publication.disarm();
            drop(state);
            trace.finish(format_args!("revision={revision}"));
            return Ok(revision);
        }
        if self.inner.options.writeback {
            self.validate_lease().await?;
        }
        let lease = self.renew_lease().await?;
        let mut publication = PublicationGuard::new(&self.inner.state);
        let revision = match self
            .inner
            .metadata
            .publish(expected_revision, &lease, namespace.clone())
            .await
        {
            Ok(revision) => revision,
            Err(error) => {
                return Err(self.fail_closed(with_context(error, "metadata-publish", None)));
            }
        };
        if !self.inner.metadata.publish_includes_flush_barrier()
            && let Err(error) = self.inner.metadata.flush().await
        {
            return Err(self.fail_closed(with_context(error, "metadata-flush", None)));
        }
        let mut state = self.lock_state()?;
        state.namespace = Arc::new(namespace);
        if !self.inner.options.writeback {
            state.revision = revision;
        }
        state.persisted_revision = revision;
        state.pending_atime.clear();
        publication.disarm();
        drop(state);
        trace.finish(format_args!("revision={revision}"));
        Ok(revision)
    }

    /// Publish coalesced read-atime changes while the caller owns the
    /// operation gate. Ordinary mutation paths call `snapshot`, so pending
    /// values are included in any later namespace publication too.
    async fn flush_pending_atime(&self) -> Result<()> {
        if self.inner.options.concurrent_writes {
            return Ok(());
        }
        let has_pending = !self.lock_state()?.pending_atime.is_empty();
        if !has_pending {
            return Ok(());
        }
        self.ensure_operation_lease().await?;
        let (namespace, revision) = self.snapshot()?;
        self.publish_namespace(revision, namespace, false)
            .await
            .map(|_| ())
    }

    async fn submit_whole_file_mutation(
        &self,
        mutation: WholeFileMutation,
    ) -> Result<WholeFileMutationResult> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let committed = Arc::new(AtomicBool::new(false));
        let mut acknowledgement =
            MutationAcknowledgementGuard::new(&self.inner.state, Arc::clone(&committed));
        self.enqueue_mutation(MutationRequest::WholeFile {
            mutation: Box::new(mutation),
            reply,
            committed,
        })
        .await?;
        match response
            .await
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("mutation batch stopped"))??
        {
            MutationResult::WholeFile(result) => {
                acknowledgement.disarm();
                Ok(result)
            }
            MutationResult::Unit => Err(FsError::new(ErrorCode::Eio)
                .with_syscall("write")
                .with_message("mutation batch returned the wrong result type")),
        }
    }

    async fn submit_unlink_mutation(&self, path: String) -> Result<()> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let committed = Arc::new(AtomicBool::new(false));
        let mut acknowledgement =
            MutationAcknowledgementGuard::new(&self.inner.state, Arc::clone(&committed));
        self.enqueue_mutation(MutationRequest::Unlink {
            path,
            reply,
            committed,
        })
        .await?;
        match response
            .await
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("mutation batch stopped"))??
        {
            MutationResult::Unit => {
                acknowledgement.disarm();
                Ok(())
            }
            MutationResult::WholeFile(_) => Err(FsError::new(ErrorCode::Eio)
                .with_syscall("unlink")
                .with_message("mutation batch returned the wrong result type")),
        }
    }

    fn begin_mutation_preparation(&self) -> MutationPreparationGuard {
        self.inner
            .preparing_mutations
            .fetch_add(1, Ordering::AcqRel);
        MutationPreparationGuard {
            active: true,
            counter: Arc::clone(&self.inner.preparing_mutations),
        }
    }

    async fn enqueue_mutation(&self, request: MutationRequest) -> Result<()> {
        let run = {
            let mut queue = self.inner.mutations.lock().map_err(|_| {
                FsError::new(ErrorCode::Eio).with_message("mutation queue poisoned")
            })?;
            if queue.pending.len() >= MAX_PENDING_MUTATIONS {
                return Err(
                    FsError::new(ErrorCode::Eagain).with_message("mutation queue is at capacity")
                );
            }
            queue.pending.push(request);
            if queue.running {
                false
            } else {
                queue.running = true;
                true
            }
        };
        if run {
            self.run_mutation_batches().await;
        }
        Ok(())
    }

    async fn run_mutation_batches(&self) {
        let mut runner = MutationRunnerGuard::new(&self.inner.mutations);
        loop {
            // Let other operations finish their immutable block work and
            // enqueue their prepared metadata mutations before this runner
            // snapshots the namespace. The bounded adaptive window improves
            // coalescing for remote providers without turning publication
            // into a timer or changing the fenced revision/CAS boundary.
            let mut previous_pending = 0;
            let mut idle_rounds = 0;
            for round in 0..MUTATION_BATCH_MAX_YIELD_ROUNDS {
                cooperative_yield().await;
                let pending = match self.inner.mutations.lock() {
                    Ok(queue) => queue.pending.len(),
                    Err(_) => break,
                };
                if pending >= MUTATION_BATCH_REQUEST_TARGET {
                    break;
                }
                if pending == previous_pending {
                    idle_rounds += 1;
                } else {
                    idle_rounds = 0;
                }
                previous_pending = pending;
                if round + 1 >= MUTATION_BATCH_INITIAL_YIELD_ROUNDS
                    && idle_rounds >= MUTATION_BATCH_IDLE_YIELD_ROUNDS
                    && self.inner.preparing_mutations.load(Ordering::Acquire) == 0
                {
                    break;
                }
            }

            let requests = {
                let mut queue = match self.inner.mutations.lock() {
                    Ok(queue) => queue,
                    Err(_) => {
                        runner.finish();
                        return;
                    }
                };
                if queue.pending.is_empty() {
                    queue.running = false;
                    runner.finish();
                    return;
                }
                std::mem::take(&mut queue.pending)
            };
            self.apply_mutation_batch(requests).await;
        }
    }

    async fn apply_mutation_batch(&self, requests: Vec<MutationRequest>) {
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        let attempts = if self.inner.options.concurrent_writes {
            MAX_CONCURRENT_CAS_RETRIES
        } else {
            1
        };
        for attempt in 0..attempts {
            let prepared = self
                .ensure_operation_lease()
                .await
                .and_then(|_| self.snapshot());
            let (mut namespace, revision) = match prepared {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    for request in requests {
                        mutation_reply(request, Err(error.clone()));
                    }
                    return;
                }
            };
            let mut changed = false;
            let mut responses = Vec::with_capacity(requests.len());

            for request in &requests {
                match request {
                    MutationRequest::WholeFile {
                        mutation, reply, ..
                    } => {
                        if reply.is_closed() {
                            responses.push(None);
                            continue;
                        }
                        let mut mutation = (**mutation).clone();
                        // Same-revision creates can share one batch. A
                        // remote CAS loss invalidates prepared inode/layout
                        // assumptions, so those requests report Conflict
                        // and take the caller's full replay path.
                        if mutation.new_inode && mutation.expected_revision == revision {
                            mutation.inode = namespace.next_inode;
                        }
                        let mut candidate = namespace.clone();
                        let result = apply_whole_file_mutation(
                            &mut candidate,
                            revision,
                            &mutation,
                            self.inner.options.concurrent_writes,
                        );
                        match result {
                            Ok(WholeFileMutationResult::Committed) => {
                                namespace = candidate;
                                changed = true;
                                responses.push(Some(Ok(MutationResult::WholeFile(
                                    WholeFileMutationResult::Committed,
                                ))));
                            }
                            Ok(WholeFileMutationResult::Conflict) => responses.push(Some(Ok(
                                MutationResult::WholeFile(WholeFileMutationResult::Conflict),
                            ))),
                            Err(error) => responses.push(Some(Err(error))),
                        }
                    }
                    MutationRequest::Unlink { path, reply, .. } => {
                        if reply.is_closed() {
                            responses.push(None);
                            continue;
                        }
                        let mut candidate = namespace.clone();
                        match apply_unlink_mutation(self, &mut candidate, path) {
                            Ok(()) => {
                                namespace = candidate;
                                changed = true;
                                responses.push(Some(Ok(MutationResult::Unit)));
                            }
                            Err(error) => responses.push(Some(Err(error))),
                        }
                    }
                }
            }

            if changed {
                match self.publish_namespace(revision, namespace, true).await {
                    Ok(_) => {}
                    Err(error)
                        if self.inner.options.concurrent_writes
                            && error.code == ErrorCode::Eagain
                            && attempt + 1 < attempts =>
                    {
                        // The provider confirmed no commit. Replay every
                        // request against a fresh revision before replying;
                        // only an acknowledged batch may report success.
                        concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                        continue;
                    }
                    Err(error) => {
                        for request in requests {
                            mutation_reply(request, Err(error.clone()));
                        }
                        return;
                    }
                }
            }
            for (request, response) in requests.into_iter().zip(responses) {
                if let Some(response) = response {
                    let committed = matches!(
                        response,
                        Ok(
                            MutationResult::WholeFile(WholeFileMutationResult::Committed)
                                | MutationResult::Unit
                        )
                    );
                    if committed {
                        request.mark_committed();
                    }
                    if !mutation_reply(request, response) && committed {
                        let _ = self.fail_closed(
                            FsError::new(ErrorCode::Eio)
                                .with_syscall("mutation-batch")
                                .with_message(
                                    "committed mutation response was canceled before acknowledgement",
                                ),
                        );
                    }
                }
            }
            return;
        }
    }

    async fn mutate<F, R>(&self, mut operation: F) -> Result<R>
    where
        F: FnMut(&mut Namespace) -> Result<R>,
    {
        let mut trace = RequestTrace::new("chunked", "mutate");
        trace.stage("gate_wait", format_args!(""));
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        trace.stage("gate_acquired", format_args!(""));
        let attempts = if self.inner.options.concurrent_writes {
            MAX_CONCURRENT_CAS_RETRIES
        } else {
            1
        };
        for attempt in 0..attempts {
            trace.stage("metadata_load_start", format_args!("attempt={attempt}"));
            let loaded = self.ensure_operation_lease().await;
            trace.stage(
                "metadata_load_end",
                format_args!(
                    "attempt={attempt} outcome={}",
                    loaded
                        .as_ref()
                        .map_or_else(|error| error.code.as_str(), |_| "ok")
                ),
            );
            loaded?;
            let (mut namespace, revision) = self.snapshot()?;
            let result = operation(&mut namespace)?;
            trace.stage(
                "publish_start",
                format_args!("attempt={attempt} expected_revision={revision}"),
            );
            match self.publish_namespace(revision, namespace, false).await {
                Ok(next_revision) => {
                    trace.finish(format_args!("attempt={attempt} revision={next_revision}"));
                    return Ok(result);
                }
                Err(error)
                    if self.inner.options.concurrent_writes
                        && error.code == ErrorCode::Eagain
                        && attempt + 1 < attempts =>
                {
                    trace.stage(
                        "cas_backoff",
                        format_args!("attempt={attempt} expected_revision={revision}"),
                    );
                    concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                }
                Err(error) => {
                    trace.finish(format_args!(
                        "attempt={attempt} error={}",
                        error.code.as_str()
                    ));
                    return Err(error);
                }
            }
        }
        Err(FsError::new(ErrorCode::Eagain)
            .with_syscall("publish concurrent metadata")
            .with_message("another writer repeatedly changed the namespace"))
    }

    async fn read_at(
        &self,
        inode: InodeId,
        path: &str,
        buffer: &mut [u8],
        position: u64,
    ) -> Result<usize> {
        let _lifecycle = self.inner.lifecycle.read().await;
        let (layout, original, orphan, size, count) = {
            let gate_profile = Span::new(Event::GateWait);
            let _gate = self.inner.gate.lock().await;
            drop(gate_profile);
            if self.inner.options.inode_updates {
                self.refresh_selected_inode(inode).await?;
            } else {
                self.ensure_operation_lease().await?;
            }
            let (layout, original, orphan, size) = if self.inner.options.inode_updates {
                let state = self.lock_state()?;
                if let Some(node) = state.orphans.get(&inode) {
                    let NodeData::File(layout) = &node.data else {
                        return Err(error_with_path(ErrorCode::Eisdir, "read", path));
                    };
                    (
                        ReadLayoutSnapshot::Owned(layout.clone()),
                        None,
                        true,
                        node.stats.size,
                    )
                } else {
                    let node = state
                        .selected_inodes
                        .get(&inode)
                        .cloned()
                        .ok_or_else(|| error_with_path(ErrorCode::Estale, "read", path))?;
                    match &node.data {
                        NodeData::File(_) => {}
                        NodeData::Directory { .. } => {
                            return Err(error_with_path(ErrorCode::Eisdir, "read", path));
                        }
                        NodeData::Special => {
                            return Err(error_with_path(ErrorCode::Enxio, "read", path));
                        }
                        NodeData::Symlink { .. } => {
                            return Err(error_with_path(ErrorCode::Eio, "read", path));
                        }
                    }
                    let size = node.stats.size;
                    (ReadLayoutSnapshot::Selected(node), None, false, size)
                }
            } else if self.inner.options.concurrent_writes && !self.inner.options.delegated {
                // MRC2 reads do not record atime or compare the original node.
                // Retain immutable metadata across block I/O without copying
                // extents. Both freshness checks still surround that I/O.
                let state = self.lock_state()?;
                if let Some(error) = &state.failure {
                    return Err(error.clone());
                }
                if state.closed {
                    return Err(FsError::new(ErrorCode::Ebadf).with_message("filesystem is closed"));
                }
                let (node, orphan) = if let Some(node) = state.namespace.nodes.get(&inode) {
                    (node, false)
                } else {
                    (
                        state
                            .orphans
                            .get(&inode)
                            .ok_or_else(|| error_with_path(ErrorCode::Estale, "read", path))?,
                        true,
                    )
                };
                let layout = match &node.data {
                    NodeData::File(layout) => {
                        if orphan {
                            let _profile = Span::new(Event::Snapshot).units(1);
                            ReadLayoutSnapshot::Owned(layout.clone())
                        } else {
                            ReadLayoutSnapshot::SharedNamespace {
                                namespace: Arc::clone(&state.namespace),
                                inode,
                            }
                        }
                    }
                    NodeData::Directory { .. } => {
                        return Err(error_with_path(ErrorCode::Eisdir, "read", path));
                    }
                    NodeData::Special => {
                        return Err(error_with_path(ErrorCode::Enxio, "read", path));
                    }
                    NodeData::Symlink { .. } => {
                        return Err(error_with_path(ErrorCode::Eio, "read", path));
                    }
                };
                (layout, None, orphan, node.stats.size)
            } else {
                let (namespace, _) = self.snapshot()?;
                let (node, orphan) = self.node_snapshot(&namespace, inode, "read", path)?;
                let layout = match &node.data {
                    NodeData::File(layout) => layout.clone(),
                    NodeData::Directory { .. } => {
                        return Err(error_with_path(ErrorCode::Eisdir, "read", path));
                    }
                    NodeData::Special => {
                        return Err(error_with_path(ErrorCode::Enxio, "read", path));
                    }
                    NodeData::Symlink { .. } => {
                        return Err(error_with_path(ErrorCode::Eio, "read", path));
                    }
                };
                let size = node.stats.size;
                (ReadLayoutSnapshot::Owned(layout), Some(node), orphan, size)
            };
            let count = if position >= size {
                0
            } else {
                let available = size - position;
                buffer
                    .len()
                    .min(usize::try_from(available).unwrap_or(usize::MAX))
            };
            (layout, original, orphan, size, count)
        };
        if count > 0 {
            read_layout_into(
                &self.inner.blocks,
                layout.layout()?,
                size,
                position,
                &mut buffer[..count],
                path,
                "read",
            )
            .await?;
        }
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        if self.inner.options.inode_updates {
            self.refresh_selected_inode(inode).await?;
        } else {
            self.ensure_operation_lease().await?;
        }
        if self.inner.options.concurrent_writes {
            return Ok(count);
        }
        if count == 0 {
            return Ok(0);
        }
        if orphan {
            let mut state = self.lock_state()?;
            if let Some(node) = state.orphans.get_mut(&inode) {
                node.stats.atime_ms = now_ms();
            }
            return Ok(count);
        }
        let (namespace, _) = self.snapshot()?;
        if namespace.nodes.get(&inode).is_some_and(|node| {
            original
                .as_ref()
                .is_some_and(|original| write_base_unchanged(node, original))
        }) {
            let atime_ms = now_ms();
            let mut state = self.lock_state()?;
            state
                .pending_atime
                .entry(inode)
                .and_modify(|pending| *pending = (*pending).max(atime_ms))
                .or_insert(atime_ms);
        } else if let Some(node) = self.lock_state()?.orphans.get_mut(&inode)
            && original
                .as_ref()
                .is_some_and(|original| write_base_unchanged(node, original))
        {
            node.stats.atime_ms = now_ms();
        }
        Ok(count)
    }

    async fn write_at(
        &self,
        inode: InodeId,
        path: &str,
        buffer: &[u8],
        position: u64,
        append: bool,
    ) -> Result<(usize, u64)> {
        let _lifecycle = self.inner.lifecycle.read().await;
        if self.inner.options.inode_updates {
            return self
                .write_selected_inode(inode, path, buffer, position, append)
                .await;
        }
        let (layout, original, orphan, start, end, new_size) = {
            let gate_profile = Span::new(Event::GateWait);
            let _gate = self.inner.gate.lock().await;
            drop(gate_profile);
            self.ensure_operation_lease().await?;
            let (namespace, _) = self.snapshot()?;
            let (node, orphan) = self.node_snapshot(&namespace, inode, "write", path)?;
            let layout = match &node.data {
                NodeData::File(layout) => layout.clone(),
                NodeData::Directory { .. } => {
                    return Err(error_with_path(ErrorCode::Eisdir, "write", path));
                }
                NodeData::Special => return Err(error_with_path(ErrorCode::Enxio, "write", path)),
                NodeData::Symlink { .. } => {
                    return Err(error_with_path(ErrorCode::Eio, "write", path));
                }
            };
            let start = if append { node.stats.size } else { position };
            let input_length = u64::try_from(buffer.len())
                .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
            let end = start
                .checked_add(input_length)
                .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
            if buffer.is_empty() {
                return Ok((0, start));
            }
            let new_size = node.stats.size.max(end);
            (layout, node, orphan, start, end, new_size)
        };

        // Block reads/writes are immutable and can safely overlap while the
        // metadata commit remains serialized below. The captured node is
        // checked again before publication so an intervening truncate,
        // replacement or unlink cannot be lost.
        let new_layout = rewrite_layout(
            &self.inner.blocks,
            &layout,
            original.stats.size,
            start,
            buffer,
            new_size,
            path,
        )
        .await?;
        self.flush_mutation_blocks()
            .await
            .map_err(|error| with_context(error, "block-flush", Some(path)))?;

        let fast_commit = {
            let gate_profile = Span::new(Event::GateWait);
            let _gate = self.inner.gate.lock().await;
            drop(gate_profile);
            self.ensure_operation_lease().await?;
            if orphan {
                let mut state = self.lock_state()?;
                match state.orphans.get_mut(&inode) {
                    Some(target) if write_base_unchanged(target, &original) => {
                        target.data = NodeData::File(new_layout);
                        set_file_size(&mut target.stats, new_size);
                        touch_modified(&mut target.stats, self.inner.options.concurrent_writes)?;
                        Some((buffer.len(), end))
                    }
                    _ => None,
                }
            } else {
                let (mut namespace, revision) = self.snapshot()?;
                let unchanged = namespace
                    .nodes
                    .get(&inode)
                    .is_some_and(|target| write_base_unchanged(target, &original));
                if !unchanged {
                    None
                } else {
                    let target = namespace
                        .nodes
                        .get_mut(&inode)
                        .ok_or_else(|| error_with_path(ErrorCode::Estale, "write", path))?;
                    target.data = NodeData::File(new_layout);
                    set_file_size(&mut target.stats, new_size);
                    touch_modified(&mut target.stats, self.inner.options.concurrent_writes)?;
                    match self.publish_namespace(revision, namespace, true).await {
                        Ok(_) => Some((buffer.len(), end)),
                        Err(error)
                            if self.inner.options.concurrent_writes
                                && error.code == ErrorCode::Eagain =>
                        {
                            // A known CAS loss has not published these
                            // immutable blocks. Rewrite against the winner's
                            // layout in the serialized fallback below.
                            None
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        };
        if let Some(result) = fast_commit {
            return Ok(result);
        }

        // Preserve the existing serialized semantics for a conflicting
        // operation. The immutable blocks uploaded by the optimistic attempt
        // are intentionally left for the explicit reconciliation grace window.
        self.write_at_serial(inode, path, buffer, position, append)
            .await
    }

    async fn write_at_serial(
        &self,
        inode: InodeId,
        path: &str,
        buffer: &[u8],
        position: u64,
        append: bool,
    ) -> Result<(usize, u64)> {
        profile::add(Event::Fallback, 0);
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        let attempts = if self.inner.options.concurrent_writes {
            MAX_CONCURRENT_CAS_RETRIES
        } else {
            1
        };
        for attempt in 0..attempts {
            self.ensure_operation_lease().await?;
            let (mut namespace, revision) = self.snapshot()?;
            let (node, orphan) = self.node_snapshot(&namespace, inode, "write", path)?;
            let layout = match &node.data {
                NodeData::File(layout) => layout.clone(),
                NodeData::Directory { .. } => {
                    return Err(error_with_path(ErrorCode::Eisdir, "write", path));
                }
                NodeData::Special => return Err(error_with_path(ErrorCode::Enxio, "write", path)),
                NodeData::Symlink { .. } => {
                    return Err(error_with_path(ErrorCode::Eio, "write", path));
                }
            };
            let start = if append { node.stats.size } else { position };
            let input_length = u64::try_from(buffer.len())
                .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
            let end = start
                .checked_add(input_length)
                .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
            if buffer.is_empty() {
                return Ok((0, start));
            }
            let new_size = node.stats.size.max(end);
            let new_layout = rewrite_layout(
                &self.inner.blocks,
                &layout,
                node.stats.size,
                start,
                buffer,
                new_size,
                path,
            )
            .await?;
            self.flush_mutation_blocks()
                .await
                .map_err(|error| with_context(error, "block-flush", Some(path)))?;
            if orphan {
                let mut state = self.lock_state()?;
                let target = state
                    .orphans
                    .get_mut(&inode)
                    .ok_or_else(|| error_with_path(ErrorCode::Estale, "write", path))?;
                target.data = NodeData::File(new_layout);
                set_file_size(&mut target.stats, new_size);
                touch_modified(&mut target.stats, self.inner.options.concurrent_writes)?;
                return Ok((buffer.len(), end));
            }
            let target = namespace
                .nodes
                .get_mut(&inode)
                .ok_or_else(|| error_with_path(ErrorCode::Estale, "write", path))?;
            target.data = NodeData::File(new_layout);
            set_file_size(&mut target.stats, new_size);
            touch_modified(&mut target.stats, self.inner.options.concurrent_writes)?;
            match self.publish_namespace(revision, namespace, true).await {
                Ok(_) => return Ok((buffer.len(), end)),
                Err(error)
                    if self.inner.options.concurrent_writes
                        && error.code == ErrorCode::Eagain
                        && attempt + 1 < attempts =>
                {
                    concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(FsError::new(ErrorCode::Eagain)
            .with_syscall("write")
            .with_message("another writer repeatedly changed the file"))
    }

    async fn write_file_atomic(&self, path: &str, data: &[u8]) -> Result<()> {
        let normalized = normalize_path(path);
        let _lifecycle = self.inner.lifecycle.read().await;
        if self.inner.options.inode_updates && self.replace_selected_file(&normalized, data).await?
        {
            return Ok(());
        }
        let preparation = self.begin_mutation_preparation();
        // This is an optimistic, read-only preparation snapshot. The state
        // mutex keeps it coherent while the batcher's revision/CAS and
        // conflict fallback decide whether it can publish after immutable
        // block work completes. Keep it outside the global mutation gate so
        // concurrent whole-file preparations can overlap; lease renewal has
        // its own gate because it may perform a provider mutation.
        self.ensure_operation_lease().await?;
        let (layout, original, inode, expected_revision, new_inode) = {
            let (namespace, revision) = self.snapshot()?;
            let entry = walk(&namespace, &normalized, true, "open", 0)?;
            self.require_inode_authority(&namespace, entry.node.unwrap_or(entry.parent))?;
            if let Some(inode) = entry.node {
                let node = namespace
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| error_with_path(ErrorCode::Estale, "open", &entry.path))?;
                let layout = match &node.data {
                    NodeData::File(layout) => layout.clone(),
                    NodeData::Directory { .. } => {
                        return Err(error_with_path(ErrorCode::Eisdir, "open", &entry.path));
                    }
                    NodeData::Special => {
                        return Err(error_with_path(ErrorCode::Enxio, "open", &entry.path));
                    }
                    NodeData::Symlink { .. } => {
                        return Err(error_with_path(ErrorCode::Eio, "open", &entry.path));
                    }
                };
                (layout, Some(node.clone()), inode, revision, false)
            } else {
                let parent = namespace
                    .nodes
                    .get(&entry.parent)
                    .ok_or_else(|| error_with_path(ErrorCode::Estale, "open", &entry.path))?;
                if !matches!(parent.data, NodeData::Directory { .. }) {
                    return Err(error_with_path(ErrorCode::Enotdir, "open", &entry.path));
                }
                let inode = namespace.next_inode;
                let original = new_file_node(
                    inode,
                    S_IFREG | (0o666 & !namespace.umask & 0o7777),
                    namespace.default_uid,
                    namespace.default_gid,
                    namespace.default_chunker.clone(),
                );
                let layout = match &original.data {
                    NodeData::File(layout) => layout.clone(),
                    _ => unreachable!("new_file_node must create a regular file"),
                };
                (layout, None, inode, revision, true)
            }
        };

        let empty_layout = FileLayout {
            chunker: layout.chunker.clone(),
            extents: Vec::new(),
        };
        let new_layout = if data.is_empty() {
            empty_layout
        } else {
            rewrite_layout(
                &self.inner.blocks,
                &empty_layout,
                0,
                0,
                data,
                u64::try_from(data.len())
                    .map_err(|_| error_with_path(ErrorCode::Efbig, "write", &normalized))?,
                &normalized,
            )
            .await?
        };
        // Full replacement blocks depend only on the input bytes and the
        // selected chunker. A known namespace CAS miss can reuse this flushed
        // immutable layout after rebasing the path and inode metadata.
        let replay_layout = new_layout.clone();
        if !data.is_empty() {
            self.flush_mutation_blocks()
                .await
                .map_err(|error| with_context(error, "block-flush", Some(&normalized)))?;
        }

        let mutation = WholeFileMutation {
            path: normalized.clone(),
            inode,
            expected_revision,
            new_inode,
            original,
            layout: new_layout,
            data_length: u64::try_from(data.len())
                .map_err(|_| error_with_path(ErrorCode::Efbig, "write", &normalized))?,
        };
        // The request is fully prepared now. Let the batch runner observe
        // other whole-file operations still doing immutable remote work, but
        // do not count this request while it waits for the shared publication
        // response; otherwise the first runner would wait on itself.
        preparation.release();
        match self.submit_whole_file_mutation(mutation).await {
            Ok(WholeFileMutationResult::Committed) => return Ok(()),
            Ok(WholeFileMutationResult::Conflict) => {}
            Err(error)
                if self.inner.options.concurrent_writes && error.code == ErrorCode::Eagain => {}
            Err(error) => return Err(error),
        }

        if self.inner.options.concurrent_writes {
            return self
                .write_file_atomic_concurrent_replay(&normalized, data, replay_layout)
                .await;
        }

        // A concurrent namespace change won the optimistic race. Preserve the
        // public writeFile semantics by falling back to the existing
        // open/write/close path; the immutable blocks from the abandoned
        // attempt remain protected by reconciliation grace.
        let handle = self
            .open_flags(
                &normalized,
                OpenFlags {
                    read: false,
                    write: true,
                    create: true,
                    truncate: true,
                    append: false,
                    exclusive: false,
                },
                0o666,
            )
            .await?;
        let operation = async {
            let mut written = 0_usize;
            while written < data.len() {
                let count = handle.write(&data[written..], Some(written as u64)).await?;
                if count == 0 || count > data.len() - written {
                    return Err(error_with_path(ErrorCode::Eio, "write", &normalized));
                }
                written += count;
            }
            Ok(())
        }
        .await;
        let close = handle.close().await;
        match (operation, close) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    /// A prepared whole-file batch can lose its CAS to an unrelated writer.
    /// Rebase path/inode metadata against the winner's namespace while reusing
    /// the already-flushed immutable layout. Reprepare blocks only if the
    /// path's chunker changed. The full replacement remains one publication;
    /// truncate followed by byte writes would expose partial revisions.
    async fn write_file_atomic_concurrent_replay(
        &self,
        path: &str,
        data: &[u8],
        mut prepared_layout: FileLayout,
    ) -> Result<()> {
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        let data_length = u64::try_from(data.len())
            .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
        for attempt in 0..MAX_CONCURRENT_CAS_RETRIES {
            self.ensure_operation_lease().await?;
            let (mut namespace, revision) = self.snapshot()?;
            let entry = walk(&namespace, path, true, "open", 0)?;
            self.require_inode_authority(&namespace, entry.node.unwrap_or(entry.parent))?;
            let (inode, chunker, new_inode) = if let Some(inode) = entry.node {
                let node = namespace
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| error_with_path(ErrorCode::Estale, "open", &entry.path))?;
                let chunker = match &node.data {
                    NodeData::File(layout) => layout.chunker.clone(),
                    NodeData::Directory { .. } => {
                        return Err(error_with_path(ErrorCode::Eisdir, "open", &entry.path));
                    }
                    NodeData::Special => {
                        return Err(error_with_path(ErrorCode::Enxio, "open", &entry.path));
                    }
                    NodeData::Symlink { .. } => {
                        return Err(error_with_path(ErrorCode::Eio, "open", &entry.path));
                    }
                };
                (inode, chunker, false)
            } else {
                let parent = namespace
                    .nodes
                    .get(&entry.parent)
                    .ok_or_else(|| error_with_path(ErrorCode::Estale, "open", &entry.path))?;
                if !matches!(parent.data, NodeData::Directory { .. }) {
                    return Err(error_with_path(ErrorCode::Enotdir, "open", &entry.path));
                }
                (
                    namespace.next_inode,
                    namespace.default_chunker.clone(),
                    true,
                )
            };
            if prepared_layout.chunker != chunker {
                let empty_layout = FileLayout {
                    chunker,
                    extents: Vec::new(),
                };
                prepared_layout = if data.is_empty() {
                    empty_layout
                } else {
                    rewrite_layout(
                        &self.inner.blocks,
                        &empty_layout,
                        0,
                        0,
                        data,
                        data_length,
                        path,
                    )
                    .await?
                };
                self.flush_mutation_blocks()
                    .await
                    .map_err(|error| with_context(error, "block-flush", Some(path)))?;
                // Block preparation may have taken a remote round trip. Reload
                // before applying inode/path edits instead of publishing a
                // namespace revision captured before that work.
                cooperative_yield().await;
                continue;
            }
            if new_inode {
                namespace.next_inode = namespace
                    .next_inode
                    .checked_add(1)
                    .ok_or_else(|| error_with_path(ErrorCode::Eoverflow, "open", &entry.path))?;
                namespace.nodes.insert(
                    inode,
                    new_file_node(
                        inode,
                        S_IFREG | (0o666 & !namespace.umask & 0o7777),
                        namespace.default_uid,
                        namespace.default_gid,
                        namespace.default_chunker.clone(),
                    ),
                );
                add_entry(
                    &mut namespace,
                    entry.parent,
                    entry.name,
                    inode,
                    "open",
                    &entry.path,
                    true,
                )?;
            }
            let target = namespace
                .nodes
                .get_mut(&inode)
                .ok_or_else(|| error_with_path(ErrorCode::Estale, "write", path))?;
            target.data = NodeData::File(prepared_layout.clone());
            set_file_size(&mut target.stats, data_length);
            touch_modified(&mut target.stats, self.inner.options.concurrent_writes)?;
            match self.publish_namespace(revision, namespace, true).await {
                Ok(_) => return Ok(()),
                Err(error)
                    if error.code == ErrorCode::Eagain
                        && attempt + 1 < MAX_CONCURRENT_CAS_RETRIES =>
                {
                    concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(FsError::new(ErrorCode::Eagain)
            .with_syscall("write file")
            .with_message("another writer repeatedly changed the namespace"))
    }

    async fn truncate_inode(&self, inode: InodeId, path: &str, length: u64) -> Result<()> {
        if self.inner.options.inode_updates {
            let _lifecycle = self.inner.lifecycle.read().await;
            return self
                .truncate_selected_inode(Some(inode), path, length)
                .await;
        }
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        let attempts = if self.inner.options.concurrent_writes {
            MAX_CONCURRENT_CAS_RETRIES
        } else {
            1
        };
        for attempt in 0..attempts {
            self.ensure_operation_lease().await?;
            let (mut namespace, revision) = self.snapshot()?;
            let (node, orphan) = self.node_snapshot(&namespace, inode, "ftruncate", path)?;
            let mut layout = match &node.data {
                NodeData::File(layout) => layout.clone(),
                NodeData::Directory { .. } => {
                    return Err(error_with_path(ErrorCode::Eisdir, "ftruncate", path));
                }
                NodeData::Special => {
                    return Err(error_with_path(ErrorCode::Enxio, "ftruncate", path));
                }
                NodeData::Symlink { .. } => {
                    return Err(error_with_path(ErrorCode::Eio, "ftruncate", path));
                }
            };
            if length < node.stats.size {
                trim_extents(&mut layout.extents, length)?;
            }
            if orphan {
                let mut state = self.lock_state()?;
                let target = state
                    .orphans
                    .get_mut(&inode)
                    .ok_or_else(|| error_with_path(ErrorCode::Estale, "ftruncate", path))?;
                target.data = NodeData::File(layout);
                set_file_size(&mut target.stats, length);
                touch_modified(&mut target.stats, self.inner.options.concurrent_writes)?;
                return Ok(());
            }
            let target = namespace
                .nodes
                .get_mut(&inode)
                .ok_or_else(|| error_with_path(ErrorCode::Estale, "ftruncate", path))?;
            target.data = NodeData::File(layout);
            set_file_size(&mut target.stats, length);
            touch_modified(&mut target.stats, self.inner.options.concurrent_writes)?;
            match self.publish_namespace(revision, namespace, false).await {
                Ok(_) => return Ok(()),
                Err(error)
                    if self.inner.options.concurrent_writes
                        && error.code == ErrorCode::Eagain
                        && attempt + 1 < attempts =>
                {
                    concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(FsError::new(ErrorCode::Eagain)
            .with_syscall("ftruncate")
            .with_message("another writer repeatedly changed the file"))
    }

    fn allocate_fd(&self, inode: InodeId) -> Result<u64> {
        let mut state = self.lock_state()?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.closed {
            return Err(FsError::new(ErrorCode::Ebadf).with_message("filesystem is closed"));
        }
        let next_count = state
            .open_refs
            .get(&inode)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| {
                FsError::new(ErrorCode::Emfile).with_message("open reference overflow")
            })?;
        let fd = state.next_fd;
        let next_fd = state.next_fd.checked_add(1).ok_or_else(|| {
            FsError::new(ErrorCode::Emfile).with_message("file descriptor overflow")
        })?;
        state.next_fd = next_fd;
        state.open_refs.insert(inode, next_count);
        Ok(fd)
    }

    /// Called with `inner.gate` held after the handle is marked closed. The
    /// local reference must be released before the first await so canceling a
    /// close cannot strand an inode behind a permanently closed handle.
    async fn close_inode_after_gate(&self, inode: InodeId) -> Result<()> {
        let should_reap =
            {
                let mut state = self.lock_state()?;
                if let Some(count) = state.open_refs.get_mut(&inode) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        state.open_refs.remove(&inode);
                    }
                }
                if !state.open_refs.contains_key(&inode) {
                    state.orphans.remove(&inode);
                }
                if (self.inner.options.concurrent_writes && !self.inner.options.delegated)
                    || state.failure.is_some()
                    || state.closed
                {
                    return Ok(());
                }
                state.namespace.nodes.get(&inode).is_some_and(|node| {
                    node.stats.nlink == 0 && !state.open_refs.contains_key(&inode)
                })
            };
        if !should_reap {
            return Ok(());
        }
        let attempts = if self.inner.options.delegated {
            MAX_CONCURRENT_CAS_RETRIES
        } else {
            1
        };
        for attempt in 0..attempts {
            self.ensure_operation_lease().await?;
            let (mut namespace, revision) = {
                let state = self.lock_state()?;
                if state.failure.is_some() || state.closed {
                    return Ok(());
                }
                if !state.namespace.nodes.get(&inode).is_some_and(|node| {
                    node.stats.nlink == 0 && !state.open_refs.contains_key(&inode)
                }) {
                    return Ok(());
                }
                (state.namespace.as_ref().clone(), state.revision)
            };
            namespace.nodes.remove(&inode);
            match self.publish_namespace(revision, namespace, false).await {
                Ok(_) => return Ok(()),
                Err(error) if error.code == ErrorCode::Eagain && attempt + 1 < attempts => {
                    concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn stat_inode(&self, inode: InodeId, syscall: &str, path: &str) -> Result<Stats> {
        let state = self.lock_state()?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.closed {
            return Err(FsError::new(ErrorCode::Ebadf).with_message("filesystem is closed"));
        }
        let mut stats = state
            .namespace
            .nodes
            .get(&inode)
            .or_else(|| state.orphans.get(&inode))
            .map(|node| node.stats.clone())
            .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, path))?;
        if let Some(atime_ms) = state.pending_atime.get(&inode) {
            stats.atime_ms = stats.atime_ms.max(*atime_ms);
        }
        Ok(stats)
    }

    /// Remove a detached inode from the next published namespace. Existing
    /// handles retain a private runtime copy until their last close, matching
    /// POSIX unlink/rmdir lifetime without publishing an unreachable
    /// directory or a stale nlink graph.
    fn reap_detached(&self, namespace: &mut Namespace, inode: InodeId) -> Result<()> {
        if self.inner.options.delegated {
            if self.lock_state()?.open_refs.contains_key(&inode)
                && namespace
                    .nodes
                    .get(&inode)
                    .is_some_and(|node| matches!(node.data, NodeData::Directory { .. }))
            {
                return Err(FsError::new(ErrorCode::Enotsup)
                    .with_message("open directory removal requires handle retirement"));
            }
            if !self.lock_state()?.open_refs.contains_key(&inode) {
                namespace.nodes.remove(&inode);
            }
            return Ok(());
        }
        if self.inner.options.concurrent_writes {
            if namespace
                .nodes
                .get(&inode)
                .is_some_and(|node| !matches!(node.data, NodeData::Directory { .. }))
            {
                // A nlink=0 tombstone retains inode identity for handles in
                // any process. Replaying a CAS conflict has no local side
                // effect, and no writer may reclaim its blocks yet.
                return Ok(());
            }
            namespace.nodes.remove(&inode);
            return Ok(());
        }
        let should_reap = namespace.nodes.get(&inode).is_some_and(|node| {
            matches!(node.data, NodeData::Directory { .. }) || node.stats.nlink == 0
        });
        if !should_reap {
            return Ok(());
        }
        let has_open = self.lock_state()?.open_refs.contains_key(&inode);
        if let Some(node) = namespace.nodes.remove(&inode)
            && has_open
        {
            self.lock_state()?.orphans.insert(inode, node);
        }
        Ok(())
    }

    async fn sync_handle(&self, syscall: &str) -> Result<()> {
        self.syncfs_with_syscall(syscall).await
    }

    /// Flush immutable blocks before the metadata durability barrier. A
    /// failed metadata barrier fails the coordinator closed because the
    /// caller cannot know whether the published revision is durable.
    pub async fn syncfs(&self) -> Result<()> {
        self.syncfs_with_syscall("syncfs").await
    }

    async fn syncfs_with_syscall(&self, syscall: &str) -> Result<()> {
        let _lifecycle = self.inner.lifecycle.write().await;
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.ensure_operation_lease().await?;
        self.snapshot()?;
        let mut barrier = self
            .inner
            .options
            .writeback
            .then(|| PublicationGuard::new(&self.inner.state));
        if self.drain_writeback().await? {
            if let Some(barrier) = &mut barrier {
                barrier.disarm();
            }
            return Ok(());
        }
        self.flush_pending_atime().await?;
        self.inner.blocks.flush().await.map_err(|error| {
            let error = with_context(error, syscall, None);
            if self.inner.options.writeback {
                self.fail_closed(error)
            } else {
                error
            }
        })?;
        self.validate_lease().await?;
        self.inner
            .metadata
            .flush()
            .await
            .map_err(|error| self.fail_closed(with_context(error, syscall, None)))?;
        if let Some(barrier) = &mut barrier {
            barrier.disarm();
        }
        Ok(())
    }

    async fn chown_path(
        &self,
        path: &str,
        uid: u32,
        gid: u32,
        follow: bool,
        syscall: &str,
    ) -> Result<()> {
        let normalized = normalize_path(path);
        self.mutate(|namespace| {
            let inode = resolve(namespace, &normalized, follow, syscall)?;
            let node = namespace
                .nodes
                .get_mut(&inode)
                .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, &normalized))?;
            if uid != u32::MAX {
                node.stats.uid = uid;
            }
            if gid != u32::MAX {
                node.stats.gid = gid;
            }
            touch_changed(&mut node.stats, self.inner.options.concurrent_writes)?;
            Ok(())
        })
        .await
    }

    async fn utimes_path(
        &self,
        path: &str,
        atime_ms: i64,
        mtime_ms: i64,
        follow: bool,
        syscall: &str,
    ) -> Result<()> {
        let normalized = normalize_path(path);
        self.mutate(|namespace| {
            let inode = resolve(namespace, &normalized, follow, syscall)?;
            let node = namespace
                .nodes
                .get_mut(&inode)
                .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, &normalized))?;
            node.stats.atime_ms = atime_ms;
            node.stats.mtime_ms = mtime_ms;
            touch_changed(&mut node.stats, self.inner.options.concurrent_writes)?;
            Ok(())
        })
        .await
    }

    async fn open_flags_inner(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
        guard: Option<(&PathGuard, &str, ObservedEntry)>,
    ) -> Result<(Arc<dyn FileHandle>, PathIdentity)> {
        let mut trace = RequestTrace::new("chunked", "open");
        trace.stage(
            "flags",
            format_args!(
                "path={path:?} create={} exclusive={} truncate={}",
                flags.create, flags.exclusive, flags.truncate
            ),
        );
        if !flags.read && !flags.write {
            return Err(error_with_path(ErrorCode::Einval, "open", path));
        }
        if flags.truncate && !flags.write {
            return Err(error_with_path(ErrorCode::Einval, "open", path));
        }
        let normalized = normalize_path(path);
        trace.stage("gate_wait", format_args!("path={normalized:?}"));
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        trace.stage("gate_acquired", format_args!("path={normalized:?}"));
        let attempts = if self.inner.options.concurrent_writes {
            MAX_CONCURRENT_CAS_RETRIES
        } else {
            1
        };
        for attempt in 0..attempts {
            trace.stage("metadata_load_start", format_args!("attempt={attempt}"));
            let loaded = self.ensure_operation_lease().await;
            trace.stage(
                "metadata_load_end",
                format_args!(
                    "attempt={attempt} outcome={}",
                    loaded
                        .as_ref()
                        .map_or_else(|error| error.code.as_str(), |_| "ok")
                ),
            );
            loaded?;
            let (mut namespace, revision) = self.snapshot()?;
            trace.stage(
                "snapshot",
                format_args!("attempt={attempt} expected_revision={revision}"),
            );
            let entry = walk(
                &namespace,
                &normalized,
                !(flags.create && flags.exclusive),
                "open",
                0,
            )?;
            if let Some((parent_guard, name, observed)) = guard {
                let original =
                    guarded_child_entry(&namespace, parent_guard, name, observed, "open")?;
                if original.path != normalized || entry.parent != original.parent {
                    return Err(stale_guard(&parent_guard.path, "open"));
                }
                if original.node.is_some_and(|inode| {
                    namespace
                        .nodes
                        .get(&inode)
                        .is_some_and(|node| matches!(node.data, NodeData::Symlink { .. }))
                }) {
                    // Regular-file CREATE must not follow a final symlink to
                    // an unobserved target, even when it stays in this parent.
                    return Err(error_with_path(ErrorCode::Eexist, "open", &normalized));
                }
                if entry.node != original.node {
                    return Err(stale_guard(&normalized, "open"));
                }
                if flags.truncate
                    && !flags.exclusive
                    && original.node.is_some()
                    && !matches!(observed, ObservedEntry::Identity(_))
                {
                    return Err(stale_guard(&normalized, "open"));
                }
            }
            if self.inner.options.delegated {
                let required = entry.node.unwrap_or(entry.parent);
                self.require_inode_authority(&namespace, required)?;
            }
            let inode = if let Some(inode) = entry.node {
                if flags.exclusive {
                    return Err(error_with_path(ErrorCode::Eexist, "open", &entry.path));
                }
                let node = namespace
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| error_with_path(ErrorCode::Estale, "open", &entry.path))?;
                let kind = FileType::from_mode(node.stats.mode);
                if kind == FileType::Directory && flags.write {
                    return Err(error_with_path(ErrorCode::Eisdir, "open", &entry.path));
                }
                if kind.is_special() {
                    return Err(error_with_path(ErrorCode::Enxio, "open", &entry.path));
                }
                if flags.truncate {
                    let node = namespace
                        .nodes
                        .get_mut(&inode)
                        .ok_or_else(|| error_with_path(ErrorCode::Estale, "open", &entry.path))?;
                    if let NodeData::File(layout) = &mut node.data {
                        layout.extents.clear();
                    }
                    set_file_size(&mut node.stats, 0);
                    touch_modified(&mut node.stats, self.inner.options.concurrent_writes)?;
                }
                inode
            } else {
                if !flags.create {
                    return Err(error_with_path(ErrorCode::Enoent, "open", &entry.path));
                }
                let parent = namespace
                    .nodes
                    .get(&entry.parent)
                    .ok_or_else(|| error_with_path(ErrorCode::Estale, "open", &entry.path))?;
                if !matches!(parent.data, NodeData::Directory { .. }) {
                    return Err(error_with_path(ErrorCode::Enotdir, "open", &entry.path));
                }
                let inode = namespace.next_inode;
                namespace.next_inode = namespace
                    .next_inode
                    .checked_add(1)
                    .ok_or_else(|| error_with_path(ErrorCode::Eoverflow, "open", &entry.path))?;
                let mode = S_IFREG | (mode & !namespace.umask & 0o7777);
                namespace.nodes.insert(
                    inode,
                    new_file_node(
                        inode,
                        mode,
                        namespace.default_uid,
                        namespace.default_gid,
                        namespace.default_chunker.clone(),
                    ),
                );
                add_entry(
                    &mut namespace,
                    entry.parent,
                    entry.name,
                    inode,
                    "open",
                    &entry.path,
                    self.inner.options.concurrent_writes,
                )?;
                inode
            };

            let changed = entry.node.is_none() || flags.truncate;
            if changed {
                trace.stage(
                    "publish_start",
                    format_args!("attempt={attempt} expected_revision={revision}"),
                );
                let result = if self.inner.options.inode_updates && entry.node.is_some() {
                    let expected = {
                        let state = self.lock_state()?;
                        if state.revision != revision {
                            None
                        } else {
                            Some(InodeVersion {
                                structural_generation: state.persisted_revision,
                                inode_revision: *state
                                    .inode_revisions
                                    .get(&inode)
                                    .ok_or_else(|| FsError::new(ErrorCode::Eio))?,
                            })
                        }
                    };
                    if let Some(expected) = expected {
                        let node = namespace.nodes.get(&inode).cloned().ok_or_else(|| {
                            error_with_path(ErrorCode::Estale, "open", &entry.path)
                        })?;
                        self.inner.blocks.flush().await?;
                        self.publish_selected_node(inode, expected, node)
                            .await
                            .map(|()| revision)
                    } else {
                        Err(FsError::new(ErrorCode::Eagain))
                    }
                } else {
                    self.publish_namespace(revision, namespace, false).await
                };
                match result {
                    Ok(next_revision) => {
                        trace.stage(
                            "publish_end",
                            format_args!("attempt={attempt} revision={next_revision}"),
                        );
                    }
                    Err(error)
                        if self.inner.options.concurrent_writes
                            && error.code == ErrorCode::Eagain
                            && attempt + 1 < attempts =>
                    {
                        trace.stage(
                            "cas_backoff",
                            format_args!("attempt={attempt} expected_revision={revision}"),
                        );
                        concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                        continue;
                    }
                    Err(error) => {
                        trace.finish(format_args!(
                            "attempt={attempt} error={}",
                            error.code.as_str()
                        ));
                        return Err(error);
                    }
                }
            }
            let fd = self.allocate_fd(inode)?;
            trace.finish(format_args!("attempt={attempt} inode={inode} fd={fd}"));
            return Ok((
                Arc::new(ChunkedHandle {
                    filesystem: self.clone(),
                    inode,
                    path: normalized,
                    fd,
                    flags,
                    generation: self.inner.delegation_generation.load(Ordering::SeqCst),
                    state: Mutex::new(HandleState {
                        position: 0,
                        closed: false,
                    }),
                    gate: AsyncGate::new(),
                }),
                PathIdentity { dev: 0, ino: inode },
            ));
        }
        Err(FsError::new(ErrorCode::Eagain)
            .with_syscall("open")
            .with_message("another writer repeatedly changed the namespace"))
    }
}

fn write_base_unchanged(current: &NodeMetadata, original: &NodeMetadata) -> bool {
    current.stats.ino == original.stats.ino
        && current.stats.size == original.stats.size
        && match (&current.data, &original.data) {
            (NodeData::File(current), NodeData::File(original)) => current == original,
            _ => false,
        }
}

struct ChunkedHandle<M, B>
where
    M: MetadataStore,
    B: BlockStore,
{
    filesystem: ChunkedFs<M, B>,
    inode: InodeId,
    path: String,
    fd: u64,
    flags: OpenFlags,
    generation: u64,
    state: Mutex<HandleState>,
    gate: AsyncGate,
}

struct HandleState {
    position: u64,
    closed: bool,
}

impl<M, B> ChunkedHandle<M, B>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    fn lock_state(&self) -> Result<MutexGuard<'_, HandleState>> {
        self.state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("handle state lock poisoned"))
    }

    fn check_generation(&self, syscall: &str) -> Result<()> {
        if self.filesystem.inner.options.delegated
            && self.generation
                != self
                    .filesystem
                    .inner
                    .delegation_generation
                    .load(Ordering::SeqCst)
        {
            return Err(error_with_path(ErrorCode::Estale, syscall, &self.path));
        }
        Ok(())
    }

    fn check_open(&self, write: bool, syscall: &str) -> Result<u64> {
        self.check_generation(syscall)?;
        let state = self.lock_state()?;
        if state.closed || (write && !self.flags.write) || (!write && !self.flags.read) {
            return Err(error_with_path(ErrorCode::Ebadf, syscall, &self.path));
        }
        Ok(state.position)
    }
}

#[async_trait]
impl<M, B> FileHandle for ChunkedHandle<M, B>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    fn fd(&self) -> Option<u64> {
        Some(self.fd)
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let _gate = self.gate.lock().await;
        let current = self.check_open(false, "read")?;
        let from = position.unwrap_or(current);
        let count = self
            .filesystem
            .read_at(self.inode, &self.path, buffer, from)
            .await?;
        if position.is_none() {
            let next = current
                .checked_add(
                    u64::try_from(count)
                        .map_err(|_| error_with_path(ErrorCode::Efbig, "read", &self.path))?,
                )
                .ok_or_else(|| error_with_path(ErrorCode::Efbig, "read", &self.path))?;
            self.lock_state()?.position = next;
        }
        Ok(count)
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let _gate = self.gate.lock().await;
        let current = self.check_open(true, "write")?;
        let (count, end) = self
            .filesystem
            .write_at(
                self.inode,
                &self.path,
                buffer,
                position.unwrap_or(current),
                self.flags.append,
            )
            .await?;
        if position.is_none() || self.flags.append {
            self.lock_state()?.position = end;
        }
        Ok(count)
    }

    async fn stat(&self) -> Result<Stats> {
        self.check_generation("fstat")?;
        let closed = {
            let state = self.lock_state()?;
            state.closed
        };
        if closed {
            return Err(error_with_path(ErrorCode::Ebadf, "fstat", &self.path));
        }
        let _gate = self.filesystem.inner.gate.lock().await;
        self.filesystem.validate_lease().await?;
        self.filesystem.stat_inode(self.inode, "fstat", &self.path)
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        let _gate = self.gate.lock().await;
        self.check_open(true, "ftruncate")?;
        self.filesystem
            .truncate_inode(self.inode, &self.path, length)
            .await
    }

    async fn sync(&self) -> Result<()> {
        self.check_generation("fsync")?;
        let _gate = self.gate.lock().await;
        let closed = {
            let state = self.lock_state()?;
            state.closed
        };
        if closed {
            return Err(error_with_path(ErrorCode::Ebadf, "fsync", &self.path));
        }
        self.filesystem.sync_handle("fsync").await
    }

    async fn datasync(&self) -> Result<()> {
        self.check_generation("fdatasync")?;
        let _gate = self.gate.lock().await;
        let closed = {
            let state = self.lock_state()?;
            state.closed
        };
        if closed {
            return Err(error_with_path(ErrorCode::Ebadf, "fdatasync", &self.path));
        }
        self.filesystem.sync_handle("fdatasync").await
    }

    async fn close(&self) -> Result<()> {
        let _gate = self.gate.lock().await;
        // Both gates are acquired before closing the handle. If this future
        // is canceled while waiting for either gate, a later close can retry.
        let _filesystem_gate = self.filesystem.inner.gate.lock().await;
        {
            let mut state = self.lock_state()?;
            if state.closed {
                return Ok(());
            }
            state.closed = true;
        }
        self.filesystem.close_inode_after_gate(self.inode).await
    }
}

#[async_trait]
impl<M, B> FsDriver for ChunkedFs<M, B>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    fn supports_guarded_mutations(&self) -> bool {
        true
    }

    fn supports_guarded_reads(&self) -> bool {
        true
    }

    fn stable_inode_ids(&self) -> bool {
        true
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.ensure_operation_lease().await?;
        let (namespace, _) = self.snapshot()?;
        match request {
            GuardedRead::Stat { target } => {
                let inode = check_path_guard(&namespace, &target, "guarded stat")?;
                let node = namespace
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| stale_guard(&target.path, "guarded stat"))?;
                Ok(GuardedReadResult::Stat(node.stats.clone()))
            }
            GuardedRead::Lookup { parent, name } => {
                let inode = check_path_guard(&namespace, &parent, "guarded lookup")?;
                let node = namespace
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| stale_guard(&parent.path, "guarded lookup"))?;
                let NodeData::Directory { entries } = &node.data else {
                    return Err(error_with_path(
                        ErrorCode::Enotdir,
                        "guarded lookup",
                        &parent.path,
                    ));
                };
                let child = match name.as_str() {
                    "." => inode,
                    ".." => walk(&namespace, &parent.path, false, "guarded lookup", 0)?.parent,
                    _ => {
                        guarded_child_path(&parent, &name, "guarded lookup")?;
                        entries
                            .iter()
                            .find(|entry| entry.name == name)
                            .map(|entry| entry.inode)
                            .ok_or_else(|| {
                                error_with_path(
                                    ErrorCode::Enoent,
                                    "guarded lookup",
                                    &format!("{}/{name}", parent.path),
                                )
                            })?
                    }
                };
                let child_node = namespace.nodes.get(&child).ok_or_else(|| {
                    stale_guard(&format!("{}/{name}", parent.path), "guarded lookup")
                })?;
                Ok(GuardedReadResult::Lookup {
                    parent: node.stats.clone(),
                    child: child_node.stats.clone(),
                })
            }
            GuardedRead::Readdir {
                directory,
                max_entries,
            } => {
                if max_entries == 0 {
                    return Err(error_with_path(
                        ErrorCode::Einval,
                        "guarded readdir",
                        &directory.path,
                    )
                    .with_message("directory entry limit must be positive"));
                }
                let inode = check_path_guard(&namespace, &directory, "guarded readdir")?;
                let node = namespace
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| stale_guard(&directory.path, "guarded readdir"))?;
                let NodeData::Directory { entries } = &node.data else {
                    return Err(error_with_path(
                        ErrorCode::Enotdir,
                        "guarded readdir",
                        &directory.path,
                    ));
                };
                if entries.len() > max_entries {
                    return Err(error_with_path(
                        ErrorCode::Eoverflow,
                        "guarded readdir",
                        &directory.path,
                    )
                    .with_message("directory exceeds the configured entry limit"));
                }
                let mut result = Vec::with_capacity(entries.len());
                for entry in entries {
                    let child = namespace
                        .nodes
                        .get(&entry.inode)
                        .ok_or_else(|| stale_guard(&directory.path, "guarded readdir"))?;
                    result.push(GuardedDirectoryEntry {
                        name: entry.name.clone(),
                        stats: child.stats.clone(),
                    });
                }
                Ok(GuardedReadResult::Directory {
                    stats: node.stats.clone(),
                    entries: result,
                })
            }
            GuardedRead::Readlink { target } => {
                let inode = check_path_guard(&namespace, &target, "guarded readlink")?;
                let node = namespace
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| stale_guard(&target.path, "guarded readlink"))?;
                let NodeData::Symlink { target: link } = &node.data else {
                    return Err(error_with_path(
                        ErrorCode::Einval,
                        "guarded readlink",
                        &target.path,
                    ));
                };
                Ok(GuardedReadResult::Readlink {
                    stats: node.stats.clone(),
                    target: link.clone(),
                })
            }
        }
    }

    async fn guarded_mutation(&self, request: GuardedMutation) -> Result<GuardedMutationResult> {
        match request {
            GuardedMutation::Setattr { target, change } => {
                self.mutate(|namespace| {
                    apply_guarded_setattr(
                        namespace,
                        &target,
                        change,
                        self.inner.options.concurrent_writes,
                    )
                })
                .await?;
                Ok(GuardedMutationResult::Applied)
            }
            GuardedMutation::Open {
                parent,
                name,
                observed,
                flags,
                mode,
            } => {
                let path = guarded_child_path(&parent, &name, "open")?;
                let (handle, identity) = self
                    .open_flags_inner(&path, flags, mode, Some((&parent, &name, observed)))
                    .await?;
                Ok(GuardedMutationResult::Opened { handle, identity })
            }
            GuardedMutation::Mkdir { parent, name, mode } => {
                let path = guarded_child_path(&parent, &name, "mkdir")?;
                let inode = self
                    .mutate(|namespace| {
                        guarded_child_entry(
                            namespace,
                            &parent,
                            &name,
                            ObservedEntry::Any,
                            "mkdir",
                        )?;
                        apply_mkdir_mutation(
                            namespace,
                            &path,
                            mode,
                            self.inner.options.concurrent_writes,
                        )
                    })
                    .await?;
                Ok(GuardedMutationResult::Created(PathIdentity {
                    dev: 0,
                    ino: inode,
                }))
            }
            GuardedMutation::Symlink {
                parent,
                name,
                target,
            } => {
                let path = guarded_child_path(&parent, &name, "symlink")?;
                let inode = self
                    .mutate(|namespace| {
                        guarded_child_entry(
                            namespace,
                            &parent,
                            &name,
                            ObservedEntry::Any,
                            "symlink",
                        )?;
                        apply_symlink_mutation(
                            namespace,
                            &target,
                            &path,
                            self.inner.options.concurrent_writes,
                        )
                    })
                    .await?;
                Ok(GuardedMutationResult::Created(PathIdentity {
                    dev: 0,
                    ino: inode,
                }))
            }
            GuardedMutation::Mknod {
                parent,
                name,
                mode,
                dev,
            } => {
                let path = guarded_child_path(&parent, &name, "mknod")?;
                let inode = self
                    .mutate(|namespace| {
                        guarded_child_entry(
                            namespace,
                            &parent,
                            &name,
                            ObservedEntry::Any,
                            "mknod",
                        )?;
                        apply_mknod_mutation(
                            namespace,
                            &path,
                            mode,
                            dev,
                            self.inner.options.concurrent_writes,
                        )
                    })
                    .await?;
                Ok(GuardedMutationResult::Created(PathIdentity {
                    dev: 0,
                    ino: inode,
                }))
            }
            GuardedMutation::Unlink {
                parent,
                name,
                entry,
            } => {
                let path = guarded_child_path(&parent, &name, "unlink")?;
                let runtime = self.clone();
                self.mutate(|namespace| {
                    guarded_child_entry(namespace, &parent, &name, entry, "unlink")?;
                    apply_unlink_mutation(&runtime, namespace, &path)
                })
                .await?;
                Ok(GuardedMutationResult::Applied)
            }
            GuardedMutation::Rmdir {
                parent,
                name,
                entry,
            } => {
                let path = guarded_child_path(&parent, &name, "rmdir")?;
                let runtime = self.clone();
                self.mutate(|namespace| {
                    guarded_child_entry(namespace, &parent, &name, entry, "rmdir")?;
                    apply_rmdir_mutation(&runtime, namespace, &path)
                })
                .await?;
                Ok(GuardedMutationResult::Applied)
            }
            GuardedMutation::Rename {
                from_parent,
                from_name,
                source,
                to_parent,
                to_name,
                destination,
            } => {
                let old_path = guarded_child_path(&from_parent, &from_name, "rename")?;
                let new_path = guarded_child_path(&to_parent, &to_name, "rename")?;
                let runtime = self.clone();
                self.mutate(|namespace| {
                    guarded_child_entry(namespace, &from_parent, &from_name, source, "rename")?;
                    guarded_child_entry(namespace, &to_parent, &to_name, destination, "rename")?;
                    apply_rename_mutation(&runtime, namespace, &old_path, &new_path)
                })
                .await?;
                Ok(GuardedMutationResult::Applied)
            }
            GuardedMutation::Link {
                source,
                to_parent,
                to_name,
                destination,
            } => {
                let new_path = guarded_child_path(&to_parent, &to_name, "link")?;
                self.mutate(|namespace| {
                    check_path_guard(namespace, &source, "link")?;
                    guarded_child_entry(namespace, &to_parent, &to_name, destination, "link")?;
                    apply_link_mutation(
                        namespace,
                        &source.path,
                        &new_path,
                        self.inner.options.concurrent_writes,
                    )
                })
                .await?;
                Ok(GuardedMutationResult::Applied)
            }
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            handles: true,
            hardlinks: true,
            symlinks: true,
            permissions: true,
            times: true,
            truncate: true,
            atomic_rename: true,
            case_sensitive: true,
            statfs: true,
            read_only: false,
            durable_writes: self.inner.metadata.durable() && self.inner.blocks.durable(),
            mknod: true,
        }
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.validate_lease().await?;
        let (namespace, _) = self.snapshot()?;
        let inode = resolve(&namespace, path, true, "stat")?;
        self.stat_inode(inode, "stat", &normalize_path(path))
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.validate_lease().await?;
        let (namespace, _) = self.snapshot()?;
        let inode = resolve(&namespace, path, false, "lstat")?;
        self.stat_inode(inode, "lstat", &normalize_path(path))
    }

    async fn statfs(&self, path: &str) -> Result<StatsFs> {
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.validate_lease().await?;
        let (namespace, _) = self.snapshot()?;
        resolve(&namespace, path, true, "statfs")?;
        const BLOCKS: u64 = 1024 * 1024;
        const FILES: u64 = 1024 * 1024;
        // Match the logical mountx filesystem accounting, not the physical
        // provider's retained immutable objects or 512-byte stat.blocks units.
        let used_blocks = namespace
            .nodes
            .values()
            .filter(|node| node.stats.nlink > 0)
            .fold(0_u64, |total, node| {
                total.saturating_add(node.stats.size.div_ceil(BLOCK_SIZE))
            });
        Ok(StatsFs {
            filesystem_type: 0x0102_1994,
            block_size: BLOCK_SIZE,
            blocks: BLOCKS,
            blocks_free: BLOCKS.saturating_sub(used_blocks),
            blocks_available: BLOCKS.saturating_sub(used_blocks),
            files: FILES,
            files_free: FILES
                .saturating_sub(u64::try_from(namespace.nodes.len()).unwrap_or(u64::MAX)),
        })
    }

    async fn syncfs(&self) -> Result<()> {
        ChunkedFs::syncfs(self).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.ensure_operation_lease().await?;
        let (mut namespace, revision) = self.snapshot()?;
        let normalized = normalize_path(path);
        let inode = resolve(&namespace, &normalized, true, "scandir")?;
        let node = namespace
            .nodes
            .get(&inode)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, "scandir", &normalized))?;
        let entries = match &node.data {
            NodeData::Directory { entries } => entries.clone(),
            _ => return Err(error_with_path(ErrorCode::Enotdir, "scandir", &normalized)),
        };
        let result = entries
            .into_iter()
            .filter_map(|entry| {
                namespace.nodes.get(&entry.inode).map(|node| DirEntry {
                    name: entry.name,
                    parent_path: normalized.clone(),
                    file_type: FileType::from_mode(node.stats.mode),
                })
            })
            .collect();
        if self.inner.options.concurrent_writes {
            return Ok(result);
        }
        if let Some(node) = namespace.nodes.get_mut(&inode) {
            node.stats.atime_ms = now_ms();
        }
        self.publish_namespace(revision, namespace, false).await?;
        Ok(result)
    }

    async fn readdir_bounded(&self, path: &str, max_entries: usize) -> Result<Vec<DirEntry>> {
        if max_entries == 0 {
            return Err(error_with_path(ErrorCode::Einval, "scandir", path)
                .with_message("directory entry limit must be positive"));
        }
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.ensure_operation_lease().await?;
        let (mut namespace, revision) = self.snapshot()?;
        let normalized = normalize_path(path);
        let inode = resolve(&namespace, &normalized, true, "scandir")?;
        let node = namespace
            .nodes
            .get(&inode)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, "scandir", &normalized))?;
        let entries = match &node.data {
            NodeData::Directory { entries } => entries,
            _ => return Err(error_with_path(ErrorCode::Enotdir, "scandir", &normalized)),
        };
        let mut result = Vec::new();
        for entry in entries {
            if result.len() == max_entries {
                return Err(
                    error_with_path(ErrorCode::Eoverflow, "scandir", &normalized)
                        .with_message("directory exceeds the configured entry limit"),
                );
            }
            if let Some(node) = namespace.nodes.get(&entry.inode) {
                result.push(DirEntry {
                    name: entry.name.clone(),
                    parent_path: normalized.clone(),
                    file_type: FileType::from_mode(node.stats.mode),
                });
            }
        }
        if self.inner.options.concurrent_writes {
            return Ok(result);
        }
        if let Some(node) = namespace.nodes.get_mut(&inode) {
            node.stats.atime_ms = now_ms();
        }
        self.publish_namespace(revision, namespace, false).await?;
        Ok(result)
    }

    async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        self.write_file_atomic(path, data).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        let parsed = OpenFlags::parse(flags, path)?;
        self.open_flags(path, parsed, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        self.open_flags_inner(path, flags, mode, None)
            .await
            .map(|(handle, _)| handle)
    }

    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        let normalized = normalize_path(path);
        self.mutate(|namespace| {
            let mode = S_IFDIR | (options.mode.unwrap_or(0o777) & !namespace.umask & 0o7777);
            if options.recursive {
                let mut current = String::from("/");
                let mut first_created = None;
                for segment in split_path(&normalized) {
                    if current != "/" {
                        current.push('/');
                    }
                    current.push_str(&segment);
                    let entry = walk(namespace, &current, true, "mkdir", 0)?;
                    if let Some(inode) = entry.node {
                        let node = namespace
                            .nodes
                            .get(&inode)
                            .ok_or_else(|| error_with_path(ErrorCode::Estale, "mkdir", &current))?;
                        if !matches!(node.data, NodeData::Directory { .. }) {
                            return Err(error_with_path(
                                if current == normalized {
                                    ErrorCode::Eexist
                                } else {
                                    ErrorCode::Enotdir
                                },
                                "mkdir",
                                &current,
                            ));
                        }
                    } else {
                        let inode = namespace.next_inode;
                        namespace.next_inode =
                            namespace.next_inode.checked_add(1).ok_or_else(|| {
                                error_with_path(ErrorCode::Eoverflow, "mkdir", &current)
                            })?;
                        namespace.nodes.insert(
                            inode,
                            new_directory_node(
                                inode,
                                mode,
                                namespace.default_uid,
                                namespace.default_gid,
                            ),
                        );
                        add_entry(
                            namespace,
                            entry.parent,
                            entry.name,
                            inode,
                            "mkdir",
                            &current,
                            self.inner.options.concurrent_writes,
                        )?;
                        first_created.get_or_insert(current.clone());
                    }
                }
                Ok(first_created)
            } else {
                apply_mkdir_mutation(
                    namespace,
                    &normalized,
                    mode,
                    self.inner.options.concurrent_writes,
                )
                .map(|_| None)
            }
        })
        .await
    }

    async fn rmdir(&self, path: &str) -> Result<()> {
        let normalized = normalize_path(path);
        let runtime = self.clone();
        self.mutate(|namespace| apply_rmdir_mutation(&runtime, namespace, &normalized))
            .await
    }

    async fn unlink(&self, path: &str) -> Result<()> {
        let normalized = normalize_path(path);
        let _lifecycle = self.inner.lifecycle.read().await;
        self.submit_unlink_mutation(normalized).await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        let old_normalized = normalize_path(old_path);
        let new_normalized = normalize_path(new_path);
        let runtime = self.clone();
        self.mutate(|namespace| {
            apply_rename_mutation(&runtime, namespace, &old_normalized, &new_normalized)
        })
        .await
    }

    async fn link(&self, existing_path: &str, new_path: &str) -> Result<()> {
        let existing = normalize_path(existing_path);
        let new_path = normalize_path(new_path);
        self.mutate(|namespace| {
            apply_link_mutation(
                namespace,
                &existing,
                &new_path,
                self.inner.options.concurrent_writes,
            )
        })
        .await
    }

    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        let normalized = normalize_path(path);
        self.mutate(|namespace| {
            apply_symlink_mutation(
                namespace,
                target,
                &normalized,
                self.inner.options.concurrent_writes,
            )
            .map(|_| ())
        })
        .await
    }

    async fn readlink(&self, path: &str) -> Result<String> {
        let gate_profile = Span::new(Event::GateWait);
        let _gate = self.inner.gate.lock().await;
        drop(gate_profile);
        self.ensure_operation_lease().await?;
        let (namespace, _) = self.snapshot()?;
        let normalized = normalize_path(path);
        let inode = resolve(&namespace, &normalized, false, "readlink")?;
        match &namespace
            .nodes
            .get(&inode)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, "readlink", &normalized))?
            .data
        {
            NodeData::Symlink { target } => Ok(target.clone()),
            _ => Err(error_with_path(ErrorCode::Einval, "readlink", &normalized)),
        }
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        let normalized = normalize_path(path);
        self.mutate(|namespace| {
            let inode = resolve(namespace, &normalized, true, "chmod")?;
            let node = namespace
                .nodes
                .get_mut(&inode)
                .ok_or_else(|| error_with_path(ErrorCode::Estale, "chmod", &normalized))?;
            node.stats.mode = (node.stats.mode & S_IFMT) | (mode & 0o7777);
            touch_changed(&mut node.stats, self.inner.options.concurrent_writes)?;
            Ok(())
        })
        .await
    }

    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.chown_path(path, uid, gid, true, "chown").await
    }

    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.chown_path(path, uid, gid, false, "lchown").await
    }

    async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        let normalized = normalize_path(path);
        if self.inner.options.inode_updates {
            let _lifecycle = self.inner.lifecycle.read().await;
            return self
                .truncate_selected_inode(None, &normalized, length)
                .await;
        }
        self.mutate(|namespace| {
            let inode = resolve(namespace, &normalized, true, "truncate")?;
            let node = namespace
                .nodes
                .get_mut(&inode)
                .ok_or_else(|| error_with_path(ErrorCode::Estale, "truncate", &normalized))?;
            let mut layout = match &node.data {
                NodeData::File(layout) => layout.clone(),
                NodeData::Directory { .. } => {
                    return Err(error_with_path(ErrorCode::Eisdir, "truncate", &normalized));
                }
                NodeData::Special => {
                    return Err(error_with_path(ErrorCode::Einval, "truncate", &normalized));
                }
                NodeData::Symlink { .. } => {
                    return Err(error_with_path(ErrorCode::Eio, "truncate", &normalized));
                }
            };
            if length < node.stats.size {
                trim_extents(&mut layout.extents, length)?;
            }
            node.data = NodeData::File(layout);
            set_file_size(&mut node.stats, length);
            touch_modified(&mut node.stats, self.inner.options.concurrent_writes)?;
            Ok(())
        })
        .await
    }

    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.utimes_path(path, atime_ms, mtime_ms, true, "utimes")
            .await
    }

    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.utimes_path(path, atime_ms, mtime_ms, false, "lutimes")
            .await
    }

    async fn mknod(&self, path: &str, mode: u32, dev: u64) -> Result<()> {
        let normalized = normalize_path(path);
        self.mutate(|namespace| {
            apply_mknod_mutation(
                namespace,
                &normalized,
                mode,
                dev,
                self.inner.options.concurrent_writes,
            )
            .map(|_| ())
        })
        .await
    }
}

fn apply_rmdir_mutation<M, B>(
    runtime: &ChunkedFs<M, B>,
    namespace: &mut Namespace,
    path: &str,
) -> Result<()>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    let normalized = normalize_path(path);

    let entry = walk(namespace, &normalized, false, "rmdir", 0)?;
    let inode = entry
        .node
        .ok_or_else(|| error_with_path(ErrorCode::Enoent, "rmdir", &entry.path))?;
    if inode == namespace.root {
        return Err(error_with_path(ErrorCode::Ebusy, "rmdir", &entry.path));
    }
    let node = namespace
        .nodes
        .get(&inode)
        .ok_or_else(|| error_with_path(ErrorCode::Estale, "rmdir", &entry.path))?;
    if !matches!(node.data, NodeData::Directory { .. }) {
        return Err(error_with_path(ErrorCode::Enotdir, "rmdir", &entry.path));
    }
    if let NodeData::Directory { entries } = &node.data
        && !entries.is_empty()
    {
        return Err(error_with_path(ErrorCode::Enotempty, "rmdir", &entry.path));
    }
    detach_entry(
        namespace,
        entry.parent,
        &entry.name,
        true,
        "rmdir",
        &entry.path,
        runtime.inner.options.concurrent_writes,
    )?;
    runtime.reap_detached(namespace, inode)
}

fn apply_rename_mutation<M, B>(
    runtime: &ChunkedFs<M, B>,
    namespace: &mut Namespace,
    old_path: &str,
    new_path: &str,
) -> Result<()>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    let old_normalized = normalize_path(old_path);
    let new_normalized = normalize_path(new_path);

    let from = walk(namespace, &old_normalized, false, "rename", 0)?;
    let source = from.node.ok_or_else(|| {
        error_with_path(ErrorCode::Enoent, "rename", &old_normalized)
            .with_dest(new_normalized.clone())
    })?;
    if source == namespace.root {
        return Err(error_with_path(
            ErrorCode::Einval,
            "rename",
            &old_normalized,
        ));
    }
    let to = walk(namespace, &new_normalized, false, "rename", 0)?;
    if to.node == Some(source) {
        return Ok(());
    }
    let source_is_dir = namespace
        .nodes
        .get(&source)
        .is_some_and(|node| matches!(node.data, NodeData::Directory { .. }));
    if source_is_dir && is_path_inside(&to.path, &from.path) {
        return Err(
            error_with_path(ErrorCode::Einval, "rename", &old_normalized)
                .with_dest(new_normalized.clone()),
        );
    }
    if let Some(destination) = to.node {
        let destination_is_dir = namespace
            .nodes
            .get(&destination)
            .is_some_and(|node| matches!(node.data, NodeData::Directory { .. }));
        if source_is_dir {
            if !destination_is_dir {
                return Err(
                    error_with_path(ErrorCode::Enotdir, "rename", &old_normalized)
                        .with_dest(new_normalized.clone()),
                );
            }
            if let Some(NodeMetadata {
                data: NodeData::Directory { entries },
                ..
            }) = namespace.nodes.get(&destination)
                && !entries.is_empty()
            {
                return Err(
                    error_with_path(ErrorCode::Enotempty, "rename", &old_normalized)
                        .with_dest(new_normalized.clone()),
                );
            }
        } else if destination_is_dir {
            return Err(
                error_with_path(ErrorCode::Eisdir, "rename", &old_normalized)
                    .with_dest(new_normalized.clone()),
            );
        }
        detach_entry(
            namespace,
            to.parent,
            &to.name,
            true,
            "rename",
            &to.path,
            runtime.inner.options.concurrent_writes,
        )?;
        runtime.reap_detached(namespace, destination)?;
    }
    detach_entry(
        namespace,
        from.parent,
        &from.name,
        false,
        "rename",
        &from.path,
        runtime.inner.options.concurrent_writes,
    )?;
    add_entry(
        namespace,
        to.parent,
        to.name,
        source,
        "rename",
        &to.path,
        runtime.inner.options.concurrent_writes,
    )?;
    if let Some(node) = namespace.nodes.get_mut(&source) {
        touch_changed(&mut node.stats, runtime.inner.options.concurrent_writes)?;
    }
    Ok(())
}

fn apply_link_mutation(
    namespace: &mut Namespace,
    existing_path: &str,
    target_path: &str,
    concurrent: bool,
) -> Result<()> {
    let existing = normalize_path(existing_path);
    let new_path = normalize_path(target_path);

    let from = walk(namespace, &existing, false, "link", 0)?;
    let inode = from.node.ok_or_else(|| {
        error_with_path(ErrorCode::Enoent, "link", &from.path).with_dest(new_path.clone())
    })?;
    if namespace
        .nodes
        .get(&inode)
        .is_some_and(|node| matches!(node.data, NodeData::Directory { .. }))
    {
        return Err(
            error_with_path(ErrorCode::Eperm, "link", &from.path).with_dest(new_path.clone())
        );
    }
    let to = walk(namespace, &new_path, false, "link", 0)?;
    if to.node.is_some() {
        return Err(error_with_path(ErrorCode::Eexist, "link", &to.path));
    }
    if let Some(node) = namespace.nodes.get_mut(&inode) {
        node.stats.nlink = node
            .stats
            .nlink
            .checked_add(1)
            .ok_or_else(|| error_with_path(ErrorCode::Emlink, "link", &from.path))?;
        touch_changed(&mut node.stats, concurrent)?;
    }
    add_entry(
        namespace, to.parent, to.name, inode, "link", &to.path, concurrent,
    )
}

fn apply_mkdir_mutation(
    namespace: &mut Namespace,
    path: &str,
    mode: u32,
    concurrent: bool,
) -> Result<InodeId> {
    let normalized = normalize_path(path);
    let entry = walk(namespace, &normalized, false, "mkdir", 0)?;
    if entry.node.is_some() {
        return Err(error_with_path(ErrorCode::Eexist, "mkdir", &entry.path));
    }
    let inode = namespace.next_inode;
    namespace.next_inode = namespace
        .next_inode
        .checked_add(1)
        .ok_or_else(|| error_with_path(ErrorCode::Eoverflow, "mkdir", &normalized))?;
    let mode = S_IFDIR | (mode & !namespace.umask & 0o7777);
    namespace.nodes.insert(
        inode,
        new_directory_node(inode, mode, namespace.default_uid, namespace.default_gid),
    );
    add_entry(
        namespace,
        entry.parent,
        entry.name,
        inode,
        "mkdir",
        &entry.path,
        concurrent,
    )?;
    Ok(inode)
}

fn apply_symlink_mutation(
    namespace: &mut Namespace,
    target: &str,
    path: &str,
    concurrent: bool,
) -> Result<InodeId> {
    let normalized = normalize_path(path);

    if target.is_empty() {
        return Err(
            error_with_path(ErrorCode::Enoent, "symlink", target).with_dest(normalized.clone())
        );
    }
    let entry = walk(namespace, &normalized, false, "symlink", 0)?;
    if entry.node.is_some() {
        return Err(error_with_path(ErrorCode::Eexist, "symlink", &entry.path));
    }
    let inode = namespace.next_inode;
    namespace.next_inode = namespace
        .next_inode
        .checked_add(1)
        .ok_or_else(|| error_with_path(ErrorCode::Eoverflow, "symlink", &normalized))?;
    let timestamp = now_ms();
    namespace.nodes.insert(
        inode,
        NodeMetadata {
            stats: Stats {
                dev: 0,
                ino: inode,
                mode: mount_rs_core::types::S_IFLNK | 0o777,
                nlink: 1,
                uid: namespace.default_uid,
                gid: namespace.default_gid,
                rdev: 0,
                size: u64::try_from(target.len())
                    .map_err(|_| error_with_path(ErrorCode::Efbig, "symlink", &entry.path))?,
                blksize: BLOCK_SIZE,
                blocks: u64::try_from(target.len())
                    .map_err(|_| error_with_path(ErrorCode::Efbig, "symlink", &entry.path))?
                    .div_ceil(512),
                atime_ms: timestamp,
                mtime_ms: timestamp,
                ctime_ms: timestamp,
                birthtime_ms: timestamp,
            },
            data: NodeData::Symlink {
                target: target.to_owned(),
            },
        },
    );
    add_entry(
        namespace,
        entry.parent,
        entry.name,
        inode,
        "symlink",
        &entry.path,
        concurrent,
    )?;
    Ok(inode)
}

fn apply_mknod_mutation(
    namespace: &mut Namespace,
    path: &str,
    mode: u32,
    dev: u64,
    concurrent: bool,
) -> Result<InodeId> {
    let normalized = normalize_path(path);

    let entry = walk(namespace, &normalized, false, "mknod", 0)?;
    if entry.node.is_some() {
        return Err(error_with_path(ErrorCode::Eexist, "mknod", &entry.path));
    }
    let kind = FileType::from_mode(mode);
    if !kind.is_special() && kind != FileType::File {
        return Err(error_with_path(ErrorCode::Eperm, "mknod", &entry.path));
    }
    let inode = namespace.next_inode;
    namespace.next_inode = namespace
        .next_inode
        .checked_add(1)
        .ok_or_else(|| error_with_path(ErrorCode::Eoverflow, "mknod", &normalized))?;
    let timestamp = now_ms();
    let mode = kind.mode_bits() | (mode & !S_IFMT & !namespace.umask & 0o7777);
    namespace.nodes.insert(
        inode,
        NodeMetadata {
            stats: Stats {
                dev: 0,
                ino: inode,
                mode,
                nlink: 1,
                uid: namespace.default_uid,
                gid: namespace.default_gid,
                rdev: if matches!(kind, FileType::BlockDevice | FileType::CharacterDevice) {
                    dev
                } else {
                    0
                },
                size: 0,
                blksize: BLOCK_SIZE,
                blocks: 0,
                atime_ms: timestamp,
                mtime_ms: timestamp,
                ctime_ms: timestamp,
                birthtime_ms: timestamp,
            },
            data: if kind == FileType::File {
                NodeData::File(FileLayout {
                    chunker: namespace.default_chunker.clone(),
                    extents: Vec::new(),
                })
            } else {
                NodeData::Special
            },
        },
    );
    add_entry(
        namespace,
        entry.parent,
        entry.name,
        inode,
        "mknod",
        &entry.path,
        concurrent,
    )?;
    Ok(inode)
}

fn mutation_reply(request: MutationRequest, result: Result<MutationResult>) -> bool {
    match request {
        MutationRequest::WholeFile { reply, .. } | MutationRequest::Unlink { reply, .. } => {
            reply.send(result).is_ok()
        }
    }
}

fn apply_whole_file_mutation(
    namespace: &mut Namespace,
    current_revision: u64,
    mutation: &WholeFileMutation,
    concurrent: bool,
) -> Result<WholeFileMutationResult> {
    if mutation.new_inode {
        let entry = walk(namespace, &mutation.path, true, "open", 0)?;
        if current_revision != mutation.expected_revision
            || namespace.next_inode != mutation.inode
            || entry.node.is_some()
        {
            return Ok(WholeFileMutationResult::Conflict);
        }
        namespace.next_inode = namespace
            .next_inode
            .checked_add(1)
            .ok_or_else(|| error_with_path(ErrorCode::Eoverflow, "open", &mutation.path))?;
        let node = new_file_node(
            mutation.inode,
            S_IFREG | (0o666 & !namespace.umask & 0o7777),
            namespace.default_uid,
            namespace.default_gid,
            namespace.default_chunker.clone(),
        );
        namespace.nodes.insert(mutation.inode, node);
        add_entry(
            namespace,
            entry.parent,
            entry.name,
            mutation.inode,
            "open",
            &entry.path,
            concurrent,
        )?;
        let target = namespace
            .nodes
            .get_mut(&mutation.inode)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, "write", &mutation.path))?;
        target.data = NodeData::File(mutation.layout.clone());
        set_file_size(&mut target.stats, mutation.data_length);
        touch_modified(&mut target.stats, concurrent)?;
        return Ok(WholeFileMutationResult::Committed);
    }

    if current_revision != mutation.expected_revision {
        return Ok(WholeFileMutationResult::Conflict);
    }
    // A same-batch unlink or rename can leave the old inode as a concurrent
    // tombstone. Path-based writeFile must not acknowledge bytes on an inode
    // that the requested path no longer names.
    if walk(namespace, &mutation.path, true, "write", 0)
        .ok()
        .and_then(|entry| entry.node)
        != Some(mutation.inode)
    {
        return Ok(WholeFileMutationResult::Conflict);
    }
    let Some(target) = namespace.nodes.get_mut(&mutation.inode) else {
        return Ok(WholeFileMutationResult::Conflict);
    };
    if !mutation
        .original
        .as_ref()
        .is_some_and(|original| write_base_unchanged(target, original))
    {
        return Ok(WholeFileMutationResult::Conflict);
    }
    target.data = NodeData::File(mutation.layout.clone());
    set_file_size(&mut target.stats, mutation.data_length);
    touch_modified(&mut target.stats, concurrent)?;
    Ok(WholeFileMutationResult::Committed)
}

fn apply_unlink_mutation<M, B>(
    runtime: &ChunkedFs<M, B>,
    namespace: &mut Namespace,
    path: &str,
) -> Result<()>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    let entry = walk(namespace, path, false, "unlink", 0)?;
    let inode = entry
        .node
        .ok_or_else(|| error_with_path(ErrorCode::Enoent, "unlink", &entry.path))?;
    let node = namespace
        .nodes
        .get(&inode)
        .ok_or_else(|| error_with_path(ErrorCode::Estale, "unlink", &entry.path))?;
    if matches!(node.data, NodeData::Directory { .. }) {
        return Err(error_with_path(ErrorCode::Eisdir, "unlink", &entry.path));
    }
    detach_entry(
        namespace,
        entry.parent,
        &entry.name,
        true,
        "unlink",
        &entry.path,
        runtime.inner.options.concurrent_writes,
    )?;
    runtime.reap_detached(namespace, inode)
}

fn validate_checkout_path(path: &str) -> Result<()> {
    if !path.starts_with('/') || path.contains('\0') {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("checkout path must be a nonempty absolute virtual path without NUL"));
    }
    Ok(())
}

fn initial_namespace(options: &ChunkedOptions) -> Result<Namespace> {
    // Capture the serialized configuration once. Besides avoiding needless
    // work, this makes validation and persistence one logical decision for a
    // custom chunker whose config is computed at runtime.
    let default_chunker = options.chunker.config();
    from_config(&default_chunker)?;
    let timestamp = now_ms();
    let root_mode = S_IFDIR | (options.root_mode & 0o7777);
    let mut nodes = BTreeMap::new();
    nodes.insert(
        1,
        NodeMetadata {
            stats: Stats {
                dev: 0,
                ino: 1,
                mode: root_mode,
                nlink: 2,
                uid: options.uid,
                gid: options.gid,
                rdev: 0,
                size: BLOCK_SIZE,
                blksize: BLOCK_SIZE,
                blocks: BLOCK_SIZE.div_ceil(512),
                atime_ms: timestamp,
                mtime_ms: timestamp,
                ctime_ms: timestamp,
                birthtime_ms: timestamp,
            },
            data: NodeData::Directory {
                entries: Vec::new(),
            },
        },
    );
    Ok(Namespace {
        format_version: NAMESPACE_FORMAT_VERSION,
        root: 1,
        next_inode: 2,
        default_uid: options.uid,
        default_gid: options.gid,
        umask: options.umask,
        default_chunker,
        nodes,
    })
}

fn new_file_node(
    inode: InodeId,
    mode: u32,
    uid: u32,
    gid: u32,
    chunker: mount_rs_core::chunking::ChunkerConfig,
) -> NodeMetadata {
    let timestamp = now_ms();
    NodeMetadata {
        stats: base_stats(inode, mode, uid, gid, 1, 0, 0),
        data: NodeData::File(FileLayout {
            chunker,
            extents: Vec::new(),
        }),
    }
    .with_times(timestamp)
}

fn new_directory_node(inode: InodeId, mode: u32, uid: u32, gid: u32) -> NodeMetadata {
    let timestamp = now_ms();
    NodeMetadata {
        stats: base_stats(
            inode,
            mode,
            uid,
            gid,
            2,
            BLOCK_SIZE,
            BLOCK_SIZE.div_ceil(512),
        ),
        data: NodeData::Directory {
            entries: Vec::new(),
        },
    }
    .with_times(timestamp)
}

trait NodeMetadataTimes {
    fn with_times(self, timestamp: i64) -> Self;
}

impl NodeMetadataTimes for NodeMetadata {
    fn with_times(mut self, timestamp: i64) -> Self {
        self.stats.atime_ms = timestamp;
        self.stats.mtime_ms = timestamp;
        self.stats.ctime_ms = timestamp;
        self.stats.birthtime_ms = timestamp;
        self
    }
}

fn base_stats(
    inode: InodeId,
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u64,
    size: u64,
    blocks: u64,
) -> Stats {
    Stats {
        dev: 0,
        ino: inode,
        mode,
        nlink,
        uid,
        gid,
        rdev: 0,
        size,
        blksize: BLOCK_SIZE,
        blocks,
        atime_ms: 0,
        mtime_ms: 0,
        ctime_ms: 0,
        birthtime_ms: 0,
    }
}

fn collect_block_roots(
    namespace: &Namespace,
    live: &mut BTreeSet<mount_rs_core::storage::BlockId>,
) {
    for node in namespace.nodes.values() {
        collect_block_roots_from_node(node, live);
    }
}

fn collect_block_roots_from_node(
    node: &NodeMetadata,
    live: &mut BTreeSet<mount_rs_core::storage::BlockId>,
) {
    let NodeData::File(layout) = &node.data else {
        return;
    };
    live.extend(layout.extents.iter().map(|extent| extent.block.clone()));
}

fn set_file_size(stats: &mut Stats, size: u64) {
    stats.size = size;
    stats.blocks = size.div_ceil(512);
}

fn touch_modified(stats: &mut Stats, concurrent: bool) -> Result<()> {
    touch_modified_at(stats, now_ms(), concurrent)
}

fn touch_changed(stats: &mut Stats, concurrent: bool) -> Result<()> {
    touch_changed_at(stats, now_ms(), concurrent)
}

fn touch_changed_at(stats: &mut Stats, now: i64, concurrent: bool) -> Result<()> {
    stats.ctime_ms = if concurrent {
        next_concurrent_time(stats.ctime_ms, now, "file change time overflow")?
    } else {
        now
    };
    Ok(())
}

fn next_concurrent_time(prior: i64, now: i64, overflow_message: &str) -> Result<i64> {
    let next = prior
        .checked_add(1)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow).with_message(overflow_message))?;
    Ok(now.max(next))
}

fn touch_modified_at(stats: &mut Stats, now: i64, concurrent: bool) -> Result<()> {
    if !concurrent {
        // Preserve the original exclusive-mode semantics, including a write
        // after explicit future utimes resetting mtime to the write time.
        stats.mtime_ms = now;
        stats.ctime_ms = now;
        return Ok(());
    }
    // Concurrent CAS writers can commit inside one millisecond or use host
    // clocks with skew. Distinct persisted mtimes prevent NFS clients from
    // treating changed data and names as an unchanged cached object.
    let next_mtime = next_concurrent_time(stats.mtime_ms, now, "file modification time overflow")?;
    let next_ctime = next_concurrent_time(stats.ctime_ms, now, "file change time overflow")?;
    stats.mtime_ms = next_mtime;
    stats.ctime_ms = next_ctime;
    Ok(())
}

fn stale_guard(path: &str, syscall: &str) -> FsError {
    error_with_path(ErrorCode::Estale, syscall, path)
        .with_message("opaque handle no longer names its original inode")
}

fn check_path_guard(namespace: &Namespace, guard: &PathGuard, syscall: &str) -> Result<InodeId> {
    let path = normalize_path(&guard.path);
    if guard.identity.ino == 0 {
        return Err(stale_guard(&path, syscall));
    }
    let inode =
        resolve(namespace, &path, false, syscall).map_err(|_| stale_guard(&path, syscall))?;
    let node = namespace
        .nodes
        .get(&inode)
        .ok_or_else(|| stale_guard(&path, syscall))?;
    if node.stats.dev != guard.identity.dev || node.stats.ino != guard.identity.ino {
        return Err(stale_guard(&path, syscall));
    }
    Ok(inode)
}

fn guarded_child_path(parent: &PathGuard, name: &str, syscall: &str) -> Result<String> {
    if name.is_empty() || name.contains('/') || name.contains('\0') || matches!(name, "." | "..") {
        return Err(error_with_path(ErrorCode::Einval, syscall, name));
    }
    Ok(normalize_path(&format!("{}/{name}", parent.path)))
}

fn check_observed_entry(
    namespace: &Namespace,
    inode: Option<InodeId>,
    observed: ObservedEntry,
    path: &str,
    syscall: &str,
) -> Result<()> {
    match observed {
        ObservedEntry::Any => Ok(()),
        ObservedEntry::Absent if inode.is_none() => Ok(()),
        ObservedEntry::Identity(identity) if identity.ino != 0 => {
            let node = inode
                .and_then(|inode| namespace.nodes.get(&inode))
                .ok_or_else(|| stale_guard(path, syscall))?;
            if node.stats.dev == identity.dev && node.stats.ino == identity.ino {
                Ok(())
            } else {
                Err(stale_guard(path, syscall))
            }
        }
        _ => Err(stale_guard(path, syscall)),
    }
}

fn guarded_child_entry(
    namespace: &Namespace,
    parent: &PathGuard,
    name: &str,
    observed: ObservedEntry,
    syscall: &str,
) -> Result<Entry> {
    let parent_inode = check_path_guard(namespace, parent, syscall)?;
    let parent_node = namespace
        .nodes
        .get(&parent_inode)
        .ok_or_else(|| stale_guard(&parent.path, syscall))?;
    if !matches!(parent_node.data, NodeData::Directory { .. }) {
        return Err(stale_guard(&parent.path, syscall));
    }
    let path = guarded_child_path(parent, name, syscall)?;
    let entry = walk(namespace, &path, false, syscall, 0)?;
    if entry.parent != parent_inode {
        return Err(stale_guard(&parent.path, syscall));
    }
    check_observed_entry(namespace, entry.node, observed, &path, syscall)?;
    Ok(entry)
}

fn apply_guarded_setattr(
    namespace: &mut Namespace,
    target: &PathGuard,
    change: GuardedSetattr,
    concurrent: bool,
) -> Result<()> {
    let inode = check_path_guard(namespace, target, "setattr")?;
    let node = namespace
        .nodes
        .get_mut(&inode)
        .ok_or_else(|| stale_guard(&target.path, "setattr"))?;
    if change
        .expected_ctime_ms
        .is_some_and(|expected| node.stats.ctime_ms != expected)
    {
        return Err(error_with_path(ErrorCode::Eagain, "setattr", &target.path)
            .with_message("change time guard does not match the current inode"));
    }
    let mut metadata_changed = false;
    if let Some(mode) = change.mode {
        node.stats.mode = (node.stats.mode & S_IFMT) | (mode & 0o7777);
        metadata_changed = true;
    }
    if let Some(uid) = change.uid
        && uid != u32::MAX
    {
        node.stats.uid = uid;
        metadata_changed = true;
    }
    if let Some(gid) = change.gid
        && gid != u32::MAX
    {
        node.stats.gid = gid;
        metadata_changed = true;
    }
    let size_changed = if let Some(length) = change.size {
        let layout = match &mut node.data {
            NodeData::File(layout) => layout,
            NodeData::Directory { .. } => {
                return Err(error_with_path(ErrorCode::Eisdir, "setattr", &target.path));
            }
            NodeData::Special => {
                return Err(error_with_path(ErrorCode::Einval, "setattr", &target.path));
            }
            NodeData::Symlink { .. } => {
                return Err(error_with_path(ErrorCode::Eio, "setattr", &target.path));
            }
        };
        if length < node.stats.size {
            trim_extents(&mut layout.extents, length)?;
        }
        set_file_size(&mut node.stats, length);
        touch_modified(&mut node.stats, concurrent)?;
        true
    } else {
        false
    };
    if let Some(atime_ms) = change.atime_ms {
        node.stats.atime_ms = atime_ms;
        metadata_changed = true;
    }
    if let Some(mtime_ms) = change.mtime_ms {
        node.stats.mtime_ms = mtime_ms;
        metadata_changed = true;
    }
    if metadata_changed && !size_changed {
        touch_changed(&mut node.stats, concurrent)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Entry {
    parent: InodeId,
    name: String,
    node: Option<InodeId>,
    path: String,
}

fn walk(
    namespace: &Namespace,
    path: &str,
    follow_final: bool,
    syscall: &str,
    depth: usize,
) -> Result<Entry> {
    if depth > MAX_SYMLINK_DEPTH {
        return Err(error_with_path(
            ErrorCode::Eloop,
            syscall,
            &normalize_path(path),
        ));
    }
    let normalized = normalize_path(path);
    if normalized == "/" {
        return Ok(Entry {
            parent: namespace.root,
            name: String::new(),
            node: Some(namespace.root),
            path: normalized,
        });
    }
    let segments = split_path(&normalized);
    let mut current = namespace.root;
    for (index, name) in segments.iter().enumerate() {
        let current_node = namespace
            .nodes
            .get(&current)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, &normalized))?;
        let entries = match &current_node.data {
            NodeData::Directory { entries } => entries,
            _ => return Err(error_with_path(ErrorCode::Enotdir, syscall, &normalized)),
        };
        let Some(child) = entries
            .iter()
            .find(|entry| entry.name == *name)
            .map(|entry| entry.inode)
        else {
            if index + 1 == segments.len() {
                return Ok(Entry {
                    parent: current,
                    name: name.clone(),
                    node: None,
                    path: normalized,
                });
            }
            return Err(error_with_path(ErrorCode::Enoent, syscall, &normalized));
        };
        let child_node = namespace
            .nodes
            .get(&child)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, &normalized))?;
        let last = index + 1 == segments.len();
        if matches!(child_node.data, NodeData::Symlink { .. }) && (!last || follow_final) {
            let target = match &child_node.data {
                NodeData::Symlink { target } => target.clone(),
                _ => unreachable!(),
            };
            if target.is_empty() {
                return Err(error_with_path(ErrorCode::Enoent, syscall, &normalized));
            }
            let parent_path = if index == 0 {
                "/".to_owned()
            } else {
                format!("/{}", segments[..index].join("/"))
            };
            let mut rewritten = if target.starts_with('/') {
                target
            } else {
                format!("{parent_path}/{target}")
            };
            if !last {
                rewritten.push('/');
                rewritten.push_str(&segments[index + 1..].join("/"));
            }
            return walk(namespace, &rewritten, follow_final, syscall, depth + 1);
        }
        if last {
            return Ok(Entry {
                parent: current,
                name: name.clone(),
                node: Some(child),
                path: normalized,
            });
        }
        current = child;
    }
    unreachable!()
}

fn resolve(
    namespace: &Namespace,
    path: &str,
    follow_final: bool,
    syscall: &str,
) -> Result<InodeId> {
    let entry = walk(namespace, path, follow_final, syscall, 0)?;
    entry
        .node
        .ok_or_else(|| error_with_path(ErrorCode::Enoent, syscall, &entry.path))
}

fn add_entry(
    namespace: &mut Namespace,
    parent: InodeId,
    name: String,
    inode: InodeId,
    syscall: &str,
    path: &str,
    concurrent: bool,
) -> Result<()> {
    let is_directory = namespace
        .nodes
        .get(&inode)
        .is_some_and(|node| matches!(node.data, NodeData::Directory { .. }));
    let parent_node = namespace
        .nodes
        .get_mut(&parent)
        .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, path))?;
    let entries = match &mut parent_node.data {
        NodeData::Directory { entries } => entries,
        _ => return Err(error_with_path(ErrorCode::Enotdir, syscall, path)),
    };
    if entries.iter().any(|entry| entry.name == name) {
        return Err(error_with_path(ErrorCode::Eexist, syscall, path));
    }
    entries.push(mount_rs_core::storage::DirectoryEntry { name, inode });
    if is_directory {
        parent_node.stats.nlink = parent_node
            .stats
            .nlink
            .checked_add(1)
            .ok_or_else(|| error_with_path(ErrorCode::Emlink, syscall, path))?;
    }
    touch_modified(&mut parent_node.stats, concurrent)?;
    Ok(())
}

fn detach_entry(
    namespace: &mut Namespace,
    parent: InodeId,
    name: &str,
    decrement_link: bool,
    syscall: &str,
    path: &str,
    concurrent: bool,
) -> Result<InodeId> {
    let inode = {
        let inode = namespace
            .nodes
            .get(&parent)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, path))?;
        let entries = match &inode.data {
            NodeData::Directory { entries } => entries,
            _ => return Err(error_with_path(ErrorCode::Enotdir, syscall, path)),
        };
        let index = entries
            .iter()
            .position(|entry| entry.name == name)
            .ok_or_else(|| error_with_path(ErrorCode::Enoent, syscall, path))?;
        let inode = entries[index].inode;
        let is_directory = namespace
            .nodes
            .get(&inode)
            .is_some_and(|node| matches!(node.data, NodeData::Directory { .. }));

        let parent_node = namespace
            .nodes
            .get_mut(&parent)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, path))?;
        let entries = match &mut parent_node.data {
            NodeData::Directory { entries } => entries,
            _ => return Err(error_with_path(ErrorCode::Enotdir, syscall, path)),
        };
        entries.remove(index);
        if is_directory {
            parent_node.stats.nlink = parent_node
                .stats
                .nlink
                .checked_sub(1)
                .ok_or_else(|| error_with_path(ErrorCode::Eio, syscall, path))?;
        }
        touch_modified(&mut parent_node.stats, concurrent)?;
        inode
    };
    if decrement_link
        && let Some(node) = namespace.nodes.get_mut(&inode)
        && !matches!(node.data, NodeData::Directory { .. })
    {
        node.stats.nlink = node
            .stats
            .nlink
            .checked_sub(1)
            .ok_or_else(|| error_with_path(ErrorCode::Eio, syscall, path))?;
        touch_changed(&mut node.stats, concurrent)?;
    }
    Ok(inode)
}

fn trim_extents(extents: &mut Vec<BlockExtent>, length: u64) -> Result<()> {
    let mut retained = Vec::with_capacity(extents.len());
    for mut extent in extents.drain(..) {
        let end = extent
            .file_offset
            .checked_add(extent.length)
            .ok_or_else(|| {
                FsError::new(ErrorCode::Eio).with_message("file extent range overflow")
            })?;
        if extent.file_offset >= length {
            continue;
        }
        if end > length {
            extent.length = length - extent.file_offset;
        }
        if extent.length > 0 {
            retained.push(extent);
        }
    }
    *extents = retained;
    Ok(())
}

fn replace_extent_range(
    extents: &mut Vec<BlockExtent>,
    start: u64,
    end: u64,
    replacement: Option<BlockExtent>,
) -> Result<()> {
    if start >= end {
        return Err(
            FsError::new(ErrorCode::Einval).with_message("invalid extent replacement range")
        );
    }
    let capacity = extents
        .len()
        .checked_add(usize::from(replacement.is_some()))
        .ok_or_else(|| FsError::new(ErrorCode::Enomem).with_message("extent list is too large"))?;
    let mut kept = Vec::with_capacity(capacity);
    for extent in extents.drain(..) {
        let extent_end = extent
            .file_offset
            .checked_add(extent.length)
            .ok_or_else(|| {
                FsError::new(ErrorCode::Eio).with_message("file extent range overflow")
            })?;
        if extent_end <= start || extent.file_offset >= end {
            kept.push(extent);
            continue;
        }
        if extent.file_offset < start {
            let length = start - extent.file_offset;
            kept.push(BlockExtent {
                file_offset: extent.file_offset,
                block: extent.block.clone(),
                block_offset: extent.block_offset,
                length,
            });
        }
        if extent_end > end {
            let delta = end - extent.file_offset;
            kept.push(BlockExtent {
                file_offset: end,
                block: extent.block,
                block_offset: extent.block_offset.checked_add(delta).ok_or_else(|| {
                    FsError::new(ErrorCode::Eio).with_message("block range overflow")
                })?,
                length: extent_end - end,
            });
        }
    }
    if let Some(replacement) = replacement {
        kept.push(replacement);
    }
    kept.sort_by_key(|extent| extent.file_offset);
    *extents = kept;
    Ok(())
}

async fn read_layout_into<B: BlockStore>(
    blocks: &Arc<B>,
    layout: &FileLayout,
    file_size: u64,
    position: u64,
    buffer: &mut [u8],
    path: &str,
    syscall: &str,
) -> Result<()> {
    buffer.fill(0);
    let buffer_length = u64::try_from(buffer.len())
        .map_err(|_| error_with_path(ErrorCode::Efbig, syscall, path))?;
    let end = position
        .checked_add(buffer_length)
        .ok_or_else(|| error_with_path(ErrorCode::Efbig, syscall, path))?;
    for extent in &layout.extents {
        let extent_end = extent
            .file_offset
            .checked_add(extent.length)
            .ok_or_else(|| error_with_path(ErrorCode::Eio, syscall, path))?;
        let start = position.max(extent.file_offset);
        let copy_end = end.min(extent_end).min(file_size);
        if start >= copy_end {
            continue;
        }
        let bytes = blocks
            .get(&extent.block)
            .await
            .map_err(|error| with_context(error, syscall, Some(path)))?;
        let source_offset = extent
            .block_offset
            .checked_add(start - extent.file_offset)
            .ok_or_else(|| error_with_path(ErrorCode::Eio, syscall, path))?;
        let copy_length = usize::try_from(copy_end - start)
            .map_err(|_| error_with_path(ErrorCode::Efbig, syscall, path))?;
        let source_offset = usize::try_from(source_offset)
            .map_err(|_| error_with_path(ErrorCode::Eio, syscall, path))?;
        let destination_offset = usize::try_from(start - position)
            .map_err(|_| error_with_path(ErrorCode::Efbig, syscall, path))?;
        let source_end = source_offset
            .checked_add(copy_length)
            .ok_or_else(|| error_with_path(ErrorCode::Eio, syscall, path))?;
        let destination_end = destination_offset
            .checked_add(copy_length)
            .ok_or_else(|| error_with_path(ErrorCode::Eio, syscall, path))?;
        if source_end > bytes.len() || destination_end > buffer.len() {
            return Err(error_with_path(ErrorCode::Eio, syscall, path)
                .with_message("block extent exceeds immutable block length"));
        }
        buffer[destination_offset..destination_end]
            .copy_from_slice(&bytes[source_offset..source_end]);
    }
    Ok(())
}

async fn read_layout_all<B: BlockStore>(
    blocks: &Arc<B>,
    layout: &FileLayout,
    file_size: u64,
    path: &str,
) -> Result<Vec<u8>> {
    let length =
        usize::try_from(file_size).map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| error_with_path(ErrorCode::Enomem, "write", path))?;
    bytes.resize(length, 0);
    read_layout_into(blocks, layout, file_size, 0, &mut bytes, path, "write").await?;
    Ok(bytes)
}

async fn rewrite_layout<B: BlockStore>(
    blocks: &Arc<B>,
    layout: &FileLayout,
    old_size: u64,
    position: u64,
    input: &[u8],
    new_size: u64,
    path: &str,
) -> Result<FileLayout> {
    let chunk_size = fixed_chunk_size(&layout.chunker)?;
    if let Some(chunk_size) = chunk_size {
        let size = u64::try_from(chunk_size)
            .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
        let first = position / size;
        let input_length = u64::try_from(input.len())
            .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
        let input_end = position
            .checked_add(input_length)
            .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
        let last = (input_end - 1) / size;
        let mut extents = layout.extents.clone();
        for index in first..=last {
            let chunk_start = index
                .checked_mul(size)
                .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
            let chunk_length = usize::try_from(
                new_size
                    .checked_sub(chunk_start)
                    .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?
                    .min(size),
            )
            .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
            let chunk_length_u64 = u64::try_from(chunk_length)
                .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
            let mut chunk = Vec::new();
            chunk
                .try_reserve_exact(chunk_length)
                .map_err(|_| error_with_path(ErrorCode::Enomem, "write", path))?;
            chunk.resize(chunk_length, 0);
            let chunk_end = chunk_start
                .checked_add(chunk_length_u64)
                .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
            // Read existing bytes only when the input leaves part of the
            // resulting chunk untouched, including a short chunk at EOF.
            if position > chunk_start || input_end < chunk_end {
                let old_read_profile = Span::new(Event::RewriteRead).units(chunk_length as u64);
                read_layout_into(
                    blocks,
                    layout,
                    old_size,
                    chunk_start,
                    &mut chunk,
                    path,
                    "write",
                )
                .await?;
                drop(old_read_profile);
            }
            let write_start = position.max(chunk_start);
            let write_end = input_end.min(chunk_end);
            let destination = usize::try_from(write_start - chunk_start)
                .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
            let source = usize::try_from(write_start - position)
                .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
            let count = usize::try_from(write_end - write_start)
                .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
            let destination_end = destination
                .checked_add(count)
                .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
            let source_end = source
                .checked_add(count)
                .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
            if destination_end > chunk.len() || source_end > input.len() {
                return Err(error_with_path(ErrorCode::Eio, "write", path)
                    .with_message("chunk write range exceeds buffer"));
            }
            chunk[destination..destination_end].copy_from_slice(&input[source..source_end]);
            let replacement = if chunk.iter().any(|byte| *byte != 0) {
                let block = blocks
                    .put(&chunk)
                    .await
                    .map_err(|error| with_context(error, "block-put", Some(path)))?;
                Some(BlockExtent {
                    file_offset: chunk_start,
                    block,
                    block_offset: 0,
                    length: chunk_length_u64,
                })
            } else {
                None
            };
            replace_extent_range(&mut extents, chunk_start, chunk_end, replacement)?;
        }
        return Ok(FileLayout {
            chunker: layout.chunker.clone(),
            extents,
        });
    }

    let chunker = from_config(&layout.chunker)?;
    let mut bytes = read_layout_all(blocks, layout, old_size, path).await?;
    let new_length =
        usize::try_from(new_size).map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
    let additional = new_length
        .checked_sub(bytes.len())
        .ok_or_else(|| error_with_path(ErrorCode::Eio, "write", path))?;
    bytes
        .try_reserve_exact(additional)
        .map_err(|_| error_with_path(ErrorCode::Enomem, "write", path))?;
    bytes.resize(new_length, 0);
    let start =
        usize::try_from(position).map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
    let input_end = start
        .checked_add(input.len())
        .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
    if input_end > bytes.len() {
        return Err(error_with_path(ErrorCode::Efbig, "write", path));
    }
    bytes[start..input_end].copy_from_slice(input);
    let mut extents = Vec::new();
    let mut offset = 0_u64;
    for chunk in chunker.chunks(&bytes) {
        if chunk.is_empty() {
            return Err(error_with_path(ErrorCode::Eio, "write", path));
        }
        let length = u64::try_from(chunk.len())
            .map_err(|_| error_with_path(ErrorCode::Efbig, "write", path))?;
        if chunk.iter().any(|byte| *byte != 0) {
            let block = blocks
                .put(chunk)
                .await
                .map_err(|error| with_context(error, "block-put", Some(path)))?;
            extents.push(BlockExtent {
                file_offset: offset,
                block,
                block_offset: 0,
                length,
            });
        }
        offset = offset
            .checked_add(length)
            .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
    }
    if offset != new_size {
        return Err(error_with_path(ErrorCode::Eio, "write", path)
            .with_message("chunker did not cover the complete file"));
    }
    Ok(FileLayout {
        chunker: layout.chunker.clone(),
        extents,
    })
}

fn fixed_chunk_size(config: &mount_rs_core::chunking::ChunkerConfig) -> Result<Option<usize>> {
    if config.algorithm != "fixed-size" {
        return Ok(None);
    }
    let chunker = from_config(config)?;
    let size = config
        .parameters
        .get("chunk_size")
        .and_then(|value| usize::try_from(*value).ok())
        .ok_or_else(|| FsError::new(ErrorCode::Einval).with_message("invalid chunk_size"))?;
    if chunker.config() != *config {
        return Err(FsError::new(ErrorCode::Eproto).with_message("chunker configuration mismatch"));
    }
    Ok(Some(size))
}

fn error_with_path(code: ErrorCode, syscall: &str, path: &str) -> FsError {
    FsError::new(code).with_syscall(syscall).with_path(path)
}

fn with_context(error: FsError, syscall: &str, path: Option<&str>) -> FsError {
    let message = error.to_string();
    let mut result = FsError::new(error.code).with_syscall(syscall);
    if let Some(path) = path {
        result = result.with_path(path);
    }
    result.with_message(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use mount_rs_core::storage::{LoadedMetadata, MetadataStore, NodeData, WriterLease};
    use mount_rs_memory::{ManualClock, MemoryBlockStore, MemoryMetadataStore};
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    struct NoopWaker;

    impl Wake for NoopWaker {
        fn wake(self: Arc<Self>) {}
        fn wake_by_ref(self: &Arc<Self>) {}
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match Future::poll(future.as_mut(), &mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn block_on_all<F: Future>(futures: Vec<Pin<Box<F>>>) -> Vec<F::Output> {
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut futures: Vec<_> = futures.into_iter().map(Some).collect();
        let mut results: Vec<Option<F::Output>> = (0..futures.len()).map(|_| None).collect();
        let mut remaining = futures.len();
        while remaining > 0 {
            for index in 0..futures.len() {
                let Some(future) = futures[index].as_mut() else {
                    continue;
                };
                if let Poll::Ready(value) = Future::poll(future.as_mut(), &mut context) {
                    futures[index] = None;
                    results[index] = Some(value);
                    remaining -= 1;
                }
            }
        }
        results
            .into_iter()
            .map(|result| result.expect("all futures must complete"))
            .collect()
    }

    /// A test-only shared backing: both coordinators retain the same inner
    /// in-memory block map when this wrapper is cloned.
    #[derive(Clone)]
    struct SharedTestBlockStore {
        inner: MemoryBlockStore,
        backing: Arc<Mutex<Option<ConcurrentBackingId>>>,
        preparations: Arc<AtomicUsize>,
        before_get: Arc<Mutex<Option<FlushHook>>>,
    }

    impl SharedTestBlockStore {
        fn new() -> Self {
            Self {
                inner: MemoryBlockStore::new(),
                backing: Arc::new(Mutex::new(None)),
                preparations: Arc::new(AtomicUsize::new(0)),
                before_get: Arc::new(Mutex::new(None)),
            }
        }
    }

    fn test_concurrent_backing() -> ConcurrentBackingId {
        ConcurrentBackingId::from_bytes([0xc1; 16]).expect("fixed nonzero backing ID")
    }

    #[async_trait]
    impl BlockStore for SharedTestBlockStore {
        fn durable(&self) -> bool {
            self.inner.durable()
        }

        async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
            self.preparations.fetch_add(1, Ordering::SeqCst);
            Ok(*self
                .backing
                .lock()
                .expect("block authority lock")
                .get_or_insert(test_concurrent_backing()))
        }

        async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
            if *self.backing.lock().expect("block authority lock") == Some(expected) {
                Ok(())
            } else {
                Err(FsError::new(ErrorCode::Estale))
            }
        }

        async fn put(&self, bytes: &[u8]) -> Result<mount_rs_core::storage::BlockId> {
            self.inner.put(bytes).await
        }

        async fn get(&self, id: &mount_rs_core::storage::BlockId) -> Result<Vec<u8>> {
            let hook = self.before_get.lock().expect("block get hook").take();
            if let Some(hook) = hook {
                hook();
            }
            self.inner.get(id).await
        }

        async fn flush(&self) -> Result<()> {
            self.inner.flush().await
        }

        async fn delete(&self, id: &mount_rs_core::storage::BlockId) -> Result<()> {
            self.inner.delete(id).await
        }
    }

    #[derive(Clone)]
    struct FaultBlockStore {
        inner: MemoryBlockStore,
        fail_put: Arc<AtomicBool>,
        pause_flush: Arc<AtomicBool>,
        fail_flush: Arc<AtomicBool>,
        gets: Arc<AtomicUsize>,
        reconciled: Arc<Mutex<Option<BTreeSet<mount_rs_core::storage::BlockId>>>>,
    }

    impl FaultBlockStore {
        fn new() -> Self {
            Self {
                inner: MemoryBlockStore::new(),
                fail_put: Arc::new(AtomicBool::new(false)),
                pause_flush: Arc::new(AtomicBool::new(false)),
                fail_flush: Arc::new(AtomicBool::new(false)),
                gets: Arc::new(AtomicUsize::new(0)),
                reconciled: Arc::new(Mutex::new(None)),
            }
        }
    }

    #[test]
    fn fixed_rewrite_reads_only_chunks_with_preserved_bytes() {
        let mut cross_chunk_expected = vec![1; 12 * 1024];
        cross_chunk_expected[2048..2048 + 8192].fill(2);
        let cases = [
            (vec![1; 4096], 0, vec![2; 4096], vec![2; 4096], 0),
            (vec![1; 100], 0, vec![2; 100], vec![2; 100], 0),
            (vec![1; 4096], 0, vec![0; 4096], vec![0; 4096], 0),
            (vec![1, 2, 3, 4], 1, vec![9, 8], vec![1, 9, 8, 4], 1),
            (vec![1, 2], 4, vec![9, 8], vec![1, 2, 0, 0, 9, 8], 1),
            (vec![], 4, vec![9, 8], vec![0, 0, 0, 0, 9, 8], 0),
            (
                vec![1; 12 * 1024],
                2048,
                vec![2; 8192],
                cross_chunk_expected,
                2,
            ),
        ];
        for (old, position, input, expected, gets) in cases {
            let blocks = Arc::new(FaultBlockStore::new());
            let extents = old
                .chunks(4096)
                .enumerate()
                .map(|(index, chunk)| BlockExtent {
                    file_offset: (index * 4096) as u64,
                    block: block_on(blocks.put(chunk)).unwrap(),
                    block_offset: 0,
                    length: chunk.len() as u64,
                })
                .collect();
            let layout = FileLayout {
                chunker: FixedSizeChunker::new(4096).unwrap().config(),
                extents,
            };
            let rewritten = block_on(rewrite_layout(
                &blocks,
                &layout,
                old.len() as u64,
                position,
                &input,
                expected.len() as u64,
                "/rewrite",
            ))
            .unwrap();
            assert_eq!(
                blocks.gets.load(Ordering::SeqCst),
                gets,
                "old-block reads at position {position} for {} input bytes",
                input.len()
            );
            assert_eq!(
                block_on(read_layout_all(
                    &blocks,
                    &rewritten,
                    expected.len() as u64,
                    "/rewrite",
                ))
                .unwrap(),
                expected
            );
        }
    }

    type FlushHook = Box<dyn FnOnce() + Send>;
    type PreflightHook = Box<dyn FnOnce() -> Result<()> + Send>;

    #[derive(Clone)]
    struct RevisionRaceMetadata {
        state: Arc<Mutex<LoadedMetadata>>,
        mode: Arc<Mutex<ConcurrentModeState>>,
        on_flush: Arc<Mutex<Option<FlushHook>>>,
        on_preflight: Arc<Mutex<Option<PreflightHook>>>,
        loads: Arc<AtomicUsize>,
    }

    impl RevisionRaceMetadata {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(LoadedMetadata {
                    revision: 0,
                    namespace: None,
                })),
                mode: Arc::new(Mutex::new(ConcurrentModeState::Legacy)),
                on_flush: Arc::new(Mutex::new(None)),
                on_preflight: Arc::new(Mutex::new(None)),
                loads: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn on_next_flush(&self, callback: impl FnOnce() + Send + 'static) {
            *self.on_flush.lock().expect("flush hook lock") = Some(Box::new(callback));
        }

        fn on_next_preflight(&self, callback: impl FnOnce() -> Result<()> + Send + 'static) {
            *self.on_preflight.lock().expect("preflight hook lock") = Some(Box::new(callback));
        }

        fn apply_revision(&self, expected_revision: u64, namespace: Namespace) -> Result<u64> {
            namespace.validate()?;
            let mut state = self.state.lock().expect("revision state lock");
            if state.revision != expected_revision {
                return Err(FsError::new(ErrorCode::Eagain));
            }
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
            state.namespace = Some(namespace);
            Ok(state.revision)
        }
    }

    #[async_trait]
    impl MetadataStore for RevisionRaceMetadata {
        fn durable(&self) -> bool {
            false
        }

        async fn load(&self) -> Result<LoadedMetadata> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            Ok(self.state.lock().expect("revision state lock").clone())
        }

        async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
            Ok(*self.mode.lock().expect("concurrent mode lock"))
        }

        async fn preflight_new_bound_mode(&self) -> Result<()> {
            let hook = self
                .on_preflight
                .lock()
                .expect("preflight hook lock")
                .take();
            if let Some(hook) = hook {
                hook()?;
            }
            if *self.mode.lock().expect("concurrent mode lock") == ConcurrentModeState::Legacy {
                Ok(())
            } else {
                Err(FsError::new(ErrorCode::Ebusy))
            }
        }

        async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
            let mut mode = self.mode.lock().expect("concurrent mode lock");
            match *mode {
                ConcurrentModeState::Legacy => {
                    *mode = ConcurrentModeState::Mrc2(backing);
                    Ok(())
                }
                ConcurrentModeState::Mrc2(current) if current == backing => Ok(()),
                ConcurrentModeState::Mrc2(_) => Err(FsError::new(ErrorCode::Estale)),
                ConcurrentModeState::Mrc1 => Err(FsError::new(ErrorCode::Ebusy)),
            }
        }

        async fn acquire_writer(&self, _owner: &str, _ttl: Duration) -> Result<WriterLease> {
            Err(FsError::new(ErrorCode::Enotsup))
        }

        async fn renew_writer(&self, _lease: &WriterLease, _ttl: Duration) -> Result<WriterLease> {
            Err(FsError::new(ErrorCode::Enotsup))
        }

        async fn release_writer(&self, _lease: &WriterLease) -> Result<()> {
            Err(FsError::new(ErrorCode::Enotsup))
        }

        async fn publish(
            &self,
            _expected_revision: u64,
            _lease: &WriterLease,
            _namespace: Namespace,
        ) -> Result<u64> {
            Err(FsError::new(ErrorCode::Enotsup))
        }

        async fn publish_bound_if_revision(
            &self,
            backing: ConcurrentBackingId,
            expected_revision: u64,
            namespace: Namespace,
        ) -> Result<u64> {
            if *self.mode.lock().expect("concurrent mode lock")
                != ConcurrentModeState::Mrc2(backing)
            {
                return Err(FsError::new(ErrorCode::Estale));
            }
            self.apply_revision(expected_revision, namespace)
        }

        async fn flush(&self) -> Result<()> {
            let hook = self.on_flush.lock().expect("flush hook lock").take();
            if let Some(hook) = hook {
                hook();
            }
            Ok(())
        }
    }

    #[derive(Clone)]
    struct ConditionalRevisionMetadata {
        inner: RevisionRaceMetadata,
        conditional_loads: Arc<AtomicUsize>,
        after_load: Arc<Mutex<Option<FlushHook>>>,
    }

    impl ConditionalRevisionMetadata {
        fn new() -> Self {
            Self {
                inner: RevisionRaceMetadata::new(),
                conditional_loads: Arc::new(AtomicUsize::new(0)),
                after_load: Arc::new(Mutex::new(None)),
            }
        }

        fn after_next_load(&self, callback: impl FnOnce() + Send + 'static) {
            *self.after_load.lock().expect("conditional load hook") = Some(Box::new(callback));
        }
    }

    #[async_trait]
    impl MetadataStore for ConditionalRevisionMetadata {
        fn durable(&self) -> bool {
            false
        }

        async fn load(&self) -> Result<LoadedMetadata> {
            self.inner.load().await
        }

        async fn load_if_changed(&self, known_revision: u64) -> Result<Option<LoadedMetadata>> {
            self.conditional_loads.fetch_add(1, Ordering::SeqCst);
            let loaded = {
                let state = self.inner.state.lock().expect("revision state lock");
                if known_revision != 0 && state.revision == known_revision {
                    None
                } else {
                    self.inner.loads.fetch_add(1, Ordering::SeqCst);
                    Some(state.clone())
                }
            };
            let hook = self
                .after_load
                .lock()
                .expect("conditional load hook")
                .take();
            if let Some(hook) = hook {
                hook();
            }
            Ok(loaded)
        }

        async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
            self.inner.concurrent_mode_state().await
        }

        async fn preflight_new_bound_mode(&self) -> Result<()> {
            self.inner.preflight_new_bound_mode().await
        }

        async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
            self.inner.prepare_bound_concurrent_mode(backing).await
        }

        async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
            self.inner.acquire_writer(owner, ttl).await
        }

        async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
            self.inner.renew_writer(lease, ttl).await
        }

        async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
            self.inner.release_writer(lease).await
        }

        async fn publish(
            &self,
            expected_revision: u64,
            lease: &WriterLease,
            namespace: Namespace,
        ) -> Result<u64> {
            self.inner
                .publish(expected_revision, lease, namespace)
                .await
        }

        async fn publish_bound_if_revision(
            &self,
            backing: ConcurrentBackingId,
            expected_revision: u64,
            namespace: Namespace,
        ) -> Result<u64> {
            self.inner
                .publish_bound_if_revision(backing, expected_revision, namespace)
                .await
        }

        async fn flush(&self) -> Result<()> {
            self.inner.flush().await
        }
    }

    fn conditional_filesystem(
        metadata: &ConditionalRevisionMetadata,
    ) -> ChunkedFs<ConditionalRevisionMetadata, SharedTestBlockStore> {
        block_on(ChunkedFs::open(
            metadata.clone(),
            SharedTestBlockStore::new(),
            ChunkedOptions::fixed("conditional", 4)
                .expect("chunker")
                .with_concurrent_writes(true),
        ))
        .expect("open concurrent filesystem")
    }

    #[test]
    #[ignore = "run alone with MOUNT_RS_PROFILE_IO=1 and --test-threads=1"]
    fn concurrent_read_profiles_only_requested_inode() {
        assert!(profile::enabled(), "start with MOUNT_RS_PROFILE_IO=1");
        let metadata = ConditionalRevisionMetadata::new();
        let fs = conditional_filesystem(&metadata);
        let content = vec![0x5a; 128];
        for index in 0..100 {
            block_on(fs.write_file(&format!("/file-{index}"), &content)).expect("create file");
        }
        let handle = block_on(fs.open("/file-0", "r", 0)).expect("open requested file");
        let before = profile::snapshot();
        let conditional_before = metadata.conditional_loads.load(Ordering::SeqCst);
        let mut bytes = [0; 4];
        assert_eq!(block_on(handle.read(&mut bytes, Some(0))).expect("read"), 4);
        assert_eq!(bytes, [0x5a; 4]);
        let delta = profile::snapshot().delta(&before).expect("profile delta");
        let cloned_nodes: u64 = delta
            .entries
            .iter()
            .filter(|entry| entry.name == "filesystem.snapshot_nodes")
            .map(|entry| entry.units)
            .sum();
        assert!(
            cloned_nodes == 0,
            "one inode read cloned {cloned_nodes} namespace nodes"
        );
        assert_eq!(
            metadata.conditional_loads.load(Ordering::SeqCst) - conditional_before,
            2,
            "read checks metadata before and after block I/O"
        );
        block_on(handle.close()).expect("close");
        block_on(fs.shutdown()).expect("shutdown");
    }

    #[test]
    fn concurrent_read_pins_original_namespace_until_second_freshness_check() {
        let metadata = ConditionalRevisionMetadata::new();
        let fs = conditional_filesystem(&metadata);
        block_on(fs.write_file("/file", b"old-data")).expect("create original");
        let handle = block_on(fs.open("/file", "r", 0)).expect("open file");
        let inode = block_on(handle.stat()).expect("stat").ino;
        let blocks = fs.block_store();
        let replacement = block_on(blocks.put(b"new!")).expect("replacement block");
        let (mut next, revision) = fs.snapshot().expect("published namespace");
        let NodeData::File(layout) = &mut next.nodes.get_mut(&inode).expect("file inode").data
        else {
            panic!("expected file");
        };
        for extent in &mut layout.extents {
            extent.block = replacement.clone();
        }
        let original = Arc::downgrade(&fs.lock_state().expect("state").namespace);
        let during_get = original.clone();
        let remote = metadata.inner.clone();
        *blocks.before_get.lock().expect("block get hook") = Some(Box::new(move || {
            assert_eq!(
                during_get.strong_count(),
                2,
                "state and in-flight read own metadata"
            );
            remote
                .apply_revision(revision, next)
                .expect("remote replacement during block I/O");
        }));
        let before = metadata.conditional_loads.load(Ordering::SeqCst);
        let mut bytes = [0; 8];
        assert_eq!(
            block_on(handle.read(&mut bytes, Some(0))).expect("original read"),
            8
        );
        assert_eq!(&bytes, b"old-data");
        assert_eq!(
            metadata.conditional_loads.load(Ordering::SeqCst) - before,
            2
        );
        assert_eq!(fs.lock_state().expect("state").revision, revision + 1);
        assert!(
            original.upgrade().is_none(),
            "completed read releases old revision"
        );
        assert_eq!(
            block_on(handle.read(&mut bytes, Some(0))).expect("updated read"),
            8
        );
        assert_eq!(&bytes, b"new!new!");
        block_on(handle.close()).expect("close");
        block_on(fs.shutdown()).expect("shutdown");
    }

    #[test]
    fn concurrent_read_checks_metadata_twice_including_eof() {
        let metadata = ConditionalRevisionMetadata::new();
        let fs = conditional_filesystem(&metadata);
        block_on(fs.write_file("/file", b"content")).expect("create file");
        let handle = block_on(fs.open("/file", "r", 0)).expect("open file");
        for (position, expected) in [(0, 4), (7, 0)] {
            let before = metadata.conditional_loads.load(Ordering::SeqCst);
            let mut bytes = [0; 4];
            assert_eq!(
                block_on(handle.read(&mut bytes, Some(position))).expect("read"),
                expected
            );
            assert_eq!(
                metadata.conditional_loads.load(Ordering::SeqCst) - before,
                2
            );
            if expected != 0 {
                assert_eq!(&bytes, b"cont");
            }
        }
        block_on(handle.close()).expect("close");
        block_on(fs.shutdown()).expect("shutdown");
    }

    #[test]
    fn concurrent_read_retains_detached_open_inode_bytes() {
        let metadata = ConditionalRevisionMetadata::new();
        let fs = conditional_filesystem(&metadata);
        block_on(fs.write_file("/file", b"content")).expect("create file");
        let handle = block_on(fs.open("/file", "r", 0)).expect("open file");
        let inode = block_on(handle.stat()).expect("stat").ino;
        // Exercise the retained detached-inode representation independently of
        // concurrent unlink's published tombstone representation.
        {
            let mut state = fs.lock_state().expect("state");
            let node = Arc::make_mut(&mut state.namespace)
                .nodes
                .remove(&inode)
                .expect("live inode");
            state.orphans.insert(inode, node);
        }
        let before = metadata.conditional_loads.load(Ordering::SeqCst);
        let mut bytes = [0; 7];
        assert_eq!(
            block_on(handle.read(&mut bytes, Some(0))).expect("orphan read"),
            7
        );
        assert_eq!(&bytes, b"content");
        assert_eq!(
            metadata.conditional_loads.load(Ordering::SeqCst) - before,
            2
        );
        block_on(handle.close()).expect("close orphan");
        block_on(fs.shutdown()).expect("shutdown");
    }

    #[test]
    fn concurrent_read_retains_failure_during_second_metadata_check() {
        for changed in [false, true] {
            let metadata = ConditionalRevisionMetadata::new();
            let fs = conditional_filesystem(&metadata);
            block_on(fs.write_file("/file", b"content")).expect("create file");
            let handle = block_on(fs.open("/file", "r", 0)).expect("open file");
            let second_check = metadata.clone();
            let failing = fs.clone();
            metadata.after_next_load(move || {
                if changed {
                    let mut remote = second_check.inner.state.lock().expect("remote state");
                    remote.revision += 1;
                    remote.namespace.as_mut().expect("namespace").default_uid = 77;
                }
                second_check.after_next_load(move || {
                    failing.fail_closed(
                        FsError::new(ErrorCode::Eio)
                            .with_message("failure during second read check"),
                    );
                });
            });
            let before = metadata.conditional_loads.load(Ordering::SeqCst);
            let mut bytes = [0; 7];
            let error = block_on(handle.read(&mut bytes, Some(0))).expect_err("second check fails");
            assert_eq!(error.code, ErrorCode::Eio);
            assert!(
                error
                    .to_string()
                    .contains("failure during second read check")
            );
            assert_eq!(&bytes, b"content", "block read precedes the second check");
            assert_eq!(
                metadata.conditional_loads.load(Ordering::SeqCst) - before,
                2
            );
            let repeated = block_on(handle.read(&mut bytes, Some(0))).expect_err("sticky failure");
            assert_eq!(repeated.code, ErrorCode::Eio);
            assert_eq!(
                metadata.conditional_loads.load(Ordering::SeqCst) - before,
                2
            );
            block_on(handle.close()).expect("close failed handle");
        }
    }

    #[test]
    fn concurrent_refresh_skips_unchanged_namespace_payload() {
        let metadata = ConditionalRevisionMetadata::new();
        let fs = conditional_filesystem(&metadata);
        assert_eq!(
            metadata.inner.loads.load(Ordering::SeqCst),
            1,
            "open fully loads"
        );
        for _ in 0..3 {
            block_on(fs.refresh_concurrent_namespace()).expect("unchanged refresh");
        }
        assert_eq!(metadata.conditional_loads.load(Ordering::SeqCst), 3);
        assert_eq!(metadata.inner.loads.load(Ordering::SeqCst), 1);
        block_on(fs.shutdown()).expect("shutdown");
    }

    #[test]
    fn concurrent_refresh_reloads_changed_namespace() {
        let metadata = ConditionalRevisionMetadata::new();
        let fs = conditional_filesystem(&metadata);
        let (mut namespace, revision) = fs.snapshot().expect("snapshot");
        namespace.default_uid = 77;
        let next = metadata
            .inner
            .apply_revision(revision, namespace)
            .expect("remote commit");
        block_on(fs.refresh_concurrent_namespace()).expect("changed refresh");
        let (namespace, revision) = fs.snapshot().expect("refreshed snapshot");
        assert_eq!(revision, next);
        assert_eq!(namespace.default_uid, 77);
        assert_eq!(metadata.inner.loads.load(Ordering::SeqCst), 2);
        block_on(fs.refresh_concurrent_namespace()).expect("now unchanged");
        assert_eq!(metadata.inner.loads.load(Ordering::SeqCst), 2);
        block_on(fs.shutdown()).expect("shutdown");
    }

    #[test]
    fn concurrent_refresh_validates_changed_namespace_and_retains_failure() {
        let metadata = ConditionalRevisionMetadata::new();
        let fs = conditional_filesystem(&metadata);
        {
            let mut state = metadata.inner.state.lock().expect("revision state");
            state.revision += 1;
            state.namespace.as_mut().expect("namespace").root = 0;
        }
        let error = block_on(fs.refresh_concurrent_namespace()).expect_err("invalid changed data");
        assert_eq!(error.code, ErrorCode::Einval);
        let calls = metadata.conditional_loads.load(Ordering::SeqCst);
        let repeated = block_on(fs.refresh_concurrent_namespace()).expect_err("failure retained");
        assert_eq!(repeated.code, error.code);
        assert_eq!(metadata.conditional_loads.load(Ordering::SeqCst), calls);
    }

    #[test]
    fn concurrent_refresh_retains_failure_after_either_conditional_response() {
        for changed in [false, true] {
            let metadata = ConditionalRevisionMetadata::new();
            let fs = conditional_filesystem(&metadata);
            if changed {
                let (namespace, revision) = fs.snapshot().expect("snapshot");
                metadata
                    .inner
                    .apply_revision(revision, namespace)
                    .expect("remote commit");
            }
            let failing = fs.clone();
            metadata.after_next_load(move || {
                failing
                    .fail_closed(FsError::new(ErrorCode::Eio).with_message("failure during load"));
            });
            let error =
                block_on(fs.refresh_concurrent_namespace()).expect_err("failure after await");
            assert_eq!(error.code, ErrorCode::Eio);
            assert!(error.to_string().contains("failure during load"));
            assert_eq!(fs.lock_state().expect("state").revision, 1);
        }
    }

    #[test]
    fn concurrent_refresh_checks_closed_after_either_conditional_response() {
        for changed in [false, true] {
            let metadata = ConditionalRevisionMetadata::new();
            let fs = conditional_filesystem(&metadata);
            if changed {
                let (namespace, revision) = fs.snapshot().expect("snapshot");
                metadata
                    .inner
                    .apply_revision(revision, namespace)
                    .expect("remote commit");
            }
            let closing = fs.clone();
            metadata.after_next_load(move || closing.lock_state().expect("state").closed = true);
            let error =
                block_on(fs.refresh_concurrent_namespace()).expect_err("closed after await");
            assert_eq!(error.code, ErrorCode::Ebadf);
            assert_eq!(fs.lock_state().expect("state").revision, 1);
        }
    }

    #[test]
    fn concurrent_refresh_does_not_regress_publication_during_conditional_load() {
        for changed in [false, true] {
            let metadata = ConditionalRevisionMetadata::new();
            let fs = conditional_filesystem(&metadata);
            if changed {
                let (mut namespace, revision) = fs.snapshot().expect("snapshot");
                namespace.default_uid = 11;
                metadata
                    .inner
                    .apply_revision(revision, namespace)
                    .expect("remote commit");
            }
            let publishing = fs.clone();
            let remote = metadata.inner.clone();
            metadata.after_next_load(move || {
                let latest = remote.state.lock().expect("revision state").clone();
                let mut namespace = latest.namespace.expect("namespace");
                namespace.default_gid = 12;
                block_on(publishing.publish_namespace(latest.revision, namespace, true))
                    .expect("local publication acknowledges during load");
            });
            block_on(fs.refresh_concurrent_namespace()).expect("racing refresh");
            let (namespace, revision) = fs.snapshot().expect("snapshot");
            assert_eq!(revision, if changed { 3 } else { 2 });
            assert_eq!(namespace.default_gid, 12);
            assert_eq!(namespace.default_uid, if changed { 11 } else { 0 });
            block_on(fs.shutdown()).expect("shutdown");
        }
    }

    #[test]
    fn metadata_conditional_load_default_fully_loads_unchanged_custom_provider() {
        let metadata = RevisionRaceMetadata::new();
        let fs = block_on(ChunkedFs::open(
            metadata.clone(),
            SharedTestBlockStore::new(),
            ChunkedOptions::fixed("fallback", 4)
                .expect("chunker")
                .with_concurrent_writes(true),
        ))
        .expect("open");
        let revision = fs.snapshot().expect("snapshot").1;
        let loaded = block_on(metadata.load_if_changed(revision))
            .expect("fallback load")
            .expect("the conservative default never omits payload");
        assert_eq!(loaded.revision, revision);
        assert_eq!(metadata.loads.load(Ordering::SeqCst), 2);
        metadata
            .state
            .lock()
            .expect("revision state")
            .namespace
            .as_mut()
            .expect("namespace")
            .root = 0;
        let error = block_on(fs.refresh_concurrent_namespace())
            .expect_err("fallback validates same-revision data");
        assert_eq!(error.code, ErrorCode::Einval);
    }

    #[test]
    fn concurrent_open_recovers_when_peer_enrolls_between_inspection_and_preflight() {
        let metadata = RevisionRaceMetadata::new();
        let blocks = SharedTestBlockStore::new();
        let peer_metadata = metadata.clone();
        let peer_blocks = blocks.clone();
        metadata.on_next_preflight(move || {
            let peer = block_on(ChunkedFs::open(
                peer_metadata,
                peer_blocks,
                ChunkedOptions::fixed("enrolling-peer", 4)?.with_concurrent_writes(true),
            ))?;
            block_on(peer.write_file("/retained", b"peer bytes"))?;
            block_on(peer.shutdown())
        });

        let fs = block_on(ChunkedFs::open(
            metadata,
            blocks.clone(),
            ChunkedOptions::fixed("racing-open", 4)
                .expect("chunker")
                .with_concurrent_writes(true),
        ))
        .expect("open must verify the authority enrolled by the peer");
        let retained = block_on(fs.open("/retained", "r", 0)).expect("open the peer's file");
        let mut bytes = [0; 10];
        assert_eq!(block_on(retained.read(&mut bytes, Some(0))).unwrap(), 10);
        assert_eq!(&bytes, b"peer bytes");
        block_on(retained.close()).expect("close the peer's file");
        assert_eq!(
            blocks.preparations.load(Ordering::SeqCst),
            1,
            "only the enrolling peer may prepare a block authority"
        );
        block_on(fs.shutdown()).expect("shutdown");
    }

    fn concurrent_open_racing_enrollment_rejects_unmatched_blocks(
        selected_backing: Option<ConcurrentBackingId>,
    ) {
        let metadata = RevisionRaceMetadata::new();
        let blocks = SharedTestBlockStore::new();
        *blocks.backing.lock().expect("block authority lock") = selected_backing;
        let peer_metadata = metadata.clone();
        metadata.on_next_preflight(move || {
            let peer = block_on(ChunkedFs::open(
                peer_metadata,
                SharedTestBlockStore::new(),
                ChunkedOptions::fixed("other-backing-peer", 4)?.with_concurrent_writes(true),
            ))?;
            block_on(peer.shutdown())
        });

        let error = block_on(ChunkedFs::open(
            metadata,
            blocks.clone(),
            ChunkedOptions::fixed("unmatched-racing-open", 4)
                .expect("chunker")
                .with_concurrent_writes(true),
        ))
        .err()
        .expect("the selected blocks do not hold the peer's authority");
        assert_eq!(error.code, ErrorCode::Estale);
        assert_eq!(
            blocks.preparations.load(Ordering::SeqCst),
            0,
            "an established MRC2 authority must only be verified"
        );
        assert_eq!(
            *blocks.backing.lock().expect("block authority lock"),
            selected_backing,
            "rejected startup must preserve the selected block marker"
        );
    }

    #[test]
    fn concurrent_open_racing_enrollment_does_not_claim_missing_block_authority() {
        concurrent_open_racing_enrollment_rejects_unmatched_blocks(None);
    }

    #[test]
    fn concurrent_open_racing_enrollment_rejects_wrong_block_authority() {
        concurrent_open_racing_enrollment_rejects_unmatched_blocks(Some(
            ConcurrentBackingId::from_bytes([0xa5; 16]).expect("different backing ID"),
        ));
    }

    #[test]
    fn concurrent_open_preserves_preflight_error_without_established_mrc2() {
        for mode in [ConcurrentModeState::Legacy, ConcurrentModeState::Mrc1] {
            let metadata = RevisionRaceMetadata::new();
            let blocks = SharedTestBlockStore::new();
            let changed_metadata = metadata.clone();
            metadata.on_next_preflight(move || {
                *changed_metadata.mode.lock().expect("concurrent mode lock") = mode;
                Err(FsError::new(ErrorCode::Eperm).with_syscall("selected preflight refusal"))
            });

            let error = block_on(ChunkedFs::open(
                metadata,
                blocks.clone(),
                ChunkedOptions::fixed("refused-racing-open", 4)
                    .expect("chunker")
                    .with_concurrent_writes(true),
            ))
            .err()
            .expect("Legacy and MRC1 cannot recover a refused preflight");
            assert_eq!(error.code, ErrorCode::Eperm);
            assert_eq!(error.syscall.as_deref(), Some("selected preflight refusal"));
            assert_eq!(blocks.preparations.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn concurrent_ack_does_not_regress_a_newer_locally_loaded_revision() {
        let metadata = RevisionRaceMetadata::new();
        let fs = block_on(ChunkedFs::open(
            metadata.clone(),
            SharedTestBlockStore::new(),
            ChunkedOptions::fixed("race", 4)
                .expect("chunker")
                .with_concurrent_writes(true),
        ))
        .expect("open concurrent filesystem");
        let (namespace, revision) = fs.snapshot().expect("initial snapshot");
        let refreshing = fs.clone();
        let remote = metadata.clone();
        metadata.on_next_flush(move || {
            let latest = block_on(remote.load()).expect("load after local CAS");
            let newer = block_on(remote.publish_bound_if_revision(
                test_concurrent_backing(),
                latest.revision,
                latest.namespace.expect("published namespace"),
            ))
            .expect("remote writer commits a newer revision");
            block_on(refreshing.refresh_concurrent_namespace()).expect("refresh newer revision");
            assert_eq!(refreshing.snapshot().expect("newer snapshot").1, newer);
        });

        let acknowledged = block_on(fs.publish_namespace(revision, namespace, true))
            .expect("local publication acknowledges");
        assert_eq!(acknowledged, revision + 1);
        assert_eq!(
            fs.snapshot().expect("post-ack snapshot").1,
            revision + 2,
            "the older acknowledgement must not overwrite a newer remote refresh"
        );
        block_on(fs.shutdown()).expect("shutdown");
    }

    #[test]
    fn prepared_whole_file_mutation_conflicts_if_same_batch_unlinks_its_path() {
        let fs = block_on(ChunkedFs::open(
            RevisionRaceMetadata::new(),
            SharedTestBlockStore::new(),
            ChunkedOptions::fixed("batch-path", 4)
                .expect("chunker")
                .with_concurrent_writes(true),
        ))
        .expect("open concurrent filesystem");
        block_on(fs.write_file("/victim", b"old")).expect("create victim");
        let (mut candidate, revision) = fs.snapshot().expect("prebatch snapshot");
        let inode = resolve(&candidate, "/victim", true, "test").expect("victim inode");
        let original = candidate.nodes.get(&inode).expect("victim node").clone();
        let NodeData::File(layout) = &original.data else {
            panic!("victim must be a file")
        };
        let mutation = WholeFileMutation {
            path: "/victim".to_owned(),
            inode,
            expected_revision: revision,
            new_inode: false,
            original: Some(original.clone()),
            layout: layout.clone(),
            data_length: original.stats.size,
        };

        apply_unlink_mutation(&fs, &mut candidate, "/victim").expect("earlier batch unlink");
        assert!(
            candidate.nodes.contains_key(&inode),
            "concurrent tombstone remains"
        );
        assert!(matches!(
            apply_whole_file_mutation(&mut candidate, revision, &mutation, true)
                .expect("prepared write outcome"),
            WholeFileMutationResult::Conflict,
        ));
        block_on(fs.shutdown()).expect("shutdown");
    }

    #[test]
    fn exclusive_write_resets_explicit_future_mtime_to_write_time() {
        let fs = block_on(ChunkedFs::open(
            MemoryMetadataStore::new(),
            MemoryBlockStore::new(),
            ChunkedOptions::fixed("legacy-mtime", 4).expect("chunker"),
        ))
        .expect("open exclusive filesystem");
        block_on(fs.write_file("/legacy", b"old")).expect("create legacy file");
        let future = now_ms()
            .checked_add(60_000)
            .expect("test timestamp fits i64");
        block_on(fs.utimes("/legacy", future, future)).expect("set explicit future mtime");
        block_on(fs.write_file("/legacy", b"new")).expect("replace file bytes");
        assert!(
            block_on(fs.stat("/legacy")).expect("legacy stat").mtime_ms < future,
            "exclusive mode keeps its original write-after-utimes timestamp semantics"
        );
        block_on(fs.shutdown()).expect("shutdown");
    }

    #[test]
    fn three_same_clock_concurrent_cas_commits_keep_distinct_mtimes() {
        let metadata = RevisionRaceMetadata::new();
        let backing = test_concurrent_backing();
        block_on(metadata.prepare_bound_concurrent_mode(backing))
            .expect("prepare bound concurrent mode");
        let options = ChunkedOptions::fixed("mtime-cas", 4).expect("chunker");
        let mut namespace = initial_namespace(&options).expect("initial namespace");
        let root = namespace.root;
        let fixed_now = 123_456_i64;
        let root_stats = &mut namespace.nodes.get_mut(&root).expect("root node").stats;
        root_stats.mtime_ms = fixed_now;
        root_stats.ctime_ms = fixed_now;
        let mut revision =
            block_on(metadata.publish_bound_if_revision(backing, 0, namespace.clone()))
                .expect("publish baseline");

        for expected in (fixed_now + 1)..=(fixed_now + 3) {
            touch_modified_at(
                &mut namespace.nodes.get_mut(&root).expect("root node").stats,
                fixed_now,
                true,
            )
            .expect("stamp one same-clock mutation");
            revision =
                block_on(metadata.publish_bound_if_revision(backing, revision, namespace.clone()))
                    .expect("publish one CAS revision");
            let loaded = block_on(metadata.load()).expect("load committed revision");
            assert_eq!(loaded.revision, revision);
            assert_eq!(
                loaded.namespace.expect("published namespace").nodes[&root]
                    .stats
                    .mtime_ms,
                expected,
                "successive same-clock revisions must not reuse an older mtime",
            );
        }
    }

    #[derive(Clone)]
    struct TestMetadataStore {
        inner: MemoryMetadataStore,
        loaded: Option<LoadedMetadata>,
        fail_publish: Arc<AtomicBool>,
        fail_flush: Arc<AtomicBool>,
        publish_includes_flush_barrier: bool,
    }

    #[async_trait]
    impl MetadataStore for TestMetadataStore {
        fn durable(&self) -> bool {
            self.inner.durable()
        }

        fn publish_includes_flush_barrier(&self) -> bool {
            self.publish_includes_flush_barrier
        }

        async fn load(&self) -> Result<LoadedMetadata> {
            match &self.loaded {
                Some(loaded) => Ok(loaded.clone()),
                None => self.inner.load().await,
            }
        }

        async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
            self.inner.acquire_writer(owner, ttl).await
        }

        async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
            self.inner.renew_writer(lease, ttl).await
        }

        async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
            self.inner.release_writer(lease).await
        }

        async fn publish(
            &self,
            expected_revision: u64,
            lease: &WriterLease,
            namespace: Namespace,
        ) -> Result<u64> {
            if self.fail_publish.load(Ordering::SeqCst) {
                return Err(
                    FsError::new(ErrorCode::Eio).with_message("injected metadata publish failure")
                );
            }
            self.inner
                .publish(expected_revision, lease, namespace)
                .await
        }

        async fn flush(&self) -> Result<()> {
            if self.fail_flush.load(Ordering::SeqCst) {
                return Err(
                    FsError::new(ErrorCode::Eio).with_message("injected metadata flush failure")
                );
            }
            self.inner.flush().await
        }
    }

    #[async_trait]
    impl BlockStore for FaultBlockStore {
        fn durable(&self) -> bool {
            false
        }

        async fn put(&self, bytes: &[u8]) -> Result<mount_rs_core::storage::BlockId> {
            if self.fail_put.load(Ordering::SeqCst) {
                return Err(FsError::new(ErrorCode::Eio).with_message("injected block put failure"));
            }
            self.inner.put(bytes).await
        }

        async fn get(&self, id: &mount_rs_core::storage::BlockId) -> Result<Vec<u8>> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            self.inner.get(id).await
        }

        async fn flush(&self) -> Result<()> {
            if self.pause_flush.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }

            if self.fail_flush.load(Ordering::SeqCst) {
                return Err(
                    FsError::new(ErrorCode::Eio).with_message("injected block flush failure")
                );
            }
            self.inner.flush().await
        }

        async fn delete(&self, id: &mount_rs_core::storage::BlockId) -> Result<()> {
            self.inner.delete(id).await
        }

        async fn reconcile(
            &self,
            live: &BTreeSet<mount_rs_core::storage::BlockId>,
            _grace: Duration,
        ) -> Result<mount_rs_core::storage::BlockReconcileReport> {
            *self.reconciled.lock().unwrap() = Some(live.clone());
            Ok(mount_rs_core::storage::BlockReconcileReport {
                scanned: live.len() as u64,
                protected: live.len() as u64,
                ..Default::default()
            })
        }
    }

    #[derive(Clone)]
    struct CountingMetadataStore {
        inner: MemoryMetadataStore,
        publishes: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl MetadataStore for CountingMetadataStore {
        fn durable(&self) -> bool {
            self.inner.durable()
        }

        async fn load(&self) -> Result<LoadedMetadata> {
            self.inner.load().await
        }

        async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
            self.inner.acquire_writer(owner, ttl).await
        }

        async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
            self.inner.renew_writer(lease, ttl).await
        }

        async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
            self.inner.release_writer(lease).await
        }

        async fn publish(
            &self,
            expected_revision: u64,
            lease: &WriterLease,
            namespace: Namespace,
        ) -> Result<u64> {
            self.publishes.fetch_add(1, Ordering::SeqCst);
            self.inner
                .publish(expected_revision, lease, namespace)
                .await
        }

        async fn flush(&self) -> Result<()> {
            self.inner.flush().await
        }
    }

    #[derive(Clone)]
    struct LeaseCountingMetadataStore {
        inner: MemoryMetadataStore,
        active_renewals: Arc<AtomicUsize>,
        max_active_renewals: Arc<AtomicUsize>,
        renewals: Arc<AtomicUsize>,
    }

    impl LeaseCountingMetadataStore {
        fn record_max_active(&self, active: usize) {
            let mut observed = self.max_active_renewals.load(Ordering::SeqCst);
            while active > observed {
                match self.max_active_renewals.compare_exchange(
                    observed,
                    active,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(next) => observed = next,
                }
            }
        }
    }

    #[async_trait]
    impl MetadataStore for LeaseCountingMetadataStore {
        fn durable(&self) -> bool {
            self.inner.durable()
        }

        async fn load(&self) -> Result<LoadedMetadata> {
            self.inner.load().await
        }

        async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
            self.inner.acquire_writer(owner, ttl).await
        }

        async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
            let active = self.active_renewals.fetch_add(1, Ordering::SeqCst) + 1;
            self.record_max_active(active);
            self.renewals.fetch_add(1, Ordering::SeqCst);
            cooperative_yield().await;
            let result = self.inner.renew_writer(lease, ttl).await;
            self.active_renewals.fetch_sub(1, Ordering::SeqCst);
            result
        }

        async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
            self.inner.release_writer(lease).await
        }

        async fn publish(
            &self,
            expected_revision: u64,
            lease: &WriterLease,
            namespace: Namespace,
        ) -> Result<u64> {
            self.inner
                .publish(expected_revision, lease, namespace)
                .await
        }

        async fn flush(&self) -> Result<()> {
            self.inner.flush().await
        }
    }

    fn options(owner: &str) -> ChunkedOptions {
        ChunkedOptions::fixed(owner, 4)
            .unwrap()
            .with_lease_ttl(Duration::from_secs(10))
    }

    #[test]
    fn exclusive_writeback_stages_until_sync_and_shutdown() {
        let metadata = MemoryMetadataStore::new();
        let blocks = MemoryBlockStore::new();
        let fs = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("writeback").with_ownership_mode(OwnershipMode::Exclusive),
        ))
        .unwrap();
        let initial = block_on(metadata.load()).unwrap();
        block_on(fs.write_file("/file", b"first")).unwrap();
        block_on(fs.write_file("/file", b"second")).unwrap();
        assert_eq!(
            block_on(metadata.load()).unwrap().revision,
            initial.revision
        );
        let handle = block_on(fs.open("/file", "r", 0)).unwrap();
        let mut bytes = [0; 6];
        block_on(handle.read(&mut bytes, Some(0))).unwrap();
        assert_eq!(&bytes, b"second");
        block_on(handle.close()).unwrap();
        block_on(fs.syncfs()).unwrap();
        assert_eq!(
            block_on(metadata.load()).unwrap().revision,
            initial.revision + 1
        );
        block_on(fs.write_file("/later", b"shutdown")).unwrap();
        block_on(fs.shutdown()).unwrap();
        let reopened = block_on(ChunkedFs::open(metadata, blocks, options("reopen"))).unwrap();
        let handle = block_on(reopened.open("/later", "r", 0)).unwrap();
        let mut bytes = [0; 8];
        block_on(handle.read(&mut bytes, Some(0))).unwrap();
        assert_eq!(&bytes, b"shutdown");
        block_on(handle.close()).unwrap();
        block_on(reopened.shutdown()).unwrap();
    }

    #[test]
    fn exclusive_writeback_barrier_failure_and_cancellation_fail_closed() {
        for cancel in [false, true] {
            let metadata = MemoryMetadataStore::new();
            let blocks = FaultBlockStore::new();
            let fs = block_on(ChunkedFs::open(
                metadata.clone(),
                blocks.clone(),
                options("barrier").with_writeback(true),
            ))
            .unwrap();
            let before = block_on(metadata.load()).unwrap().revision;
            block_on(fs.write_file("/file", b"pending")).unwrap();
            if cancel {
                blocks.pause_flush.store(true, Ordering::SeqCst);
                let mut sync = Box::pin(fs.syncfs());
                let waker = Waker::noop();
                let mut context = Context::from_waker(waker);
                assert!(matches!(sync.as_mut().poll(&mut context), Poll::Pending));
                drop(sync);
            } else {
                blocks.fail_flush.store(true, Ordering::SeqCst);
                assert_eq!(block_on(fs.syncfs()).unwrap_err().code, ErrorCode::Eio);
            }
            assert!(fs.failed());
            assert_eq!(block_on(metadata.load()).unwrap().revision, before);
            assert!(block_on(fs.write_file("/later", b"denied")).is_err());
            block_on(fs.shutdown()).unwrap();
        }
    }

    #[test]
    fn exclusive_writeback_gc_drains_and_generations_never_regress() {
        let metadata = MemoryMetadataStore::new();
        let blocks = FaultBlockStore::new();
        let fs = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("gc").with_writeback(true),
        ))
        .unwrap();
        block_on(fs.write_file("/file", b"first")).unwrap();
        block_on(fs.write_file("/file", b"second")).unwrap();
        let generation = fs.snapshot().unwrap().1;
        block_on(fs.reconcile_blocks(Duration::from_secs(60))).unwrap();
        assert_eq!(fs.snapshot().unwrap().1, generation);
        let namespace = block_on(metadata.load()).unwrap().namespace.unwrap();
        let mut durable_roots = BTreeSet::new();
        collect_block_roots(&namespace, &mut durable_roots);
        assert!(durable_roots.is_subset(blocks.reconciled.lock().unwrap().as_ref().unwrap()));
        block_on(fs.write_file("/file", b"third")).unwrap();
        assert!(fs.snapshot().unwrap().1 > generation);
        block_on(fs.shutdown()).unwrap();
    }

    #[test]
    fn exclusive_writeback_expiry_preserves_pending_state_and_takeover_fences_it() {
        for takeover in [false, true] {
            let clock = Arc::new(ManualClock::new(0));
            let metadata = MemoryMetadataStore::with_clock(clock.clone());
            let fs = block_on(ChunkedFs::open(
                metadata.clone(),
                MemoryBlockStore::new(),
                options("old")
                    .with_lease_ttl(Duration::from_secs(1))
                    .with_writeback(true),
            ))
            .unwrap();
            block_on(fs.write_file("/file", b"pending")).unwrap();
            let before = block_on(metadata.load()).unwrap().revision;
            assert!(clock.advance_ms(1_000));
            if takeover {
                let lease =
                    block_on(metadata.acquire_writer("new", Duration::from_secs(10))).unwrap();
                block_on(metadata.release_writer(&lease)).unwrap();
                assert_eq!(block_on(fs.syncfs()).unwrap_err().code, ErrorCode::Estale);
                assert!(fs.failed());
                assert_eq!(block_on(metadata.load()).unwrap().revision, before);
            } else {
                block_on(fs.syncfs()).unwrap();
                assert_eq!(block_on(metadata.load()).unwrap().revision, before + 1);
                assert_eq!(block_on(fs.stat("/file")).unwrap().size, 7);
                block_on(fs.shutdown()).unwrap();
            }
        }
    }

    #[test]
    fn exclusive_writeback_overlapping_whole_file_creates_and_namespace_ops() {
        let metadata = MemoryMetadataStore::new();
        let fs = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("overlap").with_writeback(true),
        ))
        .unwrap();
        let before = block_on(metadata.load()).unwrap().revision;
        let futures = (0..16)
            .map(|index| {
                let fs = fs.clone();
                Box::pin(async move { fs.write_file(&format!("/file-{index}"), b"bytes").await })
            })
            .collect();
        for result in block_on_all(futures) {
            result.unwrap();
        }
        block_on(fs.rename("/file-0", "/renamed")).unwrap();
        block_on(fs.unlink("/file-1")).unwrap();
        assert_eq!(block_on(fs.stat("/renamed")).unwrap().size, 5);
        assert!(block_on(fs.stat("/file-0")).is_err());
        assert!(block_on(fs.stat("/file-1")).is_err());
        assert_eq!(block_on(metadata.load()).unwrap().revision, before);
        block_on(fs.syncfs()).unwrap();
        assert_eq!(block_on(metadata.load()).unwrap().revision, before + 1);
        block_on(fs.shutdown()).unwrap();
    }

    #[test]
    fn exclusive_writeback_overlapping_offsets_and_appends_preserve_all_writes() {
        let fs = block_on(ChunkedFs::open(
            MemoryMetadataStore::new(),
            MemoryBlockStore::new(),
            options("offsets").with_writeback(true),
        ))
        .unwrap();
        block_on(fs.write_file("/file", &[0; 64])).unwrap();
        let futures = (0..16)
            .map(|index| {
                let handle = block_on(fs.open("/file", "r+", 0)).unwrap();
                Box::pin(async move {
                    handle.write(&[index as u8 + 1; 4], Some(index * 4)).await?;
                    handle.close().await
                })
            })
            .collect();
        for result in block_on_all(futures) {
            result.unwrap();
        }
        let futures = (0..16)
            .map(|_| {
                let handle = block_on(fs.open("/file", "a", 0)).unwrap();
                Box::pin(async move {
                    handle.write(b"tail", None).await?;
                    handle.close().await
                })
            })
            .collect();
        for result in block_on_all(futures) {
            result.unwrap();
        }
        let handle = block_on(fs.open("/file", "r+", 0)).unwrap();
        let mut bytes = [0; 128];
        assert_eq!(block_on(handle.read(&mut bytes, Some(0))).unwrap(), 128);
        for index in 0..16 {
            assert_eq!(&bytes[index * 4..index * 4 + 4], &[index as u8 + 1; 4]);
        }
        assert!(bytes[64..].chunks_exact(4).all(|chunk| chunk == b"tail"));
        block_on(handle.truncate(32)).unwrap();
        block_on(handle.sync()).unwrap();
        assert_eq!(block_on(handle.stat()).unwrap().size, 32);
        block_on(handle.close()).unwrap();
        block_on(fs.shutdown()).unwrap();
    }

    #[test]
    fn exclusive_writeback_metadata_barrier_failures_fail_closed() {
        for publish_failure in [false, true] {
            let metadata = TestMetadataStore {
                inner: MemoryMetadataStore::new(),
                loaded: None,
                fail_publish: Arc::new(AtomicBool::new(false)),
                fail_flush: Arc::new(AtomicBool::new(false)),
                publish_includes_flush_barrier: false,
            };
            let fs = block_on(ChunkedFs::open(
                metadata.clone(),
                MemoryBlockStore::new(),
                options("metadata-barrier").with_writeback(true),
            ))
            .unwrap();
            block_on(fs.write_file("/pending", b"bytes")).unwrap();
            if publish_failure {
                metadata.fail_publish.store(true, Ordering::SeqCst);
            } else {
                metadata.fail_flush.store(true, Ordering::SeqCst);
            }
            assert_eq!(block_on(fs.syncfs()).unwrap_err().code, ErrorCode::Eio);
            assert!(fs.failed());
            assert!(block_on(fs.stat("/pending")).is_err());
            block_on(fs.shutdown()).unwrap();
        }
    }

    #[test]
    fn exclusive_writeback_sync_uses_one_forced_provider_lease_check() {
        let metadata = LeaseCountingMetadataStore {
            inner: MemoryMetadataStore::new(),
            active_renewals: Arc::new(AtomicUsize::new(0)),
            max_active_renewals: Arc::new(AtomicUsize::new(0)),
            renewals: Arc::new(AtomicUsize::new(0)),
        };
        let fs = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("one-check").with_writeback(true),
        ))
        .unwrap();
        block_on(fs.write_file("/pending", b"bytes")).unwrap();
        let before = metadata.renewals.load(Ordering::SeqCst);
        block_on(fs.syncfs()).unwrap();
        assert_eq!(metadata.renewals.load(Ordering::SeqCst), before + 1);
        block_on(fs.shutdown()).unwrap();
    }

    #[test]
    fn shared_writeback_is_rejected() {
        let result = block_on(ChunkedFs::open(
            MemoryMetadataStore::new(),
            MemoryBlockStore::new(),
            options("invalid")
                .with_concurrent_writes(true)
                .with_writeback(true),
        ));
        assert_eq!(result.err().unwrap().code, ErrorCode::Einval);
    }

    #[test]
    fn composed_stores_keep_bytes_out_of_metadata_and_support_sparse_chunked_io() {
        let metadata = MemoryMetadataStore::new();
        let blocks = MemoryBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("one"),
        ))
        .unwrap();
        let file = block_on(filesystem.open("/file", "w+", 0o640)).unwrap();
        assert_eq!(block_on(file.write(b"abcd", Some(0))).unwrap(), 4);
        assert_eq!(block_on(file.write(b"XYZ", Some(6))).unwrap(), 3);
        let mut data = [0_u8; 9];
        assert_eq!(block_on(file.read(&mut data, Some(0))).unwrap(), 9);
        assert_eq!(&data, b"abcd\0\0XYZ");
        block_on(file.close()).unwrap();

        let namespace = block_on(metadata.load()).unwrap().namespace.unwrap();
        let node = namespace
            .nodes
            .values()
            .find(|node| node.stats.ino != namespace.root)
            .unwrap();
        match &node.data {
            NodeData::File(layout) => {
                assert_eq!(layout.chunker.parameters["chunk_size"], 4);
                assert!(layout.extents.iter().all(|extent| extent.length <= 4));
            }
            _ => panic!("expected file layout"),
        }
        assert!(!filesystem.capabilities().durable_writes);
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn whole_file_write_publishes_creation_and_bytes_atomically() {
        let metadata = MemoryMetadataStore::new();
        let blocks = MemoryBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("atomic-write"),
        ))
        .unwrap();
        let before = block_on(metadata.load()).unwrap().revision;

        let driver: &dyn FsDriver = &filesystem;
        block_on(driver.write_file("/atomic", b"atomic bytes")).unwrap();

        let committed = block_on(metadata.load()).unwrap();
        assert_eq!(committed.revision, before + 1);
        let file = block_on(filesystem.open("/atomic", "r", 0)).unwrap();
        let mut buffer = [0_u8; 12];
        assert_eq!(block_on(file.read(&mut buffer, Some(0))).unwrap(), 12);
        assert_eq!(&buffer, b"atomic bytes");
        block_on(file.close()).unwrap();
        block_on(filesystem.shutdown()).unwrap();

        let reopened = block_on(ChunkedFs::open(
            metadata,
            blocks,
            options("atomic-write-reopen"),
        ))
        .unwrap();
        let file = block_on(reopened.open("/atomic", "r", 0)).unwrap();
        let mut buffer = [0_u8; 12];
        assert_eq!(block_on(file.read(&mut buffer, Some(0))).unwrap(), 12);
        assert_eq!(&buffer, b"atomic bytes");
        block_on(file.close()).unwrap();
        block_on(reopened.shutdown()).unwrap();
    }

    #[test]
    fn concurrent_whole_file_mutations_share_one_fenced_publication() {
        const PARTICIPANTS: usize = MUTATION_BATCH_REQUEST_TARGET;
        let metadata = CountingMetadataStore {
            inner: MemoryMetadataStore::new(),
            publishes: Arc::new(AtomicUsize::new(0)),
        };
        let blocks = MemoryBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("mutation-batch"),
        ))
        .unwrap();
        for index in 0..PARTICIPANTS {
            block_on(filesystem.write_file(&format!("/file-{index}"), b"old!")).unwrap();
        }
        let before = metadata.publishes.load(Ordering::SeqCst);
        let futures = (0..PARTICIPANTS)
            .map(|index| {
                let filesystem = filesystem.clone();
                Box::pin(async move {
                    let path = format!("/file-{index}");
                    filesystem.write_file(&path, b"new!").await
                })
            })
            .collect();
        for result in block_on_all(futures) {
            result.unwrap();
        }
        assert_eq!(
            metadata.publishes.load(Ordering::SeqCst),
            before + 1,
            "concurrent whole-file mutations must share one fenced publication"
        );
        let loaded = block_on(metadata.load()).unwrap();
        let namespace = loaded.namespace.unwrap();
        for index in 0..PARTICIPANTS {
            let inode = resolve(&namespace, &format!("/file-{index}"), true, "test").unwrap();
            assert_eq!(namespace.nodes[&inode].stats.size, 4);
        }
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn concurrent_operation_lease_renewals_share_one_serialized_provider_call() {
        const PARTICIPANTS: usize = 16;
        let metadata = LeaseCountingMetadataStore {
            inner: MemoryMetadataStore::new(),
            active_renewals: Arc::new(AtomicUsize::new(0)),
            max_active_renewals: Arc::new(AtomicUsize::new(0)),
            renewals: Arc::new(AtomicUsize::new(0)),
        };
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("lease-renewal-gate"),
        ))
        .unwrap();
        let futures = (0..PARTICIPANTS)
            .map(|_| {
                let filesystem = filesystem.clone();
                Box::pin(async move { filesystem.ensure_operation_lease().await })
            })
            .collect();
        for result in block_on_all(futures) {
            result.unwrap();
        }
        assert_eq!(metadata.renewals.load(Ordering::SeqCst), 1);
        assert_eq!(metadata.max_active_renewals.load(Ordering::SeqCst), 1);
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn mutation_runner_waits_for_active_preparation_past_initial_idle_window() {
        let metadata = CountingMetadataStore {
            inner: MemoryMetadataStore::new(),
            publishes: Arc::new(AtomicUsize::new(0)),
        };
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("mutation-preparation-window"),
        ))
        .unwrap();
        block_on(filesystem.write_file("/file", b"old")).unwrap();
        let before = metadata.publishes.load(Ordering::SeqCst);

        // Model a peer whole-file operation that has finished its metadata
        // snapshot but is still preparing immutable blocks. The runner must
        // not publish the queued unlink merely because the initial adaptive
        // idle window has elapsed; the preparation counter is deliberately
        // held across that window.
        let preparation = filesystem.begin_mutation_preparation();
        let mut unlink = Box::pin(filesystem.unlink("/file"));
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let polls =
            (MUTATION_BATCH_INITIAL_YIELD_ROUNDS + MUTATION_BATCH_IDLE_YIELD_ROUNDS + 1) * 2;
        for _ in 0..polls {
            assert!(matches!(
                Future::poll(unlink.as_mut(), &mut context),
                Poll::Pending
            ));
            assert_eq!(metadata.publishes.load(Ordering::SeqCst), before);
        }

        preparation.release();
        assert!(block_on(unlink).is_ok());
        assert_eq!(metadata.publishes.load(Ordering::SeqCst), before + 1);
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn concurrent_whole_file_creates_rebase_inodes_in_one_publication() {
        const PARTICIPANTS: usize = MUTATION_BATCH_REQUEST_TARGET;
        let metadata = CountingMetadataStore {
            inner: MemoryMetadataStore::new(),
            publishes: Arc::new(AtomicUsize::new(0)),
        };
        let blocks = MemoryBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks,
            options("mutation-batch-create"),
        ))
        .unwrap();
        let before = metadata.publishes.load(Ordering::SeqCst);
        let futures = (0..PARTICIPANTS)
            .map(|index| {
                let filesystem = filesystem.clone();
                Box::pin(async move {
                    filesystem
                        .write_file(&format!("/created-{index}"), b"created")
                        .await
                })
            })
            .collect();
        for result in block_on_all(futures) {
            result.unwrap();
        }
        assert_eq!(
            metadata.publishes.load(Ordering::SeqCst),
            before + 1,
            "concurrent whole-file creates must share one fenced publication"
        );
        let loaded = block_on(metadata.load()).unwrap();
        let namespace = loaded.namespace.unwrap();
        let mut inodes = BTreeSet::new();
        for index in 0..PARTICIPANTS {
            let inode = resolve(&namespace, &format!("/created-{index}"), true, "test").unwrap();
            assert!(
                inodes.insert(inode),
                "batched creates must receive unique inodes"
            );
            assert_eq!(namespace.nodes[&inode].stats.size, 7);
        }
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn canceled_mutation_runner_releases_queued_requests_without_publishing() {
        let metadata = MemoryMetadataStore::new();
        let blocks = MemoryBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks,
            options("mutation-cancel"),
        ))
        .unwrap();
        block_on(filesystem.write_file("/file", b"old!")).unwrap();
        let before = block_on(metadata.load()).unwrap();

        let mut canceled = Box::pin(filesystem.write_file("/file", b"first"));
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        loop {
            assert!(matches!(
                Future::poll(canceled.as_mut(), &mut context),
                Poll::Pending
            ));
            let running = filesystem.inner.mutations.lock().unwrap().running;
            if running {
                break;
            }
        }
        drop(canceled);

        block_on(filesystem.write_file("/file", b"second")).unwrap();
        let after = block_on(metadata.load()).unwrap();
        assert_eq!(after.revision, before.revision + 1);
        let file = block_on(filesystem.open("/file", "r", 0)).unwrap();
        let mut bytes = [0_u8; 6];
        assert_eq!(block_on(file.read(&mut bytes, Some(0))).unwrap(), 6);
        assert_eq!(&bytes, b"second");
        block_on(file.close()).unwrap();
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn reads_coalesce_atime_and_eof_does_not_publish() {
        let metadata = MemoryMetadataStore::new();
        let blocks = MemoryBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks,
            options("lazy-atime"),
        ))
        .unwrap();
        let driver: &dyn FsDriver = &filesystem;
        block_on(driver.write_file("/read", b"read bytes")).unwrap();
        let before = block_on(metadata.load()).unwrap();

        let file = block_on(filesystem.open("/read", "r", 0)).unwrap();
        let before_stat = block_on(file.stat()).unwrap();
        let mut buffer = [0_u8; 16];
        assert_eq!(block_on(file.read(&mut buffer, Some(0))).unwrap(), 10);
        let visible = block_on(file.stat()).unwrap();
        assert!(visible.atime_ms >= before_stat.atime_ms);
        assert_eq!(block_on(metadata.load()).unwrap().revision, before.revision);

        let mut eof = [0_u8; 1];
        assert_eq!(block_on(file.read(&mut eof, Some(10))).unwrap(), 0);
        assert_eq!(block_on(metadata.load()).unwrap().revision, before.revision);

        block_on(file.sync()).unwrap();
        let synced = block_on(metadata.load()).unwrap();
        assert_eq!(synced.revision, before.revision + 1);
        let persisted_atime = synced
            .namespace
            .as_ref()
            .unwrap()
            .nodes
            .values()
            .find(|node| matches!(node.data, NodeData::File(_)))
            .unwrap()
            .stats
            .atime_ms;
        assert_eq!(persisted_atime, visible.atime_ms);
        block_on(file.close()).unwrap();
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn block_flush_failure_does_not_publish_metadata_and_retry_is_possible() {
        let metadata = MemoryMetadataStore::new();
        let blocks = FaultBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("fault"),
        ))
        .unwrap();
        let file = block_on(filesystem.open("/file", "w", 0o600)).unwrap();
        blocks.fail_flush.store(true, Ordering::SeqCst);
        let before = block_on(metadata.load()).unwrap().revision;
        assert_eq!(
            block_on(file.write(b"data", Some(0))).unwrap_err().code,
            ErrorCode::Eio
        );
        assert_eq!(block_on(metadata.load()).unwrap().revision, before);
        blocks.fail_flush.store(false, Ordering::SeqCst);
        assert_eq!(block_on(file.write(b"data", Some(0))).unwrap(), 4);
        block_on(file.close()).unwrap();
    }

    #[test]
    fn block_put_failure_does_not_acknowledge_or_publish_a_partial_write() {
        let metadata = MemoryMetadataStore::new();
        let blocks = FaultBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("put-fault"),
        ))
        .unwrap();
        let invalid = block_on(filesystem.reconcile_blocks(Duration::ZERO)).unwrap_err();
        assert_eq!(invalid.code, ErrorCode::Einval);
        assert!(blocks.reconciled.lock().unwrap().is_none());

        let file = block_on(filesystem.open("/file", "w+", 0o600)).unwrap();
        let before = block_on(metadata.load()).unwrap().revision;

        blocks.fail_put.store(true, Ordering::SeqCst);
        let error = block_on(file.write(b"data", Some(0))).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        assert_eq!(block_on(metadata.load()).unwrap().revision, before);
        assert_eq!(block_on(file.stat()).unwrap().size, 0);

        blocks.fail_put.store(false, Ordering::SeqCst);
        assert_eq!(block_on(file.write(b"data", Some(0))).unwrap(), 4);
        block_on(file.close()).unwrap();
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn reconciliation_protects_committed_and_open_unlinked_block_roots() {
        let metadata = MemoryMetadataStore::new();
        let blocks = FaultBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata,
            blocks.clone(),
            options("reconcile-roots"),
        ))
        .unwrap();
        let file = block_on(filesystem.open("/file", "w+", 0o600)).unwrap();
        block_on(file.write(b"data", Some(0))).unwrap();
        block_on(filesystem.unlink("/file")).unwrap();

        let report = block_on(filesystem.reconcile_blocks(Duration::from_secs(60))).unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(report.protected, 1);
        let roots = blocks.reconciled.lock().unwrap().clone().unwrap();
        assert_eq!(roots.len(), 1);

        block_on(file.close()).unwrap();
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn metadata_publish_failure_leaves_blocks_unreferenced_and_fails_closed() {
        let metadata = TestMetadataStore {
            inner: MemoryMetadataStore::new(),
            loaded: None,
            fail_publish: Arc::new(AtomicBool::new(false)),
            fail_flush: Arc::new(AtomicBool::new(false)),
            publish_includes_flush_barrier: false,
        };
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("publish-fault"),
        ))
        .unwrap();
        let file = block_on(filesystem.open("/file", "w+", 0o600)).unwrap();
        let before = block_on(metadata.inner.load()).unwrap().revision;
        metadata.fail_publish.store(true, Ordering::SeqCst);

        let error = block_on(file.write(b"data", Some(0))).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        assert!(filesystem.failed());
        let loaded = block_on(metadata.inner.load()).unwrap();
        assert_eq!(loaded.revision, before);
        let inode = loaded
            .namespace
            .as_ref()
            .unwrap()
            .nodes
            .values()
            .find(|node| node.stats.ino != 1)
            .unwrap();
        assert_eq!(inode.stats.size, 0);
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn failed_shutdown_releases_lease_when_pending_atime_cannot_publish() {
        let metadata = TestMetadataStore {
            inner: MemoryMetadataStore::new(),
            loaded: None,
            fail_publish: Arc::new(AtomicBool::new(false)),
            fail_flush: Arc::new(AtomicBool::new(false)),
            publish_includes_flush_barrier: false,
        };
        let blocks = MemoryBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("failed-shutdown-atime"),
        ))
        .unwrap();
        let driver: &dyn FsDriver = &filesystem;
        block_on(driver.write_file("/file", b"baseline")).unwrap();
        let file = block_on(filesystem.open("/file", "r+", 0)).unwrap();
        let mut buffer = [0_u8; 8];
        assert_eq!(block_on(file.read(&mut buffer, Some(0))).unwrap(), 8);

        metadata.fail_publish.store(true, Ordering::SeqCst);
        assert_eq!(
            block_on(file.write(b"attempted", Some(0)))
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        assert!(filesystem.failed());
        metadata.fail_publish.store(false, Ordering::SeqCst);
        block_on(file.close()).unwrap();
        block_on(filesystem.shutdown())
            .expect("a failed filesystem must release its lease during shutdown");

        let reopened = block_on(ChunkedFs::open(
            metadata,
            blocks,
            options("failed-shutdown-atime-reopen"),
        ))
        .expect("shutdown must release the failed instance lease");
        block_on(reopened.shutdown()).unwrap();
    }

    #[test]
    fn metadata_flush_failure_reports_uncertain_commit_and_fails_closed() {
        let metadata = TestMetadataStore {
            inner: MemoryMetadataStore::new(),
            loaded: None,
            fail_publish: Arc::new(AtomicBool::new(false)),
            fail_flush: Arc::new(AtomicBool::new(false)),
            publish_includes_flush_barrier: false,
        };
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("metadata-flush-fault"),
        ))
        .unwrap();
        let file = block_on(filesystem.open("/file", "w+", 0o600)).unwrap();
        let before = block_on(metadata.inner.load()).unwrap().revision;
        metadata.fail_flush.store(true, Ordering::SeqCst);

        let error = block_on(file.write(b"data", Some(0))).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        assert!(filesystem.failed());
        let loaded = block_on(metadata.inner.load()).unwrap();
        assert_eq!(loaded.revision, before + 1);
        let inode = loaded
            .namespace
            .as_ref()
            .unwrap()
            .nodes
            .values()
            .find(|node| node.stats.ino != 1)
            .unwrap();
        assert_eq!(inode.stats.size, 4);
        loaded.validate().unwrap();
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn publish_barrier_capability_skips_redundant_flush_but_syncfs_still_checks_it() {
        let metadata = TestMetadataStore {
            inner: MemoryMetadataStore::new(),
            loaded: None,
            fail_publish: Arc::new(AtomicBool::new(false)),
            fail_flush: Arc::new(AtomicBool::new(false)),
            publish_includes_flush_barrier: true,
        };
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("publish-barrier-capability"),
        ))
        .unwrap();
        let before = block_on(metadata.inner.load()).unwrap().revision;
        metadata.fail_flush.store(true, Ordering::SeqCst);

        block_on(filesystem.write_file("/file", b"data")).unwrap();
        assert!(!filesystem.failed());
        assert_eq!(
            block_on(metadata.inner.load()).unwrap().revision,
            before + 1
        );

        let error = block_on(filesystem.syncfs()).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        assert!(filesystem.failed());
        let _ = block_on(filesystem.shutdown());
    }

    #[test]
    fn fixed_size_rewrites_fetch_only_affected_chunks_and_bound_sparse_offsets() {
        let metadata = MemoryMetadataStore::new();
        let blocks = FaultBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata,
            blocks.clone(),
            options("bounded-io"),
        ))
        .unwrap();
        let file = block_on(filesystem.open("/file", "w+", 0o600)).unwrap();
        assert_eq!(block_on(file.write(b"aaaa", Some(0))).unwrap(), 4);
        assert_eq!(block_on(file.write(b"bbbb", Some(4))).unwrap(), 4);

        blocks.gets.store(0, Ordering::SeqCst);
        let mut one = [0_u8; 1];
        assert_eq!(block_on(file.read(&mut one, Some(0))).unwrap(), 1);
        assert_eq!(blocks.gets.load(Ordering::SeqCst), 1);

        blocks.gets.store(0, Ordering::SeqCst);
        assert_eq!(block_on(file.write(b"Z", Some(0))).unwrap(), 1);
        assert_eq!(
            blocks.gets.load(Ordering::SeqCst),
            1,
            "partial overwrite must fetch only its affected chunk"
        );
        blocks.gets.store(0, Ordering::SeqCst);
        let mut untouched = [0_u8; 4];
        assert_eq!(block_on(file.read(&mut untouched, Some(4))).unwrap(), 4);
        assert_eq!(&untouched, b"bbbb");
        assert_eq!(
            blocks.gets.load(Ordering::SeqCst),
            1,
            "the untouched chunk remains readable as a separate extent"
        );

        blocks.gets.store(0, Ordering::SeqCst);
        let sparse_offset = 1_u64 << 40;
        assert_eq!(block_on(file.write(b"x", Some(sparse_offset))).unwrap(), 1);
        assert_eq!(
            blocks.gets.load(Ordering::SeqCst),
            0,
            "a sparse write into an empty chunk must not scan the file"
        );
        assert_eq!(block_on(file.stat()).unwrap().size, sparse_offset + 1);
        block_on(file.close()).unwrap();
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn missing_referenced_block_fails_a_lazy_read() {
        let metadata = MemoryMetadataStore::new();
        let blocks = MemoryBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("missing-block"),
        ))
        .unwrap();
        let file = block_on(filesystem.open("/file", "w+", 0o600)).unwrap();
        block_on(file.write(b"data", Some(0))).unwrap();

        let namespace = block_on(metadata.load()).unwrap().namespace.unwrap();
        let block = namespace
            .nodes
            .values()
            .find_map(|node| match &node.data {
                NodeData::File(layout) => layout.extents.first().map(|extent| extent.block.clone()),
                _ => None,
            })
            .expect("the write should publish one referenced block");
        block_on(blocks.delete(&block)).unwrap();

        let mut buffer = [0_u8; 4];
        let error = block_on(file.read(&mut buffer, Some(0))).unwrap_err();
        assert_eq!(error.code, ErrorCode::Enoent);
        assert_eq!(error.syscall.as_deref(), Some("read"));
        assert_eq!(error.path.as_deref(), Some("/file"));
        block_on(file.close()).unwrap();
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn open_releases_exact_latest_lease_on_validation_and_initial_flush_failures() {
        let invalid = TestMetadataStore {
            inner: MemoryMetadataStore::new(),
            loaded: Some(LoadedMetadata {
                revision: 0,
                namespace: Some(initial_namespace(&options("invalid-load")).unwrap()),
            }),
            fail_publish: Arc::new(AtomicBool::new(false)),
            fail_flush: Arc::new(AtomicBool::new(false)),
            publish_includes_flush_barrier: false,
        };
        let result = block_on(ChunkedFs::open(
            invalid.clone(),
            MemoryBlockStore::new(),
            options("invalid-load"),
        ));
        assert_eq!(
            result.err().expect("invalid metadata must fail").code,
            ErrorCode::Einval
        );
        let replacement = block_on(
            invalid
                .inner
                .acquire_writer("replacement", Duration::from_secs(10)),
        )
        .expect("validation failure must release the acquired lease");
        block_on(invalid.inner.release_writer(&replacement)).unwrap();

        let flush_failure = TestMetadataStore {
            inner: MemoryMetadataStore::new(),
            loaded: None,
            fail_publish: Arc::new(AtomicBool::new(false)),
            fail_flush: Arc::new(AtomicBool::new(true)),
            publish_includes_flush_barrier: false,
        };
        let result = block_on(ChunkedFs::open(
            flush_failure.clone(),
            MemoryBlockStore::new(),
            options("initial-flush"),
        ));
        assert_eq!(
            result.err().expect("initial metadata flush must fail").code,
            ErrorCode::Eio
        );
        let replacement = block_on(
            flush_failure
                .inner
                .acquire_writer("replacement", Duration::from_secs(10)),
        )
        .expect("initial flush failure must release the renewed lease");
        block_on(flush_failure.inner.release_writer(&replacement)).unwrap();
    }

    #[test]
    fn lease_loss_fails_closed_after_another_owner_fences_the_driver() {
        let clock = Arc::new(ManualClock::new(0));
        let metadata = MemoryMetadataStore::with_clock(clock.clone());
        let blocks = MemoryBlockStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks,
            options("old").with_lease_ttl(Duration::from_secs(1)),
        ))
        .unwrap();
        assert!(clock.advance_ms(1_000));
        let newer = block_on(metadata.acquire_writer("new", Duration::from_secs(10))).unwrap();
        let file = block_on(filesystem.open("/file", "w", 0o600));
        assert_eq!(
            file.err().expect("open should be fenced").code,
            ErrorCode::Estale
        );
        assert!(filesystem.failed());
        assert_eq!(block_on(metadata.release_writer(&newer)).unwrap(), ());
    }

    #[test]
    fn expired_reacquire_fails_closed_after_another_owner_publishes_revision() {
        let clock = Arc::new(ManualClock::new(0));
        let metadata = MemoryMetadataStore::with_clock(clock.clone());
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("revision-old").with_lease_ttl(Duration::from_secs(1)),
        ))
        .unwrap();
        assert!(clock.advance_ms(1_000));

        let newer =
            block_on(metadata.acquire_writer("revision-new", Duration::from_secs(10))).unwrap();
        let loaded = block_on(metadata.load()).unwrap();
        let mut namespace = loaded.namespace.unwrap();
        namespace
            .nodes
            .get_mut(&namespace.root)
            .unwrap()
            .stats
            .ctime_ms = 2;
        let published = block_on(metadata.publish(loaded.revision, &newer, namespace)).unwrap();
        assert_eq!(published, loaded.revision + 1);
        block_on(metadata.release_writer(&newer)).unwrap();

        let error = block_on(filesystem.stat("/")).unwrap_err();
        assert_eq!(error.code, ErrorCode::Estale);
        assert!(filesystem.failed());
    }

    #[test]
    fn idle_filesystem_reacquires_its_next_fence_without_stale_reads() {
        let clock = Arc::new(ManualClock::new(0));
        let metadata = MemoryMetadataStore::with_clock(clock.clone());
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("idle").with_lease_ttl(Duration::from_secs(1)),
        ))
        .unwrap();

        assert!(clock.advance_ms(1_000));
        assert!(block_on(filesystem.stat("/")).is_ok());
        assert!(!filesystem.failed());
        block_on(filesystem.shutdown()).unwrap();
    }

    #[test]
    fn shutdown_reacquires_its_expired_unfenced_lease_before_release() {
        let clock = Arc::new(ManualClock::new(0));
        let metadata = MemoryMetadataStore::with_clock(clock.clone());
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("shutdown-expired").with_lease_ttl(Duration::from_secs(1)),
        ))
        .unwrap();

        assert!(clock.advance_ms(1_000));
        block_on(filesystem.shutdown())
            .expect("graceful shutdown should renew an expired lease owned by this filesystem");

        let replacement = block_on(metadata.acquire_writer("replacement", Duration::from_secs(10)))
            .expect("shutdown must release its refreshed lease");
        block_on(metadata.release_writer(&replacement)).unwrap();
    }

    #[test]
    fn directories_links_symlinks_and_handle_positions_follow_core_semantics() {
        let filesystem = block_on(ChunkedFs::open(
            MemoryMetadataStore::new(),
            MemoryBlockStore::new(),
            options("semantics"),
        ))
        .unwrap();
        assert_eq!(
            block_on(filesystem.mkdir(
                "/a/b",
                MkdirOptions {
                    recursive: true,
                    mode: Some(0o750)
                }
            ))
            .unwrap(),
            Some("/a".to_owned())
        );
        let file = block_on(filesystem.open("/a/b/file", "w+", 0o640)).unwrap();
        block_on(file.write(b"hello", None)).unwrap();
        let mut bytes = [0_u8; 2];
        assert_eq!(block_on(file.read(&mut bytes, Some(1))).unwrap(), 2);
        assert_eq!(&bytes, b"el");
        block_on(filesystem.link("/a/b/file", "/a/link")).unwrap();
        block_on(filesystem.symlink("b/file", "/a/sym")).unwrap();
        assert!(block_on(filesystem.stat("/a/sym")).unwrap().is_file());
        assert!(
            block_on(filesystem.lstat("/a/sym"))
                .unwrap()
                .is_symbolic_link()
        );
        block_on(file.close()).unwrap();
        assert_eq!(block_on(filesystem.readdir("/a/b")).unwrap().len(), 1);
    }

    #[test]
    fn canceled_exclusive_close_can_retry_and_reap_an_unlinked_inode() {
        let metadata = MemoryMetadataStore::new();
        let filesystem = block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options("canceled-exclusive-close"),
        ))
        .expect("open exclusive filesystem");
        let handle = block_on(filesystem.open("/file", "w+", 0o600)).expect("open file");
        let inode = block_on(handle.stat()).expect("fstat before unlink").ino;
        block_on(filesystem.unlink("/file")).expect("unlink open file");
        assert_eq!(
            filesystem.lock_state().unwrap().open_refs.get(&inode),
            Some(&1)
        );
        assert!(
            filesystem
                .lock_state()
                .unwrap()
                .orphans
                .contains_key(&inode)
        );
        assert!(
            !block_on(metadata.load())
                .unwrap()
                .namespace
                .unwrap()
                .nodes
                .contains_key(&inode)
        );

        let filesystem_gate = block_on(filesystem.inner.gate.lock());
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut interrupted = Box::pin(handle.close());
        assert!(matches!(
            Future::poll(interrupted.as_mut(), &mut context),
            Poll::Pending
        ));
        drop(interrupted);
        drop(filesystem_gate);

        block_on(handle.close()).expect("retry interrupted close");
        assert!(
            !filesystem
                .lock_state()
                .unwrap()
                .open_refs
                .contains_key(&inode)
        );
        assert!(
            !filesystem
                .lock_state()
                .unwrap()
                .orphans
                .contains_key(&inode)
        );
        assert!(
            !block_on(metadata.load())
                .unwrap()
                .namespace
                .unwrap()
                .nodes
                .contains_key(&inode)
        );
        block_on(filesystem.shutdown()).expect("shutdown exclusive filesystem");
    }

    #[test]
    fn canceled_concurrent_close_can_retry_and_release_ref_after_remote_unlink() {
        let metadata = RevisionRaceMetadata::new();
        let blocks = SharedTestBlockStore::new();
        let first = block_on(ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            ChunkedOptions::fixed("canceled-concurrent-a", 4)
                .expect("chunker")
                .with_concurrent_writes(true),
        ))
        .expect("open first concurrent coordinator");
        let second = block_on(ChunkedFs::open(
            metadata,
            blocks,
            ChunkedOptions::fixed("canceled-concurrent-b", 4)
                .expect("chunker")
                .with_concurrent_writes(true),
        ))
        .expect("open second concurrent coordinator");
        let handle = block_on(first.open("/file", "w+", 0o600)).expect("open file");
        let inode = block_on(handle.stat())
            .expect("fstat before remote unlink")
            .ino;
        block_on(second.unlink("/file")).expect("remote unlink");
        assert_eq!(
            block_on(handle.stat()).expect("open orphan fstat").ino,
            inode
        );
        assert_eq!(first.lock_state().unwrap().open_refs.get(&inode), Some(&1));
        assert_eq!(
            first
                .lock_state()
                .unwrap()
                .namespace
                .nodes
                .get(&inode)
                .expect("concurrent tombstone")
                .stats
                .nlink,
            0
        );

        let filesystem_gate = block_on(first.inner.gate.lock());
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut interrupted = Box::pin(handle.close());
        assert!(matches!(
            Future::poll(interrupted.as_mut(), &mut context),
            Poll::Pending
        ));
        drop(interrupted);
        drop(filesystem_gate);

        block_on(handle.close()).expect("retry interrupted concurrent close");
        let state = first.lock_state().unwrap();
        assert!(!state.open_refs.contains_key(&inode));
        assert_eq!(
            state
                .namespace
                .nodes
                .get(&inode)
                .expect("remote tombstone remains until distributed reclamation")
                .stats
                .nlink,
            0
        );
        drop(state);
        block_on(first.shutdown()).expect("shutdown first coordinator");
        block_on(second.shutdown()).expect("shutdown second coordinator");
    }
}

#[cfg(all(test, unix))]
mod delegated_generation_tests {
    use super::*;
    use futures_lite::future::block_on;
    use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};

    #[test]
    fn generation_overflow_never_claims_or_releases_authority() {
        block_on(async {
            let path = std::env::temp_dir().join(format!(
                "mount-rs-grant-generation-{}-{}",
                std::process::id(),
                now_ms()
            ));
            std::fs::create_dir(&path).unwrap();
            let fs = ChunkedFs::open(
                SqliteMetadataStore::open(path.join("metadata.db")).unwrap(),
                SqliteBlockStore::open(path.join("blocks.db")).unwrap(),
                ChunkedOptions::fixed("overflow", 4096)
                    .unwrap()
                    .with_checkout_path("/"),
            )
            .await
            .unwrap();
            let grant = fs.delegation_status().await.unwrap().unwrap();
            fs.inner
                .delegation_generation
                .store(u64::MAX, Ordering::SeqCst);
            assert_eq!(
                fs.checkin_scope().await.unwrap_err().code,
                ErrorCode::Eoverflow
            );
            assert_eq!(
                fs.metadata_store()
                    .delegation_state()
                    .await
                    .unwrap()
                    .unwrap()
                    .grants[&grant.token.root],
                grant
            );
            fs.inner.delegation_generation.store(1, Ordering::SeqCst);
            fs.checkin_scope().await.unwrap();
            fs.inner
                .delegation_generation
                .store(u64::MAX, Ordering::SeqCst);
            assert_eq!(
                fs.checkout_scope("/").await.unwrap_err().code,
                ErrorCode::Eoverflow
            );
            assert!(
                fs.metadata_store()
                    .delegation_state()
                    .await
                    .unwrap()
                    .unwrap()
                    .grants
                    .is_empty()
            );
            fs.shutdown().await.unwrap();
            drop(fs);
            std::fs::remove_dir_all(path).unwrap();
        });
    }
    #[test]
    fn stale_local_candidate_rebases_instead_of_denied_outside_delta() {
        block_on(async {
            let path = std::env::temp_dir().join(format!(
                "mount-rs-grant-rebase-{}-{}",
                std::process::id(),
                now_ms()
            ));
            std::fs::create_dir(&path).unwrap();
            let metadata = SqliteMetadataStore::open(path.join("metadata.db")).unwrap();
            let blocks = SqliteBlockStore::open(path.join("blocks.db")).unwrap();
            let options = ChunkedOptions::fixed("bootstrap", 4096)
                .unwrap()
                .with_checkout_path("/");
            let bootstrap = ChunkedFs::open(metadata.clone(), blocks.clone(), options)
                .await
                .unwrap();
            bootstrap
                .mkdir("/a", MkdirOptions::default())
                .await
                .unwrap();
            bootstrap
                .mkdir("/b", MkdirOptions::default())
                .await
                .unwrap();
            bootstrap.checkin_scope().await.unwrap();
            let a = ChunkedFs::open(
                metadata.clone(),
                blocks.clone(),
                ChunkedOptions::fixed("a", 4096)
                    .unwrap()
                    .with_checkout_path("/a"),
            )
            .await
            .unwrap();
            let b = ChunkedFs::open(
                metadata,
                blocks,
                ChunkedOptions::fixed("b", 4096)
                    .unwrap()
                    .with_checkout_path("/b"),
            )
            .await
            .unwrap();
            let (mut candidate, revision) = a.snapshot().unwrap();
            let inode = resolve(&candidate, "/a", true, "test").unwrap();
            candidate.nodes.get_mut(&inode).unwrap().stats.mtime_ms += 1;
            b.write_file("/b/new", b"disjoint").await.unwrap();
            a.refresh_concurrent_namespace().await.unwrap();
            assert_eq!(
                a.publish_namespace_durable(revision, candidate, false)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Eagain
            );
            a.write_file("/a/own", b"still usable").await.unwrap();
            let (mut candidate, revision) = a.snapshot().unwrap();
            candidate.nodes.get_mut(&inode).unwrap().stats.mtime_ms += 1;
            let handle = b.open("/b/new", "r+", 0).await.unwrap();
            b.unlink("/b/new").await.unwrap();
            // Authority now contains B's orphan but A has not loaded that namespace.
            assert_eq!(
                a.publish_namespace_durable(revision, candidate, false)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Eagain
            );
            handle.close().await.unwrap();
            a.write_file("/a/own", b"still usable").await.unwrap();
            a.shutdown().await.unwrap();
            b.shutdown().await.unwrap();
            bootstrap.shutdown().await.unwrap();
            drop(a);
            drop(b);
            drop(bootstrap);
            std::fs::remove_dir_all(path).unwrap();
        });
    }
}
