use super::config::{DISK_FLOOR, RSS_CAP};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub fn disk_available() -> Result<u64, String> {
    #[cfg(target_os = "macos")]
    let path = c"/System/Volumes/Data";
    #[cfg(not(target_os = "macos"))]
    let path = c"/";
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err("host free disk observation unavailable".into());
    }
    let stats = unsafe { stats.assume_init() };
    #[cfg(target_os = "macos")]
    let available = u64::from(stats.f_bavail);
    #[cfg(not(target_os = "macos"))]
    let available = stats.f_bavail;
    available
        .checked_mul(stats.f_frsize)
        .ok_or("disk observation overflow".into())
}

type DiskObservation = fn() -> Result<u64, String>;

pub struct Resources {
    stop: Arc<AtomicBool>,
    current: Arc<Mutex<Value>>,
    thread: Option<std::thread::JoinHandle<()>>,
    checkpoint_setup: Option<Arc<Mutex<Option<super::checkpoints::Setup>>>>,
}
pub fn validate_sample(value: &Value, pid: u32, now: u64) -> Result<(), String> {
    if let Some(error) = value["error"].as_str() {
        return Err(error.into());
    }
    if value["pid"].as_u64() != Some(pid as u64) || value["samples"].as_u64().unwrap_or(0) == 0 {
        return Err("resource identity or samples missing".into());
    }
    let time = value["observed_unix_ms"]
        .as_u64()
        .ok_or("resource timestamp missing")?;
    if time == 0 || time > now || now - time > 10000 {
        return Err("resource coverage stale or invalid".into());
    }
    let current = value["process_delta"]["rss_end_bytes"]
        .as_u64()
        .ok_or("current RSS observation missing")?;
    let lifetime = value["process_delta"]["lifetime_peak_rss_bytes"]
        .as_u64()
        .ok_or("OS lifetime RSS peak missing")?;
    let peak = value["peak_rss_bytes"]
        .as_u64()
        .ok_or("observed RSS peak missing")?;
    if current > RSS_CAP || lifetime > RSS_CAP || peak > RSS_CAP {
        return Err("owned process RSS cap exceeded".into());
    }
    if peak < current.max(lifetime) {
        return Err("RSS peak receipt regressed".into());
    }
    for field in ["cpu_user_us", "cpu_system_us"] {
        if value["process_delta"][field].as_u64().is_none() {
            return Err("CPU observation missing".into());
        }
    }
    if value["minimum_host_free_bytes"]
        .as_u64()
        .ok_or("host disk observation missing")?
        < DISK_FLOOR
    {
        return Err("host free disk below64GiB".into());
    }
    Ok(())
}
fn capture(
    first: &super::resource_profile::Snapshot,
    samples: u64,
    previous_peak: u64,
    previous_disk: u64,
    disk_observation: DiskObservation,
    control_scope: Option<&'static str>,
) -> Result<Value, String> {
    let span = super::metrics::observer().begin("sampler_capture");
    let result: Result<Value, String> = (|| {
        let snapshot = super::resource_profile::Snapshot::capture_process()
            .map_err(|_| "process observation failed")?;
        let mut delta = snapshot.delta(first).map_err(|_| "process delta failed")?;
        delta["quic_client_side"] =
            json!({"available":false,"reason":"process-only sampler; no transport observation"});
        let current = snapshot.resident_bytes().ok_or("current RSS unavailable")?;
        let lifetime = delta["lifetime_peak_rss_bytes"]
            .as_u64()
            .ok_or("lifetime RSS unavailable")?;
        let mut value = json!({"pid":std::process::id(),"samples":samples,"peak_rss_bytes":previous_peak.max(current).max(lifetime),"minimum_host_free_bytes":previous_disk.min(disk_observation()?),"error":null,"process_delta":delta,"sample_interval_ms":100,"observed_unix_ms":super::utc_ms()});
        if let Some(scope) = control_scope {
            value["disk_observation_scope"] = json!(scope);
        }
        Ok(value)
    })();
    span.finish(result.is_ok(), 0);
    result
}
fn retain_observation(
    previous: Value,
    result: Result<Value, String>,
    terminal: bool,
    pid: u32,
    now: u64,
) -> Value {
    let mut value = match result {
        Ok(mut value) => {
            value["terminal_sample"] = json!(terminal);
            value
        }
        Err(error) => {
            let mut value = previous.clone();
            value["error"] = json!(error);
            value["terminal_sample"] = json!(false);
            value
        }
    };
    if let Err(error) = validate_sample(&value, pid, now) {
        value["error"] = json!(error);
    }
    if !previous["error"].is_null() {
        value["error"] = previous["error"].clone();
    }
    value
}
fn validate_terminal_sample(value: &Value, pid: u32, now: u64) -> Result<(), String> {
    validate_sample(value, pid, now)?;
    if value["terminal_sample"] != true {
        return Err("final sampler-thread observation missing".into());
    }
    Ok(())
}
impl Resources {
    pub fn start(path: std::path::PathBuf) -> Result<Self, String> {
        Self::start_observing_disk(path, disk_available, None)
    }
    fn start_observing_disk(
        path: std::path::PathBuf,
        disk_observation: DiskObservation,
        control_scope: Option<&'static str>,
    ) -> Result<Self, String> {
        let first = super::resource_profile::Snapshot::capture_process()
            .map_err(|_| "resource baseline unavailable")?;
        let initial = capture(&first, 1, 0, u64::MAX, disk_observation, control_scope)?;
        validate_sample(&initial, std::process::id(), super::utc_ms())?;
        super::write_json(&path, &initial)?;
        let stop = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(initial));
        let halt = stop.clone();
        let output = current.clone();
        let checkpoint_setup = mount_rs_core::diagnostics::profile::enabled()
            .then(|| Arc::new(Mutex::new(None::<super::checkpoints::Setup>)));
        let checkpoint_control = checkpoint_setup.clone();
        let checkpoint_path = checkpoint_setup
            .as_ref()
            .map(|_| path.with_file_name("checkpoint-latest.json"));
        let thread = std::thread::spawn(move || {
            let mut checkpoints = None;
            loop {
                if !halt.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                let terminal = halt.load(Ordering::Relaxed);
                let previous = output.lock().unwrap().clone();
                let result = capture(
                    &first,
                    previous["samples"].as_u64().unwrap_or(0) + 1,
                    previous["peak_rss_bytes"].as_u64().unwrap_or(0),
                    previous["minimum_host_free_bytes"].as_u64().unwrap_or(0),
                    disk_observation,
                    control_scope,
                );
                let value = retain_observation(
                    previous,
                    result,
                    terminal,
                    std::process::id(),
                    super::utc_ms(),
                );
                *output.lock().unwrap() = value.clone();
                if super::write_json(&path, &value).is_err() {
                    output.lock().unwrap()["error"] = json!("resource receipt write failed");
                }
                if let (Some(setup), Some(path)) = (&checkpoint_control, &checkpoint_path) {
                    let configured = setup.lock().unwrap().is_some();
                    if configured {
                        checkpoints
                            .get_or_insert_with(super::checkpoints::Sampler::new)
                            .tick(setup, path, &value, terminal);
                    }
                }
                if terminal {
                    break;
                }
            }
        });
        Ok(Self {
            stop,
            current,
            thread: Some(thread),
            checkpoint_setup,
        })
    }
    /// Install only after the worker has verified its source and binary. The
    /// sampler stays unconfigured before this point, even with profiling on.
    pub fn install_checkpoints(
        &self,
        identity: super::checkpoints::Identity,
    ) -> Result<(), String> {
        let Some(setup) = &self.checkpoint_setup else {
            return Ok(());
        };
        if !identity.valid() || identity.pid != std::process::id() {
            return Err("checkpoint identity invalid".into());
        }
        let mut setup = setup.lock().unwrap();
        if setup.as_ref().is_some_and(|previous| {
            !identity.same_process(&previous.identity)
                || identity.generation < previous.identity.generation
        }) {
            return Err("checkpoint identity changed".into());
        }
        *setup = Some(super::checkpoints::Setup {
            identity,
            scope: "verified_worker_source_and_binary",
        });
        Ok(())
    }
    /// Real process observations and sampler lifecycle with a modeled disk
    /// observation. This private control is not host preflight qualification.
    #[cfg(test)]
    fn start_sampler_control(
        path: std::path::PathBuf,
        disk_observation: DiskObservation,
    ) -> Result<Self, String> {
        let resources = Self::start_observing_disk(
            path,
            disk_observation,
            Some("modeled_disk_only_sampler_lifecycle_control"),
        )?;
        if let Some(setup) = &resources.checkpoint_setup {
            *setup.lock().unwrap() = Some(super::checkpoints::Setup {
                identity: super::checkpoints::Identity {
                    pid: std::process::id(),
                    controller_pid: std::process::id(),
                    worker: 0,
                    generation: 0,
                    source_digest: "a".repeat(64),
                    binary_digest: "b".repeat(64),
                },
                scope: "modeled_identity_sampler_lifecycle_control",
            });
        }
        Ok(resources)
    }
    pub fn snapshot(&self) -> Value {
        self.current.lock().unwrap().clone()
    }
    pub fn check(&self) -> Result<(), String> {
        validate_sample(&self.snapshot(), std::process::id(), super::utc_ms())
    }
    pub async fn finish(&mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::Relaxed);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
            if std::time::Instant::now() >= deadline {
                return Err(
                    "resource sampler shutdown unproven after1s; OS observations may be blocked"
                        .into(),
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if let Some(t) = self.thread.take() {
            t.join().map_err(|_| "resource sampler panicked")?;
        }
        let result =
            validate_terminal_sample(&self.snapshot(), std::process::id(), super::utc_ms());
        // Conservative wall bound includes caller scheduling, completed join and validation.
        if std::time::Instant::now() >= deadline {
            return Err(
                "resource sampler completion observed after1s; shutdown acceptance incomplete"
                    .into(),
            );
        }
        result
    }
}
impl Drop for Resources {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.thread.as_ref().is_some_and(|t| t.is_finished())
            && let Some(t) = self.thread.take()
        {
            let _ = t.join();
        }
    }
}
#[cfg(test)]
pub fn example_sample(pid: u32) -> Value {
    json!({"pid":pid,"samples":1,"peak_rss_bytes":100,"minimum_host_free_bytes":DISK_FLOOR,"observed_unix_ms":super::utc_ms(),"error":null,"process_delta":{"rss_end_bytes":100,"lifetime_peak_rss_bytes":100,"cpu_user_us":0,"cpu_system_us":0}})
}
#[test]
fn prereview_red_resource_coverage_and_lifetime_peak() {
    assert!(validate_sample(&json!({}), 123, super::utc_ms()).is_err());
    let mut sample = example_sample(123);
    validate_sample(&sample, 123, super::utc_ms()).unwrap();
    sample["process_delta"]["lifetime_peak_rss_bytes"] = json!(RSS_CAP + 1);
    assert!(validate_sample(&sample, 123, super::utc_ms()).is_err());
}
#[tokio::test]
async fn readiness_has_synchronous_pid_cpu_rss_sample_and_sampler_stops() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("resources.json");
    let mut resources = Resources::start_sampler_control(path.clone(), || Ok(DISK_FLOOR)).unwrap();
    let initial = super::read_json(&path).unwrap();
    assert_eq!(
        initial["disk_observation_scope"],
        "modeled_disk_only_sampler_lifecycle_control"
    );
    assert_eq!(initial["minimum_host_free_bytes"], DISK_FLOOR);
    validate_sample(&initial, std::process::id(), super::utc_ms()).unwrap();
    resources.finish().await.unwrap();
    assert!(resources.thread.is_none());
    for field in ["cpu_user_us", "cpu_system_us", "rss_end_bytes"] {
        let mut value = example_sample(123);
        value["process_delta"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(validate_sample(&value, 123, super::utc_ms()).is_err());
    }
    let sample = example_sample(123);
    assert!(validate_sample(&sample, 124, super::utc_ms()).is_err());
    assert!(validate_sample(&sample, 123, super::utc_ms() + 10001).is_err());
}
#[tokio::test]
async fn immediate_stop_persists_final_os_sample_before_success() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("resources.json");
    let mut resources = Resources::start_sampler_control(path.clone(), || Ok(DISK_FLOOR)).unwrap();
    let initial = resources.snapshot();
    let stop_requested = super::utc_ms();
    resources.finish().await.unwrap();
    let final_sample = super::read_json(&path).unwrap();
    assert_eq!(
        final_sample["disk_observation_scope"],
        "modeled_disk_only_sampler_lifecycle_control"
    );
    assert_eq!(
        final_sample["terminal_sample"], true,
        "successful stop requires final sampler-thread OS capture"
    );
    assert!(final_sample["samples"].as_u64().unwrap() > initial["samples"].as_u64().unwrap());
    assert!(final_sample["observed_unix_ms"].as_u64().unwrap() >= stop_requested);
    assert_eq!(final_sample, resources.snapshot());
}

#[test]
fn final_sample_retains_cap_breach_and_sticky_error_without_large_allocation() {
    let initial = example_sample(123);
    let now = super::utc_ms();
    let mut over = initial.clone();
    over["process_delta"]["lifetime_peak_rss_bytes"] = json!(RSS_CAP + 1);
    over["peak_rss_bytes"] = json!(RSS_CAP + 1);
    let result = retain_observation(initial.clone(), Ok(over), true, 123, now);
    assert_eq!(result["peak_rss_bytes"], RSS_CAP + 1);
    assert!(validate_terminal_sample(&result, 123, now).is_err());
    let mut prior = initial.clone();
    prior["error"] = json!("prior observation failure");
    let result = retain_observation(prior, Ok(initial.clone()), true, 123, now);
    assert_eq!(result["terminal_sample"], true);
    assert_eq!(result["error"], "prior observation failure");
    assert!(validate_terminal_sample(&result, 123, now).is_err());
    let result = retain_observation(
        initial,
        Err("final OS sample failed".into()),
        true,
        123,
        now,
    );
    assert_eq!(result["terminal_sample"], false);
    assert!(validate_terminal_sample(&result, 123, now).is_err());
}
#[tokio::test]
async fn late_finish_poll_cannot_accept_completion_after_wall_budget() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("resources.json");
    let mut resources = Resources::start_sampler_control(path.clone(), || Ok(DISK_FLOOR)).unwrap();
    let mut finish = Box::pin(resources.finish());
    assert!(futures_util::poll!(finish.as_mut()).is_pending());
    // Deliberately stall this test's caller; the real owned sampler can finish.
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(super::read_json(&path).unwrap()["terminal_sample"], true);
    assert!(
        finish.await.is_err(),
        "late caller must fail closed even when sampler completed"
    );
}

#[tokio::test]
async fn sampler_control_rejects_low_or_unavailable_disk_before_publishing_or_spawning() {
    fn low_disk() -> Result<u64, String> {
        Ok(DISK_FLOOR - 1)
    }
    fn unavailable_disk() -> Result<u64, String> {
        Err("controlled disk observation unavailable".into())
    }
    let probes: [(DiskObservation, &str); 2] = [
        (low_disk, "host free disk below64GiB"),
        (unavailable_disk, "controlled disk observation unavailable"),
    ];
    for (probe, expected) in probes {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("resources.json");
        match Resources::start_sampler_control(path.clone(), probe) {
            Err(error) => assert_eq!(error, expected),
            Ok(mut resources) => {
                let _ = resources.finish().await;
                panic!("sampler admission accepted invalid disk observation");
            }
        }
        assert!(
            !path.exists(),
            "admission refusal precedes receipt publication and thread creation"
        );
    }
}

#[cfg(test)]
mod periodic_checkpoint_tests {
    use super::*;
    use mount_rs_core::diagnostics::{object_store, profile, storage};
    use std::path::Path;

    // Public modeled identity for the existing sampler-control seam. It is not
    // source/binary qualification. Production must install its verified identity
    // explicitly; merely enabling profiling must not enable worker checkpoints.
    const SOURCE_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BINARY_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const CHECKPOINT_LIMIT: usize = 512 * 1024;

    fn enabled() -> bool {
        // The root gate runs each positive control in a fresh process with
        // MOUNT_RS_PROFILE_IO=1. Ordinary disabled suites exercise the negative
        // control below and do not mutate a cached process profiling switch.
        profile::enabled() && storage::enabled()
    }

    async fn observe_after_samples(
        resources: &Resources,
        minimum_samples: u64,
        path: &Path,
    ) -> Result<Option<Value>, String> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                resources.check()?;
                if resources.snapshot()["samples"].as_u64().unwrap_or(0) >= minimum_samples {
                    return match std::fs::read(path) {
                        Ok(bytes) => {
                            if bytes.len() > CHECKPOINT_LIMIT {
                                return Err("checkpoint receipt exceeded fixed cap".into());
                            }
                            serde_json::from_slice(&bytes)
                                .map(Some)
                                .map_err(|_| "checkpoint publication exposed partial JSON".into())
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                        Err(_) => Err("checkpoint receipt read failed".into()),
                    };
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "owned sampler did not advance inside safety deadline".to_string())?
    }

    fn row<'a>(sample: &'a Value, family: &str, name: &str) -> &'a Value {
        sample[family]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == name)
            .expect("actual bank row absent")
    }

    fn assert_modeled_identity(sample: &Value) {
        assert_eq!(sample["schema"], "mount-rs.target-checkpoint.v1");
        assert_eq!(sample["observation"], "concurrent_partial");
        assert_eq!(sample["counter_scope"], "process_lifetime_cumulative");
        assert_eq!(sample["metrics_complete"], false);
        assert_eq!(
            sample["identity_scope"],
            "modeled_identity_sampler_lifecycle_control"
        );
        assert_eq!(sample["identity"]["pid"], std::process::id());
        assert_eq!(sample["identity"]["controller_pid"], std::process::id());
        assert_eq!(sample["identity"]["worker"], 0);
        assert_eq!(sample["identity"]["generation"], 0);
        assert_eq!(sample["identity"]["source_digest"], SOURCE_DIGEST);
        assert_eq!(sample["identity"]["binary_digest"], BINARY_DIGEST);
        assert!(sample["identity"]["sequence"].as_u64().unwrap() > 0);
        assert_eq!(sample["resources"]["pid"], std::process::id());
        for field in ["cpu_user_us", "cpu_system_us", "rss_end_bytes"] {
            assert!(
                sample["resources"]["process_delta"][field]
                    .as_u64()
                    .is_some()
            );
        }
        assert!(sample["resources"]["samples"].as_u64().unwrap() > 0);
        let started = sample["capture_started_unix_ms"].as_u64().unwrap();
        let completed = sample["capture_completed_unix_ms"].as_u64().unwrap();
        assert!(started > 0 && completed >= started);
        assert!(completed <= super::super::utc_ms());
    }

    #[tokio::test]
    async fn periodic_sampler_captures_actual_banks_while_storage_work_is_held() {
        if !enabled() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkpoint-latest.json");
        let mut completed = storage::Span::new(storage::Operation::TidbSqlInodeRead);
        completed.finish_success_with_rows(0, 17);
        profile::add(profile::Event::CompactNamespaceMaterializeNodes, 29);
        profile::add(profile::Event::InodeReturned, 313);
        let expected_core = serde_json::to_value(profile::snapshot()).unwrap();
        let mut held = storage::Span::new(storage::Operation::TidbSqlInodeRead);
        let expected_storage = serde_json::to_value(storage::snapshot()).unwrap();
        let object_bank = object_store::Observer::enabled();
        let client = object_bank.client(object_store::ClientRole::StandaloneProbe);
        // This records local observer activity only; no HTTP request is issued.
        drop(
            client
                .attempt(object_store::HttpMethod::Head, None)
                .headers(200),
        );
        let expected_object = object_bank.snapshot().unwrap();
        let mut resources =
            Resources::start_sampler_control(root.path().join("resources.json"), || Ok(DISK_FLOOR))
                .unwrap();
        // At least eleven completed 100ms intervals give the existing sampler
        // a due one-second checkpoint while the real storage span remains open.
        let observation = observe_after_samples(&resources, 12, &path).await;
        let shutdown = resources.finish().await;
        held.finish_success_with_rows(0, 0);
        drop(client);
        // All actual sampler ownership is settled before the expected RED.
        shutdown.unwrap();
        assert!(resources.thread.is_none());
        let sample = observation
            .unwrap()
            .expect("owned sampler must publish a periodic checkpoint before phase completion");
        assert_modeled_identity(&sample);
        assert_eq!(sample["core"]["entries"].as_array().unwrap().len(), 136);
        let storage_rows = sample["storage"]["entries"].as_array().unwrap();
        assert_eq!(storage_rows.len(), 118);
        assert_eq!(
            storage_rows[116]["name"],
            "sdk.metadata.compact_root_file_capability"
        );
        assert_eq!(
            storage_rows[117]["name"],
            "sdk.metadata.load_compact_root_file"
        );
        let expected = json!({"core":expected_core,"storage":expected_storage});
        for name in [
            "compact.namespace.materialize_nodes",
            "provider.inode_returned_bytes",
        ] {
            assert!(
                row(&sample, "core", name)["units"].as_u64().unwrap()
                    >= row(&expected, "core", name)["units"].as_u64().unwrap()
            );
        }
        let sql = row(&sample, "storage", "tidb.sql.inode_read");
        let baseline_sql = row(&expected, "storage", "tidb.sql.inode_read");
        assert!(
            sql["returned_rows"].as_u64().unwrap()
                >= baseline_sql["returned_rows"].as_u64().unwrap()
        );
        assert!(
            sql["returned_row_observations"].as_u64().unwrap()
                >= baseline_sql["returned_row_observations"].as_u64().unwrap()
        );
        assert!(sql["in_flight"].as_u64().unwrap() > 0);
        assert!(sample["storage"]["in_flight"].as_u64().unwrap() > 0);
        let records = sample["object_store_observation"]["records"]
            .as_array()
            .unwrap();
        let bytes = records
            .iter()
            .map(|record| record.as_str().unwrap())
            .collect::<String>();
        let decoded = mount_rs_service::object_store_diagnostics::decode(bytes.as_bytes()).unwrap();
        assert_eq!(
            records.len(),
            mount_rs_service::object_store_diagnostics::FRAME_COUNT
        );
        assert_eq!(decoded.capture().pid, std::process::id());
        assert_eq!(
            decoded.capture().sequence,
            sample["identity"]["sequence"].as_u64().unwrap()
        );
        assert_eq!(decoded.capture().generation, Some(0));
        assert_eq!(
            decoded.capture().context,
            mount_rs_service::object_store_diagnostics::CaptureContext::Periodic
        );
        let role = object_store::ClientRole::StandaloneProbe.index();
        let method = object_store::HttpMethod::Head.index();
        assert!(
            decoded.snapshot().clients[role].http[method].attempts_started
                >= expected_object.clients[role].http[method].attempts_started
        );
        assert!(
            decoded.snapshot().clients[role].http[method].body_dropped
                >= expected_object.clients[role].http[method].body_dropped
        );
        assert!(sample.get("delta_from_previous").is_none());
        assert!(sample.get("quiescent_phase_delta").is_none());
    }

    #[tokio::test]
    async fn periodic_sampler_replaces_latest_with_advancing_cumulative_checkpoint() {
        if !enabled() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkpoint-latest.json");
        let mut resources =
            Resources::start_sampler_control(root.path().join("resources.json"), || Ok(DISK_FLOOR))
                .unwrap();
        let first = observe_after_samples(&resources, 12, &path).await;
        profile::add(profile::Event::CompactNamespaceMaterializeNodes, 23);
        let mut operation = storage::Span::new(storage::Operation::TidbSqlInodeRead);
        operation.finish_success_with_rows(0, 31);
        let second = observe_after_samples(&resources, 24, &path).await;
        let shutdown = resources.finish().await;
        shutdown.unwrap();
        assert!(resources.thread.is_none());
        let first = first.unwrap().expect("first periodic checkpoint absent");
        let second = second.unwrap().expect("second periodic checkpoint absent");
        assert_modeled_identity(&first);
        assert_modeled_identity(&second);
        assert!(
            second["identity"]["sequence"].as_u64().unwrap()
                > first["identity"]["sequence"].as_u64().unwrap()
        );
        assert!(
            second["resources"]["samples"].as_u64().unwrap()
                > first["resources"]["samples"].as_u64().unwrap()
        );
        let first_nodes = row(&first, "core", "compact.namespace.materialize_nodes")["units"]
            .as_u64()
            .unwrap();
        let second_nodes = row(&second, "core", "compact.namespace.materialize_nodes")["units"]
            .as_u64()
            .unwrap();
        assert!(second_nodes.checked_sub(first_nodes).unwrap() >= 23);
        let first_rows = row(&first, "storage", "tidb.sql.inode_read")["returned_rows"]
            .as_u64()
            .unwrap();
        let second_rows = row(&second, "storage", "tidb.sql.inode_read")["returned_rows"]
            .as_u64()
            .unwrap();
        assert!(second_rows.checked_sub(first_rows).unwrap() >= 31);
        let mut files = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        files.sort();
        assert_eq!(files, ["checkpoint-latest.json", "resources.json"]);
        assert!(std::fs::metadata(path).unwrap().len() <= CHECKPOINT_LIMIT as u64);
    }

    #[tokio::test]
    async fn unconfigured_sampler_never_publishes_a_worker_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkpoint-latest.json");
        // The same production sampler start path, with disk observation modeled
        // to avoid imposing host qualification on this lifecycle-only control.
        // No verified or modeled checkpoint identity has been installed.
        let mut resources = Resources::start_observing_disk(
            root.path().join("resources.json"),
            || Ok(DISK_FLOOR),
            None,
        )
        .unwrap();
        let observation = observe_after_samples(&resources, 12, &path).await;
        let shutdown = resources.finish().await;
        shutdown.unwrap();
        assert!(resources.thread.is_none());
        assert!(observation.unwrap().is_none());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn profiling_off_ignores_even_an_installed_checkpoint_identity() {
        if enabled() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkpoint-latest.json");
        let mut resources = Resources::start_observing_disk(
            root.path().join("resources.json"),
            || Ok(DISK_FLOOR),
            None,
        )
        .unwrap();
        let installed = resources.install_checkpoints(super::super::checkpoints::Identity {
            pid: std::process::id(),
            controller_pid: std::process::id(),
            worker: 0,
            generation: 0,
            source_digest: SOURCE_DIGEST.into(),
            binary_digest: BINARY_DIGEST.into(),
        });
        let shutdown = resources.finish().await;
        shutdown.unwrap();
        installed.unwrap();
        assert!(resources.thread.is_none());
        assert!(resources.checkpoint_setup.is_none());
        assert!(!path.exists());
        assert!(!path.with_extension("pending").exists());
    }
}
