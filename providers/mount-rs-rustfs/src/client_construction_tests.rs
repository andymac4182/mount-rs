//! Controlled factories run through the production reservation, worker, join,
//! claim and disposal path. These tests issue no backing-service requests.

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::task::Poll;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::{ErrorCode, FsError, Result};
use object_store::memory::InMemory;
use object_store::path::Path;
use object_store::{
    GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
    PutMultipartOptions, PutOptions, PutPayload, PutResult,
};
use tokio::sync::{Notify, oneshot};

use super::client_construction::RustFsConstructionContext;
use super::{RustFsBlockStore, RustFsConfig, SignedClientBundle};

const SAFETY_DEADLINE: Duration = Duration::from_secs(5);
type Factory = Box<dyn FnOnce() -> Result<RustFsBlockStore> + Send>;

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(SAFETY_DEADLINE, future)
        .await
        .expect("controlled construction did not settle")
}

async fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut future = future;
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

fn failed<T>(result: Result<T>, code: ErrorCode) {
    match result {
        Ok(_) => panic!("operation unexpectedly succeeded"),
        Err(error) => assert_eq!(error.code, code),
    }
}

#[derive(Default)]
struct Gate {
    entered: Notify,
    released: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn hold(&self) {
        self.entered.notify_one();
        // A failed assertion cannot leave a blocking worker parked forever.
        // This is a safety bound, never a measured scheduling requirement.
        let (released, _) = self
            .changed
            .wait_timeout_while(
                self.released.lock().unwrap(),
                SAFETY_DEADLINE * 2,
                |released| !*released,
            )
            .unwrap();
        let was_released = *released;
        drop(released);
        assert!(was_released, "controlled worker was never released");
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

struct HeldFactory {
    gate: Arc<Gate>,
    calls: Arc<AtomicUsize>,
    product: Arc<Mutex<Option<Weak<InMemory>>>>,
}

impl HeldFactory {
    fn new() -> Self {
        Self {
            gate: Arc::new(Gate::default()),
            calls: Arc::new(AtomicUsize::new(0)),
            product: Arc::new(Mutex::new(None)),
        }
    }

    fn factory(&self) -> Factory {
        let gate = self.gate.clone();
        let calls = self.calls.clone();
        let product = self.product.clone();
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            gate.hold();
            let store = Arc::new(InMemory::new());
            *product.lock().unwrap() = Some(Arc::downgrade(&store));
            RustFsBlockStore::new(store, "held-constructor", false)
        })
    }

    async fn entered(&self) {
        bounded(self.gate.entered.notified()).await;
        assert_eq!(self.calls.load(Ordering::SeqCst), 1);
    }

    fn assert_disposed(&self) {
        let product = self.product.lock().unwrap();
        let weak = product
            .as_ref()
            .expect("factory did not create its product");
        assert!(weak.upgrade().is_none(), "unclaimed client remains alive");
    }
}

impl Drop for HeldFactory {
    fn drop(&mut self) {
        self.gate.release();
    }
}

fn immediate_factory() -> Factory {
    Box::new(|| RustFsBlockStore::new(Arc::new(InMemory::new()), "immediate", false))
}

#[derive(Default)]
struct RecordingObserver {
    resources: Mutex<Vec<Arc<dyn ConstructionResource>>>,
}

impl ConstructionObserver for RecordingObserver {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        self.resources.lock().unwrap().push(resource);
    }
}

impl RecordingObserver {
    fn resource(&self) -> Arc<dyn ConstructionResource> {
        let resources = self.resources.lock().unwrap();
        assert_eq!(resources.len(), 1);
        resources[0].clone()
    }
}

fn spawn_build(
    context: &RustFsConstructionContext,
    factory: Factory,
) -> tokio::task::JoinHandle<Result<RustFsBlockStore>> {
    let context = context.clone();
    tokio::spawn(async move { context.build_test(factory, None).await })
}

async fn cancel_build(task: tokio::task::JoinHandle<Result<RustFsBlockStore>>) {
    task.abort();
    match bounded(task).await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("held constructor completed before cancellation"),
    }
}

// Signal after the actual close has polled its retained join handle Pending.
// Waiting for this signal makes the latest-waiter cancellation test independent
// of executor scheduling and avoids assuming an instant build first yields.
async fn spawn_pending_close(
    context: &RustFsConstructionContext,
) -> tokio::task::JoinHandle<Result<()>> {
    let context = context.clone();
    let (pending_tx, pending_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut close = Box::pin(context.close());
        let mut pending_tx = Some(pending_tx);
        poll_fn(|cx| {
            let outcome = close.as_mut().poll(cx);
            if outcome.is_pending()
                && let Some(signal) = pending_tx.take()
            {
                let _ = signal.send(());
            }
            outcome
        })
        .await
    });
    bounded(pending_rx).await.unwrap();
    task
}

#[tokio::test(flavor = "current_thread")]
async fn held_builder_allows_current_thread_heartbeat() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldFactory::new();
    let build = spawn_build(&context, held.factory());
    held.entered().await;
    let heartbeat = tokio::spawn(async { tokio::task::yield_now().await });
    bounded(heartbeat).await.unwrap();
    assert!(!build.is_finished());
    assert_eq!(context.test_snapshot().charged, 1);
    held.gate.release();
    let product = bounded(build).await.unwrap().unwrap();
    bounded(context.close()).await.unwrap();
    assert!(
        held.product
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .upgrade()
            .is_some()
    );
    drop(product);
    held.assert_disposed();
}

#[tokio::test]
async fn canceled_build_is_joined_and_disposed_before_capacity_reuse() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldFactory::new();
    let observer = Arc::new(RecordingObserver::default());
    let owner = context.clone();
    let journal = observer.clone();
    let factory = held.factory();
    let build = tokio::spawn(async move { owner.build_test(factory, Some(&*journal)).await });
    held.entered().await;
    let ticket = observer.resource();
    cancel_build(build).await;
    assert_eq!(context.test_snapshot().retained, 1);
    assert_eq!(context.test_snapshot().charged, 1);

    let next_calls = Arc::new(AtomicUsize::new(0));
    let calls = next_calls.clone();
    let predecessor = held.product.clone();
    let mut next = Box::pin(context.build_test(
        Box::new(move || {
            assert!(
                predecessor
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .upgrade()
                    .is_none(),
                "replacement factory entered before predecessor disposal"
            );
            calls.fetch_add(1, Ordering::SeqCst);
            immediate_factory()()
        }),
        None,
    ));
    assert!(poll_once(next.as_mut()).await.is_pending());
    assert_eq!(next_calls.load(Ordering::SeqCst), 0);
    held.gate.release();
    drop(bounded(next).await.unwrap());
    held.assert_disposed();
    assert_eq!(next_calls.load(Ordering::SeqCst), 1);
    assert_eq!(context.test_snapshot().charged, 0);
    bounded(ticket.close()).await.unwrap();
    bounded(context.close()).await.unwrap();
}

#[tokio::test]
async fn canceled_close_resumes_same_retained_worker() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldFactory::new();
    let build = spawn_build(&context, held.factory());
    held.entered().await;
    cancel_build(build).await;
    let mut close = Box::pin(context.close());
    assert!(poll_once(close.as_mut()).await.is_pending());
    drop(close);
    assert!(context.test_snapshot().sealed);
    assert_eq!(context.test_snapshot().charged, 1);
    let resumed = spawn_pending_close(&context).await;
    held.gate.release();
    bounded(resumed).await.unwrap().unwrap();
    held.assert_disposed();
    assert_eq!(held.calls.load(Ordering::SeqCst), 1);
    assert_eq!(context.test_snapshot().charged, 0);
    assert_eq!(context.test_snapshot().retained, 0);
    bounded(context.close()).await.unwrap();
}

#[tokio::test]
async fn canceling_latest_close_waiter_does_not_replace_owner_waker() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldFactory::new();
    let build = spawn_build(&context, held.factory());
    held.entered().await;
    cancel_build(build).await;
    let earlier = spawn_pending_close(&context).await;
    let mut latest = Box::pin(context.close());
    assert!(poll_once(latest.as_mut()).await.is_pending());
    drop(latest);
    held.gate.release();
    // Bound the spawned task's handle: timeout cannot repoll the close future
    // and accidentally compensate for a stolen/lost worker wake.
    bounded(earlier).await.unwrap().unwrap();
    held.assert_disposed();
}

struct HeldObserver {
    journal: RecordingObserver,
    gate: Arc<Gate>,
}

impl ConstructionObserver for HeldObserver {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        self.journal.retain(resource);
        self.gate.hold();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seal_during_observer_registration_waits_and_prevents_worker_spawn() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldFactory::new();
    let observer = Arc::new(HeldObserver {
        journal: RecordingObserver::default(),
        gate: held.gate.clone(),
    });
    let owner = context.clone();
    let journal = observer.clone();
    let calls = held.calls.clone();
    let build = tokio::spawn(async move {
        owner
            .build_test(
                Box::new(move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    immediate_factory()()
                }),
                Some(&*journal),
            )
            .await
    });
    bounded(held.gate.entered.notified()).await;
    let ticket = observer.journal.resource();
    context.seal_admission().unwrap();
    let mut close = Box::pin(context.close());
    assert!(poll_once(close.as_mut()).await.is_pending());
    assert_eq!(context.test_snapshot().charged, 1);
    held.gate.release();
    assert!(bounded(build).await.unwrap().is_err());
    bounded(close).await.unwrap();
    bounded(ticket.close()).await.unwrap();
    assert_eq!(held.calls.load(Ordering::SeqCst), 0);
    assert_eq!(context.test_snapshot().charged, 0);
}

#[tokio::test]
async fn claim_before_seal_transfers_exactly_one_product() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldFactory::new();
    let build = spawn_build(&context, held.factory());
    held.entered().await;
    held.gate.release();
    let product = bounded(build).await.unwrap().unwrap();
    context.seal_admission().unwrap();
    bounded(context.close()).await.unwrap();
    bounded(context.close()).await.unwrap();
    assert_eq!(context.test_snapshot().charged, 0);
    assert!(
        held.product
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .upgrade()
            .is_some()
    );
    drop(product);
    held.assert_disposed();
}

#[tokio::test]
async fn seal_before_claim_disposes_result_and_wakes_waiting_admission() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldFactory::new();
    let build = spawn_build(&context, held.factory());
    held.entered().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut admission = Box::pin(context.build_test(
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            immediate_factory()()
        }),
        None,
    ));
    assert!(poll_once(admission.as_mut()).await.is_pending());
    context.seal_admission().unwrap();
    assert!(bounded(admission).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    held.gate.release();
    assert!(bounded(build).await.unwrap().is_err());
    bounded(context.close()).await.unwrap();
    held.assert_disposed();
    assert_eq!(context.test_snapshot().charged, 0);
}

#[tokio::test]
async fn typed_factory_error_is_preserved_and_recovers_capacity() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let result = bounded(context.build_test(
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(FsError::new(ErrorCode::Enospc).with_message("controlled build failure"))
        }),
        None,
    ))
    .await;
    match result {
        Ok(_) => panic!("typed failure became success"),
        Err(error) => {
            assert_eq!(error.code, ErrorCode::Enospc);
            assert_eq!(error.to_string(), "controlled build failure");
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(context.test_snapshot().charged, 0);
    drop(
        bounded(context.build_test(immediate_factory(), None))
            .await
            .unwrap(),
    );
    bounded(context.close()).await.unwrap();
}

async fn quarantine_still_drains_other_worker(poison: bool) {
    let context = RustFsConstructionContext::new(2).unwrap();
    let bad = HeldFactory::new();
    let bad_factory = if poison {
        bad.factory()
    } else {
        let gate = bad.gate.clone();
        let calls = bad.calls.clone();
        Box::new(move || -> Result<RustFsBlockStore> {
            calls.fetch_add(1, Ordering::SeqCst);
            gate.hold();
            panic!("controlled constructor panic");
        }) as Factory
    };
    let bad_build = spawn_build(&context, bad_factory);
    bad.entered().await;
    let good = HeldFactory::new();
    let good_build = spawn_build(&context, good.factory());
    good.entered().await;
    cancel_build(good_build).await;
    if poison {
        context.test_poison_ticket(0);
    }
    bad.gate.release();
    failed(bounded(bad_build).await.unwrap(), ErrorCode::Eio);
    assert_eq!(context.test_snapshot().charged, 2);
    failed(
        bounded(context.build_test(immediate_factory(), None)).await,
        ErrorCode::Eio,
    );
    let drain = spawn_pending_close(&context).await;
    good.gate.release();
    failed(bounded(drain).await.unwrap(), ErrorCode::Eio);
    good.assert_disposed();
    assert_eq!(good.calls.load(Ordering::SeqCst), 1);
    assert_eq!(context.test_snapshot().charged, 1);
    assert_eq!(context.test_snapshot().retained, 1);
    failed(bounded(context.close()).await, ErrorCode::Eio);
}

#[tokio::test]
async fn worker_panic_quarantines_capacity_but_close_joins_other_workers() {
    quarantine_still_drains_other_worker(false).await;
}

#[tokio::test]
async fn poisoned_ticket_quarantines_capacity_but_close_joins_other_workers() {
    quarantine_still_drains_other_worker(true).await;
}

#[tokio::test]
async fn invalid_owned_prefix_is_rejected_before_admission() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let observer = RecordingObserver::default();
    let config = RustFsConfig {
        endpoint: "http://127.0.0.1:1".into(),
        bucket: "construction-test".into(),
        access_key_id: "controlled-test-key".into(),
        secret_access_key: "controlled-test-secret".into(),
        region: "us-east-1".into(),
    };
    for prefix in ["", "/leading", "trailing/", "a//b", "a/../b", "a?b"] {
        failed(
            bounded(context.owned_prefix_probe(config.clone(), prefix.into(), Some(&observer)))
                .await,
            ErrorCode::Einval,
        );
        assert_eq!(context.test_snapshot().retained, 0);
        assert_eq!(context.test_snapshot().charged, 0);
        assert!(observer.resources.lock().unwrap().is_empty());
    }
    bounded(context.close()).await.unwrap();
}

#[test]
fn zero_construction_capacity_is_rejected() {
    failed(RustFsConstructionContext::new(0), ErrorCode::Einval);
}

#[tokio::test]
async fn owned_probe_constructor_error_preserves_existing_generic_error() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let observer = RecordingObserver::default();
    let config = RustFsConfig {
        endpoint: "invalid".into(),
        bucket: "construction-test".into(),
        access_key_id: "controlled-test-key".into(),
        secret_access_key: "controlled-test-secret".into(),
        region: "us-east-1".into(),
    };
    let error = bounded(context.owned_prefix_probe(config, "owned/prefix".into(), Some(&observer)))
        .await
        .err()
        .expect("invalid configuration must fail construction");
    assert_eq!(error.code, ErrorCode::Eio);
    assert_eq!(
        error.to_string(),
        "RustFS owned prefix observation client construction failed"
    );
    assert_eq!(context.test_snapshot().charged, 0);
    bounded(observer.resource().close()).await.unwrap();
    bounded(context.close()).await.unwrap();
}

// This wrapper's destructor is the controlled event. Backend methods must
// never run during pure construction, cancellation or disposal.
struct DropHeldStore {
    gate: Arc<Gate>,
    completed_drops: Arc<AtomicUsize>,
}

impl std::fmt::Debug for DropHeldStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DropHeldStore")
    }
}

impl std::fmt::Display for DropHeldStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DropHeldStore")
    }
}

impl Drop for DropHeldStore {
    fn drop(&mut self) {
        self.gate.hold();
        self.completed_drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ObjectStore for DropHeldStore {
    async fn put_opts(
        &self,
        _: &Path,
        _: PutPayload,
        _: PutOptions,
    ) -> object_store::Result<PutResult> {
        unreachable!("pure construction performed a backing write")
    }

    async fn put_multipart_opts(
        &self,
        _: &Path,
        _: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
        unreachable!("pure construction performed a multipart write")
    }

    async fn get_opts(&self, _: &Path, _: GetOptions) -> object_store::Result<GetResult> {
        unreachable!("pure construction performed a backing read")
    }

    async fn delete(&self, _: &Path) -> object_store::Result<()> {
        unreachable!("pure construction performed a backing delete")
    }

    fn list(&self, _: Option<&Path>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
        unreachable!("pure construction performed a backing list")
    }

    async fn list_with_delimiter(&self, _: Option<&Path>) -> object_store::Result<ListResult> {
        unreachable!("pure construction performed a backing list")
    }

    async fn copy(&self, _: &Path, _: &Path) -> object_store::Result<()> {
        unreachable!("pure construction performed a backing copy")
    }

    async fn copy_if_not_exists(&self, _: &Path, _: &Path) -> object_store::Result<()> {
        unreachable!("pure construction performed a conditional backing copy")
    }
}

struct ReleaseGateOnDrop(Arc<Gate>);

impl Drop for ReleaseGateOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

async fn spawn_pending_build(
    context: &RustFsConstructionContext,
    factory: Factory,
) -> tokio::task::JoinHandle<Result<RustFsBlockStore>> {
    let context = context.clone();
    let (pending_tx, pending_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut build = Box::pin(context.build_test(factory, None));
        let mut pending_tx = Some(pending_tx);
        poll_fn(|cx| {
            let outcome = build.as_mut().poll(cx);
            if outcome.is_pending()
                && let Some(signal) = pending_tx.take()
            {
                let _ = signal.send(());
            }
            outcome
        })
        .await
    });
    bounded(pending_rx).await.unwrap();
    task
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_product_destructor_holds_charge_until_physical_disposal_returns() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let factory_hold = HeldFactory::new();
    let drop_gate = Arc::new(Gate::default());
    let _drop_release = ReleaseGateOnDrop(drop_gate.clone());
    let completed_drops = Arc::new(AtomicUsize::new(0));
    let observer = Arc::new(RecordingObserver::default());
    let gate = factory_hold.gate.clone();
    let calls = factory_hold.calls.clone();
    let disposal_gate = drop_gate.clone();
    let drops = completed_drops.clone();
    let owner = context.clone();
    let journal = observer.clone();
    let build = tokio::spawn(async move {
        owner
            .build_test(
                Box::new(move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    gate.hold();
                    RustFsBlockStore::new(
                        Arc::new(DropHeldStore {
                            gate: disposal_gate,
                            completed_drops: drops,
                        }),
                        "held-product-disposal",
                        false,
                    )
                }),
                Some(&*journal),
            )
            .await
    });
    factory_hold.entered().await;
    cancel_build(build).await;
    let ticket = observer.resource();
    let drain = tokio::spawn(async move { ticket.close().await });
    factory_hold.gate.release();
    bounded(drop_gate.entered.notified()).await;
    assert_eq!(completed_drops.load(Ordering::SeqCst), 0);
    assert_eq!(context.test_snapshot().charged, 1);
    assert_eq!(context.test_snapshot().retained, 1);

    let next_calls = Arc::new(AtomicUsize::new(0));
    let counter = next_calls.clone();
    let drops = completed_drops.clone();
    let replacement = spawn_pending_build(
        &context,
        Box::new(move || {
            assert_eq!(
                drops.load(Ordering::SeqCst),
                1,
                "replacement entered before predecessor destructor completed"
            );
            counter.fetch_add(1, Ordering::SeqCst);
            immediate_factory()()
        }),
    )
    .await;
    assert_eq!(next_calls.load(Ordering::SeqCst), 0);
    assert_eq!(context.test_snapshot().charged, 1);
    assert!(!drain.is_finished());
    drop_gate.release();
    bounded(drain).await.unwrap().unwrap();
    drop(bounded(replacement).await.unwrap().unwrap());
    assert_eq!(completed_drops.load(Ordering::SeqCst), 1);
    assert_eq!(next_calls.load(Ordering::SeqCst), 1);
    assert_eq!(context.test_snapshot().charged, 0);
    bounded(context.close()).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn production_ticket_poison_recovery_wakes_caller_before_worker_release() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldFactory::new();
    let build = spawn_build(&context, held.factory());
    held.entered().await;
    context.test_poison_ticket(0);
    // This calls the real lock-recovery path. The poison hook itself must not
    // notify a waiter, and the held worker cannot supply a completion wake.
    assert_eq!(context.test_snapshot().charged, 1);
    failed(bounded(build).await.unwrap(), ErrorCode::Eio);
    assert!(held.product.lock().unwrap().is_none());
    assert!(!*held.gate.released.lock().unwrap());
    let close = spawn_pending_close(&context).await;
    held.gate.release();
    failed(bounded(close).await.unwrap(), ErrorCode::Eio);
    held.assert_disposed();
    assert_eq!(context.test_snapshot().charged, 1);
    assert_eq!(context.test_snapshot().retained, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn registry_poison_wakes_queued_admission_and_still_joins_both_workers() {
    let context = RustFsConstructionContext::new(2).unwrap();
    let first = HeldFactory::new();
    let first_build = spawn_build(&context, first.factory());
    first.entered().await;
    let second = HeldFactory::new();
    let second_build = spawn_build(&context, second.factory());
    second.entered().await;
    let next_calls = Arc::new(AtomicUsize::new(0));
    let calls = next_calls.clone();
    let queued = spawn_pending_build(
        &context,
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            immediate_factory()()
        }),
    )
    .await;
    context.test_poison_registry();
    let snapshot = context.test_snapshot();
    assert!(snapshot.sealed);
    assert_eq!(snapshot.charged, 2);
    failed(bounded(queued).await.unwrap(), ErrorCode::Eio);
    assert_eq!(next_calls.load(Ordering::SeqCst), 0);
    assert!(!*first.gate.released.lock().unwrap());
    assert!(!*second.gate.released.lock().unwrap());

    let close = spawn_pending_close(&context).await;
    first.gate.release();
    assert!(bounded(first_build).await.unwrap().is_err());
    first.assert_disposed();
    assert!(
        !close.is_finished(),
        "drain skipped the second retained worker"
    );
    assert_eq!(context.test_snapshot().charged, 1);
    second.gate.release();
    assert!(bounded(second_build).await.unwrap().is_err());
    failed(bounded(close).await.unwrap(), ErrorCode::Eio);
    second.assert_disposed();
    assert_eq!(context.test_snapshot().charged, 0);
    failed(bounded(context.close()).await, ErrorCode::Eio);
    failed(
        bounded(context.build_test(immediate_factory(), None)).await,
        ErrorCode::Eio,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn final_context_drop_retains_actual_ticket_without_retaining_owner_cycle() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let owner_alive = context.test_owner_liveness();
    let held = HeldFactory::new();
    let observer = Arc::new(RecordingObserver::default());
    let owner = context.clone();
    let journal = observer.clone();
    let factory = held.factory();
    let build = tokio::spawn(async move { owner.build_test(factory, Some(&*journal)).await });
    held.entered().await;
    let ticket = Arc::downgrade(&observer.resource());
    cancel_build(build).await;
    drop(observer);
    assert!(owner_alive());
    drop(context);
    assert!(
        !owner_alive(),
        "retained ticket formed a cycle back to its owner"
    );
    let retained = ticket
        .upgrade()
        .expect("final owner drop detached or discarded its unsettled ticket");
    assert!(held.product.lock().unwrap().is_none());
    let mut close = Box::pin(retained.close());
    assert!(poll_once(close.as_mut()).await.is_pending());
    held.gate.release();
    bounded(close).await.unwrap();
    held.assert_disposed();
    assert!(!owner_alive());
}

fn reusable_config() -> RustFsConfig {
    RustFsConfig {
        endpoint: "http://127.0.0.1:1".into(),
        bucket: "context-client-reuse".into(),
        access_key_id: "unit-reuse-key".into(),
        secret_access_key: "unit-reuse-secret".into(),
        region: "us-east-1".into(),
    }
}

type BundleFactory = Box<dyn FnOnce() -> Result<Arc<SignedClientBundle>> + Send>;

fn memory_bundle() -> Result<Arc<SignedClientBundle>> {
    SignedClientBundle::build_with(
        mount_rs_core::diagnostics::object_store::Observer::disabled(),
        |_, _, _| Ok(Arc::new(InMemory::new())),
    )
}

struct HeldBundleFactory {
    gate: Arc<Gate>,
    calls: Arc<AtomicUsize>,
    products: Arc<Mutex<Vec<Weak<InMemory>>>>,
}

impl HeldBundleFactory {
    fn new() -> Self {
        Self {
            gate: Arc::new(Gate::default()),
            calls: Arc::new(AtomicUsize::new(0)),
            products: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn factory(&self) -> BundleFactory {
        let gate = self.gate.clone();
        let calls = self.calls.clone();
        let products = self.products.clone();
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            gate.hold();
            SignedClientBundle::build_with(
                mount_rs_core::diagnostics::object_store::Observer::disabled(),
                |_, _, _| {
                    let client = Arc::new(InMemory::new());
                    products.lock().unwrap().push(Arc::downgrade(&client));
                    Ok(client)
                },
            )
        })
    }

    async fn entered(&self) {
        bounded(self.gate.entered.notified()).await;
        assert_eq!(self.calls.load(Ordering::SeqCst), 1);
    }

    fn assert_disposed(&self) {
        let clients = self.products.lock().unwrap();
        assert_eq!(clients.len(), 4);
        assert!(clients.iter().all(|client| client.upgrade().is_none()));
    }
}

impl Drop for HeldBundleFactory {
    fn drop(&mut self) {
        self.gate.release();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn shared_constructor_survives_first_caller_and_observer_cleanup_cancellation() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldBundleFactory::new();
    let journal = Arc::new(RecordingObserver::default());
    let owner = context.clone();
    let observer = journal.clone();
    let factory = held.factory();
    let first = tokio::spawn(async move {
        owner
            .build_shared_test(reusable_config(), factory, Some(&*observer))
            .await
    });
    held.entered().await;
    let follower_calls = Arc::new(AtomicUsize::new(0));
    let calls = follower_calls.clone();
    let mut follower = Box::pin(context.build_shared_test(
        reusable_config(),
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            memory_bundle()
        }),
        None,
    ));
    assert!(poll_once(follower.as_mut()).await.is_pending());
    assert_eq!(context.test_shared_snapshot(), (1, 1, 2));
    cancel_build(first).await;
    assert_eq!(context.test_shared_snapshot(), (1, 1, 1));
    let resource = journal.resource();
    let mut cleanup = Box::pin(resource.close());
    assert!(poll_once(cleanup.as_mut()).await.is_pending());
    drop(cleanup);
    held.gate.release();
    let store = bounded(follower).await.unwrap();
    bounded(resource.close()).await.unwrap();
    assert_eq!(follower_calls.load(Ordering::SeqCst), 0);
    assert_eq!(held.calls.load(Ordering::SeqCst), 1);
    assert_eq!(context.test_shared_snapshot(), (1, 0, 0));
    drop(store);
    assert!(
        held.products
            .lock()
            .unwrap()
            .iter()
            .all(|client| client.upgrade().is_some())
    );
    bounded(context.close()).await.unwrap();
    held.assert_disposed();
}

#[tokio::test]
async fn shared_constructor_with_every_caller_canceled_is_joined_and_reused_once() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldBundleFactory::new();
    let observer = Arc::new(RecordingObserver::default());
    let owner = context.clone();
    let journal = observer.clone();
    let factory = held.factory();
    let first = tokio::spawn(async move {
        owner
            .build_shared_test(reusable_config(), factory, Some(&*journal))
            .await
    });
    held.entered().await;
    let mut follower =
        Box::pin(context.build_shared_test(reusable_config(), Box::new(memory_bundle), None));
    assert!(poll_once(follower.as_mut()).await.is_pending());
    drop(follower);
    cancel_build(first).await;
    assert_eq!(context.test_shared_snapshot(), (1, 1, 0));
    held.gate.release();
    bounded(observer.resource().close()).await.unwrap();
    let store = bounded(context.build_shared_test(
        reusable_config(),
        Box::new(|| panic!("joined shared result was rebuilt after all callers canceled")),
        None,
    ))
    .await
    .unwrap();
    assert_eq!(held.calls.load(Ordering::SeqCst), 1);
    drop(store);
    bounded(context.close()).await.unwrap();
    held.assert_disposed();
}

#[tokio::test(flavor = "current_thread")]
async fn shared_lease_close_survives_latest_waiter_cancellation() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldBundleFactory::new();
    let observer = Arc::new(RecordingObserver::default());
    let owner = context.clone();
    let journal = observer.clone();
    let factory = held.factory();
    let opening = tokio::spawn(async move {
        owner
            .build_shared_test(reusable_config(), factory, Some(&*journal))
            .await
    });
    held.entered().await;
    cancel_build(opening).await;
    let resource = observer.resource();
    let earlier_resource = resource.clone();
    let (parked_tx, parked_rx) = oneshot::channel();
    let earlier = tokio::spawn(async move {
        let mut close = Box::pin(earlier_resource.close());
        let mut parked_tx = Some(parked_tx);
        poll_fn(|cx| {
            let result = close.as_mut().poll(cx);
            if result.is_pending()
                && let Some(signal) = parked_tx.take()
            {
                let _ = signal.send(());
            }
            result
        })
        .await
    });
    bounded(parked_rx).await.unwrap();
    let mut latest = Box::pin(resource.close());
    assert!(poll_once(latest.as_mut()).await.is_pending());
    drop(latest);
    held.gate.release();
    bounded(earlier).await.unwrap().unwrap();
    bounded(resource.close()).await.unwrap();
    assert_eq!(context.test_shared_snapshot(), (1, 0, 0));
    bounded(context.close()).await.unwrap();
    held.assert_disposed();
}

#[tokio::test(flavor = "current_thread")]
async fn shared_context_close_cancellation_retains_the_join_and_rejects_ready_hits() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldBundleFactory::new();
    let owner = context.clone();
    let factory = held.factory();
    let first = tokio::spawn(async move {
        owner
            .build_shared_test(reusable_config(), factory, None)
            .await
    });
    held.entered().await;
    let earlier = spawn_pending_close(&context).await;
    let mut latest = Box::pin(context.close());
    assert!(poll_once(latest.as_mut()).await.is_pending());
    drop(latest);
    failed(
        bounded(context.build_shared_test(reusable_config(), Box::new(memory_bundle), None)).await,
        ErrorCode::Estale,
    );
    failed(bounded(first).await.unwrap(), ErrorCode::Estale);
    assert_eq!(context.test_shared_snapshot(), (1, 1, 0));
    held.gate.release();
    bounded(earlier).await.unwrap().unwrap();
    bounded(context.close()).await.unwrap();
    held.assert_disposed();
    assert_eq!(context.test_shared_snapshot(), (0, 0, 0));
    // A ready cache hit must use the same post-seal admission check as a miss.
    let ready = RustFsConstructionContext::new(1).unwrap();
    drop(
        bounded(ready.build_shared_test(reusable_config(), Box::new(memory_bundle), None))
            .await
            .unwrap(),
    );
    ready.seal_admission().unwrap();
    failed(
        bounded(ready.build_shared_test(reusable_config(), Box::new(memory_bundle), None)).await,
        ErrorCode::Estale,
    );
    bounded(ready.close()).await.unwrap();
}

#[tokio::test]
async fn shared_fourth_role_failure_disposes_partial_clients_without_automatic_retry() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let built = Arc::new(Mutex::new(Vec::<Weak<InMemory>>::new()));
    let counter = calls.clone();
    let weak = built.clone();
    let observer = RecordingObserver::default();
    let failure = bounded(context.build_shared_test(
        reusable_config(),
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut role = 0;
            SignedClientBundle::build_with(
                mount_rs_core::diagnostics::object_store::Observer::disabled(),
                |_, _, _| {
                    role += 1;
                    if role == 4 {
                        return Err(FsError::new(ErrorCode::Enospc)
                            .with_message("controlled fourth role failure"));
                    }
                    let client = Arc::new(InMemory::new());
                    weak.lock().unwrap().push(Arc::downgrade(&client));
                    Ok(client)
                },
            )
        }),
        Some(&observer),
    ))
    .await
    .err()
    .unwrap();
    assert_eq!(failure.code, ErrorCode::Enospc);
    assert_eq!(failure.to_string(), "controlled fourth role failure");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(built.lock().unwrap().len(), 3);
    assert!(
        built
            .lock()
            .unwrap()
            .iter()
            .all(|client| client.upgrade().is_none())
    );
    bounded(observer.resource().close()).await.unwrap();
    assert_eq!(context.test_shared_snapshot(), (0, 0, 0));
    drop(
        bounded(context.build_shared_test(reusable_config(), Box::new(memory_bundle), None))
            .await
            .unwrap(),
    );
    bounded(context.close()).await.unwrap();
}

async fn shared_quarantine_drains_healthy_peer(poison: bool) {
    let context = RustFsConstructionContext::new(2).unwrap();
    let bad = HeldBundleFactory::new();
    let bad_factory: BundleFactory = if poison {
        bad.factory()
    } else {
        let gate = bad.gate.clone();
        let calls = bad.calls.clone();
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            gate.hold();
            panic!("controlled shared native constructor panic")
        })
    };
    let owner = context.clone();
    let bad_build = tokio::spawn(async move {
        owner
            .build_shared_test(reusable_config(), bad_factory, None)
            .await
    });
    bad.entered().await;
    let good = HeldBundleFactory::new();
    let owner = context.clone();
    let factory = good.factory();
    let mut config = reusable_config();
    config.bucket = "healthy-peer".into();
    let good_build =
        tokio::spawn(async move { owner.build_shared_test(config, factory, None).await });
    good.entered().await;
    if poison {
        context.test_poison_shared_ticket(0);
        assert_eq!(context.test_shared_snapshot().1, 2);
        failed(bounded(bad_build).await.unwrap(), ErrorCode::Eio);
        assert!(bad.products.lock().unwrap().is_empty());
        bad.gate.release();
    } else {
        bad.gate.release();
        failed(bounded(bad_build).await.unwrap(), ErrorCode::Eio);
    }
    let closing = spawn_pending_close(&context).await;
    assert!(!closing.is_finished());
    good.gate.release();
    assert!(bounded(good_build).await.unwrap().is_err());
    failed(bounded(closing).await.unwrap(), ErrorCode::Eio);
    good.assert_disposed();
    if poison {
        bad.assert_disposed();
    }
    failed(bounded(context.close()).await, ErrorCode::Eio);
    failed(
        bounded(context.build_shared_test(reusable_config(), Box::new(memory_bundle), None)).await,
        ErrorCode::Eio,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_constructor_poison_stays_quarantined_and_joins_healthy_peer() {
    shared_quarantine_drains_healthy_peer(true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn shared_constructor_panic_stays_quarantined_and_joins_healthy_peer() {
    shared_quarantine_drains_healthy_peer(false).await;
}

#[tokio::test]
async fn shared_configuration_cache_bound_falls_back_without_limiting_live_drives() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let first =
        bounded(context.build_shared_test(reusable_config(), Box::new(memory_bundle), None))
            .await
            .unwrap();
    for index in 1..64 {
        let mut config = reusable_config();
        config.bucket = format!("cached-config-{index}");
        drop(
            bounded(context.build_shared_test(config, Box::new(memory_bundle), None))
                .await
                .unwrap(),
        );
    }
    assert_eq!(context.test_shared_snapshot(), (64, 0, 0));
    let mut overflow_config = reusable_config();
    overflow_config.bucket = "uncached-overflow".into();
    let overflow_one =
        bounded(context.build_shared_test(overflow_config.clone(), Box::new(memory_bundle), None))
            .await
            .unwrap();
    let overflow_two =
        bounded(context.build_shared_test(overflow_config, Box::new(memory_bundle), None))
            .await
            .unwrap();
    assert!(!Arc::ptr_eq(
        &overflow_one
            .qualification_clients
            .as_ref()
            .unwrap()
            .first_data,
        &overflow_two
            .qualification_clients
            .as_ref()
            .unwrap()
            .first_data,
    ));
    let cached = bounded(context.build_shared_test(
        reusable_config(),
        Box::new(|| panic!("full cache lost existing key")),
        None,
    ))
    .await
    .unwrap();
    assert!(Arc::ptr_eq(
        &first.qualification_clients.as_ref().unwrap().first_data,
        &cached.qualification_clients.as_ref().unwrap().first_data,
    ));
    assert_eq!(context.test_shared_snapshot(), (64, 0, 0));
    drop(first);
    drop(cached);
    drop(overflow_one);
    drop(overflow_two);
    bounded(context.close()).await.unwrap();
}

#[tokio::test]
async fn shared_key_compares_every_exact_configuration_field() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let first =
        bounded(context.build_shared_test(reusable_config(), Box::new(memory_bundle), None))
            .await
            .unwrap();
    for field in 0..5 {
        let mut config = reusable_config();
        match field {
            0 => config.endpoint = "http://127.0.0.1:2".into(),
            1 => config.bucket = "changed-bucket".into(),
            2 => config.region = "us-west-2".into(),
            3 => config.access_key_id = "changed-access".into(),
            _ => config.secret_access_key = "changed-secret".into(),
        }
        let next = bounded(context.build_shared_test(config, Box::new(memory_bundle), None))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(
            &first.qualification_clients.as_ref().unwrap().first_data,
            &next.qualification_clients.as_ref().unwrap().first_data
        ));
        drop(next);
    }
    assert_eq!(context.test_shared_snapshot(), (6, 0, 0));
    drop(first);
    bounded(context.close()).await.unwrap();
}

#[tokio::test]
async fn shared_clients_keep_prefix_caches_and_durability_independent() {
    use mount_rs_core::storage::BlockStore;
    let context = RustFsConstructionContext::new(1).unwrap();
    let first = bounded(context.build_shared_test_with_prefix(
        reusable_config(),
        "drive-one/blocks".into(),
        false,
        Box::new(memory_bundle),
        None,
    ))
    .await
    .unwrap();
    let second = bounded(context.build_shared_test_with_prefix(
        reusable_config(),
        "drive-two/blocks".into(),
        true,
        Box::new(|| panic!("prefix and durability must not split the signed client key")),
        None,
    ))
    .await
    .unwrap();
    assert!(!first.durable());
    assert!(second.durable());
    let id = first.put(b"one scoped immutable block").await.unwrap();
    assert_eq!(first.get(&id).await.unwrap(), b"one scoped immutable block");
    assert!(
        second.get(&id).await.is_err(),
        "one Drive must not read another prefix's cache entry"
    );
    assert_eq!(first.stats().cache_hits, 1);
    assert_eq!(second.stats().cache_hits, 0);
    assert_eq!(second.put(b"one scoped immutable block").await.unwrap(), id);
    assert_eq!(
        second.get(&id).await.unwrap(),
        b"one scoped immutable block"
    );
    assert_eq!(first.stats().puts, 1);
    assert_eq!(second.stats().puts, 1);
    drop(first);
    drop(second);
    bounded(context.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_seal_during_initial_observer_registration_prevents_native_spawn() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let gate = Arc::new(Gate::default());
    let observer = Arc::new(HeldObserver {
        journal: RecordingObserver::default(),
        gate: gate.clone(),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let owner = context.clone();
    let journal = observer.clone();
    let opening = tokio::spawn(async move {
        owner
            .build_shared_test(
                reusable_config(),
                Box::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    memory_bundle()
                }),
                Some(&*journal),
            )
            .await
    });
    bounded(gate.entered.notified()).await;
    context.seal_admission().unwrap();
    let mut closing = Box::pin(context.close());
    assert!(poll_once(closing.as_mut()).await.is_pending());
    assert_eq!(context.test_shared_snapshot().1, 1);
    gate.release();
    failed(bounded(opening).await.unwrap(), ErrorCode::Estale);
    bounded(closing).await.unwrap();
    bounded(observer.journal.resource().close()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_ready_hit_rechecks_seal_after_observer_registration() {
    let context = RustFsConstructionContext::new(1).unwrap();
    drop(
        bounded(context.build_shared_test(reusable_config(), Box::new(memory_bundle), None))
            .await
            .unwrap(),
    );
    let gate = Arc::new(Gate::default());
    let observer = Arc::new(HeldObserver {
        journal: RecordingObserver::default(),
        gate: gate.clone(),
    });
    let owner = context.clone();
    let journal = observer.clone();
    let opening = tokio::spawn(async move {
        owner
            .build_shared_test(
                reusable_config(),
                Box::new(|| panic!("ready hit rebuilt clients")),
                Some(&*journal),
            )
            .await
    });
    bounded(gate.entered.notified()).await;
    context.seal_admission().unwrap();
    bounded(context.close()).await.unwrap();
    gate.release();
    failed(bounded(opening).await.unwrap(), ErrorCode::Estale);
    bounded(observer.journal.resource().close()).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn shared_lease_poison_wakes_close_waiting_on_release_and_retains_uncertainty() {
    let context = RustFsConstructionContext::new(1).unwrap();
    drop(
        bounded(context.build_shared_test(reusable_config(), Box::new(memory_bundle), None))
            .await
            .unwrap(),
    );
    // Inject the valid releasing state to park at the exact lease notification
    // boundary. Poison recovery must wake this existing close, not just return
    // an error to the recovery caller.
    let (lease, poison) = context.test_shared_lease_releasing(0);
    let retained = lease.clone();
    let (parked_tx, parked_rx) = oneshot::channel();
    let closing = tokio::spawn(async move {
        let mut close = Box::pin(retained.close());
        let mut parked_tx = Some(parked_tx);
        poll_fn(|cx| {
            let result = close.as_mut().poll(cx);
            if result.is_pending()
                && let Some(signal) = parked_tx.take()
            {
                let _ = signal.send(());
            }
            result
        })
        .await
    });
    bounded(parked_rx).await.unwrap();
    poison();
    failed(bounded(closing).await.unwrap(), ErrorCode::Eio);
    failed(bounded(lease.close()).await, ErrorCode::Eio);
    failed(bounded(context.close()).await, ErrorCode::Eio);
}

#[tokio::test]
async fn final_shared_context_drop_preserves_registry_poison_quarantine() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldBundleFactory::new();
    held.gate.release();
    let observer = RecordingObserver::default();
    drop(
        bounded(context.build_shared_test(reusable_config(), held.factory(), Some(&observer)))
            .await
            .unwrap(),
    );
    let alive = context.test_owner_liveness();
    let resource = observer.resource();
    context.test_poison_registry();
    // Do not call a context accessor after poison: final Drop must discover it.
    drop(context);
    assert!(!alive());
    failed(bounded(resource.close()).await, ErrorCode::Eio);
    held.assert_disposed();
}

#[tokio::test(flavor = "current_thread")]
async fn final_shared_context_drop_retains_unjoined_handle_without_owner_cycle() {
    let context = RustFsConstructionContext::new(1).unwrap();
    let held = HeldBundleFactory::new();
    let observer = Arc::new(RecordingObserver::default());
    let owner = context.clone();
    let journal = observer.clone();
    let factory = held.factory();
    let opening = tokio::spawn(async move {
        owner
            .build_shared_test(reusable_config(), factory, Some(&*journal))
            .await
    });
    held.entered().await;
    cancel_build(opening).await;
    let alive = context.test_owner_liveness();
    let resource = observer.resource();
    drop(observer);
    drop(context);
    assert!(
        !alive(),
        "retained shared ticket must not cycle to its context"
    );
    let mut closing = Box::pin(resource.close());
    assert!(poll_once(closing.as_mut()).await.is_pending());
    assert!(held.products.lock().unwrap().is_empty());
    held.gate.release();
    bounded(closing).await.unwrap();
    held.assert_disposed();
}

#[tokio::test]
async fn public_context_reuses_four_clients_across_independent_drive_facades() {
    use mount_rs_core::storage::BlockStore;

    let context = RustFsConstructionContext::new(2).unwrap();
    let config = reusable_config();
    let first = context
        .block_store(
            config.clone(),
            "partition-one/drive-one/blocks".into(),
            false,
            None,
        )
        .await
        .unwrap();
    let second = context
        .block_store(config, "partition-two/drive-two/blocks".into(), true, None)
        .await
        .unwrap();
    let one = first.qualification_clients.as_ref().unwrap();
    let two = second.qualification_clients.as_ref().unwrap();
    let shared = [
        Arc::ptr_eq(&one.first_data, &two.first_data),
        Arc::ptr_eq(&one.second_data, &two.second_data),
        Arc::ptr_eq(&one.first_probe, &two.first_probe),
        Arc::ptr_eq(&one.second_probe, &two.second_probe),
    ];
    let roles_independent = !Arc::ptr_eq(&one.first_data, &one.second_data)
        && !Arc::ptr_eq(&one.first_probe, &one.second_probe)
        && !Arc::ptr_eq(&one.first_data, &one.first_probe);
    let scopes = (
        first.prefix().to_owned(),
        second.prefix().to_owned(),
        first.durable(),
        second.durable(),
    );
    drop(first);
    drop(second);
    context.close().await.unwrap();

    assert!(
        roles_independent,
        "qualification roles must remain independently constructed"
    );
    assert_eq!(
        scopes,
        (
            "partition-one/drive-one/blocks".to_owned(),
            "partition-two/drive-two/blocks".to_owned(),
            false,
            true,
        )
    );
    assert_eq!(
        shared, [true; 4],
        "one context must reuse its four configured clients across Drive facades"
    );
}

#[tokio::test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and an isolated exact test process"]
async fn public_context_builds_four_native_clients_for_two_scoped_opens() {
    use mount_rs_core::diagnostics::object_store::{ClientRole, Observer};

    // This invokes the existing public API and actual ReqwestConnector. No test
    // constructor or mocked counter can turn eight native builds into four.
    let observer = Observer::enabled();
    let before = observer.snapshot().expect("set MOUNT_RS_PROFILE_IO=1");
    let context = RustFsConstructionContext::new(2).unwrap();
    let first = context
        .block_store(reusable_config(), "drive-one/blocks".into(), false, None)
        .await
        .unwrap();
    let second = context
        .block_store(reusable_config(), "drive-two/blocks".into(), true, None)
        .await
        .unwrap();
    let opened = observer.snapshot().unwrap();
    drop(first);
    drop(second);
    let facades_released = observer.snapshot().unwrap();
    context.close().await.unwrap();
    let closed = observer.snapshot().unwrap();

    // Facade and cache accounting retains its existing per-prefix meaning.
    assert_eq!(opened.bundles.committed - before.bundles.committed, 2);
    assert_eq!(opened.cache.created - before.cache.created, 2);
    assert_eq!(facades_released.bundles.live, before.bundles.live);
    assert_eq!(facades_released.cache.live, before.cache.live);
    for role in [
        ClientRole::PrimaryDataMixed,
        ClientRole::QualificationData,
        ClientRole::PrimaryProbeMixed,
        ClientRole::QualificationProbe,
    ] {
        let prior = before.clients[role.index()];
        let actual = opened.clients[role.index()];
        assert_eq!(actual.build.started - prior.build.started, 1, "{role:?}");
        assert_eq!(
            actual.build.succeeded - prior.build.succeeded,
            1,
            "{role:?}"
        );
        assert_eq!(actual.constructed - prior.constructed, 1, "{role:?}");
        assert_eq!(actual.build.failed, prior.build.failed, "{role:?}");
        assert_eq!(actual.build.inflight, prior.build.inflight, "{role:?}");
        assert_eq!(closed.clients[role.index()].live, prior.live, "{role:?}");
        assert!(
            actual
                .http
                .iter()
                .zip(prior.http)
                .all(|(after, before)| { after.attempts_started == before.attempts_started }),
            "construction must issue no backing requests"
        );
    }
    assert!(!closed.saturated);
}
