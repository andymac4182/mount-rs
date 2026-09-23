#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use mount_rs_9p::{P9Mount, P9MountOptions, mount_9p, p9_client_probe, parse_mount_table};
use mount_rs_memfs::MemoryFs;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::task::spawn_blocking;
use tokio::time::timeout;

const NATIVE_TEST_ENV: &str = "MOUNT_RS_9P_NATIVE_TEST";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// These tests are intentionally ignored. The selected CI job must set the
/// opt-in and run the compiled test binary with `--ignored`; an explicit run
/// must fail when the host is not actually capable of doing a native mount.
fn require_native_host() {
    assert_eq!(
        std::env::var_os(NATIVE_TEST_ENV).as_deref(),
        Some(std::ffi::OsStr::new("1")),
        "native 9P tests require {NATIVE_TEST_ENV}=1"
    );
    let probe = p9_client_probe();
    assert!(
        probe.usable,
        "native 9P probe failed: {}",
        probe.reason.unwrap_or_else(|| "unknown reason".to_owned())
    );
}

async fn make_mountpoint(name: &str) -> io::Result<PathBuf> {
    let path =
        std::env::temp_dir().join(format!("mount-rs-9p-native-{name}-{}", std::process::id()));
    spawn_blocking({
        let path = path.clone();
        move || {
            fs::create_dir(&path)?;
            Ok(path)
        }
    })
    .await
    .map_err(|error| io::Error::other(format!("mountpoint task failed: {error}")))?
}

async fn remove_mountpoint(path: PathBuf) -> io::Result<()> {
    spawn_blocking(move || {
        let table = fs::read_to_string("/proc/self/mounts")?;
        let target = path.to_string_lossy();
        if parse_mount_table(&table)
            .iter()
            .any(|entry| entry.target == target)
        {
            return Err(io::Error::other(
                "refusing to remove a live native mountpoint",
            ));
        }
        fs::remove_dir(path)
    })
    .await
    .map_err(|error| io::Error::other(format!("mountpoint cleanup task failed: {error}")))?
}

async fn mounted_file_io(mountpoint: PathBuf) -> io::Result<()> {
    timeout(
        Duration::from_secs(30),
        spawn_blocking(move || {
            use std::os::unix::fs::MetadataExt;

            let source = mountpoint.join("source");
            let renamed = mountpoint.join("renamed");
            let hardlink = mountpoint.join("hardlink");
            let payload = b"native 9P mounted I/O\n";

            fs::write(&source, payload)?;
            let mut file = File::open(&source)?;
            let mut read_back = Vec::new();
            file.read_to_end(&mut read_back)?;
            if read_back != payload {
                return Err(io::Error::other(
                    "mounted file read did not match the write",
                ));
            }

            fs::rename(&source, &renamed)?;
            fs::hard_link(&renamed, &hardlink)?;
            let mut linked_read = Vec::new();
            File::open(&hardlink)?.read_to_end(&mut linked_read)?;
            if linked_read != payload {
                return Err(io::Error::other("hard link read did not match the write"));
            }
            if fs::metadata(&renamed)?.nlink() < 2 {
                return Err(io::Error::other("hard link did not create a second link"));
            }
            Ok(())
        }),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "mounted 9P I/O timed out"))?
    .map_err(|error| io::Error::other(format!("mounted I/O task failed: {error}")))?
}

async fn concurrent_mounted_file_io(mountpoint: &Path) -> io::Result<()> {
    let mut tasks = Vec::new();
    for index in 0..8_u32 {
        let mountpoint = mountpoint.to_path_buf();
        tasks.push(spawn_blocking(move || {
            let source = mountpoint.join(format!("concurrent-{index}"));
            let renamed = mountpoint.join(format!("concurrent-{index}-renamed"));
            let payload = format!("native 9P concurrent payload {index}\n").into_bytes();
            fs::write(&source, &payload)?;
            let read_back = fs::read(&source)?;
            if read_back != payload {
                return Err(io::Error::other(format!(
                    "concurrent native 9P read did not match for worker {index}"
                )));
            }
            fs::rename(&source, &renamed)?;
            if fs::read(&renamed)? != payload {
                return Err(io::Error::other(format!(
                    "concurrent native 9P rename read did not match for worker {index}"
                )));
            }
            Ok::<(), io::Error>(())
        }));
    }
    for task in tasks {
        task.await.map_err(|error| {
            io::Error::other(format!("concurrent native I/O task failed: {error}"))
        })??;
    }
    Ok(())
}

async fn bounded_unmount(mount: &P9Mount) -> io::Result<()> {
    timeout(TEARDOWN_TIMEOUT, mount.unmount())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "native 9P teardown timed out"))?
}

async fn external_umount(path: &Path) -> io::Result<()> {
    let mut child = Command::new("umount")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stderr = child.stderr.take().expect("pipe external umount stderr");
    let status = match timeout(COMMAND_TIMEOUT, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "external umount timed out",
            ));
        }
    };
    if status.success() {
        Ok(())
    } else {
        let mut diagnostic = Vec::new();
        let stderr_state = match timeout(COMMAND_TIMEOUT, stderr.read_to_end(&mut diagnostic)).await
        {
            Ok(Ok(_)) => "complete".to_owned(),
            Ok(Err(error)) => format!("read-error: {error}"),
            Err(_) => "deadline".to_owned(),
        };
        let mount_present = fs::read_to_string("/proc/self/mounts")
            .map(|table| {
                let target = path.to_string_lossy();
                parse_mount_table(&table)
                    .iter()
                    .any(|entry| entry.target == target)
            })
            .map(|present| present.to_string())
            .unwrap_or_else(|error| format!("unknown: {error}"));
        Err(io::Error::other(format!(
            "external umount exited with {status}; target={}; mount_present={mount_present}; stderr_state={stderr_state}; stderr={}",
            path.display(),
            String::from_utf8_lossy(&diagnostic).trim()
        )))
    }
}

async fn mount_with_file_io(name: &str) -> io::Result<()> {
    let mountpoint = make_mountpoint(name).await?;
    let mount = match mount_9p(
        MemoryFs::empty(),
        &mountpoint,
        P9MountOptions {
            unmount_timeout: Some(COMMAND_TIMEOUT),
            ..P9MountOptions::default()
        },
    )
    .await
    {
        Ok(mount) => mount,
        Err(error) => {
            return match remove_mountpoint(mountpoint).await {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(io::Error::other(format!(
                    "{error}; mountpoint cleanup refused: {cleanup_error}"
                ))),
            };
        }
    };

    let io_result = mounted_file_io(mountpoint.clone()).await;
    let unmount_result = bounded_unmount(&mount).await;
    match unmount_result {
        Ok(()) => {
            // The mount is known to be detached here. remove_dir is
            // deliberate: never recursively delete a still-live mountpoint.
            let cleanup_result = remove_mountpoint(mountpoint).await;
            io_result.and(cleanup_result)
        }
        Err(error) => Err(io::Error::other(format!(
            "native 9P mount remains live; refusing recursive cleanup: {error}"
        ))),
    }
}

/// This is intentionally ignored. Running it needs a real v9fs client plus
/// CAP_SYS_ADMIN; ordinary workspace tests must never attempt a privileged
/// host mount.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Linux v9fs/CAP_SYS_ADMIN; set MOUNT_RS_9P_NATIVE_TEST=1 and pass --ignored"]
async fn native_linux_mount_and_unmount_lifecycle_is_opt_in() {
    require_native_host();
    mount_with_file_io("io")
        .await
        .expect("native 9P lifecycle and I/O");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux v9fs/CAP_SYS_ADMIN; set MOUNT_RS_9P_NATIVE_TEST=1 and pass --ignored"]
async fn native_linux_concurrent_file_io_and_unmount_are_bounded() {
    require_native_host();
    let mountpoint = make_mountpoint("concurrent")
        .await
        .expect("create native mountpoint");
    let mount = match mount_9p(
        MemoryFs::empty(),
        &mountpoint,
        P9MountOptions {
            unmount_timeout: Some(COMMAND_TIMEOUT),
            ..P9MountOptions::default()
        },
    )
    .await
    {
        Ok(mount) => mount,
        Err(error) => {
            remove_mountpoint(mountpoint)
                .await
                .expect("remove failed native mountpoint");
            panic!("native concurrent 9P mount failed: {error}");
        }
    };

    let io_result = timeout(
        Duration::from_secs(45),
        concurrent_mounted_file_io(&mountpoint),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "concurrent native I/O timed out"))
    .and_then(|result| result);
    let unmount_result = bounded_unmount(&mount).await;
    match (io_result, unmount_result) {
        (Ok(()), Ok(())) => remove_mountpoint(mountpoint)
            .await
            .expect("remove detached concurrent mountpoint"),
        (io_result, unmount_result) => panic!(
            "native concurrent 9P lifecycle failed; refusing recursive cleanup: I/O={io_result:?}, unmount={unmount_result:?}"
        ),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Linux v9fs/CAP_SYS_ADMIN; set MOUNT_RS_9P_NATIVE_TEST=1 and pass --ignored"]
async fn native_linux_server_close_releases_kernel_connection() {
    require_native_host();
    let mountpoint = make_mountpoint("server-close")
        .await
        .expect("create native mountpoint");
    let mount = match mount_9p(
        MemoryFs::empty(),
        &mountpoint,
        P9MountOptions {
            unmount_timeout: Some(COMMAND_TIMEOUT),
            ..P9MountOptions::default()
        },
    )
    .await
    {
        Ok(mount) => mount,
        Err(error) => {
            remove_mountpoint(mountpoint)
                .await
                .expect("remove failed native mountpoint");
            panic!("native server-close 9P mount failed: {error}");
        }
    };

    let io_result = mounted_file_io(mountpoint.clone()).await;
    let server_close_result = timeout(TEARDOWN_TIMEOUT, mount.server.close())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "native 9P server close timed out"))
        .and_then(|result| result);
    let connection_closed_result = timeout(TEARDOWN_TIMEOUT, mount.connection.wait_closed())
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "native 9P kernel connection did not close",
            )
        });
    let unmount_result = bounded_unmount(&mount).await;
    match (
        io_result,
        server_close_result,
        connection_closed_result,
        unmount_result,
    ) {
        (Ok(()), Ok(()), Ok(()), Ok(())) => remove_mountpoint(mountpoint)
            .await
            .expect("remove detached server-close mountpoint"),
        (io_result, server_close_result, connection_closed_result, unmount_result) => panic!(
            "native server-close 9P lifecycle failed; refusing recursive cleanup: I/O={io_result:?}, server={server_close_result:?}, connection={connection_closed_result:?}, unmount={unmount_result:?}"
        ),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Linux v9fs/CAP_SYS_ADMIN; set MOUNT_RS_9P_NATIVE_TEST=1 and pass --ignored"]
async fn native_linux_external_umount_finishes_server_lifecycle() {
    require_native_host();
    let mountpoint = make_mountpoint("external")
        .await
        .expect("create native mountpoint");
    let mount = mount_9p(
        MemoryFs::empty(),
        &mountpoint,
        P9MountOptions {
            unmount_timeout: Some(COMMAND_TIMEOUT),
            ..P9MountOptions::default()
        },
    )
    .await
    .expect("native 9P mount");

    let io_result = mounted_file_io(mountpoint.clone()).await;
    let external_result = external_umount(&mountpoint).await;
    let close_result = if external_result.is_ok() {
        match timeout(TEARDOWN_TIMEOUT, mount.wait_closed()).await {
            Ok(()) => Ok(()),
            Err(_) => {
                let fallback = bounded_unmount(&mount).await;
                Err(io::Error::other(format!(
                    "external 9P close timed out; bounded teardown attempt: {fallback:?}"
                )))
            }
        }
    } else {
        // Always make a bounded teardown attempt when the external command
        // fails; if the mount is still live, leave the mountpoint intact.
        bounded_unmount(&mount).await
    };

    match (io_result, external_result, close_result) {
        (Ok(()), Ok(()), Ok(())) => {
            assert!(!mount.active());
            assert_eq!(
                mount.server.connection_count().expect("connection count"),
                0
            );
            remove_mountpoint(mountpoint)
                .await
                .expect("remove detached native mountpoint");
        }
        (io_result, external_result, close_result) => {
            panic!(
                "native 9P external teardown failed; refusing recursive cleanup: I/O={io_result:?}, umount={external_result:?}, close={close_result:?}, active={}, connections={:?}",
                mount.active(),
                mount.server.connection_count()
            );
        }
    }
}
