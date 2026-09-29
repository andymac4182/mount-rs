//! Actual SQLite ownership at the public observed SDK construction boundary.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use mount_rs_core::construction::ConstructionObserver;
use mount_rs_core::storage::BlockStore;
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
use mount_rs_core::storage::MetadataStore;
use mount_rs_core::{ErrorCode, FsError, Result};
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
use mount_rs_sdk::StorageContext;
use mount_rs_sdk::{
    BlockStoreDecorator, ConstructionJournal, Filesystem, HostOptions, MemoryOptions, SplitOptions,
    StoreConfig,
};
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
#[cfg(any(
    windows,
    target_os = "macos",
    all(target_os = "linux", target_env = "gnu")
))]
use rusqlite::Connection;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sdk-observed-construction-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn options(&self, compact: bool) -> SplitOptions {
        let mut options = SplitOptions::memory("sdk-observed-owner", 4096);
        options.metadata = StoreConfig::Sqlite {
            path: self.0.join("metadata.db"),
        };
        options.blocks = StoreConfig::Sqlite {
            path: self.0.join("blocks.db"),
        };
        if compact {
            options = options.with_compact_inode_updates(true);
        }
        options
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

// Keep native fixture files if cleanup or ownership release is unacknowledged.
#[cfg(windows)]
struct WindowsRefusalFixture {
    fixture: Option<Fixture>,
    cleanup_acknowledged: bool,
}

#[cfg(windows)]
impl WindowsRefusalFixture {
    fn new() -> Self {
        Self {
            fixture: Some(Fixture::new()),
            cleanup_acknowledged: false,
        }
    }

    fn fixture(&self) -> &Fixture {
        self.fixture.as_ref().unwrap()
    }

    fn acknowledge_cleanup(&mut self) {
        self.cleanup_acknowledged = true;
    }
}

#[cfg(windows)]
impl Drop for WindowsRefusalFixture {
    fn drop(&mut self) {
        if !self.cleanup_acknowledged
            && let Some(fixture) = self.fixture.take()
        {
            std::mem::forget(fixture);
        }
    }
}

#[derive(Default)]
struct CaptureBlocks {
    owner: Mutex<Option<Weak<dyn BlockStore>>>,
    reject: bool,
}

impl CaptureBlocks {
    fn alive(&self) -> bool {
        self.owner
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
    }
}

impl BlockStoreDecorator for CaptureBlocks {
    fn decorate(
        &self,
        _: &StoreConfig,
        blocks: Arc<dyn BlockStore>,
    ) -> Result<Arc<dyn BlockStore>> {
        *self.owner.lock().unwrap() = Some(Arc::downgrade(&blocks));
        if self.reject {
            Err(FsError::new(ErrorCode::Eio))
        } else {
            Ok(blocks)
        }
    }
}

async fn full_bytes(filesystem: &Filesystem, path: &str, expected: &[u8]) {
    let handle = filesystem.driver().open(path, "r", 0).await.unwrap();
    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let count = handle
            .read(&mut actual[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0, "unexpected early EOF");
        assert!(count <= actual.len() - offset, "oversized read");
        offset += count;
    }
    assert_eq!(actual, expected);
    assert_eq!(
        handle
            .read(&mut [0; 17], Some(expected.len() as u64))
            .await
            .unwrap(),
        0
    );
    handle.close().await.unwrap();
}

#[tokio::test]
async fn invalid_split_options_register_no_owner() {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let mut options = SplitOptions::memory("invalid-observed-open", 4096);
    options.chunk_size_bytes = 0;
    let result = Filesystem::split_with_construction_observer(options, None, None, &journal).await;
    assert!(matches!(result, Err(error) if error.code == ErrorCode::Einval));
    attempt.fail();
    assert_eq!(journal.snapshot().retained_resources, 0);
    journal.close().await.unwrap();
    assert!(journal.snapshot().cleanup_complete);
}

#[tokio::test]
async fn decorator_failure_keeps_real_block_owner_until_owned_cleanup() {
    let fixture = Fixture::new();
    let captured = CaptureBlocks {
        reject: true,
        ..CaptureBlocks::default()
    };
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let result = Filesystem::split_with_construction_observer(
        fixture.options(true),
        None,
        Some(&captured),
        &journal,
    )
    .await;
    assert!(matches!(result, Err(error) if error.code == ErrorCode::Eio));
    attempt.fail();
    assert!(
        captured.alive(),
        "actual block provider escaped the journal"
    );
    assert!(journal.snapshot().retained_resources > 0);
    journal.close().await.unwrap();
    assert!(journal.snapshot().cleanup_complete);
    assert!(
        !captured.alive(),
        "acknowledged cleanup kept the provider alive"
    );
}

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
#[tokio::test]
async fn observed_handoff_retains_actual_owner_and_persisted_compact_backing() {
    let fixture = Fixture::new();
    let captured = CaptureBlocks::default();
    let context = StorageContext::new(2).unwrap();
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let filesystem = Filesystem::split_with_construction_observer(
        fixture.options(true),
        Some(&context),
        Some(&captured),
        &journal,
    )
    .await
    .unwrap();
    assert!(captured.alive());
    assert!(journal.snapshot().retained_resources >= 2);
    assert!(filesystem.persistent_eviction_allowed());
    let backing = filesystem.concurrent_backing_id().unwrap();
    let payload: Vec<_> = (0..8193).map(|i| ((i * 37 + i / 17) % 251) as u8).collect();
    filesystem
        .driver()
        .write_file("/acknowledged", &payload)
        .await
        .unwrap();
    attempt.handoff().unwrap();
    assert!(journal.snapshot().handed_off);
    assert_eq!(journal.snapshot().retained_resources, 0);
    assert!(
        captured.alive(),
        "handoff dropped the actual provider owner"
    );
    full_bytes(&filesystem, "/acknowledged", &payload).await;
    filesystem.shutdown().await.unwrap();
    drop(filesystem);
    assert!(!captured.alive());

    let reopened_journal = ConstructionJournal::new();
    let reopened_attempt = reopened_journal.begin().unwrap();
    let reopened = Filesystem::split_with_context_and_construction_observer(
        fixture.options(true),
        &context,
        &reopened_journal,
    )
    .await
    .unwrap();
    reopened_attempt.handoff().unwrap();
    assert_eq!(reopened.concurrent_backing_id(), Some(backing));
    full_bytes(&reopened, "/acknowledged", &payload).await;
    let metadata = SqliteMetadataStore::open(fixture.0.join("metadata.db")).unwrap();
    assert_eq!(
        metadata
            .compact_inode_mode_state()
            .await
            .unwrap()
            .unwrap()
            .backing,
        backing
    );
    let blocks = SqliteBlockStore::open(fixture.0.join("blocks.db")).unwrap();
    blocks.verify_concurrent_backing(backing).await.unwrap();
    reopened.shutdown().await.unwrap();
    drop((metadata, blocks, reopened));
    context.close().await.unwrap();
}

#[tokio::test]
async fn eviction_qualification_rejects_volatile_and_noncompact_owners() {
    assert!(!Filesystem::memory(MemoryOptions::default()).persistent_eviction_allowed());
    let fixture = Fixture::new();
    assert!(!Filesystem::host(&fixture.0, HostOptions::default()).persistent_eviction_allowed());
    let sqlite = Filesystem::sqlite(fixture.0.join("snapshot.db"))
        .await
        .unwrap();
    assert!(!sqlite.persistent_eviction_allowed());
    sqlite.shutdown().await.unwrap();
    drop(sqlite);
    let exclusive = Filesystem::split(fixture.options(false)).await.unwrap();
    assert!(!exclusive.persistent_eviction_allowed());
    exclusive.shutdown().await.unwrap();
    drop(exclusive);
    let volatile = Filesystem::split(SplitOptions::memory("volatile-observed", 4096))
        .await
        .unwrap();
    assert!(!volatile.persistent_eviction_allowed());
    volatile.shutdown().await.unwrap();
    drop(volatile);
}

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
#[tokio::test]
async fn observed_failed_publication_rejects_provider_shutdown_and_preserves_old_bytes() {
    let fixture = Fixture::new();
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let filesystem =
        Filesystem::split_with_construction_observer(fixture.options(true), None, None, &journal)
            .await
            .unwrap();
    attempt.handoff().unwrap();
    let backing = filesystem.concurrent_backing_id().unwrap();
    let original: Vec<_> = (0..8193).map(|i| ((i * 41 + i / 31) % 251) as u8).collect();
    filesystem
        .driver()
        .write_file("/acknowledged", &original)
        .await
        .unwrap();
    full_bytes(&filesystem, "/acknowledged", &original).await;
    let handle = filesystem
        .driver()
        .open("/acknowledged", "r+", 0)
        .await
        .unwrap();
    let connection = Connection::open(fixture.0.join("metadata.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_observed_sdk_publication
         BEFORE UPDATE ON mount_rs_compact_guards
         BEGIN
             SELECT RAISE(ABORT, 'observed SDK publication rejected');
         END;",
        )
        .unwrap();
    let error = handle.write(&[97; 4096], Some(4096)).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Eio);
    assert!(
        error
            .to_string()
            .contains("observed SDK publication rejected")
    );
    assert!(filesystem.failed());
    assert!(!filesystem.persistent_eviction_allowed());
    handle.close().await.unwrap();
    assert!(
        matches!(filesystem.shutdown().await, Err(error) if error.code == ErrorCode::Eio),
        "an observed failed authority must prevent provider shutdown",
    );
    assert!(filesystem.failed());
    assert_eq!(filesystem.concurrent_backing_id(), Some(backing));
    connection
        .execute_batch("DROP TRIGGER reject_observed_sdk_publication")
        .unwrap();
    drop(connection);

    // The fixture explicitly opens a reader; no runtime replacement is inferred.
    let fresh = Filesystem::split(fixture.options(true)).await.unwrap();
    assert_eq!(fresh.concurrent_backing_id(), Some(backing));
    full_bytes(&fresh, "/acknowledged", &original).await;
    fresh.shutdown().await.unwrap();
    assert!(filesystem.failed());
    drop((fresh, filesystem));
}

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
#[tokio::test]
async fn retained_unobserved_split_resource_rejects_failed_authority() {
    let fixture = Fixture::new();
    let filesystem = Filesystem::split(fixture.options(true)).await.unwrap();
    let backing = filesystem.concurrent_backing_id().unwrap();
    let original: Vec<_> = (0..8193).map(|i| ((i * 47 + i / 29) % 251) as u8).collect();
    filesystem
        .driver()
        .write_file("/acknowledged", &original)
        .await
        .unwrap();
    let handle = filesystem
        .driver()
        .open("/acknowledged", "r+", 0)
        .await
        .unwrap();
    let connection = Connection::open(fixture.0.join("metadata.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_retained_sdk_publication
         BEFORE UPDATE ON mount_rs_compact_guards
         BEGIN SELECT RAISE(ABORT, 'retained SDK publication rejected'); END;",
        )
        .unwrap();
    let error = handle.write(&[83; 4096], Some(4096)).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Eio);
    assert!(
        error
            .to_string()
            .contains("retained SDK publication rejected")
    );
    handle.close().await.unwrap();
    assert!(filesystem.failed());
    let error = mount_rs_core::construction::ConstructionResource::close(&filesystem)
        .await
        .expect_err("retained unobserved split resource accepted failed authority");
    assert_eq!(error.code, ErrorCode::Eio);
    assert!(filesystem.failed());

    // Direct legacy shutdown retains its old behavior. Retaining the returned
    // owner as a construction resource must use the stronger authority barrier.
    filesystem.shutdown().await.unwrap();
    assert!(filesystem.failed());
    connection
        .execute_batch("DROP TRIGGER reject_retained_sdk_publication")
        .unwrap();
    drop(connection);
    let fresh = Filesystem::split(fixture.options(true)).await.unwrap();
    assert_eq!(fresh.concurrent_backing_id(), Some(backing));
    full_bytes(&fresh, "/acknowledged", &original).await;
    fresh.shutdown().await.unwrap();
    drop((fresh, filesystem));
}

#[cfg(windows)]
#[tokio::test]
async fn observed_compact_sqlite_refusal_retains_owners_until_cleanup_without_publication() {
    let mut fixture = WindowsRefusalFixture::new();
    let captured = CaptureBlocks::default();
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let result = Filesystem::split_with_construction_observer(
        fixture.fixture().options(true),
        None,
        Some(&captured),
        &journal,
    )
    .await;
    let error = match result {
        Err(error) => error,
        Ok(filesystem) => {
            attempt.handoff().unwrap();
            filesystem.shutdown().await.unwrap();
            drop(filesystem);
            assert!(!captured.alive());
            fixture.acknowledge_cleanup();
            panic!("unsupported Windows compact SQLite construction unexpectedly succeeded");
        }
    };
    let before_cleanup = journal.snapshot();
    let owner_alive_before_cleanup = captured.alive();
    attempt.fail();
    let cleanup = journal.close().await;
    let after_cleanup = journal.snapshot();
    let owner_alive_after_cleanup = captured.alive();

    // Settle owned cleanup before asserting the refusal or opening inspections.
    cleanup.unwrap();
    assert!(after_cleanup.cleanup_complete);
    assert!(!after_cleanup.uncertain);
    assert_eq!(after_cleanup.retained_resources, 0);
    assert!(!owner_alive_after_cleanup);
    fixture.acknowledge_cleanup();
    assert_eq!(error.code, ErrorCode::Enotsup);
    assert!(before_cleanup.opening);
    assert!(!before_cleanup.uncertain);
    assert_eq!(before_cleanup.retained_resources, 2);
    assert!(owner_alive_before_cleanup);

    // Constructors create schemas; unsupported compact mode must not publish
    // metadata, claim block authority, or write content. Read-only connections
    // inspect those existing databases after the actual owners are released.
    let metadata = Connection::open_with_flags(
        fixture.fixture().0.join("metadata.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let blocks = Connection::open_with_flags(
        fixture.fixture().0.join("blocks.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    type StoredMetadataRow = (
        i64,
        Option<String>,
        Option<String>,
        i64,
        i64,
        Option<String>,
        Option<String>,
    );
    let stored_metadata: StoredMetadataRow = metadata
        .query_row(
            "SELECT revision, namespace, owner, fence, expires, write_mode, backing_id
             FROM mount_rs_metadata WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    let compact_rows: i64 = metadata
        .query_row("SELECT count(*) FROM mount_rs_compact_guards", [], |row| {
            row.get(0)
        })
        .unwrap();
    let inode_rows: i64 = metadata
        .query_row("SELECT count(*) FROM mount_rs_inode_guards", [], |row| {
            row.get(0)
        })
        .unwrap();
    let block_authority_rows: i64 = blocks
        .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
            row.get(0)
        })
        .unwrap();
    let content_rows: i64 = blocks
        .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
        .unwrap();
    let metadata_closed = metadata.close().map_err(|(_, error)| error);
    let blocks_closed = blocks.close().map_err(|(_, error)| error);
    metadata_closed.unwrap();
    blocks_closed.unwrap();
    assert_eq!(stored_metadata, (0, None, None, 0, 0, None, None));
    assert_eq!(compact_rows, 0);
    assert_eq!(inode_rows, 0);
    assert_eq!(block_authority_rows, 0);
    assert_eq!(content_rows, 0);
}

#[tokio::test]
async fn actual_snapshot_filesystem_owner_survives_post_open_work_until_cleanup() {
    let fixture = Fixture::new();
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let filesystem = Arc::new(
        Filesystem::sqlite(fixture.0.join("snapshot.db"))
            .await
            .unwrap(),
    );
    let owner = Arc::downgrade(&filesystem);
    journal.retain(filesystem.clone());
    let payload: Vec<_> = (0..8193).map(|i| ((i * 43 + i / 23) % 251) as u8).collect();
    filesystem
        .driver()
        .write_file("/acknowledged", &payload)
        .await
        .unwrap();
    drop(filesystem);
    attempt.fail();
    assert!(
        owner.upgrade().is_some(),
        "actual snapshot owner escaped postconfiguration"
    );
    journal.close().await.unwrap();
    assert!(journal.snapshot().cleanup_complete);
    assert!(
        owner.upgrade().is_none(),
        "cleanup retained the actual snapshot owner"
    );
    let fresh = Filesystem::sqlite(fixture.0.join("snapshot.db"))
        .await
        .unwrap();
    full_bytes(&fresh, "/acknowledged", &payload).await;
    fresh.shutdown().await.unwrap();
    drop(fresh);
}
