use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use mount_rs_core::{Capabilities, DirEntry, FileHandle, FsDriver, Result as FsResult, Stats};
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

struct Fixture {
    _directory: tempfile::TempDir,
    server: RemoteServer,
    endpoint: quinn::Endpoint,
    catalog: Arc<SqliteCatalog>,
}

struct TestAuthenticator;

#[async_trait]
impl Authenticator for TestAuthenticator {
    async fn authenticate(&self, token: &str, _: &str) -> Result<SessionIdentity, ()> {
        if !matches!(token, "valid" | "expired") {
            return Err(());
        }
        Ok(SessionIdentity {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            issuer: "https://issuer.example.com".into(),
            subject: "workload".into(),
            signing_algorithm: "ES256".into(),
            claims: json!({"aud":"mount-rs","repository_id":"repo-1"}),
            expires_at: if token == "expired" { 0 } else { i64::MAX },
        })
    }
}

impl Fixture {
    async fn new() -> Self {
        Self::with_driver(Arc::new(MemoryFs::new(MemoryOptions::default()))).await
    }

    async fn with_driver(driver: Arc<dyn FsDriver>) -> Self {
        Self::with_driver_and_auth(driver, Arc::new(TestAuthenticator)).await
    }

    async fn with_driver_and_auth(
        driver: Arc<dyn FsDriver>,
        authenticator: Arc<dyn Authenticator>,
    ) -> Self {
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
        let mut dispatcher = DriveDispatcher::new(catalog.clone());
        dispatcher
            .register_definition("red", "data", json!({"kind":"memory"}), driver)
            .unwrap();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = certificate.cert.der().clone();
        let key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
        let server = RemoteServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            vec![cert.clone()],
            key.into(),
            Arc::new(dispatcher),
            authenticator,
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
        tls.alpn_protocols = vec![b"mount-rs/1".to_vec()];
        let config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
        ));
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(config);
        Self {
            _directory: directory,
            server,
            endpoint,
            catalog,
        }
    }

    async fn connect(&self) -> quinn::Connection {
        self.endpoint
            .connect(self.server.local_addr(), "localhost")
            .unwrap()
            .await
            .unwrap()
    }
}

async fn hello(connection: &quinn::Connection, version: u16, bearer: &str) -> Result<Message, ()> {
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::ClientHello {
            version,
            partition_id: "red".into(),
            bearer: bearer.into(),
        },
    )
    .await
    .unwrap();
    send.finish().unwrap();
    read_frame(&mut recv).await.map_err(|_| ())
}

async fn closed(connection: &quinn::Connection) {
    tokio::time::timeout(Duration::from_secs(3), connection.closed())
        .await
        .unwrap();
}

#[tokio::test]
async fn rejects_unsupported_version_and_malformed_handshake_frame() {
    let fixture = Fixture::new().await;
    let wrong_version = fixture.connect().await;
    assert!(
        hello(&wrong_version, PROTOCOL_VERSION + 1, "valid")
            .await
            .is_err()
    );
    closed(&wrong_version).await;

    let invalid_json = fixture.connect().await;
    let (mut send, _recv) = invalid_json.open_bi().await.unwrap();
    send.write_all(&4_u32.to_be_bytes()).await.unwrap();
    send.write_all(b"nope").await.unwrap();
    send.finish().unwrap();
    closed(&invalid_json).await;

    let zero_length = fixture.connect().await;
    let (mut send, _recv) = zero_length.open_bi().await.unwrap();
    send.write_all(&0_u32.to_be_bytes()).await.unwrap();
    send.finish().unwrap();
    closed(&zero_length).await;

    let trailing = fixture.connect().await;
    let (mut send, _recv) = trailing.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::ClientHello {
            version: PROTOCOL_VERSION,
            partition_id: "red".into(),
            bearer: "valid".into(),
        },
    )
    .await
    .unwrap();
    send.write_all(b"trailing").await.unwrap();
    send.finish().unwrap();
    closed(&trailing).await;

    fixture.server.close().await;
}

#[tokio::test]
async fn invalid_request_id_closes_session_and_expired_identity_denies_operation() {
    let fixture = Fixture::new().await;
    let invalid_id = fixture.connect().await;
    assert!(matches!(
        hello(&invalid_id, PROTOCOL_VERSION, "valid").await,
        Ok(Message::ServerHello { .. })
    ));
    let (mut send, _recv) = invalid_id.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::Request {
            request_id: 0,
            drive_id: "data".into(),
            operation: Operation {
                name: OperationName::Stat,
                body: json!({"path":"/"}),
            },
        },
    )
    .await
    .unwrap();
    send.finish().unwrap();
    closed(&invalid_id).await;

    let expired = fixture.connect().await;
    assert!(matches!(
        hello(&expired, PROTOCOL_VERSION, "expired").await,
        Ok(Message::ServerHello { .. })
    ));
    let (mut send, mut recv) = expired.open_bi().await.unwrap();
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
    send.finish().unwrap();
    assert!(
        matches!(read_frame(&mut recv).await.unwrap(), Message::Response { request_id: 1, result: Err(error) } if error.code == "EACCES")
    );
    // Hard expiry terminally invalidates the handle session. A later renewal
    // must require reconnection rather than advertising a partly usable session.
    let (mut send, _recv) = expired.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::Renew {
            bearer: "valid".into(),
        },
    )
    .await
    .unwrap();
    send.finish().unwrap();
    closed(&expired).await;

    fixture.server.close().await;
}

#[tokio::test]
async fn catalog_backend_retarget_rejects_existing_registered_driver() {
    let fixture = Fixture::new().await;
    let connection = fixture.connect().await;
    assert!(matches!(
        hello(&connection, PROTOCOL_VERSION, "valid").await,
        Ok(Message::ServerHello { .. })
    ));
    let stat = |request_id| Message::Request {
        request_id,
        drive_id: "data".into(),
        operation: Operation {
            name: OperationName::Stat,
            body: json!({"path":"/"}),
        },
    };
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(&mut send, &stat(1)).await.unwrap();
    send.finish().unwrap();
    assert!(matches!(
        read_frame(&mut recv).await.unwrap(),
        Message::Response {
            request_id: 1,
            result: Ok(_)
        }
    ));

    let mut next = fixture.catalog.load_current().await.unwrap();
    next.partitions
        .get_mut("red")
        .unwrap()
        .drives
        .get_mut("data")
        .unwrap()
        .driver = json!({"kind":"memory","backend":"replacement"});
    fixture.catalog.compare_and_swap(1, next).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(&mut send, &stat(2)).await.unwrap();
    send.finish().unwrap();
    assert!(
        matches!(read_frame(&mut recv).await.unwrap(), Message::Response { request_id: 2, result: Err(error) } if error.code == "ESTALE")
    );
    connection.close(0_u32.into(), b"done");
    fixture.server.close().await;
}

#[tokio::test]
async fn malformed_request_frame_is_isolated_to_its_stream() {
    let fixture = Fixture::new().await;
    let connection = fixture.connect().await;
    assert!(matches!(
        hello(&connection, PROTOCOL_VERSION, "valid").await,
        Ok(Message::ServerHello { .. })
    ));
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    send.write_all(&4_u32.to_be_bytes()).await.unwrap();
    send.write_all(b"nope").await.unwrap();
    send.finish().unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), read_frame(&mut recv))
            .await
            .unwrap()
            .is_err()
    );

    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::Request {
            request_id: 9,
            drive_id: "data".into(),
            operation: Operation {
                name: OperationName::Stat,
                body: json!({"path":"/"}),
            },
        },
    )
    .await
    .unwrap();
    send.finish().unwrap();
    assert!(matches!(
        read_frame(&mut recv).await.unwrap(),
        Message::Response {
            request_id: 9,
            result: Ok(_)
        }
    ));
    connection.close(0_u32.into(), b"done");
    fixture.server.close().await;
}

struct DelayedOpen {
    memory: MemoryFs,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    closes: Arc<AtomicUsize>,
}

struct CountedHandle(Arc<AtomicUsize>);

#[async_trait]
impl FileHandle for CountedHandle {
    async fn read(&self, _: &mut [u8], _: Option<u64>) -> FsResult<usize> {
        Ok(0)
    }
    async fn write(&self, _: &[u8], _: Option<u64>) -> FsResult<usize> {
        Ok(0)
    }
    async fn stat(&self) -> FsResult<Stats> {
        Err(mount_rs_core::FsError::enosys("stat"))
    }
    async fn truncate(&self, _: u64) -> FsResult<()> {
        Ok(())
    }
    async fn close(&self) -> FsResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl FsDriver for DelayedOpen {
    fn capabilities(&self) -> Capabilities {
        self.memory.capabilities()
    }
    async fn stat(&self, path: &str) -> FsResult<Stats> {
        self.memory.stat(path).await
    }
    async fn readdir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        self.memory.readdir(path).await
    }
    async fn open(&self, _: &str, _: &str, _: u32) -> FsResult<Arc<dyn FileHandle>> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(Arc::new(CountedHandle(self.closes.clone())))
    }
}

#[tokio::test]
async fn delayed_open_after_catalog_revision_change_closes_backend_handle() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let closes = Arc::new(AtomicUsize::new(0));
    let fixture = Fixture::with_driver(Arc::new(DelayedOpen {
        memory: MemoryFs::new(MemoryOptions::default()),
        entered: entered.clone(),
        release: release.clone(),
        closes: closes.clone(),
    }))
    .await;
    let connection = fixture.connect().await;
    assert!(matches!(
        hello(&connection, PROTOCOL_VERSION, "valid").await,
        Ok(Message::ServerHello { .. })
    ));
    let (mut old_send, mut old_recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut old_send,
        &Message::Request {
            request_id: 1,
            drive_id: "data".into(),
            operation: Operation {
                name: OperationName::Open,
                body: json!({"path":"/file","flags":"r","mode":0}),
            },
        },
    )
    .await
    .unwrap();
    old_send.finish().unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();

    let next = fixture.catalog.load_current().await.unwrap();
    fixture.catalog.compare_and_swap(1, next).await.unwrap();
    let (mut fresh_send, mut fresh_recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut fresh_send,
        &Message::Request {
            request_id: 2,
            drive_id: "data".into(),
            operation: Operation {
                name: OperationName::Stat,
                body: json!({"path":"/"}),
            },
        },
    )
    .await
    .unwrap();
    fresh_send.finish().unwrap();
    assert!(matches!(
        read_frame(&mut fresh_recv).await.unwrap(),
        Message::Response {
            request_id: 2,
            result: Ok(_)
        }
    ));

    release.notify_one();
    assert!(
        matches!(read_frame(&mut old_recv).await.unwrap(), Message::Response { request_id: 1, result: Err(error) } if error.code == "ESTALE")
    );
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    connection.close(0_u32.into(), b"done");
    fixture.server.close().await;
}

struct BlockedRenew {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Authenticator for BlockedRenew {
    async fn authenticate(&self, token: &str, partition_id: &str) -> Result<SessionIdentity, ()> {
        if token == "valid" {
            self.entered.notify_one();
            self.release.notified().await;
        }
        TestAuthenticator.authenticate(token, partition_id).await
    }
}

#[tokio::test]
async fn expired_request_during_blocked_renew_terminally_invalidates_session() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let fixture = Fixture::with_driver_and_auth(
        Arc::new(MemoryFs::new(MemoryOptions::default())),
        Arc::new(BlockedRenew {
            entered: entered.clone(),
            release: release.clone(),
        }),
    )
    .await;
    let connection = fixture.connect().await;
    assert!(matches!(
        hello(&connection, PROTOCOL_VERSION, "expired").await,
        Ok(Message::ServerHello { .. })
    ));

    let (mut renew_send, mut renew_recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut renew_send,
        &Message::Renew {
            bearer: "valid".into(),
        },
    )
    .await
    .unwrap();
    renew_send.finish().unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();

    let (mut request_send, mut request_recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut request_send,
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
    request_send.finish().unwrap();
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(3), read_frame(&mut request_recv)).await.unwrap().unwrap(), Message::Response { request_id: 1, result: Err(error) } if error.code == "EACCES")
    );

    release.notify_one();
    // Once a request has observed hard expiry, a concurrent renewal cannot
    // publish a fresh session identity on the same connection.
    let renewal_result = tokio::time::timeout(Duration::from_secs(3), read_frame(&mut renew_recv))
        .await
        .unwrap();
    assert!(
        !matches!(renewal_result, Ok(Message::ServerHello { .. })),
        "renewal revived an expired session"
    );
    closed(&connection).await;
    fixture.server.close().await;
}
