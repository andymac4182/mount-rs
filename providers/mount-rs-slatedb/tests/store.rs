use mount_rs_core::Loopback;
use mount_rs_kv::KeyValueStore;
use mount_rs_kv::{UnstorageOptions, create_unstorage_driver};
use mount_rs_slatedb::SlateDbStore;
use slatedb::object_store::memory::InMemory;
use std::sync::Arc;

#[tokio::test]
async fn durable_values_survive_reopen_and_bounded_listing() {
    let objects = Arc::new(InMemory::new());
    let store = SlateDbStore::open("mount-rs-test", objects.clone())
        .await
        .unwrap();
    store.set_item_raw("docs/a", b"one".to_vec()).await.unwrap();
    store.set_item_raw("docs/b", b"two".to_vec()).await.unwrap();
    store
        .set_item_raw("other", b"three".to_vec())
        .await
        .unwrap();
    assert_eq!(
        store
            .get_keys_bounded("docs/", 1)
            .await
            .unwrap()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(store.get_keys("docs/").await.unwrap(), ["docs/a", "docs/b"]);
    store.close().await.unwrap();

    let reopened = SlateDbStore::open("mount-rs-test", objects).await.unwrap();
    assert_eq!(
        reopened.get_item_raw("docs/a").await.unwrap(),
        Some(b"one".to_vec())
    );
    reopened.remove_item("docs/a").await.unwrap();
    assert!(!reopened.has_item("docs/a").await.unwrap());
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn key_value_filesystem_reads_values_after_reopen() {
    let objects = Arc::new(InMemory::new());
    let store = SlateDbStore::open("filesystem", objects.clone())
        .await
        .unwrap();
    let fs = Loopback::new(create_unstorage_driver(
        store.clone(),
        UnstorageOptions::default(),
    ));
    fs.write_file("/note", b"persisted").await.unwrap();
    drop(fs);
    store.close().await.unwrap();

    let store = SlateDbStore::open("filesystem", objects).await.unwrap();
    let fs = Loopback::new(create_unstorage_driver(
        store.clone(),
        UnstorageOptions::default(),
    ));
    assert_eq!(fs.read_file("/note").await.unwrap(), b"persisted");
    drop(fs);
    store.close().await.unwrap();
}
