//! Opt-in native WebDAV client coverage.
//!
//! The pinned mountx oracle has a Linux `mount.davfs` harness. Its WebDAV
//! documentation also identifies Apple's `mount_webdav` as the macOS client,
//! so the probe accepts that platform's native helper without pretending the
//! oracle has a macOS mount suite. This file is ignored and environment-gated;
//! ordinary tests never ask the host kernel to mount anything.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as BlockingCommand, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mount_rs_core::{FsDriver, Loopback, MemoryFs};
use mount_rs_webdav::{WebdavServerOptions, create_webdav_server};
use tokio::process::Command;
use tokio::time::{sleep, timeout};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MOUNT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
enum NativeClientKind {
    LinuxDavfs,
    MacOsMountWebdav,
}

#[derive(Debug, Clone)]
struct NativeClient {
    kind: NativeClientKind,
    mount: PathBuf,
    umount: PathBuf,
    mount_table: Option<PathBuf>,
}

#[derive(Debug)]
struct CommandResult {
    status: ExitStatus,
    output: String,
}

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn find_executable(name: &str, fallbacks: &[&str]) -> Option<PathBuf> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = directory.join(name);
        if executable(&candidate) {
            return Some(candidate);
        }
    }
    fallbacks
        .iter()
        .map(PathBuf::from)
        .find(|candidate| executable(candidate))
}

fn command_text(program: &str, argument: &str) -> Result<String, String> {
    let output = BlockingCommand::new(program)
        .arg(argument)
        .output()
        .map_err(|error| format!("could not run {program} {argument}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {argument} exited with {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn native_webdav_client_probe() -> Result<NativeClient, String> {
    let umount = find_executable(
        "umount",
        &["/sbin/umount", "/bin/umount", "/usr/sbin/umount"],
    )
    .ok_or_else(|| "no umount executable was found".to_owned())?;

    if cfg!(target_os = "linux") {
        let mount = find_executable(
            "mount.davfs",
            &["/sbin/mount.davfs", "/usr/sbin/mount.davfs"],
        )
        .ok_or_else(|| "no mount.davfs executable was found; install davfs2".to_owned())?;
        let filesystems = fs::read_to_string("/proc/filesystems")
            .map_err(|error| format!("cannot read /proc/filesystems: {error}"))?;
        let fuse = filesystems
            .lines()
            .any(|line| line.split_whitespace().eq(["nodev", "fuse"]));
        if !fuse || !Path::new("/dev/fuse").exists() {
            return Err("davfs2 requires the kernel fuse filesystem and /dev/fuse".to_owned());
        }
        if command_text("id", "-u")?.trim() != "0" {
            return Err("the oracle's davfs2 mount harness requires root".to_owned());
        }
        return Ok(NativeClient {
            kind: NativeClientKind::LinuxDavfs,
            mount,
            umount,
            mount_table: None,
        });
    }

    if cfg!(target_os = "macos") {
        let mount = find_executable("mount_webdav", &["/sbin/mount_webdav"])
            .ok_or_else(|| "no mount_webdav executable was found".to_owned())?;
        let mount_table = find_executable("mount", &["/sbin/mount", "/bin/mount"])
            .ok_or_else(|| "no mount executable was found for native verification".to_owned())?;
        return Ok(NativeClient {
            kind: NativeClientKind::MacOsMountWebdav,
            mount,
            umount,
            mount_table: Some(mount_table),
        });
    }

    Err("the WebDAV native harness supports only macOS and Linux".to_owned())
}

fn mount_args(client: &NativeClient, url: &str, mountpoint: &Path) -> Vec<OsString> {
    match client.kind {
        NativeClientKind::LinuxDavfs => {
            let uid = command_text("id", "-u").expect("Linux probe validated id -u");
            let gid = command_text("id", "-g").expect("Linux probe validated id -g");
            vec![
                OsString::from(url),
                mountpoint.as_os_str().to_owned(),
                OsString::from("-o"),
                OsString::from(format!("rw,uid={uid},gid={gid}")),
            ]
        }
        NativeClientKind::MacOsMountWebdav => vec![
            OsString::from("-S"),
            OsString::from(url),
            mountpoint.as_os_str().to_owned(),
        ],
    }
}

fn unmount_args(client: &NativeClient, mountpoint: &Path) -> Vec<OsString> {
    match client.kind {
        NativeClientKind::LinuxDavfs => {
            vec![OsString::from("-i"), mountpoint.as_os_str().to_owned()]
        }
        NativeClientKind::MacOsMountWebdav => vec![mountpoint.as_os_str().to_owned()],
    }
}

async fn run_command(program: &Path, arguments: &[OsString]) -> Result<CommandResult, String> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|error| format!("could not run {}: {error}", program.display()))?;
    let output = timeout(COMMAND_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("{} timed out", program.display()))?
        .map_err(|error| format!("{} failed: {error}", program.display()))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(CommandResult {
        status: output.status,
        output: text,
    })
}

fn mounted(client: &NativeClient, mountpoint: &Path) -> bool {
    match client.kind {
        NativeClientKind::LinuxDavfs => {
            let escaped = mountpoint.to_string_lossy().replace(' ', "\\040");
            fs::read_to_string("/proc/self/mounts")
                .map(|table| {
                    table
                        .lines()
                        .any(|line| line.split_whitespace().nth(1) == Some(escaped.as_str()))
                })
                .unwrap_or(false)
        }
        NativeClientKind::MacOsMountWebdav => BlockingCommand::new(
            client
                .mount_table
                .as_ref()
                .expect("macOS probe validated mount executable"),
        )
        .output()
        .map(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            let marker = format!(" on {} ", mountpoint.display());
            text.lines().any(|line| line.contains(&marker))
        })
        .unwrap_or(false),
    }
}

async fn wait_until_mounted(client: &NativeClient, mountpoint: &Path) -> bool {
    let deadline = tokio::time::Instant::now() + MOUNT_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if mounted(client, mountpoint) {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    mounted(client, mountpoint)
}

struct NativeMountGuard {
    client: NativeClient,
    mountpoint: PathBuf,
    active: bool,
}

impl NativeMountGuard {
    async fn unmount(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        let result = run_command(
            &self.client.umount,
            &unmount_args(&self.client, &self.mountpoint),
        )
        .await?;
        if !result.status.success() || mounted(&self.client, &self.mountpoint) {
            return Err(format!(
                "native WebDAV unmount failed ({}): {}",
                result.status, result.output
            ));
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for NativeMountGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let program = self.client.umount.clone();
        let arguments = unmount_args(&self.client, &self.mountpoint);
        let _ = std::thread::Builder::new()
            .name("mount-rs-webdav-native-cleanup".to_owned())
            .spawn(move || {
                let _ = BlockingCommand::new(program).args(arguments).status();
            });
    }
}

async fn native_round_trip(mountpoint: &Path, driver: &Loopback) -> Result<(), String> {
    let seed_path = mountpoint.join("seed.txt");
    let seed = timeout(
        IO_TIMEOUT,
        tokio::task::spawn_blocking(move || fs::read(seed_path)),
    )
    .await
    .map_err(|_| "native GET timed out".to_owned())?
    .map_err(|error| format!("native GET task failed: {error}"))?
    .map_err(|error| format!("native GET failed: {error}"))?;
    if seed != b"seeded over the driver" {
        return Err(format!("native GET returned unexpected bytes: {seed:?}"));
    }

    let put_path = mountpoint.join("put.txt");
    let expected = b"written through native WebDAV".to_vec();
    let write_path = put_path.clone();
    let write_bytes = expected.clone();
    timeout(
        IO_TIMEOUT,
        tokio::task::spawn_blocking(move || fs::write(write_path, write_bytes)),
    )
    .await
    .map_err(|_| "native PUT timed out".to_owned())?
    .map_err(|error| format!("native PUT task failed: {error}"))?
    .map_err(|error| format!("native PUT failed: {error}"))?;

    let deadline = tokio::time::Instant::now() + IO_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Ok(actual) = driver.read_file("/put.txt").await
            && actual == expected
        {
            return Ok(());
        }
        sleep(Duration::from_millis(50)).await;
    }
    Err("native PUT did not reach the driver before the deadline".to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit native WebDAV prerequisites and host mount privileges"]
async fn native_webdav_mount_probe_and_round_trip() {
    assert_eq!(
        std::env::var("MOUNT_RS_WEBDAV_NATIVE_TEST").ok().as_deref(),
        Some("1"),
        "set MOUNT_RS_WEBDAV_NATIVE_TEST=1 when explicitly running this ignored harness"
    );
    let client = native_webdav_client_probe()
        .unwrap_or_else(|reason| panic!("native WebDAV prerequisites are missing: {reason}"));

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let mountpoint = std::env::temp_dir().join(format!(
        "mount-rs-webdav-native-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&mountpoint).expect("create empty mountpoint");

    let driver: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let loopback = Loopback::from_arc(Arc::clone(&driver));
    loopback
        .write_file("/seed.txt", b"seeded over the driver")
        .await
        .expect("seed driver file");
    let server =
        create_webdav_server(driver, WebdavServerOptions::default()).expect("loopback WebDAV bind");
    server.listen().await.expect("listen WebDAV server");

    let url = format!("{}/", server.url());
    let mounted_result = run_command(&client.mount, &mount_args(&client, &url, &mountpoint)).await;
    let active = wait_until_mounted(&client, &mountpoint).await;
    let mut guard = NativeMountGuard {
        client,
        mountpoint: mountpoint.clone(),
        active,
    };

    if let Err(error) = &mounted_result {
        let _ = guard.unmount().await;
        let _ = server.close().await;
        let _ = fs::remove_dir(&mountpoint);
        panic!("native WebDAV mount command failed: {error}");
    }
    let mount_result = mounted_result.expect("mount result exists");
    if !mount_result.status.success() || !active {
        let _ = guard.unmount().await;
        let _ = server.close().await;
        let _ = fs::remove_dir(&mountpoint);
        panic!(
            "native WebDAV mount did not become active ({}): {}",
            mount_result.status, mount_result.output
        );
    }

    let round_trip = native_round_trip(&mountpoint, &loopback).await;
    let unmount = guard.unmount().await;
    let close = server.close().await;
    let remove = fs::remove_dir(&mountpoint);
    assert!(
        round_trip.is_ok(),
        "native WebDAV I/O failed: {round_trip:?}"
    );
    assert!(unmount.is_ok(), "native WebDAV unmount failed: {unmount:?}");
    assert!(close.is_ok(), "WebDAV server close failed: {close:?}");
    assert!(remove.is_ok(), "mountpoint cleanup failed: {remove:?}");
}
