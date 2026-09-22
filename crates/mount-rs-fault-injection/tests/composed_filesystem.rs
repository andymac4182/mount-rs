use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::driver::FsDriver;
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{ErrorCode, Result};
use mount_rs_fault_injection::{
    COMMIT_UNKNOWN_MESSAGE, FaultAction, FaultBlockStore, FaultBoundary, FaultInjector,
    FaultMetadataStore, FaultOccurrence, FaultOperation, FaultOutcome, FaultPhase, FaultPlan,
    FaultRule,
};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

const BASELINE: &[u8] = b"baseline-payload-crossing-fixed-chunks";
const UPDATED: &[u8] = b"updated-payload-crossing-fixed-chunks";

#[derive(Clone)]
struct DynMetadata(Arc<dyn MetadataStore>);

#[async_trait]
impl MetadataStore for DynMetadata {
    fn durable(&self) -> bool {
        self.0.durable()
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        self.0.load().await
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        self.0.acquire_writer(owner, ttl).await
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        self.0.renew_writer(lease, ttl).await
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        self.0.release_writer(lease).await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        self.0.publish(expected_revision, lease, namespace).await
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }
}

#[derive(Clone)]
struct DynBlocks(Arc<dyn BlockStore>);

#[async_trait]
impl BlockStore for DynBlocks {
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

enum Backend {
    Memory {
        metadata: MemoryMetadataStore,
        blocks: MemoryBlockStore,
    },
    Sqlite {
        temp_dir: TempDir,
        metadata_path: PathBuf,
        blocks_path: PathBuf,
    },
}

impl Backend {
    fn memory() -> Self {
        Self::Memory {
            metadata: MemoryMetadataStore::new(),
            blocks: MemoryBlockStore::new(),
        }
    }

    fn sqlite() -> Self {
        let temp_dir = TempDir::new().expect("create scoped SQLite test directory");
        let metadata_path = temp_dir.path().join("metadata.sqlite");
        let blocks_path = temp_dir.path().join("blocks.sqlite");
        Self::Sqlite {
            temp_dir,
            metadata_path,
            blocks_path,
        }
    }

    fn open(&self) -> Result<(DynMetadata, DynBlocks)> {
        match self {
            Self::Memory { metadata, blocks } => Ok((
                DynMetadata(Arc::new(metadata.clone())),
                DynBlocks(Arc::new(blocks.clone())),
            )),
            Self::Sqlite {
                metadata_path,
                blocks_path,
                ..
            } => Ok((
                DynMetadata(Arc::new(SqliteMetadataStore::open(metadata_path)?)),
                DynBlocks(Arc::new(SqliteBlockStore::open(blocks_path)?)),
            )),
        }
    }

    fn close(self) -> std::io::Result<()> {
        match self {
            Self::Memory { .. } => Ok(()),
            Self::Sqlite { temp_dir, .. } => temp_dir.close(),
        }
    }
}

type FaultedFs = ChunkedFs<FaultMetadataStore<DynMetadata>, FaultBlockStore<DynBlocks>>;

fn options(owner: &str) -> ChunkedOptions {
    ChunkedOptions::fixed(owner, 4)
        .expect("fixed test chunker is valid")
        .with_lease_ttl(Duration::from_secs(30))
}

async fn open_faulted(
    backend: &Backend,
    plan: FaultPlan,
    owner: &str,
) -> Result<(FaultedFs, FaultInjector, DynMetadata)> {
    let (metadata, blocks) = backend.open()?;
    let injector = FaultInjector::new(plan).expect("test fault plan is valid");
    let filesystem = ChunkedFs::open(
        FaultMetadataStore::new(metadata.clone(), injector.clone()),
        FaultBlockStore::new(blocks, injector.clone()),
        options(owner),
    )
    .await?;
    Ok((filesystem, injector, metadata))
}

async fn seed_file(backend: &Backend, owner: &str, bytes: &[u8]) -> Result<Namespace> {
    let (filesystem, _, metadata) = open_faulted(backend, FaultPlan::disabled(1), owner).await?;
    let file = filesystem.open("/payload", "w+", 0o640).await?;
    assert_eq!(file.write(bytes, Some(0)).await?, bytes.len());
    file.close().await?;
    let namespace = metadata
        .load()
        .await?
        .namespace
        .expect("seeded filesystem has a namespace");
    filesystem.shutdown().await?;
    Ok(namespace)
}

async fn reopen_and_assert(
    backend: &Backend,
    expected_namespace: &Namespace,
    expected_bytes: &[u8],
    owner: &str,
) -> Result<()> {
    let (filesystem, _, metadata) = open_faulted(backend, FaultPlan::disabled(2), owner).await?;
    let loaded = metadata.load().await?;
    assert_namespace_exact(expected_namespace, loaded.namespace.as_ref());

    let file = filesystem.open("/payload", "r", 0).await?;
    let mut actual = vec![0_u8; expected_bytes.len()];
    assert_eq!(file.read(&mut actual, Some(0)).await?, expected_bytes.len());
    assert_eq!(actual, expected_bytes);
    file.close().await?;
    filesystem.shutdown().await
}

fn assert_namespace_exact(expected: &Namespace, actual: Option<&Namespace>) {
    let actual = actual.expect("reopened filesystem has a namespace");
    assert_eq!(
        serde_json::to_vec(expected).expect("expected namespace serializes"),
        serde_json::to_vec(actual).expect("actual namespace serializes")
    );
}

fn one_rule_plan(
    seed: u64,
    boundary: FaultBoundary,
    operation: FaultOperation,
    phase: FaultPhase,
    occurrence: FaultOccurrence,
    action: FaultAction,
) -> FaultPlan {
    FaultPlan::new(
        seed,
        1,
        vec![FaultRule::new(
            boundary, operation, phase, occurrence, action,
        )],
    )
    .expect("test fault plan validates")
}

#[tokio::test]
async fn before_block_put_does_not_publish_partial_write_on_memory_or_sqlite() {
    for backend in [Backend::memory(), Backend::sqlite()] {
        let expected_namespace = seed_file(&backend, "seed-block-put", BASELINE)
            .await
            .unwrap();
        let plan = one_rule_plan(
            10,
            FaultBoundary::Blocks,
            FaultOperation::Put,
            FaultPhase::Before,
            FaultOccurrence::Once,
            FaultAction::Error(ErrorCode::Enospc),
        );
        let (filesystem, injector, metadata) = open_faulted(&backend, plan, "fault-block-put")
            .await
            .unwrap();
        let file = filesystem.open("/payload", "r+", 0).await.unwrap();
        let error = file.write(UPDATED, Some(0)).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Enospc);
        assert!(!filesystem.failed());

        let loaded = metadata.load().await.unwrap();
        assert_namespace_exact(&expected_namespace, loaded.namespace.as_ref());
        let trace = injector.trace();
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0].phase, FaultPhase::Before);
        assert_eq!(trace.events[0].operation, FaultOperation::Put);
        assert_eq!(
            trace.events[0].outcome,
            FaultOutcome::InjectedError {
                code: ErrorCode::Enospc,
            }
        );

        file.close().await.unwrap();
        filesystem.shutdown().await.unwrap();
        drop(file);
        drop(filesystem);
        drop(metadata);
        drop(injector);
        reopen_and_assert(&backend, &expected_namespace, BASELINE, "reopen-block-put")
            .await
            .unwrap();
        backend
            .close()
            .expect("close scoped block-put backend directory");
    }
}

#[tokio::test]
async fn after_successful_publish_lost_ack_reopens_exact_commit_for_memory_and_sqlite() {
    for backend in [Backend::memory(), Backend::sqlite()] {
        let _baseline_namespace = seed_file(&backend, "seed-lost-ack", BASELINE)
            .await
            .unwrap();
        let plan = one_rule_plan(
            11,
            FaultBoundary::Metadata,
            FaultOperation::Publish,
            FaultPhase::After,
            FaultOccurrence::Once,
            FaultAction::LostAcknowledgment,
        );
        let (filesystem, injector, metadata) = open_faulted(&backend, plan, "fault-lost-ack")
            .await
            .unwrap();
        let file = filesystem.open("/payload", "r+", 0).await.unwrap();
        let error = file.write(UPDATED, Some(0)).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        assert!(error.to_string().contains(COMMIT_UNKNOWN_MESSAGE));
        assert!(filesystem.failed());

        let committed_namespace = metadata
            .load()
            .await
            .unwrap()
            .namespace
            .expect("successful provider publish remains visible");
        let trace = injector.trace();
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0].phase, FaultPhase::After);
        assert_eq!(trace.events[0].operation, FaultOperation::Publish);
        assert_eq!(trace.events[0].outcome, FaultOutcome::CommitUnknown);

        let stale_handle = file.stat().await.unwrap_err();
        assert_eq!(stale_handle.code, ErrorCode::Eio);
        file.close().await.unwrap();
        filesystem.shutdown().await.unwrap();
        drop(file);
        drop(filesystem);
        drop(metadata);
        drop(injector);
        reopen_and_assert(&backend, &committed_namespace, UPDATED, "reopen-lost-ack")
            .await
            .unwrap();
        backend
            .close()
            .expect("close scoped lost-ack backend directory");
    }
}

#[tokio::test]
async fn metadata_flush_error_fails_closed_but_reopens_published_data() {
    for backend in [Backend::memory(), Backend::sqlite()] {
        let _baseline_namespace = seed_file(&backend, "seed-flush", BASELINE).await.unwrap();
        let plan = one_rule_plan(
            12,
            FaultBoundary::Metadata,
            FaultOperation::Flush,
            FaultPhase::Before,
            FaultOccurrence::Once,
            FaultAction::Error(ErrorCode::Eio),
        );
        let (filesystem, injector, metadata) =
            open_faulted(&backend, plan, "fault-flush").await.unwrap();
        let file = filesystem.open("/payload", "r+", 0).await.unwrap();
        let error = file.write(UPDATED, Some(0)).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        assert!(filesystem.failed());

        let committed_namespace = metadata
            .load()
            .await
            .unwrap()
            .namespace
            .expect("metadata publish precedes the failing barrier");
        let trace = injector.trace();
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0].operation, FaultOperation::Flush);
        assert_eq!(trace.events[0].boundary, FaultBoundary::Metadata);
        assert_eq!(
            trace.events[0].outcome,
            FaultOutcome::InjectedError {
                code: ErrorCode::Eio,
            }
        );

        let failed_handle = file.stat().await.unwrap_err();
        assert_eq!(failed_handle.code, ErrorCode::Eio);
        file.close().await.unwrap();
        filesystem.shutdown().await.unwrap();
        drop(file);
        drop(filesystem);
        drop(metadata);
        drop(injector);
        reopen_and_assert(&backend, &committed_namespace, UPDATED, "reopen-flush")
            .await
            .unwrap();
        backend
            .close()
            .expect("close scoped flush backend directory");
    }
}

#[tokio::test]
async fn lease_failure_fails_existing_handles_closed_without_mutation() {
    for backend in [Backend::memory(), Backend::sqlite()] {
        let expected_namespace = seed_file(&backend, "seed-lease", BASELINE).await.unwrap();
        let plan = one_rule_plan(
            13,
            FaultBoundary::Metadata,
            FaultOperation::RenewWriter,
            FaultPhase::Before,
            FaultOccurrence::Nth(2),
            FaultAction::LeaseFailure,
        );
        let (filesystem, injector, metadata) =
            open_faulted(&backend, plan, "fault-lease").await.unwrap();
        let file = filesystem.open("/payload", "r+", 0).await.unwrap();
        let stale = file.stat().await.unwrap_err();
        assert_eq!(stale.code, ErrorCode::Estale);
        assert!(filesystem.failed());

        let stale_write = file.write(UPDATED, Some(0)).await.unwrap_err();
        assert_eq!(stale_write.code, ErrorCode::Estale);
        let loaded = metadata.load().await.unwrap();
        assert_namespace_exact(&expected_namespace, loaded.namespace.as_ref());
        let trace = injector.trace();
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0].operation, FaultOperation::RenewWriter);
        assert_eq!(trace.events[0].outcome, FaultOutcome::LeaseFailure);

        file.close().await.unwrap();
        filesystem.shutdown().await.unwrap();
        drop(file);
        drop(filesystem);
        drop(metadata);
        drop(injector);
        reopen_and_assert(&backend, &expected_namespace, BASELINE, "reopen-lease")
            .await
            .unwrap();
        backend
            .close()
            .expect("close scoped lease backend directory");
    }
}
