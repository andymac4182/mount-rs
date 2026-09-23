//! Real SQLite hosting through the native NFS client.
//!
//! This is an explicitly opt-in acceptance harness for macOS and Linux. It
//! mounts a [`ChunkedFs`] whose metadata and immutable blocks are separate
//! SQLite databases, then runs the real Python SQLite process fixture against
//! files inside the NFS mount. The fixture uses one host/kernel client only;
//! this test does not claim distributed locking or NLM safety.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::Loopback;
use mount_rs_nfs::{
    NativeNfsMount, NfsMountOptions, NfsPlatform, mount_entry_at, mount_nfs, nfs_client_probe,
    nfs_platform,
};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};

const RUN_ENV: &str = "MOUNT_RS_RUN_NATIVE_NFS_SQLITE";
const ADVERSARIAL_ENV: &str = "MOUNT_RS_SQLITE_NFS_ADVERSARIAL";
const SECOND_VIEW_ENV: &str = "MOUNT_RS_SQLITE_NFS_SECOND_VIEW";
const UNSAFE_NOLOCKS_ENV: &str = "MOUNT_RS_SQLITE_NFS_UNSAFE_NOLOCKS";
const MOUNT_TIMEOUT: Duration = Duration::from_secs(60);
const SQLITE_TIMEOUT: Duration = Duration::from_secs(180);
const ADVERSARIAL_TIMEOUT: Duration = Duration::from_secs(360);
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(45);

type SplitFilesystem = ChunkedFs<SqliteMetadataStore, SqliteBlockStore>;

#[derive(Debug)]
enum WalOutcome {
    Supported,
    Unsupported(String),
}

#[derive(Debug)]
struct SqliteFixtureOutcome {
    wal: WalOutcome,
}

// Keep the shared Python fixture as the source of the real SQLite locking and
// recovery checks, but invoke its individual cases so a filesystem that
// honestly falls back from WAL does not turn the whole acceptance run into a
// false failure.
const DELETE_ONLY_DRIVER: &str = r#"
import runpy
import sys
from pathlib import Path

fixture = runpy.run_path(sys.argv[1])
fixture["run_case"](Path(sys.argv[2]), "DELETE")
"#;

const WAL_DRIVER: &str = r#"
import runpy
import sqlite3
import sys
from pathlib import Path

mount = Path(sys.argv[2])
probe = mount / "wal-probe.sqlite"
db = sqlite3.connect(probe, timeout=0.1)
try:
    actual = str(db.execute("PRAGMA journal_mode=WAL").fetchone()[0]).upper()
finally:
    db.close()
for suffix in ("-wal", "-shm", ""):
    Path(f"{probe}{suffix}").unlink(missing_ok=True)
print(f"MOUNT_RS_WAL_MODE={actual}", flush=True)
if actual == "WAL":
    fixture = runpy.run_path(sys.argv[1])
    fixture["run_case"](mount, "WAL")
"#;

const REOPEN_DRIVER: &str = r#"
import sqlite3
import sys
from pathlib import Path

root = Path(sys.argv[1])
for journal in sys.argv[2:]:
    path = root / (journal.lower() + ".sqlite")
    db = sqlite3.connect(path, timeout=0.1)
    try:
        assert db.execute("PRAGMA journal_mode").fetchone()[0].upper() == journal
        db.execute("PRAGMA synchronous=FULL")
        assert db.execute("PRAGMA synchronous").fetchone()[0] == 2
        assert db.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        rows = db.execute("SELECT id, payload FROM items ORDER BY id").fetchall()
        assert len(rows) == 17, len(rows)
        assert rows[0] == (1, b"uncommitted"), rows[0]
        assert all(payload == b"x" * 8192 for _, payload in rows[1:])
        print(f"MOUNT_RS_SQLITE_REOPEN_OK sqlite={sqlite3.sqlite_version} journal={journal}", flush=True)
    finally:
        db.close()
"#;

/// Owns a mountpoint until the kernel mount has been removed and the empty
/// directory has been deleted. A separate exact-path fallback is needed when
/// `mount_nfs` itself times out before it can return its `NativeNfsMount`.
struct NativeNfsCleanup {
    mount: Option<NativeNfsMount>,
    mountpoint: PathBuf,
    platform: NfsPlatform,
    cleaned: bool,
}

impl NativeNfsCleanup {
    fn new(mountpoint: PathBuf, platform: NfsPlatform) -> Self {
        Self {
            mount: None,
            mountpoint,
            platform,
            cleaned: false,
        }
    }

    fn set_mount(&mut self, mount: NativeNfsMount) {
        self.mount = Some(mount);
    }

    async fn cleanup(&mut self) -> Result<(), String> {
        if self.cleaned {
            return Ok(());
        }
        let mount = self.mount.take();
        let result = tokio::time::timeout(
            CLEANUP_TIMEOUT,
            cleanup_native_mount(self.mountpoint.clone(), self.platform, mount),
        )
        .await
        .map_err(|_| {
            format!(
                "native NFS cleanup exceeded {CLEANUP_TIMEOUT:?} for {}",
                self.mountpoint.display()
            )
        })
        .and_then(|result| result);
        if result.is_ok() {
            self.cleaned = true;
        }
        result
    }
}

impl Drop for NativeNfsCleanup {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let mount = self.mount.take();
        let mountpoint = self.mountpoint.clone();
        let platform = self.platform;
        let _ = std::thread::Builder::new()
            .name("mount-rs-nfs-sqlite-cleanup".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                let _ = runtime.block_on(tokio::time::timeout(
                    CLEANUP_TIMEOUT,
                    cleanup_native_mount(mountpoint, platform, mount),
                ));
            });
    }
}

async fn cleanup_native_mount(
    mountpoint: PathBuf,
    platform: NfsPlatform,
    mount: Option<NativeNfsMount>,
) -> Result<(), String> {
    let mut warnings = Vec::new();
    if let Some(mount) = mount {
        match tokio::time::timeout(UNMOUNT_TIMEOUT, mount.unmount()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warnings.push(format!("graceful unmount: {error}")),
            Err(_) => warnings.push(format!("graceful unmount exceeded {UNMOUNT_TIMEOUT:?}")),
        }
    }

    if mount_is_present(&mountpoint, platform).await? {
        let force_errors = force_unmount(&mountpoint, platform).await;
        if let Err(error) = force_errors {
            warnings.push(error);
        }
    }

    if mount_is_present(&mountpoint, platform).await? {
        return Err(format!(
            "native NFS mount remains listed at {}; refusing to remove the mountpoint{}",
            mountpoint.display(),
            if warnings.is_empty() {
                String::new()
            } else {
                format!(" ({})", warnings.join("; "))
            }
        ));
    }

    let path_for_remove = mountpoint.clone();
    let remove = tokio::time::timeout(
        UNMOUNT_TIMEOUT,
        tokio::task::spawn_blocking(move || fs::remove_dir(&path_for_remove)),
    )
    .await
    .map_err(|_| {
        format!(
            "removing unmounted native NFS mountpoint {} exceeded {UNMOUNT_TIMEOUT:?}",
            mountpoint.display()
        )
    })?
    .map_err(|error| format!("mountpoint cleanup task failed: {error}"))?;
    if let Err(error) = remove
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(format!(
            "removing unmounted native NFS mountpoint {} failed: {error}",
            mountpoint.display()
        ));
    }
    if !warnings.is_empty() {
        eprintln!(
            "native NFS cleanup recovered after: {}",
            warnings.join("; ")
        );
    }
    Ok(())
}

async fn mount_is_present(path: &Path, platform: NfsPlatform) -> Result<bool, String> {
    mount_entry_at(path, platform)
        .await
        .map(|entry| entry.is_some())
        .map_err(|error| {
            format!(
                "could not verify native NFS mount state at {}: {error}",
                path.display()
            )
        })
}

async fn force_unmount(path: &Path, platform: NfsPlatform) -> Result<(), String> {
    let flags: &[&str] = match platform {
        NfsPlatform::Macos => &["-f"],
        NfsPlatform::Linux => &["-f", "-l"],
    };
    let mut errors = Vec::new();
    for flag in flags {
        if !mount_is_present(path, platform).await? {
            return Ok(());
        }
        let mut command = tokio::process::Command::new("umount");
        command
            .arg(flag)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(UNMOUNT_TIMEOUT, command.output())
            .await
            .map_err(|_| format!("umount {flag} exceeded {UNMOUNT_TIMEOUT:?}"))?
            .map_err(|error| format!("could not run umount {flag}: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        errors.push(format!(
            "umount {flag} failed with {}{}",
            output.status,
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    Err(errors.join("; "))
}

async fn run_python_driver(
    driver: &str,
    args: &[&OsStr],
    description: &str,
) -> Result<String, String> {
    let mut command = tokio::process::Command::new("python3");
    command.arg("-c").arg(driver);
    for arg in args {
        command.arg(arg);
    }
    let output = tokio::time::timeout(
        SQLITE_TIMEOUT,
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("Python SQLite fixture exceeded {SQLITE_TIMEOUT:?}"))?
    .map_err(|error| format!("could not start Python SQLite fixture: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        eprint!("{stdout}");
    }
    if !stderr.trim().is_empty() {
        eprint!("{stderr}");
    }
    if !output.status.success() {
        return Err(format!(
            "Python SQLite {description} failed with {}",
            output.status,
        ));
    }
    Ok(stdout.into_owned())
}

async fn run_sqlite_fixture(mountpoint: &Path) -> Result<SqliteFixtureOutcome, String> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sqlite_hosting.py");
    let delete_args = [script.as_os_str(), mountpoint.as_os_str()];
    run_python_driver(DELETE_ONLY_DRIVER, &delete_args, "DELETE fixture").await?;
    let wal_output = run_python_driver(WAL_DRIVER, &delete_args, "WAL capability probe").await?;
    let actual = wal_output
        .lines()
        .find_map(|line| line.strip_prefix("MOUNT_RS_WAL_MODE="))
        .filter(|mode| !mode.is_empty())
        .ok_or_else(|| "WAL capability probe produced no journal-mode marker".to_owned())?
        .to_ascii_uppercase();
    let wal = if actual == "WAL" {
        WalOutcome::Supported
    } else {
        eprintln!(
            "native NFS SQLite WAL unsupported: requested WAL, SQLite selected {actual}; DELETE coverage remains authoritative"
        );
        WalOutcome::Unsupported(actual)
    };
    Ok(SqliteFixtureOutcome { wal })
}

async fn run_adversarial_fixture(
    mountpoint: &Path,
    second_view: Option<&Path>,
    lock_only: bool,
) -> Result<(), String> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sqlite_nfs_adversarial.py");
    let workers =
        std::env::var("MOUNT_RS_SQLITE_NFS_LOAD_WORKERS").unwrap_or_else(|_| "2".to_owned());
    let transactions =
        std::env::var("MOUNT_RS_SQLITE_NFS_LOAD_TRANSACTIONS").unwrap_or_else(|_| "8".to_owned());
    let mut command = tokio::process::Command::new("python3");
    command
        .arg(&script)
        .arg("--mount")
        .arg(mountpoint)
        .arg("--workers")
        .arg(workers)
        .arg("--transactions")
        .arg(transactions)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(view) = second_view {
        command.arg("--second-view").arg(view);
    }
    if lock_only {
        command.arg("--lock-only");
    }
    let output = tokio::time::timeout(ADVERSARIAL_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("SQLite adversarial fixture exceeded {ADVERSARIAL_TIMEOUT:?}"))?
        .map_err(|error| format!("could not start SQLite adversarial fixture: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        eprint!("{stdout}");
    }
    if !stderr.trim().is_empty() {
        eprint!("{stderr}");
    }
    if !output.status.success() {
        return Err(format!(
            "SQLite adversarial fixture failed with {}",
            output.status
        ));
    }
    Ok(())
}

async fn probe_unsafe_nolocks(
    filesystem: &SplitFilesystem,
    platform: NfsPlatform,
) -> Result<(), String> {
    // The default NFSv3 client disables NLM locking. Probe only a competing
    // BEGIN IMMEDIATE on a test-owned file; the contender rolls back without
    // writing even if it acquires a lock while the holder is active.
    let mountpoint = tempfile::tempdir()
        .map_err(|error| format!("creating unsafe no-lock NFS mountpoint failed: {error}"))?
        .keep();
    let mut cleanup = NativeNfsCleanup::new(mountpoint.clone(), platform);
    let mount_result = tokio::time::timeout(
        MOUNT_TIMEOUT,
        mount_nfs(filesystem.clone(), &mountpoint, NfsMountOptions::default()),
    )
    .await;
    match mount_result {
        Ok(Ok(mount)) => cleanup.set_mount(mount),
        Ok(Err(error)) => {
            let cleanup_result = cleanup.cleanup().await;
            return combine_errors(
                Some(format!("unsafe no-lock NFS mount failed: {error}")),
                cleanup_result.err(),
                None,
            )
            .map_or(Ok(()), Err);
        }
        Err(_) => {
            let cleanup_result = cleanup.cleanup().await;
            return combine_errors(
                Some(format!(
                    "unsafe no-lock NFS mount exceeded {MOUNT_TIMEOUT:?}"
                )),
                cleanup_result.err(),
                None,
            )
            .map_or(Ok(()), Err);
        }
    }
    let probe = run_adversarial_fixture(&mountpoint, None, true).await;
    let cleanup_result = cleanup.cleanup().await;
    combine_errors(probe.err(), cleanup_result.err(), None).map_or(Ok(()), Err)
}

fn sqlite_mount_options() -> NfsMountOptions {
    NfsMountOptions::sqlite_single_host()
}

async fn verify_fresh_native_sqlite(
    filesystem: &SplitFilesystem,
    platform: NfsPlatform,
    fixture: &SqliteFixtureOutcome,
) -> Result<(), String> {
    let mountpoint = tempfile::tempdir()
        .map_err(|error| format!("creating fresh SQLite reopen mountpoint failed: {error}"))?
        .keep();
    let mut cleanup = NativeNfsCleanup::new(mountpoint.clone(), platform);
    let mount_result = tokio::time::timeout(
        MOUNT_TIMEOUT,
        mount_nfs(filesystem.clone(), &mountpoint, sqlite_mount_options()),
    )
    .await;
    match mount_result {
        Ok(Ok(mount)) => cleanup.set_mount(mount),
        Ok(Err(error)) => {
            let cleanup_result = cleanup.cleanup().await;
            return combine_errors(
                Some(format!("fresh SQLite reopen mount failed: {error}")),
                cleanup_result
                    .err()
                    .map(|error| format!("fresh reopen cleanup: {error}")),
                None,
            )
            .map_or(Ok(()), Err);
        }
        Err(_) => {
            let cleanup_result = cleanup.cleanup().await;
            return combine_errors(
                Some(format!(
                    "fresh SQLite reopen mount exceeded {MOUNT_TIMEOUT:?}"
                )),
                cleanup_result
                    .err()
                    .map(|error| format!("fresh reopen cleanup: {error}")),
                None,
            )
            .map_or(Ok(()), Err);
        }
    }

    let mut args = vec![mountpoint.as_os_str(), OsStr::new("DELETE")];
    if matches!(&fixture.wal, WalOutcome::Supported) {
        args.push(OsStr::new("WAL"));
    }
    let verification = run_python_driver(
        REOPEN_DRIVER,
        &args,
        "fresh native SQLite reopen verification",
    )
    .await;
    let cleanup_result = cleanup.cleanup().await;
    combine_errors(
        verification.err(),
        cleanup_result
            .err()
            .map(|error| format!("fresh reopen cleanup: {error}")),
        None,
    )
    .map_or(Ok(()), Err)
}

async fn run_native_sqlite_acceptance() -> Result<(), String> {
    let platform = nfs_platform().ok_or_else(|| {
        format!(
            "native NFS SQLite hosting supports macOS and Linux, not {}",
            std::env::consts::OS
        )
    })?;
    let probe = nfs_client_probe();
    if !probe.usable {
        return Err(format!(
            "native NFS prerequisites unavailable on {platform:?}: {}",
            probe
                .reason
                .unwrap_or_else(|| "capability probe failed without a reason".into())
        ));
    }

    let storage = tempfile::tempdir().map_err(|error| {
        format!("could not create SQLite backend directory for native NFS: {error}")
    })?;
    let metadata_path = storage.path().join("metadata.sqlite");
    let blocks_path = storage.path().join("blocks.sqlite");
    let owner = format!(
        "native-nfs-sqlite-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock before Unix epoch: {error}"))?
            .as_nanos()
    );
    let filesystem = ChunkedFs::open(
        SqliteMetadataStore::open(&metadata_path)
            .map_err(|error| format!("opening SQLite metadata store failed: {error}"))?,
        SqliteBlockStore::open(&blocks_path)
            .map_err(|error| format!("opening SQLite block store failed: {error}"))?,
        ChunkedOptions::fixed(owner, 4096)
            .map_err(|error| format!("creating fixed-size chunking configuration failed: {error}"))?
            .with_root_mode(0o777),
    )
    .await
    .map_err(|error| format!("opening split SQLite-backed filesystem failed: {error}"))?;

    let mountpoint = tempfile::tempdir()
        .map_err(|error| format!("creating native NFS mountpoint failed: {error}"))?
        .keep();
    let mut cleanup = NativeNfsCleanup::new(mountpoint.clone(), platform);

    if let Some(entry) = mount_entry_at(&mountpoint, platform)
        .await
        .map_err(|error| format!("checking fresh native NFS mountpoint failed: {error}"))?
    {
        let _ = filesystem.shutdown().await;
        return Err(format!(
            "fresh native NFS mountpoint {} is already mounted by {}",
            mountpoint.display(),
            entry.source
        ));
    }

    let mount_result = tokio::time::timeout(
        MOUNT_TIMEOUT,
        mount_nfs(filesystem.clone(), &mountpoint, sqlite_mount_options()),
    )
    .await;
    match mount_result {
        Ok(Ok(mount)) => cleanup.set_mount(mount),
        Ok(Err(error)) => {
            let cleanup_result = cleanup.cleanup().await;
            let shutdown_result = filesystem.shutdown().await;
            return Err(format_mount_phase_error(
                format!("native NFS mount failed: {error}"),
                cleanup_result,
                shutdown_result,
            ));
        }
        Err(_) => {
            let cleanup_result = cleanup.cleanup().await;
            let shutdown_result = filesystem.shutdown().await;
            return Err(format_mount_phase_error(
                format!("native NFS mount exceeded {MOUNT_TIMEOUT:?}"),
                cleanup_result,
                shutdown_result,
            ));
        }
    }

    let adversarial_requested = std::env::var(ADVERSARIAL_ENV).as_deref() == Ok("1");
    let second_view_requested =
        adversarial_requested && std::env::var(SECOND_VIEW_ENV).as_deref() == Ok("1");
    let mut second_cleanup = None;
    let second_view = if second_view_requested {
        let view = tempfile::tempdir()
            .map_err(|error| format!("creating second native NFS mountpoint failed: {error}"))?
            .keep();
        let mut owned = NativeNfsCleanup::new(view.clone(), platform);
        let second_result = tokio::time::timeout(
            MOUNT_TIMEOUT,
            mount_nfs(filesystem.clone(), &view, sqlite_mount_options()),
        )
        .await;
        match second_result {
            Ok(Ok(mount)) => owned.set_mount(mount),
            Ok(Err(error)) => {
                let _ = owned.cleanup().await;
                let _ = cleanup.cleanup().await;
                let _ = filesystem.shutdown().await;
                return Err(format!("second native NFS mount failed: {error}"));
            }
            Err(_) => {
                let _ = owned.cleanup().await;
                let _ = cleanup.cleanup().await;
                let _ = filesystem.shutdown().await;
                return Err(format!(
                    "second native NFS mount exceeded {MOUNT_TIMEOUT:?}"
                ));
            }
        }
        second_cleanup = Some(owned);
        Some(view)
    } else {
        None
    };

    let fixture_result = run_sqlite_fixture(&mountpoint).await;
    let adversarial_result = if fixture_result.is_ok() && adversarial_requested {
        run_adversarial_fixture(&mountpoint, second_view.as_deref(), false).await
    } else {
        Ok(())
    };
    let unsafe_nolocks_result = if fixture_result.is_ok()
        && adversarial_requested
        && std::env::var(UNSAFE_NOLOCKS_ENV).as_deref() == Ok("1")
    {
        probe_unsafe_nolocks(&filesystem, platform).await
    } else {
        Ok(())
    };
    let second_cleanup_result = if let Some(owned) = second_cleanup.as_mut() {
        owned.cleanup().await
    } else {
        Ok(())
    };
    let cleanup_result = cleanup.cleanup().await;
    let shutdown_result = filesystem.shutdown().await;
    if let Some(error) = combine_errors(
        fixture_result
            .as_ref()
            .err()
            .map(|error| format!("SQLite hosting: {error}")),
        combine_errors(
            adversarial_result
                .err()
                .map(|error| format!("SQLite adversarial hosting: {error}")),
            combine_errors(
                unsafe_nolocks_result
                    .err()
                    .map(|error| format!("unsafe no-lock probe: {error}")),
                second_cleanup_result
                    .err()
                    .map(|error| format!("second-view cleanup: {error}")),
                cleanup_result
                    .err()
                    .map(|error| format!("cleanup: {error}")),
            ),
            None,
        ),
        shutdown_result
            .err()
            .map(|error| format!("split-store shutdown: {error}")),
    ) {
        return Err(error);
    }
    let fixture = fixture_result.expect("successful fixture result after error check");

    let reopened = ChunkedFs::open(
        SqliteMetadataStore::open(&metadata_path)
            .map_err(|error| format!("reopening SQLite metadata store failed: {error}"))?,
        SqliteBlockStore::open(&blocks_path)
            .map_err(|error| format!("reopening SQLite block store failed: {error}"))?,
        ChunkedOptions::fixed("native-nfs-sqlite-reopen", 16384)
            .map_err(|error| format!("creating reopen chunking configuration failed: {error}"))?,
    )
    .await
    .map_err(|error| format!("reopening split SQLite-backed filesystem failed: {error}"))?;
    let verification = verify_reopened_sqlite(&reopened, &fixture).await;
    let native_reopen = if verification.is_ok() {
        verify_fresh_native_sqlite(&reopened, platform, &fixture).await
    } else {
        Err("fresh native SQLite reopen skipped because split-store verification failed".into())
    };
    let reopen_shutdown = reopened.shutdown().await;
    combine_errors(
        verification.err(),
        native_reopen
            .err()
            .map(|error| format!("fresh native SQLite reopen: {error}")),
        reopen_shutdown
            .err()
            .map(|error| format!("reopened split-store shutdown: {error}")),
    )
    .map_or(Ok(()), Err)
}

async fn verify_reopened_sqlite(
    filesystem: &SplitFilesystem,
    fixture: &SqliteFixtureOutcome,
) -> Result<(), String> {
    let loopback = Loopback::new(filesystem.clone());
    let mut paths = vec!["/delete.sqlite"];
    if matches!(&fixture.wal, WalOutcome::Supported) {
        paths.push("/wal.sqlite");
    } else if let WalOutcome::Unsupported(actual) = &fixture.wal {
        eprintln!(
            "fresh split-store reopen: verifying DELETE database; WAL remains unsupported with native journal mode {actual}"
        );
    }
    for path in paths {
        let stats = loopback
            .stat(path)
            .await
            .map_err(|error| format!("reopened filesystem stat {path} failed: {error}"))?;
        if stats.size == 0 {
            return Err(format!("reopened SQLite database {path} is empty"));
        }
        let bytes = loopback
            .read_file(path)
            .await
            .map_err(|error| format!("reopened filesystem read {path} failed: {error}"))?;
        if bytes.len() as u64 != stats.size {
            return Err(format!(
                "reopened SQLite database {path} length changed from {} to {}",
                stats.size,
                bytes.len()
            ));
        }
    }
    Ok(())
}

fn format_mount_phase_error(
    phase: String,
    cleanup: Result<(), String>,
    shutdown: mount_rs_core::Result<()>,
) -> String {
    combine_errors(
        Some(phase),
        cleanup.err().map(|error| format!("cleanup: {error}")),
        shutdown
            .err()
            .map(|error| format!("split-store shutdown: {error}")),
    )
    .unwrap_or_else(|| "native NFS mount failed without diagnostics".into())
}

fn combine_errors(
    first: Option<String>,
    second: Option<String>,
    third: Option<String>,
) -> Option<String> {
    [first, second, third]
        .into_iter()
        .flatten()
        .reduce(|left, right| format!("{left}; {right}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an explicitly enabled macOS/Linux native NFS client and real SQLite"]
async fn native_nfs_hosts_real_sqlite_on_split_store() {
    assert_eq!(
        std::env::var(RUN_ENV).as_deref(),
        Ok("1"),
        "set {RUN_ENV}=1 to run actual SQLite hosting through native NFS; --ignored must not silently pass"
    );
    if let Err(error) = run_native_sqlite_acceptance().await {
        panic!("native NFS SQLite acceptance failed: {error}");
    }
}
