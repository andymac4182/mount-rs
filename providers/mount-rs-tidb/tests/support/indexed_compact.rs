//! Actual SQL controls included by the compact integration test crate.
use super::*;

fn assert_point_statement(queries: &[String], root_entry: bool) {
    let selects: Vec<_> = queries
        .iter()
        .filter(|sql| sql.starts_with("SELECT "))
        .collect();
    assert_eq!(
        selects.len(),
        1,
        "one coherent point statement: {queries:?}"
    );
    let sql = selects[0];
    assert!(sql.contains("mount_rs_tidb_metadata AS m"));
    assert!(sql.contains("LEFT JOIN mount_rs_tidb_compact_members"));
    assert!(sql.contains("LEFT JOIN mount_rs_tidb_compact_guards"));
    if root_entry {
        assert!(sql.contains("LEFT JOIN mount_rs_tidb_compact_dentries"));
        assert!(sql.contains("d.name_hash=?"));
        assert!(
            sql.contains("d.name=?"),
            "hash candidates must use exact binary name equality"
        );
    } else {
        assert!(!sql.contains("mount_rs_tidb_compact_dentries"));
    }
    assert!(!sql.contains("FOR UPDATE"));
    assert!(queries.iter().all(|sql| {
        !sql.starts_with("SET ")
            && !sql.starts_with("START TRANSACTION")
            && !sql.eq_ignore_ascii_case("BEGIN")
            && !sql.eq_ignore_ascii_case("COMMIT")
            && !sql.eq_ignore_ascii_case("ROLLBACK")
    }));
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_selected_file_and_root_entry_use_one_nonlocking_point_statement() {
    let f = fixture(true).await;
    let inode = create(&f, "point-selected").await;
    create(&f, "unrequested-sibling").await;
    let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (_, _, audited) = snapshot.clone().into_validated_namespace().unwrap();
    let file = &snapshot.guards[&inode];
    let expected =
        CompactFileExpectation::from_structure(&audited, file.identity, &file.node).unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    assert_eq!(
        reader.compact_point_read_capability(),
        CompactPointReadCapability::Supported
    );
    reader
        .read_compact_file(f.backing, inode, expected)
        .await
        .unwrap();
    reader
        .read_compact_root_entry(
            f.backing,
            snapshot.anchor.root,
            inode,
            "point-selected",
            expected,
        )
        .await
        .unwrap();

    proxy.begin();
    let read = reader
        .read_compact_file(f.backing, inode, expected)
        .await
        .unwrap();
    let (file_queries, file_rows) = proxy.end();
    read.validate_expectation(expected).unwrap();
    let generation = read.generation();
    let outcome = read.into_inode_read().unwrap();
    proxy.begin();
    let entry = reader
        .read_compact_root_entry(
            f.backing,
            snapshot.anchor.root,
            inode,
            "point-selected",
            expected,
        )
        .await
        .unwrap();
    let (entry_queries, entry_rows) = proxy.end();
    let entry_generation = entry.generation();
    let entry_outcome = entry.into_inode_read(&audited).unwrap();
    reader.close().await.unwrap();
    f.store.close().await.unwrap();
    proxy.shutdown().await;

    assert_point_statement(&file_queries, false);
    assert_point_statement(&entry_queries, true);
    assert_eq!(file_rows, 1);
    assert_eq!(
        entry_rows, 1,
        "root header, exact dentry and file share one row"
    );
    assert_eq!(generation, snapshot.anchor.generation);
    assert_eq!(entry_generation, snapshot.anchor.generation);
    let CompactInodeRead::Unchanged(checked) = outcome else {
        panic!("fresh complete file bytes must certify the borrowed unchanged expectation");
    };
    assert_eq!(checked.identity(), file.identity);
    assert!(
        checked.into_verified_root().is_none(),
        "scoped file evidence cannot certify the graph"
    );
    let CompactInodeRead::Loaded(entry) = entry_outcome else {
        panic!("root-entry result is scoped owned file evidence")
    };
    assert_eq!(entry.guard, *file);
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_point_name_hash_candidates_require_exact_binary_names() {
    let f = fixture(true).await;
    let name = format!("exact-'\\-{}", "雪".repeat(4096));
    let inode = create(&f, &name).await;
    let sibling = create(&f, "other-candidate").await;
    let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (_, _, audited) = snapshot.clone().into_validated_namespace().unwrap();
    let file = &snapshot.guards[&inode];
    let expected =
        CompactFileExpectation::from_structure(&audited, file.identity, &file.node).unwrap();
    let before = raw(&f).await;
    let selected_hash = &before.4.iter().find(|row| row.4 == inode as i64).unwrap().2;
    let pool = Pool::from_url(&f.url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_compact_dentries SET name_hash=? WHERE volume_key=? AND inode=?",
            (selected_hash, &f.key, sibling as i64),
        )
        .await
        .unwrap();
    drop(connection);
    pool.disconnect().await.unwrap();
    let damaged = raw(&f).await;
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    reader
        .read_compact_root_entry(f.backing, snapshot.anchor.root, inode, &name, expected)
        .await
        .unwrap();
    proxy.begin();
    let entry = reader
        .read_compact_root_entry(f.backing, snapshot.anchor.root, inode, &name, expected)
        .await
        .unwrap();
    let (queries, rows) = proxy.end();
    let outcome = entry.into_inode_read(&audited).unwrap();
    let full = f.store.load_compact_snapshot(f.backing).await;
    let after = raw(&f).await;
    reader.close().await.unwrap();
    f.store.close().await.unwrap();
    proxy.shutdown().await;
    assert_point_statement(&queries, true);
    assert_eq!(
        rows, 1,
        "hash bucket candidate cannot create a second selected result"
    );
    let CompactInodeRead::Loaded(selected) = outcome else {
        panic!("selected file required")
    };
    assert_eq!(selected.guard, *file);
    assert!(
        full.is_err(),
        "Full audit must reject the deliberately inconsistent sibling hash"
    );
    assert_eq!(
        after, damaged,
        "point and audit reads preserve all physical rows"
    );
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_point_streaming_fallback_decodes_the_same_fresh_body() {
    let f = fixture(true).await;
    let inode = create(&f, "point-streaming").await;
    let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let authority = CompactAuthority::from_anchor(&snapshot.anchor).unwrap();
    let file = &snapshot.guards[&inode];
    let expected =
        CompactFileExpectation::from_authority(&authority, file.identity, &file.node).unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    reader
        .read_compact_file(f.backing, inode, expected)
        .await
        .unwrap();
    let positional =
        serde_json::to_string(&serde_json::json!([&file.node.stats, &file.node.data])).unwrap();
    let mut changed = file.node.clone();
    changed.stats.mtime_ms += 19;
    for (encoded, reference) in [
        (positional, &file.node),
        (serde_json::to_string(&changed).unwrap(), &changed),
    ] {
        let pool = Pool::from_url(&f.url).unwrap();
        let mut connection = pool.get_conn().await.unwrap();
        connection
            .exec_drop(
                "UPDATE mount_rs_tidb_compact_guards SET node=? WHERE volume_key=? AND inode=?",
                (&encoded, &f.key, inode as i64),
            )
            .await
            .unwrap();
        drop(connection);
        pool.disconnect().await.unwrap();
        let before = raw(&f).await;
        proxy.begin();
        let receipt = reader
            .read_compact_file(f.backing, inode, expected)
            .await
            .unwrap();
        let (queries, rows) = proxy.end();
        receipt.validate_expectation(expected).unwrap();
        let CompactInodeRead::Loaded(loaded) = receipt.into_inode_read().unwrap() else {
            panic!(
                "alternate encoding or unequal complete body must decode the fresh returned row"
            );
        };
        assert_eq!(loaded.guard.identity, file.identity);
        assert_eq!(&loaded.guard.node, reference);
        assert_eq!(rows, 1);
        assert_point_statement(&queries, false);
        assert_eq!(raw(&f).await, before);
    }
    reader.close().await.unwrap();
    f.store.close().await.unwrap();
    proxy.shutdown().await;
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_point_generation_precedes_missing_members_and_malformed_guards() {
    for sql in [
        "DELETE FROM mount_rs_tidb_compact_members WHERE volume_key=? AND inode=2",
        "DELETE FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=2",
        "UPDATE mount_rs_tidb_compact_guards SET node='{}' WHERE volume_key=? AND inode=2",
    ] {
        let f = fixture(true).await;
        let inode = create(&f, "generation-selected").await;
        let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let (_, _, audited) = base.clone().into_validated_namespace().unwrap();
        let file = &base.guards[&inode];
        let expected =
            CompactFileExpectation::from_structure(&audited, file.identity, &file.node).unwrap();
        create(&f, "new-generation").await;
        corrupt(&f, sql).await;
        let before = raw(&f).await;
        let selected = f
            .store
            .read_compact_file(f.backing, inode, expected)
            .await
            .unwrap();
        let root_entry = f
            .store
            .read_compact_root_entry(
                f.backing,
                base.anchor.root,
                inode,
                "generation-selected",
                expected,
            )
            .await
            .unwrap();
        let after = raw(&f).await;
        f.store.close().await.unwrap();
        assert!(selected.generation() > base.anchor.generation);
        assert!(
            selected
                .validate_expectation(expected)
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        assert!(
            selected.into_inode_read().is_err(),
            "selected contradiction remains deferred inside the receipt"
        );
        assert!(root_entry.generation() > base.anchor.generation);
        assert!(
            root_entry
                .into_inode_read(&audited)
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        assert_eq!(after, before);
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_indexed_authority_and_root_headers_are_bounded_without_graph_arrays() {
    let f = fixture(true).await;
    let pool = Pool::from_url(&f.url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    let tables: Option<u64> = connection
        .query_first(
            "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema=DATABASE() AND table_name IN ('mount_rs_tidb_compact_members','mount_rs_tidb_compact_dentries')",
        )
        .await
        .unwrap();
    drop(connection);
    pool.disconnect().await.unwrap();
    assert_eq!(
        tables,
        Some(2),
        "normalized membership and dentry tables must exist"
    );

    let empty = raw(&f).await;
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut namespace = base.namespace().unwrap();
    for n in 0..64 {
        add_file(&mut namespace, &format!("header-shape-{n}"));
    }
    let delta = CompactStructuralDelta::capture(&base, &namespace, StructuralScope::Full).unwrap();
    f.store.publish_compact_structure(&delta).await.unwrap();
    let populated = raw(&f).await;
    let fresh = f.store.load_compact_snapshot(f.backing).await.unwrap();
    f.store.close().await.unwrap();

    assert_eq!(
        serde_json::to_value(fresh.namespace().unwrap()).unwrap(),
        serde_json::to_value(&namespace).unwrap(),
    );
    assert_eq!(populated.3.len(), namespace.nodes.len());
    assert_eq!(populated.4.len(), 64);
    let authority = populated.0.6.as_ref().unwrap();
    let empty_authority = empty.0.6.as_ref().unwrap();
    assert!(!authority.contains("\"members\""));
    assert!(!authority.contains("header-shape-"));
    assert!(authority.len() <= empty_authority.len() + 32);
    let root = populated
        .1
        .iter()
        .find(|row| row.0 == namespace.root as i64)
        .unwrap();
    let empty_root = empty
        .1
        .iter()
        .find(|row| row.0 == namespace.root as i64)
        .unwrap();
    assert!(!root.4.contains("\"entries\""));
    assert!(!root.4.contains("header-shape-"));
    assert!(root.4.len() <= empty_root.4.len() + 32);
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_indexed_long_binary_names_rename_append_and_full_reorder_survive_reopen() {
    let f = fixture(true).await;
    let upper = create(&f, "binary-Ä").await;
    let lower = create(&f, "binary-ä").await;
    let long_name = format!("long-'\\-{}", "雪".repeat(4096));
    let long = create(&f, &long_name).await;
    let initial = raw(&f).await;
    assert_eq!(initial.4.len(), 3);
    assert_eq!(initial.4[0].3, "binary-Ä".as_bytes());
    assert_eq!(initial.4[1].3, "binary-ä".as_bytes());
    assert_eq!(initial.4[2].3, long_name.as_bytes());
    assert_ne!(initial.4[0].4, initial.4[1].4);

    let transition =
        root_file_rename_transition(&f, &f.store, upper, "binary-Ä", "renamed-Ä").await;
    f.store
        .publish_compact_structure(transition.delta())
        .await
        .unwrap();
    let renamed = raw(&f).await;
    assert_eq!(renamed.3, initial.3, "rename preserves membership");
    for inode in [lower, long] {
        let before = initial.4.iter().find(|row| row.4 == inode as i64).unwrap();
        let after = renamed.4.iter().find(|row| row.4 == inode as i64).unwrap();
        assert_eq!(
            after, before,
            "surviving name retains its ordinal and bytes"
        );
    }
    let appended = renamed.4.iter().find(|row| row.4 == upper as i64).unwrap();
    assert_eq!(appended.3, "renamed-Ä".as_bytes());
    assert!(appended.1 > initial.4.last().unwrap().1);

    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut namespace = base.namespace().unwrap();
    let NodeData::Directory { entries } =
        &mut namespace.nodes.get_mut(&namespace.root).unwrap().data
    else {
        panic!("root directory required");
    };
    entries.rotate_right(1);
    let expected_entries = entries.clone();
    let delta = CompactStructuralDelta::capture(&base, &namespace, StructuralScope::Full).unwrap();
    f.store.publish_compact_structure(&delta).await.unwrap();
    let after = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let reordered = raw(&f).await;
    assert_eq!(
        reordered
            .4
            .iter()
            .map(|row| (row.3.clone(), row.4 as u64))
            .collect::<Vec<_>>(),
        expected_entries
            .iter()
            .map(|entry| (entry.name.as_bytes().to_vec(), entry.inode))
            .collect::<Vec<_>>(),
        "physical ordinal order reconstructs the requested Full directory order",
    );
    f.store.close().await.unwrap();
    let fresh = TidbMetadataStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let reopened = fresh.load_compact_snapshot(f.backing).await.unwrap();
    fresh.close().await.unwrap();
    assert_eq!(reopened, after);
    assert_eq!(reordered.3, initial.3);
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_indexed_full_audit_rejects_member_guard_and_dentry_corruption_without_mutation() {
    let cases = [
        (
            "missing member",
            "DELETE FROM mount_rs_tidb_compact_members WHERE volume_key=? AND inode=2",
        ),
        (
            "extra member",
            "INSERT INTO mount_rs_tidb_compact_members(volume_key,inode) VALUES(?,42)",
        ),
        (
            "missing guard",
            "DELETE FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=2",
        ),
        (
            "extra guard",
            "INSERT INTO mount_rs_tidb_compact_guards(volume_key,inode,incarnation,epoch,revision,node) SELECT volume_key,42,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=2",
        ),
        (
            "missing dentry",
            "DELETE FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND inode=2",
        ),
        (
            "dangling child",
            "UPDATE mount_rs_tidb_compact_dentries SET inode=42 WHERE volume_key=? AND inode=2",
        ),
        (
            "foreign parent",
            "UPDATE mount_rs_tidb_compact_dentries SET parent=42 WHERE volume_key=? AND inode=2",
        ),
        (
            "wrong name hash",
            "UPDATE mount_rs_tidb_compact_dentries SET name_hash=UNHEX(REPEAT('00',32)) WHERE volume_key=? AND inode=2",
        ),
        (
            "duplicate name",
            "INSERT INTO mount_rs_tidb_compact_dentries(volume_key,parent,ordinal,name_hash,name,inode) SELECT volume_key,parent,42,name_hash,name,inode FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND inode=2",
        ),
        (
            "out of bounds ordinal",
            "UPDATE mount_rs_tidb_compact_dentries SET ordinal=9223372036854775807 WHERE volume_key=? AND inode=2",
        ),
    ];
    for (name, sql) in cases {
        let f = fixture(true).await;
        create(&f, "audited-child").await;
        let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let delta = CompactStructuralDelta::capture(
            &base,
            &base.namespace().unwrap(),
            StructuralScope::Full,
        )
        .unwrap();
        corrupt(&f, sql).await;
        let before = raw(&f).await;
        let loaded = f.store.load_compact_snapshot(f.backing).await;
        let published = f.store.publish_compact_structure(&delta).await;
        let after = raw(&f).await;
        f.store.close().await.unwrap();
        assert!(loaded.is_err(), "Full snapshot accepted {name}");
        assert!(published.is_err(), "Full publication accepted {name}");
        assert_eq!(
            after, before,
            "{name}: all physical tables must remain unchanged"
        );
    }
}

fn decrement_json_counter(value: &mut serde_json::Value, name: &str) -> bool {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(counter) = fields.get_mut(name) {
                let count = counter.as_u64().unwrap();
                *counter = serde_json::json!(count.checked_sub(1).unwrap());
                true
            } else {
                fields
                    .values_mut()
                    .any(|field| decrement_json_counter(field, name))
            }
        }
        _ => false,
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_indexed_full_audit_checks_actual_members_and_dentries_against_header_counts() {
    for authority in [true, false] {
        let f = fixture(true).await;
        create(&f, "one").await;
        create(&f, "two").await;
        let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let delta = CompactStructuralDelta::capture(
            &base,
            &base.namespace().unwrap(),
            StructuralScope::Full,
        )
        .unwrap();
        let original = raw(&f).await;
        let body = if authority {
            original.0.6.as_ref().unwrap()
        } else {
            &original
                .1
                .iter()
                .find(|row| row.0 == base.anchor.root as i64)
                .unwrap()
                .4
        };
        let mut value: serde_json::Value = serde_json::from_str(body).unwrap();
        let field = if authority {
            "members_count"
        } else {
            "entry_count"
        };
        assert!(
            decrement_json_counter(&mut value, field),
            "physical tagged header must carry {field}"
        );
        let pool = Pool::from_url(&f.url).unwrap();
        let mut connection = pool.get_conn().await.unwrap();
        let replacement = serde_json::to_string(&value).unwrap();
        if authority {
            connection
                .exec_drop(
                    "UPDATE mount_rs_tidb_metadata SET namespace=? WHERE volume_key=?",
                    (&replacement, &f.key),
                )
                .await
                .unwrap();
        } else {
            connection
                .exec_drop(
                    "UPDATE mount_rs_tidb_compact_guards SET node=? WHERE volume_key=? AND inode=?",
                    (&replacement, &f.key, base.anchor.root as i64),
                )
                .await
                .unwrap();
        }
        drop(connection);
        pool.disconnect().await.unwrap();
        let damaged = raw(&f).await;
        let loaded = f.store.load_compact_snapshot(f.backing).await;
        let published = f.store.publish_compact_structure(&delta).await;
        let after = raw(&f).await;
        f.store.close().await.unwrap();
        assert!(loaded.is_err(), "Full snapshot accepted false {field}");
        assert!(
            published.is_err(),
            "Full publication accepted false {field}"
        );
        assert_eq!(after, damaged);
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_indexed_last_link_tombstone_layout_and_membership_survive_reopen() {
    let f = fixture(true).await;
    let file = create(&f, "detached").await;
    let selected = f.store.load_compact_inode(f.backing, file).await.unwrap();
    let mut body = selected.guard.node.clone();
    body.stats.size = 4;
    body.stats.blocks = 1;
    let NodeData::File(layout) = &mut body.data else {
        panic!("regular file required")
    };
    layout.extents = vec![mount_rs_core::storage::BlockExtent {
        file_offset: 0,
        block: mount_rs_core::storage::BlockId("detached-durable-reference".into()),
        block_offset: 0,
        length: 4,
    }];
    f.store
        .publish_compact_inode(
            f.backing,
            file,
            selected.generation,
            selected.guard.identity,
            body.clone(),
        )
        .await
        .unwrap();
    let transition = root_file_unlink_transition(&f, &f.store, file, "detached").await;
    f.store
        .publish_compact_structure(transition.delta())
        .await
        .unwrap();
    let before = raw(&f).await;
    let unlinked = f.store.load_compact_snapshot(f.backing).await.unwrap();
    f.store.close().await.unwrap();
    let fresh = TidbMetadataStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let reopened = fresh.load_compact_snapshot(f.backing).await.unwrap();
    fresh.close().await.unwrap();
    assert_eq!(reopened, unlinked);
    assert!(before.3.contains(&(file as i64)));
    assert!(!before.4.iter().any(|row| row.4 == file as i64));
    assert_eq!(reopened.guards[&file].node.stats.nlink, 0);
    assert_eq!(reopened.guards[&file].node.data, body.data);
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_oversized_dentry_is_rejected_before_any_table_dml() {
    let f = fixture(true).await;
    create(&f, "existing").await;
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut namespace = base.namespace().unwrap();
    let oversized_name = "packet-long-name".repeat(5000);
    assert!(oversized_name.len() > 65536);
    add_file(&mut namespace, &oversized_name);
    let delta =
        CompactStructuralDelta::capture(&base, &namespace, StructuralScope::FileCreate).unwrap();
    let before = raw(&f).await;
    let mut url = url::Url::parse(&f.url).unwrap();
    url.query_pairs_mut()
        .append_pair("max_allowed_packet", "65536");
    let proxy = compact_proxy::Proxy::new(url.as_str()).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    proxy.begin();
    let publication = writer.publish_compact_structure(&delta).await;
    let (queries, _) = proxy.end();
    let after = raw(&f).await;
    writer.close().await.unwrap();
    f.store.close().await.unwrap();
    proxy.shutdown().await;
    assert!(publication.unwrap_err().is(ErrorCode::Efbig));
    assert!(
        !queries.iter().any(|sql| sql.starts_with("UPDATE ")
            || sql.starts_with("INSERT ")
            || sql.starts_with("DELETE ")),
        "all encoded dentry and header parameters must be preflighted before the first DML"
    );
    assert_eq!(
        after, before,
        "authority, guards, members and dentries remain byte-identical"
    );
}

fn audited_create_proposal(base: &CompactSnapshot, name: &str) -> CompactRootFileCreate {
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

fn assert_no_structural_dml_or_commit(queries: &[String], complete_parent: bool) {
    let one_position = |matches: fn(&str) -> bool, label: &str| {
        let positions: Vec<_> = queries
            .iter()
            .enumerate()
            .filter_map(|(position, sql)| matches(sql).then_some(position))
            .collect();
        assert_eq!(
            positions.len(),
            1,
            "exactly one {label} must be captured: {queries:?}"
        );
        positions[0]
    };
    let started = one_position(
        |sql| sql.starts_with("START TRANSACTION"),
        "publication transaction",
    );
    let authority_lock = one_position(
        |sql| {
            sql.starts_with("SELECT owner, fence, expires,")
                && sql.contains("FROM mount_rs_tidb_metadata")
                && sql.contains("WHERE volume_key=?")
                && sql.contains("FOR UPDATE")
        },
        "locked authority read",
    );
    let authority = one_position(
        |sql| {
            sql == "SELECT revision,write_mode,backing_id,owner,fence,expires,namespace,delegation FROM mount_rs_tidb_metadata WHERE volume_key=?"
        },
        "fresh complete authority read",
    );
    let members = one_position(
        |sql| {
            sql == "SELECT inode FROM mount_rs_tidb_compact_members WHERE volume_key=? ORDER BY inode"
        },
        "complete member read",
    );
    assert!(
        started < authority_lock && authority_lock < authority && authority < members,
        "the complete member proof must follow the authority lock and fresh authority read"
    );
    if complete_parent {
        let parent = one_position(
            |sql| {
                sql == "SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=? ORDER BY inode FOR UPDATE"
            },
            "locked selected parent read",
        );
        let entries = one_position(
            |sql| {
                sql == "SELECT parent,ordinal,name_hash,name,inode FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND parent=? ORDER BY parent,ordinal FOR UPDATE"
            },
            "locked complete parent dentry read",
        );
        assert!(
            members < parent && parent < entries,
            "the fresh parent and complete dentry range must follow the complete member proof"
        );
    }
    assert!(
        !queries.iter().any(|sql| sql.starts_with("UPDATE ")
            || sql.starts_with("INSERT ")
            || sql.starts_with("DELETE ")
            || sql.eq_ignore_ascii_case("COMMIT")),
        "an audited proposal grants no freshness and must refuse before DML: {queries:?}"
    );
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_optimistic_create_rejects_unrequested_dentry_tamper() {
    use sha2::{Digest, Sha256};

    let f = fixture(true).await;
    create(&f, "unchanged-sibling").await;
    let sibling = create(&f, "unrequested-sibling").await;
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let proposal = audited_create_proposal(&base, "audited-create");
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    let capability = writer.compact_optimistic_create_capability();
    let original = raw(&f).await;
    let replacement = "changed-unrequested-sibling";
    let replacement_hash = Sha256::digest(replacement.as_bytes()).to_vec();
    let pool = Pool::from_url(&f.url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_compact_dentries SET name_hash=?,name=? WHERE volume_key=? AND parent=? AND inode=?",
            (
                &replacement_hash,
                replacement.as_bytes(),
                &f.key,
                base.anchor.root as i64,
                sibling as i64,
            ),
        )
        .await
        .unwrap();
    drop(connection);
    pool.disconnect().await.unwrap();
    let damaged = raw(&f).await;

    proxy.begin();
    let publication = writer.publish_compact_structure(proposal.delta()).await;
    let (queries, _) = proxy.end();
    // Settle every provider and relay before the fresh, independent raw observer.
    let writer_closed = writer.close().await;
    let fixture_closed = f.store.close().await;
    proxy.shutdown().await;
    let after = raw(&f).await;

    writer_closed.unwrap();
    fixture_closed.unwrap();
    assert_eq!(capability, CompactOptimisticCreateCapability::Supported);
    assert_eq!(
        damaged.0, original.0,
        "authority and generation are unchanged"
    );
    assert_eq!(
        damaged.1, original.1,
        "all guards and root header are unchanged"
    );
    assert_eq!(damaged.2, original.2);
    assert_eq!(damaged.3, original.3);
    let mut expected_entries = original.4.clone();
    let changed = expected_entries
        .iter_mut()
        .find(|entry| entry.4 == sibling as i64)
        .unwrap();
    changed.2 = replacement_hash;
    changed.3 = replacement.as_bytes().to_vec();
    assert_eq!(
        damaged.4, expected_entries,
        "only one valid sibling name/hash pair changes; count, ordinal and child stay fixed"
    );
    assert!(
        publication.unwrap_err().is(ErrorCode::Einval),
        "the complete fresh parent must differ from the audited captured body"
    );
    assert_no_structural_dml_or_commit(&queries, true);
    assert_eq!(
        after, damaged,
        "fresh observer retains every tampered row for the owned fixture cleanup"
    );
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_indexed_optimistic_create_rejects_equal_count_member_substitution() {
    let f = fixture(true).await;
    let gap = create(&f, "removed-gap").await;
    let sibling = create(&f, "unrequested-member").await;
    let before_removal = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut namespace = before_removal.namespace().unwrap();
    namespace.nodes.remove(&gap).unwrap();
    let NodeData::Directory { entries } =
        &mut namespace.nodes.get_mut(&namespace.root).unwrap().data
    else {
        panic!("root directory required")
    };
    entries.retain(|entry| entry.inode != gap);
    let removal =
        CompactStructuralDelta::capture(&before_removal, &namespace, StructuralScope::Full)
            .unwrap();
    f.store.publish_compact_structure(&removal).await.unwrap();
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert!(!base.anchor.members.contains(&gap));
    assert!(gap < base.anchor.next_inode);
    let proposal = audited_create_proposal(&base, "audited-create");
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    let capability = writer.compact_optimistic_create_capability();
    let original = raw(&f).await;
    let pool = Pool::from_url(&f.url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_compact_members SET inode=? WHERE volume_key=? AND inode=?",
            (gap as i64, &f.key, sibling as i64),
        )
        .await
        .unwrap();
    drop(connection);
    pool.disconnect().await.unwrap();
    let damaged = raw(&f).await;

    proxy.begin();
    let publication = writer.publish_compact_structure(proposal.delta()).await;
    let (queries, _) = proxy.end();
    let writer_closed = writer.close().await;
    let fixture_closed = f.store.close().await;
    proxy.shutdown().await;
    let after = raw(&f).await;

    writer_closed.unwrap();
    fixture_closed.unwrap();
    assert_eq!(capability, CompactOptimisticCreateCapability::Supported);
    assert_eq!(
        damaged.0, original.0,
        "authority and member count are unchanged"
    );
    assert_eq!(
        damaged.1, original.1,
        "all guards and root header are unchanged"
    );
    assert_eq!(damaged.2, original.2);
    assert_eq!(damaged.4, original.4, "all root dentries are unchanged");
    let mut expected_members = original.3.clone();
    *expected_members
        .iter_mut()
        .find(|member| **member == sibling as i64)
        .unwrap() = gap as i64;
    expected_members.sort_unstable();
    assert_eq!(damaged.3, expected_members);
    assert_eq!(damaged.3.len(), original.3.len());
    // This is a valid, equal-count anchor substitution, not an invalid bound
    // that could be detected by inspecting only the small authority envelope.
    let mut substituted = base.anchor.clone();
    substituted.members = damaged.3.iter().map(|member| *member as u64).collect();
    substituted.validate().unwrap();
    assert_ne!(substituted, base.anchor);
    assert!(
        publication.unwrap_err().is(ErrorCode::Eagain),
        "the complete fresh member IDs must differ from the audited captured anchor"
    );
    // Member equality can reject before affected-body validation. Do not
    // require later guard/dentry reads to prove this earlier noncommit.
    assert_no_structural_dml_or_commit(&queries, false);
    assert_eq!(
        after, damaged,
        "fresh observer retains every tampered row for the owned fixture cleanup"
    );
}
