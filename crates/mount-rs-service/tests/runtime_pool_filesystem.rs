//! Actual SDK runtime eviction with process-persistent filesystem blocks.
//! These process-reopen controls do not establish power-loss durability.

#![cfg(all(feature = "sdk-runtime", any(target_os = "linux", target_os = "macos")))]

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::construction::ConstructionObserver;
use mount_rs_core::storage::{BlockStore, ConcurrentBackingId, MetadataStore};
use mount_rs_core::{ErrorCode, FsDriver, Result};
use mount_rs_filesystem_blocks::FilesystemBlockStore;
use mount_rs_sdk::{ConstructionJournal, Filesystem, SplitOptions, StorageContext, StoreConfig};
use mount_rs_service::filesystem_runtime::{
    ConstructedRuntime, RuntimeConstructor, SdkRuntimeFactory,
};
use mount_rs_service::runtime_pool::{DriveRegistration, RuntimeLease, RuntimePool};
use mount_rs_sqlite::SqliteMetadataStore;

const DEADLINE: Duration = Duration::from_secs(10);
const FILE: &str = "/acknowledged";

struct Constructor {
    options: SplitOptions,
    context: Arc<StorageContext>,
    calls: AtomicUsize,
    owners: Mutex<Vec<Weak<Filesystem>>>,
}

#[async_trait]
impl RuntimeConstructor for Constructor {
    async fn construct(&self, observer: &dyn ConstructionObserver) -> Result<ConstructedRuntime> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let actual = Arc::new(
            Filesystem::split_with_context_and_construction_observer(
                self.options.clone(),
                &self.context,
                observer,
            )
            .await?,
        );
        observer.retain(actual.clone());
        self.owners.lock().unwrap().push(Arc::downgrade(&actual));
        let driver = actual.driver();
        Ok(ConstructedRuntime::new(actual, driver))
    }
}

fn options(directory: &Path, owner: &str, persistent: bool) -> SplitOptions {
    let root = directory.join(format!("{owner}-blocks"));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let mut options = SplitOptions::memory(owner, 4096)
        .with_inode_updates(true)
        .with_compact_inode_updates(true);
    options.metadata = StoreConfig::Sqlite {
        path: directory.join(format!("{owner}.sqlite")),
    };
    options.blocks = StoreConfig::Filesystem { root, persistent };
    options
}

fn payload(seed: usize) -> Vec<u8> {
    (0..64 * 1024 + 173)
        .map(|offset| ((offset * 19 + seed * 37 + offset / 101) % 256) as u8)
        .collect()
}

async fn full_file(driver: &Arc<dyn FsDriver>, expected: &[u8]) {
    let handle = driver.open(FILE, "r", 0).await.unwrap();
    assert_eq!(handle.stat().await.unwrap().size, expected.len() as u64);
    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let end = actual.len().min(offset + 1379);
        let count = handle
            .read(&mut actual[offset..end], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0 && count <= end - offset);
        offset += count;
    }
    assert_eq!(actual, expected);
    for at in [expected.len(), expected.len() + 19] {
        assert_eq!(handle.read(&mut [0; 17], Some(at as u64)).await.unwrap(), 0);
    }
    handle.close().await.unwrap();
}

async fn retired(owner: &Weak<Filesystem>) {
    tokio::time::timeout(DEADLINE, async {
        while owner.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("closed actual SDK owner remained retained");
}

async fn acquire(registration: &DriveRegistration) -> Result<RuntimeLease> {
    tokio::time::timeout(DEADLINE, registration.acquire())
        .await
        .expect("runtime admission stalled")
}

struct Fixture {
    directory: PathBuf,
    persistent: bool,
    context: Arc<StorageContext>,
    pool: RuntimePool,
    constructors: Vec<Arc<Constructor>>,
    factories: Vec<Arc<SdkRuntimeFactory>>,
    registrations: Vec<DriveRegistration>,
    sibling: Filesystem,
    sibling_bytes: Vec<u8>,
}

impl Fixture {
    async fn new(persistent: bool, count: usize) -> Self {
        // Preserve backing after any failed oracle; delete only after actual
        // runtime, sibling and context shutdown have been acknowledged.
        let directory = tempfile::tempdir().unwrap().keep().canonicalize().unwrap();
        let context = Arc::new(StorageContext::new(2).unwrap());
        let pool = RuntimePool::new(1).unwrap();
        let mut constructors = Vec::new();
        let mut factories = Vec::new();
        let mut registrations = Vec::new();
        for index in 0..count {
            let owner = format!("drive-{index}");
            let constructor = Arc::new(Constructor {
                options: options(&directory, &owner, persistent),
                context: context.clone(),
                calls: AtomicUsize::new(0),
                owners: Mutex::new(Vec::new()),
            });
            let factory = SdkRuntimeFactory::new(constructor.clone());
            registrations.push(pool.register(factory.clone()).unwrap());
            constructors.push(constructor);
            factories.push(factory);
            assert!(!directory.join(format!("{owner}.sqlite")).exists());
            assert!(
                std::fs::read_dir(directory.join(format!("{owner}-blocks")))
                    .unwrap()
                    .next()
                    .is_none(),
                "lazy registration initialized the filesystem backing"
            );
        }
        let sibling_journal = ConstructionJournal::new();
        let sibling_attempt = sibling_journal.begin().unwrap();
        let sibling = Filesystem::split_with_context_and_construction_observer(
            options(&directory, "sibling", true),
            &context,
            &sibling_journal,
        )
        .await
        .unwrap();
        sibling_attempt.handoff().unwrap();
        assert!(sibling_journal.snapshot().handed_off);
        assert!(!sibling.driver().capabilities().durable_writes);
        let sibling_bytes = payload(200);
        sibling
            .driver()
            .write_file(FILE, &sibling_bytes)
            .await
            .unwrap();
        Self {
            directory,
            persistent,
            context,
            pool,
            constructors,
            factories,
            registrations,
            sibling,
            sibling_bytes,
        }
    }

    fn owner(&self, index: usize) -> Weak<Filesystem> {
        self.constructors[index]
            .owners
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .clone()
    }

    async fn stored_backing(&self, index: usize) -> ConcurrentBackingId {
        let metadata =
            SqliteMetadataStore::open(self.directory.join(format!("drive-{index}.sqlite")))
                .unwrap();
        assert!(metadata.durable());
        let backing = metadata
            .compact_inode_mode_state()
            .await
            .unwrap()
            .expect("actual MRC5 marker missing")
            .backing;
        let blocks = FilesystemBlockStore::open(
            self.directory.join(format!("drive-{index}-blocks")),
            self.persistent,
        )
        .unwrap();
        assert!(!blocks.durable());
        assert_eq!(blocks.persistent(), self.persistent);
        blocks.verify_concurrent_backing(backing).await.unwrap();
        backing
    }

    async fn finish(self) {
        tokio::time::timeout(DEADLINE, self.pool.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(self.pool.snapshot().resident, 0);
        for factory in &self.factories {
            tokio::time::timeout(DEADLINE, factory.close())
                .await
                .unwrap()
                .unwrap();
        }
        for constructor in &self.constructors {
            let owners = constructor.owners.lock().unwrap().clone();
            for owner in &owners {
                retired(owner).await;
            }
        }
        full_file(&self.sibling.driver(), &self.sibling_bytes).await;
        self.sibling.shutdown().await.unwrap();
        self.context.close().await.unwrap();
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[tokio::test]
async fn process_persistent_filesystem_blocks_reopen_through_capacity_one_sdk_runtime() {
    let fixture = Fixture::new(true, 3).await;
    let expected: Vec<_> = (0..3).map(payload).collect();
    let mut identities = [None; 3];
    let mut previous = None;
    for index in [0, 1, 2, 0, 2, 1, 0, 1, 2, 0] {
        let lease = acquire(&fixture.registrations[index]).await.unwrap();
        if let Some(previous) = previous.take() {
            retired(&previous).await;
        }
        let weak = fixture.owner(index);
        let actual = weak.upgrade().unwrap();
        assert!(actual.persistent_eviction_allowed());
        assert!(!actual.driver().capabilities().durable_writes);
        let backing = actual.concurrent_backing_id().unwrap();
        drop(actual);
        if identities[index].is_none() {
            lease
                .driver()
                .write_file(FILE, &expected[index])
                .await
                .unwrap();
            identities[index] = Some(backing);
        }
        assert_eq!(Some(backing), identities[index]);
        full_file(lease.driver(), &expected[index]).await;
        assert_eq!(fixture.stored_backing(index).await, backing);
        assert!(fixture.factories[index].construction_snapshot().is_none());
        drop(lease);
        previous = Some(weak);
        full_file(&fixture.sibling.driver(), &fixture.sibling_bytes).await;
        let state = fixture.pool.snapshot();
        assert_eq!(state.resident, 1);
        assert_eq!(state.pinned, 0);
        assert_eq!(state.quarantined, 0);
    }
    let state = fixture.pool.snapshot();
    assert_eq!(state.registered, 3);
    assert_eq!(state.open_success, 10);
    assert_eq!(state.eviction_success, 9);
    assert_eq!(state.eviction_error, 0);
    assert_eq!(
        fixture
            .constructors
            .iter()
            .map(|constructor| constructor.calls.load(Ordering::SeqCst))
            .collect::<Vec<_>>(),
        [4, 3, 3]
    );
    fixture.finish().await;
}

#[tokio::test]
async fn filesystem_blocks_without_process_persistence_remain_resident_at_capacity() {
    let fixture = Fixture::new(false, 2).await;
    let first = acquire(&fixture.registrations[0]).await.unwrap();
    let owner = fixture.owner(0);
    assert!(!owner.upgrade().unwrap().persistent_eviction_allowed());
    assert!(!first.driver().capabilities().durable_writes);
    let expected = payload(99);
    first.driver().write_file(FILE, &expected).await.unwrap();
    let backing = fixture.stored_backing(0).await;
    drop(first);
    let error = match acquire(&fixture.registrations[1]).await {
        Ok(_) => panic!("unasserted filesystem persistence authorized runtime eviction"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::Ebusy);
    assert_eq!(fixture.constructors[1].calls.load(Ordering::SeqCst), 0);
    assert!(fixture.constructors[1].owners.lock().unwrap().is_empty());
    let state = fixture.pool.snapshot();
    assert_eq!(state.resident, 1);
    assert_eq!(state.pinned, 0);
    assert_eq!(state.eviction_success, 0);
    assert_eq!(state.capacity_rejections, 1);
    let first = acquire(&fixture.registrations[0]).await.unwrap();
    full_file(first.driver(), &expected).await;
    assert_eq!(fixture.stored_backing(0).await, backing);
    drop(first);
    fixture.finish().await;
}
