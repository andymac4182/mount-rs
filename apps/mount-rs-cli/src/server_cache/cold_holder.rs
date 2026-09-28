//! Locally configured cache admission, independent of lazy filesystem owners.
//! Inspection owners are retained before polling and joined before context close.
use crate::runtime::DriverRuntimePlan;
use async_trait::async_trait;
use mount_rs_blob_cache::{
    CacheScope, IdentityEpoch, IntegrityPolicy, LocalCache, ScopeAdmission, ScopeIdentity,
    ScopeLease,
};
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::storage::InodeModeState;
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_sdk::StorageContext;
use mount_rs_service::catalog::CatalogStore;
use serde::Serialize;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

// This is the existing provider backing-verification bound, independently of
// the unchanged peer RPC, cache disk close and enclosing qualification bounds.
const INSPECTOR_DRAIN_BOUND: Duration = Duration::from_secs(20);
const MAX_ROUTES: usize = 10_000;

fn error(code: ErrorCode) -> FsError {
    FsError::new(code).with_syscall("configured cache holder")
}

#[async_trait]
trait Inspector: Send + Sync {
    async fn inspect(
        &self,
        plan: &DriverRuntimePlan,
        context: &StorageContext,
        observer: &dyn ConstructionObserver,
    ) -> Result<Option<InodeModeState>>;
}
struct SdkInspector;
#[async_trait]
impl Inspector for SdkInspector {
    async fn inspect(
        &self,
        plan: &DriverRuntimePlan,
        context: &StorageContext,
        observer: &dyn ConstructionObserver,
    ) -> Result<Option<InodeModeState>> {
        // Eligibility derives only from prepared literal TiDB + RustFS options.
        // No peer data selects a provider or supplies credentials.
        let options = plan
            .cold_cache_options()
            .ok_or_else(|| error(ErrorCode::Enotsup))?;
        context
            .inspect_compact_layout_with_construction_observer(
                &options.metadata,
                &options.blocks,
                observer,
            )
            .await
    }
}

#[derive(Default)]
struct JournalState {
    resources: Vec<Arc<dyn ConstructionResource>>,
    sealed: bool,
    uncertain: bool,
    failure: Option<FsError>,
}
#[derive(Default)]
struct InspectionJournal(Mutex<JournalState>);
impl InspectionJournal {
    fn lock(&self) -> MutexGuard<'_, JournalState> {
        match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.uncertain = true;
                state
            }
        }
    }
    async fn close(&self) -> Result<()> {
        let resources = {
            let mut state = self.lock();
            state.sealed = true;
            if let Some(failure) = &state.failure {
                return Err(failure.clone());
            }
            if state.uncertain {
                return Err(error(ErrorCode::Eio));
            }
            state.resources.clone()
        };
        // The selected SDK constructor registers one canonical provider group.
        // Close failures retain every group needed for reconciliation.
        for resource in resources {
            let result = resource.close().await;
            let mut state = self.lock();
            if let Err(failure) = result {
                state.failure = Some(failure.clone());
                return Err(failure);
            }
            if state.uncertain {
                return Err(error(ErrorCode::Eio));
            }
        }
        let released = {
            let mut state = self.lock();
            if state.uncertain {
                return Err(error(ErrorCode::Eio));
            }
            std::mem::take(&mut state.resources)
        };
        drop(released);
        Ok(())
    }
}
impl ConstructionObserver for InspectionJournal {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        let mut state = self.lock();
        if state.sealed {
            state.uncertain = true;
        }
        // retain is infallible: even a late owner remains physically retained.
        state.resources.push(resource);
    }
}

#[derive(Clone)]
struct InspectionOutcome {
    mode: Result<Option<InodeModeState>>,
    cleanup: Result<()>,
}
struct JoinState {
    task: Option<JoinHandle<InspectionOutcome>>,
    outcome: Option<Result<InspectionOutcome>>,
    uncertain: bool,
}
struct Signal(Arc<Notify>);
impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.0.notify_waiters();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_waiters();
    }
}
struct InspectionTicket {
    operation: u64,
    epoch: IdentityEpoch,
    journal: Arc<InspectionJournal>,
    joined: Mutex<JoinState>,
    changed: Arc<Notify>,
    wake: Waker,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
    #[cfg(test)]
    waiters: std::sync::atomic::AtomicUsize,
}
impl InspectionTicket {
    fn new(operation: u64, epoch: IdentityEpoch, permit: OwnedSemaphorePermit) -> Arc<Self> {
        let changed = Arc::new(Notify::new());
        Arc::new(Self {
            operation,
            epoch,
            journal: Arc::new(InspectionJournal::default()),
            joined: Mutex::new(JoinState {
                task: None,
                outcome: None,
                uncertain: false,
            }),
            wake: Waker::from(Arc::new(Signal(changed.clone()))),
            changed,
            permit: Mutex::new(Some(permit)),
            #[cfg(test)]
            waiters: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    fn lock(&self) -> MutexGuard<'_, JoinState> {
        match self.joined.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.uncertain = true;
                state
            }
        }
    }
    fn poll_join(&self) -> Poll<Result<InspectionOutcome>> {
        let mut state = self.lock();
        if let Some(outcome) = &state.outcome {
            return Poll::Ready(if state.uncertain {
                Err(error(ErrorCode::Eio))
            } else {
                outcome.clone()
            });
        }
        let Some(task) = &mut state.task else {
            return Poll::Ready(Err(error(ErrorCode::Eio)));
        };
        let joined = match Pin::new(task).poll(&mut Context::from_waker(&self.wake)) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(joined) => joined,
        };
        let outcome = if state.uncertain {
            Err(error(ErrorCode::Eio))
        } else {
            joined.map_err(|_| error(ErrorCode::Eio))
        };
        state.task = None;
        state.outcome = Some(outcome.clone());
        drop(state);
        self.changed.notify_waiters();
        Poll::Ready(outcome)
    }
    async fn join(&self) -> Result<InspectionOutcome> {
        #[cfg(test)]
        self.waiters.fetch_add(1, Ordering::AcqRel);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Poll::Ready(outcome) = self.poll_join() {
                return outcome;
            }
            changed.await;
        }
    }
    fn release_permit(&self) -> Result<()> {
        // Only call after the actual task join and provider-group cleanup.
        let mut permit = self.permit.lock().map_err(|_| error(ErrorCode::Eio))?;
        drop(permit.take());
        Ok(())
    }
}
struct Proof {
    scope: CacheScope,
    lease: ScopeLease,
}
#[derive(Default)]
struct RouteState {
    operation: u64,
    proof: Option<Proof>,
    ticket: Option<Arc<InspectionTicket>>,
    failure: Option<FsError>,
}
struct Route {
    identity: ScopeIdentity,
    definition: serde_json::Value,
    plan: Arc<DriverRuntimePlan>,
    policy: IntegrityPolicy,
    state: Mutex<RouteState>,
}
impl Route {
    fn lock(&self) -> MutexGuard<'_, RouteState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.failure.get_or_insert_with(|| error(ErrorCode::Eio));
                state
            }
        }
    }
}
#[derive(Default)]
struct RegistryState {
    sealed: bool,
    routes: BTreeMap<String, BTreeMap<String, Arc<Route>>>,
    count: usize,
    failure: Option<FsError>,
    drain_result: Option<Result<()>>,
    // At most max_inspections actual owners occupy permits. Completed tickets
    // can remain on configured routes until a canceled waiter retries.
    active: Vec<(Weak<Route>, Weak<InspectionTicket>)>,
}
#[derive(Serialize)]
pub(crate) struct HolderSnapshot {
    pub(crate) configured_routes: usize,
    pub(crate) retained_proof_groups: usize,
    pub(crate) admitted_proofs: usize,
    pub(crate) retained_inspectors: usize,
    pub(crate) sealed: bool,
    pub(crate) inspection_starts: u64,
    pub(crate) inspection_successes: u64,
    pub(crate) authority_refusals: u64,
    pub(crate) proof_reuses: u64,
}
// A deterministic terminal boundary for the actual shutdown controls. This
// owner and await are absent from every non-test build.
#[cfg(test)]
struct DrainPublicationGate {
    entered: Semaphore,
    release: Semaphore,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProofReleaseBoundary {
    Registry,
    Route,
}
#[cfg(test)]
struct ProofReleaseGate {
    boundary: ProofReleaseBoundary,
    entered: Semaphore,
    released: Mutex<bool>,
    changed: std::sync::Condvar,
}

/// This owner has no peer/ServerCache/DistributedRuntime reference. It owns only
/// prepared plans, provider context, catalog, local cache and bounded inspectors.
pub(crate) struct ConfiguredHolderRegistry {
    cluster: String,
    catalog: Arc<dyn CatalogStore>,
    context: StorageContext,
    local: Arc<LocalCache>,
    max_routes: usize,
    slots: Arc<Semaphore>,
    inspector: Arc<dyn Inspector>,
    state: Mutex<RegistryState>,
    drain_lock: tokio::sync::Mutex<()>,
    drain_bound: Duration,
    #[cfg(test)]
    drain_publication_gate: Mutex<Option<Arc<DrainPublicationGate>>>,
    #[cfg(test)]
    proof_release_gate: Mutex<Option<Arc<ProofReleaseGate>>>,
    starts: AtomicU64,
    successes: AtomicU64,
    refusals: Arc<AtomicU64>,
    reuses: AtomicU64,
}
struct HolderLimits {
    max_routes: usize,
    max_inspections: usize,
    drain_bound: Duration,
}

impl ConfiguredHolderRegistry {
    pub(crate) fn new(
        cluster: String,
        catalog: Arc<dyn CatalogStore>,
        context: StorageContext,
        local: Arc<LocalCache>,
        max_routes: usize,
        max_inspections: usize,
    ) -> Result<Arc<Self>> {
        Self::with_inspector(
            cluster,
            catalog,
            context,
            local,
            Arc::new(SdkInspector),
            HolderLimits {
                max_routes,
                max_inspections,
                drain_bound: INSPECTOR_DRAIN_BOUND,
            },
        )
    }
    fn with_inspector(
        cluster: String,
        catalog: Arc<dyn CatalogStore>,
        context: StorageContext,
        local: Arc<LocalCache>,
        inspector: Arc<dyn Inspector>,
        limits: HolderLimits,
    ) -> Result<Arc<Self>> {
        let HolderLimits {
            max_routes,
            max_inspections,
            drain_bound,
        } = limits;
        if cluster.is_empty()
            || cluster.len() > 256
            || max_routes > MAX_ROUTES
            || max_inspections == 0
            || max_inspections > 16
            || drain_bound.is_zero()
        {
            return Err(error(ErrorCode::Einval));
        }
        Ok(Arc::new(Self {
            cluster,
            catalog,
            context,
            local,
            max_routes,
            slots: Arc::new(Semaphore::new(max_inspections)),
            inspector,
            state: Mutex::new(RegistryState::default()),
            drain_lock: tokio::sync::Mutex::new(()),
            drain_bound,
            #[cfg(test)]
            drain_publication_gate: Mutex::new(None),
            #[cfg(test)]
            proof_release_gate: Mutex::new(None),
            starts: AtomicU64::new(0),
            successes: AtomicU64::new(0),
            refusals: Arc::new(AtomicU64::new(0)),
            reuses: AtomicU64::new(0),
        }))
    }
    fn lock(&self) -> MutexGuard<'_, RegistryState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.sealed = true;
                state.failure.get_or_insert_with(|| error(ErrorCode::Eio));
                state
            }
        }
    }
    fn require_open(&self) -> Result<()> {
        let state = self.lock();
        if let Some(failure) = &state.failure {
            return Err(failure.clone());
        }
        if state.sealed {
            return Err(error(ErrorCode::Ebusy));
        }
        Ok(())
    }
    pub(crate) fn register_route(
        &self,
        partition: &str,
        drive: &str,
        definition: serde_json::Value,
        plan: Arc<DriverRuntimePlan>,
        policy: IntegrityPolicy,
    ) -> Result<()> {
        let identity = ScopeIdentity {
            cluster: self.cluster.clone(),
            partition: partition.to_owned(),
            drive: drive.to_owned(),
        };
        let mut state = self.lock();
        if state.sealed || state.failure.is_some() {
            return Err(error(ErrorCode::Ebusy));
        }
        if state.count >= self.max_routes
            || state
                .routes
                .get(partition)
                .is_some_and(|p| p.contains_key(drive))
        {
            return Err(error(ErrorCode::Einval));
        }
        // Reserve only accepted local routes; rejected names cannot consume
        // additional identity capacity. This contains no provider await.
        self.local.identity_epoch(&identity)?;
        state
            .routes
            .entry(partition.to_owned())
            .or_default()
            .insert(
                drive.to_owned(),
                Arc::new(Route {
                    identity,
                    definition,
                    plan,
                    policy,
                    state: Mutex::new(RouteState::default()),
                }),
            );
        state.count += 1;
        Ok(())
    }
    fn route(&self, scope: &CacheScope) -> Result<Arc<Route>> {
        self.require_open()?;
        if scope.identity.cluster != self.cluster {
            return Err(error(ErrorCode::Eacces));
        }
        self.lock()
            .routes
            .get(scope.identity.partition.as_str())
            .and_then(|p| p.get(scope.identity.drive.as_str()))
            .cloned()
            .ok_or_else(|| error(ErrorCode::Eacces))
    }
    fn revoke(&self, route: &Route) -> Result<()> {
        self.local.revoke_identity_admission(&route.identity)?;
        let mut state = route.lock();
        state.operation = match state.operation.checked_add(1) {
            Some(operation) => operation,
            None => {
                let failure = error(ErrorCode::Eio);
                state.failure = Some(failure.clone());
                return Err(failure);
            }
        };
        // Keep any ticket/task/observer/permit, but prevent its old publication.
        let proof = state.proof.take();
        drop(state);
        drop(proof);
        self.refusals.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    async fn validate_definition(&self, route: &Route) -> Result<()> {
        let catalog = match self.catalog.load_shared_current().await {
            Ok(catalog) => catalog,
            Err(_) => {
                self.revoke(route)?;
                return Err(error(ErrorCode::Eio));
            }
        };
        if catalog
            .partitions
            .get(route.identity.partition.as_str())
            .and_then(|p| p.drives.get(route.identity.drive.as_str()))
            .is_none_or(|drive| drive.driver != route.definition)
        {
            self.revoke(route)?;
            return Err(error(ErrorCode::Estale));
        }
        self.require_open()
    }
    fn retain_owner_failure(&self, route: &Route, failure: FsError) -> FsError {
        let (failure, first) = {
            let mut state = route.lock();
            let first = state.failure.is_none();
            (state.failure.get_or_insert(failure).clone(), first)
        };
        if first {
            let _ = self.local.revoke_identity_admission(&route.identity);
        }
        failure
    }
    fn observe_ticket(
        &self,
        route: &Route,
        ticket: &InspectionTicket,
    ) -> Poll<Result<InspectionOutcome>> {
        match ticket.poll_join() {
            Poll::Ready(Err(failure)) => {
                Poll::Ready(Err(self.retain_owner_failure(route, failure)))
            }
            Poll::Ready(Ok(outcome)) => match &outcome.cleanup {
                Err(failure) => Poll::Ready(Err(self.retain_owner_failure(route, failure.clone()))),
                Ok(()) => Poll::Ready(Ok(outcome)),
            },
            Poll::Pending => Poll::Pending,
        }
    }
    fn reap_completed_capacity(&self) -> Result<()> {
        // No route/global guard crosses actual join polling. The worklist is
        // bounded by physical inspector permits, not the 10,000 route catalog.
        let active: Vec<_> = self
            .lock()
            .active
            .iter()
            .filter_map(|(route, ticket)| Some((route.upgrade()?, ticket.upgrade()?)))
            .collect();
        for (route, ticket) in active {
            if route.lock().failure.is_some() {
                continue;
            }
            if let Poll::Ready(Ok(outcome)) = self.observe_ticket(&route, &ticket)
                && outcome.cleanup.is_ok()
            {
                ticket.release_permit()?;
                self.remove_active(&ticket);
            }
        }
        Ok(())
    }
    fn remove_active(&self, ticket: &Arc<InspectionTicket>) {
        let ticket = Arc::downgrade(ticket);
        self.lock()
            .active
            .retain(|(_, current)| !Weak::ptr_eq(current, &ticket));
    }
    fn start_or_join(&self, route: &Arc<Route>) -> Result<Arc<InspectionTicket>> {
        self.require_open()?;
        {
            let state = route.lock();
            if let Some(failure) = &state.failure {
                return Err(failure.clone());
            }
            if let Some(ticket) = &state.ticket {
                return Ok(ticket.clone());
            }
        }
        if route.plan.cold_cache_options().is_none() {
            return Err(error(ErrorCode::Enotsup));
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| error(ErrorCode::Eio))?;
        let permit = match self.slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.reap_completed_capacity()?;
                self.slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| error(ErrorCode::Ebusy))?
            }
        };
        let mut state = route.lock();
        if let Some(failure) = &state.failure {
            return Err(failure.clone());
        }
        if let Some(ticket) = &state.ticket {
            return Ok(ticket.clone());
        }
        // Route -> registry is the only nested lock order. Seal never holds
        // this mutex while visiting routes. Starting and owner installation are
        // atomic with shutdown seal.
        let mut registry = self.lock();
        if let Some(failure) = &registry.failure {
            return Err(failure.clone());
        }
        if registry.sealed {
            return Err(error(ErrorCode::Ebusy));
        }
        let epoch = self.local.identity_epoch(&route.identity)?;
        state.operation = match state.operation.checked_add(1) {
            Some(operation) => operation,
            None => {
                let failure = error(ErrorCode::Eio);
                state.failure = Some(failure.clone());
                return Err(failure);
            }
        };
        let ticket = InspectionTicket::new(state.operation, epoch, permit);
        // Registry ownership exists before spawn can poll the actual operation.
        state.ticket = Some(ticket.clone());
        registry
            .active
            .push((Arc::downgrade(route), Arc::downgrade(&ticket)));
        let inspector = self.inspector.clone();
        let context = self.context.clone();
        let plan = route.plan.clone();
        let journal = ticket.journal.clone();
        let local = self.local.clone();
        let identity = route.identity.clone();
        let refusals = self.refusals.clone();
        let route_owner = Arc::downgrade(route);
        let task = runtime.spawn(async move {
            let mut mode = inspector.inspect(&plan, &context, journal.as_ref()).await;
            // Refusal invalidates admission immediately, before provider cleanup
            // can await. Cancellation of the requesting RPC cannot hide it.
            if mode.is_err() || matches!(mode, Ok(None)) {
                refusals.fetch_add(1, Ordering::Relaxed);
                if let Err(failure) = local.revoke_identity_admission(&identity)
                    && mode.is_ok()
                {
                    mode = Err(failure);
                }
            }
            let cleanup = journal.close().await;
            if let Err(failure) = &cleanup {
                // This weak edge cannot form route -> ticket -> task -> route.
                // A canceled waiter must not hide terminal owner uncertainty.
                if let Some(route) = route_owner.upgrade() {
                    route.lock().failure.get_or_insert(failure.clone());
                }
            }
            if cleanup.is_err() && mode.is_ok() {
                refusals.fetch_add(1, Ordering::Relaxed);
                if let Err(failure) = local.revoke_identity_admission(&identity) {
                    mode = Err(failure);
                }
            }
            InspectionOutcome { mode, cleanup }
        });
        ticket.lock().task = Some(task);
        drop(registry);
        self.starts.fetch_add(1, Ordering::Relaxed);
        Ok(ticket)
    }
    fn discard_known(&self, route: &Route, ticket: &Arc<InspectionTicket>) -> Result<()> {
        let mut state = route.lock();
        if state
            .ticket
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, ticket))
        {
            state.ticket = None;
        }
        drop(state);
        ticket.release_permit()?;
        self.remove_active(ticket);
        Ok(())
    }
    async fn admit_scope(&self, requested: &CacheScope) -> Result<ScopeLease> {
        let route = self.route(requested)?;
        self.validate_definition(&route).await?;
        let retained_ticket = route.lock().ticket.clone();
        if let Some(ticket) = retained_ticket {
            let _ = self.observe_ticket(&route, &ticket);
        }
        {
            let mut state = route.lock();
            self.require_open()?;
            if let Some(failure) = &state.failure {
                return Err(failure.clone());
            }
            // Healthy reuse probes the current epoch without allocating a group.
            if let Some(proof) = &state.proof
                && proof.lease.is_current()?
                && proof.scope == *requested
            {
                self.reuses.fetch_add(1, Ordering::Relaxed);
                return Ok(proof.lease.clone());
            }
            // Trusted local active registration is consulted before an old
            // retained proof can deny a newly locally verified backing.
            if let Some(lease) = self.local.current_scope_lease(requested, route.policy)? {
                let admitted = lease.clone();
                let previous = state.proof.replace(Proof {
                    scope: requested.clone(),
                    lease,
                });
                drop(state);
                drop(previous);
                return Ok(admitted);
            }
            if let Some(proof) = &state.proof
                && proof.lease.is_current()?
            {
                return Err(error(ErrorCode::Estale));
            }
            drop(state.proof.take());
        }
        let ticket = self.start_or_join(&route)?;
        let outcome = match ticket.join().await {
            Ok(outcome) => outcome,
            Err(failure) => {
                return Err(self.retain_owner_failure(&route, failure));
            }
        };
        if let Err(failure) = outcome.cleanup {
            return Err(self.retain_owner_failure(&route, failure));
        }
        let mode = match outcome.mode {
            Ok(Some(mode)) => mode,
            Ok(None) => {
                self.discard_known(&route, &ticket)?;
                return Err(error(ErrorCode::Enotsup));
            }
            Err(failure) => {
                // The actual task already observed and revoked this refusal.
                // Do not revoke a later locally verified generation again.
                self.discard_known(&route, &ticket)?;
                return Err(failure);
            }
        };
        // Fresh post-await catalog inspection, after actual join + cleanup.
        if let Err(failure) = self.validate_definition(&route).await {
            self.discard_known(&route, &ticket)?;
            return Err(failure);
        }
        let verified = CacheScope {
            identity: route.identity.clone(),
            backing: mode.backing,
        };
        let admitted;
        {
            let mut state = route.lock();
            // A route failure introduced during the final catalog await must
            // block both fresh publication and a coalesced waiter's reuse.
            if let Some(failure) = &state.failure {
                return Err(failure.clone());
            }
            let registry = self.lock();
            if let Some(failure) = &registry.failure {
                return Err(failure.clone());
            }
            if registry.sealed {
                return Err(error(ErrorCode::Ebusy));
            }
            if state.operation != ticket.operation
                || !state
                    .ticket
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &ticket))
            {
                // A coalesced waiter may observe the first waiter's completed
                // publication. Reuse that exact current proof, never resurrect
                // an old ticket or allocate another registration group.
                let accepted = state
                    .proof
                    .as_ref()
                    .filter(|proof| {
                        proof.scope == *requested && matches!(proof.lease.is_current(), Ok(true))
                    })
                    .map(|proof| proof.lease.clone());
                drop(registry);
                drop(state);
                self.discard_known(&route, &ticket)?;
                return accepted.ok_or_else(|| error(ErrorCode::Estale));
            }
            // Compare and publish under the cache's shared identity epoch lock.
            let lease = match self.local.register_scope_lease(
                verified.clone(),
                route.policy,
                ticket.epoch.clone(),
            ) {
                Ok(lease) => lease,
                Err(failure) => {
                    state.ticket = None;
                    drop(registry);
                    drop(state);
                    ticket.release_permit()?;
                    self.remove_active(&ticket);
                    return Err(failure);
                }
            };
            admitted = lease.clone();
            let previous = state.proof.replace(Proof {
                scope: verified.clone(),
                lease,
            });
            state.ticket = None;
            drop(registry);
            drop(state);
            drop(previous);
        }
        ticket.release_permit()?;
        self.remove_active(&ticket);
        self.successes.fetch_add(1, Ordering::Relaxed);
        if verified != *requested {
            return Err(error(ErrorCode::Estale));
        }
        Ok(admitted)
    }
    fn routes(&self) -> Vec<Arc<Route>> {
        self.lock()
            .routes
            .values()
            .flat_map(|p| p.values().cloned())
            .collect()
    }
    fn owner_state_error(&self) -> Option<FsError> {
        // End the registry guard before scanning route guards. Chaining a
        // temporary guard into or_else would recursively lock this mutex.
        let registry_failure = { self.lock().failure.clone() };
        registry_failure.or_else(|| {
            self.routes()
                .into_iter()
                .find_map(|route| route.lock().failure.clone())
        })
    }
    async fn drain(&self) -> Result<()> {
        let mut failure = self.lock().failure.clone();
        for route in self.routes() {
            let (ticket, route_failure) = {
                let state = route.lock();
                (state.ticket.clone(), state.failure.clone())
            };
            if let Some(error) = route_failure {
                failure.get_or_insert(error);
            }
            if let Some(ticket) = ticket {
                match ticket.join().await {
                    Ok(outcome) => {
                        let route_failure = { route.lock().failure.clone() };
                        match outcome.cleanup {
                            Ok(()) if route_failure.is_none() => {
                                if let Err(error) = self.discard_known(&route, &ticket) {
                                    failure.get_or_insert(error);
                                }
                            }
                            Ok(()) => {}
                            Err(error) => {
                                failure.get_or_insert(error);
                            }
                        }
                    }
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            }
            // Do not cache an earlier pre-await view of poisoned owner state.
            if let Some(error) = route.lock().failure.clone() {
                failure.get_or_insert(error);
            }
        }
        if let Some(error) = self.owner_state_error() {
            failure.get_or_insert(error);
        }
        failure.map_or(Ok(()), Err)
    }
    pub(crate) fn seal_admission(&self) -> Result<()> {
        let failure = {
            let mut state = self.lock();
            state.sealed = true;
            self.slots.close();
            state.failure.clone()
        };
        failure.map_or(Ok(()), Err)
    }
    pub(crate) async fn seal_and_drain(&self) -> Result<()> {
        // Attempt every retained actual join even if sealing observed poison.
        // The ownership failure is preserved by the later state checks.
        let _ = self.seal_admission();
        let _closing = self.drain_lock.lock().await;
        let previous = self.lock().drain_result.clone();
        if let Some(previous) = previous {
            return match self.owner_state_error() {
                Some(error) => Err(error),
                None => previous,
            };
        }
        let mut result = tokio::time::timeout(self.drain_bound, self.drain())
            .await
            .unwrap_or_else(|_| Err(error(ErrorCode::Ebusy)));
        if let Some(error) = self.owner_state_error() {
            result = Err(error);
        }
        #[cfg(test)]
        {
            let gate = { self.drain_publication_gate.lock().unwrap().clone() };
            if let Some(gate) = gate {
                gate.entered.add_permits(1);
                gate.release.acquire().await.unwrap().forget();
            }
        }
        let mut state = self.lock();
        if result.is_ok()
            && let Some(error) = &state.failure
        {
            result = Err(error.clone());
        }
        state.drain_result = Some(result.clone());
        result
    }
    #[cfg(test)]
    fn wait_proof_release_gate(&self, boundary: ProofReleaseBoundary) {
        let gate = { self.proof_release_gate.lock().unwrap().clone() };
        if let Some(gate) = gate
            && gate.boundary == boundary
        {
            gate.entered.add_permits(1);
            let mut released = gate.released.lock().unwrap();
            while !*released {
                released = gate.changed.wait(released).unwrap();
            }
        }
    }
    pub(crate) fn release_proofs(&self) -> Result<()> {
        if let Some(error) = self.owner_state_error() {
            return Err(error);
        }
        #[cfg(test)]
        self.wait_proof_release_gate(ProofReleaseBoundary::Registry);
        let routes: Vec<_> = {
            let state = self.lock();
            if let Some(failure) = &state.failure {
                return Err(failure.clone());
            }
            if !matches!(state.drain_result, Some(Ok(()))) {
                return Err(error(ErrorCode::Ebusy));
            }
            state
                .routes
                .values()
                .flat_map(|partition| partition.values().cloned())
                .collect()
        };
        for route in routes {
            #[cfg(test)]
            self.wait_proof_release_gate(ProofReleaseBoundary::Route);
            let proof = {
                let mut state = route.lock();
                if let Some(failure) = &state.failure {
                    return Err(failure.clone());
                }
                state.proof.take()
            };
            drop(proof);
        }
        Ok(())
    }
    pub(crate) fn snapshot(&self) -> HolderSnapshot {
        let (count, sealed) = {
            let state = self.lock();
            (state.count, state.sealed)
        };
        let mut retained_proof_groups = 0;
        let mut admitted_proofs = 0;
        let mut retained_inspectors = 0;
        for route in self.routes() {
            let state = route.lock();
            if let Some(proof) = &state.proof {
                retained_proof_groups += 1;
                if matches!(proof.lease.is_current(), Ok(true)) {
                    admitted_proofs += 1;
                }
            }
            retained_inspectors += usize::from(state.ticket.is_some());
        }
        HolderSnapshot {
            configured_routes: count,
            retained_proof_groups,
            admitted_proofs,
            retained_inspectors,
            sealed,
            inspection_starts: self.starts.load(Ordering::Relaxed),
            inspection_successes: self.successes.load(Ordering::Relaxed),
            authority_refusals: self.refusals.load(Ordering::Relaxed),
            proof_reuses: self.reuses.load(Ordering::Relaxed),
        }
    }
}
#[async_trait]
impl ScopeAdmission for ConfiguredHolderRegistry {
    async fn admit(&self, requested: &CacheScope) -> Result<ScopeLease> {
        self.admit_scope(requested).await
    }
}
impl super::InspectionOwner for ConfiguredHolderRegistry {
    fn seal_admission(&self) -> Result<()> {
        ConfiguredHolderRegistry::seal_admission(self)
    }
}
#[async_trait]
impl ConstructionResource for ConfiguredHolderRegistry {
    async fn close(&self) -> Result<()> {
        self.seal_and_drain().await
    }
}

#[cfg(test)]
#[path = "cold_holder_tests.rs"]
mod tests;
