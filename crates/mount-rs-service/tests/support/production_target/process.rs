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
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
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
    pub replicas: usize,
    pub receipts: Vec<Value>,
    pub resources: Value,
    pub core_profile: Value,
    pub phase_metrics: Value,
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
}
impl Fleet {
    pub fn expect_initialized_backings(&mut self, values: Vec<String>) {
        self.initial_backings = Some(values);
    }
    pub fn new() -> Self {
        Self {
            children: vec![],
            initial_backings: None,
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
    pub async fn ready(&mut self, private: &PrivateConfig, generation: u64) -> Result<(), String> {
        let end = Instant::now() + Duration::from_secs(600);
        loop {
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
                c.ready = Some(r);
                complete += 1;
            }
            if complete == SERVERS {
                let mut endpoints = std::collections::BTreeSet::new();
                let mut pids = std::collections::BTreeSet::new();
                let backings = validate_backing_receipts(self.children[0].ready.as_ref().unwrap())?;
                for child in &self.children {
                    let r = child.ready.as_ref().unwrap();
                    if !endpoints.insert(r.address)
                        || !pids.insert(r.pid)
                        || validate_backing_receipts(r)? != backings
                    {
                        return Err("worker endpoint/PID/backing consistency mismatch".into());
                    }
                    let sample = super::read_json(&child.root.join("resources.json"))?;
                    super::resources::validate_sample(&sample, child.child.id(), super::utc_ms())?;
                }
                if self
                    .initial_backings
                    .as_ref()
                    .is_some_and(|initial| initial != &backings)
                {
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
    pub async fn cleanup(&mut self) -> Vec<Value> {
        self.cleanup_bounded(Duration::from_secs(90), Duration::from_secs(95))
            .await
    }
    async fn cleanup_bounded(&mut self, grace: Duration, total: Duration) -> Vec<Value> {
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
        loop {
            for c in &mut self.children {
                if !c.reaped {
                    match c.child.try_wait() {
                        Ok(Some(status)) => c.record_status(status),
                        Ok(None) => {}
                        Err(_) => {}
                    }
                }
            }
            if self.children.iter().all(|c| c.reaped) {
                break;
            }
            if Instant::now() >= force_at {
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
        || r.replicas != p.config.drives
        || r.receipts.len() != p.config.drives
        || !r.address.ip().is_loopback()
    {
        return Err("worker readiness identity mismatch".into());
    }
    validate_backing_receipts(r)?;
    // Readiness is historical; Fleet also checks the current on-disk sample.
    let observed = r.resources["observed_unix_ms"]
        .as_u64()
        .ok_or("ready resource timestamp missing")?;
    super::resources::validate_sample(&r.resources, pid, observed)?;
    Ok(())
}
async fn close_replicas(
    server: &mut Option<RemoteServer>,
    filesystems: &mut Vec<mount_rs_sdk::Filesystem>,
    listener_closes: &mut CloseTasks,
) -> Result<(), String> {
    let mut failure = false;
    if let Some(s) = server.take() {
        listener_closes.tasks.push(tokio::spawn(async move {
            s.close().await;
        }));
    }
    failure |= listener_closes
        .drain(Duration::from_secs(30))
        .await
        .is_err();
    let closed = tokio::time::timeout(Duration::from_secs(30), async {
        for fs in filesystems.iter() {
            if fs.shutdown().await.is_err() {
                failure = true;
            }
        }
    })
    .await;
    if closed.is_err() {
        failure = true;
    }
    if failure {
        Err("worker replica drain unproven".into())
    } else {
        filesystems.clear();
        Ok(())
    }
}
pub async fn worker() -> Result<(), String> {
    let private = std::env::var_os("MOUNT_RS_TARGET_PRIVATE_CONFIG")
        .ok_or("private worker config required")?;
    let p: PrivateConfig =
        serde_json::from_slice(&std::fs::read(private).map_err(|_| "private config unreadable")?)
            .map_err(|_| "private config invalid")?;
    p.config.validate()?;
    let index: usize = std::env::var("MOUNT_RS_TARGET_WORKER")
        .map_err(|_| "worker index missing")?
        .parse()
        .map_err(|_| "worker index invalid")?;
    if index >= SERVERS {
        return Err("worker index outside owned fleet".into());
    }
    let root = p.output.join(format!("worker-{index}"));
    let mut resources = super::resources::Resources::start(root.join("resources.json"))?;
    let context = mount_rs_sdk::StorageContext::new(16).map_err(|_| "storage context failed")?;
    let mut filesystems = Vec::new();
    let mut server = None;
    let mut server_diagnostics = None;
    let mut listener_closes = CloseTasks::default();
    let mut generation = 0;
    let mut opens = 0;
    let mut commands = super::command::Commands::default();
    let mut phase_metrics = None;
    let mut metric_sequence = super::metrics::Sequence::default();
    let mut last_metric_sequence = 0;
    let metrics_root = root.join("metrics");
    let result = tokio::time::timeout(Duration::from_secs(2500), async {
        phase_metrics = Some(super::metrics::Local::new()?);
        std::fs::create_dir(&metrics_root)
            .map_err(|_| "worker metrics directory exists or unavailable")?;
        let phase_metrics = phase_metrics.as_mut().ok_or("worker metric owner unavailable")?;
        if super::file_digest(&std::env::current_exe().map_err(|_| "executable unavailable")?)?
            != p.binary_digest
        {
            return Err("worker binary mismatch".into());
        }
        if super::source_identity(&mut commands).await?["digest"].as_str() != Some(&p.source_digest)
        {
            return Err("worker source mismatch".into());
        }
        loop {
            resources.check()?;
            let catalog = Arc::new(
                SqliteCatalog::open(&p.catalog)
                    .await
                    .map_err(|_| "worker catalog open failed")?,
            );
            let snapshot = catalog
                .load_shared_current()
                .await
                .map_err(|_| "worker catalog read failed")?;
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
            let mut dispatcher = DriveDispatcher::new(catalog.clone());
            let mut receipts = Vec::new();
            for drive in 0..p.config.drives {
                resources.check()?;
                let fs = p.backend.open(drive, &context).await?;
                let driver = fs.driver();
                filesystems.push(fs);
                opens += 1;
                receipts.push(p.backend.receipt(drive).await?);
                dispatcher
                    .register_definition(
                        &format!("partition-{}", drive / 2),
                        &format!("sandbox-{drive}"),
                        snapshot.partitions[&format!("partition-{}", drive / 2)].drives
                            [&format!("sandbox-{drive}")]
                            .driver
                            .clone(),
                        driver,
                    )
                    .map_err(|_| "worker registration failed")?;
            }
            let auth = Arc::new(CatalogAuthenticator::with_key_source(
                catalog,
                Arc::new(Keys(p.jwk.clone())),
            ));
            server = Some(
                RemoteServer::bind_with_diagnostics(
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
                )
                .await
                .map_err(|_| "worker TLS listener bind failed")?,
            );
            server_diagnostics=server.as_ref().unwrap().diagnostics();
            let startup_metrics = phase_metrics.capture(
                super::metrics::identity(&p, std::process::id(), Some(index), generation, last_metric_sequence, "worker_startup", "ready"),
                server_diagnostics.as_ref(), Value::Null,
            )?;
            let startup_path = metrics_root.join(format!("startup-g{generation}.json"));
            super::metrics::publish_immutable(&startup_path, &startup_metrics)?;
            let ready = Ready {
                pid: std::process::id(),
                server: index,
                generation,
                address: server.as_ref().unwrap().local_addr(),
                catalog_digest: p.catalog_digest.clone(),
                source_digest: p.source_digest.clone(),
                binary_digest: p.binary_digest.clone(),
                backend_prefix: p.backend.prefix.clone(),
                mode: "MRC5".into(),
                replicas: filesystems.len(),
                receipts,
                resources: resources.snapshot(),
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
                        let captured = phase_metrics.capture(expected, server_diagnostics.as_ref(), Value::Null)?;
                        let path = metrics_root.join(format!("g{generation}-s{sequence}.json"));
                        super::metrics::publish_immutable(&path, &captured)?;
                        super::write_json(&root.join("metrics-ack.json"), &json!({"identity":requested,"file":format!("metrics/g{generation}-s{sequence}.json"),"sha256":super::file_digest(&path)?}))?;
                        last_metric_sequence = sequence;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            close_replicas(&mut server, &mut filesystems, &mut listener_closes).await?;
            let closed_metrics=phase_metrics.capture(
                super::metrics::identity(&p,std::process::id(),Some(index),generation,last_metric_sequence+1,"replica_close","after"),
                server_diagnostics.as_ref(),Value::Null,
            )?;
            super::metrics::publish_immutable(&metrics_root.join(format!("closed-g{generation}.json")),&closed_metrics)?;
            generation += 1;
        }
    })
    .await
    .map_err(|_| "worker lifetime deadline".to_string())
    .and_then(|x| x);
    let close = close_replicas(&mut server, &mut filesystems, &mut listener_closes).await;
    let context_close = tokio::time::timeout(Duration::from_secs(30), context.close()).await;
    let observer_close = commands.cleanup().await;
    let terminal_metrics = phase_metrics
        .as_mut()
        .ok_or_else(|| "worker metric setup incomplete".to_string())
        .and_then(|metrics| {
            metrics.capture(
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
                Value::Null,
            )
        })
        .and_then(|value| {
            super::metrics::publish_immutable(&metrics_root.join("terminal.json"), &value)?;
            Ok(value)
        });
    let sampler_close = resources.finish().await;
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
        replicas: 10,
        receipts: (0..10).map(|drive| json!({"drive":drive,"mode":"MRC5","backing":format!("{:032x}",drive+1),"provider_backing_verified":true})).collect(),
        resources: super::resources::example_sample(123),
        core_profile: Value::Null,
        phase_metrics: Value::Null,
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
            provider: "sqlite".into(),
        },
        backend: Backend {
            provider: "sqlite".into(),
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
    let mut ready = example_ready();
    ready.receipts[0] = Value::Null;
    assert!(validate_backing_receipts(&ready).is_err());
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
fn validate_backing_receipts(ready: &Ready) -> Result<Vec<String>, String> {
    if ready.receipts.len() != ready.replicas {
        return Err("backing receipt count mismatch".into());
    }
    ready
        .receipts
        .iter()
        .enumerate()
        .map(|(drive, r)| validate_backing_receipt(r, drive))
        .collect()
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
