//! Fixed-boundary diagnostics for the owned fixture processes. No production endpoint.
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeSet,
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const ID_FIELDS: [&str; 12] = [
    "pid",
    "role",
    "server",
    "controller_pid",
    "generation",
    "sequence",
    "phase",
    "boundary",
    "source_digest",
    "binary_digest",
    "catalog_digest",
    "backend_prefix",
];
pub fn validate_receipt(receipt: &Value, expected: &Value) -> Result<(), String> {
    for field in ID_FIELDS {
        if expected.get(field).is_none() || receipt.get(field) != expected.get(field) {
            return Err(format!("metric identity mismatch: {field}"));
        }
    }
    Ok(())
}
fn subtract(before: &Value, after: &Value) -> Result<Value, String> {
    match (before, after) {
        (Value::Number(a), Value::Number(b)) => b
            .as_u64()
            .and_then(|b| a.as_u64().and_then(|a| b.checked_sub(a)))
            .map(|v| json!(v))
            .ok_or_else(|| "metric counter reset or invalid integer".into()),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .map(|(a, b)| subtract(a, b))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        (Value::Object(a), Value::Object(b)) if a.keys().eq(b.keys()) => a
            .iter()
            .map(|(key, value)| Ok((key.clone(), subtract(value, &b[key])?)))
            .collect::<Result<Map<_, _>, String>>()
            .map(Value::Object),
        _ => Err("metric counter shape changed".into()),
    }
}
fn delta_entries(before: &Value, after: &Value) -> Result<Value, String> {
    let a = before.as_array().ok_or("metric entries missing")?;
    let b = after.as_array().ok_or("metric entries missing")?;
    if a.len() != b.len() {
        return Err("metric entries shape changed".into());
    }
    let mut entries = Vec::with_capacity(a.len());
    for (a, b) in a.iter().zip(b) {
        let a = a.as_object().ok_or("metric entry invalid")?;
        let b = b.as_object().ok_or("metric entry invalid")?;
        if a.keys().ne(b.keys())
            || a.get("name").and_then(Value::as_str).is_none()
            || a["name"] != b["name"]
        {
            return Err("metric entry identity changed".into());
        }
        let mut delta = Map::new();
        for (key, value) in b {
            let value = match key.as_str() {
                "name" => value.clone(),
                "in_flight" | "max_elapsed_ns" => {
                    json!({"before":a[key],"after":value,"scope":"gauge; not subtracted"})
                }
                _ => subtract(&a[key], value)?,
            };
            delta.insert(key.clone(), value);
        }
        entries.push(Value::Object(delta));
    }
    Ok(Value::Array(entries))
}
fn phase_delta(before: &Value, after: &Value) -> Result<Value, String> {
    for field in ID_FIELDS
        .into_iter()
        .filter(|f| !["sequence", "phase", "boundary"].contains(f))
    {
        if before["identity"].get(field).is_none()
            || before["identity"].get(field) != after["identity"].get(field)
        {
            return Err(format!("phase identity changed: {field}"));
        }
    }
    if after["identity"]["sequence"]
        .as_u64()
        .zip(before["identity"]["sequence"].as_u64())
        .is_none_or(|(a, b)| a <= b)
    {
        return Err("phase sequence did not advance".into());
    }
    let mut value =
        json!({"core":delta_entries(&before["core"]["entries"], &after["core"]["entries"])?});
    if before["storage"].is_object() || after["storage"].is_object() {
        value["storage"] =
            delta_entries(&before["storage"]["entries"], &after["storage"]["entries"])?;
    }
    if before["runtime_activation"].is_object() || after["runtime_activation"].is_object() {
        value["runtime_activation"] =
            runtime_delta(&before["runtime_activation"], &after["runtime_activation"])?;
    }
    let mut process = Map::new();
    for field in [
        "cpu_user_us",
        "cpu_system_us",
        "minor_faults",
        "major_faults",
        "block_inputs",
        "block_outputs",
        "voluntary_context_switches",
        "involuntary_context_switches",
    ] {
        if before["process_since_baseline"].is_object()
            || after["process_since_baseline"].is_object()
        {
            process.insert(
                field.into(),
                subtract(
                    &before["process_since_baseline"][field],
                    &after["process_since_baseline"][field],
                )?,
            );
        }
    }
    value["process_counters"] = Value::Object(process);
    let a = &before["process_since_baseline"];
    let b = &after["process_since_baseline"];
    let mut gauges = Map::new();
    for field in [
        "rss_end_bytes",
        "lifetime_peak_rss_bytes",
        "sqlite_heap_end_bytes",
        "sqlite_heap_lifetime_peak_bytes",
        "rust_live_end_bytes",
    ] {
        gauges.insert(field.into(), json!({"before":a[field],"after":b[field]}));
    }
    value["process_gauges"] = Value::Object(gauges);
    if a["rust_allocator_instrumented"] != b["rust_allocator_instrumented"] {
        return Err("allocation instrumentation coverage changed".into());
    }
    let allocation_available = a["rust_allocator_instrumented"] == true;
    let mut allocation = Map::new();
    if allocation_available {
        for field in [
            "rust_allocations",
            "rust_deallocations",
            "rust_reallocations",
            "rust_allocated_bytes",
            "rust_freed_bytes",
        ] {
            allocation.insert(field.into(), subtract(&a[field], &b[field])?);
        }
    }
    value["allocations"] = json!({"available":allocation_available,"status":if allocation_available{"measured"}else{"unavailable"},"counters":allocation,"scope":"optional System Rust allocator atomics; excludes foreign C allocators; instrumentation affects throughput; live bytes are endpoint gauges"});
    if before["server_quic"].is_object() || after["server_quic"].is_object() {
        let a = &before["server_quic"]["transport"];
        let b = &after["server_quic"]["transport"];
        for sample in [a, b] {
            if sample["registry_complete"] != true
                || sample["unobserved_connections"] != 0
                || sample["missing_final_samples"] != 0
            {
                return Err("server transport registry incomplete".into());
            }
        }
        value["service"] = delta_entries(
            &before["server_quic"]["entries"],
            &after["server_quic"]["entries"],
        )?;
        value["server_transport"] = json!({
            "observed_total":subtract(&a["observed_total"], &b["observed_total"] )?,
            "retired_connections":subtract(&a["retired_connections"], &b["retired_connections"] )?,
            "active_endpoints":{"before":a["active"],"after":b["active"]},
            "scope":"same worker replica generation; observed totals include retained retired sessions; UDP payload bytes exclude network headers; frame counts are not datagrams or API calls; active RTT/cwnd/MTU gauges are not subtracted; retirement excludes later close retransmissions and transports without an accepted Connection"});
    }
    let catalog_row = |name: &str| {
        value["core"]
            .as_array()
            .and_then(|entries| entries.iter().find(|entry| entry["name"] == name))
            .cloned()
    };
    let catalog_query = catalog_row("catalog.query_document_bytes");
    let catalog_decode = catalog_row("catalog.decode_validate_bytes");
    value["catalog_query_document"] = json!({"query":catalog_query,"decode":catalog_decode,"scope":"catalog document bytes from core units; separate from direct SDK/blob API bytes; nested wall is not process CPU"});
    value["scope"] = json!(
        "boundary-to-boundary process-local counters; inclusive overlapping core/storage wall; process CPU includes observer and background work; gauges remain in raw receipts"
    );
    Ok(value)
}
fn quiescent_window(before: &Value, after: &Value) -> bool {
    let Some(sequence) = before["activity_sequence_before"].as_u64() else {
        return false;
    };
    [before, after].into_iter().all(|sample| {
        sample["complete"] == true
            && sample["application_quiescent"] == true
            && ["activity_sequence_before", "activity_sequence_after"]
                .into_iter()
                .all(|field| sample[field].as_u64() == Some(sequence))
            && [
                "activity_writers_before",
                "activity_writers_after",
                "active_handshakes_before",
                "active_handshakes_after",
                "active_requests_before",
                "active_requests_after",
            ]
            .into_iter()
            .all(|field| sample[field].as_u64() == Some(0))
    })
}
pub fn family_state(enabled: bool, configured: bool, available: bool, quiescent: bool) -> Value {
    let status = if !enabled {
        "disabled"
    } else if !configured {
        "not_configured"
    } else if !available {
        "unavailable"
    } else if !quiescent {
        "nonquiescent"
    } else {
        "complete"
    };
    json!({"enabled":enabled,"configured":configured,"available":available,"complete":status=="complete","status":status})
}
#[derive(Default)]
pub struct Sequence {
    previous: Option<Value>,
}
impl Sequence {
    pub fn accept(&mut self, request: &Value) -> Result<bool, String> {
        let next = request["sequence"]
            .as_u64()
            .ok_or("metric sequence missing")?;
        if self.previous.as_ref() == Some(request) {
            return Ok(false);
        }
        let expected = self
            .previous
            .as_ref()
            .map_or(Some(1), |p| {
                p["sequence"].as_u64().and_then(|n| n.checked_add(1))
            })
            .ok_or("metric sequence overflow")?;
        if next != expected {
            return Err("metric sequence stale, conflicting or out of order".into());
        }
        self.previous = Some(request.clone());
        Ok(true)
    }
}
fn validate_fleet(receipts: &[Value], expected: &[Value]) -> Result<(), String> {
    if receipts.len() != super::config::SERVERS || expected.len() != super::config::SERVERS {
        return Err("metric fleet coverage incomplete".into());
    }
    let mut pids = BTreeSet::new();
    let mut servers = BTreeSet::new();
    for (receipt, expected) in receipts.iter().zip(expected) {
        validate_receipt(receipt, expected)?;
        if !pids.insert(receipt["pid"].as_u64().ok_or("metric PID invalid")?)
            || !servers.insert(receipt["server"].as_u64().ok_or("metric server invalid")?)
        {
            return Err("metric duplicate worker".into());
        }
    }
    Ok(())
}
pub fn publish_immutable(path: &Path, value: &Value) -> Result<(), String> {
    let span = observer().begin("metric_publication");
    let result = publish_immutable_inner(path, value);
    span.finish(result.is_ok(), result.as_ref().copied().unwrap_or(0));
    result.map(|_| ())
}
fn publish_immutable_inner(path: &Path, value: &Value) -> Result<u64, String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "metric encoding failed")?;
    let pending = path.with_extension(format!("pending-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|_| "metric pending file already exists or unavailable")?;
    let result: Result<(), String> = (|| {
        file.write_all(&bytes).map_err(|_| "metric write failed")?;
        file.flush().map_err(|_| "metric flush failed")?;
        std::fs::hard_link(&pending, path)
            .map_err(|_| "immutable metric receipt exists or publication failed")?;
        Ok(())
    })();
    drop(file);
    let removed = std::fs::remove_file(&pending);
    let result =
        result.and_then(|()| removed.map_err(|_| "metric pending cleanup failed".to_string()));
    result.map(|()| bytes.len() as u64)
}
const CATEGORIES: [&str; 21] = [
    "context_open",
    "filesystem_open",
    "backing_receipt",
    "membership",
    "file_open",
    "stat",
    "data_read",
    "expected_compare",
    "eof",
    "handle_close",
    "filesystem_context_close",
    "sampler_capture",
    "receipt_publication",
    "metric_capture",
    "metric_publication",
    "barrier_wait",
    "journal_publication",
    "file_hash",
    "expected_state_observation",
    "audit_observation",
    "byte_preparation",
];
#[derive(Default, Clone, serde::Serialize)]
struct Counter {
    calls: u64,
    success: u64,
    error: u64,
    cancelled: u64,
    in_flight: u64,
    bytes: u64,
    elapsed_ns: u64,
}
#[derive(Clone)]
pub struct Accounting {
    counters: Arc<Mutex<[Counter; CATEGORIES.len()]>>,
    enabled: bool,
    saturated: Arc<AtomicBool>,
}
impl Default for Accounting {
    fn default() -> Self {
        Self::new(true)
    }
}
impl Accounting {
    pub fn new(enabled: bool) -> Self {
        Self {
            counters: Arc::new(Mutex::new(std::array::from_fn(|_| Counter::default()))),
            enabled,
            saturated: Arc::new(AtomicBool::new(false)),
        }
    }
    fn add(&self, value: &mut u64, amount: u64) {
        *value = value.checked_add(amount).unwrap_or_else(|| {
            self.saturated.store(true, Ordering::Relaxed);
            u64::MAX
        });
    }
    pub fn complete(&self) -> bool {
        self.enabled && !self.saturated.load(Ordering::Relaxed)
    }
    pub fn begin(&self, category: &'static str) -> AccountSpan {
        if !self.enabled {
            return AccountSpan {
                accounting: None,
                index: 0,
                start: None,
                outcome: None,
                bytes: 0,
            };
        }
        let index = CATEGORIES
            .iter()
            .position(|c| *c == category)
            .expect("fixed metric accounting category");
        {
            let mut counters = self.counters.lock().unwrap();
            self.add(&mut counters[index].calls, 1);
            self.add(&mut counters[index].in_flight, 1);
        }
        AccountSpan {
            accounting: Some(self.clone()),
            index,
            start: Some(Instant::now()),
            outcome: None,
            bytes: 0,
        }
    }
    pub fn snapshot(&self) -> Value {
        let counters = self.counters.lock().unwrap();
        let mut result = Map::new();
        for (name, counter) in CATEGORIES.iter().zip(counters.iter()) {
            result.insert((*name).into(), serde_json::to_value(counter).unwrap());
        }
        result.insert("_status".into(), json!({"enabled":self.enabled,"complete":self.complete(),"counter_saturated":self.saturated.load(Ordering::Relaxed)}));
        Value::Object(result)
    }
}
pub struct AccountSpan {
    accounting: Option<Accounting>,
    index: usize,
    start: Option<Instant>,
    outcome: Option<bool>,
    bytes: u64,
}
impl AccountSpan {
    pub fn finish(mut self, success: bool, bytes: u64) {
        self.outcome = Some(success);
        self.bytes = bytes;
    }
}
impl Drop for AccountSpan {
    fn drop(&mut self) {
        let (Some(accounting), Some(start)) = (&self.accounting, self.start) else {
            return;
        };
        let mut counters = accounting.counters.lock().unwrap();
        let c = &mut counters[self.index];
        c.in_flight = c.in_flight.checked_sub(1).unwrap_or_else(|| {
            accounting.saturated.store(true, Ordering::Relaxed);
            0
        });
        accounting.add(&mut c.bytes, self.bytes);
        let elapsed = u64::try_from(start.elapsed().as_nanos()).unwrap_or_else(|_| {
            accounting.saturated.store(true, Ordering::Relaxed);
            u64::MAX
        });
        accounting.add(&mut c.elapsed_ns, elapsed);
        match self.outcome {
            Some(true) => accounting.add(&mut c.success, 1),
            Some(false) => accounting.add(&mut c.error, 1),
            None => accounting.add(&mut c.cancelled, 1),
        }
    }
}
pub fn observer() -> &'static Accounting {
    static ACCOUNTING: OnceLock<Accounting> = OnceLock::new();
    ACCOUNTING.get_or_init(|| Accounting::new(mount_rs_core::diagnostics::profile::enabled()))
}

pub fn identity(
    private: &super::process::PrivateConfig,
    pid: u32,
    server: Option<usize>,
    generation: u64,
    sequence: u64,
    phase: &str,
    boundary: &str,
) -> Value {
    json!({"pid":pid,"role":if server.is_some(){"worker"}else{"controller"},"server":server,"controller_pid":private.parent_pid,"generation":generation,"sequence":sequence,"phase":phase,"boundary":boundary,"source_digest":private.source_digest,"binary_digest":private.binary_digest,"catalog_digest":private.catalog_digest,"backend_prefix":private.backend.prefix})
}
// Runtime evidence is local observer state, distinct from close acknowledgments.
const RUNTIME_POOL_FIELDS: [&str; 13] = [
    "registered",
    "resident",
    "opening",
    "ready",
    "closing",
    "quarantined",
    "pinned",
    "open_success",
    "open_error",
    "eviction_success",
    "eviction_error",
    "waits",
    "capacity_rejections",
];
const RUNTIME_ROWS: [&str; 8] = [
    "runtime.acquire",
    "runtime.activation_wait",
    "runtime.open",
    "runtime.eviction_shutdown",
    "runtime.terminal_drain",
    "runtime.handle_close",
    "runtime.state_mutex_wait",
    "runtime.state_mutex_hold",
];
fn closed_keys(value: &Value, keys: &[&str]) -> Result<(), String> {
    let object = value.as_object().ok_or("runtime evidence object missing")?;
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err("runtime evidence fields changed".into());
    }
    Ok(())
}
fn runtime_integer(value: &Value) -> Result<u64, String> {
    value
        .as_u64()
        .ok_or_else(|| "runtime evidence integer invalid".into())
}
fn runtime_hex(value: &Value) -> Result<&str, String> {
    let value = value.as_str().ok_or("runtime backing ID missing")?;
    let parsed = mount_rs_core::storage::ConcurrentBackingId::from_hex(value)
        .map_err(|_| "runtime backing ID invalid")?;
    if parsed.to_hex() != value || value == "00000000000000000000000000000000" {
        return Err("runtime backing ID noncanonical".into());
    }
    Ok(value)
}
/// Validate exact shape and identity. False means valid incomplete observation;
/// it never authorizes shutdown, eviction, context close, or a new generation.
pub(super) fn validate_runtime_snapshot(
    value: &Value,
    generation: u64,
    drives: usize,
    expected: &[String],
) -> Result<bool, String> {
    closed_keys(
        value,
        &[
            "schema",
            "generation",
            "capacity",
            "available",
            "complete",
            "pool",
            "diagnostics",
            "observations",
        ],
    )?;
    if drives == 0
        || drives > 10_000
        || expected.len() != drives
        || value["schema"] != "mount-rs.target-runtime.v1"
        || runtime_integer(&value["generation"])? != generation
        || runtime_integer(&value["capacity"])? != drives as u64
    {
        return Err("runtime evidence identity mismatch".into());
    }
    let available = value["available"]
        .as_bool()
        .ok_or("runtime availability invalid")?;
    let declared_complete = value["complete"]
        .as_bool()
        .ok_or("runtime completeness invalid")?;
    let pool = &value["pool"];
    closed_keys(pool, &RUNTIME_POOL_FIELDS)?;
    for field in RUNTIME_POOL_FIELDS {
        runtime_integer(&pool[field])?;
    }
    let rows = value["observations"]
        .as_array()
        .ok_or("runtime observations missing")?;
    if rows.len() != drives {
        return Err("runtime observation geometry mismatch".into());
    }
    let mut constructed = 0_u64;
    let mut coherent = true;
    for (drive, (row, expected)) in rows.iter().zip(expected).enumerate() {
        closed_keys(
            row,
            &[
                "drive",
                "expected_backing",
                "observed_backing",
                "constructed",
            ],
        )?;
        if runtime_integer(&row["drive"])? != drive as u64
            || runtime_hex(&row["expected_backing"])? != expected.as_str()
            || runtime_hex(&json!(expected))? != expected.as_str()
        {
            return Err("runtime observation backing mismatch".into());
        }
        let count = runtime_integer(&row["constructed"])?;
        if count == 0 {
            if !row["observed_backing"].is_null() {
                return Err("cold runtime claims backing observation".into());
            }
        } else if runtime_hex(&row["observed_backing"])? != expected.as_str() {
            return Err("actual runtime backing mismatch".into());
        }
        coherent &= count <= 1;
        constructed = constructed
            .checked_add(count)
            .ok_or("runtime construction count overflow")?;
    }
    coherent &= pool["registered"] == drives as u64
        && runtime_integer(&pool["resident"])? <= drives as u64
        && pool["open_success"] == constructed
        && [
            "opening",
            "closing",
            "quarantined",
            "pinned",
            "open_error",
            "eviction_error",
            "eviction_success",
        ]
        .iter()
        .all(|field| pool[*field] == 0);
    if available {
        let diagnostics = &value["diagnostics"];
        closed_keys(
            diagnostics,
            &[
                "scope",
                "sampling_scope",
                "histogram_scope",
                "deferred_scope",
                "inclusive_spans_overlap",
                "concurrent_activity",
                "counter_saturated",
                "slow_record_attempts",
                "entries",
            ],
        )?;
        for (field, expected) in [
            ("scope", "process_local_preconstructed_runtime_observer"),
            (
                "sampling_scope",
                "serial_atomic_loads_not_transactional_or_drain_proof",
            ),
            (
                "histogram_scope",
                "inclusive_wall_time_log2_microseconds_32_buckets",
            ),
            (
                "deferred_scope",
                "deferred_spans_enter_only_on_post_unlock_publication",
            ),
        ] {
            if diagnostics[field] != expected {
                return Err("runtime diagnostic scope mismatch".into());
            }
        }
        if diagnostics["inclusive_spans_overlap"] != true {
            return Err("runtime diagnostic overlap missing".into());
        }
        let concurrent = diagnostics["concurrent_activity"]
            .as_bool()
            .ok_or("runtime concurrency missing")?;
        let saturated = diagnostics["counter_saturated"]
            .as_bool()
            .ok_or("runtime saturation missing")?;
        runtime_integer(&diagnostics["slow_record_attempts"])?;
        coherent &= !concurrent && !saturated;
        let entries = diagnostics["entries"]
            .as_array()
            .ok_or("runtime diagnostic rows missing")?;
        if entries.len() != RUNTIME_ROWS.len() {
            return Err("runtime diagnostic inventory changed".into());
        }
        for (row, name) in entries.iter().zip(RUNTIME_ROWS) {
            closed_keys(
                row,
                &[
                    "name",
                    "calls",
                    "success",
                    "error",
                    "cancelled",
                    "in_flight",
                    "elapsed_ns",
                    "max_elapsed_ns",
                    "latency_log2_us",
                ],
            )?;
            if row["name"] != name {
                return Err("runtime diagnostic row changed".into());
            }
            for field in [
                "calls",
                "success",
                "error",
                "cancelled",
                "in_flight",
                "elapsed_ns",
                "max_elapsed_ns",
            ] {
                runtime_integer(&row[field])?;
            }
            let histogram = row["latency_log2_us"]
                .as_array()
                .ok_or("runtime histogram missing")?;
            if histogram.len() != 32 {
                return Err("runtime histogram shape changed".into());
            }
            let mut histogram_sum = Some(0_u64);
            for bucket in histogram {
                let count = runtime_integer(bucket)?;
                histogram_sum = histogram_sum.and_then(|sum| sum.checked_add(count));
            }
            let outcomes = runtime_integer(&row["success"])?
                .checked_add(runtime_integer(&row["error"])?)
                .and_then(|sum| sum.checked_add(row["cancelled"].as_u64().unwrap()));
            coherent &= outcomes
                .and_then(|sum| sum.checked_add(row["in_flight"].as_u64().unwrap()))
                == row["calls"].as_u64()
                && outcomes == histogram_sum
                && row["in_flight"] == 0
                && runtime_integer(&row["max_elapsed_ns"])? <= runtime_integer(&row["elapsed_ns"])?;
        }
    } else if !value["diagnostics"].is_null() {
        return Err("unavailable runtime claims diagnostics".into());
    }
    let calculated_complete = available && coherent;
    if declared_complete && !calculated_complete {
        return Err("runtime completeness contradicts actual observation".into());
    }
    Ok(declared_complete && calculated_complete)
}
fn runtime_expected(value: &Value) -> Result<Vec<String>, String> {
    value["observations"]
        .as_array()
        .ok_or("runtime observations missing")?
        .iter()
        .map(|row| runtime_hex(&row["expected_backing"]).map(str::to_owned))
        .collect()
}
fn runtime_delta(before: &Value, after: &Value) -> Result<Value, String> {
    if before["schema"] != after["schema"]
        || before["generation"] != after["generation"]
        || before["capacity"] != after["capacity"]
        || runtime_expected(before)? != runtime_expected(after)?
    {
        return Err("runtime delta identity changed".into());
    }
    let mut pool = Map::new();
    for field in RUNTIME_POOL_FIELDS {
        let value = if [
            "registered",
            "resident",
            "opening",
            "ready",
            "closing",
            "quarantined",
            "pinned",
        ]
        .contains(&field)
        {
            json!({"before":before["pool"][field],"after":after["pool"][field],"scope":"gauge; not subtracted"})
        } else {
            subtract(&before["pool"][field], &after["pool"][field])?
        };
        pool.insert(field.into(), value);
    }
    let diagnostics = match (before["available"].as_bool(), after["available"].as_bool()) {
        (Some(true), Some(true)) => {
            json!({"entries":delta_entries(&before["diagnostics"]["entries"], &after["diagnostics"]["entries"])?})
        }
        (Some(false), Some(false)) => Value::Null,
        _ => return Err("runtime diagnostic coverage changed".into()),
    };
    let a = before["observations"]
        .as_array()
        .ok_or("runtime observations missing")?;
    let b = after["observations"]
        .as_array()
        .ok_or("runtime observations missing")?;
    if a.len() != b.len() {
        return Err("runtime observations changed".into());
    }
    let observations = a.iter().zip(b).map(|(a,b)| {
        if a["drive"] != b["drive"] || a["expected_backing"] != b["expected_backing"]
            || (!a["observed_backing"].is_null() && a["observed_backing"] != b["observed_backing"]) {
            return Err("runtime observation identity changed".into());
        }
        Ok(json!({"drive":b["drive"],"constructed":subtract(&a["constructed"],&b["constructed"])?,"observed_backing":{"before":a["observed_backing"],"after":b["observed_backing"],"scope":"captured-at-construction identity; not fresh inspection"}}))
    }).collect::<Result<Vec<_>,String>>()?;
    Ok(
        json!({"pool":pool,"diagnostics":diagnostics,"observations":observations,"scope":"same owned generation; inclusive local counters and endpoint gauges; not a drain proof"}),
    )
}
fn validate_worker_runtime(value: &Value, expected: &[String]) -> Result<bool, String> {
    validate_runtime_snapshot(
        &value["runtime_activation"],
        runtime_integer(&value["identity"]["generation"])?,
        expected.len(),
        expected,
    )
}
fn runtime_assignment(generation: u64, drive: usize) -> Result<usize, String> {
    let offset = match generation {
        0 | 1 => 0,
        generation if generation <= super::SERVERS as u64 + 1 => {
            ((generation - 1) % super::SERVERS as u64) as usize
        }
        _ => return Err("runtime assignment generation outside fixture proof".into()),
    };
    Ok((drive % super::SERVERS + offset) % super::SERVERS)
}
fn runtime_boundary(
    value: &Value,
    expected: &[String],
    phase: &str,
    boundary: &str,
    server: usize,
) -> Result<(), String> {
    if !validate_worker_runtime(value, expected)? {
        return Err("runtime boundary observation incomplete".into());
    }
    let generation = runtime_integer(&value["identity"]["generation"])?;
    if server >= super::SERVERS {
        return Err("runtime boundary server outside fixture proof".into());
    }
    // g0 populates assigned Drives; g1 measures those assignments. Generations
    // g2..=g11 separately prove every cross-server pair after timed traffic.
    runtime_assignment(generation, 0)?;
    let cold = matches!(
        (generation, phase, boundary),
        (_, "worker_startup", "ready")
            | (_, "worker_setup", "after_ready")
            | (_, "refresh_replicas", "after_ready")
            | (0, "online_namespace", "before")
            | (1, "routes_and_scope", "before" | "after")
            | (1, "assigned_warmup", "before")
    ) || (generation >= 2 && phase == "crossnode_routes" && boundary == "after_ready");
    let mut wanted = 0_u64;
    let observations = value["runtime_activation"]["observations"]
        .as_array()
        .ok_or("runtime observations missing")?;
    for (drive, observation) in observations.iter().enumerate() {
        let selected = !cold && runtime_assignment(generation, drive)? == server;
        wanted += u64::from(selected);
        if observation["constructed"] != u64::from(selected) {
            return Err("runtime boundary selected Drive mismatch".into());
        }
    }
    let pool = &value["runtime_activation"]["pool"];
    if pool["open_success"] != wanted || pool["resident"] != wanted || pool["ready"] != wanted {
        return Err("runtime boundary activation geometry mismatch".into());
    }
    Ok(())
}
fn runtime_closed_boundary(
    value: &Value,
    expected: &[String],
    server: usize,
) -> Result<bool, String> {
    let complete = validate_worker_runtime(value, expected)?;
    let generation = runtime_integer(&value["identity"]["generation"])?;
    let observations = value["runtime_activation"]["observations"]
        .as_array()
        .ok_or("runtime observations missing")?;
    if server >= super::SERVERS {
        return Err("closed runtime server outside fixture proof".into());
    }
    runtime_assignment(generation, 0)?;
    let mut wanted = 0_u64;
    for (drive, observation) in observations.iter().enumerate() {
        let selected = runtime_assignment(generation, drive)? == server;
        wanted += u64::from(selected);
        if observation["constructed"] != u64::from(selected) {
            return Err("closed runtime selected Drive mismatch".into());
        }
    }
    Ok(complete
        && value["runtime_activation"]["pool"]["open_success"] == wanted
        && [
            "resident",
            "opening",
            "ready",
            "closing",
            "quarantined",
            "pinned",
        ]
        .iter()
        .all(|field| value["runtime_activation"]["pool"][*field] == 0))
}
#[cfg(test)]
pub(super) fn example_runtime(generation: u64, drives: usize) -> Value {
    let mut pool = Map::new();
    for field in RUNTIME_POOL_FIELDS {
        pool.insert(
            field.into(),
            json!(if field == "registered" {
                drives as u64
            } else {
                0
            }),
        );
    }
    json!({"schema":"mount-rs.target-runtime.v1","generation":generation,"capacity":drives,"available":false,"complete":false,"pool":pool,"diagnostics":null,"observations":(0..drives).map(|drive|json!({"drive":drive,"expected_backing":format!("{:032x}",drive+1),"observed_backing":null,"constructed":0})).collect::<Vec<_>>()})
}

pub struct Local {
    baseline: Option<super::resource_profile::Snapshot>,
    previous_process: Option<super::resource_profile::Snapshot>,
    previous: Option<Value>,
    started: Instant,
}
impl Local {
    pub fn new() -> Result<Self, String> {
        let baseline = if mount_rs_core::diagnostics::profile::enabled() {
            Some(
                super::resource_profile::Snapshot::capture_process_io_boundary()
                    .map_err(|_| "metric process baseline unavailable")?,
            )
        } else {
            None
        };
        Ok(Self {
            baseline,
            previous_process: None,
            previous: None,
            started: Instant::now(),
        })
    }
    pub fn capture(
        &mut self,
        identity: Value,
        service: Option<&mount_rs_service::server::ServerDiagnostics>,
        oracle: Value,
    ) -> Result<Value, String> {
        let span = observer().begin("metric_capture");
        let result = self.capture_inner(identity, service, oracle, None);
        span.finish(result.is_ok(), 0);
        result
    }
    pub fn capture_with_runtime<F>(
        &mut self,
        identity: Value,
        service: Option<&mount_rs_service::server::ServerDiagnostics>,
        mut runtime: F,
    ) -> Result<Value, String>
    where
        F: FnMut() -> mount_rs_core::Result<Value>,
    {
        let span = observer().begin("metric_capture");
        let mut capture =
            || runtime().map_err(|_| "runtime metric capture unavailable".to_string());
        let result = self.capture_inner(identity, service, Value::Null, Some(&mut capture));
        span.finish(result.is_ok(), 0);
        result
    }
    fn capture_inner(
        &mut self,
        identity: Value,
        service: Option<&mount_rs_service::server::ServerDiagnostics>,
        oracle: Value,
        runtime: Option<&mut dyn FnMut() -> Result<Value, String>>,
    ) -> Result<Value, String> {
        let start = Instant::now();
        let started = super::utc_ms();
        // Include the bounded runtime snapshot in the existing capture allowance
        // and observer category; no added sampler or deadline is created.
        let runtime = runtime.map(|capture| capture()).transpose()?;
        let enabled = mount_rs_core::diagnostics::profile::enabled();
        let service_before =
            service.map(|observer| serde_json::to_value(observer.snapshot()).unwrap());
        let core = enabled.then(|| {
            serde_json::to_value(mount_rs_core::diagnostics::profile::snapshot()).unwrap()
        });
        let storage = enabled.then(|| {
            serde_json::to_value(mount_rs_core::diagnostics::storage::snapshot()).unwrap()
        });
        let mut process_interval = None;
        let process = match &self.baseline {
            Some(baseline) => {
                let snapshot = super::resource_profile::Snapshot::capture_process_io_boundary()
                    .map_err(|_| "metric process capture unavailable")?;
                let mut value = snapshot
                    .delta(baseline)
                    .map_err(|_| "metric process counter reset")?;
                value["quic_client_side"] = json!({"available":false,"reason":"process-only observation; actual client boundary transport retained separately"});
                if let Some(previous) = &self.previous_process {
                    let mut interval = snapshot
                        .delta(previous)
                        .map_err(|_| "metric process interval reset")?;
                    interval["quic_client_side"] =
                        json!({"available":false,"reason":"process-only observation"});
                    process_interval = Some(interval);
                }
                self.previous_process = Some(snapshot);
                Some(value)
            }
            None => None,
        };
        let service = service.map(|observer| serde_json::to_value(observer.snapshot()).unwrap());
        let worker = identity["role"] == "worker";
        let application_quiescent = if worker {
            service_before
                .as_ref()
                .zip(service.as_ref())
                .is_some_and(|(before, after)| quiescent_window(before, after))
        } else {
            true
        };
        let service_envelope = service_before.as_ref().map(|before| {
            let keys = [
                "capture_started_unix_ns",
                "capture_elapsed_ns",
                "registry_snapshot_elapsed_ns",
                "activity_sequence_before",
                "activity_sequence_after",
                "activity_writers_before",
                "activity_writers_after",
                "active_handshakes_before",
                "active_handshakes_after",
                "active_requests_before",
                "active_requests_after",
                "application_quiescent",
                "complete",
            ];
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.into(), before[key].clone()))
                    .collect(),
            )
        });
        let storage_quiescent = storage.as_ref().is_some_and(|s| s["in_flight"] == 0);
        let quiescent = application_quiescent && storage_quiescent;
        let runtime_complete = if worker {
            match runtime.as_ref() {
                Some(value) => validate_runtime_snapshot(
                    value,
                    identity["generation"]
                        .as_u64()
                        .ok_or("runtime generation missing")?,
                    value["capacity"]
                        .as_u64()
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or("runtime capacity missing")?,
                    &runtime_expected(value)?,
                )?,
                None => false,
            }
        } else {
            true
        };
        let accounting_complete =
            observer().complete() && (oracle.is_null() || oracle["_status"]["complete"] == true);
        let elapsed = start.elapsed();
        let mut value = json!({"schema":"mount-rs-phase-metrics-v1","identity":identity,"enabled":enabled,
            "capture_started_unix_ms":started,"capture_ended_unix_ms":super::utc_ms(),"capture_elapsed_ns":elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,"process_observation_elapsed_seconds":self.started.elapsed().as_secs_f64(),
            "capture_complete":!enabled || elapsed<=Duration::from_secs(30),"metrics_complete":enabled && quiescent && accounting_complete && runtime_complete && elapsed<=Duration::from_secs(30),"accounting_complete":accounting_complete,
            "quiescence":{"controller_work_drained":true,"service_observed":service.is_some(),"application_quiescent":application_quiescent,"instrumented_storage_in_flight_zero":storage_quiescent,"scope":"owned controller work drained; only instrumented service/storage activity observed; no global atomic cut or proof of all provider/background work"},
            "core":core,"storage":storage,"process_since_baseline":process,"process_since_previous_boundary":process_interval,"server_quic":service,"server_quic_before_local_capture":service_envelope,"oracle":oracle,"runtime_activation":runtime,
            "coverage":{"core":family_state(enabled,true,true,quiescent),"storage":family_state(enabled,true,true,quiescent),"process":family_state(enabled,true,true,true),
                "server_quic":family_state(enabled,worker,service.is_some(),quiescent),
                "runtime_activation":family_state(enabled,worker,runtime.as_ref().is_some_and(|value| value["available"]==true),runtime_complete),
                "catalog_pager_core":{"status":if enabled{"partial"}else{"disabled"},"available":enabled,"scope":"catalog pager hit/miss/write/unavailable core rows retained; not all SQLite connections"},
                "sqlite_cache_sql":{"enabled":enabled,"configured":true,"complete":false,"status":"unavailable","available":false,"reason":"live registry requires connection locks/PRAGMA; no independent blocking observer owner in this collector"},
                "blob_cache":{"enabled":enabled,"configured":false,"complete":false,"status":"not_configured","available":false},
                "raw_object_store":{"enabled":enabled,"configured":true,"complete":false,"status":"unavailable","available":false,"reason":"NAPI raw registry does not register these direct SDK/RustFs processes"},
                "http_attempts":{"enabled":enabled,"complete":false,"status":"unavailable","available":false},"physical_iops":{"enabled":enabled,"complete":false,"status":"unavailable","available":false}},
            "scope":"process-local core and direct SDK storage counters; inclusive overlapping wall, logical API bytes/calls; process CPU includes observers/background; no NAPI or HTTP attribution",
            "observer":observer().snapshot(),"observer_scope":"fixed scalar counts/wall/known bytes; snapshot excludes its own completed capture/publication; later outer receipt includes those costs; no isolated observer CPU"});
        if let Some(before) = &self.previous {
            if before["identity"]["generation"] == value["identity"]["generation"] && enabled {
                match phase_delta(before, &value) {
                    Ok(delta) => {
                        value["delta_from_previous"] = json!({"complete":before["metrics_complete"]==true && value["metrics_complete"]==true,"before_sequence":before["identity"]["sequence"],"counters":delta})
                    }
                    Err(error) => {
                        value["metrics_complete"] = json!(false);
                        value["delta_from_previous"] = json!({"complete":false,"error":error});
                    }
                }
            } else {
                value["delta_from_previous"] = json!({"complete":false,"reason":if enabled{"new replica generation baseline"}else{"profiling disabled"}});
            }
        }
        self.previous = Some(value.clone());
        Ok(value)
    }
}
fn boundary_deadline(started: Instant, enclosing: Instant) -> Instant {
    enclosing.min(started + Duration::from_secs(30))
}

pub struct Collector {
    pub local: Local,
    pub sequence: u64,
    pub records: Vec<Value>,
    pub complete: bool,
}
impl Collector {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            local: Local::new()?,
            sequence: 0,
            records: Vec::new(),
            complete: mount_rs_core::diagnostics::profile::enabled(),
        })
    }
    pub fn terminal_workers(
        &mut self,
        fleet: &super::process::Fleet,
        deadline: Instant,
    ) -> Result<(), String> {
        let runtime_expected = fleet.initialized_backings()?;
        let base = self
            .local
            .previous
            .as_ref()
            .ok_or("worker terminal metric identity unavailable")?["identity"]
            .clone();
        let index = self.records.len();
        self.records.push(json!({"phase":"worker_cleanup","boundary":"terminal","workers":[],"complete":false,"metrics_complete":false}));
        let mut rows = Vec::new();
        let mut identities = Vec::new();
        let mut expected = Vec::new();
        for child in &fleet.children {
            if Instant::now() >= deadline {
                self.complete = false;
                return Err("worker terminal metric observation deadline".into());
            }
            let mut id = base.clone();
            id["pid"] = json!(child.child.id());
            id["server"] = json!(child.server);
            id["role"] = json!("worker");
            id["generation"] = json!(
                child
                    .ready
                    .as_ref()
                    .ok_or("worker terminal readiness missing")?
                    .generation
            );
            id["sequence"] = json!(self.sequence + 1);
            id["phase"] = json!("worker_cleanup");
            id["boundary"] = json!("terminal");
            let path = child.root.join("metrics/terminal.json");
            match super::read_json(&path) {
                Ok(value) => {
                    let valid = validate_receipt(&value["identity"], &id);
                    let runtime_complete =
                        runtime_closed_boundary(&value, runtime_expected, child.server)?;
                    let runtime_closed = runtime_complete;
                    let complete = valid.is_ok()
                        && value["capture_complete"] == true
                        && runtime_complete
                        && runtime_closed
                        && child.reaped;
                    let terminal = super::read_json(&child.root.join("terminal.json"))?;
                    let accounting_complete =
                        terminal["observer_accounting"]["_status"]["complete"] == true;
                    self.complete &= complete && accounting_complete;
                    identities.push(value["identity"].clone());
                    expected.push(id);
                    rows.push(json!({"server":child.server,"pid":child.child.id(),"file":format!("worker-{}/metrics/terminal.json",child.server),"sha256":super::file_digest(&path)?,"complete":complete,"metrics_complete":value["metrics_complete"]==true && accounting_complete,"observer_accounting_complete":accounting_complete,"identity_error":valid.err()}));
                }
                Err(error) => {
                    self.complete = false;
                    rows.push(json!({"server":child.server,"pid":child.child.id(),"complete":false,"error":error}));
                }
            }
            self.records[index]["workers"] = json!(rows);
        }
        let complete = validate_fleet(&identities, &expected).is_ok()
            && rows.iter().all(|r| r["complete"] == true)
            && Instant::now() <= deadline;
        let metrics_complete = complete && rows.iter().all(|r| r["metrics_complete"] == true);
        self.complete &= metrics_complete;
        self.records[index]["complete"] = json!(complete);
        self.records[index]["metrics_complete"] = json!(metrics_complete);
        if complete {
            Ok(())
        } else {
            Err("worker terminal metrics incomplete".into())
        }
    }
    pub fn terminal(
        &mut self,
        output: &Path,
        oracle: Value,
        deadline: Instant,
    ) -> Result<(), String> {
        if Instant::now() >= deadline {
            self.complete = false;
            return Err("terminal metrics existing observation deadline".into());
        }
        let mut id = self
            .local
            .previous
            .as_ref()
            .ok_or("terminal metrics identity unavailable")?["identity"]
            .clone();
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("metric sequence exhausted")?;
        id["sequence"] = json!(self.sequence);
        id["phase"] = json!("controller_cleanup");
        id["boundary"] = json!("terminal");
        let value = self.local.capture(id, None, oracle)?;
        let path = output.join("metrics/terminal.json");
        publish_immutable(&path, &value)?;
        let complete = Instant::now() <= deadline && value["capture_complete"] == true;
        let metrics_complete = complete && value["metrics_complete"] == true;
        self.complete &= metrics_complete;
        self.records.push(json!({"phase":"controller_cleanup","boundary":"terminal","complete":complete,"controller":{"file":"metrics/terminal.json","sha256":super::file_digest(&path)?},"metrics_complete":metrics_complete,"scope":"after cleanup; worker terminal observations retained separately; existing audit observation deadline"}));
        if Instant::now() > deadline {
            self.complete = false;
            if let Some(record) = self.records.last_mut() {
                record["complete"] = json!(false);
                record["metrics_complete"] = json!(false);
                record["error"] = json!("post-hash observation deadline");
            }
            return Err("terminal metrics completed after observation deadline".into());
        }
        Ok(())
    }
    pub fn qualified(&self) -> bool {
        self.complete
            && observer().complete()
            && !self.records.is_empty()
            && self
                .records
                .iter()
                .all(|r| r["complete"] == true && r["metrics_complete"] == true)
    }
    pub fn summary(&self) -> Value {
        json!({"enabled":mount_rs_core::diagnostics::profile::enabled(),"metrics_complete":self.qualified(),"boundaries":self.records,"observer":observer().snapshot(),"coverage":"fixed process-local boundaries; metrics_complete covers required core/storage/process/service/runtime observations only; no complete physical/HTTP/cache coverage claim","coverage_complete":false,"required_families":["core","storage","process","service_quiescence","server_quic","runtime_activation"],"known_unavailable_families":["sqlite_live_cache_sql","direct_sdk_raw_object_store","http_attempts","physical_iops"]})
    }
    pub async fn boundary(
        &mut self,
        fleet: &mut super::process::Fleet,
        private: &super::process::PrivateConfig,
        resources: &super::resources::Resources,
        label: (&str, &str),
        oracle: Value,
        enclosing_deadline: Instant,
    ) -> Result<Duration, String> {
        let (phase, boundary) = label;
        let started = Instant::now();
        let deadline = boundary_deadline(started, enclosing_deadline);
        if started >= deadline {
            self.complete = false;
            return Err("metric inherited phase budget exhausted".into());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("metric sequence exhausted")?;
        let sequence = self.sequence;
        let generation = fleet
            .children
            .first()
            .and_then(|c| c.ready.as_ref())
            .ok_or("metric worker readiness absent")?
            .generation;
        let root = private.output.join("metrics");
        std::fs::create_dir_all(&root).map_err(|_| "controller metrics directory unavailable")?;
        let mut expected = Vec::new();
        let mut readiness_metrics = Vec::new();
        let index = self.records.len();
        self.records.push(json!({"sequence":sequence,"generation":generation,"phase":phase,"boundary":boundary,"complete":false,"workers":[],"readiness_metrics":[],"deadline_seconds":30,"remaining_enclosing_seconds":enclosing_deadline.saturating_duration_since(started).as_secs_f64(),"effective_observer_seconds":deadline.saturating_duration_since(started).as_secs_f64()}));
        for child in &fleet.children {
            let ready = child
                .ready
                .as_ref()
                .ok_or("metric worker readiness absent")?;
            if ready.generation != generation {
                return Err("metric worker generations differ".into());
            }
            let id = identity(
                private,
                child.child.id(),
                Some(child.server),
                generation,
                sequence,
                phase,
                boundary,
            );
            if boundary == "after_ready" {
                let startup_path = child
                    .root
                    .join("metrics")
                    .join(format!("startup-g{generation}.json"));
                let startup = super::read_json(&startup_path)?;
                let expected_startup = identity(
                    private,
                    child.child.id(),
                    Some(child.server),
                    generation,
                    sequence - 1,
                    "worker_startup",
                    "ready",
                );
                validate_receipt(&startup["identity"], &expected_startup)?;
                let hash = super::file_digest(&startup_path)?;
                if ready.phase_metrics["sha256"] != hash
                    || ready.phase_metrics["file"] != format!("metrics/startup-g{generation}.json")
                {
                    return Err("startup metric readiness reference mismatch".into());
                }
                if ready.runtime_activation != startup["runtime_activation"] {
                    return Err("cold readiness runtime reference mismatch".into());
                }
                runtime_boundary(
                    &startup,
                    &private.expected_backings,
                    "worker_startup",
                    "ready",
                    child.server,
                )?;
                self.complete &= startup["metrics_complete"] == true;
                readiness_metrics.push(json!({"server":child.server,"generation":generation,"file":format!("worker-{}/metrics/startup-g{generation}.json",child.server),"sha256":hash,"metrics_complete":startup["metrics_complete"]}));
                if generation > 0 {
                    let old = generation - 1;
                    let closed_path = child
                        .root
                        .join("metrics")
                        .join(format!("closed-g{old}.json"));
                    let closed = super::read_json(&closed_path)?;
                    let expected_closed = identity(
                        private,
                        child.child.id(),
                        Some(child.server),
                        old,
                        sequence,
                        "replica_close",
                        "after",
                    );
                    validate_receipt(&closed["identity"], &expected_closed)?;
                    if !runtime_closed_boundary(&closed, &private.expected_backings, child.server)?
                    {
                        return Err("closed generation runtime observation incomplete".into());
                    }
                    self.complete &= closed["metrics_complete"] == true;
                    readiness_metrics.push(json!({"server":child.server,"generation":old,"file":format!("worker-{}/metrics/closed-g{old}.json",child.server),"sha256":super::file_digest(&closed_path)?,"metrics_complete":closed["metrics_complete"]}));
                }
            }
            let existing =
                super::read_json(&child.root.join("command.json")).unwrap_or(Value::Null);
            if existing["command"] == "stop"
                || (existing["command"] == "reopen"
                    && existing["generation"].as_u64() != Some(generation))
            {
                return Err("metric request cannot overwrite pending lifecycle command".into());
            }
            super::write_json(
                &child.root.join("command.json"),
                &json!({"command":"metrics","identity":id}),
            )?;
            expected.push(id);
            self.records[index]["readiness_metrics"] = json!(readiness_metrics);
            self.records[index]["issued_workers"] = json!(expected.len());
            if Instant::now() >= deadline {
                return Err("metric issue/publication exceeded shared deadline".into());
            }
        }
        let span = observer().begin("barrier_wait");
        let mut receipts = vec![None; fleet.children.len()];
        let result=async {
            loop {
                resources.check()?;
                // Read all known receipts before checking child loss, retaining the other nine.
                for (slot,child) in receipts.iter_mut().zip(&fleet.children) {
                    if slot.is_some(){continue;}
                    let path=child.root.join("metrics").join(format!("g{generation}-s{sequence}.json"));
                    if path.exists(){
                        let ack=super::read_json(&child.root.join("metrics-ack.json")).unwrap_or(Value::Null);
                        if ack["identity"]["sequence"].as_u64().is_none_or(|s|s<sequence){continue;}
                        validate_receipt(&ack["identity"],&expected[child.server])?;
                        let mut receipt=super::read_json(&path)?;validate_receipt(&receipt["identity"],&expected[child.server])?;
                        let hash=super::file_digest(&path)?;
                        if ack["file"]!=format!("metrics/g{generation}-s{sequence}.json") || ack["sha256"]!=hash {return Err("metric acknowledgment artifact mismatch".into());}
                        runtime_boundary(&receipt,&private.expected_backings,phase,boundary,child.server)?;
                        receipt["_artifact_sha256"]=json!(hash);*slot=Some(receipt);
                    }
                }
                self.records[index]["workers"]=json!(receipts.iter().enumerate().filter_map(|(server,r)|r.as_ref().map(|r|json!({"server":server,"pid":r["identity"]["pid"],"file":format!("worker-{server}/metrics/g{generation}-s{sequence}.json"),"metrics_complete":r["metrics_complete"],"sha256":r["_artifact_sha256"]}))).collect::<Vec<_>>());
                fleet.check()?;
                if Instant::now()>=deadline{return Err("shared metric barrier deadline".into());}
                if receipts.iter().all(Option::is_some){break;}
                tokio::time::sleep(Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now()))).await;
            }
            let identities:Vec<_>=receipts.iter().map(|r|r.as_ref().unwrap()["identity"].clone()).collect();validate_fleet(&identities,&expected)?;
            let id=identity(private,std::process::id(),None,generation,sequence,phase,boundary);
            let receipt=self.local.capture(id,None,oracle)?;
            let path=root.join(format!("g{generation}-s{sequence}.json"));publish_immutable(&path,&receipt)?;
            let metrics_complete=receipts.iter().all(|r|r.as_ref().unwrap()["metrics_complete"]==true) && receipt["metrics_complete"]==true;
            self.complete &= metrics_complete;
            self.records[index]["controller"]=json!({"file":format!("metrics/g{generation}-s{sequence}.json"),"sha256":super::file_digest(&path)?,"metrics_complete":receipt["metrics_complete"]});
            self.records[index]["metrics_complete"]=json!(metrics_complete);
            if Instant::now()>deadline{return Err("metric capture/publication exceeded shared deadline".into());}
            Ok::<_,String>(())
        }.await;
        self.records[index]["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
        self.records[index]["complete"] = json!(result.is_ok());
        self.records[index]["error"] = json!(result.as_ref().err());
        self.complete &= result.is_ok();
        span.finish(result.is_ok(), 0);
        result.map(|()| started.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> Value {
        json!({"pid":123,"role":"worker","server":2,"controller_pid":99,"generation":1,
            "sequence":4,"phase":"online_payload","boundary":"after","source_digest":"source",
            "binary_digest":"binary","catalog_digest":"catalog","backend_prefix":"fixture"})
    }
    fn runtime_fixture(generation: u64, drives: usize, active: usize) -> Value {
        let mut value = example_runtime(generation, drives);
        value["available"] = json!(true);
        value["complete"] = json!(true);
        value["pool"]["resident"] = json!(active);
        value["pool"]["ready"] = json!(active);
        value["pool"]["open_success"] = json!(active);
        for row in value["observations"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .take(active)
        {
            row["observed_backing"] = row["expected_backing"].clone();
            row["constructed"] = json!(1);
        }
        value["diagnostics"] = json!({
            "scope":"process_local_preconstructed_runtime_observer",
            "sampling_scope":"serial_atomic_loads_not_transactional_or_drain_proof",
            "histogram_scope":"inclusive_wall_time_log2_microseconds_32_buckets",
            "deferred_scope":"deferred_spans_enter_only_on_post_unlock_publication",
            "inclusive_spans_overlap":true,"concurrent_activity":false,"counter_saturated":false,
            "slow_record_attempts":0,
            "entries":RUNTIME_ROWS.map(|name|json!({"name":name,"calls":0,"success":0,"error":0,"cancelled":0,"in_flight":0,"elapsed_ns":0,"max_elapsed_ns":0,"latency_log2_us":vec![0u64;32]})),
        });
        value
    }
    #[test]
    fn runtime_capture_epoch_encloses_snapshot_callback() {
        // The zero-delay control and real delayed callback both use the public
        // collector path, without opening a provider or adding a clock seam.
        for delay in [Duration::ZERO, Duration::from_millis(25)] {
            let mut local = Local::new().unwrap();
            let mut snapshot = Some(runtime_fixture(1, 10, 0));
            let mut callback_boundary = None;
            let before = super::super::utc_ms();
            let value = local
                .capture_with_runtime(identity(), None, || {
                    let began = super::super::utc_ms();
                    let elapsed = Instant::now();
                    std::thread::sleep(delay);
                    let elapsed = elapsed.elapsed();
                    let ended = super::super::utc_ms();
                    callback_boundary = Some((began, ended, elapsed));
                    Ok(snapshot
                        .take()
                        .expect("runtime snapshot captured exactly once"))
                })
                .unwrap();
            let after = super::super::utc_ms();
            let (callback_start, callback_end, callback_elapsed) =
                callback_boundary.expect("actual callback must run");
            assert!(snapshot.is_none(), "callback must consume its snapshot");
            if !delay.is_zero() {
                assert!(
                    callback_end > callback_start,
                    "delayed control must cross an epoch millisecond boundary"
                );
            }
            let recorded_start = value["capture_started_unix_ms"].as_u64().unwrap();
            let recorded_end = value["capture_ended_unix_ms"].as_u64().unwrap();
            let recorded_elapsed = value["capture_elapsed_ns"].as_u64().unwrap();
            assert!(recorded_start >= before && recorded_end <= after);
            assert!(
                recorded_start <= callback_start,
                "capture epoch starts after runtime observation: delay={delay:?}"
            );
            assert!(recorded_end >= callback_end);
            assert!(
                recorded_end.saturating_sub(recorded_start)
                    >= callback_end.saturating_sub(callback_start),
                "capture epoch envelope omits callback time"
            );
            assert!(
                u128::from(recorded_elapsed) >= callback_elapsed.as_nanos(),
                "capture monotonic elapsed omits callback time"
            );
        }
    }
    #[test]
    fn lazy_target_runtime_shape_preserves_cold_and_actual_backing_distinction() {
        let expected: Vec<_> = (1..=10).map(|id| format!("{id:032x}")).collect();
        let cold = runtime_fixture(0, 10, 0);
        assert_eq!(validate_runtime_snapshot(&cold, 0, 10, &expected), Ok(true));
        let opened = runtime_fixture(0, 10, 1);
        assert_eq!(
            validate_runtime_snapshot(&opened, 0, 10, &expected),
            Ok(true)
        );
        let unavailable = example_runtime(0, 10);
        assert_eq!(
            validate_runtime_snapshot(&unavailable, 0, 10, &expected),
            Ok(false)
        );
        let mut invented = cold.clone();
        invented["observations"][0]["observed_backing"] = json!(expected[0]);
        assert!(validate_runtime_snapshot(&invented, 0, 10, &expected).is_err());
        for field in ["generation", "capacity"] {
            let mut bad = cold.clone();
            bad[field] = json!(999);
            assert!(validate_runtime_snapshot(&bad, 0, 10, &expected).is_err());
        }
        let mut foreign = opened.clone();
        foreign["observations"][0]["observed_backing"] = json!(expected[1]);
        assert!(validate_runtime_snapshot(&foreign, 0, 10, &expected).is_err());
        let mut unknown = cold.clone();
        unknown["private_url"] = json!("not permitted");
        assert!(validate_runtime_snapshot(&unknown, 0, 10, &expected).is_err());
    }
    #[test]
    fn lazy_target_runtime_incomplete_frames_cannot_qualify_or_invent_zero() {
        let expected: Vec<_> = (1..=10).map(|id| format!("{id:032x}")).collect();
        let complete = runtime_fixture(0, 10, 0);
        for field in ["opening", "quarantined", "pinned", "open_error"] {
            let mut value = complete.clone();
            value["pool"][field] = json!(1);
            assert!(
                validate_runtime_snapshot(&value, 0, 10, &expected).is_err(),
                "declared complete {field}"
            );
            value["complete"] = json!(false);
            assert_eq!(
                validate_runtime_snapshot(&value, 0, 10, &expected),
                Ok(false)
            );
        }
        let mut row = complete.clone();
        row["diagnostics"]["entries"][0]["calls"] = json!(1);
        assert!(validate_runtime_snapshot(&row, 0, 10, &expected).is_err());
        row["complete"] = json!(false);
        assert_eq!(validate_runtime_snapshot(&row, 0, 10, &expected), Ok(false));
        let mut malformed = complete.clone();
        malformed["pool"]["resident"] = Value::Null;
        assert!(validate_runtime_snapshot(&malformed, 0, 10, &expected).is_err());
        let mut unavailable = example_runtime(0, 10);
        unavailable["complete"] = json!(true);
        assert!(validate_runtime_snapshot(&unavailable, 0, 10, &expected).is_err());
    }
    #[test]
    fn lazy_target_runtime_delta_keeps_gauges_and_rejects_cross_generation() {
        let cold = runtime_fixture(0, 10, 0);
        let opened = runtime_fixture(0, 10, 1);
        let delta = runtime_delta(&cold, &opened).unwrap();
        assert_eq!(delta["pool"]["resident"]["before"], 0);
        assert_eq!(delta["pool"]["resident"]["after"], 1);
        assert_eq!(delta["pool"]["open_success"], 1);
        assert_eq!(delta["observations"][0]["constructed"], 1);
        assert!(runtime_delta(&opened, &cold).is_err());
        let mut next = opened;
        next["generation"] = json!(1);
        assert!(runtime_delta(&cold, &next).is_err());
    }
    #[test]
    fn lazy_target_timed_modes_accept_only_balanced_assigned_runtime_owners() {
        let expected: Vec<_> = (1..=10).map(|id| format!("{id:032x}")).collect();
        let runtime = runtime_fixture(1, 10, 1);
        let frame = json!({"identity":{"generation":1},"runtime_activation":runtime});
        for mode in ["mostly_idle", "all_active"] {
            for pattern in super::super::config::PATTERNS {
                for boundary in ["before_active", "after_active", "after_idle"] {
                    runtime_boundary(&frame, &expected, &format!("{mode}/{pattern}"), boundary, 0)
                        .expect("balanced assigned runtime owners must qualify both timed modes");
                }
            }
        }
    }

    #[test]
    fn lazy_target_timed_modes_reject_unassigned_crossnode_runtime_replication() {
        let expected: Vec<_> = (1..=10).map(|id| format!("{id:032x}")).collect();
        let runtime = runtime_fixture(1, 10, 10);
        let frame = json!({"identity":{"generation":1},"runtime_activation":runtime});
        for mode in ["mostly_idle", "all_active"] {
            assert!(
                runtime_boundary(
                    &frame,
                    &expected,
                    &format!("{mode}/sequential_read"),
                    "before_active",
                    0
                )
                .is_err(),
                "unassigned replicas must not qualify the balanced production traffic profile"
            );
        }
    }

    #[test]
    fn lazy_target_nonactivating_route_checks_require_zero_opened_owners() {
        let expected: Vec<_> = (1..=10).map(|id| format!("{id:032x}")).collect();
        let cold =
            json!({"identity":{"generation":1},"runtime_activation":runtime_fixture(1,10,0)});
        runtime_boundary(&cold, &expected, "routes_and_scope", "after", 0)
            .expect("registration/auth route coverage must leave providers dormant");
        let opened =
            json!({"identity":{"generation":1},"runtime_activation":runtime_fixture(1,10,1)});
        assert!(runtime_boundary(&opened, &expected, "routes_and_scope", "after", 0).is_err());
    }

    #[test]
    fn lazy_target_postprofile_rotation_requires_only_its_exact_assigned_owners() {
        let expected: Vec<_> = (1..=10).map(|id| format!("{id:032x}")).collect();
        // g2 is the first post-profile batch, offset1: Drive9 belongs to server0.
        let mut rotated = runtime_fixture(2, 10, 1);
        rotated["observations"][0]["constructed"] = json!(0);
        rotated["observations"][0]["observed_backing"] = Value::Null;
        rotated["observations"][9]["constructed"] = json!(1);
        rotated["observations"][9]["observed_backing"] = json!(expected[9]);
        let frame = |runtime: Value| json!({"identity":{"generation":runtime["generation"]},"runtime_activation":runtime});
        let cold = frame(runtime_fixture(2, 10, 0));
        runtime_boundary(&cold, &expected, "crossnode_routes", "after_ready", 0)
            .expect("acknowledged next generation must be cold before its rotated I/O batch");
        runtime_boundary(
            &frame(rotated.clone()),
            &expected,
            "crossnode_routes",
            "after_batch",
            0,
        )
        .expect("only the exact rotated assignment may qualify this actual crossnode batch");
        let wrong = frame(runtime_fixture(2, 10, 1));
        assert!(runtime_boundary(&wrong, &expected, "crossnode_routes", "after_batch", 0).is_err());
        let mut closed = rotated;
        closed["pool"]["resident"] = json!(0);
        closed["pool"]["ready"] = json!(0);
        assert_eq!(
            runtime_closed_boundary(&frame(closed.clone()), &expected, 0),
            Ok(true)
        );
        closed["pool"]["resident"] = json!(1);
        assert_ne!(
            runtime_closed_boundary(&frame(closed), &expected, 0),
            Ok(true),
            "next batch must not reuse an unproven previous generation close"
        );
    }

    #[test]
    fn lazy_target_closed_measured_generation_rejects_unassigned_replication() {
        let expected: Vec<_> = (1..=10).map(|id| format!("{id:032x}")).collect();
        let mut runtime = runtime_fixture(1, 10, 10);
        runtime["pool"]["resident"] = json!(0);
        runtime["pool"]["ready"] = json!(0);
        let frame = json!({"identity":{"generation":1},"runtime_activation":runtime});
        assert_ne!(
            runtime_closed_boundary(&frame, &expected, 0),
            Ok(true),
            "a successfully closed replicated profile is still the wrong production traffic geometry"
        );
    }

    #[test]
    fn lazy_target_runtime_boundary_requires_actual_primary_and_all_route_activations() {
        let expected: Vec<_> = (1..=10).map(|id| format!("{id:032x}")).collect();
        let frame = |runtime: Value| json!({"identity":{"generation":runtime["generation"]},"runtime_activation":runtime});
        let cold = frame(runtime_fixture(0, 10, 0));
        runtime_boundary(&cold, &expected, "worker_startup", "ready", 0).unwrap();
        assert!(runtime_boundary(&cold, &expected, "online_namespace", "after", 0).is_err());
        let primary = frame(runtime_fixture(0, 10, 1));
        runtime_boundary(&primary, &expected, "online_namespace", "after", 0).unwrap();
        let mut wrong_primary = primary.clone();
        wrong_primary["runtime_activation"]["observations"][0]["constructed"] = json!(0);
        wrong_primary["runtime_activation"]["observations"][0]["observed_backing"] = Value::Null;
        wrong_primary["runtime_activation"]["observations"][1]["constructed"] = json!(1);
        wrong_primary["runtime_activation"]["observations"][1]["observed_backing"] =
            json!(expected[1]);
        assert!(
            runtime_boundary(&wrong_primary, &expected, "online_namespace", "after", 0).is_err()
        );
        let generation1cold = frame(runtime_fixture(1, 10, 0));
        runtime_boundary(
            &generation1cold,
            &expected,
            "refresh_replicas",
            "after_ready",
            0,
        )
        .unwrap();
        // Route/auth coverage no longer constructs every replica before timing.
        runtime_boundary(&generation1cold, &expected, "routes_and_scope", "after", 0).unwrap();
        let assigned = frame(runtime_fixture(1, 10, 1));
        runtime_boundary(&assigned, &expected, "assigned_warmup", "after", 0).unwrap();
        let all = frame(runtime_fixture(1, 10, 10));
        assert!(runtime_boundary(&all, &expected, "assigned_warmup", "after", 0).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn inherited_boundary_budget_never_restarts_expired_or_short_phase() {
        for remaining in [Duration::ZERO, Duration::from_secs(1)] {
            let now = tokio::time::Instant::now();
            let deadline = boundary_deadline(now.into_std(), (now + remaining).into_std());
            let result =
                tokio::time::timeout_at(deadline.into(), std::future::pending::<()>()).await;
            assert!(result.is_err());
            assert!(
                now.elapsed() <= remaining + Duration::from_millis(1),
                "observer received a fresh allowance after enclosing phase budget"
            );
        }
    }
    #[test]
    fn phase_receipt_requires_exact_identity_and_sequence() {
        let expected = identity();
        validate_receipt(&expected, &expected).expect("same launched identity must validate");
        for field in [
            "pid",
            "role",
            "server",
            "controller_pid",
            "generation",
            "sequence",
            "phase",
            "boundary",
            "source_digest",
            "binary_digest",
            "catalog_digest",
            "backend_prefix",
        ] {
            let mut wrong = expected.clone();
            wrong[field] = json!("wrong");
            assert!(
                validate_receipt(&wrong, &expected).is_err(),
                "wrong {field} accepted"
            );
            wrong.as_object_mut().unwrap().remove(field);
            assert!(
                validate_receipt(&wrong, &expected).is_err(),
                "missing {field} accepted"
            );
        }
    }
    #[test]
    fn checked_counter_delta_preserves_u64_and_rejects_reset_shape_changes() {
        let before = json!([{"name":"sdk.blocks.get","calls":9007199254740993u64,"bytes":4096,"latency_log2_us":[1,0]}]);
        let after = json!([{"name":"sdk.blocks.get","calls":9007199254740995u64,"bytes":8192,"latency_log2_us":[1,2]}]);
        let delta = delta_entries(&before, &after).expect("monotonic exact counters must subtract");
        assert_eq!(delta[0]["calls"], 2);
        assert_eq!(delta[0]["bytes"], 4096);
        assert_eq!(delta[0]["latency_log2_us"], json!([0, 2]));
        assert!(delta_entries(&after, &before).is_err());
        let mut changed = after.clone();
        changed[0]["name"] = json!("different");
        assert!(delta_entries(&before, &changed).is_err());
        assert!(delta_entries(&before, &json!([])).is_err());
    }
    #[test]
    fn phase_delta_rejects_cross_process_generation_and_unobserved_end() {
        let mut before = json!({"identity":identity(),"core":{"entries":[{"name":"event","calls":2,"elapsed_ns":4,"units":8}]}});
        before["identity"]["sequence"] = json!(3);
        let mut after = before.clone();
        after["identity"]["sequence"] = json!(4);
        after["core"]["entries"][0]["calls"] = json!(3);
        let delta = phase_delta(&before, &after).expect("same process and generation pair");
        assert_eq!(delta["core"][0]["calls"], 1);
        for field in ["pid", "generation", "source_digest", "binary_digest"] {
            let mut wrong = after.clone();
            wrong["identity"][field] = json!("wrong");
            assert!(
                phase_delta(&before, &wrong).is_err(),
                "cross-identity {field}"
            );
        }
        assert!(phase_delta(&before, &Value::Null).is_err());
    }
    #[test]
    fn disabled_not_configured_unavailable_and_nonquiescent_are_distinct() {
        let cases = [
            ((false, true, true, true), "disabled"),
            ((true, false, false, true), "not_configured"),
            ((true, true, false, true), "unavailable"),
            ((true, true, true, false), "nonquiescent"),
            ((true, true, true, true), "complete"),
        ];
        for ((enabled, configured, available, quiescent), status) in cases {
            let state = family_state(enabled, configured, available, quiescent);
            assert_eq!(state["status"], status);
            assert_eq!(state["complete"], status == "complete");
        }
    }
    #[test]
    fn sequence_duplicates_reuse_evidence_and_conflicts_fail_closed() {
        let mut sequence = Sequence::default();
        let mut first = identity();
        first["sequence"] = json!(1);
        assert_eq!(sequence.accept(&first), Ok(true));
        assert_eq!(sequence.accept(&first), Ok(false));
        let mut conflicting = first.clone();
        conflicting["phase"] = json!("different");
        assert!(sequence.accept(&conflicting).is_err());
        let mut future = first.clone();
        future["sequence"] = json!(3);
        assert!(sequence.accept(&future).is_err());
        let mut next = first.clone();
        next["sequence"] = json!(2);
        assert_eq!(sequence.accept(&next), Ok(true));
        assert!(sequence.accept(&first).is_err());
    }
    #[test]
    fn all_ten_distinct_launched_workers_are_required() {
        let expected: Vec<_> = (0..10)
            .map(|server| {
                let mut value = identity();
                value["server"] = json!(server);
                value["pid"] = json!(1000 + server);
                value
            })
            .collect();
        validate_fleet(&expected, &expected).expect("all launched workers observed");
        assert!(validate_fleet(&expected[..9], &expected).is_err());
        let mut duplicate = expected.clone();
        duplicate[9] = duplicate[0].clone();
        assert!(validate_fleet(&duplicate, &expected).is_err());
        let mut stale = expected.clone();
        stale[9]["sequence"] = json!(3);
        assert!(validate_fleet(&stale, &expected).is_err());
    }
    #[tokio::test]
    async fn oracle_accounting_retains_success_and_cancelled_pending_work() {
        let accounting = Accounting::default();
        accounting.begin("data_read").finish(true, 4096);
        {
            let mut pending = Box::pin(async {
                let _span = accounting.begin("data_read");
                std::future::pending::<()>().await;
            });
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(pending.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        }
        let snapshot = accounting.snapshot();
        assert_eq!(snapshot["data_read"]["calls"], 2);
        assert_eq!(snapshot["data_read"]["success"], 1);
        assert_eq!(snapshot["data_read"]["cancelled"], 1);
        assert_eq!(snapshot["data_read"]["bytes"], 4096);
        assert_eq!(snapshot["data_read"]["in_flight"], 0);
    }
    #[test]
    fn phase_optional_allocations_are_checked_and_gauges_are_endpoints() {
        let mut before = json!({"identity":identity(),"core":{"entries":[]},"process_since_baseline":{
            "cpu_user_us":0,"cpu_system_us":0,"minor_faults":0,"major_faults":0,"block_inputs":0,"block_outputs":0,"voluntary_context_switches":0,"involuntary_context_switches":0,
            "rust_allocator_instrumented":true,"rust_allocations":2,"rust_deallocations":1,"rust_reallocations":0,"rust_allocated_bytes":9007199254740993u64,"rust_freed_bytes":8,"rss_end_bytes":100,"lifetime_peak_rss_bytes":120,"sqlite_heap_end_bytes":12,"sqlite_heap_lifetime_peak_bytes":16,"rust_live_end_bytes":32}});
        before["identity"]["sequence"] = json!(3);
        let mut after = before.clone();
        after["identity"]["sequence"] = json!(4);
        after["process_since_baseline"]["rust_allocated_bytes"] = json!(9007199254740995u64);
        after["process_since_baseline"]["rss_end_bytes"] = json!(90);
        let delta = phase_delta(&before, &after).unwrap();
        assert_eq!(delta["allocations"]["counters"]["rust_allocated_bytes"], 2);
        assert_eq!(
            delta["process_gauges"]["rss_end_bytes"],
            json!({"before":100,"after":90})
        );
        after["process_since_baseline"]["rust_allocated_bytes"] = json!(1);
        assert!(phase_delta(&before, &after).is_err());
        before["process_since_baseline"]["rust_allocator_instrumented"] = json!(false);
        after["process_since_baseline"]["rust_allocator_instrumented"] = json!(false);
        assert_eq!(
            phase_delta(&before, &after).unwrap()["allocations"]["available"],
            false
        );
    }
    #[test]
    fn application_activity_must_stay_quiescent_across_local_snapshot_window() {
        let sample = json!({"complete":true,"application_quiescent":true,"activity_sequence_before":10,"activity_sequence_after":10,"activity_writers_before":0,"activity_writers_after":0,"active_requests_before":0,"active_requests_after":0,"active_handshakes_before":0,"active_handshakes_after":0});
        assert!(quiescent_window(&sample, &sample));
        let mut changed = sample.clone();
        changed["activity_sequence_before"] = json!(12);
        changed["activity_sequence_after"] = json!(12);
        assert!(!quiescent_window(&sample, &changed));
        for field in [
            "activity_writers_after",
            "active_requests_after",
            "active_handshakes_after",
        ] {
            let mut changed = sample.clone();
            changed[field] = json!(1);
            assert!(!quiescent_window(&sample, &changed));
        }
        assert!(!quiescent_window(&sample, &Value::Null));
    }
    #[test]
    fn server_transport_delta_retains_retirement_and_rejects_incomplete_registry() {
        let mut before = json!({"identity":identity(),"core":{"entries":[]},"server_quic":{"entries":[],"transport":{"registry_complete":true,"observed_total":{"udp_tx_bytes":100,"frame_tx":{"stream":10}},"retired_connections":0,"unobserved_connections":0,"missing_final_samples":0}}});
        before["identity"]["sequence"] = json!(3);
        let mut after = before.clone();
        after["identity"]["sequence"] = json!(4);
        after["server_quic"]["transport"]["observed_total"] =
            json!({"udp_tx_bytes":120,"frame_tx":{"stream":12}});
        after["server_quic"]["transport"]["retired_connections"] = json!(1);
        let delta = phase_delta(&before, &after).unwrap();
        assert_eq!(
            delta["server_transport"]["observed_total"]["udp_tx_bytes"],
            20
        );
        assert_eq!(
            delta["server_transport"]["observed_total"]["frame_tx"]["stream"],
            2
        );
        after["server_quic"]["transport"]["registry_complete"] = json!(false);
        assert!(phase_delta(&before, &after).is_err());
    }
    #[test]
    fn disabled_accounting_span_does_not_retain_recorder() {
        let accounting = Accounting::new(false);
        let before = Arc::strong_count(&accounting.counters);
        let span = accounting.begin("data_read");
        assert_eq!(
            Arc::strong_count(&accounting.counters),
            before,
            "disabled span must not retain recorder work"
        );
        span.finish(true, 4096);
        assert_eq!(accounting.snapshot()["data_read"]["calls"], 0);
    }
    #[test]
    fn accounting_overflow_is_incomplete_instead_of_panic_or_wrap() {
        let accounting = Accounting::default();
        let index = CATEGORIES
            .iter()
            .position(|name| *name == "data_read")
            .unwrap();
        accounting.counters.lock().unwrap()[index].calls = u64::MAX;
        let result = std::panic::catch_unwind(|| accounting.begin("data_read").finish(true, 4096));
        assert!(result.is_ok(), "counter overflow must not panic");
        let snapshot = accounting.snapshot();
        assert_eq!(snapshot["data_read"]["calls"], u64::MAX);
        assert_eq!(snapshot["_status"]["counter_saturated"], true);
        assert_eq!(snapshot["_status"]["complete"], false);
    }
    #[test]
    fn phase_receipt_publication_never_overwrites_prior_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("g1-s4.json");
        let first = json!({"identity":identity(),"complete":false,"reason":"nonquiescent"});
        publish_immutable(&path, &first).expect("first immutable receipt must publish");
        let bytes = std::fs::read(&path).unwrap();
        assert!(publish_immutable(&path, &json!({"complete":true})).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}
