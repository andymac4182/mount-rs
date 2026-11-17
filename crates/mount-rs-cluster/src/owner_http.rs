//! One-hop HTTP/3 owner adapter for the existing filesystem HTTP application.
use crate::forwarding::{ForwardStageTiming, StageCounter};
use crate::request_class::RequestClass;
use crate::{
    RegistryError, SessionBudget, SessionCredentials, SessionHttpHandler, VolumeRoute,
    VolumeSessions, transport::*,
};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use mount_rs_http::{
    BoxError, DriveConfig, DriveRegistry, DriveTokens, HttpServer, HttpServerOptions,
};
use mount_rs_sdk::Filesystem;
use std::sync::Arc;
use tokio::time::{Duration, Instant};

/// Owner-local completed-stage totals; no request/volume identifiers retained.
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct OwnerTimings {
    pub admission: ForwardStageTiming,
    pub application: ForwardStageTiming,
    pub headers: ForwardStageTiming,
    /// Sum of body polling time per fully produced response, including final EOF.
    pub body_read: ForwardStageTiming,
    /// Sum of send_chunked time per fully produced response (not remote delivery).
    pub body_send: ForwardStageTiming,
    pub finish: ForwardStageTiming,
}
#[derive(Default)]
struct OwnerCounters {
    admission: StageCounter,
    application: StageCounter,
    headers: StageCounter,
    body_read: StageCounter,
    body_send: StageCounter,
    finish: StageCounter,
}
impl OwnerCounters {
    fn snapshot(&self) -> OwnerTimings {
        OwnerTimings {
            admission: self.admission.snapshot(),
            application: self.application.snapshot(),
            headers: self.headers.snapshot(),
            body_read: self.body_read.snapshot(),
            body_send: self.body_send.snapshot(),
            finish: self.finish.snapshot(),
        }
    }
}
/// Successful decoded session operations, from dispatch through response construction.
#[derive(Default)]
pub(crate) struct SessionOperationCounters {
    pub(crate) stat: StageCounter,
    pub(crate) read: StageCounter,
    pub(crate) write: StageCounter,
    pub(crate) chmod: StageCounter,
}
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct SessionOperationTimings {
    pub stat: ForwardStageTiming,
    pub read: ForwardStageTiming,
    pub write: ForwardStageTiming,
    pub chmod: ForwardStageTiming,
}
#[derive(Default)]
struct OwnerDiagnostics {
    reads: OwnerCounters,
    mutations: OwnerCounters,
    session_rpc: OwnerCounters,
}
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct OwnerTimingSnapshot {
    pub reads: OwnerTimings,
    pub mutations: OwnerTimings,
    /// Session lifecycle and handle RPC; not necessarily read-only.
    pub session_rpc: OwnerTimings,
    pub session_operations: Option<SessionOperationTimings>,
}

pub const ROUTE_HEADER: &str = "x-mount-owner-route";
pub const HOP_HEADER: &str = "x-mount-hop";

/// Serve exactly one owned volume over an authenticated peer connection. The
/// surrounding PeerServer must enforce membership; ordinary drive bearer
/// authorization is still enforced by the HTTP application on every request.
#[derive(Clone)]
pub struct OwnerHttpHandler {
    filesystem: Arc<Filesystem>,
    route: VolumeRoute,
    /// The drive as `application` serves it, for the answers that are about
    /// every drive of this server: readiness and discovery.
    drive: DriveConfig,
    application: Arc<HttpServer>,
    sessions: Option<Arc<SessionHttpHandler>>,
    max_request_bytes: usize,
    diagnostics: Arc<OwnerDiagnostics>,
    session_operations: Option<Arc<SessionOperationCounters>>,
}
impl OwnerHttpHandler {
    pub async fn new(
        filesystem: Arc<Filesystem>,
        route: VolumeRoute,
        bearer_token: String,
        options: HttpServerOptions,
    ) -> Result<Self, RegistryError> {
        let tokens =
            DriveTokens::new(vec![bearer_token]).map_err(|_| RegistryError::InvalidInput)?;
        Self::with_tokens(filesystem, route, tokens, options).await
    }
    /// As [`new`](Self::new), for a drive whose accepted tokens may be
    /// replaced while it is served: keep a clone of `tokens` and
    /// [`DriveTokens::replace`] the set. A request, forwarded or not, is
    /// checked against the set in force when it reaches this owner.
    pub async fn with_tokens(
        filesystem: Arc<Filesystem>,
        route: VolumeRoute,
        tokens: DriveTokens,
        options: HttpServerOptions,
    ) -> Result<Self, RegistryError> {
        route.encode()?;
        if filesystem.checked_owner_fence().await.ok() != Some(route.fence) {
            return Err(RegistryError::InvalidInput);
        }
        let drive = DriveConfig::with_tokens(route.volume.clone(), filesystem.driver(), tokens)
            .map_err(|_| RegistryError::InvalidInput)?;
        let mut registry = DriveRegistry::new();
        registry
            .register(drive.clone())
            .map_err(|_| RegistryError::InvalidInput)?;
        Ok(Self {
            filesystem,
            route,
            drive,
            max_request_bytes: options.max_request_bytes.max(1),
            application: Arc::new(HttpServer::unbound(registry, options)),
            sessions: None,
            diagnostics: Arc::default(),
            session_operations: None,
        })
    }
    /// Enable per-owner timers for stat, content read and chmod session calls.
    pub fn with_session_operation_timings(mut self) -> Self {
        self.session_operations = Some(Arc::default());
        self
    }
    /// Enable authenticated session lifecycle routes on this exact owned
    /// filesystem. The server must own the registry expiry worker and drain
    /// both public and peer listeners before calling `close`.
    pub fn with_sessions(
        mut self,
        credentials: SessionCredentials,
        max_sessions: usize,
        handles_per_session: usize,
        idle_timeout: Duration,
        budget: SessionBudget,
    ) -> Result<Self, RegistryError> {
        // Never replace a live registry and orphan its retained handles.
        if self.sessions.is_some() {
            return Err(RegistryError::InvalidInput);
        }
        let registry = Arc::new(
            VolumeSessions::new_with_budget(
                self.filesystem.clone(),
                max_sessions,
                handles_per_session,
                idle_timeout,
                budget,
            )
            .map_err(|_| RegistryError::InvalidInput)?,
        );
        self.sessions = Some(Arc::new(
            SessionHttpHandler::new(&self.route.volume, credentials, registry)
                .map_err(|_| RegistryError::InvalidInput)?
                .with_max_request_bytes(self.max_request_bytes)
                .map_err(|_| RegistryError::InvalidInput)?,
        ));
        Ok(self)
    }
    /// The route this handler serves; forwarded requests must carry it.
    pub fn route(&self) -> &VolumeRoute {
        &self.route
    }
    pub fn session_registry(&self) -> Option<Arc<VolumeSessions>> {
        self.sessions.as_ref().map(|handler| handler.registry())
    }
    async fn dispatch(
        &self,
        request: Request<mount_rs_http::HttpBody>,
    ) -> Response<mount_rs_http::HttpBody> {
        if let Some(handler) = &self.sessions
            && handler.matches(request.uri().path())
        {
            // Through the application, so that a session request another
            // server forwarded here is in this server's access log and
            // metrics as a forwarded file request is.
            let sessions = TimedSessions {
                handler: handler.clone(),
                timings: self.session_operations.clone(),
            };
            return self.application.handle_observed(&sessions, request).await;
        }
        self.application.handle(request).await
    }
    /// Drain the peer server first, then close this application before the SDK.
    pub async fn close(&self) -> Result<(), mount_rs_http::HttpServerError> {
        self.application.close().await?;
        if let Some(registry) = self.session_registry() {
            registry.close().await.map_err(|error| {
                mount_rs_http::HttpServerError::Io(std::io::Error::other(format!(
                    "session drain failed: {}",
                    error.code.as_str()
                )))
            })?;
        }
        Ok(())
    }
    async fn serve(
        &self,
        mut request: Request<()>,
        stream: PeerServerStream,
        route: Option<VolumeRoute>,
    ) -> Result<(), PeerTransportError> {
        let counters = match RequestClass::classify(&request) {
            RequestClass::Read => &self.diagnostics.reads,
            RequestClass::Mutation => &self.diagnostics.mutations,
            RequestClass::SessionRpc => &self.diagnostics.session_rpc,
        };
        let admission_start = Instant::now();
        let start_wall = std::time::SystemTime::now();
        let valid_route = request.headers().get_all(ROUTE_HEADER).iter().count() == 1
            && route.as_ref() == Some(&self.route);
        let one_hop = request.headers().get_all(HOP_HEADER).iter().count() == 1
            && request.headers().get(HOP_HEADER).is_some_and(|v| v == "1");
        let (send, recv) = stream.split();
        let mut send = CancelSend(Some(send));
        let recv = CancelRecv(recv);
        let rejection = if !one_hop || !valid_route {
            Some(StatusCode::CONFLICT)
        } else if self.filesystem.checked_owner_fence().await.ok() != Some(self.route.fence) {
            Some(StatusCode::SERVICE_UNAVAILABLE)
        } else {
            None
        };
        if let Some(status) = rejection {
            send.0
                .as_mut()
                .unwrap()
                .send_response(
                    Response::builder()
                        .status(status)
                        .header("x-mount-dispatch", "not-dispatched")
                        .body(())
                        .unwrap(),
                )
                .await?;
            send.0.as_mut().unwrap().finish().await?;
            send.0.take();
            return Ok(());
        }
        let traced = request
            .headers_mut()
            .remove("x-mount-peer-timing")
            .is_some();
        let admission = admission_start.elapsed();
        counters.admission.record_duration(admission);
        request.headers_mut().remove(ROUTE_HEADER);
        request.headers_mut().remove(HOP_HEADER);
        let body = StreamBody::new(futures_util::stream::try_unfold(
            recv,
            |mut recv| async move {
                match recv.0.recv_chunk().await {
                    Ok(Some(chunk)) => Ok(Some((Frame::data(chunk.into_bytes()), recv))),
                    Ok(None) => Ok(None),
                    Err(error) => Err(Box::new(error) as BoxError),
                }
            },
        ))
        .boxed_unsync();
        let transport_timing = request.extensions_mut().remove::<PeerStreamTiming>();
        let is_head = request.method() == http::Method::HEAD;
        let application_start = Instant::now();
        let response = self.dispatch(request.map(|_| body)).await;
        let application = application_start.elapsed();
        counters.application.record_duration(application);
        let (mut parts, mut body) = response.into_parts();
        if traced {
            if let Some(timing) = transport_timing {
                parts.headers.insert(
                    "x-mount-owner-stream-queued-ns",
                    (timing.queued.as_nanos() as u64).into(),
                );
                parts.headers.insert(
                    "x-mount-owner-stream-resolve-ns",
                    (timing.resolve.as_nanos() as u64).into(),
                );
            }
            parts.headers.insert(
                "x-mount-owner-application-ns",
                (application.as_nanos() as u64).into(),
            );
            parts.headers.insert(
                "x-mount-owner-admission-ns",
                (admission.as_nanos() as u64).into(),
            );
            let unix = |t: std::time::SystemTime| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as u64)
            };
            parts
                .headers
                .insert("x-mount-audit-owner-start-wall-ns", unix(start_wall).into());
            parts.headers.insert(
                "x-mount-audit-owner-respond-wall-ns",
                unix(std::time::SystemTime::now()).into(),
            );
        }
        // HTTP/1 connection management headers have no meaning on HTTP/3.
        parts.headers.remove("connection");
        parts.headers.remove("transfer-encoding");
        // A listener states the length of a body it holds in full. Say it
        // here too, so that the ingress can give its client the length
        // the owner's own listener would have sent.
        if !is_head
            && parts.status != StatusCode::NO_CONTENT
            && parts.status != StatusCode::NOT_MODIFIED
            && !parts.headers.contains_key(http::header::CONTENT_LENGTH)
            && let Some(length) = hyper::body::Body::size_hint(&body).exact()
        {
            parts
                .headers
                .insert(http::header::CONTENT_LENGTH, length.into());
        }
        let headers_start = Instant::now();
        send.0
            .as_mut()
            .unwrap()
            .send_response(Response::from_parts(parts, ()))
            .await?;
        counters.headers.record(headers_start);
        let mut read_time = Duration::ZERO;
        let mut send_time = Duration::ZERO;
        // A response to HEAD has no body. The application still builds one
        // for an error (the listener's HTTP/1 writer drops it); sending it
        // to the ingress would put bytes where none are allowed, and the
        // ingress would report the answer as lost.
        loop {
            if is_head {
                break;
            }
            let read_start = Instant::now();
            let frame = body.frame().await;
            read_time += read_start.elapsed();
            let Some(frame) = frame else {
                break;
            };
            let frame = frame.map_err(|_| {
                PeerTransportError::H3("filesystem response body failed after dispatch".into())
            })?;
            if let Ok(bytes) = frame.into_data() {
                let send_start = Instant::now();
                send.0.as_mut().unwrap().send_chunked(&bytes).await?;
                send_time += send_start.elapsed();
            }
        }
        counters.body_read.record_duration(read_time);
        counters.body_send.record_duration(send_time);
        let finish_start = Instant::now();
        send.0.as_mut().unwrap().finish().await?;
        counters.finish.record(finish_start);
        send.0.take();
        Ok(())
    }
}
/// The session routes of one owner, with its operation timers.
struct TimedSessions {
    handler: Arc<SessionHttpHandler>,
    timings: Option<Arc<SessionOperationCounters>>,
}
impl mount_rs_http::HttpRequestHandler for TimedSessions {
    fn handle(
        &self,
        request: Request<mount_rs_http::HttpBody>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Response<mount_rs_http::HttpBody>> + Send + '_>>
    {
        Box::pin(self.handler.serve(request, self.timings.as_deref()))
    }
}
fn request_route(request: &Request<()>) -> Option<VolumeRoute> {
    let header = request.headers().get(ROUTE_HEADER)?.to_str().ok()?;
    if header.len() > 4096 {
        return None;
    }
    serde_json::from_str(header).ok()
}

impl PeerRequestHandler for OwnerHttpHandler {
    fn handle(
        &self,
        _identity: TlsIdentity,
        request: Request<()>,
        stream: PeerServerStream,
    ) -> PeerHandlerFuture {
        let handler = self.clone();
        Box::pin(async move {
            let route = request_route(&request);
            handler.serve(request, stream, route).await
        })
    }
}
struct CancelRecv(PeerServerRecv);
impl Drop for CancelRecv {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct CancelSend(Option<PeerServerSend>);
impl Drop for CancelSend {
    fn drop(&mut self) {
        if let Some(send) = self.0.as_mut() {
            send.cancel();
        }
    }
}

/// Bounded multi-volume dispatch on one Mount Server's private peer listener.
///
/// The set of volumes may change while the server runs (`insert`, `remove`):
/// a failover candidate serves a volume only while it holds its writer
/// lease. Requests read an immutable snapshot and take no lock. A change
/// copies the snapshot and swaps the copy in only if the snapshot is still
/// the current one, deciding again if it is not, so two changes at once
/// both take effect and neither waits.
#[derive(Clone)]
pub struct OwnerHttpRouter {
    owners: Arc<arc_swap::ArcSwap<Owners>>,
}
type Owners = std::collections::BTreeMap<String, Arc<OwnerHttpHandler>>;
const MAX_OWNED_VOLUMES: usize = 4096;
impl OwnerHttpRouter {
    pub fn new(owners: Vec<OwnerHttpHandler>) -> Result<Self, RegistryError> {
        if owners.len() > MAX_OWNED_VOLUMES {
            return Err(RegistryError::InvalidInput);
        }
        let mut map = Owners::new();
        for owner in owners {
            if map
                .insert(owner.route.volume.clone(), Arc::new(owner))
                .is_some()
            {
                return Err(RegistryError::InvalidInput);
            }
        }
        Ok(Self {
            owners: Arc::new(arc_swap::ArcSwap::from_pointee(map)),
        })
    }
    /// Start serving `owner`'s volume. Refused if the volume is already
    /// served here: remove and close the old handler first.
    pub fn insert(&self, owner: OwnerHttpHandler) -> Result<(), RegistryError> {
        let owner = Arc::new(owner);
        loop {
            let current = self.owners.load_full();
            if current.len() >= MAX_OWNED_VOLUMES || current.contains_key(&owner.route.volume) {
                return Err(RegistryError::InvalidInput);
            }
            let mut next = Owners::clone(&current);
            next.insert(owner.route.volume.clone(), Arc::clone(&owner));
            if self.swap_in(&current, next) {
                return Ok(());
            }
        }
    }
    /// Make `next` the served set if `current` still is; `false` if another
    /// change got in first, in which case nothing was changed.
    fn swap_in(&self, current: &Arc<Owners>, next: Owners) -> bool {
        let previous = self.owners.compare_and_swap(current, Arc::new(next));
        Arc::ptr_eq(&previous, current)
    }
    /// Stop routing new requests to `volume` and return its handler, which
    /// the caller must `close` before shutting the filesystem down. A
    /// request that already holds the handler finishes or is refused by it.
    ///
    /// Of two removals of one volume, exactly one is given the handler.
    pub fn remove(&self, volume: &str) -> Option<Arc<OwnerHttpHandler>> {
        loop {
            let current = self.owners.load_full();
            let removed = current.get(volume)?.clone();
            let mut next = Owners::clone(&current);
            next.remove(volume);
            if self.swap_in(&current, next) {
                return Some(removed);
            }
        }
    }
    /// The volumes served here now.
    pub fn volumes(&self) -> Vec<String> {
        self.owners.load().keys().cloned().collect()
    }
    pub async fn close(&self) -> Result<(), mount_rs_http::HttpServerError> {
        let mut failure = None;
        let owners = self.owners.load_full();
        for owner in owners.values() {
            if let Err(error) = owner.close().await {
                failure.get_or_insert(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    /// Bounded per-owner counters, without volume names or endpoint information.
    pub fn timing_snapshots(&self) -> Vec<OwnerTimingSnapshot> {
        self.owners
            .load()
            .values()
            .map(|owner| OwnerTimingSnapshot {
                reads: owner.diagnostics.reads.snapshot(),
                mutations: owner.diagnostics.mutations.snapshot(),
                session_rpc: owner.diagnostics.session_rpc.snapshot(),
                session_operations: owner.session_operations.as_ref().map(|c| {
                    SessionOperationTimings {
                        stat: c.stat.snapshot(),
                        read: c.read.snapshot(),
                        write: c.write.snapshot(),
                        chmod: c.chmod.snapshot(),
                    }
                }),
            })
            .collect()
    }
    pub(crate) fn local(&self, volume: &str) -> Option<Arc<OwnerHttpHandler>> {
        self.owners.load().get(volume).cloned()
    }
    /// Every drive served here now, as one registry.
    pub fn drives(&self) -> DriveRegistry {
        let mut drives = DriveRegistry::new();
        for owner in self.owners.load().values() {
            // Volume names are the keys of the map, so none repeats.
            let _ = drives.register(owner.drive.clone());
        }
        drives
    }
}
impl PeerRequestHandler for OwnerHttpRouter {
    fn handle(
        &self,
        _identity: TlsIdentity,
        request: Request<()>,
        mut stream: PeerServerStream,
    ) -> PeerHandlerFuture {
        let route = request_route(&request);
        if let Some(owner) = route.as_ref().and_then(|r| self.local(&r.volume)) {
            return Box::pin(async move { owner.serve(request, stream, route).await });
        }
        Box::pin(async move {
            stream
                .send_response(
                    Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .header("x-mount-dispatch", "not-dispatched")
                        .body(())
                        .unwrap(),
                )
                .await?;
            stream.finish().await
        })
    }
}
impl OwnerHttpHandler {
    pub(crate) async fn local(
        &self,
        request: Request<mount_rs_http::HttpBody>,
    ) -> Response<mount_rs_http::HttpBody> {
        if self.filesystem.checked_owner_fence().await.ok() != Some(self.route.fence) {
            let body = http_body_util::Full::new(hyper::body::Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync();
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header("x-mount-dispatch", "not-dispatched")
                .body(body)
                .unwrap();
        }
        self.dispatch(request).await
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    fn request(token: &str) -> Request<mount_rs_http::HttpBody> {
        Request::builder()
            .method("POST")
            .uri("/v1/drives/volume/sessions")
            .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(
                http_body_util::Full::new(hyper::body::Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap()
    }
    async fn owner() -> (Arc<Filesystem>, OwnerHttpHandler, String) {
        owner_with_options(HttpServerOptions::default()).await
    }
    async fn owner_with_options(
        options: HttpServerOptions,
    ) -> (Arc<Filesystem>, OwnerHttpHandler, String) {
        let fs = Arc::new(
            Filesystem::split(mount_rs_sdk::SplitOptions::memory("owner-http", 4096))
                .await
                .unwrap(),
        );
        let route = VolumeRoute {
            volume: "volume".into(),
            node: "node".into(),
            boot: "boot".into(),
            registration: "registration".into(),
            fence: fs.checked_owner_fence().await.unwrap(),
            address: "127.0.0.1:9999".parse().unwrap(),
            tls_server_name: "owner.test".into(),
        };
        let token = "a".repeat(32);
        let handler =
            OwnerHttpHandler::new(fs.clone(), route, "ordinary-file-token".into(), options)
                .await
                .unwrap()
                .with_sessions(
                    SessionCredentials::new(vec![("alice".into(), token.clone())]).unwrap(),
                    2,
                    2,
                    Duration::from_secs(60),
                    SessionBudget::new(2).unwrap(),
                )
                .unwrap();
        (fs, handler, token)
    }
    /// Changes from several threads at once: each insert and remove of a
    /// thread's own volumes takes effect, one of the inserts of a shared
    /// volume wins, and its handler is given to one of the removals.
    #[tokio::test]
    async fn changes_to_the_router_at_once_all_take_effect_and_a_volume_has_one_owner() {
        const THREADS: usize = 8;
        const VOLUMES: usize = 40;
        let (_fs, handler, _token) = owner().await;
        let named = |volume: String| {
            let mut handler = handler.clone();
            handler.route.volume = volume;
            handler
        };
        let router = OwnerHttpRouter::new(Vec::new()).unwrap();
        let (inserted, removed): (usize, usize) = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..THREADS)
                .map(|thread| {
                    let (router, named) = (&router, &named);
                    scope.spawn(move || {
                        let mut shared = (0, 0);
                        for volume in 0..VOLUMES {
                            router
                                .insert(named(format!("{thread}-{volume}")))
                                .expect("a volume of this thread's own");
                            shared.0 += usize::from(router.insert(named("shared".into())).is_ok());
                            if volume % 2 == 1 {
                                let own = router
                                    .remove(&format!("{thread}-{volume}"))
                                    .expect("this thread's own volume");
                                assert_eq!(own.route.volume, format!("{thread}-{volume}"));
                            }
                            shared.1 += usize::from(router.remove("shared").is_some());
                        }
                        shared
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().expect("a thread"))
                .fold((0, 0), |sum, each| (sum.0 + each.0, sum.1 + each.1))
        });
        // Every handler of the shared volume that went in came out once,
        // or is the one still there.
        let left = usize::from(router.remove("shared").is_some());
        assert!(inserted >= 1);
        assert_eq!(inserted, removed + left);
        let mut expected: Vec<String> = (0..THREADS)
            .flat_map(|thread| {
                (0..VOLUMES)
                    .step_by(2)
                    .map(move |volume| format!("{thread}-{volume}"))
            })
            .collect();
        expected.sort();
        assert_eq!(router.volumes(), expected);
        assert!(router.insert(named("0-0".into())).is_err());
    }
    #[tokio::test]
    async fn owner_dispatch_authenticates_sessions_and_drains_retained_handles() {
        let (fs, handler, token) = owner().await;
        assert_eq!(
            handler.local(request("wrong")).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let created = handler.local(request(&token)).await;
        assert_eq!(created.status(), StatusCode::CREATED);
        let payload: serde_json::Value =
            serde_json::from_slice(&created.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let registry = handler.session_registry().unwrap();
        let token = crate::SessionToken::from_wire(payload["session"].as_str().unwrap()).unwrap();
        let session = registry.lookup("alice", &token).unwrap();
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        handler.close().await.unwrap();
        assert!(registry.lookup("alice", &token).is_err());
        assert_eq!(
            session.stat(id).await.unwrap_err().code,
            mount_rs_core::ErrorCode::Ebadf
        );
        fs.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn owner_session_uploads_respect_application_body_limit() {
        let (fs, handler, secret) = owner_with_options(HttpServerOptions {
            max_request_bytes: 128,
            ..Default::default()
        })
        .await;
        let registry = handler.session_registry().unwrap();
        let token = registry.create("alice").await.unwrap();
        let session = registry.lookup("alice", &token).unwrap();
        let handle = session.open("/limited", "w+", 0o600).await.unwrap();
        let request = Request::builder()
            .method("PUT")
            .uri("/v1/drives/volume/sessions/handles")
            .header(http::header::AUTHORIZATION, format!("Bearer {secret}"))
            .header(crate::SESSION_HEADER, token.to_wire())
            .header("x-mount-handle", handle)
            .header("x-mount-position", 0)
            .body(
                http_body_util::Full::new(hyper::body::Bytes::from(vec![1; 129]))
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        assert_eq!(
            handler.local(request).await.status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(session.stat(handle).await.unwrap().size, 0);
        handler.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn stale_owner_rejects_session_creation_before_dispatch() {
        let (fs, handler, token) = owner().await;
        fs.shutdown().await.unwrap();
        let response = handler.local(request(&token)).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["x-mount-dispatch"], "not-dispatched");
        handler.close().await.unwrap();
    }
}
