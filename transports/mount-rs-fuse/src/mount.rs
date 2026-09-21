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

#[cfg(target_os = "linux")]
use futures_util::FutureExt;
use mount_rs_core::{FsDriver, FsError};
#[cfg(target_os = "linux")]
use mount_rs_core::{S_IFDIR, S_IFMT};
#[cfg(target_os = "linux")]
use tokio::sync::Notify;

use crate::device::DEFAULT_MAX_FRAME;
#[cfg(target_os = "linux")]
use crate::device::FuseDevice;
#[cfg(target_os = "linux")]
use crate::session::{FuseSession, FuseSessionOptions};

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

/// The native FUSE transport phase that terminated a live session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FuseTransportErrorKind {
    Read,
    Protocol,
    Write,
    Task,
}

/// An owned terminal failure from the native FUSE device/session loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuseTransportError {
    pub kind: FuseTransportErrorKind,
    pub message: String,
    pub raw_os_error: Option<i32>,
}

/// Synchronous callback invoked once for the first terminal native-session
/// failure. The callback receives an owned value and never borrows the device,
/// session, or mount lifecycle state.
pub type FuseTransportErrorHook = Arc<dyn Fn(FuseTransportError) + Send + Sync + 'static>;

/// Optional hooks for [`FuseMount`]. Kept separate from [`MountOptions`] so
/// existing struct literals remain source-compatible.
#[derive(Clone, Default)]
pub struct FuseMountHooks {
    pub on_transport_error: Option<FuseTransportErrorHook>,
}

#[cfg(target_os = "linux")]
impl FuseTransportError {
    fn from_io(kind: FuseTransportErrorKind, error: &io::Error) -> Self {
        Self {
            kind,
            message: error.to_string(),
            raw_os_error: error.raw_os_error(),
        }
    }

    fn from_message(kind: FuseTransportErrorKind, message: String) -> Self {
        Self {
            kind,
            message,
            raw_os_error: None,
        }
    }
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
    /// Maximum bytes accepted from one native FUSE device read. Native Linux
    /// mounts require enough room for a modern WRITE frame plus one page.
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

// Native mounts only accept modern protocol sessions (7.12+), whose WRITE
// request body is 40 bytes. FUSE negotiation floors max_write at one page, so
// the receive frame must fit the request header, that body, and one page of
// payload. Keeping this floor at the mount boundary prevents INIT from
// advertising a write frame that FuseDevice would reject as oversized.
#[cfg(target_os = "linux")]
const MIN_NATIVE_MAX_FRAME: usize = crate::IN_HEADER_SIZE + 40 + crate::FUSE_PAGE_SIZE;

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
        // Match the mount lifecycle contract used by the other native
        // transports: active becomes false as soon as teardown owns the
        // operation, not only after the kernel helper returns or the session
        // task happens to observe the stop request.
        self.state.active.store(false, Ordering::Release);
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
                    // A graceful helper failure leaves the mount live and
                    // retryable. Publish that fact so a later attempt does
                    // not leave an otherwise healthy mount permanently
                    // reported as inactive.
                    operation_state.active.store(true, Ordering::Release);
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

impl FuseMount {
    /// The configured `fsname` exposed as the native mount source. Unsupported
    /// platforms have no native FUSE source.
    pub fn source(&self) -> Option<&str> {
        #[cfg(target_os = "linux")]
        {
            Some(self.state.options.fsname.as_str())
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for FuseMount {
    fn drop(&mut self) {
        if self.state.mounted.load(Ordering::Acquire)
            && !self.state.unmount_started.swap(true, Ordering::AcqRel)
        {
            self.state.active.store(false, Ordering::Release);
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
    mount_with_hooks(driver, mountpoint, options, FuseMountHooks::default()).await
}

/// Mount a driver with native-session lifecycle hooks.
pub async fn mount_with_hooks(
    driver: Arc<dyn FsDriver>,
    mountpoint: impl AsRef<Path>,
    options: MountOptions,
    hooks: FuseMountHooks,
) -> Result<FuseMount, MountError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (driver, mountpoint, options, hooks);
        Err(MountError::UnsupportedPlatform)
    }

    #[cfg(target_os = "linux")]
    {
        mount_linux(driver, mountpoint.as_ref(), options, hooks).await
    }
}

#[cfg(target_os = "linux")]
async fn mount_linux(
    driver: Arc<dyn FsDriver>,
    mountpoint: &Path,
    options: MountOptions,
    hooks: FuseMountHooks,
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
    // Keep the transport buffer and protocol decoder on the same boundary.
    // The default is one MiB, but callers may deliberately choose a smaller
    // frame ceiling for a constrained device.
    let session = FuseSession::with_options(
        driver,
        FuseSessionOptions {
            max_request: options.max_frame,
            ..FuseSessionOptions::default()
        },
    );
    let state = Arc::new(MountState::new(
        mode,
        target.clone(),
        options.clone(),
        helper,
        hooks,
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
    transport_error_reported: AtomicBool,
    hooks: FuseMountHooks,
}

#[cfg(target_os = "linux")]
impl MountState {
    fn new(
        mode: MountMode,
        mountpoint: PathBuf,
        options: MountOptions,
        helper: Option<PathBuf>,
        hooks: FuseMountHooks,
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
            transport_error_reported: AtomicBool::new(false),
            hooks,
        }
    }

    fn record_transport_error(&self, error: FuseTransportError) {
        self.transport_error
            .lock()
            .expect("transport error lock poisoned")
            .get_or_insert_with(|| error.message.clone());
        if self.transport_error_reported.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(hook) = &self.hooks.on_transport_error {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook(error)));
        }
    }

    fn set_task(&self, task: tokio::task::JoinHandle<()>) {
        *self.task.lock().expect("mount task lock poisoned") = Some(task);
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.stop_notify.notify_waiters();
    }

    fn abort_task(&self) {
        if let Some(task) = self.task.lock().expect("mount task lock poisoned").take() {
            task.abort();
        }
        // Aborting the owner task drops the device descriptor without giving
        // `run_session` a chance to publish its normal terminal state. Keep
        // wait_closed() and lifecycle observers from waiting forever on that
        // bounded fallback path.
        self.mark_closed();
    }

    fn mark_closed(&self) {
        self.active.store(false, Ordering::Release);
        self.closed.store(true, Ordering::Release);
        self.closed_notify.notify_waiters();
        self.ready_notify.notify_waiters();
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
        let mut forced_deadline = None;
        let result = match attempt_unmount(&self, timeout).await {
            UnmountAttempt::Done => Ok(()),
            UnmountAttempt::TimedOut => {
                // A graceful native unmount can wait for an in-flight kernel
                // request to finish. Cancel the session before asking for a
                // lazy detach so a blocked backend future cannot deadlock the
                // forced phase behind the same request.
                self.request_stop();
                let deadline = Instant::now() + timeout;
                let task = self.task.lock().expect("mount task lock poisoned").take();
                let force = force_unmount_until(
                    self.mode,
                    &self.mountpoint,
                    self.helper.as_deref(),
                    deadline,
                );
                if let Some(task) = task {
                    // The lazy helper may itself wait for the FUSE device to
                    // close. Drain the stopped session first so the helper
                    // observes a closed descriptor instead of consuming the
                    // entire forced-teardown deadline while the session still
                    // owns it.
                    drain_session_task(&self, task, Some(deadline)).await;
                }
                force.await;
                forced_deadline = Some(deadline);
                self.record_transport_error(FuseTransportError::from_message(
                    FuseTransportErrorKind::Task,
                    format!(
                        "FUSE unmount of '{}' exceeded {}ms; forced teardown was requested",
                        self.mountpoint.display(),
                        timeout.as_millis()
                    ),
                ));
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
        if let Some(task) = task {
            drain_session_task(&self, task, forced_deadline).await;
        }
        // A normal task exit publishes this state from `run_session`; the
        // timeout/cancellation branches above do not necessarily get there.
        // Make the terminal state idempotent so wait_closed() is guaranteed
        // to complete after every bounded teardown path.
        self.mark_closed();
        self.mounted.store(false, Ordering::Release);
        result
    }

    #[cfg(not(target_os = "linux"))]
    async fn perform_unmount(self: Arc<Self>) -> Result<(), MountError> {
        Err(MountError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
async fn drain_session_task(
    state: &MountState,
    mut task: tokio::task::JoinHandle<()>,
    deadline: Option<Instant>,
) {
    // A forced unmount has already consumed part of its phase budget while
    // running the abort/lazy-detach ladder. Drain the session only for the
    // time left in that same budget; otherwise a stuck task turns the
    // documented two-phase bound into a third full timeout.
    let remaining = deadline
        .map(|deadline| deadline.saturating_duration_since(Instant::now()))
        .unwrap_or(state.options.unmount_timeout);
    if remaining.is_zero() {
        task.abort();
        let _ = task.await;
    } else {
        match tokio::time::timeout(remaining, &mut task).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                state.record_transport_error(FuseTransportError::from_message(
                    FuseTransportErrorKind::Task,
                    format!("FUSE task failed during teardown: {error}"),
                ));
            }
            Err(_) => {
                task.abort();
                let _ = task.await;
            }
        }
    }
}

#[cfg(target_os = "linux")]
async fn run_session(mut session: FuseSession, device: FuseDevice, state: Arc<MountState>) {
    let device = Arc::new(device);
    let mut failure = match std::panic::AssertUnwindSafe(run_session_loop(
        &mut session,
        Arc::clone(&device),
        Arc::clone(&state),
    ))
    .catch_unwind()
    .await
    {
        Ok(failure) => failure,
        Err(_) => Some(FuseTransportError::from_message(
            FuseTransportErrorKind::Task,
            "FUSE session task panicked".to_owned(),
        )),
    };

    let cleanup = tokio::time::timeout(
        state.options.unmount_timeout,
        std::panic::AssertUnwindSafe(session.destroy()).catch_unwind(),
    )
    .await;
    match cleanup {
        Ok(Ok(())) => {}
        Ok(Err(_)) => {
            failure.get_or_insert_with(|| {
                FuseTransportError::from_message(
                    FuseTransportErrorKind::Task,
                    "FUSE session cleanup panicked".to_owned(),
                )
            });
        }
        Err(_) => {
            failure.get_or_insert_with(|| {
                FuseTransportError::from_message(
                    FuseTransportErrorKind::Task,
                    format!(
                        "FUSE session cleanup did not finish within {}ms",
                        state.options.unmount_timeout.as_millis()
                    ),
                )
            });
        }
    }

    if let Some(error) = failure {
        state.record_transport_error(error);
    }
    state.mark_closed();
}

#[cfg(target_os = "linux")]
const MAX_PARALLEL_READS: usize = 16;

#[cfg(target_os = "linux")]
const READ_TASK_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(target_os = "linux")]
type ReadTaskResult = (u64, Result<(), FuseTransportError>);

#[cfg(target_os = "linux")]
async fn write_reply_until_stop(
    device: Arc<FuseDevice>,
    writer: Arc<tokio::sync::Mutex<()>>,
    state: Arc<MountState>,
    reply: Vec<u8>,
) -> Result<(), FuseTransportError> {
    if state.stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let _guard = tokio::select! {
        _ = state.stop_notify.notified() => return Ok(()),
        guard = writer.lock() => guard,
    };
    if state.stop.load(Ordering::Acquire) {
        return Ok(());
    }
    tokio::select! {
        _ = state.stop_notify.notified() => Ok(()),
        result = device.write_frame(&reply) => result.map_err(|error| {
            FuseTransportError::from_io(FuseTransportErrorKind::Write, &error)
        }),
    }
}

#[cfg(target_os = "linux")]
fn read_task_join_error(error: tokio::task::JoinError) -> FuseTransportError {
    FuseTransportError::from_message(
        FuseTransportErrorKind::Task,
        if error.is_panic() {
            "FUSE read task panicked".to_owned()
        } else {
            format!("FUSE read task failed: {error}")
        },
    )
}

#[cfg(target_os = "linux")]
fn read_worker_drain_timeout_error() -> FuseTransportError {
    FuseTransportError::from_message(
        FuseTransportErrorKind::Task,
        format!(
            "FUSE read worker did not cancel within {}ms",
            READ_TASK_DRAIN_TIMEOUT.as_millis()
        ),
    )
}

#[cfg(target_os = "linux")]
async fn drain_read_tasks(
    read_tasks: &mut tokio::task::JoinSet<ReadTaskResult>,
    in_flight: &mut std::collections::HashMap<u64, tokio::task::AbortHandle>,
) -> Option<FuseTransportError> {
    while let Some(task) = read_tasks.join_next().await {
        match task {
            Ok((unique, Ok(()))) => {
                in_flight.remove(&unique);
            }
            Ok((unique, Err(error))) => {
                in_flight.remove(&unique);
                return Some(error);
            }
            Err(error) if error.is_cancelled() => {}
            Err(error) => {
                in_flight.clear();
                return Some(read_task_join_error(error));
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
async fn run_session_loop(
    session: &mut FuseSession,
    device: Arc<FuseDevice>,
    state: Arc<MountState>,
) -> Option<FuseTransportError> {
    let mut failure = None;
    let writer = Arc::new(tokio::sync::Mutex::new(()));
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_PARALLEL_READS));
    let mut read_tasks = tokio::task::JoinSet::<ReadTaskResult>::new();
    let mut in_flight: std::collections::HashMap<u64, tokio::task::AbortHandle> =
        std::collections::HashMap::new();
    loop {
        if state.stop.load(Ordering::Acquire) {
            break;
        }
        let frame = tokio::select! {
            _ = state.stop_notify.notified() => break,
            task = read_tasks.join_next(), if !read_tasks.is_empty() => {
                match task {
                    Some(Ok((unique, Ok(())))) => {
                        in_flight.remove(&unique);
                    }
                    Some(Ok((unique, Err(error)))) => {
                        in_flight.remove(&unique);
                        failure = Some(error);
                        break;
                    }
                    None => {}
                    Some(Err(error)) if error.is_cancelled() => {}
                    Some(Err(error)) => {
                        in_flight.clear();
                        failure = Some(read_task_join_error(error));
                        break;
                    }
                }
                continue;
            }
            result = device.read_frame() => result,
        };
        let frame = match frame {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                failure = Some(FuseTransportError::from_io(
                    FuseTransportErrorKind::Read,
                    &error,
                ));
                break;
            }
        };
        if state.stop.load(Ordering::Acquire) {
            break;
        }
        let destroy = match session.is_destroy_frame(&frame) {
            Ok(destroy) => destroy,
            Err(error) => {
                failure = Some(FuseTransportError::from_message(
                    FuseTransportErrorKind::Protocol,
                    error.to_string(),
                ));
                break;
            }
        };
        if destroy {
            // FUSE_DESTROY is a terminal request with no reply. The kernel
            // waits for the userspace device to close, so continuing through
            // ordinary dispatch would leave fusermount blocked waiting for a
            // frame that can never arrive. Abort read workers here; the
            // bounded terminal drain below prevents an uncooperative backend
            // future from keeping the device open forever, while
            // run_session() performs the final session-owned cleanup.
            for abort in in_flight.drain().map(|(_, abort)| abort) {
                abort.abort();
            }
            state.request_stop();
            break;
        }
        match session.prepare_read(&frame) {
            Ok(Some(prepared)) => {
                let unique = prepared.unique();
                let permit = tokio::select! {
                    _ = state.stop_notify.notified() => break,
                    result = Arc::clone(&permits).acquire_owned() => match result {
                        Ok(permit) => permit,
                        Err(_) => break,
                    },
                };
                let device = Arc::clone(&device);
                let writer = Arc::clone(&writer);
                let state = Arc::clone(&state);
                let abort = read_tasks.spawn(async move {
                    let _permit = permit;
                    let reply = prepared.reply().await;
                    (
                        unique,
                        write_reply_until_stop(device, writer, state, reply).await,
                    )
                });
                in_flight.insert(unique, abort);
                continue;
            }
            Ok(None) => {
                match session.interrupt_target(&frame) {
                    Ok(Some(target)) => {
                        if let Some(abort) = in_flight.remove(&target) {
                            abort.abort();
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        failure = Some(FuseTransportError::from_message(
                            FuseTransportErrorKind::Protocol,
                            error.to_string(),
                        ));
                        break;
                    }
                }
                match tokio::time::timeout(
                    READ_TASK_DRAIN_TIMEOUT,
                    drain_read_tasks(&mut read_tasks, &mut in_flight),
                )
                .await
                {
                    Ok(Some(error)) => {
                        failure = Some(error);
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        failure = Some(read_worker_drain_timeout_error());
                        break;
                    }
                }
            }
            Err(error) => {
                failure = Some(FuseTransportError::from_message(
                    FuseTransportErrorKind::Protocol,
                    error.to_string(),
                ));
                break;
            }
        }
        let reply = tokio::select! {
            _ = state.stop_notify.notified() => break,
            result = session.handle(&frame) => match result {
                Ok(reply) => reply,
                Err(error) => {
                    failure = Some(FuseTransportError::from_message(
                        FuseTransportErrorKind::Protocol,
                        error.to_string(),
                    ));
                    break;
                }
            },
        };
        if let Some(reply) = reply
            && let Err(error) = write_reply_until_stop(
                Arc::clone(&device),
                Arc::clone(&writer),
                Arc::clone(&state),
                reply,
            )
            .await
        {
            failure = Some(error);
            break;
        }
        if session.negotiated.is_some() && !state.ready.swap(true, Ordering::AcqRel) {
            state.ready_notify.notify_waiters();
        }
    }
    read_tasks.abort_all();
    if failure.is_none() {
        match tokio::time::timeout(
            READ_TASK_DRAIN_TIMEOUT,
            drain_read_tasks(&mut read_tasks, &mut in_flight),
        )
        .await
        {
            Ok(Some(error)) => failure = Some(error),
            Ok(None) => {}
            Err(_) => {
                failure = Some(read_worker_drain_timeout_error());
                // Dropping the JoinSet aborts any worker that did not honor
                // cancellation within the bound. The native device must be
                // released even when a backend future is not cancellation
                // cooperative.
            }
        }
    }
    in_flight.clear();
    failure
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
    for (index, option) in options.mount_options.iter().enumerate() {
        validate_mount_option(index, option)?;
    }
    if options.max_frame < MIN_NATIVE_MAX_FRAME {
        return Err(MountError::InvalidOption(format!(
            "max_frame must fit a modern FUSE WRITE frame of at least {MIN_NATIVE_MAX_FRAME} bytes"
        )));
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
fn validate_mount_option(index: usize, value: &str) -> Result<(), MountError> {
    let key = value.split_once('=').map_or(value, |(key, _)| key);
    let reserved = matches!(
        key,
        "fd" | "rootmode"
            | "user_id"
            | "group_id"
            | "fsname"
            | "subtype"
            | "default_permissions"
            | "allow_other"
            | "ro"
            | "max_read"
    );
    if value.is_empty()
        || value.starts_with('-')
        || key.is_empty()
        || value.chars().any(|character| {
            character == ',' || character.is_whitespace() || character.is_control()
        })
    {
        return Err(MountError::InvalidOption(format!(
            "mount_options[{index}] must be one non-empty option token without commas, whitespace, control characters or a leading '-'"
        )));
    }
    if reserved {
        return Err(MountError::InvalidOption(format!(
            "mount_options[{index}] may not override the transport-owned '{key}' option"
        )));
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
    // Keep the privileged path aligned with fusermount3's safety defaults.
    // Without these flags, a direct mount(2) call can be rejected by the FUSE
    // kernel driver even though the equivalent rootless helper mount works.
    let flags = libc::MS_NOSUID
        | libc::MS_NODEV
        | if options.read_only {
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
        // The kernel expects only the root inode's file-type bits here. The
        // driver stat includes normal permission bits as well, so mask them
        // before serializing the privileged mount boundary.
        parts.push(format!("rootmode={:o}", root_mode & S_IFMT));
        parts.push(format!("user_id={uid}"));
        parts.push(format!("group_id={gid}"));
    }
    // The helper consumes source metadata and generic mount flags from its
    // option string. The privileged mount(2) path supplies the source and
    // flags through its syscall arguments instead, so forwarding them as
    // kernel data would make the FUSE driver reject the mount with EINVAL.
    if privileged.is_none() {
        parts.push(format!("fsname={}", options.fsname));
    }
    if options.default_permissions {
        parts.push("default_permissions".to_owned());
    }
    if options.allow_other {
        parts.push("allow_other".to_owned());
    }
    if privileged.is_none() && options.read_only {
        parts.push("ro".to_owned());
    }
    if let Some(max_read) = options.max_read {
        parts.push(format!("max_read={max_read}"));
    }
    if privileged.is_none()
        && let Some(subtype) = &options.subtype
    {
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
) -> Instant {
    let deadline = Instant::now() + timeout;
    force_unmount_until(mode, mountpoint, helper, deadline).await;
    deadline
}

#[cfg(target_os = "linux")]
async fn force_unmount_until(
    mode: MountMode,
    mountpoint: &Path,
    helper: Option<&Path>,
    deadline: Instant,
) {
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

    #[cfg(target_os = "linux")]
    use async_trait::async_trait;

    #[test]
    fn defaults_are_safe_for_a_single_mount() {
        let options = MountOptions::default();
        assert_eq!(options.mode, MountMode::Auto);
        assert!(options.default_permissions);
        assert!(!options.allow_other);
        assert_eq!(options.device, Path::new("/dev/fuse"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn privileged_mount_data_excludes_helper_metadata_and_flags() {
        let options = MountOptions {
            fsname: "mount-rs-test".to_owned(),
            subtype: Some("mount-rs".to_owned()),
            read_only: true,
            ..MountOptions::default()
        };

        let privileged = mount_data(&options, Some((3, 0o040755, 1000, 1000)))
            .expect("privileged mount data should be valid");
        assert_eq!(
            privileged.to_str().expect("mount data is UTF-8"),
            "fd=3,rootmode=40000,user_id=1000,group_id=1000,default_permissions"
        );

        let helper = mount_data(&options, None).expect("helper mount data should be valid");
        assert_eq!(
            helper.to_str().expect("mount data is UTF-8"),
            "fsname=mount-rs-test,default_permissions,ro,subtype=mount-rs"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn session_protocol_failure_reports_one_owned_transport_error() {
        use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (device_stream, mut peer) = UnixStream::pair().expect("socket pair");
        let standard = device_stream.into_std().expect("standard Unix stream");
        // SAFETY: the raw descriptor is transferred immediately into OwnedFd.
        let descriptor = unsafe { OwnedFd::from_raw_fd(standard.into_raw_fd()) };
        let device = FuseDevice::from_owned_fd(descriptor, DEFAULT_MAX_FRAME)
            .expect("socket descriptor should satisfy the device boundary");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_callback = Arc::clone(&observed);
        let state = Arc::new(MountState::new(
            MountMode::Privileged,
            PathBuf::from("/tmp/mount-rs-fuse-hook-test"),
            MountOptions::default(),
            None,
            FuseMountHooks {
                on_transport_error: Some(Arc::new(move |error| {
                    observed_callback
                        .lock()
                        .expect("callback observation lock")
                        .push(error);
                })),
            },
        ));
        let task = tokio::spawn(run_session(
            FuseSession::new(Arc::new(mount_rs_core::MemoryFs::empty())),
            device,
            Arc::clone(&state),
        ));

        // A zero-length FUSE header is a protocol failure, not a peer EOF.
        peer.write_all(&[0; crate::IN_HEADER_SIZE])
            .await
            .expect("send malformed FUSE frame");
        task.await.expect("session task should finish");
        state.record_transport_error(FuseTransportError::from_message(
            FuseTransportErrorKind::Task,
            "duplicate test failure".to_owned(),
        ));

        let observed = observed.lock().expect("callback observation lock");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, FuseTransportErrorKind::Protocol);
        assert!(observed[0].message.contains("FUSE"));
        assert!(state.closed.load(Ordering::Acquire));
        assert_eq!(
            state
                .transport_error
                .lock()
                .expect("transport error lock")
                .as_deref(),
            Some(observed[0].message.as_str())
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn aborting_session_task_marks_mount_closed() {
        let state = Arc::new(MountState::new(
            MountMode::Privileged,
            PathBuf::from("/tmp/mount-rs-fuse-abort-close-test"),
            MountOptions::default(),
            None,
            FuseMountHooks::default(),
        ));
        state.set_task(tokio::spawn(std::future::pending::<()>()));
        let mount = FuseMount {
            state: Arc::clone(&state),
            mountpoint: PathBuf::from("/tmp/mount-rs-fuse-abort-close-test"),
        };

        state.abort_task();
        tokio::time::timeout(Duration::from_secs(1), mount.wait_closed())
            .await
            .expect("aborted session should publish closed state");
        assert!(!mount.is_active());
        assert!(state.closed.load(Ordering::Acquire));

        // The test did not create a real native mount. Prevent Drop from
        // attempting the production unmount fallback for this synthetic state.
        state.mounted.store(false, Ordering::Release);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_reports_the_configured_fsname() {
        let state = Arc::new(MountState::new(
            MountMode::Privileged,
            PathBuf::from("/tmp/mount-rs-fuse-source-test"),
            MountOptions {
                fsname: "mount-rs-source-test".to_owned(),
                ..MountOptions::default()
            },
            None,
            FuseMountHooks::default(),
        ));
        let mount = FuseMount {
            state: Arc::clone(&state),
            mountpoint: PathBuf::from("/tmp/mount-rs-fuse-source-test"),
        };

        assert_eq!(mount.source(), Some("mount-rs-source-test"));
        state.mounted.store(false, Ordering::Release);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn unmount_marks_mount_inactive_before_helper_returns() {
        let state = Arc::new(MountState::new(
            MountMode::Rootless,
            PathBuf::from("/tmp/mount-rs-fuse-active-transition-test"),
            MountOptions {
                mode: MountMode::Rootless,
                ..MountOptions::default()
            },
            Some(PathBuf::from("/bin/sh")),
            FuseMountHooks::default(),
        ));
        let mount = FuseMount {
            state: Arc::clone(&state),
            mountpoint: PathBuf::from("/tmp/mount-rs-fuse-active-transition-test"),
        };

        mount.start_unmount();
        assert!(
            !mount.is_active(),
            "teardown should publish inactive immediately"
        );
        tokio::time::timeout(Duration::from_secs(1), mount.unmount())
            .await
            .expect("unmount helper should settle")
            .expect("a non-mounted synthetic path should be idempotent");
        assert!(state.closed.load(Ordering::Acquire));
        assert!(!state.mounted.load(Ordering::Acquire));
    }

    #[cfg(target_os = "linux")]
    struct PanicDriver {
        inner: Arc<mount_rs_core::MemoryFs>,
    }

    #[cfg(target_os = "linux")]
    #[async_trait]
    impl mount_rs_core::FsDriver for PanicDriver {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            self.inner.capabilities()
        }

        async fn stat(&self, _path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
            panic!("injected FUSE backend panic");
        }

        async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
            self.inner.readdir(path).await
        }

        async fn open(
            &self,
            path: &str,
            flags: &str,
            mode: u32,
        ) -> mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>> {
            self.inner.open(path, flags, mode).await
        }
    }

    #[cfg(target_os = "linux")]
    fn test_frame(opcode: u32, unique: u64, nodeid: u64, body: &[u8]) -> Vec<u8> {
        let mut frame = crate::RequestHeader {
            len: (crate::IN_HEADER_SIZE + body.len()) as u32,
            opcode,
            unique,
            nodeid,
            uid: 0,
            gid: 0,
            pid: 0,
            total_extlen: 0,
        }
        .encode()
        .to_vec();
        frame.extend_from_slice(body);
        frame
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn session_panic_closes_state_and_reports_one_owned_task_error() {
        use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (device_stream, mut peer) = UnixStream::pair().expect("socket pair");
        let standard = device_stream.into_std().expect("standard Unix stream");
        // SAFETY: the raw descriptor is transferred immediately into OwnedFd.
        let descriptor = unsafe { OwnedFd::from_raw_fd(standard.into_raw_fd()) };
        let device = FuseDevice::from_owned_fd(descriptor, DEFAULT_MAX_FRAME)
            .expect("socket descriptor should satisfy the device boundary");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_callback = Arc::clone(&observed);
        let state = Arc::new(MountState::new(
            MountMode::Privileged,
            PathBuf::from("/tmp/mount-rs-fuse-panic-test"),
            MountOptions::default(),
            None,
            FuseMountHooks {
                on_transport_error: Some(Arc::new(move |error| {
                    observed_callback
                        .lock()
                        .expect("callback observation lock")
                        .push(error);
                })),
            },
        ));
        let driver = Arc::new(PanicDriver {
            inner: Arc::new(mount_rs_core::MemoryFs::empty()),
        });
        let task = tokio::spawn(run_session(
            FuseSession::new(driver),
            device,
            Arc::clone(&state),
        ));

        let init: Vec<u8> = [7_u32, 41, 65536, u32::MAX, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        peer.write_all(&test_frame(26, 1, 0, &init))
            .await
            .expect("send init");
        // The test peer is a byte stream while the native FUSE device is
        // message-framed. Consume the INIT reply before sending the next
        // request so two test frames cannot be coalesced into one read.
        let _ = read_test_reply(&mut peer).await;
        peer.write_all(&test_frame(1, 2, 1, b"panic\0"))
            .await
            .expect("send panicking lookup");

        task.await
            .expect("session task should absorb backend panic");

        let observed = observed.lock().expect("callback observation lock");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, FuseTransportErrorKind::Task);
        assert_eq!(observed[0].message, "FUSE session task panicked");
        assert!(!state.active.load(Ordering::Acquire));
        assert!(state.closed.load(Ordering::Acquire));
        assert_eq!(
            state
                .transport_error
                .lock()
                .expect("transport error lock")
                .as_deref(),
            Some("FUSE session task panicked")
        );
    }

    #[cfg(target_os = "linux")]
    struct BlockingDriver {
        inner: Arc<mount_rs_core::MemoryFs>,
        entered: Arc<tokio::sync::Notify>,
    }

    #[cfg(target_os = "linux")]
    #[async_trait]
    impl mount_rs_core::FsDriver for BlockingDriver {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            self.inner.capabilities()
        }

        async fn stat(&self, _path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.entered.notify_one();
            std::future::pending().await
        }

        async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
            self.inner.readdir(path).await
        }

        async fn open(
            &self,
            path: &str,
            flags: &str,
            mode: u32,
        ) -> mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>> {
            self.inner.open(path, flags, mode).await
        }
    }

    #[cfg(target_os = "linux")]
    struct BlockingCloseHandle {
        inner: Arc<dyn mount_rs_core::FileHandle>,
    }

    #[cfg(target_os = "linux")]
    #[async_trait]
    impl mount_rs_core::FileHandle for BlockingCloseHandle {
        fn fd(&self) -> Option<u64> {
            self.inner.fd()
        }

        async fn read(
            &self,
            buffer: &mut [u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.inner.read(buffer, position).await
        }

        async fn write(
            &self,
            buffer: &[u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.inner.write(buffer, position).await
        }

        async fn stat(&self) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.inner.stat().await
        }

        async fn truncate(&self, length: u64) -> mount_rs_core::Result<()> {
            self.inner.truncate(length).await
        }

        async fn sync(&self) -> mount_rs_core::Result<()> {
            self.inner.sync().await
        }

        async fn datasync(&self) -> mount_rs_core::Result<()> {
            self.inner.datasync().await
        }

        async fn close(&self) -> mount_rs_core::Result<()> {
            std::future::pending().await
        }
    }

    #[cfg(target_os = "linux")]
    struct BlockingCloseDriver {
        inner: Arc<mount_rs_core::MemoryFs>,
    }

    #[cfg(target_os = "linux")]
    #[async_trait]
    impl mount_rs_core::FsDriver for BlockingCloseDriver {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            self.inner.capabilities()
        }

        async fn stat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.inner.stat(path).await
        }

        async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
            self.inner.readdir(path).await
        }

        async fn open(
            &self,
            path: &str,
            flags: &str,
            mode: u32,
        ) -> mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>> {
            let inner = self.inner.open(path, flags, mode).await?;
            Ok(Arc::new(BlockingCloseHandle { inner }))
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn destroy_bounds_backend_handle_cleanup_and_reports_task_error() {
        use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let inner = Arc::new(mount_rs_core::MemoryFs::empty());
        let file = inner.open("/file", "w", 0o644).await.expect("create file");
        file.close().await.expect("close seed file");
        let driver = Arc::new(BlockingCloseDriver { inner });

        let (device_stream, mut peer) = UnixStream::pair().expect("socket pair");
        let standard = device_stream.into_std().expect("standard Unix stream");
        // SAFETY: the raw descriptor is transferred immediately into OwnedFd.
        let descriptor = unsafe { OwnedFd::from_raw_fd(standard.into_raw_fd()) };
        let device = FuseDevice::from_owned_fd(descriptor, DEFAULT_MAX_FRAME)
            .expect("socket descriptor should satisfy the device boundary");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_callback = Arc::clone(&observed);
        let state = Arc::new(MountState::new(
            MountMode::Privileged,
            PathBuf::from("/tmp/mount-rs-fuse-blocking-close-test"),
            MountOptions {
                unmount_timeout: Duration::from_millis(25),
                ..MountOptions::default()
            },
            None,
            FuseMountHooks {
                on_transport_error: Some(Arc::new(move |error| {
                    observed_callback
                        .lock()
                        .expect("callback observation lock")
                        .push(error);
                })),
            },
        ));
        let task = tokio::spawn(run_session(
            FuseSession::new(driver),
            device,
            Arc::clone(&state),
        ));

        let init: Vec<u8> = [7_u32, 41, 65536, u32::MAX, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        peer.write_all(&test_frame(26, 1, 0, &init))
            .await
            .expect("send init");
        let _ = read_test_reply(&mut peer).await;
        peer.write_all(&test_frame(1, 2, 1, b"file\0"))
            .await
            .expect("send lookup");
        let lookup = read_test_reply(&mut peer).await;
        let nodeid = u64::from_le_bytes(lookup[16..24].try_into().unwrap());
        peer.write_all(&test_frame(14, 3, nodeid, &[0; 8]))
            .await
            .expect("send open");
        let open = read_test_reply(&mut peer).await;
        assert_eq!(i32::from_le_bytes(open[4..8].try_into().unwrap()), 0);

        peer.write_all(&test_frame(crate::constants::FUSE_DESTROY, 4, 0, &[]))
            .await
            .expect("send destroy");
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("blocking close must not keep the session alive")
            .expect("session task should finish after bounded cleanup");

        assert!(state.closed.load(Ordering::Acquire));
        let observed = observed.lock().expect("callback observation lock");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, FuseTransportErrorKind::Task);
        assert!(observed[0].message.contains("session cleanup"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn stop_cancels_an_inflight_request_and_closes_without_transport_error() {
        use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (device_stream, mut peer) = UnixStream::pair().expect("socket pair");
        let standard = device_stream.into_std().expect("standard Unix stream");
        // SAFETY: the raw descriptor is transferred immediately into OwnedFd.
        let descriptor = unsafe { OwnedFd::from_raw_fd(standard.into_raw_fd()) };
        let device = FuseDevice::from_owned_fd(descriptor, DEFAULT_MAX_FRAME)
            .expect("socket descriptor should satisfy the device boundary");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_callback = Arc::clone(&observed);
        let state = Arc::new(MountState::new(
            MountMode::Privileged,
            PathBuf::from("/tmp/mount-rs-fuse-cancel-test"),
            MountOptions::default(),
            None,
            FuseMountHooks {
                on_transport_error: Some(Arc::new(move |error| {
                    observed_callback
                        .lock()
                        .expect("callback observation lock")
                        .push(error);
                })),
            },
        ));
        let entered = Arc::new(tokio::sync::Notify::new());
        let driver = Arc::new(BlockingDriver {
            inner: Arc::new(mount_rs_core::MemoryFs::empty()),
            entered: Arc::clone(&entered),
        });
        let task = tokio::spawn(run_session(
            FuseSession::new(driver),
            device,
            Arc::clone(&state),
        ));

        let init: Vec<u8> = [7_u32, 41, 65536, u32::MAX, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        peer.write_all(&test_frame(26, 1, 0, &init))
            .await
            .expect("send init");
        // Keep the stream-backed test peer aligned with the message framing
        // provided by the native FUSE character device.
        let _ = read_test_reply(&mut peer).await;
        peer.write_all(&test_frame(1, 2, 1, b"blocked\0"))
            .await
            .expect("send blocking lookup");
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("lookup should enter the blocking backend");

        state.request_stop();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("stop should cancel the in-flight request")
            .expect("session task should finish after cancellation");

        assert!(!state.active.load(Ordering::Acquire));
        assert!(state.closed.load(Ordering::Acquire));
        assert!(
            observed
                .lock()
                .expect("callback observation lock")
                .is_empty()
        );
        assert!(
            state
                .transport_error
                .lock()
                .expect("transport error lock")
                .is_none()
        );
    }

    #[cfg(target_os = "linux")]
    struct ReadBarrierHandle {
        inner: Arc<dyn mount_rs_core::FileHandle>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        max_active: Arc<std::sync::atomic::AtomicUsize>,
        barrier: Arc<tokio::sync::Barrier>,
        entered: Arc<tokio::sync::Notify>,
    }

    #[cfg(target_os = "linux")]
    #[async_trait]
    impl mount_rs_core::FileHandle for ReadBarrierHandle {
        fn fd(&self) -> Option<u64> {
            self.inner.fd()
        }

        async fn read(
            &self,
            buffer: &mut [u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.entered.notify_one();
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.inner.read(buffer, position).await
        }

        async fn write(
            &self,
            buffer: &[u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.inner.write(buffer, position).await
        }

        async fn stat(&self) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.inner.stat().await
        }

        async fn truncate(&self, length: u64) -> mount_rs_core::Result<()> {
            self.inner.truncate(length).await
        }

        async fn sync(&self) -> mount_rs_core::Result<()> {
            self.inner.sync().await
        }

        async fn datasync(&self) -> mount_rs_core::Result<()> {
            self.inner.datasync().await
        }

        async fn close(&self) -> mount_rs_core::Result<()> {
            self.inner.close().await
        }
    }

    #[cfg(target_os = "linux")]
    struct ReadBarrierDriver {
        inner: Arc<mount_rs_core::MemoryFs>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        max_active: Arc<std::sync::atomic::AtomicUsize>,
        barrier: Arc<tokio::sync::Barrier>,
        entered: Arc<tokio::sync::Notify>,
    }

    #[cfg(target_os = "linux")]
    #[async_trait]
    impl mount_rs_core::FsDriver for ReadBarrierDriver {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            self.inner.capabilities()
        }

        async fn stat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.inner.stat(path).await
        }

        async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
            self.inner.readdir(path).await
        }

        async fn open(
            &self,
            path: &str,
            flags: &str,
            mode: u32,
        ) -> mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>> {
            let inner = self.inner.open(path, flags, mode).await?;
            Ok(Arc::new(ReadBarrierHandle {
                inner,
                active: Arc::clone(&self.active),
                max_active: Arc::clone(&self.max_active),
                barrier: Arc::clone(&self.barrier),
                entered: Arc::clone(&self.entered),
            }))
        }
    }

    #[cfg(target_os = "linux")]
    async fn read_test_reply(peer: &mut tokio::net::UnixStream) -> Vec<u8> {
        use tokio::io::AsyncReadExt;

        let mut reply = vec![0; crate::OUT_HEADER_SIZE];
        peer.read_exact(&mut reply)
            .await
            .expect("read FUSE reply header");
        let length = u32::from_le_bytes(reply[..4].try_into().unwrap()) as usize;
        assert!(length >= crate::OUT_HEADER_SIZE);
        reply.resize(length, 0);
        peer.read_exact(&mut reply[crate::OUT_HEADER_SIZE..])
            .await
            .expect("read FUSE reply body");
        reply
    }

    #[cfg(target_os = "linux")]
    fn read_body(handle: u64) -> Vec<u8> {
        let mut body = vec![0; 40];
        body[..8].copy_from_slice(&handle.to_le_bytes());
        body[16..20].copy_from_slice(&4_u32.to_le_bytes());
        body
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn positional_reads_run_in_parallel_with_bounded_session_state() {
        use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
        use std::sync::atomic::AtomicUsize;
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let inner = Arc::new(mount_rs_core::MemoryFs::empty());
        let file = inner.open("/file", "w", 0o644).await.expect("create file");
        file.write(b"data", Some(0)).await.expect("seed file");
        file.close().await.expect("close seed file");
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let driver = Arc::new(ReadBarrierDriver {
            inner,
            active,
            max_active: Arc::clone(&max_active),
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            entered: Arc::clone(&entered),
        });

        let (device_stream, mut peer) = UnixStream::pair().expect("socket pair");
        let standard = device_stream.into_std().expect("standard Unix stream");
        // SAFETY: the raw descriptor is transferred immediately into OwnedFd.
        let descriptor = unsafe { OwnedFd::from_raw_fd(standard.into_raw_fd()) };
        let device = FuseDevice::from_owned_fd(descriptor, DEFAULT_MAX_FRAME)
            .expect("socket descriptor should satisfy the device boundary");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_callback = Arc::clone(&observed);
        let state = Arc::new(MountState::new(
            MountMode::Privileged,
            PathBuf::from("/tmp/mount-rs-fuse-read-concurrency-test"),
            MountOptions::default(),
            None,
            FuseMountHooks {
                on_transport_error: Some(Arc::new(move |error| {
                    observed_callback
                        .lock()
                        .expect("callback observation lock")
                        .push(error);
                })),
            },
        ));
        let task = tokio::spawn(run_session(
            FuseSession::new(driver),
            device,
            Arc::clone(&state),
        ));

        let init: Vec<u8> = [7_u32, 41, 65536, u32::MAX, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        peer.write_all(&test_frame(26, 1, 0, &init))
            .await
            .expect("send init");
        assert_eq!(read_test_reply(&mut peer).await.len(), 80);

        peer.write_all(&test_frame(1, 2, 1, b"file\0"))
            .await
            .expect("send lookup");
        let lookup = read_test_reply(&mut peer).await;
        assert_eq!(i32::from_le_bytes(lookup[4..8].try_into().unwrap()), 0);
        let nodeid = u64::from_le_bytes(lookup[16..24].try_into().unwrap());

        peer.write_all(&test_frame(14, 3, nodeid, &[0; 8]))
            .await
            .expect("send open");
        let open = read_test_reply(&mut peer).await;
        assert_eq!(i32::from_le_bytes(open[4..8].try_into().unwrap()), 0);
        let handle = u64::from_le_bytes(open[16..24].try_into().unwrap());

        let body = read_body(handle);
        // A Unix stream may coalesce adjacent writes, but a native FUSE
        // descriptor returns one complete request per read. Wait until the
        // first worker has entered before sending the second request so this
        // test preserves that framing while still proving concurrency.
        let first_entered = entered.notified();
        peer.write_all(&test_frame(15, 4, nodeid, &body))
            .await
            .expect("send first read");
        tokio::time::timeout(Duration::from_secs(1), first_entered)
            .await
            .expect("first read should enter the worker");
        let second_entered = entered.notified();
        peer.write_all(&test_frame(15, 5, nodeid, &body))
            .await
            .expect("send second read");
        tokio::time::timeout(Duration::from_secs(1), second_entered)
            .await
            .expect("second read should enter the worker");
        let first = tokio::time::timeout(Duration::from_secs(1), read_test_reply(&mut peer))
            .await
            .expect("first parallel read reply");
        let second = tokio::time::timeout(Duration::from_secs(1), read_test_reply(&mut peer))
            .await
            .expect("second parallel read reply");
        assert_eq!(&first[16..], b"data");
        assert_eq!(&second[16..], b"data");
        assert_eq!(max_active.load(Ordering::SeqCst), 2);

        state.request_stop();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("stop should close the read session")
            .expect("read session task should finish");
        assert!(state.closed.load(Ordering::Acquire));
        assert!(
            observed
                .lock()
                .expect("callback observation lock")
                .is_empty()
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn interrupt_aborts_an_inflight_read_without_closing_the_session() {
        use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
        use std::sync::atomic::AtomicUsize;
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let inner = Arc::new(mount_rs_core::MemoryFs::empty());
        let file = inner.open("/file", "w", 0o644).await.expect("create file");
        file.write(b"data", Some(0)).await.expect("seed file");
        file.close().await.expect("close seed file");
        let driver = Arc::new(ReadBarrierDriver {
            inner,
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            entered: Arc::new(tokio::sync::Notify::new()),
        });

        let (device_stream, mut peer) = UnixStream::pair().expect("socket pair");
        let standard = device_stream.into_std().expect("standard Unix stream");
        // SAFETY: the raw descriptor is transferred immediately into OwnedFd.
        let descriptor = unsafe { OwnedFd::from_raw_fd(standard.into_raw_fd()) };
        let device = FuseDevice::from_owned_fd(descriptor, DEFAULT_MAX_FRAME)
            .expect("socket descriptor should satisfy the device boundary");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_callback = Arc::clone(&observed);
        let state = Arc::new(MountState::new(
            MountMode::Privileged,
            PathBuf::from("/tmp/mount-rs-fuse-interrupt-test"),
            MountOptions::default(),
            None,
            FuseMountHooks {
                on_transport_error: Some(Arc::new(move |error| {
                    observed_callback
                        .lock()
                        .expect("callback observation lock")
                        .push(error);
                })),
            },
        ));
        let entered = Arc::clone(&driver.entered);
        let task = tokio::spawn(run_session(
            FuseSession::new(driver),
            device,
            Arc::clone(&state),
        ));

        let init: Vec<u8> = [7_u32, 41, 65536, u32::MAX, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        peer.write_all(&test_frame(26, 1, 0, &init))
            .await
            .expect("send init");
        let _ = read_test_reply(&mut peer).await;
        peer.write_all(&test_frame(1, 2, 1, b"file\0"))
            .await
            .expect("send lookup");
        let lookup = read_test_reply(&mut peer).await;
        let nodeid = u64::from_le_bytes(lookup[16..24].try_into().unwrap());
        peer.write_all(&test_frame(14, 3, nodeid, &[0; 8]))
            .await
            .expect("send open");
        let open = read_test_reply(&mut peer).await;
        let handle = u64::from_le_bytes(open[16..24].try_into().unwrap());

        peer.write_all(&test_frame(15, 4, nodeid, &read_body(handle)))
            .await
            .expect("send blocking read");
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("read should enter the worker");

        peer.write_all(&test_frame(
            crate::constants::FUSE_INTERRUPT,
            5,
            0,
            &4_u64.to_le_bytes(),
        ))
        .await
        .expect("send interrupt");
        let interrupt = tokio::time::timeout(Duration::from_secs(1), read_test_reply(&mut peer))
            .await
            .expect("interrupt reply");
        assert_eq!(i32::from_le_bytes(interrupt[4..8].try_into().unwrap()), -11);
        assert_eq!(u64::from_le_bytes(interrupt[8..16].try_into().unwrap()), 5);
        assert!(!state.closed.load(Ordering::Acquire));

        state.request_stop();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("stop should close the interrupted session")
            .expect("interrupted session task should finish");
        assert!(state.closed.load(Ordering::Acquire));
        assert!(
            observed
                .lock()
                .expect("callback observation lock")
                .is_empty()
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn destroy_aborts_inflight_read_workers_before_session_cleanup() {
        use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
        use std::sync::atomic::AtomicUsize;
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let inner = Arc::new(mount_rs_core::MemoryFs::empty());
        let file = inner.open("/file", "w", 0o644).await.expect("create file");
        file.write(b"data", Some(0)).await.expect("seed file");
        file.close().await.expect("close seed file");
        let driver = Arc::new(ReadBarrierDriver {
            inner,
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            entered: Arc::new(tokio::sync::Notify::new()),
        });

        let (device_stream, mut peer) = UnixStream::pair().expect("socket pair");
        let standard = device_stream.into_std().expect("standard Unix stream");
        // SAFETY: the raw descriptor is transferred immediately into OwnedFd.
        let descriptor = unsafe { OwnedFd::from_raw_fd(standard.into_raw_fd()) };
        let device = FuseDevice::from_owned_fd(descriptor, DEFAULT_MAX_FRAME)
            .expect("socket descriptor should satisfy the device boundary");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_callback = Arc::clone(&observed);
        let state = Arc::new(MountState::new(
            MountMode::Privileged,
            PathBuf::from("/tmp/mount-rs-fuse-destroy-read-test"),
            MountOptions::default(),
            None,
            FuseMountHooks {
                on_transport_error: Some(Arc::new(move |error| {
                    observed_callback
                        .lock()
                        .expect("callback observation lock")
                        .push(error);
                })),
            },
        ));
        let entered = Arc::clone(&driver.entered);
        let task = tokio::spawn(run_session(
            FuseSession::new(driver),
            device,
            Arc::clone(&state),
        ));

        let init: Vec<u8> = [7_u32, 41, 65536, u32::MAX, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        peer.write_all(&test_frame(26, 1, 0, &init))
            .await
            .expect("send init");
        let _ = read_test_reply(&mut peer).await;
        peer.write_all(&test_frame(1, 2, 1, b"file\0"))
            .await
            .expect("send lookup");
        let lookup = read_test_reply(&mut peer).await;
        let nodeid = u64::from_le_bytes(lookup[16..24].try_into().unwrap());
        peer.write_all(&test_frame(14, 3, nodeid, &[0; 8]))
            .await
            .expect("send open");
        let open = read_test_reply(&mut peer).await;
        let handle = u64::from_le_bytes(open[16..24].try_into().unwrap());

        peer.write_all(&test_frame(15, 4, nodeid, &read_body(handle)))
            .await
            .expect("send blocking read");
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("read should enter the worker");

        peer.write_all(&test_frame(crate::constants::FUSE_DESTROY, 5, 0, &[]))
            .await
            .expect("send destroy");
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("FUSE_DESTROY should close the session")
            .expect("destroyed session task should finish");
        assert!(state.closed.load(Ordering::Acquire));
        assert!(
            observed
                .lock()
                .expect("callback observation lock")
                .is_empty()
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_read_drain_is_bounded_for_blocking_workers() {
        let mut read_tasks = tokio::task::JoinSet::<ReadTaskResult>::new();
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let abort = read_tasks.spawn_blocking(move || {
            let _ = started_sender.send(());
            std::thread::sleep(Duration::from_millis(1_250));
            (7, Ok(()))
        });
        let mut in_flight = std::collections::HashMap::from([(7, abort)]);

        started_receiver
            .await
            .expect("blocking read worker should start");
        for abort in in_flight.drain().map(|(_, abort)| abort) {
            abort.abort();
        }

        let result = tokio::time::timeout(
            READ_TASK_DRAIN_TIMEOUT,
            drain_read_tasks(&mut read_tasks, &mut in_flight),
        )
        .await;
        assert!(
            result.is_err(),
            "terminal cleanup must not wait indefinitely for a blocking worker"
        );
        drop(read_tasks);
    }

    #[test]
    fn mode_and_option_validation_have_no_native_side_effects() {
        #[cfg(target_os = "linux")]
        {
            assert!(validate_token("fsname", "plain-name").is_ok());
            assert!(validate_token("fsname", "bad,allow_other").is_err());
            assert!(validate_token("subtype", "bad=option").is_err());
            assert!(validate_token("fsname", "-option").is_err());

            assert!(validate_mount_option(0, "nodev").is_ok());
            assert!(validate_mount_option(1, "context=system_u:object_r:fusefs_t:s0").is_ok());
            assert!(validate_mount_option(2, "bad,allow_other").is_err());
            assert!(validate_mount_option(3, "bad option").is_err());
            assert!(validate_mount_option(4, "=missing-key").is_err());
            assert!(validate_mount_option(5, "-o").is_err());
            assert!(validate_mount_option(6, "fsname=caller-controlled").is_err());
            assert!(validate_mount_option(7, "fd=99").is_err());

            let options = MountOptions {
                mount_options: vec!["nodev".to_owned()],
                ..MountOptions::default()
            };
            assert!(validate_options(&options).is_ok());
            let options = MountOptions {
                mount_options: vec!["fsname=caller-controlled".to_owned()],
                ..MountOptions::default()
            };
            assert!(validate_options(&options).is_err());

            let options = MountOptions {
                max_frame: MIN_NATIVE_MAX_FRAME - 1,
                ..MountOptions::default()
            };
            assert!(validate_options(&options).is_err());
            let options = MountOptions {
                max_frame: MIN_NATIVE_MAX_FRAME,
                ..MountOptions::default()
            };
            assert!(validate_options(&options).is_ok());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn privileged_mount_data_masks_root_permissions() {
        let data = mount_data(
            &MountOptions::default(),
            Some((3, S_IFDIR | 0o755, 1000, 1001)),
        )
        .expect("mount data should be valid");
        assert_eq!(
            data.to_bytes(),
            b"fd=3,rootmode=40000,user_id=1000,group_id=1001,default_permissions"
        );
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
    async fn forced_unmount_reuses_escalation_deadline_for_task_drain() {
        use std::os::unix::fs::PermissionsExt;

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let helper = std::env::temp_dir().join(format!(
            "mount-rs-fuse-stuck-unmount-{}-{suffix}",
            std::process::id()
        ));
        std::fs::write(&helper, b"#!/bin/sh\nwhile :; do :; done\n").expect("write stuck helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("stuck helper metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).expect("make stuck helper executable");

        let timeout = Duration::from_millis(200);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_callback = Arc::clone(&observed);
        let state = Arc::new(MountState::new(
            MountMode::Rootless,
            PathBuf::from(format!("/tmp/mount-rs-fuse-stuck-unmount-{suffix}")),
            MountOptions {
                mode: MountMode::Rootless,
                unmount_timeout: timeout,
                ..MountOptions::default()
            },
            Some(helper.clone()),
            FuseMountHooks {
                on_transport_error: Some(Arc::new(move |error| {
                    observed_callback
                        .lock()
                        .expect("forced teardown callback lock")
                        .push(error);
                })),
            },
        ));
        state.set_task(tokio::spawn(std::future::pending::<()>()));

        let started = std::time::Instant::now();
        let result = Arc::clone(&state).perform_unmount().await;
        let elapsed = started.elapsed();
        let _ = std::fs::remove_file(&helper);

        assert!(matches!(
            result,
            Err(MountError::Timeout {
                operation: "unmount",
                after
            }) if after == timeout
        ));
        assert!(
            elapsed < timeout + timeout / 2,
            "forced teardown spent an extra full task timeout: {elapsed:?}"
        );
        assert!(!state.active.load(Ordering::Acquire));
        assert!(state.closed.load(Ordering::Acquire));
        assert!(!state.mounted.load(Ordering::Acquire));
        let observed = observed
            .lock()
            .expect("forced teardown callback observation");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, FuseTransportErrorKind::Task);
        assert!(
            observed[0]
                .message
                .contains("forced teardown was requested")
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn forced_unmount_stops_session_before_lazy_detach() {
        use std::os::unix::fs::PermissionsExt;

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let helper = std::env::temp_dir().join(format!(
            "mount-rs-fuse-ordering-helper-{}-{suffix}",
            std::process::id()
        ));
        let marker = std::env::temp_dir().join(format!(
            "mount-rs-fuse-ordering-marker-{}-{suffix}",
            std::process::id()
        ));
        let forced_marker = PathBuf::from(format!("{}.forced", marker.display()));
        let script = b"#!/bin/sh\nif [ \"$2\" = \"-z\" ]; then\n    while [ ! -f \"$4\" ]; do\n        sleep 0.01\n    done\n    : > \"$4.forced\"\n    exit 0\nfi\nwhile [ ! -f \"$3\" ]; do\n    sleep 0.01\ndone\nexit 0\n";
        std::fs::write(&helper, script).expect("write ordering helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("ordering helper metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).expect("make ordering helper executable");

        let state = Arc::new(MountState::new(
            MountMode::Rootless,
            marker.clone(),
            MountOptions {
                mode: MountMode::Rootless,
                unmount_timeout: Duration::from_millis(100),
                ..MountOptions::default()
            },
            Some(helper.clone()),
            FuseMountHooks::default(),
        ));
        let task_state = Arc::clone(&state);
        let task_marker = marker.clone();
        state.set_task(tokio::spawn(async move {
            loop {
                let notified = task_state.stop_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if task_state.stop.load(Ordering::Acquire) {
                    break;
                }
                notified.await;
            }
            std::fs::write(task_marker, b"stopped").expect("write stop marker");
        }));

        let result = Arc::clone(&state).perform_unmount().await;

        assert!(matches!(
            result,
            Err(MountError::Timeout {
                operation: "unmount",
                ..
            })
        ));
        assert!(state.stop.load(Ordering::Acquire));
        assert!(state.closed.load(Ordering::Acquire));
        assert!(!state.mounted.load(Ordering::Acquire));
        assert!(
            forced_marker.exists(),
            "lazy detach must start only after the session stop request"
        );

        let _ = std::fs::remove_file(&helper);
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(forced_marker);
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
