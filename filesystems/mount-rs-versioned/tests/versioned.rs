use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::driver::FsDriver;
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::versioning::{
    BlockStoreId, PublicationId, ReadLease, ReadLeaseRequest, VersionHead, VersionId, VersionInfo,
    VersionKind, VersionPublication, VersionedMetadataStore,
};
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_memory::{ManualClock, MemoryBlockStore, MemoryMetadataStore};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use mount_rs_versioned::{VersionedCoordinator, VersionedOptions};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

async fn write_file<D>(driver: &D, path: &str, bytes: &[u8])
where
    D: FsDriver,
{
    let handle = driver.open(path, "w", 0o644).await.unwrap();
    let written = handle.write(bytes, Some(0)).await.unwrap();
    assert_eq!(written, bytes.len());
    handle.close().await.unwrap();
    driver.syncfs().await.unwrap();
}

async fn read_file<D>(driver: &D, path: &str) -> Vec<u8>
where
    D: FsDriver,
{
    let handle = driver.open(path, "r", 0).await.unwrap();
    let mut result = Vec::new();
    let mut position = 0_u64;
    loop {
        let mut buffer = vec![0_u8; 4096];
        let count = handle.read(&mut buffer, Some(position)).await.unwrap();
        if count == 0 {
            break;
        }
        result.extend_from_slice(&buffer[..count]);
        position += u64::try_from(count).unwrap();
    }
    handle.close().await.unwrap();
    result
}

async fn seed_current<M, B>(metadata: M, blocks: B, owner: &str) -> Namespace
where
    M: MetadataStore + Clone + 'static,
    B: BlockStore + Clone + 'static,
{
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks,
        ChunkedOptions::fixed(owner, 4096).unwrap(),
    )
    .await
    .unwrap();
    write_file(&filesystem, "/hello", b"old bytes").await;
    filesystem.shutdown().await.unwrap();
    metadata.load().await.unwrap().namespace.unwrap()
}

fn options(owner: &str, block_store_id: &str) -> VersionedOptions {
    VersionedOptions::new(
        owner,
        BlockStoreId::new(block_store_id).expect("test block store id"),
    )
    .with_view_ttl(Duration::from_secs(60))
}

#[derive(Clone)]
struct SlowBlockStore {
    inner: MemoryBlockStore,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Clone)]
struct FailingPutBlockStore {
    inner: MemoryBlockStore,
}

#[derive(Clone)]
struct FailingFlushBlockStore {
    inner: MemoryBlockStore,
}

#[async_trait]
impl BlockStore for FailingFlushBlockStore {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.inner.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        Err(FsError::new(ErrorCode::Eio).with_syscall("flush blocks"))
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.inner.delete(id).await
    }
}

#[async_trait]
impl BlockStore for FailingPutBlockStore {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, _bytes: &[u8]) -> Result<BlockId> {
        Err(FsError::new(ErrorCode::Eio).with_syscall("put block"))
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.inner.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.inner.delete(id).await
    }
}

impl SlowBlockStore {
    fn new() -> Self {
        Self {
            inner: MemoryBlockStore::new(),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
}

#[async_trait]
impl BlockStore for SlowBlockStore {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.entered.notify_one();
        self.release.notified().await;
        self.inner.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.inner.delete(id).await
    }
}

#[derive(Clone)]
struct BlockingRenewMetadata {
    inner: MemoryMetadataStore,
    block_renew: Arc<AtomicBool>,
    block_after_renew: Arc<AtomicBool>,
    renew_calls: Arc<AtomicUsize>,
    block_call: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    after_renew_entered: Arc<Notify>,
    after_renew_release: Arc<Notify>,
    block_close: Arc<AtomicBool>,
    close_entered: Arc<Notify>,
    close_release: Arc<Notify>,
    fail_close: Arc<AtomicBool>,
    fail_after_publish: Arc<AtomicBool>,
    miss_publication_once: Arc<AtomicBool>,
}

impl BlockingRenewMetadata {
    fn new(inner: MemoryMetadataStore) -> Self {
        Self {
            inner,
            block_renew: Arc::new(AtomicBool::new(false)),
            block_after_renew: Arc::new(AtomicBool::new(false)),
            renew_calls: Arc::new(AtomicUsize::new(0)),
            block_call: Arc::new(AtomicUsize::new(usize::MAX)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            after_renew_entered: Arc::new(Notify::new()),
            after_renew_release: Arc::new(Notify::new()),
            block_close: Arc::new(AtomicBool::new(false)),
            close_entered: Arc::new(Notify::new()),
            close_release: Arc::new(Notify::new()),
            fail_close: Arc::new(AtomicBool::new(false)),
            fail_after_publish: Arc::new(AtomicBool::new(false)),
            miss_publication_once: Arc::new(AtomicBool::new(false)),
        }
    }

    fn block_renewals(&self) {
        self.block_renew.store(true, Ordering::Release);
    }

    fn block_next_after_renew(&self) {
        self.block_after_renew.store(true, Ordering::Release);
    }

    fn block_next_finish(&self) {
        let next_finish = self.renew_calls.load(Ordering::Acquire).saturating_add(2);
        self.block_call.store(next_finish, Ordering::Release);
    }

    fn block_next_begin(&self) {
        let next_begin = self.renew_calls.load(Ordering::Acquire).saturating_add(1);
        self.block_call.store(next_begin, Ordering::Release);
    }

    fn fail_next_close(&self) {
        self.fail_close.store(true, Ordering::Release);
    }

    fn block_next_close(&self) {
        self.block_close.store(true, Ordering::Release);
    }

    fn miss_next_publication_lookup(&self) {
        self.miss_publication_once.store(true, Ordering::Release);
    }

    fn fail_next_publish_after_commit(&self) {
        self.fail_after_publish.store(true, Ordering::Release);
    }
}

#[async_trait]
impl MetadataStore for BlockingRenewMetadata {
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
        self.inner
            .publish(expected_revision, lease, namespace)
            .await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }
}

#[async_trait]
impl VersionedMetadataStore for BlockingRenewMetadata {
    fn volume_id(&self) -> mount_rs_core::versioning::VolumeId {
        self.inner.volume_id()
    }

    async fn version_head(&self) -> Result<Option<VersionHead>> {
        self.inner.version_head().await
    }

    async fn load_version(&self, id: &VersionId) -> Result<VersionInfo> {
        self.inner.load_version(id).await
    }

    async fn list_versions(&self) -> Result<Vec<VersionInfo>> {
        self.inner.list_versions().await
    }

    async fn find_publication(&self, operation_id: &PublicationId) -> Result<Option<VersionInfo>> {
        if self.miss_publication_once.swap(false, Ordering::AcqRel) {
            return Ok(None);
        }
        self.inner.find_publication(operation_id).await
    }

    async fn publish_version(
        &self,
        lease: &WriterLease,
        publication: VersionPublication,
    ) -> Result<VersionInfo> {
        let result = self.inner.publish_version(lease, publication).await;
        if self.fail_after_publish.swap(false, Ordering::AcqRel) && result.is_ok() {
            return Err(FsError::new(ErrorCode::Eio).with_syscall("publish version"));
        }
        result
    }

    async fn open_view_pin(&self, id: &VersionId, request: ReadLeaseRequest) -> Result<ReadLease> {
        self.inner.open_view_pin(id, request).await
    }

    async fn renew_view_pin(
        &self,
        lease: &ReadLease,
        request: ReadLeaseRequest,
    ) -> Result<ReadLease> {
        let call = self.renew_calls.fetch_add(1, Ordering::AcqRel) + 1;
        if self.block_renew.load(Ordering::Acquire)
            || self.block_call.load(Ordering::Acquire) == call
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        let result = self.inner.renew_view_pin(lease, request).await;
        if result.is_ok() && self.block_after_renew.swap(false, Ordering::AcqRel) {
            self.after_renew_entered.notify_one();
            self.after_renew_release.notified().await;
        }
        result
    }

    async fn close_view_pin(&self, lease: &ReadLease) -> Result<()> {
        if self.block_close.swap(false, Ordering::AcqRel) {
            self.close_entered.notify_one();
            self.close_release.notified().await;
        }
        if self.fail_close.swap(false, Ordering::AcqRel) {
            return Err(FsError::new(ErrorCode::Eio).with_syscall("close view"));
        }
        self.inner.close_view_pin(lease).await
    }

    async fn delete_version(&self, lease: &WriterLease, id: &VersionId) -> Result<()> {
        self.inner.delete_version(lease, id).await
    }
}

#[tokio::test]
async fn memory_history_reads_immutable_bytes_restore_fork_and_pin_retention() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    seed_current(metadata.clone(), blocks.clone(), "writer-one").await;

    let coordinator =
        VersionedCoordinator::new(metadata.clone(), blocks.clone(), options("versions", "mem"))
            .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    assert_eq!(first.id.sequence, 1);
    assert!(!first.durable);
    let view = coordinator.view(&first.id).await.unwrap();
    assert_eq!(read_file(&view, "/hello").await, b"old bytes");
    let readonly = view.open("/hello", "w", 0).await;
    assert!(matches!(readonly, Err(error) if error.is(ErrorCode::Erofs)));

    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("writer-two", 4096).unwrap(),
    )
    .await
    .unwrap();
    write_file(&filesystem, "/hello", b"new bytes").await;
    filesystem.shutdown().await.unwrap();
    let second = coordinator.snapshot().await.unwrap();
    assert_eq!(second.parent, Some(first.id.clone()));
    assert_eq!(read_file(&view, "/hello").await, b"old bytes");

    let restored = coordinator.restore(&first.id).await.unwrap();
    assert_eq!(restored.parent, Some(second.id.clone()));
    assert_eq!(restored.restored_from, Some(first.id.clone()));

    let target_metadata = MemoryMetadataStore::new();
    let target_blocks = MemoryBlockStore::new();
    let target = VersionedCoordinator::new(
        target_metadata.clone(),
        target_blocks,
        options("fork-target", "mem"),
    )
    .unwrap();
    let forked = coordinator.fork_into(&first.id, &target).await.unwrap();
    assert_eq!(forked.kind, VersionKind::Fork);
    assert_eq!(forked.forked_from, Some(first.id.clone()));
    let fork_view = target.view(&forked.id).await.unwrap();
    assert_eq!(read_file(&fork_view, "/hello").await, b"old bytes");

    assert_eq!(
        coordinator.delete(&first.id).await.unwrap_err().code,
        ErrorCode::Ebusy
    );
    view.close().await.unwrap();
    fork_view.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn explicit_publication_ids_reconcile_after_head_moves() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    let namespace = seed_current(metadata.clone(), blocks.clone(), "explicit-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("explicit", "explicit-blocks"),
    )
    .unwrap();
    let initial = metadata.load().await.unwrap();
    let operation_id = PublicationId::new("explicit-publication").unwrap();
    let first = coordinator
        .publish_namespace_with_publication_id(
            initial.revision,
            None,
            namespace.clone(),
            operation_id.clone(),
        )
        .await
        .unwrap();
    let _later = coordinator.snapshot().await.unwrap();

    let replay = coordinator
        .publish_namespace_with_publication_id(
            initial.revision,
            None,
            namespace.clone(),
            operation_id.clone(),
        )
        .await
        .unwrap();
    assert_eq!(replay.id, first.id);
    let publication = VersionPublication {
        expected_revision: initial.revision,
        expected_parent: None,
        operation_id: operation_id.clone(),
        namespace: namespace.clone(),
        block_store_id: BlockStoreId::new("explicit-blocks").unwrap(),
        kind: VersionKind::NamespacePublication,
        restored_from: None,
        forked_from: None,
        durable: false,
    };
    assert_eq!(
        coordinator
            .reconcile_publication(&publication)
            .await
            .unwrap()
            .unwrap()
            .id,
        first.id
    );

    let mut mismatched = namespace.clone();
    mismatched.default_uid += 1;
    assert_eq!(
        coordinator
            .publish_namespace_with_publication_id(
                initial.revision,
                None,
                mismatched,
                operation_id,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Eexist
    );

    let restore_id = PublicationId::new("explicit-restore").unwrap();
    let restored = coordinator
        .restore_with_publication_id(&first.id, restore_id.clone())
        .await
        .unwrap();
    assert_eq!(restored.restored_from, Some(first.id.clone()));
    let restored_replay = coordinator
        .restore_with_publication_id(&first.id, restore_id)
        .await
        .unwrap();
    assert_eq!(restored_replay.id, restored.id);
}

#[tokio::test]
async fn snapshot_retry_rejects_a_different_block_store_before_writer_acquisition() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    seed_current(metadata.clone(), blocks.clone(), "snapshot-retry-writer").await;
    let original = VersionedCoordinator::new(
        metadata.clone(),
        blocks.clone(),
        options("snapshot-retry-original", "snapshot-retry-blocks-a"),
    )
    .unwrap();
    let operation_id = PublicationId::new("snapshot-retry-id").unwrap();
    let first = original
        .snapshot_with_publication_id(operation_id.clone())
        .await
        .unwrap();
    let _later = original.snapshot().await.unwrap();

    let same_store = VersionedCoordinator::new(
        metadata.clone(),
        blocks.clone(),
        options("snapshot-retry-same-store", "snapshot-retry-blocks-a"),
    )
    .unwrap();
    assert_eq!(
        same_store
            .snapshot_with_publication_id(operation_id.clone())
            .await
            .unwrap()
            .id,
        first.id
    );

    let different_store = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("snapshot-retry-different-store", "snapshot-retry-blocks-b"),
    )
    .unwrap();
    // A held writer makes an accidental lease acquisition return EAGAIN.
    // EEXIST proves the mismatched retry is rejected by the first lookup.
    let held_lease = metadata
        .acquire_writer("snapshot-retry-held-writer", Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(
        different_store
            .snapshot_with_publication_id(operation_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Eexist
    );
    metadata.release_writer(&held_lease).await.unwrap();
}

#[tokio::test]
async fn snapshot_retry_rejects_an_explicit_namespace_publication_with_the_same_id() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    let old_namespace =
        seed_current(metadata.clone(), blocks.clone(), "snapshot-collision-seed").await;
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("snapshot-collision-update", 4096).unwrap(),
    )
    .await
    .unwrap();
    write_file(&filesystem, "/hello", b"new bytes").await;
    filesystem.shutdown().await.unwrap();
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks.clone(),
        options("snapshot-collision", "collision-blocks"),
    )
    .unwrap();
    let operation_id = PublicationId::new("same-id-different-cut").unwrap();
    let revision = metadata.load().await.unwrap().revision;
    coordinator
        .publish_namespace_with_publication_id(revision, None, old_namespace, operation_id.clone())
        .await
        .unwrap();
    // Move the live cut away from the caller-supplied publication before
    // retrying the ID as an ordinary snapshot.
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks,
        ChunkedOptions::fixed("snapshot-collision-later", 4096).unwrap(),
    )
    .await
    .unwrap();
    write_file(&filesystem, "/hello", b"later bytes").await;
    filesystem.shutdown().await.unwrap();

    assert_eq!(
        coordinator
            .snapshot_with_publication_id(operation_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Eexist
    );
}

#[tokio::test]
async fn raw_publication_cannot_impersonate_a_live_snapshot() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    let namespace = seed_current(metadata.clone(), blocks.clone(), "raw-snapshot-seed").await;
    let revision = metadata.load().await.unwrap().revision;
    let coordinator = VersionedCoordinator::new(
        metadata,
        blocks,
        options("raw-snapshot", "raw-snapshot-blocks"),
    )
    .unwrap();
    let publication = VersionPublication {
        expected_revision: revision,
        expected_parent: None,
        operation_id: PublicationId::new("raw-snapshot-id").unwrap(),
        namespace,
        block_store_id: BlockStoreId::new("raw-snapshot-blocks").unwrap(),
        kind: VersionKind::Snapshot,
        restored_from: None,
        forked_from: None,
        durable: false,
    };
    assert_eq!(
        coordinator
            .publish_publication(publication)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Einval
    );
    assert!(coordinator.history().await.unwrap().is_empty());
}

#[tokio::test]
async fn sqlite_namespace_publication_kind_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let metadata_path = directory.path().join("metadata.db");
    let blocks_path = directory.path().join("blocks.db");
    let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
    let blocks = SqliteBlockStore::open(&blocks_path).unwrap();
    let namespace = seed_current(metadata.clone(), blocks.clone(), "sqlite-kind-seed").await;
    let revision = metadata.load().await.unwrap().revision;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("sqlite-kind", "sqlite-kind-blocks"),
    )
    .unwrap();
    let operation_id = PublicationId::new("sqlite-namespace-kind").unwrap();
    let first = coordinator
        .publish_namespace_with_publication_id(revision, None, namespace, operation_id.clone())
        .await
        .unwrap();
    assert_eq!(first.kind, VersionKind::NamespacePublication);
    drop(coordinator);
    drop(metadata);

    let reopened = SqliteMetadataStore::open(&metadata_path).unwrap();
    assert_eq!(
        reopened.load_version(&first.id).await.unwrap().kind,
        VersionKind::NamespacePublication
    );
    assert_eq!(
        reopened
            .find_publication(&operation_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
}

#[tokio::test]
async fn sqlite_legacy_snapshot_kind_replays_only_the_identical_explicit_payload() {
    let directory = tempfile::tempdir().unwrap();
    let metadata_path = directory.path().join("legacy-metadata.db");
    let blocks_path = directory.path().join("legacy-blocks.db");
    let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
    let blocks = SqliteBlockStore::open(&blocks_path).unwrap();
    let namespace = seed_current(metadata.clone(), blocks.clone(), "legacy-kind-seed").await;
    let revision = metadata.load().await.unwrap().revision;
    let operation_id = PublicationId::new("legacy-explicit-publication").unwrap();
    let lease = metadata
        .acquire_writer("legacy-kind-writer", Duration::from_secs(60))
        .await
        .unwrap();
    // Older publish_namespace_with_publication_id wrote this exact payload
    // using VersionKind::Snapshot. Persist it through the provider directly.
    let legacy = metadata
        .publish_version(
            &lease,
            VersionPublication {
                expected_revision: revision,
                expected_parent: None,
                operation_id: operation_id.clone(),
                namespace: namespace.clone(),
                block_store_id: BlockStoreId::new("legacy-kind-blocks").unwrap(),
                kind: VersionKind::Snapshot,
                restored_from: None,
                forked_from: None,
                durable: true,
            },
        )
        .await
        .unwrap();
    metadata.release_writer(&lease).await.unwrap();
    drop(metadata);

    let reopened = SqliteMetadataStore::open(&metadata_path).unwrap();
    let coordinator = VersionedCoordinator::new(
        reopened,
        blocks,
        options("legacy-kind-replay", "legacy-kind-blocks"),
    )
    .unwrap();
    let exact_retry = VersionPublication {
        expected_revision: revision,
        expected_parent: None,
        operation_id: operation_id.clone(),
        namespace: namespace.clone(),
        block_store_id: BlockStoreId::new("legacy-kind-blocks").unwrap(),
        kind: VersionKind::NamespacePublication,
        restored_from: None,
        forked_from: None,
        durable: true,
    };
    assert_eq!(
        coordinator
            .reconcile_publication(&exact_retry)
            .await
            .unwrap()
            .unwrap()
            .id,
        legacy.id
    );
    assert_eq!(
        coordinator
            .publish_namespace_with_publication_id(
                revision,
                None,
                namespace.clone(),
                operation_id.clone(),
            )
            .await
            .unwrap()
            .id,
        legacy.id
    );
    let mut changed = namespace;
    changed.nodes.get_mut(&2).unwrap().stats.mtime_ms += 1;
    assert_eq!(
        coordinator
            .publish_namespace_with_publication_id(revision, None, changed, operation_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Eexist
    );
}

#[tokio::test]
async fn snapshot_retry_rejects_a_different_block_store_after_writer_acquisition() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner, blocks.clone(), "snapshot-retry-lease-writer").await;
    let original = VersionedCoordinator::new(
        metadata.clone(),
        blocks.clone(),
        options(
            "snapshot-retry-lease-original",
            "snapshot-retry-lease-blocks-a",
        ),
    )
    .unwrap();
    let operation_id = PublicationId::new("snapshot-retry-lease-id").unwrap();
    original
        .snapshot_with_publication_id(operation_id.clone())
        .await
        .unwrap();

    let different_store = VersionedCoordinator::new(
        metadata.clone(),
        FailingFlushBlockStore { inner: blocks },
        options(
            "snapshot-retry-lease-different",
            "snapshot-retry-lease-blocks-b",
        ),
    )
    .unwrap();
    // Force the first lookup to miss so the second lookup runs under the writer lease.
    metadata.miss_next_publication_lookup();
    assert_eq!(
        different_store
            .snapshot_with_publication_id(operation_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Eexist
    );
    assert_eq!(original.history().await.unwrap().len(), 1);
}

#[tokio::test]
async fn fork_renews_source_pin_and_releases_it_when_copy_fails() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let source_blocks = MemoryBlockStore::new();
    seed_current(inner.clone(), source_blocks.clone(), "fork-failure-writer").await;
    let source = VersionedCoordinator::new(
        metadata.clone(),
        source_blocks,
        options("fork-failure", "fork-failure-source"),
    )
    .unwrap();
    let first = source.snapshot().await.unwrap();
    let _current = source.snapshot().await.unwrap();

    let target = VersionedCoordinator::new(
        MemoryMetadataStore::new(),
        FailingPutBlockStore {
            inner: MemoryBlockStore::new(),
        },
        options("fork-failure-target", "fork-failure-target"),
    )
    .unwrap();
    assert_eq!(
        source.fork_into(&first.id, &target).await.unwrap_err().code,
        ErrorCode::Eio
    );
    assert!(metadata.renew_calls.load(Ordering::Acquire) > 0);
    // The copy failed before target publication; source cleanup must still
    // remove the source view pin rather than waiting for its TTL.
    source.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn fork_publication_id_reconciles_lost_ack_with_fresh_coordinator() {
    let source_metadata = MemoryMetadataStore::new();
    let source_blocks = MemoryBlockStore::new();
    seed_current(
        source_metadata.clone(),
        source_blocks.clone(),
        "fork-retry-source-writer",
    )
    .await;
    let source = VersionedCoordinator::new(
        source_metadata,
        source_blocks,
        options("fork-retry-source", "fork-retry-source-blocks"),
    )
    .unwrap();
    let first = source.snapshot().await.unwrap();
    let later = source.snapshot().await.unwrap();

    let target_metadata = BlockingRenewMetadata::new(MemoryMetadataStore::new());
    let target_blocks = MemoryBlockStore::new();
    let target = VersionedCoordinator::new(
        target_metadata.clone(),
        target_blocks.clone(),
        options("fork-retry-target", "fork-retry-target-blocks"),
    )
    .unwrap();
    let operation_id = PublicationId::new("fork-retry-publication").unwrap();
    target_metadata.fail_next_publish_after_commit();

    let lost_ack = source
        .fork_into_with_publication_id(&first.id, &target, operation_id.clone())
        .await
        .unwrap_err();
    assert_eq!(lost_ack.code, ErrorCode::Eio);
    let committed = target.history().await.unwrap();
    assert_eq!(committed.len(), 1);

    let fresh_target = VersionedCoordinator::new(
        target_metadata.clone(),
        target_blocks.clone(),
        options("fork-retry-fresh-target", "fork-retry-target-blocks"),
    )
    .unwrap();
    let replay = source
        .fork_into_with_publication_id(&first.id, &fresh_target, operation_id.clone())
        .await
        .unwrap();
    assert_eq!(replay.id, committed[0].id);
    assert_eq!(replay.kind, VersionKind::Fork);
    assert_eq!(replay.forked_from, Some(first.id.clone()));
    assert_eq!(
        replay.block_store_id,
        BlockStoreId::new("fork-retry-target-blocks").unwrap()
    );
    assert_eq!(fresh_target.history().await.unwrap().len(), 1);

    let source_mismatch = source
        .fork_into_with_publication_id(&later.id, &fresh_target, operation_id.clone())
        .await
        .unwrap_err();
    assert_eq!(source_mismatch.code, ErrorCode::Eexist);

    let wrong_target = VersionedCoordinator::new(
        target_metadata,
        target_blocks,
        options("fork-retry-wrong-target", "different-target-blocks"),
    )
    .unwrap();
    let target_mismatch = source
        .fork_into_with_publication_id(&first.id, &wrong_target, operation_id)
        .await
        .unwrap_err();
    assert_eq!(target_mismatch.code, ErrorCode::Eexist);
}

#[tokio::test]
async fn view_rejects_block_store_mismatch_and_releases_pin() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    seed_current(metadata.clone(), blocks.clone(), "mismatch-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks.clone(),
        options("mismatch", "store-a"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();

    let wrong_store =
        VersionedCoordinator::new(metadata, blocks, options("wrong-store", "store-b")).unwrap();
    let error = match wrong_store.view(&first.id).await {
        Ok(_) => panic!("a view must reject a mismatched block store"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::Exdev);

    // The failed open acquired a provider pin before discovering the manifest
    // identity. Successful cleanup is required for retention to proceed.
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn closing_view_blocks_new_ops_but_open_handle_retains_pin() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    seed_current(metadata.clone(), blocks.clone(), "lifecycle-writer").await;
    let coordinator =
        VersionedCoordinator::new(metadata, blocks, options("lifecycle", "lifecycle-blocks"))
            .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = coordinator.view(&first.id).await.unwrap();
    let handle = view.open("/hello", "r", 0).await.unwrap();

    view.close().await.unwrap();
    assert!(view.stat("/hello").await.unwrap_err().is(ErrorCode::Ebadf));
    let open_after_close = view.open("/hello", "r", 0).await;
    assert!(matches!(
        open_after_close,
        Err(error) if error.is(ErrorCode::Ebadf)
    ));

    let mut bytes = [0_u8; 9];
    assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), bytes.len());
    assert_eq!(&bytes, b"old bytes");
    assert_eq!(
        coordinator.delete(&first.id).await.unwrap_err().code,
        ErrorCode::Ebusy
    );

    handle.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn concurrent_view_close_and_open_never_leaks_a_pin() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    seed_current(metadata.clone(), blocks.clone(), "race-writer").await;
    let coordinator =
        VersionedCoordinator::new(metadata, blocks, options("race", "race-blocks")).unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = Arc::new(coordinator.view(&first.id).await.unwrap());

    let opener = {
        let view = Arc::clone(&view);
        tokio::spawn(async move { view.open("/hello", "r", 0).await })
    };
    let closer = {
        let view = Arc::clone(&view);
        tokio::spawn(async move { view.close().await })
    };
    let opened = opener.await.unwrap();
    closer.await.unwrap().unwrap();
    if let Ok(handle) = opened {
        handle.close().await.unwrap();
    }
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn explicit_view_close_retries_a_failed_pin_release() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner.clone(), blocks.clone(), "close-retry-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("close-retry", "close-retry-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = coordinator.view(&first.id).await.unwrap();

    metadata.fail_next_close();
    assert_eq!(view.close().await.unwrap_err().code, ErrorCode::Eio);
    view.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn cancelled_view_close_retains_the_exact_token_for_retry() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner, blocks.clone(), "cancel-close-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("cancel-close", "cancel-close-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = Arc::new(coordinator.view(&first.id).await.unwrap());

    metadata.block_next_close();
    let close_task = tokio::spawn({
        let view = Arc::clone(&view);
        async move { view.close().await }
    });
    metadata.close_entered.notified().await;
    close_task.abort();
    assert!(close_task.await.unwrap_err().is_cancelled());
    assert_eq!(
        coordinator.delete(&first.id).await.unwrap_err().code,
        ErrorCode::Ebusy
    );

    view.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn explicit_handle_close_retries_a_failed_pin_release() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner.clone(), blocks.clone(), "handle-close-retry-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("handle-close-retry", "handle-close-retry-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = coordinator.view(&first.id).await.unwrap();
    let handle = view.open("/hello", "r", 0).await.unwrap();
    view.close().await.unwrap();

    metadata.fail_next_close();
    assert_eq!(handle.close().await.unwrap_err().code, ErrorCode::Eio);
    handle.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn cancelled_final_handle_close_retains_the_pin_for_retry() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner, blocks.clone(), "cancel-handle-close-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("cancel-handle-close", "cancel-handle-close-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = coordinator.view(&first.id).await.unwrap();
    let handle = view.open("/hello", "r", 0).await.unwrap();
    view.close().await.unwrap();

    metadata.block_next_close();
    let close_task = tokio::spawn({
        let handle = handle.clone();
        async move { handle.close().await }
    });
    metadata.close_entered.notified().await;
    close_task.abort();
    assert!(close_task.await.unwrap_err().is_cancelled());
    assert_eq!(
        coordinator.delete(&first.id).await.unwrap_err().code,
        ErrorCode::Ebusy
    );

    handle.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn expired_view_invalidates_handles_before_historical_read() {
    let clock = Arc::new(ManualClock::new(1_000));
    let metadata = MemoryMetadataStore::with_clock(clock.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(metadata.clone(), blocks.clone(), "expiry-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata,
        blocks,
        options("expiry", "expiry-blocks").with_view_ttl(Duration::from_millis(10)),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = coordinator.view(&first.id).await.unwrap();
    let handle = view.open("/hello", "r", 0).await.unwrap();

    assert!(clock.advance_ms(10));
    let mut bytes = [0_u8; 9];
    assert_eq!(
        handle.read(&mut bytes, Some(0)).await.unwrap_err().code,
        ErrorCode::Estale
    );
    assert_eq!(
        view.stat("/hello").await.unwrap_err().code,
        ErrorCode::Estale
    );

    // The expired lease has invalidated the open handle, so retention may
    // reclaim the historical version; no historical I/O can continue.
    coordinator.delete(&first.id).await.unwrap();
    handle.close().await.unwrap();
    view.close().await.unwrap();
}

#[tokio::test]
async fn slow_read_and_view_close_keep_retention_pinned_until_handle_close() {
    let metadata = MemoryMetadataStore::new();
    let blocks = SlowBlockStore::new();
    seed_current(metadata.clone(), blocks.clone(), "slow-read-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata,
        blocks.clone(),
        options("slow-read", "slow-read-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = coordinator.view(&first.id).await.unwrap();
    let handle = view.open("/hello", "r", 0).await.unwrap();
    let read_handle = handle.clone();
    let read_task = tokio::spawn(async move {
        let mut bytes = [0_u8; 9];
        let count = read_handle.read(&mut bytes, Some(0)).await?;
        Ok::<_, mount_rs_core::FsError>((count, bytes))
    });

    blocks.entered.notified().await;
    view.close().await.unwrap();
    assert!(view.stat("/hello").await.unwrap_err().is(ErrorCode::Ebadf));
    assert_eq!(
        coordinator.delete(&first.id).await.unwrap_err().code,
        ErrorCode::Ebusy
    );

    blocks.release.notify_one();
    let (count, bytes) = read_task.await.unwrap().unwrap();
    assert_eq!(count, bytes.len());
    assert_eq!(&bytes, b"old bytes");
    assert_eq!(
        coordinator.delete(&first.id).await.unwrap_err().code,
        ErrorCode::Ebusy
    );

    handle.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn slow_read_expiry_and_delete_fail_closed_after_read() {
    let clock = Arc::new(ManualClock::new(1_000));
    let metadata = MemoryMetadataStore::with_clock(clock.clone());
    let blocks = SlowBlockStore::new();
    seed_current(metadata.clone(), blocks.clone(), "slow-expiry-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata,
        blocks.clone(),
        options("slow-expiry", "slow-expiry-blocks").with_view_ttl(Duration::from_millis(10)),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = coordinator.view(&first.id).await.unwrap();
    let handle = view.open("/hello", "r", 0).await.unwrap();
    let read_handle = handle.clone();
    let read_task = tokio::spawn(async move {
        let mut bytes = [0_u8; 9];
        read_handle.read(&mut bytes, Some(0)).await
    });

    blocks.entered.notified().await;
    assert!(clock.advance_ms(10));
    // The provider no longer sees an active lease, so GC is allowed to delete
    // the version while the physical block read is still in flight. The
    // post-read renewal must make that read fail closed rather than return
    // bytes from a version that was deleted during I/O.
    coordinator.delete(&first.id).await.unwrap();

    blocks.release.notify_one();
    let error = read_task.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Estale);
    handle.close().await.unwrap();
    view.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_read_balances_operation_and_allows_pin_cleanup() {
    let metadata = MemoryMetadataStore::new();
    let blocks = SlowBlockStore::new();
    seed_current(metadata.clone(), blocks.clone(), "cancel-read-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata,
        blocks.clone(),
        options("cancel-read", "cancel-read-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = coordinator.view(&first.id).await.unwrap();
    let handle = view.open("/hello", "r", 0).await.unwrap();
    let read_handle = handle.clone();
    let read_task = tokio::spawn(async move {
        let mut bytes = [0_u8; 9];
        read_handle.read(&mut bytes, Some(0)).await
    });

    blocks.entered.notified().await;
    read_task.abort();
    assert!(read_task.await.unwrap_err().is_cancelled());
    view.close().await.unwrap();
    handle.close().await.unwrap();
    // A leaked operation count would keep the pin forever after close/handle
    // release and make this deletion fail with EBUSY.
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn cancelled_begin_renewal_balances_operation_and_closes_pin() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner.clone(), blocks.clone(), "cancel-begin-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("cancel-begin", "cancel-begin-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = Arc::new(coordinator.view(&first.id).await.unwrap());

    metadata.block_renewals();
    let stat_task = tokio::spawn({
        let view = Arc::clone(&view);
        async move { view.stat("/hello").await }
    });
    metadata.entered.notified().await;
    stat_task.abort();
    assert!(stat_task.await.unwrap_err().is_cancelled());
    view.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn committed_renewal_with_lost_reply_does_not_claim_pin_was_closed() {
    let clock = Arc::new(ManualClock::new(1_000));
    let inner = MemoryMetadataStore::with_clock(clock.clone());
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner, blocks.clone(), "ambiguous-renewal-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("ambiguous-renewal", "ambiguous-renewal-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = Arc::new(coordinator.view(&first.id).await.unwrap());

    assert!(clock.advance_ms(1));
    metadata.block_next_after_renew();
    let renew_task = tokio::spawn({
        let view = Arc::clone(&view);
        async move { view.renew(Duration::from_secs(60)).await }
    });
    metadata.after_renew_entered.notified().await;
    renew_task.abort();
    assert!(renew_task.await.unwrap_err().is_cancelled());

    // The provider committed a newer exact token, but the view only knows
    // the old one. ESTALE is not an acknowledgement that the pin is gone.
    assert_eq!(view.close().await.unwrap_err().code, ErrorCode::Estale);
    assert_eq!(
        coordinator.delete(&first.id).await.unwrap_err().code,
        ErrorCode::Ebusy
    );
}

#[tokio::test]
async fn cancelled_with_operation_finish_balances_operation_and_closes_pin() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner.clone(), blocks.clone(), "cancel-finish-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("cancel-finish", "cancel-finish-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = Arc::new(coordinator.view(&first.id).await.unwrap());

    metadata.block_next_finish();
    let stat_task = tokio::spawn({
        let view = Arc::clone(&view);
        async move { view.stat("/hello").await }
    });
    metadata.entered.notified().await;
    stat_task.abort();
    assert!(stat_task.await.unwrap_err().is_cancelled());
    view.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn cancelled_final_operation_pin_release_retries_without_losing_token() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner, blocks.clone(), "cancel-operation-release-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options(
            "cancel-operation-release",
            "cancel-operation-release-blocks",
        ),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = Arc::new(coordinator.view(&first.id).await.unwrap());

    metadata.block_next_begin();
    let stat_task = tokio::spawn({
        let view = Arc::clone(&view);
        async move { view.stat("/hello").await }
    });
    metadata.entered.notified().await;
    // Poll close once while begin holds the state mutex. Tokio's FIFO mutex
    // queues this close ahead of the operation's later finish lock attempt.
    let mut close_future = Box::pin(view.close());
    let first_poll =
        std::future::poll_fn(|cx| std::task::Poll::Ready(close_future.as_mut().poll(cx))).await;
    assert!(first_poll.is_pending());
    metadata.block_next_close();
    metadata.release.notify_one();
    close_future.await.unwrap();
    metadata.close_entered.notified().await;
    // The first close is still suspended. Make the automatic retry suspend
    // too, so the provider pin must stay active until it acknowledges release.
    metadata.block_next_close();
    stat_task.abort();
    assert!(stat_task.await.unwrap_err().is_cancelled());
    metadata.close_entered.notified().await;
    assert_eq!(
        coordinator.delete(&first.id).await.unwrap_err().code,
        ErrorCode::Ebusy
    );
    metadata.close_release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match coordinator.delete(&first.id).await {
                Ok(()) => break,
                Err(error) if error.is(ErrorCode::Ebusy) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected delete error: {error}"),
            }
        }
    })
    .await
    .expect("automatic retry must release the exact pin");
}

#[tokio::test]
async fn cancelled_open_after_handle_reservation_does_not_leak_a_pin() {
    let inner = MemoryMetadataStore::new();
    let metadata = BlockingRenewMetadata::new(inner.clone());
    let blocks = MemoryBlockStore::new();
    seed_current(inner, blocks.clone(), "cancel-open-reservation-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("cancel-open-reservation", "cancel-open-reservation-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let view = Arc::new(coordinator.view(&first.id).await.unwrap());

    metadata.block_next_finish();
    let open_task = tokio::spawn({
        let view = Arc::clone(&view);
        async move { view.open("/hello", "r", 0).await }
    });
    metadata.entered.notified().await;
    open_task.abort();
    match open_task.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("open task should be cancelled"),
    }

    view.close().await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn sqlite_history_survives_reopen_and_reads_historical_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let metadata_path = directory.path().join("metadata.db");
    let blocks_path = directory.path().join("blocks.db");
    let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
    let blocks = SqliteBlockStore::open(&blocks_path).unwrap();
    seed_current(metadata.clone(), blocks.clone(), "sqlite-writer").await;

    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks.clone(),
        options("sqlite-version", "sqlite-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    assert!(first.durable);
    drop(coordinator);
    drop(metadata);
    drop(blocks);

    let reopened_metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
    let reopened_blocks = SqliteBlockStore::open(&blocks_path).unwrap();
    let reopened = VersionedCoordinator::new(
        reopened_metadata,
        reopened_blocks,
        options("sqlite-version-reopened", "sqlite-blocks"),
    )
    .unwrap();
    let history = reopened.history().await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, first.id);
    let view = reopened.view(&first.id).await.unwrap();
    assert_eq!(read_file(&view, "/hello").await, b"old bytes");
    view.close().await.unwrap();
}

#[tokio::test]
async fn sqlite_pin_close_is_atomic_and_stale_or_missing_is_defined() {
    let metadata = SqliteMetadataStore::in_memory().unwrap();
    let blocks = SqliteBlockStore::in_memory().unwrap();
    seed_current(metadata.clone(), blocks.clone(), "sqlite-pin-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("sqlite-pin", "sqlite-pin-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let _current = coordinator.snapshot().await.unwrap();
    let pin = metadata
        .open_view_pin(
            &first.id,
            ReadLeaseRequest {
                owner: "pin-owner".to_owned(),
                ttl: Duration::from_secs(60),
            },
        )
        .await
        .unwrap();
    let stale = ReadLease {
        expires_at_ms: pin.expires_at_ms.saturating_add(1),
        ..pin.clone()
    };
    assert_eq!(
        metadata.close_view_pin(&stale).await.unwrap_err().code,
        ErrorCode::Estale
    );
    assert_eq!(
        coordinator.delete(&first.id).await.unwrap_err().code,
        ErrorCode::Ebusy
    );

    metadata.close_view_pin(&pin).await.unwrap();
    metadata.close_view_pin(&pin).await.unwrap();
    coordinator.delete(&first.id).await.unwrap();
}

#[tokio::test]
async fn sqlite_head_revision_is_a_consistent_pair() {
    let metadata = SqliteMetadataStore::in_memory().unwrap();
    let blocks = SqliteBlockStore::in_memory().unwrap();
    seed_current(metadata.clone(), blocks.clone(), "sqlite-head-writer").await;
    let coordinator = VersionedCoordinator::new(
        metadata.clone(),
        blocks,
        options("sqlite-head", "sqlite-head-blocks"),
    )
    .unwrap();
    let first = coordinator.snapshot().await.unwrap();
    let head = coordinator.head().await.unwrap().unwrap();
    let loaded = metadata.load().await.unwrap();
    assert_eq!(head.version, first.id);
    assert_eq!(head.revision, loaded.revision);
}

async fn replay_cases<M>(metadata: M, namespace: Namespace)
where
    M: VersionedMetadataStore + Clone + 'static,
{
    let lease = metadata
        .acquire_writer("replay", Duration::from_secs(60))
        .await
        .unwrap();
    let volume = metadata.volume_id();
    let operation = PublicationId::new("replay-once").unwrap();
    let block_store_id = BlockStoreId::new("replay-blocks").unwrap();
    let initial = metadata.load().await.unwrap();
    let first_publication = VersionPublication {
        expected_revision: initial.revision,
        expected_parent: None,
        operation_id: operation.clone(),
        namespace: namespace.clone(),
        block_store_id: block_store_id.clone(),
        kind: VersionKind::Snapshot,
        restored_from: None,
        forked_from: None,
        durable: false,
    };
    let first = metadata
        .publish_version(&lease, first_publication.clone())
        .await
        .unwrap();
    assert_eq!(first.id.volume, volume);

    let loaded = metadata.load().await.unwrap();
    let later = VersionPublication {
        expected_revision: loaded.revision,
        expected_parent: Some(first.id.clone()),
        operation_id: PublicationId::new("replay-later").unwrap(),
        namespace,
        block_store_id,
        kind: VersionKind::Snapshot,
        restored_from: None,
        forked_from: None,
        durable: false,
    };
    let second = metadata.publish_version(&lease, later).await.unwrap();
    assert_eq!(second.parent, Some(first.id.clone()));

    let expired_lease = WriterLease {
        expires_at_ms: 0,
        ..lease.clone()
    };
    let replay = metadata
        .publish_version(&expired_lease, first_publication.clone())
        .await
        .unwrap();
    assert_eq!(replay.id, first.id);
    assert_eq!(
        metadata
            .find_publication(&operation)
            .await
            .unwrap()
            .unwrap()
            .id,
        first.id
    );

    let mismatched = VersionPublication {
        block_store_id: BlockStoreId::new("different").unwrap(),
        ..first_publication
    };
    assert_eq!(
        metadata
            .publish_version(&expired_lease, mismatched)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Eexist
    );
}

#[tokio::test]
async fn memory_replays_committed_operation_after_head_moves() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    let namespace = seed_current(metadata.clone(), blocks, "replay-memory-writer").await;
    replay_cases(metadata, namespace).await;
}

#[tokio::test]
async fn sqlite_replays_committed_operation_after_head_moves() {
    let metadata = SqliteMetadataStore::in_memory().unwrap();
    let blocks = SqliteBlockStore::in_memory().unwrap();
    let namespace = seed_current(metadata.clone(), blocks, "replay-sqlite-writer").await;
    replay_cases(metadata, namespace).await;
}
