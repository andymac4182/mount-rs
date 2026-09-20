//! Opt-in Linux kernel harness for the native FUSE lifecycle.
//!
//! Normal `cargo test` runs no machine mount. Set `MOUNT_RS_RUN_NATIVE_FUSE=1`
//! to run this test. The host then needs a Linux `/dev/fuse` device and either
//! `CAP_SYS_ADMIN` (privileged mode) or an executable, setuid-capable
//! `fusermount3`/`fusermount` helper (rootless mode). `allow_other` is not used,
//! so `/etc/fuse.conf` is not required. This test validates actual filesystem
//! access through the kernel mount; protocol/session wire tests remain in
//! `tests/session.rs` and do not claim native mount coverage.

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use mount_rs_core::MemoryFs;
    use mount_rs_fuse::mount::{MountOptions, mount};

    fn native_mount_opted_in() -> bool {
        std::env::var_os("MOUNT_RS_RUN_NATIVE_FUSE").as_deref() == Some(std::ffi::OsStr::new("1"))
    }

    fn helper_available() -> bool {
        let names = ["fusermount3", "fusermount"];
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .chain([PathBuf::from("/usr/bin"), PathBuf::from("/bin")])
            .any(|directory| names.iter().any(|name| directory.join(name).is_file()))
    }

    fn unique_mountpoint() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mount-rs-fuse-native-{}-{nonce}",
            std::process::id()
        ))
    }

    async fn client_round_trip(path: PathBuf) -> Result<(), String> {
        tokio::task::spawn_blocking(move || {
            let file = path.join("through-kernel.txt");
            std::fs::write(&file, b"native fuse")
                .map_err(|error| format!("write through mount: {error}"))?;
            let bytes =
                std::fs::read(&file).map_err(|error| format!("read through mount: {error}"))?;
            if bytes != b"native fuse" {
                return Err(format!("unexpected bytes through mount: {bytes:?}"));
            }
            let names = std::fs::read_dir(Path::new(&path))
                .map_err(|error| format!("readdir through mount: {error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("collect readdir through mount: {error}"))?;
            if names.len() != 1 {
                return Err(format!("expected one mounted entry, found {}", names.len()));
            }
            Ok(())
        })
        .await
        .map_err(|error| format!("native client worker failed: {error}"))?
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_mount_round_trip_is_opt_in_and_real() {
        if !native_mount_opted_in() {
            return;
        }
        if !Path::new("/dev/fuse").is_file() && !Path::new("/dev/fuse").exists() {
            eprintln!("skipping native FUSE harness: /dev/fuse is unavailable");
            return;
        }
        let uid = unsafe { libc::geteuid() };
        if uid != 0 && !helper_available() {
            eprintln!("skipping native FUSE harness: no fusermount helper is available");
            return;
        }

        let mountpoint = unique_mountpoint();
        std::fs::create_dir(&mountpoint).expect("create native mountpoint");
        let options = MountOptions {
            default_permissions: false,
            fsname: "mount-rs-native-test".to_owned(),
            ..Default::default()
        };
        let mounted = match mount(Arc::new(MemoryFs::empty()), &mountpoint, options).await {
            Ok(mounted) => mounted,
            Err(error) => {
                let _ = std::fs::remove_dir(&mountpoint);
                panic!("opted-in native FUSE mount failed: {error}");
            }
        };

        let client = client_round_trip(mountpoint.clone()).await;
        let unmounted = mounted.unmount().await;
        let removed = std::fs::remove_dir(&mountpoint);
        assert!(client.is_ok(), "{client:?}");
        assert!(unmounted.is_ok(), "{unmounted:?}");
        assert!(removed.is_ok(), "{removed:?}");
    }
}
