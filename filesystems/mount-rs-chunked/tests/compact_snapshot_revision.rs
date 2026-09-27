//! Public regression for coherent compact refreshes during whole-file creates.
//! The process-global recorder requires an isolated, serial, opt-in invocation.

#![cfg(unix)]

use futures_lite::future::block_on;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::FsDriver;
use mount_rs_core::diagnostics::profile::{self, Snapshot};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::path::PathBuf;

struct PrivateVolume(PathBuf);

impl PrivateVolume {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "mount-rs-compact-revision-{}-{nonce}",
            std::process::id(),
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory).unwrap();
        Self(directory)
    }
}

impl Drop for PrivateVolume {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn calls(snapshot: &Snapshot, name: &str) -> u64 {
    snapshot
        .entries
        .iter()
        .find(|entry| entry.name == name)
        .unwrap()
        .calls
}

fn guard_delta(before: &Snapshot, after: &Snapshot, name: &str) -> (u64, u64) {
    let find = |snapshot: &Snapshot| {
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("missing fresh-create guard row {name}"));
        (entry.calls, entry.units)
    };
    let old = find(before);
    let new = find(after);
    (
        new.0.checked_sub(old.0).expect("monotonic calls"),
        new.1.checked_sub(old.1).expect("monotonic units"),
    )
}

async fn read_created(fs: &ChunkedFs<SqliteMetadataStore, SqliteBlockStore>) -> Vec<u8> {
    let reader = fs.open("/created", "r", 0).await.unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 32];
    loop {
        let count = reader
            .read(&mut buffer, Some(bytes.len() as u64))
            .await
            .unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() <= b"complete acknowledged bytes".len());
    }
    reader.close().await.unwrap();
    bytes
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn unchanged_compact_refresh_preserves_prepared_create() {
    assert!(profile::enabled(), "run with MOUNT_RS_PROFILE_IO=1");
    let volume = PrivateVolume::new();
    let metadata_path = volume.0.join("metadata.sqlite");
    let blocks_path = volume.0.join("blocks.sqlite");
    let (conflicts, replays, committed, replies, before, after) = block_on(async {
        let fs = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("compact-revision-create", 16)
                .unwrap()
                .with_compact_inode_updates(true),
        )
        .await
        .unwrap();
        let before = profile::snapshot();
        fs.write_file("/created", b"complete acknowledged bytes")
            .await
            .unwrap();
        let after = profile::snapshot();
        let conflicts = calls(&after, "filesystem.mutation.request.conflict")
            - calls(&before, "filesystem.mutation.request.conflict");
        let replays = calls(&after, "filesystem.gate_hold.whole_file_replay")
            - calls(&before, "filesystem.gate_hold.whole_file_replay");
        let committed = calls(&after, "filesystem.mutation.request.committed")
            - calls(&before, "filesystem.mutation.request.committed");
        let replies = calls(&after, "filesystem.mutation.request.reply_sent")
            - calls(&before, "filesystem.mutation.request.reply_sent");
        assert_eq!(read_created(&fs).await, b"complete acknowledged bytes",);
        fs.shutdown().await.unwrap();
        drop(fs);

        let reopened = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            ChunkedOptions::fixed("compact-revision-reopen", 16)
                .unwrap()
                .with_compact_inode_updates(true),
        )
        .await
        .unwrap();
        assert_eq!(
            read_created(&reopened).await,
            b"complete acknowledged bytes",
        );
        reopened.shutdown().await.unwrap();
        (conflicts, replays, committed, replies, before, after)
    });
    assert_eq!(
        conflicts, 0,
        "a coherent refresh without external writers must preserve a prepared create",
    );
    assert_eq!(
        replays, 0,
        "an unchanged snapshot must not force whole-file replay",
    );
    assert_eq!(committed, 1, "the prepared request must commit directly");
    assert_eq!(replies, 1, "the committed request must deliver its reply");
    assert_eq!(
        guard_delta(
            &before,
            &after,
            "filesystem.mutation.create_guard.evaluated"
        ),
        (1, 1)
    );
    assert_eq!(
        guard_delta(&before, &after, "filesystem.mutation.create_guard.passed"),
        (1, 1)
    );
    assert_eq!(
        guard_delta(&before, &after, "filesystem.mutation.create_guard.conflict"),
        (0, 0)
    );
    assert_eq!(
        guard_delta(
            &before,
            &after,
            "filesystem.mutation.create_guard.revision_mismatch"
        ),
        (0, 0)
    );
    assert_eq!(
        guard_delta(
            &before,
            &after,
            "filesystem.mutation.create_guard.allocation_mismatch"
        ),
        (0, 0)
    );
    assert_eq!(
        guard_delta(
            &before,
            &after,
            "filesystem.mutation.create_guard.path_present"
        ),
        (0, 0)
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn peer_selected_inode_update_invalidates_prepared_create() {
    use async_trait::async_trait;
    use mount_rs_core::storage::{
        BlockId, BlockReconcileReport, BlockStore, ConcurrentBackingId, MetadataStore,
    };
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[derive(Clone)]
    struct PausedBlocks {
        inner: SqliteBlockStore,
        pause: Arc<AtomicBool>,
        entered: Arc<tokio::sync::Notify>,
        resume: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl BlockStore for PausedBlocks {
        fn durable(&self) -> bool {
            self.inner.durable()
        }

        async fn prepare_concurrent_backing(&self) -> mount_rs_core::Result<ConcurrentBackingId> {
            self.inner.prepare_concurrent_backing().await
        }

        async fn verify_concurrent_backing(
            &self,
            expected: ConcurrentBackingId,
        ) -> mount_rs_core::Result<()> {
            self.inner.verify_concurrent_backing(expected).await
        }

        async fn get_for_migration(&self, id: &BlockId) -> mount_rs_core::Result<Vec<u8>> {
            self.inner.get_for_migration(id).await
        }

        async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
            if self.pause.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.resume.notified().await;
            }
            self.inner.put(bytes).await
        }

        async fn get(&self, id: &BlockId) -> mount_rs_core::Result<Vec<u8>> {
            self.inner.get(id).await
        }

        async fn flush(&self) -> mount_rs_core::Result<()> {
            self.inner.flush().await
        }

        async fn delete(&self, id: &BlockId) -> mount_rs_core::Result<()> {
            self.inner.delete(id).await
        }

        async fn reconcile(
            &self,
            live: &BTreeSet<BlockId>,
            grace: Duration,
        ) -> mount_rs_core::Result<BlockReconcileReport> {
            self.inner.reconcile(live, grace).await
        }
    }

    async fn assert_file_bytes<B: BlockStore + 'static>(
        fs: &ChunkedFs<SqliteMetadataStore, B>,
        path: &str,
        expected: &[u8],
    ) {
        let reader = fs.open(path, "r", 0).await.unwrap();
        assert_eq!(reader.stat().await.unwrap().size, expected.len() as u64);
        let mut actual = vec![0; expected.len() + 1];
        assert_eq!(
            reader.read(&mut actual, Some(0)).await.unwrap(),
            expected.len(),
        );
        assert_eq!(&actual[..expected.len()], expected);
        assert_eq!(
            reader
                .read(&mut actual, Some(expected.len() as u64))
                .await
                .unwrap(),
            0,
        );
        reader.close().await.unwrap();
        drop(reader);
    }

    assert!(profile::enabled(), "run with MOUNT_RS_PROFILE_IO=1");
    let volume = PrivateVolume::new();
    let metadata_path = volume.0.join("metadata.sqlite");
    let blocks_path = volume.0.join("blocks.sqlite");
    block_on(futures_lite::future::race(
        async {
            let blocks = PausedBlocks {
                inner: SqliteBlockStore::open(&blocks_path).unwrap(),
                pause: Arc::new(AtomicBool::new(false)),
                entered: Arc::new(tokio::sync::Notify::new()),
                resume: Arc::new(tokio::sync::Notify::new()),
            };
            let creator = ChunkedFs::open(
                SqliteMetadataStore::open(&metadata_path).unwrap(),
                blocks.clone(),
                ChunkedOptions::fixed("compact-stale-creator", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            creator.write_file("/seed", b"SEED0000").await.unwrap();
            let peer = ChunkedFs::open(
                SqliteMetadataStore::open(&metadata_path).unwrap(),
                SqliteBlockStore::open(&blocks_path).unwrap(),
                ChunkedOptions::fixed("compact-stale-peer", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            let peer_handle = peer.open("/seed", "r+", 0).await.unwrap();
            let seed_inode = peer_handle.stat().await.unwrap().ino;
            let observer = SqliteMetadataStore::open(&metadata_path).unwrap();
            let backing = observer
                .compact_inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .backing;

            blocks.pause.store(true, Ordering::SeqCst);
            let before = profile::snapshot();
            let create = async {
                creator
                    .write_file("/created", b"complete acknowledged bytes")
                    .await
                    .unwrap();
            };
            let change = async {
                blocks.entered.notified().await;
                // The creator has captured the old Full namespace and released
                // its local gate before immutable block I/O. Commit a real
                // selected update through another SQLite filesystem owner.
                let old = observer.load_compact_snapshot(backing).await.unwrap();
                assert_eq!(peer_handle.write(b"PEER", Some(0)).await.unwrap(), 4);
                let new = observer.load_compact_snapshot(backing).await.unwrap();
                assert_eq!(new.anchor.generation, old.anchor.generation);
                assert_eq!(new.anchor, old.anchor);
                assert_ne!(
                    new.guards[&seed_inode].identity, old.guards[&seed_inode].identity,
                    "the peer must commit a changed physical inode guard",
                );
                assert_ne!(
                    new.guards[&seed_inode].node, old.guards[&seed_inode].node,
                    "the persisted peer update must change the captured body",
                );
                blocks.resume.notify_one();
            };
            futures_lite::future::zip(create, change).await;
            let after = profile::snapshot();
            let observed = |name| calls(&after, name) - calls(&before, name);
            assert_eq!(observed("filesystem.mutation.request.conflict"), 1);
            assert_eq!(observed("filesystem.gate_hold.whole_file_replay"), 1);
            assert_eq!(observed("filesystem.gate_wait.whole_file_replay"), 1);
            assert_eq!(observed("filesystem.mutation.request.committed"), 0);
            assert_eq!(observed("filesystem.mutation.request.reply_sent"), 1);
            assert_eq!(observed("filesystem.mutation.attempt_requests"), 1);
            assert_eq!(observed("filesystem.mutation.attempt.no_publication"), 1);
            for name in [
                "filesystem.mutation.attempt.success",
                "filesystem.mutation.attempt.conflict",
                "filesystem.mutation.attempt.error",
                "filesystem.mutation.attempt.cancelled",
                "filesystem.mutation.request.error",
                "filesystem.mutation.request.cancelled",
                "filesystem.mutation.request.receiver_closed",
            ] {
                assert_eq!(observed(name), 0, "unexpected terminal outcome {name}");
            }

            assert_file_bytes(&creator, "/seed", b"PEER0000").await;
            assert_file_bytes(&creator, "/created", b"complete acknowledged bytes").await;
            peer_handle.close().await.unwrap();
            drop(peer_handle);
            peer.shutdown().await.unwrap();
            creator.shutdown().await.unwrap();
            drop(peer);
            drop(creator);
            drop(observer);
            drop(blocks);

            let reopened = ChunkedFs::open(
                SqliteMetadataStore::open(&metadata_path).unwrap(),
                SqliteBlockStore::open(&blocks_path).unwrap(),
                ChunkedOptions::fixed("compact-stale-reopen", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            assert_file_bytes(&reopened, "/seed", b"PEER0000").await;
            assert_file_bytes(&reopened, "/created", b"complete acknowledged bytes").await;
            reopened.shutdown().await.unwrap();
            assert_eq!(
                guard_delta(
                    &before,
                    &after,
                    "filesystem.mutation.create_guard.evaluated"
                ),
                (1, 1)
            );
            assert_eq!(
                guard_delta(&before, &after, "filesystem.mutation.create_guard.passed"),
                (0, 0)
            );
            assert_eq!(
                guard_delta(&before, &after, "filesystem.mutation.create_guard.conflict"),
                (1, 1)
            );
            assert_eq!(
                guard_delta(
                    &before,
                    &after,
                    "filesystem.mutation.create_guard.revision_mismatch"
                ),
                (1, 1)
            );
            assert_eq!(
                guard_delta(
                    &before,
                    &after,
                    "filesystem.mutation.create_guard.allocation_mismatch"
                ),
                (0, 0)
            );
            assert_eq!(
                guard_delta(
                    &before,
                    &after,
                    "filesystem.mutation.create_guard.path_present"
                ),
                (0, 0)
            );
        },
        async {
            async_io::Timer::after(Duration::from_secs(10)).await;
            panic!("peer selected-inode capture control timed out");
        },
    ));
}
