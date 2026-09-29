//! Actual TiDB selected point-read scope, corruption and cancellation controls.
use super::*;

const POINT_SCOPE_NAME: &str = "scope-'\\-雪";

struct PointScopeFixtures {
    a: Fixture,
    b: Fixture,
    inode: u64,
    a_base: CompactSnapshot,
    b_base: CompactSnapshot,
}

async fn point_scope_fixtures() -> PointScopeFixtures {
    let a = fixture(true).await;
    let b = fixture(true).await;
    assert_ne!(a.key, b.key);
    let inode = create(&a, POINT_SCOPE_NAME).await;
    assert_eq!(inode, create(&b, POINT_SCOPE_NAME).await);
    assert_eq!(
        inode, 2,
        "the scoped corruption cases select the first file"
    );
    let old_b = b.store.load_compact_inode(b.backing, inode).await.unwrap();
    let mut different = old_b.guard.node.clone();
    different.stats.mtime_ms += 73;
    b.store
        .publish_compact_inode(
            b.backing,
            inode,
            old_b.generation,
            old_b.guard.identity,
            different,
        )
        .await
        .unwrap();
    let a_base = a.store.load_compact_snapshot(a.backing).await.unwrap();
    let b_base = b.store.load_compact_snapshot(b.backing).await.unwrap();
    assert_eq!(a_base.anchor, b_base.anchor);
    assert_eq!(
        a_base.guards[&a_base.anchor.root],
        b_base.guards[&b_base.anchor.root]
    );
    assert_ne!(
        a_base.guards[&inode].identity,
        b_base.guards[&inode].identity
    );
    assert_ne!(a_base.guards[&inode].node, b_base.guards[&inode].node);
    PointScopeFixtures {
        a,
        b,
        inode,
        a_base,
        b_base,
    }
}

type PointScopeReads = (
    mount_rs_core::Result<CompactFileRead>,
    usize,
    mount_rs_core::Result<CompactRootEntryRead>,
    usize,
);

async fn point_scope_observe(
    reader: &TidbMetadataStore,
    proxy: &compact_proxy::Proxy,
    f: &Fixture,
    root: u64,
    inode: u64,
    expected: CompactFileExpectation<'_>,
) -> PointScopeReads {
    proxy.begin();
    let file = reader.read_compact_file(f.backing, inode, expected).await;
    let (_, file_rows) = proxy.end();
    proxy.begin();
    let entry = reader
        .read_compact_root_entry(f.backing, root, inode, POINT_SCOPE_NAME, expected)
        .await;
    let (_, entry_rows) = proxy.end();
    (file, file_rows, entry, entry_rows)
}

fn point_scope_assert_owned_file(outcome: CompactInodeRead, expected: &CompactGuard) {
    match outcome {
        CompactInodeRead::Unchanged(checked) => {
            assert_eq!(checked.inode(), expected.node.stats.ino);
            assert_eq!(checked.identity(), expected.identity);
            assert!(checked.into_verified_root().is_none());
        }
        CompactInodeRead::Loaded(loaded) => assert_eq!(loaded.guard, *expected),
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_points_keep_same_named_guards_in_their_own_volume() {
    let pair = point_scope_fixtures().await;
    let before_a = raw(&pair.a).await;
    let before_b = raw(&pair.b).await;
    assert_eq!(
        before_a.3, before_b.3,
        "same inode membership in both volumes"
    );
    assert_eq!(
        before_a.4, before_b.4,
        "same parent, ordinal, hash and binary name"
    );
    let proxy = compact_proxy::Proxy::new(&pair.a.url).await;
    let mut observations = vec![];
    let mut reader_closes = vec![];
    for (f, base) in [(&pair.a, &pair.a_base), (&pair.b, &pair.b_base)] {
        let (_, _, audited) = base.clone().into_validated_namespace().unwrap();
        let guard = &base.guards[&pair.inode];
        let expected =
            CompactFileExpectation::from_structure(&audited, guard.identity, &guard.node).unwrap();
        let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
            .await
            .unwrap();
        let (warm_file, _, warm_entry, _) =
            point_scope_observe(&reader, &proxy, f, base.anchor.root, pair.inode, expected).await;
        warm_file.unwrap();
        warm_entry.unwrap();
        let (file, file_rows, entry, entry_rows) =
            point_scope_observe(&reader, &proxy, f, base.anchor.root, pair.inode, expected).await;
        let file = file.map(|read| {
            let generation = read.generation();
            let validation = read.validate_expectation(expected);
            (generation, validation, read.into_inode_read())
        });
        let entry = entry.map(|read| (read.generation(), read.into_inode_read(&audited)));
        observations.push((
            base.anchor.generation,
            guard.clone(),
            file,
            file_rows,
            entry,
            entry_rows,
        ));
        reader_closes.push(reader.close().await);
    }
    let after_a = raw(&pair.a).await;
    let after_b = raw(&pair.b).await;
    let a_closed = pair.a.store.close().await;
    let b_closed = pair.b.store.close().await;
    proxy.shutdown().await;
    for closed in reader_closes {
        closed.unwrap();
    }
    a_closed.unwrap();
    b_closed.unwrap();

    assert_eq!(
        after_a, before_a,
        "all captured A rows survive both scoped reads"
    );
    assert_eq!(
        after_b, before_b,
        "all captured B rows survive both scoped reads"
    );
    for (generation, guard, file, file_rows, entry, entry_rows) in observations {
        assert_eq!(
            file_rows, 1,
            "one own-volume selected row, without decoy fanout"
        );
        assert_eq!(
            entry_rows, 1,
            "one own-volume exact dentry, without decoy fanout"
        );
        let (observed_generation, validation, outcome) = file.unwrap();
        assert_eq!(observed_generation, generation);
        validation.unwrap();
        point_scope_assert_owned_file(outcome.unwrap(), &guard);
        let (observed_generation, outcome) = entry.unwrap();
        assert_eq!(observed_generation, generation);
        let CompactInodeRead::Loaded(loaded) = outcome.unwrap() else {
            panic!("root-entry read must return the complete own-volume guard");
        };
        assert_eq!(loaded.guard, guard);
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_missing_selected_groups_cannot_borrow_another_volume() {
    let cases = [
        (
            "root member",
            false,
            "DELETE FROM mount_rs_tidb_compact_members WHERE volume_key=? AND inode=1",
        ),
        (
            "root guard",
            false,
            "DELETE FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=1",
        ),
        (
            "selected dentry",
            false,
            "DELETE FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND parent=1 AND inode=2",
        ),
        (
            "file member",
            true,
            "DELETE FROM mount_rs_tidb_compact_members WHERE volume_key=? AND inode=2",
        ),
        (
            "file guard",
            true,
            "DELETE FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=2",
        ),
    ];
    for changed_generation in [false, true] {
        for (label, missing_file_group, sql) in cases {
            let pair = point_scope_fixtures().await;
            let base = &pair.a_base;
            let (_, _, audited) = base.clone().into_validated_namespace().unwrap();
            let guard = &base.guards[&pair.inode];
            let expected =
                CompactFileExpectation::from_structure(&audited, guard.identity, &guard.node)
                    .unwrap();
            let proxy = compact_proxy::Proxy::new(&pair.a.url).await;
            let reader = TidbMetadataStore::connect_with_key(&proxy.url, &pair.a.key)
                .await
                .unwrap();
            let (warm_file, _, warm_entry, _) = point_scope_observe(
                &reader,
                &proxy,
                &pair.a,
                base.anchor.root,
                pair.inode,
                expected,
            )
            .await;
            warm_file.unwrap();
            warm_entry.unwrap();
            if changed_generation {
                create(&pair.a, "scope-new-generation").await;
            }
            let decoy = raw(&pair.b).await;
            corrupt(&pair.a, sql).await;
            let before_a = raw(&pair.a).await;
            let before_b = raw(&pair.b).await;
            let (file, file_rows, entry, entry_rows) = point_scope_observe(
                &reader,
                &proxy,
                &pair.a,
                base.anchor.root,
                pair.inode,
                expected,
            )
            .await;
            let after_a = raw(&pair.a).await;
            let after_b = raw(&pair.b).await;
            let reader_closed = reader.close().await;
            let a_closed = pair.a.store.close().await;
            let b_closed = pair.b.store.close().await;
            proxy.shutdown().await;
            reader_closed.unwrap();
            a_closed.unwrap();
            b_closed.unwrap();

            assert_eq!(
                before_b, decoy,
                "{label}: scoped corruption retains every decoy row"
            );
            assert_eq!(
                after_a, before_a,
                "{label}: reads preserve the damaged A rows"
            );
            assert_eq!(after_b, before_b, "{label}: reads preserve every B row");
            assert_eq!(
                file_rows, 1,
                "{label}: A authority survives selected absence"
            );
            assert_eq!(
                entry_rows, 1,
                "{label}: A authority survives selected absence"
            );
            let observed_generation = u64::try_from(before_a.0.0).unwrap();
            let file = file.expect("selected absence is deferred inside an authority receipt");
            let entry = entry.expect("root-entry absence is deferred inside an authority receipt");
            assert_eq!(file.generation(), observed_generation, "{label}");
            assert_eq!(entry.generation(), observed_generation, "{label}");
            if changed_generation {
                assert!(observed_generation > base.anchor.generation);
                assert!(
                    file.validate_expectation(expected)
                        .unwrap_err()
                        .is(ErrorCode::Eagain),
                    "{label}"
                );
                assert!(
                    entry
                        .into_inode_read(&audited)
                        .unwrap_err()
                        .is(ErrorCode::Eagain),
                    "{label}"
                );
                // Never consume/admit a selected FILE result after its fence failed.
                drop(file);
            } else {
                assert_eq!(observed_generation, base.anchor.generation);
                file.validate_expectation(expected).unwrap();
                assert!(
                    entry
                        .into_inode_read(&audited)
                        .unwrap_err()
                        .is(ErrorCode::Einval),
                    "{label}"
                );
                if missing_file_group {
                    assert!(
                        file.into_inode_read().unwrap_err().is(ErrorCode::Einval),
                        "{label}"
                    );
                } else {
                    // A selected FILE receipt deliberately makes no root/dentry claim.
                    point_scope_assert_owned_file(file.into_inode_read().unwrap(), guard);
                }
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_duplicate_exact_dentry_defers_cardinality_after_generation() {
    for changed_generation in [false, true] {
        let pair = point_scope_fixtures().await;
        let base = &pair.a_base;
        let (_, _, audited) = base.clone().into_validated_namespace().unwrap();
        let guard = &base.guards[&pair.inode];
        let expected =
            CompactFileExpectation::from_structure(&audited, guard.identity, &guard.node).unwrap();
        let proxy = compact_proxy::Proxy::new(&pair.a.url).await;
        let reader = TidbMetadataStore::connect_with_key(&proxy.url, &pair.a.key)
            .await
            .unwrap();
        let (warm_file, _, warm_entry, _) = point_scope_observe(
            &reader,
            &proxy,
            &pair.a,
            base.anchor.root,
            pair.inode,
            expected,
        )
        .await;
        warm_file.unwrap();
        warm_entry.unwrap();
        if changed_generation {
            create(&pair.a, "duplicate-new-generation").await;
        }
        let decoy = raw(&pair.b).await;
        corrupt(&pair.a,
            "INSERT INTO mount_rs_tidb_compact_dentries(volume_key,parent,ordinal,name_hash,name,inode) SELECT volume_key,parent,42,name_hash,name,inode FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND parent=1 AND inode=2",
        ).await;
        let before_a = raw(&pair.a).await;
        let before_b = raw(&pair.b).await;
        proxy.begin();
        let entry = reader
            .read_compact_root_entry(
                pair.a.backing,
                base.anchor.root,
                pair.inode,
                POINT_SCOPE_NAME,
                expected,
            )
            .await;
        let (_, rows) = proxy.end();
        let after_a = raw(&pair.a).await;
        let after_b = raw(&pair.b).await;
        let reader_closed = reader.close().await;
        let a_closed = pair.a.store.close().await;
        let b_closed = pair.b.store.close().await;
        proxy.shutdown().await;
        reader_closed.unwrap();
        a_closed.unwrap();
        b_closed.unwrap();

        let selected: Vec<_> = before_a
            .4
            .iter()
            .filter(|row| row.4 == pair.inode as i64)
            .collect();
        assert_eq!(
            selected.len(),
            2,
            "the exact selected name really has two physical candidates"
        );
        assert_eq!(selected[0].0, selected[1].0);
        assert_ne!(selected[0].1, selected[1].1);
        assert_eq!(selected[0].2, selected[1].2);
        assert_eq!(selected[0].3, POINT_SCOPE_NAME.as_bytes());
        assert_eq!(selected[0].3, selected[1].3);
        assert_eq!(before_b, decoy, "duplicate insertion stays inside A");
        assert_eq!(
            after_a, before_a,
            "duplicate refusal preserves every captured A row"
        );
        assert_eq!(after_b, before_b, "duplicate refusal preserves every B row");
        assert_eq!(
            rows, 2,
            "both exact A candidates are observed; B is excluded"
        );
        let entry = entry.expect("fresh authority survives duplicate selected cardinality");
        let observed_generation = u64::try_from(before_a.0.0).unwrap();
        assert_eq!(entry.generation(), observed_generation);
        let error = entry.into_inode_read(&audited).unwrap_err();
        if changed_generation {
            assert!(observed_generation > base.anchor.generation);
            assert!(
                error.is(ErrorCode::Eagain),
                "generation wins over duplicate cardinality"
            );
        } else {
            assert_eq!(observed_generation, base.anchor.generation);
            assert!(
                error.is(ErrorCode::Eio),
                "duplicate exact candidates cannot be admitted"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_root_entry_recovers_one_session_after_cancellation() {
    use mount_rs_tidb::{TidbPoolContext, TidbStorageOptions};
    use std::time::Duration;
    use tokio::time::timeout;

    const BOUND: Duration = Duration::from_secs(10);
    let f = fixture(true).await;
    let inode = create(&f, POINT_SCOPE_NAME).await;
    let initial = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (_, _, old_audited) = initial.clone().into_validated_namespace().unwrap();
    let original = &initial.guards[&inode];
    let expected =
        CompactFileExpectation::from_structure(&old_audited, original.identity, &original.node)
            .unwrap();
    let root = initial.anchor.root;

    // Capture an independent publication before interruption. Both the selected
    // body and root entries change, so a retained statement view cannot pass.
    let mut fresh_namespace = initial.namespace().unwrap();
    fresh_namespace
        .nodes
        .get_mut(&inode)
        .unwrap()
        .stats
        .mtime_ms += 17;
    add_file(&mut fresh_namespace, "root-after-cancel");
    let fresh_delta =
        CompactStructuralDelta::capture(&initial, &fresh_namespace, StructuralScope::Full).unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    proxy.begin();
    let context = TidbPoolContext::new(&proxy.url, 1).unwrap();
    let reader = context
        .metadata(TidbStorageOptions::new(&f.key))
        .await
        .unwrap();
    let warm_first = timeout(
        BOUND,
        reader.read_compact_root_entry(f.backing, root, inode, POINT_SCOPE_NAME, expected),
    )
    .await;
    let warm_second = timeout(
        BOUND,
        reader.read_compact_root_entry(f.backing, root, inode, POINT_SCOPE_NAME, expected),
    )
    .await;
    let (warm_queries, _) = proxy.end();
    let before_cancel = timeout(BOUND, raw(&f)).await;

    proxy.begin();
    proxy.trace.pause_guards.store(true, Ordering::SeqCst);
    let interrupted_reader = reader.clone();
    let backing = f.backing;
    let interrupted_authority = CompactAuthority::from_anchor(&initial.anchor).unwrap();
    let interrupted_file = original.clone();
    let reading = tokio::spawn(async move {
        let expected = CompactFileExpectation::from_authority(
            &interrupted_authority,
            interrupted_file.identity,
            &interrupted_file.node,
        )
        .unwrap();
        interrupted_reader
            .read_compact_root_entry(backing, root, inode, POINT_SCOPE_NAME, expected)
            .await
    });
    // Existing proxy handling holds this prepared EXECUTE before forwarding it.
    let reached = timeout(BOUND, proxy.trace.reached.notified()).await;
    reading.abort();
    let canceled = reading.await;
    proxy.trace.resume.notify_one();
    let after_cancel_before_publication = timeout(BOUND, raw(&f)).await;
    let published = timeout(BOUND, f.store.publish_compact_structure(&fresh_delta)).await;
    let fresh_snapshot = timeout(BOUND, f.store.load_compact_snapshot(f.backing)).await;
    let before_recovery = timeout(BOUND, raw(&f)).await;
    let recovered_stale = timeout(
        BOUND,
        reader.read_compact_root_entry(f.backing, root, inode, POINT_SCOPE_NAME, expected),
    )
    .await;
    let recovered_fresh = timeout(
        BOUND,
        reader.read_compact_root_entry(f.backing, root, inode, POINT_SCOPE_NAME, expected),
    )
    .await;
    let (cancel_queries, _) = proxy.end();
    let after_recovery = timeout(BOUND, raw(&f)).await;

    // Settle every lifecycle before checking semantic outcomes, as the FILE
    // cancellation control does. The shared context owns pool disconnect.
    let reader_closed = timeout(BOUND, reader.close()).await;
    let context_closed = timeout(BOUND, context.close()).await;
    let fixture_closed = timeout(BOUND, f.store.close()).await;
    let proxy_closed = timeout(BOUND, proxy.shutdown()).await;
    reader_closed
        .expect("root-entry reader close completed")
        .unwrap();
    context_closed
        .expect("root-entry shared context disconnected")
        .unwrap();
    fixture_closed
        .expect("independent fixture disconnected")
        .unwrap();
    proxy_closed.expect("root-entry proxy listener and relays settled");

    assert_eq!(
        before_cancel.expect("pre-cancel frame completed"),
        after_cancel_before_publication.expect("post-cancel frame completed"),
        "canceled ROOT read preserves all captured rows"
    );
    assert_eq!(
        before_recovery.expect("pre-recovery frame completed"),
        after_recovery.expect("post-recovery frame completed"),
        "recovered ROOT reads preserve all captured fresh rows"
    );
    let joined = |sql: &String| {
        sql.starts_with("SELECT m.revision,m.write_mode,m.backing_id,m.owner,")
            && sql.contains(" LEFT JOIN mount_rs_tidb_compact_guards AS r ")
            && sql.contains(" LEFT JOIN mount_rs_tidb_compact_guards AS f ")
    };
    let session_sets = |queries: &[String]| {
        queries
            .iter()
            .filter(|sql| sql.as_str() == "SET SESSION autocommit=1")
            .count()
    };
    let session_checks = |queries: &[String]| {
        queries
            .iter()
            .filter(|sql| sql.as_str() == "SELECT @@SESSION.autocommit")
            .count()
    };
    assert_eq!(session_sets(&warm_queries), 1);
    assert_eq!(session_checks(&warm_queries), 1);
    assert_eq!(warm_queries.iter().filter(|sql| joined(sql)).count(), 2);
    for read in [warm_first, warm_second] {
        let read = read.expect("warm ROOT cap-one checkout completed").unwrap();
        assert_eq!(read.generation(), initial.anchor.generation);
        let CompactInodeRead::Loaded(loaded) = read.into_inode_read(&old_audited).unwrap() else {
            panic!("warm ROOT read must contain the complete selected guard");
        };
        assert_eq!(loaded.guard, *original);
    }
    reached.expect("ROOT prepared EXECUTE reached controlled pause");
    assert!(canceled.unwrap_err().is_cancelled());
    let publication = published
        .expect("independent publication completed")
        .unwrap();
    assert!(publication.anchor.generation > initial.anchor.generation);
    let expected_guard = &publication.upserts[&inode];
    assert_ne!(expected_guard, original);
    assert_eq!(expected_guard.node, fresh_namespace.nodes[&inode]);
    let fresh_snapshot = fresh_snapshot
        .expect("fresh complete snapshot completed")
        .unwrap();
    assert_eq!(fresh_snapshot.anchor, publication.anchor);
    assert_eq!(fresh_snapshot.guards[&inode], *expected_guard);
    assert_eq!(
        serde_json::to_value(fresh_snapshot.namespace().unwrap()).unwrap(),
        serde_json::to_value(&fresh_namespace).unwrap()
    );
    let (_, _, fresh_audited) = fresh_snapshot.into_validated_namespace().unwrap();
    let stale = recovered_stale
        .expect("ROOT recovered without cap-one starvation")
        .unwrap();
    assert_eq!(stale.generation(), publication.anchor.generation);
    assert!(
        stale
            .into_inode_read(&old_audited)
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    // ROOT admission consumes the receipt. A second independent read proves
    // complete fresh-body admission against the independent complete snapshot.
    let fresh = recovered_fresh
        .expect("second ROOT recovery completed")
        .unwrap();
    assert_eq!(fresh.generation(), publication.anchor.generation);
    let CompactInodeRead::Loaded(loaded) = fresh.into_inode_read(&fresh_audited).unwrap() else {
        panic!("recovered ROOT read must expose the complete fresh selected guard");
    };
    assert_eq!(&loaded.guard, expected_guard);
    assert_eq!(cancel_queries.iter().filter(|sql| joined(sql)).count(), 3);
    assert_eq!(
        session_sets(&cancel_queries),
        session_checks(&cancel_queries)
    );
    assert!(
        session_sets(&cancel_queries) <= 1,
        "safe reuse or one newly verified replacement is permitted after abort"
    );
}

#[derive(Debug)]
struct PointScopePlanRow {
    id: String,
    depth: usize,
    actual_rows: f64,
    task: String,
    access: String,
    execution: String,
}

fn point_scope_plan_rows(rows: Vec<mysql_async::Row>) -> Vec<PointScopePlanRow> {
    assert!(
        !rows.is_empty() && rows.len() <= 128,
        "bounded actual TiDB plan required"
    );
    rows.into_iter()
        .map(|row| {
            let id: String = row.get("id").expect("TiDB EXPLAIN ANALYZE id column");
            let actual: String = row
                .get("actRows")
                .expect("TiDB EXPLAIN ANALYZE actRows column");
            let task: String = row.get("task").expect("TiDB EXPLAIN ANALYZE task column");
            let access: String = row
                .get("access object")
                .expect("TiDB EXPLAIN ANALYZE access object column");
            let execution: String = row
                .get("execution info")
                .expect("TiDB EXPLAIN ANALYZE execution info column");
            assert!(id.len() + actual.len() + task.len() + access.len() + execution.len() <= 65536);
            let start = id
                .find(|c: char| c.is_ascii_alphabetic())
                .expect("actual plan operator name");
            let depth = id[..start].chars().count();
            let actual_rows: f64 = actual.parse().expect("actual plan row count is numeric");
            assert!(actual_rows.is_finite() && actual_rows >= 0.0);
            PointScopePlanRow {
                id,
                depth,
                actual_rows,
                task,
                access,
                execution,
            }
        })
        .collect()
}

fn point_scope_plan_params(sql: &str, f: &Fixture, root: u64, inode: u64) -> mysql_async::Params {
    use mysql_async::Value;
    use sha2::{Digest, Sha256};
    let volume = || Value::Bytes(f.key.as_bytes().to_vec());
    let root = || Value::Int(i64::try_from(root).unwrap());
    let file = || Value::Int(i64::try_from(inode).unwrap());
    let hash = || Value::Bytes(Sha256::digest(POINT_SCOPE_NAME.as_bytes()).to_vec());
    let name = || Value::Bytes(POINT_SCOPE_NAME.as_bytes().to_vec());
    let values = match sql.bytes().filter(|byte| *byte == b'?').count() {
        3 => vec![file(), file(), volume()],
        5 => vec![volume(), file(), volume(), file(), volume()],
        8 => vec![
            root(),
            root(),
            root(),
            hash(),
            name(),
            file(),
            file(),
            volume(),
        ],
        13 => vec![
            volume(),
            root(),
            volume(),
            root(),
            volume(),
            root(),
            hash(),
            name(),
            volume(),
            file(),
            volume(),
            file(),
            volume(),
        ],
        _ => panic!("unknown production point parameter shape"),
    };
    mysql_async::Params::Positional(values)
}

fn point_scope_production_sql(queries: &[String]) -> &str {
    let selected: Vec<_> = queries
        .iter()
        .filter(|sql| sql.starts_with("SELECT "))
        .collect();
    assert_eq!(
        selected.len(),
        1,
        "one actual production point SELECT: {queries:?}"
    );
    selected[0]
}

fn point_scope_processed_keys(row: &PointScopePlanRow) -> Vec<u64> {
    row.execution
        .split("total_process_keys:")
        .skip(1)
        .map(|suffix| {
            let digits: String = suffix
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits
                .parse()
                .expect("TiDB scan_detail total_process_keys is numeric")
        })
        .collect()
}

fn point_scope_assert_guard_points(plan: &[PointScopePlanRow], expected: usize) {
    let accesses: Vec<_> = plan
        .iter()
        .filter(|row| {
            row.access
                .starts_with("table:mount_rs_tidb_compact_guards,")
                || matches!(row.access.as_str(), "table:g" | "table:r" | "table:f")
        })
        .collect();
    assert_eq!(
        accesses.len(),
        expected,
        "every selected guard access must be visible: {plan:?}"
    );
    for row in accesses {
        assert!(
            row.id.contains("Point_Get"),
            "selected guard must use an actual Point_Get: {row:?}"
        );
        assert_eq!(
            row.actual_rows, 1.0,
            "one actual selected guard row: {row:?}"
        );
    }
}

fn point_scope_assert_bounded_dentry(plan: &[PointScopePlanRow]) {
    let indexes: Vec<_> = plan
        .iter()
        .filter(|row| row.access.starts_with("table:d, index:name_lookup("))
        .collect();
    assert_eq!(
        indexes.len(),
        1,
        "one actual name_lookup access required: {plan:?}"
    );
    let index = indexes[0];
    assert!(
        index.id.contains("IndexRangeScan"),
        "actual name index range scan required: {index:?}"
    );
    assert_eq!(
        index.actual_rows, 1.0,
        "the selected hash range emits one index entry"
    );
    assert_eq!(
        point_scope_processed_keys(index),
        vec![1],
        "one processed name index entry"
    );
    let tables: Vec<_> = plan
        .iter()
        .enumerate()
        .filter(|(_, row)| row.access == "table:d")
        .collect();
    assert_eq!(
        tables.len(),
        1,
        "one actual selected dentry table access: {plan:?}"
    );
    let (position, table) = tables[0];
    assert!(
        table.id.contains("TableRowIDScan"),
        "dentry table access must use the selected row ID: {table:?}"
    );
    assert_eq!(
        table.actual_rows, 1.0,
        "one actual selected dentry table row"
    );
    // TiDB attaches table scan_detail to the coprocessor Selection above its
    // TableRowIDScan. Find an actual ancestor, rather than relying on plan IDs.
    let mut depth = table.depth;
    let mut table_scan = None;
    for row in plan[..=position].iter().rev() {
        if row.depth < depth || std::ptr::eq(row, table) {
            depth = row.depth;
            if row.task.starts_with("cop[") && !point_scope_processed_keys(row).is_empty() {
                table_scan = Some(row);
                break;
            }
        }
    }
    let table_scan = table_scan.expect("actual table-side coprocessor scan_detail required");
    assert_eq!(
        point_scope_processed_keys(table_scan),
        vec![1],
        "one processed selected dentry table row"
    );
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_production_points_use_bounded_access_with_1000_siblings() {
    let f = fixture(true).await;
    let empty = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut namespace = empty.namespace().unwrap();
    let inode = add_file(&mut namespace, POINT_SCOPE_NAME);
    for index in 1..1000 {
        add_file(&mut namespace, &format!("access-sibling-{index}"));
    }
    let delta = CompactStructuralDelta::capture(&empty, &namespace, StructuralScope::Full).unwrap();
    f.store.publish_compact_structure(&delta).await.unwrap();
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (_, _, audited) = base.clone().into_validated_namespace().unwrap();
    let guard = &base.guards[&inode];
    let expected =
        CompactFileExpectation::from_structure(&audited, guard.identity, &guard.node).unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    let (warm_file, _, warm_entry, _) =
        point_scope_observe(&reader, &proxy, &f, base.anchor.root, inode, expected).await;
    warm_file.unwrap();
    warm_entry.unwrap();
    let before = raw(&f).await;
    proxy.begin();
    let file = reader.read_compact_file(f.backing, inode, expected).await;
    let (file_queries, file_rows) = proxy.end();
    proxy.begin();
    let entry = reader
        .read_compact_root_entry(
            f.backing,
            base.anchor.root,
            inode,
            POINT_SCOPE_NAME,
            expected,
        )
        .await;
    let (root_queries, root_rows) = proxy.end();
    let file_sql = point_scope_production_sql(&file_queries);
    let root_sql = point_scope_production_sql(&root_queries);
    let pool = Pool::from_url(&f.url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    for sql in [
        "SET SESSION autocommit=1",
        "SET SESSION tidb_txn_mode='pessimistic'",
        "SET SESSION transaction_isolation='REPEATABLE-READ'",
    ] {
        connection.query_drop(sql).await.unwrap();
    }
    // These are diagnostic executions of the exact production prepared SQL,
    // with known fixture parameters. They establish access properties, not the
    // measured SELECT's plan, elapsed-time budget, physical IOPS or capacity.
    let file_plan: mount_rs_core::Result<Vec<mysql_async::Row>> = connection
        .exec(
            format!("EXPLAIN ANALYZE {file_sql}"),
            point_scope_plan_params(file_sql, &f, base.anchor.root, inode),
        )
        .await
        .map_err(mount_rs_core::backend_error);
    let root_plan: mount_rs_core::Result<Vec<mysql_async::Row>> = connection
        .exec(
            format!("EXPLAIN ANALYZE {root_sql}"),
            point_scope_plan_params(root_sql, &f, base.anchor.root, inode),
        )
        .await
        .map_err(mount_rs_core::backend_error);
    drop(connection);
    let plan_pool_closed = pool.disconnect().await;
    let after = raw(&f).await;
    let reader_closed = reader.close().await;
    let fixture_closed = f.store.close().await;
    proxy.shutdown().await;
    plan_pool_closed.unwrap();
    reader_closed.unwrap();
    fixture_closed.unwrap();

    assert_eq!(base.guards.len(), 1001);
    assert_eq!(before.4.len(), 1000);
    assert_eq!(
        after, before,
        "actual point and diagnostic reads preserve every captured row"
    );
    assert_eq!(file_rows, 1);
    assert_eq!(root_rows, 1);
    assert_eq!(
        serde_json::to_value(base.namespace().unwrap()).unwrap(),
        serde_json::to_value(&namespace).unwrap()
    );
    let file = file.unwrap();
    file.validate_expectation(expected).unwrap();
    point_scope_assert_owned_file(file.into_inode_read().unwrap(), guard);
    let CompactInodeRead::Loaded(loaded) = entry.unwrap().into_inode_read(&audited).unwrap() else {
        panic!("actual production ROOT read must contain the complete selected guard");
    };
    assert_eq!(loaded.guard, *guard);
    let file_plan = point_scope_plan_rows(file_plan.unwrap());
    let root_plan = point_scope_plan_rows(root_plan.unwrap());
    point_scope_assert_guard_points(&file_plan, 1);
    point_scope_assert_guard_points(&root_plan, 2);
    point_scope_assert_bounded_dentry(&root_plan);
}
