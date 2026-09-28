//! Bounded, opt-in tracing for native qualification work, never production I/O.
use serde::Serialize;
use std::{
    ffi::OsStr,
    future::Future,
    io::{self, Write},
    pin::Pin,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

pub const ENV: &str = "MOUNT_RS_TEN_PROCESS_TRACE_PROGRESS";
pub const PREFIX: &[u8] = b"native_progress ";
pub const MAX_SPANS: usize = 2048;
pub const MAX_LINES: usize = MAX_SPANS * 2;
/// Every physical line, including its LF, is strictly smaller than this bound.
pub const LINE_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug)]
pub enum Label {
    CheckedPoll,
    FleetCheck,
    FleetResourceSample,
    FleetLogRead,
    FleetDiskStat,
    FleetChildSample,
    OracleCaptureBefore,
    OracleActionPoll,
    OracleCleanupPoll,
    OracleCaptureAfter,
}
impl Label {
    fn as_str(self) -> &'static str {
        match self {
            Self::CheckedPoll => "checked_poll",
            Self::FleetCheck => "fleet_check",
            Self::FleetResourceSample => "fleet_resource_sample",
            Self::FleetLogRead => "fleet_log_read",
            Self::FleetDiskStat => "fleet_disk_stat",
            Self::FleetChildSample => "fleet_child_sample",
            Self::OracleCaptureBefore => "oracle_capture_before",
            Self::OracleActionPoll => "oracle_action_poll",
            Self::OracleCleanupPoll => "oracle_cleanup_poll",
            Self::OracleCaptureAfter => "oracle_capture_after",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Snapshot {
    pub enabled: bool,
    pub reserved_spans: usize,
    pub cap_reached: bool,
    pub clock_unavailable: bool,
    pub io_unavailable: bool,
}

fn opted_in(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

trait Clock {
    fn now_ns(&self) -> Result<u64, ()>;
}
trait Output {
    fn write_line(&self, bytes: &[u8]) -> io::Result<()>;
}
struct NativeClock;
impl Clock for NativeClock {
    fn now_ns(&self) -> Result<u64, ()> {
        super::process::monotonic_ns().map_err(|_| ())
    }
}
struct NativeOutput;
impl Output for NativeOutput {
    fn write_line(&self, bytes: &[u8]) -> io::Result<()> {
        io::stderr().lock().write_all(bytes)
    }
}

struct Trace<C, W> {
    enabled: bool,
    pid: u32,
    clock: C,
    writer: W,
    reserved: AtomicUsize,
    cap_reached: AtomicBool,
    clock_unavailable: AtomicBool,
    io_unavailable: AtomicBool,
}
impl<C: Clock, W: Output> Trace<C, W> {
    fn new(enabled: bool, pid: u32, clock: C, writer: W) -> Self {
        Self {
            enabled,
            pid,
            clock,
            writer,
            reserved: AtomicUsize::new(0),
            cap_reached: AtomicBool::new(false),
            clock_unavailable: AtomicBool::new(false),
            io_unavailable: AtomicBool::new(false),
        }
    }

    fn span(&self, label: Label) -> Guard<'_, C, W> {
        let mut guard = Guard {
            trace: self,
            label,
            active: false,
            span: 0,
            started_ns: None,
            enter_written: false,
        };
        if !self.enabled {
            return guard;
        }
        let Ok(previous) =
            self.reserved
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    (value < MAX_SPANS).then_some(value + 1)
                })
        else {
            self.cap_reached.store(true, Ordering::Relaxed);
            return guard;
        };
        guard.active = true;
        guard.span = previous + 1;
        guard.started_ns = self.clock.now_ns().ok();
        let availability = if guard.started_ns.is_some() {
            "available"
        } else {
            self.clock_unavailable.store(true, Ordering::Relaxed);
            "clock_unavailable"
        };
        guard.enter_written = self.emit(label, guard.span, "enter", guard.started_ns, availability);
        guard
    }

    fn emit(
        &self,
        label: Label,
        span: usize,
        event: &'static str,
        monotonic_ns: Option<u64>,
        availability: &'static str,
    ) -> bool {
        let record = Record {
            schema: 1,
            pid: self.pid,
            span,
            label: label.as_str(),
            event,
            monotonic_ns,
            availability,
        };
        let mut line = Line {
            bytes: [0; LINE_LIMIT],
            len: 0,
        };
        let result = (|| -> io::Result<()> {
            line.write_all(PREFIX)?;
            serde_json::to_writer(&mut line, &record).map_err(io::Error::other)?;
            line.write_all(b"\n")?;
            self.writer.write_line(&line.bytes[..line.len])
        })();
        if result.is_err() {
            self.io_unavailable.store(true, Ordering::Relaxed);
        }
        result.is_ok()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            enabled: self.enabled,
            reserved_spans: self.reserved.load(Ordering::Relaxed),
            cap_reached: self.cap_reached.load(Ordering::Relaxed),
            clock_unavailable: self.clock_unavailable.load(Ordering::Relaxed),
            io_unavailable: self.io_unavailable.load(Ordering::Relaxed),
        }
    }
}

struct Guard<'a, C: Clock, W: Output> {
    trace: &'a Trace<C, W>,
    label: Label,
    active: bool,
    span: usize,
    started_ns: Option<u64>,
    enter_written: bool,
}
impl<C: Clock, W: Output> Drop for Guard<'_, C, W> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let (finished_ns, availability) = match self.trace.clock.now_ns() {
            Err(()) => {
                self.trace.clock_unavailable.store(true, Ordering::Relaxed);
                (None, "clock_unavailable")
            }
            Ok(end) if self.started_ns.is_some_and(|start| end < start) => {
                self.trace.clock_unavailable.store(true, Ordering::Relaxed);
                (None, "clock_regressed")
            }
            Ok(end) => (
                Some(end),
                if self.started_ns.is_none() {
                    "clock_unavailable"
                } else if !self.enter_written {
                    "output_unavailable"
                } else {
                    "available"
                },
            ),
        };
        self.trace
            .emit(self.label, self.span, "leave", finished_ns, availability);
    }
}

#[derive(Serialize)]
struct Record {
    schema: u8,
    pid: u32,
    span: usize,
    label: &'static str,
    event: &'static str,
    monotonic_ns: Option<u64>,
    availability: &'static str,
}
struct Line {
    bytes: [u8; LINE_LIMIT],
    len: usize,
}
impl Write for Line {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .len
            .checked_add(bytes.len())
            .filter(|end| *end < LINE_LIMIT)
            .ok_or_else(|| io::Error::other("native progress line bound"))?;
        self.bytes[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

static GLOBAL: OnceLock<Trace<NativeClock, NativeOutput>> = OnceLock::new();
fn global() -> &'static Trace<NativeClock, NativeOutput> {
    GLOBAL.get_or_init(|| {
        Trace::new(
            opted_in(std::env::var_os(ENV).as_deref()),
            std::process::id(),
            NativeClock,
            NativeOutput,
        )
    })
}

#[must_use = "retain the guard until synchronous work finishes"]
pub struct Span {
    _inner: Guard<'static, NativeClock, NativeOutput>,
}
pub fn span(label: Label) -> Span {
    Span {
        _inner: global().span(label),
    }
}
pub fn snapshot() -> Snapshot {
    global().snapshot()
}
/// Bracket one poll only; no guard is retained while the future is suspended.
pub fn poll<F: Future>(
    future: Pin<&mut F>,
    context: &mut Context<'_>,
    label: Label,
) -> Poll<F::Output> {
    poll_with(global(), future, context, label)
}
fn poll_with<F: Future, C: Clock, W: Output>(
    trace: &Trace<C, W>,
    future: Pin<&mut F>,
    context: &mut Context<'_>,
    label: Label,
) -> Poll<F::Output> {
    let _span = trace.span(label);
    future.poll(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Mutex, atomic::AtomicU64},
        task::Waker,
    };

    struct TestClock {
        calls: AtomicUsize,
        next: AtomicU64,
        values: Mutex<VecDeque<Result<u64, ()>>>,
    }
    impl TestClock {
        fn new(values: &[Result<u64, ()>]) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                next: AtomicU64::new(1000),
                values: Mutex::new(values.iter().copied().collect()),
            }
        }
    }
    impl Clock for TestClock {
        fn now_ns(&self) -> Result<u64, ()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.values
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(self.next.fetch_add(1, Ordering::Relaxed)))
        }
    }
    struct TestOutput {
        calls: AtomicUsize,
        fail_call: Option<usize>,
        lines: Mutex<Vec<Vec<u8>>>,
    }
    impl TestOutput {
        fn new(fail_call: Option<usize>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail_call,
                lines: Mutex::new(Vec::new()),
            }
        }
        fn records(&self) -> Vec<Value> {
            self.lines
                .lock()
                .unwrap()
                .iter()
                .map(|line| {
                    assert!(line.len() < LINE_LIMIT);
                    assert!(line.starts_with(PREFIX));
                    assert_eq!(line.last(), Some(&b'\n'));
                    assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
                    serde_json::from_slice(&line[PREFIX.len()..line.len() - 1]).unwrap()
                })
                .collect()
        }
    }
    impl Output for TestOutput {
        fn write_line(&self, bytes: &[u8]) -> io::Result<()> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if Some(call) == self.fail_call {
                return Err(io::Error::other("injected output failure"));
            }
            self.lines.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
    }
    fn fixture(
        enabled: bool,
        values: &[Result<u64, ()>],
        fail_call: Option<usize>,
    ) -> Trace<TestClock, TestOutput> {
        Trace::new(
            enabled,
            4242,
            TestClock::new(values),
            TestOutput::new(fail_call),
        )
    }
    fn complete_interval(records: &[Value]) -> bool {
        records.len() == 2
            && records[0]["event"] == "enter"
            && records[1]["event"] == "leave"
            && records[0]["span"] == records[1]["span"]
            && records[0]["pid"] == records[1]["pid"]
            && records[0]["label"] == records[1]["label"]
            && records
                .iter()
                .all(|record| record["availability"] == "available")
            && match (
                records[0]["monotonic_ns"].as_u64(),
                records[1]["monotonic_ns"].as_u64(),
            ) {
                (Some(start), Some(end)) => end >= start,
                _ => false,
            }
    }
    struct PendingThenReady {
        polls: usize,
    }
    impl Future for PendingThenReady {
        type Output = u32;
        fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<u32> {
            self.polls += 1;
            if self.polls == 1 {
                Poll::Pending
            } else {
                Poll::Ready(99)
            }
        }
    }

    #[test]
    fn native_progress_disabled_performs_no_output_or_clock_reads_and_requires_exact_opt_in() {
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("01"),
            Some("true"),
            Some(" 1"),
            Some("1\n"),
        ] {
            assert!(!opted_in(value.map(OsStr::new)));
        }
        assert!(opted_in(Some(OsStr::new("1"))));
        let trace = fixture(false, &[], None);
        drop(trace.span(Label::FleetCheck));
        let mut future = PendingThenReady { polls: 0 };
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(
            poll_with(
                &trace,
                Pin::new(&mut future),
                &mut context,
                Label::CheckedPoll
            ),
            Poll::Pending
        );
        assert_eq!(trace.clock.calls.load(Ordering::Relaxed), 0);
        assert_eq!(trace.writer.calls.load(Ordering::Relaxed), 0);
        assert_eq!(trace.snapshot().reserved_spans, 0);
    }

    #[test]
    fn native_progress_monotonic_pair_preserves_numeric_identity_and_physical_bounds() {
        let trace = fixture(true, &[Ok(u64::MAX - 5), Ok(u64::MAX)], None);
        drop(trace.span(Label::FleetCheck));
        let records = trace.writer.records();
        assert!(
            complete_interval(&records),
            "enabled tracing must emit a complete monotonic enter/leave pair"
        );
        assert_eq!(records[0]["schema"], 1);
        assert_eq!(records[0]["pid"], 4242);
        assert_eq!(records[0]["span"], 1);
        assert_eq!(records[0]["label"], "fleet_check");
        assert_eq!(records[0]["monotonic_ns"].as_u64(), Some(u64::MAX - 5));
        assert_eq!(records[1]["monotonic_ns"].as_u64(), Some(u64::MAX));
        assert_eq!(trace.clock.calls.load(Ordering::Relaxed), 2);
        assert_eq!(trace.snapshot().reserved_spans, 1);
        const { assert!(MAX_LINES * (LINE_LIMIT - 1) < 1024 * 1024) };
    }

    #[test]
    fn native_progress_cap_is_atomic_bounded_and_never_reset() {
        let trace = fixture(true, &[], None);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..(MAX_SPANS / 4 + 1) {
                        drop(trace.span(Label::FleetChildSample));
                    }
                });
            }
        });
        let records = trace.writer.records();
        assert_eq!(
            records.len(),
            MAX_LINES,
            "shared concurrent reservations must stop at the fixed span cap"
        );
        let mut groups = BTreeMap::<u64, Vec<Value>>::new();
        for record in records {
            groups
                .entry(record["span"].as_u64().unwrap())
                .or_default()
                .push(record);
        }
        assert_eq!(groups.len(), MAX_SPANS);
        for records in groups.values() {
            assert!(complete_interval(records));
        }
        let clock_calls = trace.clock.calls.load(Ordering::Relaxed);
        let writer_calls = trace.writer.calls.load(Ordering::Relaxed);
        drop(trace.span(Label::FleetCheck));
        assert_eq!(trace.clock.calls.load(Ordering::Relaxed), clock_calls);
        assert_eq!(trace.writer.calls.load(Ordering::Relaxed), writer_calls);
        assert_eq!(trace.snapshot().reserved_spans, MAX_SPANS);
        assert!(trace.snapshot().cap_reached);
        assert!(
            trace
                .writer
                .lines
                .lock()
                .unwrap()
                .iter()
                .map(Vec::len)
                .sum::<usize>()
                < 1024 * 1024
        );
    }

    #[test]
    fn native_progress_clock_and_io_failures_never_fake_complete_interval() {
        for values in [[Err(()), Ok(100)], [Ok(100), Err(())], [Ok(200), Ok(100)]] {
            let trace = fixture(true, &values, None);
            drop(trace.span(Label::OracleCaptureBefore));
            assert!(
                trace.snapshot().clock_unavailable,
                "clock failure or regression must remain observable"
            );
            let records = trace.writer.records();
            assert_eq!(records.len(), 2);
            assert!(!complete_interval(&records));
            assert!(
                records
                    .iter()
                    .all(|record| record["monotonic_ns"].as_u64() != Some(0))
            );
        }
        for fail_call in [1, 2] {
            let trace = fixture(true, &[Ok(100), Ok(200)], Some(fail_call));
            drop(trace.span(Label::OracleCaptureAfter));
            assert!(
                trace.snapshot().io_unavailable,
                "output failure must remain observable"
            );
            assert!(!complete_interval(&trace.writer.records()));
        }
    }

    #[test]
    fn native_progress_each_pending_and_ready_poll_has_its_own_interval() {
        let trace = fixture(true, &[Ok(100), Ok(110), Ok(500), Ok(510)], None);
        let mut future = PendingThenReady { polls: 0 };
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(
            poll_with(
                &trace,
                Pin::new(&mut future),
                &mut context,
                Label::OracleActionPoll
            ),
            Poll::Pending
        );
        let pending = trace.writer.records();
        assert!(
            complete_interval(&pending),
            "Pending must close its poll interval before suspension"
        );
        assert_eq!(
            poll_with(
                &trace,
                Pin::new(&mut future),
                &mut context,
                Label::OracleActionPoll
            ),
            Poll::Ready(99)
        );
        let all = trace.writer.records();
        assert_eq!(all.len(), 4);
        assert!(complete_interval(&all[2..]));
        assert_ne!(all[0]["span"], all[2]["span"]);
        assert_eq!(all[1]["monotonic_ns"], 110);
        assert_eq!(all[2]["monotonic_ns"], 500);
        assert_eq!(future.polls, 2);
    }
}
