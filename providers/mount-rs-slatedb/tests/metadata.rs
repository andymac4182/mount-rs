use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::driver::FsDriver;
use mount_rs_core::storage::MetadataStore;
use mount_rs_memory::MemoryBlockStore;
use mount_rs_slatedb::SlateDbMetadataStore;
use slatedb::object_store::memory::InMemory;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn split_filesystem_reopens_with_slatedb_metadata() {
    let objects = Arc::new(InMemory::new());
    let blocks = MemoryBlockStore::new();
    let metadata = SlateDbMetadataStore::open("metadata", objects.clone())
        .await
        .unwrap();
    let fs = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("first", 4096).unwrap(),
    )
    .await
    .unwrap();
    fs.write_file("/hello", b"world").await.unwrap();
    fs.shutdown().await.unwrap();
    metadata.close().await.unwrap();

    let metadata = SlateDbMetadataStore::open("metadata", objects)
        .await
        .unwrap();
    assert!(metadata.load().await.unwrap().revision > 0);
    let fs = ChunkedFs::open(
        metadata.clone(),
        blocks,
        ChunkedOptions::fixed("second", 4096).unwrap(),
    )
    .await
    .unwrap();
    let handle = fs.open("/hello", "r", 0).await.unwrap();
    let mut bytes = [0; 5];
    assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 5);
    assert_eq!(&bytes, b"world");
    handle.close().await.unwrap();
    fs.shutdown().await.unwrap();
    metadata.close().await.unwrap();
}

#[tokio::test]
async fn released_lease_cannot_be_reused_and_fence_survives_reopen() {
    let objects = Arc::new(InMemory::new());
    let metadata = SlateDbMetadataStore::open("leases", objects.clone())
        .await
        .unwrap();
    let first = metadata
        .acquire_writer("first", Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        metadata
            .acquire_writer("second", Duration::from_secs(5))
            .await
            .is_err()
    );
    metadata.release_writer(&first).await.unwrap();
    assert!(
        metadata
            .renew_writer(&first, Duration::from_secs(5))
            .await
            .is_err()
    );
    metadata.close().await.unwrap();

    let reopened = SlateDbMetadataStore::open("leases", objects).await.unwrap();
    let second = reopened
        .acquire_writer("second", Duration::from_secs(5))
        .await
        .unwrap();
    assert!(second.fence > first.fence);
    reopened.release_writer(&second).await.unwrap();
    reopened.close().await.unwrap();
}
