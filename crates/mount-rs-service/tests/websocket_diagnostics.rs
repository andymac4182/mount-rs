use async_trait::async_trait;
use mount_rs_service::{
    catalog::SqliteCatalog,
    dispatch::{DriveDispatcher, SessionIdentity},
    server::{Authenticator, RemoteServerOptions, RemoteTransferLimits},
    websocket::WebSocketServer,
};
use std::sync::Arc;

struct Deny;
#[async_trait]
impl Authenticator for Deny {
    async fn authenticate(&self, _: &str, _: &str) -> Result<SessionIdentity, ()> {
        Err(())
    }
}

async fn bind(requested: bool) -> (WebSocketServer, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
    );
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let dispatcher = Arc::new(DriveDispatcher::new(catalog));
    let server = if requested {
        WebSocketServer::bind_with_diagnostics(
            "127.0.0.1:0".parse().unwrap(),
            vec![certificate.cert.der().clone()],
            key.into(),
            dispatcher,
            Arc::new(Deny),
            RemoteServerOptions::default(),
            RemoteTransferLimits::default(),
            true,
        )
        .await
        .unwrap()
    } else {
        WebSocketServer::bind_with_transfer_limits(
            "127.0.0.1:0".parse().unwrap(),
            vec![certificate.cert.der().clone()],
            key.into(),
            dispatcher,
            Arc::new(Deny),
            RemoteServerOptions::default(),
            RemoteTransferLimits::default(),
        )
        .await
        .unwrap()
    };
    (server, directory)
}

#[tokio::test]
async fn ordinary_websocket_constructor_has_no_observer() {
    let (server, _directory) = bind(false).await;
    assert!(server.diagnostics().is_none());
    server.close().await;
}

#[tokio::test]
async fn explicit_websocket_observer_requires_profiling_feature() {
    let (server, _directory) = bind(true).await;
    assert_eq!(
        server.diagnostics().is_some(),
        cfg!(feature = "io-profiling")
    );
    server.close().await;
}

#[cfg(feature = "io-profiling")]
mod enabled {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use mount_rs_core::{FileHandle, FsDriver};
    use mount_rs_memfs::{MemoryFs, MemoryOptions};
    use mount_rs_remote_protocol::{
        Message, Operation, OperationName,
        binary::{self, Header, IoRequest},
    };
    use mount_rs_service::{
        catalog::{
            CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        },
        websocket::{WebSocketDiagnostics, WebSocketSnapshot},
    };
    use serde_json::json;
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
        time::Duration,
    };
    use tokio::sync::Notify;
    use tokio_tungstenite::{
        WebSocketStream,
        tungstenite::{Message as WsMessage, client::IntoClientRequest},
    };

    type Socket = WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

    #[derive(Default)]
    struct Gates {
        auth_entered: Notify,
        auth_release: Notify,
        read_entered: Notify,
        read_release: Notify,
        close_entered: Notify,
        close_release: Notify,
        hold_close: AtomicBool,
        closed: AtomicU64,
    }
    struct HeldAuth(Arc<Gates>);
    #[async_trait]
    impl Authenticator for HeldAuth {
        async fn authenticate(&self, token: &str, _: &str) -> Result<SessionIdentity, ()> {
            if token == "hold-secret-bearer" {
                self.0.auth_entered.notify_one();
                self.0.auth_release.notified().await;
            }
            if token == "deny-secret-bearer" {
                return Err(());
            }
            Ok(SessionIdentity {
                partition_id: "private-partition".into(),
                policy_id: "private-policy".into(),
                issuer: "https://private-issuer.example".into(),
                subject: if token == "changed-secret-bearer" {
                    "changed-subject"
                } else {
                    "private-subject"
                }
                .into(),
                signing_algorithm: "ES256".into(),
                claims: json!({"aud":"private-audience","repository_id":"private-repository"}),
                expires_at: i64::MAX,
            })
        }
    }
    struct HeldFs {
        memory: Arc<MemoryFs>,
        gates: Arc<Gates>,
    }
    struct HeldHandle {
        handle: Arc<dyn FileHandle>,
        gates: Arc<Gates>,
    }
    #[async_trait]
    impl FileHandle for HeldHandle {
        async fn read(
            &self,
            buffer: &mut [u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.gates.read_entered.notify_one();
            self.gates.read_release.notified().await;
            self.handle.read(buffer, position).await
        }
        async fn write(
            &self,
            buffer: &[u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.handle.write(buffer, position).await
        }
        async fn stat(&self) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.handle.stat().await
        }
        async fn truncate(&self, size: u64) -> mount_rs_core::Result<()> {
            self.handle.truncate(size).await
        }
        async fn close(&self) -> mount_rs_core::Result<()> {
            if self.gates.hold_close.load(Ordering::SeqCst) {
                self.gates.close_entered.notify_one();
                self.gates.close_release.notified().await;
            }
            self.handle.close().await?;
            self.gates.closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    #[async_trait]
    impl FsDriver for HeldFs {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            self.memory.capabilities()
        }
        async fn stat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.memory.stat(path).await
        }
        async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
            self.memory.readdir(path).await
        }
        async fn open(
            &self,
            path: &str,
            flags: &str,
            mode: u32,
        ) -> mount_rs_core::Result<Arc<dyn FileHandle>> {
            Ok(Arc::new(HeldHandle {
                handle: self.memory.open(path, flags, mode).await?,
                gates: self.gates.clone(),
            }))
        }
    }
    struct Fixture {
        server: WebSocketServer,
        observer: WebSocketDiagnostics,
        roots: rustls::RootCertStore,
        gates: Arc<Gates>,
        _directory: tempfile::TempDir,
    }
    impl Fixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let catalog = Arc::new(
                SqliteCatalog::open(directory.path().join("catalog.sqlite"))
                    .await
                    .unwrap(),
            );
            let mut snapshot = CatalogSnapshot::empty();
            snapshot.partitions.insert(
                "private-partition".into(),
                PartitionDefinition {
                    drives: BTreeMap::from([(
                        "private-drive".into(),
                        DriveDefinition {
                            driver: json!({"kind":"memory"}),
                        },
                    )]),
                },
            );
            snapshot.issuer_policies.insert(
                "private-policy".into(),
                json!({"issuer":"https://private-issuer.example","audiences":["private-audience"]}),
            );
            snapshot.grants.insert(
                "private-grant".into(),
                GrantDefinition {
                    partition_id: "private-partition".into(),
                    policy_id: "private-policy".into(),
                    drives: BTreeMap::from([("private-drive".into(), Permission::Write)]),
                    claim_conditions: BTreeMap::from([(
                        "/repository_id".into(),
                        "private-repository".into(),
                    )]),
                },
            );
            catalog.compare_and_swap(0, snapshot).await.unwrap();
            let gates = Arc::new(Gates::default());
            let memory = Arc::new(MemoryFs::new(MemoryOptions::default()));
            memory
                .write_file("/private-file", b"private-payload")
                .await
                .unwrap();
            let mut dispatcher = DriveDispatcher::new(catalog);
            dispatcher
                .register(
                    "private-partition",
                    "private-drive",
                    Arc::new(HeldFs {
                        memory,
                        gates: gates.clone(),
                    }),
                )
                .unwrap();
            let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let cert = certificate.cert.der().clone();
            let key = rustls::pki_types::PrivatePkcs8KeyDer::from(
                certificate.signing_key.serialize_der(),
            );
            let server = WebSocketServer::bind_with_diagnostics(
                "127.0.0.1:0".parse().unwrap(),
                vec![cert.clone()],
                key.into(),
                Arc::new(dispatcher),
                Arc::new(HeldAuth(gates.clone())),
                RemoteServerOptions::default(),
                RemoteTransferLimits::default(),
                true,
            )
            .await
            .unwrap();
            let observer = server.diagnostics().unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots.add(cert).unwrap();
            Self {
                server,
                observer,
                roots,
                gates,
                _directory: directory,
            }
        }
        async fn raw(&self) -> Socket {
            let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(self.roots.clone())
            .with_no_client_auth();
            let tcp = tokio::net::TcpStream::connect(self.server.local_addr())
                .await
                .unwrap();
            let tls = tokio_rustls::TlsConnector::from(Arc::new(tls))
                .connect("localhost".try_into().unwrap(), tcp)
                .await
                .unwrap();
            let mut request = format!("wss://{}/mount-rs", self.server.local_addr())
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "mount-rs.v2".parse().unwrap());
            tokio_tungstenite::client_async(request, tls)
                .await
                .unwrap()
                .0
        }
        async fn established(&self) -> Socket {
            let mut socket = self.raw().await;
            hello(&mut socket, "success-secret-bearer").await;
            assert!(matches!(
                control(&mut socket).await,
                Message::ServerHello { .. }
            ));
            settled(&self.observer).await;
            socket
        }
    }
    async fn signal(notify: &Notify) {
        tokio::time::timeout(Duration::from_secs(3), notify.notified())
            .await
            .unwrap();
    }
    async fn send_bytes(socket: &mut Socket, bytes: &[u8]) {
        socket
            .send(WsMessage::Binary(
                bytes[..binary::HEADER_BYTES].to_vec().into(),
            ))
            .await
            .unwrap();
        for chunk in bytes[binary::HEADER_BYTES..].chunks(32 * 1024) {
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
    async fn send(socket: &mut Socket, message: &Message) {
        let mut bytes = Vec::new();
        binary::write_control(&mut bytes, message).await.unwrap();
        send_bytes(socket, &bytes).await;
    }
    async fn hello(socket: &mut Socket, bearer: &str) {
        send(
            socket,
            &Message::ClientHello {
                version: 2,
                partition_id: "private-partition".into(),
                bearer: bearer.into(),
            },
        )
        .await;
    }
    async fn next_binary(socket: &mut Socket) -> Vec<u8> {
        loop {
            match tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
            {
                WsMessage::Binary(bytes) => return bytes.to_vec(),
                WsMessage::Ping(_) | WsMessage::Pong(_) => {}
                _ => panic!("binary response expected"),
            }
        }
    }
    async fn response_bytes(socket: &mut Socket) -> Vec<u8> {
        let mut bytes = next_binary(socket).await;
        let header = Header::decode(bytes.as_slice().try_into().unwrap()).unwrap();
        while bytes.len() < binary::HEADER_BYTES + header.control_len + header.payload_len {
            bytes.extend_from_slice(&next_binary(socket).await);
        }
        assert!(next_binary(socket).await.is_empty());
        bytes
    }
    async fn control(socket: &mut Socket) -> Message {
        let bytes = response_bytes(socket).await;
        binary::read_control(&mut bytes.as_slice()).await.unwrap()
    }
    struct Row {
        calls: u64,
        success: u64,
        error: u64,
        timeout: u64,
        cancelled: u64,
        in_flight: u64,
    }
    fn row(snapshot: &WebSocketSnapshot, name: &str) -> Row {
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap();
        Row {
            calls: entry.calls,
            success: entry.success,
            error: entry.error,
            timeout: entry.timeout,
            cancelled: entry.cancelled,
            in_flight: entry.in_flight,
        }
    }
    fn private_free(snapshot: &WebSocketSnapshot) {
        let json = serde_json::to_string(snapshot).unwrap();
        for secret in [
            "private-partition",
            "private-drive",
            "private-policy",
            "private-subject",
            "private-issuer",
            "private-file",
            "private-payload",
            "secret-bearer",
        ] {
            assert!(!json.contains(secret));
        }
        let value = serde_json::to_value(snapshot).unwrap();
        assert!(value.get("transport").is_none());
        assert_eq!(snapshot.schema, "mount-rs.service-websocket.v1");
    }
    async fn settled(observer: &WebSocketDiagnostics) -> WebSocketSnapshot {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = observer.snapshot();
                if snapshot.complete && snapshot.application_quiescent {
                    for entry in &snapshot.entries {
                        assert_eq!(
                            entry.calls,
                            entry.success + entry.error + entry.timeout + entry.cancelled
                        );
                        assert_eq!(entry.calls, entry.latency_log2_us.iter().sum::<u64>());
                    }
                    private_free(&snapshot);
                    return snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn held_hello_authentication_and_idle_socket_have_distinct_lifetimes() {
        let fixture = Fixture::new().await;
        let mut socket = fixture.raw().await;
        hello(&mut socket, "hold-secret-bearer").await;
        signal(&fixture.gates.auth_entered).await;
        let held = fixture.observer.snapshot();
        assert_eq!(held.active_handshakes_after, 1);
        assert_eq!(held.active_requests_after, 0);
        assert_eq!(row(&held, "auth.authenticate").in_flight, 1);
        assert_eq!(row(&held, "handshake.websocket_upgrade").success, 1);
        assert!(!held.application_quiescent);
        private_free(&held);
        fixture.gates.auth_release.notify_one();
        assert!(matches!(
            control(&mut socket).await,
            Message::ServerHello { .. }
        ));
        let idle = settled(&fixture.observer).await;
        assert_eq!(row(&idle, "auth.authenticate").success, 1);
        assert_eq!(row(&idle, "request.application").calls, 0);
        let observer = fixture.observer.clone();
        fixture.server.close().await;
        assert!(observer.snapshot().application_quiescent);
    }

    #[tokio::test]
    async fn held_renewal_is_a_request_and_changed_identity_is_still_rejected() {
        let fixture = Fixture::new().await;
        let mut socket = fixture.established().await;
        send(
            &mut socket,
            &Message::Renew {
                bearer: "hold-secret-bearer".into(),
            },
        )
        .await;
        signal(&fixture.gates.auth_entered).await;
        let held = fixture.observer.snapshot();
        assert_eq!(held.active_handshakes_after, 0);
        assert_eq!(held.active_requests_after, 1);
        assert_eq!(row(&held, "auth.authenticate").in_flight, 1);
        fixture.gates.auth_release.notify_one();
        assert!(matches!(
            control(&mut socket).await,
            Message::ServerHello { .. }
        ));
        let idle = settled(&fixture.observer).await;
        assert_eq!(row(&idle, "auth.authenticate").success, 2);
        send(
            &mut socket,
            &Message::Renew {
                bearer: "changed-secret-bearer".into(),
            },
        )
        .await;
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap(),
            None | Some(Err(_)) | Some(Ok(WsMessage::Close(_)))
        ));
        fixture.server.close().await;
        let after = fixture.observer.snapshot();
        assert_eq!(row(&after, "request.application").success, 1);
        assert_eq!(row(&after, "request.application").error, 1);
        assert!(after.application_quiescent);
        private_free(&after);
    }

    #[tokio::test]
    async fn cancelled_dispatch_settles_before_actual_cleanup_finishes() {
        let fixture = Fixture::new().await;
        let mut socket = fixture.established().await;
        send(
            &mut socket,
            &Message::Request {
                request_id: 1,
                drive_id: "private-drive".into(),
                operation: Operation {
                    name: OperationName::Open,
                    body: json!({"path":"/private-file","flags":"r","mode":0}),
                },
            },
        )
        .await;
        let handle = match control(&mut socket).await {
            Message::Response {
                result: Ok(value), ..
            } => value.as_u64().unwrap(),
            _ => panic!("open failed"),
        };
        settled(&fixture.observer).await;
        let mut bytes = Vec::new();
        binary::write_request(
            &mut bytes,
            2,
            &IoRequest {
                drive_id: "private-drive",
                handle,
                position: Some(0),
            },
            15,
            None,
        )
        .await
        .unwrap();
        send_bytes(&mut socket, &bytes).await;
        signal(&fixture.gates.read_entered).await;
        let held = fixture.observer.snapshot();
        assert_eq!(held.active_requests_after, 1);
        assert_eq!(row(&held, "dispatch.read").in_flight, 1);
        fixture.gates.hold_close.store(true, Ordering::SeqCst);
        let shutdown = tokio::spawn(fixture.server.close());
        signal(&fixture.gates.close_entered).await;
        assert!(!shutdown.is_finished());
        let closing = fixture.observer.snapshot();
        assert_eq!(row(&closing, "dispatch.read").cancelled, 1);
        assert_eq!(row(&closing, "request.application").cancelled, 1);
        assert_eq!(row(&closing, "session.cleanup").in_flight, 1);
        assert!(!closing.application_quiescent);
        assert_eq!(fixture.gates.closed.load(Ordering::SeqCst), 0);
        fixture.gates.close_release.notify_one();
        shutdown.await.unwrap();
        let after = settled(&fixture.observer).await;
        assert_eq!(row(&after, "session.cleanup").success, 1);
        assert_eq!(fixture.gates.closed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn successful_binary_write_and_held_read_record_actual_dispatch_results() {
        let fixture = Fixture::new().await;
        let mut socket = fixture.established().await;
        send(
            &mut socket,
            &Message::Request {
                request_id: 1,
                drive_id: "private-drive".into(),
                operation: Operation {
                    name: OperationName::Open,
                    body: json!({"path":"/private-file","flags":"r+","mode":0}),
                },
            },
        )
        .await;
        let handle = match control(&mut socket).await {
            Message::Response {
                result: Ok(value), ..
            } => value.as_u64().unwrap(),
            _ => panic!("open failed"),
        };
        let request = IoRequest {
            drive_id: "private-drive",
            handle,
            position: Some(0),
        };
        let payload = b"private-updated";
        let mut bytes = Vec::new();
        binary::write_request(&mut bytes, 2, &request, 0, Some(payload))
            .await
            .unwrap();
        send_bytes(&mut socket, &bytes).await;
        let response = response_bytes(&mut socket).await;
        assert_eq!(
            binary::read_result(&mut response.as_slice(), 2, false, &mut [], payload.len())
                .await
                .unwrap()
                .unwrap(),
            payload.len()
        );
        let mut bytes = Vec::new();
        binary::write_request(&mut bytes, 3, &request, payload.len(), None)
            .await
            .unwrap();
        send_bytes(&mut socket, &bytes).await;
        signal(&fixture.gates.read_entered).await;
        let held = fixture.observer.snapshot();
        assert_eq!(row(&held, "dispatch.write").success, 1);
        assert_eq!(row(&held, "dispatch.read").in_flight, 1);
        fixture.gates.read_release.notify_one();
        let response = response_bytes(&mut socket).await;
        let mut buffer = vec![0; payload.len()];
        assert_eq!(
            binary::read_result(&mut response.as_slice(), 3, true, &mut buffer, 0)
                .await
                .unwrap()
                .unwrap(),
            payload.len()
        );
        assert_eq!(&buffer, payload);
        let after = settled(&fixture.observer).await;
        assert_eq!(row(&after, "dispatch.read").success, 1);
        assert_eq!(row(&after, "request.application").success, 3);
        fixture.server.close().await;
    }

    #[tokio::test]
    async fn denied_dispatch_is_an_error_even_when_response_submission_succeeds() {
        let fixture = Fixture::new().await;
        let mut socket = fixture.established().await;
        send(
            &mut socket,
            &Message::Request {
                request_id: 1,
                drive_id: "ungranted-drive".into(),
                operation: Operation {
                    name: OperationName::Stat,
                    body: json!({"path":"/private-file"}),
                },
            },
        )
        .await;
        assert!(matches!(
            control(&mut socket).await,
            Message::Response { result: Err(_), .. }
        ));
        let after = settled(&fixture.observer).await;
        assert_eq!(row(&after, "dispatch.control").error, 1);
        assert_eq!(row(&after, "request.application").error, 1);
        assert_eq!(row(&after, "response.encode").success, 2);
        assert_eq!(row(&after, "response.submit").success, 2);
        fixture.server.close().await;
    }

    #[tokio::test]
    async fn incomplete_request_body_keeps_the_existing_deadline_and_records_timeout() {
        let fixture = Fixture::new().await;
        let mut socket = fixture.established().await;
        let mut bytes = Vec::new();
        binary::write_control(
            &mut bytes,
            &Message::Request {
                request_id: 1,
                drive_id: "private-drive".into(),
                operation: Operation {
                    name: OperationName::Stat,
                    body: json!({"path":"/private-file"}),
                },
            },
        )
        .await
        .unwrap();
        socket
            .send(WsMessage::Binary(
                bytes[..binary::HEADER_BYTES].to_vec().into(),
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while row(&fixture.observer.snapshot(), "request.read_incoming").in_flight != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;
        tokio::time::resume();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap(),
            None | Some(Err(_)) | Some(Ok(WsMessage::Close(_)))
        ));
        fixture.server.close().await;
        let after = settled(&fixture.observer).await;
        assert_eq!(row(&after, "request.application").timeout, 1);
        assert_eq!(row(&after, "request.read_incoming").timeout, 1);
        assert_eq!(row(&after, "dispatch.control").calls, 0);
    }
}
