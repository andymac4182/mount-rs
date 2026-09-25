//! Actual owned FoundationDB diagnostics. No capacity or transport-fault claim.
use super::*;
use futures_util::TryStreamExt;
use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{DirectoryEntry, FileLayout, Namespace, NodeData, NodeMetadata};
use mount_rs_core::{S_IFDIR, S_IFREG, Stats};
use std::collections::BTreeMap;
const NODES: usize = 131;
fn template_namespace() -> Namespace {
    let chunker = FixedSizeChunker::new(4096).unwrap().config();
    let root_stats = Stats {
        dev: 0,
        ino: 1,
        mode: S_IFDIR | 0o755,
        nlink: 2,
        uid: 0,
        gid: 0,
        rdev: 0,
        size: 0,
        blksize: 4096,
        blocks: 0,
        atime_ms: 0,
        mtime_ms: 0,
        ctime_ms: 0,
        birthtime_ms: 0,
    };
    let mut nodes = BTreeMap::new();
    let mut entries = Vec::new();
    for inode in 2..=NODES as u64 {
        let mut stats = root_stats.clone();
        stats.ino = inode;
        stats.mode = S_IFREG | 0o644;
        stats.nlink = 1;
        nodes.insert(
            inode,
            NodeMetadata {
                stats,
                data: NodeData::File(FileLayout {
                    chunker: chunker.clone(),
                    extents: vec![],
                }),
            },
        );
        entries.push(DirectoryEntry {
            name: format!("f{inode}"),
            inode,
        });
    }
    nodes.insert(
        1,
        NodeMetadata {
            stats: root_stats,
            data: NodeData::Directory { entries },
        },
    );
    Namespace {
        format_version: 1,
        root: 1,
        next_inode: NODES as u64 + 1,
        default_uid: 0,
        default_gid: 0,
        umask: 0o022,
        default_chunker: chunker,
        nodes,
    }
}

struct Fixture {
    storage: FoundationDbStorage,
    store: FoundationDbMetadataStore,
    key: String,
    backing: ConcurrentBackingId,
}
fn reopen(f: &Fixture) -> FoundationDbStorage {
    FoundationDbStorage::connect(
        std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").unwrap(),
        FoundationDbStorageOptions::new(&f.key),
    )
    .unwrap()
}
async fn fixture(enroll: bool) -> Fixture {
    fixture_limits(enroll, FoundationDbLimits::default()).await
}
async fn fixture_limits(enroll: bool, limits: FoundationDbLimits) -> Fixture {
    let key = format!(
        "mount-rs/compact-fdb/{}/{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let storage = FoundationDbStorage::connect(
        std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").expect("actual fixture required"),
        FoundationDbStorageOptions::new(&key).with_limits(limits),
    )
    .unwrap();
    let store = storage.metadata();
    let backing = storage.blocks().prepare_concurrent_backing().await.unwrap();
    store.prepare_bound_concurrent_mode(backing).await.unwrap();
    store
        .publish_bound_if_revision(backing, 0, root_namespace())
        .await
        .unwrap();
    if enroll {
        store.prepare_compact_inode_mode(backing, 1).await.unwrap();
    }
    Fixture {
        storage,
        store,
        key,
        backing,
    }
}
async fn raw(f: &Fixture) -> Vec<(Vec<u8>, Vec<u8>)> {
    let trx = f.storage.inner.db.create_trx().unwrap();
    let p = Keyspace::new(f.key.as_bytes()).key(b"meta/");
    let end = range_end(&p).unwrap();
    trx.get_ranges_keyvalues((p.as_slice(), end.as_slice()).into(), false)
        .map_ok(|kv| (kv.key().to_vec(), kv.value().to_vec()))
        .try_collect()
        .await
        .unwrap()
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_mode_discovery_preserves_virgin_and_enrolled_keys() {
    let key = format!("mount-rs/compact-discovery-virgin/{}", uuid::Uuid::new_v4());
    let virgin = FoundationDbStorage::connect(
        std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").unwrap(),
        FoundationDbStorageOptions::new(&key),
    )
    .unwrap();
    assert_eq!(
        virgin.metadata().compact_inode_mode_state().await.unwrap(),
        None
    );
    let trx = virgin.inner.db.create_trx().unwrap();
    let prefix = Keyspace::new(key.as_bytes()).key(b"meta/");
    let end = range_end(&prefix).unwrap();
    let rows: Vec<_> = trx
        .get_ranges_keyvalues((prefix.as_slice(), end.as_slice()).into(), false)
        .map_ok(|kv| (kv.key().to_vec(), kv.value().to_vec()))
        .try_collect()
        .await
        .unwrap();
    assert!(rows.is_empty());

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
        Some(InodeModeState {
            backing: f.backing,
            structural_generation: 2
        })
    );
    assert_eq!(raw(&f).await, before);
    assert!(f.store.inode_mode_state().await.is_err());
    let keys = Keyspace::new(f.key.as_bytes());
    replace(&f, &keys.write_mode(), None).await;
    replace(&f, &keys.metadata_backing(), None).await;
    let damaged = raw(&f).await;
    assert!(f.store.compact_inode_mode_state().await.is_err());
    assert_eq!(raw(&f).await, damaged);
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_mode_discovery_rejects_retained_mrc5_keys_after_mode_change() {
    let mut accepted = Vec::new();
    for mode in [b"MRC2".as_slice(), b"MRC4".as_slice()] {
        let f = fixture(true).await;
        let keys = Keyspace::new(f.key.as_bytes());
        replace(&f, &keys.write_mode(), Some(mode)).await;
        let before = raw(&f).await;
        if f.store.compact_inode_mode_state().await.is_ok() {
            accepted.push(mode);
        }
        assert_eq!(raw(&f).await, before, "{mode:?}");
    }
    assert!(
        accepted.is_empty(),
        "accepted retained MRC5 authority as {accepted:?}"
    );
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_mode_discovery_rejects_each_retained_mrc5_marker_alone() {
    for (mode, anchor_only) in [
        (b"MRC2".as_slice(), true),
        (b"MRC2".as_slice(), false),
        (b"MRC4".as_slice(), true),
    ] {
        let f = fixture(false).await;
        let keys = Keyspace::new(f.key.as_bytes());
        let old = raw(&f).await;
        f.store
            .prepare_compact_inode_mode(f.backing, 1)
            .await
            .unwrap();
        replace(&f, &keys.write_mode(), Some(mode)).await;
        if anchor_only {
            replace(&f, &raw_guard_key(&f, 1), None).await;
        } else {
            for key in [keys.manifest(), metadata_chunk_key(&keys.chunks(), 0)] {
                let value = old.iter().find(|(candidate, _)| candidate == &key).unwrap();
                replace(&f, &key, Some(&value.1)).await;
            }
        }
        let before = raw(&f).await;
        assert!(
            f.store.compact_inode_mode_state().await.is_err(),
            "mode={mode:?} anchor_only={anchor_only}"
        );
        assert_eq!(raw(&f).await, before);
    }
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_mode_discovery_rejects_anchor_split_across_short_chunks() {
    let mut accepted = Vec::new();
    for chunk_bytes in [1, 16] {
        let limits = FoundationDbLimits {
            metadata_chunk_bytes: chunk_bytes,
            max_metadata_bytes: 4096,
            ..FoundationDbLimits::default()
        };
        let valid = fixture_limits(false, limits).await;
        let mrc2 = raw(&valid).await;
        assert_eq!(valid.store.compact_inode_mode_state().await.unwrap(), None);
        assert_eq!(raw(&valid).await, mrc2);
        valid
            .store
            .prepare_inode_mode(valid.backing, 1)
            .await
            .unwrap();
        let mrc4 = raw(&valid).await;
        assert_eq!(valid.store.compact_inode_mode_state().await.unwrap(), None);
        assert_eq!(raw(&valid).await, mrc4);
        for mode in [b"MRC2".as_slice(), b"MRC4".as_slice()] {
            let f = fixture_limits(true, limits).await;
            let keys = Keyspace::new(f.key.as_bytes());
            let first_chunk = metadata_chunk_key(&keys.chunks(), 0);
            let enrolled = raw(&f).await;
            let first = &enrolled
                .iter()
                .find(|(key, _)| key == &first_chunk)
                .unwrap()
                .1;
            assert_eq!(first.len(), chunk_bytes);
            assert!(first.len() < b"{\"layout\":\"mount-rs-compact-inodes\"".len());
            replace(&f, &keys.write_mode(), Some(mode)).await;
            replace(&f, &raw_guard_key(&f, 1), None).await;
            let before = raw(&f).await;
            assert!(
                !before
                    .iter()
                    .any(|(key, _)| key.starts_with(&keys.key(b"meta/compact-guard/")))
            );
            if f.store.compact_inode_mode_state().await.is_ok() {
                accepted.push((chunk_bytes, mode));
            }
            assert_eq!(
                raw(&f).await,
                before,
                "chunk_bytes={chunk_bytes} mode={mode:?}"
            );
            if chunk_bytes == 1 {
                replace(&f, &metadata_chunk_key(&keys.chunks(), 1), None).await;
                let truncated = raw(&f).await;
                assert!(f.store.compact_inode_mode_state().await.is_err());
                assert_eq!(raw(&f).await, truncated);
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "accepted split compact anchor as {accepted:?}"
    );
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_mode_discovery_rejects_mrc1_lease_but_accepts_legacy_lease() {
    let key = format!("mount-rs/compact-discovery-lease/{}", uuid::Uuid::new_v4());
    let storage = FoundationDbStorage::connect(
        std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").unwrap(),
        FoundationDbStorageOptions::new(&key),
    )
    .unwrap();
    let store = storage.metadata();
    let f = Fixture {
        storage,
        store,
        key,
        backing: ConcurrentBackingId::from_bytes([0x61; 16]).unwrap(),
    };
    let keys = Keyspace::new(f.key.as_bytes());
    replace(&f, &keys.lease(), Some(b"stray lease")).await;
    let legacy = raw(&f).await;
    assert_eq!(f.store.compact_inode_mode_state().await.unwrap(), None);
    assert_eq!(raw(&f).await, legacy);
    replace(&f, &keys.write_mode(), Some(b"MRC1")).await;
    replace(&f, &keys.fence(), Some(CONCURRENT_FENCE_SENTINEL)).await;
    let damaged = raw(&f).await;
    assert!(f.store.compact_inode_mode_state().await.is_err());
    assert_eq!(raw(&f).await, damaged);
}
fn root_namespace() -> Namespace {
    let mut ns = template_namespace();
    ns.nodes.retain(|id, _| *id == ns.root);
    ns.nodes.get_mut(&ns.root).unwrap().data = NodeData::Directory { entries: vec![] };
    ns.next_inode = ns.root + 1;
    ns
}
fn add_file(ns: &mut Namespace, name: &str) -> u64 {
    let id = ns.next_inode;
    let template = template_namespace();
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
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_enrollment_fences_and_reopens() {
    let f = fixture(false).await;
    assert_eq!(
        f.store.compact_inode_capability(),
        CompactInodeCapability::V1
    );
    f.store
        .prepare_compact_inode_mode(f.backing, 1)
        .await
        .unwrap();
    let snap = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(snap.anchor.generation, 2);
    assert_eq!(
        snap.guards[&1].identity,
        PhysicalInodeIdentity {
            incarnation: 2,
            epoch: 2,
            revision: 0
        }
    );
    assert_eq!(
        serde_json::to_value(snap.namespace().unwrap()).unwrap(),
        serde_json::to_value(root_namespace()).unwrap()
    );
    assert!(f.store.load().await.unwrap_err().is(ErrorCode::Estale));
    for revision in [1, 2] {
        assert!(
            f.store
                .load_if_changed(revision)
                .await
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
    }
    assert!(f.store.load_inode_snapshot(f.backing).await.is_err());
    assert!(
        f.store
            .load_inode_snapshot_if_changed(f.backing, Some(2))
            .await
            .is_err()
    );
    assert!(f.store.concurrent_mode_state().await.is_err());
    assert!(
        f.store
            .publish_bound_if_revision(f.backing, 2, root_namespace())
            .await
            .is_err()
    );
    assert!(
        f.store
            .acquire_writer("old", Duration::from_secs(10))
            .await
            .is_err()
    );
    let bytes = raw(&f).await;
    let keys = Keyspace::new(f.key.as_bytes());
    let manifest =
        decode_manifest(&bytes.iter().find(|(k, _)| *k == keys.manifest()).unwrap().1).unwrap();
    assert_eq!(manifest.revision, 2);
    let mut body = Vec::new();
    for i in 0..manifest.chunk_count {
        body.extend(
            &bytes
                .iter()
                .find(|(k, _)| *k == metadata_chunk_key(&keys.chunks(), i))
                .unwrap()
                .1,
        );
    }
    assert!(serde_json::from_slice::<Namespace>(&body).is_err());
    assert!(mount_rs_core::storage::decode_inode_namespace(&body).is_err());
    assert!(decode_compact_anchor(&body).is_ok());
    assert_eq!(
        reopen(&f)
            .metadata()
            .load_compact_snapshot(f.backing)
            .await
            .unwrap(),
        snap
    );
}

#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
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
}

#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_same_inode_one_winner_and_unrelated_preserved() {
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let b = create(&f, "b").await;
    let first = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let second = f.store.load_compact_inode(f.backing, b).await.unwrap();
    let other = reopen(&f).metadata();
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
}

#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
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
    // An open detached regular file remains writable while its orphan guard
    // exists. Its fresh physical identity, not the old linked token, is used.
    let loaded = f.store.load_compact_inode(f.backing, a).await.unwrap();
    let mut node = loaded.guard.node.clone();
    node.stats.mtime_ms += 17;
    let written = f
        .store
        .publish_compact_inode(f.backing, a, loaded.generation, loaded.guard.identity, node)
        .await
        .unwrap();
    assert_eq!(written.guard.node.stats.nlink, 0);
    let orphan = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(orphan.guards[&a], written.guard);
    let mut ns = orphan.namespace().unwrap();
    ns.nodes.remove(&a);
    let delta = CompactStructuralDelta::capture(&orphan, &ns, StructuralScope::Full).unwrap();
    let receipt = f.store.publish_compact_structure(&delta).await.unwrap();
    assert_eq!(receipt.removed, std::collections::BTreeSet::from([a]));
    assert!(receipt.upserts.is_empty());
    let after = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert!(!after.guards.contains_key(&a));
    assert_eq!(after.guards[&b], base.guards[&b]);
}

#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
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
}

#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
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
}

#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_legacy_mode_probe_refuses_enrolled_authority() {
    let f = fixture(true).await;
    assert!(
        f.store
            .inode_mode_state()
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
}

async fn replace(f: &Fixture, key: &[u8], value: Option<&[u8]>) {
    let trx = f.storage.inner.db.create_trx().unwrap();
    match value {
        Some(bytes) => trx.set(key, bytes),
        None => trx.clear(key),
    }
    trx.commit().await.unwrap();
}
fn raw_guard_key(f: &Fixture, inode: u64) -> Vec<u8> {
    let mut key = Keyspace::new(f.key.as_bytes()).key(b"meta/compact-guard/");
    key.extend(inode.to_be_bytes());
    key
}
async fn assert_rejected_unchanged(f: &Fixture, inode: u64, old: &LoadedCompactInode) {
    let before = raw(f).await;
    assert!(f.store.load_compact_snapshot(f.backing).await.is_err());
    assert!(f.store.load_compact_inode(f.backing, inode).await.is_err());
    assert!(
        f.store
            .publish_compact_inode(
                f.backing,
                inode,
                old.generation,
                old.guard.identity,
                old.guard.node.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(raw(f).await, before);
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_corrupt_authority_and_selected_records_preserve_raw_bytes() {
    let f = fixture(true).await;
    let id = create(&f, "a").await;
    let old = f.store.load_compact_inode(f.backing, id).await.unwrap();
    let keys = Keyspace::new(f.key.as_bytes());
    for (label, key, value) in [
        ("mode", keys.write_mode(), b"MRC4".to_vec()),
        ("backing", keys.metadata_backing(), vec![3; 16]),
        ("policy", keys.block_authority_policy(), b"MRBP1E".to_vec()),
        ("block authority", keys.block_authority(), vec![3; 16]),
        ("lease", keys.lease(), b"invalid".to_vec()),
        ("fence", keys.fence(), b"invalid".to_vec()),
        (
            "delegation",
            keys.key(b"meta/delegation"),
            b"invalid".to_vec(),
        ),
        ("manifest", keys.manifest(), b"invalid".to_vec()),
        (
            "chunk",
            metadata_chunk_key(&keys.chunks(), 0),
            b"invalid".to_vec(),
        ),
    ] {
        let saved = get_owned(&f.storage.inner.db.create_trx().unwrap(), &key)
            .await
            .unwrap();
        replace(&f, &key, Some(&value)).await;
        assert_rejected_unchanged(&f, id, &old).await;
        replace(&f, &key, saved.as_deref()).await;
        eprintln!("compact corruption {label}: refused, actual raw metadata bytes unchanged");
    }
    let key = raw_guard_key(&f, id);
    let good = get_owned(&f.storage.inner.db.create_trx().unwrap(), &key)
        .await
        .unwrap()
        .unwrap();
    let mut variants = Vec::new();
    variants.push(("missing", None));
    variants.push((
        "MRI4 header",
        Some([b"MRI4".as_slice(), &good[4..]].concat()),
    ));
    variants.push(("header only", Some(good[..28].to_vec())));
    let mut bytes = good.clone();
    bytes[4..12].fill(0);
    variants.push(("zero incarnation", Some(bytes)));
    let mut bytes = good.clone();
    bytes[12..20].copy_from_slice(&u64::MAX.to_be_bytes());
    variants.push(("future epoch", Some(bytes)));
    let mut node = old.guard.node.clone();
    node.stats.ino += 1;
    variants.push((
        "body inode",
        Some([&good[..28], &serde_json::to_vec(&node).unwrap()].concat()),
    ));
    let mut value = serde_json::to_value(&old.guard.node).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("unknown".into(), true.into());
    variants.push((
        "unknown body field",
        Some([&good[..28], &serde_json::to_vec(&value).unwrap()].concat()),
    ));
    for (label, bytes) in variants {
        replace(&f, &key, bytes.as_deref()).await;
        assert_rejected_unchanged(&f, id, &old).await;
        replace(&f, &key, Some(&good)).await;
        eprintln!("compact guard {label}: refused unchanged");
    }
}

#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_exact_membership_graph_and_created_absence_fail_closed() {
    let f = fixture(true).await;
    let id = create(&f, "a").await;
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = base.namespace().unwrap();
    let created = add_file(&mut ns, "b");
    let targeted =
        CompactStructuralDelta::capture(&base, &ns, StructuralScope::FileCreate).unwrap();
    let full = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
    let guard_key = raw_guard_key(&f, id);
    let good = get_owned(&f.storage.inner.db.create_trx().unwrap(), &guard_key)
        .await
        .unwrap()
        .unwrap();
    for key in [
        raw_guard_key(&f, created),
        [raw_guard_key(&f, id), vec![0]].concat(),
    ] {
        replace(&f, &key, Some(&good)).await;
        let before = raw(&f).await;
        assert!(f.store.load_compact_snapshot(f.backing).await.is_err());
        assert!(f.store.publish_compact_structure(&full).await.is_err());
        if key == raw_guard_key(&f, created) {
            assert!(f.store.publish_compact_structure(&targeted).await.is_err());
        }
        assert_eq!(raw(&f).await, before);
        replace(&f, &key, None).await;
    }
    let root_key = raw_guard_key(&f, 1);
    let good = get_owned(&f.storage.inner.db.create_trx().unwrap(), &root_key)
        .await
        .unwrap()
        .unwrap();
    let mut root = base.guards[&1].node.clone();
    root.data = NodeData::Directory { entries: vec![] };
    replace(
        &f,
        &root_key,
        Some(&[&good[..28], &serde_json::to_vec(&root).unwrap()].concat()),
    )
    .await;
    let before = raw(&f).await;
    assert!(f.store.load_compact_snapshot(f.backing).await.is_err());
    assert!(f.store.publish_compact_structure(&full).await.is_err());
    assert_eq!(raw(&f).await, before);
}

#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_enrollment_rejects_history_and_chunk_tail() {
    for suffix in [
        b"meta/inode/".as_slice(),
        b"meta/inode-version/",
        b"meta/compact-guard/",
        b"meta/delegation",
    ] {
        let f = fixture(false).await;
        let key = Keyspace::new(f.key.as_bytes()).key(suffix);
        replace(&f, &key, Some(b"history")).await;
        let before = raw(&f).await;
        assert!(
            f.store
                .prepare_compact_inode_mode(f.backing, 1)
                .await
                .is_err()
        );
        assert_eq!(raw(&f).await, before);
    }
    let f = fixture(false).await;
    let key = metadata_chunk_key(&Keyspace::new(f.key.as_bytes()).chunks(), 100);
    replace(&f, &key, Some(b"tail")).await;
    let before = raw(&f).await;
    assert!(
        f.store
            .prepare_compact_inode_mode(f.backing, 1)
            .await
            .is_err()
    );
    assert_eq!(raw(&f).await, before);
}

// A test-only single-use gate, local to the handle. Dropped release means a
// deliberate error before commit; aborting the task drops the native transaction.
pub(super) struct Control {
    point: &'static str,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}
pub(super) async fn pause(inner: &Inner, point: &str) -> TxnResult<()> {
    let control = {
        let mut slot = inner.compact_control.lock().unwrap();
        if slot.as_ref().is_some_and(|c| c.point == point) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(c) = control {
        let _ = c.entered.send(());
        c.release
            .await
            .map_err(|_| TxnError::Fs(backend_error("test abort before commit")))?;
    }
    Ok(())
}
fn gate(
    f: &Fixture,
    point: &'static str,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    *f.storage.inner.compact_control.lock().unwrap() = Some(Control {
        point,
        entered: entered_tx,
        release: release_rx,
    });
    f.storage
        .inner
        .metadata_test_attempts
        .store(0, Ordering::SeqCst);
    (entered, release)
}
async fn entered(signal: tokio::sync::oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(3), signal)
        .await
        .unwrap()
        .unwrap();
}
async fn publish_other(f: &Fixture, inode: u64) -> LoadedCompactInode {
    let store = reopen(f).metadata();
    let old = store.load_compact_inode(f.backing, inode).await.unwrap();
    let mut node = old.guard.node;
    node.stats.mtime_ms += 1;
    store
        .publish_compact_inode(f.backing, inode, old.generation, old.guard.identity, node)
        .await
        .unwrap()
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_snapshot_remains_at_one_read_version_during_two_updates() {
    let f = fixture(true).await;
    let a = create(&f, "a").await;
    let b = create(&f, "b").await;
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let (signal, release) = gate(&f, "anchor");
    let store = f.store.clone();
    let backing = f.backing;
    let task = tokio::spawn(async move { store.load_compact_snapshot(backing).await });
    entered(signal).await;
    let one = publish_other(&f, a).await;
    let two = publish_other(&f, b).await;
    assert_eq!(one.generation, before.anchor.generation);
    assert_eq!(two.generation, before.anchor.generation);
    release.send(()).unwrap();
    let during = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(during, before);
    let after = reopen(&f)
        .metadata()
        .load_compact_snapshot(backing)
        .await
        .unwrap();
    assert_eq!(after.guards[&a], one.guard);
    assert_eq!(after.guards[&b], two.guard);
    eprintln!(
        "coherent native read-version control: anchor read, two actual selected commits, old complete guard snapshot; generation unchanged"
    );
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_point_conflicts_and_anchor_race_are_bounded() {
    for race in ["same", "independent", "anchor"] {
        let f = fixture(true).await;
        let a = create(&f, "a").await;
        let b = create(&f, "b").await;
        let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
        let mut node = old.guard.node;
        node.stats.mtime_ms += 7;
        let (signal, release) = gate(&f, "before_apply");
        let store = f.store.clone();
        let backing = f.backing;
        let task = tokio::spawn(async move {
            store
                .publish_compact_inode(backing, a, old.generation, old.guard.identity, node)
                .await
        });
        entered(signal).await;
        let other = reopen(&f).metadata();
        match race {
            "same" => {
                publish_other(&f, a).await;
            }
            "independent" => {
                publish_other(&f, b).await;
            }
            _ => {
                let snap = other.load_compact_snapshot(backing).await.unwrap();
                let mut ns = snap.namespace().unwrap();
                add_file(&mut ns, "c");
                other
                    .publish_compact_structure(
                        &CompactStructuralDelta::capture(&snap, &ns, StructuralScope::FileCreate)
                            .unwrap(),
                    )
                    .await
                    .unwrap();
            }
        }
        release.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        let attempts = f
            .storage
            .inner
            .metadata_test_attempts
            .load(Ordering::SeqCst);
        if race == "independent" {
            assert!(result.is_ok());
            assert_eq!(attempts, 1);
        } else {
            assert!(result.unwrap_err().is(ErrorCode::Eagain));
            assert_eq!(attempts, 2);
        }
        eprintln!(
            "bounded native {race} race: closure attempts {attempts}; ordinary input conflicts verified"
        );
    }
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_cancel_and_abort_after_local_mutations_leave_no_commit() {
    for cancel in [false, true] {
        let f = fixture(true).await;
        let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let mut ns = base.namespace().unwrap();
        add_file(&mut ns, "a");
        let delta =
            CompactStructuralDelta::capture(&base, &ns, StructuralScope::FileCreate).unwrap();
        let before = raw(&f).await;
        let (signal, release) = gate(&f, "after_apply");
        let store = f.store.clone();
        let task = tokio::spawn(async move { store.publish_compact_structure(&delta).await });
        entered(signal).await;
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            drop(release);
        } else {
            drop(release);
            assert!(task.await.unwrap().is_err());
        }
        assert_eq!(raw(&f).await, before);
        assert_eq!(
            reopen(&f)
                .metadata()
                .load_compact_snapshot(f.backing)
                .await
                .unwrap(),
            base
        );
        eprintln!(
            "native local mutation {}: fresh raw bytes and complete snapshot unchanged",
            if cancel {
                "cancellation"
            } else {
                "error abort"
            }
        );
    }
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_real_commit_with_synthetic_lost_ack_is_not_replayed() {
    let f = fixture(true).await;
    let inode = create(&f, "a").await;
    let old = f.store.load_compact_inode(f.backing, inode).await.unwrap();
    let mut node = old.guard.node;
    node.stats.mtime_ms += 29;
    f.storage
        .inner
        .metadata_test_attempts
        .store(0, Ordering::SeqCst);
    *f.storage.inner.metadata_test_fault.lock().unwrap() =
        Some(MetadataTestFault::AfterCommitAckLoss);
    let error = f
        .store
        .publish_compact_inode(
            f.backing,
            inode,
            old.generation,
            old.guard.identity,
            node.clone(),
        )
        .await
        .unwrap_err();
    assert!(!error.is(ErrorCode::Eagain));
    assert!(error.to_string().contains("1021"));
    assert_eq!(
        f.storage
            .inner
            .metadata_test_attempts
            .load(Ordering::SeqCst),
        1
    );
    let fresh = reopen(&f)
        .metadata()
        .load_compact_inode(f.backing, inode)
        .await
        .unwrap();
    assert_eq!(fresh.guard.node, node);
    assert_eq!(fresh.guard.identity.revision, 1);
    assert_eq!(fresh.generation, old.generation);
    assert!(
        !transaction_options(
            f.storage.inner.limits,
            TransactionPolicy::FailClosedOnMaybeCommitted
        )
        .is_idempotent
    );
    eprintln!(
        "synthetic lost ACK AFTER actual commit: one attempt, nonretryable1021, independent fresh full body/revision1 verified; no network/transport injection claim"
    );
}

fn trace_reset(f: &Fixture) {
    f.storage.inner.compact_trace.lock().unwrap().clear();
}
fn trace(f: &Fixture) -> Vec<(&'static str, Vec<u8>, usize)> {
    f.storage.inner.compact_trace.lock().unwrap().clone()
}
fn no_mutations(f: &Fixture) {
    assert!(
        !trace(f)
            .iter()
            .any(|(op, _, _)| ["set", "clear", "clear_range"].contains(op))
    );
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB and MOUNT_RS_PROFILE_IO=1"]
async fn actual_compact_128_control_and_independent_complete_payload_oracles() {
    use mount_rs_core::diagnostics::profile;
    use mount_rs_core::storage::BlockExtent;
    assert!(
        profile::enabled(),
        "set MOUNT_RS_PROFILE_IO=1 before process start"
    );
    let f = fixture(true).await;
    let blocks = f.storage.blocks();
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = base.namespace().unwrap();
    let mut expected = BTreeMap::new();
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
    f.store
        .publish_compact_structure(
            &CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap(),
        )
        .await
        .unwrap();
    let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = before.namespace().unwrap();
    let new = add_file(&mut ns, "new");
    let delta = CompactStructuralDelta::capture(&before, &ns, StructuralScope::FileCreate).unwrap();
    trace_reset(&f);
    let start = profile::snapshot();
    let receipt = f.store.publish_compact_structure(&delta).await.unwrap();
    let create_profile = profile::snapshot().delta(&start).unwrap();
    // Include canonical validation's reserialization of the fetched parent.
    assert_eq!(
        create_profile
            .entries
            .iter()
            .find(|e| e.name == "provider.inode_serialized_bytes")
            .unwrap()
            .calls,
        3
    );
    let ops = trace(&f);
    let prefix = Keyspace::new(f.key.as_bytes()).key(b"meta/compact-guard/");
    assert_eq!(
        ops.iter()
            .filter(|(op, k, _)| *op == "get" && k.starts_with(&prefix))
            .count(),
        2
    );
    assert_eq!(
        ops.iter()
            .filter(|(op, k, n)| *op == "get" && k.starts_with(&prefix) && *n > 0)
            .count(),
        1
    );
    assert_eq!(
        ops.iter()
            .filter(|(op, k, _)| *op == "set" && k.starts_with(&prefix))
            .count(),
        2
    );
    assert!(
        !ops.iter()
            .any(|(op, k, _)| *op == "range" && k.starts_with(&prefix))
    );
    assert!(!ops.iter().any(|(op, _, _)| op.starts_with("clear")));
    assert_eq!(receipt.upserts.len(), 2);
    let keys = Keyspace::new(f.key.as_bytes());
    assert_eq!(
        ops.iter()
            .filter(|(op, k, _)| *op == "set" && *k == keys.manifest())
            .count(),
        1
    );
    eprintln!(
        "128 create profile {}",
        serde_json::to_string(&create_profile).unwrap()
    );
    trace_reset(&f);
    let loaded = f.store.load_compact_inode(f.backing, new).await.unwrap();
    assert_eq!(
        trace(&f)
            .iter()
            .filter(|(op, k, _)| *op == "get" && k.starts_with(&prefix))
            .count(),
        1
    );
    let mut node = loaded.guard.node;
    node.stats.mtime_ms += 1;
    trace_reset(&f);
    let start = profile::snapshot();
    f.store
        .publish_compact_inode(
            f.backing,
            new,
            loaded.generation,
            loaded.guard.identity,
            node,
        )
        .await
        .unwrap();
    let ops = trace(&f);
    assert_eq!(ops.iter().filter(|(op, _, _)| *op == "set").count(), 1);
    assert_eq!(
        ops.iter()
            .filter(|(op, k, _)| *op == "set" && k.starts_with(&prefix))
            .count(),
        1
    );
    eprintln!(
        "128 selected profile {}",
        serde_json::to_string(&profile::snapshot().delta(&start).unwrap()).unwrap()
    );
    let fresh = reopen(&f);
    let snapshot = fresh
        .metadata()
        .load_compact_snapshot(f.backing)
        .await
        .unwrap();
    assert_eq!(snapshot.guards.len(), 130);
    for (inode, bytes) in expected {
        assert_eq!(snapshot.guards[&inode], before.guards[&inode]);
        let loaded = fresh
            .metadata()
            .load_compact_inode(f.backing, inode)
            .await
            .unwrap();
        assert_eq!(loaded.guard, before.guards[&inode]);
        let node = &loaded.guard.node;
        let NodeData::File(layout) = &node.data else {
            panic!()
        };
        let mut actual = vec![0; node.stats.size as usize];
        for extent in &layout.extents {
            let block = fresh.blocks().get(&extent.block).await.unwrap();
            actual[extent.file_offset as usize..(extent.file_offset + extent.length) as usize]
                .copy_from_slice(
                    &block[extent.block_offset as usize
                        ..(extent.block_offset + extent.length) as usize],
                );
        }
        assert_eq!(actual, bytes);
    }
    eprintln!(
        "128 native control PASS: publication excludes setup/full capture; create guard GET2 (parent body1 + absence1), SET2, guard range0, manifest SET1; selected GET1 SET1 anchor writes0; independent fresh complete guards128/full payload bytes128 verified. Logical backend operations, not native physical IO. Anchor and capture remain O(N)."
    );
}

fn large_file(ns: &mut Namespace, inode: u64, block: &BlockId, target: usize) {
    use mount_rs_core::storage::BlockExtent;
    let node = ns.nodes.get_mut(&inode).unwrap();
    let NodeData::File(layout) = &mut node.data else {
        panic!()
    };
    // Fixed 1-byte extents on an actual immutable block; contiguous valid layout.
    for i in 0..target {
        layout.extents.push(BlockExtent {
            file_offset: i as u64,
            block: block.clone(),
            block_offset: 0,
            length: 1,
        });
    }
    node.stats.size = target as u64;
    node.stats.blocks = (target as u64).div_ceil(512);
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_value_configured_transaction_and_arithmetic_bounds_are_pre_mutation() {
    // Native value and configured metadata bounds exercise selected publication.
    for (label, limits, extents) in [
        (
            "configured metadata",
            FoundationDbLimits {
                metadata_chunk_bytes: 128,
                max_metadata_bytes: 2048,
                ..FoundationDbLimits::default()
            },
            30,
        ),
        ("native value", FoundationDbLimits::default(), 1000),
    ] {
        let f = fixture_limits(true, limits).await;
        let id = create(&f, "a").await;
        let block = f.storage.blocks().put(b"x").await.unwrap();
        let old = f.store.load_compact_inode(f.backing, id).await.unwrap();
        let mut ns = f
            .store
            .load_compact_snapshot(f.backing)
            .await
            .unwrap()
            .namespace()
            .unwrap();
        large_file(&mut ns, id, &block, extents);
        let before = raw(&f).await;
        trace_reset(&f);
        assert!(
            f.store
                .publish_compact_inode(
                    f.backing,
                    id,
                    old.generation,
                    old.guard.identity,
                    ns.nodes.remove(&id).unwrap()
                )
                .await
                .unwrap_err()
                .is(ErrorCode::Efbig)
        );
        no_mutations(&f);
        assert_eq!(raw(&f).await, before);
        eprintln!("{label} bound: before first mutation, actual bytes unchanged");
    }
    let f = fixture(true).await;
    let block = f.storage.blocks().put(b"x").await.unwrap();
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = base.namespace().unwrap();
    for n in 0..128 {
        let id = add_file(&mut ns, &format!("budget{n}"));
        large_file(&mut ns, id, &block, 600);
    }
    let encoded = serde_json::to_vec(&ns.nodes[&2]).unwrap().len();
    assert!(encoded < 100_000);
    assert!(encoded * 128 > 10_000_000);
    let delta = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
    let before = raw(&f).await;
    trace_reset(&f);
    let error = f.store.publish_compact_structure(&delta).await.unwrap_err();
    assert!(error.is(ErrorCode::Efbig));
    no_mutations(&f);
    assert_eq!(raw(&f).await, before);
    eprintln!(
        "transaction affected-byte bound:128 valid guards each{encoded} bytes, refused before first mutation"
    );
    let f = fixture(true).await;
    let id = create(&f, "a").await;
    let key = raw_guard_key(&f, id);
    let mut bytes = get_owned(&f.storage.inner.db.create_trx().unwrap(), &key)
        .await
        .unwrap()
        .unwrap();
    bytes[20..28].copy_from_slice(&u64::MAX.to_be_bytes());
    replace(&f, &key, Some(&bytes)).await;
    let old = f.store.load_compact_inode(f.backing, id).await.unwrap();
    let before = raw(&f).await;
    trace_reset(&f);
    assert!(
        f.store
            .publish_compact_inode(
                f.backing,
                id,
                old.generation,
                old.guard.identity,
                old.guard.node
            )
            .await
            .unwrap_err()
            .is(ErrorCode::Eoverflow)
    );
    no_mutations(&f);
    assert_eq!(raw(&f).await, before);
    let f = fixture(false).await;
    let before = raw(&f).await;
    trace_reset(&f);
    assert!(
        f.store
            .prepare_compact_inode_mode(f.backing, u64::MAX)
            .await
            .unwrap_err()
            .is(ErrorCode::Eoverflow)
    );
    no_mutations(&f);
    assert_eq!(raw(&f).await, before);
}

async fn replace_anchor(f: &Fixture, anchor: &CompactAnchor) {
    let bytes = encode_compact_anchor(anchor).unwrap();
    replace_anchor_bytes(f, &bytes, anchor.generation).await;
}
async fn replace_anchor_bytes(f: &Fixture, bytes: &[u8], generation: u64) {
    let keys = Keyspace::new(f.key.as_bytes());
    let trx = f.storage.inner.db.create_trx().unwrap();
    let prefix = keys.chunks();
    trx.clear_range(&prefix, &range_end(&prefix).unwrap());
    let size = f.storage.inner.limits.metadata_chunk_bytes;
    for (i, chunk) in bytes.chunks(size).enumerate() {
        trx.set(&metadata_chunk_key(&prefix, i as u32), chunk);
    }
    trx.set(
        &keys.manifest(),
        &encode_manifest(Manifest {
            revision: generation,
            chunk_count: bytes.len().div_ceil(size) as u32,
            payload_len: bytes.len() as u64,
        }),
    );
    trx.commit().await.unwrap();
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_strict_anchor_envelope_and_allocation_bounds() {
    let f = fixture(true).await;
    let id = create(&f, "a").await;
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let old = f.store.load_compact_inode(f.backing, id).await.unwrap();
    let encoded = encode_compact_anchor(&base.anchor).unwrap();
    for (label, pointer, value) in [
        ("future version", "/version", serde_json::json!(2)),
        ("wrong envelope", "/layout", serde_json::json!("MRC4")),
        (
            "wrong backing",
            "/anchor/backing",
            serde_json::to_value(ConcurrentBackingId::from_bytes([9; 16]).unwrap()).unwrap(),
        ),
        (
            "generation mismatch",
            "/anchor/generation",
            serde_json::json!(100),
        ),
        (
            "unsorted membership",
            "/anchor/members",
            serde_json::json!([2, 1]),
        ),
        (
            "missing membership",
            "/anchor/members",
            serde_json::json!([1]),
        ),
        (
            "next below member",
            "/anchor/next_inode",
            serde_json::json!(2),
        ),
    ] {
        let mut value_json: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        *value_json.pointer_mut(pointer).unwrap() = value;
        replace_anchor_bytes(
            &f,
            &serde_json::to_vec(&value_json).unwrap(),
            base.anchor.generation,
        )
        .await;
        assert_rejected_unchanged(&f, id, &old).await;
        replace_anchor(&f, &base.anchor).await;
        eprintln!("anchor {label}: fail closed unchanged");
    }
    let mut anchor = base.anchor.clone();
    anchor.generation = u64::MAX;
    replace_anchor(&f, &anchor).await;
    let exhausted = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = exhausted.namespace().unwrap();
    add_file(&mut ns, "overflow");
    let before = raw(&f).await;
    assert!(
        CompactStructuralDelta::capture(&exhausted, &ns, StructuralScope::FileCreate)
            .unwrap_err()
            .is(ErrorCode::Eoverflow)
    );
    assert_eq!(raw(&f).await, before);
    let mut anchor = base.anchor.clone();
    anchor.next_inode = u64::MAX;
    replace_anchor(&f, &anchor).await;
    let exhausted = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = exhausted.namespace().unwrap();
    let mut node = template_namespace().nodes[&2].clone();
    node.stats.ino = u64::MAX;
    ns.nodes.insert(u64::MAX, node);
    let before = raw(&f).await;
    assert!(CompactStructuralDelta::capture(&exhausted, &ns, StructuralScope::Full).is_err());
    assert_eq!(raw(&f).await, before);
}
#[tokio::test]
#[ignore = "requires actual owned FoundationDB"]
async fn actual_compact_chunk_tail_shrinks_atomically_without_guard_rebuild() {
    let f = fixture_limits(
        true,
        FoundationDbLimits {
            metadata_chunk_bytes: 128,
            ..FoundationDbLimits::default()
        },
    )
    .await;
    let base = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = base.namespace().unwrap();
    for n in 0..128 {
        add_file(&mut ns, &format!("tail{n}"));
    }
    f.store
        .publish_compact_structure(
            &CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap(),
        )
        .await
        .unwrap();
    let full = f.store.load_compact_snapshot(f.backing).await.unwrap();
    let mut ns = full.namespace().unwrap();
    ns.nodes.retain(|id, _| *id == 1);
    ns.nodes.get_mut(&1).unwrap().data = NodeData::Directory { entries: vec![] };
    let delta = CompactStructuralDelta::capture(&full, &ns, StructuralScope::Full).unwrap();
    trace_reset(&f);
    f.store.publish_compact_structure(&delta).await.unwrap();
    let ops = trace(&f);
    let keys = Keyspace::new(f.key.as_bytes());
    assert_eq!(
        ops.iter()
            .filter(|(op, k, _)| *op == "clear_range" && k.starts_with(&keys.chunks()))
            .count(),
        1
    );
    assert_eq!(
        ops.iter()
            .filter(|(op, k, _)| *op == "clear_range"
                && k.starts_with(&keys.key(b"meta/compact-guard/")))
            .count(),
        0
    );
    assert_eq!(ops.iter().filter(|(op, _, _)| *op == "clear").count(), 128);
    let snapshot = f.store.load_compact_snapshot(f.backing).await.unwrap();
    assert_eq!(snapshot.guards.len(), 1);
    assert_eq!(snapshot.anchor.next_inode, full.anchor.next_inode);
}
