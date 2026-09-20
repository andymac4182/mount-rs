//! Opt-in end-to-end coverage for TiDB metadata plus RustFS-backed chunks.
//!
//! The RustFS service is deliberately owned by the caller/main harness. This
//! test consumes only its loopback R2_* environment and writes below a unique
//! prefix. It never falls back to MySQL, an in-memory object store, or a mock.

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{BlockId, BlockStore, MetadataStore};
use mount_rs_core::{ErrorCode, Loopback, MkdirOptions};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_tidb::{TidbMetadataStore, TidbStorageOptions};
use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TEST_TIMEOUT: Duration = Duration::from_secs(180);
const OPT_IN: &str = "MOUNT_RS_TIDB_CHUNKED_RUSTFS";
const VOLUME_KEY: &str = "MOUNT_RS_TIDB_CHUNKED_VOLUME_KEY";
const RUSTFS_PREFIX: &str = "MOUNT_RS_TIDB_RUSTFS_PREFIX";
const FIXTURE: &str = "MOUNT_RS_TIDB_CHUNKED_RUSTFS_FIXTURE";
const EXPECT_PERSISTED: &str = "MOUNT_RS_TIDB_EXPECT_PERSISTED";
const REOPEN: &str = "MOUNT_RS_TIDB_CHUNKED_RUSTFS_REOPEN";
const FIXTURE_FORMAT: &str = "mount-rs-tidb-chunked-rustfs-v1";

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn required_env(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("{name} must be set for the opt-in TiDB/RustFS test"))
}

fn persisted_run() -> bool {
    std::env::var(EXPECT_PERSISTED).as_deref() == Ok("1")
}

fn reopen_after_seed() -> bool {
    std::env::var(REOPEN).as_deref() == Ok("1")
}

fn configured_volume_key(prefix: &str, persisted: bool) -> String {
    if persisted {
        return required_env(VOLUME_KEY);
    }
    optional_env(VOLUME_KEY).unwrap_or_else(|| format!("{prefix}/tidb-metadata"))
}

fn configured_prefix(persisted: bool) -> String {
    if persisted {
        return required_env(RUSTFS_PREFIX);
    }
    optional_env(RUSTFS_PREFIX)
        .or_else(|| optional_env("RUSTFS_COMBO_PREFIX"))
        .unwrap_or_else(|| {
            panic!(
                "{RUSTFS_PREFIX} or RUSTFS_COMBO_PREFIX must be set for the opt-in TiDB/RustFS test"
            )
        })
}

fn configured_fixture(persisted: bool) -> PathBuf {
    if let Some(path) = optional_env(FIXTURE) {
        return PathBuf::from(path);
    }
    if persisted {
        panic!("{FIXTURE} must be set to a durable path for a persisted TiDB/RustFS restart run");
    }
    let run_dir = optional_env("RUSTFS_RUN_DIR").unwrap_or_else(|| {
        panic!("{FIXTURE} or RUSTFS_RUN_DIR must be set for the opt-in TiDB/RustFS test")
    });
    PathBuf::from(run_dir).join("tidb-chunked-rustfs-blocks")
}

fn assert_fixture_path_safe(path: &Path) {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        assert!(
            !metadata.file_type().is_symlink(),
            "RustFS cleanup fixture must not be a symlink: {}",
            path.display()
        );
    }
}

fn options(volume_key: &str) -> TidbStorageOptions {
    TidbStorageOptions::new(volume_key)
        .with_durable(true)
        .with_max_block_bytes(4 * 1024 * 1024)
        .with_max_namespace_bytes(4 * 1024 * 1024)
}

fn expected_bytes() -> Vec<u8> {
    (0..97)
        .map(|index| ((index * 73 + 11) & 0xff) as u8)
        .collect()
}

fn expected_after_writes() -> Vec<u8> {
    let mut expected = expected_bytes();
    let first_patch = [0x00, 0xff, 0x01, 0xfe, 0x42, 0x00, 0xc7, 0x17, 0xa5];
    expected[5..5 + first_patch.len()].copy_from_slice(&first_patch);
    let second_patch = (0..15)
        .map(|index| (0x80_u8).wrapping_add(index * 9))
        .collect::<Vec<_>>();
    expected[49..49 + second_patch.len()].copy_from_slice(&second_patch);
    expected.truncate(63);
    let final_patch = [0xde, 0xad, 0x00, 0xbe, 0xef];
    expected[2..2 + final_patch.len()].copy_from_slice(&final_patch);
    expected
}

fn local_rustfs_config() -> R2Config {
    let config = R2Config::from_env()
        .unwrap_or_else(|_| panic!("R2_ENDPOINT, R2_BUCKET, and R2 credentials are required"));
    let endpoint = config.endpoint.trim_end_matches('/');
    assert!(
        endpoint.starts_with("http://127.0.0.1:") || endpoint.starts_with("http://localhost:"),
        "RustFS integration requires a loopback HTTP endpoint"
    );
    config
}

async fn assert_actual_tidb(url: &str) {
    let pool =
        Pool::from_url(url).unwrap_or_else(|_| panic!("the TiDB identity URL could not be parsed"));
    let mut connection = pool
        .get_conn()
        .await
        .unwrap_or_else(|_| panic!("could not connect for the TiDB identity check"));
    let identity: Option<(String, String)> = connection
        .exec_first("SELECT tidb_version(), VERSION()", ())
        .await
        .unwrap_or_else(|_| panic!("SELECT tidb_version(), VERSION() failed"));
    let (tidb_version, version) =
        identity.unwrap_or_else(|| panic!("TiDB identity query returned no row"));
    let identity_text = format!("{tidb_version} {version}").to_ascii_lowercase();
    assert!(
        identity_text.contains("tidb"),
        "server identity did not identify TiDB: tidb_version()={tidb_version:?}, VERSION()={version:?}"
    );
    println!("TIDB_IDENTITY tidb_version={tidb_version} version={version}");
    drop(connection);
    pool.disconnect()
        .await
        .unwrap_or_else(|_| panic!("could not close the TiDB identity connection"));
}

#[derive(Clone)]
struct TrackedRustFsBlocks {
    inner: R2BlockStore,
    created: Arc<Mutex<BTreeSet<String>>>,
}

impl TrackedRustFsBlocks {
    fn new(config: &R2Config, prefix: String) -> Self {
        Self {
            inner: R2BlockStore::from_config(config, prefix)
                .unwrap_or_else(|_| panic!("could not configure the RustFS block store")),
            created: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    fn created_ids(&self) -> Vec<BlockId> {
        self.created
            .lock()
            .expect("RustFS block tracker lock")
            .iter()
            .cloned()
            .map(BlockId)
            .collect()
    }

    async fn delete_ids(&self, ids: impl IntoIterator<Item = BlockId>) {
        for id in ids {
            match self.inner.delete(&id).await {
                Ok(()) => {}
                Err(error) if error.is(ErrorCode::Enoent) => {}
                Err(_) => panic!("could not clean up a RustFS test block"),
            }
        }
    }

    async fn assert_ids_absent(&self, ids: &[BlockId]) {
        for id in ids {
            let error = self
                .inner
                .get(id)
                .await
                .expect_err("deleted RustFS block must not remain readable");
            assert!(
                error.is(ErrorCode::Enoent),
                "deleted RustFS block returned the wrong error: {error}"
            );
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
        self.created
            .lock()
            .expect("RustFS block tracker lock")
            .insert(id.0.clone());
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

fn write_fixture(path: &Path, prefix: &str, volume_key: &str, ids: &[BlockId]) {
    assert_fixture_path_safe(path);
    for (name, value) in [("RustFS prefix", prefix), ("TiDB volume key", volume_key)] {
        assert!(
            !value.is_empty() && !value.contains(['\n', '\r']),
            "{name} cannot be empty or contain a newline in the restart fixture"
        );
    }
    assert!(
        !ids.is_empty(),
        "RustFS restart fixture must contain blocks"
    );
    let body = ids
        .iter()
        .map(|id| format!("block:{}", id.0))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        path,
        format!("{FIXTURE_FORMAT}\n{prefix}\n{volume_key}\n{body}\n"),
    )
    .expect("write RustFS cleanup fixture");
}

fn read_fixture(path: &Path, expected_prefix: &str, expected_volume_key: &str) -> Vec<BlockId> {
    assert_fixture_path_safe(path);
    let contents = std::fs::read_to_string(path)
        .expect("RustFS cleanup fixture must survive the TiDB restart");
    let mut lines = contents.lines();
    assert_eq!(
        lines.next(),
        Some(FIXTURE_FORMAT),
        "RustFS cleanup fixture format mismatch"
    );
    assert_eq!(
        lines.next(),
        Some(expected_prefix),
        "RustFS cleanup fixture prefix does not match this run"
    );
    assert_eq!(
        lines.next(),
        Some(expected_volume_key),
        "RustFS cleanup fixture TiDB volume key does not match this run"
    );
    let ids = lines
        .map(|line| {
            let id = line
                .strip_prefix("block:")
                .expect("RustFS cleanup fixture contains an invalid block record");
            assert!(
                !id.is_empty(),
                "RustFS cleanup fixture contains an empty block id"
            );
            BlockId(id.to_owned())
        })
        .collect::<Vec<_>>();
    assert!(
        !ids.is_empty(),
        "RustFS cleanup fixture must contain blocks"
    );
    ids
}

async fn delete_metadata_row(url: &str, volume_key: &str) {
    let pool = Pool::from_url(url).unwrap_or_else(|_| panic!("could not parse TiDB cleanup URL"));
    let mut connection = pool
        .get_conn()
        .await
        .unwrap_or_else(|_| panic!("could not connect for TiDB cleanup"));
    connection
        .exec_drop(
            "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (volume_key,),
        )
        .await
        .unwrap_or_else(|_| panic!("could not clean up the scoped TiDB metadata row"));
    drop(connection);
    pool.disconnect()
        .await
        .unwrap_or_else(|_| panic!("could not close the TiDB cleanup connection"));
}

async fn assert_metadata_row_absent(url: &str, volume_key: &str) {
    let pool = Pool::from_url(url)
        .unwrap_or_else(|_| panic!("could not parse TiDB cleanup verification URL"));
    let mut connection = pool
        .get_conn()
        .await
        .unwrap_or_else(|_| panic!("could not connect for TiDB cleanup verification"));
    let count: Option<u64> = connection
        .exec_first(
            "SELECT COUNT(*) FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (volume_key,),
        )
        .await
        .unwrap_or_else(|_| panic!("could not verify TiDB metadata cleanup"));
    assert_eq!(count, Some(0), "scoped TiDB metadata row still exists");
    drop(connection);
    pool.disconnect()
        .await
        .unwrap_or_else(|_| panic!("could not close the TiDB cleanup verification connection"));
}

async fn seed_chunked_filesystem(
    url: &str,
    volume_key: &str,
    config: &R2Config,
    prefix: &str,
    fixture: &Path,
) {
    assert_actual_tidb(url).await;
    let metadata = TidbMetadataStore::connect_with_options(url, options(volume_key))
        .await
        .unwrap_or_else(|_| panic!("could not connect TiDB metadata store"));
    let blocks = TrackedRustFsBlocks::new(config, prefix.to_owned());
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("tidb-rustfs-seed", 7)
            .unwrap()
            .with_lease_ttl(Duration::from_secs(30)),
    )
    .await
    .unwrap_or_else(|_| panic!("could not open the TiDB/RustFS ChunkedFs"));
    let loopback = Loopback::new(filesystem.clone());
    loopback
        .mkdir("/chunks", MkdirOptions::default())
        .await
        .unwrap_or_else(|_| panic!("could not create the chunked test directory"));

    let mut expected = expected_bytes();
    let file = loopback
        .open("/chunks/binary", "w+", 0o640)
        .await
        .unwrap_or_else(|_| panic!("could not open the chunked binary file"));
    assert_eq!(
        file.write(&expected, Some(0)).await.unwrap(),
        expected.len()
    );

    let first_patch = [0x00, 0xff, 0x01, 0xfe, 0x42, 0x00, 0xc7, 0x17, 0xa5];
    assert_eq!(
        file.write(&first_patch, Some(5)).await.unwrap(),
        first_patch.len()
    );
    expected[5..5 + first_patch.len()].copy_from_slice(&first_patch);

    let second_patch = (0..15)
        .map(|index| (0x80_u8).wrapping_add(index * 9))
        .collect::<Vec<_>>();
    assert_eq!(
        file.write(&second_patch, Some(49)).await.unwrap(),
        second_patch.len()
    );
    expected[49..49 + second_patch.len()].copy_from_slice(&second_patch);

    file.truncate(63)
        .await
        .unwrap_or_else(|_| panic!("could not truncate the chunked binary file"));
    expected.truncate(63);

    let final_patch = [0xde, 0xad, 0x00, 0xbe, 0xef];
    assert_eq!(
        file.write(&final_patch, Some(2)).await.unwrap(),
        final_patch.len()
    );
    expected[2..2 + final_patch.len()].copy_from_slice(&final_patch);
    assert_eq!(file.stat().await.unwrap().size, expected.len() as u64);
    file.sync()
        .await
        .unwrap_or_else(|_| panic!("could not sync the chunked binary file"));
    file.close()
        .await
        .unwrap_or_else(|_| panic!("could not close the chunked binary file"));
    loopback
        .syncfs()
        .await
        .unwrap_or_else(|_| panic!("could not sync the TiDB/RustFS filesystem"));
    assert_eq!(
        loopback.read_file("/chunks/binary").await.unwrap(),
        expected
    );
    drop(loopback);
    filesystem
        .shutdown()
        .await
        .unwrap_or_else(|_| panic!("could not release the seed writer lease"));

    let snapshot = metadata
        .load()
        .await
        .unwrap_or_else(|_| panic!("could not load the seeded TiDB namespace"));
    assert!(snapshot.revision > 0);
    metadata
        .close()
        .await
        .unwrap_or_else(|_| panic!("could not close the seed TiDB metadata pool"));

    let cas_a = TidbMetadataStore::connect_with_options(url, options(volume_key))
        .await
        .unwrap_or_else(|_| panic!("could not connect the first CAS metadata client"));
    let cas_b = TidbMetadataStore::connect_with_options(url, options(volume_key))
        .await
        .unwrap_or_else(|_| panic!("could not connect the second CAS metadata client"));
    let first = cas_a
        .acquire_writer("tidb-rustfs-cas-first", Duration::from_secs(30))
        .await
        .unwrap_or_else(|_| panic!("could not acquire the first CAS writer"));
    let busy = cas_b
        .acquire_writer("tidb-rustfs-cas-busy", Duration::from_secs(2))
        .await
        .expect_err("TiDB must fence a competing ChunkedFs writer");
    assert!(busy.is(ErrorCode::Eagain));
    cas_a
        .release_writer(&first)
        .await
        .unwrap_or_else(|_| panic!("could not release the first CAS writer"));

    let expiring = cas_a
        .acquire_writer("tidb-rustfs-cas-expiring", Duration::from_millis(250))
        .await
        .unwrap_or_else(|_| panic!("could not acquire the expiring CAS writer"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let replacement = cas_b
        .acquire_writer("tidb-rustfs-cas-replacement", Duration::from_secs(5))
        .await
        .unwrap_or_else(|_| panic!("could not replace the expired CAS writer"));
    assert!(replacement.fence > expiring.fence);
    let stale = cas_a
        .publish(
            snapshot.revision,
            &expiring,
            snapshot.namespace.clone().expect("seed namespace"),
        )
        .await
        .expect_err("expired TiDB writer must not publish ChunkedFs metadata");
    assert!(stale.is(ErrorCode::Estale));

    let current = cas_b
        .load()
        .await
        .unwrap_or_else(|_| panic!("could not load the current CAS namespace"));
    let namespace = current.namespace.clone().expect("current namespace");
    let published = cas_b
        .publish(current.revision, &replacement, namespace.clone())
        .await
        .unwrap_or_else(|_| panic!("could not publish the current CAS namespace"));
    assert_eq!(published, current.revision + 1);
    let conflict = cas_b
        .publish(current.revision, &replacement, namespace)
        .await
        .expect_err("TiDB revision CAS must reject a stale expected revision");
    assert!(conflict.is(ErrorCode::Eagain));
    cas_b
        .release_writer(&replacement)
        .await
        .unwrap_or_else(|_| panic!("could not release the replacement CAS writer"));
    cas_a
        .close()
        .await
        .unwrap_or_else(|_| panic!("could not close the first CAS metadata pool"));
    cas_b
        .close()
        .await
        .unwrap_or_else(|_| panic!("could not close the second CAS metadata pool"));

    write_fixture(fixture, prefix, volume_key, &blocks.created_ids());
    println!("TIDB_CHUNKED_RUSTFS_SEED_PASS");
}

async fn reopen_chunked_filesystem(
    url: &str,
    volume_key: &str,
    config: &R2Config,
    prefix: &str,
    fixture: &Path,
) {
    assert_actual_tidb(url).await;
    let metadata = TidbMetadataStore::connect_with_options(url, options(volume_key))
        .await
        .unwrap_or_else(|_| panic!("could not reconnect TiDB metadata store"));
    let blocks = TrackedRustFsBlocks::new(config, prefix.to_owned());
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("tidb-rustfs-reopen", 4096)
            .unwrap()
            .with_lease_ttl(Duration::from_secs(30)),
    )
    .await
    .unwrap_or_else(|_| panic!("could not reopen the TiDB/RustFS ChunkedFs"));
    let loopback = Loopback::new(filesystem.clone());
    let expected = expected_after_writes();
    let actual = loopback
        .read_file("/chunks/binary")
        .await
        .unwrap_or_else(|_| panic!("could not read the reopened RustFS chunks"));
    assert_eq!(actual, expected);
    assert_eq!(loopback.stat("/chunks/binary").await.unwrap().size, 63);
    loopback
        .syncfs()
        .await
        .unwrap_or_else(|_| panic!("could not sync the reopened TiDB/RustFS filesystem"));
    drop(loopback);
    filesystem
        .shutdown()
        .await
        .unwrap_or_else(|_| panic!("could not release the reopened writer lease"));
    metadata
        .close()
        .await
        .unwrap_or_else(|_| panic!("could not close the reopened TiDB metadata pool"));

    let ids = read_fixture(fixture, prefix, volume_key);
    blocks.delete_ids(ids.iter().cloned()).await;
    blocks.assert_ids_absent(&ids).await;
    delete_metadata_row(url, volume_key).await;
    assert_metadata_row_absent(url, volume_key).await;
    std::fs::remove_file(fixture).expect("remove RustFS cleanup fixture");
    assert_fixture_path_safe(fixture);
    assert!(
        !fixture.exists(),
        "RustFS cleanup fixture still exists after successful cleanup"
    );
    println!("TIDB_CHUNKED_RUSTFS_REOPEN_PASS");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit opt-in plus actual TiDB and RustFS services"]
async fn actual_tidb_chunked_rustfs_contract() {
    assert_eq!(
        std::env::var(OPT_IN).as_deref(),
        Ok("1"),
        "set MOUNT_RS_TIDB_CHUNKED_RUSTFS=1 to explicitly run this ignored test"
    );
    let url = required_env("MOUNT_RS_TIDB_URL");
    let persisted = persisted_run();
    let prefix = configured_prefix(persisted);
    let volume_key = configured_volume_key(&prefix, persisted);
    let fixture = configured_fixture(persisted);
    if persisted && let Some(run_dir) = optional_env("RUSTFS_RUN_DIR") {
        assert!(
            !fixture.starts_with(Path::new(&run_dir)),
            "persisted restart fixture must not live under transient RUSTFS_RUN_DIR"
        );
    }
    let config = local_rustfs_config();

    tokio::time::timeout(TEST_TIMEOUT, async {
        if persisted {
            reopen_chunked_filesystem(&url, &volume_key, &config, &prefix, &fixture).await;
        } else {
            seed_chunked_filesystem(&url, &volume_key, &config, &prefix, &fixture).await;
            if reopen_after_seed() {
                reopen_chunked_filesystem(&url, &volume_key, &config, &prefix, &fixture).await;
            }
        }
    })
    .await
    .expect("TiDB/RustFS ChunkedFs test exceeded its bounded timeout");
}
