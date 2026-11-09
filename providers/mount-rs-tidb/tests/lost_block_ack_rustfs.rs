//! Opt-in live coverage for a lost RustFS block-put acknowledgment with TiDB
//! metadata. Mirrors the composed memory/SQLite/local-directory regression in
//! `mount-rs-fault-injection/tests/composed_filesystem.rs`: the second block
//! put succeeds in RustFS, then its reply is replaced with EIO.
//!
//! The RustFS and TiDB services are owned by the caller's harness; this test
//! writes under a unique prefix and volume key and removes both afterwards.

use async_trait::async_trait;
use mount_rs_chunked::{AtimePolicy, CachedBlockStore, ChunkedFs, ChunkedOptions};
use mount_rs_core::driver::FsDriver;
use mount_rs_core::storage::{BlockId, BlockStore, MetadataStore, Namespace};
use mount_rs_core::{ErrorCode, FsError};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_tidb::{TidbMetadataStore, TidbStorageOptions};
use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OPT_IN: &str = "MOUNT_RS_TIDB_CHUNKED_RUSTFS";
const BASELINE: &[u8] = b"baseline-payload-crossing-fixed-chunks";
const UPDATED: &[u8] = b"updated-payload-crossing-fixed-chunks";

/// Counts puts; the configured put reaches RustFS and then reports EIO.
#[derive(Clone)]
struct LostReplyBlocks {
    inner: R2BlockStore,
    created: Arc<Mutex<BTreeSet<String>>>,
    puts: Arc<AtomicU64>,
    lose_put: Arc<AtomicU64>,
    lost: Arc<Mutex<Option<BlockId>>>,
}

#[async_trait]
impl BlockStore for LostReplyBlocks {
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
        let id = self.inner.put(bytes).await?;
        self.created.lock().unwrap().insert(id.0.clone());
        let n = self.puts.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.lose_put.load(Ordering::SeqCst) {
            *self.lost.lock().unwrap() = Some(id);
            return Err(FsError::new(ErrorCode::Eio));
        }
        Ok(id)
    }
    async fn get(&self, id: &BlockId) -> mount_rs_core::Result<Vec<u8>> {
        self.inner.get(id).await
    }
    async fn flush(&self) -> mount_rs_core::Result<()> {
        self.inner.flush().await
    }
    async fn delete(&self, id: &BlockId) -> mount_rs_core::Result<()> {
        self.inner.delete(id).await
    }
}

fn storage(volume_key: &str) -> TidbStorageOptions {
    TidbStorageOptions::new(volume_key).with_durable(true)
}

fn chunked(owner: &str) -> ChunkedOptions {
    ChunkedOptions::fixed(owner, 4)
        .unwrap()
        .with_lease_ttl(Duration::from_secs(30))
        .with_atime_policy(AtimePolicy::NoAtime)
}

fn encoded(namespace: &Namespace) -> Vec<u8> {
    serde_json::to_vec(namespace).unwrap()
}

async fn committed(url: &str, key: &str) -> (u64, Vec<u8>) {
    let store = TidbMetadataStore::connect_with_options(url, storage(key))
        .await
        .unwrap();
    let loaded = store.load().await.unwrap();
    (loaded.revision, encoded(&loaded.namespace.unwrap()))
}

async fn read_all(file: &Arc<dyn mount_rs_core::driver::FileHandle>, len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), len);
    bytes
}

async fn cleanup(url: &str, key: &str, blocks: &LostReplyBlocks) {
    let created = blocks.created.lock().unwrap().clone();
    for id in created {
        match blocks.inner.delete(&BlockId(id)).await {
            Ok(()) => {}
            Err(error) if error.is(ErrorCode::Enoent) => {}
            Err(error) => panic!("RustFS cleanup failed: {error}"),
        }
    }
    let pool = Pool::from_url(url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    connection
        .exec_drop(
            "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (key,),
        )
        .await
        .unwrap_or_else(|error| panic!("TiDB cleanup failed: {error}"));
    drop(connection);
    pool.disconnect().await.unwrap();
}

async fn exercise(url: &str, config: &R2Config, retry: bool) {
    let run = format!(
        "{}-lost-ack-{}-{}",
        std::env::var("RUSTFS_COMBO_PREFIX").unwrap(),
        if retry { "retry" } else { "noretry" },
        std::process::id()
    );
    let key = format!("{run}/tidb");
    let blocks = LostReplyBlocks {
        inner: R2BlockStore::from_config(config, run.clone()).unwrap(),
        created: Arc::default(),
        puts: Arc::default(),
        lose_put: Arc::new(AtomicU64::new(u64::MAX)),
        lost: Arc::default(),
    };

    // Seed the baseline through an independent owner and shut it down.
    {
        let metadata = TidbMetadataStore::connect_with_options(url, storage(&key))
            .await
            .unwrap();
        let fs = ChunkedFs::open(metadata, blocks.clone(), chunked("seed"))
            .await
            .unwrap();
        let file = fs.open("/payload", "w+", 0o640).await.unwrap();
        assert_eq!(file.write(BASELINE, Some(0)).await.unwrap(), BASELINE.len());
        file.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }
    let (seed_revision, seed_namespace) = committed(url, &key).await;

    // Second put from now on reaches RustFS but loses its acknowledgment.
    blocks.puts.store(0, Ordering::SeqCst);
    blocks.lose_put.store(2, Ordering::SeqCst);
    let metadata = TidbMetadataStore::connect_with_options(url, storage(&key))
        .await
        .unwrap();
    let cache = CachedBlockStore::new(blocks.clone(), 4096, 128);
    let fs = ChunkedFs::open(metadata.clone(), cache.clone(), chunked("cached-writer"))
        .await
        .unwrap();
    let reader = fs.open("/payload", "r", 0).await.unwrap();
    let writer = fs.open("/payload", "r+", 0).await.unwrap();
    assert_eq!(read_all(&reader, BASELINE.len()).await, BASELINE);

    let error = writer.write(UPDATED, Some(0)).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Eio);
    let lost = blocks
        .lost
        .lock()
        .unwrap()
        .clone()
        .expect("second put was lost");
    // The orphaned block is durable in RustFS but must not be published.
    blocks.inner.get(&lost).await.unwrap();
    let (revision, namespace) = committed(url, &key).await;
    assert_eq!(
        revision, seed_revision,
        "failed write must not publish metadata"
    );
    assert_eq!(namespace, seed_namespace);
    assert!(!String::from_utf8_lossy(&namespace).contains(&lost.0));

    let before = cache.stats().await;
    assert_eq!(read_all(&reader, BASELINE.len()).await, BASELINE);
    let after = cache.stats().await;
    assert!(after.hits > before.hits);
    assert_eq!(after.backing_gets_completed, before.backing_gets_completed);

    let mut expected = BASELINE.to_vec();
    if retry {
        assert_eq!(writer.write(UPDATED, Some(0)).await.unwrap(), UPDATED.len());
        expected[..UPDATED.len()].copy_from_slice(UPDATED);
        let before = cache.stats().await;
        assert_eq!(read_all(&reader, expected.len()).await, expected);
        let after = cache.stats().await;
        assert!(after.hits > before.hits);
        assert_eq!(after.backing_gets_completed, before.backing_gets_completed);
        let (revision, _) = committed(url, &key).await;
        assert!(revision > seed_revision, "acknowledged retry must publish");
    }
    reader.close().await.unwrap();
    writer.close().await.unwrap();
    fs.shutdown().await.unwrap();
    let (final_revision, final_namespace) = committed(url, &key).await;

    // Reopen through a fresh store and an empty cache: authoritative bytes only.
    let metadata = TidbMetadataStore::connect_with_options(url, storage(&key))
        .await
        .unwrap();
    let cache = CachedBlockStore::new(blocks.clone(), 4096, 128);
    let fs = ChunkedFs::open(metadata, cache.clone(), chunked("reopen"))
        .await
        .unwrap();
    let file = fs.open("/payload", "r", 0).await.unwrap();
    assert_eq!(read_all(&file, expected.len()).await, expected);
    assert!(cache.stats().await.backing_gets_completed > 0);
    file.close().await.unwrap();
    fs.shutdown().await.unwrap();
    assert_eq!(committed(url, &key).await.1, final_namespace);
    println!(
        "TIDB_RUSTFS_LOST_ACK retry={retry} seed_revision={seed_revision} \
         final_revision={final_revision} lost_block_unpublished=pass warm_reader=pass reopen=pass"
    );
    cleanup(url, &key, &blocks).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit opt-in plus actual TiDB and RustFS services"]
async fn actual_tidb_rustfs_lost_block_ack_preserves_commit_and_retry() {
    assert_eq!(std::env::var(OPT_IN).as_deref(), Ok("1"), "set {OPT_IN}=1");
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("MOUNT_RS_TIDB_URL");
    let config = R2Config::from_env().expect("R2_* RustFS configuration");
    let endpoint = config.endpoint.trim_end_matches('/');
    assert!(
        endpoint.starts_with("http://127.0.0.1:") || endpoint.starts_with("http://localhost:"),
        "requires a loopback RustFS endpoint"
    );
    tokio::time::timeout(Duration::from_secs(300), async {
        for retry in [false, true] {
            exercise(&url, &config, retry).await;
        }
    })
    .await
    .expect("lost-ack test exceeded its bounded timeout");
}
