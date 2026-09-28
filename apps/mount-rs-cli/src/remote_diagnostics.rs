//! Local, bounded service records for explicit periodic capture and shutdown.

use crate::runtime::CliError;
use mount_rs_core::diagnostics::{profile, storage};
use serde::Serialize;
use std::{
    ffi::OsStr,
    future::Future,
    io::{self, Write},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const RECORD_LIMIT: usize = 1024 * 1024;
const PREFIX: &[u8] = b"service_diagnostics ";

#[derive(Clone, Copy)]
enum Transport {
    Quic,
    WebSocket,
}

impl Transport {
    const fn label(self) -> &'static str {
        match self {
            Self::Quic => "quic",
            Self::WebSocket => "websocket",
        }
    }
}

pub(super) fn enabled(profile: Option<&OsStr>) -> bool {
    cfg!(feature = "io-profiling") && profile.is_some_and(|value| value == "1")
}

/// `profiling` is the already-selected service observer, including its feature
/// gate. Disabled diagnostics ignore the interval without parsing its value.
pub(super) fn diagnostic_interval(
    profiling: bool,
    value: Option<&OsStr>,
) -> Result<Option<Duration>, CliError> {
    if !profiling {
        return Ok(None);
    }
    let Some(value) = value else {
        return Ok(None);
    };
    let millis = value
        .to_str()
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|millis| (1_000..=60_000).contains(millis))
        .ok_or_else(|| {
            CliError::usage(
                "MOUNT_RS_DIAGNOSTIC_INTERVAL_MS must be decimal milliseconds in 1000..=60000",
            )
        })?;
    Ok(Some(Duration::from_millis(millis)))
}

/// Both listener records in one tick share this identity. Sequence orders
/// captures within this wait; Unix time is an observation of the wall clock
/// and may move independently of the monotonic timer.
#[derive(Clone, Copy, Serialize)]
pub(super) struct CaptureMetadata {
    pub(super) sequence: u64,
    pub(super) observed_unix_ms: u64,
}

/// The stop future, timer, and callback belong to this future. No capture or
/// time driver is used without an interval, and stopping drops the timer.
pub(super) async fn wait_with_periodic_capture<F: Future>(
    stop: F,
    interval: Option<Duration>,
    mut capture: impl FnMut(CaptureMetadata),
) -> F::Output {
    tokio::pin!(stop);
    let Some(period) = interval else {
        return stop.await;
    };
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sequence = 0_u64;
    loop {
        tokio::select! {
            biased;
            result = &mut stop => return result,
            _ = timer.tick() => {
                let Some(next) = sequence.checked_add(1) else {
                    // Do not reuse an identity after an exhausted sequence.
                    drop(timer);
                    return stop.await;
                };
                sequence = next;
                let observed_unix_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                    .unwrap_or(0);
                capture(CaptureMetadata { sequence, observed_unix_ms });
            }
        }
    }
}

#[derive(Serialize)]
struct Record<'a, T, S, P> {
    schema: &'static str,
    pid: u32,
    transport: &'static str,
    capture_context: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture: Option<CaptureMetadata>,
    snapshot: &'a T,
    process_diagnostics: &'a ProcessDiagnostics<S, P>,
}

#[derive(Serialize)]
struct Bank<T> {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<T>,
}

fn capture_bank<T>(enabled: bool, snapshot: impl FnOnce() -> T) -> Bank<T> {
    Bank {
        available: enabled,
        reason: (!enabled).then_some("observer_disabled"),
        snapshot: enabled.then(snapshot),
    }
}

#[derive(Serialize)]
struct FamilyCoverage {
    rows: Vec<&'static str>,
    observation: &'static str,
}

#[derive(Serialize)]
struct Coverage {
    operation_names: &'static [&'static str],
    sdk: FamilyCoverage,
    tidb: FamilyCoverage,
    napi_forwarding: FamilyCoverage,
    pglite: FamilyCoverage,
    client_websocket: FamilyCoverage,
}

#[derive(Serialize)]
struct Unavailable {
    available: bool,
    reason: &'static str,
}

#[derive(Serialize)]
struct UnavailableMetrics {
    raw_object_store: Unavailable,
    http_attempts: Unavailable,
    physical_device_iops: Unavailable,
    process_cpu: Unavailable,
    process_rss: Unavailable,
    allocator_churn: Unavailable,
}

#[derive(Serialize)]
struct ProcessDiagnostics<S, P> {
    scope: &'static str,
    capture_atomic: bool,
    application_drain_proven: bool,
    duration_semantics: &'static str,
    storage: Bank<S>,
    profile: Bank<P>,
    coverage: Coverage,
    unavailable: UnavailableMetrics,
}

fn process_diagnostics<S, P>(storage: Bank<S>, profile: Bank<P>) -> ProcessDiagnostics<S, P> {
    let family = |prefixes: &[&str], observation| FamilyCoverage {
        rows: storage::operation_names()
            .iter()
            .copied()
            .filter(|name| prefixes.iter().any(|prefix| name.starts_with(prefix)))
            .collect(),
        observation,
    };
    let unavailable = |reason| Unavailable {
        available: false,
        reason,
    };
    ProcessDiagnostics {
        scope: "process_cumulative",
        capture_atomic: false,
        application_drain_proven: false,
        duration_semantics: "inclusive_overlapping_wall_time",
        storage,
        profile,
        coverage: Coverage {
            operation_names: storage::operation_names(),
            sdk: family(&["sdk."], "sdk_erased_method_boundary"),
            tidb: family(&["tidb."], "source_instrumented_adapter_families"),
            napi_forwarding: family(&["metadata.", "blocks."], "napi_forwarding_not_used_by_cli"),
            pglite: family(&["pglite."], "selected_provider_client_lock_wait"),
            client_websocket: family(
                &["client.websocket."],
                "declared_not_cli_server_source_instrumented",
            ),
        },
        unavailable: UnavailableMetrics {
            raw_object_store: unavailable("instance_handle_not_retained"),
            http_attempts: unavailable("not_exposed_by_process_banks"),
            physical_device_iops: unavailable("outside_logical_process_banks"),
            process_cpu: unavailable("external_resource_receipt_required"),
            process_rss: unavailable("external_resource_receipt_required"),
            allocator_churn: unavailable("no_allocator_instrumentation"),
        },
    }
}

struct BoundedRecord(Vec<u8>);

impl Write for BoundedRecord {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > RECORD_LIMIT - self.0.len() {
            return Err(io::Error::other("service diagnostic record limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_record(
    transport: Transport,
    snapshot: &impl Serialize,
    pid: u32,
    process_diagnostics: &ProcessDiagnostics<impl Serialize, impl Serialize>,
) -> Vec<u8> {
    encode_capture_record(transport, snapshot, pid, process_diagnostics, None)
}

fn encode_periodic_record(
    transport: Transport,
    snapshot: &impl Serialize,
    pid: u32,
    process_diagnostics: &ProcessDiagnostics<impl Serialize, impl Serialize>,
    capture: CaptureMetadata,
) -> Vec<u8> {
    encode_capture_record(transport, snapshot, pid, process_diagnostics, Some(capture))
}

fn encode_capture_record(
    transport: Transport,
    snapshot: &impl Serialize,
    pid: u32,
    process_diagnostics: &ProcessDiagnostics<impl Serialize, impl Serialize>,
    capture: Option<CaptureMetadata>,
) -> Vec<u8> {
    let mut output = BoundedRecord(Vec::new());
    let record = Record {
        schema: "mount-rs.cli-service-diagnostics.v2",
        pid,
        transport: transport.label(),
        capture_context: if capture.is_some() {
            "periodic"
        } else {
            "shutdown"
        },
        capture,
        snapshot,
        process_diagnostics,
    };
    if output.write_all(PREFIX).is_ok()
        && serde_json::to_writer(&mut output, &record).is_ok()
        && output.write_all(b"\n").is_ok()
    {
        output.0
    } else {
        // Never publish the partially serialized record or caller error text.
        let label = transport.label();
        match capture {
            None => format!(
                "service_diagnostics {{\"schema\":\"mount-rs.cli-service-diagnostics.v2\",\"pid\":{pid},\"transport\":\"{label}\",\"capture_context\":\"shutdown\",\"diagnostic_incomplete\":true,\"reason\":\"serialization_failed_or_record_limit\"}}\n"
            ),
            Some(CaptureMetadata { sequence, observed_unix_ms }) => format!(
                "service_diagnostics {{\"schema\":\"mount-rs.cli-service-diagnostics.v2\",\"pid\":{pid},\"transport\":\"{label}\",\"capture_context\":\"periodic\",\"capture\":{{\"sequence\":{sequence},\"observed_unix_ms\":{observed_unix_ms}}},\"diagnostic_incomplete\":true,\"reason\":\"serialization_failed_or_record_limit\"}}\n"
            ),
        }.into_bytes()
    }
}

pub(super) fn emit(observer: &mount_rs_service::server::ServerDiagnostics) {
    emit_snapshot(Transport::Quic, &observer.snapshot());
}

pub(super) fn emit_websocket(observer: &mount_rs_service::websocket::WebSocketDiagnostics) {
    emit_snapshot(Transport::WebSocket, &observer.snapshot());
}

pub(super) fn emit_periodic(
    observer: Option<&mount_rs_service::server::ServerDiagnostics>,
    websocket: Option<&mount_rs_service::websocket::WebSocketDiagnostics>,
    capture: CaptureMetadata,
) {
    if observer.is_none() && websocket.is_none() {
        return;
    }
    // These process-wide cumulative banks are captured once for both listener
    // records. The listener snapshots remain separate, non-atomic observations.
    let process = process_diagnostics(
        capture_bank(storage::enabled(), storage::snapshot),
        capture_bank(profile::enabled(), profile::snapshot),
    );
    let pid = std::process::id();
    if let Some(observer) = observer {
        write_record(&encode_periodic_record(
            Transport::Quic,
            &observer.snapshot(),
            pid,
            &process,
            capture,
        ));
    }
    if let Some(observer) = websocket {
        write_record(&encode_periodic_record(
            Transport::WebSocket,
            &observer.snapshot(),
            pid,
            &process,
            capture,
        ));
    }
}

fn emit_snapshot(transport: Transport, snapshot: &impl Serialize) {
    let process = process_diagnostics(
        capture_bank(storage::enabled(), storage::snapshot),
        capture_bank(profile::enabled(), profile::snapshot),
    );
    let record = encode_record(transport, snapshot, std::process::id(), &process);
    write_record(&record);
}

fn write_record(record: &[u8]) {
    // Diagnostic output is best effort and cannot replace the service outcome.
    // The process controller owns a blocked stderr deadline and forced kills.
    let _ = io::stderr().lock().write_all(record);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::diagnostics::{profile, storage};
    use serde::ser::Error as _;
    use serde_json::{Value, json};

    fn parse_record(record: &[u8]) -> Value {
        assert!(
            record.starts_with(PREFIX),
            "missing diagnostic record prefix"
        );
        assert_eq!(record.last(), Some(&b'\n'), "missing record terminator");
        assert_eq!(record.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert!(record.len() <= RECORD_LIMIT);
        serde_json::from_slice(&record[PREFIX.len()..]).unwrap()
    }

    fn representative_storage(exact: u64) -> storage::Snapshot {
        storage::Snapshot {
            in_flight: exact,
            forwarding_boxes: storage::ForwardingBoxes {
                sites: "napi_dynamic_provider_forwarding_future",
                calls: exact,
                requested_object_bytes: exact,
            },
            entries: storage::operation_names()
                .iter()
                .map(|name| storage::Entry {
                    name,
                    in_flight: exact,
                    returned_rows: exact,
                    returned_row_observations: 1,
                    calls: exact,
                    success: exact,
                    error: 0,
                    cancelled: 0,
                    bytes: exact,
                    elapsed_ns: exact,
                    latency_log2_us: [exact; 32],
                })
                .collect(),
        }
    }

    fn disabled_process() -> ProcessDiagnostics<Value, Value> {
        process_diagnostics(
            capture_bank(false, || panic!("disabled storage capture")),
            capture_bank(false, || panic!("disabled profile capture")),
        )
    }

    #[test]
    fn typed_process_banks_preserve_registry_scopes_and_raw_u64() {
        let exact = 9_007_199_254_740_993_u64;
        let storage = representative_storage(exact);
        let profile = profile::Snapshot {
            entries: vec![profile::Entry {
                name: "catalog.load",
                calls: exact,
                elapsed_ns: exact,
                units: exact,
            }],
        };
        let process = process_diagnostics(
            capture_bank(true, || storage.clone()),
            capture_bank(true, || profile.clone()),
        );
        let value = parse_record(&encode_record(
            Transport::Quic,
            &json!({"counter": exact}),
            321,
            &process,
        ));
        assert_eq!(value["schema"], "mount-rs.cli-service-diagnostics.v2");
        let process = &value["process_diagnostics"];
        assert_eq!(process["scope"], "process_cumulative");
        assert_eq!(process["capture_atomic"], false);
        assert_eq!(process["application_drain_proven"], false);
        assert_eq!(process["storage"]["available"], true);
        assert_eq!(process["profile"]["available"], true);
        let entries = process["storage"]["snapshot"]["entries"]
            .as_array()
            .unwrap();
        assert_eq!(entries.len(), 116);
        assert_eq!(
            entries[110..116]
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
        for (actual, expected) in entries.iter().zip(&storage.entries) {
            assert_eq!(actual["name"], expected.name);
            assert_eq!(actual["calls"].as_u64(), Some(exact));
            assert_eq!(actual["returned_rows"].as_u64(), Some(exact));
            assert_eq!(actual["bytes"].as_u64(), Some(exact));
            assert_eq!(actual["latency_log2_us"].as_array().unwrap().len(), 32);
        }
        assert_eq!(entries[77]["name"], "tidb.sql.flush_probe");
        assert_eq!(
            entries[78..85]
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
            entries[85..91]
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
        assert_eq!(entries[91]["name"], "client.quic.open_bi");
        assert_eq!(
            entries[92..100]
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
            entries[100..108]
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
        assert_eq!(
            process["storage"]["snapshot"]["in_flight"].as_u64(),
            Some(exact)
        );
        assert_eq!(
            process["storage"]["snapshot"]["forwarding_boxes"]["requested_object_bytes"].as_u64(),
            Some(exact)
        );
        let profile_entry = &process["profile"]["snapshot"]["entries"][0];
        assert_eq!(profile_entry["name"], profile.entries[0].name);
        assert_eq!(profile_entry["elapsed_ns"].as_u64(), Some(exact));
        assert_eq!(profile_entry["units"].as_u64(), Some(exact));
        assert_eq!(profile_entry["calls"].as_u64(), Some(exact));
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
            process["coverage"]["operation_names"]
                .as_array()
                .unwrap()
                .len(),
            116
        );
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
            assert!(process["unavailable"][name]["reason"].as_str().is_some());
            assert!(process["unavailable"][name].get("snapshot").is_none());
        }
    }

    #[test]
    fn disabled_process_banks_do_not_capture_or_export_zero_snapshots() {
        for transport in [Transport::Quic, Transport::WebSocket] {
            let value = parse_record(&encode_record(
                transport,
                &json!({}),
                123,
                &disabled_process(),
            ));
            assert_eq!(value["transport"], transport.label());
            for name in ["storage", "profile"] {
                let bank = &value["process_diagnostics"][name];
                assert_eq!(bank["available"], false);
                assert_eq!(bank["reason"], "observer_disabled");
                assert!(bank.get("snapshot").is_none());
            }
        }
    }

    #[test]
    fn only_exact_profile_one_selects_a_compiled_service_observer() {
        assert_eq!(
            enabled(Some(OsStr::new("1"))),
            cfg!(feature = "io-profiling")
        );
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("01"),
            Some("true"),
            Some(" 1"),
            Some("1 "),
        ] {
            assert!(
                !enabled(value.map(OsStr::new)),
                "unexpected selection for {value:?}"
            );
        }
        // TRACE_SERVICE does not participate in the observer-selection API.
        assert!(!enabled(None));
    }

    #[test]
    fn generic_encoding_preserves_raw_u64_above_javascript_integer_precision() {
        let exact = 9_007_199_254_740_993_u64;
        let record = encode_record(
            Transport::Quic,
            &json!({"counter": exact}),
            123,
            &disabled_process(),
        );
        let value = parse_record(&record);
        assert_eq!(value["schema"], "mount-rs.cli-service-diagnostics.v2");
        assert_eq!(value["pid"].as_u64(), Some(123));
        assert_eq!(value["transport"], "quic");
        assert_eq!(value["capture_context"], "shutdown");
        assert_eq!(value["snapshot"]["counter"].as_u64(), Some(exact));
        assert!(!value["snapshot"]["counter"].is_string());
    }

    #[test]
    fn websocket_record_preserves_its_application_schema_and_raw_u64() {
        let exact = 9_007_199_254_740_993_u64;
        let snapshot = json!({
            "schema": "mount-rs.service-websocket.v1",
            "enabled": true,
            "active_operations": 0,
            "entries": [{"name": "response.submit", "calls": exact}],
            "known_unavailable": ["tcp_wire_bytes", "tls_wire_bytes", "websocket_frame_counts", "peer_acknowledgement", "process_cpu", "physical_device_iops"]
        });
        let value = parse_record(&encode_record(
            Transport::WebSocket,
            &snapshot,
            123,
            &disabled_process(),
        ));
        assert_eq!(value["schema"], "mount-rs.cli-service-diagnostics.v2");
        assert_eq!(value["transport"], "websocket");
        assert_eq!(value["capture_context"], "shutdown");
        assert_eq!(value["snapshot"]["schema"], "mount-rs.service-websocket.v1");
        assert_eq!(
            value["snapshot"]["entries"][0]["calls"].as_u64(),
            Some(exact)
        );
        assert!(!value["snapshot"]["entries"][0]["calls"].is_string());
        assert!(value["snapshot"].get("transport").is_none());
        assert!(
            value["snapshot"]
                .get("registry_snapshot_elapsed_ns")
                .is_none()
        );
    }

    #[test]
    fn full_current_banks_with_maximum_u64_fit_existing_record_limit() {
        let storage = representative_storage(u64::MAX);
        let mut profile = profile::snapshot();
        assert_eq!(profile.entries.len(), 136);
        for entry in &mut profile.entries {
            entry.calls = u64::MAX;
            entry.elapsed_ns = u64::MAX;
            entry.units = u64::MAX;
        }
        let process = process_diagnostics(
            capture_bank(true, || storage),
            capture_bank(true, || profile),
        );
        for transport in [Transport::Quic, Transport::WebSocket] {
            let record = encode_record(transport, &json!({}), 123, &process);
            let value = parse_record(&record);
            assert_eq!(value["transport"], transport.label());
            assert!(value.get("diagnostic_incomplete").is_none());
            let banks = &value["process_diagnostics"];
            assert_eq!(
                banks["storage"]["snapshot"]["entries"]
                    .as_array()
                    .unwrap()
                    .len(),
                116
            );
            for index in [134, 135] {
                let row = &banks["profile"]["snapshot"]["entries"][index];
                assert_eq!(row["units"].as_u64(), Some(u64::MAX));
                assert_eq!(row["calls"].as_u64(), Some(u64::MAX));
            }
        }
    }

    #[test]
    fn oversized_serializable_value_emits_only_a_bounded_incomplete_record() {
        let snapshot = json!({"active": vec!["x".repeat(256); RECORD_LIMIT / 128]});
        let process = process_diagnostics(
            capture_bank(true, || &snapshot),
            capture_bank(false, || json!({})),
        );
        for transport in [Transport::Quic, Transport::WebSocket] {
            for record in [
                encode_record(transport, &snapshot, 456, &disabled_process()),
                encode_record(transport, &json!({}), 456, &process),
            ] {
                let value = parse_record(&record);
                assert_eq!(value["transport"], transport.label());
                assert_eq!(value["pid"].as_u64(), Some(456));
                assert_eq!(value["diagnostic_incomplete"], true);
                assert_eq!(value["reason"], "serialization_failed_or_record_limit");
                assert!(value.get("snapshot").is_none());
                assert!(value.get("process_diagnostics").is_none());
            }
        }
    }

    struct Unserializable;

    impl Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(S::Error::custom("caller detail must not appear in output"))
        }
    }

    #[test]
    fn serialization_failure_uses_a_fixed_incomplete_record() {
        let process = process_diagnostics(
            capture_bank(true, || Unserializable),
            capture_bank(false, || json!({})),
        );
        for transport in [Transport::Quic, Transport::WebSocket] {
            for record in [
                encode_record(transport, &Unserializable, 789, &disabled_process()),
                encode_record(transport, &json!({}), 789, &process),
            ] {
                let value = parse_record(&record);
                assert_eq!(value["schema"], "mount-rs.cli-service-diagnostics.v2");
                assert_eq!(value["transport"], transport.label());
                assert_eq!(value["pid"].as_u64(), Some(789));
                assert_eq!(value["diagnostic_incomplete"], true);
                assert!(value.get("process_diagnostics").is_none());
                assert!(!String::from_utf8(record).unwrap().contains("caller detail"));
            }
        }
    }

    #[test]
    fn periodic_interval_is_explicit_bounded_and_ignored_without_profiling() {
        use std::time::Duration;

        assert_eq!(diagnostic_interval(true, None).unwrap(), None);
        for (raw, millis) in [("1000", 1000), ("1001", 1001), ("60000", 60000)] {
            assert_eq!(
                diagnostic_interval(true, Some(OsStr::new(raw))).unwrap(),
                Some(Duration::from_millis(millis))
            );
        }
        let safe_error = diagnostic_interval(true, Some(OsStr::new("private-invalid-value")))
            .unwrap_err()
            .to_string();
        assert!(!safe_error.contains("private-invalid-value"));
        for raw in [
            "",
            "999",
            "60001",
            "0",
            "-1000",
            "+1000",
            " 1000",
            "1000 ",
            "1_000",
            "1000.0",
            "18446744073709551616",
            "private-invalid-value",
        ] {
            let error = diagnostic_interval(true, Some(OsStr::new(raw))).unwrap_err();
            let message = error.to_string();
            assert_eq!(message, safe_error, "interval errors must use fixed text");
            assert_eq!(
                diagnostic_interval(false, Some(OsStr::new(raw))).unwrap(),
                None
            );
        }
        assert_eq!(diagnostic_interval(false, None).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn periodic_interval_rejects_non_unicode_only_when_selected() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let raw = OsString::from_vec(b"private-\xff-interval".to_vec());
        let error = diagnostic_interval(true, Some(&raw)).unwrap_err();
        assert!(!error.to_string().contains("private"));
        assert_eq!(diagnostic_interval(false, Some(&raw)).unwrap(), None);
    }

    #[test]
    fn disabled_periodic_wait_needs_no_timer_driver_or_capture() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result: Result<u64, &'static str> =
            runtime.block_on(wait_with_periodic_capture(async { Ok(17) }, None, |_| {
                panic!("disabled periodic capture")
            }));
        assert_eq!(result, Ok(17));
        let error: Result<u64, &'static str> = runtime.block_on(wait_with_periodic_capture(
            async { Err("original stop failure") },
            None,
            |_| panic!("disabled periodic capture"),
        ));
        assert_eq!(error, Err("original stop failure"));
    }

    #[tokio::test]
    async fn periodic_wait_delays_first_capture_and_returns_original_stop_result() {
        use std::{
            cell::RefCell,
            future::Future,
            task::{Context, Poll, Waker},
            time::{Duration, SystemTime, UNIX_EPOCH},
        };

        let captures = RefCell::new(Vec::new());
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let mut stop_tx = Some(stop_tx);
        let lower_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let mut wait = Box::pin(wait_with_periodic_capture(
            async { stop_rx.await.unwrap() },
            Some(Duration::from_millis(5)),
            |capture| {
                let mut values = captures.borrow_mut();
                values.push(capture);
                if values.len() == 2 {
                    stop_tx
                        .take()
                        .unwrap()
                        .send(Err::<u64, _>("original stop failure"))
                        .unwrap();
                }
            },
        ));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        assert!(
            captures.borrow().is_empty(),
            "first interval tick was immediate"
        );
        let result = tokio::time::timeout(Duration::from_secs(2), wait)
            .await
            .unwrap();
        assert_eq!(result, Err("original stop failure"));
        let values = captures.into_inner();
        assert_eq!(values.len(), 2);
        for (index, capture) in values.iter().enumerate() {
            assert_eq!(capture.sequence, (index + 1) as u64);
            assert!(u128::from(capture.observed_unix_ms) >= lower_time);
        }
    }

    #[tokio::test]
    async fn periodic_wait_prefers_ready_stop_and_owns_callback_until_drop() {
        use std::{
            future::Future,
            sync::{
                Arc,
                atomic::{AtomicBool, AtomicUsize, Ordering},
            },
            task::{Context, Poll, Waker},
            time::Duration,
        };

        let result = wait_with_periodic_capture(
            async { Err::<(), _>("ready stop") },
            Some(Duration::from_nanos(1)),
            |_| panic!("a ready stop must win over a periodic tick"),
        )
        .await;
        assert_eq!(result, Err("ready stop"));

        struct CallbackOwner(Arc<AtomicBool>);
        impl Drop for CallbackOwner {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = CallbackOwner(dropped.clone());
        let observed_calls = calls.clone();
        let mut wait = Box::pin(wait_with_periodic_capture(
            std::future::pending::<()>(),
            Some(Duration::from_secs(60)),
            move |_| {
                let _retained = &owner;
                observed_calls.fetch_add(1, Ordering::SeqCst);
            },
        ));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        assert!(!dropped.load(Ordering::SeqCst));
        drop(wait);
        assert!(
            dropped.load(Ordering::SeqCst),
            "callback escaped the owned wait future"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn periodic_encoding_adds_capture_metadata_and_preserves_shutdown_v2() {
        let exact = 9_007_199_254_740_993_u64;
        let process = process_diagnostics(
            capture_bank(true, || representative_storage(exact)),
            capture_bank(false, || json!({})),
        );
        for transport in [Transport::Quic, Transport::WebSocket] {
            let periodic = parse_record(&encode_periodic_record(
                transport,
                &json!({"counter": exact, "active_operations": 7}),
                123,
                &process,
                CaptureMetadata {
                    sequence: exact,
                    observed_unix_ms: exact,
                },
            ));
            assert_eq!(periodic["schema"], "mount-rs.cli-service-diagnostics.v2");
            assert_eq!(periodic["transport"], transport.label());
            assert_eq!(periodic["capture_context"], "periodic");
            assert_eq!(periodic["capture"]["sequence"].as_u64(), Some(exact));
            assert_eq!(
                periodic["capture"]["observed_unix_ms"].as_u64(),
                Some(exact)
            );
            assert_eq!(periodic["snapshot"]["active_operations"].as_u64(), Some(7));
            assert_eq!(
                periodic["process_diagnostics"]["scope"],
                "process_cumulative"
            );
            assert_eq!(periodic["process_diagnostics"]["capture_atomic"], false);
            assert_eq!(
                periodic["process_diagnostics"]["application_drain_proven"],
                false
            );
            assert_eq!(
                periodic["process_diagnostics"]["storage"]["snapshot"]["in_flight"].as_u64(),
                Some(exact)
            );
            let shutdown = parse_record(&encode_record(transport, &json!({}), 123, &process));
            assert_eq!(shutdown["schema"], "mount-rs.cli-service-diagnostics.v2");
            assert_eq!(shutdown["capture_context"], "shutdown");
            assert!(shutdown.get("capture").is_none(), "shutdown shape changed");
            assert_eq!(
                shutdown
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                [
                    "capture_context",
                    "pid",
                    "process_diagnostics",
                    "schema",
                    "snapshot",
                    "transport"
                ]
            );
        }
    }

    #[test]
    fn periodic_encoding_failure_is_bounded_without_losing_capture_identity() {
        let oversized = json!({"private": "x".repeat(RECORD_LIMIT + 1)});
        for transport in [Transport::Quic, Transport::WebSocket] {
            let capture = CaptureMetadata {
                sequence: 5,
                observed_unix_ms: 123_456,
            };
            for encoded in [
                encode_periodic_record(transport, &oversized, 123, &disabled_process(), capture),
                encode_periodic_record(
                    transport,
                    &Unserializable,
                    123,
                    &disabled_process(),
                    capture,
                ),
            ] {
                let record = parse_record(&encoded);
                assert_eq!(record["schema"], "mount-rs.cli-service-diagnostics.v2");
                assert_eq!(record["capture_context"], "periodic");
                assert_eq!(record["capture"]["sequence"], 5);
                assert_eq!(record["capture"]["observed_unix_ms"], 123_456);
                assert_eq!(record["diagnostic_incomplete"], true);
                assert_eq!(record["reason"], "serialization_failed_or_record_limit");
                assert!(record.get("snapshot").is_none());
                assert!(record.get("process_diagnostics").is_none());
                let text = String::from_utf8(encoded).unwrap();
                assert!(!text.contains("caller detail"));
                assert!(!text.contains("private"));
            }
        }
    }
}
