//! Actual transaction-order controls for authority-owned structural publication.
use super::*;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::time::timeout;

const BOUND: Duration = Duration::from_secs(10);
const AUTHORITY: &str = "SELECT revision,write_mode,backing_id,owner,fence,expires,namespace,delegation FROM mount_rs_tidb_metadata WHERE volume_key=?";
const MEMBERS: &str =
    "SELECT inode FROM mount_rs_tidb_compact_members WHERE volume_key=? ORDER BY inode";
const PARENT: &str = "SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=? ORDER BY inode FOR UPDATE";
const ENTRIES: &str = "SELECT parent,ordinal,name_hash,name,inode FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND parent=? ORDER BY parent,ordinal";

fn create_proposal(base: &CompactSnapshot, name: &str) -> CompactRootFileCreate {
    let (_, _, audited) = base.clone().into_validated_namespace().unwrap();
    let mut namespace = base.namespace().unwrap();
    let inode = add_file(&mut namespace, name);
    let created = namespace.nodes.remove(&inode).unwrap();
    let root = &base.guards[&base.anchor.root];
    CompactRootFileCreate::capture_audited(
        &audited,
        root.identity,
        name.into(),
        created,
        root.node.stats.mtime_ms.checked_add(1).unwrap(),
        root.node.stats.ctime_ms.checked_add(1).unwrap(),
    )
    .unwrap()
}

fn authority_lock(sql: &str) -> bool {
    sql.starts_with("SELECT owner, fence, expires, ")
        && sql.contains("FROM mount_rs_tidb_metadata")
        && sql.contains("WHERE volume_key=?")
        && sql.contains("FOR UPDATE")
}

fn one_position(queries: &[String], predicate: impl Fn(&str) -> bool, label: &str) -> usize {
    let positions: Vec<_> = queries
        .iter()
        .enumerate()
        .filter_map(|(position, sql)| predicate(sql).then_some(position))
        .collect();
    assert_eq!(positions.len(), 1, "exactly one {label}: {queries:?}");
    positions[0]
}

fn assert_complete_parent_proof(queries: &[String]) -> usize {
    let start = one_position(
        queries,
        |sql| sql.starts_with("START TRANSACTION"),
        "publication transaction",
    );
    let lock = one_position(queries, authority_lock, "authority row lock");
    let authority = one_position(queries, |sql| sql == AUTHORITY, "fresh authority read");
    let members = one_position(queries, |sql| sql == MEMBERS, "complete member read");
    let parent = one_position(queries, |sql| sql == PARENT, "locked parent read");
    let entries = one_position(
        queries,
        |sql| sql == ENTRIES,
        "complete nonlocking dentry read",
    );
    assert!(
        start < lock
            && lock < authority
            && authority < members
            && members < parent
            && parent < entries,
        "complete current proof follows authority ownership and the parent lock"
    );
    assert!(
        queries
            .iter()
            .all(|sql| !sql.contains("FROM mount_rs_tidb_compact_dentries") || sql == ENTRIES)
    );
    entries
}

fn structural_dml(sql: &str) -> bool {
    sql.starts_with("INSERT ") || sql.starts_with("UPDATE ") || sql.starts_with("DELETE ")
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_structural_parent_proof_observes_tamper_committed_after_transaction_start() {
    let f = timeout(BOUND, fixture(true)).await.unwrap();
    timeout(BOUND, create(&f, "unchanged-sibling"))
        .await
        .unwrap();
    let sibling = timeout(BOUND, create(&f, "unrequested-sibling"))
        .await
        .unwrap();
    let base = timeout(BOUND, f.store.load_compact_snapshot(f.backing))
        .await
        .unwrap()
        .unwrap();
    let proposal = create_proposal(&base, "transaction-start-create");
    let original = timeout(BOUND, raw(&f)).await.unwrap();
    let mut damaged = original.clone();
    let replacement = b"changed-unrequested-sibling";
    let replacement_hash = Sha256::digest(replacement).to_vec();
    let row = damaged
        .4
        .iter_mut()
        .find(|row| row.0 == base.anchor.root as i64 && row.4 == sibling as i64)
        .unwrap();
    let ordinal = row.1;
    row.2 = replacement_hash.clone();
    row.3 = replacement.to_vec();

    let proxy = timeout(BOUND, compact_proxy::Proxy::new(&f.url))
        .await
        .unwrap();
    let writer = timeout(
        BOUND,
        TidbMetadataStore::connect_with_key(&proxy.url, &f.key),
    )
    .await
    .unwrap()
    .unwrap();
    let pool = Pool::from_url(&f.url).unwrap();
    let mut connection = timeout(BOUND, pool.get_conn()).await.unwrap().unwrap();
    timeout(
        BOUND,
        connection.query_drop("SET SESSION tidb_txn_mode='pessimistic'"),
    )
    .await
    .unwrap()
    .unwrap();
    let mut tamper = timeout(
        BOUND,
        connection.start_transaction(mysql_async::TxOpts::default()),
    )
    .await
    .unwrap()
    .unwrap();
    let locked: Option<i64> = timeout(
        BOUND,
        tamper.exec_first(
            "SELECT revision FROM mount_rs_tidb_metadata WHERE volume_key=? FOR UPDATE",
            (&f.key,),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(locked, Some(base.anchor.generation as i64));

    proxy.begin();
    proxy
        .trace
        .pause_authority_lock
        .store(true, Ordering::SeqCst);
    let mut publishing = tokio::spawn({
        let writer = writer.clone();
        async move { writer.publish_compact_structure(proposal.delta()).await }
    });
    let reached = timeout(BOUND, proxy.trace.reached.notified()).await;
    let paused_queries = proxy.trace.queries.lock().unwrap().clone();
    // The proxy holds the lock request before forwarding it. The provider has
    // already awaited START TRANSACTION, but no SQL lock wait is claimed here.
    let (tampered, committed, rolled_back) = if reached.is_ok() {
        let tampered = timeout(
            BOUND,
            tamper.exec_drop(
                "UPDATE mount_rs_tidb_compact_dentries SET name_hash=?,name=? WHERE volume_key=? AND parent=? AND ordinal=? AND inode=?",
                (
                    &replacement_hash,
                    replacement.as_slice(),
                    &f.key,
                    base.anchor.root as i64,
                    ordinal,
                    sibling as i64,
                ),
            ),
        )
        .await;
        if matches!(&tampered, Ok(Ok(()))) {
            (
                Some(tampered),
                Some(timeout(BOUND, tamper.commit()).await),
                None,
            )
        } else {
            (
                Some(tampered),
                None,
                Some(timeout(BOUND, tamper.rollback()).await),
            )
        }
    } else {
        (None, None, Some(timeout(BOUND, tamper.rollback()).await))
    };
    // Resume on every path; retain uncertain direct COMMIT as a failed control.
    proxy.trace.resume.notify_one();
    let published = timeout(BOUND, &mut publishing).await;
    let aborted = if published.is_err() {
        publishing.abort();
        Some(timeout(BOUND, &mut publishing).await)
    } else {
        None
    };
    let (queries, _) = proxy.end();
    let writer_closed = timeout(BOUND, writer.close()).await;
    let fixture_closed = timeout(BOUND, f.store.close()).await;
    drop(connection);
    let pool_closed = timeout(BOUND, pool.disconnect()).await;
    let proxy_closed = timeout(BOUND, proxy.shutdown()).await;
    let settled = published.is_ok() || matches!(&aborted, Some(Ok(_)));
    let after = if settled
        && matches!(&writer_closed, Ok(Ok(())))
        && matches!(&fixture_closed, Ok(Ok(())))
        && matches!(&pool_closed, Ok(Ok(())))
        && proxy_closed.is_ok()
    {
        Some(timeout(BOUND, raw(&f)).await)
    } else {
        None
    };

    writer_closed.unwrap().unwrap();
    fixture_closed.unwrap().unwrap();
    pool_closed.unwrap().unwrap();
    proxy_closed.unwrap();
    reached.expect("pre-forward authority-lock request reached");
    let start = one_position(
        &paused_queries,
        |sql| sql.starts_with("START TRANSACTION"),
        "already-started transaction",
    );
    let lock = one_position(&paused_queries, authority_lock, "pending authority lock");
    assert!(start < lock);
    assert_eq!(
        lock + 1,
        paused_queries.len(),
        "lock request is the paused boundary"
    );
    assert!(paused_queries.iter().all(|sql| {
        !structural_dml(sql)
            && sql != AUTHORITY
            && sql != MEMBERS
            && sql != PARENT
            && !sql.contains("FROM mount_rs_tidb_compact_dentries")
            && !sql.eq_ignore_ascii_case("COMMIT")
    }));
    tampered.unwrap().unwrap().unwrap();
    committed.unwrap().unwrap().unwrap();
    assert!(rolled_back.is_none());
    let error = published
        .expect("publication settled")
        .expect("publication task completed")
        .unwrap_err();
    assert!(
        error.is(ErrorCode::Einval),
        "fresh complete parent refuses the changed sibling body"
    );
    assert_complete_parent_proof(&queries);
    assert!(
        queries
            .iter()
            .all(|sql| { !structural_dml(sql) && !sql.eq_ignore_ascii_case("COMMIT") })
    );
    assert_eq!(
        after
            .expect("all writers and relays settled before observer")
            .unwrap(),
        damaged,
        "fresh independent raw oracle preserves the exact damaged state; authority, guards, member set, ordinal and child remain unchanged"
    );
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_structural_create_preserves_unrelated_selected_write_under_parent_lock() {
    let f = timeout(BOUND, fixture(true)).await.unwrap();
    let unrelated = timeout(BOUND, create(&f, "unrelated-selected-file"))
        .await
        .unwrap();
    let base = timeout(BOUND, f.store.load_compact_snapshot(f.backing))
        .await
        .unwrap()
        .unwrap();
    let proposal = create_proposal(&base, "concurrent-create");
    let expected_anchor = proposal.delta().next_anchor().clone();
    let old = &base.guards[&unrelated];
    let mut newer = old.node.clone();
    newer.stats.mtime_ms = newer.stats.mtime_ms.checked_add(17).unwrap();
    newer.stats.ctime_ms = newer.stats.ctime_ms.checked_add(23).unwrap();
    let proxy = timeout(BOUND, compact_proxy::Proxy::new(&f.url))
        .await
        .unwrap();
    let writer = timeout(
        BOUND,
        TidbMetadataStore::connect_with_key(&proxy.url, &f.key),
    )
    .await
    .unwrap()
    .unwrap();

    proxy.begin();
    proxy.trace.pause_dentries.store(true, Ordering::SeqCst);
    let mut publishing = tokio::spawn({
        let writer = writer.clone();
        async move { writer.publish_compact_structure(proposal.delta()).await }
    });
    let reached = timeout(BOUND, proxy.trace.reached.notified()).await;
    let paused_queries = proxy.trace.queries.lock().unwrap().clone();
    // The next dentry request follows successful authority and parent lock
    // responses. Selected publication uses the separate direct fixture pool.
    let selected = if reached.is_ok() {
        Some(
            timeout(
                BOUND,
                f.store.publish_compact_inode(
                    f.backing,
                    unrelated,
                    base.anchor.generation,
                    old.identity,
                    newer.clone(),
                ),
            )
            .await,
        )
    } else {
        None
    };
    let still_paused = !publishing.is_finished();
    proxy.trace.resume.notify_one();
    let published = timeout(BOUND, &mut publishing).await;
    let aborted = if published.is_err() {
        publishing.abort();
        Some(timeout(BOUND, &mut publishing).await)
    } else {
        None
    };
    let (queries, _) = proxy.end();
    let writer_closed = timeout(BOUND, writer.close()).await;
    let fixture_closed = timeout(BOUND, f.store.close()).await;
    let proxy_closed = timeout(BOUND, proxy.shutdown()).await;
    let settled = published.is_ok() || matches!(&aborted, Some(Ok(_)));
    let fresh = if settled
        && matches!(&writer_closed, Ok(Ok(())))
        && matches!(&fixture_closed, Ok(Ok(())))
        && proxy_closed.is_ok()
    {
        Some(timeout(BOUND, TidbMetadataStore::connect_with_key(&f.url, &f.key)).await)
    } else {
        None
    };
    let (snapshot, observer_closed) = match &fresh {
        Some(Ok(Ok(observer))) => (
            Some(timeout(BOUND, observer.load_compact_snapshot(f.backing)).await),
            Some(timeout(BOUND, observer.close()).await),
        ),
        _ => (None, None),
    };

    writer_closed.unwrap().unwrap();
    fixture_closed.unwrap().unwrap();
    proxy_closed.unwrap();
    reached.expect("complete dentry request reached after held authority and parent locks");
    let entries = assert_complete_parent_proof(&paused_queries);
    assert_eq!(entries + 1, paused_queries.len());
    assert!(
        paused_queries
            .iter()
            .all(|sql| { !structural_dml(sql) && !sql.eq_ignore_ascii_case("COMMIT") })
    );
    assert!(
        still_paused,
        "selected write completed before structural publisher resumed"
    );
    let selected = selected
        .expect("selected publication attempted while structural locks held")
        .expect("selected publication completed under structural locks")
        .unwrap();
    assert_eq!(selected.guard.node, newer);
    assert_eq!(
        selected.guard.identity.incarnation,
        old.identity.incarnation
    );
    assert_eq!(selected.guard.identity.epoch, base.anchor.generation);
    assert_eq!(selected.guard.identity.revision, old.identity.revision + 1);
    let publication = published
        .expect("structural publication settled")
        .expect("structural task completed")
        .unwrap();
    assert_eq!(publication.anchor, expected_anchor);
    assert!(!publication.upserts.contains_key(&unrelated));
    let entries = assert_complete_parent_proof(&queries);
    let first_dml = queries.iter().position(|sql| structural_dml(sql)).unwrap();
    assert!(entries < first_dml);
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.starts_with("START TRANSACTION"))
            .count(),
        1
    );
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.eq_ignore_ascii_case("COMMIT"))
            .count(),
        1
    );
    assert!(
        fresh
            .as_ref()
            .is_some_and(|result| matches!(result, Ok(Ok(_))))
    );
    observer_closed.unwrap().unwrap().unwrap();
    let snapshot = snapshot.unwrap().unwrap().unwrap();
    assert_eq!(snapshot.anchor, expected_anchor);
    let mut expected_guards = base.guards.clone();
    expected_guards.insert(unrelated, selected.guard);
    expected_guards.extend(publication.upserts);
    assert_eq!(
        snapshot.guards, expected_guards,
        "fresh complete oracle retains every physical body and the newer unrelated selected revision"
    );
    let namespace = snapshot.namespace().unwrap();
    let NodeData::Directory { entries } = &namespace.nodes[&namespace.root].data else {
        panic!("complete root directory required");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "unrelated-selected-file");
    assert_eq!(entries[0].inode, unrelated);
    assert_eq!(entries[1].name, "concurrent-create");
    assert_eq!(entries[1].inode, base.anchor.next_inode);
    assert_eq!(namespace.nodes[&unrelated], newer);
}
