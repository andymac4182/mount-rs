#![cfg(feature = "http3")]
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

#[derive(Default)]
struct CompletionGate {
    payload_sent: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

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

#[tokio::test]
async fn pooled_http3_filesystem_authorization_fencing_and_shutdown() {
    run_case(false).await;
}

static DIRECT_WRITE_BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

// The load phases model 100 independent clients; a single-threaded runtime
// serialized their request I/O and inflated client-observed latency.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn redis_discovery_forwards_filesystem_over_http3() {
    run_case(true).await;
}

#[test]
fn membership_file_rejects_invalid_pins_and_ambiguous_nodes() {
    let peer = serde_json::json!({
        "node":"owner", "boot":"boot-a", "address":"127.0.0.1:443",
        "tls_server_name":"localhost", "spki_sha256":"1a".repeat(32)
    });
    for pin in [
        serde_json::json!("abc"),
        serde_json::json!("xx".repeat(32)),
        serde_json::json!(42),
    ] {
        let mut bad = peer.clone();
        bad["spki_sha256"] = pin;
        assert!(
            mount_rs_cluster::PeerDirectory::from_reader(
                serde_json::json!([bad]).to_string().as_bytes()
            )
            .is_err()
        );
    }
    let mut extra = peer.clone();
    extra["tls_name"] = serde_json::json!("ignored");
    for entries in [
        serde_json::json!([extra]),
        serde_json::json!([peer, peer]),
        serde_json::json!([]),
    ] {
        assert!(
            mount_rs_cluster::PeerDirectory::from_reader(entries.to_string().as_bytes()).is_err()
        );
    }
}

async fn run_case(with_redis: bool) {
    let deadline = if std::env::var_os("MOUNT_RS_TEST_CLI_LOAD").is_some() {
        900
    } else if std::env::var_os("MOUNT_RS_TEST_CLI_TIDB").is_some() {
        60
    } else {
        15
    };
    tokio::time::timeout(Duration::from_secs(deadline), async {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca = Identity {
            cert: params.self_signed(&key).unwrap(),
            key,
        };
        let owner = peer(&ca, "owner");
        let ingress = peer(&ca, "ingress");
        let owner_pin = spki_sha256_from_der(owner.cert.der()).unwrap();
        let ingress_pin = spki_sha256_from_der(ingress.cert.der()).unwrap();
        let config = load_peer_server_config_from_pem(
            owner.cert.pem().as_bytes(),
            owner.key.serialize_pem().as_bytes(),
            ca.cert.pem().as_bytes(),
        )
        .unwrap();
        let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        let limits = PeerTransportLimits {
            drain_timeout: Duration::from_secs(2),
            ..Default::default()
        };
        let cancel = CancellationToken::new();
        let membership = serde_json::json!([{
            "node": "ingress", "boot": "boot-a", "address": address,
            "tls_server_name": "localhost", "spki_sha256": ingress_pin.to_hex()
        }]);
        let directory =
            mount_rs_cluster::PeerDirectory::from_reader(membership.to_string().as_bytes())
                .unwrap();
        let pins = directory.approved_pins().unwrap();
        let replacement = peer(&ca, "ingress");
        assert!(!directory.accepts(&leaf_identity_from_der(replacement.cert.der()).unwrap()));
        assert!(!directory.accepts(&leaf_identity_from_der(owner.cert.der()).unwrap()));
        let policy = move |identity: &TlsIdentity, request: &Request<()>| {
            directory.accepts(identity) && request.uri().path() == "/transport-check"
        };
        let dispatched = Arc::new(AtomicUsize::new(0));
        let observed = dispatched.clone();
        let handler =
            move |_identity: TlsIdentity, _request: Request<()>, mut stream: PeerServerStream| {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    let mut received = Vec::new();
                    while let Some(chunk) = stream.recv_chunk().await? {
                        received.extend_from_slice(chunk.as_bytes());
                    }
                    stream
                        .send_response(Response::builder().status(200).body(()).unwrap())
                        .await?;
                    stream.send_chunk(Bytes::from(received)).await?;
                    stream.finish().await
                }
            };
        let server = PeerServer::new(endpoint, pins, limits.clone(), policy, handler).unwrap();
        let server_task = tokio::spawn(server.serve(cancel.clone()));
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(
            load_peer_client_config_from_pem(
                ingress.cert.pem().as_bytes(),
                ingress.key.serialize_pem().as_bytes(),
                ca.cert.pem().as_bytes(),
            )
            .unwrap(),
        );
        let client = Arc::new(
            PeerClient::new(
                endpoint,
                ApprovedPeerPins::new([owner_pin]).unwrap(),
                limits,
            )
            .unwrap(),
        );
        let destination = PeerDestination::new(address, "localhost");
        for i in 0..10 {
            let mut stream = client
                .open(
                    destination.clone(),
                    Request::builder()
                        .method("POST")
                        .uri("https://localhost/transport-check")
                        .body(())
                        .unwrap(),
                )
                .await
                .unwrap();
            let expected = format!("mount-rs-peer-{i}").into_bytes();
            stream
                .send_chunk(Bytes::from(expected.clone()))
                .await
                .unwrap();
            stream.finish().await.unwrap();
            assert_eq!(stream.recv_response().await.unwrap().status(), 200);
            let mut received = Vec::new();
            while let Some(chunk) = stream.recv_chunk().await.unwrap() {
                received.extend_from_slice(chunk.as_bytes());
            }
            assert_eq!(received, expected);
        }
        assert_eq!(client.pool_stats().await.pooled_connections.len(), 1);
        assert_eq!(dispatched.load(Ordering::SeqCst), 10);
        let rejected = client
            .open(
                destination,
                Request::builder()
                    .uri("https://localhost/forbidden")
                    .body(())
                    .unwrap(),
            )
            .await;
        if let Ok(mut stream) = rejected {
            let _ = stream.finish().await;
            assert!(stream.recv_response().await.is_err());
        }
        assert_eq!(dispatched.load(Ordering::SeqCst), 10);
        // Reuse the authenticated transport for the actual filesystem HTTP application.
        let filesystem = Arc::new(
            mount_rs_sdk::Filesystem::split_with_runtime(
                mount_rs_sdk::SplitOptions::memory("http3-owner", 65536),
                mount_rs_sdk::SplitRuntimeOptions {
                    read_parallelism: 1,
                    atime_policy: mount_rs_sdk::AtimePolicy::NoAtime,
                    read_lease_cache_window: Duration::from_secs(5),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let config = load_peer_server_config_from_pem(
            owner.cert.pem().as_bytes(),
            owner.key.serialize_pem().as_bytes(),
            ca.cert.pem().as_bytes(),
        )
        .unwrap();
        let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let route = mount_rs_cluster::VolumeRoute {
            volume: "volume".into(),
            node: "owner".into(),
            boot: "boot".into(),
            registration: "registration".into(),
            fence: filesystem.checked_owner_fence().await.unwrap(),
            address: endpoint.local_addr().unwrap(),
            tls_server_name: "localhost".into(),
        };
        let destination = PeerDestination::new(route.address, route.tls_server_name.clone());
        let handler = mount_rs_cluster::OwnerHttpHandler::new(
            filesystem.clone(),
            route.clone(),
            "secret-token".into(),
            mount_rs_http::HttpServerOptions::default(),
        )
        .await
        .unwrap()
        .with_sessions(
            mount_rs_cluster::SessionCredentials::new(vec![
                ("alice".into(), "a".repeat(32)),
                ("bob".into(), "b".repeat(32)),
            ])
            .unwrap(),
            2,
            2,
            Duration::from_secs(60),
            mount_rs_cluster::SessionBudget::new(2).unwrap(),
        )
        .unwrap();
        // Test-only malformed peer path. Normal requests retain the real owner
        // authorization/fencing handler; this path deliberately violates framing.
        let normal_handler = handler.clone();
        let completion_gate = Arc::new(CompletionGate::default());
        let peer_completion_gate = completion_gate.clone();
        let peer_handler =
            move |identity: TlsIdentity, request: Request<()>, mut stream: PeerServerStream| {
                let handler = normal_handler.clone();
                let completion_gate = peer_completion_gate.clone();
                async move {
                    if request.uri().path() == "/v1/drives/volume/fs/completion-test" {
                        let query = request.uri().query().unwrap_or_default();
                        let empty = query.contains("empty=1");
                        let reset = query.contains("reset=1");
                        while stream.recv_chunk().await?.is_some() {}
                        stream
                            .send_response(
                                Response::builder()
                                    .status(200)
                                    .header("content-length", if empty { "0" } else { "4" })
                                    .body(())
                                    .unwrap(),
                            )
                            .await?;
                        if !empty {
                            stream.send_chunk(Bytes::from_static(b"done")).await?;
                        }
                        completion_gate.payload_sent.notify_one();
                        completion_gate.release.notified().await;
                        if reset {
                            stream.cancel();
                            return Ok(());
                        }
                        return stream.finish().await;
                    }
                    if request.uri().path() == "/v1/drives/volume/fs/truncated-test" {
                        while stream.recv_chunk().await?.is_some() {}
                        stream
                            .send_response(
                                Response::builder()
                                    .status(200)
                                    .header("content-length", "4096")
                                    .body(())
                                    .unwrap(),
                            )
                            .await?;
                        stream.send_chunk(Bytes::from_static(b"short")).await?;
                        stream.finish().await
                    } else {
                        handler.handle(identity, request, stream).await
                    }
                }
            };
        let cancel_fs = CancellationToken::new();
        let peer = PeerServer::new(
            endpoint,
            ApprovedPeerPins::new([ingress_pin]).unwrap(),
            PeerTransportLimits::default(),
            move |identity: &TlsIdentity, _request: &Request<()>| {
                identity.spki_sha256() == ingress_pin
            },
            peer_handler,
        )
        .unwrap();
        let task_fs = tokio::spawn(peer.serve(cancel_fs.clone()));
        let data: Vec<u8> = (0..131123).map(|i| (i % 251) as u8).collect();
        let written = file_request(
            &client,
            &destination,
            &route,
            "PUT",
            "secret-token",
            &data,
            None,
        )
        .await;
        assert!(written.0.is_success());
        // The PUT above only touched mutation counters. The following GET is
        // the first read, so its wire timings must equal the read totals.
        let observer = mount_rs_cluster::OwnerHttpRouter::new(vec![handler.clone()]).unwrap();
        let request = Request::get("https://localhost/v1/drives/volume/fs/data")
            .header(
                mount_rs_cluster::ROUTE_HEADER,
                serde_json::to_string(&route).unwrap(),
            )
            .header(mount_rs_cluster::HOP_HEADER, "1")
            .header("authorization", "Bearer secret-token")
            .header("range", "bytes=0-0")
            .header("x-mount-peer-timing", "1")
            .body(())
            .unwrap();
        let mut timed = client.open(destination.clone(), request).await.unwrap();
        timed.finish().await.unwrap();
        let response = timed.recv_response().await.unwrap();
        assert!(response.status().is_success());
        while timed.recv_chunk().await.unwrap().is_some() {}
        let totals = observer.timing_snapshots()[0].reads;
        for (name, stage) in [
            ("x-mount-owner-admission-ns", totals.admission),
            ("x-mount-owner-application-ns", totals.application),
        ] {
            assert_eq!(stage.completed, 1);
            assert_eq!(
                response.headers()[name]
                    .to_str()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
                stage.nanoseconds
            );
        }

        let read = file_request(
            &client,
            &destination,
            &route,
            "GET",
            "secret-token",
            &[],
            None,
        )
        .await;
        assert_eq!(read.0, 200);
        assert_eq!(read.1, data);
        let range = file_request(
            &client,
            &destination,
            &route,
            "GET",
            "secret-token",
            &[],
            Some("bytes=4096-8191"),
        )
        .await;
        assert_eq!(range.0, 206);
        assert_eq!(range.1, data[4096..8192]);
        assert_eq!(
            file_request(
                &client,
                &destination,
                &route,
                "GET",
                "wrong-token",
                &[],
                None
            )
            .await
            .0,
            401
        );
        let mut stale = route.clone();
        stale.fence += 1;
        let rejection = file_request(
            &client,
            &destination,
            &stale,
            "PUT",
            "secret-token",
            &[],
            None,
        )
        .await;
        assert_eq!(rejection.0, 409);
        assert!(rejection.2);
        let request = Request::builder()
            .method("DELETE")
            .uri("https://localhost/v1/drives/volume/fs/data")
            .header(
                mount_rs_cluster::ROUTE_HEADER,
                serde_json::to_string(&route).unwrap(),
            )
            .header(mount_rs_cluster::HOP_HEADER, "2")
            .header("authorization", "Bearer secret-token")
            .body(())
            .unwrap();
        let mut looped = client.open(destination.clone(), request).await.unwrap();
        looped.finish().await.unwrap();
        assert_eq!(looped.recv_response().await.unwrap().status(), 409);
        assert!(looped.recv_chunk().await.unwrap().is_none());
        drop(looped);
        assert_eq!(
            file_request(
                &client,
                &destination,
                &route,
                "GET",
                "secret-token",
                &[],
                None
            )
            .await
            .1,
            data
        );
        if with_redis {
            exercise_forwarder(
                client.clone(),
                filesystem.clone(),
                &route,
                owner_pin,
                &data,
                completion_gate.clone(),
                (&ingress, &ca, &owner),
            )
            .await;
        }
        if !with_redis {
            filesystem.shutdown().await.unwrap();
        }
        let lost = file_request(
            &client,
            &destination,
            &route,
            "GET",
            "secret-token",
            &[],
            None,
        )
        .await;
        assert_eq!(lost.0, 503);
        assert!(lost.2);
        cancel_fs.cancel();
        task_fs.await.unwrap().unwrap();
        handler.close().await.unwrap();
        client.shutdown().await.unwrap();
        cancel.cancel();
        server_task.await.unwrap().unwrap();
    })
    .await
    .expect("HTTP/3 test must finish within its deadline");
}

async fn file_request(
    client: &PeerClient,
    destination: &PeerDestination,
    route: &mount_rs_cluster::VolumeRoute,
    method: &str,
    token: &str,
    data: &[u8],
    range: Option<&str>,
) -> (http::StatusCode, Vec<u8>, bool) {
    let mut request = Request::builder()
        .method(method)
        .uri("https://localhost/v1/drives/volume/fs/data")
        .header(
            mount_rs_cluster::ROUTE_HEADER,
            serde_json::to_string(route).unwrap(),
        )
        .header(mount_rs_cluster::HOP_HEADER, "1")
        .header("authorization", format!("Bearer {token}"))
        .header("content-length", data.len());
    if let Some(range) = range {
        request = request.header("range", range);
    }
    let mut stream = client
        .open(destination.clone(), request.body(()).unwrap())
        .await
        .unwrap();
    if !data.is_empty() {
        stream.send_chunked(data).await.unwrap();
    }
    stream.finish().await.unwrap();
    let response = stream.recv_response().await.unwrap();
    let not_dispatched = response
        .headers()
        .get("x-mount-dispatch")
        .is_some_and(|v| v == "not-dispatched");
    let mut received = Vec::new();
    while let Some(chunk) = stream.recv_chunk().await.unwrap() {
        received.extend_from_slice(chunk.as_bytes());
    }
    (response.status(), received, not_dispatched)
}

#[cfg(unix)]
async fn exercise_cli_ingress(
    binary: std::ffi::OsString,
    namespace: &str,
    route: &mount_rs_cluster::VolumeRoute,
    pin: SpkiSha256,
    (ingress, ca, owner): (&Identity, &Identity, &Identity),
) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let dir = tempfile::tempdir().unwrap();
    let write = |name, content| std::fs::write(dir.path().join(name), content).unwrap();
    write("client.pem", ingress.cert.pem());
    write("client.key", ingress.key.serialize_pem());
    write("ca.pem", ca.cert.pem());
    let namespace = format!("{namespace}-cli");
    let owner_address = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = owner_address.local_addr().unwrap();
    write("owner.pem", owner.cert.pem());
    write("owner.key", owner.key.serialize_pem());
    let other = peer(ca, "ingress-2");
    write("other.pem", other.cert.pem());
    write("other.key", other.key.serialize_pem());
    let other_address = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    write("peers.json", serde_json::json!([{
        "node":route.node,"boot":route.boot,"address":address,
        "tls_server_name":route.tls_server_name,"spki_sha256":pin.to_hex()
    },{
        "node":"ingress","boot":"ingress-boot","address":route.address,
        "tls_server_name":"localhost","spki_sha256":spki_sha256_from_der(ingress.cert.der()).unwrap().to_hex()
    },{
        "node":"ingress-2","boot":"ingress-2-boot","address":other_address.local_addr().unwrap(),
        "tls_server_name":"localhost","spki_sha256":spki_sha256_from_der(other.cert.der()).unwrap().to_hex()
    }]).to_string());
    let http2 = std::env::var_os("MOUNT_RS_TEST_CLI_PUBLIC_H2").is_some();
    let ingress_config = serde_json::json!({"version":1,"http":{
        "host":"127.0.0.1","port":0,"http2_prior_knowledge":http2,"drives":[],"forwarding":{
            "peers":"peers.json","certificate":"client.pem","private_key":"client.key",
            "ca":"ca.pem","redis_url":{"env":"MOUNT_RS_REDIS_TEST_URL"},
            "namespace":namespace,"bind":"127.0.0.1:0",
            "refresh_active_routes":std::env::var_os("MOUNT_RS_TEST_CLI_REFRESH_ROUTES").is_some(),
            "route_refresh_idle_ttls":std::env::var("MOUNT_RS_TEST_CLI_REFRESH_IDLE_TTLS")
                .map(|value| value.parse::<u32>().expect("expected an idle TTL count")).unwrap_or(5)
        }
    }});
    write("ingress.json", ingress_config.to_string());
    let mut second_config = ingress_config.clone();
    second_config["http"]["forwarding"]["certificate"] = "other.pem".into();
    second_config["http"]["forwarding"]["private_key"] = "other.key".into();
    write("ingress-2.json", second_config.to_string());
    let mut owner_config = ingress_config;
    owner_config["http"]["forwarding"]["owner"] = true.into();
    owner_config["http"]["forwarding"]["certificate"] = "owner.pem".into();
    owner_config["http"]["forwarding"]["private_key"] = "owner.key".into();
    owner_config["http"]["remote_sessions"] = true.into();
    owner_config["http"]["drives"] = serde_json::json!([{"id":"volume","token":{"env":"CLI_TEST_TOKEN"},
        "driver":{"kind":"splitstore","storage":{"metadata":{"kind":"memory"},"blocks":{"kind":"memory"}}}}]);
    let test_tidb = std::env::var_os("MOUNT_RS_TEST_CLI_TIDB").is_some();
    let rustfs = std::env::var_os("MOUNT_RS_TEST_CLI_RUSTFS").is_some();
    let memory_blocks = std::env::var_os("MOUNT_RS_TEST_CLI_MEMORY_BLOCKS").is_some();
    assert!(!(rustfs && memory_blocks));
    assert!(!(rustfs || memory_blocks) || test_tidb);
    if test_tidb {
        std::fs::create_dir(dir.path().join("blocks")).unwrap();
        owner_config["http"]["tidb_metadata_pool_limit"] = 16.into();
        let storage = &mut owner_config["http"]["drives"][0]["driver"]["storage"];
        storage["metadata"] = serde_json::json!({"kind":"tidb","connection":{"env":"MOUNT_RS_TIDB_URL"},
            "volume_key":namespace,"normalized":true,"durable":true});
        storage["blocks"] = serde_json::json!({"kind":"local","path":"blocks"});
        if memory_blocks {
            storage["blocks"] = serde_json::json!({"kind":"memory"});
        } else if rustfs {
            storage["blocks"] = serde_json::json!({
                "kind":"r2","endpoint":std::env::var("R2_ENDPOINT").unwrap(),
                "bucket":std::env::var("R2_BUCKET").unwrap(),
                "prefix":format!("{}/{}",std::env::var("RUSTFS_COMBO_PREFIX").unwrap(),namespace),
                "access_key_id":{"env":"R2_ACCESS_KEY_ID"},"secret_access_key":{"env":"R2_SECRET_ACCESS_KEY"},
                "durable":true
            });
        }
        storage["atime"] = "noatime".into();
        storage["block_cache"] = serde_json::json!({"max_entries":128,"max_bytes":1048576});
        if std::env::var_os("MOUNT_RS_TEST_CLI_MEMORY_METADATA").is_some() {
            // Diagnostic only: removes TiDB from this host's CPU budget. Later
            // restart checks in this test require durable metadata and fail.
            storage["metadata"] = serde_json::json!({"kind":"memory"});
            owner_config["http"]
                .as_object_mut()
                .unwrap()
                .remove("tidb_metadata_pool_limit");
        }
    }
    if std::env::var_os("MOUNT_RS_TEST_CLI_CACHE_REPORT").is_some() {
        owner_config["http"]["cache_report_interval_seconds"] = 10.into();
    }
    let load = std::env::var_os("MOUNT_RS_TEST_CLI_LOAD").is_some();
    if load {
        assert!(test_tidb);
        let drives = owner_config["http"]["drives"].as_array_mut().unwrap();
        for i in 1..100 {
            let mut drive = drives[0].clone();
            drive["id"] = format!("volume-{i}").into();
            if drive["driver"]["storage"]["metadata"]["kind"] == "tidb" {
                drive["driver"]["storage"]["metadata"]["volume_key"] =
                    format!("{namespace}-{i}").into();
            }
            if rustfs {
                let prefix = drives[0]["driver"]["storage"]["blocks"]["prefix"]
                    .as_str()
                    .unwrap();
                drive["driver"]["storage"]["blocks"]["prefix"] = format!("{prefix}-{i}").into();
            } else if !memory_blocks {
                let path = format!("blocks-{i}");
                std::fs::create_dir(dir.path().join(&path)).unwrap();
                drive["driver"]["storage"]["blocks"]["path"] = path.into();
            }
            drives.push(drive);
        }
    }
    write("owner.json", owner_config.to_string());
    let launch = |name| {
        let mut command = tokio::process::Command::new(&binary);
        if let Ok(threads) = std::env::var("MOUNT_RS_TEST_SERVER_WORKER_THREADS") {
            command.env("TOKIO_WORKER_THREADS", threads);
        }
        let mut child = command
            .env("CLI_TEST_TOKEN", "a".repeat(32))
            .args(["serve-http", "--config"])
            .arg(dir.path().join(name))
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        async move {
            let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
            let ready = lines
                .next_line()
                .await
                .unwrap()
                .expect("CLI must report listener readiness");
            let base = ready
                .strip_prefix("http listening at ")
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .to_owned();
            (child, base, lines)
        }
    };
    fn public_client(http2: bool) -> reqwest::Client {
        let builder = reqwest::Client::builder();
        let builder = if http2 {
            builder.http2_prior_knowledge()
        } else {
            builder.http1_only()
        };
        builder.build().unwrap()
    }
    async fn run_cli_load(bases: &[&str], servers: usize, http2: bool) {
        if std::env::var_os("MOUNT_RS_TEST_CLI_SESSION_LOAD").is_some() {
            run_cli_session_load(bases, servers, http2).await;
            return;
        }
        let mut clients = Vec::new();
        for i in 0..100 {
            let http = public_client(http2);
            let base = bases[i % bases.len()];
            let volume = if i == 0 {
                "volume".into()
            } else {
                format!("volume-{i}")
            };
            let path = format!("{base}/v1/drives/{volume}");
            let bytes = vec![i as u8; 4096];
            let seed = http
                .put(format!("{path}/fs/sample"))
                .bearer_auth("a".repeat(32))
                .body(bytes.clone())
                .send()
                .await
                .unwrap();
            assert_eq!(
                seed.version(),
                if http2 {
                    http::Version::HTTP_2
                } else {
                    http::Version::HTTP_11
                }
            );
            assert!(seed.status().is_success());
            seed.bytes().await.unwrap();
            clients.push((http, path, bytes));
        }
        let paced = std::env::var_os("MOUNT_RS_TEST_CLI_PACED").is_some();
        let pairs_per_sec: std::num::NonZeroU32 = std::env::var("MOUNT_RS_TEST_CLI_PAIRS_PER_SEC")
            .unwrap_or_else(|_| "80".into())
            .parse()
            .expect("expected a positive CLI load rate");
        let levels: &[usize] = if paced { &[100] } else { &[1, 10, 50, 100] };
        for &level in levels {
            for stat in [true, false] {
                let mut jobs = tokio::task::JoinSet::new();
                let barrier = Arc::new(tokio::sync::Barrier::new(level + 1));
                let epoch = tokio::time::Instant::now() + Duration::from_millis(100);
                for (client_id, (http, path, bytes)) in
                    clients.iter().take(level).cloned().enumerate()
                {
                    let barrier = barrier.clone();
                    jobs.spawn(async move {
                        let mut samples = Vec::new();
                        let url = format!("{path}/{}/sample", if stat { "stat" } else { "fs" });
                        let writer = format!("{path}/fs/writing");
                        barrier.wait().await;
                        let rounds = if paced { 100 } else { 20 };
                        for round in 0..rounds {
                            // Schedule each volume on a common monotonic clock.
                            let target = epoch
                                + Duration::from_secs_f64(
                                    (round * level + client_id) as f64
                                        / f64::from(pairs_per_sec.get()),
                                );
                            if paced {
                                tokio::time::sleep_until(target).await;
                            }
                            let read = async {
                                let start = tokio::time::Instant::now();
                                let lag_ms = if paced {
                                    start.duration_since(target).as_secs_f64() * 1000.
                                } else {
                                    0.
                                };
                                let response = http
                                    .get(&url)
                                    .bearer_auth("a".repeat(32))
                                    .send()
                                    .await
                                    .unwrap();
                                let headers_ms = start.elapsed().as_secs_f64() * 1000.;
                                assert!(response.status().is_success());
                                let body = response.bytes().await.unwrap();
                                let elapsed = start.elapsed().as_secs_f64() * 1000.;
                                if stat {
                                    let meta: serde_json::Value =
                                        serde_json::from_slice(&body).unwrap();
                                    assert_eq!(meta["size"], 4096);
                                } else {
                                    assert_eq!(body.as_ref(), bytes);
                                }
                                (elapsed, headers_ms, lag_ms)
                            };
                            let write = async {
                                let response = http
                                    .put(&writer)
                                    .bearer_auth("a".repeat(32))
                                    .body(bytes.clone())
                                    .send()
                                    .await
                                    .unwrap();
                                assert!(response.status().is_success());
                                response.bytes().await.unwrap();
                            };
                            let (ms, ()) = tokio::join!(read, write);
                            samples.push(ms);
                        }
                        samples
                    });
                }
                let phase_start = std::time::Instant::now();
                barrier.wait().await;
                let mut times = Vec::new();
                while let Some(job) = jobs.join_next().await {
                    times.extend(job.unwrap());
                }
                let writes_per_sec = times.len() as f64 / phase_start.elapsed().as_secs_f64();
                times.sort_by(|a, b| a.0.total_cmp(&b.0));
                let count = times.len();
                let (p99, headers99, _) = times[(count * 99).div_ceil(100) - 1];
                let (p9995, headers9995, _) = times[(count * 9995).div_ceil(10000) - 1];
                if paced {
                    times.sort_by(|a, b| a.2.total_cmp(&b.2));
                    let lag99 = times[(count * 99).div_ceil(100) - 1].2;
                    let scheduled_ms = |sample: &(f64, f64, f64)| sample.0 + sample.2;
                    times.sort_by(|a, b| scheduled_ms(a).total_cmp(&scheduled_ms(b)));
                    let scheduled99 = scheduled_ms(&times[(count * 99).div_ceil(100) - 1]);
                    let scheduled9995 = scheduled_ms(&times[(count * 9995).div_ceil(10000) - 1]);
                    println!(
                        "CLI_PACING servers={servers} stat={stat} target_pairs_per_sec={pairs_per_sec} start_lag_p99_ms={lag99:.3} scheduled_p99_ms={scheduled99:.3} scheduled_p9995_ms={scheduled9995:.3}"
                    );
                }
                println!(
                    "CLI_LOAD servers={servers} clients={level} stat={stat} count={count} p99_ms={p99:.3} p9995_ms={p9995:.3} p99_to_headers_ms={headers99:.3} p99_body_ms={:.3} p9995_to_headers_ms={headers9995:.3} p9995_body_ms={:.3} writes_per_sec={writes_per_sec:.1}",
                    p99 - headers99,
                    p9995 - headers9995
                );
            }
        }
    }
    async fn run_cli_session_load(bases: &[&str], servers: usize, http2: bool) {
        use mount_rs_core::FsDriver;
        use mount_rs_remote::RemoteFs;

        // Optional idle-route experiment: Redis command deltas per phase, with
        // an open quiet interval before load and a disconnected interval after.
        let idle_seconds: u64 = std::env::var("MOUNT_RS_TEST_CLI_IDLE_SECONDS")
            .map(|value| value.parse().expect("expected idle seconds"))
            .unwrap_or(0);
        let registry =
            redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
        let mut metrics = registry.get_multiplexed_async_connection().await.unwrap();
        async fn command_stats(
            connection: &mut redis::aio::MultiplexedConnection,
        ) -> std::collections::BTreeMap<String, u64> {
            let text: String = redis::cmd("INFO")
                .arg("commandstats")
                .query_async(connection)
                .await
                .unwrap();
            text.lines()
                .filter_map(|line| {
                    let (name, rest) = line.strip_prefix("cmdstat_")?.split_once(':')?;
                    let calls = rest
                        .split(',')
                        .find_map(|field| field.strip_prefix("calls="))?
                        .parse()
                        .ok()?;
                    Some((name.to_owned(), calls))
                })
                .collect()
        }
        async fn redis_phase(
            connection: &mut redis::aio::MultiplexedConnection,
            phase: &str,
            since: &mut (std::time::Instant, std::collections::BTreeMap<String, u64>),
        ) {
            let now = command_stats(connection).await;
            let seconds = since.0.elapsed().as_secs_f64();
            let delta: std::collections::BTreeMap<_, _> = now
                .iter()
                .map(|(name, calls)| {
                    (
                        name.clone(),
                        calls - since.1.get(name).copied().unwrap_or(0),
                    )
                })
                .filter(|(_, calls)| *calls != 0)
                .collect();
            println!(
                "REDIS_PHASE phase={phase} seconds={seconds:.3} calls={}",
                serde_json::to_string(&delta).unwrap()
            );
            *since = (std::time::Instant::now(), command_stats(connection).await);
        }
        async fn idle_windows(
            connection: &mut redis::aio::MultiplexedConnection,
            phase: &str,
            seconds: u64,
            since: &mut (std::time::Instant, std::collections::BTreeMap<String, u64>),
        ) {
            for window in 0..seconds.div_ceil(5) {
                tokio::time::sleep(Duration::from_secs(5.min(seconds - window * 5))).await;
                redis_phase(connection, &format!("{phase}-{}s", (window + 1) * 5), since).await;
            }
        }
        let writes = std::env::var("MOUNT_RS_TEST_CLI_SESSION_WRITES").as_deref() != Ok("0");
        let iterations: usize = std::env::var("MOUNT_RS_TEST_CLI_SESSION_ITERATIONS")
            .map(|value| value.parse().expect("expected iterations"))
            .unwrap_or(100);
        let mut since = (std::time::Instant::now(), command_stats(&mut metrics).await);
        if idle_seconds != 0 {
            println!(
                "IDLE_EXPERIMENT idle_seconds={idle_seconds} refresh_idle_ttls={} refresh_routes={}",
                std::env::var("MOUNT_RS_TEST_CLI_REFRESH_IDLE_TTLS").unwrap_or_else(|_| "5".into()),
                std::env::var_os("MOUNT_RS_TEST_CLI_REFRESH_ROUTES").is_some()
            );
            idle_windows(&mut metrics, "pre-setup", 10, &mut since).await;
        }
        let mut clients = Vec::new();
        let mut direct_writers = Vec::new();
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let mut keepalives = tokio::task::JoinSet::new();
        for id in 0..100 {
            let volume = if id == 0 {
                "volume".into()
            } else {
                format!("volume-{id}")
            };
            let fs = RemoteFs::connect_with_http2(
                bases[id % bases.len()],
                &volume,
                &"a".repeat(32),
                Duration::from_secs(30),
                http2,
            )
            .await
            .unwrap();
            let worker_fs = fs.clone();
            let worker_receiver = receiver.clone();
            keepalives.spawn(async move { worker_fs.run_keepalive(worker_receiver).await });
            let data = vec![id as u8; 4096];
            let read = fs.open("/sample", "w+", 0o600).await.unwrap();
            assert_eq!(read.write(&data, Some(0)).await.unwrap(), data.len());
            // Diagnostic: send the paired writes straight to the owner so
            // forwarded reads can be measured apart from forwarded writes.
            let write = if std::env::var_os("MOUNT_RS_TEST_CLI_DIRECT_WRITES").is_some() {
                let direct = RemoteFs::connect_with_http2(
                    DIRECT_WRITE_BASE.get().unwrap(),
                    &volume,
                    &"a".repeat(32),
                    Duration::from_secs(30),
                    http2,
                )
                .await
                .unwrap();
                let worker = direct.clone();
                let worker_receiver = receiver.clone();
                keepalives.spawn(async move { worker.run_keepalive(worker_receiver).await });
                let write = direct.open("/writing", "w", 0o600).await.unwrap();
                direct_writers.push(direct);
                write
            } else {
                fs.open("/writing", "w", 0o600).await.unwrap()
            };
            clients.push((fs, read, write, data, std::time::Instant::now()));
        }
        let compare_headers = std::env::var_os("MOUNT_RS_TEST_CLI_COMPARE_HEADER_READS").is_some();
        let phases: &[(bool, bool)] = if compare_headers {
            &[(true, false), (false, false), (false, true)]
        } else {
            &[(true, false), (false, false)]
        };
        if idle_seconds != 0 {
            redis_phase(&mut metrics, "setup", &mut since).await;
            idle_windows(&mut metrics, "idle-open", idle_seconds, &mut since).await;
        }
        async fn redis_commands(connection: &mut redis::aio::MultiplexedConnection) -> u64 {
            let info: redis::InfoDict = redis::cmd("INFO")
                .arg("stats")
                .query_async(connection)
                .await
                .unwrap();
            info.get("total_commands_processed").unwrap()
        }
        for &(stat, header_reads) in phases {
            if header_reads {
                for (fs, _, _, _, _) in &clients {
                    fs.enable_header_reads().await.unwrap();
                }
            }
            let barrier = Arc::new(tokio::sync::Barrier::new(clients.len() + 1));
            let burst = Arc::new(tokio::sync::Barrier::new(clients.len() + 1));
            let every_iteration =
                (std::env::var("MOUNT_RS_TEST_CLI_BURST_EVERY_ITERATION").as_deref() == Ok("1"))
                    .then(|| Arc::new(tokio::sync::Barrier::new(clients.len())));
            let mut tasks = tokio::task::JoinSet::new();
            for (id, (fs, read, write, data, setup_finished)) in clients.iter().cloned().enumerate()
            {
                let barrier = barrier.clone();
                let burst = burst.clone();
                let every_iteration = every_iteration.clone();
                tasks.spawn(async move {
                    let mut samples = Vec::new();
                    let mut buffer = vec![0; 4096];
                    barrier.wait().await;
                    let setup_age_ms = setup_finished.elapsed().as_millis();
                    for iteration in 0..iterations {
                        let measure = async {
                            let start = std::time::Instant::now();
                            if stat {
                                assert_eq!(fs.stat("/sample").await.unwrap().size, 4096);
                            } else {
                                assert_eq!(read.read(&mut buffer, Some(0)).await.unwrap(), 4096);
                            }
                            let elapsed = start.elapsed().as_secs_f64() * 1000.;
                            if !stat {
                                assert_eq!(buffer, data);
                            }
                            elapsed
                        };
                        let timed =
                            mount_rs_remote::BODY_TIMING.scope(std::cell::Cell::new(None), async {
                                let elapsed = measure.await;
                                let body =
                                    mount_rs_remote::BODY_TIMING.with(|slot| slot.get().unwrap());
                                (elapsed, iteration, body, id, setup_age_ms)
                            });
                        let (sample, written) = if writes {
                            tokio::join!(timed, async { Some(write.write(&data, Some(0)).await) })
                        } else {
                            (timed.await, None)
                        };
                        if let Some(written) = written {
                            assert_eq!(written.unwrap(), 4096);
                        }
                        samples.push(sample);
                        if iteration == 0 && idle_seconds != 0 {
                            // Separate the first synchronized burst from steady traffic.
                            burst.wait().await;
                        }
                        if let Some(every) = &every_iteration {
                            // Burst mode: all clients start each iteration together.
                            every.wait().await;
                        }
                    }
                    samples
                });
            }
            let commands_before = redis_commands(&mut metrics).await;
            let start = std::time::Instant::now();
            barrier.wait().await;
            let label = if stat {
                "stat"
            } else if header_reads {
                "header-read"
            } else {
                "read"
            };
            if idle_seconds != 0 {
                redis_phase(&mut metrics, &format!("pre-{label}"), &mut since).await;
                burst.wait().await;
                redis_phase(&mut metrics, &format!("{label}-first-burst"), &mut since).await;
            }
            let mut samples = Vec::new();
            while let Some(task) = tasks.join_next().await {
                samples.extend(task.unwrap());
            }
            let seconds = start.elapsed().as_secs_f64();
            let writes_per_sec = samples.len() as f64 / seconds;
            let commands_per_sec =
                (redis_commands(&mut metrics).await - commands_before) as f64 / seconds;
            if idle_seconds != 0 {
                redis_phase(&mut metrics, &format!("{label}-steady"), &mut since).await;
            }
            {
                let registry = |first: bool| {
                    samples
                        .iter()
                        .filter(|sample| {
                            (sample.1 == 0) == first && sample.2.forwarding_stages_ns[2].is_some()
                        })
                        .count()
                };
                let traced = samples
                    .iter()
                    .filter(|sample| sample.2.ingress_and_peer_ns.is_some())
                    .count();
                println!(
                    "ROUTE_SOURCE phase={label} traced={traced} first_burst_registry={} first_burst_count={} steady_registry={} steady_count={}",
                    registry(true),
                    samples.iter().filter(|s| s.1 == 0).count(),
                    registry(false),
                    samples.iter().filter(|s| s.1 != 0).count()
                );
                let mut first: Vec<f64> =
                    samples.iter().filter(|s| s.1 == 0).map(|s| s.0).collect();
                let mut steady: Vec<f64> =
                    samples.iter().filter(|s| s.1 != 0).map(|s| s.0).collect();
                first.sort_by(f64::total_cmp);
                steady.sort_by(f64::total_cmp);
                let rank =
                    |values: &[f64], q: usize, d: usize| values[(values.len() * q).div_ceil(d) - 1];
                println!(
                    "PHASE_SPLIT phase={label} first_burst_p99_ms={:.3} first_burst_max_ms={:.3} steady_p99_ms={:.3} steady_p9995_ms={:.3} steady_count={}",
                    rank(&first, 99, 100),
                    first[first.len() - 1],
                    rank(&steady, 99, 100),
                    rank(&steady, 9995, 10000),
                    steady.len()
                );
            }
            samples.sort_by(|a, b| a.0.total_cmp(&b.0));
            let count = samples.len();
            if stat {
                for (_, iteration, body, id, setup_age_ms) in &samples {
                    if *iteration == 0 && body.ingress_and_peer_ns.is_some() {
                        println!(
                            "FIRST_STAT_ROUTE client={id} setup_age_ms={setup_age_ms} registry={} gate={}",
                            body.forwarding_stages_ns[2].is_some(),
                            body.forwarding_stages_ns[1].is_some()
                        );
                    }
                }
            }
            for (elapsed, iteration, body, id, setup_age_ms) in
                samples.iter().rev().take(count / 100 + 1)
            {
                let first = body.first_chunk.unwrap();
                let last = body.last_chunk.unwrap();
                println!(
                    "SESSION_BODY stat={stat} header_reads={header_reads} iteration={iteration} client={id} setup_age_ms={setup_age_ms} total_ms={elapsed} first_ns={} remaining_payload_ns={} completion_ns={} inter_chunk_pending_ns={} inter_chunk_notified_ns={} headers_ns={} headers_pending_ns={} headers_notified_ns={} ingress_peer_ns={} forwarding_stages_ns={} owner_stages_ns={} walls_ns={}",
                    first.as_nanos(),
                    (last - first).as_nanos(),
                    (body.end - last).as_nanos(),
                    body.inter_chunk_pending_ns,
                    body.inter_chunk_notified_ns,
                    body.headers.as_nanos(),
                    body.headers_pending_ns,
                    body.headers_notified_ns,
                    body.ingress_and_peer_ns
                        .map(|(i, p)| format!("{i}/{p}"))
                        .as_deref()
                        .unwrap_or("missing"),
                    serde_json::to_string(&body.forwarding_stages_ns).unwrap(),
                    serde_json::to_string(&body.owner_stages_ns).unwrap(),
                    serde_json::to_string(&body.walls_ns).unwrap()
                );
            }
            println!(
                "CLI_SESSION_LOAD servers={servers} clients=100 stat={stat} header_reads={header_reads} redis_commands_per_sec={commands_per_sec:.1} count={count} p99_ms={:.3} p9995_ms={:.3} writes_per_sec={writes_per_sec:.1}",
                samples[(count * 99).div_ceil(100) - 1].0,
                samples[(count * 9995).div_ceil(10000) - 1].0
            );
        }
        for (fs, _, _, data, _) in clients.iter().filter(|_| writes) {
            let verify = fs.open("/writing", "r", 0o600).await.unwrap();
            let mut contents = vec![0; data.len()];
            assert_eq!(
                verify.read(&mut contents, Some(0)).await.unwrap(),
                data.len()
            );
            assert_eq!(&contents, data);
            verify.close().await.unwrap();
        }
        println!(
            "CLI_SESSION_WRITE_READBACK volumes={} verified=true",
            clients.len()
        );
        stop.send_replace(true);
        while let Some(worker) = keepalives.join_next().await {
            worker.unwrap().unwrap();
        }
        if idle_seconds != 0 {
            // Clients stop issuing requests without closing their sessions, as
            // a disconnected mount appears to the ingress until session expiry.
            redis_phase(&mut metrics, "readback", &mut since).await;
            idle_windows(&mut metrics, "disconnected", 40, &mut since).await;
        }
        for (fs, read, write, _, _) in clients {
            read.close().await.unwrap();
            write.close().await.unwrap();
            fs.shutdown().await.unwrap();
        }
        for fs in direct_writers {
            fs.shutdown().await.unwrap();
        }
    }
    drop(owner_address);
    let (owner_process, owner_url, _owner_lines) = launch("owner.json").await;
    let _ = DIRECT_WRITE_BASE.set(owner_url.clone());
    let single_server_load =
        load && std::env::var_os("MOUNT_RS_TEST_CLI_SINGLE_SERVER_LOAD").is_some();
    if single_server_load {
        run_cli_load(&[&owner_url], 1, http2).await;
    }
    let (child, base, _lines) = launch("ingress.json").await;
    let client = public_client(http2);
    let missing = client
        .get(format!("{base}/v1/drives/missing/fs/check"))
        .bearer_auth("a".repeat(32))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 503);
    assert_eq!(missing.headers()["x-mount-dispatch"], "not-dispatched");
    let url = format!("{base}/v1/drives/volume/fs/cli-roundtrip");
    let written = client
        .put(&url)
        .bearer_auth("a".repeat(32))
        .body("via-cli")
        .send()
        .await
        .unwrap();
    assert!(written.status().is_success(), "{}", written.status());
    let read = client
        .get(&url)
        .bearer_auth("a".repeat(32))
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), 200);
    assert_eq!(read.bytes().await.unwrap().as_ref(), b"via-cli");
    let created = client
        .post(format!("{base}/v1/drives/volume/sessions"))
        .bearer_auth("a".repeat(32))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let session: serde_json::Value = serde_json::from_str(&created.text().await.unwrap()).unwrap();
    let closed = client
        .delete(format!("{owner_url}/v1/drives/volume/sessions"))
        .bearer_auth("a".repeat(32))
        .header(
            mount_rs_cluster::SESSION_HEADER,
            session["session"].as_str().unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert!(closed.status().is_success(), "{}", closed.status());
    async fn stop(mut child: tokio::process::Child) {
        assert!(
            tokio::process::Command::new("kill")
                .args(["-INT", &child.id().unwrap().to_string()])
                .status()
                .await
                .unwrap()
                .success()
        );
        assert!(child.wait().await.unwrap().success());
    }
    if load && !single_server_load {
        let (other, other_url, _lines) = launch("ingress-2.json").await;
        run_cli_load(&[&base, &other_url], 3, http2).await;
        stop(other).await;
    }
    if std::env::var_os("MOUNT_RS_TEST_CLI_CACHE_REPORT").is_some() {
        // Let every volume report its cumulative counters after write readback.
        tokio::time::sleep(Duration::from_secs(12)).await;
    }
    stop(owner_process).await;
    if test_tidb {
        std::fs::create_dir(dir.path().join("empty-blocks")).unwrap();
        let wrong_blocks = &mut owner_config["http"]["drives"][0]["driver"]["storage"]["blocks"];
        if rustfs {
            wrong_blocks["prefix"] =
                format!("{}/empty", wrong_blocks["prefix"].as_str().unwrap()).into();
        } else if !memory_blocks {
            wrong_blocks["path"] = "empty-blocks".into();
        }
        write("wrong-owner.json", owner_config.to_string());
        let wrong = tokio::process::Command::new(&binary)
            .env("CLI_TEST_TOKEN", "a".repeat(32))
            .args(["serve-http", "--config"])
            .arg(dir.path().join("wrong-owner.json"))
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert!(!wrong.status.success());
        assert!(!String::from_utf8_lossy(&wrong.stdout).contains("http listening at"));
        assert!(
            String::from_utf8_lossy(&wrong.stderr)
                .contains("authoritative store lacks a committed file extent")
        );
        eprintln!(
            "CLI_WRONG_BACKING_REJECTED {}",
            String::from_utf8_lossy(&wrong.stderr).trim()
        );
        let redis = redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
        let routes = mount_rs_cluster::RedisRouteRegistry::new(
            redis.get_multiplexed_async_connection().await.unwrap(),
            namespace.clone(),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(routes.lookup("volume").await.unwrap().is_none());
        if memory_blocks {
            let response = client
                .get(&url)
                .bearer_auth("a".repeat(32))
                .send()
                .await
                .unwrap();
            assert!(response.status().is_server_error());
            println!(
                "CLI_TIDB_ROUNDTRIP normalized=true blocks=memory live_read=pass empty_restart_rejected=pass"
            );
            drop(client);
            stop(child).await;
            return;
        }
        let (new_owner, _, _lines) = launch("owner.json").await;
        // Only reads are retried while ingress rediscovers the successor fence.
        let mut verified = false;
        for _ in 0..30 {
            let read = client
                .get(&url)
                .bearer_auth("a".repeat(32))
                .send()
                .await
                .unwrap();
            if read.status().is_success() {
                assert_eq!(read.bytes().await.unwrap().as_ref(), b"via-cli");
                verified = true;
                break;
            }
            assert!(read.status().is_server_error() || read.status() == 409);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(verified, "forwarded read failed after owner restart");
        stop(new_owner).await;
        println!(
            "CLI_TIDB_ROUNDTRIP normalized=true blocks={} restart_read=pass",
            if rustfs { "rustfs" } else { "local" }
        );
    }
    drop(client);
    stop(child).await;
}

async fn exercise_forwarder(
    client: Arc<PeerClient>,
    filesystem: Arc<mount_rs_sdk::Filesystem>,
    route: &mount_rs_cluster::VolumeRoute,
    pin: SpkiSha256,
    data: &[u8],
    completion_gate: Arc<CompletionGate>,
    certificates: (&Identity, &Identity, &Identity),
) {
    use http_body_util::BodyExt;
    use mount_rs_cluster::*;
    let redis = redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
    let connection = redis.get_multiplexed_async_connection().await.unwrap();
    let namespace = format!(
        "forward-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let registry =
        RedisRouteRegistry::new(connection, namespace.clone(), Duration::from_secs(2)).unwrap();
    let registration = OwnedVolumeRegistration::new(
        filesystem.clone(),
        registry.clone(),
        route.clone(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    #[cfg(unix)]
    if let Some(binary) = std::env::var_os("MOUNT_RS_TEST_INGRESS_BINARY") {
        exercise_cli_ingress(binary, &namespace, route, pin, certificates).await;
    }
    registration.advertise().await.unwrap();
    let peers = Arc::new(
        PeerDirectory::new(vec![TrustedPeer {
            node: route.node.clone(),
            boot: route.boot.clone(),
            address: route.address,
            tls_server_name: route.tls_server_name.clone(),
            pin,
        }])
        .unwrap(),
    );
    let options = ForwardOptions::default();
    let forwarder =
        IngressForwarder::new(registry.clone(), peers.clone(), client.clone(), options).unwrap();
    let response = forwarder
        .forward(public_request("PUT", "secret-token", data))
        .await
        .unwrap();
    assert!(response.status().is_success());
    response.into_body().collect().await.unwrap();
    assert_eq!(forwarder.diagnostics().mutations.route_cache_misses, 1);
    assert_eq!(forwarder.diagnostics().mutations.route_cache_hits, 0);
    // Two real public listeners discover the same owner internally. The HTTP
    // client knows neither Redis nor the private peer destination.
    let mut listeners = Vec::new();
    let mut session_forwarders = Vec::new();
    for _ in 0..2 {
        let forwarder = IngressForwarder::new(
            registry.clone(),
            peers.clone(),
            client.clone(),
            ForwardOptions::default(),
        )
        .unwrap();
        session_forwarders.push(forwarder.clone());
        listeners.push(
            mount_rs_http::HttpServer::start_with_handler(
                Arc::new(forwarder),
                mount_rs_http::HttpServerOptions::default(),
            )
            .await
            .unwrap(),
        );
    }
    let public_client = reqwest::Client::new();
    let first_url = format!("{}/v1/drives/volume/sessions", listeners[0].url());
    let second_url = format!("{}/v1/drives/volume/sessions", listeners[1].url());
    let created = public_client
        .post(&first_url)
        .bearer_auth("a".repeat(32))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    assert_eq!(created.headers()["cache-control"], "no-store");
    let payload: serde_json::Value = serde_json::from_str(&created.text().await.unwrap()).unwrap();
    let session = payload["session"].as_str().unwrap();
    let first_handles = format!("{first_url}/handles");
    let second_handles = format!("{second_url}/handles");
    let opened = public_client.post(&first_handles).bearer_auth("a".repeat(32))
        .header(SESSION_HEADER, session)
        .body(serde_json::json!({"operation":"open","path":"/session-file","mode":384,
            "flags":{"read":true,"write":true,"create":true,"truncate":true,"append":false,"exclusive":false}}).to_string())
        .send().await.unwrap();
    assert_eq!(opened.status(), 200);
    let opened: serde_json::Value = serde_json::from_str(&opened.text().await.unwrap()).unwrap();
    let handle = opened["handle"].as_u64().unwrap();
    let written = public_client
        .put(&second_handles)
        .bearer_auth("a".repeat(32))
        .header(SESSION_HEADER, session)
        .header("x-mount-handle", handle)
        .header("x-mount-position", 0)
        .body("original")
        .send()
        .await
        .unwrap();
    assert_eq!(written.status(), 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&written.text().await.unwrap()).unwrap()["written"],
        8
    );
    filesystem
        .driver()
        .rename("/session-file", "/session-renamed")
        .await
        .unwrap();
    filesystem
        .driver()
        .unlink("/session-renamed")
        .await
        .unwrap();
    let replacement = filesystem
        .driver()
        .open("/session-file", "w", 0o600)
        .await
        .unwrap();
    replacement.write(b"replacement", Some(0)).await.unwrap();
    replacement.close().await.unwrap();
    for url in [&first_handles, &second_handles] {
        let read = public_client
            .post(url)
            .bearer_auth("a".repeat(32))
            .header(SESSION_HEADER, session)
            .body(
                serde_json::json!({"operation":"read","handle":handle,"length":8,"position":0})
                    .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(read.status(), 200);
        assert_eq!(read.bytes().await.unwrap().as_ref(), b"original");
    }
    let denied_read = public_client
        .post(&second_handles)
        .bearer_auth("b".repeat(32))
        .header(SESSION_HEADER, session)
        .body(
            serde_json::json!({"operation":"read","handle":handle,"length":8,"position":0})
                .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(denied_read.status(), 403);
    denied_read.bytes().await.unwrap();
    let stat = public_client
        .post(&second_handles)
        .bearer_auth("a".repeat(32))
        .header(SESSION_HEADER, session)
        .body(serde_json::json!({"operation":"stat","handle":handle}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(stat.status(), 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stat.text().await.unwrap()).unwrap()["size"],
        8
    );

    let denied = public_client
        .delete(&second_url)
        .bearer_auth("b".repeat(32))
        .header(SESSION_HEADER, session)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    denied.bytes().await.unwrap();
    let closed = public_client
        .delete(&second_url)
        .bearer_auth("a".repeat(32))
        .header(SESSION_HEADER, session)
        .send()
        .await
        .unwrap();
    assert_eq!(closed.status(), 204);
    closed.bytes().await.unwrap();
    let stale = public_client
        .delete(&first_url)
        .bearer_auth("a".repeat(32))
        .header(SESSION_HEADER, session)
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 403);
    stale.bytes().await.unwrap();
    {
        use mount_rs_core::FsDriver;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut writers = Vec::new();
        for (index, listener) in listeners.iter().enumerate() {
            let credential = if index == 0 { "a" } else { "b" }.repeat(32);
            let remote = mount_rs_remote::RemoteFs::connect(
                &listener.url(),
                "volume",
                &credential,
                Duration::from_secs(5),
            )
            .await
            .unwrap();
            let barrier = barrier.clone();
            writers.push(tokio::spawn(async move {
                let file = remote.open("/session-append", "a+", 0o600).await.unwrap();
                barrier.wait().await;
                for sequence in 0u8..32 {
                    let record = [index as u8, sequence, 0x5a, 0xa5];
                    assert_eq!(file.write(&record, None).await.unwrap(), 4);
                }
                file.sync().await.unwrap();
                file.close().await.unwrap();
                remote.shutdown().await.unwrap();
            }));
        }
        for writer in writers {
            writer.await.unwrap();
        }
        let reader = mount_rs_remote::RemoteFs::connect(
            &listeners[1].url(),
            "volume",
            &"a".repeat(32),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let file = reader.open("/session-append", "r", 0).await.unwrap();
        assert_eq!(file.stat().await.unwrap().size, 256);
        let mut bytes = [0; 256];
        assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), bytes.len());
        let mut seen = std::collections::BTreeSet::new();
        for record in bytes.chunks_exact(4) {
            assert!(record[0] < 2 && record[1] < 32);
            assert_eq!(&record[2..], &[0x5a, 0xa5]);
            assert!(seen.insert((record[0], record[1])));
        }
        assert_eq!(seen.len(), 64);
        file.close().await.unwrap();
        reader.unlink("/session-append").await.unwrap();
        reader.shutdown().await.unwrap();
        println!(
            "SESSION_MULTIWRITER_EVIDENCE ingresses=2 writers=2 records=64 intact=pass duplicates=0 missing=0 transport=mtls-http3 discovery=redis"
        );
    }
    for listener in listeners {
        listener.close().await.unwrap();
    }
    for forwarder in session_forwarders {
        let diagnostics = forwarder.diagnostics();
        assert!(diagnostics.session_rpc.route.completed > 0);
        assert!(diagnostics.session_rpc.headers.completed > 0);
        assert!(diagnostics.session_rpc.body.completed > 0);
        assert_eq!(diagnostics.reads.route.completed, 0);
        assert_eq!(diagnostics.mutations.route.completed, 0);
    }
    println!(
        "SESSION_FORWARDING_EVIDENCE public_ingresses=2 transport=mtls-http3 discovery=redis create=pass cross_client_denial=pass cross_ingress_close=pass handle_io=pass retained_inode=pass"
    );
    let fault_forwarder = IngressForwarder::new(
        registry.clone(),
        peers.clone(),
        client.clone(),
        ForwardOptions {
            renew_route_on_success: true,
            route_cache_ttl: Duration::from_secs(5),
            ..ForwardOptions::default()
        },
    )
    .unwrap();
    // A known byte length must not let the public writer skip peer EOF.
    for (http2, empty, reset) in [false, true].into_iter().flat_map(|http2| {
        [false, true]
            .into_iter()
            .flat_map(move |empty| [false, true].map(move |reset| (http2, empty, reset)))
    }) {
        let public = mount_rs_http::HttpServer::start_with_handler(
            Arc::new(
                IngressForwarder::new(
                    registry.clone(),
                    peers.clone(),
                    client.clone(),
                    ForwardOptions::default(),
                )
                .unwrap(),
            ),
            mount_rs_http::HttpServerOptions {
                http2_prior_knowledge: http2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let builder = reqwest::Client::builder();
        let http = if http2 {
            builder.http2_prior_knowledge()
        } else {
            builder.http1_only()
        }
        .build()
        .unwrap();
        let url = format!(
            "{}/v1/drives/volume/fs/completion-test?empty={}&reset={}",
            public.url(),
            u8::from(empty),
            u8::from(reset)
        );
        let mut pending = tokio::spawn(async move {
            http.get(url)
                .bearer_auth("secret-token")
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await
        });
        tokio::time::timeout(
            Duration::from_secs(5),
            completion_gate.payload_sent.notified(),
        )
        .await
        .unwrap();
        let early = tokio::time::timeout(Duration::from_millis(50), &mut pending).await;
        let finished_early = early.is_ok();
        completion_gate.release.notify_one();
        let result = match early {
            Ok(result) => result,
            Err(_) => pending.await,
        }
        .unwrap();
        if reset {
            assert!(result.is_err(), "peer reset must not become public success");
        } else {
            assert_eq!(
                result.unwrap().as_ref(),
                if empty {
                    b"".as_slice()
                } else {
                    b"done".as_slice()
                }
            );
        }
        public.close().await.unwrap();
        assert!(
            !finished_early,
            "public HTTP/{version} completed before peer EOF",
            version = if http2 { "2" } else { "1" }
        );
    }
    let mut malformed = public_request("GET", "secret-token", &[]);
    *malformed.uri_mut() = "/v1/drives/volume/fs/truncated-test".parse().unwrap();
    let failed_trace = mount_rs_cluster::ForwardRequestTrace::default();
    malformed.extensions_mut().insert(failed_trace.clone());
    let response = fault_forwarder.forward(malformed).await.unwrap();
    assert!(response.into_body().collect().await.is_err());
    assert!(!failed_trace.snapshot().complete);
    assert!(failed_trace.snapshot().headers_ns.is_some());
    assert!(failed_trace.snapshot().route_lookup_wait_ns.is_some());
    let fault = fault_forwarder.diagnostics();
    // Either HTTP/3 itself rejects the mismatch or our EOF check does. In
    // both cases the public body must fail, never succeed with short bytes.
    assert_eq!(
        fault.response_receive_errors + fault.response_length_errors,
        1
    );
    assert_eq!(fault.upload_completion_errors, 0);
    let trace = mount_rs_cluster::ForwardRequestTrace::default();
    let mut recovery = public_request("GET", "secret-token", &[]);
    recovery.extensions_mut().insert(trace.clone());
    let recovered = fault_forwarder.forward(recovery).await.unwrap();
    for internal in [
        "x-mount-owner-stream-queued-ns",
        "x-mount-owner-stream-resolve-ns",
    ] {
        assert!(!recovered.headers().contains_key(internal));
    }
    assert_eq!(
        recovered
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        data
    );
    let timing = trace.snapshot();
    assert!(timing.complete);
    let failed = failed_trace.snapshot();
    let key = |t: &mount_rs_cluster::ForwardRequestTiming| {
        (t.peer_connection_id.unwrap(), t.peer_stream_id.unwrap())
    };
    assert_ne!(key(&failed), key(&timing));
    assert!(timing.first_payload_received_ns.unwrap() <= timing.body_ns.unwrap());
    assert!(timing.first_payload_received_ns.unwrap() <= timing.first_payload_yielded_ns.unwrap());
    let pending = timing.header_pending_ns.unwrap();
    assert!(timing.header_notified_ns.unwrap() <= pending);
    assert!(pending <= timing.headers_ns.unwrap());
    assert!(
        timing.route_lookup_wait_ns.is_some(),
        "failed peer body must invalidate the renewed route"
    );
    let volume = timing.volume_admission_ns.unwrap();
    assert!(timing.volume_lock_pending_ns.unwrap() <= volume);
    assert!(
        timing.lookup_map_admission_ns.unwrap() + volume <= timing.route_lookup_wait_ns.unwrap()
    );
    assert_eq!(fault_forwarder.diagnostics().reads.route_cache_misses, 2);
    let total = timing.total_ns.unwrap();
    for stage in [
        timing.route_ns,
        timing.connection_ns,
        timing.stream_open_ns,
        timing.headers_ns,
        timing.body_ns,
    ] {
        assert!(stage.unwrap() <= total);
    }
    // Opt-in successful responses can keep an exact advisory route warm.
    // Authentication failures must not renew its expiry.
    let renewing = IngressForwarder::new(
        registry.clone(),
        peers.clone(),
        client.clone(),
        ForwardOptions {
            route_cache_ttl: Duration::from_millis(500),
            renew_route_on_success: true,
            ..ForwardOptions::default()
        },
    )
    .unwrap();
    for iteration in 0..3 {
        if iteration != 0 {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let response = renewing
            .forward(public_request("GET", "secret-token", &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            data
        );
    }
    assert_eq!(renewing.diagnostics().reads.route_cache_misses, 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let denied = renewing
        .forward(public_request("GET", "invalid-token", &[]))
        .await
        .unwrap();
    assert!(!denied.status().is_success());
    denied.into_body().collect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    renewing
        .forward(public_request("GET", "secret-token", &[]))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();
    assert_eq!(renewing.diagnostics().reads.route_cache_misses, 2);

    // A fresh forwarder shares one discovery lookup across concurrent reads.
    let fresh = IngressForwarder::new(
        registry.clone(),
        peers.clone(),
        client.clone(),
        ForwardOptions::default(),
    )
    .unwrap();
    let mut requests = tokio::task::JoinSet::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(20));
    for _ in 0..20 {
        let forwarder = fresh.clone();
        let barrier = barrier.clone();
        requests.spawn(async move {
            barrier.wait().await;
            let response = forwarder
                .forward(public_request("GET", "secret-token", &[]))
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.into_body().collect().await.unwrap().to_bytes()
        });
    }
    while let Some(result) = requests.join_next().await {
        assert_eq!(result.unwrap().as_ref(), data);
    }
    let discovery = fresh.diagnostics().reads;
    assert_eq!(discovery.route_cache_misses, 1);
    assert_eq!(discovery.route_cache_hits, 19);
    assert!(discovery.route_cache_coalesced_hits <= discovery.route_cache_hits);
    assert_eq!(
        discovery.route_lookup_wait.completed,
        discovery.route_cache_misses + discovery.route_cache_coalesced_hits
    );
    assert_eq!(
        discovery.route_registry_admission.completed,
        discovery.route_lookup_wait.completed
    );
    assert_eq!(
        discovery.route_volume_wait.completed,
        discovery.route_lookup_wait.completed
    );
    assert!(
        discovery.route_registry_admission.nanoseconds + discovery.route_volume_wait.nanoseconds
            <= discovery.route_lookup_wait.nanoseconds
    );
    // Pause only this test's disposable Redis, with automatic expiry. A
    // cancelled lookup leader must not strand the next caller's keyed guard.
    let cancellable = IngressForwarder::new(
        registry.clone(),
        peers.clone(),
        client.clone(),
        ForwardOptions::default(),
    )
    .unwrap();
    let mut control = redis.get_multiplexed_async_connection().await.unwrap();
    redis::cmd("CLIENT")
        .arg("PAUSE")
        .arg(1000)
        .arg("ALL")
        .query_async::<()>(&mut control)
        .await
        .unwrap();
    let worker = cancellable.clone();
    let leader = tokio::spawn(async move {
        worker
            .forward(public_request("GET", "secret-token", &[]))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while cancellable.diagnostics().reads.route_cache_misses == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let worker = cancellable.clone();
    let follower = tokio::spawn(async move {
        worker
            .forward(public_request("GET", "secret-token", &[]))
            .await
    });
    tokio::task::yield_now().await;
    leader.abort();
    assert!(leader.await.unwrap_err().is_cancelled());
    let delayed = tokio::time::timeout(Duration::from_secs(3), follower)
        .await
        .unwrap()
        .unwrap();
    // The Redis client's default 500ms response timeout is shorter than
    // this injected pause. Failure must be bounded and must not poison the
    // per-volume guard or negatively cache the route.
    assert!(matches!(delayed, Err(ForwardError::RouteUnavailable)));
    assert_eq!(cancellable.diagnostics().reads.route_cache_misses, 2);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let response = cancellable
        .forward(public_request("GET", "secret-token", &[]))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        data
    );
    assert_eq!(cancellable.diagnostics().reads.route_cache_misses, 3);
    let mut samples = Vec::new();
    // The owner may finish a GET while its empty request body is still being
    // polled by ingress. A delayed EOF must not truncate a successful response.
    let mut delayed = public_request("GET", "secret-token", &[]);
    *delayed.body_mut() = http_body_util::StreamBody::new(futures_util::stream::once(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok::<_, mount_rs_http::BoxError>(hyper::body::Frame::data(Bytes::new()))
    }))
    .boxed_unsync();
    let response = forwarder.forward(delayed).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .expect("delayed GET upload completion must preserve response")
            .to_bytes()
            .as_ref(),
        data
    );
    let before = forwarder.diagnostics();
    let mut failed_upload = public_request("GET", "secret-token", &[]);
    *failed_upload.body_mut() =
        http_body_util::StreamBody::new(futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Err::<hyper::body::Frame<Bytes>, mount_rs_http::BoxError>(
                std::io::Error::other("test upload failure").into(),
            )
        }))
        .boxed_unsync();
    let response = forwarder.forward(failed_upload).await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.into_body().collect().await.is_err());
    assert_eq!(
        forwarder.clone().diagnostics().upload_completion_errors,
        before.upload_completion_errors + 1
    );
    assert_eq!(
        forwarder.diagnostics().response_receive_errors,
        before.response_receive_errors
    );
    let decisions = forwarder.diagnostics().reads;
    assert!(decisions.route_cache_hits > 0);
    assert_eq!(
        decisions.route_cache_hits + decisions.route_cache_misses,
        decisions.route.completed
    );
    let measured_before = forwarder.diagnostics();
    for _ in 0..20 {
        let start = std::time::Instant::now();
        let response = forwarder
            .forward(public_request("GET", "secret-token", &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            data
        );
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let measured_after = forwarder.clone().diagnostics();
    for (before, after) in [
        (measured_before.reads.route, measured_after.reads.route),
        (measured_before.reads.open, measured_after.reads.open),
        (
            measured_before.reads.connection,
            measured_after.reads.connection,
        ),
        (
            measured_before.reads.stream_open,
            measured_after.reads.stream_open,
        ),
        (measured_before.reads.headers, measured_after.reads.headers),
        (
            measured_before.reads.first_payload,
            measured_after.reads.first_payload,
        ),
        (measured_before.reads.body, measured_after.reads.body),
    ] {
        assert_eq!(
            after.completed - before.completed,
            20,
            "Content-Length followed by EOF must count each stage exactly once"
        );
        assert!(after.nanoseconds >= before.nanoseconds);
    }
    assert_eq!(
        measured_after.mutations.route.completed,
        measured_before.mutations.route.completed
    );
    println!(
        "REDIS_HTTP3_FORWARDED_READ_SAMPLES_MS {}",
        serde_json::to_string(&samples).unwrap()
    );
    let mut request = public_request("GET", "secret-token", &[]);
    request
        .headers_mut()
        .insert("range", "bytes=4096-8191".parse().unwrap());
    let response = forwarder.forward(request).await.unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        &data[4096..8192]
    );
    let response = forwarder
        .forward(public_request("GET", "wrong-token", &[]))
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    response.into_body().collect().await.unwrap();
    let mut spoof = public_request("DELETE", "secret-token", &[]);
    spoof.headers_mut().insert(HOP_HEADER, "1".parse().unwrap());
    assert!(matches!(
        forwarder.forward(spoof).await,
        Err(ForwardError::InvalidRequest)
    ));

    // A public HTTP client has no Redis or owner-discovery access.
    let ingress = mount_rs_http::HttpServer::start_with_handler(
        Arc::new(forwarder.clone()),
        mount_rs_http::HttpServerOptions::default(),
    )
    .await
    .unwrap();
    let public = reqwest::Client::new();
    let url = format!("{}/v1/drives/volume/fs/data", ingress.url());
    let before_fault = forwarder.diagnostics();
    let malformed_response = public
        .get(format!(
            "{}/v1/drives/volume/fs/truncated-test",
            ingress.url()
        ))
        .bearer_auth("secret-token")
        .send()
        .await;
    // Hyper may discover the invalid body before sending headers, or after
    // the client receives them. Neither path may return a successful body.
    if let Ok(response) = malformed_response {
        assert_eq!(response.status(), 200);
        assert!(response.bytes().await.is_err());
    }
    let after_fault = forwarder.diagnostics();
    assert_eq!(
        after_fault.response_receive_errors + after_fault.response_length_errors,
        before_fault.response_receive_errors + before_fault.response_length_errors + 1,
    );
    let recovered = public
        .get(&url)
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(recovered.status(), 200);
    assert_eq!(recovered.bytes().await.unwrap().as_ref(), data);
    let write = public
        .put(&url)
        .bearer_auth("secret-token")
        .body(data.to_vec())
        .send()
        .await
        .unwrap();
    assert!(write.status().is_success());
    write.bytes().await.unwrap();
    filesystem
        .driver()
        .symlink("/data", "/metadata-link")
        .await
        .unwrap();
    for operation in ["stat", "lstat", "statfs", "readlink"] {
        let metadata_url = format!(
            "{}/v1/drives/volume/{operation}/metadata-link",
            ingress.url()
        );
        let denied = public
            .get(&metadata_url)
            .bearer_auth("wrong-token")
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 401);
        denied.bytes().await.unwrap();
        let response = public
            .get(&metadata_url)
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let value: serde_json::Value =
            serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        match operation {
            "stat" => {
                assert_eq!(value["size"], data.len());
                assert_eq!(value["mode"].as_u64().unwrap() & 0o170000, 0o100000);
            }
            "lstat" => assert_eq!(value["mode"].as_u64().unwrap() & 0o170000, 0o120000),
            "readlink" => assert_eq!(value, "/data"),
            "statfs" => assert!(value["block_size"].as_u64().unwrap() > 0),
            _ => unreachable!(),
        }
        let head = public
            .head(&metadata_url)
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(head.status(), 200);
        assert!(head.bytes().await.unwrap().is_empty());
    }
    let missing = public
        .get(format!("{}/v1/drives/volume/stat/missing", ingress.url()))
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let error: serde_json::Value = serde_json::from_slice(&missing.bytes().await.unwrap()).unwrap();
    assert_eq!(error["error"]["code"], "ENOENT");
    let mut public_samples = Vec::new();
    for _ in 0..20 {
        let started = std::time::Instant::now();
        let response = public
            .get(&url)
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.bytes().await.unwrap().as_ref(), data);
        public_samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "PUBLIC_INGRESS_HTTP3_READ_SAMPLES_MS {}",
        serde_json::to_string(&public_samples).unwrap()
    );
    let response = public
        .get(&url)
        .bearer_auth("secret-token")
        .header("range", "bytes=4096-8191")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(response.bytes().await.unwrap().as_ref(), &data[4096..8192]);
    let denied = public
        .get(&url)
        .bearer_auth("wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);
    denied.bytes().await.unwrap();
    let spoofed = public
        .delete(&url)
        .bearer_auth("secret-token")
        .header(HOP_HEADER, "1")
        .send()
        .await
        .unwrap();
    assert_eq!(spoofed.status(), 400);
    assert_eq!(
        spoofed.headers().get("x-mount-dispatch").unwrap(),
        "not-dispatched"
    );
    spoofed.bytes().await.unwrap();
    let second_forwarder =
        IngressForwarder::new(registry.clone(), peers.clone(), client.clone(), options).unwrap();
    let second_ingress = mount_rs_http::HttpServer::start_with_handler(
        Arc::new(second_forwarder),
        mount_rs_http::HttpServerOptions::default(),
    )
    .await
    .unwrap();
    let second_url = format!("{}/v1/drives/volume/fs/data", second_ingress.url());
    let updated = b"written through another ingress";
    let response = public
        .put(&second_url)
        .bearer_auth("secret-token")
        .body(updated.to_vec())
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    response.bytes().await.unwrap();
    let response = public
        .get(&url)
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(response.bytes().await.unwrap().as_ref(), updated);
    let response = public
        .put(&url)
        .bearer_auth("secret-token")
        .body(data.to_vec())
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    response.bytes().await.unwrap();
    ingress.close().await.unwrap();
    let response = public
        .get(&second_url)
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(response.bytes().await.unwrap().as_ref(), data);
    second_ingress.close().await.unwrap();
    assert!(
        public
            .get(&url)
            .bearer_auth("secret-token")
            .send()
            .await
            .is_err()
    );
    println!("PUBLIC_INGRESS_HTTP3_PASS");

    let mut stale = route.clone();
    stale.fence += 1;
    registry
        .advertise(&stale, Duration::from_secs(10))
        .await
        .unwrap();
    let fresh = IngressForwarder::new(
        registry.clone(),
        peers.clone(),
        client.clone(),
        ForwardOptions {
            renew_route_on_success: true,
            ..options
        },
    )
    .unwrap();
    let response = fresh
        .forward(public_request("PUT", "secret-token", &[]))
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    assert_eq!(
        response.headers().get("x-mount-dispatch").unwrap(),
        "not-dispatched"
    );
    response.into_body().collect().await.unwrap();
    assert_eq!(fresh.diagnostics().mutations.route_cache_misses, 1);
    registry.remove(&stale).await.unwrap();
    registration.advertise().await.unwrap();
    let response = fresh
        .forward(public_request("GET", "secret-token", &[]))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        data
    );

    // Fenced rejection must invalidate even with success renewal enabled;
    // the next read discovers the restored route rather than replaying a write.
    assert_eq!(fresh.diagnostics().reads.route_cache_misses, 1);

    stale.address.set_port(1);
    registry
        .advertise(&stale, Duration::from_secs(10))
        .await
        .unwrap();
    let untrusted = IngressForwarder::new(registry.clone(), peers, client, options).unwrap();
    assert!(matches!(
        untrusted
            .forward(public_request("GET", "secret-token", &[]))
            .await,
        Err(ForwardError::UntrustedRoute)
    ));
    registry.remove(&stale).await.unwrap();
    registration.advertise().await.unwrap();
    // Warm the ingress route immediately before stopping the authoritative
    // owner. An advisory route-cache hit must not bypass owner fencing.
    let warm = fresh
        .forward(public_request("GET", "secret-token", &[]))
        .await
        .unwrap();
    assert_eq!(warm.status(), 200);
    assert_eq!(
        warm.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        data
    );
    registry.remove(route).await.unwrap();
    filesystem.shutdown().await.unwrap();
    let stopped = fresh
        .forward(public_request("GET", "secret-token", &[]))
        .await
        .unwrap();
    assert_eq!(stopped.status(), 503);
    stopped.into_body().collect().await.unwrap();
    registration.withdraw().await.unwrap();
    println!("REDIS_HTTP3_FORWARDING_PASS");
}

fn public_request(method: &str, token: &str, data: &[u8]) -> Request<mount_rs_http::HttpBody> {
    use http_body_util::BodyExt;
    Request::builder()
        .method(method)
        .uri("/v1/drives/volume/fs/data")
        .header("authorization", format!("Bearer {token}"))
        .header("content-length", data.len())
        .body(
            http_body_util::Full::new(Bytes::copy_from_slice(data))
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap()
}

#[tokio::test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL; optionally MOUNT_RS_TIDB_URL"]
async fn renewal_enabled_route_handoff_rejects_old_owner_without_write_replay() {
    run_renewal_handoff(false).await;
}

#[tokio::test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn renewal_enabled_transport_loss_rediscovers_without_write_replay() {
    run_renewal_handoff(true).await;
}

async fn run_renewal_handoff(disconnect: bool) {
    use http_body_util::BodyExt;
    use mount_rs_cluster::*;
    use mount_rs_sdk::{Filesystem, SplitOptions, StoreConfig};
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        let namespace = format!(
            "handoff-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let metadata = match std::env::var("MOUNT_RS_TIDB_URL") {
            Ok(connection) => StoreConfig::Tidb {
                connection,
                volume_key: namespace.clone(),
                durable: false,
            },
            Err(_) => StoreConfig::Sqlite {
                path: root.path().join("metadata.sqlite"),
            },
        };
        let mut options = SplitOptions::memory("owner-a", 4096);
        options.metadata = metadata;
        options.blocks = StoreConfig::Local {
            path: root.path().join("blocks"),
        };
        std::fs::create_dir(root.path().join("blocks")).unwrap();
        let first = Arc::new(Filesystem::split(options.clone()).await.unwrap());
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca = Identity {
            cert: params.self_signed(&key).unwrap(),
            key,
        };
        let ingress = peer(&ca, "ingress");
        let ingress_pin = spki_sha256_from_der(ingress.cert.der()).unwrap();
        let owners = [peer(&ca, "owner-a"), peer(&ca, "owner-b")];
        let mut endpoints = Vec::new();
        let mut trusted = Vec::new();
        for (index, identity) in owners.iter().enumerate() {
            let endpoint = quinn::Endpoint::server(
                load_peer_server_config_from_pem(
                    identity.cert.pem().as_bytes(),
                    identity.key.serialize_pem().as_bytes(),
                    ca.cert.pem().as_bytes(),
                )
                .unwrap(),
                "127.0.0.1:0".parse().unwrap(),
            )
            .unwrap();
            trusted.push(TrustedPeer {
                node: format!("owner-{}", if index == 0 { "a" } else { "b" }),
                boot: format!("boot-{index}"),
                address: endpoint.local_addr().unwrap(),
                tls_server_name: "localhost".into(),
                pin: spki_sha256_from_der(identity.cert.der()).unwrap(),
            });
            endpoints.push(endpoint);
        }
        let first_route = VolumeRoute {
            volume: "volume".into(),
            node: trusted[0].node.clone(),
            boot: trusted[0].boot.clone(),
            registration: "first".into(),
            fence: first.checked_owner_fence().await.unwrap(),
            address: trusted[0].address,
            tls_server_name: "localhost".into(),
        };
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(
            load_peer_client_config_from_pem(
                ingress.cert.pem().as_bytes(),
                ingress.key.serialize_pem().as_bytes(),
                ca.cert.pem().as_bytes(),
            )
            .unwrap(),
        );
        let client = Arc::new(
            PeerClient::new(
                endpoint,
                ApprovedPeerPins::new(trusted.iter().map(|peer| peer.pin)).unwrap(),
                PeerTransportLimits::default(),
            )
            .unwrap(),
        );
        let redis = redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
        let registry = RedisRouteRegistry::new(
            redis.get_multiplexed_async_connection().await.unwrap(),
            namespace,
            Duration::from_secs(2),
        )
        .unwrap();
        let registration = OwnedVolumeRegistration::new(
            first.clone(),
            registry.clone(),
            first_route.clone(),
            Duration::from_secs(20),
        )
        .await
        .unwrap();
        registration.advertise().await.unwrap();
        let cancel = CancellationToken::new();
        let first_handler = OwnerHttpHandler::new(
            first.clone(),
            first_route.clone(),
            "secret-token".into(),
            mount_rs_http::HttpServerOptions::default(),
        )
        .await
        .unwrap();
        let first_endpoint = endpoints[0].clone();
        let first_server = PeerServer::new(
            endpoints.remove(0),
            ApprovedPeerPins::new([ingress_pin]).unwrap(),
            PeerTransportLimits::default(),
            move |identity: &TlsIdentity, _: &Request<()>| identity.spki_sha256() == ingress_pin,
            move |identity, request, stream| {
                let handler = first_handler.clone();
                async move { handler.handle(identity, request, stream).await }
            },
        )
        .unwrap();
        let first_task = tokio::spawn(first_server.serve(cancel.clone()));
        let forwarder = IngressForwarder::new(
            registry.clone(),
            Arc::new(PeerDirectory::new(trusted.clone()).unwrap()),
            client.clone(),
            ForwardOptions {
                route_cache_ttl: Duration::from_secs(5),
                operation_timeout: if disconnect {
                    Duration::from_millis(500)
                } else {
                    Duration::from_secs(30)
                },
                renew_route_on_success: true,
                ..Default::default()
            },
        )
        .unwrap();
        let initial = b"original owner data";
        let response = forwarder
            .forward(public_request("PUT", "secret-token", initial))
            .await
            .unwrap();
        assert!(response.status().is_success());
        response.into_body().collect().await.unwrap();
        let response = forwarder
            .forward(public_request("GET", "secret-token", &[]))
            .await
            .unwrap();
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            initial
        );
        let before_handoff = forwarder.diagnostics();
        first.shutdown().await.unwrap();
        options.owner = "owner-b".into();
        let second = Arc::new(Filesystem::split(options).await.unwrap());
        let second_route = VolumeRoute {
            volume: "volume".into(),
            node: trusted[1].node.clone(),
            boot: trusted[1].boot.clone(),
            registration: "second".into(),
            fence: second.checked_owner_fence().await.unwrap(),
            address: trusted[1].address,
            tls_server_name: "localhost".into(),
        };
        assert!(second_route.fence > first_route.fence);
        let second_handler = OwnerHttpHandler::new(
            second.clone(),
            second_route.clone(),
            "secret-token".into(),
            mount_rs_http::HttpServerOptions::default(),
        )
        .await
        .unwrap();
        let second_calls = Arc::new(AtomicUsize::new(0));
        let observed = second_calls.clone();
        let second_server = PeerServer::new(
            endpoints.remove(0),
            ApprovedPeerPins::new([ingress_pin]).unwrap(),
            PeerTransportLimits::default(),
            move |identity: &TlsIdentity, _: &Request<()>| identity.spki_sha256() == ingress_pin,
            move |identity, request, stream| {
                let handler = second_handler.clone();
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    handler.handle(identity, request, stream).await
                }
            },
        )
        .unwrap();
        let second_task = tokio::spawn(second_server.serve(cancel.clone()));
        let new_registration = OwnedVolumeRegistration::new(
            second.clone(),
            registry.clone(),
            second_route.clone(),
            Duration::from_secs(20),
        )
        .await
        .unwrap();
        new_registration.advertise().await.unwrap();
        // A late old registration cleanup must not remove the newer owner.
        registry.remove(&first_route).await.unwrap();
        assert_eq!(
            registry.lookup("volume").await.unwrap().unwrap(),
            second_route
        );
        if disconnect {
            first_endpoint.close(0u32.into(), b"test endpoint loss");
        }
        let outcome = forwarder
            .forward(public_request("PUT", "secret-token", b"must not replay"))
            .await;
        if disconnect {
            assert!(matches!(outcome, Err(ForwardError::OutcomeUnknown)));
        } else {
            let response = outcome.unwrap();
            assert_eq!(response.status(), 503);
            assert_eq!(response.headers()["x-mount-dispatch"], "not-dispatched");
            response.into_body().collect().await.unwrap();
        }
        assert_eq!(
            forwarder.diagnostics().peer_not_dispatched_503
                - before_handoff.peer_not_dispatched_503,
            u64::from(!disconnect)
        );
        assert_eq!(
            forwarder.diagnostics().mutations.route_cache_misses,
            before_handoff.mutations.route_cache_misses,
            "stale cached route must actually be exercised"
        );
        assert_eq!(
            second_calls.load(Ordering::SeqCst),
            0,
            "failed mutation replayed to new owner"
        );
        let response = forwarder
            .forward(public_request("GET", "secret-token", &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            initial
        );
        assert_eq!(forwarder.diagnostics().reads.route_cache_misses, 1);
        let updated = b"new owner data";
        let response = forwarder
            .forward(public_request("PUT", "secret-token", updated))
            .await
            .unwrap();
        assert!(response.status().is_success());
        response.into_body().collect().await.unwrap();
        let response = forwarder
            .forward(public_request("GET", "secret-token", &[]))
            .await
            .unwrap();
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            updated
        );
        registry.remove(&second_route).await.unwrap();
        second.shutdown().await.unwrap();
        client.shutdown().await.unwrap();
        cancel.cancel();
        first_task.await.unwrap().unwrap();
        second_task.await.unwrap().unwrap();
    })
    .await
    .expect("owner handoff timed out");
}
