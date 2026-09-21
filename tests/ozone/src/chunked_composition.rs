//! Explicit Ozone S3 compositions with independent metadata providers.
//!
//! These tests are ignored by default and are run only by the composition
//! harness. The harness owns a real Apache Ozone S3 Gateway, a real
//! disk-backed PGlite socket server, and a throwaway SQLite metadata file.
//! The tests never substitute an in-memory block store or a fake metadata
//! service.

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{BlockId, BlockStore, LoadedMetadata, MetadataStore};
use mount_rs_core::{ErrorCode, FsDriver, Loopback, MkdirOptions};
use mount_rs_pglite::{PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_sqlite::SqliteMetadataStore;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_postgres::NoTls;

const TEST_TIMEOUT: Duration = Duration::from_secs(180);
const CHUNK_SIZE: usize = 7;
const SQLITE_OPT_IN: &str = "MOUNT_RS_OZONE_CHUNKED_SQLITE";
const PGLITE_OPT_IN: &str = "MOUNT_RS_OZONE_CHUNKED_PGLITE";

#[derive(Clone)]
struct TrackedOzoneBlocks {
    inner: R2BlockStore,
    created: Arc<Mutex<BTreeSet<String>>>,
}

impl TrackedOzoneBlocks {
    fn new(config: &R2Config, prefix: String) -> Self {
        Self {
            inner: R2BlockStore::from_config(config, prefix)
                .expect("could not configure the Ozone block store"),
            created: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    async fn delete_created(&self) {
        let ids = self
            .created
            .lock()
            .expect("Ozone block tracker lock")
            .iter()
            .cloned()
            .map(BlockId)
            .collect::<Vec<_>>();
        for id in ids {
            match self.inner.delete(&id).await {
                Ok(()) => {}
                Err(error) if error.is(ErrorCode::Enoent) => {}
                Err(error) => panic!("could not clean up Ozone block {}: {error}", id.0),
            }
        }
    }
}

#[async_trait]
impl BlockStore for TrackedOzoneBlocks {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
        let id = self.inner.put(bytes).await?;
        self.created
            .lock()
            .expect("Ozone block tracker lock")
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

fn required_env(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("{name} must be set for the explicit Ozone composition gate"))
}

fn local_config() -> R2Config {
    let config = R2Config::from_env().expect("Ozone R2-compatible test environment is required");
    assert!(
        config.endpoint.starts_with("http://127.0.0.1:")
            || config.endpoint.starts_with("http://localhost:"),
        "Ozone composition tests only accept a loopback endpoint; refusing {}",
        config.endpoint
    );
    assert!(
        std::env::var("OZONE_TEST_PREFIX").is_ok(),
        "OZONE_TEST_PREFIX must identify this test run"
    );
    config
}

fn test_prefix() -> String {
    required_env("OZONE_TEST_PREFIX")
}

fn sqlite_metadata_path() -> PathBuf {
    PathBuf::from(required_env("OZONE_SQLITE_METADATA_FILE"))
}

fn pglite_url() -> String {
    let url = required_env("PGLITE_DATABASE_URL");
    assert!(
        url.contains("127.0.0.1:") || url.contains("localhost:"),
        "Ozone PGlite composition requires a loopback PostgreSQL-wire endpoint"
    );
    assert!(
        std::env::var("PGLITE_DATA_DIR").is_ok(),
        "PGLITE_DATA_DIR must identify the harness-owned persistent PGlite data directory"
    );
    url
}

fn expected_bytes() -> Vec<u8> {
    (0..97)
        .map(|index| ((index * 73 + 11) & 0xff) as u8)
        .collect()
}

async fn seed_chunked_filesystem<M>(
    metadata: M,
    blocks: TrackedOzoneBlocks,
    owner: &str,
) -> (Vec<u8>, LoadedMetadata)
where
    M: MetadataStore + Clone + 'static,
{
    assert!(
        metadata.durable(),
        "composition metadata must advertise durability"
    );
    assert!(blocks.durable(), "Ozone blocks must advertise durability");
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks,
        ChunkedOptions::fixed(owner, CHUNK_SIZE)
            .expect("valid fixed chunk size")
            .with_lease_ttl(Duration::from_secs(30)),
    )
    .await
    .expect("could not open the Ozone ChunkedFs composition");
    let loopback = Loopback::new(filesystem.clone());
    loopback
        .mkdir("/chunks", MkdirOptions::default())
        .await
        .expect("could not create the Ozone composition directory");

    let mut expected = expected_bytes();
    let file = loopback
        .open("/chunks/binary", "w+", 0o640)
        .await
        .expect("could not open the Ozone composition file");
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
        .expect("could not shrink the Ozone composition file");
    expected.truncate(63);
    file.truncate(88)
        .await
        .expect("could not extend the Ozone composition file");
    expected.resize(88, 0);

    let final_patch = [0xde, 0xad, 0x00, 0xbe, 0xef];
    assert_eq!(
        file.write(&final_patch, Some(2)).await.unwrap(),
        final_patch.len()
    );
    expected[2..2 + final_patch.len()].copy_from_slice(&final_patch);
    let extended_patch = (0..13)
        .map(|index| (0x30_u8).wrapping_add(index * 5))
        .collect::<Vec<_>>();
    assert_eq!(
        file.write(&extended_patch, Some(70)).await.unwrap(),
        extended_patch.len()
    );
    expected[70..70 + extended_patch.len()].copy_from_slice(&extended_patch);

    let range_start = 4_usize;
    let mut range = vec![0; 37];
    assert_eq!(
        file.read(&mut range, Some(range_start as u64))
            .await
            .unwrap(),
        range.len()
    );
    assert_eq!(&range, &expected[range_start..range_start + range.len()]);
    assert_eq!(file.stat().await.unwrap().size, expected.len() as u64);
    file.sync()
        .await
        .expect("could not sync the Ozone composition file");
    file.close()
        .await
        .expect("could not close the Ozone composition file");
    let bounded_file = loopback
        .open("/chunks/bounded", "w+", 0o640)
        .await
        .expect("could not open the bounded-listing fixture file");
    bounded_file
        .write(b"bounded listing fixture", Some(0))
        .await
        .expect("could not write the bounded-listing fixture file");
    bounded_file
        .close()
        .await
        .expect("could not close the bounded-listing fixture file");
    let bounded_entries = filesystem
        .readdir_bounded("/chunks", 2)
        .await
        .expect("Ozone metadata provider could not enumerate its bounded directory");
    let bounded_names = bounded_entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        bounded_names,
        BTreeSet::from(["binary", "bounded"]),
        "bounded Ozone directory listing returned unexpected entries"
    );
    let overflow = filesystem
        .readdir_bounded("/chunks", 1)
        .await
        .expect_err("Ozone metadata provider exceeded the configured directory bound");
    assert!(
        overflow.is(ErrorCode::Eoverflow),
        "bounded Ozone directory overflow returned the wrong error: {overflow}"
    );
    println!(
        "OZONE_CHUNKED_BOUNDED_READDIR_PASS provider_owner={owner} entries={}",
        bounded_entries.len()
    );
    assert_eq!(
        loopback.read_file("/chunks/binary").await.unwrap(),
        expected
    );
    loopback
        .syncfs()
        .await
        .expect("could not flush the Ozone composition filesystem");
    drop(loopback);
    filesystem
        .shutdown()
        .await
        .expect("could not release the Ozone composition writer lease");

    let snapshot = metadata
        .load()
        .await
        .expect("could not load the Ozone composition metadata");
    assert!(snapshot.revision > 0);
    (expected, snapshot)
}

async fn reopen_chunked_filesystem<M>(
    metadata: M,
    blocks: TrackedOzoneBlocks,
    expected: &[u8],
    owner: &str,
) -> LoadedMetadata
where
    M: MetadataStore + Clone + 'static,
{
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks,
        ChunkedOptions::fixed(owner, 4096)
            .expect("valid reopen chunk size")
            .with_lease_ttl(Duration::from_secs(30)),
    )
    .await
    .expect("could not reopen the Ozone ChunkedFs composition");
    let loopback = Loopback::new(filesystem.clone());
    assert_eq!(
        loopback.read_file("/chunks/binary").await.unwrap(),
        expected
    );

    let file = loopback
        .open("/chunks/binary", "r", 0)
        .await
        .expect("could not open the reopened Ozone composition file");
    let range_start = 61_usize;
    let mut range = vec![0; 23];
    assert_eq!(
        file.read(&mut range, Some(range_start as u64))
            .await
            .unwrap(),
        range.len()
    );
    assert_eq!(&range, &expected[range_start..range_start + range.len()]);
    file.close()
        .await
        .expect("could not close the reopened file");
    loopback
        .syncfs()
        .await
        .expect("could not sync the reopened Ozone composition filesystem");
    drop(loopback);
    filesystem
        .shutdown()
        .await
        .expect("could not release the reopened Ozone writer lease");

    metadata
        .load()
        .await
        .expect("could not load metadata after Ozone reopen")
}

async fn assert_fencing<M>(first: &M, second: &M)
where
    M: MetadataStore,
{
    let active = first
        .acquire_writer("ozone-composition-active", Duration::from_secs(30))
        .await
        .expect("could not acquire the first Ozone metadata writer");
    let busy = second
        .acquire_writer("ozone-composition-busy", Duration::from_secs(2))
        .await
        .expect_err("metadata provider accepted two active Ozone writers");
    assert!(busy.is(ErrorCode::Eagain));
    first
        .release_writer(&active)
        .await
        .expect("could not release the first Ozone metadata writer");

    let stale = first
        .acquire_writer("ozone-composition-stale", Duration::from_millis(250))
        .await
        .expect("could not acquire the expiring Ozone metadata writer");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let replacement = second
        .acquire_writer("ozone-composition-replacement", Duration::from_secs(5))
        .await
        .expect("could not acquire the replacement Ozone metadata writer");
    assert!(replacement.fence > stale.fence);

    let loaded = second
        .load()
        .await
        .expect("could not load the current Ozone metadata snapshot");
    let namespace = loaded
        .namespace
        .clone()
        .expect("Ozone composition namespace must be initialized");
    let stale_error = first
        .publish(loaded.revision, &stale, namespace.clone())
        .await
        .expect_err("expired Ozone writer published after replacement");
    assert!(stale_error.is(ErrorCode::Estale));

    let published = second
        .publish(loaded.revision, &replacement, namespace.clone())
        .await
        .expect("current Ozone writer could not publish its CAS revision");
    assert_eq!(published, loaded.revision + 1);
    let conflict = second
        .publish(loaded.revision, &replacement, namespace)
        .await
        .expect_err("Ozone metadata provider accepted a stale revision CAS");
    assert!(conflict.is(ErrorCode::Eagain));
    second
        .release_writer(&replacement)
        .await
        .expect("could not release the replacement Ozone metadata writer");
}

async fn delete_pglite_metadata_row(url: &str, volume_key: &str) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("could not connect for PGlite composition cleanup");
    let task = tokio::spawn(async move {
        connection
            .await
            .expect("PGlite composition cleanup connection failed")
    });
    client
        .execute(
            "DELETE FROM mount_rs_metadata WHERE volume_key = $1",
            &[&volume_key],
        )
        .await
        .expect("could not remove the scoped PGlite composition metadata row");
    drop(client);
    task.await
        .expect("PGlite composition cleanup task panicked");
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, future)
        .await
        .expect("Ozone ChunkedFs composition exceeded its bounded timeout")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit opt-in plus real Apache Ozone and SQLite services"]
async fn real_ozone_sqlite_chunked_composition() {
    assert_eq!(
        std::env::var(SQLITE_OPT_IN).as_deref(),
        Ok("1"),
        "set {SQLITE_OPT_IN}=1 to explicitly run the ignored Ozone/SQLite composition gate"
    );
    bounded(async {
        let config = local_config();
        let prefix = format!("{}/chunked-sqlite", test_prefix());
        let metadata_path = sqlite_metadata_path();
        let blocks = TrackedOzoneBlocks::new(&config, prefix);

        let (expected, seeded) = seed_chunked_filesystem(
            SqliteMetadataStore::open(&metadata_path).expect("could not open SQLite metadata"),
            blocks.clone(),
            "ozone-sqlite-seed",
        )
        .await;
        let reopened = reopen_chunked_filesystem(
            SqliteMetadataStore::open(&metadata_path).expect("could not reopen SQLite metadata"),
            blocks.clone(),
            &expected,
            "ozone-sqlite-reopen",
        )
        .await;
        assert!(
            reopened.revision >= seeded.revision,
            "SQLite reopen revision regressed: seeded={}, reopened={}",
            seeded.revision,
            reopened.revision
        );

        let first = SqliteMetadataStore::open(&metadata_path)
            .expect("could not open the first SQLite fencing client");
        let second = SqliteMetadataStore::open(&metadata_path)
            .expect("could not open the second SQLite fencing client");
        assert_fencing(&first, &second).await;
        blocks.delete_created().await;
        println!(
            "OZONE_SQLITE_CHUNKED_COMPOSITION_PASS metadata={} revision={}",
            metadata_path.display(),
            reopened.revision + 1
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit opt-in plus real Apache Ozone and disk-backed PGlite services"]
async fn real_ozone_pglite_chunked_composition() {
    assert_eq!(
        std::env::var(PGLITE_OPT_IN).as_deref(),
        Ok("1"),
        "set {PGLITE_OPT_IN}=1 to explicitly run the ignored Ozone/PGlite composition gate"
    );
    bounded(async {
        let config = local_config();
        let url = pglite_url();
        let prefix = format!("{}/chunked-pglite", test_prefix());
        let volume_key = format!("{prefix}/metadata");
        let options = PgliteStorageOptions::new(volume_key.clone()).with_durable(true);
        let blocks = TrackedOzoneBlocks::new(&config, prefix);

        let first_metadata = PgliteMetadataStore::connect_with_options(&url, options.clone())
            .await
            .expect("could not connect the first PGlite metadata client");
        let (expected, seeded) =
            seed_chunked_filesystem(first_metadata.clone(), blocks.clone(), "ozone-pglite-seed")
                .await;
        first_metadata
            .close()
            .await
            .expect("could not close the seeded PGlite metadata client");

        let reopened_metadata = PgliteMetadataStore::connect_with_options(&url, options.clone())
            .await
            .expect("could not reconnect the PGlite metadata client");
        let reopened = reopen_chunked_filesystem(
            reopened_metadata.clone(),
            blocks.clone(),
            &expected,
            "ozone-pglite-reopen",
        )
        .await;
        assert!(
            reopened.revision >= seeded.revision,
            "PGlite reopen revision regressed: seeded={}, reopened={}",
            seeded.revision,
            reopened.revision
        );
        reopened_metadata
            .close()
            .await
            .expect("could not close the reopened PGlite metadata client");

        let first = PgliteMetadataStore::connect_with_options(&url, options.clone())
            .await
            .expect("could not open the first PGlite fencing client");
        let second = PgliteMetadataStore::connect_with_options(&url, options)
            .await
            .expect("could not open the second PGlite fencing client");
        assert_fencing(&first, &second).await;
        first
            .close()
            .await
            .expect("could not close the first PGlite fencing client");
        second
            .close()
            .await
            .expect("could not close the second PGlite fencing client");
        delete_pglite_metadata_row(&url, &volume_key).await;
        blocks.delete_created().await;
        println!(
            "OZONE_PGLITE_CHUNKED_COMPOSITION_PASS volume={} revision={}",
            volume_key,
            reopened.revision + 1
        );
    })
    .await;
}
