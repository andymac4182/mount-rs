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

fn binary_digest_reader(
    reader: &mut impl Read,
    progress: &mut impl FnMut() -> Result<()>,
) -> Result<String> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut bytes = [0u8; 64 * 1024];
    loop {
        progress()?;
        let length = match reader.read(&mut bytes) {
            Ok(length) => length,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        };
        digest.update(&bytes[..length]);
        progress()?;
        if length == 0 {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut encoded = String::with_capacity(64);
            for byte in digest.finish().as_ref() {
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0f) as usize] as char);
            }
            return Ok(encoded);
        }
    }
}
fn binary_digest_file(path: &Path, progress: &mut impl FnMut() -> Result<()>) -> Result<String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    let digest = binary_digest_reader(&mut file, progress)?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    let current = fs::metadata(path).map_err(|e| e.to_string())?;
    let identity = |value: &fs::Metadata| {
        (
            value.dev(),
            value.ino(),
            value.len(),
            value.mtime(),
            value.mtime_nsec(),
            value.ctime(),
            value.ctime_nsec(),
        )
    };
    if !before.is_file()
        || identity(&before) != identity(&after)
        || identity(&before) != identity(&current)
    {
        return Err("binary changed during attestation".into());
    }
    progress()?;
    Ok(digest)
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
const FIRST_RSS_CAP: usize = 2048;
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RssCategory {
    ProcStatusReadFailed,
    VmrssFieldMissing,
    VmrssValueMissing,
    VmrssValueInvalid,
    RssBytesOverflow,
    TaskinfoUnavailable,
    ParentChangedBeforeSample,
    ParentChangedAfterSample,
    SampleClockBeforeFailed,
    SampleClockAfterFailed,
    RetirementPollFailed,
    UnexpectedRetirement,
    SampleTimestampOverflow,
    PidRssCapExceeded,
    OtherRssError,
}
impl RssCategory {
    fn permits_retirement(self) -> bool {
        matches!(self, Self::VmrssFieldMissing | Self::TaskinfoUnavailable)
    }
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RssSite {
    WorkerFrameController,
    WorkerFrameWorker,
    WorkerFrameChild,
    ReadinessChild,
    ActiveChild,
    ApplyTarget,
    ApplySibling,
    StopTarget,
    StopSibling,
    CleanupChild,
    OuterController,
    OuterWorker,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RssPoll {
    NotApplicable,
    NotAttempted,
    Running,
    ReapedSuccess,
    ReapedFailure,
    PollError,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum RssRole {
    Controller,
    Worker,
    Server,
    CatalogApply,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RssProducer {
    Worker,
    Controller,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RssRetention {
    NotAttempted,
    Written,
    WriteFailed,
}
#[derive(Clone, Copy)]
struct RssCandidate {
    role: RssRole,
    pid: u32,
    generation: u64,
    node: Option<u8>,
    site: RssSite,
    category: RssCategory,
    poll: RssPoll,
    observed_ns: Option<u64>,
    bytes: Option<u64>,
}
impl RssCandidate {
    fn from_identity(
        identity: &RssIdentity,
        site: RssSite,
        category: RssCategory,
        poll: RssPoll,
        observed_ns: Option<u64>,
        bytes: Option<u64>,
    ) -> Option<Self> {
        if identity.pid == 0 {
            return None;
        }
        let (role, node) = match identity.role.as_str() {
            "controller" if identity.generation == 0 && identity.node == "controller" => {
                (RssRole::Controller, None)
            }
            "worker" if identity.generation == 0 && identity.node == "worker" => {
                (RssRole::Worker, None)
            }
            "catalog-apply" if identity.generation > 0 && identity.node == "catalog" => {
                (RssRole::CatalogApply, None)
            }
            "server" if identity.generation > 0 => {
                let node = identity.node.strip_prefix("node-")?.parse::<u8>().ok()?;
                if node >= 10 || identity.node != format!("node-{node}") {
                    return None;
                }
                (RssRole::Server, Some(node))
            }
            _ => return None,
        };
        Some(Self {
            role,
            pid: identity.pid,
            generation: identity.generation,
            node,
            site,
            category,
            poll,
            observed_ns,
            bytes,
        })
    }
}
#[derive(Clone, Copy, Serialize)]
struct FirstRssFailure {
    schema: &'static str,
    producer: RssProducer,
    controller_pid: u32,
    worker_pid: u32,
    group: u32,
    observed_ns: Option<u64>,
    resource_sequence: u64,
    candidate_role: RssRole,
    candidate_pid: u32,
    candidate_generation: u64,
    candidate_node: Option<u8>,
    site: RssSite,
    category: RssCategory,
    retirement_poll: RssPoll,
    candidate_bytes: Option<u64>,
}
impl FirstRssFailure {
    fn new(
        run: &ResourceRun,
        producer: RssProducer,
        sequence: u64,
        candidate: RssCandidate,
    ) -> Self {
        Self {
            schema: "mount-rs.cache-rss-failure.v1",
            producer,
            controller_pid: run.controller_pid,
            worker_pid: run.worker_pid,
            group: run.group,
            observed_ns: candidate.observed_ns,
            resource_sequence: sequence,
            candidate_role: candidate.role,
            candidate_pid: candidate.pid,
            candidate_generation: candidate.generation,
            candidate_node: candidate.node,
            site: candidate.site,
            category: candidate.category,
            retirement_poll: candidate.poll,
            candidate_bytes: candidate.bytes,
        }
    }
}
#[derive(Debug)]
struct RssReadError {
    category: RssCategory,
    message: String,
    observed_ns: Option<u64>,
    bytes: Option<u64>,
}
impl RssReadError {
    fn new(category: RssCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            observed_ns: None,
            bytes: None,
        }
    }
    fn unavailable() -> Self {
        #[cfg(target_os = "macos")]
        let category = RssCategory::TaskinfoUnavailable;
        #[cfg(not(target_os = "macos"))]
        let category = RssCategory::VmrssFieldMissing;
        Self::new(category, RSS_UNAVAILABLE)
    }
}
// Unclassified injected/private errors remain Other; their strings never select a category.
impl From<String> for RssReadError {
    fn from(message: String) -> Self {
        Self::new(RssCategory::OtherRssError, message)
    }
}
impl From<&str> for RssReadError {
    fn from(message: &str) -> Self {
        Self::new(RssCategory::OtherRssError, message)
    }
}
type RssReadResult<T> = std::result::Result<T, RssReadError>;
struct RssAcquisition {
    observation: RssObservation,
    category: Option<RssCategory>,
}
impl From<RssObservation> for RssAcquisition {
    fn from(observation: RssObservation) -> Self {
        Self {
            observation,
            category: None,
        }
    }
}
impl RssAcquisition {
    fn category(&self) -> Option<RssCategory> {
        self.category.or_else(|| {
            if self.observation.bytes.is_none() || self.observation.missing.is_some() {
                Some(RssCategory::OtherRssError)
            } else if self.observation.bytes.is_some_and(|v| v >= RSS_CAP) {
                Some(RssCategory::PidRssCapExceeded)
            } else {
                None
            }
        })
    }
}
fn supervisor_identity(role: &str, pid: u32) -> RssIdentity {
    RssIdentity {
        role: role.into(),
        node: role.into(),
        pid,
        generation: 0,
    }
}
fn measured(identity: RssIdentity, verify_parent: bool) -> RssReadResult<RssAcquisition> {
    let started_ns = monotonic_ns()
        .map_err(|error| RssReadError::new(RssCategory::SampleClockBeforeFailed, error))?;
    let sample = if verify_parent && unsafe { libc::getppid() } as u32 != identity.pid {
        Err(RssReadError::new(
            RssCategory::ParentChangedBeforeSample,
            "controller parent relationship changed",
        ))
    } else {
        rss(identity.pid)
    };
    let sample = if verify_parent && unsafe { libc::getppid() } as u32 != identity.pid {
        Err(RssReadError::new(
            RssCategory::ParentChangedAfterSample,
            "controller parent relationship changed during sample",
        ))
    } else {
        sample
    };
    let finished_ns = monotonic_ns().map_err(|message| RssReadError {
        category: RssCategory::SampleClockAfterFailed,
        message,
        observed_ns: Some(started_ns),
        bytes: sample.as_ref().ok().copied(),
    })?;
    let (bytes, missing, category) = match sample {
        Ok(value) => (Some(value), None, None),
        Err(error) => (None, Some(error.message), Some(error.category)),
    };
    Ok(RssAcquisition {
        observation: RssObservation {
            identity,
            started_ns,
            finished_ns,
            bytes,
            missing,
        },
        category,
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
    published_sequence: u64,
    first_rss_failure: Option<FirstRssFailure>,
    first_rss_failure_retention: RssRetention,
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
    fn remember_rss(&mut self, candidate: Option<RssCandidate>) {
        if self.first_rss_failure.is_some() {
            return;
        }
        let Some(candidate) = candidate else { return };
        let record = FirstRssFailure::new(
            &self.run,
            RssProducer::Worker,
            self.published_sequence,
            candidate,
        );
        self.first_rss_failure = Some(record);
        // Diagnostic retention is independent of the original fatal error and cleanup allowance.
        self.first_rss_failure_retention = if serde_json::to_vec(&record)
            .is_ok_and(|bytes| bytes.len() <= FIRST_RSS_CAP)
            && resource_write(Path::new(&self.run.root), "rss-first-failure.json", &record).is_ok()
        {
            RssRetention::Written
        } else {
            RssRetention::WriteFailed
        };
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
const RSS_UNAVAILABLE: &str = "owned child RSS snapshot unavailable";
#[cfg(target_os = "macos")]
fn rss(pid: u32) -> RssReadResult<u64> {
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
        return Err(RssReadError::unavailable());
    }
    Ok(unsafe { info.assume_init() }.pti_resident_size)
}
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn rss(pid: u32) -> RssReadResult<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|e| RssReadError::new(RssCategory::ProcStatusReadFailed, e.to_string()))?;
    parse_linux_rss(&text)
}
#[cfg(any(test, all(target_os = "linux", target_env = "gnu")))]
fn parse_linux_rss(text: &str) -> RssReadResult<u64> {
    let row = text
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .ok_or_else(|| RssReadError::new(RssCategory::VmrssFieldMissing, RSS_UNAVAILABLE))?;
    let kb: u64 = row
        .split_whitespace()
        .next()
        .ok_or_else(|| RssReadError::new(RssCategory::VmrssValueMissing, "missing RSS value"))?
        .parse()
        .map_err(|_| RssReadError::new(RssCategory::VmrssValueInvalid, "invalid RSS value"))?;
    kb.checked_mul(1024)
        .ok_or_else(|| RssReadError::new(RssCategory::RssBytesOverflow, "RSS counter overflow"))
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
fn launch_binding(
    config: &Path,
    role: &str,
    progress: &mut impl FnMut() -> Result<()>,
) -> Result<LaunchBinding> {
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
    let cli_hash = binary_digest_file(&cli, progress)?;
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
    first_rss_failure: Option<RssCandidate>,
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
    fn start(
        root: &Path,
        spec: ProcessSpec<'_>,
        deadline: Instant,
        progress: &mut impl FnMut() -> Result<()>,
    ) -> Result<Self> {
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
        let launch = launch_binding(config, role, progress)?;
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
            first_rss_failure: None,
        })
    }
    fn poll_exit(&mut self) -> Result<()> {
        self.poll_exit_with(&mut Child::try_wait)
    }
    fn poll_exit_with(
        &mut self,
        poll: &mut impl FnMut(&mut Child) -> std::io::Result<Option<std::process::ExitStatus>>,
    ) -> Result<()> {
        if self.terminal {
            return Ok(());
        }
        if let Some(status) = poll(&mut self.child).map_err(|e| e.to_string())? {
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
    #[cfg(test)]
    fn sample_rss_with(
        &mut self,
        sample: &mut impl FnMut(u32) -> RssReadResult<u64>,
        poll: &mut impl FnMut(&mut Child) -> std::io::Result<Option<std::process::ExitStatus>>,
    ) -> Result<()> {
        self.sample_rss_at_with(RssSite::CleanupChild, true, sample, poll)
    }
    fn remember_rss(
        &mut self,
        site: RssSite,
        category: RssCategory,
        poll: RssPoll,
        observed_ns: Option<u64>,
        bytes: Option<u64>,
    ) {
        if self.first_rss_failure.is_none() {
            self.first_rss_failure = RssCandidate::from_identity(
                &self.identity(),
                site,
                category,
                poll,
                observed_ns,
                bytes,
            );
        }
    }
    fn sample_rss_at_with(
        &mut self,
        site: RssSite,
        allow_retirement: bool,
        sample: &mut impl FnMut(u32) -> RssReadResult<u64>,
        poll: &mut impl FnMut(&mut Child) -> std::io::Result<Option<std::process::ExitStatus>>,
    ) -> Result<()> {
        if self.terminal {
            return Ok(());
        }
        match sample(self.child.id()) {
            Ok(value) => self.record_rss(value).map_err(|error| {
                self.remember_rss(
                    site,
                    error.category,
                    RssPoll::NotAttempted,
                    None,
                    Some(value),
                );
                error.message
            }),
            Err(error) => {
                let mut retirement = RssPoll::NotAttempted;
                if error.category.permits_retirement() {
                    if let Err(poll_error) = self.poll_exit_with(poll) {
                        self.remember_rss(
                            site,
                            RssCategory::RetirementPollFailed,
                            RssPoll::PollError,
                            error.observed_ns,
                            None,
                        );
                        return Err(poll_error);
                    }
                    if self.terminal {
                        if allow_retirement {
                            return Ok(());
                        }
                        retirement = if self.receipt.success {
                            RssPoll::ReapedSuccess
                        } else {
                            RssPoll::ReapedFailure
                        };
                        self.remember_rss(
                            site,
                            RssCategory::UnexpectedRetirement,
                            retirement,
                            error.observed_ns,
                            None,
                        );
                        return Err(format!(
                            "{} exited before requested stop",
                            self.receipt.node
                        ));
                    }
                    retirement = RssPoll::Running;
                }
                self.remember_rss(
                    site,
                    error.category,
                    retirement,
                    error.observed_ns,
                    error.bytes,
                );
                Err(error.message)
            }
        }
    }
    fn record_rss(&mut self, value: u64) -> RssReadResult<()> {
        let stamp = u64::try_from(self.started.elapsed().as_nanos()).map_err(|_| {
            RssReadError::new(
                RssCategory::SampleTimestampOverflow,
                "sample timestamp overflow",
            )
        })?;
        self.receipt.rss_samples += 1;
        self.receipt.rss_first_ns.get_or_insert(stamp);
        self.receipt.rss_last_ns = Some(stamp);
        self.receipt.max_rss_bytes = self.receipt.max_rss_bytes.max(value);
        if value >= RSS_CAP {
            return Err(RssReadError::new(
                RssCategory::PidRssCapExceeded,
                "owned PID RSS cap exceeded",
            ));
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
    #[cfg(test)]
    fn sample_with(&mut self, sample: &mut impl FnMut(u32) -> RssReadResult<u64>) -> Result<()> {
        self.sample_at_with_poll(RssSite::ActiveChild, sample, &mut Child::try_wait)
    }
    fn sample_at_with_poll(
        &mut self,
        site: RssSite,
        sample: &mut impl FnMut(u32) -> RssReadResult<u64>,
        poll: &mut impl FnMut(&mut Child) -> std::io::Result<Option<std::process::ExitStatus>>,
    ) -> Result<()> {
        self.poll_exit()?;
        if self.terminal {
            return Err(format!(
                "{} exited before requested stop",
                self.receipt.node
            ));
        }
        self.sample_rss_at_with(site, false, sample, poll)?;
        if self.terminal {
            return Err(format!(
                "{} exited before requested stop",
                self.receipt.node
            ));
        }
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
        progress: &mut impl FnMut() -> Result<()>,
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
            || binary_digest_file(Path::new(&binding.cli_binary_path), progress)?
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
                published_sequence: 0,
                first_rss_failure: None,
                first_rss_failure_retention: RssRetention::NotAttempted,
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
            "failure":self.resources.failure,"last":self.resources.last,
            "first_rss_failure":self.resources.first_rss_failure,
            "first_rss_failure_retention":self.resources.first_rss_failure_retention})
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
    fn sample_process(
        &mut self,
        index: usize,
        site: RssSite,
        allow_retirement: bool,
    ) -> Result<()> {
        self.sample_process_with(
            index,
            site,
            allow_retirement,
            &mut rss,
            &mut Child::try_wait,
        )
    }
    fn sample_process_with(
        &mut self,
        index: usize,
        site: RssSite,
        allow_retirement: bool,
        sample: &mut impl FnMut(u32) -> RssReadResult<u64>,
        poll: &mut impl FnMut(&mut Child) -> std::io::Result<Option<std::process::ExitStatus>>,
    ) -> Result<()> {
        let process = &mut self.processes[index];
        let result = if allow_retirement {
            process.sample_rss_at_with(site, true, sample, poll)
        } else {
            process.sample_at_with_poll(site, sample, poll)
        };
        self.resources.remember_rss(process.first_rss_failure);
        result
    }
    pub fn refresh_resources(&mut self, force: bool, terminal: bool) -> Result<()> {
        let result = self.sample_resources(force, terminal);
        if let Err(error) = &result {
            self.resources.remember(error.clone());
        }
        result
    }
    fn acquire_resource(
        &mut self,
        identity: &RssIdentity,
        site: RssSite,
        verify_parent: bool,
        measure: &mut impl FnMut(RssIdentity, bool) -> RssReadResult<RssAcquisition>,
    ) -> Result<RssAcquisition> {
        let result = measure(identity.clone(), verify_parent);
        match result {
            Err(error) => {
                self.resources.remember_rss(RssCandidate::from_identity(
                    identity,
                    site,
                    error.category,
                    RssPoll::NotAttempted,
                    error.observed_ns,
                    error.bytes,
                ));
                Err(error.message)
            }
            Ok(value) => {
                if !matches!(site, RssSite::WorkerFrameChild)
                    && let Some(category) = value.category()
                {
                    self.resources.remember_rss(RssCandidate::from_identity(
                        identity,
                        site,
                        category,
                        RssPoll::NotApplicable,
                        Some(value.observation.finished_ns),
                        value.observation.bytes,
                    ));
                }
                Ok(value)
            }
        }
    }
    fn sample_resources(&mut self, force: bool, terminal: bool) -> Result<()> {
        self.sample_resources_with(force, terminal, &mut measured)
    }
    fn sample_resources_with(
        &mut self,
        force: bool,
        terminal: bool,
        measure: &mut impl FnMut(RssIdentity, bool) -> RssReadResult<RssAcquisition>,
    ) -> Result<()> {
        if let Err(error) = self.resources.stop_requested() {
            self.resources.remember(error);
        }
        let capture_started_ns = monotonic_ns()?;
        // One original poll budget covers every rebuild; retirement cannot extend it.
        let retry_deadline_ns = capture_started_ns
            .checked_add(POLL_MS * 1_000_000)
            .ok_or("RSS capture deadline overflow")?
            .min(self.resources.outer_ns)
            .min(self.resources.stop_deadline_ns.unwrap_or(u64::MAX));
        let mut unreaped = self.processes.iter().filter(|p| !p.terminal).count();
        let mut rebuilding = false;
        let (started_ns, expected, observations, finished_ns) = loop {
            let started_ns = monotonic_ns()?;
            if rebuilding && started_ns >= retry_deadline_ns {
                return Err("RSS capture retirement deadline exhausted".into());
            }
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
            let controller =
                self.acquire_resource(&expected[0], RssSite::WorkerFrameController, true, measure)?;
            let worker =
                self.acquire_resource(&expected[1], RssSite::WorkerFrameWorker, false, measure)?;
            let mut observations = vec![controller.observation, worker.observation];
            let mut retired_during_capture = false;
            for process in self.processes.iter_mut().filter(|p| !p.terminal) {
                let identity = process.identity();
                let acquisition = match measure(identity.clone(), false) {
                    Ok(value) => value,
                    Err(error) => {
                        self.resources.remember_rss(RssCandidate::from_identity(
                            &identity,
                            RssSite::WorkerFrameChild,
                            error.category,
                            RssPoll::NotAttempted,
                            error.observed_ns,
                            error.bytes,
                        ));
                        return Err(error.message);
                    }
                };
                let category = acquisition.category();
                let observation = acquisition.observation;
                let mut retirement = RssPoll::NotAttempted;
                if observation.bytes.is_none()
                    && category.is_some_and(RssCategory::permits_retirement)
                {
                    if let Err(error) = process.poll_exit() {
                        self.resources.remember_rss(RssCandidate::from_identity(
                            &identity,
                            RssSite::WorkerFrameChild,
                            RssCategory::RetirementPollFailed,
                            RssPoll::PollError,
                            Some(observation.finished_ns),
                            None,
                        ));
                        return Err(error);
                    }
                    if process.terminal {
                        // Only the retiring child's unavailable sample may be discarded.
                        // Keep earlier supervisor failures latched before this candidate is discarded.
                        if let Err(error) =
                            rss_totals(&expected[..observations.len()], &observations)
                        {
                            self.resources.remember(error);
                        }
                        retired_during_capture = true;
                        break;
                    }
                    retirement = RssPoll::Running;
                }
                if let Some(category) = category {
                    self.resources.remember_rss(RssCandidate::from_identity(
                        &identity,
                        RssSite::WorkerFrameChild,
                        category,
                        retirement,
                        Some(observation.finished_ns),
                        observation.bytes,
                    ));
                }
                if let Some(bytes) = observation.bytes
                    && let Err(error) = process.record_rss(bytes)
                {
                    self.resources.remember_rss(RssCandidate::from_identity(
                        &identity,
                        RssSite::WorkerFrameChild,
                        error.category,
                        retirement,
                        Some(observation.finished_ns),
                        Some(bytes),
                    ));
                    self.resources.remember(error.message);
                }
                observations.push(observation);
            }
            let finished_ns = monotonic_ns()?;
            if rebuilding && finished_ns >= retry_deadline_ns {
                return Err("RSS capture retirement deadline exhausted".into());
            }
            if !retired_during_capture {
                break (started_ns, expected, observations, finished_ns);
            }
            let remaining = self.processes.iter().filter(|p| !p.terminal).count();
            if remaining >= unreaped {
                return Err("RSS capture rebuild without confirmed retirement".into());
            }
            unreaped = remaining;
            if finished_ns >= retry_deadline_ns {
                return Err("RSS capture retirement deadline exhausted".into());
            }
            rebuilding = true;
            // Discard this unpublished candidate, including supervisor readings. A fresh
            // envelope and exact roster replace it; missing RSS never becomes a zero sample.
        };
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
            self.resources.published_sequence = self.resources.sequence;
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
        self.check_at(RssSite::ActiveChild)
    }
    fn check_at(&mut self, site: RssSite) -> Result<()> {
        self.refresh_resources(false, false)?;
        all_log_bytes(&self.root)?;
        if free_disk(&self.root)? < FREE_DISK_FLOOR {
            return Err("free disk below 64 GiB floor".into());
        }
        for index in 0..self.processes.len() {
            self.sample_process(index, site, false)?;
        }
        Ok(())
    }
    fn attestation_progress(&mut self, deadline: Instant) -> Result<()> {
        if Instant::now() >= deadline {
            return Err("binary attestation deadline exhausted".into());
        }
        self.resources.stop_requested()?;
        if let Some(error) = &self.resources.failure {
            return Err(error.clone());
        }
        if monotonic_ns()?.saturating_sub(self.resources.published_ns) >= POLL_MS * 1_000_000 {
            // Hashing must not starve exact owned-process sampling/publication.
            self.refresh_resources(false, false)?;
        }
        if Instant::now() >= deadline {
            return Err("late binary attestation progress".into());
        }
        Ok(())
    }
    fn finish_process(
        &mut self,
        index: usize,
        deadline: Instant,
    ) -> Result<Option<std::collections::BTreeMap<String, Bank>>> {
        if !self.processes[index].terminal {
            return Err("cannot retire an unreaped process".into());
        }
        // The process is physically reaped, so remove its numeric identity before sampling.
        let mut process = self.processes.remove(index);
        let evidence = process.finish(deadline, &mut || self.attestation_progress(deadline));
        if let Ok(Some(rows)) = &evidence {
            self.banks.push(json!({"node":process.receipt.node,"generation":process.receipt.generation,
                "pid":process.receipt.pid,"scope":"cumulative generation shutdown logical counters",
                "launch":process.receipt.launch,"maintenance_quiescence":"unavailable","rows":rows}));
        }
        // Retain ownership/proof failures too; no callback error can discard a receipt.
        self.retired.push(process.receipt.clone());
        let publication = self.refresh_resources(true, self.processes.is_empty());
        evidence.and_then(|value| publication.map(|()| value))
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
        let root = self.root.clone();
        let process = OwnedProcess::start(
            &root,
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
            &mut || self.attestation_progress(deadline),
        )?;
        self.processes.push(process);
        self.refresh_resources(true, false)?;
        let index = self.processes.len() - 1;
        loop {
            if Instant::now() >= deadline {
                return Err("CLI readiness deadline exhausted".into());
            }
            self.check_at(RssSite::ReadinessChild)?;
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
        let root = self.root.clone();
        let process = OwnedProcess::start(
            &root,
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
            &mut || self.attestation_progress(deadline),
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
                self.finish_process(index, deadline)?;
                return Ok(());
            }
            self.sample_process(index, RssSite::ApplyTarget, true)?;
            if self.processes[index].terminal {
                return self.finish_process(index, deadline).map(|_| ());
            }
            for other in 0..self.processes.len() {
                if other != index {
                    self.sample_process(other, RssSite::ApplySibling, false)?;
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
                let banks = self
                    .finish_process(index, deadline)?
                    .ok_or("server bank missing")?;
                return Ok(banks);
            }
            for other in 0..self.processes.len() {
                self.sample_process(
                    other,
                    if other == index {
                        RssSite::StopTarget
                    } else {
                        RssSite::StopSibling
                    },
                    other == index,
                )?;
            }
            if self.processes[index].terminal {
                let banks = self
                    .finish_process(index, deadline)?
                    .ok_or("server bank missing")?;
                return Ok(banks);
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
                if let Err(error) = self.processes[index].poll_exit() {
                    failure.get_or_insert(error);
                }
                if self.processes[index].terminal {
                    if let Err(error) = self.finish_process(index, deadline) {
                        failure.get_or_insert(error);
                    }
                } else {
                    if let Err(error) = self.sample_process(index, RssSite::CleanupChild, true) {
                        failure.get_or_insert(error);
                    }
                    if self.processes[index].terminal {
                        if let Err(error) = self.finish_process(index, deadline) {
                            failure.get_or_insert(error);
                        }
                        continue;
                    }
                    let process = &mut self.processes[index];
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
fn outer_acquire(
    run: &ResourceRun,
    sequence: u64,
    identity: RssIdentity,
    site: RssSite,
    first: &mut Option<FirstRssFailure>,
    measure: &mut impl FnMut(RssIdentity, bool) -> RssReadResult<RssAcquisition>,
) -> Result<RssObservation> {
    let capture = measure(identity.clone(), false);
    let (category, observed_ns, bytes) = match &capture {
        Ok(value) => (
            value.category(),
            Some(value.observation.finished_ns),
            value.observation.bytes,
        ),
        Err(error) => (Some(error.category), error.observed_ns, error.bytes),
    };
    if first.is_none()
        && let Some(category) = category
        && let Some(candidate) = RssCandidate::from_identity(
            &identity,
            site,
            category,
            RssPoll::NotApplicable,
            observed_ns,
            bytes,
        )
    {
        *first = Some(FirstRssFailure::new(
            run,
            RssProducer::Controller,
            sequence,
            candidate,
        ));
    }
    capture
        .map(|value| value.observation)
        .map_err(|error| error.message)
}
fn outer_sample(
    root: &Path,
    run: &ResourceRun,
    sequence: u64,
    first: &mut Option<FirstRssFailure>,
) -> Result<OuterResourceSample> {
    let frame: ResourceFrame = resource_read(&root.join("resource.json"))?;
    frame.validate(run, sequence, monotonic_ns()?)?;
    // Replace producer supervisor samples; only child observations/subtotal cross the IPC seam.
    let supervisors = [
        outer_acquire(
            run,
            frame.sequence,
            supervisor_identity("controller", run.controller_pid),
            RssSite::OuterController,
            first,
            &mut measured,
        )?,
        outer_acquire(
            run,
            frame.sequence,
            supervisor_identity("worker", run.worker_pid),
            RssSite::OuterWorker,
            first,
            &mut measured,
        )?,
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
    let mut progress = || {
        if Instant::now() >= deadline {
            Err("expired supervisor attestation budget".into())
        } else {
            Ok(())
        }
    };
    let binary_hash =
        binary_digest_file(&executable, &mut progress).expect("test binary attestation");
    let cli_hash = binary_digest_file(Path::new(env!("CARGO_BIN_EXE_mount-rs")), &mut progress)
        .expect("CLI binary attestation");
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
        .env("MOUNT_RS_TEN_PROCESS_CLI_SHA256", &cli_hash)
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
    let mut initial = json!({"schema":1,"complete":false,"worker_pid":group,"process_group":group,
        "supervisor_pid":std::process::id(),"worker_reaped":false,"forced":false,
        "failure":"controller did not reach a terminal receipt; retained ownership only",
        "first_rss_failure":null,"first_rss_failure_retention":"not_attempted",
        "private_credential_cleanup":"pending; private key/token files must not be exported",
        "test_binary_sha256":binary_hash,"cli_binary_sha256":cli_hash});
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
    let mut first_rss_failure = None;
    let mut first_rss_failure_retention = RssRetention::NotAttempted;
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
            let had_rss_failure = first_rss_failure.is_some();
            let sampled = outer_sample(&root, &run, last_sequence, &mut first_rss_failure);
            if !had_rss_failure && first_rss_failure.is_some() {
                initial["first_rss_failure"] = serde_json::to_value(first_rss_failure).unwrap();
                initial["first_rss_failure_retention"] =
                    serde_json::to_value(RssRetention::Written).unwrap();
                first_rss_failure_retention = if fs::write(
                    root.join("controller.json"),
                    serde_json::to_vec_pretty(&initial).unwrap(),
                )
                .is_ok()
                {
                    RssRetention::Written
                } else {
                    RssRetention::WriteFailed
                };
            }
            match sampled {
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
        "test_binary_sha256":binary_hash,"cli_binary_sha256":cli_hash,
        "debug_assertions":cfg!(debug_assertions),"local_oidc_fixture":cfg!(feature="local-oidc-fixture"),
        "forced":forced,"worker_reaped":true,"worker_success":status.success(),
        "group_gone":group_gone,"unrelated_sentinel_retained":sentinel_owned,
        "worker_max_rss_bytes":worker_max_rss,"controller_max_rss_bytes":controller_max_rss,
        "owned_aggregate_max_observed_bytes":max_total,"resource_sample_count":sample_count,
        "resource_initial_observed":initial_observed,"resource_last_sequence":last_sequence,
        "resource_last":last_resource,"resource_clock":"CLOCK_MONOTONIC",
        "first_rss_failure":first_rss_failure,"first_rss_failure_retention":first_rss_failure_retention,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        io::Cursor,
        rc::Rc,
    };

    fn inert_process(root: &Path) -> (OwnedProcess, Option<std::process::ChildStdin>) {
        inert_process_status(root, 0)
    }

    fn inert_process_status(
        root: &Path,
        status: u8,
    ) -> (OwnedProcess, Option<std::process::ChildStdin>) {
        let mut child = Command::new("/bin/sh")
            .args(["-c", &format!("printf ready; read release; exit {status}")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut ready = [0u8; 5];
        child.stdout.take().unwrap().read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready");
        let input = child.stdin.take();
        let stdout = root.join("inert.stdout");
        let stderr = root.join("inert.stderr");
        fs::write(&stdout, []).unwrap();
        fs::write(&stderr, []).unwrap();
        let receipt = ProcessReceipt {
            launch: LaunchBinding {
                config_path: String::new(),
                config_sha256: String::new(),
                cli_binary_path: "/bin/sh".into(),
                cli_binary_sha256: String::new(),
                catalog_path: String::new(),
                catalog_at_launch: None,
                catalog_after_completion: None,
                apply_expected_revision: None,
                apply_document_sha256: None,
            },
            node: "inert-owned-child".into(),
            generation: 1,
            pid: child.id(),
            role: "server".into(),
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
        (
            OwnedProcess {
                child,
                receipt,
                stdout,
                stderr,
                quic: None,
                peer: None,
                cache: None,
                terminal: false,
                started: Instant::now(),
                first_rss_failure: None,
            },
            input,
        )
    }

    fn release_inert_child(input: &mut Option<std::process::ChildStdin>, pid: u32) -> Result<()> {
        drop(input.take());
        let deadline = Instant::now() + Duration::from_millis(POLL_MS);
        loop {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            // Observe only this fixture's owned PID without reaping it: production try_wait
            // must be the operation that establishes and records actual retirement.
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if unsafe { info.assume_init().si_pid() } == pid as libc::pid_t {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("inert child exit barrier expired".into());
            }
            thread::yield_now();
        }
    }

    fn cleanup_inert_child(process: &mut OwnedProcess) {
        if !process.terminal {
            process.child.kill().unwrap();
            let status = process.child.wait().unwrap();
            process.terminal = true;
            process.receipt.reaped = true;
            process.receipt.success = status.success();
        }
    }

    #[test]
    fn rss_retirement_rechecks_the_same_child_without_a_fake_sample() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        process.record_rss(16384).unwrap();
        process.poll_exit().unwrap();
        assert!(!process.terminal);
        let owned_pid = process.child.id();
        let mut reads = 0;
        let mut polls = 0;
        let result = process.sample_rss_with(
            &mut |pid| {
                assert_eq!(pid, owned_pid);
                reads += 1;
                release_inert_child(&mut input, pid)?;
                Err(RssReadError::unavailable())
            },
            &mut |child| {
                assert_eq!(child.id(), owned_pid);
                polls += 1;
                child.try_wait()
            },
        );
        let state = (
            process.terminal,
            process.receipt.reaped,
            process.receipt.success,
            process.receipt.forced,
            process.receipt.rss_samples,
            process.receipt.max_rss_bytes,
        );
        cleanup_inert_child(&mut process);
        assert_eq!(result, Ok(()));
        assert_eq!(state, (true, true, true, false, 1, 16384));
        assert_eq!((reads, polls), (1, 1));
        process
            .sample_rss_with(&mut |_| panic!("RSS queried after reap"), &mut |_| {
                panic!("Child polled after known reap")
            })
            .unwrap();
    }

    #[test]
    fn rss_retirement_still_rejects_unexpected_exit_during_active_sampling() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let result = process.sample_with(&mut |pid| {
            release_inert_child(&mut input, pid)?;
            Err(RssReadError::unavailable())
        });
        let terminal = process.terminal;
        cleanup_inert_child(&mut process);
        assert_eq!(
            result,
            Err("inert-owned-child exited before requested stop".into())
        );
        assert!(terminal);
    }

    #[test]
    fn rss_retirement_rebuilds_a_fresh_exact_resource_frame() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        process.record_rss(16384).unwrap();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        let mut supervisor_reads = 0;
        let mut retiring_reads = 0;
        let mut discarded_finished = 0;
        let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
            let started_ns = monotonic_ns().unwrap();
            let (bytes, missing) = if identity.role == "server" {
                retiring_reads += 1;
                release_inert_child(&mut input, identity.pid)?;
                (None, Some(RSS_UNAVAILABLE.into()))
            } else {
                supervisor_reads += 1;
                (Some(4096), None)
            };
            let finished_ns = monotonic_ns().unwrap();
            if bytes.is_none() {
                discarded_finished = finished_ns;
            }
            Ok(RssAcquisition {
                observation: RssObservation {
                    identity,
                    started_ns,
                    finished_ns,
                    bytes,
                    missing,
                },
                category: bytes
                    .is_none()
                    .then(|| RssReadError::unavailable().category),
            })
        });
        let terminal = fleet.processes[0].terminal;
        let samples = fleet.processes[0].receipt.rss_samples;
        cleanup_inert_child(&mut fleet.processes[0]);
        assert_eq!(result, Ok(()));
        assert!(terminal);
        assert_eq!(samples, 1);
        assert_eq!((supervisor_reads, retiring_reads), (4, 1));
        let frame = fleet.resources.last.as_ref().unwrap();
        assert!(frame.started_ns > discarded_finished);
        assert_eq!(frame.sequence, 5);
        assert_eq!(frame.expected.len(), 2);
        assert_eq!(
            (frame.total_bytes, frame.child_bytes),
            (Some(8192), Some(0))
        );
        assert_eq!(frame.max_total_bytes, 524288);
        assert!(frame.error.is_none());
        frame
            .validate(&fleet.resources.run, 5, monotonic_ns().unwrap())
            .unwrap();
    }

    fn inert_resource_fleet(root: &Path, process: OwnedProcess) -> Fleet {
        let now = monotonic_ns().unwrap();
        let mut previous = resource_frame(now);
        previous.run.root = root.to_string_lossy().into_owned();
        previous.sequence = 4;
        previous.max_total_bytes = 524288;
        Fleet {
            root: root.into(),
            processes: vec![process],
            retired: Vec::new(),
            banks: Vec::new(),
            next_generation: 2,
            resources: ResourceMonitor {
                run: previous.run.clone(),
                outer_ns: now + RESOURCE_FRESH_NS,
                sequence: previous.sequence,
                max_total: previous.max_total_bytes,
                published_ns: now,
                published_sequence: previous.sequence,
                first_rss_failure: None,
                first_rss_failure_retention: RssRetention::NotAttempted,
                last: Some(previous),
                failure: None,
                stop_deadline_ns: None,
            },
        }
    }

    fn fixture_observation(
        identity: RssIdentity,
        unavailable: bool,
    ) -> RssReadResult<RssAcquisition> {
        let now = monotonic_ns()?;
        Ok(RssAcquisition {
            observation: RssObservation {
                identity,
                started_ns: now,
                finished_ns: now,
                bytes: (!unavailable).then_some(4096),
                missing: unavailable.then(|| RSS_UNAVAILABLE.into()),
            },
            category: unavailable.then(|| RssReadError::unavailable().category),
        })
    }

    #[test]
    fn rss_first_failure_latches_owned_live_candidate_before_cleanup_or_good_frame() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        process.receipt.node = "node-7".into();
        let owned_pid = process.child.id();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
            let unavailable = identity.role == "server";
            fixture_observation(identity, unavailable)
        });
        let first = fleet.resource_evidence()["first_rss_failure"].clone();
        cleanup_inert_child(&mut fleet.processes[0]);
        assert!(result.is_err());
        assert_eq!(first["schema"], "mount-rs.cache-rss-failure.v1");
        assert_eq!(first["candidate_pid"], owned_pid);
        assert_eq!(first["candidate_role"], "server");
        assert_eq!(first["candidate_node"], 7);
        assert_eq!(first["candidate_generation"], 1);
        assert_eq!(first["site"], "worker_frame_child");
        assert_eq!(first["retirement_poll"], "running");
        let _ = fleet.sample_resources_with(true, true, &mut |identity, _| {
            fixture_observation(identity, false)
        });
        assert_eq!(fleet.resource_evidence()["first_rss_failure"], first);
        assert!(directory.path().join("rss-first-failure.json").is_file());
    }
    #[test]
    fn rss_first_failure_typed_linux_parsing_and_private_source_errors_do_not_select_from_strings()
    {
        for (text, category) in [
            ("Name: private", RssCategory::VmrssFieldMissing),
            ("VmRSS:", RssCategory::VmrssValueMissing),
            ("VmRSS: SECRET_PATH", RssCategory::VmrssValueInvalid),
            (
                "VmRSS: 18446744073709551615 kB",
                RssCategory::RssBytesOverflow,
            ),
        ] {
            let error = parse_linux_rss(text).unwrap_err();
            assert_eq!(error.category, category);
        }
        assert_eq!(parse_linux_rss("VmRSS: 8 kB").unwrap(), 8192);
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        process.receipt.node = "node-2".into();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        let mut polls = 0;
        let result = fleet.sample_process_with(
            0,
            RssSite::ApplyTarget,
            true,
            &mut |_| {
                Err(RssReadError::new(
                    RssCategory::ProcStatusReadFailed,
                    "PRIVATE_RSS_ERROR /secret/path VmRSS snapshot unavailable",
                ))
            },
            &mut |_| {
                polls += 1;
                panic!("status-read error must not be retirement-excused")
            },
        );
        cleanup_inert_child(&mut fleet.processes[0]);
        assert_eq!(polls, 0);
        assert!(result.unwrap_err().starts_with("PRIVATE_RSS_ERROR"));
        let record = fleet.resource_evidence()["first_rss_failure"].clone();
        assert_eq!(record["category"], "proc_status_read_failed");
        assert_eq!(record["retirement_poll"], "not_attempted");
        assert!(record["observed_ns"].is_null());
        let bytes = fs::read(directory.path().join("rss-first-failure.json")).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("PRIVATE_RSS_ERROR") && !text.contains("/secret/path"));
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        process.receipt.node = "node-6".into();
        let result = process.sample_rss_at_with(
            RssSite::CleanupChild,
            true,
            &mut |_| Err(RSS_UNAVAILABLE.into()),
            &mut |_| panic!("error text must not select retirement"),
        );
        cleanup_inert_child(&mut process);
        assert_eq!(result, Err(RSS_UNAVAILABLE.into()));
        assert_eq!(
            process.first_rss_failure.unwrap().category,
            RssCategory::OtherRssError
        );
    }
    #[test]
    fn rss_first_failure_owned_direct_sites_and_poll_failure_are_exact() {
        for site in [
            RssSite::ReadinessChild,
            RssSite::ActiveChild,
            RssSite::ApplyTarget,
            RssSite::ApplySibling,
            RssSite::StopTarget,
            RssSite::StopSibling,
            RssSite::CleanupChild,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (mut process, _input) = inert_process(directory.path());
            process.receipt.node = "node-3".into();
            process.receipt.generation = 42;
            let pid = process.child.id();
            let mut fleet = inert_resource_fleet(directory.path(), process);
            let result = fleet.sample_process_with(
                0,
                site,
                true,
                &mut |actual| {
                    assert_eq!(actual, pid);
                    Err(RssReadError::unavailable())
                },
                &mut |child| {
                    assert_eq!(child.id(), pid);
                    Err(std::io::Error::other("PRIVATE_POLL_FAILURE"))
                },
            );
            cleanup_inert_child(&mut fleet.processes[0]);
            assert_eq!(result, Err("PRIVATE_POLL_FAILURE".into()));
            let record = fleet.resources.first_rss_failure.unwrap();
            assert_eq!(
                (
                    record.candidate_pid,
                    record.candidate_node,
                    record.candidate_generation
                ),
                (pid, Some(3), 42)
            );
            assert_eq!(
                (record.site, record.category, record.retirement_poll),
                (site, RssCategory::RetirementPollFailed, RssPoll::PollError)
            );
            let text = String::from_utf8(
                fs::read(directory.path().join("rss-first-failure.json")).unwrap(),
            )
            .unwrap();
            assert!(!text.contains("PRIVATE_POLL_FAILURE"));
        }
    }
    #[test]
    fn rss_first_failure_expected_reap_is_empty_and_active_reap_records_real_status() {
        for expected in [true, false] {
            for exit in [0, 7] {
                let directory = tempfile::tempdir().unwrap();
                let (mut process, mut input) = inert_process_status(directory.path(), exit);
                process.receipt.node = "node-8".into();
                let pid = process.child.id();
                let mut fleet = inert_resource_fleet(directory.path(), process);
                let site = if expected {
                    RssSite::CleanupChild
                } else {
                    RssSite::ActiveChild
                };
                let result = fleet.sample_process_with(
                    0,
                    site,
                    expected,
                    &mut |pid| {
                        release_inert_child(&mut input, pid)?;
                        Err(RssReadError::unavailable())
                    },
                    &mut Child::try_wait,
                );
                assert!(fleet.processes[0].terminal && fleet.processes[0].receipt.reaped);
                assert_eq!(fleet.processes[0].receipt.success, exit == 0);
                if expected {
                    assert!(result.is_ok());
                    assert!(fleet.resources.first_rss_failure.is_none());
                    assert!(!directory.path().join("rss-first-failure.json").exists());
                    assert!(fleet.resources.stop_deadline_ns.is_none());
                } else {
                    assert!(result.unwrap_err().contains("exited before requested stop"));
                    let first = fleet.resources.first_rss_failure.unwrap();
                    assert_eq!(first.candidate_pid, pid);
                    assert_eq!(first.category, RssCategory::UnexpectedRetirement);
                    assert_eq!(
                        first.retirement_poll,
                        if exit == 0 {
                            RssPoll::ReapedSuccess
                        } else {
                            RssPoll::ReapedFailure
                        }
                    );
                }
            }
        }
    }
    #[test]
    fn rss_first_failure_direct_error_preserves_existing_monitor_policy() {
        for already_failed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (mut process, _input) = inert_process(directory.path());
            process.receipt.node = "node-6".into();
            let mut fleet = inert_resource_fleet(directory.path(), process);
            if already_failed {
                fleet.resources.failure = Some("earlier resource failure".into());
                fleet.resources.stop_deadline_ns = Some(fleet.resources.outer_ns);
            }
            let failure_before = fleet.resources.failure.clone();
            let deadline_before = fleet.resources.stop_deadline_ns;
            let outer_before = fleet.resources.outer_ns;
            let mut reads = 0;
            let mut polls = 0;
            let result = fleet.sample_process_with(
                0,
                RssSite::ActiveChild,
                false,
                &mut |_| {
                    reads += 1;
                    Err(RssReadError::unavailable())
                },
                &mut |child| {
                    polls += 1;
                    child.try_wait()
                },
            );
            cleanup_inert_child(&mut fleet.processes[0]);
            assert_eq!(result, Err(RSS_UNAVAILABLE.into()));
            assert_eq!((reads, polls), (1, 1));
            assert!(fleet.resources.first_rss_failure.is_some());
            assert_eq!(fleet.resources.stop_deadline_ns, deadline_before);
            assert_eq!(fleet.resources.failure, failure_before);
            assert_eq!(fleet.resources.outer_ns, outer_before);
        }
    }
    #[test]
    fn rss_first_failure_write_error_is_sticky_and_cannot_replace_primary_error() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        process.receipt.node = "node-0".into();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        fleet.resources.sequence = 99; // A candidate is not necessarily published.
        fs::create_dir(directory.path().join(".rss-first-failure.json.tmp")).unwrap();
        let result = fleet.sample_process_with(
            0,
            RssSite::StopTarget,
            true,
            &mut |_| Err(RssReadError::unavailable()),
            &mut Child::try_wait,
        );
        let first = fleet.resource_evidence()["first_rss_failure"].clone();
        assert_eq!(result, Err(RSS_UNAVAILABLE.into()));
        assert_eq!(first["resource_sequence"], 4);
        assert_eq!(
            fleet.resource_evidence()["first_rss_failure_retention"],
            "write_failed"
        );
        fs::remove_dir(directory.path().join(".rss-first-failure.json.tmp")).unwrap();
        let later = fleet.sample_process_with(
            0,
            RssSite::StopTarget,
            true,
            &mut |_| Ok(8192),
            &mut Child::try_wait,
        );
        assert!(later.is_ok());
        assert_eq!(fleet.resource_evidence()["first_rss_failure"], first);
        assert_eq!(
            fleet.resource_evidence()["first_rss_failure_retention"],
            "write_failed"
        );
        assert!(!directory.path().join("rss-first-failure.json").exists());
        cleanup_inert_child(&mut fleet.processes[0]);
    }
    #[test]
    fn rss_first_failure_outer_source_is_typed_sticky_and_has_no_shared_file() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        let pid = process.child.id();
        let run = ResourceRun {
            root: "PRIVATE_ROOT_SENTINEL".into(),
            controller_pid: std::process::id(),
            worker_pid: pid,
            group: pid,
        };
        let mut first = None;
        let result = outer_acquire(
            &run,
            9,
            supervisor_identity("worker", pid),
            RssSite::OuterWorker,
            &mut first,
            &mut |_, _| {
                Err(RssReadError::new(
                    RssCategory::SampleClockBeforeFailed,
                    "PRIVATE_CLOCK_ERROR",
                ))
            },
        );
        assert_eq!(result.unwrap_err(), "PRIVATE_CLOCK_ERROR");
        let record = serde_json::to_value(first).unwrap();
        assert_eq!(record["producer"], "controller");
        assert_eq!(record["candidate_pid"], pid);
        assert_eq!(record["site"], "outer_worker");
        assert_eq!(record["retirement_poll"], "not_applicable");
        assert!(record["observed_ns"].is_null());
        let _ = outer_acquire(
            &run,
            10,
            supervisor_identity("controller", run.controller_pid),
            RssSite::OuterController,
            &mut first,
            &mut |_, _| Err(RssReadError::unavailable()),
        );
        assert_eq!(serde_json::to_value(first).unwrap(), record);
        let text = serde_json::to_string(&record).unwrap();
        assert!(!text.contains("PRIVATE_ROOT_SENTINEL") && !text.contains("PRIVATE_CLOCK_ERROR"));
        assert!(!directory.path().join("rss-first-failure.json").exists());
        cleanup_inert_child(&mut process);
    }
    #[test]
    fn rss_first_failure_cap_is_source_typed_and_success_has_no_diagnostic_write() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        process.receipt.role = "catalog-apply".into();
        process.receipt.node = "catalog".into();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        assert!(
            fleet
                .sample_process_with(
                    0,
                    RssSite::ApplyTarget,
                    true,
                    &mut |_| Ok(4096),
                    &mut Child::try_wait
                )
                .is_ok()
        );
        assert!(fleet.resources.first_rss_failure.is_none());
        assert!(!directory.path().join("rss-first-failure.json").exists());
        let result = fleet.sample_process_with(
            0,
            RssSite::ApplyTarget,
            true,
            &mut |_| Ok(RSS_CAP),
            &mut Child::try_wait,
        );
        cleanup_inert_child(&mut fleet.processes[0]);
        assert_eq!(result, Err("owned PID RSS cap exceeded".into()));
        let record = fleet.resource_evidence()["first_rss_failure"].clone();
        assert_eq!(record["category"], "pid_rss_cap_exceeded");
        assert_eq!(record["candidate_role"], "catalog-apply");
        assert_eq!(record["candidate_bytes"], RSS_CAP);
        assert!(record["candidate_node"].is_null());
    }
    #[test]
    fn rss_first_failure_pre_sample_exit_aggregate_and_malformed_frame_have_no_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        process.receipt.node = "node-4".into();
        release_inert_child(&mut input, process.child.id()).unwrap();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        let result = fleet.sample_process_with(
            0,
            RssSite::ActiveChild,
            false,
            &mut |_| panic!("pre-sample exit must not query RSS"),
            &mut |_| panic!("no unavailable-sample recovery poll after pre-sample exit"),
        );
        assert!(result.unwrap_err().contains("exited before requested stop"));
        assert!(fleet.resources.first_rss_failure.is_none());
        assert!(fleet.resources.failure.is_none());
        assert!(fleet.resources.stop_deadline_ns.is_none());
        assert!(!directory.path().join("rss-first-failure.json").exists());
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        process.receipt.node = "node-5".into();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
            let mut value = fixture_observation(identity, false)?;
            value.observation.bytes = Some(RSS_CAP / 2);
            Ok(value)
        });
        cleanup_inert_child(&mut fleet.processes[0]);
        assert!(result.unwrap_err().contains("aggregate cap exceeded"));
        assert!(fleet.resources.first_rss_failure.is_none());
        assert!(!directory.path().join("rss-first-failure.json").exists());
        fs::write(
            directory.path().join("resource.json"),
            b"PRIVATE_MALFORMED_FRAME",
        )
        .unwrap();
        let mut first = None;
        assert!(outer_sample(directory.path(), &fleet.resources.run, 0, &mut first).is_err());
        assert!(first.is_none());
    }
    #[test]
    fn rss_first_failure_max_wire_scalars_fit_two_kibibytes() {
        let run = ResourceRun {
            root: "PRIVATE_MAX_ROOT".into(),
            controller_pid: u32::MAX - 2,
            worker_pid: u32::MAX - 1,
            group: u32::MAX - 1,
        };
        let candidate = RssCandidate {
            role: RssRole::Server,
            pid: u32::MAX,
            generation: u64::MAX,
            node: Some(9),
            site: RssSite::WorkerFrameChild,
            category: RssCategory::ParentChangedBeforeSample,
            poll: RssPoll::NotAttempted,
            observed_ns: Some(u64::MAX),
            bytes: Some(u64::MAX),
        };
        let record = FirstRssFailure::new(&run, RssProducer::Worker, u64::MAX, candidate);
        let bytes = serde_json::to_vec(&record).unwrap();
        assert!(bytes.len() <= FIRST_RSS_CAP);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 15);
        assert_eq!(value["candidate_bytes"].as_u64(), Some(u64::MAX));
        assert!(
            !String::from_utf8(bytes.clone())
                .unwrap()
                .contains("PRIVATE_MAX_ROOT")
        );
        if let Some(path) = std::env::var_os("MOUNT_RS_RSS_FAILURE_TEST_FIXTURE") {
            fs::write(path, bytes).unwrap();
        }
    }
    #[test]
    fn rss_retirement_unavailable_live_child_remains_a_failure() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        let mut polls = 0;
        let result =
            process.sample_rss_with(&mut |_| Err(RssReadError::unavailable()), &mut |child| {
                polls += 1;
                child.try_wait()
            });
        let state = (
            process.terminal,
            process.receipt.reaped,
            process.receipt.rss_samples,
        );
        cleanup_inert_child(&mut process);
        assert_eq!(result, Err(RSS_UNAVAILABLE.into()));
        assert_eq!(polls, 1);
        assert_eq!(state, (false, false, 0));
    }

    #[test]
    fn rss_retirement_exit_poll_error_does_not_invent_terminal_state() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        let process_pid = process.child.id();
        let mut polls = 0;
        let result =
            process.sample_rss_with(&mut |_| Err(RssReadError::unavailable()), &mut |child| {
                assert_eq!(child.id(), process_pid);
                polls += 1;
                Err(std::io::Error::other("injected owned exit poll failure"))
            });
        let state = (
            process.terminal,
            process.receipt.reaped,
            process.receipt.rss_samples,
        );
        cleanup_inert_child(&mut process);
        assert_eq!(result, Err("injected owned exit poll failure".into()));
        assert_eq!(polls, 1);
        assert_eq!(state, (false, false, 0));
    }

    #[test]
    fn rss_retirement_does_not_excuse_malformed_samples() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        let mut polls = 0;
        let result =
            process.sample_rss_with(&mut |_| Err("invalid RSS value".into()), &mut |child| {
                polls += 1;
                child.try_wait()
            });
        cleanup_inert_child(&mut process);
        assert_eq!(result, Err("invalid RSS value".into()));
        assert_eq!(polls, 0);
        assert_eq!(process.receipt.rss_samples, 0);
    }

    #[test]
    fn rss_retirement_records_an_unsuccessful_real_exit() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process_status(directory.path(), 7);
        let result = process.sample_rss_with(
            &mut |pid| {
                release_inert_child(&mut input, pid)?;
                Err(RssReadError::unavailable())
            },
            &mut Child::try_wait,
        );
        let state = (
            process.terminal,
            process.receipt.reaped,
            process.receipt.success,
            process.receipt.forced,
        );
        cleanup_inert_child(&mut process);
        assert_eq!(result, Ok(()));
        assert_eq!(state, (true, true, false, false));
        assert_eq!(process.receipt.rss_samples, 0);
    }

    #[test]
    fn rss_retirement_live_missing_resource_frame_stays_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        let (process, _input) = inert_process(directory.path());
        let mut fleet = inert_resource_fleet(directory.path(), process);
        let mut supervisor_reads = 0;
        let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
            let unavailable = identity.role == "server";
            supervisor_reads += usize::from(!unavailable);
            fixture_observation(identity, unavailable)
        });
        let terminal = fleet.processes[0].terminal;
        cleanup_inert_child(&mut fleet.processes[0]);
        assert_eq!(result, Err("RSS snapshot unavailable".into()));
        assert!(!terminal);
        assert_eq!(supervisor_reads, 2);
        let frame = fleet.resources.last.as_ref().unwrap();
        assert_eq!(frame.expected.len(), 3);
        assert_eq!((frame.total_bytes, frame.child_bytes), (None, None));
        assert_eq!(frame.error.as_deref(), Some("RSS snapshot unavailable"));
        assert!(
            frame
                .validate(&fleet.resources.run, 5, monotonic_ns().unwrap())
                .is_err()
        );
    }

    #[test]
    fn rss_retirement_rebuild_preserves_prior_sticky_observer_failure() {
        let directory = tempfile::tempdir().unwrap();
        let (process, mut input) = inert_process(directory.path());
        let mut fleet = inert_resource_fleet(directory.path(), process);
        fleet.resources.remember("prior observer failure".into());
        let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
            let unavailable = identity.role == "server";
            if unavailable {
                release_inert_child(&mut input, identity.pid)?;
            }
            fixture_observation(identity, unavailable)
        });
        cleanup_inert_child(&mut fleet.processes[0]);
        assert_eq!(result, Err("prior observer failure".into()));
        let frame = fleet.resources.last.as_ref().unwrap();
        assert_eq!(frame.expected.len(), 2);
        assert_eq!(frame.error.as_deref(), Some("prior observer failure"));
        assert!(
            frame
                .validate(&fleet.resources.run, 5, monotonic_ns().unwrap())
                .is_err()
        );
    }

    #[test]
    fn rss_retirement_rebuild_cannot_extend_the_original_outer_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let (process, mut input) = inert_process(directory.path());
        let mut fleet = inert_resource_fleet(directory.path(), process);
        fleet.resources.outer_ns = monotonic_ns().unwrap();
        let mut supervisor_reads = 0;
        let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
            let unavailable = identity.role == "server";
            if unavailable {
                release_inert_child(&mut input, identity.pid)?;
            } else {
                supervisor_reads += 1;
            }
            fixture_observation(identity, unavailable)
        });
        let terminal = fleet.processes[0].terminal;
        cleanup_inert_child(&mut fleet.processes[0]);
        assert_eq!(
            result,
            Err("RSS capture retirement deadline exhausted".into())
        );
        assert!(terminal);
        assert_eq!(supervisor_reads, 2);
        assert_eq!(fleet.resources.sequence, 4);
        assert_eq!(fleet.resources.last.as_ref().unwrap().sequence, 4);
    }

    #[test]
    fn rss_retirement_rebuild_does_not_erase_an_earlier_missing_supervisor() {
        let directory = tempfile::tempdir().unwrap();
        let (process, mut input) = inert_process(directory.path());
        let mut fleet = inert_resource_fleet(directory.path(), process);
        let mut controller_reads = 0;
        let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
            let unavailable = match identity.role.as_str() {
                "controller" => {
                    controller_reads += 1;
                    controller_reads == 1
                }
                "server" => {
                    release_inert_child(&mut input, identity.pid)?;
                    true
                }
                _ => false,
            };
            fixture_observation(identity, unavailable)
        });
        cleanup_inert_child(&mut fleet.processes[0]);
        assert_eq!(result, Err("RSS snapshot unavailable".into()));
        assert_eq!(controller_reads, 2);
        let first = fleet.resource_evidence()["first_rss_failure"].clone();
        assert_eq!(first["candidate_role"], "controller");
        assert_eq!(first["site"], "worker_frame_controller");
        assert_eq!(first["retirement_poll"], "not_applicable");
        assert_eq!(first["candidate_generation"], 0);
        assert_eq!(
            fleet.resources.failure.as_deref(),
            Some("RSS snapshot unavailable")
        );
        let frame = fleet.resources.last.as_ref().unwrap();
        assert_eq!(frame.expected.len(), 2);
        assert_eq!(frame.error.as_deref(), Some("RSS snapshot unavailable"));
        assert!(
            frame
                .validate(&fleet.resources.run, 5, monotonic_ns().unwrap())
                .is_err()
        );
    }

    fn resource_frame(now: u64) -> ResourceFrame {
        let expected = vec![
            supervisor_identity("controller", 10),
            supervisor_identity("worker", 20),
        ];
        ResourceFrame {
            schema: 1,
            run: ResourceRun {
                root: "/private/tmp/owned-attestation-model".into(),
                controller_pid: 10,
                worker_pid: 20,
                group: 20,
            },
            sequence: 1,
            started_ns: now,
            finished_ns: now,
            observations: expected
                .iter()
                .map(|identity| RssObservation {
                    identity: identity.clone(),
                    started_ns: now,
                    finished_ns: now,
                    bytes: Some(4096),
                    missing: None,
                })
                .collect(),
            expected,
            total_bytes: Some(8192),
            child_bytes: Some(0),
            max_total_bytes: 8192,
            error: None,
            terminal: false,
        }
    }

    struct SlowReader {
        bytes: Cursor<Vec<u8>>,
        now: Rc<Cell<u64>>,
        frame: Rc<RefCell<ResourceFrame>>,
    }
    impl Read for SlowReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let now = self.now.get() + 200_000_000;
            self.now.set(now);
            let frame = self.frame.borrow();
            frame
                .validate(&frame.run, frame.sequence, now)
                .map_err(std::io::Error::other)?;
            let limit = out.len().min(4096);
            self.bytes.read(&mut out[..limit])
        }
    }

    #[test]
    fn binary_attestation_keeps_the_owned_resource_frame_fresh_during_slow_reads() {
        let bytes = vec![0xa5; 128 * 1024];
        let now = Rc::new(Cell::new(1));
        let frame = Rc::new(RefCell::new(resource_frame(1)));
        let mut reader = SlowReader {
            bytes: Cursor::new(bytes.clone()),
            now: now.clone(),
            frame: frame.clone(),
        };
        let mut progress = || {
            *frame.borrow_mut() = resource_frame(now.get());
            Ok(())
        };
        let result = binary_digest_reader(&mut reader, &mut progress);
        assert_eq!(
            result,
            Ok(sha256(&bytes)),
            "binary attestation must publish while reading, before the unchanged one-second guard expires"
        );
        assert!(now.get() > RESOURCE_FRESH_NS);
    }

    struct CountedReader {
        bytes: Cursor<Vec<u8>>,
        reads: usize,
        max_request: usize,
    }
    impl Read for CountedReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            self.max_request = self.max_request.max(out.len());
            self.bytes.read(out)
        }
    }

    #[test]
    fn binary_attestation_bounds_reads_and_preserves_the_complete_digest() {
        let bytes = vec![0x35; 3 * 64 * 1024 + 17];
        let mut reader = CountedReader {
            bytes: Cursor::new(bytes.clone()),
            reads: 0,
            max_request: 0,
        };
        let digest = binary_digest_reader(&mut reader, &mut || Ok(())).unwrap();
        assert_eq!(digest, sha256(&bytes));
        assert_eq!(reader.max_request, 64 * 1024);
        assert_eq!(reader.reads, 5);
    }

    #[test]
    fn binary_attestation_stops_without_another_read_after_progress_failure() {
        let mut reader = CountedReader {
            bytes: Cursor::new(vec![0x72; 2 * 64 * 1024]),
            reads: 0,
            max_request: 0,
        };
        let mut progress_calls = 0;
        let result = binary_digest_reader(&mut reader, &mut || {
            progress_calls += 1;
            if progress_calls == 2 {
                Err("owned outer stop".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("owned outer stop".into()));
        assert_eq!(reader.reads, 1);
    }

    #[test]
    fn binary_attestation_rejects_a_file_changed_while_streaming() {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owned-binary");
        fs::write(&path, vec![0x33; 2 * 64 * 1024]).unwrap();
        let mut progress_calls = 0;
        let result = binary_digest_file(&path, &mut || {
            progress_calls += 1;
            if progress_calls == 2 {
                OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(&[0x99])
                    .unwrap();
            }
            Ok(())
        });
        assert_eq!(result, Err("binary changed during attestation".into()));
    }

    #[test]
    fn binary_attestation_rejects_same_path_replacement_while_streaming() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owned-binary");
        let replacement = directory.path().join("replacement");
        let bytes = vec![0x33; 2 * 64 * 1024];
        fs::write(&path, &bytes).unwrap();
        fs::write(&replacement, &bytes).unwrap();
        let mut progress_calls = 0;
        let result = binary_digest_file(&path, &mut || {
            progress_calls += 1;
            if progress_calls == 2 {
                fs::rename(&replacement, &path).unwrap();
            }
            Ok(())
        });
        assert_eq!(result, Err("binary changed during attestation".into()));
    }

    #[test]
    fn binary_attestation_does_not_read_after_an_expired_deadline() {
        let mut reader = CountedReader {
            bytes: Cursor::new(vec![0x42; 4096]),
            reads: 0,
            max_request: 0,
        };
        let deadline = Instant::now();
        let result = binary_digest_reader(&mut reader, &mut || {
            if Instant::now() >= deadline {
                Err("binary attestation deadline exhausted".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("binary attestation deadline exhausted".into()));
        assert_eq!(reader.reads, 0);
    }
}
