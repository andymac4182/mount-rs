//! Owned application drain controls; the configured binary test separately
//! covers signed QUIC/WebSocket I/O and persistent fresh reopen.
use super::*;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_sdk::{Filesystem, MemoryOptions, SplitOptions};
use mount_rs_service::runtime_pool::RuntimeFactory;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio::sync::Notify;

const BOUND: Duration = Duration::from_secs(5);

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("owned lifecycle did not settle")
}

async fn context_usable(context: &StorageContext) -> bool {
    match Filesystem::split_with_context(SplitOptions::memory("context-probe", 4096), context).await
    {
        Ok(actual) => {
            actual.shutdown().await.unwrap();
            true
        }
        Err(error) => {
            assert_eq!(error.code, ErrorCode::Estale);
            false
        }
    }
}

fn constructor(context: &StorageContext) -> Arc<CliRuntimeConstructor> {
    Arc::new(CliRuntimeConstructor {
        plan: crate::runtime::DriverRuntime::prepare(
            &crate::CliOptions {
                driver: crate::DriverChoice::Memory,
                ..crate::CliOptions::default()
            },
            111,
            222,
        )
        .unwrap(),
        context: context.clone(),
        decorator: None,
    })
}

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
#[tokio::test]
async fn lazy_prepared_cli_plans_preserve_persistent_bytes_and_backing_through_capacity_one_reopen()
{
    use crate::config::{SplitStorageConfig, StorageProvider};
    use mount_rs_core::FsDriver;
    use mount_rs_core::storage::{BlockStore, ConcurrentBackingId, MetadataStore};
    use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};

    struct RecordedConstructor {
        actual: CliRuntimeConstructor,
        calls: AtomicUsize,
        owners: Mutex<Vec<Weak<dyn ConstructionResource>>>,
    }
    struct ForwardObserver<'a> {
        journal: &'a dyn ConstructionObserver,
        last: Mutex<Option<Weak<dyn ConstructionResource>>>,
    }
    impl ConstructionObserver for ForwardObserver<'_> {
        fn retain(&self, resource: Arc<dyn ConstructionResource>) {
            *lock(&self.last) = Some(Arc::downgrade(&resource));
            self.journal.retain(resource);
        }
    }
    #[async_trait]
    impl RuntimeConstructor for RecordedConstructor {
        async fn construct(
            &self,
            observer: &dyn ConstructionObserver,
        ) -> Result<ConstructedRuntime> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let forwarding = ForwardObserver {
                journal: observer,
                last: Mutex::new(None),
            };
            let runtime = self.actual.construct(&forwarding).await?;
            // The actual prepared CLI constructor's final retain is its SDK
            // Filesystem, after provider construction and before driver wrapping.
            // Forward the real journal ownership; retain only a Weak observation.
            lock(&self.owners).push(lock(&forwarding.last).clone().unwrap());
            Ok(runtime)
        }
    }
    async fn full_file(driver: &Arc<dyn FsDriver>, expected: &[u8]) {
        let handle = driver.open("/file", "r", 0).await.unwrap();
        assert_eq!(handle.stat().await.unwrap().size, expected.len() as u64);
        let mut bytes = vec![0; expected.len()];
        let mut offset = 0;
        while offset < bytes.len() {
            let end = bytes.len().min(offset + 1379);
            let read = handle
                .read(&mut bytes[offset..end], Some(offset as u64))
                .await
                .unwrap();
            assert!(read > 0 && read <= end - offset);
            offset += read;
        }
        assert_eq!(bytes, expected);
        assert_eq!(
            handle
                .read(&mut [0; 17], Some(expected.len() as u64))
                .await
                .unwrap(),
            0
        );
        handle.close().await.unwrap();
    }
    async fn stored_backing(path: &std::path::Path) -> ConcurrentBackingId {
        let metadata = SqliteMetadataStore::open(path).unwrap();
        let backing = metadata
            .compact_inode_mode_state()
            .await
            .unwrap()
            .unwrap()
            .backing;
        let blocks = SqliteBlockStore::open(path).unwrap();
        blocks.verify_concurrent_backing(backing).await.unwrap();
        backing
    }
    async fn retired(owner: &Weak<dyn ConstructionResource>) {
        bounded(async {
            while owner.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    // On any failed ownership oracle keep the persistent fixture until the
    // whole owned test process exits; remove it only after acknowledged drain.
    let directory = tempfile::tempdir().unwrap().keep();
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let scope = keeper.reserve().unwrap();
    let context = StorageContext::new(2).unwrap();
    scope.0.install_context(context.clone());
    let pool = RuntimePool::new(1).unwrap();
    scope.0.install_pool(pool.clone());
    let mut constructors = Vec::new();
    let mut registrations = Vec::new();
    let paths: Vec<_> = (0..2)
        .map(|index| directory.join(format!("drive-{index}.sqlite")))
        .collect();
    for (index, path) in paths.iter().enumerate() {
        let provider = StorageProvider::Sqlite { path: path.clone() };
        let options = crate::CliOptions {
            driver: crate::DriverChoice::SplitStore,
            storage: Some(Box::new(SplitStorageConfig {
                metadata: provider.clone(),
                blocks: provider,
                chunk_size_bytes: 4096,
                lease_ttl_ms: None,
                concurrent_writes: true,
                inode_updates: true,
                compact_inode_updates: true,
                writeback: false,
                delegated: false,
                checkout_path: None,
                owner: Some(format!("cli-lazy-{index}")),
            })),
            ..crate::CliOptions::default()
        };
        let constructor = Arc::new(RecordedConstructor {
            actual: CliRuntimeConstructor {
                plan: crate::runtime::DriverRuntime::prepare(&options, 111, 222).unwrap(),
                context: context.clone(),
                decorator: None,
            },
            calls: AtomicUsize::new(0),
            owners: Mutex::new(Vec::new()),
        });
        let factory = SdkRuntimeFactory::new(constructor.clone());
        scope.0.retain_factory(factory.clone());
        registrations.push(pool.register(factory).unwrap());
        constructors.push(constructor);
        assert!(
            !path.exists(),
            "prepared CLI registration eagerly opened a provider"
        );
    }
    assert_eq!(pool.snapshot().resident, 0);
    let sibling = bounded(Filesystem::split_with_context(
        SplitOptions::memory("healthy-sibling", 4096),
        &context,
    ))
    .await
    .unwrap();
    let sibling_bytes = vec![93; 4099];
    sibling
        .driver()
        .write_file("/file", &sibling_bytes)
        .await
        .unwrap();
    let expected: Vec<Vec<u8>> = (0..2)
        .map(|seed| {
            (0..65_543)
                .map(|offset| ((offset * 19 + seed * 37 + offset / 101) % 256) as u8)
                .collect()
        })
        .collect();
    let mut identities = [None; 2];
    let mut previous = None;
    for index in [0, 1, 0] {
        let lease = bounded(registrations[index].acquire()).await.unwrap();
        if let Some(previous) = previous.take() {
            retired(&previous).await;
        }
        let owner = lock(&constructors[index].owners).last().unwrap().clone();
        if identities[index].is_none() {
            bounded(lease.driver().write_file("/file", &expected[index]))
                .await
                .unwrap();
            identities[index] = Some(bounded(stored_backing(&paths[index])).await);
        }
        bounded(full_file(lease.driver(), &expected[index])).await;
        assert_eq!(
            Some(bounded(stored_backing(&paths[index])).await),
            identities[index]
        );
        drop(lease);
        previous = Some(owner);
        bounded(full_file(&sibling.driver(), &sibling_bytes)).await;
        assert_eq!(pool.snapshot().resident, 1);
        assert_eq!(pool.snapshot().pinned, 0);
        assert_eq!(pool.snapshot().quarantined, 0);
    }
    assert_ne!(identities[0], identities[1]);
    assert_eq!(pool.snapshot().open_success, 3);
    assert_eq!(pool.snapshot().eviction_success, 2);
    assert_eq!(
        constructors
            .iter()
            .map(|constructor| constructor.calls.load(Ordering::SeqCst))
            .collect::<Vec<_>>(),
        [2, 1]
    );
    bounded(sibling.shutdown()).await.unwrap();
    bounded(scope.0.close()).await.unwrap();
    for constructor in &constructors {
        let owners = lock(&constructor.owners).clone();
        for owner in owners {
            retired(&owner).await;
        }
    }
    assert_eq!(pool.snapshot().resident, 0);
    assert!(keeper.is_empty());
    assert!(!context_usable(&context).await);
    std::fs::remove_dir_all(directory).unwrap();
}

async fn pool_is_pending(lifecycle: &RemoteRuntimeLifecycle) {
    bounded(async {
        loop {
            if lock(&lifecycle.owned).phase == ClosePhase::Pool {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

#[tokio::test]
async fn lazy_cancelled_close_waiter_joins_one_actual_sdk_drain_before_context_close() {
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let scope = keeper.reserve().unwrap();
    let context = StorageContext::new(2).unwrap();
    scope.0.install_context(context.clone());
    let pool = RuntimePool::new(1).unwrap();
    scope.0.install_pool(pool.clone());
    let factory = SdkRuntimeFactory::new(constructor(&context));
    scope.0.retain_factory(factory.clone());
    let registration = pool.register(factory.clone()).unwrap();
    let lease = bounded(registration.acquire()).await.unwrap();
    let payload: Vec<u8> = (0..65_543).map(|i| (i % 251) as u8).collect();
    lease.driver().write_file("/file", &payload).await.unwrap();
    let handle = lease.driver().open("/file", "r", 0).await.unwrap();
    let mut actual = vec![0; payload.len()];
    assert_eq!(
        handle.read(&mut actual, Some(0)).await.unwrap(),
        payload.len()
    );
    assert_eq!(actual, payload);
    assert_eq!(
        handle
            .read(&mut [0; 7], Some(payload.len() as u64))
            .await
            .unwrap(),
        0
    );
    handle.close().await.unwrap();
    drop(handle);
    let first = tokio::spawn({
        let lifecycle = scope.0.clone();
        async move { lifecycle.close().await }
    });
    pool_is_pending(&scope.0).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(
        context_usable(&context).await,
        "a pinned actual owner lost its shared context"
    );
    assert!(matches!(keeper.reserve(), Err(error) if error.code == ErrorCode::Ebusy));
    drop(lease);
    bounded(scope.0.close()).await.unwrap();
    bounded(scope.0.close()).await.unwrap();
    factory.close().await.unwrap();
    assert!(!context_usable(&context).await);
    assert_eq!(pool.snapshot().resident, 0);
    assert!(lock(&keeper.0).is_none());
}

#[tokio::test]
async fn lazy_dropping_serving_scope_keeps_actual_pinned_owner_until_one_drain_finishes() {
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let scope = keeper.reserve().unwrap();
    let lifecycle = scope.0.clone();
    let context = StorageContext::new(2).unwrap();
    lifecycle.install_context(context.clone());
    let pool = RuntimePool::new(1).unwrap();
    lifecycle.install_pool(pool.clone());
    let factory = SdkRuntimeFactory::new(constructor(&context));
    lifecycle.retain_factory(factory.clone());
    let registration = pool.register(factory).unwrap();
    let lease = bounded(registration.acquire()).await.unwrap();
    drop(scope);
    pool_is_pending(&lifecycle).await;
    assert!(context_usable(&context).await);
    assert!(lock(&keeper.0).is_some());
    drop(lease);
    bounded(lifecycle.close()).await.unwrap();
    assert!(!context_usable(&context).await);
    assert!(lock(&keeper.0).is_none());
}

struct RefusedCleanup;
#[async_trait]
impl ConstructionResource for RefusedCleanup {
    async fn close(&self) -> Result<()> {
        Err(FsError::new(ErrorCode::Eacces))
    }
}
struct RefusedConstructor;
#[async_trait]
impl RuntimeConstructor for RefusedConstructor {
    async fn construct(&self, observer: &dyn ConstructionObserver) -> Result<ConstructedRuntime> {
        observer.retain(Arc::new(Filesystem::memory(MemoryOptions::default())));
        observer.retain(Arc::new(RefusedCleanup));
        Err(FsError::new(ErrorCode::Enospc))
    }
}

#[tokio::test]
async fn lazy_constructor_and_cleanup_failure_retain_context_and_slot_without_retry() {
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let scope = keeper.reserve().unwrap();
    let context = StorageContext::new(2).unwrap();
    scope.0.install_context(context.clone());
    let pool = RuntimePool::new(1).unwrap();
    scope.0.install_pool(pool.clone());
    let factory = SdkRuntimeFactory::new(Arc::new(RefusedConstructor));
    scope.0.retain_factory(factory.clone());
    let registration = pool.register(factory.clone()).unwrap();
    assert!(
        matches!(bounded(registration.acquire()).await, Err(error) if error.code == ErrorCode::Enospc)
    );
    assert!(bounded(scope.0.close()).await.is_err());
    assert!(bounded(scope.0.close()).await.is_err());
    assert!(context_usable(&context).await);
    assert!(matches!(keeper.reserve(), Err(error) if error.code == ErrorCode::Ebusy));
    assert!(matches!(factory.open().await, Err(error) if error.code == ErrorCode::Enospc));
    assert_eq!(pool.snapshot().quarantined, 1);
    // Unacknowledged actual construction owners/dependencies survive this
    // negative control until the owned whole test process terminates.
    std::mem::forget(keeper);
}

struct PendingOwner(Arc<AtomicUsize>);
impl Drop for PendingOwner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn lazy_aborted_worker_retains_both_installed_consuming_futures_and_terminal_failure() {
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let scope = keeper.reserve().unwrap();
    let context = StorageContext::new(2).unwrap();
    scope.0.install_context(context.clone());
    let dropped = Arc::new(AtomicUsize::new(0));
    let entered = [Arc::new(Notify::new()), Arc::new(Notify::new())];
    let mut owners = Vec::new();
    for entered in &entered {
        let owner = Arc::new(PendingOwner(dropped.clone()));
        owners.push(Arc::downgrade(&owner));
        let entered = entered.clone();
        lock(&scope.0.owned)
            .listeners
            .push(Some(Box::pin(async move {
                let _actual_owner = owner;
                entered.notify_one();
                std::future::pending::<()>().await;
                Ok(())
            })));
    }
    scope.0.request_close();
    for entered in &entered {
        bounded(entered.notified()).await;
    }
    lock(&scope.0.worker).as_ref().unwrap().abort();
    assert!(matches!(bounded(scope.0.close()).await, Err(error) if error.code == ErrorCode::Eio));
    assert!(matches!(bounded(scope.0.close()).await, Err(error) if error.code == ErrorCode::Eio));
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert!(owners.iter().all(|owner| owner.upgrade().is_some()));
    assert!(lock(&scope.0.owned).listeners.iter().all(Option::is_some));
    assert!(context_usable(&context).await);
    assert!(matches!(keeper.reserve(), Err(error) if error.code == ErrorCode::Ebusy));
    // This tests the precise retained-future state with controlled owners; it
    // is not a claim that listener internals survived an internal poll panic.
    std::mem::forget(keeper);
}

#[cfg(unix)]
#[tokio::test]
async fn lazy_actual_server_cache_unacknowledged_io_keeps_directory_and_keeper() {
    use base64::Engine;
    use mount_rs_blob_cache::{LocalCache, LocalCacheConfig};
    // Preserve the fixture before installing an intentionally unsuccessful
    // owner; this control must not remove its files on a failed cleanup ACK.
    let directory = tempfile::tempdir().unwrap().keep();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let pem = |label: &str, der: &[u8]| {
        format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
            base64::engine::general_purpose::STANDARD.encode(der)
        )
    };
    std::fs::write(
        directory.join("certificate.pem"),
        pem("CERTIFICATE", cert.der()),
    )
    .unwrap();
    std::fs::write(
        directory.join("key.pem"),
        pem("PRIVATE KEY", &signing_key.serialize_der()),
    )
    .unwrap();
    let configuration: crate::server_cache::CacheServiceConfig = serde_json::from_value(serde_json::json!({
        "cluster":"actual-drain", "node_id":"node", "disk_path":"cache", "ram_bytes":64,
        "disk_bytes":256, "max_entries":10, "max_blob_bytes":16, "peer_listen":"127.0.0.1:0",
        "ca_certificate":"certificate.pem", "certificate":"certificate.pem", "private_key":"key.pem",
        "discovery":"peer-query", "peers":[]
    })).unwrap();
    let actual =
        Arc::new(ServerCache::start(&configuration, &directory.join("config.json")).unwrap());
    let cache_owner = Arc::downgrade(&actual.local);
    let server_owner = Arc::downgrade(&actual);
    // This is an actual admitted IO ticket, not a disk-body qualification.
    // The cache's separate real-worker controls exercise disk execution/joins.
    let ticket = actual.local.io_permit().await.unwrap();
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let scope = keeper.reserve().unwrap();
    let context = StorageContext::new(2).unwrap();
    scope.0.install_context(context.clone());
    scope.0.install_cache(actual);
    let result = bounded(scope.0.close()).await;
    drop(ticket);
    let repeated = bounded(scope.0.close()).await;
    let still_owned = LocalCache::new(LocalCacheConfig {
        directory: directory.join("cache"),
        memory_bytes: 64,
        disk_bytes: 256,
        max_entries: 10,
        max_blob_bytes: 16,
    })
    .is_err();
    assert!(result.is_err_and(|error| error.code == ErrorCode::Ebusy));
    assert!(repeated.is_err_and(|error| error.code == ErrorCode::Ebusy));
    assert!(server_owner.upgrade().is_some() && cache_owner.upgrade().is_some());
    assert!(
        still_owned,
        "unsuccessful cache cleanup released the real directory lock"
    );
    assert!(!context_usable(&context).await);
    assert!(matches!(keeper.reserve(), Err(error) if error.code == ErrorCode::Ebusy));
    // The actual failed owner remains in the keeper through process exit.
    std::mem::forget(keeper);
}

// These controls inject the cache lifecycle boundary. They prove keeper/result
// ownership, not that an actual LocalCache disk worker or QUIC driver drained.
struct CacheDrainSeam {
    calls: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Option<Arc<Notify>>,
    failure: Option<ErrorCode>,
    dropped: Arc<AtomicUsize>,
}

impl Drop for CacheDrainSeam {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ConstructionResource for CacheDrainSeam {
    async fn close(&self) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if let Some(release) = &self.release {
            release.notified().await;
        }
        self.failure.map_or(Ok(()), |code| Err(FsError::new(code)))
    }
}

#[tokio::test]
async fn lazy_cache_cleanup_seam_failure_retains_owner_and_keeper_without_retry() {
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let scope = keeper.reserve().unwrap();
    let context = StorageContext::new(2).unwrap();
    scope.0.install_context(context.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let cache = Arc::new(CacheDrainSeam {
        calls: calls.clone(),
        entered: Arc::new(Notify::new()),
        release: None,
        failure: Some(ErrorCode::Eacces),
        dropped: dropped.clone(),
    });
    let owner = Arc::downgrade(&cache);
    lock(&scope.0.owned).cache = Some(cache);

    for _ in 0..2 {
        assert!(
            matches!(bounded(scope.0.close()).await, Err(error) if error.code == ErrorCode::Eacces)
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert!(owner.upgrade().is_some());
    assert!(lock(&scope.0.owned).cache.is_some());
    assert!(
        !context_usable(&context).await,
        "context precedes cache close"
    );
    assert!(matches!(keeper.reserve(), Err(error) if error.code == ErrorCode::Ebusy));
    // The injected unsuccessful owner remains retained until process exit.
    std::mem::forget(keeper);
}

#[tokio::test]
async fn lazy_cancelled_cache_cleanup_seam_waiter_joins_one_owned_drain() {
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    let scope = keeper.reserve().unwrap();
    let context = StorageContext::new(2).unwrap();
    scope.0.install_context(context.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let cache = Arc::new(CacheDrainSeam {
        calls: calls.clone(),
        entered: entered.clone(),
        release: Some(release.clone()),
        failure: None,
        dropped: dropped.clone(),
    });
    let owner = Arc::downgrade(&cache);
    lock(&scope.0.owned).cache = Some(cache);
    let first = tokio::spawn({
        let lifecycle = scope.0.clone();
        async move { lifecycle.close().await }
    });
    bounded(entered.notified()).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert!(owner.upgrade().is_some());
    assert!(lock(&scope.0.owned).current.is_some());
    assert!(matches!(keeper.reserve(), Err(error) if error.code == ErrorCode::Ebusy));
    assert!(
        !context_usable(&context).await,
        "context precedes cache close"
    );

    release.notify_one();
    bounded(scope.0.close()).await.unwrap();
    bounded(scope.0.close()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(owner.upgrade().is_none());
    assert!(keeper.is_empty());
}
