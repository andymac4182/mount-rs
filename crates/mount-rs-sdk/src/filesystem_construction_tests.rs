//! Actual SQLite delegated authority at the SDK retained-cleanup boundary.
//!
//! Include this as a child of `filesystem`, so the fixture can retain the real
//! observed provider group while inserting an acknowledgement gate around its
//! canonical erased SQLite metadata provider. No production constructor changes.

#![cfg(unix)]

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use mount_rs_core::storage::{
    CheckoutRequest, ConcurrentModeState, DelegatedCheckin, DelegatedPublish, DirectoryGrant,
    LoadedMetadata, Namespace, WriterLease,
};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use tokio::sync::Notify;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "sdk-construction-cleanup-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn metadata(&self) -> PathBuf {
        self.0.join("metadata.db")
    }

    fn blocks(&self) -> PathBuf {
        self.0.join("blocks.db")
    }

    async fn open(&self) -> (Arc<Filesystem>, Arc<CheckoutAcknowledgement>) {
        let journal = crate::ConstructionJournal::new();
        let attempt = journal.begin().unwrap();
        let opened = open_storage_in_context_with_observer(
            &StoreConfig::Sqlite {
                path: self.metadata(),
            },
            &StoreConfig::Sqlite {
                path: self.blocks(),
            },
            None,
            None,
            Some(&journal),
        )
        .await
        .unwrap();
        assert!(opened.resources.is_observed());
        let proxy = Arc::new(CheckoutAcknowledgement {
            inner: opened.metadata,
            suspend: AtomicBool::new(false),
            committed: Notify::new(),
            release_acknowledgement: Notify::new(),
            checkout_calls: AtomicUsize::new(0),
            checkin_calls: AtomicUsize::new(0),
        });
        #[cfg(feature = "observability")]
        let metadata = ErasedMetadataStore::new(proxy.clone(), global_telemetry());
        #[cfg(not(feature = "observability"))]
        let metadata = ErasedMetadataStore::new(proxy.clone());
        let driver = ChunkedFs::open_with_observer(
            metadata,
            opened.blocks,
            ChunkedOptions::fixed("sdk-pending-checkout-owner", 4096)
                .unwrap()
                .with_ownership_mode(mount_rs_chunked::OwnershipMode::Shared),
            Some(&journal),
        )
        .await
        .unwrap();
        let filesystem = Arc::new(Filesystem {
            inner: FilesystemInner::Split(driver, opened.resources),
            persistent_eviction_allowed: false,
        });
        assert!(journal.snapshot().retained_resources >= 2);
        // The actual returned SDK owner exists before construction handoff. The
        // later ambiguous checkout must be detected by that owner's teardown.
        attempt.handoff().unwrap();
        assert!(journal.snapshot().handed_off);
        assert_eq!(journal.snapshot().retained_resources, 0);
        (filesystem, proxy)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

struct CheckoutAcknowledgement {
    inner: ErasedMetadataStore,
    suspend: AtomicBool,
    committed: Notify,
    release_acknowledgement: Notify,
    checkout_calls: AtomicUsize,
    checkin_calls: AtomicUsize,
}

#[async_trait::async_trait]
impl MetadataStore for CheckoutAcknowledgement {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    fn publish_includes_flush_barrier(&self) -> bool {
        self.inner.publish_includes_flush_barrier()
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        self.inner.load().await
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

    async fn publish(
        &self,
        revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        self.inner.publish(revision, lease, namespace).await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }

    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        self.inner.concurrent_mode_state().await
    }

    async fn delegation_state(&self) -> Result<Option<DelegationState>> {
        self.inner.delegation_state().await
    }

    async fn prepare_delegated_mode(
        &self,
        backing: ConcurrentBackingId,
        revision: u64,
    ) -> Result<()> {
        self.inner.prepare_delegated_mode(backing, revision).await
    }

    async fn checkout(&self, request: &CheckoutRequest) -> Result<DirectoryGrant> {
        self.checkout_calls.fetch_add(1, Ordering::SeqCst);
        // This is the actual SQLite commit and its provider durability barrier.
        let grant = self.inner.checkout(request).await?;
        if self.suspend.load(Ordering::Acquire) {
            self.committed.notify_one();
            self.release_acknowledgement.notified().await;
        }
        Ok(grant)
    }

    async fn publish_delegated(
        &self,
        request: &DelegatedPublish,
        namespace: Namespace,
    ) -> Result<u64> {
        self.inner.publish_delegated(request, namespace).await
    }

    async fn checkin(&self, request: &DelegatedCheckin) -> Result<()> {
        self.checkin_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.checkin(request).await
    }

    async fn recover(&self, request: &DelegatedRecovery) -> Result<()> {
        self.inner.recover(request).await
    }
}

#[derive(Clone, Copy)]
enum CleanupEntry {
    ObservedShutdown,
    ConstructionResource,
}

async fn committed_checkout_cancellation(entry: CleanupEntry) {
    let fixture = Fixture::new();
    let (filesystem, proxy) = fixture.open().await;
    assert!(!filesystem.failed());
    assert!(filesystem.delegation_status().await.unwrap().is_none());
    let backing = filesystem.concurrent_backing_id().unwrap();
    proxy.suspend.store(true, Ordering::Release);
    let checkout_owner = Arc::clone(&filesystem);
    let checkout = tokio::spawn(async move { checkout_owner.checkout_scope("/").await });
    let committed = tokio::time::timeout(Duration::from_secs(5), proxy.committed.notified()).await;
    checkout.abort();
    // Settle the canceled task before any assertion, including an unexpected
    // missing commit. No checkout future may outlive the fixture or oracle.
    let joined = checkout.await;
    committed.expect("actual SQLite checkout did not reach acknowledgement gate");
    assert!(joined.unwrap_err().is_cancelled());
    assert_eq!(proxy.checkout_calls.load(Ordering::SeqCst), 1);
    assert!(
        !filesystem.failed(),
        "pending checkout is not publication failure"
    );
    assert!(filesystem.delegation_status().await.unwrap().is_none());

    // A distinct fresh connection, not the proxy's cached response, proves the
    // exact real grant was committed even though the SDK has no local grant.
    let stored = SqliteMetadataStore::open(fixture.metadata()).unwrap();
    let before = stored.delegation_state().await.unwrap().unwrap();
    assert_eq!(before.backing, backing);
    assert_eq!(before.grants.len(), 1);
    let grant = before.grants.values().next().unwrap().clone();
    SqliteBlockStore::open(fixture.blocks())
        .unwrap()
        .verify_concurrent_backing(backing)
        .await
        .unwrap();

    let result = match entry {
        CleanupEntry::ObservedShutdown => filesystem.shutdown().await,
        CleanupEntry::ConstructionResource => {
            ConstructionResource::close(filesystem.as_ref()).await
        }
    };
    let after = stored.delegation_state().await.unwrap().unwrap();
    let checkin_calls = proxy.checkin_calls.load(Ordering::SeqCst);
    let failed = filesystem.failed();
    // Only this test oracle knows the withheld acknowledgement's exact token.
    // Explicitly release it before asserting the expected semantic RED; this
    // does not turn SDK teardown into an acknowledged reconciliation receipt.
    stored
        .checkin(&DelegatedCheckin {
            backing,
            token: grant.token,
            expected_revision: stored.load().await.unwrap().revision,
        })
        .await
        .unwrap();
    assert!(
        stored
            .delegation_state()
            .await
            .unwrap()
            .unwrap()
            .grants
            .is_empty()
    );
    assert_eq!(
        after, before,
        "SDK teardown changed unacknowledged authority"
    );
    assert_eq!(
        checkin_calls, 0,
        "SDK cannot release an unacknowledged grant"
    );
    assert!(!failed);
    assert!(
        matches!(result, Err(error) if error.code == ErrorCode::Eio),
        "SDK retained cleanup accepted a committed checkout with a canceled acknowledgement"
    );
}

#[tokio::test]
async fn observed_shutdown_rejects_committed_unacknowledged_checkout() {
    committed_checkout_cancellation(CleanupEntry::ObservedShutdown).await;
}

#[tokio::test]
async fn construction_resource_close_rejects_committed_unacknowledged_checkout() {
    committed_checkout_cancellation(CleanupEntry::ConstructionResource).await;
}

#[tokio::test]
async fn healthy_delegated_shutdown_releases_exact_grant_and_preserves_bytes() {
    let fixture = Fixture::new();
    let (filesystem, proxy) = fixture.open().await;
    let backing = filesystem.concurrent_backing_id().unwrap();
    let grant = filesystem.checkout_scope("/").await.unwrap();
    let bytes: Vec<_> = (0..8193).map(|i| ((i * 37 + i / 17) % 251) as u8).collect();
    filesystem
        .driver()
        .write_file("/acknowledged", &bytes)
        .await
        .unwrap();
    filesystem.shutdown().await.unwrap();
    ConstructionResource::close(filesystem.as_ref())
        .await
        .unwrap();
    assert!(!filesystem.failed());
    assert_eq!(proxy.checkout_calls.load(Ordering::SeqCst), 1);
    assert_eq!(proxy.checkin_calls.load(Ordering::SeqCst), 1);
    let stored = SqliteMetadataStore::open(fixture.metadata()).unwrap();
    let authority = stored.delegation_state().await.unwrap().unwrap();
    assert_eq!(authority.backing, backing);
    assert!(authority.grants.is_empty());
    assert!(authority.retired.contains(&grant.token));
    drop(filesystem);
    drop(proxy);

    let (fresh, _) = fixture.open().await;
    assert_eq!(fresh.concurrent_backing_id(), Some(backing));
    fresh.checkout_scope("/").await.unwrap();
    let handle = fresh.driver().open("/acknowledged", "r", 0).await.unwrap();
    assert_eq!(handle.stat().await.unwrap().size, bytes.len() as u64);
    let mut actual = vec![0; bytes.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let end = (offset + 1379).min(actual.len());
        let count = handle
            .read(&mut actual[offset..end], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0 && count <= end - offset);
        offset += count;
    }
    assert_eq!(actual, bytes);
    assert_eq!(
        handle
            .read(&mut [0; 17], Some(bytes.len() as u64))
            .await
            .unwrap(),
        0
    );
    handle.close().await.unwrap();
    ConstructionResource::close(fresh.as_ref()).await.unwrap();
    assert!(
        stored
            .delegation_state()
            .await
            .unwrap()
            .unwrap()
            .grants
            .is_empty()
    );
}
