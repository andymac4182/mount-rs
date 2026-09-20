//! Opt-in native kernel-mount harness.
//!
//! This is intentionally ignored in normal and rootless test runs. It is the
//! only test in this crate that asks the host kernel to mount NFS; the wire
//! tests must remain runnable by an ordinary user on both supported platforms.

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_core::{MemoryFs, MemoryOptions};
use mount_rs_nfs::{NfsMountOptions, mount_nfs, nfs_client_probe};

#[tokio::test]
#[ignore = "requires an opted-in macOS/Linux NFS client and native mount privileges"]
async fn native_loopback_mount_round_trip() {
    if std::env::var("MOUNT_RS_NFS_NATIVE_TEST").ok().as_deref() != Some("1") {
        eprintln!("set MOUNT_RS_NFS_NATIVE_TEST=1 to run the real native NFS mount harness");
        return;
    }
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
    let mount = match mount_nfs(driver, &mountpoint, NfsMountOptions::default()).await {
        Ok(mount) => mount,
        Err(error) => {
            let _ = fs::remove_dir(&mountpoint);
            panic!("native NFS mount failed: {error}");
        }
    };
    let file = mountpoint.join("native-round-trip.txt");
    fs::write(&file, b"native nfs\n").expect("write through native mount");
    assert_eq!(
        fs::read(&file).expect("read through native mount"),
        b"native nfs\n"
    );
    mount.unmount().await.expect("native NFS unmount");
    fs::remove_dir(&mountpoint).expect("remove unmounted mountpoint");
}
