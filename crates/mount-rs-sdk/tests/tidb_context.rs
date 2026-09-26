//! Context lifetime across sibling drivers, including failed ChunkedFs opens.
use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext, StoreConfig};

#[tokio::test]
#[ignore = "requires actual TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_sibling_shutdown_and_failed_open_keep_context_alive() {
    let connection = std::env::var("MOUNT_RS_TIDB_URL").unwrap();
    let key = format!(
        "sdk-context-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let context = StorageContext::new(2).unwrap();
    let options = |volume: &str, blocks: &str| {
        let mut opts = SplitOptions::memory("context-driver", 4096).with_inode_updates(true);
        opts.metadata = StoreConfig::Tidb {
            connection: connection.clone(),
            volume_key: volume.into(),
            durable: true,
        };
        opts.blocks = StoreConfig::Tidb {
            connection: connection.clone(),
            volume_key: blocks.into(),
            durable: true,
        };
        opts
    };
    let first = Filesystem::split_with_context(options(&key, &key), &context)
        .await
        .unwrap();
    let other = format!("{key}-other");
    let sibling = Filesystem::split_with_context(options(&other, &other), &context)
        .await
        .unwrap();
    first
        .driver()
        .write_file("/first", b"first-volume")
        .await
        .unwrap();
    sibling
        .driver()
        .write_file("/sibling", b"sibling-volume")
        .await
        .unwrap();
    // Both providers have opened before the driver rejects the wrong block marker.
    assert!(
        Filesystem::split_with_context(options(&key, &other), &context)
            .await
            .is_err()
    );
    first.driver().unlink("/first").await.unwrap();
    first.shutdown().await.unwrap();
    sibling
        .driver()
        .write_file("/sibling", b"survives both shutdown and failed open")
        .await
        .unwrap();
    let handle = sibling.driver().open("/sibling", "r", 0).await.unwrap();
    let mut bytes = [0; 64];
    let count = handle.read(&mut bytes, Some(0)).await.unwrap();
    assert_eq!(&bytes[..count], b"survives both shutdown and failed open");
    handle.close().await.unwrap();
    sibling.driver().unlink("/sibling").await.unwrap();
    sibling.shutdown().await.unwrap();
    context.close().await.unwrap();
    assert!(
        Filesystem::split_with_context(options(&key, &key), &context)
            .await
            .is_err()
    );
}
