#![cfg(unix)]

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use mount_rs_core::{FileHandle, FsDriver};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_remote_protocol::{
    Message, Operation, OperationName, PROTOCOL_VERSION,
    binary::{self, IoRequest},
    read_frame, write_frame,
};
use mount_rs_service::{
    auth::{AuthError, CatalogAuthenticator, Jwk, OidcKeySource, OidcVerifier},
    catalog::{
        CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        SqliteCatalog,
    },
    dispatch::DriveDispatcher,
    server::RemoteServer,
};
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

struct Keys(Jwk);

#[async_trait]
impl OidcKeySource for Keys {
    async fn fetch(&self, issuer: &str, audiences: &[String]) -> Result<OidcVerifier, AuthError> {
        OidcVerifier::new(issuer, audiences, vec![self.0.clone()])
    }
}

struct CountedFs {
    memory: Arc<MemoryFs>,
    reads: Arc<AtomicU64>,
}

struct CountedHandle {
    inner: Arc<dyn FileHandle>,
    reads: Arc<AtomicU64>,
}

#[async_trait]
impl FileHandle for CountedHandle {
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> mount_rs_core::Result<usize> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read(buffer, position).await
    }
    async fn write(&self, buffer: &[u8], position: Option<u64>) -> mount_rs_core::Result<usize> {
        self.inner.write(buffer, position).await
    }
    async fn stat(&self) -> mount_rs_core::Result<mount_rs_core::Stats> {
        self.inner.stat().await
    }
    async fn truncate(&self, size: u64) -> mount_rs_core::Result<()> {
        self.inner.truncate(size).await
    }
    async fn close(&self) -> mount_rs_core::Result<()> {
        self.inner.close().await
    }
}

#[async_trait]
impl FsDriver for CountedFs {
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
        Ok(Arc::new(CountedHandle {
            inner: self.memory.open(path, flags, mode).await?,
            reads: self.reads.clone(),
        }))
    }
}

fn signed_token() -> (Jwk, String) {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let key =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let point = key.public_key().as_ref();
    let jwk = Jwk::EcP256 {
        kid: "conditional-read-test".into(),
        x: URL_SAFE_NO_PAD.encode(&point[1..33]),
        y: URL_SAFE_NO_PAD.encode(&point[33..65]),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"conditional-read-test"}"#);
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "iss":"https://issuer.example.com", "aud":"mount-rs", "sub":"workload",
            "iat":now, "exp":now+600
        }))
        .unwrap(),
    );
    let input = format!("{header}.{claims}");
    let signature = key.sign(&rng, input.as_bytes()).unwrap();
    (
        jwk,
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature)),
    )
}

fn authority() -> CatalogSnapshot {
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
            claim_conditions: BTreeMap::from([("/sub".into(), "workload".into())]),
        },
    );
    snapshot
}

async fn read_on_open_handle(
    connection: &quinn::Connection,
    handle: u64,
    request_id: u64,
) -> Result<usize, String> {
    let (mut send, mut recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
    binary::write_request(
        &mut send,
        request_id,
        &IoRequest {
            drive_id: "data",
            handle,
            position: Some(0),
        },
        3,
        None,
    )
    .await
    .map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    let mut buffer = [0; 3];
    let result = binary::read_result(&mut recv, request_id, true, &mut buffer, 0)
        .await
        .map_err(|e| e.to_string())?;
    recv.read_to_end(0).await.map_err(|e| e.to_string())?;
    result.map_err(|error| error.code)
}

#[tokio::test]
#[ignore = "owned signed QUIC and SQLite control; requires MOUNT_RS_PROFILE_IO=1"]
async fn signed_open_read_revocation_is_visible_on_all_eight_handles() {
    assert!(mount_rs_core::diagnostics::profile::enabled());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = Arc::new(SqliteCatalog::open(&path).await.unwrap());
    catalog.compare_and_swap(0, authority()).await.unwrap();
    let memory = Arc::new(MemoryFs::new(MemoryOptions::default()));
    memory.write_file("/file", b"abc").await.unwrap();
    let reads = Arc::new(AtomicU64::new(0));
    let counted = Arc::new(CountedFs {
        memory,
        reads: reads.clone(),
    });
    let mut dispatcher = DriveDispatcher::new(catalog.clone());
    dispatcher
        .register_definition("red", "data", json!({"kind":"memory"}), counted)
        .unwrap();
    let (jwk, token) = signed_token();
    let authenticator = Arc::new(CatalogAuthenticator::with_key_source(
        catalog.clone(),
        Arc::new(Keys(jwk)),
    ));
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = certificate.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
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
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::ClientHello {
            version: PROTOCOL_VERSION,
            partition_id: "red".into(),
            bearer: token,
        },
    )
    .await
    .unwrap();
    send.finish().unwrap();
    assert!(matches!(
        read_frame(&mut recv).await.unwrap(),
        Message::ServerHello {
            version: PROTOCOL_VERSION,
            ..
        }
    ));
    recv.read_to_end(0).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &Message::Request {
            request_id: 1,
            drive_id: "data".into(),
            operation: Operation {
                name: OperationName::Open,
                body: json!({"path":"/file","flags":"r","mode":420}),
            },
        },
    )
    .await
    .unwrap();
    send.finish().unwrap();
    let handle = match read_frame(&mut recv).await.unwrap() {
        Message::Response {
            request_id: 1,
            result: Ok(value),
        } => value.as_u64().unwrap(),
        other => panic!("signed open failed: {other:?}"),
    };
    recv.read_to_end(0).await.unwrap();
    let before_warm = catalog.read_diagnostics().slot_observations;
    for _ in 0..8 {
        catalog.load_shared_current().await.unwrap();
    }
    let after_warm = catalog.read_diagnostics().slot_observations;
    assert!(
        after_warm
            .iter()
            .zip(before_warm)
            .all(|(after, before)| after > &before)
    );
    assert_eq!(
        read_on_open_handle(&connection, handle, 2).await.unwrap(),
        3
    );
    assert_eq!(reads.load(Ordering::Relaxed), 1);

    let mut revoked = authority();
    revoked.revision = 1;
    revoked.grants.clear();
    let outsider = Connection::open(&path).unwrap();
    outsider
        .execute(
            "UPDATE service_catalog SET document=?1 WHERE singleton=1",
            params![serde_json::to_vec(&revoked).unwrap()],
        )
        .unwrap();
    let before_denied = catalog.read_diagnostics().slot_observations;
    for request_id in 3..11 {
        assert_eq!(
            read_on_open_handle(&connection, handle, request_id)
                .await
                .unwrap_err(),
            "EACCES"
        );
    }
    let after_denied = catalog.read_diagnostics().slot_observations;
    assert!(
        after_denied
            .iter()
            .zip(before_denied)
            .all(|(after, before)| after > &before),
        "all eight actual pool handles must serve a denied RPC"
    );
    assert_eq!(
        reads.load(Ordering::Relaxed),
        1,
        "revoked RPC must never reach backend read"
    );

    // Exercise the same opened handle through the other authorization inputs.
    let mut restored = authority();
    restored.revision = 1;
    outsider
        .execute(
            "UPDATE service_catalog SET document=?1 WHERE singleton=1",
            params![serde_json::to_vec(&restored).unwrap()],
        )
        .unwrap();
    assert_eq!(
        read_on_open_handle(&connection, handle, 11).await.unwrap(),
        3
    );
    assert_eq!(reads.load(Ordering::Relaxed), 2);

    let mut changed_issuer = restored.clone();
    changed_issuer.issuer_policies.insert(
        "oidc".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["other"]}),
    );
    outsider
        .execute(
            "UPDATE service_catalog SET document=?1 WHERE singleton=1",
            params![serde_json::to_vec(&changed_issuer).unwrap()],
        )
        .unwrap();
    assert_eq!(
        read_on_open_handle(&connection, handle, 12)
            .await
            .unwrap_err(),
        "EACCES"
    );
    assert_eq!(reads.load(Ordering::Relaxed), 2);

    let mut changed_definition = restored.clone();
    changed_definition
        .partitions
        .get_mut("red")
        .unwrap()
        .drives
        .get_mut("data")
        .unwrap()
        .driver = json!({"kind":"memory","generation":2});
    outsider
        .execute(
            "UPDATE service_catalog SET document=?1 WHERE singleton=1",
            params![serde_json::to_vec(&changed_definition).unwrap()],
        )
        .unwrap();
    assert_eq!(
        read_on_open_handle(&connection, handle, 13)
            .await
            .unwrap_err(),
        "ESTALE"
    );
    assert_eq!(reads.load(Ordering::Relaxed), 2);

    outsider
        .execute(
            "UPDATE service_catalog SET document=?1 WHERE singleton=1",
            params![serde_json::to_vec(&restored).unwrap()],
        )
        .unwrap();
    let mut own_revocation = restored;
    own_revocation.grants.clear();
    assert_eq!(
        catalog.compare_and_swap(1, own_revocation).await.unwrap(),
        2
    );
    assert_eq!(
        read_on_open_handle(&connection, handle, 14)
            .await
            .unwrap_err(),
        "EACCES"
    );
    assert_eq!(reads.load(Ordering::Relaxed), 2);
    connection.close(0_u32.into(), b"done");
    endpoint.close(0_u32.into(), b"done");
    server.close().await;
}
