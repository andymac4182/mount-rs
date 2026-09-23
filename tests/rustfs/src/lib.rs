#![cfg(test)]
//! Real RustFS integration coverage for the S3-compatible block provider.
//!
//! These tests intentionally refuse non-loopback endpoints. The shell
//! harness owns an empty RustFS container and bucket, so this gate cannot be
//! turned into a mock or accidentally pointed at a live Cloudflare R2 account.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::ErrorCode;
use mount_rs_core::storage::{BlockId, BlockStore, MetadataStore};
use mount_rs_core::{Loopback, MkdirOptions};
use mount_rs_pglite::PgliteMetadataStore;
use mount_rs_rustfs::{RustFsBlockStore, RustFsConfig};
use mount_rs_sdk::{Filesystem, FilesystemKind, SplitOptions, StoreConfig};
use mount_rs_sqlite::SqliteMetadataStore;
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectStore, PutMode, PutOptions, PutPayload, UpdateVersion};

const TEST_TIMEOUT: Duration = Duration::from_secs(60);

fn local_config() -> RustFsConfig {
    let required = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} is required by the RustFS harness"))
    };
    let config = RustFsConfig {
        endpoint: required("RUSTFS_ENDPOINT"),
        bucket: required("RUSTFS_BUCKET"),
        access_key_id: required("RUSTFS_ACCESS_KEY_ID"),
        secret_access_key: required("RUSTFS_SECRET_ACCESS_KEY"),
        region: required("RUSTFS_REGION"),
    };
    config.validate().expect("valid RustFS test configuration");
    assert!(
        config.endpoint.starts_with("http://127.0.0.1:")
            || config.endpoint.starts_with("http://localhost:"),
        "RustFS integration tests only accept a loopback endpoint; refusing {}",
        config.endpoint
    );
    assert!(
        std::env::var("RUSTFS_TEST_PREFIX").is_ok(),
        "RUSTFS_TEST_PREFIX must identify this test run"
    );
    config
}

fn test_prefix() -> String {
    std::env::var("RUSTFS_TEST_PREFIX").expect("RUSTFS_TEST_PREFIX must be set")
}

fn fixture_path() -> PathBuf {
    std::env::var_os("RUSTFS_FIXTURE_FILE")
        .map(PathBuf::from)
        .expect("RUSTFS_FIXTURE_FILE must be set")
}

fn object_path(prefix: &str, name: &str) -> ObjectPath {
    ObjectPath::from(format!("{prefix}/{name}"))
}

fn block_id(value: &str) -> BlockId {
    BlockId(value.to_owned())
}

#[derive(Clone)]
struct TrackedRustFsBlocks {
    inner: RustFsBlockStore,
    created: Arc<Mutex<BTreeSet<String>>>,
}

impl TrackedRustFsBlocks {
    fn new(config: &RustFsConfig, prefix: String, created: Arc<Mutex<BTreeSet<String>>>) -> Self {
        Self {
            inner: RustFsBlockStore::from_config(config, prefix, true).unwrap(),
            created,
        }
    }

    async fn delete_created(&self) {
        let ids = self
            .created
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            self.inner.delete(&block_id(&id)).await.unwrap();
        }
    }
}

#[async_trait]
impl BlockStore for TrackedRustFsBlocks {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
        let id = self.inner.put(bytes).await?;
        self.created.lock().unwrap().insert(id.0.clone());
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

fn split_prefix(name: &str) -> String {
    format!("{}/split-{name}", test_prefix())
}

fn sqlite_metadata_file() -> PathBuf {
    std::env::var_os("RUSTFS_SQLITE_METADATA_FILE")
        .map(PathBuf::from)
        .expect("RUSTFS_SQLITE_METADATA_FILE must be set")
}

fn sdk_metadata_file() -> PathBuf {
    std::env::var_os("RUSTFS_RUN_DIR")
        .map(PathBuf::from)
        .expect("RUSTFS_RUN_DIR must be set")
        .join("sdk-sqlite-metadata.db")
}

fn pglite_url() -> String {
    std::env::var("PGLITE_DATABASE_URL").expect("PGLITE_DATABASE_URL must be set")
}

async fn write_split_file<M, B>(metadata: M, blocks: B, owner: &str) -> Vec<u8>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    let filesystem = ChunkedFs::open(
        metadata,
        blocks,
        ChunkedOptions::fixed(owner, 7).expect("valid test chunk size"),
    )
    .await
    .unwrap();
    let loopback = Loopback::new(filesystem.clone());
    loopback
        .mkdir("/split", MkdirOptions::default())
        .await
        .unwrap();
    let expected = (0..91).map(|index| (index * 37) as u8).collect::<Vec<_>>();
    let file = loopback.open("/split/file", "w+", 0o640).await.unwrap();
    file.write(&expected, Some(0)).await.unwrap();
    file.sync().await.unwrap();
    assert_eq!(loopback.read_file("/split/file").await.unwrap(), expected);
    file.close().await.unwrap();
    drop(loopback);
    filesystem.shutdown().await.unwrap();
    expected
}

async fn read_split_file<M, B>(metadata: M, blocks: B, owner: &str) -> Vec<u8>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    let filesystem = ChunkedFs::open(
        metadata,
        blocks,
        ChunkedOptions::fixed(owner, 4096).expect("valid reopen chunk size"),
    )
    .await
    .unwrap();
    let loopback = Loopback::new(filesystem.clone());
    let result = loopback.read_file("/split/file").await.unwrap();
    drop(loopback);
    filesystem.shutdown().await.unwrap();
    result
}

async fn assert_timeout<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, future)
        .await
        .expect("RustFS request/test exceeded its bounded timeout")
}

async fn assert_rustfs_preflight_rejects_before_sqlite_conversion(
    config: &RustFsConfig,
    case: &str,
) {
    let run_dir = std::env::var_os("RUSTFS_RUN_DIR")
        .map(PathBuf::from)
        .expect("RUSTFS_RUN_DIR must be set");
    let metadata_path = run_dir.join(format!("preflight-{case}.sqlite"));
    let prefix = format!("{}/preflight-{case}", test_prefix());
    let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
    let blocks = RustFsBlockStore::from_config(config, prefix, true).unwrap();
    let result = ChunkedFs::open(
        metadata,
        blocks,
        ChunkedOptions::fixed(format!("preflight-{case}"), 4096)
            .unwrap()
            .with_concurrent_writes(true),
    )
    .await;
    let error = match result {
        Ok(filesystem) => {
            filesystem.shutdown().await.unwrap();
            panic!("{case} RustFS backing unexpectedly converted concurrent SQLite metadata");
        }
        Err(error) => error,
    };
    assert!(
        !error.is(ErrorCode::Enotsup),
        "{case} must fail from the signed RustFS write/read rather than a generic opt-in guard: {error}"
    );

    let reopened = SqliteMetadataStore::open(&metadata_path).unwrap();
    let state = reopened.load().await.unwrap();
    assert_eq!(state.revision, 0, "{case} must not publish metadata");
    assert!(
        state.namespace.is_none(),
        "{case} must leave namespace empty"
    );
    let legacy = reopened
        .acquire_writer(&format!("preflight-legacy-{case}"), Duration::from_secs(30))
        .await
        .expect("failed preflight must leave legacy single-writer mode available");
    reopened.release_writer(&legacy).await.unwrap();
}

#[tokio::test]
async fn real_rustfs_signed_concurrent_preflight_fails_closed_before_mode_conversion() {
    assert_timeout(async {
        let valid = local_config();
        let prefix = format!("{}/preflight-valid", test_prefix());
        let first = RustFsBlockStore::from_config(&valid, prefix.clone(), true).unwrap();
        let second = RustFsBlockStore::from_config(&valid, prefix.clone(), true).unwrap();
        first.prepare_concurrent_mode().await.unwrap();
        second.prepare_concurrent_mode().await.unwrap();
        valid
            .build_store()
            .unwrap()
            .head(&object_path(&prefix, "_mount-rs-concurrent-probe-v1"))
            .await
            .expect("signed preflight must leave a reusable, remotely visible probe");

        let mut wrong_credentials = valid.clone();
        wrong_credentials.secret_access_key.push_str("-wrong");
        assert_rustfs_preflight_rejects_before_sqlite_conversion(
            &wrong_credentials,
            "wrong-credentials",
        )
        .await;

        let mut missing_bucket = valid.clone();
        missing_bucket.bucket = "mount-rs-missing-rustfs-bucket".to_owned();
        assert_rustfs_preflight_rejects_before_sqlite_conversion(&missing_bucket, "missing-bucket")
            .await;

        let colliding_prefix = format!("{}/preflight-collision", test_prefix());
        let colliding_path = object_path(&colliding_prefix, "_mount-rs-concurrent-probe-v1");
        let service = valid.build_store().unwrap();
        service
            .put(
                &colliding_path,
                PutPayload::from(b"foreign probe must remain".to_vec()),
            )
            .await
            .unwrap();
        assert_rustfs_preflight_rejects_before_sqlite_conversion(&valid, "collision").await;
        assert_eq!(
            service
                .get(&colliding_path)
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"foreign probe must remain",
            "failed preflight must not replace an existing object"
        );
        println!("RUSTFS_SIGNED_CONCURRENT_PREFLIGHT_PASS");
    })
    .await;
}

#[tokio::test]
async fn real_rustfs_block_contract() {
    assert_timeout(async {
        let config = local_config();
        let prefix = test_prefix();
        let blocks = RustFsBlockStore::from_config(&config, prefix.clone(), true).unwrap();
        let object_store = config.build_store().unwrap();

        let payload = (0_u32..32_768)
            .map(|index| (index.wrapping_mul(37) & 0xff) as u8)
            .collect::<Vec<_>>();
        let first_id = blocks.put(&payload).await.unwrap();
        let second_id = blocks.put(&payload).await.unwrap();
        assert_eq!(
            first_id, second_id,
            "content-addressed immutable publication should reuse an ID"
        );
        assert_eq!(blocks.get(&first_id).await.unwrap(), payload);
        blocks.flush().await.unwrap();

        let first_path = object_path(&prefix, &first_id.0);
        assert_eq!(
            object_store
                .get_range(&first_path, 7..23)
                .await
                .unwrap()
                .as_ref(),
            &payload[7..23]
        );

        let missing = block_id("b00000000000000000000000000000000");
        assert!(
            blocks
                .get(&missing)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent),
            "missing RustFS blocks must map to ENOENT"
        );

        let conditional_name = "conditional-object";
        let conditional_path = object_path(&prefix, conditional_name);
        let conditional_body = b"first conditional value";
        let created = object_store
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
        let duplicate = object_store
            .put_opts(
                &conditional_path,
                PutPayload::from(b"must-not-overwrite".to_vec()),
                PutOptions {
                    mode: PutMode::Create,
                    ..Default::default()
                },
            )
            .await
            .expect_err("RustFS must enforce create-only publication");
        assert!(
            matches!(
                duplicate,
                object_store::Error::AlreadyExists { .. }
                    | object_store::Error::Precondition { .. }
            ),
            "unexpected RustFS conditional-create error: {duplicate}"
        );

        let metadata = object_store.head(&conditional_path).await.unwrap();
        let wrong_read = object_store
            .get_opts(
                &conditional_path,
                GetOptions {
                    if_match: Some("\"not-the-current-etag\"".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("RustFS must reject a stale conditional read");
        assert!(
            matches!(wrong_read, object_store::Error::Precondition { .. }),
            "unexpected RustFS stale-read error: {wrong_read}"
        );

        let current_version = UpdateVersion {
            e_tag: metadata.e_tag.clone(),
            version: metadata.version.clone(),
        };
        let updated = object_store
            .put_opts(
                &conditional_path,
                PutPayload::from(b"second conditional value".to_vec()),
                PutOptions {
                    mode: PutMode::Update(current_version),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_ne!(created.e_tag, updated.e_tag);
        assert_eq!(
            object_store
                .get(&conditional_path)
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"second conditional value"
        );

        let shared = Arc::new(blocks.clone());
        let mut workers = Vec::new();
        for worker in 0..8_u8 {
            let blocks = Arc::clone(&shared);
            workers.push(tokio::spawn(async move {
                let body = vec![worker; 4096];
                let id = blocks.put(&body).await.unwrap();
                (id, body)
            }));
        }
        let mut concurrent_ids = Vec::new();
        for worker in workers {
            let (id, body) = worker.await.unwrap();
            assert_eq!(shared.get(&id).await.unwrap(), body);
            concurrent_ids.push(id);
        }

        let restart_body = b"survives a fresh RustFS client after service restart".to_vec();
        let restart_id = blocks.put(&restart_body).await.unwrap();
        let fixture = fixture_path();
        std::fs::write(
            &fixture,
            format!("{}\n{}\n", restart_id.0, hex(&restart_body)),
        )
        .unwrap();

        for id in concurrent_ids.into_iter().chain([first_id]) {
            blocks.delete(&id).await.unwrap();
        }
        object_store.delete(&conditional_path).await.unwrap();
        println!(
            "RUSTFS_BLOCK_CONTRACT_PASS prefix={} committed_object={} restart_fixture={}",
            prefix,
            restart_id.0,
            fixture.display()
        );
    })
    .await;
}

#[tokio::test]
async fn real_rustfs_reopen_after_service_restart() {
    assert_timeout(async {
        let config = local_config();
        let prefix = test_prefix();
        let fixture = fixture_path();
        let lines = std::fs::read_to_string(&fixture).expect("restart fixture must exist");
        let mut lines = lines.lines();
        let id = block_id(lines.next().expect("restart fixture block ID"));
        let expected = decode_hex(lines.next().expect("restart fixture payload"));
        let blocks = RustFsBlockStore::from_config(&config, prefix.clone(), true).unwrap();
        assert_eq!(blocks.get(&id).await.unwrap(), expected);
        blocks.delete(&id).await.unwrap();
        std::fs::remove_file(&fixture).unwrap();
        println!(
            "RUSTFS_RESTART_REOPEN_PASS prefix={} block={}",
            prefix, id.0
        );
    })
    .await;
}

#[tokio::test]
async fn real_rustfs_sqlite_metadata_round_trip() {
    assert_timeout(async {
        let config = local_config();
        let prefix = split_prefix("sqlite-metadata");
        let created = Arc::new(Mutex::new(BTreeSet::new()));
        let first_blocks = TrackedRustFsBlocks::new(&config, prefix.clone(), Arc::clone(&created));
        let expected = write_split_file(
            SqliteMetadataStore::open(sqlite_metadata_file()).unwrap(),
            first_blocks,
            "rustfs-split-sqlite-first",
        )
        .await;
        let reopened_blocks = TrackedRustFsBlocks::new(&config, prefix, created.clone());
        let actual = read_split_file(
            SqliteMetadataStore::open(sqlite_metadata_file()).unwrap(),
            reopened_blocks.clone(),
            "rustfs-split-sqlite-reopen",
        )
        .await;
        assert_eq!(actual, expected);
        reopened_blocks.delete_created().await;
        println!("RUSTFS_SPLIT_SQLITE_METADATA_PASS");
    })
    .await;
}

#[tokio::test]
async fn real_rustfs_sdk_sqlite_metadata_round_trip() {
    assert_timeout(async {
        let config = local_config();
        let mut options = SplitOptions::memory("rustfs-sdk-first", 7);
        options.metadata = StoreConfig::Sqlite {
            path: sdk_metadata_file(),
        };
        options.blocks = StoreConfig::RustFs {
            endpoint: config.endpoint,
            bucket: config.bucket,
            region: config.region,
            prefix: format!("{}/sdk-sqlite-blocks", test_prefix()),
            access_key_id: config.access_key_id,
            secret_access_key: config.secret_access_key,
            durable: true,
        };

        let first = Filesystem::split(options.clone()).await.unwrap();
        assert_eq!(first.kind(), FilesystemKind::SplitStore);
        let first_view = Loopback::from_arc(first.driver());
        first_view
            .write_file("/sdk-rustfs.txt", b"public Rust SDK over real RustFS")
            .await
            .unwrap();
        drop(first_view);
        first.shutdown().await.unwrap();
        drop(first);

        options.owner = "rustfs-sdk-reopen".to_owned();
        let reopened = Filesystem::split(options).await.unwrap();
        let reopened_view = Loopback::from_arc(reopened.driver());
        assert_eq!(
            reopened_view.read_file("/sdk-rustfs.txt").await.unwrap(),
            b"public Rust SDK over real RustFS"
        );
        drop(reopened_view);
        reopened.shutdown().await.unwrap();
        println!("RUSTFS_SDK_SQLITE_METADATA_PASS");
    })
    .await;
}

#[tokio::test]
async fn real_rustfs_pglite_metadata_round_trip() {
    assert_timeout(async {
        let config = local_config();
        let prefix = split_prefix("pglite-metadata");
        let created = Arc::new(Mutex::new(BTreeSet::new()));
        let blocks = TrackedRustFsBlocks::new(&config, prefix.clone(), Arc::clone(&created));
        let key = format!("{}/pglite-metadata", test_prefix());
        let first_metadata = PgliteMetadataStore::connect_with_key(&pglite_url(), key.clone())
            .await
            .unwrap();
        let expected = write_split_file(
            first_metadata.clone(),
            blocks.clone(),
            "rustfs-split-pglite-first",
        )
        .await;
        first_metadata.close().await.unwrap();

        let reopened_metadata = PgliteMetadataStore::connect_with_key(&pglite_url(), key)
            .await
            .unwrap();
        let reopened_blocks = TrackedRustFsBlocks::new(&config, prefix, created.clone());
        let actual = read_split_file(
            reopened_metadata.clone(),
            reopened_blocks.clone(),
            "rustfs-split-pglite-reopen",
        )
        .await;
        reopened_metadata.close().await.unwrap();
        assert_eq!(actual, expected);
        reopened_blocks.delete_created().await;
        println!("RUSTFS_SPLIT_PGLITE_METADATA_PASS");
    })
    .await;
}

#[tokio::test]
async fn real_rustfs_block_benchmark() {
    assert_timeout(async {
        const BLOCK_COUNT: usize = 8;
        const BLOCK_BYTES: usize = 16 * 1024;

        let config = local_config();
        let prefix = format!("{}/benchmark", test_prefix());
        let created = Arc::new(Mutex::new(BTreeSet::new()));
        let blocks = TrackedRustFsBlocks::new(&config, prefix, Arc::clone(&created));
        let payload = (0..BLOCK_BYTES)
            .map(|index| (index as u8).wrapping_mul(31))
            .collect::<Vec<_>>();

        let started = Instant::now();
        let mut writes = Vec::with_capacity(BLOCK_COUNT);
        for _ in 0..BLOCK_COUNT {
            let blocks = blocks.clone();
            let payload = payload.clone();
            writes.push(tokio::spawn(async move {
                blocks.put(&payload).await
            }));
        }
        let mut ids = Vec::with_capacity(BLOCK_COUNT);
        for write in writes {
            ids.push(write.await.unwrap().unwrap());
        }
        blocks.flush().await.unwrap();
        let write_ms = started.elapsed().as_millis();

        let read_started = Instant::now();
        for id in &ids {
            assert_eq!(blocks.get(id).await.unwrap(), payload);
        }
        let read_ms = read_started.elapsed().as_millis();
        blocks.delete_created().await;

        println!(
            "RUSTFS_BLOCK_BENCHMARK_PASS blocks={} block_bytes={} write_ms={} read_ms={} total_ms={}",
            BLOCK_COUNT,
            BLOCK_BYTES,
            write_ms,
            read_ms,
            started.elapsed().as_millis()
        );
    })
    .await;
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2), "fixture payload is not hex");
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16).expect("fixture hex digit");
            let low = char::from(pair[1]).to_digit(16).expect("fixture hex digit");
            ((high << 4) | low) as u8
        })
        .collect()
}
