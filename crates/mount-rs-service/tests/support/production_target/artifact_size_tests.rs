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
// GREEN binds the combined writer/reader source after formatting. Historical
// RED receipts retain their original publisher source identity.
const GENERIC_METRICS_SHA256: &str =
    "56feec698c9ce019404de8c99900516e5bf3cf27dca7ae4173f3df42696ff61f";

fn compiled_source_pins() -> Value {
    let sources: &[(&str, &[u8])] = &[
        ("mod.rs", include_bytes!("mod.rs")),
        (
            "artifact_size_tests.rs",
            include_bytes!("artifact_size_tests.rs"),
        ),
        ("metrics.rs", include_bytes!("metrics.rs")),
        ("quic_artifacts.rs", include_bytes!("quic_artifacts.rs")),
        ("artifacts.rs", include_bytes!("artifacts.rs")),
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
    assert_eq!(pins["metrics.rs"], GENERIC_METRICS_SHA256);
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

fn synthetic_metric_boundary(
    journal: &mut Journal,
    phase: &str,
    boundary: &str,
    sequence: usize,
) -> Value {
    synthetic_metric_boundary_generation(journal, phase, boundary, sequence, 1)
}

fn synthetic_metric_boundary_generation(
    journal: &mut Journal,
    phase: &str,
    boundary: &str,
    sequence: usize,
    generation: usize,
) -> Value {
    let identity = json!({
        "pid":journal.value["controller_resources"]["pid"],"controller_pid":journal.value["controller_resources"]["pid"],
        "role":"controller","server":null,"generation":generation,"sequence":sequence,
        "phase":phase,"boundary":boundary,"source_digest":journal.value["source"]["digest"],
        "binary_digest":journal.value["source"]["binary_sha256"],"catalog_digest":digest(b"synthetic-artifact-catalog"),
        "backend_prefix":"synthetic-artifact-backing"
    });
    let name = format!("metrics/g{generation}-s{sequence}.json.gz");
    let path = journal.output.join(&name);
    std::fs::create_dir_all(journal.output.join("metrics")).unwrap();
    metrics::publish_immutable(
        &path,
        &json!({"identity":identity,"capture_complete":true,"metrics_complete":true}),
    )
    .unwrap();
    let mut workers = Vec::new();
    if journal.value["synthetic_artifact"]["combined_corpus"] == true {
        for server in 0..SERVERS {
            let pid = 1_000_000 + server;
            let mut worker_identity = identity.clone();
            worker_identity["pid"] = json!(pid);
            worker_identity["role"] = json!("worker");
            worker_identity["server"] = json!(server);
            let worker_name = format!("worker-{server}/metrics/g{generation}-s{sequence}.json.gz");
            let worker_path = journal.output.join(&worker_name);
            std::fs::create_dir_all(worker_path.parent().unwrap()).unwrap();
            metrics::publish_immutable(&worker_path, &json!({
                "identity":worker_identity,"capture_complete":true,"metrics_complete":true,
                "synthetic_artifact":true,"scope":"synthetic identity envelope; no production metric payload size claim"
            })).unwrap();
            workers.push(json!({"server":server,"pid":pid,"file":worker_name,"sha256":file_digest(&worker_path).unwrap()}));
        }
    }
    journal.value["phase_metrics"]["boundaries"].as_array_mut().unwrap().push(json!({
        "generation":generation,"sequence":sequence,"phase":phase,"boundary":boundary,"complete":true,"metrics_complete":true,"workers":workers,
        "controller":{"file":name,"sha256":file_digest(&path).unwrap()}
    }));
    identity
}

#[test]
#[ignore = "explicit production-shaped synthetic terminal capture regression"]
fn settled_full_geometry_terminal_obeys_capture_regular_file_limit() {
    settled_artifact_fixture(false);
}

#[test]
#[ignore = "explicit complete synthetic stored corpus; no native/provider capacity qualification"]
fn settled_combined_full_corpus_obeys_all_capture_bounds() {
    settled_artifact_fixture(true);
}

fn settled_artifact_fixture(combined: bool) {
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
                "combined_corpus":combined,
                "baseline_commit":BASELINE_COMMIT,
                "baseline_tree":BASELINE_TREE
            },
            "source":{"compiled_sources_sha256":&pins,"digest":digest(&serde_json::to_vec(&pins).unwrap()),
                "binary_sha256":digest(b"synthetic-artifact-binary")},
            "controller_resources":{"pid":std::process::id()},
            "metrics_required_for_outcome":true,
            "phase_metrics":{"boundaries":[]}
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
    if combined {
        for (index, (phase, boundary)) in [
            ("worker_setup", "after_ready"),
            ("online_namespace", "before"),
            ("online_namespace", "after"),
            ("online_payload", "before"),
            ("online_payload", "after"),
            ("initial_fresh_oracle", "before"),
            ("initial_fresh_oracle", "after"),
            ("refresh_replicas", "before"),
            ("refresh_replicas", "after_ready"),
            ("routes_and_scope", "before"),
            ("routes_and_scope", "after"),
            ("assigned_warmup", "before"),
            ("assigned_warmup", "after"),
        ]
        .into_iter()
        .enumerate()
        {
            synthetic_metric_boundary(&mut journal, phase, boundary, index + 1);
        }
    } else {
        synthetic_metric_boundary(&mut journal, "initial_fresh_oracle", "after", 2);
    }
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
            let phase = format!("{mode}/{pattern}");
            synthetic_metric_boundary(&mut journal, &phase, "before_active", sequence);
            synthetic_metric_boundary(&mut journal, &phase, "after_active", sequence + 1);
            let identity =
                synthetic_metric_boundary(&mut journal, &phase, "after_idle", sequence + 2);
            let quic_boundary = journal
                .publish_quic_boundary(&identity, mode, pattern, &network, DRIVES)
                .unwrap();
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
                "controller_quic_boundary":quic_boundary,
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
    if combined {
        for rotation in 1..=SERVERS {
            let sequence = 69 + (rotation - 1) * 2;
            synthetic_metric_boundary_generation(
                &mut journal,
                "crossnode_routes",
                "after_ready",
                sequence,
                rotation + 1,
            );
            synthetic_metric_boundary_generation(
                &mut journal,
                "crossnode_routes",
                "after_batch",
                sequence + 1,
                rotation + 1,
            );
        }
        synthetic_metric_boundary_generation(&mut journal, "final_fresh_oracle", "before", 99, 11);
        synthetic_metric_boundary_generation(&mut journal, "final_fresh_oracle", "after", 100, 11);
        synthetic_metric_boundary_generation(&mut journal, "revocation", "before", 101, 11);
        synthetic_metric_boundary_generation(&mut journal, "revocation", "after", 102, 11);
        synthetic_metric_boundary_generation(&mut journal, "terminal", "terminal", 103, 11);
        publish_and_verify_combined_ledgers(&mut journal);
    } else {
        synthetic_metric_boundary(&mut journal, "final_fresh_oracle", "after", 100);
    }
    // This is the actual settled terminal publisher with immutable per-cell references.
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
    quic_artifacts::verify_stages(directory.path(), &terminal).unwrap();
    let stages = terminal["stages"].as_array().unwrap();
    let mut shard_total_bytes = 0_u64;
    let mut largest_shard_bytes = 0_u64;
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
        assert!(
            stage["controller_quic_boundary"]
                .get("connections")
                .is_none()
        );
        let name = format!("metrics/g1-s{}.json.gz", sequence + 2);
        let metric = metrics::read_compressed(&directory.path().join(name)).unwrap();
        let shard =
            quic_artifacts::read_boundary(directory.path(), stage, &metric["identity"], DRIVES)
                .unwrap();
        let name = stage["controller_quic_boundary"]["artifact"]["file"]
            .as_str()
            .unwrap();
        let shard_metadata = std::fs::symlink_metadata(directory.path().join(name)).unwrap();
        assert!(shard_metadata.is_file());
        assert!(shard_metadata.len() <= CAPTURE_REGULAR_FILE_LIMIT);
        shard_total_bytes = shard_total_bytes.checked_add(shard_metadata.len()).unwrap();
        largest_shard_bytes = largest_shard_bytes.max(shard_metadata.len());
        let rows = shard["connections"].as_array().unwrap();
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
            "shards":CELLS,"largest_shard_bytes":largest_shard_bytes,"shard_total_bytes":shard_total_bytes,
            "terminal_bytes":metadata.len(),
            "terminal_sha256":stream_digest(&terminal_path),
            "regular_file_limit_bytes":CAPTURE_REGULAR_FILE_LIMIT,
            "baseline_commit":BASELINE_COMMIT,
            "baseline_tree":BASELINE_TREE,
            "compiled_sources_sha256":pins,
            "scope":"synthetic artifact serialization only; no native/provider capacity result"
        })
    );
    if combined {
        verify_combined_ledger_inventory(directory.path(), &terminal);
        verify_combined_synthetic_metrics(directory.path(), &terminal);
        verify_synthetic_oracle_receipts(directory.path(), &terminal);
        let settled = settled_capture_census(directory.path()).unwrap();
        assert_eq!(settled.files, 313 + 16 + 86 * 11 + 2 + 1 + 1);
        println!(
            "COMBINED_ARTIFACT_CORPUS_MEASUREMENT {}",
            json!({
                "drives":DRIVES,"partitions":DRIVES / 2,"files":10_000_000,
                "dense_generation_slots":15_340_000,"sparse_generation_entries":DRIVES,
                "ledger_packs":313,"quic_cells":CELLS,"quic_rows":CELLS * DRIVES,
                "synthetic_metric_boundaries":86,"synthetic_metric_receipts":86 * 11,
                "regular_files":settled.files,"entries_including_root":settled.entries,
                "stored_bytes":settled.bytes,"largest_regular_file_bytes":settled.largest,
                "regular_file_limit_bytes":CAPTURE_REGULAR_FILE_LIMIT,
                "aggregate_limit_bytes":CAPTURE_TOTAL_LIMIT,"regular_file_count_limit":CAPTURE_FILE_COUNT_LIMIT,
                "entry_limit":CAPTURE_ENTRY_LIMIT,"expected_state_deadline_seconds":30,
                "scope":"complete synthetic artifact corpus only; synthetic oracle receipts are not native verification; production metric payload sizes remain unqualified"
            })
        );
    }
    // Preserve the original capture bound after complete retained-row validation.
    assert!(
        metadata.len() <= CAPTURE_REGULAR_FILE_LIMIT,
        "TERMINAL_CAPTURE_REGULAR_FILE_LIMIT bytes={} limit={}",
        metadata.len(),
        CAPTURE_REGULAR_FILE_LIMIT
    );
}

const CAPTURE_TOTAL_LIMIT: u64 = 128 * 1024 * 1024;
const CAPTURE_FILE_COUNT_LIMIT: usize = 4096;
const CAPTURE_ENTRY_LIMIT: usize = 8192;

#[derive(Default)]
struct CaptureCensus {
    files: usize,
    entries: usize,
    bytes: u64,
    largest: u64,
}
#[derive(Clone, Copy)]
enum CaptureKind {
    Directory,
    Regular(u64),
    Other,
}
impl CaptureCensus {
    fn admit(&mut self, kind: CaptureKind) -> Result<(), &'static str> {
        self.entries = self.entries.checked_add(1).ok_or("entry count overflow")?;
        if self.entries > CAPTURE_ENTRY_LIMIT {
            return Err("capture entry limit");
        }
        match kind {
            CaptureKind::Directory => Ok(()),
            CaptureKind::Other => Err("symlink or nonregular capture entry"),
            CaptureKind::Regular(bytes) => {
                if bytes > CAPTURE_REGULAR_FILE_LIMIT {
                    return Err("capture regular file limit");
                }
                self.files = self.files.checked_add(1).ok_or("file count overflow")?;
                if self.files > CAPTURE_FILE_COUNT_LIMIT {
                    return Err("capture file count limit");
                }
                self.bytes = self
                    .bytes
                    .checked_add(bytes)
                    .ok_or("stored byte sum overflow")?;
                if self.bytes > CAPTURE_TOTAL_LIMIT {
                    return Err("capture total stored byte limit");
                }
                self.largest = self.largest.max(bytes);
                Ok(())
            }
        }
    }
}

fn settled_capture_census(root: &Path) -> Result<CaptureCensus, String> {
    if !std::fs::symlink_metadata(root)
        .map_err(|error| error.to_string())?
        .is_dir()
    {
        return Err("capture root is not an owned directory".into());
    }
    let mut census = CaptureCensus::default();
    census.admit(CaptureKind::Directory)?; // Count the root conservatively too.
    let mut directories = vec![root.to_owned()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            let kind = if metadata.is_dir() {
                CaptureKind::Directory
            } else if metadata.is_file() {
                CaptureKind::Regular(metadata.len())
            } else {
                CaptureKind::Other
            };
            census.admit(kind)?;
            if matches!(kind, CaptureKind::Directory) {
                directories.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("pending"))
            {
                return Err("unsettled pending artifact remains".into());
            }
        }
    }
    Ok(census)
}

#[test]
fn combined_capture_census_rejects_each_original_bound_and_special_entries() {
    let mut exact = CaptureCensus::default();
    for _ in 0..8 {
        exact
            .admit(CaptureKind::Regular(CAPTURE_REGULAR_FILE_LIMIT))
            .unwrap();
    }
    assert_eq!(exact.bytes, CAPTURE_TOTAL_LIMIT);
    assert!(exact.admit(CaptureKind::Regular(1)).is_err());
    assert!(
        CaptureCensus::default()
            .admit(CaptureKind::Regular(CAPTURE_REGULAR_FILE_LIMIT + 1))
            .is_err()
    );
    let mut files = CaptureCensus {
        files: CAPTURE_FILE_COUNT_LIMIT,
        ..CaptureCensus::default()
    };
    assert!(files.admit(CaptureKind::Regular(0)).is_err());
    let mut entries = CaptureCensus {
        entries: CAPTURE_ENTRY_LIMIT,
        ..CaptureCensus::default()
    };
    assert!(entries.admit(CaptureKind::Directory).is_err());
    assert!(CaptureCensus::default().admit(CaptureKind::Other).is_err());
}

#[cfg(unix)]
#[test]
fn combined_settled_census_rejects_actual_symlink_and_nonregular_entries() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let link_directory = tempfile::tempdir().unwrap();
    // Even a dangling link is rejected without following it.
    symlink("absent", link_directory.path().join("foreign-link")).unwrap();
    assert!(settled_capture_census(link_directory.path()).is_err());
    let special_directory = tempfile::tempdir().unwrap();
    let _listener = UnixListener::bind(special_directory.path().join("special")).unwrap();
    assert!(settled_capture_census(special_directory.path()).is_err());
}

fn full_slot_generation(drive: usize, slot: usize) -> u64 {
    if slot.is_multiple_of(257) {
        u64::MAX
    } else {
        (1_u64 << 63) | ((drive as u64) << 20) | slot as u64
    }
}

fn full_expected_drive(drive: usize) -> state::Expected {
    let mut expected = state::Expected::empty(drive, 1000);
    let mut slot = 0;
    for identity in 0..1000 {
        let name = format!("mixed-{identity}");
        expected.create(name.clone(), identity);
        for block in 0..fixture::FileProfile::Mixed.size(identity) / 4096 {
            expected.write(&name, block, full_slot_generation(drive, slot));
            slot += 1;
        }
    }
    assert_eq!(slot, 1534);
    expected.write("mixed-0", 1, u64::MAX); // Exact sparse appended generation too.
    expected.rename("mixed-1", "renamed-é".into());
    expected
}

fn publish_and_verify_combined_ledgers(journal: &mut Journal) {
    // The original observation starts before initialization publication. All
    // packs share this one deadline; construction/readback are inside it too.
    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
    let initialization_path = journal.output.join("initialization-receipts.json");
    let initialization = (0..DRIVES)
        .map(|drive| {
            json!({
                "drive":drive,"partition":drive / 2,"synthetic_artifact":true
            })
        })
        .collect::<Vec<_>>();
    write_json(&initialization_path, &json!(initialization)).unwrap();
    journal.value["initialization_receipt_file"] = json!({
        "path":"initialization-receipts.json","sha256":file_digest(&initialization_path).unwrap(),"drives":DRIVES
    });
    std::fs::create_dir(journal.output.join("expected")).unwrap();
    let mut receipts = Vec::new();
    let mut drives = 0usize;
    let mut files = 0usize;
    let mut slots = 0usize;
    let mut sparse = 0usize;
    let mut bytes = 0u64;
    for pack in 0..DRIVES.div_ceil(artifacts::PACK_DRIVES) {
        assert!(
            Instant::now() <= deadline,
            "single original expected-state deadline before pack"
        );
        let first = pack * artifacts::PACK_DRIVES;
        let expected = (first..(first + artifacts::PACK_DRIVES).min(DRIVES))
            .map(full_expected_drive)
            .collect::<Vec<_>>();
        let borrowed = expected.iter().collect::<Vec<_>>();
        let receipt = artifacts::publish_pack(&journal.output, pack, &borrowed, deadline).unwrap();
        let path = journal.output.join(&receipt.file);
        assert_eq!(stream_digest(&path), receipt.sha256);
        let decoded = metrics::read_compressed(&path).unwrap();
        assert_eq!(decoded.as_object().unwrap().len(), 4);
        assert_eq!(decoded["schema"], artifacts::PACK_SCHEMA);
        assert_eq!(decoded["pack"], pack);
        assert_eq!(decoded["first_drive"], first);
        let ledgers = decoded["ledgers"].as_array().unwrap();
        assert_eq!(ledgers.len(), expected.len());
        for (offset, ledger) in ledgers.iter().enumerate() {
            let drive = first + offset;
            assert_eq!(drive, drives);
            assert_eq!(ledger.as_object().unwrap().len(), 4);
            assert_eq!(ledger["drive"], drive);
            assert_eq!(
                ledger["oracle"],
                "tuple-seeded4096-byte blocks; initial generation0"
            );
            let generations = ledger["generations"].as_array().unwrap();
            assert_eq!(generations.len(), 1534);
            for (slot, value) in generations.iter().enumerate() {
                assert_eq!(value.as_u64(), Some(full_slot_generation(drive, slot)));
                slots += 1;
            }
            let entries = ledger["files"].as_object().unwrap();
            assert_eq!(entries.len(), 1000);
            for identity in 0..1000 {
                let name = if identity == 1 {
                    "renamed-é".to_owned()
                } else {
                    format!("mixed-{identity}")
                };
                let file = &entries[&name];
                let length =
                    fixture::FileProfile::Mixed.size(identity) + usize::from(identity == 0) * 4096;
                assert_eq!(file.as_object().unwrap().len(), 3);
                assert_eq!(file["identity"].as_u64(), Some(identity as u64));
                assert_eq!(file["length"].as_u64(), Some(length as u64));
                let changed = file["changed"].as_object().unwrap();
                assert_eq!(changed.len(), usize::from(identity == 0));
                if identity == 0 {
                    assert_eq!(changed["1"].as_u64(), Some(u64::MAX));
                    sparse += 1;
                }
                files += 1;
                bytes = bytes.checked_add(length as u64).unwrap();
            }
            drives += 1;
        }
        receipts.push(receipt);
        assert!(
            Instant::now() <= deadline,
            "single original expected-state deadline after write/hash/readback"
        );
        // decoded, expected and the <=32 borrowed views drop before next pack.
    }
    assert_eq!(
        (drives, files, slots, sparse),
        (DRIVES, 10_000_000, 15_340_000, DRIVES)
    );
    assert_eq!(bytes, 62_832_640_000 + DRIVES as u64 * 4096);
    assert_eq!(receipts.len(), 313);
    let elapsed = started.elapsed();
    assert!(elapsed <= Duration::from_secs(30));
    journal.value["expected_state_observation"] = json!({
        "complete":true,"elapsed_seconds":elapsed.as_secs_f64(),"deadline_seconds":30,
        "scope":"synthetic fixture construction plus actual publication/hash/readback under one original observation deadline"
    });
    journal.value["expected_state_receipts"] = json!({
        "schema":artifacts::INVENTORY_SCHEMA,"drive_count":DRIVES,"pack_size":artifacts::PACK_DRIVES,"packs":receipts
    });
    journal.value["synthetic_oracle_receipts"] = json!([]);
    std::fs::create_dir(journal.output.join("oracle-receipts")).unwrap();
    for label in ["initial", "final"] {
        let final_pass = label == "final";
        let count_bytes = if final_pass { bytes } else { 62_832_640_000 };
        let raw = json!({
            "pass":label,"slot_limit":8,"expected_drives":DRIVES,"started_drives":DRIVES,"completed_drives":DRIVES,
            "live_slots":0,"expected_files":10_000_000,"completed_files":10_000_000,"checked_files":10_000_000,
            "expected_bytes":count_bytes,"completed_bytes":count_bytes,"compared_bytes":count_bytes,
            "complete":true,"settled":true,"completed_drive_ids":(0..DRIVES).collect::<Vec<_>>()
        });
        let name = format!("oracle-receipts/{label}.json");
        let path = journal.output.join(&name);
        write_json(&path, &raw).unwrap();
        journal.value["synthetic_oracle_receipts"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "pass":label,"file":name,"sha256":file_digest(&path).unwrap(),
                "scope":"synthetic complete roster and totals only; no native oracle execution"
            }));
    }
}

fn verify_synthetic_oracle_receipts(root: &Path, terminal: &Value) {
    assert_eq!(terminal["synthetic_artifact"]["combined_corpus"], true);
    assert!(terminal.get("fresh_oracle_complete").is_none());
    let receipts = terminal["synthetic_oracle_receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 2);
    for (index, label) in ["initial", "final"].into_iter().enumerate() {
        let receipt = &receipts[index];
        assert_eq!(receipt["pass"], label);
        let name = format!("oracle-receipts/{label}.json");
        assert_eq!(receipt["file"], name);
        let path = root.join(name);
        assert_eq!(stream_digest(&path), receipt["sha256"].as_str().unwrap());
        let raw = read_json(&path).unwrap();
        assert_eq!(raw.as_object().unwrap().len(), 15);
        assert_eq!(raw["pass"], label);
        assert_eq!(raw["slot_limit"], 8);
        assert_eq!(raw["live_slots"], 0);
        assert_eq!(raw["complete"], true);
        assert_eq!(raw["settled"], true);
        let roster = raw["completed_drive_ids"].as_array().unwrap();
        assert_eq!(roster.len(), DRIVES);
        for (drive, value) in roster.iter().enumerate() {
            assert_eq!(value.as_u64(), Some(drive as u64));
        }
        for field in ["expected_drives", "started_drives", "completed_drives"] {
            assert_eq!(raw[field], DRIVES);
        }
        for field in ["expected_files", "completed_files", "checked_files"] {
            assert_eq!(raw[field], 10_000_000);
        }
        let bytes = 62_832_640_000_u64 + if index == 1 { DRIVES as u64 * 4096 } else { 0 };
        for field in ["expected_bytes", "completed_bytes", "compared_bytes"] {
            assert_eq!(raw[field].as_u64(), Some(bytes));
        }
        let after = terminal["phase_metrics"]["boundaries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                row["phase"] == format!("{label}_fresh_oracle") && row["boundary"] == "after"
            })
            .collect::<Vec<_>>();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0]["complete"], true);
        assert_eq!(after[0]["metrics_complete"], true);
    }
}

fn verify_combined_synthetic_metrics(root: &Path, terminal: &Value) {
    let boundaries = terminal["phase_metrics"]["boundaries"].as_array().unwrap();
    assert_eq!(boundaries.len(), 86); // 13 setup + 48 timed + 20 rotations + 4 final + 1 terminal.
    let mut identities = std::collections::BTreeSet::new();
    let mut frames = 0usize;
    for row in boundaries {
        assert_eq!(row["complete"], true);
        assert_eq!(row["metrics_complete"], true);
        let generation = row["generation"].as_u64().unwrap();
        let sequence = row["sequence"].as_u64().unwrap();
        assert!(identities.insert((generation, sequence)));
        let controller_name = format!("metrics/g{generation}-s{sequence}.json.gz");
        assert_eq!(row["controller"]["file"], controller_name);
        let controller_path = root.join(controller_name);
        assert_eq!(
            stream_digest(&controller_path),
            row["controller"]["sha256"].as_str().unwrap()
        );
        let controller = metrics::read_compressed(&controller_path).unwrap();
        let expected_identity = json!({
            "pid":terminal["controller_resources"]["pid"],"controller_pid":terminal["controller_resources"]["pid"],
            "role":"controller","server":null,"generation":generation,"sequence":sequence,
            "phase":row["phase"],"boundary":row["boundary"],"source_digest":terminal["source"]["digest"],
            "binary_digest":terminal["source"]["binary_sha256"],"catalog_digest":digest(b"synthetic-artifact-catalog"),
            "backend_prefix":"synthetic-artifact-backing"
        });
        assert_eq!(controller["identity"], expected_identity);
        assert_eq!(controller["capture_complete"], true);
        assert_eq!(controller["metrics_complete"], true);
        frames += 1;
        let workers = row["workers"].as_array().unwrap();
        assert_eq!(workers.len(), SERVERS);
        for (server, receipt) in workers.iter().enumerate() {
            let pid = 1_000_000 + server;
            assert_eq!(receipt["server"], server);
            assert_eq!(receipt["pid"], pid);
            let name = format!("worker-{server}/metrics/g{generation}-s{sequence}.json.gz");
            assert_eq!(receipt["file"], name);
            let path = root.join(name);
            assert_eq!(stream_digest(&path), receipt["sha256"].as_str().unwrap());
            let frame = metrics::read_compressed(&path).unwrap();
            let mut expected = expected_identity.clone();
            expected["pid"] = json!(pid);
            expected["server"] = json!(server);
            expected["role"] = json!("worker");
            assert_eq!(frame["identity"], expected);
            assert_eq!(frame["capture_complete"], true);
            assert_eq!(frame["metrics_complete"], true);
            assert_eq!(frame["synthetic_artifact"], true);
            frames += 1;
        }
    }
    assert_eq!(frames, 946);
}

fn verify_combined_ledger_inventory(root: &Path, terminal: &Value) {
    assert_eq!(terminal["expected_state_observation"]["complete"], true);
    assert_eq!(
        terminal["expected_state_observation"]["deadline_seconds"],
        30
    );
    assert!(
        terminal["expected_state_observation"]["elapsed_seconds"]
            .as_f64()
            .unwrap()
            <= 30.0
    );
    let inventory = &terminal["expected_state_receipts"];
    assert_eq!(inventory.as_object().unwrap().len(), 4);
    assert_eq!(inventory["schema"], artifacts::INVENTORY_SCHEMA);
    assert_eq!(inventory["drive_count"], DRIVES);
    assert_eq!(inventory["pack_size"], 32);
    let packs = inventory["packs"].as_array().unwrap();
    assert_eq!(packs.len(), 313);
    for (pack, receipt) in packs.iter().enumerate() {
        let first = pack * 32;
        assert_eq!(receipt.as_object().unwrap().len(), 5);
        assert_eq!(receipt["pack"], pack);
        assert_eq!(receipt["first_drive"], first);
        assert_eq!(receipt["count"], (DRIVES - first).min(32));
        let name = format!("expected/pack-{pack:05}.json.gz");
        assert_eq!(receipt["file"], name);
        assert_eq!(
            stream_digest(&root.join(name)),
            receipt["sha256"].as_str().unwrap()
        );
    }
    let initialization = &terminal["initialization_receipt_file"];
    assert_eq!(initialization["path"], "initialization-receipts.json");
    assert_eq!(initialization["drives"], DRIVES);
    let path = root.join("initialization-receipts.json");
    assert_eq!(
        stream_digest(&path),
        initialization["sha256"].as_str().unwrap()
    );
    let decoded = read_json(&path).unwrap();
    let rows = decoded.as_array().unwrap();
    assert_eq!(rows.len(), DRIVES);
    for (drive, row) in rows.iter().enumerate() {
        assert_eq!(row["drive"], drive);
        assert_eq!(row["partition"], drive / 2);
        assert_eq!(row["synthetic_artifact"], true);
    }
}
