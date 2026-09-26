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
}
/// None is a zero-clock, zero-timer, zero-allocation disabled path.
pub struct Progress {
    state: Option<State>,
}
impl Progress {
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
        let Some(current) = journal["phase"].as_str().and_then(phase) else {
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
    const FIXED: [&str; 13] = [
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
    fn phase_projection_is_closed_and_all_actual_patterns_are_supported() {
        assert!(phase("private/example").is_none());
        for pattern in PATTERNS {
            for mode in ["mostly_idle", "all_active"] {
                assert!(phase(&format!("{mode}/{pattern}")).is_some());
            }
        }
    }
}
