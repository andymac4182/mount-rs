//! Opt-in native lifecycle acceptance.
//!
//! The ordinary CLI suite is deliberately mount-free. These ignored tests are
//! a separate set of opt-in checks for real native kernel mounts and Ctrl-C
//! cleanup.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
#[cfg(target_os = "macos")]
use std::io::Read;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires an opt-in Linux FUSE setup; see the test command in the CLI README"]
fn cli_fuse_subprocess_mounts_and_unmounts_on_sigint() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_FUSE");

    let mountpoint = unique_mountpoint();
    fs::create_dir(&mountpoint).expect("create disposable native mountpoint");
    let child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args([
            "mount",
            "--transport",
            "fuse",
            "--empty",
            "--quiet",
            mountpoint.to_str().expect("UTF-8 temporary mountpoint"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mount-rs FUSE subprocess");
    let mut child_guard = NativeChildGuard::new(child, mountpoint.clone(), "fuse");

    let (line_sender, line_receiver) = mpsc::channel::<String>();
    let stdout = child_guard
        .child_mut()
        .stdout
        .take()
        .expect("capture CLI stdout");
    let stdout_thread = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = line_sender.send(line);
        }
    });
    let stderr = child_guard
        .child_mut()
        .stderr
        .take()
        .expect("capture CLI stderr");
    let stderr_thread = thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });

    let mut output = Vec::new();
    let ready = wait_for_mount(child_guard.child_mut(), &line_receiver, &mut output, "fuse");
    assert!(ready, "FUSE subprocess did not mount; output: {output:?}");
    assert!(
        is_mounted_at(&mountpoint),
        "kernel did not report the mountpoint"
    );

    // This is the lifecycle path under test: SIGINT is handled by the CLI,
    // which calls the transport's real unmount operation before exiting.
    // SAFETY: the child is the live subprocess just spawned above, and no
    // other process identifier is used.
    let signal_result = unsafe { libc::kill(child_guard.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(signal_result, 0, "send SIGINT to mount-rs");
    let status = wait_for_exit(child_guard.child_mut());
    assert!(status.success(), "CLI did not exit cleanly: {status}");

    stdout_thread.join().expect("join CLI stdout reader");
    let stderr_lines = stderr_thread.join().expect("join CLI stderr reader");
    output.extend(line_receiver.try_iter());
    assert!(
        output.iter().any(|line| line.contains("unmounted")),
        "CLI did not report unmount: {output:?}"
    );
    assert!(
        !is_mounted_at(&mountpoint),
        "mountpoint remained mounted after CLI exit; stdout={output:?}, stderr={stderr_lines:?}"
    );
    child_guard.disarm();
    fs::remove_dir(&mountpoint).expect("remove disposable native mountpoint");
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires an opt-in Linux FUSE setup; see the test command in the CLI README"]
fn cli_fuse_config_file_binary_mounts_and_round_trips_io() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_FUSE");

    let mountpoint = unique_mountpoint();
    let config_path = mountpoint.with_extension("json");
    fs::create_dir(&mountpoint).expect("create disposable native mountpoint");
    fs::write(
        &config_path,
        format!(
            r#"{{
  "version": 1,
  "mountpoint": "{}",
  "transport": "fuse",
  "empty": true,
  "driver": {{
    "kind": "memory"
  }}
}}"#,
            mountpoint.display()
        ),
    )
    .expect("write native config file");

    let child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["mount", "--config"])
        .arg(&config_path)
        .args(["--quiet"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn config-backed mount-rs FUSE subprocess");
    let mut child_guard = NativeChildGuard::new(child, mountpoint.clone(), "fuse");

    let (line_sender, line_receiver) = mpsc::channel::<String>();
    let stdout = child_guard
        .child_mut()
        .stdout
        .take()
        .expect("capture CLI stdout");
    let stdout_thread = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = line_sender.send(line);
        }
    });
    let stderr = child_guard
        .child_mut()
        .stderr
        .take()
        .expect("capture CLI stderr");
    let stderr_thread = thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });

    let mut output = Vec::new();
    let ready = wait_for_mount(child_guard.child_mut(), &line_receiver, &mut output, "fuse");
    assert!(
        ready,
        "config-backed subprocess did not mount; output: {output:?}"
    );
    assert!(
        is_mounted_at(&mountpoint),
        "kernel did not report config mount"
    );

    let io_result = (|| -> std::io::Result<()> {
        let path = mountpoint.join("config-round-trip");
        fs::write(&path, b"config-native")?;
        let bytes = fs::read(&path)?;
        if bytes != b"config-native" {
            return Err(std::io::Error::other("config-backed native read mismatch"));
        }
        fs::remove_file(path)
    })();

    // The config was consumed by the actual binary; now exercise the same
    // native SIGINT/unmount path as the baseline lifecycle test.
    // SAFETY: the child is the live subprocess just spawned above.
    let signal_result = unsafe { libc::kill(child_guard.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(signal_result, 0, "send SIGINT to config-backed mount-rs");
    let status = wait_for_exit(child_guard.child_mut());
    assert!(
        status.success(),
        "config-backed CLI did not exit cleanly: {status}"
    );

    stdout_thread.join().expect("join CLI stdout reader");
    let stderr_lines = stderr_thread.join().expect("join CLI stderr reader");
    output.extend(line_receiver.try_iter());
    assert!(io_result.is_ok(), "native config I/O failed: {io_result:?}");
    assert!(
        output.iter().any(|line| line.contains("unmounted")),
        "config-backed CLI did not report unmount: {output:?}"
    );
    assert!(
        !is_mounted_at(&mountpoint),
        "config-backed mount remained mounted; stdout={output:?}, stderr={stderr_lines:?}"
    );
    child_guard.disarm();
    fs::remove_dir(&mountpoint).expect("remove disposable native mountpoint");
    fs::remove_file(&config_path).expect("remove disposable native config");
}

#[test]
#[cfg(target_os = "macos")]
#[ignore = "requires opt-in macOS NFS access; see the test command in the CLI README"]
fn cli_nfs_config_binary_persists_bytes_and_cleans_up_on_sigint() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");

    let mountpoint = unique_mountpoint();
    let stem = mountpoint
        .file_name()
        .expect("unique mountpoint has a file name")
        .to_string_lossy();
    let config_path = std::env::temp_dir().join(format!("{stem}-config"));
    let backing_path = std::env::temp_dir().join(format!("{stem}-backing"));
    fs::create_dir(&mountpoint).expect("create disposable macOS NFS mountpoint");
    fs::create_dir(&backing_path).expect("create disposable host-driver backing directory");
    fs::write(
        &config_path,
        format!(
            r#"{{
  "version": 1,
  "mountpoint": "{}",
  "transport": "nfs",
  "driver": {{
    "kind": "host",
    "root": "{}"
  }}
}}"#,
            mountpoint.display(),
            backing_path.display()
        ),
    )
    .expect("write extension-free macOS NFS config");

    let payload = b"macos-nfs-config";
    run_configured_mount_cycle(&config_path, &mountpoint, |target| {
        let path = target.join("config-persistent-bytes");
        fs::write(&path, payload)?;
        let first_read = fs::read(&path)?;
        if first_read != payload {
            return Err(std::io::Error::other("initial NFS byte read mismatch"));
        }
        let mut reopened = std::fs::File::open(&path)?;
        let mut second_read = Vec::new();
        reopened.read_to_end(&mut second_read)?;
        if second_read != payload {
            return Err(std::io::Error::other("reopened NFS byte read mismatch"));
        }
        Ok(())
    });

    // Reuse the same config and host-driver backing directory in a fresh CLI
    // process. This verifies persistence across the first SIGINT/unmount, not
    // just a second file descriptor in one mounted process.
    run_configured_mount_cycle(&config_path, &mountpoint, |target| {
        let path = target.join("config-persistent-bytes");
        let mut reopened = std::fs::File::open(&path)?;
        let mut bytes = Vec::new();
        reopened.read_to_end(&mut bytes)?;
        if bytes != payload {
            return Err(std::io::Error::other("reopened process lost NFS bytes"));
        }
        fs::remove_file(path)
    });

    fs::remove_dir(&mountpoint).expect("remove disposable macOS NFS mountpoint");
    fs::remove_file(&config_path).expect("remove disposable macOS NFS config");
    fs::remove_dir(&backing_path).expect("remove disposable host-driver backing directory");
}

#[test]
#[cfg(target_os = "macos")]
#[ignore = "requires opt-in macOS NFS access and Python sqlite3"]
fn cli_nfs_sqlite_config_binary_hosts_sqlite_and_reopens() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");

    let mountpoint = unique_mountpoint();
    let stem = mountpoint
        .file_name()
        .expect("unique mountpoint has a file name")
        .to_string_lossy();
    let config_path = std::env::temp_dir().join(format!("{stem}-sqlite-config"));
    let database_path = std::env::temp_dir().join(format!("{stem}-sqlite-backend.db"));
    let (uid, gid) = effective_test_identity();
    fs::create_dir(&mountpoint).expect("create disposable SQLite NFS mountpoint");
    fs::write(
        &config_path,
        format!(
            r#"{{
  "version": 1,
  "mountpoint": "{}",
  "transport": "nfs",
  "sqlite_single_host": true,
  "driver": {{
    "kind": "sqlite",
    "database": "{}",
    "uid": {},
    "gid": {}
  }}
}}"#,
            mountpoint.display(),
            database_path.display(),
            uid,
            gid
        ),
    )
    .expect("write SQLite NFS config");

    run_configured_mount_cycle(&config_path, &mountpoint, |target| {
        run_sqlite_delete_fixture(target)
    });
    run_configured_mount_cycle(&config_path, &mountpoint, verify_sqlite_reopen);

    fs::remove_dir(&mountpoint).expect("remove disposable SQLite NFS mountpoint");
    fs::remove_file(&config_path).expect("remove disposable SQLite NFS config");
    fs::remove_file(&database_path).expect("remove disposable SQLite backend");
}

#[cfg(target_os = "macos")]
fn run_configured_mount_cycle<F>(config_path: &std::path::Path, mountpoint: &std::path::Path, io: F)
where
    F: FnOnce(&std::path::Path) -> std::io::Result<()>,
{
    let child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["mount", "--config"])
        .arg(config_path)
        .args(["--quiet"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn config-backed mount-rs NFS subprocess");
    let mut child_guard = NativeChildGuard::new(child, mountpoint.to_path_buf(), "nfs");

    let (line_sender, line_receiver) = mpsc::channel::<String>();
    let stdout = child_guard
        .child_mut()
        .stdout
        .take()
        .expect("capture CLI stdout");
    let stdout_thread = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = line_sender.send(line);
        }
    });
    let stderr = child_guard
        .child_mut()
        .stderr
        .take()
        .expect("capture CLI stderr");
    let stderr_thread = thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });

    let mut output = Vec::new();
    let ready = wait_for_mount(child_guard.child_mut(), &line_receiver, &mut output, "nfs");
    assert!(
        ready,
        "config-backed macOS NFS did not mount; output: {output:?}"
    );
    assert!(
        is_mounted_at(mountpoint),
        "macOS did not report the config-backed NFS mount"
    );
    let io_result = io(mountpoint);

    // The test is opt-in and uses only the child CLI's normal SIGINT path. It
    // never invokes sudo or installs/configures a host NFS helper.
    // SAFETY: the child is the live subprocess just spawned above.
    let signal_result = unsafe { libc::kill(child_guard.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(
        signal_result, 0,
        "send SIGINT to config-backed mount-rs NFS"
    );
    let status = wait_for_exit(child_guard.child_mut());
    assert!(
        status.success(),
        "config-backed macOS NFS CLI did not exit cleanly: {status}"
    );

    stdout_thread.join().expect("join CLI stdout reader");
    let stderr_lines = stderr_thread.join().expect("join CLI stderr reader");
    output.extend(line_receiver.try_iter());
    assert!(
        io_result.is_ok(),
        "macOS NFS byte I/O failed: {io_result:?}"
    );
    assert!(
        output.iter().any(|line| line.contains("unmounted")),
        "config-backed macOS NFS did not report unmount: {output:?}"
    );
    assert!(
        !is_mounted_at(mountpoint),
        "config-backed macOS NFS remained mounted; stdout={output:?}, stderr={stderr_lines:?}"
    );
    child_guard.disarm();
}

#[cfg(target_os = "macos")]
fn run_sqlite_delete_fixture(target: &std::path::Path) -> std::io::Result<()> {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/sqlite_hosting.py");
    run_python_fixture(
        SQLITE_DELETE_DRIVER,
        &script,
        target,
        "SQLite DELETE hosting fixture",
    )
}

#[cfg(target_os = "macos")]
fn verify_sqlite_reopen(target: &std::path::Path) -> std::io::Result<()> {
    run_python_fixture(
        SQLITE_REOPEN_DRIVER,
        std::path::Path::new("-"),
        target,
        "SQLite reopen fixture",
    )
}

#[cfg(target_os = "macos")]
fn run_python_fixture(
    program: &str,
    script: &std::path::Path,
    target: &std::path::Path,
    description: &str,
) -> std::io::Result<()> {
    let mut child = if script == std::path::Path::new("-") {
        let mut command = Command::new("python3");
        command
            .args([
                "-c",
                program,
                target.join("delete.sqlite").to_str().unwrap(),
            ])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        command.spawn()?
    } else {
        let mut command = Command::new("python3");
        command
            .args(["-c", program])
            .arg(script)
            .arg(target)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        command.spawn()?
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(std::io::Error::other(format!(
                "{description} exited with {status}"
            )));
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other(format!(
                "{description} exceeded 180 seconds"
            )));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "macos")]
const SQLITE_DELETE_DRIVER: &str = r#"
import runpy
import sys
from pathlib import Path
fixture = runpy.run_path(sys.argv[1])
fixture["run_case"](Path(sys.argv[2]), "DELETE")
"#;

#[cfg(target_os = "macos")]
const SQLITE_REOPEN_DRIVER: &str = r#"
import sqlite3
import sys
path = sys.argv[1]
db = sqlite3.connect(path, timeout=0.1)
try:
    assert db.execute("PRAGMA journal_mode").fetchone()[0].upper() == "DELETE"
    db.execute("PRAGMA synchronous=FULL")
    assert db.execute("PRAGMA synchronous").fetchone()[0] == 2
    assert db.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
    assert db.execute("SELECT count(*) FROM items").fetchone()[0] == 17
    print(f"CLI_SQLITE_REOPEN_OK sqlite={sqlite3.sqlite_version} journal=DELETE synchronous=FULL", flush=True)
finally:
    db.close()
"#;

#[cfg(target_os = "macos")]
fn effective_test_identity() -> (u32, u32) {
    let uid = std::env::var("SUDO_UID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| {
            // SAFETY: getuid has no pointer arguments or retained state.
            unsafe { libc::getuid() as u32 }
        });
    let gid = std::env::var("SUDO_GID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| {
            // SAFETY: getgid has no pointer arguments or retained state.
            unsafe { libc::getgid() as u32 }
        });
    (uid, gid)
}

fn require_opt_in(variable: &str) {
    assert_eq!(
        std::env::var(variable).ok().as_deref(),
        Some("1"),
        "set {variable}=1 to opt into this native acceptance test"
    );
}

struct NativeChildGuard {
    child: Option<std::process::Child>,
    mountpoint: PathBuf,
    transport: &'static str,
}

impl NativeChildGuard {
    fn new(child: std::process::Child, mountpoint: PathBuf, transport: &'static str) -> Self {
        Self {
            child: Some(child),
            mountpoint,
            transport,
        }
    }

    fn child_mut(&mut self) -> &mut std::process::Child {
        self.child
            .as_mut()
            .expect("native mount child is still owned")
    }

    fn id(&self) -> u32 {
        self.child
            .as_ref()
            .expect("native mount child is still owned")
            .id()
    }

    fn disarm(&mut self) {
        self.child.take();
    }
}

impl Drop for NativeChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                // A panic before the explicit lifecycle path must still give
                // the CLI a chance to unmount its native mount.
                #[cfg(unix)]
                {
                    // SAFETY: this is the subprocess owned by this guard.
                    let _ = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
                }
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while std::time::Instant::now() < deadline {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) => thread::sleep(Duration::from_millis(50)),
                    }
                }
            }
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        cleanup_native_mount(&self.mountpoint, self.transport);
    }
}

fn cleanup_native_mount(mountpoint: &std::path::Path, transport: &str) {
    if !is_mounted_at(mountpoint) {
        return;
    }

    #[cfg(target_os = "linux")]
    let candidates = if transport == "fuse" {
        vec![
            ("fusermount3", vec!["-u"]),
            ("fusermount", vec!["-u"]),
            ("umount", Vec::new()),
        ]
    } else {
        vec![("umount", Vec::new())]
    };
    #[cfg(target_os = "macos")]
    let candidates = if transport == "nfs" {
        vec![("umount", vec!["-f"])]
    } else {
        vec![("umount", Vec::new())]
    };

    for (program, args) in candidates {
        let result = Command::new(program).args(args).arg(mountpoint).status();
        if result.is_ok_and(|status| status.success()) || !is_mounted_at(mountpoint) {
            break;
        }
    }
}

fn unique_mountpoint() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "mount-rs-cli-native-{}-{nanos}",
        std::process::id()
    ))
}

fn wait_for_mount(
    child: &mut std::process::Child,
    receiver: &mpsc::Receiver<String>,
    output: &mut Vec<String>,
    expected_transport: &str,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                let mounted = strip_ansi(&line).contains(&format!("mounted {expected_transport}"));
                output.push(line);
                if mounted {
                    return true;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if child
            .try_wait()
            .expect("poll native mount subprocess")
            .is_some()
        {
            break;
        }
    }
    false
}

fn wait_for_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().expect("poll CLI exit") {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "CLI did not exit after SIGINT"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn strip_ansi(value: &str) -> String {
    let mut plain = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            plain.push(character);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for final_byte in chars.by_ref() {
            if final_byte.is_ascii_alphabetic() {
                break;
            }
        }
    }
    plain
}

#[cfg(target_os = "linux")]
fn is_mounted_at(target: &std::path::Path) -> bool {
    let target = target.to_string_lossy();
    fs::read_to_string("/proc/self/mounts")
        .map(|table| {
            table.lines().any(|line| {
                let mut fields = line.split_whitespace();
                let _source = fields.next();
                fields.next().is_some_and(|path| path == target)
            })
        })
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn is_mounted_at(target: &std::path::Path) -> bool {
    let Ok(output) = Command::new("mount").output() else {
        return false;
    };
    let canonical_target = fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let table = String::from_utf8_lossy(&output.stdout);
    mount_rs_nfs::parse_mount_table(mount_rs_nfs::NfsPlatform::Macos, &table)
        .iter()
        .any(|entry| {
            let entry_target = std::path::Path::new(&entry.target);
            entry_target == target
                || fs::canonicalize(entry_target).is_ok_and(|path| path == canonical_target)
        })
}
