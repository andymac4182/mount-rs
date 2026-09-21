//! Linux v9fs mount lifecycle and a platform-neutral capability probe.
//!
//! Wire and listener tests do not need a kernel client or privileges. The
//! [`mount_9p`] convenience is intentionally a separate, Linux-gated layer:
//! it creates a private Unix socket by default, starts the ordinary server,
//! invokes `mount(8)`, adopts the new connection, and tears the server down
//! after `umount(8)` or an externally initiated EOF.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mount_rs_core::FsDriver;
#[cfg(target_os = "linux")]
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
#[cfg(target_os = "linux")]
use tokio::time::timeout;

use crate::constants::{P9_IOHDRSZ, P9_MIN_MSIZE};
use crate::server::{P9Server, P9ServerHooks, P9ServerOptions};

pub const P9_DEFAULT_MOUNT_MSIZE: u32 = 128 * 1024 + P9_IOHDRSZ;
pub const P9_MAX_MOUNT_MSIZE: u32 = 1024 * 1024;
pub const P9_UNIX_PATH_MAX: usize = 108;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum P9Platform {
    Linux,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P9ClientProbe {
    pub usable: bool,
    pub platform: Option<P9Platform>,
    pub kernel: bool,
    pub transport: bool,
    pub modules: bool,
    pub root: bool,
    pub reason: Option<String>,
}

pub fn p9_platform() -> Option<P9Platform> {
    #[cfg(target_os = "linux")]
    {
        Some(P9Platform::Linux)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Probe the host without attempting to load modules or mount anything.
pub fn p9_client_probe() -> P9ClientProbe {
    let platform = p9_platform();
    let linux = platform.is_some();
    let root = current_uid_is_root();
    let kernel = linux && has_filesystem("9p");
    let transport = linux && Path::new("/sys/module/9pnet_fd").exists();
    let modules = linux && kernel_module_tree_exists();
    let mut missing = Vec::new();
    if platform.is_none() {
        missing.push("9P native mounts are available on Linux only".to_owned());
    } else {
        if !root {
            missing.push("mount(2) requires CAP_SYS_ADMIN, normally root".to_owned());
        }
        if !kernel {
            missing.push(if modules {
                "9p is not listed in /proc/filesystems; load the kernel module with modprobe 9p"
                    .to_owned()
            } else {
                format!(
                    "9p is unavailable and no module tree exists under /lib/modules/{}",
                    kernel_release().unwrap_or_else(|| "<unknown release>".to_owned())
                )
            });
        }
        // An explicitly selected transport may be built in or loadable at
        // mount time. Report this fact without refusing named mounts; only
        // automatic selection should apply the stricter no-fallback policy.
    }
    P9ClientProbe {
        usable: missing.is_empty(),
        platform,
        kernel,
        transport,
        modules,
        root,
        reason: (!missing.is_empty()).then(|| missing.join("; ")),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum P9MountTransport {
    #[default]
    Unix,
    Tcp,
}

#[derive(Clone)]
pub struct P9MountOptions {
    /// Reuse a previously bound server. Its accept loop is started if it is
    /// not already running, and it is closed when this mount tears down.
    pub server: Option<Arc<P9Server>>,
    /// Policy for a server created by this mount. A supplied shared server
    /// keeps its own policy instead.
    pub server_options: P9ServerOptions,
    /// Hooks used only when this mount creates its own in-process server. A
    /// supplied shared server keeps the hooks it was created with.
    pub server_hooks: P9ServerHooks,
    pub transport: P9MountTransport,
    pub host: String,
    pub port: Option<u16>,
    pub socket_path: Option<PathBuf>,
    pub mount_msize: Option<u32>,
    pub access: String,
    pub cache: String,
    pub uname: String,
    pub aname: String,
    pub read_only: bool,
    /// Report the driver's inode numbers where available.
    pub use_driver_ino: bool,
    pub mount_options: Vec<String>,
    /// `Some` bounds each mount/umount command; `None` waits without a timer.
    pub unmount_timeout: Option<Duration>,
}

impl Default for P9MountOptions {
    fn default() -> Self {
        Self {
            server: None,
            server_options: P9ServerOptions::default(),
            server_hooks: P9ServerHooks::default(),
            transport: P9MountTransport::Unix,
            host: "127.0.0.1".to_owned(),
            port: None,
            socket_path: None,
            mount_msize: None,
            access: "client".to_owned(),
            cache: "none".to_owned(),
            uname: "nobody".to_owned(),
            aname: "/".to_owned(),
            read_only: false,
            use_driver_ino: true,
            mount_options: Vec::new(),
            unmount_timeout: Some(Duration::from_secs(10)),
        }
    }
}

impl P9MountOptions {
    /// Build the listener policy for a mount-created server while preserving
    /// the public server controls that are independent of mount(8)'s option
    /// string. Transport identity is supplied by the mount helper itself so
    /// a caller-provided socket path cannot accidentally leave a TCP listener
    /// with a Unix path (or vice versa).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn server_options_for_mount(
        &self,
        transport: P9MountTransport,
        socket: Option<&Path>,
    ) -> P9ServerOptions {
        let mut options = self.server_options.clone();
        options.host = self.host.clone();
        options.port = self.port.unwrap_or(0);
        options.path = match transport {
            P9MountTransport::Unix => socket.map(Path::to_path_buf),
            P9MountTransport::Tcp => None,
        };
        if options.msize.is_none() {
            options.msize = Some(mount_msize(self.mount_msize));
        }
        options.read_only = self.read_only;
        options.use_driver_ino = self.use_driver_ino;
        options
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P9MountTarget {
    pub transport: P9MountTransport,
    pub port: Option<u16>,
}

/// A successful Linux native mount and its adopted kernel connection.
pub struct P9Mount {
    pub mountpoint: PathBuf,
    pub source: String,
    pub transport: P9MountTransport,
    pub server: Arc<P9Server>,
    pub connection: crate::server::P9Connection,
    state: Arc<MountState>,
}

struct MountState {
    stopping: AtomicBool,
    teardown_started: AtomicBool,
    unmount_lock: Mutex<()>,
    done: AtomicBool,
    complete: Notify,
    failure: Mutex<Option<String>>,
    server: Arc<P9Server>,
    socket_dir: Option<PathBuf>,
    serve_task: Mutex<Option<JoinHandle<io::Result<()>>>>,
    timeout: Option<Duration>,
}

impl P9Mount {
    pub fn active(&self) -> bool {
        !self.state.stopping.load(Ordering::Acquire) && !self.connection.is_closed()
    }

    /// Wait until the kernel connection ends and server resources are released.
    /// This does not detach a still-live kernel mount; call [`P9Mount::unmount`]
    /// after a server-close or EOF path when the mount itself must be removed.
    pub async fn wait_closed(&self) {
        self.connection.wait_closed().await;
        self.state.stopping.store(true, Ordering::Release);
        if self
            .state
            .teardown_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let result = self.state.finish_resources().await;
            self.state.record_result(result).await;
        }
        self.wait_completion().await;
    }

    /// Unmount idempotently, including after [`P9Mount::wait_closed`] has
    /// released the server resources. A failed unmount leaves the server alive
    /// so the operation can be retried, matching the upstream lifecycle
    /// contract.
    pub async fn unmount(&self) -> io::Result<()> {
        let _unmount = self.state.unmount_lock.lock().await;
        self.state.stopping.store(true, Ordering::Release);
        let result = self.unmount_once().await;
        if let Err(error) = &result {
            self.state.failure.lock().await.replace(error.to_string());
            // The mount is still live, so keep answering it and allow a later
            // call to retry after the process holding the mount lets go.
            self.state.stopping.store(false, Ordering::Release);
            return Err(io::Error::other(error.to_string()));
        }
        self.state.done.store(true, Ordering::Release);
        self.state.complete.notify_waiters();
        result
    }

    async fn unmount_once(&self) -> io::Result<()> {
        unmount_path(&self.mountpoint, self.state.timeout).await?;
        self.state.finish_resources().await
    }
}

impl MountState {
    async fn finish_resources(&self) -> io::Result<()> {
        let server_result = self.server.close().await;
        if let Some(task) = self.serve_task.lock().await.take() {
            match task.await {
                Ok(Ok(())) | Ok(Err(_)) | Err(_) => {}
            }
        }
        if let Some(directory) = &self.socket_dir {
            match std::fs::remove_dir(directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        server_result
    }

    async fn record_result(&self, result: io::Result<()>) {
        if let Err(error) = result {
            self.failure.lock().await.replace(error.to_string());
        }
        self.done.store(true, Ordering::Release);
        self.complete.notify_waiters();
    }
}

impl P9Mount {
    async fn wait_completion(&self) {
        while !self.state.done.load(Ordering::Acquire) {
            self.state.complete.notified().await;
        }
    }
}

/// Mount a driver through the host Linux v9fs client. On macOS and other
/// non-Linux targets this returns `Unsupported` without running a command.
pub async fn mount_9p<D, P>(
    driver: D,
    mountpoint: P,
    options: P9MountOptions,
) -> io::Result<P9Mount>
where
    D: FsDriver + 'static,
    P: AsRef<Path>,
{
    #[cfg(target_os = "linux")]
    {
        mount_9p_linux(driver, mountpoint.as_ref(), options).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (driver, mountpoint, options);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native 9P mounts are available on Linux only; use the rootless wire server on this host",
        ))
    }
}

/// The option list passed to Linux `mount(8)`, kept pure for portable tests and
/// for callers that need to print an exact command without running it.
pub fn p9_mount_options(target: &P9MountTarget, options: &P9MountOptions) -> io::Result<String> {
    let mut parts = vec![match target.transport {
        P9MountTransport::Unix => "trans=unix".to_owned(),
        P9MountTransport::Tcp => "trans=tcp".to_owned(),
    }];
    if target.transport == P9MountTransport::Tcp {
        let port = target.port.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "TCP 9P mounts require a port")
        })?;
        if port == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TCP 9P mount port must be in [1, 65535]",
            ));
        }
        parts.push(format!("port={port}"));
    }
    parts.push("version=9p2000.L".to_owned());
    parts.push(format!("msize={}", mount_msize(options.mount_msize)));
    for (name, value) in [
        ("access", options.access.as_str()),
        ("cache", options.cache.as_str()),
        ("uname", options.uname.as_str()),
        ("aname", options.aname.as_str()),
    ] {
        check_option_value(name, value)?;
        parts.push(format!("{name}={value}"));
    }
    if options.read_only {
        parts.push("ro".to_owned());
    }
    parts.extend(options.mount_options.iter().cloned());
    Ok(parts.join(","))
}

pub fn socket_path_refusal(path: &Path) -> Option<String> {
    let length = path.to_string_lossy().len();
    (length >= P9_UNIX_PATH_MAX).then(|| {
        format!(
            "9P Unix socket path is {length} bytes; Linux sockaddr_un allows at most {}",
            P9_UNIX_PATH_MAX - 1
        )
    })
}

pub fn tcp_source_refusal(host: &str) -> Option<String> {
    let valid = host.split('.').collect::<Vec<_>>();
    let valid = valid.len() == 4
        && valid
            .iter()
            .all(|octet| !octet.is_empty() && octet.parse::<u8>().is_ok());
    (!valid).then(|| format!("a trans=tcp 9P mount needs a dotted-quad IPv4 source, got {host:?}"))
}

#[cfg(target_os = "linux")]
fn plausible_mount_connection(
    target: &P9MountTarget,
    source: &str,
    connection: &crate::server::P9Connection,
) -> bool {
    if target.transport != P9MountTransport::Tcp || !source.starts_with("127.") {
        return true;
    }
    connection.peer.as_deref().is_some_and(|peer| {
        let peer = peer.strip_prefix("::ffff:").unwrap_or(peer);
        peer.starts_with("127.")
    })
}

#[cfg(target_os = "linux")]
async fn mount_9p_linux<D>(
    driver: D,
    mountpoint: &Path,
    options: P9MountOptions,
) -> io::Result<P9Mount>
where
    D: FsDriver + 'static,
{
    let probe = p9_client_probe();
    if !probe.usable {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("cannot mount 9P here: {}", probe.reason.unwrap_or_default()),
        ));
    }
    let mountpoint = std::fs::canonicalize(mountpoint)?;
    if !std::fs::metadata(&mountpoint)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("mountpoint {} is not a directory", mountpoint.display()),
        ));
    }
    if is_mounted_at(&mountpoint)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already has a filesystem mounted", mountpoint.display()),
        ));
    }

    let (server, source, target, socket_dir) = if let Some(server) = options.server.clone() {
        let transport = if server.unix_path().is_some() {
            P9MountTransport::Unix
        } else {
            P9MountTransport::Tcp
        };
        match transport {
            P9MountTransport::Unix => {
                let socket = server
                    .unix_path()
                    .ok_or_else(|| io::Error::other("Unix 9P server has no socket path"))?
                    .to_path_buf();
                socket_path_refusal(&socket).map_or(Ok(()), |reason| {
                    Err(io::Error::new(io::ErrorKind::InvalidInput, reason))
                })?;
                (
                    server,
                    socket.to_string_lossy().into_owned(),
                    P9MountTarget {
                        transport,
                        port: None,
                    },
                    None,
                )
            }
            P9MountTransport::Tcp => {
                let host = server.options().host.clone();
                if let Some(reason) = tcp_source_refusal(&host) {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
                }
                let port = server.local_addr()?.port();
                (
                    server,
                    host,
                    P9MountTarget {
                        transport,
                        port: Some(port),
                    },
                    None,
                )
            }
        }
    } else {
        match options.transport {
            P9MountTransport::Unix => {
                let (directory, socket) = match options.socket_path.clone() {
                    Some(path) => (None, path),
                    None => {
                        let directory = create_private_socket_directory()?;
                        let socket = directory.join("9p.sock");
                        (Some(directory), socket)
                    }
                };
                if let Some(reason) = socket_path_refusal(&socket) {
                    cleanup_socket_directory(directory.as_deref());
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
                }
                let server_options =
                    options.server_options_for_mount(P9MountTransport::Unix, Some(&socket));
                let server = match P9Server::bind_unix_with_hooks(
                    driver,
                    &socket,
                    server_options,
                    options.server_hooks.clone(),
                )
                .await
                {
                    Ok(server) => server,
                    Err(error) => {
                        cleanup_socket_directory(directory.as_deref());
                        return Err(error);
                    }
                };
                (
                    Arc::new(server),
                    socket.to_string_lossy().into_owned(),
                    P9MountTarget {
                        transport: P9MountTransport::Unix,
                        port: None,
                    },
                    directory,
                )
            }
            P9MountTransport::Tcp => {
                let host = options.host.clone();
                if let Some(reason) = tcp_source_refusal(&host) {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
                }
                let server_options = options.server_options_for_mount(P9MountTransport::Tcp, None);
                let server =
                    P9Server::bind_with_hooks(driver, server_options, options.server_hooks.clone())
                        .await?;
                let port = server.local_addr()?.port();
                (
                    Arc::new(server),
                    host,
                    P9MountTarget {
                        transport: P9MountTransport::Tcp,
                        port: Some(port),
                    },
                    None,
                )
            }
        }
    };

    let _adoption = server.adoption_lock().lock().await;
    let before = server
        .clients()?
        .into_iter()
        .map(|connection| connection.id())
        .collect::<std::collections::HashSet<_>>();
    let mount_options = match p9_mount_options(&target, &options) {
        Ok(options) => options,
        Err(error) => {
            let _ = server.close().await;
            cleanup_socket_directory(socket_dir.as_deref());
            return Err(error);
        }
    };
    let serve_task = match server.start() {
        Ok(task) => Some(task),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
        Err(error) => {
            let _ = server.close().await;
            cleanup_socket_directory(socket_dir.as_deref());
            return Err(error);
        }
    };
    let mut mount_command = Command::new("mount");
    mount_command
        .kill_on_drop(true)
        .args(["-i", "-t", "9p", "-o", &mount_options, "--"])
        .arg(&source)
        .arg(&mountpoint);
    let output = if let Some(limit) = options.unmount_timeout {
        match timeout(limit, mount_command.output()).await {
            Ok(output) => output,
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "mount -t 9p timed out",
            )),
        }
    } else {
        mount_command.output().await
    };
    let output = match output {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            let mount_error = io::Error::other(format!(
                "mount -t 9p failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
            let cleanup = cleanup_failed_mount(
                &mountpoint,
                &server,
                serve_task,
                socket_dir,
                options.unmount_timeout,
            )
            .await;
            return match cleanup {
                Ok(()) => Err(mount_error),
                Err(cleanup_error) => Err(io::Error::other(format!(
                    "{mount_error}; cleanup failed: {cleanup_error}"
                ))),
            };
        }
        Err(error) => {
            let cleanup = cleanup_failed_mount(
                &mountpoint,
                &server,
                serve_task,
                socket_dir,
                options.unmount_timeout,
            )
            .await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(io::Error::other(format!(
                    "{error}; cleanup failed: {cleanup_error}"
                ))),
            };
        }
    };
    let _ = output;

    let connection = match server.clients()?.into_iter().find(|connection| {
        !before.contains(&connection.id())
            && plausible_mount_connection(&target, &source, connection)
    }) {
        Some(connection) => connection,
        None => {
            let cleanup = cleanup_failed_mount(
                &mountpoint,
                &server,
                serve_task,
                socket_dir,
                options.unmount_timeout,
            )
            .await;
            return match cleanup {
                Ok(()) => Err(io::Error::other(
                    "mount succeeded but no 9P connection arrived",
                )),
                Err(cleanup_error) => Err(io::Error::other(format!(
                    "mount succeeded but no 9P connection arrived; cleanup failed: {cleanup_error}"
                ))),
            };
        }
    };
    drop(_adoption);
    let connection_for_monitor = connection.clone();
    let state = Arc::new(MountState {
        stopping: AtomicBool::new(false),
        teardown_started: AtomicBool::new(false),
        unmount_lock: Mutex::new(()),
        done: AtomicBool::new(false),
        complete: Notify::new(),
        failure: Mutex::new(None),
        server: Arc::clone(&server),
        socket_dir,
        serve_task: Mutex::new(serve_task),
        timeout: options.unmount_timeout,
    });
    let monitor_state = Arc::clone(&state);
    tokio::spawn(async move {
        connection_for_monitor.wait_closed().await;
        if monitor_state
            .teardown_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            monitor_state.stopping.store(true, Ordering::Release);
            let result = monitor_state.finish_resources().await;
            monitor_state.record_result(result).await;
        }
    });
    let mount = P9Mount {
        mountpoint,
        source,
        transport: target.transport,
        server,
        connection,
        state,
    };
    Ok(mount)
}

fn mount_msize(value: Option<u32>) -> u32 {
    value
        .unwrap_or(P9_DEFAULT_MOUNT_MSIZE)
        .clamp(P9_MIN_MSIZE, P9_MAX_MOUNT_MSIZE)
}

fn check_option_value(name: &str, value: &str) -> io::Result<()> {
    if value.contains(',') || value.chars().any(char::is_whitespace) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("9P mount option {name} may not contain comma or whitespace"),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn run_command(
    command: &str,
    args: &[&std::ffi::OsStr],
    limit: Option<Duration>,
) -> io::Result<std::process::ExitStatus> {
    let mut child = Command::new(command)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let status = if let Some(limit) = limit {
        match timeout(limit, child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{command} timed out"),
                ));
            }
        }
    } else {
        child.wait().await?
    };
    Ok(status)
}

#[cfg(target_os = "linux")]
async fn unmount_path(path: &Path, limit: Option<Duration>) -> io::Result<()> {
    if !is_mounted_at(path)? {
        return Ok(());
    }
    let first = run_command("umount", &[path.as_os_str()], limit).await;
    let detached = match first {
        Ok(status) if status.success() => !is_mounted_at(path)?,
        Ok(_) | Err(_) => false,
    };
    if detached {
        return Ok(());
    }

    // v9fs implements forced cancellation. Lazy detach is the final
    // kernel-side fallback, but the mount table remains the authority.
    for args in [
        vec![std::ffi::OsStr::new("-f"), path.as_os_str()],
        vec![std::ffi::OsStr::new("-l"), path.as_os_str()],
    ] {
        let _ = run_command("umount", &args, limit).await;
        if !is_mounted_at(path)? {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("could not unmount 9P mount at {}", path.display()),
    ))
}

#[cfg(target_os = "linux")]
async fn cleanup_failed_mount(
    mountpoint: &Path,
    server: &P9Server,
    serve_task: Option<JoinHandle<io::Result<()>>>,
    socket_dir: Option<PathBuf>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    let unmount_result = unmount_path(mountpoint, timeout).await;
    let server_result = server.close().await;
    if let Some(task) = serve_task {
        let _ = task.await;
    }
    let still_mounted = is_mounted_at(mountpoint).unwrap_or(true);
    if unmount_result.is_ok() && server_result.is_ok() && !still_mounted {
        cleanup_socket_directory(socket_dir.as_deref());
    }
    unmount_result?;
    if still_mounted {
        return Err(io::Error::other(
            "9P mount remains live; refusing recursive cleanup",
        ));
    }
    server_result
}

#[cfg(not(target_os = "linux"))]
async fn unmount_path(_path: &Path, _limit: Option<Duration>) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native 9P mounts are available on Linux only",
    ))
}

#[cfg(target_os = "linux")]
fn has_filesystem(name: &str) -> bool {
    std::fs::read_to_string("/proc/filesystems")
        .map(|table| {
            table
                .lines()
                .any(|line| line.split_whitespace().last() == Some(name))
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn has_filesystem(_name: &str) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn kernel_release() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|release| release.trim().to_owned())
        .filter(|release| !release.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn kernel_release() -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn kernel_module_tree_exists() -> bool {
    kernel_release()
        .map(|release| Path::new("/lib/modules").join(release).is_dir())
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn kernel_module_tree_exists() -> bool {
    false
}

fn current_uid_is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        unsafe { geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
fn create_private_socket_directory() -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir();
    let pid = std::process::id();
    for attempt in 0..100_u32 {
        let path = base.join(format!("mount-rs-9p-{pid}-{attempt}"));
        match std::fs::create_dir(&path) {
            Ok(()) => {
                std::fs::set_permissions(&path, PermissionsExt::from_mode(0o700))?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a private 9P socket directory",
    ))
}

#[cfg(target_os = "linux")]
fn cleanup_socket_directory(directory: Option<&Path>) {
    if let Some(directory) = directory {
        let _ = std::fs::remove_dir(directory);
    }
}

#[cfg(target_os = "linux")]
fn is_mounted_at(path: &Path) -> io::Result<bool> {
    let table = std::fs::read_to_string("/proc/self/mounts")?;
    let target = path.to_string_lossy();
    Ok(parse_mount_table(&table)
        .iter()
        .any(|entry| entry.target == target))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub source: String,
    pub target: String,
    pub fs_type: String,
}

pub fn parse_mount_table(table: &str) -> Vec<MountEntry> {
    table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let source = fields.next()?;
            let target = fields.next()?;
            let fs_type = fields.next()?;
            Some(MountEntry {
                source: unescape_mount_path(source),
                target: unescape_mount_path(target),
                fs_type: fs_type.to_owned(),
            })
        })
        .collect()
}

fn unescape_mount_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            let code = [chars.next(), chars.next(), chars.next()];
            if let [Some(a), Some(b), Some(c)] = code {
                let octal = [a, b, c].iter().collect::<String>();
                if let Ok(value) = u8::from_str_radix(&octal, 8) {
                    result.push(char::from(value));
                    continue;
                }
                result.push('\\');
                result.push(a);
                result.push(b);
                result.push(c);
                continue;
            }
            result.push('\\');
        } else {
            result.push(character);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_options_encode_unix_defaults_and_read_only() {
        let options = P9MountOptions {
            read_only: true,
            ..P9MountOptions::default()
        };
        assert_eq!(
            p9_mount_options(
                &P9MountTarget {
                    transport: P9MountTransport::Unix,
                    port: None,
                },
                &options,
            )
            .expect("unix mount options"),
            "trans=unix,version=9p2000.L,msize=131096,access=client,cache=none,uname=nobody,aname=/,ro"
        );
    }

    #[test]
    fn mount_options_validate_tcp_port_and_option_values() {
        let options = P9MountOptions::default();
        assert_eq!(
            p9_mount_options(
                &P9MountTarget {
                    transport: P9MountTransport::Tcp,
                    port: Some(4567),
                },
                &options,
            )
            .expect("tcp mount options"),
            "trans=tcp,port=4567,version=9p2000.L,msize=131096,access=client,cache=none,uname=nobody,aname=/"
        );
        for port in [None, Some(0)] {
            let error = p9_mount_options(
                &P9MountTarget {
                    transport: P9MountTransport::Tcp,
                    port,
                },
                &options,
            )
            .expect_err("invalid TCP port must be refused");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
        let mut invalid = options.clone();
        invalid.access = "client,ro".to_owned();
        assert!(
            p9_mount_options(
                &P9MountTarget {
                    transport: P9MountTransport::Unix,
                    port: None,
                },
                &invalid,
            )
            .is_err()
        );
    }

    #[test]
    fn mount_server_options_preserve_policy_and_overlay_transport_identity() {
        let socket = PathBuf::from("/tmp/mount-rs-policy/9p.sock");
        let mut options = P9MountOptions {
            host: "10.0.0.7".to_owned(),
            port: Some(4567),
            read_only: true,
            use_driver_ino: false,
            ..P9MountOptions::default()
        };
        options.server_options.allow_remote = true;
        options.server_options.socket_mode = 0o640;
        options.server_options.allow_shared_directory = true;
        options.server_options.max_frame = 2 * 1024 * 1024;
        options.server_options.max_in_flight = 7;
        options.server_options.msize = Some(64 * 1024);
        options.server_options.claim_ownership = false;
        options.server_options.debug = false;
        options.server_options.locks = Some(crate::locks::P9LockTable::new(Default::default()));

        let tcp = options.server_options_for_mount(P9MountTransport::Tcp, None);
        assert_eq!(tcp.host, "10.0.0.7");
        assert_eq!(tcp.port, 4567);
        assert_eq!(tcp.path, None);
        assert!(tcp.allow_remote);
        assert_eq!(tcp.socket_mode, 0o640);
        assert!(tcp.allow_shared_directory);
        assert_eq!(tcp.max_frame, 2 * 1024 * 1024);
        assert_eq!(tcp.max_in_flight, 7);
        assert_eq!(tcp.msize, Some(64 * 1024));
        assert!(!tcp.use_driver_ino);
        assert!(tcp.read_only);
        assert!(!tcp.claim_ownership);
        assert!(!tcp.debug);
        assert!(tcp.locks.is_some());

        let unix = options.server_options_for_mount(P9MountTransport::Unix, Some(&socket));
        assert_eq!(unix.path, Some(socket));
        assert_eq!(unix.port, 4567);
        assert_eq!(unix.host, "10.0.0.7");
    }

    #[test]
    fn refusal_helpers_use_kernel_boundaries() {
        assert!(socket_path_refusal(Path::new(&"a".repeat(P9_UNIX_PATH_MAX - 1))).is_none());
        assert!(socket_path_refusal(Path::new(&"a".repeat(P9_UNIX_PATH_MAX))).is_some());
        assert!(tcp_source_refusal("127.0.0.1").is_none());
        assert!(tcp_source_refusal("127.0.0.256").is_some());
        assert!(tcp_source_refusal("localhost").is_some());
    }

    #[test]
    fn mount_table_unescapes_kernel_path_fields() {
        let entries = parse_mount_table("/dev/foo /tmp/a\\040b 9p rw 0 0\n");
        assert_eq!(
            entries,
            vec![MountEntry {
                source: "/dev/foo".to_owned(),
                target: "/tmp/a b".to_owned(),
                fs_type: "9p".to_owned(),
            }]
        );
    }
}
