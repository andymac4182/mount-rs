//! Opt-in local runtime timings, kept separate from the server stage catalog.
//! Inclusive spans overlap. Serial atomic snapshots are not a drain proof.
//! Deferred mutex timings publish only after the caller releases its state lock.

#[cfg(any(feature = "io-profiling", test))]
use std::time::Duration;

use serde::Serialize;

#[cfg(feature = "io-profiling")]
use std::fmt::{self, Write as _};
#[cfg(feature = "io-profiling")]
use std::io::{self, Write};
#[cfg(feature = "io-profiling")]
use std::sync::Arc;
#[cfg(feature = "io-profiling")]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(feature = "io-profiling")]
use std::time::Instant;

const BUCKETS: usize = 32;
const NAMES: [&str; 8] = [
    "runtime.acquire",
    "runtime.activation_wait",
    "runtime.open",
    "runtime.eviction_shutdown",
    "runtime.terminal_drain",
    "runtime.handle_close",
    "runtime.state_mutex_wait",
    "runtime.state_mutex_hold",
];
#[cfg(feature = "io-profiling")]
const SLOW_NS: u64 = 100_000_000;
#[cfg(feature = "io-profiling")]
const SLOW_RECORDS: u64 = 16;
#[cfg(feature = "io-profiling")]
const RECORD_BYTES: usize = 160;

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum RuntimeStage {
    Acquire,
    ActivationWait,
    Open,
    EvictionShutdown,
    TerminalDrain,
    HandleClose,
    StateMutexWait,
    StateMutexHold,
}

#[derive(Clone, Copy)]
pub(crate) enum RuntimeOutcome {
    Success,
    Error,
    Cancelled,
}
impl RuntimeOutcome {
    pub(crate) fn result<T, E>(result: &std::result::Result<T, E>) -> Self {
        if result.is_ok() {
            Self::Success
        } else {
            Self::Error
        }
    }
    #[cfg(feature = "io-profiling")]
    fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Finite observer created before serving requests. Default means unavailable;
/// enabling the feature alone does not construct a recorder or sample a clock.
#[derive(Clone, Default)]
pub struct RuntimeDiagnostics {
    #[cfg(feature = "io-profiling")]
    recorder: Option<Arc<Recorder>>,
}
impl RuntimeDiagnostics {
    pub fn new(trace: bool) -> Self {
        #[cfg(feature = "io-profiling")]
        {
            Self {
                recorder: Some(Arc::new(Recorder::new(trace))),
            }
        }
        #[cfg(not(feature = "io-profiling"))]
        {
            let _ = trace;
            Self::default()
        }
    }
    pub fn enabled(&self) -> bool {
        #[cfg(feature = "io-profiling")]
        {
            self.recorder.is_some()
        }
        #[cfg(not(feature = "io-profiling"))]
        {
            false
        }
    }
    pub fn snapshot(&self) -> Option<RuntimeDiagnosticsSnapshot> {
        #[cfg(feature = "io-profiling")]
        {
            let recorder = self.recorder.as_ref()?;
            let before = recorder.sequence.load(Ordering::SeqCst);
            let writers_before = recorder.writers.load(Ordering::SeqCst);
            let entries = std::array::from_fn(|index| recorder.rows[index].snapshot(NAMES[index]));
            let writers_after = recorder.writers.load(Ordering::SeqCst);
            let after = recorder.sequence.load(Ordering::SeqCst);
            let counter_saturated = recorder.saturated.load(Ordering::SeqCst);
            Some(RuntimeDiagnosticsSnapshot {
                scope: "process_local_preconstructed_runtime_observer",
                sampling_scope: "serial_atomic_loads_not_transactional_or_drain_proof",
                histogram_scope: "inclusive_wall_time_log2_microseconds_32_buckets",
                deferred_scope: "deferred_spans_enter_only_on_post_unlock_publication",
                inclusive_spans_overlap: true,
                concurrent_activity: counter_saturated
                    || writers_before != 0
                    || writers_after != 0
                    || before != after,
                counter_saturated,
                slow_record_attempts: recorder.slow_records.load(Ordering::SeqCst),
                entries,
            })
        }
        #[cfg(not(feature = "io-profiling"))]
        {
            None
        }
    }

    /// Publish a previously measured interval after releasing the pool lock.
    /// No clock, task, future box or new observer is constructed here.
    #[cfg(test)]
    pub(crate) fn record_elapsed(
        &self,
        stage: RuntimeStage,
        outcome: RuntimeOutcome,
        elapsed: Duration,
    ) {
        #[cfg(feature = "io-profiling")]
        if let Some(recorder) = &self.recorder {
            publish(recorder, stage, outcome, elapsed, false);
        }
        #[cfg(not(feature = "io-profiling"))]
        let _ = (stage, outcome, elapsed);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDiagnosticsSnapshot {
    pub scope: &'static str,
    pub sampling_scope: &'static str,
    pub histogram_scope: &'static str,
    pub deferred_scope: &'static str,
    pub inclusive_spans_overlap: bool,
    /// Observed row updates or saturated counters make capture uncertain;
    /// absence does not prove drain, nor an atomic sample of rows, deferred
    /// timings and trace writer attempts.
    pub concurrent_activity: bool,
    pub counter_saturated: bool,
    pub slow_record_attempts: u64,
    pub entries: [RuntimeDiagnosticsEntry; NAMES.len()],
}
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDiagnosticsEntry {
    pub name: &'static str,
    pub calls: u64,
    pub success: u64,
    pub error: u64,
    pub cancelled: u64,
    /// Includes ordinary spans detached but not yet published. Deferred spans
    /// are absent until publication; this gauge cannot prove runtime drain.
    pub in_flight: u64,
    pub elapsed_ns: u64,
    pub max_elapsed_ns: u64,
    pub latency_log2_us: [u64; BUCKETS],
}

/// Stack-owned timing. Explicit finishes are idempotent; unfinished Drop records
/// cancellation. Declare ordinary spans before state guards so guards drop first,
/// or use new_deferred/detach and publish the completed record after unlocking.
#[derive(Default)]
pub(crate) struct RuntimeSpan {
    #[cfg(feature = "io-profiling")]
    active: Option<ActiveSpan>,
}
#[cfg(feature = "io-profiling")]
struct ActiveSpan {
    recorder: Arc<Recorder>,
    stage: RuntimeStage,
    started: Instant,
    entered: bool,
}
impl RuntimeSpan {
    pub(crate) fn new(observer: Option<&RuntimeDiagnostics>, stage: RuntimeStage) -> Self {
        Self::create(observer, stage, true)
    }
    /// Records nothing at construction, so it can start while a mutex is held.
    /// The caller must detach and publish, or finish, after releasing that mutex.
    pub(crate) fn new_deferred(observer: Option<&RuntimeDiagnostics>, stage: RuntimeStage) -> Self {
        Self::create(observer, stage, false)
    }
    fn create(observer: Option<&RuntimeDiagnostics>, stage: RuntimeStage, entered: bool) -> Self {
        #[cfg(feature = "io-profiling")]
        if let Some(recorder) = observer.and_then(|observer| observer.recorder.as_ref()) {
            if entered {
                let _activity = Activity::new(recorder);
                let row = &recorder.rows[stage as usize];
                recorder.add(&row.calls, 1);
                recorder.add(&row.in_flight, 1);
            }
            return Self {
                active: Some(ActiveSpan {
                    recorder: recorder.clone(),
                    stage,
                    started: Instant::now(),
                    entered,
                }),
            };
        }
        #[cfg(not(feature = "io-profiling"))]
        let _ = (observer, stage, entered);
        Self::default()
    }
    pub(crate) fn finish(&mut self, outcome: RuntimeOutcome) {
        self.detach(outcome).publish();
    }
    /// The supplied duration permits timing publication after a state guard is
    /// gone. It also gives tests a deterministic measurement without sleeping.
    #[cfg(test)]
    pub(crate) fn finish_elapsed(&mut self, outcome: RuntimeOutcome, elapsed: Duration) {
        self.detach_elapsed(outcome, elapsed).publish();
    }
    pub(crate) fn detach(&mut self, outcome: RuntimeOutcome) -> RuntimeCompleted {
        #[cfg(feature = "io-profiling")]
        {
            let Some(active) = self.active.take() else {
                return RuntimeCompleted::default();
            };
            let elapsed = active.started.elapsed();
            RuntimeCompleted {
                active: Some(CompletedSpan {
                    recorder: active.recorder,
                    stage: active.stage,
                    entered: active.entered,
                    outcome,
                    elapsed,
                }),
            }
        }
        #[cfg(not(feature = "io-profiling"))]
        {
            let _ = outcome;
            RuntimeCompleted::default()
        }
    }
    #[cfg(test)]
    fn detach_elapsed(&mut self, outcome: RuntimeOutcome, elapsed: Duration) -> RuntimeCompleted {
        #[cfg(feature = "io-profiling")]
        if let Some(active) = self.active.take() {
            return RuntimeCompleted {
                active: Some(CompletedSpan {
                    recorder: active.recorder,
                    stage: active.stage,
                    entered: active.entered,
                    outcome,
                    elapsed,
                }),
            };
        }
        #[cfg(not(feature = "io-profiling"))]
        let _ = (outcome, elapsed);
        RuntimeCompleted::default()
    }
}
impl Drop for RuntimeSpan {
    fn drop(&mut self) {
        self.finish(RuntimeOutcome::Cancelled);
    }
}

/// Detached completion contains only existing observer ownership and numeric
/// fields. Publish or drop it after releasing the lock; both paths are exact once.
#[derive(Default)]
pub(crate) struct RuntimeCompleted {
    #[cfg(feature = "io-profiling")]
    active: Option<CompletedSpan>,
}
#[cfg(feature = "io-profiling")]
struct CompletedSpan {
    recorder: Arc<Recorder>,
    stage: RuntimeStage,
    entered: bool,
    outcome: RuntimeOutcome,
    elapsed: Duration,
}
impl RuntimeCompleted {
    pub(crate) fn publish(&mut self) {
        #[cfg(feature = "io-profiling")]
        if let Some(active) = self.active.take() {
            publish(
                &active.recorder,
                active.stage,
                active.outcome,
                active.elapsed,
                active.entered,
            );
        }
    }
}
impl Drop for RuntimeCompleted {
    fn drop(&mut self) {
        self.publish();
    }
}

#[cfg(feature = "io-profiling")]
struct Row {
    calls: AtomicU64,
    success: AtomicU64,
    error: AtomicU64,
    cancelled: AtomicU64,
    in_flight: AtomicU64,
    elapsed_ns: AtomicU64,
    max_elapsed_ns: AtomicU64,
    histogram: [AtomicU64; BUCKETS],
}
#[cfg(feature = "io-profiling")]
impl Row {
    fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            success: AtomicU64::new(0),
            error: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            elapsed_ns: AtomicU64::new(0),
            max_elapsed_ns: AtomicU64::new(0),
            histogram: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
    fn snapshot(&self, name: &'static str) -> RuntimeDiagnosticsEntry {
        RuntimeDiagnosticsEntry {
            name,
            calls: self.calls.load(Ordering::SeqCst),
            success: self.success.load(Ordering::SeqCst),
            error: self.error.load(Ordering::SeqCst),
            cancelled: self.cancelled.load(Ordering::SeqCst),
            in_flight: self.in_flight.load(Ordering::SeqCst),
            elapsed_ns: self.elapsed_ns.load(Ordering::SeqCst),
            max_elapsed_ns: self.max_elapsed_ns.load(Ordering::SeqCst),
            latency_log2_us: std::array::from_fn(|index| {
                self.histogram[index].load(Ordering::SeqCst)
            }),
        }
    }
}
#[cfg(feature = "io-profiling")]
struct Recorder {
    rows: [Row; NAMES.len()],
    saturated: AtomicBool,
    writers: AtomicU64,
    sequence: AtomicU64,
    trace: bool,
    slow_records: AtomicU64,
}
#[cfg(feature = "io-profiling")]
impl Recorder {
    fn new(trace: bool) -> Self {
        Self {
            rows: std::array::from_fn(|_| Row::new()),
            saturated: AtomicBool::new(false),
            writers: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            trace,
            slow_records: AtomicU64::new(0),
        }
    }
    fn add(&self, counter: &AtomicU64, value: u64) {
        let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
            Some(old.checked_add(value).unwrap_or_else(|| {
                self.saturated.store(true, Ordering::SeqCst);
                u64::MAX
            }))
        });
    }
    fn subtract(&self, counter: &AtomicU64) {
        let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
            Some(old.checked_sub(1).unwrap_or_else(|| {
                self.saturated.store(true, Ordering::SeqCst);
                0
            }))
        });
    }
}
#[cfg(feature = "io-profiling")]
struct Activity<'a>(&'a Recorder);
#[cfg(feature = "io-profiling")]
impl<'a> Activity<'a> {
    fn new(recorder: &'a Recorder) -> Self {
        recorder.add(&recorder.writers, 1);
        recorder.add(&recorder.sequence, 1);
        Self(recorder)
    }
}
#[cfg(feature = "io-profiling")]
impl Drop for Activity<'_> {
    fn drop(&mut self) {
        self.0.add(&self.0.sequence, 1);
        self.0.subtract(&self.0.writers);
    }
}

#[cfg(feature = "io-profiling")]
fn publish(
    recorder: &Recorder,
    stage: RuntimeStage,
    outcome: RuntimeOutcome,
    duration: Duration,
    entered: bool,
) {
    let elapsed = u64::try_from(duration.as_nanos()).unwrap_or_else(|_| {
        recorder.saturated.store(true, Ordering::SeqCst);
        u64::MAX
    });
    {
        let _activity = Activity::new(recorder);
        let row = &recorder.rows[stage as usize];
        if !entered {
            recorder.add(&row.calls, 1);
        } else {
            recorder.subtract(&row.in_flight);
        }
        recorder.add(
            match outcome {
                RuntimeOutcome::Success => &row.success,
                RuntimeOutcome::Error => &row.error,
                RuntimeOutcome::Cancelled => &row.cancelled,
            },
            1,
        );
        recorder.add(&row.elapsed_ns, elapsed);
        row.max_elapsed_ns.fetch_max(elapsed, Ordering::SeqCst);
        recorder.add(&row.histogram[bucket(elapsed)], 1);
    }
    // The caller must publish outside its state lock. Fixed-field trace output
    // has a separate finite lifetime budget, including failed writer attempts.
    if recorder.trace && elapsed >= SLOW_NS {
        let _ = write_slow_record(|| io::stderr().lock(), recorder, stage, outcome, elapsed);
    }
}
#[cfg(feature = "io-profiling")]
fn bucket(elapsed_ns: u64) -> usize {
    let micros = (elapsed_ns / 1_000).max(1);
    (63 - micros.leading_zeros() as usize).min(BUCKETS - 1)
}

#[cfg(feature = "io-profiling")]
struct SlowRecord {
    bytes: [u8; RECORD_BYTES],
    length: usize,
}
#[cfg(feature = "io-profiling")]
impl fmt::Write for SlowRecord {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.length.checked_add(value.len()).ok_or(fmt::Error)?;
        let target = self.bytes.get_mut(self.length..end).ok_or(fmt::Error)?;
        target.copy_from_slice(value.as_bytes());
        self.length = end;
        Ok(())
    }
}
#[cfg(feature = "io-profiling")]
fn slow_record(
    stage: RuntimeStage,
    outcome: RuntimeOutcome,
    elapsed: u64,
) -> std::result::Result<SlowRecord, fmt::Error> {
    let mut record = SlowRecord {
        bytes: [0; RECORD_BYTES],
        length: 0,
    };
    writeln!(
        &mut record,
        "mount-rs runtime stage={} outcome={} elapsed_ns={}",
        NAMES[stage as usize],
        outcome.name(),
        elapsed
    )?;
    Ok(record)
}
#[cfg(feature = "io-profiling")]
fn write_slow_record<W: Write>(
    acquire_writer: impl FnOnce() -> W,
    recorder: &Recorder,
    stage: RuntimeStage,
    outcome: RuntimeOutcome,
    elapsed: u64,
) -> io::Result<bool> {
    if !recorder.trace || elapsed < SLOW_NS {
        return Ok(false);
    }
    if recorder
        .slow_records
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
            (old < SLOW_RECORDS).then(|| old + 1)
        })
        .is_err()
    {
        return Ok(false);
    }
    let record = slow_record(stage, outcome, elapsed)
        .map_err(|_| io::Error::other("runtime trace record exceeded fixed bound"))?;
    let mut writer = acquire_writer();
    writer.write_all(&record.bytes[..record.length])?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_observer_never_records() {
        let observer = RuntimeDiagnostics::default();
        assert!(!observer.enabled());
        let mut span = RuntimeSpan::new(Some(&observer), RuntimeStage::Acquire);
        span.finish_elapsed(RuntimeOutcome::Success, Duration::from_secs(1));
        span.finish(RuntimeOutcome::Error);
        observer.record_elapsed(
            RuntimeStage::Open,
            RuntimeOutcome::Error,
            Duration::from_secs(1),
        );
        assert!(observer.snapshot().is_none());
    }

    #[cfg(not(feature = "io-profiling"))]
    #[test]
    fn unavailable_feature_has_no_recorder_or_clock_state() {
        let observer = RuntimeDiagnostics::new(true);
        assert!(!observer.enabled());
        assert!(observer.snapshot().is_none());
        assert_eq!(std::mem::size_of::<RuntimeDiagnostics>(), 0);
        assert_eq!(std::mem::size_of::<RuntimeSpan>(), 0);
        assert_eq!(std::mem::size_of::<RuntimeCompleted>(), 0);
        let mut span = RuntimeSpan::new_deferred(Some(&observer), RuntimeStage::StateMutexHold);
        span.detach(RuntimeOutcome::Success).publish();
    }

    #[cfg(feature = "io-profiling")]
    #[test]
    fn ordinary_finish_is_idempotent_and_drop_cancels_unfinished_span() {
        let observer = RuntimeDiagnostics::new(false);
        let mut span = RuntimeSpan::new(Some(&observer), RuntimeStage::Acquire);
        assert_eq!(observer.snapshot().unwrap().entries[0].in_flight, 1);
        span.finish_elapsed(RuntimeOutcome::Success, Duration::from_micros(4));
        span.finish_elapsed(RuntimeOutcome::Error, Duration::from_secs(1));
        drop(span);
        drop(RuntimeSpan::new(Some(&observer), RuntimeStage::Open));
        let snapshot = observer.snapshot().unwrap();
        let row = &snapshot.entries[0];
        assert_eq!(
            (
                row.calls,
                row.success,
                row.error,
                row.cancelled,
                row.in_flight
            ),
            (1, 1, 0, 0, 0)
        );
        assert_eq!(
            (row.elapsed_ns, row.max_elapsed_ns, row.latency_log2_us[2]),
            (4_000, 4_000, 1)
        );
        let cancelled = &snapshot.entries[RuntimeStage::Open as usize];
        assert_eq!(
            (cancelled.calls, cancelled.cancelled, cancelled.in_flight),
            (1, 1, 0)
        );
        assert!(!snapshot.concurrent_activity);
    }

    #[cfg(feature = "io-profiling")]
    #[test]
    fn deferred_completion_publishes_only_after_explicit_unlock_point() {
        let observer = RuntimeDiagnostics::new(false);
        let state = std::sync::Mutex::new(());
        let mut span = RuntimeSpan::new_deferred(Some(&observer), RuntimeStage::StateMutexHold);
        let guard = state.lock().unwrap();
        let mut completed = span.detach_elapsed(RuntimeOutcome::Success, Duration::from_micros(2));
        let row = &observer.snapshot().unwrap().entries[RuntimeStage::StateMutexHold as usize];
        assert_eq!((row.calls, row.in_flight, row.success), (0, 0, 0));
        drop(guard);
        completed.publish();
        completed.publish();
        drop(completed);
        drop(span);
        let row = &observer.snapshot().unwrap().entries[RuntimeStage::StateMutexHold as usize];
        assert_eq!(
            (row.calls, row.in_flight, row.success, row.cancelled),
            (1, 0, 1, 0)
        );
        assert_eq!((row.elapsed_ns, row.latency_log2_us[1]), (2_000, 1));
    }

    #[cfg(feature = "io-profiling")]
    #[test]
    fn detached_ordinary_record_retains_in_flight_until_publication() {
        let observer = RuntimeDiagnostics::new(false);
        let mut span = RuntimeSpan::new(Some(&observer), RuntimeStage::HandleClose);
        let completed = span.detach(RuntimeOutcome::Error);
        drop(span);
        let row = &observer.snapshot().unwrap().entries[RuntimeStage::HandleClose as usize];
        assert_eq!((row.calls, row.in_flight, row.error), (1, 1, 0));
        drop(completed);
        let row = &observer.snapshot().unwrap().entries[RuntimeStage::HandleClose as usize];
        assert_eq!(
            (row.calls, row.in_flight, row.error, row.cancelled),
            (1, 0, 1, 0)
        );
    }

    #[cfg(feature = "io-profiling")]
    #[test]
    fn numeric_overflow_and_histogram_are_finite_and_visible() {
        for (elapsed, expected) in [
            (0, 0),
            (999, 0),
            (1_000, 0),
            (1_999, 0),
            (2_000, 1),
            (4_000, 2),
            (u64::MAX, 31),
        ] {
            assert_eq!(bucket(elapsed), expected);
        }
        let observer = RuntimeDiagnostics::new(false);
        let recorder = observer.recorder.as_ref().unwrap();
        let row = &recorder.rows[RuntimeStage::Acquire as usize];
        row.calls.store(u64::MAX, Ordering::SeqCst);
        row.elapsed_ns.store(u64::MAX - 1, Ordering::SeqCst);
        row.histogram[31].store(u64::MAX, Ordering::SeqCst);
        let mut span = RuntimeSpan::new(Some(&observer), RuntimeStage::Acquire);
        span.finish_elapsed(
            RuntimeOutcome::Success,
            Duration::new(u64::MAX, 999_999_999),
        );
        let snapshot = observer.snapshot().unwrap();
        let row = &snapshot.entries[0];
        assert!(snapshot.counter_saturated);
        assert!(snapshot.concurrent_activity);
        assert_eq!(
            (
                row.calls,
                row.elapsed_ns,
                row.max_elapsed_ns,
                row.latency_log2_us[31]
            ),
            (u64::MAX, u64::MAX, u64::MAX, u64::MAX)
        );
        assert_eq!((row.success, row.in_flight), (1, 0));
        assert_eq!(snapshot.entries.len(), 8);
    }

    #[cfg(feature = "io-profiling")]
    #[test]
    fn saturated_sequence_and_writer_boundaries_make_capture_uncertain() {
        let observer = RuntimeDiagnostics::new(false);
        let recorder = observer.recorder.as_ref().unwrap();
        recorder.sequence.store(u64::MAX - 1, Ordering::SeqCst);
        {
            let _activity = Activity::new(recorder);
            let snapshot = observer.snapshot().unwrap();
            assert!(snapshot.concurrent_activity);
        }
        assert_eq!(recorder.sequence.load(Ordering::SeqCst), u64::MAX);
        assert_eq!(recorder.writers.load(Ordering::SeqCst), 0);
        let mut span = RuntimeSpan::new(Some(&observer), RuntimeStage::Acquire);
        span.finish_elapsed(RuntimeOutcome::Success, Duration::ZERO);
        let snapshot = observer.snapshot().unwrap();
        assert!(snapshot.counter_saturated && snapshot.concurrent_activity);
        assert_eq!(recorder.sequence.load(Ordering::SeqCst), u64::MAX);
        assert_eq!(recorder.writers.load(Ordering::SeqCst), 0);
        assert_eq!(snapshot.entries[0].success, 1);

        let observer = RuntimeDiagnostics::new(false);
        let recorder = observer.recorder.as_ref().unwrap();
        recorder.writers.store(u64::MAX, Ordering::SeqCst);
        {
            let _activity = Activity::new(recorder);
            assert!(observer.snapshot().unwrap().counter_saturated);
        }
        assert_eq!(recorder.writers.load(Ordering::SeqCst), u64::MAX - 1);
        let snapshot = observer.snapshot().unwrap();
        assert!(snapshot.counter_saturated && snapshot.concurrent_activity);

        let observer = RuntimeDiagnostics::new(false);
        let recorder = observer.recorder.as_ref().unwrap();
        recorder.subtract(&recorder.writers);
        assert_eq!(recorder.writers.load(Ordering::SeqCst), 0);
        let snapshot = observer.snapshot().unwrap();
        assert!(snapshot.counter_saturated && snapshot.concurrent_activity);
    }

    #[cfg(feature = "io-profiling")]
    #[test]
    fn slow_formatter_is_fixed_and_observer_budget_is_bounded() {
        let stages = [
            RuntimeStage::Acquire,
            RuntimeStage::ActivationWait,
            RuntimeStage::Open,
            RuntimeStage::EvictionShutdown,
            RuntimeStage::TerminalDrain,
            RuntimeStage::HandleClose,
            RuntimeStage::StateMutexWait,
            RuntimeStage::StateMutexHold,
        ];
        for stage in stages {
            for outcome in [
                RuntimeOutcome::Success,
                RuntimeOutcome::Error,
                RuntimeOutcome::Cancelled,
            ] {
                let record = slow_record(stage, outcome, u64::MAX).unwrap();
                assert!(record.length < RECORD_BYTES);
                let text = std::str::from_utf8(&record.bytes[..record.length]).unwrap();
                assert_eq!(text.split_whitespace().count(), 5);
                assert!(text.starts_with("mount-rs runtime stage=runtime."));
                assert!(text.ends_with("elapsed_ns=18446744073709551615\n"));
            }
        }
        let recorder = Recorder::new(true);
        let mut bytes = Vec::new();
        for _ in 0..SLOW_RECORDS {
            assert!(
                write_slow_record(
                    || &mut bytes,
                    &recorder,
                    RuntimeStage::Open,
                    RuntimeOutcome::Success,
                    SLOW_NS
                )
                .unwrap()
            );
        }
        assert!(
            !write_slow_record(
                || &mut bytes,
                &recorder,
                RuntimeStage::Open,
                RuntimeOutcome::Success,
                SLOW_NS
            )
            .unwrap()
        );
        assert_eq!(recorder.slow_records.load(Ordering::SeqCst), SLOW_RECORDS);
        assert_eq!(
            bytes.iter().filter(|byte| **byte == b'\n').count(),
            SLOW_RECORDS as usize
        );
        assert!(bytes.len() <= SLOW_RECORDS as usize * RECORD_BYTES);
        let other = Recorder::new(true);
        assert!(
            write_slow_record(
                Vec::new,
                &other,
                RuntimeStage::Open,
                RuntimeOutcome::Success,
                SLOW_NS
            )
            .unwrap()
        );
    }

    #[cfg(feature = "io-profiling")]
    #[test]
    fn disabled_and_short_records_do_not_spend_budget_failed_writes_do() {
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected failure"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let disabled = Recorder::new(false);
        assert!(
            !write_slow_record(
                || FailedWriter,
                &disabled,
                RuntimeStage::Open,
                RuntimeOutcome::Error,
                u64::MAX
            )
            .unwrap()
        );
        assert_eq!(disabled.slow_records.load(Ordering::SeqCst), 0);
        let enabled = Recorder::new(true);
        assert!(
            !write_slow_record(
                || FailedWriter,
                &enabled,
                RuntimeStage::Open,
                RuntimeOutcome::Error,
                SLOW_NS - 1
            )
            .unwrap()
        );
        assert_eq!(enabled.slow_records.load(Ordering::SeqCst), 0);
        assert!(
            write_slow_record(
                || FailedWriter,
                &enabled,
                RuntimeStage::Open,
                RuntimeOutcome::Error,
                SLOW_NS
            )
            .is_err()
        );
        assert_eq!(enabled.slow_records.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "io-profiling")]
    #[test]
    fn disabled_short_and_exhausted_records_never_acquire_writer() {
        for (trace, elapsed, attempts) in [
            (false, u64::MAX, 0),
            (true, SLOW_NS - 1, 0),
            (true, SLOW_NS, SLOW_RECORDS),
        ] {
            let recorder = Recorder::new(trace);
            recorder.slow_records.store(attempts, Ordering::SeqCst);
            let acquired = std::cell::Cell::new(0);
            assert!(
                !write_slow_record(
                    || {
                        acquired.set(acquired.get() + 1);
                        io::sink()
                    },
                    &recorder,
                    RuntimeStage::Open,
                    RuntimeOutcome::Error,
                    elapsed,
                )
                .unwrap()
            );
            assert_eq!(acquired.get(), 0);
            assert_eq!(recorder.slow_records.load(Ordering::SeqCst), attempts);
        }
    }
}
