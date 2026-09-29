//! Isolated recorder checks: run ignored tests in separate enabled/disabled processes.
use super::*;
use mount_rs_core::diagnostics::storage;
use mount_rs_core::{ErrorCode, FsError};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};

thread_local! {
    static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    static REQUESTED_BYTES: Cell<u64> = const { Cell::new(0) };
}
struct CountingAllocator;
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNT_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
                REQUESTED_BYTES.with(|count| count.set(count.get() + layout.size() as u64));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        COUNT_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
                REQUESTED_BYTES.with(|count| count.set(count.get() + size as u64));
            }
        });
        unsafe { System.realloc(pointer, layout, size) }
    }
}
fn allocations<T>(action: impl FnOnce() -> T) -> (T, u64, u64) {
    ALLOCATIONS.with(|count| count.set(0));
    REQUESTED_BYTES.with(|count| count.set(0));
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
    let result = action();
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
    (
        result,
        ALLOCATIONS.with(Cell::get),
        REQUESTED_BYTES.with(Cell::get),
    )
}
fn row<'a>(snapshot: &'a storage::Snapshot, name: &str) -> &'a storage::Entry {
    snapshot
        .entries
        .iter()
        .find(|row| row.name == name)
        .expect("actual erased SDK call must expose fixed SDK row")
}
struct BlockProbe {
    calls: AtomicU64,
    flush_ready: AtomicBool,
}
impl BlockProbe {
    fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            flush_ready: AtomicBool::new(true),
        }
    }
}
#[async_trait]
impl BlockStore for BlockProbe {
    fn durable(&self) -> bool {
        false
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if bytes == b"fail" {
            return Err(FsError::new(ErrorCode::Eperm).with_syscall("probe put"));
        }
        assert_eq!(bytes, b"sdk-payload");
        Ok(BlockId("probe".into()))
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if id.0 == "fail" {
            return Err(FsError::new(ErrorCode::Eio).with_syscall("probe get"));
        }
        Ok(b"read".to_vec())
    }
    async fn get_for_migration(&self, _: &BlockId) -> Result<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(b"migration".to_vec())
    }
    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        ConcurrentBackingId::from_bytes([1; 16])
    }
    async fn verify_concurrent_backing(&self, id: ConcurrentBackingId) -> Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(id.as_bytes(), [1; 16]);
        Ok(())
    }
    async fn flush(&self) -> Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        std::future::poll_fn(|_| {
            if self.flush_ready.load(Ordering::Acquire) {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        })
        .await
    }
    async fn delete(&self, _: &BlockId) -> Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    async fn reconcile(
        &self,
        live: &std::collections::BTreeSet<BlockId>,
        grace: Duration,
    ) -> Result<BlockReconcileReport> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert!(live.is_empty());
        assert_eq!(grace, Duration::from_secs(7));
        Err(FsError::new(ErrorCode::Enotsup))
    }
}
fn erased_blocks(probe: Arc<dyn BlockStore>) -> ErasedBlockStore {
    #[cfg(feature = "observability")]
    {
        ErasedBlockStore::new(probe, Telemetry::disabled())
    }
    #[cfg(not(feature = "observability"))]
    {
        ErasedBlockStore::new(probe)
    }
}
struct MetadataProbe {
    calls: AtomicU64,
    ready: AtomicBool,
}
#[async_trait]
impl MetadataStore for MetadataProbe {
    fn durable(&self) -> bool {
        false
    }
    fn publish_includes_flush_barrier(&self) -> bool {
        true
    }
    async fn load(&self) -> Result<LoadedMetadata> {
        panic!("conditional None must not fall back to load")
    }
    async fn load_if_changed(&self, revision: u64) -> Result<Option<LoadedMetadata>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(revision, 41);
        Ok(None)
    }
    async fn load_compact_inode(
        &self,
        backing: ConcurrentBackingId,
        inode: InodeId,
    ) -> Result<LoadedCompactInode> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(backing.as_bytes(), [0x7a; 16]);
        assert_eq!(inode, 7);
        Err(FsError::new(ErrorCode::Eio)
            .with_syscall("default compact load probe")
            .with_message("custom provider retained its fresh owned fallback"))
    }
    async fn delegation_state(&self) -> Result<Option<DelegationState>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }
    async fn prepare_compact_inode_mode(
        &self,
        backing: ConcurrentBackingId,
        revision: u64,
    ) -> Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(backing.as_bytes(), [1; 16]);
        assert_eq!(revision, 41);
        std::future::poll_fn(|_| {
            if self.ready.load(Ordering::Acquire) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        Err(FsError::new(ErrorCode::Eperm)
            .with_syscall("prepare probe")
            .with_message("provider denied prepare"))
    }
    async fn flush(&self) -> Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        std::future::pending().await
    }
    async fn acquire_writer(&self, _: &str, _: Duration) -> Result<WriterLease> {
        unreachable!()
    }
    async fn renew_writer(&self, _: &WriterLease, _: Duration) -> Result<WriterLease> {
        unreachable!()
    }
    async fn release_writer(&self, _: &WriterLease) -> Result<()> {
        unreachable!()
    }
    async fn publish(&self, _: u64, _: &WriterLease, _: Namespace) -> Result<u64> {
        unreachable!()
    }
}
fn erased_metadata(probe: Arc<dyn MetadataStore>) -> ErasedMetadataStore {
    #[cfg(feature = "observability")]
    {
        ErasedMetadataStore::new(probe, Telemetry::disabled())
    }
    #[cfg(not(feature = "observability"))]
    {
        ErasedMetadataStore::new(probe)
    }
}

struct CompactReadProbe {
    read_calls: AtomicU64,
    old_load_calls: AtomicU64,
}

fn compact_read_probe_node() -> NodeMetadata {
    use mount_rs_core::chunking::ChunkerConfig;
    use mount_rs_core::storage::{FileLayout, NodeData};
    use mount_rs_core::types::{S_IFREG, Stats};

    NodeMetadata {
        stats: Stats {
            dev: 0,
            ino: 7,
            mode: S_IFREG | 0o600,
            nlink: 1,
            uid: 3,
            gid: 4,
            rdev: 0,
            size: 0,
            blksize: 4096,
            blocks: 0,
            atime_ms: 1,
            mtime_ms: 2,
            ctime_ms: 3,
            birthtime_ms: 4,
        },
        data: NodeData::File(FileLayout {
            chunker: ChunkerConfig {
                algorithm: "fixed-size".into(),
                version: 1,
                parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
            },
            extents: vec![],
        }),
    }
}

#[async_trait]
impl MetadataStore for CompactReadProbe {
    fn durable(&self) -> bool {
        false
    }

    async fn read_compact_inode(
        &self,
        backing: ConcurrentBackingId,
        inode: InodeId,
        expected: mount_rs_core::storage::compact::CompactInodeExpectation<'_>,
    ) -> Result<mount_rs_core::storage::compact::CompactInodeRead> {
        use mount_rs_core::storage::compact::check_compact_inode_unchanged;

        self.read_calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(backing.as_bytes(), [0x7a; 16]);
        assert_eq!(inode, 7);
        let checked = check_compact_inode_unchanged(
            br#"{"layout":"mount-rs-compact-inodes","version":1,"anchor":{"backing":[122,122,122,122,122,122,122,122,122,122,122,122,122,122,122,122],"generation":37,"root":1,"next_inode":8,"default_uid":3,"default_gid":4,"umask":23,"default_chunker":{"algorithm":"fixed-size","version":1,"parameters":{"chunk_size":4096}},"members":[1,7]}}"#,
            backing,
            37,
            inode,
            PhysicalInodeIdentity {
                incarnation: 2,
                epoch: 11,
                revision: 13,
            },
            br#"{"stats":{"dev":0,"ino":7,"mode":33152,"nlink":1,"uid":3,"gid":4,"rdev":0,"size":0,"blksize":4096,"blocks":0,"atime_ms":1,"mtime_ms":2,"ctime_ms":3,"birthtime_ms":4},"data":{"File":{"chunker":{"algorithm":"fixed-size","version":1,"parameters":{"chunk_size":4096}},"extents":[]}}}"#,
            expected,
        )
        .expect("the wrapper must preserve the exact borrowed expectation");
        assert_eq!(checked.generation(), 37);
        assert_eq!(checked.inode(), inode);
        Err(FsError::new(ErrorCode::Eacces)
            .with_syscall("selected compact read probe")
            .with_message("provider certified the exact forwarded expectation"))
    }

    async fn load_compact_inode(
        &self,
        _: ConcurrentBackingId,
        _: InodeId,
    ) -> Result<LoadedCompactInode> {
        self.old_load_calls.fetch_add(1, Ordering::Relaxed);
        Err(FsError::new(ErrorCode::Eio)
            .with_syscall("unexpected old compact load")
            .with_message("SDK bypassed the provider's selected read override"))
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        unreachable!()
    }
    async fn acquire_writer(&self, _: &str, _: Duration) -> Result<WriterLease> {
        unreachable!()
    }
    async fn renew_writer(&self, _: &WriterLease, _: Duration) -> Result<WriterLease> {
        unreachable!()
    }
    async fn release_writer(&self, _: &WriterLease) -> Result<()> {
        unreachable!()
    }
    async fn publish(&self, _: u64, _: &WriterLease, _: Namespace) -> Result<u64> {
        unreachable!()
    }
    async fn flush(&self) -> Result<()> {
        unreachable!()
    }
}

#[tokio::test]
async fn erased_selected_compact_read_forwards_exact_expectation_and_provider_error() {
    use mount_rs_core::storage::compact::CompactInodeExpectation;

    let backing = ConcurrentBackingId::from_bytes([0x7a; 16]).unwrap();
    let identity = PhysicalInodeIdentity {
        incarnation: 2,
        epoch: 11,
        revision: 13,
    };
    let node = compact_read_probe_node();

    // A custom provider that does not implement the new method still performs
    // exactly one fresh owned load through the trait's default implementation.
    let fallback = MetadataProbe {
        calls: AtomicU64::new(0),
        ready: AtomicBool::new(true),
    };
    let fallback_error = fallback
        .read_compact_inode(
            backing,
            7,
            CompactInodeExpectation::selected(37, identity, &node),
        )
        .await
        .unwrap_err();
    assert_eq!(fallback.calls.load(Ordering::Relaxed), 1);
    assert_eq!(fallback_error.code, ErrorCode::Eio);
    assert_eq!(
        fallback_error.syscall.as_deref(),
        Some("default compact load probe")
    );

    let probe = Arc::new(CompactReadProbe {
        read_calls: AtomicU64::new(0),
        old_load_calls: AtomicU64::new(0),
    });
    let direct_error = probe
        .read_compact_inode(
            backing,
            7,
            CompactInodeExpectation::selected(37, identity, &node),
        )
        .await
        .unwrap_err();
    assert_eq!(direct_error.code, ErrorCode::Eacces);
    assert_eq!(probe.read_calls.load(Ordering::Relaxed), 1);
    assert_eq!(probe.old_load_calls.load(Ordering::Relaxed), 0);

    let erased = erased_metadata(probe.clone());
    let forwarded_error = erased
        .read_compact_inode(
            backing,
            7,
            CompactInodeExpectation::selected(37, identity, &node),
        )
        .await
        .unwrap_err();
    assert_eq!(
        (
            probe.read_calls.load(Ordering::Relaxed),
            probe.old_load_calls.load(Ordering::Relaxed)
        ),
        (2, 0),
        "the actual erased wrapper must delegate the new read once, without the old load"
    );
    assert_eq!(forwarded_error.code, ErrorCode::Eacces);
    assert_eq!(
        forwarded_error.syscall.as_deref(),
        Some("selected compact read probe")
    );
    assert_eq!(
        forwarded_error.to_string(),
        "provider certified the exact forwarded expectation"
    );
}

fn allocation_controls(enabled: bool) {
    assert_eq!(storage::enabled(), enabled);
    let inner = Arc::new(BlockProbe::new());
    let erased = erased_blocks(inner.clone());
    let mut cx = Context::from_waker(Waker::noop());
    // Initialize cached flags, fixed bank and optional telemetry before measurement.
    let mut warm = erased.flush();
    assert!(matches!(warm.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
    drop(warm);
    let _ = storage::snapshot();
    let (_, positive, _) = allocations(|| std::hint::black_box(Box::new([0u8; 1024])));
    assert_eq!(positive, 1);
    let (direct_bytes, direct, direct_requested) = allocations(|| {
        let mut call = inner.flush();
        let bytes = std::mem::size_of_val(call.as_ref().get_ref());
        assert!(matches!(call.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
        bytes
    });
    let (sdk_bytes, sdk, sdk_requested) = allocations(|| {
        let mut call = erased.flush();
        let bytes = std::mem::size_of_val(call.as_ref().get_ref());
        assert!(matches!(call.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
        bytes
    });
    assert_eq!(direct, 1, "existing inner async-trait box");
    assert_eq!(sdk, 2, "existing erased and inner async-trait boxes only");
    let (_, updates, _) = allocations(|| {
        for _ in 0..64 {
            let mut span = storage::Span::new(storage::Operation::SdkBlocksFlush);
            span.finish_success(0);
        }
    });
    assert_eq!(updates, 0, "warmed counter updates allocate nothing");
    let (snapshot, snapshot_allocations, snapshot_requested) = allocations(storage::snapshot);
    assert_eq!(snapshot_allocations, 1, "fixed row Vec snapshot allocation");
    let (_, delta_allocations, delta_requested) =
        allocations(|| snapshot.delta(&snapshot).unwrap());
    assert_eq!(delta_allocations, 1, "fixed row Vec delta allocation");
    assert_eq!(
        snapshot.forwarding_boxes.calls, 0,
        "SDK spans add no NAPI forwarding boxes"
    );
    println!(
        "SNAPSHOT_SCOPE enabled={enabled} entries={} entry_bytes={} snapshot_allocations={snapshot_allocations} snapshot_requested={snapshot_requested} delta_allocations={delta_allocations} delta_requested={delta_requested} json_serialization=unmeasured startup=excluded",
        snapshot.entries.len(),
        std::mem::size_of::<storage::Entry>()
    );

    println!(
        "ALLOCATION_SCOPE enabled={enabled} positive={positive} direct_calls={direct} direct_requested={direct_requested} direct_future_bytes={direct_bytes} sdk_calls={sdk} sdk_requested={sdk_requested} sdk_future_bytes={sdk_bytes} span_updates={updates} span_bytes={}",
        std::mem::size_of::<storage::Span<'static>>()
    );
}
#[tokio::test]
#[ignore = "requires isolated MOUNT_RS_PROFILE_IO=1 process"]
async fn sdk_storage_enabled_behavior() {
    assert!(storage::enabled());
    let probe = Arc::new(BlockProbe::new());
    let erased = erased_blocks(probe.clone());
    let before = storage::snapshot();
    assert_eq!(
        erased.put(b"sdk-payload").await.unwrap(),
        BlockId("probe".into())
    );
    assert_eq!(probe.calls.load(Ordering::Relaxed), 1);
    let delta = storage::snapshot().delta(&before).unwrap();
    let put = row(&delta, "sdk.blocks.put");
    assert_eq!((put.calls, put.success, put.bytes), (1, 1, 11));
    assert_eq!(put.latency_log2_us.iter().sum::<u64>(), 1);
    let id = BlockId("probe".into());
    assert!(!erased.durable());
    let backing = erased.prepare_concurrent_backing().await.unwrap();
    erased.verify_concurrent_backing(backing).await.unwrap();
    assert_eq!(erased.get(&id).await.unwrap(), b"read");
    assert_eq!(erased.get_for_migration(&id).await.unwrap(), b"migration");
    let error = erased.put(b"fail").await.unwrap_err();
    assert_eq!(
        (error.code, error.syscall.as_deref()),
        (ErrorCode::Eperm, Some("probe put"))
    );
    assert_eq!(
        erased.get(&BlockId("fail".into())).await.unwrap_err().code,
        ErrorCode::Eio
    );
    erased.delete(&id).await.unwrap();
    assert_eq!(
        erased
            .reconcile(&Default::default(), Duration::from_secs(7))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Enotsup
    );
    let mut cx = Context::from_waker(Waker::noop());
    probe.flush_ready.store(false, Ordering::Release);
    let count = probe.calls.load(Ordering::Relaxed);
    let mut pending = erased.flush();
    assert!(pending.as_mut().poll(&mut cx).is_pending());
    assert!(pending.as_mut().poll(&mut cx).is_pending());
    assert_eq!(probe.calls.load(Ordering::Relaxed), count + 1);
    let held = storage::snapshot();
    assert_eq!(row(&held, "sdk.blocks.flush").in_flight, 1);
    assert_eq!(held.in_flight, 1);
    drop(pending);
    let metadata = Arc::new(MetadataProbe {
        calls: AtomicU64::new(0),
        ready: AtomicBool::new(false),
    });
    let erased_meta = erased_metadata(metadata.clone());
    assert!(!erased_meta.durable());
    assert!(erased_meta.publish_includes_flush_barrier());
    assert!(erased_meta.load_if_changed(41).await.unwrap().is_none());
    assert!(erased_meta.delegation_state().await.unwrap().is_none());
    let mut held = erased_meta.prepare_compact_inode_mode(backing, 41);
    assert!(held.as_mut().poll(&mut cx).is_pending());
    assert!(held.as_mut().poll(&mut cx).is_pending());
    assert_eq!(metadata.calls.load(Ordering::Relaxed), 3);
    assert_eq!(
        row(
            &storage::snapshot(),
            "sdk.metadata.prepare_compact_inode_mode"
        )
        .in_flight,
        1
    );
    metadata.ready.store(true, Ordering::Release);
    let Poll::Ready(Err(error)) = held.as_mut().poll(&mut cx) else {
        panic!("held error must be preserved")
    };
    assert_eq!(error.code, ErrorCode::Eperm);
    assert_eq!(error.syscall.as_deref(), Some("prepare probe"));
    assert_eq!(error.to_string(), "provider denied prepare");
    drop(held);
    let mut cancelled = erased_meta.flush();
    assert!(cancelled.as_mut().poll(&mut cx).is_pending());
    drop(cancelled);
    let count = metadata.calls.load(Ordering::Relaxed);
    drop(erased_meta.flush());
    assert_eq!(metadata.calls.load(Ordering::Relaxed), count);
    let delta = storage::snapshot().delta(&before).unwrap();
    assert_eq!(delta.in_flight, 0);
    for entry in &delta.entries {
        assert_eq!(entry.in_flight, 0);
        assert_eq!(entry.calls, entry.success + entry.error + entry.cancelled);
        assert_eq!(entry.latency_log2_us.iter().sum::<u64>(), entry.calls);
    }
    assert_eq!(
        (
            row(&delta, "sdk.blocks.put").bytes,
            row(&delta, "sdk.blocks.put").error
        ),
        (11, 1)
    );
    assert_eq!(
        (
            row(&delta, "sdk.blocks.get").bytes,
            row(&delta, "sdk.blocks.get").error
        ),
        (4, 1)
    );
    assert_eq!(row(&delta, "sdk.blocks.get_for_migration").bytes, 9);
    assert_eq!(row(&delta, "sdk.blocks.flush").cancelled, 1);
    assert_eq!(row(&delta, "sdk.metadata.flush").cancelled, 1);
    assert_eq!(row(&delta, "sdk.metadata.load_if_changed").success, 1);
    assert_eq!(row(&delta, "sdk.metadata.load").calls, 0);
    for entry in delta
        .entries
        .iter()
        .filter(|entry| entry.name.starts_with("sdk.metadata."))
    {
        assert_eq!(entry.bytes, 0);
        assert_eq!(entry.returned_row_observations, 0);
    }
    for entry in delta
        .entries
        .iter()
        .filter(|entry| !entry.name.starts_with("sdk."))
    {
        assert_eq!(entry.calls, 0, "nested seams must not be fabricated");
    }
    println!(
        "SDK_BEHAVIOR put_bytes=11 get_bytes=4 migration_bytes=9 repeated_pending_one_call=true block_cancelled=1 metadata_cancelled=1 metadata_bytes=unavailable quiescent=true"
    );
    allocation_controls(true);
}
#[test]
#[ignore = "requires isolated MOUNT_RS_PROFILE_IO=0 process"]
fn sdk_storage_disabled_allocation_control() {
    allocation_controls(false);
    let snapshot = storage::snapshot();
    assert_eq!(snapshot.in_flight, 0);
    assert!(
        snapshot
            .entries
            .iter()
            .all(|row| row.calls == 0 && row.in_flight == 0)
    );
    assert_eq!(snapshot.forwarding_boxes.calls, 0);
}
