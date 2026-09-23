//! Opt-in native kernel-mount harness.
//!
//! This is intentionally ignored in normal and rootless test runs. It is the
//! only test in this crate that asks the host kernel to mount NFS; the wire
//! tests must remain runnable by an ordinary user on both supported platforms.

// The cases below invoke macOS/Linux kernel mount clients. Keep their helper
// code under the same platform boundary as the tests; portable wire tests are
// separate and continue to compile/run on Windows.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
#[cfg(target_os = "macos")]
use std::io::{BufRead, BufReader};
#[cfg(target_os = "linux")]
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_nfs::{NativeNfsMount, NfsMountOptions, NfsVersion, mount_nfs, nfs_client_probe};
#[cfg(target_os = "macos")]
use mount_rs_nfs::{NfsPlatform, mount_entry_at};

const MOUNT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
// The expanded Linux v4.1 case deliberately exercises many kernel RPCs, but
// the client deadline remains bounded.  A timed-out blocking syscall cannot be
// cancelled by Tokio, so cleanup must not wait for its JoinHandle indefinitely.
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
static NEXT_MOUNTPOINT_ID: AtomicU64 = AtomicU64::new(0);
#[cfg(target_os = "linux")]
const V4_PAGED_ENTRY_COUNT: usize = 256;

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct NativeV4ExpandedChecks {
    nested_directory_was_directory: bool,
    offset_read: Vec<u8>,
    offset_len: u64,
    moved_bytes: Vec<u8>,
    paged_entries: BTreeSet<String>,
}

#[derive(Debug)]
struct NativeChecks {
    directory_was_directory: bool,
    entries: BTreeSet<String>,
    sidecar_entries: BTreeSet<String>,
    stat_len: u64,
    truncated_len: u64,
    renamed_bytes: Vec<u8>,
    link_bytes: Vec<u8>,
    held_replaced_bytes: Vec<u8>,
    replacement_bytes: Vec<u8>,
    surviving_hardlink_bytes: Vec<u8>,
    #[cfg(target_os = "linux")]
    v4_expanded: Option<NativeV4ExpandedChecks>,
}

struct NativeMountGuard {
    mount: Option<NativeNfsMount>,
    mountpoint: PathBuf,
}

#[cfg(target_os = "macos")]
struct NativeCwdHolder(Child);

#[cfg(target_os = "macos")]
impl Drop for NativeCwdHolder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl NativeMountGuard {
    fn new(mount: NativeNfsMount, mountpoint: PathBuf) -> Self {
        Self {
            mount: Some(mount),
            mountpoint,
        }
    }

    async fn cleanup(&mut self) -> Result<(), String> {
        let Some(mount) = self.mount.clone() else {
            return Ok(());
        };
        let unmount = match tokio::time::timeout(CLEANUP_TIMEOUT, mount.unmount()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("native NFS unmount failed: {error}")),
            Err(_) => Err("native NFS unmount timed out".to_owned()),
        };
        if unmount.is_ok() {
            self.mount.take();
        }
        let mountpoint = self.mountpoint.clone();
        let remove = match tokio::time::timeout(
            CLEANUP_TIMEOUT,
            tokio::task::spawn_blocking(move || fs::remove_dir(&mountpoint)),
        )
        .await
        {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(format!("mountpoint cleanup failed: {error}")),
            Ok(Err(error)) => Err(format!("mountpoint cleanup task failed: {error}")),
            Err(_) => Err("mountpoint cleanup timed out".to_owned()),
        };
        match (unmount, remove) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(unmount), Ok(())) => Err(unmount),
            (Ok(()), Err(remove)) => Err(remove),
            (Err(unmount), Err(remove)) => Err(format!("{unmount}; {remove}")),
        }
    }
}

impl Drop for NativeMountGuard {
    fn drop(&mut self) {
        let Some(mount) = self.mount.take() else {
            return;
        };
        let mountpoint = self.mountpoint.clone();
        let _ = std::thread::Builder::new()
            .name("mount-rs-nfs-native-cleanup".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async {
                    let _ = tokio::time::timeout(CLEANUP_TIMEOUT, mount.unmount()).await;
                    let _ = tokio::time::timeout(
                        CLEANUP_TIMEOUT,
                        tokio::task::spawn_blocking(move || fs::remove_dir(mountpoint)),
                    )
                    .await;
                });
            });
    }
}

fn exercise_namespace(mountpoint: &std::path::Path) -> std::io::Result<NativeChecks> {
    let original_dir = mountpoint.join("native-namespace-before");
    let renamed_dir = mountpoint.join("native-namespace-after");
    fs::create_dir(&original_dir)?;
    fs::rename(&original_dir, &renamed_dir)?;

    let original_file = renamed_dir.join("before.txt");
    let renamed_file = renamed_dir.join("after.txt");
    fs::write(&original_file, b"native nfs\n")?;
    fs::rename(&original_file, &renamed_file)?;
    let directory_was_directory = fs::metadata(&renamed_dir)?.is_dir();
    let stat_len = fs::metadata(&renamed_file)?.len();

    let file = OpenOptions::new().write(true).open(&renamed_file)?;
    file.set_len(4)?;
    file.sync_all()?;
    drop(file);
    let truncated_len = fs::metadata(&renamed_file)?.len();
    let renamed_bytes = fs::read(&renamed_file)?;

    let link = renamed_dir.join("hardlink.txt");
    fs::hard_link(&renamed_file, &link)?;
    let link_bytes = fs::read(&link)?;

    let rename_cases = mountpoint.join("native-rename-cases");
    fs::create_dir(&rename_cases)?;
    let destination = rename_cases.join("destination.txt");
    let replacement = rename_cases.join("replacement.txt");
    fs::write(&destination, b"held before replacement")?;
    fs::write(&replacement, b"replacement bytes")?;
    let mut held_destination = OpenOptions::new().read(true).open(&destination)?;
    fs::rename(&replacement, &destination)?;
    let mut held_replaced_bytes = Vec::new();
    held_destination.read_to_end(&mut held_replaced_bytes)?;
    drop(held_destination);
    let replacement_bytes = fs::read(&destination)?;

    let source = rename_cases.join("source.txt");
    let alias = rename_cases.join("alias.txt");
    fs::write(&source, b"same inode survives")?;
    fs::hard_link(&source, &alias)?;
    fs::rename(&source, &alias)?;
    fs::remove_file(&alias)?;
    let surviving_hardlink_bytes = fs::read(&source)?;
    let all_entries = fs::read_dir(&renamed_dir)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let sidecar_entries = all_entries
        .iter()
        .filter(|name| name.starts_with("._"))
        .cloned()
        .collect::<BTreeSet<_>>();
    let entries = all_entries
        .into_iter()
        .filter(|name| !name.starts_with("._"))
        .collect::<BTreeSet<_>>();

    Ok(NativeChecks {
        directory_was_directory,
        entries,
        sidecar_entries,
        stat_len,
        truncated_len,
        renamed_bytes,
        link_bytes,
        held_replaced_bytes,
        replacement_bytes,
        surviving_hardlink_bytes,
        #[cfg(target_os = "linux")]
        v4_expanded: None,
    })
}

fn create_empty_mountpoint() -> std::io::Result<PathBuf> {
    // macOS reports /private/var/folders in mount(8), while std::env::temp_dir
    // commonly returns its /var/folders symlink. Use the real path for exact
    // mount-table assertions and native helper calls.
    let temp_root = fs::canonicalize(std::env::temp_dir())?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    loop {
        // Two native tests run in parallel in CI. The clock alone can return
        // the same nanosecond in both threads, so claim each path atomically.
        let sequence = NEXT_MOUNTPOINT_ID.fetch_add(1, Ordering::Relaxed);
        let mountpoint = temp_root.join(format!(
            "mount-rs-nfs-native-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&mountpoint) {
            Ok(()) => return Ok(mountpoint),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_os = "linux")]
fn exercise_v4_namespace(mountpoint: &std::path::Path) -> std::io::Result<NativeChecks> {
    let mut checks = exercise_namespace(mountpoint)?;

    // Keep this workload separate from the v3 case: it is intentionally a
    // deeper v4.1 interoperability check, while the macOS v3 test remains the
    // small, already-host-validated case.
    let expanded_root = mountpoint.join("native-v4-expanded");
    let nested = expanded_root.join("nested");
    fs::create_dir_all(&nested)?;
    let nested_directory_was_directory = fs::metadata(&nested)?.is_dir();

    let original = nested.join("offset-before.bin");
    let moved = expanded_root.join("offset-after.bin");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&original)?;
    file.write_all(b"0123456789")?;
    file.seek(SeekFrom::Start(3))?;
    file.write_all(b"ab")?;
    file.sync_all()?;
    file.seek(SeekFrom::Start(2))?;
    let mut offset_read = vec![0; 4];
    file.read_exact(&mut offset_read)?;
    drop(file);
    fs::rename(&original, &moved)?;
    let offset_len = fs::metadata(&moved)?.len();
    let moved_bytes = fs::read(&moved)?;

    // A larger directory makes the kernel issue more than the two-entry
    // listing used by the base case on ordinary clients.  The exact set also
    // catches dropped, duplicated, or stale entries in READDIR replies.
    let paged = expanded_root.join("paged");
    fs::create_dir(&paged)?;
    for index in 0..V4_PAGED_ENTRY_COUNT {
        fs::write(paged.join(format!("f{index:03}")), b"x")?;
    }
    let paged_entries = fs::read_dir(&paged)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<BTreeSet<_>, _>>()?;

    checks.v4_expanded = Some(NativeV4ExpandedChecks {
        nested_directory_was_directory,
        offset_read,
        offset_len,
        moved_bytes,
        paged_entries,
    });
    Ok(checks)
}

async fn run_native_case(version: NfsVersion, opt_in: &str) -> NativeChecks {
    assert_eq!(
        std::env::var(opt_in).ok().as_deref(),
        Some("1"),
        "set {opt_in}=1 to run this real native NFS mount harness"
    );
    let probe = nfs_client_probe();
    assert!(
        probe.usable,
        "native NFS prerequisites are missing: {:?}",
        probe.reason
    );
    if version == NfsVersion::V4_1 {
        assert!(
            probe.v4,
            "Linux NFSv4 client support is missing: {:?}",
            probe.reason
        );
    }

    let mountpoint = create_empty_mountpoint().expect("create empty mountpoint");

    let driver = MemoryFs::new(MemoryOptions {
        root_mode: 0o777,
        ..MemoryOptions::default()
    });
    let options = NfsMountOptions {
        version,
        ..NfsMountOptions::default()
    };
    let mount =
        match tokio::time::timeout(MOUNT_TIMEOUT, mount_nfs(driver, &mountpoint, options)).await {
            Ok(Ok(mount)) => mount,
            Ok(Err(error)) => {
                let _ = tokio::task::spawn_blocking({
                    let mountpoint = mountpoint.clone();
                    move || fs::remove_dir(mountpoint)
                })
                .await;
                panic!("native NFS {version:?} mount failed: {error}");
            }
            Err(_) => {
                let _ = tokio::task::spawn_blocking({
                    let mountpoint = mountpoint.clone();
                    move || fs::remove_dir(mountpoint)
                })
                .await;
                panic!("native NFS {version:?} mount timed out after {MOUNT_TIMEOUT:?}");
            }
        };
    let mut guard = NativeMountGuard::new(mount, mountpoint.clone());
    let mut io_task = tokio::task::spawn_blocking({
        let mountpoint = mountpoint.clone();
        move || {
            #[cfg(target_os = "linux")]
            {
                match version {
                    NfsVersion::V3 => exercise_namespace(&mountpoint),
                    NfsVersion::V4_1 => exercise_v4_namespace(&mountpoint),
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                debug_assert_eq!(version, NfsVersion::V3);
                exercise_namespace(&mountpoint)
            }
        }
    });
    // `spawn_blocking` cannot cancel a thread already in a synchronous NFS
    // syscall.  Abort the task and make only a bounded reap attempt; the
    // cleanup deadline must remain effective even when the kernel client is
    // stuck.
    let (io_timed_out, io_result) = match tokio::time::timeout(IO_TIMEOUT, &mut io_task).await {
        Ok(result) => (false, Some(result)),
        Err(_) => {
            eprintln!(
                "native NFS {version:?} I/O exceeded {IO_TIMEOUT:?}; aborting blocking client and attempting bounded reap"
            );
            io_task.abort();
            if tokio::time::timeout(CLEANUP_TIMEOUT, io_task)
                .await
                .is_err()
            {
                eprintln!(
                    "native NFS {version:?} blocking client was not reaped within {CLEANUP_TIMEOUT:?}"
                );
            }
            (true, None)
        }
    };
    let cleanup = guard.cleanup().await;
    if let Err(error) = cleanup {
        if io_timed_out {
            panic!(
                "native NFS {version:?} operations exceeded {IO_TIMEOUT:?}; bounded blocking-client reap attempted; mount cleanup failed: {error}"
            );
        }
        panic!("native NFS {version:?} mount cleanup failed: {error}");
    }
    if io_timed_out {
        panic!(
            "native NFS {version:?} operations exceeded {IO_TIMEOUT:?}; bounded blocking-client reap attempted"
        );
    }
    match io_result.expect("non-timeout native NFS I/O result") {
        Ok(Ok(checks)) => checks,
        Ok(Err(error)) => panic!("native NFS {version:?} filesystem operations failed: {error}"),
        Err(error) => panic!("native NFS {version:?} operation task failed: {error}"),
    }
}

fn assert_namespace_checks(checks: &NativeChecks) {
    assert!(checks.directory_was_directory);
    assert!(checks.sidecar_entries.iter().all(|sidecar| {
        sidecar
            .strip_prefix("._")
            .is_some_and(|file| checks.entries.contains(file))
    }));
    if cfg!(target_os = "linux") {
        assert!(checks.sidecar_entries.is_empty());
    }
    assert_eq!(checks.stat_len, b"native nfs\n".len() as u64);
    assert_eq!(checks.truncated_len, 4);
    assert_eq!(checks.renamed_bytes, b"nati");
    assert_eq!(checks.link_bytes, b"nati");
    assert_eq!(checks.held_replaced_bytes, b"held before replacement");
    assert_eq!(checks.replacement_bytes, b"replacement bytes");
    assert_eq!(checks.surviving_hardlink_bytes, b"same inode survives");
    assert_eq!(
        checks.entries,
        BTreeSet::from(["after.txt".into(), "hardlink.txt".into()])
    );
}

#[cfg(target_os = "linux")]
fn assert_v4_expanded_checks(checks: &NativeChecks) {
    let expanded = checks
        .v4_expanded
        .as_ref()
        .expect("the v4.1 native case must run its expanded workload");
    assert!(expanded.nested_directory_was_directory);
    assert_eq!(expanded.offset_read, b"2ab5");
    assert_eq!(expanded.offset_len, 10);
    assert_eq!(expanded.moved_bytes, b"012ab56789");
    let expected = (0..V4_PAGED_ENTRY_COUNT)
        .map(|index| format!("f{index:03}"))
        .collect::<BTreeSet<_>>();
    assert_eq!(expanded.paged_entries, expected);
}

#[test]
fn parallel_native_cases_claim_distinct_empty_mountpoints() {
    let mountpoints = std::thread::scope(|scope| {
        let threads = (0..32)
            .map(|_| scope.spawn(|| create_empty_mountpoint().expect("claim mountpoint")))
            .collect::<Vec<_>>();
        threads
            .into_iter()
            .map(|thread| thread.join().expect("mountpoint thread"))
            .collect::<Vec<_>>()
    });
    assert_eq!(mountpoints.iter().collect::<BTreeSet<_>>().len(), 32);
    for mountpoint in mountpoints {
        assert!(
            fs::read_dir(&mountpoint)
                .expect("read empty mountpoint")
                .next()
                .is_none()
        );
        fs::remove_dir(mountpoint).expect("clean up mountpoint");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an opted-in macOS/Linux NFS client and native mount privileges"]
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn native_loopback_mount_round_trip() {
    let checks = run_native_case(NfsVersion::V3, "MOUNT_RS_NFS_NATIVE_TEST").await;
    assert_namespace_checks(&checks);
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an opted-in macOS NFS client and native mount privileges"]
async fn native_busy_cwd_force_unmounts_exact_path() {
    assert_eq!(
        std::env::var("MOUNT_RS_NFS_NATIVE_BUSY_TEST")
            .ok()
            .as_deref(),
        Some("1"),
        "set MOUNT_RS_NFS_NATIVE_BUSY_TEST=1 to run this real native NFS mount harness"
    );
    let probe = nfs_client_probe();
    assert!(probe.usable, "native NFS prerequisites: {:?}", probe.reason);

    let mountpoint = create_empty_mountpoint().expect("create busy-test mountpoint");
    let driver = MemoryFs::new(MemoryOptions {
        root_mode: 0o777,
        ..MemoryOptions::default()
    });
    let mount = tokio::time::timeout(
        MOUNT_TIMEOUT,
        mount_nfs(driver, &mountpoint, NfsMountOptions::default()),
    )
    .await
    .expect("busy-test mount deadline")
    .expect("mount busy-test NFS view");
    let mut guard = NativeMountGuard::new(mount.clone(), mountpoint.clone());

    let mut holder = None;
    let outcome: Result<(), String> = async {
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("cd \"$1\" || exit 1; printf 'ready\\n'; exec sleep 60")
            .arg("mount-rs-nfs-cwd-holder")
            .arg(&mountpoint)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("spawn cwd holder: {error}"))?;
        holder = Some(NativeCwdHolder(child));
        let stdout = holder
            .as_mut()
            .and_then(|holder| holder.0.stdout.take())
            .ok_or_else(|| "cwd holder readiness pipe missing".to_owned())?;
        let ready = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::task::spawn_blocking(move || {
                let mut line = String::new();
                BufReader::new(stdout).read_line(&mut line).map(|_| line)
            }),
        )
        .await
        .map_err(|_| "cwd holder readiness timed out".to_owned())?
        .map_err(|error| format!("cwd holder readiness task: {error}"))?
        .map_err(|error| format!("read cwd holder readiness: {error}"))?;
        if ready != "ready\n" {
            return Err(format!("cwd holder did not enter the mount: {ready:?}"));
        }

        // A child cwd alone does not pin a macOS NFS mount consistently.
        // Keep both a directory descriptor and an open file in this process
        // while the plain helper and the library attempt teardown.
        let _held_directory = fs::File::open(&mountpoint)
            .map_err(|error| format!("open mounted directory: {error}"))?;
        let held_path = mountpoint.join("busy-holder.txt");
        fs::write(&held_path, b"busy native NFS mount\n")
            .map_err(|error| format!("create held file: {error}"))?;
        let _held_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&held_path)
            .map_err(|error| format!("open held file: {error}"))?;
        let listed = tokio::time::timeout(
            CLEANUP_TIMEOUT,
            mount_entry_at(&mountpoint, NfsPlatform::Macos),
        )
        .await
        .map_err(|_| "initial mount-table read timed out".to_owned())?
        .map_err(|error| format!("initial mount-table read: {error}"))?;
        if listed.is_none() {
            return Err("busy-test NFS mount was not listed before umount".to_owned());
        }

        let plain = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new("/sbin/umount")
                .arg(&mountpoint)
                .env("LC_ALL", "C")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| "plain macOS umount timed out".to_owned())?
        .map_err(|error| format!("plain macOS umount: {error}"))?;
        let stderr = String::from_utf8_lossy(&plain.stderr);
        if plain.status.success() || !stderr.to_ascii_lowercase().contains("resource busy") {
            return Err(format!("plain umount did not reproduce Resource busy: {stderr}"));
        }
        let before = tokio::time::timeout(
            CLEANUP_TIMEOUT,
            mount_entry_at(&mountpoint, NfsPlatform::Macos),
        )
        .await
        .map_err(|_| "precondition mount-table read timed out".to_owned())?
        .map_err(|error| format!("precondition mount-table read: {error}"))?;
        if before.is_none() {
            return Err(format!(
                "plain umount detached the test mount despite busy error: status={:?}, stderr={stderr}",
                plain.status
            ));
        }

        tokio::time::timeout(CLEANUP_TIMEOUT, mount.unmount())
            .await
            .map_err(|_| "busy-test unmount timed out".to_owned())?
            .map_err(|error| format!("busy-test force fallback: {error}"))?;
        let entry = tokio::time::timeout(
            CLEANUP_TIMEOUT,
            mount_entry_at(&mountpoint, NfsPlatform::Macos),
        )
        .await
        .map_err(|_| "post-unmount mount-table read timed out".to_owned())?
        .map_err(|error| format!("post-unmount mount-table read: {error}"))?;
        if entry.is_some() {
            return Err("busy-test NFS mount remained listed".to_owned());
        }
        Ok(())
    }
    .await;
    drop(holder);
    let cleanup = guard.cleanup().await;
    assert!(
        outcome.is_ok() && cleanup.is_ok(),
        "busy-test outcome: {outcome:?}; cleanup: {cleanup:?}"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an opted-in Linux NFSv4.1 client and native mount privileges"]
async fn native_loopback_mount_v4_1_round_trip() {
    let checks = run_native_case(NfsVersion::V4_1, "MOUNT_RS_NFS_NATIVE_V4_TEST").await;
    assert_namespace_checks(&checks);
    assert_v4_expanded_checks(&checks);
}
