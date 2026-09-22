#![cfg(feature = "pglite-harness")]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_sqlite_vfs::{SqliteVfs, StorageBackend, StorageOptions, TokioExecutor};
use rusqlite::OpenFlags;

fn flags() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_FULL_MUTEX
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn required_pglite_url() -> String {
    std::env::var("PGLITE_DATABASE_URL").unwrap_or_else(|_| {
        panic!(
            "PGLITE_DATABASE_URL must point to a real PGlite PostgreSQL-wire server; refusing an emulated or mount-backed test"
        )
    })
}

#[test]
#[ignore = "requires a real PGlite PostgreSQL-wire server in PGLITE_DATABASE_URL"]
fn pglite_sqlite_vfs_round_trip_and_fresh_provider_reconnect() {
    let pglite_url = required_pglite_url();
    let volume_key = format!("mount-rs-sqlite-vfs/pglite/{}", unique_suffix());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread Tokio runtime");

    runtime.block_on(async {
        let provider_options = PgliteStorageOptions::new(volume_key.clone());
        let metadata =
            PgliteMetadataStore::connect_with_options(&pglite_url, provider_options.clone())
                .await
                .expect("connect first PGlite metadata provider");
        let blocks = PgliteBlockStore::connect_with_options(&pglite_url, provider_options)
            .await
            .expect("connect first PGlite block provider");

        let first_backend = StorageBackend::with_executor(
            metadata.clone(),
            blocks.clone(),
            StorageOptions::volatile_for_tests(format!("{volume_key}/first"), 4096)
                .expect("first volatile storage options"),
            TokioExecutor::current().expect("Tokio runtime handle"),
        )
        .expect("first PGlite storage bridge");
        let first_vfs = SqliteVfs::new(
            &format!("mount_rs_pglite_first_{}", unique_suffix()),
            Arc::new(first_backend),
        )
        .expect("register first PGlite SQLite VFS");

        let committed = vec![0_u8, 1, 2, 255];
        let rolled_back = vec![222_u8, 173, 190, 239];
        {
            // This opens SQLite directly through the synchronous VFS. No
            // mount, FUSE transport, or filesystem emulation is involved.
            let mut connection = first_vfs
                .open("pglite.db", flags())
                .expect("open first SQLite connection");
            connection
                .execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")
                .expect("configure rollback journal");
            connection
                .execute_batch(
                    "CREATE TABLE records(
                         id INTEGER PRIMARY KEY,
                         body BLOB NOT NULL
                     );",
                )
                .expect("create binary ledger");

            connection
                .with_connection_mut(|connection| -> rusqlite::Result<()> {
                    let transaction = connection.transaction()?;
                    transaction.execute(
                        "INSERT INTO records(id, body) VALUES (?1, ?2)",
                        rusqlite::params![1_i64, &committed],
                    )?;
                    transaction.commit()
                })
                .expect("commit binary SQLite transaction");
            connection
                .with_connection_mut(|connection| -> rusqlite::Result<()> {
                    let transaction = connection.transaction()?;
                    transaction.execute(
                        "INSERT INTO records(id, body) VALUES (?1, ?2)",
                        rusqlite::params![2_i64, &rolled_back],
                    )?;
                    transaction.rollback()
                })
                .expect("rollback binary SQLite transaction");

            let mut statement = connection
                .prepare("SELECT id, body FROM records ORDER BY id")
                .expect("prepare first binary ledger");
            let rows: Vec<(i64, Vec<u8>)> = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("read first binary ledger")
                .collect::<rusqlite::Result<_>>()
                .expect("collect first binary ledger");
            assert_eq!(rows, vec![(1, committed.clone())]);
            let integrity: String = connection
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                .expect("first integrity check");
            assert_eq!(integrity, "ok");
        }
        first_vfs.close().expect("close first SQLite VFS");
        metadata
            .close()
            .await
            .expect("close first PGlite metadata provider");
        blocks
            .close()
            .await
            .expect("close first PGlite block provider");

        let reopened_options = PgliteStorageOptions::new(volume_key.clone());
        let reopened_metadata =
            PgliteMetadataStore::connect_with_options(&pglite_url, reopened_options.clone())
                .await
                .expect("connect fresh PGlite metadata provider");
        let reopened_blocks = PgliteBlockStore::connect_with_options(&pglite_url, reopened_options)
            .await
            .expect("connect fresh PGlite block provider");
        let reopened_backend = StorageBackend::with_executor(
            reopened_metadata.clone(),
            reopened_blocks.clone(),
            StorageOptions::volatile_for_tests(format!("{volume_key}/reopen"), 4096)
                .expect("reopen volatile storage options"),
            TokioExecutor::current().expect("Tokio runtime handle"),
        )
        .expect("reopen PGlite storage bridge");
        let reopened_vfs = SqliteVfs::new(
            &format!("mount_rs_pglite_reopen_{}", unique_suffix()),
            Arc::new(reopened_backend),
        )
        .expect("register fresh PGlite SQLite VFS");

        {
            let connection = reopened_vfs
                .open("pglite.db", flags())
                .expect("reopen SQLite through fresh VFS");
            let rows: Vec<(i64, Vec<u8>)> = connection
                .prepare("SELECT id, body FROM records ORDER BY id")
                .expect("prepare reopened binary ledger")
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("read reopened binary ledger")
                .collect::<rusqlite::Result<_>>()
                .expect("collect reopened binary ledger");
            assert_eq!(rows, vec![(1, committed)]);
            let integrity: String = connection
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                .expect("reopened integrity check");
            assert_eq!(integrity, "ok");
        }
        reopened_vfs.close().expect("close fresh SQLite VFS");
        reopened_metadata
            .close()
            .await
            .expect("close fresh PGlite metadata provider");
        reopened_blocks
            .close()
            .await
            .expect("close fresh PGlite block provider");
    });
}
