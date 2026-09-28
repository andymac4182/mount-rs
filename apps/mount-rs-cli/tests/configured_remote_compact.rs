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
use mount_rs_service::service_diagnostics_frames;
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
    stderr_receiver: mpsc::Receiver<String>,
    stdout_thread: Option<thread::JoinHandle<()>>,
    stderr_thread: Option<thread::JoinHandle<()>>,
}

impl ServerProcess {
    fn spawn(config: &Path, diagnostics: bool) -> (Self, SocketAddr, SocketAddr) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mount-rs"));
        // The disabled restart deliberately keeps the interval opt-in. A
        // selected profiler is also required before a timer or capture exists.
        command.env("MOUNT_RS_DIAGNOSTIC_INTERVAL_MS", "1000");
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
        let (stderr_tx, stderr_receiver) = mpsc::channel();
        let stderr_thread = thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines() {
                let Ok(line) = line else { break };
                stderr_lines.lock().unwrap().push(line.clone());
                let _ = stderr_tx.send(line);
            }
        });
        let mut process = Self {
            child,
            stdout,
            stderr,
            stderr_receiver,
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

    fn wait_for_periodic_io_records(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut captures = std::collections::BTreeMap::<
            u64,
            std::collections::BTreeMap<String, serde_json::Value>,
        >::new();
        let mut decoder = service_diagnostics_frames::Decoder::default();
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return Err("server stopped before live capture".into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "missing periodic I/O records before SIGINT: {:?}",
                    self.stderr.lock().unwrap()
                ));
            }
            let line = match self.stderr_receiver.recv_timeout(remaining) {
                Ok(line) => line,
                Err(error) => {
                    return Err(format!(
                        "missing periodic I/O records before SIGINT: {error}; stderr={:?}",
                        self.stderr.lock().unwrap()
                    ));
                }
            };
            if line
                .as_bytes()
                .starts_with(service_diagnostics_frames::PREFIX)
            {
                assert!(line.len() < service_diagnostics_frames::LINE_LIMIT);
            }
            let Some(raw) = decoder
                .push_line(&line)
                .expect("valid contiguous live service diagnostic frames")
            else {
                continue;
            };
            let record: serde_json::Value = serde_json::from_slice(&raw).unwrap();
            assert_eq!(
                record["capture_context"], "periodic",
                "shutdown record appeared before SIGINT"
            );
            assert_eq!(record["schema"], "mount-rs.cli-service-diagnostics.v2");
            assert_eq!(record["pid"].as_u64(), Some(u64::from(self.child.id())));
            let sequence = record["capture"]["sequence"].as_u64().unwrap();
            assert!(sequence > 0);
            assert!(record["capture"]["observed_unix_ms"].as_u64().unwrap() > 0);
            let label = record["transport"].as_str().unwrap().to_owned();
            assert!(["quic", "websocket"].contains(&label.as_str()));
            let pair = captures.entry(sequence).or_default();
            assert!(
                pair.insert(label, record).is_none(),
                "duplicate transport capture in one tick"
            );
            let (Some(quic), Some(websocket)) = (pair.get("quic"), pair.get("websocket")) else {
                continue;
            };
            assert_eq!(
                quic["capture"], websocket["capture"],
                "listeners did not share one tick identity"
            );
            if ![quic, websocket]
                .into_iter()
                .all(Self::periodic_has_completed_io)
            {
                continue;
            }
            for record in [quic, websocket] {
                let snapshot = &record["snapshot"];
                assert_eq!(snapshot["enabled"], true);
                for flag in [
                    "complete",
                    "counter_saturated",
                    "concurrent_activity",
                    "application_quiescent",
                ] {
                    assert!(
                        snapshot[flag].as_bool().is_some(),
                        "missing live capture flag {flag}"
                    );
                }
                for gauge in [
                    "activity_writers_before",
                    "activity_writers_after",
                    "active_handshakes_before",
                    "active_handshakes_after",
                    "active_requests_before",
                    "active_requests_after",
                    "active_operations",
                ] {
                    assert!(
                        snapshot[gauge].as_u64().is_some(),
                        "missing live capture gauge {gauge}"
                    );
                }
                for entry in snapshot["entries"].as_array().unwrap() {
                    for counter in [
                        "calls",
                        "success",
                        "error",
                        "timeout",
                        "cancelled",
                        "in_flight",
                    ] {
                        assert!(entry[counter].as_u64().is_some(), "missing live {counter}");
                    }
                    assert_eq!(entry["latency_log2_us"].as_array().unwrap().len(), 32);
                }
                let process = &record["process_diagnostics"];
                assert_eq!(process["scope"], "process_cumulative");
                assert_eq!(process["capture_atomic"], false);
                assert_eq!(process["application_drain_proven"], false);
                assert_eq!(
                    process["duration_semantics"],
                    "inclusive_overlapping_wall_time"
                );
                assert_eq!(process["storage"]["available"], true);
                assert_eq!(process["profile"]["available"], true);
                assert!(
                    process["storage"]["snapshot"]["in_flight"]
                        .as_u64()
                        .is_some()
                );
                for entry in process["storage"]["snapshot"]["entries"]
                    .as_array()
                    .unwrap()
                {
                    assert!(entry["in_flight"].as_u64().is_some());
                }
                assert_eq!(
                    process["unavailable"]["physical_device_iops"]["available"],
                    false
                );
            }
            assert_eq!(quic["snapshot"]["schema"], "mount-rs.service-quic.v1");
            assert_eq!(
                websocket["snapshot"]["schema"],
                "mount-rs.service-websocket.v1"
            );
            assert_eq!(
                websocket["snapshot"]["quiescence_scope"],
                "application_spans_and_session_cleanup_not_passive_websocket"
            );
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "periodic evidence arrived after server exit"
            );
            println!(
                "live periodic diagnostic receipt before SIGINT: sequence={sequence} quic={quic} websocket={websocket}"
            );
            return Ok(());
        }
    }

    fn periodic_has_completed_io(record: &serde_json::Value) -> bool {
        let entries = record["snapshot"]["entries"].as_array().unwrap();
        let completed = |name| {
            entries
                .iter()
                .any(|entry| entry["name"] == name && entry["success"].as_u64().unwrap() > 0)
        };
        if !completed("dispatch.read") || !completed("dispatch.write") {
            return false;
        }
        let process = &record["process_diagnostics"];
        let storage = process["storage"]["snapshot"]["entries"]
            .as_array()
            .unwrap();
        let metadata_seen = storage.iter().any(|entry| {
            entry["name"].as_str().unwrap().starts_with("sdk.metadata.")
                && entry["calls"].as_u64().unwrap() > 0
        });
        let profile = process["profile"]["snapshot"]["entries"]
            .as_array()
            .unwrap();
        metadata_seen
            && ["provider.blocks.put_bytes", "provider.blocks.get_bytes"]
                .into_iter()
                .all(|name| {
                    profile.iter().any(|entry| {
                        entry["name"] == name
                            && entry["calls"].as_u64().unwrap() > 0
                            && entry["units"].as_u64().unwrap() > 0
                    })
                })
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
        assert!(self.stdout.lock().unwrap().iter().all(|line| {
            !line.starts_with("service_diagnostics ")
                && !line
                    .as_bytes()
                    .starts_with(service_diagnostics_frames::PREFIX)
        }));
        let stderr = self.stderr.lock().unwrap();
        let mut decoder = service_diagnostics_frames::Decoder::default();
        let mut records = Vec::<serde_json::Value>::new();
        for line in stderr.iter() {
            assert!(
                !line.starts_with("service_diagnostics "),
                "service diagnostics bypassed physical framing"
            );
            if line
                .as_bytes()
                .starts_with(service_diagnostics_frames::PREFIX)
            {
                assert!(line.len() < service_diagnostics_frames::LINE_LIMIT);
            }
            if let Some(raw) = decoder
                .push_line(line)
                .expect("valid contiguous retained service diagnostic frames")
            {
                records.push(serde_json::from_slice(&raw).unwrap());
            }
        }
        decoder
            .finish()
            .expect("complete retained service diagnostic frame groups");
        if !enabled {
            assert!(
                records.is_empty(),
                "disabled restart emitted service records: {stderr:?}"
            );
            return;
        }
        assert!(
            records
                .iter()
                .filter(|record| record["capture_context"] == "periodic")
                .count()
                >= 2,
            "missing live records"
        );
        assert!(records.iter().all(|record| {
            ["periodic", "shutdown"]
                .iter()
                .any(|context| record["capture_context"] == *context)
        }));
        let records: Vec<_> = records
            .into_iter()
            .filter(|record| record["capture_context"] == "shutdown")
            .collect();
        assert_eq!(records.len(), 2, "missing or duplicate shutdown records");
        assert!(
            records.iter().all(|record| record.get("capture").is_none()),
            "shutdown v2 shape changed"
        );
        for label in ["quic", "websocket"] {
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record["transport"] == label)
                    .count(),
                1,
                "missing or duplicate {label} record"
            );
        }
        let record = records
            .iter()
            .find(|record| record["transport"] == "quic")
            .unwrap();
        assert_eq!(record["schema"], "mount-rs.cli-service-diagnostics.v2");
        assert_eq!(record["pid"].as_u64(), Some(u64::from(self.child.id())));
        assert_eq!(record["transport"], "quic");
        assert_eq!(record["capture_context"], "shutdown");
        let snapshot = &record["snapshot"];
        assert_eq!(snapshot["schema"], "mount-rs.service-quic.v1");
        assert_eq!(snapshot["enabled"], true);
        let entries = snapshot["entries"].as_array().unwrap();
        // Five QUIC attempts below reach the actual server authenticator; the
        // malformed credential is rejected by the client before ClientHello.
        // WebSocket calls share the cache and use their own observer context.
        for (name, calls, success, error) in [
            ("auth.decode", 5, 5, 0),
            ("auth.catalog", 5, 5, 0),
            ("auth.cache.wait", 5, 5, 0),
            ("auth.policy.select", 5, 5, 0),
            ("auth.key.fetch", 1, 1, 0),
            ("auth.jwt.verify", 4, 1, 3),
            ("auth.grant.authorize", 1, 1, 0),
        ] {
            let entry = entries
                .iter()
                .find(|entry| entry["name"] == name)
                .unwrap_or_else(|| panic!("missing actual CatalogAuthenticator stage {name}"));
            assert_eq!(entry["calls"].as_u64(), Some(calls), "{name} calls");
            assert_eq!(entry["success"].as_u64(), Some(success), "{name} success");
            assert_eq!(entry["error"].as_u64(), Some(error), "{name} error");
            for counter in ["timeout", "cancelled", "in_flight"] {
                assert_eq!(entry[counter].as_u64(), Some(0), "{name} {counter}");
            }
            let histogram = entry["latency_log2_us"].as_array().unwrap();
            assert_eq!(histogram.len(), 32);
            assert_eq!(
                histogram
                    .iter()
                    .map(|value| value.as_u64().unwrap())
                    .sum::<u64>(),
                calls,
                "{name} terminal latency samples"
            );
        }
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
            "auth.decode",
            "auth.catalog",
            "auth.cache.wait",
            "auth.policy.select",
            "auth.key.fetch",
            "auth.jwt.verify",
            "auth.grant.authorize",
            "handshake.websocket_upgrade",
        ];
        assert_eq!(entries.len(), labels.len());
        for (entry, label) in entries.iter().zip(labels) {
            assert_eq!(entry["name"], label);
        }
        let upgrade = entries.last().unwrap();
        for counter in [
            "calls",
            "success",
            "error",
            "timeout",
            "cancelled",
            "in_flight",
        ] {
            assert_eq!(upgrade[counter].as_u64(), Some(0), "QUIC {counter}");
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
        assert_eq!(storage_entries.len(), 116);
        assert_eq!(names.len(), 116);
        // This SQLite fixture proves current export inventory, not marker activity.
        assert_eq!(
            storage_entries[110..116]
                .iter()
                .map(|row| row["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "object_store.backing_marker.probe.get",
                "object_store.backing_marker.probe.body_read",
                "object_store.backing_marker.data.get",
                "object_store.backing_marker.data.body_read",
                "object_store.backing_marker.probe.create",
                "object_store.backing_marker.retry_backoff",
            ]
        );
        assert_eq!(storage_entries[77]["name"], "tidb.sql.flush_probe");
        assert_eq!(
            storage_entries[78..85]
                .iter()
                .map(|entry| entry["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "foundationdb.transaction.create",
                "foundationdb.transaction.closure_attempt",
                "foundationdb.read.get",
                "foundationdb.read.get_key",
                "foundationdb.read.get_range_page",
                "foundationdb.transaction.commit",
                "foundationdb.transaction.on_error",
            ]
        );
        assert_eq!(
            storage_entries[85..91]
                .iter()
                .map(|entry| entry["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "blob_cache.miss.admission_wait",
                "blob_cache.miss.singleflight_wait",
                "blob_cache.ram.lookup",
                "blob_cache.disk.lookup",
                "blob_cache.peer.connection_lock_wait",
                "blob_cache.peer.connection_establish",
            ]
        );
        assert_eq!(storage_entries[91]["name"], "client.quic.open_bi");
        assert_eq!(
            storage_entries[92..100]
                .iter()
                .map(|row| row["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "client.quic.request_send",
                "client.quic.response_receive",
                "blob_cache.peer.request_byte_admission_wait",
                "blob_cache.peer.open_bi",
                "blob_cache.peer.request_send",
                "blob_cache.peer.response_receive",
                "blob_cache.peer.get",
                "blob_cache.peer.get_miss",
            ]
        );
        assert_eq!(
            storage_entries[100..108]
                .iter()
                .map(|row| row["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "client.websocket.tcp_connect",
                "client.websocket.tls_handshake",
                "client.websocket.upgrade",
                "client.websocket.socket_lock_wait",
                "client.websocket.request_encode",
                "client.websocket.request_send",
                "client.websocket.response_receive",
                "client.websocket.response_decode",
            ]
        );
        for entry in &storage_entries[100..108] {
            assert_eq!(entry["calls"].as_u64(), Some(0));
            assert_eq!(entry["bytes"].as_u64(), Some(0));
        }
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
            ("client_websocket", 8),
        ] {
            assert_eq!(
                process["coverage"][family]["rows"]
                    .as_array()
                    .unwrap()
                    .len(),
                count
            );
        }
        assert_eq!(
            process["coverage"]["client_websocket"]["observation"],
            "declared_not_cli_server_source_instrumented"
        );
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
        let websocket = records
            .iter()
            .find(|record| record["transport"] == "websocket")
            .unwrap();
        self.assert_websocket_diagnostics(websocket, &labels);
        assert_eq!(websocket["process_diagnostics"], *process);
    }

    fn assert_websocket_diagnostics(&self, record: &serde_json::Value, labels: &[&str]) {
        assert_eq!(record["schema"], "mount-rs.cli-service-diagnostics.v2");
        assert_eq!(record["pid"].as_u64(), Some(u64::from(self.child.id())));
        assert_eq!(record["transport"], "websocket");
        assert_eq!(record["capture_context"], "shutdown");
        let snapshot = &record["snapshot"];
        assert_eq!(snapshot["schema"], "mount-rs.service-websocket.v1");
        assert_eq!(snapshot["enabled"], true);
        assert!(snapshot.get("transport").is_none());
        assert!(snapshot.get("registry_snapshot_elapsed_ns").is_none());
        assert_eq!(
            snapshot["known_unavailable"],
            json!([
                "tcp_wire_bytes",
                "tls_wire_bytes",
                "websocket_frame_counts",
                "peer_acknowledgement",
                "process_cpu",
                "physical_device_iops",
            ])
        );
        assert_eq!(
            snapshot["quiescence_scope"],
            "application_spans_and_session_cleanup_not_passive_websocket"
        );
        assert_eq!(
            snapshot["response_scope"],
            "encoding_and_websocket_sink_submission_not_peer_acknowledgement"
        );
        assert_eq!(snapshot["complete"], true);
        assert_eq!(snapshot["application_quiescent"], true);
        assert_eq!(snapshot["concurrent_activity"], false);
        for gauge in [
            "activity_writers_before",
            "activity_writers_after",
            "active_handshakes_before",
            "active_handshakes_after",
            "active_requests_before",
            "active_requests_after",
            "active_operations",
        ] {
            assert_eq!(snapshot[gauge].as_u64(), Some(0), "unsettled {gauge}");
        }
        let entries = snapshot["entries"].as_array().unwrap();
        assert_eq!(entries.len(), labels.len());
        for (entry, label) in entries.iter().zip(labels) {
            assert_eq!(entry["name"], *label);
            assert_eq!(entry["in_flight"].as_u64(), Some(0), "{label} still active");
            let calls = entry["calls"].as_u64().unwrap();
            let terminal = ["success", "error", "timeout", "cancelled"]
                .iter()
                .map(|counter| entry[*counter].as_u64().unwrap())
                .sum::<u64>();
            assert_eq!(calls, terminal, "{label} terminal outcomes");
            let histogram = entry["latency_log2_us"].as_array().unwrap();
            assert_eq!(histogram.len(), 32);
            assert_eq!(
                histogram
                    .iter()
                    .map(|count| count.as_u64().unwrap())
                    .sum::<u64>(),
                calls
            );
        }
        // Two WebSocket attempts use the actual CatalogAuthenticator and its
        // shared, already-warmed key cache. QUIC counts above remain separate.
        for (name, calls, success, error) in [
            ("auth.decode", 2, 2, 0),
            ("auth.catalog", 2, 2, 0),
            ("auth.cache.wait", 2, 2, 0),
            ("auth.policy.select", 2, 2, 0),
            ("auth.key.fetch", 0, 0, 0),
            ("auth.jwt.verify", 2, 1, 1),
            ("auth.grant.authorize", 1, 1, 0),
        ] {
            let entry = entries.iter().find(|entry| entry["name"] == name).unwrap();
            assert_eq!(entry["calls"].as_u64(), Some(calls), "{name} calls");
            assert_eq!(entry["success"].as_u64(), Some(success), "{name} success");
            assert_eq!(entry["error"].as_u64(), Some(error), "{name} error");
        }
        for name in [
            "handshake.websocket_upgrade",
            "request.application",
            "dispatch.read",
            "dispatch.write",
            "session.cleanup",
        ] {
            let entry = entries.iter().find(|entry| entry["name"] == name).unwrap();
            assert!(
                entry["success"].as_u64().unwrap() > 0,
                "missing {name} successes"
            );
        }
        let process = &record["process_diagnostics"];
        assert_eq!(process["application_drain_proven"], false);
        assert_eq!(
            process["storage"]["snapshot"]["in_flight"].as_u64(),
            Some(0)
        );
        assert!(
            process["storage"]["snapshot"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry["in_flight"].as_u64() == Some(0))
        );
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(handle) = self.stdout_thread.take() {
            handle.join().unwrap();
        }
        if let Some(handle) = self.stderr_thread.take() {
            handle.join().unwrap();
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
                        "metadata":{"kind":"sqlite","path":"data-metadata.sqlite","journal_mode":"wal"},
                        "blocks":{"kind":"sqlite","path":"data-blocks.sqlite","journal_mode":"wal"},
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
    // Observe the actual child before any accepted workload RPC. Defer these
    // assertions until both child lifetimes and existing byte/EOF/backing
    // oracles finish, so an intended eager-start RED also completes cleanup.
    let cold_at_listener_readiness = [
        root.join("data-metadata.sqlite"),
        root.join("data-blocks.sqlite"),
    ]
    .into_iter()
    .all(|path| !path.exists());
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
    let cold_after_authentication_denials = [
        root.join("data-metadata.sqlite"),
        root.join("data-blocks.sqlite"),
    ]
    .into_iter()
    .all(|path| !path.exists());
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
    let live_diagnostics =
        cfg!(feature = "io-profiling").then(|| server.wait_for_periodic_io_records());
    server.clean_stop();
    // A missing-live-capture RED still obtains graceful shutdown records and
    // joins both pipe readers before reporting the failed requirement.
    if let Some(result) = live_diagnostics {
        result.unwrap();
    }
    server.assert_diagnostics(cfg!(feature = "io-profiling"));

    let metadata_path = root.join("data-metadata.sqlite");
    let blocks_path = root.join("data-blocks.sqlite");
    for path in [&metadata_path, &blocks_path] {
        let connection = rusqlite::Connection::open(path).unwrap();
        let journal: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        assert_eq!(synchronous, 2);
        println!(
            "configured SQLite reopen receipt: journal_mode={journal} synchronous={synchronous}"
        );
    }
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
    assert!(
        cold_at_listener_readiness,
        "actual configured CLI opened Drive providers before the first authorized operation"
    );
    assert!(
        cold_after_authentication_denials,
        "rejected authentication activated a cold Drive provider"
    );
}
