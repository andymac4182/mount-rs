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

async fn fixture(enroll: bool) -> Fixture {
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("explicit actual TiDB URL required");
    let key = format!(
        "compact-tidb-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
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
);
async fn raw(f: &Fixture) -> Raw {
    let pool = Pool::from_url(&f.url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    let authority=conn.exec_first("SELECT revision,write_mode,backing_id,owner,fence,expires,namespace,delegation FROM mount_rs_tidb_metadata WHERE volume_key=?",(&f.key,)).await.unwrap().unwrap();
    let rows=conn.exec("SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards WHERE volume_key=? ORDER BY inode",(&f.key,)).await.unwrap();
    drop(conn);
    pool.disconnect().await.unwrap();
    (authority, rows)
}
async fn corrupt(f: &Fixture, sql: &str) {
    let pool = Pool::from_url(&f.url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    conn.exec_drop(sql, (&f.key,)).await.unwrap();
    drop(conn);
    pool.disconnect().await.unwrap();
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
