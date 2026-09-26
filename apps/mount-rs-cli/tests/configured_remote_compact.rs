#![cfg(all(feature = "local-oidc-fixture", debug_assertions, unix))]

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use mount_rs_core::{
    ErrorCode, FsDriver,
    storage::{BlockStore, MetadataStore},
};
use mount_rs_remote_client::{
    connection::{ClientError, ConnectionTransport, RemoteConnection},
    credentials::CredentialSource,
    driver::RemoteFsDriver,
};
use mount_rs_remote_protocol::{Operation, OperationName};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use serde_json::json;
use std::{
    io::{BufRead, BufReader},
    net::SocketAddr,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const ISSUER: &str = "https://issuer.example.com";
const AUDIENCE: &str = "mount-rs";

struct Tokens {
    jwks: String,
    valid: String,
    invalid_signature: String,
    wrong_audience: String,
    expired: String,
}

fn tokens() -> Tokens {
    let rng = SystemRandom::new();
    let primary_pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let primary = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_FIXED_SIGNING,
        primary_pkcs8.as_ref(),
        &rng,
    )
    .unwrap();
    let other_pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let other =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, other_pkcs8.as_ref(), &rng)
            .unwrap();
    let point = primary.public_key().as_ref();
    let jwks = json!({"keys":[{
        "kty":"EC","kid":"fixture","crv":"P-256","alg":"ES256","use":"sig",
        "x":URL_SAFE_NO_PAD.encode(&point[1..33]),
        "y":URL_SAFE_NO_PAD.encode(&point[33..65])
    }]})
    .to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let sign = |key: &EcdsaKeyPair, issuer: &str, audience: &str, iat: u64, exp: u64| {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"fixture"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "iss":issuer,"aud":audience,"sub":"sandbox-1",
                "repository_id":"repo-1","iat":iat,"exp":exp
            }))
            .unwrap(),
        );
        let input = format!("{header}.{payload}");
        let signature = key.sign(&rng, input.as_bytes()).unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
    };
    Tokens {
        jwks,
        valid: sign(&primary, ISSUER, AUDIENCE, now, now + 600),
        invalid_signature: sign(&other, ISSUER, AUDIENCE, now, now + 600),
        wrong_audience: sign(&primary, ISSUER, "other-audience", now, now + 600),
        expired: sign(&primary, ISSUER, AUDIENCE, now - 600, now - 300),
    }
}

struct ServerProcess {
    child: Child,
    stdout: Arc<Mutex<Vec<String>>>,
    stderr: Arc<Mutex<Vec<String>>>,
    stdout_thread: Option<thread::JoinHandle<()>>,
    stderr_thread: Option<thread::JoinHandle<()>>,
}

impl ServerProcess {
    fn spawn(config: &Path, diagnostics: bool) -> (Self, SocketAddr, SocketAddr) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mount-rs"));
        if diagnostics {
            command
                .env("MOUNT_RS_PROFILE_IO", "1")
                .env("MOUNT_RS_TRACE_SERVICE", "1");
        } else {
            command
                .env_remove("MOUNT_RS_PROFILE_IO")
                .env_remove("MOUNT_RS_TRACE_SERVICE");
        }
        let mut child = command
            .args(["serve-remote", "--config"])
            .arg(config)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout_pipe = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let (lines_tx, lines_rx) = mpsc::channel();
        let stdout_lines = stdout.clone();
        let stdout_thread = thread::spawn(move || {
            for line in BufReader::new(stdout_pipe).lines() {
                let Ok(line) = line else { break };
                stdout_lines.lock().unwrap().push(line.clone());
                let _ = lines_tx.send(line);
            }
        });
        let stderr_lines = stderr.clone();
        let stderr_thread = thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines() {
                let Ok(line) = line else { break };
                stderr_lines.lock().unwrap().push(line);
            }
        });
        let mut process = Self {
            child,
            stdout,
            stderr,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut quic = None;
        let mut websocket = None;
        while Instant::now() < deadline && (quic.is_none() || websocket.is_none()) {
            if let Some(status) = process.child.try_wait().unwrap() {
                panic!(
                    "configured server exited before readiness: {status}; stdout={:?}; stderr={:?}",
                    process.stdout.lock().unwrap(),
                    process.stderr.lock().unwrap()
                );
            }
            if let Ok(line) = lines_rx.recv_timeout(Duration::from_millis(100)) {
                if let Some(address) = line.strip_prefix("remote listening at ") {
                    quic = Some(address.parse().unwrap());
                }
                if let Some(address) = line.strip_prefix("remote TLS websocket listening at ") {
                    websocket = Some(address.parse().unwrap());
                }
            }
        }
        let quic = quic.unwrap_or_else(|| panic!("missing QUIC readiness: {:?}", process.stdout));
        let websocket = websocket
            .unwrap_or_else(|| panic!("missing websocket readiness: {:?}", process.stdout));
        (process, quic, websocket)
    }

    fn clean_stop(&mut self) {
        let signal = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(signal, 0, "send SIGINT to configured CLI server");
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("configured CLI server did not exit after SIGINT");
            }
            thread::sleep(Duration::from_millis(25));
        };
        assert!(
            status.success(),
            "configured CLI server exit {status}; stderr={:?}",
            self.stderr.lock().unwrap()
        );
        if let Some(handle) = self.stdout_thread.take() {
            handle.join().unwrap();
        }
        if let Some(handle) = self.stderr_thread.take() {
            handle.join().unwrap();
        }
        println!(
            "configured server stdout: {:?}",
            self.stdout.lock().unwrap()
        );
        println!(
            "configured server stderr: {:?}",
            self.stderr.lock().unwrap()
        );
    }

    fn assert_diagnostics(&self, enabled: bool) {
        assert!(self.stdout_thread.is_none() && self.stderr_thread.is_none());
        assert!(
            self.stdout
                .lock()
                .unwrap()
                .iter()
                .all(|line| !line.starts_with("service_diagnostics "))
        );
        let stderr = self.stderr.lock().unwrap();
        let records: Vec<_> = stderr
            .iter()
            .filter_map(|line| line.strip_prefix("service_diagnostics "))
            .collect();
        assert_eq!(
            records.len(),
            usize::from(enabled),
            "unexpected service records: {stderr:?}"
        );
        if !enabled {
            return;
        }
        assert!(records[0].len() + "service_diagnostics \n".len() <= 1024 * 1024);
        let record: serde_json::Value = serde_json::from_str(records[0]).unwrap();
        assert_eq!(record["schema"], "mount-rs.cli-service-diagnostics.v2");
        assert_eq!(record["pid"].as_u64(), Some(u64::from(self.child.id())));
        assert_eq!(record["transport"], "quic");
        assert_eq!(record["capture_context"], "shutdown");
        let snapshot = &record["snapshot"];
        assert_eq!(snapshot["schema"], "mount-rs.service-quic.v1");
        assert_eq!(snapshot["enabled"], true);
        let entries = snapshot["entries"].as_array().unwrap();
        let labels = [
            "admission.connection",
            "handshake.application",
            "handshake.tls",
            "auth.authenticate",
            "request.application",
            "request.read_incoming",
            "admission.ingress",
            "admission.egress",
            "dispatch.control",
            "dispatch.read",
            "dispatch.write",
            "response.encode",
            "response.submit",
            "session.cleanup",
        ];
        assert_eq!(entries.len(), labels.len());
        for (entry, label) in entries.iter().zip(labels) {
            assert_eq!(entry["name"], label);
        }
        for label in ["request.application", "dispatch.read", "dispatch.write"] {
            let entry = entries.iter().find(|entry| entry["name"] == label).unwrap();
            assert!(
                entry["calls"].as_u64().unwrap() > 0,
                "missing {label} calls"
            );
        }
        let transport = &snapshot["transport"];
        assert!(transport["registered_connections"].as_u64().unwrap() > 0);
        assert!(transport["retired_connections"].as_u64().unwrap() > 0);
        assert!(transport["retired"]["udp_tx_bytes"].as_u64().unwrap() > 0);
        assert!(transport["retired"]["udp_rx_bytes"].as_u64().unwrap() > 0);
        // Read the observer's own completeness envelope; endpoint closure alone
        // does not establish request-task drain or passive QUIC quiescence.
        for flag in [
            "complete",
            "counter_saturated",
            "concurrent_activity",
            "application_quiescent",
        ] {
            assert!(snapshot[flag].as_bool().is_some(), "missing {flag}");
        }
        assert!(transport["registry_complete"].as_bool().is_some());
        assert!(transport["unobserved_connections"].as_u64().is_some());
        assert!(transport["missing_final_samples"].as_u64().is_some());
        let process = &record["process_diagnostics"];
        assert_eq!(process["scope"], "process_cumulative");
        assert_eq!(process["capture_atomic"], false);
        assert_eq!(process["application_drain_proven"], false);
        assert_eq!(process["storage"]["available"], true);
        assert_eq!(process["profile"]["available"], true);
        let storage = &process["storage"]["snapshot"];
        let storage_entries = storage["entries"].as_array().unwrap();
        let names = mount_rs_core::diagnostics::storage::operation_names();
        assert_eq!(storage_entries.len(), 78);
        for (entry, name) in storage_entries.iter().zip(names) {
            assert_eq!(entry["name"], *name);
            assert_eq!(entry["latency_log2_us"].as_array().unwrap().len(), 32);
            for counter in ["in_flight", "returned_rows", "returned_row_observations"] {
                assert!(entry[counter].as_u64().is_some());
            }
        }
        assert!(storage["in_flight"].as_u64().is_some());
        assert_eq!(
            storage["forwarding_boxes"]["sites"],
            "napi_dynamic_provider_forwarding_future"
        );
        assert!(storage_entries.iter().any(|entry| {
            entry["name"].as_str().unwrap().starts_with("sdk.metadata.")
                && entry["success"].as_u64().unwrap() > 0
        }));
        for name in ["sdk.blocks.put", "sdk.blocks.get"] {
            let entry = storage_entries
                .iter()
                .find(|entry| entry["name"] == name)
                .unwrap();
            assert!(
                entry["success"].as_u64().unwrap() > 0,
                "missing {name} successes"
            );
            assert!(entry["bytes"].as_u64().unwrap() > 0, "missing {name} bytes");
        }
        let profile_entries = process["profile"]["snapshot"]["entries"]
            .as_array()
            .unwrap();
        for name in [
            "catalog.load",
            "filesystem.gate_wait",
            "provider.metadata.load",
            "provider.blocks.put_bytes",
            "provider.blocks.get_bytes",
        ] {
            let entry = profile_entries
                .iter()
                .find(|entry| entry["name"] == name)
                .unwrap();
            assert!(entry["calls"].as_u64().unwrap() > 0, "missing {name} calls");
            if name.starts_with("provider.blocks.") {
                assert!(entry["units"].as_u64().unwrap() > 0, "missing {name} units");
            }
        }
        for (family, count) in [
            ("sdk", 47),
            ("tidb", 18),
            ("napi_forwarding", 12),
            ("pglite", 1),
        ] {
            assert_eq!(
                process["coverage"][family]["rows"]
                    .as_array()
                    .unwrap()
                    .len(),
                count
            );
        }
        for name in [
            "raw_object_store",
            "http_attempts",
            "physical_device_iops",
            "process_cpu",
            "process_rss",
            "allocator_churn",
        ] {
            assert_eq!(process["unavailable"][name]["available"], false);
            assert!(process["unavailable"][name].get("snapshot").is_none());
        }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn roots(certificate: &rustls::pki_types::CertificateDer<'static>) -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.clone()).unwrap();
    roots
}

async fn connect(
    quic: SocketAddr,
    websocket: SocketAddr,
    certificate: &rustls::pki_types::CertificateDer<'static>,
    token: &Path,
    partition: &str,
    transport: ConnectionTransport,
) -> Result<Arc<RemoteConnection>, ClientError> {
    let selection = match transport {
        ConnectionTransport::Quic => ConnectionTransport::Quic,
        ConnectionTransport::WebSocket(_) => ConnectionTransport::WebSocket(websocket),
        ConnectionTransport::Auto { .. } => unreachable!(),
    };
    RemoteConnection::connect_with_transport(
        quic,
        "localhost",
        roots(certificate),
        partition.into(),
        CredentialSource::File(token.into()),
        selection,
    )
    .await
}

async fn exercise_transport(connection: Arc<RemoteConnection>, file: &str, payload: &[u8]) {
    let data = RemoteFsDriver::new(connection.clone(), "data".into())
        .await
        .unwrap();
    let logs = RemoteFsDriver::new(connection.clone(), "logs".into())
        .await
        .unwrap();
    assert!(logs.capabilities().read_only);
    assert_eq!(
        logs.write_file("/denied", b"x").await.unwrap_err().code,
        ErrorCode::Eacces
    );
    assert!(matches!(
        connection
            .request(
                "blue/data",
                Operation {
                    name: OperationName::Stat,
                    body: json!({"path":"/"}),
                },
            )
            .await,
        Err(ClientError::Remote(code)) if code == "EACCES"
    ));

    let handle = data.open(file, "w+", 0o644).await.unwrap();
    assert_eq!(handle.write(payload, Some(0)).await.unwrap(), payload.len());
    handle.sync().await.unwrap();
    let mut received = vec![0; payload.len() + 17];
    assert_eq!(
        handle.read(&mut received, Some(0)).await.unwrap(),
        payload.len()
    );
    assert_eq!(&received[..payload.len()], payload);
    assert_eq!(
        handle
            .read(&mut received, Some(payload.len() as u64))
            .await
            .unwrap(),
        0
    );
    handle.close().await.unwrap();
    data.syncfs().await.unwrap();
    let names: Vec<_> = data
        .readdir("/")
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert!(
        names
            .iter()
            .any(|name| name == file.trim_start_matches('/'))
    );
}

async fn verify_reopen(connection: Arc<RemoteConnection>, expected: &[(&str, &[u8])]) {
    let data = RemoteFsDriver::new(connection.clone(), "data".into())
        .await
        .unwrap();
    for (path, payload) in expected {
        let handle = data.open(path, "r", 0).await.unwrap();
        let mut received = vec![0; payload.len() + 1];
        assert_eq!(
            handle.read(&mut received, Some(0)).await.unwrap(),
            payload.len()
        );
        assert_eq!(&received[..payload.len()], *payload);
        assert_eq!(
            handle
                .read(&mut received, Some(payload.len() as u64))
                .await
                .unwrap(),
            0
        );
        handle.close().await.unwrap();
    }
    let names: Vec<_> = data
        .readdir("/")
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    for (path, _) in expected {
        assert!(
            names
                .iter()
                .any(|name| name == path.trim_start_matches('/'))
        );
    }
    connection.close();
}

fn write(path: &Path, contents: impl AsRef<[u8]>) {
    std::fs::write(path, contents).unwrap();
}

fn pem(label: &str, der: &[u8]) -> String {
    let encoded = STANDARD.encode(der);
    let mut document = format!("-----BEGIN {label}-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        document.push_str(std::str::from_utf8(chunk).unwrap());
        document.push('\n');
    }
    document.push_str(&format!("-----END {label}-----\n"));
    document
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_binary_selects_mrc5_for_signed_quic_and_websocket_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let tokens = tokens();
    let valid_token = root.join("valid.jwt");
    write(&valid_token, &tokens.valid);
    let invalid_signature = root.join("invalid-signature.jwt");
    write(&invalid_signature, &tokens.invalid_signature);
    let wrong_audience = root.join("wrong-audience.jwt");
    write(&wrong_audience, &tokens.wrong_audience);
    let expired = root.join("expired.jwt");
    write(&expired, &tokens.expired);
    let malformed = root.join("malformed.jwt");
    write(&malformed, "not-a-jwt");
    write(&root.join("fixture.jwks.json"), &tokens.jwks);

    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let certificate_der = certificate.cert.der().clone();
    write(
        &root.join("cert.pem"),
        pem("CERTIFICATE", certificate.cert.der().as_ref()),
    );
    write(
        &root.join("key.pem"),
        pem("PRIVATE KEY", &certificate.signing_key.serialize_der()),
    );

    let catalog_document = root.join("catalog.json");
    write(
        &catalog_document,
        json!({
            "revision":0,
            "partitions":{
                "red":{"drives":{
                    "data":{"driver":{"kind":"splitstore","storage":{
                        "metadata":{"kind":"sqlite","path":"data-metadata.sqlite"},
                        "blocks":{"kind":"sqlite","path":"data-blocks.sqlite"},
                        "compact_inode_updates":true
                    }}},
                    "logs":{"driver":{"kind":"memory"}}
                }},
                "blue":{"drives":{"data":{"driver":{"kind":"memory"}}}}
            },
            "issuer_policies":{"oidc":{
                "issuer":ISSUER,"audiences":[AUDIENCE],"algorithms":["ES256"]
            }},
            "grants":{"workload":{
                "partition_id":"red","policy_id":"oidc",
                "drives":{"data":"write","logs":"read"},
                "claim_conditions":{"/repository_id":"repo-1"}
            }}
        })
        .to_string(),
    );
    let apply_config = root.join("apply.json");
    write(
        &apply_config,
        json!({
            "version":1,"catalog":"service.sqlite","document":"catalog.json",
            "expected_revision":0
        })
        .to_string(),
    );
    let apply = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["catalog-apply", "--config"])
        .arg(&apply_config)
        .output()
        .unwrap();
    assert!(apply.status.success(), "{apply:?}");
    println!(
        "catalog apply stdout: {}",
        String::from_utf8_lossy(&apply.stdout)
    );

    let service_config = root.join("service.json");
    write(
        &service_config,
        json!({
            "version":1,"catalog":"service.sqlite",
            "listen":"127.0.0.1:0","websocket_listen":"127.0.0.1:0",
            "certificate":"cert.pem","private_key":"key.pem",
            "local_oidc_fixture":{
                "issuer":ISSUER,"audiences":[AUDIENCE],"jwks":"fixture.jwks.json"
            }
        })
        .to_string(),
    );

    let (mut server, quic, websocket) = ServerProcess::spawn(&service_config, true);
    for rejected in [&invalid_signature, &wrong_audience, &expired, &malformed] {
        assert!(
            connect(
                quic,
                websocket,
                &certificate_der,
                rejected,
                "red",
                ConnectionTransport::Quic,
            )
            .await
            .is_err(),
            "fixture accepted rejected token {}",
            rejected.display()
        );
    }
    assert!(
        connect(
            quic,
            websocket,
            &certificate_der,
            &invalid_signature,
            "red",
            ConnectionTransport::WebSocket(websocket),
        )
        .await
        .is_err()
    );
    assert!(
        connect(
            quic,
            websocket,
            &certificate_der,
            &valid_token,
            "blue",
            ConnectionTransport::Quic,
        )
        .await
        .is_err()
    );

    let quic_payload: Vec<u8> = (0..65_543).map(|index| (index % 251) as u8).collect();
    let websocket_payload: Vec<u8> = (0..32_777).map(|index| (index % 239) as u8).collect();
    let quic_connection = connect(
        quic,
        websocket,
        &certificate_der,
        &valid_token,
        "red",
        ConnectionTransport::Quic,
    )
    .await
    .unwrap();
    exercise_transport(quic_connection.clone(), "/quic.bin", &quic_payload).await;
    quic_connection.close();
    let websocket_connection = connect(
        quic,
        websocket,
        &certificate_der,
        &valid_token,
        "red",
        ConnectionTransport::WebSocket(websocket),
    )
    .await
    .unwrap();
    exercise_transport(
        websocket_connection.clone(),
        "/websocket.bin",
        &websocket_payload,
    )
    .await;
    websocket_connection.close();
    server.clean_stop();
    server.assert_diagnostics(cfg!(feature = "io-profiling"));

    let metadata_path = root.join("data-metadata.sqlite");
    let blocks_path = root.join("data-blocks.sqlite");
    let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
    let mode = metadata.compact_inode_mode_state().await.unwrap().unwrap();
    let compact = metadata.load_compact_snapshot(mode.backing).await.unwrap();
    assert_eq!(compact.anchor.generation, mode.structural_generation);
    let namespace = compact.namespace().unwrap();
    assert_eq!(namespace.nodes.len(), 3);
    let blocks = SqliteBlockStore::open(&blocks_path).unwrap();
    blocks
        .verify_concurrent_backing(mode.backing)
        .await
        .unwrap();
    let raw_mode: String = rusqlite::Connection::open(&metadata_path)
        .unwrap()
        .query_row(
            "SELECT write_mode FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw_mode, "MRC5");
    println!(
        "persisted compact receipt: mode={raw_mode} backing={:?} generation={} members={}",
        mode.backing,
        mode.structural_generation,
        compact.anchor.members.len()
    );
    drop(blocks);
    drop(metadata);

    let (mut reopened, reopen_quic, reopen_websocket) =
        ServerProcess::spawn(&service_config, false);
    let expected = [
        ("/quic.bin", quic_payload.as_slice()),
        ("/websocket.bin", websocket_payload.as_slice()),
    ];
    verify_reopen(
        connect(
            reopen_quic,
            reopen_websocket,
            &certificate_der,
            &valid_token,
            "red",
            ConnectionTransport::Quic,
        )
        .await
        .unwrap(),
        &expected,
    )
    .await;
    verify_reopen(
        connect(
            reopen_quic,
            reopen_websocket,
            &certificate_der,
            &valid_token,
            "red",
            ConnectionTransport::WebSocket(reopen_websocket),
        )
        .await
        .unwrap(),
        &expected,
    )
    .await;
    reopened.clean_stop();
    reopened.assert_diagnostics(false);

    let reopened_metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
    let reopened_mode = reopened_metadata
        .compact_inode_mode_state()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened_mode.backing, mode.backing);
    assert_eq!(
        reopened_mode.structural_generation,
        mode.structural_generation
    );
    let reopened_snapshot = reopened_metadata
        .load_compact_snapshot(reopened_mode.backing)
        .await
        .unwrap();
    assert_eq!(reopened_snapshot.namespace().unwrap().nodes.len(), 3);
    println!(
        "fresh reopen compact receipt: backing={:?} generation={} members={}",
        reopened_mode.backing,
        reopened_mode.structural_generation,
        reopened_snapshot.anchor.members.len()
    );

    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let result = unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, usage.as_mut_ptr()) };
    assert_eq!(result, 0, "read configured CLI child resource usage");
    let usage = unsafe { usage.assume_init() };
    println!(
        "configured CLI child resource receipt: ru_maxrss={} platform_units",
        usage.ru_maxrss
    );
}
