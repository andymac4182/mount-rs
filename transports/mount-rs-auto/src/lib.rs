//! Automatic native transport selection for mount-rs.
//!
//! The facade probes FUSE, then Linux 9P, then NFS on Linux. On macOS it
//! probes NFS first; macFUSE is a different protocol and there is no native
//! 9P client. A named transport bypasses probing and is attempted exactly
//! once: a mount error is never hidden by falling back to another transport.
//!
//! The transport implementations remain separate crates. This crate only
//! owns the selection policy, typed option composition, and a tagged lifecycle
//! wrapper around their existing native mount APIs.
//!
//! Wire and probe tests are rootless and do not verify a kernel mount. An
//! actual Linux FUSE mount needs `/dev/fuse` plus `CAP_SYS_ADMIN` or an
//! executable `fusermount3`/`fusermount`; an actual Linux 9P mount additionally
//! needs the `v9fs`/`9pnet_fd` client and mount capability. Linux NFS needs a
//! usable `mount.nfs`/kernel client and the corresponding mount capability.
//! macOS uses the native NFS client (`/sbin/mount_nfs`), subject to its
//! directory-ownership and privacy/consent rules; macFUSE is not treated as a
//! compatible implementation of this Linux FUSE transport, and macOS has no
//! native v9fs client.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use mount_rs_core::FsDriver;

pub use mount_rs_9p::{
    P9ClientProbe, P9Mount, P9MountOptions, P9MountTransport, P9Platform, P9ServerHooks,
};
pub use mount_rs_fuse::mount::{
    FuseMount, FuseMountHooks, MountError as FuseMountError, MountMode, MountOptions,
};
pub use mount_rs_nfs::{
    NativeNfsMount, NfsClientProbe, NfsMountError, NfsMountOptions, NfsPlatform, NfsServerHooks,
    NfsVersion,
};

/// A transport that the facade can select or be asked to use by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Transport {
    Fuse,
    P9,
    Nfs,
}

/// Selection mode for [`AutoMountOptions`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AutoTransport {
    #[default]
    Auto,
    Fuse,
    P9,
    Nfs,
}

impl AutoTransport {
    fn named(self) -> Option<Transport> {
        match self {
            Self::Auto => None,
            Self::Fuse => Some(Transport::Fuse),
            Self::P9 => Some(Transport::P9),
            Self::Nfs => Some(Transport::Nfs),
        }
    }
}

/// Result of probing one transport without attempting a mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportProbe {
    pub usable: bool,
    pub reason: Option<String>,
}

/// Host capabilities and the transport automatic selection would choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoProbe {
    pub platform: String,
    pub chosen: Option<Transport>,
    pub preference: Vec<Transport>,
    pub fuse: TransportProbe,
    pub p9: TransportProbe,
    pub nfs: TransportProbe,
    pub reason: Option<String>,
}

/// Common options plus a complete, strongly typed override for each transport.
///
/// The shared fields provide defaults when no transport-specific options are
/// supplied. A supplied transport-specific value is a complete native option
/// object and therefore wins for that transport, matching the upstream
/// facade's `{ ...shared, ...specific }` merge. Named selection never invokes
/// [`probe_transports`].
#[derive(Clone)]
pub struct AutoMountOptions {
    pub transport: AutoTransport,
    pub read_only: Option<bool>,
    pub unmount_timeout: Option<Duration>,
    pub fuse: Option<MountOptions>,
    pub p9: Option<P9MountOptions>,
    pub nfs: Option<NfsMountOptions>,
}

impl Default for AutoMountOptions {
    fn default() -> Self {
        Self {
            transport: AutoTransport::Auto,
            read_only: None,
            unmount_timeout: None,
            fuse: None,
            p9: None,
            nfs: None,
        }
    }
}

impl AutoMountOptions {
    fn fuse_options(&self) -> MountOptions {
        if let Some(options) = &self.fuse {
            return options.clone();
        }
        let mut options = MountOptions::default();
        if let Some(read_only) = self.read_only {
            options.read_only = read_only;
        }
        if let Some(unmount_timeout) = self.unmount_timeout {
            options.unmount_timeout = unmount_timeout;
        }
        options
    }

    fn p9_options(&self) -> P9MountOptions {
        if let Some(options) = &self.p9 {
            return options.clone();
        }
        P9MountOptions {
            read_only: self.read_only.unwrap_or_default(),
            unmount_timeout: self.unmount_timeout,
            ..P9MountOptions::default()
        }
    }

    fn nfs_options(&self) -> NfsMountOptions {
        if let Some(options) = &self.nfs {
            return options.clone();
        }
        NfsMountOptions {
            read_only: self.read_only.unwrap_or_default(),
            unmount_timeout: self.unmount_timeout,
            ..NfsMountOptions::default()
        }
    }
}

/// Transport lifecycle hooks for the automatic facade.
///
/// The hook object is separate from [`AutoMountOptions`] so existing option
/// literals remain source-compatible. A supplied 9P or NFS hook replaces the
/// hook set on a mount-created server; an omitted hook preserves the options'
/// existing 9P hook and leaves the NFS hook unset.
#[derive(Clone, Default)]
pub struct AutoMountHooks {
    pub fuse: FuseMountHooks,
    pub p9: Option<P9ServerHooks>,
    pub nfs: Option<NfsServerHooks>,
}

/// A native mount error tagged with the transport that produced it.
#[derive(Debug)]
pub enum AutoMountError {
    NoTransport(String),
    Fuse(FuseMountError),
    P9(io::Error),
    Nfs(NfsMountError),
}

impl fmt::Display for AutoMountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTransport(message) => formatter.write_str(message),
            Self::Fuse(error) => error.fmt(formatter),
            Self::P9(error) => error.fmt(formatter),
            Self::Nfs(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AutoMountError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fuse(error) => Some(error),
            Self::P9(error) => Some(error),
            Self::Nfs(error) => Some(error),
            Self::NoTransport(_) => None,
        }
    }
}

/// A successful mount tagged with the serving transport.
///
/// FUSE and 9P mounts are held behind `Arc` because their transport crates do
/// not expose `Clone`; the underlying lifecycle methods operate by shared
/// reference. NFS already has a cloneable shared inner lifecycle.
#[derive(Clone)]
pub enum AutoMount {
    Fuse {
        mount: Arc<FuseMount>,
        mountpoint: PathBuf,
    },
    P9 {
        mount: Arc<P9Mount>,
        mountpoint: PathBuf,
    },
    Nfs {
        mount: NativeNfsMount,
        mountpoint: PathBuf,
    },
}

impl fmt::Debug for AutoMount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AutoMount")
            .field("transport", &self.transport())
            .field("mountpoint", &self.mountpoint())
            .field("active", &self.active())
            .finish()
    }
}

impl AutoMount {
    pub fn transport(&self) -> Transport {
        match self {
            Self::Fuse { .. } => Transport::Fuse,
            Self::P9 { .. } => Transport::P9,
            Self::Nfs { .. } => Transport::Nfs,
        }
    }

    pub fn mountpoint(&self) -> &Path {
        match self {
            Self::Fuse { mountpoint, .. }
            | Self::P9 { mountpoint, .. }
            | Self::Nfs { mountpoint, .. } => mountpoint,
        }
    }

    /// The transport source where the underlying API exposes one. FUSE uses
    /// its configured `fsname`; unsupported-platform FUSE mounts have none.
    pub fn source(&self) -> Option<&str> {
        match self {
            Self::Fuse { mount, .. } => mount.source(),
            Self::P9 { mount, .. } => Some(mount.source.as_str()),
            Self::Nfs { mount, .. } => Some(mount.source()),
        }
    }

    pub fn active(&self) -> bool {
        match self {
            Self::Fuse { mount, .. } => fuse_active(mount),
            Self::P9 { mount, .. } => mount.active(),
            Self::Nfs { mount, .. } => mount.active(),
        }
    }

    /// Delegate teardown to the underlying transport and remove successful
    /// facade-created mounts from the local registry. A failed teardown keeps
    /// the mount registered so callers can retry.
    pub async fn unmount(&self) -> Result<(), AutoMountError> {
        let result = match self {
            Self::Fuse { mount, .. } => fuse_unmount(mount).await.map_err(AutoMountError::Fuse),
            Self::P9 { mount, .. } => mount.unmount().await.map_err(AutoMountError::P9),
            Self::Nfs { mount, .. } => mount.unmount().await.map_err(AutoMountError::Nfs),
        };
        if result.is_ok() {
            unregister_mountpoint(self.mountpoint());
        }
        result
    }
}

static LOADED: OnceLock<Mutex<BTreeSet<Transport>>> = OnceLock::new();
static LIVE: OnceLock<Mutex<Vec<AutoMount>>> = OnceLock::new();

fn loaded_registry() -> &'static Mutex<BTreeSet<Transport>> {
    LOADED.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn live_registry() -> &'static Mutex<Vec<AutoMount>> {
    LIVE.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn mark_loaded(transport: Transport) {
    lock_recover(loaded_registry()).insert(transport);
}

/// Transports this facade has dispatched to. Rust links the transport crates
/// statically, so this means “attempted through this facade”, not a dynamic
/// module-import event.
pub fn loaded_transports() -> Vec<Transport> {
    lock_recover(loaded_registry()).iter().copied().collect()
}

fn register_mount(mount: AutoMount) {
    lock_recover(live_registry()).push(mount);
}

fn unregister_mountpoint(mountpoint: &Path) {
    lock_recover(live_registry()).retain(|mount| mount.mountpoint() != mountpoint);
}

/// Facade-created live FUSE/9P mounts, plus all live NFS mounts once this
/// facade has dispatched to NFS. The underlying Rust FUSE and 9P crates expose
/// no process-wide registry, so direct mounts made through those crates remain
/// intentionally outside this result rather than being misreported.
pub fn live_mounts() -> Vec<AutoMount> {
    let mut local = lock_recover(live_registry());
    local.retain(AutoMount::active);
    let mut mounts = local.clone();
    drop(local);

    if loaded(Transport::Nfs) {
        for mount in mount_rs_nfs::live_nfs_mounts() {
            let mountpoint = mount.mountpoint().to_owned();
            if !mounts
                .iter()
                .any(|existing| existing.mountpoint() == mountpoint)
            {
                mounts.push(AutoMount::Nfs { mount, mountpoint });
            }
        }
    }
    mounts
}

fn loaded(transport: Transport) -> bool {
    lock_recover(loaded_registry()).contains(&transport)
}

/// Unmount every mount visible to this facade. This never rejects: each
/// transport error is returned individually. NFS uses its existing process
/// registry; FUSE and 9P can only include mounts created through this facade.
pub async fn unmount_all() -> Vec<AutoMountError> {
    let mut failures = Vec::new();

    if loaded(Transport::Nfs) {
        failures.extend(
            mount_rs_nfs::unmount_all_nfs()
                .await
                .into_iter()
                .map(AutoMountError::Nfs),
        );
    }

    let mounts = lock_recover(live_registry()).clone();
    for mount in mounts {
        if mount.transport() == Transport::Nfs && loaded(Transport::Nfs) {
            continue;
        }
        if let Err(error) = mount.unmount().await {
            failures.push(error);
        }
    }
    lock_recover(live_registry()).retain(AutoMount::active);
    failures
}

/// Probe the current host using the automatic preference order.
pub fn probe_transports() -> AutoProbe {
    probe_transports_for(std::env::consts::OS)
}

/// Probe a named platform while retaining real host facts for that platform's
/// client. This is the deterministic seam used by portable tests.
pub fn probe_transports_for(platform: &str) -> AutoProbe {
    let fuse = probe_fuse(platform);
    let p9 = probe_p9(platform);
    let nfs = probe_nfs(platform);
    let preference = if platform == "linux" {
        vec![Transport::Fuse, Transport::P9, Transport::Nfs]
    } else {
        vec![Transport::Nfs, Transport::Fuse, Transport::P9]
    };
    let chosen = preference
        .iter()
        .copied()
        .find(|transport| match transport {
            Transport::Fuse => fuse.usable,
            Transport::P9 => p9.usable,
            Transport::Nfs => nfs.usable,
        });
    let reason = (chosen.is_none()).then(|| {
        format!(
            "no transport can mount on this host — FUSE: {}; 9P: {}; NFS: {}",
            reason_text(&fuse),
            reason_text(&p9),
            reason_text(&nfs),
        )
    });
    AutoProbe {
        platform: platform.to_owned(),
        chosen,
        preference,
        fuse,
        p9,
        nfs,
        reason,
    }
}

fn reason_text(probe: &TransportProbe) -> &str {
    probe.reason.as_deref().unwrap_or("usable")
}

fn usable() -> TransportProbe {
    TransportProbe {
        usable: true,
        reason: None,
    }
}

fn unusable(reason: impl Into<String>) -> TransportProbe {
    TransportProbe {
        usable: false,
        reason: Some(reason.into()),
    }
}

fn probe_fuse(platform: &str) -> TransportProbe {
    if platform != "linux" {
        return unusable(if platform == "darwin" || platform == "macos" {
            "FUSE needs Linux, and this is macOS — macFUSE is a third-party kernel extension "
                .to_owned()
                + "speaking its own protocol dialect, which mountx does not implement"
        } else {
            format!("FUSE needs Linux, this is {platform}")
        });
    }
    if !Path::new("/dev/fuse").exists() {
        return unusable(
            "no /dev/fuse — the fuse module is not loaded, or this container was not given "
                .to_owned()
                + "the device (docker: --device /dev/fuse)",
        );
    }
    if current_uid_is_root() || find_executable(&["fusermount3", "fusermount"]).is_some() {
        usable()
    } else {
        unusable("rootless FUSE mounting requires fusermount3 or fusermount on PATH")
    }
}

fn p9_client_probe_for(platform: &str) -> P9ClientProbe {
    let mut probe = if platform != "linux" {
        P9ClientProbe {
            usable: false,
            platform: None,
            kernel: false,
            transport: false,
            modules: false,
            root: current_uid_is_root(),
            reason: None,
        }
    } else if cfg!(target_os = "linux") {
        mount_rs_9p::p9_client_probe()
    } else {
        // The TypeScript probe accepts a platform override for deterministic
        // tests, but it only reads Linux's procfs when the actual process is
        // Linux. Preserve that distinction instead of reporting this host's
        // macOS/Windows facts as facts about the requested Linux host.
        P9ClientProbe {
            usable: false,
            platform: Some(P9Platform::Linux),
            kernel: false,
            transport: false,
            modules: false,
            root: current_uid_is_root(),
            reason: None,
        }
    };
    probe.reason = p9_probe_reason(platform, &probe);
    probe.usable = probe.reason.is_none();
    probe
}

fn p9_probe_reason(platform: &str, probe: &P9ClientProbe) -> Option<String> {
    if platform != "linux" {
        return Some(format!(
            "this is {platform}; 9P mounts on Linux only — no other kernel has a v9fs client"
        ));
    }

    let mut missing = Vec::new();
    if !probe.root {
        missing.push(
            "mounting 9P needs root: mount(2) needs CAP_SYS_ADMIN and v9fs has no setuid helper "
                .to_owned()
                + "the way FUSE has fusermount3",
        );
    }
    if !probe.kernel {
        let release = kernel_release_for_probe().unwrap_or_else(|| "<unknown release>".to_owned());
        missing.push(if probe.modules {
            format!(
                "no `9p` in /proc/filesystems (the module is not loaded; `modprobe 9p` should find it under /lib/modules/{release})"
            )
        } else {
            format!(
                "the kernel has no 9p filesystem and no module tree at /lib/modules/{release} to load one from"
            )
        });
    }
    (!missing.is_empty()).then(|| missing.join("; "))
}

#[cfg(target_os = "linux")]
fn kernel_release_for_probe() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|release| release.trim().to_owned())
        .filter(|release| !release.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn kernel_release_for_probe() -> Option<String> {
    None
}

fn probe_p9(platform: &str) -> TransportProbe {
    let probe = p9_client_probe_for(platform);
    if !probe.usable {
        return unusable(
            probe
                .reason
                .unwrap_or_else(|| "the Linux 9P client is unavailable".to_owned()),
        );
    }
    if let Some(reason) = p9_module_refusal(&probe) {
        unusable(reason)
    } else {
        usable()
    }
}

fn nfs_probe_reason(platform: &str, probe: &NfsClientProbe) -> String {
    let mut missing = Vec::new();
    match platform {
        "linux" => {
            if !probe.root {
                missing.push(
                    "mounting NFS needs root on Linux and this process is not root".to_owned(),
                );
            }
            if probe.helper.is_none() && !probe.kernel {
                missing.push(
                    "no /sbin/mount.nfs (install nfs-common / nfs-utils) and no `nfs` in "
                        .to_owned()
                        + "/proc/filesystems",
                );
            }
        }
        "darwin" => {
            if probe.helper.is_none() && !probe.kernel {
                missing
                    .push("no /sbin/mount_nfs, which every macOS is supposed to have".to_owned());
            }
        }
        _ => missing.push(format!(
            "this is {platform}; the NFS transport mounts on Linux and macOS only"
        )),
    }
    if missing.is_empty() {
        probe
            .reason
            .clone()
            .unwrap_or_else(|| "the host NFS client is unavailable".to_owned())
    } else {
        missing.join("; ")
    }
}

fn probe_nfs(platform: &str) -> TransportProbe {
    let probe = mount_rs_nfs::native::nfs_client_probe_for(platform);
    if probe.usable {
        usable()
    } else {
        unusable(nfs_probe_reason(platform, &probe))
    }
}

/// The automatic-only stricter 9P refusal. Named 9P selection intentionally
/// bypasses this function and delegates directly to `mount_9p`.
pub fn p9_module_refusal(probe: &P9ClientProbe) -> Option<String> {
    if !probe.usable || probe.transport || probe.modules {
        return None;
    }
    Some(
        "the kernel has a 9p filesystem but no 9pnet_fd in /sys/module — the module registering "
            .to_owned()
            + "the trans=unix this transport mounts with — and no module tree to load it from, "
            + "which is what a virtio-only guest looks like; `mountx --transport 9p` will try it "
            + "anyway",
    )
}

fn choose_transport(
    requested: AutoTransport,
    probe: Option<&AutoProbe>,
) -> Result<Transport, AutoMountError> {
    if let Some(named) = requested.named() {
        return Ok(named);
    }
    let probe = probe.ok_or_else(|| {
        AutoMountError::NoTransport("automatic selection requires a probe".to_owned())
    })?;
    probe.chosen.ok_or_else(|| {
        AutoMountError::NoTransport(
            probe
                .reason
                .clone()
                .unwrap_or_else(|| "no transport can mount on this host".to_owned()),
        )
    })
}

/// Mount `driver` at `mountpoint` using the named transport or the host's
/// automatic choice. The selected transport is called once; errors are
/// returned as-is through [`AutoMountError`] and no fallback is attempted.
pub async fn mount<D, P>(
    driver: D,
    mountpoint: P,
    options: AutoMountOptions,
) -> Result<AutoMount, AutoMountError>
where
    D: FsDriver + 'static,
    P: AsRef<Path>,
{
    mount_with_hooks(driver, mountpoint, options, AutoMountHooks::default()).await
}

/// Mount `driver` through the automatic facade with transport lifecycle hooks.
/// The selected transport is called once; no fallback is attempted after a
/// named or automatically selected mount fails.
pub async fn mount_with_hooks<D, P>(
    driver: D,
    mountpoint: P,
    options: AutoMountOptions,
    hooks: AutoMountHooks,
) -> Result<AutoMount, AutoMountError>
where
    D: FsDriver + 'static,
    P: AsRef<Path>,
{
    let probe = if options.transport.named().is_none() {
        Some(probe_transports())
    } else {
        None
    };
    let transport = choose_transport(options.transport, probe.as_ref())?;
    mark_loaded(transport);
    let requested_mountpoint = mountpoint.as_ref().to_owned();
    let fuse_options = options.fuse_options();
    let mut p9_options = options.p9_options();
    if let Some(server_hooks) = hooks.p9 {
        p9_options.server_hooks = server_hooks;
    }
    let nfs_options = options.nfs_options();

    let mounted = match transport {
        Transport::Fuse => {
            let mount = mount_rs_fuse::mount::mount_with_hooks(
                Arc::new(driver),
                &requested_mountpoint,
                fuse_options,
                hooks.fuse,
            )
            .await
            .map_err(AutoMountError::Fuse)?;
            let mountpoint = fuse_mountpoint(&mount, requested_mountpoint);
            AutoMount::Fuse {
                mount: Arc::new(mount),
                mountpoint,
            }
        }
        Transport::P9 => {
            let mount = mount_rs_9p::mount_9p(driver, &requested_mountpoint, p9_options)
                .await
                .map_err(AutoMountError::P9)?;
            let mountpoint = mount.mountpoint.clone();
            AutoMount::P9 {
                mount: Arc::new(mount),
                mountpoint,
            }
        }
        Transport::Nfs => {
            let mount = mount_rs_nfs::mount_nfs_with_hooks(
                driver,
                &requested_mountpoint,
                nfs_options,
                hooks.nfs.unwrap_or_default(),
            )
            .await
            .map_err(AutoMountError::Nfs)?;
            let mountpoint = mount.mountpoint().to_owned();
            AutoMount::Nfs { mount, mountpoint }
        }
    };
    register_mount(mounted.clone());
    Ok(mounted)
}

#[cfg(target_os = "linux")]
fn fuse_mountpoint(mount: &FuseMount, _requested: PathBuf) -> PathBuf {
    mount.mountpoint().to_owned()
}

#[cfg(not(target_os = "linux"))]
fn fuse_mountpoint(_mount: &FuseMount, requested: PathBuf) -> PathBuf {
    requested
}

#[cfg(target_os = "linux")]
fn fuse_active(mount: &FuseMount) -> bool {
    mount.is_active()
}

#[cfg(not(target_os = "linux"))]
fn fuse_active(_mount: &FuseMount) -> bool {
    false
}

#[cfg(target_os = "linux")]
async fn fuse_unmount(mount: &FuseMount) -> Result<(), FuseMountError> {
    mount.unmount().await
}

#[cfg(not(target_os = "linux"))]
async fn fuse_unmount(_mount: &FuseMount) -> Result<(), FuseMountError> {
    Err(FuseMountError::UnsupportedPlatform)
}

fn find_executable(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for name in names {
            let candidate = directory.join(name);
            if executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
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

fn current_uid_is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and does not retain pointers.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p9_probe(overrides: impl FnOnce(&mut P9ClientProbe)) -> P9ClientProbe {
        let mut probe = P9ClientProbe {
            usable: true,
            platform: Some(mount_rs_9p::P9Platform::Linux),
            kernel: true,
            transport: true,
            modules: true,
            root: true,
            reason: None,
        };
        overrides(&mut probe);
        probe
    }

    #[test]
    fn preference_and_platform_reasons_are_deterministic() {
        assert_eq!(
            probe_transports_for("linux").preference,
            vec![Transport::Fuse, Transport::P9, Transport::Nfs]
        );
        assert_eq!(
            probe_transports_for("darwin").preference,
            vec![Transport::Nfs, Transport::Fuse, Transport::P9]
        );
        let darwin = probe_transports_for("darwin");
        assert!(!darwin.fuse.usable);
        assert_eq!(
            darwin.fuse.reason.as_deref(),
            Some(concat!(
                "FUSE needs Linux, and this is macOS — macFUSE is a third-party kernel ",
                "extension speaking its own protocol dialect, which mountx does not implement"
            ))
        );
        assert!(!darwin.p9.usable);
        assert_eq!(
            darwin.p9.reason.as_deref(),
            Some("this is darwin; 9P mounts on Linux only — no other kernel has a v9fs client")
        );
        let win32 = probe_transports_for("win32");
        assert_eq!(
            win32.fuse.reason.as_deref(),
            Some("FUSE needs Linux, this is win32")
        );
    }

    #[test]
    fn unsupported_platform_reports_all_transport_reasons() {
        let probe = probe_transports_for("win32");
        assert_eq!(probe.chosen, None);
        assert!(!probe.fuse.usable);
        assert!(!probe.p9.usable);
        assert!(!probe.nfs.usable);
        let reason = probe.reason.expect("an unsupported platform has no choice");
        assert!(reason.contains("FUSE:"));
        assert!(reason.contains("9P:"));
        assert!(reason.contains("NFS:"));
        assert!(reason.contains("win32"));
    }

    #[test]
    fn p9_refusal_is_stricter_only_for_automatic_selection() {
        assert_eq!(p9_module_refusal(&p9_probe(|_| {})), None);
        assert_eq!(
            p9_module_refusal(&p9_probe(|probe| {
                probe.transport = false;
                probe.modules = true;
            })),
            None
        );
        let refusal = p9_module_refusal(&p9_probe(|probe| {
            probe.transport = false;
            probe.modules = false;
        }))
        .unwrap();
        assert!(refusal.contains("9pnet_fd"));
        assert!(refusal.contains("trans=unix"));
        assert!(refusal.contains("--transport 9p"));
        assert_eq!(
            refusal,
            "the kernel has a 9p filesystem but no 9pnet_fd in /sys/module — the module registering "
                .to_owned()
                + "the trans=unix this transport mounts with — and no module tree to load it from, "
                + "which is what a virtio-only guest looks like; `mountx --transport 9p` will try it "
                + "anyway"
        );

        let unusable = p9_probe(|probe| {
            probe.usable = false;
            probe.reason = Some("needs root".to_owned());
            probe.transport = false;
            probe.modules = false;
        });
        assert_eq!(p9_module_refusal(&unusable), None);
    }

    #[test]
    fn named_selection_bypasses_probe_and_never_falls_back() {
        let no_transport = AutoProbe {
            platform: "linux".to_owned(),
            chosen: None,
            preference: vec![Transport::Fuse, Transport::P9, Transport::Nfs],
            fuse: unusable("fuse failed"),
            p9: unusable("9p failed"),
            nfs: usable(),
            reason: Some("all probes refused".to_owned()),
        };
        assert_eq!(
            choose_transport(AutoTransport::P9, Some(&no_transport)).unwrap(),
            Transport::P9
        );
        assert_eq!(
            choose_transport(AutoTransport::Fuse, None).unwrap(),
            Transport::Fuse
        );
        assert_error_code(
            choose_transport(AutoTransport::Auto, Some(&no_transport)),
            AutoMountError::NoTransport("all probes refused".to_owned()),
        );
    }

    #[test]
    fn shared_options_supply_defaults_and_specific_options_win() {
        let options = AutoMountOptions {
            read_only: Some(true),
            unmount_timeout: Some(Duration::from_millis(25)),
            ..AutoMountOptions::default()
        };
        let fuse = options.fuse_options();
        assert!(fuse.read_only);
        assert_eq!(fuse.unmount_timeout, Duration::from_millis(25));

        let options = AutoMountOptions {
            read_only: Some(true),
            unmount_timeout: Some(Duration::from_millis(25)),
            fuse: Some(MountOptions {
                read_only: false,
                unmount_timeout: Duration::from_secs(2),
                ..MountOptions::default()
            }),
            ..AutoMountOptions::default()
        };
        let fuse = options.fuse_options();
        assert!(!fuse.read_only);
        assert_eq!(fuse.unmount_timeout, Duration::from_secs(2));
    }

    fn assert_error_code(result: Result<Transport, AutoMountError>, expected: AutoMountError) {
        match result {
            Ok(transport) => panic!("expected {expected:?}, got {transport:?}"),
            Err(AutoMountError::NoTransport(message)) => match expected {
                AutoMountError::NoTransport(expected_message) => {
                    assert_eq!(message, expected_message)
                }
                _ => panic!("unexpected error variant"),
            },
            Err(_) => panic!("unexpected error variant"),
        }
    }
}
