#![cfg(feature = "foundationdb")]
use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{BlockStore, MetadataStore, Namespace, NodeData, NodeMetadata};
use mount_rs_core::storage::{DirectoryEntry, FileLayout};
use mount_rs_core::{ErrorCode, S_IFDIR, S_IFREG, Stats};
use mount_rs_foundationdb::{FoundationDbStorage, FoundationDbStorageOptions};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
fn empty_namespace() -> Namespace {
    let root = 1;
    let chunker = FixedSizeChunker::new(4096).unwrap();
    let stats = Stats {
        dev: 1,
        ino: root,
        mode: S_IFDIR | 0o755,
        nlink: 2,
        uid: 0,
        gid: 0,
        rdev: 0,
        size: 4096,
        blksize: 4096,
        blocks: 8,
        atime_ms: 1,
        mtime_ms: 1,
        ctime_ms: 1,
        birthtime_ms: 1,
    };
    Namespace {
        format_version: 1,
        root,
        next_inode: root + 1,
        default_uid: 0,
        default_gid: 0,
        umask: 0,
        default_chunker: chunker.config(),
        nodes: BTreeMap::from([(
            root,
            NodeMetadata {
                stats,
                data: NodeData::Directory { entries: vec![] },
            },
        )]),
    }
}

#[tokio::test]
#[ignore = "requires actual foundationdb service"]
async fn independent_inode_cas_and_complete_structural_fold() {
    let cluster_file = std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE")
        .expect("set MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let prefix = format!("mount-rs/inode/{}/{unique}", std::process::id());
    let storage =
        FoundationDbStorage::connect(&cluster_file, FoundationDbStorageOptions::new(&prefix))
            .unwrap();
    let second =
        FoundationDbStorage::connect(&cluster_file, FoundationDbStorageOptions::new(&prefix))
            .unwrap();
    let metadata = storage.metadata();
    let other = second.metadata();
    let backing = storage.blocks().prepare_concurrent_backing().await.unwrap();
    let mut ns = empty_namespace();
    for inode in [2, 3] {
        let mut node = ns.nodes[&1].clone();
        node.stats.ino = inode;
        node.stats.mode = S_IFREG | 0o644;
        node.stats.nlink = 1;
        node.stats.size = 0;
        node.stats.blocks = 0;
        node.data = NodeData::File(FileLayout {
            chunker: ns.default_chunker.clone(),
            extents: vec![],
        });
        ns.nodes.insert(inode, node);
    }
    ns.nodes.get_mut(&1).unwrap().data = NodeData::Directory {
        entries: vec![
            DirectoryEntry {
                name: "a".into(),
                inode: 2,
            },
            DirectoryEntry {
                name: "b".into(),
                inode: 3,
            },
        ],
    };
    ns.next_inode = 4;
    metadata
        .prepare_bound_concurrent_mode(backing)
        .await
        .unwrap();
    let revision = metadata
        .publish_bound_if_revision(backing, 0, ns.clone())
        .await
        .unwrap();
    metadata
        .prepare_inode_mode(backing, revision)
        .await
        .unwrap();
    assert!(metadata.load().await.is_err());
    assert!(metadata.load_if_changed(revision).await.is_err());
    assert!(metadata.concurrent_mode_state().await.is_err());
    assert!(
        metadata
            .publish_bound_if_revision(backing, revision, ns)
            .await
            .is_err()
    );
    assert!(
        metadata
            .acquire_writer("old", Duration::from_secs(10))
            .await
            .is_err()
    );
    // Reproduce the already-running legacy reader, which compares only its
    // known manifest revision and then parses plain Namespace JSON. Enrollment
    // must force it off that unchanged fast path and make its new decode fail.
    let old_reader_db = foundationdb::Database::from_path(&cluster_file).unwrap();
    let old_reader = old_reader_db.create_trx().unwrap();
    let mut manifest_key = prefix.as_bytes().to_vec();
    manifest_key.extend_from_slice(b"\0meta/manifest");
    let root = old_reader
        .get(&manifest_key, false)
        .await
        .unwrap()
        .unwrap()
        .to_vec();
    let enrolled_generation = u64::from_be_bytes(root[4..12].try_into().unwrap());
    assert_eq!(enrolled_generation, revision + 1);
    assert_ne!(enrolled_generation, revision);
    let chunks = u32::from_be_bytes(root[12..16].try_into().unwrap());
    let mut old_payload = Vec::new();
    for index in 0..chunks {
        let mut chunk_key = prefix.as_bytes().to_vec();
        chunk_key.extend_from_slice(b"\0meta/chunk/");
        chunk_key.extend_from_slice(&index.to_be_bytes());
        old_payload.extend_from_slice(&old_reader.get(&chunk_key, false).await.unwrap().unwrap());
    }
    assert!(serde_json::from_slice::<Namespace>(&old_payload).is_err());
    assert!(mount_rs_core::storage::decode_inode_namespace(&old_payload).is_ok());
    drop(old_reader);
    drop(old_reader_db);
    let snapshot = metadata.load_inode_snapshot(backing).await.unwrap();
    assert_eq!(snapshot.structural_generation, enrolled_generation);
    let a = metadata.load_inode(backing, 2).await.unwrap();
    let b = other.load_inode(backing, 3).await.unwrap();
    assert!(
        metadata
            .load_inode_if_changed(backing, 2, Some(a.version))
            .await
            .unwrap()
            .is_none()
    );
    let mut node_a = a.node.clone();
    node_a.stats.mtime_ms += 10;
    let mut node_b = b.node.clone();
    node_b.stats.mtime_ms += 20;
    // Distinct selected guards never share a write key: both first CAS attempts succeed.
    let (va, vb) = tokio::join!(
        metadata.publish_inode_if_version(backing, 2, a.version, node_a.clone()),
        other.publish_inode_if_version(backing, 3, b.version, node_b.clone())
    );
    let va = va.unwrap();
    let vb = vb.unwrap();
    assert_eq!(va.structural_generation, snapshot.structural_generation);
    assert_eq!(vb.structural_generation, snapshot.structural_generation);
    assert_eq!(va.inode_revision, 1);
    assert_eq!(vb.inode_revision, 1);
    assert!(
        metadata
            .publish_structure_if_versions(
                backing,
                snapshot.structural_generation,
                &snapshot.inode_revisions,
                snapshot.namespace
            )
            .await
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    let mut next_a = node_a.clone();
    next_a.stats.mtime_ms += 1;
    let mut competing = node_a.clone();
    competing.stats.mtime_ms += 2;
    let (one, two) = tokio::join!(
        metadata.publish_inode_if_version(backing, 2, va, next_a),
        other.publish_inode_if_version(backing, 2, va, competing)
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    assert!(
        one.err()
            .or_else(|| two.err())
            .unwrap()
            .is(ErrorCode::Eagain)
    );
    let reopened =
        FoundationDbStorage::connect(&cluster_file, FoundationDbStorageOptions::new(&prefix))
            .unwrap();
    let folded = reopened
        .metadata()
        .load_inode_snapshot(backing)
        .await
        .unwrap();
    assert_eq!(folded.namespace.nodes[&3], node_b);
    assert_eq!(folded.inode_revisions[&2], 2);
    assert_eq!(folded.inode_revisions[&3], 1);
    let token = metadata.load_inode(backing, 2).await.unwrap().version;
    let mut structure = folded.namespace.clone();
    if let NodeData::Directory { entries } = &mut structure.nodes.get_mut(&1).unwrap().data {
        entries[0].name = "renamed".into();
    }
    let generation = metadata
        .publish_structure_if_versions(
            backing,
            folded.structural_generation,
            &folded.inode_revisions,
            structure,
        )
        .await
        .unwrap();
    assert_eq!(generation, folded.structural_generation + 1);
    assert!(
        metadata
            .publish_inode_if_version(backing, 2, token, node_a)
            .await
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    let after = other.load_inode_snapshot(backing).await.unwrap();
    assert!(after.inode_revisions.values().all(|v| *v == 0));
    assert_eq!(after.namespace.nodes[&3], node_b);
    let removed = metadata.load_inode(backing, 2).await.unwrap();
    let mut unlinked = after.namespace.clone();
    if let NodeData::Directory { entries } = &mut unlinked.nodes.get_mut(&1).unwrap().data {
        entries.retain(|entry| entry.inode != 2);
    }
    unlinked.nodes.remove(&2);
    metadata
        .publish_structure_if_versions(
            backing,
            after.structural_generation,
            &after.inode_revisions,
            unlinked,
        )
        .await
        .unwrap();
    assert!(
        metadata
            .publish_inode_if_version(backing, 2, removed.version, removed.node)
            .await
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    assert!(
        metadata
            .load_inode(backing, 2)
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );

    // A missing guard under the same structural root is corruption, rather
    // than evidence of an ordinary CAS loss. It must stay fail closed.
    let current = metadata.load_inode(backing, 3).await.unwrap();
    let db = foundationdb::Database::from_path(&cluster_file).unwrap();
    let mut token_key = prefix.as_bytes().to_vec();
    token_key.extend_from_slice(b"\0meta/inode-version/");
    token_key.extend_from_slice(&3u64.to_be_bytes());
    let token = db
        .create_trx()
        .unwrap()
        .get(&token_key, false)
        .await
        .unwrap()
        .unwrap()
        .to_vec();
    let mut mismatched = token.clone();
    mismatched[19] ^= 1;
    let trx = db.create_trx().unwrap();
    trx.set(&token_key, &mismatched);
    trx.commit().await.unwrap();
    assert!(
        metadata
            .load_inode(backing, 3)
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    assert!(
        metadata
            .load_inode_if_changed(backing, 3, Some(current.version))
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    assert!(
        metadata
            .load_inode_snapshot(backing)
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    let trx = db.create_trx().unwrap();
    trx.set(&token_key, b"malformed-token");
    trx.commit().await.unwrap();
    assert!(
        metadata
            .load_inode_if_changed(backing, 3, Some(current.version))
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    assert!(
        metadata
            .load_inode_snapshot(backing)
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    let trx = db.create_trx().unwrap();
    trx.clear(&token_key);
    trx.commit().await.unwrap();
    assert!(
        metadata
            .load_inode(backing, 3)
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
    assert!(
        metadata
            .load_inode_snapshot(backing)
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    assert!(
        metadata
            .publish_inode_if_version(backing, 3, current.version, current.node.clone())
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    let trx = db.create_trx().unwrap();
    trx.set(&token_key, &token);
    trx.commit().await.unwrap();
    assert_eq!(
        metadata.load_inode(backing, 3).await.unwrap().version,
        current.version
    );

    let trx = db.create_trx().unwrap();
    let mut guard_key = prefix.as_bytes().to_vec();
    guard_key.extend_from_slice(b"\0meta/inode/");
    guard_key.extend_from_slice(&3u64.to_be_bytes());
    trx.clear(&guard_key);
    trx.commit().await.unwrap();
    let corrupt = metadata
        .publish_inode_if_version(backing, 3, current.version, current.node)
        .await
        .unwrap_err();
    assert!(!corrupt.is(ErrorCode::Eagain));
    assert!(
        metadata
            .load_inode(backing, 3)
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
    assert!(
        metadata
            .load_inode_snapshot(backing)
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    drop(db);
    drop(reopened);
    drop(metadata);
    drop(other);
    drop(storage);
    drop(second);
    mount_rs_foundationdb::shutdown_client_network().unwrap();
}
