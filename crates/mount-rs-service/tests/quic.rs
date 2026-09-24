use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use async_trait::async_trait;
use mount_rs_core::FsDriver;
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_remote_protocol::{
    Message, Operation, OperationName, PROTOCOL_VERSION, read_frame, write_frame,
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

struct TestAuthenticator;

#[async_trait]
impl Authenticator for TestAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<SessionIdentity, ()> {
        if token != "test-token" {
            return Err(());
        }
        Ok(SessionIdentity {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            issuer: "https://issuer.example.com".into(),
            subject: "workload-1".into(),
            claims: json!({"repository_id":"repo-1"}),
            expires_at: i64::MAX,
        })
    }
}

#[tokio::test]
async fn quic_session_routes_authorized_drive_and_rejects_other_partition() {
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
        "reader".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            drives: BTreeMap::from([("data".into(), Permission::Read)]),
            claim_conditions: BTreeMap::from([("/repository_id".into(), "repo-1".into())]),
        },
    );
    catalog.compare_and_swap(0, snapshot).await.unwrap();
    let mut dispatcher = DriveDispatcher::new(catalog);
    dispatcher
        .register(
            "red",
            "data",
            Arc::new(MemoryFs::new(MemoryOptions::default())) as Arc<dyn FsDriver>,
        )
        .unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = certificate.cert.der().clone();
    let key_der =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let server = RemoteServer::bind(
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        vec![cert_der.clone()],
        key_der.into(),
        Arc::new(dispatcher),
        Arc::new(TestAuthenticator),
    )
    .await
    .unwrap();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der).unwrap();
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"mount-rs/1".to_vec()];
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
    ));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_config);
    let connection = endpoint
        .connect(server.local_addr(), "localhost")
        .unwrap()
        .await
        .unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::ClientHello {
            version: PROTOCOL_VERSION,
            partition_id: "red".into(),
            bearer: "test-token".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame(&mut recv).await.unwrap(),
        Message::ServerHello {
            version: PROTOCOL_VERSION,
            ..
        }
    ));
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::Request {
            request_id: 1,
            drive_id: "data".into(),
            operation: Operation {
                name: OperationName::Stat,
                body: json!({"path":"/"}),
            },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame(&mut recv).await.unwrap(),
        Message::Response {
            request_id: 1,
            result: Ok(_)
        }
    ));
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::Request {
            request_id: 2,
            drive_id: "blue/data".into(),
            operation: Operation {
                name: OperationName::Stat,
                body: json!({"path":"/"}),
            },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame(&mut recv).await.unwrap(),
        Message::Response {
            request_id: 2,
            result: Err(_)
        }
    ));
    server.close().await;
}
