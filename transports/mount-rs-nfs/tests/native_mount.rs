//! Opt-in native kernel-mount harness.
//!
//! This is intentionally ignored in normal and rootless test runs. It is the
//! only test in this crate that asks the host kernel to mount NFS; the wire
//! tests must remain runnable by an ordinary user on both supported platforms.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_core::{MemoryFs, MemoryOptions};
use mount_rs_nfs::{NativeNfsMount, NfsMountOptions, NfsVersion, mount_nfs, nfs_client_probe};

const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
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
    #[cfg(target_os = "linux")]
    v4_expanded: Option<NativeV4ExpandedChecks>,
}

struct NativeMountGuard {
    mount: Option<NativeNfsMount>,
    mountpoint: PathBuf,
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
        let unmount = match tokio::time::timeout(TEST_TIMEOUT, mount.unmount()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("native NFS unmount failed: {error}")),
            Err(_) => Err("native NFS unmount timed out".to_owned()),
        };
        if unmount.is_ok() {
            self.mount.take();
        }
        let mountpoint = self.mountpoint.clone();
        let remove = match tokio::time::timeout(
            TEST_TIMEOUT,
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
                    let _ = tokio::time::timeout(TEST_TIMEOUT, mount.unmount()).await;
                    let _ = tokio::time::timeout(
                        TEST_TIMEOUT,
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
        #[cfg(target_os = "linux")]
        v4_expanded: None,
    })
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

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let mountpoint = std::env::temp_dir().join(format!(
        "mount-rs-nfs-native-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&mountpoint).expect("create empty mountpoint");

    let driver = MemoryFs::new(MemoryOptions {
        root_mode: 0o777,
        ..MemoryOptions::default()
    });
    let options = NfsMountOptions {
        version,
        ..NfsMountOptions::default()
    };
    let mount =
        match tokio::time::timeout(TEST_TIMEOUT, mount_nfs(driver, &mountpoint, options)).await {
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
                panic!("native NFS {version:?} mount timed out after {TEST_TIMEOUT:?}");
            }
        };
    let mut guard = NativeMountGuard::new(mount, mountpoint.clone());
    let io_result = tokio::time::timeout(
        TEST_TIMEOUT,
        tokio::task::spawn_blocking({
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
        }),
    )
    .await;
    let cleanup = guard.cleanup().await;
    if let Err(error) = cleanup {
        panic!("native NFS {version:?} mount cleanup failed: {error}");
    }
    match io_result {
        Ok(Ok(Ok(checks))) => checks,
        Ok(Ok(Err(error))) => {
            panic!("native NFS {version:?} filesystem operations failed: {error}")
        }
        Ok(Err(error)) => panic!("native NFS {version:?} operation task failed: {error}"),
        Err(_) => panic!("native NFS {version:?} operations timed out after {TEST_TIMEOUT:?}"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an opted-in macOS/Linux NFS client and native mount privileges"]
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn native_loopback_mount_round_trip() {
    let checks = run_native_case(NfsVersion::V3, "MOUNT_RS_NFS_NATIVE_TEST").await;
    assert_namespace_checks(&checks);
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an opted-in Linux NFSv4.1 client and native mount privileges"]
async fn native_loopback_mount_v4_1_round_trip() {
    let checks = run_native_case(NfsVersion::V4_1, "MOUNT_RS_NFS_NATIVE_V4_TEST").await;
    assert_namespace_checks(&checks);
    assert_v4_expanded_checks(&checks);
}
