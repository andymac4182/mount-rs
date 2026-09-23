//! Opt-in request phase diagnostics for local integration investigations.
//!
//! Set `MOUNT_RS_TRACE_REQUESTS=1` before starting the process. Diagnostics go
//! to stderr and may contain filesystem paths supplied by the caller. They are
//! disabled by default and do not install a subscriber or change request limits.

use std::fmt;
use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static ENABLED: OnceLock<bool> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One diagnostic operation, identified independently of concurrent requests.
#[doc(hidden)]
pub struct RequestTrace {
    component: &'static str,
    operation: &'static str,
    enabled: Option<(u64, Instant)>,
    last_stage: &'static str,
    finished: bool,
}

impl RequestTrace {
    pub fn is_enabled(&self) -> bool {
        self.enabled.is_some()
    }

    pub fn new(component: &'static str, operation: &'static str) -> Self {
        let enabled = *ENABLED
            .get_or_init(|| std::env::var_os("MOUNT_RS_TRACE_REQUESTS").is_some_and(|v| v == "1"));
        let mut trace = Self {
            component,
            operation,
            enabled: enabled.then(|| (NEXT_ID.fetch_add(1, Ordering::Relaxed), Instant::now())),
            last_stage: "entry",
            finished: false,
        };
        trace.stage("entry", format_args!(""));
        trace
    }

    pub fn stage(&mut self, stage: &'static str, details: fmt::Arguments<'_>) {
        self.last_stage = stage;
        self.emit(stage, details);
    }

    pub fn finish(&mut self, details: fmt::Arguments<'_>) {
        self.emit("exit", details);
        self.finished = true;
    }

    fn emit(&self, stage: &str, details: fmt::Arguments<'_>) {
        let Some((id, started)) = self.enabled else {
            return;
        };
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        // Broken diagnostic output must not terminate a filesystem operation.
        let _ = writeln!(
            io::stderr().lock(),
            "MOUNT_RS_REQUEST_TRACE ts_ms={ts_ms} pid={} id={id} component={} operation={} stage={stage} elapsed_us={} {details}",
            std::process::id(),
            self.component,
            self.operation,
            started.elapsed().as_micros(),
        );
    }
}

impl Drop for RequestTrace {
    fn drop(&mut self) {
        if !self.finished {
            self.emit("dropped", format_args!("last_stage={}", self.last_stage));
        }
    }
}
