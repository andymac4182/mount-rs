//! Local, bounded service records for explicit periodic capture and shutdown.

use crate::runtime::CliError;
use mount_rs_core::diagnostics::{filesystem_blocks, object_store, profile, storage};
use mount_rs_service::object_store_diagnostics::{Capture, CaptureContext, CodecError, Sample};
use mount_rs_service::service_diagnostics_frames;
use serde::Serialize;
use std::{
    ffi::OsStr,
    future::Future,
    io::{self, Write},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const RECORD_LIMIT: usize = 1024 * 1024;
#[cfg(test)]
const PREFIX: &[u8] = service_diagnostics_frames::PREFIX;
const LOGICAL_PREFIX: &[u8] = b"service_diagnostics ";

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

// Process-local, fixed-bank export only. An observed zero snapshot does not
// establish that a filesystem provider is configured or durable.
fn capture_filesystem_blocks(
    enabled: bool,
    snapshot: impl FnOnce() -> Option<filesystem_blocks::Snapshot>,
) -> Bank<filesystem_blocks::Snapshot> {
    if !enabled {
        return Bank {
            available: false,
            reason: Some("observer_disabled"),
            snapshot: None,
        };
    }
    match snapshot() {
        Some(snapshot) => Bank {
            available: true,
            reason: None,
            snapshot: Some(snapshot),
        },
        None => Bank {
            available: false,
            reason: Some("observer_snapshot_unavailable"),
            snapshot: None,
        },
    }
}

const FILESYSTEM_BLOCKS_SCOPE: &str = "process cumulative filesystem PUT/flush helper observations; inclusive overlapping wall time; offered_bytes count starts, not stored or durable bytes; compiled device helper flag, not volume certification; no configured-provider census, physical IOPS or provider drain acknowledgment";

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
    filesystem_blocks: Bank<filesystem_blocks::Snapshot>,
    filesystem_blocks_scope: &'static str,
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
        filesystem_blocks: capture_filesystem_blocks(false, || None),
        filesystem_blocks_scope: FILESYSTEM_BLOCKS_SCOPE,
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

fn capture_process_diagnostics(
    filesystem_enabled: bool,
    filesystem_snapshot: impl FnOnce() -> Option<filesystem_blocks::Snapshot>,
) -> ProcessDiagnostics<storage::Snapshot, profile::Snapshot> {
    let mut process = process_diagnostics(
        capture_bank(storage::enabled(), storage::snapshot),
        capture_bank(profile::enabled(), profile::snapshot),
    );
    process.filesystem_blocks = capture_filesystem_blocks(filesystem_enabled, filesystem_snapshot);
    process
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
    let logical_record = if output.write_all(LOGICAL_PREFIX).is_ok()
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
    };
    // Preserve the complete logical JSON observation, including every bank
    // row and raw u64, while respecting the process controller's physical
    // line bound. The caller writes this complete group under one stderr lock.
    // Export allocations are outside the warmed-bank update guarantees.
    service_diagnostics_frames::encode(
        &logical_record[LOGICAL_PREFIX.len()..logical_record.len() - 1],
    )
    .unwrap_or_default()
}

// Shutdown sidebands are distinct process observations even when both
// listener diagnostics are emitted in the same millisecond. Exhaustion omits
// a sample instead of reusing an identity. No new timer is installed.
static OBJECT_STORE_SHUTDOWN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn shutdown_object_store_capture(
    profiling: bool,
    sequence: &AtomicU64,
    pid: impl FnOnce() -> u32,
    observed_unix_ms: impl FnOnce() -> u64,
) -> Option<Capture> {
    if !profiling {
        return None;
    }
    let previous = sequence
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .ok()?;
    Some(Capture {
        pid: pid(),
        sequence: previous + 1,
        observed_unix_ms: observed_unix_ms(),
        context: CaptureContext::Shutdown,
        generation: None,
    })
}

fn capture_object_store(
    profiling: bool,
    capture: Capture,
    snapshot: impl FnOnce() -> Option<object_store::Snapshot>,
) -> Result<Option<Sample>, CodecError> {
    mount_rs_service::object_store_diagnostics::capture(profiling, capture, snapshot)
}

fn emit_object_store(sample: Result<Option<Sample>, CodecError>) {
    if let Ok(Some(sample)) = sample {
        // Export serialization/record allocations are outside warmed-bank
        // claims. Failed or partial writes remain unavailable to a complete-set
        // decoder; the controller retains the existing stderr/output limits.
        let _ = sample.write(&mut io::stderr().lock());
    }
}

pub(super) fn emit(observer: &mount_rs_service::server::ServerDiagnostics) {
    emit_snapshot(Transport::Quic, &observer.snapshot());
}

pub(super) fn emit_websocket(observer: &mount_rs_service::websocket::WebSocketDiagnostics) {
    emit_snapshot(Transport::WebSocket, &observer.snapshot());
}

struct PeriodicRecords<'a, Q, W, S, P> {
    quic: Option<&'a Q>,
    websocket: Option<&'a W>,
    process: &'a ProcessDiagnostics<S, P>,
    pid: u32,
    capture: CaptureMetadata,
}

struct RecordSink<F>(F);

impl<F: FnMut(&[u8])> Write for RecordSink<F> {
    fn write(&mut self, record: &[u8]) -> io::Result<usize> {
        (self.0)(record);
        Ok(record.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// The application routes one captured process value alongside zero, one or
// two listener snapshots. The injected sink is the existing best-effort record
// writer in production; tests observe this same routing boundary.
fn emit_periodic_records<Q: Serialize, W: Serialize, S: Serialize, P: Serialize>(
    records: PeriodicRecords<'_, Q, W, S, P>,
    object_store_enabled: bool,
    object_store_snapshot: impl FnOnce() -> Option<object_store::Snapshot>,
    mut output: impl FnMut(&[u8]),
) {
    if records.quic.is_none() && records.websocket.is_none() {
        return;
    }
    let sample = capture_object_store(
        object_store_enabled,
        Capture {
            pid: records.pid,
            sequence: records.capture.sequence,
            observed_unix_ms: records.capture.observed_unix_ms,
            context: CaptureContext::Periodic,
            generation: None,
        },
        object_store_snapshot,
    );
    if let Some(snapshot) = records.quic {
        output(&encode_periodic_record(
            Transport::Quic,
            snapshot,
            records.pid,
            records.process,
            records.capture,
        ));
    }
    if let Some(snapshot) = records.websocket {
        output(&encode_periodic_record(
            Transport::WebSocket,
            snapshot,
            records.pid,
            records.process,
            records.capture,
        ));
    }
    // A frame is staged before the callback is invoked. Diagnostic writes
    // retain best-effort outcome semantics and the controller's existing caps.
    if let Ok(Some(sample)) = sample {
        let _ = sample.write(&mut RecordSink(output));
    }
}

pub(super) fn emit_periodic(
    observer: Option<&mount_rs_service::server::ServerDiagnostics>,
    websocket: Option<&mount_rs_service::websocket::WebSocketDiagnostics>,
    capture: CaptureMetadata,
) {
    if observer.is_none() && websocket.is_none() {
        return;
    }
    // Process-wide banks are captured once for both listener records. The
    // listener snapshots remain separate, non-atomic observations.
    let filesystem = filesystem_blocks::Observer::enabled();
    let process = capture_process_diagnostics(filesystem.is_enabled(), || filesystem.snapshot());
    let quic = observer.map(|observer| observer.snapshot());
    let websocket = websocket.map(|observer| observer.snapshot());
    let object_store = object_store::Observer::enabled();
    emit_periodic_records(
        PeriodicRecords {
            quic: quic.as_ref(),
            websocket: websocket.as_ref(),
            process: &process,
            pid: std::process::id(),
            capture,
        },
        object_store.is_enabled(),
        || object_store.snapshot(),
        write_record,
    );
}

fn emit_snapshot(transport: Transport, snapshot: &impl Serialize) {
    let object_store = object_store::Observer::enabled();
    let object_store_sample = shutdown_object_store_capture(
        object_store.is_enabled(),
        &OBJECT_STORE_SHUTDOWN_SEQUENCE,
        std::process::id,
        || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(0)
        },
    )
    .map(|capture| {
        capture_object_store(object_store.is_enabled(), capture, || {
            object_store.snapshot()
        })
    });
    let filesystem = filesystem_blocks::Observer::enabled();
    let process = capture_process_diagnostics(filesystem.is_enabled(), || filesystem.snapshot());
    let record = encode_record(transport, snapshot, std::process::id(), &process);
    write_record(&record);
    if let Some(sample) = object_store_sample {
        // Shutdown context names this observation point; release counters and
        // zero in-flight rows do not acknowledge provider/socket shutdown.
        emit_object_store(sample);
    }
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
    use mount_rs_service::object_store_diagnostics;
    use serde::ser::Error as _;
    use serde_json::{Value, json};

    fn maximum_filesystem_snapshot() -> filesystem_blocks::Snapshot {
        let row = filesystem_blocks::CounterSnapshot {
            started: u64::MAX,
            inflight: u64::MAX,
            succeeded: u64::MAX,
            failed: u64::MAX,
            abandoned: u64::MAX,
            elapsed_ns: u64::MAX,
            max_ns: u64::MAX,
            offered_bytes: u64::MAX,
        };
        filesystem_blocks::Snapshot {
            operations: [filesystem_blocks::OperationSnapshot {
                waiter: row,
                queue: row,
                worker: row,
                put_path: [u64::MAX; filesystem_blocks::PATH_COUNT],
            }; filesystem_blocks::OP_COUNT],
            stages: [row; filesystem_blocks::STAGE_COUNT],
            saturated: true,
            concurrent_activity: true,
            ..Default::default()
        }
    }

    #[test]
    fn filesystem_blocks_disabled_missing_and_observed_zero_are_distinct() {
        use std::cell::Cell;
        let disabled = capture_filesystem_blocks(false, || panic!("disabled filesystem snapshot"));
        let disabled = serde_json::to_value(disabled).unwrap();
        assert_eq!(disabled["available"], false);
        assert_eq!(disabled["reason"], "observer_disabled");
        assert!(disabled.get("snapshot").is_none());
        let calls = Cell::new(0);
        let missing = capture_filesystem_blocks(true, || {
            calls.set(calls.get() + 1);
            None
        });
        assert_eq!(calls.get(), 1, "enabled filesystem bank was not sampled");
        let missing = serde_json::to_value(missing).unwrap();
        assert_eq!(missing["available"], false);
        assert_eq!(missing["reason"], "observer_snapshot_unavailable");
        assert!(missing.get("snapshot").is_none());
        let zero = capture_filesystem_blocks(true, || Some(filesystem_blocks::Snapshot::default()));
        let zero = serde_json::to_value(zero).unwrap();
        assert_eq!(zero["available"], true);
        assert!(zero.get("reason").is_none());
        assert!(
            zero["snapshot"].is_object(),
            "observed zero work must retain a typed bank"
        );
        assert_eq!(zero["snapshot"]["schema"], filesystem_blocks::SCHEMA);
        assert_eq!(zero["snapshot"]["stages"][0]["started"].as_u64(), Some(0));
    }

    #[test]
    fn filesystem_blocks_complete_maximum_bank_keeps_record_identity_and_limits() {
        let filesystem = maximum_filesystem_snapshot();
        let expected = serde_json::to_value(filesystem).unwrap();
        let storage = representative_storage(u64::MAX);
        let mut profile = profile::snapshot();
        assert_eq!(profile.entries.len(), 136);
        for entry in &mut profile.entries {
            entry.calls = u64::MAX;
            entry.elapsed_ns = u64::MAX;
            entry.units = u64::MAX;
        }
        let mut process = process_diagnostics(
            capture_bank(true, || storage),
            capture_bank(true, || profile),
        );
        process.filesystem_blocks = capture_filesystem_blocks(true, || Some(filesystem));
        for transport in [Transport::Quic, Transport::WebSocket] {
            let capture = CaptureMetadata {
                sequence: u64::MAX,
                observed_unix_ms: u64::MAX,
            };
            for (record, context) in [
                (
                    encode_periodic_record(transport, &json!({}), u32::MAX, &process, capture),
                    "periodic",
                ),
                (
                    encode_record(transport, &json!({}), u32::MAX, &process),
                    "shutdown",
                ),
            ] {
                assert!(
                    record
                        .split_inclusive(|byte| *byte == b'\n')
                        .all(|line| line.len() <= service_diagnostics_frames::LINE_LIMIT)
                );
                let value = parse_record(&record);
                assert!(value.get("diagnostic_incomplete").is_none());
                assert_eq!(value["schema"], "mount-rs.cli-service-diagnostics.v2");
                assert_eq!(value["pid"].as_u64(), Some(u64::from(u32::MAX)));
                assert_eq!(value["transport"], transport.label());
                assert_eq!(value["capture_context"], context);
                if context == "periodic" {
                    assert_eq!(value["capture"]["sequence"].as_u64(), Some(u64::MAX));
                    assert_eq!(
                        value["capture"]["observed_unix_ms"].as_u64(),
                        Some(u64::MAX)
                    );
                } else {
                    assert!(value.get("capture").is_none());
                }
                let banks = &value["process_diagnostics"];
                assert_eq!(
                    banks["filesystem_blocks"]["available"], true,
                    "filesystem bank missing from complete service record"
                );
                assert_eq!(banks["filesystem_blocks"]["snapshot"], expected);
                assert_eq!(banks["filesystem_blocks_scope"], FILESYSTEM_BLOCKS_SCOPE);
                assert_eq!(banks["capture_atomic"], false);
                assert_eq!(banks["application_drain_proven"], false);
                assert_eq!(
                    banks["storage"]["snapshot"]["entries"]
                        .as_array()
                        .unwrap()
                        .len(),
                    118
                );
                assert_eq!(
                    banks["profile"]["snapshot"]["entries"]
                        .as_array()
                        .unwrap()
                        .len(),
                    136
                );
                assert_eq!(banks["filesystem_blocks"]["snapshot"]["operations"][1]["queue"]["offered_bytes"].as_u64(), Some(u64::MAX));
            }
        }
    }

    #[test]
    fn filesystem_blocks_actual_process_capture_is_shared_by_periodic_listeners() {
        use std::cell::Cell;
        let filesystem = maximum_filesystem_snapshot();
        let calls = Cell::new(0);
        let process = capture_process_diagnostics(true, || {
            calls.set(calls.get() + 1);
            Some(filesystem)
        });
        assert_eq!(
            calls.get(),
            1,
            "actual process capture skipped its enabled filesystem bank"
        );
        let capture = CaptureMetadata {
            sequence: 9_007_199_254_740_993,
            observed_unix_ms: 17,
        };
        let quic = json!({"active_operations": 1});
        let websocket = json!({"active_operations": 2});
        let mut records = Vec::new();
        emit_periodic_records(
            PeriodicRecords {
                quic: Some(&quic),
                websocket: Some(&websocket),
                process: &process,
                pid: 321,
                capture,
            },
            false,
            || panic!("disabled independent object-store bank"),
            |record| records.push(record.to_vec()),
        );
        assert_eq!(
            calls.get(),
            1,
            "listener routing must not resample filesystem counters"
        );
        assert_eq!(records.len(), 2);
        let first = parse_record(&records[0]);
        let second = parse_record(&records[1]);
        assert_eq!(first["capture"], second["capture"]);
        assert_eq!(first["process_diagnostics"], second["process_diagnostics"]);
        for value in [first, second] {
            assert_eq!(value["pid"].as_u64(), Some(321));
            assert_eq!(
                value["capture"]["sequence"].as_u64(),
                Some(capture.sequence)
            );
            assert_eq!(
                value["process_diagnostics"]["filesystem_blocks"]["snapshot"],
                serde_json::to_value(filesystem).unwrap()
            );
        }
    }

    #[test]
    fn disabled_object_store_sideband_never_samples_or_exports_zero_rows() {
        let capture = Capture {
            pid: 321,
            sequence: 0,
            observed_unix_ms: 17,
            context: CaptureContext::Periodic,
            generation: None,
        };
        assert!(
            capture_object_store(false, capture, || panic!("disabled object-store capture"))
                .unwrap()
                .is_none()
        );
        // An enabled application observer can still have an unavailable bank;
        // it must not fabricate seven zero snapshots for that case.
        let unavailable = object_store::Observer::disabled();
        assert!(
            capture_object_store(true, capture, || unavailable.snapshot())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn object_store_sideband_uses_exact_periodic_identity_and_one_real_snapshot() {
        use object_store::ClientRole;
        use std::cell::Cell;

        let observer = object_store::Observer::isolated();
        let _primary = observer.client(ClientRole::PrimaryDataMixed);
        let calls = Cell::new(0);
        let capture = Capture {
            pid: 321,
            sequence: 9007199254740993,
            observed_unix_ms: 29,
            context: CaptureContext::Periodic,
            generation: None,
        };
        let sample = capture_object_store(true, capture, || {
            calls.set(calls.get() + 1);
            let captured = observer.snapshot();
            // The live bank changes after the actual capture. Serialization
            // must preserve the returned copy, not fetch a later bank state.
            let _later = observer.client(ClientRole::QualificationProbe);
            captured
        })
        .unwrap()
        .expect("enabled real bank must export a sample");
        let mut output = Vec::new();
        sample.write(&mut output).unwrap();
        let decoded = object_store_diagnostics::decode(&output).unwrap();
        assert_eq!(decoded.capture(), &capture);
        assert_eq!(calls.get(), 1);
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 7);
        for line in output.split_inclusive(|byte| *byte == b'\n') {
            assert!(line.len() <= object_store_diagnostics::RECORD_LIMIT);
            assert!(line.starts_with(object_store_diagnostics::PREFIX));
        }
        assert_eq!(decoded.snapshot().clients[0].constructed, 1);
        assert_eq!(decoded.snapshot().clients[0].live, 1);
        assert_eq!(decoded.snapshot().clients[3].constructed, 0);
        assert_eq!(observer.snapshot().unwrap().clients[3].constructed, 1);
        assert_eq!(observer.snapshot().unwrap().clients[3].released, 1);
    }

    #[test]
    fn object_store_sideband_keeps_max_u64_and_legacy_schema_separate() {
        let capture = Capture {
            pid: 321,
            sequence: u64::MAX,
            observed_unix_ms: u64::MAX,
            context: CaptureContext::Periodic,
            generation: Some(u64::MAX),
        };
        let mut snapshot = object_store::Snapshot::default();
        snapshot.clients[0].constructed = u64::MAX;
        snapshot.clients[0].http[0].body_bytes = u64::MAX;
        snapshot.bundles.live = u64::MAX;
        snapshot.cache.payload_bytes = u64::MAX;
        snapshot.concurrent_activity = true;
        let sample = capture_object_store(true, capture, || Some(snapshot))
            .unwrap()
            .expect("enabled max-u64 sample must remain available");
        let mut output = Vec::new();
        sample.write(&mut output).unwrap();
        let decoded = object_store_diagnostics::decode(&output).unwrap();
        assert_eq!(decoded.capture(), &capture);
        assert_eq!(decoded.snapshot(), &snapshot);
        for line in output.split_inclusive(|byte| *byte == b'\n') {
            assert!(line.len() <= 16 * 1024);
        }
        let legacy = parse_record(&encode_periodic_record(
            Transport::Quic,
            &json!({"counter": u64::MAX}),
            321,
            &disabled_process(),
            CaptureMetadata {
                sequence: u64::MAX,
                observed_unix_ms: u64::MAX,
            },
        ));
        assert_eq!(legacy["schema"], "mount-rs.cli-service-diagnostics.v2");
        assert_eq!(legacy["capture"]["sequence"].as_u64(), Some(u64::MAX));
        assert_eq!(legacy["snapshot"]["counter"].as_u64(), Some(u64::MAX));
        assert!(legacy.get("object_store_observation").is_none());
        assert_eq!(
            legacy["process_diagnostics"]["unavailable"]["http_attempts"]["available"],
            false
        );
        assert_eq!(RECORD_LIMIT, 1024 * 1024);
    }

    #[test]
    fn shutdown_object_store_identity_is_lazy_unique_and_exhaustion_closed() {
        let sequence = AtomicU64::new(0);
        assert!(
            shutdown_object_store_capture(
                false,
                &sequence,
                || panic!("disabled pid"),
                || panic!("disabled clock")
            )
            .is_none()
        );
        assert_eq!(sequence.load(Ordering::Relaxed), 0);
        let first = shutdown_object_store_capture(true, &sequence, || 321, || 42).unwrap();
        let second = shutdown_object_store_capture(true, &sequence, || 321, || 42).unwrap();
        assert_eq!(first.context, CaptureContext::Shutdown);
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(first.observed_unix_ms, second.observed_unix_ms);
        assert_ne!(first, second);
        let exhausted = AtomicU64::new(u64::MAX);
        assert!(
            shutdown_object_store_capture(
                true,
                &exhausted,
                || panic!("exhausted pid"),
                || panic!("exhausted clock")
            )
            .is_none()
        );
        assert_eq!(exhausted.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn actual_periodic_router_binds_one_sample_to_zero_one_or_two_listeners() {
        use std::cell::Cell;
        let quic = json!({"quic_counter": 3});
        let websocket = json!({"websocket_counter": 5});
        for (quic, websocket) in [
            (None, None),
            (Some(&quic), None),
            (None, Some(&websocket)),
            (Some(&quic), Some(&websocket)),
        ] {
            let observer = object_store::Observer::isolated();
            let _client = observer.client(object_store::ClientRole::PrimaryDataMixed);
            let calls = Cell::new(0);
            let capture = CaptureMetadata {
                sequence: 9007199254740993,
                observed_unix_ms: 51,
            };
            let mut records = Vec::new();
            emit_periodic_records(
                PeriodicRecords {
                    quic,
                    websocket,
                    process: &disabled_process(),
                    pid: 321,
                    capture,
                },
                true,
                || {
                    calls.set(calls.get() + 1);
                    observer.snapshot()
                },
                |record| records.push(record.to_vec()),
            );
            let listener_count = usize::from(quic.is_some()) + usize::from(websocket.is_some());
            let old_records: Vec<_> = records
                .iter()
                .filter(|record| record.starts_with(PREFIX))
                .collect();
            let new_records: Vec<_> = records
                .iter()
                .filter(|record| record.starts_with(object_store_diagnostics::PREFIX))
                .collect();
            assert_eq!(old_records.len(), listener_count);
            if listener_count == 0 {
                assert_eq!(
                    calls.get(),
                    0,
                    "no listeners must not sample a process bank"
                );
                assert!(records.is_empty());
                continue;
            }
            assert_eq!(
                calls.get(),
                1,
                "one/two listeners share one actual bank capture"
            );
            assert_eq!(
                new_records.len(),
                7,
                "listener count must not duplicate sidebands"
            );
            for record in old_records {
                let legacy = parse_record(record);
                assert_eq!(legacy["pid"].as_u64(), Some(321));
                assert_eq!(
                    legacy["capture"]["sequence"].as_u64(),
                    Some(capture.sequence)
                );
                assert_eq!(
                    legacy["capture"]["observed_unix_ms"].as_u64(),
                    Some(capture.observed_unix_ms)
                );
            }
            let bytes: Vec<_> = new_records
                .iter()
                .flat_map(|record| record.iter().copied())
                .collect();
            let sample = object_store_diagnostics::decode(&bytes).unwrap();
            assert_eq!(
                sample.capture(),
                &Capture {
                    pid: 321,
                    sequence: capture.sequence,
                    observed_unix_ms: capture.observed_unix_ms,
                    context: CaptureContext::Periodic,
                    generation: None,
                }
            );
            assert_eq!(sample.snapshot().clients[0].constructed, 1);
            assert_eq!(sample.snapshot().clients[0].live, 1);
        }
    }

    fn parse_record(record: &[u8]) -> Value {
        assert!(
            record.starts_with(PREFIX),
            "missing diagnostic record prefix"
        );
        assert_eq!(record.last(), Some(&b'\n'), "missing record terminator");
        assert!(
            record
                .split_inclusive(|byte| *byte == b'\n')
                .all(|line| line.len() <= service_diagnostics_frames::LINE_LIMIT)
        );
        let logical = service_diagnostics_frames::decode(record).unwrap();
        assert!(logical.len() + LOGICAL_PREFIX.len() < RECORD_LIMIT);
        serde_json::from_slice(&logical).unwrap()
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
        assert_eq!(entries.len(), 118);
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
        assert_eq!(
            entries[116..118]
                .iter()
                .map(|row| row["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "sdk.metadata.compact_root_file_capability",
                "sdk.metadata.load_compact_root_file",
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
            ("sdk", 49),
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
            118
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
            assert!(
                record
                    .split_inclusive(|byte| *byte == b'\n')
                    .all(|line| line.len() <= 16 * 1024),
                "complete enabled banks exceed the native controller's physical line limit"
            );
            let value = parse_record(&record);
            assert_eq!(value["transport"], transport.label());
            assert!(value.get("diagnostic_incomplete").is_none());
            let banks = &value["process_diagnostics"];
            assert_eq!(
                banks["storage"]["snapshot"]["entries"]
                    .as_array()
                    .unwrap()
                    .len(),
                118
            );
            assert_eq!(
                banks["storage"]["snapshot"]["entries"].as_array().unwrap()[116..118]
                    .iter()
                    .map(|row| row["name"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec![
                    "sdk.metadata.compact_root_file_capability",
                    "sdk.metadata.load_compact_root_file",
                ]
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
