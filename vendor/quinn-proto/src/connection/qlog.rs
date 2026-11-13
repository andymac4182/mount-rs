// Function bodies in this module are regularly cfg'd out
#![allow(unused_variables)]

#[cfg(feature = "qlog")]
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "qlog")]
use qlog::{
    events::{
        Event, EventData,
        quic::{
            PacketHeader, PacketLost, PacketLostTrigger, PacketReceived, PacketSent, PacketType,
        },
    },
    streamer::QlogStreamer,
};
#[cfg(feature = "qlog")]
use tracing::warn;

use crate::{
    ConnectionId, Instant,
    connection::{PathData, SentPacket},
    packet::SpaceId,
};

/// Shareable handle to a single qlog output stream
#[cfg(feature = "qlog")]
#[derive(Clone)]
pub struct QlogStream(pub(crate) Arc<crate::turn::Turn<QlogStreamer>>);

#[cfg(feature = "qlog")]
impl QlogStream {
    fn emit_event(&self, orig_rem_cid: ConnectionId, event: EventData, now: Instant) {
        // Time will be overwritten by `add_event_with_instant`
        let mut event = Event::with_time(0.0, event);
        event.group_id = Some(orig_rem_cid.to_string());

        let mut qlog_streamer = self.0.lock();
        if let Err(e) = qlog_streamer.add_event_with_instant(event, now) {
            warn!("could not emit qlog event: {e}");
        }
    }
}

/// A [`QlogStream`] that may be either dynamically disabled or compiled out entirely
#[derive(Clone, Default)]
pub(crate) struct QlogSink {
    #[cfg(feature = "qlog")]
    stream: Option<QlogStream>,
}

impl QlogSink {
    pub(crate) fn is_enabled(&self) -> bool {
        #[cfg(feature = "qlog")]
        {
            self.stream.is_some()
        }
        #[cfg(not(feature = "qlog"))]
        {
            false
        }
    }

    pub(super) fn emit_recovery_metrics(
        &self,
        pto_count: u32,
        path: &mut PathData,
        now: Instant,
        orig_rem_cid: ConnectionId,
    ) {
        #[cfg(feature = "qlog")]
        {
            let Some(stream) = self.stream.as_ref() else {
                return;
            };

            let Some(metrics) = path.qlog_recovery_metrics(pto_count) else {
                return;
            };

            stream.emit_event(orig_rem_cid, EventData::MetricsUpdated(metrics), now);
        }
    }

    pub(super) fn emit_packet_lost(
        &self,
        pn: u64,
        info: &SentPacket,
        loss_delay: Duration,
        space: SpaceId,
        now: Instant,
        orig_rem_cid: ConnectionId,
    ) {
        #[cfg(feature = "qlog")]
        {
            let Some(stream) = self.stream.as_ref() else {
                return;
            };

            let event = PacketLost {
                header: Some(PacketHeader {
                    packet_number: Some(pn),
                    packet_type: packet_type(space, false),
                    length: Some(info.size),
                    ..Default::default()
                }),
                frames: None,
                trigger: Some(
                    match info.time_sent.saturating_duration_since(now) >= loss_delay {
                        true => PacketLostTrigger::TimeThreshold,
                        false => PacketLostTrigger::ReorderingThreshold,
                    },
                ),
            };

            stream.emit_event(orig_rem_cid, EventData::PacketLost(event), now);
        }
    }

    pub(super) fn emit_packet_sent(
        &self,
        pn: u64,
        len: usize,
        space: SpaceId,
        is_0rtt: bool,
        now: Instant,
        orig_rem_cid: ConnectionId,
    ) {
        #[cfg(feature = "qlog")]
        {
            let Some(stream) = self.stream.as_ref() else {
                return;
            };

            let event = PacketSent {
                header: PacketHeader {
                    packet_number: Some(pn),
                    packet_type: packet_type(space, is_0rtt),
                    length: Some(len as u16),
                    ..Default::default()
                },
                ..Default::default()
            };

            stream.emit_event(orig_rem_cid, EventData::PacketSent(event), now);
        }
    }

    pub(super) fn emit_packet_received(
        &self,
        pn: u64,
        space: SpaceId,
        is_0rtt: bool,
        now: Instant,
        orig_rem_cid: ConnectionId,
    ) {
        #[cfg(feature = "qlog")]
        {
            let Some(stream) = self.stream.as_ref() else {
                return;
            };

            let event = PacketReceived {
                header: PacketHeader {
                    packet_number: Some(pn),
                    packet_type: packet_type(space, is_0rtt),
                    ..Default::default()
                },
                ..Default::default()
            };

            stream.emit_event(orig_rem_cid, EventData::PacketReceived(event), now);
        }
    }
}

#[cfg(feature = "qlog")]
impl From<Option<QlogStream>> for QlogSink {
    fn from(stream: Option<QlogStream>) -> Self {
        Self { stream }
    }
}

#[cfg(feature = "qlog")]
fn packet_type(space: SpaceId, is_0rtt: bool) -> PacketType {
    match space {
        SpaceId::Initial => PacketType::Initial,
        SpaceId::Handshake => PacketType::Handshake,
        SpaceId::Data if is_0rtt => PacketType::ZeroRtt,
        SpaceId::Data => PacketType::OneRtt,
    }
}

#[cfg(all(test, feature = "qlog"))]
mod tests {
    use std::{
        io,
        sync::{Arc, Barrier, Mutex},
    };

    use qlog::events::{
        EventData,
        quic::{PacketLost, PacketLostTrigger},
    };

    use crate::{ConnectionId, Instant, QlogConfig};

    /// A sink that keeps every byte, each write whole.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Connections sharing one stream emit at once. The streamer writes an
    /// event in many small writes, so every record must still come out
    /// whole: one separator, one JSON object, one newline.
    #[test]
    fn concurrent_events_reach_the_trace_whole() {
        const THREADS: usize = 4;
        const EVENTS: usize = 2_000;
        let sink = Sink::default();
        let mut config = QlogConfig::default();
        config.writer(Box::new(sink.clone()));
        let stream = config.into_stream().expect("qlog stream");
        let start = Barrier::new(THREADS);
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let (stream, start) = (&stream, &start);
                scope.spawn(move || {
                    let cid = ConnectionId::new(&[thread as u8; 8]);
                    start.wait();
                    for _ in 0..EVENTS {
                        let lost = PacketLost {
                            trigger: Some(PacketLostTrigger::TimeThreshold),
                            ..PacketLost::default()
                        };
                        stream.emit_event(cid, EventData::PacketLost(lost), Instant::now());
                    }
                });
            }
        });
        let bytes = sink.0.lock().unwrap().clone();
        let text = String::from_utf8(bytes).expect("trace is UTF-8");
        let records = text
            .split('\u{1e}')
            .filter(|record| !record.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(
            records.len(),
            1 + THREADS * EVENTS,
            "one header and every event"
        );
        for record in &records[1..] {
            assert!(
                record.starts_with('{')
                    && record.ends_with("}\n")
                    && record.matches('\n').count() == 1,
                "a record came out interleaved: {record:?}"
            );
            assert!(
                record.contains("transport:packet_lost") || record.contains("recovery:packet_lost")
            );
        }
    }
}
