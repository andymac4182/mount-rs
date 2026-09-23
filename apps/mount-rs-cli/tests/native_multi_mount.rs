//! Opt-in macOS acceptance for two NFS mountpoints served by one CLI process.
//!
//! The test owns the child process and both exact mountpoints. Its normal path
//! checks the CLI's SIGINT unmount; its failure path performs bounded cleanup
//! before reporting the error.

#![cfg(target_os = "macos")]

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const VISIBILITY_TIMEOUT: Duration = Duration::from_secs(15);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(15);

#[test]
#[ignore = "requires opt-in native macOS NFS mounts"]
fn cli_one_process_two_nfs_mountpoints_share_files_and_unmount() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_MULTI_MOUNT");

    let mut scope = TestScope::new();
    let mut cli = NativeCli::spawn(&scope).expect("spawn run-owned two-mount CLI");
    let exercise = exercise_shared_mounts(&mut cli, &scope);
    // Wait for the owned CLI and inspect both kernel entries before any
    // fallback unmount. That keeps the normal SIGINT assertion meaningful.
    let stop = cli.stop();
    let cleanup = scope.cleanup();
    assert!(
        exercise.is_ok() && stop.is_ok() && cleanup.is_ok(),
        "two-mount acceptance: exercise={exercise:?}; SIGINT stop={stop:?}; awaited cleanup={cleanup:?}"
    );
}

#[test]
#[cfg(feature = "foundationdb")]
#[ignore = "requires opt-in native NFS and an owned disposable FoundationDB cluster"]
fn cli_one_process_two_foundationdb_mountpoints_share_files_and_reopen() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_FOUNDATIONDB_MULTI_MOUNT");
    require_opt_in("MOUNT_RS_FOUNDATIONDB_DISPOSABLE_CLUSTER");
    let cluster_file = PathBuf::from(
        std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE")
            .expect("set MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE to the disposable cluster file"),
    );
    let cluster_description =
        fs::read_to_string(&cluster_file).expect("read disposable FoundationDB cluster file");
    assert!(
        cluster_description.contains("@127.0.0.1:"),
        "native acceptance requires an owned loopback disposable cluster"
    );

    let mut scope = TestScope::new();
    let config = scope
        .write_foundationdb_config(&cluster_file)
        .expect("write run-owned shared FoundationDB config");
    let mut first =
        NativeCli::spawn_config(&scope, &config, "cli-fdb-first.log").expect("spawn first CLI");
    let first_exercise = exercise_shared_mounts(&mut first, &scope);
    let first_stop = first.stop();
    drop(first);

    let (reopen, reopen_stop) = if first_exercise.is_ok() && first_stop.is_ok() {
        match NativeCli::spawn_config(&scope, &config, "cli-fdb-reopen.log") {
            Ok(mut fresh) => {
                let result = verify_foundationdb_reopen(&mut fresh, &scope);
                let stop = fresh.stop();
                (result, stop)
            }
            Err(error) => (Err(format!("spawn fresh CLI: {error}")), Ok(())),
        }
    } else {
        (
            Err("fresh CLI reopen skipped because the first mount cycle failed".to_owned()),
            Ok(()),
        )
    };
    let cleanup = scope.cleanup();
    assert!(
        first_exercise.is_ok()
            && first_stop.is_ok()
            && reopen.is_ok()
            && reopen_stop.is_ok()
            && cleanup.is_ok(),
        "shared FoundationDB two-mount acceptance: first={first_exercise:?}; first SIGINT={first_stop:?}; reopen={reopen:?}; reopen SIGINT={reopen_stop:?}; awaited cleanup={cleanup:?}"
    );
}

fn require_opt_in(name: &str) {
    assert_eq!(
        std::env::var(name).ok().as_deref(),
        Some("1"),
        "set {name}=1 to opt into this native acceptance test"
    );
}

fn exercise_shared_mounts(cli: &mut NativeCli, scope: &TestScope) -> Result<(), String> {
    wait_both_ready(cli, scope)?;

    let a_bytes = b"created through the first CLI mount";
    let b_bytes = b"created through the second CLI mount";
    fs::write(scope.mount_a.join("from-a.txt"), a_bytes)
        .map_err(|error| format!("write through mount A: {error}"))?;
    await_bytes(&scope.mount_b.join("from-a.txt"), a_bytes)
        .map_err(|error| format!("B did not see A's file: {error}"))?;
    fs::write(scope.mount_b.join("from-b.txt"), b_bytes)
        .map_err(|error| format!("write through mount B: {error}"))?;
    await_bytes(&scope.mount_a.join("from-b.txt"), b_bytes)
        .map_err(|error| format!("A did not see B's file: {error}"))?;
    if cli
        .child
        .try_wait()
        .map_err(|error| format!("poll owned CLI after I/O: {error}"))?
        .is_some()
    {
        return Err(format!(
            "CLI exited during bidirectional I/O; log={}",
            cli.log()
        ));
    }
    Ok(())
}

#[cfg(feature = "foundationdb")]
fn verify_foundationdb_reopen(cli: &mut NativeCli, scope: &TestScope) -> Result<(), String> {
    wait_both_ready(cli, scope)?;
    await_bytes(
        &scope.mount_a.join("from-a.txt"),
        b"created through the first CLI mount",
    )?;
    await_bytes(
        &scope.mount_b.join("from-a.txt"),
        b"created through the first CLI mount",
    )?;
    await_bytes(
        &scope.mount_a.join("from-b.txt"),
        b"created through the second CLI mount",
    )?;
    await_bytes(
        &scope.mount_b.join("from-b.txt"),
        b"created through the second CLI mount",
    )?;
    fs::write(
        scope.mount_b.join("after-reopen.txt"),
        b"new CLI writes through B",
    )
    .map_err(|error| format!("fresh CLI write through B: {error}"))?;
    await_bytes(
        &scope.mount_a.join("after-reopen.txt"),
        b"new CLI writes through B",
    )?;
    Ok(())
}

fn wait_both_ready(cli: &mut NativeCli, scope: &TestScope) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if mounted_at(&scope.mount_a)? && mounted_at(&scope.mount_b)? {
            break;
        }
        if cli
            .child
            .try_wait()
            .map_err(|error| format!("poll owned CLI: {error}"))?
            .is_some()
        {
            return Err(format!(
                "CLI exited before both NFS mounts were ready; log={}",
                cli.log()
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "two NFS mountpoints did not appear within {READY_TIMEOUT:?}; log={}",
                cli.log()
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn await_bytes(path: &Path, expected: &[u8]) -> Result<(), String> {
    let deadline = Instant::now() + VISIBILITY_TIMEOUT;
    loop {
        let last = match fs::read(path) {
            Ok(bytes) if bytes == expected => return Ok(()),
            Ok(bytes) => format!("expected {expected:?}, got {bytes:?}"),
            Err(error) => error.to_string(),
        };
        if Instant::now() >= deadline {
            return Err(format!(
                "{} after {VISIBILITY_TIMEOUT:?}: {last}",
                path.display()
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

struct TestScope {
    root: PathBuf,
    cli_binary: PathBuf,
    mount_a: PathBuf,
    mount_b: PathBuf,
    cleaned: bool,
}

impl TestScope {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mount-rs-cli-multi-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create disposable two-mount test directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("restrict disposable test directory");
        let root = fs::canonicalize(root).expect("canonical disposable test directory");
        let mount_a = root.join("mount-a");
        let mount_b = root.join("mount-b");
        fs::create_dir(&mount_a).expect("create mount A directory");
        fs::create_dir(&mount_b).expect("create mount B directory");
        // Keep the binary selected by this test stable through a second CLI
        // reopen even if another Cargo build rewrites the shared target.
        let cli_binary = root.join("mount-rs-test-cli");
        fs::copy(env!("CARGO_BIN_EXE_mount-rs"), &cli_binary)
            .expect("copy this test's CLI into its run-owned scope");
        fs::set_permissions(&cli_binary, fs::Permissions::from_mode(0o700))
            .expect("make run-owned test CLI executable");
        Self {
            root,
            cli_binary,
            mount_a,
            mount_b,
            cleaned: false,
        }
    }

    #[cfg(feature = "foundationdb")]
    fn write_foundationdb_config(&self, cluster_file: &Path) -> Result<PathBuf, String> {
        let run_name = self
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "run-owned directory name is not UTF-8".to_owned())?;
        let provider = serde_json::json!({
            "kind": "foundationdb",
            "cluster_file": cluster_file,
            "volume_key": format!("mount-rs/cli-one-process-multi/{run_name}"),
            "durable": true,
            "lease_authority": "revision-cas"
        });
        let config = serde_json::json!({
            "version": 1,
            "mountpoint": self.mount_a,
            "transport": "nfs",
            "driver": {
                "kind": "splitstore",
                "storage": {
                    "concurrent_writes": true,
                    "metadata": provider.clone(),
                    "blocks": provider,
                    "chunk_size_bytes": 4096,
                    "owner": "mount-rs-cli-one-process-multi"
                }
            }
        });
        let config_path = self.root.join("shared-foundationdb.json");
        fs::write(
            &config_path,
            serde_json::to_vec_pretty(&config)
                .map_err(|error| format!("serialize shared FDB config: {error}"))?,
        )
        .map_err(|error| format!("write shared FDB config: {error}"))?;
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("restrict shared FDB config: {error}"))?;
        Ok(config_path)
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if self.cleaned {
            return Ok(());
        }
        let a = unmount_exact_bounded(&self.mount_a);
        let b = unmount_exact_bounded(&self.mount_b);
        if mounted_at(&self.mount_a)? || mounted_at(&self.mount_b)? {
            return Err(format!(
                "NFS mount remains; preserving {}; exact cleanup A={a:?}, B={b:?}",
                self.root.display()
            ));
        }
        fs::remove_dir_all(&self.root)
            .map_err(|error| format!("remove {}: {error}", self.root.display()))?;
        self.cleaned = true;
        a.and(b)
    }
}

impl Drop for TestScope {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("two-mount native test cleanup failed: {error}");
        }
    }
}

struct NativeCli {
    child: Child,
    log_path: PathBuf,
    mount_a: PathBuf,
    mount_b: PathBuf,
    reaped: bool,
}

impl NativeCli {
    fn spawn(scope: &TestScope) -> io::Result<Self> {
        let mut command = Command::new(&scope.cli_binary);
        command
            .args(["mount", "--transport", "nfs", "--driver", "memory"])
            .arg("--mountpoint")
            .arg(&scope.mount_a)
            .arg("--also-mountpoint")
            .arg(&scope.mount_b)
            .args(["--empty", "--quiet"]);
        Self::spawn_command(scope, command, "cli.log")
    }

    #[cfg(feature = "foundationdb")]
    fn spawn_config(scope: &TestScope, config: &Path, log_name: &str) -> io::Result<Self> {
        let mut command = Command::new(&scope.cli_binary);
        command
            .args(["mount", "--config"])
            .arg(config)
            .arg("--also-mountpoint")
            .arg(&scope.mount_b)
            .arg("--quiet");
        Self::spawn_command(scope, command, log_name)
    }

    fn spawn_command(scope: &TestScope, mut command: Command, log_name: &str) -> io::Result<Self> {
        let log_path = scope.root.join(log_name);
        let log = fs::File::create(&log_path)?;
        let child = command
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?;
        Ok(Self {
            child,
            log_path,
            mount_a: scope.mount_a.clone(),
            mount_b: scope.mount_b.clone(),
            reaped: false,
        })
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log_path)
            .unwrap_or_else(|error| format!("<log unreadable: {error}>"))
    }

    fn stop(&mut self) -> Result<(), String> {
        if self.reaped {
            return Ok(());
        }
        let mut signal_error = None;
        match self.child.try_wait() {
            Ok(None) => {
                // SAFETY: the PID belongs to this exact child process.
                let signal = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
                if signal != 0 {
                    signal_error =
                        Some(format!("signal owned CLI: {}", io::Error::last_os_error()));
                    let _ = self.child.kill();
                }
            }
            Ok(Some(_)) => {}
            Err(error) => {
                signal_error = Some(format!("poll owned CLI before SIGINT: {error}"));
                let _ = self.child.kill();
            }
        }
        let status = wait_child_bounded(&mut self.child, EXIT_TIMEOUT);
        self.reaped = true;
        let status = status.ok_or_else(|| {
            let _ = self.child.kill();
            let _ = self.child.wait();
            format!(
                "CLI did not exit within {EXIT_TIMEOUT:?}; log={}",
                self.log()
            )
        })?;
        let a_mounted = mounted_at(&self.mount_a)?;
        let b_mounted = mounted_at(&self.mount_b)?;
        let log = self.log();
        let a_reported = log.lines().any(|line| {
            line.contains("unmounted") && line.contains(self.mount_a.to_string_lossy().as_ref())
        });
        let b_reported = log.lines().any(|line| {
            line.contains("unmounted") && line.contains(self.mount_b.to_string_lossy().as_ref())
        });
        if !status.success()
            || signal_error.is_some()
            || a_mounted
            || b_mounted
            || !a_reported
            || !b_reported
        {
            return Err(format!(
                "CLI exit={status}; signal={signal_error:?}; A mounted={a_mounted}, B mounted={b_mounted}; A reported={a_reported}, B reported={b_reported}; log={log}"
            ));
        }
        Ok(())
    }
}

impl Drop for NativeCli {
    fn drop(&mut self) {
        if !self.reaped
            && let Err(error) = self.stop()
        {
            eprintln!("owned two-mount CLI cleanup failed: {error}");
        }
    }
}

fn wait_child_bounded(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() >= deadline => return None,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
}

fn unmount_exact_bounded(target: &Path) -> Result<(), String> {
    if !mounted_at(target)? {
        return Ok(());
    }
    let mut helper = Command::new("/sbin/umount")
        .arg("-f")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn exact umount for {}: {error}", target.display()))?;
    if wait_child_bounded(&mut helper, UNMOUNT_TIMEOUT).is_none() {
        let _ = helper.kill();
        let _ = helper.wait();
    }
    if mounted_at(target)? {
        Err(format!("exact NFS mount {} remains", target.display()))
    } else {
        Ok(())
    }
}

fn mounted_at(target: &Path) -> Result<bool, String> {
    let output = Command::new("mount")
        .output()
        .map_err(|error| format!("read macOS mount table: {error}"))?;
    if !output.status.success() {
        return Err(format!("mount table command exited {}", output.status));
    }
    let table = String::from_utf8_lossy(&output.stdout);
    let canonical_target = fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    Ok(
        mount_rs_nfs::parse_mount_table(mount_rs_nfs::NfsPlatform::Macos, &table)
            .iter()
            .any(|entry| {
                let entry_target = Path::new(&entry.target);
                entry_target == target
                    || fs::canonicalize(entry_target).is_ok_and(|path| path == canonical_target)
            }),
    )
}
