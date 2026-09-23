use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::chunking::ChunkerConfig;
use mount_rs_core::storage::{
    BlockId, BlockStore, ConcurrentBackingId, ConcurrentModeState, LoadedMetadata, MetadataStore,
    Namespace, WriterLease,
};
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_fault_injection::{
    FaultAction, FaultBlockStore, FaultBoundary, FaultInjector, FaultMetadataStore,
    FaultOccurrence, FaultOperation, FaultOutcome, FaultPhase, FaultPlan, FaultRule, PlanError,
    is_commit_unknown,
};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
struct FakeMetadata {
    state: Arc<Mutex<MetadataState>>,
}

#[derive(Default)]
struct MetadataState {
    publish_calls: u32,
    flush_calls: u32,
    revision: u64,
}

impl FakeMetadata {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MetadataState::default())),
        }
    }

    fn publish_calls(&self) -> u32 {
        self.state.lock().unwrap().publish_calls
    }
}

#[async_trait]
impl MetadataStore for FakeMetadata {
    fn durable(&self) -> bool {
        true
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let revision = self.state.lock().unwrap().revision;
        Ok(LoadedMetadata {
            revision,
            namespace: None,
        })
    }

    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        Ok(ConcurrentModeState::Mrc2(authority()))
    }

    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        if backing == authority() {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Estale))
        }
    }

    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        _namespace: Namespace,
    ) -> Result<u64> {
        if backing != authority() {
            return Err(FsError::new(ErrorCode::Estale));
        }
        let mut state = self.state.lock().unwrap();
        state.publish_calls += 1;
        state.revision = expected_revision + 1;
        Ok(state.revision)
    }

    async fn migrate_mrc1_to_bound_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        if backing == authority() && expected_revision == 7 {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Estale))
        }
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        Ok(WriterLease {
            owner: owner.to_owned(),
            fence: 1,
            expires_at_ms: ttl.as_millis() as u64,
        })
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        Ok(WriterLease {
            expires_at_ms: lease.expires_at_ms + ttl.as_millis() as u64,
            ..lease.clone()
        })
    }

    async fn release_writer(&self, _lease: &WriterLease) -> Result<()> {
        Ok(())
    }

    async fn publish(
        &self,
        expected_revision: u64,
        _lease: &WriterLease,
        _namespace: Namespace,
    ) -> Result<u64> {
        let mut state = self.state.lock().unwrap();
        state.publish_calls += 1;
        state.revision = expected_revision + 1;
        Ok(state.revision)
    }

    async fn flush(&self) -> Result<()> {
        self.state.lock().unwrap().flush_calls += 1;
        Ok(())
    }
}

#[derive(Clone)]
struct FakeBlocks {
    state: Arc<Mutex<BlockState>>,
}

#[derive(Default)]
struct BlockState {
    puts: u32,
    gets: u32,
    flushes: u32,
    deletes: u32,
}

impl FakeBlocks {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(BlockState::default())),
        }
    }

    fn counts(&self) -> (u32, u32, u32, u32) {
        let state = self.state.lock().unwrap();
        (state.puts, state.gets, state.flushes, state.deletes)
    }
}

#[async_trait]
impl BlockStore for FakeBlocks {
    fn durable(&self) -> bool {
        true
    }

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        Ok(authority())
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        if expected == authority() {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Estale))
        }
    }

    async fn get_for_migration(&self, _id: &BlockId) -> Result<Vec<u8>> {
        self.state.lock().unwrap().gets += 1;
        Ok(vec![4, 5, 6])
    }

    async fn put(&self, _bytes: &[u8]) -> Result<BlockId> {
        self.state.lock().unwrap().puts += 1;
        Ok(BlockId("fake-block".to_owned()))
    }

    async fn get(&self, _id: &BlockId) -> Result<Vec<u8>> {
        self.state.lock().unwrap().gets += 1;
        Ok(vec![1, 2, 3])
    }

    async fn flush(&self) -> Result<()> {
        self.state.lock().unwrap().flushes += 1;
        Ok(())
    }

    async fn delete(&self, _id: &BlockId) -> Result<()> {
        self.state.lock().unwrap().deletes += 1;
        Ok(())
    }
}

fn namespace() -> Namespace {
    Namespace {
        format_version: 1,
        root: 1,
        next_inode: 2,
        default_uid: 0,
        default_gid: 0,
        umask: 0,
        default_chunker: ChunkerConfig {
            algorithm: "fixed-size".to_owned(),
            version: 1,
            parameters: BTreeMap::from([("chunk_size".to_owned(), 4096)]),
        },
        nodes: BTreeMap::new(),
    }
}

fn authority() -> ConcurrentBackingId {
    ConcurrentBackingId::from_bytes([0xb1; 16]).unwrap()
}

#[tokio::test]
async fn bound_authority_forwards_and_publish_fault_prevents_inner_commit() {
    let inner = FakeMetadata::new();
    let rule = FaultRule::new(
        FaultBoundary::Metadata,
        FaultOperation::Publish,
        FaultPhase::Before,
        FaultOccurrence::Once,
        FaultAction::Error(ErrorCode::Eio),
    );
    let injector = FaultInjector::new(FaultPlan::new(71, 1, vec![rule]).unwrap()).unwrap();
    let metadata = FaultMetadataStore::new(inner.clone(), injector);
    assert_eq!(
        metadata.concurrent_mode_state().await.unwrap(),
        ConcurrentModeState::Mrc2(authority())
    );
    metadata
        .prepare_bound_concurrent_mode(authority())
        .await
        .unwrap();
    metadata
        .migrate_mrc1_to_bound_mode(authority(), 7)
        .await
        .unwrap();
    code(
        metadata
            .publish_bound_if_revision(authority(), 0, namespace())
            .await,
        ErrorCode::Eio,
    );
    assert_eq!(inner.publish_calls(), 0);

    let inner_blocks = FakeBlocks::new();
    let blocks = FaultBlockStore::new(inner_blocks.clone(), FaultInjector::disabled(72));
    assert_eq!(
        blocks.prepare_concurrent_backing().await.unwrap(),
        authority()
    );
    blocks.verify_concurrent_backing(authority()).await.unwrap();
    assert_eq!(
        blocks
            .get_for_migration(&BlockId("migration".into()))
            .await
            .unwrap(),
        vec![4, 5, 6]
    );
    assert_eq!(inner_blocks.counts().1, 1);

    let read_rule = FaultRule::new(
        FaultBoundary::Blocks,
        FaultOperation::Get,
        FaultPhase::Before,
        FaultOccurrence::Once,
        FaultAction::Error(ErrorCode::Eio),
    );
    let read_fault = FaultInjector::new(FaultPlan::new(73, 1, vec![read_rule]).unwrap()).unwrap();
    let unread = FakeBlocks::new();
    let faulted_blocks = FaultBlockStore::new(unread.clone(), read_fault);
    code(
        faulted_blocks
            .get_for_migration(&BlockId("migration".into()))
            .await,
        ErrorCode::Eio,
    );
    assert_eq!(unread.counts().1, 0);
}

fn code(result: Result<impl Sized>, expected: ErrorCode) {
    let error = match result {
        Ok(_) => panic!("fault should have failed"),
        Err(error) => error,
    };
    assert_eq!(error.code, expected);
}

#[tokio::test]
async fn no_fault_is_transparent_for_metadata_and_blocks() {
    let metadata_inner = FakeMetadata::new();
    let metadata = FaultMetadataStore::new(metadata_inner.clone(), FaultInjector::disabled(11));
    assert!(metadata.durable());
    assert_eq!(metadata.load().await.unwrap().revision, 0);
    let lease = metadata
        .acquire_writer("owner", Duration::from_secs(1))
        .await
        .unwrap();
    metadata
        .renew_writer(&lease, Duration::from_secs(1))
        .await
        .unwrap();
    metadata.publish(0, &lease, namespace()).await.unwrap();
    metadata.flush().await.unwrap();
    metadata.release_writer(&lease).await.unwrap();
    assert_eq!(metadata.injector().trace().events.len(), 0);
    assert_eq!(metadata_inner.publish_calls(), 1);

    let blocks_inner = FakeBlocks::new();
    let blocks = FaultBlockStore::new(blocks_inner.clone(), FaultInjector::disabled(12));
    let id = blocks.put(b"bytes").await.unwrap();
    assert_eq!(blocks.get(&id).await.unwrap(), vec![1, 2, 3]);
    blocks.flush().await.unwrap();
    blocks.delete(&id).await.unwrap();
    assert_eq!(blocks.injector().trace().events.len(), 0);
    assert_eq!(blocks_inner.counts(), (1, 1, 1, 1));
}

#[tokio::test]
async fn fault_wrapped_blocks_reject_unsupported_concurrent_backing_before_conversion() {
    let temp_dir = tempfile::tempdir().expect("create local SQLite metadata directory");
    let metadata = SqliteMetadataStore::open(temp_dir.path().join("metadata.sqlite"))
        .expect("open file-backed metadata");
    let blocks = SqliteBlockStore::in_memory().expect("open volatile block store");
    let wrapped = FaultBlockStore::new(blocks, FaultInjector::disabled(13));
    let options = ChunkedOptions::fixed("fault-wrapped-unsupported-blocks", 4096)
        .expect("valid fixed-size chunker")
        .with_concurrent_writes(true);

    let error = ChunkedFs::open(metadata.clone(), wrapped, options)
        .await
        .err()
        .expect("volatile blocks must reject concurrent mode");
    assert_eq!(error.code, ErrorCode::Enotsup);
    assert_eq!(
        error.syscall.as_deref(),
        Some("prepare concurrent SQLite blocks")
    );

    // The block check runs before the metadata provider persists its MRC1
    // conversion, so the legacy lease path must still work.
    let lease = metadata
        .acquire_writer("legacy-after-rejected-open", Duration::from_secs(60))
        .await
        .expect("metadata was not converted");
    metadata.release_writer(&lease).await.unwrap();
}

#[test]
fn plan_validation_rejects_ambiguous_or_unavailable_faults() {
    let invalid_occurrence = FaultRule::new(
        FaultBoundary::Metadata,
        FaultOperation::Load,
        FaultPhase::Before,
        FaultOccurrence::Nth(0),
        FaultAction::Error(ErrorCode::Eio),
    );
    assert!(matches!(
        FaultPlan::new(1, 1, vec![invalid_occurrence]),
        Err(PlanError::InvalidOccurrence { .. })
    ));

    let invalid_boundary = FaultRule::new(
        FaultBoundary::Blocks,
        FaultOperation::Load,
        FaultPhase::Before,
        FaultOccurrence::Once,
        FaultAction::Error(ErrorCode::Eio),
    );
    assert!(matches!(
        FaultPlan::new(1, 1, vec![invalid_boundary]),
        Err(PlanError::InvalidBoundary { .. })
    ));

    let lost_before_publish = FaultRule::new(
        FaultBoundary::Metadata,
        FaultOperation::Publish,
        FaultPhase::Before,
        FaultOccurrence::Once,
        FaultAction::LostAcknowledgment,
    );
    assert!(matches!(
        FaultPlan::new(1, 1, vec![lost_before_publish]),
        Err(PlanError::InvalidAction { .. })
    ));

    let cas_after_publish = FaultRule::new(
        FaultBoundary::Metadata,
        FaultOperation::Publish,
        FaultPhase::After,
        FaultOccurrence::Once,
        FaultAction::CasConflict,
    );
    assert!(matches!(
        FaultPlan::new(1, 1, vec![cas_after_publish]),
        Err(PlanError::InvalidAction { .. })
    ));

    let duplicate = FaultRule::new(
        FaultBoundary::Metadata,
        FaultOperation::Load,
        FaultPhase::Before,
        FaultOccurrence::Once,
        FaultAction::Error(ErrorCode::Eio),
    );
    assert!(matches!(
        FaultPlan::new(1, 2, vec![duplicate, duplicate]),
        Err(PlanError::DuplicateSelector { .. })
    ));

    let delay = FaultRule::new(
        FaultBoundary::Metadata,
        FaultOperation::Load,
        FaultPhase::Before,
        FaultOccurrence::Once,
        FaultAction::DelayMs(1),
    );
    #[cfg(not(feature = "tokio-delay"))]
    assert!(matches!(
        FaultPlan::new(1, 1, vec![delay]),
        Err(PlanError::DelayFeatureDisabled { .. })
    ));
    #[cfg(feature = "tokio-delay")]
    assert!(FaultPlan::new(1, 1, vec![delay]).is_ok());

    assert!(FaultPlan::disabled(99).validate().is_ok());
}

async fn replay_once(
    plan: FaultPlan,
) -> (
    Option<(ErrorCode, Option<String>)>,
    mount_rs_fault_injection::FaultTrace,
) {
    let metadata = FaultMetadataStore::new(FakeMetadata::new(), FaultInjector::new(plan).unwrap());
    let first = metadata.load().await.unwrap();
    assert_eq!(first.revision, 0);
    let second = metadata.load().await;
    let error = second.err().map(|error| (error.code, error.syscall));
    (error, metadata.injector().trace())
}

#[tokio::test]
async fn seeded_replay_is_identical_and_fires_exactly_once() {
    let plan = FaultPlan::new(
        0xfeed_beef,
        3,
        vec![FaultRule::new(
            FaultBoundary::Metadata,
            FaultOperation::Load,
            FaultPhase::Before,
            FaultOccurrence::Nth(2),
            FaultAction::Error(ErrorCode::Eio),
        )],
    )
    .unwrap();
    let (first_error, first_trace) = replay_once(plan.clone()).await;
    let (second_error, second_trace) = replay_once(plan).await;
    assert_eq!(
        first_error,
        Some((
            ErrorCode::Eio,
            Some("fault-injection:metadata.load".to_owned())
        ))
    );
    assert_eq!(first_error, second_error);
    assert_eq!(first_trace, second_trace);
    assert_eq!(first_trace.seed, 0xfeed_beef);
    assert_eq!(first_trace.events.len(), 1);
    assert_eq!(first_trace.events[0].occurrence, 2);
}

#[tokio::test]
async fn concurrent_trace_records_observed_reservation_order_not_seeded_schedule() {
    let seed = 0x1234_5678;
    let plan = FaultPlan::new(
        seed,
        2,
        vec![
            FaultRule::new(
                FaultBoundary::Metadata,
                FaultOperation::Load,
                FaultPhase::Before,
                FaultOccurrence::Once,
                FaultAction::Error(ErrorCode::Eio),
            ),
            FaultRule::new(
                FaultBoundary::Metadata,
                FaultOperation::Flush,
                FaultPhase::Before,
                FaultOccurrence::Once,
                FaultAction::Error(ErrorCode::Eio),
            ),
        ],
    )
    .unwrap();
    let injector = FaultInjector::new(plan).unwrap();

    let (load, flush) = tokio::join!(
        injector.before(FaultBoundary::Metadata, FaultOperation::Load),
        injector.before(FaultBoundary::Metadata, FaultOperation::Flush),
    );
    code(load, ErrorCode::Eio);
    code(flush, ErrorCode::Eio);

    let trace = injector.trace();
    assert_eq!(trace.seed, seed);
    assert_eq!(
        trace
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(
        trace
            .events
            .iter()
            .all(|event| { event.seed == seed && !matches!(event.outcome, FaultOutcome::Pending) })
    );
    assert!(trace.events.iter().any(|event| {
        event.operation == FaultOperation::Load
            && event.phase == FaultPhase::Before
            && event.outcome
                == FaultOutcome::InjectedError {
                    code: ErrorCode::Eio,
                }
    }));
    assert!(trace.events.iter().any(|event| {
        event.operation == FaultOperation::Flush
            && event.phase == FaultPhase::Before
            && event.outcome
                == FaultOutcome::InjectedError {
                    code: ErrorCode::Eio,
                }
    }));
}

#[tokio::test]
async fn cancelled_after_publish_keeps_pending_evidence_after_provider_side_effect() {
    let plan = FaultPlan::new(
        21,
        1,
        vec![FaultRule::new(
            FaultBoundary::Metadata,
            FaultOperation::Publish,
            FaultPhase::After,
            FaultOccurrence::Once,
            FaultAction::LostAcknowledgment,
        )],
    )
    .unwrap();
    let injector = FaultInjector::new(plan).unwrap();
    let side_effects = Arc::new(AtomicU32::new(0));
    let provider_started = Arc::new(tokio::sync::Notify::new());
    let task = {
        let injector = injector.clone();
        let side_effects = side_effects.clone();
        let provider_started = provider_started.clone();
        tokio::spawn(async move {
            injector
                .after(
                    FaultBoundary::Metadata,
                    FaultOperation::Publish,
                    || async move {
                        side_effects.fetch_add(1, Ordering::SeqCst);
                        provider_started.notify_one();
                        std::future::pending::<Result<u64>>().await
                    },
                )
                .await
        })
    };

    provider_started.notified().await;
    let pending_trace = injector.trace();
    assert_eq!(pending_trace.events.len(), 1);
    assert_eq!(pending_trace.events[0].outcome, FaultOutcome::Pending);
    assert_eq!(side_effects.load(Ordering::SeqCst), 1);

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let trace = injector.trace();
    assert_eq!(trace.events.len(), 1);
    assert_eq!(trace.events[0].action, FaultAction::LostAcknowledgment);
    assert_eq!(trace.events[0].phase, FaultPhase::After);
    assert_eq!(trace.events[0].outcome, FaultOutcome::Cancelled);
    assert_eq!(side_effects.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "tokio-delay")]
#[tokio::test]
async fn cancelled_feature_enabled_delay_keeps_pending_evidence() {
    let plan = FaultPlan::new(
        22,
        1,
        vec![FaultRule::new(
            FaultBoundary::Blocks,
            FaultOperation::Get,
            FaultPhase::After,
            FaultOccurrence::Once,
            FaultAction::DelayMs(60_000),
        )],
    )
    .unwrap();
    let injector = FaultInjector::new(plan).unwrap();
    let provider_started = Arc::new(tokio::sync::Notify::new());
    let task = {
        let injector = injector.clone();
        let provider_started = provider_started.clone();
        tokio::spawn(async move {
            injector
                .after(FaultBoundary::Blocks, FaultOperation::Get, || async move {
                    provider_started.notify_one();
                    Ok::<u8, FsError>(7)
                })
                .await
        })
    };

    provider_started.notified().await;
    tokio::task::yield_now().await;
    assert_eq!(injector.trace().events[0].outcome, FaultOutcome::Pending);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(injector.trace().events[0].outcome, FaultOutcome::Cancelled);
}

#[tokio::test]
async fn required_errors_and_unknown_publish_are_recorded_without_rollback_claim() {
    let plan = FaultPlan::new(
        7,
        6,
        vec![
            FaultRule::new(
                FaultBoundary::Metadata,
                FaultOperation::Load,
                FaultPhase::Before,
                FaultOccurrence::Once,
                FaultAction::Error(ErrorCode::Eio),
            ),
            FaultRule::new(
                FaultBoundary::Blocks,
                FaultOperation::Put,
                FaultPhase::Before,
                FaultOccurrence::Once,
                FaultAction::Error(ErrorCode::Enospc),
            ),
            FaultRule::new(
                FaultBoundary::Metadata,
                FaultOperation::RenewWriter,
                FaultPhase::Before,
                FaultOccurrence::Once,
                FaultAction::LeaseFailure,
            ),
            FaultRule::new(
                FaultBoundary::Metadata,
                FaultOperation::Publish,
                FaultPhase::Before,
                FaultOccurrence::Once,
                FaultAction::CasConflict,
            ),
            FaultRule::new(
                FaultBoundary::Metadata,
                FaultOperation::Flush,
                FaultPhase::Before,
                FaultOccurrence::Once,
                FaultAction::Error(ErrorCode::Eio),
            ),
            FaultRule::new(
                FaultBoundary::Metadata,
                FaultOperation::Publish,
                FaultPhase::After,
                FaultOccurrence::Once,
                FaultAction::LostAcknowledgment,
            ),
        ],
    )
    .unwrap();
    let injector = FaultInjector::new(plan).unwrap();
    let metadata_inner = FakeMetadata::new();
    let metadata = FaultMetadataStore::new(metadata_inner.clone(), injector.clone());
    let blocks_inner = FakeBlocks::new();
    let blocks = FaultBlockStore::new(blocks_inner.clone(), injector.clone());

    code(metadata.load().await, ErrorCode::Eio);
    code(blocks.put(b"full block").await, ErrorCode::Enospc);
    let lease = metadata
        .acquire_writer("owner", Duration::from_secs(1))
        .await
        .unwrap();
    code(
        metadata.renew_writer(&lease, Duration::from_secs(1)).await,
        ErrorCode::Estale,
    );
    code(
        metadata.publish(0, &lease, namespace()).await,
        ErrorCode::Eagain,
    );
    code(metadata.flush().await, ErrorCode::Eio);

    let unknown = metadata
        .publish(0, &lease, namespace())
        .await
        .expect_err("after-publish lost acknowledgment must be observable");
    assert!(is_commit_unknown(&unknown));
    assert_eq!(metadata_inner.publish_calls(), 1);
    assert_eq!(blocks_inner.counts().0, 0);

    let trace = injector.trace();
    assert_eq!(trace.events.len(), 6);
    assert_eq!(
        trace.events[0].outcome,
        FaultOutcome::InjectedError {
            code: ErrorCode::Eio
        }
    );
    assert_eq!(
        trace.events[1].outcome,
        FaultOutcome::InjectedError {
            code: ErrorCode::Enospc
        }
    );
    assert_eq!(trace.events[2].outcome, FaultOutcome::LeaseFailure);
    assert_eq!(trace.events[3].outcome, FaultOutcome::CasConflict);
    assert_eq!(
        trace.events[4].outcome,
        FaultOutcome::InjectedError {
            code: ErrorCode::Eio
        }
    );
    assert_eq!(trace.events[5].outcome, FaultOutcome::CommitUnknown);
}

#[allow(dead_code)]
fn _error_type_is_used_for_public_contract() -> FsError {
    FsError::new(ErrorCode::Eio)
}
