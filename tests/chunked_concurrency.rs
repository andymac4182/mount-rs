use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::{
    ErrorCode, FsDriver,
    storage::{BlockId, BlockStore, MetadataStore, NAMESPACE_FORMAT_VERSION, NodeData},
};
use mount_rs_memfs::MemoryFs;
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use tempfile::tempdir;
use tokio::sync::{Barrier, Notify};
use tokio::time::timeout;

const OP_TIMEOUT: Duration = Duration::from_secs(10);
const RECORD_LEN: usize = 32;
const APPEND_WORKERS: usize = 6;
const RECORDS_PER_WORKER: usize = 20;

fn append_record(worker: usize, sequence: usize) -> Vec<u8> {
    let header = format!("{worker:02}:{sequence:02}|");
    let mut record = vec![b'.'; RECORD_LEN];
    record[..header.len()].copy_from_slice(header.as_bytes());
    record
}

async fn append_records<D>(driver: D) -> Vec<Vec<u8>>
where
    D: FsDriver + 'static,
{
    let fs = mount_rs_core::Loopback::new(driver);
    let seed = fs.open("/records", "w", 0o600).await.unwrap();
    seed.close().await.unwrap();

    let barrier = Arc::new(Barrier::new(APPEND_WORKERS));
    let mut tasks = Vec::with_capacity(APPEND_WORKERS);
    for worker in 0..APPEND_WORKERS {
        let fs = fs.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            let handle = fs.open("/records", "a", 0o600).await.unwrap();
            barrier.wait().await;
            for sequence in 0..RECORDS_PER_WORKER {
                let record = append_record(worker, sequence);
                assert_eq!(handle.write(&record, None).await.unwrap(), record.len());
            }
            handle.close().await.unwrap();
        }));
    }

    for task in tasks {
        timeout(OP_TIMEOUT, task)
            .await
            .expect("append worker timed out")
            .expect("append worker panicked");
    }

    let bytes = timeout(OP_TIMEOUT, fs.read_file("/records"))
        .await
        .expect("append read timed out")
        .unwrap();
    assert_eq!(bytes.len() % RECORD_LEN, 0);

    let mut records: Vec<_> = bytes
        .chunks_exact(RECORD_LEN)
        .map(ToOwned::to_owned)
        .collect();
    records.sort();
    records
}

async fn chunked_append_records<M, B>(metadata: M, blocks: B) -> Vec<Vec<u8>>
where
    M: MetadataStore + Clone + 'static,
    B: BlockStore + Clone + 'static,
{
    let fs = ChunkedFs::open(
        metadata,
        blocks,
        ChunkedOptions::fixed("append-owner", RECORD_LEN).unwrap(),
    )
    .await
    .unwrap();
    let records = append_records(fs.clone()).await;
    fs.shutdown().await.unwrap();
    records
}

#[tokio::test]
async fn concurrent_appends_are_atomic_across_split_provider_combinations() {
    let mut expected = Vec::with_capacity(APPEND_WORKERS * RECORDS_PER_WORKER);
    for worker in 0..APPEND_WORKERS {
        for sequence in 0..RECORDS_PER_WORKER {
            expected.push(append_record(worker, sequence));
        }
    }
    expected.sort();

    let memory_fs = timeout(OP_TIMEOUT, append_records(MemoryFs::empty()))
        .await
        .expect("MemoryFs append test timed out");
    assert_eq!(memory_fs, expected);

    let memory_memory = timeout(
        OP_TIMEOUT,
        chunked_append_records(MemoryMetadataStore::new(), MemoryBlockStore::new()),
    )
    .await
    .expect("memory/memory append test timed out");
    assert_eq!(memory_memory, memory_fs);

    let sqlite_metadata = timeout(
        OP_TIMEOUT,
        chunked_append_records(
            SqliteMetadataStore::in_memory().unwrap(),
            MemoryBlockStore::new(),
        ),
    )
    .await
    .expect("sqlite-metadata append test timed out");
    assert_eq!(sqlite_metadata, memory_fs);

    let sqlite_blocks = timeout(
        OP_TIMEOUT,
        chunked_append_records(
            MemoryMetadataStore::new(),
            SqliteBlockStore::in_memory().unwrap(),
        ),
    )
    .await
    .expect("sqlite-block append test timed out");
    assert_eq!(sqlite_blocks, memory_fs);

    let sqlite_split = timeout(
        OP_TIMEOUT,
        chunked_append_records(
            SqliteMetadataStore::in_memory().unwrap(),
            SqliteBlockStore::in_memory().unwrap(),
        ),
    )
    .await
    .expect("sqlite/sqlite append test timed out");
    assert_eq!(sqlite_split, memory_fs);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SharedInodeOutcome {
    data_after_first_unlink: Vec<u8>,
    nlink_after_first_unlink: u64,
    data_after_last_unlink: Vec<u8>,
    nlink_after_last_unlink: u64,
    missing_path_error: ErrorCode,
}

async fn shared_inode_outcome<D>(driver: D) -> SharedInodeOutcome
where
    D: FsDriver + 'static,
{
    let fs = mount_rs_core::Loopback::new(driver);
    let initial = fs.open("/source", "w", 0o600).await.unwrap();
    assert_eq!(initial.write(b"aaaa", Some(0)).await.unwrap(), 4);
    initial.close().await.unwrap();

    let held = fs.open("/source", "r+", 0o600).await.unwrap();
    fs.link("/source", "/alias").await.unwrap();
    fs.rename("/source", "/renamed").await.unwrap();
    fs.unlink("/renamed").await.unwrap();

    assert_eq!(held.write(b"bb", Some(0)).await.unwrap(), 2);
    assert_eq!(held.write(b"cc", Some(2)).await.unwrap(), 2);

    let alias_stat = fs.stat("/alias").await.unwrap();
    let mut data_after_first_unlink = vec![0; 4];
    assert_eq!(
        held.read(&mut data_after_first_unlink, Some(0))
            .await
            .unwrap(),
        4
    );

    fs.unlink("/alias").await.unwrap();
    let orphan_stat = held.stat().await.unwrap();
    let mut data_after_last_unlink = vec![0; 4];
    assert_eq!(
        held.read(&mut data_after_last_unlink, Some(0))
            .await
            .unwrap(),
        4
    );
    held.close().await.unwrap();

    let missing_path_error = fs.stat("/alias").await.unwrap_err().code;
    SharedInodeOutcome {
        data_after_first_unlink,
        nlink_after_first_unlink: alias_stat.nlink,
        data_after_last_unlink,
        nlink_after_last_unlink: orphan_stat.nlink,
        missing_path_error,
    }
}

async fn chunked_shared_inode_outcome<M, B>(metadata: M, blocks: B) -> SharedInodeOutcome
where
    M: MetadataStore + Clone + 'static,
    B: BlockStore + Clone + 'static,
{
    let fs = ChunkedFs::open(
        metadata,
        blocks,
        ChunkedOptions::fixed("inode-owner", RECORD_LEN).unwrap(),
    )
    .await
    .unwrap();
    let outcome = shared_inode_outcome(fs.clone()).await;
    fs.shutdown().await.unwrap();
    outcome
}

#[tokio::test]
async fn shared_inode_handles_match_memoryfs_across_links_rename_and_unlink() {
    let expected = timeout(OP_TIMEOUT, shared_inode_outcome(MemoryFs::empty()))
        .await
        .expect("MemoryFs inode test timed out");

    let memory_memory = timeout(
        OP_TIMEOUT,
        chunked_shared_inode_outcome(MemoryMetadataStore::new(), MemoryBlockStore::new()),
    )
    .await
    .expect("memory/memory inode test timed out");
    assert_eq!(memory_memory, expected);

    let sqlite_metadata = timeout(
        OP_TIMEOUT,
        chunked_shared_inode_outcome(
            SqliteMetadataStore::in_memory().unwrap(),
            MemoryBlockStore::new(),
        ),
    )
    .await
    .expect("sqlite-metadata inode test timed out");
    assert_eq!(sqlite_metadata, expected);

    let sqlite_blocks = timeout(
        OP_TIMEOUT,
        chunked_shared_inode_outcome(
            MemoryMetadataStore::new(),
            SqliteBlockStore::in_memory().unwrap(),
        ),
    )
    .await
    .expect("sqlite-block inode test timed out");
    assert_eq!(sqlite_blocks, expected);

    let sqlite_split = timeout(
        OP_TIMEOUT,
        chunked_shared_inode_outcome(
            SqliteMetadataStore::in_memory().unwrap(),
            SqliteBlockStore::in_memory().unwrap(),
        ),
    )
    .await
    .expect("sqlite/sqlite inode test timed out");
    assert_eq!(sqlite_split, expected);
}

#[tokio::test]
async fn fixed_chunk_metadata_survives_partial_rewrite_truncate_extend_and_reopen() {
    timeout(OP_TIMEOUT, async {
        let directory = tempdir().unwrap();
        let metadata_path = directory.path().join("metadata.sqlite");
        let blocks_path = directory.path().join("blocks.sqlite");

        let first = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("layout-first", 4).unwrap(),
        )
        .await
        .unwrap();
        let first_loopback = mount_rs_core::Loopback::new(first.clone());
        let file = first.open("/payload", "w+", 0o600).await.unwrap();
        file.write(b"abcdefghij", Some(0)).await.unwrap();
        file.write(b"XYZZ", Some(3)).await.unwrap();
        file.truncate(6).await.unwrap();
        file.truncate(11).await.unwrap();
        file.write(b"tail", Some(7)).await.unwrap();
        file.sync().await.unwrap();
        file.close().await.unwrap();

        let expected = b"abcXYZ\0tail";
        assert_eq!(
            first_loopback.read_file("/payload").await.unwrap(),
            expected
        );
        first.shutdown().await.unwrap();

        let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
        let loaded = metadata.load().await.unwrap();
        assert!(
            loaded.revision >= 6,
            "each acknowledged mutation is versioned"
        );
        let namespace = loaded.namespace.clone().unwrap();
        assert_eq!(namespace.format_version, NAMESPACE_FORMAT_VERSION);
        assert_eq!(namespace.default_chunker.algorithm, "fixed-size");
        assert_eq!(namespace.default_chunker.version, 1);
        assert_eq!(namespace.default_chunker.parameters["chunk_size"], 4);
        let payload = namespace
            .nodes
            .values()
            .find(|node| {
                node.stats.ino != namespace.root && node.stats.size == expected.len() as u64
            })
            .expect("persisted payload inode");
        let NodeData::File(layout) = &payload.data else {
            panic!("persisted payload must remain a file layout");
        };
        assert_eq!(layout.chunker, namespace.default_chunker);
        assert!(layout.extents.iter().all(|extent| {
            extent.file_offset % 4 == 0 && extent.block_offset == 0 && extent.length <= 4
        }));
        loaded.validate().unwrap();
        drop(metadata);

        // The persisted namespace default, rather than a new opener's
        // requested default, must control files created after a reopen.
        let reopened = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("layout-second", 1024).unwrap(),
        )
        .await
        .unwrap();
        let reopened_loopback = mount_rs_core::Loopback::new(reopened.clone());
        assert_eq!(
            reopened_loopback.read_file("/payload").await.unwrap(),
            expected
        );
        reopened_loopback
            .write_file("/new", b"12345")
            .await
            .unwrap();
        let reopened_namespace = reopened
            .metadata_store()
            .load()
            .await
            .unwrap()
            .namespace
            .unwrap();
        let new_file = reopened_namespace
            .nodes
            .values()
            .find(|node| node.stats.size == 5)
            .expect("new file inode after reopen");
        let NodeData::File(new_layout) = &new_file.data else {
            panic!("new file must remain a file layout");
        };
        assert_eq!(new_layout.chunker.parameters["chunk_size"], 4);
        reopened.shutdown().await.unwrap();
    })
    .await
    .expect("fixed chunk persistence test timed out");
}

#[tokio::test]
async fn sqlite_writer_instances_fence_single_owner_across_connections() {
    timeout(OP_TIMEOUT, async {
        let directory = tempdir().unwrap();
        let metadata_path = directory.path().join("metadata.sqlite");
        let blocks_path = directory.path().join("blocks.sqlite");

        let first = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("first-owner", 16)
                .unwrap()
                .with_lease_ttl(Duration::from_secs(60)),
        )
        .await
        .unwrap();
        let first_loopback = mount_rs_core::Loopback::new(first.clone());
        first_loopback
            .write_file("/before", b"before")
            .await
            .unwrap();

        let busy = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("busy-owner", 16)
                .unwrap()
                .with_lease_ttl(Duration::from_secs(60)),
        )
        .await;
        let busy_error = match busy {
            Ok(_) => panic!("a second live SQLite writer acquired the lease"),
            Err(error) => error,
        };
        assert_eq!(busy_error.code, ErrorCode::Eagain);

        // Expire the persisted authority explicitly after the live-owner
        // assertion. Slow filesystem setup must not consume the test lease;
        // publication and replacement still use the real SQLite provider.
        let fault_connection = rusqlite::Connection::open(&metadata_path).unwrap();
        assert_eq!(
            fault_connection
                .execute("UPDATE mount_rs_metadata SET expires=0", [])
                .unwrap(),
            1
        );
        drop(fault_connection);

        let second = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("second-owner", 16)
                .unwrap()
                .with_lease_ttl(Duration::from_secs(60)),
        )
        .await
        .unwrap();
        let second_loopback = mount_rs_core::Loopback::new(second.clone());
        assert_eq!(
            second_loopback.read_file("/before").await.unwrap(),
            b"before"
        );

        let stale = first.stat("/").await.unwrap_err();
        assert_eq!(stale.code, ErrorCode::Estale);
        assert!(first.failed());

        second_loopback
            .write_file("/winner", b"winner")
            .await
            .unwrap();
        second.shutdown().await.unwrap();
        let first_shutdown = first
            .shutdown()
            .await
            .expect_err("fenced writer unexpectedly released an old lease");
        assert_eq!(first_shutdown.code, ErrorCode::Estale);

        let third = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("third-owner", 16).unwrap(),
        )
        .await
        .unwrap();
        let third_loopback = mount_rs_core::Loopback::new(third.clone());
        assert_eq!(
            third_loopback.read_file("/before").await.unwrap(),
            b"before"
        );
        assert_eq!(
            third_loopback.read_file("/winner").await.unwrap(),
            b"winner"
        );
        third.shutdown().await.unwrap();
    })
    .await
    .expect("SQLite writer fencing test timed out");
}

#[tokio::test]
async fn fenced_writer_cannot_publish_after_a_blocked_partial_write() {
    timeout(OP_TIMEOUT, async {
        let clock = Arc::new(mount_rs_memory::ManualClock::new(0));
        let metadata = MemoryMetadataStore::with_clock(clock.clone());
        let blocks = BlockingBlockStore::new();
        let fs = ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            ChunkedOptions::fixed("stale-owner", 4)
                .unwrap()
                .with_lease_ttl(Duration::from_secs(1)),
        )
        .await
        .unwrap();
        let handle = fs.open("/payload", "w+", 0o600).await.unwrap();
        let entered = blocks.entered.notified();
        let write_task = tokio::spawn(async move { handle.write(b"stale", Some(1)).await });

        timeout(OP_TIMEOUT, entered)
            .await
            .expect("partial write did not reach the block provider");
        assert!(clock.advance_ms(1_000));
        let replacement = metadata
            .acquire_writer("replacement-owner", Duration::from_secs(10))
            .await
            .unwrap();
        let revision_before_release = metadata.load().await.unwrap().revision;

        blocks.release.notify_one();
        let error = timeout(OP_TIMEOUT, write_task)
            .await
            .expect("fenced write timed out")
            .expect("fenced write task panicked")
            .expect_err("stale writer unexpectedly acknowledged a write");
        assert_eq!(error.code, ErrorCode::Estale);
        assert_eq!(
            metadata.load().await.unwrap().revision,
            revision_before_release
        );
        assert!(fs.failed());

        metadata.release_writer(&replacement).await.unwrap();
        let fresh = ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            ChunkedOptions::fixed("fresh-owner", 64).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            mount_rs_core::Loopback::new(fresh.clone())
                .read_file("/payload")
                .await
                .unwrap(),
            b""
        );
        fresh.shutdown().await.unwrap();
        let _ = fs.shutdown().await;
    })
    .await
    .expect("stale publication test timed out");
}

#[derive(Clone)]
struct BlockingBlockStore {
    inner: MemoryBlockStore,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    blocked: Arc<AtomicBool>,
}

impl BlockingBlockStore {
    fn new() -> Self {
        Self {
            inner: MemoryBlockStore::new(),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            blocked: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[derive(Clone)]
struct BlockingReadBlockStore {
    inner: MemoryBlockStore,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    blocked: Arc<AtomicBool>,
}

impl BlockingReadBlockStore {
    fn new() -> Self {
        Self {
            inner: MemoryBlockStore::new(),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            blocked: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl BlockStore for BlockingReadBlockStore {
    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> mount_rs_core::Result<Vec<u8>> {
        if !self.blocked.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.get(id).await
    }

    async fn delete(&self, id: &BlockId) -> mount_rs_core::Result<()> {
        self.inner.delete(id).await
    }

    async fn flush(&self) -> mount_rs_core::Result<()> {
        self.inner.flush().await
    }

    fn durable(&self) -> bool {
        self.inner.durable()
    }
}

#[async_trait]
impl BlockStore for BlockingBlockStore {
    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
        if !self.blocked.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> mount_rs_core::Result<Vec<u8>> {
        self.inner.get(id).await
    }

    async fn delete(&self, id: &BlockId) -> mount_rs_core::Result<()> {
        self.inner.delete(id).await
    }

    async fn flush(&self) -> mount_rs_core::Result<()> {
        self.inner.flush().await
    }

    fn durable(&self) -> bool {
        self.inner.durable()
    }
}

#[tokio::test]
async fn shutdown_waits_for_an_inflight_operation_without_deadlocking() {
    timeout(OP_TIMEOUT, async {
        let blocks = BlockingBlockStore::new();
        let entered = blocks.entered.notified();
        let fs = ChunkedFs::open(
            MemoryMetadataStore::new(),
            blocks.clone(),
            ChunkedOptions::fixed("shutdown-owner", 8).unwrap(),
        )
        .await
        .unwrap();
        let handle = fs.open("/file", "w+", 0o600).await.unwrap();

        let write_task = tokio::spawn(async move { handle.write(b"blocked", Some(0)).await });
        timeout(OP_TIMEOUT, entered)
            .await
            .expect("write did not reach the blocking block store");

        let shutdown_fs = fs.clone();
        let shutdown_task = tokio::spawn(async move { shutdown_fs.shutdown().await });
        tokio::task::yield_now().await;
        blocks.release.notify_one();

        let write_result = timeout(OP_TIMEOUT, write_task)
            .await
            .expect("inflight write timed out")
            .expect("inflight write panicked")
            .unwrap();
        assert_eq!(write_result, 7);
        timeout(OP_TIMEOUT, shutdown_task)
            .await
            .expect("shutdown timed out")
            .expect("shutdown panicked")
            .unwrap();

        let closed = fs.stat("/").await.unwrap_err();
        assert_eq!(closed.code, ErrorCode::Ebadf);
    })
    .await
    .expect("shutdown/inflight test timed out");
}

#[tokio::test]
async fn cancelling_a_pending_operation_releases_the_filesystem_gate() {
    timeout(OP_TIMEOUT, async {
        let blocks = BlockingBlockStore::new();
        let entered = blocks.entered.notified();
        let fs = ChunkedFs::open(
            MemoryMetadataStore::new(),
            blocks.clone(),
            ChunkedOptions::fixed("cancel-owner", 8).unwrap(),
        )
        .await
        .unwrap();
        let handle = fs.open("/file", "w+", 0o600).await.unwrap();

        let pending = tokio::spawn(async move { handle.write(b"cancel", Some(0)).await });
        timeout(OP_TIMEOUT, entered)
            .await
            .expect("write did not reach the blocking block store");
        pending.abort();
        let cancelled = timeout(OP_TIMEOUT, pending)
            .await
            .expect("cancelled write join timed out")
            .expect_err("cancelled write unexpectedly completed");
        assert!(cancelled.is_cancelled());

        let root = timeout(OP_TIMEOUT, fs.stat("/"))
            .await
            .expect("stat after cancellation timed out")
            .unwrap();
        assert!(root.is_directory());
        let file = fs.stat("/file").await.unwrap();
        assert_eq!(file.size, 0);
        fs.shutdown().await.unwrap();
    })
    .await
    .expect("cancellation gate test timed out");
}

#[tokio::test]
async fn blocked_remote_read_releases_the_metadata_gate_but_shutdown_waits_for_it() {
    timeout(OP_TIMEOUT, async {
        let metadata = MemoryMetadataStore::new();
        let blocks = BlockingReadBlockStore::new();
        let entered = blocks.entered.notified();
        let fs = ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            ChunkedOptions::fixed("read-concurrency-owner", 8).unwrap(),
        )
        .await
        .unwrap();
        let writer = fs.open("/file", "w+", 0o600).await.unwrap();
        writer.write(b"blocked-read", Some(0)).await.unwrap();
        writer.close().await.unwrap();

        let reader = fs.open("/file", "r", 0o600).await.unwrap();
        let read_task = tokio::spawn(async move {
            let mut bytes = vec![0; 12];
            let count = reader.read(&mut bytes, Some(0)).await?;
            reader.close().await?;
            Ok::<_, mount_rs_core::FsError>((count, bytes))
        });
        timeout(OP_TIMEOUT, entered)
            .await
            .expect("read did not reach the blocking block store");

        let stat_task = tokio::spawn({
            let fs = fs.clone();
            async move { fs.stat("/").await }
        });
        timeout(Duration::from_millis(250), stat_task)
            .await
            .expect("metadata gate remained held by the blocked read")
            .expect("stat task panicked")
            .expect("stat failed while the remote read was blocked");

        let writer = fs.open("/file", "r+", 0o600).await.unwrap();
        writer.write(b"updated-read", Some(0)).await.unwrap();
        writer.close().await.unwrap();

        let shutdown_task = tokio::spawn({
            let fs = fs.clone();
            async move { fs.shutdown().await }
        });
        tokio::task::yield_now().await;
        blocks.release.notify_one();

        let (count, bytes) = timeout(OP_TIMEOUT, read_task)
            .await
            .expect("blocked read timed out")
            .expect("blocked read panicked")
            .unwrap();
        assert_eq!(count, 12);
        assert_eq!(&bytes, b"blocked-read");
        timeout(OP_TIMEOUT, shutdown_task)
            .await
            .expect("shutdown timed out behind the blocked read")
            .expect("shutdown panicked")
            .unwrap();

        let reopened = ChunkedFs::open(
            metadata,
            blocks,
            ChunkedOptions::fixed("read-concurrency-verifier", 8).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            mount_rs_core::Loopback::new(reopened.clone())
                .read_file("/file")
                .await
                .unwrap(),
            b"updated-read"
        );
        reopened.shutdown().await.unwrap();
    })
    .await
    .expect("read concurrency/shutdown test timed out");
}
