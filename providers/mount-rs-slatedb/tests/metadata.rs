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

/// The store's state is held across the persisted write. Here a publish
/// is inside that write when its lease runs out and another writer asks
/// for the lease. The acquire must wait for the publish and build on what
/// it wrote; built on the state from before, its record would replace the
/// publish's, and the revision the publish reported would be gone after a
/// reopen.
///
/// The test runs on one thread and does not yield between starting the
/// publish and starting the acquire, so the publish is always exactly at
/// its first wait inside the write. The lease expires by the real clock.
#[tokio::test]
async fn an_acquire_waits_for_a_publish_whose_write_is_in_flight() {
    // Any valid namespace will do.
    let namespace = {
        let seed = mount_rs_memory::MemoryMetadataStore::new();
        let fs = ChunkedFs::open(
            seed.clone(),
            MemoryBlockStore::new(),
            ChunkedOptions::fixed("seed", 4096).unwrap(),
        )
        .await
        .unwrap();
        fs.write_file("/hello", b"world").await.unwrap();
        fs.shutdown().await.unwrap();
        seed.load().await.unwrap().namespace.unwrap()
    };

    let objects = Arc::new(InMemory::new());
    let metadata = SlateDbMetadataStore::open("overlap", objects.clone())
        .await
        .unwrap();
    let first = metadata
        .acquire_writer("first", Duration::from_secs(60))
        .await
        .unwrap();
    let revision = metadata.load().await.unwrap().revision;
    // Shorten the lease only now, so the publish starts well inside it.
    let lease_ttl = Duration::from_millis(400);
    let first = metadata.renew_writer(&first, lease_ttl).await.unwrap();

    let publishing = tokio::spawn({
        let (metadata, first) = (metadata.clone(), first.clone());
        async move { metadata.publish(revision, &first, namespace).await }
    });
    // One turn: the publish checks its lease and enters the write.
    tokio::task::yield_now().await;
    assert!(!publishing.is_finished(), "the write did not wait");
    // The lease runs out with the write still in flight. Blocking, so the
    // publish makes no progress meanwhile.
    std::thread::sleep(lease_ttl + Duration::from_millis(50));
    let second = metadata
        .acquire_writer("second", Duration::from_secs(60))
        .await
        .unwrap();
    let published = publishing.await.unwrap().unwrap();
    assert_eq!(published, revision + 1);
    assert!(second.fence > first.fence);
    assert_eq!(metadata.load().await.unwrap().revision, published);
    metadata.close().await.unwrap();

    let reopened = SlateDbMetadataStore::open("overlap", objects)
        .await
        .unwrap();
    let loaded = reopened.load().await.unwrap();
    assert_eq!(
        loaded.revision, published,
        "a publish that reported success is gone after reopen"
    );
    assert!(loaded.namespace.is_some());
    let third = reopened
        .acquire_writer("third", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(
        third.fence > second.fence,
        "fence {} was issued twice",
        third.fence
    );
    reopened.close().await.unwrap();
}
