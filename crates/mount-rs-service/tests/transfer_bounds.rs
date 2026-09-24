use async_trait::async_trait;
use mount_rs_core::{FsDriver, Loopback};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_remote_protocol::{
    Message, Operation, OperationName,
    binary::{self, IoRequest},
    read_frame, write_frame,
};
use mount_rs_service::{
    catalog::{
        CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        SqliteCatalog,
    },
    dispatch::{DriveDispatcher, SessionIdentity},
    server::{Authenticator, RemoteServer, RemoteServerOptions, RemoteTransferLimits},
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

struct Auth;
#[async_trait]
impl Authenticator for Auth {
    async fn authenticate(&self, token: &str, partition: &str) -> Result<SessionIdentity, ()> {
        if token != "token" || partition != "red" {
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
    _directory: tempfile::TempDir,
    server: RemoteServer,
    endpoint: quinn::Endpoint,
    connection: quinn::Connection,
    memory: Arc<MemoryFs>,
}
impl Fixture {
    async fn new(limits: RemoteTransferLimits) -> Self {
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
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = certificate.cert.der().clone();
        let key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
        let server = RemoteServer::bind_with_transfer_limits(
            "127.0.0.1:0".parse().unwrap(),
            vec![cert.clone()],
            key.into(),
            Arc::new(dispatcher),
            Arc::new(Auth),
            RemoteServerOptions::default(),
            limits,
        )
        .await
        .unwrap();
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
        let config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
        ));
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(config);
        let connection = endpoint
            .connect(server.local_addr(), "localhost")
            .unwrap()
            .await
            .unwrap();
        Self {
            _directory: directory,
            server,
            endpoint,
            connection,
            memory,
        }
    }
    async fn exchange(&self, message: Message) -> Message {
        let (mut send, mut recv) = self.connection.open_bi().await.unwrap();
        write_frame(&mut send, &message).await.unwrap();
        send.finish().unwrap();
        let response = read_frame(&mut recv).await.unwrap();
        recv.read_to_end(0).await.unwrap();
        response
    }
    async fn hello(&self) {
        assert!(
            matches!(self.exchange(Message::ClientHello{version:2,partition_id:"red".into(),bearer:"token".into()}).await,Message::ServerHello{version,..} if version==2)
        );
    }
    async fn open(&self) -> u64 {
        match self
            .exchange(Message::Request {
                request_id: 1,
                drive_id: "data".into(),
                operation: Operation {
                    name: OperationName::Open,
                    body: json!({"path":"/file","flags":"r+","mode":420}),
                },
            })
            .await
        {
            Message::Response {
                request_id: 1,
                result: Ok(v),
            } => v.as_u64().unwrap(),
            _ => panic!("open failed"),
        }
    }
    async fn close(self) {
        self.connection.close(0_u32.into(), b"done");
        self.endpoint.close(0_u32.into(), b"done");
        self.server.close().await;
    }
}

#[tokio::test]
async fn global_data_saturation_keeps_renewal_and_hello_responsive_and_cancel_releases() {
    let f = Fixture::new(RemoteTransferLimits {
        active_data_operations: 1,
        ..RemoteTransferLimits::default()
    })
    .await;
    f.hello().await;
    let handle = f.open().await;
    let request = IoRequest {
        drive_id: "data".into(),
        handle,
        position: Some(0),
    };
    let metadata = serde_json::to_vec(&request).unwrap();
    let (mut held_send, mut held_recv) = f.connection.open_bi().await.unwrap();
    // Declared raw bytes are charged before either metadata or payload arrives.
    let header = binary::Header::new(
        binary::Kind::Write,
        10,
        metadata.len(),
        binary::MAX_IO_BYTES,
        0,
    )
    .unwrap();
    held_send
        .write_all(&header.encode().unwrap())
        .await
        .unwrap();
    // Let the first accepted request reach admission; this RPC also proves the
    // separate control lane can progress while the data request awaits its body.
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            f.exchange(Message::Renew {
                bearer: "token".into()
            })
        )
        .await
        .unwrap(),
        Message::ServerHello { .. }
    ));

    let second = f
        .endpoint
        .connect(f.server.local_addr(), "localhost")
        .unwrap()
        .await
        .unwrap();
    let (mut hello_send, mut hello_recv) = second.open_bi().await.unwrap();
    write_frame(
        &mut hello_send,
        &Message::ClientHello {
            version: 2,
            partition_id: "red".into(),
            bearer: "token".into(),
        },
    )
    .await
    .unwrap();
    hello_send.finish().unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), read_frame(&mut hello_recv))
            .await
            .unwrap()
            .unwrap(),
        Message::ServerHello { .. }
    ));

    let (mut send, mut recv) = second.open_bi().await.unwrap();
    binary::write_request(&mut send, 11, &request, 0, Some(b"bad"))
        .await
        .unwrap();
    send.finish().unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(2),
            binary::read_result(&mut recv, 11, false, &mut [], 3)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(
        Loopback::from_arc(f.memory.clone())
            .read_file("/file")
            .await
            .unwrap(),
        b"old"
    );

    // Cancelling the body read must release both global operation and byte
    // admission. Observing the response FIN synchronizes completion of the task.
    held_send.reset(0_u32.into()).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), held_recv.read_to_end(0))
        .await
        .unwrap();
    let (mut send, mut recv) = second.open_bi().await.unwrap();
    // This request uses another connection's handle, so authorization denies it
    // after admission. A valid request on the original connection proves release.
    binary::write_request(&mut send, 12, &request, 0, Some(b"new"))
        .await
        .unwrap();
    send.finish().unwrap();
    assert!(
        binary::read_result(&mut recv, 12, false, &mut [], 3)
            .await
            .unwrap()
            .is_err()
    );
    let (mut send, mut recv) = f.connection.open_bi().await.unwrap();
    binary::write_request(&mut send, 13, &request, 0, Some(b"new"))
        .await
        .unwrap();
    send.finish().unwrap();
    assert_eq!(
        binary::read_result(&mut recv, 13, false, &mut [], 3)
            .await
            .unwrap()
            .unwrap(),
        3
    );
    assert_eq!(
        Loopback::from_arc(f.memory.clone())
            .read_file("/file")
            .await
            .unwrap(),
        b"new"
    );
    second.close(0_u32.into(), b"done");
    f.close().await;
}

#[tokio::test]
async fn valid_large_generic_control_and_max_raw_payload_share_capacity() {
    let f = Fixture::new(RemoteTransferLimits::default()).await;
    f.hello().await;
    let handle = f.open().await;
    // Generic control still permits the existing 8MiB envelope capacity;
    // unknown metadata fields do not shrink that public codec limit.
    let response = f
        .exchange(Message::Request {
            request_id: 20,
            drive_id: "data".into(),
            operation: Operation {
                name: OperationName::Stat,
                body: json!({"path":"/file","padding":"x".repeat(7 * 1024 * 1024)}),
            },
        })
        .await;
    assert!(matches!(response, Message::Response { result: Ok(_), .. }));
    let request = IoRequest {
        drive_id: "data".into(),
        handle,
        position: Some(0),
    };
    let bytes = vec![255; binary::MAX_IO_BYTES];
    let (mut send, mut recv) = f.connection.open_bi().await.unwrap();
    binary::write_request(&mut send, 21, &request, 0, Some(&bytes))
        .await
        .unwrap();
    send.finish().unwrap();
    assert_eq!(
        binary::read_result(&mut recv, 21, false, &mut [], bytes.len())
            .await
            .unwrap()
            .unwrap(),
        bytes.len()
    );
    f.close().await;
}

#[tokio::test]
async fn per_connection_data_saturation_preserves_a_stream_for_renewal() {
    let f = Fixture::new(RemoteTransferLimits::default()).await;
    f.hello().await;
    let handle = f.open().await;
    let request = IoRequest {
        drive_id: "data".into(),
        handle,
        position: Some(0),
    };
    let metadata = serde_json::to_vec(&request).unwrap();
    let mut held = Vec::new();
    for id in 100..132 {
        let (mut send, recv) = tokio::time::timeout(Duration::from_secs(2), f.connection.open_bi())
            .await
            .unwrap()
            .unwrap();
        let header = binary::Header::new(
            binary::Kind::Write,
            id,
            metadata.len(),
            binary::MAX_IO_BYTES,
            0,
        )
        .unwrap();
        send.write_all(&header.encode().unwrap()).await.unwrap();
        held.push((send, recv));
    }
    // The last transport stream must reject extra data without waiting for a
    // data slot, returning stream credit so renewal can open and complete.
    let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(2), f.connection.open_bi())
        .await
        .unwrap()
        .unwrap();
    binary::write_request(&mut send, 132, &request, 0, Some(b"bad"))
        .await
        .unwrap();
    send.finish().unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        binary::read_result(&mut recv, 132, false, &mut [], 3),
    )
    .await
    .unwrap();
    assert!(
        result.is_err(),
        "extra data must fail admission before dispatch"
    );
    let _ = recv.read_to_end(0).await;
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            f.exchange(Message::Renew {
                bearer: "token".into()
            }),
        )
        .await
        .unwrap(),
        Message::ServerHello { .. }
    ));
    assert_eq!(
        Loopback::from_arc(f.memory.clone())
            .read_file("/file")
            .await
            .unwrap(),
        b"old"
    );
    for (mut send, _) in held {
        let _ = send.reset(0_u32.into());
    }
    f.close().await;
}
