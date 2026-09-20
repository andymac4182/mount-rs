//! Composed-driver acceptance across independently selected storage providers.
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::{
    FsDriver, Loopback, MkdirOptions,
    storage::{BlockId, BlockStore, MetadataStore},
};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectStore, PutMode, PutOptions, PutPayload, UpdateVersion};

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

/// Records only objects successfully created by this test, never lists/deletes
/// a bucket or a pre-existing prefix. Cleanup runs only after the driver
/// closes and verifies that the unique owned prefix is empty.
#[derive(Clone)]
struct TrackedR2Blocks {
    inner: R2BlockStore,
    object_store: Arc<dyn ObjectStore>,
    prefix: String,
    created: Arc<Mutex<BTreeSet<String>>>,
}

impl TrackedR2Blocks {
    fn new(config: &R2Config, prefix: String) -> Self {
        let object_store = config.build_store().expect("live R2 object store");
        let inner = R2BlockStore::new(object_store.clone(), prefix.clone(), true)
            .expect("live R2 block store");
        Self {
            inner,
            object_store,
            prefix,
            created: Default::default(),
        }
    }

    fn object_path(&self, name: &str) -> ObjectPath {
        ObjectPath::from(format!("{}/{}", self.prefix, name))
    }

    fn track_key(&self, key: impl Into<String>) {
        self.created
            .lock()
            .expect("tracked R2 object lock")
            .insert(key.into());
    }

    fn track_block(&self, id: &BlockId) {
        self.track_key(format!("{}/{}", self.prefix, id.0));
    }

    async fn cleanup(&self) {
        let keys = self
            .created
            .lock()
            .expect("tracked R2 object lock")
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            match self.object_store.delete(&ObjectPath::from(key)).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(error) => panic!("delete owned live R2 object: {error}"),
            }
        }

        let remaining = self
            .object_store
            .list_with_delimiter(Some(&ObjectPath::from(self.prefix.clone())))
            .await
            .expect("list owned live R2 prefix after cleanup");
        assert!(
            remaining.objects.is_empty() && remaining.common_prefixes.is_empty(),
            "live R2 owned prefix still contains objects after exact cleanup: {:?}",
            remaining.objects
        );
    }
}

async fn run_with_exact_r2_cleanup<F>(blocks: TrackedR2Blocks, operation: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    // A spawned test body lets cleanup run even when an assertion panics. The
    // cleanup itself remains exact-key-only and still verifies the unique
    // owned prefix after all successful operations have finished.
    let result = tokio::spawn(operation).await;
    blocks.cleanup().await;
    result.expect("live R2 test operation panicked");
}

#[async_trait::async_trait]
impl BlockStore for TrackedR2Blocks {
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<mount_rs_core::storage::BlockId> {
        let id = self.inner.put(bytes).await?;
        self.track_block(&id);
        Ok(id)
    }
    async fn get(&self, id: &BlockId) -> mount_rs_core::Result<Vec<u8>> {
        self.inner.get(id).await
    }
    async fn flush(&self) -> mount_rs_core::Result<()> {
        self.inner.flush().await
    }
    async fn delete(&self, id: &BlockId) -> mount_rs_core::Result<()> {
        let result = self.inner.delete(id).await;
        if result.is_ok() {
            self.created
                .lock()
                .expect("tracked R2 object lock")
                .remove(&format!("{}/{}", self.prefix, id.0));
        }
        result
    }
}

async fn assert_live_r2_block_contract(config: &R2Config, blocks: &TrackedR2Blocks) {
    let payload = (0_u32..131_072)
        .map(|index| (index.wrapping_mul(37) & 0xff) as u8)
        .collect::<Vec<_>>();
    let first_id = blocks.put(&payload).await.unwrap();
    let second_id = blocks.put(&payload).await.unwrap();
    assert_ne!(
        first_id, second_id,
        "live R2 immutable block publication reused an ID"
    );
    blocks.flush().await.unwrap();
    assert_eq!(blocks.get(&first_id).await.unwrap(), payload);

    // Rebuild both provider clients after publication. The full read goes
    // through a fresh R2BlockStore; the range calls go through fresh signed
    // ObjectStore clients because BlockStore intentionally exposes full reads.
    let fresh_blocks = R2BlockStore::from_config(config, blocks.prefix.clone()).unwrap();
    assert_eq!(fresh_blocks.get(&first_id).await.unwrap(), payload);
    let fresh_object_store = config.build_store().unwrap();
    let first_path = blocks.object_path(&first_id.0);
    assert_eq!(
        fresh_object_store
            .get(&first_path)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        payload
    );
    assert_eq!(
        fresh_object_store
            .get_range(&first_path, 7..23)
            .await
            .unwrap()
            .as_ref(),
        &payload[7..23]
    );
    let ranges = fresh_object_store
        .get_ranges(&first_path, &[0..11, 4096..4113, 131_000..131_072])
        .await
        .unwrap();
    assert_eq!(ranges[0].as_ref(), &payload[0..11]);
    assert_eq!(ranges[1].as_ref(), &payload[4096..4113]);
    assert_eq!(ranges[2].as_ref(), &payload[131_000..131_072]);

    // A block object itself is immutable: a second create-only publication at
    // its exact key must fail and must not replace its bytes.
    let duplicate = fresh_object_store
        .put_opts(
            &first_path,
            PutPayload::from(b"must-not-overwrite".to_vec()),
            PutOptions {
                mode: PutMode::Create,
                ..Default::default()
            },
        )
        .await
        .expect_err("live R2 must reject an immutable block overwrite");
    assert!(
        matches!(
            duplicate,
            object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. }
        ),
        "unexpected live R2 immutable overwrite error: {duplicate}"
    );
    assert_eq!(fresh_blocks.get(&first_id).await.unwrap(), payload);

    let conditional_name = "conditional-object";
    let conditional_path = blocks.object_path(conditional_name);
    blocks.track_key(conditional_path.to_string());
    let conditional_body = b"first conditional value";
    let created = fresh_object_store
        .put_opts(
            &conditional_path,
            PutPayload::from(conditional_body.to_vec()),
            PutOptions {
                mode: PutMode::Create,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let duplicate = fresh_object_store
        .put_opts(
            &conditional_path,
            PutPayload::from(b"must-not-overwrite".to_vec()),
            PutOptions {
                mode: PutMode::Create,
                ..Default::default()
            },
        )
        .await
        .expect_err("live R2 must reject a duplicate conditional create");
    assert!(
        matches!(
            duplicate,
            object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. }
        ),
        "unexpected live R2 conditional-create error: {duplicate}"
    );

    let metadata = fresh_object_store.head(&conditional_path).await.unwrap();
    assert!(
        metadata.e_tag.is_some(),
        "live R2 conditional-write coverage requires an object ETag"
    );
    let stale_read = fresh_object_store
        .get_opts(
            &conditional_path,
            GetOptions {
                if_match: Some("\"not-the-current-etag\"".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect_err("live R2 must reject a stale conditional read");
    assert!(
        matches!(stale_read, object_store::Error::Precondition { .. }),
        "unexpected live R2 stale-read error: {stale_read}"
    );
    let stale_update = fresh_object_store
        .put_opts(
            &conditional_path,
            PutPayload::from(b"must-not-win-the-CAS".to_vec()),
            PutOptions {
                mode: PutMode::Update(UpdateVersion {
                    e_tag: Some("\"not-the-current-etag\"".to_owned()),
                    version: None,
                }),
                ..Default::default()
            },
        )
        .await
        .expect_err("live R2 must reject a stale conditional write");
    assert!(
        matches!(stale_update, object_store::Error::Precondition { .. }),
        "unexpected live R2 stale-write error: {stale_update}"
    );
    let updated = fresh_object_store
        .put_opts(
            &conditional_path,
            PutPayload::from(b"second conditional value".to_vec()),
            PutOptions {
                mode: PutMode::Update(UpdateVersion {
                    e_tag: metadata.e_tag.clone(),
                    version: metadata.version.clone(),
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_ne!(created.e_tag, updated.e_tag);
    assert_eq!(
        fresh_object_store
            .get(&conditional_path)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        b"second conditional value"
    );

    // Publish through independent signed clients concurrently. Each task
    // reconstructs its client so this also covers reconnect/fresh-client
    // publication instead of sharing one in-process HTTP client.
    let mut workers = Vec::new();
    for worker in 0..8_u8 {
        let config = config.clone();
        let prefix = blocks.prefix.clone();
        workers.push(tokio::spawn(async move {
            let client = R2BlockStore::from_config(&config, prefix).unwrap();
            let body = vec![worker; 4096];
            let id = client.put(&body).await.unwrap();
            (id, body)
        }));
    }
    let mut concurrent_ids = BTreeSet::new();
    for worker in workers {
        let (id, body) = worker.await.unwrap();
        assert!(concurrent_ids.insert(id.0.clone()));
        blocks.track_block(&id);
        let fresh_client = R2BlockStore::from_config(config, blocks.prefix.clone()).unwrap();
        assert_eq!(fresh_client.get(&id).await.unwrap(), body);
    }
}

#[tokio::test]
#[ignore = "requires dedicated live Cloudflare R2 test credentials; local object storage is not evidence"]
async fn live_r2_blocks_with_independent_sqlite_metadata() {
    let config =
        mount_rs_r2::R2Config::from_env().expect("dedicated R2 test configuration required");
    let prefix = format!(
        "mount-rs-tests/split-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let blocks = TrackedR2Blocks {
        inner: mount_rs_r2::R2BlockStore::from_config(&config, prefix.clone()).unwrap(),
        object_store: config.build_store().unwrap(),
        prefix: prefix.clone(),
        created: Default::default(),
    };
    run_with_exact_r2_cleanup(blocks.clone(), async move {
        assert_live_r2_block_contract(&config, &blocks).await;
        let directory = tempfile::tempdir().unwrap();
        let metadata_path = directory.path().join("metadata.sqlite");
        exercise(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            blocks.clone(),
        )
        .await;
        // Fresh metadata connection and fresh authenticated R2 client, rather
        // than merely retaining the original driver's in-process handles.
        let reopened = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            mount_rs_r2::R2BlockStore::from_config(&config, prefix).unwrap(),
            ChunkedOptions::fixed("live-r2-reopen", 4096).unwrap(),
        )
        .await
        .unwrap();
        let mut expected = (0..33).map(|i| (i * 37) as u8).collect::<Vec<_>>();
        expected[5..14].copy_from_slice(&[0, 255, 12, 14, 16, 18, 20, 22, 24]);
        expected.resize(84, 0);
        expected[82..].copy_from_slice(&[99, 98]);
        assert_eq!(
            Loopback::new(reopened.clone())
                .read_file("/alias")
                .await
                .unwrap(),
            expected
        );
        reopened.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test]
#[ignore = "requires the isolated real PGlite server from scripts/test-pglite.sh"]
async fn pglite_metadata_and_blocks_compose_independently() {
    use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore};
    let url = std::env::var("PGLITE_DATABASE_URL").expect("PGLITE_DATABASE_URL required");
    let scope = format!(
        "split-store-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    exercise(
        PgliteMetadataStore::connect_with_key(&url, format!("{scope}-metadata-memory"))
            .await
            .unwrap(),
        MemoryBlockStore::new(),
    )
    .await;
    exercise(
        SqliteMetadataStore::in_memory().unwrap(),
        PgliteBlockStore::connect_with_key(&url, format!("{scope}-sqlite-blocks"))
            .await
            .unwrap(),
    )
    .await;
    exercise(
        PgliteMetadataStore::connect_with_key(&url, format!("{scope}-both"))
            .await
            .unwrap(),
        PgliteBlockStore::connect_with_key(&url, format!("{scope}-both"))
            .await
            .unwrap(),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires dedicated live R2 credentials and an isolated real PGlite server"]
async fn live_r2_blocks_with_independent_pglite_metadata() {
    use mount_rs_pglite::PgliteMetadataStore;
    let config = mount_rs_r2::R2Config::from_env().expect("dedicated R2 configuration required");
    let url = std::env::var("PGLITE_DATABASE_URL").expect("PGLITE_DATABASE_URL required");
    let scope = format!(
        "pglite-r2-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let prefix = format!("mount-rs-tests/{scope}");
    let blocks = TrackedR2Blocks::new(&config, prefix.clone());
    run_with_exact_r2_cleanup(blocks.clone(), async move {
        assert_live_r2_block_contract(&config, &blocks).await;
        exercise(
            PgliteMetadataStore::connect_with_key(&url, &scope)
                .await
                .unwrap(),
            blocks.clone(),
        )
        .await;
        // Both clients are fresh; no retained in-process byte cache can
        // satisfy this read.
        let reopened = ChunkedFs::open(
            PgliteMetadataStore::connect_with_key(&url, &scope)
                .await
                .unwrap(),
            mount_rs_r2::R2BlockStore::from_config(&config, prefix).unwrap(),
            ChunkedOptions::fixed("pglite-r2-reopen", 65536).unwrap(),
        )
        .await
        .unwrap();
        let mut expected = (0..33).map(|i| (i * 37) as u8).collect::<Vec<_>>();
        expected[5..14].copy_from_slice(&[0, 255, 12, 14, 16, 18, 20, 22, 24]);
        expected.resize(84, 0);
        expected[82..].copy_from_slice(&[99, 98]);
        assert_eq!(
            Loopback::new(reopened.clone())
                .read_file("/alias")
                .await
                .unwrap(),
            expected
        );
        reopened.shutdown().await.unwrap();
    })
    .await;
    // The metadata volume belongs to the isolated test server. Never clear a
    // shared database or list/delete objects outside the exact created IDs.
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
