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
#[ignore = "requires the prepare/reopen phases of scripts/test-rustfs.sh"]
fn remote_vfs_survives_rustfs_restart() {
    let phase = required_env("RUSTFS_VFS_RESTART_PHASE");
    assert!(matches!(phase.as_str(), "prepare" | "reopen"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("restart test runtime");
    runtime.block_on(async {
        let (config, _) = remote_config();
        // The harness retains this exact prefix across the service restart,
        // while every test process and provider connection is fresh.
        let prefix = format!("{}/sqlite-vfs-restart", required_env("RUSTFS_TEST_PREFIX"));
        let metadata = PgliteMetadataStore::connect_with_options(
            &required_env("PGLITE_DATABASE_URL"),
            PgliteStorageOptions::new(format!("{prefix}/metadata")).with_durable(true),
        )
        .await
        .expect("restart metadata provider");
        let blocks = R2BlockStore::from_config(&config, format!("{prefix}/blocks"))
            .expect("restart block provider");
        let backend = StorageBackend::with_executor(
            metadata.clone(),
            blocks,
            StorageOptions::new(format!("{prefix}/{phase}"), 4096).unwrap(),
            TokioExecutor::current().unwrap(),
        )
        .expect("restart storage bridge");
        let vfs = SqliteVfs::new(&registration_name("restart"), Arc::new(backend)).unwrap();
        let connection = vfs.open("restart.db", flags()).unwrap();
        let mode: String = connection
            .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        connection.execute_batch("PRAGMA synchronous=FULL").unwrap();
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(synchronous, 2);
        if phase == "prepare" {
            connection
                .execute_batch(
                    "CREATE TABLE ledger(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
                 BEGIN IMMEDIATE;
                 INSERT INTO ledger VALUES (1, x'0001ff'), (2, x'ff1000');
                 COMMIT;
                 BEGIN IMMEDIATE;
                 INSERT INTO ledger VALUES (3, x'deadbeef');
                 ROLLBACK;",
                )
                .unwrap();
        }
        let mut statement = connection
            .prepare("SELECT id, body FROM ledger ORDER BY id")
            .unwrap();
        let rows: Vec<(i64, Vec<u8>)> = statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, vec![(1, vec![0, 1, 255]), (2, vec![255, 16, 0])]);
        drop(statement);
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        drop(connection);
        metadata.close().await.unwrap();
        // Blocks are deliberately retained between phases. The isolated
        // harness owns and removes the entire test bucket/data directory.
        println!("RUSTFS_VFS_RESTART_PHASE_PASS phase={phase}");
    });
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
        // process lifetime. This checks fresh remote clients, not a service
        // restart: neither PGlite nor RustFS is restarted by this test.
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
