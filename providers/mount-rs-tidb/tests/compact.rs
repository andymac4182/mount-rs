//! Owned actual-TiDB compact-layout correctness diagnostics, not capacity qualification.
#[path = "support/guard_fixture.rs"]
mod guard_fixture;

use mount_rs_core::ErrorCode;
use mount_rs_core::storage::{
    ConcurrentBackingId, DirectoryEntry, MetadataStore, Namespace, NodeData, compact::*,
};
use mount_rs_tidb::TidbMetadataStore;
use mysql_async::{Pool, prelude::Queryable};
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture {
    store: TidbMetadataStore,
    url: String,
    key: String,
    backing: ConcurrentBackingId,
}

fn root_namespace() -> Namespace {
    let mut ns = guard_fixture::namespace();
    ns.nodes.retain(|id, _| *id == ns.root);
    ns.nodes.get_mut(&ns.root).unwrap().data = NodeData::Directory { entries: vec![] };
    ns.next_inode = ns.root + 1;
    ns
}

fn compact_test_key(prefix: &str, pid: u32, nanos: u128) -> String {
    assert!(
        !prefix.is_empty()
            && prefix.len() <= 96
            && prefix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "compact test prefix must contain 1..=96 ASCII alphanumeric or hyphen bytes"
    );
    format!("{prefix}-{pid}-{nanos}")
}

#[test]
fn compact_test_keys_keep_legacy_default_and_reject_unsafe_owned_prefixes() {
    assert_eq!(
        compact_test_key("compact-tidb", 17, 42),
        "compact-tidb-17-42"
    );
    assert_eq!(compact_test_key("owned-4f1A", 17, 42), "owned-4f1A-17-42");
    assert!(compact_test_key(&"x".repeat(96), 17, 42).starts_with(&"x".repeat(96)));
    for prefix in [
        "",
        "has space",
        "wild%",
        "wild_",
        "slash/",
        "quote'",
        "雪",
        &"x".repeat(97),
    ] {
        assert!(
            std::panic::catch_unwind(|| compact_test_key(prefix, 17, 42)).is_err(),
            "invalid prefix was accepted: {prefix:?}"
        );
    }
}

async fn fixture(enroll: bool) -> Fixture {
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("explicit actual TiDB URL required");
    let prefix = match std::env::var("MOUNT_RS_TIDB_COMPACT_TEST_PREFIX") {
        Ok(prefix) => prefix,
        Err(std::env::VarError::NotPresent) => "compact-tidb".to_owned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("compact test prefix must be Unicode ASCII")
        }
    };
    let key = compact_test_key(
        &prefix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    );
    let store = TidbMetadataStore::connect_with_key(&url, &key)
        .await
        .unwrap();
    let backing = ConcurrentBackingId::from_bytes([0x65; 16]).unwrap();
    store.prepare_bound_concurrent_mode(backing).await.unwrap();
    store
        .publish_bound_if_revision(backing, 0, root_namespace())
        .await
        .unwrap();
    if enroll {
        store.prepare_compact_inode_mode(backing, 1).await.unwrap();
    }
    Fixture {
        store,
        url,
        key,
        backing,
    }
}

fn add_file(ns: &mut Namespace, name: &str) -> u64 {
    let id = ns.next_inode;
    let template = guard_fixture::namespace();
    let mut node = template.nodes[&2].clone();
    node.stats.ino = id;
    ns.nodes.insert(id, node);
    let NodeData::Directory { entries } = &mut ns.nodes.get_mut(&ns.root).unwrap().data else {
        panic!("root directory required")
    };
    entries.push(DirectoryEntry {
        name: name.into(),
        inode: id,
    });
    ns.next_inode += 1;
    id
}

async fn create(f: &Fixture, name: &str) -> u64 {
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = base.namespace().unwrap();
    let id = add_file(&mut ns, name);
    let delta = CompactStructuralDelta::capture(&base, &ns, StructuralScope::FileCreate).unwrap();
    f.store.publish_compact_structure(&delta).await.unwrap();
    id
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_enrollment_fences_old_readers_and_reopens() {
    let f = fixture(false).await;
    assert_eq!(
        f.store.compact_inode_capability(),
        CompactInodeCapability::V1
    );
    f.store
        .prepare_compact_inode_mode(f.backing, 1)
        .await
        .unwrap();
    let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(snapshot.anchor.generation, 2);
    assert_eq!(
        serde_json::to_value(snapshot.namespace().unwrap()).unwrap(),
        serde_json::to_value(root_namespace()).unwrap()
    );
    assert_eq!(
        snapshot.guards[&snapshot.anchor.root].identity,
        PhysicalInodeIdentity {
            incarnation: 2,
            epoch: 2,
            revision: 0
        }
    );
    assert!(f.store.load().await.unwrap_err().is(ErrorCode::Estale));
    assert!(
        f.store
            .load_if_changed(2)
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
    assert!(f.store.load_inode_snapshot(f.backing).await.is_err());
    let pool = Pool::from_url(&f.url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    // Reproduce the old mode-blind revision read exactly: enrollment must advance it.
    let (revision, body): (i64, String) = conn
        .exec_first(
            "SELECT revision,namespace FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (&f.key,),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revision, 2);
    assert!(serde_json::from_str::<Namespace>(&body).is_err());
    drop(conn);
    pool.disconnect().await.unwrap();
    f.store.close().await.unwrap();
    let fresh = TidbMetadataStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    assert_eq!(
        fresh.load_compact_snapshot(f.backing).await.unwrap(),
        snapshot
    );
    fresh.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_selected_physical_identity_survives_independent_structure() {
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let mut node = old.guard.node.clone();
    node.stats.mtime_ms += 1;
    let first = f
        .store
        .publish_compact_inode(
            f.backing,
            a,
            old.generation,
            old.guard.identity,
            node.clone(),
        )
        .await
        .unwrap();
    create(&f, "b").await;
    let current = f.store.load_compact_inode(f.backing, a).await.unwrap();
    assert_eq!(current.guard, first.guard);
    assert!(current.generation > first.generation);
    assert_eq!(
        current
            .guard
            .identity
            .logical_version(current.generation)
            .unwrap()
            .inode_revision,
        0
    );
    assert!(
        f.store
            .publish_compact_inode(
                f.backing,
                a,
                current.generation,
                old.guard.identity,
                old.guard.node
            )
            .await
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    let mut changed = node;
    changed.stats.mtime_ms += 1;
    let next = f
        .store
        .publish_compact_inode(
            f.backing,
            a,
            current.generation,
            current.guard.identity,
            changed,
        )
        .await
        .unwrap();
    assert_eq!(
        next.guard.identity.incarnation,
        old.guard.identity.incarnation
    );
    assert_eq!(next.guard.identity.epoch, current.generation);
    assert_eq!(next.guard.identity.revision, 1);
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_selected_streaming_certifies_full_audited_root_and_file() {
    let f = fixture(true).await;
    let inode = create(&f, "streamed-file").await;
    create(&f, "unrequested-sibling").await;
    let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (_, _, audited) = snapshot.clone().into_validated_namespace().unwrap();
    let file = &snapshot.guards[&inode];
    let root_read = f
        .store
        .read_compact_inode(f.backing, snapshot.anchor.root, audited.expect_root())
        .await;
    let file_read = f
        .store
        .read_compact_inode(
            f.backing,
            inode,
            CompactInodeExpectation::selected(
                snapshot.anchor.generation,
                file.identity,
                &file.node,
            ),
        )
        .await;
    let root_owned = f
        .store
        .load_compact_inode(f.backing, snapshot.anchor.root)
        .await;
    let file_owned = f.store.load_compact_inode(f.backing, inode).await;
    f.store.close().await.unwrap();

    let CompactInodeRead::Unchanged(root) = root_read.unwrap() else {
        panic!("canonical fresh root bytes must use streamed certification")
    };
    assert_eq!(root.generation(), snapshot.anchor.generation);
    assert_eq!(root.inode(), snapshot.anchor.root);
    assert_eq!(
        root.identity(),
        snapshot.guards[&snapshot.anchor.root].identity
    );
    let verified = root
        .into_verified_root()
        .expect("Full-audited root receipt required");
    assert_eq!(verified.generation(), snapshot.anchor.generation);
    assert_eq!(verified.guard(), &snapshot.guards[&snapshot.anchor.root]);
    assert_eq!(verified.guard(), &root_owned.unwrap().guard);
    let CompactInodeRead::Unchanged(file_read) = file_read.unwrap() else {
        panic!("canonical fresh file bytes must use streamed certification")
    };
    assert_eq!(file_read.generation(), snapshot.anchor.generation);
    assert_eq!(file_read.inode(), inode);
    assert_eq!(file_read.identity(), file.identity);
    assert!(file_read.into_verified_root().is_none());
    assert_eq!(file_owned.unwrap().guard, *file);
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_compact_selected_streaming_fallback_preserves_fresh_complete_logical_read() {
    let f = fixture(true).await;
    let inode = create(&f, "streamed-fallback").await;
    let original = f.store.load_compact_inode(f.backing, inode).await.unwrap();
    let mut changed = original.guard.node.clone();
    changed.stats.mtime_ms += 17;
    let acknowledged = f
        .store
        .publish_compact_inode(
            f.backing,
            inode,
            original.generation,
            original.guard.identity,
            changed,
        )
        .await
        .unwrap();
    let stale_expectation = CompactInodeExpectation::selected(
        original.generation,
        original.guard.identity,
        &original.guard.node,
    );
    let revision_read = f
        .store
        .read_compact_inode(f.backing, inode, stale_expectation)
        .await;
    let revision_owned = f.store.load_compact_inode(f.backing, inode).await;
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    // Warm the complete logical read before the encoding control.
    reader.load_compact_inode(f.backing, inode).await.unwrap();

    // Derived Serde accepts positional struct representations. The complete
    // fallback reconstructs the fresh logical guard from this server row.
    let sequence = serde_json::to_string(&serde_json::json!([
        &acknowledged.guard.node.stats,
        &acknowledged.guard.node.data,
    ]))
    .unwrap();
    let sequence_reference =
        serde_json::from_str::<mount_rs_core::storage::NodeMetadata>(&sequence);
    let pool = Pool::from_url(&f.url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_compact_guards SET node=? WHERE volume_key=? AND inode=?",
            (&sequence, &f.key, inode as i64),
        )
        .await
        .unwrap();
    drop(connection);
    pool.disconnect().await.unwrap();
    let before = raw(&f).await;
    proxy.begin();
    let sequence_read = reader
        .read_compact_inode(
            f.backing,
            inode,
            CompactInodeExpectation::selected(
                acknowledged.generation,
                acknowledged.guard.identity,
                &acknowledged.guard.node,
            ),
        )
        .await;
    let (queries, rows) = proxy.end();
    let sequence_owned = f.store.load_compact_inode(f.backing, inode).await;
    let after = raw(&f).await;
    reader.close().await.unwrap();
    f.store.close().await.unwrap();
    proxy.shutdown().await;

    let CompactInodeRead::Loaded(revision_read) = revision_read.unwrap() else {
        panic!("changed physical revision must return the fresh owned result")
    };
    assert_eq!(revision_read, revision_owned.unwrap());
    assert_eq!(revision_read, acknowledged);
    assert_ne!(revision_read.guard.identity, original.guard.identity);
    assert_eq!(sequence_reference.unwrap(), acknowledged.guard.node);
    assert_eq!(
        before, after,
        "fallback must preserve the same authority and guard bytes"
    );
    assert_eq!(
        before.1.iter().find(|row| row.0 == inode as i64).unwrap().4,
        sequence,
        "the actual server row retains the positional representation",
    );
    let sequence_owned = sequence_owned.unwrap();
    assert_eq!(sequence_owned, acknowledged);
    match sequence_read.unwrap() {
        CompactInodeRead::Loaded(sequence_read) => assert_eq!(sequence_read, sequence_owned),
        CompactInodeRead::Unchanged(checked) => {
            assert_eq!(checked.generation(), sequence_owned.generation);
            assert_eq!(checked.identity(), sequence_owned.guard.identity);
            assert!(checked.into_verified_root().is_none());
        }
    }
    assert_eq!(rows, 1);
    assert!(
        queries
            .iter()
            .any(|sql| sql.contains("FROM mount_rs_tidb_compact_members"))
    );
    assert!(
        queries
            .iter()
            .any(|sql| sql.starts_with("START TRANSACTION"))
    );
    assert!(queries.iter().all(|sql| !sql.contains("FOR UPDATE")));
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_compact_selected_streaming_preserves_authority_anchor_guard_error_precedence() {
    for case in [
        "stale-authority",
        "unsupported-anchor",
        "bad-guard",
        "missing-guard",
    ] {
        let f = fixture(true).await;
        let inode = create(&f, "streamed-precedence").await;
        let original = f.store.load_compact_inode(f.backing, inode).await.unwrap();
        let proxy = compact_proxy::Proxy::new(&f.url).await;
        let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
            .await
            .unwrap();
        reader.load_compact_inode(f.backing, inode).await.unwrap();
        let pool = Pool::from_url(&f.url).unwrap();
        let mut connection = pool.get_conn().await.unwrap();
        if case == "missing-guard" {
            connection
                .exec_drop(
                    "DELETE FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=?",
                    (&f.key, inode as i64),
                )
                .await
                .unwrap();
        } else {
            connection.exec_drop(
                "UPDATE mount_rs_tidb_compact_guards SET node='{}' WHERE volume_key=? AND inode=?",
                (&f.key, inode as i64),
            ).await.unwrap();
        }
        if case == "stale-authority" {
            connection
                .exec_drop(
                    "UPDATE mount_rs_tidb_metadata SET write_mode='MRC4' WHERE volume_key=?",
                    (&f.key,),
                )
                .await
                .unwrap();
        } else if case == "unsupported-anchor" {
            let (anchor,): (String,) = connection
                .exec_first(
                    "SELECT namespace FROM mount_rs_tidb_metadata WHERE volume_key=?",
                    (&f.key,),
                )
                .await
                .unwrap()
                .unwrap();
            let mut anchor: serde_json::Value = serde_json::from_str(&anchor).unwrap();
            anchor["authority"]["default_chunker"]["algorithm"] =
                serde_json::json!("unsupported-streamed-precedence-chunker");
            connection
                .exec_drop(
                    "UPDATE mount_rs_tidb_metadata SET namespace=? WHERE volume_key=?",
                    (serde_json::to_string(&anchor).unwrap(), &f.key),
                )
                .await
                .unwrap();
        }
        drop(connection);
        pool.disconnect().await.unwrap();
        let before = raw(&f).await;
        let owned = f.store.load_compact_inode(f.backing, inode).await;
        proxy.begin();
        let streamed = reader
            .read_compact_inode(
                f.backing,
                inode,
                CompactInodeExpectation::selected(
                    original.generation,
                    original.guard.identity,
                    &original.guard.node,
                ),
            )
            .await;
        let (queries, rows) = proxy.end();
        let after = raw(&f).await;
        reader.close().await.unwrap();
        f.store.close().await.unwrap();
        proxy.shutdown().await;

        let owned = owned.unwrap_err();
        let streamed = streamed.unwrap_err();
        let expected_code = match case {
            "stale-authority" | "missing-guard" => ErrorCode::Estale,
            "unsupported-anchor" => ErrorCode::Enotsup,
            "bad-guard" => ErrorCode::Eio,
            _ => unreachable!(),
        };
        assert_eq!(
            owned.code, expected_code,
            "{case}: reference decoder precedence"
        );
        assert_eq!(
            (
                streamed.code,
                &streamed.syscall,
                &streamed.path,
                &streamed.dest,
                streamed.to_string()
            ),
            (
                owned.code,
                &owned.syscall,
                &owned.path,
                &owned.dest,
                owned.to_string()
            ),
            "{case}: borrowed-row fallback must preserve the complete reference error",
        );
        assert_eq!(
            before, after,
            "{case}: read failures must preserve all raw rows"
        );
        assert!(
            rows <= 1,
            "{case}: complete logical selected read must not enumerate unrelated guards"
        );
        assert!(
            queries
                .iter()
                .any(|sql| sql.starts_with("START TRANSACTION")),
            "{case}: complete logical read uses one snapshot"
        );
        assert!(
            queries.iter().all(|sql| !sql.contains("FOR UPDATE")),
            "{case}"
        );
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_same_inode_one_winner_and_unrelated_preserved() {
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let b = create(&f, "b").await;
    let first = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let second = f.store.load_compact_inode(f.backing, b).await.unwrap();
    let other = TidbMetadataStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let mut node1 = first.guard.node.clone();
    node1.stats.mtime_ms += 1;
    let mut node2 = first.guard.node.clone();
    node2.stats.mtime_ms += 2;
    let (one, two) = tokio::join!(
        f.store
            .publish_compact_inode(f.backing, a, first.generation, first.guard.identity, node1),
        other.publish_compact_inode(f.backing, a, first.generation, first.guard.identity, node2)
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    assert!(
        one.as_ref()
            .err()
            .or(two.as_ref().err())
            .unwrap()
            .is(ErrorCode::Eagain)
    );
    let winner = one.or(two).unwrap();
    let mut changed = second.guard.node.clone();
    changed.stats.mtime_ms += 3;
    let bnext = other
        .publish_compact_inode(
            f.backing,
            b,
            second.generation,
            second.guard.identity,
            changed,
        )
        .await
        .unwrap();
    let fresh = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(fresh.guards[&a], winner.guard);
    assert_eq!(fresh.guards[&b], bnext.guard);
    other.close().await.unwrap();
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_full_unlink_orphan_removal_preserves_untouched_guard() {
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let b = create(&f, "b").await;
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = base.namespace().unwrap();
    let NodeData::Directory { entries } = &mut ns.nodes.get_mut(&ns.root).unwrap().data else {
        panic!()
    };
    entries.retain(|entry| entry.inode != a);
    ns.nodes.get_mut(&a).unwrap().stats.nlink = 0;
    let delta = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
    f.store.publish_compact_structure(&delta).await.unwrap();
    let orphan = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(orphan.guards[&a].node.stats.nlink, 0);
    assert_eq!(orphan.guards[&b], base.guards[&b]);
    let mut ns = orphan.namespace().unwrap();
    ns.nodes.remove(&a);
    let delta = CompactStructuralDelta::capture(&orphan, &ns, StructuralScope::Full).unwrap();
    let receipt = f.store.publish_compact_structure(&delta).await.unwrap();
    assert_eq!(receipt.removed, std::collections::BTreeSet::from([a]));
    assert!(receipt.upserts.is_empty());
    let after = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert!(!after.guards.contains_key(&a));
    assert_eq!(after.guards[&b], base.guards[&b]);
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_enrollment_rejects_nonfresh_namespace_without_mutation() {
    let f = fixture(false).await;
    let before = f.store.load().await.unwrap();
    let mut ns = before.namespace.unwrap();
    add_file(&mut ns, "used");
    f.store
        .publish_bound_if_revision(f.backing, 1, ns)
        .await
        .unwrap();
    let before = f.store.load().await.unwrap();
    assert!(
        f.store
            .prepare_compact_inode_mode(f.backing, 2)
            .await
            .unwrap_err()
            .is(ErrorCode::Ebusy)
    );
    let after = f.store.load().await.unwrap();
    assert_eq!(after.revision, before.revision);
    assert_eq!(
        serde_json::to_value(after.namespace).unwrap(),
        serde_json::to_value(before.namespace).unwrap()
    );
    f.store.close().await.unwrap();
}

#[path = "support/compact_proxy.rs"]
mod compact_proxy;
use std::sync::atomic::Ordering;

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_compact_root_file_transitions_target_two_guards() {
    for siblings in [128, 1000] {
        let f = fixture(true).await;
        let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let mut ns = base.namespace().unwrap();
        let file = add_file(&mut ns, "selected-source");
        for n in 0..siblings {
            add_file(&mut ns, &format!("sibling-{n}"));
        }
        let setup = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
        f.store.publish_compact_structure(&setup).await.unwrap();
        let expected = f.store.load_compact_snapshot(f.backing).await.unwrap();
        assert_eq!(expected.guards.len(), siblings + 2);

        let proxy = compact_proxy::Proxy::new(&f.url).await;
        let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
            .await
            .unwrap();
        assert_eq!(
            reader.compact_root_file_capability(),
            CompactRootFileCapability::Supported
        );
        // Warm the owned connection and prepared statement outside the trace.
        reader
            .load_compact_root_file(f.backing, expected.anchor.root, file)
            .await
            .unwrap();
        proxy.begin();
        let captured = reader
            .load_compact_root_file(f.backing, expected.anchor.root, file)
            .await
            .unwrap();
        let (queries, _) = proxy.end();

        assert_eq!(captured.anchor(), &expected.anchor);
        assert_eq!(
            captured.root(),
            Some(&expected.guards[&expected.anchor.root])
        );
        assert_eq!(captured.file(), Some(&expected.guards[&file]));
        let selects: Vec<_> = queries
            .iter()
            .filter(|sql| sql.starts_with("SELECT "))
            .collect();
        assert!(
            selects
                .iter()
                .any(|sql| sql.contains("FROM mount_rs_tidb_metadata"))
        );
        assert!(
            selects
                .iter()
                .any(|sql| sql.contains("FROM mount_rs_tidb_compact_members"))
        );
        assert!(
            selects
                .iter()
                .any(|sql| sql.contains("FROM mount_rs_tidb_compact_dentries"))
        );
        assert!(selects.iter().all(|sql| !sql.contains("FOR UPDATE")));
        assert_eq!(
            queries
                .iter()
                .filter(|sql| sql.starts_with("START TRANSACTION"))
                .count(),
            1,
            "{siblings} siblings: complete root-file capture uses one coherent snapshot"
        );
        let (_, _, audited) = expected.clone().into_validated_namespace().unwrap();
        let verified = audited
            .verify_root_file(
                captured,
                file,
                &expected.guards[&file].node,
                expected.guards[&file].identity,
            )
            .unwrap();
        let root_stats = &expected.guards[&expected.anchor.root].node.stats;
        let file_stats = &expected.guards[&file].node.stats;
        let rename = CompactRootFileTransition::capture(
            verified,
            CompactRootFileIntent::RenameAbsent {
                from: "selected-source".into(),
                to: "renamed-source".into(),
            },
            CompactRootFileTimes {
                parent_mtime_ms: root_stats.mtime_ms + 2,
                parent_ctime_ms: root_stats.ctime_ms + 2,
                file_ctime_ms: file_stats.ctime_ms + 1,
            },
        )
        .unwrap();
        proxy.begin();
        reader
            .publish_compact_structure(rename.delta())
            .await
            .unwrap();
        let (rename_queries, rename_rows) = proxy.end();
        assert_root_file_publication_queries(&rename_queries, rename_rows);
        let renamed = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let NodeData::Directory { entries } = &renamed.guards[&renamed.anchor.root].node.data
        else {
            panic!("root directory required");
        };
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "renamed-source" && entry.inode == file)
        );
        assert!(!entries.iter().any(|entry| entry.name == "selected-source"));
        assert_eq!(renamed.guards[&file].node.stats.nlink, 1);

        let (_, _, audited) = renamed.clone().into_validated_namespace().unwrap();
        let read = reader
            .load_compact_root_file(f.backing, renamed.anchor.root, file)
            .await
            .unwrap();
        let verified = audited
            .verify_root_file(
                read,
                file,
                &renamed.guards[&file].node,
                renamed.guards[&file].identity,
            )
            .unwrap();
        let root_stats = &renamed.guards[&renamed.anchor.root].node.stats;
        let file_stats = &renamed.guards[&file].node.stats;
        let unlink = CompactRootFileTransition::capture(
            verified,
            CompactRootFileIntent::UnlinkLastLink {
                name: "renamed-source".into(),
            },
            CompactRootFileTimes {
                parent_mtime_ms: root_stats.mtime_ms + 1,
                parent_ctime_ms: root_stats.ctime_ms + 1,
                file_ctime_ms: file_stats.ctime_ms + 1,
            },
        )
        .unwrap();
        proxy.begin();
        reader
            .publish_compact_structure(unlink.delta())
            .await
            .unwrap();
        let (unlink_queries, unlink_rows) = proxy.end();
        assert_root_file_publication_queries(&unlink_queries, unlink_rows);
        let unlinked = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let NodeData::Directory { entries } = &unlinked.guards[&unlinked.anchor.root].node.data
        else {
            panic!("root directory required");
        };
        assert!(!entries.iter().any(|entry| entry.inode == file));
        assert_eq!(unlinked.guards[&file].node.stats.nlink, 0);
        assert!(unlinked.anchor.members.contains(&file));
        assert_eq!(unlinked.guards.len(), siblings + 2);
        reader.close().await.unwrap();
        proxy.shutdown().await;
        f.store.close().await.unwrap();
        eprintln!(
            "compact root-file transitions: siblings={siblings}; one coherent complete capture; each publication two locked guards, two guard updates and one authority update; source tombstone retained"
        );
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_compact_root_file_capture_uses_one_view() {
    let f = fixture(true).await;
    let file = create(&f, "source").await;
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let transition = root_file_rename_transition(&f, &f.store, file, "source", "moved").await;
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    reader
        .load_compact_root_file(f.backing, before.anchor.root, file)
        .await
        .unwrap();
    proxy.begin();
    proxy.trace.pause_anchor.store(true, Ordering::SeqCst);
    let backing = f.backing;
    let root = before.anchor.root;
    let reading = tokio::spawn(async move {
        let result = reader.load_compact_root_file(backing, root, file).await;
        reader.close().await.unwrap();
        result
    });
    proxy.reached().await;
    // TiDB fixes this repeatable-read view when the transaction starts, before
    // the paused authority SELECT. Commit both structural bodies, then an
    // independent selected revision; every captured row must stay old.
    f.store
        .publish_compact_structure(transition.delta())
        .await
        .unwrap();
    let old = f.store.load_compact_inode(f.backing, file).await.unwrap();
    let mut node = old.guard.node;
    node.stats.mtime_ms += 1;
    f.store
        .publish_compact_inode(f.backing, file, old.generation, old.guard.identity, node)
        .await
        .unwrap();
    let expected = f.store.load_compact_snapshot(f.backing).await.unwrap();
    proxy.trace.resume.notify_one();
    let captured = reading.await.unwrap().unwrap();
    let (queries, guard_rows) = proxy.end();
    let fresh = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(
        guard_rows, 2,
        "both complete guards share the authority transaction view"
    );
    assert_eq!(
        captured.into_parts(),
        (
            before.anchor.clone(),
            Some(before.guards[&root].clone()),
            Some(before.guards[&file].clone()),
        ),
        "authority, root and file must all retain the transaction-start view",
    );
    assert_eq!(
        fresh, expected,
        "independent fresh reads observe both committed updates"
    );
    assert_ne!(expected.anchor, before.anchor);
    assert_ne!(expected.guards[&root], before.guards[&root]);
    assert_ne!(expected.guards[&file], before.guards[&file]);
    let selects: Vec<_> = queries
        .iter()
        .filter(|sql| sql.starts_with("SELECT "))
        .collect();
    assert!(
        selects
            .iter()
            .any(|sql| sql.contains("FROM mount_rs_tidb_compact_members"))
    );
    assert!(
        selects
            .iter()
            .any(|sql| sql.contains("FROM mount_rs_tidb_compact_dentries"))
    );
    assert!(selects.iter().all(|sql| !sql.contains("FOR UPDATE")));
    let starts: Vec<_> = queries
        .iter()
        .map(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|sql| sql.starts_with("START TRANSACTION"))
        .collect();
    assert_eq!(starts.len(), 1);
    proxy.shutdown().await;
    f.store.close().await.unwrap();
}

fn assert_root_file_publication_queries(queries: &[String], returned_guard_rows: usize) {
    assert_eq!(returned_guard_rows, 2, "exactly two locked guard rows");
    let guard_reads: Vec<_> = queries
        .iter()
        .filter(|sql| {
            sql.starts_with(
                "SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards",
            )
        })
        .collect();
    assert_eq!(guard_reads.len(), 1);
    assert!(guard_reads[0].contains("inode IN (?,?) ORDER BY inode FOR UPDATE"));
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.starts_with("UPDATE mount_rs_tidb_compact_guards"))
            .count(),
        2,
    );
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.starts_with("UPDATE mount_rs_tidb_metadata SET revision="))
            .count(),
        1,
    );
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.eq_ignore_ascii_case("COMMIT"))
            .count(),
        1
    );
    assert!(
        !queries.iter().any(|sql| {
            (sql.starts_with("INSERT ") || sql.starts_with("UPDATE ") || sql.starts_with("DELETE "))
                && sql.contains("mount_rs_tidb_compact_members")
        }),
        "rename/unlink retain the selected member, including its tombstone"
    );
    let dentry_writes = queries
        .iter()
        .filter(|sql| {
            (sql.starts_with("INSERT ") || sql.starts_with("UPDATE ") || sql.starts_with("DELETE "))
                && sql.contains("mount_rs_tidb_compact_dentries")
        })
        .count();
    assert!(
        (1..=2).contains(&dentry_writes),
        "root transition changes only its removed/appended dentry"
    );
}

async fn root_file_rename_transition(
    f: &Fixture,
    reader: &TidbMetadataStore,
    file: u64,
    from: &str,
    to: &str,
) -> CompactRootFileTransition {
    let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (_, _, audited) = snapshot.clone().into_validated_namespace().unwrap();
    let read = reader
        .load_compact_root_file(f.backing, snapshot.anchor.root, file)
        .await
        .unwrap();
    let verified = audited
        .verify_root_file(
            read,
            file,
            &snapshot.guards[&file].node,
            snapshot.guards[&file].identity,
        )
        .unwrap();
    let root = &snapshot.guards[&snapshot.anchor.root].node.stats;
    let file_stats = &snapshot.guards[&file].node.stats;
    CompactRootFileTransition::capture(
        verified,
        CompactRootFileIntent::RenameAbsent {
            from: from.into(),
            to: to.into(),
        },
        CompactRootFileTimes {
            parent_mtime_ms: root.mtime_ms.checked_add(2).unwrap(),
            parent_ctime_ms: root.ctime_ms.checked_add(2).unwrap(),
            file_ctime_ms: file_stats.ctime_ms.checked_add(1).unwrap(),
        },
    )
    .unwrap()
}

async fn root_file_unlink_transition(
    f: &Fixture,
    reader: &TidbMetadataStore,
    file: u64,
    name: &str,
) -> CompactRootFileTransition {
    let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (_, _, audited) = snapshot.clone().into_validated_namespace().unwrap();
    let read = reader
        .load_compact_root_file(f.backing, snapshot.anchor.root, file)
        .await
        .unwrap();
    let verified = audited
        .verify_root_file(
            read,
            file,
            &snapshot.guards[&file].node,
            snapshot.guards[&file].identity,
        )
        .unwrap();
    let root = &snapshot.guards[&snapshot.anchor.root].node.stats;
    let file_stats = &snapshot.guards[&file].node.stats;
    CompactRootFileTransition::capture(
        verified,
        CompactRootFileIntent::UnlinkLastLink { name: name.into() },
        CompactRootFileTimes {
            parent_mtime_ms: root.mtime_ms.checked_add(1).unwrap(),
            parent_ctime_ms: root.ctime_ms.checked_add(1).unwrap(),
            file_ctime_ms: file_stats.ctime_ms.checked_add(1).unwrap(),
        },
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_compact_root_file_wait_rechecks_source_and_authority() {
    let f = fixture(true).await;
    let file = create(&f, "source").await;
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    let transition = root_file_rename_transition(&f, &writer, file, "source", "moved").await;
    let pool = Pool::from_url(&f.url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    conn.query_drop("SET SESSION tidb_txn_mode='pessimistic'")
        .await
        .unwrap();
    let mut tx = conn
        .start_transaction(mysql_async::TxOpts::default())
        .await
        .unwrap();
    let (revision, body): (i64, String) = tx
        .exec_first(
            "SELECT revision,node FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=? FOR UPDATE",
            (&f.key, file),
        )
        .await
        .unwrap()
        .unwrap();
    let mut node: mount_rs_core::storage::NodeMetadata = serde_json::from_str(&body).unwrap();
    node.stats.mtime_ms += 1;
    tx.exec_drop(
        "UPDATE mount_rs_tidb_compact_guards SET revision=?,node=? WHERE volume_key=? AND inode=?",
        (
            revision + 1,
            serde_json::to_string(&node).unwrap(),
            &f.key,
            file,
        ),
    )
    .await
    .unwrap();
    proxy.begin();
    proxy.trace.pause_guards.store(true, Ordering::SeqCst);
    let publishing = tokio::spawn(async move {
        let result = writer.publish_compact_structure(transition.delta()).await;
        writer.close().await.unwrap();
        result
    });
    proxy.reached().await;
    proxy.trace.resume.notify_one();
    tx.commit().await.unwrap();
    let result = publishing.await.unwrap();
    assert!(result.unwrap_err().is(ErrorCode::Eagain));
    let (queries, _) = proxy.end();
    assert!(!queries.iter().any(|sql| sql.starts_with("UPDATE ")
        || sql.starts_with("INSERT ")
        || sql.starts_with("DELETE ")));
    let after_source = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(after_source.guards[&file].node, node);
    assert_eq!(
        after_source.guards[&file].identity.revision,
        revision as u64 + 1
    );
    assert_eq!(after_source.anchor.members.len(), 2);

    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    let transition = root_file_rename_transition(&f, &writer, file, "source", "moved").await;
    create(&f, "independent").await;
    let before = raw(&f).await;
    proxy.begin();
    assert!(
        writer
            .publish_compact_structure(transition.delta())
            .await
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    let (queries, _) = proxy.end();
    assert!(!queries.iter().any(|sql| sql.starts_with("UPDATE ")
        || sql.starts_with("INSERT ")
        || sql.starts_with("DELETE ")));
    assert_eq!(raw(&f).await, before);
    writer.close().await.unwrap();
    proxy.shutdown().await;
    drop(conn);
    pool.disconnect().await.unwrap();
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_compact_root_file_commit_ack_unknown_not_replayed() {
    use std::time::Duration;
    use tokio::time::timeout;

    let f = fixture(true).await;
    let file = create(&f, "source").await;
    for rename in [true, false] {
        let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let proxy = compact_proxy::Proxy::new(&f.url).await;
        let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
            .await
            .unwrap();
        let transition = if rename {
            root_file_rename_transition(&f, &writer, file, "source", "moved").await
        } else {
            root_file_unlink_transition(&f, &writer, file, "moved").await
        };
        proxy.begin();
        proxy.trace.drop_commit_ack.store(true, Ordering::SeqCst);
        let result = timeout(
            Duration::from_secs(10),
            writer.publish_compact_structure(transition.delta()),
        )
        .await
        .expect("controlled root-file publication completed");
        let (queries, rows) = proxy.end();
        let commits = proxy.trace.commits.load(Ordering::SeqCst);
        let dropped = proxy.trace.dropped_acks.load(Ordering::SeqCst);
        let fresh = f.store.load_compact_snapshot(f.backing).await.unwrap();
        timeout(Duration::from_secs(10), writer.close())
            .await
            .expect("writer disconnect completed")
            .unwrap();
        timeout(Duration::from_secs(10), proxy.shutdown())
            .await
            .expect("proxy relay settled");
        let error = result.unwrap_err();
        assert!(error.is(ErrorCode::Eio));
        assert!(!error.is(ErrorCode::Eagain));
        assert!(error.to_string().contains("commit outcome is unknown"));
        assert_eq!(commits, 1);
        assert_eq!(dropped, 1);
        assert_root_file_publication_queries(&queries, rows);
        assert_eq!(fresh.anchor.generation, before.anchor.generation + 1);
        assert_eq!(fresh.guards[&file].identity.epoch, fresh.anchor.generation);
        let NodeData::Directory { entries } = &fresh.guards[&fresh.anchor.root].node.data else {
            panic!("root directory required");
        };
        if rename {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.name == "moved" && entry.inode == file)
            );
            assert!(!entries.iter().any(|entry| entry.name == "source"));
            assert_eq!(fresh.guards[&file].node.stats.nlink, 1);
        } else {
            assert!(!entries.iter().any(|entry| entry.inode == file));
            assert_eq!(fresh.guards[&file].node.stats.nlink, 0);
            assert!(fresh.anchor.members.contains(&file));
        }
    }
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_compact_root_file_packet_rejects_before_mutation() {
    use mount_rs_core::storage::{BlockExtent, BlockId};

    let f = fixture(true).await;
    let file = create(&f, "source").await;
    let old = f.store.load_compact_inode(f.backing, file).await.unwrap();
    let mut node = old.guard.node.clone();
    node.stats.size = 1024;
    node.stats.blocks = 2;
    // The locked input row must fit the client's receive packet. Make the
    // serialized body close enough to that limit for the provider's outgoing
    // statement budget to reject it before any DML, for either transition.
    for offset in 0..1024 {
        let NodeData::File(layout) = &mut node.data else {
            panic!("source file required")
        };
        layout.extents.push(BlockExtent {
            file_offset: offset,
            block: BlockId(format!("b{}", "1".repeat(64))),
            block_offset: 0,
            length: 1,
        });
        if serde_json::to_vec(&node).unwrap().len() >= 64900 {
            break;
        }
    }
    let input_bytes = serde_json::to_vec(&node).unwrap().len();
    assert!((64900..=65100).contains(&input_bytes));
    assert!(
        input_bytes + 128 < 65536,
        "complete guard row fits receive packet"
    );
    f.store
        .publish_compact_inode(f.backing, file, old.generation, old.guard.identity, node)
        .await
        .unwrap();
    let rename = root_file_rename_transition(&f, &f.store, file, "source", "moved").await;
    let unlink = root_file_unlink_transition(&f, &f.store, file, "source").await;
    let before = raw(&f).await;
    let mut url = url::Url::parse(&f.url).unwrap();
    url.query_pairs_mut()
        .append_pair("max_allowed_packet", "65536");
    let proxy = compact_proxy::Proxy::new(url.as_str()).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    for transition in [&rename, &unlink] {
        proxy.begin();
        let error = writer
            .publish_compact_structure(transition.delta())
            .await
            .unwrap_err();
        let (queries, rows) = proxy.end();
        assert!(error.is(ErrorCode::Efbig));
        assert_eq!(rows, 2);
        assert!(!queries.iter().any(|sql| sql.starts_with("UPDATE ")
            || sql.starts_with("INSERT ")
            || sql.starts_with("DELETE ")));
        assert_eq!(raw(&f).await, before);
    }
    writer.close().await.unwrap();
    proxy.shutdown().await;
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_snapshot_uses_one_view_without_generation_change() {
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let b = create(&f, "b").await;
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    proxy.begin();
    proxy.trace.pause_guards.store(true, Ordering::SeqCst);
    let reading = tokio::spawn({
        let backing = f.backing;
        async move {
            let result = reader.load_compact_snapshot(backing).await;
            reader.close().await.unwrap();
            result
        }
    });
    proxy.reached().await;
    // The anchor SELECT has completed. Change a guard without changing generation
    // before the snapshot's guard SELECT reaches TiDB.
    let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let mut node = old.guard.node;
    node.stats.mtime_ms += 7;
    f.store
        .publish_compact_inode(f.backing, a, old.generation, old.guard.identity, node)
        .await
        .unwrap();
    let second = f.store.load_compact_inode(f.backing, b).await.unwrap();
    let mut second_node = second.guard.node;
    second_node.stats.mtime_ms += 11;
    f.store
        .publish_compact_inode(
            f.backing,
            b,
            second.generation,
            second.guard.identity,
            second_node,
        )
        .await
        .unwrap();
    proxy.trace.resume.notify_one();
    let snapshot = reading.await.unwrap().unwrap();
    assert_eq!(
        snapshot, before,
        "a full snapshot must remain at its anchor read view"
    );
    assert_ne!(
        f.store
            .load_compact_snapshot(f.backing)
            .await
            .unwrap()
            .guards[&a],
        snapshot.guards[&a]
    );
    let fresh = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(fresh.anchor, before.anchor);
    assert_ne!(fresh.guards[&a], snapshot.guards[&a]);
    assert_ne!(fresh.guards[&b], snapshot.guards[&b]);
    eprintln!(
        "compact TiDB RR control: anchor unchanged; two independent selected writes committed after anchor read; snapshot returned both OLD complete guards; fresh snapshot returned both NEW complete guards"
    );
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_aborted_structure_rolls_back_prior_guard_writes() {
    let f = fixture(true).await;
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let raw_before = raw(&f).await;
    let mut ns = before.namespace().unwrap();
    add_file(&mut ns, "aborted");
    let delta = CompactStructuralDelta::capture(&before, &ns, StructuralScope::FileCreate).unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    proxy.begin();
    proxy.trace.abort_anchor_write.store(true, Ordering::SeqCst);
    let error = writer.publish_compact_structure(&delta).await.unwrap_err();
    assert!(!error.is(ErrorCode::Eagain));
    let (queries, rows) = proxy.end();
    assert_eq!(rows, 1);
    assert_eq!(
        queries
            .iter()
            .filter(|s| s.starts_with("INSERT INTO mount_rs_tidb_compact_guards"))
            .count(),
        1
    );
    assert_eq!(
        queries
            .iter()
            .filter(|s| s.starts_with("UPDATE mount_rs_tidb_compact_guards"))
            .count(),
        1
    );
    assert_eq!(proxy.trace.commits.load(Ordering::SeqCst), 0);
    for table in [
        "mount_rs_tidb_compact_members",
        "mount_rs_tidb_compact_dentries",
    ] {
        assert!(
            queries
                .iter()
                .any(|sql| sql.starts_with("INSERT INTO ") && sql.contains(table)),
            "fault must occur after new {table} rows were written"
        );
    }
    assert_eq!(
        raw(&f).await,
        raw_before,
        "rollback restores authority, guards, members and dentries"
    );
    let fresh = TidbMetadataStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    assert_eq!(
        fresh.load_compact_snapshot(f.backing).await.unwrap(),
        before
    );
    writer.close().await.unwrap();
    fresh.close().await.unwrap();
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_lost_commit_ack_is_unknown_and_not_replayed() {
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let mut node = old.guard.node.clone();
    node.stats.mtime_ms += 9;
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    proxy.begin();
    proxy.trace.drop_commit_ack.store(true, Ordering::SeqCst);
    let error = writer
        .publish_compact_inode(
            f.backing,
            a,
            old.generation,
            old.guard.identity,
            node.clone(),
        )
        .await
        .unwrap_err();
    assert!(!error.is(ErrorCode::Eagain));
    assert!(error.to_string().contains("commit outcome is unknown"));
    assert_eq!(proxy.trace.commits.load(Ordering::SeqCst), 1);
    assert_eq!(proxy.trace.dropped_acks.load(Ordering::SeqCst), 1);
    let (queries, rows) = proxy.end();
    assert_eq!(rows, 1);
    assert_eq!(
        queries
            .iter()
            .filter(|s| s.starts_with("UPDATE mount_rs_tidb_compact_guards"))
            .count(),
        1
    );
    let fresh = TidbMetadataStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let loaded = fresh.load_compact_inode(f.backing, a).await.unwrap();
    assert_eq!(loaded.guard.node, node);
    assert_eq!(
        loaded.guard.identity.revision,
        old.guard.identity.revision + 1
    );
    writer.close().await.unwrap();
    fresh.close().await.unwrap();
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_file_create_lost_commit_ack_is_unknown_and_not_replayed() {
    use std::time::Duration;
    use tokio::time::timeout;

    const BOUND: Duration = Duration::from_secs(10);
    let f = fixture(true).await;
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = before.namespace().unwrap();
    let new = add_file(&mut ns, "lost-create-ack");
    ns.nodes.get_mut(&new).unwrap().stats.mtime_ms = 9;
    let delta = CompactStructuralDelta::capture(&before, &ns, StructuralScope::FileCreate).unwrap();

    // Assemble the committed state independently of the shared delta evaluator.
    // A generation-only oracle would miss a partial parent/new-file mutation.
    let mut anchor = before.anchor.clone();
    anchor.generation += 1;
    anchor.next_inode += 1;
    anchor.members.push(new);
    let expected = CompactSnapshot {
        guards: std::collections::BTreeMap::from([
            (
                anchor.root,
                CompactGuard {
                    identity: PhysicalInodeIdentity {
                        incarnation: before.guards[&anchor.root].identity.incarnation,
                        epoch: anchor.generation,
                        revision: 0,
                    },
                    node: ns.nodes[&anchor.root].clone(),
                },
            ),
            (
                new,
                CompactGuard {
                    identity: PhysicalInodeIdentity {
                        incarnation: anchor.generation,
                        epoch: anchor.generation,
                        revision: 0,
                    },
                    node: ns.nodes[&new].clone(),
                },
            ),
        ]),
        anchor,
    };
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    proxy.begin();
    // The existing relay discards only an actual successful server COMMIT OK.
    proxy.trace.drop_commit_ack.store(true, Ordering::SeqCst);
    let publication = timeout(BOUND, writer.publish_compact_structure(&delta)).await;
    let (queries, rows) = proxy.end();
    let commits = proxy.trace.commits.load(Ordering::SeqCst);
    let dropped_acks = proxy.trace.dropped_acks.load(Ordering::SeqCst);

    // Bypass the faulting connection for the full anchor/body/identity oracle.
    let fresh = timeout(BOUND, TidbMetadataStore::connect_with_key(&f.url, &f.key)).await;
    let loaded = match fresh.as_ref() {
        Ok(Ok(store)) => Some(timeout(BOUND, store.load_compact_snapshot(f.backing)).await),
        _ => None,
    };
    // Attempt all disconnects and drain the actual relay before assertions.
    let writer_closed = timeout(BOUND, writer.close()).await;
    let fresh_closed = match fresh.as_ref() {
        Ok(Ok(store)) => Some(timeout(BOUND, store.close()).await),
        _ => None,
    };
    let fixture_closed = timeout(BOUND, f.store.close()).await;
    let proxy_closed = timeout(BOUND, proxy.shutdown()).await;

    writer_closed.expect("writer disconnect completed").unwrap();
    fixture_closed
        .expect("fixture disconnect completed")
        .unwrap();
    proxy_closed.expect("proxy listener and relays settled");
    fresh.expect("fresh store connect completed").unwrap();
    fresh_closed
        .expect("fresh store available for close")
        .expect("fresh store disconnect completed")
        .unwrap();

    let error = publication
        .expect("controlled file-create publication completed")
        .unwrap_err();
    assert!(error.is(ErrorCode::Eio));
    assert!(!error.is(ErrorCode::Eagain));
    assert!(
        error
            .to_string()
            .contains("publish compact structure commit outcome is unknown")
    );
    assert_eq!(commits, 1);
    assert_eq!(dropped_acks, 1);
    assert_eq!(rows, 1);
    // A replay can fail at its next read without another mutation or COMMIT.
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.starts_with("START TRANSACTION"))
            .count(),
        1
    );
    let joined = |sql: &String| {
        compact_proxy::joined_authority_select(sql)
            && sql.contains(" LEFT JOIN mount_rs_tidb_compact_guards AS g ")
    };
    assert_eq!(
        queries
            .iter()
            .filter(|sql| {
                joined(sql)
                    || sql.starts_with("SELECT revision,write_mode,backing_id,owner,fence,expires,namespace,delegation")
            })
            .count(),
        1,
        "one anchor-bearing read must precede the one publication"
    );
    assert_eq!(
        queries
            .iter()
            .filter(|sql| {
                joined(sql)
                    || sql.starts_with("SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards")
            })
            .count(),
        1,
        "one parent-bearing read must precede the one publication"
    );
    for (prefix, count) in [
        ("INSERT INTO mount_rs_tidb_compact_guards", 1),
        ("UPDATE mount_rs_tidb_compact_guards", 1),
        ("DELETE FROM mount_rs_tidb_compact_guards", 0),
        ("UPDATE mount_rs_tidb_metadata", 1),
        ("INSERT INTO mount_rs_tidb_compact_members", 1),
        ("DELETE FROM mount_rs_tidb_compact_members", 0),
        ("INSERT INTO mount_rs_tidb_compact_dentries", 1),
        ("DELETE FROM mount_rs_tidb_compact_dentries", 0),
    ] {
        assert_eq!(
            queries.iter().filter(|sql| sql.starts_with(prefix)).count(),
            count,
            "{prefix}"
        );
    }
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.eq_ignore_ascii_case("COMMIT"))
            .count(),
        1
    );
    assert_eq!(
        loaded
            .expect("fresh store available for full snapshot")
            .expect("fresh full snapshot completed")
            .unwrap(),
        expected,
        "lost acknowledgment must leave exactly one complete file create committed"
    );
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_targeted_counts_and_128_fresh_full_byte_oracles() {
    use mount_rs_core::storage::{BlockExtent, BlockStore};
    use mount_rs_tidb::TidbBlockStore;
    let f = fixture(true).await;
    let blocks = TidbBlockStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = base.namespace().unwrap();
    let mut expected = std::collections::BTreeMap::new();
    // Setup and shared full-snapshot/delta capture are outside measured publication.
    for n in 0..128 {
        let inode = add_file(&mut ns, &format!("file{n}"));
        let bytes: Vec<u8> = (0..257).map(|i| ((i * 37 + n * 11) % 251) as u8).collect();
        let block = blocks.put(&bytes).await.unwrap();
        let node = ns.nodes.get_mut(&inode).unwrap();
        node.stats.size = bytes.len() as u64;
        node.stats.blocks = 1;
        let NodeData::File(layout) = &mut node.data else {
            panic!()
        };
        layout.extents.push(BlockExtent {
            file_offset: 0,
            block,
            block_offset: 0,
            length: bytes.len() as u64,
        });
        expected.insert(inode, bytes);
    }
    let setup = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
    f.store.publish_compact_structure(&setup).await.unwrap();
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = before.namespace().unwrap();
    let new = add_file(&mut ns, "new");
    let delta = CompactStructuralDelta::capture(&before, &ns, StructuralScope::FileCreate).unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let store = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    proxy.begin();
    let receipt = store.publish_compact_structure(&delta).await.unwrap();
    let (queries, rows) = proxy.end();
    assert_eq!(rows, 1);
    assert_eq!(receipt.upserts.len(), 2);
    for (prefix, count) in [
        ("INSERT INTO mount_rs_tidb_compact_guards", 1),
        ("UPDATE mount_rs_tidb_compact_guards", 1),
        ("DELETE FROM mount_rs_tidb_compact_guards", 0),
        ("UPDATE mount_rs_tidb_metadata", 1),
    ] {
        assert_eq!(
            queries.iter().filter(|s| s.starts_with(prefix)).count(),
            count,
            "{prefix}"
        );
    }
    proxy.begin();
    let current = store.load_compact_inode(f.backing, new).await.unwrap();
    assert_eq!(proxy.end().1, 1);
    let mut changed = current.guard.node;
    changed.stats.mtime_ms += 1;
    proxy.begin();
    store
        .publish_compact_inode(
            f.backing,
            new,
            current.generation,
            current.guard.identity,
            changed,
        )
        .await
        .unwrap();
    let (queries, rows) = proxy.end();
    assert_eq!(rows, 1);
    assert_eq!(
        queries
            .iter()
            .filter(|s| s.starts_with("UPDATE mount_rs_tidb_compact_guards"))
            .count(),
        1
    );
    assert_eq!(
        queries
            .iter()
            .filter(|s| s.starts_with("UPDATE mount_rs_tidb_metadata"))
            .count(),
        0
    );
    proxy.begin();
    let after = store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(proxy.end().1, 130);
    for &id in expected.keys() {
        assert_eq!(before.guards[&id], after.guards[&id]);
    }
    store.close().await.unwrap();
    f.store.close().await.unwrap();
    blocks.close().await.unwrap();
    let fresh = TidbMetadataStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let blocks = TidbBlockStore::connect_with_key(&f.url, &f.key)
        .await
        .unwrap();
    let snapshot = fresh.load_compact_snapshot(f.backing).await.unwrap();
    for (id, bytes) in expected {
        let node = &snapshot.guards[&id].node;
        let NodeData::File(layout) = &node.data else {
            panic!()
        };
        let mut actual = vec![0; node.stats.size as usize];
        for extent in &layout.extents {
            let block = blocks.get(&extent.block).await.unwrap();
            actual[extent.file_offset as usize..(extent.file_offset + extent.length) as usize]
                .copy_from_slice(
                    &block[extent.block_offset as usize
                        ..(extent.block_offset + extent.length) as usize],
                );
        }
        assert_eq!(actual, bytes);
    }
    eprintln!(
        "compact TiDB128 control: setup/full capture excluded; targeted create actual rows fetched1, guard inserts1 updates1 deletes0 anchor updates1; selected load rows1; selected publication rows1 updates1 anchor updates0; full snapshot rows130; untouched physical guards128 and all128 full-byte fresh-store oracles PASS"
    );
    fresh.close().await.unwrap();
    blocks.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_selected_ignores_root_lock_and_rechecks_authority_after_guard_wait() {
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let pool = Pool::from_url(&f.url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    conn.query_drop("SET SESSION tidb_txn_mode='pessimistic'")
        .await
        .unwrap();
    let (mode, isolation): (String, String) = conn
        .query_first("SELECT @@SESSION.tidb_txn_mode,@@SESSION.transaction_isolation")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mode, "pessimistic");
    assert_eq!(isolation, "REPEATABLE-READ");
    let mut tx = conn
        .start_transaction(mysql_async::TxOpts::default())
        .await
        .unwrap();
    let _: Option<i64> = tx
        .exec_first(
            "SELECT revision FROM mount_rs_tidb_metadata WHERE volume_key=? FOR UPDATE",
            (&f.key,),
        )
        .await
        .unwrap();
    let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let mut node = old.guard.node.clone();
    node.stats.mtime_ms += 1;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        f.store
            .publish_compact_inode(f.backing, a, old.generation, old.guard.identity, node),
    )
    .await;
    tx.rollback().await.unwrap();
    result
        .expect("unrelated root lock must not block selected publication")
        .unwrap();
    let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let mut tx = conn
        .start_transaction(mysql_async::TxOpts::default())
        .await
        .unwrap();
    let _:Option<i64>=tx.exec_first("SELECT revision FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=? FOR UPDATE",(&f.key,a)).await.unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    proxy.begin();
    proxy.trace.pause_guards.store(true, Ordering::SeqCst);
    let backing = f.backing;
    let writing = tokio::spawn(async move {
        let mut node = old.guard.node;
        node.stats.mtime_ms += 1;
        let result = writer
            .publish_compact_inode(backing, a, old.generation, old.guard.identity, node)
            .await;
        writer.close().await.unwrap();
        result
    });
    proxy.reached().await;
    proxy.trace.resume.notify_one();
    // Writer has begun before unrelated structural commit. Its guard is locked.
    create(&f, "independent").await;
    assert!(!writing.is_finished());
    tx.rollback().await.unwrap();
    assert!(writing.await.unwrap().unwrap_err().is(ErrorCode::Eagain));
    drop(conn);
    pool.disconnect().await.unwrap();
    f.store.close().await.unwrap();
}

type Raw = (
    (
        i64,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        i64,
        i64,
        Option<String>,
        Option<String>,
    ),
    Vec<(i64, i64, i64, i64, String)>,
    Option<Vec<u8>>,
    Vec<i64>,
    Vec<(i64, i64, Vec<u8>, Vec<u8>, i64)>,
);
async fn raw(f: &Fixture) -> Raw {
    let pool = Pool::from_url(&f.url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    let authority=conn.exec_first("SELECT revision,write_mode,backing_id,owner,fence,expires,namespace,delegation FROM mount_rs_tidb_metadata WHERE volume_key=?",(&f.key,)).await.unwrap().unwrap();
    let rows=conn.exec("SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards WHERE volume_key=? ORDER BY inode",(&f.key,)).await.unwrap();
    let marker: Option<Vec<u8>> = conn
        .exec_first(
            "SELECT backing_id FROM mount_rs_tidb_block_authority WHERE volume_key=?",
            (&f.key,),
        )
        .await
        .unwrap();
    let members = conn
        .exec(
            "SELECT inode FROM mount_rs_tidb_compact_members WHERE volume_key=? ORDER BY inode",
            (&f.key,),
        )
        .await
        .unwrap();
    let dentries = conn
        .exec(
            "SELECT parent,ordinal,name_hash,name,inode FROM mount_rs_tidb_compact_dentries WHERE volume_key=? ORDER BY parent,ordinal",
            (&f.key,),
        )
        .await
        .unwrap();
    drop(conn);
    pool.disconnect().await.unwrap();
    (authority, rows, marker, members, dentries)
}

#[path = "support/indexed_compact.rs"]
mod indexed_compact;
#[path = "support/indexed_point_scope.rs"]
mod indexed_point_scope;
#[path = "support/indexed_write_query.rs"]
mod indexed_write_query;
#[path = "support/structural_lock_scope.rs"]
mod structural_lock_scope;
async fn corrupt(f: &Fixture, sql: &str) {
    let pool = Pool::from_url(&f.url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    conn.exec_drop(sql, (&f.key,)).await.unwrap();
    drop(conn);
    pool.disconnect().await.unwrap();
}
#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_mode_discovery_preserves_authority_and_rejects_deleted_markers() {
    let virgin_key = format!(
        "compact-discovery-virgin-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let virgin = TidbMetadataStore::connect_with_key(
        &std::env::var("MOUNT_RS_TIDB_URL").unwrap(),
        &virgin_key,
    )
    .await
    .unwrap();
    assert_eq!(virgin.compact_inode_mode_state().await.unwrap(), None);
    let f = fixture(false).await;
    let before = raw(&f).await;
    assert_eq!(f.store.compact_inode_mode_state().await.unwrap(), None);
    assert_eq!(raw(&f).await, before);
    let mrc4 = fixture(false).await;
    mrc4.store
        .prepare_inode_mode(mrc4.backing, 1)
        .await
        .unwrap();
    let before_mrc4 = raw(&mrc4).await;
    assert_eq!(mrc4.store.compact_inode_mode_state().await.unwrap(), None);
    assert_eq!(raw(&mrc4).await, before_mrc4);
    f.store
        .prepare_compact_inode_mode(f.backing, 1)
        .await
        .unwrap();
    let before = raw(&f).await;
    assert_eq!(
        f.store.compact_inode_mode_state().await.unwrap(),
        Some(mount_rs_core::storage::InodeModeState {
            backing: f.backing,
            structural_generation: 2
        })
    );
    assert_eq!(raw(&f).await, before);
    corrupt(
        &f,
        "UPDATE mount_rs_tidb_metadata SET write_mode=NULL,backing_id=NULL WHERE volume_key=?",
    )
    .await;
    let damaged = raw(&f).await;
    assert!(f.store.compact_inode_mode_state().await.is_err());
    assert_eq!(raw(&f).await, damaged);
}
#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_mode_discovery_rejects_retained_mrc5_authority_after_mode_change() {
    let mut accepted = Vec::new();
    for mode in ["MRC2", "MRC4"] {
        let f = fixture(true).await;
        let pool = Pool::from_url(&f.url).unwrap();
        let mut conn = pool.get_conn().await.unwrap();
        conn.exec_drop(
            "UPDATE mount_rs_tidb_metadata SET write_mode=? WHERE volume_key=?",
            (mode, &f.key),
        )
        .await
        .unwrap();
        drop(conn);
        pool.disconnect().await.unwrap();
        let before = raw(&f).await;
        if f.store.compact_inode_mode_state().await.is_ok() {
            accepted.push(mode);
        }
        assert_eq!(raw(&f).await, before, "{mode}");
    }
    assert!(
        accepted.is_empty(),
        "accepted retained MRC5 authority as {accepted:?}"
    );
}
#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_mode_discovery_rejects_each_retained_mrc5_marker_alone() {
    for (mode, anchor_only) in [("MRC2", true), ("MRC2", false), ("MRC4", true)] {
        let f = fixture(false).await;
        let old_namespace = raw(&f).await.0.6.unwrap();
        f.store
            .prepare_compact_inode_mode(f.backing, 1)
            .await
            .unwrap();
        let pool = Pool::from_url(&f.url).unwrap();
        let mut conn = pool.get_conn().await.unwrap();
        conn.exec_drop(
            "UPDATE mount_rs_tidb_metadata SET write_mode=? WHERE volume_key=?",
            (mode, &f.key),
        )
        .await
        .unwrap();
        drop(conn);
        pool.disconnect().await.unwrap();
        if anchor_only {
            for table in [
                "mount_rs_tidb_compact_guards",
                "mount_rs_tidb_compact_members",
                "mount_rs_tidb_compact_dentries",
            ] {
                corrupt(&f, &format!("DELETE FROM {table} WHERE volume_key=?")).await;
            }
        } else {
            let pool = Pool::from_url(&f.url).unwrap();
            let mut conn = pool.get_conn().await.unwrap();
            conn.exec_drop(
                "UPDATE mount_rs_tidb_metadata SET namespace=? WHERE volume_key=?",
                (&old_namespace, &f.key),
            )
            .await
            .unwrap();
            drop(conn);
            pool.disconnect().await.unwrap();
        }
        let before = raw(&f).await;
        if anchor_only {
            assert!(before.1.is_empty(), "anchor-only control retains no guards");
            assert!(
                before.3.is_empty(),
                "anchor-only control retains no members"
            );
            assert!(
                before.4.is_empty(),
                "anchor-only control retains no dentries"
            );
        }
        assert!(
            f.store.compact_inode_mode_state().await.is_err(),
            "mode={mode} anchor_only={anchor_only}"
        );
        assert_eq!(raw(&f).await, before);
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_authority_and_corruption_refusals_preserve_bytes() {
    let cases = [
        (
            "owner",
            "UPDATE mount_rs_tidb_metadata SET owner='expired' WHERE volume_key=?",
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
            "backing",
            "UPDATE mount_rs_tidb_metadata SET backing_id='11111111111111111111111111111111' WHERE volume_key=?",
        ),
        (
            "generation",
            "UPDATE mount_rs_tidb_metadata SET revision=revision+1 WHERE volume_key=?",
        ),
        (
            "mode",
            "UPDATE mount_rs_tidb_metadata SET write_mode='MRC4' WHERE volume_key=?",
        ),
        (
            "delegation",
            "UPDATE mount_rs_tidb_metadata SET delegation='{}' WHERE volume_key=?",
        ),
        (
            "malformed anchor",
            "UPDATE mount_rs_tidb_metadata SET namespace='{}' WHERE volume_key=?",
        ),
        (
            "future epoch",
            "UPDATE mount_rs_tidb_compact_guards SET epoch=999 WHERE volume_key=? AND inode=2",
        ),
        (
            "zero incarnation",
            "UPDATE mount_rs_tidb_compact_guards SET incarnation=0 WHERE volume_key=? AND inode=2",
        ),
        (
            "negative revision",
            "UPDATE mount_rs_tidb_compact_guards SET revision=-1 WHERE volume_key=? AND inode=2",
        ),
        (
            "malformed body",
            "UPDATE mount_rs_tidb_compact_guards SET node='{}' WHERE volume_key=? AND inode=2",
        ),
        (
            "equal-count different membership",
            "UPDATE mount_rs_tidb_compact_guards SET inode=42 WHERE volume_key=? AND inode=2",
        ),
    ];
    for (name, sql) in cases {
        let f = fixture(true).await;
        let a = create(&f, "a").await;
        let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let delta = CompactStructuralDelta::capture(
            &snapshot,
            &snapshot.namespace().unwrap(),
            StructuralScope::Full,
        )
        .unwrap();
        let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
        corrupt(&f, sql).await;
        let before = raw(&f).await;
        assert!(
            f.store.load_compact_snapshot(f.backing).await.is_err(),
            "{name}"
        );
        assert!(
            f.store.load_compact_inode(f.backing, a).await.is_err(),
            "{name}"
        );
        assert!(
            f.store
                .publish_compact_inode(
                    f.backing,
                    a,
                    old.generation,
                    old.guard.identity,
                    old.guard.node
                )
                .await
                .is_err(),
            "{name}"
        );
        assert!(
            f.store.publish_compact_structure(&delta).await.is_err(),
            "{name}"
        );
        assert_eq!(raw(&f).await, before, "{name}");
        eprintln!(
            "compact TiDB negative {name}: snapshot/load/selected/full refused; exact raw bytes preserved"
        );
        f.store.close().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_signed_arithmetic_and_byte_limits_are_pre_mutation() {
    use mount_rs_tidb::TidbStorageOptions;
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let current = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let before = raw(&f).await;
    let bounded = TidbMetadataStore::connect_with_options(
        &f.url,
        TidbStorageOptions::new(&f.key).with_max_namespace_bytes(1),
    )
    .await
    .unwrap();
    let mut changed = current.guard.node.clone();
    changed.stats.mtime_ms += 1;
    assert!(
        bounded
            .publish_compact_inode(
                f.backing,
                a,
                current.generation,
                current.guard.identity,
                changed
            )
            .await
            .unwrap_err()
            .is(ErrorCode::Efbig)
    );
    let mut ns = snapshot.namespace().unwrap();
    add_file(&mut ns, "b");
    let delta =
        CompactStructuralDelta::capture(&snapshot, &ns, StructuralScope::FileCreate).unwrap();
    assert!(
        bounded
            .publish_compact_structure(&delta)
            .await
            .unwrap_err()
            .is(ErrorCode::Efbig)
    );
    assert_eq!(raw(&f).await, before);
    ns.next_inode = i64::MAX as u64 + 1;
    let delta = CompactStructuralDelta::capture(&snapshot, &ns, StructuralScope::Full).unwrap();
    assert!(
        f.store
            .publish_compact_structure(&delta)
            .await
            .unwrap_err()
            .is(ErrorCode::Eoverflow)
    );
    assert_eq!(raw(&f).await, before);
    corrupt(&f,"UPDATE mount_rs_tidb_compact_guards SET revision=9223372036854775807 WHERE volume_key=? AND inode=2").await;
    let current = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let before = raw(&f).await;
    let mut changed = current.guard.node;
    changed.stats.mtime_ms += 1;
    assert!(
        f.store
            .publish_compact_inode(
                f.backing,
                a,
                current.generation,
                current.guard.identity,
                changed
            )
            .await
            .unwrap_err()
            .is(ErrorCode::Eoverflow)
    );
    assert_eq!(raw(&f).await, before);
    bounded.close().await.unwrap();
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_client_packet_limit_rejects_all_upserts_before_mutation() {
    use mount_rs_core::storage::{BlockExtent, BlockId};
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut url = url::Url::parse(&f.url).unwrap();
    url.query_pairs_mut()
        .append_pair("max_allowed_packet", "65536");
    let proxy = compact_proxy::Proxy::new(url.as_str()).await;
    let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    let mut node = before.guards[&a].node.clone();
    node.stats.size = 1024;
    node.stats.blocks = 2;
    let NodeData::File(layout) = &mut node.data else {
        panic!()
    };
    layout.extents = (0..1024)
        .map(|offset| BlockExtent {
            file_offset: offset,
            block: BlockId(format!("b{}", "1".repeat(64))),
            block_offset: 0,
            length: 1,
        })
        .collect();
    assert!(serde_json::to_vec(&node).unwrap().len() > 65536);
    let raw_before = raw(&f).await;
    proxy.begin();
    assert!(
        writer
            .publish_compact_inode(
                f.backing,
                a,
                before.anchor.generation,
                before.guards[&a].identity,
                node.clone()
            )
            .await
            .unwrap_err()
            .is(ErrorCode::Efbig)
    );
    let (queries, _) = proxy.end();
    assert!(
        !queries.iter().any(|s| s.starts_with("UPDATE ")
            || s.starts_with("INSERT ")
            || s.starts_with("DELETE "))
    );
    let mut ns = before.namespace().unwrap();
    let new = add_file(&mut ns, "oversized");
    node.stats.ino = new;
    ns.nodes.insert(new, node);
    let delta = CompactStructuralDelta::capture(&before, &ns, StructuralScope::FileCreate).unwrap();
    proxy.begin();
    assert!(
        writer
            .publish_compact_structure(&delta)
            .await
            .unwrap_err()
            .is(ErrorCode::Efbig)
    );
    let (queries, _) = proxy.end();
    assert!(
        !queries.iter().any(|s| s.starts_with("UPDATE ")
            || s.starts_with("INSERT ")
            || s.starts_with("DELETE "))
    );
    assert_eq!(raw(&f).await, raw_before);
    eprintln!(
        "compact TiDB packet control: client cap65536; oversized selected/create bodies rejected EFBIG; actual mutation statements0; raw authority+guard bytes unchanged"
    );
    writer.close().await.unwrap();
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_compact_captured_delta_preserves_new_unrelated_body_and_full_conflicts() {
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = base.namespace().unwrap();
    add_file(&mut ns, "b");
    let targeted =
        CompactStructuralDelta::capture(&base, &ns, StructuralScope::FileCreate).unwrap();
    let full = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
    let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let mut node = old.guard.node;
    node.stats.mtime_ms += 13;
    let selected = f
        .store
        .publish_compact_inode(f.backing, a, old.generation, old.guard.identity, node)
        .await
        .unwrap();
    let before = raw(&f).await;
    assert!(
        f.store
            .publish_compact_structure(&full)
            .await
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    assert_eq!(raw(&f).await, before);
    f.store.publish_compact_structure(&targeted).await.unwrap();
    let after = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(after.guards[&a], selected.guard);
    assert_eq!(after.anchor.generation, base.anchor.generation + 1);
    f.store.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual owned TiDB, MOUNT_RS_TIDB_URL and MOUNT_RS_PROFILE_IO=1; run serial"]
async fn actual_compact_selected_load_uses_one_autocommit_joined_query() {
    use mount_rs_core::diagnostics::storage;
    assert!(
        storage::enabled(),
        "run this actual control with MOUNT_RS_PROFILE_IO=1"
    );
    let f = fixture(true).await;
    let inode = create(&f, "joined-read").await;
    let expected = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (_, _, audited) = expected.clone().into_validated_namespace().unwrap();
    let file = &expected.guards[&inode];
    let expectation =
        CompactFileExpectation::from_structure(&audited, file.identity, &file.node).unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;
    let reader = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
        .await
        .unwrap();
    // Warm the owned pool/session and prepared statement outside measurement.
    reader
        .read_compact_file(f.backing, inode, expectation)
        .await
        .unwrap();
    proxy.begin();
    let before = storage::snapshot();
    let loaded = reader
        .read_compact_file(f.backing, inode, expectation)
        .await
        .unwrap();
    let delta = storage::snapshot().delta(&before).unwrap();
    let (queries, rows) = proxy.end();
    // Expected semantic RED must still explicitly settle owned provider resources.
    reader.close().await.unwrap();
    f.store.close().await.unwrap();

    assert_eq!(loaded.generation(), expected.anchor.generation);
    loaded.validate_expectation(expectation).unwrap();
    let CompactInodeRead::Unchanged(checked) = loaded.into_inode_read().unwrap() else {
        panic!("canonical scoped file bytes must certify the unchanged expectation");
    };
    assert_eq!(checked.identity(), file.identity);
    assert!(checked.into_verified_root().is_none());
    assert_eq!(rows, 1, "one complete selected guard row must be observed");
    let selects: Vec<_> = queries
        .iter()
        .filter(|sql| sql.starts_with("SELECT "))
        .collect();
    assert_eq!(
        selects.len(),
        1,
        "one actual SQL SELECT must supply small authority, selected member and complete file"
    );
    assert!(selects[0].contains("mount_rs_tidb_metadata"));
    assert!(selects[0].contains("LEFT JOIN mount_rs_tidb_compact_guards"));
    assert!(selects[0].contains("LEFT JOIN mount_rs_tidb_compact_members"));
    assert!(!selects[0].contains("mount_rs_tidb_compact_dentries"));
    assert!(
        !selects[0].contains("FOR UPDATE"),
        "selected read remains nonlocking"
    );
    assert!(
        queries.iter().all(|sql| {
            !sql.starts_with("SET ")
                && !sql.starts_with("START TRANSACTION")
                && !sql.eq_ignore_ascii_case("BEGIN")
                && !sql.eq_ignore_ascii_case("COMMIT")
                && !sql.eq_ignore_ascii_case("ROLLBACK")
        }),
        "selected read must send no transaction or session control SQL: {queries:?}"
    );
    for (name, calls) in [
        ("tidb.pool.checkout", 1),
        ("tidb.tx.begin.compact_read", 0),
        ("tidb.tx.rollback", 0),
        ("tidb.sql.inode_read", 1),
        ("tidb.sql.metadata_read", 0),
        ("tidb.tx.commit", 0),
    ] {
        let row = delta
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap();
        assert_eq!(row.calls, calls, "{name}");
        assert_eq!(row.success, calls, "{name}");
        assert_eq!(row.error, 0, "{name}");
        assert_eq!(row.cancelled, 0, "{name}");
    }
    assert_eq!(delta.in_flight, 0);
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_compact_selected_load_reuses_one_session_after_drop_and_cancellation() {
    use mount_rs_tidb::{TidbPoolContext, TidbStorageOptions};
    use std::time::Duration;
    use tokio::time::timeout;

    const BOUND: Duration = Duration::from_secs(10);
    let f = fixture(true).await;
    let inode = create(&f, "session-reuse").await;
    let original = f.store.load_compact_inode(f.backing, inode).await.unwrap();
    let initial = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (_, _, old_audited) = initial.clone().into_validated_namespace().unwrap();
    let authority = CompactAuthority::from_anchor(&initial.anchor).unwrap();
    let expected = CompactFileExpectation::from_authority(
        &authority,
        original.guard.identity,
        &original.guard.node,
    )
    .unwrap();
    let proxy = compact_proxy::Proxy::new(&f.url).await;

    // Trace starts before the first connection. A cap of one and exactly one
    // verified session callback pair distinguish warm reuse from reconnects.
    proxy.begin();
    let context = TidbPoolContext::new(&proxy.url, 1).unwrap();
    let reader = context
        .metadata(TidbStorageOptions::new(&f.key))
        .await
        .unwrap();
    let warm_first = timeout(BOUND, reader.read_compact_file(f.backing, inode, expected)).await;
    let warm_second = timeout(BOUND, reader.read_compact_file(f.backing, inode, expected)).await;
    let (warm_queries, _) = proxy.end();

    // The independent writer advances the physical identity. Rejecting an
    // update with the previous identity drops an already-started, tracked
    // transaction; the cap-one checkout must wait for recycler rollback.
    let mut updated_node = original.guard.node.clone();
    updated_node.stats.mtime_ms += 1;
    let updated = f
        .store
        .publish_compact_inode(
            f.backing,
            inode,
            original.generation,
            original.guard.identity,
            updated_node,
        )
        .await
        .unwrap();

    // Capture the future independent structural publication before creating
    // any deliberately interrupted work. It changes both authority generation
    // and the selected guard's contents, so stale snapshots cannot pass.
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut fresh_namespace = base.namespace().unwrap();
    fresh_namespace
        .nodes
        .get_mut(&inode)
        .unwrap()
        .stats
        .mtime_ms += 13;
    add_file(&mut fresh_namespace, "after-cancel");
    let fresh_delta =
        CompactStructuralDelta::capture(&base, &fresh_namespace, StructuralScope::Full).unwrap();

    proxy.begin();
    let rejected = timeout(
        BOUND,
        reader.publish_compact_inode(
            f.backing,
            inode,
            updated.generation,
            original.guard.identity,
            original.guard.node.clone(),
        ),
    )
    .await;
    let after_drop = timeout(BOUND, reader.read_compact_file(f.backing, inode, expected)).await;
    let (drop_queries, _) = proxy.end();

    let before_cancel = timeout(BOUND, raw(&f)).await;
    proxy.begin();
    proxy
        .trace
        .pause_guard_response
        .store(true, Ordering::SeqCst);
    let interrupted_reader = reader.clone();
    let backing = f.backing;
    let interrupted_authority = authority.clone();
    let interrupted_file = original.guard.clone();
    let reading = tokio::spawn(async move {
        let expected = CompactFileExpectation::from_authority(
            &interrupted_authority,
            interrupted_file.identity,
            &interrupted_file.node,
        )
        .unwrap();
        interrupted_reader
            .read_compact_file(backing, inode, expected)
            .await
    });
    // The response pause holds an actual binary guard row already returned
    // by TiDB. Cancellation therefore follows server execution of the FILE
    // hint. Releasing the row lets the cap-one pool drain or discard the result.
    let reached = timeout(BOUND, proxy.trace.reached.notified()).await;
    reading.abort();
    let canceled = reading.await;
    proxy.trace.resume.notify_one();

    let after_cancel_before_publication = timeout(BOUND, raw(&f)).await;
    let published = timeout(BOUND, f.store.publish_compact_structure(&fresh_delta)).await;
    let fresh_snapshot = timeout(BOUND, f.store.load_compact_snapshot(f.backing)).await;
    let before_recovery = timeout(BOUND, raw(&f)).await;
    let after_cancel = timeout(BOUND, reader.read_compact_file(f.backing, inode, expected)).await;
    let after_cancel_again =
        timeout(BOUND, reader.read_compact_file(f.backing, inode, expected)).await;
    let recovered_root_stale = timeout(
        BOUND,
        reader.read_compact_root_entry(
            f.backing,
            initial.anchor.root,
            inode,
            "session-reuse",
            expected,
        ),
    )
    .await;
    let recovered_root_fresh = timeout(
        BOUND,
        reader.read_compact_root_entry(
            f.backing,
            initial.anchor.root,
            inode,
            "session-reuse",
            expected,
        ),
    )
    .await;
    let (cancel_queries, cancel_rows) = proxy.end();
    let paused_guard_rows = proxy.trace.paused_guard_rows.load(Ordering::SeqCst);
    let after_recovery = timeout(BOUND, raw(&f)).await;

    // Attempt every shutdown before semantic assertions. Store close releases
    // its lifecycle; the shared context owns disconnect. Proxy shutdown drains
    // relay tasks and drops their socket halves before its listener completes.
    let reader_closed = timeout(BOUND, reader.close()).await;
    let context_closed = timeout(BOUND, context.close()).await;
    let fixture_closed = timeout(BOUND, f.store.close()).await;
    let proxy_closed = timeout(BOUND, proxy.shutdown()).await;

    reader_closed.expect("reader close completed").unwrap();
    context_closed
        .expect("shared context disconnect completed")
        .unwrap();
    fixture_closed
        .expect("independent fixture disconnect completed")
        .unwrap();
    proxy_closed.expect("proxy listener and relays settled");

    let joined = |sql: &String| {
        compact_proxy::joined_authority_select(sql)
            && sql.contains(" LEFT JOIN mount_rs_tidb_compact_guards AS g ")
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
    for loaded in [warm_first, warm_second] {
        let loaded = loaded.expect("warm cap-one checkout completed").unwrap();
        assert_eq!(loaded.generation(), original.generation);
        loaded.validate_expectation(expected).unwrap();
        let CompactInodeRead::Unchanged(checked) = loaded.into_inode_read().unwrap() else {
            panic!("warm file must certify complete unchanged bytes")
        };
        assert_eq!(checked.identity(), original.guard.identity);
    }

    assert!(
        rejected
            .expect("stale selected update completed")
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    let after_drop = after_drop
        .expect("checkout after tracked transaction drop completed")
        .unwrap();
    assert_eq!(after_drop.generation(), updated.generation);
    after_drop.validate_expectation(expected).unwrap();
    let CompactInodeRead::Loaded(after_drop) = after_drop.into_inode_read().unwrap() else {
        panic!("changed physical identity must return fresh owned file")
    };
    assert_eq!(after_drop.guard, updated.guard);
    let rollback = drop_queries
        .iter()
        .position(|sql| sql.eq_ignore_ascii_case("ROLLBACK"))
        .expect("dropped tracked transaction was rolled back");
    let selected = drop_queries
        .iter()
        .position(joined)
        .expect("selected read followed tracked transaction cleanup");
    assert!(rollback < selected);
    assert_eq!(
        session_sets(&drop_queries),
        0,
        "cleanup reused the configured session"
    );
    assert_eq!(session_checks(&drop_queries), 0);

    reached.expect("actual selected guard row reached controlled response pause");
    assert_eq!(
        paused_guard_rows, 1,
        "one server-produced FILE row was paused"
    );
    assert!(canceled.unwrap_err().is_cancelled());
    assert_eq!(
        before_cancel.expect("pre-cancel frame completed"),
        after_cancel_before_publication.expect("post-cancel frame completed"),
        "canceled server-executed FILE read preserves every captured row"
    );
    assert_eq!(
        before_recovery.expect("pre-recovery frame completed"),
        after_recovery.expect("post-recovery frame completed"),
        "recovered FILE and ROOT reads preserve every captured fresh row"
    );
    let publication = published
        .expect("fresh independent structural publication completed")
        .unwrap();
    assert!(publication.anchor.generation > updated.generation);
    let expected_guard = &publication.upserts[&inode];
    assert_eq!(expected_guard.node, fresh_namespace.nodes[&inode]);
    assert_ne!(expected_guard, &updated.guard);
    let fresh_snapshot = fresh_snapshot
        .expect("fresh complete snapshot completed")
        .unwrap();
    assert_eq!(fresh_snapshot.anchor, publication.anchor);
    assert_eq!(&fresh_snapshot.guards[&inode], expected_guard);
    assert_eq!(
        serde_json::to_value(fresh_snapshot.namespace().unwrap()).unwrap(),
        serde_json::to_value(&fresh_namespace).unwrap()
    );
    let (_, _, fresh_audited) = fresh_snapshot.into_validated_namespace().unwrap();
    for loaded in [after_cancel, after_cancel_again] {
        let loaded = loaded
            .expect("selected read recovered without cap-one starvation")
            .unwrap();
        assert_eq!(loaded.generation(), publication.anchor.generation);
        assert!(
            loaded
                .validate_expectation(expected)
                .unwrap_err()
                .is(ErrorCode::Eagain),
            "caller observes changed authority before admitting a selected result"
        );
        let CompactInodeRead::Loaded(loaded) = loaded.into_inode_read().unwrap() else {
            panic!("fresh structure must return a fresh complete file")
        };
        assert_eq!(&loaded.guard, expected_guard);
    }
    let root_stale = recovered_root_stale
        .expect("unhinted ROOT recovered without cap-one starvation")
        .unwrap();
    assert_eq!(root_stale.generation(), publication.anchor.generation);
    assert!(
        root_stale
            .into_inode_read(&old_audited)
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    let root_fresh = recovered_root_fresh
        .expect("second unhinted ROOT recovery completed")
        .unwrap();
    assert_eq!(root_fresh.generation(), publication.anchor.generation);
    let CompactInodeRead::Loaded(root_fresh) = root_fresh.into_inode_read(&fresh_audited).unwrap()
    else {
        panic!("recovered ROOT must expose the complete fresh selected guard")
    };
    assert_eq!(&root_fresh.guard, expected_guard);
    assert_eq!(cancel_queries.iter().filter(|sql| joined(sql)).count(), 3);
    let root_queries: Vec<_> = cancel_queries
        .iter()
        .filter(|sql| {
            sql.contains(" LEFT JOIN mount_rs_tidb_compact_guards AS r ")
                && sql.contains(" LEFT JOIN mount_rs_tidb_compact_guards AS f ")
        })
        .collect();
    assert_eq!(root_queries.len(), 2);
    assert!(root_queries.iter().all(|sql| {
        sql.starts_with("SELECT m.revision,m.write_mode,m.backing_id,m.owner,")
            && !sql.contains("SET_VAR")
    }));
    assert_eq!(
        cancel_rows, 5,
        "one canceled FILE, two fresh FILE and two ROOT rows"
    );
    // Safe reuse or one newly verified replacement is permitted after abort.
    // These counts do not claim a server connection ID or blanket discard.
    assert_eq!(
        session_sets(&cancel_queries),
        session_checks(&cancel_queries)
    );
    assert!(session_sets(&cancel_queries) <= 1);
}
