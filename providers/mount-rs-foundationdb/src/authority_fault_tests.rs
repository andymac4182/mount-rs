//! Actual transaction fault boundaries. All hooks are compiled only for unit tests.
use super::*;
use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{DirectoryEntry, FileLayout};
use mount_rs_core::{S_IFDIR, S_IFREG, Stats};

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

fn arm(storage: &FoundationDbStorage, fault: MetadataTestFault) {
    storage
        .inner
        .metadata_test_attempts
        .store(0, Ordering::SeqCst);
    *storage.inner.metadata_test_fault.lock().unwrap() = Some(fault);
}
fn attempts(storage: &FoundationDbStorage, count: u64) {
    assert_eq!(
        storage.inner.metadata_test_attempts.load(Ordering::SeqCst),
        count
    );
}
async fn assert_snapshot(
    metadata: &FoundationDbMetadataStore,
    backing: ConcurrentBackingId,
    before: &InodeMetadataSnapshot,
) {
    let after = metadata.load_inode_snapshot(backing).await.unwrap();
    assert_eq!(after.structural_generation, before.structural_generation);
    assert_eq!(after.inode_revisions, before.inode_revisions);
    assert_eq!(
        serde_json::to_value(after.namespace).unwrap(),
        serde_json::to_value(&before.namespace).unwrap()
    );
}

#[tokio::test]
#[ignore = "requires actual FoundationDB and MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE"]
async fn native_policy_marker_conflicts_and_committed_ack_loss_preserve_authority() {
    let cluster = std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").unwrap();
    let prefix = format!(
        "mount-rs/policy-faults/{}/{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    for policy in [
        FoundationDbBlockAuthorityPolicy::SameKeyspace,
        FoundationDbBlockAuthorityPolicy::ExternalBlockStore,
    ] {
        let metadata_prefix = format!("{prefix}/{policy:?}/metadata");
        let block_prefix = if policy == FoundationDbBlockAuthorityPolicy::SameKeyspace {
            metadata_prefix.clone()
        } else {
            format!("{prefix}/{policy:?}/blocks")
        };
        let storage = FoundationDbStorage::connect(
            &cluster,
            FoundationDbStorageOptions::new(&metadata_prefix).with_block_authority_policy(policy),
        )
        .unwrap();
        let block_storage =
            FoundationDbStorage::connect(&cluster, FoundationDbStorageOptions::new(&block_prefix))
                .unwrap();
        let metadata = storage.metadata();
        let backing = block_storage
            .blocks()
            .prepare_concurrent_backing()
            .await
            .unwrap();
        let keys = Keyspace::new(metadata_prefix.as_bytes());
        arm(&storage, MetadataTestFault::AfterCommitAckLoss);
        assert_eq!(
            metadata
                .prepare_bound_concurrent_mode(backing)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        attempts(&storage, 1);
        assert_eq!(
            metadata.concurrent_mode_state().await.unwrap(),
            ConcurrentModeState::Mrc2(backing)
        );
        let trx = storage.inner.db.create_trx().unwrap();
        assert_eq!(
            trx.get(&keys.block_authority_policy(), false)
                .await
                .unwrap()
                .unwrap()
                .as_ref(),
            policy.encode()
        );
        drop(trx);
        let mut namespace = empty_namespace();
        let mut file = namespace.nodes[&1].clone();
        file.stats.ino = 2;
        file.stats.mode = S_IFREG | 0o644;
        file.stats.nlink = 1;
        file.stats.size = 0;
        file.stats.blocks = 0;
        file.data = NodeData::File(FileLayout {
            chunker: namespace.default_chunker.clone(),
            extents: vec![],
        });
        namespace.nodes.insert(2, file);
        namespace.next_inode = 3;
        namespace.nodes.get_mut(&1).unwrap().data = NodeData::Directory {
            entries: vec![DirectoryEntry {
                name: "oracle".into(),
                inode: 2,
            }],
        };
        let revision = metadata
            .publish_bound_if_revision(backing, 0, namespace)
            .await
            .unwrap();
        arm(&storage, MetadataTestFault::AfterCommitAckLoss);
        assert_eq!(
            metadata
                .prepare_inode_mode(backing, revision)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        attempts(&storage, 1);
        let before = metadata.load_inode_snapshot(backing).await.unwrap();
        let root = 2;
        let version = InodeVersion {
            structural_generation: before.structural_generation,
            inode_revision: before.inode_revisions[&root],
        };
        arm(&storage, MetadataTestFault::AfterCommitAckLoss);
        assert_eq!(
            metadata
                .publish_inode_if_version(
                    backing,
                    root,
                    version,
                    before.namespace.nodes[&root].clone()
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        attempts(&storage, 1);
        let selected = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(selected.structural_generation, before.structural_generation);
        assert_eq!(selected.inode_revisions[&root], version.inode_revision + 1);
        arm(&storage, MetadataTestFault::AfterCommitAckLoss);
        assert_eq!(
            metadata
                .publish_structure_if_versions(
                    backing,
                    selected.structural_generation,
                    &selected.inode_revisions,
                    selected.namespace.clone()
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        attempts(&storage, 1);
        let before = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(
            before.structural_generation,
            selected.structural_generation + 1
        );
        let version = InodeVersion {
            structural_generation: before.structural_generation,
            inode_revision: before.inode_revisions[&root],
        };
        let opposite = if policy == FoundationDbBlockAuthorityPolicy::SameKeyspace {
            FoundationDbBlockAuthorityPolicy::ExternalBlockStore
        } else {
            FoundationDbBlockAuthorityPolicy::SameKeyspace
        };
        // Change the actual authority after the operation has read and staged
        // its mutation, before FDB commits it. Conflict retry must revalidate.
        let mut changes = vec![(
            keys.block_authority_policy(),
            opposite.encode().to_vec(),
            policy.encode().to_vec(),
        )];
        if policy == FoundationDbBlockAuthorityPolicy::SameKeyspace {
            changes.push((
                keys.block_authority(),
                vec![9; 16],
                backing.as_bytes().to_vec(),
            ));
        }
        for (key, changed, original) in changes {
            arm(
                &storage,
                MetadataTestFault::BeforeCommitChange {
                    key: key.clone(),
                    value: changed,
                },
            );
            assert_eq!(
                metadata
                    .publish_inode_if_version(
                        backing,
                        root,
                        version,
                        before.namespace.nodes[&root].clone()
                    )
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Estale
            );
            attempts(&storage, 2);
            let trx = storage.inner.db.create_trx().unwrap();
            trx.set(&key, &original);
            trx.commit().await.unwrap();
            assert_snapshot(&metadata, backing, &before).await;
        }
    }
    let db = Database::from_path(&cluster).unwrap();
    let trx = db.create_trx().unwrap();
    trx.clear_range(prefix.as_bytes(), &range_end(prefix.as_bytes()).unwrap());
    trx.commit().await.unwrap();
    drop(db);
    shutdown_client_network().unwrap();
}
