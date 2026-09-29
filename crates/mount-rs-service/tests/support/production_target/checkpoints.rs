//! Concurrent diagnostic checkpoints. These never qualify strict boundaries.

use mount_rs_core::diagnostics::{object_store, profile, storage};
use mount_rs_service::object_store_diagnostics as codec;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

pub(super) const RECEIPT_LIMIT: usize = 512 * 1024;
const RECORD_LIMIT: usize = 8 * 1024;
const OUTPUT_BUDGET: usize = 32 * 1024;
const EXHAUSTION_RESERVE: usize = 512;
const HANDOFF_GRACE_MS: u64 = 10_000;
const CORE_ROWS: [&str; 8] = [
    "filesystem.snapshot_nodes",
    "compact.namespace.materialize_nodes",
    "compact.structure.delta_capture_nodes",
    "compact.structure.expected_guard_nodes",
    "provider.compact_anchor_returned_bytes",
    "provider.compact_anchor_serialized_bytes",
    "provider.inode_returned_bytes",
    "provider.inode_serialized_bytes",
];
const STORAGE_ROWS: [&str; 4] = [
    "sdk.metadata.load_compact_snapshot",
    "sdk.metadata.publish_compact_structure",
    "tidb.sql.inode_read",
    "tidb.sql.inode_write",
];

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(super) struct Identity {
    pub pid: u32,
    pub controller_pid: u32,
    pub worker: usize,
    pub generation: u64,
    pub source_digest: String,
    pub binary_digest: String,
}
impl Identity {
    pub fn same_process(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.controller_pid == other.controller_pid
            && self.worker == other.worker
            && self.source_digest == other.source_digest
            && self.binary_digest == other.binary_digest
    }
    pub fn valid(&self) -> bool {
        let digest = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        self.pid != 0
            && self.controller_pid != 0
            && self.worker < super::config::SERVERS
            && digest(&self.source_digest)
            && digest(&self.binary_digest)
    }
}
#[derive(Clone)]
pub(super) struct Setup {
    pub identity: Identity,
    pub scope: &'static str,
}

fn capture(
    identity: &Identity,
    scope: &str,
    sequence: u64,
    resources: &Value,
) -> Result<Value, &'static str> {
    let started = super::utc_ms();
    // Exactly one actual capture of each bank; no runtime/provider callbacks.
    let core = profile::snapshot();
    let storage = storage::snapshot();
    let observer = object_store::Observer::enabled();
    let object = codec::capture(
        observer.is_enabled(),
        codec::Capture {
            pid: identity.pid,
            sequence,
            observed_unix_ms: started,
            context: codec::CaptureContext::Periodic,
            generation: Some(identity.generation),
        },
        || observer.snapshot(),
    )
    .map_err(|_| "object_store_capture_invalid")?;
    let object_store = match object {
        Some(sample) => {
            let mut bytes = BoundedBuffer::new(codec::FRAME_COUNT * codec::RECORD_LIMIT);
            sample
                .write(&mut bytes)
                .map_err(|_| "object_store_encoding_failed")?;
            let text =
                std::str::from_utf8(&bytes.bytes).map_err(|_| "object_store_encoding_failed")?;
            json!({"available":true,"records":text.split_inclusive('\n').collect::<Vec<_>>()})
        }
        None => json!({"available":false,"records":[],"reason":"profiling_disabled"}),
    };
    Ok(json!({
        "schema":"mount-rs.target-checkpoint.v1", "observation":"concurrent_partial",
        "counter_scope":"process_lifetime_cumulative", "metrics_complete":false,
        "available":true, "reason":null, "identity_scope":scope,
        "identity":{"pid":identity.pid,"controller_pid":identity.controller_pid,"worker":identity.worker,
            "generation":identity.generation,"sequence":sequence,"source_digest":identity.source_digest,"binary_digest":identity.binary_digest},
        "capture_started_unix_ms":started,"capture_completed_unix_ms":super::utc_ms(),
        "core":core,"storage":storage,"object_store_observation":object_store,"resources":resources,
        "scope":"serial atomic loads during concurrent work; logical calls/rows/decoded bytes; process CPU includes observer/background work; not wire bytes, physical IOPS, a transactional cut or a strict phase boundary"
    }))
}

pub(super) struct Sampler {
    sequence: u64,
    next: Instant,
    previous: Option<Value>,
    failure: Option<&'static str>,
    buffer: BoundedBuffer,
}
impl Sampler {
    pub fn new() -> Self {
        Self {
            sequence: 0,
            next: Instant::now(),
            previous: None,
            failure: None,
            buffer: BoundedBuffer::new(RECEIPT_LIMIT),
        }
    }
    pub fn tick(
        &mut self,
        setup: &Mutex<Option<Setup>>,
        path: &Path,
        resources: &Value,
        terminal: bool,
    ) {
        self.tick_with_capture(setup, path, resources, terminal, capture);
    }
    fn tick_with_capture(
        &mut self,
        setup: &Mutex<Option<Setup>>,
        path: &Path,
        resources: &Value,
        terminal: bool,
        capture: impl FnOnce(&Identity, &str, u64, &Value) -> Result<Value, &'static str>,
    ) {
        if !terminal && Instant::now() < self.next {
            return;
        }
        self.next = Instant::now() + Duration::from_secs(1);
        let Some(context) = setup.lock().unwrap().clone() else {
            return;
        };
        let Some(sequence) = self.sequence.checked_add(1) else {
            self.failure = Some("sequence_exhausted");
            return;
        };
        self.sequence = sequence;
        let span = super::metrics::observer().begin("metric_capture");
        let sample = capture(&context.identity, context.scope, sequence, resources);
        let Ok(mut value) = sample else {
            self.failure.get_or_insert("capture_failed");
            span.finish(false, 0);
            return;
        };
        let current_context = setup.lock().unwrap().clone();
        if current_context.as_ref().is_some_and(|now| {
            now.identity.same_process(&context.identity)
                && now.identity.generation > context.identity.generation
                && now.scope == context.scope
        }) {
            // A normal context transition rejects this one capture. Keep the
            // last valid file and high-water sample, charge its sequence, and
            // wait for the next scheduled tick without retrying the banks.
            span.finish(false, 0);
            return;
        }
        if current_context
            .as_ref()
            .is_none_or(|now| now.identity != context.identity || now.scope != context.scope)
        {
            self.failure.get_or_insert("checkpoint_identity_mismatch");
        }
        if let Some(previous) = &self.previous
            && let Err(reason) = follows(&value, previous)
        {
            self.failure.get_or_insert(reason);
        }
        if let Some(reason) = self.failure {
            value["available"] = json!(false);
            value["reason"] = json!(reason);
        } else {
            self.previous = Some(value.clone());
        }
        span.finish(self.failure.is_none(), 0);
        if publish(path, &value, &mut self.buffer).is_err() {
            self.failure.get_or_insert("publication_failed");
        }
    }
}

struct BoundedBuffer {
    bytes: Vec<u8>,
    limit: usize,
}
impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit),
            limit,
        }
    }
}
impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("checkpoint byte budget"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn publish(path: &Path, value: &Value, buffer: &mut BoundedBuffer) -> Result<(), &'static str> {
    use std::os::unix::fs::OpenOptionsExt;
    let span = super::metrics::observer().begin("receipt_publication");
    let mut attempted_bytes = 0;
    let result = (|| {
        buffer.bytes.clear();
        serde_json::to_writer(&mut *buffer, value).map_err(|_| "checkpoint_encoding_failed")?;
        let pending = path.with_extension("pending");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&pending)
            .map_err(|_| "checkpoint_publication_failed")?;
        if !file
            .metadata()
            .map_err(|_| "checkpoint_publication_failed")?
            .is_file()
        {
            return Err("checkpoint_publication_failed");
        }
        // Bytes offered to the owned file write, including a partial failure;
        // encoding/open refusal offers zero bytes. This is not accepted I/O.
        attempted_bytes = buffer.bytes.len() as u64;
        let result = file
            .write_all(&buffer.bytes)
            .map_err(|_| "checkpoint_publication_failed")
            .and_then(|()| {
                std::fs::rename(&pending, path).map_err(|_| "checkpoint_publication_failed")
            });
        if result.is_err() {
            // Only remove the pending file successfully created by this call.
            let _ = std::fs::remove_file(&pending);
        }
        result
    })();
    span.finish(result.is_ok(), attempted_bytes);
    result
}
pub(super) fn read_latest(path: &Path) -> Result<Option<Value>, &'static str> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("checkpoint_invalid"),
    };
    let metadata = file.metadata().map_err(|_| "checkpoint_invalid")?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("checkpoint_invalid");
    }
    let mut bytes = Vec::with_capacity(RECEIPT_LIMIT + 1);
    file.take((RECEIPT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "checkpoint_invalid")?;
    if bytes.len() > RECEIPT_LIMIT {
        return Err("checkpoint_invalid");
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "checkpoint_invalid")
}

fn number(value: &Value) -> Result<u64, &'static str> {
    value.as_u64().ok_or("checkpoint_invalid")
}
fn object_sample(value: &Value) -> Result<Option<codec::Sample>, &'static str> {
    let observation = &value["object_store_observation"];
    let records = observation["records"]
        .as_array()
        .ok_or("checkpoint_invalid")?;
    if observation["available"] == false && records.is_empty() {
        return Ok(None);
    }
    if observation["available"] != true || records.len() != codec::FRAME_COUNT {
        return Err("checkpoint_invalid");
    }
    let mut bytes = BoundedBuffer::new(codec::FRAME_COUNT * codec::RECORD_LIMIT);
    for record in records {
        let record = record.as_str().ok_or("checkpoint_invalid")?;
        if record.len() > codec::RECORD_LIMIT {
            return Err("checkpoint_invalid");
        }
        bytes
            .write_all(record.as_bytes())
            .map_err(|_| "checkpoint_invalid")?;
    }
    codec::decode(&bytes.bytes)
        .map(Some)
        .map_err(|_| "checkpoint_invalid")
}

// Parent-side schema initialization is separate from measured worker captures.
// Disabled projection never calls this initializer.
fn core_names() -> &'static Vec<&'static str> {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        profile::snapshot()
            .entries
            .into_iter()
            .map(|row| row.name)
            .collect()
    })
}
fn validate(value: &Value, expected: &Identity, published: u64) -> Result<(), &'static str> {
    if !expected.valid()
        || value["schema"] != "mount-rs.target-checkpoint.v1"
        || value["observation"] != "concurrent_partial"
        || value["counter_scope"] != "process_lifetime_cumulative"
        || value["metrics_complete"] != false
        || value["available"] != true
        || !value["reason"].is_null()
    {
        return Err("checkpoint_invalid");
    }
    let expected_value = serde_json::to_value(expected).map_err(|_| "checkpoint_invalid")?;
    for (name, expected) in expected_value.as_object().unwrap() {
        if value["identity"].get(name) != Some(expected) {
            return Err("checkpoint_identity_mismatch");
        }
    }
    if number(&value["identity"]["sequence"])? == 0 {
        return Err("checkpoint_invalid");
    }
    let started = number(&value["capture_started_unix_ms"])?;
    let completed = number(&value["capture_completed_unix_ms"])?;
    if started == 0 || started > completed || completed > published || published - completed > 10000
    {
        return Err("checkpoint_stale");
    }
    super::resources::validate_sample(&value["resources"], expected.pid, published)
        .map_err(|_| "checkpoint_resource_invalid")?;
    for (family, names, fields) in [
        (
            "core",
            core_names().as_slice(),
            &["calls", "elapsed_ns", "units"][..],
        ),
        (
            "storage",
            storage::operation_names(),
            &[
                "in_flight",
                "returned_rows",
                "returned_row_observations",
                "calls",
                "success",
                "error",
                "cancelled",
                "bytes",
                "elapsed_ns",
            ][..],
        ),
    ] {
        let rows = value[family]["entries"]
            .as_array()
            .ok_or("checkpoint_invalid")?;
        if rows.len() != names.len() {
            return Err("checkpoint_shape_changed");
        }
        for (row, name) in rows.iter().zip(names) {
            if row["name"] != *name {
                return Err("checkpoint_shape_changed");
            }
            for field in fields {
                number(&row[*field])?;
            }
            let extra = if family == "storage" {
                let histogram = row["latency_log2_us"]
                    .as_array()
                    .ok_or("checkpoint_invalid")?;
                if histogram.len() != 32 {
                    return Err("checkpoint_shape_changed");
                }
                for bucket in histogram {
                    number(bucket)?;
                }
                2
            } else {
                1
            };
            if row.as_object().ok_or("checkpoint_invalid")?.len() != fields.len() + extra {
                return Err("checkpoint_shape_changed");
            }
        }
    }
    number(&value["storage"]["in_flight"])?;
    if let Some(sample) = object_sample(value)? {
        let identity = sample.capture();
        if identity.pid != expected.pid
            || identity.generation != Some(expected.generation)
            || identity.sequence != number(&value["identity"]["sequence"])?
            || identity.observed_unix_ms != started
            || identity.context != codec::CaptureContext::Periodic
        {
            return Err("checkpoint_identity_mismatch");
        }
    }
    Ok(())
}
fn monotonic(now: &Value, before: &Value, field: &str) -> Result<(), &'static str> {
    if matches!(
        field,
        "in_flight"
            | "inflight"
            | "attempts_inflight"
            | "bodies_inflight"
            | "builds_inflight"
            | "live"
            | "resident_entries"
            | "payload_bytes"
            | "unknown_live"
            | "concurrent_activity"
            | "saturated"
    ) {
        return Ok(());
    }
    match (now, before) {
        (Value::Number(_), Value::Number(_)) if number(now)? >= number(before)? => Ok(()),
        (Value::String(a), Value::String(b)) if a == b => Ok(()),
        (Value::Bool(a), Value::Bool(b)) if a == b => Ok(()),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (a, b) in a.iter().zip(b) {
                monotonic(a, b, field)?;
            }
            Ok(())
        }
        (Value::Object(a), Value::Object(b)) if a.keys().eq(b.keys()) => {
            for (key, a) in a {
                monotonic(a, &b[key], key)?;
            }
            Ok(())
        }
        _ => Err("checkpoint_counter_regression"),
    }
}
fn follows(value: &Value, previous: &Value) -> Result<(), &'static str> {
    for key in [
        "pid",
        "controller_pid",
        "worker",
        "source_digest",
        "binary_digest",
    ] {
        if value["identity"][key] != previous["identity"][key] {
            return Err("checkpoint_identity_mismatch");
        }
    }
    if number(&value["identity"]["sequence"])? <= number(&previous["identity"]["sequence"])?
        || number(&value["identity"]["generation"])? < number(&previous["identity"]["generation"])?
        || number(&value["capture_started_unix_ms"])?
            < number(&previous["capture_started_unix_ms"])?
    {
        return Err("checkpoint_sequence_regression");
    }
    for family in ["core", "storage"] {
        monotonic(&value[family], &previous[family], family)?;
    }
    for field in ["cpu_user_us", "cpu_system_us"] {
        monotonic(
            &value["resources"]["process_delta"][field],
            &previous["resources"]["process_delta"][field],
            field,
        )?;
    }
    monotonic(
        &value["resources"]["samples"],
        &previous["resources"]["samples"],
        "samples",
    )?;
    match (object_sample(value)?, object_sample(previous)?) {
        (Some(now), Some(before)) => monotonic(
            &serde_json::to_value(now.snapshot()).unwrap(),
            &serde_json::to_value(before.snapshot()).unwrap(),
            "object_store",
        )?,
        (None, None) => {}
        _ => return Err("checkpoint_coverage_changed"),
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct Projection {
    previous: Option<Value>,
    pending: Option<Value>,
    expected_context: Option<Identity>,
    waiting_since: Option<u64>,
    bytes: usize,
    exhausted: bool,
    failure: Option<&'static str>,
}
impl Projection {
    fn wait_for_context(&mut self, published: u64) -> Result<Option<Value>, &'static str> {
        let started = *self.waiting_since.get_or_insert(published);
        if published < started || published - started > HANDOFF_GRACE_MS {
            Err("checkpoint_handoff_expired")
        } else {
            Ok(None)
        }
    }

    fn observe(
        &mut self,
        sample: Result<Option<Value>, &'static str>,
        expected: &Identity,
        published: u64,
    ) -> Result<Option<Value>, &'static str> {
        if !expected.valid() || published == 0 {
            return Err("checkpoint_identity_mismatch");
        }
        if self.expected_context.as_ref().is_some_and(|previous| {
            !expected.same_process(previous) || expected.generation < previous.generation
        }) {
            return Err("checkpoint_identity_mismatch");
        }
        self.expected_context = Some(expected.clone());
        let Some(value) = sample.map_err(|_| "checkpoint_invalid")? else {
            if self.previous.as_ref().is_none_or(|previous| {
                previous["identity"]["generation"].as_u64() != Some(expected.generation)
            }) {
                return self.wait_for_context(published);
            }
            return Err("checkpoint_missing");
        };
        // Validate every invariant and the actual capture context before
        // considering a generation mismatch transient. The parent can lag or
        // lead the independently published sampler file during a handoff.
        let mut captured_identity = expected.clone();
        captured_identity.generation = number(&value["identity"]["generation"])?;
        validate(&value, &captured_identity, published)?;
        if let Some(high_water) = self.pending.as_ref().or(self.previous.as_ref())
            && value != *high_water
        {
            follows(&value, high_water)?;
        }
        if captured_identity.generation != expected.generation {
            if self.previous.as_ref() != Some(&value) {
                self.pending = Some(value);
            }
            return self.wait_for_context(published);
        }
        if let Some(started) = self.waiting_since
            && (published < started || published - started > HANDOFF_GRACE_MS)
        {
            return Err("checkpoint_handoff_expired");
        }
        self.waiting_since = None;
        self.pending = None;
        if self.previous.as_ref() == Some(&value) {
            return Ok(None);
        }
        self.previous = Some(value.clone());
        Ok(Some(value))
    }

    pub fn write(
        &mut self,
        sample: Result<Option<Value>, &'static str>,
        expected: &Identity,
        phase: &str,
        published: u64,
        writer: &mut impl Write,
    ) -> bool {
        if self.exhausted {
            return true;
        }
        let accepted = self.observe(sample, expected, published);
        let value = match accepted {
            Ok(None) => return true,
            Ok(Some(value)) => Some(value),
            Err(reason) => {
                self.failure.get_or_insert(reason);
                None
            }
        };
        let object_quality = value.as_ref().and_then(|value| object_sample(value).ok().flatten()).map(|sample| {
            if sample.snapshot().saturated { self.failure.get_or_insert("checkpoint_counter_saturated"); }
            json!({"saturated":sample.snapshot().saturated,"concurrent_activity":sample.snapshot().concurrent_activity})
        });
        let selected = |family: &str, names: &[&str]| -> Vec<Value> {
            names.iter().map(|name| {
                let row = value.as_ref().and_then(|v| v[family]["entries"].as_array()).and_then(|rows| rows.iter().find(|r| r["name"] == *name));
                match (family,row) {
                    ("core",Some(row)) => json!({"name":name,"calls":row["calls"],"elapsed_ns":row["elapsed_ns"],"units":row["units"]}),
                    (_,Some(row)) => json!({"name":name,"calls":row["calls"],"elapsed_ns":row["elapsed_ns"],"returned_rows":row["returned_rows"],"returned_row_observations":row["returned_row_observations"],"bytes":row["bytes"],"in_flight_gauge":row["in_flight"]}),
                    _ => Value::Null,
                }
            }).collect()
        };
        let record = json!({"schema":"mount-rs.checkpoint-progress.v1","identity":expected,
            "sequence":value.as_ref().map(|v|&v["identity"]["sequence"]),"published_unix_ms":published,
            "capture_started_unix_ms":value.as_ref().map(|v|&v["capture_started_unix_ms"]),
            "capture_completed_unix_ms":value.as_ref().map(|v|&v["capture_completed_unix_ms"]),
            "publication_phase_context":phase,"observation":"concurrent_partial","metrics_complete":false,
            "counter_scope":"process_lifetime_cumulative","available":value.is_some(),"diagnostics_complete":self.failure.is_none(),"reason":self.failure,
            "core":selected("core", &CORE_ROWS),"storage":selected("storage",&STORAGE_ROWS),"object_store_quality":object_quality,
            "resource_samples":value.as_ref().map(|v|&v["resources"]["samples"]),
            "cpu_user_us":value.as_ref().map(|v|&v["resources"]["process_delta"]["cpu_user_us"]),
            "cpu_system_us":value.as_ref().map(|v|&v["resources"]["process_delta"]["cpu_system_us"])});
        let mut buffer = BoundedBuffer::new(RECORD_LIMIT);
        let encoded = mount_rs_service::startup::write_bounded::<RECORD_LIMIT>(
            &mut buffer,
            b"\ncheckpoint_progress ",
            &record,
        );
        if encoded.is_err() {
            self.failure.get_or_insert("checkpoint_encoding_failed");
            return false;
        }
        if self.bytes + buffer.bytes.len() > OUTPUT_BUDGET - EXHAUSTION_RESERVE {
            self.exhausted = true;
            buffer.bytes.clear();
            let terminal = json!({"schema":"mount-rs.checkpoint-progress.v1","pid":expected.pid,"worker":expected.worker,"diagnostics_complete":false,"reason":"budget_exhausted","latest_file_continues":true});
            let encoded = mount_rs_service::startup::write_bounded::<EXHAUSTION_RESERVE>(
                &mut buffer,
                b"\ncheckpoint_progress ",
                &terminal,
            );
            if encoded.is_err() {
                return false;
            }
        }
        // Charge attempted bytes even after a partial sink failure; never retry
        // an uncertain write or exceed the original parent stderr allowance.
        self.bytes += buffer.bytes.len();
        let success = writer.write_all(&buffer.bytes).is_ok();
        if !success {
            self.failure.get_or_insert("checkpoint_sink_failed");
        }
        success
    }
    pub fn exhausted(&self) -> bool {
        self.exhausted
    }
}

/// Historical, bounded evidence reader for the owned ignored fixture test.
/// This validates a diagnostic receipt; it never qualifies a phase boundary.
#[cfg(test)]
pub fn retained_checkpoint(path: &Path, expected: &Value) -> Result<Value, &'static str> {
    let identity = Identity {
        pid: number(&expected["pid"])?
            .try_into()
            .map_err(|_| "checkpoint_identity_mismatch")?,
        controller_pid: number(&expected["controller_pid"])?
            .try_into()
            .map_err(|_| "checkpoint_identity_mismatch")?,
        worker: number(&expected["worker"])?
            .try_into()
            .map_err(|_| "checkpoint_identity_mismatch")?,
        generation: number(&expected["generation"])?,
        source_digest: expected["source_digest"]
            .as_str()
            .ok_or("checkpoint_identity_mismatch")?
            .to_owned(),
        binary_digest: expected["binary_digest"]
            .as_str()
            .ok_or("checkpoint_identity_mismatch")?
            .to_owned(),
    };
    let value = read_latest(path)?.ok_or("checkpoint_missing")?;
    if value["identity_scope"] != "verified_worker_source_and_binary" {
        return Err("checkpoint_identity_mismatch");
    }
    let completed = number(&value["capture_completed_unix_ms"])?;
    if completed > super::utc_ms() {
        return Err("checkpoint_stale");
    }
    validate(&value, &identity, completed)?;
    let object = object_sample(&value)?.ok_or("checkpoint_object_store_unavailable")?;
    if object.snapshot().saturated {
        return Err("checkpoint_counter_saturated");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity {
            pid: std::process::id(),
            controller_pid: std::process::id(),
            worker: 0,
            generation: 0,
            source_digest: "a".repeat(64),
            binary_digest: "b".repeat(64),
        }
    }

    fn fixture(sequence: u64) -> Value {
        let mut value = capture(
            &identity(),
            "modeled_identity_sampler_lifecycle_control",
            sequence,
            &super::super::resources::example_sample(std::process::id()),
        )
        .unwrap();
        value["resources"]["samples"] = json!(sequence);
        value["resources"]["process_delta"]["cpu_user_us"] = json!(sequence);
        value["resources"]["process_delta"]["cpu_system_us"] = json!(sequence);
        value
    }

    #[test]
    fn validation_rejects_identity_shape_time_and_counter_regression() {
        let original = fixture(1);
        validate(&original, &identity(), super::super::utc_ms()).unwrap();
        for (pointer, replacement) in [
            ("/identity/pid", json!(0)),
            ("/identity/source_digest", json!("foreign")),
            ("/observation", json!("quiescent")),
            ("/metrics_complete", json!(true)),
            ("/capture_completed_unix_ms", json!(u64::MAX)),
            ("/storage/entries/0/calls", json!(-1)),
        ] {
            let mut invalid = original.clone();
            *invalid.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                validate(&invalid, &identity(), super::super::utc_ms()).is_err(),
                "{pointer}"
            );
        }
        let mut later = fixture(2);
        later["core"]["entries"][0]["calls"] = json!(4);
        let mut earlier = original.clone();
        earlier["core"]["entries"][0]["calls"] = json!(5);
        assert!(follows(&later, &earlier).is_err());
        later["core"]["entries"][0]["calls"] = json!(5);
        later["storage"]["entries"][0]["in_flight"] = json!(0);
        earlier["storage"]["entries"][0]["in_flight"] = json!(10);
        follows(&later, &earlier).unwrap();
    }

    #[test]
    fn projector_has_a_hard_total_budget_and_sticky_failure() {
        let mut projection = Projection::default();
        let mut bytes = Vec::new();
        for sequence in 1..100 {
            projection.write(
                Ok(Some(fixture(sequence))),
                &identity(),
                "online_namespace",
                super::super::utc_ms(),
                &mut bytes,
            );
        }
        assert!(projection.exhausted);
        assert!(bytes.len() <= OUTPUT_BUDGET);
        assert!(
            String::from_utf8(bytes.clone())
                .unwrap()
                .contains("budget_exhausted")
        );
        assert!(
            String::from_utf8(bytes.clone())
                .unwrap()
                .contains("\"available\":true")
        );
        let emitted = bytes.len();
        projection.write(
            Ok(None),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        assert_eq!(bytes.len(), emitted);

        let mut projection = Projection::default();
        projection.write(
            Ok(Some(fixture(1))),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut Vec::new(),
        );
        projection.write(
            Err("PRIVATE_ERROR"),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut Vec::new(),
        );
        let mut output = Vec::new();
        projection.write(
            Ok(Some(fixture(2))),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut output,
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("checkpoint_invalid"));
        assert!(!text.contains("PRIVATE_ERROR"));
    }

    #[test]
    fn bounded_latest_publication_replaces_in_place_and_reader_refuses_unsafe_inputs() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkpoint-latest.json");
        let mut buffer = BoundedBuffer::new(RECEIPT_LIMIT);
        publish(&path, &fixture(1), &mut buffer).unwrap();
        publish(&path, &fixture(2), &mut buffer).unwrap();
        assert_eq!(
            read_latest(&path).unwrap().unwrap()["identity"]["sequence"],
            2
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read_latest(&link).is_err());
        let oversized = root.path().join("oversized");
        std::fs::write(&oversized, vec![b' '; RECEIPT_LIMIT + 1]).unwrap();
        assert!(read_latest(&oversized).is_err());
        let mut tiny = BoundedBuffer::new(8);
        assert!(publish(&path, &fixture(3), &mut tiny).is_err());
        assert_eq!(
            read_latest(&path).unwrap().unwrap()["identity"]["sequence"],
            2
        );
    }

    #[test]
    fn periodic_receipt_cannot_replace_a_strict_boundary_identity() {
        let checkpoint = fixture(1);
        let expected = json!({"pid":std::process::id(),"role":"worker","server":0,
            "controller_pid":std::process::id(),"generation":0,"sequence":1,
            "phase":"online_namespace","boundary":"after","source_digest":"a".repeat(64),
            "binary_digest":"b".repeat(64),"catalog_digest":"c".repeat(64),"backend_prefix":"modeled"});
        assert!(
            super::super::metrics::validate_receipt(&checkpoint["identity"], &expected).is_err()
        );
        assert_eq!(checkpoint["metrics_complete"], false);
        assert!(checkpoint.get("delta_from_previous").is_none());
    }

    #[test]
    fn duplicate_checkpoint_is_normal_but_same_sequence_conflict_is_sticky() {
        let first = fixture(1);
        let mut projection = Projection::default();
        let mut bytes = Vec::new();
        projection.write(
            Ok(Some(first.clone())),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        let first_length = bytes.len();
        projection.write(
            Ok(Some(first.clone())),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        assert_eq!(bytes.len(), first_length);
        assert!(projection.failure.is_none());
        let mut conflict = first.clone();
        conflict["core"]["entries"][0]["calls"] =
            json!(number(&first["core"]["entries"][0]["calls"]).unwrap() + 1);
        projection.write(
            Ok(Some(conflict)),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        assert_eq!(projection.failure, Some("checkpoint_sequence_regression"));
        assert_eq!(projection.previous.as_ref(), Some(&first));
    }

    #[test]
    fn max_width_projection_is_bounded_and_unknown_fields_are_not_exported() {
        let mut value = fixture(1);
        value["private_error"] = json!("PRIVATE_SENTINEL");
        for family in ["core", "storage"] {
            for row in value[family]["entries"].as_array_mut().unwrap() {
                for (key, field) in row.as_object_mut().unwrap() {
                    if key != "name" && key != "latency_log2_us" {
                        *field = json!(u64::MAX);
                    }
                }
            }
        }
        let mut projection = Projection::default();
        let mut bytes = Vec::new();
        assert!(projection.write(
            Ok(Some(value)),
            &identity(),
            "mostly_idle/sequential_overwrite",
            super::super::utc_ms(),
            &mut bytes
        ));
        assert!(bytes.len() <= RECORD_LIMIT);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("18446744073709551615"));
        assert!(text.contains("\"available\":true"));
        assert!(!text.contains("PRIVATE_SENTINEL"));
    }

    #[test]
    fn pending_hardlink_and_fifo_are_preserved_and_refused() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkpoint-latest.json");
        let pending = path.with_extension("pending");
        let protected = root.path().join("protected");
        std::fs::write(&protected, b"UNCHANGED").unwrap();
        std::fs::hard_link(&protected, &pending).unwrap();
        let mut buffer = BoundedBuffer::new(RECEIPT_LIMIT);
        assert!(publish(&path, &fixture(1), &mut buffer).is_err());
        assert_eq!(std::fs::read(&protected).unwrap(), b"UNCHANGED");
        assert!(read_latest(&pending).is_err());
        std::fs::remove_file(&pending).unwrap();
        let name = std::ffi::CString::new(pending.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(publish(&path, &fixture(1), &mut buffer).is_err());
        assert!(read_latest(&pending).is_err());
        assert!(
            std::fs::symlink_metadata(&pending).unwrap().file_type()
                != std::fs::symlink_metadata(&protected).unwrap().file_type()
        );
        assert_eq!(std::fs::read(&protected).unwrap(), b"UNCHANGED");
    }

    #[test]
    fn partial_sink_failure_is_charged_and_cannot_reset_the_output_budget() {
        struct Fail {
            written: usize,
        }
        impl Write for Fail {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.written > 0 {
                    return Err(io::Error::other("PRIVATE_SINK_ERROR"));
                }
                let amount = bytes.len().min(7);
                self.written += amount;
                Ok(amount)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut projection = Projection::default();
        let mut sink = Fail { written: 0 };
        assert!(!projection.write(
            Ok(Some(fixture(1))),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut sink
        ));
        assert!(projection.bytes > sink.written);
        let charged = projection.bytes;
        let mut output = Vec::new();
        projection.write(
            Ok(Some(fixture(2))),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut output,
        );
        assert!(projection.bytes > charged);
        assert_eq!(projection.failure, Some("checkpoint_sink_failed"));
        assert!(
            !String::from_utf8(output)
                .unwrap()
                .contains("PRIVATE_SINK_ERROR")
        );
    }

    #[test]
    fn latest_file_continues_after_log_exhaustion_and_generation_does_not_reset_sequence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkpoint-latest.json");
        let setup = Mutex::new(Some(Setup {
            identity: identity(),
            scope: "modeled_identity_sampler_lifecycle_control",
        }));
        let resources = super::super::resources::example_sample(std::process::id());
        let mut sampler = Sampler::new();
        sampler.tick(&setup, &path, &resources, true);
        let first = read_latest(&path).unwrap().unwrap();
        let mut projection = Projection::default();
        let mut output = Vec::new();
        for sequence in 1..100 {
            projection.write(
                Ok(Some(fixture(sequence))),
                &identity(),
                "online_namespace",
                super::super::utc_ms(),
                &mut output,
            );
        }
        assert!(projection.exhausted());
        let emitted = output.len();
        setup.lock().unwrap().as_mut().unwrap().identity.generation = 1;
        sampler.tick(&setup, &path, &resources, true);
        let second = read_latest(&path).unwrap().unwrap();
        assert_eq!(second["identity"]["generation"], 1);
        assert_eq!(second["identity"]["sequence"], 2);
        assert_eq!(second["available"], true);
        follows(&second, &first).unwrap();
        let mut next_identity = identity();
        next_identity.generation = 1;
        projection.write(
            Ok(Some(second)),
            &next_identity,
            "online_namespace",
            super::super::utc_ms(),
            &mut output,
        );
        assert_eq!(output.len(), emitted);
    }

    #[test]
    fn malformed_object_store_frames_cannot_be_projected_as_an_available_bank() {
        let mut value = fixture(1);
        value["object_store_observation"] =
            json!({"available":true,"records":["PRIVATE_INVALID_FRAME"]});
        let mut projection = Projection::default();
        let mut bytes = Vec::new();
        projection.write(
            Ok(Some(value)),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"available\":false"));
        assert!(!text.contains("PRIVATE_INVALID_FRAME"));
        assert_eq!(projection.failure, Some("checkpoint_invalid"));
    }

    fn generation_fixture(sequence: u64, generation: u64) -> Value {
        let mut owner = identity();
        owner.generation = generation;
        // Re-encode the real object-store capture with the same generation and
        // sequence as the outer receipt, including when profiling is enabled.
        let mut value = capture(
            &owner,
            "modeled_identity_sampler_lifecycle_control",
            sequence,
            &super::super::resources::example_sample(std::process::id()),
        )
        .unwrap();
        value["resources"]["samples"] = json!(sequence);
        value["resources"]["process_delta"]["cpu_user_us"] = json!(sequence);
        value["resources"]["process_delta"]["cpu_system_us"] = json!(sequence);
        value
    }

    #[test]
    fn bootstrap_missing_checkpoint_recovers_without_sticky_failure() {
        let mut projection = Projection::default();
        let mut bytes = Vec::new();
        projection.write(
            Ok(None),
            &identity(),
            "worker_setup",
            super::super::utc_ms(),
            &mut bytes,
        );
        let sample = fixture(1);
        projection.write(
            Ok(Some(sample.clone())),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        assert_eq!(projection.previous.as_ref(), Some(&sample));
        assert!(
            projection.failure.is_none(),
            "ordinary sampler bootstrap became a permanent diagnostic failure"
        );
        assert!(
            String::from_utf8(bytes)
                .unwrap()
                .contains("\"diagnostics_complete\":true")
        );
    }

    #[test]
    fn prior_generation_handoff_waits_without_resetting_cumulative_high_water() {
        let mut projection = Projection::default();
        let mut bytes = Vec::new();
        let first = generation_fixture(1, 0);
        projection.write(
            Ok(Some(first.clone())),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        let charged = projection.bytes;
        let mut next_owner = identity();
        next_owner.generation = 1;
        for _ in 0..3 {
            projection.write(
                Ok(Some(first.clone())),
                &next_owner,
                "online_namespace",
                super::super::utc_ms(),
                &mut bytes,
            );
        }
        assert_eq!(projection.previous.as_ref(), Some(&first));
        assert_eq!(
            projection.bytes, charged,
            "normal duplicate handoff consumed the output budget"
        );
        let second = generation_fixture(2, 1);
        projection.write(
            Ok(Some(second.clone())),
            &next_owner,
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        assert!(
            projection.failure.is_none(),
            "normal generation handoff became sticky failure"
        );
        assert_eq!(projection.previous.as_ref(), Some(&second));
        assert!(projection.bytes > charged);
        follows(&second, &first).unwrap();
    }

    #[test]
    fn bootstrap_and_generation_handoff_have_a_finite_grace_period() {
        let mut missing = Projection::default();
        let mut bytes = Vec::new();
        let started = super::super::utc_ms();
        missing.write(Ok(None), &identity(), "worker_setup", started, &mut bytes);
        assert!(
            missing.failure.is_none(),
            "initial missing receipt must be transient"
        );
        missing.write(
            Ok(None),
            &identity(),
            "worker_setup",
            started + 10_001,
            &mut bytes,
        );
        assert!(
            missing.failure.is_some(),
            "missing checkpoint grace was renewed indefinitely"
        );

        let mut handoff = Projection::default();
        let first = generation_fixture(1, 0);
        let started = super::super::utc_ms();
        handoff.write(
            Ok(Some(first.clone())),
            &identity(),
            "online_namespace",
            started,
            &mut bytes,
        );
        let mut next_owner = identity();
        next_owner.generation = 1;
        handoff.write(
            Ok(Some(first.clone())),
            &next_owner,
            "online_namespace",
            started,
            &mut bytes,
        );
        assert!(handoff.failure.is_none());
        handoff.write(
            Ok(Some(first.clone())),
            &next_owner,
            "online_namespace",
            started + 10_001,
            &mut bytes,
        );
        assert!(
            handoff.failure.is_some(),
            "old generation remained transient after expiry"
        );
        assert_eq!(handoff.previous.as_ref(), Some(&first));
    }

    #[test]
    fn future_generation_checkpoint_waits_for_parent_context_without_poisoning() {
        let mut projection = Projection::default();
        let mut bytes = Vec::new();
        let first = generation_fixture(1, 0);
        projection.write(
            Ok(Some(first.clone())),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        let charged = projection.bytes;
        let future = generation_fixture(2, 1);
        projection.write(
            Ok(Some(future.clone())),
            &identity(),
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        assert!(
            projection.failure.is_none(),
            "sampler publication ahead of parent context became sticky failure"
        );
        assert_eq!(projection.previous.as_ref(), Some(&first));
        assert_eq!(projection.bytes, charged);
        let mut next_owner = identity();
        next_owner.generation = 1;
        projection.write(
            Ok(Some(future.clone())),
            &next_owner,
            "online_namespace",
            super::super::utc_ms(),
            &mut bytes,
        );
        assert!(projection.failure.is_none());
        assert_eq!(projection.previous.as_ref(), Some(&future));
    }

    #[test]
    fn foreign_identity_and_regressions_remain_sticky_during_handoff() {
        for fault in [
            "pid",
            "source",
            "conflict",
            "sequence",
            "counter",
            "generation",
        ] {
            let mut projection = Projection::default();
            let mut bytes = Vec::new();
            let mut owner = identity();
            owner.generation = 1;
            let mut first = generation_fixture(2, 1);
            first["core"]["entries"][0]["calls"] = json!(10);
            projection.write(
                Ok(Some(first.clone())),
                &owner,
                "online_namespace",
                super::super::utc_ms(),
                &mut bytes,
            );
            let mut invalid = generation_fixture(3, 1);
            invalid["core"]["entries"][0]["calls"] = json!(10);
            match fault {
                "pid" => invalid["identity"]["pid"] = json!(0),
                "source" => invalid["identity"]["source_digest"] = json!("c".repeat(64)),
                "conflict" => {
                    invalid = first.clone();
                    invalid["core"]["entries"][0]["calls"] = json!(11);
                }
                "sequence" => invalid = generation_fixture(1, 1),
                "counter" => invalid["core"]["entries"][0]["calls"] = json!(9),
                "generation" => invalid = generation_fixture(3, 0),
                _ => unreachable!(),
            }
            projection.write(
                Ok(Some(invalid)),
                &owner,
                "online_namespace",
                super::super::utc_ms(),
                &mut bytes,
            );
            assert!(
                projection.failure.is_some(),
                "{fault} was treated as a transient handoff"
            );
            assert_eq!(
                projection.previous.as_ref(),
                Some(&first),
                "{fault} replaced the accepted high-water sample"
            );
            let mut recovered = generation_fixture(4, 1);
            recovered["core"]["entries"][0]["calls"] = json!(11);
            projection.write(
                Ok(Some(recovered)),
                &owner,
                "online_namespace",
                super::super::utc_ms(),
                &mut bytes,
            );
            assert!(
                projection.failure.is_some(),
                "{fault} failure was erased by later data"
            );
        }
    }

    #[test]
    fn sampler_rejects_one_context_race_then_recovers_without_retry_or_history_reset() {
        use std::cell::Cell;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkpoint-latest.json");
        let setup = Mutex::new(Some(Setup {
            identity: identity(),
            scope: "modeled_identity_sampler_lifecycle_control",
        }));
        let resources = super::super::resources::example_sample(std::process::id());
        let mut sampler = Sampler::new();
        sampler.tick(&setup, &path, &resources, true);
        let first = read_latest(&path).unwrap().unwrap();
        let captures = Cell::new(0);
        sampler.tick_with_capture(
            &setup,
            &path,
            &resources,
            true,
            |owner, scope, sequence, resources| {
                captures.set(captures.get() + 1);
                let actual = capture(owner, scope, sequence, resources);
                setup.lock().unwrap().as_mut().unwrap().identity.generation = 1;
                actual
            },
        );
        assert_eq!(
            captures.get(),
            1,
            "racing capture was retried in the same tick"
        );
        assert_eq!(
            sampler.previous.as_ref(),
            Some(&first),
            "racing capture replaced the accepted high-water sample"
        );
        let rejected = read_latest(&path).unwrap().unwrap();
        assert_eq!(
            rejected, first,
            "racing capture replaced the last valid latest file"
        );
        sampler.tick_with_capture(
            &setup,
            &path,
            &resources,
            true,
            |owner, scope, sequence, resources| {
                captures.set(captures.get() + 1);
                capture(owner, scope, sequence, resources)
            },
        );
        let recovered = read_latest(&path).unwrap().unwrap();
        assert_eq!(captures.get(), 2);
        assert_eq!(recovered["identity"]["sequence"], 3);
        assert_eq!(recovered["identity"]["generation"], 1);
        assert_eq!(
            recovered["available"], true,
            "one ordinary context race permanently poisoned later stable captures"
        );
        assert!(sampler.failure.is_none());
        assert_eq!(sampler.previous.as_ref(), Some(&recovered));
        follows(&recovered, &first).unwrap();
    }
}
