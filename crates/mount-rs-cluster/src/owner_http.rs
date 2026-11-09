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
use mount_rs_http::{BoxError, DriveConfig, DriveRegistry, HttpServer, HttpServerOptions};
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
        route.encode()?;
        if filesystem.checked_owner_fence().await.ok() != Some(route.fence) {
            return Err(RegistryError::InvalidInput);
        }
        let mut registry = DriveRegistry::new();
        registry
            .register(
                DriveConfig::new(route.volume.clone(), filesystem.driver(), bearer_token)
                    .map_err(|_| RegistryError::InvalidInput)?,
            )
            .map_err(|_| RegistryError::InvalidInput)?;
        Ok(Self {
            filesystem,
            route,
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
            return handler
                .serve(request, self.session_operations.as_deref())
                .await;
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
        let headers_start = Instant::now();
        send.0
            .as_mut()
            .unwrap()
            .send_response(Response::from_parts(parts, ()))
            .await?;
        counters.headers.record(headers_start);
        let mut read_time = Duration::ZERO;
        let mut send_time = Duration::ZERO;
        loop {
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
#[derive(Clone)]
pub struct OwnerHttpRouter {
    owners: Arc<std::collections::BTreeMap<String, OwnerHttpHandler>>,
}
impl OwnerHttpRouter {
    pub fn new(owners: Vec<OwnerHttpHandler>) -> Result<Self, RegistryError> {
        if owners.len() > 4096 {
            return Err(RegistryError::InvalidInput);
        }
        let mut map = std::collections::BTreeMap::new();
        for owner in owners {
            if map.insert(owner.route.volume.clone(), owner).is_some() {
                return Err(RegistryError::InvalidInput);
            }
        }
        Ok(Self {
            owners: Arc::new(map),
        })
    }
    pub async fn close(&self) -> Result<(), mount_rs_http::HttpServerError> {
        let mut failure = None;
        for owner in self.owners.values() {
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
    pub(crate) fn local(&self, volume: &str) -> Option<&OwnerHttpHandler> {
        self.owners.get(volume)
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
        if let Some(owner) = route
            .as_ref()
            .and_then(|r| self.owners.get(&r.volume))
            .cloned()
        {
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
