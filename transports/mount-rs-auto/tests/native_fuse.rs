//! Opt-in Linux kernel harness for the automatic facade's named FUSE path.
//!
//! The normal test suite is rootless and does not claim native-mount coverage.
//! Running this ignored test requires `MOUNT_RS_RUN_NATIVE_FUSE=1`, a usable
//! `/dev/fuse`, and either mount capability or an executable fusermount helper.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mount_rs_auto::{AutoMountOptions, AutoTransport, MountMode, MountOptions, mount};
use mount_rs_core::MemoryFs;

fn opted_in() -> bool {
    std::env::var_os("MOUNT_RS_RUN_NATIVE_FUSE").as_deref() == Some(std::ffi::OsStr::new("1"))
}

fn helper_available() -> bool {
    let names = ["fusermount3", "fusermount"];
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .chain([PathBuf::from("/usr/bin"), PathBuf::from("/bin")])
        .any(|directory| {
            names.iter().any(|name| {
                let path = directory.join(name);
                let Ok(metadata) = std::fs::metadata(path) else {
                    return false;
                };
                if !metadata.is_file() {
                    return false;
                }
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            })
        })
}

fn unique_mountpoint() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("mount-rs-auto-fuse-{}-{nonce}", std::process::id()))
}

async fn mounted_file_round_trip(mountpoint: PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let original = mountpoint.join("through-auto.txt");
        let renamed = mountpoint.join("renamed.txt");
        let linked = mountpoint.join("linked.txt");
        std::fs::write(&original, b"automatic fuse")
            .map_err(|error| format!("write through native mount: {error}"))?;
        let bytes = std::fs::read(&original)
            .map_err(|error| format!("read through native mount: {error}"))?;
        if bytes != b"automatic fuse" {
            return Err(format!("unexpected initial bytes: {bytes:?}"));
        }
        std::fs::rename(&original, &renamed)
            .map_err(|error| format!("rename through native mount: {error}"))?;
        std::fs::hard_link(&renamed, &linked)
            .map_err(|error| format!("hard-link through native mount: {error}"))?;
        let linked_bytes = std::fs::read(&linked)
            .map_err(|error| format!("read hard link through native mount: {error}"))?;
        if linked_bytes != b"automatic fuse" {
            return Err(format!("unexpected hard-link bytes: {linked_bytes:?}"));
        }
        let entries = std::fs::read_dir(Path::new(&mountpoint))
            .map_err(|error| format!("readdir through native mount: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("collect readdir through native mount: {error}"))?;
        if entries.len() != 2 {
            return Err(format!(
                "expected two mounted entries, found {}",
                entries.len()
            ));
        }
        Ok(())
    })
    .await
    .map_err(|error| format!("native client worker failed: {error}"))?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an explicitly enabled Linux FUSE kernel harness"]
async fn named_fuse_mount_is_real_and_has_bounded_teardown() {
    assert!(
        opted_in(),
        "set MOUNT_RS_RUN_NATIVE_FUSE=1 when explicitly running this ignored native harness"
    );
    assert!(
        Path::new("/dev/fuse").exists(),
        "native FUSE harness requires /dev/fuse; this is an environment prerequisite, not a skip"
    );
    let uid = unsafe { libc::geteuid() };
    assert!(
        uid == 0 || helper_available(),
        "native FUSE harness requires root/CAP_SYS_ADMIN or an executable fusermount helper"
    );

    let mountpoint = unique_mountpoint();
    std::fs::create_dir(&mountpoint).expect("create native mountpoint");
    let options = AutoMountOptions {
        transport: AutoTransport::Fuse,
        fuse: Some(MountOptions {
            mode: MountMode::Auto,
            default_permissions: false,
            fsname: "mount-rs-auto-native-test".to_owned(),
            init_timeout: Duration::from_secs(10),
            unmount_timeout: Duration::from_secs(10),
            ..MountOptions::default()
        }),
        ..AutoMountOptions::default()
    };

    let mounted = match tokio::time::timeout(
        Duration::from_secs(15),
        mount(MemoryFs::empty(), &mountpoint, options),
    )
    .await
    {
        Ok(Ok(mounted)) => mounted,
        Ok(Err(error)) => {
            std::fs::remove_dir(&mountpoint).unwrap_or_else(|cleanup| {
                panic!(
                    "automatic FUSE mount failed ({error}) and its mountpoint is still busy; refusing recursive cleanup: {cleanup}"
                )
            });
            panic!("opted-in automatic FUSE mount failed: {error}");
        }
        Err(_) => {
            std::fs::remove_dir(&mountpoint).unwrap_or_else(|cleanup| {
                panic!(
                    "automatic FUSE mount timed out and its mountpoint is still busy; refusing recursive cleanup: {cleanup}"
                )
            });
            panic!("automatic FUSE mount did not complete within 15 seconds");
        }
    };

    let client = tokio::time::timeout(
        Duration::from_secs(15),
        mounted_file_round_trip(mountpoint.clone()),
    )
    .await;

    let mut unmount_result = tokio::time::timeout(Duration::from_secs(15), mounted.unmount()).await;
    if mounted.active() {
        unmount_result = tokio::time::timeout(Duration::from_secs(15), mounted.unmount()).await;
    }
    let still_active = mounted.active();
    assert!(client.is_ok(), "native client timed out: {client:?}");
    assert!(client.as_ref().is_ok_and(Result::is_ok), "{client:?}");
    assert!(
        unmount_result.is_ok(),
        "native automatic unmount timed out: {unmount_result:?}"
    );
    assert!(
        unmount_result.as_ref().is_ok_and(Result::is_ok),
        "{unmount_result:?}"
    );
    assert!(
        !still_active,
        "native mount is still active; refusing to remove its mountpoint"
    );
    std::fs::remove_dir(&mountpoint).expect("remove native mountpoint after unmount");
}
