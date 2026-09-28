use super::*;
use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

fn row(observer: &Observer) -> HttpSnapshot {
    observer.snapshot().unwrap().clients[ClientRole::PrimaryDataMixed.index()].http
        [HttpMethod::Get.index()]
}

#[test]
fn headers_and_body_completion_are_separate_observations() {
    let observer = Observer::isolated();
    let client = observer.client(ClientRole::PrimaryDataMixed);
    observer.known_extra_future_box(ClientRole::PrimaryDataMixed, HttpMethod::Get);
    // An unpolled wrapper has a construction site, but no dispatched attempt.
    assert_eq!(
        (
            row(&observer).known_extra_future_boxes,
            row(&observer).attempts_started
        ),
        (1, 0)
    );
    let attempt = client.attempt(HttpMethod::Get, None);
    assert_eq!(row(&observer).attempts_inflight, 1);
    let mut body = attempt.headers(503);
    observer.known_extra_response_body_box(ClientRole::PrimaryDataMixed, HttpMethod::Get);
    body.data(3);
    let pending = row(&observer);
    assert_eq!(
        (
            pending.header_responses,
            pending.status[4],
            pending.body_eof,
            pending.bodies_inflight
        ),
        (1, 1, 0, 1)
    );
    body.eof();
    let complete = row(&observer);
    assert_eq!(
        (
            complete.attempts_inflight,
            complete.bodies_inflight,
            complete.body_eof,
            complete.body_bytes,
            complete.body_chunks
        ),
        (0, 0, 1, 3, 1)
    );
    assert_eq!(
        (
            complete.offered_unknown,
            complete.known_extra_response_body_boxes
        ),
        (1, 1)
    );
}

#[test]
fn dispatch_errors_pre_header_cancellation_and_body_drop_are_distinct() {
    let observer = Observer::isolated();
    let client = observer.client(ClientRole::PrimaryDataMixed);
    client.attempt(HttpMethod::Get, Some(7)).transport_error();
    drop(client.attempt(HttpMethod::Get, Some(2)));
    drop(client.attempt(HttpMethod::Get, None).headers(200));
    client.attempt(HttpMethod::Get, None).headers(200).error();
    let observed = row(&observer);
    assert_eq!(
        (
            observed.attempts_started,
            observed.transport_errors,
            observed.cancelled_before_headers
        ),
        (4, 1, 1)
    );
    assert_eq!(
        (
            observed.body_dropped,
            observed.body_errors,
            observed.body_eof
        ),
        (1, 1, 0)
    );
    assert_eq!(
        (
            observed.offered_known,
            observed.offered_bytes,
            observed.offered_unknown
        ),
        (2, 9, 2)
    );
    assert_eq!(
        (observed.attempts_inflight, observed.bodies_inflight),
        (0, 0)
    );
}

#[test]
fn actual_service_lifetime_can_outlive_bundle_group() {
    let observer = Observer::isolated();
    let bundle = observer.bundle_build().finish_success().unwrap();
    let bundle_clone = Arc::clone(&bundle);
    let client = observer.client(ClientRole::QualificationProbe);
    drop(bundle);
    assert_eq!(observer.snapshot().unwrap().bundles.live, 1);
    drop(bundle_clone);
    let during_client = observer.snapshot().unwrap();
    assert_eq!(
        (during_client.bundles.live, during_client.bundles.released),
        (0, 1)
    );
    assert_eq!(
        during_client.clients[ClientRole::QualificationProbe.index()].live,
        1
    );
    drop(client);
    let released = observer.snapshot().unwrap();
    assert_eq!(
        (
            released.clients[3].constructed,
            released.clients[3].released,
            released.clients[3].live
        ),
        (1, 1, 0)
    );
}

#[test]
fn failed_and_abandoned_builds_never_commit_residency() {
    let observer = Observer::isolated();
    observer.bundle_build().finish_error();
    drop(observer.bundle_build());
    let observed = observer.snapshot().unwrap().bundles;
    assert_eq!(
        (
            observed.builds_started,
            observed.failed,
            observed.abandoned,
            observed.committed,
            observed.live,
            observed.builds_inflight
        ),
        (2, 1, 1, 0, 0, 0)
    );
}

#[test]
fn enabled_guards_read_clock_and_disabled_guards_do_not() {
    let reads = Cell::new(0);
    let clock = || {
        reads.set(reads.get() + 1);
        Instant::now()
    };
    let disabled = Observer::disabled();
    let inert = BundleBuildSpan::new_with_clock(&disabled, clock);
    assert_eq!(reads.get(), 0);
    assert!(inert.finish_success().is_none());
    let inert_client = disabled.client(ClientRole::PrimaryDataMixed);
    let inert_attempt = inert_client.attempt_with_clock(HttpMethod::Get, None, clock);
    let inert_body = inert_attempt.headers_with_clock(200, clock);
    inert_body.eof();
    assert_eq!(reads.get(), 0);
    assert!(disabled.snapshot().is_none());
    let observer = Observer::isolated();
    let build = BundleBuildSpan::new_with_clock(&observer, clock);
    std::thread::sleep(Duration::from_millis(1));
    drop(build.finish_success());
    let client = observer.client(ClientRole::PrimaryDataMixed);
    let attempt = client.attempt_with_clock(HttpMethod::Get, None, clock);
    std::thread::sleep(Duration::from_millis(1));
    let body = attempt.headers_with_clock(200, clock);
    std::thread::sleep(Duration::from_millis(1));
    body.eof();
    assert_eq!(reads.get(), 3);
    let observed = row(&observer);
    assert!(observed.dispatch_elapsed_ns > 0 && observed.dispatch_max_ns > 0);
    assert!(observed.body_elapsed_ns > 0 && observed.body_max_ns > 0);
    assert!(observer.snapshot().unwrap().bundles.elapsed_ns > 0);
}

#[test]
fn cache_unknown_and_final_release_remove_only_own_contribution() {
    let observer = Observer::isolated();
    let first = observer.cache_residency();
    let second = observer.cache_residency();
    first.publish(2, 7);
    second.publish(3, 11);
    first.mark_unknown();
    first.mark_unknown();
    first.publish(9, 99); // Unknown owner cannot republish unverified values.
    let unknown = observer.snapshot().unwrap().cache;
    assert_eq!(
        (
            unknown.live,
            unknown.resident_entries,
            unknown.payload_bytes,
            unknown.unknown_live
        ),
        (2, 3, 11, 1)
    );
    drop(first);
    let remaining = observer.snapshot().unwrap().cache;
    assert_eq!(
        (
            remaining.live,
            remaining.resident_entries,
            remaining.payload_bytes,
            remaining.unknown_live
        ),
        (1, 3, 11, 0)
    );
    drop(second);
    let final_snapshot = observer.snapshot().unwrap().cache;
    assert_eq!(
        (
            final_snapshot.created,
            final_snapshot.released,
            final_snapshot.live,
            final_snapshot.resident_entries,
            final_snapshot.payload_bytes
        ),
        (2, 2, 0, 0, 0)
    );
}

#[test]
fn fixed_bank_saturates_and_snapshot_discloses_active_update() {
    let observer = Observer::isolated();
    let bank = observer.bank().unwrap();
    bank.clients[0].http[0]
        .counters
        .offered_bytes
        .store(u64::MAX - 1, Ordering::SeqCst);
    let client = observer.client(ClientRole::PrimaryDataMixed);
    client.attempt(HttpMethod::Get, Some(9)).transport_error();
    assert_eq!(row(&observer).offered_bytes, u64::MAX);
    assert!(observer.snapshot().unwrap().saturated);
    let active = bank.begin_update();
    assert!(observer.snapshot().unwrap().concurrent_activity);
    drop(active);
    // Saturated revision/counters must never claim a certain snapshot.
    assert!(observer.snapshot().unwrap().concurrent_activity);
    let clean = Observer::isolated();
    assert!(!clean.snapshot().unwrap().concurrent_activity);
}
