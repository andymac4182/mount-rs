//! Bounded library prerequisites for actual persistent SQLite runtime eviction.
//! This local topology does not substitute for the full production target.

#![cfg(unix)]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::storage::{BlockStore, ConcurrentBackingId, MetadataStore};
use mount_rs_core::{ErrorCode, FsDriver, Result};
use mount_rs_sdk::{ConstructionJournal, Filesystem, SplitOptions, StorageContext, StoreConfig};
use mount_rs_service::runtime_pool::{DriveRegistration, ManagedDrive, RuntimeLease, RuntimePool};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const DEADLINE: Duration = Duration::from_secs(10);
const CHUNK: usize = 4096;
const FILE: &str = "/acknowledged";

fn options(path: &Path, owner: &str, compact: bool) -> SplitOptions {
    let store = StoreConfig::Sqlite {
        path: path.to_owned(),
    };
    let mut options = SplitOptions::memory(owner, CHUNK).with_inode_updates(true);
    if compact {
        options = options.with_compact_inode_updates(true);
    }
    options.metadata = store.clone();
    options.blocks = store;
    options
}

fn payload(seed: usize) -> Vec<u8> {
    (0..64 * 1024 + 173)
        .map(|offset| ((offset * 19 + seed * 37 + offset / 101) % 256) as u8)
        .collect()
}

async fn full_file(driver: &Arc<dyn FsDriver>, expected: &[u8]) {
    full_file_at(driver, FILE, expected).await;
}

async fn full_file_at(driver: &Arc<dyn FsDriver>, path: &str, expected: &[u8]) {
    let handle = driver.open(path, "r", 0).await.unwrap();
    assert_eq!(handle.stat().await.unwrap().size, expected.len() as u64);
    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let end = actual.len().min(offset + 1379);
        let read = handle
            .read(&mut actual[offset..end], Some(offset as u64))
            .await
            .unwrap();
        assert!(
            read > 0 && read <= end - offset,
            "invalid positioned full-file read"
        );
        offset += read;
    }
    assert_eq!(actual, expected, "acknowledged full payload changed");
    assert_eq!(
        handle
            .read(&mut [0; 17], Some(expected.len() as u64))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        handle
            .read(&mut [0; 17], Some(expected.len() as u64 + 19))
            .await
            .unwrap(),
        0
    );
    handle.close().await.unwrap();
}

async fn stored_backing(path: &Path, compact: bool) -> ConcurrentBackingId {
    let metadata = SqliteMetadataStore::open(path).unwrap();
    assert!(metadata.durable());
    let backing = if compact {
        metadata
            .compact_inode_mode_state()
            .await
            .unwrap()
            .expect("actual MRC5 marker missing")
            .backing
    } else {
        assert!(metadata.compact_inode_mode_state().await.unwrap().is_none());
        metadata
            .inode_mode_state()
            .await
            .unwrap()
            .expect("actual MRC4 marker missing")
            .backing
    };
    let blocks = SqliteBlockStore::open(path).unwrap();
    assert!(blocks.durable());
    blocks.verify_concurrent_backing(backing).await.unwrap();
    backing
}

async fn acquire(registration: &DriveRegistration) -> Result<RuntimeLease> {
    tokio::time::timeout(DEADLINE, registration.acquire())
        .await
        .expect("runtime admission stalled")
}

async fn denied(registration: &DriveRegistration, expected: ErrorCode) {
    let error = match acquire(registration).await {
        Ok(_) => panic!("unexpected runtime admission"),
        Err(error) => error,
    };
    assert_eq!(error.code, expected);
}

async fn retired(owner: &Weak<SqliteRuntime>) {
    tokio::time::timeout(DEADLINE, async {
        while owner.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("closed generation retained its actual runtime owner");
}

struct SqliteRuntime {
    filesystem: Filesystem,
    shutdowns: Arc<AtomicUsize>,
}

#[async_trait]
impl ManagedDrive for SqliteRuntime {
    fn driver(&self) -> Arc<dyn FsDriver> {
        self.filesystem.driver()
    }
    fn failed(&self) -> bool {
        self.filesystem.failed()
    }
    fn eviction_allowed(&self) -> bool {
        self.filesystem.persistent_eviction_allowed()
    }
    fn concurrent_backing_id(&self) -> Option<ConcurrentBackingId> {
        self.filesystem.concurrent_backing_id()
    }
    async fn shutdown(&self) -> Result<()> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        self.filesystem.shutdown().await
    }
}

struct SqliteFactory {
    options: SplitOptions,
    context: Arc<StorageContext>,
    opens: AtomicUsize,
    shutdowns: Arc<AtomicUsize>,
    owners: Mutex<Vec<Weak<SqliteRuntime>>>,
    journals: Mutex<Vec<ConstructionJournal>>,
    // If handoff itself is uncertain, the returned actual owner must survive
    // the factory's Err rather than becoming a dropped local variable.
    uncertain_owners: Mutex<Vec<Arc<SqliteRuntime>>>,
}

impl SqliteFactory {
    fn new(options: SplitOptions, context: Arc<StorageContext>) -> Arc<Self> {
        Arc::new(Self {
            options,
            context,
            opens: AtomicUsize::new(0),
            shutdowns: Arc::new(AtomicUsize::new(0)),
            owners: Mutex::new(Vec::new()),
            journals: Mutex::new(Vec::new()),
            uncertain_owners: Mutex::new(Vec::new()),
        })
    }
    fn opened(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }
    fn closed(&self) -> usize {
        self.shutdowns.load(Ordering::SeqCst)
    }
    fn current(&self) -> Weak<SqliteRuntime> {
        self.owners.lock().unwrap().last().unwrap().clone()
    }
    fn assert_handoffs(&self) {
        assert!(self.uncertain_owners.lock().unwrap().is_empty());
        let journals = self.journals.lock().unwrap();
        assert_eq!(journals.len(), self.opened());
        for journal in journals.iter() {
            let snapshot = journal.snapshot();
            assert!(snapshot.handed_off && !snapshot.uncertain);
            assert_eq!(
                snapshot.retained_resources, 0,
                "successful journal retained a generation"
            );
        }
    }
}

#[async_trait]
impl mount_rs_service::runtime_pool::RuntimeFactory for SqliteFactory {
    async fn open(&self) -> Result<Arc<dyn ManagedDrive>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        let journal = ConstructionJournal::new();
        // The factory owns the attempt before its first constructor poll.
        self.journals.lock().unwrap().push(journal.clone());
        let attempt = journal.begin()?;
        let opened = Filesystem::split_with_context_and_construction_observer(
            self.options.clone(),
            &self.context,
            &journal,
        )
        .await;
        let filesystem = match opened {
            Ok(filesystem) => filesystem,
            Err(error) => {
                attempt.fail();
                return Err(error);
            }
        };
        let owner = Arc::new(SqliteRuntime {
            filesystem,
            shutdowns: self.shutdowns.clone(),
        });
        self.uncertain_owners.lock().unwrap().push(owner.clone());
        attempt.handoff()?;
        self.uncertain_owners
            .lock()
            .unwrap()
            .retain(|held| !Arc::ptr_eq(held, &owner));
        self.owners.lock().unwrap().push(Arc::downgrade(&owner));
        Ok(owner)
    }
}

async fn sibling(options: SplitOptions, context: &StorageContext) -> Filesystem {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let filesystem =
        Filesystem::split_with_context_and_construction_observer(options, context, &journal)
            .await
            .unwrap();
    attempt.handoff().unwrap();
    assert!(journal.snapshot().handed_off);
    filesystem
}

struct Fixture {
    directory: TempDir,
    context: Arc<StorageContext>,
    pool: RuntimePool,
    registrations: Vec<DriveRegistration>,
    factories: Vec<Arc<SqliteFactory>>,
    sibling: Filesystem,
    sibling_bytes: Vec<u8>,
    sibling_progress: AtomicUsize,
}

impl Fixture {
    async fn new(capacity: usize, count: usize) -> Self {
        Self::with_format(capacity, count, true).await
    }
    async fn with_format(capacity: usize, count: usize, compact: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let context = Arc::new(StorageContext::new(2).unwrap());
        let pool = RuntimePool::new(capacity).unwrap();
        let mut factories = Vec::new();
        let mut registrations = Vec::new();
        for index in 0..count {
            let path = directory.path().join(format!("drive-{index}.sqlite"));
            let factory = SqliteFactory::new(
                options(&path, &format!("drive-{index}"), compact),
                context.clone(),
            );
            registrations.push(pool.register(factory.clone()).unwrap());
            factories.push(factory);
            assert!(!path.exists(), "registration eagerly opened SQLite storage");
        }
        let sibling = sibling(
            options(
                &directory.path().join("context-sibling.sqlite"),
                "sibling",
                true,
            ),
            &context,
        )
        .await;
        let sibling_bytes = payload(200);
        sibling
            .driver()
            .write_file(FILE, &sibling_bytes)
            .await
            .unwrap();
        assert_eq!(pool.snapshot().resident, 0);
        assert!(factories.iter().all(|factory| factory.opened() == 0));
        Self {
            directory,
            context,
            pool,
            registrations,
            factories,
            sibling,
            sibling_bytes,
            sibling_progress: AtomicUsize::new(0),
        }
    }
    fn path(&self, index: usize) -> std::path::PathBuf {
        self.directory.path().join(format!("drive-{index}.sqlite"))
    }
    async fn sibling_usable(&self) {
        full_file(&self.sibling.driver(), &self.sibling_bytes).await;
        let progress = self
            .sibling_progress
            .fetch_add(1, Ordering::SeqCst)
            .to_le_bytes();
        self.sibling
            .driver()
            .write_file("/progress", &progress)
            .await
            .unwrap();
        full_file_at(&self.sibling.driver(), "/progress", &progress).await;
    }
    async fn finish_healthy(&self) {
        tokio::time::timeout(DEADLINE, self.pool.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(self.pool.snapshot().resident, 0);
        self.sibling_usable().await;
        for factory in &self.factories {
            factory.assert_handoffs();
            if factory.opened() != 0 {
                retired(&factory.current()).await;
            }
            assert_eq!(factory.closed(), factory.opened());
        }
        self.sibling.shutdown().await.unwrap();
        self.context.close().await.unwrap();
    }
}

#[tokio::test]
async fn actual_mrc5_bytes_eof_and_backing_survive_repeated_owned_eviction() {
    let fixture = Fixture::new(1, 3).await;
    let expected: Vec<_> = (0..3).map(payload).collect();
    let mut identities = [None; 3];
    let mut previous = None;
    for index in [0, 1, 2, 0, 2, 1, 0, 1, 2, 0] {
        let lease = acquire(&fixture.registrations[index]).await.unwrap();
        if let Some(previous) = previous.take() {
            retired(&previous).await;
        }
        let owner = fixture.factories[index].current();
        assert!(
            owner
                .upgrade()
                .unwrap()
                .filesystem
                .persistent_eviction_allowed()
        );
        let actual = owner
            .upgrade()
            .unwrap()
            .filesystem
            .concurrent_backing_id()
            .unwrap();
        if identities[index].is_none() {
            lease
                .driver()
                .write_file(FILE, &expected[index])
                .await
                .unwrap();
            identities[index] = Some(actual);
        }
        assert_eq!(Some(actual), identities[index]);
        full_file(lease.driver(), &expected[index]).await;
        assert_eq!(stored_backing(&fixture.path(index), true).await, actual);
        drop(lease);
        previous = Some(owner);
        let snapshot = fixture.pool.snapshot();
        assert_eq!(snapshot.resident, 1);
        assert_eq!(snapshot.pinned, 0);
        assert_eq!(snapshot.quarantined, 0);
        fixture.sibling_usable().await;
    }
    let snapshot = fixture.pool.snapshot();
    assert_eq!(snapshot.registered, 3);
    assert_eq!(snapshot.open_success, 10);
    assert_eq!(snapshot.eviction_success, 9);
    assert_eq!(snapshot.eviction_error, 0);
    assert_eq!(
        fixture
            .factories
            .iter()
            .map(|factory| factory.opened())
            .collect::<Vec<_>>(),
        [4, 3, 3]
    );
    for (index, identity) in identities.into_iter().enumerate() {
        assert_eq!(
            stored_backing(&fixture.path(index), true).await,
            identity.unwrap()
        );
    }
    fixture.finish_healthy().await;
}

#[tokio::test]
async fn actual_mrc5_cloned_request_lease_prevents_eviction_without_opening_target() {
    let fixture = Fixture::new(1, 2).await;
    let first = acquire(&fixture.registrations[0]).await.unwrap();
    let expected = payload(10);
    first.driver().write_file(FILE, &expected).await.unwrap();
    let backing = stored_backing(&fixture.path(0), true).await;
    let owner = fixture.factories[0].current();
    let held = first.clone();
    drop(first);
    denied(&fixture.registrations[1], ErrorCode::Ebusy).await;
    assert_eq!(fixture.pool.snapshot().pinned, 1);
    assert_eq!(fixture.factories[1].opened(), 0);
    assert_eq!(fixture.factories[0].closed(), 0);
    full_file(held.driver(), &expected).await;
    fixture.sibling_usable().await;
    drop(held);
    drop(acquire(&fixture.registrations[1]).await.unwrap());
    retired(&owner).await;
    let reopened = acquire(&fixture.registrations[0]).await.unwrap();
    full_file(reopened.driver(), &expected).await;
    assert_eq!(stored_backing(&fixture.path(0), true).await, backing);
    assert_eq!(fixture.factories[0].opened(), 2);
    drop(reopened);
    fixture.finish_healthy().await;
}

#[tokio::test]
async fn actual_mrc5_handle_pin_keeps_mutations_and_sparse_eof_through_reopen() {
    let fixture = Fixture::new(1, 2).await;
    let lease = acquire(&fixture.registrations[0]).await.unwrap();
    let mut expected = payload(20);
    lease.driver().write_file(FILE, &expected).await.unwrap();
    let backing = stored_backing(&fixture.path(0), true).await;
    let owner = fixture.factories[0].current();
    let handle = lease.wrap_handle(lease.driver().open(FILE, "r+", 0).await.unwrap());
    let held = handle.clone();
    drop(lease);
    drop(handle);
    denied(&fixture.registrations[1], ErrorCode::Ebusy).await;
    assert_eq!(fixture.factories[1].opened(), 0);
    assert_eq!(fixture.pool.snapshot().pinned, 1);
    let replacement = payload(99);
    let at = CHUNK - 11;
    let amount = CHUNK + 29;
    assert_eq!(
        held.write(&replacement[..amount], Some(at as u64))
            .await
            .unwrap(),
        amount
    );
    expected[at..at + amount].copy_from_slice(&replacement[..amount]);
    let append_at = expected.len() + 17;
    assert_eq!(
        held.write(&replacement[..31], Some(append_at as u64))
            .await
            .unwrap(),
        31
    );
    expected.resize(append_at, 0);
    expected.extend_from_slice(&replacement[..31]);
    assert_eq!(held.stat().await.unwrap().size, expected.len() as u64);
    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let count = held
            .read(&mut actual[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0 && count <= actual.len() - offset);
        offset += count;
    }
    assert_eq!(actual, expected);
    assert_eq!(
        held.read(&mut [0], Some(expected.len() as u64))
            .await
            .unwrap(),
        0
    );
    held.sync().await.unwrap();
    fixture.sibling_usable().await;
    held.close().await.unwrap();
    drop(held);
    drop(acquire(&fixture.registrations[1]).await.unwrap());
    retired(&owner).await;
    let reopened = acquire(&fixture.registrations[0]).await.unwrap();
    full_file(reopened.driver(), &expected).await;
    assert_eq!(stored_backing(&fixture.path(0), true).await, backing);
    drop(reopened);
    fixture.finish_healthy().await;
}

fn guard_row(connection: &Connection, inode: u64) -> (i64, i64, i64, String) {
    connection
        .query_row(
            "SELECT incarnation,epoch,revision,node FROM mount_rs_compact_guards WHERE inode=?1",
            params![inode.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

#[tokio::test]
async fn actual_mrc5_failed_publication_quarantines_owner_without_replacement_generation() {
    let fixture = Fixture::new(2, 3).await;
    let first = acquire(&fixture.registrations[0]).await.unwrap();
    first.driver().write_file(FILE, &payload(30)).await.unwrap();
    full_file(first.driver(), &payload(30)).await;
    let backing = stored_backing(&fixture.path(0), true).await;
    let failed_owner = fixture.factories[0].current();
    let handle = first.wrap_handle(first.driver().open(FILE, "r+", 0).await.unwrap());
    let inode = handle.stat().await.unwrap().ino;
    let sibling_lease = acquire(&fixture.registrations[1]).await.unwrap();
    let healthy_bytes = payload(31);
    sibling_lease
        .driver()
        .write_file(FILE, &healthy_bytes)
        .await
        .unwrap();
    let healthy_owner = fixture.factories[1].current();
    let connection = Connection::open(fixture.path(0)).unwrap();
    let before = guard_row(&connection, inode);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_pool_sqlite_publication
         BEFORE UPDATE ON mount_rs_compact_guards
         BEGIN SELECT RAISE(ABORT, 'pool SQLite publication rejected'); END;",
        )
        .unwrap();
    let failure = handle
        .write(&payload(90)[..CHUNK], Some(CHUNK as u64))
        .await
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::Eio);
    assert!(
        failure
            .to_string()
            .contains("pool SQLite publication rejected"),
        "failure missed actual SQLite publication: {failure}"
    );
    assert_eq!(guard_row(&connection, inode), before);
    assert!(failed_owner.upgrade().unwrap().filesystem.failed());
    assert!(
        !failed_owner
            .upgrade()
            .unwrap()
            .filesystem
            .persistent_eviction_allowed()
    );
    denied(&fixture.registrations[0], ErrorCode::Eio).await;
    handle.close().await.unwrap();
    drop(handle);
    drop(first);
    connection
        .execute_batch("DROP TRIGGER reject_pool_sqlite_publication")
        .unwrap();
    drop(connection);
    denied(&fixture.registrations[0], ErrorCode::Eio).await;
    denied(&fixture.registrations[2], ErrorCode::Ebusy).await;
    assert_eq!(fixture.factories[0].opened(), 1);
    assert_eq!(fixture.factories[0].closed(), 0);
    assert_eq!(fixture.factories[2].opened(), 0);
    assert_eq!(fixture.pool.snapshot().resident, 2);
    assert_eq!(fixture.pool.snapshot().quarantined, 1);
    full_file(sibling_lease.driver(), &healthy_bytes).await;
    fixture.sibling_usable().await;
    drop(sibling_lease);
    let other = acquire(&fixture.registrations[2]).await.unwrap();
    other.driver().write_file(FILE, &payload(32)).await.unwrap();
    full_file(other.driver(), &payload(32)).await;
    retired(&healthy_owner).await;
    drop(other);
    denied(&fixture.registrations[0], ErrorCode::Eio).await;
    assert_eq!(fixture.factories[0].opened(), 1);
    assert_eq!(stored_backing(&fixture.path(0), true).await, backing);
    assert!(
        failed_owner.upgrade().unwrap().filesystem.failed(),
        "provider repair healed the old runtime owner"
    );
    let failure = tokio::time::timeout(DEADLINE, fixture.pool.shutdown())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::Eio);
    assert_eq!(fixture.pool.snapshot().resident, 1);
    assert_eq!(fixture.pool.snapshot().quarantined, 1);
    assert_eq!(fixture.factories[0].opened(), 1);
    assert_eq!(fixture.factories[0].closed(), 0);
    assert!(
        failed_owner.upgrade().is_some(),
        "failed drain dropped the actual quarantined owner"
    );
    assert_eq!(fixture.factories[1].closed(), 1);
    assert_eq!(fixture.factories[2].closed(), 1);
    fixture.sibling_usable().await;
    for factory in &fixture.factories {
        factory.assert_handoffs();
    }
    fixture.sibling.shutdown().await.unwrap();
    fixture.context.close().await.unwrap();
    // The pool remains retained through every assertion. Lexical test teardown
    // is not an acknowledged cleanup or a qualification of this failed owner.
}

#[tokio::test]
async fn actual_durable_mrc4_backing_does_not_qualify_for_runtime_eviction() {
    let fixture = Fixture::with_format(1, 2, false).await;
    let lease = acquire(&fixture.registrations[0]).await.unwrap();
    let expected = payload(40);
    lease.driver().write_file(FILE, &expected).await.unwrap();
    full_file(lease.driver(), &expected).await;
    let backing = stored_backing(&fixture.path(0), false).await;
    let owner = fixture.factories[0].current();
    assert_eq!(
        owner.upgrade().unwrap().filesystem.concurrent_backing_id(),
        Some(backing)
    );
    assert!(
        !owner
            .upgrade()
            .unwrap()
            .filesystem
            .persistent_eviction_allowed()
    );
    drop(lease);
    denied(&fixture.registrations[1], ErrorCode::Ebusy).await;
    assert_eq!(fixture.pool.snapshot().pinned, 0);
    assert_eq!(fixture.factories[1].opened(), 0);
    assert_eq!(fixture.factories[0].closed(), 0);
    assert_eq!(fixture.pool.snapshot().eviction_success, 0);
    fixture.sibling_usable().await;
    fixture.finish_healthy().await;
}
