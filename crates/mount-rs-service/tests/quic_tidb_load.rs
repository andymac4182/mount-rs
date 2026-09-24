//! Opt-in real TiDB workload: ten independent coordinators and 100 wire QUIC clients.
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::FsDriver;
use mount_rs_core::Loopback;
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
use mysql_async::{Pool, prelude::Queryable};
use std::time::{SystemTime, UNIX_EPOCH};

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

async fn setup_with_left(
    left: Arc<dyn FsDriver>,
) -> (RemoteServer, quinn::Endpoint, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
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
    catalog.compare_and_swap(0, snapshot).await.unwrap();
    let mut dispatcher = DriveDispatcher::new(catalog);
    for (drive, driver) in [("left", left.clone()), ("right", left.clone())] {
        dispatcher.register("load", drive, driver).unwrap();
    }
    dispatcher.register("isolated", "left", left).unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = certificate.cert.der().clone();
    let key_der =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let server = RemoteServer::bind(
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        vec![cert_der.clone()],
        key_der.into(),
        Arc::new(dispatcher),
        Arc::new(WorkloadAuthenticator),
    )
    .await
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der).unwrap();
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
    (server, endpoint, directory)
}

async fn connect(
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
            recv.read_to_end(1).await.map_err(|e| e.to_string())?;
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
            Ok(result.map_err(|e| e.code))
        }
        other => Err(format!("unexpected response for {id}: {other:?}")),
    }
}

async fn success(
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

type TidbFs = ChunkedFs<TidbMetadataStore, TidbBlockStore>;
const CLIENTS: usize = 100;
const SERVERS: usize = 10;
const DEADLINE: Duration = Duration::from_secs(600);

async fn open(
    url: &str,
    key: &str,
    index: usize,
) -> Result<(TidbFs, TidbMetadataStore, TidbBlockStore), String> {
    let options = TidbStorageOptions::new(key).with_durable(true);
    // Deliberately redact provider errors: a connection error may contain the operator URL.
    let metadata = TidbMetadataStore::connect_with_options(url, options.clone())
        .await
        .map_err(|_| "TiDB metadata connect failed")?;
    let blocks = TidbBlockStore::connect_with_options(url, options)
        .await
        .map_err(|_| "TiDB blocks connect failed")?;
    let fs = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed(format!("remote-tidb-{index}"), 4096)
            .map_err(|_| "invalid chunk options")?
            .with_concurrent_writes(true),
    )
    .await
    .map_err(|_| "TiDB filesystem open failed")?;
    Ok((fs, metadata, blocks))
}

fn payload(client: usize, round: usize) -> Vec<u8> {
    let mut data: Vec<u8> = (0..4096)
        .map(|byte| ((byte * 31 + client * 17 + round * 13) % 256) as u8)
        .collect();
    let marker = format!("client={client};round={round};");
    data[..marker.len()].copy_from_slice(marker.as_bytes());
    data
}

#[derive(Clone)]
struct Acknowledged {
    client: usize,
    round: usize,
    renamed: bool,
}
#[derive(Default)]
struct Metrics {
    client: usize,
    completed: usize,
    latencies: Vec<Duration>,
}

async fn client_work(
    connection: quinn::Connection,
    client: usize,
    rounds: usize,
    barrier: Arc<tokio::sync::Barrier>,
    ledger: Arc<tokio::sync::Mutex<Vec<Acknowledged>>>,
) -> Result<Metrics, String> {
    barrier.wait().await;
    let mut metrics = Metrics {
        client,
        ..Metrics::default()
    };
    let mut id = 0;
    for round in 0..rounds {
        let path = format!("/client-{client}-round-{round}");
        let destination = format!("/renamed-{client}-round-{round}");
        let data = payload(client, round);
        // Each mutation is sent exactly once. Transport failure is uncertain and fails the test.
        let started = Instant::now();
        id += 1;
        let handle = success(
            &connection,
            id,
            "left",
            OperationName::Open,
            json!({"path":path,"flags":"w+","mode":420}),
        )
        .await?
        .as_u64()
        .ok_or("invalid handle")?;
        id += 1;
        let written = success(
            &connection,
            id,
            "left",
            OperationName::HandleWrite,
            json!({"handle":handle,"data":data,"position":0}),
        )
        .await?;
        if written != json!(4096) {
            return Err(format!("client {client} short write"));
        }
        ledger.lock().await.push(Acknowledged {
            client,
            round,
            renamed: false,
        });
        id += 1;
        success(
            &connection,
            id,
            "left",
            OperationName::HandleClose,
            json!({"handle":handle}),
        )
        .await?;
        id += 1;
        success(
            &connection,
            id,
            "left",
            OperationName::Rename,
            json!({"path":path,"destination":destination}),
        )
        .await?;
        ledger
            .lock()
            .await
            .iter_mut()
            .find(|a| a.client == client && a.round == round)
            .ok_or("missing acknowledgement")?
            .renamed = true;
        id += 1;
        let handle = success(
            &connection,
            id,
            "left",
            OperationName::Open,
            json!({"path":destination,"flags":"r","mode":0}),
        )
        .await?
        .as_u64()
        .ok_or("invalid read handle")?;
        id += 1;
        if success(
            &connection,
            id,
            "left",
            OperationName::HandleRead,
            json!({"handle":handle,"length":4096,"position":0}),
        )
        .await?
            != json!(data)
        {
            return Err(format!("client {client} round {round} read mismatch"));
        }
        id += 1;
        success(
            &connection,
            id,
            "left",
            OperationName::HandleClose,
            json!({"handle":handle}),
        )
        .await?;
        id += 1;
        success(&connection, id, "left", OperationName::Syncfs, json!({})).await?;
        metrics.latencies.push(started.elapsed());
        metrics.completed += 1;
    }
    connection.close(0_u32.into(), b"completed");
    Ok(metrics)
}

async fn workload(url: &str, key: &str, rounds: usize) -> Result<(), String> {
    let mut servers = Vec::new();
    let mut endpoints = Vec::new();
    let mut directories = Vec::new();
    let mut providers = Vec::new();
    for server in 0..SERVERS {
        let (fs, metadata, blocks) = open(url, key, server).await?;
        let (listener, endpoint, directory) = setup_with_left(Arc::new(fs.clone())).await;
        servers.push(listener);
        endpoints.push(endpoint);
        directories.push(directory);
        providers.push((fs, metadata, blocks));
    }
    let ledger = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let mut tasks = tokio::task::JoinSet::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(CLIENTS));
    let started = Instant::now();
    let mut errors = Vec::new();
    let mut completed = [0usize; SERVERS];
    let mut latencies = Vec::new();
    let phase = tokio::time::timeout(Duration::from_secs(500), async {
        // Connect every client before spawning work: a failed hello cannot strand the barrier.
        let mut connections = Vec::new();
        for client in 0..CLIENTS {
            let server = client / 10;
            connections.push(connect(&endpoints[server], servers[server].local_addr()).await?);
        }
        // A catalogued drive without a grant must fail before it reaches storage.
        let denied = request(
            &connections[0],
            1,
            "right",
            OperationName::Open,
            json!({"path":"/forbidden","flags":"w+","mode":420}),
        )
        .await?;
        if denied != Err("EACCES".into()) {
            return Err(format!("unauthorized Drive accepted: {denied:?}"));
        }
        // A separately catalogued and authenticated Partition has no grants to
        // this same TiDB backing volume: a mutation must be denied.
        let isolated =
            connect_partition(&endpoints[0], servers[0].local_addr(), "isolated").await?;
        let denied = request(
            &isolated,
            1,
            "left",
            OperationName::Open,
            json!({"path":"/partition-forbidden","flags":"w+","mode":420}),
        )
        .await?;
        if denied != Err("EACCES".into()) {
            return Err(format!("isolated Partition mutation accepted: {denied:?}"));
        }
        isolated.close(0_u32.into(), b"partition checked");
        for (client, connection) in connections.into_iter().enumerate() {
            tasks.spawn(client_work(
                connection,
                client,
                rounds,
                barrier.clone(),
                ledger.clone(),
            ));
        }

        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(m)) => {
                    eprintln!(
                        "TIDB_REMOTE_CLIENT client={} server={} completed={}",
                        m.client,
                        m.client / 10,
                        m.completed
                    );
                    completed[m.client / 10] += m.completed;
                    latencies.extend(m.latencies);
                }
                Ok(Err(e)) => errors.push(e),
                Err(e) => errors.push(format!("client task failed: {e}")),
            }
        }
        Ok::<(), String>(())
    })
    .await;
    match phase {
        Ok(Ok(())) => {}
        Ok(Err(error)) => errors.push(error),
        Err(_) => errors.push("500 second client phase deadline exceeded".into()),
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    for endpoint in endpoints {
        endpoint.close(0_u32.into(), b"shutdown");
    }
    let shutdown = tokio::time::timeout(Duration::from_secs(30), async {
        for server in servers {
            server.close().await;
        }
        for (fs, metadata, blocks) in providers {
            if fs.shutdown().await.is_err() {
                errors.push("filesystem shutdown failed".into());
            }
            if metadata.close().await.is_err() {
                errors.push("metadata close failed".into());
            }
            if blocks.close().await.is_err() {
                errors.push("blocks close failed".into());
            }
        }
    })
    .await;
    if shutdown.is_err() {
        errors.push("shutdown exceeded 30 seconds".into());
    }
    drop(directories);
    let (fresh, metadata, blocks) =
        tokio::time::timeout(Duration::from_secs(30), open(url, key, SERVERS))
            .await
            .map_err(|_| "fresh reopen exceeded 30 seconds")??;
    let view = Loopback::new(fresh.clone());
    let acknowledged = ledger.lock().await.clone();
    let mut verified_files = 0;
    let verified = tokio::time::timeout(Duration::from_secs(30),async {
    let expected_names: std::collections::BTreeSet<String> = acknowledged.iter().map(|a| if a.renamed { format!("renamed-{}-round-{}",a.client,a.round) } else { format!("client-{}-round-{}",a.client,a.round) }).collect();
    match view.readdir("/").await {
        Ok(entries) => {
            let names: std::collections::BTreeSet<String> = entries.into_iter().map(|entry| entry.name).collect();
            if names != expected_names { errors.push(format!("fresh namespace mismatch: expected {} acknowledged files, found {} entries",expected_names.len(),names.len())); }
        }
        Err(_) => errors.push("fresh namespace listing failed".into()),
    }
    for forbidden in ["/forbidden", "/partition-forbidden"] {
        if !matches!(view.stat(forbidden).await,Err(e) if e.is(mount_rs_core::ErrorCode::Enoent)) {
            errors.push(format!("denied mutation persisted: {forbidden}"));
        }
    }
    for a in &acknowledged {
        let original = format!("/client-{}-round-{}", a.client, a.round);
        let renamed = format!("/renamed-{}-round-{}", a.client, a.round);
        let path = if a.renamed { &renamed } else { &original };
        match view.read_file(path).await {
            Ok(bytes) if bytes == payload(a.client, a.round) => { verified_files += 1; }
            _ => errors.push(format!("fresh reopen mismatch: {path}")),
        }
        if a.renamed
            && !matches!(view.stat(&original).await,Err(e) if e.is(mount_rs_core::ErrorCode::Enoent))
        {
            errors.push(format!("acknowledged rename left original {original}"));
        }
    }
    }).await;
    if verified.is_err() {
        errors.push("fresh verification exceeded 30 seconds".into());
    }
    let closed = tokio::time::timeout(Duration::from_secs(30), async {
        if fresh.shutdown().await.is_err() {
            errors.push("fresh shutdown failed".into());
        }
        if metadata.close().await.is_err() {
            errors.push("fresh metadata close failed".into());
        }
        if blocks.close().await.is_err() {
            errors.push("fresh blocks close failed".into());
        }
    })
    .await;
    if closed.is_err() {
        errors.push("fresh shutdown exceeded 30 seconds".into());
    }
    latencies.sort();
    let percentile = |p: usize| {
        latencies
            .get(latencies.len().saturating_sub(1) * p / 100)
            .copied()
            .unwrap_or_default()
            .as_millis()
    };
    if completed != [10 * rounds; SERVERS] {
        errors.push("client/server completion counts incomplete".into());
    }
    if acknowledged.len() != CLIENTS * rounds {
        errors.push("acknowledged write count incomplete".into());
    }
    let elapsed = started.elapsed();
    eprintln!(
        "TIDB_REMOTE_LOAD clients={CLIENTS} servers={SERVERS} rounds={rounds} acknowledged_writes={} acknowledged_renames={} verified_files={} per_server_completed={completed:?} elapsed_ms={} operations_per_second={:.2} payload_mib_per_second={:.2} lifecycle_p50_ms={} lifecycle_p95_ms={} lifecycle_p99_ms={} failures={}",
        acknowledged.len(),
        acknowledged.iter().filter(|a| a.renamed).count(),
        verified_files,
        elapsed.as_millis(),
        latencies.len() as f64 * 8.0 / elapsed.as_secs_f64(),
        latencies.len() as f64 * 8192.0 / 1048576.0 / elapsed.as_secs_f64(),
        percentile(50),
        percentile(95),
        percentile(99),
        errors.len()
    );
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("TiDB remote workload failures: {errors:?}"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "requires actual disposable TiDB and MOUNT_RS_TIDB_URL; 100 clients / 10 independent servers"]
async fn actual_tidb_100_clients_10_servers_preserve_acknowledged_files() {
    tokio::time::timeout(DEADLINE, packet())
        .await
        .expect("600 second overall TiDB packet deadline exceeded");
}

async fn packet() {
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("MOUNT_RS_TIDB_URL is required");
    let rounds = std::env::var("MOUNT_RS_REMOTE_TIDB_ROUNDS")
        .map(|v| v.parse::<usize>().expect("rounds must be an integer"))
        .unwrap_or(2);
    assert!(
        (1..=20).contains(&rounds),
        "rounds must be between 1 and 20"
    );
    let pool = Pool::from_url(&url).unwrap_or_else(|_| panic!("invalid TiDB URL (redacted)"));
    let mut connection = pool
        .get_conn()
        .await
        .unwrap_or_else(|_| panic!("TiDB identity connect failed (redacted)"));
    let (identity, version): (String, String) = connection
        .query_first("SELECT tidb_version(), VERSION()")
        .await
        .unwrap_or_else(|_| panic!("actual TiDB identity query failed (redacted)"))
        .expect("TiDB identity missing");
    assert!(
        identity.to_ascii_lowercase().contains("release version:")
            && version.to_ascii_lowercase().contains("tidb"),
        "actual TiDB required"
    );
    eprintln!("TIDB_REMOTE_IDENTITY tidb_version={identity} version={version}");
    drop(connection);
    pool.disconnect()
        .await
        .unwrap_or_else(|_| panic!("TiDB identity disconnect failed (redacted)"));
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = format!("remote-load-{}-{timestamp}", std::process::id());
    workload(&url, &key, rounds)
        .await
        .expect("real TiDB QUIC workload failed");
}

#[test]
fn payload_identifies_client_and_round() {
    assert!(
        payload(61, 0) != payload(0, 1),
        "client/round payload collision"
    );
    let unique: std::collections::BTreeSet<Vec<u8>> = (0..CLIENTS)
        .flat_map(|client| (0..20).map(move |round| payload(client, round)))
        .collect();
    assert_eq!(unique.len(), CLIENTS * 20);
}
