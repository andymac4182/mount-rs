//! Serialized, verifying TLS WebSocket transport. No reconnect or request replay.
use crate::connection::{ClientError, Transport};
use crate::decisions::TransactionCompletion;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use mount_rs_remote_protocol::{
    Message,
    binary::{self, Header, IoRequest},
};
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    io::AsyncReadExt,
    net::TcpStream,
    sync::{Mutex, watch},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message as WsMessage, client::IntoClientRequest, protocol::WebSocketConfig},
};

pub const CHUNK_BYTES: usize = 32 * 1024;
pub const SUBPROTOCOL: &str = "mount-rs.v2";
type Socket = WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(CHUNK_BYTES * 2)
        .max_message_size(Some(CHUNK_BYTES))
        .max_frame_size(Some(CHUNK_BYTES))
}

pub(crate) struct WebSocketTransport {
    socket: Mutex<Option<Socket>>,
    shutdown: watch::Sender<bool>,
}
impl WebSocketTransport {
    pub(crate) async fn connect(
        address: SocketAddr,
        name: &str,
        roots: rustls::RootCertStore,
    ) -> Result<Arc<dyn Transport>, ClientError> {
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| ClientError::Protocol)?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let server_name = rustls::pki_types::ServerName::try_from(name.to_owned())
            .map_err(|_| ClientError::Protocol)?;
        let tcp = TcpStream::connect(address)
            .await
            .map_err(|_| ClientError::Transport)?;
        tcp.set_nodelay(true).map_err(|_| ClientError::Transport)?;
        let tls = tokio_rustls::TlsConnector::from(Arc::new(tls))
            .connect(server_name, tcp)
            .await
            .map_err(|_| ClientError::Authentication)?;
        let mut request = format!("wss://{address}/mount-rs")
            .into_client_request()
            .map_err(|_| ClientError::Protocol)?;
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", SUBPROTOCOL.parse().unwrap());
        let (socket, response) =
            tokio_tungstenite::client_async_with_config(request, tls, Some(config()))
                .await
                .map_err(|_| ClientError::Protocol)?;
        if response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok())
            != Some(SUBPROTOCOL)
        {
            return Err(ClientError::Protocol);
        }
        let (shutdown, _) = watch::channel(false);
        Ok(Arc::new(Self {
            socket: Mutex::new(Some(socket)),
            shutdown,
        }))
    }
    async fn transact(&self, request: Request<'_>) -> Result<Vec<u8>, ClientError> {
        let mut shutdown = self.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(ClientError::Transport);
        }
        let guard = tokio::select! {
            biased;
            _ = shutdown.changed() => return Err(ClientError::Transport),
            guard = self.socket.lock() => guard,
        };
        let mut transaction = Transaction {
            socket: guard,
            shutdown: &self.shutdown,
            completion: TransactionCompletion::default(),
        };
        let socket = transaction.socket.as_mut().ok_or(ClientError::Transport)?;
        let result = tokio::select! {
            biased;
            _ = shutdown.changed() => Err(ClientError::Transport),
            result = async {
                let mut bytes = Vec::new();
                match &request {
                    Request::Control(message) => binary::write_control(&mut bytes, message).await,
                    Request::Io{id, request, length, data} => binary::write_request(&mut bytes, *id, *request, *length, *data).await,
                }.map_err(|_| ClientError::Protocol)?;
                send(socket, &bytes).await?;
                let header = receive(socket).await?;
                let header: [u8; binary::HEADER_BYTES] = header.as_ref().try_into().map_err(|_| ClientError::Protocol)?;
                let decoded = Header::decode(header).map_err(|_| ClientError::Protocol)?;
                let length = decoded.control_len + decoded.payload_len;
                let mut bytes = Vec::with_capacity(binary::HEADER_BYTES + length);
                bytes.extend_from_slice(&header);
                while bytes.len() < binary::HEADER_BYTES + length {
                    let chunk = receive(socket).await?;
                    if chunk.is_empty() || chunk.len() > binary::HEADER_BYTES + length - bytes.len() {return Err(ClientError::Protocol);}
                    bytes.extend_from_slice(&chunk);
                }
                if !receive(socket).await?.is_empty() {return Err(ClientError::Protocol);}
                request.validate_response(decoded, &bytes[binary::HEADER_BYTES..])?;
                Ok(bytes)
            } => result,
        };
        if result.is_ok() {
            transaction.completion.complete_response();
        }
        result
    }
}
// Dropping a caller future after acquiring the transaction must destroy the
// socket: partial envelopes and unread responses cannot be reused safely.
struct Transaction<'a> {
    socket: tokio::sync::MutexGuard<'a, Option<Socket>>,
    shutdown: &'a watch::Sender<bool>,
    completion: TransactionCompletion,
}
impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if self.completion.destroy_on_drop() {
            self.socket.take();
            self.shutdown.send_replace(true);
        }
    }
}
enum Request<'a> {
    Control(Message),
    Io {
        id: u64,
        request: &'a IoRequest<&'a str>,
        length: usize,
        data: Option<&'a [u8]>,
    },
}
impl Request<'_> {
    fn validate_response(&self, header: Header, body: &[u8]) -> Result<(), ClientError> {
        use binary::Kind;
        let valid = match self {
            Self::Control(request) if header.kind == Kind::Control => {
                let response: Message =
                    serde_json::from_slice(body).map_err(|_| ClientError::Protocol)?;
                match (request, response) {
                    (
                        Message::ClientHello { .. } | Message::Renew { .. },
                        Message::ServerHello { version, .. },
                    ) => version == mount_rs_remote_protocol::PROTOCOL_VERSION,
                    (
                        Message::Request { request_id, .. },
                        Message::Response {
                            request_id: response_id,
                            ..
                        },
                    ) => *request_id == response_id,
                    _ => false,
                }
            }
            Self::Io {
                id, length, data, ..
            } if header.request_id == *id => match header.kind {
                Kind::Error => body
                    .iter()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
                Kind::ReadResult => data.is_none() && header.count <= *length,
                Kind::WriteResult => data.is_some_and(|data| header.count <= data.len()),
                _ => false,
            },
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(ClientError::Protocol)
        }
    }
}
async fn receive(
    socket: &mut Socket,
) -> Result<tokio_tungstenite::tungstenite::Bytes, ClientError> {
    loop {
        match socket.next().await {
            Some(Ok(WsMessage::Binary(bytes))) => return Ok(bytes),
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {
                socket.flush().await.map_err(|_| ClientError::Transport)?;
            }
            Some(Ok(WsMessage::Close(_))) | None => return Err(ClientError::Transport),
            _ => return Err(ClientError::Protocol),
        }
    }
}
async fn send(socket: &mut Socket, bytes: &[u8]) -> Result<(), ClientError> {
    socket
        .send(WsMessage::Binary(
            bytes[..binary::HEADER_BYTES].to_vec().into(),
        ))
        .await
        .map_err(|_| ClientError::Transport)?;
    for chunk in bytes[binary::HEADER_BYTES..].chunks(CHUNK_BYTES) {
        socket
            .send(WsMessage::Binary(chunk.to_vec().into()))
            .await
            .map_err(|_| ClientError::Transport)?;
    }
    socket
        .send(WsMessage::Binary(Vec::new().into()))
        .await
        .map_err(|_| ClientError::Transport)?;
    Ok(())
}
#[async_trait]
impl Transport for WebSocketTransport {
    async fn exchange(&self, message: Message) -> Result<Message, ClientError> {
        let bytes = self.transact(Request::Control(message)).await?;
        let mut cursor = bytes.as_slice();
        let response = binary::read_control(&mut cursor)
            .await
            .map_err(|_| ClientError::Protocol)?;
        if !cursor.is_empty() {
            return Err(ClientError::Protocol);
        }
        Ok(response)
    }
    async fn read(
        &self,
        id: u64,
        request: &IoRequest<&str>,
        buffer: &mut [u8],
    ) -> Result<usize, ClientError> {
        let bytes = self
            .transact(Request::Io {
                id,
                request,
                length: buffer.len(),
                data: None,
            })
            .await?;
        let mut cursor = bytes.as_slice();
        let result = binary::read_result(&mut cursor, id, true, buffer, 0)
            .await
            .map_err(|_| ClientError::Protocol)?;
        if cursor.read_u8().await.is_ok() {
            return Err(ClientError::Protocol);
        }
        result.map_err(|e| ClientError::Remote(e.code))
    }
    async fn write(
        &self,
        id: u64,
        request: &IoRequest<&str>,
        data: &[u8],
    ) -> Result<usize, ClientError> {
        let bytes = self
            .transact(Request::Io {
                id,
                request,
                length: 0,
                data: Some(data),
            })
            .await?;
        let mut cursor = bytes.as_slice();
        let result = binary::read_result(&mut cursor, id, false, &mut [], data.len())
            .await
            .map_err(|_| ClientError::Protocol)?;
        if !cursor.is_empty() {
            return Err(ClientError::Protocol);
        }
        result.map_err(|e| ClientError::Remote(e.code))
    }
    fn close(&self) {
        self.shutdown.send_replace(true);
        if let Ok(mut guard) = self.socket.try_lock() {
            guard.take();
        }
    }
}
