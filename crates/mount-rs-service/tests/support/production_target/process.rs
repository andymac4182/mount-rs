use super::{
    backend::Backend,
    config::{Config, SERVERS},
    fixture::target_catalog,
};
use mount_rs_service::{
    auth::{AuthError, CatalogAuthenticator, Jwk, OidcKeySource, OidcVerifier},
    catalog::SqliteCatalog,
    dispatch::DriveDispatcher,
    server::{RemoteServer, RemoteServerOptions, RemoteTransferLimits},
    startup::{
        Identity as StartupIdentity, Snapshot as StartupSnapshot, Stage as StartupStage, Startup,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
#[derive(Serialize, Deserialize)]
pub struct PrivateConfig {
    pub config: Config,
    pub backend: Backend,
    pub catalog: PathBuf,
    pub catalog_digest: String,
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
    pub jwk: Jwk,
    pub source_digest: String,
    pub binary_digest: String,
    pub output: PathBuf,
    pub parent_pid: u32,
    pub expected_backings: Vec<String>,
    pub expected_backings_sha256: String,
}
struct Keys(Jwk);
#[async_trait::async_trait]
impl OidcKeySource for Keys {
    async fn fetch(&self, issuer: &str, audiences: &[String]) -> Result<OidcVerifier, AuthError> {
        OidcVerifier::new(issuer, audiences, vec![self.0.clone()])
    }
}
#[derive(Serialize, Deserialize, Clone)]
pub struct Ready {
    pub pid: u32,
    pub server: usize,
    pub generation: u64,
    pub address: std::net::SocketAddr,
    pub catalog_digest: String,
    pub source_digest: String,
    pub binary_digest: String,
    pub backend_prefix: String,
    pub mode: String,
    pub schema: String,
    pub planned_drives: usize,
    pub registered_drives: usize,
    pub max_active_drives: usize,
    pub expected_backings_sha256: String,
    pub runtime_activation: Value,
    pub resources: Value,
    pub core_profile: Value,
    pub phase_metrics: Value,
    #[serde(default)]
    pub startup_diagnostics: Option<Value>,
}
pub struct OwnedChild {
    pub child: Child,
    pub server: usize,
    pub ready: Option<Ready>,
    pub exited: Option<i32>,
    pub reaped: bool,
    pub signal: Option<i32>,
    pub forced: bool,
    pub root: PathBuf,
}
pub struct Fleet {
    pub children: Vec<OwnedChild>,
    initial_backings: Option<Vec<String>>,
    startup_complete: bool,
    startup_next: Option<Instant>,
    startup_seen: [Option<(u64, u64, u64)>; SERVERS],
    resource_expected: [bool; SERVERS],
    resource_terminal_accepted: [bool; SERVERS],
    resource_generation: Option<u64>,
    resource_generation_seen: [Option<u64>; SERVERS],
}
impl Fleet {
    pub fn expect_initialized_backings(&mut self, values: Vec<String>) {
        self.initial_backings = Some(values);
    }
    pub(super) fn initialized_backings(&self) -> Result<&[String], String> {
        self.initial_backings
            .as_deref()
            .ok_or_else(|| "initializer backing baseline absent".into())
    }
    pub fn new() -> Self {
        Self {
            children: vec![],
            initial_backings: None,
            startup_complete: true,
            startup_next: None,
            startup_seen: [None; SERVERS],
            resource_expected: [false; SERVERS],
            resource_terminal_accepted: [false; SERVERS],
            resource_generation: None,
            resource_generation_seen: [None; SERVERS],
        }
    }
    pub fn launch(&mut self, private: &Path, output: &Path, index: usize) -> Result<(), String> {
        let root = output.join(format!("worker-{index}"));
        std::fs::create_dir(&root).map_err(|_| "worker directory exists or unavailable")?;
        let out =
            std::fs::File::create(root.join("worker.log")).map_err(|_| "worker log failed")?;
        let child = Command::new(std::env::current_exe().map_err(|_| "binary path unavailable")?)
            .args([
                "--ignored",
                "--exact",
                "production_target_worker",
                "--nocapture",
            ])
            .env("MOUNT_RS_TARGET_PRIVATE_CONFIG", private)
            .env("MOUNT_RS_TARGET_WORKER", index.to_string())
            .stdout(Stdio::from(
                out.try_clone().map_err(|_| "log clone failed")?,
            ))
            .stderr(Stdio::from(out))
            .stdin(Stdio::null())
            .spawn()
            .map_err(|_| "worker launch failed")?;
        self.children.push(OwnedChild {
            child,
            server: index,
            ready: None,
            exited: None,
            reaped: false,
            signal: None,
            forced: false,
            root,
        });
        Ok(())
    }
    pub fn check(&mut self) -> Result<(), String> {
        for c in &mut self.children {
            if let Some(status) = c.child.try_wait().map_err(|_| "worker wait failed")? {
                c.record_status(status);
                return Err(format!("owned worker {} exited before shutdown", c.server));
            }
            let path = c.root.join("resources.json");
            if path.exists() {
                let r = super::read_json(&path)?;
                super::resources::validate_sample(&r, c.child.id(), super::utc_ms())?;
            } else if c.ready.is_some() {
                return Err("ready worker resource coverage missing".into());
            }
        }
        Ok(())
    }
    pub async fn ready(
        &mut self,
        private: &PrivateConfig,
        generation: u64,
        progress: &mut super::progress::Progress,
        resources: &super::resources::Resources,
    ) -> Result<(), String> {
        self.resource_generation = Some(generation);
        let end = Instant::now() + Duration::from_secs(600);
        loop {
            if mount_rs_core::diagnostics::profile::enabled()
                && self.project_startup(private, generation)
            {
                super::project_resource_progress(self, resources, progress, true);
            }
            self.check()?;
            let mut complete = 0;
            for c in &mut self.children {
                let path = c.root.join("ready.json");
                if !path.exists() {
                    continue;
                }
                let r: Ready = serde_json::from_value(super::read_json(&path)?)
                    .map_err(|_| "invalid worker readiness")?;
                if r.generation < generation {
                    continue;
                }
                validate_ready(&r, c.child.id(), c.server, generation, private)?;
                if mount_rs_core::diagnostics::profile::enabled() {
                    self.startup_complete &= ready_startup_complete(&r, private.config.drives);
                }
                self.resource_expected[c.server] = true;
                c.ready = Some(r);
                complete += 1;
            }
            if complete == SERVERS {
                let mut endpoints = std::collections::BTreeSet::new();
                let mut pids = std::collections::BTreeSet::new();
                let backings = expected_backings(private)?;
                for child in &self.children {
                    let r = child.ready.as_ref().unwrap();
                    if !endpoints.insert(r.address)
                        || !pids.insert(r.pid)
                        || r.expected_backings_sha256 != private.expected_backings_sha256
                    {
                        return Err("worker endpoint/PID/backing consistency mismatch".into());
                    }
                    let sample = super::read_json(&child.root.join("resources.json"))?;
                    super::resources::validate_sample(&sample, child.child.id(), super::utc_ms())?;
                }
                if self.initial_backings.as_ref() != Some(&backings) {
                    return Err("backing identity changed across replica generations".into());
                }
                self.initial_backings = Some(backings);
                return Ok(());
            }
            if Instant::now() >= end {
                return Err("worker readiness timeout".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub fn startup_accounting_complete(&self) -> bool {
        self.startup_complete
    }
    fn project_startup(&mut self, private: &PrivateConfig, generation: u64) -> bool {
        let now = Instant::now();
        if self.startup_next.is_some_and(|next| now < next) {
            return false;
        }
        self.startup_next = Some(now + mount_rs_service::startup::PROGRESS_INTERVAL);
        for child in &self.children {
            let path = child
                .root
                .join(format!("startup-progress-g{generation}.json"));
            let bytes = match read_startup_file(&path) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue, // Early configuration has not yet learned its owned output directory.
                Err(_) => {
                    self.startup_complete = false;
                    continue;
                }
            };
            let mut snapshot = match owned_startup(
                &bytes,
                child.child.id(),
                child.server,
                generation,
                private.config.drives,
            ) {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    self.startup_complete = false;
                    continue;
                }
            };
            // Running observations must be no older than three publication periods,
            // with at most one second of future wall-clock skew. Ready is historical.
            let wall = super::utc_ms();
            if snapshot.terminal_outcome == mount_rs_service::startup::Outcome::Running
                && (snapshot.observed_unix_ms > wall.saturating_add(1000)
                    || wall.saturating_sub(snapshot.observed_unix_ms) > 15_000)
            {
                snapshot.accounting_complete = false;
            }
            self.startup_complete &= snapshot.accounting_complete;
            // Configuration includes the sampler and source/binary checks. This remains
            // true across reopen, because its sampler baseline belongs to this Child.
            self.resource_expected[child.server] |=
                snapshot.stages[StartupStage::Configuration as usize].success >= 4;
            let key = (
                snapshot.generation,
                snapshot.observed_unix_ms,
                snapshot.elapsed_ns,
            );
            if self.startup_seen[child.server] != Some(key) {
                if Startup::write_record(&mut std::io::stderr().lock(), &snapshot).is_err() {
                    self.startup_complete = false;
                } else {
                    self.resource_generation_seen[child.server] = Some(snapshot.generation);
                }
                self.startup_seen[child.server] = Some(key);
            }
        }
        true
    }
    pub fn project_resources(&mut self, progress: &mut super::progress::Progress) {
        self.project_resources_with(progress, |progress, owner, root| {
            progress.resource(owner, || read_resource_file(&root.join("resources.json")))
        });
        self.project_checkpoints_with(progress, |progress, owner, root| {
            progress.checkpoint(owner, || {
                super::checkpoints::read_latest(&root.join("checkpoint-latest.json"))
            });
        });
    }
    fn project_checkpoints_with(
        &self,
        progress: &mut super::progress::Progress,
        mut project: impl FnMut(&mut super::progress::Progress, super::progress::ResourceOwner, &Path),
    ) {
        for child in &self.children {
            if self.resource_expected[child.server] && !child.reaped {
                project(progress, self.checkpoint_owner(child), &child.root);
            }
        }
    }
    fn checkpoint_owner(&self, child: &OwnedChild) -> super::progress::ResourceOwner {
        let mut owner = self.resource_owner(child);
        if owner.generation_context.is_none() {
            // Fleet::ready retains validated receipts independently of the
            // throttled startup emission that owns legacy resource context.
            owner.generation_context = child.ready.as_ref().and_then(|ready| {
                (ready.pid == child.child.id()
                    && ready.server == child.server
                    && Some(ready.generation) == self.resource_generation)
                    .then_some(ready.generation)
            });
        }
        owner
    }
    fn resource_owner(&self, child: &OwnedChild) -> super::progress::ResourceOwner {
        let context = self.resource_generation_seen[child.server]
            .filter(|generation| Some(*generation) == self.resource_generation);
        super::progress::ResourceOwner {
            pid: child.child.id(),
            worker: Some(child.server),
            generation_context: context,
        }
    }
    fn project_resources_with(
        &mut self,
        progress: &mut super::progress::Progress,
        mut project: impl FnMut(
            &mut super::progress::Progress,
            super::progress::ResourceOwner,
            &Path,
        ) -> bool,
    ) {
        if !progress.resources_due() {
            return;
        }
        for child in &self.children {
            if !self.resource_expected[child.server]
                || child.reaped
                || self.resource_terminal_accepted[child.server]
            {
                continue; // Unconfigured or completed observation coverage is not a live sample.
            }
            let accepted = project(progress, self.resource_owner(child), &child.root);
            self.resource_terminal_accepted[child.server] |= accepted;
        }
    }
    fn project_reaped_resource(&mut self, index: usize, progress: &mut super::progress::Progress) {
        let child = &self.children[index];
        if !self.resource_expected[child.server] || self.resource_terminal_accepted[child.server] {
            return;
        }
        progress.resource_boundary();
        let accepted = progress.terminal_resource(self.resource_owner(child), || {
            read_resource_file(&child.root.join("resources.json"))
        });
        self.resource_terminal_accepted[child.server] |= accepted;
        progress.resources_done();
    }
    pub fn command(&self, command: &str, generation: u64) -> Result<(), String> {
        for c in &self.children {
            super::write_json(
                &c.root.join("command.json"),
                &json!({
                        "command":command,
                        "generation":generation}
                ),
            )?;
        }
        Ok(())
    }
    pub async fn cleanup(
        &mut self,
        resources: Option<&super::resources::Resources>,
        progress: &mut super::progress::Progress,
    ) -> Vec<Value> {
        self.cleanup_observed(
            Duration::from_secs(90),
            Duration::from_secs(95),
            resources,
            progress,
        )
        .await
    }
    #[cfg(test)]
    async fn cleanup_bounded(&mut self, grace: Duration, total: Duration) -> Vec<Value> {
        self.cleanup_observed(
            grace,
            total,
            None,
            &mut super::progress::Progress::disabled(),
        )
        .await
    }
    async fn cleanup_observed(
        &mut self,
        grace: Duration,
        total: Duration,
        resources: Option<&super::resources::Resources>,
        progress: &mut super::progress::Progress,
    ) -> Vec<Value> {
        let started = Instant::now();
        let force_at = started + grace.min(total);
        let deadline = started + total;
        let mut errors = Vec::new();
        for c in &self.children {
            if super::write_json(
                &c.root.join("command.json"),
                &json!({"command":"stop","generation":u64::MAX}),
            )
            .is_err()
            {
                errors.push(json!({"server":c.server,"error":"shutdown command failed"}));
            }
        }
        for index in 0..self.children.len() {
            if self.children[index].reaped {
                self.project_reaped_resource(index, progress);
            }
        }
        loop {
            for index in 0..self.children.len() {
                let c = &mut self.children[index];
                let mut newly_reaped = false;
                if !c.reaped {
                    match c.child.try_wait() {
                        Ok(Some(status)) => {
                            c.record_status(status);
                            newly_reaped = true;
                        }
                        Ok(None) => {}
                        Err(_) => {}
                    }
                }
                if newly_reaped {
                    // Capture required coverage at the real owned reap boundary, before
                    // later terminal/oracle work can age a completed sampler receipt.
                    self.project_reaped_resource(index, progress);
                }
            }
            if self.children.iter().all(|c| c.reaped) {
                break;
            }
            let cleanup_tick = Instant::now();
            if cleanup_tick >= force_at {
                for c in &mut self.children {
                    if !c.reaped && !c.forced {
                        c.forced = true;
                        let killed = c.child.kill();
                        errors.push(json!({"server":c.server,"pid":c.child.id(),"error":"forced termination; drain unproven","kill_succeeded":killed.is_ok()}));
                    }
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            // Reuse the established public 5s cadence and this cleanup's existing clock.
            progress.resource_tick(cleanup_tick);
            if progress.resources_due() {
                if let Some(resources) = resources {
                    progress.resource(
                        super::progress::ResourceOwner {
                            pid: std::process::id(),
                            worker: None,
                            generation_context: None,
                        },
                        || Ok(Some(resources.snapshot())),
                    );
                }
                self.project_resources(progress);
                progress.resources_done();
            }
            tokio::time::sleep(
                Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
            )
            .await;
        }
        for c in &self.children {
            if !c.reaped {
                errors.push(json!({"server":c.server,"pid":c.child.id(),"error":"reap unproven after bounded cleanup; process may remain"}));
            }
            let terminal = super::read_json(&c.root.join("terminal.json")).unwrap_or(Value::Null);
            if mount_rs_core::diagnostics::profile::enabled() {
                self.startup_complete &= c
                    .ready
                    .as_ref()
                    .is_some_and(|ready| terminal_startup_complete(&terminal, ready));
            }
            if !c.reaped || c.exited != Some(0) || terminal["clean"] != true {
                errors.push(json!({"server":c.server,"error":"child exit or cleanup not clean","terminal":terminal}));
            }
        }
        errors
    }
    pub fn receipts(&self) -> Vec<Value> {
        self.children
            .iter()
            .map(|c| {
                json!({
                        "pid":c.child.id(),
                        "server":c.server,
                        "ready":c.ready,
                        "exit_code":c.exited,
                        "reap_confirmed":c.reaped,"exit_signal":c.signal,
                        "forced":c.forced,
                        "terminal":super::read_json(&c.root.join("terminal.json")).ok(),
                        "resources":super::read_json(&c.root.join("resources.json")).ok()}
                )
            })
            .collect()
    }
}
// The process-only sampler receipt has a fixed shape (no per-client/device rows).
// 16KiB leaves ample headroom for all u64 fields while bounding hostile observations.
const RESOURCE_RECEIPT_LIMIT: usize = 16 * 1024;
fn read_resource_file(path: &Path) -> Result<Option<Value>, String> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("resource receipt invalid".into()),
    };
    if !file
        .metadata()
        .map_err(|_| "resource receipt invalid")?
        .is_file()
    {
        return Err("resource receipt invalid".into());
    }
    let mut bytes = Vec::with_capacity(RESOURCE_RECEIPT_LIMIT + 1);
    file.take((RESOURCE_RECEIPT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "resource receipt invalid")?;
    if bytes.len() > RESOURCE_RECEIPT_LIMIT {
        return Err("resource receipt invalid".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "resource receipt invalid".into())
}
#[test]
fn checkpoint_projection_uses_matching_ready_before_startup_emission() {
    let root = tempfile::tempdir().unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", "read ignored || true"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut fleet = Fleet::new();
    fleet.children.push(OwnedChild {
        child,
        server: 0,
        ready: None,
        exited: None,
        reaped: false,
        signal: None,
        forced: false,
        root: root.path().to_owned(),
    });
    let mut progress = super::progress::Progress::disabled();
    let mut records = Vec::new();
    let mut capture = |label, fleet: &Fleet| {
        fleet.project_checkpoints_with(&mut progress, |_, owner, path| {
            records.push((
                label,
                owner.pid,
                owner.worker,
                owner.generation_context,
                path == root.path(),
            ));
        });
    };
    capture("unconfigured", &fleet);
    fleet.resource_expected[0] = true;
    fleet.resource_generation = Some(0);
    // Model a fast Ready receipt already retained by Fleet::ready while the
    // independent startup progress throttle has not emitted any generation.
    fleet.startup_next = Some(Instant::now() + Duration::from_secs(5));
    capture("missing_ready", &fleet);
    let mut ready = example_ready();
    ready.pid = pid;
    ready.resources = super::resources::example_sample(pid);
    fleet.children[0].ready = Some(ready.clone());
    capture("ready_g0", &fleet);
    let legacy_g0 = fleet.resource_owner(&fleet.children[0]).generation_context;
    fleet.resource_generation = Some(1);
    capture("old_ready_after_reopen", &fleet);
    ready.generation = 1;
    ready.runtime_activation = super::metrics::example_runtime(1, 10);
    fleet.children[0].ready = Some(ready.clone());
    capture("ready_g1", &fleet);
    let legacy_g1 = fleet.resource_owner(&fleet.children[0]).generation_context;
    fleet.children[0].ready.as_mut().unwrap().pid = pid.wrapping_add(1);
    capture("foreign_pid", &fleet);
    fleet.children[0].ready = Some(ready.clone());
    fleet.children[0].ready.as_mut().unwrap().server = 1;
    capture("foreign_server", &fleet);
    fleet.children[0].ready = Some(ready);
    fleet.resource_generation = None;
    capture("no_requested_generation", &fleet);
    fleet.resource_generation = Some(1);
    fleet.resource_generation_seen[0] = Some(1);
    fleet.children[0].ready = None;
    capture("startup_context", &fleet);
    let startup_accounting = (fleet.startup_seen[0], fleet.resource_generation_seen[0]);
    // Always settle the actual owned child before the expected RED assertion.
    fleet.children[0].child.stdin.take();
    let status = fleet.children[0].child.wait().unwrap();
    fleet.children[0].record_status(status);
    capture("reaped", &fleet);
    assert!(fleet.children[0].reaped);
    assert_eq!(legacy_g0, None);
    assert_eq!(legacy_g1, None);
    assert_eq!(startup_accounting, (None, Some(1)));
    assert_eq!(
        records,
        vec![
            ("missing_ready", pid, Some(0), None, true),
            ("ready_g0", pid, Some(0), Some(0), true),
            ("old_ready_after_reopen", pid, Some(0), None, true),
            ("ready_g1", pid, Some(0), Some(1), true),
            ("foreign_pid", pid, Some(0), None, true),
            ("foreign_server", pid, Some(0), None, true),
            ("no_requested_generation", pid, Some(0), None, true),
            ("startup_context", pid, Some(0), Some(1), true),
        ]
    );
}
#[test]
fn resource_progress_owned_expectation_and_generation_context_do_not_reset_sampler() {
    let root = tempfile::tempdir().unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", "read ignored || true"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut fleet = Fleet::new();
    fleet.children.push(OwnedChild {
        child,
        server: 0,
        ready: None,
        exited: None,
        reaped: false,
        signal: None,
        forced: false,
        root: root.path().to_path_buf(),
    });
    let mut progress = super::progress::Progress::new(
        true,
        &Config {
            full_target: false,
            drives: 10,
            files: 2,
            seconds: 1,
            population_seconds: super::config::PHASE_SECONDS,
            provider: "sqlite".into(),
        },
    );
    progress.source(&json!({"revision":"a".repeat(40),"checkout_status":"","digest":"b".repeat(64),"binary_sha256":"c".repeat(64)}));
    fleet.project_resources_with(&mut progress, |_, _, _| {
        panic!("sampler not yet configured")
    });
    fleet.resource_expected[0] = true;
    fleet.resource_generation = Some(0);
    fleet.resource_generation_seen[0] = Some(0);
    let mut records = Vec::new();
    fleet.project_resources_with(&mut progress, |_, owner, path| {
        assert_eq!(path, root.path());
        records.push((owner.pid, owner.worker, owner.generation_context));
        false
    });
    fleet.resource_generation = Some(1);
    fleet.project_resources_with(&mut progress, |_, owner, _| {
        records.push((owner.pid, owner.worker, owner.generation_context));
        false
    });
    fleet.resource_generation_seen[0] = Some(1);
    fleet.project_resources_with(&mut progress, |_, owner, _| {
        records.push((owner.pid, owner.worker, owner.generation_context));
        false
    });
    assert_eq!(
        records,
        vec![
            (pid, Some(0), Some(0)),
            (pid, Some(0), None),
            (pid, Some(0), Some(1))
        ]
    );
    assert!(fleet.resource_expected[0]);
    fleet.project_resources_with(&mut progress, |_, _, _| true);
    assert!(fleet.resource_terminal_accepted[0]);
    fleet.project_resources_with(&mut progress, |_, _, _| {
        panic!("accepted terminal is historical even while Child remains live")
    });
    let mut disabled = super::progress::Progress::disabled();
    fleet.project_resources_with(&mut disabled, |_, _, _| {
        panic!("disabled worker source read")
    });
    fleet.children[0].child.stdin.take();
    let status = fleet.children[0].child.wait().unwrap();
    fleet.children[0].record_status(status);
    assert!(fleet.children[0].reaped);
    fleet.project_resources_with(&mut progress, |_, _, _| {
        panic!("reaped worker must not be reread after delayed terminal work")
    });
    progress.finish(false, false);
}
#[tokio::test]
async fn resource_progress_cleanup_captures_owned_terminal_once_and_preserves_live_stale_failures()
{
    for pre_recorded_reap in [false, true] {
        for terminal in [Some(true), Some(false), None] {
            let root = tempfile::tempdir().unwrap();
            let child = Command::new("/bin/sh")
                .args(["-c", "read ignored || true"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let pid = child.id();
            let mut fleet = Fleet::new();
            fleet.children.push(OwnedChild {
                child,
                server: 0,
                ready: None,
                exited: None,
                reaped: false,
                signal: None,
                forced: false,
                root: root.path().to_owned(),
            });
            fleet.resource_expected[0] = true;
            let mut progress = super::progress::Progress::new(
                true,
                &Config {
                    full_target: false,
                    drives: 10,
                    files: 2,
                    seconds: 1,
                    population_seconds: super::config::PHASE_SECONDS,
                    provider: "sqlite".into(),
                },
            );
            progress.source(&json!({"revision":"a".repeat(40),"checkout_status":"","digest":"b".repeat(64),"binary_sha256":"c".repeat(64)}));
            progress.cleanup_context();
            let path = root.path().join("resources.json");
            let mut sample = json!({"pid":pid,"samples":2,"sample_interval_ms":100,
            "observed_unix_ms":super::utc_ms(),"terminal_sample":terminal,
            "peak_rss_bytes":4096,"minimum_host_free_bytes":super::config::DISK_FLOOR,"error":null,
            "process_delta":{"cpu_user_us":3,"cpu_system_us":2,"rss_end_bytes":2048,
                "lifetime_peak_rss_bytes":4096,"block_inputs":5,"block_outputs":7}});
            super::write_json(&path, &sample).unwrap();
            // Exercise both an earlier owned reap and cleanup's actual try_wait reap.
            fleet.children[0].child.stdin.take();
            if pre_recorded_reap {
                let status = fleet.children[0].child.wait().unwrap();
                fleet.children[0].record_status(status);
            } else {
                assert!(!fleet.children[0].reaped);
                assert_eq!(fleet.children[0].exited, None);
            }
            let errors = fleet
                .cleanup_observed(
                    Duration::from_secs(1),
                    Duration::from_secs(2),
                    None,
                    &mut progress,
                )
                .await;
            assert!(fleet.children[0].reaped);
            assert!(
                !errors.is_empty(),
                "resource observation does not forge private clean/drain evidence"
            );
            assert_eq!(fleet.resource_terminal_accepted[0], terminal == Some(true));
            assert_eq!(progress.complete(), terminal == Some(true));
            if terminal == Some(true) {
                // Model >10s later terminal/oracle work by aging the persisted sample.
                sample["observed_unix_ms"] = json!(super::utc_ms() - 10001);
                super::write_json(&path, &sample).unwrap();
                progress.resource_boundary();
                fleet.project_resources_with(&mut progress, |_, _, _| {
                    panic!("accepted reaped terminal reread")
                });
                assert!(progress.complete());
            }
            progress.finish(false, false);
        }
    }
    // A still-live worker with an old observation must remain a real failure.
    let root = tempfile::tempdir().unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", "read ignored || true"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut fleet = Fleet::new();
    let pid = child.id();
    fleet.children.push(OwnedChild {
        child,
        server: 0,
        ready: None,
        exited: None,
        reaped: false,
        signal: None,
        forced: false,
        root: root.path().to_owned(),
    });
    fleet.resource_expected[0] = true;
    let mut progress = super::progress::Progress::new(
        true,
        &Config {
            full_target: false,
            drives: 10,
            files: 2,
            seconds: 1,
            population_seconds: super::config::PHASE_SECONDS,
            provider: "sqlite".into(),
        },
    );
    progress.source(&json!({"revision":"a".repeat(40),"checkout_status":"","digest":"b".repeat(64),"binary_sha256":"c".repeat(64)}));
    super::write_json(
        &root.path().join("resources.json"),
        &json!({"pid":pid,"error":null,
        "observed_unix_ms":super::utc_ms()-10001}),
    )
    .unwrap();
    fleet.project_resources(&mut progress);
    assert!(!progress.complete());
    assert!(!fleet.resource_terminal_accepted[0]);
    fleet.children[0].child.stdin.take();
    let status = fleet.children[0].child.wait().unwrap();
    fleet.children[0].record_status(status);
    progress.finish(false, false);
}
#[test]
fn resource_progress_file_reader_distinguishes_absent_and_malformed_owned_receipts() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("resources.json");
    assert!(read_resource_file(&path).unwrap().is_none());
    std::fs::write(&path, b"PRIVATE_MALFORMED_SAMPLE").unwrap();
    assert_eq!(
        read_resource_file(&path).unwrap_err(),
        "resource receipt invalid"
    );
    super::write_json(&path, &json!({"pid":123})).unwrap();
    assert_eq!(read_resource_file(&path).unwrap().unwrap()["pid"], 123);
}
#[test]
fn resource_progress_reader_rejects_symlink_fifo_and_oversize_owned_sources() {
    use std::{
        io::Write,
        os::unix::fs::{OpenOptionsExt, symlink},
    };
    let root = tempfile::tempdir().unwrap();
    let regular = root.path().join("regular");
    std::fs::write(&regular, b"{}").unwrap();
    assert!(read_resource_file(&regular).unwrap().is_some());
    let link = root.path().join("link");
    symlink(&regular, &link).unwrap();
    let symlink_rejected = read_resource_file(&link).is_err();
    let oversized = root.path().join("oversized");
    let mut bytes = b"{}".to_vec();
    bytes.resize(RESOURCE_RECEIPT_LIMIT, b' ');
    std::fs::write(&oversized, &bytes).unwrap();
    assert!(read_resource_file(&oversized).unwrap().is_some());
    bytes.push(b' ');
    std::fs::write(&oversized, &bytes).unwrap();
    let oversized_rejected = read_resource_file(&oversized).is_err();
    let fifo = root.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    writer.write_all(b"{}").unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        drop(writer);
    });
    let started = Instant::now();
    let result = read_resource_file(&fifo);
    release.join().unwrap();
    assert!(
        symlink_rejected && oversized_rejected && result.is_err(),
        "symlink, oversized and special files must be rejected: {symlink_rejected}/{oversized_rejected}/{}",
        result.is_err()
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "special file read must not block"
    );
}
impl OwnedChild {
    fn record_status(&mut self, status: std::process::ExitStatus) {
        use std::os::unix::process::ExitStatusExt;
        self.reaped = true;
        self.exited = status.code();
        self.signal = status.signal();
    }
}
impl Drop for Fleet {
    fn drop(&mut self) {
        for c in &mut self.children {
            if c.reaped {
                continue;
            }
            if let Ok(Some(status)) = c.child.try_wait() {
                c.record_status(status);
                continue;
            }
            c.forced = true;
            let killed = c.child.kill();
            if let Ok(Some(status)) = c.child.try_wait() {
                c.record_status(status);
            }
            let _ = super::write_json(
                &c.root.join("emergency-owner.json"),
                &json!({"pid":c.child.id(),"kill_succeeded":killed.is_ok(),"reap_confirmed":c.reaped,"exit_code":c.exited,"exit_signal":c.signal,"scope":"nonblocking Drop fallback; unproven reap remains incomplete"}),
            );
        }
    }
}
fn validate_ready(
    r: &Ready,
    pid: u32,
    index: usize,
    generation: u64,
    p: &PrivateConfig,
) -> Result<(), String> {
    if r.pid != pid
        || r.server != index
        || r.generation != generation
        || r.catalog_digest != p.catalog_digest
        || r.source_digest != p.source_digest
        || r.binary_digest != p.binary_digest
        || r.backend_prefix != p.backend.prefix
        || r.mode != "MRC5"
        || r.schema != "mount-rs.production-ready.v2"
        || r.planned_drives != p.config.drives
        || r.registered_drives != p.config.drives
        || r.max_active_drives != p.config.drives
        || r.expected_backings_sha256 != p.expected_backings_sha256
        || !r.address.ip().is_loopback()
    {
        return Err("worker readiness identity mismatch".into());
    }
    let expected = expected_backings(p)?;
    super::metrics::validate_runtime_snapshot(
        &r.runtime_activation,
        generation,
        p.config.drives,
        &expected,
    )?;
    if !cold_runtime(&r.runtime_activation, p.config.drives) {
        return Err("worker readiness was not an actual cold registration".into());
    }
    // Readiness is historical; Fleet also checks the current on-disk sample.
    let observed = r.resources["observed_unix_ms"]
        .as_u64()
        .ok_or("ready resource timestamp missing")?;
    super::resources::validate_sample(&r.resources, pid, observed)?;
    Ok(())
}
fn runtime_keeper() -> Arc<super::lazy_runtime::TargetRuntimeKeeper> {
    static KEEPER: OnceLock<Arc<super::lazy_runtime::TargetRuntimeKeeper>> = OnceLock::new();
    KEEPER
        .get_or_init(|| Arc::new(super::lazy_runtime::TargetRuntimeKeeper::default()))
        .clone()
}
fn cold_runtime(value: &Value, drives: usize) -> bool {
    value["pool"]["registered"].as_u64() == Some(drives as u64)
        && [
            "resident",
            "opening",
            "ready",
            "closing",
            "quarantined",
            "pinned",
            "open_success",
            "open_error",
            "eviction_success",
            "eviction_error",
            "waits",
            "capacity_rejections",
        ]
        .into_iter()
        .all(|field| value["pool"][field].as_u64() == Some(0))
        && value["observations"].as_array().is_some_and(|rows| {
            rows.len() == drives
                && rows.iter().all(|row| {
                    row["constructed"].as_u64() == Some(0) && row["observed_backing"].is_null()
                })
        })
}
fn expected_backings(p: &PrivateConfig) -> Result<Vec<String>, String> {
    if p.expected_backings.len() != p.config.drives
        || super::digest(
            &serde_json::to_vec(&p.expected_backings)
                .map_err(|_| "backing manifest encoding failed")?,
        ) != p.expected_backings_sha256
    {
        return Err("initializer backing manifest binding mismatch".into());
    }
    p.expected_backings
        .iter()
        .map(|value| {
            let id = mount_rs_core::storage::ConcurrentBackingId::from_hex(value)
                .map_err(|_| "initializer backing manifest invalid")?;
            if id.to_hex() != *value || value == "00000000000000000000000000000000" {
                return Err("initializer backing manifest noncanonical".into());
            }
            Ok(id.to_hex())
        })
        .collect()
}
pub async fn worker() -> Result<(), String> {
    let index: usize = std::env::var("MOUNT_RS_TARGET_WORKER")
        .map_err(|_| "worker index missing")?
        .parse()
        .map_err(|_| "worker index invalid")?;
    if index >= SERVERS {
        return Err("worker index outside owned fleet".into());
    }
    let mut startup = Startup::new_lazy(
        mount_rs_core::diagnostics::profile::enabled(),
        StartupIdentity::worker(index, 0),
    );
    startup.publish(&mut |snapshot| Startup::write_record(&mut std::io::stderr().lock(), snapshot));
    let mut startup_root = None;
    let result = worker_observed(index, &mut startup, &mut startup_root).await;
    if result.is_err() {
        startup.finish_startup(false);
        if startup
            .snapshot()
            .is_some_and(|snapshot| snapshot.cleanup_outcome.is_none())
        {
            let _ = startup.write_banks(&mut std::io::stderr().lock());
        }
        startup.publish(&mut |snapshot| match &startup_root {
            Some(root) => publish_startup_progress(root, snapshot.generation, snapshot),
            None => Startup::write_record(&mut std::io::stderr().lock(), snapshot),
        });
    }
    result
}
async fn worker_observed(
    index: usize,
    startup: &mut Startup,
    startup_root: &mut Option<PathBuf>,
) -> Result<(), String> {
    let p: PrivateConfig = startup
        .observe(
            StartupStage::Configuration,
            async {
                let private = std::env::var_os("MOUNT_RS_TARGET_PRIVATE_CONFIG")
                    .ok_or("private worker config required")?;
                let p: PrivateConfig = serde_json::from_slice(
                    &std::fs::read(private).map_err(|_| "private config unreadable")?,
                )
                .map_err(|_| "private config invalid")?;
                p.config.validate()?;
                Ok::<_, String>(p)
            },
            &mut |snapshot| Startup::write_record(&mut std::io::stderr().lock(), snapshot),
        )
        .await?;
    let root = p.output.join(format!("worker-{index}"));
    if startup.enabled() {
        *startup_root = Some(root.clone());
    }
    startup.publish(&mut |snapshot| publish_startup_progress(&root, 0, snapshot));
    let mut resources = startup
        .observe(
            StartupStage::Configuration,
            async { super::resources::Resources::start(root.join("resources.json")) },
            &mut |snapshot| publish_startup_progress(&root, 0, snapshot),
        )
        .await?;
    // Reserve process-lifetime authority before context or provider construction.
    let scope = runtime_keeper()
        .reserve()
        .map_err(|_| "worker runtime owner occupied")?;
    let context = startup
        .observe(
            StartupStage::Configuration,
            async { mount_rs_sdk::StorageContext::new(16).map_err(|_| "storage context failed") },
            &mut |snapshot| publish_startup_progress(&root, 0, snapshot),
        )
        .await?;
    scope
        .install_context(context.clone())
        .map_err(|_| "worker context retention failed")?;
    let mut current_generation = None;
    let mut prepared = false;
    let mut server_diagnostics = None;
    let mut generation = 0;
    let mut opens = 0;
    let mut commands = super::command::Commands::default();
    let mut phase_metrics = None;
    let mut metric_sequence = super::metrics::Sequence::default();
    let mut last_metric_sequence = 0;
    let metrics_root = root.join("metrics");
    let result = tokio::time::timeout(Duration::from_secs(2500), async {
        startup.observe(StartupStage::Configuration, async {
        phase_metrics = Some(super::metrics::Local::new()?);
        std::fs::create_dir(&metrics_root)
            .map_err(|_| "worker metrics directory exists or unavailable")?;
        if super::file_digest(&std::env::current_exe().map_err(|_| "executable unavailable")?)?
            != p.binary_digest
        {
            return Err("worker binary mismatch".into());
        }
        if super::source_identity(&mut commands).await?["digest"].as_str() != Some(&p.source_digest)
        {
            return Err("worker source mismatch".into());
        }
        Ok::<_, String>(())
        }, &mut |snapshot| publish_startup_progress(&root, 0, snapshot)).await?;
        let phase_metrics = phase_metrics.as_mut().ok_or("worker metric owner unavailable")?;
        loop {
            if mount_rs_core::diagnostics::profile::enabled() {
                resources.install_checkpoints(super::checkpoints::Identity {
                    pid: std::process::id(), controller_pid: p.parent_pid, worker: index, generation,
                    source_digest: p.source_digest.clone(), binary_digest: p.binary_digest.clone(),
                })?;
            }
            if generation != 0 {
                *startup = Startup::new_lazy(mount_rs_core::diagnostics::profile::enabled(), StartupIdentity::worker(index, generation));
            }
            let mut startup_sink = |snapshot: &StartupSnapshot| publish_startup_progress(&root, generation, snapshot);
            startup.publish(&mut startup_sink);
            resources.check()?;
            let catalog = Arc::new(
                startup.observe(StartupStage::CatalogOpen, async {
                    SqliteCatalog::open(&p.catalog).await.map_err(|_| "worker catalog open failed")
                }, &mut startup_sink).await?,
            );
            let snapshot = startup.observe(StartupStage::CatalogLoad, async {
                catalog.load_shared_current().await.map_err(|_| "worker catalog read failed")
            }, &mut startup_sink).await?;
            startup.plan_lazy((p.config.drives / 2) as u64, p.config.drives as u64, p.config.drives as u64)
                .map_err(|_| "worker lazy startup plan invalid")?;
            startup.observe(StartupStage::CatalogValidate, async {
            if super::digest(
                &serde_json::to_vec(snapshot.as_ref())
                    .map_err(|_| "catalog serialization failed")?,
            ) != p.catalog_digest
            {
                return Err("worker catalog differs".into());
            }
            let expected = target_catalog(p.config.drives);
            if snapshot.partitions != expected.partitions {
                return Err("worker catalog shape differs".into());
            }
            Ok::<_, String>(())
            }, &mut startup_sink).await?;
            if !prepared {
                let ids = expected_backings(&p)?;
                let options = p.backend.prepared_options(p.config.drives)?;
                let plans = options.into_iter().enumerate().map(|(drive, options)| {
                    Ok::<_, String>(super::lazy_runtime::PreparedDrive {
                        options,
                        expected_backing: mount_rs_core::storage::ConcurrentBackingId::from_hex(&ids[drive])
                            .map_err(|_| "worker expected backing invalid")?,
                    })
                }).collect::<Result<Vec<_>, _>>()?;
                scope.install_prepared(plans).map_err(|_| "worker immutable runtime planning failed")?;
                prepared = true;
            }
            let runtime = scope.new_generation(generation).map_err(|_| "worker generation admission refused")?;
            current_generation = Some(runtime.clone());
            let mut dispatcher = DriveDispatcher::new(catalog.clone());
            for drive in 0..p.config.drives {
                resources.check()?;
                let registration = startup.observe(StartupStage::DriveConfig, async {
                    runtime.register(drive).map_err(|_| "worker lazy runtime registration failed")
                }, &mut startup_sink).await?;
                startup.construction_plan();
                startup.observe(StartupStage::DriveRegister, async { dispatcher
                    .register_lazy_definition(
                        &format!("partition-{}", drive / 2),
                        &format!("sandbox-{drive}"),
                        snapshot.partitions[&format!("partition-{}", drive / 2)].drives
                            [&format!("sandbox-{drive}")].driver.clone(),
                        registration,
                    ).map_err(|_| "worker registration failed")
                }, &mut startup_sink).await?;
                startup.registered();
            }
            let auth = Arc::new(CatalogAuthenticator::with_key_source(
                catalog,
                Arc::new(Keys(p.jwk.clone())),
            ));
            let server = startup.observe(StartupStage::ListenerBind, RemoteServer::bind_with_diagnostics(
                    "127.0.0.1:0".parse().unwrap(),
                    vec![rustls::pki_types::CertificateDer::from(p.cert.clone())],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(p.key.clone()).into(),
                    Arc::new(dispatcher),
                    auth,
                    RemoteServerOptions {
                        max_connections: (p.config.drives / SERVERS + 32).max(128),
                    },
                    RemoteTransferLimits::default(),
                    mount_rs_core::diagnostics::profile::enabled(),
                ), &mut startup_sink).await.map_err(|_| "worker TLS listener bind failed")?;
            let address = server.local_addr();
            server_diagnostics=server.diagnostics();
            runtime.install_server(server).map_err(|_| "worker listener retention failed")?;
            startup.finish_startup(true);
            startup.publish(&mut startup_sink);
            let startup_metrics = phase_metrics.capture_with_runtime(
                super::metrics::identity(&p, std::process::id(), Some(index), generation, last_metric_sequence, "worker_startup", "ready"),
                server_diagnostics.as_ref(), || runtime.runtime_activation_snapshot(),
            )?;
            let startup_path = metrics_root.join(format!("startup-g{generation}.json"));
            super::metrics::publish_immutable(&startup_path, &startup_metrics)?;
            let ready = Ready {
                pid: std::process::id(),
                server: index,
                generation,
                address,
                catalog_digest: p.catalog_digest.clone(),
                source_digest: p.source_digest.clone(),
                binary_digest: p.binary_digest.clone(),
                backend_prefix: p.backend.prefix.clone(),
                mode: "MRC5".into(),
                schema: "mount-rs.production-ready.v2".into(),
                planned_drives: p.config.drives,
                registered_drives: runtime.pool_snapshot().registered,
                max_active_drives: p.config.drives,
                expected_backings_sha256: p.expected_backings_sha256.clone(),
                runtime_activation: startup_metrics["runtime_activation"].clone(),
                resources: resources.snapshot(),
                startup_diagnostics: startup.snapshot().map(|value| serde_json::to_value(value).unwrap()),
                phase_metrics: json!({"file":format!("metrics/startup-g{generation}.json"),"sha256":super::file_digest(&startup_path)?,"metrics_complete":startup_metrics["metrics_complete"]}),
                core_profile: json!({
                        "enabled":mount_rs_core::diagnostics::profile::enabled(),
                        "scope":"worker service SDK startup and replica refresh cumulative counters",
                        "snapshot":mount_rs_core::diagnostics::profile::snapshot()}
                ),
            };
            publish_ready(&root, &ready)?;
            loop {
                resources.check()?;
                if unsafe { libc::getppid() } as u32 != p.parent_pid {
                    return Err("controller no longer owns worker".into());
                }
                let command = super::read_json(&root.join("command.json")).unwrap_or(Value::Null);
                if command["command"] == "stop" {
                    return Ok(());
                }
                if command["command"] == "reopen"
                    && command["generation"].as_u64() == Some(generation + 1)
                {
                    break;
                }
                if command["command"] == "metrics" {
                    let requested = &command["identity"];
                    let sequence = requested["sequence"].as_u64().ok_or("metric command sequence missing")?;
                    let expected = super::metrics::identity(&p, std::process::id(), Some(index), generation, sequence,
                        requested["phase"].as_str().ok_or("metric command phase missing")?,
                        requested["boundary"].as_str().ok_or("metric command boundary missing")?);
                    super::metrics::validate_receipt(requested, &expected)?;
                    if metric_sequence.accept(requested)? {
                        let captured = phase_metrics.capture_with_runtime(expected, server_diagnostics.as_ref(), || runtime.runtime_activation_snapshot())?;
                        let path = metrics_root.join(format!("g{generation}-s{sequence}.json"));
                        super::metrics::publish_immutable(&path, &captured)?;
                        super::write_json(&root.join("metrics-ack.json"), &json!({"identity":requested,"file":format!("metrics/g{generation}-s{sequence}.json"),"sha256":super::file_digest(&path)?}))?;
                        last_metric_sequence = sequence;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            runtime.close().await.map_err(|_| "worker replica drain unproven")?;
            opens += runtime.pool_snapshot().open_success;
            current_generation = None;
            let closed_metrics=phase_metrics.capture_with_runtime(
                super::metrics::identity(&p,std::process::id(),Some(index),generation,last_metric_sequence+1,"replica_close","after"),
                server_diagnostics.as_ref(),|| runtime.runtime_activation_snapshot(),
            )?;
            super::metrics::publish_immutable(&metrics_root.join(format!("closed-g{generation}.json")),&closed_metrics)?;
            generation += 1;
        }
    })
    .await
    .map_err(|_| "worker lifetime deadline".to_string())
    .and_then(|x| x);
    startup.finish_startup(result.is_ok());
    startup.publish(&mut |snapshot| publish_startup_progress(&root, generation, snapshot));
    if result.is_err() {
        let _ = startup.write_banks(&mut std::io::stderr().lock());
    }
    let cleanup = startup.begin(StartupStage::Cleanup);
    let close = match &current_generation {
        Some(runtime) => runtime
            .close()
            .await
            .map_err(|_| "worker replica drain unproven".to_string()),
        None => Ok(()),
    };
    if let Some(runtime) = &current_generation {
        opens += runtime.pool_snapshot().open_success;
    }
    // Scope owns the context and refuses this transition unless every generation ACKed.
    let context_close = tokio::time::timeout(Duration::from_secs(30), scope.close_context()).await;
    let observer_close = commands.cleanup().await;
    let terminal_metrics = phase_metrics
        .as_mut()
        .ok_or_else(|| "worker metric setup incomplete".to_string())
        .and_then(|metrics| {
            metrics.capture_with_runtime(
                super::metrics::identity(
                    &p,
                    std::process::id(),
                    Some(index),
                    generation,
                    last_metric_sequence + 1,
                    "worker_cleanup",
                    "terminal",
                ),
                server_diagnostics.as_ref(),
                || {
                    current_generation
                        .as_ref()
                        .ok_or_else(|| mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio))?
                        .runtime_activation_snapshot()
                },
            )
        })
        .and_then(|value| {
            super::metrics::publish_immutable(&metrics_root.join("terminal.json"), &value)?;
            Ok(value)
        });
    let sampler_close = resources.finish().await;
    let cleanup_ok = close.is_ok()
        && matches!(context_close, Ok(Ok(())))
        && sampler_close.is_ok()
        && observer_close.is_ok();
    cleanup.finish(cleanup_ok);
    startup.finish_cleanup(cleanup_ok);
    startup.publish(&mut |snapshot| publish_startup_progress(&root, generation, snapshot));
    let clean = result.is_ok()
        && close.is_ok()
        && matches!(context_close, Ok(Ok(())))
        && sampler_close.is_ok()
        && observer_close.is_ok();
    super::write_json(
        &root.join("terminal.json"),
        &json!({
                "pid":std::process::id(),
                "server":index,
                "clean":clean,
                "error":result.err(),
                "replica_close_error":close.err(),
                "context_closed":matches!(context_close,
                    Ok(Ok(()))),
                "phase_metrics":{"file":"metrics/terminal.json","capture_error":terminal_metrics.as_ref().err(),"metrics_complete":terminal_metrics.as_ref().ok().map(|m| &m["metrics_complete"])},
                "startup_diagnostics":startup.snapshot(),
                "replica_opens":opens,"sampler_shutdown_error":sampler_close.err(),"observer_close_error":observer_close.err(),"observer_processes":commands.receipts(),
                "resources":resources.snapshot(),
                "observer_accounting":super::metrics::observer().snapshot(),
                "observer_accounting_scope":"includes completed final metric publication and sampler; excludes this terminal receipt write; wall is inclusive, not isolated CPU",
                "core_profile":{
                    "enabled":mount_rs_core::diagnostics::profile::enabled(),
                    "scope":"worker service SDK cumulative startup, workload and cleanup counters; no NAPI recorder",
                    "snapshot":mount_rs_core::diagnostics::profile::snapshot()}
            }
        ),
    )?;
    if clean {
        Ok(())
    } else {
        Err("worker incomplete; retained terminal receipt".into())
    }
}
fn read_startup_file(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "startup observation is not an owned regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(mount_rs_service::startup::RECORD_LIMIT + 1);
    file.take((mount_rs_service::startup::RECORD_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > mount_rs_service::startup::RECORD_LIMIT {
        return Err(std::io::Error::other("startup observation exceeds bound"));
    }
    Ok(Some(bytes))
}
fn owned_startup(
    bytes: &[u8],
    pid: u32,
    index: usize,
    generation: u64,
    drives: usize,
) -> std::io::Result<StartupSnapshot> {
    let snapshot = StartupSnapshot::parse(bytes)?;
    if snapshot.pid != pid
        || snapshot.worker.map(usize::from) != Some(index)
        || snapshot.generation != generation
        || snapshot
            .planned_drives
            .is_some_and(|planned| planned != drives as u64)
        || snapshot
            .configured_partitions
            .is_some_and(|partitions| partitions != (drives / 2) as u64)
    {
        return Err(std::io::Error::other(
            "startup observation identity mismatch",
        ));
    }
    Ok(snapshot)
}
fn lazy_plan_complete(snapshot: &StartupSnapshot, drives: usize) -> bool {
    snapshot.accounting_complete
        && snapshot.terminal_outcome == mount_rs_service::startup::Outcome::Ready
        && snapshot.planned_drives == Some(drives as u64)
        && snapshot.registered_drives == drives as u64
        && snapshot.construction_mode == Some(mount_rs_service::startup::ConstructionMode::Lazy)
        && snapshot.max_active_drives == Some(drives as u64)
        && snapshot.construction_plans == Some(drives as u64)
        && snapshot.open_started == 0
        && snapshot.open_success == 0
}
fn ready_startup_complete(ready: &Ready, drives: usize) -> bool {
    let Some(value) = &ready.startup_diagnostics else {
        return false;
    };
    let Ok(bytes) = serde_json::to_vec(value) else {
        return false;
    };
    owned_startup(&bytes, ready.pid, ready.server, ready.generation, drives)
        .is_ok_and(|snapshot| lazy_plan_complete(&snapshot, drives))
}
fn publish_startup_progress(
    root: &Path,
    generation: u64,
    snapshot: &StartupSnapshot,
) -> std::io::Result<()> {
    let path = root.join(format!("startup-progress-g{generation}.json"));
    let pending = path.with_extension("pending");
    let mut file = std::fs::File::create(&pending)?;
    Startup::write_json(&mut file, snapshot)?;
    std::fs::rename(&pending, &path)?;
    Startup::write_record(&mut std::io::stderr().lock(), snapshot)
}
fn publish_ready(root: &Path, ready: &Ready) -> Result<(), String> {
    let value = serde_json::to_value(ready).map_err(|_| "readiness encoding failed")?;
    let generation = root.join(format!("ready-generation-{}.json", ready.generation));
    if generation.exists() {
        return Err("readiness generation already published".into());
    }
    super::write_json(&generation, &value)?;
    super::write_json(&root.join("ready.json"), &value)
}
#[cfg(test)]
fn example_ready() -> Ready {
    Ready {
        pid: 123,
        server: 0,
        generation: 0,
        address: "127.0.0.1:1234".parse().unwrap(),
        catalog_digest: "catalog".into(),
        source_digest: "source".into(),
        binary_digest: "binary".into(),
        backend_prefix: "owned".into(),
        mode: "MRC5".into(),
        schema: "mount-rs.production-ready.v2".into(),
        planned_drives: 10,
        registered_drives: 10,
        max_active_drives: 10,
        expected_backings_sha256: super::digest(
            &serde_json::to_vec(&example_expected_backings()).unwrap(),
        ),
        runtime_activation: super::metrics::example_runtime(0, 10),
        resources: super::resources::example_sample(123),
        core_profile: Value::Null,
        phase_metrics: Value::Null,
        startup_diagnostics: None,
    }
}
#[test]
fn readiness_preserves_startup_and_refresh_receipts() {
    let root = tempfile::tempdir().unwrap();
    let mut ready = example_ready();
    publish_ready(root.path(), &ready).unwrap();
    ready.generation = 1;
    publish_ready(root.path(), &ready).unwrap();
    assert_eq!(
        super::read_json(&root.path().join("ready-generation-0.json")).unwrap()["generation"],
        0
    );
    assert_eq!(
        super::read_json(&root.path().join("ready-generation-1.json")).unwrap()["generation"],
        1
    );
    assert_eq!(
        super::read_json(&root.path().join("ready.json")).unwrap()["generation"],
        1
    );
}
#[test]
fn wrong_readiness_identities_are_rejected() {
    let ready = example_ready();
    let p = PrivateConfig {
        config: Config {
            full_target: false,
            drives: 10,
            files: 2,
            seconds: 1,
            population_seconds: super::config::PHASE_SECONDS,
            provider: "sqlite".into(),
        },
        backend: Backend {
            provider: "sqlite".into(),
            block_provider: "metadata".into(),
            root: PathBuf::new(),
            prefix: "owned".into(),
        },
        catalog: PathBuf::new(),
        catalog_digest: "catalog".into(),
        cert: vec![],
        key: vec![],
        jwk: Jwk::EcP256 {
            kid: "test".into(),
            x: "test".into(),
            y: "test".into(),
        },
        source_digest: "source".into(),
        binary_digest: "binary".into(),
        output: PathBuf::new(),
        parent_pid: 1,
        expected_backings: example_expected_backings(),
        expected_backings_sha256: super::digest(
            &serde_json::to_vec(&example_expected_backings()).unwrap(),
        ),
    };
    validate_ready(&ready, 123, 0, 0, &p).unwrap();
    assert!(validate_ready(&ready, 124, 0, 0, &p).is_err());
    assert!(validate_ready(&ready, 123, 1, 0, &p).is_err());
    assert!(validate_ready(&ready, 123, 0, 1, &p).is_err());
    for field in [
        "catalog_digest",
        "source_digest",
        "binary_digest",
        "backend_prefix",
        "mode",
    ] {
        let mut value = serde_json::to_value(&ready).unwrap();
        value[field] = json!("wrong");
        let wrong: Ready = serde_json::from_value(value).unwrap();
        assert!(validate_ready(&wrong, 123, 0, 0, &p).is_err());
    }
}
#[tokio::test]
async fn cleanup_forces_and_reaps_owned_noncooperating_child_within_budget() {
    let root = tempfile::tempdir().unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", "exec sleep 30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut fleet = Fleet::new();
    fleet.children.push(OwnedChild {
        child,
        server: 0,
        ready: None,
        exited: None,
        reaped: false,
        signal: None,
        forced: false,
        root: root.path().to_owned(),
    });
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        fleet.cleanup_bounded(Duration::from_millis(20), Duration::from_millis(300)),
    )
    .await;
    assert!(
        result.is_ok(),
        "cleanup must reserve force/reap time inside its bound"
    );
    assert!(!result.unwrap().is_empty());
    assert!(fleet.children[0].forced);
    assert!(fleet.children[0].child.try_wait().unwrap().is_some());
}
#[test]
fn prereview_red_malformed_backing_receipts() {
    assert!(validate_backing_receipt(&Value::Null, 0).is_err());
}
pub(super) fn validate_backing_receipt(r: &Value, drive: usize) -> Result<String, String> {
    if r["drive"].as_u64() != Some(drive as u64)
        || r["mode"] != "MRC5"
        || r["provider_backing_verified"] != true
    {
        return Err("invalid Drive backing receipt".into());
    }
    let value = r["backing"].as_str().ok_or("backing identity missing")?;
    let backing = mount_rs_core::storage::ConcurrentBackingId::from_hex(value)
        .map_err(|_| "backing identity invalid")?;
    if backing.to_hex() == "00000000000000000000000000000000" {
        return Err("zero backing identity".into());
    }
    Ok(backing.to_hex())
}
#[cfg(test)]
fn example_expected_backings() -> Vec<String> {
    (0..10).map(|drive| format!("{:032x}", drive + 1)).collect()
}
#[derive(Default)]
struct CloseTasks {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    failed: bool,
}
impl CloseTasks {
    async fn drain(&mut self, budget: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut index = 0;
        while index < self.tasks.len() {
            match tokio::time::timeout_at(deadline, &mut self.tasks[index]).await {
                Ok(result) => {
                    self.failed |= result.is_err();
                    drop(self.tasks.swap_remove(index));
                }
                Err(_) => {
                    self.failed = true;
                    index += 1;
                }
            }
        }
        if self.failed {
            Err("listener drain incomplete".into())
        } else {
            self.tasks.clear();
            Ok(())
        }
    }
}
#[tokio::test]
async fn prereview_red_completed_listener_failure_is_consumed_once() {
    let mut owner = CloseTasks::default();
    owner
        .tasks
        .push(tokio::spawn(async { panic!("injected close failure") }));
    assert!(owner.drain(Duration::from_millis(20)).await.is_err());
    assert!(
        owner.tasks.is_empty(),
        "completed failure handle must be consumed"
    );
    assert!(
        owner.drain(Duration::from_millis(20)).await.is_err(),
        "failure must remain sticky without repoll"
    );
}
#[tokio::test]
async fn listener_timeout_retains_only_pending_handle_until_completion() {
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let mut owner = CloseTasks::default();
    owner.tasks.push(tokio::spawn(async {
        let _ = receive.await;
    }));
    assert!(owner.drain(Duration::from_millis(1)).await.is_err());
    assert_eq!(owner.tasks.len(), 1);
    send.send(()).unwrap();
    assert!(owner.drain(Duration::from_secs(1)).await.is_err());
    assert!(owner.tasks.is_empty());
}

#[test]
fn startup_progress_identity_bounds_and_incomplete_readiness_are_separate() {
    let startup = Startup::new_lazy(true, StartupIdentity::worker(0, 2));
    startup.plan_lazy(5, 10, 10).unwrap();
    for _ in 0..10 {
        startup.construction_plan();
        startup.begin(StartupStage::DriveRegister).finish(true);
        startup.registered();
    }
    startup.publish(&mut |_| Err(std::io::Error::other("observer unavailable")));
    startup.finish_startup(true);
    let snapshot = startup.snapshot().unwrap();
    assert_eq!(
        snapshot.terminal_outcome,
        mount_rs_service::startup::Outcome::Ready
    );
    let bytes = serde_json::to_vec(&snapshot).unwrap();
    assert!(owned_startup(&bytes, snapshot.pid, 0, 2, 10).is_ok());
    for (pid, worker, generation, drives) in [
        (snapshot.pid + 1, 0, 2, 10),
        (snapshot.pid, 1, 2, 10),
        (snapshot.pid, 0, 1, 10),
        (snapshot.pid, 0, 2, 12),
    ] {
        assert!(owned_startup(&bytes, pid, worker, generation, drives).is_err());
    }
    let mut ready = example_ready();
    ready.pid = snapshot.pid;
    ready.generation = 2;
    ready.startup_diagnostics = Some(serde_json::to_value(&snapshot).unwrap());
    assert!(!ready_startup_complete(&ready, 10));
    // Ready's product identity/content validation remains independent of the observer.
    assert_eq!(
        ready.planned_drives as u64,
        snapshot.planned_drives.unwrap()
    );
    ready.startup_diagnostics = None;
    assert!(!ready_startup_complete(&ready, 10));
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-startup.json");
    assert!(read_startup_file(&path).unwrap().is_none());
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(read_startup_file(&path).unwrap().unwrap(), bytes);
    std::fs::write(
        &path,
        vec![b' '; mount_rs_service::startup::RECORD_LIMIT + 1],
    )
    .unwrap();
    assert!(read_startup_file(&path).is_err());
}

fn terminal_startup_complete(terminal: &Value, ready: &Ready) -> bool {
    if terminal["pid"].as_u64() != Some(u64::from(ready.pid))
        || terminal["server"].as_u64() != Some(ready.server as u64)
    {
        return false;
    }
    let Ok(bytes) = serde_json::to_vec(&terminal["startup_diagnostics"]) else {
        return false;
    };
    owned_startup(
        &bytes,
        ready.pid,
        ready.server,
        ready.generation,
        ready.planned_drives,
    )
    .is_ok_and(|snapshot| {
        lazy_plan_complete(&snapshot, ready.planned_drives)
            && snapshot.cleanup_outcome == Some(mount_rs_service::startup::CleanupOutcome::Success)
    })
}
#[test]
fn terminal_startup_publication_failure_cannot_reuse_ready_completeness() {
    let startup = Startup::new_lazy(true, StartupIdentity::worker(0, 0));
    startup.plan_lazy(5, 10, 10).unwrap();
    for _ in 0..10 {
        startup.construction_plan();
        startup.begin(StartupStage::DriveRegister).finish(true);
        startup.registered();
    }
    startup.finish_startup(true);
    let mut ready = example_ready();
    ready.pid = std::process::id();
    ready.startup_diagnostics = startup.snapshot().map(|s| serde_json::to_value(s).unwrap());
    assert!(ready_startup_complete(&ready, 10));
    startup.begin(StartupStage::Cleanup).finish(true);
    startup.finish_cleanup(true);
    let complete = json!({"pid":ready.pid,"server":ready.server,"clean":true,"startup_diagnostics":startup.snapshot()});
    assert!(terminal_startup_complete(&complete, &ready));
    let mut wrong_generation = complete.clone();
    wrong_generation["startup_diagnostics"]["generation"] = json!(1);
    assert!(!terminal_startup_complete(&wrong_generation, &ready));
    startup.publish(&mut |_| Err(std::io::Error::other("late publication failed")));
    let terminal = json!({"pid":ready.pid,"server":ready.server,"clean":true,"startup_diagnostics":startup.snapshot()});
    assert!(!terminal_startup_complete(&terminal, &ready));
    assert_eq!(
        terminal["clean"], true,
        "product cleanup remains successful"
    );
}

#[test]
fn lazy_target_terminal_requires_the_same_cold_plan_as_ready() {
    let mut ready = example_ready();
    ready.pid = std::process::id();
    let lazy = Startup::new_lazy(true, StartupIdentity::worker(0, 0));
    lazy.plan_lazy(5, 10, 10).unwrap();
    for _ in 0..10 {
        lazy.construction_plan();
        lazy.registered();
    }
    lazy.finish_startup(true);
    lazy.begin(StartupStage::Cleanup).finish(true);
    lazy.finish_cleanup(true);
    let complete =
        json!({"pid":ready.pid,"server":ready.server,"startup_diagnostics":lazy.snapshot()});
    assert!(terminal_startup_complete(&complete, &ready));
    let mut reduced = complete.clone();
    reduced["startup_diagnostics"]["max_active_drives"] = json!(1);
    let bytes = serde_json::to_vec(&reduced["startup_diagnostics"]).unwrap();
    assert!(
        owned_startup(&bytes, ready.pid, 0, 0, 10).is_ok(),
        "negative must be structurally valid"
    );
    assert!(
        !terminal_startup_complete(&reduced, &ready),
        "terminal must not shrink default capacity"
    );
    let eager = Startup::new(true, StartupIdentity::worker(0, 0));
    eager.plan(5, 10);
    for _ in 0..10 {
        eager.begin(StartupStage::DriveOpen).finish(true);
        eager.registered();
    }
    eager.finish_startup(true);
    eager.begin(StartupStage::Cleanup).finish(true);
    eager.finish_cleanup(true);
    let old = json!({"pid":ready.pid,"server":ready.server,"startup_diagnostics":eager.snapshot()});
    let bytes = serde_json::to_vec(&old["startup_diagnostics"]).unwrap();
    assert!(
        owned_startup(&bytes, ready.pid, 0, 0, 10).is_ok(),
        "legacy eager negative must parse"
    );
    assert!(
        !terminal_startup_complete(&old, &ready),
        "terminal must carry the same cold lazy plan"
    );
}
