//! The HTTP boundary for the S3 session.
//!
//! The listener is deliberately loopback-only because this crate does not
//! provide TLS or replay protection. A configured credential pair enables
//! SigV4 verification before dispatch, but it never authorizes a remote bind;
//! put a reviewed TLS/mTLS proxy in front of the loopback listener when remote
//! access is required.

use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::{
    Router,
    body::{Body, BodyDataStream},
    extract::State,
    http::{HeaderName, HeaderValue, Request, Response, StatusCode},
    response::IntoResponse,
    routing::any,
    serve::Listener,
};
use futures_core::Stream;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpListener,
    sync::{Mutex, oneshot},
    task::JoinHandle,
};

use crate::session::{S3RequestBody, S3RequestHead, S3Session, S3StreamBody, S3StreamResponse};
use crate::sigv4::{Credentials, HeaderEntry};

pub const DEFAULT_HOST: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct S3ServerOptions {
    pub host: IpAddr,
    pub port: u16,
    pub drain_timeout: Duration,
}

impl Default for S3ServerOptions {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST,
            port: 0,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
        }
    }
}

/// The transport phase that terminated an S3 listener task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum S3TransportErrorKind {
    Server,
    Connection,
}

/// A transport failure reported to an embedding server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct S3TransportError {
    pub kind: S3TransportErrorKind,
    pub peer: Option<String>,
    pub message: String,
}

/// Synchronous callback used by the transport task to report a terminal
/// listener or HTTP-service failure.
pub type S3TransportErrorHook = Arc<dyn Fn(S3TransportError) + Send + Sync + 'static>;

/// Optional hooks for an [`S3Server`]. Kept separate from
/// [`S3ServerOptions`] so callback ownership remains distinct from binding
/// and lifecycle settings.
#[derive(Clone, Default)]
pub struct S3ServerHooks {
    pub on_transport_error: Option<S3TransportErrorHook>,
}

#[derive(Debug, Error)]
pub enum S3BindError {
    #[error("unauthenticated S3 gateway must bind a loopback address; refused {host}")]
    UnauthenticatedNonLoopback { host: IpAddr },
    #[error("S3 gateway has no TLS boundary and must bind a loopback address; refused {host}")]
    NonLoopbackUnsupported { host: IpAddr },
    #[error("failed to bind S3 gateway: {0}")]
    Bind(#[source] std::io::Error),
    #[error("S3 server task failed: {0}")]
    Task(String),
}

pub struct S3Server {
    session: Arc<S3Session>,
    address: SocketAddr,
    drain_timeout: Duration,
    active_connections: Arc<AtomicUsize>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<JoinHandle<Result<(), std::io::Error>>>>,
}

impl S3Server {
    pub async fn start(
        session: Arc<S3Session>,
        options: S3ServerOptions,
    ) -> Result<Self, S3BindError> {
        Self::start_with_hooks(session, options, S3ServerHooks::default()).await
    }

    pub async fn start_with_hooks(
        session: Arc<S3Session>,
        options: S3ServerOptions,
        hooks: S3ServerHooks,
    ) -> Result<Self, S3BindError> {
        if !options.host.is_loopback() {
            return Err(if session.options.credentials.is_none() {
                S3BindError::UnauthenticatedNonLoopback { host: options.host }
            } else {
                S3BindError::NonLoopbackUnsupported { host: options.host }
            });
        }
        let listener = TcpListener::bind(SocketAddr::new(options.host, options.port))
            .await
            .map_err(S3BindError::Bind)?;
        let address = listener.local_addr().map_err(S3BindError::Bind)?;
        let active_connections = Arc::new(AtomicUsize::new(0));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let app = Router::new()
            .fallback(any(handle_http))
            .with_state(session.clone());
        let hooks_for_task = hooks.clone();
        let tracked_listener = TrackedListener {
            listener,
            active_connections: Arc::clone(&active_connections),
            hooks: hooks.clone(),
        };
        let task = tokio::spawn(async move {
            let result = axum::serve(tracked_listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
            if let Err(error) = &result {
                report(
                    &hooks_for_task,
                    S3TransportError {
                        kind: S3TransportErrorKind::Server,
                        peer: None,
                        message: error.to_string(),
                    },
                );
            }
            result
        });
        Ok(Self {
            session,
            address,
            drain_timeout: options.drain_timeout,
            active_connections,
            shutdown: Mutex::new(Some(shutdown_tx)),
            task: Mutex::new(Some(task)),
        })
    }

    pub async fn start_driver<D>(driver: D, options: S3ServerOptions) -> Result<Self, S3BindError>
    where
        D: mount_rs_core::FsDriver + 'static,
    {
        Self::start(Arc::new(S3Session::new(driver)), options).await
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn url(&self) -> String {
        let host = match self.address.ip() {
            IpAddr::V4(_) => self.address.ip().to_string(),
            IpAddr::V6(_) => format!("[{}]", self.address.ip()),
        };
        format!("http://{}:{}/", host, self.address.port())
    }

    pub fn session(&self) -> &Arc<S3Session> {
        &self.session
    }

    /// Number of accepted TCP connections that are still owned by the HTTP
    /// server. The count follows the connection lifetime, including idle
    /// keep-alive sockets, rather than counting requests.
    pub fn connections(&self) -> usize {
        self.active_connections.load(Ordering::Acquire)
    }

    pub async fn close(&self) -> Result<(), S3BindError> {
        if let Some(sender) = self.shutdown.lock().await.take() {
            let _ = sender.send(());
        }
        if let Some(task) = self.task.lock().await.take() {
            tokio::time::timeout(self.drain_timeout, task)
                .await
                .map_err(|_| S3BindError::Task("S3 server close timed out".to_owned()))?
                .map_err(|error| S3BindError::Task(error.to_string()))?
                .map_err(|error| S3BindError::Task(error.to_string()))?;
        }
        self.session
            .close()
            .await
            .map_err(|error| S3BindError::Task(error.to_string()))
    }
}

impl Drop for S3Server {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.get_mut().take() {
            let _ = sender.send(());
        }
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

struct TrackedListener {
    listener: TcpListener,
    active_connections: Arc<AtomicUsize>,
    hooks: S3ServerHooks,
}

impl Listener for TrackedListener {
    type Io = TrackedIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.listener.accept().await {
                Ok((stream, peer)) => {
                    self.active_connections.fetch_add(1, Ordering::AcqRel);
                    return (
                        TrackedIo {
                            stream,
                            active_connections: Arc::clone(&self.active_connections),
                            hooks: self.hooks.clone(),
                            peer: peer.to_string(),
                            reported: false,
                        },
                        peer,
                    );
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                    ) => {}
                Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

struct TrackedIo {
    stream: tokio::net::TcpStream,
    active_connections: Arc<AtomicUsize>,
    hooks: S3ServerHooks,
    peer: String,
    reported: bool,
}

impl TrackedIo {
    fn report_io_error<T>(&mut self, result: Poll<std::io::Result<T>>) -> Poll<std::io::Result<T>> {
        if let Poll::Ready(Err(error)) = &result
            && !self.reported
        {
            self.reported = true;
            report(
                &self.hooks,
                S3TransportError {
                    kind: S3TransportErrorKind::Connection,
                    peer: Some(self.peer.clone()),
                    message: error.to_string(),
                },
            );
        }
        result
    }
}

impl AsyncRead for TrackedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let result = Pin::new(&mut self.stream).poll_read(context, buffer);
        self.report_io_error(result)
    }
}

impl AsyncWrite for TrackedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.stream).poll_write(context, buffer);
        self.report_io_error(result)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let result = Pin::new(&mut self.stream).poll_flush(context);
        self.report_io_error(result)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let result = Pin::new(&mut self.stream).poll_shutdown(context);
        self.report_io_error(result)
    }
}

impl Drop for TrackedIo {
    fn drop(&mut self) {
        let _ =
            self.active_connections
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    count.checked_sub(1)
                });
    }
}

async fn handle_http(
    State(session): State<Arc<S3Session>>,
    request: Request<Body>,
) -> impl IntoResponse {
    let method = request.method().as_str().to_owned();
    let target = request
        .uri()
        .path_and_query()
        .map_or_else(|| "/".to_owned(), |value| value.as_str().to_owned());
    let headers = request
        .headers()
        .iter()
        .map(|(name, value)| HeaderEntry::new(name.as_str(), value.to_str().unwrap_or_default()))
        .collect::<Vec<_>>();
    let body: S3RequestBody = Box::pin(RequestBodyStream {
        inner: request.into_body().into_data_stream(),
    });
    let response = session
        .handle_request_stream(
            S3RequestHead {
                method,
                target,
                headers,
            },
            body,
        )
        .await;
    response_from_s3(response)
}

struct RequestBodyStream {
    inner: BodyDataStream,
}

impl Stream for RequestBodyStream {
    type Item = Result<Vec<u8>, String>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(context) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(bytes.to_vec()))),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error.to_string()))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn response_from_s3(response: S3StreamResponse) -> Response<Body> {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    for (name, value) in response.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            builder = builder.header(name, value);
        }
    }
    let body = match response.body {
        Some(S3StreamBody::Bytes(bytes)) => Body::from(bytes),
        Some(S3StreamBody::Stream(stream)) => Body::from_stream(stream),
        None => Body::empty(),
    };
    builder.body(body).unwrap_or_else(|_| {
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::empty())
            .expect("static response")
    })
}

/// Convenience constructor for a single-driver gateway.
pub async fn create_s3_server<D>(
    driver: D,
    options: S3ServerOptions,
    credentials: Option<Credentials>,
    region: Option<String>,
) -> Result<S3Server, S3BindError>
where
    D: mount_rs_core::FsDriver + 'static,
{
    let session_options = crate::session::S3SessionOptions {
        credentials,
        region,
        ..crate::session::S3SessionOptions::default()
    };
    let session = Arc::new(S3Session::new_with_options(driver, session_options));
    S3Server::start(session, options).await
}

/// Convenience constructor with transport failure reporting.
pub async fn create_s3_server_with_hooks<D>(
    driver: D,
    options: S3ServerOptions,
    credentials: Option<Credentials>,
    region: Option<String>,
    hooks: S3ServerHooks,
) -> Result<S3Server, S3BindError>
where
    D: mount_rs_core::FsDriver + 'static,
{
    let session_options = crate::session::S3SessionOptions {
        credentials,
        region,
        ..crate::session::S3SessionOptions::default()
    };
    let session = Arc::new(S3Session::new_with_options(driver, session_options));
    S3Server::start_with_hooks(session, options, hooks).await
}

fn report(hooks: &S3ServerHooks, error: S3TransportError) {
    if let Some(hook) = &hooks.on_transport_error {
        hook(error);
    }
}
