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
        Self::with_driver(limits, None).await
    }
    async fn with_driver(limits: RemoteTransferLimits, driver: Option<Arc<dyn FsDriver>>) -> Self {
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
        dispatcher
            .register("red", "data", driver.unwrap_or_else(|| memory.clone()))
            .unwrap();
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
async fn send<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    message: &Message,
    terminator: bool,
) {
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
async fn next_binary<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> Vec<u8> {
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
async fn control_body<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> Message {
    let mut bytes = next_binary(socket).await;
    let header = Header::decode(bytes.as_slice().try_into().unwrap()).unwrap();
    while bytes.len() < 32 + header.control_len {
        bytes.extend_from_slice(&next_binary(socket).await)
    }
    binary::read_control(&mut bytes.as_slice()).await.unwrap()
}
async fn control<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> Message {
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

#[tokio::test]
async fn progressed_quic_tls_stall_never_contacts_websocket_fallback() {
    let certificate = rcgen::generate_simple_self_signed(
        std::iter::once("localhost".to_owned())
            .chain((0..300).map(|n| format!("host-{n}.example.test")))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let cert = certificate.cert.der().clone();
    let key: rustls::pki_types::PrivateKeyDer<'static> =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into();
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.clone()], key)
    .unwrap();
    tls.alpn_protocols = vec![b"mount-rs/2".to_vec()];
    let server = quinn::Endpoint::server(
        quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap(),
        )),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    let downstream = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let upstream = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let proxy_address = downstream.local_addr().unwrap();
    let server_address = server.local_addr().unwrap();
    let delivered = Arc::new(tokio::sync::Notify::new());
    let proxy = tokio::spawn({
        let delivered = delivered.clone();
        async move {
            let mut client = None;
            let mut response_delivered = false;
            let mut client_bytes = vec![0; 65536];
            let mut server_bytes = vec![0; 65536];
            loop {
                tokio::select! {
                    result=downstream.recv_from(&mut client_bytes)=>{let(count,address)=result.unwrap();client=Some(address);upstream.send_to(&client_bytes[..count],server_address).await.unwrap();},
                    result=upstream.recv_from(&mut server_bytes)=>{let(count,_)=result.unwrap();if !response_delivered {assert!(count>200,"actual QUIC TLS server flight");downstream.send_to(&server_bytes[..count],client.unwrap()).await.unwrap();response_delivered=true;delivered.notify_one();}},
                }
            }
        }
    });
    let handshake = tokio::spawn({
        let server = server.clone();
        async move {
            let incoming = server.accept().await.unwrap();
            let mut connecting = incoming.accept().unwrap();
            let negotiation = connecting
                .handshake_data()
                .await
                .unwrap()
                .downcast::<quinn::crypto::rustls::HandshakeData>()
                .unwrap();
            assert_eq!(
                negotiation.protocol.as_deref(),
                Some(b"mount-rs/2".as_slice())
            );
            let _ = connecting.await;
        }
    });
    let trap = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let token = directory.path().join("token");
    std::fs::write(&token, "dmFsaWQ.e30.c2ln").unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let connect = RemoteConnection::connect_with_transport(
        proxy_address,
        "localhost",
        roots,
        "red".into(),
        CredentialSource::File(token),
        ConnectionTransport::Auto {
            websocket: trap.local_addr().unwrap(),
        },
    );
    tokio::pin!(connect);
    tokio::select! {
        result=&mut connect=>assert!(result.is_err()),
        _=trap.accept()=>panic!("progressed QUIC TLS must never contact WebSocket fallback"),
        _=tokio::time::sleep(Duration::from_secs(6))=>panic!("initial selection must terminate"),
    }
    tokio::time::timeout(Duration::from_secs(1), delivered.notified())
        .await
        .unwrap();
    proxy.abort();
    server.close(0u32.into(), b"done");
    handshake.await.unwrap();
}

struct CloseGateFs {
    memory: Arc<MemoryFs>,
    opened: AtomicUsize,
    gate: Arc<CloseGate>,
}
struct CloseGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    invoked: AtomicUsize,
    completed: AtomicUsize,
}
struct CloseGateHandle {
    inner: Arc<dyn mount_rs_core::FileHandle>,
    first: bool,
    gate: Arc<CloseGate>,
}
#[async_trait]
impl mount_rs_core::FileHandle for CloseGateHandle {
    async fn read(&self, b: &mut [u8], p: Option<u64>) -> mount_rs_core::Result<usize> {
        self.inner.read(b, p).await
    }
    async fn write(&self, b: &[u8], p: Option<u64>) -> mount_rs_core::Result<usize> {
        self.inner.write(b, p).await
    }
    async fn stat(&self) -> mount_rs_core::Result<mount_rs_core::Stats> {
        self.inner.stat().await
    }
    async fn truncate(&self, n: u64) -> mount_rs_core::Result<()> {
        self.inner.truncate(n).await
    }
    async fn close(&self) -> mount_rs_core::Result<()> {
        self.gate.invoked.fetch_add(1, Ordering::SeqCst);
        if self.first {
            self.gate.entered.notify_one();
            self.gate.release.notified().await;
        }
        self.inner.close().await?;
        self.gate.completed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[async_trait]
impl FsDriver for CloseGateFs {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.memory.capabilities()
    }
    async fn stat(&self, p: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
        self.memory.stat(p).await
    }
    async fn readdir(&self, p: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
        self.memory.readdir(p).await
    }
    async fn open(
        &self,
        p: &str,
        f: &str,
        m: u32,
    ) -> mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>> {
        Ok(Arc::new(CloseGateHandle {
            inner: self.memory.open(p, f, m).await?,
            first: self.opened.fetch_add(1, Ordering::SeqCst) == 0,
            gate: self.gate.clone(),
        }))
    }
}
#[tokio::test]
async fn websocket_shutdown_waits_for_all_actual_closes_after_thirty_seconds() {
    let gate = Arc::new(CloseGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        invoked: AtomicUsize::new(0),
        completed: AtomicUsize::new(0),
    });
    let memory = Arc::new(MemoryFs::new(MemoryOptions::default()));
    memory.write_file("/file", b"old").await.unwrap();
    let driver = Arc::new(CloseGateFs {
        memory,
        opened: AtomicUsize::new(0),
        gate: gate.clone(),
    });
    let f = Fixture::with_driver(RemoteTransferLimits::default(), Some(driver)).await;
    let mut socket = f.raw("mount-rs.v2").await.unwrap();
    hello(&mut socket).await;
    for request_id in 1..=2 {
        send(
            &mut socket,
            &Message::Request {
                request_id,
                drive_id: "data".into(),
                operation: Operation {
                    name: OperationName::Open,
                    body: json!({"path":"/file","flags":"r","mode":0}),
                },
            },
            true,
        )
        .await;
        assert!(matches!(
            control(&mut socket).await,
            Message::Response { result: Ok(_), .. }
        ));
    }
    let shutdown = tokio::spawn(f.close());
    gate.entered.notified().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::task::yield_now().await;
    assert!(
        !shutdown.is_finished(),
        "shutdown must not silently abandon handles at30s"
    );
    assert_eq!(gate.invoked.load(Ordering::SeqCst), 2);
    assert_eq!(gate.completed.load(Ordering::SeqCst), 1);
    gate.release.notify_one();
    shutdown.await.unwrap();
    tokio::time::resume();
    assert_eq!(gate.invoked.load(Ordering::SeqCst), 2);
    assert_eq!(gate.completed.load(Ordering::SeqCst), 2);
}

// Tungstenite's callback requires its unboxed HTTP ErrorResponse type.
#[allow(clippy::result_large_err)]
fn fixture_upgrade(
    _: &tokio_tungstenite::tungstenite::handshake::server::Request,
    mut response: tokio_tungstenite::tungstenite::handshake::server::Response,
) -> Result<
    tokio_tungstenite::tungstenite::handshake::server::Response,
    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
> {
    response
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "mount-rs.v2".parse().unwrap());
    Ok(response)
}

#[tokio::test]
async fn malformed_tls_websocket_responses_fail_closed_without_socket_reuse() {
    for variant in 0..4 {
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = certificate.cert.der().clone();
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tcp.set_nodelay(true).unwrap();
            let tls = tokio_rustls::TlsAcceptor::from(Arc::new(tls))
                .accept(tcp)
                .await
                .unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(tls, fixture_upgrade)
                .await
                .unwrap();
            assert!(matches!(
                peer_incoming(&mut socket).await,
                binary::Incoming::Control(Message::ClientHello { version: 2, .. })
            ));
            send(
                &mut socket,
                &Message::ServerHello {
                    version: 2,
                    session_id: "fixture".into(),
                },
                true,
            )
            .await;
            let request = loop {
                let request = peer_incoming(&mut socket).await;
                if matches!(request, binary::Incoming::Control(Message::Renew { .. })) {
                    send(
                        &mut socket,
                        &Message::ServerHello {
                            version: 2,
                            session_id: "renewed".into(),
                        },
                        true,
                    )
                    .await;
                } else {
                    break request;
                }
            };
            match request {
                binary::Incoming::Control(Message::Request { request_id, .. }) => {
                    send(
                        &mut socket,
                        &Message::Response {
                            request_id: if variant == 0 {
                                request_id + 1
                            } else {
                                request_id
                            },
                            result: Ok(json!(null)),
                        },
                        variant != 3,
                    )
                    .await
                }
                binary::Incoming::Write { request_id, .. } => {
                    let mut bytes = Vec::new();
                    binary::write_result(
                        &mut bytes,
                        request_id,
                        Ok(if variant == 1 {
                            binary::IoResult::Write(2)
                        } else {
                            binary::IoResult::Read(vec![1])
                        }),
                    )
                    .await
                    .unwrap();
                    socket
                        .send(WsMessage::Binary(bytes[..32].to_vec().into()))
                        .await
                        .unwrap();
                    if bytes.len() > 32 {
                        socket
                            .send(WsMessage::Binary(bytes[32..].to_vec().into()))
                            .await
                            .unwrap();
                    }
                    socket
                        .send(WsMessage::Binary(Vec::new().into()))
                        .await
                        .unwrap();
                }
                _ => panic!("fixture operation"),
            }
            if variant != 3 {
                let next = tokio::time::timeout(Duration::from_secs(2), socket.next())
                    .await
                    .unwrap();
                assert!(
                    matches!(next, None | Some(Err(_)) | Some(Ok(WsMessage::Close(_)))),
                    "malformed response must close socket without another request"
                );
            }
        });
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let token = directory.path().join("token");
        std::fs::write(&token, "dmFsaWQ.e30.c2ln").unwrap();
        let connection = RemoteConnection::connect_with_transport(
            address,
            "localhost",
            roots,
            "red".into(),
            CredentialSource::File(token),
            ConnectionTransport::WebSocket(address),
        )
        .await
        .unwrap();
        let result = if variant == 1 || variant == 2 {
            tokio::time::timeout(
                Duration::from_secs(2),
                connection.write("data", 1, Some(0), &[1]),
            )
            .await
            .unwrap()
            .map(|_| json!(null))
        } else {
            tokio::time::timeout(
                Duration::from_secs(2),
                connection.request(
                    "data",
                    Operation {
                        name: OperationName::Stat,
                        body: json!({"path":"/"}),
                    },
                ),
            )
            .await
            .unwrap()
        };
        assert!(matches!(
            result,
            Err(ClientError::Transport | ClientError::Protocol)
        ));
        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(1),
                connection.request(
                    "data",
                    Operation {
                        name: OperationName::Stat,
                        body: json!({"path":"/"})
                    }
                )
            )
            .await
            .unwrap(),
            Err(ClientError::Transport)
        ));
        peer.await.unwrap();
    }
}
async fn peer_incoming<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> binary::Incoming {
    let bytes = next_binary(socket).await;
    let header = Header::decode(bytes.as_slice().try_into().unwrap()).unwrap();
    let mut body = Vec::new();
    while body.len() < header.control_len + header.payload_len {
        body.extend_from_slice(&next_binary(socket).await);
    }
    assert!(next_binary(socket).await.is_empty());
    binary::read_body(&mut body.as_slice(), header)
        .await
        .unwrap()
}
