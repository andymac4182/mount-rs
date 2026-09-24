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
    connection::{ClientError, RemoteConnection},
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
    commits: AtomicUsize,
    committed: tokio::sync::Notify,
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
        self.memory.open(path, flags, mode).await
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
        commits: AtomicUsize::new(0),
        committed: tokio::sync::Notify::new(),
    });
    let mut dispatcher = DriveDispatcher::new(catalog);
    dispatcher
        .register_definition("red", "data", definition, backend.clone())
        .unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = certificate.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let server = RemoteServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        vec![cert.clone()],
        key.into(),
        Arc::new(dispatcher),
        Arc::new(TestAuthenticator),
    )
    .await
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let token_path = directory.path().join("token");
    std::fs::write(&token_path, "dmFsaWQ.e30.c2ln").unwrap();
    let connection = RemoteConnection::connect(
        server.local_addr(),
        "localhost",
        roots,
        "red".into(),
        CredentialSource::File(token_path),
    )
    .await
    .unwrap();
    let request = tokio::spawn({
        let connection = connection.clone();
        async move {
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
    connection.close();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), request)
            .await
            .unwrap()
            .unwrap(),
        Err(ClientError::Transport)
    ));
    assert_eq!(backend.commits.load(Ordering::SeqCst), 1);
    assert!(matches!(
        connection
            .request(
                "data",
                Operation {
                    name: OperationName::Stat,
                    body: json!({"path":"/"})
                }
            )
            .await,
        Err(ClientError::Transport)
    ));
    assert_eq!(backend.commits.load(Ordering::SeqCst), 1);
    server.close().await;
}
