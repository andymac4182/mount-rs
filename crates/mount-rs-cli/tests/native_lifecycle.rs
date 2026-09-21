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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires an opt-in Linux FUSE setup; see the test command in the CLI README"]
fn cli_fuse_subprocess_mounts_and_unmounts_on_sigint() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_FUSE");

    let mountpoint = unique_mountpoint();
    let artifacts = NativeArtifacts::new(mountpoint.clone(), "fuse");
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
    artifacts.finish();
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires an opt-in Linux FUSE setup; see the test command in the CLI README"]
fn cli_fuse_config_file_binary_mounts_and_round_trips_io() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_FUSE");

    let mountpoint = unique_mountpoint();
    let config_path = mountpoint.with_extension("json");
    let mut artifacts = NativeArtifacts::new(mountpoint.clone(), "fuse");
    artifacts.file(config_path.clone());
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
    artifacts.finish();
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires an opt-in Linux FUSE setup and Python sqlite3"]
fn cli_fuse_sqlite_config_recovers_after_mount_service_crash() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_FUSE");

    let mountpoint = unique_mountpoint();
    let config_path = mountpoint.with_extension("sqlite-config.json");
    let database_path = mountpoint.with_extension("sqlite-backend.db");
    let (uid, gid) = effective_test_identity();
    let mut artifacts = NativeArtifacts::new(mountpoint.clone(), "fuse");
    artifacts.file(config_path.clone());
    artifacts.file(database_path.clone());
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = database_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        artifacts.file(PathBuf::from(sidecar));
    }
    fs::create_dir(&mountpoint).expect("create disposable SQLite FUSE mountpoint");
    fs::write(
        &config_path,
        format!(
            r#"{{
  "version": 1,
  "mountpoint": "{}",
  "transport": "fuse",
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
    .expect("write SQLite FUSE config");

    let child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["mount", "--config"])
        .arg(&config_path)
        .args(["--quiet"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn SQLite FUSE mount-rs subprocess");
    let mut child_guard = NativeChildGuard::new(child, mountpoint.clone(), "fuse");

    let (line_sender, line_receiver) = mpsc::channel::<String>();
    let stdout = child_guard
        .child_mut()
        .stdout
        .take()
        .expect("capture SQLite FUSE CLI stdout");
    let stdout_thread = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = line_sender.send(line);
        }
    });
    let stderr = child_guard
        .child_mut()
        .stderr
        .take()
        .expect("capture SQLite FUSE CLI stderr");
    let stderr_thread = thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });

    let mut output = Vec::new();
    assert!(
        wait_for_mount(child_guard.child_mut(), &line_receiver, &mut output, "fuse"),
        "SQLite FUSE subprocess did not mount; output: {output:?}"
    );
    assert!(
        is_mounted_at(&mountpoint),
        "kernel did not report SQLite FUSE mount"
    );

    let fixture_result = run_sqlite_delete_fixture(&mountpoint);

    // Abruptly terminate the actual mount service after committed SQLite
    // activity. Cleanup is explicit and bounded because a crashed FUSE
    // userspace server can leave the kernel mount behind.
    // SAFETY: this is the subprocess spawned by this test.
    let kill_result = unsafe { libc::kill(child_guard.id() as libc::pid_t, libc::SIGKILL) };
    assert_eq!(kill_result, 0, "SIGKILL SQLite FUSE mount service");
    let status = wait_for_exit(child_guard.child_mut());
    assert!(
        !status.success(),
        "SIGKILLed SQLite FUSE mount service unexpectedly succeeded: {status}"
    );

    stdout_thread
        .join()
        .expect("join SQLite FUSE stdout reader");
    let stderr_lines = stderr_thread
        .join()
        .expect("join SQLite FUSE stderr reader");
    output.extend(line_receiver.try_iter());

    let cleanup_result = cleanup_native_mount_bounded(&mountpoint);
    assert!(
        fixture_result.is_ok(),
        "SQLite FUSE fixture failed: {fixture_result:?}; stdout={output:?}; stderr={stderr_lines:?}"
    );
    assert!(
        cleanup_result.is_ok(),
        "SQLite FUSE crash cleanup failed: {cleanup_result:?}; stdout={output:?}; stderr={stderr_lines:?}"
    );
    assert!(
        !is_mounted_at(&mountpoint),
        "SQLite FUSE mount remained after bounded crash cleanup"
    );
    child_guard.disarm();

    // A fresh CLI process must reopen the same provider state after the
    // service crash. This cycle uses normal SIGINT cleanup.
    run_configured_mount_cycle(&config_path, &mountpoint, "fuse", verify_sqlite_reopen);

    artifacts.finish();
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
    let mut artifacts = NativeArtifacts::new(mountpoint.clone(), "nfs");
    artifacts.file(config_path.clone());
    artifacts.directory(backing_path.clone());
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
    run_configured_mount_cycle(&config_path, &mountpoint, "nfs", |target| {
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
    run_configured_mount_cycle(&config_path, &mountpoint, "nfs", |target| {
        let path = target.join("config-persistent-bytes");
        let mut reopened = std::fs::File::open(&path)?;
        let mut bytes = Vec::new();
        reopened.read_to_end(&mut bytes)?;
        if bytes != payload {
            return Err(std::io::Error::other("reopened process lost NFS bytes"));
        }
        fs::remove_file(path)
    });

    artifacts.finish();
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
    let mut artifacts = NativeArtifacts::new(mountpoint.clone(), "nfs");
    artifacts.file(config_path.clone());
    artifacts.file(database_path.clone());
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = database_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        artifacts.file(PathBuf::from(sidecar));
    }
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

    run_configured_mount_cycle(&config_path, &mountpoint, "nfs", |target| {
        run_sqlite_delete_fixture(target)
    });
    run_configured_mount_cycle(&config_path, &mountpoint, "nfs", verify_sqlite_reopen);

    artifacts.finish();
}

#[test]
#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    feature = "foundationdb"
))]
#[ignore = "requires opt-in native transport, FoundationDB, RustFS, and the host libfdb_c"]
fn cli_foundationdb_rustfs_config_binary_mounts_and_reopens() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_FOUNDATIONDB");
    let cluster_file = std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE")
        .expect("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE must be set");
    let r2_endpoint = std::env::var("R2_ENDPOINT").expect("R2_ENDPOINT must be set");
    let r2_bucket = std::env::var("R2_BUCKET").expect("R2_BUCKET must be set");
    for name in ["R2_ACCESS_KEY_ID", "R2_SECRET_ACCESS_KEY"] {
        assert!(
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .is_some(),
            "{name} must be set"
        );
    }
    let shared_provider =
        std::env::var("MOUNT_RS_CLI_FOUNDATIONDB_SHARED_PROVIDER").as_deref() == Ok("1");
    let authority_prefix = if shared_provider {
        let prefix = std::env::var("MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX")
            .expect("MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX must be set for shared-provider mode");
        assert!(
            !prefix.trim().is_empty(),
            "shared-provider authority prefix must not be empty"
        );
        Some(prefix)
    } else {
        None
    };

    let transport = if cfg!(target_os = "linux") {
        "fuse"
    } else {
        "nfs"
    };
    let mountpoint = unique_mountpoint();
    let config_path = mountpoint.with_extension("foundationdb.json");
    let volume_key = format!(
        "mount-rs/cli-foundationdb/{}/{}",
        std::process::id(),
        unique_mountpoint().display()
    );
    let block_prefix = format!("{volume_key}/blocks");
    let mut artifacts = NativeArtifacts::new(mountpoint.clone(), transport);
    artifacts.file(config_path.clone());
    fs::create_dir(&mountpoint).expect("create FoundationDB native mountpoint");
    let mut metadata = serde_json::json!({
        "kind": "foundationdb",
        "cluster_file": cluster_file,
        "volume_key": volume_key,
        "durable": true,
        "lease_authority": if shared_provider {
            "shared-provider"
        } else {
            "persisted-single-authority"
        }
    });
    if let Some(prefix) = authority_prefix.as_deref() {
        metadata["authority_prefix"] = serde_json::json!(prefix);
    }
    let config = serde_json::json!({
        "version": 1,
        "mountpoint": mountpoint,
        "transport": transport,
        "driver": {
            "kind": "splitstore",
            "storage": {
                "metadata": metadata,
                "blocks": {
                    "kind": "r2",
                    "endpoint": r2_endpoint,
                    "bucket": r2_bucket,
                    "prefix": block_prefix,
                    "access_key_id": {"env": "R2_ACCESS_KEY_ID"},
                    "secret_access_key": {"env": "R2_SECRET_ACCESS_KEY"},
                    "durable": true
                },
                "chunk_size_bytes": 4096,
                "owner": "mount-rs-cli-foundationdb-native"
            }
        }
    });
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize FoundationDB native config"),
    )
    .expect("write FoundationDB native config");

    let path = "foundationdb-native-round-trip";
    let payload = b"FoundationDB metadata with RustFS blocks";
    run_configured_mount_cycle(&config_path, &mountpoint, transport, |target| {
        let path = target.join(path);
        fs::write(&path, payload)?;
        if fs::read(&path)? != payload {
            return Err(std::io::Error::other(
                "FoundationDB/RustFS native read mismatch",
            ));
        }
        Ok(())
    });
    run_configured_mount_cycle(&config_path, &mountpoint, transport, |target| {
        let path = target.join(path);
        if fs::read(&path)? != payload {
            return Err(std::io::Error::other(
                "FoundationDB/RustFS native reopen mismatch",
            ));
        }
        fs::remove_file(path)
    });

    artifacts.finish();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_configured_mount_cycle<F>(
    config_path: &std::path::Path,
    mountpoint: &std::path::Path,
    transport: &'static str,
    io: F,
) where
    F: FnOnce(&std::path::Path) -> std::io::Result<()>,
{
    let child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["mount", "--config"])
        .arg(config_path)
        .args(["--quiet"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn config-backed mount-rs subprocess");
    let mut child_guard = NativeChildGuard::new(child, mountpoint.to_path_buf(), transport);

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
    let ready = wait_for_mount(
        child_guard.child_mut(),
        &line_receiver,
        &mut output,
        transport,
    );
    if !ready {
        // Reap the short-lived child before joining the reader threads so an
        // early mount failure includes the diagnostic that the CLI wrote to
        // stderr. The guard still performs bounded native-mount cleanup if a
        // partially initialized transport left a mount behind.
        let _ = child_guard.child_mut().kill();
        let status = child_guard
            .child_mut()
            .wait()
            .expect("reap failed config-backed mount-rs subprocess");
        stdout_thread.join().expect("join CLI stdout reader");
        let stderr_lines = stderr_thread.join().expect("join CLI stderr reader");
        output.extend(line_receiver.try_iter());
        panic!(
            "config-backed {transport} did not mount; status={status}; stdout={output:?}; stderr={stderr_lines:?}"
        );
    }
    assert!(
        is_mounted_at(mountpoint),
        "kernel did not report the config-backed {transport} mount"
    );
    let io_result = io(mountpoint);

    // The test is opt-in and uses only the child CLI's normal SIGINT path. It
    // never invokes sudo or installs/configures a host NFS helper.
    // SAFETY: the child is the live subprocess just spawned above.
    let signal_result = unsafe { libc::kill(child_guard.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(
        signal_result, 0,
        "send SIGINT to config-backed mount-rs {transport}"
    );
    let status = wait_for_exit(child_guard.child_mut());
    assert!(
        status.success(),
        "config-backed {transport} CLI did not exit cleanly: {status}"
    );

    stdout_thread.join().expect("join CLI stdout reader");
    let stderr_lines = stderr_thread.join().expect("join CLI stderr reader");
    output.extend(line_receiver.try_iter());
    assert!(
        io_result.is_ok(),
        "config-backed {transport} I/O failed: {io_result:?}"
    );
    assert!(
        output.iter().any(|line| line.contains("unmounted")),
        "config-backed {transport} did not report unmount: {output:?}"
    );
    assert!(
        !is_mounted_at(mountpoint),
        "config-backed {transport} remained mounted; stdout={output:?}, stderr={stderr_lines:?}"
    );
    child_guard.disarm();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn verify_sqlite_reopen(target: &std::path::Path) -> std::io::Result<()> {
    run_python_fixture(
        SQLITE_REOPEN_DRIVER,
        std::path::Path::new("-"),
        target,
        "SQLite reopen fixture",
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
const SQLITE_DELETE_DRIVER: &str = r#"
import runpy
import sys
from pathlib import Path
fixture = runpy.run_path(sys.argv[1])
fixture["run_case"](Path(sys.argv[2]), "DELETE")
"#;

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

struct NativeArtifacts {
    mountpoint: PathBuf,
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    transport: &'static str,
    armed: bool,
}

impl NativeArtifacts {
    fn new(mountpoint: PathBuf, transport: &'static str) -> Self {
        Self {
            mountpoint,
            files: Vec::new(),
            directories: Vec::new(),
            transport,
            armed: true,
        }
    }

    fn file(&mut self, path: PathBuf) {
        self.files.push(path);
    }

    fn directory(&mut self, path: PathBuf) {
        self.directories.push(path);
    }

    fn finish(mut self) {
        if let Err(error) = self.cleanup() {
            panic!("native artifact cleanup failed: {error}");
        }
    }

    fn cleanup(&mut self) -> Result<(), String> {
        cleanup_native_mount(&self.mountpoint, self.transport);
        let mut errors = Vec::new();

        if is_mounted_at(&self.mountpoint) {
            return Err(format!(
                "mountpoint {} remained mounted; preserving backing artifacts",
                self.mountpoint.display()
            ));
        } else if let Err(error) = remove_directory(&self.mountpoint) {
            errors.push(format!(
                "remove mountpoint {}: {error}",
                self.mountpoint.display()
            ));
        }

        for path in &self.files {
            if let Err(error) = remove_file(path) {
                errors.push(format!("remove file {}: {error}", path.display()));
            }
        }
        for path in self.directories.iter().rev() {
            if let Err(error) = remove_directory(path) {
                errors.push(format!("remove directory {}: {error}", path.display()));
            }
        }

        if errors.is_empty() {
            self.armed = false;
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

impl Drop for NativeArtifacts {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = self.cleanup()
        {
            eprintln!("native artifact cleanup failed during unwinding: {error}");
        }
    }
}

fn remove_file(path: &std::path::Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_directory(path: &std::path::Path) -> std::io::Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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
    #[cfg(target_os = "linux")]
    {
        let _ = transport;
        if let Err(error) = cleanup_native_mount_bounded(mountpoint) {
            eprintln!("bounded native mount cleanup failed: {error}");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if !is_mounted_at(mountpoint) {
            return;
        }

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
}

#[cfg(target_os = "linux")]
fn cleanup_native_mount_bounded(mountpoint: &std::path::Path) -> Result<(), String> {
    if !is_mounted_at(mountpoint) {
        return Ok(());
    }

    let candidates = [
        ("fusermount3", &["-u"] as &[&str]),
        ("fusermount", &["-u"] as &[&str]),
        ("umount", &[] as &[&str]),
    ];
    for (program, args) in candidates {
        let mut child = match Command::new(program)
            .args(args)
            .arg(mountpoint)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => continue,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    if !is_mounted_at(mountpoint) {
                        return Ok(());
                    }
                    break;
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("poll {program} cleanup: {error}"));
                }
            }
        }
        if !is_mounted_at(mountpoint) {
            return Ok(());
        }
    }

    if is_mounted_at(mountpoint) {
        Err(format!(
            "FUSE mount remains at {} after bounded exact-path cleanup",
            mountpoint.display()
        ))
    } else {
        Ok(())
    }
}

fn unique_mountpoint() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "mount-rs-cli-native-{}-{nanos}-{sequence}",
        std::process::id(),
    ))
}

#[test]
fn native_artifacts_are_unique_and_cleaned_without_a_child() {
    let mountpoint = unique_mountpoint();
    let config_path = mountpoint.with_extension("json");
    let backing_path = mountpoint.with_extension("backing");
    let mut artifacts = NativeArtifacts::new(mountpoint.clone(), "fuse");
    artifacts.file(config_path.clone());
    artifacts.directory(backing_path.clone());

    fs::create_dir(&mountpoint).expect("create test mountpoint");
    fs::write(&config_path, b"{}").expect("create test config");
    fs::create_dir(&backing_path).expect("create test backing directory");
    artifacts.finish();

    assert!(!mountpoint.exists());
    assert!(!config_path.exists());
    assert!(!backing_path.exists());
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
