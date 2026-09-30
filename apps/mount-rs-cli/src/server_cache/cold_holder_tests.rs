//! Deterministic ownership/authority controls, not TiDB/RustFS qualification.
//! The actual provider GET/inspection controls live in the SDK tests; the
//! backing-savings gates require actual CLI servers and durable providers.
use super::*;
use mount_rs_blob_cache::LocalCacheConfig;
#[cfg(unix)]
use mount_rs_core::storage::ConcurrentBackingId;
#[cfg(unix)]
use mount_rs_sdk::Filesystem;
use mount_rs_sdk::{SplitOptions, StoreConfig};
#[cfg(unix)]
use mount_rs_service::catalog::{
    CatalogError, CatalogSnapshot, DriveDefinition, PartitionDefinition,
};
#[cfg(unix)]
use std::collections::VecDeque;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicUsize};
#[cfg(unix)]
use tokio::sync::Semaphore;
#[cfg(unix)]
const BOUND: Duration = Duration::from_secs(5);
#[cfg(unix)]
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("holder control did not settle")
}
#[cfg(unix)]
fn failed<T>(result: Result<T>, code: ErrorCode) {
    match result {
        Err(error) => assert_eq!(error.code, code),
        Ok(_) => panic!("expected typed refusal"),
    }
}
#[cfg(unix)]
fn backing(value: u8) -> ConcurrentBackingId {
    ConcurrentBackingId::from_bytes([value; 16]).unwrap()
}
fn options(aws: bool) -> SplitOptions {
    let mut options = SplitOptions::memory("trusted-local-holder", 4096);
    options.metadata = StoreConfig::Tidb {
        connection: "mysql://configured.invalid:4000/metadata".into(),
        volume_key: "fixed-metadata-volume".into(),
        durable: true,
    };
    options.blocks = if aws {
        StoreConfig::AwsS3 {
            bucket: "configured-bucket".into(),
            region: "us-east-1".into(),
            prefix: "fixed-prefix".into(),
            durable: true,
        }
    } else {
        StoreConfig::RustFs {
            endpoint: "http://127.0.0.1:19000".into(),
            bucket: "configured-bucket".into(),
            region: "us-east-1".into(),
            prefix: "fixed-prefix".into(),
            access_key_id: "test-configured-key".into(),
            secret_access_key: "test-configured-secret".into(),
            durable: true,
        }
    };
    options.concurrent_writes = true;
    options.inode_updates = true;
    options.compact_inode_updates = true;
    options
}
#[cfg(unix)]
struct Catalog {
    value: Mutex<Arc<CatalogSnapshot>>,
    fail: AtomicBool,
    hold: AtomicBool,
    loads: AtomicUsize,
    hold_call: AtomicUsize,
    entered: Semaphore,
    release: Semaphore,
}
#[cfg(unix)]
impl Catalog {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(Arc::new(CatalogSnapshot::empty())),
            fail: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            loads: AtomicUsize::new(0),
            hold_call: AtomicUsize::new(0),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        })
    }
    fn define(&self, partition: &str, drive: &str, definition: serde_json::Value) {
        let mut current = self.value.lock().unwrap();
        let next = Arc::make_mut(&mut current);
        next.revision += 1;
        next.partitions
            .entry(partition.into())
            .or_insert_with(|| PartitionDefinition {
                drives: BTreeMap::new(),
            })
            .drives
            .insert(drive.into(), DriveDefinition { driver: definition });
    }
}
#[cfg(unix)]
#[async_trait]
impl CatalogStore for Catalog {
    async fn load_current(&self) -> std::result::Result<CatalogSnapshot, CatalogError> {
        self.load_shared_current()
            .await
            .map(|value| value.as_ref().clone())
    }
    async fn load_shared_current(&self) -> std::result::Result<Arc<CatalogSnapshot>, CatalogError> {
        let call = self.loads.fetch_add(1, Ordering::AcqRel) + 1;
        if self.hold.load(Ordering::Acquire) || self.hold_call.load(Ordering::Acquire) == call {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        if self.fail.load(Ordering::Acquire) {
            return Err(CatalogError::Invalid("controlled catalog failure"));
        }
        Ok(self.value.lock().unwrap().clone())
    }
    async fn compare_and_swap(
        &self,
        _: u64,
        _: CatalogSnapshot,
    ) -> std::result::Result<u64, CatalogError> {
        Err(CatalogError::Conflict)
    }
}
#[cfg(unix)]
struct Resource {
    closes: AtomicUsize,
    fail: AtomicBool,
    hold: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}
#[cfg(unix)]
impl Resource {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            closes: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        })
    }
}
#[cfg(unix)]
#[async_trait]
impl ConstructionResource for Resource {
    async fn close(&self) -> Result<()> {
        self.closes.fetch_add(1, Ordering::AcqRel);
        self.entered.add_permits(1);
        if self.hold.load(Ordering::Acquire) {
            self.release.acquire().await.unwrap().forget();
        }
        if self.fail.load(Ordering::Acquire) {
            Err(error(ErrorCode::Eio))
        } else {
            Ok(())
        }
    }
}
#[cfg(unix)]
enum Reply {
    Mode(u8),
    Failure(ErrorCode),
    None,
    Panic,
}
#[cfg(unix)]
struct ControlledInspector {
    calls: AtomicUsize,
    replies: Mutex<VecDeque<Reply>>,
    resource: Arc<Resource>,
    hold: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}
#[cfg(unix)]
impl ControlledInspector {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            replies: Mutex::new(replies.into_iter().collect()),
            resource: Resource::new(),
            hold: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        })
    }
}
#[cfg(unix)]
#[async_trait]
impl Inspector for ControlledInspector {
    async fn inspect(
        &self,
        plan: &DriverRuntimePlan,
        _: &StorageContext,
        observer: &dyn ConstructionObserver,
    ) -> Result<Option<InodeModeState>> {
        assert!(
            plan.cold_cache_options().is_some(),
            "peer-selected provider bypassed eligibility"
        );
        observer.retain(self.resource.clone());
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.entered.add_permits(1);
        if self.hold.load(Ordering::Acquire) {
            self.release.acquire().await.unwrap().forget();
        }
        match self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected provider inspection")
        {
            Reply::Mode(value) => Ok(Some(InodeModeState {
                backing: backing(value),
                structural_generation: 1,
            })),
            Reply::Failure(code) => Err(error(code)),
            Reply::None => Ok(None),
            Reply::Panic => panic!("controlled inspector failure"),
        }
    }
}
#[cfg(unix)]
struct Fixture {
    path: std::path::PathBuf,
    local: Arc<LocalCache>,
    context: StorageContext,
    catalog: Arc<Catalog>,
    inspector: Arc<ControlledInspector>,
    registry: Arc<ConfiguredHolderRegistry>,
}
#[cfg(unix)]
impl Fixture {
    fn new(
        replies: impl IntoIterator<Item = Reply>,
        max_routes: usize,
        inspections: usize,
    ) -> Self {
        // Remove only after positive resource drain. Failed owner controls retain
        // their fixture until this owned test process exits.
        let path = tempfile::tempdir().unwrap().keep();
        let local = LocalCache::new_with_scope_capacity(
            LocalCacheConfig {
                directory: path.clone(),
                memory_bytes: 64,
                disk_bytes: 128,
                max_entries: 3,
                max_blob_bytes: 64,
            },
            20_000,
        )
        .unwrap();
        let context = StorageContext::new(2).unwrap();
        let catalog = Catalog::new();
        let inspector = ControlledInspector::new(replies);
        let registry = ConfiguredHolderRegistry::with_inspector(
            "cluster".into(),
            catalog.clone(),
            context.clone(),
            local.clone(),
            inspector.clone(),
            HolderLimits {
                max_routes,
                max_inspections: inspections,
                drain_bound: BOUND,
            },
        )
        .unwrap();
        Self {
            path,
            local,
            context,
            catalog,
            inspector,
            registry,
        }
    }
    fn route(&self, partition: &str, drive: &str, aws: bool) -> CacheScope {
        let definition = serde_json::json!({"trusted-definition":drive,"provider":if aws {"aws"} else {"rustfs"}});
        self.catalog.define(partition, drive, definition.clone());
        self.registry
            .register_route(
                partition,
                drive,
                definition,
                Arc::new(DriverRuntimePlan::split_plan_for_holder_tests(options(aws))),
                IntegrityPolicy::ObjectStoreSha256OrOpaque,
            )
            .unwrap();
        CacheScope {
            identity: ScopeIdentity {
                cluster: "cluster".into(),
                partition: partition.into(),
                drive: drive.into(),
            },
            backing: backing(1),
        }
    }
    async fn close(self) {
        bounded(self.registry.seal_and_drain()).await.unwrap();
        self.registry.release_proofs().unwrap();
        bounded(self.context.close()).await.unwrap();
        bounded(self.local.shutdown()).await.unwrap();
        std::fs::remove_dir_all(self.path).unwrap();
    }
}
#[cfg(unix)]
#[tokio::test]
async fn cold_holder_admission_uses_configured_scope_and_survives_active_store_eviction() {
    let f = Fixture::new([Reply::Mode(1)], 2, 1);
    let scope = f.route("partition", "drive", false);
    let active = f
        .local
        .register_scope_lease(
            scope.clone(),
            IntegrityPolicy::ObjectStoreSha256OrOpaque,
            f.local.identity_epoch(&scope.identity).unwrap(),
        )
        .unwrap();
    drop(active);
    assert!(f.local.scope_policy(&scope).is_none());
    let admitted = bounded(f.registry.admit(&scope)).await.unwrap();
    assert!(admitted.is_current().unwrap());
    drop(admitted);
    let reused = bounded(f.registry.admit(&scope)).await.unwrap();
    assert!(reused.is_current().unwrap());
    drop(reused);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 1);
    assert_eq!(f.inspector.resource.closes.load(Ordering::Acquire), 1);
    assert_eq!(f.registry.snapshot().retained_proof_groups, 1);
    let mut other = scope.clone();
    other.identity.drive = "peer-only-name".into();
    failed(f.registry.admit(&other).await, ErrorCode::Eacces);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn wrong_peer_backing_does_not_select_the_verified_backing_or_repeat_inspection() {
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    let mut wrong = scope.clone();
    wrong.backing = backing(2);
    failed(f.registry.admit(&wrong).await, ErrorCode::Estale);
    failed(f.registry.admit(&wrong).await, ErrorCode::Estale);
    let lease = f.registry.admit(&scope).await.unwrap();
    assert!(lease.is_current().unwrap());
    drop(lease);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 1);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn cancelled_waiter_retains_actual_task_and_coalesces_without_new_provider_work() {
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    f.inspector.hold.store(true, Ordering::Release);
    let r = f.registry.clone();
    let s = scope.clone();
    let cancelled = tokio::spawn(async move { r.admit(&s).await });
    bounded(f.inspector.entered.acquire())
        .await
        .unwrap()
        .forget();
    cancelled.abort();
    assert!(matches!(cancelled.await, Err(error) if error.is_cancelled()));
    assert_eq!(f.registry.snapshot().retained_inspectors, 1);
    let r = f.registry.clone();
    let s = scope.clone();
    let next = tokio::spawn(async move { r.admit(&s).await });
    f.inspector.release.add_permits(1);
    let lease = bounded(next).await.unwrap().unwrap();
    assert!(lease.is_current().unwrap());
    drop(lease);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 1);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn simultaneous_waiters_reuse_one_successful_publication() {
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    f.inspector.hold.store(true, Ordering::Release);
    let r = f.registry.clone();
    let s = scope.clone();
    let a = tokio::spawn(async move { r.admit(&s).await });
    bounded(f.inspector.entered.acquire())
        .await
        .unwrap()
        .forget();
    let r = f.registry.clone();
    let s = scope.clone();
    let b = tokio::spawn(async move { r.admit(&s).await });
    let route = f.registry.route(&scope).unwrap();
    let ticket = route.lock().ticket.clone().unwrap();
    bounded(async {
        while ticket.waiters.load(Ordering::Acquire) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    f.inspector.release.add_permits(1);
    let a = bounded(a).await.unwrap().unwrap();
    let b = bounded(b).await.unwrap().unwrap();
    assert!(a.is_current().unwrap() && b.is_current().unwrap());
    drop((a, b));
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 1);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn inspection_limit_has_no_wait_queue_and_cancellation_does_not_release_capacity() {
    let f = Fixture::new([Reply::Mode(1), Reply::Mode(1)], 2, 1);
    let a = f.route("partition", "a", false);
    let b = f.route("partition", "b", false);
    f.inspector.hold.store(true, Ordering::Release);
    let r = f.registry.clone();
    let s = a.clone();
    let waiter = tokio::spawn(async move { r.admit(&s).await });
    bounded(f.inspector.entered.acquire())
        .await
        .unwrap()
        .forget();
    waiter.abort();
    let _ = waiter.await;
    failed(f.registry.admit(&b).await, ErrorCode::Ebusy);
    f.inspector.release.add_permits(1);
    let lease = bounded(f.registry.admit(&a)).await.unwrap();
    drop(lease);
    f.inspector.hold.store(false, Ordering::Release);
    let lease = bounded(f.registry.admit(&b)).await.unwrap();
    drop(lease);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 2);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn revoked_old_await_cannot_publish_after_exact_catalog_restore() {
    let f = Fixture::new([Reply::Mode(1), Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    f.inspector.hold.store(true, Ordering::Release);
    let r = f.registry.clone();
    let s = scope.clone();
    let old = tokio::spawn(async move { r.admit(&s).await });
    bounded(f.inspector.entered.acquire())
        .await
        .unwrap()
        .forget();
    let original = f.catalog.value.lock().unwrap().partitions["partition"].drives["drive"]
        .driver
        .clone();
    f.catalog.define(
        "partition",
        "drive",
        serde_json::json!({"changed":"definition"}),
    );
    failed(f.registry.admit(&scope).await, ErrorCode::Estale);
    f.catalog.define("partition", "drive", original);
    f.inspector.release.add_permits(1);
    failed(bounded(old).await.unwrap(), ErrorCode::Estale);
    assert!(f.local.scope_policy(&scope).is_none());
    f.inspector.hold.store(false, Ordering::Release);
    let fresh = bounded(f.registry.admit(&scope)).await.unwrap();
    assert!(fresh.is_current().unwrap());
    drop(fresh);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 2);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn observed_authority_failure_invalidates_retained_proof_and_later_fresh_admission_survives_old_drop()
 {
    let f = Fixture::new([Reply::Mode(1), Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    let old = f.registry.admit(&scope).await.unwrap();
    f.catalog.fail.store(true, Ordering::Release);
    failed(f.registry.admit(&scope).await, ErrorCode::Eio);
    assert!(!old.is_current().unwrap());
    f.catalog.fail.store(false, Ordering::Release);
    let fresh = f.registry.admit(&scope).await.unwrap();
    drop(old);
    assert!(fresh.is_current().unwrap());
    assert!(f.local.scope_policy(&scope).is_some());
    drop(fresh);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn newer_locally_verified_backing_is_adopted_before_older_retained_proof() {
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let old_scope = f.route("partition", "drive", false);
    let old = f.registry.admit(&old_scope).await.unwrap();
    let mut new_scope = old_scope.clone();
    new_scope.backing = backing(2);
    let active = f
        .local
        .register_scope_lease(
            new_scope.clone(),
            IntegrityPolicy::ObjectStoreSha256OrOpaque,
            f.local.identity_epoch(&new_scope.identity).unwrap(),
        )
        .unwrap();
    let retained = f.registry.admit(&new_scope).await.unwrap();
    assert!(!old.is_current().unwrap());
    drop((active, old));
    assert!(retained.is_current().unwrap());
    drop(retained);
    failed(f.registry.admit(&old_scope).await, ErrorCode::Estale);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 1);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn cold_aws_is_excluded_but_existing_verified_active_aws_proof_is_retained() {
    let f = Fixture::new([], 1, 1);
    let scope = f.route("partition", "aws-drive", true);
    failed(f.registry.admit(&scope).await, ErrorCode::Enotsup);
    let active = f
        .local
        .register_scope_lease(
            scope.clone(),
            IntegrityPolicy::ObjectStoreSha256OrOpaque,
            f.local.identity_epoch(&scope.identity).unwrap(),
        )
        .unwrap();
    let retained = f.registry.admit(&scope).await.unwrap();
    drop(active);
    assert!(retained.is_current().unwrap());
    drop(retained);
    let again = f.registry.admit(&scope).await.unwrap();
    assert!(again.is_current().unwrap());
    drop(again);
    assert_eq!(f.registry.snapshot().retained_proof_groups, 1);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 0);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn no_metadata_inspection_starts_when_catalog_wait_resumes_after_shutdown_seal() {
    let f = Fixture::new([], 1, 1);
    let scope = f.route("partition", "drive", false);
    f.catalog.hold.store(true, Ordering::Release);
    let r = f.registry.clone();
    let s = scope.clone();
    let pending = tokio::spawn(async move { r.admit(&s).await });
    bounded(f.catalog.entered.acquire()).await.unwrap().forget();
    f.registry.seal_and_drain().await.unwrap();
    f.context.close().await.unwrap();
    f.catalog.release.add_permits(1);
    failed(bounded(pending).await.unwrap(), ErrorCode::Ebusy);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 0);
    // Public admission cannot reopen provider state after context closure.
    failed(f.registry.admit(&scope).await, ErrorCode::Ebusy);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn provider_cleanup_failure_keeps_actual_owner_and_inspection_capacity() {
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    f.inspector.resource.fail.store(true, Ordering::Release);
    failed(f.registry.admit(&scope).await, ErrorCode::Eio);
    assert_eq!(f.registry.snapshot().retained_inspectors, 1);
    assert_eq!(f.registry.slots.available_permits(), 0);
    failed(f.registry.seal_and_drain().await, ErrorCode::Eio);
    failed(f.registry.release_proofs(), ErrorCode::Eio);
    let actual = Filesystem::split_with_context(
        SplitOptions::memory("context-still-owned", 4096),
        &f.context,
    )
    .await
    .unwrap();
    actual.shutdown().await.unwrap();
    let route = f.registry.route(&scope).err(); // Registry is sealed after failed drain.
    assert!(route.is_some());
    let resource: Arc<dyn ConstructionResource> = f.inspector.resource.clone();
    let route = f.registry.lock().routes["partition"]["drive"].clone();
    let retained = route.lock().ticket.clone().unwrap();
    assert!(
        retained
            .journal
            .lock()
            .resources
            .iter()
            .any(|owner| Arc::ptr_eq(owner, &resource))
    );
    // Retain actual unknown owner state until the owned process exits.
    std::mem::forget(f);
}
#[cfg(unix)]
#[tokio::test]
async fn actual_task_failure_keeps_registered_resource_and_never_qualifies_as_drained() {
    let f = Fixture::new([Reply::Panic], 1, 1);
    let scope = f.route("partition", "drive", false);
    failed(bounded(f.registry.admit(&scope)).await, ErrorCode::Eio);
    assert_eq!(f.inspector.resource.closes.load(Ordering::Acquire), 0);
    assert_eq!(f.registry.snapshot().retained_inspectors, 1);
    failed(f.registry.seal_and_drain().await, ErrorCode::Eio);
    assert_eq!(f.registry.slots.available_permits(), 0);
    let resource: Arc<dyn ConstructionResource> = f.inspector.resource.clone();
    let route = f.registry.lock().routes["partition"]["drive"].clone();
    let retained = route.lock().ticket.clone().unwrap();
    assert!(
        retained
            .journal
            .lock()
            .resources
            .iter()
            .any(|owner| Arc::ptr_eq(owner, &resource))
    );
    std::mem::forget(f);
}
#[cfg(unix)]
#[tokio::test]
async fn missing_mode_and_typed_authority_refusal_never_publish_a_scope() {
    let f = Fixture::new(
        [
            Reply::None,
            Reply::Failure(ErrorCode::Eacces),
            Reply::Mode(1),
        ],
        1,
        1,
    );
    let scope = f.route("partition", "drive", false);
    failed(f.registry.admit(&scope).await, ErrorCode::Enotsup);
    failed(f.registry.admit(&scope).await, ErrorCode::Eacces);
    assert!(f.local.scope_policy(&scope).is_none());
    let lease = f.registry.admit(&scope).await.unwrap();
    assert!(lease.is_current().unwrap());
    drop(lease);
    assert_eq!(f.inspector.resource.closes.load(Ordering::Acquire), 3);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn configured_route_capacity_is_independent_of_three_blob_lru_entries() {
    let f = Fixture::new([], 10_000, 1);
    for drive in 0..10_000 {
        f.route(
            &format!("partition-{}", drive / 2),
            &format!("drive-{drive}"),
            false,
        );
    }
    let stats = f.registry.snapshot();
    assert_eq!(stats.configured_routes, 10_000);
    assert_eq!(stats.retained_proof_groups, 0);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 0);
    let plan = Arc::new(DriverRuntimePlan::split_plan_for_holder_tests(options(
        false,
    )));
    failed(
        f.registry.register_route(
            "extra",
            "extra",
            serde_json::json!({}),
            plan,
            IntegrityPolicy::Opaque,
        ),
        ErrorCode::Einval,
    );
    f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn completed_cancelled_inspection_releases_capacity_without_revisiting_its_route() {
    let f = Fixture::new([Reply::Mode(1), Reply::Mode(1)], 2, 1);
    let a = f.route("partition", "a", false);
    let b = f.route("partition", "b", false);
    f.inspector.hold.store(true, Ordering::Release);
    let r = f.registry.clone();
    let waiter = tokio::spawn(async move { r.admit(&a).await });
    bounded(f.inspector.entered.acquire())
        .await
        .unwrap()
        .forget();
    waiter.abort();
    let _ = waiter.await;
    f.inspector.hold.store(false, Ordering::Release);
    f.inspector.release.add_permits(1);
    bounded(f.inspector.resource.entered.acquire())
        .await
        .unwrap()
        .forget();
    // Observe the actual first task, including provider cleanup. It remains on
    // its route but never consumes new admission capacity after the reaper joins.
    let first = f.registry.lock().routes["partition"]["a"].clone();
    let ticket = first.lock().ticket.clone().unwrap();
    bounded(ticket.join()).await.unwrap();
    let lease = bounded(f.registry.admit(&b)).await.unwrap();
    assert!(lease.is_current().unwrap());
    drop(lease);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 2);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn cancelled_refusal_revokes_authority_before_held_cleanup_completes() {
    let f = Fixture::new([Reply::Failure(ErrorCode::Eacces)], 1, 1);
    let scope = f.route("partition", "drive", false);
    f.inspector.hold.store(true, Ordering::Release);
    f.inspector.resource.hold.store(true, Ordering::Release);
    let r = f.registry.clone();
    let s = scope.clone();
    let waiter = tokio::spawn(async move { r.admit(&s).await });
    bounded(f.inspector.entered.acquire())
        .await
        .unwrap()
        .forget();
    waiter.abort();
    let _ = waiter.await;
    // A separate trusted active verification can race an older pending check.
    let active = f
        .local
        .register_scope_lease(
            scope.clone(),
            IntegrityPolicy::ObjectStoreSha256OrOpaque,
            f.local.identity_epoch(&scope.identity).unwrap(),
        )
        .unwrap();
    f.inspector.release.add_permits(1);
    bounded(f.inspector.resource.entered.acquire())
        .await
        .unwrap()
        .forget();
    assert!(!active.is_current().unwrap());
    assert!(f.local.scope_policy(&scope).is_none());
    f.inspector.resource.release.add_permits(1);
    drop(active);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn poisoned_registry_owner_state_cannot_qualify_a_clean_empty_drain() {
    let f = Fixture::new([], 1, 1);
    f.route("partition", "drive", false);
    let r = f.registry.clone();
    let panicked = std::thread::spawn(move || {
        let _guard = r.state.lock().unwrap();
        panic!("controlled owner-state poison");
    });
    assert!(panicked.join().is_err());
    failed(f.registry.seal_and_drain().await, ErrorCode::Eio);
    failed(f.registry.release_proofs(), ErrorCode::Eio);
    std::mem::forget(f);
}

#[cfg(unix)]
#[tokio::test]
async fn actual_registry_drain_precedes_context_close_even_when_close_waiter_is_cancelled() {
    use crate::remote_runtime::RemoteRuntimeKeeper;
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    f.inspector.hold.store(true, Ordering::Release);
    let r = f.registry.clone();
    let s = scope.clone();
    let admission = tokio::spawn(async move { r.admit(&s).await });
    bounded(f.inspector.entered.acquire())
        .await
        .unwrap()
        .forget();
    admission.abort();
    let _ = admission.await;
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let owned = keeper.reserve().unwrap();
    owned.0.install_context(f.context.clone()).unwrap();
    owned.0.install_inspector_owner_for_test(f.registry.clone());
    let lifecycle = owned.0.clone();
    let close = tokio::spawn(async move { lifecycle.close().await });
    bounded(async {
        while !f.registry.snapshot().sealed {
            tokio::task::yield_now().await;
        }
    })
    .await;
    close.abort();
    assert!(close.await.unwrap_err().is_cancelled());
    let actual = Filesystem::split_with_context(
        SplitOptions::memory("inspection-drain-probe", 4096),
        &f.context,
    )
    .await
    .unwrap();
    actual.shutdown().await.unwrap();
    assert!(!keeper.is_empty());
    f.inspector.release.add_permits(1);
    bounded(owned.0.close()).await.unwrap();
    assert!(keeper.is_empty());
    failed(
        Filesystem::split_with_context(
            SplitOptions::memory("closed-context-probe", 4096),
            &f.context,
        )
        .await,
        ErrorCode::Estale,
    );
    failed(f.registry.admit(&scope).await, ErrorCode::Ebusy);
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn failed_actual_registry_drain_retains_keeper_and_blocks_context_close() {
    use crate::remote_runtime::RemoteRuntimeKeeper;
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    f.inspector.resource.fail.store(true, Ordering::Release);
    failed(f.registry.admit(&scope).await, ErrorCode::Eio);
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let owned = keeper.reserve().unwrap();
    owned.0.install_context(f.context.clone()).unwrap();
    owned.0.install_inspector_owner_for_test(f.registry.clone());
    let registry = Arc::downgrade(&f.registry);
    let context = f.context.clone();
    failed(bounded(owned.0.close()).await, ErrorCode::Eio);
    failed(bounded(owned.0.close()).await, ErrorCode::Eio);
    drop(f);
    assert!(registry.upgrade().is_some());
    assert!(!keeper.is_empty());
    let actual = Filesystem::split_with_context(
        SplitOptions::memory("failed-inspection-context-probe", 4096),
        &context,
    )
    .await
    .unwrap();
    actual.shutdown().await.unwrap();
    match keeper.reserve() {
        Err(error) => assert_eq!(error.code, ErrorCode::Ebusy),
        Ok(_) => panic!("failed owner replaced"),
    }
    std::mem::forget(keeper);
}

#[cfg(unix)]
#[tokio::test]
async fn canceled_failed_owner_cannot_adopt_a_later_verified_active_scope() {
    for reply in [Reply::Mode(1), Reply::Panic] {
        let cleanup_failure = matches!(reply, Reply::Mode(_));
        let f = Fixture::new([reply], 1, 1);
        let scope = f.route("partition", "drive", false);
        f.inspector.hold.store(true, Ordering::Release);
        f.inspector
            .resource
            .fail
            .store(cleanup_failure, Ordering::Release);
        let r = f.registry.clone();
        let s = scope.clone();
        let waiter = tokio::spawn(async move { r.admit(&s).await });
        bounded(f.inspector.entered.acquire())
            .await
            .unwrap()
            .forget();
        waiter.abort();
        let _ = waiter.await;
        f.inspector.release.add_permits(1);
        let route = f.registry.route(&scope).unwrap();
        let ticket = route.lock().ticket.clone().unwrap();
        let joined = bounded(ticket.join()).await;
        assert!(joined.is_err() || joined.is_ok_and(|outcome| outcome.cleanup.is_err()));
        let active = f
            .local
            .register_scope_lease(
                scope.clone(),
                IntegrityPolicy::ObjectStoreSha256OrOpaque,
                f.local.identity_epoch(&scope.identity).unwrap(),
            )
            .unwrap();
        failed(f.registry.admit(&scope).await, ErrorCode::Eio);
        assert_eq!(f.registry.snapshot().retained_inspectors, 1);
        failed(f.registry.seal_and_drain().await, ErrorCode::Eio);
        drop(active);
        std::mem::forget(f);
    }
}
#[cfg(unix)]
#[tokio::test]
async fn owner_state_poison_during_actual_cleanup_blocks_positive_drain() {
    for poison_registry in [false, true] {
        let f = Fixture::new([Reply::Mode(1)], 1, 1);
        let scope = f.route("partition", "drive", false);
        f.inspector.resource.hold.store(true, Ordering::Release);
        let r = f.registry.clone();
        let s = scope.clone();
        let admission = tokio::spawn(async move { r.admit(&s).await });
        bounded(f.inspector.resource.entered.acquire())
            .await
            .unwrap()
            .forget();
        admission.abort();
        let _ = admission.await;
        let r = f.registry.clone();
        let close = tokio::spawn(async move { r.seal_and_drain().await });
        bounded(async {
            while !f.registry.snapshot().sealed {
                tokio::task::yield_now().await;
            }
        })
        .await;
        if poison_registry {
            let r = f.registry.clone();
            assert!(
                std::thread::spawn(move || {
                    let _guard = r.state.lock().unwrap();
                    panic!("registry poison during close");
                })
                .join()
                .is_err()
            );
        } else {
            let route = f.registry.lock().routes["partition"]["drive"].clone();
            assert!(
                std::thread::spawn(move || {
                    let _guard = route.state.lock().unwrap();
                    panic!("route poison during close");
                })
                .join()
                .is_err()
            );
        }
        f.inspector.resource.release.add_permits(1);
        failed(bounded(close).await.unwrap(), ErrorCode::Eio);
        failed(f.registry.release_proofs(), ErrorCode::Eio);
        std::mem::forget(f);
    }
}

#[test]
fn filesystem_blocks_are_not_admitted_by_the_rustfs_cold_holder() {
    let mut configured = options(false);
    configured.blocks = StoreConfig::Filesystem {
        root: "local-drive-blocks".into(),
        persistent: true,
    };
    assert!(
        DriverRuntimePlan::split_plan_for_holder_tests(configured)
            .cold_cache_options()
            .is_none()
    );
}

#[test]
fn cold_inspection_is_limited_to_exact_prepared_tidb_rustfs_mrc5_options() {
    let accepted = options(false);
    assert!(
        DriverRuntimePlan::split_plan_for_holder_tests(accepted.clone())
            .cold_cache_options()
            .is_some()
    );
    for mode in 0..7 {
        let mut rejected = accepted.clone();
        match mode {
            0 => rejected.metadata = StoreConfig::Memory,
            1 => rejected.blocks = StoreConfig::Memory,
            2 => rejected.concurrent_writes = false,
            3 => rejected.inode_updates = false,
            4 => rejected.compact_inode_updates = false,
            5 => rejected.writeback = true,
            _ => rejected.delegated = true,
        }
        assert!(
            DriverRuntimePlan::split_plan_for_holder_tests(rejected)
                .cold_cache_options()
                .is_none()
        );
    }
    let mut checkout = accepted;
    checkout.checkout_path = Some("/checkout".into());
    assert!(
        DriverRuntimePlan::split_plan_for_holder_tests(checkout)
            .cold_cache_options()
            .is_none()
    );
}

// Causal unchanged-API RED: first-poll close must synchronously seal holders,
// even while an actual failed constructor journal/factory close is held. This
// is an application ownership control, not real TiDB/RustFS qualification.
#[cfg(unix)]
#[tokio::test]
async fn first_close_request_seals_holders_before_held_failed_factory_cleanup() {
    use crate::remote_runtime::RemoteRuntimeKeeper;
    use mount_rs_service::filesystem_runtime::{
        ConstructedRuntime, RuntimeConstructor, SdkRuntimeFactory,
    };
    use mount_rs_service::runtime_pool::RuntimeFactory;
    struct RefusedConstructor(Arc<Resource>);
    #[async_trait]
    impl RuntimeConstructor for RefusedConstructor {
        async fn construct(
            &self,
            observer: &dyn ConstructionObserver,
        ) -> Result<ConstructedRuntime> {
            observer.retain(self.0.clone());
            Err(error(ErrorCode::Eacces))
        }
    }
    let f = Fixture::new([], 1, 1);
    let scope = f.route("partition", "drive", false);
    let resource = Resource::new();
    resource.hold.store(true, Ordering::Release);
    resource.fail.store(true, Ordering::Release);
    let factory = SdkRuntimeFactory::new(Arc::new(RefusedConstructor(resource.clone())));
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let owned = keeper.reserve().unwrap();
    owned.0.install_context(f.context.clone()).unwrap();
    owned.0.install_inspector_owner_for_test(f.registry.clone());
    owned.0.retain_factory(factory.clone());
    let opening = tokio::spawn({
        let factory = factory.clone();
        async move { factory.open().await }
    });
    bounded(resource.entered.acquire()).await.unwrap().forget();
    {
        let close = owned.0.close();
        tokio::pin!(close);
        std::future::poll_fn(|cx| {
            assert!(
                matches!(close.as_mut().poll(cx), Poll::Pending),
                "held actual factory unexpectedly drained"
            );
            Poll::Ready(())
        })
        .await;
        // This assertion causally fails on the pinned four-patch candidate;
        // it uses one acknowledged first poll, not elapsed time or scheduling.
        assert!(
            f.registry.snapshot().sealed,
            "first close request left holder admission open"
        );
    }
    failed(f.registry.admit(&scope).await, ErrorCode::Ebusy);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 0);
    let actual = Filesystem::split_with_context(
        SplitOptions::memory("held-factory-context-probe", 4096),
        &f.context,
    )
    .await
    .unwrap();
    actual.shutdown().await.unwrap();
    assert!(!keeper.is_empty());
    resource.release.add_permits(1);
    failed(bounded(opening).await.unwrap(), ErrorCode::Eacces);
    failed(bounded(owned.0.close()).await, ErrorCode::Eio);
    failed(bounded(owned.0.close()).await, ErrorCode::Eio);
    failed(f.registry.admit(&scope).await, ErrorCode::Ebusy);
    assert_eq!(resource.closes.load(Ordering::Acquire), 1);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 0);
    let actual = Filesystem::split_with_context(
        SplitOptions::memory("failed-factory-context-probe", 4096),
        &f.context,
    )
    .await
    .unwrap();
    actual.shutdown().await.unwrap();
    assert!(!keeper.is_empty());
    // The earlier failed factory owner blocks context close; the separately
    // retained holder admission is nevertheless sealed and cannot restart.
    drop(factory);
    std::mem::forget(keeper);
    std::mem::forget(f);
}

// The catalog and inspector are controlled models. The route mutex, actual
// inspector JoinHandle, construction journal, admission and drain are real.
#[cfg(unix)]
async fn hold_final_catalog_check_after_actual_inspector_join(
    f: &Fixture,
    scope: &CacheScope,
) -> (
    Arc<Route>,
    Arc<InspectionTicket>,
    tokio::task::JoinHandle<Result<ScopeLease>>,
) {
    f.catalog.hold_call.store(2, Ordering::Release);
    let registry = f.registry.clone();
    let requested = scope.clone();
    let admission = tokio::spawn(async move { registry.admit(&requested).await });
    bounded(f.catalog.entered.acquire()).await.unwrap().forget();
    assert_eq!(f.catalog.loads.load(Ordering::Acquire), 2);
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 1);
    assert_eq!(f.inspector.resource.closes.load(Ordering::Acquire), 1);
    let route = f.registry.route(scope).unwrap();
    let ticket = route.lock().ticket.clone().unwrap();
    {
        let joined = ticket.lock();
        assert!(joined.task.is_none());
        assert!(matches!(
            joined.outcome.as_ref(),
            Some(Ok(outcome)) if outcome.cleanup.is_ok()
                && matches!(outcome.mode.as_ref(), Ok(Some(_)))
        ));
    }
    assert!(ticket.journal.lock().resources.is_empty());
    (route, ticket, admission)
}

#[cfg(unix)]
fn poison_actual_route_state(route: Arc<Route>) {
    assert!(
        std::thread::spawn(move || {
            let _guard = route.state.lock().unwrap();
            panic!("controlled route poison during final catalog check");
        })
        .join()
        .is_err()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn route_poison_during_final_catalog_check_blocks_new_proof_publication() {
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    let (route, ticket, admission) =
        hold_final_catalog_check_after_actual_inspector_join(&f, &scope).await;
    poison_actual_route_state(route.clone());
    f.catalog.release.add_permits(1);
    let result = bounded(admission).await.unwrap();
    assert!(
        matches!(result, Err(ref failure) if failure.code == ErrorCode::Eio),
        "poisoned route published a new holder proof"
    );
    assert!(f.local.scope_policy(&scope).is_none());
    assert!(route.lock().proof.is_none());
    assert!(Arc::ptr_eq(route.lock().ticket.as_ref().unwrap(), &ticket));
    assert_eq!(f.registry.snapshot().retained_inspectors, 1);
    assert_eq!(f.registry.snapshot().inspection_successes, 0);
    assert_eq!(f.registry.slots.available_permits(), 0);
    assert!(ticket.permit.lock().unwrap().is_some());
    failed(bounded(f.registry.seal_and_drain()).await, ErrorCode::Eio);
    failed(f.registry.release_proofs(), ErrorCode::Eio);
    assert!(Arc::ptr_eq(route.lock().ticket.as_ref().unwrap(), &ticket));
    assert_eq!(f.registry.slots.available_permits(), 0);
    // The unknown route owner state and its actual ticket remain physically
    // retained; a positive task/provider cleanup does not acknowledge it.
    std::mem::forget(f);
}

#[cfg(unix)]
#[tokio::test]
async fn route_poison_during_final_catalog_check_blocks_coalesced_proof_reuse() {
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    let (route, ticket, admission) =
        hold_final_catalog_check_after_actual_inspector_join(&f, &scope).await;
    // The other actual waiter publishes from the same successfully joined
    // ticket while this waiter's post-inspection catalog check stays held.
    let first_published = bounded(f.registry.admit(&scope)).await.unwrap();
    assert!(first_published.is_current().unwrap());
    assert!(route.lock().ticket.is_none());
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 1);
    assert_eq!(f.inspector.resource.closes.load(Ordering::Acquire), 1);
    assert_eq!(f.registry.snapshot().inspection_successes, 1);
    assert_eq!(f.registry.slots.available_permits(), 1);
    assert!(ticket.permit.lock().unwrap().is_none());
    poison_actual_route_state(route.clone());
    f.catalog.release.add_permits(1);
    let result = bounded(admission).await.unwrap();
    assert!(
        matches!(result, Err(ref failure) if failure.code == ErrorCode::Eio),
        "poisoned route reused a coalesced holder proof"
    );
    assert_eq!(f.registry.snapshot().retained_proof_groups, 1);
    assert_eq!(f.registry.snapshot().inspection_successes, 1);
    failed(bounded(f.registry.seal_and_drain()).await, ErrorCode::Eio);
    failed(f.registry.release_proofs(), ErrorCode::Eio);
    drop(first_published);
    // The proof published before poison is retained, not falsely released by
    // the failed drain. Its former waiter does not permit fresh admission.
    assert_eq!(f.registry.snapshot().retained_proof_groups, 1);
    std::mem::forget(f);
}

#[cfg(unix)]
#[tokio::test]
async fn first_drain_ack_rejects_registry_poison_at_terminal_publication() {
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    // Admission consumes the actual inspector JoinHandle and positively closes
    // its real journal. The remaining proof group is independently retained.
    let admitted = bounded(f.registry.admit(&scope)).await.unwrap();
    let route = f.registry.route(&scope).unwrap();
    assert!(admitted.is_current().unwrap());
    assert!(route.lock().ticket.is_none());
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 1);
    assert_eq!(f.inspector.resource.closes.load(Ordering::Acquire), 1);
    assert_eq!(f.registry.snapshot().retained_proof_groups, 1);
    assert_eq!(f.registry.slots.available_permits(), 1);
    let gate = Arc::new(DrainPublicationGate {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    *f.registry.drain_publication_gate.lock().unwrap() = Some(gate.clone());
    let registry = f.registry.clone();
    let close = tokio::spawn(async move { registry.seal_and_drain().await });
    bounded(gate.entered.acquire()).await.unwrap().forget();
    assert!(f.registry.snapshot().sealed);
    assert!(f.registry.lock().failure.is_none());
    assert!(f.registry.lock().drain_result.is_none());
    // Only the final publication lock, after the last positive owner query,
    // observes this actual mutex poison. No timeout or provider error causes it.
    let registry = f.registry.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = registry.state.lock().unwrap();
            panic!("controlled registry poison at terminal publication");
        })
        .join()
        .is_err()
    );
    gate.release.add_permits(1);
    let result = bounded(close).await.unwrap();
    assert!(
        matches!(result, Err(ref failure) if failure.code == ErrorCode::Eio),
        "terminal registry poison was acknowledged as a successful drain"
    );
    assert!(matches!(
        f.registry.lock().drain_result.as_ref(),
        Some(Err(failure)) if failure.code == ErrorCode::Eio
    ));
    assert!(route.lock().proof.is_some());
    assert!(admitted.is_current().unwrap());
    failed(f.registry.release_proofs(), ErrorCode::Eio);
    failed(bounded(f.registry.seal_and_drain()).await, ErrorCode::Eio);
    assert_eq!(f.registry.snapshot().retained_proof_groups, 1);
    // The direct method never owns context close. Verify that this failed ACK
    // left the real context usable; lifecycle containment is tested separately.
    let actual = Filesystem::split_with_context(
        SplitOptions::memory("terminal-publication-context-probe", 4096),
        &f.context,
    )
    .await
    .unwrap();
    actual.shutdown().await.unwrap();
    drop(admitted);
    assert!(route.lock().proof.is_some());
    // Unknown registry state remains physically retained until this isolated
    // process exits. Process containment is not a positive in-test cleanup ACK.
    std::mem::forget(f);
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_publication_poison_preserves_an_existing_typed_drain_error() {
    let f = Fixture::new([], 1, 1);
    let scope = f.route("partition", "drive", false);
    let route = f.registry.route(&scope).unwrap();
    // This is a controlled prior owner error, not a claimed provider refusal.
    // The actual drain must keep its selected typed error when a second owner
    // failure is newly observed at the terminal publication lock.
    route.lock().failure = Some(error(ErrorCode::Eacces));
    let gate = Arc::new(DrainPublicationGate {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    *f.registry.drain_publication_gate.lock().unwrap() = Some(gate.clone());
    let registry = f.registry.clone();
    let close = tokio::spawn(async move { registry.seal_and_drain().await });
    bounded(gate.entered.acquire()).await.unwrap().forget();
    assert!(f.registry.snapshot().sealed);
    assert!(f.registry.lock().failure.is_none());
    assert!(f.registry.lock().drain_result.is_none());
    let registry = f.registry.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = registry.state.lock().unwrap();
            panic!("controlled secondary registry poison at terminal publication");
        })
        .join()
        .is_err()
    );
    gate.release.add_permits(1);
    failed(bounded(close).await.unwrap(), ErrorCode::Eacces);
    assert!(matches!(
        f.registry.lock().drain_result.as_ref(),
        Some(Err(failure)) if failure.code == ErrorCode::Eacces
    ));
    assert!(matches!(
        f.registry.lock().failure.as_ref(),
        Some(failure) if failure.code == ErrorCode::Eio
    ));
    failed(f.registry.release_proofs(), ErrorCode::Eio);
    std::mem::forget(f);
}

#[cfg(unix)]
struct ReleaseProofGateOnDrop(Arc<ProofReleaseGate>);
#[cfg(unix)]
impl Drop for ReleaseProofGateOnDrop {
    fn drop(&mut self) {
        // Always release the actual blocking owner, including observer/assertion
        // unwinding. A test failure cannot leave a deliberately held gate shut.
        let mut released = self
            .0
            .released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *released = true;
        self.0.changed.notify_all();
    }
}

#[cfg(unix)]
async fn assert_final_proof_release_refuses_new_poison(boundary: ProofReleaseBoundary) {
    let f = Fixture::new([Reply::Mode(1)], 1, 1);
    let scope = f.route("partition", "drive", false);
    let admitted = bounded(f.registry.admit(&scope)).await.unwrap();
    let route = f.registry.route(&scope).unwrap();
    // These are actual admission/journal/JoinHandle and shutdown acknowledgments,
    // before the later synchronous proof-release boundary is controlled.
    assert!(route.lock().ticket.is_none());
    assert_eq!(f.inspector.calls.load(Ordering::Acquire), 1);
    assert_eq!(f.inspector.resource.closes.load(Ordering::Acquire), 1);
    bounded(f.registry.seal_and_drain()).await.unwrap();
    assert!(matches!(f.registry.lock().drain_result, Some(Ok(()))));
    assert!(admitted.is_current().unwrap());
    assert_eq!(f.registry.snapshot().retained_proof_groups, 1);
    let gate = Arc::new(ProofReleaseGate {
        boundary,
        entered: Semaphore::new(0),
        released: Mutex::new(false),
        changed: std::sync::Condvar::new(),
    });
    let release_on_drop = ReleaseProofGateOnDrop(gate.clone());
    *f.registry.proof_release_gate.lock().unwrap() = Some(gate.clone());
    let registry = f.registry.clone();
    let release = tokio::task::spawn_blocking(move || registry.release_proofs());
    let entered = match tokio::time::timeout(BOUND, gate.entered.acquire()).await {
        Ok(Ok(permit)) => {
            permit.forget();
            true
        }
        _ => false,
    };
    let poisoned = if entered {
        let poison = match boundary {
            ProofReleaseBoundary::Registry => {
                let registry = f.registry.clone();
                tokio::task::spawn_blocking(move || {
                    let _guard = registry.state.lock().unwrap();
                    panic!("controlled registry poison after proof-release owner query");
                })
            }
            ProofReleaseBoundary::Route => {
                let route = route.clone();
                tokio::task::spawn_blocking(move || {
                    let _guard = route.state.lock().unwrap();
                    panic!("controlled route poison before final proof take");
                })
            }
        };
        matches!(tokio::time::timeout(BOUND, poison).await, Ok(Err(_)))
    } else {
        false
    };
    // Release even if boundary observation or poison joining failed, then
    // positively join the actual proof-release blocking task before assertions.
    drop(release_on_drop);
    let result = bounded(release).await.unwrap();
    assert!(entered, "actual proof-release boundary was not observed");
    assert!(
        poisoned,
        "actual owner mutex poison was not positively joined"
    );
    assert!(
        matches!(result, Err(ref failure) if failure.code == ErrorCode::Eio),
        "new owner poison released a retained holder proof"
    );
    assert!(route.lock().proof.is_some());
    assert_eq!(f.registry.snapshot().retained_proof_groups, 1);
    assert!(admitted.is_current().unwrap());
    failed(bounded(f.registry.seal_and_drain()).await, ErrorCode::Eio);
    let actual = bounded(Filesystem::split_with_context(
        SplitOptions::memory("proof-release-owner-context-probe", 4096),
        &f.context,
    ))
    .await
    .unwrap();
    bounded(actual.shutdown()).await.unwrap();
    drop(admitted);
    assert!(route.lock().proof.is_some());
    // Direct proof release owns no context close. Failed registry/route owners
    // remain retained; the enclosing lifecycle must separately refuse context ACK.
    std::mem::forget(f);
}

#[cfg(unix)]
#[tokio::test]
async fn proof_release_refuses_registry_poison_after_final_owner_query() {
    assert_final_proof_release_refuses_new_poison(ProofReleaseBoundary::Registry).await;
}

#[cfg(unix)]
#[tokio::test]
async fn proof_release_refuses_route_poison_before_final_proof_take() {
    assert_final_proof_release_refuses_new_poison(ProofReleaseBoundary::Route).await;
}

#[cfg(not(unix))]
#[test]
fn cold_holder_cache_refusal_preserves_uncreated_directory_for_each_tier() {
    let root = tempfile::tempdir().unwrap();
    for (memory_bytes, disk_bytes) in [(64, 0), (0, 128), (64, 128)] {
        let directory = root
            .path()
            .join(format!("cache-{memory_bytes}-{disk_bytes}"));
        let result = LocalCache::new_with_scope_capacity(
            LocalCacheConfig {
                directory: directory.clone(),
                memory_bytes,
                disk_bytes,
                max_entries: 3,
                max_blob_bytes: 64,
            },
            20_000,
        );
        let error = match result {
            Ok(_) => panic!("unsupported platform constructed a local cache"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));
        assert!(
            !directory.exists(),
            "refused cache must not create its directory"
        );
    }
}
