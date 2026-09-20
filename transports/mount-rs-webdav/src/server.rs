//! Portable HTTP/1.1 adapter for [`crate::session::WebdavSession`].
//!
//! This is deliberately a small Tokio/Hyper server rather than a native mount
//! wrapper.  It binds a TCP socket on macOS and Linux, buffers each request
//! body under an explicit limit, and hands the normalized request to the same
//! session used by unit tests.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
use crate::session::{WebdavSession, WebdavSessionOptions};

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
    state: Mutex<ServerState>,
    connections: Arc<AtomicUsize>,
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
    fn new(driver: Arc<dyn FsDriver>, options: WebdavServerOptions) -> Self {
        Self {
            session: Arc::new(WebdavSession::new(driver, options.session)),
            host: options.host,
            requested_port: options.port,
            actual_port: AtomicU16::new(options.port),
            max_request_bytes: options.max_request_bytes,
            drain_timeout: options.drain_timeout,
            shutdown: Arc::new(Notify::new()),
            closing: Arc::new(AtomicUsize::new(0)),
            drained: Arc::new(Notify::new()),
            state: Mutex::new(ServerState { task: None }),
            connections: Arc::new(AtomicUsize::new(0)),
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

    pub async fn listen(&self) -> Result<(), WebdavServerError> {
        if self
            .state
            .lock()
            .map(|state| state.task.is_some())
            .unwrap_or(false)
        {
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
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.notified() => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _peer)) = accepted else { break };
                        let session = Arc::clone(&session);
                        let counter = Arc::clone(&counter_for_task);
                        let drained = Arc::clone(&drained_for_task);
                        let shutdown = Arc::clone(&shutdown);
                        let closing = Arc::clone(&closing_for_task);
                        counter.fetch_add(1, Ordering::AcqRel);
                        tokio::spawn(async move {
                            let _guard = ConnectionGuard { counter, drained };
                            let service = service_fn(move |request| {
                                handle_request(request, Arc::clone(&session), max_request_bytes)
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
                                    let _ = connection.await;
                                }
                                result = &mut connection => {
                                    if let Err(error) = result {
                                        let _ = error;
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
        let task = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.task.take());
        let Some(task) = task else { return Ok(()) };
        self.closing.store(1, Ordering::Release);
        self.shutdown.notify_waiters();
        let connections = Arc::clone(&self.connections);
        let drained = Arc::clone(&self.drained);
        tokio::time::timeout(self.drain_timeout, async move {
            task.await.map_err(WebdavServerError::Join)?;
            while connections.load(Ordering::Acquire) != 0 {
                let notified = drained.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if connections.load(Ordering::Acquire) == 0 {
                    break;
                }
                notified.await;
            }
            Ok(())
        })
        .await
        .map_err(|_| {
            WebdavServerError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "WebDAV server close timed out",
            ))
        })?
    }
}

impl Drop for WebdavServer {
    fn drop(&mut self) {
        self.closing.store(1, Ordering::Release);
        self.shutdown.notify_waiters();
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

async fn handle_request(
    request: Request<Incoming>,
    session: Arc<WebdavSession>,
    max_request_bytes: usize,
) -> Result<Response<HttpBody>, std::convert::Infallible> {
    let (parts, body) = request.into_parts();
    let mut headers = BTreeMap::new();
    for (name, value) in &parts.headers {
        if let Ok(value) = value.to_str() {
            headers.insert(name.as_str().to_owned(), value.to_owned());
        }
    }
    if headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > max_request_bytes)
    {
        let response = crate::protocol::fault_response(&WebdavError::Fault(
            DavFault::new(413).with_message("the request body exceeds the configured limit"),
        ));
        let mut response = response;
        response
            .headers
            .insert("connection".to_owned(), "close".to_owned());
        return Ok(to_http_response(response, false));
    }
    let body = match collect_request_body(body, max_request_bytes).await {
        Ok(body) => body,
        Err(error) => {
            let mut response = crate::protocol::fault_response(&error);
            response
                .headers
                .insert("connection".to_owned(), "close".to_owned());
            return Ok(to_http_response(response, true));
        }
    };
    let head = WebdavRequestHead {
        method: parts.method.as_str().to_owned(),
        target: parts.uri.to_string(),
        headers,
    };
    let is_head = head.method.eq_ignore_ascii_case("HEAD");
    let response = session.handle_request(head, body).await;
    Ok(to_http_response(response, is_head))
}

async fn collect_request_body(mut body: Incoming, limit: usize) -> Result<Vec<u8>, WebdavError> {
    let mut result = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| WebdavError::Body(error.to_string()))?;
        if let Ok(data) = frame.into_data() {
            if result.len().saturating_add(data.len()) > limit {
                return Err(WebdavError::Fault(
                    DavFault::new(413)
                        .with_message("the request body exceeds the configured limit"),
                ));
            }
            result.extend_from_slice(&data);
        }
    }
    Ok(result)
}

fn to_http_response(response: WebdavResponse, head: bool) -> Response<HttpBody> {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = response.headers;
    let body = if head {
        full_body(Bytes::new())
    } else {
        match response.body {
            None => full_body(Bytes::new()),
            Some(crate::protocol::WebdavBody::Bytes(bytes)) => full_body(Bytes::from(bytes)),
            Some(crate::protocol::WebdavBody::File(file)) => stream_file(file),
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

fn stream_file(file: crate::protocol::FileBody) -> HttpBody {
    let (mut sender, body) = Channel::<Bytes, BoxError>::new(2);
    tokio::spawn(async move {
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

        while position < end {
            let wanted = (end - position).min(buffer.len() as u64) as usize;
            match handle.read(&mut buffer[..wanted], Some(position)).await {
                Ok(0) => {
                    failure = Some(Box::new(
                        mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio)
                            .with_syscall("read")
                            .with_message("short WebDAV response body"),
                    ));
                    break;
                }
                Ok(count) if count <= wanted => {
                    if sender
                        .send_data(Bytes::copy_from_slice(&buffer[..count]))
                        .await
                        .is_err()
                    {
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

        if failure.is_none() {
            if let Err(error) = handle.close().await {
                failure = Some(Box::new(error));
            }
        } else {
            let _ = handle.close().await;
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
    if let Some(error) = bind_refusal(&options.host, options.session.credentials.is_some()) {
        return Err(error);
    }
    Ok(WebdavServer::new(driver, options))
}
