//! Durable two-coordinator directory authority; no native kernel handoff claim.
// Physical SQLite backing authority is currently supported on Unix only.
#![cfg(unix)]
use futures_lite::future::block_on;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions, OwnershipMode};
use mount_rs_core::storage::{DelegatedRecovery, MetadataStore};
use mount_rs_core::{ErrorCode, FsDriver, MkdirOptions};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

type Fs = ChunkedFs<SqliteMetadataStore, SqliteBlockStore>;
struct Volume(PathBuf);
impl Volume {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mount-rs-delegated-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    async fn open(&self, options: ChunkedOptions) -> mount_rs_core::Result<Fs> {
        ChunkedFs::open(
            SqliteMetadataStore::open(self.0.join("metadata.db"))?,
            SqliteBlockStore::open(self.0.join("blocks.db"))?,
            options,
        )
        .await
    }
}
impl Drop for Volume {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn options(owner: &str) -> ChunkedOptions {
    ChunkedOptions::fixed(owner, 4096)
        .unwrap()
        .with_ownership_mode(OwnershipMode::Shared)
}

#[test]
fn durable_disjoint_scopes_deny_file_reads_without_authority_and_cleanly_handoff() {
    block_on(async {
        let volume = Volume::new();
        let bootstrap = volume
            .open(options("bootstrap").with_checkout_path("/"))
            .await
            .unwrap();
        bootstrap
            .mkdir("/a", MkdirOptions::default())
            .await
            .unwrap();
        bootstrap
            .mkdir("/b", MkdirOptions::default())
            .await
            .unwrap();
        bootstrap.checkin_scope().await.unwrap();
        let a = volume.open(options("a")).await.unwrap();
        let b = volume.open(options("b")).await.unwrap();
        assert_eq!(
            a.open("/a/no-file", "w", 0o600).await.err().unwrap().code,
            ErrorCode::Eacces
        );
        a.checkout_scope("/a").await.unwrap();
        assert_eq!(
            b.checkout_scope("/a").await.unwrap_err().code,
            ErrorCode::Estale
        );
        b.checkout_scope("/b").await.unwrap();
        a.write_file("/a/data", b"first").await.unwrap();
        b.write_file("/b/data", b"second").await.unwrap();
        assert_eq!(
            b.open("/a/data", "r", 0).await.err().unwrap().code,
            ErrorCode::Eacces
        );
        assert_eq!(
            b.write_file("/a/data", b"denied").await.unwrap_err().code,
            ErrorCode::Eacces
        );
        assert_eq!(
            b.chmod("/a/data", 0o777).await.unwrap_err().code,
            ErrorCode::Eacces
        );
        b.write_file("/b/data", b"second").await.unwrap();
        let handle = a.open("/a/data", "r+", 0).await.unwrap();
        assert_eq!(a.checkin_scope().await.unwrap_err().code, ErrorCode::Ebusy);
        let mut bytes = [0; 5];
        assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 5);
        assert_eq!(&bytes, b"first");
        a.unlink("/a/data").await.unwrap();
        // Persistent orphan provenance keeps file-data authority bound to the exact grant.
        handle.write(b"after", Some(0)).await.unwrap();
        handle.close().await.unwrap();
        a.checkin_scope().await.unwrap();
        assert_eq!(handle.stat().await.unwrap_err().code, ErrorCode::Estale);
        b.checkin_scope().await.unwrap();
        let next = volume
            .open(options("handoff").with_checkout_path("/b"))
            .await
            .unwrap();
        let handle = next.open("/b/data", "r", 0).await.unwrap();
        let mut bytes = [0; 6];
        handle.read(&mut bytes, None).await.unwrap();
        assert_eq!(&bytes, b"second");
        handle.close().await.unwrap();
        next.shutdown().await.unwrap();
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
        bootstrap.shutdown().await.unwrap();
    });
}

#[test]
fn explicit_recovery_retires_old_handles_and_legacy_cas_is_rejected() {
    block_on(async {
        let volume = Volume::new();
        let old = volume
            .open(options("old").with_checkout_path("/"))
            .await
            .unwrap();
        old.write_file("/data", b"proof").await.unwrap();
        let handle = old.open("/data", "r+", 0).await.unwrap();
        let grant = old.delegation_status().await.unwrap().unwrap();
        let metadata = old.metadata_store();
        let backing = metadata.delegation_state().await.unwrap().unwrap().backing;
        metadata
            .recover(&DelegatedRecovery {
                backing,
                root: grant.token.root,
                expected_fence: grant.token.fence,
            })
            .await
            .unwrap();
        let mut bytes = [0; 5];
        assert_eq!(
            handle.read(&mut bytes, None).await.unwrap_err().code,
            ErrorCode::Estale
        );
        handle.close().await.unwrap();
        let next = volume
            .open(options("next").with_checkout_path("/"))
            .await
            .unwrap();
        next.shutdown().await.unwrap();
        assert!(
            volume
                .open(
                    ChunkedOptions::fixed("cas", 4096)
                        .unwrap()
                        .with_concurrent_writes(true)
                )
                .await
                .is_err()
        );
        assert!(
            volume
                .open(ChunkedOptions::fixed("lease", 4096).unwrap())
                .await
                .is_err()
        );
    });
}

#[test]
fn options_preserve_legacy_cas_and_fail_unsupported_provider_before_mutation() {
    let legacy = options("owner").with_concurrent_writes(true);
    assert!(!legacy.delegated);
    assert!(legacy.checkout_path.is_none());
    let exclusive = options("owner")
        .with_checkout_path("/")
        .with_ownership_mode(OwnershipMode::Exclusive);
    assert!(!exclusive.delegated);
    assert!(exclusive.checkout_path.is_none());
    block_on(async {
        let metadata = SqliteMetadataStore::in_memory().unwrap();
        let blocks = SqliteBlockStore::in_memory().unwrap();
        let error = ChunkedFs::open(metadata.clone(), blocks, options("unsupported"))
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::Enotsup);
        assert_eq!(metadata.load().await.unwrap().revision, 0);
        let volume = Volume::new();
        let metadata = SqliteMetadataStore::open(volume.0.join("metadata.db")).unwrap();
        let error = ChunkedFs::open(
            metadata.clone(),
            SqliteBlockStore::in_memory().unwrap(),
            options("unsupported-blocks"),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.code, ErrorCode::Enotsup);
        assert_eq!(metadata.load().await.unwrap().revision, 0);
        assert!(metadata.delegation_state().await.unwrap().is_none());
    });
}

#[derive(Clone)]
struct AmbiguousMetadata {
    inner: SqliteMetadataStore,
    checkout_error: std::sync::Arc<std::sync::atomic::AtomicBool>,
    checkout_stale: std::sync::Arc<std::sync::atomic::AtomicBool>,
    hide_after_stale: bool,
    state_unavailable_once: std::sync::Arc<std::sync::atomic::AtomicBool>,
    checkin_error: std::sync::Arc<std::sync::atomic::AtomicBool>,
    publication_conflicts: std::sync::Arc<AtomicU64>,
    suspend_release: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait::async_trait]
impl MetadataStore for AmbiguousMetadata {
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    async fn load(&self) -> mount_rs_core::Result<mount_rs_core::storage::LoadedMetadata> {
        self.inner.load().await
    }
    async fn flush(&self) -> mount_rs_core::Result<()> {
        self.inner.flush().await
    }
    fn publish_includes_flush_barrier(&self) -> bool {
        true
    }
    async fn delegation_state(
        &self,
    ) -> mount_rs_core::Result<Option<mount_rs_core::storage::DelegationState>> {
        if self.state_unavailable_once.swap(false, Ordering::SeqCst) {
            return Err(mount_rs_core::FsError::new(ErrorCode::Eio));
        }
        self.inner.delegation_state().await
    }
    async fn prepare_delegated_mode(
        &self,
        backing: mount_rs_core::storage::ConcurrentBackingId,
        revision: u64,
    ) -> mount_rs_core::Result<()> {
        self.inner.prepare_delegated_mode(backing, revision).await
    }
    async fn checkout(
        &self,
        request: &mount_rs_core::storage::CheckoutRequest,
    ) -> mount_rs_core::Result<mount_rs_core::storage::DirectoryGrant> {
        let grant = self.inner.checkout(request).await?;
        if self.checkout_stale.swap(false, Ordering::SeqCst) {
            self.state_unavailable_once
                .store(self.hide_after_stale, Ordering::SeqCst);
            return Err(mount_rs_core::FsError::new(ErrorCode::Estale));
        }
        if self.checkout_error.swap(false, Ordering::SeqCst) {
            return Err(mount_rs_core::FsError::new(ErrorCode::Eio));
        }
        Ok(grant)
    }
    async fn checkin(
        &self,
        request: &mount_rs_core::storage::DelegatedCheckin,
    ) -> mount_rs_core::Result<()> {
        self.inner.checkin(request).await?;
        if self.suspend_release.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.checkin_error.swap(false, Ordering::SeqCst) {
            return Err(mount_rs_core::FsError::new(ErrorCode::Eio));
        }
        Ok(())
    }
    async fn publish_delegated(
        &self,
        request: &mount_rs_core::storage::DelegatedPublish,
        namespace: mount_rs_core::storage::Namespace,
    ) -> mount_rs_core::Result<u64> {
        if self
            .publication_conflicts
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(mount_rs_core::FsError::new(ErrorCode::Eagain));
        }
        self.inner.publish_delegated(request, namespace).await
    }
    async fn acquire_writer(
        &self,
        owner: &str,
        ttl: std::time::Duration,
    ) -> mount_rs_core::Result<mount_rs_core::storage::WriterLease> {
        self.inner.acquire_writer(owner, ttl).await
    }
    async fn renew_writer(
        &self,
        lease: &mount_rs_core::storage::WriterLease,
        ttl: std::time::Duration,
    ) -> mount_rs_core::Result<mount_rs_core::storage::WriterLease> {
        self.inner.renew_writer(lease, ttl).await
    }
    async fn release_writer(
        &self,
        lease: &mount_rs_core::storage::WriterLease,
    ) -> mount_rs_core::Result<()> {
        self.inner.release_writer(lease).await
    }
    async fn publish(
        &self,
        revision: u64,
        lease: &mount_rs_core::storage::WriterLease,
        namespace: mount_rs_core::storage::Namespace,
    ) -> mount_rs_core::Result<u64> {
        self.inner.publish(revision, lease, namespace).await
    }
}

#[test]
fn ambiguous_claim_and_release_retries_keep_identity_and_block_file_operations() {
    block_on(async {
        let volume = Volume::new();
        let bootstrap = volume.open(options("bootstrap")).await.unwrap();
        let metadata = AmbiguousMetadata {
            inner: SqliteMetadataStore::open(volume.0.join("metadata.db")).unwrap(),
            checkout_error: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            checkout_stale: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hide_after_stale: false,
            state_unavailable_once: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            checkin_error: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            publication_conflicts: std::sync::Arc::new(AtomicU64::new(0)),
            suspend_release: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let fs = ChunkedFs::open(
            metadata.clone(),
            SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
            options("faults"),
        )
        .await
        .unwrap();
        assert_eq!(
            fs.checkout_scope("/").await.unwrap_err().code,
            ErrorCode::Eio
        );
        let persisted = metadata
            .inner
            .delegation_state()
            .await
            .unwrap()
            .unwrap()
            .grants[&1]
            .clone();
        let acknowledged = fs.checkout_scope("/").await.unwrap();
        assert_eq!(acknowledged.token, persisted.token);
        fs.write_file("/data", b"durable").await.unwrap();
        assert_eq!(fs.checkin_scope().await.unwrap_err().code, ErrorCode::Eio);
        assert_eq!(
            fs.open("/data", "r", 0).await.err().unwrap().code,
            ErrorCode::Ebusy
        );
        fs.checkin_scope().await.unwrap();
        assert!(fs.delegation_status().await.unwrap().is_none());
        fs.shutdown().await.unwrap();
        bootstrap.shutdown().await.unwrap();
    });
}

#[derive(Clone)]
struct FailedFlushBlocks {
    inner: SqliteBlockStore,
    fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
    suspend: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait::async_trait]
impl mount_rs_core::storage::BlockStore for FailedFlushBlocks {
    fn durable(&self) -> bool {
        true
    }
    async fn prepare_concurrent_backing(
        &self,
    ) -> mount_rs_core::Result<mount_rs_core::storage::ConcurrentBackingId> {
        self.inner.prepare_concurrent_backing().await
    }
    async fn verify_concurrent_backing(
        &self,
        id: mount_rs_core::storage::ConcurrentBackingId,
    ) -> mount_rs_core::Result<()> {
        self.inner.verify_concurrent_backing(id).await
    }
    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<mount_rs_core::storage::BlockId> {
        self.inner.put(bytes).await
    }
    async fn get(&self, id: &mount_rs_core::storage::BlockId) -> mount_rs_core::Result<Vec<u8>> {
        self.inner.get(id).await
    }
    async fn delete(&self, id: &mount_rs_core::storage::BlockId) -> mount_rs_core::Result<()> {
        self.inner.delete(id).await
    }
    async fn flush(&self) -> mount_rs_core::Result<()> {
        if self.suspend.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(mount_rs_core::FsError::new(ErrorCode::Eio));
        }
        self.inner.flush().await
    }
}

#[test]
fn failed_checkin_barrier_retains_grant_and_blocks_new_operations_until_retry() {
    use mount_rs_core::storage::BlockStore;
    block_on(async {
        let volume = Volume::new();
        let blocks = FailedFlushBlocks {
            inner: SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
            fail: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            suspend: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let fs = ChunkedFs::open(
            SqliteMetadataStore::open(volume.0.join("metadata.db")).unwrap(),
            blocks.clone(),
            options("drain").with_checkout_path("/"),
        )
        .await
        .unwrap();
        fs.write_file("/data", b"durable").await.unwrap();
        let grant = fs.delegation_status().await.unwrap().unwrap();
        blocks.fail.store(true, Ordering::SeqCst);
        assert_eq!(fs.checkin_scope().await.unwrap_err().code, ErrorCode::Eio);
        assert_eq!(
            fs.metadata_store()
                .delegation_state()
                .await
                .unwrap()
                .unwrap()
                .grants[&1],
            grant
        );
        assert_eq!(
            fs.open("/data", "r", 0).await.err().unwrap().code,
            ErrorCode::Ebusy
        );
        blocks.fail.store(false, Ordering::SeqCst);
        blocks.flush().await.unwrap();
        fs.checkin_scope().await.unwrap();
        fs.shutdown().await.unwrap();
    });
}

#[test]
fn orphan_reap_replays_conflicts_and_checkin_retries_exhausted_cleanup() {
    block_on(async {
        let volume = Volume::new();
        let metadata = AmbiguousMetadata {
            inner: SqliteMetadataStore::open(volume.0.join("metadata.db")).unwrap(),
            checkout_error: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            checkout_stale: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hide_after_stale: false,
            state_unavailable_once: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            checkin_error: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            publication_conflicts: std::sync::Arc::new(AtomicU64::new(0)),
            suspend_release: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        // Bootstrap before using the wrapper (its legacy mode inspection intentionally is unsupported).
        let bootstrap = volume.open(options("bootstrap")).await.unwrap();
        let fs = ChunkedFs::open(
            metadata.clone(),
            SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
            options("reap").with_checkout_path("/"),
        )
        .await
        .unwrap();
        fs.write_file("/one", b"first").await.unwrap();
        let handle = fs.open("/one", "r+", 0).await.unwrap();
        fs.unlink("/one").await.unwrap();
        metadata.publication_conflicts.store(1, Ordering::SeqCst);
        handle.close().await.unwrap();
        assert!(
            fs.delegation_status()
                .await
                .unwrap()
                .unwrap()
                .orphan_inodes
                .is_empty()
        );

        fs.write_file("/two", b"second").await.unwrap();
        let handle = fs.open("/two", "r+", 0).await.unwrap();
        fs.unlink("/two").await.unwrap();
        metadata.publication_conflicts.store(128, Ordering::SeqCst);
        assert_eq!(handle.close().await.unwrap_err().code, ErrorCode::Eagain);
        metadata.publication_conflicts.store(128, Ordering::SeqCst);
        assert_eq!(
            fs.checkin_scope().await.unwrap_err().code,
            ErrorCode::Eagain
        );
        assert!(
            !metadata
                .inner
                .delegation_state()
                .await
                .unwrap()
                .unwrap()
                .grants[&1]
                .orphan_inodes
                .is_empty()
        );
        metadata.publication_conflicts.store(0, Ordering::SeqCst);
        fs.checkin_scope().await.unwrap();
        assert!(
            metadata
                .inner
                .delegation_state()
                .await
                .unwrap()
                .unwrap()
                .grants
                .is_empty()
        );
        fs.shutdown().await.unwrap();
        bootstrap.shutdown().await.unwrap();
    });
}

#[test]
fn actual_future_cancellation_during_checkin_flush_and_release_retries_safely() {
    block_on(async {
        let volume = Volume::new();
        let bootstrap = volume.open(options("bootstrap")).await.unwrap();
        let metadata = AmbiguousMetadata {
            inner: SqliteMetadataStore::open(volume.0.join("metadata.db")).unwrap(),
            checkout_error: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            checkout_stale: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hide_after_stale: false,
            state_unavailable_once: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            checkin_error: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            publication_conflicts: std::sync::Arc::new(AtomicU64::new(0)),
            suspend_release: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let blocks = FailedFlushBlocks {
            inner: SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
            fail: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            suspend: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let fs = ChunkedFs::open(
            metadata.clone(),
            blocks.clone(),
            options("cancel").with_checkout_path("/"),
        )
        .await
        .unwrap();
        fs.write_file("/data", b"durable").await.unwrap();
        let grant = fs.delegation_status().await.unwrap().unwrap();
        blocks.suspend.store(true, Ordering::SeqCst);
        let mut checking_in = Box::pin(fs.checkin_scope());
        assert!(
            futures_lite::future::poll_once(&mut checking_in)
                .await
                .is_none()
        );
        drop(checking_in);
        assert_eq!(
            metadata
                .inner
                .delegation_state()
                .await
                .unwrap()
                .unwrap()
                .grants[&1],
            grant
        );
        assert_eq!(
            fs.open("/data", "r", 0).await.err().unwrap().code,
            ErrorCode::Ebusy
        );
        blocks.suspend.store(false, Ordering::SeqCst);
        fs.checkin_scope().await.unwrap();

        let grant = fs.checkout_scope("/").await.unwrap();
        metadata.suspend_release.store(true, Ordering::SeqCst);
        let mut checking_in = Box::pin(fs.checkin_scope());
        assert!(
            futures_lite::future::poll_once(&mut checking_in)
                .await
                .is_none()
        );
        drop(checking_in);
        let authority = metadata.inner.delegation_state().await.unwrap().unwrap();
        assert!(authority.grants.is_empty());
        assert!(authority.retired.contains(&grant.token));
        assert_eq!(
            fs.open("/data", "r", 0).await.err().unwrap().code,
            ErrorCode::Ebusy
        );
        metadata.suspend_release.store(false, Ordering::SeqCst);
        fs.checkin_scope().await.unwrap();
        assert!(fs.delegation_status().await.unwrap().is_none());
        fs.shutdown().await.unwrap();
        bootstrap.shutdown().await.unwrap();
    });
}

#[test]
fn checkout_path_boundaries_reject_before_initialization_or_grant() {
    block_on(async {
        for path in ["", "relative", "/nul\0path"] {
            let volume = Volume::new();
            assert_eq!(
                volume
                    .open(options("invalid").with_checkout_path(path))
                    .await
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::Einval
            );
            let metadata = SqliteMetadataStore::open(volume.0.join("metadata.db")).unwrap();
            assert_eq!(metadata.load().await.unwrap().revision, 0);
            assert!(metadata.delegation_state().await.unwrap().is_none());
            let fs = volume.open(options("valid")).await.unwrap();
            assert_eq!(
                fs.checkout_scope(path).await.unwrap_err().code,
                ErrorCode::Einval
            );
            assert!(
                fs.metadata_store()
                    .delegation_state()
                    .await
                    .unwrap()
                    .unwrap()
                    .grants
                    .is_empty()
            );
            fs.shutdown().await.unwrap();
        }
    });
}

#[test]
fn postcommit_stale_claim_retains_retry_identity_even_when_reconciliation_unavailable() {
    block_on(async {
        for hide_after_stale in [false, true] {
            let volume = Volume::new();
            let bootstrap = volume.open(options("bootstrap")).await.unwrap();
            let metadata = AmbiguousMetadata {
                inner: SqliteMetadataStore::open(volume.0.join("metadata.db")).unwrap(),
                checkout_error: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                checkout_stale: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                hide_after_stale,
                state_unavailable_once: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                    false,
                )),
                checkin_error: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                publication_conflicts: std::sync::Arc::new(AtomicU64::new(0)),
                suspend_release: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            };
            let fs = ChunkedFs::open(
                metadata.clone(),
                SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap(),
                options("stale"),
            )
            .await
            .unwrap();
            assert_eq!(
                fs.checkout_scope("/").await.unwrap_err().code,
                ErrorCode::Estale
            );
            let committed = metadata
                .inner
                .delegation_state()
                .await
                .unwrap()
                .unwrap()
                .grants[&1]
                .clone();
            assert_eq!(fs.checkout_scope("/").await.unwrap().token, committed.token);
            fs.shutdown().await.unwrap();
            bootstrap.shutdown().await.unwrap();
        }
    });
}
