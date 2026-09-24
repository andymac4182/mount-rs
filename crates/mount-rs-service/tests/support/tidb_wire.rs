//! Shared production QUIC wire fixture for provider saturation workloads.
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use async_trait::async_trait;
use mount_rs_core::FsDriver;

struct WorkloadAuthenticator;
use mount_rs_remote_protocol::binary::{self, IoRequest};
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
        if (token != "load-token"
            && token
                .strip_prefix("load-token-")
                .and_then(|s| s.parse::<usize>().ok())
                .is_none())
            || !["load", "isolated"].contains(&partition_id)
        {
            return Err(());
        }
        Ok(SessionIdentity {
            partition_id: partition_id.into(),
            policy_id: "load-policy".into(),
            issuer: "https://load.example.com".into(),
            subject: "load-client".into(),
            signing_algorithm: "ES256".into(),
            claims: json!({"aud":"mount-rs","repository_id":"load-repo","sandbox_id":token.strip_prefix("load-token-").unwrap_or("shared")}),
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
    bind_dispatcher(dispatcher, directory).await
}

async fn bind_dispatcher(
    dispatcher: DriveDispatcher,
    directory: tempfile::TempDir,
) -> Result<(RemoteServer, quinn::Endpoint, tempfile::TempDir), String> {
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
    tls.alpn_protocols = vec![b"mount-rs/2".to_vec()];
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
    let max_connections = std::env::var("MOUNT_RS_REMOTE_SATURATION_CONNECTION_LIMIT")
        .map(|value| value.parse().map_err(|_| "invalid connection limit"))
        .unwrap_or(Ok(128))?;
    let server = RemoteServer::bind_with_options(
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .map_err(|_| "wire setup failed (redacted)")?,
        vec![cert_der.clone()],
        key_der.into(),
        Arc::new(dispatcher),
        Arc::new(WorkloadAuthenticator),
        mount_rs_service::server::RemoteServerOptions { max_connections },
    )
    .await
    .map_err(|_| "wire setup failed (redacted)")?;
    Ok((server, endpoint, directory))
}

pub async fn setup_with_drives(
    drivers: Vec<Arc<dyn FsDriver>>,
) -> Result<(RemoteServer, quinn::Endpoint, tempfile::TempDir), String> {
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .map_err(|_| "catalog open failed")?,
    );
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.issuer_policies.insert(
        "load-policy".into(),
        json!({"issuer":"https://load.example.com","audiences":["mount-rs"]}),
    );
    let mut drives = BTreeMap::new();
    for i in 0..drivers.len() {
        let name = format!("sandbox-{i}");
        drives.insert(
            name.clone(),
            DriveDefinition {
                driver: json!({"kind":"tidb-test","sandbox":i}),
            },
        );
        snapshot.grants.insert(
            name.clone(),
            GrantDefinition {
                partition_id: "load".into(),
                policy_id: "load-policy".into(),
                drives: BTreeMap::from([(name, Permission::Write)]),
                claim_conditions: BTreeMap::from([("/sandbox_id".into(), i.to_string())]),
            },
        );
    }
    snapshot
        .partitions
        .insert("load".into(), PartitionDefinition { drives });
    catalog
        .compare_and_swap(0, snapshot)
        .await
        .map_err(|_| "catalog publication failed")?;
    let mut dispatcher = DriveDispatcher::new(catalog);
    for (i, driver) in drivers.into_iter().enumerate() {
        dispatcher
            .register("load", &format!("sandbox-{i}"), driver)
            .map_err(|_| "drive registration failed")?;
    }
    bind_dispatcher(dispatcher, directory).await
}

pub async fn connect_sandbox(
    endpoint: &quinn::Endpoint,
    address: SocketAddr,
    sandbox: usize,
) -> Result<quinn::Connection, String> {
    connect_token(endpoint, address, "load", &format!("load-token-{sandbox}")).await
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
    connect_token(endpoint, address, partition, "load-token").await
}

async fn connect_token(
    endpoint: &quinn::Endpoint,
    address: SocketAddr,
    partition: &str,
    token: &str,
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
            bearer: token.into(),
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

pub async fn request(
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

/// One typed raw-I/O stream, with FIN and a validated empty reply trailer.
/// Transport or protocol failures terminally close the connection; callers never replay.
#[allow(dead_code)] // Shared fixture: only saturation targets exercise raw I/O.
pub async fn handle_read(
    connection: &quinn::Connection,
    id: u64,
    request: &IoRequest,
    buffer: &mut [u8],
) -> Result<usize, String> {
    typed_io(connection, id, request, None, buffer).await
}
#[allow(dead_code)] // Shared fixture: only saturation targets exercise raw I/O.
pub async fn handle_write(
    connection: &quinn::Connection,
    id: u64,
    request: &IoRequest,
    data: &[u8],
) -> Result<usize, String> {
    typed_io(connection, id, request, Some(data), &mut []).await
}
#[allow(dead_code)] // Reachable through raw-I/O helpers in saturation targets.
async fn typed_io(
    connection: &quinn::Connection,
    id: u64,
    request: &IoRequest,
    data: Option<&[u8]>,
    buffer: &mut [u8],
) -> Result<usize, String> {
    let result = async {
        let (mut send, mut recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
        binary::write_request(&mut send, id, request, buffer.len(), data)
            .await
            .map_err(|e| e.to_string())?;
        send.finish().map_err(|e| e.to_string())?;
        let count = binary::read_result(
            &mut recv,
            id,
            data.is_none(),
            buffer,
            data.map_or(0, <[u8]>::len),
        )
        .await
        .map_err(|e| e.to_string())?;
        recv.read_to_end(0)
            .await
            .map_err(|_| "trailing or invalid response bytes".to_string())?;
        count.map_err(|e| e.code)
    }
    .await;
    if result.is_err() {
        connection.close(1_u32.into(), b"typed I/O failed; never replayed");
    }
    result
}
