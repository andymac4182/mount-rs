//! Independently opened SQLite coordinators under revision-CAS load.
//!
//! The metadata and block files belong to one disposable directory. Each
//! coordinator has its own SQLite connections and ChunkedFs runtime state,
//! matching the storage boundary between separate CLI processes. Native NFS
//! tests exercise the kernel and transport boundary separately.

// Concurrent SQLite authority is bound to Unix physical file identities.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions, migrate_mrc1_backing};
use mount_rs_core::storage::{
    BlockExtent, BlockId, BlockStore, ConcurrentModeState, MetadataStore, NodeData,
};
use mount_rs_core::{ErrorCode, Loopback};
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

/// Leave the owned fixture in the on-disk state produced by an older MRC1
/// writer. This stays valid when the fresh concurrent opener starts creating
/// MRC2 volumes in the protocol integration step.
fn stamp_owned_mrc1_fixture(metadata: &Path, blocks: &Path) {
    let changed = Connection::open(metadata)
        .expect("open owned metadata fixture")
        .execute(
            "UPDATE mount_rs_metadata
             SET write_mode='MRC1', backing_id=NULL, owner=NULL,
                 fence=9223372036854775807, expires=0
             WHERE id=1",
            [],
        )
        .expect("stamp legacy MRC1 metadata fence");
    assert_eq!(changed, 1, "stamp exactly one metadata volume");
    Connection::open(blocks)
        .expect("open owned blocks fixture")
        .execute("DELETE FROM mount_rs_block_authority", [])
        .expect("remove any later MRC2-only block authority marker");
}

async fn exercise(writers: usize, lifecycles: usize, journal: &str) {
    assert!((2..=16).contains(&writers));
    assert!((1..=200).contains(&lifecycles));
    let scope = TempDir::new().expect("own disposable SQLite load directory");
    let metadata = scope.path().join("metadata.sqlite");
    let blocks = scope.path().join("blocks.sqlite");
    // Let the provider create and stamp its owned metadata file before the
    // journal probe opens it with raw SQLite. An already existing unstamped
    // database cannot safely enroll in independent concurrent mode.
    drop(SqliteMetadataStore::open(&metadata).expect("create owned metadata backing"));
    drop(SqliteBlockStore::open(&blocks).expect("create owned block backing"));
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
                    // Debug retains the provider stage (`block-put` versus
                    // `metadata-publish`) hidden by FsError's display text.
                    .unwrap_or_else(|error| panic!("write {original}: {error:?}"));
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
    let second = ChunkedFs::open(
        SqliteMetadataStore::open(&metadata).expect("open independent shared metadata"),
        SqliteBlockStore::open(&blocks_b).expect("open different physical blocks"),
        ChunkedOptions::fixed("sqlite-writer-wrong-backing", 4096)
            .expect("fixed chunker")
            .with_concurrent_writes(true),
    )
    .await;
    let error = match second {
        Ok(writer_b) => {
            writer_b
                .shutdown()
                .await
                .expect("close unexpected second writer");
            panic!("different physical blocks opened against one shared metadata authority");
        }
        Err(error) => error,
    };
    assert_eq!(error.code, mount_rs_core::ErrorCode::Estale);

    let metadata_connection = Connection::open(&metadata).expect("inspect bound metadata");
    let mode: String = metadata_connection
        .query_row(
            "SELECT write_mode FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("read persisted concurrent mode");
    assert_eq!(mode, "MRC2");
    let metadata_backing: String = metadata_connection
        .query_row(
            "SELECT backing_id FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("read bound metadata authority");
    let blocks_a_connection = Connection::open(&blocks_a).expect("inspect correct backing");
    let block_backing: String = blocks_a_connection
        .query_row(
            "SELECT backing_id FROM mount_rs_block_authority WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("read correct block authority");
    assert_eq!(metadata_backing, block_backing);
    let blocks_b_connection = Connection::open(&blocks_b).expect("inspect rejected blocks");
    let blocks_b_count: i64 = blocks_b_connection
        .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
        .expect("count wrong backing blocks");
    assert_eq!(blocks_b_count, 0);
    let authority_b_count: i64 = blocks_b_connection
        .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
            row.get(0)
        })
        .expect("count wrong backing authority markers");
    assert_eq!(
        authority_b_count, 0,
        "MRC2 reopen cannot claim a missing marker"
    );

    Loopback::new(writer_a.clone())
        .write_file("/from-a", b"shared metadata, private block bytes")
        .await
        .expect("writer A publishes one metadata reference");
    writer_a.shutdown().await.expect("close correct writer");
    let reopened = open_writer(&metadata, &blocks_a, 2).await;
    assert_eq!(
        Loopback::new(reopened.clone())
            .read_file("/from-a")
            .await
            .expect("correct backing still reads"),
        b"shared metadata, private block bytes"
    );
    reopened.shutdown().await.expect("close reopened writer");
}

#[tokio::test]
async fn missing_bound_block_marker_reopen_fails_without_recreating_it() {
    let scope = TempDir::new().expect("own disposable SQLite marker directory");
    let metadata = scope.path().join("metadata.sqlite");
    let blocks = scope.path().join("blocks.sqlite");
    let writer = open_writer(&metadata, &blocks, 0).await;
    Loopback::new(writer.clone())
        .write_file("/seed", b"committed")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let metadata_connection = Connection::open(&metadata).unwrap();
    let revision: i64 = metadata_connection
        .query_row(
            "SELECT revision FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let blocks_connection = Connection::open(&blocks).unwrap();
    blocks_connection
        .execute("DELETE FROM mount_rs_block_authority WHERE id=1", [])
        .unwrap();
    let result = ChunkedFs::open(
        SqliteMetadataStore::open(&metadata).unwrap(),
        SqliteBlockStore::open(&blocks).unwrap(),
        ChunkedOptions::fixed("missing-marker", 4096)
            .unwrap()
            .with_concurrent_writes(true),
    )
    .await;
    let error = match result {
        Ok(driver) => {
            driver.shutdown().await.unwrap();
            panic!("MRC2 reopen recreated a missing block marker");
        }
        Err(error) => error,
    };
    assert_eq!(error.code, mount_rs_core::ErrorCode::Estale);
    let marker_count: i64 = blocks_connection
        .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(marker_count, 0);
    let after: i64 = metadata_connection
        .query_row(
            "SELECT revision FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, revision);
}

#[tokio::test]
async fn changed_bound_block_marker_stops_next_publication() {
    let scope = TempDir::new().expect("own disposable SQLite tamper directory");
    let metadata = scope.path().join("metadata.sqlite");
    let blocks = scope.path().join("blocks.sqlite");
    let writer = open_writer(&metadata, &blocks, 0).await;
    let view = Loopback::new(writer.clone());
    view.write_file("/seed", b"prior commit").await.unwrap();
    let metadata_connection = Connection::open(&metadata).unwrap();
    let revision: i64 = metadata_connection
        .query_row(
            "SELECT revision FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let blocks_connection = Connection::open(&blocks).unwrap();
    let original: String = blocks_connection
        .query_row(
            "SELECT backing_id FROM mount_rs_block_authority WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    blocks_connection
        .execute(
            "UPDATE mount_rs_block_authority SET backing_id=?1 WHERE id=1",
            ["77777777777777777777777777777777"],
        )
        .unwrap();
    let error = view
        .write_file("/unpublished", b"must not commit")
        .await
        .unwrap_err();
    assert_eq!(error.code, mount_rs_core::ErrorCode::Estale);
    let after: i64 = metadata_connection
        .query_row(
            "SELECT revision FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        after, revision,
        "wrong backing cannot advance the namespace"
    );

    blocks_connection
        .execute(
            "UPDATE mount_rs_block_authority SET backing_id=?1 WHERE id=1",
            [&original],
        )
        .unwrap();
    let _ = writer.shutdown().await;
    let reopened = open_writer(&metadata, &blocks, 1).await;
    let reopened_view = Loopback::new(reopened.clone());
    assert_eq!(
        reopened_view.read_file("/seed").await.unwrap(),
        b"prior commit"
    );
    assert_eq!(
        reopened_view
            .read_file("/unpublished")
            .await
            .unwrap_err()
            .code,
        mount_rs_core::ErrorCode::Enoent
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn mrc1_open_rejects_before_claiming_a_block_authority() {
    let scope = TempDir::new().expect("own disposable old-writer directory");
    let metadata_path = scope.path().join("metadata.sqlite");
    let blocks_path = scope.path().join("blocks.sqlite");
    let writer = open_writer(&metadata_path, &blocks_path, 0).await;
    writer.shutdown().await.expect("stop disposable writer");
    stamp_owned_mrc1_fixture(&metadata_path, &blocks_path);

    let result = ChunkedFs::open(
        SqliteMetadataStore::open(&metadata_path).unwrap(),
        SqliteBlockStore::open(&blocks_path).unwrap(),
        ChunkedOptions::fixed("old-writer", 4096)
            .unwrap()
            .with_concurrent_writes(true),
    )
    .await;
    let error = match result {
        Ok(driver) => {
            driver.shutdown().await.unwrap();
            panic!("MRC1 opened without explicit offline migration");
        }
        Err(error) => error,
    };
    assert_eq!(error.code, mount_rs_core::ErrorCode::Ebusy);
    assert!(error.to_string().contains("migrate-concurrent-backing"));
    let blocks_connection = Connection::open(&blocks_path).unwrap();
    let markers: i64 = blocks_connection
        .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(markers, 0, "MRC1 rejection must not create a marker");
    let metadata_connection = Connection::open(&metadata_path).unwrap();
    let mode: String = metadata_connection
        .query_row(
            "SELECT write_mode FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mode, "MRC1");
}

#[tokio::test]
async fn mrc1_migration_binds_sqlite_blocks_without_changing_namespace() {
    let scope = TempDir::new().expect("own disposable MRC1 migration directory");
    let metadata_path = scope.path().join("metadata.sqlite");
    let blocks_path = scope.path().join("blocks.sqlite");
    let writer = open_writer(&metadata_path, &blocks_path, 0).await;
    Loopback::new(writer.clone())
        .write_file("/survives.bin", b"verified MRC1 block bytes")
        .await
        .expect("publish a referenced MRC1 block");
    writer.shutdown().await.expect("stop the old MRC1 writer");
    stamp_owned_mrc1_fixture(&metadata_path, &blocks_path);

    let metadata = SqliteMetadataStore::open(&metadata_path).expect("reopen MRC1 metadata");
    let blocks = SqliteBlockStore::open(&blocks_path).expect("reopen MRC1 blocks");
    let before = metadata.load().await.expect("load MRC1 namespace");
    assert_eq!(
        metadata
            .concurrent_mode_state()
            .await
            .expect("inspect MRC1 mode"),
        ConcurrentModeState::Mrc1
    );
    let original_json: String = Connection::open(&metadata_path)
        .expect("inspect owned metadata file")
        .query_row(
            "SELECT namespace FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("read exact MRC1 namespace bytes");

    let authority = migrate_mrc1_backing(&metadata, &blocks, before.revision)
        .await
        .expect("migrate verified MRC1 backing");
    assert_eq!(
        metadata
            .concurrent_mode_state()
            .await
            .expect("inspect MRC2 mode"),
        ConcurrentModeState::Mrc2(authority)
    );
    blocks
        .verify_concurrent_backing(authority)
        .await
        .expect("persist the selected block authority");
    assert_eq!(
        metadata.load().await.expect("reload metadata").revision,
        before.revision
    );
    let migrated_json: String = Connection::open(&metadata_path)
        .expect("inspect migrated metadata file")
        .query_row(
            "SELECT namespace FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("read exact MRC2 namespace bytes");
    assert_eq!(migrated_json, original_json);
}

async fn seeded_mrc1_volume() -> (TempDir, PathBuf, PathBuf, BlockId, u64) {
    let scope = TempDir::new().expect("own disposable MRC1 volume");
    let metadata_path = scope.path().join("metadata.sqlite");
    let blocks_path = scope.path().join("blocks.sqlite");
    let writer = open_writer(&metadata_path, &blocks_path, 0).await;
    Loopback::new(writer.clone())
        .write_file("/linked.bin", b"linked MRC1 immutable bytes")
        .await
        .expect("publish linked MRC1 file");
    writer.shutdown().await.expect("stop old MRC1 writer");
    stamp_owned_mrc1_fixture(&metadata_path, &blocks_path);
    let metadata = SqliteMetadataStore::open(&metadata_path).expect("inspect MRC1 metadata");
    let loaded = metadata.load().await.expect("load MRC1 namespace");
    let block = loaded
        .namespace
        .expect("initialized namespace")
        .nodes
        .values()
        .find_map(|node| match &node.data {
            NodeData::File(layout) => layout.extents.first().map(|extent| extent.block.clone()),
            _ => None,
        })
        .expect("linked file block reference");
    (scope, metadata_path, blocks_path, block, loaded.revision)
}

async fn assert_mrc1_unbound(metadata_path: &Path, revision: u64) {
    let metadata = SqliteMetadataStore::open(metadata_path).expect("reopen MRC1 metadata");
    assert_eq!(
        metadata
            .concurrent_mode_state()
            .await
            .expect("inspect write mode"),
        ConcurrentModeState::Mrc1
    );
    assert_eq!(
        metadata.load().await.expect("load revision").revision,
        revision
    );
    let stored: Option<String> = Connection::open(metadata_path)
        .expect("inspect owned metadata file")
        .query_row(
            "SELECT backing_id FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("read authority binding");
    assert!(
        stored.is_none(),
        "failed migration must not bind an authority"
    );
}

/// Simulate a second old writer completing a revision change after the
/// migration's direct block read and before its provider CAS.
struct RevisionAdvancingBlocks {
    inner: SqliteBlockStore,
    metadata_path: PathBuf,
    expected_revision: u64,
}

#[async_trait]
impl BlockStore for RevisionAdvancingBlocks {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn prepare_concurrent_backing(
        &self,
    ) -> mount_rs_core::Result<mount_rs_core::storage::ConcurrentBackingId> {
        self.inner.prepare_concurrent_backing().await
    }

    async fn verify_concurrent_backing(
        &self,
        expected: mount_rs_core::storage::ConcurrentBackingId,
    ) -> mount_rs_core::Result<()> {
        self.inner.verify_concurrent_backing(expected).await
    }

    async fn get_for_migration(&self, id: &BlockId) -> mount_rs_core::Result<Vec<u8>> {
        let bytes = self.inner.get_for_migration(id).await?;
        let changed = Connection::open(&self.metadata_path)
            .expect("open test-owned competing writer")
            .execute(
                "UPDATE mount_rs_metadata SET revision=revision+1
                 WHERE id=1 AND write_mode='MRC1' AND revision=?1",
                rusqlite::params![i64::try_from(self.expected_revision).unwrap()],
            )
            .expect("commit competing revision");
        assert_eq!(changed, 1, "race injects exactly one revision change");
        Ok(bytes)
    }

    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
        self.inner.put(bytes).await
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

#[tokio::test]
async fn mrc1_migration_rejects_missing_short_and_different_sqlite_blocks() {
    for fault in ["missing", "short", "different"] {
        let (scope, metadata_path, blocks_path, block, revision) = seeded_mrc1_volume().await;
        let metadata = SqliteMetadataStore::open(&metadata_path).expect("reopen MRC1 metadata");
        let selected_path = if fault == "different" {
            scope.path().join("different-blocks.sqlite")
        } else {
            blocks_path.clone()
        };
        let blocks = SqliteBlockStore::open(&selected_path).expect("open selected block database");
        match fault {
            "missing" => blocks
                .delete(&block)
                .await
                .expect("remove test-owned block"),
            "short" => {
                Connection::open(&selected_path)
                    .expect("corrupt test-owned block database")
                    .execute(
                        "UPDATE mount_rs_blocks SET bytes=X'00' WHERE id=?1",
                        rusqlite::params![block.0],
                    )
                    .expect("shorten referenced block");
            }
            "different" => {}
            _ => unreachable!(),
        }
        let error = migrate_mrc1_backing(&metadata, &blocks, revision)
            .await
            .expect_err("unreadable or too-short selected blocks must reject migration");
        assert_eq!(
            error.code,
            if fault == "short" {
                ErrorCode::Eio
            } else {
                ErrorCode::Enoent
            },
            "fault={fault}: {error:?}"
        );
        assert_mrc1_unbound(&metadata_path, revision).await;
    }
}

#[tokio::test]
async fn mrc1_migration_checks_unlinked_file_blocks() {
    let (_scope, metadata_path, blocks_path, _linked, revision) = seeded_mrc1_volume().await;
    let metadata = SqliteMetadataStore::open(&metadata_path).expect("reopen MRC1 metadata");
    let blocks = SqliteBlockStore::open(&blocks_path).expect("reopen MRC1 blocks");
    let orphan_bytes = b"retained unlinked MRC1 bytes";
    let orphan_block = blocks
        .put(orphan_bytes)
        .await
        .expect("store orphan-only block");
    let mut namespace = metadata
        .load()
        .await
        .expect("load MRC1 namespace")
        .namespace
        .expect("initialized namespace");
    let orphan_inode = namespace.next_inode;
    namespace.next_inode += 1;
    let mut orphan = namespace
        .nodes
        .values()
        .find(|node| matches!(node.data, NodeData::File(_)))
        .expect("linked file template")
        .clone();
    orphan.stats.ino = orphan_inode;
    orphan.stats.nlink = 0;
    orphan.stats.size = orphan_bytes.len() as u64;
    orphan.stats.blocks = orphan.stats.size.div_ceil(512);
    let NodeData::File(layout) = &mut orphan.data else {
        unreachable!()
    };
    layout.extents = vec![BlockExtent {
        file_offset: 0,
        block: orphan_block.clone(),
        block_offset: 0,
        length: orphan_bytes.len() as u64,
    }];
    namespace.nodes.insert(orphan_inode, orphan);
    namespace.validate().expect("valid retained unlinked file");
    let namespace_json = serde_json::to_string(&namespace).expect("encode owned legacy namespace");
    let changed = Connection::open(&metadata_path)
        .expect("open owned MRC1 metadata")
        .execute(
            "UPDATE mount_rs_metadata SET namespace=?1, revision=revision+1
             WHERE id=1 AND write_mode='MRC1' AND revision=?2",
            rusqlite::params![namespace_json, i64::try_from(revision).unwrap()],
        )
        .expect("publish test-owned retained unlinked inode");
    assert_eq!(changed, 1, "publish one legacy namespace revision");
    let next_revision = revision + 1;
    blocks
        .delete(&orphan_block)
        .await
        .expect("remove only the orphan block");
    let error = migrate_mrc1_backing(&metadata, &blocks, next_revision)
        .await
        .expect_err("the orphan-only block must be scanned");
    assert_eq!(error.code, ErrorCode::Enoent);
    assert_mrc1_unbound(&metadata_path, next_revision).await;
}

#[tokio::test]
async fn mrc1_migration_rejects_changed_revision_and_version_history() {
    let (_scope, metadata_path, blocks_path, _block, revision) = seeded_mrc1_volume().await;
    let metadata = SqliteMetadataStore::open(&metadata_path).expect("reopen MRC1 metadata");
    let blocks = SqliteBlockStore::open(&blocks_path).expect("reopen MRC1 blocks");
    let error = migrate_mrc1_backing(&metadata, &blocks, revision - 1)
        .await
        .expect_err("stale expected revision must reject migration");
    assert_eq!(error.code, ErrorCode::Eagain);
    assert_mrc1_unbound(&metadata_path, revision).await;
    let claimed: i64 = Connection::open(&blocks_path)
        .expect("inspect owned block database")
        .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
            row.get(0)
        })
        .expect("count authority rows");
    assert_eq!(claimed, 0, "revision conflict must precede block claim");

    Connection::open(&metadata_path)
        .expect("add test-owned version record")
        .execute(
            "INSERT INTO mount_rs_versions
             (id,volume_id,sequence,namespace,block_store_id,kind,created_at_ms,durable,operation_id)
             VALUES ('fixture-version', (SELECT volume_id FROM mount_rs_metadata WHERE id=1), 1,
                     (SELECT namespace FROM mount_rs_metadata WHERE id=1), 'fixture-blocks',
                     'initial', 0, 1, 'fixture-operation')",
            [],
        )
        .expect("retain a historical version row");
    let error = migrate_mrc1_backing(&metadata, &blocks, revision)
        .await
        .expect_err("history must prevent migration");
    assert_eq!(error.code, ErrorCode::Ebusy);
    assert_mrc1_unbound(&metadata_path, revision).await;
}

#[tokio::test]
async fn mrc1_migration_cas_rejects_revision_changed_during_scan() {
    let (_scope, metadata_path, blocks_path, _block, revision) = seeded_mrc1_volume().await;
    let metadata = SqliteMetadataStore::open(&metadata_path).expect("reopen MRC1 metadata");
    let blocks = RevisionAdvancingBlocks {
        inner: SqliteBlockStore::open(&blocks_path).expect("reopen MRC1 blocks"),
        metadata_path: metadata_path.clone(),
        expected_revision: revision,
    };
    let error = migrate_mrc1_backing(&metadata, &blocks, revision)
        .await
        .expect_err("provider CAS must reject a concurrent revision change");
    assert_eq!(error.code, ErrorCode::Eagain);
    assert_mrc1_unbound(&metadata_path, revision + 1).await;
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
