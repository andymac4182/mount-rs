//! TLS WebSocket listener using the authoritative Drive dispatcher.
//! One MRB2 header message precedes bounded binary body chunks. No pipelining.
use crate::{
    dispatch::{DriveDispatcher, SessionHandles, SessionIdentity},
    server::{Authenticator, RemoteServerOptions},
    transfer::{Admission, Budgets, RemoteTransferLimits, ResponseBuffer, ResponseReservation},
};
use futures_util::{SinkExt, StreamExt};
use mount_rs_remote_protocol::{
    Message, PROTOCOL_VERSION,
    binary::{self, Header, Incoming},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message as WsMessage,
        handshake::server::{ErrorResponse, Request, Response},
        protocol::WebSocketConfig,
    },
};

const CHUNK_BYTES: usize = 32 * 1024;
const SUBPROTOCOL: &str = "mount-rs.v2";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
type Socket = WebSocketStream<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>;
type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct WebSocketServer {
    address: SocketAddr,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl WebSocketServer {
    pub async fn bind(
        address: SocketAddr,
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        dispatcher: Arc<DriveDispatcher>,
        authenticator: Arc<dyn Authenticator>,
    ) -> Result<Self, Error> {
        Self::bind_with_options(
            address,
            certs,
            key,
            dispatcher,
            authenticator,
            RemoteServerOptions::default(),
        )
        .await
    }
    pub async fn bind_with_options(
        address: SocketAddr,
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        dispatcher: Arc<DriveDispatcher>,
        authenticator: Arc<dyn Authenticator>,
        options: RemoteServerOptions,
    ) -> Result<Self, Error> {
        Self::bind_with_transfer_limits(
            address,
            certs,
            key,
            dispatcher,
            authenticator,
            options,
            RemoteTransferLimits::default(),
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_with_transfer_limits(
        address: SocketAddr,
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        dispatcher: Arc<DriveDispatcher>,
        authenticator: Arc<dyn Authenticator>,
        options: RemoteServerOptions,
        limits: RemoteTransferLimits,
    ) -> Result<Self, Error> {
        options.validate()?;
        limits.validate()?;
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind(address).await?;
        let address = listener.local_addr()?;
        let connections = Arc::new(Semaphore::new(options.max_connections));
        let budgets = Arc::new(Budgets::new(limits));
        let (shutdown, mut stopping) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut sessions = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = stopping.changed() => break,
                    Some(_) = sessions.join_next(), if !sessions.is_empty() => {},
                    incoming = listener.accept() => {
                        let Ok((tcp, _)) = incoming else {break;};
                        let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {drop(tcp); continue;};
                        let _ = tcp.set_nodelay(true);
                        let acceptor = acceptor.clone();
                        let dispatcher = Arc::clone(&dispatcher);
                        let authenticator = Arc::clone(&authenticator);
                        let budgets = Arc::clone(&budgets);
                        let mut stopping = stopping.clone();
                        sessions.spawn(async move {
                            let _permit = permit;
                            let handshake = async {
                                let tls = acceptor.accept(tcp).await?;
                                let config = WebSocketConfig::default().read_buffer_size(4096).write_buffer_size(0)
                                    .max_write_buffer_size(CHUNK_BYTES * 2).max_message_size(Some(CHUNK_BYTES)).max_frame_size(Some(CHUNK_BYTES));
                                Ok::<_, Error>(tokio_tungstenite::accept_hdr_async_with_config(tls, upgrade, Some(config)).await?)
                            };
                            let socket = tokio::select! {
                                biased;
                                _ = stopping.changed() => return,
                                result = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake) => match result {Ok(Ok(socket)) => socket, _ => return},
                            };
                            serve(socket, dispatcher, authenticator, budgets, stopping).await;
                        });
                    }
                }
            }
            // Each session observes cancellation and closes handles before joining.
            while sessions.join_next().await.is_some() {}
        });
        Ok(Self {
            address,
            shutdown,
            task,
        })
    }
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    pub async fn close(self) {
        self.shutdown.send_replace(true);
        let _ = self.task.await;
    }
}
#[allow(clippy::result_large_err)]
fn upgrade(request: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
    if request.uri().path() != "/mount-rs"
        || request
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok())
            != Some(SUBPROTOCOL)
    {
        let mut denial = ErrorResponse::new(Some("unsupported remote protocol".into()));
        *denial.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::BAD_REQUEST;
        return Err(denial);
    }
    response
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", SUBPROTOCOL.parse().unwrap());
    Ok(response)
}
async fn receive(socket: &mut Socket) -> Result<tokio_tungstenite::tungstenite::Bytes, Error> {
    loop {
        match socket.next().await {
            Some(Ok(WsMessage::Binary(bytes))) => return Ok(bytes),
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => socket.flush().await?,
            _ => return Err("invalid or closed websocket message".into()),
        }
    }
}
async fn incoming(socket: &mut Socket, budgets: &Budgets) -> Result<(Incoming, Admission), Error> {
    let bytes = receive(socket).await?;
    let header = Header::decode(
        bytes
            .as_ref()
            .try_into()
            .map_err(|_| "invalid header message")?,
    )?;
    let admission = budgets.ingress(header)?;
    let _wire_permit = budgets.websocket_wire(header)?;
    // Charge before even the library allocates its first body message. The
    // fixed <=32KiB pre-header window is bounded separately by connections.
    let length = header.control_len + header.payload_len;
    let mut bytes = Vec::with_capacity(length);
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        while bytes.len() < length {
            let chunk = receive(socket).await?;
            if chunk.is_empty() || chunk.len() > length - bytes.len() {
                return Err::<(), Error>("invalid body chunk".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if !receive(socket).await?.is_empty() {
            return Err("invalid envelope terminator".into());
        }
        Ok(())
    })
    .await??;
    let mut cursor = bytes.as_slice();
    let incoming = binary::read_body(&mut cursor, header).await?;
    if !cursor.is_empty() {
        return Err("trailing body bytes".into());
    }
    Ok((incoming, admission))
}
async fn send(socket: &mut Socket, bytes: &[u8]) -> Result<(), Error> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        socket
            .send(WsMessage::Binary(
                bytes[..binary::HEADER_BYTES].to_vec().into(),
            ))
            .await?;
        for chunk in bytes[binary::HEADER_BYTES..].chunks(CHUNK_BYTES) {
            socket
                .send(WsMessage::Binary(chunk.to_vec().into()))
                .await?;
        }
        socket.send(WsMessage::Binary(Vec::new().into())).await?;
        Ok::<(), Error>(())
    })
    .await?
}
async fn serve(
    mut socket: Socket,
    dispatcher: Arc<DriveDispatcher>,
    authenticator: Arc<dyn Authenticator>,
    budgets: Arc<Budgets>,
    mut stopping: watch::Receiver<bool>,
) {
    let handles = SessionHandles::default();
    // Cancellation drops in-flight RPCs (including uncertain committed writes),
    // then cleanup always executes outside that cancelled future.
    tokio::select! {
        biased;
        _ = stopping.changed() => {},
        _ = session(&mut socket, &dispatcher, &*authenticator, &budgets, &handles) => {},
    }
    drop(socket);
    handles.close_all().await;
}
async fn session(
    socket: &mut Socket,
    dispatcher: &DriveDispatcher,
    authenticator: &dyn Authenticator,
    budgets: &Budgets,
    handles: &SessionHandles,
) -> Result<(), Error> {
    let (hello, admission) =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming(socket, budgets)).await??;
    let Incoming::Control(Message::ClientHello {
        version,
        partition_id,
        bearer,
    }) = hello
    else {
        return Err("invalid hello".into());
    };
    if version != PROTOCOL_VERSION {
        return Err("unsupported protocol".into());
    }
    let mut identity = tokio::time::timeout(
        OPERATION_TIMEOUT,
        authenticator.authenticate(&bearer, &partition_id),
    )
    .await?
    .map_err(|_| "authentication failed")?;
    if identity.partition_id != partition_id {
        return Err("partition mismatch".into());
    }
    let hello = Message::ServerHello {
        version: PROTOCOL_VERSION,
        session_id: crate::server::session_id(),
    };
    let reservation = ResponseReservation::message(&hello);
    let permit = budgets.egress(reservation.control, reservation.charge)?;
    let mut output = ResponseBuffer::new(reservation.wire_limit);
    binary::write_control(&mut output, &hello).await?;
    send(socket, &output.bytes).await?;
    drop((permit, admission));
    loop {
        let (request, mut admission) = incoming(socket, budgets).await?;
        if handles.is_closed().await {
            return Err("session invalidated".into());
        }
        if matches!(request, Incoming::Control(Message::Request { .. })) {
            budgets.request_operation(&mut admission)?;
        }
        let reservation = match &request {
            Incoming::Control(message) => ResponseReservation::message(message),
            Incoming::Read { length, .. } => ResponseReservation::io(*length),
            Incoming::Write { .. } => ResponseReservation::io(0),
        };
        let permit = budgets.egress(reservation.control, reservation.charge)?;
        let mut output = ResponseBuffer::new(reservation.wire_limit);
        tokio::time::timeout(
            OPERATION_TIMEOUT,
            dispatch(
                request,
                &mut identity,
                dispatcher,
                authenticator,
                handles,
                &mut output,
            ),
        )
        .await??;
        send(socket, &output.bytes).await?;
        drop((permit, admission));
    }
}
async fn dispatch(
    request: Incoming,
    identity: &mut SessionIdentity,
    dispatcher: &DriveDispatcher,
    authenticator: &dyn Authenticator,
    handles: &SessionHandles,
    output: &mut ResponseBuffer,
) -> Result<(), Error> {
    match request {
        Incoming::Control(Message::Request {
            request_id,
            drive_id,
            operation,
        }) if request_id != 0 => {
            let result = dispatcher
                .dispatch_request(identity, &drive_id, &operation, handles, request_id)
                .await;
            binary::write_control(output, &Message::Response { request_id, result }).await?;
        }
        Incoming::Control(Message::Renew { bearer }) => {
            let next = authenticator
                .authenticate(&bearer, &identity.partition_id)
                .await
                .map_err(|_| "authentication failed")?;
            if next.partition_id != identity.partition_id
                || next.policy_id != identity.policy_id
                || next.issuer != identity.issuer
                || next.subject != identity.subject
                || !dispatcher.renewal_matches(identity, &next).await
            {
                return Err("identity changed".into());
            }
            if handles.is_closed().await {
                return Err("session invalidated".into());
            }
            *identity = next;
            binary::write_control(
                output,
                &Message::ServerHello {
                    version: PROTOCOL_VERSION,
                    session_id: "renewed".into(),
                },
            )
            .await?;
        }
        Incoming::Read {
            request_id,
            request,
            length,
        } => {
            let result = dispatcher
                .dispatch_io(identity, &request, handles, request_id, length, None)
                .await;
            binary::write_result(output, request_id, result).await?;
        }
        Incoming::Write {
            request_id,
            request,
            data,
        } => {
            let result = dispatcher
                .dispatch_io(identity, &request, handles, request_id, 0, Some(&data))
                .await;
            binary::write_result(output, request_id, result).await?;
        }
        _ => return Err("invalid request".into()),
    }
    Ok(())
}
