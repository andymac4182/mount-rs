#[path = "../src/inode_batch_size.rs"]
mod batch_size;
#[path = "support/guard_fixture.rs"]
mod guard_fixture;
#[path = "support/guard_proxy.rs"]
mod guard_proxy;
use mount_rs_core::storage::{BlockExtent, BlockStore, MetadataStore, NodeData};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore};
use mysql_async::prelude::Queryable;
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::test]
#[ignore = "requires actual TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_structural_guard_values_statement_count() {
    guard_values_case(false).await;
}
#[tokio::test]
#[ignore = "requires actual TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_guard_values_utf8_low_cap_wire_bound() {
    guard_values_case(true).await;
}
async fn guard_values_case(long_scope: bool) {
    let mut url = std::env::var("MOUNT_RS_TIDB_URL").unwrap();
    if long_scope {
        let mut parsed = url::Url::parse(&url).unwrap();
        parsed
            .query_pairs_mut()
            .append_pair("max_allowed_packet", "65536");
        url = parsed.into();
    }
    let mut key = format!(
        "guard-count-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    if long_scope {
        key.push_str(&"🗃".repeat(255 - key.chars().count()));
    }
    let proxy = guard_proxy::Proxy::new(&url).await;
    let metadata = TidbMetadataStore::connect_with_key(&proxy.url, &key)
        .await
        .unwrap();
    let direct = TidbMetadataStore::connect_with_key(&url, &key)
        .await
        .unwrap();
    let blocks = TidbBlockStore::connect_with_key(&url, &key).await.unwrap();
    let backing = blocks.prepare_concurrent_backing().await.unwrap();
    metadata
        .prepare_bound_concurrent_mode(backing)
        .await
        .unwrap();
    let bytes: Vec<u8> = (2_u64..=131)
        .flat_map(|inode| (0_u64..4).flat_map(move |part| (inode * 4 + part).to_le_bytes()))
        .collect();
    let block = blocks.put(&bytes).await.unwrap();
    let mut expected = guard_fixture::namespace();
    for (&inode, node) in expected.nodes.iter_mut().filter(|(id, _)| **id != 1) {
        node.stats.size = 32;
        node.stats.blocks = 1;
        let NodeData::File(layout) = &mut node.data else {
            unreachable!()
        };
        layout.extents.push(BlockExtent {
            file_offset: 0,
            block: block.clone(),
            block_offset: (inode - 2) * 32,
            length: 32,
        });
    }
    metadata
        .publish_bound_if_revision(backing, 0, expected.clone())
        .await
        .unwrap();
    proxy.begin();
    metadata.prepare_inode_mode(backing, 1).await.unwrap();
    let enrollment = proxy.end();
    let mut windows = vec![("enrollment", enrollment)];
    for generation in [2, 3] {
        let old = direct.load_inode_snapshot(backing).await.unwrap();
        expected.nodes.get_mut(&2).unwrap().stats.mtime_ms = generation as i64;
        proxy.begin();
        metadata
            .publish_structure_if_versions(
                backing,
                generation,
                &old.inode_revisions,
                expected.clone(),
            )
            .await
            .unwrap();
        windows.push((
            if generation == 2 {
                "structure-first-cache-warm"
            } else {
                "structure-warm"
            },
            proxy.end(),
        ));
    }
    let actual = direct.load_inode_snapshot(backing).await.unwrap();
    assert_eq!(actual.structural_generation, 4);
    assert_eq!(actual.inode_revisions.len(), 131);
    assert!(actual.inode_revisions.values().all(|v| *v == 0));
    assert_eq!(
        serde_json::to_vec(&actual.namespace).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    let read = blocks.get(&block).await.unwrap();
    assert_eq!(read, bytes);
    for inode in 2..=131 {
        let start = (inode - 2) * 32;
        assert_eq!(&read[start..start + 32], &bytes[start..start + 32]);
    }
    println!(
        "GUARD_ORACLE_PASS provider=tidb nodes=131 files=130 all_file_bytes=4160 generation=4"
    );
    metadata.close().await.unwrap();
    direct.close().await.unwrap();
    blocks.close().await.unwrap();
    let pool = mysql_async::Pool::from_url(&url).unwrap();
    let mut c = pool.get_conn().await.unwrap();
    for table in [
        "mount_rs_tidb_inodes",
        "mount_rs_tidb_metadata",
        "mount_rs_tidb_blocks",
        "mount_rs_tidb_block_authority",
    ] {
        c.exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (&key,))
            .await
            .unwrap();
    }
    drop(c);
    pool.disconnect().await.unwrap();
    for (phase, (inserts, prepares)) in &windows {
        println!(
            "GUARD_WIRE provider=tidb phase={phase} executes={} prepares={prepares} rows={:?} prepare_shape_bytes_per_execute_sum={} execute_bytes={}",
            inserts.len(),
            inserts.iter().map(|i| i.rows).collect::<Vec<_>>(),
            inserts.iter().map(|i| i.prepare_bytes).sum::<usize>(),
            inserts.iter().map(|i| i.execute_bytes).sum::<usize>()
        );
    }
    for (_, (inserts, _)) in windows {
        for insert in &inserts {
            let bound =
                batch_size::encoded_bound(insert.rows, insert.volume_bytes, insert.node_bytes)
                    .unwrap();
            assert!(insert.prepare_bytes + insert.execute_bytes <= bound);
            assert!(bound <= if long_scope { 65536 } else { 256 * 1024 });
        }

        if long_scope {
            assert!(inserts.iter().all(|i| i.rows < 64));
            assert_eq!(inserts.iter().map(|i| i.rows).sum::<usize>(), 131);
            assert!(inserts.iter().all(|i| i.rows <= 64));
        } else {
            assert_eq!(
                inserts.iter().map(|i| i.rows).collect::<Vec<_>>(),
                [64, 64, 3]
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires actual TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_multibatch_structural_commit_ack_is_not_replayed() {
    use std::sync::atomic::Ordering;
    let url = std::env::var("MOUNT_RS_TIDB_URL").unwrap();
    let key = format!(
        "guard-ack-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let proxy = guard_proxy::Proxy::new(&url).await;
    let store = TidbMetadataStore::connect_with_key(&proxy.url, &key)
        .await
        .unwrap();
    let direct = TidbMetadataStore::connect_with_key(&url, &key)
        .await
        .unwrap();
    let backing = mount_rs_core::storage::ConcurrentBackingId::from_bytes([6; 16]).unwrap();
    store.prepare_bound_concurrent_mode(backing).await.unwrap();
    let mut expected = guard_fixture::namespace();
    store
        .publish_bound_if_revision(backing, 0, expected.clone())
        .await
        .unwrap();
    store.prepare_inode_mode(backing, 1).await.unwrap();
    let before = direct.load_inode_snapshot(backing).await.unwrap();
    expected.nodes.get_mut(&2).unwrap().stats.mtime_ms = 777;
    proxy.begin();
    proxy.trace.drop_commit_ack.store(true, Ordering::SeqCst);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        store.publish_structure_if_versions(backing, 2, &before.inode_revisions, expected.clone()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code, mount_rs_core::ErrorCode::Eio);
    assert!(error.to_string().contains("commit outcome is unknown"));
    let (inserts, _) = proxy.end();
    assert_eq!(
        inserts.iter().map(|i| i.rows).collect::<Vec<_>>(),
        [64, 64, 3]
    );
    assert_eq!(proxy.trace.commits.load(Ordering::SeqCst), 1);
    assert_eq!(proxy.trace.acks_dropped.load(Ordering::SeqCst), 1);
    let after = direct.load_inode_snapshot(backing).await.unwrap();
    assert_eq!(after.structural_generation, 3);
    assert_eq!(after.inode_revisions.len(), 131);
    assert!(after.inode_revisions.values().all(|r| *r == 0));
    assert_eq!(
        serde_json::to_vec(&after.namespace).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    store.close().await.unwrap();
    direct.close().await.unwrap();
    let pool = mysql_async::Pool::from_url(&url).unwrap();
    let mut c = pool.get_conn().await.unwrap();
    for table in ["mount_rs_tidb_inodes", "mount_rs_tidb_metadata"] {
        c.exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (&key,))
            .await
            .unwrap();
    }
    drop(c);
    pool.disconnect().await.unwrap();
    println!(
        "GUARD_LOST_ACK_PASS provider=tidb inserts=[64,64,3] commit_sends=1 committed_ok_dropped=1 fresh_generation=3 exact_namespace=true replay=false"
    );
}
