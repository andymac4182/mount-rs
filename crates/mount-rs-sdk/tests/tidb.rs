//! Real TiDB consumer evidence. The opt-in local harness owns service cleanup.
use mount_rs_sdk::{
    AtimePolicy, BlockCacheOptions, Filesystem, Loopback, SplitOptions, SplitRuntimeOptions,
    StoreConfig,
};
use mysql_async::{Pool, prelude::Queryable};
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::test]
#[ignore = "requires real TiDB via MOUNT_RS_TIDB_URL; optionally the isolated RustFS harness"]
async fn tidb_sdk_cache_and_reopen() {
    sdk_cache_and_reopen(false).await;
}

async fn sdk_cache_and_reopen(normalized: bool) {
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("MOUNT_RS_TIDB_URL required");
    let pool = Pool::from_url(&url).expect("valid TiDB URL required");
    let mut connection = pool.get_conn().await.expect("TiDB connection");
    let identity: Option<(String, String)> = connection
        .query_first("SELECT tidb_version(), VERSION()")
        .await
        .expect("real TiDB identity");
    assert!(identity.is_some_and(|(a, b)| format!("{a} {b}").to_lowercase().contains("tidb")));
    let directory = tempfile::tempdir().unwrap();
    let mut blocks = vec![
        ("memory", StoreConfig::Memory),
        (
            "sqlite-local",
            StoreConfig::Sqlite {
                path: directory.path().join("blocks.db"),
            },
        ),
    ];
    #[cfg(unix)]
    {
        let root = directory.path().join("raw-blocks");
        std::fs::create_dir(&root).unwrap();
        blocks.push(("local-files", StoreConfig::Local { path: root }));
    }
    if std::env::var("MOUNT_RS_SDK_RUSTFS").as_deref() == Ok("1") {
        // The RustFS harness owns endpoint identity, the unique prefix and deletion.
        assert!(
            std::path::Path::new(
                &std::env::var("RUSTFS_RUN_DIR").expect("isolated RustFS harness required")
            )
            .is_dir()
        );
        let config = mount_rs_r2::R2Config::from_env().expect("RustFS harness settings");
        assert!(
            config.endpoint.starts_with("http://127.0.0.1:")
                || config.endpoint.starts_with("http://localhost:")
        );
        blocks.push((
            "rustfs",
            StoreConfig::R2 {
                endpoint: config.endpoint,
                bucket: config.bucket,
                prefix: format!(
                    "{}/sdk-{normalized}",
                    std::env::var("RUSTFS_COMBO_PREFIX").unwrap()
                ),
                access_key_id: config.access_key_id,
                secret_access_key: config.secret_access_key,
                durable: false,
            },
        ));
    }
    for (name, blocks) in blocks {
        let key = format!(
            "sdk-audit-{}-{}-{name}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut options = SplitOptions::memory(format!("{key}-writer"), 4096);
        options.metadata = StoreConfig::Tidb {
            connection: url.clone(),
            volume_key: key.clone(),
            durable: true,
        };
        options.blocks = blocks;
        let runtime = SplitRuntimeOptions {
            read_parallelism: 1,
            lease_ttl: std::time::Duration::from_secs(240),
            atime_policy: AtimePolicy::NoAtime,
            read_lease_cache_window: std::time::Duration::from_secs(5),
            block_cache: Some(BlockCacheOptions {
                max_bytes: 65536,
                max_entries: 32,
            }),
        };
        let result = async {
            let cache = mount_rs_sdk::DiskCacheOptions {
                root: directory.path().to_owned(),
                scope: key.clone(),
                max_bytes: 65536,
                max_entries: 32,
            };
            let persistent_probe = matches!(name, "local-files" | "rustfs");
            let fs = if persistent_probe {
                Filesystem::split_with_disk_cache(options.clone(), runtime, cache.clone()).await?
            } else {
                Filesystem::split_with_runtime(options.clone(), runtime).await?
            };
            let mut contender = options.clone();
            contender.owner = format!("{key}-other-mount");
            let conflict = Filesystem::split_with_runtime(contender, runtime).await;
            assert_eq!(
                conflict
                    .err()
                    .expect("current exclusive mount boundary")
                    .code,
                mount_rs_sdk::ErrorCode::Eagain
            );
            let view = Loopback::from_arc(fs.driver());
            let mut payload = vec![0xa5; 8192];
            payload[4096..].fill(0x5a);
            view.write_file("/sdk", &payload).await?;
            assert_eq!(view.read_file("/sdk").await?, payload);
            let stats = fs.block_cache_stats().await.unwrap();
            assert!(stats.hits >= 2);
            assert_eq!(stats.misses, 0);
            if name == "memory" {
                assert!(!fs.driver().capabilities().durable_writes);
            }
            fs.driver().chmod("/sdk", 0o640).await?;
            let encoded: Option<String> = connection
                .exec_first(
                    "SELECT namespace FROM mount_rs_tidb_metadata WHERE volume_key=?",
                    (&key,),
                )
                .await
                .expect("read stored format");
            assert_eq!(
                encoded.as_deref() == Some("{\"mount_rs_normalized_records\":1}"),
                normalized
            );
            if persistent_probe {
                assert!(fs.disk_cache_stats().await?.unwrap().resident_entries > 0);
            }
            fs.shutdown().await?;
            drop(view);
            drop(fs);
            if name == "memory" {
                let rejected = Filesystem::split_with_runtime(options, runtime).await;
                assert_eq!(
                    rejected
                        .err()
                        .expect("lost memory contents must reject reopen")
                        .code,
                    mount_rs_sdk::ErrorCode::Estale
                );
                let released: Option<i64> = connection
                    .exec_first(
                        "SELECT owner IS NULL FROM mount_rs_tidb_metadata WHERE volume_key=?",
                        (&key,),
                    )
                    .await
                    .expect("read lease after rejected reopen");
                assert_eq!(released, Some(1), "failed reopen retained the writer lease");
            } else {
                if persistent_probe {
                    let mut incorrect = options.clone();
                    match &mut incorrect.blocks {
                        StoreConfig::Local { path } => {
                            *path = directory.path().join("wrong-root");
                            std::fs::create_dir_all(path).unwrap();
                        }
                        StoreConfig::R2 { prefix, .. } => prefix.push_str("/wrong-prefix"),
                        _ => unreachable!(),
                    }
                    let failed =
                        Filesystem::split_with_disk_cache(incorrect, runtime, cache.clone()).await;
                    assert_eq!(
                        failed
                            .err()
                            .expect("wrong block location must reject startup")
                            .code,
                        mount_rs_sdk::ErrorCode::Estale
                    );
                }
                if persistent_probe {
                    let mut partial = options.clone();
                    match &mut partial.blocks {
                        StoreConfig::Local { path } => {
                            use mount_rs_core::storage::BlockStore;
                            *path = directory.path().join("partial-blocks");
                            std::fs::create_dir(&*path).unwrap();
                            mount_rs_host::LocalBlockStore::open(path)?
                                .put(&payload[..4096])
                                .await?;
                        }
                        StoreConfig::R2 { prefix, .. } => {
                            let objects = mount_rs_r2::R2Config::from_env()?.build_store()?;
                            let original = prefix.as_str().into();
                            let listing =
                                objects.list_with_delimiter(Some(&original)).await.unwrap();
                            assert_eq!(listing.objects.len(), 2);
                            prefix.push_str("-partial");
                            let mut copied = 0;
                            for object in listing.objects {
                                let bytes = objects
                                    .get(&object.location)
                                    .await
                                    .unwrap()
                                    .bytes()
                                    .await
                                    .unwrap();
                                if bytes.as_ref() == &payload[..4096] {
                                    let id = object.location.filename().unwrap();
                                    let destination = format!("{prefix}/{id}").into();
                                    objects.copy(&object.location, &destination).await.unwrap();
                                    copied += 1;
                                }
                            }
                            assert_eq!(copied, 1);
                        }
                        _ => unreachable!(),
                    }
                    let sampled =
                        Filesystem::split_with_disk_cache(partial.clone(), runtime, cache.clone())
                            .await?;
                    let view = Loopback::from_arc(sampled.driver());
                    assert_eq!(view.read_file("/sdk").await?, payload);
                    assert!(sampled.disk_cache_stats().await?.unwrap().hits >= 2);
                    sampled.shutdown().await?;
                    drop(view);
                    drop(sampled);
                    let error = Filesystem::split_with_verified_blocks(
                        partial,
                        runtime,
                        Some(cache.clone()),
                        Default::default(),
                    )
                    .await
                    .err()
                    .expect("missing second block must reject recovery open");
                    assert_eq!(error.code, mount_rs_sdk::ErrorCode::Estale);
                }
                let reopened = Filesystem::split_with_verified_blocks(
                    options,
                    runtime,
                    None,
                    Default::default(),
                )
                .await?;
                let view = Loopback::from_arc(reopened.driver());
                assert_eq!(view.read_file("/sdk").await?, payload);
                assert!(reopened.block_cache_stats().await.unwrap().misses > 0);
                assert_eq!(view.read_file("/sdk").await?, payload);
                assert!(reopened.block_cache_stats().await.unwrap().hits >= 2);
                reopened.shutdown().await?;
            }
            Ok::<(), mount_rs_core::FsError>(())
        }
        .await;
        if normalized {
            connection
                .exec_drop(
                    "DELETE FROM mount_rs_tidb_metadata_records WHERE volume_key=?",
                    (&key,),
                )
                .await
                .expect("remove scoped normalized records");
        }
        connection
            .exec_drop(
                "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&key,),
            )
            .await
            .expect("remove only scoped SDK metadata");
        result.expect("TiDB SDK cache/reopen contract");
        println!("TIDB_SDK_CACHE_PASS blocks={name} normalized={normalized}");
    }
    drop(connection);
    pool.disconnect().await.unwrap();
}

#[tokio::test]
#[ignore = "requires real TiDB via MOUNT_RS_TIDB_URL"]
async fn shared_tidb_pool_survives_filesystem_shutdown() {
    use mount_rs_sdk::TidbStorePool;
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("fixture URL");
    let cleanup = Pool::from_url(&url).expect("fixture URL");
    let mut connection = cleanup.get_conn().await.expect("fixture connection");
    let _: Option<String> = connection
        .query_first("SELECT tidb_version()")
        .await
        .expect("actual TiDB required");
    let prefix = format!(
        "sdk-shared-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let pool = TidbStorePool::new(&url, 2).expect("shared pool");
    let options = |suffix: &str| {
        let mut options = SplitOptions::memory(format!("owner-{suffix}"), 4096);
        let store = StoreConfig::TidbShared {
            pool: pool.clone(),
            volume_key: format!("{prefix}-{suffix}"),
            durable: true,
        };
        options.metadata = store.clone();
        options.blocks = store;
        options
    };
    let a = Filesystem::split(options("a")).await.expect("open a");
    let b = Filesystem::split(options("b")).await.expect("open b");
    let file = a
        .driver()
        .open("/saved", "w", 0o600)
        .await
        .expect("create a");
    file.write(b"shared-pool payload", Some(0))
        .await
        .expect("write a");
    file.close().await.expect("close file");
    a.shutdown().await.expect("shutdown a");
    assert!(
        b.driver().stat("/saved").await.is_err(),
        "namespace isolation"
    );
    b.driver()
        .open("/alive", "w", 0o600)
        .await
        .expect("b remains writable")
        .close()
        .await
        .expect("close b file");
    let reopened = Filesystem::split(options("a")).await.expect("reopen a");
    let file = reopened
        .driver()
        .open("/saved", "r", 0)
        .await
        .expect("open persisted file");
    let mut bytes = vec![0; 19];
    assert_eq!(
        file.read(&mut bytes, Some(0))
            .await
            .expect("read persisted bytes"),
        19
    );
    assert_eq!(bytes, b"shared-pool payload");
    file.close().await.expect("close read");
    reopened.shutdown().await.expect("shutdown reopened a");
    b.shutdown().await.expect("shutdown b");
    pool.close().await.expect("owner closes pool last");
    for suffix in ["a", "b"] {
        let key = format!("{prefix}-{suffix}");
        for table in ["mount_rs_tidb_metadata", "mount_rs_tidb_blocks"] {
            connection
                .exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (&key,))
                .await
                .expect("clean unique fixture scope");
        }
    }
    drop(connection);
    cleanup.disconnect().await.expect("cleanup pool");
}
