//! Local, bounded shutdown records for QUIC and existing process banks.

use mount_rs_core::diagnostics::{profile, storage};
use serde::Serialize;
use std::{
    ffi::OsStr,
    io::{self, Write},
};

const RECORD_LIMIT: usize = 1024 * 1024;
const PREFIX: &[u8] = b"service_diagnostics ";

pub(super) fn enabled(profile: Option<&OsStr>) -> bool {
    cfg!(feature = "io-profiling") && profile.is_some_and(|value| value == "1")
}

#[derive(Serialize)]
struct Record<'a, T, S, P> {
    schema: &'static str,
    pid: u32,
    transport: &'static str,
    capture_context: &'static str,
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
    snapshot: &impl Serialize,
    pid: u32,
    process_diagnostics: &ProcessDiagnostics<impl Serialize, impl Serialize>,
) -> Vec<u8> {
    let mut output = BoundedRecord(Vec::new());
    let record = Record {
        schema: "mount-rs.cli-service-diagnostics.v2",
        pid,
        transport: "quic",
        capture_context: "shutdown",
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
        format!(
            "service_diagnostics {{\"schema\":\"mount-rs.cli-service-diagnostics.v2\",\"pid\":{pid},\"transport\":\"quic\",\"capture_context\":\"shutdown\",\"diagnostic_incomplete\":true,\"reason\":\"serialization_failed_or_record_limit\"}}\n"
        )
        .into_bytes()
    }
}

pub(super) fn emit(observer: &mount_rs_service::server::ServerDiagnostics) {
    let snapshot = observer.snapshot();
    let process = process_diagnostics(
        capture_bank(storage::enabled(), storage::snapshot),
        capture_bank(profile::enabled(), profile::snapshot),
    );
    let record = encode_record(&snapshot, std::process::id(), &process);
    // Diagnostic output is best effort and cannot replace the service outcome.
    // The process controller owns a blocked stderr deadline and forced kills.
    let _ = io::stderr().lock().write_all(&record);
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
        let value = parse_record(&encode_record(&json!({"counter": exact}), 321, &process));
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
        assert_eq!(entries.len(), 78);
        for (actual, expected) in entries.iter().zip(&storage.entries) {
            assert_eq!(actual["name"], expected.name);
            assert_eq!(actual["calls"].as_u64(), Some(exact));
            assert_eq!(actual["returned_rows"].as_u64(), Some(exact));
            assert_eq!(actual["bytes"].as_u64(), Some(exact));
            assert_eq!(actual["latency_log2_us"].as_array().unwrap().len(), 32);
        }
        assert_eq!(entries.last().unwrap()["name"], "tidb.sql.flush_probe");
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
            78
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
        let value = parse_record(&encode_record(&json!({}), 123, &disabled_process()));
        for name in ["storage", "profile"] {
            let bank = &value["process_diagnostics"][name];
            assert_eq!(bank["available"], false);
            assert_eq!(bank["reason"], "observer_disabled");
            assert!(bank.get("snapshot").is_none());
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
        let record = encode_record(&json!({"counter": exact}), 123, &disabled_process());
        let value = parse_record(&record);
        assert_eq!(value["schema"], "mount-rs.cli-service-diagnostics.v2");
        assert_eq!(value["pid"].as_u64(), Some(123));
        assert_eq!(value["transport"], "quic");
        assert_eq!(value["capture_context"], "shutdown");
        assert_eq!(value["snapshot"]["counter"].as_u64(), Some(exact));
        assert!(!value["snapshot"]["counter"].is_string());
    }

    #[test]
    fn oversized_serializable_value_emits_only_a_bounded_incomplete_record() {
        let snapshot = json!({"active": vec!["x".repeat(256); RECORD_LIMIT / 128]});
        let process = process_diagnostics(
            capture_bank(true, || &snapshot),
            capture_bank(false, || json!({})),
        );
        for record in [
            encode_record(&snapshot, 456, &disabled_process()),
            encode_record(&json!({}), 456, &process),
        ] {
            let value = parse_record(&record);
            assert_eq!(value["pid"].as_u64(), Some(456));
            assert_eq!(value["diagnostic_incomplete"], true);
            assert_eq!(value["reason"], "serialization_failed_or_record_limit");
            assert!(value.get("snapshot").is_none());
            assert!(value.get("process_diagnostics").is_none());
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
        for record in [
            encode_record(&Unserializable, 789, &disabled_process()),
            encode_record(&json!({}), 789, &process),
        ] {
            let value = parse_record(&record);
            assert_eq!(value["schema"], "mount-rs.cli-service-diagnostics.v2");
            assert_eq!(value["pid"].as_u64(), Some(789));
            assert_eq!(value["diagnostic_incomplete"], true);
            assert!(value.get("process_diagnostics").is_none());
            assert!(!String::from_utf8(record).unwrap().contains("caller detail"));
        }
    }
}
