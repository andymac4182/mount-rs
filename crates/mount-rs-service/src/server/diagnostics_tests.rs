use super::*;

#[test]
fn entering_mutation_between_end_envelope_reads_cannot_claim_quiescence() {
    let observer = ServerDiagnostics::new(1, false);
    let pending = std::cell::RefCell::new(None);
    let snapshot = observer.snapshot_with_end_hook(|| {
        let mutation = Mutation::new(&observer.inner);
        let metric = &observer.inner.metrics[Operation::Request as usize];
        observer.inner.add(&metric.calls, 1);
        observer.inner.add(&metric.in_flight, 1);
        *pending.borrow_mut() = Some(mutation);
    });
    let live_requests = observer.inner.metrics[Operation::Request as usize]
        .in_flight
        .load(Ordering::SeqCst);
    drop(pending.into_inner());
    assert_eq!(live_requests, 1);
    assert!(
        !snapshot.complete,
        "concurrent mutation was omitted from capture envelope"
    );
    assert!(!snapshot.application_quiescent);
}

#[test]
fn spans_hold_activity_until_terminal_and_drop_records_cancellation() {
    let observer = ServerDiagnostics::new(2, false);
    let baseline = observer.snapshot();
    assert!(baseline.application_quiescent);
    let mut handshake = Span::new(Some(&observer), Operation::Handshake);
    let request = Span::new(Some(&observer), Operation::Request);
    let held = observer.snapshot();
    assert_eq!(held.active_handshakes_after, 1);
    assert_eq!(held.active_requests_after, 1);
    assert!(!held.application_quiescent);
    handshake.finish(Outcome::Error);
    drop(request);
    let after = observer.snapshot();
    assert!(after.complete && after.application_quiescent);
    assert_eq!(after.active_handshakes_after, 0);
    assert_eq!(after.active_requests_after, 0);
    let request = &after.entries[Operation::Request as usize];
    assert_eq!(
        (request.calls, request.cancelled, request.in_flight),
        (1, 1, 0)
    );
    assert_eq!(request.latency_log2_us.iter().sum::<u64>(), 1);
    assert_eq!(after.entries[Operation::Handshake as usize].error, 1);
    assert!(after.activity_sequence_after > baseline.activity_sequence_after);
}

#[test]
fn saturation_and_mutating_capture_cannot_claim_complete_quiescence() {
    let observer = ServerDiagnostics::new(1, false);
    let mutation = Mutation::new(&observer.inner);
    let during = observer.snapshot();
    assert!(!during.complete && !during.application_quiescent);
    drop(mutation);
    observer.inner.metrics[Operation::Request as usize]
        .calls
        .store(u64::MAX, Ordering::Relaxed);
    let mut span = Span::new(Some(&observer), Operation::Request);
    span.finish(Outcome::Success);
    let after = observer.snapshot();
    assert!(after.counter_saturated);
    assert!(!after.complete && !after.application_quiescent);
    assert_eq!(after.entries[Operation::Request as usize].calls, u64::MAX);
}

#[test]
fn slow_records_are_bounded_and_have_only_fixed_labels() {
    let observer = ServerDiagnostics::new(1, false);
    let mut output = Vec::new();
    for _ in 0..30 {
        write_slow_record(
            &mut output,
            &observer.inner,
            Operation::DispatchRead,
            Outcome::Timeout,
            200_000_000,
        )
        .unwrap();
    }
    let output = String::from_utf8(output).unwrap();
    assert_eq!(output.lines().count(), 16);
    assert!(output.lines().all(|line| line
        == "MOUNT_RS_SERVICE_SLOW operation=dispatch.read outcome=timeout elapsed_us=200000"));
}

#[test]
fn transport_fold_saturates_instead_of_wrapping() {
    let mut target = TransportCounters {
        udp_tx_bytes: u64::MAX - 2,
        ..Default::default()
    };
    let add = TransportCounters {
        udp_tx_bytes: 3,
        frame_rx: FrameCounts {
            stream: 7,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(target.add(add));
    assert_eq!(target.udp_tx_bytes, u64::MAX);
    assert_eq!(target.frame_rx.stream, 7);
}
