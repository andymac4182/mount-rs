//! Opt-in native WebDAV client coverage.
//!
//! The pinned mountx oracle has a Linux `mount.davfs` harness. Its WebDAV
//! documentation also identifies Apple's `mount_webdav` as the macOS client,
//! so the probe accepts that platform's native helper without pretending the
//! oracle has a macOS mount suite. This file is ignored and environment-gated;
//! ordinary tests never ask the host kernel to mount anything. The explicit
//! probe covers one round trip, eight concurrent native-client I/O pairs, and
//! a server-close/remount restart cycle.

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
const NATIVE_CONCURRENCY: usize = 8;

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

struct DavfsConfiguration(Option<PathBuf>);

impl DavfsConfiguration {
    fn create(client: &NativeClient, mountpoint: &Path) -> Self {
        if !matches!(client.kind, NativeClientKind::LinuxDavfs) {
            return Self(None);
        }
        // Match the upstream harness without changing /etc/davfs2 or the
        // developer's configuration. Anonymous tests must not prompt on stdin.
        use std::io::Write;
        let path = mountpoint.with_extension("davfs.conf");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("create isolated davfs configuration");
        file.write_all(b"ask_auth 0\ndelay_upload 0\ndir_refresh 1\nfile_refresh 1\n")
            .expect("write isolated davfs configuration");
        Self(Some(path))
    }
}

impl Drop for DavfsConfiguration {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn mount_args(
    client: &NativeClient,
    url: &str,
    mountpoint: &Path,
    config: &DavfsConfiguration,
) -> Vec<OsString> {
    match client.kind {
        NativeClientKind::LinuxDavfs => {
            let uid = command_text("id", "-u").expect("Linux probe validated id -u");
            let gid = command_text("id", "-g").expect("Linux probe validated id -g");
            vec![
                OsString::from(url),
                mountpoint.as_os_str().to_owned(),
                OsString::from("-o"),
                OsString::from(format!(
                    "conf={},rw,uid={uid},gid={gid}",
                    config
                        .0
                        .as_ref()
                        .expect("Linux davfs configuration")
                        .display()
                )),
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

async fn native_concurrent_round_trips(mountpoint: &Path, driver: &Loopback) -> Result<(), String> {
    let mut tasks = Vec::with_capacity(NATIVE_CONCURRENCY);
    for index in 0..NATIVE_CONCURRENCY {
        let path = mountpoint.join(format!("parallel-{index}.txt"));
        let expected = format!("native concurrent payload {index}\n").repeat(1024);
        tasks.push(tokio::task::spawn_blocking(move || {
            fs::write(&path, expected.as_bytes())
                .map_err(|error| format!("native concurrent PUT failed: {error}"))?;
            let actual = fs::read(&path)
                .map_err(|error| format!("native concurrent GET failed: {error}"))?;
            if actual.as_slice() != expected.as_bytes() {
                return Err(format!(
                    "native concurrent GET returned unexpected bytes for {}",
                    path.display()
                ));
            }
            Ok::<_, String>((path, expected))
        }));
    }

    let completed = timeout(IO_TIMEOUT, async {
        let mut completed = Vec::with_capacity(tasks.len());
        for task in tasks {
            completed.push(
                task.await
                    .map_err(|error| format!("native concurrent I/O task failed: {error}"))??,
            );
        }
        Ok::<_, String>(completed)
    })
    .await
    .map_err(|_| "native concurrent I/O timed out".to_owned())??;

    timeout(IO_TIMEOUT, async {
        let deadline = tokio::time::Instant::now() + IO_TIMEOUT;
        for (path, expected) in completed {
            let name = path
                .file_name()
                .ok_or_else(|| {
                    format!(
                        "native concurrent path has no file name: {}",
                        path.display()
                    )
                })?
                .to_string_lossy();
            let driver_path = format!("/{name}");
            let mut observed = None;
            while tokio::time::Instant::now() < deadline {
                if let Ok(actual) = driver.read_file(&driver_path).await {
                    observed = Some(actual.clone());
                    if actual == expected.as_bytes() {
                        break;
                    }
                }
                sleep(Duration::from_millis(50)).await;
            }
            if observed.as_deref() != Some(expected.as_bytes()) {
                return Err(format!(
                    "native concurrent PUT did not reach the driver for {driver_path}"
                ));
            }
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|_| "native concurrent driver readback timed out".to_owned())??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit native WebDAV prerequisites and host mount privileges"]
async fn native_webdav_mount_probe_round_trip_concurrency_and_restart() {
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
    // macOS's temporary directory may use /var while mount(8) reports its
    // canonical /private/var path. Compare and unmount the same identity.
    let mountpoint = fs::canonicalize(mountpoint).expect("canonicalize empty mountpoint");
    let config = DavfsConfiguration::create(&client, &mountpoint);

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
    let mounted_result = run_command(
        &client.mount,
        &mount_args(&client, &url, &mountpoint, &config),
    )
    .await;
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
    let concurrent = if round_trip.is_ok() {
        native_concurrent_round_trips(&mountpoint, &loopback).await
    } else {
        Err("skipped native concurrency after the basic round trip failed".to_owned())
    };
    // Exercise the adverse ordering explicitly: close the HTTP server while
    // the native client still owns its mount, then unmount, relisten, remount,
    // and verify that the same driver contents remain available.
    let close_while_mounted = server.close().await;
    let unmount_after_close = guard.unmount().await;
    let close_retry = if close_while_mounted.is_err() {
        Some(server.close().await)
    } else {
        None
    };
    let close_after_teardown =
        close_while_mounted.is_ok() || close_retry.as_ref().is_some_and(|result| result.is_ok());
    let restart_listen = if close_after_teardown && unmount_after_close.is_ok() {
        Some(server.listen().await)
    } else {
        None
    };
    let restart_mount = if restart_listen.as_ref().is_some_and(|result| result.is_ok()) {
        let result = run_command(
            &guard.client.mount,
            &mount_args(
                &guard.client,
                &format!("{}/", server.url()),
                &mountpoint,
                &config,
            ),
        )
        .await;
        let active = wait_until_mounted(&guard.client, &mountpoint).await;
        if active {
            guard.active = true;
        }
        Some(match result {
            Ok(result) if result.status.success() && active => Ok(()),
            Ok(result) => Err(format!(
                "native WebDAV restart mount did not become active ({}): {}",
                result.status, result.output
            )),
            Err(error) => Err(format!("native WebDAV restart mount failed: {error}")),
        })
    } else {
        None
    };
    let restart_round_trip = if restart_mount.as_ref().is_some_and(|result| result.is_ok()) {
        Some(native_round_trip(&mountpoint, &loopback).await)
    } else {
        None
    };
    let unmount_after_restart = guard.unmount().await;
    let final_close = server.close().await;
    let remove = fs::remove_dir(&mountpoint);
    assert!(
        round_trip.is_ok(),
        "native WebDAV I/O failed: {round_trip:?}"
    );
    assert!(
        concurrent.is_ok(),
        "native WebDAV concurrent I/O failed: {concurrent:?}"
    );
    assert!(
        close_while_mounted.is_ok(),
        "WebDAV server close while mounted failed: {close_while_mounted:?}"
    );
    assert!(
        unmount_after_close.is_ok(),
        "native WebDAV unmount after server close failed: {unmount_after_close:?}"
    );
    assert!(
        close_after_teardown,
        "WebDAV server did not close after mounted teardown: initial={close_while_mounted:?}, retry={close_retry:?}"
    );
    assert!(
        restart_listen.as_ref().is_some_and(|result| result.is_ok()),
        "WebDAV server relisten after mounted teardown failed: {restart_listen:?}"
    );
    assert!(
        restart_mount.as_ref().is_some_and(|result| result.is_ok()),
        "native WebDAV remount after server restart failed: {restart_mount:?}"
    );
    assert!(
        restart_round_trip
            .as_ref()
            .is_some_and(|result| result.is_ok()),
        "native WebDAV post-restart I/O failed: {restart_round_trip:?}"
    );
    assert!(
        unmount_after_restart.is_ok(),
        "native WebDAV post-restart unmount failed: {unmount_after_restart:?}"
    );
    assert!(
        final_close.is_ok(),
        "WebDAV final server close failed: {final_close:?}"
    );
    assert!(remove.is_ok(), "mountpoint cleanup failed: {remove:?}");
}
