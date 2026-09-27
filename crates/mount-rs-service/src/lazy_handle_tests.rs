//! Behavioral controls for adopting returned handles before admission awaits.

use super::*;
use crate::{
    catalog::{
        CatalogError, CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition,
    },
    runtime_pool::{ManagedDrive, RuntimeFactory, RuntimePool},
};
use async_trait::async_trait;
use mount_rs_core::storage::ConcurrentBackingId;
use mount_rs_core::{
    Capabilities, DirEntry, FsError, GuardedMutation, GuardedMutationResult, PathGuard,
    PathIdentity, Result as FsResult, Stats,
};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use std::sync::{
    Mutex as StdMutex, Weak,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Default)]
struct Gates {
    open_entered: Notify,
    open_release: Notify,
    returned: Notify,
    close_entered: Notify,
    close_release: Notify,
    close_done: Notify,
    read_entered: Notify,
    read_release: Notify,
    write_entered: Notify,
    write_release: Notify,
    closes: AtomicUsize,
    shutdowns: AtomicUsize,
    stats: AtomicUsize,
    fail_close: AtomicBool,
    weak_handle: StdMutex<Option<Weak<ActualHandle>>>,
}
struct ActualHandle(Arc<Gates>);
#[async_trait]
impl FileHandle for ActualHandle {
    async fn read(&self, _: &mut [u8], _: Option<u64>) -> FsResult<usize> {
        self.0.read_entered.notify_one();
        self.0.read_release.notified().await;
        Ok(0)
    }
    async fn write(&self, data: &[u8], _: Option<u64>) -> FsResult<usize> {
        Ok(data.len())
    }
    async fn stat(&self) -> FsResult<Stats> {
        Err(FsError::enosys("fixture stat"))
    }
    async fn truncate(&self, _: u64) -> FsResult<()> {
        Ok(())
    }
    async fn close(&self) -> FsResult<()> {
        self.0.closes.fetch_add(1, Ordering::SeqCst);
        self.0.close_entered.notify_one();
        self.0.close_release.notified().await;
        self.0.close_done.notify_one();
        if self.0.fail_close.load(Ordering::SeqCst) {
            Err(FsError::new(mount_rs_core::ErrorCode::Eio))
        } else {
            Ok(())
        }
    }
}
struct Driver {
    memory: MemoryFs,
    gates: Arc<Gates>,
}
#[async_trait]
impl FsDriver for Driver {
    fn capabilities(&self) -> Capabilities {
        self.memory.capabilities()
    }
    fn supports_guarded_mutations(&self) -> bool {
        true
    }
    async fn stat(&self, path: &str) -> FsResult<Stats> {
        self.gates.stats.fetch_add(1, Ordering::SeqCst);
        self.memory.stat(path).await
    }
    async fn readdir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        self.memory.readdir(path).await
    }
    async fn open(&self, _: &str, _: &str, _: u32) -> FsResult<Arc<dyn FileHandle>> {
        self.gates.open_entered.notify_one();
        self.gates.open_release.notified().await;
        let handle = Arc::new(ActualHandle(self.gates.clone()));
        *self.gates.weak_handle.lock().unwrap() = Some(Arc::downgrade(&handle));
        self.gates.returned.notify_one();
        Ok(handle)
    }
    async fn guarded_mutation(&self, _: GuardedMutation) -> FsResult<GuardedMutationResult> {
        Ok(GuardedMutationResult::Opened {
            handle: self.open("/file", "r", 0).await?,
            identity: PathIdentity { dev: 1, ino: 2 },
        })
    }
    async fn write_file(&self, _: &str, _: &[u8]) -> FsResult<()> {
        self.gates.write_entered.notify_one();
        self.gates.write_release.notified().await;
        Ok(())
    }
}
struct Runtime(Arc<Driver>);
#[async_trait]
impl ManagedDrive for Runtime {
    fn driver(&self) -> Arc<dyn FsDriver> {
        self.0.clone()
    }
    fn failed(&self) -> bool {
        false
    }
    // Synthetic eligibility exercises lifecycle decisions, not persistence.
    fn eviction_allowed(&self) -> bool {
        true
    }
    fn concurrent_backing_id(&self) -> Option<ConcurrentBackingId> {
        Some(ConcurrentBackingId::from_bytes([1; 16]).unwrap())
    }
    async fn shutdown(&self) -> FsResult<()> {
        self.0.gates.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Factory(Arc<Runtime>);
#[async_trait]
impl RuntimeFactory for Factory {
    async fn open(&self) -> FsResult<Arc<dyn ManagedDrive>> {
        Ok(self.0.clone())
    }
}
struct Catalog(StdMutex<Arc<CatalogSnapshot>>);
#[async_trait]
impl CatalogStore for Catalog {
    async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
        Ok((**self.0.lock().unwrap()).clone())
    }
    async fn load_shared_current(&self) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn compare_and_swap(&self, _: u64, _: CatalogSnapshot) -> Result<u64, CatalogError> {
        Err(CatalogError::Conflict)
    }
}
fn fixture_with_catalog() -> (
    Arc<DriveDispatcher>,
    RuntimePool,
    crate::runtime_pool::DriveRegistration,
    Arc<Gates>,
    Arc<Catalog>,
) {
    let mut catalog = CatalogSnapshot::empty();
    catalog.revision = 1;
    catalog.partitions.insert(
        "p".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "d".into(),
                DriveDefinition {
                    driver: serde_json::json!({"kind":"memory"}),
                },
            )]),
        },
    );
    catalog.issuer_policies.insert(
        "policy".into(),
        serde_json::json!({"issuer":"https://issuer.example","audiences":["mount-rs"]}),
    );
    catalog.grants.insert(
        "g".into(),
        GrantDefinition {
            partition_id: "p".into(),
            policy_id: "policy".into(),
            drives: BTreeMap::from([("d".into(), Permission::Write)]),
            claim_conditions: BTreeMap::from([("/sandbox_id".into(), "sandbox".into())]),
        },
    );
    let gates = Arc::new(Gates::default());
    let runtime = Arc::new(Runtime(Arc::new(Driver {
        memory: MemoryFs::new(MemoryOptions::default()),
        gates: gates.clone(),
    })));
    let pool = RuntimePool::new(1).unwrap();
    let registration = pool.register(Arc::new(Factory(runtime))).unwrap();
    let other = pool
        .register(Arc::new(Factory(Arc::new(Runtime(Arc::new(Driver {
            memory: MemoryFs::new(MemoryOptions::default()),
            gates: Arc::new(Gates::default()),
        }))))))
        .unwrap();
    let catalog = Arc::new(Catalog(StdMutex::new(Arc::new(catalog))));
    let mut dispatcher = DriveDispatcher::new(catalog.clone());
    dispatcher
        .register_lazy_definition("p", "d", serde_json::json!({"kind":"memory"}), registration)
        .unwrap();
    (Arc::new(dispatcher), pool, other, gates, catalog)
}
fn fixture() -> (
    Arc<DriveDispatcher>,
    RuntimePool,
    crate::runtime_pool::DriveRegistration,
    Arc<Gates>,
) {
    let (dispatcher, pool, other, gates, _catalog) = fixture_with_catalog();
    (dispatcher, pool, other, gates)
}
fn identity() -> SessionIdentity {
    SessionIdentity {
        partition_id: "p".into(),
        policy_id: "policy".into(),
        issuer: "https://issuer.example".into(),
        subject: "sandbox".into(),
        signing_algorithm: "ES256".into(),
        claims: serde_json::json!({"aud":"mount-rs","sandbox_id":"sandbox"}),
        expires_at: i64::MAX,
    }
}
fn open(guarded: bool) -> Operation {
    if guarded {
        Operation {
            name: OperationName::GuardedMutation,
            body: serde_json::to_value(GuardedMutation::Open {
                parent: PathGuard {
                    path: "/".into(),
                    identity: PathIdentity { dev: 1, ino: 1 },
                },
                name: "file".into(),
                observed: mount_rs_core::ObservedEntry::Any,
                flags: mount_rs_core::OpenFlags::parse("r", "/file").unwrap(),
                mode: 0,
            })
            .unwrap(),
        }
    } else {
        Operation {
            name: OperationName::Open,
            body: serde_json::json!({"path":"/file","flags":"r","mode":0}),
        }
    }
}
async fn notified(notify: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .unwrap();
}
fn busy<T>(result: FsResult<T>) {
    assert!(matches!(result,Err(e) if e.code==mount_rs_core::ErrorCode::Ebusy));
}

async fn cancelled_admission(guarded: bool) {
    let (dispatcher, pool, other, gates) = fixture();
    let handles = Arc::new(SessionHandles::default());
    let task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        let handles = handles.clone();
        async move {
            dispatcher
                .dispatch_with_handles(&identity(), "d", &open(guarded), &handles)
                .await
        }
    });
    notified(&gates.open_entered).await;
    let state = handles.state.lock().await;
    gates.open_release.notify_one();
    notified(&gates.returned).await;
    // This current-thread task returns the handle and reaches the blocked
    // admission await in the same poll, before this test can abort it.
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    notified(&gates.close_entered).await;
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().pinned, 1);
    busy(other.acquire().await);
    assert!(
        gates
            .weak_handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .upgrade()
            .is_some()
    );
    assert_eq!(gates.closes.load(Ordering::SeqCst), 1);
    drop(state);
    gates.close_release.notify_one();
    notified(&gates.close_done).await;
    let lease = tokio::time::timeout(Duration::from_secs(5), other.acquire())
        .await
        .unwrap()
        .unwrap();
    drop(lease);
    handles.close_all().await;
    assert_eq!(gates.closes.load(Ordering::SeqCst), 1);
    assert!(
        gates
            .weak_handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .upgrade()
            .is_none()
    );
    pool.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancelled_lazy_open_admission_retains_exact_close_and_runtime_pin() {
    cancelled_admission(false).await;
}
#[tokio::test]
async fn cancelled_lazy_guarded_open_admission_retains_exact_close_and_runtime_pin() {
    cancelled_admission(true).await;
}

#[tokio::test]
async fn cancelled_lazy_write_quarantines_before_releasing_request_pin() {
    let (dispatcher, pool, other, gates) = fixture();
    let task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move {
            dispatcher
                .dispatch(
                    &identity(),
                    "d",
                    &Operation {
                        name: OperationName::Write,
                        body: serde_json::json!({"path":"/file","data":[1,2,3]}),
                    },
                )
                .await
        }
    });
    notified(&gates.write_entered).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(pool.snapshot().resident, 1);
    assert_eq!(pool.snapshot().quarantined, 1);
    assert_eq!(pool.snapshot().pinned, 0);
    busy(other.acquire().await);
    assert_eq!(gates.shutdowns.load(Ordering::SeqCst), 0);
    assert!(pool.shutdown().await.is_err());
    assert_eq!(pool.snapshot().resident, 1);
}

async fn admitted(
    dispatcher: &Arc<DriveDispatcher>,
    handles: &Arc<SessionHandles>,
    gates: &Gates,
) -> u64 {
    let task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        let handles = handles.clone();
        async move {
            dispatcher
                .dispatch_with_handles(&identity(), "d", &open(false), &handles)
                .await
        }
    });
    notified(&gates.open_entered).await;
    gates.open_release.notify_one();
    task.await.unwrap().unwrap().as_u64().unwrap()
}
#[tokio::test]
async fn failed_lazy_handle_close_retains_actual_handle_after_session_drop() {
    let (dispatcher, pool, other, gates) = fixture();
    let handles = Arc::new(SessionHandles::default());
    let id = admitted(&dispatcher, &handles, &gates).await;
    gates.fail_close.store(true, Ordering::SeqCst);
    let task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        let handles = handles.clone();
        async move {
            dispatcher
                .dispatch_with_handles(
                    &identity(),
                    "d",
                    &Operation {
                        name: OperationName::HandleClose,
                        body: serde_json::json!({"handle":id}),
                    },
                    &handles,
                )
                .await
        }
    });
    notified(&gates.close_entered).await;
    gates.close_release.notify_one();
    assert_eq!(task.await.unwrap().unwrap_err().code, "EIO");
    handles.close_all().await;
    drop(handles);
    assert_eq!(gates.closes.load(Ordering::SeqCst), 1);
    assert_eq!(pool.snapshot().quarantined, 1);
    assert!(
        gates
            .weak_handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .upgrade()
            .is_some()
    );
    busy(other.acquire().await);
    assert!(pool.shutdown().await.is_err());
    assert!(
        gates
            .weak_handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .upgrade()
            .is_some()
    );
}

#[tokio::test]
async fn borrowed_lazy_handle_prevents_close_and_runtime_eviction_until_io_finishes() {
    let (dispatcher, pool, other, gates) = fixture();
    let handles = Arc::new(SessionHandles::default());
    let id = admitted(&dispatcher, &handles, &gates).await;
    let read = tokio::spawn({
        let dispatcher = dispatcher.clone();
        let handles = handles.clone();
        async move {
            dispatcher
                .dispatch_with_handles(
                    &identity(),
                    "d",
                    &Operation {
                        name: OperationName::HandleRead,
                        body: serde_json::json!({"handle":id,"length":1,"position":0}),
                    },
                    &handles,
                )
                .await
        }
    });
    notified(&gates.read_entered).await;
    let close = tokio::spawn({
        let dispatcher = dispatcher.clone();
        let handles = handles.clone();
        async move {
            dispatcher
                .dispatch_with_handles(
                    &identity(),
                    "d",
                    &Operation {
                        name: OperationName::HandleClose,
                        body: serde_json::json!({"handle":id}),
                    },
                    &handles,
                )
                .await
        }
    });
    // Observe session removal rather than using a sleep: the owned close has
    // been scheduled, but its write gate must wait for the borrowed read.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !handles.state.lock().await.entries.contains_key(&id) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!close.is_finished());
    assert_eq!(gates.closes.load(Ordering::SeqCst), 0);
    assert_eq!(pool.snapshot().pinned, 1);
    busy(other.acquire().await);
    gates.read_release.notify_one();
    read.await.unwrap().unwrap();
    notified(&gates.close_entered).await;
    assert_eq!(pool.snapshot().pinned, 1);
    busy(other.acquire().await);
    gates.close_release.notify_one();
    close.await.unwrap().unwrap();
    let lease = other.acquire().await.unwrap();
    drop(lease);
    handles.close_all().await;
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn authorization_rechecks_revocation_after_owned_handle_cleanup_wait() {
    let (dispatcher, pool, _other, gates, catalog) = fixture_with_catalog();
    let handles = Arc::new(SessionHandles::default());
    let _id = admitted(&dispatcher, &handles, &gates).await;
    let mut changed = (**catalog.0.lock().unwrap()).clone();
    changed.revision = 2;
    *catalog.0.lock().unwrap() = Arc::new(changed);
    let task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        let handles = handles.clone();
        async move {
            dispatcher
                .dispatch_with_handles(
                    &identity(),
                    "d",
                    &Operation {
                        name: OperationName::Stat,
                        body: serde_json::json!({"path":"/"}),
                    },
                    &handles,
                )
                .await
        }
    });
    notified(&gates.close_entered).await;
    let mut revoked = (**catalog.0.lock().unwrap()).clone();
    revoked.grants.clear();
    *catalog.0.lock().unwrap() = Arc::new(revoked);
    gates.close_release.notify_one();
    let result = task.await.unwrap();
    assert_eq!(
        gates.stats.load(Ordering::SeqCst),
        0,
        "authorization captured before cleanup wait reached backend"
    );
    assert_eq!(result.unwrap_err().code, "EACCES");
    handles.close_all().await;
    drop(handles);
    pool.shutdown().await.unwrap();
}
