//! CLI-owned session routing and expiry. SDK owners must outlive a successful drain.
use crate::config::SessionLimitsConfig;
use crate::runtime::CliError;
use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, SessionSupervisor, VolumeSessions};
use mount_rs_http::{DriveRegistry, HttpBody, HttpRequestHandler, HttpServer, HttpServerOptions};
use mount_rs_sdk::Filesystem;
use std::{collections::BTreeMap, pin::Pin, sync::Arc, time::Duration};

pub(crate) struct Sessions {
    handlers: BTreeMap<String, SessionHttpHandler>,
    budget: mount_rs_cluster::SessionBudget,
    limits: SessionLimitsConfig,
    max_request_bytes: usize,
}
impl Sessions {
    pub(crate) fn new(
        max_request_bytes: usize,
        limits: SessionLimitsConfig,
    ) -> Result<Self, CliError> {
        Ok(Self {
            handlers: BTreeMap::new(),
            budget: mount_rs_cluster::SessionBudget::new(limits.max_sessions)?,
            limits,
            max_request_bytes,
        })
    }
    pub(crate) fn add(
        &mut self,
        volume: &str,
        credentials: SessionCredentials,
        filesystem: Arc<Filesystem>,
    ) -> Result<(), CliError> {
        let std::collections::btree_map::Entry::Vacant(entry) = self.handlers.entry(volume.into())
        else {
            return Err(CliError::usage(format!(
                "duplicate HTTP session volume '{volume}'"
            )));
        };
        let registry = Arc::new(VolumeSessions::new_with_budget(
            filesystem,
            self.limits.max_sessions_per_drive,
            self.limits.max_handles_per_session,
            Duration::from_secs(self.limits.idle_timeout_seconds),
            self.budget.clone(),
        )?);
        let handler = SessionHttpHandler::new(volume, credentials, registry)?
            .with_max_request_bytes(self.max_request_bytes)?;
        entry.insert(handler);
        Ok(())
    }
    pub(crate) async fn start(
        self,
        drives: DriveRegistry,
        options: HttpServerOptions,
        forwarding: Option<&crate::config::ForwardingConfig>,
    ) -> Result<Service, CliError> {
        let drain_timeout = options.drain_timeout;
        if self.handlers.is_empty() && forwarding.is_none() {
            let listener = HttpServer::start(drives, options)
                .await
                .map_err(|e| CliError::runtime(e.to_string()))?;
            return Ok(Service {
                listener,
                owner: None,
                application: None,
                peers: Vec::new(),
                reloader: None,
                supervisor: None,
                drain_timeout,
            });
        }
        let remote = match forwarding {
            Some(config) => {
                let files = PeerFiles::load(config, None)?;
                let (forwarder, peers) = connect_forwarder(
                    config,
                    self.max_request_bytes,
                    options.max_upload_bytes,
                    &files,
                )
                .await?;
                let reloader = files.reloader(config, &forwarder)?;
                Some((forwarder, peers, reloader))
            }
            None => None,
        };
        let local = drives.ids().map(str::to_owned).collect();
        let registries: Vec<_> = self
            .handlers
            .values()
            .map(SessionHttpHandler::registry)
            .collect();
        let supervisor = if registries.is_empty() {
            None
        } else {
            Some(SessionSupervisor::start(registries)?)
        };
        let application = Arc::new(HttpServer::unbound(drives, options.clone()));
        let handler = Arc::new(Router {
            handlers: self.handlers,
            local,
            remote: remote.as_ref().map(|(forwarder, ..)| forwarder.clone()),
            application: application.clone(),
        });
        let (peers, reloader) = match remote {
            Some((_, peers, reloader)) => (peers, reloader),
            None => (Vec::new(), None),
        };
        match HttpServer::start_with_handler(handler, options).await {
            Ok(listener) => Ok(Service {
                listener,
                owner: None,
                application: Some(application),
                supervisor,
                peers,
                reloader,
                drain_timeout,
            }),
            Err(error) => {
                let cleanup = if let Some(supervisor) = supervisor {
                    supervisor.shutdown().await
                } else {
                    Ok(())
                };
                application
                    .close()
                    .await
                    .map_err(|e| CliError::runtime(e.to_string()))?;
                cleanup?;
                Err(CliError::runtime(error.to_string()))
            }
        }
    }
}
struct Router {
    local: std::collections::BTreeSet<String>,
    remote: Option<mount_rs_cluster::IngressForwarder>,
    handlers: BTreeMap<String, SessionHttpHandler>,
    application: Arc<HttpServer>,
}
impl HttpRequestHandler for Router {
    fn handle(
        &self,
        request: http::Request<HttpBody>,
    ) -> Pin<Box<dyn std::future::Future<Output = http::Response<HttpBody>> + Send + '_>> {
        Box::pin(async move {
            let volume = request
                .uri()
                .path()
                .strip_prefix("/v1/drives/")
                .and_then(|path| path.split_once('/'))
                .map(|(volume, _)| volume);
            if let Some(remote) = &self.remote
                && volume.is_some_and(|volume| !self.local.contains(volume))
            {
                #[cfg(feature = "audit-forward-timing")]
                {
                    let mut request = request;
                    let trace = mount_rs_cluster::ForwardRequestTrace::default();
                    request.extensions_mut().insert(trace.clone());
                    let wall = || {
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_nanos() as u64)
                    };
                    let start_wall = wall();
                    let started = std::time::Instant::now();
                    let mut response = remote.handle(request).await;
                    response
                        .headers_mut()
                        .insert("x-mount-audit-ingress-start-wall-ns", start_wall.into());
                    response
                        .headers_mut()
                        .insert("x-mount-audit-ingress-end-wall-ns", wall().into());
                    let elapsed = started.elapsed().as_nanos() as u64;
                    response
                        .headers_mut()
                        .insert("x-mount-audit-ingress-ns", elapsed.into());
                    let timing = trace.snapshot();
                    for (key, value) in [
                        ("x-mount-audit-route-ns", timing.route_ns),
                        ("x-mount-audit-route-gate-ns", timing.route_lookup_wait_ns),
                        ("x-mount-audit-registry-ns", timing.registry_lookup_ns),
                        ("x-mount-audit-connection-ns", timing.connection_ns),
                        ("x-mount-audit-stream-ns", timing.stream_open_ns),
                        (
                            "x-mount-audit-owner-queued-ns",
                            timing.owner_stream_queued_ns,
                        ),
                        (
                            "x-mount-audit-owner-resolve-ns",
                            timing.owner_stream_resolve_ns,
                        ),
                        (
                            "x-mount-audit-owner-admission-ns",
                            timing.owner_admission_ns,
                        ),
                        (
                            "x-mount-audit-owner-application-ns",
                            timing.owner_application_ns,
                        ),
                    ] {
                        if let Some(value) = value {
                            response.headers_mut().insert(key, value.into());
                        }
                    }
                    if let Some(headers) = timing.headers_ns {
                        response
                            .headers_mut()
                            .insert("x-mount-audit-peer-headers-ns", headers.into());
                    }
                    return response;
                }
                #[cfg(not(feature = "audit-forward-timing"))]
                return remote.handle(request).await;
            }
            if let Some(handler) = volume.and_then(|volume| self.handlers.get(volume))
                && handler.matches(request.uri().path())
            {
                return handler.handle(request).await;
            }
            self.application.handle(request).await
        })
    }
}
pub(crate) struct Service {
    listener: HttpServer,
    owner: Option<OwnerService>,
    peers: Vec<Arc<mount_rs_cluster::transport::PeerClient>>,
    /// Reloads the trusted membership file while the service runs.
    reloader: Option<crate::membership_reload::MembershipReloader>,
    application: Option<Arc<HttpServer>>,
    supervisor: Option<SessionSupervisor>,
    drain_timeout: Duration,
}
impl Service {
    pub(crate) fn url(&self) -> String {
        self.listener.url()
    }
    /// Accepts that failed and were retried by the public listener.
    pub(crate) fn accept_failures(&self) -> u64 {
        self.listener.accept_failures()
    }
    /// The drives a failover candidate owns now; empty on any other server.
    pub(crate) fn failover_filesystems(&self) -> Vec<(String, Arc<Filesystem>)> {
        self.owner
            .as_ref()
            .and_then(|owner| owner.failover.as_ref())
            .map(mount_rs_cluster::FailoverSupervisor::filesystems)
            .unwrap_or_default()
    }
    pub(crate) fn failure(&self) -> Option<&'static str> {
        if self.owner.as_ref().is_some_and(OwnerService::failed) {
            Some("peer listener or volume registration failed")
        } else if self.listener.listener_failed() {
            Some("HTTP listener exited unexpectedly")
        } else if self
            .supervisor
            .as_ref()
            .is_some_and(SessionSupervisor::failed)
        {
            Some("HTTP session supervisor failed")
        } else {
            None
        }
    }
    /// Stop every part of the service in order: the public listener, then
    /// the owner's route and peer listener, the peer client, the sessions and
    /// the application. A step that fails does not skip the later ones, so a
    /// public drain that ran out of time still withdraws the Redis route and
    /// closes the sessions before the caller shuts the drives down. Returns
    /// the first failure.
    pub(crate) async fn close(&self) -> Result<(), CliError> {
        let mut first: Option<CliError> = None;
        let mut keep = |result: Result<(), CliError>| {
            let Err(error) = result else { return };
            first = Some(match first.take() {
                None => error,
                // A failure that left work running must stay one, whatever
                // failed before it: the process must not wait on that work.
                Some(earlier)
                    if error.abandons_running_work() && !earlier.abandons_running_work() =>
                {
                    CliError::abandoning(format!("{earlier}; {error}"))
                }
                Some(earlier) => earlier,
            });
        };
        // No membership reload may start once the service is closing.
        if let Some(reloader) = &self.reloader {
            keep(reloader.stop().await);
        }
        keep(
            self.listener
                .close()
                .await
                .map_err(|e| CliError::runtime(e.to_string())),
        );
        if let Some(owner) = &self.owner {
            keep(owner.close().await);
        }
        for peer in &self.peers {
            keep(
                peer.shutdown()
                    .await
                    .map_err(|e| CliError::runtime(e.to_string())),
            );
        }
        if let Some(supervisor) = &self.supervisor {
            keep(
                supervisor
                    .shutdown_with_timeout(self.drain_timeout)
                    .await
                    .map_err(Into::into),
            );
        }
        if let Some(application) = &self.application {
            keep(
                application
                    .close()
                    .await
                    .map_err(|e| CliError::runtime(e.to_string())),
            );
        }
        first.map_or(Ok(()), Err)
    }
}

/// Most connections one server opens to each peer
/// (`forwarding.peer_connections` is at most this).
const MAX_PEER_CONNECTIONS: usize = 16;

/// Room every forwarding server has for members by default, so that a
/// reload can add servers to a small cluster without a restart.
const DEFAULT_MEMBER_CAPACITY: usize = 64;

/// A forwarding client keeps one connection to each server it forwards to,
/// so it needs a connection and a destination slot per member. The limits
/// cannot grow while the process runs, so they are sized for the largest
/// membership a reload may install rather than for the one at startup.
fn client_limits(
    membership: &mount_rs_cluster::PeerMembership,
) -> mount_rs_cluster::transport::PeerTransportLimits {
    mount_rs_cluster::transport::PeerTransportLimits {
        max_connections: membership.capacity(),
        max_destinations: membership.capacity(),
        ..Default::default()
    }
}

/// An owner accepts up to `MAX_PEER_CONNECTIONS` connections from every
/// member, so that no peer_connections setting on the others can lock one
/// of them out. The transport default of 16 admitted only 16 in all.
fn server_limits(
    membership: &mount_rs_cluster::PeerMembership,
) -> mount_rs_cluster::transport::PeerTransportLimits {
    mount_rs_cluster::transport::PeerTransportLimits {
        max_connections: membership.capacity() * MAX_PEER_CONNECTIONS,
        ..Default::default()
    }
}

struct PeerFiles {
    /// The trusted membership, which a reload replaces while the server runs.
    membership: Arc<mount_rs_cluster::PeerMembership>,
    /// The membership file as read at startup.
    membership_file: Vec<u8>,
    certificate: Vec<u8>,
    key: Vec<u8>,
    ca: Vec<u8>,
}

impl PeerFiles {
    /// Read the peer files. `listener` is the identity of a server that
    /// also listens for peers (a volume owner); its record must be present,
    /// and a reload may not change what its listener and routes depend on.
    fn load(
        config: &crate::config::ForwardingConfig,
        listener: Option<&mount_rs_cluster::transport::TlsIdentity>,
    ) -> Result<Self, CliError> {
        use mount_rs_cluster::{PeerDirectory, PeerMembership, transport::StagedPeerIdentity};
        let read = |path: &std::path::Path| {
            std::fs::read(path)
                .map_err(|e| CliError::runtime(format!("cannot read {}: {e}", path.display())))
        };
        let fail = |e: &dyn std::fmt::Display| {
            CliError::runtime(format!(
                "HTTP forwarding startup: {}: {e}",
                config.peers.display()
            ))
        };
        let membership_file =
            crate::membership_reload::read_membership(&config.peers).map_err(CliError::runtime)?;
        let directory = PeerDirectory::parse(membership_file.as_slice()).map_err(|e| fail(&e))?;
        let certificate = read(&config.certificate)?;
        let key = read(&config.private_key)?;
        let ca = read(&config.ca)?;
        let capacity = config
            .max_members
            .unwrap_or_else(|| directory.len().max(DEFAULT_MEMBER_CAPACITY));
        let membership =
            PeerMembership::new(Arc::new(directory), capacity).map_err(|e| fail(&e))?;
        let membership = match listener {
            Some(identity) => membership
                .with_local_listener(identity.clone())
                .map_err(|_| {
                    CliError::usage("owner identity is absent from the configured peer directory")
                })?,
            None => {
                // An ingress dials out only, and has never had to be in its
                // own copy of the file to start. When it is, a reload must
                // not take it out: the owners read the same file and would
                // stop accepting it.
                let directory = membership.directory();
                match StagedPeerIdentity::from_pem(&certificate, &key, &ca) {
                    Ok(identity) if directory.accepts(identity.identity()) => membership
                        .with_local_identity(identity.identity().clone())
                        .map_err(|e| fail(&e))?,
                    _ => membership,
                }
            }
        };
        Ok(Self {
            membership: Arc::new(membership),
            membership_file,
            certificate,
            key,
            ca,
        })
    }

    /// Start reloading the membership file for `forwarder`.
    fn reloader(
        &self,
        config: &crate::config::ForwardingConfig,
        forwarder: &mount_rs_cluster::IngressForwarder,
    ) -> Result<Option<crate::membership_reload::MembershipReloader>, CliError> {
        let poll = (config.membership_reload_seconds > 0)
            .then(|| Duration::from_secs(config.membership_reload_seconds));
        // Without SIGHUP or a poll interval there is nothing to reload on.
        if cfg!(not(unix)) && poll.is_none() {
            return Ok(None);
        }
        crate::membership_reload::MembershipReloader::start(
            config.peers.clone(),
            self.membership_file.clone(),
            poll,
            forwarder.clone(),
        )
        .map(Some)
    }
}

async fn connect_forwarder(
    config: &crate::config::ForwardingConfig,
    max_request_bytes: usize,
    max_upload_bytes: u64,
    files: &PeerFiles,
) -> Result<
    (
        mount_rs_cluster::IngressForwarder,
        Vec<Arc<mount_rs_cluster::transport::PeerClient>>,
    ),
    CliError,
> {
    use mount_rs_cluster::{ForwardOptions, IngressForwarder, transport::*};
    let fail =
        |e: &dyn std::fmt::Display| CliError::runtime(format!("HTTP forwarding startup: {e}"));
    let membership = files.membership.clone();
    // One client per connection, each on its own UDP socket: the first on the
    // configured address, the rest on the same IP with a free port.
    let mut peers = Vec::with_capacity(config.peer_connections);
    for index in 0..config.peer_connections {
        let tls = load_peer_client_config_from_pem(&files.certificate, &files.key, &files.ca)
            .map_err(|e| fail(&e))?;
        let bind = if index == 0 {
            config.bind
        } else {
            std::net::SocketAddr::new(config.bind.ip(), 0)
        };
        let mut endpoint = quinn::Endpoint::client(bind).map_err(|e| fail(&e))?;
        endpoint.set_default_client_config(tls);
        peers.push(Arc::new(
            // The membership's own pin set, so that a reload reaches every
            // client: new handshakes are checked against the new pins and
            // connections whose pin has gone are closed.
            PeerClient::new_with_pin_provider(
                endpoint,
                membership.pins(),
                client_limits(&membership),
            )
            .map_err(|e| fail(&e))?,
        ));
    }
    let registry = crate::redis_connect::connect_registry(config).await?;
    let forwarder = IngressForwarder::with_membership(
        registry,
        membership,
        peers[0].clone(),
        ForwardOptions {
            max_request_bytes,
            max_upload_bytes,
            refresh_active_routes: config.refresh_active_routes,
            refresh_idle_ttls: config.route_refresh_idle_ttls,
            ..Default::default()
        },
    )
    .map_err(|e| fail(&e))?
    .with_additional_clients(peers[1..].to_vec());
    Ok((forwarder, peers))
}

struct OwnerService {
    /// Routes of the drives opened at startup; `None` on a failover
    /// candidate, whose drives come and go under `failover`.
    registrations: Option<mount_rs_cluster::RegistrationSupervisor>,
    failover: Option<mount_rs_cluster::FailoverSupervisor>,
    /// How long `close` waits for `failover` to release its drives.
    release_timeout: Duration,
    router: mount_rs_cluster::OwnerHttpRouter,
    stop: tokio_util::sync::CancellationToken,
    /// The peer listener. Its error text is reported by the first `close`.
    task: crate::joined::JoinedTask<String>,
}
impl OwnerService {
    fn failed(&self) -> bool {
        // The listener ending is a failure unless `close` asked it to end.
        // A failover candidate does not fail when one drive does: that drive
        // goes back to standby and the others are untouched.
        self.registrations
            .as_ref()
            .is_some_and(mount_rs_cluster::RegistrationSupervisor::failed)
            || self
                .failover
                .as_ref()
                .is_some_and(mount_rs_cluster::FailoverSupervisor::failed)
            || (self.task.is_finished() && !self.stop.is_cancelled())
    }
    async fn close(&self) -> Result<(), CliError> {
        let registration = match &self.registrations {
            Some(registrations) => registrations.shutdown().await,
            None => Ok(()),
        };
        // Withdraws each owned drive's route, drains it and releases its
        // writer lease, so a standby can take over at once. Storage that
        // has stopped answering must not keep the process from exiting: its
        // lease then expires by itself.
        let released = match &self.failover {
            Some(failover) => tokio::time::timeout(self.release_timeout, failover.shutdown())
                .await
                .is_ok(),
            None => true,
        };
        self.stop.cancel();
        self.task.join().await.map_err(CliError::runtime)?;
        self.router
            .close()
            .await
            .map_err(|e| CliError::runtime(e.to_string()))?;
        registration?;
        if !released {
            return Err(CliError::abandoning(format!(
                "failover drives were not released within {} ms; their writer leases are left to expire",
                self.release_timeout.as_millis()
            )));
        }
        Ok(())
    }
}

type OwnedDrive = (String, String, Arc<Filesystem>, Option<SessionCredentials>);

/// The drives of an owner.
pub(crate) enum OwnedDrives {
    /// Opened at startup and owned until the process stops.
    Open(Vec<OwnedDrive>),
    /// `forwarding.failover`: each is opened when this server takes it over
    /// and released when it loses or hands back the drive.
    Candidates {
        drives: Vec<mount_rs_cluster::FailoverDrive>,
        /// How long a stop waits for the drives owned then to be released.
        release_timeout: Duration,
    },
}

/// How long a published route and a failover announcement live in Redis.
const ROUTE_TTL: Duration = Duration::from_secs(15);

/// One JSON line on stderr per failover state change of a drive.
fn report_failover(event: &mount_rs_cluster::FailoverEvent) {
    use mount_rs_cluster::{FailoverEvent as Event, ReleaseReason};
    use std::io::Write;
    let record = match event {
        Event::Acquired { volume, fence } => {
            serde_json::json!({"drive":volume,"state":"acquired","fence":fence})
        }
        Event::Published { volume, fence } => {
            serde_json::json!({"drive":volume,"state":"published","fence":fence})
        }
        Event::Released {
            volume,
            fence,
            reason,
        } => {
            let reason = match reason {
                ReleaseReason::LeaseLost => "lease_lost",
                ReleaseReason::FailBack => "fail_back",
                ReleaseReason::Shutdown => "shutdown",
            };
            serde_json::json!({"drive":volume,"state":"released","fence":fence,"reason":reason})
        }
        Event::LeaseHeld { volume } => serde_json::json!({"drive":volume,"state":"lease_held"}),
        Event::OpenFailed { volume, error } => {
            serde_json::json!({"drive":volume,"state":"open_failed","error":error})
        }
        Event::RouteUnpublished { volume, error } => {
            serde_json::json!({"drive":volume,"state":"route_unpublished","error":error})
        }
        Event::ReleaseFailed { volume, error } => {
            serde_json::json!({"drive":volume,"state":"release_failed","error":error})
        }
    };
    let mut record = record;
    record["event"] = "drive_failover".into();
    let _ = writeln!(std::io::stderr().lock(), "{record}");
}

pub(crate) async fn start_owner(
    config: &crate::config::ForwardingConfig,
    drives: OwnedDrives,
    options: HttpServerOptions,
    session_limits: SessionLimitsConfig,
) -> Result<Service, CliError> {
    use mount_rs_cluster::{
        ClusterIngress, OwnedVolumeRegistration, OwnerHttpHandler, OwnerHttpRouter,
        RegistrationRecovery, RegistrationSupervisor, VolumeRoute, transport::*,
    };
    let fail = |e: &dyn std::fmt::Display| CliError::runtime(format!("peer owner startup: {e}"));
    let read = |path: &std::path::Path| {
        std::fs::read(path)
            .map_err(|e| CliError::runtime(format!("cannot read {}: {e}", path.display())))
    };
    let identity = StagedPeerIdentity::from_pem(
        &read(&config.certificate)?,
        &read(&config.private_key)?,
        &read(&config.ca)?,
    )
    .map_err(|e| fail(&e))?;
    let files = PeerFiles::load(config, Some(identity.identity()))?;
    let membership = files.membership.clone();
    // This server's own record. A reload cannot change the fields used
    // here, so the listener and the published routes stay valid.
    let member = membership
        .directory()
        .authenticated_member(identity.identity())
        .cloned()
        .ok_or_else(|| {
            CliError::usage("owner identity is absent from the configured peer directory")
        })?;
    let tls = load_peer_server_config_from_pem(&files.certificate, &files.key, &files.ca)
        .map_err(|e| fail(&e))?;
    let endpoint = quinn::Endpoint::server(tls, member.address).map_err(|e| fail(&e))?;
    let registry = crate::redis_connect::connect_registry(config).await?;
    let budget = mount_rs_cluster::SessionBudget::new(session_limits.max_sessions)?;
    let mut handlers = Vec::new();
    let mut registrations = Vec::new();
    let mut sessions = Vec::new();
    let (drives, candidates, release_timeout) = match drives {
        OwnedDrives::Open(drives) => (drives, None, Duration::ZERO),
        OwnedDrives::Candidates {
            drives,
            release_timeout,
        } => (Vec::new(), Some(drives), release_timeout),
    };
    for (volume, token, fs, credentials) in drives {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce)
            .map_err(|_| CliError::runtime("cannot generate owner registration ID"))?;
        let route = VolumeRoute {
            volume,
            node: member.node.clone(),
            boot: member.boot.clone(),
            address: member.address,
            tls_server_name: member.tls_server_name.clone(),
            fence: fs.checked_owner_fence().await?,
            registration: format!("{:032x}", u128::from_be_bytes(nonce)),
        };
        let registration =
            OwnedVolumeRegistration::new(fs.clone(), registry.clone(), route.clone(), ROUTE_TTL)
                .await
                .map_err(|e| CliError::runtime(format!("owner registration: {e:?}")))?;
        let mut handler = OwnerHttpHandler::new(fs, route, token, options.clone())
            .await
            .map_err(|e| fail(&e))?;
        if let Some(credentials) = credentials {
            handler = handler
                .with_sessions(
                    credentials,
                    session_limits.max_sessions_per_drive,
                    session_limits.max_handles_per_session,
                    Duration::from_secs(session_limits.idle_timeout_seconds),
                    budget.clone(),
                )
                .map_err(|e| fail(&e))?;
            sessions.extend(handler.session_registry());
        }
        registrations.push(Arc::new(registration));
        handlers.push(handler);
    }
    let router = OwnerHttpRouter::new(handlers).map_err(|e| fail(&e))?;
    let (remote, peers) = connect_forwarder(
        config,
        options.max_request_bytes,
        options.max_upload_bytes,
        &files,
    )
    .await?;
    let reloader = files.reloader(config, &remote)?;
    let public = Arc::new(ClusterIngress::new(router.clone(), remote));
    let limits = server_limits(&membership);
    // Both checks read the membership in force when they run: the pins at
    // every handshake and request, the policy at every request. A peer
    // removed or re-keyed by a reload is refused from then on.
    let pins = membership.pins();
    let members = membership.clone();
    let policy = move |identity: &TlsIdentity, _: &http::Request<()>| membership.accepts(identity);
    let server = PeerServer::new_with_pin_provider(endpoint, pins, limits, policy, router.clone())
        .map_err(|e| fail(&e))?;
    let supervisor = if sessions.is_empty() {
        None
    } else {
        Some(SessionSupervisor::start(sessions)?)
    };
    let stop = tokio_util::sync::CancellationToken::new();
    let serving = server.serve(stop.clone());
    let task = crate::joined::JoinedTask::spawn(
        async move { serving.await.map_err(|e| e.to_string()) },
        || "peer listener task ended without a result".to_owned(),
    );
    let (registrations, failover) = if let Some(candidates) = candidates {
        let timings = config.failover.expect("candidates come from failover");
        let options = mount_rs_cluster::FailoverOptions {
            timings: mount_rs_cluster::FailoverTimings {
                poll: Duration::from_millis(timings.poll_interval_ms),
                stagger: Duration::from_millis(timings.takeover_stagger_ms),
                fail_back_after: timings
                    .fail_back
                    .then(|| Duration::from_secs(timings.fail_back_after_seconds)),
                fail_back_hold_off: ROUTE_TTL,
            },
            route_ttl: ROUTE_TTL,
            node: member.node.clone(),
            boot: member.boot.clone(),
            address: member.address,
            tls_server_name: member.tls_server_name.clone(),
            http: options.clone(),
            max_sessions_per_drive: session_limits.max_sessions_per_drive,
            max_handles_per_session: session_limits.max_handles_per_session,
            session_idle_timeout: Duration::from_secs(session_limits.idle_timeout_seconds),
            session_budget: budget.clone(),
            // Redis only lists names; a candidate counts if the membership
            // file in force now has it.
            trusted: Arc::new(move |node| members.directory().member(node).is_some()),
            events: Arc::new(report_failover),
        };
        let started = mount_rs_cluster::FailoverSupervisor::start(
            registry.clone(),
            router.clone(),
            candidates,
            options,
        );
        let failover = match started {
            Ok(failover) => failover,
            Err(error) => {
                stop.cancel();
                let _ = task.join().await;
                return Err(fail(&error));
            }
        };
        // Let every drive make its first decision, so that a server which
        // can take its drives at once is serving them when it reports
        // ready. A drive whose lease is still held stays a standby.
        failover.settled(Duration::from_secs(15)).await;
        (None, Some(failover))
    } else {
        let startup = RegistrationSupervisor::start_ready(
            registrations,
            RegistrationRecovery {
                initial_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(2),
                // A Redis outage of any length must not stop this server: its
                // own drives do not depend on Redis.
                max_attempts: RegistrationRecovery::UNLIMITED_ATTEMPTS,
            },
            Duration::from_secs(10),
        )
        .await;
        match startup {
            Ok(registrations) => (Some(registrations), None),
            Err(error) => {
                stop.cancel();
                let _ = task.join().await;
                router.close().await.map_err(|e| fail(&e))?;
                return Err(error.into());
            }
        }
    };
    let owner = OwnerService {
        registrations,
        failover,
        release_timeout,
        router,
        stop,
        task,
    };
    let drain_timeout = options.drain_timeout;
    match HttpServer::start_with_handler(public, options).await {
        Ok(listener) => Ok(Service {
            listener,
            owner: Some(owner),
            peers,
            reloader,
            application: None,
            supervisor,
            drain_timeout,
        }),
        Err(error) => {
            owner.close().await?;
            Err(fail(&error))
        }
    }
}
