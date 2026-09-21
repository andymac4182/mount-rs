use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, channel::Channel, combinators::UnsyncBoxBody};
use hyper::body::{Body as HttpBodyTrait, Frame, Incoming, SizeHint};
use hyper::header;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use mount_rs_core::{ErrorCode, FileHandle, FileType, FsDriver, FsError, MkdirOptions, Stats};
#[cfg(feature = "observability")]
use mount_rs_observability::Telemetry;
#[cfg(feature = "observability-otlp")]
use mount_rs_observability::extract_headers;
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify, Semaphore};

pub const DEFAULT_MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_READ_CHUNK_BYTES: usize = 64 * 1024;
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;
pub const DEFAULT_MAX_DIRECTORY_ENTRIES: usize = 4096;
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type HttpBody = UnsyncBoxBody<Bytes, BoxError>;

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
                f.write_str("drive id must be 1-64 ASCII letters, digits, '-', '_' or '.'")
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
        validate_drive_id(&id)?;
        if bearer_token.is_empty() || bearer_token.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(RegistryError::EmptyBearerToken);
        }
        Ok(Self {
            id,
            driver,
            bearer_token,
        })
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
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_directory_entries: usize,
    pub read_chunk_bytes: usize,
    pub drain_timeout: Duration,
    pub max_connections: usize,
    pub request_timeout: Duration,
    #[cfg(feature = "observability")]
    pub telemetry: Telemetry,
}

impl Default for HttpServerOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 0,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_directory_entries: DEFAULT_MAX_DIRECTORY_ENTRIES,
            read_chunk_bytes: DEFAULT_READ_CHUNK_BYTES,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
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
                "HTTP listener host '{host}' is not allowed; bind to loopback and use a TLS reverse proxy for remote clients"
            ),
            Self::Join(error) => error.fmt(f),
            Self::Timeout => f.write_str("HTTP server close timed out"),
        }
    }
}

impl std::error::Error for HttpServerError {}

struct AppState {
    registry: Arc<DriveRegistry>,
    config: RequestConfig,
}

#[derive(Clone)]
struct RequestConfig {
    max_request_bytes: usize,
    max_response_bytes: usize,
    max_directory_entries: usize,
    read_chunk_bytes: usize,
    request_timeout: Duration,
    control: RuntimeControl,
    #[cfg(feature = "observability")]
    telemetry: Telemetry,
}

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
}

impl CloseOutcome {
    fn result(&self) -> Result<(), HttpServerError> {
        match self {
            Self::Success => Ok(()),
            Self::Join(error) => Err(HttpServerError::Join(Arc::clone(error))),
        }
    }
}

enum ListenerState {
    Running(tokio::task::JoinHandle<()>),
    Finished(CloseOutcome),
}

/// A running multi-drive HTTP/1.1 server.
pub struct HttpServer {
    state: Arc<AppState>,
    host: String,
    requested_port: u16,
    actual_port: AtomicU16,
    drain_timeout: Duration,
    shutdown: Arc<Notify>,
    closing: Arc<AtomicUsize>,
    drained: Arc<Notify>,
    state_changed: Notify,
    close_outcome: Mutex<Option<CloseOutcome>>,
    task: Mutex<Option<ListenerState>>,
    connections: Arc<AtomicUsize>,
    connection_permits: Arc<Semaphore>,
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
        Self {
            state: Arc::new(AppState {
                registry: Arc::new(registry),
                config: RequestConfig {
                    max_request_bytes: options.max_request_bytes.max(1),
                    max_response_bytes: options.max_response_bytes.max(1),
                    max_directory_entries: options.max_directory_entries.max(1),
                    read_chunk_bytes: options.read_chunk_bytes.max(1),
                    request_timeout: positive_duration(options.request_timeout),
                    control,
                    #[cfg(feature = "observability")]
                    telemetry: options.telemetry,
                },
            }),
            host: options.host,
            requested_port: options.port,
            actual_port: AtomicU16::new(options.port),
            drain_timeout: options.drain_timeout,
            shutdown,
            closing,
            drained,
            state_changed: Notify::new(),
            close_outcome: Mutex::new(None),
            task: Mutex::new(None),
            connections: Arc::new(AtomicUsize::new(0)),
            connection_permits: Arc::new(Semaphore::new(options.max_connections.max(1))),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.actual_port.load(Ordering::Acquire)
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

    pub async fn listen(&self) -> Result<(), HttpServerError> {
        if self.task.lock().await.is_some() {
            return Ok(());
        }

        validate_bind_host(&self.host)?;
        let address = socket_address(&self.host, self.requested_port)
            .await
            .map_err(HttpServerError::Bind)?;
        let listener = TcpListener::bind(address)
            .await
            .map_err(HttpServerError::Bind)?;
        let port = listener.local_addr().map_err(HttpServerError::Io)?.port();
        self.actual_port.store(port, Ordering::Release);

        let state = Arc::clone(&self.state);
        let shutdown = Arc::clone(&self.shutdown);
        let counter = Arc::clone(&self.connections);
        let drained = Arc::clone(&self.drained);
        let closing = Arc::clone(&self.closing);
        let connection_permits = Arc::clone(&self.connection_permits);
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
                tokio::select! {
                    _ = shutdown_notified => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _peer)) = accepted else { break };
                        if closing.load(Ordering::Acquire) != 0 {
                            break;
                        }
                        let Ok(permit) = Arc::clone(&connection_permits).try_acquire_owned()
                        else {
                            continue;
                        };
                        let state = Arc::clone(&state);
                        let shutdown = Arc::clone(&shutdown);
                        let counter = Arc::clone(&counter);
                        let drained = Arc::clone(&drained);
                        let closing = Arc::clone(&closing);
                        counter.fetch_add(1, Ordering::AcqRel);
                        tokio::spawn(async move {
                            let _permit = permit;
                            let _guard = ConnectionGuard { counter, drained };
                            let request_timeout = state.config.request_timeout;
                            let service = service_fn(move |request| {
                                handle_request(request, Arc::clone(&state))
                            });
                            let io = TokioIo::new(stream);
                            let mut builder = hyper::server::conn::http1::Builder::new();
                            builder
                                .keep_alive(true)
                                .header_read_timeout(Some(request_timeout))
                                .timer(TokioTimer::new());
                            let connection = builder.serve_connection(io, service);
                            tokio::pin!(connection);
                            let shutdown_notified = shutdown.notified();
                            tokio::pin!(shutdown_notified);
                            shutdown_notified.as_mut().enable();
                            if closing.load(Ordering::Acquire) != 0 {
                                return;
                            }
                            tokio::select! {
                                _ = shutdown_notified => {
                                    connection.as_mut().graceful_shutdown();
                                    let _ = connection.await;
                                }
                                _ = &mut connection => {}
                            }
                        });
                    }
                }
            }
        });
        let mut task_slot = self.task.lock().await;
        if task_slot.is_none() {
            *task_slot = Some(ListenerState::Running(task));
            self.state_changed.notify_waiters();
        } else {
            task.abort();
        }
        Ok(())
    }

    pub async fn close(&self) -> Result<(), HttpServerError> {
        if let Some(outcome) = self.close_outcome.lock().await.clone() {
            return outcome.result();
        }
        self.closing
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .ok();
        self.shutdown.notify_waiters();
        if let Some(outcome) = self.close_outcome.lock().await.clone() {
            return outcome.result();
        }
        match tokio::time::timeout(self.drain_timeout, self.complete_close()).await {
            Ok(result) => result,
            Err(_) => Err(HttpServerError::Timeout),
        }
    }

    async fn complete_close(&self) -> Result<(), HttpServerError> {
        let listener_outcome = self.await_listener().await;
        let connections = Arc::clone(&self.connections);
        let background_tasks = Arc::clone(&self.state.config.control.background_tasks);
        let drained = Arc::clone(&self.drained);
        wait_for_drain(connections, background_tasks, drained).await;

        let mut outcome_slot = self.close_outcome.lock().await;
        if let Some(outcome) = outcome_slot.as_ref() {
            return outcome.result();
        }
        *outcome_slot = Some(listener_outcome.clone());
        self.closing.store(2, Ordering::Release);
        self.state_changed.notify_waiters();
        listener_outcome.result()
    }

    async fn await_listener(&self) -> CloseOutcome {
        loop {
            let mut task_slot = self.task.lock().await;
            let Some(state) = task_slot.as_mut() else {
                drop(task_slot);
                let notified = self.state_changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.task.lock().await.is_some() {
                    continue;
                }
                notified.await;
                continue;
            };
            let outcome = match state {
                ListenerState::Finished(outcome) => return outcome.clone(),
                ListenerState::Running(task) => match task.await {
                    Ok(()) => CloseOutcome::Success,
                    Err(error) => CloseOutcome::Join(Arc::new(error)),
                },
            };
            // This assignment is deliberately adjacent to the await while
            // the same mutex guard is held.  A cancelled close can therefore
            // leave only Running or Finished, never a consumed JoinHandle
            // without its cached outcome.
            *task_slot = Some(ListenerState::Finished(outcome.clone()));
            self.state_changed.notify_waiters();
            return outcome;
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.closing.store(1, Ordering::Release);
        self.shutdown.notify_waiters();
        if let Ok(mut task) = self.task.try_lock()
            && let Some(ListenerState::Running(task)) = task.as_mut()
        {
            task.abort();
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
        code: ErrorCode,
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
            code: ErrorCode::Einval,
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
            code: ErrorCode::Eacces,
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
            code: ErrorCode::Enoent,
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
            code: ErrorCode::Enotsup,
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
            code: ErrorCode::Efbig,
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
            code: ErrorCode::Eoverflow,
            message: "directory response exceeds the configured limit".to_owned(),
            close: true,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }

    fn request_timeout() -> Self {
        Self::Client {
            status: StatusCode::REQUEST_TIMEOUT,
            code: ErrorCode::Eio,
            message: "request exceeded the configured timeout".to_owned(),
            close: true,
            content_range: None,
            allow: None,
            authenticate: false,
        }
    }

    fn server_closing() -> Self {
        Self::Client {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: ErrorCode::Eio,
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
            code: ErrorCode::Einval,
            message: "range is not satisfiable".to_owned(),
            close: false,
            content_range: Some(format!("bytes */{size}")),
            allow: None,
            authenticate: false,
        }
    }
}

#[derive(Debug)]
enum Route {
    Health { readiness: bool },
    Discovery,
    File { drive: String, path: String },
    Entries { drive: String, path: String },
    Operation { drive: String, operation: String },
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

async fn handle_request(
    request: Request<Incoming>,
    state: Arc<AppState>,
) -> Result<Response<HttpBody>, std::convert::Infallible> {
    handle_request_instrumented(request, state).await
}

async fn handle_request_instrumented(
    request: Request<Incoming>,
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
    request: Request<Incoming>,
    state: Arc<AppState>,
) -> Response<HttpBody> {
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
    request: Request<Incoming>,
    state: Arc<AppState>,
) -> Response<HttpBody> {
    let (parts, body) = request.into_parts();
    let route = match parse_route(parts.uri.path()) {
        Ok(route) => route,
        Err(error) => return error_response(error),
    };

    match &route {
        Route::Health { readiness } => {
            return handle_health(&parts.method, &state.registry, *readiness);
        }
        Route::Discovery => {
            return handle_discovery(&parts.method, &parts.headers, &state.registry);
        }
        Route::File { .. } | Route::Entries { .. } | Route::Operation { .. } => {}
    }

    let drive_id = match &route {
        Route::File { drive, .. }
        | Route::Entries { drive, .. }
        | Route::Operation { drive, .. } => drive,
        Route::Discovery | Route::Health { .. } => unreachable!(),
    };
    let Some(drive) = state.registry.get(drive_id).cloned() else {
        return error_response(RequestError::not_found());
    };
    if !authorized(&parts.headers, &drive) {
        return error_response(RequestError::unauthorized());
    }
    if content_length_exceeds(&parts.headers, state.config.max_request_bytes) {
        return error_response(RequestError::body_too_large());
    }

    let result = match route {
        Route::File { path, .. } => {
            handle_file(
                parts.method,
                parts.headers,
                body,
                drive,
                path,
                state.config.clone(),
            )
            .await
        }
        Route::Entries { path, .. } => {
            handle_entries(parts.method, drive, path, state.config.clone()).await
        }
        Route::Operation { operation, .. } => {
            handle_operation(parts.method, body, drive, operation, state.config.clone()).await
        }
        Route::Discovery | Route::Health { .. } => unreachable!(),
    };
    result.unwrap_or_else(error_response)
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

    let is_ready = !readiness || !registry.is_empty();
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
    };
    let bytes = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{\"status\":\"error\"}".to_vec());
    health_json_response(status, Bytes::from(bytes), method == Method::HEAD)
}

async fn handle_file(
    method: Method,
    headers: hyper::HeaderMap,
    body: Incoming,
    drive: DriveConfig,
    path: String,
    config: RequestConfig,
) -> Result<Response<HttpBody>, RequestError> {
    match method {
        Method::GET | Method::HEAD => {
            handle_file_read(
                method == Method::HEAD,
                &headers,
                drive,
                path,
                config.clone(),
            )
            .await
        }
        Method::PUT => handle_file_write(body, drive, path, config).await,
        Method::DELETE => handle_file_delete(drive, path).await,
        _ => Err(RequestError::method_not_allowed("GET, HEAD, PUT, DELETE")),
    }
}

async fn handle_file_read(
    head: bool,
    headers: &hyper::HeaderMap,
    drive: DriveConfig,
    path: String,
    config: RequestConfig,
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
        let entries = drive
            .driver()
            .readdir_bounded(&path, config.max_directory_entries)
            .await
            .map_err(directory_listing_error)?;
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

    let range = match headers.get(header::RANGE) {
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
        .header(header::ACCEPT_RANGES, "bytes");
    if let Some(content_range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, content_range);
    }
    if head {
        return builder
            .body(head_body(length))
            .map_err(|_| RequestError::Fs(FsError::backend("failed to build HTTP response")));
    }

    let handle = drive
        .driver()
        .open(&path, "r", 0)
        .await
        .map_err(RequestError::Fs)?;
    builder
        .body(stream_file(
            handle,
            start,
            length,
            config.read_chunk_bytes,
            config.control,
        ))
        .map_err(|_| RequestError::Fs(FsError::backend("failed to build HTTP response")))
}

async fn handle_file_write(
    mut body: Incoming,
    drive: DriveConfig,
    path: String,
    config: RequestConfig,
) -> Result<Response<HttpBody>, RequestError> {
    let handle = drive
        .driver()
        .open(&path, "w", 0o666)
        .await
        .map_err(RequestError::Fs)?;
    let mut close_guard = FileCloseGuard::new(handle, &config.control);
    let handle = close_guard.handle();
    let mut written = 0usize;
    let result = async {
        while let Some(frame) =
            next_request_frame(&mut body, &config.control.shutdown, &config.control.closing).await?
        {
            let Ok(data) = frame.into_data() else {
                continue;
            };
            if written.saturating_add(data.len()) > config.max_request_bytes {
                return Err(RequestError::body_too_large());
            }
            let mut offset = 0;
            while offset < data.len() {
                let count = write_with_shutdown(
                    &handle,
                    &data[offset..],
                    &config.control.shutdown,
                    &config.control.closing,
                )
                .await?;
                if count == 0 || count > data.len() - offset {
                    return Err(RequestError::Fs(
                        FsError::new(ErrorCode::Eio)
                            .with_syscall("write")
                            .with_path(path.clone())
                            .with_message("driver returned an invalid write length"),
                    ));
                }
                offset += count;
                written += count;
            }
        }
        close_guard.close().await.map_err(RequestError::Fs)?;
        Ok::<_, RequestError>(())
    }
    .await;
    if let Err(error) = result {
        let _ = close_guard.close().await;
        return Err(error);
    }
    let payload = serde_json::to_vec(&WritePayload {
        path: &path,
        bytes: written,
    })
    .map_err(|_| RequestError::Fs(FsError::backend("failed to encode write result")))?;
    Ok(json_response(StatusCode::OK, Bytes::from(payload), false))
}

async fn handle_file_delete(
    drive: DriveConfig,
    path: String,
) -> Result<Response<HttpBody>, RequestError> {
    let stats = drive
        .driver()
        .lstat(&path)
        .await
        .map_err(RequestError::Fs)?;
    if stats.is_directory() {
        drive
            .driver()
            .rmdir(&path)
            .await
            .map_err(RequestError::Fs)?;
    } else {
        drive
            .driver()
            .unlink(&path)
            .await
            .map_err(RequestError::Fs)?;
    }
    Ok(empty_response(StatusCode::NO_CONTENT))
}

async fn handle_entries(
    method: Method,
    drive: DriveConfig,
    path: String,
    config: RequestConfig,
) -> Result<Response<HttpBody>, RequestError> {
    if method != Method::GET && method != Method::HEAD {
        return Err(RequestError::method_not_allowed("GET, HEAD"));
    }
    let entries = drive
        .driver()
        .readdir_bounded(&path, config.max_directory_entries)
        .await
        .map_err(directory_listing_error)?;
    let payload = serde_json::to_vec(&entries)
        .map_err(|_| RequestError::Fs(FsError::backend("failed to encode directory entries")))?;
    if payload.len() > config.max_response_bytes {
        return Err(RequestError::response_too_large());
    }
    Ok(json_response(
        StatusCode::OK,
        Bytes::from(payload),
        method == Method::HEAD,
    ))
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
    body: Incoming,
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
                        mode: request.mode,
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
            let bytes = read_limited_body(body, config.max_request_bytes, &config.control).await?;
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
    body: &mut Incoming,
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
                code: ErrorCode::Eio,
                message: "request body read failed".to_owned(),
                close: true,
                content_range: None,
                allow: None,
                authenticate: false,
            }),
        },
    }
}

async fn write_with_shutdown(
    handle: &Arc<dyn FileHandle>,
    data: &[u8],
    shutdown: &Notify,
    closing: &AtomicUsize,
) -> Result<usize, RequestError> {
    if closing.load(Ordering::Acquire) != 0 {
        return Err(RequestError::server_closing());
    }
    let notified = shutdown.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if closing.load(Ordering::Acquire) != 0 {
        return Err(RequestError::server_closing());
    }
    let write = handle.write(data, None);
    tokio::pin!(write);
    tokio::select! {
        _ = notified => Err(RequestError::server_closing()),
        result = write => result.map_err(RequestError::Fs),
    }
}

async fn parse_json_body<T: for<'de> Deserialize<'de>>(
    body: Incoming,
    max_request_bytes: usize,
    control: &RuntimeControl,
) -> Result<T, RequestError> {
    let bytes = read_limited_body(body, max_request_bytes, control).await?;
    serde_json::from_slice(&bytes).map_err(|_| RequestError::bad_request("invalid JSON request"))
}

async fn read_limited_body(
    mut body: Incoming,
    max_request_bytes: usize,
    control: &RuntimeControl,
) -> Result<Vec<u8>, RequestError> {
    let mut bytes = Vec::new();
    while let Some(frame) =
        next_request_frame(&mut body, &control.shutdown, &control.closing).await?
    {
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if bytes.len().saturating_add(data.len()) > max_request_bytes {
            return Err(RequestError::body_too_large());
        }
        bytes.extend_from_slice(&data);
    }
    Ok(bytes)
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

fn authorized(headers: &hyper::HeaderMap, drive: &DriveConfig) -> bool {
    bearer_token(headers).is_some_and(|token| drive.token_matches(token))
}

fn bearer_token(headers: &hyper::HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
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

fn content_length_exceeds(headers: &hyper::HeaderMap, limit: usize) -> bool {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > limit as u64)
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
    let (status, code, message, close, content_range, allow, authenticate) = match error {
        RequestError::Fs(error) => (
            status_for_error(error.code),
            error.code,
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
            code: code.as_str().to_owned(),
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
    builder
        .body(full_body(Bytes::from(body)))
        .unwrap_or_else(|_| Response::new(full_body(Bytes::new())))
}

fn status_for_error(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::Eacces | ErrorCode::Eperm | ErrorCode::Erofs => StatusCode::FORBIDDEN,
        ErrorCode::Enoent | ErrorCode::Estale => StatusCode::NOT_FOUND,
        ErrorCode::Eexist | ErrorCode::Enotempty | ErrorCode::Ebusy => StatusCode::CONFLICT,
        ErrorCode::Enotsup | ErrorCode::Enosys => StatusCode::NOT_IMPLEMENTED,
        ErrorCode::Enotdir | ErrorCode::Eisdir | ErrorCode::Einval | ErrorCode::Eloop => {
            StatusCode::BAD_REQUEST
        }
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

fn stream_file(
    handle: Arc<dyn FileHandle>,
    start: u64,
    length: u64,
    chunk_size: usize,
    control: RuntimeControl,
) -> HttpBody {
    let (mut sender, body) = Channel::<Bytes, BoxError>::new(2);
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
        let mut buffer = vec![0_u8; chunk_size.max(1)];
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
                    let send_result = tokio::select! {
                        _ = notified => break,
                        result = sender.send_data(Bytes::copy_from_slice(&buffer[..count])) => result,
                    };
                    if send_result.is_err() {
                        break;
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
            sender.abort(error);
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

fn validate_drive_id(id: &str) -> Result<(), RegistryError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::MemoryFs;

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
        *server.task.lock().await = Some(ListenerState::Running(task));

        let drain_guard = TaskGuard::new(
            Arc::clone(&server.state.config.control.background_tasks),
            Arc::clone(&server.state.config.control.drained),
        );
        let first_server = Arc::clone(&server);
        let first_close = tokio::spawn(async move { first_server.close().await });
        tokio::time::timeout(Duration::from_secs(1), &mut started)
            .await
            .expect("listener task did not start");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if server.task.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first close did not own the shared listener wait");

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
        *server.task.lock().await = Some(ListenerState::Running(task));
        let drain_guard = TaskGuard::new(
            Arc::clone(&server.state.config.control.background_tasks),
            Arc::clone(&server.state.config.control.drained),
        );

        let first_server = Arc::clone(&server);
        let first_close = tokio::spawn(async move { first_server.close().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if server.task.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("close did not begin waiting for the listener");

        let published = server.state_changed.notified();
        tokio::pin!(published);
        published.as_mut().enable();
        listener_release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), &mut published)
            .await
            .expect("completed listener result was not published");
        let task_slot = tokio::time::timeout(Duration::from_secs(1), server.task.lock())
            .await
            .expect("published listener state remained locked");
        assert!(matches!(
            task_slot.as_ref(),
            Some(ListenerState::Finished(CloseOutcome::Success))
        ));
        drop(task_slot);

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
        *server.task.lock().await = Some(ListenerState::Running(task));

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
