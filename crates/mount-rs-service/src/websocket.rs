//! TLS WebSocket listener using the authoritative Drive dispatcher.
//! One MRB2 header message precedes bounded binary body chunks. No pipelining.
pub use crate::server::{WebSocketDiagnostics, WebSocketSnapshot};
use crate::{
    dispatch::{DriveDispatcher, SessionHandles, SessionIdentity},
    server::{
        Authenticator, DiagnosticOperation, Outcome, RemoteServerOptions, ServerDiagnostics, Span,
        auth_scope,
    },
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
    diagnostics: Option<WebSocketDiagnostics>,
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
        Self::bind_with_diagnostics(
            address,
            certs,
            key,
            dispatcher,
            authenticator,
            options,
            limits,
            false,
        )
        .await
    }
    /// Explicit application observer in an io-profiling build. Ordinary binds
    /// remain disabled even when PROFILE_IO is set. TRACE_SERVICE additionally
    /// enables the existing bounded, fixed-label slow records.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_with_diagnostics(
        address: SocketAddr,
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        dispatcher: Arc<DriveDispatcher>,
        authenticator: Arc<dyn Authenticator>,
        options: RemoteServerOptions,
        limits: RemoteTransferLimits,
        diagnostics_enabled: bool,
    ) -> Result<Self, Error> {
        options.validate()?;
        limits.validate()?;
        let diagnostics = (diagnostics_enabled && cfg!(feature = "io-profiling")).then(|| {
            WebSocketDiagnostics::new(
                std::env::var_os("MOUNT_RS_TRACE_SERVICE").is_some_and(|value| value == "1"),
            )
        });
        let task_diagnostics = diagnostics.clone();
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
                        let observer = task_diagnostics.as_ref().map(WebSocketDiagnostics::application_observer);
                        let mut admission = Span::new(observer, DiagnosticOperation::ConnectionAdmission);
                        let permit = Arc::clone(&connections).try_acquire_owned();
                        admission.finish(Outcome::result(&permit));
                        let Ok(permit) = permit else {drop(tcp); continue;};
                        let _ = tcp.set_nodelay(true);
                        let acceptor = acceptor.clone();
                        let dispatcher = Arc::clone(&dispatcher);
                        let authenticator = Arc::clone(&authenticator);
                        let budgets = Arc::clone(&budgets);
                        let mut stopping = stopping.clone();
                        let diagnostics = task_diagnostics.clone();
                        sessions.spawn(async move {
                            let _permit = permit;
                            let observer = diagnostics.as_ref().map(WebSocketDiagnostics::application_observer);
                            let mut application_handshake = Span::new(observer, DiagnosticOperation::Handshake);
                            let handshake = async {
                                let tls = observed(observer, DiagnosticOperation::TlsHandshake, acceptor.accept(tcp)).await?;
                                let config = WebSocketConfig::default().read_buffer_size(4096).write_buffer_size(0)
                                    .max_write_buffer_size(CHUNK_BYTES * 2).max_message_size(Some(CHUNK_BYTES)).max_frame_size(Some(CHUNK_BYTES));
                                Ok::<_, Error>(observed(observer, DiagnosticOperation::WebSocketUpgrade,
                                    tokio_tungstenite::accept_hdr_async_with_config(tls, upgrade, Some(config))).await?)
                            };
                            let socket = tokio::select! {
                                biased;
                                _ = stopping.changed() => return,
                                result = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake) => match result {
                                    Ok(Ok(socket)) => socket,
                                    Ok(Err(_)) => { application_handshake.finish(Outcome::Error); return; },
                                    Err(_) => { application_handshake.finish(Outcome::Timeout); return; },
                                },
                            };
                            serve(socket, dispatcher, authenticator, budgets, stopping, observer, application_handshake).await;
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
            diagnostics,
        })
    }
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    #[must_use]
    pub fn diagnostics(&self) -> Option<WebSocketDiagnostics> {
        self.diagnostics.clone()
    }
    pub async fn close(self) {
        self.shutdown.send_replace(true);
        let _ = self.task.await;
    }
}

async fn observed<T, E>(
    observer: Option<&ServerDiagnostics>,
    operation: DiagnosticOperation,
    future: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let mut span = Span::new(observer, operation);
    let result = future.await;
    span.finish(Outcome::result(&result));
    result
}

pub(crate) struct Failure {
    error: Error,
    outcome: Outcome,
}

/// Hello retains its existing auth deadline; renewal is timed by dispatch.
pub(crate) async fn authenticate(
    authenticator: &dyn Authenticator,
    bearer: &str,
    partition: &str,
    observer: Option<&ServerDiagnostics>,
    deadline: Option<Duration>,
) -> Result<SessionIdentity, Failure> {
    let mut span = Span::new(observer, DiagnosticOperation::Authentication);
    let future = auth_scope(observer, authenticator.authenticate(bearer, partition));
    let result = if let Some(deadline) = deadline {
        match tokio::time::timeout(deadline, future).await {
            Ok(result) => result,
            Err(error) => {
                span.finish(Outcome::Timeout);
                return Err(Failure {
                    error: error.into(),
                    outcome: Outcome::Timeout,
                });
            }
        }
    } else {
        future.await
    };
    let outcome = Outcome::result(&result);
    span.finish(outcome);
    result.map_err(|_| Failure {
        error: "authentication failed".into(),
        outcome,
    })
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
async fn incoming(
    socket: &mut Socket,
    budgets: &Budgets,
    observer: Option<&ServerDiagnostics>,
    record_request: bool,
) -> Result<(Incoming, Admission, Span), Failure> {
    // Waiting for the next header is idle socket traffic, not an application
    // request. Start both spans only once its first binary message arrives.
    let bytes = receive(socket).await.map_err(|error| Failure {
        error,
        outcome: Outcome::Error,
    })?;
    let mut request = Span::new(
        if record_request { observer } else { None },
        DiagnosticOperation::Request,
    );
    let mut reading = Span::new(observer, DiagnosticOperation::IncomingRead);
    let mut outcome = Outcome::Error;
    let result = async {
        let header = Header::decode(
            bytes
                .as_ref()
                .try_into()
                .map_err(|_| "invalid header message")?,
        )?;
        let mut ingress = Span::new(observer, DiagnosticOperation::IngressAdmission);
        let admitted = budgets
            .ingress(header)
            .and_then(|admission| budgets.websocket_wire(header).map(|wire| (admission, wire)));
        ingress.finish(Outcome::result(&admitted));
        let (admission, _wire_permit) = admitted?;
        // Charge before even the library allocates its first body message. The
        // fixed <=32KiB pre-header window is bounded separately by connections.
        let length = header.control_len + header.payload_len;
        let mut bytes = Vec::with_capacity(length);
        let body = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
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
        .await;
        match body {
            Ok(result) => result?,
            Err(error) => {
                outcome = Outcome::Timeout;
                return Err(error.into());
            }
        }
        let mut cursor = bytes.as_slice();
        let incoming = binary::read_body(&mut cursor, header).await?;
        if !cursor.is_empty() {
            return Err("trailing body bytes".into());
        }
        outcome = Outcome::Success;
        Ok::<_, Error>((incoming, admission))
    }
    .await;
    reading.finish(outcome);
    match result {
        Ok((incoming, admission)) => Ok((incoming, admission, request)),
        Err(error) => {
            request.finish(outcome);
            Err(Failure { error, outcome })
        }
    }
}
async fn send(
    socket: &mut Socket,
    bytes: &[u8],
    observer: Option<&ServerDiagnostics>,
) -> Result<(), Failure> {
    let mut submit = Span::new(observer, DiagnosticOperation::ResponseSubmit);
    let result = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
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
    .await;
    let outcome = match &result {
        Ok(result) => Outcome::result(result),
        Err(_) => Outcome::Timeout,
    };
    submit.finish(outcome);
    match result {
        Ok(result) => result.map_err(|error| Failure { error, outcome }),
        Err(error) => Err(Failure {
            error: error.into(),
            outcome,
        }),
    }
}
async fn serve(
    mut socket: Socket,
    dispatcher: Arc<DriveDispatcher>,
    authenticator: Arc<dyn Authenticator>,
    budgets: Arc<Budgets>,
    mut stopping: watch::Receiver<bool>,
    observer: Option<&ServerDiagnostics>,
    mut handshake: Span,
) {
    let handles = SessionHandles::default();
    // Cancellation drops in-flight RPCs (including uncertain committed writes),
    // then cleanup always executes outside that cancelled future.
    let result = tokio::select! {
        biased;
        _ = stopping.changed() => None,
        result = session(&mut socket, &dispatcher, &*authenticator, &budgets, &handles, observer, &mut handshake) => Some(result),
    };
    handshake.finish(match result {
        Some(result) => Outcome::result(&result),
        None => Outcome::Cancelled,
    });
    drop(socket);
    let mut cleanup = Span::new(observer, DiagnosticOperation::SessionCleanup);
    handles.close_all().await;
    cleanup.finish(Outcome::Success);
}
async fn session(
    socket: &mut Socket,
    dispatcher: &DriveDispatcher,
    authenticator: &dyn Authenticator,
    budgets: &Budgets,
    handles: &SessionHandles,
    observer: Option<&ServerDiagnostics>,
    handshake: &mut Span,
) -> Result<(), Error> {
    let (hello, admission, _request) = match tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        incoming(socket, budgets, observer, false),
    )
    .await
    {
        Ok(Ok(received)) => received,
        Ok(Err(failure)) => {
            handshake.finish(failure.outcome);
            return Err(failure.error);
        }
        Err(error) => {
            handshake.finish(Outcome::Timeout);
            return Err(error.into());
        }
    };
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
    let mut identity = match authenticate(
        authenticator,
        &bearer,
        &partition_id,
        observer,
        Some(OPERATION_TIMEOUT),
    )
    .await
    {
        Ok(identity) => identity,
        Err(failure) => {
            handshake.finish(failure.outcome);
            return Err(failure.error);
        }
    };
    if identity.partition_id != partition_id {
        return Err("partition mismatch".into());
    }
    let hello = Message::ServerHello {
        version: PROTOCOL_VERSION,
        session_id: crate::server::session_id(),
    };
    let reservation = ResponseReservation::message(&hello);
    let mut egress = Span::new(observer, DiagnosticOperation::EgressAdmission);
    let permit = budgets.egress(reservation.control, reservation.charge);
    egress.finish(Outcome::result(&permit));
    let permit = permit?;
    let mut output = ResponseBuffer::new(reservation.wire_limit);
    observed(
        observer,
        DiagnosticOperation::ResponseEncode,
        binary::write_control(&mut output, &hello),
    )
    .await?;
    if let Err(failure) = send(socket, &output.bytes, observer).await {
        handshake.finish(failure.outcome);
        return Err(failure.error);
    }
    drop((permit, admission));
    handshake.finish(Outcome::Success);
    loop {
        let (request, mut admission, mut request_span) = incoming(socket, budgets, observer, true)
            .await
            .map_err(|failure| failure.error)?;
        let mut outcome = Outcome::Error;
        let result = async {
            if handles.is_closed().await {
                return Err("session invalidated".into());
            }
            if matches!(request, Incoming::Control(Message::Request { .. })) {
                let mut ingress = Span::new(observer, DiagnosticOperation::IngressAdmission);
                let admitted = budgets.request_operation(&mut admission);
                ingress.finish(Outcome::result(&admitted));
                admitted?;
            }
            let reservation = match &request {
                Incoming::Control(message) => ResponseReservation::message(message),
                Incoming::Read { length, .. } => ResponseReservation::io(*length),
                Incoming::Write { .. } => ResponseReservation::io(0),
            };
            let mut egress = Span::new(observer, DiagnosticOperation::EgressAdmission);
            let permit = budgets.egress(reservation.control, reservation.charge);
            egress.finish(Outcome::result(&permit));
            let permit = permit?;
            let mut output = ResponseBuffer::new(reservation.wire_limit);
            let dispatched = match tokio::time::timeout(
                OPERATION_TIMEOUT,
                dispatch(
                    request,
                    &mut identity,
                    dispatcher,
                    authenticator,
                    handles,
                    &mut output,
                    observer,
                ),
            )
            .await
            {
                Ok(result) => result?,
                Err(error) => {
                    outcome = Outcome::Timeout;
                    return Err(error.into());
                }
            };
            if let Err(failure) = send(socket, &output.bytes, observer).await {
                outcome = failure.outcome;
                return Err(failure.error);
            }
            drop((permit, admission));
            outcome = dispatched;
            Ok::<(), Error>(())
        }
        .await;
        request_span.finish(outcome);
        result?;
    }
}
async fn dispatch(
    request: Incoming,
    identity: &mut SessionIdentity,
    dispatcher: &DriveDispatcher,
    authenticator: &dyn Authenticator,
    handles: &SessionHandles,
    output: &mut ResponseBuffer,
    observer: Option<&ServerDiagnostics>,
) -> Result<Outcome, Error> {
    let outcome = match request {
        Incoming::Control(Message::Request {
            request_id,
            drive_id,
            operation,
        }) if request_id != 0 => {
            let result = observed(
                observer,
                DiagnosticOperation::DispatchControl,
                dispatcher.dispatch_request(identity, &drive_id, &operation, handles, request_id),
            )
            .await;
            let outcome = Outcome::result(&result);
            observed(
                observer,
                DiagnosticOperation::ResponseEncode,
                binary::write_control(output, &Message::Response { request_id, result }),
            )
            .await?;
            outcome
        }
        Incoming::Control(Message::Renew { bearer }) => {
            let next = authenticate(
                authenticator,
                &bearer,
                &identity.partition_id,
                observer,
                None,
            )
            .await
            .map_err(|failure| failure.error)?;
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
            observed(
                observer,
                DiagnosticOperation::ResponseEncode,
                binary::write_control(
                    output,
                    &Message::ServerHello {
                        version: PROTOCOL_VERSION,
                        session_id: "renewed".into(),
                    },
                ),
            )
            .await?;
            Outcome::Success
        }
        Incoming::Read {
            request_id,
            request,
            length,
        } => {
            let result = observed(
                observer,
                DiagnosticOperation::DispatchRead,
                dispatcher.dispatch_io(identity, &request, handles, request_id, length, None),
            )
            .await;
            let outcome = Outcome::result(&result);
            observed(
                observer,
                DiagnosticOperation::ResponseEncode,
                binary::write_result(output, request_id, result),
            )
            .await?;
            outcome
        }
        Incoming::Write {
            request_id,
            request,
            data,
        } => {
            let result = observed(
                observer,
                DiagnosticOperation::DispatchWrite,
                dispatcher.dispatch_io(identity, &request, handles, request_id, 0, Some(&data)),
            )
            .await;
            let outcome = Outcome::result(&result);
            observed(
                observer,
                DiagnosticOperation::ResponseEncode,
                binary::write_result(output, request_id, result),
            )
            .await?;
            outcome
        }
        _ => return Err("invalid request".into()),
    };
    Ok(outcome)
}
