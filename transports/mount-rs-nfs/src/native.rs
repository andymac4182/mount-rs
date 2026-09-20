//! Native kernel-mount lifecycle for the NFSv3/NFSv4.1 server.
//!
//! The wire server is deliberately usable without privileges. This module is
//! the separate bridge to the host's `mount(8)`/`umount(8)` commands. It keeps
//! all platform-specific behavior here so the XDR, RPC, and session code stays
//! portable on macOS and Linux.
//!
//! Linux can select the v4.1 wire service. macOS is deliberately refused for
//! v4.1 because this crate does not provide the v4.0 compatibility mode that
//! Apple's client expects; use NFSv3 there.

use std::fmt;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use mount_rs_core::FsDriver;
use tokio::process::Command;
use tokio::sync::Mutex as AsyncMutex;

use crate::server::{NfsServer, NfsServerOptions, create_nfs_server};

const MACOS_MOUNT_TABLE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_UNMOUNT_TIMEOUT: Duration = Duration::from_secs(10);
// This is carried through the existing public `mount_options` field so the
// profile can be added without breaking downstream struct literals. It is
// consumed by `nfs_mount_options` and is never passed to mount(8).
const SQLITE_SINGLE_HOST_PROFILE: &str = "mount-rs-sqlite-single-host";
const LINUX_HELPERS: &[&str] = &[
    "/sbin/mount.nfs",
    "/usr/sbin/mount.nfs",
    "/usr/local/sbin/mount.nfs",
];
const MACOS_HELPERS: &[&str] = &["/sbin/mount_nfs"];

/// A host this transport knows how to put a kernel NFS client in front of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfsPlatform {
    Linux,
    Macos,
}

/// Narrow a target operating-system name to the platforms supported by the
/// native lifecycle. This is public so cross-platform test harnesses can cover
/// both option spellings from either host.
pub fn nfs_platform() -> Option<NfsPlatform> {
    nfs_platform_for(std::env::consts::OS)
}

/// Convert a Rust target OS name to the native NFS platform, if supported.
pub fn nfs_platform_for(platform: &str) -> Option<NfsPlatform> {
    match platform {
        "linux" => Some(NfsPlatform::Linux),
        "macos" | "darwin" => Some(NfsPlatform::Macos),
        _ => None,
    }
}

/// The host-side NFS client capabilities discovered without starting a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfsClientProbe {
    pub usable: bool,
    pub platform: Option<NfsPlatform>,
    pub helper: Option<PathBuf>,
    pub kernel: bool,
    pub v4: bool,
    pub root: bool,
    pub reason: Option<String>,
}

/// Probe the current host for the helper, kernel client, and Linux privilege
/// needed by `mount(8)`. This is intentionally a capability report, not a
/// claim that a mount will succeed: the mountpoint, privacy policy, and
/// namespace/capability configuration are checked only at mount time.
pub fn nfs_client_probe() -> NfsClientProbe {
    nfs_client_probe_for(std::env::consts::OS)
}

/// Probe a named platform. The filesystem checks still use the current host;
/// this helper is primarily useful for deterministic cross-platform tests.
pub fn nfs_client_probe_for(platform: &str) -> NfsClientProbe {
    let host = nfs_platform_for(platform);
    let root = effective_uid().is_some_and(|uid| uid == 0);
    let helper = host.and_then(|platform| {
        let paths = match platform {
            NfsPlatform::Linux => LINUX_HELPERS,
            NfsPlatform::Macos => MACOS_HELPERS,
        };
        paths
            .iter()
            .map(Path::new)
            .find(|path| executable_file(path))
            .map(Path::to_path_buf)
    });

    let mut kernel = helper.is_some() && host == Some(NfsPlatform::Macos);
    let mut v4 = false;
    if host == Some(NfsPlatform::Linux) {
        let filesystems = fs::read_to_string("/proc/filesystems").unwrap_or_default();
        kernel = has_filesystem(&filesystems, "nfs");
        v4 = has_filesystem(&filesystems, "nfs4") || helper.is_some();
    }

    let mut missing = Vec::new();
    if host.is_none() {
        missing.push(format!(
            "this is {platform}; the NFS transport mounts on Linux and macOS only"
        ));
    }
    if host == Some(NfsPlatform::Linux) && !root {
        missing.push("mounting NFS needs root on Linux (or an equivalent mount capability)".into());
    }
    if host.is_some() && helper.is_none() && !kernel {
        missing.push(match host {
            Some(NfsPlatform::Macos) => {
                "no /sbin/mount_nfs, which every supported macOS should provide".into()
            }
            Some(NfsPlatform::Linux) => {
                "no mount.nfs helper and no nfs filesystem in /proc/filesystems (install nfs-common or nfs-utils)".into()
            }
            None => unreachable!(),
        });
    }

    NfsClientProbe {
        usable: missing.is_empty(),
        platform: host,
        helper,
        kernel,
        v4,
        root,
        reason: (!missing.is_empty()).then(|| missing.join("; ")),
    }
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn has_filesystem(filesystems: &str, wanted: &str) -> bool {
    filesystems.lines().any(|line| {
        line.split_whitespace()
            .last()
            .is_some_and(|name| name == wanted)
    })
}

fn effective_uid() -> Option<u32> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("id").arg("-u").output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The client version requested from `mount(8)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NfsVersion {
    #[default]
    V3,
    V4_1,
}

/// Native mount configuration. `server_options` configures the in-process
/// NFSv3/NFSv4.1 listener; the remaining fields configure the host client.
#[derive(Debug, Clone)]
pub struct NfsMountOptions {
    pub server_options: NfsServerOptions,
    pub version: NfsVersion,
    pub export_path: String,
    pub read_only: bool,
    pub hard: bool,
    pub timeo: Option<f64>,
    pub retrans: Option<f64>,
    pub nobrowse: bool,
    pub mount_options: Vec<String>,
    /// `None` means wait without a lifecycle deadline. `Some(Duration::ZERO)`
    /// has the same meaning for parity with upstream's `0` setting.
    pub unmount_timeout: Option<Duration>,
}

impl Default for NfsMountOptions {
    fn default() -> Self {
        Self {
            server_options: NfsServerOptions::default(),
            version: NfsVersion::V3,
            export_path: "/".into(),
            read_only: false,
            hard: false,
            timeo: None,
            retrans: None,
            nobrowse: true,
            mount_options: Vec::new(),
            unmount_timeout: Some(DEFAULT_UNMOUNT_TIMEOUT),
        }
    }
}

impl NfsMountOptions {
    /// Build a conservative profile for SQLite hosted through this process's
    /// loopback NFS server and one host kernel client.
    ///
    /// The profile uses a hard mount and local-only locking: `locallocks` on
    /// macOS and `local_lock=all` on Linux. It is intentionally scoped to a
    /// single host/client and does not provide distributed locking, cross-host
    /// coordination, or power-loss durability guarantees. General NFS callers
    /// retain the historical default profile, including soft mounts.
    pub fn sqlite_single_host() -> Self {
        let mut options = Self {
            hard: true,
            ..Self::default()
        };
        options
            .mount_options
            .push(SQLITE_SINGLE_HOST_PROFILE.to_owned());
        options
    }
}

/// Refuse the one client/version combination for which the upstream contract
/// has no interoperable server: macOS is treated as v4.0-only.
pub fn version_refusal(platform: NfsPlatform, version: NfsVersion) -> Option<String> {
    if version == NfsVersion::V4_1 && platform == NfsPlatform::Macos {
        Some(
            "mount-rs-nfs: NFSv4.1 is Linux-only; macOS is treated as NFSv4.0-only, and this "
                .to_owned()
                + "server does not provide a v4.0 compatibility mode. Use NFSv3 on macOS.",
        )
    } else {
        None
    }
}

/// Explain the macOS ownership rule for an unprivileged native mount.
pub fn ownership_refusal(
    platform: NfsPlatform,
    mountpoint: &Path,
    owner_uid: u32,
    caller_uid: u32,
) -> Option<String> {
    if platform != NfsPlatform::Macos || caller_uid == 0 || owner_uid == caller_uid {
        return None;
    }
    Some(format!(
        "mount-rs-nfs: macOS lets an ordinary user mount only onto a directory it owns; {} belongs to uid {owner_uid} while this process is uid {caller_uid}",
        mountpoint.display()
    ))
}

/// Advice for macOS's Full Disk Access/consent refusal path.
pub fn consent_advice(mountpoint: &Path) -> String {
    format!(
        "macOS refused access to the network volume at {}. Grant Full Disk Access to the terminal or process running this code under System Settings > Privacy & Security, then retry; sudo umount -f may be refused by the same policy.",
        mountpoint.display()
    )
}

/// Build the platform-correct `-o` value for the native client.
pub fn nfs_mount_options(
    port: u16,
    options: &NfsMountOptions,
    platform: NfsPlatform,
) -> Result<String, NfsMountError> {
    let sqlite_single_host = options
        .mount_options
        .iter()
        .any(|option| option == SQLITE_SINGLE_HOST_PROFILE);
    if sqlite_single_host {
        if options.version != NfsVersion::V3 {
            return Err(NfsMountError::InvalidOptions(
                "sqlite_single_host profile supports NFSv3 only".into(),
            ));
        }
        if !options.hard {
            return Err(NfsMountError::InvalidOptions(
                "sqlite_single_host profile requires hard=true".into(),
            ));
        }
        if let Some(option) = options
            .mount_options
            .iter()
            .find(|option| sqlite_profile_conflict(option))
        {
            return Err(NfsMountError::InvalidOptions(format!(
                "sqlite_single_host profile conflicts with mount option {option:?}"
            )));
        }
    }
    if let Some(message) = version_refusal(platform, options.version) {
        return Err(NfsMountError::UnsupportedVersion(message));
    }
    let mut parts = match options.version {
        NfsVersion::V3 => vec![
            "vers=3".to_owned(),
            "proto=tcp".to_owned(),
            format!("port={port}"),
            format!("mountport={port}"),
            if sqlite_single_host {
                match platform {
                    NfsPlatform::Macos => "locallocks".to_owned(),
                    NfsPlatform::Linux => "local_lock=all".to_owned(),
                }
            } else if platform == NfsPlatform::Macos {
                "nolocks".to_owned()
            } else {
                "nolock".to_owned()
            },
        ],
        NfsVersion::V4_1 => vec![
            "vers=4.1".into(),
            "proto=tcp".into(),
            format!("port={port}"),
        ],
    };
    if options.hard {
        if platform == NfsPlatform::Linux || sqlite_single_host {
            parts.push("hard".into());
        }
    } else {
        parts.push("soft".into());
    }
    parts.push(format!("timeo={}", option_count(options.timeo, 50, 1)));
    parts.push(format!("retrans={}", option_count(options.retrans, 2, 0)));
    if platform == NfsPlatform::Macos && options.nobrowse {
        parts.push("nobrowse".into());
    }
    if options.read_only {
        parts.push("ro".into());
    }
    parts.extend(
        options
            .mount_options
            .iter()
            .filter(|option| *option != SQLITE_SINGLE_HOST_PROFILE)
            .cloned(),
    );
    Ok(parts.join(","))
}

fn sqlite_profile_conflict(option: &str) -> bool {
    option.split(',').any(|token| {
        let name = token
            .trim()
            .split_once('=')
            .map_or(token.trim(), |(name, _)| name.trim());
        [
            "soft",
            "softerr",
            "softreval",
            "hard",
            "lock",
            "nolock",
            "nolocks",
            "nolockd",
            "nonlm",
            "locallocks",
            "nolocallocks",
            "local_lock",
        ]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
    })
}

fn option_count(value: Option<f64>, fallback: u64, floor: u64) -> u64 {
    match value {
        Some(value) if value.is_finite() => value.trunc().max(floor as f64) as u64,
        _ => fallback,
    }
}

/// A parsed entry from `/proc/self/mounts` or macOS `mount(8)` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub source: String,
    pub target: String,
    pub fs_type: String,
}

/// Parse either Linux's escaped mount table or macOS's prose output.
pub fn parse_mount_table(platform: NfsPlatform, table: &str) -> Vec<MountEntry> {
    table
        .lines()
        .filter_map(|line| match platform {
            NfsPlatform::Linux => {
                let mut fields = line.split_whitespace();
                let source = fields.next()?;
                let target = fields.next()?;
                let fs_type = fields.next()?;
                Some(MountEntry {
                    source: unescape_mount_path(source),
                    target: unescape_mount_path(target),
                    fs_type: fs_type.to_owned(),
                })
            }
            NfsPlatform::Macos => {
                let (source, rest) = line.rsplit_once(" on ")?;
                let (target, metadata) = rest.split_once(" (")?;
                let fs_type = metadata
                    .split([',', ')'])
                    .next()
                    .filter(|value| !value.is_empty())?;
                Some(MountEntry {
                    source: source.to_owned(),
                    target: target.to_owned(),
                    fs_type: fs_type.to_owned(),
                })
            }
        })
        .collect()
}

fn unescape_mount_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut result = String::with_capacity(path.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &bytes[index + 1..index + 4];
            let value = octal.iter().try_fold(0_u8, |value, digit| {
                (*digit >= b'0' && *digit <= b'7').then_some(value * 8 + (*digit - b'0'))
            });
            if let Some(value) = value {
                result.push(value as char);
                index += 4;
                continue;
            }
        }
        result.push(bytes[index] as char);
        index += 1;
    }
    result
}

/// Read the host mount table and find the last entry at `target`.
///
/// A table read failure is returned instead of being treated as "unmounted";
/// teardown uses that distinction to keep the server alive when it cannot
/// prove that a live kernel mount is gone.
pub async fn mount_entry_at(
    target: impl AsRef<Path>,
    platform: NfsPlatform,
) -> Result<Option<MountEntry>, NfsMountError> {
    let entries = read_mount_table(platform, None).await?;
    let target = target.as_ref().to_string_lossy();
    Ok(entries.into_iter().rfind(|entry| entry.target == target))
}

async fn read_mount_table(
    platform: NfsPlatform,
    timeout: Option<Duration>,
) -> Result<Vec<MountEntry>, NfsMountError> {
    match platform {
        NfsPlatform::Linux => fs::read_to_string("/proc/self/mounts")
            .map(|contents| parse_mount_table(platform, &contents))
            .map_err(|error| NfsMountError::MountTable(error.to_string())),
        NfsPlatform::Macos => {
            let result = run_command(
                "mount",
                &[],
                Some(timeout.unwrap_or(MACOS_MOUNT_TABLE_TIMEOUT)),
            )
            .await?;
            if result.timed_out {
                return Err(NfsMountError::MountTable(
                    "mount(8) did not return the mount table before the timeout".into(),
                ));
            }
            if result.status != Some(0) {
                return Err(NfsMountError::MountTable(format_command_failure(
                    "mount",
                    &[],
                    &result,
                )));
            }
            Ok(parse_mount_table(platform, &result.stdout))
        }
    }
}

/// Native mount errors retain command output so a caller can distinguish
/// missing privilege, missing helpers, and macOS privacy refusal.
#[derive(Debug)]
pub enum NfsMountError {
    UnsupportedPlatform(String),
    ClientUnavailable(String),
    UnsupportedVersion(String),
    InvalidOptions(String),
    InvalidMountpoint(String),
    InvalidExportPath(String),
    AlreadyMounted(String),
    CommandSpawn { program: String, message: String },
    CommandFailed { program: String, message: String },
    CommandTimedOut { program: String, timeout: Duration },
    MountTable(String),
    Server(io::Error),
    Io(io::Error),
    Unmount(String),
}

impl fmt::Display for NfsMountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform(message)
            | Self::ClientUnavailable(message)
            | Self::UnsupportedVersion(message)
            | Self::InvalidOptions(message)
            | Self::InvalidMountpoint(message)
            | Self::InvalidExportPath(message)
            | Self::AlreadyMounted(message)
            | Self::CommandFailed { message, .. }
            | Self::MountTable(message)
            | Self::Unmount(message) => formatter.write_str(message),
            Self::CommandSpawn { program, message } => {
                write!(formatter, "could not run {program}: {message}")
            }
            Self::CommandTimedOut { program, timeout } => {
                write!(formatter, "{program} did not return within {timeout:?}")
            }
            Self::Server(error) | Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NfsMountError {}

#[derive(Debug)]
struct CommandResult {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

async fn run_command(
    program: &str,
    args: &[String],
    timeout: Option<Duration>,
) -> Result<CommandResult, NfsMountError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|error| NfsMountError::CommandSpawn {
            program: program.to_owned(),
            message: error.to_string(),
        })?;
    let output = if let Some(timeout) = timeout {
        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(output) => output.map_err(|error| NfsMountError::CommandSpawn {
                program: program.to_owned(),
                message: error.to_string(),
            })?,
            Err(_) => {
                return Ok(CommandResult {
                    status: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: true,
                });
            }
        }
    } else {
        child
            .wait_with_output()
            .await
            .map_err(|error| NfsMountError::CommandSpawn {
                program: program.to_owned(),
                message: error.to_string(),
            })?
    };
    Ok(CommandResult {
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        timed_out: false,
    })
}

fn format_command_failure(program: &str, args: &[String], result: &CommandResult) -> String {
    let command = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let output = if result.stderr.trim().is_empty() {
        result.stdout.trim()
    } else {
        result.stderr.trim()
    };
    format!("{command} failed with status {:?}: {output}", result.status)
}

fn format_host(address: IpAddr) -> String {
    match address {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    }
}

fn validate_export_path(export_path: &str) -> Result<(), NfsMountError> {
    if export_path
        .chars()
        .any(|character| character.is_whitespace() || character == ',')
    {
        return Err(NfsMountError::InvalidExportPath(format!(
            "export path may not contain whitespace or a comma: {export_path:?}"
        )));
    }
    Ok(())
}

fn remaining(deadline: Option<Instant>) -> Option<Duration> {
    deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()))
}

fn deadline(timeout: Option<Duration>) -> Option<Instant> {
    timeout
        .filter(|timeout| !timeout.is_zero())
        .map(|timeout| Instant::now() + timeout)
}

struct NativeNfsMountInner {
    mountpoint: PathBuf,
    source: String,
    port: u16,
    platform: NfsPlatform,
    options: NfsMountOptions,
    server: NfsServer,
    stopping: AtomicBool,
    mounted: AtomicBool,
    finished: AtomicBool,
    unmount_lock: AsyncMutex<()>,
}

/// A live host-kernel NFS mount backed by an in-process NFSv3 or NFSv4.1 server.
#[derive(Clone)]
pub struct NativeNfsMount {
    inner: Arc<NativeNfsMountInner>,
}

impl NativeNfsMount {
    pub fn mountpoint(&self) -> &Path {
        &self.inner.mountpoint
    }

    pub fn source(&self) -> &str {
        &self.inner.source
    }

    pub fn port(&self) -> u16 {
        self.inner.port
    }

    pub fn server(&self) -> &NfsServer {
        &self.inner.server
    }

    /// False once teardown starts, including a retryable failed teardown.
    pub fn active(&self) -> bool {
        !self.inner.stopping.load(Ordering::Acquire)
    }

    /// Gracefully unmount, escalating to the platform's force ladder on a
    /// deadline. Concurrent callers serialize; a failed unmount leaves the
    /// server live and can be retried.
    pub async fn unmount(&self) -> Result<(), NfsMountError> {
        let _guard = self.inner.unmount_lock.lock().await;
        if !self.inner.mounted.load(Ordering::Acquire) {
            return self.finish().await;
        }
        self.inner.stopping.store(true, Ordering::Release);
        let timeout = self.inner.options.unmount_timeout;
        let deadline = deadline(timeout);

        match self.graceful_unmount(deadline).await? {
            UnmountOutcome::Done => {
                self.inner.mounted.store(false, Ordering::Release);
                self.finish().await
            }
            UnmountOutcome::TimedOut => self.force_unmount(timeout).await,
        }
    }

    async fn graceful_unmount(
        &self,
        deadline: Option<Instant>,
    ) -> Result<UnmountOutcome, NfsMountError> {
        if !self.mount_is_present(deadline).await {
            return Ok(UnmountOutcome::Done);
        }
        let args = vec![self.inner.mountpoint.to_string_lossy().into_owned()];
        let result = run_command("umount", &args, remaining(deadline)).await?;
        if result.timed_out {
            return Ok(UnmountOutcome::TimedOut);
        }
        if result.status == Some(0) || !self.mount_is_present(deadline).await {
            return Ok(UnmountOutcome::Done);
        }
        Err(NfsMountError::Unmount(format!(
            "could not unmount {}: {}",
            self.inner.mountpoint.display(),
            format_command_failure("umount", &args, &result),
        )))
    }

    async fn force_unmount(&self, timeout: Option<Duration>) -> Result<(), NfsMountError> {
        let deadline = deadline(timeout);
        let mountpoint = self.inner.mountpoint.to_string_lossy().into_owned();
        let ladder: &[&str] = if self.inner.platform == NfsPlatform::Macos {
            &["-f"]
        } else {
            &["-f", "-l"]
        };
        let mut consent_denied = false;
        for flag in ladder {
            if !self.mount_is_present(deadline).await {
                break;
            }
            let args = vec![(*flag).to_owned(), mountpoint.clone()];
            let result = run_command("umount", &args, remaining(deadline)).await?;
            if result.timed_out {
                continue;
            }
            if self.inner.platform == NfsPlatform::Macos
                && result
                    .stderr
                    .to_ascii_lowercase()
                    .contains("operation not permitted")
            {
                consent_denied = true;
            }
        }
        let still_present = self.mount_is_present(deadline).await;
        self.inner.mounted.store(!still_present, Ordering::Release);
        let close_result = self.finish().await;
        if consent_denied {
            return Err(NfsMountError::Unmount(consent_advice(
                &self.inner.mountpoint,
            )));
        }
        if still_present {
            return Err(NfsMountError::Unmount(format!(
                "unmounting {} exceeded its deadline; the server was stopped but the mount is still listed",
                self.inner.mountpoint.display()
            )));
        }
        close_result
    }

    async fn mount_is_present(&self, timeout: Option<Instant>) -> bool {
        match read_mount_table(self.inner.platform, remaining(timeout)).await {
            Ok(entries) => entries
                .iter()
                .any(|entry| entry.target == self.inner.mountpoint.to_string_lossy()),
            Err(_) => true,
        }
    }

    async fn finish(&self) -> Result<(), NfsMountError> {
        self.inner.mounted.store(false, Ordering::Release);
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.inner
            .server
            .close()
            .await
            .map_err(NfsMountError::Server)
    }
}

enum UnmountOutcome {
    Done,
    TimedOut,
}

static LIVE_MOUNTS: OnceLock<Mutex<Vec<Weak<NativeNfsMountInner>>>> = OnceLock::new();

fn live_registry() -> &'static Mutex<Vec<Weak<NativeNfsMountInner>>> {
    LIVE_MOUNTS.get_or_init(|| Mutex::new(Vec::new()))
}

fn register_mount(mount: &Arc<NativeNfsMountInner>) {
    let mut mounts = live_registry().lock().expect("NFS live mount registry");
    mounts.retain(|mount| mount.strong_count() != 0);
    mounts.push(Arc::downgrade(mount));
}

/// Return live native mounts created by this process, in creation order.
pub fn live_nfs_mounts() -> Vec<NativeNfsMount> {
    let mut mounts = live_registry().lock().expect("NFS live mount registry");
    let live = mounts.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
    mounts.retain(|mount| mount.strong_count() != 0);
    live.into_iter()
        .map(|inner| NativeNfsMount { inner })
        .collect()
}

/// Unmount every live native mount. Failures are returned individually and do
/// not prevent later mounts from receiving a teardown attempt.
pub async fn unmount_all_nfs() -> Vec<NfsMountError> {
    let mounts = live_nfs_mounts();
    let mut failures = Vec::new();
    for mount in mounts {
        if let Err(error) = mount.unmount().await {
            failures.push(error);
        }
    }
    failures
}

/// Start an NFS server and put the host kernel NFS client in front of it.
pub async fn mount_nfs<D>(
    driver: D,
    mountpoint: impl AsRef<Path>,
    options: NfsMountOptions,
) -> Result<NativeNfsMount, NfsMountError>
where
    D: FsDriver + 'static,
{
    let platform = nfs_platform().ok_or_else(|| {
        NfsMountError::UnsupportedPlatform(format!(
            "native NFS mounts are supported on Linux and macOS, not {}",
            std::env::consts::OS
        ))
    })?;
    if let Some(message) = version_refusal(platform, options.version) {
        return Err(NfsMountError::UnsupportedVersion(message));
    }
    let probe = nfs_client_probe();
    if !probe.usable {
        return Err(NfsMountError::ClientUnavailable(
            probe
                .reason
                .unwrap_or_else(|| "the host NFS client is unavailable".into()),
        ));
    }
    let resolved = fs::canonicalize(mountpoint.as_ref()).map_err(|error| {
        NfsMountError::InvalidMountpoint(format!(
            "{} is not usable: {error}",
            mountpoint.as_ref().display()
        ))
    })?;
    let metadata = fs::metadata(&resolved).map_err(|error| {
        NfsMountError::InvalidMountpoint(format!("{}: {error}", resolved.display()))
    })?;
    if !metadata.is_dir() {
        return Err(NfsMountError::InvalidMountpoint(format!(
            "{} is not a directory",
            resolved.display()
        )));
    }
    #[cfg(unix)]
    let owner_uid = {
        use std::os::unix::fs::MetadataExt;
        metadata.uid()
    };
    #[cfg(not(unix))]
    let owner_uid = 0;
    let caller_uid = effective_uid().unwrap_or(owner_uid);
    if let Some(message) = ownership_refusal(platform, &resolved, owner_uid, caller_uid) {
        return Err(NfsMountError::InvalidMountpoint(message));
    }
    if live_nfs_mounts()
        .iter()
        .any(|mount| mount.mountpoint() == resolved)
    {
        return Err(NfsMountError::AlreadyMounted(format!(
            "{} is already mounted by this process",
            resolved.display()
        )));
    }
    validate_export_path(&options.export_path)?;
    if let Ok(Some(entry)) = mount_entry_at(&resolved, platform).await {
        return Err(NfsMountError::AlreadyMounted(format!(
            "{} already has {} mounted at {}",
            resolved.display(),
            entry.fs_type,
            entry.source
        )));
    }

    let server = create_nfs_server(driver, options.server_options.clone());
    let address = server.listen().await.map_err(NfsMountError::Server)?;
    let port = address.port();
    let source = format!(
        "{}:{}",
        format_host(address.ip()),
        options.export_path.as_str()
    );
    let option_string = nfs_mount_options(port, &options, platform)?;
    let args = vec![
        "-t".into(),
        "nfs".into(),
        "-o".into(),
        option_string,
        source.clone(),
        resolved.to_string_lossy().into_owned(),
    ];
    let result = run_command("mount", &args, None).await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let _ = server.close().await;
            return Err(error);
        }
    };
    if result.timed_out || result.status != Some(0) {
        let error = if result.timed_out {
            NfsMountError::CommandTimedOut {
                program: "mount".into(),
                timeout: Duration::from_secs(0),
            }
        } else {
            NfsMountError::CommandFailed {
                program: "mount".into(),
                message: format_command_failure("mount", &args, &result),
            }
        };
        let _ = server.close().await;
        return Err(error);
    }
    let inner = Arc::new(NativeNfsMountInner {
        mountpoint: resolved,
        source,
        port,
        platform,
        options,
        server,
        stopping: AtomicBool::new(false),
        mounted: AtomicBool::new(true),
        finished: AtomicBool::new(false),
        unmount_lock: AsyncMutex::new(()),
    });
    register_mount(&inner);
    Ok(NativeNfsMount { inner })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_mapping_is_explicit() {
        assert_eq!(nfs_platform_for("linux"), Some(NfsPlatform::Linux));
        assert_eq!(nfs_platform_for("macos"), Some(NfsPlatform::Macos));
        assert_eq!(nfs_platform_for("windows"), None);
    }

    #[test]
    fn v3_options_use_platform_lock_spelling() {
        let options = NfsMountOptions::default();
        assert_eq!(
            nfs_mount_options(2049, &options, NfsPlatform::Linux).expect("linux options"),
            "vers=3,proto=tcp,port=2049,mountport=2049,nolock,soft,timeo=50,retrans=2"
        );
        assert_eq!(
            nfs_mount_options(2049, &options, NfsPlatform::Macos).expect("macOS options"),
            "vers=3,proto=tcp,port=2049,mountport=2049,nolocks,soft,timeo=50,retrans=2,nobrowse"
        );
    }

    #[test]
    fn sqlite_single_host_profile_is_hard_and_local_only() {
        let options = NfsMountOptions::sqlite_single_host();
        assert_eq!(
            nfs_mount_options(2049, &options, NfsPlatform::Linux).expect("linux options"),
            "vers=3,proto=tcp,port=2049,mountport=2049,local_lock=all,hard,timeo=50,retrans=2"
        );
        assert_eq!(
            nfs_mount_options(2049, &options, NfsPlatform::Macos).expect("macOS options"),
            "vers=3,proto=tcp,port=2049,mountport=2049,locallocks,hard,timeo=50,retrans=2,nobrowse"
        );
    }

    #[test]
    fn sqlite_single_host_profile_rejects_conflicting_mount_options() {
        let mut options = NfsMountOptions::sqlite_single_host();
        options.mount_options.push("soft".into());
        assert!(matches!(
            nfs_mount_options(2049, &options, NfsPlatform::Macos),
            Err(NfsMountError::InvalidOptions(_))
        ));

        let mut options = NfsMountOptions::sqlite_single_host();
        options.mount_options.push("nolock".into());
        assert!(matches!(
            nfs_mount_options(2049, &options, NfsPlatform::Linux),
            Err(NfsMountError::InvalidOptions(_))
        ));

        let mut options = NfsMountOptions::sqlite_single_host();
        options.mount_options.push("timeo=50,soft".into());
        assert!(matches!(
            nfs_mount_options(2049, &options, NfsPlatform::Macos),
            Err(NfsMountError::InvalidOptions(_))
        ));

        let mut options = NfsMountOptions::sqlite_single_host();
        options.mount_options.push("rsize=4096,nolocks".into());
        assert!(matches!(
            nfs_mount_options(2049, &options, NfsPlatform::Macos),
            Err(NfsMountError::InvalidOptions(_))
        ));

        let mut options = NfsMountOptions::sqlite_single_host();
        options.hard = false;
        assert!(matches!(
            nfs_mount_options(2049, &options, NfsPlatform::Macos),
            Err(NfsMountError::InvalidOptions(_))
        ));
    }

    #[test]
    fn sqlite_single_host_profile_rejects_nfs_v4() {
        let mut options = NfsMountOptions::sqlite_single_host();
        options.version = NfsVersion::V4_1;
        assert!(matches!(
            nfs_mount_options(2049, &options, NfsPlatform::Linux),
            Err(NfsMountError::InvalidOptions(_))
        ));
        assert!(matches!(
            nfs_mount_options(2049, &options, NfsPlatform::Macos),
            Err(NfsMountError::InvalidOptions(_))
        ));
    }

    #[test]
    fn v4_options_are_linux_only_and_do_not_add_v3_controls() {
        let options = NfsMountOptions {
            version: NfsVersion::V4_1,
            hard: true,
            ..NfsMountOptions::default()
        };
        assert_eq!(
            nfs_mount_options(3000, &options, NfsPlatform::Linux).expect("v4 options"),
            "vers=4.1,proto=tcp,port=3000,hard,timeo=50,retrans=2"
        );
        assert!(matches!(
            nfs_mount_options(3000, &options, NfsPlatform::Macos),
            Err(NfsMountError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn option_counts_match_mountx_clamping() {
        let options = NfsMountOptions {
            timeo: Some(f64::NAN),
            retrans: Some(-3.8),
            mount_options: vec!["locallocks".into()],
            ..NfsMountOptions::default()
        };
        let value = nfs_mount_options(1, &options, NfsPlatform::Macos).expect("options");
        assert!(value.contains("timeo=50"));
        assert!(value.contains("retrans=0"));
        assert!(value.ends_with(",locallocks"));
    }

    #[test]
    fn mount_tables_parse_linux_escapes_and_darwin_prose() {
        let linux = parse_mount_table(
            NfsPlatform::Linux,
            "server:/a\\040b /private/a\\040b nfs rw 0 0\n",
        );
        assert_eq!(linux[0].target, "/private/a b");
        let darwin = parse_mount_table(
            NfsPlatform::Macos,
            "127.0.0.1:/ on /private/tmp (nfs, nodev, nosuid)\n",
        );
        assert_eq!(darwin[0].fs_type, "nfs");
        assert_eq!(darwin[0].target, "/private/tmp");
    }

    #[test]
    fn refusal_messages_cover_macos_ownership_and_v4() {
        assert!(ownership_refusal(NfsPlatform::Macos, Path::new("/tmp/mount"), 501, 502).is_some());
        assert!(ownership_refusal(NfsPlatform::Macos, Path::new("/tmp/mount"), 501, 0).is_none());
        assert!(ownership_refusal(NfsPlatform::Linux, Path::new("/tmp/mount"), 501, 502).is_none());
        assert!(version_refusal(NfsPlatform::Macos, NfsVersion::V4_1).is_some());
        assert!(version_refusal(NfsPlatform::Linux, NfsVersion::V4_1).is_none());
    }
}
