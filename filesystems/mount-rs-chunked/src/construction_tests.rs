//! Construction must retain actual authority before an acknowledgement can be lost.

use super::*;
use futures_lite::future::{block_on, poll_once};
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::storage::{BlockId, DelegationState, LoadedMetadata};
use mount_rs_memory::MemoryBlockStore;
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::path::{Path, PathBuf};

struct OwnedDirectory(PathBuf);
impl OwnedDirectory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "mount-rs-construction-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn metadata(&self) -> PathBuf {
        self.0.join("metadata.sqlite")
    }
    fn blocks(&self) -> PathBuf {
        self.0.join("blocks.sqlite")
    }
}
impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Point {
    Load,
    Acquired,
    PublicationAcknowledgement,
    CheckoutAcknowledgement,
    Release,
    Released,
}
#[derive(Clone, Copy)]
enum Fault {
    Error,
    Suspend,
    Panic,
}

#[derive(Clone)]
struct MetadataProbe {
    inner: Arc<SqliteMetadataStore>,
    fault: Arc<Mutex<Option<(Point, Fault)>>>,
    entered: Arc<AtomicUsize>,
    acquired: Arc<Mutex<Vec<WriterLease>>>,
    renewed: Arc<Mutex<Vec<WriterLease>>>,
    released: Arc<Mutex<Vec<WriterLease>>>,
    grants: Arc<Mutex<Vec<DirectoryGrant>>>,
    events: Arc<Mutex<Vec<&'static str>>>,
    widen_renewal: bool,
}
impl MetadataProbe {
    fn new(path: &Path) -> Self {
        Self {
            inner: Arc::new(SqliteMetadataStore::open(path).unwrap()),
            fault: Arc::new(Mutex::new(None)),
            entered: Arc::new(AtomicUsize::new(0)),
            acquired: Arc::new(Mutex::new(Vec::new())),
            renewed: Arc::new(Mutex::new(Vec::new())),
            released: Arc::new(Mutex::new(Vec::new())),
            grants: Arc::new(Mutex::new(Vec::new())),
            events: Arc::new(Mutex::new(Vec::new())),
            widen_renewal: false,
        }
    }
    fn arm(&self, point: Point, fault: Fault) {
        *self.fault.lock().unwrap() = Some((point, fault));
    }
    async fn trip(&self, point: Point) -> Result<()> {
        let fault = {
            let mut fault = self.fault.lock().unwrap();
            if fault.as_ref().is_some_and(|(at, _)| *at == point) {
                fault.take().map(|(_, fault)| fault)
            } else {
                None
            }
        };
        let Some(fault) = fault else { return Ok(()) };
        self.entered.fetch_add(1, Ordering::SeqCst);
        match fault {
            Fault::Error => Err(FsError::new(ErrorCode::Eio).with_syscall("construction-oracle")),
            Fault::Suspend => std::future::pending().await,
            Fault::Panic => panic!("intentional constructor acknowledgement panic"),
        }
    }
    fn release_calls(&self) -> usize {
        self.released.lock().unwrap().len()
    }
}

#[async_trait]
impl MetadataStore for MetadataProbe {
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    fn publish_includes_flush_barrier(&self) -> bool {
        self.inner.publish_includes_flush_barrier()
    }
    async fn load(&self) -> Result<LoadedMetadata> {
        self.trip(Point::Load).await?;
        self.inner.load().await
    }
    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        let lease = self.inner.acquire_writer(owner, ttl).await?;
        self.acquired.lock().unwrap().push(lease.clone());
        self.trip(Point::Acquired).await?;
        Ok(lease)
    }
    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        // Make the actual SQLite expiry change deterministically, even when
        // acquisition and renewal occur in the same SQLite clock tick.
        let ttl = if self.widen_renewal {
            ttl + Duration::from_secs(5)
        } else {
            ttl
        };
        let renewed = self.inner.renew_writer(lease, ttl).await?;
        self.renewed.lock().unwrap().push(renewed.clone());
        Ok(renewed)
    }
    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        self.released.lock().unwrap().push(lease.clone());
        self.events.lock().unwrap().push("writer-release");
        self.trip(Point::Release).await?;
        self.inner.release_writer(lease).await?;
        self.trip(Point::Released).await
    }
    async fn publish(
        &self,
        revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        let revision = self.inner.publish(revision, lease, namespace).await?;
        self.trip(Point::PublicationAcknowledgement).await?;
        Ok(revision)
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
        let grant = self.inner.checkout(request).await?;
        self.grants.lock().unwrap().push(grant.clone());
        self.trip(Point::CheckoutAcknowledgement).await?;
        Ok(grant)
    }
    async fn checkin(&self, request: &DelegatedCheckin) -> Result<()> {
        self.inner.checkin(request).await
    }
    async fn publish_delegated(
        &self,
        request: &DelegatedPublish,
        namespace: Namespace,
    ) -> Result<u64> {
        self.inner.publish_delegated(request, namespace).await
    }
}

#[derive(Default)]
struct Observer(Mutex<Vec<Arc<dyn ConstructionResource>>>);
impl ConstructionObserver for Observer {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        self.0.lock().unwrap().push(resource);
    }
}
impl Observer {
    fn retained(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    async fn close_reverse(&self) -> Result<()> {
        let resources = self.0.lock().unwrap().clone();
        for resource in resources.iter().rev() {
            resource.close().await?;
        }
        Ok(())
    }
}

struct ProviderProbe {
    closed: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<&'static str>>>,
}
#[async_trait]
impl ConstructionResource for ProviderProbe {
    async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        self.events.lock().unwrap().push("provider-close");
        Ok(())
    }
}

// A weak reference to the real SQLite provider, rather than a separate drop
// counter, detects loss of the block owner before filesystem transfer.
struct BlockOwnerProbe(Arc<SqliteBlockStore>);
#[async_trait]
impl BlockStore for BlockOwnerProbe {
    fn durable(&self) -> bool {
        self.0.durable()
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.0.put(bytes).await
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.0.get(id).await
    }
    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.0.delete(id).await
    }
}

#[test]
fn failed_construction_keeps_actual_block_owner_until_acknowledged_authority_cleanup() {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    metadata.arm(Point::Load, Fault::Error);
    let blocks = Arc::new(SqliteBlockStore::open(directory.blocks()).unwrap());
    let weak_blocks = Arc::downgrade(&blocks);
    let observer = Observer::default();
    let open = ChunkedFs::open_with_observer(
        metadata.clone(),
        BlockOwnerProbe(Arc::clone(&blocks)),
        options(),
        Some(&observer),
    );
    drop(blocks);
    assert!(block_on(open).is_err());
    assert!(
        weak_blocks.upgrade().is_some(),
        "failed open dropped the actual block provider"
    );
    metadata.arm(Point::Release, Fault::Error);
    assert!(block_on(observer.close_reverse()).is_err());
    assert!(
        weak_blocks.upgrade().is_some(),
        "unacknowledged release dropped the actual block provider"
    );
    block_on(observer.close_reverse()).unwrap();
    assert!(
        weak_blocks.upgrade().is_none(),
        "acknowledged cleanup retained the actual block provider"
    );
    assert!(lease_row(&directory.metadata()).0.is_none());
}

#[test]
fn cancelled_post_acquire_load_keeps_actual_block_owner_until_acknowledged_cleanup() {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    metadata.arm(Point::Load, Fault::Suspend);
    let blocks = Arc::new(SqliteBlockStore::open(directory.blocks()).unwrap());
    let weak_blocks = Arc::downgrade(&blocks);
    let observer = Observer::default();
    let mut open = Box::pin(ChunkedFs::open_with_observer(
        metadata.clone(),
        BlockOwnerProbe(Arc::clone(&blocks)),
        options(),
        Some(&observer),
    ));
    drop(blocks);
    assert!(block_on(poll_once(open.as_mut())).is_none());
    assert_eq!(metadata.entered.load(Ordering::SeqCst), 1);
    assert!(lease_row(&directory.metadata()).0.is_some());
    drop(open);
    assert!(
        weak_blocks.upgrade().is_some(),
        "canceled open dropped its actual block provider"
    );
    block_on(observer.close_reverse()).unwrap();
    assert!(
        weak_blocks.upgrade().is_none(),
        "acknowledged cleanup retained the actual block provider"
    );
    assert!(lease_row(&directory.metadata()).0.is_none());
}

#[test]
fn cancelled_unknown_acquisition_keeps_actual_block_owner_for_reconciliation() {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    metadata.arm(Point::Acquired, Fault::Suspend);
    let blocks = Arc::new(SqliteBlockStore::open(directory.blocks()).unwrap());
    let weak_blocks = Arc::downgrade(&blocks);
    let observer = Observer::default();
    let mut open = Box::pin(ChunkedFs::open_with_observer(
        metadata.clone(),
        BlockOwnerProbe(Arc::clone(&blocks)),
        options(),
        Some(&observer),
    ));
    drop(blocks);
    assert!(block_on(poll_once(open.as_mut())).is_none());
    assert_eq!(metadata.entered.load(Ordering::SeqCst), 1);
    drop(open);
    assert!(
        weak_blocks.upgrade().is_some(),
        "unknown acquisition dropped its actual block provider"
    );
    assert!(block_on(observer.close_reverse()).is_err());
    assert!(
        weak_blocks.upgrade().is_some(),
        "uncertain cleanup dropped the provider needed for reconciliation"
    );
    // The test oracle, unlike the constructor, received this actual token.
    // Releasing it here cleans the fixture but does not acknowledge journal cleanup.
    let actual = metadata.acquired.lock().unwrap()[0].clone();
    block_on(metadata.inner.release_writer(&actual)).unwrap();
    assert!(weak_blocks.upgrade().is_some());
}

fn lease_row(path: &Path) -> (Option<String>, u64, u64) {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .query_row(
            "SELECT owner, fence, expires FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}
fn options() -> ChunkedOptions {
    ChunkedOptions::fixed("construction-owner", 64).unwrap()
}

#[test]
fn acknowledged_sqlite_lease_is_retained_on_later_load_error_until_cleanup() {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    metadata.arm(Point::Load, Fault::Error);
    let observer = Observer::default();
    let error = block_on(ChunkedFs::open_with_observer(
        metadata.clone(),
        MemoryBlockStore::new(),
        options(),
        Some(&observer),
    ))
    .err()
    .expect("load fault must fail construction");
    assert_eq!(error.code, ErrorCode::Eio);
    assert_eq!(observer.retained(), 1);
    assert_eq!(
        metadata.release_calls(),
        0,
        "open discarded journal ownership through best effort release"
    );
    let acquired = metadata.acquired.lock().unwrap()[0].clone();
    assert_eq!(
        lease_row(&directory.metadata()),
        (Some(acquired.owner), acquired.fence, acquired.expires_at_ms)
    );
    block_on(observer.close_reverse()).unwrap();
    assert_eq!(metadata.release_calls(), 1);
    assert!(lease_row(&directory.metadata()).0.is_none());
    let next = block_on(
        metadata
            .inner
            .acquire_writer("after-cleanup", Duration::from_secs(30)),
    )
    .unwrap();
    block_on(metadata.inner.release_writer(&next)).unwrap();
}

#[test]
fn transferred_filesystem_releases_latest_sqlite_token_before_provider_and_drops_its_owners() {
    let directory = OwnedDirectory::new();
    let mut metadata = MetadataProbe::new(&directory.metadata());
    metadata.widen_renewal = true;
    let references_before = Arc::strong_count(&metadata.inner);
    let observer = Observer::default();
    let provider_closed = Arc::new(AtomicBool::new(false));
    observer.retain(Arc::new(ProviderProbe {
        closed: provider_closed.clone(),
        events: metadata.events.clone(),
    }));
    let filesystem = block_on(ChunkedFs::open_with_observer(
        metadata.clone(),
        MemoryBlockStore::new(),
        options(),
        Some(&observer),
    ))
    .unwrap();
    assert_eq!(observer.retained(), 2);
    let acquired = metadata.acquired.lock().unwrap()[0].clone();
    let latest = metadata.renewed.lock().unwrap().last().unwrap().clone();
    assert!(latest.expires_at_ms > acquired.expires_at_ms);
    assert_eq!(
        lease_row(&directory.metadata()),
        (
            Some(latest.owner.clone()),
            latest.fence,
            latest.expires_at_ms
        )
    );
    drop(filesystem);
    block_on(observer.close_reverse()).unwrap();
    // Healthy shutdown deliberately refreshes once more. Release must use
    // that final acknowledged token, which can differ from the open token.
    let cleanup_token = metadata.renewed.lock().unwrap().last().unwrap().clone();
    assert!(provider_closed.load(Ordering::Acquire));
    assert_eq!(
        metadata.released.lock().unwrap().as_slice(),
        &[cleanup_token]
    );
    assert_eq!(
        metadata.events.lock().unwrap().as_slice(),
        &["writer-release", "provider-close"]
    );
    assert!(lease_row(&directory.metadata()).0.is_none());
    assert_eq!(
        Arc::strong_count(&metadata.inner),
        references_before,
        "completed construction journal retained the actual filesystem/provider"
    );
    block_on(observer.close_reverse()).unwrap();
    assert_eq!(
        metadata.release_calls(),
        1,
        "cleanup released a transferred old token twice"
    );
}

#[test]
fn failed_exact_token_release_retains_owner_and_allows_acknowledged_cleanup_retry() {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    metadata.arm(Point::Load, Fault::Error);
    let observer = Observer::default();
    assert!(
        block_on(ChunkedFs::open_with_observer(
            metadata.clone(),
            MemoryBlockStore::new(),
            options(),
            Some(&observer),
        ))
        .is_err()
    );
    metadata.arm(Point::Release, Fault::Error);
    assert_eq!(
        block_on(observer.close_reverse()).unwrap_err().code,
        ErrorCode::Eio
    );
    assert!(lease_row(&directory.metadata()).0.is_some());
    block_on(observer.close_reverse()).unwrap();
    let released = metadata.released.lock().unwrap();
    assert_eq!(released.len(), 2);
    assert_eq!(released[0], released[1]);
    assert!(lease_row(&directory.metadata()).0.is_none());
}

#[test]
fn committed_but_unacknowledged_release_never_becomes_proven_cleanup() {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    metadata.arm(Point::Load, Fault::Error);
    let observer = Observer::default();
    assert!(
        block_on(ChunkedFs::open_with_observer(
            metadata.clone(),
            MemoryBlockStore::new(),
            options(),
            Some(&observer),
        ))
        .is_err()
    );
    metadata.arm(Point::Released, Fault::Error);
    assert_eq!(
        block_on(observer.close_reverse()).unwrap_err().code,
        ErrorCode::Eio
    );
    assert!(
        lease_row(&directory.metadata()).0.is_none(),
        "real SQLite release must have committed before the injected lost acknowledgement"
    );
    assert!(
        block_on(observer.close_reverse()).is_err(),
        "stale retry was mistaken for acknowledged release"
    );
    assert_eq!(observer.retained(), 1);
}

fn acquisition_acknowledgement_is_lost(fault: Fault) {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    metadata.arm(Point::Acquired, fault);
    let observer = Observer::default();
    let provider_closed = Arc::new(AtomicBool::new(false));
    observer.retain(Arc::new(ProviderProbe {
        closed: provider_closed.clone(),
        events: metadata.events.clone(),
    }));
    match fault {
        Fault::Suspend => {
            let mut open = Box::pin(ChunkedFs::open_with_observer(
                metadata.clone(),
                MemoryBlockStore::new(),
                options(),
                Some(&observer),
            ));
            assert!(block_on(poll_once(open.as_mut())).is_none());
            assert_eq!(metadata.entered.load(Ordering::SeqCst), 1);
            assert_eq!(
                observer.retained(),
                2,
                "intent owner was not retained before acquire returned"
            );
            drop(open);
        }
        Fault::Panic => {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                block_on(ChunkedFs::open_with_observer(
                    metadata.clone(),
                    MemoryBlockStore::new(),
                    options(),
                    Some(&observer),
                ))
            }));
            assert!(outcome.is_err());
        }
        Fault::Error => {
            assert!(
                block_on(ChunkedFs::open_with_observer(
                    metadata.clone(),
                    MemoryBlockStore::new(),
                    options(),
                    Some(&observer),
                ))
                .is_err()
            );
        }
    }
    assert_eq!(metadata.entered.load(Ordering::SeqCst), 1);
    assert_eq!(observer.retained(), 2);
    assert!(
        lease_row(&directory.metadata()).0.is_some(),
        "the oracle did not cross a real acquisition commit"
    );
    assert!(
        block_on(observer.close_reverse()).is_err(),
        "unknown acquisition was reported closed"
    );
    assert_eq!(
        metadata.release_calls(),
        0,
        "cleanup fabricated an unknown token"
    );
    assert!(
        !provider_closed.load(Ordering::Acquire),
        "cleanup closed the provider required for authority reconciliation"
    );
    // The oracle knows the provider-returned token that the constructor never
    // received; release it solely to leave this test's owned directory clean.
    let actual = metadata.acquired.lock().unwrap()[0].clone();
    block_on(metadata.inner.release_writer(&actual)).unwrap();
}

#[test]
fn cancelled_acquisition_keeps_unknown_actual_sqlite_authority_and_provider() {
    acquisition_acknowledgement_is_lost(Fault::Suspend);
}
#[test]
fn panicking_acquisition_keeps_unknown_actual_sqlite_authority_and_provider() {
    acquisition_acknowledgement_is_lost(Fault::Panic);
}
#[test]
fn errored_acquisition_keeps_unknown_actual_sqlite_authority_and_provider() {
    acquisition_acknowledgement_is_lost(Fault::Error);
}

fn publication_acknowledgement_is_lost(fault: Fault) {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    metadata.arm(Point::PublicationAcknowledgement, fault);
    let observer = Observer::default();
    let provider_closed = Arc::new(AtomicBool::new(false));
    observer.retain(Arc::new(ProviderProbe {
        closed: provider_closed.clone(),
        events: metadata.events.clone(),
    }));
    match fault {
        Fault::Suspend => {
            let mut open = Box::pin(ChunkedFs::open_with_observer(
                metadata.clone(),
                MemoryBlockStore::new(),
                options(),
                Some(&observer),
            ));
            assert!(block_on(poll_once(open.as_mut())).is_none());
            assert_eq!(metadata.entered.load(Ordering::SeqCst), 1);
            assert_eq!(
                observer.retained(),
                2,
                "filesystem was not retained before publish acknowledgement"
            );
            drop(open);
        }
        Fault::Panic => {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                block_on(ChunkedFs::open_with_observer(
                    metadata.clone(),
                    MemoryBlockStore::new(),
                    options(),
                    Some(&observer),
                ))
            }));
            assert!(outcome.is_err());
        }
        Fault::Error => {
            assert!(
                block_on(ChunkedFs::open_with_observer(
                    metadata.clone(),
                    MemoryBlockStore::new(),
                    options(),
                    Some(&observer),
                ))
                .is_err()
            );
        }
    }
    assert_eq!(metadata.entered.load(Ordering::SeqCst), 1);
    let committed = block_on(metadata.inner.load()).unwrap();
    assert_eq!(committed.revision, 1);
    assert!(
        committed.namespace.is_some(),
        "publication oracle never crossed a real metadata commit"
    );
    assert_eq!(observer.retained(), 2);
    assert!(
        block_on(observer.close_reverse()).is_err(),
        "unacknowledged initial publication lost its failed filesystem owner"
    );
    assert_eq!(metadata.release_calls(), 0);
    assert!(!provider_closed.load(Ordering::Acquire));
    let actual = metadata.renewed.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        lease_row(&directory.metadata()),
        (
            Some(actual.owner.clone()),
            actual.fence,
            actual.expires_at_ms
        )
    );
    // Only the test oracle knows the exact provider acknowledgement that was
    // intentionally withheld from the construction future.
    block_on(metadata.inner.release_writer(&actual)).unwrap();
}

#[test]
fn cancelled_initial_publication_retains_actual_failed_filesystem_and_provider() {
    publication_acknowledgement_is_lost(Fault::Suspend);
}
#[test]
fn panicking_initial_publication_retains_actual_failed_filesystem_and_provider() {
    publication_acknowledgement_is_lost(Fault::Panic);
}
#[test]
fn errored_initial_publication_retains_actual_failed_filesystem_and_provider() {
    publication_acknowledgement_is_lost(Fault::Error);
}

fn checkout_acknowledgement_is_lost(fault: Fault) {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    let blocks = SqliteBlockStore::open(directory.blocks()).unwrap();
    metadata.arm(Point::CheckoutAcknowledgement, fault);
    let observer = Observer::default();
    let open_options = options().with_checkout_path("/");
    match fault {
        Fault::Suspend => {
            let mut open = Box::pin(ChunkedFs::open_with_observer(
                metadata.clone(),
                blocks,
                open_options,
                Some(&observer),
            ));
            assert!(block_on(poll_once(open.as_mut())).is_none());
            assert_eq!(metadata.entered.load(Ordering::SeqCst), 1);
            assert_eq!(
                observer.retained(),
                1,
                "actual delegated filesystem was not retained before checkout"
            );
            drop(open);
        }
        Fault::Panic => {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                block_on(ChunkedFs::open_with_observer(
                    metadata.clone(),
                    blocks,
                    open_options,
                    Some(&observer),
                ))
            }));
            assert!(outcome.is_err());
        }
        Fault::Error => {
            assert!(
                block_on(ChunkedFs::open_with_observer(
                    metadata.clone(),
                    blocks,
                    open_options,
                    Some(&observer),
                ))
                .is_err()
            );
        }
    }
    assert_eq!(metadata.entered.load(Ordering::SeqCst), 1);
    assert_eq!(observer.retained(), 1);
    let authority = block_on(metadata.inner.delegation_state())
        .unwrap()
        .unwrap();
    assert_eq!(
        authority.grants.len(),
        1,
        "checkout oracle never committed a real grant"
    );
    assert!(
        block_on(observer.close_reverse()).is_err(),
        "missing local grant hid pending committed checkout"
    );
    assert_eq!(
        block_on(metadata.inner.delegation_state())
            .unwrap()
            .unwrap()
            .grants
            .len(),
        1
    );
    let grant = metadata.grants.lock().unwrap()[0].clone();
    let loaded = block_on(metadata.inner.load()).unwrap();
    block_on(metadata.inner.checkin(&DelegatedCheckin {
        backing: authority.backing,
        token: grant.token,
        expected_revision: loaded.revision,
    }))
    .unwrap();
}

#[test]
fn cancelled_checkout_retains_actual_filesystem_and_ambiguous_request() {
    checkout_acknowledgement_is_lost(Fault::Suspend);
}
#[test]
fn panicking_checkout_retains_actual_filesystem_and_ambiguous_request() {
    checkout_acknowledgement_is_lost(Fault::Panic);
}

#[test]
fn actual_failed_filesystem_is_retained_even_after_successful_shutdown() {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    let observer = Observer::default();
    let filesystem = block_on(ChunkedFs::open_with_observer(
        metadata.clone(),
        MemoryBlockStore::new(),
        options(),
        Some(&observer),
    ))
    .unwrap();
    filesystem.fail_closed(FsError::new(ErrorCode::Eio).with_syscall("construction-failure"));
    assert!(filesystem.failed());
    block_on(filesystem.shutdown()).unwrap();
    assert!(lease_row(&directory.metadata()).0.is_none());
    assert!(
        block_on(observer.close_reverse()).is_err(),
        "successful shutdown erased sticky construction uncertainty"
    );
    assert_eq!(observer.retained(), 1);
}

#[test]
fn unobserved_open_preserves_existing_best_effort_error_cleanup() {
    let directory = OwnedDirectory::new();
    let metadata = MetadataProbe::new(&directory.metadata());
    metadata.arm(Point::Load, Fault::Error);
    assert!(
        block_on(ChunkedFs::open(
            metadata.clone(),
            MemoryBlockStore::new(),
            options()
        ))
        .is_err()
    );
    assert_eq!(metadata.release_calls(), 1);
    assert!(lease_row(&directory.metadata()).0.is_none());
}
