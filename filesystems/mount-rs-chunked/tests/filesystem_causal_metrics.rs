//! RED controls for causal counters at real filesystem/provider boundaries.
//!
//! Run this integration binary alone with MOUNT_RS_PROFILE_IO=1 and one test
//! thread: the existing core recorder is process-global and snapshots allocate.
//! Fixtures count actual provider dispatches independently of proposed metrics.

use async_trait::async_trait;
use futures_lite::future::block_on;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::chunking::{Chunker, ChunkerConfig, FixedSizeChunker};
use mount_rs_core::diagnostics::profile::{self, Snapshot};
use mount_rs_core::storage::{
    BlockId, BlockStore, ConcurrentBackingId, ConcurrentModeState, FileLayout, LoadedMetadata,
    MetadataStore, Namespace, NodeData, WriterLease,
};
use mount_rs_core::{ErrorCode, FsDriver, FsError, Result};
use mount_rs_memory::{ManualClock, MemoryBlockStore, MemoryMetadataStore};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

#[derive(Default)]
struct CasState {
    revision: u64,
    namespace: Option<Namespace>,
    backing: Option<ConcurrentBackingId>,
}

#[derive(Clone, Default)]
struct CasMetadata {
    state: Arc<Mutex<CasState>>,
    forced_conflicts: Arc<AtomicU64>,
    attempts: Arc<AtomicU64>,
    conflicts: Arc<AtomicU64>,
    commits: Arc<AtomicU64>,
    hold_next: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
    resume: Arc<tokio::sync::Notify>,
    next_winner_chunker: Arc<Mutex<Option<ChunkerConfig>>>,
    hold_after_commit: Arc<AtomicBool>,
    committed_entered: Arc<AtomicBool>,
    committed_resume: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl MetadataStore for CasMetadata {
    fn durable(&self) -> bool {
        false
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let state = self.state.lock().unwrap();
        Ok(LoadedMetadata {
            revision: state.revision,
            namespace: state.namespace.clone(),
        })
    }

    async fn load_if_changed(&self, known: u64) -> Result<Option<LoadedMetadata>> {
        let loaded = self.load().await?;
        Ok((loaded.revision != known).then_some(loaded))
    }

    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .backing
            .map_or(ConcurrentModeState::Legacy, ConcurrentModeState::Mrc2))
    }

    async fn preflight_new_bound_mode(&self) -> Result<()> {
        let state = self.state.lock().unwrap();
        if state.revision == 0 && state.namespace.is_none() && state.backing.is_none() {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Ebusy))
        }
    }

    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.backing.is_some_and(|actual| actual != backing) {
            return Err(FsError::new(ErrorCode::Estale));
        }
        state.backing = Some(backing);
        Ok(())
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
        _revision: u64,
        _lease: &WriterLease,
        _namespace: Namespace,
    ) -> Result<u64> {
        Err(FsError::new(ErrorCode::Enotsup))
    }

    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        namespace.validate()?;
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.hold_next.swap(false, Ordering::SeqCst) {
            self.entered.store(true, Ordering::SeqCst);
            self.resume.notified().await;
        }
        let committed_revision = {
            let mut state = self.state.lock().unwrap();
            if state.backing != Some(backing) {
                return Err(FsError::new(ErrorCode::Estale));
            }
            if self
                .forced_conflicts
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                // A separate writer advances a valid namespace. By default
                // it changes only the revision; the chunker control also
                // commits a validated winner configuration for a new path.
                if let Some(chunker) = self.next_winner_chunker.lock().unwrap().take() {
                    let mut winner = state.namespace.clone().unwrap();
                    winner.default_chunker = chunker;
                    winner.validate()?;
                    state.namespace = Some(winner);
                }
                state.revision += 1;
                self.conflicts.fetch_add(1, Ordering::SeqCst);
                return Err(FsError::new(ErrorCode::Eagain));
            }
            if state.revision != revision {
                self.conflicts.fetch_add(1, Ordering::SeqCst);
                return Err(FsError::new(ErrorCode::Eagain));
            }
            state.revision += 1;
            state.namespace = Some(namespace);
            self.commits.fetch_add(1, Ordering::SeqCst);
            state.revision
        };
        // Release the provider's state mutex before suspension. The marker
        // establishes a real commit while the runner has no returned receipt.
        if self.hold_after_commit.swap(false, Ordering::SeqCst) {
            self.committed_entered.store(true, Ordering::SeqCst);
            self.committed_resume.notified().await;
        }
        Ok(committed_revision)
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct BlockCounts {
    calls: AtomicU64,
    bytes: AtomicU64,
    success: AtomicU64,
    error: AtomicU64,
    cancelled: AtomicU64,
    gets: AtomicU64,
    hold_next: AtomicBool,
    hold_at_call: AtomicU64,
    fail_next: AtomicBool,
}

struct PutDispatch<'a> {
    counts: &'a BlockCounts,
    finished: bool,
}

impl Drop for PutDispatch<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.counts.cancelled.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Clone)]
struct CountingBlocks {
    inner: MemoryBlockStore,
    counts: Arc<BlockCounts>,
    backing: ConcurrentBackingId,
}

impl Default for CountingBlocks {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let mut bytes = [0xc7; 16];
        bytes[8..].copy_from_slice(&NEXT.fetch_add(1, Ordering::SeqCst).to_be_bytes());
        Self {
            inner: MemoryBlockStore::new(),
            counts: Arc::new(BlockCounts::default()),
            backing: ConcurrentBackingId::from_bytes(bytes).unwrap(),
        }
    }
}

#[async_trait]
impl BlockStore for CountingBlocks {
    fn durable(&self) -> bool {
        false
    }

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        Ok(self.backing)
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        if expected == self.backing {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Estale))
        }
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let call = self.counts.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.counts
            .bytes
            .fetch_add(bytes.len() as u64, Ordering::SeqCst);
        let mut dispatch = PutDispatch {
            counts: &self.counts,
            finished: false,
        };
        if self.counts.hold_next.swap(false, Ordering::SeqCst)
            || self.counts.hold_at_call.load(Ordering::SeqCst) == call
        {
            std::future::pending::<()>().await;
        }
        let result = if self.counts.fail_next.swap(false, Ordering::SeqCst) {
            Err(FsError::new(ErrorCode::Eio))
        } else {
            self.inner.put(bytes).await
        };
        dispatch.finished = true;
        if result.is_ok() {
            self.counts.success.fetch_add(1, Ordering::SeqCst);
        } else {
            self.counts.error.fetch_add(1, Ordering::SeqCst);
        }
        result
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.counts.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.inner.delete(id).await
    }
}

type Fs = ChunkedFs<CasMetadata, CountingBlocks>;

fn open() -> (CasMetadata, CountingBlocks, Fs) {
    assert!(profile::enabled(), "run with MOUNT_RS_PROFILE_IO=1");
    let metadata = CasMetadata::default();
    let blocks = CountingBlocks::default();
    let fs = block_on(ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("causal-control", 16)
            .unwrap()
            .with_concurrent_writes(true),
    ))
    .unwrap();
    (metadata, blocks, fs)
}

fn row(snapshot: &Snapshot, name: &str) -> (u64, u64) {
    snapshot
        .entries
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| (entry.calls, entry.units))
        .unwrap_or_else(|| panic!("missing causal metric {name} after real filesystem operation"))
}

fn delta(before: &Snapshot, after: &Snapshot, name: &str) -> (u64, u64) {
    let now = row(after, name);
    let old = row(before, name);
    (
        now.0.checked_sub(old.0).unwrap(),
        now.1.checked_sub(old.1).unwrap(),
    )
}

fn read_file(fs: &Fs, path: &str, expected: &[u8]) {
    block_on(async {
        let handle = fs.open(path, "r", 0).await.unwrap();
        let mut bytes = vec![0; expected.len()];
        assert_eq!(
            handle.read(&mut bytes, Some(0)).await.unwrap(),
            expected.len()
        );
        assert_eq!(bytes, expected);
        handle.close().await.unwrap();
    });
}

fn root_file_layout<'a>(namespace: &'a Namespace, name: &str) -> &'a FileLayout {
    let NodeData::Directory { entries } = &namespace.nodes[&namespace.root].data else {
        panic!("validated namespace root must be a directory");
    };
    let inode = entries
        .iter()
        .find(|entry| entry.name == name)
        .unwrap()
        .inode;
    let NodeData::File(layout) = &namespace.nodes[&inode].data else {
        panic!("committed entry must be a regular file");
    };
    layout
}

struct NoopWake;
impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = Waker::from(Arc::new(NoopWake));
    future.poll(&mut Context::from_waker(&waker))
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn partial_overwrite_attributes_initial_fallback_and_retry_puts() {
    let (metadata, blocks, fs) = open();
    block_on(fs.write_file("/data", b"abcdefgh")).unwrap();
    let handle = block_on(fs.open("/data", "r+", 0)).unwrap();
    let calls = blocks.counts.calls.load(Ordering::SeqCst);
    let bytes = blocks.counts.bytes.load(Ordering::SeqCst);
    let gets = blocks.counts.gets.load(Ordering::SeqCst);
    let attempts = metadata.attempts.load(Ordering::SeqCst);
    let commits = metadata.commits.load(Ordering::SeqCst);
    let before = profile::snapshot();
    metadata.forced_conflicts.store(2, Ordering::SeqCst);
    assert_eq!(block_on(handle.write(b"XY", Some(2))).unwrap(), 2);
    let after = profile::snapshot();
    assert_eq!(blocks.counts.calls.load(Ordering::SeqCst) - calls, 3);
    assert_eq!(blocks.counts.bytes.load(Ordering::SeqCst) - bytes, 24);
    assert_eq!(blocks.counts.gets.load(Ordering::SeqCst) - gets, 3);
    assert_eq!(metadata.attempts.load(Ordering::SeqCst) - attempts, 3);
    assert_eq!(metadata.commits.load(Ordering::SeqCst) - commits, 1);
    assert_eq!(metadata.conflicts.load(Ordering::SeqCst), 2);
    read_file(&fs, "/data", b"abXYefgh");
    block_on(handle.close()).unwrap();
    block_on(fs.shutdown()).unwrap();
    // Two application input bytes produce three actual eight-byte put calls.
    for reason in ["initial", "fallback", "retry_rewrite"] {
        assert_eq!(
            delta(&before, &after, &format!("filesystem.block_put.{reason}")),
            (1, 8)
        );
        assert_eq!(
            delta(
                &before,
                &after,
                &format!("filesystem.block_put.{reason}.success")
            ),
            (1, 8)
        );
    }
    assert_eq!(
        delta(&before, &after, "filesystem.gate_phase.cas_backoff").0,
        1
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn dropped_pending_put_records_attempted_bytes_and_cancellation() {
    let (_, blocks, fs) = open();
    let calls = blocks.counts.calls.load(Ordering::SeqCst);
    let before = profile::snapshot();
    blocks.counts.hold_next.store(true, Ordering::SeqCst);
    let mut write = Box::pin(fs.write_file("/cancelled", b"four"));
    assert!(poll_once(write.as_mut()).is_pending());
    assert_eq!(blocks.counts.calls.load(Ordering::SeqCst) - calls, 1);
    drop(write);
    let after = profile::snapshot();
    assert_eq!(blocks.counts.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(
        block_on(fs.stat("/cancelled")).unwrap_err().code,
        ErrorCode::Enoent
    );
    block_on(fs.shutdown()).unwrap();
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial"),
        (1, 4)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial.cancelled"),
        (1, 0)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial.success"),
        (0, 0)
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn failed_put_preserves_error_and_zero_successful_input_bytes() {
    let (_, blocks, fs) = open();
    let before = profile::snapshot();
    blocks.counts.fail_next.store(true, Ordering::SeqCst);
    assert_eq!(
        block_on(fs.write_file("/failed", b"four"))
            .unwrap_err()
            .code,
        ErrorCode::Eio
    );
    let after = profile::snapshot();
    assert_eq!(blocks.counts.calls.load(Ordering::SeqCst), 1);
    assert_eq!(blocks.counts.bytes.load(Ordering::SeqCst), 4);
    assert_eq!(blocks.counts.error.load(Ordering::SeqCst), 1);
    assert_eq!(
        block_on(fs.stat("/failed")).unwrap_err().code,
        ErrorCode::Enoent
    );
    block_on(fs.shutdown()).unwrap();
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial"),
        (1, 4)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial.error"),
        (1, 0)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial.success"),
        (0, 0)
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn coalesced_conflicting_batch_separates_candidate_replay_from_sent_replies() {
    let (metadata, blocks, fs) = open();
    let attempts = metadata.attempts.load(Ordering::SeqCst);
    let commits = metadata.commits.load(Ordering::SeqCst);
    let before = profile::snapshot();
    metadata.forced_conflicts.store(1, Ordering::SeqCst);
    let mut first = Box::pin(fs.write_file("/first", b"left"));
    assert!(poll_once(first.as_mut()).is_pending());
    let mut second = Box::pin(fs.write_file("/second", b"right"));
    assert!(poll_once(second.as_mut()).is_pending());
    block_on(first).unwrap();
    block_on(second).unwrap();
    let after = profile::snapshot();
    assert_eq!(blocks.counts.calls.load(Ordering::SeqCst), 2);
    assert_eq!(blocks.counts.bytes.load(Ordering::SeqCst), 9);
    assert_eq!(metadata.attempts.load(Ordering::SeqCst) - attempts, 2);
    assert_eq!(metadata.commits.load(Ordering::SeqCst) - commits, 1);
    assert_eq!(metadata.conflicts.load(Ordering::SeqCst), 1);
    // One confirmed provider CAS loss retries the same two prepared creates
    // against a fresh candidate; both then commit in one publication.
    assert_eq!(
        delta(
            &before,
            &after,
            "filesystem.mutation_batch_attempted_requests"
        ),
        (1, 2)
    );
    read_file(&fs, "/first", b"left");
    read_file(&fs, "/second", b"right");
    block_on(fs.shutdown()).unwrap();
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.attempt_requests"),
        (2, 4)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.attempt.conflict"),
        (1, 2)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.attempt.success"),
        (1, 2)
    );
    assert_eq!(
        delta(
            &before,
            &after,
            "filesystem.mutation.attempt.no_publication"
        ),
        (0, 0)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.enqueue_requests").1,
        2
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.dequeue_requests").1,
        2
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.queue_wait_requests"),
        (2, 2)
    );
    assert!(delta(&before, &after, "filesystem.mutation.coalescing_yields").1 >= 1);
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.committed").1,
        2
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.conflict").1,
        0
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.reply_sent").1,
        2
    );
    assert_eq!(
        delta(&before, &after, "filesystem.gate_hold.whole_file_replay").0,
        0
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn canceled_queued_request_is_skipped_and_not_counted_as_committed() {
    let (metadata, blocks, fs) = open();
    let commits = metadata.commits.load(Ordering::SeqCst);
    let before = profile::snapshot();
    let mut first = Box::pin(fs.write_file("/first", b"left"));
    assert!(poll_once(first.as_mut()).is_pending());
    let mut cancelled = Box::pin(fs.write_file("/cancelled", b"right"));
    assert!(poll_once(cancelled.as_mut()).is_pending());
    drop(cancelled);
    block_on(first).unwrap();
    let after = profile::snapshot();
    assert_eq!(blocks.counts.calls.load(Ordering::SeqCst), 2);
    assert_eq!(metadata.commits.load(Ordering::SeqCst) - commits, 1);
    read_file(&fs, "/first", b"left");
    assert_eq!(
        block_on(fs.stat("/cancelled")).unwrap_err().code,
        ErrorCode::Enoent
    );
    block_on(fs.shutdown()).unwrap();
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.attempt_requests"),
        (1, 1)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.cancelled").1,
        1
    );
    assert_eq!(
        delta(
            &before,
            &after,
            "filesystem.mutation.request.receiver_closed"
        )
        .1,
        1
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.committed").1,
        1
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.reply_sent").1,
        1
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn canceled_metadata_waiter_has_no_hold_while_publication_owns_gate() {
    let (metadata, _, fs) = open();
    let before = profile::snapshot();
    metadata.hold_next.store(true, Ordering::SeqCst);
    let mut write = Box::pin(fs.write_file("/held", b"four"));
    for _ in 0..256 {
        assert!(poll_once(write.as_mut()).is_pending());
        if metadata.entered.load(Ordering::SeqCst) {
            break;
        }
    }
    assert!(
        metadata.entered.load(Ordering::SeqCst),
        "publication was never dispatched"
    );
    let mut waiter = Box::pin(fs.stat("/"));
    assert!(poll_once(waiter.as_mut()).is_pending());
    drop(waiter);
    metadata.resume.notify_one();
    block_on(write).unwrap();
    let after = profile::snapshot();
    read_file(&fs, "/held", b"four");
    block_on(fs.shutdown()).unwrap();
    assert_eq!(delta(&before, &after, "filesystem.gate_wait.metadata").0, 1);
    assert_eq!(
        delta(&before, &after, "filesystem.gate_wait.metadata.cancelled").0,
        1
    );
    assert_eq!(delta(&before, &after, "filesystem.gate_hold.metadata").0, 0);
    assert_eq!(
        delta(&before, &after, "filesystem.gate_hold.mutation_batch").0,
        1
    );
    assert_eq!(
        delta(&before, &after, "filesystem.gate_phase.publication").0,
        1
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn manual_clock_lease_recovery_is_observed_under_an_acquired_gate() {
    assert!(profile::enabled(), "run with MOUNT_RS_PROFILE_IO=1");
    let clock = Arc::new(ManualClock::new(0));
    let metadata = MemoryMetadataStore::with_clock(clock.clone());
    let fs = block_on(ChunkedFs::open(
        metadata.clone(),
        MemoryBlockStore::new(),
        ChunkedOptions::fixed("lease-control", 16)
            .unwrap()
            .with_lease_ttl(Duration::from_secs(1)),
    ))
    .unwrap();
    let revision = block_on(metadata.load()).unwrap().revision;
    let before = profile::snapshot();
    assert!(clock.advance_ms(1_000));
    block_on(fs.stat("/")).unwrap();
    let after = profile::snapshot();
    assert_eq!(block_on(metadata.load()).unwrap().revision, revision);
    assert!(!fs.failed());
    block_on(fs.shutdown()).unwrap();
    assert_eq!(delta(&before, &after, "filesystem.gate_hold.metadata").0, 1);
    assert_eq!(delta(&before, &after, "filesystem.gate_phase.refresh").0, 1);
    assert_eq!(
        delta(&before, &after, "filesystem.gate_phase.recovery").0,
        1
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn changed_chunker_replay_attributes_only_actual_reprepared_puts() {
    let (metadata, blocks, fs) = open();
    let attempts = metadata.attempts.load(Ordering::SeqCst);
    let commits = metadata.commits.load(Ordering::SeqCst);
    let winner_chunker = FixedSizeChunker::new(4).unwrap().config();
    *metadata.next_winner_chunker.lock().unwrap() = Some(winner_chunker.clone());
    metadata.forced_conflicts.store(1, Ordering::SeqCst);
    let before = profile::snapshot();
    block_on(fs.write_file("/changed", b"ABCDEFGH")).unwrap();
    let after = profile::snapshot();
    assert_eq!(blocks.counts.calls.load(Ordering::SeqCst), 3);
    assert_eq!(blocks.counts.bytes.load(Ordering::SeqCst), 16);
    assert_eq!(blocks.counts.success.load(Ordering::SeqCst), 3);
    assert_eq!(blocks.counts.error.load(Ordering::SeqCst), 0);
    assert_eq!(blocks.counts.cancelled.load(Ordering::SeqCst), 0);
    assert_eq!(metadata.attempts.load(Ordering::SeqCst) - attempts, 2);
    assert_eq!(metadata.commits.load(Ordering::SeqCst) - commits, 1);
    assert_eq!(metadata.conflicts.load(Ordering::SeqCst), 1);
    let loaded = block_on(metadata.load()).unwrap();
    let namespace = loaded.namespace.unwrap();
    namespace.validate().unwrap();
    assert_eq!(namespace.default_chunker, winner_chunker);
    let layout = root_file_layout(&namespace, "changed");
    assert_eq!(layout.chunker, winner_chunker);
    assert_eq!(layout.extents.len(), 2);
    assert!(layout.extents.iter().all(|extent| extent.length == 4));
    read_file(&fs, "/changed", b"ABCDEFGH");
    block_on(fs.shutdown()).unwrap();
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial"),
        (1, 8)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial.success"),
        (1, 8)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.chunker_reprepare"),
        (2, 8)
    );
    assert_eq!(
        delta(
            &before,
            &after,
            "filesystem.block_put.chunker_reprepare.success"
        ),
        (2, 8)
    );
    for reason in ["fallback", "retry_rewrite"] {
        assert_eq!(
            delta(&before, &after, &format!("filesystem.block_put.{reason}")),
            (0, 0)
        );
    }
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.attempt.conflict"),
        (1, 1)
    );
    assert_eq!(
        delta(
            &before,
            &after,
            "filesystem.mutation.attempt.no_publication"
        ),
        (1, 1)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.gate_hold.whole_file_replay").0,
        1
    );
    assert_eq!(
        delta(&before, &after, "filesystem.gate_phase.block_rewrite").0,
        1
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn empty_and_all_zero_whole_files_dispatch_no_block_puts() {
    let (metadata, blocks, fs) = open();
    let attempts = metadata.attempts.load(Ordering::SeqCst);
    let commits = metadata.commits.load(Ordering::SeqCst);
    let zeros = [0_u8; 37];
    let before = profile::snapshot();
    block_on(fs.write_file("/empty", b"")).unwrap();
    block_on(fs.write_file("/zeros", &zeros)).unwrap();
    let after = profile::snapshot();
    assert_eq!(blocks.counts.calls.load(Ordering::SeqCst), 0);
    assert_eq!(blocks.counts.bytes.load(Ordering::SeqCst), 0);
    assert_eq!(blocks.counts.success.load(Ordering::SeqCst), 0);
    assert_eq!(blocks.counts.error.load(Ordering::SeqCst), 0);
    assert_eq!(blocks.counts.cancelled.load(Ordering::SeqCst), 0);
    assert_eq!(metadata.attempts.load(Ordering::SeqCst) - attempts, 2);
    assert_eq!(metadata.commits.load(Ordering::SeqCst) - commits, 2);
    let namespace = block_on(metadata.load()).unwrap().namespace.unwrap();
    namespace.validate().unwrap();
    assert!(root_file_layout(&namespace, "empty").extents.is_empty());
    assert!(root_file_layout(&namespace, "zeros").extents.is_empty());
    read_file(&fs, "/empty", b"");
    read_file(&fs, "/zeros", &zeros);
    assert_eq!(blocks.counts.gets.load(Ordering::SeqCst), 0);
    block_on(fs.shutdown()).unwrap();
    for reason in ["initial", "fallback", "retry_rewrite", "chunker_reprepare"] {
        for terminal in ["", ".success", ".error", ".cancelled"] {
            assert_eq!(
                delta(
                    &before,
                    &after,
                    &format!("filesystem.block_put.{reason}{terminal}")
                ),
                (0, 0),
            );
        }
    }
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.committed").1,
        2
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.reply_sent").1,
        2
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn cancelled_acquired_fallback_records_hold_and_block_rewrite_phase() {
    let (metadata, blocks, fs) = open();
    block_on(fs.write_file("/data", b"abcdefgh")).unwrap();
    let handle = block_on(fs.open("/data", "r+", 0)).unwrap();
    let calls = blocks.counts.calls.load(Ordering::SeqCst);
    let successes = blocks.counts.success.load(Ordering::SeqCst);
    let attempts = metadata.attempts.load(Ordering::SeqCst);
    let commits = metadata.commits.load(Ordering::SeqCst);
    blocks
        .counts
        .hold_at_call
        .store(calls + 2, Ordering::SeqCst);
    metadata.forced_conflicts.store(1, Ordering::SeqCst);
    let before = profile::snapshot();
    let mut write = Box::pin(handle.write(b"XY", Some(2)));
    for _ in 0..256 {
        assert!(poll_once(write.as_mut()).is_pending());
        if blocks.counts.calls.load(Ordering::SeqCst) == calls + 2 {
            break;
        }
    }
    assert_eq!(blocks.counts.calls.load(Ordering::SeqCst) - calls, 2);
    assert_eq!(metadata.attempts.load(Ordering::SeqCst) - attempts, 1);
    assert_eq!(metadata.commits.load(Ordering::SeqCst) - commits, 0);
    drop(write);
    let after = profile::snapshot();
    assert_eq!(blocks.counts.success.load(Ordering::SeqCst) - successes, 1);
    assert_eq!(blocks.counts.cancelled.load(Ordering::SeqCst), 1);
    assert!(
        !fs.failed(),
        "canceling block work before publication has a known noncommit"
    );
    let namespace = block_on(metadata.load()).unwrap().namespace.unwrap();
    namespace.validate().unwrap();
    read_file(&fs, "/data", b"abcdefgh");
    block_on(handle.close()).unwrap();
    block_on(fs.shutdown()).unwrap();
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial"),
        (1, 8)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.initial.success"),
        (1, 8)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.fallback"),
        (1, 8)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.fallback.cancelled"),
        (1, 0)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.block_put.fallback.success"),
        (0, 0)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.gate_wait.write_fallback").0,
        1
    );
    assert_eq!(
        delta(
            &before,
            &after,
            "filesystem.gate_wait.write_fallback.cancelled"
        )
        .0,
        0
    );
    assert_eq!(
        delta(&before, &after, "filesystem.gate_hold.write_fallback").0,
        1
    );
    assert_eq!(
        delta(&before, &after, "filesystem.gate_phase.block_rewrite").0,
        1
    );
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn dropped_follower_after_known_commit_retains_commit_and_closed_reply() {
    let (metadata, blocks, fs) = open();
    let attempts = metadata.attempts.load(Ordering::SeqCst);
    let commits = metadata.commits.load(Ordering::SeqCst);
    metadata.hold_after_commit.store(true, Ordering::SeqCst);
    let before = profile::snapshot();
    let mut runner = Box::pin(fs.write_file("/runner", b"left"));
    assert!(poll_once(runner.as_mut()).is_pending());
    let mut follower = Box::pin(fs.write_file("/follower", b"right"));
    assert!(poll_once(follower.as_mut()).is_pending());
    for _ in 0..256 {
        assert!(poll_once(runner.as_mut()).is_pending());
        if metadata.committed_entered.load(Ordering::SeqCst) {
            break;
        }
    }
    assert!(
        metadata.committed_entered.load(Ordering::SeqCst),
        "provider never applied the batch"
    );
    assert_eq!(metadata.attempts.load(Ordering::SeqCst) - attempts, 1);
    assert_eq!(metadata.commits.load(Ordering::SeqCst) - commits, 1);
    let namespace = block_on(metadata.load()).unwrap().namespace.unwrap();
    namespace.validate().unwrap();
    assert_eq!(root_file_layout(&namespace, "runner").extents.len(), 1);
    assert_eq!(root_file_layout(&namespace, "follower").extents.len(), 1);
    drop(follower);
    metadata.committed_resume.notify_one();
    block_on(runner).unwrap();
    let after = profile::snapshot();
    assert_eq!(blocks.counts.calls.load(Ordering::SeqCst), 2);
    assert_eq!(blocks.counts.bytes.load(Ordering::SeqCst), 9);
    assert_eq!(metadata.attempts.load(Ordering::SeqCst) - attempts, 1);
    assert_eq!(metadata.commits.load(Ordering::SeqCst) - commits, 1);
    assert!(
        fs.failed(),
        "known commit with a lost reply must retain existing fail-closed behavior"
    );
    assert_eq!(
        block_on(fs.stat("/follower")).unwrap_err().code,
        ErrorCode::Eio
    );
    block_on(fs.shutdown()).unwrap();
    // An independent coordinator reads the provider's committed namespace and
    // bytes; the failed original coordinator cannot acknowledge the follower.
    let observer = block_on(ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("committed-observer", 16)
            .unwrap()
            .with_concurrent_writes(true),
    ))
    .unwrap();
    read_file(&observer, "/runner", b"left");
    read_file(&observer, "/follower", b"right");
    block_on(observer.shutdown()).unwrap();
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.attempt_requests"),
        (1, 2)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.attempt.success"),
        (1, 2)
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.committed").1,
        2
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.reply_sent").1,
        1
    );
    assert_eq!(
        delta(
            &before,
            &after,
            "filesystem.mutation.request.receiver_closed"
        )
        .1,
        1
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.cancelled").1,
        0
    );
    assert_eq!(
        delta(&before, &after, "filesystem.mutation.request.error").1,
        0
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn sqlite_compact_selected_gates_and_phases_preserve_payload_after_reopen() {
    use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;

    assert!(profile::enabled(), "run with MOUNT_RS_PROFILE_IO=1");
    struct PrivateVolume(PathBuf);
    impl Drop for PrivateVolume {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "mount-rs-causal-compact-{}-{nonce}",
        std::process::id(),
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let volume = PrivateVolume(directory);
    block_on(async {
        let metadata_path = volume.0.join("metadata.db");
        let blocks_path = volume.0.join("blocks.db");
        let fs = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("causal-compact-first", 16)
                .unwrap()
                .with_compact_inode_updates(true),
        )
        .await
        .unwrap();
        assert!(fs.capabilities().durable_writes);
        assert!(
            fs.metadata_store()
                .compact_inode_mode_state()
                .await
                .unwrap()
                .is_some(),
        );
        let before_create = profile::snapshot();
        fs.write_file("/data", b"abcdefgh").await.unwrap();
        let after_create = profile::snapshot();
        let before_open = profile::snapshot();
        let handle = fs.open("/data", "r+", 0).await.unwrap();
        let after_open = profile::snapshot();

        let before_write = profile::snapshot();
        assert_eq!(handle.write(b"XY", Some(2)).await.unwrap(), 2);
        let after_write = profile::snapshot();
        for kind in ["write_prepare", "write_commit"] {
            assert_eq!(
                delta(
                    &before_write,
                    &after_write,
                    &format!("filesystem.gate_wait.{kind}")
                ),
                (1, 0),
            );
            assert_eq!(
                delta(
                    &before_write,
                    &after_write,
                    &format!("filesystem.gate_hold.{kind}")
                ),
                (1, 0),
            );
            assert_eq!(
                delta(
                    &before_write,
                    &after_write,
                    &format!("filesystem.gate_wait.{kind}.cancelled")
                ),
                (0, 0),
            );
        }
        assert_eq!(
            delta(&before_write, &after_write, "filesystem.gate_phase.refresh"),
            (1, 0)
        );
        assert_eq!(
            delta(
                &before_write,
                &after_write,
                "filesystem.gate_phase.publication"
            ),
            (1, 0)
        );
        // Immutable selected-write block work runs between the two gates.
        assert_eq!(
            delta(
                &before_write,
                &after_write,
                "filesystem.gate_phase.block_rewrite"
            ),
            (0, 0)
        );

        let before_stat = profile::snapshot();
        assert_eq!(fs.stat("/data").await.unwrap().size, 8);
        let after_stat = profile::snapshot();
        assert_eq!(
            delta(&before_stat, &after_stat, "filesystem.gate_wait.metadata"),
            (1, 0)
        );
        assert_eq!(
            delta(&before_stat, &after_stat, "filesystem.gate_hold.metadata"),
            (1, 0)
        );
        assert_eq!(
            delta(
                &before_stat,
                &after_stat,
                "filesystem.gate_wait.metadata.cancelled"
            ),
            (0, 0)
        );
        assert_eq!(
            delta(&before_stat, &after_stat, "filesystem.gate_phase.refresh"),
            (1, 0)
        );
        assert_eq!(
            delta(
                &before_stat,
                &after_stat,
                "filesystem.gate_phase.publication"
            ),
            (0, 0)
        );

        let mut bytes = [0_u8; 8];
        let before_read = profile::snapshot();
        assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 8);
        let after_read = profile::snapshot();
        assert_eq!(&bytes, b"abXYefgh");
        // The public read refreshes under a gate on both sides of block I/O.
        assert_eq!(
            delta(&before_read, &after_read, "filesystem.gate_wait.read"),
            (2, 0)
        );
        assert_eq!(
            delta(&before_read, &after_read, "filesystem.gate_hold.read"),
            (2, 0)
        );
        assert_eq!(
            delta(
                &before_read,
                &after_read,
                "filesystem.gate_wait.read.cancelled"
            ),
            (0, 0)
        );
        assert_eq!(
            delta(&before_read, &after_read, "filesystem.gate_phase.refresh"),
            (2, 0)
        );
        assert_eq!(
            delta(
                &before_read,
                &after_read,
                "filesystem.gate_phase.publication"
            ),
            (0, 0)
        );
        let before_eof = profile::snapshot();
        assert_eq!(handle.read(&mut bytes, Some(8)).await.unwrap(), 0);
        let after_eof = profile::snapshot();
        assert_eq!(&bytes, b"abXYefgh");
        handle.close().await.unwrap();
        drop(handle);
        fs.shutdown().await.unwrap();
        drop(fs);

        // Open fresh SQLite connections after every first-owner handle and
        // store has been dropped; this reads persisted metadata and blocks.
        let reopened = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("causal-compact-reopen", 16)
                .unwrap()
                .with_compact_inode_updates(true),
        )
        .await
        .unwrap();
        let reader = reopened.open("/data", "r", 0).await.unwrap();
        let mut persisted = [0_u8; 8];
        assert_eq!(reader.read(&mut persisted, Some(0)).await.unwrap(), 8);
        assert_eq!(&persisted, b"abXYefgh");
        assert_eq!(reader.read(&mut persisted, Some(8)).await.unwrap(), 0);
        reader.close().await.unwrap();
        drop(reader);
        reopened.shutdown().await.unwrap();

        // These rows measure node work caused by the real persisted create.
        // The baseline excludes filesystem initialization and the snapshots
        // exclude the later selected write and reopen readback.
        for name in [
            "filesystem.snapshot_nodes",
            "compact.namespace.materialize_nodes",
            "filesystem.mutation.candidate_clone_nodes",
            "compact.structure.delta_capture_nodes",
            "compact.structure.expected_guard_nodes",
        ] {
            let (calls, nodes) = delta(&before_create, &after_create, name);
            assert!(calls > 0, "{name} must record a create call");
            assert!(nodes > 0, "{name} must record touched nodes");
        }

        // Classify the actual refresh calls only after complete bytes have
        // survived fresh SQLite connections. EOF still performs both handle
        // freshness checks even though it performs no immutable block read.
        let refresh_names = [
            "filesystem.refresh.replace_probe",
            "filesystem.refresh.create_capture",
            "filesystem.refresh.batch_capture",
            "filesystem.refresh.path_structure",
            "filesystem.refresh.read_before",
            "filesystem.refresh.read_after",
        ];
        for (phase, before, after, expected_calls) in [
            ("create", &before_create, &after_create, [1, 1, 1, 0, 0, 0]),
            ("open", &before_open, &after_open, [0, 0, 0, 1, 0, 0]),
            ("write", &before_write, &after_write, [0, 0, 0, 0, 0, 0]),
            ("stat", &before_stat, &after_stat, [0, 0, 0, 1, 0, 0]),
            ("read", &before_read, &after_read, [0, 0, 0, 0, 1, 1]),
            ("eof", &before_eof, &after_eof, [0, 0, 0, 0, 1, 1]),
        ] {
            for (name, calls) in refresh_names.into_iter().zip(expected_calls) {
                assert_eq!(
                    delta(before, after, name),
                    (calls, 0),
                    "{phase} must classify only its actual {name} calls",
                );
            }
        }
        for (before, after) in [(&before_open, &after_open), (&before_stat, &after_stat)] {
            assert_eq!(
                delta(before, after, "filesystem.inode_path_guard"),
                (2, 0),
                "the root and file each require a selected path guard",
            );
        }
        for (before, after) in [(&before_read, &after_read), (&before_eof, &after_eof)] {
            assert_eq!(
                delta(before, after, "filesystem.inode_path_guard"),
                (0, 0),
                "handle reads refresh their selected inode without resolving a path",
            );
        }
    });
}
