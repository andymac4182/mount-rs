//! Portable HTTP/1.1 adapter for [`crate::session::WebdavSession`].
//!
//! This is deliberately a small Tokio/Hyper server rather than a native mount
//! wrapper.  It binds a TCP socket on macOS and Linux, streams `PUT` bodies
//! through an explicit limit, buffers only bounded XML bodies, and hands the
//! normalized request to the same session used by unit tests.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, channel::Channel, combinators::UnsyncBoxBody};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use mount_rs_core::FsDriver;
use tokio::net::TcpListener;
use tokio::sync::Notify;

use crate::constants::{DEFAULT_HOST, DEFAULT_MAX_REQUEST_BYTES};
use crate::protocol::{DavFault, WebdavError, WebdavRequestHead, WebdavResponse};
use crate::session::{WebdavRequestBody, WebdavSession, WebdavSessionHooks, WebdavSessionOptions};

pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type HttpBody = UnsyncBoxBody<Bytes, BoxError>;

/// A bind rejected before a socket is opened.
#[derive(Debug, Clone)]
pub struct WebdavBindError {
    pub host: String,
    pub message: String,
}

impl fmt::Display for WebdavBindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WebdavBindError {}

/// The transport phase that terminated a WebDAV listener or connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebdavTransportErrorKind {
    Accept,
    Connection,
}

/// A transport failure reported to an embedding server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebdavTransportError {
    pub kind: WebdavTransportErrorKind,
    pub peer: Option<String>,
    pub message: String,
}

/// Synchronous callback used by the transport task to report one terminal
/// listener or connection failure.
pub type WebdavTransportErrorHook = Arc<dyn Fn(WebdavTransportError) + Send + Sync + 'static>;

/// Optional hooks for a [`WebdavServer`]. Kept separate from
/// [`WebdavServerOptions`] so existing option literals remain compatible.
#[derive(Clone, Default)]
pub struct WebdavServerHooks {
    pub on_transport_error: Option<WebdavTransportErrorHook>,
}

#[derive(Debug)]
pub enum WebdavServerError {
    Bind(std::io::Error),
    Io(std::io::Error),
    Join(tokio::task::JoinError),
}

impl fmt::Display for WebdavServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(error) | Self::Io(error) => error.fmt(f),
            Self::Join(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for WebdavServerError {}

/// Server configuration.  Session settings are kept together so the request
/// layer remains reusable without a socket.
#[derive(Debug, Clone)]
pub struct WebdavServerOptions {
    pub host: String,
    pub port: u16,
    pub drain_timeout: Duration,
    pub max_request_bytes: usize,
    pub session: WebdavSessionOptions,
}

impl Default for WebdavServerOptions {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: 0,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            session: WebdavSessionOptions::default(),
        }
    }
}

impl WebdavServerOptions {
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.session.credentials = Some(crate::session::WebdavCredentials {
            username: username.into(),
            password: password.into(),
        });
        self
    }

    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_session_options(mut self, options: WebdavSessionOptions) -> Self {
        self.session = options;
        self
    }
}

/// Accept a concrete driver or the requested `Arc<dyn FsDriver>` form without
/// putting an `Arc` wrapper inside another `Arc`.
struct ServerState {
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
struct StreamControl {
    background_tasks: Arc<AtomicUsize>,
    drained: Arc<Notify>,
    shutdown: Arc<Notify>,
    closing: Arc<AtomicUsize>,
}

struct ConnectionRegistry {
    active: AtomicUsize,
    next_id: AtomicUsize,
    tasks: Mutex<HashMap<usize, tokio::task::AbortHandle>>,
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self {
            active: AtomicUsize::new(0),
            next_id: AtomicUsize::new(0),
            tasks: Mutex::new(HashMap::new()),
        }
    }
}

impl ConnectionRegistry {
    fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    fn next_id(&self) -> usize {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Register the task while holding the task map lock. A connection guard
    /// is moved into the future before it is spawned, so aborting a task before
    /// its first poll still decrements the active count on future drop.
    fn spawn<F>(&self, id: usize, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let task = tokio::spawn(future);
        tasks.insert(id, task.abort_handle());
    }

    fn abort_all(&self) {
        let tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let handles = tasks.values().cloned().collect::<Vec<_>>();
        drop(tasks);
        for task in handles {
            task.abort();
        }
    }
}

/// A running or runnable WebDAV HTTP server.
pub struct WebdavServer {
    pub session: Arc<WebdavSession>,
    host: String,
    requested_port: u16,
    actual_port: AtomicU16,
    max_request_bytes: usize,
    drain_timeout: Duration,
    shutdown: Arc<Notify>,
    closing: Arc<AtomicUsize>,
    drained: Arc<Notify>,
    background_tasks: Arc<AtomicUsize>,
    lifecycle: tokio::sync::Mutex<()>,
    state: Mutex<ServerState>,
    connections: Arc<ConnectionRegistry>,
    hooks: WebdavServerHooks,
}

impl fmt::Debug for WebdavServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebdavServer")
            .field("host", &self.host)
            .field("port", &self.port())
            .field("connections", &self.connections())
            .finish()
    }
}

impl WebdavServer {
    fn new(
        driver: Arc<dyn FsDriver>,
        options: WebdavServerOptions,
        hooks: WebdavServerHooks,
        session_hooks: WebdavSessionHooks,
    ) -> Self {
        Self {
            session: Arc::new(WebdavSession::new_with_hooks(
                driver,
                options.session,
                session_hooks,
            )),
            host: options.host,
            requested_port: options.port,
            actual_port: AtomicU16::new(options.port),
            max_request_bytes: options.max_request_bytes,
            drain_timeout: options.drain_timeout,
            shutdown: Arc::new(Notify::new()),
            closing: Arc::new(AtomicUsize::new(0)),
            drained: Arc::new(Notify::new()),
            background_tasks: Arc::new(AtomicUsize::new(0)),
            lifecycle: tokio::sync::Mutex::new(()),
            state: Mutex::new(ServerState { task: None }),
            connections: Arc::new(ConnectionRegistry::default()),
            hooks,
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
        self.connections.active()
    }

    pub async fn listen(&self) -> Result<(), WebdavServerError> {
        let _lifecycle = self.lifecycle.lock().await;
        let running = self
            .state
            .lock()
            .map(|state| task_is_running(state.task.as_ref()))
            .unwrap_or(false);
        let closing = self.closing.load(Ordering::Acquire) != 0;
        if closing {
            return Err(WebdavServerError::Io(std::io::Error::other(if running {
                "WebDAV server is closing"
            } else {
                "WebDAV server is still draining connections"
            })));
        }
        if running {
            return Ok(());
        }
        let address = socket_address(&self.host, self.requested_port)
            .await
            .map_err(WebdavServerError::Bind)?;
        self.closing.store(0, Ordering::Release);
        let listener = TcpListener::bind(address)
            .await
            .map_err(WebdavServerError::Bind)?;
        let port = listener.local_addr().map_err(WebdavServerError::Io)?.port();
        self.actual_port.store(port, Ordering::Release);
        let session = Arc::clone(&self.session);
        let shutdown = Arc::clone(&self.shutdown);
        let max_request_bytes = self.max_request_bytes;
        let counter_for_task = Arc::clone(&self.connections);
        let drained_for_task = Arc::clone(&self.drained);
        let closing_for_task = Arc::clone(&self.closing);
        let stream_control_for_task = StreamControl {
            background_tasks: Arc::clone(&self.background_tasks),
            drained: Arc::clone(&self.drained),
            shutdown: Arc::clone(&self.shutdown),
            closing: Arc::clone(&self.closing),
        };
        let hooks_for_task = self.hooks.clone();
        let task = tokio::spawn(async move {
            loop {
                let shutdown_notified = shutdown.notified();
                tokio::pin!(shutdown_notified);
                shutdown_notified.as_mut().enable();
                if closing_for_task.load(Ordering::Acquire) != 0 {
                    break;
                }
                tokio::select! {
                    _ = shutdown_notified => break,
                    accepted = listener.accept() => {
                        let (stream, peer) = match accepted {
                            Ok(accepted) => accepted,
                            Err(error) => {
                                report(&hooks_for_task, WebdavTransportError {
                                    kind: WebdavTransportErrorKind::Accept,
                                    peer: None,
                                    message: error.to_string(),
                                });
                                break;
                            }
                        };
                        let session = Arc::clone(&session);
                        let connections = Arc::clone(&counter_for_task);
                        let drained = Arc::clone(&drained_for_task);
                        let shutdown = Arc::clone(&shutdown);
                        let closing = Arc::clone(&closing_for_task);
                        let hooks = hooks_for_task.clone();
                        let stream_control = stream_control_for_task.clone();
                        let peer = peer.to_string();
                        let connection_id = connections.next_id();
                        connections.active.fetch_add(1, Ordering::AcqRel);
                        let guard = ConnectionGuard {
                            connections: Arc::clone(&connections),
                            id: connection_id,
                            drained,
                        };
                        connections.spawn(connection_id, async move {
                            let _guard = guard;
                            let service = service_fn(move |request| {
                                handle_request(
                                    request,
                                    Arc::clone(&session),
                                    max_request_bytes,
                                    stream_control.clone(),
                                )
                            });
                            let io = TokioIo::new(stream);
                            let connection = hyper::server::conn::http1::Builder::new()
                                .keep_alive(true)
                                .serve_connection(io, service);
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
                                    if let Err(error) = connection.await {
                                        report(&hooks, WebdavTransportError {
                                            kind: WebdavTransportErrorKind::Connection,
                                            peer: Some(peer),
                                            message: error.to_string(),
                                        });
                                    }
                                }
                                result = &mut connection => {
                                    if let Err(error) = result {
                                        report(&hooks, WebdavTransportError {
                                            kind: WebdavTransportErrorKind::Connection,
                                            peer: Some(peer),
                                            message: error.to_string(),
                                        });
                                    }
                                }
                            }
                        });
                    }
                }
            }
        });
        if let Ok(mut state) = self.state.lock() {
            state.task = Some(task);
        }
        Ok(())
    }

    pub async fn close(&self) -> Result<(), WebdavServerError> {
        let _lifecycle = self.lifecycle.lock().await;
        let task = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.task.take());
        if task.is_none() && self.closing.load(Ordering::Acquire) == 0 {
            return Ok(());
        }
        self.closing.store(1, Ordering::Release);
        self.shutdown.notify_waiters();
        let connections = Arc::clone(&self.connections);
        let drained = Arc::clone(&self.drained);
        let background_tasks = Arc::clone(&self.background_tasks);
        let mut task = task;
        let result = tokio::time::timeout(self.drain_timeout, async {
            if let Some(task_handle) = task.as_mut() {
                let join_result = task_handle.await;
                task = None;
                join_result.map_err(WebdavServerError::Join)?;
            }
            while connections.active() != 0 || background_tasks.load(Ordering::Acquire) != 0 {
                let notified = drained.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if connections.active() == 0 && background_tasks.load(Ordering::Acquire) == 0 {
                    break;
                }
                notified.await;
            }
            Ok(())
        })
        .await;
        match result {
            Ok(result) => {
                if result.is_ok() {
                    self.closing.store(0, Ordering::Release);
                }
                result
            }
            Err(_) => {
                connections.abort_all();
                if let Some(task) = task.take()
                    && !task.is_finished()
                    && let Ok(mut state) = self.state.lock()
                {
                    state.task = Some(task);
                }
                Err(WebdavServerError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "WebDAV server close timed out",
                )))
            }
        }
    }
}

impl Drop for WebdavServer {
    fn drop(&mut self) {
        self.closing.store(1, Ordering::Release);
        self.shutdown.notify_waiters();
        self.connections.abort_all();
    }
}

struct ConnectionGuard {
    connections: Arc<ConnectionRegistry>,
    id: usize,
    drained: Arc<Notify>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.connections.active.fetch_sub(1, Ordering::AcqRel);
        let mut tasks = self
            .connections
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        tasks.remove(&self.id);
        self.drained.notify_waiters();
    }
}

async fn handle_request(
    request: Request<Incoming>,
    session: Arc<WebdavSession>,
    max_request_bytes: usize,
    stream_control: StreamControl,
) -> Result<Response<HttpBody>, std::convert::Infallible> {
    let (parts, body) = request.into_parts();
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in &parts.headers {
        if let Ok(value) = value.to_str() {
            let name = name.as_str().to_owned();
            if let Some(existing) = headers.get_mut(&name) {
                let separator = if name == "if" { " " } else { "," };
                existing.push_str(separator);
                existing.push_str(value);
            } else {
                headers.insert(name, value.to_owned());
            }
        }
    }
    let head = WebdavRequestHead {
        method: parts.method.as_str().to_owned(),
        target: parts.uri.to_string(),
        headers,
    };
    let is_head = head.method.eq_ignore_ascii_case("HEAD");
    if head
        .headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > max_request_bytes)
    {
        let mut response = session.reject_request(
            &head,
            WebdavError::Fault(
                DavFault::new(413).with_message("the request body exceeds the configured limit"),
            ),
        );
        response
            .headers
            .insert("connection".to_owned(), "close".to_owned());
        return Ok(to_http_response(response, is_head, &stream_control));
    }
    let body = LimitedRequestBody {
        inner: IncomingRequestBody { body },
        limit: max_request_bytes,
        total: 0,
        exceeded: false,
    };
    let response = session.handle_request_stream(head, body).await;
    Ok(to_http_response(response, is_head, &stream_control))
}

struct IncomingRequestBody {
    body: Incoming,
}

impl WebdavRequestBody for IncomingRequestBody {
    fn poll_next_chunk(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, WebdavError>>> {
        loop {
            let this = self.as_mut().get_mut();
            let mut frame = std::pin::pin!(this.body.frame());
            match Future::poll(frame.as_mut(), cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Some(Err(WebdavError::Body(error.to_string()))));
                }
                Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                    Ok(data) => return Poll::Ready(Some(Ok(data))),
                    Err(_trailers) => continue,
                },
            }
        }
    }
}

struct LimitedRequestBody<B> {
    inner: B,
    limit: usize,
    total: usize,
    exceeded: bool,
}

impl<B> WebdavRequestBody for LimitedRequestBody<B>
where
    B: WebdavRequestBody + Unpin,
{
    fn poll_next_chunk(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, WebdavError>>> {
        loop {
            let this = self.as_mut().get_mut();
            if this.exceeded {
                match Pin::new(&mut this.inner).poll_next_chunk(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Ready(Some(Ok(_))) | Poll::Ready(Some(Err(_))) => continue,
                }
            }
            match Pin::new(&mut this.inner).poll_next_chunk(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(error))),
                Poll::Ready(Some(Ok(data))) => {
                    if this.total.saturating_add(data.len()) > this.limit {
                        this.exceeded = true;
                        return Poll::Ready(Some(Err(WebdavError::Fault(
                            DavFault::new(413)
                                .with_message("the request body exceeds the configured limit"),
                        ))));
                    }
                    this.total += data.len();
                    return Poll::Ready(Some(Ok(data)));
                }
            }
        }
    }
}

fn to_http_response(
    response: WebdavResponse,
    head: bool,
    stream_control: &StreamControl,
) -> Response<HttpBody> {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = response.headers;
    let body = if head {
        full_body(Bytes::new())
    } else {
        match response.body {
            None => full_body(Bytes::new()),
            Some(crate::protocol::WebdavBody::Bytes(bytes)) => full_body(Bytes::from(bytes)),
            Some(crate::protocol::WebdavBody::File(file)) => {
                stream_file(file, stream_control.clone())
            }
        }
    };
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| Response::new(full_body(Bytes::new())))
}

fn full_body(bytes: Bytes) -> HttpBody {
    Full::new(bytes)
        .map_err(|never| -> BoxError { match never {} })
        .boxed_unsync()
}

fn stream_file(file: crate::protocol::FileBody, control: StreamControl) -> HttpBody {
    let (mut sender, body) = Channel::<Bytes, BoxError>::new(2);
    let task = StreamTaskGuard::new(&control);
    tokio::spawn(async move {
        let _task = task;
        let crate::protocol::FileBody {
            handle,
            start,
            length,
            chunk_size,
        } = file;
        let end = start.saturating_add(length);
        let mut position = start;
        let mut buffer = vec![0_u8; chunk_size.max(1)];
        let mut failure: Option<BoxError> = None;

        let mut close_guard = ResponseFileCloseGuard::new(handle, &control);
        let handle = close_guard.handle();

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
                        mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio)
                            .with_syscall("read")
                            .with_message("short WebDAV response body"),
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
                        mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio)
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

struct StreamTaskGuard {
    counter: Arc<AtomicUsize>,
    drained: Arc<Notify>,
}

impl StreamTaskGuard {
    fn new(control: &StreamControl) -> Self {
        control.background_tasks.fetch_add(1, Ordering::AcqRel);
        Self::from_parts(
            Arc::clone(&control.background_tasks),
            Arc::clone(&control.drained),
        )
    }

    fn from_parts(counter: Arc<AtomicUsize>, drained: Arc<Notify>) -> Self {
        Self { counter, drained }
    }
}

impl Drop for StreamTaskGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
        self.drained.notify_waiters();
    }
}

/// Ensures that a provider file handle is closed when a streamed response is
/// cancelled by a client disconnect or server shutdown.  A fallback close is
/// tracked separately so the server drain cannot race the cleanup task.
struct ResponseFileCloseGuard {
    handle: Option<Arc<dyn mount_rs_core::FileHandle>>,
    background_tasks: Arc<AtomicUsize>,
    drained: Arc<Notify>,
}

impl ResponseFileCloseGuard {
    fn new(handle: Arc<dyn mount_rs_core::FileHandle>, control: &StreamControl) -> Self {
        Self {
            handle: Some(handle),
            background_tasks: Arc::clone(&control.background_tasks),
            drained: Arc::clone(&control.drained),
        }
    }

    fn handle(&self) -> Arc<dyn mount_rs_core::FileHandle> {
        Arc::clone(
            self.handle
                .as_ref()
                .expect("response file close guard handle"),
        )
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

impl Drop for ResponseFileCloseGuard {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let background_tasks = Arc::clone(&self.background_tasks);
        let drained = Arc::clone(&self.drained);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        background_tasks.fetch_add(1, Ordering::AcqRel);
        let task = StreamTaskGuard::from_parts(background_tasks, drained);
        runtime.spawn(async move {
            let _task = task;
            let _ = handle.close().await;
        });
    }
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

pub fn is_loopback_host(host: &str) -> bool {
    let Some(host) = unbracket_host(host) else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host == "::1" {
        return true;
    }
    if let Some(mapped) = host.strip_prefix("::ffff:") {
        return mapped
            .parse::<Ipv4Addr>()
            .is_ok_and(|address| address.is_loopback());
    }
    host.parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn unbracket_host(host: &str) -> Option<&str> {
    match (host.starts_with('['), host.ends_with(']')) {
        (true, true) if host.len() > 2 => Some(&host[1..host.len() - 1]),
        (false, false) if !host.contains(['[', ']']) => Some(host),
        _ => None,
    }
}

fn task_is_running(task: Option<&tokio::task::JoinHandle<()>>) -> bool {
    task.is_some_and(|task| !task.is_finished())
}

pub fn bind_refusal(host: &str, credentials: bool) -> Option<WebdavBindError> {
    if credentials || is_loopback_host(host) {
        return None;
    }
    let message = if host.is_empty() || host == "0.0.0.0" || host == "::" || host == "[::]" {
        format!(
            "refusing unauthenticated WebDAV bind to {host}; that address binds every interface"
        )
    } else {
        format!(
            "refusing unauthenticated WebDAV bind to {host}; the address is reachable from outside the machine"
        )
    };
    Some(WebdavBindError {
        host: host.to_owned(),
        message,
    })
}

pub fn create_webdav_server(
    driver: Arc<dyn FsDriver>,
    options: WebdavServerOptions,
) -> Result<WebdavServer, WebdavBindError> {
    create_webdav_server_with_hooks(driver, options, WebdavServerHooks::default())
}

pub fn create_webdav_server_with_hooks(
    driver: Arc<dyn FsDriver>,
    options: WebdavServerOptions,
    hooks: WebdavServerHooks,
) -> Result<WebdavServer, WebdavBindError> {
    create_webdav_server_with_session_hooks(driver, options, hooks, WebdavSessionHooks::default())
}

/// Create a WebDAV server with both transport- and request-level hooks.
pub fn create_webdav_server_with_session_hooks(
    driver: Arc<dyn FsDriver>,
    options: WebdavServerOptions,
    hooks: WebdavServerHooks,
    session_hooks: WebdavSessionHooks,
) -> Result<WebdavServer, WebdavBindError> {
    if let Some(error) = bind_refusal(&options.host, options.session.credentials.is_some()) {
        return Err(error);
    }
    Ok(WebdavServer::new(driver, options, hooks, session_hooks))
}

fn report(hooks: &WebdavServerHooks, error: WebdavTransportError) {
    if let Some(hook) = &hooks.on_transport_error {
        hook(error);
    }
}

#[cfg(test)]
mod tests {
    use super::task_is_running;

    #[tokio::test]
    async fn a_finished_listener_task_is_not_considered_running() {
        let task = tokio::spawn(async {});
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }

        assert!(!task_is_running(Some(&task)));
        assert!(!task_is_running(None));
    }

    #[tokio::test]
    async fn a_pending_listener_task_is_considered_running() {
        let task = tokio::spawn(std::future::pending::<()>());
        tokio::task::yield_now().await;

        assert!(task_is_running(Some(&task)));

        task.abort();
    }
}
