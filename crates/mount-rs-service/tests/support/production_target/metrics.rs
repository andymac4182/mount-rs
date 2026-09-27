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
        let result = self.capture_inner(identity, service, oracle);
        span.finish(result.is_ok(), 0);
        result
    }
    fn capture_inner(
        &mut self,
        identity: Value,
        service: Option<&mount_rs_service::server::ServerDiagnostics>,
        oracle: Value,
    ) -> Result<Value, String> {
        let start = Instant::now();
        let started = super::utc_ms();
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
        let accounting_complete =
            observer().complete() && (oracle.is_null() || oracle["_status"]["complete"] == true);
        let elapsed = start.elapsed();
        let mut value = json!({"schema":"mount-rs-phase-metrics-v1","identity":identity,"enabled":enabled,
            "capture_started_unix_ms":started,"capture_ended_unix_ms":super::utc_ms(),"capture_elapsed_ns":elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,"process_observation_elapsed_seconds":self.started.elapsed().as_secs_f64(),
            "capture_complete":!enabled || elapsed<=Duration::from_secs(30),"metrics_complete":enabled && quiescent && accounting_complete && elapsed<=Duration::from_secs(30),"accounting_complete":accounting_complete,
            "quiescence":{"controller_work_drained":true,"service_observed":service.is_some(),"application_quiescent":application_quiescent,"instrumented_storage_in_flight_zero":storage_quiescent,"scope":"owned controller work drained; only instrumented service/storage activity observed; no global atomic cut or proof of all provider/background work"},
            "core":core,"storage":storage,"process_since_baseline":process,"process_since_previous_boundary":process_interval,"server_quic":service,"server_quic_before_local_capture":service_envelope,"oracle":oracle,
            "coverage":{"core":family_state(enabled,true,true,quiescent),"storage":family_state(enabled,true,true,quiescent),"process":family_state(enabled,true,true,true),
                "server_quic":family_state(enabled,worker,service.is_some(),quiescent),
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
                    let complete =
                        valid.is_ok() && value["capture_complete"] == true && child.reaped;
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
        json!({"enabled":mount_rs_core::diagnostics::profile::enabled(),"metrics_complete":self.qualified(),"boundaries":self.records,"observer":observer().snapshot(),"coverage":"fixed process-local boundaries; metrics_complete covers required core/storage/process/service observations only; no complete physical/HTTP/cache coverage claim","coverage_complete":false,"required_families":["core","storage","process","service_quiescence","server_quic"],"known_unavailable_families":["sqlite_live_cache_sql","direct_sdk_raw_object_store","http_attempts","physical_iops"]})
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
