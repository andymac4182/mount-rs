#[test]
fn measurement_oracles() {
    assert!(timeout_message(false).contains("read request timeout"));
    assert!(!timeout_message(false).contains("uncertain"));
    assert!(!timeout_message(false).contains("write"));
    assert!(timeout_message(true).contains("write outcome uncertain"));
    assert!(timeout_message(true).contains("never replayed"));
    assert!(validate_depths(&[1, 2, 4, 8]).is_ok());
    assert!(validate_depths(&[0]).is_err());
    assert!(validate_depths(&[33]).is_err());
    assert!(validate_depths(&[1, 1]).is_err());
    assert!(valid_read(2).is_err());
    assert!(valid_write(4095).is_err());
    assert_ne!(payload(0, 0, 1), payload(0, 1, 1));
    let mut h = Histogram::default();
    h.record(std::time::Duration::from_micros(100));
    h.record(std::time::Duration::from_micros(200));
    assert_eq!(h.count, 2);
    assert!(h.percentile(99) >= 200);
}

#[test]
fn read_content_accepts_exact_payload() {
    let expected = payload(2, 3, 7);
    assert!(valid_read_content(&expected, &expected).is_ok());
}
#[test]
fn read_content_rejects_invalid_lengths() {
    let expected = payload(2, 3, 7);
    for received in [&[][..], &expected[..BYTES - 1], &vec![0; BYTES + 1][..]] {
        assert!(valid_read_content(received, &expected).is_err());
    }
    assert!(valid_read_content(&expected[..BYTES - 1], &expected[..BYTES - 1]).is_err());
    let oversized = vec![0; BYTES + 1];
    assert!(valid_read_content(&oversized, &oversized).is_err());
}
#[test]
fn read_content_compares_every_byte_and_payload_marker() {
    let expected = payload(2, 3, 7);
    for index in 0..BYTES {
        let mut received = expected.clone();
        received[index] ^= 1;
        assert!(valid_read_content(&received, &expected).is_err());
    }
    for received in [payload(4, 3, 7), payload(2, 4, 7), payload(2, 3, 8)] {
        assert!(valid_read_content(&received, &expected).is_err());
    }
}

#[test]
fn reused_payload_buffer_preserves_original_pattern_and_marker() {
    let mut bytes = vec![0; BYTES];
    for (client, lane, sequence) in [(0, 0, 0), (2, 3, 7), (99, 31, 1_310_000_001)] {
        payload_into(&mut bytes, client, lane, sequence);
        assert_eq!(bytes, payload(client, lane, sequence));
    }
}

#[test]
fn payload_generation_finishes_before_operation_latency_starts() {
    let mut bytes = vec![0; BYTES];
    let mut prepared_at = None;
    let start = start_after_payload(|| {
        payload_into(&mut bytes, 2, 5, 7);
        prepared_at = Some(Instant::now());
    });
    assert_eq!(bytes, payload(2, 5, 7));
    assert!(start >= prepared_at.unwrap());
}

#[cfg(all(feature = "resource-profiling", unix))]
#[path = "support/resource_profile.rs"]
mod resource_profile;
#[path = "support/tidb_wire.rs"]
mod wire;
use mount_rs_core::Loopback;
use mount_rs_remote_protocol::{OperationName, binary::IoRequest};
#[path = "support/saturation_backend.rs"]
mod backend;
#[cfg(unix)]
#[allow(dead_code)]
#[path = "support/production_target/command.rs"]
mod command;
#[allow(dead_code)]
#[path = "support/remote_blocks.rs"]
mod remote_blocks;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const DEFAULT_CLIENTS: usize = 100;
const DEFAULT_SERVERS: usize = 10;
const BYTES: usize = 4096;
fn validate_depths(depths: &[usize]) -> Result<(), String> {
    if depths.is_empty()
        || depths.len() > 6
        || depths.iter().any(|d| !(1..=32).contains(d))
        || depths.iter().copied().collect::<BTreeSet<_>>().len() != depths.len()
    {
        return Err("depths require 1..6 distinct integers in 1..32".into());
    }
    Ok(())
}
fn valid_read(count: usize) -> Result<(), String> {
    if count == BYTES {
        Ok(())
    } else {
        Err("partial or invalid read".into())
    }
}
fn valid_read_content(received: &[u8], expected: &[u8]) -> Result<(), String> {
    valid_read(received.len())?;
    if expected.len() != BYTES {
        return Err("partial or invalid expected payload".into());
    }
    if received != expected {
        return Err("read content mismatch".into());
    }
    Ok(())
}
fn valid_write(count: usize) -> Result<(), String> {
    if count == BYTES {
        Ok(())
    } else {
        Err("partial or invalid write".into())
    }
}
// Numeric arrays are a diagnostic lane on the current v2 control envelope.
// This mode is test-only and does not add production codec compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Binary,
    NumericJson,
}
impl Codec {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "binary" => Ok(Self::Binary),
            "numeric-json" => Ok(Self::NumericJson),
            _ => Err("codec requires binary or numeric-json".into()),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::NumericJson => "numeric-json",
        }
    }
}
enum IoReply {
    Count(usize),
    Numeric(Value),
}
fn valid_numeric_read(received: &Value, expected: &[u8]) -> Result<(), String> {
    let values = received
        .as_array()
        .filter(|v| v.len() == BYTES && expected.len() == BYTES)
        .ok_or("partial or invalid read")?;
    if values
        .iter()
        .zip(expected)
        .all(|(value, expected)| value.as_u64() == Some(u64::from(*expected)))
    {
        Ok(())
    } else {
        Err("read content mismatch".into())
    }
}
fn valid_numeric_write(received: &Value) -> Result<(), String> {
    if received.as_u64() == Some(BYTES as u64) {
        Ok(())
    } else {
        Err("partial or invalid write".into())
    }
}

const RUNNER_SOURCE_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/quic_tidb_saturation.rs");
const BACKEND_SOURCE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/support/saturation_backend.rs"
);

fn runner_source_digest(runner: &[u8], backend: &[u8]) -> String {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(b"mount-rs-saturation-runner-sources-v1\0");
    for (name, bytes) in [
        (b"quic_tidb_saturation.rs".as_slice(), runner),
        (b"support/saturation_backend.rs".as_slice(), backend),
    ] {
        digest.update(&(name.len() as u64).to_be_bytes());
        digest.update(name);
        digest.update(&(bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn require_matching_runner_source(compiled: &str, current: &str) -> Result<(), String> {
    if compiled == current {
        Ok(())
    } else {
        Err("runner source bytes changed since executable compilation".into())
    }
}

fn source_binary_receipt() -> Result<Value, String> {
    use std::io::Read;
    let compiled_source_sha256 = runner_source_digest(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/quic_tidb_saturation.rs"
        )),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/saturation_backend.rs"
        )),
    );
    let current_source_sha256 = runner_source_digest(
        &std::fs::read(RUNNER_SOURCE_PATH).map_err(|_| "runner source read failed")?,
        &std::fs::read(BACKEND_SOURCE_PATH).map_err(|_| "backend source read failed")?,
    );
    require_matching_runner_source(&compiled_source_sha256, &current_source_sha256)?;
    let mut supplemental_source_sha256 = serde_json::Map::new();
    for (name, compiled) in [
        (
            "support/remote_blocks.rs",
            include_bytes!("support/remote_blocks.rs").as_slice(),
        ),
        (
            "support/production_target/command.rs",
            include_bytes!("support/production_target/command.rs").as_slice(),
        ),
    ] {
        let actual = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join(name),
        )
        .map_err(|_| "supplemental runner source read failed")?;
        if actual != compiled {
            return Err("supplemental runner source changed since executable compilation".into());
        }
        let digest: String = ring::digest::digest(&ring::digest::SHA256, compiled)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        supplemental_source_sha256.insert(name.into(), json!(digest));
    }
    let executable = std::env::current_exe().map_err(|_| "executable path unavailable")?;
    let mut input = std::fs::File::open(&executable).map_err(|_| "executable read failed")?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|_| "executable digest read failed")?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let executable_sha256: String = digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let source = std::process::Command::new("git")
        .args(["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"])
        .output()
        .map_err(|_| "source revision probe failed")?;
    if !source.status.success() {
        return Err("source revision probe failed".into());
    }
    let checkout_revision = String::from_utf8(source.stdout)
        .map_err(|_| "source revision invalid")?
        .trim()
        .to_owned();
    if checkout_revision.len() != 40 {
        return Err("source revision invalid".into());
    }
    let status = std::process::Command::new("git")
        .args(["-C", env!("CARGO_MANIFEST_DIR"), "status", "--porcelain"])
        .output()
        .map_err(|_| "source status probe failed")?;
    if !status.status.success() {
        return Err("source status probe failed".into());
    }
    Ok(json!({
        "executable":executable,
        "executable_sha256":executable_sha256,
        "checkout_revision_at_run":checkout_revision,
        "checkout_dirty_at_run":!status.stdout.is_empty(),
        "compiled_revision_env":option_env!("MOUNT_RS_SATURATION_SOURCE_REVISION"),
        "compiled_runner_source_sha256":compiled_source_sha256,
        "current_runner_source_sha256":current_source_sha256,
        "supplemental_source_sha256":supplemental_source_sha256,
        "features":{"resource_profiling":cfg!(feature="resource-profiling"),"allocation_profiling":cfg!(feature="allocation-profiling"),"foundationdb":cfg!(feature="saturation-foundationdb")},
    }))
}

#[test]
fn source_receipt_binds_compiled_and_current_runner_bytes() {
    let receipt = source_binary_receipt().unwrap();
    let compiled = receipt["compiled_runner_source_sha256"].as_str().unwrap();
    let current = receipt["current_runner_source_sha256"].as_str().unwrap();
    assert_eq!(compiled.len(), 64);
    assert_eq!(compiled, current);
    assert!(require_matching_runner_source(compiled, "different").is_err());
}

#[test]
fn artifact_inode_options_match_effective_backend_mode() {
    for (mode, inode, compact) in [
        (backend::InodeMode::Legacy, false, false),
        (backend::InodeMode::Inode, true, false),
        (backend::InodeMode::Compact, true, true),
    ] {
        let fields = json!({
            "requested_inode_mode":mode.label(),
            "inode_updates":mode.inode_updates(),
            "compact_inode_updates":mode.compact_inode_updates(),
        });
        assert_eq!(fields["inode_updates"], inode);
        assert_eq!(fields["compact_inode_updates"], compact);
    }
}

#[test]
fn rejected_mode_receipt_or_owned_count_writes_failed_artifact() {
    let directory = tempfile::tempdir().unwrap();
    for (case, mode_error, count_error) in [
        ("mode", Some("persisted marker mismatch"), None),
        ("count", None, Some("owned SQL count failed")),
    ] {
        let path = directory.path().join(format!("{case}.json"));
        let mut artifact = json!({"schema":"mount-rs-provider-saturation-v2"});
        finalize_artifact_qualification(&mut artifact, 4, "passed", mode_error, count_error);
        write_saturation_artifact(&artifact, Some(&path), case).unwrap();
        for written in [&path, &directory.path().join(format!("{case}-{case}.json"))] {
            let observed: Value = serde_json::from_slice(&std::fs::read(written).unwrap()).unwrap();
            assert_eq!(observed["verification_status"], "failed", "{case}");
            assert_eq!(observed["verified_files"], 0, "{case}");
            assert_eq!(observed["file_verification_status"], "passed", "{case}");
            assert_eq!(observed["persisted_mode_receipt_error"], json!(mode_error));
            assert_eq!(
                observed["owned_logical_sql_counts_error"],
                json!(count_error)
            );
        }
    }
}

// Both banks use the existing drained-stage boundaries. Snapshots are relaxed,
// sequential observations, not an atomic cut of all background/server activity.
struct StageDiagnostics {
    core: mount_rs_core::diagnostics::profile::Snapshot,
    storage: Option<mount_rs_core::diagnostics::storage::Snapshot>,
}
impl StageDiagnostics {
    fn capture(storage_enabled: bool) -> Self {
        Self {
            core: mount_rs_core::diagnostics::profile::snapshot(),
            storage: storage_enabled.then(mount_rs_core::diagnostics::storage::snapshot),
        }
    }

    fn finish_into(self, report: &mut Value) -> Result<(), String> {
        let core_after = mount_rs_core::diagnostics::profile::snapshot();
        report["io_profile"] = serde_json::to_value(core_after.delta(&self.core)?)
            .map_err(|_| "profile encode failed")?;
        let observed = match self.storage {
            Some(before) => {
                let after = mount_rs_core::diagnostics::storage::snapshot();
                // Fail on changed identity/shape or a reset; never fabricate a
                // zero delta or repeat a snapshot to hide a pending operation.
                let delta = after.delta(&before)?;
                let pending = [&before, &after].iter().any(|snapshot| {
                    snapshot.in_flight != 0 || snapshot.entries.iter().any(|row| row.in_flight != 0)
                });
                let consistent = [&before, &after].iter().all(|snapshot| {
                    snapshot.entries.iter().all(|row| {
                        row.success
                            .checked_add(row.error)
                            .and_then(|n| n.checked_add(row.cancelled))
                            == Some(row.calls)
                            && row
                                .latency_log2_us
                                .iter()
                                .try_fold(0u64, |n, count| n.checked_add(*count))
                                == Some(row.calls)
                    })
                });
                json!({
                    "enabled":true,
                    "complete":!pending && consistent,
                    "status":if pending {"nonquiescent"} else if consistent {"complete"} else {"inconsistent"},
                    "before":before,"after":after,"delta":delta,
                })
            }
            None => {
                json!({"enabled":false,"complete":false,"status":"disabled","before":null,"after":null,"delta":null})
            }
        };
        let enabled = observed["enabled"] == true;
        report["storage_profile"] = json!({
            "schema":"mount-rs.remote-stage-storage.v1",
            "operations":mount_rs_core::diagnostics::storage::operation_names(),
            "measurement":{
                "calls":"completed_instrumented_method_or_stage_invocations; success_error_cancelled_are_distinct",
                "bytes":"known_successful_stage_specific_bytes; payload_or_plaintext_envelope_as_declared_by_family; zero_does_not_establish_no_payload",
                "known_byte_families":{"direct_sdk_blocks":"successful_put_input_and_get_or_migration_payload_bytes; SDK_metadata_bytes_unavailable","tidb_sql":"known_successful_returned_query_bytes; not_base_datastore_IOPS","production_remote_client":"unavailable","peer_blob_cache":"not_configured"},
                "returned_rows":"known_successful_returned_SQL_rows; zero_observations_means_unavailable",
                "duration":"inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap",
                "latency_histogram":"32_log2_microsecond_buckets; bucket0_below1us; bucket_n_[2^(n-1),2^n)_us; final_bucket_includes_higher_latencies",
                "in_flight":"before_and_after_are_endpoint_gauges; delta_retains_after_gauge_without_subtraction",
                "forwarding_boxes":"unavailable; this_fixture_does_not_call_NAPI_forwarding_sites"
            },
            "coverage":{
                "direct_sdk_storage":{"configured":true,"available":enabled,"status":if enabled {"observed"}else{"disabled"}},
                "production_remote_client":{"available":false,"status":"unavailable","reason":"direct test wire bypasses production remote client"},
                "peer_blob_cache":{"available":false,"status":"not_configured","reason":"peer blob cache not configured"},
                "service_stage_observer":{"available":false,"status":"unavailable","reason":"fixture binds without service diagnostics observer"},
                "oidc_authentication":{"available":false,"status":"unavailable","reason":"synthetic fixture authenticator bypasses OIDC signature validation"}
            },
            "scope":"one process hosts all client and server coordinators; one shared Partition; one file per active client; shared or separate Drives as selected by the artifact; direct SDK method boundaries overlap core/provider wall; no independent server process, production topology or physical IOPS qualification",
            "boundary_scope":"stage workers drained; complete means consistent observed zero instrumented global and per-row boundary gauges only; relaxed sequential snapshots are not an atomic cut or proof of all server/provider/background quiescence"
        });
        let Value::Object(observed) = observed else {
            unreachable!()
        };
        report["storage_profile"]
            .as_object_mut()
            .unwrap()
            .extend(observed);
        Ok(())
    }
}

#[test]
#[ignore = "isolated enabled storage export behavioral gate"]
fn storage_stage_artifact_preserves_public_spans_and_disabled_coverage() {
    use mount_rs_core::diagnostics::{profile, storage};
    assert!(
        storage::enabled(),
        "explicit MOUNT_RS_PROFILE_IO=1 required"
    );
    assert!(profile::enabled());
    let actual_before = storage::snapshot();
    assert_eq!(actual_before.in_flight, 0);
    let boundary = StageDiagnostics::capture(true);
    let mut put = storage::Span::new(storage::Operation::SdkBlocksPut);
    put.finish_success(4096);
    let mut failed = storage::Span::new(storage::Operation::SdkMetadataFlush);
    failed.finish_error();
    drop(storage::Span::new(storage::Operation::SdkBlocksGet));
    let mut rows = storage::Span::new(storage::Operation::TidbSqlBlockRead);
    rows.finish_success_with_rows(17, 0);
    drop(profile::Span::new(profile::Event::BlockPut).units(4096));

    let actual = storage::snapshot().delta(&actual_before).unwrap();
    let real_row = |name: &str| actual.entries.iter().find(|row| row.name == name).unwrap();
    assert_eq!(real_row("sdk.blocks.put").success, 1);
    assert_eq!(real_row("sdk.blocks.put").bytes, 4096);
    assert_eq!(real_row("sdk.metadata.flush").error, 1);
    assert_eq!(real_row("sdk.blocks.get").cancelled, 1);
    assert_eq!(real_row("tidb.sql.block_read").returned_rows, 0);
    assert_eq!(real_row("tidb.sql.block_read").returned_row_observations, 1);
    assert_eq!(actual.in_flight, 0);

    let mut completed = json!({"case":"completed"});
    boundary.finish_into(&mut completed).unwrap();
    let pending_boundary = StageDiagnostics::capture(true);
    let pending = storage::Span::new(storage::Operation::SdkMetadataLoad);
    let pending_actual = storage::snapshot();
    assert_eq!(pending_actual.in_flight, 1);
    assert_eq!(
        pending_actual
            .entries
            .iter()
            .find(|row| row.name == "sdk.metadata.load")
            .unwrap()
            .in_flight,
        1
    );
    let mut nonquiescent = json!({"case":"nonquiescent"});
    pending_boundary.finish_into(&mut nonquiescent).unwrap();
    drop(pending);
    assert_eq!(storage::snapshot().in_flight, 0);

    let disabled_boundary = StageDiagnostics::capture(false);
    let mut disabled = json!({"case":"disabled"});
    disabled_boundary.finish_into(&mut disabled).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stage.json");
    let artifact = json!({"stages":[completed,nonquiescent,disabled]});
    write_saturation_artifact(&artifact, Some(&path), "storage-stage").unwrap();
    let observed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let retained: Value = serde_json::from_slice(
        &std::fs::read(directory.path().join("stage-storage-stage.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(observed, retained);
    let core_put = observed["stages"][0]["io_profile"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "provider.blocks.put_bytes")
        .unwrap();
    assert_eq!(core_put["calls"], 1);
    assert_eq!(core_put["units"], 4096);
    println!(
        "MOUNT_RS_STORAGE_STAGE_EXPORT behavior_oracles=complete public_success_error_cancel_verified=true known_zero_rows_verified=true pending_gauge_verified=true core_profile_preserved=true retained_artifact_equal=true"
    );

    let complete = &observed["stages"][0]["storage_profile"];
    assert!(
        complete.is_object(),
        "measured stage artifact omitted storage bank after public span and encoding oracles"
    );
    assert_eq!(complete["enabled"], true);
    assert_eq!(complete["complete"], true);
    assert_eq!(complete["status"], "complete");
    let names: Vec<_> = storage::operation_names()
        .iter()
        .copied()
        .map(Value::from)
        .collect();
    assert_eq!(complete["operations"], json!(names));
    assert_eq!(
        complete["measurement"]["bytes"],
        "known_successful_stage_specific_bytes; payload_or_plaintext_envelope_as_declared_by_family; zero_does_not_establish_no_payload"
    );
    assert_eq!(
        complete["measurement"]["returned_rows"],
        "known_successful_returned_SQL_rows; zero_observations_means_unavailable"
    );
    assert_eq!(
        complete["measurement"]["duration"],
        "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap"
    );
    let entries = complete["delta"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), names.len());
    let row = |name: &str| entries.iter().find(|row| row["name"] == name).unwrap();
    for (name, success, error, cancelled, bytes, returned_observations) in [
        ("sdk.blocks.put", 1, 0, 0, 4096, 0),
        ("sdk.metadata.flush", 0, 1, 0, 0, 0),
        ("sdk.blocks.get", 0, 0, 1, 0, 0),
        ("tidb.sql.block_read", 1, 0, 0, 17, 1),
    ] {
        assert_eq!(row(name)["calls"], 1);
        assert_eq!(row(name)["success"], success);
        assert_eq!(row(name)["error"], error);
        assert_eq!(row(name)["cancelled"], cancelled);
        assert_eq!(row(name)["bytes"], bytes);
        assert_eq!(row(name)["returned_rows"], 0);
        assert_eq!(
            row(name)["returned_row_observations"],
            returned_observations
        );
        assert_eq!(row(name)["in_flight"], 0);
        assert_eq!(
            row(name)["latency_log2_us"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_u64().unwrap())
                .sum::<u64>(),
            1
        );
    }
    assert_eq!(complete["before"]["in_flight"], 0);
    assert_eq!(complete["after"]["in_flight"], 0);
    assert_eq!(
        complete["coverage"]["production_remote_client"]["available"],
        false
    );
    assert_eq!(
        complete["coverage"]["production_remote_client"]["status"],
        "unavailable"
    );
    assert_eq!(
        complete["coverage"]["production_remote_client"]["reason"],
        "direct test wire bypasses production remote client"
    );
    assert_eq!(complete["coverage"]["peer_blob_cache"]["available"], false);
    assert_eq!(
        complete["coverage"]["peer_blob_cache"]["status"],
        "not_configured"
    );
    assert_eq!(
        complete["coverage"]["peer_blob_cache"]["reason"],
        "peer blob cache not configured"
    );
    assert_eq!(
        complete["coverage"]["service_stage_observer"]["available"],
        false
    );
    assert_eq!(
        complete["coverage"]["service_stage_observer"]["reason"],
        "fixture binds without service diagnostics observer"
    );
    assert_eq!(
        complete["coverage"]["oidc_authentication"]["available"],
        false
    );
    assert_eq!(
        complete["coverage"]["oidc_authentication"]["reason"],
        "synthetic fixture authenticator bypasses OIDC signature validation"
    );

    let pending = &observed["stages"][1]["storage_profile"];
    assert_eq!(pending["complete"], false);
    assert_eq!(pending["status"], "nonquiescent");
    assert_eq!(pending["before"]["in_flight"], 0);
    assert_eq!(pending["after"]["in_flight"], 1);
    assert_eq!(pending["delta"]["in_flight"], 1);
    assert_eq!(
        pending["after"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == "sdk.metadata.load")
            .unwrap()["in_flight"],
        1
    );
    let disabled = &observed["stages"][2]["storage_profile"];
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["complete"], false);
    assert_eq!(disabled["status"], "disabled");
    for field in ["before", "after", "delta"] {
        assert!(
            disabled[field].is_null(),
            "disabled storage cannot export observations"
        );
    }
}
#[test]
fn diagnostic_codec_selection_and_numeric_oracles_are_explicit() {
    assert_eq!(Codec::parse("binary").unwrap(), Codec::Binary);
    assert_eq!(Codec::parse("numeric-json").unwrap(), Codec::NumericJson);
    for invalid in ["", "v1", "json", "fallback"] {
        assert!(Codec::parse(invalid).is_err());
    }
    let expected = payload(2, 3, 7);
    assert!(valid_numeric_read(&json!(&expected), &expected).is_ok());
    assert!(valid_numeric_read(&json!(&expected[..BYTES - 1]), &expected).is_err());
    for invalid in [json!(-1), json!(256), json!(1.5), json!("1"), Value::Null] {
        let mut received = json!(&expected);
        received[BYTES - 1] = invalid;
        assert!(valid_numeric_read(&received, &expected).is_err());
    }
    assert!(valid_numeric_write(&json!(BYTES)).is_ok());
    assert!(valid_numeric_write(&json!(BYTES - 1)).is_err());
}

fn payload(client: usize, lane: usize, seq: u64) -> Vec<u8> {
    let mut bytes = vec![0; BYTES];
    let marker = format!("client={client};lane={lane};sequence={seq};");
    fill_payload_pattern(&mut bytes, client, lane, seq);
    bytes[..marker.len()].copy_from_slice(marker.as_bytes());
    bytes
}
fn fill_payload_pattern(bytes: &mut [u8], client: usize, lane: usize, seq: u64) {
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i as u64 * 31 + seq * 13 + client as u64 * 17 + lane as u64 * 19) as u8;
    }
}
fn payload_into(bytes: &mut [u8], client: usize, lane: usize, seq: u64) {
    use std::io::Write;
    assert_eq!(bytes.len(), BYTES);
    fill_payload_pattern(bytes, client, lane, seq);
    // Three decimal u64-sized identifiers and their labels fit in 128 bytes.
    let mut marker = [0_u8; 128];
    let mut cursor = std::io::Cursor::new(marker.as_mut_slice());
    write!(cursor, "client={client};lane={lane};sequence={seq};")
        .expect("payload marker fits in stack buffer");
    let marker_len = cursor.position() as usize;
    bytes[..marker_len].copy_from_slice(&marker[..marker_len]);
}

fn start_after_payload(prepare: impl FnOnce()) -> Instant {
    prepare();
    Instant::now()
}
// Fixed 64-bucket logarithmic histogram, upper-bound microseconds; constant memory per worker.
#[derive(Clone)]
struct Histogram {
    buckets: [u64; 64],
    count: u64,
}
impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: [0; 64],
            count: 0,
        }
    }
}
impl Histogram {
    fn record(&mut self, d: Duration) {
        let us = d.as_micros().max(1).min(u64::MAX as u128) as u64;
        let bucket = (64 - (us - 1).leading_zeros()) as usize;
        self.buckets[bucket.min(63)] += 1;
        self.count += 1;
    }
    fn merge(&mut self, other: &Self) {
        for (a, b) in self.buckets.iter_mut().zip(other.buckets) {
            *a += b;
        }
        self.count += other.count;
    }
    fn percentile(&self, p: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let rank = (self.count * p).div_ceil(100);
        let mut n = 0;
        for (i, b) in self.buckets.iter().enumerate() {
            n += b;
            if n >= rank {
                return 1u64 << i;
            }
        }
        u64::MAX
    }
    fn json(&self) -> Value {
        json!({"completed":self.count,"p50_us_upper":self.percentile(50),"p95_us_upper":self.percentile(95),"p99_us_upper":self.percentile(99)})
    }
}
#[derive(Clone, Copy, Debug)]
enum Mode {
    Read,
    Write,
    Mixed,
}
#[derive(Default)]
struct LaneResult {
    read: Histogram,
    write: Histogram,
    last: Vec<(usize, u64)>,
    errors: Vec<String>,
}
struct Client {
    drive: String,
    connection: quinn::Connection,
    handles: Vec<u64>,
}
fn env_num(name: &str, default: usize, min: usize, max: usize) -> usize {
    let n = std::env::var(name)
        .map(|v| v.parse().expect("integer setting required"))
        .unwrap_or(default);
    assert!(
        (min..=max).contains(&n),
        "{name} outside bounds {min}..={max}"
    );
    n
}
// File population and verification still use every block. Only timed lane
// positions are limited, so inode size can vary with a fixed blob working set.
fn selected_hot_blocks(
    value: Result<String, std::env::VarError>,
    file_blocks: usize,
    depths: &[usize],
) -> Result<usize, String> {
    let hot = match value {
        Err(std::env::VarError::NotPresent) => file_blocks,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("hot block selector must be Unicode decimal".into());
        }
        Ok(value) => {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("hot block selector must be unsigned decimal".into());
            }
            value
                .parse::<usize>()
                .map_err(|_| "hot block selector overflow")?
        }
    };
    let max_depth = depths.iter().copied().max().ok_or("no lane depths")?;
    if max_depth == 0 || depths.contains(&0) || hot < max_depth || hot > file_blocks {
        return Err("hot blocks must cover all lanes and fit the full file".into());
    }
    Ok(hot)
}

fn lane_positions(blocks: usize, lane: usize, depth: usize) -> Vec<usize> {
    assert!(depth > 0 && lane < depth && blocks >= depth);
    (lane..blocks).step_by(depth).collect()
}

fn validate_hot_ledger(
    ledger: &[(usize, usize, usize, u64)],
    expected: &[Vec<(usize, u64)>],
    hot_blocks: usize,
    depth: usize,
) -> Result<(), String> {
    for &(client, lane, block, _) in ledger {
        if depth == 0
            || client >= expected.len()
            || lane >= depth
            || block >= hot_blocks
            || block >= expected[client].len()
            || block % depth != lane
        {
            return Err("write ledger escaped its client/lane/hot block range".into());
        }
    }
    Ok(())
}

fn validate_cold_suffix(expected: &[Vec<(usize, u64)>], hot_blocks: usize) -> Result<(), String> {
    for blocks in expected {
        if hot_blocks > blocks.len()
            || blocks
                .iter()
                .enumerate()
                .skip(hot_blocks)
                .any(|(block, value)| *value != (0, block as u64 + 1))
        {
            return Err("cold suffix no longer matches the initial full file".into());
        }
    }
    Ok(())
}

#[test]
fn hot_block_selection_is_strict_and_preserves_default_file_range() {
    for blocks in [32, 128, 256, 1024] {
        assert_eq!(
            selected_hot_blocks(Err(std::env::VarError::NotPresent), blocks, &[1, 2, 8]).unwrap(),
            blocks
        );
        assert_eq!(
            selected_hot_blocks(Ok("32".into()), blocks, &[1, 8]).unwrap(),
            32
        );
    }
    for invalid in [
        "",
        "0",
        "-1",
        "+32",
        " 32",
        "32 ",
        "3.2",
        "３２",
        "257",
        "999999999999999999999999999999",
    ] {
        assert!(selected_hot_blocks(Ok(invalid.into()), 256, &[1]).is_err());
    }
    assert!(selected_hot_blocks(Ok("3".into()), 256, &[1, 4]).is_err());
    assert!(selected_hot_blocks(Ok("32".into()), 256, &[]).is_err());
    assert!(selected_hot_blocks(Ok("32".into()), 256, &[0, 1]).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        assert!(
            selected_hot_blocks(
                Err(std::env::VarError::NotUnicode(
                    std::ffi::OsString::from_vec(vec![0xff])
                )),
                256,
                &[1],
            )
            .is_err()
        );
    }
}

#[test]
fn hot_lane_positions_cover_disjoint_nonempty_ranges_without_touching_cold_blocks() {
    for hot in [1, 3, 5, 32, 128, 256] {
        for depth in 1..=hot.min(8) {
            let mut observed = BTreeSet::new();
            for lane in 0..depth {
                let positions = lane_positions(hot, lane, depth);
                assert!(!positions.is_empty());
                assert_eq!(positions, (lane..hot).step_by(depth).collect::<Vec<_>>());
                for block in positions {
                    assert!(block < hot && block % depth == lane && observed.insert(block));
                }
            }
            assert_eq!(observed, (0..hot).collect());
        }
    }
}

#[test]
fn hot_ledger_rejects_out_of_bounds_or_wrong_lane_before_expected_updates() {
    let expected = vec![vec![(0, 1); 256]];
    assert!(validate_hot_ledger(&[(0, 0, 0, 99), (0, 1, 31, 100)], &expected, 32, 2).is_ok());
    for invalid in [
        (1, 0, 0, 1),
        (0, 2, 0, 1),
        (0, 0, 32, 1),
        (0, 0, 31, 1),
        (0, 0, 256, 1),
    ] {
        assert!(validate_hot_ledger(&[invalid], &expected, 32, 2).is_err());
    }
    assert!(validate_hot_ledger(&[(0, 0, 0, 1)], &expected, 32, 0).is_err());
}

#[test]
fn hot_ledger_preserves_seeded_cold_suffix_and_refuses_a_changed_tail() {
    let mut expected = vec![
        (0..256)
            .map(|block| (0, block as u64 + 1))
            .collect::<Vec<_>>(),
    ];
    expected[0][31] = (0, 999);
    assert!(validate_cold_suffix(&expected, 32).is_ok());
    expected[0][255] = (0, 999);
    assert!(validate_cold_suffix(&expected, 32).is_err());
}

#[allow(clippy::too_many_arguments)] // Explicit worker inputs are test-only and immutable.
async fn lane(
    connection: quinn::Connection,
    drive: String,
    handle: u64,
    client: usize,
    lane: usize,
    depth: usize,
    blocks: usize,
    mode: Mode,
    codec: Codec,
    deadline: Instant,
    base: u64,
    timeout: Duration,
    mut expected: Vec<(usize, u64)>,
) -> LaneResult {
    let mut result = LaneResult::default();
    let mut seq = 0u64;
    let mut rng = (client as u64 + 1) * 7919 + (lane as u64 + 1) * 104729;
    let positions = lane_positions(blocks, lane, depth);
    let mut expected_bytes = vec![0; BYTES];
    let mut read_buffer = vec![0; BYTES];
    let mut request = IoRequest {
        drive_id: drive,
        handle,
        position: None,
    };
    while Instant::now() < deadline {
        seq += 1;
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let block = positions[rng as usize % positions.len()];
        let writing =
            matches!(mode, Mode::Write) || matches!(mode, Mode::Mixed) && seq.is_multiple_of(2);
        request.position = Some((block * BYTES) as u64);
        let start = start_after_payload(|| {
            let (payload_lane, payload_sequence) = if writing {
                (lane, base + seq)
            } else {
                expected[block]
            };
            payload_into(&mut expected_bytes, client, payload_lane, payload_sequence);
        });
        let response=tokio::time::timeout(timeout,async {
            match codec {
                Codec::Binary=>{
                    let count=if writing { wire::handle_write(&connection,base+seq,&request,&expected_bytes).await? }
                    else { wire::handle_read(&connection,base+seq,&request,&mut read_buffer).await? };
                    Ok(IoReply::Count(count))
                }
                Codec::NumericJson=>{
                    let (name,body)=if writing { (OperationName::HandleWrite,json!({"handle":handle,"position":request.position,"data":&expected_bytes})) }
                    else { (OperationName::HandleRead,json!({"handle":handle,"position":request.position,"length":BYTES})) };
                    wire::success(&connection,base+seq,&request.drive_id,name,body).await.map(IoReply::Numeric)
                }
            }
        }).await;
        match response {
            Ok(Ok(reply)) => {
                let valid = match (reply, writing) {
                    (IoReply::Count(count), true) => valid_write(count),
                    (IoReply::Count(count), false) => valid_read(count)
                        .and_then(|()| valid_read_content(&read_buffer, &expected_bytes)),
                    (IoReply::Numeric(value), true) => valid_numeric_write(&value),
                    (IoReply::Numeric(value), false) => valid_numeric_read(&value, &expected_bytes),
                };
                if let Err(e) = valid {
                    result.errors.push(e);
                    break;
                }
                if writing {
                    result.write.record(start.elapsed());
                    expected[block] = (lane, base + seq);
                    if let Some(p) = result.last.iter_mut().find(|(b, _)| *b == block) {
                        p.1 = base + seq;
                    } else {
                        result.last.push((block, base + seq));
                    }
                } else {
                    // Read content validation remains inside the latency scope.
                    result.read.record(start.elapsed());
                }
            }
            Ok(Err(e)) => {
                connection.close(1_u32.into(), b"I/O failed; never replayed");
                result.errors.push(e);
                break;
            }
            Err(_) => {
                connection.close(1_u32.into(), b"request timeout; never replayed");
                result.errors.push(timeout_message(writing).into());
                break;
            }
        }
    }
    result
}
#[allow(clippy::too_many_arguments)]
async fn stage(
    clients: &[Client],
    server_count: usize,
    active_clients: usize,
    depth: usize,
    blocks: usize,
    mode: Mode,
    codec: Codec,
    seconds: usize,
    base: u64,
    timeout: Duration,
    expected: &[Vec<(usize, u64)>],
) -> (Value, Vec<(usize, usize, usize, u64)>, Vec<String>) {
    #[cfg(all(feature = "resource-profiling", unix))]
    let resources_before = resource_profile::Snapshot::capture_io_boundary(clients)
        .expect("process resource profile unavailable");
    let start_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds as u64);
    let mut tasks = tokio::task::JoinSet::new();
    for (client, c) in clients.iter().take(active_clients).enumerate() {
        for lane_id in 0..depth {
            let connection = c.connection.clone();
            let drive = c.drive.clone();
            let handle = c.handles[lane_id];
            let expected = expected[client].clone();
            tasks.spawn(async move {
                (
                    client,
                    lane_id,
                    lane(
                        connection,
                        drive,
                        handle,
                        client,
                        lane_id,
                        depth,
                        blocks,
                        mode,
                        codec,
                        deadline,
                        base + lane_id as u64 * 10_000_000,
                        timeout,
                        expected,
                    )
                    .await,
                )
            });
        }
    }
    let (mut read, mut write) = (Histogram::default(), Histogram::default());
    let mut errors = vec![];
    let mut ledger = vec![];
    while let Some(r) = tasks.join_next().await {
        match r {
            Ok((client, lane, r)) => {
                read.merge(&r.read);
                write.merge(&r.write);
                errors.extend(r.errors);
                ledger.extend(
                    r.last
                        .into_iter()
                        .map(|(block, seq)| (client, lane, block, seq)),
                );
            }
            Err(e) => errors.push(format!("worker failed: {e}")),
        }
    }
    if read.count + write.count == 0 {
        errors.push("stage completed zero operations".into());
    }
    let elapsed = start.elapsed().as_secs_f64();
    let iops = (read.count + write.count) as f64 / elapsed;
    #[cfg(all(feature = "resource-profiling", unix))]
    let resources = resource_profile::Snapshot::capture_io_boundary(clients)
        .expect("process resource profile unavailable")
        .delta(&resources_before)
        .expect("process resource counters invalid");
    let mut report = json!({"mode":format!("{mode:?}"),"codec":codec.label(),"nominal_seconds":seconds,"start_unix_ms":start_unix_ms,"finish_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),"clients":clients.len(),"active_clients":active_clients,"servers":server_count,"per_client_depth":depth,"total_queue_depth":active_clients*depth,"elapsed_seconds_including_drain":elapsed,"read":read.json(),"write":write.json(),"read_iops":read.count as f64/elapsed,"write_iops":write.count as f64/elapsed,"total_iops":iops,"payload_mib_per_second":iops*BYTES as f64/1048576.0,"reference_target_iops":100000,"target_attainment":iops/100000.0,"failures":errors.len(),"cache":"cache-warm randomized dataset; no cold-cache claim"});
    report["runtime"] = runtime_worker_receipt();
    #[cfg(all(feature = "resource-profiling", unix))]
    let report = {
        let mut report = report;
        report["resources"] = resources;
        report
    };
    (report, ledger, errors)
}
const RUNTIME_WORKER_SELECTOR: &str = "MOUNT_RS_REMOTE_SATURATION_RUNTIME_WORKERS";

fn selected_runtime_workers(value: Option<&str>) -> Result<usize, &'static str> {
    match value {
        None | Some("16") => Ok(16),
        Some("32") => Ok(32),
        Some("64") => Ok(64),
        Some(_) => Err("runtime worker selector requires exactly 16, 32 or 64"),
    }
}

const JOURNAL_SELECTOR: &str = "MOUNT_RS_REMOTE_SATURATION_SQLITE_JOURNAL";

// Experimental fixture selection only. Validate before opening any provider.
fn selected_sqlite_journal<'a>(
    value: Option<&'a str>,
    provider: &str,
    separate: bool,
    provision: bool,
    preseed: bool,
    profiled: bool,
) -> Result<Option<&'a str>, &'static str> {
    let Some(mode) = value else { return Ok(None) };
    if !matches!(mode, "DELETE" | "WAL") {
        return Err("SQLite journal selector requires DELETE or WAL");
    }
    if provider != "sqlite" || !separate || !provision || preseed || !profiled {
        return Err(
            "SQLite journal selection requires profiled, provisioned, separate owned SQLite drives without preseed",
        );
    }
    Ok(Some(mode))
}

fn validate_journal_connections(
    snapshot: &Value,
    mode: &str,
    count: usize,
    expected: Option<&BTreeSet<u64>>,
) -> Result<BTreeSet<u64>, String> {
    let rows = snapshot["connections"]
        .as_array()
        .ok_or("SQLite connections unavailable")?;
    if snapshot.get("error").is_some() || count == 0 || rows.len() != count {
        return Err("SQLite journal connection count mismatch".into());
    }
    let mut ids = BTreeSet::new();
    for row in rows {
        let id = row["connection_id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or("SQLite connection identity unavailable")?;
        if !ids.insert(id) || row.get("error").is_some() || row["counter_overflow"] != false {
            return Err("SQLite connection diagnostic incomplete".into());
        }
        let config = &row["configuration"];
        if config["journal_mode"] != mode.to_ascii_lowercase()
            || config["synchronous"] != 2
            || config["locking_mode"] != "normal"
            || config["is_autocommit"] != true
            || config["busy_timeout_ms"] != 5000
            || config["fullfsync"] != 0
            || config["checkpoint_fullfsync"] != 0
            || config["wal_autocheckpoint_pages"] != 1000
            || config["cache_size"] != -2000
            || row["page_size"] != 4096
        {
            return Err("SQLite journal connection configuration mismatch".into());
        }
    }
    if expected.is_some_and(|expected| *expected != ids) {
        return Err("SQLite journal connection identities changed".into());
    }
    Ok(ids)
}

fn complete_terminal_os_io(resources: &Value) -> bool {
    let io = &resources["os_io"];
    io["enabled_start"] == true
        && io["enabled_end"] == true
        && io["process_disk"]["complete"] == true
        && (io["host_block_device"]["status"] == "unselected"
            || io["host_block_device"]["complete"] == true)
}

fn owned_sqlite_file_receipts(backends: &[backend::Backend]) -> Result<Vec<Value>, String> {
    backends
        .iter()
        .enumerate()
        .map(|(index, backend)| {
            Ok(json!({"drive_index":index,"files":backend.owned_sqlite_file_bytes()?}))
        })
        .collect()
}

#[test]
fn terminal_io_rejects_unavailable_banks_despite_successful_outer_snapshot() {
    let valid = json!({"os_io":{"enabled_start":true,"enabled_end":true,
        "process_disk":{"complete":true},"host_block_device":{"complete":true}}});
    assert!(complete_terminal_os_io(&valid));
    assert!(!complete_terminal_os_io(&Value::Null));
    for bank in ["process_disk", "host_block_device"] {
        let mut incomplete = valid.clone();
        incomplete["os_io"][bank]["complete"] = json!(false);
        assert!(!complete_terminal_os_io(&incomplete));
    }
    let mut unselected = valid;
    unselected["os_io"]["host_block_device"] = json!({"complete":false,"status":"unselected"});
    assert!(complete_terminal_os_io(&unselected));
    unselected["os_io"]["enabled_end"] = json!(false);
    assert!(!complete_terminal_os_io(&unselected));
}

#[test]
fn sqlite_journal_selector_requires_owned_profiled_fixture() {
    assert_eq!(
        selected_sqlite_journal(None, "tidb", false, false, true, false),
        Ok(None)
    );
    for mode in ["DELETE", "WAL"] {
        assert_eq!(
            selected_sqlite_journal(Some(mode), "sqlite", true, true, false, true),
            Ok(Some(mode))
        );
    }
    for mode in ["", "wal", "delete", " WAL", "WAL;", "NORMAL"] {
        assert!(selected_sqlite_journal(Some(mode), "sqlite", true, true, false, true).is_err());
    }
    for (provider, separate, provision, preseed, profiled) in [
        ("tidb", true, true, false, true),
        ("sqlite", false, true, false, true),
        ("sqlite", true, false, false, true),
        ("sqlite", true, true, true, true),
        ("sqlite", true, true, false, false),
    ] {
        assert!(
            selected_sqlite_journal(
                Some("WAL"),
                provider,
                separate,
                provision,
                preseed,
                profiled
            )
            .is_err()
        );
    }
}

#[test]
fn sqlite_journal_receipt_rejects_missing_changed_or_weaker_connections() {
    let row = json!({"connection_id":1,"counter_overflow":false,"page_size":4096,
        "configuration":{"journal_mode":"wal","synchronous":2,"locking_mode":"normal",
            "is_autocommit":true,"busy_timeout_ms":5000,"fullfsync":0,
            "checkpoint_fullfsync":0,"wal_autocheckpoint_pages":1000,"cache_size":-2000}});
    let sample = json!({"connections":[row]});
    let ids = validate_journal_connections(&sample, "WAL", 1, None).unwrap();
    assert!(validate_journal_connections(&sample, "WAL", 1, Some(&ids)).is_ok());
    assert!(validate_journal_connections(&sample, "DELETE", 1, None).is_err());
    assert!(validate_journal_connections(&sample, "WAL", 2, None).is_err());
    assert!(validate_journal_connections(&sample, "WAL", 1, Some(&BTreeSet::from([2]))).is_err());
    for (path, value) in [
        ("synchronous", json!(1)),
        ("wal_autocheckpoint_pages", json!(0)),
        ("is_autocommit", json!(false)),
        ("busy_timeout_ms", json!(100)),
    ] {
        let mut invalid = sample.clone();
        invalid["connections"][0]["configuration"][path] = value;
        assert!(validate_journal_connections(&invalid, "WAL", 1, None).is_err());
    }
    for (path, value) in [
        ("connection_id", json!(0)),
        ("counter_overflow", json!(true)),
        ("error", json!("unavailable")),
        ("page_size", json!(8192)),
    ] {
        let mut invalid = sample.clone();
        invalid["connections"][0][path] = value;
        assert!(validate_journal_connections(&invalid, "WAL", 1, None).is_err());
    }
    let duplicate = json!({"connections":[sample["connections"][0],sample["connections"][0]]});
    assert!(validate_journal_connections(&duplicate, "WAL", 2, None).is_err());
}

fn saturation_runtime(value: Option<&str>) -> Result<tokio::runtime::Runtime, &'static str> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(selected_runtime_workers(value)?)
        .enable_all()
        .build()
        .map_err(|_| "saturation runtime construction failed")
}

fn runtime_worker_receipt() -> Value {
    json!({
        "scheduler":"tokio_multi_thread",
        "worker_threads":tokio::runtime::Handle::current().metrics().num_workers(),
        "selector_env":RUNTIME_WORKER_SELECTOR,
        "selector_value":std::env::var(RUNTIME_WORKER_SELECTOR).ok(),
        "scope":"observed Tokio scheduler worker pool size; not busy or physical thread count; per-client I/O depth and workload unchanged"
    })
}

#[test]
fn runtime_worker_selector_is_strict_and_defaults_to_16() {
    assert_eq!(selected_runtime_workers(None), Ok(16));
    for (value, expected) in [("16", 16), ("32", 32), ("64", 64)] {
        assert_eq!(selected_runtime_workers(Some(value)), Ok(expected));
    }
    for invalid in [
        "", "0", "1", "8", "17", "31", "65", "128", "016", "+32", "32 ", " 32", "32,64", "invalid",
    ] {
        assert!(
            selected_runtime_workers(Some(invalid)).is_err(),
            "{invalid:?}"
        );
    }
}

#[test]
#[ignore = "explicit serial runtime worker-pool observation control"]
fn runtime_worker_selector_reaches_observed_tokio_pool() {
    let caller_thread = std::thread::current().id();
    for selector in [None, Some("16"), Some("32"), Some("64")] {
        let expected = selected_runtime_workers(selector).unwrap();
        let runtime = saturation_runtime(selector).unwrap();
        let (receipt, task_thread) = runtime.block_on(async {
            tokio::spawn(async { (runtime_worker_receipt(), std::thread::current().id()) })
                .await
                .unwrap()
        });
        assert_ne!(
            task_thread, caller_thread,
            "probe must execute on a runtime worker"
        );
        let encoded = serde_json::to_vec(&receipt).unwrap();
        let observed: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(observed["worker_threads"], expected);
        assert_eq!(observed["scheduler"], "tokio_multi_thread");
        assert_eq!(observed["selector_env"], RUNTIME_WORKER_SELECTOR);
        println!(
            "MOUNT_RS_RUNTIME_WORKERS expected={expected} observed={} worker_task_verified=true",
            observed["worker_threads"]
        );
        // Each complete runtime is dropped before the next is constructed.
        drop(runtime);
    }
}

#[test]
#[ignore = "requires disposable provider; 100 QUIC clients / 10 independent coordinators"]
fn actual_tidb_100_clients_10_servers_saturation() {
    let selected = match std::env::var(RUNTIME_WORKER_SELECTOR) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => panic!("runtime worker selector must be Unicode"),
    };
    let runtime = saturation_runtime(selected.as_deref()).expect("invalid saturation runtime");
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(1800), packet())
            .await
            .expect("overall saturation deadline exceeded")
            .expect("saturation failed");
    });
}
async fn stage_observer(
    phase: &str,
    mode: Mode,
    depth: usize,
    id: &str,
    successes: u64,
    failures: usize,
    client_count: usize,
) -> Result<(), String> {
    let Ok(executable) = std::env::var("MOUNT_RS_DATASTORE_STAGE_OBSERVER") else {
        return Ok(());
    };
    if executable.is_empty() {
        return Ok(());
    }
    let args = vec![
        phase.to_owned(),
        format!("{mode:?}").to_lowercase(),
        (client_count * depth).to_string(),
        id.to_owned(),
        successes.to_string(),
        failures.to_string(),
    ];
    tokio::task::spawn_blocking(move || {
        let mut child = std::process::Command::new(executable)
            .args(args)
            .spawn()
            .map_err(|_| "datastore observer start failed")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|_| "datastore observer wait failed")?
            {
                return if status.success() {
                    Ok(())
                } else {
                    Err("datastore observer failed".to_owned())
                };
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("datastore observer timeout".to_owned());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    })
    .await
    .map_err(|_| "datastore observer task failed".to_owned())?
}
async fn packet() -> Result<(), String> {
    let binding = source_binary_receipt()?;
    let client_count = env_num(
        "MOUNT_RS_REMOTE_SATURATION_CLIENTS",
        DEFAULT_CLIENTS,
        1,
        10_000,
    );
    let active_clients = env_num(
        "MOUNT_RS_REMOTE_SATURATION_ACTIVE_CLIENTS",
        client_count,
        1,
        client_count,
    );
    let setup_concurrency = env_num("MOUNT_RS_REMOTE_SATURATION_SETUP_CONCURRENCY", 100, 1, 100);
    let server_count = env_num(
        "MOUNT_RS_REMOTE_SATURATION_SERVERS",
        DEFAULT_SERVERS,
        1,
        client_count,
    );
    let depths: Vec<usize> = std::env::var("MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS")
        .unwrap_or("1,2,4,8".into())
        .split(',')
        .map(|d| d.parse().expect("depth integer"))
        .collect();
    validate_depths(&depths)?;
    let mut modes = parse_modes(
        &std::env::var("MOUNT_RS_REMOTE_TIDB_SATURATION_MODES")
            .unwrap_or_else(|_| "read,write".into()),
    )?;
    if std::env::var("MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED").as_deref() == Ok("1") {
        modes.push(Mode::Mixed);
    }
    let codec = Codec::parse(
        &std::env::var("MOUNT_RS_REMOTE_SATURATION_CODEC").unwrap_or_else(|_| "binary".into()),
    )?;
    let seconds = env_num("MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS", 5, 1, 30);
    let warmup = env_num("MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS", 1, 1, 10);
    let blocks = env_num("MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS", 64, 32, 1024);
    assert!(blocks >= *depths.iter().max().unwrap());
    let hot_blocks = selected_hot_blocks(
        std::env::var("MOUNT_RS_REMOTE_SATURATION_HOT_BLOCKS"),
        blocks,
        &depths,
    )?;
    let timeout = Duration::from_secs(env_num(
        "MOUNT_RS_REMOTE_TIDB_SATURATION_REQUEST_TIMEOUT_SECONDS",
        30,
        1,
        60,
    ) as u64);
    let key = format!(
        "remote-saturation-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let preseed = std::env::var("MOUNT_RS_REMOTE_SATURATION_PRESEED").as_deref() == Ok("1");
    let separate =
        std::env::var("MOUNT_RS_REMOTE_SATURATION_SEPARATE_DRIVES").as_deref() == Ok("1");
    let provision = separate
        && std::env::var("MOUNT_RS_REMOTE_SATURATION_PROVISION_DRIVES").as_deref() == Ok("1");
    let requested_journal = match std::env::var(JOURNAL_SELECTOR) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("SQLite journal selector must be Unicode".into());
        }
    };
    let journal = selected_sqlite_journal(
        requested_journal.as_deref(),
        &std::env::var("MOUNT_RS_REMOTE_SATURATION_PROVIDER").unwrap_or_else(|_| "tidb".into()),
        separate,
        provision,
        preseed,
        cfg!(all(feature = "resource-profiling", unix))
            && mount_rs_core::diagnostics::profile::enabled(),
    )?;
    let backend = backend::Backend::from_environment(&key).await?;
    let tidb_pool_max_connections =
        env_num("MOUNT_RS_REMOTE_SATURATION_TIDB_POOL_MAX", 16, 1, 1024);
    let storage_contexts: Vec<_> = (0..server_count)
        .map(|_| {
            mount_rs_sdk::StorageContext::new(tidb_pool_max_connections)
                .map_err(|_| "invalid TiDB pool maximum")
        })
        .collect::<Result<_, _>>()?;
    let mut drive_backends = vec![];
    if separate {
        for i in 0..client_count {
            drive_backends.push(backend.child(i)?);
        }
    } else if preseed {
        backend.preseed_empty_files(active_clients).await?;
    }
    let drive_backends = std::sync::Arc::new(drive_backends);
    let (mut servers, mut endpoints, mut dirs, mut providers) = (vec![], vec![], vec![], vec![]);
    let max_depth = *depths.iter().max().unwrap();
    let mut clients = vec![];
    let mut reports = vec![];
    let mut failed_phase = None;
    let mut namespace_bytes = None;
    let mut setup_seconds = None;
    let mut driver_setup_seconds = None;
    let mut provisioning_seconds = None;
    let mut journal_receipts = vec![];
    let mut journal_connection_ids = None;
    let journal_connection_count = 2 * client_count * server_count;
    let mut journal_setup_seconds = None;
    let driver_setup_started = Instant::now();
    let topology = &backend.topology;
    let mut expected: Vec<Vec<(usize, u64)>> =
        vec![(0..blocks).map(|block| (0, block as u64 + 1)).collect(); client_count];
    let work = tokio::time::timeout(Duration::from_secs(1500), async {
        if provision {
            let started = Instant::now();
            for b in drive_backends.iter() {
                let fs = b.open(0).await?;
                fs.shutdown().await.map_err(|_| "drive provisioning shutdown failed")?;
                drop(fs);
            }
            provisioning_seconds = Some(started.elapsed().as_secs_f64());
        }
        if let Some(mode) = journal {
            let started = Instant::now();
            for (index, b) in drive_backends.iter().enumerate() {
                journal_receipts.push(json!({"drive_index":index,"configuration":b.configure_owned_sqlite_journal(mode)?}));
            }
            journal_setup_seconds = Some(started.elapsed().as_secs_f64());
        }
        if separate {
            let mut starts = tokio::task::JoinSet::new();
            for (i, context) in storage_contexts.iter().enumerate() {
                let backends = drive_backends.clone();
                let context = context.clone();
                starts.spawn(async move {
                    let mut opened = vec![];
                    let result = async {
                        let mut drivers = vec![];
                        for b in backends.iter() {
                            let fs = b.open_in_context(i, Some(&context)).await?;
                            drivers.push(fs.driver()); opened.push(fs);
                        }
                        wire::setup_with_drives(drivers).await
                    }.await;
                    (i,opened,result)
                });
            }
            let mut prepared = vec![];
            let mut errors = vec![];
            while let Some(result) = starts.join_next().await {
                match result {
                    Ok((i,opened,result)) => {
                        providers.extend(opened);
                        match result {Ok(wire)=>prepared.push((i,wire)),Err(e)=>errors.push(e)}
                    }
                    Err(e)=>errors.push(format!("server setup task failed: {e}")),
                }
            }
            prepared.sort_by_key(|(i,_)|*i);
            for (_, (s,e,d)) in prepared {servers.push(s);endpoints.push(e);dirs.push(d);}
            if !errors.is_empty() {return Err(format!("server setup failures: {errors:?}"));}
        } else {
            for (i, context) in storage_contexts.iter().enumerate() {
                let fs = backend.open_in_context(i, Some(context)).await?;
                let left = fs.driver(); providers.push(fs);
                let (s,e,d) = wire::setup_with_left(left).await?;
                servers.push(s);endpoints.push(e);dirs.push(d);
            }
        }

        if let Some(mode) = journal {
            journal_connection_ids = Some(validate_journal_connections(
                &mount_rs_sqlite::sqlite_io_diagnostics(false), mode, journal_connection_count, None)?);
        }
        driver_setup_seconds = Some(driver_setup_started.elapsed().as_secs_f64());
        let mut setup_tasks = tokio::task::JoinSet::new();
        let setup_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(setup_concurrency));
        let setup_started = Instant::now();
        for client in 0..client_count {
            let endpoint = endpoints[client % server_count].clone();
            let address = servers[client % server_count].local_addr();
            let setup_slots = setup_slots.clone();
            setup_tasks.spawn(async move {
                let _permit = setup_slots.acquire_owned().await.map_err(|_| "setup semaphore closed")?;
                let drive = if separate {format!("sandbox-{client}")} else {"left".into()};
                let connection = if separate {wire::connect_sandbox(&endpoint,address,client).await?} else {wire::connect(&endpoint, address).await?};
                if separate && client_count > 1 {
                    let denied = wire::request(&connection, 900_000, &format!("sandbox-{}", (client+1)%client_count), OperationName::Stat,json!({"path":"/"})).await?;
                    if denied != Err("EACCES".into()) {return Err("cross-sandbox access was not denied".into());}
                }
                if client >= active_clients {
                    if separate { wire::success(&connection, 1, &drive, OperationName::Stat, json!({"path":"/"})).await?; }
                    return Ok((client, Client { drive, connection, handles: vec![] }));
                }
                let handle = wire::success(
                    &connection,
                    1,
                    &drive,
                    OperationName::Open,
                    json!({"path":format!("/saturation-{client}"),"flags":if preseed && !separate {"r+"} else {"w+"},"mode":420}),
                )
                .await?
                .as_u64()
                .ok_or("invalid handle")?;
                for first in (0..blocks).step_by(256) {
                    let count = (blocks - first).min(256);
                    let data: Vec<u8> = (first..first+count).flat_map(|block| seeded_block(client, block)).collect();
                    let written=wire::handle_write(&connection,2+first as u64,&IoRequest{drive_id:drive.clone(),handle,position:Some((first*BYTES) as u64)},&data).await?;
                    if written != count * BYTES {
                        return Err("short setup write".into());
                    }
                }
                let mut handles = vec![handle];
                for lane in 1..max_depth {
                    let extra = wire::success(
                        &connection,
                        10_000 + lane as u64,
                        &drive,
                        OperationName::Open,
                        json!({"path":format!("/saturation-{client}"),"flags":"r+","mode":420}),
                    )
                    .await?
                    .as_u64()
                    .ok_or("invalid lane handle")?;
                    handles.push(extra);
                }
                Ok::<_, String>((
                    client,
                    Client {
                        drive,
                        connection,
                        handles,
                    },
                ))
            });
        }
        let mut prepared = vec![];
        let mut setup_errors = vec![];
        while let Some(result) = setup_tasks.join_next().await {
            match result {
                Ok(Ok(c)) => prepared.push(c),
                Ok(Err(e)) => setup_errors.push(e),
                Err(e) => setup_errors.push(format!("setup task failed: {e}")),
            }
        }
        if !setup_errors.is_empty() {
            return Err(format!("setup failures: count={} samples={:?}",setup_errors.len(),setup_errors.iter().take(8).collect::<Vec<_>>()));
        }
        prepared.sort_by_key(|(client, _)| *client);
        clients.extend(prepared.into_iter().map(|(_, c)| c));
        setup_seconds = Some(setup_started.elapsed().as_secs_f64());
        namespace_bytes =
            tokio::time::timeout(Duration::from_secs(30), if separate {drive_backends[0].namespace_bytes()} else {backend.namespace_bytes()})
                .await
                .map_err(|_| "namespace size probe deadline exceeded")??;
        let mut phase = 0u64;
        for depth in depths {
            for mode in &modes {
                for (measured, duration) in [(false, warmup), (true, seconds)] {
                    phase += 1;
                    let stage_id = format!("{key}-{phase}");
                    if measured { stage_observer("begin", *mode, depth, &stage_id, 0, 0, active_clients).await?; }
                    let stage_diagnostics = StageDiagnostics::capture(measured && mount_rs_core::diagnostics::storage::enabled());
                    let files_before = if measured && journal.is_some() {
                        Some(owned_sqlite_file_receipts(&drive_backends)?)
                    } else { None };
                    let sqlite_before = if measured && backend.name == "sqlite" && mount_rs_core::diagnostics::profile::enabled() {
                        Some(mount_rs_sqlite::sqlite_io_diagnostics(true))
                    } else { None };
                    if let (Some(mode), Some(before)) = (journal, sqlite_before.as_ref()) {
                        validate_journal_connections(before, mode, journal_connection_count, journal_connection_ids.as_ref())?;
                    }
                    let (mut report, ledger, errors) = stage(
                        &clients,
                        server_count,
                        active_clients,
                        depth,
                        hot_blocks,
                        *mode,
                        codec,
                        duration,
                        phase * 1_000_000_000,
                        timeout,
                        &expected,
                    )
                    .await;
                    report["file_blocks"] = json!(blocks);
                    report["hot_blocks"] = json!(hot_blocks);
                    report["hot_working_set_bytes"] = json!(active_clients * hot_blocks * BYTES);
                    if measured {
                        stage_diagnostics.finish_into(&mut report)?;
                        if let Some(before) = files_before {
                            report["sqlite_owned_files_begin"] = json!(before);
                            report["sqlite_owned_files_end"] = json!(owned_sqlite_file_receipts(&drive_backends)?);
                        }
                        if let Some(before) = sqlite_before {
                            report["sqlite_io_begin"] = before;
                            report["sqlite_io_end"] = mount_rs_sqlite::sqlite_io_diagnostics(false);
                            if let Some(mode) = journal {
                                validate_journal_connections(&report["sqlite_io_end"], mode, journal_connection_count, journal_connection_ids.as_ref())?;
                            }
                        }
                        stage_observer("end", *mode, depth, &stage_id,
                            report["read"]["completed"].as_u64().unwrap_or(0) + report["write"]["completed"].as_u64().unwrap_or(0), errors.len(), active_clients).await?;
                    }
                    validate_hot_ledger(&ledger, &expected, hot_blocks, depth)?;
                    for (c, l, b, s) in ledger {
                        expected[c][b] = (l, s);
                    }
                    validate_cold_suffix(&expected[..active_clients], hot_blocks)?;
                    if !errors.is_empty() {
                        let samples:Vec<&String>=errors.iter().take(8).collect();
                        failed_phase=Some(json!({"report":report,"measured":measured,"failure_count":errors.len(),"timeout_failures":errors.iter().filter(|e|e.contains("request timeout")).count(),"error_samples":samples}));
                        if measured {eprintln!("REMOTE_PROVIDER_SATURATION {report}");reports.push(report);}
                        return Err(format!("stage failures: count={} samples={samples:?}",errors.len()));
                    }
                    if measured {
                        eprintln!("REMOTE_PROVIDER_SATURATION {report}");
                        reports.push(report);
                    }
                }
            }
        }
        for c in &clients {
            for (lane, handle) in c.handles.iter().enumerate() {
                wire::success(
                    &c.connection,
                    u64::MAX - 1 - lane as u64,
                    &c.drive,
                    OperationName::HandleClose,
                    json!({"handle":handle}),
                )
                .await?;
            }
        }
        Ok::<(), String>(())
    })
    .await
    .unwrap_or_else(|_| Err("setup/work deadline exceeded".into()));
    // This separate interval includes final provider drops and possible last-close
    // checkpoints, but no fresh verification connections or payload operations.
    #[cfg(all(feature = "resource-profiling", unix))]
    let terminal_files_before = journal.map(|_| owned_sqlite_file_receipts(&drive_backends));
    #[cfg(all(feature = "resource-profiling", unix))]
    let terminal_before =
        journal.map(|_| resource_profile::Snapshot::capture_process_io_boundary());
    let terminal_started = Instant::now();
    // Always shut down all created resources after work failure. No write is retried.
    for e in endpoints {
        e.close(0u32.into(), b"shutdown");
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(60), async {
        let mut shutdown_tasks = tokio::task::JoinSet::new();
        for s in servers {
            shutdown_tasks.spawn(async move {
                s.close().await;
            });
        }
        let mut failures = vec![];
        while let Some(result) = shutdown_tasks.join_next().await {
            if result.is_err() {
                failures.push("server shutdown task failed");
            }
        }

        for fs in providers {
            if fs.shutdown().await.is_err() {
                failures.push("filesystem shutdown failed");
            }
        }
        for context in &storage_contexts {
            if context.close().await.is_err() {
                failures.push("storage context shutdown failed");
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!("shutdown failures: {failures:?}"))
        }
    })
    .await
    .unwrap_or_else(|_| Err("shutdown deadline exceeded".into()));
    drop(dirs);
    #[cfg(all(feature = "resource-profiling", unix))]
    let terminal_receipt = if let Some(before) = terminal_before {
        let elapsed_seconds = terminal_started.elapsed().as_secs_f64();
        let observed = before.and_then(|before| {
            resource_profile::Snapshot::capture_process_io_boundary()?.delta(&before)
        });
        let remaining = mount_rs_sqlite::sqlite_io_diagnostics(false);
        let files_before = terminal_files_before.expect("journal terminal file receipt selected");
        let files_after = owned_sqlite_file_receipts(&drive_backends);
        let incomplete = !observed.as_ref().is_ok_and(complete_terminal_os_io)
            || remaining["connections"]
                .as_array()
                .is_none_or(|rows| !rows.is_empty())
            || files_before.is_err()
            || files_after.is_err();
        json!({"elapsed_seconds":elapsed_seconds,"resources":observed.as_ref().ok(),
            "resource_error":observed.as_ref().err(),"remaining_provider_connections":remaining,
            "files_before":files_before.as_ref().ok(),"files_after":files_after.as_ref().ok(),
            "files_before_error":files_before.as_ref().err(),"files_after_error":files_after.as_ref().err(),
            "incomplete":incomplete,"scope":"server close and filesystem shutdown/drop; before fresh verification; includes process background and observer work; not checkpoint-only I/O"})
    } else {
        Value::Null
    };
    #[cfg(not(all(feature = "resource-profiling", unix)))]
    let terminal_receipt = {
        let _ = terminal_started;
        Value::Null
    };
    let cleanup = if terminal_receipt["incomplete"] == true {
        Err("SQLite terminal resource or connection-close receipt incomplete".into())
    } else {
        cleanup
    };
    let verification_run = work.is_ok() && cleanup.is_ok();
    let verification = if verification_run {
        if separate {
            async {
                for (client, b) in drive_backends.iter().take(active_clients).enumerate() {
                    verify_one_separate_drive_with_shutdown(
                        b,
                        server_count,
                        client,
                        &expected[client],
                        |fs| async move {
                            fs.shutdown()
                                .await
                                .map_err(|_| "separate fresh shutdown failed".into())
                        },
                    )
                    .await?;
                }
                Ok(())
            }
            .await
        } else {
            verify(&backend, server_count, &expected[..active_clients]).await
        }
    } else {
        Ok(())
    };
    let verification_status = if !verification_run {
        "skipped"
    } else if verification.is_ok() {
        "passed"
    } else {
        "failed"
    };
    let snapshot_verification =
        std::env::var("MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY").as_deref() == Ok("1");
    let mut artifact = json!({"separate_drives":separate,"drive_count":if separate {client_count} else {1},"driver_replicas":if separate {client_count*server_count} else {server_count},"verification_method":if separate && snapshot_verification {"all stored and fresh driver files"} else if separate {"all fresh driver files"} else if snapshot_verification {"all stored files plus fresh driver sample"} else {"all fresh driver files"},"fresh_driver_sample_limit":if separate {active_clients} else if snapshot_verification {64} else {active_clients},"schema":"mount-rs-provider-saturation-v2","inode_updates":backend.inode_mode.inode_updates(),"compact_inode_updates":backend.inode_mode.compact_inode_updates(),"provider":backend.name,"provider_identity":backend.identity,"provider_version":backend.version,"volume_key":key,"clients":client_count,"active_clients":active_clients,"servers":server_count,"offline_empty_file_preseed":preseed,"setup_concurrency":setup_concurrency,"setup_seconds":setup_seconds,"driver_setup_seconds":driver_setup_seconds,"parallel_server_startup":separate,"drives_provisioned_before_startup":provision,"provisioning_seconds":provisioning_seconds,"dataset_bytes":active_clients*blocks*BYTES,"namespace_bytes":namespace_bytes,"topology":topology,"debug_assertions":cfg!(debug_assertions),"build_profile":if cfg!(debug_assertions){"debug"}else{"release"},"warmup_seconds":warmup,"nominal_stage_seconds":seconds,"configured_modes":modes.iter().map(|m|format!("{m:?}")).collect::<Vec<_>>(),"audit_logging":"enabled; request audit cost included","latency_histogram":"power-of-two microsecond upper bounds","stages":reports,"failed_phase":failed_phase,"verification_status":verification_status,"verified_files":if verification_status=="passed"{active_clients}else{0},"work_error":work.as_ref().err(),"cleanup_error":cleanup.as_ref().err(),"verification_error":verification.as_ref().err()});
    artifact["metadata_provider"] = json!(backend.name);
    artifact["block_provider"] = json!(backend.block_provider());
    artifact["block_provider_selection"] = json!({"selector":"MOUNT_RS_REMOTE_SATURATION_BLOCK_PROVIDER",
        "requested":if backend.block_provider() == "rustfs" { "rustfs" } else { "metadata" }});
    artifact["rustfs_preflight"] = json!(backend.rustfs_preflight());
    artifact["runtime"] = runtime_worker_receipt();
    artifact["file_blocks"] = json!(blocks);
    artifact["hot_blocks"] = json!(hot_blocks);
    artifact["hot_working_set_bytes"] = json!(active_clients * hot_blocks * BYTES);
    artifact["cold_suffix_blocks_per_file"] = json!(blocks - hot_blocks);
    artifact["sqlite_journal_experiment"] = json!({"requested":journal,"setup_seconds":journal_setup_seconds,
        "owned_drive_receipts":journal_receipts,"provider_connection_ids":journal_connection_ids,
        "provider_connection_count":journal.map(|_| journal_connection_count),"terminal_cleanup":terminal_receipt,
        "production_default_changed":false});
    artifact["requested_inode_mode"] = json!(backend.inode_mode.label());
    artifact["inode_mode_selector_env"] = json!({
        "MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES":std::env::var("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES").ok(),
        "MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES":std::env::var("MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES").ok(),
    });
    artifact["source_binary_binding"] = binding;
    let mut mode_receipt_error = None;
    if verification_run && (backend.name == "tidb" || backend.name == "sqlite") {
        let mut receipts = vec![];
        if separate {
            for (index, owned) in drive_backends.iter().enumerate() {
                match owned.persisted_mode_receipt().await {
                    Ok(receipt) => receipts.push(json!({"drive_index":index,"mode":receipt})),
                    Err(error) => {
                        mode_receipt_error = Some(error);
                        break;
                    }
                }
            }
        } else {
            match backend.persisted_mode_receipt().await {
                Ok(receipt) => receipts.push(json!({"drive_index":0,"mode":receipt})),
                Err(error) => mode_receipt_error = Some(error),
            }
        }
        artifact["persisted_mode_receipts"] = json!(receipts);
    }
    if let Some(error) = &mode_receipt_error {
        artifact["persisted_mode_receipt_error"] = json!(error);
    }
    #[cfg(all(feature = "resource-profiling", unix))]
    let mut owned_count_error = None;
    #[cfg(all(feature = "resource-profiling", unix))]
    if verification_run && backend.name == "tidb" {
        let mut counts = vec![];
        if separate {
            for (index, owned) in drive_backends.iter().enumerate() {
                match owned.owned_counts().await {
                    Ok(value) => counts.push(json!({"drive_index":index,"counts":value})),
                    Err(error) => {
                        owned_count_error = Some(error);
                        break;
                    }
                }
            }
        } else {
            match backend.owned_counts().await {
                Ok(value) => counts.push(json!({"drive_index":0,"counts":value})),
                Err(error) => owned_count_error = Some(error),
            }
        }
        artifact["owned_logical_sql_counts"] = json!(counts);
        if let Some(error) = &owned_count_error {
            artifact["owned_logical_sql_counts_error"] = json!(error);
        }
    }
    artifact["wire_protocol_version"] = json!(2);
    artifact["wire_io"] = json!(codec.label());
    artifact["read_buffers"] = json!("binary lane reuses per-lane buffer");
    artifact["codec_selection_env"] = json!("MOUNT_RS_REMOTE_SATURATION_CODEC");
    artifact["numeric_diagnostic_scope"] =
        json!("current v2 control envelope; no compatibility fallback");
    artifact["encoding_timing"] = json!(
        "wire request construction and serialization, response validation, and read content comparison included; payload generation excluded before both read and write latency timestamps"
    );
    artifact["tidb_pool"] = json!({"scope":"per server per exact connection identity", "max_connections":tidb_pool_max_connections,"schema_initialization":"once per context and role"});
    let limits = mount_rs_service::server::RemoteTransferLimits::default();
    artifact["server_admission"] = json!({"scope":"per server, shared across all connections","active_data_operations":limits.active_data_operations,"active_control_operations":limits.active_control_operations,"data_bytes_each_direction":limits.data_bytes,"reserved_control_bytes_each_direction":limits.control_bytes,"quic_bidi_streams_per_connection":40});
    #[cfg(all(feature = "resource-profiling", unix))]
    let owned_count_error_ref = owned_count_error.as_deref();
    #[cfg(not(all(feature = "resource-profiling", unix)))]
    let owned_count_error_ref = None;
    finalize_artifact_qualification(
        &mut artifact,
        active_clients,
        verification_status,
        mode_receipt_error.as_deref(),
        owned_count_error_ref,
    );
    let output =
        std::env::var_os("MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT").map(std::path::PathBuf::from);
    write_saturation_artifact(&artifact, output.as_deref(), &key)?;
    work?;
    cleanup?;
    if let Some(error) = mode_receipt_error {
        return Err(error);
    }
    #[cfg(all(feature = "resource-profiling", unix))]
    if let Some(error) = owned_count_error {
        return Err(error);
    }
    verification
}

async fn verify_one_separate_drive_with_shutdown<F, Fut>(
    backend: &backend::Backend,
    server_count: usize,
    client: usize,
    expected: &[(usize, u64)],
    shutdown: F,
) -> Result<(), String>
where
    F: FnOnce(mount_rs_sdk::Filesystem) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let fs = backend.open(server_count).await?;
    let view = Loopback::from_arc(fs.driver());
    let verified = async {
        let actual = view
            .read_file(&format!("/saturation-{client}"))
            .await
            .map_err(|_| "separate fresh read failed")?;
        let want: Vec<u8> = expected
            .iter()
            .flat_map(|(lane, seq)| payload(client, *lane, *seq))
            .collect();
        if actual != want {
            return Err("separate drive content mismatch".into());
        }
        if std::env::var("MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY").as_deref() == Ok("1") {
            backend
                .verify_stored_files_from(client, &[expected.to_vec()])
                .await?;
        }
        Ok(())
    }
    .await;
    let closed = shutdown(fs).await;
    closed?;
    verified
}

fn finalize_artifact_qualification(
    artifact: &mut Value,
    active_clients: usize,
    file_verification_status: &str,
    mode_receipt_error: Option<&str>,
    owned_count_error: Option<&str>,
) {
    artifact["file_verification_status"] = json!(file_verification_status);
    let qualification_status = if mode_receipt_error.is_some()
        || owned_count_error.is_some()
        || !artifact["work_error"].is_null()
        || !artifact["cleanup_error"].is_null()
        || !artifact["verification_error"].is_null()
    {
        "failed"
    } else {
        file_verification_status
    };
    artifact["verification_status"] = json!(qualification_status);
    artifact["verified_files"] = json!(if qualification_status == "passed" {
        active_clients
    } else {
        0
    });
    if let Some(error) = mode_receipt_error {
        artifact["persisted_mode_receipt_error"] = json!(error);
    }
    if let Some(error) = owned_count_error {
        artifact["owned_logical_sql_counts_error"] = json!(error);
    }
}

fn write_saturation_artifact(
    artifact: &Value,
    path: Option<&std::path::Path>,
    key: &str,
) -> Result<(), String> {
    if let Some(path) = path {
        let bytes = serde_json::to_vec_pretty(artifact).unwrap();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("saturation");
        let retained = path.with_file_name(format!("{stem}-{key}.json"));
        std::fs::write(&retained, &bytes)
            .map_err(|e| format!("retained artifact write failed: {e}"))?;
        std::fs::write(path, bytes).map_err(|e| format!("artifact write failed: {e}"))?;
    }
    Ok(())
}

async fn verify(
    backend: &backend::Backend,
    server_count: usize,
    expected: &[Vec<(usize, u64)>],
) -> Result<(), String> {
    let fs = backend.open(server_count).await?;
    let view = Loopback::from_arc(fs.driver());
    let snapshot_verification =
        std::env::var("MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY").as_deref() == Ok("1");
    let verify_seconds = env_num("MOUNT_RS_REMOTE_SATURATION_VERIFY_SECONDS", 120, 1, 1200);
    let verified = tokio::time::timeout(Duration::from_secs(verify_seconds as u64), async {
        let names: BTreeSet<String> = view
            .readdir("/")
            .await
            .map_err(|_| "fresh namespace listing failed")?
            .into_iter()
            .map(|e| e.name)
            .collect();
        let want: BTreeSet<String> = (0..expected.len())
            .map(|c| format!("saturation-{c}"))
            .collect();
        if names != want {
            return Err("fresh namespace mismatch".into());
        }
        if snapshot_verification {
            backend.verify_stored_files(expected).await?;
        }
        for (client, blocks) in expected.iter().enumerate() {
            if snapshot_verification && client % expected.len().div_ceil(64) != 0 {
                continue;
            }
            let actual = view
                .read_file(&format!("/saturation-{client}"))
                .await
                .map_err(|_| "fresh read failed")?;
            let want: Vec<u8> = blocks
                .iter()
                .flat_map(|(lane, seq)| payload(client, *lane, *seq))
                .collect();
            if actual != want {
                return Err(format!("fresh file mismatch client {client}"));
            }
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|_| "verification deadline exceeded");
    let closed = tokio::time::timeout(Duration::from_secs(30), async {
        let mut failures = vec![];
        if fs.shutdown().await.is_err() {
            failures.push("fresh shutdown failed");
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!("fresh close failures: {failures:?}"))
        }
    })
    .await
    .map_err(|_| "fresh close deadline exceeded")?;
    closed?;
    verified??;
    Ok(())
}

fn seeded_block(client: usize, block: usize) -> Vec<u8> {
    payload(client, 0, block as u64 + 1)
}
#[test]
fn initial_dataset_has_100_times_64_distinct_blocks() {
    let unique: BTreeSet<Vec<u8>> = (0..DEFAULT_CLIENTS)
        .flat_map(|client| (0..64).map(move |block| seeded_block(client, block)))
        .collect();
    assert_eq!(unique.len(), DEFAULT_CLIENTS * 64);
}

#[tokio::test]
#[ignore = "requires owned TiDB and inode mode"]
async fn snapshot_oracle_rejects_incorrect_byte_ledger() {
    assert_eq!(
        std::env::var("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES").as_deref(),
        Ok("1")
    );
    let key = format!(
        "snapshot-oracle-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let backend = backend::Backend::from_environment(&key).await.unwrap();
    backend.preseed_empty_files(1).await.unwrap();
    let fs = backend.open(0).await.unwrap();
    Loopback::from_arc(fs.driver())
        .write_file("/saturation-0", &payload(0, 0, 1))
        .await
        .unwrap();
    fs.shutdown().await.unwrap();
    backend.verify_stored_files(&[vec![(0, 1)]]).await.unwrap();
    let error = backend
        .verify_stored_files(&[vec![(0, 2)]])
        .await
        .unwrap_err();
    assert!(error.contains("stored file mismatch"));
}

#[tokio::test]
#[ignore = "explicit owned SQLite compact cold-tail oracle"]
async fn hot_working_set_oracles_reject_corrupted_cold_tail() {
    assert_eq!(
        std::env::var("MOUNT_RS_REMOTE_SATURATION_PROVIDER").as_deref(),
        Ok("sqlite")
    );
    assert_eq!(
        std::env::var("MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES").as_deref(),
        Ok("1")
    );
    let key = format!(
        "hot-tail-oracle-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let backend = backend::Backend::from_environment(&key).await.unwrap();
    let mut expected = (0..64)
        .map(|block| (0, block as u64 + 1))
        .collect::<Vec<_>>();
    for (block, value) in expected.iter_mut().enumerate().take(32) {
        *value = (0, 900_000 + block as u64);
    }
    validate_cold_suffix(&[expected.clone()], 32).unwrap();
    let mut bytes = expected
        .iter()
        .flat_map(|(lane, seq)| payload(0, *lane, *seq))
        .collect::<Vec<_>>();
    let fs = backend.open(0).await.unwrap();
    let view = Loopback::from_arc(fs.driver());
    view.write_file("/saturation-0", &bytes).await.unwrap();
    fs.shutdown().await.unwrap();
    drop(view);
    drop(fs);
    backend
        .verify_stored_files(&[expected.clone()])
        .await
        .unwrap();
    verify_one_separate_drive_with_shutdown(&backend, 1, 0, &expected, |fs| async move {
        fs.shutdown()
            .await
            .map_err(|_| "cold-tail positive shutdown failed".to_owned())
    })
    .await
    .unwrap();

    // Alter a real cold block while keeping the full expected file unchanged.
    bytes[63 * BYTES + 17] ^= 0x40;
    let fs = backend.open(2).await.unwrap();
    let view = Loopback::from_arc(fs.driver());
    view.write_file("/saturation-0", &bytes).await.unwrap();
    fs.shutdown().await.unwrap();
    drop(view);
    drop(fs);
    let stored = backend
        .verify_stored_files(&[expected.clone()])
        .await
        .unwrap_err();
    assert!(stored.contains("stored file mismatch"), "{stored}");
    let fresh =
        verify_one_separate_drive_with_shutdown(&backend, 3, 0, &expected, |fs| async move {
            fs.shutdown()
                .await
                .map_err(|_| "cold-tail negative shutdown failed".to_owned())
        })
        .await
        .unwrap_err();
    assert_eq!(fresh, "separate drive content mismatch");
}

#[test]
fn mode_selection_is_explicit_and_bounded() {
    assert!(matches!(
        parse_modes("read").unwrap().as_slice(),
        [Mode::Read]
    ));
    assert!(matches!(
        parse_modes("write").unwrap().as_slice(),
        [Mode::Write]
    ));
    assert!(matches!(
        parse_modes("read,write").unwrap().as_slice(),
        [Mode::Read, Mode::Write]
    ));
    for invalid in ["", "unknown", "read,read", "write,write", "read,", "mixed"] {
        assert!(parse_modes(invalid).is_err(), "accepted {invalid}");
    }
}

fn parse_modes(input: &str) -> Result<Vec<Mode>, String> {
    let mut seen = BTreeSet::new();
    let mut modes = vec![];
    for name in input.split(',') {
        if !seen.insert(name) {
            return Err("duplicate saturation mode".into());
        }
        modes.push(match name {
            "read" => Mode::Read,
            "write" => Mode::Write,
            _ => return Err("saturation modes require read or write".into()),
        });
    }
    Ok(modes)
}

fn timeout_message(writing: bool) -> &'static str {
    if writing {
        "write request timeout: write outcome uncertain; never replayed"
    } else {
        "read request timeout; never replayed"
    }
}

#[path = "support/production_scale.rs"]
mod production_scale;
