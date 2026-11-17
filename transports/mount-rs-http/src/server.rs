use mount_rs_core::turn_gate::{TurnLock, TurnLockGuard};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use arc_swap::ArcSwap;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::body::{Body as HttpBodyTrait, Frame, Incoming, SizeHint};
use hyper::header;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};

use crate::conditional::{self, Current, Outcome};
use mount_rs_core::{
    ErrorCode, FileHandle, FileType, FsDriver, FsError, MkdirOptions, Stats, StreamedFileWrite,
};
#[cfg(feature = "observability")]
use mount_rs_observability::Telemetry;
#[cfg(feature = "observability-otlp")]
use mount_rs_observability::extract_headers;
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, watch};

pub const DEFAULT_MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// The largest file a `PUT` may upload. Bodies above `max_request_bytes` are
/// streamed to storage rather than held in memory.
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// How much of a streamed upload is gathered before each write to the drive,
/// at most. Each write through a handle commits the file's metadata, so
/// fewer, larger writes are much cheaper for large files.
const STREAM_UPLOAD_BUFFER: usize = 8 * 1024 * 1024;
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_READ_CHUNK_BYTES: usize = 64 * 1024;
/// Longer than [`DEFAULT_REQUEST_TIMEOUT`], so a graceful stop lets every
/// request that was accepted either finish or time out by itself.
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(35);
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;
pub const DEFAULT_MAX_DIRECTORY_ENTRIES: usize = 100_000;
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The default upload memory budget, as a multiple of `max_request_bytes`.
pub const DEFAULT_BUFFERED_UPLOAD_REQUESTS: usize = 4;
/// Names beginning with this are the server's own: the temporary sibling an
/// upload is written to before it replaces its target on a driver whose
/// `write_file` is not atomic. Clients can neither name nor list them.
/// The header that names a request. A request that carries one valid value
/// (1 to 128 characters of `A-Z a-z 0-9 . _ -`) keeps it; any other request
/// is given a generated one. Every response the application produces
/// carries the value, and so does the request a handler or a forwarding hop
/// sees.
pub const REQUEST_ID_HEADER: &str = "x-request-id";
/// Longest client-supplied request id that is echoed.
pub const MAX_REQUEST_ID_BYTES: usize = 128;

pub const RESERVED_NAME_PREFIX: &str = ".mount-rs-upload.";

/// How long a close that ran out of drain time waits (at most; never longer
/// than the drain timeout itself) for the requests it aborted to let go of
/// their file handles and finish publishing.
const ABORT_GRACE: Duration = Duration::from_secs(5);
/// The upload budget is counted in units of this many bytes.
const UPLOAD_BUDGET_UNIT: usize = 1024;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type HttpBody = UnsyncBoxBody<Bytes, BoxError>;

/// Errors found while constructing a named-drive registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    InvalidDriveId,
    EmptyBearerToken,
    DuplicateDrive,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDriveId => {
                f.write_str("drive id must be 1-64 ASCII letters, digits, '-', '_' or '.' and cannot equal '.' or '..'")
            }
            Self::EmptyBearerToken => {
                f.write_str("drive bearer token must be non-empty and contain no whitespace")
            }
            Self::DuplicateDrive => f.write_str("drive id is already registered"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// A named logical drive.  The bearer token is intentionally private and is
/// never included in `Debug`, discovery responses, or error responses.
pub struct DriveConfig {
    id: String,
    driver: Arc<dyn FsDriver>,
    bearer_token: String,
}

impl fmt::Debug for DriveConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DriveConfig")
            .field("id", &self.id)
            .field("driver", &"<FsDriver>")
            .field("bearer_token", &"<redacted>")
            .finish()
    }
}

impl Clone for DriveConfig {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            driver: Arc::clone(&self.driver),
            bearer_token: self.bearer_token.clone(),
        }
    }
}

impl DriveConfig {
    /// Create a drive from the same trait-object seam used by native mounts.
    pub fn new(
        id: impl Into<String>,
        driver: Arc<dyn FsDriver>,
        bearer_token: impl Into<String>,
    ) -> Result<Self, RegistryError> {
        let id = id.into();
        let bearer_token = bearer_token.into();
        Self::validate_id(&id)?;
        Self::validate_bearer_token(&bearer_token)?;
        Ok(Self {
            id,
            driver,
            bearer_token,
        })
    }

    /// Validate a public drive path segment before opening its backend.
    pub fn validate_id(id: &str) -> Result<(), RegistryError> {
        if id.is_empty()
            || id.len() > 64
            || id == "."
            || id == ".."
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(RegistryError::InvalidDriveId);
        }
        Ok(())
    }

    /// Validate the configured token before acquiring backend resources.
    pub fn validate_bearer_token(token: &str) -> Result<(), RegistryError> {
        if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(RegistryError::EmptyBearerToken);
        }
        Ok(())
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn driver(&self) -> Arc<dyn FsDriver> {
        Arc::clone(&self.driver)
    }

    fn token_matches(&self, candidate: &str) -> bool {
        constant_time_eq(self.bearer_token.as_bytes(), candidate.as_bytes())
    }
}

/// Configuration-time registry of stable drive names.
#[derive(Debug, Clone, Default)]
pub struct DriveRegistry {
    drives: BTreeMap<String, DriveConfig>,
}

impl DriveRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, drive: DriveConfig) -> Result<(), RegistryError> {
        if self.drives.contains_key(drive.id()) {
            return Err(RegistryError::DuplicateDrive);
        }
        self.drives.insert(drive.id.clone(), drive);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&DriveConfig> {
        self.drives.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.drives.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.drives.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drives.is_empty()
    }
}

/// HTTP listener and transport limits.
#[derive(Debug, Clone)]
pub struct HttpServerOptions {
    pub host: String,
    pub port: u16,
    /// Let `host` be an address other than loopback (default: false). The
    /// listener speaks cleartext HTTP, so bearer tokens and file contents
    /// cross the network unencrypted: enable this only where the network
    /// between the clients, or the load balancer that terminates TLS, and
    /// this listener is private, such as a container network or a VPC.
    pub allow_non_loopback: bool,
    pub max_request_bytes: usize,
    /// Largest file body a `PUT` may upload. Bodies up to `max_request_bytes`
    /// are received whole before the drive is touched; larger ones are
    /// written to a temporary file as they arrive and renamed into place.
    /// Values below `max_request_bytes` are raised to it.
    pub max_upload_bytes: u64,
    pub max_response_bytes: usize,
    pub max_directory_entries: usize,
    pub read_chunk_bytes: usize,
    /// Disable TCP packet coalescing on accepted connections (default: true).
    pub tcp_nodelay: bool,
    /// Accept prior-knowledge HTTP/2 instead of HTTP/1.1 (default: false).
    pub http2_prior_knowledge: bool,
    pub drain_timeout: Duration,
    pub max_connections: usize,
    pub request_timeout: Duration,
    /// Memory that uploads being received may hold in total. A `PUT`
    /// reserves its declared length (or `max_request_bytes` when it declares
    /// none) before its body is read, and waits while the budget is spent.
    /// `None` is four times `max_request_bytes`; a smaller value than
    /// `max_request_bytes` is raised to it.
    pub max_buffered_upload_bytes: Option<usize>,
    #[cfg(feature = "observability")]
    pub telemetry: Telemetry,
}

impl Default for HttpServerOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 0,
            allow_non_loopback: false,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_directory_entries: DEFAULT_MAX_DIRECTORY_ENTRIES,
            read_chunk_bytes: DEFAULT_READ_CHUNK_BYTES,
            tcp_nodelay: true,
            http2_prior_knowledge: false,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_buffered_upload_bytes: None,
            #[cfg(feature = "observability")]
            telemetry: Telemetry::disabled(),
        }
    }
}

#[cfg(feature = "observability")]
impl HttpServerOptions {
    /// Enable application-owned HTTP telemetry for this listener.
    pub fn with_telemetry(mut self, telemetry: Telemetry) -> Self {
        self.telemetry = telemetry;
        self
    }
}

/// Errors from binding or stopping the HTTP listener.
#[derive(Debug)]
pub enum HttpServerError {
    Bind(std::io::Error),
    Io(std::io::Error),
    InsecureBind { host: String },
    Join(Arc<tokio::task::JoinError>),
    Timeout,
}

impl fmt::Display for HttpServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(error) | Self::Io(error) => error.fmt(f),
            Self::InsecureBind { host } => write!(
                f,
                "HTTP listener host '{host}' is not loopback; bind to loopback behind a TLS reverse proxy, or set allow_non_loopback where the network to this listener is private"
            ),
            Self::Join(error) => error.fmt(f),
            Self::Timeout => f.write_str("HTTP server close timed out"),
        }
    }
}

impl std::error::Error for HttpServerError {}

/// Request adapter for an HTTP listener (for example, server-side forwarding).
/// Implementations own authorization, request limits and response semantics.
/// Bodies remain streaming; dropping a response must release its resources.
pub trait HttpRequestHandler: Send + Sync + 'static {
    fn on_http_service_call(&self, _request: &mut Request<HttpBody>) {}

    fn handle(
        &self,
        request: Request<HttpBody>,
    ) -> Pin<Box<dyn std::future::Future<Output = Response<HttpBody>> + Send + '_>>;
}

struct AppState {
    handler: Option<Arc<dyn HttpRequestHandler>>,
    registry: Arc<DriveRegistry>,
    config: RequestConfig,
}

#[derive(Clone)]
struct RequestConfig {
    max_request_bytes: usize,
    max_upload_bytes: u64,
    max_response_bytes: usize,
    max_directory_entries: usize,
    read_chunk_bytes: usize,
    request_timeout: Duration,
    upload_budget: Arc<Semaphore>,
    control: RuntimeControl,
    #[cfg(feature = "observability")]
    telemetry: Telemetry,
}

impl RequestConfig {
    /// How much of a streamed upload is gathered before each drive write.
    /// The upload budget always has room for `max_request_bytes`.
    fn stream_buffer(&self) -> usize {
        STREAM_UPLOAD_BUFFER.min(self.max_request_bytes)
    }
}

/// Stops one connection: set by a download whose client stopped reading and
/// by a close that ran out of drain time.
#[derive(Clone)]
struct ConnectionKill(Arc<Notify>);

#[derive(Clone)]
struct RuntimeControl {
    background_tasks: Arc<AtomicUsize>,
    drained: Arc<Notify>,
    shutdown: Arc<Notify>,
    closing: Arc<AtomicUsize>,
}

#[derive(Clone, Debug)]
enum CloseOutcome {
    Success,
    Join(Arc<tokio::task::JoinError>),
    /// The runtime went away before the accept task's result was published.
    Abandoned,
}

impl CloseOutcome {
    fn result(&self) -> Result<(), HttpServerError> {
        match self {
            Self::Success => Ok(()),
            Self::Join(error) => Err(HttpServerError::Join(Arc::clone(error))),
            Self::Abandoned => Err(HttpServerError::Timeout),
        }
    }
}

/// The one listener a server ever has. Its accept task is joined by a
/// supervisor task that publishes the result, so any number of closes can
/// wait for it, and a close that is cancelled takes nothing with it.
struct ListenerHandle {
    /// `None` for a server with no TCP listener.
    abort: Option<tokio::task::AbortHandle>,
    /// The port the listener is bound to, when there is one.
    port: Option<u16>,
    /// `Some` once the accept task has ended.
    done: watch::Receiver<Option<CloseOutcome>>,
}

impl ListenerHandle {
    /// Supervise `task` as the accept task of a listener bound to `port`.
    fn supervise(task: tokio::task::JoinHandle<()>, port: Option<u16>) -> Self {
        let abort = task.abort_handle();
        let (publish, done) = watch::channel(None);
        tokio::spawn(async move {
            let outcome = match task.await {
                Ok(()) => CloseOutcome::Success,
                Err(error) => CloseOutcome::Join(Arc::new(error)),
            };
            publish.send_replace(Some(outcome));
        });
        Self {
            abort: Some(abort),
            port,
            done,
        }
    }

    /// A listener that was never opened and has already ended cleanly.
    fn unbound() -> Self {
        let (_publish, done) = watch::channel(Some(CloseOutcome::Success));
        Self {
            abort: None,
            port: None,
            done,
        }
    }
}

/// A running multi-drive HTTP server.
pub struct HttpServer {
    state: Arc<AppState>,
    host: String,
    allow_non_loopback: bool,
    requested_port: u16,
    tcp_nodelay: bool,
    http2_prior_knowledge: bool,
    drain_timeout: Duration,
    shutdown: Arc<Notify>,
    closing: Arc<AtomicUsize>,
    drained: Arc<Notify>,
    state_changed: Notify,
    /// Set once, by the close that finishes draining.
    close_outcome: std::sync::OnceLock<CloseOutcome>,
    /// Set once, by the `listen` whose listener is kept.
    listener: OnceLock<ListenerHandle>,
    connections: Arc<AtomicUsize>,
    connection_permits: Arc<Semaphore>,
    /// Set to true by a close that ran out of drain time: every connection
    /// still being served stops.
    stop_all: watch::Sender<bool>,
    accept_failures: Arc<AtomicU64>,
}

impl fmt::Debug for HttpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpServer")
            .field("host", &self.host)
            .field("port", &self.port())
            .field("connections", &self.connections())
            .finish()
    }
}

impl HttpServer {
    pub async fn start(
        registry: DriveRegistry,
        options: HttpServerOptions,
    ) -> Result<Self, HttpServerError> {
        let server = Self::new(registry, options);
        server.listen().await?;
        Ok(server)
    }

    /// Bind a public listener to an application-owned streaming request adapter.
    /// This retains the normal connection and graceful-shutdown lifecycle.
    pub async fn start_with_handler(
        handler: Arc<dyn HttpRequestHandler>,
        options: HttpServerOptions,
    ) -> Result<Self, HttpServerError> {
        let mut server = Self::new(DriveRegistry::new(), options);
        Arc::get_mut(&mut server.state)
            .expect("new application state is unique")
            .handler = Some(handler);
        server.listen().await?;
        Ok(server)
    }

    fn new(registry: DriveRegistry, options: HttpServerOptions) -> Self {
        let background_tasks = Arc::new(AtomicUsize::new(0));
        let drained = Arc::new(Notify::new());
        let shutdown = Arc::new(Notify::new());
        let closing = Arc::new(AtomicUsize::new(0));
        let control = RuntimeControl {
            background_tasks,
            drained: Arc::clone(&drained),
            shutdown: Arc::clone(&shutdown),
            closing: Arc::clone(&closing),
        };
        let max_request_bytes = options.max_request_bytes.max(1);
        let upload_budget = options
            .max_buffered_upload_bytes
            .unwrap_or_else(|| max_request_bytes.saturating_mul(DEFAULT_BUFFERED_UPLOAD_REQUESTS))
            .max(max_request_bytes);
        Self {
            state: Arc::new(AppState {
                handler: None,
                registry: Arc::new(registry),
                config: RequestConfig {
                    max_request_bytes,
                    max_upload_bytes: options.max_upload_bytes.max(max_request_bytes as u64),
                    max_response_bytes: options.max_response_bytes.max(1),
                    max_directory_entries: options.max_directory_entries.max(1),
                    read_chunk_bytes: options.read_chunk_bytes.max(1),
                    request_timeout: positive_duration(options.request_timeout),
                    upload_budget: Arc::new(Semaphore::new(upload_budget_units(upload_budget))),
                    control,
                    #[cfg(feature = "observability")]
                    telemetry: options.telemetry,
                },
            }),
            host: options.host,
            allow_non_loopback: options.allow_non_loopback,
            requested_port: options.port,
            tcp_nodelay: options.tcp_nodelay,
            http2_prior_knowledge: options.http2_prior_knowledge,
            drain_timeout: options.drain_timeout,
            shutdown,
            closing,
            drained,
            state_changed: Notify::new(),
            close_outcome: std::sync::OnceLock::new(),
            listener: OnceLock::new(),
            connections: Arc::new(AtomicUsize::new(0)),
            connection_permits: Arc::new(Semaphore::new(options.max_connections.max(1))),
            stop_all: watch::channel(false).0,
            accept_failures: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Construct the HTTP application without opening a TCP listener.
    /// Alternate transports must call `close` after draining their requests.
    pub fn unbound(registry: DriveRegistry, options: HttpServerOptions) -> Self {
        let server = Self::new(registry, options);
        // There is deliberately no listener task to await during close.
        let _ = server.listener.set(ListenerHandle::unbound());
        server
    }

    /// Serve one request from an alternate transport using the same authorization,
    /// filesystem semantics, limits and shutdown tracking as the TCP listener.
    pub async fn handle(&self, request: Request<HttpBody>) -> Response<HttpBody> {
        self.connections.fetch_add(1, Ordering::AcqRel);
        let _guard = ConnectionGuard {
            counter: self.connections.clone(),
            drained: self.drained.clone(),
        };
        if self.closing.load(Ordering::Acquire) != 0 {
            let mut request = request;
            let id = assign_request_id(&mut request);
            return with_request_id(error_response(RequestError::server_closing()), id);
        }
        match handle_request(request, self.state.clone()).await {
            Ok(response) => response,
            Err(never) => match never {},
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// The bound port once a listener is up, and the requested port before.
    /// It comes from the listener that was kept, so a `listen` that lost to a
    /// concurrent one never shows through here.
    pub fn port(&self) -> u16 {
        self.listener
            .get()
            .and_then(|listener| listener.port)
            .unwrap_or(self.requested_port)
    }

    pub fn url(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("http://{host}:{}", self.port())
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Acquire)
    }

    /// How many times accepting a connection has failed and been retried.
    /// The listener keeps running through these (for example while the
    /// process is out of file descriptors); a rising count is the only sign.
    pub fn accept_failures(&self) -> u64 {
        self.accept_failures.load(Ordering::Relaxed)
    }

    pub async fn listen(&self) -> Result<(), HttpServerError> {
        if self.listener.get().is_some() {
            return Ok(());
        }

        if !self.allow_non_loopback {
            validate_bind_host(&self.host)?;
        }
        let address = socket_address(&self.host, self.requested_port)
            .await
            .map_err(HttpServerError::Bind)?;
        let listener = TcpListener::bind(address)
            .await
            .map_err(HttpServerError::Bind)?;
        let port = listener.local_addr().map_err(HttpServerError::Io)?.port();

        let state = Arc::clone(&self.state);
        let shutdown = Arc::clone(&self.shutdown);
        let counter = Arc::clone(&self.connections);
        let drained = Arc::clone(&self.drained);
        let closing = Arc::clone(&self.closing);
        let connection_permits = Arc::clone(&self.connection_permits);
        let tcp_nodelay = self.tcp_nodelay;
        let http2_prior_knowledge = self.http2_prior_knowledge;
        let stop_all = self.stop_all.clone();
        let accept_failures = Arc::clone(&self.accept_failures);
        let task = tokio::spawn(async move {
            loop {
                if closing.load(Ordering::Acquire) != 0 {
                    break;
                }
                let shutdown_notified = shutdown.notified();
                tokio::pin!(shutdown_notified);
                shutdown_notified.as_mut().enable();
                if closing.load(Ordering::Acquire) != 0 {
                    break;
                }
                let accepted = accept_next(
                    || listener.accept(),
                    |_error| {
                        accept_failures.fetch_add(1, Ordering::Relaxed);
                    },
                );
                tokio::select! {
                    _ = shutdown_notified => break,
                    (stream, _peer) = accepted => {
                        if stream.set_nodelay(tcp_nodelay).is_err() {
                            continue;
                        }
                        if closing.load(Ordering::Acquire) != 0 {
                            break;
                        }
                        let Ok(permit) = Arc::clone(&connection_permits).try_acquire_owned()
                        else {
                            continue;
                        };
                        let state = Arc::clone(&state);
                        let shutdown = Arc::clone(&shutdown);
                        let closing = Arc::clone(&closing);
                        counter.fetch_add(1, Ordering::AcqRel);
                        let kill = Arc::new(Notify::new());
                        let mut stop_all = stop_all.subscribe();
                        let guard = ConnectionGuard {
                            counter: Arc::clone(&counter),
                            drained: Arc::clone(&drained),
                        };
                        tokio::spawn(async move {
                            let _permit = permit;
                            let _guard = guard;
                            let request_timeout = state.config.request_timeout;
                            let request_kill = ConnectionKill(Arc::clone(&kill));
                            let service = service_fn(move |request: Request<Incoming>| {
                                let mut request = request.map(|body| {
                                    body.map_err(|error| -> BoxError { Box::new(error) })
                                        .boxed_unsync()
                                });
                                if !http2_prior_knowledge {
                                    // One HTTP/1.1 connection carries one
                                    // response at a time, so a stalled
                                    // download may take its connection down.
                                    request.extensions_mut().insert(request_kill.clone());
                                }
                                if let Some(handler) = &state.handler {
                                    handler.on_http_service_call(&mut request);
                                }
                                handle_request(request, Arc::clone(&state))
                            });
                            let io = TokioIo::new(stream);
                            // Serve until the connection ends, the server
                            // drains it, or it is stopped outright.
                            macro_rules! serve {
                                ($connection:expr) => {{
                                    let connection = $connection;
                                    tokio::pin!(connection);
                                    let shutdown_notified = shutdown.notified();
                                    tokio::pin!(shutdown_notified);
                                    shutdown_notified.as_mut().enable();
                                    if closing.load(Ordering::Acquire) != 0 {
                                        return;
                                    }
                                    // Stopped alone, by a download whose
                                    // client stopped reading, or with every
                                    // other connection.
                                    let killed = async {
                                        tokio::select! {
                                            _ = kill.notified() => {}
                                            stop = async {
                                                stop_all.wait_for(|stop| *stop).await.is_ok()
                                            } => {
                                                if !stop {
                                                    std::future::pending::<()>().await;
                                                }
                                            }
                                        }
                                    };
                                    tokio::pin!(killed);
                                    tokio::select! {
                                        _ = &mut killed => {}
                                        _ = shutdown_notified => {
                                            connection.as_mut().graceful_shutdown();
                                            tokio::select! {
                                                _ = &mut killed => {}
                                                _ = &mut connection => {}
                                            }
                                        }
                                        _ = &mut connection => {}
                                    }
                                }};
                            }
                            if http2_prior_knowledge {
                                let mut builder = hyper::server::conn::http2::Builder::new(
                                    hyper_util::rt::TokioExecutor::new(),
                                );
                                builder.timer(TokioTimer::new());
                                serve!(builder.serve_connection(io, service));
                                return;
                            }
                            let mut builder = hyper::server::conn::http1::Builder::new();
                            builder
                                .keep_alive(true)
                                .header_read_timeout(Some(request_timeout))
                                .timer(TokioTimer::new());
                            serve!(builder.serve_connection(io, service));
                        });
                    }
                }
            }
        });
        self.adopt_listener(task, Some(port));
        Ok(())
    }

    /// Keep `task` as this server's listener, or abort it when a concurrent
    /// `listen` got there first.
    fn adopt_listener(&self, task: tokio::task::JoinHandle<()>, port: Option<u16>) {
        let handle = ListenerHandle::supervise(task, port);
        match self.listener.set(handle) {
            Ok(()) => self.state_changed.notify_waiters(),
            Err(lost) => {
                if let Some(abort) = &lost.abort {
                    abort.abort();
                }
            }
        }
    }

    /// Report an accept task that terminated before shutdown was requested.
    /// Checking the join handle leaves it available for the normal drain path.
    pub fn listener_failed(&self) -> bool {
        self.listener
            .get()
            .and_then(|listener| listener.abort.as_ref())
            .is_some_and(tokio::task::AbortHandle::is_finished)
            && self.closing.load(Ordering::Acquire) == 0
    }

    pub async fn close(&self) -> Result<(), HttpServerError> {
        if let Some(outcome) = self.close_outcome.get() {
            return outcome.result();
        }
        self.closing
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .ok();
        self.shutdown.notify_waiters();
        if let Some(outcome) = self.close_outcome.get() {
            return outcome.result();
        }
        match tokio::time::timeout(self.drain_timeout, self.complete_close()).await {
            Ok(result) => result,
            Err(_) => {
                // Out of drain time: stop what is still being served, so the
                // caller can release the drives. A stopped request lets go at
                // its next await; closing its file handles, and a publication
                // already under way, are given a little longer to land.
                self.stop_all.send_replace(true);
                let grace = ABORT_GRACE.min(self.drain_timeout);
                let _ = tokio::time::timeout(grace, self.complete_close()).await;
                Err(HttpServerError::Timeout)
            }
        }
    }

    async fn complete_close(&self) -> Result<(), HttpServerError> {
        let listener_outcome = self.await_listener().await;
        let connections = Arc::clone(&self.connections);
        let background_tasks = Arc::clone(&self.state.config.control.background_tasks);
        let drained = Arc::clone(&self.drained);
        wait_for_drain(connections, background_tasks, drained).await;

        // The first close to get here decides the outcome for all of them.
        let mut first = false;
        let outcome = self.close_outcome.get_or_init(|| {
            first = true;
            listener_outcome
        });
        if first {
            self.closing.store(2, Ordering::Release);
            self.state_changed.notify_waiters();
        }
        outcome.result()
    }

    async fn await_listener(&self) -> CloseOutcome {
        let listener = loop {
            if let Some(listener) = self.listener.get() {
                break listener;
            }
            let notified = self.state_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(listener) = self.listener.get() {
                break listener;
            }
            notified.await;
        };
        // Every close waits on its own receiver, so cancelling one leaves
        // the published result where the others and a retry will find it.
        let mut done = listener.done.clone();
        match done.wait_for(Option::is_some).await {
            Ok(outcome) => outcome.clone().unwrap_or(CloseOutcome::Abandoned),
            Err(_) => CloseOutcome::Abandoned,
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.closing.store(1, Ordering::Release);
        self.shutdown.notify_waiters();
        if let Some(abort) = self
            .listener
            .get()
            .and_then(|listener| listener.abort.as_ref())
        {
            abort.abort();
        }
    }
}

async fn wait_for_drain(
    connections: Arc<AtomicUsize>,
    background_tasks: Arc<AtomicUsize>,
    drained: Arc<Notify>,
) {
    while connections.load(Ordering::Acquire) != 0 || background_tasks.load(Ordering::Acquire) != 0
    {
        let notified = drained.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if connections.load(Ordering::Acquire) == 0 && background_tasks.load(Ordering::Acquire) == 0
        {
            break;
        }
        notified.await;
    }
}

struct ConnectionGuard {
    counter: Arc<AtomicUsize>,
    drained: Arc<Notify>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
        self.drained.notify_waiters();
    }
}

struct TaskGuard {
    counter: Arc<AtomicUsize>,
    drained: Arc<Notify>,
}

impl TaskGuard {
    fn new(counter: Arc<AtomicUsize>, drained: Arc<Notify>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self { counter, drained }
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
        self.drained.notify_waiters();
    }
}

/// Ensures a handle is closed even if a request future is cancelled while a
/// client disconnects or the server begins shutdown.  The fallback close is
/// itself tracked by the server drain counter.
struct FileCloseGuard {
    handle: Option<Arc<dyn FileHandle>>,
    background_tasks: Arc<AtomicUsize>,
    drained: Arc<Notify>,
}

impl FileCloseGuard {
    fn new(handle: Arc<dyn FileHandle>, control: &RuntimeControl) -> Self {
        Self {
            handle: Some(handle),
            background_tasks: Arc::clone(&control.background_tasks),
            drained: Arc::clone(&control.drained),
        }
    }

    fn handle(&self) -> Arc<dyn FileHandle> {
        Arc::clone(self.handle.as_ref().expect("file close guard handle"))
    }

    async fn close(&mut self) -> mount_rs_core::Result<()> {
        let Some(handle) = self.handle.as_ref() else {
            return Ok(());
        };
        let result = handle.close().await;
        if result.is_ok() {
            self.handle = None;
        }
        result
    }
}

impl Drop for FileCloseGuard {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let background_tasks = Arc::clone(&self.background_tasks);
        let drained = Arc::clone(&self.drained);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // Reserve the drain slot before handing the task to Tokio.  If the
        // request and the server close race, close() must see this cleanup
        // even while the spawned future is still waiting in the scheduler.
        let task = TaskGuard::new(background_tasks, drained);
        runtime.spawn(async move {
            let _task = task;
            let _ = handle.close().await;
        });
    }
}

#[derive(Debug)]
enum RequestError {
    Fs(FsError),
    Client {
        status: StatusCode,
        code: &'static str,
        message: String,
        close: bool,
        content_range: Option<String>,
        allow: Option<String>,
        authenticate: bool,
    },
}

impl RequestError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self::Client {
            status: StatusCode::BAD_REQUEST,
            code: ErrorCode::Einval.as_str(),
            message: message.into(),
            close: false,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }

    fn unauthorized() -> Self {
        Self::Client {
            status: StatusCode::UNAUTHORIZED,
            code: ErrorCode::Eacces.as_str(),
            message: "bearer authorization required".to_owned(),
            close: false,
            content_range: None,
            allow: None,
            authenticate: true,
        }
    }

    fn not_found() -> Self {
        Self::Client {
            status: StatusCode::NOT_FOUND,
            code: ErrorCode::Enoent.as_str(),
            message: "drive or route not found".to_owned(),
            close: false,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }

    fn method_not_allowed(allow: &'static str) -> Self {
        Self::Client {
            status: StatusCode::METHOD_NOT_ALLOWED,
            code: ErrorCode::Enotsup.as_str(),
            message: "method is not supported for this route".to_owned(),
            close: false,
            content_range: None,
            allow: Some(allow.to_owned()),
            authenticate: false,
        }
    }

    fn body_too_large() -> Self {
        Self::Client {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: ErrorCode::Efbig.as_str(),
            message: "request body exceeds the configured limit".to_owned(),
            close: true,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }

    fn response_too_large() -> Self {
        Self::Client {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: ErrorCode::Eoverflow.as_str(),
            message: "directory response exceeds the configured limit".to_owned(),
            close: true,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }

    /// The server ran out of time; the client did nothing wrong, and a
    /// write may or may not have been published.
    fn request_timeout() -> Self {
        Self::Client {
            status: StatusCode::GATEWAY_TIMEOUT,
            code: "ETIMEDOUT",
            message: "request exceeded the configured timeout".to_owned(),
            close: true,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }

    fn not_acceptable() -> Self {
        Self::Client {
            status: StatusCode::NOT_ACCEPTABLE,
            code: ErrorCode::Enotsup.as_str(),
            message: "no acceptable content encoding: identity or gzip is required".to_owned(),
            close: false,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }

    fn reserved_name() -> Self {
        Self::bad_request("names beginning with .mount-rs-upload. are reserved")
    }

    fn server_closing() -> Self {
        Self::Client {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: ErrorCode::Eio.as_str(),
            message: "server is shutting down".to_owned(),
            close: true,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }

    fn range_not_satisfiable(size: u64) -> Self {
        Self::Client {
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            code: ErrorCode::Einval.as_str(),
            message: "range is not satisfiable".to_owned(),
            close: false,
            content_range: Some(format!("bytes */{size}")),
            allow: None,
            authenticate: false,
        }
    }

    fn precondition_failed() -> Self {
        Self::Client {
            status: StatusCode::PRECONDITION_FAILED,
            code: "EPRECONDITION",
            message: "the file does not match the request's preconditions".to_owned(),
            close: false,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }
}

#[derive(Debug)]
enum Route {
    Health {
        readiness: bool,
    },
    Discovery,
    File {
        drive: String,
        path: String,
    },
    Entries {
        drive: String,
        path: String,
    },
    Metadata {
        drive: String,
        path: String,
        kind: String,
    },
    Operation {
        drive: String,
        operation: String,
    },
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

#[derive(Serialize)]
struct DriveSummary<'a> {
    id: &'a str,
}

#[derive(Serialize)]
struct HealthPayload<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    drives: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed: Option<usize>,
}

#[derive(Serialize)]
struct MetadataPayload<'a> {
    path: &'a str,
    stats: &'a Stats,
}

#[derive(Serialize)]
struct DirectoryPayload<'a> {
    path: &'a str,
    stats: &'a Stats,
    entries: Vec<mount_rs_core::DirEntry>,
}

#[derive(Serialize)]
struct WritePayload<'a> {
    path: &'a str,
    bytes: usize,
}

#[derive(Serialize)]
struct MkdirPayload<'a> {
    path: &'a str,
    created: Option<String>,
}

#[derive(Deserialize)]
struct MkdirRequest {
    path: String,
    #[serde(default)]
    recursive: bool,
    mode: Option<u32>,
}

#[derive(Deserialize)]
struct RenameRequest {
    from: String,
    to: String,
}

#[derive(Deserialize)]
struct TruncateRequest {
    path: String,
    length: u64,
}

#[derive(Clone, Copy)]
struct ByteRange {
    start: u64,
    length: u64,
}

#[cfg(feature = "observability")]
struct HttpResponseFailure {
    response: Response<HttpBody>,
    error_code: &'static str,
}

#[cfg(feature = "observability")]
impl HttpResponseFailure {
    fn from_response(
        response: Response<HttpBody>,
    ) -> std::result::Result<Response<HttpBody>, Box<Self>> {
        let status = response.status();
        let error_code = if status.is_client_error() {
            Some("http_client_error")
        } else if status.is_server_error() {
            Some("http_server_error")
        } else {
            None
        };
        match error_code {
            Some(error_code) => Err(Box::new(Self {
                response,
                error_code,
            })),
            None => Ok(response),
        }
    }
}

/// Marks a request whose id has been settled, so that an application nested
/// behind a handler (session routing in front of the drive routes) leaves
/// the id to the listener that received the request.
#[derive(Clone, Copy)]
struct RequestScope;

fn valid_request_id(value: &[u8]) -> bool {
    (1..=MAX_REQUEST_ID_BYTES).contains(&value.len())
        && value
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// A new request id: 32 lowercase hexadecimal characters, unique for
/// practical purposes across processes. It identifies a request; it is not a
/// secret and authorizes nothing.
fn generate_request_id() -> header::HeaderValue {
    use std::hash::{BuildHasher, RandomState};
    static KEYS: OnceLock<(RandomState, RandomState)> = OnceLock::new();
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (high, low) = KEYS.get_or_init(|| (RandomState::new(), RandomState::new()));
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let id = format!(
        "{:016x}{:016x}",
        high.hash_one(sequence),
        low.hash_one(sequence)
    );
    header::HeaderValue::from_maybe_shared(Bytes::from(id))
        .expect("hexadecimal digits are a valid header value")
}

/// Settle the id of a request that does not have one yet: keep the single
/// valid `x-request-id` the client sent, or replace whatever it sent with a
/// generated id. The request then carries exactly that value, which is what
/// a handler forwards. Returns `None` for a request an outer listener has
/// already settled.
fn assign_request_id(request: &mut Request<HttpBody>) -> Option<header::HeaderValue> {
    if request.extensions().get::<RequestScope>().is_some() {
        return None;
    }
    request.extensions_mut().insert(RequestScope);
    let headers = request.headers_mut();
    let mut sent = headers.get_all(REQUEST_ID_HEADER).iter();
    if let (Some(value), None) = (sent.next(), sent.next())
        && valid_request_id(value.as_bytes())
    {
        return Some(value.clone());
    }
    let id = generate_request_id();
    headers.insert(REQUEST_ID_HEADER, id.clone());
    Some(id)
}

fn with_request_id(
    mut response: Response<HttpBody>,
    id: Option<header::HeaderValue>,
) -> Response<HttpBody> {
    if let Some(id) = id {
        response.headers_mut().insert(REQUEST_ID_HEADER, id);
    }
    response
}

async fn handle_request(
    mut request: Request<HttpBody>,
    state: Arc<AppState>,
) -> Result<Response<HttpBody>, std::convert::Infallible> {
    let id = assign_request_id(&mut request);
    let response = match handle_request_instrumented(request, state).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    Ok(with_request_id(response, id))
}

async fn handle_request_instrumented(
    request: Request<HttpBody>,
    state: Arc<AppState>,
) -> Result<Response<HttpBody>, std::convert::Infallible> {
    #[cfg(feature = "observability")]
    if state.config.telemetry.is_enabled() {
        let path = request.uri().path().to_owned();
        let telemetry = state.config.telemetry.clone();
        #[cfg(feature = "observability-otlp")]
        let parent_context = extract_headers(request.headers());
        #[cfg(feature = "observability-otlp")]
        let response = telemetry
            .observe_result_with_context(
                "http",
                "request",
                Some(&path),
                &parent_context,
                async move {
                    HttpResponseFailure::from_response(
                        handle_request_with_timeout(request, state).await,
                    )
                },
                |failure| Some(failure.error_code),
            )
            .await;
        #[cfg(not(feature = "observability-otlp"))]
        let response = telemetry
            .observe_result(
                "http",
                "request",
                Some(&path),
                async move {
                    HttpResponseFailure::from_response(
                        handle_request_with_timeout(request, state).await,
                    )
                },
                |failure| Some(failure.error_code),
            )
            .await;
        return Ok(match response {
            Ok(response) => response,
            Err(failure) => failure.response,
        });
    }

    Ok(handle_request_with_timeout(request, state).await)
}

async fn handle_request_with_timeout(
    request: Request<HttpBody>,
    state: Arc<AppState>,
) -> Response<HttpBody> {
    // A streamed upload may take far longer than one request timeout. Its
    // body, drive writes and publication each have to make progress within
    // the timeout instead.
    if streams_upload(&request, state.config.max_request_bytes) {
        return handle_request_uninstrumented(request, state).await;
    }
    match tokio::time::timeout(
        state.config.request_timeout,
        handle_request_uninstrumented(request, state),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => error_response(RequestError::request_timeout()),
    }
}

async fn handle_request_uninstrumented(
    request: Request<HttpBody>,
    state: Arc<AppState>,
) -> Response<HttpBody> {
    if state.config.control.closing.load(Ordering::Acquire) != 0 {
        return error_response(RequestError::server_closing());
    }
    if let Some(handler) = &state.handler {
        // A handler (session routing, forwarding) gets the same answer to a
        // declared oversized upload as the built-in routes: 413 after the
        // body is read, rather than a connection closed mid-upload.
        if content_length_exceeds(
            request.headers(),
            body_limit(request.method(), request.uri().path(), &state.config),
        ) {
            let body = request.into_body();
            return refuse(body, RequestError::body_too_large(), &state.config.control).await;
        }
        return handler.handle(request).await;
    }
    let (parts, body) = request.into_parts();
    let route = match parse_route(parts.uri.path()) {
        Ok(route) => route,
        Err(error) => return refuse(body, error, &state.config.control).await,
    };

    match &route {
        Route::Health { readiness } => {
            return handle_health(&parts.method, &state.registry, *readiness);
        }
        Route::Discovery => {
            return handle_discovery(&parts.method, &parts.headers, &state.registry);
        }
        Route::File { .. }
        | Route::Metadata { .. }
        | Route::Entries { .. }
        | Route::Operation { .. } => {}
    }

    let drive_id = match &route {
        Route::File { drive, .. }
        | Route::Metadata { drive, .. }
        | Route::Entries { drive, .. }
        | Route::Operation { drive, .. } => drive,
        Route::Discovery | Route::Health { .. } => unreachable!(),
    };
    // An unknown drive answers exactly as a wrong token does, so the
    // response does not say which drive ids exist.
    let Some(drive) = state
        .registry
        .get(drive_id)
        .filter(|drive| authorized(&parts.headers, drive))
        .cloned()
    else {
        return refuse(body, RequestError::unauthorized(), &state.config.control).await;
    };
    if content_length_exceeds(
        &parts.headers,
        body_limit(&parts.method, parts.uri.path(), &state.config),
    ) {
        return refuse(body, RequestError::body_too_large(), &state.config.control).await;
    }

    let result = match route {
        Route::File { path, .. } => {
            let kill = parts.extensions.get::<ConnectionKill>().cloned();
            handle_file(
                parts.method,
                parts.headers,
                body,
                drive,
                path,
                state.config.clone(),
                kill,
            )
            .await
        }
        Route::Metadata { path, kind, .. } => {
            handle_metadata(parts.method, drive, path, kind).await
        }
        Route::Entries { path, .. } => {
            handle_entries(
                parts.method,
                &parts.headers,
                drive,
                path,
                state.config.clone(),
            )
            .await
        }
        Route::Operation { operation, .. } => {
            handle_operation(parts.method, body, drive, operation, state.config.clone()).await
        }
        Route::Discovery | Route::Health { .. } => unreachable!(),
    };
    result.unwrap_or_else(error_response)
}

async fn handle_metadata(
    method: Method,
    drive: DriveConfig,
    path: String,
    kind: String,
) -> Result<Response<HttpBody>, RequestError> {
    if method != Method::GET && method != Method::HEAD {
        return Err(RequestError::method_not_allowed("GET, HEAD"));
    }
    let payload = match kind.as_str() {
        "stat" => serde_json::to_vec(&drive.driver().stat(&path).await.map_err(RequestError::Fs)?),
        "lstat" => serde_json::to_vec(
            &drive
                .driver()
                .lstat(&path)
                .await
                .map_err(RequestError::Fs)?,
        ),
        "statfs" => serde_json::to_vec(
            &drive
                .driver()
                .statfs(&path)
                .await
                .map_err(RequestError::Fs)?,
        ),
        "readlink" => serde_json::to_vec(
            &drive
                .driver()
                .readlink(&path)
                .await
                .map_err(RequestError::Fs)?,
        ),
        _ => return Err(RequestError::not_found()),
    }
    .map_err(|_| RequestError::Fs(FsError::backend("failed to encode metadata result")))?;
    Ok(json_response(
        StatusCode::OK,
        Bytes::from(payload),
        method == Method::HEAD,
    ))
}

fn handle_discovery(
    method: &Method,
    headers: &hyper::HeaderMap,
    registry: &DriveRegistry,
) -> Response<HttpBody> {
    let Some(token) = bearer_token(headers) else {
        return error_response(RequestError::unauthorized());
    };
    if method != Method::GET && method != Method::HEAD {
        return error_response(RequestError::method_not_allowed("GET, HEAD"));
    }
    let drives: Vec<_> = registry
        .drives
        .values()
        .filter(|drive| drive.token_matches(token))
        .map(|drive| DriveSummary { id: drive.id() })
        .collect();
    if drives.is_empty() {
        return error_response(RequestError::unauthorized());
    }
    let body = serde_json::to_vec(&drives).unwrap_or_else(|_| b"[]".to_vec());
    json_response(StatusCode::OK, Bytes::from(body), method == Method::HEAD)
}

fn handle_health(method: &Method, registry: &DriveRegistry, readiness: bool) -> Response<HttpBody> {
    if method != Method::GET && method != Method::HEAD {
        return error_response(RequestError::method_not_allowed("GET, HEAD"));
    }

    // Readiness asks every drive whether it can still serve. A drive that
    // failed closed answers every request with an error until the process
    // is replaced, so one such drive takes the whole process out of rotation.
    let failed = if readiness {
        registry
            .drives
            .values()
            .filter(|drive| drive.driver.health().is_err())
            .count()
    } else {
        0
    };
    let is_ready = !readiness || (!registry.is_empty() && failed == 0);
    let status = if is_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let payload = HealthPayload {
        status: if readiness {
            if is_ready { "ready" } else { "not_ready" }
        } else {
            "ok"
        },
        drives: readiness.then_some(registry.len()),
        failed: (readiness && !is_ready).then_some(failed),
    };
    let bytes = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{\"status\":\"error\"}".to_vec());
    health_json_response(status, Bytes::from(bytes), method == Method::HEAD)
}

async fn handle_file(
    method: Method,
    headers: hyper::HeaderMap,
    body: HttpBody,
    drive: DriveConfig,
    path: String,
    config: RequestConfig,
    kill: Option<ConnectionKill>,
) -> Result<Response<HttpBody>, RequestError> {
    match method {
        Method::GET | Method::HEAD => {
            handle_file_read(
                method == Method::HEAD,
                &headers,
                drive,
                path,
                config.clone(),
                kill,
            )
            .await
        }
        Method::PUT => handle_file_write(&headers, body, drive, path, config).await,
        Method::DELETE => handle_file_delete(&headers, drive, path, config).await,
        _ => Err(RequestError::method_not_allowed("GET, HEAD, PUT, DELETE")),
    }
}

async fn handle_file_read(
    head: bool,
    headers: &hyper::HeaderMap,
    drive: DriveConfig,
    path: String,
    config: RequestConfig,
    kill: Option<ConnectionKill>,
) -> Result<Response<HttpBody>, RequestError> {
    let lstat = drive
        .driver()
        .lstat(&path)
        .await
        .map_err(RequestError::Fs)?;
    let stats = if lstat.is_symbolic_link() {
        drive.driver().stat(&path).await.map_err(RequestError::Fs)?
    } else {
        lstat
    };
    if stats.is_directory() {
        let entries = list_directory(&drive, &path, config.max_directory_entries).await?;
        let payload = serde_json::to_vec(&DirectoryPayload {
            path: &path,
            stats: &stats,
            entries,
        })
        .map_err(|_| RequestError::Fs(FsError::backend("failed to encode directory metadata")))?;
        if payload.len() > config.max_response_bytes {
            return Err(RequestError::response_too_large());
        }
        return Ok(json_response(StatusCode::OK, Bytes::from(payload), head));
    }
    if stats.file_type() != FileType::File && stats.file_type() != FileType::Symlink {
        let payload = serde_json::to_vec(&MetadataPayload {
            path: &path,
            stats: &stats,
        })
        .map_err(|_| RequestError::Fs(FsError::backend("failed to encode metadata")))?;
        return Ok(json_response(StatusCode::OK, Bytes::from(payload), head));
    }

    // Bind representation metadata to the same open inode as the body. Path
    // metadata above only selects directory/special handling and can race rename.
    let mut opened = if head {
        None
    } else {
        Some(FileCloseGuard::new(
            drive
                .driver()
                .open(&path, "r", 0)
                .await
                .map_err(RequestError::Fs)?,
            &config.control,
        ))
    };
    let stats = if let Some(guard) = &opened {
        let stats = guard.handle().stat().await.map_err(RequestError::Fs)?;
        if !stats.is_file() {
            return Err(RequestError::Fs(FsError::new(if stats.is_directory() {
                ErrorCode::Eisdir
            } else {
                ErrorCode::Enxio
            })));
        }
        stats
    } else {
        stats
    };
    let content = match &opened {
        Some(guard) => guard.handle().content_tag().await,
        None => drive.driver().content_tag(&path).await,
    }
    .map_err(RequestError::Fs)?;
    let etag = conditional::entity_tag(&stats, content);
    match conditional::evaluate(headers, &Current::Tagged(etag.clone()), true) {
        Outcome::Proceed => {}
        Outcome::NotModified => return Ok(not_modified(&etag)),
        Outcome::Failed => return Err(RequestError::precondition_failed()),
    }

    let range = match headers.get(header::RANGE) {
        Some(_) if !conditional::range_applies(headers, &etag) => None,
        None => None,
        Some(value) => {
            let value = value
                .to_str()
                .map_err(|_| RequestError::bad_request("range header is not valid UTF-8"))?;
            Some(
                parse_range(value, stats.size)
                    .map_err(|_| RequestError::range_not_satisfiable(stats.size))?,
            )
        }
    };
    let (start, length, status, content_range) = match range {
        Some(range) => (
            range.start,
            range.length,
            StatusCode::PARTIAL_CONTENT,
            Some(format!(
                "bytes {}-{}/{}",
                range.start,
                range.start.saturating_add(range.length).saturating_sub(1),
                stats.size
            )),
        ),
        None => (0, stats.size, StatusCode::OK, None),
    };

    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, &etag);
    if let Some(content_range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, content_range);
    }
    if head {
        return builder
            .body(head_body(length))
            .map_err(|_| RequestError::Fs(FsError::backend("failed to build HTTP response")));
    }

    let handle = opened
        .as_mut()
        .expect("GET opened handle")
        .handle
        .take()
        .expect("GET handle ownership");
    builder
        .body(stream_file(
            handle,
            start,
            length,
            config.read_chunk_bytes,
            config.request_timeout,
            kill,
            config.control,
        ))
        .map_err(|_| RequestError::Fs(FsError::backend("failed to build HTTP response")))
}

/// Replace a file with the request body, in one step.
///
/// A body of up to `max_request_bytes` is received in full before the drive
/// is touched, then published in one step of the driver. A larger body, or
/// one without a declared length that grows past `max_request_bytes`, is
/// handed to the driver's streamed whole-file write as it arrives where the
/// driver has one, and otherwise written to a temporary sibling and renamed
/// over the target once complete. Either way a request that fails, is abandoned, times out
/// or meets a shutdown while its body is arriving leaves the target as it
/// was, and a reader, another writer or a restart sees the previous content
/// or the new, never a prefix. A request abandoned during publication is
/// published or not, as the driver got to it.
async fn handle_file_write(
    headers: &hyper::HeaderMap,
    body: HttpBody,
    drive: DriveConfig,
    path: String,
    config: RequestConfig,
) -> Result<Response<HttpBody>, RequestError> {
    let preconditions = Preconditions::new(headers, &drive, &path);
    // Refuse before receiving a body that could not be published. The
    // preconditions are checked again, under the path's write lock, just
    // before publishing.
    preconditions
        .check(drive.driver().as_ref(), &config)
        .await?;
    let declared = declared_length(headers);
    let (source, permit) = if declared
        .is_some_and(|length| length > config.max_request_bytes as u64)
    {
        // Streaming holds one write buffer, not the body.
        let permit = reserve_upload_budget(&config, config.stream_buffer()).await?;
        (
            UploadSource::Stream(StreamedUpload::new(Vec::new(), body)),
            permit,
        )
    } else {
        // An upload with no declared length may grow to the request limit
        // before it is known to need streaming.
        let declared = declared.map(|length| length as usize);
        let permit =
            reserve_upload_budget(&config, declared.unwrap_or(config.max_request_bytes)).await?;
        match read_body_or_spill(body, declared.unwrap_or(0), &config).await? {
            Received::Whole(data) => (UploadSource::Whole(data), permit),
            Received::Spilled { prefix, body } => (
                UploadSource::Stream(StreamedUpload::new(prefix, body)),
                permit,
            ),
        }
    };
    if config.control.closing.load(Ordering::Acquire) != 0 {
        return Err(RequestError::server_closing());
    }
    let (written, lock) =
        publish_upload(drive.driver(), &path, source, &preconditions, &config).await?;
    drop(permit);
    // Read under the lock, so the tag is of this write and not a later one.
    let etag = match current_state(drive.driver().as_ref(), &path, &config).await {
        Ok(Current::Tagged(etag)) => Some(etag),
        _ => None,
    };
    drop(lock);
    let payload = serde_json::to_vec(&WritePayload {
        path: &path,
        bytes: written,
    })
    .map_err(|_| RequestError::Fs(FsError::backend("failed to encode write result")))?;
    let mut response = json_response(StatusCode::OK, Bytes::from(payload), false);
    if let Some(value) = etag.and_then(|etag| header::HeaderValue::from_str(&etag).ok()) {
        response.headers_mut().insert(header::ETAG, value);
    }
    Ok(response)
}

/// A write's `If-Match` and `If-None-Match`, and the lock that makes
/// checking them and publishing one step for HTTP requests.
struct Preconditions<'a> {
    headers: &'a hyper::HeaderMap,
    drive: &'a str,
    path: &'a str,
}

impl<'a> Preconditions<'a> {
    fn new(headers: &'a hyper::HeaderMap, drive: &'a DriveConfig, path: &'a str) -> Self {
        Self {
            headers,
            drive: &drive.id,
            path,
        }
    }

    /// Fail with 412 unless the preconditions hold now.
    async fn check(
        &self,
        driver: &dyn FsDriver,
        config: &RequestConfig,
    ) -> Result<(), RequestError> {
        if !conditional::is_conditional(self.headers) {
            return Ok(());
        }
        let current = current_state(driver, self.path, config).await?;
        match conditional::evaluate(self.headers, &current, false) {
            Outcome::Proceed => Ok(()),
            Outcome::NotModified | Outcome::Failed => Err(RequestError::precondition_failed()),
        }
    }

    /// Take the path's write lock, then check the preconditions. Every
    /// HTTP write and delete of the path takes it while it publishes, so
    /// none lands between another's check and its publication.
    async fn lock(
        &self,
        driver: &dyn FsDriver,
        config: &RequestConfig,
    ) -> Result<TurnLockGuard<'static, ()>, RequestError> {
        let lock = tokio::time::timeout(
            positive_duration(config.request_timeout),
            write_lock(self.drive, self.path).lock(),
        )
        .await
        .map_err(|_| RequestError::request_timeout())?;
        self.check(driver, config).await?;
        Ok(lock)
    }
}

/// One of a fixed set of locks, chosen by drive and path. Unrelated paths
/// that share one only wait for each other's publication.
fn write_lock(drive: &str, path: &str) -> &'static TurnLock<()> {
    use std::hash::{Hash, Hasher};
    const LOCKS: usize = 1024;
    static WRITE_LOCKS: OnceLock<Vec<TurnLock<()>>> = OnceLock::new();
    let table = WRITE_LOCKS.get_or_init(|| (0..LOCKS).map(|_| TurnLock::new(())).collect());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (drive, path).hash(&mut hasher);
    &table[(hasher.finish() % LOCKS as u64) as usize]
}

/// What a write's preconditions are evaluated against: the file the path
/// names, following symbolic links as a write does.
async fn current_state(
    driver: &dyn FsDriver,
    path: &str,
    config: &RequestConfig,
) -> Result<Current, RequestError> {
    let missing = |error: &RequestError| {
        matches!(error, RequestError::Fs(error)
            if matches!(error.code, ErrorCode::Enoent | ErrorCode::Enotdir))
    };
    let stats = match bounded(config, driver.stat(path)).await {
        Ok(stats) => stats,
        Err(error) if missing(&error) => return Ok(Current::Missing),
        Err(error) => return Err(error),
    };
    if !stats.is_file() {
        return Ok(Current::Untagged);
    }
    match bounded(config, driver.content_tag(path)).await {
        Ok(content) => Ok(Current::Tagged(conditional::entity_tag(&stats, content))),
        Err(error) if missing(&error) => Ok(Current::Missing),
        Err(error) => Err(error),
    }
}

fn not_modified(etag: &str) -> Response<HttpBody> {
    let mut response = Response::new(full_body(Bytes::new()));
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    if let Ok(value) = header::HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

/// Where an upload's bytes come from.
enum UploadSource {
    /// Received in full, at most `max_request_bytes`.
    Whole(Vec<u8>),
    /// Still arriving: what was received so far, then the rest of the body.
    Stream(StreamedUpload),
}

enum Received {
    Whole(Vec<u8>),
    Spilled { prefix: Vec<u8>, body: HttpBody },
}

/// Receive a body of up to `max_request_bytes`, or return what arrived so
/// far with the rest of the body once it outgrows that.
async fn read_body_or_spill(
    mut body: HttpBody,
    expected_bytes: usize,
    config: &RequestConfig,
) -> Result<Received, RequestError> {
    let mut bytes = Vec::with_capacity(expected_bytes.min(config.max_request_bytes));
    while let Some(frame) = next_upload_frame(&mut body, config).await? {
        let Ok(data) = frame.into_data() else {
            continue;
        };
        bytes.extend_from_slice(&data);
        if bytes.len() as u64 > config.max_upload_bytes {
            drop(bytes);
            discard_body(body, &config.control).await;
            return Err(RequestError::body_too_large());
        }
        if bytes.len() > config.max_request_bytes {
            return Ok(Received::Spilled {
                prefix: bytes,
                body,
            });
        }
    }
    Ok(Received::Whole(bytes))
}

/// The next frame of an upload body, which must arrive within the request
/// timeout of the previous one.
async fn next_upload_frame(
    body: &mut HttpBody,
    config: &RequestConfig,
) -> Result<Option<Frame<Bytes>>, RequestError> {
    tokio::time::timeout(
        config.request_timeout,
        next_request_frame(body, &config.control.shutdown, &config.control.closing),
    )
    .await
    .map_err(|_| RequestError::request_timeout())?
}

/// One drive call of an upload, which must finish within the request timeout.
async fn bounded<T>(
    config: &RequestConfig,
    call: impl std::future::Future<Output = mount_rs_core::Result<T>>,
) -> Result<T, RequestError> {
    tokio::time::timeout(config.request_timeout, call)
        .await
        .map_err(|_| RequestError::request_timeout())?
        .map_err(RequestError::Fs)
}

struct StreamedUpload {
    /// Received but not yet handed out, or the piece last handed out.
    pending: Vec<u8>,
    body: HttpBody,
    /// Bytes handed out before `pending`.
    offset: u64,
    /// `pending` is the piece last handed out.
    handed: bool,
    finished: bool,
}

impl StreamedUpload {
    fn new(prefix: Vec<u8>, body: HttpBody) -> Self {
        Self {
            pending: prefix,
            body,
            offset: 0,
            handed: false,
            finished: false,
        }
    }

    fn whole(data: Vec<u8>) -> Self {
        Self::new(data, empty_body())
    }

    /// The next piece of the upload, of about one write buffer, or `None`
    /// once it has all been handed out.
    async fn next_piece(&mut self, config: &RequestConfig) -> Result<Option<&[u8]>, RequestError> {
        if self.handed {
            self.offset += self.pending.len() as u64;
            self.pending.clear();
            self.handed = false;
        }
        let buffer = config.stream_buffer();
        self.pending
            .reserve(buffer.saturating_sub(self.pending.len()));
        while !self.finished && self.pending.len() < buffer {
            let Some(frame) = next_upload_frame(&mut self.body, config).await? else {
                self.finished = true;
                break;
            };
            if let Ok(data) = frame.into_data() {
                let total = self
                    .offset
                    .saturating_add(self.pending.len() as u64)
                    .saturating_add(data.len() as u64);
                if total > config.max_upload_bytes {
                    let body = std::mem::replace(&mut self.body, empty_body());
                    discard_body(body, &config.control).await;
                    return Err(RequestError::body_too_large());
                }
                self.pending.extend_from_slice(&data);
            }
        }
        if self.pending.is_empty() {
            return Ok(None);
        }
        self.handed = true;
        Ok(Some(&self.pending))
    }

    /// Write the whole upload to `file` from offset zero, a buffer at a time,
    /// and return its length.
    async fn write_to(
        mut self,
        file: &dyn FileHandle,
        path: &str,
        config: &RequestConfig,
    ) -> Result<u64, RequestError> {
        let mut offset = 0_u64;
        while let Some(piece) = self.next_piece(config).await? {
            write_all_at(file, piece, offset, path, config).await?;
            offset += piece.len() as u64;
        }
        Ok(offset)
    }

    /// Hand the whole upload to a driver's streamed write, a buffer at a
    /// time, and return the write ready to commit.
    async fn write_into(
        mut self,
        mut write: Box<dyn StreamedFileWrite>,
        config: &RequestConfig,
    ) -> Result<Box<dyn StreamedFileWrite>, RequestError> {
        while let Some(piece) = self.next_piece(config).await? {
            bounded(config, write.write(piece)).await?;
        }
        Ok(write)
    }
}

fn empty_body() -> HttpBody {
    http_body_util::Empty::new()
        .map_err(|never| match never {})
        .boxed_unsync()
}

async fn write_all_at(
    file: &dyn FileHandle,
    data: &[u8],
    start: u64,
    path: &str,
    config: &RequestConfig,
) -> Result<(), RequestError> {
    let mut written = 0;
    while written < data.len() {
        let count = bounded(
            config,
            file.write(&data[written..], Some(start + written as u64)),
        )
        .await?;
        if count == 0 || count > data.len() - written {
            return Err(RequestError::Fs(
                FsError::new(ErrorCode::Eio)
                    .with_syscall("write")
                    .with_path(path)
                    .with_message("driver returned an invalid write length"),
            ));
        }
        written += count;
    }
    Ok(())
}

fn upload_budget_units(bytes: usize) -> usize {
    bytes
        .div_ceil(UPLOAD_BUDGET_UNIT)
        .clamp(1, (u32::MAX as usize).min(Semaphore::MAX_PERMITS))
}

/// Wait for room in the upload memory budget. The wait is part of the
/// request, so it ends with the request timeout or the server's shutdown.
async fn reserve_upload_budget(
    config: &RequestConfig,
    bytes: usize,
) -> Result<OwnedSemaphorePermit, RequestError> {
    let units = upload_budget_units(bytes) as u32;
    let control = &config.control;
    let notified = control.shutdown.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if control.closing.load(Ordering::Acquire) != 0 {
        return Err(RequestError::server_closing());
    }
    tokio::select! {
        _ = notified => Err(RequestError::server_closing()),
        // A streamed upload has no overall request timeout to end the wait.
        () = tokio::time::sleep(config.request_timeout) => Err(RequestError::request_timeout()),
        permit = Arc::clone(&config.upload_budget).acquire_many_owned(units) => permit
            .map_err(|_| RequestError::Fs(FsError::backend("upload budget unavailable"))),
    }
}

/// Publish a received upload. A driver whose `write_file` is atomic does it
/// in one call when the body was received whole; a driver with a streamed
/// whole-file write takes the body as it arrives; for any other the bytes go
/// to a hidden sibling first.
async fn publish_upload(
    driver: Arc<dyn FsDriver>,
    path: &str,
    source: UploadSource,
    preconditions: &Preconditions<'_>,
    config: &RequestConfig,
) -> Result<(usize, TurnLockGuard<'static, ()>), RequestError> {
    if let UploadSource::Whole(data) = &source
        && driver.write_file_is_atomic()
    {
        let lock = preconditions.lock(driver.as_ref(), config).await?;
        bounded(config, driver.write_file(path, data)).await?;
        return Ok((data.len(), lock));
    }
    let existing = match bounded(config, driver.lstat(path)).await {
        Ok(stats) => Some(stats),
        Err(RequestError::Fs(error)) if error.code == ErrorCode::Enoent => None,
        Err(error) => return Err(error),
    };
    if let Some(stats) = &existing {
        if stats.is_directory() {
            return Err(RequestError::Fs(
                FsError::new(ErrorCode::Eisdir)
                    .with_syscall("open")
                    .with_path(path),
            ));
        }
        if stats.is_symbolic_link() {
            // Renaming over a link would replace the link, not what it
            // names. Write through it in place, as before; this one case is
            // not atomic.
            let lock = preconditions.lock(driver.as_ref(), config).await?;
            let written = write_through(driver.as_ref(), path, source, config).await?;
            return Ok((written, lock));
        }
        if !stats.is_file() {
            return Err(RequestError::Fs(
                FsError::new(ErrorCode::Enxio)
                    .with_syscall("open")
                    .with_path(path),
            ));
        }
    }
    if let Some(write) = bounded(config, driver.begin_file_write(path)).await? {
        // The driver stores the body as it arrives and publishes it in one
        // step, without a sibling to write and rename.
        let stream = match source {
            UploadSource::Whole(data) => StreamedUpload::whole(data),
            UploadSource::Stream(stream) => stream,
        };
        let write = stream.write_into(write, config).await?;
        // As for a sibling: the body is stored, so check the preconditions
        // under the path's lock and publish while holding it.
        let lock = preconditions.lock(driver.as_ref(), config).await?;
        let written = bounded(config, write.commit()).await?;
        let written = usize::try_from(written).map_err(|_| {
            RequestError::Fs(
                FsError::new(ErrorCode::Efbig)
                    .with_syscall("write")
                    .with_path(path),
            )
        })?;
        return Ok((written, lock));
    }
    let mut sibling = SiblingGuard {
        sibling: Some(ActiveSibling::new(path)),
        driver: Arc::clone(&driver),
        control: config.control.clone(),
    };
    let name = sibling.path().to_owned();
    let result = async {
        let written = write_sibling(driver.as_ref(), &name, source, config).await?;
        if let Some(stats) = &existing {
            // The replacement is a new file: keep the permissions the
            // previous one had where the driver can.
            let _ = bounded(config, driver.chmod(&name, stats.mode & 0o7777)).await;
        }
        let lock = preconditions.lock(driver.as_ref(), config).await?;
        bounded(config, driver.rename(&name, path)).await?;
        Ok((written, lock))
    }
    .await;
    match result {
        Ok((written, lock)) => {
            sibling.sibling = None;
            // Make the new name durable where the driver can sync a
            // directory; a driver that cannot has nothing more to offer.
            if let Ok(directory) = bounded(config, driver.open(parent_path(path), "r", 0)).await {
                let _ = bounded(config, directory.sync()).await;
                let _ = bounded(config, directory.close()).await;
            }
            Ok((written, lock))
        }
        Err(error) => {
            let _ = bounded(config, driver.unlink(&name)).await;
            sibling.sibling = None;
            Err(error)
        }
    }
}

/// Write an upload through a symbolic link, in place.
async fn write_through(
    driver: &dyn FsDriver,
    path: &str,
    source: UploadSource,
    config: &RequestConfig,
) -> Result<usize, RequestError> {
    match source {
        UploadSource::Whole(data) => {
            bounded(config, driver.write_file(path, &data)).await?;
            Ok(data.len())
        }
        UploadSource::Stream(stream) => write_handle(driver, path, "w", stream, config).await,
    }
}

/// Write and sync an upload to a new file that nothing else names.
async fn write_sibling(
    driver: &dyn FsDriver,
    sibling: &str,
    source: UploadSource,
    config: &RequestConfig,
) -> Result<usize, RequestError> {
    let stream = match source {
        UploadSource::Whole(data) => StreamedUpload::whole(data),
        UploadSource::Stream(stream) => stream,
    };
    write_handle(driver, sibling, "wx", stream, config).await
}

async fn write_handle(
    driver: &dyn FsDriver,
    path: &str,
    flags: &str,
    stream: StreamedUpload,
    config: &RequestConfig,
) -> Result<usize, RequestError> {
    let mut handle = FileCloseGuard::new(
        bounded(config, driver.open(path, flags, 0o666)).await?,
        &config.control,
    );
    let file = handle.handle();
    let written = async {
        let written = stream.write_to(file.as_ref(), path, config).await?;
        bounded(config, file.sync()).await?;
        Ok::<_, RequestError>(written)
    }
    .await;
    let closed = bounded(config, handle.close()).await;
    let written = written?;
    closed?;
    usize::try_from(written).map_err(|_| {
        RequestError::Fs(
            FsError::new(ErrorCode::Efbig)
                .with_syscall("write")
                .with_path(path),
        )
    })
}

/// Removes an upload's temporary sibling when the request that was writing
/// it is dropped part-way: abandoned by its client, timed out or aborted.
struct SiblingGuard {
    sibling: Option<ActiveSibling>,
    driver: Arc<dyn FsDriver>,
    control: RuntimeControl,
}

impl SiblingGuard {
    fn path(&self) -> &str {
        self.sibling
            .as_ref()
            .map_or("", |sibling| sibling.path.as_str())
    }
}

impl Drop for SiblingGuard {
    fn drop(&mut self) {
        let Some(sibling) = self.sibling.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let driver = Arc::clone(&self.driver);
        // Counted before it is spawned, so a close waits for the removal.
        let task = TaskGuard::new(
            Arc::clone(&self.control.background_tasks),
            Arc::clone(&self.control.drained),
        );
        runtime.spawn(async move {
            let _task = task;
            // If the rename had already happened there is nothing here.
            let _ = driver.unlink(&sibling.path).await;
        });
    }
}

fn parent_path(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/",
        Some((parent, _)) => parent,
    }
}

fn child_path(directory: &str, name: &str) -> String {
    format!("{}/{name}", directory.trim_end_matches('/'))
}

/// The temporary siblings this process is writing right now. Any other
/// reserved name in a directory was left by a process that stopped mid-upload.
fn active_siblings() -> &'static ArcSwap<im::HashSet<String>> {
    static ACTIVE: OnceLock<ArcSwap<im::HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

struct ActiveSibling {
    path: String,
}

impl ActiveSibling {
    /// A name beside `target` that no client can use and no other upload has.
    fn new(target: &str) -> Self {
        use std::hash::{BuildHasher, Hasher};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        // Two independently keyed hashes of a per-process sequence number:
        // unique here, and not repeated by another process on the same drive.
        let word = || {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u64(sequence);
            hasher.write_u32(std::process::id());
            hasher.finish()
        };
        let path = child_path(
            parent_path(target),
            &format!("{RESERVED_NAME_PREFIX}{:016x}{:016x}", word(), word()),
        );
        active_siblings().rcu(|active| active.update(path.clone()));
        Self { path }
    }
}

impl Drop for ActiveSibling {
    fn drop(&mut self) {
        active_siblings().rcu(|active| active.without(&self.path));
    }
}

/// List a directory without the server's own temporary names.
async fn list_directory(
    drive: &DriveConfig,
    path: &str,
    max_entries: usize,
) -> Result<Vec<mount_rs_core::DirEntry>, RequestError> {
    let mut entries = drive
        .driver()
        .readdir_bounded(path, max_entries)
        .await
        .map_err(directory_listing_error)?;
    entries.retain(|entry| !entry.name.starts_with(RESERVED_NAME_PREFIX));
    Ok(entries)
}

async fn handle_file_delete(
    headers: &hyper::HeaderMap,
    drive: DriveConfig,
    path: String,
    config: RequestConfig,
) -> Result<Response<HttpBody>, RequestError> {
    // Drives answer EBUSY here, which would reach the client as a 503 that
    // invites a retry. The refusal is permanent, so say so.
    if path == "/" {
        return Err(RequestError::bad_request(
            "the drive root cannot be removed",
        ));
    }
    let driver = drive.driver();
    let _lock = Preconditions::new(headers, &drive, &path)
        .lock(driver.as_ref(), &config)
        .await?;
    let stats = driver.lstat(&path).await.map_err(RequestError::Fs)?;
    if stats.is_directory() {
        match driver.rmdir(&path).await {
            Err(error)
                if error.code == ErrorCode::Enotempty
                    && remove_abandoned_siblings(
                        driver.as_ref(),
                        &path,
                        config.max_directory_entries,
                    )
                    .await =>
            {
                driver.rmdir(&path).await.map_err(RequestError::Fs)?;
            }
            result => result.map_err(RequestError::Fs)?,
        }
    } else {
        driver.unlink(&path).await.map_err(RequestError::Fs)?;
    }
    Ok(empty_response(StatusCode::NO_CONTENT))
}

/// A directory that lists as empty may still hold temporary siblings left
/// by a process that stopped mid-upload; no client can name them, so remove
/// them here. Returns whether the directory is worth another `rmdir`.
async fn remove_abandoned_siblings(driver: &dyn FsDriver, path: &str, max_entries: usize) -> bool {
    let Ok(entries) = driver.readdir_bounded(path, max_entries).await else {
        return false;
    };
    let abandoned = |name: &str| {
        name.starts_with(RESERVED_NAME_PREFIX)
            && !active_siblings().load().contains(&child_path(path, name))
    };
    if entries.is_empty() || !entries.iter().all(|entry| abandoned(&entry.name)) {
        return false;
    }
    for entry in entries {
        if driver.unlink(&child_path(path, &entry.name)).await.is_err() {
            return false;
        }
    }
    true
}

async fn handle_entries(
    method: Method,
    headers: &hyper::HeaderMap,
    drive: DriveConfig,
    path: String,
    config: RequestConfig,
) -> Result<Response<HttpBody>, RequestError> {
    if method != Method::GET && method != Method::HEAD {
        return Err(RequestError::method_not_allowed("GET, HEAD"));
    }
    let entries = list_directory(&drive, &path, config.max_directory_entries).await?;
    let payload = serde_json::to_vec(&entries)
        .map_err(|_| RequestError::Fs(FsError::backend("failed to encode directory entries")))?;
    if payload.len() > config.max_response_bytes {
        return Err(RequestError::response_too_large());
    }
    entries_response(payload, headers, method == Method::HEAD).await
}

// Bound compression work across all drives without occupying async executor threads.
static LISTING_ENCODERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

fn encoding_quality(value: &str) -> Option<u16> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match whole {
        "0" => Some(fraction.parse::<u16>().unwrap_or(0) * 10u16.pow(3 - fraction.len() as u32)),
        "1" if fraction.bytes().all(|b| b == b'0') => Some(1000),
        _ => None,
    }
}

fn listing_encodings(headers: &hyper::HeaderMap) -> (u16, u16) {
    let mut gzip = None;
    let mut identity = None;
    let mut wildcard = None;
    for header in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(value) = header.to_str() else { continue };
        for item in value.split(',') {
            let mut parts = item.trim().split(';');
            let name = parts.next().unwrap_or("").trim();
            let mut quality = 1000;
            let mut seen_quality = false;
            for parameter in parts {
                let Some((key, value)) = parameter.trim().split_once('=') else {
                    quality = 0;
                    break;
                };
                if !key.trim().eq_ignore_ascii_case("q") || seen_quality {
                    quality = 0;
                    break;
                }
                seen_quality = true;
                quality = encoding_quality(value.trim()).unwrap_or(0);
            }
            let slot = if name.eq_ignore_ascii_case("gzip") {
                &mut gzip
            } else if name.eq_ignore_ascii_case("identity") {
                &mut identity
            } else if name == "*" {
                &mut wildcard
            } else {
                continue;
            };
            // Conflicting duplicate preferences resolve conservatively.
            *slot = Some(slot.map_or(quality, |old: u16| old.min(quality)));
        }
    }
    (
        gzip.or(wildcard).unwrap_or(0),
        identity.unwrap_or(if wildcard == Some(0) { 0 } else { 1000 }),
    )
}

async fn entries_response(
    payload: Vec<u8>,
    headers: &hyper::HeaderMap,
    head: bool,
) -> Result<Response<HttpBody>, RequestError> {
    let (gzip, identity) = listing_encodings(headers);
    if gzip == 0 && identity == 0 {
        let mut response = error_response(RequestError::not_acceptable());
        response.headers_mut().insert(
            header::VARY,
            header::HeaderValue::from_static("Accept-Encoding"),
        );
        return Ok(response);
    }
    let use_gzip = gzip > 0 && gzip >= identity && (payload.len() >= 1024 || identity == 0);
    let bytes = if use_gzip {
        let permit = LISTING_ENCODERS
            .acquire()
            .await
            .map_err(|_| RequestError::Fs(FsError::backend("listing encoder unavailable")))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&payload)?;
            encoder.finish()
        })
        .await
        .map_err(|_| RequestError::Fs(FsError::backend("listing encoder failed")))?
        .map_err(|_| RequestError::Fs(FsError::backend("listing compression failed")))?
    } else {
        payload
    };
    let mut response = json_response(StatusCode::OK, Bytes::from(bytes), head);
    response.headers_mut().insert(
        header::VARY,
        header::HeaderValue::from_static("Accept-Encoding"),
    );
    if use_gzip {
        response.headers_mut().insert(
            header::CONTENT_ENCODING,
            header::HeaderValue::from_static("gzip"),
        );
    }
    Ok(response)
}

fn directory_listing_error(error: FsError) -> RequestError {
    if error.code == ErrorCode::Eoverflow {
        RequestError::response_too_large()
    } else {
        RequestError::Fs(error)
    }
}

async fn handle_operation(
    method: Method,
    body: HttpBody,
    drive: DriveConfig,
    operation: String,
    config: RequestConfig,
) -> Result<Response<HttpBody>, RequestError> {
    if method != Method::POST {
        return Err(RequestError::method_not_allowed("POST"));
    }
    match operation.as_str() {
        "mkdir" => {
            let request: MkdirRequest =
                parse_json_body(body, config.max_request_bytes, &config.control).await?;
            let path = validate_virtual_path(&request.path)?;
            let created = drive
                .driver()
                .mkdir(
                    &path,
                    MkdirOptions {
                        recursive: request.recursive,
                        // Permission bits only: a client does not get to
                        // set the file type, setuid, setgid or sticky bits.
                        mode: request.mode.map(|mode| mode & 0o777),
                    },
                )
                .await
                .map_err(RequestError::Fs)?;
            let payload = serde_json::to_vec(&MkdirPayload {
                path: &path,
                created,
            })
            .map_err(|_| RequestError::Fs(FsError::backend("failed to encode mkdir result")))?;
            Ok(json_response(StatusCode::OK, Bytes::from(payload), false))
        }
        "rename" => {
            let request: RenameRequest =
                parse_json_body(body, config.max_request_bytes, &config.control).await?;
            let from = validate_virtual_path(&request.from)?;
            let to = validate_virtual_path(&request.to)?;
            drive
                .driver()
                .rename(&from, &to)
                .await
                .map_err(RequestError::Fs)?;
            Ok(json_response(
                StatusCode::OK,
                Bytes::from_static(br#"{"ok":true}"#),
                false,
            ))
        }
        "truncate" => {
            let request: TruncateRequest =
                parse_json_body(body, config.max_request_bytes, &config.control).await?;
            let path = validate_virtual_path(&request.path)?;
            drive
                .driver()
                .truncate(&path, request.length)
                .await
                .map_err(RequestError::Fs)?;
            Ok(json_response(
                StatusCode::OK,
                Bytes::from_static(br#"{"ok":true}"#),
                false,
            ))
        }
        "sync" => {
            let bytes =
                read_limited_body(body, config.max_request_bytes, 0, &config.control).await?;
            if !bytes.is_empty() {
                return Err(RequestError::bad_request(
                    "sync does not accept a request body",
                ));
            }
            drive.driver().syncfs().await.map_err(RequestError::Fs)?;
            Ok(empty_response(StatusCode::NO_CONTENT))
        }
        _ => Err(RequestError::not_found()),
    }
}

async fn next_request_frame(
    body: &mut HttpBody,
    shutdown: &Notify,
    closing: &AtomicUsize,
) -> Result<Option<Frame<Bytes>>, RequestError> {
    if closing.load(Ordering::Acquire) != 0 {
        return Err(RequestError::server_closing());
    }
    let notified = shutdown.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if closing.load(Ordering::Acquire) != 0 {
        return Err(RequestError::server_closing());
    }
    tokio::select! {
        _ = notified => Err(RequestError::server_closing()),
        frame = body.frame() => match frame {
            None => Ok(None),
            Some(Ok(frame)) => Ok(Some(frame)),
            Some(Err(_)) => Err(RequestError::Client {
                status: StatusCode::BAD_REQUEST,
                code: ErrorCode::Eio.as_str(),
                message: "request body read failed".to_owned(),
                close: true,
                content_range: None,
                allow: None,
                authenticate: false,
            }),
        },
    }
}

async fn parse_json_body<T: for<'de> Deserialize<'de>>(
    body: HttpBody,
    max_request_bytes: usize,
    control: &RuntimeControl,
) -> Result<T, RequestError> {
    let bytes = read_limited_body(body, max_request_bytes, 0, control).await?;
    serde_json::from_slice(&bytes).map_err(|_| RequestError::bad_request("invalid JSON request"))
}

async fn read_limited_body(
    mut body: HttpBody,
    max_request_bytes: usize,
    expected_bytes: usize,
    control: &RuntimeControl,
) -> Result<Vec<u8>, RequestError> {
    let mut bytes = Vec::with_capacity(expected_bytes.min(max_request_bytes));
    while let Some(frame) =
        next_request_frame(&mut body, &control.shutdown, &control.closing).await?
    {
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if bytes.len().saturating_add(data.len()) > max_request_bytes {
            drop(bytes);
            discard_body(body, control).await;
            return Err(RequestError::body_too_large());
        }
        bytes.extend_from_slice(&data);
    }
    Ok(bytes)
}

/// The most of a refused request's body read and dropped before replying.
const DISCARD_BODY_BYTES: usize = 64 << 20;
/// The longest a refused request's body is read and dropped before replying.
const DISCARD_BODY_TIME: Duration = Duration::from_secs(5);

/// Answer a request without using its body, after reading the body out.
async fn refuse(
    body: HttpBody,
    error: RequestError,
    control: &RuntimeControl,
) -> Response<HttpBody> {
    discard_body(body, control).await;
    error_response(error)
}

/// Read and drop what is left of a request body, within byte and time
/// limits, so a refusal reaches the client.
///
/// A client that sends its whole body before reading the reply (Node's
/// `fetch`, Python's `http.client`) gets a connection reset under its write
/// if the server replies and closes with body bytes unread, and never sees
/// the reply. Past the limits the connection is closed anyway.
async fn discard_body(mut body: HttpBody, control: &RuntimeControl) {
    if body.is_end_stream() {
        return;
    }
    let drain = async {
        let mut read = 0_usize;
        while read < DISCARD_BODY_BYTES {
            match next_request_frame(&mut body, &control.shutdown, &control.closing).await {
                Ok(Some(frame)) => {
                    read = read.saturating_add(frame.data_ref().map_or(0, Bytes::len));
                }
                Ok(None) | Err(_) => return,
            }
        }
    };
    let _ = tokio::time::timeout(DISCARD_BODY_TIME, drain).await;
}

fn parse_route(raw_path: &str) -> Result<Route, RequestError> {
    let segments = decode_uri_segments(raw_path)?;
    match segments.as_slice() {
        [health] if health == "healthz" => Ok(Route::Health { readiness: false }),
        [ready] if ready == "readyz" => Ok(Route::Health { readiness: true }),
        [version, drives] if version == "v1" && drives == "drives" => Ok(Route::Discovery),
        [version, drives, drive, kind, rest @ ..]
            if version == "v1" && drives == "drives" && kind == "fs" =>
        {
            Ok(Route::File {
                drive: drive.clone(),
                path: path_from_segments(rest),
            })
        }
        [version, drives, drive, kind, rest @ ..]
            if version == "v1" && drives == "drives" && kind == "entries" =>
        {
            Ok(Route::Entries {
                drive: drive.clone(),
                path: path_from_segments(rest),
            })
        }
        [version, drives, drive, kind, rest @ ..]
            if version == "v1"
                && drives == "drives"
                && matches!(kind.as_str(), "stat" | "lstat" | "statfs" | "readlink") =>
        {
            Ok(Route::Metadata {
                drive: drive.clone(),
                path: path_from_segments(rest),
                kind: kind.clone(),
            })
        }
        [version, drives, drive, kind, operation]
            if version == "v1" && drives == "drives" && kind == "ops" =>
        {
            Ok(Route::Operation {
                drive: drive.clone(),
                operation: operation.clone(),
            })
        }
        _ => Err(RequestError::not_found()),
    }
}

fn decode_uri_segments(raw_path: &str) -> Result<Vec<String>, RequestError> {
    if !raw_path.starts_with('/') {
        return Err(RequestError::bad_request("request path must be absolute"));
    }
    let raw_segments: Vec<_> = raw_path.split('/').collect();
    let mut segments = Vec::with_capacity(raw_segments.len().saturating_sub(1));
    for (index, raw_segment) in raw_segments.iter().enumerate().skip(1) {
        if raw_segment.is_empty() {
            if index + 1 == raw_segments.len() {
                continue;
            }
            return Err(RequestError::bad_request(
                "empty path segment is not allowed",
            ));
        }
        let segment = percent_decode_str(raw_segment)
            .decode_utf8()
            .map_err(|_| RequestError::bad_request("request path is not valid UTF-8"))?;
        if segment.starts_with(RESERVED_NAME_PREFIX) {
            return Err(RequestError::reserved_name());
        }
        if segment == "."
            || segment == ".."
            || segment.contains('/')
            || segment.contains('\\')
            || segment.contains('\0')
        {
            return Err(RequestError::bad_request(
                "path traversal and encoded separators are not allowed",
            ));
        }
        segments.push(segment.into_owned());
    }
    Ok(segments)
}

fn path_from_segments(segments: &[String]) -> String {
    if segments.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", segments.join("/"))
    }
}

fn validate_virtual_path(path: &str) -> Result<String, RequestError> {
    if !path.starts_with('/') || path.contains('\0') || path.contains('\\') {
        return Err(RequestError::bad_request(
            "filesystem path must be absolute and use slash separators",
        ));
    }
    let pieces: Vec<_> = path.split('/').collect();
    for (index, piece) in pieces.iter().enumerate().skip(1) {
        if piece.is_empty() {
            if index + 1 == pieces.len() {
                continue;
            }
            return Err(RequestError::bad_request(
                "empty path segment is not allowed",
            ));
        }
        if *piece == "." || *piece == ".." {
            return Err(RequestError::bad_request("path traversal is not allowed"));
        }
        if piece.starts_with(RESERVED_NAME_PREFIX) {
            return Err(RequestError::reserved_name());
        }
    }
    Ok(path_from_segments(
        &pieces
            .into_iter()
            .skip(1)
            .filter(|piece| !piece.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    ))
}

/// Accept failures that say nothing about the listener: the caller that
/// was being accepted went away. These are retried at once.
fn retry_accept(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind;
    matches!(
        kind,
        ErrorKind::Interrupted | ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset
    )
}

/// Wait for the next connection, outliving failed accepts.
///
/// An accept fails for reasons that pass: the process is briefly out of file
/// descriptors, or a client gave up before it was accepted. Ending the
/// listener on one of those leaves a process that looks alive and never
/// answers again. Each other failure is reported, and the pause (10 ms,
/// doubling to 1 s) keeps a persistent one from spinning.
async fn accept_next<T, F, Fut>(mut accept: F, mut failed: impl FnMut(&std::io::Error)) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    let mut pause = Duration::from_millis(10);
    loop {
        match accept().await {
            Ok(accepted) => return accepted,
            Err(error) if retry_accept(error.kind()) => {}
            Err(error) => {
                failed(&error);
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_secs(1));
            }
        }
    }
}

fn authorized(headers: &hyper::HeaderMap, drive: &DriveConfig) -> bool {
    bearer_token(headers).is_some_and(|token| drive.token_matches(token))
}

fn bearer_token(headers: &hyper::HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    // The scheme name is case-insensitive (RFC 9110 section 11.1).
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim_start_matches(' ');
    if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return None;
    }
    Some(token)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(left.get(index).copied().unwrap_or(0))
            ^ usize::from(right.get(index).copied().unwrap_or(0));
    }
    difference == 0
}

fn content_length_exceeds(headers: &hyper::HeaderMap, limit: u64) -> bool {
    declared_length(headers).is_some_and(|length| length > limit)
}

fn declared_length(headers: &hyper::HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

/// Whether a request replaces a file with its body: `PUT /v1/drives/{id}/fs/...`.
pub fn is_file_upload(method: &Method, path: &str) -> bool {
    method == Method::PUT
        && path
            .strip_prefix("/v1/drives/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(_, rest)| rest == "fs" || rest.starts_with("fs/"))
}

/// Whether a request is a file upload that may be streamed rather than held
/// in memory: one that declares more than `max_request_bytes`, or no length.
pub fn streams_upload<B>(request: &Request<B>, max_request_bytes: usize) -> bool {
    is_file_upload(request.method(), request.uri().path())
        && declared_length(request.headers()).is_none_or(|length| length > max_request_bytes as u64)
}

fn body_limit(method: &Method, path: &str, config: &RequestConfig) -> u64 {
    if is_file_upload(method, path) {
        config.max_upload_bytes
    } else {
        config.max_request_bytes as u64
    }
}

fn positive_duration(duration: Duration) -> Duration {
    if duration.is_zero() {
        Duration::from_millis(1)
    } else {
        duration
    }
}

fn validate_bind_host(host: &str) -> Result<(), HttpServerError> {
    let normalized = unbracket_host(host).ok_or_else(|| HttpServerError::InsecureBind {
        host: host.to_owned(),
    })?;
    let is_loopback = normalized.eq_ignore_ascii_case("localhost")
        || normalized
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if is_loopback {
        Ok(())
    } else {
        Err(HttpServerError::InsecureBind {
            host: host.to_owned(),
        })
    }
}

fn parse_range(value: &str, size: u64) -> Result<ByteRange, ()> {
    let Some(spec) = value.strip_prefix("bytes=") else {
        return Err(());
    };
    if spec.contains(',') {
        return Err(());
    }
    let Some((start, end)) = spec.split_once('-') else {
        return Err(());
    };
    if size == 0 {
        return Err(());
    }
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let length = suffix.min(size);
        return Ok(ByteRange {
            start: size - length,
            length,
        });
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= size {
        return Err(());
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(size - 1)
    };
    if end < start {
        return Err(());
    }
    Ok(ByteRange {
        start,
        length: end - start + 1,
    })
}

fn error_response(error: RequestError) -> Response<HttpBody> {
    let mut retry_after = false;
    let (status, code, message, close, content_range, allow, authenticate) = match error {
        RequestError::Fs(error) => (
            {
                retry_after = matches!(error.code, ErrorCode::Eagain | ErrorCode::Ebusy);
                status_for_error(error.code)
            },
            error.code.as_str(),
            error.code.description().to_owned(),
            false,
            None,
            None,
            false,
        ),
        RequestError::Client {
            status,
            code,
            message,
            close,
            content_range,
            allow,
            authenticate,
        } => (
            status,
            code,
            message,
            close,
            content_range,
            allow,
            authenticate,
        ),
    };
    let body = serde_json::to_vec(&ErrorEnvelope {
        error: ErrorDetail {
            code: code.to_owned(),
            message,
        },
    })
    .unwrap_or_else(|_| b"{\"error\":{\"code\":\"EIO\",\"message\":\"internal error\"}}".to_vec());
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, body.len().to_string());
    if close {
        builder = builder.header(header::CONNECTION, "close");
    }
    if let Some(content_range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, content_range);
    }
    if let Some(allow) = allow {
        builder = builder.header(header::ALLOW, allow);
    }
    if authenticate {
        builder = builder.header(header::WWW_AUTHENTICATE, "Bearer");
    }
    if retry_after {
        builder = builder.header(header::RETRY_AFTER, "1");
    }
    builder
        .body(full_body(Bytes::from(body)))
        .unwrap_or_else(|_| Response::new(full_body(Bytes::new())))
}

/// The HTTP status for a filesystem error. A condition the client caused
/// or can correct is a 4xx; one only the server or its storage can clear is
/// a 5xx, and only an error with no better description is a 500.
fn status_for_error(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::Enoent | ErrorCode::Enotdir => StatusCode::NOT_FOUND,
        ErrorCode::Eexist | ErrorCode::Enotempty | ErrorCode::Eisdir => StatusCode::CONFLICT,
        ErrorCode::Einval
        | ErrorCode::Eloop
        | ErrorCode::Exdev
        | ErrorCode::Emlink
        | ErrorCode::Enxio => StatusCode::BAD_REQUEST,
        ErrorCode::Eacces | ErrorCode::Eperm | ErrorCode::Erofs => StatusCode::FORBIDDEN,
        // A drive that failed closed answers ESTALE for everything, and a
        // drive being shut down EBADF: the files are not gone, the drive is
        // unavailable.
        ErrorCode::Estale | ErrorCode::Ebadf | ErrorCode::Eagain | ErrorCode::Ebusy => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        ErrorCode::Enospc | ErrorCode::Edquot => StatusCode::INSUFFICIENT_STORAGE,
        ErrorCode::Efbig | ErrorCode::Eoverflow => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::Enametoolong => StatusCode::URI_TOO_LONG,
        ErrorCode::Enotsup | ErrorCode::Enosys => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn json_response(status: StatusCode, bytes: Bytes, head: bool) -> Response<HttpBody> {
    let length = bytes.len();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, length.to_string())
        .body(if head {
            head_body(length as u64)
        } else {
            full_body(bytes)
        })
        .unwrap_or_else(|_| Response::new(full_body(Bytes::new())))
}

fn health_json_response(status: StatusCode, bytes: Bytes, head: bool) -> Response<HttpBody> {
    let mut response = json_response(status, bytes, head);
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

fn empty_response(status: StatusCode) -> Response<HttpBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, "0")
        .body(full_body(Bytes::new()))
        .unwrap_or_else(|_| Response::new(full_body(Bytes::new())))
}

fn full_body(bytes: Bytes) -> HttpBody {
    Full::new(bytes)
        .map_err(|never| -> BoxError { match never {} })
        .boxed_unsync()
}

/// A body which emits no bytes but preserves the representation length for a
/// HEAD response.  Hyper uses the body size hint when constructing the wire
/// headers, while the empty stream keeps HEAD from transferring the payload.
struct HeadBody {
    length: u64,
}

impl HttpBodyTrait for HeadBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(None)
    }

    fn size_hint(&self) -> SizeHint {
        let mut hint = SizeHint::new();
        hint.set_exact(self.length);
        hint
    }
}

fn head_body(length: u64) -> HttpBody {
    HeadBody { length }.boxed_unsync()
}

// Data, errors and EOF share one ordered queue. A separate error channel can
// close between polling an empty data queue and polling that second channel,
// incorrectly reporting EOF while the final data frame is already queued.
struct OrderedResponseBody {
    receiver: tokio::sync::mpsc::Receiver<Result<Bytes, BoxError>>,
}
impl HttpBodyTrait for OrderedResponseBody {
    type Data = Bytes;
    type Error = BoxError;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        self.get_mut()
            .receiver
            .poll_recv(cx)
            .map(|item| item.map(|result| result.map(Frame::data)))
    }
}

async fn send_response_failure(
    sender: &tokio::sync::mpsc::Sender<Result<Bytes, BoxError>>,
    error: BoxError,
    control: &RuntimeControl,
) {
    let notified = control.shutdown.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if control.closing.load(Ordering::Acquire) != 0 {
        return;
    }
    tokio::select! {
        _ = notified => {}
        _ = sender.send(Err(error)) => {}
    }
}

fn stream_file(
    handle: Arc<dyn FileHandle>,
    start: u64,
    length: u64,
    chunk_size: usize,
    send_timeout: Duration,
    kill: Option<ConnectionKill>,
    control: RuntimeControl,
) -> HttpBody {
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    let body = OrderedResponseBody { receiver };
    let task = TaskGuard::new(
        Arc::clone(&control.background_tasks),
        Arc::clone(&control.drained),
    );
    tokio::spawn(async move {
        let _task = task;
        let mut close_guard = FileCloseGuard::new(handle, &control);
        let handle = close_guard.handle();
        let end = start.saturating_add(length);
        let mut position = start;
        // Small ranges should not reserve an entire streaming chunk. Keep the
        // configured bound for large responses and avoid allocating for EOF.
        let buffer_size = length.min(chunk_size.max(1) as u64) as usize;
        let mut buffer = vec![0_u8; buffer_size];
        let mut failure: Option<BoxError> = None;
        while position < end {
            let wanted = (end - position).min(buffer.len() as u64) as usize;
            if control.closing.load(Ordering::Acquire) != 0 {
                break;
            }
            let notified = control.shutdown.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if control.closing.load(Ordering::Acquire) != 0 {
                break;
            }
            let read_result = tokio::select! {
                _ = notified => break,
                result = handle.read(&mut buffer[..wanted], Some(position)) => result,
            };
            match read_result {
                Ok(0) => {
                    failure = Some(Box::new(
                        FsError::new(ErrorCode::Eio)
                            .with_syscall("read")
                            .with_message("short HTTP response body"),
                    ));
                    break;
                }
                Ok(count) if count <= wanted => {
                    let notified = control.shutdown.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if control.closing.load(Ordering::Acquire) != 0 {
                        break;
                    }
                    let chunk = Bytes::copy_from_slice(&buffer[..count]);
                    let send_result = tokio::select! {
                        _ = notified => break,
                        result = tokio::time::timeout(send_timeout, sender.send(Ok(chunk))) => result,
                    };
                    match send_result {
                        Ok(Ok(())) => {}
                        // The response was dropped: the client went away.
                        Ok(Err(_)) => break,
                        Err(_) => {
                            // Nothing has taken a chunk for a whole request
                            // timeout: the client stopped reading. Give up
                            // the file handle, and the connection with it
                            // when it carries nothing else.
                            if let Some(kill) = &kill {
                                kill.0.notify_one();
                            }
                            break;
                        }
                    }
                    position += count as u64;
                }
                Ok(_) => {
                    failure = Some(Box::new(
                        FsError::new(ErrorCode::Eio)
                            .with_syscall("read")
                            .with_message("driver returned more bytes than requested"),
                    ));
                    break;
                }
                Err(error) => {
                    failure = Some(Box::new(error));
                    break;
                }
            }
        }
        if let Err(error) = close_guard.close().await
            && failure.is_none()
        {
            failure = Some(Box::new(error));
        }
        if let Some(error) = failure {
            send_response_failure(&sender, error, &control).await;
        }
    });
    body.boxed_unsync()
}

async fn socket_address(host: &str, port: u16) -> Result<SocketAddr, std::io::Error> {
    let host = unbracket_host(host).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "malformed bracketed host")
    })?;
    if let Ok(address) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(address, port));
    }
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    }
    if host.is_empty() {
        return Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port));
    }
    tokio::net::lookup_host((host, port))
        .await?
        .next()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                format!("host {host} did not resolve to an address"),
            )
        })
}

fn unbracket_host(host: &str) -> Option<&str> {
    match (host.starts_with('['), host.ends_with(']')) {
        (true, true) if host.len() > 2 => Some(&host[1..host.len() - 1]),
        (false, false) if !host.contains(['[', ']']) => Some(host),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_memfs::MemoryFs;

    #[test]
    fn interrupted_and_aborted_accepts_keep_the_listener_running() {
        use std::io::ErrorKind;
        for kind in [
            ErrorKind::Interrupted,
            ErrorKind::ConnectionAborted,
            ErrorKind::ConnectionReset,
        ] {
            assert!(retry_accept(kind));
        }
        assert!(!retry_accept(ErrorKind::PermissionDenied));
        assert!(!retry_accept(ErrorKind::OutOfMemory));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_accept_is_reported_and_the_listener_keeps_accepting() {
        let attempts = std::cell::Cell::new(0);
        let reported = std::cell::RefCell::new(Vec::new());
        let started = tokio::time::Instant::now();
        let accepted = accept_next(
            || {
                attempts.set(attempts.get() + 1);
                let attempt = attempts.get();
                async move {
                    match attempt {
                        // The caller went away first: retried at once, and
                        // not worth reporting.
                        1 => Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted)),
                        // EMFILE, ENFILE: out of file descriptors.
                        2 | 3 => Err(std::io::Error::from_raw_os_error(24)),
                        4 => Err(std::io::Error::from_raw_os_error(23)),
                        _ => Ok(attempt),
                    }
                }
            },
            |error| reported.borrow_mut().push(error.raw_os_error()),
        )
        .await;
        assert_eq!(accepted, 5);
        assert_eq!(*reported.borrow(), vec![Some(24), Some(24), Some(23)]);
        // 10 ms, 20 ms, 40 ms: it backs off instead of spinning.
        assert_eq!(started.elapsed(), Duration::from_millis(70));
    }

    #[tokio::test(start_paused = true)]
    async fn the_pause_after_a_failed_accept_stops_growing_at_one_second() {
        let attempts = std::cell::Cell::new(0);
        let started = tokio::time::Instant::now();
        accept_next(
            || {
                attempts.set(attempts.get() + 1);
                let attempt = attempts.get();
                async move {
                    if attempt <= 10 {
                        Err(std::io::Error::from_raw_os_error(24))
                    } else {
                        Ok(())
                    }
                }
            },
            |_error| {},
        )
        .await;
        // 10+20+40+80+160+320+640 ms, then one second each for the rest.
        assert_eq!(started.elapsed(), Duration::from_millis(1270 + 3000));
    }

    #[test]
    fn listing_encoding_preferences() {
        for (value, expected) in [
            ("", (0, 1000)),
            ("gzip", (1000, 1000)),
            ("GZIP;q=0.5, identity;q=0.2", (500, 200)),
            ("*;q=1, gzip;q=0", (0, 1000)),
            ("*;q=0", (0, 0)),
            ("*;q=0, identity;q=1", (0, 1000)),
            ("gzip;q=1.001", (0, 1000)),
            ("gzip;q=0.0001", (0, 1000)),
            ("gzip;q=NaN", (0, 1000)),
            ("gzip;q=1;q=0", (0, 1000)),
            ("gzip;q=0, gzip", (0, 1000)),
        ] {
            let mut headers = hyper::HeaderMap::new();
            headers.insert(header::ACCEPT_ENCODING, value.parse().unwrap());
            assert_eq!(listing_encodings(&headers), expected, "{value}");
        }
        let mut headers = hyper::HeaderMap::new();
        headers.append(header::ACCEPT_ENCODING, "br".parse().unwrap());
        headers.append(
            header::ACCEPT_ENCODING,
            "gzip, identity;q=0".parse().unwrap(),
        );
        assert_eq!(listing_encodings(&headers), (1000, 0));
        assert_eq!(listing_encodings(&hyper::HeaderMap::new()), (0, 1000));
    }

    #[tokio::test]
    async fn listing_compression_roundtrip_and_head_length() {
        use std::io::Read;
        let payload = vec![b'a'; 67054];
        let mut headers = hyper::HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
        let get = entries_response(payload.clone(), &headers, false)
            .await
            .unwrap();
        assert_eq!(get.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(get.headers()[header::VARY], "Accept-Encoding");
        let length = get.headers()[header::CONTENT_LENGTH].clone();
        let bytes = get.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            length.to_str().unwrap().parse::<usize>().unwrap(),
            bytes.len()
        );
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_ref())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, payload);
        let head = entries_response(payload.clone(), &headers, true)
            .await
            .unwrap();
        assert_eq!(head.headers()[header::CONTENT_LENGTH], length);
        assert_eq!(head.headers()[header::CONTENT_ENCODING], "gzip");
        assert!(
            head.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );
        headers.insert(header::ACCEPT_ENCODING, "gzip;q=0".parse().unwrap());
        let plain = entries_response(payload.clone(), &headers, false)
            .await
            .unwrap();
        assert!(!plain.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(
            plain.into_body().collect().await.unwrap().to_bytes(),
            payload
        );
        headers.insert(header::ACCEPT_ENCODING, "*;q=0".parse().unwrap());
        assert_eq!(
            entries_response(payload, &headers, false)
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_ACCEPTABLE
        );
        headers.insert(
            header::ACCEPT_ENCODING,
            "gzip, identity;q=0".parse().unwrap(),
        );
        assert_eq!(
            entries_response(b"[]".to_vec(), &headers, false)
                .await
                .unwrap()
                .headers()[header::CONTENT_ENCODING],
            "gzip"
        );
    }

    mod handoff_waker {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/support/handoff_waker.rs"
        ));
    }

    #[test]
    fn final_response_frame_survives_producer_close_during_poll_registration() {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let mut body = OrderedResponseBody { receiver };
        let waker = handoff_waker::on_clone(1, move || {
            sender
                .try_send(Ok(Bytes::from_static(b"complete")))
                .unwrap();
            drop(sender);
        });
        let mut cx = Context::from_waker(&waker);
        let Poll::Ready(Some(Ok(frame))) = Pin::new(&mut body).poll_frame(&mut cx) else {
            panic!("final data frame must be delivered before EOF");
        };
        assert_eq!(frame.into_data().unwrap(), b"complete".as_slice());
        assert!(matches!(
            Pin::new(&mut body).poll_frame(&mut cx),
            Poll::Ready(None)
        ));
    }

    #[tokio::test]
    async fn response_error_follows_queued_data_and_receiver_drop_releases_sender() {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let mut body = OrderedResponseBody { receiver };
        sender
            .send(Ok(Bytes::from_static(b"prefix")))
            .await
            .unwrap();
        sender
            .send(Err(Box::new(FsError::new(ErrorCode::Eio)) as BoxError))
            .await
            .unwrap();
        drop(sender);
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            b"prefix".as_slice()
        );
        assert!(body.frame().await.unwrap().is_err());
        assert!(body.frame().await.is_none());
        let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);
        drop(OrderedResponseBody { receiver });
        assert!(sender.send(Ok(Bytes::new())).await.is_err());
    }

    #[tokio::test]
    async fn full_error_queue_does_not_prevent_server_shutdown() {
        let control = RuntimeControl {
            background_tasks: Arc::new(AtomicUsize::new(0)),
            drained: Arc::new(Notify::new()),
            shutdown: Arc::new(Notify::new()),
            closing: Arc::new(AtomicUsize::new(0)),
        };
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(Bytes::from_static(b"queued")))
            .await
            .unwrap();
        let mut error = Box::pin(send_response_failure(
            &sender,
            Box::new(FsError::new(ErrorCode::Eio)),
            &control,
        ));
        assert!(std::future::poll_fn(|cx| Poll::Ready(error.as_mut().poll(cx).is_pending())).await);
        control.closing.store(1, Ordering::Release);
        control.shutdown.notify_waiters();
        tokio::time::timeout(Duration::from_secs(1), error)
            .await
            .unwrap();
        // A shutdown already in progress also bypasses the full queue.
        tokio::time::timeout(
            Duration::from_secs(1),
            send_response_failure(&sender, Box::new(FsError::new(ErrorCode::Eio)), &control),
        )
        .await
        .unwrap();
    }

    struct PendingCloseHandle {
        release: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl FileHandle for PendingCloseHandle {
        async fn read(
            &self,
            _buffer: &mut [u8],
            _position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            Ok(0)
        }

        async fn write(
            &self,
            _buffer: &[u8],
            _position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            Ok(0)
        }

        async fn stat(&self) -> mount_rs_core::Result<Stats> {
            Ok(Stats {
                dev: 0,
                ino: 0,
                mode: mount_rs_core::S_IFREG,
                nlink: 1,
                uid: 0,
                gid: 0,
                rdev: 0,
                size: 0,
                blksize: 0,
                blocks: 0,
                atime_ms: 0,
                mtime_ms: 0,
                ctime_ms: 0,
                birthtime_ms: 0,
            })
        }

        async fn truncate(&self, _length: u64) -> mount_rs_core::Result<()> {
            Ok(())
        }

        async fn close(&self) -> mount_rs_core::Result<()> {
            self.release.notified().await;
            Ok(())
        }
    }

    #[test]
    fn drive_debug_never_contains_the_bearer_token() {
        let drive = DriveConfig::new("private", Arc::new(MemoryFs::empty()), "test-secret-value")
            .expect("drive config");
        let debug = format!("{drive:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("test-secret-value"));
    }

    #[tokio::test]
    async fn non_loopback_bind_is_rejected_before_socket_bind() {
        let result = HttpServer::start(
            DriveRegistry::new(),
            HttpServerOptions {
                host: "0.0.0.0".to_owned(),
                ..HttpServerOptions::default()
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(HttpServerError::InsecureBind { host }) if host == "0.0.0.0"
        ));
    }

    #[tokio::test]
    async fn non_loopback_bind_is_served_once_allowed() {
        let server = HttpServer::start(
            DriveRegistry::new(),
            HttpServerOptions {
                host: "0.0.0.0".to_owned(),
                allow_non_loopback: true,
                ..HttpServerOptions::default()
            },
        )
        .await
        .expect("an allowed non-loopback bind starts");
        assert_eq!(server.url(), format!("http://0.0.0.0:{}", server.port()));
        // Every interface, loopback among them, reaches it.
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", server.port()))
            .await
            .expect("connect");
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        socket
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .expect("request");
        let mut response = String::new();
        socket
            .read_to_string(&mut response)
            .await
            .expect("response");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        server.close().await.expect("close");
    }

    #[test]
    fn uri_traversal_and_encoded_separators_fail_closed() {
        assert!(parse_route("/v1/drives/private/fs/%2e%2e/escape").is_err());
        assert!(parse_route("/v1/drives/private/fs/%2fescape").is_err());
        assert!(validate_virtual_path("/../escape").is_err());
    }

    #[test]
    fn ranges_are_single_and_bounded() {
        assert_eq!(parse_range("bytes=2-4", 10).unwrap().start, 2);
        assert_eq!(parse_range("bytes=2-4", 10).unwrap().length, 3);
        assert_eq!(parse_range("bytes=-3", 10).unwrap().start, 7);
        assert_eq!(parse_range("bytes=8-", 10).unwrap().length, 2);
        assert!(parse_range("bytes=0-1,3-4", 10).is_err());
        assert!(parse_range("bytes=10-", 10).is_err());
    }

    #[tokio::test]
    async fn cancelled_handle_cleanup_is_reserved_before_spawn() {
        let background_tasks = Arc::new(AtomicUsize::new(0));
        let drained = Arc::new(Notify::new());
        let control = RuntimeControl {
            background_tasks: Arc::clone(&background_tasks),
            drained: Arc::clone(&drained),
            shutdown: Arc::new(Notify::new()),
            closing: Arc::new(AtomicUsize::new(0)),
        };
        let release = Arc::new(Notify::new());
        let guard = FileCloseGuard::new(
            Arc::new(PendingCloseHandle {
                release: Arc::clone(&release),
            }),
            &control,
        );

        drop(guard);
        assert_eq!(background_tasks.load(Ordering::Acquire), 1);

        release.notify_one();
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_drain(Arc::new(AtomicUsize::new(0)), background_tasks, drained),
        )
        .await
        .expect("cancelled handle cleanup did not drain");
    }

    /// Wait until a spawned `close` has announced itself and had turns to
    /// reach its wait for the listener.
    async fn wait_until_close_is_waiting(server: &HttpServer) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while server.closing.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("close did not start");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    /// `listen` calls racing for an ephemeral port each bind one. The ones
    /// that lose are aborted, and their ports must not be the one reported.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_listens_report_the_port_of_the_listener_that_was_kept() {
        const LISTENS: usize = 4;
        for _ in 0..200 {
            let server = Arc::new(HttpServer::new(
                DriveRegistry::new(),
                HttpServerOptions::default(),
            ));
            let start = Arc::new(tokio::sync::Barrier::new(LISTENS));
            let mut listens = Vec::new();
            for _ in 0..LISTENS {
                let server = Arc::clone(&server);
                let start = Arc::clone(&start);
                listens.push(tokio::spawn(async move {
                    start.wait().await;
                    server.listen().await
                }));
            }
            for listen in listens {
                listen.await.expect("listen task").unwrap();
            }
            // The kept listener is the one behind the reported port: its
            // accept task holds the only socket still bound.
            let kept = server.listener.get().expect("a listener was kept");
            assert_eq!(Some(server.port()), kept.port);
            let stream = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::TcpStream::connect(("127.0.0.1", server.port())),
            )
            .await
            .expect("the reported port did not answer")
            .expect("the reported port is not the live listener");
            // A socket of an aborted listener may still accept at the TCP
            // level for a moment; only the live one serves a request.
            drop(stream);
            let response = tokio::time::timeout(
                Duration::from_secs(5),
                reqwest::get(format!("http://127.0.0.1:{}/missing", server.port())),
            )
            .await
            .expect("the reported port did not serve a request")
            .expect("the reported port is not the live listener");
            assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
            server.close().await.unwrap();
        }
    }

    /// Many closes, some cancelled while waiting, all share the listener's
    /// one result, and the accept task is never detached unjoined.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_and_cancelled_closes_all_see_the_listener_result() {
        for _ in 0..50 {
            let server = Arc::new(HttpServer::new(
                DriveRegistry::new(),
                HttpServerOptions {
                    drain_timeout: Duration::from_secs(5),
                    ..HttpServerOptions::default()
                },
            ));
            let release = Arc::new(Notify::new());
            let wait = Arc::clone(&release);
            server.adopt_listener(
                tokio::spawn(async move {
                    wait.notified().await;
                    panic!("injected listener failure");
                }),
                None,
            );
            let mut cancelled = Vec::new();
            let mut closes = Vec::new();
            for index in 0..12 {
                let closing = Arc::clone(&server);
                let close = tokio::spawn(async move { closing.close().await });
                if index % 3 == 0 {
                    cancelled.push(close);
                } else {
                    closes.push(close);
                }
            }
            wait_until_close_is_waiting(&server).await;
            for close in &cancelled {
                close.abort();
            }
            assert!(!server.listener.get().unwrap().done.borrow().is_some());
            release.notify_one();
            let mut errors = Vec::new();
            for close in closes {
                match close.await.expect("close task") {
                    Err(HttpServerError::Join(error)) => errors.push(error),
                    other => panic!("unexpected close result: {other:?}"),
                }
            }
            assert!(errors.iter().all(|error| Arc::ptr_eq(error, &errors[0])));
            assert!(errors[0].is_panic());
            match server.close().await {
                Err(HttpServerError::Join(error)) => assert!(Arc::ptr_eq(&error, &errors[0])),
                other => panic!("unexpected repeat close result: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn an_unbound_server_closes_without_a_listener() {
        let options = HttpServerOptions {
            port: 4321,
            ..HttpServerOptions::default()
        };
        let server = HttpServer::unbound(DriveRegistry::new(), options);
        server.listen().await.unwrap();
        assert_eq!(server.port(), 4321, "no listener was bound");
        assert!(!server.listener_failed());
        tokio::time::timeout(Duration::from_secs(1), server.close())
            .await
            .expect("close waited for a listener that does not exist")
            .unwrap();
        server.close().await.unwrap();
    }

    #[tokio::test]
    async fn unexpected_listener_exit_is_visible_before_close() {
        let server = HttpServer::new(DriveRegistry::new(), HttpServerOptions::default());
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move { wait.await.unwrap() });
        server.adopt_listener(task, None);
        assert!(!server.listener_failed());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !server.listener_failed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        server.close().await.unwrap();
        assert!(!server.listener_failed());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_close_keeps_listener_for_a_retry() {
        let server = Arc::new(HttpServer::new(
            DriveRegistry::new(),
            HttpServerOptions {
                drain_timeout: Duration::from_millis(25),
                ..HttpServerOptions::default()
            },
        ));
        let listener_started = Arc::new(Notify::new());
        let listener_release = Arc::new(Notify::new());
        let started = listener_started.notified();
        tokio::pin!(started);
        started.as_mut().enable();
        let task_started = Arc::clone(&listener_started);
        let task_release = Arc::clone(&listener_release);
        let task = tokio::spawn(async move {
            task_started.notify_waiters();
            task_release.notified().await;
        });
        server.adopt_listener(task, None);

        let drain_guard = TaskGuard::new(
            Arc::clone(&server.state.config.control.background_tasks),
            Arc::clone(&server.state.config.control.drained),
        );
        let first_server = Arc::clone(&server);
        let first_close = tokio::spawn(async move { first_server.close().await });
        tokio::time::timeout(Duration::from_secs(1), &mut started)
            .await
            .expect("listener task did not start");
        wait_until_close_is_waiting(&server).await;

        first_close.abort();
        assert!(
            first_close
                .await
                .expect_err("close was not cancelled")
                .is_cancelled()
        );

        let second = tokio::time::timeout(Duration::from_secs(1), server.close())
            .await
            .expect("retry close hung after first close cancellation");
        assert!(matches!(second, Err(HttpServerError::Timeout)));

        listener_release.notify_one();
        let drain_retry = tokio::time::timeout(Duration::from_secs(1), server.close())
            .await
            .expect("drain retry hung after listener completion");
        assert!(matches!(drain_retry, Err(HttpServerError::Timeout)));

        drop(drain_guard);
        server
            .close()
            .await
            .expect("final close after drain release");
    }

    #[tokio::test]
    async fn prior_knowledge_http2_roundtrip_and_drain() {
        let server = HttpServer::new(
            DriveRegistry::new(),
            HttpServerOptions {
                http2_prior_knowledge: true,
                ..HttpServerOptions::default()
            },
        );
        server.listen().await.unwrap();
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .build()
            .unwrap();
        let response = client
            .get(format!("http://127.0.0.1:{}/missing", server.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.version(), hyper::Version::HTTP_2);
        assert!(!response.status().is_success());
        let _ = response.bytes().await.unwrap();
        server.close().await.unwrap();
        assert_eq!(server.connections(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_close_after_listener_join_keeps_finished_state() {
        let server = Arc::new(HttpServer::new(
            DriveRegistry::new(),
            HttpServerOptions {
                drain_timeout: Duration::from_secs(10),
                ..HttpServerOptions::default()
            },
        ));
        let listener_release = Arc::new(Notify::new());
        let task_release = Arc::clone(&listener_release);
        let task = tokio::spawn(async move {
            task_release.notified().await;
        });
        server.adopt_listener(task, None);
        let drain_guard = TaskGuard::new(
            Arc::clone(&server.state.config.control.background_tasks),
            Arc::clone(&server.state.config.control.drained),
        );

        let first_server = Arc::clone(&server);
        let first_close = tokio::spawn(async move { first_server.close().await });
        wait_until_close_is_waiting(&server).await;

        let mut published = server.listener.get().expect("listener").done.clone();
        listener_release.notify_one();
        let outcome =
            tokio::time::timeout(Duration::from_secs(1), published.wait_for(Option::is_some))
                .await
                .expect("completed listener result was not published")
                .expect("listener supervisor went away")
                .clone();
        assert!(matches!(outcome, Some(CloseOutcome::Success)));

        assert!(
            !first_close.is_finished(),
            "close completed before cancellation"
        );
        first_close.abort();
        assert!(
            first_close
                .await
                .expect_err("close was not cancelled after publication")
                .is_cancelled()
        );
        drop(drain_guard);
        server
            .close()
            .await
            .expect("retry after published-listener cancellation");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_close_callers_share_listener_failure() {
        let server = Arc::new(HttpServer::new(
            DriveRegistry::new(),
            HttpServerOptions {
                drain_timeout: Duration::from_secs(1),
                ..HttpServerOptions::default()
            },
        ));
        let task = tokio::spawn(async {
            panic!("injected listener failure");
        });
        server.adopt_listener(task, None);

        let first_server = Arc::clone(&server);
        let second_server = Arc::clone(&server);
        let (first, second) = tokio::join!(
            tokio::spawn(async move { first_server.close().await }),
            tokio::spawn(async move { second_server.close().await }),
        );
        let first = first
            .expect("first close task panicked")
            .expect_err("first close unexpectedly succeeded");
        let second = second
            .expect("second close task panicked")
            .expect_err("second close unexpectedly succeeded");
        let first = match first {
            HttpServerError::Join(error) => error,
            other => panic!("unexpected first close result: {other:?}"),
        };
        let second = match second {
            HttpServerError::Join(error) => error,
            other => panic!("unexpected second close result: {other:?}"),
        };
        assert!(Arc::ptr_eq(&first, &second));

        let repeat = server
            .close()
            .await
            .expect_err("repeat close unexpectedly succeeded");
        let repeat = match repeat {
            HttpServerError::Join(error) => error,
            other => panic!("unexpected repeat close result: {other:?}"),
        };
        assert!(Arc::ptr_eq(&first, &repeat));
    }
}
