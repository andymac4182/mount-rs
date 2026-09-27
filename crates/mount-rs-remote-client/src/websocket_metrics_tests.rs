//! Isolated real TLS WebSocket stage and serialized transaction qualification.
use super::*;
use mount_rs_core::diagnostics::storage::{self, Operation as StorageOperation};
use mount_rs_remote_protocol::{
    Operation, OperationName, WireError,
    binary::{Incoming, IoResult},
};
use std::{
    future::{Future, poll_fn},
    task::Poll,
    time::Duration,
};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request as UpgradeRequest, Response as UpgradeResponse,
};

type ServerSocket = WebSocketStream<tokio_rustls::server::TlsStream<TcpStream>>;
type Outcomes = [(u64, u64, u64); 8];
const ZERO: (u64, u64, u64) = (0, 0, 0);
const SUCCESS: (u64, u64, u64) = (1, 0, 0);
const ERROR: (u64, u64, u64) = (0, 1, 0);
const CANCELLED: (u64, u64, u64) = (0, 0, 1);
const OPERATIONS: [StorageOperation; 8] = [
    StorageOperation::RemoteClientWebSocketTcpConnect,
    StorageOperation::RemoteClientWebSocketTlsHandshake,
    StorageOperation::RemoteClientWebSocketUpgrade,
    StorageOperation::RemoteClientWebSocketSocketLockWait,
    StorageOperation::RemoteClientWebSocketRequestEncode,
    StorageOperation::RemoteClientWebSocketRequestSend,
    StorageOperation::RemoteClientWebSocketResponseReceive,
    StorageOperation::RemoteClientWebSocketResponseDecode,
];

async fn listener() -> (
    TcpListener,
    tokio_rustls::TlsAcceptor,
    rustls::RootCertStore,
) {
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = certificate.cert.der().clone();
    let key =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.clone()], key)
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    (
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        tokio_rustls::TlsAcceptor::from(Arc::new(tls)),
        roots,
    )
}

// Tungstenite's callback requires the concrete HTTP error response.
#[allow(clippy::result_large_err)]
fn upgrade(
    request: &UpgradeRequest,
    mut response: UpgradeResponse,
) -> Result<UpgradeResponse, ErrorResponse> {
    assert_eq!(
        request.headers().get("Sec-WebSocket-Protocol").unwrap(),
        SUBPROTOCOL
    );
    response
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", SUBPROTOCOL.parse().unwrap());
    Ok(response)
}

async fn pair() -> (Arc<dyn Transport>, ServerSocket) {
    let (listener, acceptor, roots) = listener().await;
    let (client, server) = tokio::join!(
        WebSocketTransport::connect(listener.local_addr().unwrap(), "localhost", roots),
        async {
            let (tcp, _) = listener.accept().await.unwrap();
            tcp.set_nodelay(true).unwrap();
            let tls = acceptor.accept(tcp).await.unwrap();
            tokio_tungstenite::accept_hdr_async_with_config(tls, upgrade, Some(config()))
                .await
                .unwrap()
        }
    );
    (client.unwrap(), server)
}

fn message() -> Message {
    Message::Request {
        request_id: 7,
        drive_id: "private_metrics_drive".into(),
        operation: Operation {
            name: OperationName::Stat,
            body: serde_json::json!({"path":"/private_metrics_path"}),
        },
    }
}

fn response() -> Message {
    Message::Response {
        request_id: 7,
        result: Ok(serde_json::json!({"complete":true})),
    }
}

async fn control_bytes(response: &Message) -> Vec<u8> {
    let mut bytes = Vec::new();
    binary::write_control(&mut bytes, response).await.unwrap();
    bytes
}

async fn result_bytes(id: u64, result: Result<IoResult, WireError>) -> Vec<u8> {
    let mut bytes = Vec::new();
    binary::write_result(&mut bytes, id, result).await.unwrap();
    bytes
}

async fn receive_envelope(socket: &mut ServerSocket) -> Incoming {
    let first = socket.next().await.unwrap().unwrap();
    let WsMessage::Binary(first) = first else {
        panic!("expected request header");
    };
    let header = Header::decode(first.as_ref().try_into().unwrap()).unwrap();
    let length = header.control_len + header.payload_len;
    let mut body = Vec::with_capacity(length);
    while body.len() < length {
        let WsMessage::Binary(chunk) = socket.next().await.unwrap().unwrap() else {
            panic!("expected request body");
        };
        assert!(!chunk.is_empty());
        assert!(chunk.len() <= CHUNK_BYTES);
        assert!(chunk.len() <= length - body.len());
        body.extend_from_slice(&chunk);
    }
    assert!(
        matches!(socket.next().await.unwrap().unwrap(), WsMessage::Binary(end) if end.is_empty())
    );
    let mut cursor = body.as_slice();
    let request = binary::read_body(&mut cursor, header).await.unwrap();
    assert!(cursor.is_empty());
    request
}

async fn send_envelope(socket: &mut ServerSocket, bytes: &[u8]) {
    socket
        .send(WsMessage::Binary(
            bytes[..binary::HEADER_BYTES].to_vec().into(),
        ))
        .await
        .unwrap();
    for chunk in bytes[binary::HEADER_BYTES..].chunks(CHUNK_BYTES) {
        socket
            .send(WsMessage::Binary(chunk.to_vec().into()))
            .await
            .unwrap();
    }
    socket
        .send(WsMessage::Binary(Vec::new().into()))
        .await
        .unwrap();
}

async fn pending(future: &mut (impl Future + Unpin)) {
    poll_fn(|cx| {
        assert!(
            std::pin::Pin::new(&mut *future).poll(cx).is_pending(),
            "the held peer boundary must keep the caller pending"
        );
        Poll::Ready(())
    })
    .await;
}

fn gauge(expected: [u64; 8]) {
    let snapshot = storage::snapshot();
    assert_eq!(snapshot.in_flight, expected.iter().sum::<u64>());
    for (operation, expected) in OPERATIONS.into_iter().zip(expected) {
        assert_eq!(snapshot.entries[operation as usize].in_flight, expected);
    }
}

fn phase(before: &storage::Snapshot, expected: Outcomes) -> storage::Snapshot {
    let delta = storage::snapshot().delta(before).unwrap();
    assert_eq!(delta.in_flight, 0);
    assert_eq!(delta.entries.len(), 108);
    for (operation, expected) in OPERATIONS.into_iter().zip(expected) {
        let row = &delta.entries[operation as usize];
        assert_eq!(
            (row.success, row.error, row.cancelled),
            expected,
            "{}",
            row.name
        );
        assert_eq!(
            row.calls,
            expected.0 + expected.1 + expected.2,
            "{}",
            row.name
        );
        assert_eq!(row.latency_log2_us.iter().sum::<u64>(), row.calls);
        assert_eq!(
            (
                row.in_flight,
                row.bytes,
                row.returned_rows,
                row.returned_row_observations
            ),
            (0, 0, 0, 0)
        );
    }
    let serialized = serde_json::to_string(&delta).unwrap();
    for secret in ["private_metrics_drive", "/private_metrics_path"] {
        assert!(!serialized.contains(secret));
    }
    delta
}

async fn no_replay(socket: &mut ServerSocket) {
    assert!(
        matches!(
            socket.next().await,
            None | Some(Err(_)) | Some(Ok(WsMessage::Close(_)))
        ),
        "discarded transaction must not replay a request"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "isolated process: MOUNT_RS_PROFILE_IO=1, storage/request traces=0"]
async fn websocket_stages_preserve_serialized_transactions() {
    assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MOUNT_RS_TRACE_STORAGE").as_deref(), Ok("0"));
    assert!(storage::enabled());
    timeout(Duration::from_secs(20), async {
        let mut phases = Vec::new();
        let before = storage::snapshot();
        let (transport, mut server) = pair().await;
        phases.push(("connect", phase(&before, [SUCCESS, SUCCESS, SUCCESS, ZERO, ZERO, ZERO, ZERO, ZERO])));
        let request = IoRequest { drive_id: "private_metrics_drive", handle: 11, position: Some(23) };
        let payload: Vec<u8> = (0..(CHUNK_BYTES * 2 + 7)).map(|n| (n * 71 % 256) as u8).collect();
        let mut buffer = vec![0; payload.len()];

        let before = storage::snapshot();
        let (result, ()) = tokio::join!(transport.exchange(message()), async {
            assert!(matches!(receive_envelope(&mut server).await, Incoming::Control(actual) if actual == message()));
            send_envelope(&mut server, &control_bytes(&response()).await).await;
        });
        assert_eq!(result.unwrap(), response());
        let (result, ()) = tokio::join!(transport.read(1, &request, &mut buffer), async {
            match receive_envelope(&mut server).await {
                Incoming::Read { request_id, request: actual, length } => {
                    assert_eq!((request_id, actual.drive_id.as_ref(), actual.handle, actual.position, length), (1, request.drive_id, 11, Some(23), payload.len()));
                }
                _ => panic!("expected binary read"),
            }
            send_envelope(&mut server, &result_bytes(1, Ok(IoResult::Read(payload.clone()))).await).await;
        });
        assert_eq!(result.unwrap(), payload.len());
        assert_eq!(buffer, payload);
        let (result, ()) = tokio::join!(transport.write(2, &request, &payload), async {
            match receive_envelope(&mut server).await {
                Incoming::Write { request_id, request: actual, data } => {
                    assert_eq!((request_id, actual.drive_id.as_ref(), actual.handle, actual.position), (2, request.drive_id, 11, Some(23)));
                    assert_eq!(data, payload);
                }
                _ => panic!("expected binary write"),
            }
            send_envelope(&mut server, &result_bytes(2, Ok(IoResult::Write(payload.len()))).await).await;
        });
        assert_eq!(result.unwrap(), payload.len());
        phases.push(("successful_reuse", phase(&before, [ZERO, ZERO, ZERO, (3, 0, 0), (3, 0, 0), (3, 0, 0), (3, 0, 0), (3, 0, 0)])));

        let before = storage::snapshot();
        let mut first = transport.exchange(message());
        tokio::select! {
            result = &mut first => panic!("response must remain held: {result:?}"),
            incoming = receive_envelope(&mut server) => assert!(matches!(incoming, Incoming::Control(actual) if actual == message())),
        }
        pending(&mut first).await;
        let mut second = transport.read(3, &request, &mut buffer);
        pending(&mut second).await;
        gauge([0, 0, 0, 1, 0, 0, 1, 0]);
        drop(second);
        gauge([0, 0, 0, 0, 0, 0, 1, 0]);
        let (result, ()) = tokio::join!(&mut first, async {
            send_envelope(&mut server, &control_bytes(&response()).await).await;
        });
        assert_eq!(result.unwrap(), response());
        drop(first);
        assert_eq!(buffer, payload);
        phases.push(("cancel_before_acquisition", phase(&before, [ZERO, ZERO, ZERO, (1, 0, 1), SUCCESS, SUCCESS, SUCCESS, SUCCESS])));

        // The same socket accepts another distinct request after queued cancellation.
        let before = storage::snapshot();
        let (result, ()) = tokio::join!(transport.write(4, &request, &payload), async {
            assert!(matches!(receive_envelope(&mut server).await, Incoming::Write { request_id: 4, data, .. } if data == payload));
            send_envelope(&mut server, &result_bytes(4, Ok(IoResult::Write(payload.len()))).await).await;
        });
        assert_eq!(result.unwrap(), payload.len());
        phases.push(("reuse_after_queued_cancellation", phase(&before, [ZERO, ZERO, ZERO, SUCCESS, SUCCESS, SUCCESS, SUCCESS, SUCCESS])));

        let before = storage::snapshot();
        let (result, ()) = tokio::join!(transport.read(5, &request, &mut buffer), async {
            assert!(matches!(receive_envelope(&mut server).await, Incoming::Read { request_id: 5, .. }));
            send_envelope(&mut server, &result_bytes(5, Err(WireError { code: "EIO".into() })).await).await;
        });
        assert!(matches!(result, Err(ClientError::Remote(code)) if code == "EIO"));
        assert_eq!(buffer, payload);
        phases.push(("canonical_remote_error", phase(&before, [ZERO, ZERO, ZERO, SUCCESS, SUCCESS, SUCCESS, SUCCESS, SUCCESS])));

        let before = storage::snapshot();
        let mut call = transport.write(6, &request, &payload);
        tokio::select! {
            result = &mut call => panic!("submitted write response must remain held: {result:?}"),
            incoming = receive_envelope(&mut server) => assert!(matches!(incoming, Incoming::Write { request_id: 6, data, .. } if data == payload)),
        }
        pending(&mut call).await;
        gauge([0, 0, 0, 0, 0, 0, 1, 0]);
        drop(call);
        phases.push(("cancel_after_send", phase(&before, [ZERO, ZERO, ZERO, SUCCESS, SUCCESS, SUCCESS, CANCELLED, ZERO])));
        let before = storage::snapshot();
        assert!(matches!(transport.exchange(message()).await, Err(ClientError::Transport)));
        assert!(matches!(transport.read(8, &request, &mut buffer).await, Err(ClientError::Transport)));
        assert!(matches!(transport.write(9, &request, &payload).await, Err(ClientError::Transport)));
        no_replay(&mut server).await;
        phases.push(("closed_socket_no_replay", phase(&before, [ZERO, ZERO, ZERO, (0, 3, 0), ZERO, ZERO, ZERO, ZERO])));
        transport.close();
        drop(server);

        for malformed in [true, false] {
            let (transport, mut server) = pair().await;
            let before = storage::snapshot();
            let (result, ()) = tokio::join!(transport.exchange(message()), async {
                assert!(matches!(receive_envelope(&mut server).await, Incoming::Control(actual) if actual == message()));
                if malformed {
                    server.send(WsMessage::Binary(vec![0, 0xff, 1].into())).await.unwrap();
                } else {
                    let wrong = Message::Response { request_id: 99, result: Ok(serde_json::json!({})) };
                    send_envelope(&mut server, &control_bytes(&wrong).await).await;
                }
            });
            assert!(matches!(result, Err(ClientError::Protocol)));
            phases.push((if malformed { "malformed_header" } else { "response_id_validation" }, phase(&before, [ZERO, ZERO, ZERO, SUCCESS, SUCCESS, SUCCESS, ERROR, ZERO])));
            assert!(matches!(transport.exchange(message()).await, Err(ClientError::Transport)));
            no_replay(&mut server).await;
            transport.close();
        }

        let (transport, mut server) = pair().await;
        let oversized = vec![0; binary::MAX_IO_BYTES + 1];
        let before = storage::snapshot();
        assert!(matches!(transport.write(10, &request, &oversized).await, Err(ClientError::Protocol)));
        no_replay(&mut server).await;
        phases.push(("request_encode_error", phase(&before, [ZERO, ZERO, ZERO, SUCCESS, ERROR, ZERO, ZERO, ZERO])));
        transport.close();

        let (listener, acceptor, roots) = listener().await;
        let address = listener.local_addr().unwrap();
        let before = storage::snapshot();
        let mut call = Box::pin(WebSocketTransport::connect(address, "localhost", roots));
        let (mut tcp, _) = tokio::select! {
            result = &mut call => panic!("TLS peer must remain held: {}", result.is_ok()),
            accepted = listener.accept() => accepted.unwrap(),
        };
        let mut tls_header = [0; 5];
        tokio::select! {
            result = &mut call => panic!("TLS peer must remain held: {}", result.is_ok()),
            received = tcp.read_exact(&mut tls_header) => { received.unwrap(); },
        }
        assert_eq!(tls_header[0], 22, "actual TLS ClientHello record reached the peer");
        pending(&mut call).await;
        gauge([0, 1, 0, 0, 0, 0, 0, 0]);
        drop(call);
        drop(tcp);
        drop(acceptor);
        phases.push(("cancel_tls_handshake", phase(&before, [SUCCESS, CANCELLED, ZERO, ZERO, ZERO, ZERO, ZERO, ZERO])));

        let (listener, acceptor, roots) = self::listener().await;
        let address = listener.local_addr().unwrap();
        let before = storage::snapshot();
        let mut call = Box::pin(WebSocketTransport::connect(address, "localhost", roots));
        let mut tls = tokio::select! {
            result = &mut call => panic!("upgrade peer must remain held: {}", result.is_ok()),
            accepted = async { let (tcp, _) = listener.accept().await.unwrap(); acceptor.accept(tcp).await.unwrap() } => accepted,
        };
        let mut upgrade_request = Vec::new();
        tokio::select! {
            result = &mut call => panic!("upgrade peer must remain held: {}", result.is_ok()),
            () = async {
                while !upgrade_request.ends_with(b"\r\n\r\n") {
                    assert!(upgrade_request.len() < 4096);
                    upgrade_request.push(tls.read_u8().await.unwrap());
                }
            } => {},
        }
        let upgrade_request = std::str::from_utf8(&upgrade_request).unwrap();
        assert!(upgrade_request.starts_with("GET /mount-rs HTTP/1.1\r\n"));
        assert!(upgrade_request.contains(SUBPROTOCOL));
        pending(&mut call).await;
        gauge([0, 0, 1, 0, 0, 0, 0, 0]);
        drop(call);
        drop(tls);
        phases.push(("cancel_upgrade", phase(&before, [SUCCESS, SUCCESS, CANCELLED, ZERO, ZERO, ZERO, ZERO, ZERO])));

        let (listener, acceptor, _) = self::listener().await;
        let before = storage::snapshot();
        let (client, server) = tokio::join!(
            WebSocketTransport::connect(listener.local_addr().unwrap(), "localhost", rustls::RootCertStore::empty()),
            async { let (tcp, _) = listener.accept().await.unwrap(); acceptor.accept(tcp).await }
        );
        assert!(matches!(client, Err(ClientError::Authentication)));
        assert!(server.is_err());
        phases.push(("tls_authentication_error", phase(&before, [SUCCESS, ERROR, ZERO, ZERO, ZERO, ZERO, ZERO, ZERO])));

        let (listener, acceptor, roots) = self::listener().await;
        let before = storage::snapshot();
        let (client, server) = tokio::join!(
            WebSocketTransport::connect(listener.local_addr().unwrap(), "localhost", roots),
            async {
                let (tcp, _) = listener.accept().await.unwrap();
                let tls = acceptor.accept(tcp).await.unwrap();
                tokio_tungstenite::accept_async_with_config(tls, Some(config())).await.unwrap()
            }
        );
        assert!(matches!(client, Err(ClientError::Protocol)));
        drop(server);
        phases.push(("upgrade_subprotocol_error", phase(&before, [SUCCESS, SUCCESS, ERROR, ZERO, ZERO, ZERO, ZERO, ZERO])));

        let address = listener.local_addr().unwrap();
        drop(listener);
        let before = storage::snapshot();
        assert!(matches!(WebSocketTransport::connect(address, "localhost", rustls::RootCertStore::empty()).await, Err(ClientError::Transport)));
        phases.push(("tcp_connect_error", phase(&before, [ERROR, ZERO, ZERO, ZERO, ZERO, ZERO, ZERO, ZERO])));
        gauge([0; 8]);
        println!("MOUNT_RS_WEBSOCKET_CLIENT_STAGE_OUTCOMES {}", serde_json::to_string(&phases).unwrap());
    })
    .await
    .expect("bounded WebSocket stage fixture");
}
