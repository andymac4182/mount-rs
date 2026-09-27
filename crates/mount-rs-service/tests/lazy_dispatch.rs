//! Fresh authorization around owned, lazily activated Drive runtimes.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, FileHandle, FsDriver, FsError, Result as FsResult, Stats,
};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_remote_protocol::{Operation, OperationName};
use mount_rs_service::{
    catalog::{
        CatalogError, CatalogSnapshot, CatalogStore, DriveDefinition, GrantDefinition,
        PartitionDefinition, Permission,
    },
    dispatch::{DriveDispatcher, SessionHandles, SessionIdentity},
    runtime_pool::{DriveRegistration, ManagedDrive, RuntimeFactory, RuntimePool},
};
use serde_json::json;
use tokio::sync::Notify;

struct Catalog {
    snapshot: Mutex<Arc<CatalogSnapshot>>,
    loads: AtomicUsize,
    hold_load: AtomicUsize,
    entered: Notify,
    release: Notify,
}

#[async_trait]
impl CatalogStore for Catalog {
    async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
        Ok((**self.snapshot.lock().unwrap()).clone())
    }
    async fn load_shared_current(&self) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        let load = self.loads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.hold_load.load(Ordering::SeqCst) == load {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(self.snapshot.lock().unwrap().clone())
    }
    async fn compare_and_swap(
        &self,
        expected: u64,
        mut next: CatalogSnapshot,
    ) -> Result<u64, CatalogError> {
        let mut current = self.snapshot.lock().unwrap();
        if current.revision != expected {
            return Err(CatalogError::Conflict);
        }
        next.revision = expected + 1;
        let revision = next.revision;
        *current = Arc::new(next);
        Ok(revision)
    }
}

impl Catalog {
    fn change(&self, change: impl FnOnce(&mut CatalogSnapshot)) {
        // Deliberately preserve revision. Rechecking only the number is unsafe.
        let mut current = self.snapshot.lock().unwrap();
        let mut next = (**current).clone();
        change(&mut next);
        *current = Arc::new(next);
    }
}

struct Driver {
    memory: MemoryFs,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl FsDriver for Driver {
    fn capabilities(&self) -> Capabilities {
        self.memory.capabilities()
    }
    async fn stat(&self, path: &str) -> FsResult<Stats> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.memory.stat(path).await
    }
    async fn readdir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        self.memory.readdir(path).await
    }
    async fn open(&self, _: &str, _: &str, _: u32) -> FsResult<Arc<dyn FileHandle>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(FsError::enosys("fixture open"))
    }
    async fn write_file(&self, _: &str, _: &[u8]) -> FsResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
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
    async fn shutdown(&self) -> FsResult<()> {
        Ok(())
    }
}
struct Factory {
    opens: AtomicUsize,
    hold: AtomicBool,
    entered: Notify,
    release: Notify,
    runtime: Arc<Runtime>,
}
#[async_trait]
impl RuntimeFactory for Factory {
    async fn open(&self) -> FsResult<Arc<dyn ManagedDrive>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        if self.hold.load(Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(self.runtime.clone())
    }
}

fn identity() -> SessionIdentity {
    SessionIdentity {
        partition_id: "p".into(),
        policy_id: "policy".into(),
        issuer: "https://issuer.example".into(),
        subject: "sandbox".into(),
        signing_algorithm: "ES256".into(),
        claims: json!({"aud":"mount-rs", "sandbox_id":"sandbox"}),
        expires_at: i64::MAX,
    }
}
fn snapshot() -> CatalogSnapshot {
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.revision = 1;
    snapshot.partitions.insert(
        "p".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "d".into(),
                DriveDefinition {
                    driver: json!({"kind":"memory"}),
                },
            )]),
        },
    );
    snapshot.issuer_policies.insert(
        "policy".into(),
        json!({
            "issuer":"https://issuer.example", "audiences":["mount-rs"], "algorithms":["ES256"]
        }),
    );
    snapshot.grants.insert(
        "sandbox".into(),
        GrantDefinition {
            partition_id: "p".into(),
            policy_id: "policy".into(),
            drives: BTreeMap::from([("d".into(), Permission::Write)]),
            claim_conditions: BTreeMap::from([("/sandbox_id".into(), "sandbox".into())]),
        },
    );
    snapshot
}
fn fixture(
    hold: bool,
) -> (
    Arc<Catalog>,
    Arc<DriveDispatcher>,
    RuntimePool,
    Arc<Factory>,
    Arc<AtomicUsize>,
) {
    let (catalog, dispatcher, pool, factory, calls, _registration) =
        fixture_with_registration(hold);
    (catalog, dispatcher, pool, factory, calls)
}
fn fixture_with_registration(
    hold: bool,
) -> (
    Arc<Catalog>,
    Arc<DriveDispatcher>,
    RuntimePool,
    Arc<Factory>,
    Arc<AtomicUsize>,
    DriveRegistration,
) {
    let catalog = Arc::new(Catalog {
        snapshot: Mutex::new(Arc::new(snapshot())),
        loads: AtomicUsize::new(0),
        hold_load: AtomicUsize::new(0),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(Factory {
        opens: AtomicUsize::new(0),
        hold: AtomicBool::new(hold),
        entered: Notify::new(),
        release: Notify::new(),
        runtime: Arc::new(Runtime(Arc::new(Driver {
            memory: MemoryFs::new(MemoryOptions::default()),
            calls: calls.clone(),
        }))),
    });
    let pool = RuntimePool::new(1).unwrap();
    let registration = pool.register(factory.clone()).unwrap();
    let mut dispatcher = DriveDispatcher::new(catalog.clone());
    dispatcher
        .register_lazy_definition("p", "d", json!({"kind":"memory"}), registration.clone())
        .unwrap();
    (
        catalog,
        Arc::new(dispatcher),
        pool,
        factory,
        calls,
        registration,
    )
}
fn stat() -> Operation {
    Operation {
        name: OperationName::Stat,
        body: json!({"path":"/"}),
    }
}
async fn entered(factory: &Factory) {
    tokio::time::timeout(Duration::from_secs(5), factory.entered.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn lazy_registration_and_denied_scopes_invoke_no_factory() {
    let (catalog, dispatcher, pool, factory, calls) = fixture(false);
    assert_eq!(pool.snapshot().resident, 0);
    for mode in 0..5 {
        let mut id = identity();
        match mode {
            0 => id.partition_id = "other".into(),
            1 => id.claims["sandbox_id"] = json!("other"),
            2 => id.signing_algorithm = "RS256".into(),
            3 => id.expires_at = 0,
            _ => catalog.change(|s| {
                s.partitions
                    .get_mut("p")
                    .unwrap()
                    .drives
                    .get_mut("d")
                    .unwrap()
                    .driver = json!({"kind":"host"})
            }),
        }
        let error = dispatcher.dispatch(&id, "d", &stat()).await.unwrap_err();
        assert_eq!(error.code, if mode == 4 { "ESTALE" } else { "EACCES" });
    }
    assert_eq!(factory.opens.load(Ordering::SeqCst), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(pool.snapshot().resident, 0);
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_handle_never_activates_a_cold_drive() {
    let (_, dispatcher, pool, factory, calls) = fixture(false);
    let handles = SessionHandles::default();
    let operation = Operation {
        name: OperationName::HandleStat,
        body: json!({"handle":77}),
    };
    assert_eq!(
        dispatcher
            .dispatch_with_handles(&identity(), "d", &operation, &handles)
            .await
            .unwrap_err()
            .code,
        "EBADF"
    );
    assert_eq!(factory.opens.load(Ordering::SeqCst), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(pool.snapshot().resident, 0);
    handles.close_all().await;
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn cold_open_rechecks_same_revision_grant_policy_definition_and_permission() {
    for mode in 0..4 {
        let (catalog, dispatcher, pool, factory, calls) = fixture(true);
        let operation = if mode == 3 {
            Operation {
                name: OperationName::Write,
                body: json!({"path":"/file", "data":[1,2,3]}),
            }
        } else {
            stat()
        };
        let task = tokio::spawn({
            let dispatcher = dispatcher.clone();
            async move { dispatcher.dispatch(&identity(), "d", &operation).await }
        });
        entered(&factory).await;
        catalog.change(|s| match mode {
            0 => s.grants.clear(),
            1 => {
                s.issuer_policies.get_mut("policy").unwrap()["algorithms"] = json!(["RS256"]);
            }
            2 => {
                s.partitions
                    .get_mut("p")
                    .unwrap()
                    .drives
                    .get_mut("d")
                    .unwrap()
                    .driver = json!({"kind":"host"});
            }
            _ => {
                s.grants
                    .get_mut("sandbox")
                    .unwrap()
                    .drives
                    .insert("d".into(), Permission::Read);
            }
        });
        factory.release.notify_one();
        let failure = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(failure.code, if mode == 2 { "ESTALE" } else { "EACCES" });
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "stale authorization reached backend for mode {mode}"
        );
        assert_eq!(
            catalog.loads.load(Ordering::SeqCst),
            2,
            "one initial and one post-wait fresh check"
        );
        *catalog.snapshot.lock().unwrap() = Arc::new(snapshot());
        dispatcher
            .dispatch(&identity(), "d", &stat())
            .await
            .unwrap();
        assert_eq!(
            catalog.loads.load(Ordering::SeqCst),
            3,
            "hot path must retain one current load"
        );
        assert_eq!(factory.opens.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        pool.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn cold_open_rechecks_expiry_before_any_backend_operation() {
    let (_, dispatcher, pool, factory, calls) = fixture(true);
    let mut id = identity();
    id.expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 2;
    let expiry = id.expires_at as u64;
    let task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.dispatch(&id, "d", &stat()).await }
    });
    entered(&factory).await;
    // This timer deliberately crosses the real wall-clock expiry. Notify gates
    // remain the synchronization mechanism for runtime ownership.
    let remaining = (UNIX_EPOCH + Duration::from_secs(expiry))
        .duration_since(SystemTime::now())
        .unwrap_or_default();
    tokio::time::sleep(remaining + Duration::from_millis(10)).await;
    assert!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            >= expiry
    );
    factory.release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        "EACCES"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_cold_request_keeps_open_owner_without_quarantining_uninvoked_backend() {
    let (_, dispatcher, pool, factory, calls) = fixture(true);
    let task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move {
            dispatcher
                .dispatch(
                    &identity(),
                    "d",
                    &Operation {
                        name: OperationName::Write,
                        body: json!({"path":"/file", "data":[1]}),
                    },
                )
                .await
        }
    });
    entered(&factory).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(pool.snapshot().opening, 1);
    factory.release.notify_one();
    tokio::time::timeout(
        Duration::from_secs(5),
        dispatcher.dispatch(&identity(), "d", &stat()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(factory.opens.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(pool.snapshot().quarantined, 0);
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn quarantined_generation_during_post_open_authorization_never_reaches_backend() {
    let (catalog, dispatcher, pool, factory, calls, registration) = fixture_with_registration(true);
    catalog.hold_load.store(2, Ordering::SeqCst);
    let task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.dispatch(&identity(), "d", &stat()).await }
    });
    entered(&factory).await;
    factory.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), catalog.entered.notified())
        .await
        .unwrap();
    let lease = registration.acquire().await.unwrap();
    assert!(!lease.waited());
    lease.quarantine();
    drop(lease);
    assert_eq!(pool.snapshot().quarantined, 1);
    catalog.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "quarantined generation reached backend after authorization wait"
    );
    assert_eq!(result.unwrap_err().code, "EIO");
    assert_eq!(factory.opens.load(Ordering::SeqCst), 1);
    assert_eq!(catalog.loads.load(Ordering::SeqCst), 2);
    assert!(pool.shutdown().await.is_err());
    assert_eq!(pool.snapshot().resident, 1);
}

#[tokio::test]
async fn expiry_during_post_open_catalog_load_never_reaches_backend() {
    let (catalog, dispatcher, pool, factory, calls) = fixture(true);
    catalog.hold_load.store(2, Ordering::SeqCst);
    let mut id = identity();
    id.expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 2;
    let expiry = id.expires_at as u64;
    let task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.dispatch(&id, "d", &stat()).await }
    });
    entered(&factory).await;
    factory.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), catalog.entered.notified())
        .await
        .unwrap();
    let remaining = (UNIX_EPOCH + Duration::from_secs(expiry))
        .duration_since(SystemTime::now())
        .unwrap_or_default();
    tokio::time::sleep(remaining + Duration::from_millis(10)).await;
    assert!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            >= expiry
    );
    catalog.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "expired identity reached backend after catalog wait"
    );
    assert_eq!(result.unwrap_err().code, "EACCES");
    assert_eq!(factory.opens.load(Ordering::SeqCst), 1);
    assert_eq!(catalog.loads.load(Ordering::SeqCst), 2);
    pool.shutdown().await.unwrap();
}
