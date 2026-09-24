//! Opt-in real TiDB workload: ten independent coordinators and 100 wire QUIC clients.
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::FsDriver;

use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};

struct WorkloadAuthenticator;
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
use serde_json::{Value, json};

#[async_trait]
impl Authenticator for WorkloadAuthenticator {
    async fn authenticate(&self, token: &str, partition_id: &str) -> Result<SessionIdentity, ()> {
        if token != "load-token" || !["load", "isolated"].contains(&partition_id) {
            return Err(());
        }
        Ok(SessionIdentity {
            partition_id: partition_id.into(),
            policy_id: "load-policy".into(),
            issuer: "https://load.example.com".into(),
            subject: "load-client".into(),
            signing_algorithm: "ES256".into(),
            claims: json!({"aud":"mount-rs","repository_id":"load-repo"}),
            expires_at: i64::MAX,
        })
    }
}

pub async fn setup_with_left(
    left: Arc<dyn FsDriver>,
) -> Result<(RemoteServer, quinn::Endpoint, tempfile::TempDir), String> {
    let directory = tempfile::tempdir().map_err(|_| "wire setup failed (redacted)")?;
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .map_err(|_| "wire setup failed (redacted)")?,
    );
    let drives = BTreeMap::from([
        (
            "left".into(),
            DriveDefinition {
                driver: json!({"kind":"tidb-test"}),
            },
        ),
        (
            "right".into(),
            DriveDefinition {
                driver: json!({"kind":"tidb-test"}),
            },
        ),
    ]);
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.partitions.insert(
        "load".into(),
        PartitionDefinition {
            drives: drives.clone(),
        },
    );
    snapshot
        .partitions
        .insert("isolated".into(), PartitionDefinition { drives });
    snapshot.issuer_policies.insert(
        "load-policy".into(),
        json!({"issuer":"https://load.example.com","audiences":["mount-rs"]}),
    );
    snapshot.grants.insert(
        "writer".into(),
        GrantDefinition {
            partition_id: "load".into(),
            policy_id: "load-policy".into(),
            drives: BTreeMap::from([("left".into(), Permission::Write)]),
            claim_conditions: BTreeMap::from([("/repository_id".into(), "load-repo".into())]),
        },
    );
    catalog
        .compare_and_swap(0, snapshot)
        .await
        .map_err(|_| "wire setup failed (redacted)")?;
    let mut dispatcher = DriveDispatcher::new(catalog);
    for (drive, driver) in [("left", left.clone()), ("right", left.clone())] {
        dispatcher
            .register("load", drive, driver)
            .map_err(|_| "wire setup failed (redacted)")?;
    }
    dispatcher
        .register("isolated", "left", left)
        .map_err(|_| "wire setup failed (redacted)")?;
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(|_| "wire setup failed (redacted)")?;
    let cert_der = certificate.cert.der().clone();
    let key_der =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(cert_der.clone())
        .map_err(|_| "wire setup failed (redacted)")?;
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "wire setup failed (redacted)")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"mount-rs/1".to_vec()];
    let config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|_| "wire setup failed (redacted)")?,
    ));
    let mut endpoint = quinn::Endpoint::client(
        "127.0.0.1:0"
            .parse()
            .map_err(|_| "wire setup failed (redacted)")?,
    )
    .map_err(|_| "wire setup failed (redacted)")?;
    endpoint.set_default_client_config(config);
    let server = RemoteServer::bind(
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .map_err(|_| "wire setup failed (redacted)")?,
        vec![cert_der.clone()],
        key_der.into(),
        Arc::new(dispatcher),
        Arc::new(WorkloadAuthenticator),
    )
    .await
    .map_err(|_| "wire setup failed (redacted)")?;
    Ok((server, endpoint, directory))
}

pub async fn connect(
    endpoint: &quinn::Endpoint,
    address: SocketAddr,
) -> Result<quinn::Connection, String> {
    connect_partition(endpoint, address, "load").await
}

async fn connect_partition(
    endpoint: &quinn::Endpoint,
    address: SocketAddr,
    partition: &str,
) -> Result<quinn::Connection, String> {
    let connection = endpoint
        .connect(address, "localhost")
        .map_err(|e| e.to_string())?
        .await
        .map_err(|e| e.to_string())?;
    let (mut send, mut recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
    write_frame(
        &mut send,
        &Message::ClientHello {
            version: PROTOCOL_VERSION,
            partition_id: partition.into(),
            bearer: "load-token".into(),
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    match read_frame(&mut recv).await.map_err(|e| e.to_string())? {
        Message::ServerHello {
            version: PROTOCOL_VERSION,
            ..
        } => {
            if !recv
                .read_to_end(1)
                .await
                .map_err(|e| e.to_string())?
                .is_empty()
            {
                return Err("trailing response bytes".into());
            }
            Ok(connection)
        }
        other => Err(format!("unexpected hello: {other:?}")),
    }
}

async fn request(
    connection: &quinn::Connection,
    id: u64,
    drive: &str,
    name: OperationName,
    body: Value,
) -> Result<Result<Value, String>, String> {
    let (mut send, mut recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
    write_frame(
        &mut send,
        &Message::Request {
            request_id: id,
            drive_id: drive.into(),
            operation: Operation { name, body },
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    match read_frame(&mut recv).await.map_err(|e| e.to_string())? {
        Message::Response { request_id, result } if request_id == id => {
            if !recv
                .read_to_end(1)
                .await
                .map_err(|e| e.to_string())?
                .is_empty()
            {
                return Err("trailing response bytes".into());
            }
            Ok(result.map_err(|e| e.code))
        }
        other => Err(format!("unexpected response for {id}: {other:?}")),
    }
}

pub async fn success(
    connection: &quinn::Connection,
    id: u64,
    drive: &str,
    name: OperationName,
    body: Value,
) -> Result<Value, String> {
    request(connection, id, drive, name, body)
        .await?
        .map_err(|e| format!("{drive} request {id}: {e}"))
}

pub type TidbFs = ChunkedFs<TidbMetadataStore, TidbBlockStore>;

pub async fn open(
    url: &str,
    key: &str,
    index: usize,
) -> Result<(TidbFs, TidbMetadataStore, TidbBlockStore), String> {
    let options = TidbStorageOptions::new(key).with_durable(true);
    // Deliberately redact provider errors: a connection error may contain the operator URL.
    let metadata = TidbMetadataStore::connect_with_options(url, options.clone())
        .await
        .map_err(|_| "TiDB metadata connect failed")?;
    let chunk_options = ChunkedOptions::fixed(format!("remote-tidb-{index}"), 4096)
        .map_err(|_| "invalid chunk options")?
        .with_concurrent_writes(true);
    let blocks = match TidbBlockStore::connect_with_options(url, options).await {
        Ok(blocks) => blocks,
        Err(_) => {
            let closed = metadata.close().await;
            return Err(if closed.is_ok() {
                "TiDB blocks connect failed"
            } else {
                "TiDB blocks connect failed; metadata cleanup failed"
            }
            .into());
        }
    };
    let fs = match ChunkedFs::open(metadata.clone(), blocks.clone(), chunk_options).await {
        Ok(fs) => fs,
        Err(_) => {
            let m = metadata.close().await;
            let b = blocks.close().await;
            return Err(if m.is_ok() && b.is_ok() {
                "TiDB filesystem open failed"
            } else {
                "TiDB filesystem open failed; provider cleanup failed"
            }
            .into());
        }
    };
    Ok((fs, metadata, blocks))
}
