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
    server::{Authenticator, RemoteServer},
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
    async fn new() -> Self {
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
        let server = RemoteServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            vec![cert.clone()],
            key.into(),
            Arc::new(dispatcher),
            Arc::new(Auth),
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
async fn binary_client_reads_writes_and_renews_same_handle() {
    let f = Fixture::new().await;
    f.hello().await;
    let handle = f.open().await;
    let request: IoRequest = IoRequest {
        drive_id: "data".into(),
        handle,
        position: Some(0),
    };
    let (mut send, mut recv) = f.connection.open_bi().await.unwrap();
    binary::write_request(&mut send, 2, &request, 0, Some(&[0, 128, 255]))
        .await
        .unwrap();
    send.finish().unwrap();
    assert_eq!(
        binary::read_result(&mut recv, 2, false, &mut [], 3)
            .await
            .unwrap()
            .unwrap(),
        3
    );
    assert!(matches!(
        f.exchange(Message::Renew {
            bearer: "token".into()
        })
        .await,
        Message::ServerHello { version: 2, .. }
    ));
    let (mut send, mut recv) = f.connection.open_bi().await.unwrap();
    binary::write_request(&mut send, 3, &request, 3, None)
        .await
        .unwrap();
    send.finish().unwrap();
    let mut data = [0; 3];
    assert_eq!(
        binary::read_result(&mut recv, 3, true, &mut data, 0)
            .await
            .unwrap()
            .unwrap(),
        3
    );
    assert_eq!(data, [0, 128, 255]);
    f.close().await;
}
#[tokio::test]
async fn alpn_v2_rejects_hello_v1_without_codec_guessing() {
    let f = Fixture::new().await;
    let (mut send, mut recv) = f.connection.open_bi().await.unwrap();
    binary::write_control(
        &mut send,
        &Message::ClientHello {
            version: 1,
            partition_id: "red".into(),
            bearer: "token".into(),
        },
    )
    .await
    .unwrap();
    send.finish().unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), binary::read_control(&mut recv))
            .await
            .unwrap()
            .is_err()
    );
    f.close().await;
}
#[tokio::test]
async fn v2_trailing_bytes_are_rejected_before_backend_write() {
    let f = Fixture::new().await;
    f.hello().await;
    let handle = f.open().await;
    let (mut send, mut recv) = f.connection.open_bi().await.unwrap();
    binary::write_request(
        &mut send,
        2,
        &IoRequest {
            drive_id: "data",
            handle,
            position: Some(0),
        },
        0,
        Some(b"new"),
    )
    .await
    .unwrap();
    send.write_all(b"extra").await.unwrap();
    send.finish().unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(3),
            binary::read_result(&mut recv, 2, false, &mut [], 3)
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
    f.close().await;
}
