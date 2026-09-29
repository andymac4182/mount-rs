//! Actual selected-write SQL controls included by the compact integration crate.
use super::*;
use mount_rs_core::diagnostics::storage;

fn assert_write_point_queries(queries: &[String]) {
    let locks: Vec<_> = queries
        .iter()
        .enumerate()
        .filter(|(_, sql)| {
            sql.starts_with("SELECT inode,")
                && sql.contains("FROM mount_rs_tidb_compact_guards ")
                && sql.contains("FOR UPDATE")
        })
        .collect();
    assert_eq!(locks.len(), 1, "one selected guard lock: {queries:?}");
    assert!(locks[0].1.contains("AND inode=?"));
    let authority: Vec<_> = queries
        .iter()
        .enumerate()
        .filter(|(_, sql)| {
            sql.starts_with("SELECT ")
                && sql.contains("mount_rs_tidb_metadata AS m")
                && sql.contains("LEFT JOIN mount_rs_tidb_compact_members")
                && !sql.contains("mount_rs_tidb_compact_guards")
        })
        .collect();
    assert_eq!(
        authority.len(),
        1,
        "one authority/member point statement: {queries:?}"
    );
    let (authority_position, sql) = authority[0];
    assert!(
        locks[0].0 < authority_position,
        "authority follows the guard lock"
    );
    assert!(sql.contains("s.volume_key=?"));
    assert!(sql.contains("s.inode=?"));
    assert!(sql.contains("WHERE m.volume_key=?"));
    assert!(!sql.contains("FOR UPDATE"));
    assert!(!sql.contains("LOCK IN SHARE MODE"));
    assert!(queries.iter().all(|sql| {
        !sql.contains("FROM mount_rs_tidb_compact_members ")
            && !sql.contains("mount_rs_tidb_compact_dentries")
    }));
    let dml: Vec<_> = queries
        .iter()
        .filter(|sql| {
            sql.starts_with("INSERT ") || sql.starts_with("UPDATE ") || sql.starts_with("DELETE ")
        })
        .collect();
    assert_eq!(dml.len(), 1, "only the selected guard is written");
    assert!(dml[0].starts_with("UPDATE mount_rs_tidb_compact_guards "));
}

#[tokio::test]
#[ignore = "requires actual owned TiDB, MOUNT_RS_TIDB_URL and MOUNT_RS_PROFILE_IO=1; run serial"]
async fn actual_indexed_write_cycle_uses_two_inode_reads() {
    assert!(storage::enabled(), "run with MOUNT_RS_PROFILE_IO=1");
    let f = fixture(true).await;
    let inode = create(&f, "write-query-selected").await;
    create(&f, "write-query-sibling").await;
    let initial = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let initial_file = &initial.guards[&inode];
    let initial_authority = CompactAuthority::from_anchor(&initial.anchor).unwrap();
    let initial_expectation = CompactFileExpectation::from_authority(
        &initial_authority,
        initial_file.identity,
        &initial_file.node,
    )
    .unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    // Warm connection/session and both prepared paths outside the counter window.
    writer
        .read_compact_file(f.backing, inode, initial_expectation)
        .await
        .unwrap();
    writer
        .publish_compact_inode(
            f.backing,
            inode,
            initial.anchor.generation,
            initial_file.identity,
            initial_file.node.clone(),
        )
        .await
        .unwrap();
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let file = &before.guards[&inode];
    let authority = CompactAuthority::from_anchor(&before.anchor).unwrap();
    let expectation =
        CompactFileExpectation::from_authority(&authority, file.identity, &file.node).unwrap();
    let mut node = file.node.clone();
    node.stats.mtime_ms += 1;

    proxy.begin();
    let counters_before = storage::snapshot();
    let read = writer
        .read_compact_file(f.backing, inode, expectation)
        .await
        .unwrap();
    let published = writer
        .publish_compact_inode(
            f.backing,
            inode,
            before.anchor.generation,
            file.identity,
            node.clone(),
        )
        .await
        .unwrap();
    let delta = storage::snapshot().delta(&counters_before).unwrap();
    let (queries, rows) = proxy.end();
    let fresh = f.store.load_compact_snapshot(f.backing).await.unwrap();
    // Settle providers and proxy before the deliberately failing RED budget.
    writer.close().await.unwrap();
    f.store.close().await.unwrap();
    proxy.shutdown().await;

    read.validate_expectation(expectation).unwrap();
    assert!(matches!(
        read.into_inode_read().unwrap(),
        CompactInodeRead::Unchanged(_)
    ));
    assert_eq!(published.guard.node, node);
    assert_eq!(fresh.anchor, before.anchor);
    for (&id, guard) in &before.guards {
        assert_eq!(
            &fresh.guards[&id],
            if id == inode { &published.guard } else { guard }
        );
    }
    assert_eq!(
        rows, 2,
        "the file read and the selected guard lock each return one row"
    );
    let inode_reads = delta
        .entries
        .iter()
        .find(|entry| entry.name == "tidb.sql.inode_read")
        .unwrap();
    eprintln!(
        "indexed write query control: inode_reads={} statements={} guard_rows={rows}; fresh full oracle passed",
        inode_reads.calls,
        queries.len()
    );
    assert_eq!(
        inode_reads.calls, 2,
        "the write must not issue a separate membership query"
    );
    for (name, calls) in [
        ("tidb.sql.inode_read", 2),
        ("tidb.sql.metadata_read", 1),
        ("tidb.sql.inode_write", 1),
        ("tidb.tx.begin.inode", 1),
        ("tidb.tx.commit", 1),
        ("tidb.tx.rollback", 0),
    ] {
        let entry = delta
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap();
        assert_eq!(entry.calls, calls, "{name}");
        assert_eq!(entry.success, calls, "{name}");
        assert_eq!(entry.error, 0, "{name}");
        assert_eq!(entry.cancelled, 0, "{name}");
    }
    assert_eq!(delta.in_flight, 0);
    assert_eq!(
        queries.len(),
        8,
        "one statement is removed from the complete write cycle"
    );
    assert_write_point_queries(&queries);
}

fn assert_no_write_dml(queries: &[String]) {
    assert!(
        queries.iter().all(|sql| {
            !sql.starts_with("INSERT ")
                && !sql.starts_with("UPDATE ")
                && !sql.starts_with("DELETE ")
                && !sql.eq_ignore_ascii_case("COMMIT")
        }),
        "refusal must precede DML and COMMIT: {queries:?}"
    );
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_write_authority_refusal_precedes_missing_member() {
    for (case, sql) in [
        (
            "mode",
            "UPDATE mount_rs_tidb_metadata SET write_mode='MRC4' WHERE volume_key=?",
        ),
        (
            "backing",
            "UPDATE mount_rs_tidb_metadata SET backing_id='11111111111111111111111111111111' WHERE volume_key=?",
        ),
        (
            "owner",
            "UPDATE mount_rs_tidb_metadata SET owner='foreign-owner' WHERE volume_key=?",
        ),
        (
            "fence",
            "UPDATE mount_rs_tidb_metadata SET fence=42 WHERE volume_key=?",
        ),
        (
            "expiry",
            "UPDATE mount_rs_tidb_metadata SET expires=1 WHERE volume_key=?",
        ),
        (
            "delegation",
            "UPDATE mount_rs_tidb_metadata SET delegation='{}' WHERE volume_key=?",
        ),
    ] {
        let f = fixture(true).await;
        let inode = create(&f, "authority-precedence").await;
        assert_eq!(inode, 2);
        let old = f.store.load_compact_inode(f.backing, inode).await.unwrap();
        let proxy = compact_proxy::Proxy::new(&f.url).await;
        let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
            .await
            .unwrap();
        corrupt(&f, sql).await;
        corrupt(
            &f,
            "DELETE FROM mount_rs_tidb_compact_members WHERE volume_key=? AND inode=2",
        )
        .await;
        let before = raw(&f).await;
        let mut node = old.guard.node;
        node.stats.mtime_ms += 1;
        proxy.begin();
        let publication = writer
            .publish_compact_inode(f.backing, inode, old.generation, old.guard.identity, node)
            .await;
        let (queries, _) = proxy.end();
        let after = raw(&f).await;
        writer.close().await.unwrap();
        f.store.close().await.unwrap();
        proxy.shutdown().await;
        assert!(
            publication.unwrap_err().is(ErrorCode::Estale),
            "{case}: authority must precede absent membership"
        );
        assert_no_write_dml(&queries);
        assert_eq!(
            after, before,
            "{case}: refusal preserves every physical row"
        );
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_write_generation_conflict_precedes_missing_member() {
    let f = fixture(true).await;
    let inode = create(&f, "generation-precedence").await;
    assert_eq!(inode, 2);
    let old = f.store.load_compact_inode(f.backing, inode).await.unwrap();
    create(&f, "new-generation").await;
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    corrupt(
        &f,
        "DELETE FROM mount_rs_tidb_compact_members WHERE volume_key=? AND inode=2",
    )
    .await;
    let before = raw(&f).await;
    let mut node = old.guard.node;
    node.stats.mtime_ms += 1;
    proxy.begin();
    let publication = writer
        .publish_compact_inode(f.backing, inode, old.generation, old.guard.identity, node)
        .await;
    let (queries, _) = proxy.end();
    let after = raw(&f).await;
    writer.close().await.unwrap();
    f.store.close().await.unwrap();
    proxy.shutdown().await;
    assert!(publication.unwrap_err().is(ErrorCode::Eagain));
    assert_no_write_dml(&queries);
    assert_eq!(after, before);
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_write_missing_member_refuses_without_mutation() {
    let f = fixture(true).await;
    let inode = create(&f, "missing-write-member").await;
    assert_eq!(inode, 2);
    let foreign = fixture(true).await;
    assert_eq!(create(&foreign, "foreign-same-inode").await, inode);
    let foreign_before = raw(&foreign).await;
    let old = f.store.load_compact_inode(f.backing, inode).await.unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    corrupt(
        &f,
        "DELETE FROM mount_rs_tidb_compact_members WHERE volume_key=? AND inode=2",
    )
    .await;
    let before = raw(&f).await;
    let mut node = old.guard.node;
    node.stats.mtime_ms += 1;
    proxy.begin();
    let publication = writer
        .publish_compact_inode(f.backing, inode, old.generation, old.guard.identity, node)
        .await;
    let (queries, rows) = proxy.end();
    let after = raw(&f).await;
    let foreign_after = raw(&foreign).await;
    writer.close().await.unwrap();
    f.store.close().await.unwrap();
    foreign.store.close().await.unwrap();
    proxy.shutdown().await;
    assert!(publication.unwrap_err().is(ErrorCode::Einval));
    assert_eq!(
        rows, 1,
        "the retained guard is locked before membership refusal"
    );
    assert_no_write_dml(&queries);
    assert_eq!(after, before);
    assert_eq!(
        foreign_after, foreign_before,
        "same inode in another volume supplies no membership"
    );
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_write_retained_tombstone_preserves_membership_and_reopens_bytes() {
    use mount_rs_core::storage::{BlockExtent, BlockStore};
    use mount_rs_tidb::TidbBlockStore;

    const OLD_BYTES: &[u8] = b"original detached bytes";
    const NEW_BYTES: &[u8] = b"updated detached immutable bytes";
    let f = fixture(true).await;
    let inode = create(&f, "write-tombstone").await;
    let blocks = TidbBlockStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let old_block = blocks.put(OLD_BYTES).await.unwrap();
    blocks.flush().await.unwrap();
    let old = f.store.load_compact_inode(f.backing, inode).await.unwrap();
    let mut populated = old.guard.node;
    populated.stats.size = OLD_BYTES.len() as u64;
    populated.stats.blocks = 1;
    let NodeData::File(layout) = &mut populated.data else {
        panic!("regular file required")
    };
    layout.extents = vec![BlockExtent {
        file_offset: 0,
        block: old_block.clone(),
        block_offset: 0,
        length: OLD_BYTES.len() as u64,
    }];
    f.store
        .publish_compact_inode(
            f.backing,
            inode,
            old.generation,
            old.guard.identity,
            populated,
        )
        .await
        .unwrap();
    let transition = root_file_unlink_transition(&f, &f.store, inode, "write-tombstone").await;
    f.store
        .publish_compact_structure(transition.delta())
        .await
        .unwrap();
    let old = f.store.load_compact_inode(f.backing, inode).await.unwrap();
    assert_eq!(old.guard.node.stats.nlink, 0);
    let new_block = blocks.put(NEW_BYTES).await.unwrap();
    blocks.flush().await.unwrap();
    let mut updated = old.guard.node;
    updated.stats.size = NEW_BYTES.len() as u64;
    updated.stats.mtime_ms += 1;
    updated.stats.ctime_ms += 1;
    let NodeData::File(layout) = &mut updated.data else {
        panic!("retained regular file required")
    };
    layout.extents = vec![BlockExtent {
        file_offset: 0,
        block: new_block.clone(),
        block_offset: 0,
        length: NEW_BYTES.len() as u64,
    }];
    let before = raw(&f).await;
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    proxy.begin();
    let published = writer
        .publish_compact_inode(
            f.backing,
            inode,
            old.generation,
            old.guard.identity,
            updated.clone(),
        )
        .await
        .unwrap();
    let (queries, rows) = proxy.end();
    let after = raw(&f).await;
    writer.close().await.unwrap();
    f.store.close().await.unwrap();
    blocks.close().await.unwrap();
    proxy.shutdown().await;

    let fresh = TidbMetadataStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let fresh_blocks = TidbBlockStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let reopened = fresh.load_compact_snapshot(f.backing).await.unwrap();
    let actual_old = fresh_blocks.get(&old_block).await.unwrap();
    let actual_new = fresh_blocks.get(&new_block).await.unwrap();
    fresh.close().await.unwrap();
    fresh_blocks.close().await.unwrap();

    assert_eq!(rows, 1);
    assert_write_point_queries(&queries);
    assert_eq!(
        before.0, after.0,
        "selected publication leaves authority unchanged"
    );
    assert_eq!(before.2, after.2, "backing marker remains unchanged");
    assert_eq!(before.3, after.3, "tombstone remains a selected member");
    assert_eq!(before.4, after.4, "selected publication creates no link");
    assert!(after.3.contains(&(inode as i64)));
    assert!(!after.4.iter().any(|row| row.4 == inode as i64));
    assert_eq!(reopened.guards[&inode], published.guard);
    assert_eq!(reopened.guards[&inode].node, updated);
    assert_eq!(reopened.guards[&inode].node.stats.nlink, 0);
    assert_eq!(actual_old, OLD_BYTES);
    assert_eq!(actual_new, NEW_BYTES);
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_write_independent_file_ignores_another_guard_lock() {
    let f = fixture(true).await;
    let held_inode = create(&f, "held-write-guard").await;
    let inode = create(&f, "independent-write-guard").await;
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let file = &before.guards[&inode];
    let mut node = file.node.clone();
    node.stats.mtime_ms += 1;
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    let pool = Pool::from_url(&f.url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    connection
        .query_drop("SET SESSION tidb_txn_mode='pessimistic'")
        .await
        .unwrap();
    let mut held = connection
        .start_transaction(mysql_async::TxOpts::default())
        .await
        .unwrap();
    let locked: Option<i64> = held
        .exec_first(
            "SELECT inode FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=? FOR UPDATE",
            (&f.key, held_inode as i64),
        )
        .await
        .unwrap();
    assert_eq!(locked, Some(held_inode as i64));
    proxy.begin();
    let publication = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        writer.publish_compact_inode(
            f.backing,
            inode,
            before.anchor.generation,
            file.identity,
            node.clone(),
        ),
    )
    .await;
    let (queries, rows) = proxy.end();
    // Release the deliberately held transaction even if the bounded writer failed.
    held.rollback().await.unwrap();
    drop(connection);
    pool.disconnect().await.unwrap();
    let fresh = f.store.load_compact_snapshot(f.backing).await.unwrap();
    writer.close().await.unwrap();
    f.store.close().await.unwrap();
    proxy.shutdown().await;

    let published = publication
        .expect("an independent file must publish before the held guard is released")
        .unwrap();
    assert_eq!(rows, 1);
    assert_write_point_queries(&queries);
    assert_eq!(fresh.anchor, before.anchor);
    assert_eq!(fresh.guards[&held_inode], before.guards[&held_inode]);
    assert_eq!(fresh.guards[&inode], published.guard);
    assert_eq!(published.guard.node, node);
}
