//! Owned process controller. File limits are cooperative observations, not emission limits.
use super::{
    config::sha256,
    contracts::*,
    progress_trace::{self, Label},
};
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
const FIRST_FRAME_VALIDATION_CAP: usize = 2048;
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FrameValidationSite {
    BeforeRssAcquisition,
    FinalRecompose,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
struct FrameRunScalars {
    controller_pid: u32,
    worker_pid: u32,
    group: u32,
}
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
struct FrameValidationPredicates {
    schema: bool,
    run: bool,
    sequence: bool,
    frame_error: bool,
    time_order: bool,
    future: bool,
    stale: bool,
    wire_cap: bool,
    other_contract: bool,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
struct FirstFrameValidationFailure {
    schema: &'static str,
    site: FrameValidationSite,
    expected_schema: u32,
    expected_run: FrameRunScalars,
    accepted_sequence: u64,
    freshness_ns: u64,
    frame_schema: u32,
    frame_run: FrameRunScalars,
    frame_sequence: u64,
    frame_started_ns: u64,
    frame_finished_ns: u64,
    validation_now_ns: u64,
    age_ns: Option<u64>,
    failed: FrameValidationPredicates,
}
#[derive(Default)]
struct OuterSampleEvidence {
    first_rss_failure: Option<FirstRssFailure>,
    first_frame_validation_failure: Option<FirstFrameValidationFailure>,
    #[cfg(test)]
    validation_clock: Option<Box<dyn FnMut() -> Result<u64>>>,
}
impl OuterSampleEvidence {
    fn validation_now(&mut self) -> Result<u64> {
        #[cfg(test)]
        if let Some(clock) = &mut self.validation_clock {
            return clock();
        }
        monotonic_ns()
    }

    fn retain_frame_validation_failure(
        &mut self,
        frame: &ResourceFrame,
        run: &ResourceRun,
        sequence: u64,
        now: u64,
        site: FrameValidationSite,
    ) {
        if self.first_frame_validation_failure.is_some()
            || frame.validate(run, sequence, now).is_ok()
        {
            return;
        }
        let age_ns = now.checked_sub(frame.started_ns);
        let mut failed = FrameValidationPredicates {
            schema: frame.schema != 1,
            run: &frame.run != run
                || frame.run.controller_pid == 0
                || frame.run.worker_pid == 0
                || frame.run.controller_pid == frame.run.worker_pid
                || frame.run.group != frame.run.worker_pid
                || !Path::new(&frame.run.root).is_absolute(),
            sequence: frame.sequence == 0 || frame.sequence < sequence,
            frame_error: frame.error.is_some(),
            time_order: frame.started_ns > frame.finished_ns,
            future: frame.finished_ns > now,
            stale: age_ns.is_some_and(|age| age > RESOURCE_FRESH_NS),
            wire_cap: serde_json::to_vec(frame)
                .is_ok_and(|bytes| bytes.len() as u64 > RESOURCE_CAP),
            other_contract: false,
        };
        failed.other_contract = !(failed.schema
            || failed.run
            || failed.sequence
            || failed.frame_error
            || failed.time_order
            || failed.future
            || failed.stale
            || failed.wire_cap);
        let record = FirstFrameValidationFailure {
            schema: "mount-rs.cache-frame-validation-failure.v1",
            site,
            expected_schema: 1,
            expected_run: FrameRunScalars {
                controller_pid: run.controller_pid,
                worker_pid: run.worker_pid,
                group: run.group,
            },
            accepted_sequence: sequence,
            freshness_ns: RESOURCE_FRESH_NS,
            frame_schema: frame.schema,
            frame_run: FrameRunScalars {
                controller_pid: frame.run.controller_pid,
                worker_pid: frame.run.worker_pid,
                group: frame.run.group,
            },
            frame_sequence: frame.sequence,
            frame_started_ns: frame.started_ns,
            frame_finished_ns: frame.finished_ns,
            validation_now_ns: now,
            age_ns,
            failed,
        };
        // Only this closed scalar DTO crosses into the receipt, never frame strings/paths.
        if serde_json::to_vec_pretty(&record)
            .is_ok_and(|bytes| bytes.len() <= FIRST_FRAME_VALIDATION_CAP)
        {
            self.first_frame_validation_failure = Some(record);
        }
    }
}
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
    WorkerFrameStopRequestedChild,
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
#[derive(Clone, Copy, Debug, Serialize)]
struct TaskinfoFailure {
    sample_started_ns: Option<u64>,
    sample_finished_ns: Option<u64>,
    proc_pidinfo_return_bytes: i32,
    proc_pidinfo_expected_bytes: u32,
    proc_pidinfo_errno_after_call: i32,
}
#[derive(Clone, Copy, Debug, Default, Serialize)]
struct RssDiagnostic {
    taskinfo: Option<TaskinfoFailure>,
    successful_sigint_observed_ns: Option<u64>,
}
impl RssDiagnostic {
    fn with_owned_stop(mut self, stamp: Option<u64>) -> Self {
        self.successful_sigint_observed_ns = stamp;
        self
    }
}
#[derive(Clone, Copy, Serialize)]
struct FirstRssReap {
    schema: &'static str,
    first_resource_sequence: u64,
    candidate_role: RssRole,
    candidate_pid: u32,
    candidate_generation: u64,
    candidate_node: Option<u8>,
    successful_sigint_observed_ns: Option<u64>,
    actual_reap_observed_ns: Option<u64>,
    reaped_success: bool,
    forced: bool,
}
#[derive(Clone, Copy)]
struct StopBudget {
    deadline: Instant,
    force_at: Instant,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PendingDisposition {
    ConfirmedReap,
    DeadlineExhausted,
    OtherFailure,
    FailedExit,
    ForcedExit,
    OwnerChanged,
    PollFailed,
}
#[derive(Clone, Copy, Serialize)]
struct PendingRetirementEvidence {
    schema: &'static str,
    resource_sequence: u64,
    candidate_role: RssRole,
    candidate_pid: u32,
    candidate_generation: u64,
    candidate_node: Option<u8>,
    eligible_ns: u64,
    deadline_ns: u64,
    settled_ns: Option<u64>,
    disposition: PendingDisposition,
}
trait StopObserver {
    fn now_ns(&mut self) -> Result<u64>;
    fn now(&mut self) -> Instant;
    fn taskinfo_bytes(&self) -> Option<u32>;
    fn measure(
        &mut self,
        identity: RssIdentity,
        verify_parent: bool,
    ) -> RssReadResult<RssAcquisition>;
    fn poll(&mut self, process: &mut OwnedProcess) -> Result<()>;
    fn wait(&mut self, deadline: Instant) -> Result<()>;
    fn progress(&mut self, root: &Path) -> Result<()>;
}
struct NativeStopObserver;
impl StopObserver for NativeStopObserver {
    fn now_ns(&mut self) -> Result<u64> {
        monotonic_ns()
    }
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn taskinfo_bytes(&self) -> Option<u32> {
        #[cfg(target_os = "macos")]
        {
            Some(std::mem::size_of::<libc::proc_taskinfo>() as u32)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }
    fn measure(
        &mut self,
        identity: RssIdentity,
        verify_parent: bool,
    ) -> RssReadResult<RssAcquisition> {
        measured(identity, verify_parent)
    }
    fn poll(&mut self, process: &mut OwnedProcess) -> Result<()> {
        process.poll_exit()
    }
    fn wait(&mut self, deadline: Instant) -> Result<()> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(RSS_UNAVAILABLE)?;
        thread::sleep(remaining.min(Duration::from_millis(1)));
        Ok(())
    }
    fn progress(&mut self, root: &Path) -> Result<()> {
        all_log_bytes(root)?;
        if free_disk(root)? < FREE_DISK_FLOOR {
            return Err("disk floor lost during RSS retirement".into());
        }
        Ok(())
    }
}
struct MeasureStopObserver<'a, F> {
    measure: &'a mut F,
}
impl<F> StopObserver for MeasureStopObserver<'_, F>
where
    F: FnMut(RssIdentity, bool) -> RssReadResult<RssAcquisition>,
{
    fn now_ns(&mut self) -> Result<u64> {
        monotonic_ns()
    }
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn taskinfo_bytes(&self) -> Option<u32> {
        NativeStopObserver.taskinfo_bytes()
    }
    fn measure(
        &mut self,
        identity: RssIdentity,
        verify_parent: bool,
    ) -> RssReadResult<RssAcquisition> {
        (self.measure)(identity, verify_parent)
    }
    fn poll(&mut self, process: &mut OwnedProcess) -> Result<()> {
        process.poll_exit()
    }
    fn wait(&mut self, deadline: Instant) -> Result<()> {
        NativeStopObserver.wait(deadline)
    }
    fn progress(&mut self, root: &Path) -> Result<()> {
        NativeStopObserver.progress(root)
    }
}
struct PendingOwner {
    identity: RssIdentity,
}
#[derive(Clone, Copy)]
struct RetirementClosure {
    disposition: PendingDisposition,
    settled_ns: Option<u64>,
}
struct RetainedRetirement {
    identity: RssIdentity,
    capture_started_ns: u64,
    deadline_ns: u64,
    deadline: Instant,
    cleanup_ns: u64,
    cleanup_deadline: Instant,
    eligible_ns: u64,
    closed: Option<RetirementClosure>,
}
struct RetirementCycle {
    captured_ns: u64,
    deadline_ns: u64,
    deadline: Instant,
    cleanup_ns: u64,
    cleanup_deadline: Instant,
    owners: Vec<PendingOwner>,
    disposition: PendingDisposition,
    evidence_ns: Option<u64>,
}
impl RetirementCycle {
    fn new(observer: &mut impl StopObserver, outer_ns: u64) -> Result<Self> {
        // Common-clock -> Instant conversion: Instant first is conservative.
        let anchor = observer.now();
        let captured_ns = observer.now_ns()?;
        let deadline_ns = captured_ns
            .checked_add(POLL_MS * 1_000_000)
            .ok_or("RSS capture deadline overflow")?
            .min(outer_ns);
        let deadline = anchored_deadline(anchor, captured_ns, deadline_ns)?;
        let cleanup_ns = captured_ns
            .checked_add(SHUTDOWN_SECONDS * 1_000_000_000)
            .ok_or("RSS cleanup deadline overflow")?
            .min(outer_ns);
        Ok(Self {
            captured_ns,
            deadline_ns,
            deadline,
            cleanup_ns,
            cleanup_deadline: anchored_deadline(anchor, captured_ns, cleanup_ns)?,
            owners: Vec::new(),
            disposition: PendingDisposition::OtherFailure,
            evidence_ns: None,
        })
    }
    fn tighten_common(&mut self, stamp: u64, observer: &mut impl StopObserver) -> Result<()> {
        self.deadline_ns = self.deadline_ns.min(stamp);
        let anchor = observer.now();
        let now = observer.now_ns()?;
        self.deadline = self
            .deadline
            .min(anchored_deadline(anchor, now, self.deadline_ns)?);
        Ok(())
    }
    fn tighten_owner(
        &mut self,
        budget: StopBudget,
        observer: &mut impl StopObserver,
    ) -> Result<()> {
        // Instant -> common-clock conversion: common first is conservative.
        let common = observer.now_ns()?;
        let native = observer.now();
        let convert = |end: Instant| -> Result<u64> {
            let nanos = u64::try_from(
                end.checked_duration_since(native)
                    .ok_or(RSS_UNAVAILABLE)?
                    .as_nanos(),
            )
            .map_err(|_| "RSS stop duration overflow")?;
            common
                .checked_add(nanos)
                .ok_or_else(|| "RSS stop deadline overflow".into())
        };
        self.cleanup_ns = self.cleanup_ns.min(convert(budget.deadline)?);
        self.cleanup_deadline = self.cleanup_deadline.min(budget.deadline);
        self.deadline_ns = self
            .deadline_ns
            .min(convert(budget.deadline.min(budget.force_at))?);
        self.deadline = self.deadline.min(budget.deadline).min(budget.force_at);
        self.check(observer)
    }
    fn check(&mut self, observer: &mut impl StopObserver) -> Result<()> {
        if observer.now_ns()? >= self.deadline_ns || observer.now() >= self.deadline {
            self.disposition = PendingDisposition::DeadlineExhausted;
            return Err(RSS_UNAVAILABLE.into());
        }
        Ok(())
    }
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
    diagnostic: RssDiagnostic,
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
            diagnostic: RssDiagnostic::default(),
        })
    }
    fn with_diagnostic(mut self, diagnostic: RssDiagnostic) -> Self {
        self.diagnostic = diagnostic;
        self
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
    diagnostic: RssDiagnostic,
}
impl FirstRssFailure {
    fn new(
        run: &ResourceRun,
        producer: RssProducer,
        sequence: u64,
        candidate: RssCandidate,
    ) -> Self {
        Self {
            schema: "mount-rs.cache-rss-failure.v2",
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
            diagnostic: candidate.diagnostic,
        }
    }
}
#[derive(Debug)]
struct RssReadError {
    category: RssCategory,
    message: String,
    observed_ns: Option<u64>,
    bytes: Option<u64>,
    taskinfo: Option<TaskinfoFailure>,
}
impl RssReadError {
    fn new(category: RssCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            observed_ns: None,
            bytes: None,
            taskinfo: None,
        }
    }
    fn diagnostic(&self) -> RssDiagnostic {
        // Owned stop observations are attached later from the retained Child's owner.
        RssDiagnostic {
            taskinfo: self.taskinfo,
            successful_sigint_observed_ns: None,
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
    diagnostic: RssDiagnostic,
}
impl From<RssObservation> for RssAcquisition {
    fn from(observation: RssObservation) -> Self {
        Self {
            observation,
            category: None,
            diagnostic: RssDiagnostic::default(),
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
        taskinfo: sample.as_ref().err().and_then(|error| error.taskinfo),
    })?;
    let (bytes, missing, category, diagnostic) = match sample {
        Ok(value) => (Some(value), None, None, RssDiagnostic::default()),
        Err(error) => {
            let diagnostic = error.diagnostic();
            (None, Some(error.message), Some(error.category), diagnostic)
        }
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
        diagnostic,
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
    published_started_ns: u64,
    published_sequence: u64,
    first_rss_failure: Option<FirstRssFailure>,
    first_rss_failure_retention: RssRetention,
    first_rss_reap: Option<FirstRssReap>,
    first_pending_retirement: Option<PendingRetirementEvidence>,
}
impl ResourceMonitor {
    fn stop_requested(&mut self) -> Result<()> {
        self.stop_requested_at(monotonic_ns()?)
    }
    fn stop_requested_at(&mut self, now: u64) -> Result<()> {
        let path = Path::new(&self.run.root).join("stop.json");
        if path.exists() {
            let stop: ResourceStop = resource_read(&path)?;
            stop.validate(&self.run, now, self.outer_ns)?;
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
    fn remember_reap(&mut self, process: &OwnedProcess) {
        if self.first_rss_reap.is_some()
            || !process.terminal
            || !process.receipt.reaped
            || process.receipt.pid != process.child.id()
        {
            return;
        }
        let Some(first) = self.first_rss_failure else {
            return;
        };
        let Some(candidate) = RssCandidate::from_identity(
            &process.identity(),
            first.site,
            first.category,
            first.retirement_poll,
            first.observed_ns,
            first.candidate_bytes,
        ) else {
            return;
        };
        if candidate.role != first.candidate_role
            || candidate.pid != first.candidate_pid
            || candidate.generation != first.candidate_generation
            || candidate.node != first.candidate_node
        {
            return;
        }
        let record = FirstRssReap {
            schema: "mount-rs.cache-rss-reap-link.v1",
            first_resource_sequence: first.resource_sequence,
            candidate_role: candidate.role,
            candidate_pid: candidate.pid,
            candidate_generation: candidate.generation,
            candidate_node: candidate.node,
            successful_sigint_observed_ns: process.successful_sigint_observed_ns,
            actual_reap_observed_ns: process.actual_reap_observed_ns,
            reaped_success: process.receipt.success,
            forced: process.receipt.forced,
        };
        // This later scalar observation never revises the retained first failure,
        // qualifies launch attestation, or replaces the original fatal error.
        if serde_json::to_vec_pretty(&record).is_ok_and(|bytes| bytes.len() <= FIRST_RSS_CAP) {
            self.first_rss_reap = Some(record);
        }
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
    let sample_started_ns = monotonic_ns().ok();
    // Caller binds this PID to self, a verified parent, or a retained unreaped Child.
    // Reset this thread's errno before the single native call and capture it
    // immediately afterward, before clocks, formatting, allocation, or other I/O.
    unsafe {
        *libc::__error() = 0;
    }
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };
    let errno_after_call = unsafe { *libc::__error() };
    let sample_finished_ns = monotonic_ns().ok();
    if got != size as libc::c_int {
        let mut error = RssReadError::unavailable();
        error.taskinfo = Some(TaskinfoFailure {
            sample_started_ns,
            sample_finished_ns,
            proc_pidinfo_return_bytes: got,
            proc_pidinfo_expected_bytes: size as u32,
            proc_pidinfo_errno_after_call: errno_after_call,
        });
        return Err(error);
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
    object_store_projection: Option<serde_json::Value>,
    pub quic: Option<SocketAddr>,
    peer: Option<SocketAddr>,
    pub cache: Option<PathBuf>,
    terminal: bool,
    stop_requested: bool,
    stop_budget: Option<StopBudget>,
    retirement: Option<RetainedRetirement>,
    successful_sigint_observed_ns: Option<u64>,
    actual_reap_observed_ns: Option<u64>,
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
            object_store_projection: None,
            quic: None,
            peer,
            cache,
            terminal: false,
            stop_requested: false,
            stop_budget: None,
            retirement: None,
            successful_sigint_observed_ns: None,
            actual_reap_observed_ns: None,
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
            self.actual_reap_observed_ns = monotonic_ns().ok();
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
        if value == libc::SIGINT {
            if !self.stop_requested {
                self.successful_sigint_observed_ns = monotonic_ns().ok();
            }
            self.stop_requested = true;
        }
        Ok(())
    }
    fn request_stop(&mut self, budget: StopBudget) -> Result<()> {
        let already_requested = self.stop_requested;
        self.signal(libc::SIGINT)?;
        self.bind_stop_budget(already_requested, budget);
        Ok(())
    }
    fn bind_stop_budget(&mut self, already_requested: bool, budget: StopBudget) {
        if !already_requested {
            self.stop_budget = Some(budget);
        } else if let Some(original) = &mut self.stop_budget {
            original.deadline = original.deadline.min(budget.deadline);
            original.force_at = original.force_at.min(budget.force_at);
        }
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
        diagnostic: RssDiagnostic,
    ) {
        if self.first_rss_failure.is_none() {
            self.first_rss_failure = RssCandidate::from_identity(
                &self.identity(),
                site,
                category,
                poll,
                observed_ns,
                bytes,
            )
            .map(|candidate| {
                candidate
                    .with_diagnostic(diagnostic.with_owned_stop(self.successful_sigint_observed_ns))
            });
        }
    }
    fn sample_rss_at_with(
        &mut self,
        site: RssSite,
        allow_retirement: bool,
        sample: &mut impl FnMut(u32) -> RssReadResult<u64>,
        poll: &mut impl FnMut(&mut Child) -> std::io::Result<Option<std::process::ExitStatus>>,
    ) -> Result<()> {
        // All low-level callers honor retained quarantine independently of stop
        // metadata. Fleet dispatch performs the full fresh cleanup observation.
        if self.retirement.is_some() && !self.terminal {
            self.poll_exit_with(poll)?;
            return Err(RSS_UNAVAILABLE.into());
        }
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
                    error.diagnostic(),
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
                            error.diagnostic(),
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
                            error.diagnostic(),
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
                    error.diagnostic(),
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
            let banks = parse_banks(&stderr)?;
            self.object_store_projection = Some(super::object_store_projection::project_cli(
                &super::object_store_projection::CliBinding {
                    node: &self.receipt.node,
                    generation: self.receipt.generation,
                    pid: self.receipt.pid,
                    path: &self.stderr.to_string_lossy(),
                    owner_complete: true,
                },
                stderr.as_bytes(),
            ));
            Some(banks)
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
    pub object_store_observations: Vec<serde_json::Value>,
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
            object_store_observations: Vec::new(),
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
                published_started_ns: 0,
                published_sequence: 0,
                first_rss_failure: None,
                first_rss_failure_retention: RssRetention::NotAttempted,
                first_rss_reap: None,
                first_pending_retirement: None,
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
            "first_rss_failure_retention":self.resources.first_rss_failure_retention,
            "first_rss_reap":self.resources.first_rss_reap,
            "first_pending_retirement":self.resources.first_pending_retirement})
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
        if self.has_retirement_quarantine() {
            return self.cleanup_retirement_with(false, false, &mut NativeStopObserver);
        }
        if allow_retirement
            && matches!(site, RssSite::StopTarget | RssSite::CleanupChild)
            && self.processes[index].stop_budget.is_some()
        {
            return self.observe_stopped_process_with(index, site, &mut NativeStopObserver);
        }
        self.sample_process_with(
            index,
            site,
            allow_retirement,
            &mut rss,
            &mut Child::try_wait,
        )
    }
    fn observe_stopped_process_with(
        &mut self,
        index: usize,
        site: RssSite,
        observer: &mut impl StopObserver,
    ) -> Result<()> {
        if !matches!(site, RssSite::StopTarget | RssSite::CleanupChild) {
            return Err(RSS_UNAVAILABLE.into());
        }
        let identity = self
            .processes
            .get(index)
            .ok_or("RSS stop target absent")?
            .identity();
        self.observe_retirement_with(true, false, Some((identity, site)), observer)
    }
    fn observe_stopped_frame_with(
        &mut self,
        force: bool,
        terminal: bool,
        observer: &mut impl StopObserver,
    ) -> Result<()> {
        self.observe_retirement_with(force, terminal, None, observer)
    }
    fn observe_retirement_with(
        &mut self,
        force: bool,
        terminal: bool,
        direct: Option<(RssIdentity, RssSite)>,
        observer: &mut impl StopObserver,
    ) -> Result<()> {
        if self.has_retirement_quarantine() {
            return self.cleanup_retirement_with(force, terminal, observer);
        }
        let mut cycle = RetirementCycle::new(observer, self.resources.outer_ns)?;
        let mut result =
            self.capture_retirement_with(force, terminal, direct.as_ref(), observer, &mut cycle);
        // A provisional typed diagnostic is not a fatal latch. Only settlement of
        // the entire bounded cycle decides whether the observation gap was accepted.
        if !cycle.owners.is_empty() {
            let settled_ns = match observer.now_ns() {
                Ok(stamp) => {
                    if cycle.disposition == PendingDisposition::ConfirmedReap
                        && (stamp >= cycle.deadline_ns || observer.now() >= cycle.deadline)
                    {
                        cycle.disposition = PendingDisposition::DeadlineExhausted;
                        if result.is_ok() {
                            result = Err(RSS_UNAVAILABLE.into());
                        }
                    }
                    Some(stamp)
                }
                Err(error) => {
                    cycle.disposition = PendingDisposition::OtherFailure;
                    if result.is_ok() {
                        result = Err(error);
                    }
                    None
                }
            };
            if let Some(record) = &mut self.resources.first_pending_retirement
                && Some(record.eligible_ns) == cycle.evidence_ns
            {
                record.deadline_ns = record.deadline_ns.min(cycle.deadline_ns);
                record.settled_ns = settled_ns;
                record.disposition = cycle.disposition;
            }
            // Proof remains attached to the actual retained Child across every
            // fatal return. The first closure, including a null clock, is final.
            for process in &mut self.processes {
                if let Some(proof) = &mut process.retirement
                    && proof.closed.is_none()
                    && cycle
                        .owners
                        .iter()
                        .any(|owner| owner.identity == proof.identity)
                {
                    proof.deadline_ns = proof.deadline_ns.min(cycle.deadline_ns);
                    proof.deadline = proof.deadline.min(cycle.deadline);
                    proof.cleanup_ns = proof.cleanup_ns.min(cycle.cleanup_ns);
                    proof.cleanup_deadline = proof.cleanup_deadline.min(cycle.cleanup_deadline);
                    proof.closed = Some(RetirementClosure {
                        disposition: cycle.disposition,
                        settled_ns,
                    });
                }
            }
        }
        if let Err(error) = &result {
            // Freeze cleanup at the original failure/capture and owner bounds
            // before remember() can derive a later cleanup allowance.
            self.resources.stop_deadline_ns = Some(
                self.resources
                    .stop_deadline_ns
                    .map_or(cycle.cleanup_ns, |v| v.min(cycle.cleanup_ns)),
            );
            self.resources.remember(error.clone());
        }
        result
    }
    fn has_retirement_quarantine(&self) -> bool {
        self.processes.iter().any(|process| {
            process.retirement.as_ref().is_some_and(|proof| {
                !process.terminal
                    || process.identity() != proof.identity
                    || process.receipt.pid != proof.identity.pid
                    || proof.closed.is_none_or(|closed| {
                        closed.disposition != PendingDisposition::ConfirmedReap
                    })
            })
        })
    }
    fn retain_cycle_fences(&mut self, cycle: &RetirementCycle) {
        for process in &mut self.processes {
            if let Some(proof) = &mut process.retirement
                && proof.closed.is_none()
                && cycle
                    .owners
                    .iter()
                    .any(|owner| owner.identity == proof.identity)
            {
                proof.deadline_ns = proof.deadline_ns.min(cycle.deadline_ns);
                proof.deadline = proof.deadline.min(cycle.deadline);
                proof.cleanup_ns = proof.cleanup_ns.min(cycle.cleanup_ns);
                proof.cleanup_deadline = proof.cleanup_deadline.min(cycle.cleanup_deadline);
            }
        }
    }
    fn cleanup_retirement_with(
        &mut self,
        _force: bool,
        terminal: bool,
        observer: &mut impl StopObserver,
    ) -> Result<()> {
        let original = self
            .resources
            .failure
            .clone()
            .unwrap_or_else(|| RSS_UNAVAILABLE.into());
        let settled_ns = observer.now_ns().ok();
        // Even an interrupted, never-closed capture cannot resume on reentry.
        // Closure metadata is retained on the exact owner, not inferred from the
        // nullable first-global settlement timestamp or a current vector index.
        for process in &mut self.processes {
            if let Some(proof) = &mut process.retirement {
                self.resources.stop_deadline_ns = Some(
                    self.resources
                        .stop_deadline_ns
                        .map_or(proof.cleanup_ns, |end| end.min(proof.cleanup_ns)),
                );
                if proof.closed.is_none() {
                    let closed = RetirementClosure {
                        disposition: PendingDisposition::OtherFailure,
                        settled_ns,
                    };
                    proof.closed = Some(closed);
                    if let Some(record) = &mut self.resources.first_pending_retirement
                        && record.eligible_ns == proof.eligible_ns
                        && record.candidate_pid == proof.identity.pid
                        && record.candidate_generation == proof.identity.generation
                    {
                        record.deadline_ns = record.deadline_ns.min(proof.deadline_ns);
                        record.disposition = closed.disposition;
                        record.settled_ns = closed.settled_ns;
                    }
                }
            }
        }
        self.resources.remember(original.clone());
        if let Err(error) = self.cleanup_retirement_pass(terminal, observer) {
            self.resources.remember(error);
        }
        // A complete late cleanup observation cannot rehabilitate the first gap.
        Err(original)
    }
    fn cleanup_retirement_pass(
        &mut self,
        terminal: bool,
        observer: &mut impl StopObserver,
    ) -> Result<()> {
        let now = observer.now_ns()?;
        if let Err(error) = self.resources.stop_requested_at(now) {
            self.resources.remember(error);
        }
        let cleanup_ns = self
            .processes
            .iter()
            .filter_map(|p| p.retirement.as_ref().map(|proof| proof.cleanup_ns))
            .fold(
                self.resources
                    .outer_ns
                    .min(self.resources.stop_deadline_ns.unwrap_or(u64::MAX)),
                u64::min,
            );
        let anchor = observer.now();
        let common = observer.now_ns()?;
        let cleanup_deadline = self
            .processes
            .iter()
            .filter_map(|p| p.retirement.as_ref().map(|proof| proof.cleanup_deadline))
            .fold(anchored_deadline(anchor, common, cleanup_ns)?, Instant::min);
        let check = |observer: &mut dyn StopObserver| -> Result<()> {
            if observer.now_ns()? >= cleanup_ns || observer.now() >= cleanup_deadline {
                return Err("closed RSS retirement cleanup deadline exhausted".into());
            }
            Ok(())
        };
        check(observer)?;
        observer.progress(&self.root)?;
        check(observer)?;
        let started_ns = observer.now_ns()?;
        let mut incomplete = false;
        for process in &mut self.processes {
            if let Some(proof) = &process.retirement
                && (process.identity() != proof.identity
                    || process.receipt.pid != proof.identity.pid
                    || proof.capture_started_ns > proof.eligible_ns
                    || proof.closed.is_none())
            {
                return Err("closed RSS retirement owner/proof changed".into());
            }
            observer.poll(process)?;
            if let Some(proof) = &process.retirement {
                if process.identity() != proof.identity || process.receipt.pid != proof.identity.pid
                {
                    return Err("closed RSS retirement owner changed during poll".into());
                }
                incomplete |= !process.terminal;
                self.resources.remember_reap(process);
            }
            if process.terminal
                && (!process.receipt.reaped || !process.receipt.success || process.receipt.forced)
            {
                // Retain the failure, but actual reaping allows ordinary complete
                // cleanup publication. This is never a successful pending capture.
                self.resources
                    .remember("unsuccessful owned reap during closed RSS cleanup".into());
            }
        }
        let mut expected = vec![
            supervisor_identity("controller", self.resources.run.controller_pid),
            supervisor_identity("worker", self.resources.run.worker_pid),
        ];
        expected.extend(
            self.processes
                .iter()
                .filter(|p| !p.terminal)
                .map(OwnedProcess::identity),
        );
        if expected.len() > RESOURCE_MAX_IDENTITIES {
            return Err("excessive RSS cleanup roster".into());
        }
        let mut observations = Vec::with_capacity(expected.len());
        let mut total = 0u64;
        for identity in &expected {
            let child_index = self
                .processes
                .iter()
                .position(|p| p.child.id() == identity.pid);
            if child_index.is_some_and(|index| self.processes[index].retirement.is_some()) {
                // Quarantine wins even if mutable stop_budget/stop flags changed.
                incomplete = true;
                continue;
            }
            let site = if identity.role == "controller" {
                RssSite::WorkerFrameController
            } else if identity.role == "worker" {
                RssSite::WorkerFrameWorker
            } else {
                RssSite::WorkerFrameChild
            };
            let value = match observer.measure(identity.clone(), identity.role == "controller") {
                Ok(value) => value,
                Err(error) => {
                    self.resources.remember_rss(
                        RssCandidate::from_identity(
                            identity,
                            site,
                            error.category,
                            RssPoll::NotAttempted,
                            error.observed_ns,
                            error.bytes,
                        )
                        .map(|candidate| candidate.with_diagnostic(error.diagnostic())),
                    );
                    return Err(error.message);
                }
            };
            let observation = &value.observation;
            if observation.identity != *identity
                || observation.started_ns < started_ns
                || observation.started_ns > observation.finished_ns
                || observation.finished_ns > observer.now_ns()?
            {
                return Err("invalid closed RSS cleanup acquisition".into());
            }
            if let Some(category) = value.category() {
                self.resources.remember_rss(
                    RssCandidate::from_identity(
                        identity,
                        site,
                        category,
                        RssPoll::NotAttempted,
                        Some(observation.finished_ns),
                        observation.bytes,
                    )
                    .map(|candidate| candidate.with_diagnostic(value.diagnostic)),
                );
                if let (Some(index), Some(bytes)) = (child_index, observation.bytes) {
                    // Preserve real numeric cap-crossing observations in the owned
                    // receipt before returning the failure; missing RSS adds nothing.
                    self.processes[index]
                        .record_rss(bytes)
                        .map_err(|error| error.message)?;
                }
                return Err(observation
                    .missing
                    .clone()
                    .unwrap_or_else(|| "closed RSS cleanup cap exceeded".into()));
            }
            let bytes = observation.bytes.ok_or(RSS_UNAVAILABLE)?;
            if let Some(index) = child_index {
                self.processes[index]
                    .record_rss(bytes)
                    .map_err(|error| error.message)?;
            }
            total = total.checked_add(bytes).ok_or("RSS cleanup sum overflow")?;
            if total >= RSS_CAP {
                return Err("observed RSS cleanup aggregate cap exceeded".into());
            }
            observations.push(value.observation);
            check(observer)?;
        }
        check(observer)?;
        if incomplete {
            return Ok(());
        }
        let finished_ns = observer.now_ns()?;
        let totals = rss_totals(&expected, &observations)?;
        self.resources.sequence = self
            .resources
            .sequence
            .checked_add(1)
            .ok_or("resource sequence overflow")?;
        self.resources.max_total = self.resources.max_total.max(totals.total);
        let mut frame = ResourceFrame {
            schema: 1,
            run: self.resources.run.clone(),
            sequence: self.resources.sequence,
            started_ns,
            finished_ns,
            expected,
            observations,
            total_bytes: Some(totals.total),
            child_bytes: Some(totals.children),
            max_total_bytes: self.resources.max_total,
            error: None,
            terminal,
        };
        frame.validate(&self.resources.run, frame.sequence, observer.now_ns()?)?;
        frame.error = self.resources.failure.clone();
        check(observer)?;
        resource_write(&self.root, "resource.json", &frame)?;
        self.resources.published_ns = frame.finished_ns;
        self.resources.published_started_ns = frame.started_ns;
        self.resources.published_sequence = frame.sequence;
        check(observer)?;
        self.resources.last = Some(frame);
        Ok(())
    }
    fn capture_retirement_with(
        &mut self,
        force: bool,
        terminal: bool,
        direct: Option<&(RssIdentity, RssSite)>,
        observer: &mut impl StopObserver,
        cycle: &mut RetirementCycle,
    ) -> Result<()> {
        let mut committed: Option<ResourceFrame> = None;
        let retained_roster = self
            .processes
            .iter()
            .map(OwnedProcess::identity)
            .collect::<Vec<_>>();
        loop {
            cycle.check(observer)?;
            let now = observer.now_ns()?;
            if let Err(error) = self.resources.stop_requested_at(now) {
                // An outer stop remains sticky while owned cleanup continues.
                self.resources.stop_deadline_ns = Some(
                    self.resources
                        .stop_deadline_ns
                        .map_or(cycle.cleanup_ns, |v| v.min(cycle.cleanup_ns)),
                );
                self.resources.remember(error);
            }
            if let Some(end) = self.resources.stop_deadline_ns {
                cycle.cleanup_ns = cycle.cleanup_ns.min(end);
                let anchor = observer.now();
                let common = observer.now_ns()?;
                cycle.cleanup_deadline = cycle
                    .cleanup_deadline
                    .min(anchored_deadline(anchor, common, end)?);
                let force = end
                    .checked_sub(FORCE_SECONDS * 1_000_000_000)
                    .ok_or(RSS_UNAVAILABLE)?;
                if force <= now {
                    cycle.disposition = PendingDisposition::DeadlineExhausted;
                    return Err(RSS_UNAVAILABLE.into());
                }
                cycle.tighten_common(force, observer)?;
            }
            self.retain_cycle_fences(cycle);
            observer.progress(&self.root)?;
            cycle.check(observer)?;
            // Retained children are never removed here. finish_process remains the
            // only owner of attestation, socket, lock, bank and receipt completion.
            for pending in &cycle.owners {
                let process = self
                    .processes
                    .iter_mut()
                    .find(|process| {
                        process
                            .retirement
                            .as_ref()
                            .is_some_and(|proof| proof.identity == pending.identity)
                    })
                    .ok_or("retained pending RSS owner absent")?;
                if process.identity() != pending.identity
                    || process.receipt.pid != pending.identity.pid
                {
                    cycle.disposition = PendingDisposition::OwnerChanged;
                    return Err("pending RSS owner changed".into());
                }
                if let Err(error) = observer.poll(process) {
                    cycle.disposition = PendingDisposition::PollFailed;
                    return Err(error);
                }
                if process.identity() != pending.identity
                    || process.receipt.pid != pending.identity.pid
                {
                    cycle.disposition = PendingDisposition::OwnerChanged;
                    return Err("pending RSS owner changed".into());
                }
                if process.terminal {
                    self.resources.remember_reap(process);
                    if process.receipt.forced {
                        cycle.disposition = PendingDisposition::ForcedExit;
                        return Err("forced pending RSS retirement".into());
                    }
                    if !process.receipt.reaped || !process.receipt.success {
                        cycle.disposition = PendingDisposition::FailedExit;
                        return Err("unsuccessful pending RSS retirement".into());
                    }
                }
            }
            cycle.check(observer)?;
            let started_ns = observer.now_ns()?;
            let mut expected = vec![
                supervisor_identity("controller", self.resources.run.controller_pid),
                supervisor_identity("worker", self.resources.run.worker_pid),
            ];
            for process in &mut self.processes {
                if !cycle
                    .owners
                    .iter()
                    .any(|owner| owner.identity == process.identity())
                {
                    observer.poll(process)?;
                }
                if process.terminal
                    && (!process.receipt.reaped
                        || !process.receipt.success
                        || process.receipt.forced)
                {
                    cycle.disposition = if process.receipt.forced {
                        PendingDisposition::ForcedExit
                    } else {
                        PendingDisposition::FailedExit
                    };
                    return Err("unsuccessful nonpending owned RSS retirement".into());
                }
                if !process.terminal {
                    expected.push(process.identity());
                }
            }
            if !self
                .processes
                .iter()
                .map(OwnedProcess::identity)
                .eq(retained_roster.iter().cloned())
            {
                cycle.disposition = PendingDisposition::OwnerChanged;
                return Err("RSS retained roster changed during capture".into());
            }
            if expected.len() > RESOURCE_MAX_IDENTITIES {
                return Err("excessive RSS roster".into());
            }
            let mut observations = Vec::with_capacity(expected.len());
            let mut known_total = 0u64;
            // Supervisors are freshly read on every pass, including final rebuild.
            for (position, site) in [RssSite::WorkerFrameController, RssSite::WorkerFrameWorker]
                .into_iter()
                .enumerate()
            {
                let identity = &expected[position];
                let value = match observer.measure(identity.clone(), position == 0) {
                    Ok(value) => value,
                    Err(error) => {
                        self.resources.remember_rss(
                            RssCandidate::from_identity(
                                identity,
                                site,
                                error.category,
                                RssPoll::NotApplicable,
                                error.observed_ns,
                                error.bytes,
                            )
                            .map(|candidate| candidate.with_diagnostic(error.diagnostic())),
                        );
                        return Err(error.message);
                    }
                };
                let now = observer.now_ns()?;
                let observation = &value.observation;
                if observation.identity != *identity
                    || observation.started_ns < started_ns
                    || observation.started_ns > observation.finished_ns
                    || observation.finished_ns > now
                {
                    return Err("invalid supervisor RSS acquisition".into());
                }
                if let Some(category) = value.category() {
                    self.resources.remember_rss(
                        RssCandidate::from_identity(
                            identity,
                            site,
                            category,
                            RssPoll::NotApplicable,
                            Some(observation.finished_ns),
                            observation.bytes,
                        )
                        .map(|candidate| candidate.with_diagnostic(value.diagnostic)),
                    );
                    return Err(observation
                        .missing
                        .clone()
                        .unwrap_or_else(|| "supervisor RSS cap exceeded".into()));
                }
                known_total = known_total
                    .checked_add(observation.bytes.ok_or(RSS_UNAVAILABLE)?)
                    .ok_or("RSS sum overflow")?;
                if known_total >= RSS_CAP {
                    return Err("observed RSS aggregate cap exceeded".into());
                }
                observations.push(value.observation);
                cycle.check(observer)?;
            }
            let mut rebuild = false;
            for index in 0..self.processes.len() {
                if self.processes[index].terminal
                    || cycle
                        .owners
                        .iter()
                        .any(|owner| owner.identity == self.processes[index].identity())
                {
                    continue;
                }
                let identity = self.processes[index].identity();
                let site = match direct {
                    Some((target, site)) if target == &identity => *site,
                    Some(_) => RssSite::StopSibling,
                    None if self.processes[index].stop_requested => {
                        RssSite::WorkerFrameStopRequestedChild
                    }
                    None => RssSite::WorkerFrameChild,
                };
                let acquisition = observer.measure(identity.clone(), false);
                let now = observer.now_ns()?;
                let (category, diagnostic, observed_ns, bytes, message) = match &acquisition {
                    Ok(value) => {
                        let observation = &value.observation;
                        if observation.identity != identity
                            || observation.started_ns < started_ns
                            || observation.started_ns > observation.finished_ns
                            || observation.finished_ns > now
                        {
                            return Err("invalid owned RSS acquisition".into());
                        }
                        (
                            value.category(),
                            value.diagnostic,
                            Some(observation.finished_ns),
                            observation.bytes,
                            observation
                                .missing
                                .clone()
                                .unwrap_or_else(|| RSS_UNAVAILABLE.into()),
                        )
                    }
                    Err(error) => (
                        Some(error.category),
                        error.diagnostic(),
                        error.observed_ns,
                        error.bytes,
                        error.message.clone(),
                    ),
                };
                let diagnostic =
                    diagnostic.with_owned_stop(self.processes[index].successful_sigint_observed_ns);
                if let Some(category) = category {
                    let mut candidate = RssCandidate::from_identity(
                        &identity,
                        site,
                        category,
                        RssPoll::NotAttempted,
                        observed_ns,
                        bytes,
                    )
                    .map(|candidate| candidate.with_diagnostic(diagnostic));
                    let may_retire = direct.is_none_or(|(target, _)| target == &identity);
                    if bytes.is_none()
                        && category.permits_retirement()
                        && may_retire
                        && (acquisition.is_ok() || self.processes[index].stop_budget.is_some())
                    {
                        if let Err(error) = observer.poll(&mut self.processes[index]) {
                            if let Some(value) = &mut candidate {
                                value.category = RssCategory::RetirementPollFailed;
                                value.poll = RssPoll::PollError;
                            }
                            self.resources.remember_rss(candidate);
                            cycle.disposition = PendingDisposition::PollFailed;
                            return Err(error);
                        }
                        // Preserve the existing immediate actual-reap rebuild. A running
                        // owner needs the additional narrow proof below before any wait.
                        if self.processes[index].terminal {
                            if self.processes[index].receipt.forced
                                || !self.processes[index].receipt.success
                                || !self.processes[index].receipt.reaped
                            {
                                self.resources.remember_rss(candidate);
                                return Err("unsuccessful RSS retirement".into());
                            }
                            rebuild = true;
                            continue;
                        }
                        if let Some(value) = &mut candidate {
                            value.poll = RssPoll::Running;
                        }
                        if self.processes[index].receipt.forced {
                            // A concurrent owned force is never an eligible gap. Still
                            // collect an already available real exit without waiting or
                            // converting the forced status into successful retirement.
                            let polled = observer.poll(&mut self.processes[index]);
                            self.resources.remember_rss(candidate);
                            self.resources.remember_reap(&self.processes[index]);
                            cycle.disposition = PendingDisposition::ForcedExit;
                            polled?;
                            return Err("forced RSS retirement".into());
                        }
                    }
                    self.resources.remember_rss(candidate);
                    if let Some(bytes) = bytes {
                        // Real numeric observations still update the retained receipt,
                        // including a cap-crossing observation; no missing sample does.
                        self.processes[index]
                            .record_rss(bytes)
                            .map_err(|error| error.message)?;
                    }
                    let process = &self.processes[index];
                    let facts = diagnostic.taskinfo;
                    let allowed_scope = direct.is_none_or(|(target, _)| target == &identity);
                    let eligible = allowed_scope && category == RssCategory::TaskinfoUnavailable && bytes.is_none()
                        && process.stop_requested && !process.receipt.forced && !process.terminal
                        && process.receipt.pid == identity.pid && process.identity() == identity
                        && candidate.is_some_and(|value| value.role == RssRole::Server)
                        && process.stop_budget.is_some()
                        && facts.is_some_and(|facts| {
                            facts.proc_pidinfo_return_bytes == 0
                                && facts.proc_pidinfo_errno_after_call == libc::ESRCH
                                && observer.taskinfo_bytes() == Some(facts.proc_pidinfo_expected_bytes)
                                && facts.proc_pidinfo_expected_bytes != 0
                                && matches!((process.successful_sigint_observed_ns, facts.sample_started_ns, facts.sample_finished_ns),
                                    (Some(stop), Some(start), Some(finish)) if stop <= start && started_ns <= start && start <= finish && finish <= now)
                                && match &acquisition {
                                    Ok(value) => matches!((facts.sample_started_ns, facts.sample_finished_ns),
                                        (Some(start), Some(finish)) if value.observation.started_ns <= start
                                            && finish <= value.observation.finished_ns),
                                    Err(_) => true,
                                }
                        });
                    if !eligible {
                        return Err(message);
                    }
                    if committed.is_none() {
                        let frame: ResourceFrame = resource_read(&self.root.join("resource.json"))?;
                        let checked_now = observer.now_ns()?;
                        frame.validate(
                            &self.resources.run,
                            self.resources.published_sequence,
                            checked_now,
                        )?;
                        if frame.sequence != self.resources.published_sequence
                            || frame.started_ns != self.resources.published_started_ns
                            || frame.finished_ns != self.resources.published_ns
                        {
                            return Err("pending RSS authority is not the committed frame".into());
                        }
                        committed = Some(frame);
                    }
                    let frame = committed.as_ref().ok_or("RSS committed frame missing")?;
                    // Compare the entire retained roster. Only exact identities of
                    // actually reaped retained owners may be filtered from the old
                    // publication; ghosts, omissions and foreign generations fail.
                    let retired = self
                        .processes
                        .iter()
                        .filter(|process| process.terminal && process.receipt.reaped)
                        .map(OwnedProcess::identity)
                        .collect::<std::collections::BTreeSet<_>>();
                    let published_live = frame
                        .expected
                        .iter()
                        .filter(|member| !retired.contains(*member))
                        .cloned()
                        .collect::<std::collections::BTreeSet<_>>();
                    let current_live = std::iter::once(supervisor_identity(
                        "controller",
                        self.resources.run.controller_pid,
                    ))
                    .chain(std::iter::once(supervisor_identity(
                        "worker",
                        self.resources.run.worker_pid,
                    )))
                    .chain(
                        self.processes
                            .iter()
                            .filter(|process| !process.terminal)
                            .map(OwnedProcess::identity),
                    )
                    .collect::<std::collections::BTreeSet<_>>();
                    if published_live != current_live || !published_live.contains(&identity) {
                        return Err(
                            "pending RSS authority does not bind the complete retained roster"
                                .into(),
                        );
                    }
                    cycle.tighten_common(
                        frame
                            .started_ns
                            .checked_add(RESOURCE_FRESH_NS)
                            .ok_or("RSS freshness overflow")?,
                        observer,
                    )?;
                    cycle.tighten_owner(process.stop_budget.ok_or(RSS_UNAVAILABLE)?, observer)?;
                    cycle.check(observer)?;
                    let eligible_ns = observer.now_ns()?;
                    if self.resources.first_pending_retirement.is_none() {
                        let candidate = candidate.ok_or("RSS candidate missing")?;
                        self.resources.first_pending_retirement = Some(PendingRetirementEvidence {
                            schema: "mount-rs.cache-rss-pending-retirement.v1",
                            resource_sequence: self.resources.published_sequence,
                            candidate_role: candidate.role,
                            candidate_pid: candidate.pid,
                            candidate_generation: candidate.generation,
                            candidate_node: candidate.node,
                            eligible_ns,
                            deadline_ns: cycle.deadline_ns,
                            settled_ns: None,
                            disposition: PendingDisposition::OtherFailure,
                        });
                        cycle.evidence_ns = Some(eligible_ns);
                    }
                    // Bounded by canonical server nodes and the validated exact roster.
                    if cycle.owners.len() >= 10 {
                        return Err("excessive pending RSS owners".into());
                    }
                    self.processes[index].retirement = Some(RetainedRetirement {
                        identity: identity.clone(),
                        capture_started_ns: cycle.captured_ns,
                        deadline_ns: cycle.deadline_ns,
                        deadline: cycle.deadline,
                        cleanup_ns: cycle.cleanup_ns,
                        cleanup_deadline: cycle.cleanup_deadline,
                        eligible_ns,
                        closed: None,
                    });
                    cycle.owners.push(PendingOwner { identity });
                    self.retain_cycle_fences(cycle);
                    continue;
                }
                let value = acquisition.map_err(|error| error.message)?;
                let bytes = value.observation.bytes.ok_or(RSS_UNAVAILABLE)?;
                self.processes[index]
                    .record_rss(bytes)
                    .map_err(|error| error.message)?;
                known_total = known_total.checked_add(bytes).ok_or("RSS sum overflow")?;
                if known_total >= RSS_CAP {
                    return Err("observed RSS aggregate cap exceeded".into());
                }
                observations.push(value.observation);
                cycle.check(observer)?;
            }
            cycle.check(observer)?;
            if rebuild {
                continue;
            }
            if self
                .processes
                .iter()
                .any(|process| !process.terminal && process.retirement.is_some())
            {
                // The known subtotal is enforced but never published as a complete
                // aggregate. Missing RSS is not a zero or a receipt sample.
                observer.wait(cycle.deadline)?;
                continue;
            }
            let finished_ns = observer.now_ns()?;
            let totals = rss_totals(&expected, &observations)?;
            self.resources.max_total = self.resources.max_total.max(totals.total);
            self.resources.sequence = self
                .resources
                .sequence
                .checked_add(1)
                .ok_or("resource sequence overflow")?;
            let roster_changed = self
                .resources
                .last
                .as_ref()
                .is_none_or(|value| value.expected != expected);
            let mut frame = ResourceFrame {
                schema: 1,
                run: self.resources.run.clone(),
                sequence: self.resources.sequence,
                started_ns,
                finished_ns,
                expected,
                observations,
                total_bytes: Some(totals.total),
                child_bytes: Some(totals.children),
                max_total_bytes: self.resources.max_total,
                error: None,
                terminal,
            };
            // Validate the complete numeric/identity/time envelope independently of
            // the sticky operational error that a cleanup frame must still retain.
            frame.validate(&self.resources.run, frame.sequence, observer.now_ns()?)?;
            frame.error = self.resources.failure.clone();
            let should_publish = force
                || !cycle.owners.is_empty()
                || roster_changed
                || frame.error.is_some()
                || finished_ns.saturating_sub(self.resources.published_ns) >= POLL_MS * 1_000_000;
            cycle.check(observer)?;
            if should_publish {
                resource_write(&self.root, "resource.json", &frame)?;
                self.resources.published_ns = finished_ns;
                self.resources.published_started_ns = started_ns;
                self.resources.published_sequence = frame.sequence;
                // A late completed write is still a failed cycle, never qualification.
                cycle.check(observer)?;
            }
            self.resources.last = Some(frame);
            cycle.disposition = PendingDisposition::ConfirmedReap;
            return self.resources.failure.clone().map_or(Ok(()), Err);
        }
    }
    fn sample_process_with(
        &mut self,
        index: usize,
        site: RssSite,
        allow_retirement: bool,
        sample: &mut impl FnMut(u32) -> RssReadResult<u64>,
        poll: &mut impl FnMut(&mut Child) -> std::io::Result<Option<std::process::ExitStatus>>,
    ) -> Result<()> {
        if self.processes[index].retirement.is_some() {
            self.processes[index].poll_exit_with(poll)?;
            self.resources.remember_reap(&self.processes[index]);
            return Err(self
                .resources
                .failure
                .clone()
                .unwrap_or_else(|| RSS_UNAVAILABLE.into()));
        }
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
                self.resources.remember_rss(
                    RssCandidate::from_identity(
                        identity,
                        site,
                        error.category,
                        RssPoll::NotAttempted,
                        error.observed_ns,
                        error.bytes,
                    )
                    .map(|candidate| {
                        candidate.with_diagnostic(error.diagnostic().with_owned_stop(None))
                    }),
                );
                Err(error.message)
            }
            Ok(value) => {
                if !matches!(site, RssSite::WorkerFrameChild)
                    && let Some(category) = value.category()
                {
                    self.resources.remember_rss(
                        RssCandidate::from_identity(
                            identity,
                            site,
                            category,
                            RssPoll::NotApplicable,
                            Some(value.observation.finished_ns),
                            value.observation.bytes,
                        )
                        .map(|candidate| {
                            candidate.with_diagnostic(value.diagnostic.with_owned_stop(None))
                        }),
                    );
                }
                Ok(value)
            }
        }
    }
    fn sample_resources(&mut self, force: bool, terminal: bool) -> Result<()> {
        if self.has_retirement_quarantine() {
            return self.cleanup_retirement_with(force, terminal, &mut NativeStopObserver);
        }
        if self
            .processes
            .iter()
            .any(|process| process.stop_requested && process.stop_budget.is_some())
        {
            return self.observe_stopped_frame_with(force, terminal, &mut NativeStopObserver);
        }
        self.sample_resources_with(force, terminal, &mut measured)
    }
    fn sample_resources_with(
        &mut self,
        force: bool,
        terminal: bool,
        measure: &mut impl FnMut(RssIdentity, bool) -> RssReadResult<RssAcquisition>,
    ) -> Result<()> {
        if self.has_retirement_quarantine() {
            return self.cleanup_retirement_with(
                force,
                terminal,
                &mut MeasureStopObserver { measure },
            );
        }
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
                let site = if process.stop_requested {
                    RssSite::WorkerFrameStopRequestedChild
                } else {
                    RssSite::WorkerFrameChild
                };
                let acquisition = match measure(identity.clone(), false) {
                    Ok(value) => value,
                    Err(error) => {
                        self.resources.remember_rss(
                            RssCandidate::from_identity(
                                &identity,
                                site,
                                error.category,
                                RssPoll::NotAttempted,
                                error.observed_ns,
                                error.bytes,
                            )
                            .map(|candidate| {
                                candidate.with_diagnostic(
                                    error
                                        .diagnostic()
                                        .with_owned_stop(process.successful_sigint_observed_ns),
                                )
                            }),
                        );
                        return Err(error.message);
                    }
                };
                let category = acquisition.category();
                let diagnostic = acquisition
                    .diagnostic
                    .with_owned_stop(process.successful_sigint_observed_ns);
                let observation = acquisition.observation;
                let mut retirement = RssPoll::NotAttempted;
                if observation.bytes.is_none()
                    && category.is_some_and(RssCategory::permits_retirement)
                {
                    if let Err(error) = process.poll_exit() {
                        self.resources.remember_rss(
                            RssCandidate::from_identity(
                                &identity,
                                site,
                                RssCategory::RetirementPollFailed,
                                RssPoll::PollError,
                                Some(observation.finished_ns),
                                None,
                            )
                            .map(|candidate| candidate.with_diagnostic(diagnostic)),
                        );
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
                    self.resources.remember_rss(
                        RssCandidate::from_identity(
                            &identity,
                            site,
                            category,
                            retirement,
                            Some(observation.finished_ns),
                            observation.bytes,
                        )
                        .map(|candidate| candidate.with_diagnostic(diagnostic)),
                    );
                }
                if let Some(bytes) = observation.bytes
                    && let Err(error) = process.record_rss(bytes)
                {
                    self.resources.remember_rss(
                        RssCandidate::from_identity(
                            &identity,
                            site,
                            error.category,
                            retirement,
                            Some(observation.finished_ns),
                            Some(bytes),
                        )
                        .map(|candidate| candidate.with_diagnostic(diagnostic)),
                    );
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
            self.resources.published_started_ns = started_ns;
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
        let _check = progress_trace::span(Label::FleetCheck);
        {
            let _sample = progress_trace::span(Label::FleetResourceSample);
            self.refresh_resources(false, false)?;
        }
        {
            let _logs = progress_trace::span(Label::FleetLogRead);
            all_log_bytes(&self.root)?;
        }
        let available = {
            let _disk = progress_trace::span(Label::FleetDiskStat);
            free_disk(&self.root)?
        };
        if available < FREE_DISK_FLOOR {
            return Err("free disk below 64 GiB floor".into());
        }
        for index in 0..self.processes.len() {
            let _child = progress_trace::span(Label::FleetChildSample);
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
        self.resources.remember_reap(&process);
        let evidence = process.finish(deadline, &mut || self.attestation_progress(deadline));
        if let Ok(Some(rows)) = &evidence {
            self.banks.push(json!({"node":process.receipt.node,"generation":process.receipt.generation,
                "pid":process.receipt.pid,"scope":"cumulative generation shutdown logical counters",
                "launch":process.receipt.launch,"maintenance_quiescence":"unavailable","rows":rows}));
        }
        if process.receipt.role == "server" {
            let projection = if evidence.is_ok() {
                process.object_store_projection.take().unwrap_or_else(
                    || json!({"status":"unavailable","reason":"disabled_or_missing"}),
                )
            } else {
                json!({"status":"unavailable","reason":"owner_not_qualified"})
            };
            self.object_store_observations.push(projection);
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
        let already_requested = self.processes[index].stop_requested;
        self.processes[index].signal(libc::SIGINT)?;
        let force_at = deadline
            .checked_sub(Duration::from_secs(FORCE_SECONDS))
            .ok_or("invalid stop deadline")?;
        self.processes[index]
            .bind_stop_budget(already_requested, StopBudget { deadline, force_at });
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
        let original_budget = StopBudget {
            deadline,
            force_at: deadline
                .checked_sub(Duration::from_secs(FORCE_SECONDS))
                .unwrap_or(deadline),
        };
        for process in &mut self.processes {
            if !process.terminal
                && let Err(error) = process.request_stop(original_budget)
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
    let (category, observed_ns, bytes, diagnostic) = match &capture {
        Ok(value) => (
            value.category(),
            Some(value.observation.finished_ns),
            value.observation.bytes,
            value.diagnostic,
        ),
        Err(error) => (
            Some(error.category),
            error.observed_ns,
            error.bytes,
            error.diagnostic(),
        ),
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
        .map(|candidate| candidate.with_diagnostic(diagnostic.with_owned_stop(None)))
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
enum OuterWorkerAcquisition {
    Observed(RssObservation),
    Exited,
}
fn outer_worker_acquire(
    run: &ResourceRun,
    sequence: u64,
    child: &Child,
    first: &mut Option<FirstRssFailure>,
    measure: &mut impl FnMut(RssIdentity, bool) -> RssReadResult<RssAcquisition>,
    poll: &mut impl FnMut(&Child) -> Result<bool>,
) -> Result<OuterWorkerAcquisition> {
    if child.id() != run.worker_pid || run.group != run.worker_pid {
        return Err("outer RSS worker ownership mismatch".into());
    }
    let identity = supervisor_identity("worker", run.worker_pid);
    let capture = measure(identity.clone(), false);
    let (mut category, observed_ns, bytes, diagnostic) = match &capture {
        Ok(value) => (
            value.category(),
            Some(value.observation.finished_ns),
            value.observation.bytes,
            value.diagnostic,
        ),
        Err(error) => (
            Some(error.category),
            error.observed_ns,
            error.bytes,
            error.diagnostic(),
        ),
    };
    let mut retirement = RssPoll::NotApplicable;
    let mut poll_error = None;
    if bytes.is_none() && category.is_some_and(RssCategory::permits_retirement) {
        match poll(child) {
            // WNOWAIT observes this retained Child without releasing its original group identity.
            // The supervisor's existing exit branch must still validate closure and actually reap.
            Ok(true) => return Ok(OuterWorkerAcquisition::Exited),
            Ok(false) => retirement = RssPoll::Running,
            Err(error) => {
                category = Some(RssCategory::RetirementPollFailed);
                retirement = RssPoll::PollError;
                poll_error = Some(error);
            }
        }
    }
    if first.is_none()
        && let Some(category) = category
        && let Some(candidate) = RssCandidate::from_identity(
            &identity,
            RssSite::OuterWorker,
            category,
            retirement,
            observed_ns,
            bytes,
        )
        .map(|candidate| candidate.with_diagnostic(diagnostic.with_owned_stop(None)))
    {
        *first = Some(FirstRssFailure::new(
            run,
            RssProducer::Controller,
            sequence,
            candidate,
        ));
    }
    if let Some(error) = poll_error {
        return Err(error);
    }
    capture
        .map(|value| OuterWorkerAcquisition::Observed(value.observation))
        .map_err(|error| error.message)
}
fn worker_cleanup_receipt_closed(root: &Path, run: &ResourceRun) -> bool {
    receipt_value(&root.join("receipt.json"))
        .map(|value| {
            value["owned_cleanup_closed"] == true
                && value["worker_pid"] == run.worker_pid
                && value["supervisor_pid"] == run.controller_pid
                && value["process_group"] == run.group
        })
        .unwrap_or(false)
}
fn outer_sample(
    root: &Path,
    run: &ResourceRun,
    sequence: u64,
    evidence: &mut OuterSampleEvidence,
    child: &Child,
) -> Result<Option<OuterResourceSample>> {
    outer_sample_with(
        root,
        run,
        sequence,
        evidence,
        child,
        &mut measured,
        &mut exited_without_reap,
    )
}
fn outer_sample_with(
    root: &Path,
    run: &ResourceRun,
    sequence: u64,
    evidence: &mut OuterSampleEvidence,
    child: &Child,
    measure: &mut impl FnMut(RssIdentity, bool) -> RssReadResult<RssAcquisition>,
    poll: &mut impl FnMut(&Child) -> Result<bool>,
) -> Result<Option<OuterResourceSample>> {
    let frame: ResourceFrame = resource_read(&root.join("resource.json"))?;
    let validation_now = evidence.validation_now()?;
    if let Err(error) = frame.validate(run, sequence, validation_now) {
        evidence.retain_frame_validation_failure(
            &frame,
            run,
            sequence,
            validation_now,
            FrameValidationSite::BeforeRssAcquisition,
        );
        return Err(error);
    }
    // Replace producer supervisor samples; only child observations/subtotal cross the IPC seam.
    let controller = outer_acquire(
        run,
        frame.sequence,
        supervisor_identity("controller", run.controller_pid),
        RssSite::OuterController,
        &mut evidence.first_rss_failure,
        measure,
    )?;
    // A later worker exit must not discard an already-fatal controller observation.
    rss_totals(
        std::slice::from_ref(&controller.identity),
        std::slice::from_ref(&controller),
    )?;
    let worker = match outer_worker_acquire(
        run,
        frame.sequence,
        child,
        &mut evidence.first_rss_failure,
        measure,
        poll,
    )? {
        OuterWorkerAcquisition::Observed(value) => value,
        OuterWorkerAcquisition::Exited => return Ok(None),
    };
    let supervisors = [controller, worker];
    let validation_now = evidence.validation_now()?;
    let (totals, observations) =
        match frame.recompose_observed(run, sequence, validation_now, supervisors) {
            Ok(value) => value,
            Err(error) => {
                evidence.retain_frame_validation_failure(
                    &frame,
                    run,
                    sequence,
                    validation_now,
                    FrameValidationSite::FinalRecompose,
                );
                return Err(error);
            }
        };
    let limit_error = rss_caps(&totals, &observations).err();
    Ok(Some(OuterResourceSample {
        frame,
        totals,
        observations,
        limit_error,
    }))
}

pub fn supervise() {
    supervise_selected(
        "native_worker",
        "SQLite public CLI debug local OIDC fixture",
    );
}

pub fn supervise_tidb_rustfs_cold() {
    supervise_selected(
        "native_tidb_rustfs_cold_worker",
        "TiDB/RustFS public CLI debug local OIDC cold retirement fixture",
    );
}

fn supervise_selected(worker_entry: &'static str, scope: &'static str) {
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
        .args(["--ignored", "--exact", worker_entry, "--nocapture"])
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
        "first_frame_validation_failure":null,
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
    let mut outer_evidence = OuterSampleEvidence::default();
    let mut first_rss_failure_retention = RssRetention::NotAttempted;
    let status = loop {
        if Instant::now() >= deadline {
            panic!("worker reap budget exhausted; retained incomplete evidence");
        }
        if exited_without_reap(&worker.child).expect("worker WNOWAIT poll") {
            let cleanup_receipt = worker_cleanup_receipt_closed(&root, &run);
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
            let had_rss_failure = outer_evidence.first_rss_failure.is_some();
            let had_frame_failure = outer_evidence.first_frame_validation_failure.is_some();
            let sampled = outer_sample(
                &root,
                &run,
                last_sequence,
                &mut outer_evidence,
                &worker.child,
            );
            if !had_rss_failure && outer_evidence.first_rss_failure.is_some() {
                initial["first_rss_failure"] =
                    serde_json::to_value(outer_evidence.first_rss_failure).unwrap();
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
            if !had_frame_failure && outer_evidence.first_frame_validation_failure.is_some() {
                initial["first_frame_validation_failure"] =
                    serde_json::to_value(outer_evidence.first_frame_validation_failure).unwrap();
                let _ = fs::write(
                    root.join("controller.json"),
                    serde_json::to_vec_pretty(&initial).unwrap(),
                );
            }
            match sampled {
                // Confirmed WNOWAIT exit returns to the existing closure/group/reap branch.
                Ok(None) => continue,
                Ok(Some(OuterResourceSample {
                    frame,
                    totals,
                    observations,
                    limit_error,
                })) => {
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
    let controller = json!({"schema":1,"scope":scope,"worker_entry":worker_entry,
        "worker_pid":group,"process_group":group,"supervisor_pid":std::process::id(),
        "test_binary_sha256":binary_hash,"cli_binary_sha256":cli_hash,
        "debug_assertions":cfg!(debug_assertions),"local_oidc_fixture":cfg!(feature="local-oidc-fixture"),
        "forced":forced,"worker_reaped":true,"worker_success":status.success(),
        "group_gone":group_gone,"unrelated_sentinel_retained":sentinel_owned,
        "worker_max_rss_bytes":worker_max_rss,"controller_max_rss_bytes":controller_max_rss,
        "owned_aggregate_max_observed_bytes":max_total,"resource_sample_count":sample_count,
        "resource_initial_observed":initial_observed,"resource_last_sequence":last_sequence,
        "resource_last":last_resource,"resource_clock":"CLOCK_MONOTONIC",
        "first_rss_failure":outer_evidence.first_rss_failure,"first_rss_failure_retention":first_rss_failure_retention,
        "first_frame_validation_failure":outer_evidence.first_frame_validation_failure,
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
        inert_process_script(root, &format!("printf ready; read release; exit {status}"))
    }

    fn inert_process_ignoring_sigint(
        root: &Path,
    ) -> (OwnedProcess, Option<std::process::ChildStdin>) {
        inert_process_script(root, "trap '' INT; printf ready; read release; exit 0")
    }

    fn inert_process_script(
        root: &Path,
        script: &str,
    ) -> (OwnedProcess, Option<std::process::ChildStdin>) {
        let mut child = Command::new("/bin/sh")
            .args(["-c", script])
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
                object_store_projection: None,
                quic: None,
                peer: None,
                cache: None,
                terminal: false,
                stop_requested: false,
                stop_budget: None,
                retirement: None,
                successful_sigint_observed_ns: None,
                actual_reap_observed_ns: None,
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

    fn with_reaped_inert_children(fleet: &mut Fleet, check: impl FnOnce(&mut Fleet)) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(fleet)));
        for process in &mut fleet.processes {
            cleanup_inert_child(process);
        }
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
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
                diagnostic: RssDiagnostic::default(),
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
            object_store_observations: Vec::new(),
            next_generation: 2,
            resources: ResourceMonitor {
                run: previous.run.clone(),
                outer_ns: now + RESOURCE_FRESH_NS,
                sequence: previous.sequence,
                max_total: previous.max_total_bytes,
                published_ns: now,
                published_started_ns: previous.started_ns,
                published_sequence: previous.sequence,
                first_rss_failure: None,
                first_rss_failure_retention: RssRetention::NotAttempted,
                first_rss_reap: None,
                first_pending_retirement: None,
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
            diagnostic: RssDiagnostic::default(),
        })
    }

    struct PendingFixtureTarget {
        input: Option<std::process::ChildStdin>,
        native_reads: u64,
        polls: u64,
        fault_after_wait: u64,
        release_after_first_poll: bool,
    }
    struct PendingFixtureObserver {
        anchor: Instant,
        anchor_ns: u64,
        elapsed_ns: u64,
        waits: u64,
        targets: std::collections::BTreeMap<u32, PendingFixtureTarget>,
        held_inputs: Vec<Option<std::process::ChildStdin>>,
        other_reads: u64,
        supervisor_reads: u64,
        fault: &'static str,
        root: PathBuf,
        original_frame: Vec<u8>,
        first_error: Option<Vec<u8>>,
        first_wait_limit: Option<Instant>,
    }
    impl StopObserver for PendingFixtureObserver {
        fn now_ns(&mut self) -> Result<u64> {
            if self.fault == "clock_after_pending" && self.waits > 0 {
                return Err("fixture pending clock failure".into());
            }
            self.anchor_ns
                .checked_add(self.elapsed_ns)
                .ok_or("fixture clock overflow".into())
        }
        fn now(&mut self) -> Instant {
            self.anchor + Duration::from_nanos(self.elapsed_ns)
        }
        fn taskinfo_bytes(&self) -> Option<u32> {
            (self.fault != "non_darwin").then_some(96) // Synthetic proof; production uses its actual Darwin size.
        }
        fn measure(&mut self, identity: RssIdentity, _: bool) -> RssReadResult<RssAcquisition> {
            if self.fault == "late_final" && self.targets.values().all(|target| target.polls >= 2) {
                self.elapsed_ns = POLL_MS * 1_000_000 + 1;
            }
            let stamp = self.now_ns().map_err(RssReadError::from)?;
            let mut value = fixture_observation(identity.clone(), false)?;
            value.observation.started_ns = stamp;
            value.observation.finished_ns = stamp;
            if let Some(target) = self.targets.get_mut(&identity.pid) {
                if self.waits >= target.fault_after_wait {
                    target.native_reads += 1;
                    assert_eq!(
                        target.native_reads, 1,
                        "pending target native RSS was reread"
                    );
                    let mut error = diagnostic_unavailable(stamp, stamp);
                    if self.fault == "category" {
                        error.category = RssCategory::OtherRssError;
                    }
                    let facts = error.taskinfo.as_mut().unwrap();
                    match self.fault {
                        "errno" => facts.proc_pidinfo_errno_after_call = libc::EIO,
                        "short" => facts.proc_pidinfo_return_bytes = 1,
                        "negative_count" => facts.proc_pidinfo_return_bytes = -1,
                        "size" => facts.proc_pidinfo_expected_bytes = 95,
                        "null_clock" => facts.sample_started_ns = None,
                        "null_finish" => facts.sample_finished_ns = None,
                        "before_stop" => facts.sample_started_ns = Some(0),
                        "reversed_clock" => facts.sample_started_ns = Some(stamp + 1),
                        "future_clock" => facts.sample_finished_ns = Some(stamp + 1),
                        "stale_direct_error" => {
                            // A typed native error occurred after the known stop,
                            // but before the current capture's fixed entry clock.
                            let stale = self.anchor_ns + 1_000_000;
                            facts.sample_started_ns = Some(stale);
                            facts.sample_finished_ns = Some(stale);
                        }
                        _ => {}
                    }
                    if self.fault == "no_taskinfo" {
                        error.taskinfo = None;
                    }
                    if matches!(self.fault, "direct_error" | "stale_direct_error") {
                        return Err(error);
                    }
                    value.category = Some(error.category);
                    value.diagnostic = error.diagnostic();
                    value.observation.bytes = None;
                    value.observation.missing = Some(error.message);
                    match self.fault {
                        "acquisition_foreign" => value.observation.identity.generation += 1,
                        "acquisition_reversed" => value.observation.started_ns = stamp + 1,
                        "acquisition_future" => value.observation.finished_ns = stamp + 1,
                        _ => {}
                    }
                }
                return Ok(value);
            }
            if matches!(identity.role.as_str(), "controller" | "worker") {
                self.supervisor_reads += 1;
            } else {
                self.other_reads += 1;
            }
            if self.waits > 0 {
                match (self.fault, identity.role.as_str()) {
                    ("controller", "controller") | ("worker", "worker") => {
                        return Err(RssReadError::new(
                            RssCategory::ProcStatusReadFailed,
                            "fixture supervisor source failure",
                        ));
                    }
                    ("controller_cap", "controller") | ("sibling_cap", "server") => {
                        value.observation.bytes = Some(RSS_CAP);
                    }
                    ("aggregate_cap", "controller" | "worker") => {
                        value.observation.bytes = Some(RSS_CAP / 2);
                    }
                    ("sibling_missing", "server") => {
                        let error = diagnostic_unavailable(stamp, stamp);
                        value.category = Some(error.category);
                        value.diagnostic = error.diagnostic();
                        value.observation.bytes = None;
                        value.observation.missing = Some(error.message);
                    }
                    _ => {}
                }
            }
            Ok(value)
        }
        fn poll(&mut self, process: &mut OwnedProcess) -> Result<()> {
            if (self.fault == "poll_error"
                || (self.fault == "poll_after_pending" && self.waits > 0))
                && self
                    .targets
                    .get(&process.child.id())
                    .is_some_and(|target| target.native_reads > 0)
            {
                return Err("fixture retained Child poll failure".into());
            }
            let was_terminal = process.terminal;
            if !was_terminal
                && self.waits > 0
                && !self.targets.contains_key(&process.child.id())
                && matches!(self.fault, "sibling_exit" | "sibling_forced_exit")
            {
                if self.fault == "sibling_forced_exit" {
                    process.receipt.forced = true;
                    process.child.kill().map_err(|error| error.to_string())?;
                }
                // WNOWAIT observes the real status without consuming it. The
                // same retained Child below establishes actual reaping before
                // the resolver can remove this sibling from its live roster.
                release_inert_child(self.held_inputs.last_mut().unwrap(), process.child.id())?;
            }
            process.poll_exit()?; // Actual retained Child::try_wait; no constructed ExitStatus.
            if !was_terminal
                && let Some(target) = self.targets.get_mut(&process.child.id())
                && target.native_reads > 0
            {
                target.polls += 1;
                if self.fault == "owner_changed" && self.waits > 0 {
                    process.receipt.generation += 1;
                }
                if target.polls == 1 && target.release_after_first_poll && !process.terminal {
                    if self.fault == "forced_exit" {
                        process.receipt.forced = true;
                        process.child.kill().map_err(|error| error.to_string())?;
                    }
                    // The first poll genuinely observed running. Release afterward and
                    // let only a later actual try_wait establish and record reaping.
                    release_inert_child(&mut target.input, process.child.id())?;
                }
            }
            Ok(())
        }
        fn wait(&mut self, deadline: Instant) -> Result<()> {
            assert!(self.now() < deadline, "waiting after original bound");
            if let Some(first) = self.first_wait_limit {
                assert!(
                    deadline <= first,
                    "pending observation allowance was extended"
                );
            } else {
                self.first_wait_limit = Some(deadline);
            }
            assert_eq!(
                fs::read(self.root.join("resource.json")).unwrap(),
                self.original_frame,
                "an incomplete pending frame was published"
            );
            let error = fs::read(self.root.join("rss-first-failure.json")).unwrap();
            if let Some(first) = &self.first_error {
                assert_eq!(&error, first, "immutable first RSS error was rewritten");
            } else {
                self.first_error = Some(error);
            }
            self.waits += 1;
            self.elapsed_ns += 1_000_000;
            Ok(())
        }
        fn progress(&mut self, _: &Path) -> Result<()> {
            if self.fault == "progress" && self.waits > 0 {
                Err("fixture disk/output progress failure".into())
            } else {
                Ok(())
            }
        }
    }
    fn pending_fixture(
        root: &Path,
        exit_status: u8,
        sibling: bool,
    ) -> (Fleet, PendingFixtureObserver) {
        pending_fixture_with_sibling_status(root, exit_status, sibling.then_some(0))
    }
    fn pending_fixture_with_sibling_status(
        root: &Path,
        exit_status: u8,
        sibling_status: Option<u8>,
    ) -> (Fleet, PendingFixtureObserver) {
        let script = format!("trap '' INT; printf ready; read release; exit {exit_status}");
        let (mut process, input) = inert_process_script(root, &script);
        process.receipt.node = "node-0".into();
        let target_pid = process.child.id();
        let mut fleet = inert_resource_fleet(root, process);
        let mut held_inputs = Vec::new();
        if let Some(status) = sibling_status {
            let script = format!("trap '' INT; printf ready; read release; exit {status}");
            let (mut process, input) = inert_process_script(root, &script);
            process.receipt.node = "node-1".into();
            fleet.processes.push(process);
            held_inputs.push(input);
        }
        fleet.resources.outer_ns = monotonic_ns().unwrap() + 60_000_000_000;
        fleet
            .sample_resources_with(true, false, &mut |identity, _| {
                fixture_observation(identity, false)
            })
            .unwrap();
        // Fixture supplies the exact start of the frame that was actually written.
        fleet.resources.published_started_ns = fleet.resources.last.as_ref().unwrap().started_ns;
        fleet.processes[0].signal(libc::SIGINT).unwrap();
        let anchor = Instant::now();
        let anchor_ns = monotonic_ns().unwrap();
        // Freeze synthetic policy inputs after real signal/setup, before resolver entry.
        // Production retains its real original budget; metadata has its own RED control.
        let force_at = anchor + Duration::from_secs(1);
        fleet.processes[0].stop_budget = Some(StopBudget {
            deadline: force_at + Duration::from_secs(FORCE_SECONDS),
            force_at,
        });
        let targets = std::collections::BTreeMap::from([(
            target_pid,
            PendingFixtureTarget {
                input,
                native_reads: 0,
                polls: 0,
                fault_after_wait: 0,
                release_after_first_poll: true,
            },
        )]);
        let mut observer = PendingFixtureObserver {
            anchor,
            anchor_ns,
            elapsed_ns: 0,
            waits: 0,
            targets,
            held_inputs,
            other_reads: 0,
            supervisor_reads: 0,
            fault: "none",
            root: root.into(),
            original_frame: fs::read(root.join("resource.json")).unwrap(),
            first_error: None,
            first_wait_limit: None,
        };
        // Fixture RSS/clocks are synthetic; commit its fresh complete original
        // baseline only after real signal/setup. Nothing is renewed after entry.
        pending_prior_publication_remaining(&mut fleet, &mut observer, RESOURCE_FRESH_NS);
        (fleet, observer)
    }
    #[test]
    fn rss_pending_exit_direct_error_native_clock_must_be_inside_current_capture() {
        for stale in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
            with_reaped_inert_children(&mut fleet, |fleet| {
                // Synthetic diagnostic/policy clocks isolate the lower bound:
                // stop=anchor, native error=anchor+1ms, capture=anchor+2ms.
                // The actual successful owned signal and actual Child remain.
                fleet.processes[0].successful_sigint_observed_ns = Some(observer.anchor_ns);
                observer.elapsed_ns = 2_000_000;
                observer.fault = if stale {
                    "stale_direct_error"
                } else {
                    "direct_error"
                };
                let result =
                    fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer);
                if stale {
                    assert!(
                        result.is_err(),
                        "a pre-capture typed native error granted a fresh pending proof"
                    );
                    assert_eq!(observer.waits, 0);
                    assert_eq!(observer.targets.values().next().unwrap().native_reads, 1);
                    assert!(!fleet.processes[0].terminal);
                    assert!(fleet.resources.first_pending_retirement.is_none());
                    assert_eq!(
                        fs::read(observer.root.join("resource.json")).unwrap(),
                        observer.original_frame
                    );
                } else {
                    assert_eq!(result, Ok(()));
                    assert_pending_success(fleet, &mut observer);
                }
            });
        }
    }

    #[test]
    fn rss_pending_exit_nonpending_failed_or_forced_sibling_reap_remains_fatal() {
        for forced in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let sibling_status = if forced { 0 } else { 7 };
            let (mut fleet, mut observer) =
                pending_fixture_with_sibling_status(directory.path(), 0, Some(sibling_status));
            with_reaped_inert_children(&mut fleet, |fleet| {
                observer.fault = if forced {
                    "sibling_forced_exit"
                } else {
                    "sibling_exit"
                };
                let result =
                    fleet.observe_stopped_process_with(0, RssSite::StopTarget, &mut observer);
                assert!(
                    result.is_err(),
                    "a real failed/forced sibling disappeared from an accepted pending frame"
                );
                assert!(
                    observer.waits > 0,
                    "the sibling failed before the pending cycle began"
                );
                assert_eq!(observer.targets.values().next().unwrap().native_reads, 1);
                assert!(fleet.processes[1].terminal && fleet.processes[1].receipt.reaped);
                assert!(!fleet.processes[1].receipt.success);
                assert_eq!(fleet.processes[1].receipt.forced, forced);
                // The selected target really exited successfully too. Its
                // success must not erase the sibling's failed owned status.
                fleet.processes[0].poll_exit().unwrap();
                assert!(fleet.processes[0].terminal && fleet.processes[0].receipt.reaped);
                assert!(fleet.processes[0].receipt.success && !fleet.processes[0].receipt.forced);
                assert!(fleet.resources.failure.is_some());
                assert_eq!(
                    fs::read(observer.root.join("resource.json")).unwrap(),
                    observer.original_frame
                );
                assert_eq!(
                    fs::read(observer.root.join("rss-first-failure.json")).unwrap(),
                    observer.first_error.clone().unwrap()
                );
            });
        }
    }

    fn assert_pending_success(fleet: &Fleet, observer: &mut PendingFixtureObserver) {
        assert!(observer.waits > 0);
        assert!(
            observer.supervisor_reads >= 4,
            "supervisors were not freshly rebuilt"
        );
        for (pid, target) in &observer.targets {
            assert_eq!(target.native_reads, 1);
            assert!(target.polls >= 2);
            let process = fleet
                .processes
                .iter()
                .find(|process| process.child.id() == *pid)
                .unwrap();
            assert!(process.terminal && process.receipt.reaped && process.receipt.success);
            assert!(!process.receipt.forced);
            assert_eq!(
                process.receipt.rss_samples, 1,
                "missing RSS became a fabricated sample"
            );
        }
        let frame = fleet.resources.last.as_ref().unwrap();
        assert!(frame.error.is_none());
        assert!(
            frame
                .observations
                .iter()
                .all(|value| value.bytes.is_some() && value.missing.is_none())
        );
        assert!(
            frame
                .expected
                .iter()
                .all(|identity| !observer.targets.contains_key(&identity.pid))
        );
        frame
            .validate(
                &fleet.resources.run,
                frame.sequence,
                observer.now_ns().unwrap(),
            )
            .unwrap();
        assert_eq!(fleet.resources.published_started_ns, frame.started_ns);
        assert_eq!(
            fs::read(observer.root.join("rss-first-failure.json")).unwrap(),
            observer.first_error.clone().unwrap()
        );
        let evidence = fleet.resource_evidence();
        assert_eq!(
            evidence["first_rss_failure"]["schema"],
            "mount-rs.cache-rss-failure.v2"
        );
        assert_eq!(evidence["first_rss_failure"]["retirement_poll"], "running");
        assert_eq!(
            evidence["first_pending_retirement"]["disposition"],
            "confirmed_reap"
        );
        assert!(evidence["first_rss_reap"].is_object());
    }
    #[test]
    fn rss_pending_exit_owner_records_only_the_first_successful_original_stop_budget() {
        let directory = tempfile::tempdir().unwrap();
        let (process, _input) = inert_process_ignoring_sigint(directory.path());
        let mut fleet = inert_resource_fleet(directory.path(), process);
        with_reaped_inert_children(&mut fleet, |fleet| {
            let force_at = Instant::now() + Duration::from_secs(1);
            let original = StopBudget {
                deadline: force_at + Duration::from_secs(FORCE_SECONDS),
                force_at,
            };
            fleet.processes[0].request_stop(original).unwrap();
            assert_eq!(
                fleet.processes[0]
                    .stop_budget
                    .as_ref()
                    .map(|budget| budget.deadline),
                Some(original.deadline)
            );
            // Model unavailable first timestamp after the successful owned signal;
            // the once flag must prevent a later signal from filling it.
            fleet.processes[0].successful_sigint_observed_ns = None;
            let later = StopBudget {
                deadline: original.deadline + Duration::from_secs(1),
                force_at: original.force_at + Duration::from_secs(1),
            };
            fleet.processes[0].request_stop(later).unwrap();
            assert_eq!(
                fleet.processes[0]
                    .stop_budget
                    .as_ref()
                    .map(|budget| budget.deadline),
                Some(original.deadline)
            );
            assert!(fleet.processes[0].successful_sigint_observed_ns.is_none());
            let tighter = StopBudget {
                deadline: original.deadline - Duration::from_millis(1),
                force_at: original.force_at - Duration::from_millis(1),
            };
            fleet.processes[0].request_stop(tighter).unwrap();
            assert_eq!(
                fleet.processes[0]
                    .stop_budget
                    .as_ref()
                    .map(|budget| budget.deadline),
                Some(tighter.deadline)
            );
            assert_eq!(
                fleet.processes[0]
                    .stop_budget
                    .as_ref()
                    .map(|budget| budget.force_at),
                Some(tighter.force_at)
            );
            assert!(fleet.processes[0].successful_sigint_observed_ns.is_none());
        });
    }
    fn pending_direct_success(site: RssSite) {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
        with_reaped_inert_children(&mut fleet, |fleet| {
            let result = fleet.observe_stopped_process_with(0, site, &mut observer);
            assert_eq!(result, Ok(()));
            assert!(observer.other_reads >= 2);
            assert_pending_success(fleet, &mut observer);
        });
    }
    #[test]
    fn rss_pending_exit_stop_target_requires_actual_reap_and_fresh_complete_frame() {
        pending_direct_success(RssSite::StopTarget);
    }
    #[test]
    fn rss_pending_exit_cleanup_target_requires_actual_reap_and_fresh_complete_frame() {
        pending_direct_success(RssSite::CleanupChild);
    }
    #[test]
    fn rss_pending_exit_frame_requires_actual_reap_without_target_native_reread() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
        with_reaped_inert_children(&mut fleet, |fleet| {
            assert_eq!(
                fleet.observe_stopped_frame_with(true, false, &mut observer),
                Ok(())
            );
            assert_pending_success(fleet, &mut observer);
        });
    }
    #[test]
    fn rss_pending_exit_direct_typed_error_requires_actual_reap_and_complete_frame() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
        with_reaped_inert_children(&mut fleet, |fleet| {
            observer.fault = "direct_error";
            assert_eq!(
                fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer),
                Ok(())
            );
            assert_pending_success(fleet, &mut observer);
        });
    }
    #[test]
    fn rss_pending_exit_running_forever_exhausts_original_bound_without_reset() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
        with_reaped_inert_children(&mut fleet, |fleet| {
            observer
                .targets
                .values_mut()
                .next()
                .unwrap()
                .release_after_first_poll = false;
            let force_at = observer.anchor + Duration::from_millis(7);
            fleet.processes[0].stop_budget = Some(StopBudget {
                deadline: force_at + Duration::from_secs(FORCE_SECONDS),
                force_at,
            });
            let result = fleet.observe_stopped_process_with(0, RssSite::StopTarget, &mut observer);
            assert_eq!(result, Err(RSS_UNAVAILABLE.into()));
            assert!(!fleet.processes[0].terminal);
            assert_eq!(observer.targets.values().next().unwrap().native_reads, 1);
            assert!(observer.waits > 0 && observer.waits <= 7);
            assert_eq!(
                fs::read(observer.root.join("resource.json")).unwrap(),
                observer.original_frame
            );
            let evidence = fleet.resource_evidence();
            assert_eq!(
                evidence["first_pending_retirement"]["disposition"],
                "deadline_exhausted"
            );
            let limit = evidence["first_pending_retirement"]["deadline_ns"]
                .as_u64()
                .unwrap();
            assert!(limit <= observer.anchor_ns + 7_000_000);
            assert!(
                fleet.resources.stop_deadline_ns.unwrap()
                    <= observer.anchor_ns + SHUTDOWN_SECONDS * 1_000_000_000
            );
        });
    }
    #[test]
    fn rss_pending_exit_supervisor_sibling_caps_and_progress_failures_remain_fatal() {
        for fault in [
            "controller",
            "worker",
            "controller_cap",
            "sibling_cap",
            "aggregate_cap",
            "sibling_missing",
            "progress",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
            with_reaped_inert_children(&mut fleet, |fleet| {
                observer.fault = fault;
                observer
                    .targets
                    .values_mut()
                    .next()
                    .unwrap()
                    .release_after_first_poll = false;
                let result =
                    fleet.observe_stopped_process_with(0, RssSite::StopTarget, &mut observer);
                assert!(result.is_err(), "{fault}");
                assert!(observer.waits > 0, "{fault}");
                let required_supervisor_reads = match fault {
                    "progress" => 2,
                    "controller" | "controller_cap" => 3,
                    _ => 4,
                };
                assert!(
                    observer.supervisor_reads >= required_supervisor_reads,
                    "{fault}"
                );
                if matches!(fault, "controller" | "worker") {
                    assert!(result.unwrap_err().contains("supervisor"));
                }
                assert!(!fleet.processes[0].terminal);
                assert_eq!(observer.targets.values().next().unwrap().native_reads, 1);
                assert_eq!(
                    fs::read(observer.root.join("resource.json")).unwrap(),
                    observer.original_frame
                );
                assert_eq!(
                    fs::read(observer.root.join("rss-first-failure.json")).unwrap(),
                    observer.first_error.clone().unwrap()
                );
            });
        }
    }
    #[test]
    fn rss_pending_exit_unsuccessful_and_forced_actual_exits_remain_fatal() {
        for (status, fault) in [(7, "none"), (0, "forced_exit")] {
            let directory = tempfile::tempdir().unwrap();
            let (mut fleet, mut observer) = pending_fixture(directory.path(), status, false);
            with_reaped_inert_children(&mut fleet, |fleet| {
                observer.fault = fault;
                let result =
                    fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer);
                assert!(result.is_err());
                assert!(fleet.processes[0].terminal && fleet.processes[0].receipt.reaped);
                assert!(!fleet.processes[0].receipt.success);
                assert_eq!(fleet.processes[0].receipt.forced, fault == "forced_exit");
                assert_eq!(observer.targets.values().next().unwrap().native_reads, 1);
                assert_eq!(
                    fs::read(observer.root.join("resource.json")).unwrap(),
                    observer.original_frame
                );
            });
        }
    }
    #[test]
    fn rss_pending_exit_wrong_native_proofs_and_disallowed_scopes_remain_fatal() {
        for fault in [
            "category",
            "errno",
            "short",
            "negative_count",
            "size",
            "null_clock",
            "null_finish",
            "before_stop",
            "reversed_clock",
            "future_clock",
            "no_taskinfo",
            "non_darwin",
            "acquisition_foreign",
            "acquisition_reversed",
            "acquisition_future",
            "no_budget",
            "null_stop",
            "no_stop",
            "forced_owner",
            "wrong_pid",
            "wrong_node",
            "wrong_generation",
            "wrong_role",
            "unobserved_generation",
            "unobserved_node",
            "unobserved_run",
            "unpublished_generation",
            "unpublished_node",
            "unpublished_freshness",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
            with_reaped_inert_children(&mut fleet, |fleet| {
                observer.fault = fault;
                observer
                    .targets
                    .values_mut()
                    .next()
                    .unwrap()
                    .release_after_first_poll = false;
                if fault == "no_budget" {
                    fleet.processes[0].stop_budget = None;
                }
                if fault == "null_stop" {
                    fleet.processes[0].successful_sigint_observed_ns = None;
                }
                if fault == "no_stop" {
                    fleet.processes[0].stop_requested = false;
                }
                if fault == "forced_owner" {
                    fleet.processes[0].receipt.forced = true;
                }
                if fault == "wrong_pid" {
                    fleet.processes[0].receipt.pid = u32::MAX;
                }
                if fault == "wrong_node" {
                    fleet.processes[0].receipt.node = "node-00".into();
                }
                if fault == "wrong_generation" {
                    fleet.processes[0].receipt.generation = 0;
                }
                if fault == "wrong_role" {
                    fleet.processes[0].receipt.role = "catalog-apply".into();
                }
                if fault == "unobserved_generation" {
                    fleet.processes[0].receipt.generation = 2;
                }
                if fault == "unobserved_node" {
                    fleet.processes[0].receipt.node = "node-9".into();
                }
                if fault == "unobserved_run" {
                    fleet.resources.run.worker_pid = u32::MAX;
                }
                if fault == "unpublished_generation" {
                    fleet.processes[0].receipt.generation = 2;
                }
                if fault == "unpublished_node" {
                    fleet.processes[0].receipt.node = "node-9".into();
                }
                if matches!(
                    fault,
                    "unpublished_generation" | "unpublished_node" | "unpublished_freshness"
                ) {
                    pending_make_unpublished_view(
                        fleet,
                        &mut observer,
                        fault == "unpublished_freshness",
                    );
                }
                assert!(
                    fleet
                        .observe_stopped_process_with(0, RssSite::StopTarget, &mut observer)
                        .is_err(),
                    "{fault}"
                );
                assert_eq!(observer.waits, 0, "{fault}");
                assert!(!fleet.processes[0].terminal);
                if matches!(
                    fault,
                    "unpublished_generation" | "unpublished_node" | "unpublished_freshness"
                ) {
                    assert_eq!(
                        fs::read(observer.root.join("resource.json")).unwrap(),
                        observer.original_frame,
                        "{fault}"
                    );
                    assert!(
                        fleet.resources.first_pending_retirement.is_none(),
                        "{fault}"
                    );
                }
            });
        }
        for site in [
            RssSite::ActiveChild,
            RssSite::StopSibling,
            RssSite::ApplySibling,
            RssSite::ReadinessChild,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
            with_reaped_inert_children(&mut fleet, |fleet| {
                observer
                    .targets
                    .values_mut()
                    .next()
                    .unwrap()
                    .release_after_first_poll = false;
                assert!(
                    fleet
                        .observe_stopped_process_with(0, site, &mut observer)
                        .is_err()
                );
                assert_eq!(observer.waits, 0);
                assert!(!fleet.processes[0].terminal);
            });
        }
        // The public frame entry point must use committed authority too. Passing
        // the direct-stop negatives alone must not leave this path uncovered.
        for fault in [
            "unpublished_generation",
            "unpublished_node",
            "unpublished_freshness",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
            with_reaped_inert_children(&mut fleet, |fleet| {
                observer.fault = fault;
                observer
                    .targets
                    .values_mut()
                    .next()
                    .unwrap()
                    .release_after_first_poll = false;
                if fault == "unpublished_generation" {
                    fleet.processes[0].receipt.generation = 2;
                }
                if fault == "unpublished_node" {
                    fleet.processes[0].receipt.node = "node-9".into();
                }
                pending_make_unpublished_view(
                    fleet,
                    &mut observer,
                    fault == "unpublished_freshness",
                );
                assert!(
                    fleet
                        .observe_stopped_frame_with(true, false, &mut observer)
                        .is_err(),
                    "{fault}"
                );
                assert_eq!(observer.waits, 0, "{fault}");
                assert!(!fleet.processes[0].terminal, "{fault}");
                assert!(
                    fleet.resources.first_pending_retirement.is_none(),
                    "{fault}"
                );
                assert_eq!(
                    fs::read(observer.root.join("resource.json")).unwrap(),
                    observer.original_frame,
                    "{fault}"
                );
            });
        }
    }
    #[test]
    fn rss_pending_exit_fatal_reentry_retains_quarantine_across_frame_and_direct_cleanup() {
        for (first_is_frame, forced) in [(false, false), (true, false), (false, true), (true, true)]
        {
            let directory = tempfile::tempdir().unwrap();
            let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
            with_reaped_inert_children(&mut fleet, |fleet| {
                observer.fault = "progress";
                observer
                    .targets
                    .values_mut()
                    .next()
                    .unwrap()
                    .release_after_first_poll = false;
                let first = if first_is_frame {
                    fleet.observe_stopped_frame_with(true, false, &mut observer)
                } else {
                    fleet.observe_stopped_process_with(0, RssSite::StopTarget, &mut observer)
                };
                let original_error = first.unwrap_err();
                assert!(original_error.contains("progress"));
                assert_eq!(fleet.resources.failure.as_ref(), Some(&original_error));
                assert!(!fleet.processes[0].terminal);
                assert!(observer.waits > 0);
                let first_waits = observer.waits;
                let first_limit = observer.first_wait_limit.unwrap();
                let first_settlement =
                    fleet.resource_evidence()["first_pending_retirement"].clone();
                let first_file = fs::read(observer.root.join("rss-first-failure.json")).unwrap();
                // Clear the transient source fault only. The real Fleet failure and
                // original pending owner/capture lifetime must stay authoritative.
                observer.fault = "none";
                for frame in [true, false, true] {
                    let prior_supervisors = observer.supervisor_reads;
                    let prior_siblings = observer.other_reads;
                    let result = if frame {
                        fleet.observe_stopped_frame_with(true, false, &mut observer)
                    } else {
                        fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer)
                    };
                    assert_eq!(result, Err(original_error.clone()));
                    assert_eq!(
                        observer.targets.values().next().unwrap().native_reads,
                        1,
                        "a fatal return discarded exact-child native RSS quarantine"
                    );
                    assert_eq!(
                        observer.waits, first_waits,
                        "a closed fatal observation acquired another pending allowance"
                    );
                    assert_eq!(observer.first_wait_limit, Some(first_limit));
                    assert!(
                        observer.supervisor_reads >= prior_supervisors + 2,
                        "remaining supervisors must still be sampled during owned cleanup"
                    );
                    assert!(
                        observer.other_reads > prior_siblings,
                        "remaining live siblings must still be sampled during owned cleanup"
                    );
                    assert!(!fleet.processes[0].terminal);
                    assert_eq!(
                        fs::read(observer.root.join("resource.json")).unwrap(),
                        observer.original_frame
                    );
                    assert_eq!(
                        fs::read(observer.root.join("rss-first-failure.json")).unwrap(),
                        first_file
                    );
                    assert_eq!(
                        fleet.resource_evidence()["first_pending_retirement"],
                        first_settlement
                    );
                }
                // A later actual reap closes only the owned process. It must not
                // retroactively turn the failed original observation into success.
                let pid = fleet.processes[0].child.id();
                if forced {
                    fleet.processes[0].receipt.forced = true;
                    fleet.processes[0].child.kill().unwrap();
                }
                release_inert_child(&mut observer.targets.get_mut(&pid).unwrap().input, pid)
                    .unwrap();
                let result =
                    fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer);
                assert_eq!(result, Err(original_error.clone()));
                assert_eq!(observer.targets.get(&pid).unwrap().native_reads, 1);
                assert!(fleet.processes[0].terminal && fleet.processes[0].receipt.reaped);
                assert_eq!(fleet.processes[0].receipt.success, !forced);
                assert_eq!(fleet.processes[0].receipt.forced, forced);
                assert_eq!(
                    fleet.resource_evidence()["first_pending_retirement"],
                    first_settlement
                );
                assert_eq!(
                    fs::read(observer.root.join("rss-first-failure.json")).unwrap(),
                    first_file
                );
                assert_eq!(fleet.resources.failure.as_ref(), Some(&original_error));
                assert!(fleet.resources.first_rss_reap.is_some());
                // Ordinary complete cleanup publication after actual reaping is
                // allowed to retain the original failure, including a forced exit.
                assert_eq!(
                    fleet.observe_stopped_frame_with(true, false, &mut observer),
                    Err(original_error.clone())
                );
                assert_eq!(observer.targets.get(&pid).unwrap().native_reads, 1);
                assert_eq!(observer.waits, first_waits);
                assert_eq!(
                    fleet.resource_evidence()["first_pending_retirement"],
                    first_settlement
                );
                let complete: ResourceFrame =
                    resource_read(&observer.root.join("resource.json")).unwrap();
                assert!(!complete.expected.iter().any(|identity| identity.pid == pid));
                assert_eq!(complete.error.as_ref(), Some(&original_error));
                assert_eq!(complete.expected.len(), complete.observations.len());
                assert!(
                    complete
                        .observations
                        .iter()
                        .all(|sample| sample.bytes.is_some() && sample.missing.is_none())
                );
            });
        }
    }

    #[test]
    fn rss_pending_exit_expired_capture_reentry_cannot_reread_or_restart_observation() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
        with_reaped_inert_children(&mut fleet, |fleet| {
            observer
                .targets
                .values_mut()
                .next()
                .unwrap()
                .release_after_first_poll = false;
            let first = fleet.observe_stopped_frame_with(true, false, &mut observer);
            let original_error = first.unwrap_err();
            assert!(observer.waits > 0);
            assert_eq!(observer.elapsed_ns, POLL_MS * 1_000_000);
            let first_waits = observer.waits;
            let first_limit = observer.first_wait_limit.unwrap();
            let first_settlement = fleet.resource_evidence()["first_pending_retirement"].clone();
            assert_eq!(first_settlement["disposition"], "deadline_exhausted");
            let first_file = fs::read(observer.root.join("rss-first-failure.json")).unwrap();
            let cleanup_bound = fleet.resources.stop_deadline_ns;
            for frame in [false, true, false] {
                let result = if frame {
                    fleet.observe_stopped_frame_with(true, false, &mut observer)
                } else {
                    fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer)
                };
                assert_eq!(result, Err(original_error.clone()));
                assert_eq!(
                    observer.targets.values().next().unwrap().native_reads,
                    1,
                    "expired capture was rebuilt by rereading its missing target"
                );
                assert_eq!(observer.waits, first_waits);
                assert_eq!(observer.first_wait_limit, Some(first_limit));
                assert_eq!(
                    fleet.resources.stop_deadline_ns, cleanup_bound,
                    "cleanup allowance was replaced after capture exhaustion"
                );
                assert_eq!(
                    fleet.resource_evidence()["first_pending_retirement"],
                    first_settlement
                );
                assert_eq!(
                    fs::read(observer.root.join("rss-first-failure.json")).unwrap(),
                    first_file
                );
                assert_eq!(
                    fs::read(observer.root.join("resource.json")).unwrap(),
                    observer.original_frame
                );
                assert!(!fleet.processes[0].terminal);
            }
        });
    }

    #[test]
    fn rss_pending_exit_committed_authority_requires_exact_retained_roster() {
        for frame_scope in [false, true] {
            for fault in [
                "omitted_sibling",
                "extra_identity",
                "foreign_retained_generation",
            ] {
                let directory = tempfile::tempdir().unwrap();
                let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
                with_reaped_inert_children(&mut fleet, |fleet| {
                    observer
                        .targets
                        .values_mut()
                        .next()
                        .unwrap()
                        .release_after_first_poll = false;
                    let target = fleet.processes[0].identity();
                    let sibling = fleet.processes[1].identity();
                    let mut committed: ResourceFrame =
                        resource_read(&observer.root.join("resource.json")).unwrap();
                    match fault {
                        "omitted_sibling" => {
                            committed.expected.retain(|identity| identity != &sibling);
                            committed
                                .observations
                                .retain(|sample| sample.identity != sibling);
                        }
                        "extra_identity" => {
                            let foreign = RssIdentity {
                                role: "server".into(),
                                node: "node-9".into(),
                                pid: u32::MAX,
                                generation: 999,
                            };
                            assert!(
                                !committed
                                    .expected
                                    .iter()
                                    .any(|identity| identity.pid == foreign.pid)
                            );
                            let mut sample = fixture_observation(foreign.clone(), false)
                                .unwrap()
                                .observation;
                            sample.started_ns = committed.started_ns;
                            sample.finished_ns = committed.finished_ns;
                            committed.expected.push(foreign);
                            committed.observations.push(sample);
                        }
                        "foreign_retained_generation" => fleet.processes[1].receipt.generation += 1,
                        _ => unreachable!(),
                    }
                    let totals = rss_totals(&committed.expected, &committed.observations).unwrap();
                    committed.total_bytes = Some(totals.total);
                    committed.child_bytes = Some(totals.children);
                    committed.max_total_bytes = committed.max_total_bytes.max(totals.total);
                    assert!(
                        committed.expected.contains(&target),
                        "negative must isolate full roster authority from contains(target)"
                    );
                    committed
                        .validate(&fleet.resources.run, committed.sequence, observer.anchor_ns)
                        .unwrap();
                    // This is an otherwise valid complete committed frame with the
                    // existing publication identity; only retained roster binding differs.
                    resource_write(&observer.root, "resource.json", &committed).unwrap();
                    fleet.resources.last = Some(committed);
                    observer.original_frame =
                        fs::read(observer.root.join("resource.json")).unwrap();
                    let result = if frame_scope {
                        fleet.observe_stopped_frame_with(true, false, &mut observer)
                    } else {
                        fleet.observe_stopped_process_with(0, RssSite::StopTarget, &mut observer)
                    };
                    assert!(result.is_err(), "{fault}");
                    assert_eq!(
                        observer.waits, 0,
                        "unbound committed roster authorized waiting: {fault}"
                    );
                    assert_eq!(
                        observer.targets.values().next().unwrap().native_reads,
                        1,
                        "{fault}"
                    );
                    assert!(!fleet.processes[0].terminal, "{fault}");
                    assert!(
                        fleet.resources.first_pending_retirement.is_none(),
                        "{fault}"
                    );
                    assert_eq!(
                        fs::read(observer.root.join("resource.json")).unwrap(),
                        observer.original_frame,
                        "{fault}"
                    );
                });
            }
        }
    }

    #[test]
    fn rss_pending_exit_compact_and_pretty_settlement_evidence_stays_bounded() {
        let record = PendingRetirementEvidence {
            schema: "mount-rs.cache-rss-pending-retirement.v1",
            resource_sequence: u64::MAX,
            candidate_role: RssRole::Server,
            candidate_pid: u32::MAX,
            candidate_generation: u64::MAX,
            candidate_node: Some(9),
            eligible_ns: u64::MAX,
            deadline_ns: u64::MAX,
            settled_ns: Some(u64::MAX),
            disposition: PendingDisposition::DeadlineExhausted,
        };
        for bytes in [
            serde_json::to_vec(&record).unwrap(),
            serde_json::to_vec_pretty(&record).unwrap(),
        ] {
            assert!(bytes.len() <= FIRST_RSS_CAP);
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["deadline_ns"], u64::MAX);
            assert_eq!(value["candidate_pid"], u32::MAX);
        }
    }
    #[test]
    fn rss_pending_exit_prior_bare_sigint_cannot_acquire_original_budget_later() {
        let directory = tempfile::tempdir().unwrap();
        let (process, _input) = inert_process_ignoring_sigint(directory.path());
        let mut fleet = inert_resource_fleet(directory.path(), process);
        with_reaped_inert_children(&mut fleet, |fleet| {
            fleet.processes[0].signal(libc::SIGINT).unwrap();
            assert!(fleet.processes[0].stop_budget.is_none());
            let force_at = Instant::now() + Duration::from_secs(1);
            fleet.processes[0]
                .request_stop(StopBudget {
                    deadline: force_at + Duration::from_secs(FORCE_SECONDS),
                    force_at,
                })
                .unwrap();
            assert!(fleet.processes[0].stop_budget.is_none());
        });
    }
    #[test]
    fn rss_pending_exit_terminal_owner_cannot_record_a_successful_stop_budget() {
        let directory = tempfile::tempdir().unwrap();
        let (process, mut input) = inert_process_ignoring_sigint(directory.path());
        let mut fleet = inert_resource_fleet(directory.path(), process);
        with_reaped_inert_children(&mut fleet, |fleet| {
            release_inert_child(&mut input, fleet.processes[0].child.id()).unwrap();
            let force_at = Instant::now() + Duration::from_secs(1);
            assert!(
                fleet.processes[0]
                    .request_stop(StopBudget {
                        deadline: force_at + Duration::from_secs(FORCE_SECONDS),
                        force_at
                    })
                    .is_err()
            );
            assert!(fleet.processes[0].terminal && fleet.processes[0].receipt.reaped);
            assert!(!fleet.processes[0].stop_requested);
            assert!(fleet.processes[0].successful_sigint_observed_ns.is_none());
            assert!(fleet.processes[0].stop_budget.is_none());
        });
    }
    fn pending_prior_publication_remaining(
        fleet: &mut Fleet,
        observer: &mut PendingFixtureObserver,
        remaining: u64,
    ) {
        let started_ns = observer
            .anchor_ns
            .checked_sub(RESOURCE_FRESH_NS - remaining)
            .unwrap();
        let frame = fleet.resources.last.as_mut().unwrap();
        frame.started_ns = started_ns;
        frame.finished_ns = started_ns;
        for sample in &mut frame.observations {
            sample.started_ns = started_ns;
            sample.finished_ns = started_ns;
        }
        frame
            .validate(&fleet.resources.run, frame.sequence, observer.anchor_ns)
            .unwrap();
        fleet.resources.published_started_ns = started_ns;
        fleet.resources.published_ns = started_ns;
        resource_write(&observer.root, "resource.json", frame).unwrap();
        observer.original_frame = fs::read(observer.root.join("resource.json")).unwrap();
    }
    fn pending_make_unpublished_view(
        fleet: &mut Fleet,
        observer: &mut PendingFixtureObserver,
        expired_prior: bool,
    ) {
        if expired_prior {
            let prior = fleet.resources.last.as_mut().unwrap();
            let started_ns = observer
                .anchor_ns
                .checked_sub(RESOURCE_FRESH_NS + 1)
                .unwrap();
            prior.started_ns = started_ns;
            prior.finished_ns = started_ns;
            for sample in &mut prior.observations {
                sample.started_ns = started_ns;
                sample.finished_ns = started_ns;
            }
            fleet.resources.published_started_ns = started_ns;
            fleet.resources.published_ns = started_ns;
            resource_write(&observer.root, "resource.json", prior).unwrap();
            observer.original_frame = fs::read(observer.root.join("resource.json")).unwrap();
        }
        let identity = fleet.processes[0].identity();
        let mut unpublished = fleet.resources.last.as_ref().unwrap().clone();
        unpublished.sequence += 1;
        unpublished.started_ns = observer.anchor_ns;
        unpublished.finished_ns = observer.anchor_ns;
        for member in &mut unpublished.expected {
            if member.role == "server" && member.pid == identity.pid {
                *member = identity.clone();
            }
        }
        for sample in &mut unpublished.observations {
            if sample.identity.role == "server" && sample.identity.pid == identity.pid {
                sample.identity = identity.clone();
            }
            sample.started_ns = observer.anchor_ns;
            sample.finished_ns = observer.anchor_ns;
        }
        unpublished
            .validate(
                &fleet.resources.run,
                unpublished.sequence,
                observer.anchor_ns,
            )
            .unwrap();
        fleet.resources.sequence = unpublished.sequence;
        fleet.resources.last = Some(unpublished);
        assert!(
            fleet.resources.last.as_ref().unwrap().sequence > fleet.resources.published_sequence
        );
        // The valid newer in-memory candidate was never committed to resource.json.
        assert_eq!(
            fs::read(observer.root.join("resource.json")).unwrap(),
            observer.original_frame
        );
    }
    #[test]
    fn rss_pending_exit_original_capture_freshness_outer_and_resource_force_bounds_do_not_reset() {
        for bound in ["capture", "freshness", "outer", "resource_force"] {
            let directory = tempfile::tempdir().unwrap();
            let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
            with_reaped_inert_children(&mut fleet, |fleet| {
                observer
                    .targets
                    .values_mut()
                    .next()
                    .unwrap()
                    .release_after_first_poll = false;
                let max_waits = if bound == "capture" { POLL_MS } else { 7 };
                let original_limit_ns = observer.anchor_ns + max_waits * 1_000_000;
                match bound {
                    "freshness" => {
                        pending_prior_publication_remaining(fleet, &mut observer, 7_000_000)
                    }
                    "outer" => fleet.resources.outer_ns = original_limit_ns,
                    "resource_force" => {
                        fleet.resources.stop_deadline_ns =
                            Some(original_limit_ns + FORCE_SECONDS * 1_000_000_000)
                    }
                    _ => {}
                }
                assert_eq!(
                    fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer),
                    Err(RSS_UNAVAILABLE.into()),
                    "{bound}"
                );
                assert!(observer.waits > 0 && observer.waits <= max_waits, "{bound}");
                assert_eq!(
                    observer.targets.values().next().unwrap().native_reads,
                    1,
                    "{bound}"
                );
                assert!(!fleet.processes[0].terminal, "{bound}");
                assert_eq!(
                    fs::read(observer.root.join("resource.json")).unwrap(),
                    observer.original_frame,
                    "{bound}"
                );
                let settlement = fleet.resource_evidence();
                assert_eq!(
                    settlement["first_pending_retirement"]["disposition"], "deadline_exhausted",
                    "{bound}"
                );
                assert!(
                    settlement["first_pending_retirement"]["deadline_ns"]
                        .as_u64()
                        .unwrap()
                        <= original_limit_ns,
                    "{bound}"
                );
            });
        }
    }
    #[test]
    fn rss_pending_exit_expired_original_owner_budget_refuses_before_waiting() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
        with_reaped_inert_children(&mut fleet, |fleet| {
            let force_at = observer.anchor;
            fleet.processes[0].stop_budget = Some(StopBudget {
                deadline: force_at + Duration::from_secs(FORCE_SECONDS),
                force_at,
            });
            assert!(
                fleet
                    .observe_stopped_process_with(0, RssSite::StopTarget, &mut observer)
                    .is_err()
            );
            assert_eq!(observer.waits, 0);
            assert!(!fleet.processes[0].terminal);
            assert_eq!(
                fs::read(observer.root.join("resource.json")).unwrap(),
                observer.original_frame
            );
        });
    }
    #[test]
    fn rss_pending_exit_actual_successful_reap_cannot_publish_after_original_capture_bound() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
        with_reaped_inert_children(&mut fleet, |fleet| {
            observer.fault = "late_final";
            assert!(
                fleet
                    .observe_stopped_process_with(0, RssSite::StopTarget, &mut observer)
                    .is_err()
            );
            assert!(
                fleet.processes[0].terminal
                    && fleet.processes[0].receipt.reaped
                    && fleet.processes[0].receipt.success
            );
            assert!(!fleet.processes[0].receipt.forced);
            assert_eq!(observer.targets.values().next().unwrap().native_reads, 1);
            assert_eq!(
                fs::read(observer.root.join("resource.json")).unwrap(),
                observer.original_frame
            );
            assert_eq!(
                fleet.resource_evidence()["first_pending_retirement"]["disposition"],
                "deadline_exhausted"
            );
        });
    }
    #[test]
    fn rss_pending_exit_retained_child_poll_failures_remain_fatal_before_and_during_pending() {
        for fault in ["poll_error", "poll_after_pending"] {
            let directory = tempfile::tempdir().unwrap();
            let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
            with_reaped_inert_children(&mut fleet, |fleet| {
                observer.fault = fault;
                observer
                    .targets
                    .values_mut()
                    .next()
                    .unwrap()
                    .release_after_first_poll = false;
                assert_eq!(
                    fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer),
                    Err("fixture retained Child poll failure".into())
                );
                assert_eq!(observer.waits > 0, fault == "poll_after_pending");
                assert!(!fleet.processes[0].terminal);
                assert_eq!(observer.targets.values().next().unwrap().native_reads, 1);
                let evidence = fleet.resource_evidence();
                if fault == "poll_error" {
                    assert_eq!(
                        evidence["first_rss_failure"]["category"],
                        "retirement_poll_failed"
                    );
                    assert_eq!(
                        evidence["first_rss_failure"]["retirement_poll"],
                        "poll_error"
                    );
                } else {
                    assert_eq!(
                        evidence["first_rss_failure"]["category"],
                        "taskinfo_unavailable"
                    );
                    assert_eq!(evidence["first_rss_failure"]["retirement_poll"], "running");
                    assert_eq!(
                        evidence["first_pending_retirement"]["disposition"],
                        "poll_failed"
                    );
                    assert_eq!(
                        fs::read(observer.root.join("rss-first-failure.json")).unwrap(),
                        observer.first_error.clone().unwrap()
                    );
                }
                assert_eq!(
                    fs::read(observer.root.join("resource.json")).unwrap(),
                    observer.original_frame
                );
            });
        }
    }
    #[test]
    fn rss_pending_exit_common_clock_failure_during_pending_remains_fatal() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
        with_reaped_inert_children(&mut fleet, |fleet| {
            observer.fault = "clock_after_pending";
            observer
                .targets
                .values_mut()
                .next()
                .unwrap()
                .release_after_first_poll = false;
            assert_eq!(
                fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer),
                Err("fixture pending clock failure".into())
            );
            assert!(observer.waits > 0);
            assert!(!fleet.processes[0].terminal);
            assert_eq!(observer.targets.values().next().unwrap().native_reads, 1);
            assert_eq!(
                fs::read(observer.root.join("resource.json")).unwrap(),
                observer.original_frame
            );
            assert_eq!(
                fs::read(observer.root.join("rss-first-failure.json")).unwrap(),
                observer.first_error.clone().unwrap()
            );
            let evidence = fleet.resource_evidence();
            assert_eq!(
                evidence["first_pending_retirement"]["disposition"],
                "other_failure"
            );
            assert!(evidence["first_pending_retirement"]["settled_ns"].is_null());
        });
    }
    #[test]
    fn rss_pending_exit_retained_owner_identity_cannot_change_during_pending() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
        with_reaped_inert_children(&mut fleet, |fleet| {
            observer.fault = "owner_changed";
            observer
                .targets
                .values_mut()
                .next()
                .unwrap()
                .release_after_first_poll = false;
            assert!(
                fleet
                    .observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer)
                    .is_err()
            );
            assert!(observer.waits > 0);
            assert!(!fleet.processes[0].terminal);
            assert_eq!(observer.targets.values().next().unwrap().native_reads, 1);
            let evidence = fleet.resource_evidence();
            assert_eq!(evidence["first_rss_failure"]["candidate_generation"], 1);
            assert_eq!(
                evidence["first_pending_retirement"]["disposition"],
                "owner_changed"
            );
            assert_eq!(
                fs::read(observer.root.join("resource.json")).unwrap(),
                observer.original_frame
            );
            assert_eq!(
                fs::read(observer.root.join("rss-first-failure.json")).unwrap(),
                observer.first_error.clone().unwrap()
            );
        });
    }
    fn pending_activate_second(
        fleet: &mut Fleet,
        observer: &mut PendingFixtureObserver,
        fault_after_wait: u64,
        force_after: Duration,
    ) {
        fleet.processes[1].signal(libc::SIGINT).unwrap();
        // Complete actual signals/setup, then freeze deterministic original test budgets.
        observer.anchor = Instant::now();
        observer.anchor_ns = monotonic_ns().unwrap();
        pending_prior_publication_remaining(fleet, observer, RESOURCE_FRESH_NS);
        let force_at = observer.anchor + force_after;
        fleet.processes[1].stop_budget = Some(StopBudget {
            deadline: force_at + Duration::from_secs(FORCE_SECONDS),
            force_at,
        });
        let pid = fleet.processes[1].child.id();
        observer.targets.insert(
            pid,
            PendingFixtureTarget {
                input: observer.held_inputs.pop().unwrap(),
                native_reads: 0,
                polls: 0,
                fault_after_wait,
                release_after_first_poll: true,
            },
        );
    }
    #[test]
    fn rss_pending_exit_two_intentional_frame_targets_require_two_actual_reaps() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
        with_reaped_inert_children(&mut fleet, |fleet| {
            pending_activate_second(fleet, &mut observer, 0, Duration::from_secs(1));
            let force_at = observer.anchor + Duration::from_secs(1);
            fleet.processes[0].stop_budget = Some(StopBudget {
                deadline: force_at + Duration::from_secs(FORCE_SECONDS),
                force_at,
            });
            assert_eq!(
                fleet.observe_stopped_frame_with(true, false, &mut observer),
                Ok(())
            );
            assert_pending_success(fleet, &mut observer);
            assert_eq!(fleet.resources.last.as_ref().unwrap().expected.len(), 2);
        });
    }
    #[test]
    fn rss_pending_exit_later_target_cannot_reset_the_first_shared_original_bound() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
        with_reaped_inert_children(&mut fleet, |fleet| {
            observer
                .targets
                .values_mut()
                .next()
                .unwrap()
                .release_after_first_poll = false;
            pending_activate_second(fleet, &mut observer, 2, Duration::from_millis(10));
            let force_at = observer.anchor + Duration::from_millis(7);
            fleet.processes[0].stop_budget = Some(StopBudget {
                deadline: force_at + Duration::from_secs(FORCE_SECONDS),
                force_at,
            });
            let result = fleet.observe_stopped_frame_with(true, false, &mut observer);
            assert_eq!(result, Err(RSS_UNAVAILABLE.into()));
            assert!(observer.waits > 2 && observer.waits <= 7);
            for target in observer.targets.values() {
                assert_eq!(target.native_reads, 1);
            }
            assert!(!fleet.processes[0].terminal);
            assert!(fleet.processes[1].terminal && fleet.processes[1].receipt.success);
            assert_eq!(
                fs::read(observer.root.join("resource.json")).unwrap(),
                observer.original_frame
            );
        });
    }
    #[test]
    fn rss_pending_exit_direct_permission_does_not_cover_another_stopped_sibling() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, true);
        with_reaped_inert_children(&mut fleet, |fleet| {
            observer
                .targets
                .values_mut()
                .next()
                .unwrap()
                .release_after_first_poll = false;
            fleet.processes[1].signal(libc::SIGINT).unwrap();
            observer.anchor = Instant::now();
            observer.anchor_ns = monotonic_ns().unwrap();
            pending_prior_publication_remaining(fleet, &mut observer, RESOURCE_FRESH_NS);
            let force_at = observer.anchor + Duration::from_secs(1);
            for process in &mut fleet.processes {
                process.stop_budget = Some(StopBudget {
                    deadline: force_at + Duration::from_secs(FORCE_SECONDS),
                    force_at,
                });
            }
            observer.fault = "sibling_missing";
            assert!(
                fleet
                    .observe_stopped_process_with(0, RssSite::StopTarget, &mut observer)
                    .is_err()
            );
            assert!(observer.waits > 0);
            assert!(!fleet.processes[0].terminal && !fleet.processes[1].terminal);
        });
    }
    #[test]
    fn rss_pending_exit_prior_failure_survives_successful_actual_retirement() {
        let directory = tempfile::tempdir().unwrap();
        let (mut fleet, mut observer) = pending_fixture(directory.path(), 0, false);
        with_reaped_inert_children(&mut fleet, |fleet| {
            fleet.resources.remember("PRIOR_UNRELATED_FAILURE".into());
            assert_eq!(
                fleet.observe_stopped_process_with(0, RssSite::CleanupChild, &mut observer),
                Err("PRIOR_UNRELATED_FAILURE".into())
            );
            assert!(
                fleet.processes[0].terminal
                    && fleet.processes[0].receipt.reaped
                    && fleet.processes[0].receipt.success
            );
            assert_eq!(
                fleet.resources.failure.as_deref(),
                Some("PRIOR_UNRELATED_FAILURE")
            );
            let frame = fleet.resources.last.as_ref().unwrap();
            assert_eq!(frame.error.as_deref(), Some("PRIOR_UNRELATED_FAILURE"));
            assert!(
                frame
                    .observations
                    .iter()
                    .all(|value| value.bytes.is_some() && value.missing.is_none())
            );
        });
    }

    fn diagnostic_unavailable(started_ns: u64, finished_ns: u64) -> RssReadError {
        let mut error = RssReadError::new(RssCategory::TaskinfoUnavailable, RSS_UNAVAILABLE);
        error.taskinfo = Some(TaskinfoFailure {
            sample_started_ns: Some(started_ns),
            sample_finished_ns: Some(finished_ns),
            proc_pidinfo_return_bytes: 0,
            // Fixture scalar, not an asserted native struct size.
            proc_pidinfo_expected_bytes: 96,
            proc_pidinfo_errno_after_call: libc::ESRCH,
        });
        error
    }

    fn diagnostic_clock_failure_without_clocks() -> RssReadError {
        let mut error = diagnostic_unavailable(0, 0);
        error.category = RssCategory::SampleClockAfterFailed;
        error.message = "PRIVATE_SAMPLE_CLOCK_FAILURE".into();
        let facts = error.taskinfo.as_mut().unwrap();
        facts.sample_started_ns = None;
        facts.sample_finished_ns = None;
        error
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rss_diagnostic_macos_invalid_pid_failure_has_actual_call_scalars() {
        // c_int -1 cannot name a live positive PID. This native failure probe
        // supplies neither authority nor a resource observation to any frame.
        let error = rss(u32::MAX).unwrap_err();
        assert_eq!(error.category, RssCategory::TaskinfoUnavailable);
        assert_eq!(error.message, RSS_UNAVAILABLE);
        assert!(error.bytes.is_none());
        let facts = error.taskinfo.unwrap();
        assert_eq!(
            facts.proc_pidinfo_expected_bytes,
            std::mem::size_of::<libc::proc_taskinfo>() as u32,
        );
        assert_ne!(
            facts.proc_pidinfo_return_bytes,
            facts.proc_pidinfo_expected_bytes as i32,
        );
        let started = facts.sample_started_ns.unwrap();
        let finished = facts.sample_finished_ns.unwrap();
        assert!(started <= finished);
        // Keep raw errno evidence; do not require an invented kernel cause.
        let value = serde_json::to_value(facts).unwrap();
        assert_eq!(
            value["proc_pidinfo_errno_after_call"].as_i64(),
            Some(i64::from(facts.proc_pidinfo_errno_after_call)),
        );
        assert!(error.diagnostic().successful_sigint_observed_ns.is_none());
    }

    #[test]
    fn rss_diagnostic_direct_failure_keeps_fatal_result_and_links_actual_owned_reap() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process_ignoring_sigint(directory.path());
        process.receipt.node = "node-7".into();
        let owned_pid = process.child.id();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        with_reaped_inert_children(&mut fleet, |fleet| {
            assert!(fleet.processes[0].successful_sigint_observed_ns.is_none());
            fleet.processes[0].signal(0).unwrap();
            assert!(fleet.processes[0].successful_sigint_observed_ns.is_none());
            assert!(fleet.processes[0].signal(libc::c_int::MAX).is_err());
            assert!(fleet.processes[0].successful_sigint_observed_ns.is_none());
            fleet.processes[0].signal(libc::SIGINT).unwrap();
            let stop = fleet.processes[0].successful_sigint_observed_ns;
            fleet.processes[0].signal(0).unwrap();
            fleet.processes[0].signal(libc::SIGINT).unwrap();
            assert_eq!(fleet.processes[0].successful_sigint_observed_ns, stop);

            let started = monotonic_ns().unwrap();
            let finished = monotonic_ns().unwrap();
            let mut reads = 0;
            let mut polls = 0;
            let samples_before = fleet.processes[0].receipt.rss_samples;
            let result = fleet.sample_process_with(
                0,
                RssSite::CleanupChild,
                true,
                &mut |pid| {
                    assert_eq!(pid, owned_pid);
                    reads += 1;
                    Err(diagnostic_unavailable(started, finished))
                },
                &mut |child| {
                    assert_eq!(child.id(), owned_pid);
                    polls += 1;
                    Child::try_wait(child)
                },
            );
            assert_eq!(result, Err(RSS_UNAVAILABLE.into()));
            assert_eq!((reads, polls), (1, 1));
            assert!(!fleet.processes[0].terminal);
            assert_eq!(fleet.processes[0].receipt.rss_samples, samples_before);
            let first = fleet.resource_evidence()["first_rss_failure"].clone();
            assert_eq!(first["category"], "taskinfo_unavailable");
            assert_eq!(first["retirement_poll"], "running");
            assert!(first["candidate_bytes"].is_null());
            assert_eq!(
                first["diagnostic"]["taskinfo"]["sample_started_ns"],
                started
            );
            assert_eq!(
                first["diagnostic"]["taskinfo"]["sample_finished_ns"],
                finished
            );
            assert_eq!(
                first["diagnostic"]["taskinfo"]["proc_pidinfo_return_bytes"],
                0
            );
            assert_eq!(
                first["diagnostic"]["taskinfo"]["proc_pidinfo_expected_bytes"],
                96
            );
            assert_eq!(
                first["diagnostic"]["taskinfo"]["proc_pidinfo_errno_after_call"],
                libc::ESRCH
            );
            let stop = stop.expect("successful owned SIGINT observation missing");
            assert_eq!(first["diagnostic"]["successful_sigint_observed_ns"], stop);
            assert!(stop <= started && started <= finished);
            let retained = fs::read(directory.path().join("rss-first-failure.json")).unwrap();
            assert!(retained.len() <= FIRST_RSS_CAP);
            assert!(fleet.resource_evidence()["first_rss_reap"].is_null());

            // Only the retained actual Child establishes a reap; no invented status.
            release_inert_child(&mut input, owned_pid).unwrap();
            fleet.processes[0].poll_exit().unwrap();
            assert!(fleet.processes[0].terminal && fleet.processes[0].receipt.reaped);
            assert!(fleet.processes[0].receipt.success);
            let reaped = fleet.processes[0].actual_reap_observed_ns.unwrap();
            let (resources, processes) = (&mut fleet.resources, &fleet.processes);
            resources.remember_reap(&processes[0]);
            let link = fleet.resource_evidence()["first_rss_reap"].clone();
            assert_eq!(link["schema"], "mount-rs.cache-rss-reap-link.v1");
            assert_eq!(link["candidate_pid"], first["candidate_pid"]);
            assert_eq!(link["candidate_generation"], first["candidate_generation"]);
            assert_eq!(link["candidate_node"], first["candidate_node"]);
            assert_eq!(link["candidate_role"], first["candidate_role"]);
            assert_eq!(link["first_resource_sequence"], first["resource_sequence"]);
            assert_eq!(link["successful_sigint_observed_ns"], stop);
            assert_eq!(link["actual_reap_observed_ns"], reaped);
            assert!(finished <= reaped);
            assert_eq!(link["reaped_success"], true);
            assert_eq!(link["forced"], false);
            assert!(serde_json::to_vec_pretty(&link).unwrap().len() <= FIRST_RSS_CAP);
            assert_eq!(fleet.resource_evidence()["first_rss_failure"], first);
            assert_eq!(
                fs::read(directory.path().join("rss-first-failure.json")).unwrap(),
                retained
            );
            assert_eq!(result, Err(RSS_UNAVAILABLE.into()));
        });
    }

    #[test]
    fn rss_diagnostic_frame_capture_preserves_owned_failure_facts() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process_ignoring_sigint(directory.path());
        process.receipt.node = "node-7".into();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        with_reaped_inert_children(&mut fleet, |fleet| {
            fleet.processes[0].signal(libc::SIGINT).unwrap();
            let stop = fleet.processes[0].successful_sigint_observed_ns;
            let mut reads = 0;
            let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
                let server = identity.role == "server";
                let mut acquisition = fixture_observation(identity, server)?;
                if server {
                    reads += 1;
                    let stamp = acquisition.observation.finished_ns;
                    let error = diagnostic_unavailable(stamp, stamp);
                    acquisition.category = Some(error.category);
                    acquisition.diagnostic = error.diagnostic();
                }
                Ok(acquisition)
            });
            assert!(result.is_err());
            assert_eq!(reads, 1);
            let first = fleet.resource_evidence()["first_rss_failure"].clone();
            assert_eq!(first["site"], "worker_frame_stop_requested_child");
            assert_eq!(first["retirement_poll"], "running");
            assert_eq!(
                first["diagnostic"]["taskinfo"]["proc_pidinfo_errno_after_call"],
                libc::ESRCH
            );
            let stop = stop.expect("successful owned SIGINT observation missing");
            assert_eq!(first["diagnostic"]["successful_sigint_observed_ns"], stop);
            assert!(first["candidate_bytes"].is_null());
        });
    }

    #[test]
    fn rss_diagnostic_outer_capture_preserves_syscall_facts() {
        let run = ResourceRun {
            root: "PRIVATE_DIAGNOSTIC_ROOT".into(),
            controller_pid: 10,
            worker_pid: 20,
            group: 20,
        };
        let identity = supervisor_identity("controller", run.controller_pid);
        let mut outer_first = None;
        let result = outer_acquire(
            &run,
            123,
            identity,
            RssSite::OuterController,
            &mut outer_first,
            &mut |identity, _| {
                let mut acquisition = fixture_observation(identity, true)?;
                let stamp = acquisition.observation.finished_ns;
                let error = diagnostic_unavailable(stamp, stamp);
                acquisition.category = Some(error.category);
                acquisition.diagnostic = error.diagnostic();
                Ok(acquisition)
            },
        );
        assert!(result.unwrap().bytes.is_none());
        // The existing caller's totals/frame checks refuse the missing observation.
        let first = serde_json::to_value(outer_first.unwrap()).unwrap();
        assert_eq!(first["candidate_role"], "controller");
        assert_eq!(first["category"], "taskinfo_unavailable");
        assert!(first["diagnostic"]["successful_sigint_observed_ns"].is_null());
        assert_eq!(
            first["diagnostic"]["taskinfo"]["proc_pidinfo_errno_after_call"],
            libc::ESRCH
        );
        assert!(
            !serde_json::to_string(&first)
                .unwrap()
                .contains("PRIVATE_DIAGNOSTIC_ROOT")
        );
    }

    #[test]
    fn rss_diagnostic_error_paths_preserve_null_clocks_and_original_refusal() {
        for site in ["acquire", "child_frame", "outer", "outer_worker"] {
            let directory = tempfile::tempdir().unwrap();
            let (mut process, _input) = inert_process(directory.path());
            process.receipt.node = "node-7".into();
            let mut fleet = inert_resource_fleet(directory.path(), process);
            with_reaped_inert_children(&mut fleet, |fleet| {
                let mut outer_first = None;
                let mut polls = 0;
                let result = match site {
                    "acquire" => {
                        let identity =
                            supervisor_identity("controller", fleet.resources.run.controller_pid);
                        fleet
                            .acquire_resource(
                                &identity,
                                RssSite::WorkerFrameController,
                                true,
                                &mut |_, _| Err(diagnostic_clock_failure_without_clocks()),
                            )
                            .map(|_| ())
                    }
                    "child_frame" => {
                        fleet.sample_resources_with(true, false, &mut |identity, _| {
                            if identity.role == "server" {
                                Err(diagnostic_clock_failure_without_clocks())
                            } else {
                                fixture_observation(identity, false)
                            }
                        })
                    }
                    "outer" => {
                        let run = &fleet.resources.run;
                        outer_acquire(
                            run,
                            123,
                            supervisor_identity("controller", run.controller_pid),
                            RssSite::OuterController,
                            &mut outer_first,
                            &mut |_, _| Err(diagnostic_clock_failure_without_clocks()),
                        )
                        .map(|_| ())
                    }
                    _ => {
                        let child = &fleet.processes[0].child;
                        let run = ResourceRun {
                            root: fleet.resources.run.root.clone(),
                            controller_pid: fleet.resources.run.controller_pid,
                            worker_pid: child.id(),
                            group: child.id(),
                        };
                        outer_worker_acquire(
                            &run,
                            123,
                            child,
                            &mut outer_first,
                            &mut |_, _| Err(diagnostic_clock_failure_without_clocks()),
                            &mut |child| {
                                polls += 1;
                                exited_without_reap(child)
                            },
                        )
                        .map(|_| ())
                    }
                };
                assert_eq!(result, Err("PRIVATE_SAMPLE_CLOCK_FAILURE".into()), "{site}");
                assert_eq!(polls, 0, "clock failure must not permit retirement");
                let first = if site.starts_with("outer") {
                    serde_json::to_value(outer_first.unwrap()).unwrap()
                } else {
                    fleet.resource_evidence()["first_rss_failure"].clone()
                };
                assert_eq!(first["category"], "sample_clock_after_failed");
                assert!(first["candidate_bytes"].is_null());
                assert!(first["diagnostic"]["taskinfo"].is_object(), "{site}");
                assert!(first["diagnostic"]["taskinfo"]["sample_started_ns"].is_null());
                assert!(first["diagnostic"]["taskinfo"]["sample_finished_ns"].is_null());
                assert_eq!(
                    first["diagnostic"]["taskinfo"]["proc_pidinfo_errno_after_call"],
                    libc::ESRCH
                );
                let bytes = serde_json::to_vec_pretty(&first).unwrap();
                assert!(bytes.len() <= FIRST_RSS_CAP);
                assert!(
                    !String::from_utf8(bytes)
                        .unwrap()
                        .contains("PRIVATE_SAMPLE_CLOCK_FAILURE")
                );
            });
        }
    }

    #[test]
    fn rss_diagnostic_reap_link_requires_the_exact_retained_child_identity() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process_ignoring_sigint(directory.path());
        process.receipt.node = "node-7".into();
        let owned_pid = process.child.id();
        let (mut sibling, mut sibling_input) = inert_process_ignoring_sigint(directory.path());
        sibling.receipt.node = "node-8".into();
        let sibling_pid = sibling.child.id();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        fleet.processes.push(sibling);
        with_reaped_inert_children(&mut fleet, |fleet| {
            fleet.processes[0].signal(libc::SIGINT).unwrap();
            let candidate = RssCandidate::from_identity(
                &fleet.processes[0].identity(),
                RssSite::CleanupChild,
                RssCategory::TaskinfoUnavailable,
                RssPoll::Running,
                None,
                None,
            );
            fleet.resources.remember_rss(candidate);
            let first = fleet.resource_evidence()["first_rss_failure"].clone();
            let retained = fs::read(directory.path().join("rss-first-failure.json")).unwrap();
            {
                let (resources, processes) = (&mut fleet.resources, &fleet.processes);
                resources.remember_reap(&processes[0]);
            }
            assert!(fleet.resource_evidence()["first_rss_reap"].is_null());
            release_inert_child(&mut sibling_input, sibling_pid).unwrap();
            fleet.processes[1].poll_exit().unwrap();
            {
                let (resources, processes) = (&mut fleet.resources, &fleet.processes);
                resources.remember_reap(&processes[1]);
            }
            assert!(fleet.resource_evidence()["first_rss_reap"].is_null());
            release_inert_child(&mut input, owned_pid).unwrap();
            fleet.processes[0].poll_exit().unwrap();
            assert!(fleet.processes[0].terminal && fleet.processes[0].receipt.reaped);
            let original = fleet.processes[0].receipt.clone();
            for mismatch in ["role", "pid", "generation", "node", "canonical_node"] {
                match mismatch {
                    "role" => fleet.processes[0].receipt.role = "catalog-apply".into(),
                    "pid" => fleet.processes[0].receipt.pid = sibling_pid,
                    "generation" => fleet.processes[0].receipt.generation = original.generation + 1,
                    "node" => fleet.processes[0].receipt.node = "node-8".into(),
                    _ => fleet.processes[0].receipt.node = "node-07".into(),
                }
                {
                    let (resources, processes) = (&mut fleet.resources, &fleet.processes);
                    resources.remember_reap(&processes[0]);
                }
                assert!(
                    fleet.resource_evidence()["first_rss_reap"].is_null(),
                    "{mismatch}"
                );
                fleet.processes[0].receipt = original.clone();
            }
            {
                let (resources, processes) = (&mut fleet.resources, &fleet.processes);
                resources.remember_reap(&processes[0]);
            }
            let link = fleet.resource_evidence()["first_rss_reap"].clone();
            assert!(link.is_object(), "exact actual Child reap must be linked");
            assert_eq!(link["candidate_pid"], owned_pid);
            assert_eq!(link["candidate_role"], first["candidate_role"]);
            assert_eq!(link["candidate_generation"], first["candidate_generation"]);
            assert_eq!(link["candidate_node"], first["candidate_node"]);
            assert_eq!(link["first_resource_sequence"], first["resource_sequence"]);
            assert_eq!(link["reaped_success"], true);
            assert_eq!(link["forced"], false);
            assert_eq!(fleet.resource_evidence()["first_rss_failure"], first);
            assert_eq!(
                fs::read(directory.path().join("rss-first-failure.json")).unwrap(),
                retained
            );
        });
    }

    #[test]
    fn rss_diagnostic_maximum_compact_and_pretty_payloads_keep_two_kibibyte_cap() {
        let run = ResourceRun {
            root: "PRIVATE_MAX_DIAGNOSTIC_ROOT".into(),
            controller_pid: u32::MAX - 2,
            worker_pid: u32::MAX - 1,
            group: u32::MAX - 1,
        };
        let candidate = RssCandidate {
            role: RssRole::Server,
            pid: u32::MAX,
            generation: u64::MAX,
            node: Some(9),
            site: RssSite::WorkerFrameStopRequestedChild,
            category: RssCategory::ParentChangedBeforeSample,
            poll: RssPoll::NotAttempted,
            observed_ns: Some(u64::MAX),
            bytes: Some(u64::MAX),
            diagnostic: RssDiagnostic {
                taskinfo: Some(TaskinfoFailure {
                    sample_started_ns: Some(u64::MAX),
                    sample_finished_ns: Some(u64::MAX),
                    proc_pidinfo_return_bytes: i32::MIN,
                    proc_pidinfo_expected_bytes: u32::MAX,
                    proc_pidinfo_errno_after_call: i32::MAX,
                }),
                successful_sigint_observed_ns: Some(u64::MAX),
            },
        };
        let first = FirstRssFailure::new(&run, RssProducer::Worker, u64::MAX, candidate);
        let reap = FirstRssReap {
            schema: "mount-rs.cache-rss-reap-link.v1",
            first_resource_sequence: u64::MAX,
            candidate_role: candidate.role,
            candidate_pid: candidate.pid,
            candidate_generation: candidate.generation,
            candidate_node: candidate.node,
            successful_sigint_observed_ns: Some(u64::MAX),
            actual_reap_observed_ns: Some(u64::MAX),
            reaped_success: false,
            forced: false,
        };
        for bytes in [
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec_pretty(&first).unwrap(),
        ] {
            assert!(bytes.len() <= FIRST_RSS_CAP, "{} bytes", bytes.len());
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["schema"], "mount-rs.cache-rss-failure.v2");
            assert_eq!(value.as_object().unwrap().len(), 16);
            assert_eq!(
                value["diagnostic"]["taskinfo"]["proc_pidinfo_return_bytes"],
                i32::MIN
            );
            assert_eq!(
                value["diagnostic"]["taskinfo"]["proc_pidinfo_errno_after_call"],
                i32::MAX
            );
            assert!(
                !String::from_utf8(bytes)
                    .unwrap()
                    .contains("PRIVATE_MAX_DIAGNOSTIC_ROOT")
            );
        }
        for bytes in [
            serde_json::to_vec(&reap).unwrap(),
            serde_json::to_vec_pretty(&reap).unwrap(),
        ] {
            assert!(bytes.len() <= FIRST_RSS_CAP, "{} bytes", bytes.len());
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["candidate_pid"], u32::MAX);
            assert_eq!(value["candidate_generation"], u64::MAX);
            assert_eq!(value["actual_reap_observed_ns"], u64::MAX);
            assert!(
                !String::from_utf8(bytes)
                    .unwrap()
                    .contains("PRIVATE_MAX_DIAGNOSTIC_ROOT")
            );
        }
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
        assert_eq!(first["schema"], "mount-rs.cache-rss-failure.v2");
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
    fn rss_first_failure_frame_stop_intent_requires_successful_owned_sigint() {
        for signal in [0, libc::c_int::MAX, libc::SIGINT] {
            for failure in ["missing", "cap", "read"] {
                let directory = tempfile::tempdir().unwrap();
                let (mut process, _input) = inert_process_ignoring_sigint(directory.path());
                process.receipt.node = "node-7".into();
                let owned_pid = process.child.id();
                let mut fleet = inert_resource_fleet(directory.path(), process);
                with_reaped_inert_children(&mut fleet, |fleet| {
                    let signal_result = fleet.processes[0].signal(signal);
                    assert_eq!(signal_result.is_ok(), signal != libc::c_int::MAX);
                    // A later successful non-stop signal must not reset a recorded request.
                    fleet.processes[0].signal(0).unwrap();
                    let mut child_reads = 0;
                    let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
                        if identity.role != "server" {
                            return fixture_observation(identity, false);
                        }
                        assert_eq!(identity.pid, owned_pid);
                        child_reads += 1;
                        match failure {
                            "missing" => fixture_observation(identity, true),
                            "cap" => {
                                let mut value = fixture_observation(identity, false)?;
                                value.observation.bytes = Some(RSS_CAP);
                                Ok(value)
                            }
                            _ => Err(RssReadError::new(
                                RssCategory::ProcStatusReadFailed,
                                "PRIVATE_READ_FAILURE",
                            )),
                        }
                    });
                    let first = fleet.resource_evidence()["first_rss_failure"].clone();
                    let frame = fleet.resources.last.as_ref().unwrap();
                    if failure != "read" {
                        assert!(frame.error.is_some());
                        if failure == "missing" {
                            assert_eq!((frame.total_bytes, frame.child_bytes), (None, None));
                        }
                        assert_eq!(frame.expected.len(), 3);
                    }
                    assert!(result.is_err());
                    assert_eq!(child_reads, 1);
                    assert_eq!(first.as_object().unwrap().len(), 16);
                    assert_eq!(first["candidate_pid"], owned_pid);
                    assert_eq!(first["candidate_node"], 7);
                    assert_eq!(first["candidate_generation"], 1);
                    assert_eq!(
                        first["site"],
                        if signal == libc::SIGINT {
                            "worker_frame_stop_requested_child"
                        } else {
                            "worker_frame_child"
                        }
                    );
                    match failure {
                        "missing" => {
                            assert_eq!(
                                first["category"],
                                serde_json::to_value(RssReadError::unavailable().category).unwrap()
                            );
                            assert_eq!(first["retirement_poll"], "running");
                            assert!(first["candidate_bytes"].is_null());
                        }
                        "cap" => {
                            assert_eq!(first["category"], "pid_rss_cap_exceeded");
                            assert_eq!(first["retirement_poll"], "not_attempted");
                            assert_eq!(first["candidate_bytes"], RSS_CAP);
                        }
                        _ => {
                            assert_eq!(first["category"], "proc_status_read_failed");
                            assert_eq!(first["retirement_poll"], "not_attempted");
                            assert!(first["candidate_bytes"].is_null());
                        }
                    }
                    let retained =
                        fs::read(directory.path().join("rss-first-failure.json")).unwrap();
                    assert!(retained.len() <= FIRST_RSS_CAP);
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&retained).unwrap(),
                        first
                    );
                    assert!(
                        !String::from_utf8(retained)
                            .unwrap()
                            .contains("PRIVATE_READ_FAILURE")
                    );
                });
            }
        }
    }
    #[test]
    fn rss_first_failure_requested_child_does_not_label_sibling_or_replace_first_record() {
        let directory = tempfile::tempdir().unwrap();
        let (mut requested, _requested_input) = inert_process_ignoring_sigint(directory.path());
        requested.receipt.node = "node-0".into();
        let (mut sibling, _sibling_input) = inert_process_ignoring_sigint(directory.path());
        sibling.receipt.node = "node-1".into();
        sibling.receipt.generation = 2;
        let sibling_pid = sibling.child.id();
        let mut fleet = inert_resource_fleet(directory.path(), requested);
        fleet.processes.push(sibling);
        with_reaped_inert_children(&mut fleet, |fleet| {
            fleet.processes[0].signal(libc::SIGINT).unwrap();
            let result = fleet.sample_resources_with(true, false, &mut |identity, _| {
                let unavailable = identity.pid == sibling_pid;
                fixture_observation(identity, unavailable)
            });
            let first = fleet.resource_evidence()["first_rss_failure"].clone();
            let retained = fs::read(directory.path().join("rss-first-failure.json")).unwrap();
            assert!(result.is_err());
            assert_eq!(first["candidate_pid"], sibling_pid);
            assert_eq!(first["site"], "worker_frame_child");
            assert_eq!(first["retirement_poll"], "running");
            fleet.processes[1].signal(libc::SIGINT).unwrap();
            let later = fleet.sample_resources_with(true, false, &mut |identity, _| {
                fixture_observation(identity, false)
            });
            assert!(later.is_err());
            assert_eq!(fleet.resource_evidence()["first_rss_failure"], first);
            assert_eq!(
                fs::read(directory.path().join("rss-first-failure.json")).unwrap(),
                retained
            );
        });
    }
    #[test]
    fn rss_stop_intent_does_not_allow_unexpected_active_retirement() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process_ignoring_sigint(directory.path());
        process.receipt.node = "node-2".into();
        let mut fleet = inert_resource_fleet(directory.path(), process);
        with_reaped_inert_children(&mut fleet, |fleet| {
            let process = &mut fleet.processes[0];
            process.signal(libc::SIGINT).unwrap();
            let result = process.sample_with(&mut |pid| {
                release_inert_child(&mut input, pid)?;
                Err(RssReadError::unavailable())
            });
            assert_eq!(result, Err("node-2 exited before requested stop".into()));
            assert!(process.terminal && process.receipt.reaped && process.receipt.success);
            let first = process.first_rss_failure.unwrap();
            assert_eq!(first.site, RssSite::ActiveChild);
            assert_eq!(first.category, RssCategory::UnexpectedRetirement);
            assert_eq!(first.poll, RssPoll::ReapedSuccess);
            assert!(process.signal(libc::SIGINT).is_err());
        });
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
    fn outer_worker_run(root: &Path, child: &Child) -> ResourceRun {
        ResourceRun {
            root: root.to_string_lossy().into_owned(),
            controller_pid: std::process::id(),
            worker_pid: child.id(),
            group: child.id(),
        }
    }

    fn frame_validation_owned_frame(run: &ResourceRun, now: u64, sequence: u64) -> ResourceFrame {
        let mut frame = resource_frame(now);
        frame.run = run.clone();
        frame.sequence = sequence;
        frame.expected = vec![
            supervisor_identity("controller", run.controller_pid),
            supervisor_identity("worker", run.worker_pid),
        ];
        for (observation, identity) in frame.observations.iter_mut().zip(&frame.expected) {
            observation.identity = identity.clone();
        }
        frame
    }

    fn frame_validation_acquisition(identity: RssIdentity, now: u64) -> RssAcquisition {
        RssAcquisition {
            observation: RssObservation {
                identity,
                started_ns: now,
                finished_ns: now,
                bytes: Some(4096),
                missing: None,
            },
            category: None,
            diagnostic: RssDiagnostic::default(),
        }
    }

    fn frame_validation_evidence(clock: &Rc<Cell<u64>>) -> OuterSampleEvidence {
        let clock = Rc::clone(clock);
        OuterSampleEvidence {
            validation_clock: Some(Box::new(move || Ok(clock.get()))),
            ..OuterSampleEvidence::default()
        }
    }

    fn frame_validation_close_child(
        process: &mut OwnedProcess,
        input: &mut Option<std::process::ChildStdin>,
    ) {
        release_inert_child(input, process.child.id()).unwrap();
        process.poll_exit().unwrap();
        cleanup_inert_child(process);
        assert!(process.receipt.reaped && process.receipt.success && process.terminal);
    }

    #[test]
    fn frame_validation_same_fresh_sequence_then_stale_is_retained_before_rss() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let started = 1_000_000_000;
        let frame = frame_validation_owned_frame(&run, started, 7);
        resource_write(directory.path(), "resource.json", &frame).unwrap();
        let clock = Rc::new(Cell::new(started + RESOURCE_FRESH_NS));
        let mut evidence = frame_validation_evidence(&clock);
        let reads = Cell::new(0);
        let mut measure = |identity, _| {
            reads.set(reads.get() + 1);
            Ok(frame_validation_acquisition(identity, clock.get()))
        };
        let mut poll = |_: &Child| panic!("a valid observation must not perform a retirement poll");
        let first = outer_sample_with(
            directory.path(),
            &run,
            7,
            &mut evidence,
            &process.child,
            &mut measure,
            &mut poll,
        );
        let repeated = outer_sample_with(
            directory.path(),
            &run,
            7,
            &mut evidence,
            &process.child,
            &mut measure,
            &mut poll,
        );
        let fresh_reads = reads.get();
        clock.set(started + RESOURCE_FRESH_NS + 1);
        let rejected = outer_sample_with(
            directory.path(),
            &run,
            7,
            &mut evidence,
            &process.child,
            &mut measure,
            &mut poll,
        );
        frame_validation_close_child(&mut process, &mut input);
        assert!(matches!(first, Ok(Some(_))) && matches!(repeated, Ok(Some(_))));
        assert_eq!(fresh_reads, 4);
        assert_eq!(
            reads.get(),
            fresh_reads,
            "stale rejection must precede all RSS queries"
        );
        assert_eq!(
            rejected.err().as_deref(),
            Some("RSS frame missing, stale, foreign, regressed or incomplete")
        );
        assert!(evidence.first_rss_failure.is_none());
        let record = evidence
            .first_frame_validation_failure
            .expect("failed frame validation must retain its exact first evidence");
        assert_eq!(record.site, FrameValidationSite::BeforeRssAcquisition);
        assert_eq!(record.accepted_sequence, 7);
        assert_eq!(record.frame_sequence, 7);
        assert_eq!(
            (record.frame_started_ns, record.frame_finished_ns),
            (started, started)
        );
        assert_eq!(record.validation_now_ns, clock.get());
        assert_eq!(record.age_ns, Some(RESOURCE_FRESH_NS + 1));
        assert_eq!(
            record.expected_run,
            FrameRunScalars {
                controller_pid: run.controller_pid,
                worker_pid: run.worker_pid,
                group: run.group
            }
        );
        assert_eq!(record.frame_run, record.expected_run);
        assert_eq!(
            record.failed,
            FrameValidationPredicates {
                stale: true,
                ..FrameValidationPredicates::default()
            }
        );
    }

    #[test]
    fn frame_validation_final_recompose_retains_loaded_frame_and_first_failure() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let started = 2_000_000_000;
        let frame = frame_validation_owned_frame(&run, started, 9);
        resource_write(directory.path(), "resource.json", &frame).unwrap();
        let clock = Rc::new(Cell::new(started));
        let mut evidence = frame_validation_evidence(&clock);
        let reads = Cell::new(0);
        let later_now = started + RESOURCE_FRESH_NS + 1;
        let mut replacement = frame_validation_owned_frame(&run, later_now, 10);
        let result = outer_sample_with(
            directory.path(),
            &run,
            8,
            &mut evidence,
            &process.child,
            &mut |identity, _| {
                reads.set(reads.get() + 1);
                if identity.role == "controller" {
                    resource_write(directory.path(), "resource.json", &replacement)?;
                    clock.set(later_now);
                }
                Ok(frame_validation_acquisition(identity, started))
            },
            &mut |_| panic!("observed supervisors must not perform retirement polls"),
        );
        let first = evidence.first_frame_validation_failure;
        replacement.schema = 2;
        replacement.error = Some("PRIVATE_LATER_FRAME_ERROR".into());
        resource_write(directory.path(), "resource.json", &replacement).unwrap();
        let later = outer_sample_with(
            directory.path(),
            &run,
            8,
            &mut evidence,
            &process.child,
            &mut |_, _| panic!("the later invalid frame must fail before RSS queries"),
            &mut |_| panic!("the later invalid frame must fail before retirement polls"),
        );
        frame_validation_close_child(&mut process, &mut input);
        assert_eq!(reads.get(), 2);
        assert_eq!(
            result.err().as_deref(),
            Some("RSS frame missing, stale, foreign, regressed or incomplete")
        );
        assert!(later.is_err());
        assert!(evidence.first_rss_failure.is_none());
        assert_eq!(evidence.first_frame_validation_failure, first);
        let record = first.expect(
            "final recomposition must retain the loaded failed frame, not reread its replacement",
        );
        assert_eq!(record.site, FrameValidationSite::FinalRecompose);
        assert_eq!(record.accepted_sequence, 8);
        assert_eq!(record.frame_sequence, 9);
        assert_eq!(
            (record.frame_started_ns, record.frame_finished_ns),
            (started, started)
        );
        assert_eq!(record.validation_now_ns, later_now);
        assert_eq!(record.age_ns, Some(RESOURCE_FRESH_NS + 1));
        assert_eq!(
            record.failed,
            FrameValidationPredicates {
                stale: true,
                ..FrameValidationPredicates::default()
            }
        );
    }

    #[test]
    fn frame_validation_max_scalars_are_bounded_without_private_strings() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let mut frame = frame_validation_owned_frame(&run, u64::MAX, u64::MAX);
        frame.schema = u32::MAX;
        frame.run.controller_pid = u32::MAX;
        frame.run.worker_pid = u32::MAX;
        frame.run.group = u32::MAX;
        frame.run.root = "PRIVATE_FRAME_ROOT".into();
        frame.error = Some("PRIVATE_FRAME_ERROR".into());
        resource_write(directory.path(), "resource.json", &frame).unwrap();
        let clock = Rc::new(Cell::new(1));
        let mut evidence = frame_validation_evidence(&clock);
        let result = outer_sample_with(
            directory.path(),
            &run,
            u64::MAX,
            &mut evidence,
            &process.child,
            &mut |_, _| panic!("invalid frame must not query RSS"),
            &mut |_| panic!("invalid frame must not poll the worker"),
        );
        frame_validation_close_child(&mut process, &mut input);
        assert_eq!(
            result.err().as_deref(),
            Some("RSS frame missing, stale, foreign, regressed or incomplete")
        );
        assert!(evidence.first_rss_failure.is_none());
        let record = evidence
            .first_frame_validation_failure
            .expect("parsed failed frames require bounded numeric evidence");
        assert_eq!(record.frame_schema, u32::MAX);
        assert_eq!(record.frame_sequence, u64::MAX);
        assert_eq!(record.accepted_sequence, u64::MAX);
        assert_eq!(record.frame_started_ns, u64::MAX);
        assert_eq!(record.frame_finished_ns, u64::MAX);
        assert_eq!(record.age_ns, None, "future timestamps have no checked age");
        assert_eq!(
            record.failed,
            FrameValidationPredicates {
                schema: true,
                run: true,
                frame_error: true,
                future: true,
                ..FrameValidationPredicates::default()
            }
        );
        let bytes = serde_json::to_vec(&record).unwrap();
        assert!(bytes.len() <= FIRST_FRAME_VALIDATION_CAP);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("PRIVATE_FRAME_ROOT") && !text.contains("PRIVATE_FRAME_ERROR"));
        assert!(!text.contains(&run.root));
    }

    #[test]
    fn frame_validation_malformed_json_has_no_numeric_evidence_or_rss_queries() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        fs::write(directory.path().join("resource.json"), b"{").unwrap();
        let mut evidence = OuterSampleEvidence {
            validation_clock: Some(Box::new(|| {
                panic!("unparsed JSON has no validation timestamp")
            })),
            ..OuterSampleEvidence::default()
        };
        let result = outer_sample_with(
            directory.path(),
            &run,
            0,
            &mut evidence,
            &process.child,
            &mut |_, _| panic!("unparsed JSON must not query RSS"),
            &mut |_| panic!("unparsed JSON must not poll the worker"),
        );
        frame_validation_close_child(&mut process, &mut input);
        assert_eq!(
            result.err(),
            Some(
                serde_json::from_slice::<ResourceFrame>(b"{")
                    .unwrap_err()
                    .to_string()
            )
        );
        assert!(evidence.first_frame_validation_failure.is_none());
        assert!(evidence.first_rss_failure.is_none());
    }

    #[test]
    fn frame_validation_other_contract_failure_has_categorical_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let now = 4_000_000_000;
        let mut frame = frame_validation_owned_frame(&run, now, 3);
        frame.expected.pop();
        frame.observations.pop();
        frame.total_bytes = Some(4096);
        resource_write(directory.path(), "resource.json", &frame).unwrap();
        let clock = Rc::new(Cell::new(now));
        let mut evidence = frame_validation_evidence(&clock);
        let result = outer_sample_with(
            directory.path(),
            &run,
            2,
            &mut evidence,
            &process.child,
            &mut |_, _| panic!("an invalid publication contract must fail before RSS acquisition"),
            &mut |_| panic!("an invalid publication contract must not poll the worker"),
        );
        frame_validation_close_child(&mut process, &mut input);
        assert_eq!(
            result.err().as_deref(),
            Some("RSS frame lacks exact supervisor identity")
        );
        assert!(evidence.first_rss_failure.is_none());
        let record = evidence
            .first_frame_validation_failure
            .expect("contract-only frame failures require static categorical evidence");
        assert_eq!(
            record.failed,
            FrameValidationPredicates {
                other_contract: true,
                ..FrameValidationPredicates::default()
            }
        );
        assert_eq!(record.frame_sequence, 3);
        assert_eq!(record.accepted_sequence, 2);
        assert_eq!(record.age_ns, Some(0));
    }

    #[test]
    fn frame_validation_final_rss_error_does_not_fabricate_frame_failure() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let now = 3_000_000_000;
        let frame = frame_validation_owned_frame(&run, now, 1);
        resource_write(directory.path(), "resource.json", &frame).unwrap();
        let clock = Rc::new(Cell::new(now));
        let mut evidence = frame_validation_evidence(&clock);
        let result = outer_sample_with(
            directory.path(),
            &run,
            0,
            &mut evidence,
            &process.child,
            &mut |identity, _| {
                let is_worker = identity.role == "worker";
                let mut value = frame_validation_acquisition(identity, now);
                if is_worker {
                    value.observation.finished_ns = now + 1;
                }
                Ok(value)
            },
            &mut |_| panic!("observed supervisors must not perform retirement polls"),
        );
        frame_validation_close_child(&mut process, &mut input);
        assert_eq!(
            result.err().as_deref(),
            Some("outer RSS sample missing, stale or future")
        );
        assert!(evidence.first_frame_validation_failure.is_none());
        assert!(evidence.first_rss_failure.is_none());
    }

    #[test]
    fn rss_outer_worker_retirement_rejects_earlier_controller_sample_failures() {
        for unavailable in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (mut process, mut input) = inert_process(directory.path());
            let run = outer_worker_run(directory.path(), &process.child);
            let mut frame = resource_frame(monotonic_ns().unwrap());
            frame.run = run.clone();
            frame.expected = vec![
                supervisor_identity("controller", run.controller_pid),
                supervisor_identity("worker", run.worker_pid),
            ];
            for (observation, identity) in frame.observations.iter_mut().zip(&frame.expected) {
                observation.identity = identity.clone();
            }
            resource_write(directory.path(), "resource.json", &frame).unwrap();
            let mut first = OuterSampleEvidence::default();
            let mut worker_reads = 0;
            let mut polls = 0;
            let result = outer_sample_with(
                directory.path(),
                &run,
                0,
                &mut first,
                &process.child,
                &mut |identity, _| {
                    if identity.role == "controller" {
                        let mut value = fixture_observation(identity, unavailable)?;
                        if !unavailable {
                            value.observation.bytes = Some(RSS_CAP);
                        }
                        Ok(value)
                    } else {
                        worker_reads += 1;
                        release_inert_child(&mut input, identity.pid)?;
                        fixture_observation(identity, true)
                    }
                },
                &mut |child| {
                    polls += 1;
                    exited_without_reap(child)
                },
            );
            process.poll_exit().unwrap();
            cleanup_inert_child(&mut process);
            assert!(result.is_err());
            assert_eq!((worker_reads, polls), (0, 0));
            let record = first.first_rss_failure.unwrap();
            assert_eq!(record.site, RssSite::OuterController);
            assert_eq!(
                record.category,
                if unavailable {
                    RssReadError::unavailable().category
                } else {
                    RssCategory::PidRssCapExceeded
                }
            );
        }
    }

    #[test]
    fn rss_outer_worker_retirement_confirms_the_exit_between_poll_and_sample() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        assert!(!exited_without_reap(&process.child).unwrap());
        let mut first = None;
        let mut reads = 0;
        let mut polls = 0;
        let result = outer_worker_acquire(
            &run,
            9,
            &process.child,
            &mut first,
            &mut |identity, verify_parent| {
                assert_eq!(identity, supervisor_identity("worker", run.worker_pid));
                assert!(!verify_parent);
                reads += 1;
                release_inert_child(&mut input, identity.pid)?;
                fixture_observation(identity, true)
            },
            &mut |child| {
                assert_eq!(child.id(), run.worker_pid);
                polls += 1;
                exited_without_reap(child)
            },
        );
        let terminal = exited_without_reap(&process.child).unwrap();
        process.poll_exit().unwrap();
        cleanup_inert_child(&mut process);
        assert!(matches!(result, Ok(OuterWorkerAcquisition::Exited)));
        assert!(terminal);
        assert!(first.is_none());
        assert_eq!((reads, polls), (1, 1));
        assert_eq!(process.receipt.rss_samples, 0);
        assert!(!directory.path().join("rss-first-failure.json").exists());
    }

    #[test]
    fn rss_outer_worker_retirement_keeps_a_running_worker_failure() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let mut first = None;
        let mut polls = 0;
        let result = outer_worker_acquire(
            &run,
            9,
            &process.child,
            &mut first,
            &mut |identity, _| fixture_observation(identity, true),
            &mut |child| {
                polls += 1;
                exited_without_reap(child)
            },
        );
        cleanup_inert_child(&mut process);
        let Ok(OuterWorkerAcquisition::Observed(observation)) = result else {
            panic!("live owned worker unavailable must remain an observation failure")
        };
        assert!(
            rss_totals(
                std::slice::from_ref(&observation.identity),
                std::slice::from_ref(&observation)
            )
            .is_err()
        );
        let record = first.unwrap();
        assert_eq!(record.category, RssReadError::unavailable().category);
        assert_eq!(record.retirement_poll, RssPoll::Running);
        assert_eq!(polls, 1);
    }

    #[test]
    fn rss_outer_worker_retirement_poll_failure_is_fatal_and_typed() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let mut first = None;
        let mut polls = 0;
        let result = outer_worker_acquire(
            &run,
            9,
            &process.child,
            &mut first,
            &mut |identity, _| fixture_observation(identity, true),
            &mut |child| {
                assert_eq!(child.id(), run.worker_pid);
                polls += 1;
                Err("injected owned WNOWAIT failure".into())
            },
        );
        cleanup_inert_child(&mut process);
        assert!(matches!(result, Err(error) if error == "injected owned WNOWAIT failure"));
        let record = first.unwrap();
        assert_eq!(record.category, RssCategory::RetirementPollFailed);
        assert_eq!(record.retirement_poll, RssPoll::PollError);
        assert_eq!(polls, 1);
    }

    #[test]
    fn rss_outer_worker_retirement_cannot_excuse_a_cap_sample_after_exit() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let mut first = None;
        let mut polls = 0;
        let result = outer_worker_acquire(
            &run,
            9,
            &process.child,
            &mut first,
            &mut |identity, _| {
                release_inert_child(&mut input, identity.pid)?;
                let mut value = fixture_observation(identity, false)?;
                value.observation.bytes = Some(RSS_CAP);
                Ok(value)
            },
            &mut |_| {
                polls += 1;
                Ok(true)
            },
        );
        process.poll_exit().unwrap();
        cleanup_inert_child(&mut process);
        let Ok(OuterWorkerAcquisition::Observed(observation)) = result else {
            panic!("owned worker RSS cap must remain an observed cap violation")
        };
        assert!(
            rss_totals(
                std::slice::from_ref(&observation.identity),
                std::slice::from_ref(&observation)
            )
            .is_err()
        );
        let record = first.unwrap();
        assert_eq!(record.category, RssCategory::PidRssCapExceeded);
        assert_eq!(record.candidate_bytes, Some(RSS_CAP));
        assert_eq!(record.retirement_poll, RssPoll::NotApplicable);
        assert_eq!(polls, 0);
    }

    #[test]
    fn rss_outer_worker_retirement_rejects_another_retained_child_identity() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        let mut run = outer_worker_run(directory.path(), &process.child);
        run.worker_pid += 1;
        run.group = run.worker_pid;
        let mut first = None;
        let mut reads = 0;
        let mut polls = 0;
        let result = outer_worker_acquire(
            &run,
            9,
            &process.child,
            &mut first,
            &mut |identity, _| {
                reads += 1;
                fixture_observation(identity, true)
            },
            &mut |_| {
                polls += 1;
                Ok(true)
            },
        );
        cleanup_inert_child(&mut process);
        assert!(matches!(result, Err(error) if error == "outer RSS worker ownership mismatch"));
        assert_eq!((reads, polls), (0, 0));
        assert!(first.is_none());
    }

    #[test]
    fn rss_outer_worker_retirement_does_not_excuse_a_controller_or_untyped_error() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, _input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let identity = supervisor_identity("controller", run.controller_pid);
        let mut first = None;
        let observation = outer_acquire(
            &run,
            9,
            identity.clone(),
            RssSite::OuterController,
            &mut first,
            &mut |identity, _| fixture_observation(identity, true),
        )
        .unwrap();
        assert!(rss_totals(&[identity], &[observation]).is_err());
        assert_eq!(first.unwrap().site, RssSite::OuterController);
        for category in [
            RssCategory::ProcStatusReadFailed,
            RssCategory::VmrssValueMissing,
            RssCategory::VmrssValueInvalid,
            RssCategory::SampleClockBeforeFailed,
            RssCategory::SampleClockAfterFailed,
            RssCategory::OtherRssError,
        ] {
            let mut first = None;
            let mut polls = 0;
            let result = outer_worker_acquire(
                &run,
                9,
                &process.child,
                &mut first,
                &mut |_, _| Err(RssReadError::new(category, RSS_UNAVAILABLE)),
                &mut |_| {
                    polls += 1;
                    Ok(true)
                },
            );
            assert!(matches!(result, Err(error) if error == RSS_UNAVAILABLE));
            assert_eq!(first.unwrap().category, category);
            assert_eq!(polls, 0);
        }
        cleanup_inert_child(&mut process);
    }

    #[test]
    fn rss_outer_worker_retirement_preserves_an_existing_first_failure() {
        let directory = tempfile::tempdir().unwrap();
        let (mut process, mut input) = inert_process(directory.path());
        let run = outer_worker_run(directory.path(), &process.child);
        let mut first = None;
        let _ = outer_acquire(
            &run,
            8,
            supervisor_identity("controller", run.controller_pid),
            RssSite::OuterController,
            &mut first,
            &mut |_, _| {
                Err(RssReadError::new(
                    RssCategory::OtherRssError,
                    "earlier failure",
                ))
            },
        );
        let before = serde_json::to_value(first).unwrap();
        let result = outer_worker_acquire(
            &run,
            9,
            &process.child,
            &mut first,
            &mut |identity, _| {
                release_inert_child(&mut input, identity.pid)?;
                fixture_observation(identity, true)
            },
            &mut exited_without_reap,
        );
        process.poll_exit().unwrap();
        cleanup_inert_child(&mut process);
        assert!(matches!(result, Ok(OuterWorkerAcquisition::Exited)));
        assert_eq!(serde_json::to_value(first).unwrap(), before);
    }

    #[test]
    fn rss_outer_worker_retirement_keeps_failed_cleanup_receipts_and_exit_status() {
        for status in [0, 7] {
            for receipt in [None, Some(false), Some(true)] {
                let directory = tempfile::tempdir().unwrap();
                let (mut process, mut input) = inert_process_status(directory.path(), status);
                let run = outer_worker_run(directory.path(), &process.child);
                if let Some(closed) = receipt {
                    fs::write(
                        directory.path().join("receipt.json"),
                        json!({"owned_cleanup_closed":closed,"worker_pid":run.worker_pid,
                            "supervisor_pid":run.controller_pid,"process_group":if closed {run.group + 1} else {run.group}}).to_string(),
                    )
                    .unwrap();
                }
                let mut first = None;
                let result = outer_worker_acquire(
                    &run,
                    9,
                    &process.child,
                    &mut first,
                    &mut |identity, _| {
                        release_inert_child(&mut input, identity.pid)?;
                        fixture_observation(identity, true)
                    },
                    &mut exited_without_reap,
                );
                assert!(!worker_cleanup_receipt_closed(directory.path(), &run));
                let exit = process.child.try_wait().unwrap().unwrap();
                process.terminal = true;
                process.receipt.reaped = true;
                process.receipt.success = exit.success();
                assert!(matches!(result, Ok(OuterWorkerAcquisition::Exited)));
                assert_eq!(process.receipt.success, status == 0);
                assert!(first.is_none());
            }
        }
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
        let mut first = OuterSampleEvidence::default();
        assert!(
            outer_sample(
                directory.path(),
                &fleet.resources.run,
                0,
                &mut first,
                &fleet.processes[0].child,
            )
            .is_err()
        );
        assert!(first.first_rss_failure.is_none());
        assert!(first.first_frame_validation_failure.is_none());
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
            diagnostic: RssDiagnostic::default(),
        };
        let record = FirstRssFailure::new(&run, RssProducer::Worker, u64::MAX, candidate);
        let bytes = serde_json::to_vec(&record).unwrap();
        assert!(bytes.len() <= FIRST_RSS_CAP);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 16);
        assert_eq!(value["candidate_bytes"].as_u64(), Some(u64::MAX));
        let requested = FirstRssFailure {
            site: RssSite::WorkerFrameStopRequestedChild,
            ..record
        };
        let requested_bytes = serde_json::to_vec(&requested).unwrap();
        assert!(requested_bytes.len() <= FIRST_RSS_CAP);
        let requested_value: serde_json::Value = serde_json::from_slice(&requested_bytes).unwrap();
        assert_eq!(requested_value.as_object().unwrap().len(), 16);
        assert_eq!(requested_value["site"], "worker_frame_stop_requested_child");
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
