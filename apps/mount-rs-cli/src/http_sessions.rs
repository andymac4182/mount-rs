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
                supervisor: None,
                drain_timeout,
            });
        }
        let remote = match forwarding {
            Some(config) => {
                let files = PeerFiles::load(config)?;
                Some(
                    connect_forwarder(
                        config,
                        self.max_request_bytes,
                        options.max_upload_bytes,
                        &files,
                    )
                    .await?,
                )
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
            remote: remote.as_ref().map(|(forwarder, _)| forwarder.clone()),
            application: application.clone(),
        });
        match HttpServer::start_with_handler(handler, options).await {
            Ok(listener) => Ok(Service {
                listener,
                owner: None,
                application: Some(application),
                supervisor,
                peers: remote.map(|(_, peers)| peers).unwrap_or_default(),
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
        let mut first = None;
        let mut keep = |result: Result<(), CliError>| {
            if let Err(error) = result {
                first.get_or_insert(error);
            }
        };
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

/// A forwarding client keeps one connection to each server it forwards to,
/// so it needs a connection and a destination slot per member.
fn client_limits(
    directory: &mount_rs_cluster::PeerDirectory,
) -> mount_rs_cluster::transport::PeerTransportLimits {
    mount_rs_cluster::transport::PeerTransportLimits {
        max_connections: directory.len(),
        max_destinations: directory.len(),
        ..Default::default()
    }
}

/// An owner accepts up to `MAX_PEER_CONNECTIONS` connections from every
/// member, so that no peer_connections setting on the others can lock one
/// of them out. The transport default of 16 admitted only 16 in all.
fn server_limits(
    directory: &mount_rs_cluster::PeerDirectory,
) -> mount_rs_cluster::transport::PeerTransportLimits {
    mount_rs_cluster::transport::PeerTransportLimits {
        max_connections: directory.len() * MAX_PEER_CONNECTIONS,
        ..Default::default()
    }
}

struct PeerFiles {
    directory: Arc<mount_rs_cluster::PeerDirectory>,
    certificate: Vec<u8>,
    key: Vec<u8>,
    ca: Vec<u8>,
}

impl PeerFiles {
    fn load(config: &crate::config::ForwardingConfig) -> Result<Self, CliError> {
        let read = |path: &std::path::Path| {
            std::fs::read(path)
                .map_err(|e| CliError::runtime(format!("cannot read {}: {e}", path.display())))
        };
        let directory =
            mount_rs_cluster::PeerDirectory::from_reader(read(&config.peers)?.as_slice())
                .map_err(|e| CliError::runtime(format!("HTTP forwarding startup: {e}")))?;
        Ok(Self {
            directory: Arc::new(directory),
            certificate: read(&config.certificate)?,
            key: read(&config.private_key)?,
            ca: read(&config.ca)?,
        })
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
    let directory = files.directory.clone();
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
            PeerClient::new(
                endpoint,
                directory.approved_pins().map_err(|e| fail(&e))?,
                client_limits(&directory),
            )
            .map_err(|e| fail(&e))?,
        ));
    }
    let registry = connect_registry(config).await?;
    let forwarder = IngressForwarder::new(
        registry,
        directory,
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

async fn connect_registry(
    config: &crate::config::ForwardingConfig,
) -> Result<mount_rs_cluster::RedisRouteRegistry, CliError> {
    use mount_rs_cluster::RedisRouteRegistry;
    let url = std::env::var(&config.redis_url.name).map_err(|_| {
        CliError::runtime(format!(
            "missing environment variable {}",
            config.redis_url.name
        ))
    })?;
    let redis =
        redis::Client::open(url).map_err(|_| CliError::runtime("invalid forwarding Redis URL"))?;
    let manager = tokio::time::timeout(Duration::from_secs(5), redis.get_connection_manager())
        .await
        .map_err(|_| CliError::runtime("forwarding Redis connection timed out"))?
        .map_err(|_| CliError::runtime("forwarding Redis connection failed"))?;
    RedisRouteRegistry::with_manager(manager, config.namespace.clone(), Duration::from_secs(2))
        .map_err(|e| CliError::runtime(e.to_string()))
}

struct OwnerService {
    registrations: mount_rs_cluster::RegistrationSupervisor,
    router: mount_rs_cluster::OwnerHttpRouter,
    stop: tokio_util::sync::CancellationToken,
    /// The peer listener. Its error text is reported by the first `close`.
    task: crate::joined::JoinedTask<String>,
}
impl OwnerService {
    fn failed(&self) -> bool {
        // The listener ending is a failure unless `close` asked it to end.
        self.registrations.failed() || (self.task.is_finished() && !self.stop.is_cancelled())
    }
    async fn close(&self) -> Result<(), CliError> {
        let registration = self.registrations.shutdown().await;
        self.stop.cancel();
        self.task.join().await.map_err(CliError::runtime)?;
        self.router
            .close()
            .await
            .map_err(|e| CliError::runtime(e.to_string()))?;
        registration?;
        Ok(())
    }
}

type OwnedDrive = (String, String, Arc<Filesystem>, Option<SessionCredentials>);

pub(crate) async fn start_owner(
    config: &crate::config::ForwardingConfig,
    drives: Vec<OwnedDrive>,
    options: HttpServerOptions,
    session_limits: SessionLimitsConfig,
) -> Result<Service, CliError> {
    use mount_rs_cluster::{
        ClusterIngress, OwnedVolumeRegistration, OwnerHttpHandler, OwnerHttpRouter,
        RegistrationRecovery, RegistrationSupervisor, VolumeRoute, transport::*,
    };
    let fail = |e: &dyn std::fmt::Display| CliError::runtime(format!("peer owner startup: {e}"));
    let files = PeerFiles::load(config)?;
    let directory = files.directory.clone();
    let identity = StagedPeerIdentity::from_pem(&files.certificate, &files.key, &files.ca)
        .map_err(|e| fail(&e))?;
    let member = directory
        .authenticated_member(identity.identity())
        .ok_or_else(|| {
            CliError::usage("owner identity is absent from the configured peer directory")
        })?;
    let tls = load_peer_server_config_from_pem(&files.certificate, &files.key, &files.ca)
        .map_err(|e| fail(&e))?;
    let endpoint = quinn::Endpoint::server(tls, member.address).map_err(|e| fail(&e))?;
    let registry = connect_registry(config).await?;
    let budget = mount_rs_cluster::SessionBudget::new(session_limits.max_sessions)?;
    let mut handlers = Vec::new();
    let mut registrations = Vec::new();
    let mut sessions = Vec::new();
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
        let registration = OwnedVolumeRegistration::new(
            fs.clone(),
            registry.clone(),
            route.clone(),
            Duration::from_secs(15),
        )
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
    let public = Arc::new(ClusterIngress::new(router.clone(), remote));
    let pins = directory.approved_pins().map_err(|e| fail(&e))?;
    let limits = server_limits(&directory);
    let policy = move |identity: &TlsIdentity, _: &http::Request<()>| directory.accepts(identity);
    let server =
        PeerServer::new(endpoint, pins, limits, policy, router.clone()).map_err(|e| fail(&e))?;
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
    let startup = RegistrationSupervisor::start_ready(
        registrations,
        RegistrationRecovery {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            max_attempts: 10,
        },
        Duration::from_secs(10),
    )
    .await;
    let registrations = match startup {
        Ok(registrations) => registrations,
        Err(error) => {
            stop.cancel();
            let _ = task.join().await;
            router.close().await.map_err(|e| fail(&e))?;
            return Err(error.into());
        }
    };
    let owner = OwnerService {
        registrations,
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
