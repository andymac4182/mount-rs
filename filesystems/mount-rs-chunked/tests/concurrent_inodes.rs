//! Independent inode revisions over durable SQLite metadata and blocks.
// Physical SQLite backing authority is currently supported on Unix only.
#![cfg(unix)]
use futures_lite::future::block_on;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::MetadataStore;
use mount_rs_core::{ErrorCode, FsDriver};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

type Fs = ChunkedFs<SqliteMetadataStore, SqliteBlockStore>;
struct Volume(PathBuf);
impl Volume {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mount-rs-inodes-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn metadata(&self) -> SqliteMetadataStore {
        SqliteMetadataStore::open(self.0.join("metadata.db")).unwrap()
    }
    async fn open(&self, owner: &str) -> Fs {
        ChunkedFs::open(
            self.metadata(),
            SqliteBlockStore::open(self.0.join("blocks.db")).unwrap(),
            ChunkedOptions::fixed(owner, 16)
                .unwrap()
                .with_inode_updates(true),
        )
        .await
        .unwrap()
    }
}
impl Drop for Volume {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn read_file(fs: &Fs, path: &str) -> Vec<u8> {
    let file = fs.open(path, "r", 0).await.unwrap();
    let mut bytes = vec![0; usize::try_from(file.stat().await.unwrap().size).unwrap()];
    let mut offset = 0;
    while offset < bytes.len() {
        let count = file
            .read(&mut bytes[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0, "file ended before its reported size");
        offset += count;
    }
    file.close().await.unwrap();
    bytes
}

#[test]
fn independent_handle_writes_preserve_structure_and_other_inode_revision() {
    block_on(async {
        let volume = Volume::new();
        let a = volume.open("a").await;
        a.write_file("/a", b"aaaa").await.unwrap();
        a.write_file("/b", b"bbbb").await.unwrap();
        let b = volume.open("b").await;
        let left = a.open("/a", "r+", 0).await.unwrap();
        let right = b.open("/b", "r+", 0).await.unwrap();
        let metadata = volume.metadata();
        let backing = metadata.inode_mode_state().await.unwrap().unwrap().backing;
        let before = metadata.load_inode_snapshot(backing).await.unwrap();
        left.write(b"LEFT", Some(0)).await.unwrap();
        let middle = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(middle.structural_generation, before.structural_generation);
        assert_eq!(
            middle
                .inode_revisions
                .iter()
                .filter(|(inode, revision)| before.inode_revisions.get(inode) != Some(revision))
                .count(),
            1
        );
        right.write(b"RIGHT", Some(0)).await.unwrap();
        let after = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(after.structural_generation, before.structural_generation);
        assert_eq!(
            after
                .inode_revisions
                .iter()
                .filter(|(inode, revision)| before.inode_revisions.get(inode) != Some(revision))
                .count(),
            2
        );
        let mut data = [0; 5];
        assert_eq!(right.read(&mut data, Some(0)).await.unwrap(), 5);
        assert_eq!(&data, b"RIGHT");
        assert_eq!(a.stat("/b").await.unwrap().size, 5);
        // Structural publication must fold both independently updated records.
        a.rename("/a", "/renamed").await.unwrap();
        assert_eq!(read_file(&b, "/renamed").await, b"LEFT");
        assert_eq!(read_file(&a, "/b").await, b"RIGHT");
        left.close().await.unwrap();
        right.close().await.unwrap();
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    });
}

#[test]
fn concurrent_append_replays_same_inode_and_detached_handle_keeps_latest_bytes() {
    let volume = Volume::new();
    let (a, b) = block_on(async {
        let a = volume.open("a").await;
        a.write_file("/data", b"").await.unwrap();
        (a, volume.open("b").await)
    });
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = [a.clone(), b.clone()]
        .into_iter()
        .enumerate()
        .map(|(index, fs)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                block_on(async {
                    let file = fs.open("/data", "a", 0).await.unwrap();
                    barrier.wait();
                    for _ in 0..12 {
                        file.write(&[b'A' + index as u8], None).await.unwrap();
                    }
                    file.close().await.unwrap();
                })
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    block_on(async {
        let bytes = read_file(&a, "/data").await;
        assert_eq!(bytes.len(), 24);
        assert_eq!(bytes.iter().filter(|byte| **byte == b'A').count(), 12);
        assert_eq!(bytes.iter().filter(|byte| **byte == b'B').count(), 12);
        let file = a.open("/data", "r+", 0).await.unwrap();
        file.write(b"latest", Some(0)).await.unwrap();
        b.unlink("/data").await.unwrap();
        let mut data = [0; 6];
        file.read(&mut data, Some(0)).await.unwrap();
        assert_eq!(&data, b"latest");
        file.write(b"orphan", Some(0)).await.unwrap();
        file.read(&mut data, Some(0)).await.unwrap();
        assert_eq!(&data, b"orphan");
        assert_eq!(a.stat("/data").await.unwrap_err().code, ErrorCode::Enoent);
        file.close().await.unwrap();
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    });
}

#[test]
fn established_inode_mode_requires_opt_in_and_rejects_writeback() {
    block_on(async {
        let volume = Volume::new();
        let fs = volume.open("inode-mode").await;
        fs.shutdown().await.unwrap();
        let blocks = || SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap();
        assert!(
            ChunkedFs::open(
                volume.metadata(),
                blocks(),
                ChunkedOptions::fixed("old", 16)
                    .unwrap()
                    .with_concurrent_writes(true)
            )
            .await
            .is_err()
        );
        assert_eq!(
            ChunkedFs::open(
                volume.metadata(),
                blocks(),
                ChunkedOptions::fixed("invalid", 16)
                    .unwrap()
                    .with_inode_updates(true)
                    .with_writeback(true)
            )
            .await
            .err()
            .unwrap()
            .code,
            ErrorCode::Einval
        );
    });
}

#[test]
fn existing_replacements_and_handle_truncates_only_publish_their_inode() {
    block_on(async {
        let volume = Volume::new();
        let a = volume.open("a").await;
        a.write_file("/left", b"old").await.unwrap();
        a.write_file("/right", b"other").await.unwrap();
        let b = volume.open("b").await;
        let metadata = volume.metadata();
        let backing = metadata.inode_mode_state().await.unwrap().unwrap().backing;
        let before = metadata.load_inode_snapshot(backing).await.unwrap();
        for bytes in [b"replacement".as_slice(), b"", b"last value"] {
            a.write_file("/left", bytes).await.unwrap();
            assert_eq!(read_file(&b, "/left").await, bytes);
        }
        let replaced = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(replaced.structural_generation, before.structural_generation);
        assert_eq!(
            replaced
                .inode_revisions
                .iter()
                .filter(|(inode, revision)| before.inode_revisions.get(inode) != Some(revision))
                .count(),
            1
        );
        let file = a.open("/left", "r+", 0).await.unwrap();
        file.truncate(4).await.unwrap();
        assert_eq!(read_file(&b, "/left").await, b"last");
        file.truncate(7).await.unwrap();
        assert_eq!(read_file(&b, "/left").await, b"last\0\0\0");
        let truncated = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(
            truncated.structural_generation,
            before.structural_generation
        );
        assert_eq!(
            truncated
                .inode_revisions
                .iter()
                .filter(|(inode, revision)| before.inode_revisions.get(inode) != Some(revision))
                .count(),
            1
        );
        file.close().await.unwrap();
        b.truncate("/left", 2).await.unwrap();
        assert_eq!(read_file(&a, "/left").await, b"la");
        let opened = a.open("/left", "w", 0).await.unwrap();
        opened.close().await.unwrap();
        assert!(read_file(&b, "/left").await.is_empty());
        assert_eq!(
            metadata
                .inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .structural_generation,
            before.structural_generation
        );
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    });
}

#[test]
fn simultaneous_replacement_and_other_inode_truncate_keep_both_results() {
    let volume = Volume::new();
    let (a, b) = block_on(async {
        let a = volume.open("a").await;
        a.write_file("/left", b"original").await.unwrap();
        a.write_file("/right", b"original").await.unwrap();
        (a, volume.open("b").await)
    });
    let generation = block_on(async {
        volume
            .metadata()
            .inode_mode_state()
            .await
            .unwrap()
            .unwrap()
            .structural_generation
    });
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let replacement = {
        let fs = a.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            block_on(async {
                barrier.wait();
                for _ in 0..12 {
                    fs.write_file("/left", b"replaced").await.unwrap();
                }
            })
        })
    };
    let truncate = {
        let fs = b.clone();
        std::thread::spawn(move || {
            block_on(async {
                let file = fs.open("/right", "r+", 0).await.unwrap();
                barrier.wait();
                for size in [2, 8, 2, 8, 2, 8, 2, 8, 2, 8, 2] {
                    file.truncate(size).await.unwrap();
                }
                file.close().await.unwrap();
            })
        })
    };
    replacement.join().unwrap();
    truncate.join().unwrap();
    block_on(async {
        assert_eq!(read_file(&b, "/left").await, b"replaced");
        assert_eq!(read_file(&a, "/right").await, b"or");
        assert_eq!(
            volume
                .metadata()
                .inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .structural_generation,
            generation
        );
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    });
}

// A deterministic peer completes enrollment at each await boundary in open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnrollmentBoundary {
    InitialInspection,
    LegacyInspection,
    Preflight,
    Load,
    RootPublication,
    Enrollment,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RaceFault {
    None,
    ChangedBacking,
    CorruptSnapshot,
    AmbiguousPublication,
    AmbiguousEnrollment,
}
struct RacingMetadata {
    inner: SqliteMetadataStore,
    path: PathBuf,
    boundary: EnrollmentBoundary,
    fault: RaceFault,
    fired: std::sync::atomic::AtomicBool,
}
impl RacingMetadata {
    async fn peer(&self, boundary: EnrollmentBoundary) {
        if self.boundary != boundary || self.fired.swap(true, Ordering::SeqCst) {
            return;
        }
        let peer = ChunkedFs::open(
            SqliteMetadataStore::open(self.path.join("metadata.db")).unwrap(),
            SqliteBlockStore::open(self.path.join("blocks.db")).unwrap(),
            ChunkedOptions::fixed("peer", 16)
                .unwrap()
                .with_inode_updates(true),
        )
        .await
        .unwrap();
        peer.write_file("/peer", b"peer authority bytes")
            .await
            .unwrap();
        peer.shutdown().await.unwrap();
    }
}
use async_trait::async_trait;
use mount_rs_core::storage::{
    ConcurrentBackingId, ConcurrentModeState, InodeMetadataSnapshot, InodeModeState,
    LoadedMetadata, Namespace, WriterLease,
};
use mount_rs_core::{FsError, Result};
use std::time::Duration;
#[async_trait]
impl MetadataStore for RacingMetadata {
    fn durable(&self) -> bool {
        true
    }
    async fn load(&self) -> Result<LoadedMetadata> {
        self.peer(EnrollmentBoundary::Load).await;
        self.inner.load().await
    }
    async fn inode_mode_state(&self) -> Result<Option<InodeModeState>> {
        let state = self.inner.inode_mode_state().await?;
        self.peer(EnrollmentBoundary::InitialInspection).await;
        if self.fault == RaceFault::ChangedBacking && self.fired.load(Ordering::SeqCst) {
            return Ok(state.map(|mut mode| {
                mode.backing = ConcurrentBackingId::from_bytes([42; 16]).unwrap();
                mode
            }));
        }
        Ok(state)
    }
    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        // TiDB's legacy inspector reports EIO for MRC4, unlike its load ESTALE.
        if self.inner.inode_mode_state().await?.is_some() {
            return Err(FsError::backend("legacy inspector cannot decode MRC4"));
        }
        let state = self.inner.concurrent_mode_state().await?;
        self.peer(EnrollmentBoundary::LegacyInspection).await;
        Ok(state)
    }
    async fn preflight_new_bound_mode(&self) -> Result<()> {
        let result = self.inner.preflight_new_bound_mode().await;
        self.peer(EnrollmentBoundary::Preflight).await;
        result
    }
    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        self.inner.prepare_bound_concurrent_mode(backing).await
    }
    async fn prepare_inode_mode(&self, backing: ConcurrentBackingId, revision: u64) -> Result<()> {
        self.peer(EnrollmentBoundary::Enrollment).await;
        if self.fault == RaceFault::AmbiguousEnrollment {
            return Err(FsError::backend("lost commit acknowledgement"));
        }
        self.inner.prepare_inode_mode(backing, revision).await
    }
    async fn load_inode_snapshot(
        &self,
        backing: ConcurrentBackingId,
    ) -> Result<InodeMetadataSnapshot> {
        let mut snapshot = self.inner.load_inode_snapshot(backing).await?;
        if self.fault == RaceFault::CorruptSnapshot {
            snapshot.inode_revisions.clear();
        }
        Ok(snapshot)
    }
    async fn load_inode(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
    ) -> Result<mount_rs_core::storage::LoadedInode> {
        self.inner.load_inode(backing, inode).await
    }
    async fn load_inode_if_changed(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
        known: Option<mount_rs_core::storage::InodeVersion>,
    ) -> Result<Option<mount_rs_core::storage::LoadedInode>> {
        self.inner
            .load_inode_if_changed(backing, inode, known)
            .await
    }
    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        revision: u64,
        ns: Namespace,
    ) -> Result<u64> {
        self.peer(EnrollmentBoundary::RootPublication).await;
        if self.fault == RaceFault::AmbiguousPublication {
            return Err(FsError::backend("lost commit acknowledgement"));
        }
        self.inner
            .publish_bound_if_revision(backing, revision, ns)
            .await
    }
    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        self.inner.acquire_writer(owner, ttl).await
    }
    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        self.inner.renew_writer(lease, ttl).await
    }
    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        self.inner.release_writer(lease).await
    }
    async fn publish(&self, revision: u64, lease: &WriterLease, ns: Namespace) -> Result<u64> {
        self.inner.publish(revision, lease, ns).await
    }
    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }
}
#[test]
fn peer_inode_enrollment_at_every_startup_boundary_preserves_authority() {
    block_on(async {
        let mut failures = Vec::new();
        for boundary in [
            EnrollmentBoundary::InitialInspection,
            EnrollmentBoundary::LegacyInspection,
            EnrollmentBoundary::Preflight,
            EnrollmentBoundary::Load,
            EnrollmentBoundary::RootPublication,
            EnrollmentBoundary::Enrollment,
        ] {
            let volume = Volume::new();
            let metadata = RacingMetadata {
                inner: volume.metadata(),
                path: volume.0.clone(),
                boundary,
                fault: RaceFault::None,
                fired: false.into(),
            };
            let result = ChunkedFs::open(
                metadata,
                SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
                ChunkedOptions::fixed("racing", 16)
                    .unwrap()
                    .with_inode_updates(true),
            )
            .await;
            let fs = match result {
                Ok(fs) => fs,
                Err(error) => {
                    failures.push(format!("{boundary:?}: {error:?}"));
                    continue;
                }
            };
            assert_eq!(fs.stat("/peer").await.unwrap().size, 20);
            let handle = fs.open("/peer", "r", 0).await.unwrap();
            let mut bytes = [0; 20];
            assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 20);
            assert_eq!(&bytes, b"peer authority bytes");
            handle.close().await.unwrap();
            let backing = volume
                .metadata()
                .inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .backing;
            mount_rs_core::storage::BlockStore::verify_concurrent_backing(
                &SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
                backing,
            )
            .await
            .unwrap();
            fs.shutdown().await.unwrap();
        }
        assert!(
            failures.is_empty(),
            "startup boundaries failed: {failures:#?}"
        );
    });
}

#[test]
fn startup_recovery_rejects_changed_authority_corrupt_snapshot_and_ambiguous_commits() {
    block_on(async {
        for (boundary, fault) in [
            (EnrollmentBoundary::Load, RaceFault::ChangedBacking),
            (
                EnrollmentBoundary::InitialInspection,
                RaceFault::CorruptSnapshot,
            ),
            (
                EnrollmentBoundary::RootPublication,
                RaceFault::AmbiguousPublication,
            ),
            (
                EnrollmentBoundary::Enrollment,
                RaceFault::AmbiguousEnrollment,
            ),
        ] {
            let volume = Volume::new();
            let metadata = RacingMetadata {
                inner: volume.metadata(),
                path: volume.0.clone(),
                boundary,
                fault,
                fired: false.into(),
            };
            let result = ChunkedFs::open(
                metadata,
                SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
                ChunkedOptions::fixed("rejected", 16)
                    .unwrap()
                    .with_inode_updates(true),
            )
            .await;
            assert!(result.is_err(), "invalid recovery at {boundary:?}");
            if matches!(
                fault,
                RaceFault::AmbiguousPublication | RaceFault::AmbiguousEnrollment
            ) {
                assert_eq!(result.err().unwrap().code, ErrorCode::Eio);
            }
        }
        let volume = Volume::new();
        let fs = volume.open("established").await;
        fs.shutdown().await.unwrap();
        let backing = volume
            .metadata()
            .inode_mode_state()
            .await
            .unwrap()
            .unwrap()
            .backing;
        let missing = SqliteBlockStore::open(volume.0.join("missing-marker.db")).unwrap();
        assert!(
            ChunkedFs::open(
                volume.metadata(),
                missing.clone(),
                ChunkedOptions::fixed("missing", 16)
                    .unwrap()
                    .with_inode_updates(true)
            )
            .await
            .is_err()
        );
        assert!(
            mount_rs_core::storage::BlockStore::verify_concurrent_backing(&missing, backing)
                .await
                .is_err()
        );
    });
}
