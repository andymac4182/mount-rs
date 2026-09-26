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

pub struct Resources {
    stop: Arc<AtomicBool>,
    current: Arc<Mutex<Value>>,
    thread: Option<std::thread::JoinHandle<()>>,
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
) -> Result<Value, String> {
    let snapshot = super::resource_profile::Snapshot::capture_process()
        .map_err(|_| "process observation failed")?;
    let mut delta = snapshot.delta(first).map_err(|_| "process delta failed")?;
    delta["quic_client_side"] =
        json!({"available":false,"reason":"process-only sampler; no transport observation"});
    let current = snapshot.resident_bytes().ok_or("current RSS unavailable")?;
    let lifetime = delta["lifetime_peak_rss_bytes"]
        .as_u64()
        .ok_or("lifetime RSS unavailable")?;
    let value = json!({"pid":std::process::id(),"samples":samples,"peak_rss_bytes":previous_peak.max(current).max(lifetime),"minimum_host_free_bytes":previous_disk.min(disk_available()?),"error":null,"process_delta":delta,"sample_interval_ms":100,"observed_unix_ms":super::utc_ms()});
    Ok(value)
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
        let first = super::resource_profile::Snapshot::capture_process()
            .map_err(|_| "resource baseline unavailable")?;
        let initial = capture(&first, 1, 0, u64::MAX)?;
        validate_sample(&initial, std::process::id(), super::utc_ms())?;
        super::write_json(&path, &initial)?;
        let stop = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(initial));
        let halt = stop.clone();
        let output = current.clone();
        let thread = std::thread::spawn(move || {
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
                if terminal {
                    break;
                }
            }
        });
        Ok(Self {
            stop,
            current,
            thread: Some(thread),
        })
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
    let mut resources = Resources::start(path.clone()).unwrap();
    validate_sample(
        &super::read_json(&path).unwrap(),
        std::process::id(),
        super::utc_ms(),
    )
    .unwrap();
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
    let mut resources = Resources::start(path.clone()).unwrap();
    let initial = resources.snapshot();
    let stop_requested = super::utc_ms();
    resources.finish().await.unwrap();
    let final_sample = super::read_json(&path).unwrap();
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
    let mut resources = Resources::start(path.clone()).unwrap();
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
