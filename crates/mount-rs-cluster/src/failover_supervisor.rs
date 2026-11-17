//! Runs the failover machine for every candidate volume of one server: opens
//! a volume when the machine says so, serves and publishes it while its
//! writer lease is held, and releases it when the lease is lost, handed back
//! or the server stops. See `docs/serve-http-failover.md`.
//!
//! Rules kept here, whatever the registry says:
//! * a route is published only for a volume that is open, and so whose
//!   writer lease this process holds, under that lease's fence;
//! * a volume is opened only after the registry was read and showed no
//!   other node's route;
//! * a registry that cannot be reached never makes an owner let go.
use crate::{
    FailoverMachine, FailoverTimings, OpenOutcome, OwnedVolumeRegistration, OwnerAction,
    OwnerHttpHandler, OwnerHttpRouter, OwnerView, RedisRouteRegistry, RegistrationError,
    RegistryError, ReleaseReason, RouteView, SessionBudget, SessionCredentials, SessionSupervisor,
    StandbyAction, VolumeRoute,
};
use arc_swap::ArcSwapOption;
use mount_rs_core::ErrorCode;
use mount_rs_http::HttpServerOptions;
use mount_rs_sdk::Filesystem;
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::watch, task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;

pub type OpenFuture<'a> =
    Pin<Box<dyn Future<Output = mount_rs_core::Result<Arc<Filesystem>>> + Send + 'a>>;

/// Opens one volume for exclusive writing, which takes its writer lease.
///
/// `EAGAIN` must mean that the lease is held by someone else; any other
/// error is treated as this node being unable to serve the volume for now.
/// The future is always polled to completion, so an open that acquired the
/// lease is never abandoned.
pub trait DriveFactory: Send + Sync + 'static {
    fn open(&self) -> OpenFuture<'_>;
}
impl<F, Fut> DriveFactory for F
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = mount_rs_core::Result<Arc<Filesystem>>> + Send + 'static,
{
    fn open(&self) -> OpenFuture<'_> {
        Box::pin(self())
    }
}

/// One volume this server is a candidate owner of.
pub struct FailoverDrive {
    pub volume: String,
    pub bearer_token: String,
    /// Enables remote sessions on the volume while it is owned here.
    pub sessions: Option<SessionCredentials>,
    pub factory: Arc<dyn DriveFactory>,
}

#[derive(Clone)]
pub struct FailoverOptions {
    pub timings: FailoverTimings,
    /// Lifetime of a published route and of a candidate announcement.
    pub route_ttl: Duration,
    /// This server's own membership record, published in its routes.
    pub node: String,
    pub boot: String,
    pub address: SocketAddr,
    pub tls_server_name: String,
    pub http: HttpServerOptions,
    pub max_sessions_per_drive: usize,
    pub max_handles_per_session: usize,
    pub session_idle_timeout: Duration,
    pub session_budget: SessionBudget,
    /// Whether a node id read from the candidate set is a trusted member.
    /// Asked on every use, so a membership reload takes effect at once.
    pub trusted: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Called on every state change, from the volume's own task.
    pub events: Arc<dyn Fn(&FailoverEvent) + Send + Sync>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveStatus {
    Standby,
    Opening,
    /// The lease is held under `fence`. `published` is whether the route is
    /// currently confirmed in the registry.
    Owner {
        fence: u64,
        published: bool,
    },
    Releasing,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailoverEvent {
    /// The volume is open here: this server holds its writer lease under
    /// this fence and serves it on its own listener.
    Acquired { volume: String, fence: u64 },
    /// The volume's route is published, at first or again after an outage.
    Published { volume: String, fence: u64 },
    /// The volume was released and this node is a standby again.
    Released {
        volume: String,
        fence: u64,
        reason: ReleaseReason,
    },
    /// No route is published but the lease is still held by someone else.
    /// Reported once per absence, not on every retry.
    LeaseHeld { volume: String },
    /// The volume could not be opened or served; the node backs off.
    OpenFailed { volume: String, error: String },
    /// The lease is held but the route cannot be published or refreshed.
    /// Reported once per outage.
    RouteUnpublished { volume: String, error: String },
    /// Releasing the volume did not finish cleanly. The lease may stay held
    /// until it expires.
    ReleaseFailed { volume: String, error: String },
}

struct Shared {
    volume: String,
    status: watch::Sender<DriveStatus>,
    /// True once the volume has made its first decision since startup.
    settled: watch::Sender<bool>,
    filesystem: ArcSwapOption<Filesystem>,
}

/// Owns the failover task of every candidate volume of one server.
///
/// Call [`Self::shutdown`] before the process exits: it releases every
/// volume this server owns, so a standby can take over at once. Dropping the
/// supervisor instead aborts the tasks and leaves the leases to expire.
pub struct FailoverSupervisor {
    cancel: CancellationToken,
    drives: Vec<Arc<Shared>>,
    failed: Arc<AtomicBool>,
    task: crate::join_slot::JoinSlot<()>,
}

struct ExitGuard {
    failed: Arc<AtomicBool>,
    cancel: CancellationToken,
}
impl Drop for ExitGuard {
    fn drop(&mut self) {
        // A task that ends without being asked to has stopped failing over.
        if !self.cancel.is_cancelled() {
            self.failed.store(true, Ordering::Release);
        }
    }
}

impl FailoverSupervisor {
    /// Start one task per drive. Every drive starts as a standby; none is
    /// opened before the registry has been read.
    pub fn start(
        registry: RedisRouteRegistry,
        router: OwnerHttpRouter,
        drives: Vec<FailoverDrive>,
        options: FailoverOptions,
    ) -> Result<Self, RegistryError> {
        options.timings.validate()?;
        if drives.is_empty()
            || drives.len() > 4096
            || options.route_ttl < Duration::from_millis(60)
            || options.route_ttl > Duration::from_secs(30)
        {
            return Err(RegistryError::InvalidInput);
        }
        let mut volumes = std::collections::BTreeSet::new();
        if drives.iter().any(|d| !volumes.insert(d.volume.clone())) {
            return Err(RegistryError::InvalidInput);
        }
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| RegistryError::InvalidInput)?;
        let cancel = CancellationToken::new();
        let failed = Arc::new(AtomicBool::new(false));
        let options = Arc::new(options);
        let mut shared = Vec::new();
        let mut workers = Vec::new();
        for drive in drives {
            let machine =
                FailoverMachine::new(drive.volume.clone(), options.node.clone(), options.timings)?;
            let state = Arc::new(Shared {
                volume: drive.volume.clone(),
                status: watch::channel(DriveStatus::Standby).0,
                settled: watch::channel(false).0,
                filesystem: ArcSwapOption::empty(),
            });
            shared.push(state.clone());
            workers.push(Worker {
                drive,
                registry: registry.clone(),
                router: router.clone(),
                options: options.clone(),
                machine,
                shared: state,
                cancel: cancel.clone(),
                live: None,
                announced_at: None,
                announced: false,
                lease_held_reported: false,
            });
        }
        let guard_failed = failed.clone();
        let guard_cancel = cancel.clone();
        let task = runtime.spawn(async move {
            let mut tasks = JoinSet::new();
            for worker in workers {
                let guard = ExitGuard {
                    failed: guard_failed.clone(),
                    cancel: guard_cancel.clone(),
                };
                tasks.spawn(async move {
                    let _guard = guard;
                    worker.run().await;
                });
            }
            while tasks.join_next().await.is_some() {}
        });
        Ok(Self {
            cancel,
            drives: shared,
            failed,
            task: crate::join_slot::JoinSlot::new(task),
        })
    }

    /// Wait until every drive has made its first decision: it is published
    /// here, or it is a standby that has read the registry. Returns false
    /// if that took longer than `wait`.
    pub async fn settled(&self, wait: Duration) -> bool {
        let all = async {
            for drive in &self.drives {
                let mut settled = drive.settled.subscribe();
                if settled.wait_for(|settled| *settled).await.is_err() {
                    return false;
                }
            }
            true
        };
        tokio::time::timeout(wait, all).await.unwrap_or(false)
    }

    pub fn statuses(&self) -> Vec<(String, DriveStatus)> {
        self.drives
            .iter()
            .map(|drive| (drive.volume.clone(), *drive.status.borrow()))
            .collect()
    }

    /// The volumes open here now, for observation only (cache statistics).
    pub fn filesystems(&self) -> Vec<(String, Arc<Filesystem>)> {
        self.drives
            .iter()
            .filter_map(|drive| Some((drive.volume.clone(), drive.filesystem.load_full()?)))
            .collect()
    }

    /// A failover task ended without being asked to.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Release every volume owned here (route withdrawn, requests drained,
    /// lease released) and stop. Safe to call again.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(Err(_)) = self.task.join().await {
            self.failed.store(true, Ordering::Release);
        }
    }
}
impl Drop for FailoverSupervisor {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

struct Worker {
    drive: FailoverDrive,
    registry: RedisRouteRegistry,
    router: OwnerHttpRouter,
    options: Arc<FailoverOptions>,
    machine: FailoverMachine,
    shared: Arc<Shared>,
    cancel: CancellationToken,
    /// Trusted live candidates as last read; `None` when the read failed.
    live: Option<Vec<String>>,
    announced_at: Option<Instant>,
    announced: bool,
    lease_held_reported: bool,
}

/// A volume open here: the lease is held for as long as this exists.
struct Owned {
    filesystem: Arc<Filesystem>,
    registration: OwnedVolumeRegistration,
    sessions: Option<SessionSupervisor>,
    fence: u64,
}

impl Worker {
    fn event(&self, event: FailoverEvent) {
        (self.options.events)(&event);
    }
    fn settle(&self) {
        self.shared.settled.send_replace(true);
    }
    fn status(&self, status: DriveStatus) {
        self.shared.status.send_replace(status);
    }
    fn volume(&self) -> String {
        self.drive.volume.clone()
    }
    fn cadence(&self) -> Duration {
        self.options.route_ttl / 3
    }

    /// Keep this node's announcement alive while it is a viable candidate
    /// and read who else is. `fresh` reads now rather than on the cadence.
    async fn refresh_candidates(&mut self, now: Instant, fresh: bool) {
        let trusted = |members: Vec<String>| -> Vec<String> {
            members
                .into_iter()
                .filter(|node| (self.options.trusted)(node))
                .collect()
        };
        let due = fresh
            || self.live.is_none()
            || self
                .announced_at
                .is_none_or(|at| now.saturating_duration_since(at) >= self.cadence());
        if !due {
            return;
        }
        let volume = &self.drive.volume;
        let read = if self.machine.is_candidate(now) {
            let result = self
                .registry
                .announce_candidate(volume, &self.options.node, self.options.route_ttl)
                .await;
            self.announced |= result.is_ok();
            result
        } else {
            if self.announced
                && self
                    .registry
                    .withdraw_candidate(volume, &self.options.node)
                    .await
                    .is_ok()
            {
                self.announced = false;
            }
            self.registry.candidates(volume).await
        };
        match read {
            Ok(members) => {
                self.live = Some(trusted(members));
                self.announced_at = Some(now);
            }
            Err(_) => {
                self.live = None;
                self.announced_at = None;
            }
        }
    }

    async fn run(mut self) {
        loop {
            if !self.standby().await {
                break;
            }
            self.status(DriveStatus::Opening);
            // Never cancelled: an open that took the lease must be kept or
            // released, not dropped half-way.
            let opened = self.drive.factory.open().await;
            let now = Instant::now();
            let owned = match opened {
                Ok(filesystem) => self.install(filesystem).await,
                Err(error) => Err(error),
            };
            match owned {
                Ok(owned) => {
                    self.machine.opened(now, OpenOutcome::Acquired);
                    self.lease_held_reported = false;
                    self.event(FailoverEvent::Acquired {
                        volume: self.volume(),
                        fence: owned.fence,
                    });
                    let reason = self.own(&owned).await;
                    self.release(owned, reason).await;
                    if reason == ReleaseReason::Shutdown {
                        break;
                    }
                }
                Err(error) if error.code == ErrorCode::Eagain => {
                    self.machine.opened(now, OpenOutcome::LeaseHeld);
                    if !self.lease_held_reported {
                        self.lease_held_reported = true;
                        self.event(FailoverEvent::LeaseHeld {
                            volume: self.volume(),
                        });
                    }
                }
                Err(error) => {
                    self.machine.opened(now, OpenOutcome::Failed);
                    self.event(FailoverEvent::OpenFailed {
                        volume: self.volume(),
                        error: error.to_string(),
                    });
                }
            }
            self.status(DriveStatus::Standby);
            self.settle();
        }
        if self.announced {
            let _ = self
                .registry
                .withdraw_candidate(&self.drive.volume, &self.options.node)
                .await;
        }
        self.status(DriveStatus::Stopped);
        self.settle();
    }

    /// Poll as a standby until the machine asks for an open (true) or the
    /// server is stopping (false).
    async fn standby(&mut self) -> bool {
        loop {
            if self.cancel.is_cancelled() {
                return false;
            }
            let found = self.registry.lookup(&self.drive.volume).await;
            let route = match &found {
                Err(_) => RouteView::Unknown,
                Ok(None) => RouteView::Absent,
                // This process holds no route while it is a standby, so a
                // route naming this node was left behind by an earlier one.
                Ok(Some(route)) if route.node == self.options.node => RouteView::Absent,
                Ok(Some(route)) => RouteView::Published(&route.node),
            };
            if matches!(route, RouteView::Published(_)) {
                self.lease_held_reported = false;
            }
            self.refresh_candidates(Instant::now(), route == RouteView::Absent)
                .await;
            // Decide on what was just read, at the time it was read.
            let action = self
                .machine
                .standby(Instant::now(), route, self.live.as_deref());
            if action == StandbyAction::Open && !self.cancel.is_cancelled() {
                return true;
            }
            self.settle();
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return false,
                _ = tokio::time::sleep(self.options.timings.poll) => {}
            }
        }
    }

    /// Bind the open volume to a route and serve it on the peer router and,
    /// through it, the public listener. Nothing is published yet. On an
    /// error the volume is shut down, which releases the lease.
    async fn install(&self, filesystem: Arc<Filesystem>) -> mount_rs_core::Result<Owned> {
        match self.try_install(&filesystem).await {
            Ok(owned) => Ok(owned),
            Err(error) => {
                if let Err(release) = filesystem.shutdown().await {
                    self.event(FailoverEvent::ReleaseFailed {
                        volume: self.volume(),
                        error: release.to_string(),
                    });
                }
                Err(error)
            }
        }
    }

    async fn try_install(&self, filesystem: &Arc<Filesystem>) -> mount_rs_core::Result<Owned> {
        let failed = |what: &str| {
            mount_rs_core::FsError::new(ErrorCode::Eio)
                .with_syscall("failover-install")
                .with_message(what.to_owned())
        };
        // Fails for a volume without exclusive ownership: it has no fence.
        let fence = filesystem.checked_owner_fence().await?;
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| failed("cannot generate a registration id"))?;
        let route = VolumeRoute {
            volume: self.volume(),
            node: self.options.node.clone(),
            boot: self.options.boot.clone(),
            registration: format!("{:032x}", u128::from_be_bytes(nonce)),
            fence,
            address: self.options.address,
            tls_server_name: self.options.tls_server_name.clone(),
            protocols: crate::PeerProtocolRange::SUPPORTED,
        };
        let registration = OwnedVolumeRegistration::new(
            filesystem.clone(),
            self.registry.clone(),
            route.clone(),
            self.options.route_ttl,
        )
        .await
        .map_err(|error| failed(&format!("route registration: {error:?}")))?;
        let mut handler = OwnerHttpHandler::new(
            filesystem.clone(),
            route,
            self.drive.bearer_token.clone(),
            self.options.http.clone(),
        )
        .await
        .map_err(|error| failed(&format!("owner handler: {error}")))?;
        if let Some(credentials) = &self.drive.sessions {
            handler = handler
                .with_sessions(
                    credentials.clone(),
                    self.options.max_sessions_per_drive,
                    self.options.max_handles_per_session,
                    self.options.session_idle_timeout,
                    self.options.session_budget.clone(),
                )
                .map_err(|error| failed(&format!("owner sessions: {error}")))?;
        }
        let sessions = handler
            .session_registry()
            .map(|registry| SessionSupervisor::start(vec![registry]))
            .transpose()?;
        if self.router.insert(handler).is_err() {
            if let Some(sessions) = sessions {
                let _ = sessions.shutdown().await;
            }
            return Err(failed("the volume is already served by this server"));
        }
        self.shared.filesystem.store(Some(filesystem.clone()));
        Ok(Owned {
            filesystem: filesystem.clone(),
            registration,
            sessions,
            fence,
        })
    }

    /// Hold the volume: keep its route published and its candidacy alive,
    /// and watch the lease. Returns why the volume must be released.
    async fn own(&mut self, owned: &Owned) -> ReleaseReason {
        let cadence = self.cadence();
        let mut published = false;
        let mut outage_reported = false;
        let mut next_publish = Instant::now();
        loop {
            if self.cancel.is_cancelled() {
                return ReleaseReason::Shutdown;
            }
            let now = Instant::now();
            // The lease, not the registry, is what makes this node the owner.
            let lease = tokio::time::timeout(cadence, owned.filesystem.checked_owner_fence()).await;
            let mut view = match lease {
                Ok(Ok(fence)) if fence == owned.fence => OwnerView::Held,
                Ok(_) => OwnerView::Lost,
                // Storage did not answer. The volume fails itself closed if
                // a renewal is lost; until then nothing is known.
                Err(_) => OwnerView::Held,
            };
            if view == OwnerView::Held && now >= next_publish {
                // Advertising validates the lease before and after the
                // write, sets a missing record, extends this exact record,
                // and is refused only by a live record with a fence at
                // least as high: a successor.
                let result = tokio::time::timeout(cadence, owned.registration.advertise())
                    .await
                    .unwrap_or(Err(RegistrationError::Deadline));
                match result {
                    Ok(()) => {
                        next_publish = now + cadence;
                        if !published {
                            published = true;
                            outage_reported = false;
                            self.event(FailoverEvent::Published {
                                volume: self.volume(),
                                fence: owned.fence,
                            });
                        }
                    }
                    Err(
                        RegistrationError::OwnershipLost
                        | RegistrationError::Conflict
                        | RegistrationError::Registry(
                            RegistryError::InvalidInput | RegistryError::InvalidRecord,
                        ),
                    ) => view = OwnerView::Lost,
                    Err(error) => {
                        // The registry is unreachable. Keep the lease and
                        // keep serving this server's own listener.
                        published = false;
                        if !outage_reported {
                            outage_reported = true;
                            self.event(FailoverEvent::RouteUnpublished {
                                volume: self.volume(),
                                error: format!("{error:?}"),
                            });
                        }
                    }
                }
            }
            self.status(DriveStatus::Owner {
                fence: owned.fence,
                published,
            });
            if view == OwnerView::Held {
                self.refresh_candidates(now, false).await;
            }
            if let OwnerAction::Release(reason) =
                self.machine
                    .owner(Instant::now(), view, self.live.as_deref())
            {
                return reason;
            }
            self.settle();
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return ReleaseReason::Shutdown,
                _ = tokio::time::sleep(self.options.timings.poll) => {}
            }
        }
    }

    /// Withdraw the route, stop serving, drain, and release the lease, in
    /// that order. Runs to completion even when the server is stopping.
    async fn release(&mut self, owned: Owned, reason: ReleaseReason) {
        self.status(DriveStatus::Releasing);
        let volume = self.volume();
        let events = self.options.events.clone();
        let report = |error: String| {
            events(&FailoverEvent::ReleaseFailed {
                volume: volume.clone(),
                error,
            })
        };
        // Exact-record removal: it cannot touch a successor's route. If the
        // registry is unreachable the record expires by itself.
        let _ = owned.registration.withdraw().await;
        if reason == ReleaseReason::Shutdown
            && self.announced
            && self
                .registry
                .withdraw_candidate(&volume, &self.options.node)
                .await
                .is_ok()
        {
            // Gone before the lease is released, so that the standbys rank
            // themselves without this node when they see the route go.
            self.announced = false;
        }
        if let Some(handler) = self.router.remove(&volume)
            && let Err(error) = handler.close().await
        {
            report(format!("request drain: {error}"));
        }
        if let Some(sessions) = &owned.sessions
            && let Err(error) = sessions
                .shutdown_with_timeout(self.options.http.drain_timeout)
                .await
        {
            report(format!("session drain: {error}"));
        }
        self.shared.filesystem.store(None);
        if let Err(error) = owned.filesystem.shutdown().await {
            // A lost lease cannot be released; that is expected.
            if reason != ReleaseReason::LeaseLost {
                report(format!("volume shutdown: {error}"));
            }
        }
        self.machine.released(Instant::now(), reason);
        // Read the candidate set again before the next decision.
        self.live = None;
        self.event(FailoverEvent::Released {
            volume,
            fence: owned.fence,
            reason,
        });
    }
}
