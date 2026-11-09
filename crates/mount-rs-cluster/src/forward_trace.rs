//! Opt-in per-request timings for correlating tails in diagnostic harnesses.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::time::Instant;

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    Route,
    Connection,
    Stream,
    Headers,
    Body,
    Total,
    RouteWait,
    RegistryLookup,
    OwnerAdmission,
    OwnerApplication,
    VolumePending,
    VolumeNotified,
    HeaderPending,
    HeaderNotified,
    OwnerStreamQueued,
    OwnerStreamResolve,
    VerifyEnd,
    UploadJoin,
    PeerConnectionId,
    PeerStreamId,
    LookupMapAdmission,
    VolumeAdmission,
    FirstPayloadReceived,
    FirstPayloadYielded,
}

#[derive(Debug)]
struct Inner {
    stages: [AtomicU64; Stage::FirstPayloadYielded as usize + 1],
    complete: AtomicBool,
}

/// Attach a fresh instance to an in-process request's extensions before forwarding.
/// Clones observe that one request; do not reuse across requests. No headers,
/// volume identifiers, credentials or payloads are collected or exposed over HTTP.
/// Requests without this extension allocate no trace and update no trace atomics.
#[derive(Clone, Debug)]
pub struct ForwardRequestTrace(Arc<Inner>);

impl Default for ForwardRequestTrace {
    fn default() -> Self {
        Self(Arc::new(Inner {
            stages: std::array::from_fn(|_| AtomicU64::new(u64::MAX)),
            complete: AtomicBool::new(false),
        }))
    }
}

/// Nanoseconds for one request, not aggregate percentiles. A false `complete`
/// means in-flight, cancelled or failed; missing stages must not count as zero.
/// Body ends at declared final payload (or EOF for unknown length). Total ends
/// at verified EOF/upload completion, before downstream network delivery.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct ForwardRequestTiming {
    pub complete: bool,
    pub peer_connection_id: Option<u64>,
    pub peer_stream_id: Option<u64>,
    pub volume_lock_pending_ns: Option<u64>,
    pub volume_lock_notified_ns: Option<u64>,
    pub route_ns: Option<u64>,
    /// Substage of route: registry/per-volume admission waiting, excluding Redis.
    /// None for an immediate snapshot-cache hit; do not add it to route_ns.
    pub route_lookup_wait_ns: Option<u64>,
    /// Entering the shared lookup map, pruning weak entries and obtaining the volume gate.
    pub lookup_map_admission_ns: Option<u64>,
    /// Elapsed time acquiring the per-volume gate, including any pending intervals.
    pub volume_admission_ns: Option<u64>,
    /// Time awaiting registry lookup on a cache miss, excluding admission.
    pub registry_lookup_ns: Option<u64>,
    pub connection_ns: Option<u64>,
    pub stream_open_ns: Option<u64>,
    pub headers_ns: Option<u64>,
    /// Elapsed gaps between successive polls for peer response headers.
    pub header_pending_ns: Option<u64>,
    /// Portion of those gaps after the first observed wake.
    pub header_notified_ns: Option<u64>,
    /// Owner transport task spawn to first poll, when peer-stream-timing is enabled.
    pub owner_stream_queued_ns: Option<u64>,
    /// Owner-local time inside the resolving future, including its pending intervals.
    pub owner_stream_resolve_ns: Option<u64>,
    /// Peer headers to the first nonempty chunk received by the ingress body reader.
    pub first_payload_received_ns: Option<u64>,
    /// Peer headers to yielding the first nonempty frame to the downstream HTTP writer.
    /// Includes final-frame verification when the first chunk completes the declared body.
    pub first_payload_yielded_ns: Option<u64>,
    pub body_ns: Option<u64>,
    pub owner_admission_ns: Option<u64>,
    pub owner_application_ns: Option<u64>,
    /// Final declared payload (or zero-length headers) to confirmed peer EOF.
    pub verify_end_ns: Option<u64>,
    /// Time awaiting the upload task after peer EOF on successful responses.
    pub upload_join_ns: Option<u64>,
    pub total_ns: Option<u64>,
}

impl ForwardRequestTrace {
    pub(crate) fn record(&self, stage: Stage, start: Instant) {
        let nanos = u64::try_from(start.elapsed().as_nanos())
            .unwrap_or(u64::MAX - 1)
            .min(u64::MAX - 1);
        self.0.stages[stage as usize].store(nanos, Ordering::Relaxed);
    }
    pub(crate) fn interval(&self, stage: Stage, nanos: u64) {
        self.0.stages[stage as usize].store(nanos, Ordering::Relaxed);
    }
    pub(crate) fn finish(&self, start: Instant) {
        self.record(Stage::Total, start);
        self.0.complete.store(true, Ordering::Release);
    }
    /// A completed snapshot publishes all preceding stage updates. An incomplete
    /// snapshot is partial and may race subsequent updates; sample again after drain.
    pub fn snapshot(&self) -> ForwardRequestTiming {
        let complete = self.0.complete.load(Ordering::Acquire);
        let values = self.0.stages.each_ref().map(|value| {
            let value = value.load(Ordering::Relaxed);
            (value != u64::MAX).then_some(value)
        });
        ForwardRequestTiming {
            complete,
            peer_connection_id: values[Stage::PeerConnectionId as usize],
            peer_stream_id: values[Stage::PeerStreamId as usize],
            owner_admission_ns: values[Stage::OwnerAdmission as usize],
            owner_application_ns: values[Stage::OwnerApplication as usize],
            volume_lock_pending_ns: values[Stage::VolumePending as usize],
            volume_lock_notified_ns: values[Stage::VolumeNotified as usize],
            route_ns: values[0],
            route_lookup_wait_ns: values[6],
            lookup_map_admission_ns: values[Stage::LookupMapAdmission as usize],
            volume_admission_ns: values[Stage::VolumeAdmission as usize],
            registry_lookup_ns: values[Stage::RegistryLookup as usize],
            connection_ns: values[1],
            stream_open_ns: values[2],
            headers_ns: values[3],
            header_pending_ns: values[Stage::HeaderPending as usize],
            header_notified_ns: values[Stage::HeaderNotified as usize],
            owner_stream_queued_ns: values[Stage::OwnerStreamQueued as usize],
            owner_stream_resolve_ns: values[Stage::OwnerStreamResolve as usize],
            first_payload_received_ns: values[Stage::FirstPayloadReceived as usize],
            first_payload_yielded_ns: values[Stage::FirstPayloadYielded as usize],
            body_ns: values[4],
            verify_end_ns: values[Stage::VerifyEnd as usize],
            upload_join_ns: values[Stage::UploadJoin as usize],
            total_ns: values[5],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_stages_remain_distinct_from_completed_measurements() {
        let trace = ForwardRequestTrace::default();
        assert!(!trace.snapshot().complete);
        assert_eq!(trace.snapshot().route_ns, None);
        let observer = trace.clone();
        trace.record(Stage::Route, Instant::now());
        assert!(observer.snapshot().route_ns.is_some());
        assert_eq!(observer.snapshot().total_ns, None);
        assert!(!observer.snapshot().complete);
        trace.finish(Instant::now());
        assert!(observer.snapshot().complete);
        assert!(observer.snapshot().total_ns.is_some());
        assert_eq!(observer.snapshot().body_ns, None);
    }
}
