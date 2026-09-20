//! Actual Linux kernel I/O, separate from rootless protocol-frame tests.

use mount_rs_core::{FsDriver, Loopback, MemoryFs};
use mount_rs_fuse::mount::{MountOptions, mount};
use std::sync::Arc;

async fn unmount(
    mounted: mount_rs_fuse::mount::FuseMount,
) -> Result<(), mount_rs_fuse::mount::MountError> {
    #[cfg(target_os = "linux")]
    {
        mounted.unmount().await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = mounted;
        Err(mount_rs_fuse::mount::MountError::UnsupportedPlatform)
    }
}

async fn mounted_round_trip(driver: Arc<dyn FsDriver>) {
    let directory = tempfile::tempdir().unwrap();
    let mounted = mount(
        driver.clone(),
        directory.path(),
        MountOptions {
            default_permissions: false,
            ..Default::default()
        },
    )
    .await
    .expect("mount backend through Linux FUSE");
    let path = directory.path().to_owned();
    // Blocking kernel calls must not starve the userspace server's runtime.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            std::fs::create_dir(path.join("dir"))?;
            std::fs::write(path.join("dir/file"), [0, 255, 1, 127])?;
            std::fs::rename(path.join("dir/file"), path.join("renamed"))?;
            std::fs::hard_link(path.join("renamed"), path.join("alias"))?;
            if std::fs::read(path.join("alias"))? != [0, 255, 1, 127] {
                return Err(std::io::Error::other(
                    "mounted read returned different bytes",
                ));
            }
            std::fs::remove_file(path.join("renamed"))?;
            if std::fs::read(path.join("alias"))? != [0, 255, 1, 127] {
                return Err(std::io::Error::other("hard link failed to survive unlink"));
            }
            Ok(())
        }),
    )
    .await;
    let unmounted =
        tokio::time::timeout(std::time::Duration::from_secs(15), unmount(mounted)).await;
    assert!(
        result
            .as_ref()
            .is_ok_and(|task| task.as_ref().is_ok_and(Result::is_ok)),
        "kernel I/O failed: {result:?}"
    );
    assert!(
        unmounted.as_ref().is_ok_and(Result::is_ok),
        "unmount failed: {unmounted:?}"
    );
    assert_eq!(
        Loopback::from_arc(driver)
            .read_file("/alias")
            .await
            .unwrap(),
        [0, 255, 1, 127]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an explicitly enabled Linux FUSE kernel harness"]
async fn mounted_memory_sqlite_and_object_store_match_and_persist() {
    assert_eq!(
        std::env::var("MOUNT_RS_RUN_NATIVE_FUSE").as_deref(),
        Ok("1")
    );
    mounted_round_trip(Arc::new(MemoryFs::empty())).await;
    let database_dir = tempfile::tempdir().unwrap();
    let database = database_dir.path().join("filesystem.sqlite");
    mounted_round_trip(Arc::new(
        mount_rs_sqlite::open_sqlite(&database).await.unwrap(),
    ))
    .await;
    assert_eq!(
        Loopback::new(mount_rs_sqlite::open_sqlite(&database).await.unwrap())
            .read_file("/alias")
            .await
            .unwrap(),
        [0, 255, 1, 127]
    );

    // This proves the object-store adapter, not live Cloudflare credentials.
    let store = Arc::new(object_store::memory::InMemory::new());
    mounted_round_trip(Arc::new(
        mount_rs_r2::open_object_store(store.clone(), "kernel/state")
            .await
            .unwrap(),
    ))
    .await;
    assert_eq!(
        Loopback::new(
            mount_rs_r2::open_object_store(store, "kernel/state")
                .await
                .unwrap()
        )
        .read_file("/alias")
        .await
        .unwrap(),
        [0, 255, 1, 127]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux FUSE and a real PGlite socket server"]
async fn mounted_pglite_persists_through_connection_reopen() {
    assert_eq!(
        std::env::var("MOUNT_RS_RUN_NATIVE_FUSE").as_deref(),
        Ok("1")
    );
    let url = std::env::var("PGLITE_DATABASE_URL").expect("PGLITE_DATABASE_URL required");
    let key = format!("native-pglite-{}", std::process::id());
    mounted_round_trip(Arc::new(
        mount_rs_pglite::connect_pglite_with_key(&url, &key)
            .await
            .unwrap(),
    ))
    .await;
    let reopened = Loopback::new(
        mount_rs_pglite::connect_pglite_with_key(&url, &key)
            .await
            .unwrap(),
    );
    assert_eq!(
        reopened.read_file("/alias").await.unwrap(),
        [0, 255, 1, 127]
    );
}
