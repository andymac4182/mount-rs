use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::driver::FsDriver;
use mount_rs_core::storage::{
    BlockExtent, BlockId, BlockStore, DirectoryEntry, FileLayout, Namespace, NodeData, NodeMetadata,
};
use mount_rs_core::types::{S_IFDIR, S_IFREG, Stats};
use mount_rs_core::versioning::BlockStoreId;
use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_versioned::{VersionedCoordinator, VersionedOptions};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

const EXACT_BYTES: &[u8] = b"\x00mount-rs-pglite-versioned\xff\x01exact-bytes\x00";

fn namespace(block: BlockId) -> Namespace {
    let chunker = FixedSizeChunker::new(4096).unwrap().config();
    let size = EXACT_BYTES.len() as u64;
    let root_stats = Stats {
        dev: 1,
        ino: 1,
        mode: S_IFDIR | 0o755,
        nlink: 2,
        uid: 1000,
        gid: 1000,
        rdev: 0,
        size: 0,
        blksize: 4096,
        blocks: 0,
        atime_ms: 1,
        mtime_ms: 1,
        ctime_ms: 1,
        birthtime_ms: 1,
    };
    Namespace {
        format_version: 1,
        root: 1,
        next_inode: 3,
        default_uid: 1000,
        default_gid: 1000,
        umask: 0o022,
        default_chunker: chunker.clone(),
        nodes: BTreeMap::from([
            (
                1,
                NodeMetadata {
                    stats: root_stats.clone(),
                    data: NodeData::Directory {
                        entries: vec![DirectoryEntry {
                            name: "exact".to_owned(),
                            inode: 2,
                        }],
                    },
                },
            ),
            (
                2,
                NodeMetadata {
                    stats: Stats {
                        dev: 1,
                        ino: 2,
                        mode: S_IFREG | 0o644,
                        nlink: 1,
                        uid: 1000,
                        gid: 1000,
                        rdev: 0,
                        size,
                        blksize: 4096,
                        blocks: size.div_ceil(512),
                        atime_ms: 1,
                        mtime_ms: 1,
                        ctime_ms: 1,
                        birthtime_ms: 1,
                    },
                    data: NodeData::File(FileLayout {
                        chunker,
                        extents: vec![BlockExtent {
                            file_offset: 0,
                            block,
                            block_offset: 0,
                            length: size,
                        }],
                    }),
                },
            ),
        ]),
    }
}

async fn read_exact_file<D>(driver: &D)
where
    D: FsDriver,
{
    let handle = driver.open("/exact", "r", 0).await.unwrap();
    let mut bytes = vec![0; EXACT_BYTES.len()];
    let count = handle.read(&mut bytes, Some(0)).await.unwrap();
    assert_eq!(count, EXACT_BYTES.len());
    assert_eq!(bytes, EXACT_BYTES);
    let mut eof = [0; 1];
    assert_eq!(handle.read(&mut eof, Some(count as u64)).await.unwrap(), 0);
    handle.close().await.unwrap();
}

fn unique_volume_key() -> String {
    format!(
        "w04-versioned-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos()
    )
}

#[test]
#[ignore = "requires PGLITE_DATABASE_URL and an isolated real PGlite server"]
fn pglite_versioning_snapshot_history_view_survives_reconnect() {
    let url = std::env::var("PGLITE_DATABASE_URL").expect("PGLITE_DATABASE_URL required");
    let volume_key = unique_volume_key();
    let provider_options = PgliteStorageOptions::new(volume_key.clone());
    let versioned_options = VersionedOptions::new(
        "w04-versioned-test",
        BlockStoreId::new("pglite-versioned-test").unwrap(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build Tokio test runtime");

    runtime.block_on(async move {
        let metadata = PgliteMetadataStore::connect_with_options(&url, provider_options.clone())
            .await
            .unwrap();
        let blocks = PgliteBlockStore::connect_with_options(&url, provider_options.clone())
            .await
            .unwrap();
        let block = blocks.put(EXACT_BYTES).await.unwrap();
        blocks.flush().await.unwrap();

        let coordinator =
            VersionedCoordinator::new(metadata.clone(), blocks.clone(), versioned_options.clone())
                .unwrap();
        let first = coordinator
            .publish_namespace(0, None, namespace(block.clone()))
            .await
            .unwrap();
        let snapshot = coordinator.snapshot().await.unwrap();
        assert_eq!(snapshot.parent, Some(first.id.clone()));
        assert_eq!(
            coordinator.head().await.unwrap().unwrap().version,
            snapshot.id
        );

        let history = coordinator.history().await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].id, first.id);
        assert_eq!(history[1].id, snapshot.id);

        let view = coordinator.view(&first.id).await.unwrap();
        read_exact_file(&view).await;
        view.close().await.unwrap();
        drop(view);
        drop(coordinator);

        metadata.close().await.unwrap();
        blocks.close().await.unwrap();
        drop(metadata);
        drop(blocks);

        let reopened_metadata =
            PgliteMetadataStore::connect_with_options(&url, provider_options.clone())
                .await
                .unwrap();
        let reopened_blocks = PgliteBlockStore::connect_with_options(&url, provider_options)
            .await
            .unwrap();
        assert_eq!(reopened_blocks.get(&block).await.unwrap(), EXACT_BYTES);

        let reopened = VersionedCoordinator::new(
            reopened_metadata.clone(),
            reopened_blocks.clone(),
            versioned_options,
        )
        .unwrap();
        let reopened_history = reopened.history().await.unwrap();
        assert_eq!(reopened_history.len(), 2);
        assert_eq!(reopened_history[0].id, first.id);
        assert_eq!(reopened_history[1].id, snapshot.id);
        let reopened_view = reopened.view(&first.id).await.unwrap();
        read_exact_file(&reopened_view).await;
        reopened_view.close().await.unwrap();
        drop(reopened_view);
        drop(reopened);

        reopened_metadata.close().await.unwrap();
        reopened_blocks.close().await.unwrap();
    });
}
