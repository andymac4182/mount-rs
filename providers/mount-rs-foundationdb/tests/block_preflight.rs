#![cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]

use mount_rs_core::storage::BlockStore;
use mount_rs_foundationdb::{
    FoundationDbLimits, FoundationDbStorage, FoundationDbStorageOptions, shutdown_client_network,
};
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct OwnedClusterFile {
    directory: PathBuf,
    path: PathBuf,
}

impl OwnedClusterFile {
    fn new(port: u16) -> Self {
        let run_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time is after the Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "mount-rs-fdb-block-preflight-{}-{run_id:x}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("create exact run-owned fixture directory");
        let path = directory.join("fdb.cluster");
        let cluster_id = format!("{:08x}", run_id as u32);
        fs::write(&path, format!("mount_rs:{cluster_id}@127.0.0.1:{port}\n"))
            .expect("write syntactically valid disposable FoundationDB cluster file");
        Self { directory, path }
    }
}

impl Drop for OwnedClusterFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.directory);
    }
}

#[test]
fn concurrent_block_preflight_rejects_an_unavailable_shared_cluster() {
    // Keep this loopback port occupied by a listener that never speaks the
    // FoundationDB protocol. Database::from_path can still create a client
    // handle, but this fixture has no live shared backing to authorize.
    let unavailable = TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve a disposable loopback port without starting FoundationDB");
    let port = unavailable.local_addr().expect("read reserved port").port();
    let cluster = OwnedClusterFile::new(port);
    let cluster_path = cluster.path.clone();
    let fixture_dir = cluster.directory.clone();

    let limits = FoundationDbLimits {
        transaction_timeout: Duration::from_millis(750),
        transaction_retry_limit: 1,
        ..FoundationDbLimits::default()
    };
    let storage = FoundationDbStorage::connect(
        &cluster.path,
        FoundationDbStorageOptions::new(format!("block-preflight/{port}")).with_limits(limits),
    )
    .expect("create a native client handle for the unavailable cluster");
    let blocks = storage.blocks();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build bounded provider test runtime");
    let preflight = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), blocks.prepare_concurrent_mode()).await
    });

    // Drain the native network before asserting the expected RED/GREEN result.
    // A failed assertion then cannot leave a client thread running at process
    // exit, and the exact fixture is removed on either outcome.
    drop(runtime);
    drop(blocks);
    drop(storage);
    shutdown_client_network().expect("stop and join the native client network");
    drop(unavailable);
    drop(cluster);
    assert!(
        !cluster_path.exists(),
        "remove the exact run-owned cluster file"
    );
    assert!(!fixture_dir.exists(), "remove the exact fixture directory");

    match preflight {
        Ok(Err(_)) => println!("FOUNDATIONDB_BLOCK_PREFLIGHT_UNAVAILABLE_PASS"),
        Ok(Ok(())) => panic!("unavailable FoundationDB blocks must fail concurrent preflight"),
        Err(_) => panic!("unavailable FoundationDB block preflight must fail within five seconds"),
    }
}
