//! Fixed-label, opt-in storage operation diagnostics. Durations are inclusive
//! wall time; concurrent and nested spans cannot be summed as CPU time.

use serde::Serialize;
use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const HISTOGRAM_BUCKETS: usize = 32;
const SLOW_THRESHOLD_NS: u64 = 100_000_000;
const MAX_SLOW_RECORDS: u64 = 16;
static ENABLED: OnceLock<bool> = OnceLock::new();
static TRACE_ENABLED: OnceLock<bool> = OnceLock::new();
static GLOBAL: OnceLock<Recorder> = OnceLock::new();
static SLOW_RECORDS: AtomicU64 = AtomicU64::new(0);

/// Labels are a closed enum; no caller-provided strings enter the recorder.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Operation {
    MetadataLoad,
    MetadataConditionalLoad,
    MetadataSnapshot,
    MetadataPublish,
    MetadataFlush,
    BlockPut,
    BlockGet,
    BlockFlush,
    BlockVerifyBacking,
    BlockPrepareBacking,
    BlockDelete,
    BlockReconcile,
    PgliteClientWait,
}
const NAMES: [&str; 13] = [
    "metadata.load",
    "metadata.load_if_changed",
    "metadata.snapshot",
    "metadata.publish",
    "metadata.flush",
    "blocks.put",
    "blocks.get",
    "blocks.flush",
    "blocks.verify_backing",
    "blocks.prepare_backing",
    "blocks.delete",
    "blocks.reconcile",
    "pglite.client_lock_wait",
];

#[derive(Clone, Copy)]
pub enum Outcome {
    Success,
    Error,
    Cancelled,
}
impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

struct Metric {
    calls: AtomicU64,
    success: AtomicU64,
    error: AtomicU64,
    cancelled: AtomicU64,
    bytes: AtomicU64,
    elapsed_ns: AtomicU64,
    latency_log2_us: [AtomicU64; HISTOGRAM_BUCKETS],
}
impl Metric {
    fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            success: AtomicU64::new(0),
            error: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            elapsed_ns: AtomicU64::new(0),
            latency_log2_us: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}
struct Recorder {
    metrics: [Metric; NAMES.len()],
    in_flight: AtomicU64,
    forwarding_boxes: AtomicU64,
    forwarding_requested_bytes: AtomicU64,
}
impl Recorder {
    fn new() -> Self {
        Self {
            metrics: std::array::from_fn(|_| Metric::new()),
            in_flight: AtomicU64::new(0),
            forwarding_boxes: AtomicU64::new(0),
            forwarding_requested_bytes: AtomicU64::new(0),
        }
    }
    fn record_forwarding_box(&self, requested_object_bytes: u64) {
        self.forwarding_boxes.fetch_add(1, Ordering::Relaxed);
        self.forwarding_requested_bytes
            .fetch_add(requested_object_bytes, Ordering::Relaxed);
    }
    fn record(&self, operation: Operation, outcome: Outcome, bytes: u64, elapsed_ns: u64) {
        let metric = &self.metrics[operation as usize];
        metric.calls.fetch_add(1, Ordering::Relaxed);
        match outcome {
            Outcome::Success => &metric.success,
            Outcome::Error => &metric.error,
            Outcome::Cancelled => &metric.cancelled,
        }
        .fetch_add(1, Ordering::Relaxed);
        metric.bytes.fetch_add(bytes, Ordering::Relaxed);
        metric.elapsed_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        let micros = elapsed_ns / 1_000;
        let bucket = if micros == 0 {
            0
        } else {
            (u64::BITS - micros.leading_zeros()) as usize
        }
        .min(HISTOGRAM_BUCKETS - 1);
        metric.latency_log2_us[bucket].fetch_add(1, Ordering::Relaxed);
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            in_flight: self.in_flight.load(Ordering::Relaxed),
            forwarding_boxes: ForwardingBoxes {
                sites: "napi_dynamic_provider_forwarding_future",
                calls: self.forwarding_boxes.load(Ordering::Relaxed),
                requested_object_bytes: self.forwarding_requested_bytes.load(Ordering::Relaxed),
            },
            entries: self
                .metrics
                .iter()
                .enumerate()
                .map(|(index, metric)| Entry {
                    name: NAMES[index],
                    calls: metric.calls.load(Ordering::Relaxed),
                    success: metric.success.load(Ordering::Relaxed),
                    error: metric.error.load(Ordering::Relaxed),
                    cancelled: metric.cancelled.load(Ordering::Relaxed),
                    bytes: metric.bytes.load(Ordering::Relaxed),
                    elapsed_ns: metric.elapsed_ns.load(Ordering::Relaxed),
                    latency_log2_us: std::array::from_fn(|bucket| {
                        metric.latency_log2_us[bucket].load(Ordering::Relaxed)
                    }),
                })
                .collect(),
        }
    }
}

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("MOUNT_RS_PROFILE_IO").is_some_and(|v| v == "1"))
}
fn trace_enabled() -> bool {
    *TRACE_ENABLED
        .get_or_init(|| std::env::var_os("MOUNT_RS_TRACE_STORAGE").is_some_and(|v| v == "1"))
}
pub fn record_forwarding_box(requested_object_bytes: u64) {
    // Called only by the enabled N-API Box::pin site. This update allocates
    // nothing; it counts requested future object bytes, not allocator overhead.
    GLOBAL
        .get_or_init(Recorder::new)
        .record_forwarding_box(requested_object_bytes);
}
pub fn snapshot() -> Snapshot {
    GLOBAL.get_or_init(Recorder::new).snapshot()
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub name: &'static str,
    pub calls: u64,
    pub success: u64,
    pub error: u64,
    pub cancelled: u64,
    /// Successful payload bytes at the logical provider boundary only.
    pub bytes: u64,
    pub elapsed_ns: u64,
    /// Bucket 0 is below 1 us; bucket n covers [2^(n-1), 2^n) us.
    /// The last bucket also includes all higher latencies.
    pub latency_log2_us: [u64; HISTOGRAM_BUCKETS],
}
#[derive(Clone, Debug, Serialize)]
pub struct ForwardingBoxes {
    pub sites: &'static str,
    pub calls: u64,
    pub requested_object_bytes: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub in_flight: u64,
    pub forwarding_boxes: ForwardingBoxes,
    pub entries: Vec<Entry>,
}
impl Snapshot {
    /// Compare quiescent snapshots; concurrent reads are not atomic.
    pub fn delta(&self, before: &Self) -> Result<Self, &'static str> {
        if self.entries.len() != before.entries.len() {
            return Err("storage shape changed");
        }
        let mut entries = Vec::with_capacity(self.entries.len());
        for (now, old) in self.entries.iter().zip(&before.entries) {
            if now.name != old.name {
                return Err("storage shape changed");
            }
            let subtract = |a: u64, b: u64| a.checked_sub(b).ok_or("storage counter reset");
            let mut latency_log2_us = [0; HISTOGRAM_BUCKETS];
            for (bucket, value) in latency_log2_us.iter_mut().enumerate() {
                *value = subtract(now.latency_log2_us[bucket], old.latency_log2_us[bucket])?;
            }
            entries.push(Entry {
                name: now.name,
                calls: subtract(now.calls, old.calls)?,
                success: subtract(now.success, old.success)?,
                error: subtract(now.error, old.error)?,
                cancelled: subtract(now.cancelled, old.cancelled)?,
                bytes: subtract(now.bytes, old.bytes)?,
                elapsed_ns: subtract(now.elapsed_ns, old.elapsed_ns)?,
                latency_log2_us,
            });
        }
        if self.forwarding_boxes.sites != before.forwarding_boxes.sites {
            return Err("forwarding allocation site changed");
        }
        Ok(Self {
            in_flight: self.in_flight,
            forwarding_boxes: ForwardingBoxes {
                sites: self.forwarding_boxes.sites,
                calls: self
                    .forwarding_boxes
                    .calls
                    .checked_sub(before.forwarding_boxes.calls)
                    .ok_or("storage counter reset")?,
                requested_object_bytes: self
                    .forwarding_boxes
                    .requested_object_bytes
                    .checked_sub(before.forwarding_boxes.requested_object_bytes)
                    .ok_or("storage counter reset")?,
            },
            entries,
        })
    }
}

/// Dropping an unfinished span records cancellation, including a dropped
/// future. The disabled path stores no timer and does not allocate.
pub struct Span<'a> {
    recorder: Option<&'a Recorder>,
    operation: Operation,
    started: Option<Instant>,
    finished: bool,
}
impl Span<'static> {
    pub fn new(operation: Operation) -> Self {
        let recorder = enabled().then(|| GLOBAL.get_or_init(Recorder::new));
        if let Some(recorder) = recorder {
            recorder.in_flight.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            recorder,
            operation,
            started: recorder.map(|_| Instant::now()),
            finished: false,
        }
    }
}
impl<'a> Span<'a> {
    #[cfg(test)]
    fn with_recorder(recorder: &'a Recorder, operation: Operation) -> Self {
        recorder.in_flight.fetch_add(1, Ordering::Relaxed);
        Self {
            recorder: Some(recorder),
            operation,
            started: Some(Instant::now()),
            finished: false,
        }
    }
    fn finish(&mut self, outcome: Outcome, bytes: u64) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let (Some(recorder), Some(started)) = (self.recorder, self.started) {
            let elapsed_ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            recorder.record(self.operation, outcome, bytes, elapsed_ns);
            recorder.in_flight.fetch_sub(1, Ordering::Relaxed);
            if elapsed_ns >= SLOW_THRESHOLD_NS && trace_enabled() {
                let _ = write_slow_record(
                    &mut io::stderr().lock(),
                    self.operation,
                    outcome,
                    elapsed_ns,
                );
            }
        }
    }
    pub fn finish_success(&mut self, bytes: u64) {
        self.finish(Outcome::Success, bytes);
    }
    pub fn finish_error(&mut self) {
        self.finish(Outcome::Error, 0);
    }
}
impl Drop for Span<'_> {
    fn drop(&mut self) {
        self.finish(Outcome::Cancelled, 0);
    }
}

fn write_slow_record(
    output: &mut impl Write,
    operation: Operation,
    outcome: Outcome,
    elapsed_ns: u64,
) -> io::Result<()> {
    if SLOW_RECORDS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            (value < MAX_SLOW_RECORDS).then_some(value + 1)
        })
        .is_ok()
    {
        writeln!(
            output,
            "MOUNT_RS_STORAGE_SLOW operation={} outcome={} elapsed_us={}",
            NAMES[operation as usize],
            outcome.name(),
            elapsed_ns / 1_000
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_bytes_outcomes_and_log2_latency_without_dynamic_labels() {
        let recorder = Recorder::new();
        let before = recorder.snapshot();
        let mut success = Span::with_recorder(&recorder, Operation::BlockPut);
        success.finish_success(4096);
        let mut error = Span::with_recorder(&recorder, Operation::MetadataPublish);
        error.finish_error();
        drop(Span::with_recorder(&recorder, Operation::BlockGet));
        let delta = recorder.snapshot().delta(&before).unwrap();
        assert_eq!(delta.entries[Operation::BlockPut as usize].success, 1);
        assert_eq!(delta.entries[Operation::BlockPut as usize].bytes, 4096);
        assert_eq!(delta.entries[Operation::MetadataPublish as usize].error, 1);
        assert_eq!(delta.entries[Operation::BlockGet as usize].cancelled, 1);
        assert_eq!(
            delta.entries[Operation::BlockPut as usize]
                .latency_log2_us
                .iter()
                .sum::<u64>(),
            1
        );
    }

    #[test]
    fn delta_rejects_counter_resets_and_changed_names() {
        let recorder = Recorder::new();
        let before = recorder.snapshot();
        recorder.record(Operation::BlockPut, Outcome::Success, 4, 1_000);
        let after = recorder.snapshot();
        assert!(before.delta(&after).is_err());
        let mut renamed = after.clone();
        renamed.entries[0].name = "other";
        assert!(renamed.delta(&before).is_err());
    }

    #[test]
    fn pool_wait_has_distinct_fixed_latency_label() {
        let recorder = Recorder::new();
        recorder.record(Operation::PgliteClientWait, Outcome::Success, 0, 2000);
        let snapshot = recorder.snapshot();
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.name == "pglite.client_lock_wait")
            .unwrap();
        assert_eq!(entry.calls, 1);
        assert_eq!(entry.latency_log2_us.iter().sum::<u64>(), 1);
    }

    #[test]
    fn in_flight_gauge_exposes_nonquiescent_snapshot() {
        let recorder = Recorder::new();
        let span = Span::with_recorder(&recorder, Operation::BlockGet);
        assert_eq!(recorder.snapshot().in_flight, 1);
        drop(span);
        assert_eq!(recorder.snapshot().in_flight, 0);
    }

    #[test]
    fn slow_log_is_bounded_and_contains_only_fixed_fields() {
        let mut output = Vec::new();
        for _ in 0..32 {
            let _ = write_slow_record(
                &mut output,
                Operation::BlockPut,
                Outcome::Error,
                999_000_000,
            );
        }
        let log = String::from_utf8(output).unwrap();
        assert_eq!(log.lines().count(), MAX_SLOW_RECORDS as usize);
        assert!(
            log.lines()
                .all(|line| line.contains("operation=blocks.put outcome=error"))
        );
    }
}
