use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, FileHandle, FsDriver, Loopback, Result as FsResult, Stats,
};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_remote_client::{
    connection::{ClientError, ConnectionTransport, RemoteConnection},
    credentials::CredentialSource,
};
use mount_rs_remote_protocol::{Operation, OperationName};
use mount_rs_service::{
    catalog::{
        CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        SqliteCatalog,
    },
    dispatch::{DriveDispatcher, SessionIdentity},
    server::{Authenticator, RemoteServer},
    websocket::WebSocketServer,
};
use serde_json::json;

struct TestAuthenticator;

#[async_trait]
impl Authenticator for TestAuthenticator {
    async fn authenticate(&self, bearer: &str, _: &str) -> Result<SessionIdentity, ()> {
        if bearer != "dmFsaWQ.e30.c2ln" {
            return Err(());
        }
        Ok(SessionIdentity {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            issuer: "https://issuer.example.com".into(),
            subject: "workload".into(),
            signing_algorithm: "ES256".into(),
            claims: json!({"aud":"mount-rs","repository_id":"repo-1"}),
            expires_at: i64::MAX,
        })
    }
}

struct CommitThenWait {
    memory: MemoryFs,
    commits: Arc<AtomicUsize>,
    committed: Arc<tokio::sync::Notify>,
    closes: Arc<AtomicUsize>,
}

struct CommitHandle {
    handle: Arc<dyn FileHandle>,
    commits: Arc<AtomicUsize>,
    committed: Arc<tokio::sync::Notify>,
    closes: Arc<AtomicUsize>,
}
#[async_trait]
impl FileHandle for CommitHandle {
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> FsResult<usize> {
        self.handle.read(buffer, position).await
    }
    async fn stat(&self) -> FsResult<Stats> {
        self.handle.stat().await
    }
    async fn truncate(&self, length: u64) -> FsResult<()> {
        self.handle.truncate(length).await
    }
    async fn close(&self) -> FsResult<()> {
        self.handle.close().await?;
        self.closes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn write(&self, data: &[u8], position: Option<u64>) -> FsResult<usize> {
        self.handle.write(data, position).await?;
        self.commits.fetch_add(1, Ordering::SeqCst);
        self.committed.notify_one();
        std::future::pending().await
    }
}

#[async_trait]
impl FsDriver for CommitThenWait {
    fn capabilities(&self) -> Capabilities {
        self.memory.capabilities()
    }
    async fn stat(&self, path: &str) -> FsResult<Stats> {
        self.memory.stat(path).await
    }
    async fn readdir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        self.memory.readdir(path).await
    }
    async fn open(&self, path: &str, flags: &str, mode: u32) -> FsResult<Arc<dyn FileHandle>> {
        Ok(Arc::new(CommitHandle {
            handle: self.memory.open(path, flags, mode).await?,
            commits: self.commits.clone(),
            committed: self.committed.clone(),
            closes: self.closes.clone(),
        }))
    }
    async fn write_file(&self, path: &str, data: &[u8]) -> FsResult<()> {
        self.memory.write_file(path, data).await?;
        self.commits.fetch_add(1, Ordering::SeqCst);
        self.committed.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn quic_disconnect_after_backend_commit_does_not_replay_uncertain_write() {
    uncertain_write(false, false, false).await;
}
#[tokio::test]
async fn binary_disconnect_after_backend_commit_does_not_replay_uncertain_handle_write() {
    uncertain_write(true, false, false).await;
}
#[tokio::test]
async fn websocket_uncertain_generic_write_commits_once_without_replay() {
    uncertain_write(false, true, false).await;
}
#[tokio::test]
async fn websocket_uncertain_binary_write_commits_once_without_replay() {
    uncertain_write(true, true, false).await;
}
#[tokio::test]
async fn cancelled_websocket_write_fails_closed_without_replay() {
    uncertain_write(true, true, true).await;
}
enum TestServer {
    Quic(RemoteServer),
    WebSocket(WebSocketServer),
}
impl TestServer {
    fn local_addr(&self) -> std::net::SocketAddr {
        match self {
            Self::Quic(s) => s.local_addr(),
            Self::WebSocket(s) => s.local_addr(),
        }
    }
    async fn close(self) {
        match self {
            Self::Quic(s) => s.close().await,
            Self::WebSocket(s) => s.close().await,
        }
    }
}
async fn uncertain_write(binary: bool, websocket: bool, cancel_caller: bool) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
    );
    let definition = json!({"kind":"memory"});
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "data".into(),
                DriveDefinition {
                    driver: definition.clone(),
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
    let backend = Arc::new(CommitThenWait {
        memory: MemoryFs::new(MemoryOptions::default()),
        commits: Arc::new(AtomicUsize::new(0)),
        committed: Arc::new(tokio::sync::Notify::new()),
        closes: Arc::new(AtomicUsize::new(0)),
    });
    let mut dispatcher = DriveDispatcher::new(catalog);
    dispatcher
        .register_definition("red", "data", definition, backend.clone())
        .unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = certificate.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let dispatcher = Arc::new(dispatcher);
    let authenticator = Arc::new(TestAuthenticator);
    let server = if websocket {
        TestServer::WebSocket(
            WebSocketServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                vec![cert.clone()],
                key.into(),
                dispatcher,
                authenticator,
            )
            .await
            .unwrap(),
        )
    } else {
        TestServer::Quic(
            RemoteServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                vec![cert.clone()],
                key.into(),
                dispatcher,
                authenticator,
            )
            .await
            .unwrap(),
        )
    };
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let token_path = directory.path().join("token");
    std::fs::write(&token_path, "dmFsaWQ.e30.c2ln").unwrap();
    let connection = RemoteConnection::connect_with_transport(
        server.local_addr(),
        "localhost",
        roots,
        "red".into(),
        CredentialSource::File(token_path),
        if websocket {
            ConnectionTransport::WebSocket(server.local_addr())
        } else {
            ConnectionTransport::Quic
        },
    )
    .await
    .unwrap();
    assert_eq!(connection.protocol_version(), 2);
    let handle = if binary {
        Some(
            connection
                .request(
                    "data",
                    Operation {
                        name: OperationName::Open,
                        body: json!({"path":"/once","flags":"w+","mode":420}),
                    },
                )
                .await
                .unwrap()
                .as_u64()
                .unwrap(),
        )
    } else {
        None
    };
    let request = tokio::spawn({
        let connection = connection.clone();
        async move {
            if let Some(handle) = handle {
                connection
                    .write("data", handle, Some(0), &[1, 2, 3])
                    .await
                    .map(|_| json!(null))
            } else {
                connection
                    .request(
                        "data",
                        Operation {
                            name: OperationName::Write,
                            body: json!({"path":"/once","data":[1,2,3]}),
                        },
                    )
                    .await
            }
        }
    });
    tokio::time::timeout(Duration::from_secs(3), backend.committed.notified())
        .await
        .unwrap();
    assert_eq!(
        Loopback::from_arc(backend.clone())
            .read_file("/once")
            .await
            .unwrap(),
        [1, 2, 3]
    );
    let closes_before_shutdown = backend.closes.load(Ordering::SeqCst);
    if cancel_caller {
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
    } else {
        connection.close();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(3), request)
                .await
                .unwrap()
                .unwrap(),
            Err(ClientError::Transport | ClientError::Protocol)
        ));
    }
    assert_eq!(backend.commits.load(Ordering::SeqCst), 1);
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(3),
            connection.request(
                "data",
                Operation {
                    name: OperationName::Stat,
                    body: json!({"path":"/"})
                }
            )
        )
        .await
        .expect("cancelled socket must fail closed immediately"),
        Err(ClientError::Transport)
    ));
    assert_eq!(backend.commits.load(Ordering::SeqCst), 1);
    server.close().await;
    assert_eq!(
        backend.closes.load(Ordering::SeqCst),
        closes_before_shutdown + usize::from(binary),
        "session handle cleanup must finish before server shutdown returns"
    );
}
