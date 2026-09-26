//! One authenticated QUIC connection for a single Partition.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine;
use mount_rs_remote_protocol::binary::{self, IoRequest};
use mount_rs_remote_protocol::{Message, Operation, PROTOCOL_VERSION, read_frame, write_frame};
use quinn::crypto::rustls::QuicClientConfig;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::credentials::{CredentialSource, SecretToken};
use crate::decisions::{
    QuicFailure, QuicFailureDecision, TransactionCompletion, classify_quic_failure,
    finish_io_completion,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const RENEW_MARGIN_SECONDS: i64 = 60;

#[derive(Clone, PartialEq, Eq)]
pub enum ClientError {
    Transport,
    Protocol,
    Authentication,
    Credential,
    Remote(String),
}

impl fmt::Debug for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport => f.write_str("Transport"),
            Self::Protocol => f.write_str("Protocol"),
            Self::Authentication => f.write_str("Authentication"),
            Self::Credential => f.write_str("Credential"),
            Self::Remote(_) => f.write_str("Remote([redacted])"),
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport => f.write_str("remote transport failed"),
            Self::Protocol => f.write_str("remote protocol error"),
            Self::Authentication => f.write_str("remote authentication failed"),
            Self::Credential => f.write_str("credential retrieval failed"),
            Self::Remote(_) => f.write_str("remote filesystem error"),
        }
    }
}

impl std::error::Error for ClientError {}

#[async_trait]
pub trait Transport: Send + Sync {
    fn version(&self) -> u16 {
        PROTOCOL_VERSION
    }
    async fn exchange(&self, message: Message) -> Result<Message, ClientError>;
    async fn read(
        &self,
        _id: u64,
        _request: &IoRequest<&str>,
        _buffer: &mut [u8],
    ) -> Result<usize, ClientError> {
        Err(ClientError::Protocol)
    }
    async fn write(
        &self,
        _id: u64,
        _request: &IoRequest<&str>,
        _data: &[u8],
    ) -> Result<usize, ClientError> {
        Err(ClientError::Protocol)
    }
    fn close(&self) {}
}

struct QuicTransport {
    _endpoint: quinn::Endpoint,
    connection: quinn::Connection,
    version: u16,
}

// Once a stream is acquired, dropping a caller may leave a committed operation
// without its acknowledgment. Close the connection rather than reusing it.
struct QuicTransaction<'a> {
    transport: &'a QuicTransport,
    completion: TransactionCompletion,
}

impl Drop for QuicTransaction<'_> {
    fn drop(&mut self) {
        if self.completion.destroy_on_drop() {
            self.transport.close();
        }
    }
}

#[async_trait]
impl Transport for QuicTransport {
    fn version(&self) -> u16 {
        self.version
    }
    async fn exchange(&self, message: Message) -> Result<Message, ClientError> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|_| ClientError::Transport)?;
        let mut transaction = QuicTransaction {
            transport: self,
            completion: TransactionCompletion::default(),
        };
        write_frame(&mut send, &message)
            .await
            .map_err(|_| ClientError::Transport)?;
        send.finish().map_err(|_| ClientError::Transport)?;
        let response = read_frame(&mut recv)
            .await
            .map_err(|_| ClientError::Protocol)?;
        recv.read_to_end(0)
            .await
            .map_err(|_| ClientError::Protocol)?;
        transaction.completion.complete_response();
        Ok(response)
    }

    async fn read(
        &self,
        id: u64,
        request: &IoRequest<&str>,
        buffer: &mut [u8],
    ) -> Result<usize, ClientError> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|_| ClientError::Transport)?;
        let mut transaction = QuicTransaction {
            transport: self,
            completion: TransactionCompletion::default(),
        };
        binary::write_request(&mut send, id, request, buffer.len(), None)
            .await
            .map_err(|_| ClientError::Protocol)?;
        send.finish().map_err(|_| ClientError::Transport)?;
        let result = binary::read_result(&mut recv, id, true, buffer, 0)
            .await
            .map_err(|_| ClientError::Protocol)?;
        recv.read_to_end(0)
            .await
            .map_err(|_| ClientError::Protocol)?;
        transaction.completion.complete_response();
        result.map_err(|e| ClientError::Remote(e.code))
    }
    async fn write(
        &self,
        id: u64,
        request: &IoRequest<&str>,
        data: &[u8],
    ) -> Result<usize, ClientError> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|_| ClientError::Transport)?;
        let mut transaction = QuicTransaction {
            transport: self,
            completion: TransactionCompletion::default(),
        };
        binary::write_request(&mut send, id, request, 0, Some(data))
            .await
            .map_err(|_| ClientError::Protocol)?;
        send.finish().map_err(|_| ClientError::Transport)?;
        let result = binary::read_result(&mut recv, id, false, &mut [], data.len())
            .await
            .map_err(|_| ClientError::Protocol)?;
        recv.read_to_end(0)
            .await
            .map_err(|_| ClientError::Protocol)?;
        transaction.completion.complete_response();
        result.map_err(|e| ClientError::Remote(e.code))
    }

    fn close(&self) {
        self.connection.close(0_u32.into(), b"client shutdown");
        self._endpoint.close(0_u32.into(), b"client shutdown");
    }
}

/// Initial transport selection only. No switch after authentication or RPCs.
#[derive(Clone, Copy, Debug)]
pub enum ConnectionTransport {
    Quic,
    WebSocket(SocketAddr),
    Auto { websocket: SocketAddr },
}

/// Conservative Auto selection evidence: any received datagram means the
/// endpoint is no longer positively unavailable, even before TLS exposes ALPN.
#[derive(Debug)]
struct ContactSocket {
    inner: Arc<dyn quinn::AsyncUdpSocket>,
    received: Arc<AtomicBool>,
}
impl quinn::AsyncUdpSocket for ContactSocket {
    fn create_io_poller(self: Arc<Self>) -> std::pin::Pin<Box<dyn quinn::UdpPoller>> {
        self.inner.clone().create_io_poller()
    }
    fn try_send(&self, transmit: &quinn::udp::Transmit) -> std::io::Result<()> {
        self.inner.try_send(transmit)
    }
    fn poll_recv(
        &self,
        cx: &mut std::task::Context<'_>,
        bufs: &mut [std::io::IoSliceMut<'_>],
        meta: &mut [quinn::udp::RecvMeta],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let result = self.inner.poll_recv(cx, bufs, meta);
        if matches!(&result, std::task::Poll::Ready(Ok(count)) if *count > 0) {
            self.received.store(true, Ordering::Release);
        }
        result
    }
    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.inner.local_addr()
    }
    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }
    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }
    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

pub struct RemoteConnection {
    transport: Arc<dyn Transport>,
    credentials: Option<CredentialSource>,
    expires_at: AtomicI64,
    renew_lock: Mutex<()>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl Drop for RemoteConnection {
    fn drop(&mut self) {
        self.transport.close();
    }
}

impl RemoteConnection {
    pub fn protocol_version(&self) -> u16 {
        self.transport.version()
    }
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.transport.close();
        }
    }

    pub async fn connect(
        address: SocketAddr,
        server_name: &str,
        roots: rustls::RootCertStore,
        partition_id: String,
        credentials: CredentialSource,
    ) -> Result<Arc<Self>, ClientError> {
        Self::connect_with_transport(
            address,
            server_name,
            roots,
            partition_id,
            credentials,
            ConnectionTransport::Quic,
        )
        .await
    }

    pub async fn connect_with_transport(
        address: SocketAddr,
        server_name: &str,
        roots: rustls::RootCertStore,
        partition_id: String,
        credentials: CredentialSource,
        selection: ConnectionTransport,
    ) -> Result<Arc<Self>, ClientError> {
        let transport = match selection {
            ConnectionTransport::Quic => {
                Self::connect_quic_transport(address, server_name, roots, REQUEST_TIMEOUT, false)
                    .await
                    .map_err(QuicEstablishment::client_error)?
            }
            ConnectionTransport::WebSocket(websocket) => {
                Self::connect_websocket_transport(websocket, server_name, roots).await?
            }
            ConnectionTransport::Auto { websocket } => match Self::connect_quic_transport(
                address,
                server_name,
                roots.clone(),
                Duration::from_secs(3),
                true,
            )
            .await
            {
                Ok(transport) => transport,
                Err(QuicEstablishment::Unavailable) => {
                    Self::connect_websocket_transport(websocket, server_name, roots).await?
                }
                Err(error) => return Err(error.client_error()),
            },
        };
        let token = credentials
            .token()
            .await
            .map_err(|_| ClientError::Credential)?;
        let hello = Message::ClientHello {
            version: PROTOCOL_VERSION,
            partition_id,
            bearer: token.expose().to_owned(),
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, transport.exchange(hello)).await {
            Ok(Ok(Message::ServerHello { version, .. })) if version == transport.version() => {}
            _ => return Err(ClientError::Authentication),
        }
        Ok(Arc::new(Self {
            transport,
            credentials: Some(credentials),
            expires_at: AtomicI64::new(token_exp(&token)),
            renew_lock: Mutex::new(()),
            next_id: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }))
    }

    async fn connect_quic_transport(
        address: SocketAddr,
        server_name: &str,
        roots: rustls::RootCertStore,
        connect_timeout: Duration,
        classify_unavailable: bool,
    ) -> Result<Arc<dyn Transport>, QuicEstablishment> {
        let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| ClientError::Transport)?
        .with_root_certificates(roots)
        .with_no_client_auth();
        tls.alpn_protocols = vec![b"mount-rs/2".to_vec()];
        let mut config = quinn::ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(tls).map_err(|_| ClientError::Transport)?,
        ));
        // Modest per-connection queues still stream the full 8MiB generic
        // control cap; application admission independently bounds allocations.
        let mut windows = quinn::TransportConfig::default();
        windows.stream_receive_window((512 * 1024_u32).into());
        windows.receive_window((4 * 1024 * 1024_u32).into());
        windows.send_window(1024 * 1024);
        windows.max_concurrent_bidi_streams(0_u32.into());
        windows.max_concurrent_uni_streams(0_u32.into());
        config.transport_config(Arc::new(windows));
        let bind: SocketAddr = if address.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        }
        .parse()
        .map_err(|_| ClientError::Transport)?;
        let received = Arc::new(AtomicBool::new(false));
        let mut endpoint = if classify_unavailable {
            use quinn::Runtime;
            let udp = std::net::UdpSocket::bind(bind).map_err(|_| ClientError::Transport)?;
            udp.set_nonblocking(true)
                .map_err(|_| ClientError::Transport)?;
            let runtime = Arc::new(quinn::TokioRuntime);
            let inner = runtime
                .wrap_udp_socket(udp)
                .map_err(|_| ClientError::Transport)?;
            let socket = Arc::new(ContactSocket {
                inner,
                received: received.clone(),
            });
            quinn::Endpoint::new_with_abstract_socket(
                quinn::EndpointConfig::default(),
                None,
                socket,
                runtime,
            )
            .map_err(|_| ClientError::Transport)?
        } else {
            quinn::Endpoint::client(bind).map_err(|_| ClientError::Transport)?
        };
        endpoint.set_default_client_config(config);
        let connection = tokio::time::timeout(
            connect_timeout,
            endpoint
                .connect(address, server_name)
                .map_err(|_| ClientError::Transport)?,
        )
        .await
        .map_err(|_| {
            QuicEstablishment::from(classify_quic_failure(
                received.load(Ordering::Acquire),
                QuicFailure::Deadline,
            ))
        })?
        .map_err(|error| {
            let failure = match error {
                quinn::ConnectionError::TimedOut => QuicFailure::TimedOut,
                quinn::ConnectionError::ConnectionClosed(ref close)
                    if close.error_code == quinn::TransportErrorCode::CONNECTION_REFUSED =>
                {
                    QuicFailure::Refused
                }
                _ => QuicFailure::Other,
            };
            QuicEstablishment::from(classify_quic_failure(
                received.load(Ordering::Acquire),
                failure,
            ))
        })?;
        let handshake = connection
            .handshake_data()
            .ok_or(ClientError::Protocol)?
            .downcast::<quinn::crypto::rustls::HandshakeData>()
            .map_err(|_| ClientError::Protocol)?;
        if handshake.protocol.as_deref() != Some(b"mount-rs/2") {
            return Err(ClientError::Protocol.into());
        }
        let transport: Arc<dyn Transport> = Arc::new(QuicTransport {
            _endpoint: endpoint,
            connection,
            version: PROTOCOL_VERSION,
        });
        Ok(transport)
    }

    async fn connect_websocket_transport(
        address: SocketAddr,
        server_name: &str,
        roots: rustls::RootCertStore,
    ) -> Result<Arc<dyn Transport>, ClientError> {
        tokio::time::timeout(
            Duration::from_secs(10),
            crate::websocket::WebSocketTransport::connect(address, server_name, roots),
        )
        .await
        .map_err(|_| ClientError::Transport)?
    }

    #[cfg(test)]
    pub(crate) fn from_transport(transport: Arc<dyn Transport>) -> Arc<Self> {
        Arc::new(Self {
            transport,
            credentials: None,
            expires_at: AtomicI64::new(i64::MAX),
            renew_lock: Mutex::new(()),
            next_id: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        })
    }

    pub async fn request(
        &self,
        drive_id: &str,
        operation: Operation,
    ) -> Result<Value, ClientError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ClientError::Transport);
        }
        self.renew_if_needed().await?;
        let request_id = self
            .next_id
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or(ClientError::Protocol)?;
        let message = Message::Request {
            request_id,
            drive_id: drive_id.to_owned(),
            operation,
        };
        let response =
            match tokio::time::timeout(REQUEST_TIMEOUT, self.transport.exchange(message)).await {
                Ok(Ok(response)) => response,
                _ => {
                    self.close();
                    return Err(ClientError::Transport);
                }
            };
        let result = match response {
            Message::Response {
                request_id: response_id,
                result,
            } if response_id == request_id => {
                result.map_err(|error| ClientError::Remote(error.code))
            }
            _ => Err(ClientError::Protocol),
        };
        if matches!(result, Err(ClientError::Protocol)) {
            self.close();
        }
        result
    }

    async fn io_id(&self) -> Result<u64, ClientError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ClientError::Transport);
        }
        self.renew_if_needed().await?;
        self.next_id
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or(ClientError::Protocol)
    }
    pub async fn read(
        &self,
        drive_id: &str,
        handle: u64,
        position: Option<u64>,
        buffer: &mut [u8],
    ) -> Result<usize, ClientError> {
        if buffer.len() > binary::MAX_IO_BYTES {
            return Err(ClientError::Protocol);
        }
        let id = self.io_id().await?;
        let request = IoRequest {
            drive_id,
            handle,
            position,
        };
        {
            let limit = buffer.len();
            self.finish_io(
                tokio::time::timeout(REQUEST_TIMEOUT, self.transport.read(id, &request, buffer))
                    .await,
                limit,
            )
        }
    }
    pub async fn write(
        &self,
        drive_id: &str,
        handle: u64,
        position: Option<u64>,
        data: &[u8],
    ) -> Result<usize, ClientError> {
        if data.len() > binary::MAX_IO_BYTES {
            return Err(ClientError::Protocol);
        }
        let id = self.io_id().await?;
        let request = IoRequest {
            drive_id,
            handle,
            position,
        };
        self.finish_io(
            tokio::time::timeout(REQUEST_TIMEOUT, self.transport.write(id, &request, data)).await,
            data.len(),
        )
    }
    fn finish_io(
        &self,
        result: Result<Result<usize, ClientError>, tokio::time::error::Elapsed>,
        limit: usize,
    ) -> Result<usize, ClientError> {
        let (result, close) = finish_io_completion(result.ok(), limit);
        if close {
            self.close();
        }
        result
    }

    async fn renew_if_needed(&self) -> Result<(), ClientError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ClientError::Transport);
        }
        let Some(credentials) = &self.credentials else {
            return Ok(());
        };
        if self.expires_at.load(Ordering::Acquire) > now_seconds() + RENEW_MARGIN_SECONDS {
            return Ok(());
        }
        let _guard = self.renew_lock.lock().await;
        if self.expires_at.load(Ordering::Acquire) > now_seconds() + RENEW_MARGIN_SECONDS {
            return Ok(());
        }
        let token = credentials
            .token()
            .await
            .map_err(|_| ClientError::Credential)?;
        let response = tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.transport.exchange(Message::Renew {
                bearer: token.expose().to_owned(),
            }),
        )
        .await;
        let response = match response {
            Ok(Ok(response)) => response,
            _ => {
                self.close();
                return Err(ClientError::Transport);
            }
        };
        match response {
            Message::ServerHello { version, .. } if version == self.transport.version() => {
                self.expires_at.store(token_exp(&token), Ordering::Release);
                Ok(())
            }
            _ => {
                self.close();
                Err(ClientError::Authentication)
            }
        }
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// This claim only schedules renewal. The server verifies every token.
fn token_exp(token: &SecretToken) -> i64 {
    let Some(payload) = token.expose().split('.').nth(1) else {
        return 0;
    };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
        return 0;
    };
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| value.get("exp")?.as_i64())
        .unwrap_or(0)
}

enum QuicEstablishment {
    Unavailable,
    Fatal(ClientError),
}
impl From<QuicFailureDecision> for QuicEstablishment {
    fn from(decision: QuicFailureDecision) -> Self {
        match decision {
            QuicFailureDecision::Unavailable => Self::Unavailable,
            QuicFailureDecision::Transport => Self::Fatal(ClientError::Transport),
            QuicFailureDecision::Authentication => Self::Fatal(ClientError::Authentication),
        }
    }
}
impl From<ClientError> for QuicEstablishment {
    fn from(error: ClientError) -> Self {
        Self::Fatal(error)
    }
}
impl QuicEstablishment {
    fn client_error(self) -> ClientError {
        match self {
            Self::Unavailable => ClientError::Transport,
            Self::Fatal(error) => error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_remote_protocol::OperationName;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeTransport {
        calls: AtomicUsize,
        closed: AtomicBool,
        response: Message,
    }

    #[async_trait]
    impl Transport for FakeTransport {
        async fn exchange(&self, _: Message) -> Result<Message, ClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.response.clone())
        }
        fn close(&self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn rejects_mismatched_response_without_retry() {
        let transport = Arc::new(FakeTransport {
            calls: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            response: Message::Response {
                request_id: 999,
                result: Ok(Value::Null),
            },
        });
        let connection = RemoteConnection::from_transport(transport.clone());
        let result = connection
            .request(
                "drive",
                Operation {
                    name: OperationName::Stat,
                    body: serde_json::json!({"path":"/"}),
                },
            )
            .await;
        assert!(matches!(result, Err(ClientError::Protocol)));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert!(transport.closed.load(Ordering::SeqCst));
        let second = connection
            .request(
                "drive",
                Operation {
                    name: OperationName::Stat,
                    body: serde_json::json!({"path":"/"}),
                },
            )
            .await;
        assert!(matches!(second, Err(ClientError::Transport)));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
}
