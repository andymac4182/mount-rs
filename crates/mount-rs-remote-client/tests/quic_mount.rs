use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use mount_rs_core::{FsDriver, Loopback};
use mount_rs_remote_client::{
    connection::{ConnectionTransport, RemoteConnection},
    credentials::CredentialSource,
    driver::RemoteFsDriver,
};
use mount_rs_service::{
    auth::{AuthError, CatalogAuthenticator, Jwk, OidcKeySource, OidcVerifier},
    catalog::{
        CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        SqliteCatalog,
    },
    dispatch::DriveDispatcher,
    server::RemoteServer,
    websocket::WebSocketServer,
};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

struct Keys(Jwk);
#[async_trait::async_trait]
impl OidcKeySource for Keys {
    async fn fetch(&self, issuer: &str, audiences: &[String]) -> Result<OidcVerifier, AuthError> {
        OidcVerifier::new(issuer, audiences, vec![self.0.clone()])
    }
}
fn token() -> (String, String, Jwk) {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let key =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let point = key.public_key().as_ref();
    let jwk = Jwk::EcP256 {
        kid: "fixture".into(),
        x: URL_SAFE_NO_PAD.encode(&point[1..33]),
        y: URL_SAFE_NO_PAD.encode(&point[33..65]),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"fixture"}"#);
    let sign = |lifetime| {
        let payload=URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"iss":"https://issuer.example.com","aud":"mount-rs","sub":"sandbox-1","repository_id":"repo-1","iat":now,"exp":now+lifetime})).unwrap());
        let input = format!("{header}.{payload}");
        let signature = key.sign(&rng, input.as_bytes()).unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
    };
    (sign(30), sign(300), jwk)
}
#[tokio::test]
async fn signed_oidc_multiple_drives_persistence_and_revocation() {
    signed_oidc_roundtrip(0).await;
}

#[tokio::test]
async fn websocket_signed_oidc_multiple_drives_persistence_and_revocation() {
    signed_oidc_roundtrip(1).await;
}

#[tokio::test]
async fn udp_unavailable_falls_back_before_oidc_and_roundtrips() {
    signed_oidc_roundtrip(2).await;
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
async fn signed_oidc_roundtrip(mode: u8) {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("drive.sqlite");
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
    );
    let mut snapshot = CatalogSnapshot::empty();
    for partition in ["red", "blue"] {
        snapshot.partitions.insert(
            partition.into(),
            PartitionDefinition {
                drives: BTreeMap::from([
                    (
                        "data".into(),
                        DriveDefinition {
                            driver: json!({"kind":"sqlite","database":db}),
                        },
                    ),
                    (
                        "logs".into(),
                        DriveDefinition {
                            driver: json!({"kind":"memory"}),
                        },
                    ),
                ]),
            },
        );
    }
    snapshot.issuer_policies.insert(
        "oidc".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    snapshot.grants.insert(
        "workload".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            drives: BTreeMap::from([
                ("data".into(), Permission::Write),
                ("logs".into(), Permission::Read),
            ]),
            claim_conditions: BTreeMap::from([("/repository_id".into(), "repo-1".into())]),
        },
    );
    catalog.compare_and_swap(0, snapshot).await.unwrap();
    let filesystem = mount_rs_sdk::Filesystem::sqlite(&db).await.unwrap();
    filesystem.driver().chmod("/", 0o777).await.unwrap();
    let memory = mount_rs_sdk::Filesystem::memory(Default::default());
    memory
        .driver()
        .write_file("/marker", b"read-only drive")
        .await
        .unwrap();
    let mut dispatcher = DriveDispatcher::new(catalog.clone());
    dispatcher
        .register("red", "data", filesystem.driver())
        .unwrap();
    dispatcher.register("red", "logs", memory.driver()).unwrap();
    let (jwt, renewed_jwt, jwk) = token();
    let token_path = directory.path().join("token.jwt");
    std::fs::write(&token_path, jwt).unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = certificate.cert.der().clone();
    let key =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into();
    let dispatcher = Arc::new(dispatcher);
    let authenticator = Arc::new(CatalogAuthenticator::with_key_source(
        catalog.clone(),
        Arc::new(Keys(jwk)),
    ));
    let server = if mode == 0 {
        TestServer::Quic(
            RemoteServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                vec![cert.clone()],
                key,
                dispatcher,
                authenticator,
            )
            .await
            .unwrap(),
        )
    } else {
        TestServer::WebSocket(
            WebSocketServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                vec![cert.clone()],
                key,
                dispatcher,
                authenticator,
            )
            .await
            .unwrap(),
        )
    };
    let selection = match mode {
        0 => ConnectionTransport::Quic,
        1 => ConnectionTransport::WebSocket(server.local_addr()),
        _ => ConnectionTransport::Auto {
            websocket: server.local_addr(),
        },
    };
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let credentials = CredentialSource::File(token_path.clone());
    assert!(
        RemoteConnection::connect_with_transport(
            server.local_addr(),
            "localhost",
            roots.clone(),
            "blue".into(),
            credentials.clone(),
            selection
        )
        .await
        .is_err()
    );
    assert!(
        RemoteConnection::connect_with_transport(
            server.local_addr(),
            "localhost",
            rustls::RootCertStore::empty(),
            "red".into(),
            credentials.clone(),
            selection
        )
        .await
        .is_err()
    );
    let connection = RemoteConnection::connect_with_transport(
        server.local_addr(),
        "localhost",
        roots,
        "red".into(),
        credentials,
        selection,
    )
    .await
    .unwrap();
    assert_eq!(connection.protocol_version(), 2);
    let data = RemoteFsDriver::new(connection.clone(), "data".into())
        .await
        .unwrap();
    let logs = RemoteFsDriver::new(connection.clone(), "logs".into())
        .await
        .unwrap();
    assert!(logs.capabilities().read_only);
    assert!(
        matches!(connection.request("blue/data", mount_rs_remote_protocol::Operation { name: mount_rs_remote_protocol::OperationName::Stat, body: json!({"path":"/"}) }).await, Err(mount_rs_remote_client::connection::ClientError::Remote(code)) if code == "EACCES")
    );
    assert_eq!(
        logs.write_file("/denied", b"x").await.unwrap_err().code,
        mount_rs_core::ErrorCode::Eacces
    );
    let writer = data.open("/binary", "w+", 0o644).await.unwrap();
    let payload: Vec<u8> = (0..mount_rs_remote_protocol::binary::MAX_IO_BYTES)
        .map(|n| n as u8)
        .collect();
    assert_eq!(
        writer.write(&payload, Some(0)).await.unwrap(),
        payload.len()
    );
    let mut received = vec![0; payload.len()];
    assert_eq!(
        writer.read(&mut received, Some(0)).await.unwrap(),
        payload.len()
    );
    assert_eq!(received, payload);
    writer.close().await.unwrap();
    let readonly = logs.open("/marker", "r", 0).await.unwrap();
    assert_eq!(
        readonly.write(b"denied", Some(0)).await.unwrap_err().code,
        mount_rs_core::ErrorCode::Eacces
    );
    readonly.close().await.unwrap();
    data.write_file("/saved", b"persistent remote bytes")
        .await
        .unwrap();
    let handle = data.open("/saved", "r", 0).await.unwrap();
    let replacement = directory.path().join("token.replacement");
    std::fs::write(&replacement, renewed_jwt).unwrap();
    std::fs::rename(replacement, &token_path).unwrap();
    // The next read renews the expiring token without rebinding this handle.
    let mut bytes = [0; 10];
    assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 10);
    assert_eq!(&bytes, b"persistent");
    handle.close().await.unwrap();
    data.rename("/saved", "/renamed").await.unwrap();
    data.syncfs().await.unwrap();
    assert_eq!(
        Loopback::new(data).read_file("/renamed").await.unwrap(),
        b"persistent remote bytes"
    );

    if std::env::var_os("MOUNT_RS_REMOTE_NATIVE_NFS").is_some() {
        let driver = RemoteFsDriver::new(connection.clone(), "data".into())
            .await
            .unwrap();
        let mountpoint = directory.path().join("native");
        std::fs::create_dir(&mountpoint).unwrap();
        let mounted = mount_rs_auto::mount(
            driver,
            &mountpoint,
            mount_rs_auto::AutoMountOptions {
                transport: mount_rs_auto::AutoTransport::Nfs,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let logs_driver = RemoteFsDriver::new(connection.clone(), "logs".into())
            .await
            .unwrap();
        let logs_mountpoint = directory.path().join("native-logs");
        std::fs::create_dir(&logs_mountpoint).unwrap();
        let logs_mounted = match mount_rs_auto::mount(
            logs_driver,
            &logs_mountpoint,
            mount_rs_auto::AutoMountOptions {
                transport: mount_rs_auto::AutoTransport::Nfs,
                ..Default::default()
            },
        )
        .await
        {
            Ok(mount) => mount,
            Err(error) => {
                mounted.unmount().await.unwrap();
                panic!("read-only native mount failed: {error}");
            }
        };
        let mounted_path = mountpoint.clone();
        let result = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
            let file = std::fs::File::create(mounted_path.join("native-file"))?;
            use std::io::Write;
            (&file).write_all(b"native remote bytes")?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(
                mounted_path.join("native-file"),
                mounted_path.join("native-renamed"),
            )?;
            assert_eq!(
                std::fs::read(logs_mountpoint.join("marker"))?,
                b"read-only drive"
            );
            assert!(std::fs::write(logs_mountpoint.join("denied"), b"x").is_err());
            std::fs::read(mounted_path.join("native-renamed"))
        })
        .await
        .unwrap();
        logs_mounted.unmount().await.unwrap();
        mounted.unmount().await.unwrap();
        assert_eq!(result.unwrap(), b"native remote bytes");
    }
    let mut revoked = catalog.load_current().await.unwrap();
    revoked.grants.clear();
    catalog.compare_and_swap(1, revoked).await.unwrap();
    assert_eq!(
        logs.stat("/").await.unwrap_err().code,
        mount_rs_core::ErrorCode::Eacces
    );
    connection.close();
    server.close().await;
    filesystem.shutdown().await.unwrap();
    let reopened = mount_rs_sdk::Filesystem::sqlite(&db).await.unwrap();
    assert_eq!(
        Loopback::from_arc(reopened.driver())
            .read_file("/renamed")
            .await
            .unwrap(),
        b"persistent remote bytes"
    );
    reopened.shutdown().await.unwrap();
}
