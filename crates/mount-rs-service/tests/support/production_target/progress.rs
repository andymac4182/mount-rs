//! Fixed public scalars from the existing controller journal. No workload policy.
use super::config::{Config, SERVERS};
use mount_rs_service::startup::{PROGRESS_INTERVAL, write_bounded};
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{self, Write},
    time::Instant,
};

const LIMIT: usize = 4096;
const RESOURCE_LIMIT: usize = 2048;
#[derive(Clone, Copy)]
struct ResourceSource {
    revision: [u8; 40],
    digest: [u8; 64],
    binary: [u8; 64],
}
fn fixed_hex<const N: usize>(value: &Value) -> Option<[u8; N]> {
    let text = value.as_str()?;
    if text.len() != N
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    text.as_bytes().try_into().ok()
}
#[derive(Clone, Copy)]
struct ResourceObservation {
    observed: u64,
    samples: u64,
    terminal: Option<bool>,
    cpu_user: u64,
    cpu_system: u64,
    rss_current: u64,
    rss_lifetime: u64,
    rss_peak: u64,
    free: u64,
    block_inputs: u64,
    block_outputs: u64,
}
impl ResourceObservation {
    fn parse(value: &Value, pid: u32, published: u64) -> Result<Self, &'static str> {
        let number = |value: &Value| value.as_u64().ok_or("invalid_sample");
        if number(&value["pid"])? != u64::from(pid) {
            return Err("foreign_pid");
        }
        match value.get("error") {
            Some(Value::Null) => {}
            Some(Value::String(_)) => return Err("resource_validation_failed"),
            _ => return Err("invalid_sample"),
        }
        let observed = number(&value["observed_unix_ms"])?;
        if observed == 0 || observed > published || published - observed > 10000 {
            return Err("stale_sample");
        }
        let samples = number(&value["samples"])?;
        if samples == 0 || value["sample_interval_ms"].as_u64() != Some(100) {
            return Err("invalid_sample");
        }
        let terminal = match value.get("terminal_sample") {
            Some(Value::Bool(value)) => Some(*value),
            None | Some(Value::Null) if samples == 1 => None,
            _ => return Err("invalid_sample"),
        };
        let delta = &value["process_delta"];
        let result = Self {
            observed,
            samples,
            terminal,
            cpu_user: number(&delta["cpu_user_us"])?,
            cpu_system: number(&delta["cpu_system_us"])?,
            rss_current: number(&delta["rss_end_bytes"])?,
            rss_lifetime: number(&delta["lifetime_peak_rss_bytes"])?,
            rss_peak: number(&value["peak_rss_bytes"])?,
            free: number(&value["minimum_host_free_bytes"])?,
            block_inputs: number(&delta["block_inputs"])?,
            block_outputs: number(&delta["block_outputs"])?,
        };
        super::resources::validate_sample(value, pid, published)
            .map_err(|_| "resource_validation_failed")?;
        Ok(result)
    }
    fn follows(self, previous: Self) -> bool {
        self.samples >= previous.samples
            && self.cpu_user >= previous.cpu_user
            && self.cpu_system >= previous.cpu_system
            && self.rss_peak >= previous.rss_peak
            && self.rss_lifetime >= previous.rss_lifetime
            && self.free <= previous.free
            && self.block_inputs >= previous.block_inputs
            && self.block_outputs >= previous.block_outputs
            && (previous.terminal != Some(true) || self.terminal == Some(true))
    }
}
#[derive(Serialize)]
struct ResourceRecord<'a> {
    schema: &'static str,
    controller_pid: u32,
    source_revision: &'a str,
    source_digest: &'a str,
    binary_sha256: &'a str,
    role: &'static str,
    pid: u32,
    worker: Option<usize>,
    generation_context: Option<u64>,
    phase: &'static str,
    observed_unix_ms: Option<u64>,
    published_unix_ms: u64,
    samples: Option<u64>,
    sample_interval_ms: u64,
    terminal_sample: Option<bool>,
    available: bool,
    reason: Option<&'static str>,
    counter_scope: &'static str,
    cpu_user_us: Option<u64>,
    cpu_system_us: Option<u64>,
    rss_current_bytes: Option<u64>,
    rss_lifetime_peak_bytes: Option<u64>,
    rss_peak_bytes: Option<u64>,
    minimum_host_free_bytes: Option<u64>,
    block_inputs: Option<u64>,
    block_outputs: Option<u64>,
    process_disk_read_bytes: Option<u64>,
    process_disk_write_bytes: Option<u64>,
    process_disk_bytes_reason: &'static str,
}
#[derive(Serialize)]
struct Record<'a> {
    schema: &'static str,
    event: &'static str,
    pid: u32,
    elapsed_ns: u64,
    mode: &'static str,
    provider: &'static str,
    servers: u64,
    clients: u64,
    drives: u64,
    partitions: u64,
    files_per_drive: u64,
    phase_seconds: u64,
    phase: &'static str,
    source_revision: Option<&'a str>,
    source_verified: bool,
    host_free_bytes: Option<u64>,
    initialized_drives: u64,
    connected_clients: u64,
    outcome: &'static str,
    accounting_complete: bool,
}
struct State {
    started: Instant,
    next: Instant,
    mode: &'static str,
    provider: &'static str,
    drives: u64,
    files: u64,
    seconds: u64,
    phase: &'static str,
    revision: Option<[u8; 40]>,
    free: Option<u64>,
    initialized: u64,
    connected: u64,
    outcome: &'static str,
    complete: bool,
    terminal: bool,
    cleanup_context: bool,
    resource_source: Option<ResourceSource>,
    resource_pending: bool,
    resource_last: [Option<(u32, ResourceObservation)>; SERVERS + 1],
}
/// None is a zero-clock, zero-timer, zero-allocation disabled path.
pub struct Progress {
    state: Option<State>,
}
#[derive(Clone, Copy)]
pub struct ResourceOwner {
    pub pid: u32,
    pub worker: Option<usize>,
    pub generation_context: Option<u64>,
}
impl Progress {
    pub fn resources_due(&self) -> bool {
        self.state.as_ref().is_some_and(|state| {
            !state.terminal && state.resource_source.is_some() && state.resource_pending
        })
    }
    /// Logging context only: private workload phases and deadlines remain unchanged.
    pub fn cleanup_context(&mut self) {
        let Some(state) = &mut self.state else { return };
        if state.terminal || state.cleanup_context {
            return;
        }
        state.cleanup_context = true;
        state.phase = "terminal";
        self.emit("phase");
    }
    pub fn resource_tick(&mut self, now: Instant) {
        if self.state.as_ref().is_some_and(|state| {
            !state.terminal
                && state.resource_source.is_some()
                && !state.resource_pending
                && now >= state.next
        }) {
            self.emit("progress");
        }
    }
    pub fn resource_boundary(&mut self) {
        if self.state.as_ref().is_some_and(|state| {
            !state.terminal && state.resource_source.is_some() && !state.resource_pending
        }) {
            self.emit("progress");
        }
    }
    pub fn resources_done(&mut self) {
        if let Some(state) = &mut self.state {
            state.resource_pending = false;
        }
    }
    /// Returns true only after publishing a validated terminal observation.
    pub fn resource(
        &mut self,
        owner: ResourceOwner,
        read: impl FnOnce() -> Result<Option<Value>, String>,
    ) -> bool {
        self.resource_required(owner, read, false)
    }
    pub fn terminal_resource(
        &mut self,
        owner: ResourceOwner,
        read: impl FnOnce() -> Result<Option<Value>, String>,
    ) -> bool {
        self.resource_required(owner, read, true)
    }
    fn resource_required(
        &mut self,
        owner: ResourceOwner,
        read: impl FnOnce() -> Result<Option<Value>, String>,
        require_terminal: bool,
    ) -> bool {
        if !self.resources_due() {
            return false;
        }
        let capture = super::metrics::observer().begin("metric_capture");
        let sample = read();
        let published = super::utc_ms();
        let success = self.resource_to_required(
            owner,
            || sample,
            published,
            &mut io::stderr().lock(),
            require_terminal,
        );
        capture.finish(success, 0);
        if !success {
            return false;
        }
        let slot = owner.worker.map_or(0, |worker| worker + 1);
        self.state.as_ref().unwrap().resource_last[slot]
            .is_some_and(|(_, sample)| sample.terminal == Some(true))
    }
    #[cfg(test)]
    fn resource_to(
        &mut self,
        owner: ResourceOwner,
        read: impl FnOnce() -> Result<Option<Value>, String>,
        published: u64,
        writer: &mut impl Write,
    ) -> bool {
        self.resource_to_required(owner, read, published, writer, false)
    }
    fn resource_to_required(
        &mut self,
        owner: ResourceOwner,
        read: impl FnOnce() -> Result<Option<Value>, String>,
        published: u64,
        writer: &mut impl Write,
        require_terminal: bool,
    ) -> bool {
        if !self.resources_due() {
            return true;
        }
        if owner.pid == 0
            || owner.worker.is_some_and(|worker| worker >= SERVERS)
            || (owner.worker.is_none()
                && (owner.pid != std::process::id() || owner.generation_context.is_some()))
            || published == 0
        {
            self.state.as_mut().unwrap().complete = false;
            return false;
        }
        let sample = read();
        let state = self.state.as_mut().unwrap();
        let slot = owner.worker.map_or(0, |worker| worker + 1);
        let observation = match sample {
            Ok(Some(value)) => ResourceObservation::parse(&value, owner.pid, published),
            Ok(None) => Err("missing_sample"),
            Err(_) => Err("invalid_sample"),
        }
        .and_then(|observation| {
            if require_terminal && observation.terminal != Some(true) {
                return Err("resource_validation_failed");
            }
            if state.resource_last[slot]
                .is_some_and(|(pid, previous)| pid != owner.pid || !observation.follows(previous))
            {
                Err("invalid_sample")
            } else {
                Ok(observation)
            }
        });
        let (observation, reason) = match observation {
            Ok(observation) => {
                state.resource_last[slot] = Some((owner.pid, observation));
                (Some(observation), None)
            }
            Err(reason) => {
                state.complete = false;
                (None, Some(reason))
            }
        };
        let source = state.resource_source.as_ref().unwrap();
        let record = ResourceRecord {
            schema: "mount-rs.resource-progress.v1",
            controller_pid: std::process::id(),
            source_revision: std::str::from_utf8(&source.revision).unwrap(),
            source_digest: std::str::from_utf8(&source.digest).unwrap(),
            binary_sha256: std::str::from_utf8(&source.binary).unwrap(),
            role: if owner.worker.is_some() {
                "worker"
            } else {
                "controller"
            },
            pid: owner.pid,
            worker: owner.worker,
            generation_context: owner.generation_context,
            phase: state.phase,
            observed_unix_ms: observation.map(|v| v.observed),
            published_unix_ms: published,
            samples: observation.map(|v| v.samples),
            sample_interval_ms: 100,
            terminal_sample: observation.and_then(|v| v.terminal),
            available: observation.is_some(),
            reason,
            counter_scope: "sampler_baseline_cumulative_process",
            cpu_user_us: observation.map(|v| v.cpu_user),
            cpu_system_us: observation.map(|v| v.cpu_system),
            rss_current_bytes: observation.map(|v| v.rss_current),
            rss_lifetime_peak_bytes: observation.map(|v| v.rss_lifetime),
            rss_peak_bytes: observation.map(|v| v.rss_peak),
            minimum_host_free_bytes: observation.map(|v| v.free),
            block_inputs: observation.map(|v| v.block_inputs),
            block_outputs: observation.map(|v| v.block_outputs),
            process_disk_read_bytes: None,
            process_disk_write_bytes: None,
            process_disk_bytes_reason: "not_captured_by_sampler",
        };
        let publication = super::metrics::observer().begin("receipt_publication");
        let result = write_bounded::<RESOURCE_LIMIT>(writer, b"\nresource_progress ", &record);
        publication.finish(result.is_ok(), 0);
        if result.is_err() {
            state.complete = false;
        }
        observation.is_some() && result.is_ok()
    }
    pub fn disabled() -> Self {
        Self { state: None }
    }
    pub fn new(enabled: bool, config: &Config) -> Self {
        Self {
            state: enabled.then(|| {
                let now = Instant::now();
                State {
                    started: now,
                    next: now,
                    mode: if config.full_target {
                        "full"
                    } else {
                        "control"
                    },
                    provider: if config.provider == "tidb" {
                        "tidb"
                    } else {
                        "sqlite"
                    },
                    drives: config.drives as u64,
                    files: config.files as u64,
                    seconds: config.seconds,
                    phase: "preflight",
                    revision: None,
                    free: None,
                    initialized: 0,
                    connected: 0,
                    outcome: "running",
                    complete: true,
                    terminal: false,
                    cleanup_context: false,
                    resource_source: None,
                    resource_pending: false,
                    resource_last: [None; SERVERS + 1],
                }
            }),
        }
    }
    fn record(&self, event: &'static str) -> Option<Record<'_>> {
        let state = self.state.as_ref()?;
        let revision = state
            .revision
            .as_ref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok());
        Some(Record {
            schema: "mount-rs.target-progress.v1",
            event,
            pid: std::process::id(),
            elapsed_ns: state.started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            mode: state.mode,
            provider: state.provider,
            servers: SERVERS as u64,
            clients: state.drives,
            drives: state.drives,
            partitions: state.drives / 2,
            files_per_drive: state.files,
            phase_seconds: state.seconds,
            phase: state.phase,
            source_revision: revision,
            source_verified: revision.is_some(),
            host_free_bytes: state.free,
            initialized_drives: state.initialized,
            connected_clients: state.connected,
            outcome: state.outcome,
            accounting_complete: state.complete,
        })
    }
    fn emit_to(&mut self, event: &'static str, writer: &mut impl Write) {
        let Some(record) = self.record(event) else {
            return;
        };
        let result = write_bounded::<LIMIT>(writer, b"\ntarget_progress ", &record);
        if let Some(state) = &mut self.state {
            state.next = Instant::now() + PROGRESS_INTERVAL;
            state.resource_pending = true;
            if result.is_err() {
                state.complete = false;
            }
        }
    }
    fn emit(&mut self, event: &'static str) {
        self.emit_to(event, &mut io::stderr().lock());
    }
    pub fn start(&mut self) {
        self.emit("controller_start");
    }
    pub fn source(&mut self, source: &Value) {
        let Some(state) = &mut self.state else {
            return;
        };
        let revision = source["revision"].as_str().unwrap_or("");
        if state.revision.is_some()
            || source["checkout_status"].as_str() != Some("")
            || revision.len() != 40
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            state.complete = false;
            return;
        }
        state.revision = Some(
            revision
                .as_bytes()
                .try_into()
                .expect("validated revision length"),
        );
        state.resource_source = fixed_hex::<64>(&source["digest"])
            .zip(fixed_hex::<64>(&source["binary_sha256"]))
            .map(|(digest, binary)| ResourceSource {
                revision: state.revision.unwrap(),
                digest,
                binary,
            });
        if state.resource_source.is_none() {
            state.complete = false;
        }
        self.emit("source_verified");
    }
    pub fn capacity(&mut self, bytes: u64) {
        let Some(state) = &mut self.state else {
            return;
        };
        state.free = Some(bytes);
        self.emit("capacity");
    }
    pub fn observe(&mut self, journal: &Value) -> bool {
        let Some(state) = &mut self.state else {
            return false;
        };
        if state.terminal {
            return false;
        }
        let current = if state.cleanup_context {
            Some("terminal")
        } else {
            journal["phase"].as_str().and_then(phase)
        };
        let Some(current) = current else {
            state.complete = false;
            return false;
        };
        let changed = state.phase != current;
        state.phase = current;
        let initialized = journal["initialization"]["initialized_drives"]
            .as_u64()
            .unwrap_or(0);
        let connected = journal["connected_clients"].as_u64().unwrap_or(0);
        if initialized < state.initialized
            || connected < state.connected
            || initialized > state.drives
            || connected > state.drives
        {
            state.complete = false;
        }
        state.initialized = initialized;
        state.connected = connected;
        if changed || Instant::now() >= state.next {
            self.emit(if changed { "phase" } else { "progress" });
            true
        } else {
            false
        }
    }
    pub fn complete(&self) -> bool {
        self.state.as_ref().is_none_or(|state| state.complete)
    }
    pub fn finish(&mut self, success: bool, complete: bool) {
        let Some(state) = &mut self.state else {
            return;
        };
        if state.terminal {
            return;
        }
        state.terminal = true;
        state.phase = "terminal";
        state.outcome = if success { "success" } else { "error" };
        state.complete &= complete && success;
        self.emit("terminal");
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        let Some(state) = &mut self.state else {
            return;
        };
        if state.terminal {
            return;
        }
        state.terminal = true;
        state.phase = "terminal";
        state.outcome = "cancelled";
        state.complete = false;
        self.emit("terminal");
    }
}
fn phase(value: &str) -> Option<&'static str> {
    const FIXED: [&str; 15] = [
        "preflight",
        "empty_drive_initialization",
        "worker_setup",
        "injected_timeout",
        "signed_connections",
        "online_namespace",
        "online_payload",
        "initial_fresh_oracle",
        "refresh_replicas",
        "routes_and_scope",
        "assigned_warmup",
        "crossnode_routes",
        "final_fresh_oracle",
        "revocation",
        "terminal",
    ];
    if let Some(found) = FIXED.iter().find(|candidate| **candidate == value) {
        return Some(found);
    }
    const MODES: [[&str; 8]; 2] = [
        [
            "mostly_idle/sequential_read",
            "mostly_idle/random_read",
            "mostly_idle/sequential_overwrite",
            "mostly_idle/random_overwrite",
            "mostly_idle/mixed",
            "mostly_idle/hot_file",
            "mostly_idle/append_truncate",
            "mostly_idle/churn",
        ],
        [
            "all_active/sequential_read",
            "all_active/random_read",
            "all_active/sequential_overwrite",
            "all_active/random_overwrite",
            "all_active/mixed",
            "all_active/hot_file",
            "all_active/append_truncate",
            "all_active/churn",
        ],
    ];
    MODES
        .into_iter()
        .flatten()
        .find(|candidate| *candidate == value)
}
#[cfg(test)]
mod tests {
    use super::super::config::PATTERNS;
    use super::*;
    use serde_json::json;
    fn config() -> Config {
        Config {
            full_target: true,
            drives: 10000,
            files: 1000,
            seconds: 30,
            provider: "sqlite".into(),
        }
    }
    fn resource_source() -> Value {
        json!({"revision":"a".repeat(40),"checkout_status":"","digest":"b".repeat(64),"binary_sha256":"c".repeat(64)})
    }
    fn resource_sample(pid: u32, now: u64) -> Value {
        json!({"pid":pid,"samples":1,"sample_interval_ms":100,"observed_unix_ms":now,"peak_rss_bytes":4096,"minimum_host_free_bytes":super::super::config::DISK_FLOOR,"error":null,"process_delta":{"cpu_user_us":9007199254740993u64,"cpu_system_us":2,"rss_end_bytes":2048,"lifetime_peak_rss_bytes":4096,"block_inputs":9007199254740995u64,"block_outputs":7},"private_key":"RESOURCE_PRIVATE_SENTINEL"})
    }
    #[test]
    fn resource_progress_projects_closed_lossless_owned_scalars() {
        let mut progress = Progress::new(true, &config());
        progress.source(&resource_source());
        let mut bytes = Vec::new();
        progress.resource_to(
            ResourceOwner {
                pid: std::process::id(),
                worker: None,
                generation_context: None,
            },
            || Ok(Some(resource_sample(std::process::id(), 1000))),
            1000,
            &mut bytes,
        );
        assert!(
            bytes.starts_with(b"\nresource_progress "),
            "resource projection missing"
        );
        let value: Value =
            serde_json::from_slice(bytes.strip_prefix(b"\nresource_progress ").unwrap()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 29);
        assert_eq!(value["schema"], "mount-rs.resource-progress.v1");
        assert_eq!(value["cpu_user_us"].as_u64(), Some(9007199254740993));
        assert_eq!(value["block_inputs"].as_u64(), Some(9007199254740995));
        assert_eq!(value["terminal_sample"], Value::Null);
        assert_eq!(value["available"], true);
        assert_eq!(value["reason"], Value::Null);
        assert_eq!(value["process_disk_read_bytes"], Value::Null);
        assert_eq!(value["process_disk_write_bytes"], Value::Null);
        assert_eq!(
            value["process_disk_bytes_reason"],
            "not_captured_by_sampler"
        );
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("RESOURCE_PRIVATE_SENTINEL")
        );
        progress.finish(false, false);
    }
    #[test]
    fn resource_progress_redacts_unavailable_private_errors() {
        let mut progress = Progress::new(true, &config());
        progress.source(&resource_source());
        let mut sample = resource_sample(std::process::id(), 1000);
        sample["error"] = json!("RESOURCE_PRIVATE_SENTINEL");
        let mut bytes = Vec::new();
        progress.resource_to(
            ResourceOwner {
                pid: std::process::id(),
                worker: None,
                generation_context: None,
            },
            || Ok(Some(sample)),
            1000,
            &mut bytes,
        );
        assert!(
            bytes.starts_with(b"\nresource_progress "),
            "unavailable projection missing"
        );
        let value: Value =
            serde_json::from_slice(bytes.strip_prefix(b"\nresource_progress ").unwrap()).unwrap();
        assert_eq!(value["available"], false);
        assert_eq!(value["reason"], "resource_validation_failed");
        for field in [
            "observed_unix_ms",
            "samples",
            "terminal_sample",
            "cpu_user_us",
            "cpu_system_us",
            "rss_current_bytes",
            "rss_lifetime_peak_bytes",
            "rss_peak_bytes",
            "minimum_host_free_bytes",
            "block_inputs",
            "block_outputs",
        ] {
            assert!(value[field].is_null(), "unavailable scalar leaked: {field}");
        }
        assert!(!progress.complete());
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("RESOURCE_PRIVATE_SENTINEL")
        );
        progress.finish(false, false);
    }
    #[test]
    fn resource_progress_disabled_never_reads_the_source_or_sink() {
        struct Fail;
        impl Write for Fail {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                panic!("disabled resource sink used")
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("disabled resource sink flushed")
            }
        }
        let mut progress = Progress::new(false, &config());
        progress.resource(
            ResourceOwner {
                pid: 1,
                worker: None,
                generation_context: None,
            },
            || panic!("disabled resource publication read"),
        );
        progress.resource_to(
            ResourceOwner {
                pid: 1,
                worker: None,
                generation_context: None,
            },
            || panic!("disabled resource source read"),
            1000,
            &mut Fail,
        );
        assert!(progress.complete());
    }
    fn project_sample(progress: &mut Progress, sample: Result<Option<Value>, String>) -> Value {
        let mut bytes = Vec::new();
        progress.resource_to(
            ResourceOwner {
                pid: std::process::id(),
                worker: None,
                generation_context: None,
            },
            || sample,
            20000,
            &mut bytes,
        );
        serde_json::from_slice(bytes.strip_prefix(b"\nresource_progress ").unwrap()).unwrap()
    }
    #[test]
    fn resource_progress_strict_shape_identity_time_and_error_controls() {
        let baseline = resource_sample(std::process::id(), 20000);
        let mut cases = vec![
            (Ok(None), "missing_sample"),
            (Err("PRIVATE_READ_ERROR".into()), "invalid_sample"),
        ];
        for (pointer, replacement, reason) in [
            (
                "/pid",
                json!(u64::from(std::process::id()) + 1),
                "foreign_pid",
            ),
            ("/observed_unix_ms", json!(9999), "stale_sample"),
            ("/observed_unix_ms", json!(20001), "stale_sample"),
            ("/error", json!(false), "invalid_sample"),
            ("/samples", json!(true), "invalid_sample"),
            ("/samples", json!(0), "invalid_sample"),
            ("/sample_interval_ms", json!(99), "invalid_sample"),
            ("/process_delta/block_inputs", json!(-1), "invalid_sample"),
            ("/process_delta/block_outputs", json!(1.5), "invalid_sample"),
            ("/process_delta/cpu_user_us", Value::Null, "invalid_sample"),
            ("/peak_rss_bytes", json!(1), "resource_validation_failed"),
            (
                "/minimum_host_free_bytes",
                json!(0),
                "resource_validation_failed",
            ),
            (
                "/process_delta/lifetime_peak_rss_bytes",
                json!(super::super::config::RSS_CAP + 1),
                "resource_validation_failed",
            ),
        ] {
            let mut value = baseline.clone();
            *value.pointer_mut(pointer).unwrap() = replacement;
            cases.push((Ok(Some(value)), reason));
        }
        // An absent ordinary terminal flag and a malformed ordinary flag both fail closed.
        let mut missing_terminal = baseline.clone();
        missing_terminal["samples"] = json!(2);
        cases.push((Ok(Some(missing_terminal)), "invalid_sample"));
        let mut bad_terminal = baseline.clone();
        bad_terminal["terminal_sample"] = json!("PRIVATE_TERMINAL");
        cases.push((Ok(Some(bad_terminal)), "invalid_sample"));
        for (sample, reason) in cases {
            let mut progress = Progress::new(true, &config());
            progress.source(&resource_source());
            let value = project_sample(&mut progress, sample);
            assert_eq!(value["available"], false);
            assert_eq!(value["reason"], reason);
            assert_eq!(value["samples"], Value::Null);
            assert_eq!(value["block_inputs"], Value::Null);
            assert!(!serde_json::to_string(&value).unwrap().contains("PRIVATE"));
            assert!(!progress.complete());
            progress.finish(false, false);
        }
    }
    #[test]
    fn resource_progress_cumulative_counters_survive_context_changes_and_unavailability() {
        let mut progress = Progress::new(true, &config());
        progress.source(&resource_source());
        let first = resource_sample(std::process::id(), 20000);
        assert_eq!(
            project_sample(&mut progress, Ok(Some(first.clone())))["available"],
            true
        );
        assert_eq!(
            project_sample(&mut progress, Ok(None))["reason"],
            "missing_sample"
        );
        for pointer in [
            "/samples",
            "/process_delta/cpu_user_us",
            "/process_delta/cpu_system_us",
            "/process_delta/block_inputs",
            "/process_delta/block_outputs",
            "/peak_rss_bytes",
        ] {
            let mut regressed = first.clone();
            *regressed.pointer_mut(pointer).unwrap() = json!(0);
            assert_eq!(
                project_sample(&mut progress, Ok(Some(regressed)))["available"],
                false
            );
        }
        let mut higher_disk = first.clone();
        higher_disk["minimum_host_free_bytes"] = json!(super::super::config::DISK_FLOOR + 1);
        assert_eq!(
            project_sample(&mut progress, Ok(Some(higher_disk)))["reason"],
            "invalid_sample"
        );
        let mut lower_lifetime = first.clone();
        lower_lifetime["process_delta"]["lifetime_peak_rss_bytes"] = json!(1024);
        assert_eq!(
            project_sample(&mut progress, Ok(Some(lower_lifetime)))["reason"],
            "invalid_sample"
        );
        let mut lower_current = first.clone();
        lower_current["process_delta"]["rss_end_bytes"] = json!(1024);
        assert_eq!(
            project_sample(&mut progress, Ok(Some(lower_current)))["available"],
            true
        );
        let mut next = first;
        next["samples"] = json!(2);
        next["terminal_sample"] = json!(false);
        assert_eq!(
            project_sample(&mut progress, Ok(Some(next)))["available"],
            true
        );
        assert!(
            !progress.complete(),
            "later valid samples cannot clear prior failure"
        );
        progress.finish(false, false);
    }
    #[test]
    fn resource_progress_terminal_observation_cannot_regress_to_running_or_initial() {
        for later in [json!(false), Value::Null] {
            let mut progress = Progress::new(true, &config());
            progress.source(&resource_source());
            let mut terminal = resource_sample(std::process::id(), 20000);
            terminal["terminal_sample"] = json!(true);
            assert_eq!(
                project_sample(&mut progress, Ok(Some(terminal.clone())))["available"],
                true
            );
            terminal["terminal_sample"] = later;
            let regressed = project_sample(&mut progress, Ok(Some(terminal)));
            assert_eq!(regressed["available"], false);
            assert_eq!(regressed["reason"], "invalid_sample");
            assert!(!progress.complete());
            progress.finish(false, false);
        }
    }
    #[test]
    fn resource_progress_batch_has_one_due_period_and_terminal_precedes_target_finish() {
        let mut progress = Progress::new(true, &config());
        progress.source(&resource_source());
        assert!(progress.resources_due());
        progress.resources_done();
        assert!(!progress.resources_due());
        progress.resource_to(
            ResourceOwner {
                pid: std::process::id(),
                worker: None,
                generation_context: None,
            },
            || panic!("not-due source read"),
            20000,
            &mut Vec::new(),
        );
        progress.observe(&json!({"phase":"terminal"}));
        assert!(progress.resources_due());
        let mut terminal = resource_sample(std::process::id(), 20000);
        terminal["samples"] = json!(2);
        terminal["terminal_sample"] = json!(true);
        let record = project_sample(&mut progress, Ok(Some(terminal)));
        assert_eq!(record["phase"], "terminal");
        assert_eq!(record["terminal_sample"], true);
        progress.resources_done();
        progress.finish(false, false);
        assert!(!progress.resources_due());
        progress.resource_to(
            ResourceOwner {
                pid: std::process::id(),
                worker: None,
                generation_context: None,
            },
            || panic!("post-terminal source read"),
            20000,
            &mut Vec::new(),
        );
    }
    #[test]
    fn resource_progress_cleanup_context_preserves_private_work_phase_and_existing_cadence() {
        let mut progress = Progress::new(true, &config());
        progress.source(&resource_source());
        let journal = json!({"phase":"revocation","connected_clients":10000});
        progress.observe(&journal);
        progress.resources_done();
        let original = journal.clone();
        progress.cleanup_context();
        assert_eq!(journal, original);
        assert_eq!(progress.record("progress").unwrap().phase, "terminal");
        progress.resources_done();
        progress.observe(&journal);
        assert_eq!(progress.record("progress").unwrap().phase, "terminal");
        assert!(!progress.resources_due());
        let next = progress.state.as_ref().unwrap().next;
        progress.resource_tick(next - std::time::Duration::from_nanos(1));
        assert!(!progress.resources_due());
        progress.resource_tick(next);
        assert!(progress.resources_due());
        assert!(progress.complete());
        progress.finish(false, false);
        let mut disabled = Progress::disabled();
        disabled.cleanup_context();
        disabled.resource_tick(next);
        assert!(!disabled.resources_due());
    }
    #[test]
    fn resource_progress_requires_verified_hashes_and_retains_sink_failure() {
        let mut invalid = Progress::new(true, &config());
        let mut source = resource_source();
        source["digest"] = json!("PRIVATE_DIGEST");
        invalid.source(&source);
        invalid.resource(
            ResourceOwner {
                pid: std::process::id(),
                worker: None,
                generation_context: None,
            },
            || panic!("unverified resource source read"),
        );
        assert!(!invalid.complete());
        invalid.finish(false, false);
        struct Fail;
        impl Write for Fail {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("PRIVATE_SINK_ERROR"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut progress = Progress::new(true, &config());
        progress.source(&resource_source());
        assert!(!progress.resource_to(
            ResourceOwner {
                pid: std::process::id(),
                worker: None,
                generation_context: None
            },
            || Ok(Some(resource_sample(std::process::id(), 20000))),
            20000,
            &mut Fail
        ));
        assert!(!progress.complete());
        assert_eq!(
            project_sample(
                &mut progress,
                Ok(Some(resource_sample(std::process::id(), 20000)))
            )["available"],
            true
        );
        assert!(!progress.complete());
        progress.finish(false, false);
    }
    #[test]
    fn resource_progress_max_u64_wire_record_fits_two_kibibytes() {
        let record = ResourceRecord {
            schema: "mount-rs.resource-progress.v1",
            controller_pid: u32::MAX,
            source_revision: "ffffffffffffffffffffffffffffffffffffffff",
            source_digest: "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            binary_sha256: "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            role: "worker",
            pid: u32::MAX - 1,
            worker: Some(9),
            generation_context: Some(u64::MAX),
            phase: "mostly_idle/sequential_overwrite",
            observed_unix_ms: Some(u64::MAX),
            published_unix_ms: u64::MAX,
            samples: Some(u64::MAX),
            sample_interval_ms: 100,
            terminal_sample: Some(false),
            available: true,
            reason: None,
            counter_scope: "sampler_baseline_cumulative_process",
            cpu_user_us: Some(u64::MAX),
            cpu_system_us: Some(u64::MAX),
            rss_current_bytes: Some(u64::MAX),
            rss_lifetime_peak_bytes: Some(u64::MAX),
            rss_peak_bytes: Some(u64::MAX),
            minimum_host_free_bytes: Some(u64::MAX),
            block_inputs: Some(u64::MAX),
            block_outputs: Some(u64::MAX),
            process_disk_read_bytes: None,
            process_disk_write_bytes: None,
            process_disk_bytes_reason: "not_captured_by_sampler",
        };
        let mut bytes = Vec::new();
        write_bounded::<RESOURCE_LIMIT>(&mut bytes, b"\nresource_progress ", &record).unwrap();
        assert!(bytes.len() <= RESOURCE_LIMIT);
        let value: Value =
            serde_json::from_slice(bytes.strip_prefix(b"\nresource_progress ").unwrap()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 29);
        assert_eq!(value["samples"].as_u64(), Some(u64::MAX));
        assert_eq!(value["cpu_user_us"].as_u64(), Some(u64::MAX));
        if let Some(path) = std::env::var_os("MOUNT_RS_RESOURCE_PROGRESS_TEST_FIXTURE") {
            std::fs::write(path, &bytes).unwrap();
        }
    }
    #[test]
    fn partial_progress_is_public_bounded_and_never_shrinks_the_full_target() {
        let mut progress = Progress::new(true, &config());
        progress.observe(&json!({"phase":"signed_connections", "connected_clients":17, "initialization":{"initialized_drives":10000}, "private_token":"do not emit"}));
        let mut bytes = Vec::new();
        progress.emit_to("progress", &mut bytes);
        assert!(bytes.starts_with(b"\ntarget_progress "));
        let json: Value =
            serde_json::from_slice(bytes.strip_prefix(b"\ntarget_progress ").unwrap()).unwrap();
        assert_eq!(json["connected_clients"], 17);
        assert_eq!(json["initialized_drives"], 10000);
        assert_eq!(
            (
                json["servers"].as_u64(),
                json["clients"].as_u64(),
                json["partitions"].as_u64(),
                json["files_per_drive"].as_u64(),
                json["phase_seconds"].as_u64()
            ),
            (Some(10), Some(10000), Some(5000), Some(1000), Some(30))
        );
        assert_eq!(json.as_object().unwrap().len(), 20);
        assert!(!String::from_utf8(bytes).unwrap().contains("private_token"));
        progress.finish(false, false);
    }
    #[test]
    fn source_claim_requires_clean_exact_revision_and_terminal_keeps_last_partial_counts() {
        let mut dirty = Progress::new(true, &config());
        dirty.source(&json!({"revision":"a".repeat(40), "checkout_status":" M secret"}));
        assert!(dirty.record("progress").unwrap().source_revision.is_none());
        assert!(!dirty.complete());
        dirty.finish(false, false);
        let mut clean = Progress::new(true, &config());
        clean.source(&json!({"revision":"a".repeat(40), "checkout_status":""}));
        assert_eq!(
            clean.record("progress").unwrap().source_revision,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert!(clean.observe(&json!({"phase":"signed_connections", "connected_clients":7, "initialization":{"initialized_drives":10000}})));
        assert!(!clean.observe(&json!({"phase":"signed_connections", "connected_clients":8, "initialization":{"initialized_drives":10000}})));
        clean.finish(false, false);
        let terminal = clean.record("terminal").unwrap();
        assert_eq!(
            (terminal.connected_clients, terminal.initialized_drives),
            (8, 10000)
        );
        assert_eq!(
            (
                terminal.phase,
                terminal.outcome,
                terminal.accounting_complete
            ),
            ("terminal", "error", false)
        );
    }
    #[test]
    fn disabled_progress_does_not_write_and_failed_sink_is_sticky() {
        struct Fail;
        impl Write for Fail {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("fail"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut disabled = Progress::new(false, &config());
        disabled.emit_to("progress", &mut Fail);
        assert!(disabled.record("progress").is_none());
        assert!(disabled.complete());
        let mut enabled = Progress::new(true, &config());
        enabled.emit_to("progress", &mut Fail);
        assert!(!enabled.complete());
        enabled.finish(false, false);
    }
    #[test]
    fn lazy_target_balanced_validation_phases_preserve_closed_progress_accounting() {
        let mut progress = Progress::new(true, &config());
        progress.source(&resource_source());
        assert!(progress.complete());
        for name in ["assigned_warmup", "crossnode_routes"] {
            progress.observe(&json!({"phase":name,"connected_clients":10000,
                "initialization":{"initialized_drives":10000}}));
            assert!(
                progress.complete(),
                "balanced validation phase must retain complete public accounting: {name}"
            );
            let mut bytes = Vec::new();
            progress.emit_to("phase", &mut bytes);
            assert!(bytes.len() <= LIMIT);
            assert!(bytes.ends_with(b"\n"));
            let record: Value =
                serde_json::from_slice(bytes.strip_prefix(b"\ntarget_progress ").unwrap()).unwrap();
            assert_eq!(record["phase"], name);
            assert_eq!(record["accounting_complete"], true);
            assert_eq!(record["source_verified"], true);
            assert_eq!(record["clients"], 10000);
            assert_eq!(record["connected_clients"], 10000);
            assert_eq!(record["initialized_drives"], 10000);
            assert_eq!(
                (
                    record["servers"].as_u64(),
                    record["drives"].as_u64(),
                    record["partitions"].as_u64(),
                    record["files_per_drive"].as_u64(),
                    record["phase_seconds"].as_u64()
                ),
                (Some(10), Some(10000), Some(5000), Some(1000), Some(30))
            );
            assert_eq!(record.as_object().unwrap().len(), 20);
        }
        let mut unknown = Progress::new(true, &config());
        unknown.source(&resource_source());
        unknown.observe(&json!({"phase":"crossnode_routes/private"}));
        assert!(
            !unknown.complete(),
            "closed phase parser must still reject unknown phases"
        );
        unknown.observe(
            &json!({"phase":"routes_and_scope","connected_clients":10000,
            "initialization":{"initialized_drives":10000}}),
        );
        assert!(
            !unknown.complete(),
            "unknown phase accounting failure must stay sticky after a recognized phase"
        );
        let mut bytes = Vec::new();
        unknown.emit_to("phase", &mut bytes);
        let record: Value =
            serde_json::from_slice(bytes.strip_prefix(b"\ntarget_progress ").unwrap()).unwrap();
        assert_eq!(record["accounting_complete"], false);
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("crossnode_routes/private")
        );
    }

    #[test]
    fn phase_projection_is_closed_and_all_actual_patterns_are_supported() {
        assert!(phase("private/example").is_none());
        for pattern in PATTERNS {
            for mode in ["mostly_idle", "all_active"] {
                assert!(phase(&format!("{mode}/{pattern}")).is_some());
            }
        }
    }
}
