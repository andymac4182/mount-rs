//! CLI-owned session routing and expiry. SDK owners must outlive a successful drain.
use crate::runtime::CliError;
use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, SessionSupervisor, VolumeSessions};
use mount_rs_http::{DriveRegistry, HttpBody, HttpRequestHandler, HttpServer, HttpServerOptions};
use mount_rs_sdk::Filesystem;
use std::{collections::BTreeMap, pin::Pin, sync::Arc, time::Duration};

pub(crate) struct Sessions {
    handlers: BTreeMap<String, SessionHttpHandler>,
    budget: mount_rs_cluster::SessionBudget,
    max_request_bytes: usize,
}
impl Sessions {
    pub(crate) fn new(max_request_bytes: usize) -> Result<Self, CliError> {
        Ok(Self {
            handlers: BTreeMap::new(),
            budget: mount_rs_cluster::SessionBudget::new(4096)?,
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
            128,
            128,
            Duration::from_secs(60),
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
                peer: None,
                supervisor: None,
                drain_timeout,
            });
        }
        let remote = match forwarding {
            Some(config) => {
                let files = PeerFiles::load(config)?;
                Some(connect_forwarder(config, self.max_request_bytes, &files).await?)
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
                peer: remote.map(|(_, peer)| peer),
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
    peer: Option<Arc<mount_rs_cluster::transport::PeerClient>>,
    application: Option<Arc<HttpServer>>,
    supervisor: Option<SessionSupervisor>,
    drain_timeout: Duration,
}
impl Service {
    pub(crate) fn url(&self) -> String {
        self.listener.url()
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
    // Retain this value and the SDKs on failure/cancellation for retry. Never
    // release an owner merely because its listener has stopped accepting.
    pub(crate) async fn close(&self) -> Result<(), CliError> {
        self.listener
            .close()
            .await
            .map_err(|e| CliError::runtime(e.to_string()))?;
        if let Some(owner) = &self.owner {
            owner.close().await?;
        }
        if let Some(peer) = &self.peer {
            peer.shutdown()
                .await
                .map_err(|e| CliError::runtime(e.to_string()))?;
        }
        if let Some(supervisor) = &self.supervisor {
            supervisor.shutdown_with_timeout(self.drain_timeout).await?;
        }
        if let Some(application) = &self.application {
            application
                .close()
                .await
                .map_err(|e| CliError::runtime(e.to_string()))?;
        }
        Ok(())
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
    files: &PeerFiles,
) -> Result<
    (
        mount_rs_cluster::IngressForwarder,
        Arc<mount_rs_cluster::transport::PeerClient>,
    ),
    CliError,
> {
    use mount_rs_cluster::{ForwardOptions, IngressForwarder, transport::*};
    let fail =
        |e: &dyn std::fmt::Display| CliError::runtime(format!("HTTP forwarding startup: {e}"));
    let directory = files.directory.clone();
    let tls = load_peer_client_config_from_pem(&files.certificate, &files.key, &files.ca)
        .map_err(|e| fail(&e))?;
    let mut endpoint = quinn::Endpoint::client(config.bind).map_err(|e| fail(&e))?;
    endpoint.set_default_client_config(tls);
    let peer = Arc::new(
        PeerClient::new(
            endpoint,
            directory.approved_pins().map_err(|e| fail(&e))?,
            PeerTransportLimits::default(),
        )
        .map_err(|e| fail(&e))?,
    );
    let registry = connect_registry(config).await?;
    let forwarder = IngressForwarder::new(
        registry,
        directory,
        peer.clone(),
        ForwardOptions {
            max_request_bytes,
            refresh_active_routes: config.refresh_active_routes,
            refresh_idle_ttls: config.route_refresh_idle_ttls,
            ..Default::default()
        },
    )
    .map_err(|e| fail(&e))?;
    Ok((forwarder, peer))
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
    task: tokio::sync::Mutex<
        Option<
            tokio::task::JoinHandle<Result<(), mount_rs_cluster::transport::PeerTransportError>>,
        >,
    >,
}
impl OwnerService {
    fn failed(&self) -> bool {
        self.registrations.failed()
            || self
                .task
                .try_lock()
                .is_ok_and(|task| task.as_ref().is_some_and(|task| task.is_finished()))
    }
    async fn close(&self) -> Result<(), CliError> {
        let registration = self.registrations.shutdown().await;
        self.stop.cancel();
        let mut task = self.task.lock().await;
        if let Some(worker) = task.as_mut() {
            let result = worker.await;
            task.take();
            result
                .map_err(|e| CliError::runtime(e.to_string()))?
                .map_err(|e| CliError::runtime(e.to_string()))?;
        }
        drop(task);
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
    let budget = mount_rs_cluster::SessionBudget::new(4096)?;
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
                    128,
                    128,
                    Duration::from_secs(60),
                    budget.clone(),
                )
                .map_err(|e| fail(&e))?;
            sessions.extend(handler.session_registry());
        }
        registrations.push(Arc::new(registration));
        handlers.push(handler);
    }
    let router = OwnerHttpRouter::new(handlers).map_err(|e| fail(&e))?;
    let (remote, peer) = connect_forwarder(config, options.max_request_bytes, &files).await?;
    let public = Arc::new(ClusterIngress::new(router.clone(), remote));
    let pins = directory.approved_pins().map_err(|e| fail(&e))?;
    let policy = move |identity: &TlsIdentity, _: &http::Request<()>| directory.accepts(identity);
    let server = PeerServer::new(
        endpoint,
        pins,
        PeerTransportLimits::default(),
        policy,
        router.clone(),
    )
    .map_err(|e| fail(&e))?;
    let supervisor = if sessions.is_empty() {
        None
    } else {
        Some(SessionSupervisor::start(sessions)?)
    };
    let stop = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(server.serve(stop.clone()));
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
            let _ = task.await;
            router.close().await.map_err(|e| fail(&e))?;
            return Err(error.into());
        }
    };
    let owner = OwnerService {
        registrations,
        router,
        stop,
        task: tokio::sync::Mutex::new(Some(task)),
    };
    let drain_timeout = options.drain_timeout;
    match HttpServer::start_with_handler(public, options).await {
        Ok(listener) => Ok(Service {
            listener,
            owner: Some(owner),
            peer: Some(peer),
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
