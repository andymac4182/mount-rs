//! Prepared CLI construction choice for the shared service lifecycle adapter.
//!
//! Provider/context/cache ownership remains server-owned. This constructor is
//! immutable and does not activate providers until the runtime pool opens it.

use async_trait::async_trait;
use mount_rs_core::Result;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_sdk::StorageContext;
use mount_rs_service::filesystem_runtime::{ConstructedRuntime, RuntimeConstructor};

use crate::runtime::DriverRuntimePlan;
use crate::server_cache::DriveCacheDecorator;

pub(crate) struct CliRuntimeConstructor {
    pub(crate) plan: Arc<DriverRuntimePlan>,
    pub(crate) context: StorageContext,
    pub(crate) decorator: Option<DriveCacheDecorator>,
}

#[async_trait]
impl RuntimeConstructor for CliRuntimeConstructor {
    async fn construct(&self, observer: &dyn ConstructionObserver) -> Result<ConstructedRuntime> {
        self.plan
            .construct_for_service(
                self.decorator
                    .as_ref()
                    .map(|decorator| decorator as &dyn mount_rs_sdk::BlockStoreDecorator),
                &self.context,
                observer,
            )
            .await
    }
}

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll};

use mount_rs_core::{ErrorCode, FsError};
use mount_rs_service::filesystem_runtime::SdkRuntimeFactory;
use mount_rs_service::runtime_pool::RuntimePool;
use tokio::sync::watch;

use crate::server_cache::ServerCache;

#[async_trait]
impl ConstructionResource for ServerCache {
    async fn close(&self) -> Result<()> {
        self.shutdown().await
    }
}

type CloseFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// Constructor cleanup is independent of backing/SQL close authority.
#[async_trait]
trait ClientBuildOwner: Send + Sync {
    fn seal_admission(&self) -> Result<()>;
    async fn close(&self) -> Result<()>;
}

#[async_trait]
impl ClientBuildOwner for StorageContext {
    fn seal_admission(&self) -> Result<()> {
        self.seal_client_builds()
    }

    async fn close(&self) -> Result<()> {
        self.close_client_builds().await
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One executable invocation retains unresolved actual owners until exit. Tests
/// inject their own keeper; no other invocation can replace a retained owner.
#[derive(Default)]
pub(crate) struct RemoteRuntimeKeeper(Mutex<Option<Arc<RemoteRuntimeLifecycle>>>);

impl RemoteRuntimeKeeper {
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        lock(&self.0).is_none()
    }

    #[cfg(test)]
    pub(crate) fn retained(&self) -> Option<Arc<RemoteRuntimeLifecycle>> {
        lock(&self.0).clone()
    }
    pub(crate) fn reserve(self: &Arc<Self>) -> Result<RemoteRuntimeScope> {
        let mut slot = lock(&self.0);
        if slot.is_some() {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        let lifecycle = Arc::new(RemoteRuntimeLifecycle {
            keeper: Arc::downgrade(self),
            closing: AtomicBool::new(false),
            owned: Mutex::new(OwnedResources::default()),
            completion: watch::channel(None).0,
            #[cfg(test)]
            worker: Mutex::new(None),
        });
        *slot = Some(lifecycle.clone());
        Ok(RemoteRuntimeScope(lifecycle))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ClosePhase {
    #[default]
    Listeners,
    Pool,
    Factories,
    ClientBuilds,
    Inspectors,
    Context,
    Cache,
    Complete,
}

#[derive(Default)]
struct OwnedResources {
    // Each consuming listener future owns the actual server before its first
    // poll, including during ordinary serving. Pending futures stay installed.
    listeners: Vec<Option<CloseFuture>>,
    pool: Option<RuntimePool>,
    factories: Vec<Arc<SdkRuntimeFactory>>,
    context: Option<StorageContext>,
    client_builds: Option<Arc<dyn ClientBuildOwner>>,
    inspectors: Option<Arc<dyn crate::server_cache::InspectionOwner>>,
    cache: Option<Arc<dyn ConstructionResource>>,
    phase: ClosePhase,
    current: Option<CloseFuture>,
    next_factory: usize,
    failure: Option<FsError>,
}

pub(crate) struct RemoteRuntimeLifecycle {
    keeper: Weak<RemoteRuntimeKeeper>,
    closing: AtomicBool,
    owned: Mutex<OwnedResources>,
    completion: watch::Sender<Option<Result<()>>>,
    #[cfg(test)]
    worker: Mutex<Option<tokio::task::AbortHandle>>,
}

/// Dropping the serving or close waiter requests the same independently owned
/// drain. Runtime loss leaves the keeper occupied, never a replacement permit.
pub(crate) struct RemoteRuntimeScope(pub(crate) Arc<RemoteRuntimeLifecycle>);

impl Drop for RemoteRuntimeScope {
    fn drop(&mut self) {
        self.0.request_close();
    }
}

impl RemoteRuntimeLifecycle {
    #[cfg(test)]
    pub(crate) fn listener_count(&self) -> usize {
        lock(&self.owned).listeners.len()
    }
    pub(crate) fn install_context(&self, context: StorageContext) -> Result<()> {
        let mut owned = lock(&self.owned);
        let rejection = if self.closing.load(Ordering::Acquire) {
            Some(ErrorCode::Estale)
        } else if owned.context.is_some() {
            Some(ErrorCode::Ebusy)
        } else {
            None
        };
        if let Some(code) = rejection {
            let error = FsError::new(code);
            // A rejected handoff cannot replace the existing owner or publish
            // behind its drain. Seal immediately, then quarantine the unproven
            // incoming context for process lifetime; SQL close is unauthorized.
            if let Err(seal_error) = context.seal_client_builds() {
                owned.failure.get_or_insert(seal_error);
            }
            owned.failure.get_or_insert(error.clone());
            std::mem::forget(context);
            return Err(error);
        }
        owned.client_builds = Some(Arc::new(context.clone()));
        owned.context = Some(context);
        Ok(())
    }

    pub(crate) fn install_cache(&self, cache: Arc<ServerCache>) {
        let mut owned = lock(&self.owned);
        owned.inspectors = cache.inspector_owner();
        owned.cache = Some(cache);
    }

    #[cfg(test)]
    pub(crate) fn install_inspector_owner_for_test(
        &self,
        owner: Arc<dyn crate::server_cache::InspectionOwner>,
    ) {
        lock(&self.owned).inspectors = Some(owner);
    }

    pub(crate) fn install_pool(&self, pool: RuntimePool) {
        lock(&self.owned).pool = Some(pool);
    }

    pub(crate) fn retain_factory(&self, factory: Arc<SdkRuntimeFactory>) {
        lock(&self.owned).factories.push(factory);
    }

    pub(crate) fn install_quic(&self, server: mount_rs_service::server::RemoteServer) {
        lock(&self.owned).listeners.push(Some(Box::pin(async move {
            server.close().await;
            Ok(())
        })));
    }

    pub(crate) fn install_websocket(&self, server: mount_rs_service::websocket::WebSocketServer) {
        lock(&self.owned).listeners.push(Some(Box::pin(async move {
            server.close().await;
            Ok(())
        })));
    }

    fn finish(&self, result: Result<()>) {
        self.completion.send_if_modified(|value| {
            if value.is_some() {
                false
            } else {
                *value = Some(result);
                true
            }
        });
    }

    fn request_close(self: &Arc<Self>) {
        let (client_builds, inspectors) = {
            // Serialize the first close snapshot with context installation.
            let owned = lock(&self.owned);
            if self.closing.swap(true, Ordering::AcqRel) {
                return;
            }
            (owned.client_builds.clone(), owned.inspectors.clone())
        };
        if let Some(client_builds) = client_builds
            && let Err(error) = client_builds.seal_admission()
        {
            lock(&self.owned).failure.get_or_insert(error);
        }
        // Stop fresh holder inspections on the first shutdown request, before
        // listener/pool/factory drains can await or fail. Invoke the separate
        // owner outside the lifecycle mutex; retain actual joins for Inspectors.
        if let Some(inspectors) = inspectors
            && let Err(error) = inspectors.seal_admission()
        {
            lock(&self.owned).failure.get_or_insert(error);
        }
        let guard = DrainGuard {
            lifecycle: self.clone(),
            acknowledged: false,
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let worker = runtime.spawn(async move { guard.run().await });
                #[cfg(test)]
                {
                    *lock(&self.worker) = Some(worker.abort_handle());
                }
                // Detach only the worker handle, not any resource/close future.
                drop(worker);
            }
            Err(_) => drop(guard),
        }
    }

    pub(crate) async fn close(self: &Arc<Self>) -> Result<()> {
        self.request_close();
        let mut receiver = self.completion.subscribe();
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| FsError::new(ErrorCode::Eio))?;
        }
    }

    fn poll_close(&self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let mut owned = lock(&self.owned);
        loop {
            if owned.phase == ClosePhase::Listeners {
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
                    owned.failure.get_or_insert(error);
                }
                // Every listener is first-polled before yielding on either.
                if owned.listeners.iter().any(Option::is_some) {
                    return Poll::Pending;
                }
                owned.phase = ClosePhase::Pool;
            }
            if let Some(future) = &mut owned.current {
                match future.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => {
                        if let Err(error) = result {
                            owned.failure.get_or_insert(error);
                        }
                        owned.current = None;
                        owned.phase = match owned.phase {
                            ClosePhase::Pool => ClosePhase::Factories,
                            ClosePhase::Factories => ClosePhase::Factories,
                            ClosePhase::ClientBuilds => ClosePhase::Inspectors,
                            ClosePhase::Inspectors => ClosePhase::Context,
                            ClosePhase::Context => ClosePhase::Cache,
                            ClosePhase::Cache => ClosePhase::Complete,
                            _ => unreachable!("installed close phase"),
                        };
                    }
                }
            }
            match owned.phase {
                ClosePhase::Pool => {
                    if let Some(pool) = owned.pool.clone() {
                        owned.current = Some(Box::pin(async move { pool.shutdown().await }));
                    } else {
                        owned.phase = ClosePhase::Factories;
                    }
                }
                ClosePhase::Factories => {
                    if let Some(factory) = owned.factories.get(owned.next_factory).cloned() {
                        owned.next_factory += 1;
                        owned.current = Some(Box::pin(async move { factory.close().await }));
                    } else {
                        owned.phase = ClosePhase::ClientBuilds;
                    }
                }
                ClosePhase::ClientBuilds => {
                    // Pure constructor joins must run even after pool/factory
                    // failure. Preserve the authority barrier in Inspectors.
                    if let Some(client_builds) = owned.client_builds.clone() {
                        owned.current = Some(Box::pin(async move { client_builds.close().await }));
                    } else {
                        owned.phase = ClosePhase::Inspectors;
                    }
                }
                ClosePhase::Inspectors => {
                    if let Some(error) = &owned.failure {
                        return Poll::Ready(Err(error.clone()));
                    }
                    if let Some(inspectors) = owned.inspectors.clone() {
                        owned.current = Some(Box::pin(async move { inspectors.close().await }));
                    } else {
                        owned.phase = ClosePhase::Context;
                    }
                }
                ClosePhase::Context => {
                    if let Some(error) = &owned.failure {
                        return Poll::Ready(Err(error.clone()));
                    }
                    if let Some(context) = owned.context.clone() {
                        owned.current = Some(Box::pin(async move { context.close().await }));
                    } else {
                        owned.phase = ClosePhase::Cache;
                    }
                }
                ClosePhase::Cache => {
                    if let Some(error) = &owned.failure {
                        return Poll::Ready(Err(error.clone()));
                    }
                    if let Some(cache) = owned.cache.clone() {
                        // Store the borrowed shutdown's actual join future too.
                        owned.current = Some(Box::pin(async move { cache.close().await }));
                    } else {
                        owned.phase = ClosePhase::Complete;
                    }
                }
                ClosePhase::Complete => {
                    if let Some(error) = &owned.failure {
                        return Poll::Ready(Err(error.clone()));
                    }
                    *owned = OwnedResources {
                        phase: ClosePhase::Complete,
                        ..OwnedResources::default()
                    };
                    return Poll::Ready(Ok(()));
                }
                ClosePhase::Listeners => unreachable!("listeners polled first"),
            }
        }
    }
}

struct DrainGuard {
    lifecycle: Arc<RemoteRuntimeLifecycle>,
    acknowledged: bool,
}

impl DrainGuard {
    async fn run(mut self) {
        let result = std::future::poll_fn(|cx| self.lifecycle.poll_close(cx)).await;
        if result.is_ok()
            && let Some(keeper) = self.lifecycle.keeper.upgrade()
        {
            let mut slot = lock(&keeper.0);
            if slot
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, &self.lifecycle))
            {
                *slot = None;
            }
        }
        self.lifecycle.finish(result);
        self.acknowledged = true;
    }
}

impl Drop for DrainGuard {
    fn drop(&mut self) {
        if !self.acknowledged {
            self.lifecycle.finish(Err(FsError::new(ErrorCode::Eio)));
        }
    }
}

#[cfg(test)]
#[path = "remote_runtime/lifecycle_tests.rs"]
mod lifecycle_tests;
