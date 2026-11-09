//! Lifecycle and scope isolation against the actual TiDB fixture.
use mount_rs_core::storage::{BlockStore, MetadataStore};
use mount_rs_tidb::{TidbStorageOptions, TidbStorePool};
use mysql_async::{Pool, prelude::Queryable};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[tokio::test]
#[ignore = "requires isolated actual TiDB"]
async fn shared_pool_survives_volume_close_and_preserves_scope() {
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("TiDB fixture URL required");
    let cleanup = Pool::from_url(&url).expect("fixture URL");
    let mut connection = cleanup.get_conn().await.expect("fixture connection");
    let version: Option<String> = connection
        .query_first("SELECT tidb_version()")
        .await
        .expect("actual TiDB required");
    assert!(version.is_some());
    let scope = format!(
        "shared-pool-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let a_key = format!("{scope}-a");
    let b_key = format!("{scope}-b");
    let pool = TidbStorePool::new(&url, 1).expect("bounded pool");
    let a = pool
        .metadata(TidbStorageOptions::new(&a_key))
        .await
        .expect("volume a");
    let b = pool
        .metadata(TidbStorageOptions::new(&b_key))
        .await
        .expect("volume b");
    let a_lease = a
        .acquire_writer("a", Duration::from_secs(30))
        .await
        .expect("a lease");
    let b_lease = b
        .acquire_writer("b", Duration::from_secs(30))
        .await
        .expect("b lease");
    a.release_writer(&a_lease).await.expect("release a");
    a.close().await.expect("borrowed close");
    let b_lease = b
        .renew_writer(&b_lease, Duration::from_secs(30))
        .await
        .expect("b survives a close");
    let a_blocks = pool
        .blocks(TidbStorageOptions::new(&a_key))
        .await
        .expect("a blocks");
    let b_blocks = pool
        .blocks(TidbStorageOptions::new(&b_key))
        .await
        .expect("b blocks");
    let id = a_blocks.put(b"scoped bytes").await.expect("put a");
    a_blocks.close().await.expect("borrowed block close");
    assert!(
        b_blocks.get(&id).await.is_err(),
        "block must not cross volume scope"
    );
    assert_eq!(
        a_blocks.get(&id).await.expect("shared owner retains pool"),
        b"scoped bytes"
    );
    a_blocks.delete(&id).await.expect("delete fixture block");
    b.release_writer(&b_lease).await.expect("release b");
    pool.close().await.expect("explicit owner close");
    assert!(
        b.load().await.is_err(),
        "owner close affects all borrowed stores"
    );
    for key in [&a_key, &b_key] {
        connection
            .exec_drop(
                "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (key,),
            )
            .await
            .expect("clean unique fixture row");
    }
    drop(connection);
    cleanup.disconnect().await.expect("cleanup pool");
}

#[test]
fn zero_pool_capacity_is_rejected_before_connecting() {
    assert!(TidbStorePool::new("mysql://localhost/test", 0).is_err());
}
