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
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    use async_trait::async_trait;
    use mount_rs_core::{FileHandle, FsDriver};
    use mount_rs_fuse::mount::{
        FuseMountHooks, FuseTransportError, FuseTransportErrorKind, MountOptions, mount,
        mount_with_hooks,
    };
    use mount_rs_memfs::MemoryFs;

    static NEXT_MOUNTPOINT_ID: AtomicU64 = AtomicU64::new(0);

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
        let sequence = NEXT_MOUNTPOINT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "mount-rs-fuse-native-{}-{nonce}-{sequence}",
            std::process::id()
        ))
    }

    fn assert_native_prerequisites() {
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
    }

    fn native_mount_options() -> MountOptions {
        MountOptions {
            default_permissions: false,
            fsname: "mount-rs-native-test".to_owned(),
            init_timeout: std::time::Duration::from_secs(10),
            unmount_timeout: std::time::Duration::from_secs(10),
            ..Default::default()
        }
    }

    struct PanicReadHandle {
        inner: Arc<dyn FileHandle>,
    }

    #[async_trait]
    impl FileHandle for PanicReadHandle {
        async fn read(
            &self,
            _buffer: &mut [u8],
            _position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            panic!("injected native FUSE read panic");
        }

        async fn write(
            &self,
            buffer: &[u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.inner.write(buffer, position).await
        }

        async fn stat(&self) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.inner.stat().await
        }

        async fn truncate(&self, length: u64) -> mount_rs_core::Result<()> {
            self.inner.truncate(length).await
        }

        async fn sync(&self) -> mount_rs_core::Result<()> {
            self.inner.sync().await
        }

        async fn datasync(&self) -> mount_rs_core::Result<()> {
            self.inner.datasync().await
        }

        async fn close(&self) -> mount_rs_core::Result<()> {
            self.inner.close().await
        }
    }

    struct PanicReadDriver {
        inner: Arc<MemoryFs>,
    }

    #[async_trait]
    impl FsDriver for PanicReadDriver {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            self.inner.capabilities()
        }

        async fn stat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.inner.stat(path).await
        }

        async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
            self.inner.readdir(path).await
        }

        async fn open(
            &self,
            path: &str,
            flags: &str,
            mode: u32,
        ) -> mount_rs_core::Result<Arc<dyn FileHandle>> {
            let inner = self.inner.open(path, flags, mode).await?;
            Ok(Arc::new(PanicReadHandle { inner }))
        }
    }

    struct BlockingReadHandle {
        inner: Arc<dyn FileHandle>,
        entered: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl FileHandle for BlockingReadHandle {
        async fn read(
            &self,
            _buffer: &mut [u8],
            _position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.entered.notify_one();
            std::future::pending::<mount_rs_core::Result<usize>>().await
        }

        async fn write(
            &self,
            buffer: &[u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.inner.write(buffer, position).await
        }

        async fn stat(&self) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.inner.stat().await
        }

        async fn truncate(&self, length: u64) -> mount_rs_core::Result<()> {
            self.inner.truncate(length).await
        }

        async fn sync(&self) -> mount_rs_core::Result<()> {
            self.inner.sync().await
        }

        async fn datasync(&self) -> mount_rs_core::Result<()> {
            self.inner.datasync().await
        }

        async fn close(&self) -> mount_rs_core::Result<()> {
            self.inner.close().await
        }
    }

    struct BlockingReadDriver {
        inner: Arc<MemoryFs>,
        entered: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl FsDriver for BlockingReadDriver {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            self.inner.capabilities()
        }

        async fn stat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.inner.stat(path).await
        }

        async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
            self.inner.readdir(path).await
        }

        async fn open(
            &self,
            path: &str,
            flags: &str,
            mode: u32,
        ) -> mount_rs_core::Result<Arc<dyn FileHandle>> {
            let inner = self.inner.open(path, flags, mode).await?;
            Ok(Arc::new(BlockingReadHandle {
                inner,
                entered: Arc::clone(&self.entered),
            }))
        }
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
        assert_native_prerequisites();

        let mountpoint = unique_mountpoint();
        std::fs::create_dir(&mountpoint).expect("create native mountpoint");
        let options = native_mount_options();
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires an explicitly enabled Linux FUSE kernel harness"]
    async fn native_unmount_interrupts_a_blocked_read() {
        assert_native_prerequisites();

        let mountpoint = unique_mountpoint();
        std::fs::create_dir(&mountpoint).expect("create native blocked-read mountpoint");
        let entered = Arc::new(tokio::sync::Notify::new());
        let driver = Arc::new(BlockingReadDriver {
            inner: Arc::new(MemoryFs::empty()),
            entered: Arc::clone(&entered),
        });
        let mounted = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            mount(driver, &mountpoint, native_mount_options()),
        )
        .await
        {
            Ok(Ok(mounted)) => mounted,
            Ok(Err(error)) => {
                let _ = std::fs::remove_dir(&mountpoint);
                panic!("opted-in native blocked-read FUSE mount failed: {error}");
            }
            Err(_) => {
                let _ = std::fs::remove_dir(&mountpoint);
                panic!("native blocked-read FUSE mount did not complete within 15 seconds");
            }
        };

        let file = mountpoint.join("blocked.txt");
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio::task::spawn_blocking({
                let file = file.clone();
                move || std::fs::write(file, b"blocked")
            }),
        )
        .await
        .expect("native blocked-read file write timed out")
        .expect("native blocked-read file write task failed")
        .expect("native blocked-read file write failed");

        let read_task = tokio::task::spawn_blocking(move || std::fs::read(file));
        tokio::time::timeout(std::time::Duration::from_secs(15), entered.notified())
            .await
            .expect("native blocked read did not reach the backend");

        let unmounted =
            tokio::time::timeout(std::time::Duration::from_secs(15), mounted.unmount()).await;
        let read_result = tokio::time::timeout(std::time::Duration::from_secs(15), read_task).await;
        let removed = std::fs::remove_dir(&mountpoint);
        assert!(
            unmounted.is_ok(),
            "native blocked-read unmount timed out: {unmounted:?}"
        );
        assert!(
            unmounted.as_ref().is_ok_and(Result::is_ok),
            "native blocked-read unmount failed: {unmounted:?}"
        );
        assert!(
            read_result.is_ok(),
            "native blocked read did not finish: {read_result:?}"
        );
        let read_result = read_result
            .expect("checked native blocked read completion")
            .expect("native blocked read task failed");
        assert!(
            read_result.is_err(),
            "kernel read must fail when unmount interrupts the backend read"
        );
        assert!(
            removed.is_ok(),
            "remove native blocked-read mountpoint: {removed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires an explicitly enabled Linux FUSE kernel harness"]
    async fn native_backend_read_panic_reports_transport_error_and_closes() {
        assert_native_prerequisites();

        let mountpoint = unique_mountpoint();
        std::fs::create_dir(&mountpoint).expect("create native panic mountpoint");
        let observed = Arc::new(std::sync::Mutex::new(Vec::<FuseTransportError>::new()));
        let observed_callback = Arc::clone(&observed);
        let driver = Arc::new(PanicReadDriver {
            inner: Arc::new(MemoryFs::empty()),
        });
        let mounted = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            mount_with_hooks(
                driver,
                &mountpoint,
                native_mount_options(),
                FuseMountHooks {
                    on_transport_error: Some(Arc::new(move |error| {
                        observed_callback
                            .lock()
                            .expect("transport callback lock")
                            .push(error);
                    })),
                },
            ),
        )
        .await
        {
            Ok(Ok(mounted)) => mounted,
            Ok(Err(error)) => {
                let _ = std::fs::remove_dir(&mountpoint);
                panic!("opted-in native panic FUSE mount failed: {error}");
            }
            Err(_) => {
                let _ = std::fs::remove_dir(&mountpoint);
                panic!("native panic FUSE mount did not complete within 15 seconds");
            }
        };

        let file = mountpoint.join("panic.txt");
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio::task::spawn_blocking({
                let file = file.clone();
                move || std::fs::write(file, b"panic")
            }),
        )
        .await
        .expect("native panic file write timed out")
        .expect("native panic file write task failed")
        .expect("native panic file write failed");

        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio::task::spawn_blocking(move || std::fs::read(file)),
        )
        .await
        .expect("native panic read timed out")
        .expect("native panic read task failed");
        assert!(
            read_result.is_err(),
            "the backend panic must not produce a successful read"
        );

        tokio::time::timeout(std::time::Duration::from_secs(15), mounted.wait_closed())
            .await
            .expect("native panic session did not close");

        {
            let observed = observed.lock().expect("transport callback lock");
            assert_eq!(observed.len(), 1);
            assert_eq!(observed[0].kind, FuseTransportErrorKind::Task);
            assert_eq!(observed[0].message, "FUSE read task panicked");
        }

        let unmounted =
            tokio::time::timeout(std::time::Duration::from_secs(15), mounted.unmount()).await;
        let removed = std::fs::remove_dir(&mountpoint);
        assert!(
            unmounted.is_ok(),
            "native panic unmount timed out: {unmounted:?}"
        );
        assert!(
            unmounted.as_ref().is_ok_and(Result::is_ok),
            "native panic unmount failed: {unmounted:?}"
        );
        assert!(
            removed.is_ok(),
            "remove native panic mountpoint: {removed:?}"
        );
    }
}
