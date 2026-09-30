//! Synthetic artifact shape regression: actual QUIC delta serialization and
//! Journal::flush; no provider, socket, native mount, or capacity measurement.
use super::*;
use std::io::{BufReader, Read};

const DRIVES: usize = 10_000;
const CELLS: usize = 16;
// Private capture owner v3/v4 sampled native_output + short_tmp every 2s.
// This is its regular-file limit, not an aggregate publisher reservation.
const CAPTURE_REGULAR_FILE_LIMIT: u64 = 16 * 1024 * 1024;
const BASELINE_COMMIT: &str = "25f8d6f30714cc1a937aa0fe029fe967529cf566";
const BASELINE_TREE: &str = "e56798c27402ead2ca4bc450c2ae7334143e8a4a";
const PRESERVED_METRICS_SHA256: &str =
    "46e39eea032f706e03af08ec146335dc2883a6c1fe280ae6a1b16b11ff3a738b";

fn compiled_source_pins() -> Value {
    let sources: &[(&str, &[u8])] = &[
        ("mod.rs", include_bytes!("mod.rs")),
        (
            "artifact_size_tests.rs",
            include_bytes!("artifact_size_tests.rs"),
        ),
        ("metrics.rs", include_bytes!("metrics.rs")),
        ("config.rs", include_bytes!("config.rs")),
        ("state.rs", include_bytes!("state.rs")),
        ("progress.rs", include_bytes!("progress.rs")),
        ("timing.rs", include_bytes!("timing.rs")),
        (
            "../resource_profile.rs",
            include_bytes!("../resource_profile.rs"),
        ),
        ("../device_io.rs", include_bytes!("../device_io.rs")),
        (
            "../resource_profile/sqlite_heap.rs",
            include_bytes!("../resource_profile/sqlite_heap.rs"),
        ),
        (
            "../../quic_production_target.rs",
            include_bytes!("../../quic_production_target.rs"),
        ),
        ("../../../Cargo.toml", include_bytes!("../../../Cargo.toml")),
        (
            "../../../../../Cargo.toml",
            include_bytes!("../../../../../Cargo.toml"),
        ),
        (
            "../../../../../Cargo.lock",
            include_bytes!("../../../../../Cargo.lock"),
        ),
        (
            "../../../../../scripts/cargo-shared",
            include_bytes!("../../../../../scripts/cargo-shared"),
        ),
        (
            "../../../../../scripts/cargo-shared-env.sh",
            include_bytes!("../../../../../scripts/cargo-shared-env.sh"),
        ),
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/production_target");
    let mut pins = std::collections::BTreeMap::new();
    for (path, compiled) in sources {
        assert_eq!(
            std::fs::read(root.join(path)).unwrap().as_slice(),
            *compiled,
            "artifact regression binary/source mismatch: {path}"
        );
        pins.insert(*path, digest(compiled));
    }
    assert_eq!(pins["metrics.rs"], PRESERVED_METRICS_SHA256);
    json!(pins)
}

fn stream_digest(path: &Path) -> String {
    let mut input = BufReader::new(std::fs::File::open(path).unwrap());
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    digest
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
#[ignore = "explicit production-shaped synthetic terminal capture regression"]
fn settled_full_geometry_terminal_obeys_capture_regular_file_limit() {
    let pins = compiled_source_pins();
    let configuration = Config {
        full_target: true,
        drives: DRIVES,
        files: 1000,
        seconds: 30,
        population_seconds: PHASE_SECONDS,
        provider: "sqlite".into(),
    };
    configuration.validate().unwrap();
    assert_eq!(PATTERNS.len() * 2, CELLS);
    let directory = tempfile::tempdir().unwrap();
    let now = Instant::now();
    let mut journal = Journal {
        value: json!({
            "phase":"terminal",
            "last_work_phase":"revocation",
            "created_unix_ms":1000,
            "phase_started_unix_ms":2000,
            "full_target":true,
            "configuration":&configuration,
            "stages":[],
            "workload_complete":true,
            "metrics_complete":true,
            "error":null,
            "cleanup_errors":[],
            "synthetic_artifact": {
                "scope":"serialization fixture only; no native/provider capacity qualification",
                "baseline_commit":BASELINE_COMMIT,
                "baseline_tree":BASELINE_TREE
            },
            "source":{"compiled_sources_sha256":&pins}
        }),
        output: directory.path().to_owned(),
        counts: (0..DRIVES)
            .map(|_| Arc::new(Mutex::new(state::Counts::default())))
            .collect(),
        enclosing_deadline: now + Duration::from_secs(WORK_SECONDS),
        phase_deadline: now + Duration::from_secs(PHASE_SECONDS),
        progress: progress::Progress::disabled(),
        oracle_progress: None,
    };
    let mut cell = 0;
    for mostly_idle in [true, false] {
        for pattern in PATTERNS {
            let mode = if mostly_idle {
                "mostly_idle"
            } else {
                "all_active"
            };
            let active = configuration.active(mostly_idle);
            for counts in &journal.counts[..active] {
                let mut counts = counts.lock().unwrap();
                let request = counts.begin();
                counts.acknowledge(request).unwrap();
            }
            let sequence = 21 + 3 * cell;
            let network =
                resource_profile::artifact_fixture_connection_deltas(cell, DRIVES).unwrap();
            journal.value["stages"].as_array_mut().unwrap().push(json!({
                "mode":mode,
                "pattern":pattern,
                "metric_sequences":[sequence,sequence+1,sequence+2],
                "rpc_latency_histogram_log2_microseconds":vec![0_u64;32],
                "timing":{
                    "active_elapsed_seconds":30.0,
                    "idle_liveness_elapsed_seconds":1.0,
                    "phase_elapsed_seconds":32.0,
                    "metrics_observer_elapsed_seconds":1.0,
                    "cycles_per_second":active as f64/30.0
                },
                "controller_quic_boundary":{
                    "scope":"actual retained client connections; active workload plus idle liveness; snapshot observer outside active throughput interval; server transport retained separately in worker phase receipts",
                    "connections":network
                },
                "configured_active_clients":active,
                "clients_with_completed_cycles":active,
                "connected_clients":DRIVES,
                "idle_liveness_acknowledgments":DRIVES-active,
                "cycles":active,
                "elapsed_seconds":32.0,
                "elapsed_scope":"overall phase including boundary observers, active work and idle liveness",
                "requested_seconds":30,
                "acknowledged_requests":active,
                "scope":"RPC acknowledgements include open/close; cycles are workload operations, not physical IOPS"
            }));
            cell += 1;
        }
    }
    assert_eq!(cell, CELLS);
    // This is the actual settled terminal publisher, preserving the streamed
    // compressed metric publisher unchanged.
    journal.flush().unwrap();
    drop(journal);
    let terminal_path = directory.path().join("terminal.json");
    let metadata = std::fs::symlink_metadata(&terminal_path).unwrap();
    assert!(metadata.is_file());
    assert!(!directory.path().join("terminal.pending").exists());
    let terminal: Value =
        serde_json::from_reader(BufReader::new(std::fs::File::open(&terminal_path).unwrap()))
            .unwrap();
    assert_eq!(terminal["phase"], "terminal");
    assert_eq!(terminal["configuration"]["drives"], DRIVES);
    assert_eq!(terminal["configuration"]["files"], 1000);
    assert_eq!(terminal["configuration"]["seconds"], 30);
    let counts = terminal["lanes"].as_array().unwrap();
    assert_eq!(counts.len(), DRIVES);
    for (lane, counts) in counts.iter().enumerate() {
        let acknowledged = if lane < configuration.active(true) {
            16
        } else {
            8
        };
        assert_eq!(counts["attempts"], acknowledged);
        assert_eq!(counts["acknowledged"], acknowledged);
        assert_eq!(counts["next_id"], acknowledged);
        assert_eq!(counts["failed"], 0);
        assert_eq!(counts["uncertain"], 0);
        assert!(counts["pending"].is_null());
        assert!(
            counts["uncertain_request_ids"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(terminal["source"]["compiled_sources_sha256"], pins);
    let stages = terminal["stages"].as_array().unwrap();
    assert_eq!(stages.len(), CELLS);
    for (cell, stage) in stages.iter().enumerate() {
        assert_eq!(
            stage["mode"],
            if cell < 8 {
                "mostly_idle"
            } else {
                "all_active"
            }
        );
        assert_eq!(stage["pattern"], PATTERNS[cell % 8]);
        let sequence = 21 + 3 * cell;
        assert_eq!(
            stage["metric_sequences"],
            json!([sequence, sequence + 1, sequence + 2])
        );
        assert_eq!(stage["connected_clients"], DRIVES);
        assert_eq!(stage["requested_seconds"], 30);
        let rows = stage["controller_quic_boundary"]["connections"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), DRIVES);
        for (lane, row) in rows.iter().enumerate() {
            assert_eq!(row.as_object().unwrap().len(), 2);
            assert_eq!(row["lane"], lane);
            let lane = lane as u64;
            let cell = cell as u64;
            let lost = (lane + cell) % 3;
            assert_eq!(
                row["quic"],
                json!({
                    "tx_bytes":1_000_000+lane*4096+cell,
                    "rx_bytes":2_000_000+lane*8192+cell,
                    "tx_datagrams":1000+lane+cell,
                    "rx_datagrams":2000+lane+cell,
                    "tx_ios":900+lane+cell,
                    "rx_ios":1800+lane+cell,
                    "lost_packets":lost,
                    "lost_bytes":lost*1200,
                    "sent_packets":1000+lane+cell,
                    "congestion_events":cell%4
                })
            );
        }
    }
    println!(
        "ARTIFACT_LAYOUT_MEASUREMENT {}",
        json!({
            "cells":CELLS,
            "connections_per_cell":DRIVES,
            "counter_fields_per_connection":10,
            "terminal_bytes":metadata.len(),
            "terminal_sha256":stream_digest(&terminal_path),
            "regular_file_limit_bytes":CAPTURE_REGULAR_FILE_LIMIT,
            "baseline_commit":BASELINE_COMMIT,
            "baseline_tree":BASELINE_TREE,
            "compiled_sources_sha256":pins,
            "scope":"synthetic artifact serialization only; no native/provider capacity result"
        })
    );
    // The sole intended RED assertion, after complete retained-row validation.
    assert!(
        metadata.len() <= CAPTURE_REGULAR_FILE_LIMIT,
        "TERMINAL_CAPTURE_REGULAR_FILE_LIMIT bytes={} limit={}",
        metadata.len(),
        CAPTURE_REGULAR_FILE_LIMIT
    );
}
