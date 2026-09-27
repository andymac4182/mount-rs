//! Bounded, owned Drive activation and persistent runtime eviction.
//!
//! A registration is cheap and does not open storage. Every uncertain lifecycle
//! outcome retains its resident charge; a caller timeout never replaces an owner.

use std::ops::{Deref, DerefMut};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use futures_util::FutureExt;
use mount_rs_core::storage::ConcurrentBackingId;
use mount_rs_core::{ErrorCode, FileHandle, FsDriver, FsError, Result, Stats};
use tokio::sync::{Notify, RwLock, watch};

use crate::runtime_diagnostics::{
    RuntimeCompleted, RuntimeDiagnostics, RuntimeDiagnosticsSnapshot, RuntimeOutcome, RuntimeSpan,
    RuntimeStage,
};

/// Immutable construction plan for one registered Drive.
///
/// On an error or panic, construction must retain ownership of partially opened
/// resources and any uncertain cleanup independently of the dropped open future.
/// Cleanup must not be canceled with a waiter. The pool retains this factory and
/// a nonretryable charge, but a plain `FsError` cannot transfer partial resources
/// to the pool. Factories must not assume an error permits another open attempt.
#[async_trait]
pub trait RuntimeFactory: Send + Sync {
    async fn open(&self) -> Result<Arc<dyn ManagedDrive>>;
}

/// The actual storage lifecycle owner, rather than a forwarding driver.
///
/// Synchronous observation callbacks run under the pool's global mutex. They
/// must be bounded and nonblocking, must not re-enter the pool, and must allocate
/// nothing when used by callers requiring zero added hot-path allocations. They
/// must report the immutable opened owner; safe eviction initially requires a
/// healthy persistent MRC5 owner with a fixed nonempty backing identity.
#[async_trait]
pub trait ManagedDrive: Send + Sync {
    fn driver(&self) -> Arc<dyn FsDriver>;
    fn failed(&self) -> bool;
    /// Qualification for safe persistent reopen. Volatile owners remain resident.
    fn eviction_allowed(&self) -> bool {
        false
    }
    /// The identity observed by this opened owner, not a fresh authority check.
    fn concurrent_backing_id(&self) -> Option<ConcurrentBackingId> {
        None
    }
    async fn shutdown(&self) -> Result<()>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct RuntimePoolSnapshot {
    pub registered: usize,
    pub resident: usize,
    pub opening: usize,
    pub ready: usize,
    pub closing: usize,
    pub quarantined: usize,
    pub pinned: usize,
    pub open_success: usize,
    pub open_error: usize,
    pub eviction_success: usize,
    pub eviction_error: usize,
    pub waits: usize,
    pub capacity_rejections: usize,
}

struct Completion(watch::Sender<Option<Result<()>>>);
impl Completion {
    fn new() -> Arc<Self> {
        Arc::new(Self(watch::channel(None).0))
    }
    fn subscribe(&self) -> watch::Receiver<Option<Result<()>>> {
        self.0.subscribe()
    }
    fn finish(&self, result: Result<()>) {
        self.0.send_if_modified(|value| {
            if value.is_some() {
                false
            } else {
                *value = Some(result);
                true
            }
        });
    }
}

async fn wait_completion(mut receiver: watch::Receiver<Option<Result<()>>>) -> Result<()> {
    loop {
        if let Some(result) = receiver.borrow().clone() {
            return result;
        }
        receiver.changed().await.map_err(|_| uncertain())?;
    }
}

fn uncertain() -> FsError {
    FsError::new(ErrorCode::Eio)
}
fn busy() -> FsError {
    FsError::new(ErrorCode::Ebusy)
}
fn terminal() -> FsError {
    FsError::new(ErrorCode::Ebadf)
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // User observation panics are caught before they can unwind through this
    // mutex. Pin exhaustion quarantines the slot before its deliberate panic,
    // without changing the pin count; recovery must allow outstanding leases to
    // release their pins and the owned drain to observe that tombstone. State
    // transitions otherwise preserve charge/owner invariants before publication.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Owner {
    runtime: Arc<dyn ManagedDrive>,
    // A panicking driver accessor still leaves the actual lifecycle owner held.
    driver: Option<Arc<dyn FsDriver>>,
}
impl Owner {
    fn healthy(&self) -> Result<()> {
        match catch_unwind(AssertUnwindSafe(|| self.runtime.failed())) {
            Ok(false) => Ok(()),
            _ => Err(uncertain()),
        }
    }
    fn qualified(&self) -> Result<bool> {
        catch_unwind(AssertUnwindSafe(|| self.runtime.eviction_allowed())).map_err(|_| uncertain())
    }
    fn backing_matches(&self, expected: ConcurrentBackingId) -> Result<()> {
        match catch_unwind(AssertUnwindSafe(|| self.runtime.concurrent_backing_id())) {
            Ok(Some(actual)) if actual == expected => Ok(()),
            _ => Err(uncertain()),
        }
    }
}

enum Phase {
    Dormant,
    // An uncharged ticket waiting for a particular victim's charge.
    Admitting(Arc<Completion>),
    Opening(Arc<Completion>),
    Ready {
        owner: Arc<Owner>,
        pins: usize,
        used: u64,
    },
    Closing {
        owner: Arc<Owner>,
        done: Arc<Completion>,
    },
    Quarantined {
        owner: Option<Arc<Owner>>,
        pins: usize,
        error: FsError,
    },
}

struct Slot {
    factory: Arc<dyn RuntimeFactory>,
    phase: Phase,
    // Outer None means never successfully bound; an observed None is also fixed.
    binding: Option<Option<ConcurrentBackingId>>,
    retained_handles: Vec<Arc<dyn FileHandle>>,
}
struct State {
    slots: Vec<Slot>,
    resident: usize,
    clock: u64,
    stopped: bool,
    shutdown: Option<Arc<Completion>>,
    counters: RuntimePoolSnapshot,
}
struct Inner {
    capacity: usize,
    diagnostics: RuntimeDiagnostics,
    state: Mutex<State>,
    changed: Notify,
}

// The actual guard is first so unwinding also unlocks before timing fields drop.
// Neither deferred span publishes while the state mutex is held.
struct PoolStateGuard<'a> {
    guard: Option<MutexGuard<'a, State>>,
    wait: RuntimeCompleted,
    hold: RuntimeSpan,
}
fn lock_state(inner: &Inner) -> PoolStateGuard<'_> {
    let mut wait =
        RuntimeSpan::new_deferred(Some(&inner.diagnostics), RuntimeStage::StateMutexWait);
    let guard = lock(&inner.state);
    let wait = wait.detach(RuntimeOutcome::Success);
    let hold = RuntimeSpan::new_deferred(Some(&inner.diagnostics), RuntimeStage::StateMutexHold);
    PoolStateGuard {
        guard: Some(guard),
        wait,
        hold,
    }
}
impl Deref for PoolStateGuard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        self.guard.as_ref().expect("owned pool state guard")
    }
}
impl DerefMut for PoolStateGuard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        self.guard.as_mut().expect("owned pool state guard")
    }
}
impl Drop for PoolStateGuard<'_> {
    fn drop(&mut self) {
        let outcome = if std::thread::panicking() {
            RuntimeOutcome::Error
        } else {
            RuntimeOutcome::Success
        };
        let mut hold = self.hold.detach(outcome);
        drop(self.guard.take());
        self.wait.publish();
        hold.publish();
    }
}

/// One resident budget shared by opening, ready, closing and quarantined owners.
#[derive(Clone)]
pub struct RuntimePool {
    inner: Arc<Inner>,
}
/// An immutable slot. Cloning a registration never activates storage.
#[derive(Clone)]
pub struct DriveRegistration {
    inner: Arc<Inner>,
    id: usize,
}
/// A pin on the exact opened lifecycle owner.
pub struct RuntimeLease {
    inner: Arc<Inner>,
    id: usize,
    owner: Arc<Owner>,
    waited: bool,
}

impl RuntimePool {
    pub fn new(capacity: usize) -> Result<Self> {
        Self::with_diagnostics(capacity, RuntimeDiagnostics::default())
    }

    /// Uses an explicit observer constructed before request handling.
    pub fn with_diagnostics(capacity: usize, diagnostics: RuntimeDiagnostics) -> Result<Self> {
        if capacity == 0 {
            return Err(FsError::new(ErrorCode::Einval));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                capacity,
                diagnostics,
                state: Mutex::new(State {
                    slots: Vec::new(),
                    resident: 0,
                    clock: 0,
                    stopped: false,
                    shutdown: None,
                    counters: RuntimePoolSnapshot::default(),
                }),
                changed: Notify::new(),
            }),
        })
    }

    pub fn diagnostics(&self) -> Option<RuntimeDiagnosticsSnapshot> {
        self.inner.diagnostics.snapshot()
    }

    pub fn register(&self, factory: Arc<dyn RuntimeFactory>) -> Result<DriveRegistration> {
        let mut state = lock_state(&self.inner);
        if state.stopped {
            return Err(terminal());
        }
        let id = state.slots.len();
        state.slots.push(Slot {
            factory,
            phase: Phase::Dormant,
            binding: None,
            retained_handles: Vec::new(),
        });
        Ok(DriveRegistration {
            inner: self.inner.clone(),
            id,
        })
    }

    pub fn snapshot(&self) -> RuntimePoolSnapshot {
        let state = lock_state(&self.inner);
        let mut snapshot = state.counters;
        snapshot.registered = state.slots.len();
        snapshot.resident = state.resident;
        for slot in &state.slots {
            match &slot.phase {
                Phase::Opening(_) => snapshot.opening += 1,
                Phase::Ready { pins, .. } => {
                    snapshot.ready += 1;
                    snapshot.pinned = snapshot.pinned.saturating_add(*pins);
                }
                Phase::Closing { .. } => snapshot.closing += 1,
                Phase::Quarantined { pins, .. } => {
                    snapshot.quarantined += 1;
                    snapshot.pinned = snapshot.pinned.saturating_add(*pins);
                }
                Phase::Dormant | Phase::Admitting(_) => {}
            }
        }
        snapshot
    }

    /// Stops admission synchronously when polled and joins one owned drain.
    /// An error requires retaining this pool and prohibits a replacement owner.
    pub async fn shutdown(&self) -> Result<()> {
        let (completion, start) = {
            let mut state = lock_state(&self.inner);
            if let Some(completion) = &state.shutdown {
                (completion.clone(), false)
            } else {
                let completion = Completion::new();
                state.stopped = true;
                state.shutdown = Some(completion.clone());
                (completion, true)
            }
        };
        if start {
            let guard = DrainGuard {
                span: RuntimeSpan::new(Some(&self.inner.diagnostics), RuntimeStage::TerminalDrain),
                inner: self.inner.clone(),
                completion: completion.clone(),
                complete: false,
            };
            spawn_owned(async move { drain(guard).await });
        }
        wait_completion(completion.subscribe()).await
    }
}

enum Acquisition {
    Lease(RuntimeLease),
    Wait(watch::Receiver<Option<Result<()>>>),
    Open {
        done: Arc<Completion>,
        factory: Arc<dyn RuntimeFactory>,
    },
    Evict {
        victim: usize,
        owner: Arc<Owner>,
        backing: ConcurrentBackingId,
        done: Arc<Completion>,
        ticket: Arc<Completion>,
    },
}

impl DriveRegistration {
    pub async fn acquire(&self) -> Result<RuntimeLease> {
        let mut span = RuntimeSpan::new(Some(&self.inner.diagnostics), RuntimeStage::Acquire);
        let result = self.acquire_inner().await;
        span.finish(RuntimeOutcome::result(&result));
        result
    }

    async fn acquire_inner(&self) -> Result<RuntimeLease> {
        let mut waited = false;
        loop {
            let action = begin_acquire(&self.inner, self.id, waited)?;
            let receiver = match action {
                Acquisition::Lease(lease) => return Ok(lease),
                Acquisition::Wait(receiver) => receiver,
                Acquisition::Open { done, factory } => {
                    let receiver = done.subscribe();
                    start_open(self.inner.clone(), self.id, done, factory);
                    receiver
                }
                Acquisition::Evict {
                    victim,
                    owner,
                    backing,
                    done,
                    ticket,
                } => {
                    let receiver = ticket.subscribe();
                    start_close(
                        self.inner.clone(),
                        victim,
                        owner,
                        done,
                        CloseMode::Evict {
                            target: self.id,
                            ticket,
                            backing,
                        },
                    );
                    receiver
                }
            };
            waited = true;
            let mut span =
                RuntimeSpan::new(Some(&self.inner.diagnostics), RuntimeStage::ActivationWait);
            let result = wait_completion(receiver).await;
            span.finish(RuntimeOutcome::result(&result));
            result?;
        }
    }
}

fn begin_acquire(inner: &Arc<Inner>, id: usize, waited: bool) -> Result<Acquisition> {
    let mut state = lock_state(inner);
    if state.stopped {
        return Err(terminal());
    }
    let action = match &state.slots[id].phase {
        Phase::Ready { owner, pins, .. } => {
            let owner = owner.clone();
            if let Err(error) = owner.healthy() {
                quarantine_slot(&mut state.slots[id], error.clone());
                return Err(error);
            }
            let Some(next) = pins.checked_add(1) else {
                let error = FsError::new(ErrorCode::Eoverflow);
                quarantine_slot(&mut state.slots[id], error.clone());
                return Err(error);
            };
            state.clock = state.clock.saturating_add(1);
            let used = state.clock;
            if let Phase::Ready {
                pins, used: last, ..
            } = &mut state.slots[id].phase
            {
                *pins = next;
                *last = used;
            }
            return Ok(Acquisition::Lease(RuntimeLease {
                inner: inner.clone(),
                id,
                owner,
                waited,
            }));
        }
        Phase::Admitting(done) | Phase::Opening(done) | Phase::Closing { done, .. } => {
            Acquisition::Wait(done.subscribe())
        }
        Phase::Quarantined { error, .. } => return Err(error.clone()),
        Phase::Dormant if state.resident < inner.capacity => {
            let done = Completion::new();
            state.resident += 1;
            state.slots[id].phase = Phase::Opening(done.clone());
            Acquisition::Open {
                done,
                factory: state.slots[id].factory.clone(),
            }
        }
        Phase::Dormant => {
            let mut candidate: Option<(usize, u64, Arc<Owner>, ConcurrentBackingId)> = None;
            for index in 0..state.slots.len() {
                let Phase::Ready {
                    owner,
                    pins: 0,
                    used,
                } = &state.slots[index].phase
                else {
                    continue;
                };
                let owner = owner.clone();
                let used = *used;
                let Some(Some(backing)) = state.slots[index].binding else {
                    continue;
                };
                let eligible = owner
                    .healthy()
                    .and_then(|_| owner.backing_matches(backing))
                    .and_then(|_| owner.qualified());
                match eligible {
                    Err(error) => quarantine_slot(&mut state.slots[index], error),
                    Ok(true)
                        if candidate
                            .as_ref()
                            .is_none_or(|(_, previous, _, _)| used < *previous) =>
                    {
                        candidate = Some((index, used, owner, backing));
                    }
                    Ok(_) => {}
                }
            }
            let Some((victim, _, owner, backing)) = candidate else {
                state.counters.capacity_rejections =
                    state.counters.capacity_rejections.saturating_add(1);
                return Err(busy());
            };
            let ticket = Completion::new();
            let done = Completion::new();
            state.slots[id].phase = Phase::Admitting(ticket.clone());
            state.slots[victim].phase = Phase::Closing {
                owner: owner.clone(),
                done: done.clone(),
            };
            Acquisition::Evict {
                victim,
                owner,
                backing,
                done,
                ticket,
            }
        }
    };
    if !waited {
        state.counters.waits = state.counters.waits.saturating_add(1);
    }
    Ok(action)
}

fn quarantine_slot(slot: &mut Slot, error: FsError) {
    let (owner, pins) = match &slot.phase {
        Phase::Ready { owner, pins, .. } => (Some(owner.clone()), *pins),
        Phase::Closing { owner, .. } => (Some(owner.clone()), 0),
        Phase::Quarantined { .. } => return,
        Phase::Opening(_) => (None, 0),
        Phase::Dormant | Phase::Admitting(_) => return,
    };
    slot.phase = Phase::Quarantined { owner, pins, error };
}

impl RuntimeLease {
    /// Borrowed driver access must remain within this lease's lifetime.
    pub fn driver(&self) -> &Arc<dyn FsDriver> {
        self.owner
            .driver
            .as_ref()
            .expect("validated runtime driver")
    }
    pub fn waited(&self) -> bool {
        self.waited
    }
    pub fn quarantine(&self) {
        let mut state = lock_state(&self.inner);
        quarantine_slot(&mut state.slots[self.id], uncertain());
        drop(state);
        self.inner.changed.notify_waiters();
    }
    pub(crate) fn healthy(&self) -> Result<()> {
        let mut state = lock_state(&self.inner);
        let slot = &mut state.slots[self.id];
        match &slot.phase {
            Phase::Quarantined { error, .. } => return Err(error.clone()),
            Phase::Ready { owner, .. } if Arc::ptr_eq(owner, &self.owner) => {}
            _ => return Err(terminal()),
        }
        if let Err(error) = self.owner.healthy() {
            quarantine_slot(slot, error.clone());
            drop(state);
            self.inner.changed.notify_waiters();
            return Err(error);
        }
        Ok(())
    }
    fn retain_failed_handle(&self, actual: Arc<dyn FileHandle>) {
        let mut state = lock_state(&self.inner);
        let slot = &mut state.slots[self.id];
        quarantine_slot(slot, uncertain());
        slot.retained_handles.push(actual);
        drop(state);
        self.inner.changed.notify_waiters();
    }
    /// Adopts a returned backend handle synchronously, before session insertion.
    pub fn wrap_handle(&self, actual: Arc<dyn FileHandle>) -> Arc<RuntimeHandle> {
        RuntimeHandle::new(actual, Some(self.clone()))
    }
    pub(crate) fn mutation_guard(&self) -> MutationGuard<'_> {
        MutationGuard {
            lease: self,
            complete: false,
        }
    }
}

impl Clone for RuntimeLease {
    fn clone(&self) -> Self {
        let mut state = lock_state(&self.inner);
        let slot = &mut state.slots[self.id];
        let pins = match &mut slot.phase {
            Phase::Ready { owner, pins, .. } if Arc::ptr_eq(owner, &self.owner) => pins,
            Phase::Quarantined {
                owner: Some(owner),
                pins,
                ..
            } if Arc::ptr_eq(owner, &self.owner) => pins,
            _ => panic!("runtime lease lost its owner"),
        };
        if let Some(next) = pins.checked_add(1) {
            *pins = next;
        } else {
            quarantine_slot(slot, FsError::new(ErrorCode::Eoverflow));
            panic!("runtime pin count exhausted");
        }
        Self {
            inner: self.inner.clone(),
            id: self.id,
            owner: self.owner.clone(),
            waited: self.waited,
        }
    }
}
impl Drop for RuntimeLease {
    fn drop(&mut self) {
        let mut state = lock_state(&self.inner);
        let pins = match &mut state.slots[self.id].phase {
            Phase::Ready { owner, pins, .. } if Arc::ptr_eq(owner, &self.owner) => Some(pins),
            Phase::Quarantined {
                owner: Some(owner),
                pins,
                ..
            } if Arc::ptr_eq(owner, &self.owner) => Some(pins),
            _ => None,
        };
        if let Some(pins) = pins {
            if let Some(next) = pins.checked_sub(1) {
                *pins = next;
            } else {
                quarantine_slot(&mut state.slots[self.id], uncertain());
            }
        }
        drop(state);
        self.inner.changed.notify_waiters();
    }
}

pub(crate) struct MutationGuard<'a> {
    lease: &'a RuntimeLease,
    complete: bool,
}
impl MutationGuard<'_> {
    pub(crate) fn complete(&mut self) {
        self.complete = true;
    }
}
impl Drop for MutationGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.lease.quarantine();
        }
    }
}

enum CloseMode {
    Evict {
        target: usize,
        ticket: Arc<Completion>,
        backing: ConcurrentBackingId,
    },
    Terminal,
}
enum TaskKind {
    Open,
    Close(CloseMode),
}
struct TaskGuard {
    inner: Arc<Inner>,
    id: usize,
    owner: Option<Arc<Owner>>,
    done: Arc<Completion>,
    kind: TaskKind,
    complete: bool,
    span: RuntimeSpan,
}
impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.complete {
            match &self.kind {
                TaskKind::Open => {
                    let owner = self.owner.clone();
                    finish_open(self, owner, None, Err(uncertain()), true);
                }
                TaskKind::Close(mode) => finish_close(
                    &self.inner,
                    self.id,
                    Err(uncertain()),
                    &self.done,
                    mode,
                    &mut self.span,
                    true,
                ),
            }
        }
    }
}

fn spawn_owned(future: impl std::future::Future<Output = ()> + Send + 'static) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(future);
    }
    // Without a runtime the future is dropped and its guard records uncertainty.
}

fn start_open(
    inner: Arc<Inner>,
    id: usize,
    done: Arc<Completion>,
    factory: Arc<dyn RuntimeFactory>,
) {
    let mut guard = TaskGuard {
        span: RuntimeSpan::new(Some(&inner.diagnostics), RuntimeStage::Open),
        inner,
        id,
        owner: None,
        done,
        kind: TaskKind::Open,
        complete: false,
    };
    spawn_owned(async move {
        let opened = AssertUnwindSafe(factory.open()).catch_unwind().await;
        let (owner, backing, result) = match opened {
            Ok(Ok(runtime)) => {
                let observations = catch_unwind(AssertUnwindSafe(|| {
                    (
                        runtime.driver(),
                        runtime.concurrent_backing_id(),
                        runtime.failed(),
                    )
                }));
                match observations {
                    Ok((driver, identity, false)) => (
                        Some(Arc::new(Owner {
                            runtime,
                            driver: Some(driver),
                        })),
                        Some(identity),
                        Ok(()),
                    ),
                    Ok((driver, _, true)) => (
                        Some(Arc::new(Owner {
                            runtime,
                            driver: Some(driver),
                        })),
                        None,
                        Err(uncertain()),
                    ),
                    Err(_) => (
                        Some(Arc::new(Owner {
                            runtime,
                            driver: None,
                        })),
                        None,
                        Err(uncertain()),
                    ),
                }
            }
            Ok(Err(error)) => (None, None, Err(error)),
            Err(_) => (None, None, Err(uncertain())),
        };
        guard.owner = owner.clone();
        finish_open(&mut guard, owner, backing, result, false);
        guard.complete = true;
        drop(guard);
    });
}

fn finish_open(
    guard: &mut TaskGuard,
    owner: Option<Arc<Owner>>,
    backing: Option<Option<ConcurrentBackingId>>,
    mut result: Result<()>,
    interrupted: bool,
) {
    let inner = &guard.inner;
    let id = guard.id;
    let done = &guard.done;
    let span = &mut guard.span;
    let mut state = lock_state(inner);
    if !matches!(&state.slots[id].phase, Phase::Opening(current) if Arc::ptr_eq(current, done)) {
        drop(state);
        span.finish(if interrupted {
            RuntimeOutcome::Cancelled
        } else {
            RuntimeOutcome::Error
        });
        done.finish(Err(uncertain()));
        return;
    }
    if result.is_ok()
        && state.slots[id]
            .binding
            .is_some_and(|binding| Some(binding) != backing)
    {
        result = Err(uncertain());
    }
    match &result {
        Ok(()) => {
            state.slots[id].binding = backing;
            state.clock = state.clock.saturating_add(1);
            state.slots[id].phase = Phase::Ready {
                owner: owner.expect("successful open owns a runtime"),
                pins: 0,
                used: state.clock,
            };
            state.counters.open_success = state.counters.open_success.saturating_add(1);
        }
        Err(error) => {
            state.slots[id].phase = Phase::Quarantined {
                owner,
                pins: 0,
                error: error.clone(),
            };
            state.counters.open_error = state.counters.open_error.saturating_add(1);
        }
    }
    drop(state);
    span.finish(if interrupted {
        RuntimeOutcome::Cancelled
    } else {
        RuntimeOutcome::result(&result)
    });
    done.finish(result);
    inner.changed.notify_waiters();
}

fn start_close(
    inner: Arc<Inner>,
    id: usize,
    owner: Arc<Owner>,
    done: Arc<Completion>,
    mode: CloseMode,
) {
    let mut guard = TaskGuard {
        span: if matches!(&mode, CloseMode::Evict { .. }) {
            RuntimeSpan::new(Some(&inner.diagnostics), RuntimeStage::EvictionShutdown)
        } else {
            RuntimeSpan::default()
        },
        inner,
        id,
        owner: Some(owner.clone()),
        done,
        kind: TaskKind::Close(mode),
        complete: false,
    };
    spawn_owned(async move {
        let mut result = owner.healthy();
        if result.is_ok()
            && let TaskKind::Close(CloseMode::Evict { backing, .. }) = &guard.kind
        {
            result = owner
                .backing_matches(*backing)
                .and_then(|_| owner.qualified())
                .and_then(|eligible| if eligible { Ok(()) } else { Err(uncertain()) });
        }
        if result.is_ok() {
            result = match AssertUnwindSafe(owner.runtime.shutdown())
                .catch_unwind()
                .await
            {
                Ok(result) => result,
                Err(_) => Err(uncertain()),
            };
        }
        if result.is_ok() {
            result = owner.healthy();
        }
        if result.is_ok()
            && let TaskKind::Close(CloseMode::Evict { backing, .. }) = &guard.kind
        {
            result = owner
                .backing_matches(*backing)
                .and_then(|_| owner.qualified())
                .and_then(|eligible| if eligible { Ok(()) } else { Err(uncertain()) });
        }
        if let TaskKind::Close(mode) = &guard.kind {
            finish_close(
                &guard.inner,
                guard.id,
                result,
                &guard.done,
                mode,
                &mut guard.span,
                false,
            );
        }
        guard.complete = true;
        drop(guard);
    });
}

fn finish_close(
    inner: &Arc<Inner>,
    id: usize,
    mut result: Result<()>,
    done: &Arc<Completion>,
    mode: &CloseMode,
    span: &mut RuntimeSpan,
    interrupted: bool,
) {
    let mut state = lock_state(inner);
    let owner = match &state.slots[id].phase {
        Phase::Closing {
            owner,
            done: current,
        } if Arc::ptr_eq(current, done) => Some(owner.clone()),
        _ => None,
    };
    let Some(owner) = owner else {
        // A drop guard must not overwrite a transition already handed off.
        drop(state);
        span.finish(if interrupted {
            RuntimeOutcome::Cancelled
        } else {
            RuntimeOutcome::Error
        });
        done.finish(Err(uncertain()));
        if let CloseMode::Evict { ticket, .. } = mode {
            ticket.finish(Err(uncertain()));
        }
        inner.changed.notify_waiters();
        return;
    };
    if result.is_ok()
        && let CloseMode::Evict { backing, .. } = mode
        && state.slots[id].binding != Some(Some(*backing))
    {
        result = Err(uncertain());
    }
    let mut launch = None;
    let mut ticket_result = None;
    match &result {
        Ok(()) => {
            state.slots[id].phase = Phase::Dormant;
            match mode {
                CloseMode::Evict { target, ticket, .. } => {
                    state.counters.eviction_success =
                        state.counters.eviction_success.saturating_add(1);
                    if state.stopped {
                        state.resident -= 1;
                        state.slots[*target].phase = Phase::Dormant;
                        ticket_result = Some((ticket, Err(terminal())));
                    } else {
                        state.slots[*target].phase = Phase::Opening(ticket.clone());
                        launch = Some((
                            *target,
                            ticket.clone(),
                            state.slots[*target].factory.clone(),
                        ));
                    }
                }
                CloseMode::Terminal => state.resident -= 1,
            }
        }
        Err(error) => {
            state.slots[id].phase = Phase::Quarantined {
                owner: Some(owner),
                pins: 0,
                error: error.clone(),
            };
            if let CloseMode::Evict { target, ticket, .. } = mode {
                state.counters.eviction_error = state.counters.eviction_error.saturating_add(1);
                state.slots[*target].phase = Phase::Dormant;
                ticket_result = Some((ticket, result.clone()));
            }
        }
    }
    drop(state);
    span.finish(if interrupted {
        RuntimeOutcome::Cancelled
    } else {
        RuntimeOutcome::result(&result)
    });
    done.finish(result);
    if let Some((ticket, result)) = ticket_result {
        ticket.finish(result);
    }
    inner.changed.notify_waiters();
    if let Some((target, ticket, factory)) = launch {
        start_open(inner.clone(), target, ticket, factory);
    }
}

struct DrainGuard {
    inner: Arc<Inner>,
    completion: Arc<Completion>,
    complete: bool,
    span: RuntimeSpan,
}
impl Drop for DrainGuard {
    fn drop(&mut self) {
        if !self.complete {
            let mut state = lock_state(&self.inner);
            for slot in &mut state.slots {
                if matches!(&slot.phase, Phase::Ready { .. }) {
                    quarantine_slot(slot, uncertain());
                }
            }
            drop(state);
            self.span.finish(RuntimeOutcome::Cancelled);
            self.completion.finish(Err(uncertain()));
            self.inner.changed.notify_waiters();
        }
    }
}

async fn drain(mut guard: DrainGuard) {
    loop {
        let changed = guard.inner.changed.notified();
        let (closes, pending, failure) = {
            let mut state = lock_state(&guard.inner);
            let mut closes = Vec::new();
            let mut pending = false;
            let mut failure = None;
            for id in 0..state.slots.len() {
                match &state.slots[id].phase {
                    Phase::Ready { owner, pins, .. } => {
                        let owner = owner.clone();
                        if let Err(error) = owner.healthy() {
                            quarantine_slot(&mut state.slots[id], error.clone());
                            failure.get_or_insert(error);
                        } else if *pins == 0 {
                            let done = Completion::new();
                            state.slots[id].phase = Phase::Closing {
                                owner: owner.clone(),
                                done: done.clone(),
                            };
                            closes.push((id, owner, done));
                            pending = true;
                        } else {
                            pending = true;
                        }
                    }
                    Phase::Opening(_) | Phase::Closing { .. } | Phase::Admitting(_) => {
                        pending = true
                    }
                    Phase::Quarantined { error, .. } => {
                        failure.get_or_insert(error.clone());
                    }
                    Phase::Dormant => {}
                }
            }
            (closes, pending, failure)
        };
        for (id, owner, done) in closes {
            start_close(guard.inner.clone(), id, owner, done, CloseMode::Terminal);
        }
        if !pending {
            let result = failure.map_or(Ok(()), Err);
            guard.span.finish(RuntimeOutcome::result(&result));
            guard.completion.finish(result);
            guard.complete = true;
            return;
        }
        changed.await;
    }
}

/// A pinned backend handle with inherent delegation: no extra async-trait box
/// is added to reads and writes. Close and Drop join one owned backend close.
pub struct RuntimeHandle {
    state: Arc<HandleOwner>,
}
struct HandleOwner {
    actual: Arc<dyn FileHandle>,
    lease: Option<RuntimeLease>,
    gate: RwLock<()>,
    close: Mutex<Option<Arc<Completion>>>,
}
impl RuntimeHandle {
    fn new(actual: Arc<dyn FileHandle>, lease: Option<RuntimeLease>) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(HandleOwner {
                actual,
                lease,
                gate: RwLock::new(()),
                close: Mutex::new(None),
            }),
        })
    }
    pub(crate) fn unmanaged(actual: Arc<dyn FileHandle>) -> Arc<Self> {
        Self::new(actual, None)
    }
    fn open(&self) -> Result<()> {
        if lock(&self.state.close).is_some() {
            return Err(FsError::new(ErrorCode::Ebadf));
        }
        if let Some(lease) = &self.state.lease {
            lease.healthy()?;
        }
        Ok(())
    }
    pub fn fd(&self) -> Option<u64> {
        self.state.actual.fd()
    }
    pub async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let _gate = self.state.gate.read().await;
        self.open()?;
        self.state.actual.read(buffer, position).await
    }
    pub async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let _gate = self.state.gate.read().await;
        self.open()?;
        let mut mutation = self.state.lease.as_ref().map(RuntimeLease::mutation_guard);
        let result = self.state.actual.write(buffer, position).await;
        if let Some(mutation) = &mut mutation {
            mutation.complete();
        }
        result
    }
    pub async fn stat(&self) -> Result<Stats> {
        let _gate = self.state.gate.read().await;
        self.open()?;
        self.state.actual.stat().await
    }
    pub async fn truncate(&self, length: u64) -> Result<()> {
        let _gate = self.state.gate.read().await;
        self.open()?;
        let mut mutation = self.state.lease.as_ref().map(RuntimeLease::mutation_guard);
        let result = self.state.actual.truncate(length).await;
        if let Some(mutation) = &mut mutation {
            mutation.complete();
        }
        result
    }
    pub async fn sync(&self) -> Result<()> {
        let _gate = self.state.gate.read().await;
        self.open()?;
        let mut mutation = self.state.lease.as_ref().map(RuntimeLease::mutation_guard);
        let result = self.state.actual.sync().await;
        if let Some(mutation) = &mut mutation {
            mutation.complete();
        }
        result
    }
    pub async fn datasync(&self) -> Result<()> {
        let _gate = self.state.gate.read().await;
        self.open()?;
        let mut mutation = self.state.lease.as_ref().map(RuntimeLease::mutation_guard);
        let result = self.state.actual.datasync().await;
        if let Some(mutation) = &mut mutation {
            mutation.complete();
        }
        result
    }
    pub async fn close(&self) -> Result<()> {
        wait_completion(self.close_completion()).await
    }
    pub(crate) fn close_completion(&self) -> watch::Receiver<Option<Result<()>>> {
        self.state.start_close().subscribe()
    }
}
impl Drop for RuntimeHandle {
    fn drop(&mut self) {
        self.state.start_close();
    }
}

impl HandleOwner {
    fn start_close(self: &Arc<Self>) -> Arc<Completion> {
        let mut close = lock(&self.close);
        if let Some(completion) = &*close {
            return completion.clone();
        }
        let completion = Completion::new();
        *close = Some(completion.clone());
        drop(close);
        let mut guard = HandleCloseGuard {
            span: RuntimeSpan::new(
                self.lease.as_ref().map(|lease| &lease.inner.diagnostics),
                RuntimeStage::HandleClose,
            ),
            owner: Some(self.clone()),
            done: completion.clone(),
            complete: false,
        };
        spawn_owned(async move {
            let result = {
                let owner = guard.owner.as_ref().expect("owned handle close");
                let _gate = owner.gate.write().await;
                match AssertUnwindSafe(owner.actual.close()).catch_unwind().await {
                    Ok(result) => result,
                    Err(_) => Err(uncertain()),
                }
            };
            if result.is_err() {
                guard
                    .owner
                    .as_ref()
                    .expect("owned handle close")
                    .retain_failure();
            }
            guard.complete = true;
            // Release the actor's owner before waking a successful waiter. A
            // borrowed holder retains its own Arc and pin independently.
            drop(guard.owner.take());
            guard.span.finish(RuntimeOutcome::result(&result));
            guard.done.finish(result);
        });
        completion
    }
    fn retain_failure(&self) {
        if let Some(lease) = &self.lease {
            lease.retain_failed_handle(self.actual.clone());
        }
    }
}
struct HandleCloseGuard {
    owner: Option<Arc<HandleOwner>>,
    done: Arc<Completion>,
    complete: bool,
    span: RuntimeSpan,
}
impl Drop for HandleCloseGuard {
    fn drop(&mut self) {
        if !self.complete {
            if let Some(owner) = &self.owner {
                owner.retain_failure();
            }
            self.span.finish(RuntimeOutcome::Cancelled);
            self.done.finish(Err(uncertain()));
        }
    }
}
