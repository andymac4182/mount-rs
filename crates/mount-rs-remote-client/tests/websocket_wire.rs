//! Actual TLS/WebSocket framing and fail-closed initial transport selection.
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use mount_rs_core::{FsDriver, Loopback};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_remote_client::{
    connection::{ClientError, ConnectionTransport, RemoteConnection},
    credentials::CredentialSource,
};
use mount_rs_remote_protocol::{
    Message, Operation, OperationName,
    binary::{self, Header, Kind},
};
use mount_rs_service::{
    catalog::{
        CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        SqliteCatalog,
    },
    dispatch::{DriveDispatcher, SessionIdentity},
    server::{Authenticator, RemoteServer, RemoteServerOptions, RemoteTransferLimits},
    websocket::WebSocketServer,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message as WsMessage, client::IntoClientRequest},
};
type Socket = WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;
struct Auth(Arc<AtomicUsize>);
#[async_trait]
impl Authenticator for Auth {
    async fn authenticate(&self, token: &str, partition: &str) -> Result<SessionIdentity, ()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if token != "dmFsaWQ.e30.c2ln" || partition != "red" {
            return Err(());
        }
        Ok(SessionIdentity {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            issuer: "https://issuer.example.com".into(),
            subject: "subject".into(),
            signing_algorithm: "ES256".into(),
            claims: json!({"aud":"mount-rs","repository_id":"repo-1"}),
            expires_at: i64::MAX,
        })
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    websocket: WebSocketServer,
    quic: RemoteServer,
    roots: rustls::RootCertStore,
    memory: Arc<MemoryFs>,
    auth_calls: Arc<AtomicUsize>,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_limits(RemoteTransferLimits::default()).await
    }
    async fn with_limits(limits: RemoteTransferLimits) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Arc::new(
            SqliteCatalog::open(directory.path().join("catalog.sqlite"))
                .await
                .unwrap(),
        );
        let mut snapshot = CatalogSnapshot::empty();
        snapshot.partitions.insert(
            "red".into(),
            PartitionDefinition {
                drives: BTreeMap::from([(
                    "data".into(),
                    DriveDefinition {
                        driver: json!({"kind":"memory"}),
                    },
                )]),
            },
        );
        snapshot.issuer_policies.insert(
            "oidc".into(),
            json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
        );
        snapshot.grants.insert(
            "writer".into(),
            GrantDefinition {
                partition_id: "red".into(),
                policy_id: "oidc".into(),
                drives: BTreeMap::from([("data".into(), Permission::Write)]),
                claim_conditions: BTreeMap::from([("/repository_id".into(), "repo-1".into())]),
            },
        );
        catalog.compare_and_swap(0, snapshot).await.unwrap();
        let memory = Arc::new(MemoryFs::new(MemoryOptions::default()));
        memory.write_file("/file", b"old").await.unwrap();
        let mut dispatcher = DriveDispatcher::new(catalog);
        dispatcher.register("red", "data", memory.clone()).unwrap();
        let dispatcher = Arc::new(dispatcher);
        let auth_calls = Arc::new(AtomicUsize::new(0));
        let auth = Arc::new(Auth(auth_calls.clone()));
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = certificate.cert.der().clone();
        let key: rustls::pki_types::PrivateKeyDer<'static> =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der())
                .into();
        let websocket = WebSocketServer::bind_with_transfer_limits(
            "127.0.0.1:0".parse().unwrap(),
            vec![cert.clone()],
            key.clone_key(),
            dispatcher.clone(),
            auth.clone(),
            RemoteServerOptions::default(),
            limits,
        )
        .await
        .unwrap();
        let quic = RemoteServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            vec![cert.clone()],
            key,
            dispatcher,
            auth,
        )
        .await
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        Self {
            directory,
            websocket,
            quic,
            roots,
            memory,
            auth_calls,
        }
    }
    async fn raw(
        &self,
        protocol: &str,
    ) -> Result<Socket, Box<dyn std::error::Error + Send + Sync>> {
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(self.roots.clone())
        .with_no_client_auth();
        let tcp = tokio::net::TcpStream::connect(self.websocket.local_addr()).await?;
        tcp.set_nodelay(true)?;
        let tls = tokio_rustls::TlsConnector::from(Arc::new(tls))
            .connect("localhost".try_into()?, tcp)
            .await?;
        let mut request =
            format!("wss://{}/mount-rs", self.websocket.local_addr()).into_client_request()?;
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", protocol.parse()?);
        let (socket, response) = tokio_tungstenite::client_async(request, tls).await?;
        assert_eq!(
            response.headers().get("Sec-WebSocket-Protocol").unwrap(),
            protocol
        );
        Ok(socket)
    }
    fn credentials(&self, token: &str) -> CredentialSource {
        let path = self.directory.path().join("token");
        std::fs::write(&path, token).unwrap();
        CredentialSource::File(path)
    }
    async fn close(self) {
        self.websocket.close().await;
        self.quic.close().await;
    }
}
async fn send(socket: &mut Socket, message: &Message, terminator: bool) {
    let mut bytes = Vec::new();
    binary::write_control(&mut bytes, message).await.unwrap();
    socket
        .send(WsMessage::Binary(bytes[..32].to_vec().into()))
        .await
        .unwrap();
    for chunk in bytes[32..].chunks(32 * 1024) {
        socket
            .send(WsMessage::Binary(chunk.to_vec().into()))
            .await
            .unwrap();
    }
    if terminator {
        socket
            .send(WsMessage::Binary(Vec::new().into()))
            .await
            .unwrap();
    }
}
async fn next_binary(socket: &mut Socket) -> Vec<u8> {
    loop {
        match tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            WsMessage::Binary(b) => return b.to_vec(),
            WsMessage::Pong(_) => {}
            _ => panic!("binary websocket message expected"),
        }
    }
}
async fn control_body(socket: &mut Socket) -> Message {
    let mut bytes = next_binary(socket).await;
    let header = Header::decode(bytes.as_slice().try_into().unwrap()).unwrap();
    while bytes.len() < 32 + header.control_len {
        bytes.extend_from_slice(&next_binary(socket).await)
    }
    binary::read_control(&mut bytes.as_slice()).await.unwrap()
}
async fn control(socket: &mut Socket) -> Message {
    let message = control_body(socket).await;
    assert!(next_binary(socket).await.is_empty());
    message
}
async fn hello(socket: &mut Socket) {
    send(
        socket,
        &Message::ClientHello {
            version: 2,
            partition_id: "red".into(),
            bearer: "dmFsaWQ.e30.c2ln".into(),
        },
        true,
    )
    .await;
    assert!(matches!(
        control(socket).await,
        Message::ServerHello { version: 2, .. }
    ));
}
async fn closed(socket: &mut Socket) {
    let result = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap();
    assert!(matches!(
        result,
        None | Some(Err(_)) | Some(Ok(WsMessage::Close(_)))
    ));
}
fn write() -> Message {
    Message::Request {
        request_id: 1,
        drive_id: "data".into(),
        operation: Operation {
            name: OperationName::Write,
            body: json!({"path":"/file","data":[110,101,119]}),
        },
    }
}
#[tokio::test]
async fn incomplete_envelope_never_commits_until_end_marker() {
    let f = Fixture::new().await;
    let mut socket = f.raw("mount-rs.v2").await.unwrap();
    hello(&mut socket).await;
    send(&mut socket, &write(), false).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), socket.next())
            .await
            .is_err(),
        "must wait for envelope terminator"
    );
    assert_eq!(
        Loopback::from_arc(f.memory.clone())
            .read_file("/file")
            .await
            .unwrap(),
        b"old"
    );
    socket
        .send(WsMessage::Binary(Vec::new().into()))
        .await
        .unwrap();
    assert!(matches!(
        control(&mut socket).await,
        Message::Response { result: Ok(_), .. }
    ));
    assert_eq!(
        Loopback::from_arc(f.memory.clone())
            .read_file("/file")
            .await
            .unwrap(),
        b"new"
    );
    drop(socket);
    f.close().await;
}
#[tokio::test]
async fn websocket_rejects_old_subprotocol_and_hello_version() {
    let f = Fixture::new().await;
    assert!(f.raw("mount-rs.v1").await.is_err());
    let mut socket = f.raw("mount-rs.v2").await.unwrap();
    send(
        &mut socket,
        &Message::ClientHello {
            version: 1,
            partition_id: "red".into(),
            bearer: "dmFsaWQ.e30.c2ln".into(),
        },
        true,
    )
    .await;
    closed(&mut socket).await;
    assert_eq!(f.auth_calls.load(Ordering::SeqCst), 0);
    f.close().await;
}
#[tokio::test]
async fn malformed_oversized_and_trailing_chunks_fail_before_mutation() {
    let f = Fixture::new().await;
    for variant in 0..5 {
        let mut socket = f.raw("mount-rs.v2").await.unwrap();
        hello(&mut socket).await;
        match variant {
            0 => socket
                .send(WsMessage::Text("invalid".into()))
                .await
                .unwrap(),
            1 => socket
                .send(WsMessage::Binary(vec![0; 32 * 1024 + 1].into()))
                .await
                .unwrap(),
            2 => {
                send(&mut socket, &write(), false).await;
                socket
                    .send(WsMessage::Binary(b"trailing".to_vec().into()))
                    .await
                    .unwrap();
            }
            3 => {
                socket
                    .send(WsMessage::Binary(
                        Header::new(Kind::Control, 0, 100, 0, 0)
                            .unwrap()
                            .encode()
                            .unwrap()
                            .to_vec()
                            .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(WsMessage::Binary(Vec::new().into()))
                    .await
                    .unwrap();
            }
            _ => {
                let mut header = Header::new(Kind::Control, 0, 100, 0, 0)
                    .unwrap()
                    .encode()
                    .unwrap();
                header[16..20]
                    .copy_from_slice(&((binary::MAX_CONTROL_BYTES + 1) as u32).to_be_bytes());
                socket
                    .send(WsMessage::Binary(header.to_vec().into()))
                    .await
                    .unwrap();
            }
        }
        closed(&mut socket).await;
        assert_eq!(
            Loopback::from_arc(f.memory.clone())
                .read_file("/file")
                .await
                .unwrap(),
            b"old"
        );
    }
    f.close().await;
}
#[tokio::test]
async fn auto_never_falls_back_after_quic_certificate_or_authentication_failure() {
    let f = Fixture::new().await;
    let selection = ConnectionTransport::Auto {
        websocket: f.websocket.local_addr(),
    };
    let credentials = f.credentials("dmFsaWQ.e30.c2ln");
    let certificate = RemoteConnection::connect_with_transport(
        f.quic.local_addr(),
        "localhost",
        rustls::RootCertStore::empty(),
        "red".into(),
        credentials,
        selection,
    )
    .await;
    assert!(matches!(certificate, Err(ClientError::Authentication)));
    assert_eq!(f.auth_calls.load(Ordering::SeqCst), 0);
    let denied = RemoteConnection::connect_with_transport(
        f.quic.local_addr(),
        "localhost",
        f.roots.clone(),
        "red".into(),
        f.credentials("YmFk.e30.c2ln"),
        selection,
    )
    .await;
    assert!(matches!(denied, Err(ClientError::Authentication)));
    assert_eq!(f.auth_calls.load(Ordering::SeqCst), 1);
    f.close().await;
}
#[tokio::test]
async fn websocket_shutdown_closes_authenticated_and_handshake_connections() {
    let f = Fixture::new().await;
    let mut socket = f.raw("mount-rs.v2").await.unwrap();
    hello(&mut socket).await;
    let _stalled = tokio::net::TcpStream::connect(f.websocket.local_addr())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), f.close())
        .await
        .unwrap();
    closed(&mut socket).await;
}

#[tokio::test]
async fn authenticated_idle_mount_survives_sixty_seconds() {
    let f = Fixture::new().await;
    let mut socket = f.raw("mount-rs.v2").await.unwrap();
    hello(&mut socket).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    send(
        &mut socket,
        &Message::Request {
            request_id: 1,
            drive_id: "data".into(),
            operation: Operation {
                name: OperationName::Stat,
                body: json!({"path":"/"}),
            },
        },
        true,
    )
    .await;
    assert!(matches!(
        control(&mut socket).await,
        Message::Response { result: Ok(_), .. }
    ));
    drop(socket);
    f.close().await;
}

#[tokio::test]
async fn fragmented_header_with_interleaved_ping_uses_current_codec() {
    use tokio_tungstenite::tungstenite::protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    };
    let f = Fixture::new().await;
    let mut socket = f.raw("mount-rs.v2").await.unwrap();
    hello(&mut socket).await;
    let mut bytes = Vec::new();
    binary::write_control(&mut bytes, &write()).await.unwrap();
    socket
        .send(WsMessage::Frame(Frame::message(
            bytes[..16].to_vec(),
            OpCode::Data(Data::Binary),
            false,
        )))
        .await
        .unwrap();
    socket
        .send(WsMessage::Ping(b"probe".to_vec().into()))
        .await
        .unwrap();
    socket
        .send(WsMessage::Frame(Frame::message(
            bytes[16..32].to_vec(),
            OpCode::Data(Data::Continue),
            true,
        )))
        .await
        .unwrap();
    socket
        .send(WsMessage::Binary(bytes[32..].to_vec().into()))
        .await
        .unwrap();
    socket
        .send(WsMessage::Binary(Vec::new().into()))
        .await
        .unwrap();
    assert!(matches!(
        control(&mut socket).await,
        Message::Response { result: Ok(_), .. }
    ));
    assert_eq!(
        Loopback::from_arc(f.memory.clone())
            .read_file("/file")
            .await
            .unwrap(),
        b"new"
    );
    drop(socket);
    f.close().await;
}
#[tokio::test]
async fn incomplete_body_times_out_and_never_changes_backing_bytes() {
    let f = Fixture::new().await;
    let mut socket = f.raw("mount-rs.v2").await.unwrap();
    hello(&mut socket).await;
    socket
        .send(WsMessage::Binary(
            Header::new(Kind::Control, 0, 100, 0, 0)
                .unwrap()
                .encode()
                .unwrap()
                .to_vec()
                .into(),
        ))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(12), socket.next())
        .await
        .unwrap();
    assert!(matches!(
        result,
        None | Some(Err(_)) | Some(Ok(WsMessage::Close(_)))
    ));
    assert_eq!(
        Loopback::from_arc(f.memory.clone())
            .read_file("/file")
            .await
            .unwrap(),
        b"old"
    );
    f.close().await;
}

#[tokio::test]
async fn data_admission_before_body_preserves_other_connection_hello_and_releases_on_cancel() {
    let f = Fixture::with_limits(RemoteTransferLimits {
        active_data_operations: 1,
        ..RemoteTransferLimits::default()
    })
    .await;
    let mut held = f.raw("mount-rs.v2").await.unwrap();
    hello(&mut held).await;
    let header = Header::new(
        Kind::Write,
        7,
        binary::IO_PREFIX_BYTES + 4,
        binary::MAX_IO_BYTES,
        0,
    )
    .unwrap();
    held.send(WsMessage::Binary(header.encode().unwrap().to_vec().into()))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), held.next())
            .await
            .is_err()
    );
    let mut probe = f.raw("mount-rs.v2").await.unwrap();
    hello(&mut probe).await;
    // Data quota is held even though the raw payload has not arrived. Hello
    // uses reserved control capacity on this separate serialized connection.
    send(&mut probe, &write(), true).await;
    closed(&mut probe).await;
    assert_eq!(
        Loopback::from_arc(f.memory.clone())
            .read_file("/file")
            .await
            .unwrap(),
        b"old"
    );
    held.send(WsMessage::Binary(Vec::new().into()))
        .await
        .unwrap();
    closed(&mut held).await;
    let mut next = f.raw("mount-rs.v2").await.unwrap();
    hello(&mut next).await;
    send(&mut next, &write(), true).await;
    assert!(matches!(
        control(&mut next).await,
        Message::Response { result: Ok(_), .. }
    ));
    drop(next);
    f.close().await;
}
