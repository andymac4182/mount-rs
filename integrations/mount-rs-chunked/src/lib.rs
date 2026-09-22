//! Filesystem orchestration over independent metadata and immutable block stores.
//!
//! [`ChunkedFs`] deliberately keeps namespace records and file bytes in
//! separate providers. A write stores new immutable blocks and flushes them
//! before the fenced metadata revision publishes references to those blocks.
//! The implementation is intentionally not a snapshot adapter and does not
//! provide copy-on-write views.

use async_trait::async_trait;
use mount_rs_core::chunking::{Chunker, FixedSizeChunker, from_config};
use mount_rs_core::driver::{FileHandle, FsDriver};
use mount_rs_core::error::{ErrorCode, FsError, Result};
use mount_rs_core::handle::OpenFlags;
use mount_rs_core::path::{is_path_inside, normalize_path, split_path};
use mount_rs_core::storage::{
    BlockExtent, BlockReconcileReport, BlockStore, FileLayout, InodeId, MetadataStore,
    NAMESPACE_FORMAT_VERSION, Namespace, NodeData, NodeMetadata, WriterLease,
};
use mount_rs_core::types::{
    Capabilities, DirEntry, FileType, MkdirOptions, S_IFDIR, S_IFMT, S_IFREG, Stats, StatsFs,
    now_ms,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;

const BLOCK_SIZE: u64 = 4096;
const MAX_SYMLINK_DEPTH: usize = 40;
const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
const LEASE_RENEWAL_MARGIN: Duration = Duration::from_secs(5);
const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;
const MAX_PENDING_MUTATIONS: usize = 1024;
// A small fixed collection window lets concurrently prepared remote block
// operations enqueue their metadata mutations before the first publisher
// snapshots the queue. It is deliberately scheduler-yield based rather than
// time based so core tests remain runtime-independent and single mutations do
// not wait on an unbounded or provider-controlled delay.
const MUTATION_BATCH_YIELD_ROUNDS: usize = 8;

/// Runtime configuration for a newly-created namespace.
///
/// The chunker's serialized configuration is stored in the namespace and in
/// every file layout. Existing files always use their persisted configuration;
/// changing this value only affects a newly-created namespace.
#[derive(Clone)]
pub struct ChunkedOptions {
    pub owner: String,
    pub lease_ttl: Duration,
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
    namespace: Namespace,
    revision: u64,
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

enum MutationResult {
    WholeFile(WholeFileMutationResult),
    Unit,
}

enum WholeFileMutationResult {
    Committed,
    Conflict,
}

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
    },
    Unlink {
        path: String,
        reply: tokio::sync::oneshot::Sender<Result<MutationResult>>,
    },
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

struct ChunkedInner<M, B>
where
    M: MetadataStore,
    B: BlockStore,
{
    metadata: Arc<M>,
    blocks: Arc<B>,
    options: ChunkedOptions,
    gate: AsyncGate,
    lifecycle: tokio::sync::RwLock<()>,
    state: Mutex<RuntimeState>,
    lease: Mutex<Option<WriterLease>>,
    lease_renewed: AtomicBool,
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
    /// Acquire the provider-enforced writer lease and open the current
    /// namespace. An empty metadata store is initialized with an empty root
    /// directory through the same fenced publication path.
    pub async fn open(metadata: M, blocks: B, options: ChunkedOptions) -> Result<Self> {
        let metadata = Arc::new(metadata);
        let blocks = Arc::new(blocks);
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
                options,
                gate: AsyncGate::new(),
                lifecycle: tokio::sync::RwLock::new(()),
                state: Mutex::new(RuntimeState {
                    namespace: namespace.clone(),
                    revision: loaded.revision,
                    next_fd: 3,
                    open_refs: HashMap::new(),
                    pending_atime: HashMap::new(),
                    orphans: HashMap::new(),
                    failure: None,
                    closed: false,
                }),
                lease: Mutex::new(Some(lease.clone())),
                lease_renewed: AtomicBool::new(false),
                mutations: Mutex::new(MutationQueue::new()),
            }),
        };

        if needs_initial_publish
            && let Err(error) = filesystem
                .publish_namespace(loaded.revision, namespace, false)
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
        // Optimistic block I/O deliberately runs outside the operation gate.
        // Take the lifecycle write lock first so an in-flight write can
        // reacquire the operation gate and publish before shutdown fences and
        // releases the writer lease. New optimistic operations are prevented
        // from starting while this writer is queued.
        let _lifecycle = self.inner.lifecycle.write().await;
        let _gate = self.inner.gate.lock().await;
        self.flush_pending_atime().await?;
        // Provider I/O can outlive the lease TTL (for example, a bounded
        // remote R2 write). Refresh our own lease before releasing it so a
        // graceful shutdown is not reported as ESTALE merely because the
        // last operation was slow. `renew_lease` safely reacquires the next
        // fence when our lease expired without another owner publishing a
        // revision; a fenced instance still fails closed.
        let refresh = if self.lock_lease()?.is_some() {
            self.validate_lease().await
        } else {
            Ok(())
        };
        let lease = {
            let mut state = self.lock_lease()?;
            state.take()
        };
        {
            let mut state = self.lock_state()?;
            state.closed = true;
        }
        refresh?;
        let Some(lease) = lease else {
            return Ok(());
        };
        self.inner.metadata.release_writer(&lease).await
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
        if grace.is_zero() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("reconcile blocks")
                .with_message("reconciliation grace period must be positive"));
        }
        let _gate = self.inner.gate.lock().await;
        self.validate_lease().await?;
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
        let mut namespace = state.namespace.clone();
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
        if let Ok(mut state) = self.inner.state.lock()
            && state.failure.is_none()
        {
            state.failure = Some(error.clone());
        }
        error
    }

    async fn renew_lease(&self) -> Result<WriterLease> {
        self.renew_lease_inner(false).await
    }

    async fn validate_lease(&self) -> Result<()> {
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

        let expected_fence = match current.fence.checked_add(1) {
            Some(fence) => fence,
            None => {
                let _ = self.inner.metadata.release_writer(&acquired).await;
                return Err(self.fail_closed(
                    FsError::new(ErrorCode::Eoverflow)
                        .with_syscall("lease-acquire")
                        .with_message("writer lease fence overflow"),
                ));
            }
        };
        if acquired.owner != self.inner.options.owner || acquired.fence != expected_fence {
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
                    Ok(state.revision)
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
        self.renew_lease().await.map(|_| ())
    }

    async fn publish_namespace(
        &self,
        expected_revision: u64,
        namespace: Namespace,
        blocks_flushed: bool,
    ) -> Result<u64> {
        if !blocks_flushed {
            self.inner
                .blocks
                .flush()
                .await
                .map_err(|error| with_context(error, "block-flush", None))?;
        }
        let lease = self.renew_lease().await?;
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
        state.namespace = namespace;
        state.revision = revision;
        state.pending_atime.clear();
        Ok(revision)
    }

    /// Publish coalesced read-atime changes while the caller owns the
    /// operation gate. Ordinary mutation paths call `snapshot`, so pending
    /// values are included in any later namespace publication too.
    async fn flush_pending_atime(&self) -> Result<()> {
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
        self.enqueue_mutation(MutationRequest::WholeFile {
            mutation: Box::new(mutation),
            reply,
        })
        .await?;
        match response
            .await
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("mutation batch stopped"))??
        {
            MutationResult::WholeFile(result) => Ok(result),
            MutationResult::Unit => Err(FsError::new(ErrorCode::Eio)
                .with_syscall("write")
                .with_message("mutation batch returned the wrong result type")),
        }
    }

    async fn submit_unlink_mutation(&self, path: String) -> Result<()> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_mutation(MutationRequest::Unlink { path, reply })
            .await?;
        match response
            .await
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("mutation batch stopped"))??
        {
            MutationResult::Unit => Ok(()),
            MutationResult::WholeFile(_) => Err(FsError::new(ErrorCode::Eio)
                .with_syscall("unlink")
                .with_message("mutation batch returned the wrong result type")),
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
            // snapshots the namespace. The fixed round count is a bounded
            // batching window: it improves coalescing for remote providers
            // without turning publication into a timer or changing the
            // fenced revision/CAS boundary below.
            for _ in 0..MUTATION_BATCH_YIELD_ROUNDS {
                cooperative_yield().await;
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
        let mut responses = Vec::with_capacity(requests.len());
        let _gate = self.inner.gate.lock().await;
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

        for request in requests {
            match request {
                MutationRequest::WholeFile { mutation, reply } => {
                    if reply.is_closed() {
                        continue;
                    }
                    let mut mutation = mutation;
                    // Concurrent creates can all snapshot the same next inode
                    // before this batch publishes. Rebase only those creates
                    // that were prepared from this batch's revision; stale
                    // requests still take the serialized conflict path.
                    if mutation.new_inode && mutation.expected_revision == revision {
                        mutation.inode = namespace.next_inode;
                    }
                    let mut candidate = namespace.clone();
                    let result = apply_whole_file_mutation(&mut candidate, revision, &mutation);
                    match result {
                        Ok(WholeFileMutationResult::Committed) => {
                            namespace = candidate;
                            changed = true;
                            responses.push((
                                reply,
                                Ok(MutationResult::WholeFile(
                                    WholeFileMutationResult::Committed,
                                )),
                            ));
                        }
                        Ok(WholeFileMutationResult::Conflict) => {
                            responses.push((
                                reply,
                                Ok(MutationResult::WholeFile(WholeFileMutationResult::Conflict)),
                            ));
                        }
                        Err(error) => responses.push((reply, Err(error))),
                    }
                }
                MutationRequest::Unlink { path, reply } => {
                    if reply.is_closed() {
                        continue;
                    }
                    let mut candidate = namespace.clone();
                    match apply_unlink_mutation(self, &mut candidate, &path) {
                        Ok(()) => {
                            namespace = candidate;
                            changed = true;
                            responses.push((reply, Ok(MutationResult::Unit)));
                        }
                        Err(error) => responses.push((reply, Err(error))),
                    }
                }
            }
        }

        if changed && let Err(error) = self.publish_namespace(revision, namespace, true).await {
            for (reply, _) in responses {
                let _ = reply.send(Err(error.clone()));
            }
            return;
        }
        for (reply, result) in responses {
            let _ = reply.send(result);
        }
    }

    async fn mutate<F, R>(&self, operation: F) -> Result<R>
    where
        F: FnOnce(&mut Namespace) -> Result<R>,
    {
        let _gate = self.inner.gate.lock().await;
        self.ensure_operation_lease().await?;
        let (mut namespace, revision) = self.snapshot()?;
        let result = operation(&mut namespace)?;
        self.publish_namespace(revision, namespace, false).await?;
        Ok(result)
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
            let _gate = self.inner.gate.lock().await;
            self.ensure_operation_lease().await?;
            let (namespace, _) = self.snapshot()?;
            let (node, orphan) = self.node_snapshot(&namespace, inode, "read", path)?;
            let layout = match &node.data {
                NodeData::File(layout) => layout.clone(),
                NodeData::Directory { .. } => {
                    return Err(error_with_path(ErrorCode::Eisdir, "read", path));
                }
                NodeData::Special => return Err(error_with_path(ErrorCode::Enxio, "read", path)),
                NodeData::Symlink { .. } => {
                    return Err(error_with_path(ErrorCode::Eio, "read", path));
                }
            };
            let size = node.stats.size;
            let count = if position >= size {
                0
            } else {
                let available = size - position;
                buffer
                    .len()
                    .min(usize::try_from(available).unwrap_or(usize::MAX))
            };
            (layout, node, orphan, size, count)
        };
        if count > 0 {
            read_layout_into(
                &self.inner.blocks,
                &layout,
                size,
                position,
                &mut buffer[..count],
                path,
                "read",
            )
            .await?;
        }
        let _gate = self.inner.gate.lock().await;
        self.ensure_operation_lease().await?;
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
        if namespace
            .nodes
            .get(&inode)
            .is_some_and(|node| write_base_unchanged(node, &original))
        {
            let atime_ms = now_ms();
            let mut state = self.lock_state()?;
            state
                .pending_atime
                .entry(inode)
                .and_modify(|pending| *pending = (*pending).max(atime_ms))
                .or_insert(atime_ms);
        } else if let Some(node) = self.lock_state()?.orphans.get_mut(&inode)
            && write_base_unchanged(node, &original)
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
        let (layout, original, orphan, start, end, new_size) = {
            let _gate = self.inner.gate.lock().await;
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
        self.inner
            .blocks
            .flush()
            .await
            .map_err(|error| with_context(error, "block-flush", Some(path)))?;

        let fast_commit = {
            let _gate = self.inner.gate.lock().await;
            self.ensure_operation_lease().await?;
            if orphan {
                let mut state = self.lock_state()?;
                match state.orphans.get_mut(&inode) {
                    Some(target) if write_base_unchanged(target, &original) => {
                        target.data = NodeData::File(new_layout);
                        set_file_size(&mut target.stats, new_size);
                        touch_modified(&mut target.stats);
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
                    touch_modified(&mut target.stats);
                    self.publish_namespace(revision, namespace, true).await?;
                    Some((buffer.len(), end))
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
        let _gate = self.inner.gate.lock().await;
        self.ensure_operation_lease().await?;
        let (mut namespace, revision) = self.snapshot()?;
        let (node, orphan) = self.node_snapshot(&namespace, inode, "write", path)?;
        let layout = match &node.data {
            NodeData::File(layout) => layout.clone(),
            NodeData::Directory { .. } => {
                return Err(error_with_path(ErrorCode::Eisdir, "write", path));
            }
            NodeData::Special => return Err(error_with_path(ErrorCode::Enxio, "write", path)),
            NodeData::Symlink { .. } => return Err(error_with_path(ErrorCode::Eio, "write", path)),
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
        self.inner
            .blocks
            .flush()
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
            touch_modified(&mut target.stats);
            return Ok((buffer.len(), end));
        }
        let target = namespace
            .nodes
            .get_mut(&inode)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, "write", path))?;
        target.data = NodeData::File(new_layout);
        set_file_size(&mut target.stats, new_size);
        touch_modified(&mut target.stats);
        self.publish_namespace(revision, namespace, true).await?;
        Ok((buffer.len(), end))
    }

    async fn write_file_atomic(&self, path: &str, data: &[u8]) -> Result<()> {
        let normalized = normalize_path(path);
        let _lifecycle = self.inner.lifecycle.read().await;
        let (layout, original, inode, expected_revision, new_inode) = {
            let _gate = self.inner.gate.lock().await;
            self.ensure_operation_lease().await?;
            let (namespace, revision) = self.snapshot()?;
            let entry = walk(&namespace, &normalized, true, "open", 0)?;
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
        if !data.is_empty() {
            self.inner
                .blocks
                .flush()
                .await
                .map_err(|error| with_context(error, "block-flush", Some(&normalized)))?;
        }

        let committed = self
            .submit_whole_file_mutation(WholeFileMutation {
                path: normalized.clone(),
                inode,
                expected_revision,
                new_inode,
                original,
                layout: new_layout,
                data_length: u64::try_from(data.len())
                    .map_err(|_| error_with_path(ErrorCode::Efbig, "write", &normalized))?,
            })
            .await?;
        if matches!(committed, WholeFileMutationResult::Committed) {
            return Ok(());
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

    async fn truncate_inode(&self, inode: InodeId, path: &str, length: u64) -> Result<()> {
        let _gate = self.inner.gate.lock().await;
        self.ensure_operation_lease().await?;
        let (mut namespace, revision) = self.snapshot()?;
        let (node, orphan) = self.node_snapshot(&namespace, inode, "ftruncate", path)?;
        let mut layout = match &node.data {
            NodeData::File(layout) => layout.clone(),
            NodeData::Directory { .. } => {
                return Err(error_with_path(ErrorCode::Eisdir, "ftruncate", path));
            }
            NodeData::Special => return Err(error_with_path(ErrorCode::Enxio, "ftruncate", path)),
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
            touch_modified(&mut target.stats);
            return Ok(());
        }
        let target = namespace
            .nodes
            .get_mut(&inode)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, "ftruncate", path))?;
        target.data = NodeData::File(layout);
        set_file_size(&mut target.stats, length);
        touch_modified(&mut target.stats);
        self.publish_namespace(revision, namespace, false).await?;
        Ok(())
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

    async fn close_inode(&self, inode: InodeId) -> Result<()> {
        let _gate = self.inner.gate.lock().await;
        self.ensure_operation_lease().await?;
        let (namespace, revision) = {
            let mut state = self.lock_state()?;
            if let Some(count) = state.open_refs.get_mut(&inode) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.open_refs.remove(&inode);
                }
            }
            if state.failure.is_some() || state.closed {
                return Ok(());
            }
            if state.orphans.contains_key(&inode) && !state.open_refs.contains_key(&inode) {
                state.orphans.remove(&inode);
                return Ok(());
            }
            let should_remove =
                state.namespace.nodes.get(&inode).is_some_and(|node| {
                    node.stats.nlink == 0 && !state.open_refs.contains_key(&inode)
                });
            if !should_remove {
                return Ok(());
            }
            (state.namespace.clone(), state.revision)
        };
        let mut namespace = namespace;
        namespace.nodes.remove(&inode);
        self.publish_namespace(revision, namespace, false).await?;
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
        let _gate = self.inner.gate.lock().await;
        self.ensure_operation_lease().await?;
        self.snapshot()?;
        self.flush_pending_atime().await?;
        self.inner
            .blocks
            .flush()
            .await
            .map_err(|error| with_context(error, syscall, None))?;
        self.validate_lease().await?;
        self.inner
            .metadata
            .flush()
            .await
            .map_err(|error| self.fail_closed(with_context(error, syscall, None)))?;
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
            node.stats.ctime_ms = now_ms();
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
            node.stats.ctime_ms = now_ms();
            Ok(())
        })
        .await
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

    fn check_open(&self, write: bool, syscall: &str) -> Result<u64> {
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
        {
            let mut state = self.lock_state()?;
            if state.closed {
                return Ok(());
            }
            state.closed = true;
        }
        self.filesystem.close_inode(self.inode).await
    }
}

#[async_trait]
impl<M, B> FsDriver for ChunkedFs<M, B>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
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
        let _gate = self.inner.gate.lock().await;
        self.validate_lease().await?;
        let (namespace, _) = self.snapshot()?;
        let inode = resolve(&namespace, path, true, "stat")?;
        self.stat_inode(inode, "stat", &normalize_path(path))
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        let _gate = self.inner.gate.lock().await;
        self.validate_lease().await?;
        let (namespace, _) = self.snapshot()?;
        let inode = resolve(&namespace, path, false, "lstat")?;
        self.stat_inode(inode, "lstat", &normalize_path(path))
    }

    async fn statfs(&self, path: &str) -> Result<StatsFs> {
        let _gate = self.inner.gate.lock().await;
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
        let _gate = self.inner.gate.lock().await;
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
        let _gate = self.inner.gate.lock().await;
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
        if !flags.read && !flags.write {
            return Err(error_with_path(ErrorCode::Einval, "open", path));
        }
        if flags.truncate && !flags.write {
            return Err(error_with_path(ErrorCode::Einval, "open", path));
        }
        let _gate = self.inner.gate.lock().await;
        self.ensure_operation_lease().await?;
        let (mut namespace, revision) = self.snapshot()?;
        let normalized = normalize_path(path);
        let entry = walk(
            &namespace,
            &normalized,
            !(flags.create && flags.exclusive),
            "open",
            0,
        )?;
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
                touch_modified(&mut node.stats);
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
            )?;
            inode
        };

        let changed = entry.node.is_none() || flags.truncate;
        if changed {
            self.publish_namespace(revision, namespace, false).await?;
        }
        let fd = self.allocate_fd(inode)?;
        Ok(Arc::new(ChunkedHandle {
            filesystem: self.clone(),
            inode,
            path: normalized,
            fd,
            flags,
            state: Mutex::new(HandleState {
                position: 0,
                closed: false,
            }),
            gate: AsyncGate::new(),
        }))
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
                        )?;
                        first_created.get_or_insert(current.clone());
                    }
                }
                Ok(first_created)
            } else {
                let entry = walk(namespace, &normalized, false, "mkdir", 0)?;
                if entry.node.is_some() {
                    return Err(error_with_path(ErrorCode::Eexist, "mkdir", &entry.path));
                }
                let inode = namespace.next_inode;
                namespace.next_inode = namespace
                    .next_inode
                    .checked_add(1)
                    .ok_or_else(|| error_with_path(ErrorCode::Eoverflow, "mkdir", &normalized))?;
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
                    &normalized,
                )?;
                Ok(None)
            }
        })
        .await
    }

    async fn rmdir(&self, path: &str) -> Result<()> {
        let normalized = normalize_path(path);
        let runtime = self.clone();
        self.mutate(|namespace| {
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
            )?;
            runtime.reap_detached(namespace, inode)
        })
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
                        .with_dest(new_normalized),
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
                                .with_dest(new_normalized),
                        );
                    }
                    if let Some(NodeMetadata {
                        data: NodeData::Directory { entries },
                        ..
                    }) = namespace.nodes.get(&destination)
                        && !entries.is_empty()
                    {
                        return Err(error_with_path(
                            ErrorCode::Enotempty,
                            "rename",
                            &old_normalized,
                        )
                        .with_dest(new_normalized));
                    }
                } else if destination_is_dir {
                    return Err(
                        error_with_path(ErrorCode::Eisdir, "rename", &old_normalized)
                            .with_dest(new_normalized),
                    );
                }
                detach_entry(namespace, to.parent, &to.name, true, "rename", &to.path)?;
                runtime.reap_detached(namespace, destination)?;
            }
            detach_entry(
                namespace,
                from.parent,
                &from.name,
                false,
                "rename",
                &from.path,
            )?;
            add_entry(namespace, to.parent, to.name, source, "rename", &to.path)?;
            if let Some(node) = namespace.nodes.get_mut(&source) {
                node.stats.ctime_ms = now_ms();
            }
            Ok(())
        })
        .await
    }

    async fn link(&self, existing_path: &str, new_path: &str) -> Result<()> {
        let existing = normalize_path(existing_path);
        let new_path = normalize_path(new_path);
        self.mutate(|namespace| {
            let from = walk(namespace, &existing, false, "link", 0)?;
            let inode = from.node.ok_or_else(|| {
                error_with_path(ErrorCode::Enoent, "link", &from.path).with_dest(new_path.clone())
            })?;
            if namespace
                .nodes
                .get(&inode)
                .is_some_and(|node| matches!(node.data, NodeData::Directory { .. }))
            {
                return Err(error_with_path(ErrorCode::Eperm, "link", &from.path)
                    .with_dest(new_path.clone()));
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
                node.stats.ctime_ms = now_ms();
            }
            add_entry(namespace, to.parent, to.name, inode, "link", &to.path)
        })
        .await
    }

    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        let normalized = normalize_path(path);
        self.mutate(|namespace| {
            if target.is_empty() {
                return Err(error_with_path(ErrorCode::Enoent, "symlink", target)
                    .with_dest(normalized.clone()));
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
                        size: u64::try_from(target.len()).map_err(|_| {
                            error_with_path(ErrorCode::Efbig, "symlink", &entry.path)
                        })?,
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
            )
        })
        .await
    }

    async fn readlink(&self, path: &str) -> Result<String> {
        let _gate = self.inner.gate.lock().await;
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
            node.stats.ctime_ms = now_ms();
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
            touch_modified(&mut node.stats);
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
            )
        })
        .await
    }
}

fn mutation_reply(request: MutationRequest, result: Result<MutationResult>) {
    match request {
        MutationRequest::WholeFile { reply, .. } | MutationRequest::Unlink { reply, .. } => {
            let _ = reply.send(result);
        }
    }
}

fn apply_whole_file_mutation(
    namespace: &mut Namespace,
    current_revision: u64,
    mutation: &WholeFileMutation,
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
        )?;
        let target = namespace
            .nodes
            .get_mut(&mutation.inode)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, "write", &mutation.path))?;
        target.data = NodeData::File(mutation.layout.clone());
        set_file_size(&mut target.stats, mutation.data_length);
        touch_modified(&mut target.stats);
        return Ok(WholeFileMutationResult::Committed);
    }

    if current_revision != mutation.expected_revision {
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
    touch_modified(&mut target.stats);
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
    )?;
    runtime.reap_detached(namespace, inode)
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

fn touch_modified(stats: &mut Stats) {
    let now = now_ms();
    stats.mtime_ms = now;
    stats.ctime_ms = now;
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
    touch_modified(&mut parent_node.stats);
    Ok(())
}

fn detach_entry(
    namespace: &mut Namespace,
    parent: InodeId,
    name: &str,
    decrement_link: bool,
    syscall: &str,
    path: &str,
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
        touch_modified(&mut parent_node.stats);
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
        node.stats.ctime_ms = now_ms();
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
            let write_start = position.max(chunk_start);
            let chunk_end = chunk_start
                .checked_add(chunk_length_u64)
                .ok_or_else(|| error_with_path(ErrorCode::Efbig, "write", path))?;
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

    #[derive(Clone)]
    struct FaultBlockStore {
        inner: MemoryBlockStore,
        fail_put: Arc<AtomicBool>,
        fail_flush: Arc<AtomicBool>,
        gets: Arc<AtomicUsize>,
        reconciled: Arc<Mutex<Option<BTreeSet<mount_rs_core::storage::BlockId>>>>,
    }

    impl FaultBlockStore {
        fn new() -> Self {
            Self {
                inner: MemoryBlockStore::new(),
                fail_put: Arc::new(AtomicBool::new(false)),
                fail_flush: Arc::new(AtomicBool::new(false)),
                gets: Arc::new(AtomicUsize::new(0)),
                reconciled: Arc::new(Mutex::new(None)),
            }
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

    fn options(owner: &str) -> ChunkedOptions {
        ChunkedOptions::fixed(owner, 4)
            .unwrap()
            .with_lease_ttl(Duration::from_secs(10))
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
        const PARTICIPANTS: usize = 4;
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
    fn concurrent_whole_file_creates_rebase_inodes_in_one_publication() {
        const PARTICIPANTS: usize = 4;
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
}
