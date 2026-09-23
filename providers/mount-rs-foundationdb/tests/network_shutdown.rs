#![cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]

use mount_rs_core::ErrorCode;
use mount_rs_core::storage::BlockStore;
use mount_rs_foundationdb::{
    FoundationDbStorage, FoundationDbStorageOptions, shutdown_client_network,
};
use std::env;
use std::time::Duration;

#[test]
fn terminal_shutdown_drains_the_native_network_and_rejects_new_connections() {
    let Some(cluster_file) = env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE")
        .ok()
        .filter(|path| !path.trim().is_empty())
    else {
        eprintln!("SKIP network shutdown: set MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE");
        return;
    };
    let prefix = env::var("MOUNT_RS_FOUNDATIONDB_TEST_PREFIX")
        .expect("set a run-owned MOUNT_RS_FOUNDATIONDB_TEST_PREFIX for the live cluster test");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build disposable provider test runtime");
    let storage = FoundationDbStorage::connect(
        &cluster_file,
        FoundationDbStorageOptions::new(format!("{prefix}/network-shutdown"))
            .with_persisted_lease_oracle(),
    )
    .expect("connect live FoundationDB cluster");
    let blocks = storage.blocks();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let id = blocks.put(b"native client lifecycle").await?;
            assert_eq!(blocks.get(&id).await?, b"native client lifecycle");
            blocks.delete(&id).await
        })
        .await
        .expect("bounded FoundationDB transaction path")
        .expect("live FoundationDB block transaction path");
    });
    drop(runtime);

    let still_open = shutdown_client_network()
        .expect_err("a live storage handle must keep the native network running");
    assert_eq!(still_open.code, ErrorCode::Ebusy);
    drop(storage);
    let still_open = shutdown_client_network()
        .expect_err("an independently held block store must keep the network running");
    assert_eq!(still_open.code, ErrorCode::Ebusy);
    drop(blocks);

    shutdown_client_network().expect("stop and join the native client network");
    shutdown_client_network().expect("terminal stop is idempotent");
    let reconnect = FoundationDbStorage::connect(
        &cluster_file,
        FoundationDbStorageOptions::new(format!("{prefix}/after-network-shutdown")),
    );
    let reconnect_error = match reconnect {
        Ok(_) => panic!("FoundationDB must not restart a stopped native client network"),
        Err(error) => error,
    };
    assert_eq!(reconnect_error.code, ErrorCode::Eio);
    println!("FOUNDATIONDB_NATIVE_NETWORK_SHUTDOWN_PASS");
}
