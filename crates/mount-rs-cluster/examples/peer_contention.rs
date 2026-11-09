//! Fixed-response peer HTTP/3 diagnostic; no filesystem, Redis or public HTTP.
use bytes::Bytes;
use http::{Request, Response};
use mount_rs_cluster::transport::*;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct Identity {
    cert: Certificate,
    key: KeyPair,
}
fn peer(ca: &Identity, name: &str) -> Identity {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
    params.subject_alt_names.push(SanType::URI(
        format!("urn:agent-tunnel:peer:{name}").try_into().unwrap(),
    ));
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    Identity {
        cert: params.signed_by(&key, &ca.cert, &ca.key).unwrap(),
        key,
    }
}

async fn request(
    client: &PeerClient,
    destination: PeerDestination,
    size: usize,
    caller: usize,
) -> [f64; 4] {
    let start = std::time::Instant::now();
    let mut stream = client
        .open(
            destination,
            Request::builder()
                .method("GET")
                .uri(format!("https://localhost/{size}"))
                .header("x-audit-volume", format!("volume-{caller}"))
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    let opened = std::time::Instant::now();
    stream.finish().await.unwrap();
    assert_eq!(stream.recv_response().await.unwrap().status(), 200);
    let headers = std::time::Instant::now();
    let mut received = Vec::new();
    while let Some(chunk) = stream.recv_chunk().await.unwrap() {
        received.extend_from_slice(chunk.as_bytes());
    }
    let finished = std::time::Instant::now();
    assert_eq!(received.len(), size);
    assert!(received.iter().all(|byte| *byte == 0xa5));
    [
        finished.duration_since(start),
        opened.duration_since(start),
        headers.duration_since(opened),
        finished.duration_since(headers),
    ]
    .map(|duration| duration.as_secs_f64() * 1000.0)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    tokio::time::timeout(Duration::from_secs(180), run())
        .await
        .expect("peer diagnostic timed out");
}

async fn run() {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca = Identity {
        cert: params.self_signed(&key).unwrap(),
        key,
    };
    let identities: Vec<_> = (0..3).map(|i| peer(&ca, &format!("server-{i}"))).collect();
    let pins: Vec<_> = identities
        .iter()
        .map(|identity| spki_sha256_from_der(identity.cert.der()).unwrap())
        .collect();
    let cancel = CancellationToken::new();
    let dispatched = Arc::new(AtomicUsize::new(0));
    let mut servers = Vec::new();
    let mut clients = Vec::new();
    let mut destinations = Vec::new();
    for identity in &identities {
        let server_config = load_peer_server_config_from_pem(
            identity.cert.pem().as_bytes(),
            identity.key.serialize_pem().as_bytes(),
            ca.cert.pem().as_bytes(),
        )
        .unwrap();
        let endpoint =
            quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
        destinations.push(PeerDestination::new(
            endpoint.local_addr().unwrap(),
            "localhost",
        ));
        let allowed = pins.clone();
        let policy = move |identity: &TlsIdentity, request: &Request<()>| {
            allowed.contains(&identity.spki_sha256())
                && request.method() == "GET"
                && matches!(request.uri().path(), "/0" | "/4096" | "/65536")
        };
        let observed = dispatched.clone();
        let payload = Bytes::from(vec![0xa5; 65536]);
        let handler =
            move |_identity: TlsIdentity, request: Request<()>, mut stream: PeerServerStream| {
                let payload = payload.clone();
                let observed = observed.clone();
                async move {
                    while let Some(chunk) = stream.recv_chunk().await? {
                        assert!(chunk.as_bytes().is_empty());
                    }
                    let size: usize = request.uri().path()[1..].parse().unwrap();
                    observed.fetch_add(1, Ordering::Relaxed);
                    stream
                        .send_response(Response::builder().status(200).body(()).unwrap())
                        .await?;
                    if size != 0 {
                        stream.send_chunk(payload.slice(..size)).await?;
                    }
                    stream.finish().await
                }
            };
        let limits = PeerTransportLimits {
            drain_timeout: Duration::from_secs(2),
            ..Default::default()
        };
        let server = PeerServer::new(
            endpoint,
            ApprovedPeerPins::new(pins.clone()).unwrap(),
            limits.clone(),
            policy,
            handler,
        )
        .unwrap();
        servers.push(tokio::spawn(server.serve(cancel.clone())));
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(
            load_peer_client_config_from_pem(
                identity.cert.pem().as_bytes(),
                identity.key.serialize_pem().as_bytes(),
                ca.cert.pem().as_bytes(),
            )
            .unwrap(),
        );
        clients.push(Arc::new(
            PeerClient::new(
                endpoint,
                ApprovedPeerPins::new(pins.clone()).unwrap(),
                limits,
            )
            .unwrap(),
        ));
    }
    println!(
        "bytes,clients,repeat,count,p50_ms,p99_ms,open_p99_ms,headers_p99_ms,body_p99_ms,pooled_connections,dispatched"
    );
    for size in [0, 4096, 65536] {
        for callers in [1, 5, 10, 25, 50, 100] {
            for repeat in 0..2 {
                let before = dispatched.load(Ordering::Relaxed);
                let barrier = Arc::new(tokio::sync::Barrier::new(callers));
                let mut tasks = tokio::task::JoinSet::new();
                for caller in 0..callers {
                    let node = caller % 3;
                    let client = clients[node].clone();
                    let destination = destinations[(node + 1) % 3].clone();
                    let barrier = barrier.clone();
                    tasks.spawn(async move {
                        let mut samples = Vec::with_capacity(100);
                        for sample in 0..=100 {
                            barrier.wait().await;
                            let result = request(&client, destination.clone(), size, caller).await;
                            if sample != 0 {
                                samples.push(result);
                            }
                        }
                        samples
                    });
                }
                let mut samples = Vec::new();
                while let Some(task) = tasks.join_next().await {
                    samples.extend(task.unwrap());
                }
                let count = samples.len();
                assert_eq!(count, callers * 100);
                let calls = dispatched.load(Ordering::Relaxed) - before;
                assert_eq!(calls, callers * 101);
                let mut connections = 0;
                for client in &clients {
                    connections += client.pool_stats().await.pooled_connections.len();
                }
                assert!(connections <= 3);
                let mut stages = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
                for sample in samples {
                    for (stage, value) in stages.iter_mut().zip(sample) {
                        stage.push(value);
                    }
                }
                for stage in &mut stages {
                    stage.sort_by(f64::total_cmp);
                }
                let p99 = (count * 99).div_ceil(100) - 1;
                println!(
                    "{size},{callers},{repeat},{count},{:.6},{:.6},{:.6},{:.6},{:.6},{connections},{calls}",
                    stages[0][(count * 50).div_ceil(100) - 1],
                    stages[0][p99],
                    stages[1][p99],
                    stages[2][p99],
                    stages[3][p99]
                );
            }
        }
    }
    for client in clients {
        client.shutdown().await.unwrap();
    }
    cancel.cancel();
    for server in servers {
        server.await.unwrap().unwrap();
    }
}
