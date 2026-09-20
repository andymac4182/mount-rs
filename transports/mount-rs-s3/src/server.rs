//! The HTTP boundary for the S3 session.
//!
//! The unauthenticated mode is deliberately loopback-only. A configured
//! credential pair enables SigV4 verification before dispatch and permits an
//! operator-selected non-loopback bind.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderName, HeaderValue, Request, Response, StatusCode},
    response::IntoResponse,
    routing::any,
};
use thiserror::Error;
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
    task::JoinHandle,
};

use crate::protocol::S3Response;
use crate::session::{S3RequestHead, S3Session};
use crate::sigv4::{Credentials, HeaderEntry};

pub const DEFAULT_HOST: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

#[derive(Debug, Clone)]
pub struct S3ServerOptions {
    pub host: IpAddr,
    pub port: u16,
}

impl Default for S3ServerOptions {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST,
            port: 0,
        }
    }
}

#[derive(Debug, Error)]
pub enum S3BindError {
    #[error("unauthenticated S3 gateway must bind a loopback address; refused {host}")]
    UnauthenticatedNonLoopback { host: IpAddr },
    #[error("failed to bind S3 gateway: {0}")]
    Bind(#[source] std::io::Error),
    #[error("S3 server task failed: {0}")]
    Task(String),
}

pub struct S3Server {
    session: Arc<S3Session>,
    address: SocketAddr,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<JoinHandle<Result<(), std::io::Error>>>>,
}

impl S3Server {
    pub async fn start(
        session: Arc<S3Session>,
        options: S3ServerOptions,
    ) -> Result<Self, S3BindError> {
        if session.options.credentials.is_none() && !options.host.is_loopback() {
            return Err(S3BindError::UnauthenticatedNonLoopback { host: options.host });
        }
        let listener = TcpListener::bind(SocketAddr::new(options.host, options.port))
            .await
            .map_err(S3BindError::Bind)?;
        let address = listener.local_addr().map_err(S3BindError::Bind)?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let app = Router::new()
            .fallback(any(handle_http))
            .with_state(session.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
        });
        Ok(Self {
            session,
            address,
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

    pub async fn close(&self) -> Result<(), S3BindError> {
        if let Some(sender) = self.shutdown.lock().await.take() {
            let _ = sender.send(());
        }
        if let Some(task) = self.task.lock().await.take() {
            task.await
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
    let body = match to_bytes(request.into_body(), session.options.max_body_bytes).await {
        Ok(body) => body.to_vec(),
        Err(_) => {
            return response_from_s3(S3Response {
                status: 413,
                headers: vec![("content-length".to_owned(), "0".to_owned())],
                body: Vec::new(),
            });
        }
    };
    let response = session
        .handle_request(
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

fn response_from_s3(response: S3Response) -> Response<Body> {
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
    builder.body(Body::from(response.body)).unwrap_or_else(|_| {
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
