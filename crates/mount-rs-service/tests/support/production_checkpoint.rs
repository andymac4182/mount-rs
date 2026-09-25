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

#[derive(serde::Serialize)]
struct PendingItem {
    file: usize,
    request_id: u64,
    bytes: u64,
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
    acknowledged_by_lane: [u64; 10],
    pending: BTreeMap<usize, PendingItem>,
}
impl PhaseAccounting {
    fn new(name: &'static str, items: u64, bytes: u64) -> Self {
        Self {
            name,
            configured_items: items,
            configured_bytes: bytes,
            attempted_items: 0,
            acknowledged_items: 0,
            uncertain_items: 0,
            acknowledged_bytes: 0,
            uncertain_bytes: 0,
            oracle_verified_items: 0,
            uncertain_samples: Vec::new(),
            control_failures: 0,
            acknowledged_by_lane: [0; 10],
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
        request_id: u64,
        bytes: u64,
    ) -> Result<(), String> {
        if lane >= 10
            || file >= 1000
            || self.pending.contains_key(&lane)
            || self.attempted_items >= self.configured_items
        {
            return Err("phase attempt exceeds bound or overlaps unresolved lane".into());
        }
        self.pending.insert(
            lane,
            PendingItem {
                file,
                request_id,
                bytes,
            },
        );
        self.attempted_items += 1;
        Ok(())
    }
    fn acknowledge_lane(&mut self, lane: usize, bytes: u64) -> Result<(), String> {
        if self.pending.get(&lane).map(|item| item.bytes) != Some(bytes) {
            return Err("acknowledgment does not match pending work".into());
        }
        self.pending.remove(&lane);
        self.acknowledged_items += 1;
        self.acknowledged_bytes += bytes;
        self.acknowledged_by_lane[lane] += 1;
        Ok(())
    }
    fn uncertain_lane(&mut self, lane: usize, bytes: u64) -> Result<(), String> {
        if self.pending.get(&lane).map(|item| item.bytes) != Some(bytes) {
            return Err("uncertainty does not match pending work".into());
        }
        let pending = self.pending.remove(&lane).unwrap();
        if self.uncertain_samples.len() < 10 {
            self.uncertain_samples.push((lane, pending));
        }
        self.uncertain_items += 1;
        self.uncertain_bytes += bytes;
        Ok(())
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
}
impl ScopeAccounting {
    fn complete(&self, clients: u64) -> bool {
        self.sibling_attempts == clients
            && self.sibling_denials == clients
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
    fn new(source: &str, identity: &str, version: &str) -> Self {
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
            configuration: json!({"servers":10,"clients":10,"drives":10,"partitions":5,"files_per_drive":1000,"initial_blocks":15340,"initial_payload_bytes":62832640,"oracle":"40-byte LE Partition/Drive/file/block/generation tuple plus SHA256-seeded SplitMix64; compare every byte independently through fresh backend","ledger_initial_generation":0,"ledger_slots_per_drive":1534,"ledger_vector_payload_bytes":122720,"file_sizes":"990x4096+9x131072+1x1048576 per Drive","pool_max_per_server":16,"runtime_workers":4,"preparation_workers":10,"requests_per_drive":1,"journal_flush_completed":64,"journal_flush_seconds":1,"sampler_interval_ms":100,"protocol_deadline_seconds":30,"phase_budget_seconds":600,"work_budget_seconds":1800,"work_budget_scope":"async work body; excludes initial setup and synchronous observers","cleanup_stage_deadline_seconds":30,"final_cleanup_async_deadline_seconds":90,"cleanup_scope":"concurrent listeners then drivers then contexts; unresolved listener tasks retained through terminal artifact","rss_stop_bytes":24u64*1024*1024*1024,"host_free_floor_bytes":64u64*1024*1024*1024,"token_lifetime_seconds":2100,"auth":"signed ES256 with owned static JWK; discovery network excluded","build_mode":if cfg!(debug_assertions){"debug"}else{"release"},"observer_success_unit":"acknowledged preparation files; not data operations or lifecycle IOPS", "scope":"one process loopback; initial100 empty replicas; refreshed100 populated replicas; no10k capacity claim"}),
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
            namespace: PhaseAccounting::new("namespace", 10_000, 0),
            population: PhaseAccounting::new("payload", 10_000, 62_832_640),
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
        self.network_windows.push(json!({"phase":phase,"connection_lifetime_ids":connections.iter().map(quinn::Connection::stable_id).collect::<Vec<_>>(),"per_drive_lane":after.connection_deltas(before),"aggregate":after.delta(before),"scope":"ten retained primary client connections; excludes auth/setup/temporary100 route probes; UDP bytes exclude IP/UDP headers"}));
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
        self.stages.push(json!({"phase":self.phase, "started_utc_unix_ms":self.phase_started_utc_unix_ms,"ended_utc_unix_ms":utc_unix_ms(), "sampled_peaks":sampler.checkpoint(), "elapsed_seconds":self.stage_started.elapsed().as_secs_f64(), "profile":mount_rs_core::diagnostics::profile::snapshot().delta(&self.profile_started), "resources":super::super::resource_profile::Snapshot::capture_process().unwrap().delta(&self.resource_started)}));
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
) -> Result<(), String> {
    use futures_util::{StreamExt, stream::FuturesUnordered};
    use mount_rs_remote_protocol::{OperationName, binary::IoRequest};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    let shared = Arc::new(Mutex::new(journal));
    let stopped = Arc::new(AtomicBool::new(false));
    let mut workers = FuturesUnordered::new();
    for (drive, connection) in connections.iter().cloned().enumerate() {
        let shared = shared.clone();
        let stopped = stopped.clone();
        workers.push(async move {
            let mut id = ((drive as u64 + 1) << 48) | if payload_phase { 1_000_000 } else { 1 };
            let drive_id = format!("sandbox-{drive}");
            let work: Result<(), String> = async {
                for file in 0..1000 {
                    if stopped.load(Ordering::Acquire) { return Err("another preparation worker failed; no new work dispatched".into()); }
                    if !payload_phase {
                        shared.lock().unwrap().namespace.attempt_lane(drive, file, id, 0)?;
                    }
                    let opened = checkpoint_wire::success(&connection, id, &drive_id, OperationName::Open, json!({"path":format!("/mixed-{file}"),"flags":if payload_phase {"r+"} else {"w+"},"mode":420})).await;
                    let handle = match opened.ok().and_then(|value| value.as_u64()) {
                        Some(handle) => handle,
                        None => {
                            let mut journal = shared.lock().unwrap();
                            if payload_phase { journal.population.control_failure(); } else { journal.namespace.uncertain_lane(drive, 0)?; }
                            return Err("preparation Open failed or malformed; never replayed".into());
                        }
                    };
                    id += 1;
                    if payload_phase {
                        let size = FileProfile::Mixed.size(file);
                        let mut payload = vec![0u8;size];
                        for (block, bytes) in payload.chunks_mut(4096).enumerate() { bytes.copy_from_slice(&oracle_block((drive / 2) as u64, drive as u64, file as u64, block as u64, 0)); }
                        shared.lock().unwrap().population.attempt_lane(drive, file, id, size as u64)?;
                        let request = IoRequest { drive_id: drive_id.clone(), handle, position: Some(0) };
                        match checkpoint_wire::handle_write(&connection, id, &request, &payload).await {
                            Ok(count) if count == size => shared.lock().unwrap().population.acknowledge_lane(drive, size as u64)?,
                            _ => { shared.lock().unwrap().population.uncertain_lane(drive, size as u64)?; return Err("population write outcome uncertain; never replayed".into()); }
                        }
                        id += 1;
                    } else {
                        // Valid Open already acknowledges creation. Close errors
                        // retain this count and fail only the control boundary.
                        shared.lock().unwrap().namespace.acknowledge_lane(drive, 0)?;
                    }
                    if checkpoint_wire::success(&connection, id, &drive_id, OperationName::HandleClose, json!({"handle":handle})).await.is_err() {
                        let mut journal = shared.lock().unwrap();
                        if payload_phase { journal.population.control_failure(); } else { journal.namespace.control_failure(); }
                        return Err("preparation HandleClose failed after acknowledged work".into());
                    }
                    id += 1;
                    let mut journal = shared.lock().unwrap();
                    if !payload_phase && drive == 0 && [1, 10, 100, 250, 500, 1000].contains(&(file + 1)) {
                        let profile = mount_rs_core::diagnostics::profile::snapshot().delta(&journal.profile_started);
                        let elapsed = journal.stage_started.elapsed().as_secs_f64();
                        let acknowledged = journal.namespace.acknowledged_items;
                        journal.structural_checkpoints.push(json!({"trigger_drive":0,"trigger_drive_acknowledged_files":file+1,"global_acknowledged_files":acknowledged,"elapsed_seconds":elapsed,"global_profile":profile,"scope":"global concurrent counters at Drive0 progress; not isolated per-Drive CPU or SQL"}));
                    }
                    journal.persist(output)?;
                }
                Ok(())
            }.await;
            if let Err(error) = &work {
                stopped.store(true, Ordering::Release);
                let mut journal = shared.lock().unwrap();
                if journal.work_error.is_none() { journal.work_error = Some(error.clone()); }
                journal.flush(output)?;
            }
            work
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
#[ignore = "actual TiDB ten-server 10000-file population; exclusive compute lease required"]
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
    let children: Vec<_> = (0..10).map(|drive| backend.child(drive).unwrap()).collect();
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
        .compare_and_swap(0, target_catalog(10))
        .await
        .unwrap();
    let tokens = SignedTokens::new();
    let mut journal = CheckpointJournal::new(&source, &backend.identity, &backend.version);
    journal.stage_started = setup_started;
    journal.profile_started = profile_baseline;
    journal.resource_started = resource_baseline;
    let mut filesystems = Vec::new();
    let mut servers = Vec::new();
    // Handles retain consumed RemoteServer ownership across timeout and cancellation.
    let mut listener_closes = Vec::new();
    let mut connections = Vec::new();
    let ledger: Vec<_> = (0..10)
        .map(|_| GenerationLedger::new(FileProfile::Mixed, 1000))
        .collect();
    let mut request_id = 1u64;
    let work: Result<(), String> = tokio::time::timeout(Duration::from_secs(1800), async {
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
        for (client, (server, endpoint, _)) in servers.iter().enumerate() {
            let token = tokens.token(client, 2100);
            let connection = wire::connect_token(
                endpoint,
                server.local_addr(),
                &format!("partition-{}", client / 2),
                &token,
            )
            .await?;
            journal.connected_clients += 1;
            journal.scope.sibling_attempts += 1;
            let sibling = wire::request(
                &connection,
                request_id,
                &format!("sandbox-{}", client ^ 1),
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
            journal.scope.partition_attempts += 1;
            expect_authentication_denial(
                endpoint,
                server.local_addr(),
                &format!("partition-{}", ((client + 2) % 10) / 2),
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
        journal.begin("online_namespace_ten_workers_depth1_per_drive", &sampler);
        let network_before = resource_profile::Snapshot::capture_connections(&connections).unwrap();
        let prepared = prepare_online_phase(&connections, &mut journal, &output, false).await;
        journal.record_network("namespace", &connections, &network_before);
        prepared?;
        journal.begin("payload_ten_workers_depth1_per_drive", &sampler);
        let network_before = resource_profile::Snapshot::capture_connections(&connections).unwrap();
        let prepared = prepare_online_phase(&connections, &mut journal, &output, true).await;
        journal.record_network("payload", &connections, &network_before);
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
        // End the population stage, then freshly open all100 full namespaces
        // together. Never retain an accidental200 filesystem replicas.
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
        for (drive, (server, endpoint, _)) in servers.iter().enumerate() {
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
            verify_mixed_backing_files(child, drive, &ledger[drive], &mut journal, &output).await?;
            journal.persist(&output)?;
        }
        journal.begin("all_server_route_probes", &sampler);
        for (server, endpoint, _) in &servers {
            for drive in 0..10 {
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
        if !journal.scope.complete(10)
            || journal.connected_clients != 10
            || journal.refreshed_connected_clients != 10
            || journal.refreshed_replica_opens != 100
            || journal.all_server_drive_routes != 100
        {
            return Err("incomplete target checkpoint counts".into());
        }
        if !journal.population.complete()
            || journal.verified_files != 10_000
            || journal.verified_bytes != 62_832_640
        {
            return Err("full population oracle incomplete".into());
        }
        Ok(())
    })
    .await
    .map_err(|_| "checkpoint deadline reached; partial point unqualified".to_string())
    .and_then(|result| result);
    journal.work_error = work.as_ref().err().cloned();
    if work.is_err() {
        for phase in [&mut journal.namespace, &mut journal.population] {
            let pending: Vec<_> = phase
                .pending
                .iter()
                .map(|(lane, item)| (*lane, item.bytes))
                .collect();
            for (lane, bytes) in pending {
                phase.uncertain_lane(lane, bytes).unwrap();
            }
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
