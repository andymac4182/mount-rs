use std::fmt::{self, Write as _};
use std::io::{self, Write as _};
use std::time::Instant;

const MAX_RECORDS: u8 = 16;
const MAX_RECORD_BYTES: usize = 512;
const MAX_SAFE_ELAPSED_MS: u128 = 9_007_199_254_740_991;

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    MainBodyEntered,
    S3CreateStarted,
    S3CreateCompleted,
    WebdavCreateStarted,
    WebdavCreateCompleted,
    WebdavListenStarted,
    WebdavListenCompleted,
    ReadyPublished,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::MainBodyEntered => "main_body_entered",
            Self::S3CreateStarted => "s3_create_started",
            Self::S3CreateCompleted => "s3_create_completed",
            Self::WebdavCreateStarted => "webdav_create_started",
            Self::WebdavCreateCompleted => "webdav_create_completed",
            Self::WebdavListenStarted => "webdav_listen_started",
            Self::WebdavListenCompleted => "webdav_listen_completed",
            Self::ReadyPublished => "ready_published",
        }
    }
}

#[derive(Clone, Copy)]
enum Reason {
    ClockInvalid,
    ClockRegressed,
    PublicationCap,
    SinkFailed,
    SinkShortWrite,
}

impl Reason {
    fn label(self) -> &'static str {
        match self {
            Self::ClockInvalid => "clock_invalid",
            Self::ClockRegressed => "clock_regressed",
            Self::PublicationCap => "publication_cap",
            Self::SinkFailed => "sink_failed",
            Self::SinkShortWrite => "sink_short_write",
        }
    }
}

pub(crate) struct Progress {
    started: Instant,
    pid: u32,
    seen: u16,
    attempted: u8,
    available: bool,
    previous: Option<u128>,
    reason: Option<Reason>,
}

impl Progress {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            pid: std::process::id(),
            seen: 0,
            attempted: 0,
            available: true,
            previous: None,
            reason: None,
        }
    }

    pub(crate) fn observe(&mut self, stage: Stage) {
        let started = self.started;
        self.observe_with(stage, &mut || started.elapsed().as_millis(), &mut |line| {
            // One bounded write: no retry, timer or ownership change. Blocking
            // sink latency is not bounded by the record byte/count limits.
            io::stderr().lock().write(line)
        });
    }

    fn observe_with(
        &mut self,
        stage: Stage,
        clock: &mut impl FnMut() -> u128,
        write: &mut impl FnMut(&[u8]) -> io::Result<usize>,
    ) {
        let mask = 1_u16 << stage as u8;
        if !self.available || self.seen & mask != 0 {
            return;
        }
        self.seen |= mask;
        let elapsed = clock();
        let invalid = if elapsed > MAX_SAFE_ELAPSED_MS {
            Some(Reason::ClockInvalid)
        } else if self.previous.is_some_and(|previous| elapsed < previous) {
            Some(Reason::ClockRegressed)
        } else {
            None
        };
        if let Some(reason) = invalid {
            self.disable(reason);
            self.publish("observer_unavailable", self.previous, self.reason, write);
            return;
        }
        self.previous = Some(elapsed);
        self.publish(stage.label(), Some(elapsed), None, write);
    }

    fn disable(&mut self, reason: Reason) {
        self.available = false;
        self.reason.get_or_insert(reason);
    }

    fn publish(
        &mut self,
        stage: &'static str,
        elapsed: Option<u128>,
        reason: Option<Reason>,
        write: &mut impl FnMut(&[u8]) -> io::Result<usize>,
    ) {
        if self.attempted >= MAX_RECORDS {
            self.disable(Reason::PublicationCap);
            return;
        }
        let mut line = LineBuffer::new();
        let availability = if reason.is_some() {
            "unavailable"
        } else {
            "observed"
        };
        let formatted = writeln!(
            line,
            "{{\"schema\":\"mount-rs.http-oracle-startup.v1\",\"pid\":{},\"scope\":\"application_async_main_body\",\"stage\":\"{}\",\"sequence\":{},\"elapsed_ms\":{},\"availability\":\"{}\",\"reason\":{},\"compiler_observation\":\"unobserved\"}}",
            self.pid,
            stage,
            self.attempted + 1,
            JsonElapsed(elapsed),
            availability,
            JsonReason(reason),
        );
        if formatted.is_err() {
            self.disable(Reason::PublicationCap);
            return;
        }
        self.attempted += 1;
        match write(&line.bytes[..line.length]) {
            Ok(length) if length == line.length => {}
            Ok(_) => self.disable(Reason::SinkShortWrite),
            Err(_) => self.disable(Reason::SinkFailed),
        }
    }
}

// Formatting is held in a fixed stack buffer before a single sink call.
struct LineBuffer {
    bytes: [u8; MAX_RECORD_BYTES],
    length: usize,
}

impl LineBuffer {
    fn new() -> Self {
        Self {
            bytes: [0; MAX_RECORD_BYTES],
            length: 0,
        }
    }
}

impl fmt::Write for LineBuffer {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.length.checked_add(value.len()).ok_or(fmt::Error)?;
        if end > self.bytes.len() {
            return Err(fmt::Error);
        }
        self.bytes[self.length..end].copy_from_slice(value.as_bytes());
        self.length = end;
        Ok(())
    }
}

struct JsonElapsed(Option<u128>);

impl fmt::Display for JsonElapsed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => write!(formatter, "{value}"),
            None => formatter.write_str("null"),
        }
    }
}

struct JsonReason(Option<Reason>);

impl fmt::Display for JsonReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(reason) => write!(formatter, "\"{}\"", reason.label()),
            None => formatter.write_str("null"),
        }
    }
}

#[cfg(test)]
mod http_fixture_progress_tests {
    use super::*;

    const STAGES: [Stage; 8] = [
        Stage::MainBodyEntered,
        Stage::S3CreateStarted,
        Stage::S3CreateCompleted,
        Stage::WebdavCreateStarted,
        Stage::WebdavCreateCompleted,
        Stage::WebdavListenStarted,
        Stage::WebdavListenCompleted,
        Stage::ReadyPublished,
    ];

    #[test]
    fn records_actual_application_body_and_each_setup_boundary() {
        let mut progress = Progress::new();
        let mut now = 0;
        let mut output = Vec::new();
        for stage in STAGES {
            progress.observe_with(stage, &mut || now, &mut |line| {
                output.extend_from_slice(line);
                Ok(line.len())
            });
            now += 5;
        }
        let output = std::str::from_utf8(&output).unwrap();
        let records: Vec<serde_json::Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let stages: Vec<_> = records
            .iter()
            .map(|record| record["stage"].as_str().unwrap())
            .collect();
        assert_eq!(
            stages,
            [
                "main_body_entered",
                "s3_create_started",
                "s3_create_completed",
                "webdav_create_started",
                "webdav_create_completed",
                "webdav_listen_started",
                "webdav_listen_completed",
                "ready_published",
            ]
        );
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record["schema"], "mount-rs.http-oracle-startup.v1");
            assert_eq!(record["scope"], "application_async_main_body");
            assert_eq!(record["compiler_observation"], "unobserved");
            assert_eq!(record["pid"], std::process::id());
            assert_eq!(record["sequence"], index + 1);
            assert_eq!(record["elapsed_ms"], index * 5);
            assert_eq!(record["availability"], "observed");
            assert!(record["reason"].is_null());
            assert_eq!(record.as_object().unwrap().len(), 9);
        }
    }

    #[test]
    fn duplicate_stages_read_no_extra_clocks_or_sinks() {
        let mut progress = Progress::new();
        let mut clocks = 0;
        let mut writes = 0;
        for _ in 0..1000 {
            for stage in STAGES {
                progress.observe_with(
                    stage,
                    &mut || {
                        clocks += 1;
                        0
                    },
                    &mut |line| {
                        writes += 1;
                        assert!(line.len() <= 512);
                        Ok(line.len())
                    },
                );
            }
        }
        assert_eq!(clocks, 8);
        assert_eq!(writes, 8);
        assert!(writes <= 16);
    }

    #[test]
    fn maximum_supported_elapsed_keeps_the_record_bounded() {
        let mut progress = Progress::new();
        let mut output = Vec::new();
        for stage in [Stage::MainBodyEntered, Stage::ReadyPublished] {
            progress.observe_with(stage, &mut || 9_007_199_254_740_991, &mut |line| {
                assert!(line.len() <= 512);
                assert_eq!(line.last(), Some(&b'\n'));
                output.push(serde_json::from_slice::<serde_json::Value>(line).unwrap());
                Ok(line.len())
            });
        }
        assert_eq!(output.len(), 2);
        assert_eq!(output[1]["elapsed_ms"], 9_007_199_254_740_991_u64);
    }

    #[test]
    fn invalid_clock_emits_one_redacted_unavailable_record_then_stops() {
        let mut progress = Progress::new();
        let mut output = Vec::new();
        let mut clocks = 0;
        for stage in STAGES {
            progress.observe_with(
                stage,
                &mut || {
                    clocks += 1;
                    u128::MAX
                },
                &mut |line| {
                    output.push(serde_json::from_slice::<serde_json::Value>(line).unwrap());
                    Ok(line.len())
                },
            );
        }
        assert_eq!(output.len(), 1);
        assert_eq!(clocks, 1);
        assert_eq!(output[0]["stage"], "observer_unavailable");
        assert_eq!(output[0]["reason"], "clock_invalid");
        assert_eq!(output[0]["availability"], "unavailable");
        assert!(output[0]["elapsed_ms"].is_null());
    }

    #[test]
    fn regressed_clock_retains_the_last_valid_elapsed_and_disables_updates() {
        let mut progress = Progress::new();
        let mut output = Vec::new();
        let mut now = 100;
        for stage in [
            Stage::MainBodyEntered,
            Stage::S3CreateStarted,
            Stage::ReadyPublished,
        ] {
            progress.observe_with(stage, &mut || now, &mut |line| {
                output.push(serde_json::from_slice::<serde_json::Value>(line).unwrap());
                Ok(line.len())
            });
            now -= 1;
        }
        assert_eq!(output.len(), 2);
        assert_eq!(output[1]["stage"], "observer_unavailable");
        assert_eq!(output[1]["reason"], "clock_regressed");
        assert_eq!(output[1]["elapsed_ms"], 100);
    }

    #[test]
    fn failed_sink_is_not_retried_and_future_updates_read_no_clock() {
        sink_failure_control(false);
    }

    #[test]
    fn short_sink_is_not_retried_and_future_updates_read_no_clock() {
        sink_failure_control(true);
    }

    fn sink_failure_control(short: bool) {
        let mut progress = Progress::new();
        let mut clocks = 0;
        let mut writes = 0;
        for stage in STAGES {
            progress.observe_with(
                stage,
                &mut || {
                    clocks += 1;
                    0
                },
                &mut |_| {
                    writes += 1;
                    if short {
                        Ok(0)
                    } else {
                        Err(io::Error::other("PRIVATE raw sink failure"))
                    }
                },
            );
        }
        assert_eq!(clocks, 1);
        assert_eq!(writes, 1);
    }
}
