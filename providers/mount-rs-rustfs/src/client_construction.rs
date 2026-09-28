//! Bounded ownership of pure client constructors and their actual join handles.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};

use async_trait::async_trait;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::{ErrorCode, FsError, Result};
use object_store::aws::AmazonS3;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::{
    OWNED_PREFIX_OBSERVATION_DEADLINE, RustFsBlockStore, RustFsConfig,
    observe_owned_prefix_absence_with, validate_owned_prefix,
};

fn sealed_error() -> FsError {
    FsError::new(ErrorCode::Estale).with_message("RustFS client construction admission is sealed")
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
            } => RustFsBlockStore::from_config(&config, prefix, durable)
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
        registry.sealed |= registry.quarantined;
        if registry.sealed {
            for ticket in &registry.tickets {
                ticket.seal();
            }
        }
        registry.tickets.retain(|ticket| !ticket.acknowledged());
        registry
    }

    fn drive(&self) {
        let tickets = self.lock().tickets.clone();
        for ticket in tickets {
            ticket.drive();
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let registry = self.registry.get_mut().unwrap_or_else(|p| p.into_inner());
        for ticket in registry.tickets.drain(..) {
            if !ticket.acknowledged() {
                // Explicit close is required. Keep unproven ownership alive for
                // process lifetime, including an actual pending join handle.
                ticket.seal();
                std::mem::forget(ticket);
            }
        }
    }
}

/// Bounded, shared ownership of constructor work. Call `close` explicitly.
///
/// Cleanup acknowledges constructor join and result transfer/disposal, not an
/// HTTP connection or socket shutdown. Panic and poison remain quarantined.
#[derive(Clone)]
pub struct RustFsConstructionContext {
    owner: Arc<Owner>,
}

impl RustFsConstructionContext {
    pub fn new(max_builds: usize) -> Result<Self> {
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
        })
    }

    /// Reject and wake admissions synchronously; do not wait for constructors.
    pub fn seal_admission(&self) -> Result<()> {
        let mut registry = self.owner.lock();
        registry.sealed = true;
        for ticket in &registry.tickets {
            ticket.seal();
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
                if registry.tickets.len() < self.owner.max_builds {
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
        match self
            .build(
                Recipe::BlockStore {
                    config,
                    prefix,
                    durable,
                },
                observer,
            )
            .await?
        {
            Product::BlockStore(store) => Ok(*store),
            Product::OwnedPrefixProbe(_) => unreachable!("closed block-store recipe"),
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
            charged: registry.tickets.len(),
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
