//! Bounded engine-level write cost of the covering revision index on actual TiDB.
//! The disposable test cluster is required because this test temporarily drops the index.

use mount_rs_tidb::{TidbMetadataStore, TidbStorageOptions};
use mysql_async::{Pool, prelude::Queryable};
use serde_json::json;
use std::{
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const UPDATES: usize = 200;
const INDEX: &str = "idx_mount_rs_volume_revision";

fn observe(action: &str, stage: &str, successes: usize) {
    let observer = std::env::var("MOUNT_RS_DATASTORE_STAGE_OBSERVER")
        .expect("actual TiDB stage observer required");
    let status = Command::new(observer)
        .args([action, "write", "1", stage, &successes.to_string(), "0"])
        .status()
        .expect("run TiDB stage observer");
    assert!(status.success(), "observer failed for {stage} {action}");
}

#[tokio::test]
#[ignore = "requires isolated disposable actual TiDB and observer"]
async fn direct_tidb_revision_index_write_cost() {
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("actual TiDB URL required");
    let pool = Pool::from_url(&url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    let version: String = connection
        .query_first("SELECT VERSION()")
        .await
        .unwrap()
        .unwrap();
    assert!(version.to_ascii_lowercase().contains("tidb"));
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = format!("tidb-index-write-cost-{}-{stamp}", std::process::id());
    let metadata = TidbMetadataStore::connect_with_options(&url, TidbStorageOptions::new(&key))
        .await
        .unwrap();
    metadata.close().await.unwrap();
    // The full row is intentionally large, as it is in the Q100 service stage.
    let body = "7d3a91c4e8b2f056".repeat(28_000);
    let mut revision = 0_i64;
    let mut stages = Vec::new();
    for (label, has_index) in [
        ("no-index-a", false),
        ("with-index", true),
        ("no-index-b", false),
    ] {
        if has_index {
            connection.query_drop(format!("ALTER TABLE mount_rs_tidb_metadata ADD INDEX IF NOT EXISTS {INDEX} (volume_key, revision)"))
                .await.unwrap();
        } else {
            connection
                .query_drop(format!(
                    "ALTER TABLE mount_rs_tidb_metadata DROP INDEX IF EXISTS {INDEX}"
                ))
                .await
                .unwrap();
        }
        let stage = format!("tidb-index-write-{label}-{stamp}");
        observe("begin", &stage, 0);
        let started = Instant::now();
        for seq in 0..UPDATES {
            revision += 1;
            let namespace = format!(r#"{{"blob":"{body}","revision":{revision},"seq":{seq}}}"#);
            connection
                .exec_drop(
                    "UPDATE mount_rs_tidb_metadata SET revision=?, namespace=? WHERE volume_key=?",
                    (revision, &namespace, &key),
                )
                .await
                .unwrap();
            assert_eq!(connection.affected_rows(), 1);
        }
        let elapsed = started.elapsed().as_secs_f64();
        observe("end", &stage, UPDATES);
        stages.push(json!({"stage":label,"successes":UPDATES,"namespace_bytes":body.len(),"elapsed_seconds":elapsed,"ops_per_second":UPDATES as f64/elapsed}));
    }
    connection.query_drop(format!("ALTER TABLE mount_rs_tidb_metadata ADD INDEX IF NOT EXISTS {INDEX} (volume_key, revision)"))
        .await.unwrap();
    let artifact = json!({"kind":"tidb_revision_index_write_cost","stages":stages});
    if let Ok(path) = std::env::var("MOUNT_RS_TIDB_INDEX_WRITE_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
    }
    println!("TIDB_INDEX_WRITE_COST {artifact}");
    drop(connection);
    pool.disconnect().await.unwrap();
}
