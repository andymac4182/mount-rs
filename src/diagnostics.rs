//! Opt-in request phase diagnostics for local integration investigations.
//!
//! Set `MOUNT_RS_TRACE_REQUESTS=1` before starting the process. Diagnostics go
//! to stderr and may contain filesystem paths supplied by the caller. They are
//! disabled by default and do not install a subscriber or change request limits.
//! `MOUNT_RS_TRACE_FAILURES=1` independently enables bounded exceptional
//! records at instrumented error boundaries, without request phase tracing.

#[doc(hidden)]
pub mod profile;
#[doc(hidden)]
pub mod storage;

use std::fmt;
use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::FsError;

static ENABLED: OnceLock<bool> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static FAILURES_ENABLED: OnceLock<bool> = OnceLock::new();
static FAILURE_COUNT: AtomicU64 = AtomicU64::new(0);
const MAX_FAILURE_RECORDS: u64 = 16;
const MAX_FAILURE_DETAILS_BYTES: usize = 2048;

/// Emit at most sixteen bounded exceptional records per process when
/// `MOUNT_RS_TRACE_FAILURES=1`. This does not enable request phase tracing.
#[doc(hidden)]
pub fn trace_failure(component: &'static str, operation: &'static str, error: &FsError) {
    if !*FAILURES_ENABLED
        .get_or_init(|| std::env::var_os("MOUNT_RS_TRACE_FAILURES").is_some_and(|v| v == "1"))
        || !reserve_failure(&FAILURE_COUNT)
    {
        return;
    }
    write_failure(&mut io::stderr().lock(), component, operation, error);
}

fn reserve_failure(count: &AtomicU64) -> bool {
    count
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            (value < MAX_FAILURE_RECORDS).then_some(value + 1)
        })
        .is_ok()
}

fn write_failure(output: &mut impl Write, component: &str, operation: &str, error: &FsError) {
    let mut details = BoundedDetails(String::new());
    // Debug retains code, syscall and the underlying message together.
    let truncated = fmt::write(&mut details, format_args!("{error:?}")).is_err();
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    // Diagnostic output must never replace a filesystem error or panic.
    let _ = writeln!(
        output,
        "MOUNT_RS_FAILURE_TRACE ts_ms={ts_ms} pid={} component={component} operation={operation} code={} truncated={truncated} details={}",
        std::process::id(),
        error.code.as_str(),
        details.0,
    );
}

struct BoundedDetails(String);

impl fmt::Write for BoundedDetails {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for character in value.chars() {
            // Escape controls even for fields whose Debug formatter might
            // write raw text. Keep one record on one physical line.
            if character.is_control() {
                for escaped in character.escape_default() {
                    self.push(escaped)?;
                }
            } else {
                self.push(character)?;
            }
        }
        Ok(())
    }
}

impl BoundedDetails {
    fn push(&mut self, character: char) -> fmt::Result {
        if self.0.len() + character.len_utf8() > MAX_FAILURE_DETAILS_BYTES {
            return Err(fmt::Error);
        }
        self.0.push(character);
        Ok(())
    }
}

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

#[cfg(test)]
mod failure_tests {
    use super::*;
    use crate::ErrorCode;

    #[test]
    fn concurrent_exceptional_records_stop_at_the_process_budget() {
        let count = AtomicU64::new(0);
        let accepted = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..8 {
                        if reserve_failure(&count) {
                            accepted.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(accepted.load(Ordering::Relaxed), MAX_FAILURE_RECORDS);
        assert!(!reserve_failure(&count));
    }

    #[test]
    fn exceptional_record_keeps_backend_code_and_context() {
        let error = FsError::new(ErrorCode::Eio)
            .with_syscall("metadata-load")
            .with_message("FoundationDB error 1031: transaction timed out");
        let mut output = Vec::new();
        write_failure(&mut output, "nfs", "write_path", &error);
        let record = String::from_utf8(output).unwrap();
        assert!(record.contains("code=EIO truncated=false"));
        assert!(record.contains("metadata-load"));
        assert!(record.contains("1031"));
        assert_eq!(record.lines().count(), 1);
    }

    #[test]
    fn exceptional_record_bounds_unicode_and_escapes_controls() {
        let error = FsError::backend(format!("first\n\r\u{1b}{}", "界".repeat(4096)));
        let mut output = Vec::new();
        write_failure(&mut output, "nfs", "write", &error);
        let record = String::from_utf8(output).unwrap();
        assert!(record.contains("truncated=true"));
        assert!(record.len() < MAX_FAILURE_DETAILS_BYTES + 256);
        assert_eq!(record.lines().count(), 1);
        assert!(!record.contains('\u{1b}'));
    }

    #[test]
    fn broken_exceptional_output_is_best_effort() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        write_failure(
            &mut Broken,
            "nfs",
            "commit_path",
            &FsError::backend("original"),
        );
    }
}
