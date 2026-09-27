//! Runtime timings distinguish canceled callers from their owned lifecycle tasks.
//! Synthetic eligibility exercises policy; these fakes do not qualify storage.

#![cfg(feature = "io-profiling")]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::storage::ConcurrentBackingId;
use mount_rs_core::{ErrorCode, FileHandle, FsDriver, FsError, Result as FsResult, Stats};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_service::runtime_diagnostics::{RuntimeDiagnostics, RuntimeDiagnosticsEntry};
use mount_rs_service::runtime_pool::{
    DriveRegistration, ManagedDrive, RuntimeFactory, RuntimeLease, RuntimePool,
};
use tokio::sync::Notify;

const DEADLINE: Duration = Duration::from_secs(5);
const LABELS: [&str; 8] = [
    "runtime.acquire",
    "runtime.activation_wait",
    "runtime.open",
    "runtime.eviction_shutdown",
    "runtime.terminal_drain",
    "runtime.handle_close",
    "runtime.state_mutex_wait",
    "runtime.state_mutex_hold",
];

fn row(observer: &RuntimeDiagnostics, name: &str) -> RuntimeDiagnosticsEntry {
    observer
        .snapshot()
        .expect("explicit observer unavailable")
        .entries
        .into_iter()
        .find(|entry| entry.name == name)
        .expect("fixed runtime stage missing")
}
fn catalog(observer: &RuntimeDiagnostics) {
    let snapshot = observer.snapshot().unwrap();
    assert_eq!(snapshot.entries.map(|entry| entry.name), LABELS);
    assert!(snapshot.inclusive_spans_overlap);
}
async fn observed(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(DEADLINE, async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("controlled metric completion did not arrive");
}
async fn notified(event: &Notify) {
    tokio::time::timeout(DEADLINE, event.notified())
        .await
        .expect("controlled backend event did not arrive");
}
async fn acquire(registration: &DriveRegistration) -> FsResult<RuntimeLease> {
    tokio::time::timeout(DEADLINE, registration.acquire())
        .await
        .expect("runtime acquisition did not finish")
}
async fn acquire_error(registration: &DriveRegistration, code: ErrorCode) -> FsError {
    let error = match acquire(registration).await {
        Ok(_) => panic!("unexpected runtime admission"),
        Err(error) => error,
    };
    assert_eq!(error.code, code);
    error
}
async fn joined<T>(task: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(DEADLINE, task)
        .await
        .expect("controlled task did not finish")
        .expect("controlled task failed")
}
async fn cancelled<T>(task: tokio::task::JoinHandle<T>) {
    task.abort();
    let error = match tokio::time::timeout(DEADLINE, task)
        .await
        .expect("canceled waiter did not retire")
    {
        Ok(_) => panic!("canceled waiter completed normally"),
        Err(error) => error,
    };
    assert!(error.is_cancelled());
}
async fn shutdown(pool: &RuntimePool) -> FsResult<()> {
    tokio::time::timeout(DEADLINE, pool.shutdown())
        .await
        .expect("owned runtime drain did not finish")
}
fn spawned(
    registration: &Arc<DriveRegistration>,
) -> tokio::task::JoinHandle<FsResult<RuntimeLease>> {
    let registration = registration.clone();
    tokio::spawn(async move { registration.acquire().await })
}

#[derive(Default)]
struct Probe {
    opens: AtomicUsize,
    shutdowns: AtomicUsize,
    open_started: Notify,
    open_release: Notify,
    shutdown_started: Notify,
    shutdown_release: Notify,
    sample_armed: AtomicBool,
    sampled: AtomicBool,
    inside: [AtomicU64; 4],
}
#[derive(Clone, Copy)]
enum OpenResult {
    Success,
    Error,
    Panic,
}
struct Factory {
    probe: Arc<Probe>,
    open_gated: bool,
    shutdown_gated: bool,
    shutdown_error: bool,
    result: OpenResult,
    observer: Option<RuntimeDiagnostics>,
}
impl Factory {
    fn new() -> Self {
        Self {
            probe: Arc::default(),
            open_gated: false,
            shutdown_gated: false,
            shutdown_error: false,
            result: OpenResult::Success,
            observer: None,
        }
    }
}
struct Runtime {
    factory: Arc<Factory>,
    driver: Arc<dyn FsDriver>,
}
struct SharedFactory(Arc<Factory>);
#[async_trait]
impl RuntimeFactory for SharedFactory {
    async fn open(&self) -> FsResult<Arc<dyn ManagedDrive>> {
        let factory = &self.0;
        factory.probe.opens.fetch_add(1, Ordering::SeqCst);
        factory.probe.open_started.notify_one();
        if factory.open_gated {
            factory.probe.open_release.notified().await;
        }
        // Injected failures occur before acquiring any provider or cleanup task.
        match factory.result {
            OpenResult::Error => {
                return Err(FsError::new(ErrorCode::Eio).with_syscall("original open failure"));
            }
            OpenResult::Panic => panic!("injected construction panic without partial resources"),
            OpenResult::Success => {}
        }
        Ok(Arc::new(Runtime {
            factory: factory.clone(),
            driver: Arc::new(MemoryFs::new(MemoryOptions::default())),
        }))
    }
}
#[async_trait]
impl ManagedDrive for Runtime {
    fn driver(&self) -> Arc<dyn FsDriver> {
        self.driver.clone()
    }
    fn failed(&self) -> bool {
        if self
            .factory
            .probe
            .sample_armed
            .swap(false, Ordering::SeqCst)
        {
            // Fixed atomic snapshot only: no pool reentry, mutex or heap allocation.
            let snapshot = self.factory.observer.as_ref().unwrap().snapshot().unwrap();
            let wait = &snapshot.entries[6];
            let hold = &snapshot.entries[7];
            for (destination, value) in self.factory.probe.inside.iter().zip([
                wait.calls,
                wait.success,
                hold.calls,
                hold.success,
            ]) {
                destination.store(value, Ordering::SeqCst);
            }
            self.factory.probe.sampled.store(true, Ordering::SeqCst);
        }
        false
    }
    fn eviction_allowed(&self) -> bool {
        true
    }
    fn concurrent_backing_id(&self) -> Option<ConcurrentBackingId> {
        Some(ConcurrentBackingId::from_bytes([1; 16]).unwrap())
    }
    async fn shutdown(&self) -> FsResult<()> {
        self.factory.probe.shutdowns.fetch_add(1, Ordering::SeqCst);
        self.factory.probe.shutdown_started.notify_one();
        if self.factory.shutdown_gated {
            self.factory.probe.shutdown_release.notified().await;
        }
        if self.factory.shutdown_error {
            Err(FsError::new(ErrorCode::Eio).with_syscall("original shutdown failure"))
        } else {
            Ok(())
        }
    }
}
fn register(pool: &RuntimePool, factory: &Arc<Factory>) -> Arc<DriveRegistration> {
    Arc::new(
        pool.register(Arc::new(SharedFactory(factory.clone())))
            .unwrap(),
    )
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
struct Handle {
    probe: Arc<CloseProbe>,
    fail: bool,
}
#[async_trait]
impl FileHandle for Handle {
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
            Err(FsError::new(ErrorCode::Eio))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn canceled_callers_leave_owned_open_and_terminal_drain_observed_until_completion() {
    let observer = RuntimeDiagnostics::new(false);
    catalog(&observer);
    let pool = Arc::new(RuntimePool::with_diagnostics(1, observer.clone()).unwrap());
    let mut configuration = Factory::new();
    configuration.open_gated = true;
    configuration.shutdown_gated = true;
    let factory = Arc::new(configuration);
    let registration = register(&pool, &factory);
    let first = spawned(&registration);
    notified(&factory.probe.open_started).await;
    assert_eq!(row(&observer, "runtime.open").in_flight, 1);
    cancelled(first).await;
    let acquire_row = row(&observer, "runtime.acquire");
    let wait = row(&observer, "runtime.activation_wait");
    assert_eq!((acquire_row.cancelled, acquire_row.in_flight), (1, 0));
    assert_eq!((wait.cancelled, wait.in_flight), (1, 0));
    assert_eq!(
        (
            row(&observer, "runtime.open").in_flight,
            row(&observer, "runtime.open").cancelled
        ),
        (1, 0)
    );
    let second = spawned(&registration);
    observed(|| row(&observer, "runtime.activation_wait").in_flight == 1).await;
    factory.probe.open_release.notify_one();
    let lease = joined(second).await.unwrap();
    assert!(lease.waited());
    drop(lease);
    observed(|| {
        row(&observer, "runtime.open").success == 1 && row(&observer, "runtime.open").in_flight == 0
    })
    .await;
    assert_eq!(factory.probe.opens.load(Ordering::SeqCst), 1);
    assert_eq!(
        (
            row(&observer, "runtime.acquire").success,
            row(&observer, "runtime.activation_wait").success
        ),
        (1, 1)
    );
    let waiter = tokio::spawn({
        let pool = pool.clone();
        async move { pool.shutdown().await }
    });
    notified(&factory.probe.shutdown_started).await;
    cancelled(waiter).await;
    let drain = row(&observer, "runtime.terminal_drain");
    assert_eq!((drain.calls, drain.in_flight, drain.cancelled), (1, 1, 0));
    factory.probe.shutdown_release.notify_one();
    shutdown(&pool).await.unwrap();
    observed(|| row(&observer, "runtime.terminal_drain").in_flight == 0).await;
    let drain = row(&observer, "runtime.terminal_drain");
    assert_eq!(
        (drain.calls, drain.success, drain.error, drain.cancelled),
        (1, 1, 0, 0)
    );
    assert_eq!(factory.probe.shutdowns.load(Ordering::SeqCst), 1);
    assert!(pool.diagnostics().is_some());
}

#[tokio::test]
async fn drop_owned_close_and_canceled_eviction_have_independent_active_and_success_rows() {
    let observer = RuntimeDiagnostics::new(false);
    let pool = RuntimePool::with_diagnostics(1, observer.clone()).unwrap();
    let mut configuration = Factory::new();
    configuration.shutdown_gated = true;
    let first = Arc::new(configuration);
    let other = Arc::new(Factory::new());
    let a = register(&pool, &first);
    let b = register(&pool, &other);
    let lease = acquire(&a).await.unwrap();
    let probe = Arc::new(CloseProbe::default());
    let handle = lease.wrap_handle(Arc::new(Handle {
        probe: probe.clone(),
        fail: false,
    }));
    drop(lease);
    drop(handle);
    notified(&probe.started).await;
    assert_eq!(
        (
            row(&observer, "runtime.handle_close").calls,
            row(&observer, "runtime.handle_close").in_flight
        ),
        (1, 1)
    );
    acquire_error(&b, ErrorCode::Ebusy).await;
    assert!(pool.snapshot().pinned > 0);
    assert_eq!(other.probe.opens.load(Ordering::SeqCst), 0);
    probe.release.notify_one();
    observed(|| pool.snapshot().pinned == 0 && row(&observer, "runtime.handle_close").success == 1)
        .await;
    let waiter = spawned(&b);
    notified(&first.probe.shutdown_started).await;
    assert_eq!(row(&observer, "runtime.eviction_shutdown").in_flight, 1);
    cancelled(waiter).await;
    assert_eq!(row(&observer, "runtime.activation_wait").cancelled, 1);
    let eviction = row(&observer, "runtime.eviction_shutdown");
    assert_eq!(
        (eviction.calls, eviction.in_flight, eviction.cancelled),
        (1, 1, 0)
    );
    let replacement = spawned(&b);
    observed(|| row(&observer, "runtime.activation_wait").in_flight == 1).await;
    first.probe.shutdown_release.notify_one();
    let replacement = joined(replacement).await.unwrap();
    assert!(replacement.waited());
    observed(|| {
        row(&observer, "runtime.eviction_shutdown").success == 1
            && row(&observer, "runtime.open").success == 2
    })
    .await;
    let eviction = row(&observer, "runtime.eviction_shutdown");
    assert_eq!(
        (
            eviction.calls,
            eviction.success,
            eviction.error,
            eviction.cancelled,
            eviction.in_flight
        ),
        (1, 1, 0, 0, 0)
    );
    let open = row(&observer, "runtime.open");
    assert_eq!(
        (
            open.calls,
            open.success,
            open.error,
            open.cancelled,
            open.in_flight
        ),
        (2, 2, 0, 0, 0)
    );
    let acquire_row = row(&observer, "runtime.acquire");
    assert_eq!(
        (
            acquire_row.success,
            acquire_row.error,
            acquire_row.cancelled
        ),
        (2, 1, 1)
    );
    assert_eq!(
        (
            first.probe.shutdowns.load(Ordering::SeqCst),
            other.probe.opens.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
    let close = row(&observer, "runtime.handle_close");
    assert_eq!(
        (
            close.calls,
            close.success,
            close.error,
            close.cancelled,
            close.in_flight
        ),
        (1, 1, 0, 0, 0)
    );
    assert_eq!(
        (
            probe.calls.load(Ordering::SeqCst),
            probe.finished.load(Ordering::SeqCst),
            probe.cancelled.load(Ordering::SeqCst)
        ),
        (1, 1, 0)
    );
    drop(replacement);
    shutdown(&pool).await.unwrap();
}

#[tokio::test]
async fn failed_and_panicking_owners_record_errors_without_replay_or_losing_actual_handles() {
    for failure in [OpenResult::Error, OpenResult::Panic] {
        let observer = RuntimeDiagnostics::new(false);
        let pool = RuntimePool::with_diagnostics(1, observer.clone()).unwrap();
        let mut configuration = Factory::new();
        configuration.result = failure;
        let factory = Arc::new(configuration);
        let other = Arc::new(Factory::new());
        let a = register(&pool, &factory);
        let b = register(&pool, &other);
        let error = acquire_error(&a, ErrorCode::Eio).await;
        if matches!(failure, OpenResult::Error) {
            assert_eq!(error.syscall.as_deref(), Some("original open failure"));
        }
        acquire_error(&a, ErrorCode::Eio).await;
        acquire_error(&b, ErrorCode::Ebusy).await;
        assert_eq!(
            (
                factory.probe.opens.load(Ordering::SeqCst),
                other.probe.opens.load(Ordering::SeqCst)
            ),
            (1, 0)
        );
        assert_eq!(
            (pool.snapshot().resident, pool.snapshot().quarantined),
            (1, 1)
        );
        let open = row(&observer, "runtime.open");
        assert_eq!(
            (open.calls, open.error, open.cancelled, open.in_flight),
            (1, 1, 0, 0)
        );
        assert_eq!(row(&observer, "runtime.acquire").error, 3);
        assert!(shutdown(&pool).await.is_err());
        assert!(shutdown(&pool).await.is_err());
        let drain = row(&observer, "runtime.terminal_drain");
        assert_eq!(
            (drain.calls, drain.error, drain.cancelled, drain.in_flight),
            (1, 1, 0, 0)
        );
        assert_eq!(factory.probe.shutdowns.load(Ordering::SeqCst), 0);
    }
    let observer = RuntimeDiagnostics::new(false);
    let pool = RuntimePool::with_diagnostics(1, observer.clone()).unwrap();
    let first = Arc::new(Factory::new());
    let other = Arc::new(Factory::new());
    let a = register(&pool, &first);
    let b = register(&pool, &other);
    let lease = acquire(&a).await.unwrap();
    let probe = Arc::new(CloseProbe::default());
    let actual: Arc<dyn FileHandle> = Arc::new(Handle {
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
        "failed close lost its actual backend owner"
    );
    acquire_error(&a, ErrorCode::Eio).await;
    acquire_error(&b, ErrorCode::Ebusy).await;
    let close = row(&observer, "runtime.handle_close");
    assert_eq!(
        (
            close.calls,
            close.error,
            close.success,
            close.cancelled,
            close.in_flight
        ),
        (1, 1, 0, 0, 0)
    );
    assert_eq!(
        (pool.snapshot().resident, pool.snapshot().quarantined),
        (1, 1)
    );
    assert!(shutdown(&pool).await.is_err());
    assert_eq!(row(&observer, "runtime.terminal_drain").error, 1);
    assert_eq!(
        (
            probe.calls.load(Ordering::SeqCst),
            probe.cancelled.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
    assert_eq!(
        (
            first.probe.shutdowns.load(Ordering::SeqCst),
            other.probe.opens.load(Ordering::SeqCst)
        ),
        (0, 0)
    );
}

#[tokio::test]
async fn default_pool_is_unobserved_and_hot_mutex_timings_publish_after_unlock() {
    let default = RuntimePool::new(1).unwrap();
    let factory = Arc::new(Factory::new());
    let registration = register(&default, &factory);
    assert!(default.diagnostics().is_none());
    drop(acquire(&registration).await.unwrap());
    assert!(default.diagnostics().is_none());
    shutdown(&default).await.unwrap();

    let observer = RuntimeDiagnostics::new(false);
    let pool = RuntimePool::with_diagnostics(1, observer.clone()).unwrap();
    let mut configuration = Factory::new();
    configuration.observer = Some(observer.clone());
    let factory = Arc::new(configuration);
    let registration = register(&pool, &factory);
    drop(acquire(&registration).await.unwrap());
    observed(|| {
        row(&observer, "runtime.open").success == 1 && row(&observer, "runtime.open").in_flight == 0
    })
    .await;
    let wait_before = row(&observer, "runtime.state_mutex_wait");
    let hold_before = row(&observer, "runtime.state_mutex_hold");
    let baseline = [
        wait_before.calls,
        wait_before.success,
        hold_before.calls,
        hold_before.success,
    ];
    factory.probe.sample_armed.store(true, Ordering::SeqCst);
    let lease = acquire(&registration).await.unwrap();
    assert!(!lease.waited());
    assert!(factory.probe.sampled.load(Ordering::SeqCst));
    let inside =
        std::array::from_fn::<_, 4, _>(|index| factory.probe.inside[index].load(Ordering::SeqCst));
    assert_eq!(
        inside, baseline,
        "current mutex timing published while the state guard was held"
    );
    let wait_after = row(&observer, "runtime.state_mutex_wait");
    let hold_after = row(&observer, "runtime.state_mutex_hold");
    assert_eq!(
        [
            wait_after.calls,
            wait_after.success,
            hold_after.calls,
            hold_after.success
        ],
        baseline.map(|value| value + 1)
    );
    assert_eq!((wait_after.in_flight, hold_after.in_flight), (0, 0));
    assert_eq!(factory.probe.opens.load(Ordering::SeqCst), 1);
    let snapshot = pool.diagnostics().unwrap();
    assert_eq!(snapshot.entries[6].calls, wait_after.calls);
    assert_eq!(snapshot.entries[7].calls, hold_after.calls);
    drop(lease);
    shutdown(&pool).await.unwrap();
}

struct TicketWake {
    observer: RuntimeDiagnostics,
    armed: AtomicBool,
    captured: AtomicBool,
    row: [AtomicU64; 4],
    ready: Notify,
}
impl TicketWake {
    fn capture(&self) {
        if self.armed.load(Ordering::SeqCst) && !self.captured.swap(true, Ordering::SeqCst) {
            // This executes synchronously in watch's admission-ticket wake.
            // Only the finite observer is read; the pool mutex is never entered.
            let snapshot = self.observer.snapshot().unwrap();
            let eviction = &snapshot.entries[3];
            for (destination, value) in self.row.iter().zip([
                eviction.calls,
                eviction.success,
                eviction.error,
                eviction.in_flight,
            ]) {
                destination.store(value, Ordering::SeqCst);
            }
            self.ready.notify_one();
        }
    }
}
impl Wake for TicketWake {
    fn wake(self: Arc<Self>) {
        self.capture();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.capture();
    }
}

#[tokio::test]
async fn eviction_completion_metrics_publish_before_admission_ticket_wakes() {
    for stopped in [false, true] {
        let observer = RuntimeDiagnostics::new(false);
        let pool = RuntimePool::with_diagnostics(1, observer.clone()).unwrap();
        let mut configuration = Factory::new();
        configuration.shutdown_gated = true;
        configuration.shutdown_error = !stopped;
        let victim = Arc::new(configuration);
        let replacement = Arc::new(Factory::new());
        let a = register(&pool, &victim);
        let b = register(&pool, &replacement);
        drop(acquire(&a).await.unwrap());
        let wake = Arc::new(TicketWake {
            observer: observer.clone(),
            armed: AtomicBool::new(false),
            captured: AtomicBool::new(false),
            row: std::array::from_fn(|_| AtomicU64::new(0)),
            ready: Notify::new(),
        });
        let waker = Waker::from(wake.clone());
        let mut admission = std::pin::pin!(b.acquire());
        assert!(matches!(
            admission.as_mut().poll(&mut Context::from_waker(&waker)),
            Poll::Pending
        ));
        notified(&victim.probe.shutdown_started).await;
        assert_eq!(row(&observer, "runtime.eviction_shutdown").in_flight, 1);
        let mut drain = std::pin::pin!(pool.shutdown());
        if stopped {
            // The first poll synchronously stops admission while the same
            // owned victim close remains blocked on its controlled gate.
            assert!(matches!(
                drain.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
        }
        wake.armed.store(true, Ordering::SeqCst);
        victim.probe.shutdown_release.notify_one();
        notified(&wake.ready).await;
        let observed =
            std::array::from_fn::<_, 4, _>(|index| wake.row[index].load(Ordering::SeqCst));
        let expected = if stopped { [1, 1, 0, 0] } else { [1, 0, 1, 0] };
        assert_eq!(
            observed, expected,
            "admission ticket woke before owned eviction timing settled"
        );
        let error = match tokio::time::timeout(DEADLINE, admission.as_mut())
            .await
            .unwrap()
        {
            Ok(_) => panic!("replacement was admitted after failed or stopped eviction"),
            Err(error) => error,
        };
        assert_eq!(
            error.code,
            if stopped {
                ErrorCode::Ebadf
            } else {
                ErrorCode::Eio
            }
        );
        if !stopped {
            assert_eq!(error.syscall.as_deref(), Some("original shutdown failure"));
        }
        assert_eq!(victim.probe.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(replacement.probe.opens.load(Ordering::SeqCst), 0);
        if stopped {
            tokio::time::timeout(DEADLINE, drain.as_mut())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(pool.snapshot().resident, 0);
        } else {
            assert_eq!(
                (pool.snapshot().resident, pool.snapshot().quarantined),
                (1, 1)
            );
            assert!(shutdown(&pool).await.is_err());
            assert_eq!(victim.probe.shutdowns.load(Ordering::SeqCst), 1);
        }
    }
}
