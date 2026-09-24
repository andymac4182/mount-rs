//! Linux-only end-to-end SQLite hosting acceptance.
//!
//! These tests exercise the real SQLite process through a real child FUSE
//! service backed by separate SQLite metadata and block databases. They are
//! ignored by default: macOS has no Linux FUSE device, and running them is
//! deliberately an opt-in kernel test rather than a protocol smoke test.
//!
//! The crash case proves process-restart recovery after SIGKILL. It does not
//! claim power-loss durability. Lease recovery is driven through the public
//! `ChunkedFs::open` acquisition path; the test never edits or bypasses the
//! provider's ownership fence.

#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::fmt;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::FileTypeExt;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(target_os = "linux")]
use tokio::process::{Child, ChildStdout, Command};

#[cfg(target_os = "linux")]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(target_os = "linux")]
const SERVICE_EXIT_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(target_os = "linux")]
// A loaded CI runner measured DELETE seeds at 21-25 s versus under 2 s on
// its passing same-SHA PR peer. WAL seeds take about 7 s on that peer, so
// keep a bounded deadline that covers this observed >10x native I/O swing.
// Live stage checkpoints identify the SQLite call if this bound is exceeded.
const PYTHON_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(target_os = "linux")]
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(target_os = "linux")]
const LEASE_RETRY_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(target_os = "linux")]
// Keep the lease short for crash recovery without making a large SQLite/FUSE
// request race the provider clock on a loaded Linux CI runner.
const LEASE_TTL_MS: &str = "5000";

#[cfg(target_os = "linux")]
const PYTHON_SQLITE: &str = r#"
import sqlite3
import sys
import time

path, journal, phase, marker = sys.argv[1:5]
started = time.monotonic()
def stage(name):
    print(f"SQLITE_PYTHON_STAGE journal={journal} phase={phase} stage={name} elapsed_ms={int((time.monotonic() - started) * 1000)}", file=sys.stderr, flush=True)

payload = bytes(range(256)) * 4096
expected = (marker.encode("ascii") + b"\0") * 8192
stage("connect-start")
db = sqlite3.connect(path, timeout=2.0)
stage("connect-pass")
try:
    db.execute("PRAGMA busy_timeout=2000")
    stage("journal-start")
    actual = db.execute("PRAGMA journal_mode=" + journal).fetchone()[0].upper()
    stage("journal-pass")
    assert actual == journal, (journal, actual)
    stage("synchronous-start")
    db.execute("PRAGMA synchronous=FULL")
    assert db.execute("PRAGMA synchronous").fetchone()[0] == 2
    stage("synchronous-pass")

    if phase == "seed":
        stage("create-start")
        db.execute("CREATE TABLE records(id INTEGER PRIMARY KEY, payload BLOB NOT NULL)")
        stage("create-pass")
        stage("begin-start")
        db.execute("BEGIN IMMEDIATE")
        stage("begin-pass")
        stage("insert-start")
        db.execute("INSERT INTO records VALUES(1, ?)", (payload,))
        stage("insert-pass")
        stage("commit-start")
        db.commit()
        stage("commit-pass")
        stage("select-start")
        assert db.execute("SELECT payload FROM records WHERE id=1").fetchone()[0] == payload
        stage("select-pass")
    elif phase == "verify":
        stage("select-start")
        assert db.execute("SELECT payload FROM records WHERE id=1").fetchone()[0] == payload
        stage("select-pass")
        stage("begin-start")
        db.execute("BEGIN IMMEDIATE")
        stage("begin-pass")
        stage("insert-start")
        db.execute("INSERT INTO records VALUES(2, ?)", (expected,))
        stage("insert-pass")
        stage("commit-start")
        db.commit()
        stage("commit-pass")
        stage("select-start")
        assert db.execute("SELECT payload FROM records WHERE id=2").fetchone()[0] == expected
        stage("select-pass")
    else:
        raise AssertionError("unknown phase: " + phase)

    stage("integrity-start")
    assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    stage("integrity-pass")
finally:
    stage("close-start")
    db.close()
    stage("close-pass")
"#;

#[cfg(target_os = "linux")]
struct NativeTools {
    fusermount: String,
}

#[cfg(target_os = "linux")]
struct CasePaths {
    root: PathBuf,
    mountpoint: PathBuf,
    metadata: PathBuf,
    blocks: PathBuf,
    exclusive_writeback: bool,
}

#[cfg(target_os = "linux")]
struct ServiceProcess {
    child: Child,
    // Keep the read end alive until the child exits. Otherwise the service's
    // final STOPPED announcement can fail with EPIPE during a graceful test.
    _stdout: BufReader<ChildStdout>,
}

#[cfg(target_os = "linux")]
enum StartFailure {
    LeaseBusy(String),
    Failed(String),
}

#[cfg(target_os = "linux")]
impl fmt::Display for StartFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LeaseBusy(message) => write!(formatter, "lease busy: {message}"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

#[cfg(target_os = "linux")]
fn service_executable() -> PathBuf {
    if let Some(path) = env::var_os("CARGO_BIN_EXE_sqlite_mount_service") {
        return PathBuf::from(path);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("target"));
    let target = if target.is_absolute() {
        target
    } else {
        manifest.join(target)
    };
    target
        .join(if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        })
        .join("examples")
        .join(if cfg!(windows) {
            "sqlite_mount_service.exe"
        } else {
            "sqlite_mount_service"
        })
}

#[cfg(target_os = "linux")]
async fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
async fn native_tools() -> NativeTools {
    assert_eq!(
        env::var("MOUNT_RS_RUN_NATIVE_FUSE").as_deref(),
        Ok("1"),
        "set MOUNT_RS_RUN_NATIVE_FUSE=1 to opt into the Linux kernel harness"
    );
    let device = fs::metadata("/dev/fuse").expect("Linux FUSE device /dev/fuse is required");
    assert!(
        device.file_type().is_char_device(),
        "/dev/fuse must be a character device"
    );
    assert!(
        command_available("python3").await,
        "python3 is required to drive the real SQLite engine"
    );
    assert!(
        command_available("mountpoint").await,
        "the mountpoint command is required for explicit unmount verification"
    );
    let mut fusermount = None;
    for candidate in ["fusermount3", "fusermount"] {
        if command_available(candidate).await {
            fusermount = Some(candidate);
            break;
        }
    }
    let fusermount = fusermount.unwrap_or_else(|| {
        panic!("fusermount3 or fusermount is required for bounded dead-mount cleanup")
    });
    let service = service_executable();
    assert!(
        service.is_file(),
        "missing {}; build the example before running this ignored test",
        service.display()
    );
    NativeTools {
        fusermount: fusermount.to_owned(),
    }
}

#[cfg(target_os = "linux")]
fn new_case(journal: &str, exclusive_writeback: bool) -> CasePaths {
    let root = tempfile::tempdir()
        .expect("create acceptance temp root")
        .keep();
    let mountpoint = root.join(format!("mount-{}", journal.to_ascii_lowercase()));
    fs::create_dir(&mountpoint).expect("create non-mounted acceptance mountpoint");
    CasePaths {
        metadata: root.join("metadata.sqlite"),
        blocks: root.join("blocks.sqlite"),
        mountpoint,
        root,
        exclusive_writeback,
    }
}

#[cfg(target_os = "linux")]
async fn is_mounted(path: &Path) -> bool {
    Command::new("mountpoint")
        .arg("-q")
        .arg(path)
        .status()
        .await
        .expect("run mountpoint for mount-state verification")
        .success()
}

#[cfg(target_os = "linux")]
async fn wait_unmounted(path: &Path) {
    let deadline = Instant::now() + UNMOUNT_TIMEOUT;
    loop {
        if !is_mounted(path).await {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "mountpoint {} remained mounted past cleanup deadline",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(target_os = "linux")]
async fn cleanup_dead_mount(tools: &NativeTools, path: &Path) {
    for arguments in [["-u"].as_slice(), ["-u", "-z"].as_slice()] {
        let _status = tokio::time::timeout(
            UNMOUNT_TIMEOUT,
            Command::new(&tools.fusermount)
                .args(arguments)
                .arg("--")
                .arg(path)
                .status(),
        )
        .await
        .expect("dead-mount fusermount deadline")
        .expect("start fusermount for dead-mount cleanup");
        if !is_mounted(path).await {
            break;
        }
    }
    wait_unmounted(path).await;
}

#[cfg(target_os = "linux")]
fn native_fuse_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(target_os = "linux")]
async fn cleanup_case(case: CasePaths) {
    wait_unmounted(&case.mountpoint).await;
    fs::remove_dir(&case.mountpoint).expect("remove verified-unmounted empty mountpoint");
    for base in [&case.metadata, &case.blocks] {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let mut path = base.as_os_str().to_os_string();
            path.push(suffix);
            match fs::remove_file(PathBuf::from(path)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove backend file after unmount: {error}"),
            }
        }
    }
    fs::remove_dir(&case.root).expect("remove explicit acceptance temp root");
}

#[cfg(target_os = "linux")]
async fn start_service(case: &CasePaths, owner: &str) -> Result<ServiceProcess, StartFailure> {
    let mut child = Command::new(service_executable())
        .arg(&case.metadata)
        .arg(&case.blocks)
        .arg(&case.mountpoint)
        .arg(owner)
        .arg(LEASE_TTL_MS)
        .env("MOUNT_RS_FUSE_MODE", "rootless")
        .env(
            "MOUNT_RS_SQLITE_EXCLUSIVE_WRITEBACK",
            if case.exclusive_writeback { "1" } else { "0" },
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| StartFailure::Failed(format!("spawn service: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| StartFailure::Failed("service stdout was not piped".to_owned()))?;
    let mut stdout = BufReader::new(stdout);
    let mut line = String::new();
    match tokio::time::timeout(STARTUP_TIMEOUT, stdout.read_line(&mut line)).await {
        Ok(Ok(count)) if count > 0 => {}
        Ok(Ok(_)) => {
            let status = child.wait().await.map_err(|error| {
                StartFailure::Failed(format!("wait for service failure: {error}"))
            })?;
            return Err(StartFailure::Failed(format!(
                "service exited before READY with {status}"
            )));
        }
        Ok(Err(error)) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(StartFailure::Failed(format!(
                "read service readiness: {error}"
            )));
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(StartFailure::Failed(
                "service did not announce READY before deadline".to_owned(),
            ));
        }
    };
    if line.trim() == "READY" {
        Ok(ServiceProcess {
            child,
            _stdout: stdout,
        })
    } else if line.contains("EAGAIN") || line.contains("acquire writer") {
        let _ = child.wait().await;
        Err(StartFailure::LeaseBusy(line))
    } else {
        let status = child
            .wait()
            .await
            .map_err(|error| StartFailure::Failed(format!("wait for service error: {error}")))?;
        Err(StartFailure::Failed(format!("{line} ({status})")))
    }
}

#[cfg(target_os = "linux")]
impl ServiceProcess {
    async fn graceful(mut self) {
        let mut stdin = self
            .child
            .stdin
            .take()
            .expect("service stdin was not piped");
        stdin
            .write_all(b"quit\n")
            .await
            .expect("send graceful quit to service");
        drop(stdin);
        let status = tokio::time::timeout(SERVICE_EXIT_TIMEOUT, self.child.wait())
            .await
            .expect("graceful service exit deadline")
            .expect("wait for graceful service exit");
        assert!(status.success(), "service graceful exit failed: {status}");
    }

    async fn crash(mut self) {
        self.child
            .start_kill()
            .expect("send SIGKILL to mount service");
        let status = tokio::time::timeout(SERVICE_EXIT_TIMEOUT, self.child.wait())
            .await
            .expect("crashed service exit deadline")
            .expect("wait for SIGKILL service exit");
        #[cfg(target_os = "linux")]
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(9),
            "service must be terminated by SIGKILL, not merely asked to quit"
        );
        assert!(!status.success(), "SIGKILL unexpectedly reported success");
    }
}

#[cfg(target_os = "linux")]
async fn run_python(path: &Path, journal: &str, phase: &str, marker: &str) {
    let started = Instant::now();
    eprintln!("SQLITE_PYTHON_PHASE_START journal={journal} phase={phase}");
    let status = tokio::time::timeout(
        PYTHON_TIMEOUT,
        Command::new("python3")
            .arg("-c")
            .arg(PYTHON_SQLITE)
            .arg(path)
            .arg(journal)
            .arg(phase)
            .arg(marker)
            .kill_on_drop(true)
            // Stream checkpoints and tracebacks before the deadline. Captured
            // output disappears when timeout drops the subprocess future.
            .stderr(Stdio::inherit())
            .stdout(Stdio::null())
            .status(),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "Python SQLite deadline journal={journal} phase={phase} elapsed_ms={}",
            started.elapsed().as_millis()
        )
    })
    .expect("start Python SQLite process");
    assert!(
        status.success(),
        "Python SQLite {phase} failed: status={status}"
    );
    eprintln!(
        "SQLITE_PYTHON_PHASE_PASS journal={journal} phase={phase} elapsed_ms={}",
        started.elapsed().as_millis()
    );
}

#[cfg(target_os = "linux")]
async fn graceful_case(journal: &str) {
    let case = new_case(journal, false);
    let first = start_service(&case, &format!("graceful-first-{journal}"))
        .await
        .unwrap_or_else(|failure| panic!("start graceful service: {failure}"));
    let marker = format!("{journal}-committed");
    run_python(
        &case.mountpoint.join(format!("{journal}.sqlite")),
        journal,
        "seed",
        &marker,
    )
    .await;
    first.graceful().await;
    wait_unmounted(&case.mountpoint).await;

    let second = start_service(&case, &format!("graceful-second-{journal}"))
        .await
        .unwrap_or_else(|failure| panic!("restart after graceful service: {failure}"));
    run_python(
        &case.mountpoint.join(format!("{journal}.sqlite")),
        journal,
        "verify",
        &marker,
    )
    .await;
    second.graceful().await;
    cleanup_case(case).await;
}

#[cfg(target_os = "linux")]
async fn crash_case(journal: &str, tools: &NativeTools, exclusive_writeback: bool) {
    let case = new_case(journal, exclusive_writeback);
    let first = start_service(&case, &format!("crash-first-{journal}"))
        .await
        .unwrap_or_else(|failure| panic!("start crash service: {failure}"));
    let marker = format!("{journal}-committed");
    run_python(
        &case.mountpoint.join(format!("{journal}.sqlite")),
        journal,
        "seed",
        &marker,
    )
    .await;
    first.crash().await;

    // Probe the provider lease before touching the dead kernel mount. This
    // makes the fencing assertion independent of how long fusermount cleanup
    // takes: a fresh service must observe the old lease while it is still
    // live, rather than silently treating lease expiry as proof of fencing.
    let mut observed_fence = match start_service(&case, &format!("crash-restart-{journal}")).await {
        Err(StartFailure::LeaseBusy(_)) => true,
        Err(failure) => panic!("stale lease was not rejected after SIGKILL: {failure}"),
        Ok(service) => {
            service.crash().await;
            panic!("restart acquired the provider lease before dead-mount cleanup")
        }
    };
    cleanup_dead_mount(tools, &case.mountpoint).await;

    let deadline = Instant::now() + LEASE_RETRY_TIMEOUT;
    let second = loop {
        match start_service(&case, &format!("crash-restart-{journal}")).await {
            Ok(service) => break service,
            Err(StartFailure::LeaseBusy(message)) => {
                observed_fence = true;
                assert!(
                    Instant::now() < deadline,
                    "stale writer lease did not make bounded restart progress: {message}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(failure) => panic!("restart after SIGKILL: {failure}"),
        }
    };
    assert!(
        observed_fence,
        "restart succeeded without observing the production writer fence"
    );
    run_python(
        &case.mountpoint.join(format!("{journal}.sqlite")),
        journal,
        "verify",
        &marker,
    )
    .await;
    second.graceful().await;
    cleanup_case(case).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux /dev/fuse, fusermount3, Python SQLite, and an explicitly enabled native harness"]
async fn sqlite_service_graceful_restart_reopens_delete_and_wal_databases() {
    #[cfg(not(target_os = "linux"))]
    panic!("Linux FUSE service acceptance is unsupported on macOS");

    #[cfg(target_os = "linux")]
    {
        let _test_lock = native_fuse_test_lock().lock().await;
        let _tools = native_tools().await;
        for journal in ["DELETE", "WAL"] {
            graceful_case(journal).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux /dev/fuse, fusermount3, Python SQLite, and an explicitly enabled native harness"]
async fn sqlite_service_sigkill_restart_reopens_committed_delete_and_wal_databases() {
    #[cfg(not(target_os = "linux"))]
    panic!("Linux FUSE service acceptance is unsupported on macOS");

    #[cfg(target_os = "linux")]
    {
        let _test_lock = native_fuse_test_lock().lock().await;
        let tools = native_tools().await;
        for journal in ["DELETE", "WAL"] {
            crash_case(journal, &tools, false).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit Linux FUSE and tests mount-service SIGKILL, not power loss"]
async fn exclusive_writeback_service_sigkill_preserves_synced_delete_and_wal() {
    #[cfg(not(target_os = "linux"))]
    panic!("Linux FUSE service acceptance is unsupported on macOS");

    #[cfg(target_os = "linux")]
    {
        let _test_lock = native_fuse_test_lock().lock().await;
        let tools = native_tools().await;
        for journal in ["DELETE", "WAL"] {
            crash_case(journal, &tools, true).await;
        }
    }
}
