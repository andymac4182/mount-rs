#![cfg(target_os = "linux")]

use std::time::Duration;

use mount_rs_9p::{P9MountOptions, mount_9p, p9_client_probe};
use mount_rs_core::MemoryFs;

/// This is intentionally both ignored and environment-gated. Running it needs
/// a real v9fs client plus CAP_SYS_ADMIN; ordinary workspace tests must never
/// attempt a privileged host mount.
#[tokio::test]
#[ignore = "requires Linux v9fs/CAP_SYS_ADMIN; set MOUNT_RS_9P_NATIVE_TEST=1 and pass --ignored"]
async fn native_linux_mount_and_unmount_lifecycle_is_opt_in() {
    if std::env::var_os("MOUNT_RS_9P_NATIVE_TEST").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }
    let probe = p9_client_probe();
    if !probe.usable {
        eprintln!(
            "skipping native 9P test: {}",
            probe.reason.unwrap_or_default()
        );
        return;
    }

    let mountpoint =
        std::env::temp_dir().join(format!("mount-rs-9p-native-{}", std::process::id()));
    std::fs::create_dir(&mountpoint).expect("create native mountpoint");
    let options = P9MountOptions {
        unmount_timeout: Some(Duration::from_secs(10)),
        ..P9MountOptions::default()
    };
    let mount = mount_9p(MemoryFs::empty(), &mountpoint, options)
        .await
        .expect("native 9P mount");
    assert!(mount.active());
    mount.unmount().await.expect("native 9P unmount");
    assert!(!mount.active());
    std::fs::remove_dir_all(mountpoint).expect("remove native mountpoint");
}

#[tokio::test]
#[ignore = "requires Linux v9fs/CAP_SYS_ADMIN; set MOUNT_RS_9P_NATIVE_TEST=1 and pass --ignored"]
async fn native_linux_external_umount_finishes_server_lifecycle() {
    if std::env::var_os("MOUNT_RS_9P_NATIVE_TEST").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }
    if !p9_client_probe().usable {
        return;
    }

    let mountpoint = std::env::temp_dir().join(format!(
        "mount-rs-9p-native-external-{}",
        std::process::id()
    ));
    std::fs::create_dir(&mountpoint).expect("create native mountpoint");
    let mount = mount_9p(MemoryFs::empty(), &mountpoint, P9MountOptions::default())
        .await
        .expect("native 9P mount");
    std::process::Command::new("umount")
        .arg(&mountpoint)
        .status()
        .expect("run external umount")
        .success()
        .then_some(())
        .expect("external umount succeeds");
    mount.wait_closed().await;
    assert!(!mount.active());
    assert_eq!(
        mount.server.connection_count().expect("connection count"),
        0
    );
    std::fs::remove_dir_all(mountpoint).expect("remove native mountpoint");
}
