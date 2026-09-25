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

/// Count completed full reads independently of the process-wide clone profiler.
/// Fixture creation uses the ordinary store, so these counters describe only
/// the independently reopened reader and its measured operation window.
struct CountingInodeMetadata {
    inner: SqliteMetadataStore,
    snapshots: AtomicU64,
    snapshot_nodes: AtomicU64,
    fault: std::sync::atomic::AtomicU8,
    selected_checks: AtomicU64,
    selected_bodies: AtomicU64,
    peer: Option<Fs>,
    late_snapshot: std::sync::Mutex<Option<InodeMetadataSnapshot>>,
}
impl CountingInodeMetadata {
    fn new(inner: SqliteMetadataStore, peer: Option<Fs>) -> Self {
        Self {
            inner,
            snapshots: AtomicU64::new(0),
            snapshot_nodes: AtomicU64::new(0),
            fault: 0.into(),
            selected_checks: AtomicU64::new(0),
            selected_bodies: AtomicU64::new(0),
            peer,
            late_snapshot: std::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl MetadataStore for CountingInodeMetadata {
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    async fn load(&self) -> Result<LoadedMetadata> {
        self.inner.load().await
    }
    async fn inode_mode_state(&self) -> Result<Option<InodeModeState>> {
        self.inner.inode_mode_state().await
    }
    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        self.inner.concurrent_mode_state().await
    }
    async fn load_inode_snapshot_if_changed(
        &self,
        backing: ConcurrentBackingId,
        known: Option<u64>,
    ) -> Result<Option<InodeMetadataSnapshot>> {
        match self.fault.load(Ordering::SeqCst) {
            1 => {
                let mut snapshot = self.inner.load_inode_snapshot(backing).await?;
                snapshot
                    .namespace
                    .nodes
                    .get_mut(&snapshot.namespace.root)
                    .unwrap()
                    .stats
                    .mode ^= 0o111;
                return Ok(Some(snapshot));
            }
            5 => std::future::pending::<()>().await,
            6 => {
                return Ok(self.late_snapshot.lock().unwrap().clone());
            }
            _ => {}
        }
        let loaded = self
            .inner
            .load_inode_snapshot_if_changed(backing, known)
            .await?;
        if self
            .fault
            .compare_exchange(3, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let peer = self.peer.as_ref().unwrap();
            peer.rename("/file", "/previous").await?;
            peer.write_file("/file", b"replacement").await?;
        }
        if let Some(snapshot) = &loaded {
            self.snapshots.fetch_add(1, Ordering::SeqCst);
            self.snapshot_nodes
                .fetch_add(snapshot.namespace.nodes.len() as u64, Ordering::SeqCst);
        }
        Ok(loaded)
    }
    async fn load_inode_snapshot(
        &self,
        backing: ConcurrentBackingId,
    ) -> Result<InodeMetadataSnapshot> {
        let snapshot = self.inner.load_inode_snapshot(backing).await?;
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        self.snapshot_nodes
            .fetch_add(snapshot.namespace.nodes.len() as u64, Ordering::SeqCst);
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
        self.selected_checks.fetch_add(1, Ordering::SeqCst);
        if self.fault.load(Ordering::SeqCst) == 4 {
            self.peer.as_ref().unwrap().chmod("/", 0o755).await?;
        }
        match self.fault.load(Ordering::SeqCst) {
            7 | 8 => {
                let mut loaded = self.inner.load_inode(backing, inode).await?;
                if self.fault.load(Ordering::SeqCst) == 7 {
                    loaded.node.stats.mode ^= 0o111;
                } else {
                    loaded.version.inode_revision += 1;
                }
                return Ok(Some(loaded));
            }
            9 => std::future::pending::<()>().await,
            10 => return Err(FsError::new(ErrorCode::Estale)),
            _ => {}
        }
        let mut loaded = self
            .inner
            .load_inode_if_changed(backing, inode, known)
            .await?;
        if self.fault.load(Ordering::SeqCst) == 2
            && let Some(loaded) = &mut loaded
            && matches!(loaded.node.data, mount_rs_core::storage::NodeData::File(_))
        {
            loaded.node.stats.mode ^= 0o111;
        }
        if loaded.is_some() {
            self.selected_bodies.fetch_add(1, Ordering::SeqCst);
        }
        Ok(loaded)
    }
    async fn publish_inode_if_version(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
        expected: mount_rs_core::storage::InodeVersion,
        node: mount_rs_core::storage::NodeMetadata,
    ) -> Result<mount_rs_core::storage::InodeVersion> {
        self.inner
            .publish_inode_if_version(backing, inode, expected, node)
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
#[ignore = "run alone with MOUNT_RS_PROFILE_IO=1 and --test-threads=1"]
fn existing_inode_paths_do_not_reload_or_clone_the_whole_namespace() {
    use mount_rs_core::diagnostics::profile;
    assert!(profile::enabled(), "start with MOUNT_RS_PROFILE_IO=1");
    block_on(async {
        const FILES: usize = 128;
        const OPERATIONS: usize = 12;
        let volume = Volume::new();
        let writer = volume.open("prepare-open-amplification").await;
        let contents: Vec<Vec<u8>> = (0..FILES)
            .map(|index| {
                (0..64)
                    .map(|offset| (index as u8).wrapping_add(offset as u8))
                    .collect()
            })
            .collect();
        for (index, content) in contents.iter().enumerate() {
            writer
                .write_file(&format!("/file-{index}"), content)
                .await
                .unwrap();
        }
        writer.shutdown().await.unwrap();
        let reader = ChunkedFs::open(
            CountingInodeMetadata::new(volume.metadata(), None),
            SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
            ChunkedOptions::fixed("read-open-amplification", 16)
                .unwrap()
                .with_inode_updates(true),
        )
        .await
        .unwrap();
        let metadata = reader.metadata_store();
        let generation = metadata.inode_mode_state().await.unwrap().unwrap();
        // Initial coherent loading is necessary and deliberately excluded.
        let snapshots_before = metadata.snapshots.load(Ordering::SeqCst);
        assert!(snapshots_before > 0);
        let nodes_before = metadata.snapshot_nodes.load(Ordering::SeqCst);
        let selected_before = metadata.selected_checks.load(Ordering::SeqCst);
        let bodies_before = metadata.selected_bodies.load(Ordering::SeqCst);
        let profile_before = profile::snapshot();
        for iteration in 0..OPERATIONS {
            // Repeat three existing targets to exercise cold and cached nodes.
            let index = [0, FILES / 2, FILES - 1][iteration % 3];
            let path = format!("/file-{index}");
            let file = reader.open(&path, "r", 0).await.unwrap();
            assert_eq!(reader.stat(&path).await.unwrap().size, 64);
            assert_eq!(file.stat().await.unwrap().size, 64);
            let mut actual = [0; 64];
            assert_eq!(file.read(&mut actual, Some(0)).await.unwrap(), 64);
            assert_eq!(actual.as_slice(), contents[index], "iteration {iteration}");
            assert_eq!(file.read(&mut actual, Some(64)).await.unwrap(), 0);
            file.close().await.unwrap();
        }
        let snapshots = metadata.snapshots.load(Ordering::SeqCst) - snapshots_before;
        let returned_nodes = metadata.snapshot_nodes.load(Ordering::SeqCst) - nodes_before;
        let selected_checks = metadata.selected_checks.load(Ordering::SeqCst) - selected_before;
        let selected_bodies = metadata.selected_bodies.load(Ordering::SeqCst) - bodies_before;
        let delta = profile::snapshot().delta(&profile_before).unwrap();
        let clones = delta
            .entries
            .iter()
            .find(|entry| entry.name == "filesystem.snapshot_nodes");
        let clone_calls = clones.map_or(0, |entry| entry.calls);
        let cloned_nodes = clones.map_or(0, |entry| entry.units);
        assert_eq!(metadata.inode_mode_state().await.unwrap(), Some(generation));
        reader.shutdown().await.unwrap();
        eprintln!(
            "existing inode paths: files={FILES}, full_byte_oracles={OPERATIONS}, \
             full_snapshots={snapshots}, returned_nodes={returned_nodes}, \
             namespace_clone_calls={clone_calls}, cloned_nodes={cloned_nodes}, \
             selected_checks={selected_checks}, selected_bodies={selected_bodies}"
        );
        assert_eq!(
            selected_bodies, 3,
            "only three distinct accessed file bodies are needed"
        );
        assert_eq!(
            (snapshots, clone_calls, cloned_nodes),
            (0, 0, 0),
            "unchanged existing-file open/read/stat must not load or clone the entire namespace; \
             all {OPERATIONS} full-byte oracles passed, but returned {returned_nodes} nodes"
        );
    });
}

async fn counted_reader(
    volume: &Volume,
    peer: Option<Fs>,
) -> ChunkedFs<CountingInodeMetadata, SqliteBlockStore> {
    ChunkedFs::open(
        CountingInodeMetadata::new(volume.metadata(), peer),
        SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
        ChunkedOptions::fixed("counted-reader", 16)
            .unwrap()
            .with_inode_updates(true),
    )
    .await
    .unwrap()
}

#[test]
fn conditional_paths_return_peer_selected_stats_and_structural_changes() {
    use mount_rs_core::driver::{GuardedRead, GuardedReadResult, PathGuard, PathIdentity};
    block_on(async {
        let volume = Volume::new();
        let writer = volume.open("writer").await;
        writer.write_file("/file", b"first").await.unwrap();
        writer.symlink("/file", "/link").await.unwrap();
        let reader = counted_reader(&volume, None).await;
        let handle = reader.open("/file", "r+", 0).await.unwrap();
        let original = handle.stat().await.unwrap();
        let generation = writer.metadata_store().inode_mode_state().await.unwrap();
        let root = reader.stat("/").await.unwrap();
        let guard = PathGuard {
            path: "/file".into(),
            identity: PathIdentity::from_stats(&original).unwrap(),
        };
        let parent = PathGuard {
            path: "/".into(),
            identity: PathIdentity::from_stats(&root).unwrap(),
        };
        writer
            .write_file("/file", b"much longer peer bytes")
            .await
            .unwrap();
        assert_eq!(
            writer.metadata_store().inode_mode_state().await.unwrap(),
            generation
        );
        let fresh = writer.stat("/file").await.unwrap();
        assert!(fresh.mtime_ms > original.mtime_ms);
        for stats in [
            reader.stat("/file").await.unwrap(),
            reader.lstat("/file").await.unwrap(),
            reader.stat("/link").await.unwrap(),
            handle.stat().await.unwrap(),
        ] {
            assert_eq!(stats, fresh);
        }
        let GuardedReadResult::Stat(stats) = reader
            .guarded_read(GuardedRead::Stat {
                target: guard.clone(),
            })
            .await
            .unwrap()
        else {
            panic!("stat")
        };
        assert_eq!(stats, fresh);
        let GuardedReadResult::Lookup {
            parent: parent_stats,
            child,
        } = reader
            .guarded_read(GuardedRead::Lookup {
                parent,
                name: "file".into(),
            })
            .await
            .unwrap()
        else {
            panic!("lookup")
        };
        assert_eq!(child, fresh);
        assert_eq!(parent_stats, reader.stat("/").await.unwrap());
        let mut bytes = [0; 22];
        assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), bytes.len());
        assert_eq!(&bytes, b"much longer peer bytes");
        handle.write(b"local", Some(22)).await.unwrap();
        assert_eq!(handle.stat().await.unwrap().size, 27);
        assert_eq!(reader.stat("/file").await.unwrap().size, 27);
        writer.chmod("/file", 0o600).await.unwrap();
        assert_eq!(reader.stat("/file").await.unwrap().mode & 0o777, 0o600);
        writer.chown("/file", 123, 456).await.unwrap();
        let stats = reader.stat("/file").await.unwrap();
        assert_eq!((stats.uid, stats.gid), (123, 456));
        writer.rename("/file", "/moved").await.unwrap();
        assert_eq!(
            reader.stat("/file").await.unwrap_err().code,
            ErrorCode::Enoent
        );
        assert_eq!(
            reader
                .guarded_read(GuardedRead::Stat { target: guard })
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(reader.stat("/moved").await.unwrap().ino, original.ino);
        writer.unlink("/link").await.unwrap();
        writer.symlink("/moved", "/link").await.unwrap();
        assert_eq!(reader.stat("/link").await.unwrap().size, 27);
        assert_ne!(reader.lstat("/link").await.unwrap().ino, original.ino);
        assert_eq!(reader.readlink("/link").await.unwrap(), "/moved");
        handle.close().await.unwrap();
        reader.shutdown().await.unwrap();
        writer.shutdown().await.unwrap();
    });
}

#[test]
fn conditional_path_discards_identity_after_structural_race_and_bounds_churn() {
    block_on(async {
        let volume = Volume::new();
        let peer = volume.open("peer").await;
        peer.write_file("/file", b"old").await.unwrap();
        let reader = counted_reader(&volume, Some(peer.clone())).await;
        let old_inode = peer.stat("/file").await.unwrap().ino;
        reader.metadata_store().fault.store(3, Ordering::SeqCst);
        let handle = reader.open("/file", "r", 0).await.unwrap();
        assert_ne!(handle.stat().await.unwrap().ino, old_inode);
        let mut bytes = [0; 11];
        assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 11);
        assert_eq!(&bytes, b"replacement");
        handle.close().await.unwrap();
        let metadata = reader.metadata_store();
        let checks_before = metadata.selected_checks.load(Ordering::SeqCst);
        metadata.fault.store(4, Ordering::SeqCst);
        let error = reader.open("/file", "r", 0).await.err().unwrap();
        assert_eq!(error.code, ErrorCode::Eagain);
        let checks = metadata.selected_checks.load(Ordering::SeqCst) - checks_before;
        assert_eq!(checks, 128, "the entire retry chain has a finite bound");
        metadata.fault.store(0, Ordering::SeqCst);
        let handle = reader.open("/file", "r", 0).await.unwrap();
        assert_eq!(
            handle.fd(),
            Some(4),
            "exhausted path retries must not allocate descriptors"
        );
        handle.close().await.unwrap();
        reader.shutdown().await.unwrap();
        peer.shutdown().await.unwrap();
    });
}

#[test]
fn conditional_path_rejects_same_generation_structural_payloads_before_installation() {
    block_on(async {
        for fault in [1, 2, 7, 8, 10] {
            let volume = Volume::new();
            let writer = volume.open("writer").await;
            writer.write_file("/file", b"bytes").await.unwrap();
            let reader = counted_reader(&volume, None).await;
            reader.metadata_store().fault.store(fault, Ordering::SeqCst);
            let error = reader.open("/file", "r", 0).await.err().unwrap();
            assert_eq!(error.code, ErrorCode::Estale, "fault {fault}");
            assert!(reader.failed());
            reader.metadata_store().fault.store(0, Ordering::SeqCst);
            assert_eq!(
                reader.stat("/file").await.unwrap_err().code,
                ErrorCode::Estale
            );
            let _ = reader.shutdown().await;
            writer.shutdown().await.unwrap();
        }
    });
}

#[test]
fn cancelled_conditional_path_does_not_allocate_descriptor_or_poison_reader() {
    for fault in [5, 9] {
        block_on(async {
            let volume = Volume::new();
            let writer = volume.open("writer").await;
            writer.write_file("/file", b"bytes").await.unwrap();
            let reader = counted_reader(&volume, None).await;
            reader.metadata_store().fault.store(fault, Ordering::SeqCst);
            let mut pending = Box::pin(reader.open("/file", "r", 0));
            assert!(
                futures_lite::future::poll_once(pending.as_mut())
                    .await
                    .is_none()
            );
            drop(pending);
            reader.metadata_store().fault.store(0, Ordering::SeqCst);
            let handle = reader.open("/file", "r", 0).await.unwrap();
            assert_eq!(handle.fd(), Some(3));
            assert!(!reader.failed());
            handle.close().await.unwrap();
            reader.shutdown().await.unwrap();
            assert_eq!(
                reader.open("/file", "r", 0).await.err().unwrap().code,
                ErrorCode::Ebadf
            );
            writer.shutdown().await.unwrap();
        });
    }
}

#[test]
fn custom_inode_provider_conditional_default_still_fully_loads() {
    block_on(async {
        let volume = Volume::new();
        let writer = volume.open("writer").await;
        writer
            .write_file("/file", b"default snapshot")
            .await
            .unwrap();
        let metadata = RacingMetadata {
            inner: volume.metadata(),
            path: volume.0.clone(),
            boundary: EnrollmentBoundary::InitialInspection,
            fault: RaceFault::None,
            fired: true.into(),
        };
        let state = metadata.inode_mode_state().await.unwrap().unwrap();
        let snapshot = metadata
            .load_inode_snapshot_if_changed(state.backing, Some(state.structural_generation))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.structural_generation, state.structural_generation);
        assert_eq!(snapshot.namespace.nodes.len(), 2);
        snapshot.validate().unwrap();
        writer.shutdown().await.unwrap();
    });
}

#[test]
fn delayed_older_snapshot_cannot_replace_newer_runtime_identity() {
    block_on(async {
        let volume = Volume::new();
        let writer = volume.open("writer").await;
        writer.write_file("/file", b"old").await.unwrap();
        let reader = counted_reader(&volume, None).await;
        let metadata = reader.metadata_store();
        let backing = metadata.inode_mode_state().await.unwrap().unwrap().backing;
        let late_snapshot = metadata.inner.load_inode_snapshot(backing).await.unwrap();
        *metadata.late_snapshot.lock().unwrap() = Some(late_snapshot);
        writer.rename("/file", "/previous").await.unwrap();
        writer
            .write_file("/file", b"current identity")
            .await
            .unwrap();
        let expected = reader.stat("/file").await.unwrap();
        let metadata = reader.metadata_store();
        metadata.fault.store(6, Ordering::SeqCst);
        assert_eq!(
            reader.open("/file", "r", 0).await.err().unwrap().code,
            ErrorCode::Eagain
        );
        metadata.fault.store(0, Ordering::SeqCst);
        let handle = reader.open("/file", "r", 0).await.unwrap();
        assert_eq!(handle.fd(), Some(3));
        assert_eq!(handle.stat().await.unwrap(), expected);
        let mut bytes = [0; 16];
        assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 16);
        assert_eq!(&bytes, b"current identity");
        handle.close().await.unwrap();
        reader.shutdown().await.unwrap();
        writer.shutdown().await.unwrap();
    });
}
