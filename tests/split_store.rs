//! Composed-driver acceptance across independently selected storage providers.
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::{
    FsDriver, Loopback, MkdirOptions,
    storage::{BlockStore, MetadataStore},
};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};

async fn exercise<M, B>(metadata: M, blocks: B)
where
    M: MetadataStore + Clone + 'static,
    B: BlockStore + Clone + 'static,
{
    let durable = metadata.durable() && blocks.durable();
    let fs = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("first", 7).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(fs.capabilities().durable_writes, durable);
    let loopback = Loopback::new(fs.clone());
    loopback
        .mkdir("/dir", MkdirOptions::default())
        .await
        .unwrap();
    let file = loopback.open("/dir/file", "w+", 0o640).await.unwrap();
    let mut expected = (0..71).map(|i| (i * 37) as u8).collect::<Vec<_>>();
    file.write(&expected, Some(0)).await.unwrap();
    file.write(&[0, 255, 12, 14, 16, 18, 20, 22, 24], Some(5))
        .await
        .unwrap();
    expected[5..14].copy_from_slice(&[0, 255, 12, 14, 16, 18, 20, 22, 24]);
    file.truncate(33).await.unwrap();
    expected.truncate(33);
    file.truncate(65).await.unwrap();
    expected.resize(65, 0);
    file.write(&[99, 98], Some(82)).await.unwrap();
    expected.resize(84, 0);
    expected[82..].copy_from_slice(&[99, 98]);
    file.sync().await.unwrap();
    let mut actual = vec![0; expected.len() + 3];
    assert_eq!(
        file.read(&mut actual, Some(0)).await.unwrap(),
        expected.len()
    );
    assert_eq!(&actual[..expected.len()], expected);
    loopback.link("/dir/file", "/alias").await.unwrap();
    loopback.symlink("alias", "/link").await.unwrap();
    loopback.unlink("/dir/file").await.unwrap();
    assert_eq!(file.stat().await.unwrap().nlink, 1);
    file.close().await.unwrap();
    assert_eq!(loopback.read_file("/link").await.unwrap(), expected);
    let loaded = metadata.load().await.unwrap();
    loaded.validate().unwrap();
    assert!(loaded.namespace.unwrap().nodes.values().any(|node|
        matches!(&node.data, mount_rs_core::storage::NodeData::File(layout) if layout.extents.len() > 1)));
    fs.shutdown().await.unwrap();
    // Changing the requested default must not reinterpret existing extents.
    let reopened = ChunkedFs::open(
        metadata,
        blocks,
        ChunkedOptions::fixed("second", 4096).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        Loopback::new(reopened.clone())
            .read_file("/alias")
            .await
            .unwrap(),
        expected
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn memory_metadata_with_memory_blocks() {
    exercise(MemoryMetadataStore::new(), MemoryBlockStore::new()).await;
}

#[tokio::test]
async fn sqlite_metadata_with_memory_blocks() {
    let directory = tempfile::tempdir().unwrap();
    exercise(
        SqliteMetadataStore::open(directory.path().join("metadata.db")).unwrap(),
        MemoryBlockStore::new(),
    )
    .await;
}

#[tokio::test]
async fn memory_metadata_with_sqlite_blocks() {
    let directory = tempfile::tempdir().unwrap();
    exercise(
        MemoryMetadataStore::new(),
        SqliteBlockStore::open(directory.path().join("blocks.db")).unwrap(),
    )
    .await;
}

#[tokio::test]
async fn separate_sqlite_metadata_and_block_databases() {
    let directory = tempfile::tempdir().unwrap();
    exercise(
        SqliteMetadataStore::open(directory.path().join("metadata.db")).unwrap(),
        SqliteBlockStore::open(directory.path().join("blocks.db")).unwrap(),
    )
    .await;
}

#[tokio::test]
async fn sqlite_metadata_with_independent_object_store_blocks() {
    let directory = tempfile::tempdir().unwrap();
    let blocks = mount_rs_r2::R2BlockStore::new(
        std::sync::Arc::new(object_store::memory::InMemory::new()),
        "mixed/blocks",
        false,
    )
    .unwrap();
    exercise(
        SqliteMetadataStore::open(directory.path().join("metadata.db")).unwrap(),
        blocks,
    )
    .await;
}

#[derive(Clone)]
struct FailingMetadataFlush {
    inner: MemoryMetadataStore,
    fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl MetadataStore for FailingMetadataFlush {
    fn durable(&self) -> bool {
        false
    }
    async fn load(&self) -> mount_rs_core::Result<mount_rs_core::storage::LoadedMetadata> {
        self.inner.load().await
    }
    async fn acquire_writer(
        &self,
        owner: &str,
        ttl: std::time::Duration,
    ) -> mount_rs_core::Result<mount_rs_core::storage::WriterLease> {
        self.inner.acquire_writer(owner, ttl).await
    }
    async fn renew_writer(
        &self,
        lease: &mount_rs_core::storage::WriterLease,
        ttl: std::time::Duration,
    ) -> mount_rs_core::Result<mount_rs_core::storage::WriterLease> {
        self.inner.renew_writer(lease, ttl).await
    }
    async fn release_writer(
        &self,
        lease: &mount_rs_core::storage::WriterLease,
    ) -> mount_rs_core::Result<()> {
        self.inner.release_writer(lease).await
    }
    async fn publish(
        &self,
        revision: u64,
        lease: &mount_rs_core::storage::WriterLease,
        namespace: mount_rs_core::storage::Namespace,
    ) -> mount_rs_core::Result<u64> {
        self.inner.publish(revision, lease, namespace).await
    }
    async fn flush(&self) -> mount_rs_core::Result<()> {
        if self.fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
            Err(mount_rs_core::FsError::backend(
                "injected metadata barrier failure",
            ))
        } else {
            self.inner.flush().await
        }
    }
}

#[tokio::test]
async fn uncertain_metadata_commit_is_reported_and_driver_fails_closed() {
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let metadata = FailingMetadataFlush {
        inner: MemoryMetadataStore::new(),
        fail: fail.clone(),
    };
    let blocks = MemoryBlockStore::new();
    let fs = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("first", 7).unwrap(),
    )
    .await
    .unwrap();
    let file = fs.open("/file", "w+", 0o600).await.unwrap();
    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        file.write(b"committed-but-unacknowledged", Some(0))
            .await
            .unwrap_err()
            .is(mount_rs_core::ErrorCode::Eio)
    );
    assert!(fs.failed());
    assert!(file.write(b"must-not-continue", Some(0)).await.is_err());
    fs.shutdown().await.unwrap();
    let reopened = ChunkedFs::open(
        metadata,
        blocks,
        ChunkedOptions::fixed("recovery", 7).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        Loopback::new(reopened.clone())
            .read_file("/file")
            .await
            .unwrap(),
        b"committed-but-unacknowledged"
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn sparse_terabyte_offset_and_unlinked_handle_do_not_require_whole_file_allocation() {
    let fs = ChunkedFs::open(
        MemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedOptions::fixed("sparse", 4096).unwrap(),
    )
    .await
    .unwrap();
    let file = fs.open("/sparse", "w+", 0o600).await.unwrap();
    let offset = 1_u64 << 40;
    file.write(&[123], Some(offset)).await.unwrap();
    assert_eq!(file.stat().await.unwrap().size, offset + 1);
    let mut tail = [255; 3];
    assert_eq!(file.read(&mut tail, Some(offset - 2)).await.unwrap(), 3);
    assert_eq!(tail, [0, 0, 123]);
    fs.unlink("/sparse").await.unwrap();
    assert_eq!(file.stat().await.unwrap().nlink, 0);
    file.write(&[99], Some(offset)).await.unwrap();
    file.read(&mut tail, Some(offset - 2)).await.unwrap();
    assert_eq!(tail, [0, 0, 99]);
    file.close().await.unwrap();
    fs.shutdown().await.unwrap();
}
