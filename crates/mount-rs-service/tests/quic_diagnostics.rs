use async_trait::async_trait;
use mount_rs_service::{
    catalog::SqliteCatalog,
    dispatch::{DriveDispatcher, SessionIdentity},
    server::{Authenticator, RemoteServer, RemoteServerOptions, RemoteTransferLimits},
};
use std::sync::Arc;

struct Deny;

#[async_trait]
impl Authenticator for Deny {
    async fn authenticate(&self, _: &str, _: &str) -> Result<SessionIdentity, ()> {
        Err(())
    }
}

async fn bind(diagnostics: bool) -> (RemoteServer, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
    );
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let server = RemoteServer::bind_with_diagnostics(
        "127.0.0.1:0".parse().unwrap(),
        vec![certificate.cert.der().clone()],
        key.into(),
        Arc::new(DriveDispatcher::new(catalog)),
        Arc::new(Deny),
        RemoteServerOptions::default(),
        RemoteTransferLimits::default(),
        diagnostics,
    )
    .await
    .unwrap();
    (server, directory)
}

#[tokio::test]
async fn disabled_server_has_no_local_observer() {
    let (server, _directory) = bind(false).await;
    assert!(server.diagnostics().is_none());
    server.close().await;
}

#[cfg(feature = "io-profiling")]
#[tokio::test]
async fn requested_server_diagnostics_observer_is_available() {
    let (server, _directory) = bind(true).await;
    let observer_available = server.diagnostics().is_some();
    server.close().await;
    assert!(observer_available, "observer required");
}

#[cfg(not(feature = "io-profiling"))]
#[tokio::test]
async fn default_build_explicit_request_is_unavailable() {
    let (server, _directory) = bind(true).await;
    let observer_available = server.diagnostics().is_some();
    server.close().await;
    assert!(!observer_available);
}

#[cfg(feature = "io-profiling")]
mod enabled {
    use super::*;
    use mount_rs_core::{FileHandle, FsDriver};
    use mount_rs_memfs::{MemoryFs, MemoryOptions};
    use mount_rs_remote_protocol::{
        Message, Operation, OperationName,
        binary::{self, IoRequest},
        read_frame, write_frame,
    };
    use mount_rs_service::{
        catalog::{
            CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        },
        server::{ServerDiagnostics, ServerSnapshot},
    };
    use serde_json::json;
    use std::{collections::BTreeMap, time::Duration};
    use tokio::sync::Notify;

    struct HeldAuth {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    #[async_trait]
    impl Authenticator for HeldAuth {
        async fn authenticate(&self, token: &str, _: &str) -> Result<SessionIdentity, ()> {
            if token == "hold-secret-bearer" {
                self.entered.notify_one();
                self.release.notified().await;
            }
            if token == "deny-secret-bearer" {
                return Err(());
            }
            Ok(SessionIdentity {
                partition_id: "private-partition".into(),
                policy_id: "private-policy".into(),
                issuer: "https://private-issuer.example".into(),
                subject: "private-subject".into(),
                signing_algorithm: "ES256".into(),
                claims: json!({"aud":"private-audience", "repository_id":"private-repository"}),
                expires_at: i64::MAX,
            })
        }
    }
    struct HeldFs {
        memory: Arc<MemoryFs>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    struct HeldHandle {
        handle: Arc<dyn FileHandle>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    #[async_trait]
    impl FileHandle for HeldHandle {
        async fn read(
            &self,
            buffer: &mut [u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.entered.notify_one();
            self.release.notified().await;
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
            self.handle.close().await
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
                entered: self.entered.clone(),
                release: self.release.clone(),
            }))
        }
    }
    struct Fixture {
        server: RemoteServer,
        observer: ServerDiagnostics,
        endpoint: quinn::Endpoint,
        connection: quinn::Connection,
        auth_entered: Arc<Notify>,
        auth_release: Arc<Notify>,
        read_entered: Arc<Notify>,
        read_release: Arc<Notify>,
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
            let memory = Arc::new(MemoryFs::new(MemoryOptions::default()));
            memory
                .write_file("/private-file", b"private-payload")
                .await
                .unwrap();
            let read_entered = Arc::new(Notify::new());
            let read_release = Arc::new(Notify::new());
            let mut dispatcher = DriveDispatcher::new(catalog);
            dispatcher
                .register(
                    "private-partition",
                    "private-drive",
                    Arc::new(HeldFs {
                        memory,
                        entered: read_entered.clone(),
                        release: read_release.clone(),
                    }),
                )
                .unwrap();
            let auth_entered = Arc::new(Notify::new());
            let auth_release = Arc::new(Notify::new());
            let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let cert = certificate.cert.der().clone();
            let key = rustls::pki_types::PrivatePkcs8KeyDer::from(
                certificate.signing_key.serialize_der(),
            );
            let server = RemoteServer::bind_with_diagnostics(
                "127.0.0.1:0".parse().unwrap(),
                vec![cert.clone()],
                key.into(),
                Arc::new(dispatcher),
                Arc::new(HeldAuth {
                    entered: auth_entered.clone(),
                    release: auth_release.clone(),
                }),
                RemoteServerOptions { max_connections: 2 },
                RemoteTransferLimits::default(),
                true,
            )
            .await
            .unwrap();
            let observer = server.diagnostics().unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots.add(cert).unwrap();
            let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
            tls.alpn_protocols = vec![b"mount-rs/2".to_vec()];
            let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
            endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
                quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
            )));
            let connection = endpoint
                .connect(server.local_addr(), "localhost")
                .unwrap()
                .await
                .unwrap();
            Self {
                server,
                observer,
                endpoint,
                connection,
                auth_entered,
                auth_release,
                read_entered,
                read_release,
                _directory: directory,
            }
        }
        async fn hello(&self, token: &str) -> (quinn::SendStream, quinn::RecvStream) {
            let (mut send, recv) = self.connection.open_bi().await.unwrap();
            write_frame(
                &mut send,
                &Message::ClientHello {
                    version: 2,
                    partition_id: "private-partition".into(),
                    bearer: token.into(),
                },
            )
            .await
            .unwrap();
            send.finish().unwrap();
            (send, recv)
        }
        async fn established(&self) {
            let (_, mut recv) = self.hello("success-secret-bearer").await;
            assert!(matches!(
                read_frame(&mut recv).await.unwrap(),
                Message::ServerHello { .. }
            ));
            recv.read_to_end(0).await.unwrap();
        }
        async fn control(&self, id: u64, drive: &str, operation: Operation) -> Message {
            let (mut send, mut recv) = self.connection.open_bi().await.unwrap();
            write_frame(
                &mut send,
                &Message::Request {
                    request_id: id,
                    drive_id: drive.into(),
                    operation,
                },
            )
            .await
            .unwrap();
            send.finish().unwrap();
            let response = read_frame(&mut recv).await.unwrap();
            recv.read_to_end(0).await.unwrap();
            response
        }
        async fn open(&self) -> u64 {
            match self
                .control(
                    1,
                    "private-drive",
                    Operation {
                        name: OperationName::Open,
                        body: json!({"path":"/private-file","flags":"r+","mode":420}),
                    },
                )
                .await
            {
                Message::Response {
                    result: Ok(value), ..
                } => value.as_u64().unwrap(),
                _ => panic!("open failed"),
            }
        }
        async fn close(self) -> ServerSnapshot {
            self.connection.close(0_u32.into(), b"done");
            self.endpoint.close(0_u32.into(), b"done");
            self.server.close().await;
            self.observer.snapshot()
        }
    }
    fn entry(snapshot: &ServerSnapshot, name: &str) -> (u64, u64, u64, u64) {
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap();
        (entry.calls, entry.success, entry.error, entry.cancelled)
    }
    fn assert_private_free(snapshot: &ServerSnapshot) {
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
            assert!(
                !json.contains(secret),
                "diagnostics contain secret sentinel"
            );
        }
    }
    async fn signal(notify: &Notify) {
        tokio::time::timeout(Duration::from_secs(3), notify.notified())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn held_authentication_exposes_handshake_and_actual_quinn_then_retires() {
        let fixture = Fixture::new().await;
        let (_, mut recv) = fixture.hello("hold-secret-bearer").await;
        signal(&fixture.auth_entered).await;
        let held = fixture.observer.snapshot();
        assert_eq!(held.active_handshakes_after, 1);
        assert_eq!(held.active_requests_after, 0);
        assert!(!held.application_quiescent);
        assert_eq!(held.transport.active.len(), 1);
        assert_eq!(held.transport.active[0].id, 1);
        assert!(held.transport.observed_total.udp_rx_bytes > 0);
        assert!(held.transport.observed_total.frame_rx.stream > 0);
        assert_private_free(&held);
        fixture.auth_release.notify_one();
        assert!(matches!(
            read_frame(&mut recv).await.unwrap(),
            Message::ServerHello { .. }
        ));
        recv.read_to_end(0).await.unwrap();
        let after = fixture.close().await;
        assert!(after.complete && after.application_quiescent);
        assert_eq!(entry(&after, "auth.authenticate"), (1, 1, 0, 0));
        assert_eq!(entry(&after, "handshake.application"), (1, 1, 0, 0));
        assert_eq!(after.transport.registered_connections, 1);
        assert_eq!(after.transport.retired_connections, 1);
        assert!(after.transport.active.is_empty());
        assert!(after.transport.retired.udp_rx_bytes >= held.transport.observed_total.udp_rx_bytes);
        assert_private_free(&after);
    }

    #[tokio::test]
    async fn held_dispatch_and_denial_preserve_request_lifetime_and_terminal_counts() {
        let fixture = Fixture::new().await;
        fixture.established().await;
        let handle = fixture.open().await;
        let (mut send, mut recv) = fixture.connection.open_bi().await.unwrap();
        binary::write_request(
            &mut send,
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
        send.finish().unwrap();
        signal(&fixture.read_entered).await;
        let held = fixture.observer.snapshot();
        assert_eq!(held.active_requests_after, 1);
        assert_eq!(
            held.entries
                .iter()
                .find(|entry| entry.name == "dispatch.read")
                .unwrap()
                .in_flight,
            1
        );
        assert!(!held.application_quiescent);
        fixture.read_release.notify_one();
        let mut data = [0; 15];
        assert_eq!(
            binary::read_result(&mut recv, 2, true, &mut data, 0)
                .await
                .unwrap()
                .unwrap(),
            15
        );
        assert_eq!(&data, b"private-payload");
        recv.read_to_end(0).await.unwrap();
        assert!(matches!(
            fixture
                .control(
                    3,
                    "ungranted-drive",
                    Operation {
                        name: OperationName::Stat,
                        body: json!({"path":"/private-file"})
                    }
                )
                .await,
            Message::Response { result: Err(_), .. }
        ));
        let after = fixture.close().await;
        assert_eq!(entry(&after, "request.application"), (3, 2, 1, 0));
        assert_eq!(entry(&after, "dispatch.read"), (1, 1, 0, 0));
        assert_eq!(entry(&after, "dispatch.control"), (2, 1, 1, 0));
        assert!(after.complete && after.application_quiescent);
        assert_private_free(&after);
    }

    #[tokio::test]
    async fn closing_connection_cancels_held_dispatch_and_releases_all_gauges() {
        let fixture = Fixture::new().await;
        fixture.established().await;
        let handle = fixture.open().await;
        let (mut send, _recv) = fixture.connection.open_bi().await.unwrap();
        binary::write_request(
            &mut send,
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
        send.finish().unwrap();
        signal(&fixture.read_entered).await;
        let after = fixture.close().await;
        assert_eq!(entry(&after, "dispatch.read"), (1, 0, 0, 1));
        assert_eq!(entry(&after, "request.application"), (2, 1, 0, 1));
        assert!(after.complete && after.application_quiescent);
        assert!(after.transport.active.is_empty());
    }

    #[tokio::test]
    async fn denied_authentication_retires_transport_without_dispatch() {
        let fixture = Fixture::new().await;
        let (_, mut recv) = fixture.hello("deny-secret-bearer").await;
        assert!(
            tokio::time::timeout(Duration::from_secs(3), read_frame(&mut recv))
                .await
                .unwrap()
                .is_err()
        );
        let after = fixture.close().await;
        assert_eq!(entry(&after, "auth.authenticate"), (1, 0, 1, 0));
        assert_eq!(entry(&after, "handshake.application"), (1, 0, 1, 0));
        assert_eq!(entry(&after, "dispatch.control").0, 0);
        assert_eq!(after.transport.retired_connections, 1);
        assert!(after.complete && after.application_quiescent);
    }

    #[tokio::test]
    async fn reconnect_increases_connection_id_and_keeps_retired_bytes() {
        let fixture = Fixture::new().await;
        fixture.established().await;
        let first = fixture.observer.snapshot();
        assert_eq!(first.transport.active[0].id, 1);
        let first_rx = first.transport.observed_total.udp_rx_bytes;
        assert!(first_rx > 0);
        fixture.connection.close(0_u32.into(), b"first done");
        let retired = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = fixture.observer.snapshot();
                if snapshot.transport.retired_connections == 1 && snapshot.application_quiescent {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(retired.transport.active.is_empty());
        assert!(retired.transport.retired.udp_rx_bytes >= first_rx);
        let second = fixture
            .endpoint
            .connect(fixture.server.local_addr(), "localhost")
            .unwrap()
            .await
            .unwrap();
        let (mut send, mut recv) = second.open_bi().await.unwrap();
        write_frame(
            &mut send,
            &Message::ClientHello {
                version: 2,
                partition_id: "private-partition".into(),
                bearer: "success-secret-bearer".into(),
            },
        )
        .await
        .unwrap();
        send.finish().unwrap();
        assert!(matches!(
            read_frame(&mut recv).await.unwrap(),
            Message::ServerHello { .. }
        ));
        recv.read_to_end(0).await.unwrap();
        let reconnected = fixture.observer.snapshot();
        assert_eq!(reconnected.transport.active.len(), 1);
        assert_eq!(reconnected.transport.active[0].id, 2);
        assert_eq!(reconnected.transport.registered_connections, 2);
        assert_eq!(reconnected.transport.retired_connections, 1);
        assert_eq!(
            reconnected.transport.retired.udp_rx_bytes,
            retired.transport.retired.udp_rx_bytes
        );
        assert!(
            reconnected.transport.observed_total.udp_rx_bytes
                > retired.transport.observed_total.udp_rx_bytes
        );
        second.close(0_u32.into(), b"second done");
        let after = fixture.close().await;
        assert_eq!(after.transport.retired_connections, 2);
        assert!(after.transport.active.is_empty());
        assert!(after.transport.registry_complete);
        assert_eq!(after.transport.unobserved_connections, 0);
        assert_eq!(after.transport.missing_final_samples, 0);
        assert!(after.complete && after.application_quiescent);
    }

    #[tokio::test]
    async fn deadline_no_hello_is_timeout() {
        let fixture = Fixture::new().await;
        tokio::time::timeout(Duration::from_secs(12), fixture.connection.closed())
            .await
            .unwrap();
        let after = fixture.close().await;
        let handshake = after
            .entries
            .iter()
            .find(|entry| entry.name == "handshake.application")
            .unwrap();
        assert_eq!(
            handshake.timeout, 1,
            "application hello deadline was not classified timeout"
        );
        assert_eq!(handshake.error, 0);
        assert_eq!(entry(&after, "auth.authenticate").0, 0);
        assert!(after.complete && after.application_quiescent);
    }

    #[tokio::test]
    async fn deadline_request_fin_is_timeout() {
        let fixture = Fixture::new().await;
        fixture.established().await;
        let (mut send, _recv) = fixture.connection.open_bi().await.unwrap();
        write_frame(
            &mut send,
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
        // Hold this complete framed RPC's stream open: dispatch must wait for
        // the unchanged 10-second server FIN deadline, then deny the stream.
        tokio::time::timeout(Duration::from_secs(12), fixture.connection.closed())
            .await
            .unwrap();
        drop(send);
        let after = fixture.close().await;
        let request = after
            .entries
            .iter()
            .find(|entry| entry.name == "request.application")
            .unwrap();
        assert_eq!(
            request.timeout, 1,
            "request FIN deadline was not classified timeout"
        );
        assert_eq!(request.error, 0);
        assert_eq!(entry(&after, "dispatch.control").0, 0);
        assert!(after.complete && after.application_quiescent);
    }
}
