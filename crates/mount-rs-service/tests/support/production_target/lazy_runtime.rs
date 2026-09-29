//! Production-target worker runtime ownership.
//!
//! Prepared options are supplied once by Backend::options. This module neither
//! resolves provider configuration nor performs a fresh backing inspection.
//! The process supplies a static Arc<TargetRuntimeKeeper>; tests inject an Arc.
//!
//! Snapshot completeness is a serial observation, not an atomic cut or a drain
//! proof. Only the retained listener/pool/factory/context close futures supply
//! lifecycle acknowledgments. Timeout results stay sticky even after late ACKs.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::construction::ConstructionObserver;
use mount_rs_core::storage::ConcurrentBackingId;
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext};
use mount_rs_service::filesystem_runtime::{
    ConstructedRuntime, RuntimeConstructor, SdkRuntimeFactory,
};
use mount_rs_service::runtime_diagnostics::{RuntimeDiagnostics, RuntimeDiagnosticsSnapshot};
use mount_rs_service::runtime_pool::{DriveRegistration, RuntimePool, RuntimePoolSnapshot};
use mount_rs_service::server::RemoteServer;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::Sleep;

type CloseFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
const CLOSE_ALLOWANCE: Duration = Duration::from_secs(30);
const MAX_DRIVES: usize = 10_000;
const RUNTIME_ROWS: [&str; 8] = [
    "runtime.acquire",
    "runtime.activation_wait",
    "runtime.open",
    "runtime.eviction_shutdown",
    "runtime.terminal_drain",
    "runtime.handle_close",
    "runtime.state_mutex_wait",
    "runtime.state_mutex_hold",
];

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn uncertain() -> FsError {
    FsError::new(ErrorCode::Eio)
}

fn busy() -> FsError {
    FsError::new(ErrorCode::Ebusy)
}

fn invalid() -> FsError {
    FsError::new(ErrorCode::Einval)
}

fn publish(completion: &watch::Sender<Option<Result<()>>>, result: Result<()>) {
    completion.send_if_modified(|value| {
        if value.is_some() {
            false
        } else {
            *value = Some(result);
            true
        }
    });
}

async fn join(mut receiver: watch::Receiver<Option<Result<()>>>) -> Result<()> {
    loop {
        if let Some(result) = receiver.borrow().clone() {
            return result;
        }
        receiver.changed().await.map_err(|_| uncertain())?;
    }
}

/// Exact Backend split options and an actual initializer backing baseline.
/// No provider connection strings/options are serialized by this module.
#[derive(Clone)]
pub(super) struct PreparedDrive {
    pub(super) options: SplitOptions,
    pub(super) expected_backing: ConcurrentBackingId,
}

#[derive(Clone, Copy, Default)]
struct ConstructionObservation {
    observed_backing: Option<ConcurrentBackingId>,
    constructed: u64,
}

struct ActivationEvidence {
    observations: Mutex<Vec<ConstructionObservation>>,
    valid: AtomicBool,
}

impl ActivationEvidence {
    fn new(drives: usize) -> Arc<Self> {
        Arc::new(Self {
            observations: Mutex::new(vec![ConstructionObservation::default(); drives]),
            valid: AtomicBool::new(true),
        })
    }

    /// Infallible, bounded construction bookkeeping. Observer failure never
    /// changes the SDK constructor's successful product result.
    fn constructed(&self, drive: usize, backing: ConcurrentBackingId) {
        if self.observations.is_poisoned() {
            self.valid.store(false, Ordering::Release);
        }
        let mut observations = lock(&self.observations);
        let Some(observation) = observations.get_mut(drive) else {
            self.valid.store(false, Ordering::Release);
            return;
        };
        if observation
            .observed_backing
            .is_some_and(|previous| previous != backing)
        {
            self.valid.store(false, Ordering::Release);
        }
        observation.observed_backing = Some(backing);
        match observation.constructed.checked_add(1) {
            Some(count) => {
                observation.constructed = count;
                if count != 1 {
                    self.valid.store(false, Ordering::Release);
                }
            }
            None => self.valid.store(false, Ordering::Release),
        }
    }
}

struct TargetRuntimeConstructor {
    options: SplitOptions,
    context: StorageContext,
    expected_backing: ConcurrentBackingId,
    drive: usize,
    evidence: Arc<ActivationEvidence>,
}

#[async_trait]
impl RuntimeConstructor for TargetRuntimeConstructor {
    async fn construct(&self, observer: &dyn ConstructionObserver) -> Result<ConstructedRuntime> {
        let filesystem = Arc::new(
            Filesystem::split_with_context_and_construction_observer(
                self.options.clone(),
                &self.context,
                observer,
            )
            .await?,
        );
        // Register the actual lifecycle owner before validation, driver access,
        // bookkeeping or another await can fail/unwind.
        observer.retain(filesystem.clone());
        let observed = filesystem
            .concurrent_backing_id()
            .filter(|backing| *backing == self.expected_backing)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        self.evidence.constructed(self.drive, observed);
        let driver = filesystem.driver();
        Ok(ConstructedRuntime::new(filesystem, driver))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GenerationPhase {
    #[default]
    Serving,
    Listeners,
    Pool,
    Factories,
    Complete,
}

impl GenerationPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Serving => "serving",
            Self::Listeners => "listeners",
            Self::Pool => "pool",
            Self::Factories => "factories",
            Self::Complete => "complete",
        }
    }
}

struct GenerationResources {
    // Actual consuming listener futures exist before their first poll. A
    // borrowed waiter timeout cannot drop these futures or their servers.
    listeners: Vec<Option<CloseFuture>>,
    pool: Option<RuntimePool>,
    factories: Vec<Arc<SdkRuntimeFactory>>,
    phase: GenerationPhase,
    current: Option<CloseFuture>,
    budget: Option<Pin<Box<Sleep>>>,
    budget_expired: bool,
    next_factory: usize,
    listener_acknowledged: bool,
    listener_failed: bool,
    pool_acknowledged: bool,
    factories_acknowledged: usize,
    failure: Option<FsError>,
    closed: bool,
    final_pool: Option<RuntimePoolSnapshot>,
    final_diagnostics: Option<RuntimeDiagnosticsSnapshot>,
}

pub(super) struct TargetGenerationOwner {
    generation: u64,
    capacity: usize,
    plans: Arc<[PreparedDrive]>,
    context: StorageContext,
    worker: Arc<WorkerOwner>,
    evidence: Arc<ActivationEvidence>,
    closing: AtomicBool,
    owned: Mutex<GenerationResources>,
    completion: watch::Sender<Option<Result<()>>>,
}

impl TargetGenerationOwner {
    fn new(
        generation: u64,
        plans: Arc<[PreparedDrive]>,
        context: StorageContext,
        worker: Arc<WorkerOwner>,
    ) -> Result<Arc<Self>> {
        let capacity = plans.len();
        if !(1..=MAX_DRIVES).contains(&capacity) {
            return Err(invalid());
        }
        let diagnostics = if mount_rs_core::diagnostics::profile::enabled() {
            RuntimeDiagnostics::new(false)
        } else {
            RuntimeDiagnostics::default()
        };
        let pool = RuntimePool::with_diagnostics(capacity, diagnostics)?;
        Ok(Arc::new(Self {
            generation,
            capacity,
            plans,
            context,
            worker,
            evidence: ActivationEvidence::new(capacity),
            closing: AtomicBool::new(false),
            owned: Mutex::new(GenerationResources {
                listeners: Vec::new(),
                pool: Some(pool),
                factories: Vec::new(),
                phase: GenerationPhase::Serving,
                current: None,
                budget: None,
                budget_expired: false,
                next_factory: 0,
                listener_acknowledged: false,
                listener_failed: false,
                pool_acknowledged: false,
                factories_acknowledged: 0,
                failure: None,
                closed: false,
                final_pool: None,
                final_diagnostics: None,
            }),
            completion: watch::channel(None).0,
        }))
    }

    /// Retain the actual public factory before cold pool registration. The
    /// parent registers the returned handle with the exact catalog definition.
    pub(super) fn register(&self, drive: usize) -> Result<DriveRegistration> {
        let mut owned = lock(&self.owned);
        if self.owned.is_poisoned()
            || self.closing.load(Ordering::Acquire)
            || owned.phase != GenerationPhase::Serving
            || owned.failure.is_some()
            || drive != owned.factories.len()
        {
            return Err(busy());
        }
        let plan = self.plans.get(drive).ok_or_else(invalid)?;
        let factory = SdkRuntimeFactory::new(Arc::new(TargetRuntimeConstructor {
            options: plan.options.clone(),
            context: self.context.clone(),
            expected_backing: plan.expected_backing,
            drive,
            evidence: self.evidence.clone(),
        }));
        owned.factories.push(factory.clone());
        let pool = owned.pool.as_ref().ok_or_else(uncertain)?;
        match pool.register(factory) {
            Ok(registration) => Ok(registration),
            Err(error) => {
                owned.failure.get_or_insert_with(|| error.clone());
                Err(error)
            }
        }
    }

    /// Called synchronously immediately after successful bind, before another
    /// await. Retains the passed server even when invalid lifecycle is detected.
    pub(super) fn install_server(self: &Arc<Self>, server: RemoteServer) -> Result<()> {
        let worker = self.worker.clone();
        // Serialize an actual handoff against terminal context/keeper release.
        // Invalid old-generation handoffs are quarantined in the same keeper.
        let mut keeper = lock(&worker.keeper.0);
        keeper.retain(worker.clone());
        let mut worker_resources = lock(&worker.owned);
        let mut owned = lock(&self.owned);
        let valid = !worker.keeper.0.is_poisoned()
            && !self.owned.is_poisoned()
            && !self.closing.load(Ordering::Acquire)
            && !worker.closing.load(Ordering::Acquire)
            && !worker.owned.is_poisoned()
            && worker_resources.failure.is_none()
            && worker_resources
                .generation
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, self))
            && owned.phase == GenerationPhase::Serving
            && owned.listeners.is_empty()
            && owned.failure.is_none();
        owned.listeners.push(Some(Box::pin(async move {
            server.close().await;
            Ok(())
        })));
        owned.listener_acknowledged = false;
        owned.closed = false;
        if valid {
            Ok(())
        } else {
            let error = busy();
            owned.failure.get_or_insert_with(|| error.clone());
            worker_resources
                .failure
                .get_or_insert_with(|| error.clone());
            if !worker_resources
                .generation
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, self))
                && !worker_resources
                    .retained_generations
                    .iter()
                    .any(|retained| Arc::ptr_eq(retained, self))
            {
                worker_resources.retained_generations.push(self.clone());
            }
            Err(error)
        }
    }

    pub(super) fn pool_snapshot(&self) -> RuntimePoolSnapshot {
        let owned = lock(&self.owned);
        match &owned.pool {
            Some(pool) => pool.snapshot(),
            None => owned
                .final_pool
                .expect("actual successful pool close snapshot"),
        }
    }

    /// Fixed bounded schema. IDs are captured observations after successful SDK
    /// construction and expected-ID validation, not fresh provider inspections.
    /// They remain distinct from the pool's successful handoff/open counters.
    pub(super) fn runtime_activation_snapshot(&self) -> Result<Value> {
        if self.owned.is_poisoned() || self.evidence.observations.is_poisoned() {
            return Err(uncertain());
        }
        let (pool, diagnostics, owner_valid) = {
            let owned = lock(&self.owned);
            match &owned.pool {
                Some(pool) => (pool.snapshot(), pool.diagnostics(), owned.failure.is_none()),
                None => (
                    owned.final_pool.ok_or_else(uncertain)?,
                    owned.final_diagnostics.clone(),
                    owned.failure.is_none(),
                ),
            }
        };
        let observations = lock(&self.evidence.observations);
        if observations.len() != self.capacity || self.plans.len() != self.capacity {
            return Err(uncertain());
        }
        let mut valid = owner_valid && self.evidence.valid.load(Ordering::Acquire);
        let mut constructed = 0_u64;
        let rows: Vec<Value> = observations
            .iter()
            .zip(self.plans.iter())
            .enumerate()
            .map(|(drive, (observation, plan))| {
                valid &= match (observation.constructed, observation.observed_backing) {
                    (0, None) => true,
                    (1, Some(actual)) => actual == plan.expected_backing,
                    _ => false,
                };
                match constructed.checked_add(observation.constructed) {
                    Some(count) => constructed = count,
                    None => valid = false,
                }
                json!({
                    "drive":drive,
                    "expected_backing":plan.expected_backing.to_hex(),
                    "observed_backing":observation.observed_backing.map(ConcurrentBackingId::to_hex),
                    "constructed":observation.constructed,
                })
            })
            .collect();
        // Separate atomic/mutex samples: disagreement makes observation
        // incomplete and can be recaptured. No product operation is changed.
        valid &= pool.registered == self.capacity
            && pool.resident <= self.capacity
            && pool.open_success as u64 == constructed
            && pool.opening == 0
            && pool.closing == 0
            && pool.quarantined == 0
            && pool.pinned == 0
            && pool.open_error == 0
            && pool.eviction_error == 0
            && pool.eviction_success == 0;
        let available = diagnostics.is_some();
        let complete = valid && diagnostics.as_ref().is_some_and(diagnostics_complete);
        Ok(json!({
            "schema":"mount-rs.target-runtime.v1",
            "generation":self.generation,
            "capacity":self.capacity,
            "available":available,
            "complete":complete,
            "pool":pool,
            "diagnostics":diagnostics,
            "observations":rows,
        }))
    }

    fn cleanup_acknowledged(&self) -> bool {
        let owned = lock(&self.owned);
        !self.owned.is_poisoned()
            && owned.closed
            && owned.failure.is_none()
            && owned.listener_acknowledged
            && owned.pool_acknowledged
            && owned.listeners.iter().all(Option::is_none)
            && owned.pool.is_none()
            && owned.factories.is_empty()
            && owned.final_pool.is_some()
    }

    fn closed_successfully(&self) -> bool {
        let owned = lock(&self.owned);
        !self.owned.is_poisoned()
            && owned.closed
            && owned.failure.is_none()
            && owned.listener_acknowledged
            && owned.pool_acknowledged
            && owned.listeners.iter().all(Option::is_none)
            && owned.pool.is_none()
            && owned.factories.is_empty()
            && owned.factories_acknowledged == self.capacity
            && owned
                .final_pool
                .is_some_and(|snapshot| snapshot.registered == self.capacity)
            && matches!(self.completion.borrow().as_ref(), Some(Ok(())))
    }

    fn request_close(self: &Arc<Self>) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        let guard = GenerationDrainGuard {
            generation: self.clone(),
            completed: false,
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                // The task owns only a poller. All actual owner futures and
                // resources remain installed in the process-retained generation.
                drop(runtime.spawn(async move { guard.run().await }));
            }
            Err(_) => drop(guard),
        }
    }

    pub(super) async fn close(self: &Arc<Self>) -> Result<()> {
        self.request_close();
        join(self.completion.subscribe()).await?;
        if self.cleanup_acknowledged() {
            Ok(())
        } else {
            Err(uncertain())
        }
    }

    fn record_failure(&self, error: FsError) {
        let error = {
            let mut owned = lock(&self.owned);
            owned.failure.get_or_insert(error).clone()
        };
        publish(&self.completion, Err(error));
    }

    fn poll_budget(&self, owned: &mut GenerationResources, cx: &mut Context<'_>) {
        if !owned.budget_expired
            && owned
                .budget
                .as_mut()
                .is_some_and(|budget| budget.as_mut().poll(cx).is_ready())
        {
            owned.budget_expired = true;
            // EIO is a closed typed uncertainty result; no error message or
            // provider configuration enters the public snapshot.
            let error = owned.failure.get_or_insert_with(uncertain).clone();
            publish(&self.completion, Err(error));
        }
    }

    fn poll_listeners(&self, owned: &mut GenerationResources, cx: &mut Context<'_>) {
        let mut failure = None;
        for listener in &mut owned.listeners {
            if let Some(future) = listener
                && let Poll::Ready(result) = future.as_mut().poll(cx)
            {
                if let Err(error) = result {
                    failure.get_or_insert(error);
                }
                *listener = None;
            }
        }
        if let Some(error) = failure {
            owned.listener_failed = true;
            let error = owned.failure.get_or_insert(error).clone();
            publish(&self.completion, Err(error));
        }
        owned.listener_acknowledged =
            !owned.listener_failed && owned.listeners.iter().all(Option::is_none);
    }

    fn poll_close(&self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let mut owned = lock(&self.owned);
        if self.owned.is_poisoned() {
            let error = owned.failure.get_or_insert_with(uncertain).clone();
            return Poll::Ready(Err(error));
        }
        if owned.phase == GenerationPhase::Serving {
            owned.phase = GenerationPhase::Listeners;
            owned.budget = Some(Box::pin(tokio::time::sleep(CLOSE_ALLOWANCE)));
        }
        loop {
            // Pending consuming listener futures remain installed and are
            // polled alongside storage after the listener allowance expires.
            self.poll_listeners(&mut owned, cx);
            if owned.phase == GenerationPhase::Listeners {
                self.poll_budget(&mut owned, cx);
                if owned.listeners.iter().any(Option::is_some) && !owned.budget_expired {
                    return Poll::Pending;
                }
                // Match the existing listener30s then replica30s progression:
                // storage starts after listener ACK OR listener deadline, once.
                // A pending listener remains retained; timeout stays sticky.
                owned.phase = GenerationPhase::Pool;
                owned.budget = Some(Box::pin(tokio::time::sleep(CLOSE_ALLOWANCE)));
                owned.budget_expired = false;
            }
            if let Some(future) = &mut owned.current {
                match future.as_mut().poll(cx) {
                    Poll::Pending => {
                        self.poll_budget(&mut owned, cx);
                        return Poll::Pending;
                    }
                    Poll::Ready(result) => {
                        self.poll_budget(&mut owned, cx);
                        owned.current = None;
                        match (owned.phase, result) {
                            (_, Err(error)) => {
                                // Pool ACK is required before factory joins;
                                // any factory error stops before dependent
                                // context closure and retains every owner.
                                let error = owned.failure.get_or_insert(error).clone();
                                publish(&self.completion, Err(error));
                                owned.phase = GenerationPhase::Complete;
                            }
                            (GenerationPhase::Pool, Ok(())) => {
                                owned.pool_acknowledged = true;
                                owned.phase = GenerationPhase::Factories;
                            }
                            (GenerationPhase::Factories, Ok(())) => {
                                owned.factories_acknowledged += 1;
                            }
                            _ => return Poll::Ready(Err(uncertain())),
                        }
                    }
                }
            }
            match owned.phase {
                GenerationPhase::Pool => {
                    let Some(pool) = owned.pool.clone() else {
                        return Poll::Ready(Err(uncertain()));
                    };
                    owned.current = Some(Box::pin(async move { pool.shutdown().await }));
                }
                GenerationPhase::Factories => {
                    if let Some(factory) = owned.factories.get(owned.next_factory).cloned() {
                        owned.next_factory += 1;
                        owned.current = Some(Box::pin(async move { factory.close().await }));
                    } else {
                        owned.phase = GenerationPhase::Complete;
                    }
                }
                GenerationPhase::Complete => {
                    self.poll_budget(&mut owned, cx);
                    if owned.listeners.iter().any(Option::is_some) {
                        return Poll::Pending;
                    }
                    if let Some(error) = &owned.failure {
                        // Retain even late-ACK owners after an exceeded budget.
                        return Poll::Ready(Err(error.clone()));
                    }
                    let Some(pool) = &owned.pool else {
                        return Poll::Ready(Err(uncertain()));
                    };
                    let final_pool = pool.snapshot();
                    let final_diagnostics = pool.diagnostics();
                    if final_pool.resident != 0
                        || final_pool.opening != 0
                        || final_pool.ready != 0
                        || final_pool.closing != 0
                        || final_pool.quarantined != 0
                        || final_pool.pinned != 0
                        || !owned.listener_acknowledged
                        || !owned.pool_acknowledged
                        || owned.factories_acknowledged != owned.factories.len()
                    {
                        return Poll::Ready(Err(uncertain()));
                    }
                    owned.final_pool = Some(final_pool);
                    owned.final_diagnostics = final_diagnostics;
                    owned.pool = None;
                    owned.factories.clear();
                    owned.listeners.clear();
                    owned.budget = None;
                    owned.closed = true;
                    return Poll::Ready(Ok(()));
                }
                GenerationPhase::Serving | GenerationPhase::Listeners => {
                    return Poll::Ready(Err(uncertain()));
                }
            }
        }
    }
}

fn diagnostics_complete(snapshot: &RuntimeDiagnosticsSnapshot) -> bool {
    !snapshot.counter_saturated
        && !snapshot.concurrent_activity
        && snapshot
            .entries
            .iter()
            .zip(RUNTIME_ROWS)
            .all(|(row, name)| {
                row.name == name
                    && row.in_flight == 0
                    && row
                        .success
                        .checked_add(row.error)
                        .and_then(|count| count.checked_add(row.cancelled))
                        == Some(row.calls)
            })
}

struct GenerationDrainGuard {
    generation: Arc<TargetGenerationOwner>,
    completed: bool,
}

impl GenerationDrainGuard {
    async fn run(mut self) {
        let result = std::future::poll_fn(|cx| self.generation.poll_close(cx)).await;
        if let Err(error) = &result {
            self.generation.record_failure(error.clone());
        }
        publish(&self.generation.completion, result);
        self.completed = true;
    }
}

impl Drop for GenerationDrainGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.generation.record_failure(uncertain());
        }
    }
}

/// A process-lifetime strong owner, reserved before StorageContext::new and
/// before any provider poll. The process must store its Arc in a static slot.
#[derive(Default)]
pub(super) struct TargetRuntimeKeeper(Mutex<KeeperState>);

#[derive(Default)]
struct KeeperState {
    active: Option<Arc<WorkerOwner>>,
    // Only an illegal late handoff can strand a previously acknowledged owner.
    // Its actual passed resources remain here, never in a stale local scope.
    quarantined: Vec<Arc<WorkerOwner>>,
}

impl KeeperState {
    fn retain(&mut self, owner: Arc<WorkerOwner>) {
        match &self.active {
            Some(active) if Arc::ptr_eq(active, &owner) => {}
            None => self.active = Some(owner),
            Some(_) => {
                if !self.quarantined.iter().any(|old| Arc::ptr_eq(old, &owner)) {
                    self.quarantined.push(owner);
                }
            }
        }
    }
}

impl TargetRuntimeKeeper {
    pub(super) fn reserve(self: &Arc<Self>) -> Result<TargetRuntimeScope> {
        let mut slot = lock(&self.0);
        if self.0.is_poisoned() || slot.active.is_some() || !slot.quarantined.is_empty() {
            return Err(busy());
        }
        let owner = Arc::new(WorkerOwner {
            // This retained cycle is released only after actual terminal ACKs.
            // Even a dropped injected keeper cannot drop uncertain resources.
            keeper: self.clone(),
            closing: AtomicBool::new(false),
            owned: Mutex::new(WorkerResources::default()),
            completion: watch::channel(None).0,
        });
        slot.active = Some(owner.clone());
        Ok(TargetRuntimeScope(owner))
    }

    #[cfg(test)]
    pub(super) fn occupied(&self) -> bool {
        let slot = lock(&self.0);
        slot.active.is_some() || !slot.quarantined.is_empty()
    }
}

#[derive(Default)]
struct WorkerResources {
    // A duplicate/late installation retains the passed actual context and
    // poisons qualification; it never replaces/drops the prior context owner.
    contexts: Vec<StorageContext>,
    plans: Option<Arc<[PreparedDrive]>>,
    generation: Option<Arc<TargetGenerationOwner>>,
    final_generation: Option<Value>,
    retained_generations: Vec<Arc<TargetGenerationOwner>>,
    current: Option<CloseFuture>,
    context_budget: Option<Pin<Box<Sleep>>>,
    generation_acknowledged: bool,
    context_started: bool,
    context_closed: bool,
    failure: Option<FsError>,
    complete: bool,
}

struct WorkerOwner {
    keeper: Arc<TargetRuntimeKeeper>,
    closing: AtomicBool,
    owned: Mutex<WorkerResources>,
    completion: watch::Sender<Option<Result<()>>>,
}

/// Drop requests the same independently owned terminal drain. The static
/// keeper remains occupied on panic, cancellation, timeout or runtime loss.
pub(super) struct TargetRuntimeScope(Arc<WorkerOwner>);

impl TargetRuntimeScope {
    pub(super) fn install_context(&self, context: StorageContext) -> Result<()> {
        let mut keeper = lock(&self.0.keeper.0);
        keeper.retain(self.0.clone());
        let mut owned = lock(&self.0.owned);
        owned.contexts.push(context);
        owned.context_closed = false;
        if self.0.keeper.0.is_poisoned()
            || self.0.owned.is_poisoned()
            || self.0.closing.load(Ordering::Acquire)
            || owned.contexts.len() != 1
            || owned.plans.is_some()
            || owned.generation.is_some()
        {
            let error = busy();
            owned.failure.get_or_insert_with(|| error.clone());
            Err(error)
        } else {
            Ok(())
        }
    }

    pub(super) fn install_prepared(&self, plans: Vec<PreparedDrive>) -> Result<()> {
        let mut owned = lock(&self.0.owned);
        if self.0.owned.is_poisoned()
            || self.0.closing.load(Ordering::Acquire)
            || owned.failure.is_some()
            || owned.contexts.len() != 1
            || owned.plans.is_some()
            || owned.generation.is_some()
            || !(1..=MAX_DRIVES).contains(&plans.len())
        {
            return Err(invalid());
        }
        owned.plans = Some(plans.into());
        Ok(())
    }

    pub(super) fn new_generation(&self, generation: u64) -> Result<Arc<TargetGenerationOwner>> {
        let mut owned = lock(&self.0.owned);
        if self.0.owned.is_poisoned()
            || self.0.closing.load(Ordering::Acquire)
            || owned.failure.is_some()
            || owned.contexts.len() != 1
        {
            return Err(busy());
        }
        match &owned.generation {
            Some(previous)
                if previous.closed_successfully()
                    && previous.generation.checked_add(1) == Some(generation) => {}
            None if generation == 0 => {}
            _ => return Err(busy()),
        }
        let plans = owned.plans.clone().ok_or_else(invalid)?;
        let next = TargetGenerationOwner::new(
            generation,
            plans,
            owned.contexts[0].clone(),
            self.0.clone(),
        )?;
        // The actual pool and generation are in the process owner before the
        // caller can create a factory, register a route or await bind.
        owned.generation = Some(next.clone());
        Ok(next)
    }

    pub(super) async fn close(&self) -> Result<()> {
        self.0.request_close();
        join(self.0.completion.subscribe()).await?;
        let owned = lock(&self.0.owned);
        if !self.0.owned.is_poisoned()
            && owned.complete
            && owned.failure.is_none()
            && owned.retained_generations.is_empty()
            && owned
                .generation
                .as_ref()
                .is_none_or(|generation| generation.cleanup_acknowledged())
        {
            Ok(())
        } else {
            Err(uncertain())
        }
    }

    pub(super) async fn close_context(&self) -> Result<()> {
        // Rejoins terminal closure; never bypasses generation prerequisites.
        self.close().await
    }

    pub(super) fn context_closed(&self) -> bool {
        lock(&self.0.owned).context_closed
    }

    pub(super) fn snapshot(&self) -> Result<Value> {
        if self.0.owned.is_poisoned() {
            return Err(uncertain());
        }
        let owned = lock(&self.0.owned);
        let generation = owned
            .generation
            .as_ref()
            .map(|generation| {
                if generation.owned.is_poisoned() {
                    return Err(uncertain());
                }
                let resources = lock(&generation.owned);
                Ok(json!({
                    "generation":generation.generation,
                    "phase":resources.phase.name(),
                    "listener_acknowledged":resources.listener_acknowledged,
                    "pool_acknowledged":resources.pool_acknowledged,
                    "factories_acknowledged":resources.factories_acknowledged,
                    "closed":resources.closed,
                    "unproven":resources.failure.is_some(),
                }))
            })
            .transpose()?
            .or_else(|| owned.final_generation.clone());
        let unproven = owned.failure.is_some()
            || generation
                .as_ref()
                .is_some_and(|generation| generation["unproven"] == true);
        Ok(json!({
            "schema":"mount-rs.target-runtime-owner.v1",
            "closing":self.0.closing.load(Ordering::Acquire),
            "generation":generation,
            "context_installed":!owned.contexts.is_empty(),
            "context_closed":owned.context_closed,
            "unproven":unproven,
            "complete":owned.complete && !unproven,
        }))
    }
}

impl Drop for TargetRuntimeScope {
    fn drop(&mut self) {
        self.0.request_close();
    }
}

impl WorkerOwner {
    fn request_close(self: &Arc<Self>) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        let guard = WorkerDrainGuard {
            owner: self.clone(),
            completed: false,
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn(async move { guard.run().await })),
            Err(_) => drop(guard),
        }
    }

    fn record_failure(&self, error: FsError) {
        let error = {
            let mut owned = lock(&self.owned);
            owned.failure.get_or_insert(error).clone()
        };
        publish(&self.completion, Err(error));
    }

    fn poll_close(&self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let mut owned = lock(&self.owned);
        if self.keeper.0.is_poisoned()
            || self.owned.is_poisoned()
            || (owned.failure.is_some() && !(owned.context_started && owned.current.is_some()))
        {
            let error = owned.failure.get_or_insert_with(uncertain).clone();
            return Poll::Ready(Err(error));
        }
        loop {
            if owned.context_started
                && owned.current.is_some()
                && owned
                    .context_budget
                    .as_mut()
                    .is_some_and(|budget| budget.as_mut().poll(cx).is_ready())
            {
                owned.context_budget = None;
                let error = owned.failure.get_or_insert_with(uncertain).clone();
                // Timeout publishes failure but never removes the retained
                // actual future. The same worker continues polling it.
                publish(&self.completion, Err(error));
            }
            if let Some(future) = &mut owned.current {
                match future.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => {
                        if owned.context_started
                            && owned
                                .context_budget
                                .as_mut()
                                .is_some_and(|budget| budget.as_mut().poll(cx).is_ready())
                        {
                            owned.context_budget = None;
                            let error = owned.failure.get_or_insert_with(uncertain).clone();
                            publish(&self.completion, Err(error));
                        }
                        owned.current = None;
                        if let Err(error) = result {
                            let error = owned.failure.get_or_insert(error).clone();
                            return Poll::Ready(Err(error));
                        }
                        if owned.context_started {
                            owned.context_closed = true;
                            if let Some(error) = &owned.failure {
                                return Poll::Ready(Err(error.clone()));
                            }
                        } else {
                            owned.generation_acknowledged = true;
                        }
                    }
                }
            }
            if !owned.generation_acknowledged {
                match owned.generation.clone() {
                    Some(generation) => {
                        owned.current = Some(Box::pin(async move { generation.close().await }));
                    }
                    None => owned.generation_acknowledged = true,
                }
                continue;
            }
            if !owned.context_started {
                if owned.contexts.len() > 1 {
                    return Poll::Ready(Err(uncertain()));
                }
                match owned.contexts.first().cloned() {
                    Some(context) => {
                        owned.context_started = true;
                        owned.context_budget = Some(Box::pin(tokio::time::sleep(CLOSE_ALLOWANCE)));
                        owned.current = Some(Box::pin(async move { context.close().await }));
                        continue;
                    }
                    None => {
                        // A failed StorageContext::new created no actual owner.
                        owned.context_started = true;
                    }
                }
            }
            if let Some(error) = &owned.failure {
                return Poll::Ready(Err(error.clone()));
            }
            if !owned.contexts.is_empty() && !owned.context_closed {
                return Poll::Ready(Err(uncertain()));
            }
            // Drop no actual resources before the preceding explicit ACKs.
            if let Some(generation) = owned.generation.clone() {
                let resources = lock(&generation.owned);
                owned.final_generation = Some(json!({
                    "generation":generation.generation,
                    "phase":resources.phase.name(),
                    "listener_acknowledged":resources.listener_acknowledged,
                    "pool_acknowledged":resources.pool_acknowledged,
                    "factories_acknowledged":resources.factories_acknowledged,
                    "closed":resources.closed,
                    "unproven":resources.failure.is_some(),
                }));
            }
            // Break Worker->Generation->Worker only after actual ACKs. An
            // external old generation keeps the worker and keeper reachable
            // so a forbidden late actual handoff is still quarantined.
            owned.generation = None;
            owned.contexts.clear();
            owned.plans = None;
            owned.context_budget = None;
            owned.complete = true;
            return Poll::Ready(Ok(()));
        }
    }
}

struct WorkerDrainGuard {
    owner: Arc<WorkerOwner>,
    completed: bool,
}

impl WorkerDrainGuard {
    async fn run(mut self) {
        let mut result = std::future::poll_fn(|cx| self.owner.poll_close(cx)).await;
        if result.is_ok() {
            let mut slot = lock(&self.owner.keeper.0);
            let owned = lock(&self.owner.owned);
            // Installation takes these same locks in this order. Validate
            // poison and publish success within the keeper-release boundary.
            if !self.owner.keeper.0.is_poisoned()
                && !self.owner.owned.is_poisoned()
                && owned.complete
                && owned.failure.is_none()
                && owned.retained_generations.is_empty()
                && owned
                    .generation
                    .as_ref()
                    .is_none_or(|generation| generation.cleanup_acknowledged())
                && slot
                    .active
                    .as_ref()
                    .is_some_and(|owner| Arc::ptr_eq(owner, &self.owner))
            {
                slot.active = None;
                publish(&self.owner.completion, Ok(()));
                self.completed = true;
                return;
            }
            // A poison or late handoff after the actual drain cannot reuse a
            // cached success or release uncertain process-lifetime authority.
            result = Err(uncertain());
        }
        if let Err(error) = &result {
            self.owner.record_failure(error.clone());
        }
        publish(&self.owner.completion, result);
        self.completed = true;
    }
}

impl Drop for WorkerDrainGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.owner.record_failure(uncertain());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::FsDriver;
    use mount_rs_core::Loopback;
    use mount_rs_sdk::StoreConfig;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Notify;

    const TEST_BOUND: Duration = Duration::from_secs(5);

    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(TEST_BOUND, future)
            .await
            .expect("owned lazy target control did not settle")
    }

    async fn settle(predicate: impl Fn() -> bool) {
        // A runnable polling loop prevents Tokio's paused clock from advancing.
        // Keep an independent real-time bound on these test observations.
        let deadline = std::time::Instant::now() + TEST_BOUND;
        bounded(async {
            while !predicate() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "owned lazy target control did not settle before wall deadline"
                );
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    async fn context_usable(context: &StorageContext) -> bool {
        match Filesystem::split_with_context(
            SplitOptions::memory("lazy-target-context-probe", 4096),
            context,
        )
        .await
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

    async fn full_bytes(driver: &Arc<dyn FsDriver>, expected: &[u8]) {
        let handle = driver.open("/file", "r", 0).await.unwrap();
        assert_eq!(handle.stat().await.unwrap().size, expected.len() as u64);
        let mut actual = vec![0; expected.len()];
        let mut offset = 0;
        while offset < actual.len() {
            let end = actual.len().min(offset + 997);
            let count = handle
                .read(&mut actual[offset..end], Some(offset as u64))
                .await
                .unwrap();
            assert!(count > 0 && count <= end - offset);
            offset += count;
        }
        assert_eq!(actual, expected);
        assert_eq!(
            handle
                .read(&mut [0; 19], Some(expected.len() as u64))
                .await
                .unwrap(),
            0
        );
        handle.close().await.unwrap();
    }

    /// Each initializer and each fresh oracle owns a separate actual context.
    /// Keep the fixture on any failed proof; remove only after all actual ACKs.
    async fn initialized(drives: usize) -> (PathBuf, Vec<PreparedDrive>, Vec<Vec<u8>>) {
        let directory = tempfile::tempdir().unwrap().keep();
        let context = StorageContext::new(16).unwrap();
        let mut plans = Vec::new();
        let mut payloads = Vec::new();
        for drive in 0..drives {
            let mut options = SplitOptions::memory(format!("lazy-target-{drive}"), 4096)
                .with_concurrent_writes(true)
                .with_inode_updates(true)
                .with_compact_inode_updates(true);
            options.metadata = StoreConfig::Sqlite {
                path: directory.join(format!("drive-{drive}.sqlite")),
            };
            options.blocks = options.metadata.clone();
            let filesystem = Filesystem::split_with_context(options.clone(), &context)
                .await
                .unwrap();
            let expected_backing = filesystem.concurrent_backing_id().unwrap();
            let payload: Vec<_> = (0..6107)
                .map(|index| ((index * 31 + drive * 73) % 251) as u8)
                .collect();
            Loopback::from_arc(filesystem.driver())
                .write_file("/file", &payload)
                .await
                .unwrap();
            filesystem.shutdown().await.unwrap();
            plans.push(PreparedDrive {
                options,
                expected_backing,
            });
            payloads.push(payload);
        }
        context.close().await.unwrap();
        (directory, plans, payloads)
    }

    async fn fresh_oracle(plans: &[PreparedDrive], payloads: &[Vec<u8>]) {
        assert_eq!(plans.len(), payloads.len());
        let context = StorageContext::new(16).unwrap();
        for (plan, expected) in plans.iter().zip(payloads) {
            let filesystem = Filesystem::split_with_context(plan.options.clone(), &context)
                .await
                .unwrap();
            assert_eq!(
                filesystem.concurrent_backing_id(),
                Some(plan.expected_backing)
            );
            full_bytes(&filesystem.driver(), expected).await;
            filesystem.shutdown().await.unwrap();
        }
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn lazy_target_prepared_registration_preserves_full_bytes_and_backing_across_generation_reopen()
     {
        let (directory, plans, payloads) = initialized(2).await;
        let keeper = Arc::new(TargetRuntimeKeeper::default());
        let scope = keeper.reserve().unwrap();
        let context = StorageContext::new(16).unwrap();
        scope.install_context(context.clone()).unwrap();
        scope.install_prepared(plans.clone()).unwrap();
        for generation in 0..2 {
            let owner = scope.new_generation(generation).unwrap();
            let registrations: Vec<_> = (0..plans.len())
                .map(|drive| owner.register(drive).unwrap())
                .collect();
            let cold = owner.runtime_activation_snapshot().unwrap();
            assert_eq!(cold["capacity"], 2);
            assert_eq!(cold["pool"]["registered"], 2);
            assert_eq!(cold["pool"]["resident"], 0);
            assert_eq!(cold["pool"]["open_success"], 0);
            assert!(
                cold["observations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| { row["constructed"] == 0 && row["observed_backing"].is_null() })
            );
            for (registration, expected) in registrations.iter().zip(&payloads) {
                let lease = bounded(registration.acquire()).await.unwrap();
                full_bytes(lease.driver(), expected).await;
                drop(lease);
            }
            bounded(owner.close()).await.unwrap();
            let closed = owner.runtime_activation_snapshot().unwrap();
            assert_eq!(closed["pool"]["registered"], 2);
            assert_eq!(closed["pool"]["resident"], 0);
            assert_eq!(closed["pool"]["open_success"], 2);
            assert!(
                closed["observations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| {
                        row["constructed"] == 1
                            && row["observed_backing"] == row["expected_backing"]
                    })
            );
            assert!(!scope.context_closed());
        }
        bounded(scope.close_context()).await.unwrap();
        assert!(scope.context_closed());
        assert!(!keeper.occupied());
        assert!(!context_usable(&context).await);
        fresh_oracle(&plans, &payloads).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn lazy_target_cancelled_waiter_retains_real_lease_and_rejoins_same_acknowledged_drain() {
        let (directory, plans, payloads) = initialized(1).await;
        let keeper = Arc::new(TargetRuntimeKeeper::default());
        let scope = keeper.reserve().unwrap();
        let context = StorageContext::new(16).unwrap();
        scope.install_context(context.clone()).unwrap();
        scope.install_prepared(plans.clone()).unwrap();
        let generation = scope.new_generation(0).unwrap();
        let registration = generation.register(0).unwrap();
        let lease = bounded(registration.acquire()).await.unwrap();
        let waiting_generation = generation.clone();
        let waiter = tokio::spawn(async move { waiting_generation.close().await });
        settle(|| {
            let owned = lock(&generation.owned);
            owned.phase == GenerationPhase::Pool && owned.current.is_some()
        })
        .await;
        assert!(matches!(bounded(registration.acquire()).await,
            Err(error) if error.code == ErrorCode::Ebadf));
        waiter.abort();
        assert!(bounded(waiter).await.unwrap_err().is_cancelled());
        assert!(keeper.occupied());
        assert!(!scope.context_closed());
        assert!(context_usable(&context).await);
        assert!(matches!(scope.new_generation(1), Err(error) if error.code == ErrorCode::Ebusy));
        full_bytes(lease.driver(), &payloads[0]).await;
        drop(lease);
        bounded(generation.close()).await.unwrap();
        bounded(scope.close_context()).await.unwrap();
        assert!(!keeper.occupied());
        fresh_oracle(&plans, &payloads).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn lazy_target_backing_mismatch_preserves_typed_error_without_retry_or_context_close() {
        let (directory, plans, payloads) = initialized(1).await;
        let keeper = Arc::new(TargetRuntimeKeeper::default());
        let scope = keeper.reserve().unwrap();
        let context = StorageContext::new(16).unwrap();
        scope.install_context(context.clone()).unwrap();
        let mut wrong = plans[0].expected_backing.as_bytes();
        wrong[0] ^= 1;
        if wrong == [0; 16] {
            wrong[1] = 1;
        }
        let mut mismatched = plans.clone();
        mismatched[0].expected_backing = ConcurrentBackingId::from_bytes(wrong).unwrap();
        scope.install_prepared(mismatched).unwrap();
        let generation = scope.new_generation(0).unwrap();
        let registration = generation.register(0).unwrap();
        for _ in 0..2 {
            assert!(matches!(bounded(registration.acquire()).await,
                Err(error) if error.code == ErrorCode::Estale));
        }
        assert_eq!(generation.pool_snapshot().open_error, 1);
        assert_eq!(generation.pool_snapshot().quarantined, 1);
        let snapshot = generation.runtime_activation_snapshot().unwrap();
        assert_eq!(snapshot["observations"][0]["constructed"], 0);
        assert!(snapshot["observations"][0]["observed_backing"].is_null());
        assert_eq!(snapshot["complete"], false);
        assert!(bounded(scope.close_context()).await.is_err());
        assert!(!scope.context_closed());
        assert!(context_usable(&context).await);
        assert!(matches!(scope.new_generation(1), Err(error) if error.code == ErrorCode::Ebusy));
        assert!(keeper.occupied());
        fresh_oracle(&plans, &payloads).await;
        // Negative ownership evidence and persistent fixture stay retained to
        // test-process exit. A fresh oracle is not a failed pool's close ACK.
        assert!(directory.exists());
    }

    #[test]
    fn lazy_target_no_runtime_retains_unknown_context_even_when_injected_keeper_is_dropped() {
        let keeper = Arc::new(TargetRuntimeKeeper::default());
        let weak_keeper = Arc::downgrade(&keeper);
        let scope = keeper.reserve().unwrap();
        scope
            .install_context(StorageContext::new(16).unwrap())
            .unwrap();
        let weak_owner = Arc::downgrade(&scope.0);
        drop(scope);
        assert!(keeper.occupied());
        assert!(matches!(keeper.reserve(), Err(error) if error.code == ErrorCode::Ebusy));
        drop(keeper);
        let retained = weak_owner.upgrade().unwrap();
        assert!(weak_keeper.upgrade().is_some());
        assert!(matches!(retained.completion.borrow().as_ref(),
            Some(Err(error)) if error.code == ErrorCode::Eio));
        assert!(!lock(&retained.owned).context_closed);
    }

    struct PendingListenerOwner(Arc<AtomicUsize>);

    impl Drop for PendingListenerOwner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn lazy_target_listener_deadline_starts_one_storage_allowance_without_replacement_or_context_ack()
     {
        let (directory, plans, _) = initialized(1).await;
        let keeper = Arc::new(TargetRuntimeKeeper::default());
        let scope = keeper.reserve().unwrap();
        let context = StorageContext::new(16).unwrap();
        scope.install_context(context.clone()).unwrap();
        scope.install_prepared(plans).unwrap();
        let generation = scope.new_generation(0).unwrap();
        let registration = generation.register(0).unwrap();
        let lease = bounded(registration.acquire()).await.unwrap();
        let dropped = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(Notify::new());
        let marker = Arc::new(PendingListenerOwner(dropped.clone()));
        let weak_marker = Arc::downgrade(&marker);
        let entered_by_listener = entered.clone();
        // Controlled consuming-future ownership; this deliberately does not
        // claim to emulate the internal RemoteServer listener implementation.
        lock(&generation.owned)
            .listeners
            .push(Some(Box::pin(async move {
                let _owner = marker;
                entered_by_listener.notify_one();
                std::future::pending::<()>().await;
                Ok(())
            })));
        tokio::time::pause();
        generation.request_close();
        bounded(entered.notified()).await;
        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(lock(&generation.owned).phase, GenerationPhase::Listeners);
        tokio::time::advance(Duration::from_secs(1)).await;
        // Tokio rounds deadlines upward to a millisecond timer tick. Cross
        // that rounding boundary without changing the owner's 30s allowance.
        tokio::time::advance(Duration::from_millis(2)).await;
        settle(|| lock(&generation.owned).phase == GenerationPhase::Pool).await;
        assert_eq!(
            lock(&generation.owned)
                .budget
                .as_ref()
                .unwrap()
                .deadline()
                .saturating_duration_since(tokio::time::Instant::now()),
            CLOSE_ALLOWANCE
        );
        assert!(matches!(bounded(registration.acquire()).await,
            Err(error) if error.code == ErrorCode::Ebadf));
        assert!(bounded(generation.close()).await.is_err());
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(!lock(&generation.owned).budget_expired);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::time::advance(Duration::from_millis(2)).await;
        settle(|| lock(&generation.owned).budget_expired).await;
        assert_eq!(lock(&generation.owned).next_factory, 0);
        assert!(!lock(&generation.owned).pool_acknowledged);
        // Resume actual time before real SQLite/provider close work. Virtual
        // time here qualifies only the deterministic budget state transitions.
        tokio::time::resume();
        drop(lease);
        settle(|| lock(&generation.owned).phase == GenerationPhase::Complete).await;
        assert!(lock(&generation.owned).pool_acknowledged);
        assert!(!lock(&generation.owned).listener_acknowledged);
        assert!(bounded(scope.close_context()).await.is_err());
        assert!(!scope.context_closed());
        assert!(matches!(scope.new_generation(1), Err(error) if error.code == ErrorCode::Ebusy));
        assert!(keeper.occupied());
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert!(weak_marker.upgrade().is_some());
        assert!(directory.exists());
        // The shared context, pool/factory ownership scaffolding, and unproven
        // controlled listener remain retained. The activated filesystem has an
        // actual pool close ACK; no context/listener ACK or replacement follows.
    }

    #[tokio::test]
    async fn lazy_target_keeper_poison_refuses_terminal_ack_and_retains_context() {
        let keeper = Arc::new(TargetRuntimeKeeper::default());
        let scope = keeper.reserve().unwrap();
        let context = StorageContext::new(16).unwrap();
        scope.install_context(context.clone()).unwrap();
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _keeper_guard = keeper.0.lock().unwrap();
            panic!("poison only target runtime keeper");
        }));
        assert!(poisoned.is_err());
        assert!(keeper.0.is_poisoned());
        assert!(!scope.0.owned.is_poisoned());
        let closed = bounded(scope.close_context()).await;
        assert!(closed.is_err_and(|error| error.code == ErrorCode::Eio));
        assert!(keeper.occupied());
        assert!(!scope.context_closed());
        assert!(context_usable(&context).await);
        assert!(matches!(keeper.reserve(), Err(error) if error.code == ErrorCode::Ebusy));
        let snapshot = scope.snapshot().unwrap();
        assert_eq!(snapshot["unproven"], true);
        assert_eq!(snapshot["complete"], false);
        assert!(!scope.0.owned.is_poisoned());
        // Failed ownership stays rooted through the retained keeper/worker
        // cycle; no generation or provider close ACK was invented.
    }

    #[tokio::test]
    async fn lazy_target_late_context_handoff_is_rerooted_and_cannot_reuse_cached_success() {
        let keeper = Arc::new(TargetRuntimeKeeper::default());
        let scope = keeper.reserve().unwrap();
        let first = StorageContext::new(16).unwrap();
        scope.install_context(first.clone()).unwrap();
        bounded(scope.close_context()).await.unwrap();
        assert!(scope.context_closed());
        assert!(!keeper.occupied());
        assert!(!context_usable(&first).await);
        let late = StorageContext::new(16).unwrap();
        assert!(matches!(scope.install_context(late.clone()),
            Err(error) if error.code == ErrorCode::Ebusy));
        assert!(!scope.context_closed());
        assert!(keeper.occupied());
        assert!(context_usable(&late).await);
        assert!(bounded(scope.close_context()).await.is_err());
        assert!(matches!(keeper.reserve(), Err(error) if error.code == ErrorCode::Ebusy));
        assert_eq!(scope.snapshot().unwrap()["unproven"], true);
    }
}
