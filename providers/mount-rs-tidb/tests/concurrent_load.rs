//! Real TiDB, independently opened filesystem coordinators under CAS load.

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::{Loopback, storage::MetadataStore};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
use mysql_async::{Pool, prelude::Queryable};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Barrier;
use tokio::task::JoinSet;

type TidbFs = ChunkedFs<TidbMetadataStore, TidbBlockStore>;
const WRITERS: usize = 4;
const LIFECYCLES: usize = 40;

async fn join_writers(mut tasks: JoinSet<()>) {
    let mut failures = Vec::new();
    let completed = tokio::time::timeout(Duration::from_secs(180), async {
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
    })
    .await;
    if completed.is_err() {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        panic!("TiDB writer packet exceeded its 180 second deadline");
    }
    assert!(failures.is_empty(), "TiDB writers failed: {failures:?}");
}

async fn open(url: &str, key: &str, writer: usize) -> (TidbFs, TidbMetadataStore, TidbBlockStore) {
    let options = TidbStorageOptions::new(key).with_durable(true);
    let metadata = TidbMetadataStore::connect_with_options(url, options.clone())
        .await
        .unwrap();
    let blocks = TidbBlockStore::connect_with_options(url, options)
        .await
        .unwrap();
    let driver = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed(format!("tidb-load-{writer}"), 4096)
            .unwrap()
            .with_concurrent_writes(true),
    )
    .await
    .unwrap();
    (driver, metadata, blocks)
}

fn payload(writer: usize, index: usize) -> Vec<u8> {
    format!("tidb-writer={writer};index={index};")
        .repeat(64)
        .into_bytes()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an actual disposable TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_four_coordinator_load_preserves_all_acknowledged_mutations() {
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("actual TiDB endpoint required");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = format!("tidb-concurrent-load-{}-{timestamp}", std::process::id());
    let pool = Pool::from_url(&url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    let (identity, version): (String, String) = connection
        .query_first("SELECT tidb_version(), VERSION()")
        .await
        .unwrap()
        .unwrap();
    assert!(
        version.to_ascii_lowercase().contains("tidb"),
        "actual TiDB required: VERSION()={version:?}"
    );
    assert!(
        identity.to_ascii_lowercase().contains("release version:"),
        "TiDB release identity required: tidb_version()={identity:?}"
    );
    println!("TIDB_CONCURRENT_LOAD_IDENTITY tidb_version={identity} version={version}");
    drop(connection);

    let mut drivers = Vec::new();
    let mut providers = Vec::new();
    for writer in 0..WRITERS {
        let (driver, metadata, blocks) = open(&url, &key, writer).await;
        drivers.push(driver);
        providers.push((metadata, blocks));
    }
    let started = Instant::now();
    let barrier = Arc::new(Barrier::new(WRITERS));
    let mut tasks = JoinSet::new();
    for (writer, driver) in drivers.iter().cloned().enumerate() {
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            let view = Loopback::new(driver);
            barrier.wait().await;
            for index in 0..LIFECYCLES {
                let original = format!("/writer-{writer}-{index}");
                let renamed = format!("/renamed-{writer}-{index}");
                view.write_file(&original, &payload(writer, index))
                    .await
                    .unwrap_or_else(|error| panic!("write {original}: {error:?}"));
                view.rename(&original, &renamed).await.unwrap();
                if index % 2 == 0 {
                    view.unlink(&renamed).await.unwrap();
                }
            }
        });
    }
    join_writers(tasks).await;
    Loopback::new(drivers[0].clone())
        .write_file("/shared", &vec![0; WRITERS * 4096])
        .await
        .unwrap();
    let barrier = Arc::new(Barrier::new(WRITERS));
    let mut tasks = JoinSet::new();
    for (writer, driver) in drivers.iter().cloned().enumerate() {
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            let file = Loopback::new(driver)
                .open("/shared", "r+", 0)
                .await
                .unwrap();
            barrier.wait().await;
            assert_eq!(
                file.write(
                    &vec![b'A' + writer as u8; 4096],
                    Some((writer * 4096) as u64)
                )
                .await
                .unwrap(),
                4096
            );
            file.close().await.unwrap();
        });
    }
    join_writers(tasks).await;
    for driver in drivers {
        driver.shutdown().await.unwrap();
    }
    for (metadata, blocks) in providers {
        metadata.close().await.unwrap();
        blocks.close().await.unwrap();
    }

    let (fresh, metadata, blocks) = open(&url, &key, WRITERS).await;
    let view = Loopback::new(fresh.clone());
    for writer in 0..WRITERS {
        for index in 0..LIFECYCLES {
            let renamed = format!("/renamed-{writer}-{index}");
            if index % 2 == 0 {
                assert!(
                    view.read_file(&renamed)
                        .await
                        .unwrap_err()
                        .is(mount_rs_core::ErrorCode::Enoent)
                );
            } else {
                assert_eq!(
                    view.read_file(&renamed).await.unwrap(),
                    payload(writer, index)
                );
            }
            assert!(
                view.read_file(&format!("/writer-{writer}-{index}"))
                    .await
                    .unwrap_err()
                    .is(mount_rs_core::ErrorCode::Enoent)
            );
        }
    }
    let expected: Vec<u8> = (0..WRITERS)
        .flat_map(|writer| vec![b'A' + writer as u8; 4096])
        .collect();
    assert_eq!(view.read_file("/shared").await.unwrap(), expected);
    let revision = metadata.load().await.unwrap().revision;
    let mut connection = pool.get_conn().await.unwrap();
    let namespace_bytes: u64 = connection
        .exec_first(
            "SELECT OCTET_LENGTH(namespace) FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (&key,),
        )
        .await
        .unwrap()
        .unwrap();
    let block_rows: u64 = connection
        .exec_first(
            "SELECT COUNT(*) FROM mount_rs_tidb_blocks WHERE volume_key=?",
            (&key,),
        )
        .await
        .unwrap()
        .unwrap();
    drop(connection);
    fresh.shutdown().await.unwrap();
    metadata.close().await.unwrap();
    blocks.close().await.unwrap();
    pool.disconnect().await.unwrap();
    println!(
        "TIDB_CONCURRENT_LOAD_PASS writers={WRITERS} lifecycles_per_writer={LIFECYCLES} acknowledged_mutations=405 fresh_surviving_files=81 revision={revision} namespace_bytes={namespace_bytes} block_rows={block_rows} elapsed_ms={}",
        started.elapsed().as_millis()
    );
}
