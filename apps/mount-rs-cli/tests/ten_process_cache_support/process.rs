//! Owned process controller. File limits are cooperative observations, not emission limits.
use super::{config::sha256, contracts::*};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::Read,
    net::{SocketAddr, UdpSocket},
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// CLOCK_MONOTONIC is the shared boot clock in both owned processes.
pub fn monotonic_ns() -> Result<u64> {
    let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, value.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let value = unsafe { value.assume_init() };
    let seconds = u64::try_from(value.tv_sec).map_err(|_| "negative monotonic seconds")?;
    let nanos = u64::try_from(value.tv_nsec).map_err(|_| "negative monotonic nanos")?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(nanos))
        .ok_or_else(|| "monotonic clock overflow".into())
}
pub fn native_deadline(stamp: u64) -> Result<Instant> {
    let anchor = Instant::now();
    let common_now = monotonic_ns()?;
    anchored_deadline(anchor, common_now, stamp)
}
fn resource_read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(RESOURCE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > RESOURCE_CAP {
        return Err("resource IPC byte cap exceeded".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
fn resource_write<T: Serialize>(root: &Path, name: &str, value: &T) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > RESOURCE_CAP {
        return Err("resource IPC byte cap exceeded".into());
    }
    let temporary = root.join(format!(".{name}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(temporary, root.join(name)).map_err(|e| e.to_string())
}
fn supervisor_identity(role: &str, pid: u32) -> RssIdentity {
    RssIdentity {
        role: role.into(),
        node: role.into(),
        pid,
        generation: 0,
    }
}
fn measured(identity: RssIdentity, verify_parent: bool) -> Result<RssObservation> {
    let started_ns = monotonic_ns()?;
    let sample = if verify_parent && unsafe { libc::getppid() } as u32 != identity.pid {
        Err("controller parent relationship changed".into())
    } else {
        rss(identity.pid)
    };
    let sample = if verify_parent && unsafe { libc::getppid() } as u32 != identity.pid {
        Err("controller parent relationship changed during sample".into())
    } else {
        sample
    };
    let finished_ns = monotonic_ns()?;
    let (bytes, missing) = match sample {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error)),
    };
    Ok(RssObservation {
        identity,
        started_ns,
        finished_ns,
        bytes,
        missing,
    })
}
struct ResourceMonitor {
    run: ResourceRun,
    outer_ns: u64,
    sequence: u64,
    max_total: u64,
    last: Option<ResourceFrame>,
    failure: Option<String>,
    stop_deadline_ns: Option<u64>,
    published_ns: u64,
}
impl ResourceMonitor {
    fn stop_requested(&mut self) -> Result<()> {
        let path = Path::new(&self.run.root).join("stop.json");
        if path.exists() {
            let stop: ResourceStop = resource_read(&path)?;
            stop.validate(&self.run, monotonic_ns()?, self.outer_ns)?;
            self.stop_deadline_ns = Some(
                self.stop_deadline_ns
                    .map_or(stop.deadline_ns, |v| v.min(stop.deadline_ns)),
            );
            return Err(format!("outer resource stop: {}", stop.reason));
        }
        Ok(())
    }
    fn remember(&mut self, error: String) {
        self.failure.get_or_insert(error);
        // The first failure starts one bounded cleanup allowance, never reset by later samples.
        let end = monotonic_ns()
            .ok()
            .and_then(|v| v.checked_add(SHUTDOWN_SECONDS * 1_000_000_000))
            .unwrap_or(self.outer_ns)
            .min(self.outer_ns);
        self.stop_deadline_ns = Some(self.stop_deadline_ns.map_or(end, |v| v.min(end)));
    }
}

pub fn free_disk(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // The path and out pointer remain valid for the synchronous native call.
    if unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let info = unsafe { info.assume_init() };
    let bytes = (info.f_bavail as u128) * (info.f_frsize as u128);
    u64::try_from(bytes).map_err(|_| "free disk counter overflow".into())
}
#[cfg(target_os = "macos")]
fn rss(pid: u32) -> Result<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_taskinfo>();
    // Caller binds this PID to self, a verified parent, or a retained unreaped Child.
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };
    if got != size as libc::c_int {
        return Err("owned child RSS snapshot unavailable".into());
    }
    Ok(unsafe { info.assume_init() }.pti_resident_size)
}
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn rss(pid: u32) -> Result<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/status")).map_err(|e| e.to_string())?;
    let row = text
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .ok_or("owned child RSS snapshot unavailable")?;
    let kb: u64 = row
        .split_whitespace()
        .next()
        .ok_or("missing RSS value")?
        .parse()
        .map_err(|_| "invalid RSS value")?;
    kb.checked_mul(1024)
        .ok_or_else(|| "RSS counter overflow".into())
}
pub fn output(path: &Path, terminal: bool) -> Result<String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > FILE_CAP {
        return Err(format!(
            "cooperative output cap exceeded: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(FILE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    validate_output(&bytes, terminal).map(str::to_owned)
}
fn all_log_bytes(root: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("stdout" | "stderr")
        ) {
            output(&path, false)?;
            total = total
                .checked_add(entry.metadata().map_err(|e| e.to_string())?.len())
                .ok_or("log counter overflow")?;
        }
    }
    if total > TOTAL_CAP {
        return Err("cooperative retained total output cap exceeded".into());
    }
    Ok(total)
}
fn streams(root: &Path, stem: &str) -> Result<(PathBuf, PathBuf, File, File)> {
    let stdout = root.join(format!("{stem}.stdout"));
    let stderr = root.join(format!("{stem}.stderr"));
    let out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stdout)
        .map_err(|e| e.to_string())?;
    let err = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stderr)
        .map_err(|e| e.to_string())?;
    Ok((stdout, stderr, out, err))
}
fn bounded_file(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(FILE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > FILE_CAP {
        return Err("binding file cap exceeded".into());
    }
    Ok(bytes)
}
fn catalog_identity(path: &Path) -> Result<Option<CatalogIdentity>> {
    if !path.exists() {
        return Ok(None);
    }
    let file = fs::metadata(path).map_err(|e| e.to_string())?;
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
    connection
        .busy_timeout(Duration::from_millis(500))
        .map_err(|e| e.to_string())?;
    let (revision, body): (i64, Option<Vec<u8>>) = connection.query_row(
        "SELECT revision, CASE WHEN length(document)<=?1 THEN document ELSE NULL END FROM service_catalog WHERE singleton=1",
        [FILE_CAP], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|e| e.to_string())?;
    let body = body.ok_or("catalog document binding cap exceeded")?;
    Ok(Some(CatalogIdentity {
        device: file.dev(),
        inode: file.ino(),
        revision: u64::try_from(revision).map_err(|_| "negative catalog revision")?,
        document_sha256: sha256(&body),
    }))
}
fn launch_binding(config: &Path, role: &str) -> Result<LaunchBinding> {
    let config = config.canonicalize().map_err(|e| e.to_string())?;
    let bytes = bounded_file(&config)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let catalog = PathBuf::from(
        value["catalog"]
            .as_str()
            .ok_or("configured catalog path missing")?,
    );
    if !catalog.is_absolute() {
        return Err("fixture catalog must be an exact absolute path".into());
    }
    let cli = PathBuf::from(env!("CARGO_BIN_EXE_mount-rs"))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let cli_hash = sha256(&fs::read(&cli).map_err(|e| e.to_string())?);
    let expected_cli_hash = std::env::var("MOUNT_RS_TEN_PROCESS_CLI_SHA256")
        .map_err(|_| "supervisor CLI hash missing")?;
    if cli_hash != expected_cli_hash {
        return Err("CLI hash does not match supervisor's attested binary".into());
    }
    let apply_document = if role == "catalog-apply" {
        let path = PathBuf::from(
            value["document"]
                .as_str()
                .ok_or("apply document path missing")?,
        );
        Some(sha256(&bounded_file(&path)?))
    } else {
        None
    };
    Ok(LaunchBinding {
        config_path: config.to_string_lossy().into_owned(),
        config_sha256: sha256(&bytes),
        cli_binary_path: cli.to_string_lossy().into_owned(),
        cli_binary_sha256: cli_hash,
        catalog_path: catalog.to_string_lossy().into_owned(),
        catalog_at_launch: catalog_identity(&catalog)?,
        catalog_after_completion: None,
        apply_expected_revision: value["expected_revision"].as_u64(),
        apply_document_sha256: apply_document,
    })
}
pub struct OwnedProcess {
    child: Child,
    pub receipt: ProcessReceipt,
    stdout: PathBuf,
    stderr: PathBuf,
    pub quic: Option<SocketAddr>,
    peer: Option<SocketAddr>,
    pub cache: Option<PathBuf>,
    terminal: bool,
    started: Instant,
}
struct ProcessSpec<'a> {
    node: String,
    generation: u64,
    role: &'a str,
    args: &'a [&'a str],
    config: &'a Path,
    peer: Option<SocketAddr>,
    cache: Option<PathBuf>,
}
impl OwnedProcess {
    fn start(root: &Path, spec: ProcessSpec<'_>, deadline: Instant) -> Result<Self> {
        let ProcessSpec {
            node,
            generation,
            role,
            args,
            config,
            peer,
            cache,
        } = spec;
        if Instant::now() >= deadline {
            return Err("expired process start deadline".into());
        }
        let launch = launch_binding(config, role)?;
        let stem = format!("{role}-{node}-{generation}");
        let (stdout, stderr, out, err) = streams(root, &stem)?;
        if Instant::now() >= deadline {
            return Err("late launch binding/capture setup".into());
        }
        let child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
            .args(args)
            .arg(config)
            .current_dir(root)
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| e.to_string())?;
        let receipt = ProcessReceipt {
            launch,
            node,
            generation,
            pid: child.id(),
            role: role.into(),
            reaped: false,
            success: false,
            forced: false,
            sockets_reusable: false,
            disk_lock_reusable: false,
            max_rss_bytes: 0,
            rss_samples: 0,
            rss_first_ns: None,
            rss_last_ns: None,
            stdout_bytes: 0,
            stderr_bytes: 0,
        };
        Ok(Self {
            child,
            receipt,
            stdout,
            stderr,
            quic: None,
            peer,
            cache,
            terminal: false,
            started: Instant::now(),
        })
    }
    fn poll_exit(&mut self) -> Result<()> {
        if self.terminal {
            return Ok(());
        }
        if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
            self.terminal = true;
            self.receipt.reaped = true;
            self.receipt.success = status.success();
        }
        Ok(())
    }
    fn signal(&mut self, value: libc::c_int) -> Result<()> {
        self.poll_exit()?;
        if self.terminal {
            return Err("owned process already exited".into());
        }
        if unsafe { libc::kill(self.child.id() as libc::pid_t, value) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    fn sample_rss(&mut self) -> Result<()> {
        if self.terminal {
            return Ok(());
        }
        let value = rss(self.child.id())?;
        self.record_rss(value)
    }
    fn record_rss(&mut self, value: u64) -> Result<()> {
        let stamp = u64::try_from(self.started.elapsed().as_nanos())
            .map_err(|_| "sample timestamp overflow")?;
        self.receipt.rss_samples += 1;
        self.receipt.rss_first_ns.get_or_insert(stamp);
        self.receipt.rss_last_ns = Some(stamp);
        self.receipt.max_rss_bytes = self.receipt.max_rss_bytes.max(value);
        if value >= RSS_CAP {
            return Err("owned PID RSS cap exceeded".into());
        }
        Ok(())
    }
    fn identity(&self) -> RssIdentity {
        RssIdentity {
            role: self.receipt.role.clone(),
            node: self.receipt.node.clone(),
            pid: self.child.id(),
            generation: self.receipt.generation,
        }
    }
    fn sample(&mut self) -> Result<()> {
        self.poll_exit()?;
        if self.terminal {
            return Err(format!(
                "{} exited before requested stop",
                self.receipt.node
            ));
        }
        self.sample_rss()?;
        output(&self.stdout, false)?;
        output(&self.stderr, false)?;
        Ok(())
    }
    fn reuse(&mut self) -> Result<()> {
        // These are fresh binds after the actual Child reap, separate from shutdown counters.
        for address in [self.quic, self.peer].into_iter().flatten() {
            let socket = UdpSocket::bind(address)
                .map_err(|e| format!("socket not reusable {address}: {e}"))?;
            drop(socket);
        }
        self.receipt.sockets_reusable = true;
        if let Some(cache) = &self.cache {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(cache.join(".lock"))
                .map_err(|e| e.to_string())?;
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err("cache lock not released after actual process reap".into());
            }
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) } != 0 {
                return Err("cache lock proof unlock failed".into());
            }
        }
        self.receipt.disk_lock_reusable = true;
        Ok(())
    }
    fn finish(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<std::collections::BTreeMap<String, Bank>>> {
        if !self.terminal {
            return Err("cannot finish an unreaped process".into());
        }
        if Instant::now() >= deadline {
            return Err("late process completion".into());
        }
        self.receipt.stdout_bytes = fs::metadata(&self.stdout).map_err(|e| e.to_string())?.len();
        self.receipt.stderr_bytes = fs::metadata(&self.stderr).map_err(|e| e.to_string())?.len();
        output(&self.stdout, true)?;
        let stderr = output(&self.stderr, true)?;
        self.reuse()?;
        let binding = &mut self.receipt.launch;
        if sha256(&bounded_file(Path::new(&binding.config_path))?) != binding.config_sha256
            || sha256(&fs::read(&binding.cli_binary_path).map_err(|e| e.to_string())?)
                != binding.cli_binary_sha256
        {
            return Err("launched config or CLI binary changed during generation".into());
        }
        binding.catalog_after_completion = catalog_identity(Path::new(&binding.catalog_path))?;
        self.receipt.qualify(
            &self.receipt.node,
            self.receipt.generation,
            self.receipt.pid,
        )?;
        let banks = if self.receipt.role == "server" {
            Some(parse_banks(&stderr)?)
        } else {
            None
        };
        if Instant::now() >= deadline {
            return Err("late process proof/parse completion".into());
        }
        Ok(banks)
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // Never wait in Drop. The independently owned outer group watchdog covers stalled cleanup.
        if !self.terminal {
            self.receipt.forced = true;
            let _ = self.child.kill();
            if let Ok(Some(status)) = self.child.try_wait() {
                self.terminal = true;
                self.receipt.reaped = true;
                self.receipt.success = status.success();
            }
        }
    }
}
pub struct Fleet {
    pub root: PathBuf,
    pub processes: Vec<OwnedProcess>,
    pub retired: Vec<ProcessReceipt>,
    pub banks: Vec<serde_json::Value>,
    next_generation: u64,
    resources: ResourceMonitor,
}
impl Fleet {
    pub fn new(root: &Path, controller_pid: u32, outer_ns: u64) -> Result<Self> {
        let run = ResourceRun {
            root: root.to_string_lossy().into_owned(),
            controller_pid,
            worker_pid: std::process::id(),
            group: unsafe { libc::getpgrp() } as u32,
        };
        let mut fleet = Self {
            root: root.into(),
            processes: Vec::new(),
            retired: Vec::new(),
            banks: Vec::new(),
            next_generation: 1,
            resources: ResourceMonitor {
                run,
                outer_ns,
                sequence: 0,
                max_total: 0,
                last: None,
                failure: None,
                stop_deadline_ns: None,
                published_ns: 0,
            },
        };
        // The first report is sampled with zero children before runtime or fixture construction.
        fleet.refresh_resources(true, false)?;
        resource_write(
            root,
            "resource-initial.json",
            fleet
                .resources
                .last
                .as_ref()
                .ok_or("initial RSS frame missing")?,
        )?;
        Ok(fleet)
    }
    pub fn cleanup_deadline(&mut self) -> Result<Instant> {
        if let Err(error) = self.resources.stop_requested() {
            self.resources.remember(error);
        }
        let now = monotonic_ns()?;
        let own = now
            .checked_add(SHUTDOWN_SECONDS * 1_000_000_000)
            .ok_or("cleanup timestamp overflow")?;
        native_deadline(
            self.resources
                .stop_deadline_ns
                .unwrap_or(own)
                .min(own)
                .min(self.resources.outer_ns),
        )
    }
    pub fn resource_evidence(&self) -> serde_json::Value {
        json!({"clock":"CLOCK_MONOTONIC","cooperative_skewed":true,
            "aggregate_cap_bytes":RSS_CAP,"individual_cap_bytes":RSS_CAP,
            "max_observed_total_bytes":self.resources.max_total,"sequence":self.resources.sequence,
            "failure":self.resources.failure,"last":self.resources.last})
    }
    pub fn owned_cleanup_closed(&self) -> bool {
        self.processes.is_empty()
            && self
                .retired
                .iter()
                .all(|p| p.reaped && p.sockets_reusable && p.disk_lock_reusable)
            && self
                .resources
                .last
                .as_ref()
                .is_some_and(|f| f.terminal && f.expected.len() == 2)
    }
    pub fn refresh_resources(&mut self, force: bool, terminal: bool) -> Result<()> {
        let result = self.sample_resources(force, terminal);
        if let Err(error) = &result {
            self.resources.remember(error.clone());
        }
        result
    }
    fn sample_resources(&mut self, force: bool, terminal: bool) -> Result<()> {
        if let Err(error) = self.resources.stop_requested() {
            self.resources.remember(error);
        }
        let started_ns = monotonic_ns()?;
        let mut expected = vec![
            supervisor_identity("controller", self.resources.run.controller_pid),
            supervisor_identity("worker", self.resources.run.worker_pid),
        ];
        // Take the exact roster after actual reaps; no retired PID is queried.
        for process in &mut self.processes {
            process.poll_exit()?;
        }
        expected.extend(
            self.processes
                .iter()
                .filter(|p| !p.terminal)
                .map(OwnedProcess::identity),
        );
        let mut observations = vec![
            measured(expected[0].clone(), true)?,
            measured(expected[1].clone(), false)?,
        ];
        for process in self.processes.iter_mut().filter(|p| !p.terminal) {
            let observation = measured(process.identity(), false)?;
            if let Some(bytes) = observation.bytes
                && let Err(error) = process.record_rss(bytes)
            {
                self.resources.remember(error);
            }
            observations.push(observation);
        }
        let finished_ns = monotonic_ns()?;
        let total_bytes = observations
            .iter()
            .try_fold(0u64, |sum, v| sum.checked_add(v.bytes?));
        let child_bytes = observations
            .iter()
            .skip(2)
            .try_fold(0u64, |sum, v| sum.checked_add(v.bytes?));
        if let Some(total) = total_bytes {
            self.resources.max_total = self.resources.max_total.max(total);
        }
        if let Err(error) = rss_totals(&expected, &observations) {
            self.resources.remember(error);
        }
        self.resources.sequence = self
            .resources
            .sequence
            .checked_add(1)
            .ok_or("resource sequence overflow")?;
        let roster_changed = self
            .resources
            .last
            .as_ref()
            .is_none_or(|v| v.expected != expected);
        let frame = ResourceFrame {
            schema: 1,
            run: self.resources.run.clone(),
            sequence: self.resources.sequence,
            started_ns,
            finished_ns,
            expected,
            observations,
            total_bytes,
            child_bytes,
            max_total_bytes: self.resources.max_total,
            error: self.resources.failure.clone(),
            terminal,
        };
        let should_publish = force
            || roster_changed
            || frame.error.is_some()
            || finished_ns.saturating_sub(self.resources.published_ns) >= POLL_MS * 1_000_000;
        self.resources.last = Some(frame);
        if should_publish {
            resource_write(
                &self.root,
                "resource.json",
                self.resources.last.as_ref().ok_or("RSS frame missing")?,
            )?;
            self.resources.published_ns = finished_ns;
        }
        match &self.resources.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
    pub fn generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation += 1;
        generation
    }
    pub fn check(&mut self) -> Result<()> {
        self.refresh_resources(false, false)?;
        all_log_bytes(&self.root)?;
        if free_disk(&self.root)? < FREE_DISK_FLOOR {
            return Err("free disk below 64 GiB floor".into());
        }
        for process in &mut self.processes {
            process.sample()?;
        }
        Ok(())
    }
    pub fn launch(
        &mut self,
        node: usize,
        generation: u64,
        config: &Path,
        peer: SocketAddr,
        cache: &Path,
        deadline: Instant,
    ) -> Result<SocketAddr> {
        let process = OwnedProcess::start(
            &self.root,
            ProcessSpec {
                node: format!("node-{node}"),
                generation,
                role: "server",
                args: &["serve-remote", "--config"],
                config,
                peer: Some(peer),
                cache: Some(cache.into()),
            },
            deadline,
        )?;
        self.processes.push(process);
        self.refresh_resources(true, false)?;
        let index = self.processes.len() - 1;
        loop {
            if Instant::now() >= deadline {
                return Err("CLI readiness deadline exhausted".into());
            }
            self.check()?;
            let text = output(&self.processes[index].stdout, false)?;
            for line in text
                .split_inclusive('\n')
                .filter(|line| line.ends_with('\n'))
            {
                if let Some(address) = line.trim_end().strip_prefix("remote listening at ") {
                    let address: SocketAddr =
                        address.parse().map_err(|_| "invalid CLI readiness")?;
                    if !address.ip().is_loopback() {
                        return Err("non-private CLI endpoint".into());
                    }
                    self.processes[index].quic = Some(address);
                    if Instant::now() >= deadline {
                        return Err("late CLI readiness".into());
                    }
                    return Ok(address);
                }
            }
            if Instant::now() >= deadline {
                return Err("CLI readiness deadline exhausted".into());
            }
            thread::sleep(Duration::from_millis(POLL_MS));
        }
    }
    pub fn apply(&mut self, config: &Path, deadline: Instant) -> Result<()> {
        let generation = self.generation();
        let process = OwnedProcess::start(
            &self.root,
            ProcessSpec {
                node: "catalog".into(),
                generation,
                role: "catalog-apply",
                args: &["catalog-apply", "--config"],
                config,
                peer: None,
                cache: None,
            },
            deadline,
        )?;
        self.processes.push(process);
        self.refresh_resources(true, false)?;
        let index = self.processes.len() - 1;
        loop {
            if Instant::now() >= deadline {
                return Err("public catalog-apply deadline exhausted".into());
            }
            all_log_bytes(&self.root)?;
            self.refresh_resources(false, false)?;
            self.processes[index].poll_exit()?;
            if self.processes[index].terminal {
                self.processes[index].finish(deadline)?;
                let process = self.processes.remove(index);
                self.retired.push(process.receipt.clone());
                self.refresh_resources(true, false)?;
                return Ok(());
            }
            self.processes[index].sample_rss()?;
            for (other, process) in self.processes.iter_mut().enumerate() {
                if other != index {
                    process.sample()?;
                }
            }
            thread::sleep(Duration::from_millis(POLL_MS));
        }
    }
    pub fn address(&self, n: usize) -> Result<SocketAddr> {
        self.processes
            .iter()
            .find(|p| p.receipt.node == format!("node-{n}"))
            .and_then(|p| p.quic)
            .ok_or_else(|| "requested node is not active".into())
    }
    pub fn cache(&self, n: usize) -> Result<PathBuf> {
        self.processes
            .iter()
            .find(|p| p.receipt.node == format!("node-{n}"))
            .and_then(|p| p.cache.clone())
            .ok_or_else(|| "requested cache is not active".into())
    }
    pub fn stop_node(
        &mut self,
        n: usize,
        deadline: Instant,
    ) -> Result<std::collections::BTreeMap<String, Bank>> {
        let index = self
            .processes
            .iter()
            .position(|p| p.receipt.node == format!("node-{n}"))
            .ok_or("node not active")?;
        if Instant::now() >= deadline {
            return Err("expired stop deadline".into());
        }
        self.processes[index].signal(libc::SIGINT)?;
        let force_at = deadline
            .checked_sub(Duration::from_secs(FORCE_SECONDS))
            .ok_or("invalid stop deadline")?;
        loop {
            if Instant::now() >= deadline {
                return Err("node reap deadline exhausted".into());
            }
            all_log_bytes(&self.root)?;
            self.refresh_resources(false, false)?;
            if free_disk(&self.root)? < FREE_DISK_FLOOR {
                return Err("disk floor lost during node shutdown".into());
            }
            self.processes[index].poll_exit()?;
            if self.processes[index].terminal {
                let banks = self.processes[index]
                    .finish(deadline)?
                    .ok_or("server bank missing")?;
                let process = self.processes.remove(index);
                self.banks.push(json!({"node":process.receipt.node,"generation":process.receipt.generation,
                    "pid":process.receipt.pid,"scope":"cumulative generation shutdown logical counters",
                    "launch":process.receipt.launch,"maintenance_quiescence":"unavailable","rows":banks}));
                self.retired.push(process.receipt.clone());
                self.refresh_resources(true, false)?;
                return Ok(banks);
            }
            for (other, process) in self.processes.iter_mut().enumerate() {
                if other == index {
                    process.sample_rss()?;
                } else {
                    process.sample()?;
                }
            }
            if Instant::now() >= force_at {
                self.processes[index].receipt.forced = true;
                self.processes[index]
                    .child
                    .kill()
                    .map_err(|e| e.to_string())?;
            }
            thread::sleep(Duration::from_millis(POLL_MS));
        }
    }
    pub fn stop_all(&mut self, mut deadline: Instant) -> Result<()> {
        // Signal the fleet together; one shared deadline bounds all server shutdowns.
        let mut failure = self.resources.failure.clone();
        if let Err(error) = self.refresh_resources(true, self.processes.is_empty()) {
            failure.get_or_insert(error);
        }
        if let Some(stamp) = self.resources.stop_deadline_ns {
            deadline = deadline.min(native_deadline(stamp).unwrap_or_else(|_| Instant::now()));
        }
        for process in &mut self.processes {
            if !process.terminal
                && let Err(error) = process.signal(libc::SIGINT)
            {
                failure.get_or_insert(error);
            }
        }
        while !self.processes.is_empty() && Instant::now() < deadline {
            if let Err(error) = self.refresh_resources(false, false) {
                failure.get_or_insert(error);
            }
            if let Some(stamp) = self.resources.stop_deadline_ns {
                deadline = deadline.min(native_deadline(stamp).unwrap_or_else(|_| Instant::now()));
            }
            let force_at = deadline
                .checked_sub(Duration::from_secs(FORCE_SECONDS))
                .unwrap_or(deadline);
            if let Err(error) = all_log_bytes(&self.root) {
                failure.get_or_insert(error);
            }
            if free_disk(&self.root).unwrap_or(0) < FREE_DISK_FLOOR {
                failure.get_or_insert("disk floor lost during cleanup".into());
            }
            for index in (0..self.processes.len()).rev() {
                let process = &mut self.processes[index];
                if let Err(error) = process.poll_exit() {
                    failure.get_or_insert(error);
                }
                if process.terminal {
                    let evidence = process.finish(deadline);
                    match evidence {
                            Ok(Some(rows)) => self.banks.push(json!({"node":process.receipt.node,
                                "generation":process.receipt.generation,"pid":process.receipt.pid,
                                "scope":"cumulative generation shutdown logical counters",
                                "launch":process.receipt.launch,"maintenance_quiescence":"unavailable","rows":rows})),
                            Ok(None) => {}, Err(error) => { failure.get_or_insert(error); }
                        }
                    let process = self.processes.remove(index);
                    self.retired.push(process.receipt.clone());
                } else {
                    if let Err(error) = process.sample_rss() {
                        failure.get_or_insert(error);
                    }
                    if Instant::now() >= force_at {
                        process.receipt.forced = true;
                        let _ = process.child.kill();
                    }
                    if let Err(error) =
                        output(&process.stdout, false).and_then(|_| output(&process.stderr, false))
                    {
                        process.receipt.forced = true;
                        let _ = process.child.kill();
                        failure.get_or_insert(error);
                    }
                }
            }
            if let Err(error) = self.refresh_resources(true, self.processes.is_empty()) {
                failure.get_or_insert(error);
            }
            thread::sleep(Duration::from_millis(POLL_MS));
        }
        if !self.processes.is_empty() {
            return Err("fleet reap deadline exhausted; evidence incomplete".into());
        }
        if let Err(error) = self.refresh_resources(true, true) {
            failure.get_or_insert(error);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        all_log_bytes(&self.root)?;
        if Instant::now() >= deadline {
            return Err("late cleanup proof completion".into());
        }
        Ok(())
    }
}

struct WorkerGuard {
    child: Child,
    group: u32,
    reaped: bool,
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if !self.reaped {
            // The leader is still unreaped, so its owned group ID cannot have been reused.
            let _ = unsafe { libc::kill(-(self.group as libc::pid_t), libc::SIGKILL) };
            let _ = self.child.try_wait();
        }
    }
}
fn group_gone(group: u32) -> bool {
    (unsafe { libc::kill(-(group as libc::pid_t), 0) }) == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}
fn exited_without_reap(child: &Child) -> Result<bool> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // WNOWAIT retains the leader identity until any necessary original-group kill is complete.
    if unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { info.assume_init().si_pid() } == child.id() as libc::pid_t)
}
fn receipt_value(path: &Path) -> Result<serde_json::Value> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(FILE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > FILE_CAP {
        return Err("receipt byte cap exceeded".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

struct OuterResourceSample {
    frame: ResourceFrame,
    totals: RssTotals,
    observations: Vec<RssObservation>,
    limit_error: Option<String>,
}
fn outer_sample(root: &Path, run: &ResourceRun, sequence: u64) -> Result<OuterResourceSample> {
    let frame: ResourceFrame = resource_read(&root.join("resource.json"))?;
    frame.validate(run, sequence, monotonic_ns()?)?;
    // Replace producer supervisor samples; only child observations/subtotal cross the IPC seam.
    let supervisors = [
        measured(supervisor_identity("controller", run.controller_pid), false)?,
        measured(supervisor_identity("worker", run.worker_pid), false)?,
    ];
    let (totals, observations) =
        frame.recompose_observed(run, sequence, monotonic_ns()?, supervisors)?;
    let limit_error = rss_caps(&totals, &observations).err();
    Ok(OuterResourceSample {
        frame,
        totals,
        observations,
        limit_error,
    })
}

pub fn supervise() {
    assert_eq!(
        std::env::var("MOUNT_RS_TEN_PROCESS_RUN").as_deref(),
        Ok("1"),
        "explicit native RUN=1 required"
    );
    let root = PathBuf::from(
        std::env::var_os("MOUNT_RS_TEN_PROCESS_OUTPUT").expect("fresh private OUTPUT required"),
    );
    assert!(
        root.is_absolute() && !root.exists(),
        "OUTPUT must be a fresh absolute private path"
    );
    fs::create_dir(&root).expect("retain private evidence root from acquisition");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("private evidence root permissions");
    let started_ns = monotonic_ns().expect("shared boot clock");
    let setup_ns = started_ns + SETUP_SECONDS * 1_000_000_000;
    let outer_ns = started_ns
        + (SETUP_SECONDS + WORK_SECONDS + SHUTDOWN_SECONDS + AUDIT_SECONDS) * 1_000_000_000;
    let deadline = native_deadline(outer_ns).expect("outer common-clock budget");
    let initial_limit_ns = (started_ns + REQUEST_SECONDS * 1_000_000_000).min(setup_ns);
    let initial_deadline = native_deadline(initial_limit_ns).expect("initial report budget");
    println!("ten-process cache evidence: {}", root.display());
    let available = free_disk(&root).expect("free disk preflight");
    assert!(
        available >= FREE_DISK_FLOOR,
        "native preflight requires 64 GiB free"
    );
    let sentinel = UdpSocket::bind("127.0.0.1:0").expect("unrelated sentinel");
    let sentinel_address = sentinel.local_addr().expect("sentinel address");
    let (out_path, err_path, out, err) = streams(&root, "worker").expect("worker capture");
    let executable = std::env::current_exe().expect("test binary");
    let binary = fs::read(&executable).expect("test binary attestation");
    let cli = fs::read(env!("CARGO_BIN_EXE_mount-rs")).expect("CLI binary attestation");
    assert!(Instant::now() < deadline, "expired supervisor start budget");
    let child = Command::new(&executable)
        .args(["--ignored", "--exact", "native_worker", "--nocapture"])
        .env("MOUNT_RS_TEN_PROCESS_ROOT", &root)
        .env("MOUNT_RS_TEN_PROCESS_SETUP_NS", setup_ns.to_string())
        .env("MOUNT_RS_TEN_PROCESS_OUTER_NS", outer_ns.to_string())
        .env(
            "MOUNT_RS_TEN_PROCESS_SUPERVISOR",
            std::process::id().to_string(),
        )
        .env("MOUNT_RS_TEN_PROCESS_CLI_SHA256", sha256(&cli))
        .env(
            "MOUNT_RS_TEN_PROCESS_SENTINEL",
            sentinel_address.to_string(),
        )
        .process_group(0)
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .expect("owned worker");
    let group = child.id();
    let run = ResourceRun {
        root: root.to_string_lossy().into_owned(),
        controller_pid: std::process::id(),
        worker_pid: group,
        group,
    };
    let mut worker = WorkerGuard {
        child,
        group,
        reaped: false,
    };
    let initial = json!({"schema":1,"complete":false,"worker_pid":group,"process_group":group,
        "supervisor_pid":std::process::id(),"worker_reaped":false,"forced":false,
        "failure":"controller did not reach a terminal receipt; retained ownership only",
        "private_credential_cleanup":"pending; private key/token files must not be exported",
        "test_binary_sha256":sha256(&binary),"cli_binary_sha256":sha256(&cli)});
    fs::write(
        root.join("controller.json"),
        serde_json::to_vec_pretty(&initial).unwrap(),
    )
    .expect("initial incomplete ownership receipt");
    let mut stop_deadline = None;
    let mut failure = None;
    let mut forced = false;
    let mut max_total = 0;
    let mut controller_max_rss = 0;
    let mut worker_max_rss = 0;
    let mut sample_count = 0u64;
    let mut last_sequence = 0;
    let mut initial_observed = false;
    let mut last_resource = None;
    let status = loop {
        if Instant::now() >= deadline {
            panic!("worker reap budget exhausted; retained incomplete evidence");
        }
        if exited_without_reap(&worker.child).expect("worker WNOWAIT poll") {
            let cleanup_receipt = receipt_value(&root.join("receipt.json"))
                .map(|value| {
                    value["owned_cleanup_closed"] == true
                        && value["worker_pid"] == group
                        && value["supervisor_pid"] == std::process::id()
                        && value["process_group"] == group
                })
                .unwrap_or(false);
            if !cleanup_receipt {
                forced = true;
                failure.get_or_insert(
                    "worker exited without physical child-reap/socket/lock closure receipt".into(),
                );
                // The original group leader is still unreaped and its identity cannot be reused.
                let _ = unsafe { libc::kill(-(group as libc::pid_t), libc::SIGKILL) };
            }
            let status = worker
                .child
                .try_wait()
                .expect("worker terminal reap")
                .expect("WNOWAIT observed exit");
            worker.reaped = true; // Permanently disarm numeric group actions after reap.
            break status;
        }
        if !initial_observed {
            let path = root.join("resource-initial.json");
            if Instant::now() >= initial_deadline
                || monotonic_ns().expect("initial pre-read clock") >= initial_limit_ns
            {
                failure.get_or_insert("initial report observation deadline exhausted".into());
            } else if path.exists() {
                let initial: Result<ResourceFrame> = resource_read(&path);
                match initial.and_then(|v| {
                    v.validate_initial(&run, monotonic_ns()?, initial_limit_ns)?;
                    // Parsing/validation is cooperative; recheck before accepting the positive result.
                    if Instant::now() >= initial_deadline || monotonic_ns()? >= initial_limit_ns {
                        return Err(
                            "initial report observation deadline exhausted after validation".into(),
                        );
                    }
                    Ok(v)
                }) {
                    Ok(_) => initial_observed = true,
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            } else if Instant::now() >= initial_deadline {
                failure.get_or_insert(
                    "initial sampled empty-child report missing before initial/setup deadline"
                        .into(),
                );
            }
        }
        if initial_observed {
            match outer_sample(&root, &run, last_sequence) {
                Ok(OuterResourceSample {
                    frame,
                    totals,
                    observations,
                    limit_error,
                }) => {
                    max_total = max_total.max(totals.total);
                    controller_max_rss = controller_max_rss
                        .max(observations[0].bytes.expect("validated controller RSS"));
                    worker_max_rss =
                        worker_max_rss.max(observations[1].bytes.expect("validated worker RSS"));
                    sample_count += 1;
                    last_sequence = frame.sequence;
                    if let Some(error) = &limit_error {
                        failure.get_or_insert(error.clone());
                    }
                    last_resource = Some(
                        json!({"frame":frame,"recomposed_total_bytes":totals.total,
                        "child_only_bytes":totals.children,"observations":observations,"limit_error":limit_error}),
                    );
                }
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        if let Err(error) = all_log_bytes(&root) {
            failure.get_or_insert(error);
            // Output overflow retains the existing immediate original-group termination policy.
            forced = true;
            let _ = unsafe { libc::kill(-(group as libc::pid_t), libc::SIGKILL) };
        }
        if free_disk(&root).unwrap_or(0) < FREE_DISK_FLOOR {
            failure.get_or_insert("disk floor lost".to_owned());
        }
        if failure.is_some() && stop_deadline.is_none() {
            let requested_ns = monotonic_ns().expect("stop request clock");
            let deadline_ns = (requested_ns + SHUTDOWN_SECONDS * 1_000_000_000).min(outer_ns);
            let stop = ResourceStop {
                schema: 1,
                run: run.clone(),
                requested_ns,
                deadline_ns,
                reason: failure.clone().expect("sticky failure"),
            };
            if let Err(error) = resource_write(&root, "stop.json", &stop) {
                failure.get_or_insert(error);
            }
            stop_deadline = Some(native_deadline(deadline_ns).unwrap_or_else(|_| Instant::now()));
        }
        let effective_deadline = stop_deadline.unwrap_or(deadline).min(deadline);
        let force_at = effective_deadline
            .checked_sub(Duration::from_secs(FORCE_SECONDS))
            .unwrap_or(effective_deadline);
        if Instant::now() >= force_at && !forced {
            forced = true;
            // Child is still owned and unreaped; no unrelated process-group ID is accepted.
            if unsafe { libc::kill(-(group as libc::pid_t), libc::SIGKILL) } != 0 {
                failure.get_or_insert("owned worker process-group kill failed".to_owned());
            }
        }
        if Instant::now() >= effective_deadline {
            panic!("worker reap budget exhausted; no grandchild reap claim");
        }
        thread::sleep(Duration::from_millis(POLL_MS));
    };
    if !group_gone(group) {
        forced = true;
        failure.get_or_insert(
            "worker exited with owned descendants; no fabricated grandchild reap".into(),
        );
        // Read-only disappearance observation after reap; never signal a possibly reused group ID.
        while !group_gone(group) && Instant::now() < stop_deadline.unwrap_or(deadline).min(deadline)
        {
            thread::sleep(Duration::from_millis(POLL_MS));
        }
    }
    let group_gone = group_gone(group);
    let sentinel_owned = sentinel.local_addr().ok() == Some(sentinel_address)
        && UdpSocket::bind(sentinel_address).is_err();
    let capture = output(&out_path, true).and_then(|_| output(&err_path, true));
    let controller = json!({"schema":1,"scope":"SQLite public CLI debug local OIDC fixture",
        "worker_pid":group,"process_group":group,"supervisor_pid":std::process::id(),
        "test_binary_sha256":sha256(&binary),"cli_binary_sha256":sha256(&cli),
        "debug_assertions":cfg!(debug_assertions),"local_oidc_fixture":cfg!(feature="local-oidc-fixture"),
        "forced":forced,"worker_reaped":true,"worker_success":status.success(),
        "group_gone":group_gone,"unrelated_sentinel_retained":sentinel_owned,
        "worker_max_rss_bytes":worker_max_rss,"controller_max_rss_bytes":controller_max_rss,
        "owned_aggregate_max_observed_bytes":max_total,"resource_sample_count":sample_count,
        "resource_initial_observed":initial_observed,"resource_last_sequence":last_sequence,
        "resource_last":last_resource,"resource_clock":"CLOCK_MONOTONIC",
        "resource_fresh_ns":RESOURCE_FRESH_NS,"resource_sampling":"cooperative sequential/skewed; not a hard bound",
        "owned_aggregate_rss_cap_bytes":RSS_CAP,"cooperative_poll_ms":POLL_MS,"failure":failure});
    fs::write(
        root.join("controller.json"),
        serde_json::to_vec_pretty(&controller).unwrap(),
    )
    .expect("controller receipt");
    let receipt_path = root.join("receipt.json");
    let receipt = receipt_value(&receipt_path).unwrap_or(json!({"complete":false}));
    let retained = root;
    assert!(
        capture.is_ok()
            && !forced
            && failure.is_none()
            && status.success()
            && group_gone
            && sentinel_owned
            && receipt["complete"] == true
            && initial_observed
            && sample_count > 0
            && Instant::now() < deadline,
        "qualification incomplete; inspect {}",
        retained.display()
    );
}
