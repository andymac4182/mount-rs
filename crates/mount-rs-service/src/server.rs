//! TLS-authenticated QUIC sessions for one Partition per connection.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use mount_rs_remote_protocol::binary::{self, Incoming, InlineDriveId, IoRequest};
use mount_rs_remote_protocol::{Message, PROTOCOL_VERSION, WireError};
use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::{Mutex, Semaphore};

use crate::dispatch::{DriveDispatcher, SessionHandles, SessionIdentity};
pub use crate::transfer::RemoteTransferLimits;
use crate::transfer::{Admission, Budgets, ResponseBuffer, ResponseReservation, charged_bytes};

mod diagnostics;
use diagnostics::{Operation as DiagnosticOperation, Outcome, Span};
pub use diagnostics::{ServerDiagnostics, ServerSnapshot};

const OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const ALPN: &[u8] = b"mount-rs/2";
const MAX_DATA_STREAMS: usize = 32;
// Quinn batches MAX_STREAMS updates after more than 1/8 of the window is
// freed. Eight extra credits leave control room even with five pending credits.
const TRANSPORT_STREAMS: u32 = 40;

#[derive(Clone, Copy, Debug)]
pub struct RemoteServerOptions {
    pub max_connections: usize,
}

impl Default for RemoteServerOptions {
    fn default() -> Self {
        Self {
            max_connections: 128,
        }
    }
}

impl RemoteServerOptions {
    pub fn validate(self) -> Result<(), &'static str> {
        if !(1..=16_384).contains(&self.max_connections) {
            return Err("max_connections must be in 1..=16384");
        }
        Ok(())
    }
}

#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(&self, token: &str, partition_id: &str) -> Result<SessionIdentity, ()>;
}

pub struct RemoteServer {
    endpoint: quinn::Endpoint,
    task: tokio::task::JoinHandle<()>,
    diagnostics: Option<ServerDiagnostics>,
}

impl RemoteServer {
    /// Clone the configured local observer, if available.
    #[must_use]
    pub fn diagnostics(&self) -> Option<ServerDiagnostics> {
        self.diagnostics.clone()
    }

    pub async fn bind(
        address: SocketAddr,
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        dispatcher: Arc<DriveDispatcher>,
        authenticator: Arc<dyn Authenticator>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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

    /// Bind with server-wide independent ingress/egress transfer capacities.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_with_transfer_limits(
        address: SocketAddr,
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        dispatcher: Arc<DriveDispatcher>,
        authenticator: Arc<dyn Authenticator>,
        options: RemoteServerOptions,
        limits: RemoteTransferLimits,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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

    /// Bind with a local observer when explicitly enabled in an io-profiling
    /// build. Ordinary constructors remain disabled, including when PROFILE_IO
    /// is set. Slow fixed-label records additionally require TRACE_SERVICE=1.
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
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        limits.validate()?;
        let budgets = Arc::new(Budgets::new(limits));
        options.validate()?;
        let diagnostics = (diagnostics_enabled && cfg!(feature = "io-profiling")).then(|| {
            ServerDiagnostics::new(
                options.max_connections,
                std::env::var_os("MOUNT_RS_TRACE_SERVICE").is_some_and(|value| value == "1"),
            )
        });
        let task_diagnostics = diagnostics.clone();
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut server_config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
        let transport = Arc::get_mut(&mut server_config.transport).expect("new transport config");
        // Reserve a stream for small control RPCs. Admission never queues
        // behind data permits; global byte and operation limits are separate.
        transport.stream_receive_window((256 * 1024_u32).into());
        transport.receive_window((2 * 1024 * 1024_u32).into());
        transport.send_window(2 * 1024 * 1024);
        transport.max_concurrent_bidi_streams(TRANSPORT_STREAMS.into());
        transport.max_concurrent_uni_streams(0_u32.into());
        transport.max_idle_timeout(Some(std::time::Duration::from_secs(60).try_into()?));
        transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));
        let endpoint = quinn::Endpoint::server(server_config, address)?;
        let accept_endpoint = endpoint.clone();
        let connections = Arc::new(Semaphore::new(options.max_connections));
        let task = tokio::spawn(async move {
            let mut sessions = tokio::task::JoinSet::new();
            while let Some(incoming) = accept_endpoint.accept().await {
                let mut admission = Span::new(
                    task_diagnostics.as_ref(),
                    DiagnosticOperation::ConnectionAdmission,
                );
                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                    admission.finish(Outcome::Error);
                    incoming.refuse();
                    continue;
                };
                admission.finish(Outcome::Success);
                let dispatcher = Arc::clone(&dispatcher);
                let authenticator = Arc::clone(&authenticator);
                let budgets = Arc::clone(&budgets);
                let diagnostics = task_diagnostics.clone();
                let mut handshake = Span::new(diagnostics.as_ref(), DiagnosticOperation::Handshake);
                while sessions.try_join_next().is_some() {}
                sessions.spawn(async move {
                    let _permit = permit;
                    let mut tls =
                        Span::new(diagnostics.as_ref(), DiagnosticOperation::TlsHandshake);
                    match tokio::time::timeout(std::time::Duration::from_secs(10), incoming).await {
                        Ok(Ok(connection)) => {
                            tls.finish(Outcome::Success);
                            let _registration =
                                diagnostics.as_ref().map(|d| d.register(&connection));
                            let outcome = serve_connection(
                                connection,
                                dispatcher,
                                authenticator,
                                budgets,
                                diagnostics,
                                &mut handshake,
                            )
                            .await;
                            handshake.finish(outcome);
                        }
                        Ok(Err(_)) => {
                            tls.finish(Outcome::Error);
                            handshake.finish(Outcome::Error);
                        }
                        Err(_) => {
                            tls.finish(Outcome::Timeout);
                            handshake.finish(Outcome::Timeout);
                        }
                    }
                });
            }
            while sessions.join_next().await.is_some() {}
        });
        Ok(Self {
            endpoint,
            task,
            diagnostics,
        })
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.endpoint.local_addr().expect("bound QUIC endpoint")
    }

    pub async fn close(self) {
        self.endpoint.close(0_u32.into(), b"server shutdown");
        let _ = self.task.await;
        self.endpoint.wait_idle().await;
    }
}

async fn serve_connection(
    connection: quinn::Connection,
    dispatcher: Arc<DriveDispatcher>,
    authenticator: Arc<dyn Authenticator>,
    budgets: Arc<Budgets>,
    diagnostics: Option<ServerDiagnostics>,
    handshake_span: &mut Span,
) -> Outcome {
    let connection_operations = Arc::new(Semaphore::new(MAX_DATA_STREAMS));
    let Some(handshake) = connection.handshake_data() else {
        return Outcome::Error;
    };
    let Ok(handshake) = handshake.downcast::<quinn::crypto::rustls::HandshakeData>() else {
        return Outcome::Error;
    };
    if handshake.protocol.as_deref() != Some(ALPN) {
        return Outcome::Error;
    }
    let accepted =
        tokio::time::timeout(std::time::Duration::from_secs(10), connection.accept_bi()).await;
    let failure = deadline_failure(&accepted);
    let Ok(Ok((mut send, mut recv))) = accepted else {
        return failure;
    };
    let hello_result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        read_incoming(
            &mut recv,
            &budgets,
            &connection_operations,
            diagnostics.as_ref(),
        ),
    )
    .await;
    let failure = deadline_failure(&hello_result);
    let Ok(Ok((
        Incoming::Control(Message::ClientHello {
            version,
            partition_id,
            bearer,
        }),
        _hello_admission,
    ))) = hello_result
    else {
        connection.close(1_u32.into(), b"invalid handshake");
        return failure;
    };
    if version != PROTOCOL_VERSION {
        connection.close(1_u32.into(), b"unsupported protocol");
        return Outcome::Error;
    }
    let mut authentication = Span::new(diagnostics.as_ref(), DiagnosticOperation::Authentication);
    let authentication_result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        authenticator.authenticate(&bearer, &partition_id),
    )
    .await;
    authentication.finish(match &authentication_result {
        Ok(result) => Outcome::result(result),
        Err(_) => Outcome::Timeout,
    });
    let failure = deadline_failure(&authentication_result);
    let Ok(Ok(identity)) = authentication_result else {
        eprintln!("remote authentication denied");
        connection.close(1_u32.into(), b"authentication failed");
        return failure;
    };
    if identity.partition_id != partition_id {
        connection.close(1_u32.into(), b"authentication failed");
        return Outcome::Error;
    }
    // A hello occupies one complete stream: consume FIN so transport credit
    // is returned, and reject any bytes after the framed message.
    let finished =
        tokio::time::timeout(std::time::Duration::from_secs(10), recv.read_to_end(0)).await;
    if !matches!(finished, Ok(Ok(_))) {
        connection.close(1_u32.into(), b"invalid handshake stream");
        return deadline_failure(&finished);
    }
    let response = Message::ServerHello {
        version: PROTOCOL_VERSION,
        session_id: session_id(),
    };
    let reservation = ResponseReservation::message(&response);
    let mut egress = Span::new(diagnostics.as_ref(), DiagnosticOperation::EgressAdmission);
    let permit = budgets.egress(reservation.control, reservation.charge);
    egress.finish(Outcome::result(&permit));
    let Ok(permit) = permit else {
        return Outcome::Error;
    };
    let mut buffer = ResponseBuffer::new(reservation.wire_limit);
    let mut encoding = Span::new(diagnostics.as_ref(), DiagnosticOperation::ResponseEncode);
    let encoded = binary::write_control(&mut buffer, &response).await;
    encoding.finish(Outcome::result(&encoded));
    if encoded.is_err() {
        return Outcome::Error;
    }
    let mut submission = Span::new(diagnostics.as_ref(), DiagnosticOperation::ResponseSubmit);
    let submitted = send.write_chunk(charged_bytes(buffer.bytes, permit)).await;
    submission.finish(Outcome::result(&submitted));
    if submitted.is_err() {
        return Outcome::Error;
    }
    drop(_hello_admission);
    // Release the hello stream's transport credit before admitting requests.
    let _ = send.finish();
    drop(send);
    drop(recv);
    handshake_span.finish(Outcome::Success);
    let identity = Arc::new(Mutex::new(Arc::new(identity)));
    let handles = Arc::new(SessionHandles::default());
    let mut tasks = tokio::task::JoinSet::new();
    while let Ok((send, recv)) = connection.accept_bi().await {
        let identity = Arc::clone(&identity);
        let dispatcher = Arc::clone(&dispatcher);
        let authenticator = Arc::clone(&authenticator);
        let handles = Arc::clone(&handles);
        let stream_connection = connection.clone();
        let budgets = Arc::clone(&budgets);
        let connection_operations = Arc::clone(&connection_operations);
        let diagnostics = diagnostics.clone();
        let mut request_span = Span::new(diagnostics.as_ref(), DiagnosticOperation::Request);
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            let outcome = serve_stream(
                send,
                recv,
                identity,
                dispatcher,
                authenticator,
                handles,
                stream_connection,
                budgets,
                connection_operations,
                diagnostics,
            )
            .await;
            request_span.finish(outcome);
        });
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    let mut cleanup = Span::new(diagnostics.as_ref(), DiagnosticOperation::SessionCleanup);
    handles.close_all().await;
    cleanup.finish(Outcome::Success);
    Outcome::Success
}

#[allow(clippy::too_many_arguments)]
async fn serve_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    identity: Arc<Mutex<Arc<SessionIdentity>>>,
    dispatcher: Arc<DriveDispatcher>,
    authenticator: Arc<dyn Authenticator>,
    handles: Arc<SessionHandles>,
    connection: quinn::Connection,
    budgets: Arc<Budgets>,
    connection_operations: Arc<Semaphore>,
    diagnostics: Option<ServerDiagnostics>,
) -> Outcome {
    let incoming_result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        read_incoming(
            &mut recv,
            &budgets,
            &connection_operations,
            diagnostics.as_ref(),
        ),
    )
    .await;
    let (incoming, mut admission) = match incoming_result {
        Ok(Ok(incoming)) => incoming,
        Ok(Err(_)) => return Outcome::Error,
        Err(_) => return Outcome::Timeout,
    };
    // Admit one complete RPC before any mutating operation can start.
    let finished =
        tokio::time::timeout(std::time::Duration::from_secs(10), recv.read_to_end(0)).await;
    if !matches!(finished, Ok(Ok(_))) {
        connection.close(1_u32.into(), b"invalid request stream");
        return deadline_failure(&finished);
    }
    let message = match incoming {
        Incoming::Control(message) => message,
        Incoming::Read {
            request_id,
            request,
            length,
        } => {
            return serve_io(
                &mut send,
                &connection,
                &identity,
                &dispatcher,
                &handles,
                &budgets,
                request_id,
                &request,
                length,
                None,
                diagnostics.as_ref(),
            )
            .await;
        }
        Incoming::Write {
            request_id,
            request,
            data,
        } => {
            return serve_io(
                &mut send,
                &connection,
                &identity,
                &dispatcher,
                &handles,
                &budgets,
                request_id,
                &request,
                0,
                Some(&data),
                diagnostics.as_ref(),
            )
            .await;
        }
    };
    if matches!(message, Message::Request { .. }) {
        let mut ingress = Span::new(diagnostics.as_ref(), DiagnosticOperation::IngressAdmission);
        let admitted = budgets.request_operation(&mut admission).and_then(|()| {
            budgets.connection_data_operation(&mut admission, &connection_operations)
        });
        ingress.finish(Outcome::result(&admitted));
        if admitted.is_err() {
            return Outcome::Error;
        }
    }
    // Reserve output before reads or authoritative mutations. Raw reads use
    // their validated declared size; fixed scalar metadata has a small cap.
    let reservation = ResponseReservation::message(&message);
    let mut egress = Span::new(diagnostics.as_ref(), DiagnosticOperation::EgressAdmission);
    let egress_result = budgets.egress(reservation.control, reservation.charge);
    egress.finish(Outcome::result(&egress_result));
    let Ok(egress_permit) = egress_result else {
        return Outcome::Error;
    };
    if handles.is_closed().await {
        connection.close(1_u32.into(), b"session invalidated");
        return Outcome::Error;
    }
    let mut response_outcome = Outcome::Success;
    let response = match message {
        Message::Request {
            request_id,
            drive_id,
            operation,
        } => {
            if request_id == 0 {
                connection.close(1_u32.into(), b"invalid request id");
                return Outcome::Error;
            }
            let current = identity.lock().await.clone();
            let mut dispatch =
                Span::new(diagnostics.as_ref(), DiagnosticOperation::DispatchControl);
            let result = match tokio::time::timeout(OPERATION_TIMEOUT, async {
                dispatcher
                    .dispatch_request(&current, &drive_id, &operation, &handles, request_id)
                    .await
            })
            .await
            {
                Ok(result) => {
                    dispatch.finish(Outcome::result(&result));
                    result
                }
                Err(_) => {
                    dispatch.finish(Outcome::Timeout);
                    eprintln!(
                        "{}",
                        serde_json::json!({"event":"remote_denial","request_id":request_id,"outcome":"timeout"})
                    );
                    connection.close(1_u32.into(), b"operation timed out");
                    return Outcome::Timeout;
                }
            };
            if let Err(error) = &result {
                response_outcome = Outcome::Error;
                eprintln!(
                    "{}",
                    serde_json::json!({"event":"remote_denial","partition_id":current.partition_id,"drive_id":drive_id,"policy_id":current.policy_id,"operation":operation.name,"request_id":request_id,"outcome":error.code})
                );
            }
            Message::Response { request_id, result }
        }
        Message::Renew { bearer } => {
            let partition = identity.lock().await.partition_id.clone();
            let mut authentication =
                Span::new(diagnostics.as_ref(), DiagnosticOperation::Authentication);
            let authentication_result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                authenticator.authenticate(&bearer, &partition),
            )
            .await;
            authentication.finish(match &authentication_result {
                Ok(result) => Outcome::result(result),
                Err(_) => Outcome::Timeout,
            });
            let failure = deadline_failure(&authentication_result);
            let Ok(Ok(next)) = authentication_result else {
                Arc::make_mut(&mut *identity.lock().await).expires_at = 0;
                connection.close(1_u32.into(), b"authentication failed");
                return failure;
            };
            let mut current = identity.lock().await;

            if next.partition_id != current.partition_id
                || next.policy_id != current.policy_id
                || next.issuer != current.issuer
                || next.subject != current.subject
                || !dispatcher.renewal_matches(&current, &next).await
            {
                Arc::make_mut(&mut *current).expires_at = 0;
                connection.close(1_u32.into(), b"identity changed");
                response_outcome = Outcome::Error;
                denied()
            } else {
                if handles.is_closed().await {
                    connection.close(1_u32.into(), b"session invalidated");
                    return Outcome::Error;
                }
                *current = Arc::new(next);
                Message::ServerHello {
                    version: PROTOCOL_VERSION,
                    session_id: "renewed".into(),
                }
            }
        }
        _ => {
            response_outcome = Outcome::Error;
            denied()
        }
    };
    let mut buffer = ResponseBuffer::new(reservation.wire_limit);
    let mut encoding = Span::new(diagnostics.as_ref(), DiagnosticOperation::ResponseEncode);
    let encoded = binary::write_control(&mut buffer, &response).await;
    encoding.finish(Outcome::result(&encoded));
    if encoded.is_err() {
        return Outcome::Error;
    }
    let payload = charged_bytes(buffer.bytes, egress_permit);
    let mut submission = Span::new(diagnostics.as_ref(), DiagnosticOperation::ResponseSubmit);
    let submitted = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        send.write_chunk(payload),
    )
    .await;
    let finished = send.finish();
    let submit_outcome = match submitted {
        Ok(Ok(())) if finished.is_ok() => Outcome::Success,
        Ok(_) => Outcome::Error,
        Err(_) => Outcome::Timeout,
    };
    submission.finish(submit_outcome);
    if matches!(submit_outcome, Outcome::Success) {
        response_outcome
    } else {
        submit_outcome
    }
}

// The typed lane retains inline/borrowed metadata through authoritative dispatch.
#[allow(clippy::too_many_arguments)]
async fn serve_io(
    send: &mut quinn::SendStream,
    connection: &quinn::Connection,
    identity: &Mutex<Arc<SessionIdentity>>,
    dispatcher: &DriveDispatcher,
    handles: &SessionHandles,
    budgets: &Budgets,
    request_id: u64,
    request: &IoRequest<InlineDriveId>,
    length: usize,
    data: Option<&[u8]>,
    diagnostics: Option<&ServerDiagnostics>,
) -> Outcome {
    let reservation = ResponseReservation::io(length);
    let mut egress = Span::new(diagnostics, DiagnosticOperation::EgressAdmission);
    let permit = budgets.egress(reservation.control, reservation.charge);
    egress.finish(Outcome::result(&permit));
    let Ok(permit) = permit else {
        return Outcome::Error;
    };
    if handles.is_closed().await {
        connection.close(1_u32.into(), b"session invalidated");
        return Outcome::Error;
    }
    let current = Arc::clone(&*identity.lock().await);
    let mut dispatch = Span::new(
        diagnostics,
        if data.is_some() {
            DiagnosticOperation::DispatchWrite
        } else {
            DiagnosticOperation::DispatchRead
        },
    );
    let result = match tokio::time::timeout(
        OPERATION_TIMEOUT,
        dispatcher.dispatch_io(&current, request, handles, request_id, length, data),
    )
    .await
    {
        Ok(result) => {
            dispatch.finish(Outcome::result(&result));
            result
        }
        Err(_) => {
            dispatch.finish(Outcome::Timeout);
            eprintln!(
                "{}",
                serde_json::json!({"event":"remote_denial","request_id":request_id,"outcome":"timeout"})
            );
            connection.close(1_u32.into(), b"operation timed out");
            return Outcome::Timeout;
        }
    };
    let response_outcome = Outcome::result(&result);
    if let Err(error) = &result {
        eprintln!(
            "{}",
            serde_json::json!({"event":"remote_denial","partition_id":current.partition_id,"drive_id":request.drive_id.as_ref(),"policy_id":current.policy_id,"operation":if data.is_some(){"handle_write"}else{"handle_read"},"request_id":request_id,"outcome":error.code})
        );
    }
    let mut buffer = ResponseBuffer::new(reservation.wire_limit);
    let mut encoding = Span::new(diagnostics, DiagnosticOperation::ResponseEncode);
    let encoded = binary::write_result(&mut buffer, request_id, result).await;
    encoding.finish(Outcome::result(&encoded));
    if encoded.is_err() {
        return Outcome::Error;
    }
    let payload = charged_bytes(buffer.bytes, permit);
    let mut submission = Span::new(diagnostics, DiagnosticOperation::ResponseSubmit);
    let submitted = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        send.write_chunk(payload),
    )
    .await;
    let finished = send.finish();
    let submit_outcome = match submitted {
        Ok(Ok(())) if finished.is_ok() => Outcome::Success,
        Ok(_) => Outcome::Error,
        Err(_) => Outcome::Timeout,
    };
    submission.finish(submit_outcome);
    if matches!(submit_outcome, Outcome::Success) {
        response_outcome
    } else {
        submit_outcome
    }
}

async fn read_incoming(
    recv: &mut quinn::RecvStream,
    budgets: &Budgets,
    connection_operations: &Arc<Semaphore>,
    diagnostics: Option<&ServerDiagnostics>,
) -> Result<(Incoming, Admission), mount_rs_remote_protocol::FrameError> {
    let mut reading = Span::new(diagnostics, DiagnosticOperation::IncomingRead);
    let result = async {
        let header = binary::Header::read(recv).await?;
        let mut ingress = Span::new(diagnostics, DiagnosticOperation::IngressAdmission);
        let admitted = (|| {
            let mut admission = budgets.ingress(header)?;
            budgets.connection_data_operation(&mut admission, connection_operations)?;
            Ok::<_, mount_rs_remote_protocol::FrameError>(admission)
        })();
        ingress.finish(Outcome::result(&admitted));
        let admission = admitted?;
        let incoming = binary::read_body(recv, header).await?;
        Ok((incoming, admission))
    }
    .await;
    reading.finish(Outcome::result(&result));
    result
}

fn denied() -> Message {
    Message::Response {
        request_id: 0,
        result: Err(WireError {
            code: "EACCES".into(),
        }),
    }
}

// Called only when a deadline result is rejected. Preserve inner errors and
// malformed successful envelopes as errors; elapsed outer deadlines are timeouts.
fn deadline_failure<T>(result: &Result<T, tokio::time::error::Elapsed>) -> Outcome {
    if result.is_err() {
        Outcome::Timeout
    } else {
        Outcome::Error
    }
}

pub(crate) fn session_id() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0_u8; 16];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        return String::new();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{Outcome, RemoteServerOptions, deadline_failure};

    #[tokio::test]
    async fn deadline_failure_distinguishes_elapsed_from_inner_error() {
        let elapsed =
            tokio::time::timeout(std::time::Duration::ZERO, std::future::pending::<()>()).await;
        assert!(matches!(deadline_failure(&elapsed), Outcome::Timeout));
        assert!(matches!(
            deadline_failure(&Ok::<_, tokio::time::error::Elapsed>(Err::<(), ()>(()))),
            Outcome::Error
        ));
    }

    #[test]
    fn connection_admission_options_preserve_default_and_bound_capacity() {
        assert_eq!(RemoteServerOptions::default().max_connections, 128);
        assert!(
            RemoteServerOptions {
                max_connections: 1_024
            }
            .validate()
            .is_ok()
        );
        assert!(
            RemoteServerOptions { max_connections: 0 }
                .validate()
                .is_err()
        );
        assert!(
            RemoteServerOptions {
                max_connections: 16_385
            }
            .validate()
            .is_err()
        );
    }
}
