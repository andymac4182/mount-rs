//! Independently opened SQLite coordinators under revision-CAS load.
//!
//! The metadata and block files belong to one disposable directory. Each
//! coordinator has its own SQLite connections and ChunkedFs runtime state,
//! matching the storage boundary between separate CLI processes. Native NFS
//! tests exercise the kernel and transport boundary separately.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::Loopback;
use mount_rs_core::storage::MetadataStore;
use mount_rs_memory::MemoryBlockStore;
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use rusqlite::Connection;
use tempfile::TempDir;
use tokio::sync::Barrier;

type SqliteFs = ChunkedFs<SqliteMetadataStore, SqliteBlockStore>;

fn create_journal(path: &Path, journal: &str) {
    let connection = Connection::open(path).expect("create owned SQLite database");
    connection
        .execute_batch("PRAGMA synchronous=FULL")
        .expect("set FULL synchronization");
    let actual: String = connection
        .query_row(&format!("PRAGMA journal_mode={journal}"), [], |row| {
            row.get(0)
        })
        .expect("set test journal mode");
    assert_eq!(actual.to_ascii_uppercase(), journal);
}

async fn open_writer(metadata: &Path, blocks: &Path, number: usize) -> SqliteFs {
    ChunkedFs::open(
        SqliteMetadataStore::open(metadata).expect("open independent metadata connection"),
        SqliteBlockStore::open(blocks).expect("open independent block connection"),
        ChunkedOptions::fixed(format!("sqlite-writer-{number}"), 4096)
            .expect("fixed chunker")
            .with_concurrent_writes(true),
    )
    .await
    .expect("open independent concurrent writer")
}

async fn exercise(writers: usize, lifecycles: usize, journal: &str) {
    assert!((2..=16).contains(&writers));
    assert!((1..=200).contains(&lifecycles));
    let scope = TempDir::new().expect("own disposable SQLite load directory");
    let metadata = scope.path().join("metadata.sqlite");
    let blocks = scope.path().join("blocks.sqlite");
    create_journal(&metadata, journal);
    create_journal(&blocks, journal);

    let mut drivers = Vec::with_capacity(writers);
    for writer in 0..writers {
        drivers.push(open_writer(&metadata, &blocks, writer).await);
    }

    let started = Instant::now();
    let barrier = Arc::new(Barrier::new(writers));
    let mut tasks = Vec::with_capacity(writers);
    for (writer, driver) in drivers.iter().cloned().enumerate() {
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            let view = Loopback::new(driver);
            barrier.wait().await;
            for lifecycle in 0..lifecycles {
                let original = format!("/writer-{writer}-{lifecycle}.bin");
                let renamed = format!("/renamed-{writer}-{lifecycle}.bin");
                let payload = format!("writer={writer};lifecycle={lifecycle};").into_bytes();
                view.write_file(&original, &payload)
                    .await
                    .unwrap_or_else(|error| panic!("write {original}: {error}"));
                view.rename(&original, &renamed)
                    .await
                    .unwrap_or_else(|error| panic!("rename {original}: {error}"));
                if lifecycle % 2 == 0 {
                    view.unlink(&renamed)
                        .await
                        .unwrap_or_else(|error| panic!("unlink {renamed}: {error}"));
                }
            }
        }));
    }
    for task in tasks {
        task.await.expect("SQLite writer task must complete");
    }

    let seeded = vec![0_u8; writers * 4096];
    Loopback::new(drivers[0].clone())
        .write_file("/shared.bin", &seeded)
        .await
        .expect("seed disjoint-range file");
    let barrier = Arc::new(Barrier::new(writers));
    let mut tasks = Vec::with_capacity(writers);
    for (writer, driver) in drivers.iter().cloned().enumerate() {
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            let view = Loopback::new(driver);
            let file = view
                .open("/shared.bin", "r+", 0)
                .await
                .expect("open shared file from independent coordinator");
            barrier.wait().await;
            let payload = vec![b'A' + writer as u8; 4096];
            let written = file
                .write(&payload, Some((writer * 4096) as u64))
                .await
                .expect("acknowledge disjoint-range write");
            assert_eq!(written, payload.len());
            file.close().await.expect("close shared file handle");
        }));
    }
    for task in tasks {
        task.await.expect("disjoint writer task must complete");
    }

    for driver in drivers {
        driver.shutdown().await.expect("close independent writer");
    }
    let fresh = open_writer(&metadata, &blocks, writers).await;
    let view = Loopback::new(fresh.clone());
    for writer in 0..writers {
        for lifecycle in 0..lifecycles {
            let renamed = format!("/renamed-{writer}-{lifecycle}.bin");
            if lifecycle % 2 == 0 {
                assert!(
                    view.read_file(&renamed).await.is_err(),
                    "{renamed} must be absent"
                );
            } else {
                let payload = format!("writer={writer};lifecycle={lifecycle};").into_bytes();
                assert_eq!(
                    view.read_file(&renamed).await.unwrap(),
                    payload,
                    "{renamed}"
                );
            }
        }
    }
    let merged = view
        .read_file("/shared.bin")
        .await
        .expect("fresh shared read");
    let expected = (0..writers)
        .flat_map(|writer| vec![b'A' + writer as u8; 4096])
        .collect::<Vec<_>>();
    assert_eq!(merged, expected, "all acknowledged disjoint writes survive");
    fresh.shutdown().await.expect("close fresh writer");

    let metadata_connection = Connection::open(&metadata).expect("inspect metadata database");
    let blocks_connection = Connection::open(&blocks).expect("inspect blocks database");
    for (name, connection) in [
        ("metadata", &metadata_connection),
        ("blocks", &blocks_connection),
    ] {
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("run SQLite integrity check");
        assert_eq!(integrity, "ok", "{name} database integrity");
    }
    let block_count: i64 = blocks_connection
        .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
        .expect("count immutable blocks");
    let namespace_bytes: i64 = metadata_connection
        .query_row(
            "SELECT length(namespace) FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("measure metadata namespace");
    let elapsed = started.elapsed();
    eprintln!(
        "SQLITE_CONCURRENT_LOAD journal={journal} writers={writers} lifecycles={} acknowledged_lifecycle_ops={} elapsed_ms={} blocks={block_count} namespace_bytes={namespace_bytes}",
        writers * lifecycles,
        writers * (2 * lifecycles + lifecycles.div_ceil(2)) + writers,
        elapsed.as_millis(),
    );
    assert!(
        elapsed < Duration::from_secs(300),
        "bounded load exceeded five minutes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_independent_sqlite_coordinators_merge_and_reopen() {
    exercise(2, 12, "DELETE").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_sqlite_coordinators_merge_and_reopen() {
    exercise(4, 12, "DELETE").await;
}

#[tokio::test]
async fn different_block_databases_cannot_read_a_shared_metadata_reference() {
    let scope = TempDir::new().expect("own disposable SQLite mismatch directory");
    let metadata = scope.path().join("metadata.sqlite");
    let blocks_a = scope.path().join("blocks-a.sqlite");
    let blocks_b = scope.path().join("blocks-b.sqlite");
    let writer_a = open_writer(&metadata, &blocks_a, 0).await;
    let writer_b = open_writer(&metadata, &blocks_b, 1).await;
    Loopback::new(writer_a.clone())
        .write_file("/from-a", b"shared metadata, private block bytes")
        .await
        .expect("writer A publishes one metadata reference");
    let error = Loopback::new(writer_b.clone())
        .read_file("/from-a")
        .await
        .expect_err("writer B cannot resolve a block in its different database");
    assert!(
        matches!(
            error.code,
            mount_rs_core::ErrorCode::Eio | mount_rs_core::ErrorCode::Enoent
        ),
        "a mismatched block backing must fail closed: {error}"
    );
    assert_eq!(
        Loopback::new(writer_a.clone())
            .read_file("/from-a")
            .await
            .expect("correct backing still reads"),
        b"shared metadata, private block bytes"
    );
    writer_a.shutdown().await.expect("close first writer");
    writer_b.shutdown().await.expect("close second writer");
}

#[tokio::test]
async fn volatile_blocks_reject_concurrent_sqlite_before_mode_conversion() {
    let scope = TempDir::new().expect("own disposable SQLite metadata directory");
    let metadata_path = scope.path().join("metadata.sqlite");
    let metadata = SqliteMetadataStore::open(&metadata_path).expect("open fresh metadata");
    let result = ChunkedFs::open(
        metadata,
        MemoryBlockStore::new(),
        ChunkedOptions::fixed("volatile-block-probe", 4096)
            .expect("fixed chunker")
            .with_concurrent_writes(true),
    )
    .await;
    let error = match result {
        Ok(driver) => {
            driver
                .shutdown()
                .await
                .expect("close unexpectedly opened driver");
            panic!("volatile blocks must not open an independently writable volume");
        }
        Err(error) => error,
    };
    assert!(error.is(mount_rs_core::ErrorCode::Enotsup));

    let connection = Connection::open(&metadata_path).expect("inspect rejected metadata");
    let state: (i64, Option<String>, Option<String>, i64) = connection
        .query_row(
            "SELECT revision, namespace, write_mode, fence FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("load rejected metadata row");
    assert_eq!(state, (0, None, None, 0));
    let reopened = SqliteMetadataStore::open(&metadata_path).expect("open unchanged metadata");
    let lease = reopened
        .acquire_writer("legacy-writer", Duration::from_secs(30))
        .await
        .expect("legacy writer still opens after block preflight rejection");
    reopened
        .release_writer(&lease)
        .await
        .expect("release legacy writer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in local SQLite backing journal-mode matrix"]
async fn independent_sqlite_coordinators_journal_mode_matrix() {
    assert_eq!(
        std::env::var("MOUNT_RS_SQLITE_MULTIWRITER_JOURNAL_MATRIX")
            .ok()
            .as_deref(),
        Some("1"),
        "set MOUNT_RS_SQLITE_MULTIWRITER_JOURNAL_MATRIX=1 for owned journal matrix"
    );
    // WAL persists across connections. TRUNCATE and PERSIST do not, so
    // setting them on the preparatory connection would only exercise DELETE
    // once independently opened provider connections take over.
    for journal in ["DELETE", "WAL"] {
        exercise(4, 12, journal).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "bounded opt-in concurrent SQLite provider load"]
async fn independent_sqlite_coordinators_load_and_check_integrity() {
    assert_eq!(
        std::env::var("MOUNT_RS_SQLITE_MULTIWRITER_LOAD")
            .ok()
            .as_deref(),
        Some("1"),
        "set MOUNT_RS_SQLITE_MULTIWRITER_LOAD=1 for owned load"
    );
    let journal = std::env::var("MOUNT_RS_SQLITE_MULTIWRITER_JOURNAL")
        .unwrap_or_else(|_| "DELETE".to_owned())
        .to_ascii_uppercase();
    assert!(matches!(journal.as_str(), "DELETE" | "WAL"));
    exercise(8, 100, &journal).await;
}
