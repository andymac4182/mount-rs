//! Private authored controls only; not compiled or executed during authoring.
use super::*;
use mount_rs_core::diagnostics::object_store::{ClientRole, HttpMethod, Observer, Snapshot};
use mount_rs_service::object_store_diagnostics::{self as codec, Capture, CaptureContext, Sample};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    io::{self, Write},
    sync::atomic::{AtomicU64, Ordering},
};

fn cli_binding() -> CliBinding<'static> {
    CliBinding {
        node: "node-0",
        generation: 29,
        pid: 321,
        path: "server-node-0-29.stderr",
        owner_complete: true,
    }
}

fn identity(pid: u32, sequence: u64, context: CaptureContext) -> Capture {
    Capture {
        pid,
        sequence,
        observed_unix_ms: 100 + sequence,
        context,
        generation: None,
    }
}

fn encoded(capture: Capture, snapshot: Snapshot) -> Vec<u8> {
    let sample = codec::capture(true, capture, || Some(snapshot))
        .unwrap()
        .unwrap();
    let mut bytes = Vec::new();
    sample.write(&mut bytes).unwrap();
    assert_eq!(bytes.split_inclusive(|byte| *byte == b'\n').count(), 7);
    assert_eq!(codec::decode(&bytes).unwrap(), sample);
    bytes
}

fn wire_hash(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn attempts(value: u64) -> Snapshot {
    let mut snapshot = Snapshot::default();
    snapshot.clients[0].http[0].attempts_started = value;
    snapshot
}

fn pair(context: CaptureContext, before: Snapshot, after: Snapshot) -> Vec<u8> {
    let mut bytes = encoded(identity(321, 0, context), before);
    bytes.extend(encoded(identity(321, 1, context), after));
    bytes
}

fn refused(value: &Value, reason: &str) {
    assert_eq!(value["status"], "unavailable");
    assert_eq!(value["reason"], reason);
}

fn held(capture: Capture, snapshot: Snapshot) -> Captured {
    let bytes = encoded(capture, snapshot);
    Captured {
        sample: Some(codec::decode(&bytes).unwrap()),
        evidence: json!({"status":"observed","capture":capture,
            "raw_export_sha256":wire_hash(&bytes),
            "quality":{"saturated":snapshot.saturated,
                "concurrent_activity":snapshot.concurrent_activity,
                "cache_unknown_live":snapshot.cache.unknown_live}}),
    }
}

#[derive(Default)]
struct CountWrites {
    calls: usize,
    bytes: Vec<u8>,
}
impl Write for CountWrites {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.calls += 1;
        Ok(())
    }
}

#[test]
fn cli_complete_codec_preserves_external_generation_hashes_and_u64() {
    let mut after = Snapshot::default();
    for (role, client) in after.clients.iter_mut().enumerate() {
        for (method, http) in client.http.iter_mut().enumerate() {
            http.attempts_started = (role * 10 + method + 1) as u64;
        }
    }
    after.clients[5].http[5].attempts_started = u64::MAX;
    after.clients[5].live = u64::MAX;
    after.cache.payload_bytes = u64::MAX;
    let first = encoded(
        identity(321, 0, CaptureContext::Periodic),
        Snapshot::default(),
    );
    let last = encoded(identity(321, 1, CaptureContext::Periodic), after);
    let mut bytes = b"legacy readiness line\nblob_cache legacy retained line\nservice_diagnostics legacy retained line\n".to_vec();
    bytes.extend(&first);
    bytes.extend(b"legacy unrelated line\n");
    bytes.extend(&last);
    let result = project_cli(&cli_binding(), &bytes);
    assert_eq!(result["status"], "observed");
    assert_eq!(result["owner"]["generation"].as_u64(), Some(29));
    assert_eq!(result["owner"]["pid"].as_u64(), Some(321));
    assert_eq!(result["owner"]["stderr"]["path"], cli_binding().path);
    assert_eq!(
        result["owner"]["stderr"]["bytes"].as_u64(),
        Some(bytes.len() as u64)
    );
    assert_eq!(result["owner"]["stderr"]["sha256"], wire_hash(&bytes));
    let history = &result["contexts"]["periodic"];
    assert_eq!(history["samples"].as_u64(), Some(2));
    assert_eq!(history["first"]["raw_export_sha256"], wire_hash(&first));
    assert_eq!(history["last"]["raw_export_sha256"], wire_hash(&last));
    assert_eq!(
        history["first"]["capture"],
        json!(identity(321, 0, CaptureContext::Periodic))
    );
    assert!(history["first"]["capture"].get("generation").is_none());
    assert!(result["contexts"]["shutdown"].is_null());
    let window = &history["window"];
    assert_eq!(window["status"], "observed");
    assert_eq!(window["clients"].as_array().unwrap().len(), 6);
    for (role, client) in after.clients.iter().enumerate() {
        assert_eq!(window["clients"][role]["http"].as_array().unwrap().len(), 6);
        for (method, http) in client.http.iter().enumerate() {
            assert_eq!(
                window["clients"][role]["http"][method]["counters"]["attempts_started"].as_u64(),
                Some(http.attempts_started)
            );
        }
    }
    assert_eq!(
        window["clients"][5]["gauges"]["live"]["after"].as_u64(),
        Some(u64::MAX)
    );
    assert_eq!(
        window["cache"]["gauges"]["payload_bytes"]["after"].as_u64(),
        Some(u64::MAX)
    );
    assert_eq!(
        codec::decode(&last).unwrap().snapshot().clients[5].http[5].attempts_started,
        u64::MAX
    );
}

#[test]
fn cli_near_maximum_counter_keeps_a_one_unit_delta_exact() {
    let bytes = pair(
        CaptureContext::Periodic,
        attempts(u64::MAX - 1),
        attempts(u64::MAX),
    );
    let result = project_cli(&cli_binding(), &bytes);
    assert_eq!(result["status"], "observed");
    assert_eq!(result["contexts"]["periodic"]["window"]["clients"][0]["http"][0]["counters"]["attempts_started"].as_u64(), Some(1));
}

#[test]
fn cli_periodic_and_shutdown_histories_keep_identical_sequences_separate() {
    let mut bytes = encoded(identity(321, 0, CaptureContext::Periodic), attempts(10));
    bytes.extend(encoded(
        identity(321, 0, CaptureContext::Shutdown),
        attempts(50),
    ));
    bytes.extend(encoded(
        identity(321, 1, CaptureContext::Periodic),
        attempts(15),
    ));
    bytes.extend(encoded(
        identity(321, 1, CaptureContext::Shutdown),
        attempts(52),
    ));
    let result = project_cli(&cli_binding(), &bytes);
    assert_eq!(result["status"], "observed");
    for (context, delta) in [("periodic", 5), ("shutdown", 2)] {
        let history = &result["contexts"][context];
        assert_eq!(history["samples"].as_u64(), Some(2));
        assert_eq!(history["first"]["capture"]["sequence"].as_u64(), Some(0));
        assert_eq!(
            history["first"]["capture"]["observed_unix_ms"].as_u64(),
            Some(100)
        );
        assert_eq!(
            history["window"]["clients"][0]["http"][0]["counters"]["attempts_started"].as_u64(),
            Some(delta)
        );
    }
}

#[test]
fn cli_single_complete_sample_has_no_window() {
    let bytes = encoded(
        identity(321, 0, CaptureContext::Shutdown),
        attempts(u64::MAX),
    );
    let result = project_cli(&cli_binding(), &bytes);
    assert_eq!(result["status"], "observed");
    let history = &result["contexts"]["shutdown"];
    assert_eq!(history["samples"].as_u64(), Some(1));
    refused(&history["window"], "single_sample");
}

#[test]
fn cli_missing_duplicate_or_malformed_frames_cannot_be_salvaged() {
    let full = encoded(
        identity(321, 0, CaptureContext::Periodic),
        Snapshot::default(),
    );
    let lines: Vec<_> = full.split_inclusive(|byte| *byte == b'\n').collect();
    let partial = lines[..6].concat();
    let mut duplicate = full.clone();
    duplicate.extend_from_slice(lines[0]);
    let mut malformed = full.clone();
    let mut damaged = lines[0].to_vec();
    damaged[codec::PREFIX.len()] = b'!';
    malformed.extend(damaged);
    for bytes in [partial, duplicate, malformed] {
        refused(&project_cli(&cli_binding(), &bytes), "invalid_frames");
    }
}

#[test]
fn cli_foreign_pid_generation_or_context_is_rejected() {
    let base = identity(321, 0, CaptureContext::Periodic);
    for capture in [
        Capture { pid: 322, ..base },
        Capture {
            generation: Some(29),
            ..base
        },
        Capture {
            context: CaptureContext::WorkerBoundary,
            ..base
        },
    ] {
        let bytes = encoded(capture, Snapshot::default());
        refused(&project_cli(&cli_binding(), &bytes), "foreign_capture");
    }
}

#[test]
fn cli_repeated_or_regressed_capture_identity_is_rejected() {
    let first = identity(321, 2, CaptureContext::Periodic);
    for after in [
        first,
        Capture {
            sequence: 1,
            observed_unix_ms: 103,
            ..first
        },
        Capture {
            sequence: 3,
            observed_unix_ms: 101,
            ..first
        },
    ] {
        let mut bytes = encoded(first, attempts(1));
        bytes.extend(encoded(after, attempts(2)));
        refused(
            &project_cli(&cli_binding(), &bytes),
            "duplicate_or_regressed_capture",
        );
    }
}

#[test]
fn cli_counter_reset_is_rejected_before_any_window_publication() {
    let bytes = pair(CaptureContext::Shutdown, attempts(5), attempts(4));
    refused(&project_cli(&cli_binding(), &bytes), "counter_reset");
    let mut recovered = encoded(identity(321, 0, CaptureContext::Periodic), attempts(5));
    recovered.extend(encoded(
        identity(321, 1, CaptureContext::Periodic),
        attempts(4),
    ));
    recovered.extend(encoded(
        identity(321, 2, CaptureContext::Periodic),
        attempts(6),
    ));
    refused(&project_cli(&cli_binding(), &recovered), "counter_reset");
    let mut before = attempts(1);
    before.clients[0].http[0].dispatch_max_ns = 100;
    before.clients[0].http[0].body_max_ns = 90;
    before.bundles.max_ns = 80;
    for maximum in 0..3 {
        let mut after = before;
        after.clients[0].http[0].attempts_started = 2;
        match maximum {
            0 => after.clients[0].http[0].dispatch_max_ns = 99,
            1 => after.clients[0].http[0].body_max_ns = 89,
            _ => after.bundles.max_ns = 79,
        }
        refused(
            &project_cli(
                &cli_binding(),
                &pair(CaptureContext::Periodic, before, after),
            ),
            "counter_reset",
        );
    }
}

#[test]
fn cli_gauge_decreases_and_monotonic_maxima_keep_endpoint_values() {
    let mut before = attempts(5);
    before.clients[0].live = 2;
    before.clients[0].http[0].attempts_inflight = 3;
    before.clients[0].http[0].bodies_inflight = 2;
    before.clients[0].http[0].dispatch_max_ns = 100;
    before.clients[0].http[0].body_max_ns = 90;
    before.bundles.max_ns = 80;
    before.cache.payload_bytes = 512;
    let mut after = attempts(6);
    after.clients[0].live = 1;
    after.clients[0].http[0].dispatch_max_ns = 170;
    after.clients[0].http[0].body_max_ns = 160;
    after.bundles.max_ns = 150;
    after.cache.payload_bytes = 256;
    let result = project_cli(
        &cli_binding(),
        &pair(CaptureContext::Periodic, before, after),
    );
    assert_eq!(result["status"], "observed");
    let window = &result["contexts"]["periodic"]["window"];
    assert_eq!(
        window["clients"][0]["gauges"]["live"],
        json!({"before":2,"after":1})
    );
    assert_eq!(
        window["clients"][0]["http"][0]["gauges"]["attempts_inflight"],
        json!({"before":3,"after":0})
    );
    assert_eq!(
        window["clients"][0]["http"][0]["gauges"]["bodies_inflight"],
        json!({"before":2,"after":0})
    );
    assert_eq!(
        window["clients"][0]["http"][0]["maxima"]["dispatch_max_ns"],
        json!({"before":100,"after":170})
    );
    assert_eq!(
        window["clients"][0]["http"][0]["maxima"]["body_max_ns"],
        json!({"before":90,"after":160})
    );
    assert_eq!(
        window["bundles"]["maxima"]["max_ns"],
        json!({"before":80,"after":150})
    );
    assert_eq!(
        window["cache"]["gauges"]["payload_bytes"],
        json!({"before":512,"after":256})
    );
    assert!(
        window["clients"][0]["http"][0]["counters"]
            .get("attempts_inflight")
            .is_none()
    );
    assert!(window["cache"]["counters"].get("payload_bytes").is_none());
}

#[test]
fn cli_saturation_refuses_windows_but_concurrent_and_cache_quality_are_retained() {
    let clean = attempts(1);
    let mut saturated = attempts(2);
    saturated.saturated = true;
    refused(
        &project_cli(
            &cli_binding(),
            &pair(CaptureContext::Periodic, clean, saturated),
        ),
        "incomplete_quality",
    );
    for (before_concurrent, after_concurrent, before_unknown, after_unknown) in [
        (true, false, 0, 0),
        (false, true, 0, 0),
        (false, false, 2, 0),
        (false, false, 0, 3),
    ] {
        let mut before = attempts(1);
        before.concurrent_activity = before_concurrent;
        before.cache.unknown_live = before_unknown;
        before.cache.payload_bytes = 512;
        let mut after = attempts(2);
        after.concurrent_activity = after_concurrent;
        after.cache.unknown_live = after_unknown;
        after.cache.payload_bytes = 256;
        let result = project_cli(
            &cli_binding(),
            &pair(CaptureContext::Periodic, before, after),
        );
        assert_eq!(result["status"], "observed");
        let history = &result["contexts"]["periodic"];
        let window = &history["window"];
        assert_eq!(window["status"], "observed");
        assert_eq!(
            window["clients"][0]["http"][0]["counters"]["attempts_started"].as_u64(),
            Some(1)
        );
        assert_eq!(
            window["quality"],
            json!({
            "before_concurrent_activity":before_concurrent,
            "after_concurrent_activity":after_concurrent,
            "cache_totals_complete":before_unknown == 0 && after_unknown == 0,
            "capture_atomic":false,"application_drain_proven":false})
        );
        assert_eq!(
            history["quality"],
            json!({
            "any_concurrent_activity":before_concurrent || after_concurrent,
            "cache_totals_complete":before_unknown == 0 && after_unknown == 0,
            "capture_atomic":false,"application_drain_proven":false})
        );
        assert_eq!(
            history["first"]["quality"],
            json!({"saturated":false,
            "concurrent_activity":before_concurrent,"cache_unknown_live":before_unknown})
        );
        assert_eq!(
            history["last"]["quality"],
            json!({"saturated":false,
            "concurrent_activity":after_concurrent,"cache_unknown_live":after_unknown})
        );
        assert_eq!(
            window["cache"]["gauges"]["unknown_live"],
            json!({"before":before_unknown,"after":after_unknown})
        );
        assert_eq!(
            window["cache"]["gauges"]["payload_bytes"],
            json!({"before":512,"after":256})
        );
        assert!(window["cache"]["counters"].get("unknown_live").is_none());
    }
    let mut middle = attempts(2);
    middle.concurrent_activity = true;
    middle.cache.unknown_live = 7;
    let mut bytes = encoded(identity(321, 0, CaptureContext::Periodic), clean);
    bytes.extend(encoded(identity(321, 1, CaptureContext::Periodic), middle));
    bytes.extend(encoded(
        identity(321, 2, CaptureContext::Periodic),
        attempts(3),
    ));
    let result = project_cli(&cli_binding(), &bytes);
    assert_eq!(result["status"], "observed");
    let history = &result["contexts"]["periodic"];
    assert_eq!(history["samples"].as_u64(), Some(3));
    assert_eq!(
        history["quality"],
        json!({"any_concurrent_activity":true,
        "cache_totals_complete":false,"capture_atomic":false,"application_drain_proven":false})
    );
    assert_eq!(history["window"]["status"], "observed");
    assert_eq!(
        history["window"]["clients"][0]["http"][0]["counters"]["attempts_started"].as_u64(),
        Some(2)
    );
    assert_eq!(
        history["window"]["quality"],
        json!({"before_concurrent_activity":false,
        "after_concurrent_activity":false,"cache_totals_complete":true,
        "capture_atomic":false,"application_drain_proven":false})
    );
}

#[test]
fn cli_existing_output_caps_utf8_and_terminal_lf_remain_required() {
    let complete = encoded(
        identity(321, 0, CaptureContext::Periodic),
        Snapshot::default(),
    );
    let mut no_lf = complete.clone();
    no_lf.pop();
    let file_cap = b"x\n".repeat(4 * 1024 * 1024 + 1);
    let mut line_cap = vec![b'x'; 16 * 1024 + 1];
    line_cap.push(b'\n');
    for bytes in [file_cap, line_cap, vec![0xff, b'\n'], no_lf] {
        refused(&project_cli(&cli_binding(), &bytes), "invalid_output");
    }
}

#[test]
fn cli_missing_frames_and_incomplete_owner_never_export_zero() {
    refused(
        &project_cli(&cli_binding(), b"legacy line\n"),
        "disabled_or_missing",
    );
    let mut owner = cli_binding();
    owner.owner_complete = false;
    let complete = encoded(
        identity(321, 0, CaptureContext::Periodic),
        Snapshot::default(),
    );
    refused(&project_cli(&owner, &complete), "invalid_owner");
    owner = cli_binding();
    owner.pid = 0;
    refused(&project_cli(&owner, &complete), "invalid_owner");
}

#[test]
fn worker_disabled_capture_skips_identity_snapshot_sequence_and_writes() {
    let sequence = AtomicU64::new(19);
    let identities = Cell::new(0);
    let snapshots = Cell::new(0);
    let mut output = CountWrites::default();
    let captured = capture_worker(
        false,
        &sequence,
        |_| {
            identities.set(identities.get() + 1);
            panic!("disabled identity or clock callback");
        },
        || {
            snapshots.set(snapshots.get() + 1);
            panic!("disabled snapshot callback");
        },
        &mut output,
    );
    assert!(captured.sample.is_none());
    refused(&captured.evidence, "disabled_or_missing");
    assert_eq!(sequence.load(Ordering::Relaxed), 19);
    assert_eq!(
        (
            identities.get(),
            snapshots.get(),
            output.calls,
            output.bytes.len()
        ),
        (0, 0, 0, 0)
    );
}

#[test]
fn worker_exhausted_capture_cannot_reuse_identity_or_write() {
    let sequence = AtomicU64::new(u64::MAX);
    let mut output = CountWrites::default();
    let captured = capture_worker(
        true,
        &sequence,
        |_| panic!("exhausted identity callback"),
        || panic!("exhausted snapshot callback"),
        &mut output,
    );
    assert!(captured.sample.is_none());
    refused(&captured.evidence, "capture_exhausted");
    assert_eq!(sequence.load(Ordering::Relaxed), u64::MAX);
    assert_eq!((output.calls, output.bytes.len()), (0, 0));
}

#[test]
fn worker_actual_isolated_bank_captures_bind_output_owner_and_window() {
    let observer = Observer::isolated();
    let client = observer.client(ClientRole::QualificationData);
    let residency = observer.cache_residency();
    residency.publish(2, 512);
    client.attempt(HttpMethod::Get, Some(3)).headers(200).eof();
    let sequence = AtomicU64::new(0);
    let pid = std::process::id();
    let mut before_bytes = Vec::new();
    let before = capture_worker(
        true,
        &sequence,
        |value| Some(identity(pid, value, CaptureContext::WorkerBoundary)),
        || observer.snapshot(),
        &mut before_bytes,
    );
    let mut body = client.attempt(HttpMethod::Put, Some(7)).headers(201);
    body.data(7);
    body.eof();
    residency.publish(1, 256);
    drop(client);
    let mut after_bytes = Vec::new();
    let after = capture_worker(
        true,
        &sequence,
        |value| Some(identity(pid, value, CaptureContext::WorkerBoundary)),
        || observer.snapshot(),
        &mut after_bytes,
    );
    for (captured, bytes) in [(&before, &before_bytes), (&after, &after_bytes)] {
        let decoded: Sample = codec::decode(bytes).unwrap();
        assert_eq!(captured.sample.as_ref(), Some(&decoded));
        assert_eq!(captured.evidence["status"], "observed");
        assert_eq!(captured.evidence["capture"], json!(decoded.capture()));
        assert_eq!(captured.evidence["raw_export_sha256"], wire_hash(bytes));
        assert_eq!(
            captured.evidence["quality"],
            json!({"saturated":false,
            "concurrent_activity":false,"cache_unknown_live":0})
        );
    }
    assert_eq!(before.sample.as_ref().unwrap().capture().sequence, 0);
    assert_eq!(after.sample.as_ref().unwrap().capture().sequence, 1);
    let binding = WorkerBinding {
        pid,
        job: 7,
        phase: json!({"name":"isolated-owner-window"}),
    };
    let result = project_worker(&binding, &before, &after);
    assert_eq!(result["status"], "observed");
    assert_eq!(result["owner"]["pid"].as_u64(), Some(pid as u64));
    assert_eq!(result["owner"]["job"].as_u64(), Some(7));
    assert_eq!(result["owner"]["phase"], binding.phase);
    let window = &result["window"];
    assert_eq!(window["status"], "observed");
    assert_eq!(
        window["clients"][2]["http"][2]["counters"]["attempts_started"].as_u64(),
        Some(1)
    );
    assert_eq!(
        window["clients"][2]["http"][2]["counters"]["body_bytes"].as_u64(),
        Some(7)
    );
    assert_eq!(
        window["clients"][2]["http"][0]["counters"]["attempts_started"].as_u64(),
        Some(0)
    );
    assert_eq!(
        window["clients"][2]["gauges"]["live"],
        json!({"before":1,"after":0})
    );
    assert_eq!(
        window["cache"]["gauges"]["payload_bytes"],
        json!({"before":512,"after":256})
    );
    let foreign = WorkerBinding {
        pid: pid + 1,
        job: 7,
        phase: binding.phase.clone(),
    };
    refused(&project_worker(&foreign, &before, &after), "mixed_window");
}

#[test]
fn worker_mixed_capture_pid_context_generation_or_order_is_rejected() {
    let pid = std::process::id();
    let base = identity(pid, 1, CaptureContext::WorkerBoundary);
    let before = held(base, attempts(1));
    let binding = WorkerBinding {
        pid,
        job: 3,
        phase: json!({"name":"binding-control"}),
    };
    for capture in [
        Capture {
            pid: pid + 1,
            sequence: 2,
            ..base
        },
        Capture {
            context: CaptureContext::Periodic,
            sequence: 2,
            ..base
        },
        Capture {
            generation: Some(3),
            sequence: 2,
            ..base
        },
        base,
        Capture {
            sequence: 0,
            ..base
        },
    ] {
        let after = held(capture, attempts(2));
        refused(&project_worker(&binding, &before, &after), "mixed_window");
    }
}

#[test]
fn worker_reset_and_saturation_refuse_windows_but_quality_is_retained() {
    let pid = std::process::id();
    let before = held(
        identity(pid, 1, CaptureContext::WorkerBoundary),
        attempts(2),
    );
    let binding = WorkerBinding {
        pid,
        job: 5,
        phase: json!({"name":"worker-quality-control"}),
    };
    let reset = held(
        identity(pid, 2, CaptureContext::WorkerBoundary),
        attempts(1),
    );
    refused(&project_worker(&binding, &before, &reset), "counter_reset");
    let mut saturated = attempts(3);
    saturated.saturated = true;
    let saturated_after = held(identity(pid, 2, CaptureContext::WorkerBoundary), saturated);
    refused(
        &project_worker(&binding, &before, &saturated_after),
        "incomplete_quality",
    );
    for (before_concurrent, after_concurrent, before_unknown, after_unknown) in [
        (true, false, 0, 0),
        (false, true, 0, 0),
        (false, false, 2, 0),
        (false, false, 0, 3),
    ] {
        let mut before_snapshot = attempts(2);
        before_snapshot.concurrent_activity = before_concurrent;
        before_snapshot.cache.unknown_live = before_unknown;
        before_snapshot.cache.payload_bytes = 512;
        let mut after_snapshot = attempts(3);
        after_snapshot.concurrent_activity = after_concurrent;
        after_snapshot.cache.unknown_live = after_unknown;
        after_snapshot.cache.payload_bytes = 256;
        let before = held(
            identity(pid, 1, CaptureContext::WorkerBoundary),
            before_snapshot,
        );
        let after = held(
            identity(pid, 2, CaptureContext::WorkerBoundary),
            after_snapshot,
        );
        let result = project_worker(&binding, &before, &after);
        assert_eq!(result["status"], "observed");
        let window = &result["window"];
        assert_eq!(window["status"], "observed");
        assert_eq!(
            window["clients"][0]["http"][0]["counters"]["attempts_started"].as_u64(),
            Some(1)
        );
        assert_eq!(
            window["quality"],
            json!({
            "before_concurrent_activity":before_concurrent,
            "after_concurrent_activity":after_concurrent,
            "cache_totals_complete":before_unknown == 0 && after_unknown == 0,
            "capture_atomic":false,"application_drain_proven":false})
        );
        assert_eq!(
            result["before"]["quality"],
            json!({"saturated":false,
            "concurrent_activity":before_concurrent,"cache_unknown_live":before_unknown})
        );
        assert_eq!(
            result["after"]["quality"],
            json!({"saturated":false,
            "concurrent_activity":after_concurrent,"cache_unknown_live":after_unknown})
        );
        assert_eq!(
            window["cache"]["gauges"]["unknown_live"],
            json!({"before":before_unknown,"after":after_unknown})
        );
        assert_eq!(
            window["cache"]["gauges"]["payload_bytes"],
            json!({"before":512,"after":256})
        );
        assert!(window["cache"]["counters"].get("unknown_live").is_none());
    }
}

// Private additional control, appended to the existing projection test module.
// Authored source only; not compiled or executed here.
#[test]
fn worker_reserved_sequence_rejects_callback_identity_drift_before_snapshot_or_write() {
    let sequence = std::sync::atomic::AtomicU64::new(7);
    let identities = std::cell::Cell::new(0);
    let snapshots = std::cell::Cell::new(0);
    let mut output = CountWrites::default();
    let captured = capture_worker(
        true,
        &sequence,
        |reserved| {
            identities.set(identities.get() + 1);
            assert_eq!(reserved, 7);
            Some(mount_rs_service::object_store_diagnostics::Capture {
                pid: std::process::id(),
                sequence: 8,
                observed_unix_ms: 107,
                context: mount_rs_service::object_store_diagnostics::CaptureContext::WorkerBoundary,
                generation: None,
            })
        },
        || {
            snapshots.set(snapshots.get() + 1);
            panic!("identity drift reached snapshot callback");
        },
        &mut output,
    );
    assert!(captured.sample.is_none());
    assert_eq!(captured.evidence["status"], "unavailable");
    assert_eq!(captured.evidence["reason"], "capture_identity");
    assert_eq!(identities.get(), 1);
    assert_eq!(snapshots.get(), 0);
    assert_eq!(sequence.load(std::sync::atomic::Ordering::SeqCst), 8);
    assert_eq!(output.calls, 0);
    assert!(output.bytes.is_empty());
}

fn held_client_build(capture: Capture, builds: &[Value; 6]) -> Captured {
    let original = encoded(capture, Snapshot::default());
    let mut records: Vec<Value> = original
        .split_inclusive(|byte| *byte == b'\n')
        .map(|line| {
            assert!(line.starts_with(codec::PREFIX));
            assert_eq!(line.last(), Some(&b'\n'));
            serde_json::from_slice(&line[codec::PREFIX.len()..]).unwrap()
        })
        .collect();
    assert_eq!(records.len(), 7);
    for (record, build) in records.iter_mut().take(6).zip(builds) {
        assert_eq!(record["kind"], "http_role");
        record["snapshot"]["build"] = build.clone();
    }
    let mut bytes = Vec::new();
    for record in records {
        let row = serde_json::to_vec(&record).unwrap();
        assert!(codec::PREFIX.len() + row.len() < codec::RECORD_LIMIT);
        bytes.extend_from_slice(codec::PREFIX);
        bytes.extend_from_slice(&row);
        bytes.push(b'\n');
    }
    let sample = codec::decode(&bytes)
        .expect("complete per-role client build rows must decode before projection");
    assert_eq!(sample.capture(), &capture);
    Captured {
        sample: Some(sample),
        evidence: json!({"status":"observed","capture":capture,
            "raw_export_sha256":wire_hash(&bytes),
            "quality":{"saturated":false,"concurrent_activity":false,"cache_unknown_live":0}}),
    }
}

fn client_build_rows(after: bool) -> [Value; 6] {
    std::array::from_fn(|role| {
        let role = role as u64;
        let delta = if after { role + 1 } else { 0 };
        let started = if role == 5 {
            u64::MAX - u64::from(!after)
        } else {
            1_000 + role * 100 + delta
        };
        json!({
            "started":started, "inflight":if after { 3 + role } else { 10 + role },
            "succeeded":100 + role * 10 + delta * 2,
            "failed":20 + role + delta * 3,
            "abandoned":10 + role + delta * 4,
            "elapsed_ns":5_000 + role * 100 + delta * 5,
            "max_ns":if after { 200 + role } else { 100 + role },
        })
    })
}

#[test]
fn worker_client_build_windows_preserve_deltas_gauges_maxima_and_reject_resets() {
    let pid = std::process::id();
    let before_rows = client_build_rows(false);
    let before = held_client_build(
        identity(pid, 1, CaptureContext::WorkerBoundary),
        &before_rows,
    );
    let after = held_client_build(
        identity(pid, 2, CaptureContext::WorkerBoundary),
        &client_build_rows(true),
    );
    let binding = WorkerBinding {
        pid,
        job: 7,
        phase: json!({"name":"client-build-window"}),
    };
    let result = project_worker(&binding, &before, &after);
    assert_eq!(result["status"], "observed");
    assert_eq!(result["owner"]["pid"].as_u64(), Some(pid as u64));
    assert_eq!(result["owner"]["job"].as_u64(), Some(7));
    assert_eq!(result["owner"]["phase"], binding.phase);
    assert_eq!(result["before"], before.evidence);
    assert_eq!(result["after"], after.evidence);
    let clients = result["window"]["clients"].as_array().unwrap();
    assert_eq!(clients.len(), 6);
    for (role, client) in clients.iter().enumerate() {
        let role_value = role as u64;
        let delta = role_value + 1;
        assert_eq!(client["role"], ROLES[role]);
        assert_eq!(
            client["build"],
            json!({
                "counters":{"started":if role == 5 { 1 } else { delta },
                    "succeeded":delta * 2,"failed":delta * 3,"abandoned":delta * 4,
                    "elapsed_ns":delta * 5},
                "gauges":{"inflight":{"before":10 + role_value,"after":3 + role_value}},
                "maxima":{"max_ns":{"before":100 + role_value,"after":200 + role_value}},
            })
        );
        assert_eq!(client["http"].as_array().unwrap().len(), 6);
        assert_eq!(
            client["http"][0]["counters"]["attempts_started"].as_u64(),
            Some(0)
        );
    }
    for role in 0..6 {
        for field in [
            "started",
            "succeeded",
            "failed",
            "abandoned",
            "elapsed_ns",
            "max_ns",
        ] {
            let mut after_rows = client_build_rows(true);
            after_rows[role][field] = json!(before_rows[role][field].as_u64().unwrap() - 1);
            let after = held_client_build(
                identity(pid, 2, CaptureContext::WorkerBoundary),
                &after_rows,
            );
            let result = project_worker(&binding, &before, &after);
            refused(&result, "counter_reset");
            assert!(
                result.get("window").is_none(),
                "reset {field} at role {role} published a window"
            );
            assert_eq!(result["before"], before.evidence);
            assert_eq!(result["after"], after.evidence);
        }
    }
}
