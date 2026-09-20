#![cfg(feature = "remote-harness")]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use mount_rs_core::storage::{BlockId, BlockStore, MetadataStore};
use mount_rs_pglite::{PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_sqlite_vfs::{SqliteVfs, StorageBackend, StorageOptions, TokioExecutor};
use rusqlite::OpenFlags;

fn flags() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_FULL_MUTEX
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} must be set by the real PGlite + RustFS harness; refusing a partial remote test"
        )
    })
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn remote_config() -> (R2Config, String) {
    let config = R2Config::from_env().expect("RustFS R2-compatible environment");
    assert!(
        config.endpoint.starts_with("http://127.0.0.1:")
            || config.endpoint.starts_with("http://localhost:"),
        "remote VFS hook accepts only a loopback RustFS endpoint, got {}",
        config.endpoint
    );
    let prefix = format!(
        "{}/sqlite-vfs/{}",
        required_env("RUSTFS_TEST_PREFIX"),
        unique_suffix()
    );
    (config, prefix)
}

#[derive(Clone)]
struct TrackedR2Blocks {
    inner: R2BlockStore,
    created: Arc<Mutex<BTreeSet<String>>>,
}

impl TrackedR2Blocks {
    fn new(config: &R2Config, prefix: String) -> Self {
        Self {
            inner: R2BlockStore::from_config(config, prefix).expect("R2 block store"),
            created: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    async fn cleanup(&self) {
        let ids = self
            .created
            .lock()
            .expect("tracked R2 block lock")
            .iter()
            .cloned()
            .map(BlockId)
            .collect::<Vec<_>>();
        for id in ids {
            self.inner
                .delete(&id)
                .await
                .expect("delete remote test block");
        }
    }
}

#[async_trait]
impl BlockStore for TrackedR2Blocks {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
        let id = self.inner.put(bytes).await?;
        self.created
            .lock()
            .expect("tracked R2 block lock")
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
        self.created
            .lock()
            .expect("tracked R2 block lock")
            .remove(&id.0);
        self.inner.delete(id).await
    }
}

fn registration_name(label: &str) -> String {
    format!("mount_rs_remote_{label}_{}", unique_suffix())
}

#[test]
#[ignore = "requires scripts/test-rustfs.sh or an equivalent real PGlite + RustFS environment"]
fn remote_pglite_rustfs_sqlite_vfs() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread Tokio runtime");

    runtime.block_on(async {
        let (config, prefix) = remote_config();
        let pglite_url = required_env("PGLITE_DATABASE_URL");
        let volume_key = format!("{prefix}/metadata");
        let provider_options = PgliteStorageOptions::new(volume_key.clone()).with_durable(true);

        let metadata =
            PgliteMetadataStore::connect_with_options(&pglite_url, provider_options.clone())
                .await
                .expect("PGlite metadata provider");
        let blocks = TrackedR2Blocks::new(&config, format!("{prefix}/blocks"));
        assert!(metadata.durable());
        assert!(blocks.durable());

        let first_backend = StorageBackend::with_executor(
            metadata.clone(),
            blocks.clone(),
            StorageOptions::new(format!("{volume_key}/first"), 4096)
                .expect("first storage options"),
            TokioExecutor::current().expect("Tokio runtime handle"),
        )
        .expect("remote storage bridge");
        let first_vfs = SqliteVfs::new(&registration_name("first"), Arc::new(first_backend))
            .expect("first remote VFS registration");
        {
            let connection = first_vfs
                .open("remote.db", flags())
                .expect("open remote SQLite database");
            connection
                .execute_batch(
                    "PRAGMA journal_mode=DELETE;
                     PRAGMA synchronous=FULL;
                     CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
                     INSERT INTO records(body) VALUES (x'0001ff');",
                )
                .expect("remote SQLite transaction");
            let body: Vec<u8> = connection
                .query_row("SELECT body FROM records", [], |row| row.get(0))
                .expect("read remote SQLite blob");
            assert_eq!(body, [0, 1, 255]);
        }
        metadata.close().await.expect("close first PGlite metadata");

        // Reconnect both providers and reopen through a fresh VFS name. The
        // registration itself is intentionally retained by SQLite for the
        // process lifetime, while the provider clients exercise a real
        // remote restart boundary.
        let reopened_metadata =
            PgliteMetadataStore::connect_with_options(&pglite_url, provider_options)
                .await
                .expect("reopen PGlite metadata provider");
        let reopened_blocks = TrackedR2Blocks::new(&config, format!("{prefix}/blocks"));
        let reopened_backend = StorageBackend::with_executor(
            reopened_metadata.clone(),
            reopened_blocks.clone(),
            StorageOptions::new(format!("{volume_key}/reopen"), 4096)
                .expect("reopen storage options"),
            TokioExecutor::current().expect("Tokio runtime handle"),
        )
        .expect("reopen remote storage bridge");
        let reopened_vfs = SqliteVfs::new(&registration_name("reopen"), Arc::new(reopened_backend))
            .expect("reopen remote VFS registration");
        let connection = reopened_vfs
            .open("remote.db", flags())
            .expect("reopen remote SQLite database");
        let body: Vec<u8> = connection
            .query_row("SELECT body FROM records", [], |row| row.get(0))
            .expect("read reopened remote SQLite blob");
        assert_eq!(body, [0, 1, 255]);
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("reopened remote integrity check");
        assert_eq!(integrity, "ok");
        drop(connection);

        reopened_metadata
            .close()
            .await
            .expect("close reopened PGlite metadata");
        blocks.cleanup().await;
        reopened_blocks.cleanup().await;
    });
}
