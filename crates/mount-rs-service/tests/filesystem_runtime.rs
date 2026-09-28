//! Production SDK construction adapter controls; no provider or process mock
//! can stand in for the separate real persistent eviction and shipped CLI gate.
#![cfg(feature = "sdk-runtime")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_sdk::{Filesystem, MemoryOptions};
use mount_rs_service::filesystem_runtime::{
    ConstructedRuntime, RuntimeConstructor, SdkRuntimeFactory,
};
use mount_rs_service::runtime_pool::RuntimeFactory;
use tokio::sync::Notify;

const DEADLINE: Duration = Duration::from_secs(5);

// The shutdown-witness control uses the real SDK MRC5 owner and actual SQLite
// metadata/blocks. A forwarding decorator gates one immutable put, which holds
// the real lifecycle reader while actual shutdown queues its lifecycle writer.
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
mod actual_shutdown {
    use super::*;
    use mount_rs_core::storage::{BlockId, BlockStore, ConcurrentBackingId};
    use mount_rs_sdk::{BlockStoreDecorator, SplitOptions, StorageContext, StoreConfig};

    struct Hold {
        armed: std::sync::atomic::AtomicBool,
        entered: Notify,
        release: Notify,
    }
    struct Decorator(Arc<Hold>);
    struct Blocks {
        inner: Arc<dyn BlockStore>,
        hold: Arc<Hold>,
    }
    impl BlockStoreDecorator for Decorator {
        fn decorate(
            &self,
            _: &StoreConfig,
            inner: Arc<dyn BlockStore>,
        ) -> Result<Arc<dyn BlockStore>> {
            Ok(Arc::new(Blocks {
                inner,
                hold: self.0.clone(),
            }))
        }
    }
    #[async_trait]
    impl BlockStore for Blocks {
        fn durable(&self) -> bool {
            self.inner.durable()
        }
        async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
            self.inner.prepare_concurrent_backing().await
        }
        async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
            self.inner.verify_concurrent_backing(expected).await
        }
        async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
            self.inner.get_for_migration(id).await
        }
        async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
            if self.hold.armed.swap(false, Ordering::SeqCst) {
                self.hold.entered.notify_one();
                self.hold.release.notified().await;
            }
            self.inner.put(bytes).await
        }
        async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
            self.inner.get(id).await
        }
        async fn flush(&self) -> Result<()> {
            self.inner.flush().await
        }
        async fn delete(&self, id: &BlockId) -> Result<()> {
            self.inner.delete(id).await
        }
    }
    struct Constructor {
        options: SplitOptions,
        context: StorageContext,
        hold: Arc<Hold>,
        calls: AtomicUsize,
    }
    #[async_trait]
    impl RuntimeConstructor for Constructor {
        async fn construct(
            &self,
            observer: &dyn ConstructionObserver,
        ) -> Result<ConstructedRuntime> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let actual = Arc::new(
                Filesystem::split_with_construction_observer(
                    self.options.clone(),
                    Some(&self.context),
                    Some(&Decorator(self.hold.clone())),
                    observer,
                )
                .await?,
            );
            observer.retain(actual.clone());
            let driver = actual.driver();
            Ok(ConstructedRuntime::new(actual, driver))
        }
    }

    #[tokio::test]
    async fn cancelled_then_failed_actual_sdk_shutdown_never_authorizes_a_replacement() {
        let directory = tempfile::tempdir().unwrap().keep();
        let database = directory.join("actual.sqlite");
        let mut options = SplitOptions::memory("actual-shutdown-witness", 4096)
            .with_inode_updates(true)
            .with_compact_inode_updates(true);
        options.metadata = StoreConfig::Sqlite {
            path: database.clone(),
        };
        options.blocks = options.metadata.clone();
        let context = StorageContext::new(2).unwrap();
        let hold = Arc::new(Hold {
            armed: std::sync::atomic::AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
        });
        let constructor = Arc::new(Constructor {
            options,
            context: context.clone(),
            hold: hold.clone(),
            calls: AtomicUsize::new(0),
        });
        let factory = SdkRuntimeFactory::new(constructor.clone());
        let owner = factory.open().await.unwrap();
        let payload: Vec<u8> = (0..8193).map(|offset| (offset % 251) as u8).collect();
        hold.armed.store(true, Ordering::SeqCst);
        let writer = {
            let driver = owner.driver();
            let payload = payload.clone();
            tokio::spawn(async move { driver.write_file("/acknowledged", &payload).await })
        };
        signal(&hold.entered).await;
        let mut shutdown = Box::pin(owner.shutdown());
        assert!(futures_util::poll!(shutdown.as_mut()).is_pending());
        drop(shutdown);
        refused(&factory, ErrorCode::Ebusy).await;
        assert_eq!(constructor.calls.load(Ordering::SeqCst), 1);
        hold.release.notify_one();
        tokio::time::timeout(DEADLINE, writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        owner.shutdown().await.unwrap();
        drop(owner);
        let reopened = factory.open().await.unwrap();
        assert_eq!(constructor.calls.load(Ordering::SeqCst), 2);
        let handle = reopened
            .driver()
            .open("/acknowledged", "r", 0)
            .await
            .unwrap();
        assert_eq!(handle.stat().await.unwrap().size, payload.len() as u64);
        let mut actual = vec![0; payload.len()];
        let mut offset = 0;
        while offset < actual.len() {
            let count = handle
                .read(&mut actual[offset..], Some(offset as u64))
                .await
                .unwrap();
            assert!(count > 0 && count <= actual.len() - offset);
            offset += count;
        }
        assert_eq!(actual, payload);
        assert_eq!(
            handle
                .read(&mut [0; 17], Some(payload.len() as u64))
                .await
                .unwrap(),
            0
        );
        handle.close().await.unwrap();
        drop(handle);
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_actual_owner_publication
            BEFORE UPDATE ON mount_rs_compact_guards
            BEGIN SELECT RAISE(ABORT, 'actual owner publication rejected'); END;",
            )
            .unwrap();
        assert_eq!(
            reopened
                .driver()
                .write_file("/acknowledged", b"different")
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        drop(connection);
        assert!(reopened.failed());
        assert_eq!(reopened.shutdown().await.unwrap_err().code, ErrorCode::Eio);
        refused(&factory, ErrorCode::Ebusy).await;
        assert_eq!(closed(&factory).await.unwrap_err().code, ErrorCode::Ebusy);
        assert_eq!(constructor.calls.load(Ordering::SeqCst), 2);
        // The negative control leaves real publication authority uncertain.
        // Retain it and its context until test-process termination; this is
        // neither acknowledged close nor permission to delete its fixture.
        std::mem::forget((reopened, factory, constructor, context));
        // `directory` is already kept; an owned parent may retire it only after
        // the entire test process has settled. This test never removes it.
    }
}

struct Resource {
    calls: AtomicUsize,
    entered: Notify,
    release: Notify,
    gate: bool,
    failure: bool,
    panic: bool,
}

impl Resource {
    fn new(gate: bool, failure: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Notify::new(),
            gate,
            failure,
            panic: false,
        })
    }
}

#[async_trait]
impl ConstructionResource for Resource {
    async fn close(&self) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.gate {
            self.release.notified().await;
        }
        if self.panic {
            panic!("controlled cleanup panic");
        }
        if self.failure {
            Err(FsError::new(ErrorCode::Eacces))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum Outcome {
    Pending,
    Failure,
    Panic,
    Success,
}

struct Constructor {
    calls: AtomicUsize,
    entered: Notify,
    outcome: Outcome,
    resource: Mutex<Option<Arc<Resource>>>,
    actual: Mutex<Vec<Weak<Filesystem>>>,
}

impl Constructor {
    fn new(outcome: Outcome, resource: Arc<Resource>) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            outcome,
            resource: Mutex::new(Some(resource)),
            actual: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl RuntimeConstructor for Constructor {
    async fn construct(&self, observer: &dyn ConstructionObserver) -> Result<ConstructedRuntime> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !matches!(self.outcome, Outcome::Success) {
            observer.retain(self.resource.lock().unwrap().take().unwrap());
        }
        self.entered.notify_one();
        match self.outcome {
            Outcome::Pending => std::future::pending().await,
            Outcome::Failure => Err(FsError::new(ErrorCode::Enospc)),
            Outcome::Panic => panic!("controlled constructor panic"),
            Outcome::Success => {
                let actual = Arc::new(Filesystem::memory(MemoryOptions::default()));
                observer.retain(actual.clone());
                self.actual.lock().unwrap().push(Arc::downgrade(&actual));
                let driver = actual.driver();
                Ok(ConstructedRuntime::new(actual, driver))
            }
        }
    }
}

async fn signal(signal: &Notify) {
    tokio::time::timeout(DEADLINE, signal.notified())
        .await
        .unwrap();
}

async fn closed(factory: &SdkRuntimeFactory) -> Result<()> {
    tokio::time::timeout(DEADLINE, factory.close())
        .await
        .unwrap()
}

async fn refused(factory: &SdkRuntimeFactory, code: ErrorCode) {
    match factory.open().await {
        Ok(_) => panic!("unexpected replacement construction"),
        Err(error) => assert_eq!(error.code, code),
    }
}

#[tokio::test]
async fn factory_retains_before_first_poll_and_abandoned_open_is_nonretryable() {
    let resource = Resource::new(false, false);
    let constructor = Constructor::new(Outcome::Pending, resource.clone());
    let factory = SdkRuntimeFactory::new(constructor.clone());
    let waiter = {
        let factory = factory.clone();
        tokio::spawn(async move { factory.open().await })
    };
    signal(&constructor.entered).await;
    let snapshot = factory.construction_snapshot().unwrap();
    assert!(snapshot.opening);
    assert_eq!(snapshot.retained_resources, 1);
    refused(&factory, ErrorCode::Ebusy).await;
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    let snapshot = factory.construction_snapshot().unwrap();
    assert!(snapshot.uncertain && !snapshot.cleanup_complete);
    assert_eq!(snapshot.retained_resources, 1);
    assert_eq!(closed(&factory).await.unwrap_err().code, ErrorCode::Eio);
    refused(&factory, ErrorCode::Eio).await;
    assert_eq!(constructor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(resource.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cleanup_panic_preserves_primary_error_and_retains_registered_resource() {
    let mut resource = Resource::new(false, false);
    Arc::get_mut(&mut resource).unwrap().panic = true;
    let weak = Arc::downgrade(&resource);
    let constructor = Constructor::new(Outcome::Failure, resource.clone());
    let factory = SdkRuntimeFactory::new(constructor.clone());
    refused(&factory, ErrorCode::Enospc).await;
    assert_eq!(closed(&factory).await.unwrap_err().code, ErrorCode::Eio);
    assert_eq!(resource.calls.load(Ordering::SeqCst), 1);
    drop(resource);
    assert!(
        weak.upgrade().is_some(),
        "failed cleanup lost its registered owner"
    );
    refused(&factory, ErrorCode::Enospc).await;
    assert_eq!(constructor.calls.load(Ordering::SeqCst), 1);
    assert!(factory.construction_snapshot().unwrap().uncertain);
    assert_eq!(closed(&factory).await.unwrap_err().code, ErrorCode::Eio);
    assert_eq!(weak.upgrade().unwrap().calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn owner_drop_without_shutdown_never_invents_a_close_receipt() {
    let constructor = Constructor::new(Outcome::Success, Resource::new(false, false));
    let factory = SdkRuntimeFactory::new(constructor.clone());
    let owner = factory.open().await.unwrap();
    let weak = constructor.actual.lock().unwrap()[0].clone();
    drop(owner);
    assert!(weak.upgrade().is_none());
    refused(&factory, ErrorCode::Ebusy).await;
    assert_eq!(closed(&factory).await.unwrap_err().code, ErrorCode::Ebusy);
    assert_eq!(constructor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn primary_constructor_error_survives_authority_cleanup_failure_and_close_rejoin() {
    let resource = Resource::new(false, true);
    let constructor = Constructor::new(Outcome::Failure, resource.clone());
    let factory = SdkRuntimeFactory::new(constructor.clone());
    refused(&factory, ErrorCode::Enospc).await;
    assert_eq!(closed(&factory).await.unwrap_err().code, ErrorCode::Eacces);
    assert_eq!(closed(&factory).await.unwrap_err().code, ErrorCode::Eacces);
    refused(&factory, ErrorCode::Enospc).await;
    assert_eq!(constructor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(resource.calls.load(Ordering::SeqCst), 1);
    let snapshot = factory.construction_snapshot().unwrap();
    assert!(snapshot.uncertain && !snapshot.cleanup_complete);
    assert_eq!(snapshot.retained_resources, 1);
}

#[tokio::test]
async fn canceling_failure_cleanup_waiters_does_not_cancel_or_repeat_its_owned_cleanup() {
    let resource = Resource::new(true, false);
    let constructor = Constructor::new(Outcome::Failure, resource.clone());
    let factory = SdkRuntimeFactory::new(constructor.clone());
    let open = {
        let factory = factory.clone();
        tokio::spawn(async move { factory.open().await })
    };
    signal(&resource.entered).await;
    open.abort();
    assert!(matches!(open.await, Err(error) if error.is_cancelled()));
    let mut close = Box::pin(factory.close());
    assert!(futures_util::poll!(close.as_mut()).is_pending());
    drop(close);
    assert_eq!(resource.calls.load(Ordering::SeqCst), 1);
    resource.release.notify_one();
    closed(&factory).await.unwrap();
    closed(&factory).await.unwrap();
    assert!(factory.construction_snapshot().unwrap().cleanup_complete);
    assert_eq!(resource.calls.load(Ordering::SeqCst), 1);
    refused(&factory, ErrorCode::Enospc).await;
}

#[tokio::test]
async fn panicking_constructor_retains_its_registered_owner_and_no_retry() {
    let resource = Resource::new(false, false);
    let constructor = Constructor::new(Outcome::Panic, resource.clone());
    let factory = SdkRuntimeFactory::new(constructor.clone());
    let task = {
        let factory = factory.clone();
        tokio::spawn(async move { factory.open().await })
    };
    assert!(matches!(task.await, Err(error) if error.is_panic()));
    let snapshot = factory.construction_snapshot().unwrap();
    assert!(snapshot.uncertain);
    assert_eq!(snapshot.retained_resources, 1);
    refused(&factory, ErrorCode::Eio).await;
    assert_eq!(closed(&factory).await.unwrap_err().code, ErrorCode::Eio);
    assert_eq!(resource.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn actual_filesystem_owner_handoff_requires_acknowledged_close_before_next_generation() {
    let resource = Resource::new(false, false);
    let constructor = Constructor::new(Outcome::Success, resource.clone());
    let factory = SdkRuntimeFactory::new(constructor.clone());
    let owner = factory.open().await.unwrap();
    let weak = constructor.actual.lock().unwrap()[0].clone();
    assert!(weak.upgrade().is_some());
    assert!(factory.construction_snapshot().is_none());
    // A selected driver is a forwarding interface, not close permission.
    owner
        .driver()
        .write_file("/acknowledged", b"binary\0payload")
        .await
        .unwrap();
    refused(&factory, ErrorCode::Ebusy).await;
    owner.shutdown().await.unwrap();
    drop(owner);
    assert!(weak.upgrade().is_none());
    let second = factory.open().await.unwrap();
    assert_eq!(constructor.calls.load(Ordering::SeqCst), 2);
    second.shutdown().await.unwrap();
    drop(second);
    closed(&factory).await.unwrap();
    refused(&factory, ErrorCode::Ebadf).await;
}
