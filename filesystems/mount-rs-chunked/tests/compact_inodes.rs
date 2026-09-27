use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::ErrorCode;
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};

#[cfg(unix)]
mod sqlite_runtime {
    use super::*;
    use async_trait::async_trait;
    use mount_rs_core::FsDriver;
    use mount_rs_core::storage::compact::{
        CompactInodeCapability, CompactPublication, CompactSnapshot, CompactStructuralDelta,
        LoadedCompactInode, PhysicalInodeIdentity,
    };
    use mount_rs_core::storage::{
        BlockId, BlockStore, ConcurrentBackingId, ConcurrentModeState, InodeMetadataSnapshot,
        InodeModeState, InodeVersion, LoadedInode, LoadedMetadata, MetadataStore, Namespace,
        NodeData, NodeMetadata, WriterLease,
    };
    use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Volume(PathBuf);
    impl Volume {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "mount-rs-compact-runtime-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn metadata(&self) -> SqliteMetadataStore {
            SqliteMetadataStore::open(self.0.join("metadata.db")).unwrap()
        }

        fn blocks(&self) -> SqliteBlockStore {
            SqliteBlockStore::open(self.0.join("blocks.db")).unwrap()
        }

        async fn open(&self, owner: &str) -> ChunkedFs<SqliteMetadataStore, SqliteBlockStore> {
            ChunkedFs::open(
                self.metadata(),
                self.blocks(),
                ChunkedOptions::fixed(owner, 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
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

    #[derive(Clone)]
    struct RecordingMetadata {
        inner: SqliteMetadataStore,
        modeled: Arc<Mutex<Option<CompactSnapshot>>>,
        submitted: Arc<Mutex<Vec<PhysicalInodeIdentity>>>,
        old_calls: Arc<AtomicU64>,
        selected_calls: Arc<AtomicU64>,
        full_calls: Arc<AtomicU64>,
        lose_selected_ack: Arc<AtomicBool>,
        corrupt_selected_receipt: Arc<AtomicBool>,
        corrupt_full_receipt: Arc<AtomicBool>,
        foreign_snapshot_once: Arc<AtomicBool>,
        backward_snapshot_once: Arc<Mutex<Option<CompactSnapshot>>>,
        force_flush: Arc<AtomicBool>,
        fail_flush_once: Arc<AtomicBool>,
        hold_selected_once: Arc<AtomicBool>,
        selected_entered: Arc<tokio::sync::Notify>,
        selected_resume: Arc<tokio::sync::Notify>,
        hold_load_once: Arc<AtomicBool>,
        corrupt_selected_load: Arc<AtomicBool>,
        load_entered: Arc<tokio::sync::Notify>,
        load_resume: Arc<tokio::sync::Notify>,
        hold_full_once: Arc<AtomicBool>,
        full_entered: Arc<tokio::sync::Notify>,
        full_resume: Arc<tokio::sync::Notify>,
    }

    impl RecordingMetadata {
        fn new(inner: SqliteMetadataStore) -> Self {
            Self {
                inner,
                modeled: Arc::new(Mutex::new(None)),
                submitted: Arc::new(Mutex::new(Vec::new())),
                old_calls: Arc::new(AtomicU64::new(0)),
                selected_calls: Arc::new(AtomicU64::new(0)),
                full_calls: Arc::new(AtomicU64::new(0)),
                lose_selected_ack: Arc::new(AtomicBool::new(false)),
                corrupt_selected_receipt: Arc::new(AtomicBool::new(false)),
                corrupt_full_receipt: Arc::new(AtomicBool::new(false)),
                foreign_snapshot_once: Arc::new(AtomicBool::new(false)),
                backward_snapshot_once: Arc::new(Mutex::new(None)),
                force_flush: Arc::new(AtomicBool::new(false)),
                fail_flush_once: Arc::new(AtomicBool::new(false)),
                hold_selected_once: Arc::new(AtomicBool::new(false)),
                selected_entered: Arc::new(tokio::sync::Notify::new()),
                selected_resume: Arc::new(tokio::sync::Notify::new()),
                hold_load_once: Arc::new(AtomicBool::new(false)),
                corrupt_selected_load: Arc::new(AtomicBool::new(false)),
                load_entered: Arc::new(tokio::sync::Notify::new()),
                load_resume: Arc::new(tokio::sync::Notify::new()),
                hold_full_once: Arc::new(AtomicBool::new(false)),
                full_entered: Arc::new(tokio::sync::Notify::new()),
                full_resume: Arc::new(tokio::sync::Notify::new()),
            }
        }

        fn old(&self) -> mount_rs_core::Result<()> {
            self.old_calls.fetch_add(1, Ordering::SeqCst);
            Err(mount_rs_core::FsError::new(ErrorCode::Eio)
                .with_message("MRC4 method reached from compact filesystem"))
        }
    }

    #[async_trait]
    impl MetadataStore for RecordingMetadata {
        fn durable(&self) -> bool {
            self.inner.durable()
        }
        fn publish_includes_flush_barrier(&self) -> bool {
            !self.force_flush.load(Ordering::SeqCst) && self.inner.publish_includes_flush_barrier()
        }
        fn compact_inode_capability(&self) -> CompactInodeCapability {
            self.inner.compact_inode_capability()
        }
        async fn compact_inode_mode_state(&self) -> mount_rs_core::Result<Option<InodeModeState>> {
            if let Some(snapshot) = self.modeled.lock().unwrap().as_ref() {
                return Ok(Some(InodeModeState {
                    backing: snapshot.anchor.backing,
                    structural_generation: snapshot.anchor.generation,
                }));
            }
            self.inner.compact_inode_mode_state().await
        }
        async fn prepare_compact_inode_mode(
            &self,
            backing: ConcurrentBackingId,
            revision: u64,
        ) -> mount_rs_core::Result<()> {
            self.inner
                .prepare_compact_inode_mode(backing, revision)
                .await
        }
        async fn load_compact_snapshot(
            &self,
            backing: ConcurrentBackingId,
        ) -> mount_rs_core::Result<CompactSnapshot> {
            if let Some(snapshot) = self.modeled.lock().unwrap().as_ref() {
                assert_eq!(snapshot.anchor.backing, backing);
                return Ok(snapshot.clone());
            }
            let mut snapshot = self.inner.load_compact_snapshot(backing).await?;
            if self.foreign_snapshot_once.swap(false, Ordering::SeqCst) {
                snapshot.anchor.backing =
                    ConcurrentBackingId::from_hex("00000000000000000000000000000001").unwrap();
                assert_ne!(snapshot.anchor.backing, backing);
            }
            if let Some(older) = self.backward_snapshot_once.lock().unwrap().take() {
                snapshot = older;
            }
            Ok(snapshot)
        }
        async fn load_compact_inode(
            &self,
            backing: ConcurrentBackingId,
            inode: u64,
        ) -> mount_rs_core::Result<LoadedCompactInode> {
            if let Some(snapshot) = self.modeled.lock().unwrap().as_ref() {
                assert_eq!(snapshot.anchor.backing, backing);
                return LoadedCompactInode::from_guard(
                    &snapshot.anchor,
                    inode,
                    snapshot.guards[&inode].clone(),
                );
            }
            let mut loaded = self.inner.load_compact_inode(backing, inode).await?;
            if matches!(loaded.guard.node.data, NodeData::File(_))
                && self.corrupt_selected_load.swap(false, Ordering::SeqCst)
            {
                loaded.guard.identity.epoch = loaded.generation + 1;
            }
            if self.hold_load_once.swap(false, Ordering::SeqCst) {
                self.load_entered.notify_one();
                self.load_resume.notified().await;
            }
            Ok(loaded)
        }
        async fn publish_compact_inode(
            &self,
            backing: ConcurrentBackingId,
            inode: u64,
            generation: u64,
            expected: PhysicalInodeIdentity,
            node: NodeMetadata,
        ) -> mount_rs_core::Result<LoadedCompactInode> {
            self.selected_calls.fetch_add(1, Ordering::SeqCst);
            self.submitted.lock().unwrap().push(expected);
            if let Some(snapshot) = self.modeled.lock().unwrap().as_mut() {
                *snapshot = snapshot.selected_update(backing, generation, inode, expected, node)?;
                return LoadedCompactInode::from_guard(
                    &snapshot.anchor,
                    inode,
                    snapshot.guards[&inode].clone(),
                );
            }
            if self.hold_selected_once.swap(false, Ordering::SeqCst) {
                self.selected_entered.notify_one();
                self.selected_resume.notified().await;
            }
            let mut receipt = self
                .inner
                .publish_compact_inode(backing, inode, generation, expected, node)
                .await?;
            if self.corrupt_selected_receipt.swap(false, Ordering::SeqCst) {
                receipt.guard.identity.revision += 1;
            }
            if self.lose_selected_ack.swap(false, Ordering::SeqCst) {
                return Err(mount_rs_core::FsError::new(ErrorCode::Eio)
                    .with_message("injected lost selected acknowledgement"));
            }
            Ok(receipt)
        }
        async fn publish_compact_structure(
            &self,
            delta: &CompactStructuralDelta,
        ) -> mount_rs_core::Result<CompactPublication> {
            self.full_calls.fetch_add(1, Ordering::SeqCst);
            if self.hold_full_once.swap(false, Ordering::SeqCst) {
                self.full_entered.notify_one();
                self.full_resume.notified().await;
            }
            let mut receipt = self.inner.publish_compact_structure(delta).await?;
            if self.corrupt_full_receipt.swap(false, Ordering::SeqCst) {
                receipt.anchor.generation += 1;
            }
            Ok(receipt)
        }
        async fn load(&self) -> mount_rs_core::Result<LoadedMetadata> {
            self.old()?;
            unreachable!()
        }
        async fn inode_mode_state(&self) -> mount_rs_core::Result<Option<InodeModeState>> {
            self.old()?;
            unreachable!()
        }
        async fn concurrent_mode_state(&self) -> mount_rs_core::Result<ConcurrentModeState> {
            self.old()?;
            unreachable!()
        }
        async fn load_inode_snapshot(
            &self,
            _: ConcurrentBackingId,
        ) -> mount_rs_core::Result<InodeMetadataSnapshot> {
            self.old()?;
            unreachable!()
        }
        async fn load_inode(
            &self,
            _: ConcurrentBackingId,
            _: u64,
        ) -> mount_rs_core::Result<LoadedInode> {
            self.old()?;
            unreachable!()
        }
        async fn publish_inode_if_version(
            &self,
            _: ConcurrentBackingId,
            _: u64,
            _: InodeVersion,
            _: NodeMetadata,
        ) -> mount_rs_core::Result<InodeVersion> {
            self.old()?;
            unreachable!()
        }
        async fn publish_structure_if_versions(
            &self,
            _: ConcurrentBackingId,
            _: u64,
            _: &std::collections::BTreeMap<u64, u64>,
            _: Namespace,
        ) -> mount_rs_core::Result<u64> {
            self.old()?;
            unreachable!()
        }
        async fn acquire_writer(&self, _: &str, _: Duration) -> mount_rs_core::Result<WriterLease> {
            self.old()?;
            unreachable!()
        }
        async fn renew_writer(
            &self,
            _: &WriterLease,
            _: Duration,
        ) -> mount_rs_core::Result<WriterLease> {
            self.old()?;
            unreachable!()
        }
        async fn release_writer(&self, _: &WriterLease) -> mount_rs_core::Result<()> {
            self.old()
        }
        async fn publish(
            &self,
            _: u64,
            _: &WriterLease,
            _: Namespace,
        ) -> mount_rs_core::Result<u64> {
            self.old()?;
            unreachable!()
        }
        async fn publish_bound_if_revision(
            &self,
            _: ConcurrentBackingId,
            _: u64,
            _: Namespace,
        ) -> mount_rs_core::Result<u64> {
            self.old()?;
            unreachable!()
        }
        async fn flush(&self) -> mount_rs_core::Result<()> {
            if self.fail_flush_once.swap(false, Ordering::SeqCst) {
                return Err(mount_rs_core::FsError::new(ErrorCode::Eio)
                    .with_message("injected required metadata flush failure"));
            }
            self.inner.flush().await
        }
    }

    #[derive(Clone)]
    struct PausedBlocks {
        inner: SqliteBlockStore,
        pause: Arc<AtomicBool>,
        entered: Arc<tokio::sync::Notify>,
        resume: Arc<tokio::sync::Notify>,
        puts: Arc<AtomicU64>,
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
        async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
            self.puts.fetch_add(1, Ordering::SeqCst);
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
    }

    // The recording seam models the permitted SQL late acknowledgement ordering.
    // SQLite supplies real immutable blocks and valid initial snapshots; this is
    // not a claim that SQLite executed the old-generation SQL transaction race.
    async fn captured_body_race(operation: &str) {
        let volume = Volume::new();
        let setup = volume.open("capture-setup").await;
        setup.write_file("/a", b"base").await.unwrap();
        let inode = setup.stat("/a").await.unwrap().ino;
        let backing = volume
            .metadata()
            .compact_inode_mode_state()
            .await
            .unwrap()
            .unwrap()
            .backing;
        let old_snapshot = volume
            .metadata()
            .load_compact_snapshot(backing)
            .await
            .unwrap();
        let old = old_snapshot.guards[&inode].clone();
        let peer = setup.open("/a", "a", 0).await.unwrap();
        peer.write(b"PEER", None).await.unwrap();
        peer.close().await.unwrap();
        let peer_snapshot = volume
            .metadata()
            .load_compact_snapshot(backing)
            .await
            .unwrap();
        let newer = peer_snapshot.guards[&inode].clone();
        // This is exactly the selected transition that the delayed transaction
        // validated before an unrelated structure transaction advanced the anchor.
        assert_eq!(
            old_snapshot
                .selected_update(
                    backing,
                    old_snapshot.anchor.generation,
                    inode,
                    old.identity,
                    newer.node.clone()
                )
                .unwrap(),
            peer_snapshot
        );
        setup.write_file("/unrelated", b"other").await.unwrap();
        let mut snapshot = volume
            .metadata()
            .load_compact_snapshot(backing)
            .await
            .unwrap();
        setup.shutdown().await.unwrap();
        let generation = snapshot.anchor.generation;
        assert!(old.identity.epoch < generation && newer.identity.epoch < generation);
        assert_ne!(old.identity, newer.identity);
        assert_eq!(
            old.identity.logical_version(generation).unwrap(),
            newer.identity.logical_version(generation).unwrap()
        );
        snapshot.guards.insert(inode, old.clone());
        snapshot.namespace().unwrap();
        let recorder = RecordingMetadata::new(volume.metadata());
        *recorder.modeled.lock().unwrap() = Some(snapshot);
        let blocks = PausedBlocks {
            inner: volume.blocks(),
            pause: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(tokio::sync::Notify::new()),
            resume: Arc::new(tokio::sync::Notify::new()),
            puts: Arc::new(AtomicU64::new(0)),
        };
        let fs = ChunkedFs::open(
            recorder.clone(),
            blocks.clone(),
            ChunkedOptions::fixed("capture-runtime", 16)
                .unwrap()
                .with_compact_inode_updates(true),
        )
        .await
        .unwrap();
        let handle = fs
            .open("/a", if operation == "append" { "a" } else { "r+" }, 0)
            .await
            .unwrap();
        blocks.pause.store(true, Ordering::SeqCst);
        let mutate = async {
            match operation {
                "replace" => fs.write_file("/a", b"replacement").await.unwrap(),
                "append" => {
                    handle.write(b"TAIL", None).await.unwrap();
                }
                _ => {
                    handle.write(b"X", Some(0)).await.unwrap();
                }
            }
        };
        let refresh = async {
            blocks.entered.notified().await;
            {
                let mut modeled = recorder.modeled.lock().unwrap();
                let snapshot = modeled.as_mut().unwrap();
                // Commit only the already-validated selected guard, as allowed by
                // a late SQL acknowledgement; preserve the newer anchor.
                snapshot.guards.insert(inode, newer.clone());
                snapshot.namespace().unwrap();
            }
            if operation != "partial-without-refresh" {
                assert_eq!(fs.stat("/a").await.unwrap().size, 8);
            }
            blocks.resume.notify_one();
        };
        futures_lite::future::zip(mutate, refresh).await;
        handle.close().await.unwrap();
        let submitted = recorder.submitted.lock().unwrap().clone();
        println!(
            "CAPTURE_RACE operation={operation} original={:?} refreshed={:?} submitted={submitted:?} puts={}",
            old.identity,
            newer.identity,
            blocks.puts.load(Ordering::SeqCst)
        );
        // A local known conflict may avoid submitting old.identity. Otherwise
        // exact CAS must reject it. In either case prepare from the fresh body.
        assert_eq!(submitted.last(), Some(&newer.identity));
        if operation == "partial-without-refresh" {
            assert_eq!(submitted, vec![old.identity, newer.identity]);
        }
        assert!(
            submitted
                .iter()
                .all(|token| *token == old.identity || *token == newer.identity)
        );
        let fresh = ChunkedFs::open(
            recorder.clone(),
            volume.blocks(),
            ChunkedOptions::fixed("capture-fresh", 16)
                .unwrap()
                .with_compact_inode_updates(true),
        )
        .await
        .unwrap();
        let expected: &[u8] = match operation {
            "replace" => b"replacement",
            "append" => b"basePEERTAIL",
            _ => b"XasePEER",
        };
        let reader = fresh.open("/a", "r", 0).await.unwrap();
        let mut bytes = vec![0; expected.len() + 1];
        assert_eq!(
            reader.read(&mut bytes, Some(0)).await.unwrap(),
            expected.len()
        );
        assert_eq!(&bytes[..expected.len()], expected);
        assert_eq!(
            reader
                .read(&mut bytes, Some(expected.len() as u64))
                .await
                .unwrap(),
            0
        );
        assert!(
            blocks.puts.load(Ordering::SeqCst) >= 2,
            "superseded captured body was not prepared again"
        );
        reader.close().await.unwrap();
        fresh.shutdown().await.unwrap();
        fs.shutdown().await.unwrap();
        assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
    }

    fn bounded_capture_race(operation: &str) {
        futures_lite::future::block_on(futures_lite::future::race(
            captured_body_race(operation),
            async {
                async_io::Timer::after(Duration::from_secs(10)).await;
                panic!("captured body race timed out");
            },
        ));
    }

    #[test]
    fn captured_physical_partial_write_recaptures_after_same_runtime_refresh() {
        bounded_capture_race("partial");
    }
    #[test]
    fn captured_physical_append_recaptures_after_same_runtime_refresh() {
        bounded_capture_race("append");
    }
    #[test]
    fn captured_physical_partial_submits_original_token_without_local_refresh() {
        bounded_capture_race("partial-without-refresh");
    }
    #[test]
    fn captured_physical_replace_recaptures_after_same_runtime_refresh() {
        bounded_capture_race("replace");
    }

    #[derive(Clone)]
    struct SimultaneousInitializer {
        inner: SqliteMetadataStore,
        first_read: Option<Arc<std::sync::Barrier>>,
        waited: Arc<AtomicBool>,
        advance: Option<ChunkedFs<SqliteMetadataStore, SqliteBlockStore>>,
        startup_fault: Option<(StartupFaultPoint, ErrorCode)>,
        fault_fired: Arc<AtomicBool>,
        peer_blocks: Option<PathBuf>,
        prepare_calls: Arc<AtomicU64>,
        root_publish_calls: Arc<AtomicU64>,
        force_flush: bool,
        flush_calls: Arc<AtomicU64>,
        fail_flush_at: u64,
        hold_flush_at: u64,
        flush_entered: Arc<tokio::sync::Notify>,
        flush_resume: Arc<tokio::sync::Notify>,
        enrollment_calls: Arc<AtomicU64>,
        hold_enroll_before_call: Arc<AtomicBool>,
        enroll_entered: Arc<tokio::sync::Notify>,
        enroll_resume: Arc<tokio::sync::Notify>,
        hold_root_after_commit: Arc<AtomicBool>,
        root_entered: Arc<tokio::sync::Notify>,
        root_resume: Arc<tokio::sync::Notify>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum StartupFaultPoint {
        Prepare,
        RootPublish,
    }

    impl SimultaneousInitializer {
        async fn enroll_peer(&self) {
            let blocks = SqliteBlockStore::open(self.peer_blocks.as_ref().unwrap()).unwrap();
            let peer = ChunkedFs::open(
                self.inner.clone(),
                blocks,
                ChunkedOptions::fixed("fault-visible-peer", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            peer.shutdown().await.unwrap();
        }
    }

    fn startup_probe(volume: &Volume) -> SimultaneousInitializer {
        SimultaneousInitializer {
            inner: volume.metadata(),
            first_read: None,
            waited: Arc::new(AtomicBool::new(false)),
            advance: None,
            startup_fault: None,
            fault_fired: Arc::new(AtomicBool::new(false)),
            peer_blocks: None,
            prepare_calls: Arc::new(AtomicU64::new(0)),
            root_publish_calls: Arc::new(AtomicU64::new(0)),
            force_flush: false,
            flush_calls: Arc::new(AtomicU64::new(0)),
            fail_flush_at: 0,
            hold_flush_at: 0,
            flush_entered: Arc::new(tokio::sync::Notify::new()),
            flush_resume: Arc::new(tokio::sync::Notify::new()),
            enrollment_calls: Arc::new(AtomicU64::new(0)),
            hold_enroll_before_call: Arc::new(AtomicBool::new(false)),
            enroll_entered: Arc::new(tokio::sync::Notify::new()),
            enroll_resume: Arc::new(tokio::sync::Notify::new()),
            hold_root_after_commit: Arc::new(AtomicBool::new(false)),
            root_entered: Arc::new(tokio::sync::Notify::new()),
            root_resume: Arc::new(tokio::sync::Notify::new()),
        }
    }

    #[async_trait]
    impl MetadataStore for SimultaneousInitializer {
        fn durable(&self) -> bool {
            self.inner.durable()
        }
        fn publish_includes_flush_barrier(&self) -> bool {
            !self.force_flush && self.inner.publish_includes_flush_barrier()
        }
        fn compact_inode_capability(&self) -> CompactInodeCapability {
            self.inner.compact_inode_capability()
        }
        async fn compact_inode_mode_state(&self) -> mount_rs_core::Result<Option<InodeModeState>> {
            let observed = self.inner.compact_inode_mode_state().await?;
            if !self.waited.swap(true, Ordering::SeqCst) {
                if let Some(barrier) = &self.first_read {
                    barrier.wait();
                }
                if let Some(fs) = &self.advance {
                    fs.mkdir("/advanced-during-inspect", Default::default())
                        .await?;
                }
            }
            Ok(observed)
        }
        async fn prepare_compact_inode_mode(
            &self,
            backing: ConcurrentBackingId,
            revision: u64,
        ) -> mount_rs_core::Result<()> {
            self.enrollment_calls.fetch_add(1, Ordering::SeqCst);
            if self.hold_enroll_before_call.swap(false, Ordering::SeqCst) {
                self.enroll_entered.notify_one();
                self.enroll_resume.notified().await;
            }
            self.inner
                .prepare_compact_inode_mode(backing, revision)
                .await
                .inspect_err(|error| eprintln!("INITIALIZER_BOUNDARY enroll: {error:?}"))
        }
        async fn load_compact_snapshot(
            &self,
            backing: ConcurrentBackingId,
        ) -> mount_rs_core::Result<CompactSnapshot> {
            self.inner
                .load_compact_snapshot(backing)
                .await
                .inspect_err(|error| eprintln!("INITIALIZER_BOUNDARY snapshot: {error:?}"))
        }
        async fn load_compact_inode(
            &self,
            backing: ConcurrentBackingId,
            inode: u64,
        ) -> mount_rs_core::Result<LoadedCompactInode> {
            self.inner.load_compact_inode(backing, inode).await
        }
        async fn publish_compact_inode(
            &self,
            backing: ConcurrentBackingId,
            inode: u64,
            generation: u64,
            expected: PhysicalInodeIdentity,
            node: NodeMetadata,
        ) -> mount_rs_core::Result<LoadedCompactInode> {
            self.inner
                .publish_compact_inode(backing, inode, generation, expected, node)
                .await
        }
        async fn publish_compact_structure(
            &self,
            delta: &CompactStructuralDelta,
        ) -> mount_rs_core::Result<CompactPublication> {
            self.inner.publish_compact_structure(delta).await
        }
        async fn load(&self) -> mount_rs_core::Result<LoadedMetadata> {
            self.inner
                .load()
                .await
                .inspect_err(|error| eprintln!("INITIALIZER_BOUNDARY load: {error:?}"))
        }
        async fn inode_mode_state(&self) -> mount_rs_core::Result<Option<InodeModeState>> {
            self.inner.inode_mode_state().await
        }
        async fn concurrent_mode_state(&self) -> mount_rs_core::Result<ConcurrentModeState> {
            self.inner.concurrent_mode_state().await
        }
        async fn preflight_new_bound_mode(&self) -> mount_rs_core::Result<()> {
            self.inner
                .preflight_new_bound_mode()
                .await
                .inspect_err(|error| eprintln!("INITIALIZER_BOUNDARY preflight: {error:?}"))
        }
        async fn prepare_bound_concurrent_mode(
            &self,
            backing: ConcurrentBackingId,
        ) -> mount_rs_core::Result<()> {
            self.prepare_calls.fetch_add(1, Ordering::SeqCst);
            if matches!(self.startup_fault, Some((StartupFaultPoint::Prepare, _)))
                && !self.fault_fired.swap(true, Ordering::SeqCst)
            {
                self.enroll_peer().await;
                return Err(mount_rs_core::FsError::new(self.startup_fault.unwrap().1));
            }
            self.inner
                .prepare_bound_concurrent_mode(backing)
                .await
                .inspect_err(|error| eprintln!("INITIALIZER_BOUNDARY prepare: {error:?}"))
        }
        async fn publish_bound_if_revision(
            &self,
            backing: ConcurrentBackingId,
            revision: u64,
            namespace: Namespace,
        ) -> mount_rs_core::Result<u64> {
            self.root_publish_calls.fetch_add(1, Ordering::SeqCst);
            if matches!(
                self.startup_fault,
                Some((StartupFaultPoint::RootPublish, _))
            ) && !self.fault_fired.swap(true, Ordering::SeqCst)
            {
                let _committed = self
                    .inner
                    .publish_bound_if_revision(backing, revision, namespace)
                    .await?;
                self.enroll_peer().await;
                return Err(mount_rs_core::FsError::new(self.startup_fault.unwrap().1));
            }
            if self.hold_root_after_commit.swap(false, Ordering::SeqCst) {
                let committed = self
                    .inner
                    .publish_bound_if_revision(backing, revision, namespace)
                    .await?;
                self.root_entered.notify_one();
                self.root_resume.notified().await;
                return Ok(committed);
            }
            self.inner
                .publish_bound_if_revision(backing, revision, namespace)
                .await
                .inspect_err(|error| eprintln!("INITIALIZER_BOUNDARY root: {error:?}"))
        }
        async fn acquire_writer(
            &self,
            owner: &str,
            ttl: Duration,
        ) -> mount_rs_core::Result<WriterLease> {
            self.inner.acquire_writer(owner, ttl).await
        }
        async fn renew_writer(
            &self,
            lease: &WriterLease,
            ttl: Duration,
        ) -> mount_rs_core::Result<WriterLease> {
            self.inner.renew_writer(lease, ttl).await
        }
        async fn release_writer(&self, lease: &WriterLease) -> mount_rs_core::Result<()> {
            self.inner.release_writer(lease).await
        }
        async fn publish(
            &self,
            revision: u64,
            lease: &WriterLease,
            namespace: Namespace,
        ) -> mount_rs_core::Result<u64> {
            self.inner.publish(revision, lease, namespace).await
        }
        async fn flush(&self) -> mount_rs_core::Result<()> {
            let call = self.flush_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == self.fail_flush_at {
                return Err(mount_rs_core::FsError::new(ErrorCode::Eio)
                    .with_message("injected startup required flush failure"));
            }
            if call == self.hold_flush_at {
                self.flush_entered.notify_one();
                self.flush_resume.notified().await;
            }
            self.inner.flush().await
        }
    }

    #[test]
    fn compact_root_enrollment_selected_write_full_rename_and_reopen_keep_bytes() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let first = volume.open("compact-first").await;
            let mode = volume
                .metadata()
                .compact_inode_mode_state()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(mode.structural_generation, 2);
            first.write_file("/file", b"first").await.unwrap();
            let handle = first.open("/file", "r+", 0).await.unwrap();
            handle.write(b"SECOND", Some(0)).await.unwrap();
            handle.close().await.unwrap();
            first.rename("/file", "/renamed").await.unwrap();
            first.shutdown().await.unwrap();

            let reopened = volume.open("compact-reopen").await;
            let file = reopened.open("/renamed", "r", 0).await.unwrap();
            let mut bytes = [0; 6];
            assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), 6);
            assert_eq!(&bytes, b"SECOND");
            assert_eq!(file.read(&mut bytes, Some(6)).await.unwrap(), 0);
            file.close().await.unwrap();
            reopened.shutdown().await.unwrap();
        });
    }

    #[test]
    fn established_compact_with_missing_marker_fails_without_repair() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            volume.open("enroll").await.shutdown().await.unwrap();
            let mode = volume
                .metadata()
                .compact_inode_mode_state()
                .await
                .unwrap()
                .unwrap();
            let missing = volume.0.join("missing-block-marker.db");
            let error = match ChunkedFs::open(
                volume.metadata(),
                SqliteBlockStore::open(&missing).unwrap(),
                ChunkedOptions::fixed("bad-marker", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            {
                Ok(_) => panic!("missing marker was repaired"),
                Err(error) => error,
            };
            assert!(matches!(error.code, ErrorCode::Estale | ErrorCode::Eio));
            assert_eq!(
                volume
                    .metadata()
                    .compact_inode_mode_state()
                    .await
                    .unwrap()
                    .unwrap(),
                mode
            );
            assert!(
                mount_rs_core::storage::BlockStore::verify_concurrent_backing(
                    &SqliteBlockStore::open(&missing).unwrap(),
                    mode.backing
                )
                .await
                .is_err()
            );
        });
    }

    #[test]
    fn root_only_mrc2_enrolls_on_same_backing_and_reopens_compact() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let mrc2 = ChunkedFs::open(
                volume.metadata(),
                volume.blocks(),
                ChunkedOptions::fixed("mrc2-root", 16)
                    .unwrap()
                    .with_concurrent_writes(true),
            )
            .await
            .unwrap();
            mrc2.shutdown().await.unwrap();
            let backing = match volume.metadata().concurrent_mode_state().await.unwrap() {
                ConcurrentModeState::Mrc2(backing) => backing,
                other => panic!("expected MRC2 backing, found {other:?}"),
            };
            let compact = volume.open("mrc2-compact").await;
            let mode = volume
                .metadata()
                .compact_inode_mode_state()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(mode.backing, backing);
            compact.shutdown().await.unwrap();
            volume.open("mrc2-reopen").await.shutdown().await.unwrap();
        });
    }

    #[test]
    fn peer_enrollment_after_root_capture_is_known_noncommit() {
        futures_lite::future::block_on(futures_lite::future::race(
            async {
                let volume = Volume::new();
                let probe = startup_probe(&volume);
                probe.hold_enroll_before_call.store(true, Ordering::SeqCst);
                let initial = ChunkedFs::open(
                    probe.clone(),
                    volume.blocks(),
                    ChunkedOptions::fixed("paused-enrollment", 16)
                        .unwrap()
                        .with_compact_inode_updates(true),
                );
                let peer = async {
                    probe.enroll_entered.notified().await;
                    let winner = volume.open("enrollment-winner").await;
                    assert!(winner.stat("/").await.is_ok());
                    winner.shutdown().await.unwrap();
                    let metadata = volume.metadata();
                    let mode = metadata.compact_inode_mode_state().await.unwrap().unwrap();
                    let snapshot = metadata.load_compact_snapshot(mode.backing).await.unwrap();
                    let raw = (
                        std::fs::read(volume.0.join("metadata.db")).unwrap(),
                        std::fs::read(volume.0.join("metadata.db-wal")).ok(),
                    );
                    probe.enroll_resume.notify_one();
                    (mode, snapshot, raw)
                };
                let (result, (mode, snapshot, raw)) =
                    futures_lite::future::zip(initial, peer).await;
                let metadata = volume.metadata();
                assert_eq!(
                    metadata.compact_inode_mode_state().await.unwrap(),
                    Some(mode)
                );
                assert_eq!(
                    metadata.load_compact_snapshot(mode.backing).await.unwrap(),
                    snapshot
                );
                assert_eq!(std::fs::read(volume.0.join("metadata.db")).unwrap(), raw.0);
                assert_eq!(std::fs::read(volume.0.join("metadata.db-wal")).ok(), raw.1);
                println!(
                    "ENROLLMENT_RACE result={:?} coherent_authority_unchanged=1 raw_database_wal_unchanged=1",
                    result.as_ref().err()
                );
                let fresh = volume.open("enrollment-fresh").await;
                assert_eq!(fresh.stat("/").await.unwrap().ino, snapshot.anchor.root);
                fresh.shutdown().await.unwrap();
                // Enrollment lost to a same-backing peer before any provider DML.
                let serving = result.unwrap();
                assert_eq!(serving.stat("/").await.unwrap().ino, snapshot.anchor.root);
                assert_eq!(probe.enrollment_calls.load(Ordering::SeqCst), 1);
                assert_eq!(probe.prepare_calls.load(Ordering::SeqCst), 1);
                assert_eq!(probe.root_publish_calls.load(Ordering::SeqCst), 1);
                serving.shutdown().await.unwrap();
            },
            async {
                async_io::Timer::after(Duration::from_secs(10)).await;
                panic!("enrollment boundary diagnostic timed out");
            },
        ));
    }

    #[test]
    fn simultaneous_initializers_enroll_one_root_and_one_mode() {
        let volume = Volume::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for id in 0..2 {
            let metadata = SimultaneousInitializer {
                inner: volume.metadata(),
                first_read: Some(barrier.clone()),
                waited: Arc::new(AtomicBool::new(false)),
                advance: None,
                startup_fault: None,
                fault_fired: Arc::new(AtomicBool::new(false)),
                peer_blocks: None,
                prepare_calls: Arc::new(AtomicU64::new(0)),
                root_publish_calls: Arc::new(AtomicU64::new(0)),
                force_flush: false,
                flush_calls: Arc::new(AtomicU64::new(0)),
                fail_flush_at: 0,
                hold_flush_at: 0,
                flush_entered: Arc::new(tokio::sync::Notify::new()),
                flush_resume: Arc::new(tokio::sync::Notify::new()),
                enrollment_calls: Arc::new(AtomicU64::new(0)),
                hold_enroll_before_call: Arc::new(AtomicBool::new(false)),
                enroll_entered: Arc::new(tokio::sync::Notify::new()),
                enroll_resume: Arc::new(tokio::sync::Notify::new()),
                hold_root_after_commit: Arc::new(AtomicBool::new(false)),
                root_entered: Arc::new(tokio::sync::Notify::new()),
                root_resume: Arc::new(tokio::sync::Notify::new()),
            };
            let blocks = volume.blocks();
            workers.push(std::thread::spawn(move || {
                futures_lite::future::block_on(async {
                    let fs = ChunkedFs::open(
                        metadata,
                        blocks,
                        ChunkedOptions::fixed(format!("initializer-{id}"), 16)
                            .unwrap()
                            .with_compact_inode_updates(true),
                    )
                    .await?;
                    fs.shutdown().await
                })
            }));
        }
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        futures_lite::future::block_on(async {
            let mode = volume
                .metadata()
                .compact_inode_mode_state()
                .await
                .unwrap()
                .unwrap();
            let snapshot = volume
                .metadata()
                .load_compact_snapshot(mode.backing)
                .await
                .unwrap();
            assert_eq!(snapshot.anchor.members.len(), 1);
            assert_eq!(snapshot.anchor.generation, mode.structural_generation);
            volume
                .open("after-two-initializers")
                .await
                .shutdown()
                .await
                .unwrap();
        });
    }

    #[test]
    fn startup_mutation_errors_remain_terminal_even_when_peer_mrc5_is_visible() {
        futures_lite::future::block_on(async {
            for point in [StartupFaultPoint::Prepare, StartupFaultPoint::RootPublish] {
                for code in [ErrorCode::Estale, ErrorCode::Enotsup] {
                    if point == StartupFaultPoint::RootPublish && code == ErrorCode::Enotsup {
                        continue;
                    }
                    let volume = Volume::new();
                    let metadata = SimultaneousInitializer {
                        inner: volume.metadata(),
                        first_read: None,
                        waited: Arc::new(AtomicBool::new(false)),
                        advance: None,
                        startup_fault: Some((point, code)),
                        fault_fired: Arc::new(AtomicBool::new(false)),
                        peer_blocks: Some(volume.0.join("blocks.db")),
                        prepare_calls: Arc::new(AtomicU64::new(0)),
                        root_publish_calls: Arc::new(AtomicU64::new(0)),
                        force_flush: false,
                        flush_calls: Arc::new(AtomicU64::new(0)),
                        fail_flush_at: 0,
                        hold_flush_at: 0,
                        flush_entered: Arc::new(tokio::sync::Notify::new()),
                        flush_resume: Arc::new(tokio::sync::Notify::new()),
                        enrollment_calls: Arc::new(AtomicU64::new(0)),
                        hold_enroll_before_call: Arc::new(AtomicBool::new(false)),
                        enroll_entered: Arc::new(tokio::sync::Notify::new()),
                        enroll_resume: Arc::new(tokio::sync::Notify::new()),
                        hold_root_after_commit: Arc::new(AtomicBool::new(false)),
                        root_entered: Arc::new(tokio::sync::Notify::new()),
                        root_resume: Arc::new(tokio::sync::Notify::new()),
                    };
                    let error = match ChunkedFs::open(
                        metadata.clone(),
                        volume.blocks(),
                        ChunkedOptions::fixed("fault-startup", 16)
                            .unwrap()
                            .with_compact_inode_updates(true),
                    )
                    .await
                    {
                        Ok(_) => panic!("startup mutation error was suppressed by peer MRC5"),
                        Err(error) => error,
                    };
                    assert_eq!(error.code, code);
                    assert!(metadata.fault_fired.load(Ordering::SeqCst));
                    assert!(
                        volume
                            .metadata()
                            .compact_inode_mode_state()
                            .await
                            .unwrap()
                            .is_some()
                    );
                    assert_eq!(metadata.prepare_calls.load(Ordering::SeqCst), 1);
                    assert_eq!(
                        metadata.root_publish_calls.load(Ordering::SeqCst),
                        u64::from(point == StartupFaultPoint::RootPublish)
                    );
                    let oracle = volume.open("startup-error-oracle").await;
                    assert_eq!(oracle.stat("/").await.unwrap().nlink, 2);
                    oracle.shutdown().await.unwrap();
                }
            }
        });
    }

    #[test]
    fn startup_required_flush_failures_return_error_with_fresh_durable_oracle() {
        futures_lite::future::block_on(async {
            for fail_at in [1, 2] {
                let volume = Volume::new();
                let mut metadata = startup_probe(&volume);
                metadata.force_flush = true;
                metadata.fail_flush_at = fail_at;
                let error = match ChunkedFs::open(
                    metadata.clone(),
                    volume.blocks(),
                    ChunkedOptions::fixed("startup-flush-fault", 16)
                        .unwrap()
                        .with_compact_inode_updates(true),
                )
                .await
                {
                    Ok(_) => panic!("failed startup flush returned a serving filesystem"),
                    Err(error) => error,
                };
                assert_eq!(error.code, ErrorCode::Eio);
                assert_eq!(metadata.flush_calls.load(Ordering::SeqCst), fail_at);
                assert_eq!(metadata.root_publish_calls.load(Ordering::SeqCst), 1);
                assert_eq!(
                    volume
                        .metadata()
                        .compact_inode_mode_state()
                        .await
                        .unwrap()
                        .is_some(),
                    fail_at == 2
                );
                let oracle = volume.open("startup-flush-oracle").await;
                oracle.stat("/").await.unwrap();
                oracle.shutdown().await.unwrap();
            }
        });
    }

    #[test]
    fn cancellation_during_startup_publication_or_required_flush_has_no_escaped_owner() {
        futures_lite::future::block_on(async {
            for hold_flush in [false, true] {
                let volume = Volume::new();
                let mut metadata = startup_probe(&volume);
                if hold_flush {
                    metadata.force_flush = true;
                    metadata.hold_flush_at = 2;
                } else {
                    metadata
                        .hold_root_after_commit
                        .store(true, Ordering::SeqCst);
                }
                let entered = if hold_flush {
                    metadata.flush_entered.clone()
                } else {
                    metadata.root_entered.clone()
                };
                futures_lite::future::race(
                    async {
                        let result = ChunkedFs::open(
                            metadata.clone(),
                            volume.blocks(),
                            ChunkedOptions::fixed("startup-cancel", 16)
                                .unwrap()
                                .with_compact_inode_updates(true),
                        )
                        .await;
                        panic!("startup returned before cancellation: {:?}", result.err());
                    },
                    async {
                        entered.notified().await;
                    },
                )
                .await;
                assert_eq!(metadata.root_publish_calls.load(Ordering::SeqCst), 1);
                assert_eq!(
                    metadata.flush_calls.load(Ordering::SeqCst),
                    if hold_flush { 2 } else { 0 }
                );
                assert_eq!(
                    volume
                        .metadata()
                        .compact_inode_mode_state()
                        .await
                        .unwrap()
                        .is_some(),
                    hold_flush
                );
                let oracle = volume.open("startup-cancel-oracle").await;
                oracle.stat("/").await.unwrap();
                oracle.shutdown().await.unwrap();
                assert_eq!(metadata.root_publish_calls.load(Ordering::SeqCst), 1);
            }
        });
    }

    #[test]
    fn structural_advance_between_discovery_and_snapshot_restarts_open() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let advancing = volume.open("advancing").await;
            let metadata = SimultaneousInitializer {
                inner: volume.metadata(),
                first_read: None,
                waited: Arc::new(AtomicBool::new(false)),
                advance: Some(advancing.clone()),
                startup_fault: None,
                fault_fired: Arc::new(AtomicBool::new(false)),
                peer_blocks: None,
                prepare_calls: Arc::new(AtomicU64::new(0)),
                root_publish_calls: Arc::new(AtomicU64::new(0)),
                force_flush: false,
                flush_calls: Arc::new(AtomicU64::new(0)),
                fail_flush_at: 0,
                hold_flush_at: 0,
                flush_entered: Arc::new(tokio::sync::Notify::new()),
                flush_resume: Arc::new(tokio::sync::Notify::new()),
                enrollment_calls: Arc::new(AtomicU64::new(0)),
                hold_enroll_before_call: Arc::new(AtomicBool::new(false)),
                enroll_entered: Arc::new(tokio::sync::Notify::new()),
                enroll_resume: Arc::new(tokio::sync::Notify::new()),
                hold_root_after_commit: Arc::new(AtomicBool::new(false)),
                root_entered: Arc::new(tokio::sync::Notify::new()),
                root_resume: Arc::new(tokio::sync::Notify::new()),
            };
            let opened = ChunkedFs::open(
                metadata,
                volume.blocks(),
                ChunkedOptions::fixed("raced-open", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            opened.stat("/advanced-during-inspect").await.unwrap();
            opened.shutdown().await.unwrap();
            advancing.shutdown().await.unwrap();
        });
    }

    #[test]
    fn populated_mrc2_and_mrc4_require_offline_migration() {
        futures_lite::future::block_on(async {
            for inode_updates in [false, true] {
                let volume = Volume::new();
                let old = ChunkedFs::open(
                    volume.metadata(),
                    volume.blocks(),
                    ChunkedOptions::fixed("old", 16)
                        .unwrap()
                        .with_concurrent_writes(true)
                        .with_inode_updates(inode_updates),
                )
                .await
                .unwrap();
                old.write_file("/existing", b"preserve").await.unwrap();
                old.shutdown().await.unwrap();
                let original_mode = volume.metadata().inode_mode_state().await.unwrap();
                let original_backing = if inode_updates {
                    None
                } else {
                    Some(volume.metadata().concurrent_mode_state().await.unwrap())
                };
                let error = match ChunkedFs::open(
                    volume.metadata(),
                    volume.blocks(),
                    ChunkedOptions::fixed("attempt-compact", 16)
                        .unwrap()
                        .with_compact_inode_updates(true),
                )
                .await
                {
                    Ok(_) => panic!("populated prior mode was enrolled"),
                    Err(error) => error,
                };
                assert_eq!(error.code, ErrorCode::Ebusy);
                assert_eq!(
                    volume.metadata().inode_mode_state().await.unwrap(),
                    original_mode
                );
                if let Some(original_backing) = original_backing {
                    assert_eq!(
                        volume.metadata().concurrent_mode_state().await.unwrap(),
                        original_backing
                    );
                }
                assert!(
                    volume
                        .metadata()
                        .compact_inode_mode_state()
                        .await
                        .unwrap()
                        .is_none()
                );
                let reopened = ChunkedFs::open(
                    volume.metadata(),
                    volume.blocks(),
                    ChunkedOptions::fixed("old-reopen", 16)
                        .unwrap()
                        .with_concurrent_writes(true)
                        .with_inode_updates(inode_updates),
                )
                .await
                .unwrap();
                let file = reopened.open("/existing", "r", 0).await.unwrap();
                let mut bytes = [0; 8];
                assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), 8);
                assert_eq!(&bytes, b"preserve");
                file.close().await.unwrap();
                reopened.shutdown().await.unwrap();
            }
        });
    }

    #[test]
    fn compact_open_truncate_uses_fresh_selected_identity() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let fs = volume.open("truncate-selected").await;
            fs.write_file("/file", b"old bytes").await.unwrap();
            let file = fs.open("/file", "w", 0).await.unwrap();
            assert_eq!(file.stat().await.unwrap().size, 0);
            let mut byte = [0];
            assert_eq!(
                file.read(&mut byte, Some(0)).await.unwrap_err().code,
                ErrorCode::Ebadf
            );
            file.close().await.unwrap();
            let reader = fs.open("/file", "r", 0).await.unwrap();
            assert_eq!(reader.read(&mut byte, Some(0)).await.unwrap(), 0);
            reader.close().await.unwrap();
            fs.shutdown().await.unwrap();
        });
    }

    #[test]
    fn malformed_selected_load_fails_bounded_and_poisoned_owner() {
        let volume = Volume::new();
        futures_lite::future::block_on(async {
            volume.open("load-enroll").await.shutdown().await.unwrap();
        });
        let recorder = RecordingMetadata::new(volume.metadata());
        let fs = futures_lite::future::block_on(ChunkedFs::open(
            recorder.clone(),
            volume.blocks(),
            ChunkedOptions::fixed("bad-load", 16)
                .unwrap()
                .with_compact_inode_updates(true),
        ))
        .unwrap();
        futures_lite::future::block_on(fs.write_file("/file", b"good")).unwrap();
        recorder.corrupt_selected_load.store(true, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let first = futures_lite::future::block_on(fs.stat("/file"));
            let second = futures_lite::future::block_on(fs.stat("/file"));
            tx.send((first.map(|_| ()), second.map(|_| ()))).unwrap();
        });
        let (first, second) = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("malformed selected load must fail without deadlocking state");
        assert_eq!(first.unwrap_err().code, ErrorCode::Einval);
        assert_eq!(second.unwrap_err().code, ErrorCode::Einval);
        worker.join().unwrap();
        assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn returned_compact_filesystem_never_calls_legacy_or_mrc4_metadata_methods() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            volume
                .open("enroll-recorded")
                .await
                .shutdown()
                .await
                .unwrap();
            let recorder = RecordingMetadata::new(volume.metadata());
            let fs = ChunkedFs::open(
                recorder.clone(),
                volume.blocks(),
                ChunkedOptions::fixed("recorded", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            fs.write_file("/file", b"first").await.unwrap();
            let file = fs.open("/file", "r+", 0).await.unwrap();
            file.write(b"after", Some(0)).await.unwrap();
            file.close().await.unwrap();
            fs.rename("/file", "/renamed").await.unwrap();
            assert_eq!(fs.stat("/renamed").await.unwrap().size, 5);
            fs.shutdown().await.unwrap();
            assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
            assert!(recorder.selected_calls.load(Ordering::SeqCst) > 0);
            assert!(recorder.full_calls.load(Ordering::SeqCst) > 0);
        });
    }

    #[test]
    fn selected_write_after_structure_change_uses_physical_epoch_not_projected_zero() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let a = volume.open("epoch-a").await;
            a.write_file("/a", b"before").await.unwrap();
            let b = volume.open("epoch-b").await;
            let metadata = volume.metadata();
            let backing = metadata
                .compact_inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .backing;
            let before = metadata.load_compact_snapshot(backing).await.unwrap();
            let inode = before
                .anchor
                .members
                .iter()
                .copied()
                .find(|id| *id != before.anchor.root)
                .unwrap();
            let old = before.guards[&inode].identity;
            a.write_file("/b", b"other").await.unwrap();
            let after_create = metadata.load_compact_snapshot(backing).await.unwrap();
            assert_eq!(after_create.guards[&inode].identity, old);
            assert_eq!(
                old.logical_version(after_create.anchor.generation)
                    .unwrap()
                    .inode_revision,
                0
            );
            let handle = b.open("/a", "r+", 0).await.unwrap();
            handle.write(b"AFTER!", Some(0)).await.unwrap();
            handle.close().await.unwrap();
            let after_write = metadata.load_compact_snapshot(backing).await.unwrap();
            assert_eq!(
                after_write.guards[&inode].identity.incarnation,
                old.incarnation
            );
            assert_eq!(
                after_write.guards[&inode].identity.epoch,
                after_create.anchor.generation
            );
            assert_eq!(after_write.guards[&inode].identity.revision, 1);
            let reader = a.open("/a", "r", 0).await.unwrap();
            let mut bytes = [0; 6];
            assert_eq!(reader.read(&mut bytes, Some(0)).await.unwrap(), 6);
            assert_eq!(&bytes, b"AFTER!");
            reader.close().await.unwrap();
            a.shutdown().await.unwrap();
            b.shutdown().await.unwrap();
        });
    }

    #[test]
    fn full_conflict_with_unrelated_selected_write_recaptures_without_losing_bytes() {
        let volume = Volume::new();
        futures_lite::future::block_on(async {
            let setup = volume.open("full-setup").await;
            setup.write_file("/a", b"before").await.unwrap();
            setup.shutdown().await.unwrap();
        });
        let recorder = RecordingMetadata::new(volume.metadata());
        recorder.hold_full_once.store(true, Ordering::SeqCst);
        let mutator = futures_lite::future::block_on(ChunkedFs::open(
            recorder.clone(),
            volume.blocks(),
            ChunkedOptions::fixed("full-mutator", 16)
                .unwrap()
                .with_compact_inode_updates(true),
        ))
        .unwrap();
        let peer = futures_lite::future::block_on(volume.open("selected-peer"));
        let worker = std::thread::spawn(move || {
            futures_lite::future::block_on(async move {
                mutator.rename("/a", "/renamed").await.unwrap();
                mutator.shutdown().await.unwrap();
            })
        });
        futures_lite::future::block_on(async {
            recorder.full_entered.notified().await;
            let handle = peer.open("/a", "r+", 0).await.unwrap();
            handle.write(b"AFTER!", Some(0)).await.unwrap();
            handle.close().await.unwrap();
            recorder.full_resume.notify_one();
            peer.shutdown().await.unwrap();
        });
        worker.join().unwrap();
        futures_lite::future::block_on(async {
            let reader = volume.open("full-oracle").await;
            let file = reader.open("/renamed", "r", 0).await.unwrap();
            let mut bytes = [0; 6];
            assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), 6);
            assert_eq!(&bytes, b"AFTER!");
            file.close().await.unwrap();
            reader.shutdown().await.unwrap();
        });
        assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
        assert!(recorder.full_calls.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn lost_selected_ack_fails_closed_without_replay_but_fresh_oracle_sees_commit() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let setup = volume.open("lost-ack-setup").await;
            setup.write_file("/file", b"before").await.unwrap();
            setup.shutdown().await.unwrap();
            let recorder = RecordingMetadata::new(volume.metadata());
            let fs = ChunkedFs::open(
                recorder.clone(),
                volume.blocks(),
                ChunkedOptions::fixed("lost-ack", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            let handle = fs.open("/file", "r+", 0).await.unwrap();
            recorder.lose_selected_ack.store(true, Ordering::SeqCst);
            assert_eq!(
                handle.write(b"AFTER!", Some(0)).await.unwrap_err().code,
                ErrorCode::Eio
            );
            assert!(fs.failed());
            assert_eq!(recorder.selected_calls.load(Ordering::SeqCst), 1);
            assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
            let fresh = volume.open("lost-ack-oracle").await;
            let file = fresh.open("/file", "r", 0).await.unwrap();
            let mut bytes = [0; 6];
            assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), 6);
            assert_eq!(&bytes, b"AFTER!");
            file.close().await.unwrap();
            fresh.shutdown().await.unwrap();
        });
    }

    #[test]
    fn malformed_full_receipt_poisoned_owner_does_not_replay_committed_rename() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let setup = volume.open("receipt-setup").await;
            setup.write_file("/file", b"payload").await.unwrap();
            setup.shutdown().await.unwrap();
            let recorder = RecordingMetadata::new(volume.metadata());
            let fs = ChunkedFs::open(
                recorder.clone(),
                volume.blocks(),
                ChunkedOptions::fixed("receipt-owner", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            recorder.corrupt_full_receipt.store(true, Ordering::SeqCst);
            assert!(fs.rename("/file", "/renamed").await.is_err());
            assert!(fs.failed());
            assert_eq!(recorder.full_calls.load(Ordering::SeqCst), 1);
            assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
            let oracle = volume.open("receipt-oracle").await;
            assert_eq!(
                oracle.stat("/file").await.unwrap_err().code,
                ErrorCode::Enoent
            );
            assert_eq!(oracle.stat("/renamed").await.unwrap().size, 7);
            oracle.shutdown().await.unwrap();
        });
    }

    #[test]
    fn malformed_selected_receipt_poisoned_owner_does_not_replay_committed_write() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let setup = volume.open("selected-receipt-setup").await;
            setup.write_file("/file", b"before").await.unwrap();
            setup.shutdown().await.unwrap();
            let recorder = RecordingMetadata::new(volume.metadata());
            let fs = ChunkedFs::open(
                recorder.clone(),
                volume.blocks(),
                ChunkedOptions::fixed("selected-receipt-owner", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            let file = fs.open("/file", "r+", 0).await.unwrap();
            recorder
                .corrupt_selected_receipt
                .store(true, Ordering::SeqCst);
            assert!(file.write(b"AFTER!", Some(0)).await.is_err());
            assert!(fs.failed());
            assert_eq!(recorder.selected_calls.load(Ordering::SeqCst), 1);
            let oracle = volume.open("selected-receipt-oracle").await;
            let reader = oracle.open("/file", "r", 0).await.unwrap();
            let mut bytes = [0; 6];
            reader.read(&mut bytes, Some(0)).await.unwrap();
            assert_eq!(&bytes, b"AFTER!");
            reader.close().await.unwrap();
            oracle.shutdown().await.unwrap();
        });
    }

    #[test]
    fn required_metadata_flush_failure_after_selected_commit_fails_closed() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let setup = volume.open("flush-setup").await;
            setup.write_file("/file", b"before").await.unwrap();
            setup.shutdown().await.unwrap();
            let recorder = RecordingMetadata::new(volume.metadata());
            let fs = ChunkedFs::open(
                recorder.clone(),
                volume.blocks(),
                ChunkedOptions::fixed("flush-owner", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            let file = fs.open("/file", "r+", 0).await.unwrap();
            recorder.force_flush.store(true, Ordering::SeqCst);
            recorder.fail_flush_once.store(true, Ordering::SeqCst);
            assert_eq!(
                file.write(b"AFTER!", Some(0)).await.unwrap_err().code,
                ErrorCode::Eio
            );
            assert!(fs.failed());
            assert_eq!(recorder.selected_calls.load(Ordering::SeqCst), 1);
            let oracle = volume.open("flush-oracle").await;
            let reader = oracle.open("/file", "r", 0).await.unwrap();
            let mut bytes = [0; 6];
            reader.read(&mut bytes, Some(0)).await.unwrap();
            assert_eq!(&bytes, b"AFTER!");
            reader.close().await.unwrap();
            oracle.shutdown().await.unwrap();
        });
    }

    #[test]
    fn cancellation_while_selected_publication_is_pending_poisoned_owner_without_replay() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let setup = volume.open("cancel-setup").await;
            setup.write_file("/file", b"before").await.unwrap();
            setup.shutdown().await.unwrap();
            let recorder = RecordingMetadata::new(volume.metadata());
            let fs = ChunkedFs::open(
                recorder.clone(),
                volume.blocks(),
                ChunkedOptions::fixed("cancel-owner", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            let file = fs.open("/file", "r+", 0).await.unwrap();
            recorder.hold_selected_once.store(true, Ordering::SeqCst);
            futures_lite::future::race(
                async { file.write(b"AFTER!", Some(0)).await.map(|_| ()) },
                async {
                    recorder.selected_entered.notified().await;
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert!(fs.failed());
            assert_eq!(recorder.selected_calls.load(Ordering::SeqCst), 1);
            assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
            let oracle = volume.open("cancel-oracle").await;
            let reader = oracle.open("/file", "r", 0).await.unwrap();
            let mut bytes = [0; 6];
            reader.read(&mut bytes, Some(0)).await.unwrap();
            assert_eq!(&bytes, b"before");
            reader.close().await.unwrap();
            oracle.shutdown().await.unwrap();
        });
    }

    #[test]
    fn compact_full_operations_preserve_links_moves_and_open_unlinked_bytes() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let fs = volume.open("full-semantics").await;
            fs.mkdir("/left", Default::default()).await.unwrap();
            fs.mkdir("/right", Default::default()).await.unwrap();
            fs.write_file("/left/file", b"initial").await.unwrap();
            fs.link("/left/file", "/right/hard").await.unwrap();
            assert_eq!(fs.stat("/right/hard").await.unwrap().nlink, 2);
            fs.rename("/left/file", "/right/moved").await.unwrap();
            fs.unlink("/right/hard").await.unwrap();
            assert_eq!(fs.stat("/right/moved").await.unwrap().nlink, 1);
            fs.symlink("/right/moved", "/left/link").await.unwrap();
            let handle = fs.open("/right/moved", "r+", 0).await.unwrap();
            handle.write(b"LATEST!", Some(0)).await.unwrap();
            fs.unlink("/right/moved").await.unwrap();
            let mut bytes = [0; 7];
            assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 7);
            assert_eq!(&bytes, b"LATEST!");
            handle.write(b"ORPHAN!", Some(0)).await.unwrap();
            assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 7);
            assert_eq!(&bytes, b"ORPHAN!");
            handle.close().await.unwrap();
            assert_eq!(
                fs.stat("/right/moved").await.unwrap_err().code,
                ErrorCode::Enoent
            );
            fs.unlink("/left/link").await.unwrap();
            fs.rmdir("/left").await.unwrap();
            fs.rmdir("/right").await.unwrap();
            fs.shutdown().await.unwrap();
            let reopened = volume.open("full-semantics-reopen").await;
            assert_eq!(
                reopened.stat("/right/moved").await.unwrap_err().code,
                ErrorCode::Enoent
            );
            reopened.shutdown().await.unwrap();
        });
    }

    #[test]
    fn selected_read_waiting_to_install_cannot_regress_later_full_publication() {
        let volume = Volume::new();
        futures_lite::future::block_on(async {
            let setup = volume.open("gate-setup").await;
            setup.write_file("/file", b"payload").await.unwrap();
            setup.shutdown().await.unwrap();
        });
        let recorder = RecordingMetadata::new(volume.metadata());
        let fs = futures_lite::future::block_on(ChunkedFs::open(
            recorder.clone(),
            volume.blocks(),
            ChunkedOptions::fixed("gate-owner", 16)
                .unwrap()
                .with_compact_inode_updates(true),
        ))
        .unwrap();
        recorder.hold_load_once.store(true, Ordering::SeqCst);
        let reader_fs = fs.clone();
        let reader = std::thread::spawn(move || {
            futures_lite::future::block_on(async { reader_fs.stat("/file").await.unwrap() })
        });
        futures_lite::future::block_on(recorder.load_entered.notified());
        let writer_fs = fs.clone();
        let writer = std::thread::spawn(move || {
            futures_lite::future::block_on(async {
                writer_fs.rename("/file", "/renamed").await.unwrap();
            })
        });
        recorder.load_resume.notify_one();
        assert_eq!(reader.join().unwrap().size, 7);
        writer.join().unwrap();
        futures_lite::future::block_on(async {
            assert_eq!(fs.stat("/renamed").await.unwrap().size, 7);
            fs.shutdown().await.unwrap();
        });
        assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn compact_statfs_reads_coherent_allocation_after_selected_and_peer_full_changes() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let first = volume.open("statfs-first").await;
            let root = first.statfs("/").await.unwrap();
            assert_eq!(root.files - root.files_free, 1);
            let initial_used_blocks = root.blocks - root.blocks_free;
            first.write_file("/file", b"a").await.unwrap();
            let peer = volume.open("statfs-peer").await;
            let file = first.open("/file", "r+", 0).await.unwrap();
            file.write(&vec![b'x'; 4097], Some(0)).await.unwrap();
            file.close().await.unwrap();
            let selected = first.statfs("/file").await.unwrap();
            assert_eq!(selected.files - selected.files_free, 2);
            assert_eq!(
                selected.blocks - selected.blocks_free,
                initial_used_blocks + 2
            );
            peer.write_file("/other", b"z").await.unwrap();
            let changed = first.statfs("/").await.unwrap();
            assert_eq!(changed.files - changed.files_free, 3);
            assert_eq!(
                changed.blocks - changed.blocks_free,
                initial_used_blocks + 3
            );
            peer.shutdown().await.unwrap();
            first.shutdown().await.unwrap();
        });
    }

    #[test]
    fn foreign_backing_full_refresh_poisoned_before_install_or_publication() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let setup = volume.open("foreign-setup").await;
            setup.write_file("/file", b"body").await.unwrap();
            setup.shutdown().await.unwrap();
            let recorder = RecordingMetadata::new(volume.metadata());
            let fs = ChunkedFs::open(
                recorder.clone(),
                volume.blocks(),
                ChunkedOptions::fixed("foreign-refresh", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            let mode_before = volume.metadata().compact_inode_mode_state().await.unwrap();
            recorder.foreign_snapshot_once.store(true, Ordering::SeqCst);
            assert!(fs.statfs("/").await.is_err());
            assert!(fs.failed());
            assert!(fs.stat("/file").await.is_err());
            assert_eq!(recorder.full_calls.load(Ordering::SeqCst), 0);
            assert_eq!(recorder.selected_calls.load(Ordering::SeqCst), 0);
            assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                volume.metadata().compact_inode_mode_state().await.unwrap(),
                mode_before
            );
        });
    }

    #[test]
    fn foreign_backing_full_capture_poisoned_before_publication() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let setup = volume.open("foreign-capture-setup").await;
            setup.write_file("/file", b"body").await.unwrap();
            setup.shutdown().await.unwrap();
            let recorder = RecordingMetadata::new(volume.metadata());
            let fs = ChunkedFs::open(
                recorder.clone(),
                volume.blocks(),
                ChunkedOptions::fixed("foreign-capture", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            recorder.foreign_snapshot_once.store(true, Ordering::SeqCst);
            assert!(fs.rename("/file", "/renamed").await.is_err());
            assert!(fs.failed());
            assert_eq!(recorder.full_calls.load(Ordering::SeqCst), 0);
            assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
            let oracle = volume.open("foreign-capture-oracle").await;
            assert_eq!(oracle.stat("/file").await.unwrap().size, 4);
            assert_eq!(
                oracle.stat("/renamed").await.unwrap_err().code,
                ErrorCode::Enoent
            );
            oracle.shutdown().await.unwrap();
        });
    }

    #[test]
    fn backward_full_generation_with_unchanged_local_revision_poisoned() {
        futures_lite::future::block_on(async {
            let volume = Volume::new();
            let setup = volume.open("backward-setup").await;
            setup.write_file("/file", b"body").await.unwrap();
            let metadata = volume.metadata();
            let backing = metadata
                .compact_inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .backing;
            let older = metadata.load_compact_snapshot(backing).await.unwrap();
            setup.write_file("/newer", b"later").await.unwrap();
            setup.shutdown().await.unwrap();
            let recorder = RecordingMetadata::new(volume.metadata());
            let fs = ChunkedFs::open(
                recorder.clone(),
                volume.blocks(),
                ChunkedOptions::fixed("backward-owner", 16)
                    .unwrap()
                    .with_compact_inode_updates(true),
            )
            .await
            .unwrap();
            *recorder.backward_snapshot_once.lock().unwrap() = Some(older);
            assert!(fs.statfs("/").await.is_err());
            assert!(fs.failed());
            assert_eq!(recorder.full_calls.load(Ordering::SeqCst), 0);
            assert_eq!(recorder.old_calls.load(Ordering::SeqCst), 0);
        });
    }
}

#[test]
fn explicit_compact_selection_rejects_unsupported_store_before_mutation() {
    futures_lite::future::block_on(async {
        let metadata = MemoryMetadataStore::new();
        let options = ChunkedOptions::fixed("compact-unsupported", 4096)
            .unwrap()
            .with_compact_inode_updates(true);
        let error = match ChunkedFs::open(metadata.clone(), MemoryBlockStore::new(), options).await
        {
            Ok(_) => panic!("unsupported compact provider opened"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::Enotsup);
        let loaded = mount_rs_core::storage::MetadataStore::load(&metadata)
            .await
            .unwrap();
        assert_eq!(loaded.revision, 0);
        assert!(loaded.namespace.is_none());
    });
}

#[test]
fn compact_option_contradictions_fail_before_metadata_mutation() {
    futures_lite::future::block_on(async {
        for contradiction in 0..5 {
            let metadata = MemoryMetadataStore::new();
            let mut options = ChunkedOptions::fixed("compact-contradiction", 4096)
                .unwrap()
                .with_compact_inode_updates(true);
            match contradiction {
                0 => options.inode_updates = false,
                1 => options.concurrent_writes = false,
                2 => options.delegated = true,
                3 => options.writeback = true,
                4 => options.checkout_path = Some("/subtree".into()),
                _ => unreachable!(),
            }
            let error =
                match ChunkedFs::open(metadata.clone(), MemoryBlockStore::new(), options).await {
                    Ok(_) => panic!("contradictory compact options opened"),
                    Err(error) => error,
                };
            assert_eq!(error.code, ErrorCode::Einval);
            let loaded = mount_rs_core::storage::MetadataStore::load(&metadata)
                .await
                .unwrap();
            assert_eq!(loaded.revision, 0);
            assert!(loaded.namespace.is_none());
        }
    });
}
