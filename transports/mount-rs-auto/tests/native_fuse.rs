//! Opt-in Linux kernel harness for the automatic facade's named FUSE path.
//!
//! The normal test suite is rootless and does not claim native-mount coverage.
//! Running this ignored test requires `MOUNT_RS_RUN_NATIVE_FUSE=1`, a usable
//! `/dev/fuse`, and either mount capability or an executable fusermount helper.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use mount_rs_auto::{
    AutoMountHooks, AutoMountOptions, AutoTransport, MountMode, MountOptions, mount,
    mount_with_hooks,
};
use mount_rs_core::{FileHandle, FsDriver, MemoryFs};
use mount_rs_fuse::mount::{FuseMountHooks, FuseTransportError, FuseTransportErrorKind};

static NEXT_MOUNTPOINT_ID: AtomicU64 = AtomicU64::new(0);

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
    let sequence = NEXT_MOUNTPOINT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "mount-rs-auto-fuse-{}-{nonce}-{sequence}",
        std::process::id()
    ))
}

fn assert_native_prerequisites() {
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
}

fn native_fuse_options() -> MountOptions {
    MountOptions {
        mode: MountMode::Auto,
        default_permissions: false,
        fsname: "mount-rs-auto-native-test".to_owned(),
        init_timeout: Duration::from_secs(10),
        unmount_timeout: Duration::from_secs(10),
        ..MountOptions::default()
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
        panic!("injected automatic FUSE read panic");
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> mount_rs_core::Result<usize> {
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
    assert_native_prerequisites();

    let mountpoint = unique_mountpoint();
    std::fs::create_dir(&mountpoint).expect("create native mountpoint");
    let options = AutoMountOptions {
        transport: AutoTransport::Fuse,
        fuse: Some(native_fuse_options()),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an explicitly enabled Linux FUSE kernel harness"]
async fn named_fuse_mount_reports_backend_panic_to_root_hook() {
    assert_native_prerequisites();

    let mountpoint = unique_mountpoint();
    std::fs::create_dir(&mountpoint).expect("create native callback mountpoint");
    let observed = Arc::new(Mutex::new(Vec::<FuseTransportError>::new()));
    let observed_callback = Arc::clone(&observed);
    let options = AutoMountOptions {
        transport: AutoTransport::Fuse,
        fuse: Some(native_fuse_options()),
        ..AutoMountOptions::default()
    };
    let mounted = match tokio::time::timeout(
        Duration::from_secs(15),
        mount_with_hooks(
            PanicReadDriver {
                inner: Arc::new(MemoryFs::empty()),
            },
            &mountpoint,
            options,
            AutoMountHooks {
                fuse: FuseMountHooks {
                    on_transport_error: Some(Arc::new(move |error| {
                        observed_callback
                            .lock()
                            .expect("root transport callback lock")
                            .push(error);
                    })),
                },
                p9: None,
                nfs: None,
            },
        ),
    )
    .await
    {
        Ok(Ok(mounted)) => mounted,
        Ok(Err(error)) => {
            let _ = std::fs::remove_dir(&mountpoint);
            panic!("opted-in automatic callback FUSE mount failed: {error}");
        }
        Err(_) => {
            let _ = std::fs::remove_dir(&mountpoint);
            panic!("automatic callback FUSE mount did not complete within 15 seconds");
        }
    };

    let file = mountpoint.join("panic.txt");
    tokio::time::timeout(
        Duration::from_secs(15),
        tokio::task::spawn_blocking({
            let file = file.clone();
            move || std::fs::write(file, b"panic")
        }),
    )
    .await
    .expect("automatic callback file write timed out")
    .expect("automatic callback file write task failed")
    .expect("automatic callback file write failed");

    let read_result = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::task::spawn_blocking(move || std::fs::read(file)),
    )
    .await
    .expect("automatic callback read timed out")
    .expect("automatic callback read task failed");
    assert!(
        read_result.is_err(),
        "the backend panic must not produce a successful automatic read"
    );

    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if !mounted.active() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("automatic callback mount did not close after backend panic");

    {
        let observed = observed.lock().expect("root transport callback lock");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, FuseTransportErrorKind::Task);
        assert_eq!(observed[0].message, "FUSE read task panicked");
    }

    let unmounted = tokio::time::timeout(Duration::from_secs(15), mounted.unmount()).await;
    assert!(
        unmounted.is_ok(),
        "automatic callback unmount timed out: {unmounted:?}"
    );
    assert!(
        unmounted.as_ref().is_ok_and(Result::is_ok),
        "automatic callback unmount failed: {unmounted:?}"
    );
    assert!(
        !mounted.active(),
        "automatic callback mount is still active"
    );
    std::fs::remove_dir(&mountpoint).expect("remove automatic callback mountpoint");
}
