//! Opt-in real Linux FUSE qualification of explicit exclusive writeback.
use mount_rs_chunked::{ChunkedFs, ChunkedOptions, OwnershipMode};
use mount_rs_core::FsDriver;
use mount_rs_fuse::mount::{MountOptions, mount};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::{sync::Arc, time::Duration};

async fn sqlite_processes(driver: Arc<dyn FsDriver>, verify_only: bool) {
    // Preserve a possibly mounted directory when teardown fails.
    let directory = tempfile::tempdir().unwrap().keep();
    let mounted = mount(
        driver,
        &directory,
        MountOptions {
            default_permissions: false,
            ..Default::default()
        },
    )
    .await
    .expect("mount exclusive writeback backend");
    let mut command = tokio::process::Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/sqlite_exclusive_qualification.py"
        ))
        .arg(&directory)
        .kill_on_drop(true);
    if verify_only {
        command.arg("--verify-only");
    }
    let result = tokio::time::timeout(Duration::from_secs(90), command.status()).await;
    #[cfg(target_os = "linux")]
    let cleanup = tokio::time::timeout(Duration::from_secs(15), mounted.unmount()).await;
    #[cfg(not(target_os = "linux"))]
    let cleanup: Result<Result<(), mount_rs_fuse::mount::MountError>, ()> = {
        let _ = mounted;
        Err(())
    };
    if cleanup.as_ref().is_ok_and(Result::is_ok) {
        std::fs::remove_dir(&directory).unwrap();
    }
    assert!(
        result
            .as_ref()
            .is_ok_and(|status| status.as_ref().is_ok_and(|s| s.success())),
        "SQLite process qualification failed: {result:?}"
    );
    assert!(
        cleanup.as_ref().is_ok_and(Result::is_ok),
        "FUSE teardown failed: {cleanup:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit Linux /dev/fuse qualification"]
async fn exclusive_writeback_sqlite_processes_and_durable_reopen() {
    assert_eq!(
        std::env::var("MOUNT_RS_RUN_NATIVE_FUSE").as_deref(),
        Ok("1")
    );
    let directory = tempfile::tempdir().unwrap();
    let metadata = directory.path().join("metadata.sqlite");
    let blocks = directory.path().join("blocks.sqlite");
    for reopen in [false, true] {
        let options = ChunkedOptions::fixed(
            if reopen {
                "exclusive-native-reopen"
            } else {
                "exclusive-native"
            },
            4096,
        )
        .unwrap()
        .with_ownership_mode(OwnershipMode::Exclusive)
        .with_writeback(true);
        let fs = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata).unwrap(),
            SqliteBlockStore::open(&blocks).unwrap(),
            options,
        )
        .await
        .unwrap();
        sqlite_processes(Arc::new(fs.clone()), reopen).await;
        fs.shutdown()
            .await
            .expect("durable exclusive writeback shutdown");
    }
}
