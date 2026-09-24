#![cfg(feature = "foundationdb")]
use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{BlockStore, MetadataStore, Namespace, NodeData, NodeMetadata};
use mount_rs_core::{ErrorCode, S_IFDIR, Stats};
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
async fn delegated_directory_authority_fences_old_and_stale_clients() {
    let cluster_file = std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE")
        .expect("set MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let prefix = format!("mount-rs/delegation/{}/{unique}", std::process::id());
    let storage =
        FoundationDbStorage::connect(&cluster_file, FoundationDbStorageOptions::new(&prefix))
            .unwrap();
    let second =
        FoundationDbStorage::connect(&cluster_file, FoundationDbStorageOptions::new(&prefix))
            .unwrap();
    let metadata = storage.metadata();
    let other = second.metadata();
    let backing = storage.blocks().prepare_concurrent_backing().await.unwrap();
    let mut namespace = empty_namespace();

    use mount_rs_core::delegation::{
        CheckoutRequest, DelegatedCheckin, DelegatedPublish, DelegatedRecovery,
    };
    use mount_rs_core::storage::DirectoryEntry;
    for ino in [2, 3] {
        let mut node = namespace.nodes[&namespace.root].clone();
        node.stats.ino = ino;
        node.stats.nlink = 2;
        namespace.nodes.insert(ino, node);
    }
    namespace
        .nodes
        .get_mut(&namespace.root)
        .unwrap()
        .stats
        .nlink = 4;
    namespace.nodes.get_mut(&namespace.root).unwrap().data = NodeData::Directory {
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
    namespace.next_inode = 4;
    metadata
        .prepare_bound_concurrent_mode(backing)
        .await
        .unwrap();
    let revision = metadata
        .publish_bound_if_revision(backing, 0, namespace.clone())
        .await
        .unwrap();
    metadata
        .prepare_delegated_mode(backing, revision)
        .await
        .unwrap();
    metadata
        .prepare_delegated_mode(backing, revision)
        .await
        .unwrap();
    assert!(
        metadata
            .publish_bound_if_revision(backing, revision, namespace.clone())
            .await
            .is_err()
    );
    assert!(
        metadata
            .acquire_writer("old-writer", Duration::from_secs(10))
            .await
            .is_err()
    );
    let request_a = CheckoutRequest {
        backing,
        root: 2,
        owner: "a".into(),
    };
    let request_b = CheckoutRequest {
        backing,
        root: 3,
        owner: "b".into(),
    };
    let (a, b) = tokio::join!(metadata.checkout(&request_a), other.checkout(&request_b));
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.token.fence, b.token.fence);
    assert_eq!(
        metadata
            .checkout(&CheckoutRequest {
                backing,
                root: 2,
                owner: "a".into()
            })
            .await
            .unwrap(),
        a
    );
    assert!(
        other
            .checkout(&CheckoutRequest {
                backing,
                root: namespace.root,
                owner: "overlap".into()
            })
            .await
            .is_err()
    );
    let request = DelegatedPublish {
        backing,
        token: a.token.clone(),
        expected_revision: revision,
    };
    let mut hostile = namespace.clone();
    hostile.nodes.get_mut(&3).unwrap().stats.ctime_ms += 1;
    assert!(metadata.publish_delegated(&request, hostile).await.is_err());
    let mut header = namespace.clone();
    header.umask = 0o777;
    assert!(metadata.publish_delegated(&request, header).await.is_err());
    assert_eq!(other.load().await.unwrap().revision, revision);
    namespace.nodes.get_mut(&2).unwrap().stats.ctime_ms += 1;
    let updated = metadata
        .publish_delegated(&request, namespace.clone())
        .await
        .unwrap();
    assert!(
        metadata
            .publish_delegated(&request, namespace.clone())
            .await
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    let release = DelegatedCheckin {
        backing,
        token: a.token.clone(),
        expected_revision: updated,
    };
    metadata.checkin(&release).await.unwrap();
    other.checkin(&release).await.unwrap();
    let new = other
        .checkout(&CheckoutRequest {
            backing,
            root: 2,
            owner: "new".into(),
        })
        .await
        .unwrap();
    assert!(new.token.fence > a.token.fence);
    assert!(
        metadata
            .publish_delegated(
                &DelegatedPublish {
                    expected_revision: updated,
                    ..request
                },
                namespace
            )
            .await
            .is_err()
    );
    assert!(
        metadata
            .recover(&DelegatedRecovery {
                backing,
                root: 2,
                expected_fence: a.token.fence
            })
            .await
            .is_err()
    );
    other
        .recover(&DelegatedRecovery {
            backing,
            root: 2,
            expected_fence: new.token.fence,
        })
        .await
        .unwrap();
    other
        .recover(&DelegatedRecovery {
            backing,
            root: 2,
            expected_fence: new.token.fence,
        })
        .await
        .unwrap();
    assert!(
        !metadata
            .delegation_state()
            .await
            .unwrap()
            .unwrap()
            .grants
            .contains_key(&2)
    );
    drop(metadata);
    drop(other);
    drop(storage);
    drop(second);
    mount_rs_foundationdb::shutdown_client_network().unwrap();
}
