//! Opt-in local observations of application tasks and accepted Quinn connections.
//! Inclusive spans overlap. UDP observations exclude network headers and traffic
//! without an accepted connection, or after its session task retires.

use serde::Serialize;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const BUCKETS: usize = 32;
const SLOW_NS: u64 = 100_000_000;
const SLOW_RECORDS: u64 = 16;

#[derive(Clone, Copy)]
#[repr(usize)]
pub(super) enum Operation {
    ConnectionAdmission,
    Handshake,
    TlsHandshake,
    Authentication,
    Request,
    IncomingRead,
    IngressAdmission,
    EgressAdmission,
    DispatchControl,
    DispatchRead,
    DispatchWrite,
    ResponseEncode,
    ResponseSubmit,
    SessionCleanup,
}
const NAMES: [&str; 14] = [
    "admission.connection",
    "handshake.application",
    "handshake.tls",
    "auth.authenticate",
    "request.application",
    "request.read_incoming",
    "admission.ingress",
    "admission.egress",
    "dispatch.control",
    "dispatch.read",
    "dispatch.write",
    "response.encode",
    "response.submit",
    "session.cleanup",
];

#[derive(Clone, Copy)]
pub(super) enum Outcome {
    Success,
    Error,
    Timeout,
    Cancelled,
}
impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }
    pub(super) fn result<T, E>(result: &Result<T, E>) -> Self {
        if result.is_ok() {
            Self::Success
        } else {
            Self::Error
        }
    }
}

struct Metric {
    calls: AtomicU64,
    success: AtomicU64,
    error: AtomicU64,
    timeout: AtomicU64,
    cancelled: AtomicU64,
    in_flight: AtomicU64,
    elapsed_ns: AtomicU64,
    max_elapsed_ns: AtomicU64,
    latency_log2_us: [AtomicU64; BUCKETS],
}
impl Metric {
    fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            success: AtomicU64::new(0),
            error: AtomicU64::new(0),
            timeout: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            elapsed_ns: AtomicU64::new(0),
            max_elapsed_ns: AtomicU64::new(0),
            latency_log2_us: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
    fn snapshot(&self, name: &'static str) -> ServiceEntry {
        ServiceEntry {
            name,
            calls: self.calls.load(Ordering::SeqCst),
            success: self.success.load(Ordering::SeqCst),
            error: self.error.load(Ordering::SeqCst),
            timeout: self.timeout.load(Ordering::SeqCst),
            cancelled: self.cancelled.load(Ordering::SeqCst),
            in_flight: self.in_flight.load(Ordering::SeqCst),
            elapsed_ns: self.elapsed_ns.load(Ordering::SeqCst),
            max_elapsed_ns: self.max_elapsed_ns.load(Ordering::SeqCst),
            latency_log2_us: std::array::from_fn(|index| {
                self.latency_log2_us[index].load(Ordering::SeqCst)
            }),
        }
    }
}

struct RegisteredConnection {
    id: u64,
    connection: quinn::Connection,
}
struct Registry {
    active: Vec<RegisteredConnection>,
    capacity: usize,
    next_id: u64,
    registered: u64,
    retired: u64,
    unobserved: u64,
    missing_final_samples: u64,
    retired_counters: TransportCounters,
}
struct Recorder {
    metrics: [Metric; NAMES.len()],
    activity_writers: AtomicU64,
    activity_sequence: AtomicU64,
    saturated: AtomicBool,
    registry_incomplete: AtomicBool,
    registry: Mutex<Registry>,
    trace: bool,
    slow_records: AtomicU64,
}
impl Recorder {
    fn add(&self, counter: &AtomicU64, value: u64) {
        let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
            match old.checked_add(value) {
                Some(next) => Some(next),
                None => {
                    self.saturated.store(true, Ordering::SeqCst);
                    Some(u64::MAX)
                }
            }
        });
    }
    fn subtract(&self, counter: &AtomicU64) {
        let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
            match old.checked_sub(1) {
                Some(next) => Some(next),
                None => {
                    self.saturated.store(true, Ordering::SeqCst);
                    Some(0)
                }
            }
        });
    }
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(|error| {
            self.registry_incomplete.store(true, Ordering::SeqCst);
            error.into_inner()
        })
    }
}

/// Cloneable local observer; snapshots may be taken after the server closes.
/// This does not expose a network endpoint or retain retired connections.
#[derive(Clone)]
pub struct ServerDiagnostics {
    inner: Arc<Recorder>,
}
impl ServerDiagnostics {
    pub(super) fn new(capacity: usize, trace: bool) -> Self {
        Self {
            inner: Arc::new(Recorder {
                metrics: std::array::from_fn(|_| Metric::new()),
                activity_writers: AtomicU64::new(0),
                activity_sequence: AtomicU64::new(0),
                saturated: AtomicBool::new(false),
                registry_incomplete: AtomicBool::new(false),
                registry: Mutex::new(Registry {
                    active: Vec::with_capacity(capacity),
                    capacity,
                    next_id: 1,
                    registered: 0,
                    retired: 0,
                    unobserved: 0,
                    missing_final_samples: 0,
                    retired_counters: TransportCounters::default(),
                }),
                trace,
                slow_records: AtomicU64::new(0),
            }),
        }
    }

    /// Serial, process-local observations. `complete` requires unchanged
    /// application activity while reading; passive QUIC traffic can continue.
    /// Gauge zero without the sequence/writer envelope is not quiescence proof.
    #[must_use]
    pub fn snapshot(&self) -> ServerSnapshot {
        self.snapshot_with_end_hook(|| {})
    }

    fn snapshot_with_end_hook(&self, after_writers: impl FnOnce()) -> ServerSnapshot {
        let started = Instant::now();
        let capture_started_unix_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok());
        let state = &self.inner;
        let activity_sequence_before = state.activity_sequence.load(Ordering::SeqCst);
        let activity_writers_before = state.activity_writers.load(Ordering::SeqCst);
        let active_handshakes_before = state.metrics[Operation::Handshake as usize]
            .in_flight
            .load(Ordering::SeqCst);
        let active_requests_before = state.metrics[Operation::Request as usize]
            .in_flight
            .load(Ordering::SeqCst);
        let entries: [ServiceEntry; NAMES.len()] =
            std::array::from_fn(|index| state.metrics[index].snapshot(NAMES[index]));
        let registry_started = Instant::now();
        let transport = {
            let registry = state.registry();
            let active: Vec<_> = registry
                .active
                .iter()
                .map(|entry| {
                    let stats = entry.connection.stats();
                    ConnectionObservation {
                        id: entry.id,
                        counters: TransportCounters::capture(&stats),
                        gauges: PathGauges {
                            rtt_ns: duration_ns(stats.path.rtt, state),
                            min_rtt_ns: duration_ns(stats.path.min_rtt, state),
                            congestion_window_bytes: stats.path.cwnd,
                            current_mtu_bytes: stats.path.current_mtu,
                        },
                    }
                })
                .collect();
            let mut observed_total = registry.retired_counters;
            for entry in &active {
                if observed_total.add(entry.counters) {
                    state.saturated.store(true, Ordering::SeqCst);
                }
            }
            TransportSnapshot {
                scope: "accepted_connection_until_session_retirement",
                udp_bytes: "payload_inside_udp_datagrams_excludes_network_headers",
                frame_counts: "quinn_reported_frame_counts_excludes_padding",
                application_wire_bytes: "unavailable",
                upstream_counter_overflow_detection: "unavailable",
                registry_capacity: registry.capacity,
                registered_connections: registry.registered,
                retired_connections: registry.retired,
                unobserved_connections: registry.unobserved,
                missing_final_samples: registry.missing_final_samples,
                registry_complete: !state.registry_incomplete.load(Ordering::SeqCst),
                active,
                retired: registry.retired_counters,
                observed_total,
            }
        };
        let registry_snapshot_elapsed_ns = duration_ns(registry_started.elapsed(), state);
        let active_handshakes_after = state.metrics[Operation::Handshake as usize]
            .in_flight
            .load(Ordering::SeqCst);
        let active_requests_after = state.metrics[Operation::Request as usize]
            .in_flight
            .load(Ordering::SeqCst);
        let activity_writers_after = state.activity_writers.load(Ordering::SeqCst);
        after_writers();
        let activity_sequence_after = state.activity_sequence.load(Ordering::SeqCst);
        let active_operations = entries.iter().fold(0_u64, |sum, entry| {
            saturating_add(sum, entry.in_flight, &state.saturated)
        });
        let capture_elapsed_ns = duration_ns(started.elapsed(), state);
        let counter_saturated = state.saturated.load(Ordering::SeqCst);
        let concurrent_activity = activity_sequence_before != activity_sequence_after
            || activity_writers_before != 0
            || activity_writers_after != 0;
        let complete = !concurrent_activity && !counter_saturated && transport.registry_complete;
        ServerSnapshot {
            schema: "mount-rs.service-quic.v1",
            enabled: true,
            capture_started_unix_ns,
            capture_elapsed_ns,
            registry_snapshot_elapsed_ns,
            activity_sequence_before,
            activity_sequence_after,
            activity_writers_before,
            activity_writers_after,
            active_handshakes_before,
            active_handshakes_after,
            active_requests_before,
            active_requests_after,
            active_operations,
            complete,
            counter_saturated,
            concurrent_activity,
            application_quiescent: complete
                && active_operations == 0
                && active_handshakes_before == 0
                && active_handshakes_after == 0
                && active_requests_before == 0
                && active_requests_after == 0,
            quiescence_scope: "application_spans_and_session_cleanup_not_passive_quic",
            dispatch_scope: "inclusive_authorization_catalog_and_filesystem_dispatch",
            response_scope: "encoding_and_quinn_submission_not_peer_acknowledgement",
            histogram_scope: "inclusive_wall_time_log2_microseconds_32_buckets",
            cumulative_maxima: "not_subtractable_as_phase_maxima",
            entries,
            transport,
        }
    }

    pub(super) fn register(&self, connection: &quinn::Connection) -> ConnectionGuard {
        let state = &self.inner;
        let _mutation = Mutation::new(state);
        let mut registry = state.registry();
        let id = if registry.active.len() >= registry.capacity || registry.next_id == u64::MAX {
            state.registry_incomplete.store(true, Ordering::SeqCst);
            if registry.next_id == u64::MAX {
                state.saturated.store(true, Ordering::SeqCst);
            }
            registry.unobserved = saturating_add(registry.unobserved, 1, &state.saturated);
            None
        } else {
            let id = registry.next_id;
            registry.next_id += 1;
            registry.registered = saturating_add(registry.registered, 1, &state.saturated);
            registry.active.push(RegisteredConnection {
                id,
                connection: connection.clone(),
            });
            Some(id)
        };
        ConnectionGuard {
            observer: self.clone(),
            id,
        }
    }
}

/// Fixed-label inclusive service operation observation.
#[derive(Clone, Debug, Serialize)]
pub struct ServiceEntry {
    pub name: &'static str,
    pub calls: u64,
    pub success: u64,
    pub error: u64,
    pub timeout: u64,
    pub cancelled: u64,
    pub in_flight: u64,
    pub elapsed_ns: u64,
    pub max_elapsed_ns: u64,
    pub latency_log2_us: [u64; BUCKETS],
}

#[derive(Clone, Debug, Serialize)]
pub struct ServerSnapshot {
    pub schema: &'static str,
    pub enabled: bool,
    pub capture_started_unix_ns: Option<u64>,
    pub capture_elapsed_ns: u64,
    pub registry_snapshot_elapsed_ns: u64,
    pub activity_sequence_before: u64,
    pub activity_sequence_after: u64,
    pub activity_writers_before: u64,
    pub activity_writers_after: u64,
    pub active_handshakes_before: u64,
    pub active_handshakes_after: u64,
    pub active_requests_before: u64,
    pub active_requests_after: u64,
    /// Nested spans, not a cardinality of distinct application requests.
    pub active_operations: u64,
    pub complete: bool,
    pub counter_saturated: bool,
    pub concurrent_activity: bool,
    pub application_quiescent: bool,
    pub quiescence_scope: &'static str,
    pub dispatch_scope: &'static str,
    pub response_scope: &'static str,
    pub histogram_scope: &'static str,
    pub cumulative_maxima: &'static str,
    pub entries: [ServiceEntry; NAMES.len()],
    pub transport: TransportSnapshot,
}

struct Mutation<'a> {
    recorder: &'a Recorder,
}
impl<'a> Mutation<'a> {
    fn new(recorder: &'a Recorder) -> Self {
        recorder.add(&recorder.activity_writers, 1);
        // Announce entry before changing rows/gauges. A capture may already
        // have read its final writer count, so exit-only generations miss a
        // writer paused between that count and the final sequence read.
        recorder.add(&recorder.activity_sequence, 1);
        Self { recorder }
    }
}
impl Drop for Mutation<'_> {
    fn drop(&mut self) {
        self.recorder.add(&self.recorder.activity_sequence, 1);
        self.recorder.subtract(&self.recorder.activity_writers);
    }
}

pub(super) struct Span {
    recorder: Option<Arc<Recorder>>,
    operation: Operation,
    started: Option<Instant>,
    finished: bool,
}
impl Span {
    pub(super) fn new(observer: Option<&ServerDiagnostics>, operation: Operation) -> Self {
        let recorder = observer.map(|observer| Arc::clone(&observer.inner));
        let started = recorder.as_ref().map(|recorder| {
            let _mutation = Mutation::new(recorder);
            recorder.add(&recorder.metrics[operation as usize].calls, 1);
            recorder.add(&recorder.metrics[operation as usize].in_flight, 1);
            Instant::now()
        });
        Self {
            recorder,
            operation,
            started,
            finished: false,
        }
    }
    pub(super) fn finish(&mut self, outcome: Outcome) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let (Some(recorder), Some(started)) = (&self.recorder, self.started) {
            let _mutation = Mutation::new(recorder);
            let elapsed = duration_ns(started.elapsed(), recorder);
            let metric = &recorder.metrics[self.operation as usize];
            recorder.add(
                match outcome {
                    Outcome::Success => &metric.success,
                    Outcome::Error => &metric.error,
                    Outcome::Timeout => &metric.timeout,
                    Outcome::Cancelled => &metric.cancelled,
                },
                1,
            );
            recorder.add(&metric.elapsed_ns, elapsed);
            metric.max_elapsed_ns.fetch_max(elapsed, Ordering::SeqCst);
            let micros = elapsed / 1_000;
            let bucket = if micros == 0 {
                0
            } else {
                (u64::BITS - micros.leading_zeros()) as usize
            }
            .min(BUCKETS - 1);
            recorder.add(&metric.latency_log2_us[bucket], 1);
            if recorder.trace && elapsed >= SLOW_NS {
                let _ = write_slow_record(
                    &mut io::stderr().lock(),
                    recorder,
                    self.operation,
                    outcome,
                    elapsed,
                );
            }
            // Publish terminal counters before zeroing the active gauge.
            recorder.subtract(&metric.in_flight);
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        self.finish(Outcome::Cancelled);
    }
}

pub(super) struct ConnectionGuard {
    observer: ServerDiagnostics,
    id: Option<u64>,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let Some(id) = self.id else {
            return;
        };
        let state = &self.observer.inner;
        let _mutation = Mutation::new(state);
        let mut registry = state.registry();
        let Some(index) = registry.active.iter().position(|entry| entry.id == id) else {
            state.registry_incomplete.store(true, Ordering::SeqCst);
            registry.missing_final_samples =
                saturating_add(registry.missing_final_samples, 1, &state.saturated);
            return;
        };
        let entry = registry.active.swap_remove(index);
        if registry
            .retired_counters
            .add(TransportCounters::capture(&entry.connection.stats()))
        {
            state.saturated.store(true, Ordering::SeqCst);
        }
        registry.retired = saturating_add(registry.retired, 1, &state.saturated);
    }
}

fn duration_ns(duration: std::time::Duration, state: &Recorder) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or_else(|_| {
        state.saturated.store(true, Ordering::SeqCst);
        u64::MAX
    })
}
fn saturating_add(left: u64, right: u64, saturated: &AtomicBool) -> u64 {
    left.checked_add(right).unwrap_or_else(|| {
        saturated.store(true, Ordering::SeqCst);
        u64::MAX
    })
}
fn write_slow_record(
    output: &mut impl Write,
    state: &Recorder,
    operation: Operation,
    outcome: Outcome,
    elapsed_ns: u64,
) -> io::Result<()> {
    if state
        .slow_records
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            if old < SLOW_RECORDS {
                Some(old + 1)
            } else {
                None
            }
        })
        .is_ok()
    {
        writeln!(
            output,
            "MOUNT_RS_SERVICE_SLOW operation={} outcome={} elapsed_us={}",
            NAMES[operation as usize],
            outcome.name(),
            elapsed_ns / 1_000,
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct FrameCounts {
    pub acks: u64,
    pub ack_frequency: u64,
    pub crypto: u64,
    pub connection_close: u64,
    pub data_blocked: u64,
    pub datagram: u64,
    pub handshake_done: u64,
    pub immediate_ack: u64,
    pub max_data: u64,
    pub max_stream_data: u64,
    pub max_streams_bidi: u64,
    pub max_streams_uni: u64,
    pub new_connection_id: u64,
    pub new_token: u64,
    pub path_challenge: u64,
    pub path_response: u64,
    pub ping: u64,
    pub reset_stream: u64,
    pub retire_connection_id: u64,
    pub stream_data_blocked: u64,
    pub streams_blocked_bidi: u64,
    pub streams_blocked_uni: u64,
    pub stop_sending: u64,
    pub stream: u64,
}
impl FrameCounts {
    fn capture(stats: &quinn::FrameStats) -> Self {
        Self {
            acks: stats.acks,
            ack_frequency: stats.ack_frequency,
            crypto: stats.crypto,
            connection_close: stats.connection_close,
            data_blocked: stats.data_blocked,
            datagram: stats.datagram,
            handshake_done: u64::from(stats.handshake_done),
            immediate_ack: stats.immediate_ack,
            max_data: stats.max_data,
            max_stream_data: stats.max_stream_data,
            max_streams_bidi: stats.max_streams_bidi,
            max_streams_uni: stats.max_streams_uni,
            new_connection_id: stats.new_connection_id,
            new_token: stats.new_token,
            path_challenge: stats.path_challenge,
            path_response: stats.path_response,
            ping: stats.ping,
            reset_stream: stats.reset_stream,
            retire_connection_id: stats.retire_connection_id,
            stream_data_blocked: stats.stream_data_blocked,
            streams_blocked_bidi: stats.streams_blocked_bidi,
            streams_blocked_uni: stats.streams_blocked_uni,
            stop_sending: stats.stop_sending,
            stream: stats.stream,
        }
    }
    fn add(&mut self, other: Self) -> bool {
        let mut saturated = false;
        macro_rules! add {
            ($($field:ident),*) => {$(
                self.$field = self.$field.checked_add(other.$field).unwrap_or_else(|| {
                    saturated = true;
                    u64::MAX
                });
            )*};
        }
        add!(
            acks,
            ack_frequency,
            crypto,
            connection_close,
            data_blocked,
            datagram,
            handshake_done,
            immediate_ack,
            max_data,
            max_stream_data,
            max_streams_bidi,
            max_streams_uni,
            new_connection_id,
            new_token,
            path_challenge,
            path_response,
            ping,
            reset_stream,
            retire_connection_id,
            stream_data_blocked,
            streams_blocked_bidi,
            streams_blocked_uni,
            stop_sending,
            stream
        );
        saturated
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct TransportCounters {
    pub udp_tx_bytes: u64,
    pub udp_rx_bytes: u64,
    pub udp_tx_datagrams: u64,
    pub udp_rx_datagrams: u64,
    pub udp_tx_ios: u64,
    pub udp_rx_ios: u64,
    pub frame_tx: FrameCounts,
    pub frame_rx: FrameCounts,
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub congestion_events: u64,
    pub sent_plpmtud_probes: u64,
    pub lost_plpmtud_probes: u64,
    pub black_holes_detected: u64,
}
impl TransportCounters {
    fn capture(stats: &quinn::ConnectionStats) -> Self {
        Self {
            udp_tx_bytes: stats.udp_tx.bytes,
            udp_rx_bytes: stats.udp_rx.bytes,
            udp_tx_datagrams: stats.udp_tx.datagrams,
            udp_rx_datagrams: stats.udp_rx.datagrams,
            udp_tx_ios: stats.udp_tx.ios,
            udp_rx_ios: stats.udp_rx.ios,
            frame_tx: FrameCounts::capture(&stats.frame_tx),
            frame_rx: FrameCounts::capture(&stats.frame_rx),
            sent_packets: stats.path.sent_packets,
            lost_packets: stats.path.lost_packets,
            lost_bytes: stats.path.lost_bytes,
            congestion_events: stats.path.congestion_events,
            sent_plpmtud_probes: stats.path.sent_plpmtud_probes,
            lost_plpmtud_probes: stats.path.lost_plpmtud_probes,
            black_holes_detected: stats.path.black_holes_detected,
        }
    }
    fn add(&mut self, other: Self) -> bool {
        let mut saturated = false;
        macro_rules! add {
            ($($field:ident),*) => {$(
                self.$field = self.$field.checked_add(other.$field).unwrap_or_else(|| {
                    saturated = true;
                    u64::MAX
                });
            )*};
        }
        add!(
            udp_tx_bytes,
            udp_rx_bytes,
            udp_tx_datagrams,
            udp_rx_datagrams,
            udp_tx_ios,
            udp_rx_ios,
            sent_packets,
            lost_packets,
            lost_bytes,
            congestion_events,
            sent_plpmtud_probes,
            lost_plpmtud_probes,
            black_holes_detected
        );
        saturated |= self.frame_tx.add(other.frame_tx);
        saturated |= self.frame_rx.add(other.frame_rx);
        saturated
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PathGauges {
    pub rtt_ns: u64,
    pub min_rtt_ns: u64,
    pub congestion_window_bytes: u64,
    pub current_mtu_bytes: u16,
}
#[derive(Clone, Debug, Serialize)]
pub struct ConnectionObservation {
    pub id: u64,
    pub counters: TransportCounters,
    pub gauges: PathGauges,
}
#[derive(Clone, Debug, Serialize)]
pub struct TransportSnapshot {
    pub scope: &'static str,
    pub udp_bytes: &'static str,
    pub frame_counts: &'static str,
    pub application_wire_bytes: &'static str,
    pub upstream_counter_overflow_detection: &'static str,
    pub registry_capacity: usize,
    pub registered_connections: u64,
    pub retired_connections: u64,
    pub unobserved_connections: u64,
    pub missing_final_samples: u64,
    pub registry_complete: bool,
    pub active: Vec<ConnectionObservation>,
    pub retired: TransportCounters,
    pub observed_total: TransportCounters,
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
