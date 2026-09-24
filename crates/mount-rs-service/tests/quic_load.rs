//! Loopback QUIC workload. The ignored variant is a bounded, configurable soak gate.

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

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
use serde_json::{Value, json};

struct WorkloadAuthenticator;

struct StatGate {
    arrivals: tokio::sync::watch::Sender<usize>,
    active: AtomicUsize,
    peak: AtomicUsize,
    release: tokio::sync::Semaphore,
}

struct GatedStatFs {
    memory: MemoryFs,
    gate: Arc<StatGate>,
}

#[async_trait]
impl FsDriver for GatedStatFs {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.memory.capabilities()
    }

    async fn stat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
        let active = self.gate.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.gate.peak.fetch_max(active, Ordering::SeqCst);
        self.gate.arrivals.send_modify(|arrivals| *arrivals += 1);
        let permit = self
            .gate
            .release
            .acquire()
            .await
            .expect("gate remains open");
        drop(permit);
        self.gate.active.fetch_sub(1, Ordering::SeqCst);
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
    ) -> mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>> {
        self.memory.open(path, flags, mode).await
    }
}

#[async_trait]
impl Authenticator for WorkloadAuthenticator {
    async fn authenticate(&self, token: &str, partition_id: &str) -> Result<SessionIdentity, ()> {
        if token != "load-token" || partition_id != "load" {
            return Err(());
        }
        Ok(SessionIdentity {
            partition_id: "load".into(),
            policy_id: "load-policy".into(),
            issuer: "https://load.example.com".into(),
            subject: "load-client".into(),
            signing_algorithm: "ES256".into(),
            claims: json!({"aud":"mount-rs","repository_id":"load-repo"}),
            expires_at: i64::MAX,
        })
    }
}

async fn setup() -> (RemoteServer, quinn::Endpoint, tempfile::TempDir) {
    setup_with_left(Arc::new(MemoryFs::new(MemoryOptions::default()))).await
}

async fn setup_with_left(
    left: Arc<dyn FsDriver>,
) -> (RemoteServer, quinn::Endpoint, tempfile::TempDir) {
    setup_with_left_and_options(
        left,
        mount_rs_service::server::RemoteServerOptions::default(),
    )
    .await
}

async fn setup_with_left_and_options(
    left: Arc<dyn FsDriver>,
    options: mount_rs_service::server::RemoteServerOptions,
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
                driver: json!({"kind":"memory"}),
            },
        ),
        (
            "right".into(),
            DriveDefinition {
                driver: json!({"kind":"memory"}),
            },
        ),
    ]);
    let mut snapshot = CatalogSnapshot::empty();
    snapshot
        .partitions
        .insert("load".into(), PartitionDefinition { drives });
    snapshot.issuer_policies.insert(
        "load-policy".into(),
        json!({"issuer":"https://load.example.com","audiences":["mount-rs"]}),
    );
    snapshot.grants.insert(
        "writer".into(),
        GrantDefinition {
            partition_id: "load".into(),
            policy_id: "load-policy".into(),
            drives: BTreeMap::from([
                ("left".into(), Permission::Write),
                ("right".into(), Permission::Write),
            ]),
            claim_conditions: BTreeMap::from([("/repository_id".into(), "load-repo".into())]),
        },
    );
    catalog.compare_and_swap(0, snapshot).await.unwrap();
    let mut dispatcher = DriveDispatcher::new(catalog);
    for (drive, driver) in [
        ("left", left),
        (
            "right",
            Arc::new(MemoryFs::new(MemoryOptions::default())) as Arc<dyn FsDriver>,
        ),
    ] {
        dispatcher.register("load", drive, driver).unwrap();
    }
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = certificate.cert.der().clone();
    let key_der =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let server = RemoteServer::bind_with_options(
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        vec![cert_der.clone()],
        key_der.into(),
        Arc::new(dispatcher),
        Arc::new(WorkloadAuthenticator),
        options,
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
    tls.alpn_protocols = vec![b"mount-rs/2".to_vec()];
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
            partition_id: "load".into(),
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

#[derive(Default)]
struct Metrics {
    operations: usize,
    bytes: usize,
    latencies: Vec<Duration>,
}

async fn run_client(
    endpoint: quinn::Endpoint,
    address: SocketAddr,
    client: usize,
    rounds: usize,
    payload_bytes: usize,
) -> Result<Metrics, String> {
    let connection = connect(&endpoint, address).await?;
    let mut metrics = Metrics::default();
    // Reuse one path per client and drive: even the maximum configured soak
    // retains at most two payloads per client in the in-memory backends.
    let path = format!("/client-{client}");
    for round in 0..rounds {
        let mut expected = Vec::new();
        for (drive_index, drive) in ["left", "right"].iter().enumerate() {
            let id = (round * 20 + drive_index * 10 + 1) as u64;
            let marker = format!("client={client};round={round};drive={drive};");
            let mut data: Vec<u8> = (0..payload_bytes)
                .map(|index| {
                    (index as u64)
                        .wrapping_mul(31)
                        .wrapping_add((client as u64).wrapping_mul(17))
                        .wrapping_add((round as u64).wrapping_mul(13))
                        .wrapping_add(drive_index as u64) as u8
                })
                .collect();
            data[..marker.len()].copy_from_slice(marker.as_bytes());
            let started = Instant::now();
            let handle = success(
                &connection,
                id,
                drive,
                OperationName::Open,
                json!({"path":path,"flags":"w+","mode":420}),
            )
            .await?
            .as_u64()
            .ok_or_else(|| "open returned nonnumeric handle".to_string())?;
            let written = success(
                &connection,
                id + 1,
                drive,
                OperationName::HandleWrite,
                json!({"handle":handle,"data":data,"position":0}),
            )
            .await?;
            if written != json!(data.len()) {
                return Err(format!("short write on {drive}: {written}"));
            }
            let read = success(
                &connection,
                id + 2,
                drive,
                OperationName::HandleRead,
                json!({"handle":handle,"length":data.len(),"position":0}),
            )
            .await?;
            if read != json!(data) {
                return Err(format!("data mismatch on {drive} {path}"));
            }
            let other = if *drive == "left" { "right" } else { "left" };
            let denied = request(
                &connection,
                id + 3,
                other,
                OperationName::HandleRead,
                json!({"handle":handle,"length":1,"position":0}),
            )
            .await?;
            if denied != Err("EBADF".into()) {
                return Err(format!("cross-drive handle accepted: {denied:?}"));
            }
            success(
                &connection,
                id + 4,
                drive,
                OperationName::HandleClose,
                json!({"handle":handle}),
            )
            .await?;
            metrics.operations += 5;
            metrics.bytes += data.len() * 2;
            metrics.latencies.push(started.elapsed());
            expected.push(data);
        }
        // The same path exists on both drives. Verify the second write did not alter the first.
        for (drive_index, drive) in ["left", "right"].iter().enumerate() {
            let id = (round * 20 + drive_index * 10 + 6) as u64;
            let handle = success(
                &connection,
                id,
                drive,
                OperationName::Open,
                json!({"path":path,"flags":"r","mode":0}),
            )
            .await?
            .as_u64()
            .ok_or_else(|| "reopen returned nonnumeric handle".to_string())?;
            let read = success(
                &connection,
                id + 1,
                drive,
                OperationName::HandleRead,
                json!({"handle":handle,"length":expected[drive_index].len(),"position":0}),
            )
            .await?;
            if read != json!(expected[drive_index]) {
                return Err(format!("cross-drive data mismatch on {drive} {path}"));
            }
            success(
                &connection,
                id + 2,
                drive,
                OperationName::HandleClose,
                json!({"handle":handle}),
            )
            .await?;
            metrics.operations += 3;
            metrics.bytes += expected[drive_index].len();
        }
    }
    connection.close(0_u32.into(), b"completed");
    Ok(metrics)
}

async fn workload(clients: usize, rounds: usize, payload_bytes: usize) {
    let (server, endpoint, _directory) = setup().await;
    let address = server.local_addr();
    let started = Instant::now();
    let mut jobs = tokio::task::JoinSet::new();
    for client in 0..clients {
        jobs.spawn(run_client(
            endpoint.clone(),
            address,
            client,
            rounds,
            payload_bytes,
        ));
    }
    let mut metrics = Metrics::default();
    let mut errors = Vec::new();
    while let Some(result) = jobs.join_next().await {
        match result {
            Ok(Ok(m)) => {
                metrics.operations += m.operations;
                metrics.bytes += m.bytes;
                metrics.latencies.extend(m.latencies);
            }
            Ok(Err(e)) => errors.push(e),
            Err(e) => errors.push(e.to_string()),
        }
    }
    metrics.latencies.sort();
    let elapsed = started.elapsed();
    let percentile = |percent: usize| {
        metrics
            .latencies
            .get((metrics.latencies.len().saturating_sub(1) * percent) / 100)
            .copied()
            .unwrap_or_default()
    };
    eprintln!(
        "quic_load clients={clients} rounds={rounds} payload_bytes={payload_bytes} operations={} bytes={} elapsed_ms={} ops_per_second={:.1} mib_per_second={:.2} batch_p50_ms={} batch_p95_ms={} errors={}",
        metrics.operations,
        metrics.bytes,
        elapsed.as_millis(),
        metrics.operations as f64 / elapsed.as_secs_f64(),
        metrics.bytes as f64 / elapsed.as_secs_f64() / 1_048_576.0,
        percentile(50).as_millis(),
        percentile(95).as_millis(),
        errors.len()
    );
    endpoint.close(0_u32.into(), b"completed");
    server.close().await;
    assert!(errors.is_empty(), "workload failures: {errors:?}");
    assert_eq!(metrics.operations, clients * rounds * 2 * 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_clients_mixed_read_write_preserve_drive_isolation() {
    tokio::time::timeout(Duration::from_secs(30), workload(4, 4, 4096))
        .await
        .expect("bounded CI workload timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_connection_admits_32_data_requests_and_rejects_16_overflow_requests() {
    let (arrivals, mut observed) = tokio::sync::watch::channel(0);
    let gate = Arc::new(StatGate {
        arrivals,
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        release: tokio::sync::Semaphore::new(0),
    });
    let left = Arc::new(GatedStatFs {
        memory: MemoryFs::new(MemoryOptions::default()),
        gate: gate.clone(),
    });
    let (server, endpoint, _directory) = setup_with_left(left).await;
    let connection = connect(&endpoint, server.local_addr()).await.unwrap();
    let start = Arc::new(tokio::sync::Barrier::new(49));
    let mut jobs = tokio::task::JoinSet::new();
    for id in 1..=48 {
        let connection = connection.clone();
        let start = start.clone();
        jobs.spawn(async move {
            start.wait().await;
            success(
                &connection,
                id,
                "left",
                OperationName::Stat,
                json!({"path":"/"}),
            )
            .await
        });
    }
    start.wait().await;
    let reached_limit = tokio::time::timeout(Duration::from_secs(10), async {
        while *observed.borrow_and_update() < 32 {
            observed.changed().await.expect("stat gate remains open");
        }
    })
    .await;
    let held = *observed.borrow();
    // All request tasks have started. While the first 32 backend calls are
    // blocked, a 33rd arrival would show that the server admitted too many.
    let exceeded_limit = tokio::time::timeout(Duration::from_millis(500), observed.changed())
        .await
        .is_ok();
    let before_release = *observed.borrow();

    assert!(
        reached_limit.is_ok(),
        "only {held} stat calls reached the backend"
    );
    assert_eq!(held, 32, "server admitted more than 32 active streams");
    assert!(!exceeded_limit, "a 33rd stat call entered before release");
    assert_eq!(before_release, 32);
    tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..16 {
            assert!(
                jobs.join_next().await.unwrap().unwrap().is_err(),
                "overflow request entered dispatch"
            );
        }
    })
    .await
    .expect("overflow requests did not fail fast");
    assert_eq!(*observed.borrow(), 32);
    gate.release.add_permits(32);
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut completed = 0;
        while let Some(result) = jobs.join_next().await {
            assert!(
                result.unwrap().unwrap().is_object(),
                "stat returned invalid data"
            );
            completed += 1;
        }
        assert_eq!(completed, 32);
    })
    .await
    .expect("admitted streams did not complete");
    assert_eq!(*observed.borrow(), 32);
    assert!(gate.peak.load(Ordering::SeqCst) <= 32);
    assert_eq!(gate.active.load(Ordering::SeqCst), 0);
    connection.close(0_u32.into(), b"completed");
    endpoint.close(0_u32.into(), b"completed");
    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_handle_cannot_cross_quic_sessions() {
    let (server, endpoint, _directory) = setup().await;
    let first = connect(&endpoint, server.local_addr()).await.unwrap();
    let second = connect(&endpoint, server.local_addr()).await.unwrap();
    let handle = success(
        &first,
        1,
        "left",
        OperationName::Open,
        json!({"path":"/session-private","flags":"w+","mode":420}),
    )
    .await
    .unwrap()
    .as_u64()
    .unwrap();
    let denied = request(
        &second,
        2,
        "left",
        OperationName::HandleRead,
        json!({"handle":handle,"length":1,"position":0}),
    )
    .await
    .unwrap();
    assert_eq!(denied, Err("EBADF".into()));
    success(
        &first,
        3,
        "left",
        OperationName::HandleClose,
        json!({"handle":handle}),
    )
    .await
    .unwrap();
    first.close(0_u32.into(), b"completed");
    second.close(0_u32.into(), b"completed");
    endpoint.close(0_u32.into(), b"completed");
    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_limit_refuses_129th_session_and_recovers_after_close() {
    connection_admission(128).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_connection_limit_refuses_overload_and_recovers_after_close() {
    connection_admission(2).await;
}

async fn connection_admission(limit: usize) {
    let (server, endpoint, _directory) = setup_with_left_and_options(
        Arc::new(MemoryFs::new(MemoryOptions::default())),
        mount_rs_service::server::RemoteServerOptions {
            max_connections: limit,
        },
    )
    .await;
    let address = server.local_addr();
    let mut sessions = Vec::with_capacity(limit);
    tokio::time::timeout(Duration::from_secs(30), async {
        for _ in 0..limit {
            sessions.push(connect(&endpoint, address).await.expect("admitted session"));
        }
    })
    .await
    .expect("admitting configured sessions timed out");

    // Every hello was answered, so each server session has passed handshake and
    // retains one connection permit. A refused extra connection must not disturb them.
    let extra = tokio::time::timeout(Duration::from_secs(5), connect(&endpoint, address))
        .await
        .expect("extra connection did not resolve");
    assert!(extra.is_err(), "extra connection was admitted");
    success(
        &sessions[limit - 1],
        1,
        "left",
        OperationName::Stat,
        json!({"path":"/"}),
    )
    .await
    .expect("existing session failed after overload");

    sessions.pop().unwrap().close(0_u32.into(), b"free slot");
    let recovered = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connection) = connect(&endpoint, address).await {
                break connection;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("slot did not recover after closing one session");
    success(
        &recovered,
        1,
        "left",
        OperationName::Stat,
        json!({"path":"/"}),
    )
    .await
    .expect("recovered session could not dispatch");
    recovered.close(0_u32.into(), b"completed");
    for session in sessions {
        session.close(0_u32.into(), b"completed");
    }
    endpoint.close(0_u32.into(), b"completed");
    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in bounded soak: use scripts/test-quic-load.sh soak"]
async fn bounded_quic_soak() {
    fn bounded(name: &str, default: usize, maximum: usize) -> usize {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
            .min(maximum)
    }
    let clients = bounded("MOUNT_RS_QUIC_LOAD_CLIENTS", 8, 64);
    let rounds = bounded("MOUNT_RS_QUIC_LOAD_ROUNDS", 100, 1000);
    let payload_bytes = bounded("MOUNT_RS_QUIC_LOAD_PAYLOAD_BYTES", 4096, 65536).max(64);
    tokio::time::timeout(
        Duration::from_secs(300),
        workload(clients, rounds, payload_bytes),
    )
    .await
    .expect("bounded soak timed out");
}
