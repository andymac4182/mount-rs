//! Lifecycle observations must preserve a failed owner's publication uncertainty.

#![cfg(unix)]

use std::path::Path;

use mount_rs_core::ErrorCode;
use mount_rs_core::storage::{BlockStore, ConcurrentBackingId, MetadataStore};
use mount_rs_sdk::{Filesystem, HostOptions, MemoryOptions, SplitOptions, StoreConfig};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use rusqlite::{Connection, params};

const FILE: &str = "/acknowledged";
const CHUNK_BYTES: usize = 4096;

fn payload(seed: usize) -> Vec<u8> {
    (0..12_461)
        .map(|offset| ((offset * 19 + seed * 37 + offset / 101) % 256) as u8)
        .collect()
}

fn compact_options(path: &Path, owner: &str) -> SplitOptions {
    let store = StoreConfig::Sqlite {
        path: path.to_owned(),
    };
    let mut options = SplitOptions::memory(owner, CHUNK_BYTES).with_compact_inode_updates(true);
    options.metadata = store.clone();
    options.blocks = store;
    options
}

async fn full_file_oracle(filesystem: &Filesystem, expected: &[u8]) {
    let handle = filesystem.driver().open(FILE, "r", 0).await.unwrap();
    assert_eq!(handle.stat().await.unwrap().size, expected.len() as u64);
    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let remaining = actual.len() - offset;
        let count = handle
            .read(&mut actual[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(
            count > 0 && count <= remaining,
            "invalid full-file read count"
        );
        offset += count;
    }
    assert_eq!(actual, expected, "full acknowledged payload changed");
    assert_eq!(
        handle
            .read(&mut [0; 1], Some(expected.len() as u64))
            .await
            .unwrap(),
        0,
        "unexpected bytes beyond the acknowledged EOF"
    );
    handle.close().await.unwrap();
}

async fn persisted_compact_backing(path: &Path) -> ConcurrentBackingId {
    let metadata = SqliteMetadataStore::open(path).unwrap();
    let mode = metadata
        .compact_inode_mode_state()
        .await
        .unwrap()
        .expect("actual SQLite metadata must retain its MRC5 marker");
    let blocks = SqliteBlockStore::open(path).unwrap();
    blocks
        .verify_concurrent_backing(mode.backing)
        .await
        .unwrap();
    mode.backing
}

fn stored_guard(connection: &Connection, inode: u64) -> (i64, i64, i64, String) {
    connection
        .query_row(
            "SELECT incarnation,epoch,revision,node FROM mount_rs_compact_guards WHERE inode=?1",
            params![inode.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

#[tokio::test]
async fn sqlite_mrc5_publication_failure_remains_sticky_after_successful_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed-owner.sqlite");
    let filesystem = Filesystem::split(compact_options(&path, "failed-owner"))
        .await
        .unwrap();
    assert!(!filesystem.failed());
    let backing = filesystem.concurrent_backing_id().unwrap();
    assert_eq!(backing, persisted_compact_backing(&path).await);

    let acknowledged = payload(1);
    filesystem
        .driver()
        .write_file(FILE, &acknowledged)
        .await
        .unwrap();
    full_file_oracle(&filesystem, &acknowledged).await;
    filesystem.driver().syncfs().await.unwrap();
    let handle = filesystem.driver().open(FILE, "r+", 0).await.unwrap();
    let inode = handle.stat().await.unwrap().ino;
    let connection = Connection::open(&path).unwrap();
    let before = stored_guard(&connection, inode);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_runtime_health_publication
             BEFORE UPDATE ON mount_rs_compact_guards
             BEGIN
                 SELECT RAISE(ABORT, 'runtime-health publication rejected');
             END;",
        )
        .unwrap();

    let replacement = payload(9);
    let failure = handle
        .write(&replacement[..CHUNK_BYTES], Some(CHUNK_BYTES as u64))
        .await
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::Eio);
    assert!(
        failure
            .to_string()
            .contains("runtime-health publication rejected"),
        "overwrite must fail at the injected SQLite UPDATE: {failure}"
    );
    assert_eq!(stored_guard(&connection, inode), before);
    assert!(
        filesystem.failed(),
        "publication failure must remain sticky"
    );
    assert_eq!(filesystem.concurrent_backing_id(), Some(backing));
    handle.close().await.unwrap();

    // MRC5 has no writer lease to release. Successful teardown must not be
    // confused with clearing this owner's failed publication state.
    filesystem.shutdown().await.unwrap();
    assert!(
        filesystem.failed(),
        "shutdown success must not imply health"
    );
    assert_eq!(filesystem.concurrent_backing_id(), Some(backing));
    connection
        .execute_batch("DROP TRIGGER reject_runtime_health_publication")
        .unwrap();
    drop(connection);
    assert_eq!(backing, persisted_compact_backing(&path).await);

    // This is a distinct owner with fresh provider connections, explicitly
    // opened by the test rather than an automatic replacement of the failed one.
    let fresh = Filesystem::split(compact_options(&path, "explicit-fresh-reader"))
        .await
        .unwrap();
    assert!(!fresh.failed());
    assert_eq!(fresh.concurrent_backing_id(), Some(backing));
    full_file_oracle(&fresh, &acknowledged).await;
    fresh.shutdown().await.unwrap();
    assert!(
        filesystem.failed(),
        "a fresh reader cannot heal the old owner"
    );
    assert_eq!(filesystem.concurrent_backing_id(), Some(backing));
}

#[tokio::test]
async fn healthy_sqlite_mrc5_retains_identity_and_full_bytes_after_explicit_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("healthy-owner.sqlite");
    let first = Filesystem::split(compact_options(&path, "healthy-owner"))
        .await
        .unwrap();
    let backing = first.concurrent_backing_id().unwrap();
    assert_eq!(backing, persisted_compact_backing(&path).await);
    let expected = payload(3);
    first.driver().write_file(FILE, &expected).await.unwrap();
    full_file_oracle(&first, &expected).await;
    assert!(!first.failed());
    first.shutdown().await.unwrap();
    assert!(!first.failed());
    assert_eq!(first.concurrent_backing_id(), Some(backing));
    drop(first);

    let fresh = Filesystem::split(compact_options(&path, "healthy-fresh-reader"))
        .await
        .unwrap();
    assert!(!fresh.failed());
    assert_eq!(fresh.concurrent_backing_id(), Some(backing));
    assert_eq!(backing, persisted_compact_backing(&path).await);
    full_file_oracle(&fresh, &expected).await;
    fresh.shutdown().await.unwrap();
    assert!(!fresh.failed());
}

#[tokio::test]
async fn nonchunked_memory_and_host_observations_preserve_existing_io() {
    let directory = tempfile::tempdir().unwrap();
    let expected = payload(4);
    let filesystems = [
        Filesystem::memory(MemoryOptions::default()),
        Filesystem::host(directory.path(), HostOptions { read_only: false }),
    ];
    for filesystem in filesystems {
        assert!(!filesystem.failed());
        assert_eq!(filesystem.concurrent_backing_id(), None);
        filesystem
            .driver()
            .write_file(FILE, &expected)
            .await
            .unwrap();
        full_file_oracle(&filesystem, &expected).await;
        filesystem.shutdown().await.unwrap();
        assert!(!filesystem.failed());
        assert_eq!(filesystem.concurrent_backing_id(), None);
    }
}

#[tokio::test]
async fn nonchunked_sqlite_observations_preserve_snapshot_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("snapshot-owner.sqlite");
    let first = Filesystem::sqlite(&path).await.unwrap();
    assert!(!first.failed());
    assert_eq!(first.concurrent_backing_id(), None);
    let expected = payload(5);
    first.driver().write_file(FILE, &expected).await.unwrap();
    first.shutdown().await.unwrap();
    assert!(!first.failed());
    assert_eq!(first.concurrent_backing_id(), None);
    drop(first);

    let fresh = Filesystem::sqlite(&path).await.unwrap();
    assert!(!fresh.failed());
    assert_eq!(fresh.concurrent_backing_id(), None);
    full_file_oracle(&fresh, &expected).await;
    fresh.shutdown().await.unwrap();
}

#[tokio::test]
async fn exclusive_split_reports_no_concurrent_backing_identity() {
    let filesystem = Filesystem::split(SplitOptions::memory("exclusive-health", CHUNK_BYTES))
        .await
        .unwrap();
    assert!(!filesystem.failed());
    assert_eq!(filesystem.concurrent_backing_id(), None);
    let expected = payload(6);
    filesystem
        .driver()
        .write_file(FILE, &expected)
        .await
        .unwrap();
    full_file_oracle(&filesystem, &expected).await;
    filesystem.shutdown().await.unwrap();
    assert!(!filesystem.failed());
    assert_eq!(filesystem.concurrent_backing_id(), None);
}
