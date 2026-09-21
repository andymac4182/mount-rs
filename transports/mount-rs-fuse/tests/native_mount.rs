//! Opt-in Linux kernel harness for the native FUSE lifecycle.
//!
//! Normal `cargo test` runs no machine mount because this test is ignored. An
//! explicit `--ignored` run must also set `MOUNT_RS_RUN_NATIVE_FUSE=1`; the
//! host then needs a Linux `/dev/fuse` device and either `CAP_SYS_ADMIN`
//! (privileged mode) or an executable, setuid-capable `fusermount3`/`fusermount`
//! helper (rootless mode). `allow_other` is not used, so `/etc/fuse.conf` is
//! not required. This test validates actual filesystem access through the
//! kernel mount; protocol/session wire tests remain in `tests/session.rs` and
//! do not claim native mount coverage.

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
            .any(|directory| {
                names.iter().any(|name| {
                    let path = directory.join(name);
                    std::fs::metadata(path)
                        .map(|metadata| {
                            metadata.is_file() && {
                                #[cfg(unix)]
                                {
                                    use std::os::unix::fs::PermissionsExt;
                                    metadata.permissions().mode() & 0o111 != 0
                                }
                                #[cfg(not(unix))]
                                {
                                    true
                                }
                            }
                        })
                        .unwrap_or(false)
                })
            })
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
        let mut workers = Vec::new();
        for index in 0..8_u32 {
            let path = path.clone();
            workers.push(tokio::task::spawn_blocking(move || {
                let source = path.join(format!("through-kernel-{index}.txt"));
                let renamed = path.join(format!("through-kernel-{index}-renamed.txt"));
                let payload = format!("native fuse {index}").into_bytes();
                std::fs::write(&source, &payload)
                    .map_err(|error| format!("write through mount for worker {index}: {error}"))?;
                let bytes = std::fs::read(&source)
                    .map_err(|error| format!("read through mount for worker {index}: {error}"))?;
                if bytes != payload {
                    return Err(format!(
                        "unexpected bytes through mount for worker {index}: {bytes:?}"
                    ));
                }
                std::fs::rename(&source, &renamed).map_err(|error| {
                    format!("rename through mount for worker {index}: {error}")
                })?;
                let renamed_bytes = std::fs::read(&renamed).map_err(|error| {
                    format!("read renamed file through mount for worker {index}: {error}")
                })?;
                if renamed_bytes != payload {
                    return Err(format!(
                        "unexpected renamed bytes through mount for worker {index}: {renamed_bytes:?}"
                    ));
                }
                Ok::<(), String>(())
            }));
        }
        for (index, worker) in workers.into_iter().enumerate() {
            worker
                .await
                .map_err(|error| format!("native client worker {index} failed: {error}"))??;
        }
        let names = std::fs::read_dir(Path::new(&path))
            .map_err(|error| format!("readdir through mount: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("collect readdir through mount: {error}"))?;
        if names.len() != 8 {
            return Err(format!(
                "expected eight concurrent mounted entries, found {}",
                names.len()
            ));
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires an explicitly enabled Linux FUSE kernel harness"]
    async fn native_mount_round_trip_is_opt_in_and_real() {
        assert!(
            native_mount_opted_in(),
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
        let options = MountOptions {
            default_permissions: false,
            fsname: "mount-rs-native-test".to_owned(),
            init_timeout: std::time::Duration::from_secs(10),
            unmount_timeout: std::time::Duration::from_secs(10),
            ..Default::default()
        };
        let mounted = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            mount(Arc::new(MemoryFs::empty()), &mountpoint, options),
        )
        .await
        {
            Ok(Ok(mounted)) => mounted,
            Ok(Err(error)) => {
                let _ = std::fs::remove_dir(&mountpoint);
                panic!("opted-in native FUSE mount failed: {error}");
            }
            Err(_) => {
                let _ = std::fs::remove_dir(&mountpoint);
                panic!("native FUSE mount did not complete within 15 seconds");
            }
        };

        let client = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            client_round_trip(mountpoint.clone()),
        )
        .await;
        let unmounted =
            tokio::time::timeout(std::time::Duration::from_secs(15), mounted.unmount()).await;
        let removed = std::fs::remove_dir(&mountpoint);
        assert!(client.is_ok(), "native client timed out: {client:?}");
        assert!(client.as_ref().is_ok_and(Result::is_ok), "{client:?}");
        assert!(unmounted.is_ok(), "native unmount timed out: {unmounted:?}");
        assert!(unmounted.as_ref().is_ok_and(Result::is_ok), "{unmounted:?}");
        assert!(removed.is_ok(), "remove native mountpoint: {removed:?}");
    }
}
