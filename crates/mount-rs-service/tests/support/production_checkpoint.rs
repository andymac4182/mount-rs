//! Bounded actual-backend checkpoint and observer/accounting regressions.
//! This helper source is not executed capacity evidence by itself.
use super::*;
use mount_rs_service::dispatch::ReadFailureFields;
use std::time::Duration;

// Borrow all resources for the whole stage: cancellation never consumes the
// owner's vector. Each future receives the same deadline, so a stage is bounded
// by one deadline rather than resource_count times the deadline.
async fn attempt_cleanup<'a, T, F, Fut>(resources: &'a [T], deadline: Duration, mut close: F) -> u64
where
    F: FnMut(&'a T) -> Fut,
    Fut: std::future::Future<Output = std::result::Result<(), ()>>,
{
    use futures_util::{StreamExt, stream::FuturesUnordered};
    let mut pending = FuturesUnordered::new();
    for resource in resources {
        pending.push(tokio::time::timeout(deadline, close(resource)));
    }
    let mut failures = 0;
    while let Some(outcome) = pending.next().await {
        if !matches!(outcome, Ok(Ok(()))) {
            failures += 1;
        }
    }
    failures
}

async fn await_listener_tasks(
    tasks: &mut [Option<tokio::task::JoinHandle<()>>],
    deadline: Duration,
) -> u64 {
    use futures_util::{StreamExt, stream::FuturesUnordered};
    let mut pending = FuturesUnordered::new();
    for slot in tasks.iter_mut().filter(|slot| slot.is_some()) {
        pending.push(async move {
            let outcome = tokio::time::timeout(deadline, slot.as_mut().unwrap()).await;
            if outcome.is_ok() {
                slot.take();
            }
            outcome
        });
    }
    let mut failures = 0;
    while let Some(outcome) = pending.next().await {
        if !matches!(outcome, Ok(Ok(()))) {
            failures += 1;
        }
    }
    failures
}

#[tokio::test]
async fn cleanup_attempts_all_resources_and_preserves_failures() {
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let resources = vec![0, 1, 2];
    let errors = attempt_cleanup(&resources, Duration::from_millis(20), |item| {
        let calls = calls.clone();
        async move {
            calls.lock().unwrap().push(*item);
            if *item == 0 { Err(()) } else { Ok(()) }
        }
    })
    .await;
    assert_eq!(errors, 1);
    assert_eq!(*calls.lock().unwrap(), vec![0, 1, 2]);
    assert_eq!(resources, vec![0, 1, 2]);
}

#[tokio::test]
async fn cleanup_cancellation_keeps_owned_resources_and_hang_is_bounded() {
    let resources = vec![0, 1, 2];
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cleanup = attempt_cleanup(&resources, Duration::from_secs(30), |_| {
        let entered = entered.clone();
        async move {
            entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<std::result::Result<(), ()>>().await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(10), cleanup)
            .await
            .is_err()
    );
    assert_eq!(entered.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(resources.len(), 3);
    let errors = attempt_cleanup(&resources, Duration::from_millis(1), |_| {
        std::future::pending::<std::result::Result<(), ()>>()
    })
    .await;
    assert_eq!(errors, 3);
}

#[tokio::test]
async fn cleanup_listener_timeout_retains_task_until_terminal_evidence_and_continues() {
    let mut listeners = vec![Some(tokio::spawn(std::future::pending::<()>()))];
    let failures = await_listener_tasks(&mut listeners, Duration::from_millis(1)).await;
    assert_eq!(failures, 1);
    assert!(listeners[0].is_some());
    let order = std::sync::Mutex::new(Vec::new());
    let drivers = [0, 1, 2];
    let driver_errors = attempt_cleanup(&drivers, Duration::from_millis(20), |_| async {
        order.lock().unwrap().push("driver");
        Err(())
    })
    .await;
    let context_errors = attempt_cleanup(&[0, 1], Duration::from_millis(20), |_| async {
        order.lock().unwrap().push("context");
        Err(())
    })
    .await;
    assert_eq!(
        *order.lock().unwrap(),
        vec!["driver", "driver", "driver", "context", "context"]
    );
    let terminal = serde_json::to_string(&json!({"cleanup_errors":failures+driver_errors+context_errors,"unproven_listener_drains":failures})).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&terminal).unwrap()["cleanup_errors"],
        6
    );
    assert!(
        listeners[0].is_some(),
        "terminal evidence precedes ownership loss"
    );
    listeners[0].take().unwrap().abort();
}

#[tokio::test]
async fn cleanup_completed_listener_is_not_polled_twice_after_another_timeout() {
    let mut listeners = vec![
        Some(tokio::spawn(async {})),
        Some(tokio::spawn(std::future::pending::<()>())),
    ];
    assert_eq!(
        await_listener_tasks(&mut listeners, Duration::from_millis(1)).await,
        1
    );
    assert!(listeners[0].is_none());
    assert_eq!(
        await_listener_tasks(&mut listeners, Duration::from_millis(1)).await,
        1
    );
    listeners[1].take().unwrap().abort();
}

#[test]
fn mutation_batch_summary_distinguishes_zero_from_missing_coverage() {
    let empty = mount_rs_core::diagnostics::profile::Snapshot {
        entries: Vec::new(),
    };
    assert_eq!(
        mutation_batch_summary(&Ok(empty.clone()), true)["attempted_requests"],
        0
    );
    assert!(mutation_batch_summary(&Ok(empty), false)["attempted_requests"].is_null());
    assert!(mutation_batch_summary(&Err("counter reset"), true)["attempted_requests"].is_null());
}
fn mutation_batch_summary(
    profile: &Result<mount_rs_core::diagnostics::profile::Snapshot, &'static str>,
    enabled: bool,
) -> serde_json::Value {
    if !enabled {
        return json!({"coverage":"profiling_disabled","batch_calls":null,"attempted_requests":null});
    }
    match profile {
        Err(error) => json!({"coverage":error,"batch_calls":null,"attempted_requests":null}),
        Ok(profile) => {
            let entry = profile
                .entries
                .iter()
                .find(|entry| entry.name == "filesystem.mutation_batch_attempted_requests");
            json!({"coverage":"profile_delta","batch_calls":entry.map_or(0,|entry|entry.calls),"attempted_requests":entry.map_or(0,|entry|entry.units),"scope":"attempted requests per batch event, not acknowledged files or publications; Open creates may bypass this event"})
        }
    }
}

#[test]
fn bounded_population_config_preserves_defaults_and_25_singleton() {
    let control = PopulationConfig::new(10, 1).unwrap();
    assert_eq!(control, PopulationConfig::default());
    let point = PopulationConfig::new(25, 4).unwrap();
    assert_eq!(
        (
            point.partitions(),
            point.files(),
            point.blocks(),
            point.bytes(),
            point.replicas()
        ),
        (13, 25000, 38350, 157081600, 250)
    );
    assert_eq!(point.sibling(24), None);
    assert_eq!(point.sibling(23), Some(22));
    assert_eq!(point.sibling_count(), 24);
    for drive in 0..25 {
        assert_ne!(drive / 2, point.other_partition(drive));
        assert!(point.server(drive) < 10);
    }
    assert!(PopulationConfig::new(10000, 1).is_err());
    assert!(PopulationConfig::new(25, 64).is_err());
}

#[test]
fn singleton_scope_requires_exact_applicability_and_fail_closed_denominators() {
    let mut scope = ScopeAccounting {
        sibling_attempts: 24,
        sibling_denials: 24,
        sibling_not_applicable: 1,
        partition_attempts: 25,
        partition_denials: 25,
        transport_errors: 0,
    };
    assert!(scope.complete_counts(25, 24));
    scope.sibling_not_applicable = 0;
    assert!(!scope.complete_counts(25, 24));
    scope.sibling_not_applicable = 1;
    scope.transport_errors = 1;
    assert!(!scope.complete_counts(25, 24));
}

#[test]
fn multislot_ack_requires_exact_identity_and_cancellation_resolves_once() {
    let config = PopulationConfig::new(25, 4).unwrap();
    let mut phase = PhaseAccounting::configured("namespace", config, 4, 0);
    let a = WorkIdentity::new(24, 0, 0, 100, 0, "namespace");
    let b = WorkIdentity::new(24, 1, 1, 101, 0, "namespace");
    phase.attempt_work(a.clone()).unwrap();
    phase.attempt_work(b.clone()).unwrap();
    let mut wrong = b.clone();
    wrong.request_id += 1;
    assert!(phase.acknowledge_work(&wrong).is_err());
    for wrong in [
        WorkIdentity::new(24, 1, 2, 101, 0, "namespace"),
        WorkIdentity::new(24, 1, 1, 101, 1, "namespace"),
        WorkIdentity::new(24, 1, 1, 101, 0, "payload"),
        WorkIdentity::new(usize::MAX, 1, 1, 101, 0, "namespace"),
    ] {
        assert!(phase.acknowledge_work(&wrong).is_err());
    }
    phase.acknowledge_work(&b).unwrap();
    assert!(phase.acknowledge_work(&b).is_err());
    assert_eq!(phase.acknowledged_by_lane[24], 1);
    assert_eq!(phase.acknowledged_by_slot[24], vec![0, 1, 0, 0]);
    phase.cancel_pending();
    phase.cancel_pending();
    assert_eq!(phase.uncertain_items, 1);
    assert_eq!(phase.uncertain_by_slot[24], vec![1, 0, 0, 0]);
    assert_eq!(phase.uncertain_samples[0].1.request_id, a.request_id);
}

#[test]
fn preparation_gate_stops_new_chains_and_retains_slots_through_close() {
    let mut gate = PreparationGate::new(PopulationConfig::new(25, 4).unwrap(), 4);
    for drive in 0..25 {
        for slot in 0..4 {
            gate.admit(drive, slot).unwrap();
        }
    }
    assert_eq!(gate.peak, 100);
    assert!(gate.admit(24, 0).is_err());
    gate.stop();
    gate.finish(24, 0);
    assert!(gate.admit(24, 0).is_err());
    assert_eq!(gate.active.len(), 99);
    assert!(gate.peak_per_drive.iter().all(|peak| *peak == 4));
}

#[tokio::test]
async fn configured_slots_are_first_polled_bounded_and_cover_each_file_once() {
    use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
    for (drives, depth) in [(10, 1), (10, 4), (25, 1), (25, 4)] {
        let config = PopulationConfig::new(drives, depth).unwrap();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(PreparationGate::new(config, depth)));
        let mut workers = FuturesUnordered::new();
        let mut files = std::collections::BTreeSet::new();
        for (drive, slot) in preparation_slots(config, depth) {
            for file in (slot..1000).step_by(depth) {
                assert!(files.insert((drive, file)));
            }
            let gate = gate.clone();
            workers.push(async move {
                gate.lock().unwrap().admit(drive, slot).unwrap();
                std::future::pending::<()>().await;
            });
        }
        assert!(workers.next().now_or_never().is_none());
        assert_eq!(gate.lock().unwrap().peak, drives * depth);
        assert!(
            gate.lock()
                .unwrap()
                .peak_per_drive
                .iter()
                .all(|peak| *peak == depth)
        );
        assert_eq!(files.len(), drives * 1000);
        gate.lock().unwrap().stop();
        drop(workers);
        assert!(gate.lock().unwrap().admit(0, 0).is_err());
    }
}

#[tokio::test]
async fn enclosing_timeout_retains_actual_phase_depth_ack_pending_and_partial_network() {
    use futures_util::{StreamExt, stream::FuturesUnordered};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    };
    struct Dropped(Arc<AtomicU64>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let config = PopulationConfig::new(10, 4).unwrap();
    let network = Arc::new(AtomicU64::new(7));
    let dropped = Arc::new(AtomicU64::new(0));
    let owner = PreparationOwner::new();
    owner
        .start(RetainedPreparation::new(
            config,
            4,
            "namespace",
            false,
            7u64,
        ))
        .unwrap();
    let accounting = Arc::new(Mutex::new(PhaseAccounting::configured(
        "namespace",
        config,
        4,
        0,
    )));
    let result = run_preparation_work(&owner, Duration::from_millis(10), async {
        let mut workers = FuturesUnordered::new();
        for (drive, slot) in preparation_slots(config, 4) {
            let gate = owner.gate();
            let accounting = accounting.clone();
            let network = network.clone();
            let dropped = dropped.clone();
            workers.push(async move {
                let _dropped = Dropped(dropped);
                gate.lock().unwrap().admit(drive, slot).unwrap();
                let identity = WorkIdentity::new(
                    drive,
                    slot,
                    slot,
                    population_request_id(drive, 1, slot, 0).unwrap(),
                    0,
                    "namespace",
                );
                {
                    let mut accounting = accounting.lock().unwrap();
                    accounting.attempt_work(identity.clone()).unwrap();
                    if (drive, slot) == (0, 0) {
                        accounting.acknowledge_work(&identity).unwrap();
                    }
                }
                network.fetch_add(1, Ordering::SeqCst);
                std::future::pending::<()>().await;
            });
        }
        workers.next().await;
    })
    .await;
    let mut observed = None;
    let result = result.finish(|phase, partial| {
        observed = Some(phase.finish(
            partial,
            |before| json!({"delta":network.load(Ordering::SeqCst)-before}),
        ))
    });
    assert!(result.is_err());
    owner.finish_returned(|_, _| panic!("partial timeout phase captured twice"));
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        40,
        "actual enclosing timeout dropped every started worker"
    );
    let mut accounting = accounting.lock().unwrap();
    assert_eq!(accounting.acknowledged_items, 1);
    assert_eq!(accounting.pending.len(), 39);
    let observed =
        observed.expect("shared production timeout finalizer must capture partial phase");
    assert_eq!(observed["peak"], 40);
    assert_eq!(observed["uncompleted_chains"], 40);
    assert_eq!(observed["network"]["delta"], 40);
    assert_eq!(observed["partial"], true);
    assert_eq!(observed["per_drive_peak"], json!(vec![4; 10]));
    accounting.cancel_pending();
    accounting.cancel_pending();
    assert_eq!(accounting.uncertain_items, 39);
    assert_eq!(accounting.acknowledged_items, 1);
}

#[tokio::test]
async fn production_owner_captures_normal_and_returned_error_once_as_complete() {
    for failure in [false, true] {
        let owner = PreparationOwner::new();
        let mut captures = Vec::new();
        let completed = run_preparation_work(&owner, Duration::from_secs(1), async {
            owner
                .start(RetainedPreparation::new(
                    PopulationConfig::default(),
                    1,
                    "namespace",
                    false,
                    0u64,
                ))
                .unwrap();
            owner.finish_returned(|phase, partial| {
                captures.push(phase.finish(partial, |_| json!("returned")))
            });
            owner.finish_returned(|_, _| panic!("phase captured twice"));
            if failure {
                Err("returned phase error")
            } else {
                Ok(())
            }
        })
        .await;
        let result = completed.finish(|_, _| panic!("outer finalizer duplicated returned capture"));
        assert_eq!(result.unwrap().is_err(), failure);
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0]["partial"], false);
    }
}

#[test]
fn population_ids_are_disjoint_and_thresholds_cross_once() {
    let mut ids = std::collections::BTreeSet::new();
    for drive in [0, 24, 9999] {
        for phase in [1, 2] {
            for file in 0..1000 {
                for step in 0..3 {
                    let id = population_request_id(drive, phase, file, step).unwrap();
                    assert!(id >= (1u64 << 48));
                    assert!(ids.insert(id));
                }
            }
        }
    }
    assert!(population_request_id(10000, 1, 0, 0).is_err());
    let mut thresholds = ProgressThresholds::default();
    assert_eq!(thresholds.crossed(11), vec![1, 10]);
    assert!(thresholds.crossed(11).is_empty());
    assert_eq!(thresholds.crossed(1000), vec![100, 250, 500, 1000]);
}

#[test]
fn read_failure_fields_never_retain_untrusted_details() {
    let secret = "mysql://user:secret@host/private?token=bearer SQL SELECT payload";
    let fields = ReadFailureFields::from_parts("EIO", Some(secret), None, 4096);
    let encoded = serde_json::to_string(&fields).unwrap();
    assert!(!encoded.contains(secret));
    for forbidden in ["secret", "SELECT", "payload", "bearer", "mysql://"] {
        assert!(!encoded.contains(forbidden));
    }
    assert_eq!(fields.category, "driver_error");
    assert_eq!(fields.syscall, Some("other"));
    assert_eq!(fields.posix_code, Some("EIO"));
}

#[test]
fn read_failure_fields_distinguish_error_from_invalid_driver_count() {
    let driver = ReadFailureFields::from_parts("EIO", Some("read"), None, 4096);
    assert_eq!(driver.category, "driver_error");
    assert_eq!(driver.syscall, Some("read"));
    let count = ReadFailureFields::from_parts("EIO", None, Some(4097), 4096);
    assert_eq!(count.category, "driver_count_exceeds_buffer");
    assert_eq!(count.returned_count, Some(4097));
    assert_eq!(count.buffer_length, 4096);
    // A short success is not the service's count-exceeds-buffer EIO branch.
    assert!(ReadFailureFields::try_from_success(4095, 4096).is_none());
    assert!(ReadFailureFields::try_from_success(4096, 4096).is_none());
    let unknown =
        ReadFailureFields::from_parts("arbitrary secret", Some("arbitrary secret"), None, 4096);
    assert_eq!(unknown.posix_code, None);
    assert_eq!(unknown.syscall, Some("other"));
}

#[test]
fn phase_accounting_does_not_turn_uncertain_or_partial_work_into_success() {
    let mut phase = PhaseAccounting::new("payload", 10_000, 62_832_640);
    phase.attempt(4096).unwrap();
    phase.acknowledge(4096).unwrap();
    phase.attempt(131_072).unwrap();
    phase.uncertain(131_072).unwrap();
    assert_eq!(phase.attempted_items, 2);
    assert_eq!(phase.acknowledged_items, 1);
    assert_eq!(phase.uncertain_items, 1);
    assert_eq!(phase.acknowledged_bytes, 4096);
    assert!(!phase.complete());
    assert!(
        phase.acknowledge(1).is_err(),
        "unattempted work must not be acknowledged"
    );
    assert!(
        phase.oracle_verified_items == 0,
        "acknowledgment is not byte verification"
    );
}

#[test]
fn phase_accounting_counts_failed_sibling_and_partition_probes_separately() {
    let mut phase = ScopeAccounting {
        sibling_attempts: 10,
        sibling_denials: 9,
        partition_attempts: 10,
        partition_denials: 10,
        ..Default::default()
    };
    assert!(!phase.complete(10));
    phase.sibling_denials = 10;
    assert!(phase.complete(10));
    phase.transport_errors = 1;
    assert!(!phase.complete(10));
}

#[test]
fn concurrent_phase_accounting_preserves_each_identity_and_out_of_order_results() {
    let mut phase = PhaseAccounting::new("payload", 2, 12_288);
    phase.attempt_lane(0, 7, 11, 4096).unwrap();
    phase.attempt_lane(1, 8, 12, 8192).unwrap();
    assert_eq!(phase.pending.len(), 2);
    assert_eq!(phase.pending[&0].file, 7);
    assert_eq!(phase.pending[&1].request_id, 12);
    assert!(phase.attempt_lane(0, 9, 13, 4096).is_err());
    phase.acknowledge_lane(1, 8192).unwrap();
    assert!(phase.acknowledge_lane(1, 8192).is_err());
    phase.uncertain_lane(0, 4096).unwrap();
    assert_eq!(phase.acknowledged_bytes, 8192);
    assert_eq!(phase.uncertain_bytes, 4096);
    assert_eq!(phase.uncertain_samples[0].0, 0);
    assert_eq!(phase.uncertain_samples[0].1.file, 7);
    assert_eq!(phase.uncertain_samples[0].1.request_id, 11);
    assert!(!phase.complete());
}

#[test]
fn known_open_or_full_write_ack_survives_later_close_failure() {
    for (name, bytes) in [("namespace", 0), ("payload", 4096)] {
        let mut phase = PhaseAccounting::new(name, 1, bytes);
        phase.attempt_lane(0, 0, 100, bytes).unwrap();
        phase.acknowledge_lane(0, bytes).unwrap();
        phase.control_failure();
        assert_eq!(phase.acknowledged_items, 1);
        assert_eq!(phase.acknowledged_bytes, bytes);
        assert!(phase.pending.is_empty());
        assert!(!phase.complete());
        assert!(phase.acknowledge_lane(0, bytes).is_err());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
struct PopulationConfig {
    drives: usize,
    namespace_depth: usize,
}
impl Default for PopulationConfig {
    fn default() -> Self {
        Self {
            drives: 10,
            namespace_depth: 1,
        }
    }
}
impl PopulationConfig {
    fn new(drives: usize, namespace_depth: usize) -> Result<Self, String> {
        if ![10, 25].contains(&drives) || ![1, 4].contains(&namespace_depth) {
            return Err("only10/25 Drives and Open1/Open4 accepted".into());
        }
        Ok(Self {
            drives,
            namespace_depth,
        })
    }
    #[cfg(all(feature = "resource-profiling", unix))]
    fn environment() -> Result<Self, String> {
        fn value(key: &str, default: usize) -> Result<usize, String> {
            match std::env::var(key) {
                Ok(value) => value.parse().map_err(|_| format!("invalid {key}")),
                Err(std::env::VarError::NotPresent) => Ok(default),
                Err(_) => Err(format!("invalid {key}")),
            }
        }
        Self::new(
            value("MOUNT_RS_CHECKPOINT_DRIVES", 10)?,
            value("MOUNT_RS_CHECKPOINT_NAMESPACE_DEPTH", 1)?,
        )
    }
    fn partitions(self) -> usize {
        self.drives.div_ceil(2)
    }
    fn files(self) -> u64 {
        self.drives as u64 * 1000
    }
    fn blocks(self) -> u64 {
        self.drives as u64 * 1534
    }
    fn bytes(self) -> u64 {
        self.drives as u64 * 6_283_264
    }
    fn replicas(self) -> u64 {
        self.drives as u64 * 10
    }
    fn server(self, drive: usize) -> usize {
        drive % 10
    }
    fn sibling(self, drive: usize) -> Option<usize> {
        let sibling = drive ^ 1;
        (sibling < self.drives).then_some(sibling)
    }
    fn sibling_count(self) -> u64 {
        (self.drives - self.drives % 2) as u64
    }
    fn other_partition(self, drive: usize) -> usize {
        ((drive / 2) + 1) % self.partitions()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct WorkIdentity {
    drive: usize,
    slot: usize,
    file: usize,
    request_id: u64,
    bytes: u64,
    kind: &'static str,
}
impl WorkIdentity {
    fn new(
        drive: usize,
        slot: usize,
        file: usize,
        request_id: u64,
        bytes: u64,
        kind: &'static str,
    ) -> Self {
        Self {
            drive,
            slot,
            file,
            request_id,
            bytes,
            kind,
        }
    }
}
type PendingItem = WorkIdentity;

fn population_request_id(drive: usize, phase: u64, file: usize, step: u64) -> Result<u64, String> {
    if drive >= 10000 || ![1, 2].contains(&phase) || file >= 1000 || step >= 3 {
        return Err("request identity exceeds bounded dimensions".into());
    }
    Ok(((drive as u64 + 1) << 48) | (phase << 40) | (file as u64 * 4 + step))
}
#[derive(Default)]
struct ProgressThresholds {
    next: usize,
}
impl ProgressThresholds {
    fn crossed(&mut self, acknowledged: u64) -> Vec<u64> {
        let thresholds = [1, 10, 100, 250, 500, 1000];
        let mut crossed = Vec::new();
        while self.next < thresholds.len() && thresholds[self.next] <= acknowledged {
            crossed.push(thresholds[self.next]);
            self.next += 1;
        }
        crossed
    }
}
fn preparation_slots(config: PopulationConfig, depth: usize) -> Vec<(usize, usize)> {
    (0..config.drives)
        .flat_map(|drive| (0..depth).map(move |slot| (drive, slot)))
        .collect()
}
struct PreparationOwner<N> {
    retained: std::sync::Mutex<Option<RetainedPreparation<N>>>,
}
impl<N> PreparationOwner<N> {
    fn new() -> Self {
        Self {
            retained: std::sync::Mutex::new(None),
        }
    }
    fn start(&self, phase: RetainedPreparation<N>) -> Result<(), &'static str> {
        let mut retained = self.retained.lock().unwrap();
        if retained.is_some() {
            return Err("prior preparation phase remains owned");
        }
        *retained = Some(phase);
        Ok(())
    }
    fn gate(&self) -> std::sync::Arc<std::sync::Mutex<PreparationGate>> {
        self.retained.lock().unwrap().as_ref().unwrap().gate.clone()
    }
    fn take(&self) -> Option<RetainedPreparation<N>> {
        self.retained.lock().unwrap().take()
    }
    fn finish_returned(&self, collect: impl FnOnce(RetainedPreparation<N>, bool)) {
        if let Some(phase) = self.take() {
            collect(phase, false);
        }
    }
}
struct OwnedPreparationWork<R, N> {
    result: Result<R, tokio::time::error::Elapsed>,
    partial: Option<RetainedPreparation<N>>,
}
impl<R, N> OwnedPreparationWork<R, N> {
    fn finish(
        self,
        collect: impl FnOnce(RetainedPreparation<N>, bool),
    ) -> Result<R, tokio::time::error::Elapsed> {
        if let Some(phase) = self.partial {
            collect(phase, true);
        }
        self.result
    }
}
// Shared production deadline/ownership orchestration. The future is dropped by
// timeout before we take its phase, and collection precedes error propagation.
async fn run_preparation_work<R, N>(
    owner: &PreparationOwner<N>,
    deadline: Duration,
    work: impl std::future::Future<Output = R>,
) -> OwnedPreparationWork<R, N> {
    let result = tokio::time::timeout(deadline, work).await;
    let partial = owner.take();
    OwnedPreparationWork { result, partial }
}

// Owned by the enclosing checkpoint, not by the cancelable phase future.
struct RetainedPreparation<N> {
    gate: std::sync::Arc<std::sync::Mutex<PreparationGate>>,
    network_before: N,
    phase: &'static str,
    payload: bool,
}
impl<N> RetainedPreparation<N> {
    fn new(
        config: PopulationConfig,
        depth: usize,
        phase: &'static str,
        payload: bool,
        network_before: N,
    ) -> Self {
        Self {
            gate: std::sync::Arc::new(std::sync::Mutex::new(PreparationGate::new(config, depth))),
            network_before,
            phase,
            payload,
        }
    }
    fn finish(
        self,
        partial: bool,
        network: impl FnOnce(&N) -> serde_json::Value,
    ) -> serde_json::Value {
        let gate = self.gate.lock().unwrap();
        json!({"phase":self.phase,"payload":self.payload,"peak":gate.peak,"per_drive_peak":gate.peak_per_drive,"uncompleted_chains":gate.active.len(),"configured_depth":gate.depth,"partial":partial,"network":network(&self.network_before),"path":"Open then HandleClose; direct structural path, no assumed MutationBatch coalescing"})
    }
}
#[cfg(all(feature = "resource-profiling", unix))]
fn finish_retained_preparation(
    retained: RetainedPreparation<super::super::resource_profile::Snapshot>,
    journal: &mut CheckpointJournal,
    connections: &[quinn::Connection],
    partial: bool,
) {
    let phase = retained.phase;
    let payload = retained.payload;
    let observation = retained.finish(partial, |before| {
        journal.record_network(phase, connections, before);
        let index = journal.network_windows.len() - 1;
        journal.network_windows[index]["partial"] = json!(partial);
        journal.network_windows[index]["boundary"] = json!(if partial {
            "outer cancellation before closing connections"
        } else {
            "returned phase including errors"
        });
        json!({"window_index":index})
    });
    journal.configuration[if payload {
        "observed_payload_chains"
    } else {
        "observed_namespace_chains"
    }] = observation;
}
struct PreparationGate {
    config: PopulationConfig,
    depth: usize,
    stopped: bool,
    active: std::collections::BTreeSet<(usize, usize)>,
    peak: usize,
    peak_per_drive: Vec<usize>,
}
impl PreparationGate {
    fn new(config: PopulationConfig, depth: usize) -> Self {
        Self {
            config,
            depth,
            stopped: false,
            active: Default::default(),
            peak: 0,
            peak_per_drive: vec![0; config.drives],
        }
    }
    fn admit(&mut self, drive: usize, slot: usize) -> Result<(), String> {
        if self.stopped
            || drive >= self.config.drives
            || slot >= self.depth
            || !self.active.insert((drive, slot))
        {
            return Err("preparation stopped or slot unavailable".into());
        }
        self.peak = self.peak.max(self.active.len());
        let active = self.active.iter().filter(|(d, _)| *d == drive).count();
        self.peak_per_drive[drive] = self.peak_per_drive[drive].max(active);
        Ok(())
    }
    fn finish(&mut self, drive: usize, slot: usize) {
        self.active.remove(&(drive, slot));
    }
    fn stop(&mut self) {
        self.stopped = true;
    }
}
#[derive(serde::Serialize)]
struct PhaseAccounting {
    name: &'static str,
    configured_items: u64,
    configured_bytes: u64,
    attempted_items: u64,
    acknowledged_items: u64,
    uncertain_items: u64,
    acknowledged_bytes: u64,
    uncertain_bytes: u64,
    oracle_verified_items: u64,
    uncertain_samples: Vec<(usize, PendingItem)>,
    control_failures: u64,
    acknowledged_by_lane: Vec<u64>,
    // Fixed-slot strided workers acknowledge an ordered prefix within each slot.
    // These counts identify every acknowledged file without an unbounded event log.
    acknowledged_by_slot: Vec<Vec<u64>>,
    uncertain_by_slot: Vec<Vec<u64>>,
    depth: usize,
    pending: BTreeMap<usize, PendingItem>,
}
impl PhaseAccounting {
    fn new(name: &'static str, items: u64, bytes: u64) -> Self {
        let mut phase = Self::configured(name, PopulationConfig::default(), 1, bytes);
        phase.configured_items = items;
        phase
    }
    fn configured(name: &'static str, config: PopulationConfig, depth: usize, bytes: u64) -> Self {
        Self {
            name,
            configured_items: config.files(),
            configured_bytes: bytes,
            attempted_items: 0,
            acknowledged_items: 0,
            uncertain_items: 0,
            acknowledged_bytes: 0,
            uncertain_bytes: 0,
            oracle_verified_items: 0,
            uncertain_samples: Vec::new(),
            control_failures: 0,
            acknowledged_by_lane: vec![0; config.drives],
            acknowledged_by_slot: vec![vec![0; depth]; config.drives],
            uncertain_by_slot: vec![vec![0; depth]; config.drives],
            depth,
            pending: BTreeMap::new(),
        }
    }
    fn attempt(&mut self, bytes: u64) -> Result<(), String> {
        self.attempt_lane(0, 0, 0, bytes)
    }
    fn acknowledge(&mut self, bytes: u64) -> Result<(), String> {
        self.acknowledge_lane(0, bytes)
    }
    fn uncertain(&mut self, bytes: u64) -> Result<(), String> {
        self.uncertain_lane(0, bytes)
    }
    fn attempt_lane(
        &mut self,
        lane: usize,
        file: usize,
        id: u64,
        bytes: u64,
    ) -> Result<(), String> {
        self.attempt_work(WorkIdentity::new(lane, 0, file, id, bytes, self.name))
    }
    fn attempt_work(&mut self, work: WorkIdentity) -> Result<(), String> {
        if work.drive >= self.acknowledged_by_lane.len()
            || work.slot >= self.depth
            || work.file >= 1000
            || work.kind != self.name
            || self
                .pending
                .contains_key(&(work.drive * self.depth + work.slot))
            || self.attempted_items >= self.configured_items
        {
            return Err("phase attempt exceeds bound or overlaps pending slot".into());
        }
        let key = work.drive * self.depth + work.slot;
        self.pending.insert(key, work);
        self.attempted_items += 1;
        Ok(())
    }
    fn acknowledge_lane(&mut self, lane: usize, bytes: u64) -> Result<(), String> {
        let work = self
            .pending
            .get(&(lane * self.depth))
            .filter(|work| work.bytes == bytes)
            .cloned()
            .ok_or("unmatched acknowledgment")?;
        self.acknowledge_work(&work)
    }
    fn acknowledge_work(&mut self, work: &WorkIdentity) -> Result<(), String> {
        if work.drive >= self.acknowledged_by_lane.len() || work.slot >= self.depth {
            return Err("identity exceeds configured slots".into());
        }
        let key = work.drive * self.depth + work.slot;
        if self.pending.get(&key) != Some(work) {
            return Err("acknowledgment differs from exact pending identity".into());
        }
        self.pending.remove(&key);
        self.acknowledged_items += 1;
        self.acknowledged_bytes += work.bytes;
        self.acknowledged_by_lane[work.drive] += 1;
        self.acknowledged_by_slot[work.drive][work.slot] += 1;
        Ok(())
    }
    fn uncertain_lane(&mut self, lane: usize, bytes: u64) -> Result<(), String> {
        let work = self
            .pending
            .get(&(lane * self.depth))
            .filter(|work| work.bytes == bytes)
            .cloned()
            .ok_or("unmatched uncertainty")?;
        self.uncertain_work(&work)
    }
    fn uncertain_work(&mut self, work: &WorkIdentity) -> Result<(), String> {
        if work.drive >= self.acknowledged_by_lane.len() || work.slot >= self.depth {
            return Err("identity exceeds configured slots".into());
        }
        let key = work.drive * self.depth + work.slot;
        if self.pending.get(&key) != Some(work) {
            return Err("uncertainty differs from exact pending identity".into());
        }
        self.pending.remove(&key);
        if self.uncertain_samples.len() < 10 {
            self.uncertain_samples.push((work.drive, work.clone()));
        }
        self.uncertain_items += 1;
        self.uncertain_by_slot[work.drive][work.slot] += 1;
        self.uncertain_bytes += work.bytes;
        Ok(())
    }
    fn cancel_pending(&mut self) {
        let pending: Vec<_> = self.pending.values().cloned().collect();
        for work in pending {
            self.uncertain_work(&work).unwrap();
        }
    }
    fn control_failure(&mut self) {
        self.control_failures += 1;
    }
    fn complete(&self) -> bool {
        self.pending.is_empty()
            && self.uncertain_items == 0
            && self.control_failures == 0
            && self.acknowledged_items == self.configured_items
            && self.acknowledged_bytes == self.configured_bytes
    }
}
#[derive(Default, serde::Serialize)]
struct ScopeAccounting {
    sibling_attempts: u64,
    sibling_denials: u64,
    partition_attempts: u64,
    partition_denials: u64,
    transport_errors: u64,
    sibling_not_applicable: u64,
}
impl ScopeAccounting {
    fn complete(&self, clients: u64) -> bool {
        self.complete_counts(clients, clients)
    }
    fn complete_counts(&self, clients: u64, siblings: u64) -> bool {
        self.sibling_attempts == siblings
            && self.sibling_denials == siblings
            && self.sibling_not_applicable == clients - siblings
            && self.partition_attempts == clients
            && self.partition_denials == clients
            && self.transport_errors == 0
    }
}
#[cfg(all(feature = "resource-profiling", unix))]
fn utc_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}
#[cfg(all(feature = "resource-profiling", unix))]
fn host_available_bytes() -> Result<u64, String> {
    #[cfg(target_os = "macos")]
    let path = c"/System/Volumes/Data";
    #[cfg(not(target_os = "macos"))]
    let path = c"/";
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err("host capacity observation failed".into());
    }
    let stats = unsafe { stats.assume_init() };
    #[cfg(target_os = "macos")]
    let available = u64::from(stats.f_bavail);
    #[cfg(not(target_os = "macos"))]
    let available = stats.f_bavail;
    available
        .checked_mul(stats.f_frsize)
        .ok_or("host capacity overflow".into())
}
#[cfg(all(feature = "resource-profiling", unix))]
#[derive(serde::Serialize)]
struct CheckpointJournal {
    host_available_start_bytes: u64,
    host_available_end_bytes: Option<u64>,
    created_utc_unix_ms: u64,
    phase_started_utc_unix_ms: u64,
    observer_errors: Vec<String>,
    #[serde(skip)]
    observer_stage: Option<String>,
    #[serde(skip)]
    observer_acknowledged_start: u64,
    journal_writes: u64,
    journal_bytes: u64,
    #[serde(skip)]
    last_flush: std::time::Instant,
    #[serde(skip)]
    last_flush_completed: u64,
    #[serde(skip)]
    last_flush_phase: &'static str,
    configuration: serde_json::Value,
    schema: &'static str,
    source: String,
    provider_identity: String,
    provider_version: String,
    phase: &'static str,
    empty_replica_opens: u64,
    refreshed_replica_opens: u64,
    connected_clients: u64,
    refreshed_connected_clients: u64,
    all_server_drive_routes: u64,
    namespace: PhaseAccounting,
    population: PhaseAccounting,
    scope: ScopeAccounting,
    protocol_read_requests: u64,
    protocol_verified_bytes: u64,
    verified_files: u64,
    verified_bytes: u64,
    cleanup_errors: u64,
    unproven_listener_drains: u64,
    work_error: Option<String>,
    resources: Option<super::super::resource_profile::ProcessSummary>,
    #[serde(skip)]
    stage_started: std::time::Instant,
    #[serde(skip)]
    profile_started: mount_rs_core::diagnostics::profile::Snapshot,
    #[serde(skip)]
    resource_started: super::super::resource_profile::Snapshot,
    network_windows: Vec<serde_json::Value>,
    structural_checkpoints: Vec<serde_json::Value>,
    stages: Vec<serde_json::Value>,
    backing_counts: Vec<serde_json::Value>,
}
#[cfg(all(feature = "resource-profiling", unix))]
impl CheckpointJournal {
    fn new(source: &str, identity: &str, version: &str, config: PopulationConfig) -> Self {
        Self {
            host_available_start_bytes: host_available_bytes().unwrap(),
            host_available_end_bytes: None,
            journal_writes: 0,
            journal_bytes: 0,
            last_flush: std::time::Instant::now(),
            last_flush_completed: 0,
            last_flush_phase: "",
            created_utc_unix_ms: utc_unix_ms(),
            phase_started_utc_unix_ms: utc_unix_ms(),
            observer_errors: Vec::new(),
            observer_stage: None,
            observer_acknowledged_start: 0,
            configuration: json!({"servers":10,"clients":config.drives,"drives":config.drives,"partitions":config.partitions(),"singleton_drives":config.drives%2,"files_per_drive":1000,"initial_blocks":config.blocks(),"initial_payload_bytes":config.bytes(),"oracle":"40-byte LE Partition/Drive/file/block/generation tuple plus SHA256-seeded SplitMix64; compare every byte independently through fresh backend","ledger_initial_generation":0,"ledger_slots_per_drive":1534,"ledger_vector_payload_bytes":config.drives*1534*8,"file_sizes":"990x4096+9x131072+1x1048576 per Drive","pool_max_per_server":16,"runtime_workers":4,"preparation_workers":config.drives*config.namespace_depth,"namespace_depth":config.namespace_depth,"payload_depth":1,"journal_flush_completed":64,"journal_flush_seconds":1,"sampler_interval_ms":100,"protocol_deadline_seconds":30,"phase_budget_seconds":600,"work_budget_seconds":1800,"work_budget_scope":"async work body; excludes initial setup and synchronous observers","cleanup_stage_deadline_seconds":30,"final_cleanup_async_deadline_seconds":90,"cleanup_scope":"concurrent listeners then drivers then contexts; unresolved listener tasks retained through terminal artifact","rss_stop_bytes":24u64*1024*1024*1024,"host_free_floor_bytes":64u64*1024*1024*1024,"token_lifetime_seconds":2100,"auth":"signed ES256 with owned static JWK; discovery network excluded","build_mode":if cfg!(debug_assertions){"debug"}else{"release"},"observer_success_unit":"acknowledged preparation files; not data operations or lifecycle IOPS", "scope":"one process loopback; configured servers x Drives eager empty and refreshed full replicas; no10k capacity claim","oracle_parallelism":1,"ack_identity_encoding":"each slot acknowledges an ordered file prefix: slot + depth * ordinal; IDs derived from Drive/phase/file/step"}),
            schema: "mount-rs-production-tidb-checkpoint-v1",
            source: source.into(),
            provider_identity: identity.into(),
            provider_version: version.into(),
            phase: "setup",
            empty_replica_opens: 0,
            refreshed_replica_opens: 0,
            connected_clients: 0,
            refreshed_connected_clients: 0,
            all_server_drive_routes: 0,
            namespace: PhaseAccounting::configured("namespace", config, config.namespace_depth, 0),
            population: PhaseAccounting::configured("payload", config, 1, config.bytes()),
            scope: ScopeAccounting::default(),
            protocol_read_requests: 0,
            protocol_verified_bytes: 0,
            verified_files: 0,
            verified_bytes: 0,
            cleanup_errors: 0,
            unproven_listener_drains: 0,
            work_error: None,
            resources: None,
            stage_started: std::time::Instant::now(),
            profile_started: mount_rs_core::diagnostics::profile::snapshot(),
            resource_started: super::super::resource_profile::Snapshot::capture_process().unwrap(),
            network_windows: Vec::new(),
            structural_checkpoints: Vec::new(),
            stages: Vec::new(),
            backing_counts: Vec::new(),
        }
    }
    fn begin(
        &mut self,
        phase: &'static str,
        sampler: &super::super::resource_profile::ProcessSampler,
    ) {
        self.finish_stage(sampler);
        self.phase = phase;
        self.phase_started_utc_unix_ms = utc_unix_ms();
        let stage_id = format!("task4-{}-{}-{phase}", std::process::id(), self.stages.len());
        if self.observe_stage("begin", &stage_id, 0, 0) {
            self.observer_stage = Some(stage_id);
        }
        self.observer_acknowledged_start =
            self.namespace.acknowledged_items + self.population.acknowledged_items;
        self.stage_started = std::time::Instant::now();
        self.profile_started = mount_rs_core::diagnostics::profile::snapshot();
        self.resource_started =
            super::super::resource_profile::Snapshot::capture_process().unwrap();
    }
    fn record_network(
        &mut self,
        phase: &'static str,
        connections: &[quinn::Connection],
        before: &super::super::resource_profile::Snapshot,
    ) {
        let after =
            super::super::resource_profile::Snapshot::capture_connections(connections).unwrap();
        self.network_windows.push(json!({"phase":phase,"connection_lifetime_ids":connections.iter().map(quinn::Connection::stable_id).collect::<Vec<_>>(),"per_drive_lane":after.connection_deltas(before),"aggregate":after.delta(before),"scope":"all configured retained primary client connections; excludes auth/setup/temporary routing probes; UDP bytes exclude IP/UDP headers"}));
    }
    fn observe_stage(
        &mut self,
        action: &str,
        stage_id: &str,
        successes: u64,
        failures: u64,
    ) -> bool {
        let Some(observer) = std::env::var_os("MOUNT_RS_PRODUCTION_STAGE_OBSERVER") else {
            return false;
        };
        let result = std::process::Command::new(observer)
            .args([
                action,
                "mixed",
                "1",
                stage_id,
                &successes.to_string(),
                &failures.to_string(),
            ])
            .output();
        if !matches!(result, Ok(ref output) if output.status.success()) {
            self.observer_errors.push(format!(
                "{action} {stage_id}: counter coverage unavailable; details redacted"
            ));
            false
        } else {
            true
        }
    }
    fn finish_stage(&mut self, sampler: &super::super::resource_profile::ProcessSampler) {
        if let Some(stage_id) = self.observer_stage.take() {
            let successes = self.namespace.acknowledged_items + self.population.acknowledged_items
                - self.observer_acknowledged_start;
            self.observe_stage(
                "end",
                &stage_id,
                successes,
                u64::from(self.work_error.is_some()),
            );
        }
        let profile = mount_rs_core::diagnostics::profile::snapshot().delta(&self.profile_started);
        let mutation_batch =
            mutation_batch_summary(&profile, mount_rs_core::diagnostics::profile::enabled());
        self.stages.push(json!({"phase":self.phase, "started_utc_unix_ms":self.phase_started_utc_unix_ms,"ended_utc_unix_ms":utc_unix_ms(), "sampled_peaks":sampler.checkpoint(), "elapsed_seconds":self.stage_started.elapsed().as_secs_f64(), "profile":profile,"mutation_batch":mutation_batch, "resources":super::super::resource_profile::Snapshot::capture_process().unwrap().delta(&self.resource_started)}));
    }
    fn persist(&mut self, output: &str) -> Result<(), String> {
        self.check_budget()?;
        let completed = self.namespace.acknowledged_items
            + self.population.acknowledged_items
            + self.empty_replica_opens
            + self.refreshed_replica_opens
            + self.verified_files
            + self.all_server_drive_routes;
        if self.work_error.is_none()
            && self.last_flush_phase == self.phase
            && completed - self.last_flush_completed < 64
            && self.last_flush.elapsed() < std::time::Duration::from_secs(1)
        {
            return Ok(());
        }
        self.flush(output)
    }
    fn check_budget(&self) -> Result<(), String> {
        if self.work_error.is_none() {
            if host_available_bytes()? < 64 * 1024 * 1024 * 1024 {
                return Err("host free space below64GiB stop budget".into());
            }
            if super::super::resource_profile::Snapshot::capture_process()
                .map_err(|_| "process resource unavailable")?
                .resident_bytes()
                .is_some_and(|bytes| bytes > 24 * 1024 * 1024 * 1024)
            {
                return Err("process RSS exceeds24GiB stop budget".into());
            }
            if self.stage_started.elapsed() > std::time::Duration::from_secs(600) {
                return Err(format!(
                    "phase {} exceeded600s budget; partial point unqualified",
                    self.phase
                ));
            }
        }
        Ok(())
    }
    fn flush(&mut self, output: &str) -> Result<(), String> {
        self.journal_writes += 1;
        let previous_bytes = self.journal_bytes;
        // Exact fixed point: the serialized total includes this write's bytes.
        let bytes = loop {
            let bytes =
                serde_json::to_vec_pretty(self).map_err(|_| "journal serialization failed")?;
            let total = previous_bytes + bytes.len() as u64;
            if total == self.journal_bytes {
                break bytes;
            }
            self.journal_bytes = total;
        };
        std::fs::write(output, bytes).map_err(|_| "journal write failed")?;
        self.last_flush = std::time::Instant::now();
        self.last_flush_completed = self.namespace.acknowledged_items
            + self.population.acknowledged_items
            + self.empty_replica_opens
            + self.refreshed_replica_opens
            + self.verified_files
            + self.all_server_drive_routes;
        self.last_flush_phase = self.phase;
        Ok(())
    }
}
#[cfg(all(feature = "resource-profiling", unix))]
async fn verify_mixed_backing_files(
    backend: &super::super::backend::Backend,
    drive: usize,
    ledger: &GenerationLedger,
    journal: &mut CheckpointJournal,
    output: &str,
) -> Result<(), String> {
    let fs = backend.open(0).await?;
    let result: Result<(), String> = async {
        let driver = fs.driver();
        let entries = driver
            .readdir("/")
            .await
            .map_err(|_| "fresh oracle readdir failed")?;
        let actual: std::collections::BTreeSet<_> = entries
            .iter()
            .filter(|entry| entry.name != "." && entry.name != "..")
            .map(|entry| entry.name.as_str())
            .collect();
        let expected: std::collections::BTreeSet<_> =
            (0..1000).map(|file| format!("mixed-{file}")).collect();
        if actual.len() != 1000 || !expected.iter().all(|name| actual.contains(name.as_str())) {
            return Err("fresh membership oracle mismatch".into());
        }
        for file in 0..1000 {
            let handle = driver
                .open(&format!("/mixed-{file}"), "r", 0)
                .await
                .map_err(|_| "fresh oracle open failed")?;
            let size = FileProfile::Mixed.size(file);
            if handle
                .stat()
                .await
                .map_err(|_| "fresh oracle stat failed")?
                .size
                != size as u64
            {
                return Err("fresh file size mismatch".into());
            }
            let mut bytes = [0u8; 4096];
            for block in 0..size / 4096 {
                if handle
                    .read(&mut bytes, Some((block * 4096) as u64))
                    .await
                    .map_err(|_| "fresh oracle read failed")?
                    != 4096
                    || !verify_block(
                        &bytes,
                        (drive / 2) as u64,
                        drive as u64,
                        file as u64,
                        block as u64,
                        ledger.generation(file, block),
                    )
                {
                    return Err("fresh full-byte oracle mismatch".into());
                }
                journal.verified_bytes += 4096;
            }
            if handle
                .read(&mut bytes, Some(size as u64))
                .await
                .map_err(|_| "fresh oracle EOF failed")?
                != 0
            {
                return Err("fresh EOF mismatch".into());
            }
            handle
                .close()
                .await
                .map_err(|_| "fresh oracle close failed")?;
            journal.verified_files += 1;
            journal.population.oracle_verified_items += 1;
            journal.namespace.oracle_verified_items += 1;
            journal.persist(output)?;
        }
        Ok(())
    }
    .await;
    let stopped = fs
        .shutdown()
        .await
        .map_err(|_| "fresh oracle shutdown failed".to_string());
    result.and(stopped)
}

#[cfg(all(feature = "resource-profiling", unix))]
mod checkpoint_wire {
    use super::super::super::wire as raw;
    use mount_rs_remote_protocol::{OperationName, binary::IoRequest};
    async fn bounded<T>(
        connection: &quinn::Connection,
        future: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        match tokio::time::timeout(std::time::Duration::from_secs(30), future).await {
            Ok(result) => result,
            Err(_) => {
                connection.close(1u32.into(), b"deadline; never replayed");
                Err("protocol deadline; mutation outcome may be uncertain; never replayed".into())
            }
        }
    }
    pub async fn connect_token(
        endpoint: &quinn::Endpoint,
        address: std::net::SocketAddr,
        partition: &str,
        token: &str,
    ) -> Result<quinn::Connection, String> {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            raw::connect_token(endpoint, address, partition, token),
        )
        .await
        .map_err(|_| "connection deadline")?
    }
    pub async fn request(
        connection: &quinn::Connection,
        id: u64,
        drive: &str,
        operation: OperationName,
        body: serde_json::Value,
    ) -> Result<Result<serde_json::Value, String>, String> {
        bounded(
            connection,
            raw::request(connection, id, drive, operation, body),
        )
        .await
    }
    pub async fn success(
        connection: &quinn::Connection,
        id: u64,
        drive: &str,
        operation: OperationName,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        bounded(
            connection,
            raw::success(connection, id, drive, operation, body),
        )
        .await
    }
    pub async fn handle_read(
        connection: &quinn::Connection,
        id: u64,
        request: &IoRequest,
        buffer: &mut [u8],
    ) -> Result<usize, String> {
        bounded(
            connection,
            raw::handle_read(connection, id, request, buffer),
        )
        .await
    }
    pub async fn handle_write(
        connection: &quinn::Connection,
        id: u64,
        request: &IoRequest,
        data: &[u8],
    ) -> Result<usize, String> {
        bounded(connection, raw::handle_write(connection, id, request, data)).await
    }
}

#[cfg(all(feature = "resource-profiling", unix))]
async fn prepare_online_phase(
    connections: &[quinn::Connection],
    journal: &mut CheckpointJournal,
    output: &str,
    payload_phase: bool,
    config: PopulationConfig,
    gate: std::sync::Arc<std::sync::Mutex<PreparationGate>>,
) -> Result<(), String> {
    use futures_util::{StreamExt, stream::FuturesUnordered};
    use mount_rs_remote_protocol::{OperationName, binary::IoRequest};
    use std::sync::{Arc, Mutex};
    let depth = if payload_phase {
        1
    } else {
        config.namespace_depth
    };
    let shared = Arc::new(Mutex::new(journal));
    let thresholds = Arc::new(Mutex::new(ProgressThresholds::default()));
    let mut workers = FuturesUnordered::new();
    for (drive, slot) in preparation_slots(config, depth) {
        let connection = connections[drive].clone();
        let shared = shared.clone();
        let gate = gate.clone();
        let thresholds = thresholds.clone();
        workers.push(async move {
            let drive_id=format!("sandbox-{drive}");
            let work:Result<(),String>=async {
                for file in (slot..1000).step_by(depth) {
                    gate.lock().unwrap().admit(drive,slot)?;
                    let phase=if payload_phase {2} else {1};
                    let open_id=population_request_id(drive,phase,file,0)?;
                    let identity=WorkIdentity::new(drive,slot,file,open_id,0,"namespace");
                    if !payload_phase {shared.lock().unwrap().namespace.attempt_work(identity.clone())?;}
                    let opened=checkpoint_wire::success(&connection,open_id,&drive_id,OperationName::Open,json!({"path":format!("/mixed-{file}"),"flags":if payload_phase {"r+"} else {"w+"},"mode":420})).await;
                    let handle=match opened.ok().and_then(|value|value.as_u64()) {
                        Some(handle)=>handle,
                        None=> {let mut journal=shared.lock().unwrap(); if payload_phase {journal.population.control_failure();} else {journal.namespace.uncertain_work(&identity)?;} return Err("preparation Open failed or malformed; never replayed".into());}
                    };
                    if payload_phase {
                        let size=FileProfile::Mixed.size(file);let mut payload=vec![0u8;size];
                        for (block,bytes) in payload.chunks_mut(4096).enumerate() {bytes.copy_from_slice(&oracle_block((drive/2)as u64,drive as u64,file as u64,block as u64,0));}
                        let write_id=population_request_id(drive,phase,file,1)?;
                        let identity=WorkIdentity::new(drive,slot,file,write_id,size as u64,"payload");
                        shared.lock().unwrap().population.attempt_work(identity.clone())?;
                        let request=IoRequest{drive_id:drive_id.clone(),handle,position:Some(0)};
                        match checkpoint_wire::handle_write(&connection,write_id,&request,&payload).await {
                            Ok(count)if count==size=>shared.lock().unwrap().population.acknowledge_work(&identity)?,
                            _=>{shared.lock().unwrap().population.uncertain_work(&identity)?;return Err("population write outcome uncertain; never replayed".into());}
                        }
                    } else {shared.lock().unwrap().namespace.acknowledge_work(&identity)?;}
                    let close_id=population_request_id(drive,phase,file,2)?;
                    if checkpoint_wire::success(&connection,close_id,&drive_id,OperationName::HandleClose,json!({"handle":handle})).await.is_err() {
                        let mut journal=shared.lock().unwrap();if payload_phase {journal.population.control_failure();} else {journal.namespace.control_failure();}return Err("preparation Close failed after acknowledged mutation".into());
                    }
                    gate.lock().unwrap().finish(drive,slot);
                    let mut journal=shared.lock().unwrap();
                    if !payload_phase && drive==0 {
                        let acknowledged=journal.namespace.acknowledged_by_lane[0];
                        for threshold in thresholds.lock().unwrap().crossed(acknowledged) {
                            let profile=mount_rs_core::diagnostics::profile::snapshot().delta(&journal.profile_started);
                            let elapsed=journal.stage_started.elapsed().as_secs_f64(); let global_acknowledged=journal.namespace.acknowledged_items;
                            journal.structural_checkpoints.push(json!({"trigger_drive":0,"threshold":threshold,"trigger_drive_acknowledged_files":acknowledged,"global_acknowledged_files":global_acknowledged,"elapsed_seconds":elapsed,"global_profile":profile,"scope":"global concurrent counters; not isolated Drive CPU or SQL"}));
                        }
                    }
                    journal.persist(output)?;
                } Ok(())
            }.await;
            if let Err(error)=&work {
                // This mutex defines the stop boundary, coherent with every admission.
                gate.lock().unwrap().stop();
                let mut journal=shared.lock().unwrap();if journal.work_error.is_none(){journal.work_error=Some(error.clone());}journal.flush(output)?;
            } work
        });
    }
    let mut first_error = None;
    while let Some(result) = workers.next().await {
        if let Err(error) = result
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Actual backend/full-file checkpoint, deliberately ignored for a serial lease.
/// Runs only with explicit owned output/source identifiers and real TiDB config.
#[cfg(all(feature = "resource-profiling", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "actual TiDB ten-server fully populated10/25 Drive Open1/Open4 checkpoint; exclusive compute lease required"]
async fn ten_server_tidb_ten_drive_mixed_population_checkpoint() {
    use super::super::{backend, resource_profile};
    use checkpoint_wire as wire;
    use mount_rs_remote_protocol::OperationName;
    use std::{
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    let output =
        std::env::var("MOUNT_RS_PRODUCTION_CHECKPOINT_OUTPUT").expect("owned output required");
    let source =
        std::env::var("MOUNT_RS_PROFILE_SOURCE_REVISION").expect("frozen source revision required");
    assert_eq!(
        std::env::var("MOUNT_RS_REMOTE_SATURATION_PROVIDER").as_deref(),
        Ok("tidb")
    );
    assert_eq!(
        std::env::var("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES").as_deref(),
        Ok("1")
    );
    let config = PopulationConfig::environment().unwrap();
    let sampler = resource_profile::ProcessSampler::start(Duration::from_millis(100)).unwrap();
    let setup_started = std::time::Instant::now();
    let profile_baseline = mount_rs_core::diagnostics::profile::snapshot();
    let resource_baseline = resource_profile::Snapshot::capture_process().unwrap();
    let key = format!(
        "production-checkpoint-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let backend = backend::Backend::from_environment(&key).await.unwrap();
    let children: Vec<_> = (0..config.drives)
        .map(|drive| backend.child(drive).unwrap())
        .collect();
    let contexts: Vec<_> = (0..10)
        .map(|_| mount_rs_sdk::StorageContext::new(16).unwrap())
        .collect();
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        mount_rs_service::catalog::SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
    );
    catalog
        .compare_and_swap(0, target_catalog(config.drives))
        .await
        .unwrap();
    let tokens = SignedTokens::new();
    let mut journal = CheckpointJournal::new(&source, &backend.identity, &backend.version, config);
    journal.stage_started = setup_started;
    journal.profile_started = profile_baseline;
    journal.resource_started = resource_baseline;
    let mut filesystems = Vec::new();
    let mut servers = Vec::new();
    // Handles retain consumed RemoteServer ownership across timeout and cancellation.
    let mut listener_closes = Vec::new();
    let mut connections = Vec::new();
    let ledger: Vec<_> = (0..config.drives)
        .map(|_| GenerationLedger::new(FileProfile::Mixed, 1000))
        .collect();
    let mut request_id = 1u64;
    // Both gate and network-start snapshot survive dropping the work future.
    let retained_preparation = PreparationOwner::new();
    let work: Result<(), String> =
        run_preparation_work(&retained_preparation, Duration::from_secs(1800), async {
            // Initial replicas are empty. Their count is separate from refreshed,
            // fully populated replicas opened below; neither is invented from config.
            journal.begin("empty_replica_open", &sampler);
            for context in &contexts {
                let mut drivers = Vec::new();
                for child in &children {
                    let fs = child
                        .open_in_context(filesystems.len(), Some(context))
                        .await?;
                    drivers.push(fs.driver());
                    filesystems.push(fs);
                    journal.empty_replica_opens += 1;
                    journal.persist(&output)?;
                }
                servers.push(
                    start_balanced_fixture(
                        Arc::new(
                            mount_rs_service::catalog::SqliteCatalog::open(
                                directory.path().join("catalog.sqlite"),
                            )
                            .await
                            .map_err(|_| "server catalog open failed")?,
                        ),
                        drivers,
                        &tokens,
                    )
                    .await?,
                );
            }
            journal.begin("signed_connections", &sampler);
            for client in 0..config.drives {
                let (server, endpoint, _) = &servers[config.server(client)];
                let token = tokens.token(client, 2100);
                let connection = wire::connect_token(
                    endpoint,
                    server.local_addr(),
                    &format!("partition-{}", client / 2),
                    &token,
                )
                .await?;
                journal.connected_clients += 1;
                if let Some(sibling_drive) = config.sibling(client) {
                    journal.scope.sibling_attempts += 1;
                    let sibling = wire::request(
                        &connection,
                        request_id,
                        &format!("sandbox-{}", sibling_drive),
                        OperationName::Stat,
                        json!({"path":"/"}),
                    )
                    .await
                    .inspect_err(|_| {
                        journal.scope.transport_errors += 1;
                    })?;
                    request_id += 1;
                    if sibling != Err("EACCES".into()) {
                        return Err("sibling scope denial missing".into());
                    }
                    journal.scope.sibling_denials += 1;
                } else {
                    journal.scope.sibling_not_applicable += 1;
                }
                journal.scope.partition_attempts += 1;
                expect_authentication_denial(
                    endpoint,
                    server.local_addr(),
                    &format!("partition-{}", config.other_partition(client)),
                    &token,
                )
                .await
                .inspect_err(|_| {
                    journal.scope.transport_errors += 1;
                })?;
                journal.scope.partition_denials += 1;
                connections.push(connection);
                journal.persist(&output)?;
            }
            journal.begin(
                match (config.drives, config.namespace_depth) {
                    (10, 1) => "online_namespace_ten_workers_depth1_per_drive",
                    (25, 1) => "online_namespace_25_workers_depth1_per_drive",
                    _ => "online_namespace_open4_per_drive",
                },
                &sampler,
            );
            retained_preparation
                .start(RetainedPreparation::new(
                    config,
                    config.namespace_depth,
                    "namespace",
                    false,
                    resource_profile::Snapshot::capture_connections(&connections).unwrap(),
                ))
                .unwrap();
            let prepared = prepare_online_phase(
                &connections,
                &mut journal,
                &output,
                false,
                config,
                retained_preparation.gate(),
            )
            .await;
            retained_preparation.finish_returned(|phase, partial| {
                finish_retained_preparation(phase, &mut journal, &connections, partial)
            });
            prepared?;
            journal.begin(
                if config.drives == 10 {
                    "payload_ten_workers_depth1_per_drive"
                } else {
                    "payload_25_workers_depth1_per_drive"
                },
                &sampler,
            );
            retained_preparation
                .start(RetainedPreparation::new(
                    config,
                    1,
                    "payload",
                    true,
                    resource_profile::Snapshot::capture_connections(&connections).unwrap(),
                ))
                .unwrap();
            let prepared = prepare_online_phase(
                &connections,
                &mut journal,
                &output,
                true,
                config,
                retained_preparation.gate(),
            )
            .await;
            retained_preparation.finish_returned(|phase, partial| {
                finish_retained_preparation(phase, &mut journal, &connections, partial)
            });
            prepared?;
            journal.begin("owned_backing_counts", &sampler);
            for child in &children {
                let counts = child.owned_counts().await?;
                if counts["blocks"].as_u64() != Some(1534)
                    || counts["logical_block_bytes"].as_u64() != Some(6_283_264)
                    || counts["inodes"].as_u64() != Some(1001)
                {
                    return Err("owned logical backing count mismatch".into());
                }
                journal.backing_counts.push(counts);
                journal.persist(&output)?;
            }
            journal.begin("fresh_full_namespace_replicas", &sampler);
            // End population, then freshly open all configured full namespaces
            // together. Never retain both empty and refreshed server x Drive sets.
            for connection in connections.drain(..) {
                connection.close(0u32.into(), b"population stage complete");
            }
            for (server, endpoint, _) in servers.drain(..) {
                endpoint.close(0u32.into(), b"population stage complete");
                listener_closes.push(Some(tokio::spawn(async move {
                    server.close().await;
                    endpoint.wait_idle().await;
                })));
            }
            let listener_errors =
                await_listener_tasks(&mut listener_closes, Duration::from_secs(30)).await;
            journal.unproven_listener_drains += listener_errors;
            journal.cleanup_errors += listener_errors;
            // Borrow, never drain: outer cancellation leaves every replica owned.
            let replica_errors =
                attempt_cleanup(&filesystems, Duration::from_secs(30), |fs| async move {
                    fs.shutdown().await.map_err(|_| ())
                })
                .await;
            journal.cleanup_errors += replica_errors;
            if listener_errors + replica_errors != 0 {
                return Err("population transition cleanup incomplete".into());
            }
            listener_closes.clear();
            filesystems.clear();
            for context in &contexts {
                let mut drivers = Vec::new();
                for child in &children {
                    let fs = child
                        .open_in_context(filesystems.len(), Some(context))
                        .await?;
                    drivers.push(fs.driver());
                    journal.refreshed_replica_opens += 1;
                    filesystems.push(fs);
                    journal.persist(&output)?;
                }
                servers.push(
                    start_balanced_fixture(
                        Arc::new(
                            mount_rs_service::catalog::SqliteCatalog::open(
                                directory.path().join("catalog.sqlite"),
                            )
                            .await
                            .map_err(|_| "server catalog open failed")?,
                        ),
                        drivers,
                        &tokens,
                    )
                    .await?,
                );
            }
            for drive in 0..config.drives {
                let (server, endpoint, _) = &servers[config.server(drive)];
                let connection = wire::connect_token(
                    endpoint,
                    server.local_addr(),
                    &format!("partition-{}", drive / 2),
                    &tokens.token(drive, 2100),
                )
                .await?;
                connections.push(connection);
                journal.refreshed_connected_clients += 1;
                journal.persist(&output)?;
            }
            journal.begin("bounded_protocol_mixed_size_read_oracles", &sampler);
            let probe_network_before =
                resource_profile::Snapshot::capture_connections(&connections).unwrap();
            for (drive, connection) in connections.iter().enumerate() {
                let drive_id = format!("sandbox-{drive}");
                for file in [0, 990, 999] {
                    let handle = wire::success(
                        connection,
                        request_id,
                        &drive_id,
                        OperationName::Open,
                        json!({"path":format!("/mixed-{file}"),"flags":"r","mode":0}),
                    )
                    .await?
                    .as_u64()
                    .ok_or("invalid probe handle")?;
                    request_id += 1;
                    let blocks = FileProfile::Mixed.size(file) / 4096;
                    for block in [0, blocks - 1] {
                        let request = mount_rs_remote_protocol::binary::IoRequest {
                            drive_id: drive_id.clone(),
                            handle,
                            position: Some((block * 4096) as u64),
                        };
                        let mut bytes = [0u8; 4096];
                        journal.protocol_read_requests += 1;
                        let count =
                            wire::handle_read(connection, request_id, &request, &mut bytes).await?;
                        request_id += 1;
                        if count != 4096
                            || !verify_block(
                                &bytes,
                                (drive / 2) as u64,
                                drive as u64,
                                file as u64,
                                block as u64,
                                0,
                            )
                        {
                            return Err("protocol mixed-size full-byte probe mismatch".into());
                        }
                        journal.protocol_verified_bytes += 4096;
                    }
                    let request = mount_rs_remote_protocol::binary::IoRequest {
                        drive_id: drive_id.clone(),
                        handle,
                        position: Some(FileProfile::Mixed.size(file) as u64),
                    };
                    let mut bytes = [0u8; 4096];
                    journal.protocol_read_requests += 1;
                    if wire::handle_read(connection, request_id, &request, &mut bytes).await? != 0 {
                        return Err("protocol EOF probe mismatch".into());
                    }
                    request_id += 1;
                    wire::success(
                        connection,
                        request_id,
                        &drive_id,
                        OperationName::HandleClose,
                        json!({"handle":handle}),
                    )
                    .await?;
                    request_id += 1;
                    journal.persist(&output)?;
                }
            }
            journal.record_network("protocol_read_probes", &connections, &probe_network_before);
            journal.begin("fresh_byte_oracle", &sampler);
            // A fresh backend open is the oracle: protocol write acknowledgments
            // cannot verify durable bytes or namespace membership by themselves.
            for (drive, child) in children.iter().enumerate() {
                verify_mixed_backing_files(child, drive, &ledger[drive], &mut journal, &output)
                    .await?;
                journal.persist(&output)?;
            }
            journal.begin("all_server_route_probes", &sampler);
            for (server, endpoint, _) in &servers {
                for drive in 0..config.drives {
                    let connection = wire::connect_token(
                        endpoint,
                        server.local_addr(),
                        &format!("partition-{}", drive / 2),
                        &tokens.token(drive, 2100),
                    )
                    .await?;
                    wire::success(
                        &connection,
                        request_id,
                        &format!("sandbox-{drive}"),
                        OperationName::Stat,
                        json!({"path":"/mixed-999"}),
                    )
                    .await?;
                    request_id += 1;
                    journal.all_server_drive_routes += 1;
                    connection.close(0u32.into(), b"route probe complete");
                    journal.persist(&output)?;
                }
            }
            if !journal
                .scope
                .complete_counts(config.drives as u64, config.sibling_count())
                || journal.connected_clients != config.drives as u64
                || journal.refreshed_connected_clients != config.drives as u64
                || journal.empty_replica_opens != config.replicas()
                || journal.refreshed_replica_opens != config.replicas()
                || journal.all_server_drive_routes != config.replicas()
            {
                return Err("incomplete target checkpoint counts".into());
            }
            if !journal.namespace.complete()
                || !journal.population.complete()
                || journal.verified_files != config.files()
                || journal.verified_bytes != config.bytes()
            {
                return Err("full population oracle incomplete".into());
            }
            Ok(())
        })
        .await
        .finish(|phase, partial| {
            finish_retained_preparation(phase, &mut journal, &connections, partial)
        })
        .map_err(|_| "checkpoint deadline reached; partial point unqualified".to_string())
        .and_then(|result| result);
    journal.work_error = work.as_ref().err().cloned();
    if work.is_err() {
        for phase in [&mut journal.namespace, &mut journal.population] {
            phase.cancel_pending();
        }
    }
    journal.begin("cleanup", &sampler);
    for connection in connections {
        connection.close(0u32.into(), b"checkpoint end");
    }
    for (server, endpoint, _) in servers {
        endpoint.close(0u32.into(), b"checkpoint end");
        listener_closes.push(Some(tokio::spawn(async move {
            server.close().await;
            endpoint.wait_idle().await;
        })));
    }
    let listener_errors = await_listener_tasks(&mut listener_closes, Duration::from_secs(30)).await;
    journal.unproven_listener_drains += listener_errors;
    journal.cleanup_errors += listener_errors;
    journal.cleanup_errors +=
        attempt_cleanup(&filesystems, Duration::from_secs(30), |fs| async move {
            fs.shutdown().await.map_err(|_| ())
        })
        .await;
    // All driver attempts precede all shared pool close attempts. Three concurrent
    // deadline stages bound final cleanup at90s, excluding synchronous observers.
    journal.cleanup_errors +=
        attempt_cleanup(&contexts, Duration::from_secs(30), |context| async move {
            context.close().await.map_err(|_| ())
        })
        .await;
    journal.host_available_end_bytes = host_available_bytes().ok();
    journal.finish_stage(&sampler);
    journal.resources = Some(sampler.finish().unwrap());
    journal.flush(&output).unwrap();
    // Only after terminal evidence may unresolved task handles leave scope; they
    // remain explicitly unproven, never successful drains.
    drop(listener_closes);
    work.unwrap();
    assert_eq!(journal.cleanup_errors, 0);
    assert!(
        journal.observer_errors.is_empty(),
        "observer coverage failed"
    );
    assert_eq!(journal.resources.as_ref().unwrap().capture_errors, 0);
}
