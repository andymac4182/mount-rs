//! Mount-independent version coordinator and immutable historical driver.
//!
//! The coordinator does not reimplement a mutable filesystem. A caller,
//! normally the chunked coordinator, publishes the current namespace through
//! the additive VersionedMetadataStore capability. This crate supplies the
//! publication protocol, history/pin operations, and a read-only driver that
//! resolves bytes directly from immutable blocks.

use async_trait::async_trait;
use mount_rs_core::driver::{FileHandle, FsDriver};
use mount_rs_core::error::{ErrorCode, FsError, Result};
use mount_rs_core::handle::OpenFlags;
use mount_rs_core::path::{normalize_path, split_path};
use mount_rs_core::storage::{
    BlockId, BlockStore, FileLayout, MetadataStore, Namespace, NodeData, NodeMetadata, WriterLease,
};
use mount_rs_core::types::{Capabilities, DirEntry, MkdirOptions, Stats, StatsFs};
use mount_rs_core::versioning::{
    BlockStoreId, PublicationId, ReadLease, ReadLeaseRequest, VersionHead, VersionId, VersionInfo,
    VersionKind, VersionPublication, VersionedMetadataStore,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const MAX_SYMLINK_DEPTH: usize = 40;
const BLOCK_SIZE: u64 = 4096;
static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct VersionedOptions {
    pub owner: String,
    pub lease_ttl: Duration,
    pub view_ttl: Duration,
    pub block_store_id: BlockStoreId,
}

impl VersionedOptions {
    pub fn new(owner: impl Into<String>, block_store_id: BlockStoreId) -> Self {
        Self {
            owner: owner.into(),
            lease_ttl: Duration::from_secs(30),
            view_ttl: Duration::from_secs(60),
            block_store_id,
        }
    }

    pub fn with_lease_ttl(mut self, lease_ttl: Duration) -> Self {
        self.lease_ttl = lease_ttl;
        self
    }

    pub fn with_view_ttl(mut self, view_ttl: Duration) -> Self {
        self.view_ttl = view_ttl;
        self
    }
}

#[derive(Clone)]
pub struct VersionedCoordinator<M, B>
where
    M: VersionedMetadataStore + Clone,
    B: BlockStore + Clone,
{
    metadata: M,
    blocks: B,
    options: VersionedOptions,
}

impl<M, B> VersionedCoordinator<M, B>
where
    M: VersionedMetadataStore + Clone + 'static,
    B: BlockStore + Clone + 'static,
{
    pub fn new(metadata: M, blocks: B, options: VersionedOptions) -> Result<Self> {
        if options.owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("version coordinator owner must not be empty"));
        }
        Ok(Self {
            metadata,
            blocks,
            options,
        })
    }

    pub fn volume_id(&self) -> mount_rs_core::versioning::VolumeId {
        self.metadata.volume_id()
    }

    pub fn metadata(&self) -> &M {
        &self.metadata
    }

    pub fn blocks(&self) -> &B {
        &self.blocks
    }

    pub async fn history(&self) -> Result<Vec<VersionInfo>> {
        self.metadata.list_versions().await
    }

    pub async fn head(&self) -> Result<Option<VersionHead>> {
        self.metadata.version_head().await
    }

    /// Capture the provider current namespace. Callers that also own a
    /// mutable filesystem must quiesce it before invoking this method.
    pub async fn snapshot(&self) -> Result<VersionInfo> {
        self.snapshot_with_publication_id(next_operation_id(&self.options.owner)?)
            .await
    }

    /// Capture the current namespace using a caller-owned retry identity.
    /// Reusing the same ID after an ambiguous response first reconciles the
    /// committed record, even if the current head has advanced since the
    /// original capture.
    pub async fn snapshot_with_publication_id(
        &self,
        operation_id: PublicationId,
    ) -> Result<VersionInfo> {
        if let Some(existing) = self.metadata.find_publication(&operation_id).await? {
            if existing.kind != VersionKind::Snapshot {
                return Err(FsError::new(ErrorCode::Eexist)
                    .with_syscall("snapshot")
                    .with_message("publication id was reused for another operation"));
            }
            return Ok(existing);
        }
        let lease = self
            .metadata
            .acquire_writer(&self.options.owner, self.options.lease_ttl)
            .await?;
        let result = async {
            if let Some(existing) = self.metadata.find_publication(&operation_id).await? {
                if existing.kind != VersionKind::Snapshot {
                    return Err(FsError::new(ErrorCode::Eexist)
                        .with_syscall("snapshot")
                        .with_message("publication id was reused for another operation"));
                }
                return Ok(existing);
            }
            self.snapshot_with_lease(&lease, operation_id).await
        }
        .await;
        finish_lease(&self.metadata, lease, result).await
    }

    /// Look up a committed operation by its stable retry identity.
    pub async fn find_publication(
        &self,
        operation_id: &PublicationId,
    ) -> Result<Option<VersionInfo>> {
        self.metadata.find_publication(operation_id).await
    }

    /// Verify a committed operation against the original publication payload.
    /// `Some` is safe to return to the caller; `None` means the provider has
    /// no committed record for the operation yet.
    pub async fn reconcile_publication(
        &self,
        publication: &VersionPublication,
    ) -> Result<Option<VersionInfo>> {
        self.metadata.reconcile_publication(publication).await
    }

    async fn snapshot_with_lease(
        &self,
        lease: &WriterLease,
        operation_id: PublicationId,
    ) -> Result<VersionInfo> {
        let loaded = self.metadata.load().await?;
        loaded.validate()?;
        let namespace = loaded.namespace.ok_or_else(|| {
            FsError::new(ErrorCode::Enoent)
                .with_syscall("snapshot")
                .with_message("cannot snapshot an uninitialized filesystem")
        })?;
        let head = self.metadata.version_head().await?;
        let expected_parent = head.as_ref().map(|value| value.version.clone());
        self.publish_with_lease(
            lease,
            VersionPublication {
                expected_revision: loaded.revision,
                expected_parent,
                operation_id,
                namespace,
                block_store_id: self.options.block_store_id.clone(),
                kind: VersionKind::Snapshot,
                restored_from: None,
                forked_from: None,
                durable: self.metadata.durable() && self.blocks.durable(),
            },
        )
        .await
    }

    /// Publish a caller-supplied namespace under an acquired writer lease.
    /// This is the integration seam for a future coordinator that can hold
    /// its operation gate across the logical snapshot cut.
    pub async fn publish_namespace(
        &self,
        expected_revision: u64,
        expected_parent: Option<VersionId>,
        namespace: Namespace,
    ) -> Result<VersionInfo> {
        self.publish_namespace_with_publication_id(
            expected_revision,
            expected_parent,
            namespace,
            next_operation_id(&self.options.owner)?,
        )
        .await
    }

    /// Publish a caller-supplied namespace with a durable retry identity.
    /// The provider's idempotency transaction is consulted before a writer
    /// lease is acquired, and again inside the provider transaction.
    pub async fn publish_namespace_with_publication_id(
        &self,
        expected_revision: u64,
        expected_parent: Option<VersionId>,
        namespace: Namespace,
        operation_id: PublicationId,
    ) -> Result<VersionInfo> {
        self.publish_publication(VersionPublication {
            expected_revision,
            expected_parent,
            operation_id,
            namespace,
            block_store_id: self.options.block_store_id.clone(),
            kind: VersionKind::Snapshot,
            restored_from: None,
            forked_from: None,
            durable: self.metadata.durable() && self.blocks.durable(),
        })
        .await
    }

    /// Publish an explicit provider publication. This is the low-level seam
    /// for integrations that must retain the exact namespace, CAS revision,
    /// parent, and publication ID across a retry.
    pub async fn publish_publication(
        &self,
        publication: VersionPublication,
    ) -> Result<VersionInfo> {
        if publication.block_store_id != self.options.block_store_id {
            return Err(FsError::new(ErrorCode::Exdev)
                .with_syscall("publish version")
                .with_message("publication uses a different block store"));
        }
        publication.validate(&self.volume_id())?;
        if let Some(existing) = self.reconcile_publication(&publication).await? {
            return Ok(existing);
        }
        self.blocks.flush().await?;
        let lease = self
            .metadata
            .acquire_writer(&self.options.owner, self.options.lease_ttl)
            .await?;
        let result = self.publish_with_lease(&lease, publication).await;
        finish_lease(&self.metadata, lease, result).await
    }

    async fn publish_with_lease(
        &self,
        lease: &WriterLease,
        publication: VersionPublication,
    ) -> Result<VersionInfo> {
        publication.validate(&self.volume_id())?;
        self.blocks.flush().await?;
        self.metadata.publish_version(lease, publication).await
    }

    pub async fn view(&self, id: &VersionId) -> Result<VersionedView<M, B>> {
        let pin = self
            .metadata
            .open_view_pin(
                id,
                ReadLeaseRequest {
                    owner: self.options.owner.clone(),
                    ttl: self.options.view_ttl,
                },
            )
            .await?;
        let record = match self.metadata.load_version(id).await {
            Ok(record) => record,
            Err(error) => {
                let _ = self.metadata.close_view_pin(&pin).await;
                return Err(error);
            }
        };
        if record.block_store_id != self.options.block_store_id {
            let cleanup = self.metadata.close_view_pin(&pin).await;
            cleanup?;
            return Err(FsError::new(ErrorCode::Exdev)
                .with_syscall("open version view")
                .with_message("historical view uses a different block store"));
        }
        Ok(VersionedView::new(
            self.metadata.clone(),
            self.blocks.clone(),
            record,
            pin,
            self.options.view_ttl,
        ))
    }

    pub async fn restore(&self, id: &VersionId) -> Result<VersionInfo> {
        self.restore_with_publication_id(id, next_operation_id(&self.options.owner)?)
            .await
    }

    /// Restore a retained version with a caller-owned retry identity.
    pub async fn restore_with_publication_id(
        &self,
        id: &VersionId,
        operation_id: PublicationId,
    ) -> Result<VersionInfo> {
        if let Some(existing) = self.metadata.find_publication(&operation_id).await? {
            if existing.kind != VersionKind::Restore
                || existing.restored_from.as_ref() != Some(id)
                || existing.block_store_id != self.options.block_store_id
            {
                return Err(FsError::new(ErrorCode::Eexist)
                    .with_syscall("restore")
                    .with_message("publication id was reused for another restore source"));
            }
            return Ok(existing);
        }
        let selected = self.metadata.load_version(id).await?;
        if selected.block_store_id != self.options.block_store_id {
            return Err(FsError::new(ErrorCode::Exdev)
                .with_syscall("restore")
                .with_message("restore source uses a different block store"));
        }
        let loaded = self.metadata.load().await?;
        loaded.validate()?;
        let head = self.metadata.version_head().await?;
        self.publish_publication(VersionPublication {
            expected_revision: loaded.revision,
            expected_parent: head.map(|value| value.version),
            operation_id,
            namespace: selected.namespace,
            block_store_id: self.options.block_store_id.clone(),
            kind: VersionKind::Restore,
            restored_from: Some(selected.id),
            forked_from: None,
            durable: self.metadata.durable() && self.blocks.durable(),
        })
        .await
    }

    pub async fn delete(&self, id: &VersionId) -> Result<()> {
        let lease = self
            .metadata
            .acquire_writer(&self.options.owner, self.options.lease_ttl)
            .await?;
        let result = self.metadata.delete_version(&lease, id).await;
        finish_lease(&self.metadata, lease, result).await
    }

    /// Fork a retained version into another coordinator. Block-store labels
    /// qualify manifest references, but are not a runtime capability proof;
    /// every referenced block is therefore copied and the target extents are
    /// rewritten before the target head is activated. The convenience wrapper
    /// generates its retry identity before any block copy begins; callers that
    /// must retry an ambiguous response should use the explicit-ID method.
    pub async fn fork_into<M2, B2>(
        &self,
        id: &VersionId,
        target: &VersionedCoordinator<M2, B2>,
    ) -> Result<VersionInfo>
    where
        M2: VersionedMetadataStore + Clone + 'static,
        B2: BlockStore + Clone + 'static,
    {
        let operation_id = next_operation_id(&target.options.owner)?;
        self.fork_into_with_publication_id(id, target, operation_id)
            .await
    }

    /// Fork a retained version using a caller-owned publication identity.
    ///
    /// The operation ID is looked up before copying blocks, so a retry with a
    /// fresh target coordinator can reconcile a committed fork even when the
    /// original publish response was lost. A replay is accepted only when its
    /// committed record identifies this source version, the fork operation,
    /// and the target block-store identity.
    pub async fn fork_into_with_publication_id<M2, B2>(
        &self,
        id: &VersionId,
        target: &VersionedCoordinator<M2, B2>,
        operation_id: PublicationId,
    ) -> Result<VersionInfo>
    where
        M2: VersionedMetadataStore + Clone + 'static,
        B2: BlockStore + Clone + 'static,
    {
        if operation_id.as_str().is_empty() || operation_id.as_str().contains('\0') {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("fork")
                .with_message("fork publication id must be non-empty"));
        }
        let source_view = self.view(id).await?;
        let result = async {
            source_view.renew(self.options.view_ttl).await?;
            let source = source_view.record.clone();
            source.validate_for_volume(&self.volume_id())?;
            if source.id != *id {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("fork")
                    .with_message(
                        "fork source record identity does not match the requested version",
                    ));
            }
            if source.block_store_id != self.options.block_store_id {
                return Err(FsError::new(ErrorCode::Exdev)
                    .with_syscall("fork")
                    .with_message("fork source uses a different block store"));
            }
            if let Some(existing) = target.metadata.find_publication(&operation_id).await? {
                return validate_replayed_fork(
                    existing,
                    &source,
                    &target.volume_id(),
                    &target.options.block_store_id,
                );
            }
            let namespace = source_view.copy_namespace_to(&target.blocks).await?;
            let lease = target
                .metadata
                .acquire_writer(&target.options.owner, target.options.lease_ttl)
                .await?;
            let result = async {
                let loaded = target.metadata.load().await?;
                loaded.validate()?;
                let head = target.metadata.version_head().await?;
                let publication = VersionPublication {
                    expected_revision: loaded.revision,
                    expected_parent: head.map(|value| value.version),
                    operation_id: operation_id.clone(),
                    namespace,
                    block_store_id: target.options.block_store_id.clone(),
                    kind: VersionKind::Fork,
                    restored_from: None,
                    forked_from: Some(source.id.clone()),
                    durable: target.metadata.durable() && target.blocks.durable(),
                };
                if let Some(existing) = target.metadata.reconcile_publication(&publication).await? {
                    return Ok(existing);
                }
                if loaded.namespace.is_some() {
                    return Err(FsError::new(ErrorCode::Eexist)
                        .with_syscall("fork")
                        .with_message("fork target is already initialized"));
                }
                target.publish_with_lease(&lease, publication).await
            }
            .await;
            finish_lease(&target.metadata, lease, result).await
        }
        .await;
        let close_result = source_view.close().await;
        match result {
            Err(error) => Err(error),
            Ok(value) => {
                close_result?;
                Ok(value)
            }
        }
    }
}

fn validate_replayed_fork(
    existing: VersionInfo,
    source: &VersionInfo,
    target_volume: &mount_rs_core::versioning::VolumeId,
    target_block_store_id: &BlockStoreId,
) -> Result<VersionInfo> {
    existing.validate_for_volume(target_volume)?;
    if existing.kind != VersionKind::Fork
        || existing.forked_from.as_ref() != Some(&source.id)
        || &existing.block_store_id != target_block_store_id
    {
        return Err(FsError::new(ErrorCode::Eexist)
            .with_syscall("fork")
            .with_message("fork publication id was reused for another payload"));
    }
    Ok(existing)
}

async fn finish_lease<M, T>(metadata: &M, lease: WriterLease, result: Result<T>) -> Result<T>
where
    M: MetadataStore,
{
    let release = metadata.release_writer(&lease).await;
    match result {
        Err(error) => Err(error),
        Ok(value) => {
            release?;
            Ok(value)
        }
    }
}

fn next_operation_id(owner: &str) -> Result<PublicationId> {
    let sequence = NEXT_OPERATION.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            FsError::new(ErrorCode::Eio).with_message("system clock is before Unix epoch")
        })?
        .as_nanos();
    // Convenience IDs are process-generated. Durable retry callers should use
    // an explicit PublicationId; the timestamp and sequence avoid accidental
    // reuse across normal restarts and calls without claiming a durable UUID.
    PublicationId::new(format!(
        "{owner}-{}-{timestamp}-{sequence}",
        std::process::id()
    ))
}

struct ViewState {
    pin: Option<ReadLease>,
    closed: bool,
    handles: usize,
}

struct ViewLifetime<M>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
{
    metadata: M,
    default_ttl: Duration,
    state: Mutex<ViewState>,
    invalidated: AtomicBool,
    operations: AtomicUsize,
}

impl<M> ViewLifetime<M>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
{
    fn new(metadata: M, pin: ReadLease, default_ttl: Duration) -> Self {
        Self {
            metadata,
            default_ttl,
            state: Mutex::new(ViewState {
                pin: Some(pin),
                closed: false,
                handles: 0,
            }),
            invalidated: AtomicBool::new(false),
            operations: AtomicUsize::new(0),
        }
    }

    async fn begin(
        self: &Arc<Self>,
        allow_closed: bool,
        syscall: &str,
    ) -> Result<OperationGuard<M>> {
        let mut state = self.state.lock().await;
        if self.invalidated.load(Ordering::Acquire) {
            return Err(view_expired_error(syscall));
        }
        if state.closed && !allow_closed {
            return Err(view_closed_error(syscall));
        }
        let current = state
            .pin
            .as_ref()
            .cloned()
            .ok_or_else(|| view_expired_error(syscall))?;
        self.operations.fetch_add(1, Ordering::AcqRel);
        let guard = OperationGuard::new(Arc::clone(self));
        let renewed = match self
            .metadata
            .renew_view_pin(
                &current,
                ReadLeaseRequest {
                    owner: current.owner.clone(),
                    ttl: self.default_ttl,
                },
            )
            .await
        {
            Ok(renewed) => renewed,
            Err(error) => {
                self.invalidated.store(true, Ordering::Release);
                state.closed = true;
                drop(state);
                drop(guard);
                let _ = self.close().await;
                return Err(error);
            }
        };
        if self.invalidated.load(Ordering::Acquire) {
            state.closed = true;
            state.pin = Some(renewed);
            drop(state);
            drop(guard);
            let _ = self.close().await;
            return Err(view_expired_error(syscall));
        }
        state.pin = Some(renewed);
        Ok(guard)
    }

    async fn renew(&self, ttl: Duration) -> Result<()> {
        let mut state = self.state.lock().await;
        if self.invalidated.load(Ordering::Acquire) {
            return Err(view_expired_error("renew view"));
        }
        if state.closed {
            return Err(view_closed_error("renew view"));
        }
        let current = state
            .pin
            .as_ref()
            .cloned()
            .ok_or_else(|| view_expired_error("renew view"))?;
        let renewed = match self
            .metadata
            .renew_view_pin(
                &current,
                ReadLeaseRequest {
                    owner: current.owner.clone(),
                    ttl,
                },
            )
            .await
        {
            Ok(renewed) => renewed,
            Err(error) => {
                self.invalidated.store(true, Ordering::Release);
                state.closed = true;
                drop(state);
                let _ = self.close().await;
                return Err(error);
            }
        };
        if self.invalidated.load(Ordering::Acquire) {
            state.closed = true;
            state.pin = Some(renewed);
            drop(state);
            let _ = self.close().await;
            return Err(view_expired_error("renew view"));
        }
        state.pin = Some(renewed);
        Ok(())
    }

    async fn prepare_finish(&self, syscall: &str) -> FinishAction {
        let mut state = self.state.lock().await;
        let renewal = if self.invalidated.load(Ordering::Acquire) {
            None
        } else if let Some(current) = state.pin.as_ref().cloned() {
            Some(
                self.metadata
                    .renew_view_pin(
                        &current,
                        ReadLeaseRequest {
                            owner: current.owner.clone(),
                            ttl: self.default_ttl,
                        },
                    )
                    .await,
            )
        } else {
            None
        };
        let mut pin = None;
        let result = match renewal {
            Some(Ok(renewed)) if !self.invalidated.load(Ordering::Acquire) => {
                state.pin = Some(renewed);
                Ok(())
            }
            Some(Ok(renewed)) => {
                self.invalidated.store(true, Ordering::Release);
                state.closed = true;
                pin = Some(renewed);
                state.pin = None;
                Err(view_expired_error(syscall))
            }
            Some(Err(error)) => {
                self.invalidated.store(true, Ordering::Release);
                state.closed = true;
                pin = state.pin.take();
                Err(error)
            }
            None => {
                self.invalidated.store(true, Ordering::Release);
                state.closed = true;
                pin = state.pin.take();
                Err(view_expired_error(syscall))
            }
        };
        let previous = self.operations.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "operation guard must account for begin");
        if state.closed && state.handles == 0 && self.operations.load(Ordering::Acquire) == 0 {
            if pin.is_none() {
                pin = state.pin.take();
            } else {
                state.pin = None;
            }
        }
        FinishAction { result, pin }
    }

    fn cancel_operation(&self) {
        let previous = self.operations.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "operation guard must account for begin");
        self.invalidated.store(true, Ordering::Release);
    }

    async fn with_operation<T, F, Fut>(
        self: &Arc<Self>,
        allow_closed: bool,
        syscall: &str,
        operation: F,
    ) -> Result<T>
    where
        T: Send,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>> + Send,
    {
        let guard = self.begin(allow_closed, syscall).await?;
        let result = operation().await;
        let finish = guard.finish(syscall).await;
        match result {
            Err(error) => Err(error),
            Ok(value) => {
                finish?;
                Ok(value)
            }
        }
    }

    async fn acquire_handle(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        if self.invalidated.load(Ordering::Acquire) {
            return Err(view_expired_error("open"));
        }
        if state.closed {
            return Err(view_closed_error("open"));
        }
        if state.pin.is_none() {
            return Err(view_expired_error("open"));
        }
        state.handles += 1;
        Ok(())
    }

    async fn release_handle(&self) -> Result<()> {
        let pin = {
            let mut state = self.state.lock().await;
            if state.handles == 0 {
                return Ok(());
            }
            state.handles -= 1;
            if state.closed && state.handles == 0 && self.operations.load(Ordering::Acquire) == 0 {
                state.pin.take()
            } else {
                None
            }
        };
        if let Some(pin) = pin
            && let Err(error) = self.release_pin(pin).await
        {
            // The handle is still logically open when its final pin release
            // fails; restore the count so the caller's explicit close can
            // retry the provider operation.
            let mut state = self.state.lock().await;
            state.handles += 1;
            return Err(error);
        }
        Ok(())
    }

    async fn abandon_handle(&self) -> Result<()> {
        let pin = {
            let mut state = self.state.lock().await;
            if state.handles == 0 {
                return Ok(());
            }
            state.handles -= 1;
            if state.closed && state.handles == 0 && self.operations.load(Ordering::Acquire) == 0 {
                state.pin.take()
            } else {
                None
            }
        };
        if let Some(pin) = pin {
            self.release_pin(pin).await?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        let pin = {
            let mut state = self.state.lock().await;
            state.closed = true;
            if state.handles == 0 && self.operations.load(Ordering::Acquire) == 0 {
                state.pin.take()
            } else {
                None
            }
        };
        if let Some(pin) = pin {
            self.release_pin(pin).await?;
        }
        Ok(())
    }

    async fn release_pin(&self, pin: ReadLease) -> Result<()> {
        match self.metadata.close_view_pin(&pin).await {
            Ok(()) => Ok(()),
            Err(error) if error.is(ErrorCode::Estale) => Ok(()),
            Err(error) => {
                // A provider error is not proof that the pin was removed.
                // Keep the exact token so an explicit close can retry. If a
                // concurrent renewal installed a newer token, do not replace
                // that newer state with the failed release token.
                let mut state = self.state.lock().await;
                if state.pin.is_none() {
                    state.pin = Some(pin);
                }
                Err(error)
            }
        }
    }
}

struct FinishAction {
    result: Result<()>,
    pin: Option<ReadLease>,
}

struct OperationGuard<M>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
{
    lifetime: Arc<ViewLifetime<M>>,
    active: bool,
}

impl<M> OperationGuard<M>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
{
    fn new(lifetime: Arc<ViewLifetime<M>>) -> Self {
        Self {
            lifetime,
            active: true,
        }
    }

    async fn finish(mut self, syscall: &str) -> Result<()> {
        let action = self.lifetime.prepare_finish(syscall).await;
        // prepare_finish decrements the operation count before returning and
        // performs no further await. Disarm before asynchronous pin cleanup so
        // cancellation cannot decrement the same operation twice.
        self.active = false;
        if let Some(pin) = action.pin {
            self.lifetime.release_pin(pin).await?;
        }
        action.result
    }
}

impl<M> Drop for OperationGuard<M>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        if self.active {
            // Drop is the cancellation path for both the user operation and
            // begin/finish lease renewal. It synchronously balances the local
            // count and invalidates the view; an explicit close then releases
            // the provider pin without relying on an async Drop implementation.
            self.lifetime.cancel_operation();
        }
    }
}

/// Read-only historical view backed by a provider read-retention pin.
///
/// Rust cannot await provider cleanup from `Drop`, so dropping this value
/// without calling [`Self::close`] may retain the pin until its TTL. Explicit
/// close is retryable after a provider error and must be used when prompt
/// retention release matters.
pub struct VersionedView<M, B>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
    B: BlockStore + Clone,
{
    blocks: B,
    record: VersionInfo,
    lifetime: Arc<ViewLifetime<M>>,
}

impl<M, B> VersionedView<M, B>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
    B: BlockStore + Clone,
{
    fn new(
        metadata: M,
        blocks: B,
        record: VersionInfo,
        pin: ReadLease,
        view_ttl: Duration,
    ) -> Self {
        Self {
            blocks,
            record,
            lifetime: Arc::new(ViewLifetime::new(metadata, pin, view_ttl)),
        }
    }

    pub fn info(&self) -> &VersionInfo {
        &self.record
    }

    pub fn version(&self) -> &VersionId {
        &self.record.id
    }

    pub async fn renew(&self, ttl: Duration) -> Result<()> {
        self.lifetime.renew(ttl).await
    }

    async fn copy_namespace_to<B2>(&self, target: &B2) -> Result<Namespace>
    where
        B2: BlockStore,
    {
        self.renew(self.lifetime.default_ttl).await?;
        let mut copied = self.record.namespace.clone();
        let mut blocks: BTreeMap<BlockId, BlockId> = BTreeMap::new();
        for node in copied.nodes.values_mut() {
            let NodeData::File(layout) = &mut node.data else {
                continue;
            };
            for extent in &mut layout.extents {
                let source_id = extent.block.clone();
                let target_id = if let Some(id) = blocks.get(&source_id) {
                    id.clone()
                } else {
                    self.renew(self.lifetime.default_ttl).await?;
                    let bytes = self.blocks.get(&source_id).await?;
                    let id = target.put(&bytes).await?;
                    blocks.insert(source_id, id.clone());
                    self.renew(self.lifetime.default_ttl).await?;
                    id
                };
                extent.block = target_id;
            }
        }
        self.renew(self.lifetime.default_ttl).await?;
        target.flush().await?;
        copied.validate()?;
        Ok(copied)
    }

    /// Close is explicit because Rust cannot await provider cleanup from
    /// `Drop`. Dropping a view can therefore retain its provider pin until
    /// the lease TTL. If close returns a provider error, the pin token is
    /// retained and this method should be retried on the same view.
    pub async fn close(&self) -> Result<()> {
        self.lifetime.close().await
    }

    fn resolve(&self, path: &str, follow_final: bool, syscall: &str) -> Result<u64> {
        resolve_namespace(&self.record.namespace, path, follow_final, syscall, 0)
    }

    fn node(&self, inode: u64, syscall: &str, path: &str) -> Result<&NodeMetadata> {
        self.record
            .namespace
            .nodes
            .get(&inode)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, path))
    }

    async fn read_only<T>(&self, syscall: &str, path: &str) -> Result<T>
    where
        T: Send,
    {
        let error = read_only_error(syscall, path);
        self.lifetime
            .with_operation(false, syscall, || async move { Err(error) })
            .await
    }

    async fn read_only_dest<T>(&self, syscall: &str, path: &str, dest: &str) -> Result<T>
    where
        T: Send,
    {
        let error = FsError::new(ErrorCode::Erofs)
            .with_syscall(syscall)
            .with_path(path)
            .with_dest(dest);
        self.lifetime
            .with_operation(false, syscall, || async move { Err(error) })
            .await
    }
}

fn view_closed_error(syscall: &str) -> FsError {
    FsError::new(ErrorCode::Ebadf)
        .with_syscall(syscall)
        .with_message("historical view is closed")
}

fn view_expired_error(syscall: &str) -> FsError {
    FsError::new(ErrorCode::Estale)
        .with_syscall(syscall)
        .with_message("historical view read lease expired")
}

#[async_trait]
impl<M, B> FsDriver for VersionedView<M, B>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
    B: BlockStore + Clone + Send + Sync + 'static,
{
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            handles: true,
            hardlinks: true,
            symlinks: true,
            permissions: true,
            times: true,
            statfs: true,
            read_only: true,
            case_sensitive: true,
            ..Capabilities::default()
        }
    }

    async fn syncfs(&self) -> Result<()> {
        self.lifetime
            .with_operation(false, "syncfs", || async { Ok(()) })
            .await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.lifetime
            .with_operation(false, "stat", || async {
                let normalized = normalize_path(path);
                let inode = self.resolve(&normalized, true, "stat")?;
                Ok(self.node(inode, "stat", &normalized)?.stats.clone())
            })
            .await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.lifetime
            .with_operation(false, "lstat", || async {
                let normalized = normalize_path(path);
                let inode = self.resolve(&normalized, false, "lstat")?;
                Ok(self.node(inode, "lstat", &normalized)?.stats.clone())
            })
            .await
    }

    async fn statfs(&self, path: &str) -> Result<StatsFs> {
        self.lifetime
            .with_operation(false, "statfs", || async {
                let normalized = normalize_path(path);
                self.resolve(&normalized, true, "statfs")?;
                let files = u64::try_from(self.record.namespace.nodes.len()).unwrap_or(u64::MAX);
                let blocks = self
                    .record
                    .namespace
                    .nodes
                    .values()
                    .map(|node| node.stats.blocks)
                    .sum();
                Ok(StatsFs {
                    filesystem_type: 0x0102_1994,
                    block_size: BLOCK_SIZE,
                    blocks,
                    blocks_free: 0,
                    blocks_available: 0,
                    files,
                    files_free: 0,
                })
            })
            .await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.lifetime
            .with_operation(false, "scandir", || async {
                let normalized = normalize_path(path);
                let inode = self.resolve(&normalized, true, "scandir")?;
                let node = self.node(inode, "scandir", &normalized)?;
                let NodeData::Directory { entries } = &node.data else {
                    return Err(error_with_path(ErrorCode::Enotdir, "scandir", &normalized));
                };
                entries
                    .iter()
                    .map(|entry| {
                        let child = self.node(entry.inode, "scandir", &normalized)?;
                        Ok(DirEntry {
                            name: entry.name.clone(),
                            parent_path: normalized.clone(),
                            file_type: child.stats.file_type(),
                        })
                    })
                    .collect()
            })
            .await
    }

    async fn open(&self, path: &str, flags: &str, _mode: u32) -> Result<Arc<dyn FileHandle>> {
        let operation = self.lifetime.begin(false, "open").await?;
        let normalized = normalize_path(path);
        let result = (|| {
            let parsed = OpenFlags::parse(flags, &normalized)?;
            if parsed.write || parsed.create || parsed.truncate || parsed.append || parsed.exclusive
            {
                return Err(read_only_error("open", &normalized));
            }
            let inode = self.resolve(&normalized, true, "open")?;
            let node = self.node(inode, "open", &normalized)?;
            if node.stats.is_directory() {
                return Err(error_with_path(ErrorCode::Eisdir, "open", &normalized));
            }
            if node.stats.file_type().is_special() {
                return Err(error_with_path(ErrorCode::Enxio, "open", &normalized));
            }
            Ok(inode)
        })();
        let inode = match result {
            Ok(inode) => inode,
            Err(error) => {
                let _ = operation.finish("open").await;
                return Err(error);
            }
        };
        if let Err(error) = self.lifetime.acquire_handle().await {
            let _ = operation.finish("open").await;
            return Err(error);
        }
        if let Err(error) = operation.finish("open").await {
            let _ = self.lifetime.abandon_handle().await;
            return Err(error);
        }
        Ok(Arc::new(HistoricalFileHandle {
            lifetime: Arc::clone(&self.lifetime),
            namespace: Arc::new(self.record.namespace.clone()),
            blocks: self.blocks.clone(),
            inode,
            path: normalized,
            state: Mutex::new(HandleState {
                position: 0,
                closed: false,
            }),
        }) as Arc<dyn FileHandle>)
    }

    async fn readlink(&self, path: &str) -> Result<String> {
        self.lifetime
            .with_operation(false, "readlink", || async {
                let normalized = normalize_path(path);
                let inode = self.resolve(&normalized, false, "readlink")?;
                match &self.node(inode, "readlink", &normalized)?.data {
                    NodeData::Symlink { target } => Ok(target.clone()),
                    _ => Err(error_with_path(ErrorCode::Einval, "readlink", &normalized)),
                }
            })
            .await
    }

    async fn mkdir(&self, path: &str, _options: MkdirOptions) -> Result<Option<String>> {
        self.read_only("mkdir", path).await
    }

    async fn rmdir(&self, path: &str) -> Result<()> {
        self.read_only("rmdir", path).await
    }

    async fn unlink(&self, path: &str) -> Result<()> {
        self.read_only("unlink", path).await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        self.read_only_dest("rename", old_path, new_path).await
    }

    async fn link(&self, existing_path: &str, new_path: &str) -> Result<()> {
        self.read_only_dest("link", existing_path, new_path).await
    }

    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        self.read_only_dest("symlink", target, path).await
    }

    async fn chmod(&self, path: &str, _mode: u32) -> Result<()> {
        self.read_only("chmod", path).await
    }

    async fn chown(&self, path: &str, _uid: u32, _gid: u32) -> Result<()> {
        self.read_only("chown", path).await
    }

    async fn lchown(&self, path: &str, _uid: u32, _gid: u32) -> Result<()> {
        self.read_only("lchown", path).await
    }

    async fn truncate(&self, path: &str, _length: u64) -> Result<()> {
        self.read_only("truncate", path).await
    }

    fn has_utimens(&self) -> bool {
        true
    }

    async fn utimens(
        &self,
        path: &str,
        _atime_ns: i128,
        _mtime_ns: i128,
        _follow_symlinks: bool,
    ) -> Result<()> {
        self.read_only("utimens", path).await
    }

    async fn utimes(&self, path: &str, _atime_ms: i64, _mtime_ms: i64) -> Result<()> {
        self.read_only("utimes", path).await
    }

    async fn lutimes(&self, path: &str, _atime_ms: i64, _mtime_ms: i64) -> Result<()> {
        self.read_only("lutimes", path).await
    }

    async fn mknod(&self, path: &str, _mode: u32, _dev: u64) -> Result<()> {
        self.read_only("mknod", path).await
    }
}

struct HandleState {
    position: u64,
    closed: bool,
}

struct HistoricalFileHandle<M, B>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
    B: BlockStore + Clone,
{
    lifetime: Arc<ViewLifetime<M>>,
    namespace: Arc<Namespace>,
    blocks: B,
    inode: u64,
    path: String,
    state: Mutex<HandleState>,
}

#[async_trait]
impl<M, B> FileHandle for HistoricalFileHandle<M, B>
where
    M: VersionedMetadataStore + Clone + Send + Sync + 'static,
    B: BlockStore + Clone + Send + Sync + 'static,
{
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(error_with_path(ErrorCode::Ebadf, "read", &self.path));
        }
        let operation = self.lifetime.begin(true, "read").await?;
        let start = position.unwrap_or(state.position);
        let result = async {
            let node = self
                .namespace
                .nodes
                .get(&self.inode)
                .ok_or_else(|| error_with_path(ErrorCode::Estale, "read", &self.path))?;
            let layout = match &node.data {
                NodeData::File(layout) => layout,
                NodeData::Directory { .. } => {
                    return Err(error_with_path(ErrorCode::Eisdir, "read", &self.path));
                }
                _ => return Err(error_with_path(ErrorCode::Enxio, "read", &self.path)),
            };
            read_layout(
                layout,
                node.stats.size,
                start,
                buffer,
                &self.blocks,
                &self.path,
            )
            .await
        }
        .await;
        let finish = operation.finish("read").await;
        let count = match result {
            Err(error) => return Err(error),
            Ok(count) => {
                finish?;
                count
            }
        };
        if position.is_none() {
            state.position = start
                .checked_add(u64::try_from(count).map_err(|_| FsError::new(ErrorCode::Eoverflow))?)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        }
        Ok(count)
    }

    async fn write(&self, _buffer: &[u8], _position: Option<u64>) -> Result<usize> {
        Err(read_only_error("write", &self.path))
    }

    async fn stat(&self) -> Result<Stats> {
        let _state = self.state.lock().await;
        let state = &_state;
        if state.closed {
            return Err(error_with_path(ErrorCode::Ebadf, "fstat", &self.path));
        }
        let operation = self.lifetime.begin(true, "fstat").await?;
        let result = self
            .namespace
            .nodes
            .get(&self.inode)
            .map(|node| node.stats.clone())
            .ok_or_else(|| error_with_path(ErrorCode::Estale, "fstat", &self.path));
        let finish = operation.finish("fstat").await;
        match result {
            Err(error) => Err(error),
            Ok(stats) => {
                finish?;
                Ok(stats)
            }
        }
    }

    async fn truncate(&self, _length: u64) -> Result<()> {
        Err(read_only_error("ftruncate", &self.path))
    }

    async fn close(&self) -> Result<()> {
        // Keep the handle mutex held until provider cleanup succeeds. This
        // serializes concurrent close calls and leaves `closed == false` on a
        // retryable provider failure.
        let mut state = self.state.lock().await;
        if state.closed {
            return Ok(());
        }
        self.lifetime.release_handle().await?;
        state.closed = true;
        Ok(())
    }
}

async fn read_layout<B>(
    layout: &FileLayout,
    file_size: u64,
    start: u64,
    buffer: &mut [u8],
    blocks: &B,
    path: &str,
) -> Result<usize>
where
    B: BlockStore,
{
    if start >= file_size || buffer.is_empty() {
        return Ok(0);
    }
    let requested = u64::try_from(buffer.len()).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
    let end = start
        .checked_add(requested)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?
        .min(file_size);
    let count = usize::try_from(end - start).map_err(|_| FsError::new(ErrorCode::Efbig))?;
    buffer[..count].fill(0);
    for extent in &layout.extents {
        let extent_end = extent
            .file_offset
            .checked_add(extent.length)
            .ok_or_else(|| error_with_path(ErrorCode::Eio, "read", path))?;
        let overlap_start = start.max(extent.file_offset);
        let overlap_end = end.min(extent_end);
        if overlap_start >= overlap_end {
            continue;
        }
        let bytes = blocks
            .get(&extent.block)
            .await
            .map_err(|error| error.with_syscall("read").with_path(path))?;
        let block_offset = extent
            .block_offset
            .checked_add(overlap_start - extent.file_offset)
            .ok_or_else(|| error_with_path(ErrorCode::Eio, "read", path))?;
        let copy_len = usize::try_from(overlap_end - overlap_start)
            .map_err(|_| error_with_path(ErrorCode::Efbig, "read", path))?;
        let block_offset = usize::try_from(block_offset)
            .map_err(|_| error_with_path(ErrorCode::Efbig, "read", path))?;
        let block_end = block_offset
            .checked_add(copy_len)
            .ok_or_else(|| error_with_path(ErrorCode::Eio, "read", path))?;
        if block_end > bytes.len() {
            return Err(error_with_path(ErrorCode::Eio, "read", path));
        }
        let destination = usize::try_from(overlap_start - start)
            .map_err(|_| error_with_path(ErrorCode::Efbig, "read", path))?;
        buffer[destination..destination + copy_len]
            .copy_from_slice(&bytes[block_offset..block_end]);
    }
    Ok(count)
}

fn resolve_namespace(
    namespace: &Namespace,
    path: &str,
    follow_final: bool,
    syscall: &str,
    depth: usize,
) -> Result<u64> {
    if depth > MAX_SYMLINK_DEPTH {
        return Err(error_with_path(ErrorCode::Eloop, syscall, path));
    }
    let normalized = normalize_path(path);
    if normalized == "/" {
        return Ok(namespace.root);
    }
    let segments = split_path(&normalized);
    let mut current = namespace.root;
    for (index, name) in segments.iter().enumerate() {
        let current_node = namespace
            .nodes
            .get(&current)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, &normalized))?;
        let NodeData::Directory { entries } = &current_node.data else {
            return Err(error_with_path(ErrorCode::Enotdir, syscall, &normalized));
        };
        let child = entries
            .iter()
            .find(|entry| entry.name == *name)
            .map(|entry| entry.inode)
            .ok_or_else(|| error_with_path(ErrorCode::Enoent, syscall, &normalized))?;
        let child_node = namespace
            .nodes
            .get(&child)
            .ok_or_else(|| error_with_path(ErrorCode::Estale, syscall, &normalized))?;
        let last = index + 1 == segments.len();
        if matches!(child_node.data, NodeData::Symlink { .. }) && (!last || follow_final) {
            let NodeData::Symlink { target } = &child_node.data else {
                unreachable!()
            };
            if target.is_empty() {
                return Err(error_with_path(ErrorCode::Enoent, syscall, &normalized));
            }
            let parent_path = if index == 0 {
                "/".to_owned()
            } else {
                format!("/{}", segments[..index].join("/"))
            };
            let mut rewritten = if target.starts_with('/') {
                target.clone()
            } else {
                format!("{parent_path}/{target}")
            };
            if !last {
                rewritten.push('/');
                rewritten.push_str(&segments[index + 1..].join("/"));
            }
            return resolve_namespace(namespace, &rewritten, follow_final, syscall, depth + 1);
        }
        if last {
            return Ok(child);
        }
        current = child;
    }
    Err(error_with_path(ErrorCode::Enoent, syscall, &normalized))
}

fn error_with_path(code: ErrorCode, syscall: &str, path: &str) -> FsError {
    FsError::new(code).with_syscall(syscall).with_path(path)
}

fn read_only_error(syscall: &str, path: &str) -> FsError {
    error_with_path(ErrorCode::Erofs, syscall, path)
}
