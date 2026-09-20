//! Opt-in native kernel-mount harness.
//!
//! This is intentionally ignored in normal and rootless test runs. It is the
//! only test in this crate that asks the host kernel to mount NFS; the wire
//! tests must remain runnable by an ordinary user on both supported platforms.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_core::{MemoryFs, MemoryOptions};
use mount_rs_nfs::{NativeNfsMount, NfsMountOptions, mount_nfs, nfs_client_probe};

const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an opted-in macOS/Linux NFS client and native mount privileges"]
async fn native_loopback_mount_round_trip() {
    assert_eq!(
        std::env::var("MOUNT_RS_NFS_NATIVE_TEST").ok().as_deref(),
        Some("1"),
        "set MOUNT_RS_NFS_NATIVE_TEST=1 to run the real native NFS mount harness"
    );
    let probe = nfs_client_probe();
    assert!(
        probe.usable,
        "native NFS prerequisites are missing: {:?}",
        probe.reason
    );

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
    let mount = match tokio::time::timeout(
        TEST_TIMEOUT,
        mount_nfs(driver, &mountpoint, NfsMountOptions::default()),
    )
    .await
    {
        Ok(Ok(mount)) => mount,
        Ok(Err(error)) => {
            let _ = tokio::task::spawn_blocking({
                let mountpoint = mountpoint.clone();
                move || fs::remove_dir(mountpoint)
            })
            .await;
            panic!("native NFS mount failed: {error}");
        }
        Err(_) => {
            let _ = tokio::task::spawn_blocking({
                let mountpoint = mountpoint.clone();
                move || fs::remove_dir(mountpoint)
            })
            .await;
            panic!("native NFS mount timed out after {TEST_TIMEOUT:?}");
        }
    };
    let mut guard = NativeMountGuard::new(mount, mountpoint.clone());
    let file = mountpoint.join("native-round-trip.txt");
    let io_result = tokio::time::timeout(
        TEST_TIMEOUT,
        tokio::task::spawn_blocking({
            let file = file.clone();
            move || {
                fs::write(&file, b"native nfs\n")?;
                fs::read(&file)
            }
        }),
    )
    .await;
    let cleanup = guard.cleanup().await;
    let bytes = match io_result {
        Ok(Ok(Ok(bytes))) => bytes,
        Ok(Ok(Err(error))) => panic!("native mount I/O failed: {error}"),
        Ok(Err(error)) => panic!("native mount I/O task failed: {error}"),
        Err(_) => panic!("native mount I/O timed out after {TEST_TIMEOUT:?}"),
    };
    if let Err(error) = cleanup {
        panic!("native mount cleanup failed: {error}");
    }
    assert_eq!(bytes, b"native nfs\n");
}
