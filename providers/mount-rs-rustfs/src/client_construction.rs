//! Bounded ownership of pure client constructors and their actual join handles.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll, Wake, Waker};

use async_trait::async_trait;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::{ErrorCode, FsError, Result};
use object_store::aws::AmazonS3;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::{
    OWNED_PREFIX_OBSERVATION_DEADLINE, RawBlockCacheBudget, RustFsBlockStore, RustFsConfig,
    SignedClientBundle, observe_owned_prefix_absence_with, validate_owned_prefix,
};

fn sealed_error() -> FsError {
    FsError::new(ErrorCode::Estale).with_message("RustFS client construction admission is sealed")
}

// This bounds retained configurations, independently of concurrent constructor
// capacity. A miss when full uses the original owned, uncached construction.
const MAX_CACHED_CLIENT_BUNDLES: usize = 64;

// No Debug implementation: exact credentials participate in equality but must
// never enter a diagnostic, error or metrics label. Client policy is immutable
// within a context; changes to transport defaults require a fresh context.
struct ClientBundleKey(RustFsConfig);

impl ClientBundleKey {
    fn matches(&self, config: &RustFsConfig) -> bool {
        self.0.endpoint == config.endpoint
            && self.0.bucket == config.bucket
            && self.0.region == config.region
            && self.0.access_key_id == config.access_key_id
            && self.0.secret_access_key == config.secret_access_key
    }
}

enum BundleRecipe {
    Config(RustFsConfig),
    #[cfg(test)]
    Test(Box<dyn FnOnce() -> Result<Arc<SignedClientBundle>> + Send>),
}

impl BundleRecipe {
    fn build(self) -> Result<Arc<SignedClientBundle>> {
        match self {
            Self::Config(config) => SignedClientBundle::build(&config),
            #[cfg(test)]
            Self::Test(factory) => factory(),
        }
    }
}

#[derive(Default)]
struct SharedBundleState {
    registered: bool,
    sealed: bool,
    quarantined: bool,
    waiters: usize,
    handle: Option<JoinHandle<Result<Arc<SignedClientBundle>>>>,
    ready: Option<Result<Arc<SignedClientBundle>>>,
    joined: bool,
    disposing: bool,
    receipt: Option<Result<()>>,
}

struct SharedBundleTicket {
    key: ClientBundleKey,
    state: Mutex<SharedBundleState>,
    signals: Arc<Signals>,
}

impl SharedBundleTicket {
    fn lock(&self) -> MutexGuard<'_, SharedBundleState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poison) => {
                let mut state = poison.into_inner();
                let newly_quarantined = !state.quarantined;
                state.quarantined = true;
                state.sealed = true;
                if state.receipt.is_some() {
                    state.receipt = Some(Err(quarantine_error()));
                }
                if newly_quarantined {
                    self.signals.changed();
                }
                state
            }
        }
    }

    fn charged(&self) -> bool {
        let state = self.lock();
        state.quarantined || !state.joined || state.disposing
    }

    fn removable(&self) -> bool {
        let state = self.lock();
        matches!(state.receipt, Some(Ok(())))
            || (state.joined
                && state.waiters == 0
                && !state.quarantined
                && matches!(state.ready, Some(Err(_))))
    }

    fn seal(&self) {
        let changed = {
            let mut state = self.lock();
            let changed = !state.sealed;
            state.sealed = true;
            changed
        };
        if changed {
            self.signals.changed();
        }
    }

    fn drive(&self) {
        let discarded = {
            let mut state = self.lock();
            if state.receipt.is_some() || state.disposing || !state.registered {
                return;
            }
            if let Some(handle) = state.handle.as_mut() {
                let waker = Waker::from(self.signals.clone());
                let mut cx = Context::from_waker(&waker);
                match Pin::new(handle).poll(&mut cx) {
                    Poll::Pending => return,
                    Poll::Ready(result) => {
                        state.handle = None;
                        state.joined = true;
                        match result {
                            Ok(product) => state.ready = Some(product),
                            Err(_) => {
                                state.quarantined = true;
                                state.sealed = true;
                            }
                        }
                        // The entry already belongs to the context. A complete
                        // joined result releases constructor capacity even when
                        // every caller canceled; no publisher task is detached.
                        self.signals.changed();
                    }
                }
            }
            if !state.sealed && !state.quarantined {
                return;
            }
            state.disposing = true;
            state.ready.take()
        };
        let disposed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(discarded)));
        {
            let mut state = self.lock();
            state.quarantined |= disposed.is_err();
            state.disposing = false;
            state.joined = true;
            state.receipt = Some(if state.quarantined {
                Err(quarantine_error())
            } else {
                Ok(())
            });
        }
        self.signals.changed();
    }

    // A caller journal owns an acquisition/join, not the context's cached
    // clients. Its close cannot cancel another caller or evict a ready bundle.
    async fn drain_acquisition(&self) -> Result<()> {
        loop {
            let changed = self.signals.ticket.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.drive();
            let result = {
                let state = self.lock();
                if let Some(receipt) = &state.receipt {
                    Some(receipt.clone())
                } else if state.joined && !state.sealed && !state.quarantined {
                    Some(Ok(()))
                } else {
                    None
                }
            };
            if let Some(result) = result {
                return result;
            }
            changed.await;
        }
    }
}

struct SharedWaiter(Arc<SharedBundleTicket>);

impl Drop for SharedWaiter {
    fn drop(&mut self) {
        self.0.lock().waiters -= 1;
        self.0.signals.changed();
    }
}

struct SharedRegistration<'a> {
    ticket: &'a SharedBundleTicket,
    leader: bool,
    completed: bool,
}

impl Drop for SharedRegistration<'_> {
    fn drop(&mut self) {
        if !self.completed {
            let mut state = self.ticket.lock();
            if self.leader {
                state.registered = true;
            }
            state.sealed = true;
            state.quarantined = true;
            drop(state);
            self.ticket.signals.changed();
        }
    }
}

struct SharedLeaseState {
    entry: Option<Arc<SharedBundleTicket>>,
    receipt: Option<Result<()>>,
    releasing: bool,
    quarantined: bool,
}

struct SharedLease {
    state: Mutex<SharedLeaseState>,
    changed: Notify,
    ticket: Weak<SharedBundleTicket>,
}

impl SharedLease {
    fn lock(&self) -> MutexGuard<'_, SharedLeaseState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poison) => {
                let mut state = poison.into_inner();
                let newly_quarantined = !state.quarantined;
                state.quarantined = true;
                state.receipt = Some(Err(quarantine_error()));
                if newly_quarantined {
                    if let Some(entry) = self.ticket.upgrade() {
                        let mut shared = entry.lock();
                        shared.quarantined = true;
                        shared.sealed = true;
                        if shared.receipt.is_some() {
                            shared.receipt = Some(Err(quarantine_error()));
                        }
                        drop(shared);
                        entry.signals.changed();
                    }
                    self.changed.notify_waiters();
                }
                state
            }
        }
    }
}

#[async_trait]
impl ConstructionResource for SharedLease {
    async fn close(&self) -> Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let entry = {
                let state = self.lock();
                if let Some(receipt) = &state.receipt {
                    return receipt.clone();
                }
                (!state.releasing).then(|| {
                    state
                        .entry
                        .as_ref()
                        .expect("unclosed shared lease owns its entry")
                        .clone()
                })
            };
            let Some(entry) = entry else {
                changed.await;
                continue;
            };
            let result = entry.drain_acquisition().await;
            let release = {
                let mut state = self.lock();
                if let Some(receipt) = &state.receipt {
                    return receipt.clone();
                }
                if state.releasing {
                    None
                } else {
                    state.releasing = true;
                    Some(if result.is_ok() {
                        state.entry.take()
                    } else {
                        None
                    })
                }
            };
            let Some(released) = release else {
                drop(entry);
                changed.await;
                continue;
            };
            // No await separates actual lease release from its receipt. A
            // failed cleanup preserves the same retained entry for diagnosis.
            drop(released);
            drop(entry);
            {
                let mut state = self.lock();
                if state.quarantined {
                    return Err(quarantine_error());
                }
                state.releasing = false;
                state.receipt = Some(result.clone());
            }
            self.changed.notify_waiters();
            return result;
        }
    }
}

enum BundleReservation {
    Shared {
        entry: Arc<SharedBundleTicket>,
        leader: bool,
    },
    Uncached,
}

fn quarantine_error() -> FsError {
    FsError::backend("RustFS client construction cleanup is quarantined")
}

/// A separately configured startup client; construction does not perform LIST.
pub struct OwnedPrefixProbe {
    store: AmazonS3,
    prefix: String,
}

impl OwnedPrefixProbe {
    /// Await the existing exact-prefix observation with its 30 second deadline.
    pub async fn observe_owned_prefix_absence(&self) -> Result<bool> {
        observe_owned_prefix_absence_with(
            &self.store,
            &self.prefix,
            OWNED_PREFIX_OBSERVATION_DEADLINE,
        )
        .await
    }
}

enum Recipe {
    BlockStore {
        config: RustFsConfig,
        prefix: String,
        durable: bool,
        raw_cache_budget: RawBlockCacheBudget,
    },
    OwnedPrefixProbe {
        config: RustFsConfig,
        prefix: String,
    },
    #[cfg(test)]
    Test(Box<dyn FnOnce() -> Result<RustFsBlockStore> + Send>),
}

enum Product {
    BlockStore(Box<RustFsBlockStore>),
    OwnedPrefixProbe(Box<OwnedPrefixProbe>),
}

impl Recipe {
    fn build(self) -> Result<Product> {
        match self {
            Self::BlockStore {
                config,
                prefix,
                durable,
                raw_cache_budget,
            } => RustFsBlockStore::from_config_with_cache_budget(
                &config,
                prefix,
                durable,
                raw_cache_budget,
            )
            .map(Box::new)
            .map(Product::BlockStore),
            Self::OwnedPrefixProbe { config, prefix } => {
                let store = config.build_client_with_probe_limits(true).map_err(|_| {
                    FsError::backend("RustFS owned prefix observation client construction failed")
                })?;
                Ok(Product::OwnedPrefixProbe(Box::new(OwnedPrefixProbe {
                    store,
                    prefix,
                })))
            }
            #[cfg(test)]
            Self::Test(factory) => factory().map(Box::new).map(Product::BlockStore),
        }
    }
}

// Signals retain neither a ticket nor its registry. The actual task always sees
// this fixed owner waker, regardless of which caller most recently polled it.
struct Signals {
    ticket: Notify,
    registry: Arc<Notify>,
}

impl Signals {
    fn changed(&self) {
        self.ticket.notify_waiters();
        self.registry.notify_waiters();
    }
}

impl Wake for Signals {
    fn wake(self: Arc<Self>) {
        self.changed();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.changed();
    }
}

#[derive(Default)]
struct TicketState {
    registered: bool,
    sealed: bool,
    quarantined: bool,
    handle: Option<JoinHandle<Result<Product>>>,
    ready: Option<Result<Product>>,
    disposing: bool,
    receipt: Option<Result<()>>,
}

struct Ticket {
    state: Mutex<TicketState>,
    signals: Arc<Signals>,
}

impl Ticket {
    fn lock(&self) -> MutexGuard<'_, TicketState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poison) => {
                let mut state = poison.into_inner();
                let newly_quarantined = !state.quarantined;
                state.quarantined = true;
                state.sealed = true;
                if state.receipt.is_some() {
                    state.receipt = Some(Err(quarantine_error()));
                }
                if newly_quarantined {
                    self.signals.changed();
                }
                state
            }
        }
    }

    fn seal(&self) {
        let changed = {
            let mut state = self.lock();
            let changed = !state.sealed;
            state.sealed = true;
            changed
        };
        if changed {
            self.signals.changed();
        }
    }

    fn acknowledged(&self) -> bool {
        matches!(self.lock().receipt, Some(Ok(())))
    }

    // No await may separate extracting a discarded product from its actual
    // disposal and receipt. Charge remains held throughout that interval.
    fn drive(&self) {
        let discarded = {
            let mut state = self.lock();
            if state.receipt.is_some() || state.disposing || !state.registered {
                return;
            }
            if let Some(handle) = state.handle.as_mut() {
                let waker = Waker::from(self.signals.clone());
                let mut cx = Context::from_waker(&waker);
                match Pin::new(handle).poll(&mut cx) {
                    Poll::Pending => return,
                    Poll::Ready(result) => {
                        state.handle = None;
                        match result {
                            Ok(product) => state.ready = Some(product),
                            Err(_) => {
                                state.quarantined = true;
                                state.sealed = true;
                            }
                        }
                    }
                }
            }
            if !state.sealed && !state.quarantined {
                return;
            }
            state.disposing = true;
            state.ready.take()
        };
        let disposed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(discarded)));
        {
            let mut state = self.lock();
            state.quarantined |= disposed.is_err();
            state.disposing = false;
            state.receipt = Some(if state.quarantined {
                Err(quarantine_error())
            } else {
                Ok(())
            });
        }
        self.signals.changed();
    }

    async fn drain(&self) -> Result<()> {
        loop {
            let changed = self.signals.ticket.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.drive();
            if let Some(receipt) = self.lock().receipt.clone() {
                return receipt;
            }
            changed.await;
        }
    }
}

#[async_trait]
impl ConstructionResource for Ticket {
    async fn close(&self) -> Result<()> {
        self.seal();
        self.drain().await
    }
}

// Cancellation only seals ownership. It never takes or aborts the join handle.
struct BuildWaiter(Arc<Ticket>);

impl Drop for BuildWaiter {
    fn drop(&mut self) {
        self.0.seal();
    }
}

struct Registration<'a> {
    ticket: &'a Ticket,
    completed: bool,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if !self.completed {
            let mut state = self.ticket.lock();
            state.registered = true;
            state.sealed = true;
            state.quarantined = true;
            drop(state);
            self.ticket.signals.changed();
        }
    }
}

#[derive(Default)]
struct Registry {
    tickets: Vec<Arc<Ticket>>,
    bundles: Vec<Arc<SharedBundleTicket>>,
    sealed: bool,
    quarantined: bool,
}

struct Owner {
    registry: Mutex<Registry>,
    changed: Arc<Notify>,
    max_builds: usize,
}

impl Owner {
    fn lock(&self) -> MutexGuard<'_, Registry> {
        let mut registry = match self.registry.lock() {
            Ok(registry) => registry,
            Err(poison) => {
                let mut registry = poison.into_inner();
                registry.quarantined = true;
                registry
            }
        };
        registry.quarantined |= registry
            .tickets
            .iter()
            .any(|ticket| ticket.lock().quarantined);
        registry.quarantined |= registry
            .bundles
            .iter()
            .any(|entry| entry.lock().quarantined);
        registry.sealed |= registry.quarantined;
        if registry.sealed {
            for ticket in &registry.tickets {
                ticket.seal();
            }
            for entry in &registry.bundles {
                entry.seal();
            }
        }
        registry.tickets.retain(|ticket| !ticket.acknowledged());
        registry.bundles.retain(|entry| !entry.removable());
        registry
    }

    fn drive(&self) {
        let (tickets, bundles) = {
            let registry = self.lock();
            (registry.tickets.clone(), registry.bundles.clone())
        };
        for ticket in tickets {
            ticket.drive();
        }
        for entry in bundles {
            entry.drive();
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let registry = match self.registry.get_mut() {
            Ok(registry) => registry,
            Err(poison) => {
                let registry = poison.into_inner();
                registry.quarantined = true;
                registry.sealed = true;
                registry
            }
        };
        for ticket in registry.tickets.drain(..) {
            if !ticket.acknowledged() {
                // Explicit close is required. Keep unproven ownership alive for
                // process lifetime, including an actual pending join handle.
                ticket.seal();
                std::mem::forget(ticket);
            }
        }
        for entry in registry.bundles.drain(..) {
            if registry.quarantined {
                let mut state = entry.lock();
                state.quarantined = true;
                state.sealed = true;
                if state.receipt.is_some() {
                    state.receipt = Some(Err(quarantine_error()));
                }
            }
            entry.seal();
            entry.drive();
            if !matches!(entry.lock().receipt, Some(Ok(()))) {
                // Preserve pending or quarantined native ownership just as the
                // original independently owned constructor path does.
                std::mem::forget(entry);
            }
        }
    }
}

/// Bounded, shared ownership of constructor work. Call `close` explicitly.
///
/// Cleanup acknowledges constructor join and result transfer/disposal, not an
/// HTTP connection or socket shutdown. Panic and poison remain quarantined.
/// Exact configurations reuse four immutable signed clients across independent
/// Drive facades. At most 64 configurations are cached; additional configurations
/// use the independently owned construction path rather than rejecting drives.
/// Transport policy is fixed for this context's lifetime. Create a new context
/// after changing transport defaults, credentials or endpoint policy.
#[derive(Clone)]
pub struct RustFsConstructionContext {
    owner: Arc<Owner>,
    raw_cache_budget: RawBlockCacheBudget,
}

impl RustFsConstructionContext {
    pub fn new(max_builds: usize) -> Result<Self> {
        Self::new_with_cache_budget(max_builds, RawBlockCacheBudget::new(64 * 1024 * 1024, 4096))
    }

    /// Apply one common adapter capacity owner across every configuration and
    /// prefix, including the uncached-client fallback and retained facades.
    pub fn new_with_cache_budget(
        max_builds: usize,
        raw_cache_budget: RawBlockCacheBudget,
    ) -> Result<Self> {
        if max_builds == 0 {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("RustFS construction capacity must be positive"));
        }
        Ok(Self {
            owner: Arc::new(Owner {
                registry: Mutex::new(Registry::default()),
                changed: Arc::new(Notify::new()),
                max_builds,
            }),
            raw_cache_budget,
        })
    }

    /// The returned type originates in mount-rs-object-store-blocks.
    pub fn raw_cache_budget(&self) -> RawBlockCacheBudget {
        self.raw_cache_budget.clone()
    }

    /// Reject and wake admissions synchronously; do not wait for constructors.
    pub fn seal_admission(&self) -> Result<()> {
        let mut registry = self.owner.lock();
        registry.sealed = true;
        for ticket in &registry.tickets {
            ticket.seal();
        }
        for entry in &registry.bundles {
            entry.seal();
        }
        let failed = registry.quarantined;
        drop(registry);
        self.owner.changed.notify_waiters();
        if failed {
            Err(quarantine_error())
        } else {
            Ok(())
        }
    }

    /// Seal, join every retained worker and dispose all unclaimed products.
    /// Cancellation preserves the same tickets for a later close.
    pub async fn close(&self) -> Result<()> {
        let _ = self.seal_admission();
        loop {
            let changed = self.owner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.owner.drive();
            let result = {
                let registry = self.owner.lock();
                if registry
                    .tickets
                    .iter()
                    .all(|ticket| ticket.lock().receipt.is_some())
                    && registry
                        .bundles
                        .iter()
                        .all(|entry| entry.lock().receipt.is_some())
                {
                    Some(if registry.quarantined {
                        Err(quarantine_error())
                    } else {
                        Ok(())
                    })
                } else {
                    None
                }
            };
            if let Some(result) = result {
                return result;
            }
            changed.await;
        }
    }

    async fn reserve(&self) -> Result<Arc<Ticket>> {
        loop {
            let changed = self.owner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.owner.drive();
            {
                let mut registry = self.owner.lock();
                if registry.quarantined {
                    return Err(quarantine_error());
                }
                if registry.sealed {
                    return Err(sealed_error());
                }
                let charged_bundles = registry
                    .bundles
                    .iter()
                    .filter(|entry| entry.charged())
                    .count();
                if registry.tickets.len() + charged_bundles < self.owner.max_builds {
                    let ticket = Arc::new(Ticket {
                        state: Mutex::new(TicketState::default()),
                        signals: Arc::new(Signals {
                            ticket: Notify::new(),
                            registry: self.owner.changed.clone(),
                        }),
                    });
                    registry.tickets.push(ticket.clone());
                    return Ok(ticket);
                }
            }
            changed.await;
        }
    }

    async fn reserve_bundle(&self, config: &RustFsConfig) -> Result<BundleReservation> {
        loop {
            let changed = self.owner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.owner.drive();
            {
                let mut registry = self.owner.lock();
                if registry.quarantined {
                    return Err(quarantine_error());
                }
                if registry.sealed {
                    return Err(sealed_error());
                }
                if let Some(entry) = registry
                    .bundles
                    .iter()
                    .find(|entry| entry.key.matches(config))
                {
                    entry.lock().waiters += 1;
                    return Ok(BundleReservation::Shared {
                        entry: entry.clone(),
                        leader: false,
                    });
                }
                if registry.bundles.len() == MAX_CACHED_CLIENT_BUNDLES {
                    return Ok(BundleReservation::Uncached);
                }
                let charged_bundles = registry
                    .bundles
                    .iter()
                    .filter(|entry| entry.charged())
                    .count();
                if registry.tickets.len() + charged_bundles < self.owner.max_builds {
                    let entry = Arc::new(SharedBundleTicket {
                        key: ClientBundleKey(config.clone()),
                        state: Mutex::new(SharedBundleState {
                            waiters: 1,
                            ..Default::default()
                        }),
                        signals: Arc::new(Signals {
                            ticket: Notify::new(),
                            registry: self.owner.changed.clone(),
                        }),
                    });
                    registry.bundles.push(entry.clone());
                    return Ok(BundleReservation::Shared {
                        entry,
                        leader: true,
                    });
                }
            }
            changed.await;
        }
    }

    async fn acquire_bundle(
        &self,
        entry: Arc<SharedBundleTicket>,
        leader: bool,
        recipe: BundleRecipe,
        observer: Option<&dyn ConstructionObserver>,
    ) -> Result<Arc<SignedClientBundle>> {
        let _waiter = SharedWaiter(entry.clone());
        let mut registration = SharedRegistration {
            ticket: &entry,
            leader,
            completed: false,
        };
        if let Some(observer) = observer {
            observer.retain(Arc::new(SharedLease {
                state: Mutex::new(SharedLeaseState {
                    entry: Some(entry.clone()),
                    receipt: None,
                    releasing: false,
                    quarantined: false,
                }),
                changed: Notify::new(),
                ticket: Arc::downgrade(&entry),
            }));
        }
        {
            let registry = self.owner.lock();
            let mut state = entry.lock();
            if registry.sealed || state.sealed || state.quarantined {
                state.sealed = true;
            } else if leader {
                state.handle = Some(tokio::task::spawn_blocking(move || recipe.build()));
            }
            if leader {
                state.registered = true;
            }
            registration.completed = true;
        }
        entry.signals.changed();
        loop {
            let changed = entry.signals.ticket.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            entry.drive();
            let result = {
                let registry = self.owner.lock();
                let state = entry.lock();
                if state.quarantined {
                    return Err(quarantine_error());
                }
                if registry.sealed || state.sealed {
                    return Err(sealed_error());
                }
                state.ready.clone()
            };
            if let Some(result) = result {
                return result;
            }
            changed.await;
        }
    }

    async fn build(
        &self,
        recipe: Recipe,
        observer: Option<&dyn ConstructionObserver>,
    ) -> Result<Product> {
        let ticket = self.reserve().await?;
        let _waiter = BuildWaiter(ticket.clone());
        let mut registration = Registration {
            ticket: &ticket,
            completed: false,
        };
        // The strongly retained reservation precedes this arbitrary callback.
        if let Some(observer) = observer {
            observer.retain(ticket.clone());
        }
        {
            // Installation, claim and context seal share this lock order.
            let registry = self.owner.lock();
            let mut state = ticket.lock();
            if registry.sealed || state.sealed || state.quarantined {
                state.sealed = true;
            } else {
                // The closure owns only its recipe, never the ticket/context.
                state.handle = Some(tokio::task::spawn_blocking(move || recipe.build()));
            }
            state.registered = true;
            registration.completed = true;
        }
        ticket.signals.changed();
        loop {
            let changed = ticket.signals.ticket.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            ticket.drive();
            let claimed = {
                let registry = self.owner.lock();
                let mut state = ticket.lock();
                if state.quarantined {
                    // A poisoned running handle still belongs to close/drain.
                    return Err(quarantine_error());
                }
                if let Some(receipt) = &state.receipt {
                    return receipt.clone().and_then(|()| Err(sealed_error()));
                }
                if !registry.sealed && !state.sealed {
                    let product = state.ready.take();
                    state.disposing = product.is_some();
                    product
                } else {
                    None
                }
            };
            if let Some(product) = claimed {
                // Transfer has won against seal. No await occurs before its
                // independent terminal cleanup receipt releases the charge.
                {
                    let mut state = ticket.lock();
                    if state.quarantined {
                        state.ready = Some(product);
                        state.disposing = false;
                        drop(state);
                        ticket.drive();
                        return Err(quarantine_error());
                    }
                    state.disposing = false;
                    state.receipt = Some(Ok(()));
                }
                ticket.signals.changed();
                return product;
            }
            changed.await;
        }
    }

    pub async fn block_store(
        &self,
        config: RustFsConfig,
        prefix: String,
        durable: bool,
        observer: Option<&dyn ConstructionObserver>,
    ) -> Result<RustFsBlockStore> {
        match self.reserve_bundle(&config).await? {
            BundleReservation::Uncached => {
                match self
                    .build(
                        Recipe::BlockStore {
                            config,
                            prefix,
                            durable,
                            raw_cache_budget: self.raw_cache_budget.clone(),
                        },
                        observer,
                    )
                    .await?
                {
                    Product::BlockStore(store) => Ok(*store),
                    Product::OwnedPrefixProbe(_) => unreachable!("closed block-store recipe"),
                }
            }
            BundleReservation::Shared { entry, leader } => {
                let metrics = mount_rs_core::diagnostics::object_store::Observer::enabled();
                let span = metrics.bundle_build();
                let result = self
                    .acquire_bundle(entry, leader, BundleRecipe::Config(config), observer)
                    .await
                    .and_then(|bundle| {
                        RustFsBlockStore::from_client_bundle_with_cache_budget(
                            &bundle,
                            prefix,
                            durable,
                            self.raw_cache_budget.clone(),
                        )
                    });
                match result {
                    Ok(mut store) => {
                        store._http_bundle = span.finish_success();
                        Ok(store)
                    }
                    Err(error) => {
                        span.finish_error();
                        Err(error)
                    }
                }
            }
        }
    }

    pub async fn owned_prefix_probe(
        &self,
        config: RustFsConfig,
        prefix: String,
        observer: Option<&dyn ConstructionObserver>,
    ) -> Result<OwnedPrefixProbe> {
        validate_owned_prefix(&prefix)?;
        match self
            .build(Recipe::OwnedPrefixProbe { config, prefix }, observer)
            .await?
        {
            Product::OwnedPrefixProbe(probe) => Ok(*probe),
            Product::BlockStore(_) => unreachable!("closed prefix-probe recipe"),
        }
    }

    #[cfg(test)]
    pub(crate) async fn build_shared_test(
        &self,
        config: RustFsConfig,
        factory: Box<dyn FnOnce() -> Result<Arc<SignedClientBundle>> + Send>,
        observer: Option<&dyn ConstructionObserver>,
    ) -> Result<RustFsBlockStore> {
        self.build_shared_test_with_prefix(config, "shared-test".into(), false, factory, observer)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn build_shared_test_with_prefix(
        &self,
        config: RustFsConfig,
        prefix: String,
        durable: bool,
        factory: Box<dyn FnOnce() -> Result<Arc<SignedClientBundle>> + Send>,
        observer: Option<&dyn ConstructionObserver>,
    ) -> Result<RustFsBlockStore> {
        match self.reserve_bundle(&config).await? {
            BundleReservation::Shared { entry, leader } => {
                let bundle = self
                    .acquire_bundle(entry, leader, BundleRecipe::Test(factory), observer)
                    .await?;
                RustFsBlockStore::from_client_bundle_with_cache_budget(
                    &bundle,
                    prefix,
                    durable,
                    self.raw_cache_budget.clone(),
                )
            }
            BundleReservation::Uncached => {
                let budget = self.raw_cache_budget.clone();
                self.build_test(
                    Box::new(move || {
                        let bundle = factory()?;
                        RustFsBlockStore::from_client_bundle_with_cache_budget(
                            &bundle, prefix, durable, budget,
                        )
                    }),
                    observer,
                )
                .await
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn test_shared_lease_releasing(
        &self,
        index: usize,
    ) -> (Arc<dyn ConstructionResource>, impl Fn() + use<>) {
        let entry = self.owner.lock().bundles[index].clone();
        let lease = Arc::new(SharedLease {
            state: Mutex::new(SharedLeaseState {
                entry: Some(entry.clone()),
                receipt: None,
                releasing: true,
                quarantined: false,
            }),
            changed: Notify::new(),
            ticket: Arc::downgrade(&entry),
        });
        let controlled = lease.clone();
        (lease, move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = controlled.state.lock().unwrap();
                panic!("controlled shared lease release poison");
            }));
            drop(controlled.lock());
        })
    }

    #[cfg(test)]
    pub(crate) fn test_shared_snapshot(&self) -> (usize, usize, usize) {
        let registry = self.owner.lock();
        (
            registry.bundles.len(),
            registry
                .bundles
                .iter()
                .filter(|entry| entry.charged())
                .count(),
            registry
                .bundles
                .iter()
                .map(|entry| entry.lock().waiters)
                .sum(),
        )
    }

    #[cfg(test)]
    pub(crate) fn test_poison_shared_ticket(&self, index: usize) {
        let entry = self.owner.lock().bundles[index].clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = entry.state.lock().unwrap();
            panic!("controlled shared construction ticket poison");
        }));
    }

    #[cfg(test)]
    pub(crate) async fn build_test(
        &self,
        factory: Box<dyn FnOnce() -> Result<RustFsBlockStore> + Send>,
        observer: Option<&dyn ConstructionObserver>,
    ) -> Result<RustFsBlockStore> {
        match self.build(Recipe::Test(factory), observer).await? {
            Product::BlockStore(store) => Ok(*store),
            Product::OwnedPrefixProbe(_) => unreachable!("closed test recipe"),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_snapshot(&self) -> TestSnapshot {
        let registry = self.owner.lock();
        TestSnapshot {
            retained: registry.tickets.len(),
            charged: registry.tickets.len()
                + registry
                    .bundles
                    .iter()
                    .filter(|entry| entry.charged())
                    .count(),
            sealed: registry.sealed,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_poison_ticket(&self, index: usize) {
        let ticket = self.owner.lock().tickets[index].clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = ticket.state.lock().unwrap();
            panic!("controlled construction ticket poison");
        }));
    }

    #[cfg(test)]
    pub(crate) fn test_poison_registry(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self.owner.registry.lock().unwrap();
            panic!("controlled construction registry poison");
        }));
    }

    #[cfg(test)]
    pub(crate) fn test_owner_liveness(&self) -> impl Fn() -> bool + use<> {
        let owner = Arc::downgrade(&self.owner);
        move || owner.strong_count() != 0
    }
}

#[cfg(test)]
pub(crate) struct TestSnapshot {
    pub(crate) retained: usize,
    pub(crate) charged: usize,
    pub(crate) sealed: bool,
}
