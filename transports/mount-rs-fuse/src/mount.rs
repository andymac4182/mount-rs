//! Native FUSE mounting and connection lifecycle.
//!
//! The protocol session is deliberately kept in [`crate::session::FuseSession`].
//! This module only owns the Linux boundary around it: validating a directory,
//! opening or receiving the FUSE device, issuing the mount operation, and
//! shutting the connection down again.
//!
//! Native FUSE is Linux-only.  macOS has no `/dev/fuse` with the Linux wire
//! protocol, so [`mount`] returns [`MountError::UnsupportedPlatform`] there;
//! the workspace's NFS transport is the portable native path.

#[cfg(target_os = "linux")]
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use mount_rs_core::{FsDriver, FsError};
#[cfg(target_os = "linux")]
use mount_rs_core::{S_IFDIR, S_IFMT};
#[cfg(target_os = "linux")]
use tokio::sync::Notify;

use crate::device::DEFAULT_MAX_FRAME;
#[cfg(target_os = "linux")]
use crate::device::FuseDevice;
#[cfg(target_os = "linux")]
use crate::session::FuseSession;

/// Which Linux mount authority to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountMode {
    /// Use the direct `mount(2)` path as root and `fusermount3` otherwise.
    Auto,
    /// Open the device in this process and call `mount(2)`.  The caller needs
    /// `CAP_SYS_ADMIN` (normally uid 0).
    Privileged,
    /// Ask `fusermount3`/`fusermount` to mount and return its device fd over a
    /// Unix socket using `SCM_RIGHTS`.
    Rootless,
}

/// Options for a native Linux FUSE mount.
#[derive(Debug, Clone)]
pub struct MountOptions {
    pub mode: MountMode,
    pub fsname: String,
    pub subtype: Option<String>,
    pub default_permissions: bool,
    pub allow_other: bool,
    pub read_only: bool,
    pub max_read: Option<u32>,
    /// Additional kernel/libfuse mount options. Each item is one option, not
    /// another argv element or a shell fragment.
    pub mount_options: Vec<String>,
    /// Device used by the privileged path. Rootless mounting never opens it.
    pub device: PathBuf,
    pub max_frame: usize,
    pub init_timeout: Duration,
    pub unmount_timeout: Duration,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            mode: MountMode::Auto,
            fsname: "mount-rs".to_owned(),
            subtype: None,
            default_permissions: true,
            allow_other: false,
            read_only: false,
            max_read: None,
            mount_options: Vec::new(),
            device: PathBuf::from("/dev/fuse"),
            max_frame: DEFAULT_MAX_FRAME,
            init_timeout: Duration::from_secs(10),
            unmount_timeout: Duration::from_secs(10),
        }
    }
}

/// Errors from validation, the native helper, or the session lifecycle.
#[derive(Debug)]
pub enum MountError {
    UnsupportedPlatform,
    InvalidMountpoint {
        path: PathBuf,
        reason: String,
    },
    InvalidOption(String),
    Driver(FsError),
    Io(io::Error),
    Protocol(crate::ProtocolError),
    Native(String),
    Timeout {
        operation: &'static str,
        after: Duration,
    },
    Lifecycle(String),
}

impl fmt::Display for MountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => f.write_str(
                "native Linux FUSE is unsupported on this platform; use the NFS transport on macOS",
            ),
            Self::InvalidMountpoint { path, reason } => {
                write!(f, "mountpoint '{}' is not usable: {reason}", path.display())
            }
            Self::InvalidOption(reason) => f.write_str(reason),
            Self::Driver(error) => write!(f, "driver root is not usable: {error}"),
            Self::Io(error) => error.fmt(f),
            Self::Protocol(error) => write!(f, "FUSE protocol error: {error}"),
            Self::Native(error) => f.write_str(error),
            Self::Timeout { operation, after } => {
                write!(
                    f,
                    "{operation} did not finish within {}ms",
                    after.as_millis()
                )
            }
            Self::Lifecycle(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for MountError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Driver(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for MountError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::ProtocolError> for MountError {
    fn from(error: crate::ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

/// A live native FUSE mount.
#[cfg(target_os = "linux")]
pub struct FuseMount {
    state: Arc<MountState>,
    mountpoint: PathBuf,
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub struct FuseMount;

#[cfg(target_os = "linux")]
impl fmt::Debug for FuseMount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FuseMount")
            .field("mountpoint", &self.mountpoint)
            .field("mode", &self.state.mode)
            .field("active", &self.is_active())
            .finish()
    }
}

#[cfg(target_os = "linux")]
impl FuseMount {
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    pub fn mode(&self) -> MountMode {
        self.state.mode
    }

    /// Whether the session task is still serving requests.
    pub fn is_active(&self) -> bool {
        self.state.active.load(Ordering::Acquire)
    }

    /// Wait until the device loop has destroyed its session and dropped the
    /// device descriptor.
    pub async fn wait_closed(&self) {
        loop {
            let notified = self.state.closed_notify.notified();
            tokio::pin!(notified);
            // Register before checking the state. `notify_waiters()` does not
            // retain a permit for a future created after the notification.
            notified.as_mut().enable();
            if self.state.closed.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Unmount this mount exactly once. Concurrent callers share the same
    /// operation; a failed graceful unmount leaves the mount live and permits
    /// a later retry.
    pub async fn unmount(&self) -> Result<(), MountError> {
        loop {
            let notified = self.state.unmount_notify.notified();
            tokio::pin!(notified);
            // Register before checking the result. `notify_waiters()` does not
            // retain a permit for a future created after the notification.
            notified.as_mut().enable();
            self.start_unmount();
            if let Some(result) = self
                .state
                .unmount_result
                .lock()
                .expect("mount result lock poisoned")
                .clone()
            {
                return result.map_err(MountError::Lifecycle);
            }
            notified.await;
        }
    }

    fn start_unmount(&self) {
        if !self.state.mounted.load(Ordering::Acquire) {
            let mut result = self
                .state
                .unmount_result
                .lock()
                .expect("mount result lock poisoned");
            if result.is_none() {
                *result = Some(Ok(()));
                self.state.unmount_notify.notify_waiters();
            }
            return;
        }
        if self.state.unmount_started.swap(true, Ordering::AcqRel) {
            return;
        }
        *self
            .state
            .unmount_result
            .lock()
            .expect("mount result lock poisoned") = None;
        let state = Arc::clone(&self.state);
        let spawn = tokio::runtime::Handle::try_current();
        if let Ok(handle) = spawn {
            handle.spawn(async move {
                let operation_state = Arc::clone(&state);
                let result = state
                    .perform_unmount()
                    .await
                    .map_err(|error| error.to_string());
                if result.is_err() && operation_state.mounted.load(Ordering::Acquire) {
                    operation_state
                        .unmount_started
                        .store(false, Ordering::Release);
                }
                *operation_state
                    .unmount_result
                    .lock()
                    .expect("mount result lock poisoned") = Some(result);
                operation_state.unmount_notify.notify_waiters();
            });
        } else {
            // A destructor can run after a runtime has gone away. We cannot
            // issue a blocking unmount there, but closing the session task's fd
            // still aborts the kernel connection and avoids an fd leak.
            state.request_stop();
            state.abort_task();
            *state
                .unmount_result
                .lock()
                .expect("mount result lock poisoned") = Some(Err(
                "cannot unmount after the Tokio runtime has stopped".to_owned(),
            ));
            state.unmount_notify.notify_waiters();
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for FuseMount {
    fn drop(&mut self) {
        if self.state.mounted.load(Ordering::Acquire)
            && !self.state.unmount_started.swap(true, Ordering::AcqRel)
        {
            let state = Arc::clone(&self.state);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = state.perform_unmount().await;
                });
            } else {
                state.request_stop();
                state.abort_task();
            }
        }
    }
}

/// Mount a driver at an existing directory.
///
/// No native operation is attempted until this function is called. On Linux,
/// the returned future resolves only after the kernel's `FUSE_INIT` exchange
/// has completed, so a successful call means the session is serving requests.
pub async fn mount(
    driver: Arc<dyn FsDriver>,
    mountpoint: impl AsRef<Path>,
    options: MountOptions,
) -> Result<FuseMount, MountError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (driver, mountpoint, options);
        Err(MountError::UnsupportedPlatform)
    }

    #[cfg(target_os = "linux")]
    {
        mount_linux(driver, mountpoint.as_ref(), options).await
    }
}

#[cfg(target_os = "linux")]
async fn mount_linux(
    driver: Arc<dyn FsDriver>,
    mountpoint: &Path,
    options: MountOptions,
) -> Result<FuseMount, MountError> {
    use std::os::fd::AsRawFd;

    let target = validate_mountpoint(mountpoint)?;
    validate_options(&options)?;
    if mounted_at(&target) {
        return Err(MountError::InvalidMountpoint {
            path: target,
            reason: "a filesystem is already mounted there".to_owned(),
        });
    }

    let root = driver.stat("/").await.map_err(MountError::Driver)?;
    if root.mode & S_IFMT != S_IFDIR {
        return Err(MountError::InvalidMountpoint {
            path: target,
            reason: format!(
                "the driver's root must be a directory (mode 0o{:o})",
                root.mode
            ),
        });
    }

    let uid = unsafe { libc::geteuid() as u32 };
    let gid = unsafe { libc::getegid() as u32 };
    let (mode, helper) = choose_mode(options.mode, uid)?;
    let fd = match mode {
        MountMode::Privileged => {
            let fd = open_fuse_fd(&options.device)?;
            if let Err(error) =
                mount_privileged(&target, &options, fd.as_raw_fd(), root.mode, uid, gid)
            {
                return Err(MountError::Native(format!(
                    "mount(2) failed for '{}': {error}",
                    target.display()
                )));
            }
            fd
        }
        MountMode::Rootless => {
            let helper = helper
                .as_deref()
                .ok_or_else(|| MountError::Native("no fusermount helper was found".to_owned()))?;
            mount_rootless(&target, &options, helper).await?
        }
        MountMode::Auto => unreachable!("choose_mode resolves Auto"),
    };

    let device = match FuseDevice::from_owned_fd(fd, options.max_frame) {
        Ok(device) => device,
        Err(error) => {
            force_unmount_async(mode, &target, helper.as_deref(), options.unmount_timeout).await;
            return Err(MountError::Io(error));
        }
    };
    let mut session = FuseSession::new(driver);
    // Keep the transport buffer and protocol decoder on the same boundary.
    // The default is one MiB, but callers may deliberately choose a smaller
    // frame ceiling for a constrained device.
    session.max_request = options.max_frame;
    let state = Arc::new(MountState::new(
        mode,
        target.clone(),
        options.clone(),
        helper,
    ));
    let task_state = Arc::clone(&state);
    let task = tokio::spawn(run_session(session, device, task_state));
    state.set_task(task);
    let mounted = FuseMount {
        state: Arc::clone(&state),
        mountpoint: target,
    };

    let ready = tokio::time::timeout(options.init_timeout, state.wait_ready()).await;
    match ready {
        Ok(Ok(())) => Ok(mounted),
        Ok(Err(error)) => {
            let _ = mounted.unmount().await;
            Err(MountError::Lifecycle(error))
        }
        Err(_) => {
            let timeout = options.init_timeout;
            let _ = mounted.unmount().await;
            Err(MountError::Timeout {
                operation: "FUSE_INIT",
                after: timeout,
            })
        }
    }
}

#[cfg(target_os = "linux")]
struct MountState {
    mode: MountMode,
    mountpoint: PathBuf,
    options: MountOptions,
    helper: Option<PathBuf>,
    active: AtomicBool,
    mounted: AtomicBool,
    closed: AtomicBool,
    ready: AtomicBool,
    stop: AtomicBool,
    unmount_started: AtomicBool,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    ready_notify: Notify,
    closed_notify: Notify,
    stop_notify: Notify,
    unmount_notify: Notify,
    unmount_result: Mutex<Option<Result<(), String>>>,
    transport_error: Mutex<Option<String>>,
}

#[cfg(target_os = "linux")]
impl MountState {
    fn new(
        mode: MountMode,
        mountpoint: PathBuf,
        options: MountOptions,
        helper: Option<PathBuf>,
    ) -> Self {
        Self {
            mode,
            mountpoint,
            options,
            helper,
            active: AtomicBool::new(true),
            mounted: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            unmount_started: AtomicBool::new(false),
            task: Mutex::new(None),
            ready_notify: Notify::new(),
            closed_notify: Notify::new(),
            stop_notify: Notify::new(),
            unmount_notify: Notify::new(),
            unmount_result: Mutex::new(None),
            transport_error: Mutex::new(None),
        }
    }

    fn set_task(&self, task: tokio::task::JoinHandle<()>) {
        *self.task.lock().expect("mount task lock poisoned") = Some(task);
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.stop_notify.notify_one();
    }

    fn abort_task(&self) {
        if let Some(task) = self.task.lock().expect("mount task lock poisoned").take() {
            task.abort();
        }
    }

    async fn wait_ready(&self) -> Result<(), String> {
        loop {
            let notified = self.ready_notify.notified();
            tokio::pin!(notified);
            // The session can notify while the state is being checked. Enable
            // the waiter first so that wakeup cannot be lost in that gap.
            notified.as_mut().enable();
            if self.ready.load(Ordering::Acquire) {
                return Ok(());
            }
            if self.closed.load(Ordering::Acquire) {
                return Err(self
                    .transport_error
                    .lock()
                    .expect("transport error lock poisoned")
                    .clone()
                    .unwrap_or_else(|| "FUSE session closed before FUSE_INIT".to_owned()));
            }
            notified.await;
        }
    }

    #[cfg(target_os = "linux")]
    async fn perform_unmount(self: Arc<Self>) -> Result<(), MountError> {
        let timeout = self.options.unmount_timeout;
        let result = match attempt_unmount(&self, timeout).await {
            UnmountAttempt::Done => Ok(()),
            UnmountAttempt::TimedOut => {
                force_unmount_async(self.mode, &self.mountpoint, self.helper.as_deref(), timeout)
                    .await;
                Err(MountError::Timeout {
                    operation: "unmount",
                    after: timeout,
                })
            }
            UnmountAttempt::Failed(_error) if !mounted_at(&self.mountpoint) => Ok(()),
            UnmountAttempt::Failed(error) => {
                self.unmount_started.store(false, Ordering::Release);
                return Err(error);
            }
        };

        self.request_stop();
        let task = self.task.lock().expect("mount task lock poisoned").take();
        if let Some(mut task) = task {
            match tokio::time::timeout(timeout, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.transport_error
                        .lock()
                        .expect("transport error lock poisoned")
                        .get_or_insert_with(|| {
                            format!("FUSE task failed during teardown: {error}")
                        });
                }
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        self.mounted.store(false, Ordering::Release);
        self.active.store(false, Ordering::Release);
        result
    }

    #[cfg(not(target_os = "linux"))]
    async fn perform_unmount(self: Arc<Self>) -> Result<(), MountError> {
        Err(MountError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
async fn run_session(mut session: FuseSession, device: FuseDevice, state: Arc<MountState>) {
    let mut failure = None;
    loop {
        if state.stop.load(Ordering::Acquire) {
            break;
        }
        let frame = tokio::select! {
            _ = state.stop_notify.notified() => break,
            result = device.read_frame() => result,
        };
        let frame = match frame {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                failure = Some(format!("FUSE device read failed: {error}"));
                break;
            }
        };
        let reply = match session.handle(&frame).await {
            Ok(reply) => reply,
            Err(error) => {
                failure = Some(error.to_string());
                break;
            }
        };
        if let Some(reply) = reply
            && let Err(error) = device.write_frame(&reply).await
        {
            failure = Some(format!("FUSE device write failed: {error}"));
            break;
        }
        if session.negotiated.is_some() && !state.ready.swap(true, Ordering::AcqRel) {
            state.ready_notify.notify_waiters();
        }
    }
    session.destroy().await;
    if let Some(error) = failure {
        *state
            .transport_error
            .lock()
            .expect("transport error lock poisoned") = Some(error);
    }
    state.active.store(false, Ordering::Release);
    state.closed.store(true, Ordering::Release);
    state.closed_notify.notify_waiters();
    state.ready_notify.notify_waiters();
}

#[cfg(target_os = "linux")]
fn validate_mountpoint(path: &Path) -> Result<PathBuf, MountError> {
    let target = std::fs::canonicalize(path).map_err(|error| MountError::InvalidMountpoint {
        path: path.to_owned(),
        reason: error.to_string(),
    })?;
    let metadata = std::fs::metadata(&target).map_err(|error| MountError::InvalidMountpoint {
        path: target.clone(),
        reason: error.to_string(),
    })?;
    if !metadata.is_dir() {
        return Err(MountError::InvalidMountpoint {
            path: target,
            reason: "not a directory".to_owned(),
        });
    }
    Ok(target)
}

#[cfg(target_os = "linux")]
fn validate_options(options: &MountOptions) -> Result<(), MountError> {
    validate_token("fsname", &options.fsname)?;
    if let Some(subtype) = &options.subtype {
        validate_token("subtype", subtype)?;
    }
    if options.max_frame < crate::IN_HEADER_SIZE {
        return Err(MountError::InvalidOption(
            "max_frame is smaller than the FUSE request header".to_owned(),
        ));
    }
    if options.max_read == Some(0) {
        return Err(MountError::InvalidOption(
            "max_read must be greater than zero".to_owned(),
        ));
    }
    if options.init_timeout.is_zero() || options.unmount_timeout.is_zero() {
        return Err(MountError::InvalidOption(
            "mount timeouts must be non-zero".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_token(name: &str, value: &str) -> Result<(), MountError> {
    if value.is_empty()
        || value.starts_with('-')
        || value
            .chars()
            .any(|c| c == ',' || c == '=' || c.is_whitespace())
    {
        return Err(MountError::InvalidOption(format!(
            "{name} may not be empty, begin with '-' or contain a comma, equals sign or whitespace"
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn choose_mode(mode: MountMode, uid: u32) -> Result<(MountMode, Option<PathBuf>), MountError> {
    match mode {
        MountMode::Privileged => Ok((MountMode::Privileged, None)),
        MountMode::Rootless => Ok((MountMode::Rootless, Some(find_helper()?))),
        MountMode::Auto if uid == 0 => Ok((MountMode::Privileged, None)),
        MountMode::Auto => Ok((MountMode::Rootless, Some(find_helper()?))),
    }
}

#[cfg(target_os = "linux")]
fn find_helper() -> Result<PathBuf, MountError> {
    let names = ["fusermount3", "fusermount"];
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for name in names {
            let candidate = directory.join(name);
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    for directory in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        for name in names {
            let candidate = Path::new(directory).join(name);
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Err(MountError::Native(
        "rootless FUSE mounting requires fusermount3 or fusermount on PATH".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn open_fuse_fd(path: &Path) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    let path = path_to_cstring(path)?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is freshly returned by open and is transferred into the
    // OwnedFd immediately.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn mount_privileged(
    mountpoint: &Path,
    options: &MountOptions,
    fd: std::os::fd::RawFd,
    root_mode: u32,
    uid: u32,
    gid: u32,
) -> io::Result<()> {
    let source = path_to_cstring(Path::new(&options.fsname))?;
    let target = path_to_cstring(mountpoint)?;
    let fstype = std::ffi::CString::new("fuse").expect("literal has no NUL");
    let data = mount_data(options, Some((fd, root_mode, uid, gid)))?;
    let flags = if options.read_only {
        libc::MS_RDONLY
    } else {
        0
    };
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags,
            data.as_ptr().cast::<libc::c_void>(),
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
async fn mount_rootless(
    mountpoint: &Path,
    options: &MountOptions,
    helper: &Path,
) -> Result<std::os::fd::OwnedFd, MountError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    let (ours, theirs) = socket_pair().map_err(MountError::Io)?;
    let child_fd = theirs.as_raw_fd();
    let comm_fd = 3;
    let mount_options = mount_data(options, None).map_err(MountError::Io)?;
    let mut command = tokio::process::Command::new(helper);
    command
        .args([
            "-o",
            mount_options
                .as_c_str()
                .to_str()
                .expect("mount options are UTF-8"),
            "--",
        ])
        .arg(mountpoint)
        .env("_FUSE_COMMFD", comm_fd.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // The helper expects the fd number named by _FUSE_COMMFD to survive exec.
    // socketpair uses CLOEXEC, so dup2 is needed even when the original number
    // already happens to be three.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::dup2(child_fd, comm_fd) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(comm_fd, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            if child_fd != comm_fd {
                libc::close(child_fd);
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| {
        MountError::Native(format!("could not run {}: {error}", helper.display()))
    })?;
    let mut stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        if let Some(stderr) = &mut stderr {
            let _ = stderr.read_to_end(&mut bytes).await;
        }
        bytes
    });
    let status = match tokio::time::timeout(options.init_timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            let _ = child.start_kill();
            stderr_task.abort();
            drop(ours);
            drop(theirs);
            return Err(MountError::Native(format!(
                "{} wait failed: {error}",
                helper.display()
            )));
        }
        Err(_) => {
            // A helper can be stuck in mount(2), so do not await its reaping
            // here. Sending SIGKILL and dropping the child is bounded even
            // when the helper is in an uninterruptible kernel wait.
            let _ = child.start_kill();
            stderr_task.abort();
            drop(ours);
            drop(theirs);
            drop(child);
            let _ = undo_rootless_mount(mountpoint, helper, options.init_timeout).await;
            return Err(MountError::Timeout {
                operation: "rootless FUSE mount helper",
                after: options.init_timeout,
            });
        }
    };
    drop(theirs);
    let stderr = stderr_task
        .await
        .map_err(|error| MountError::Lifecycle(format!("stderr worker failed: {error}")))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(MountError::Native(format!(
            "{} failed to mount '{}': {}",
            helper.display(),
            mountpoint.display(),
            stderr.trim()
        )));
    }
    match recv_fd_async(ours, options.init_timeout).await {
        Ok(fd) => Ok(fd),
        Err(error) => {
            // A helper can theoretically mount successfully and exit without
            // sending its fd. Do not leave an unservable mount behind.  Use a
            // normal unmount first: the mount has not been handed to a
            // session, so this should quiesce immediately and gives the
            // helper a definite answer.  A lazy detach is only a last resort
            // for a genuinely stuck kernel teardown.
            if let Some(cleanup) =
                undo_rootless_mount(mountpoint, helper, options.init_timeout).await
            {
                return Err(MountError::Native(format!(
                    "could not receive the FUSE device descriptor: {error}; {cleanup}"
                )));
            }
            Err(MountError::Io(error))
        }
    }
}

#[cfg(target_os = "linux")]
async fn undo_rootless_mount(
    mountpoint: &Path,
    helper: &Path,
    timeout: Duration,
) -> Option<String> {
    let normal = run_child(
        helper.as_os_str(),
        &[
            PathBuf::from("-u"),
            PathBuf::from("--"),
            mountpoint.to_owned(),
        ],
        timeout,
    )
    .await;
    if child_succeeded(&normal) {
        return None;
    }
    let normal_detail = child_failure_detail(helper, &normal);
    let lazy = run_child(
        helper.as_os_str(),
        &[
            PathBuf::from("-u"),
            PathBuf::from("-z"),
            PathBuf::from("--"),
            mountpoint.to_owned(),
        ],
        timeout,
    )
    .await;
    if child_succeeded(&lazy) {
        return None;
    }
    Some(format!(
        "could not clean up the rootless mount at '{}'; normal unmount: {}; lazy unmount: {}",
        mountpoint.display(),
        normal_detail,
        child_failure_detail(helper, &lazy)
    ))
}

#[cfg(target_os = "linux")]
fn child_succeeded(result: &Result<Option<ChildOutcome>, MountError>) -> bool {
    matches!(result, Ok(Some(outcome)) if outcome.status.success())
}

#[cfg(target_os = "linux")]
fn child_failure_detail(
    helper: &Path,
    result: &Result<Option<ChildOutcome>, MountError>,
) -> String {
    match result {
        Ok(None) => format!("{} timed out", helper.display()),
        Ok(Some(outcome)) => {
            if outcome.stderr.is_empty() {
                format!("{} exited with status {}", helper.display(), outcome.status)
            } else {
                format!(
                    "{} exited with status {}: {}",
                    helper.display(),
                    outcome.status,
                    outcome.stderr
                )
            }
        }
        Err(error) => error.to_string(),
    }
}

#[cfg(target_os = "linux")]
fn mount_data(
    options: &MountOptions,
    privileged: Option<(std::os::fd::RawFd, u32, u32, u32)>,
) -> io::Result<std::ffi::CString> {
    let mut parts = Vec::new();
    if let Some((fd, root_mode, uid, gid)) = privileged {
        parts.push(format!("fd={fd}"));
        parts.push(format!("rootmode={root_mode:o}"));
        parts.push(format!("user_id={uid}"));
        parts.push(format!("group_id={gid}"));
    }
    parts.push(format!("fsname={}", options.fsname));
    if options.default_permissions {
        parts.push("default_permissions".to_owned());
    }
    if options.allow_other {
        parts.push("allow_other".to_owned());
    }
    if options.read_only {
        parts.push("ro".to_owned());
    }
    if let Some(max_read) = options.max_read {
        parts.push(format!("max_read={max_read}"));
    }
    if let Some(subtype) = &options.subtype {
        parts.push(format!("subtype={subtype}"));
    }
    parts.extend(options.mount_options.iter().cloned());
    std::ffi::CString::new(parts.join(",")).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "FUSE mount options contain an embedded NUL",
        )
    })
}

#[cfg(target_os = "linux")]
enum UnmountAttempt {
    Done,
    TimedOut,
    Failed(MountError),
}

#[cfg(target_os = "linux")]
struct ChildOutcome {
    status: std::process::ExitStatus,
    stderr: String,
}

#[cfg(target_os = "linux")]
async fn run_child(
    program: &OsStr,
    arguments: &[PathBuf],
    timeout: Duration,
) -> Result<Option<ChildOutcome>, MountError> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        MountError::Native(format!(
            "could not run {}: {error}",
            program.to_string_lossy()
        ))
    })?;
    let mut stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        if let Some(stderr) = &mut stderr {
            let _ = stderr.read_to_end(&mut bytes).await;
        }
        bytes
    });
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let _ = stderr_task.await;
            return Err(MountError::Native(format!(
                "{} wait failed: {error}",
                program.to_string_lossy()
            )));
        }
        Err(_) => {
            // Do not await child reaping: a helper blocked in an uninterruptible
            // kernel mount/umount syscall cannot be reaped on our deadline.
            // `kill_on_drop` supplies the same SIGKILL backstop, while closing
            // the stderr task's pipe ensures this process does not retain a
            // child stream after the timeout.
            let _ = child.start_kill();
            stderr_task.abort();
            return Ok(None);
        }
    };
    let stderr = stderr_task
        .await
        .map_err(|error| MountError::Lifecycle(format!("stderr worker failed: {error}")))?;
    Ok(Some(ChildOutcome {
        status,
        stderr: String::from_utf8_lossy(&stderr).trim().to_owned(),
    }))
}

#[cfg(target_os = "linux")]
async fn attempt_unmount(state: &MountState, timeout: Duration) -> UnmountAttempt {
    let (program, arguments) = match state.mode {
        MountMode::Privileged => (OsStr::new("umount"), vec![state.mountpoint.clone()]),
        MountMode::Rootless => {
            let Some(helper) = state.helper.as_deref() else {
                return UnmountAttempt::Failed(MountError::Native(
                    "rootless mount helper path was lost".to_owned(),
                ));
            };
            (
                helper.as_os_str(),
                vec![
                    PathBuf::from("-u"),
                    PathBuf::from("--"),
                    state.mountpoint.clone(),
                ],
            )
        }
        MountMode::Auto => unreachable!("mounted mode is resolved before state creation"),
    };
    match run_child(program, &arguments, timeout).await {
        Ok(None) => UnmountAttempt::TimedOut,
        Ok(Some(outcome)) if outcome.status.success() => UnmountAttempt::Done,
        Ok(Some(outcome)) => UnmountAttempt::Failed(MountError::Native(format!(
            "unmount of '{}' failed (status {}): {}",
            state.mountpoint.display(),
            outcome.status,
            if outcome.stderr.is_empty() {
                "no diagnostic output"
            } else {
                &outcome.stderr
            }
        ))),
        Err(error) => UnmountAttempt::Failed(error),
    }
}

#[cfg(target_os = "linux")]
async fn force_unmount_async(
    mode: MountMode,
    mountpoint: &Path,
    helper: Option<&Path>,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    match mode {
        MountMode::Rootless => {
            if let Some(helper) = helper {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let _ = run_child(
                    helper.as_os_str(),
                    &[
                        PathBuf::from("-u"),
                        PathBuf::from("-z"),
                        PathBuf::from("--"),
                        mountpoint.to_owned(),
                    ],
                    remaining,
                )
                .await;
            }
        }
        MountMode::Privileged => {
            for arguments in [
                vec![PathBuf::from("-f"), mountpoint.to_owned()],
                vec![PathBuf::from("-l"), mountpoint.to_owned()],
            ] {
                if !mounted_at(mountpoint) {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let _ = run_child(OsStr::new("umount"), &arguments, remaining).await;
            }
        }
        MountMode::Auto => unreachable!("mounted mode is resolved before state creation"),
    }
}

#[cfg(target_os = "linux")]
fn socket_pair() -> io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::FromRawFd;
    let mut fds = [-1; 2];
    let result = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socketpair initialized both descriptors and ownership is
    // transferred immediately.
    Ok((
        unsafe { std::os::fd::OwnedFd::from_raw_fd(fds[0]) },
        unsafe { std::os::fd::OwnedFd::from_raw_fd(fds[1]) },
    ))
}

#[cfg(target_os = "linux")]
async fn recv_fd_async(
    socket: std::os::fd::OwnedFd,
    timeout: Duration,
) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd;

    set_nonblocking(socket.as_raw_fd())?;
    let socket = tokio::io::unix::AsyncFd::new(socket)?;
    let receive = async {
        loop {
            let mut ready = socket.readable().await?;
            match ready.try_io(|inner| recv_fd(inner.get_ref().as_raw_fd())) {
                Ok(Ok(fd)) => return Ok(fd),
                Ok(Err(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Ok(Err(error)) => return Err(error),
                Err(_would_block) => {}
            }
        }
    };
    tokio::time::timeout(timeout, receive).await.map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out waiting for fusermount to send the FUSE device",
        )
    })?
}

#[cfg(target_os = "linux")]
fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn recv_fd(socket: std::os::fd::RawFd) -> io::Result<std::os::fd::OwnedFd> {
    use std::mem::{size_of, zeroed};
    use std::os::fd::{FromRawFd, RawFd};
    use std::ptr;

    let mut byte = [0_u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast::<libc::c_void>(),
        iov_len: byte.len(),
    };
    let control_len = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) as usize };
    let mut control = vec![0_u8; control_len];
    // SAFETY: msghdr/iovec are initialized below before recvmsg reads them.
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast::<libc::c_void>();
    message.msg_controllen = control.len();
    let result = unsafe { libc::recvmsg(socket, &mut message, libc::MSG_CMSG_CLOEXEC) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "fusermount closed the fd-passing socket without sending a device",
        ));
    }
    let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !header.is_null() {
        let header_ref = unsafe { &*header };
        if header_ref.cmsg_level == libc::SOL_SOCKET
            && header_ref.cmsg_type == libc::SCM_RIGHTS
            && header_ref.cmsg_len >= unsafe { libc::CMSG_LEN(size_of::<RawFd>() as u32) } as usize
        {
            let data = unsafe { libc::CMSG_DATA(header) };
            let fd = unsafe { ptr::read_unaligned(data.cast::<RawFd>()) };
            if fd < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fusermount sent an invalid device descriptor",
                ));
            }
            // SAFETY: the descriptor was freshly received from SCM_RIGHTS and
            // ownership is transferred to this OwnedFd.
            return Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        }
        header = unsafe { libc::CMSG_NXTHDR(&message, header) };
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "fusermount reply did not contain an SCM_RIGHTS descriptor",
    ))
}

#[cfg(target_os = "linux")]
fn path_to_cstring(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "FUSE path contains an embedded NUL",
        )
    })
}

#[cfg(target_os = "linux")]
fn mounted_at(target: &Path) -> bool {
    let Ok(table) = std::fs::read_to_string("/proc/self/mounts") else {
        return false;
    };
    let target = target.to_string_lossy();
    table.lines().any(|line| {
        line.split_whitespace()
            .nth(1)
            .map(unescape_mount_path)
            .is_some_and(|path| path == target)
    })
}

#[cfg(target_os = "linux")]
fn unescape_mount_path(path: &str) -> String {
    path.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_for_a_single_mount() {
        let options = MountOptions::default();
        assert_eq!(options.mode, MountMode::Auto);
        assert!(options.default_permissions);
        assert!(!options.allow_other);
        assert_eq!(options.device, Path::new("/dev/fuse"));
    }

    #[test]
    fn mode_and_option_validation_have_no_native_side_effects() {
        #[cfg(target_os = "linux")]
        {
            assert!(validate_token("fsname", "plain-name").is_ok());
            assert!(validate_token("fsname", "bad,allow_other").is_err());
            assert!(validate_token("subtype", "bad=option").is_err());
            assert!(validate_token("fsname", "-option").is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mount_path_unescape_matches_proc_mounts() {
        assert_eq!(unescape_mount_path("/tmp/a\\040b\\134c"), "/tmp/a b\\c");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn child_timeout_returns_without_waiting_for_a_stuck_helper() {
        let started = std::time::Instant::now();
        let result = run_child(
            OsStr::new("/bin/sh"),
            &[PathBuf::from("-c"), PathBuf::from("while true; do :; done")],
            Duration::from_millis(25),
        )
        .await
        .expect("helper should spawn");
        assert!(
            result.is_none(),
            "a timed-out helper must not look successful"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout path waited for helper reaping"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn child_failure_preserves_helper_diagnostic() {
        let result = run_child(
            OsStr::new("/bin/sh"),
            &[
                PathBuf::from("-c"),
                PathBuf::from("printf helper-failure >&2; exit 7"),
            ],
            Duration::from_secs(1),
        )
        .await
        .expect("helper should spawn")
        .expect("helper should finish before the deadline");
        assert_eq!(result.status.code(), Some(7));
        assert_eq!(result.stderr, "helper-failure");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn received_device_descriptor_is_close_on_exec_and_async_safe() {
        use std::fs::File;
        use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};

        let (ours, theirs) = socket_pair().expect("socketpair");
        let source = File::open("/dev/null").expect("/dev/null");
        let source_fd = source.as_raw_fd();
        let sender = std::thread::spawn(move || {
            send_fd(theirs.as_raw_fd(), source_fd).expect("send SCM_RIGHTS descriptor");
            drop(theirs);
            drop(source);
        });
        let received = recv_fd_async(ours, Duration::from_secs(1))
            .await
            .expect("receive SCM_RIGHTS descriptor");
        sender.join().expect("sender thread");
        let flags = unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        let _file = unsafe { File::from_raw_fd(received.into_raw_fd()) };
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn descriptor_receive_times_out_when_helper_sends_nothing() {
        let (ours, _theirs) = socket_pair().expect("socketpair");
        let error = recv_fd_async(ours, Duration::from_millis(20))
            .await
            .expect_err("descriptor receive must be bounded");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[cfg(target_os = "linux")]
    fn send_fd(socket: std::os::fd::RawFd, fd: std::os::fd::RawFd) -> io::Result<()> {
        use std::mem::{size_of, zeroed};
        use std::ptr;

        let mut byte = [0_u8; 1];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast::<libc::c_void>(),
            iov_len: byte.len(),
        };
        let control_len =
            unsafe { libc::CMSG_SPACE(size_of::<std::os::fd::RawFd>() as u32) as usize };
        let mut control = vec![0_u8; control_len];
        // SAFETY: msghdr/iovec are initialized before sendmsg, and the
        // control buffer is large enough for one RawFd.
        let mut message: libc::msghdr = unsafe { zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast::<libc::c_void>();
        message.msg_controllen = control.len();
        let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        if header.is_null() {
            return Err(io::Error::other("could not allocate an SCM_RIGHTS header"));
        }
        unsafe {
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size_of::<std::os::fd::RawFd>() as u32) as usize;
            ptr::write_unaligned(libc::CMSG_DATA(header).cast::<std::os::fd::RawFd>(), fd);
        }
        let sent = unsafe { libc::sendmsg(socket, &message, libc::MSG_NOSIGNAL) };
        if sent < 0 {
            Err(io::Error::last_os_error())
        } else if sent == 1 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "SCM_RIGHTS test message was not sent in full",
            ))
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn mount_is_explicitly_unsupported_without_touching_the_path() {
        let result = mount(
            Arc::new(mount_rs_core::MemoryFs::empty()),
            "/definitely/not/a/native/fuse/mount",
            MountOptions::default(),
        )
        .await;
        assert!(matches!(result, Err(MountError::UnsupportedPlatform)));
    }
}
