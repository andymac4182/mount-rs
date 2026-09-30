//! Owned runtime lifetimes and bounded activation through the public pool API.
//! Synthetic eligibility exercises policy; actual reopen durability uses SQLite.

#![cfg(unix)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::storage::{BlockStore, ConcurrentBackingId, MetadataStore};
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, FsError, Result as FsResult, Stats,
};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext, StoreConfig};
use mount_rs_service::runtime_pool::{
    DriveRegistration, ManagedDrive, RuntimeFactory, RuntimeLease, RuntimePool,
};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use tokio::sync::Notify;

const DEADLINE: Duration = Duration::from_secs(5);
const FILE: &str = "/acknowledged";

async fn notified(event: &Notify) {
    tokio::time::timeout(DEADLINE, event.notified())
        .await
        .expect("controlled lifecycle event did not arrive");
}

async fn acquire(registration: &DriveRegistration) -> FsResult<RuntimeLease> {
    tokio::time::timeout(DEADLINE, registration.acquire())
        .await
        .expect("runtime acquisition did not finish")
}

async fn acquire_error(registration: &DriveRegistration, code: ErrorCode) {
    let error = match acquire(registration).await {
        Ok(_) => panic!("unexpected successful runtime acquisition"),
        Err(error) => error,
    };
    assert_eq!(error.code, code);
}

async fn joined<T>(task: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(DEADLINE, task)
        .await
        .expect("controlled task did not finish")
        .expect("controlled task failed")
}

async fn shutdown(pool: &RuntimePool) -> FsResult<()> {
    tokio::time::timeout(DEADLINE, pool.shutdown())
        .await
        .expect("owned pool shutdown did not finish")
}

async fn observed(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(DEADLINE, async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("controlled completion observation did not arrive");
}

fn backing(byte: u8) -> ConcurrentBackingId {
    ConcurrentBackingId::from_bytes([byte; 16]).unwrap()
}

#[derive(Default)]
struct Probe {
    opens: AtomicUsize,
    shutdowns: AtomicUsize,
    backend_calls: AtomicUsize,
    failed: AtomicBool,
    open_started: Notify,
    open_release: Notify,
    shutdown_started: Notify,
    shutdown_release: Notify,
    owners: Mutex<Vec<Weak<dyn ManagedDrive>>>,
}

#[derive(Clone, Copy)]
enum OpenOutcome {
    Success,
    Error,
    Panic,
}

struct FakeFactory {
    probe: Arc<Probe>,
    eligible: bool,
    open_gated: bool,
    shutdown_gated: bool,
    shutdown_error: bool,
    fail_after_shutdown: bool,
    outcome: OpenOutcome,
    backings: Vec<ConcurrentBackingId>,
}

impl FakeFactory {
    fn new(eligible: bool) -> Self {
        Self {
            probe: Arc::default(),
            eligible,
            open_gated: false,
            shutdown_gated: false,
            shutdown_error: false,
            fail_after_shutdown: false,
            outcome: OpenOutcome::Success,
            backings: vec![backing(1)],
        }
    }
}

struct CountedMemory {
    memory: MemoryFs,
    probe: Arc<Probe>,
}

#[async_trait]
impl FsDriver for CountedMemory {
    fn capabilities(&self) -> Capabilities {
        self.memory.capabilities()
    }
    async fn stat(&self, path: &str) -> FsResult<Stats> {
        self.probe.backend_calls.fetch_add(1, Ordering::SeqCst);
        self.memory.stat(path).await
    }
    async fn readdir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        self.probe.backend_calls.fetch_add(1, Ordering::SeqCst);
        self.memory.readdir(path).await
    }
    async fn open(&self, path: &str, flags: &str, mode: u32) -> FsResult<Arc<dyn FileHandle>> {
        self.probe.backend_calls.fetch_add(1, Ordering::SeqCst);
        self.memory.open(path, flags, mode).await
    }
}

struct FakeRuntime {
    driver: Arc<dyn FsDriver>,
    factory: Arc<FakeFactory>,
    backing: Option<ConcurrentBackingId>,
}

#[async_trait]
impl ManagedDrive for FakeRuntime {
    fn driver(&self) -> Arc<dyn FsDriver> {
        self.driver.clone()
    }
    fn failed(&self) -> bool {
        self.factory.probe.failed.load(Ordering::SeqCst)
    }
    fn eviction_allowed(&self) -> bool {
        self.factory.eligible
    }
    fn concurrent_backing_id(&self) -> Option<ConcurrentBackingId> {
        self.backing
    }
    async fn shutdown(&self) -> FsResult<()> {
        let probe = &self.factory.probe;
        probe.shutdowns.fetch_add(1, Ordering::SeqCst);
        probe.shutdown_started.notify_one();
        if self.factory.shutdown_gated {
            probe.shutdown_release.notified().await;
        }
        if self.factory.fail_after_shutdown {
            probe.failed.store(true, Ordering::SeqCst);
        }
        if self.factory.shutdown_error {
            Err(FsError::new(ErrorCode::Eio).with_message("injected shutdown failure"))
        } else {
            Ok(())
        }
    }
}

struct SharedFactory(Arc<FakeFactory>);

#[async_trait]
impl RuntimeFactory for SharedFactory {
    async fn open(&self) -> FsResult<Arc<dyn ManagedDrive>> {
        let index = self.0.probe.opens.fetch_add(1, Ordering::SeqCst);
        self.0.probe.open_started.notify_one();
        if self.0.open_gated {
            self.0.probe.open_release.notified().await;
        }
        match self.0.outcome {
            OpenOutcome::Error => {
                return Err(FsError::new(ErrorCode::Eio).with_message("injected open failure"));
            }
            OpenOutcome::Panic => panic!("injected owned factory panic"),
            OpenOutcome::Success => {}
        }
        let identity = self.0.backings[index.min(self.0.backings.len() - 1)];
        let runtime: Arc<dyn ManagedDrive> = Arc::new(FakeRuntime {
            driver: Arc::new(CountedMemory {
                memory: MemoryFs::new(MemoryOptions::default()),
                probe: self.0.probe.clone(),
            }),
            factory: self.0.clone(),
            backing: self.0.eligible.then_some(identity),
        });
        self.0
            .probe
            .owners
            .lock()
            .unwrap()
            .push(Arc::downgrade(&runtime));
        Ok(runtime)
    }
}

fn register(pool: &RuntimePool, factory: &Arc<FakeFactory>) -> Arc<DriveRegistration> {
    Arc::new(
        pool.register(Arc::new(SharedFactory(factory.clone())))
            .unwrap(),
    )
}

#[tokio::test]
async fn registrations_open_no_runtimes_and_zero_capacity_is_rejected() {
    assert!(RuntimePool::new(0).is_err());
    let pool = RuntimePool::new(2).unwrap();
    let factory = Arc::new(FakeFactory::new(false));
    let registrations: Vec<_> = (0..10_000).map(|_| register(&pool, &factory)).collect();
    let snapshot = pool.snapshot();
    assert_eq!(snapshot.registered, registrations.len());
    assert_eq!(snapshot.resident, 0);
    assert_eq!(snapshot.opening, 0);
    assert_eq!(snapshot.open_success, 0);
    assert_eq!(factory.probe.opens.load(Ordering::SeqCst), 0);
    shutdown(&pool).await.unwrap();
}

#[tokio::test]
async fn cancelled_first_waiter_preserves_one_owned_cold_open() {
    let pool = RuntimePool::new(1).unwrap();
    let mut configured = FakeFactory::new(true);
    configured.open_gated = true;
    let factory = Arc::new(configured);
    let registration = register(&pool, &factory);
    let first = tokio::spawn({
        let registration = registration.clone();
        async move { registration.acquire().await }
    });
    notified(&factory.probe.open_started).await;
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().opening, 1);
    let waits_before_second = pool.snapshot().waits;
    let second = tokio::spawn({
        let registration = registration.clone();
        async move { registration.acquire().await }
    });
    observed(|| pool.snapshot().waits > waits_before_second).await;
    first.abort();
    let cancelled = match first.await {
        Ok(_) => panic!("cancelled waiter completed"),
        Err(error) => error,
    };
    assert!(cancelled.is_cancelled());
    factory.probe.open_release.notify_one();
    let lease = joined(second).await.unwrap();
    assert!(lease.waited());
    lease.driver().stat("/").await.unwrap();
    assert_eq!(factory.probe.opens.load(Ordering::SeqCst), 1);
    assert_eq!(pool.snapshot().open_success, 1);
    drop(lease);
    let hot = acquire(&registration).await.unwrap();
    assert!(!hot.waited());
    drop(hot);
    shutdown(&pool).await.unwrap();
}

#[tokio::test]
async fn closing_victim_holds_capacity_and_replacement_callers_share_admission() {
    let pool = RuntimePool::new(1).unwrap();
    let mut configured = FakeFactory::new(true);
    configured.shutdown_gated = true;
    let victim = Arc::new(configured);
    let replacement = Arc::new(FakeFactory::new(true));
    let unrelated = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &victim);
    let b = register(&pool, &replacement);
    let c = register(&pool, &unrelated);
    drop(acquire(&a).await.unwrap());
    let first = tokio::spawn({
        let b = b.clone();
        async move { b.acquire().await }
    });
    notified(&victim.probe.shutdown_started).await;
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().closing, 1);
    assert_eq!(replacement.probe.opens.load(Ordering::SeqCst), 0);
    let waits_before_second = pool.snapshot().waits;
    let second = tokio::spawn({
        let b = b.clone();
        async move { b.acquire().await }
    });
    observed(|| pool.snapshot().waits > waits_before_second).await;
    acquire_error(&c, ErrorCode::Ebusy).await;
    assert_eq!(unrelated.probe.opens.load(Ordering::SeqCst), 0);
    victim.probe.shutdown_release.notify_one();
    let first = joined(first).await.unwrap();
    let second = joined(second).await.unwrap();
    assert!(first.waited() && second.waited());
    assert_eq!(replacement.probe.opens.load(Ordering::SeqCst), 1);
    assert_eq!(victim.probe.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().eviction_success, 1);
    drop((first, second));
    shutdown(&pool).await.unwrap();
}

#[tokio::test]
async fn pinned_owner_refuses_another_drive_without_opening_it() {
    let pool = RuntimePool::new(1).unwrap();
    let first = Arc::new(FakeFactory::new(true));
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &first);
    let b = register(&pool, &other);
    let lease = acquire(&a).await.unwrap();
    acquire_error(&b, ErrorCode::Ebusy).await;
    assert!(pool.snapshot().pinned > 0);
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().capacity_rejections, 1);
    assert_eq!(first.probe.shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(other.probe.opens.load(Ordering::SeqCst), 0);
    drop(lease);
    shutdown(&pool).await.unwrap();
}

#[tokio::test]
async fn unqualified_idle_owner_is_never_evicted() {
    let pool = RuntimePool::new(1).unwrap();
    let first = Arc::new(FakeFactory::new(false));
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &first);
    let b = register(&pool, &other);
    drop(acquire(&a).await.unwrap());
    acquire_error(&b, ErrorCode::Ebusy).await;
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(first.probe.shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(other.probe.opens.load(Ordering::SeqCst), 0);
    shutdown(&pool).await.unwrap();
}

#[tokio::test]
async fn quarantined_owner_keeps_its_charge_and_cannot_be_reopened() {
    let pool = RuntimePool::new(1).unwrap();
    let first = Arc::new(FakeFactory::new(true));
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &first);
    let b = register(&pool, &other);
    let lease = acquire(&a).await.unwrap();
    lease.quarantine();
    drop(lease);
    acquire_error(&a, ErrorCode::Eio).await;
    acquire_error(&b, ErrorCode::Ebusy).await;
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().quarantined, 1);
    assert_eq!(first.probe.opens.load(Ordering::SeqCst), 1);
    assert_eq!(other.probe.opens.load(Ordering::SeqCst), 0);
    assert!(first.probe.owners.lock().unwrap()[0].upgrade().is_some());
    let _ = shutdown(&pool).await;
}

#[tokio::test]
async fn failed_health_prevents_idle_shutdown_and_replacement() {
    let pool = RuntimePool::new(1).unwrap();
    let first = Arc::new(FakeFactory::new(true));
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &first);
    let b = register(&pool, &other);
    let lease = acquire(&a).await.unwrap();
    first.probe.failed.store(true, Ordering::SeqCst);
    drop(lease);
    acquire_error(&b, ErrorCode::Ebusy).await;
    acquire_error(&a, ErrorCode::Eio).await;
    assert_eq!(first.probe.shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().quarantined, 1);
    assert_eq!(other.probe.opens.load(Ordering::SeqCst), 0);
    let _ = shutdown(&pool).await;
}

#[tokio::test]
async fn health_failure_after_successful_shutdown_retains_owner_and_capacity() {
    let pool = RuntimePool::new(1).unwrap();
    let mut configured = FakeFactory::new(true);
    configured.fail_after_shutdown = true;
    let first = Arc::new(configured);
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &first);
    let b = register(&pool, &other);
    drop(acquire(&a).await.unwrap());
    assert!(acquire(&b).await.is_err());
    acquire_error(&a, ErrorCode::Eio).await;
    assert_eq!(first.probe.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().quarantined, 1);
    assert_eq!(other.probe.opens.load(Ordering::SeqCst), 0);
    assert!(first.probe.owners.lock().unwrap()[0].upgrade().is_some());
    let _ = shutdown(&pool).await;
    assert_eq!(first.probe.shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shutdown_error_is_retained_without_replay_or_replacement() {
    let pool = RuntimePool::new(1).unwrap();
    let mut configured = FakeFactory::new(true);
    configured.shutdown_error = true;
    let first = Arc::new(configured);
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &first);
    let b = register(&pool, &other);
    drop(acquire(&a).await.unwrap());
    assert!(acquire(&b).await.is_err());
    acquire_error(&a, ErrorCode::Eio).await;
    acquire_error(&b, ErrorCode::Ebusy).await;
    assert_eq!(pool.snapshot().quarantined, 1);
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(other.probe.opens.load(Ordering::SeqCst), 0);
    assert!(first.probe.owners.lock().unwrap()[0].upgrade().is_some());
    assert!(shutdown(&pool).await.is_err());
    assert_eq!(first.probe.shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelled_shutdown_waiter_retains_the_exact_owned_close() {
    let pool = Arc::new(RuntimePool::new(1).unwrap());
    let mut configured = FakeFactory::new(true);
    configured.shutdown_gated = true;
    let factory = Arc::new(configured);
    let registration = register(&pool, &factory);
    drop(acquire(&registration).await.unwrap());
    let waiter = tokio::spawn({
        let pool = pool.clone();
        async move { pool.shutdown().await }
    });
    notified(&factory.probe.shutdown_started).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().closing, 1);
    assert!(acquire(&registration).await.is_err());
    factory.probe.shutdown_release.notify_one();
    shutdown(&pool).await.unwrap();
    assert_eq!(factory.probe.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(pool.snapshot().resident, 0);
    assert!(acquire(&registration).await.is_err());
}

async fn failed_open(outcome: OpenOutcome) {
    let pool = RuntimePool::new(1).unwrap();
    let mut configured = FakeFactory::new(true);
    configured.outcome = outcome;
    let factory = Arc::new(configured);
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &factory);
    let b = register(&pool, &other);
    acquire_error(&a, ErrorCode::Eio).await;
    acquire_error(&a, ErrorCode::Eio).await;
    acquire_error(&b, ErrorCode::Ebusy).await;
    assert_eq!(factory.probe.opens.load(Ordering::SeqCst), 1);
    assert_eq!(other.probe.opens.load(Ordering::SeqCst), 0);
    assert_eq!(pool.snapshot().open_error, 1);
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().quarantined, 1);
    let _ = shutdown(&pool).await;
}

#[tokio::test]
async fn failed_open_keeps_a_nonretryable_capacity_charge() {
    failed_open(OpenOutcome::Error).await;
}

#[tokio::test]
async fn panicking_open_keeps_a_nonretryable_capacity_charge() {
    failed_open(OpenOutcome::Panic).await;
}

#[tokio::test]
async fn changed_backing_after_eviction_quarantines_the_new_owner() {
    let pool = RuntimePool::new(1).unwrap();
    let mut configured = FakeFactory::new(true);
    configured.backings = vec![backing(1), backing(2)];
    let first = Arc::new(configured);
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &first);
    let b = register(&pool, &other);
    let lease = acquire(&a).await.unwrap();
    lease.driver().stat("/").await.unwrap();
    drop(lease);
    drop(acquire(&b).await.unwrap());
    assert!(acquire(&a).await.is_err());
    assert!(acquire(&a).await.is_err());
    assert_eq!(first.probe.opens.load(Ordering::SeqCst), 2);
    assert_eq!(first.probe.backend_calls.load(Ordering::SeqCst), 1);
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().quarantined, 1);
    assert!(first.probe.owners.lock().unwrap()[1].upgrade().is_some());
    let _ = shutdown(&pool).await;
}

#[derive(Default)]
struct CloseProbe {
    calls: AtomicUsize,
    cancelled: AtomicUsize,
    finished: AtomicUsize,
    started: Notify,
    release: Notify,
}

struct CloseAttempt<'a> {
    probe: &'a CloseProbe,
    complete: bool,
}
impl Drop for CloseAttempt<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.probe.cancelled.fetch_add(1, Ordering::SeqCst);
        }
    }
}

struct GatedHandle {
    probe: Arc<CloseProbe>,
    fail: bool,
}

#[async_trait]
impl FileHandle for GatedHandle {
    async fn read(&self, _: &mut [u8], _: Option<u64>) -> FsResult<usize> {
        Ok(0)
    }
    async fn write(&self, _: &[u8], _: Option<u64>) -> FsResult<usize> {
        Err(FsError::enosys("write"))
    }
    async fn stat(&self) -> FsResult<Stats> {
        Err(FsError::enosys("stat"))
    }
    async fn truncate(&self, _: u64) -> FsResult<()> {
        Err(FsError::enosys("truncate"))
    }
    async fn close(&self) -> FsResult<()> {
        let mut attempt = CloseAttempt {
            probe: &self.probe,
            complete: false,
        };
        self.probe.calls.fetch_add(1, Ordering::SeqCst);
        self.probe.started.notify_one();
        self.probe.release.notified().await;
        attempt.complete = true;
        self.probe.finished.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err(FsError::new(ErrorCode::Eio).with_message("injected handle close failure"))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn dropping_adopted_handle_owns_close_and_keeps_its_runtime_pin() {
    let pool = RuntimePool::new(1).unwrap();
    let factory = Arc::new(FakeFactory::new(true));
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &factory);
    let b = register(&pool, &other);
    let lease = acquire(&a).await.unwrap();
    let probe = Arc::new(CloseProbe::default());
    let actual: Arc<dyn FileHandle> = Arc::new(GatedHandle {
        probe: probe.clone(),
        fail: false,
    });
    let weak = Arc::downgrade(&actual);
    let handle = lease.wrap_handle(actual);
    drop(lease);
    drop(handle);
    notified(&probe.started).await;
    acquire_error(&b, ErrorCode::Ebusy).await;
    assert!(weak.upgrade().is_some());
    assert!(pool.snapshot().pinned > 0);
    probe.release.notify_one();
    observed(|| pool.snapshot().pinned == 0 && probe.finished.load(Ordering::SeqCst) == 1).await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 0);
    drop(acquire(&b).await.unwrap());
    shutdown(&pool).await.unwrap();
}

#[tokio::test]
async fn cancelled_handle_close_waiter_never_cancels_or_replays_backend_close() {
    let pool = RuntimePool::new(1).unwrap();
    let factory = Arc::new(FakeFactory::new(true));
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &factory);
    let b = register(&pool, &other);
    let lease = acquire(&a).await.unwrap();
    let probe = Arc::new(CloseProbe::default());
    let handle = lease.wrap_handle(Arc::new(GatedHandle {
        probe: probe.clone(),
        fail: false,
    }));
    drop(lease);
    let waiter = tokio::spawn({
        let handle = handle.clone();
        async move { handle.close().await }
    });
    notified(&probe.started).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 0);
    acquire_error(&b, ErrorCode::Ebusy).await;
    probe.release.notify_one();
    tokio::time::timeout(DEADLINE, handle.close())
        .await
        .unwrap()
        .unwrap();
    drop(handle);
    observed(|| pool.snapshot().pinned == 0).await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 0);
    drop(acquire(&b).await.unwrap());
    shutdown(&pool).await.unwrap();
}

#[tokio::test]
async fn failed_handle_close_retains_the_actual_handle_under_quarantine() {
    let pool = RuntimePool::new(1).unwrap();
    let factory = Arc::new(FakeFactory::new(true));
    let other = Arc::new(FakeFactory::new(true));
    let a = register(&pool, &factory);
    let b = register(&pool, &other);
    let lease = acquire(&a).await.unwrap();
    let probe = Arc::new(CloseProbe::default());
    let actual: Arc<dyn FileHandle> = Arc::new(GatedHandle {
        probe: probe.clone(),
        fail: true,
    });
    let weak = Arc::downgrade(&actual);
    let handle = lease.wrap_handle(actual);
    drop(lease);
    probe.release.notify_one();
    let error = tokio::time::timeout(DEADLINE, handle.close())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Eio);
    drop(handle);
    assert!(
        weak.upgrade().is_some(),
        "failed close lost its actual handle"
    );
    acquire_error(&a, ErrorCode::Eio).await;
    acquire_error(&b, ErrorCode::Ebusy).await;
    assert_eq!(pool.snapshot().quarantined, 1);
    assert_eq!(pool.snapshot().resident, 1);
    let _ = shutdown(&pool).await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
}

struct SqliteFactory {
    options: SplitOptions,
    context: Arc<StorageContext>,
    opens: AtomicUsize,
}

struct SqliteRuntime(Filesystem);

#[async_trait]
impl RuntimeFactory for SqliteFactory {
    async fn open(&self) -> FsResult<Arc<dyn ManagedDrive>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(SqliteRuntime(
            Filesystem::split_with_context(self.options.clone(), &self.context).await?,
        )))
    }
}

#[async_trait]
impl ManagedDrive for SqliteRuntime {
    fn driver(&self) -> Arc<dyn FsDriver> {
        self.0.driver()
    }
    fn failed(&self) -> bool {
        self.0.failed()
    }
    fn eviction_allowed(&self) -> bool {
        !self.0.failed() && self.0.concurrent_backing_id().is_some()
    }
    fn concurrent_backing_id(&self) -> Option<ConcurrentBackingId> {
        self.0.concurrent_backing_id()
    }
    async fn shutdown(&self) -> FsResult<()> {
        self.0.shutdown().await
    }
}

fn sqlite_options(path: &Path, owner: &str) -> SplitOptions {
    let store = StoreConfig::Sqlite {
        path: path.to_owned(),
    };
    let mut options = SplitOptions::memory(owner, 4096).with_compact_inode_updates(true);
    options.metadata = store.clone();
    options.blocks = store;
    options
}

fn payload(seed: usize) -> Vec<u8> {
    (0..12_461)
        .map(|offset| ((offset * 19 + seed * 37 + offset / 101) % 256) as u8)
        .collect()
}

async fn full_file_oracle(driver: &Arc<dyn FsDriver>, expected: &[u8]) {
    let handle = driver.open(FILE, "r", 0).await.unwrap();
    assert_eq!(handle.stat().await.unwrap().size, expected.len() as u64);
    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let remaining = actual.len() - offset;
        let count = handle
            .read(&mut actual[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0 && count <= remaining);
        offset += count;
    }
    assert_eq!(actual, expected);
    assert_eq!(
        handle
            .read(&mut [0], Some(expected.len() as u64))
            .await
            .unwrap(),
        0
    );
    handle.close().await.unwrap();
}

async fn stored_backing(path: &Path) -> ConcurrentBackingId {
    let metadata = SqliteMetadataStore::open(path).unwrap();
    let identity = metadata
        .compact_inode_mode_state()
        .await
        .unwrap()
        .unwrap()
        .backing;
    SqliteBlockStore::open(path)
        .unwrap()
        .verify_concurrent_backing(identity)
        .await
        .unwrap();
    identity
}

#[tokio::test]
async fn actual_sqlite_full_bytes_eof_and_backing_survive_repeated_bounded_eviction() {
    let directory = tempfile::tempdir().unwrap();
    let context = Arc::new(StorageContext::new(2).unwrap());
    let pool = RuntimePool::new(1).unwrap();
    let mut registrations = Vec::new();
    let mut factories = Vec::new();
    let mut identities = Vec::new();
    let mut expected = Vec::new();
    for index in 0..3 {
        let path = directory.path().join(format!("drive-{index}.sqlite"));
        let options = sqlite_options(&path, &format!("pool-drive-{index}"));
        let initial = Filesystem::split_with_context(options.clone(), &context)
            .await
            .unwrap();
        let bytes = payload(index);
        initial.driver().write_file(FILE, &bytes).await.unwrap();
        identities.push(initial.concurrent_backing_id().unwrap());
        initial.shutdown().await.unwrap();
        drop(initial);
        assert_eq!(stored_backing(&path).await, identities[index]);
        let factory = Arc::new(SqliteFactory {
            options,
            context: context.clone(),
            opens: AtomicUsize::new(0),
        });
        registrations.push(pool.register(factory.clone()).unwrap());
        factories.push(factory);
        expected.push(bytes);
    }
    let sibling_path = directory.path().join("context-sibling.sqlite");
    let sibling =
        Filesystem::split_with_context(sqlite_options(&sibling_path, "context-sibling"), &context)
            .await
            .unwrap();
    let sibling_bytes = payload(7);
    sibling
        .driver()
        .write_file(FILE, &sibling_bytes)
        .await
        .unwrap();
    assert_eq!(pool.snapshot().resident, 0);
    assert!(
        factories
            .iter()
            .all(|f| f.opens.load(Ordering::SeqCst) == 0)
    );

    for index in [0, 1, 2, 0, 2, 1, 0] {
        let lease = acquire(&registrations[index]).await.unwrap();
        full_file_oracle(lease.driver(), &expected[index]).await;
        assert_eq!(pool.snapshot().resident, 1);
        drop(lease);
        full_file_oracle(&sibling.driver(), &sibling_bytes).await;
        let path = directory.path().join(format!("drive-{index}.sqlite"));
        assert_eq!(stored_backing(&path).await, identities[index]);
    }

    let lease = acquire(&registrations[1]).await.unwrap();
    let handle = lease.wrap_handle(lease.driver().open(FILE, "r+", 0).await.unwrap());
    let replacement = payload(99);
    assert_eq!(
        handle
            .write(&replacement[..4096], Some(4096))
            .await
            .unwrap(),
        4096
    );
    expected[1][4096..8192].copy_from_slice(&replacement[..4096]);
    handle.close().await.unwrap();
    drop(handle);
    drop(lease);
    drop(acquire(&registrations[2]).await.unwrap());
    let fresh = acquire(&registrations[1]).await.unwrap();
    full_file_oracle(fresh.driver(), &expected[1]).await;
    drop(fresh);
    let snapshot = pool.snapshot();
    assert_eq!(snapshot.registered, 3);
    assert_eq!(snapshot.resident, 1);
    assert_eq!(snapshot.quarantined, 0);
    assert!(snapshot.eviction_success >= 6);
    assert_eq!(
        snapshot.open_success,
        factories
            .iter()
            .map(|f| f.opens.load(Ordering::SeqCst))
            .sum::<usize>()
    );
    shutdown(&pool).await.unwrap();
    assert_eq!(pool.snapshot().resident, 0);
    full_file_oracle(&sibling.driver(), &sibling_bytes).await;
    sibling.shutdown().await.unwrap();
    drop(sibling);

    for index in 0..3 {
        let path = directory.path().join(format!("drive-{index}.sqlite"));
        assert_eq!(stored_backing(&path).await, identities[index]);
        let fresh = Filesystem::split_with_context(
            sqlite_options(&path, &format!("final-oracle-{index}")),
            &context,
        )
        .await
        .unwrap();
        assert_eq!(fresh.concurrent_backing_id(), Some(identities[index]));
        full_file_oracle(&fresh.driver(), &expected[index]).await;
        fresh.shutdown().await.unwrap();
    }
    context.close().await.unwrap();
}
