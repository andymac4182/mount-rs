#![cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{BlockStore, MetadataStore, Namespace, NodeData, NodeMetadata};
use mount_rs_core::{ErrorCode, FsDriver, S_IFDIR, Stats};
use mount_rs_foundationdb::{
    FoundationDbBlockAuthorityPolicy, FoundationDbMetadataStore, FoundationDbStorage,
    FoundationDbStorageOptions,
};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};
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

fn assert_snapshot_eq(
    actual: mount_rs_core::storage::InodeMetadataSnapshot,
    expected: &mount_rs_core::storage::InodeMetadataSnapshot,
) {
    assert_eq!(actual.structural_generation, expected.structural_generation);
    assert_eq!(actual.inode_revisions, expected.inode_revisions);
    assert_eq!(
        serde_json::to_value(actual.namespace).unwrap(),
        serde_json::to_value(&expected.namespace).unwrap()
    );
}
fn key(prefix: &str, suffix: &[u8]) -> Vec<u8> {
    let mut key = prefix.as_bytes().to_vec();
    key.push(0);
    key.extend_from_slice(suffix);
    key
}
async fn raw(db: &foundationdb::Database, key: &[u8]) -> Option<Vec<u8>> {
    db.create_trx()
        .unwrap()
        .get(key, false)
        .await
        .unwrap()
        .map(|value| value.to_vec())
}
async fn set(db: &foundationdb::Database, key: &[u8], value: Option<&[u8]>) {
    let trx = db.create_trx().unwrap();
    match value {
        Some(value) => trx.set(key, value),
        None => trx.clear(key),
    }
    trx.commit().await.unwrap();
}
async fn rejects_inode_authority(
    metadata: &FoundationDbMetadataStore,
    backing: mount_rs_core::storage::ConcurrentBackingId,
    snapshot: &mount_rs_core::storage::InodeMetadataSnapshot,
) {
    let root = snapshot.namespace.root;
    let version = mount_rs_core::storage::InodeVersion {
        structural_generation: snapshot.structural_generation,
        inode_revision: snapshot.inode_revisions[&root],
    };
    let errors = [
        metadata.inode_mode_state().await.err(),
        metadata
            .prepare_inode_mode(backing, snapshot.structural_generation)
            .await
            .err(),
        metadata.load_inode_snapshot(backing).await.err(),
        metadata.load_inode(backing, root).await.err(),
        metadata
            .load_inode_if_changed(backing, root, Some(version))
            .await
            .err(),
        metadata
            .publish_inode_if_version(
                backing,
                root,
                version,
                snapshot.namespace.nodes[&root].clone(),
            )
            .await
            .err(),
        metadata
            .publish_structure_if_versions(
                backing,
                snapshot.structural_generation,
                &snapshot.inode_revisions,
                snapshot.namespace.clone(),
            )
            .await
            .err(),
    ];
    assert!(
        errors.iter().all(|error| error
            .as_ref()
            .is_some_and(|error| error.code == ErrorCode::Estale)),
        "authority error codes: {errors:?}"
    );
}

#[tokio::test]
#[ignore = "requires actual FoundationDB and MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE"]
async fn actual_split_inode_authority_preserves_local_and_external_guards() {
    let cluster = std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").unwrap();
    let prefix = format!(
        "mount-rs/split-policy/{}/{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    for external in [false, true] {
        let metadata_prefix = format!("{prefix}/{external}/metadata");
        let block_prefix = if external {
            format!("{prefix}/{external}/blocks")
        } else {
            metadata_prefix.clone()
        };
        let policy = if external {
            FoundationDbBlockAuthorityPolicy::ExternalBlockStore
        } else {
            FoundationDbBlockAuthorityPolicy::SameKeyspace
        };
        let opposite = if external {
            FoundationDbBlockAuthorityPolicy::SameKeyspace
        } else {
            FoundationDbBlockAuthorityPolicy::ExternalBlockStore
        };
        let metadata_storage = FoundationDbStorage::connect(
            &cluster,
            FoundationDbStorageOptions::new(&metadata_prefix).with_block_authority_policy(policy),
        )
        .unwrap();
        let wrong_storage = FoundationDbStorage::connect(
            &cluster,
            FoundationDbStorageOptions::new(&metadata_prefix).with_block_authority_policy(opposite),
        )
        .unwrap();
        let block_storage =
            FoundationDbStorage::connect(&cluster, FoundationDbStorageOptions::new(&block_prefix))
                .unwrap();
        let metadata = metadata_storage.metadata();
        let blocks = block_storage.blocks();
        let db = foundationdb::Database::from_path(&cluster).unwrap();
        let policy_key = key(&metadata_prefix, b"meta/block-policy");
        let marker_key = key(&block_prefix, b"block-authority");
        let backing = blocks.prepare_concurrent_backing().await.unwrap();
        metadata
            .prepare_bound_concurrent_mode(backing)
            .await
            .unwrap();
        let encoded_policy = raw(&db, &policy_key).await.unwrap();
        assert_eq!(encoded_policy, if external { b"MRBP1E" } else { b"MRBP1S" });
        assert_eq!(
            wrong_storage
                .metadata()
                .prepare_bound_concurrent_mode(backing)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(
            wrong_storage
                .metadata()
                .publish_bound_if_revision(backing, 0, empty_namespace())
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        let revision = metadata
            .publish_bound_if_revision(backing, 0, empty_namespace())
            .await
            .unwrap();
        // An absent legacy policy remains local, even with a missing marker.
        if !external {
            set(&db, &policy_key, None).await;
            let marker = raw(&db, &marker_key).await.unwrap();
            set(&db, &marker_key, None).await;
            assert_eq!(
                wrong_storage
                    .metadata()
                    .prepare_inode_mode(backing, revision)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Estale
            );
            assert_eq!(
                metadata
                    .prepare_inode_mode(backing, revision)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Estale
            );
            assert!(raw(&db, &marker_key).await.is_none());
            set(&db, &marker_key, Some(&marker)).await;
        }
        metadata
            .prepare_inode_mode(backing, revision)
            .await
            .unwrap();
        let snapshot = metadata.load_inode_snapshot(backing).await.unwrap();
        rejects_inode_authority(&wrong_storage.metadata(), backing, &snapshot).await;
        // Corrupting/deleting/changing policy cannot bypass unchanged-token reads.
        for tamper in [
            Some(b"unknown-policy".as_slice()),
            Some(if external {
                b"MRBP1S".as_slice()
            } else {
                b"MRBP1E".as_slice()
            }),
            None,
        ] {
            if !external && tamper.is_none() {
                continue;
            } // Old absent policy is explicitly local.
            set(&db, &policy_key, tamper).await;
            rejects_inode_authority(&metadata, backing, &snapshot).await;
            set(&db, &policy_key, Some(&encoded_policy)).await;
            assert_snapshot_eq(
                metadata.load_inode_snapshot(backing).await.unwrap(),
                &snapshot,
            );
        }
        if external {
            assert!(
                raw(&db, &key(&metadata_prefix, b"block-authority"))
                    .await
                    .is_none(),
                "external enrollment must not manufacture a local marker"
            );
        } else {
            let marker = raw(&db, &marker_key).await.unwrap();
            for tamper in [
                None,
                Some(b"malformed".as_slice()),
                Some([9u8; 16].as_slice()),
            ] {
                set(&db, &marker_key, tamper).await;
                rejects_inode_authority(&metadata, backing, &snapshot).await;
                set(&db, &marker_key, Some(&marker)).await;
                assert_snapshot_eq(
                    metadata.load_inode_snapshot(backing).await.unwrap(),
                    &snapshot,
                );
            }
        }
        // The real ChunkedFs must check the actual store before either kind
        // of visible publication, including after it was successfully opened.
        for structural in [false, true] {
            let fs = ChunkedFs::open(
                metadata.clone(),
                blocks.clone(),
                ChunkedOptions::fixed("policy-oracle", 4096)
                    .unwrap()
                    .with_concurrent_writes(true)
                    .with_inode_updates(true),
            )
            .await
            .unwrap();
            fs.write_file("/oracle", b"immutable before marker tamper")
                .await
                .unwrap();
            let handle = fs.open("/oracle", "r", 0o644).await.unwrap();
            let mut bytes = [0u8; 64];
            let length = handle.read(&mut bytes, Some(0)).await.unwrap();
            assert_eq!(&bytes[..length], b"immutable before marker tamper");
            handle.close().await.unwrap();
            let before = metadata.load_inode_snapshot(backing).await.unwrap();
            let marker = raw(&db, &marker_key).await.unwrap();
            set(&db, &marker_key, None).await;
            let error = fs
                .write_file(
                    if structural { "/new-file" } else { "/oracle" },
                    b"must not publish",
                )
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Estale);
            assert!(
                ChunkedFs::open(
                    metadata.clone(),
                    blocks.clone(),
                    ChunkedOptions::fixed("missing-marker", 4096)
                        .unwrap()
                        .with_concurrent_writes(true)
                        .with_inode_updates(true)
                )
                .await
                .is_err()
            );
            assert!(
                raw(&db, &marker_key).await.is_none(),
                "failed reopen must not recreate marker"
            );
            set(&db, &marker_key, Some(&marker)).await;
            assert_snapshot_eq(
                metadata.load_inode_snapshot(backing).await.unwrap(),
                &before,
            );
            assert!(fs.write_file("/still-fenced", b"no").await.is_err());
            let _ = fs.shutdown().await;
        }
        let transaction = db.create_trx().unwrap();
        let owned = format!("{prefix}/{external}/").into_bytes();
        let mut end = owned.clone();
        end.push(255);
        transaction.clear_range(&owned, &end);
        transaction.commit().await.unwrap();
        assert!(raw(&db, &policy_key).await.is_none());
    }
    mount_rs_foundationdb::shutdown_client_network().unwrap();
}
