//! Correctness oracle for the production metadata/blob composition.
use mount_rs_rustfs::RustFsConfig;
use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext, StoreConfig};
use mysql_async::{Pool, prelude::Queryable};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit owned fixture setting {name} required"))
}

fn payload(seed: usize, len: usize) -> Vec<u8> {
    (0..len)
        .map(|n| ((n * 37 + seed * 19 + n / 97) % 251) as u8)
        .collect()
}

async fn read_exact_file(fs: &Filesystem, path: &str, expected: &[u8]) {
    let handle = fs.driver().open(path, "r", 0).await.unwrap();
    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let count = handle
            .read(&mut actual[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0, "short acknowledged file");
        offset += count;
    }
    assert_eq!(actual, expected);
    assert_eq!(
        handle
            .read(&mut [0], Some(expected.len() as u64))
            .await
            .unwrap(),
        0
    );
    handle.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires explicit owned TiDB and RustFS fixture settings"]
async fn actual_tidb_rustfs_indexed_peer_writes_patterns_unlink_and_fresh_reopen() {
    let connection = required("MOUNT_RS_TIDB_URL");
    let config = RustFsConfig {
        endpoint: required("MOUNT_RS_RUSTFS_ENDPOINT"),
        bucket: required("MOUNT_RS_RUSTFS_BUCKET"),
        region: required("MOUNT_RS_RUSTFS_REGION"),
        access_key_id: required("MOUNT_RS_RUSTFS_ACCESS_KEY_ID"),
        secret_access_key: required("MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY"),
    };
    let key = format!(
        "indexed-rustfs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let prefix = format!("{key}/blocks");
    println!("INDEXED_OWNED_SCOPE metadata={key} blobs={prefix}");
    assert!(config.observe_owned_prefix_absence(&key).await.unwrap());
    let options = || {
        let mut options =
            SplitOptions::memory("indexed-rustfs-oracle", 4096).with_compact_inode_updates(true);
        options.metadata = StoreConfig::Tidb {
            connection: connection.clone(),
            volume_key: key.clone(),
            durable: true,
        };
        options.blocks = StoreConfig::RustFs {
            endpoint: config.endpoint.clone(),
            bucket: config.bucket.clone(),
            region: config.region.clone(),
            access_key_id: config.access_key_id.clone(),
            secret_access_key: config.secret_access_key.clone(),
            prefix: prefix.clone(),
            durable: true,
        };
        options
    };
    let first_context = StorageContext::new(4).unwrap();
    let first = Filesystem::split_with_context(options(), &first_context)
        .await
        .unwrap();
    let original = payload(1, 12_345);
    first.driver().write_file("/a", &original).await.unwrap();
    first
        .driver()
        .write_file("/b", &payload(2, 8192))
        .await
        .unwrap();
    let second_context = StorageContext::new(4).unwrap();
    let second = Filesystem::split_with_context(options(), &second_context)
        .await
        .unwrap();
    read_exact_file(&second, "/a", &original).await;

    let a = first.driver().open("/a", "r+", 0).await.unwrap();
    let b = second.driver().open("/b", "r+", 0).await.unwrap();
    let patch_a = payload(9, 4096);
    let patch_b = payload(10, 4096);
    let (written_a, written_b) =
        tokio::join!(a.write(&patch_a, Some(2048)), b.write(&patch_b, Some(0)));
    assert_eq!(written_a.unwrap(), patch_a.len());
    assert_eq!(written_b.unwrap(), patch_b.len());
    a.close().await.unwrap();
    b.close().await.unwrap();
    let mut expected_a = original;
    expected_a[2048..6144].copy_from_slice(&patch_a);
    let mut expected_b = payload(2, 8192);
    expected_b[..4096].copy_from_slice(&patch_b);
    read_exact_file(&second, "/a", &expected_a).await;
    read_exact_file(&first, "/b", &expected_b).await;

    let append = second.driver().open("/a", "a", 0).await.unwrap();
    let tail = payload(11, 173);
    assert_eq!(append.write(&tail, None).await.unwrap(), tail.len());
    append.close().await.unwrap();
    expected_a.extend_from_slice(&tail);
    first.driver().rename("/a", "/renamed").await.unwrap();
    read_exact_file(&second, "/renamed", &expected_a).await;
    second.driver().truncate("/renamed", 4097).await.unwrap();
    expected_a.truncate(4097);
    first.driver().truncate("/renamed", 8193).await.unwrap();
    expected_a.resize(8193, 0);
    read_exact_file(&second, "/renamed", &expected_a).await;
    let orphan = second.driver().open("/b", "r+", 0).await.unwrap();
    first.driver().unlink("/b").await.unwrap();
    let mut orphan_bytes = vec![0; expected_b.len()];
    assert_eq!(
        orphan.read(&mut orphan_bytes, Some(0)).await.unwrap(),
        expected_b.len()
    );
    assert_eq!(orphan_bytes, expected_b);
    let orphan_patch = payload(12, 97);
    assert_eq!(
        orphan.write(&orphan_patch, Some(33)).await.unwrap(),
        orphan_patch.len()
    );
    expected_b[33..130].copy_from_slice(&orphan_patch);
    assert_eq!(
        orphan.read(&mut orphan_bytes, Some(0)).await.unwrap(),
        expected_b.len()
    );
    assert_eq!(orphan_bytes, expected_b);
    orphan.close().await.unwrap();
    assert!(first.driver().open("/b", "r", 0).await.is_err());
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
    first_context.close().await.unwrap();
    second_context.close().await.unwrap();
    drop((first, second, first_context, second_context));

    let fresh_context = StorageContext::new(2).unwrap();
    let fresh = Filesystem::split_with_context(options(), &fresh_context)
        .await
        .unwrap();
    read_exact_file(&fresh, "/renamed", &expected_a).await;
    assert!(fresh.driver().open("/b", "r", 0).await.is_err());
    fresh.shutdown().await.unwrap();
    fresh_context.close().await.unwrap();
    drop((fresh, fresh_context));

    // Both data-store cleanups are restricted to this run's exact fresh scope.
    let pool = Pool::from_url(&connection).unwrap();
    let mut sql = pool.get_conn().await.unwrap();
    for table in [
        "mount_rs_tidb_compact_dentries",
        "mount_rs_tidb_compact_members",
        "mount_rs_tidb_compact_guards",
        "mount_rs_tidb_inodes",
        "mount_rs_tidb_metadata",
        "mount_rs_tidb_blocks",
        "mount_rs_tidb_block_authority",
    ] {
        sql.exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (&key,))
            .await
            .unwrap();
    }
    drop(sql);
    pool.disconnect().await.unwrap();
    let store = config.build_store().unwrap();
    let mut prefixes = vec![format!("{key}/").into()];
    let mut objects = Vec::new();
    while let Some(prefix) = prefixes.pop() {
        let listed = store.list_with_delimiter(Some(&prefix)).await.unwrap();
        prefixes.extend(listed.common_prefixes);
        objects.extend(listed.objects.into_iter().map(|object| object.location));
        assert!(prefixes.len() <= 32 && objects.len() <= 128);
    }
    for location in &objects {
        assert!(location.as_ref().starts_with(&format!("{key}/")));
    }
    for location in objects {
        store.delete(&location).await.unwrap();
    }
    assert!(config.observe_owned_prefix_absence(&key).await.unwrap());
    println!(
        "INDEXED_TIDB_RUSTFS_PASS two_contexts=2 concurrent_disjoint_writes=2 full_bytes=verified fresh_reopen=1 cleanup=complete"
    );
}
