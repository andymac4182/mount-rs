//! Public current-path controls for a prepared fresh create racing a SQLite peer.
//! Run serially with MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0.

#![cfg(unix)]

use async_trait::async_trait;
use futures_lite::future::block_on;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::diagnostics::profile::{self, Snapshot};
use mount_rs_core::storage::{BlockId, BlockReconcileReport, BlockStore, ConcurrentBackingId};
use mount_rs_core::{ErrorCode, FsDriver, MkdirOptions};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

static VOLUME_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const CREATED_BYTES: &[u8] = b"complete acknowledged bytes across several blocks";
const PEER_BYTES: &[u8] = b"peer bytes across several blocks";

struct PrivateVolume(PathBuf);

impl PrivateVolume {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = VOLUME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "mount-rs-create-rebase-{}-{nonce}-{sequence}",
            std::process::id(),
        ));
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700).create(&directory).unwrap();
        Self(directory)
    }
}

impl Drop for PrivateVolume {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

fn calls(snapshot: &Snapshot, name: &str) -> u64 {
    snapshot
        .entries
        .iter()
        .find(|entry| entry.name == name)
        .unwrap()
        .calls
}

fn delta(before: &Snapshot, after: &Snapshot, name: &str) -> u64 {
    calls(after, name).checked_sub(calls(before, name)).unwrap()
}

async fn assert_bytes<B: BlockStore + 'static>(
    fs: &ChunkedFs<SqliteMetadataStore, B>,
    path: &str,
    expected: &[u8],
) -> u64 {
    let reader = fs.open(path, "r", 0).await.unwrap();
    let stat = reader.stat().await.unwrap();
    assert_eq!(stat.size, expected.len() as u64, "size of {path}");
    let mut actual = Vec::new();
    let mut buffer = [0; 13];
    loop {
        let count = reader
            .read(&mut buffer, Some(actual.len() as u64))
            .await
            .unwrap();
        if count == 0 {
            break;
        }
        actual.extend_from_slice(&buffer[..count]);
        assert!(actual.len() <= expected.len(), "overlong read of {path}");
    }
    assert_eq!(actual, expected, "bytes of {path}");
    assert_eq!(
        reader
            .read(&mut buffer, Some(expected.len() as u64))
            .await
            .unwrap(),
        0,
        "EOF of {path}"
    );
    reader.close().await.unwrap();
    stat.ino
}

#[derive(Clone, Copy)]
enum Case {
    PeerAllocation,
    RetargetedSymlink,
    OccupiedPath,
    RemovedParent,
}

fn run(case: Case) {
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
            let options = |name| {
                ChunkedOptions::fixed(name, 16)
                    .unwrap()
                    .with_compact_inode_updates(true)
            };
            let creator = ChunkedFs::open(
                SqliteMetadataStore::open(&metadata_path).unwrap(),
                blocks.clone(),
                options("rebase-creator"),
            )
            .await
            .unwrap();
            match case {
                Case::RetargetedSymlink => {
                    creator
                        .mkdir("/old", MkdirOptions::default())
                        .await
                        .unwrap();
                    creator
                        .mkdir("/new", MkdirOptions::default())
                        .await
                        .unwrap();
                    creator.symlink("/old", "/alias").await.unwrap();
                }
                Case::RemovedParent => {
                    creator
                        .mkdir("/parent", MkdirOptions::default())
                        .await
                        .unwrap();
                }
                _ => {}
            }
            let peer = ChunkedFs::open(
                SqliteMetadataStore::open(&metadata_path).unwrap(),
                SqliteBlockStore::open(&blocks_path).unwrap(),
                options("rebase-peer"),
            )
            .await
            .unwrap();
            let path = match case {
                Case::RetargetedSymlink => "/alias/created",
                Case::RemovedParent => "/parent/created",
                _ => "/created",
            };
            blocks.pause.store(true, Ordering::SeqCst);
            let create = creator.write_file(path, CREATED_BYTES);
            let change = async {
                blocks.entered.notified().await;
                // The first immutable PUT follows the creator's Full namespace capture.
                // Finish the peer mutation before collecting creator-only counters.
                match case {
                    Case::PeerAllocation => peer.write_file("/peer", PEER_BYTES).await.unwrap(),
                    Case::RetargetedSymlink => {
                        peer.unlink("/alias").await.unwrap();
                        peer.symlink("/new", "/alias").await.unwrap();
                    }
                    Case::OccupiedPath => peer.write_file("/created", PEER_BYTES).await.unwrap(),
                    Case::RemovedParent => peer.rmdir("/parent").await.unwrap(),
                }
                let before = profile::snapshot();
                blocks.resume.notify_one();
                before
            };
            let (result, before) = futures_lite::future::zip(create, change).await;
            let after = profile::snapshot();
            match case {
                Case::RemovedParent => assert_eq!(result.unwrap_err().code, ErrorCode::Enoent),
                _ => result.unwrap(),
            }
            match case {
                Case::PeerAllocation => {
                    let created_inode = assert_bytes(&creator, "/created", CREATED_BYTES).await;
                    let peer_inode = assert_bytes(&creator, "/peer", PEER_BYTES).await;
                    assert_ne!(created_inode, peer_inode);
                }
                Case::RetargetedSymlink => {
                    assert_bytes(&creator, "/new/created", CREATED_BYTES).await;
                    assert_eq!(
                        creator.stat("/old/created").await.unwrap_err().code,
                        ErrorCode::Enoent
                    );
                }
                Case::OccupiedPath => {
                    assert_bytes(&creator, "/created", CREATED_BYTES).await;
                }
                Case::RemovedParent => {
                    assert_eq!(
                        creator.stat("/parent").await.unwrap_err().code,
                        ErrorCode::Enoent
                    );
                    assert_eq!(
                        creator.stat("/parent/created").await.unwrap_err().code,
                        ErrorCode::Enoent
                    );
                }
            }
            peer.shutdown().await.unwrap();
            creator.shutdown().await.unwrap();
            drop(peer);
            drop(creator);
            drop(blocks);
            let reopened = ChunkedFs::open(
                SqliteMetadataStore::open(&metadata_path).unwrap(),
                SqliteBlockStore::open(&blocks_path).unwrap(),
                options("rebase-reopened"),
            )
            .await
            .unwrap();
            match case {
                Case::PeerAllocation => {
                    let created_inode = assert_bytes(&reopened, "/created", CREATED_BYTES).await;
                    let peer_inode = assert_bytes(&reopened, "/peer", PEER_BYTES).await;
                    assert_ne!(created_inode, peer_inode);
                }
                Case::RetargetedSymlink => {
                    assert_bytes(&reopened, "/new/created", CREATED_BYTES).await;
                    assert_bytes(&reopened, "/alias/created", CREATED_BYTES).await;
                    assert_eq!(
                        reopened.stat("/old/created").await.unwrap_err().code,
                        ErrorCode::Enoent
                    );
                }
                Case::OccupiedPath => {
                    assert_bytes(&reopened, "/created", CREATED_BYTES).await;
                }
                Case::RemovedParent => {
                    assert_eq!(
                        reopened.stat("/parent").await.unwrap_err().code,
                        ErrorCode::Enoent
                    );
                    assert_eq!(
                        reopened.stat("/parent/created").await.unwrap_err().code,
                        ErrorCode::Enoent
                    );
                }
            }
            reopened.shutdown().await.unwrap();
            match case {
                Case::PeerAllocation | Case::RetargetedSymlink => {
                    assert_eq!(
                        delta(&before, &after, "filesystem.mutation.request.conflict"),
                        0
                    );
                    assert_eq!(
                        delta(&before, &after, "filesystem.gate_hold.whole_file_replay"),
                        0
                    );
                    assert_eq!(
                        delta(&before, &after, "filesystem.mutation.request.committed"),
                        1
                    );
                    assert_eq!(
                        delta(&before, &after, "filesystem.mutation.request.reply_sent"),
                        1
                    );
                    assert_eq!(
                        delta(&before, &after, "filesystem.mutation.create_guard.passed"),
                        1
                    );
                }
                Case::OccupiedPath => {
                    assert_eq!(
                        delta(
                            &before,
                            &after,
                            "filesystem.mutation.create_guard.path_present"
                        ),
                        1
                    );
                    assert_eq!(
                        delta(&before, &after, "filesystem.mutation.request.conflict"),
                        1
                    );
                    assert_eq!(
                        delta(&before, &after, "filesystem.gate_hold.whole_file_replay"),
                        1
                    );
                    assert_eq!(
                        delta(&before, &after, "filesystem.mutation.request.reply_sent"),
                        1
                    );
                }
                Case::RemovedParent => {
                    assert_eq!(
                        delta(&before, &after, "filesystem.mutation.request.committed"),
                        0
                    );
                    assert_eq!(
                        delta(&before, &after, "filesystem.mutation.request.reply_sent"),
                        1
                    );
                }
            }
        },
        async {
            async_io::Timer::after(Duration::from_secs(10)).await;
            panic!("current-path paused create timed out");
        },
    ));
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn peer_allocation_rebases_prepared_create() {
    run(Case::PeerAllocation);
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn retargeted_symlink_rebases_into_current_parent() {
    run(Case::RetargetedSymlink);
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn occupied_current_path_keeps_guard_conflict_and_replay() {
    run(Case::OccupiedPath);
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn removed_current_parent_is_rejected() {
    run(Case::RemovedParent);
}
