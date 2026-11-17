//! The `serve-http` access log: one JSON line on stderr per request.
//!
//! A request only formats its line and offers it to a bounded queue; one
//! thread owns the writing. A stderr that has stopped being read therefore
//! costs lines, which are counted and reported, and never holds a request.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mount_rs_http::{AccessLog, AccessRecord};

/// Lines that may wait for the writer before new ones are dropped.
const QUEUED_LINES: usize = 8192;
/// Lines written with one lock of the output.
const BATCH_LINES: usize = 256;

enum Message {
    Line(String),
    /// Answered once everything queued before it has been written.
    Flush(SyncSender<()>),
}

#[derive(Debug)]
pub(crate) struct JsonAccessLog {
    queue: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
}

impl JsonAccessLog {
    /// Write to this process's stderr, the stream its other JSON event
    /// lines use.
    pub(crate) fn stderr() -> std::io::Result<Arc<Self>> {
        Self::start(QUEUED_LINES, std::io::stderr)
    }

    fn start<W: Write>(
        queued_lines: usize,
        output: impl Fn() -> W + Send + 'static,
    ) -> std::io::Result<Arc<Self>> {
        let (queue, lines) = std::sync::mpsc::sync_channel(queued_lines);
        let dropped = Arc::new(AtomicU64::new(0));
        let reported = Arc::clone(&dropped);
        std::thread::Builder::new()
            .name("http-access-log".to_owned())
            .spawn(move || write_lines(&lines, &reported, output))?;
        Ok(Arc::new(Self { queue, dropped }))
    }

    /// Wait, for at most `limit`, until every line recorded so far has been
    /// written. Called once the listener has stopped, so that the lines of
    /// the last requests are not lost with the process.
    pub(crate) fn flush(&self, limit: Duration) {
        let deadline = Instant::now() + limit;
        let (written, wait) = std::sync::mpsc::sync_channel(1);
        let mut marker = Message::Flush(written);
        loop {
            match self.queue.try_send(marker) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return;
                    }
                    marker = returned;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        let _ = wait.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

impl AccessLog for JsonAccessLog {
    fn record(&self, record: &AccessRecord<'_>) {
        if self
            .queue
            .try_send(Message::Line(render(record, SystemTime::now())))
            .is_err()
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The line for one request. Every value is one the server chose or
/// checked: there is no path, query or header text in it.
fn render(record: &AccessRecord<'_>, finished: SystemTime) -> String {
    let unix_ms = finished
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    let mut line = serde_json::json!({
        "event": "http_request",
        "unix_ms": unix_ms,
        "request_id": record.request_id,
        "method": record.method,
        "route": record.route,
        "status": record.status,
        "duration_ms": record.duration.as_micros() as f64 / 1000.0,
        "bytes_in": record.bytes_in,
        "bytes_out": record.bytes_out,
    });
    if let Some(drive) = record.drive {
        line["drive"] = drive.into();
    }
    line.to_string()
}

fn write_lines<W: Write>(lines: &Receiver<Message>, dropped: &AtomicU64, output: impl Fn() -> W) {
    let mut reported = 0;
    let mut batch = Vec::new();
    loop {
        // Wake now and then even when idle, to report lines dropped while
        // the queue was full.
        let first = match lines.recv_timeout(Duration::from_secs(1)) {
            Ok(message) => Some(message),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut flushes = Vec::new();
        let mut next = first;
        let mut count = 0;
        while let Some(message) = next {
            match message {
                Message::Line(line) => {
                    batch.extend_from_slice(line.as_bytes());
                    batch.push(b'\n');
                    count += 1;
                }
                Message::Flush(written) => flushes.push(written),
            }
            if count >= BATCH_LINES {
                break;
            }
            next = lines.try_recv().ok();
        }
        let total = dropped.load(Ordering::Relaxed);
        if total != reported {
            reported = total;
            let record = serde_json::json!({"event": "http_access_log_dropped", "total": total});
            batch.extend_from_slice(record.to_string().as_bytes());
            batch.push(b'\n');
        }
        if !batch.is_empty() {
            let mut output = output();
            let _ = output.write_all(&batch);
            let _ = output.flush();
            batch.clear();
        }
        for written in flushes {
            let _ = written.try_send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Shared {
        fn lines(&self) -> Vec<serde_json::Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
                .collect()
        }
    }

    fn record(status: Option<u16>, drive: Option<&'static str>) -> AccessRecord<'static> {
        AccessRecord {
            request_id: "caller-7",
            method: "PUT",
            route: "/v1/drives/{id}/fs/{path}",
            drive,
            status,
            duration: Duration::from_micros(12_345),
            bytes_in: 19,
            bytes_out: 44,
        }
    }

    #[test]
    fn a_line_has_the_documented_fields_and_nothing_else() {
        let finished = UNIX_EPOCH + Duration::from_millis(1_794_439_474_775);
        let line: serde_json::Value =
            serde_json::from_str(&render(&record(Some(200), Some("files")), finished)).unwrap();
        assert_eq!(
            line,
            serde_json::json!({
                "event": "http_request",
                "unix_ms": 1_794_439_474_775_u64,
                "request_id": "caller-7",
                "method": "PUT",
                "route": "/v1/drives/{id}/fs/{path}",
                "drive": "files",
                "status": 200,
                "duration_ms": 12.345,
                "bytes_in": 19,
                "bytes_out": 44,
            })
        );
        // No response, and no configured drive named.
        let line: serde_json::Value =
            serde_json::from_str(&render(&record(None, None), finished)).unwrap();
        assert_eq!(line["status"], serde_json::Value::Null);
        assert!(line.get("drive").is_none());
        assert!(!render(&record(None, None), finished).contains('\n'));
    }

    #[test]
    fn recorded_lines_are_written_in_order_by_the_time_flush_returns() {
        let output = Shared::default();
        let sink = output.clone();
        let log = JsonAccessLog::start(64, move || sink.clone()).unwrap();
        for status in [200, 404, 503] {
            log.record(&record(Some(status), None));
        }
        log.flush(Duration::from_secs(5));
        let statuses: Vec<_> = output
            .lines()
            .iter()
            .map(|line| line["status"].as_u64().unwrap())
            .collect();
        assert_eq!(statuses, [200, 404, 503]);
    }

    /// An output that accepts nothing until it is released.
    #[derive(Clone)]
    struct Gated {
        open: Arc<(Mutex<bool>, std::sync::Condvar)>,
        output: Shared,
    }

    impl Write for Gated {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let (open, changed) = &*self.open;
            let mut open = open.lock().unwrap();
            while !*open {
                open = changed.wait(open).unwrap();
            }
            self.output.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_stalled_output_drops_lines_without_blocking_and_reports_how_many() {
        let gate = Gated {
            open: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            output: Shared::default(),
        };
        let sink = gate.clone();
        let log = JsonAccessLog::start(4, move || sink.clone()).unwrap();
        // The writer takes at most one batch and stalls; the queue holds
        // four more; everything else has nowhere to go.
        let started = Instant::now();
        for _ in 0..1000 {
            log.record(&record(Some(200), None));
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "recording waited for the stalled output"
        );
        let dropped = log.dropped.load(Ordering::Relaxed);
        assert!(
            dropped >= 1000 - 4 - BATCH_LINES as u64,
            "dropped {dropped}"
        );
        // A flush gives up at its limit instead of waiting for the output.
        let flushing = Instant::now();
        log.flush(Duration::from_millis(100));
        assert!(flushing.elapsed() < Duration::from_secs(2));

        *gate.open.0.lock().unwrap() = true;
        gate.open.1.notify_all();
        log.flush(Duration::from_secs(5));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let lines = gate.output.lines();
            let written = lines
                .iter()
                .filter(|line| line["event"] == "http_request")
                .count() as u64;
            let reported = lines
                .iter()
                .filter(|line| line["event"] == "http_access_log_dropped")
                .filter_map(|line| line["total"].as_u64())
                .max();
            if reported == Some(dropped) {
                assert_eq!(written + dropped, 1000, "every line is written or counted");
                break;
            }
            assert!(Instant::now() < deadline, "no drop report: {lines:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
