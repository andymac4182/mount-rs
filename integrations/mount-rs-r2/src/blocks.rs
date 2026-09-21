//! Immutable byte blocks backed by an [\`object_store::ObjectStore\`].
//!
//! This adapter deliberately does not store namespace metadata or a snapshot
//! manifest. Each successful \`put\` is one provider-confirmed object upload;
//! metadata providers remain responsible for publishing references to it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use mount_rs_core::storage::{BlockId, BlockReconcileReport, BlockStore};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, PutMode, PutOptions, PutPayload};

const BLOCK_ID_PREFIX: char = 'b';
const BLOCK_ID_HEX_BYTES: usize = 32;
const MAX_CREATE_ATTEMPTS: usize = 16;

static NEXT_BLOCK_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Stable, bounded classes for object-store block errors.
///
/// The adapter deliberately does not expose provider messages, object paths,
/// or request identifiers as metric labels. `Authentication` includes the
/// invalid or expired workload-identity path; applications can alert on it
/// without parsing a backend error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum R2BlockStoreErrorClass {
    NotFound,
    Authentication,
    Permission,
    ConditionalConflict,
    Throttled,
    Client,
    Server,
}

/// Point-in-time diagnostics for an immutable object-store block provider.
///
/// The counters describe logical block-store operations. `retry_exhausted`
/// counts only terminal errors carrying the object-store client's bounded
/// retry marker; successful internal retries are intentionally not inferred
/// from a provider error string and require deployment-level instrumentation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct R2BlockStoreStats {
    pub puts: u64,
    pub gets: u64,
    pub deletes: u64,
    pub reconciles: u64,
    pub successes: u64,
    pub errors: u64,
    pub duration_ms_total: u64,
    pub duration_ms_max: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub conditional_conflicts: u64,
    pub id_collision_exhausted: u64,
    pub retry_exhausted: u64,
    pub error_classes: BTreeMap<R2BlockStoreErrorClass, u64>,
}

#[derive(Default)]
struct R2BlockStoreStatsState {
    puts: AtomicU64,
    gets: AtomicU64,
    deletes: AtomicU64,
    reconciles: AtomicU64,
    successes: AtomicU64,
    errors: AtomicU64,
    duration_ms_total: AtomicU64,
    duration_ms_max: AtomicU64,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    conditional_conflicts: AtomicU64,
    id_collision_exhausted: AtomicU64,
    retry_exhausted: AtomicU64,
    not_found_errors: AtomicU64,
    authentication_errors: AtomicU64,
    permission_errors: AtomicU64,
    conditional_errors: AtomicU64,
    throttled_errors: AtomicU64,
    client_errors: AtomicU64,
    server_errors: AtomicU64,
}

#[derive(Clone, Copy)]
enum BlockOperation {
    Put,
    Get,
    Delete,
    Reconcile,
}

impl R2BlockStoreStatsState {
    fn start(&self, operation: BlockOperation) -> Instant {
        match operation {
            BlockOperation::Put => increment(&self.puts),
            BlockOperation::Get => increment(&self.gets),
            BlockOperation::Delete => increment(&self.deletes),
            BlockOperation::Reconcile => increment(&self.reconciles),
        }
        Instant::now()
    }

    fn success(&self, started: Instant, bytes_read: u64, bytes_written: u64) {
        increment(&self.successes);
        let elapsed_ms = elapsed_ms(started);
        add(&self.duration_ms_total, elapsed_ms);
        update_max(&self.duration_ms_max, elapsed_ms);
        add(&self.bytes_read, bytes_read);
        add(&self.bytes_written, bytes_written);
    }

    fn error(&self, started: Instant, error: &object_store::Error) {
        increment(&self.errors);
        let elapsed_ms = elapsed_ms(started);
        add(&self.duration_ms_total, elapsed_ms);
        update_max(&self.duration_ms_max, elapsed_ms);
        if has_retry_exhaustion_marker(error) {
            increment(&self.retry_exhausted);
        }
        match classify_error(error) {
            R2BlockStoreErrorClass::NotFound => increment(&self.not_found_errors),
            R2BlockStoreErrorClass::Authentication => increment(&self.authentication_errors),
            R2BlockStoreErrorClass::Permission => increment(&self.permission_errors),
            R2BlockStoreErrorClass::ConditionalConflict => increment(&self.conditional_errors),
            R2BlockStoreErrorClass::Throttled => increment(&self.throttled_errors),
            R2BlockStoreErrorClass::Client => increment(&self.client_errors),
            R2BlockStoreErrorClass::Server => increment(&self.server_errors),
        }
    }

    fn conditional_conflict(&self) {
        increment(&self.conditional_conflicts);
    }

    fn snapshot(&self) -> R2BlockStoreStats {
        let mut error_classes = BTreeMap::new();
        insert_nonzero(
            &mut error_classes,
            R2BlockStoreErrorClass::NotFound,
            load(&self.not_found_errors),
        );
        insert_nonzero(
            &mut error_classes,
            R2BlockStoreErrorClass::Authentication,
            load(&self.authentication_errors),
        );
        insert_nonzero(
            &mut error_classes,
            R2BlockStoreErrorClass::Permission,
            load(&self.permission_errors),
        );
        insert_nonzero(
            &mut error_classes,
            R2BlockStoreErrorClass::ConditionalConflict,
            load(&self.conditional_errors),
        );
        insert_nonzero(
            &mut error_classes,
            R2BlockStoreErrorClass::Throttled,
            load(&self.throttled_errors),
        );
        insert_nonzero(
            &mut error_classes,
            R2BlockStoreErrorClass::Client,
            load(&self.client_errors),
        );
        insert_nonzero(
            &mut error_classes,
            R2BlockStoreErrorClass::Server,
            load(&self.server_errors),
        );
        R2BlockStoreStats {
            puts: load(&self.puts),
            gets: load(&self.gets),
            deletes: load(&self.deletes),
            reconciles: load(&self.reconciles),
            successes: load(&self.successes),
            errors: load(&self.errors),
            duration_ms_total: load(&self.duration_ms_total),
            duration_ms_max: load(&self.duration_ms_max),
            bytes_read: load(&self.bytes_read),
            bytes_written: load(&self.bytes_written),
            conditional_conflicts: load(&self.conditional_conflicts),
            id_collision_exhausted: load(&self.id_collision_exhausted),
            retry_exhausted: load(&self.retry_exhausted),
            error_classes,
        }
    }
}

fn increment(value: &AtomicU64) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(1))
    });
}

fn add(value: &AtomicU64, amount: u64) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

fn load(value: &AtomicU64) -> u64 {
    value.load(Ordering::Relaxed)
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn update_max(value: &AtomicU64, candidate: u64) {
    let mut current = load(value);
    while candidate > current {
        match value.compare_exchange_weak(current, candidate, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn insert_nonzero(
    classes: &mut BTreeMap<R2BlockStoreErrorClass, u64>,
    class: R2BlockStoreErrorClass,
    count: u64,
) {
    if count > 0 {
        classes.insert(class, count);
    }
}

fn classify_error(error: &object_store::Error) -> R2BlockStoreErrorClass {
    if is_throttled_error(error) {
        return R2BlockStoreErrorClass::Throttled;
    }
    match error {
        object_store::Error::NotFound { .. } => R2BlockStoreErrorClass::NotFound,
        object_store::Error::Unauthenticated { .. } => R2BlockStoreErrorClass::Authentication,
        object_store::Error::PermissionDenied { .. } => R2BlockStoreErrorClass::Permission,
        object_store::Error::AlreadyExists { .. }
        | object_store::Error::Precondition { .. }
        | object_store::Error::NotModified { .. } => R2BlockStoreErrorClass::ConditionalConflict,
        object_store::Error::InvalidPath { .. }
        | object_store::Error::NotSupported { .. }
        | object_store::Error::NotImplemented
        | object_store::Error::UnknownConfigurationKey { .. } => R2BlockStoreErrorClass::Client,
        object_store::Error::Generic { .. } | object_store::Error::JoinError { .. } => {
            R2BlockStoreErrorClass::Server
        }
        _ => R2BlockStoreErrorClass::Server,
    }
}

fn is_throttled_error(error: &object_store::Error) -> bool {
    let message = error.to_string();
    message.contains("SlowDown") || message.contains(" 429 ") || message.contains(" 503 ")
}

fn has_retry_exhaustion_marker(error: &object_store::Error) -> bool {
    let message = error.to_string();
    message.contains("after ") && message.contains("max_retries:")
}

/// Immutable blocks stored below one object-store prefix.
#[derive(Clone)]
pub struct R2BlockStore {
    store: Arc<dyn ObjectStore>,
    prefix: ObjectPath,
    durable: bool,
    stats: Arc<R2BlockStoreStatsState>,
}

impl R2BlockStore {
    /// Create a block store over an existing object store.
    ///
    /// \`prefix\` is normalized once and all block IDs are validated before a
    /// provider call. \`durable\` is intentionally caller-declared: an
    /// in-memory object store is volatile, while a configured R2 endpoint is
    /// normally durable. The adapter never guesses from the object-store
    /// implementation type.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        prefix: impl Into<String>,
        durable: bool,
    ) -> Result<Self> {
        Ok(Self {
            store,
            prefix: validate_prefix(&prefix.into())?,
            durable,
            stats: Arc::new(R2BlockStoreStatsState::default()),
        })
    }

    /// Build a durable block store using the same signed S3-compatible client
    /// as [\`crate::R2Store\`].
    pub fn from_config(config: &crate::R2Config, prefix: impl Into<String>) -> Result<Self> {
        Self::new(config.build_store()?, prefix, true)
    }

    /// Return the configured scope for diagnostics and provider setup tests.
    pub fn prefix(&self) -> &str {
        self.prefix.as_ref()
    }

    /// Return a bounded, label-safe snapshot of object-store block activity.
    ///
    /// The snapshot is shared by clones of this block store. It is intended
    /// for health endpoints and application-owned exporters; it does not
    /// install a collector or turn provider-account evidence into a hosted
    /// SLO result.
    pub fn stats(&self) -> R2BlockStoreStats {
        self.stats.snapshot()
    }

    fn object_path(&self, id: &BlockId) -> Result<ObjectPath> {
        validate_block_id(id)?;
        Ok(ObjectPath::from(format!("{}/{}", self.prefix, id.0)))
    }
}

#[async_trait]
impl BlockStore for R2BlockStore {
    fn durable(&self) -> bool {
        self.durable
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let started = self.stats.start(BlockOperation::Put);
        for _ in 0..MAX_CREATE_ATTEMPTS {
            let id = BlockId(next_block_id());
            let path = self.object_path(&id)?;
            let result = self
                .store
                .put_opts(
                    &path,
                    PutPayload::from(bytes.to_vec()),
                    PutOptions {
                        mode: PutMode::Create,
                        ..Default::default()
                    },
                )
                .await;
            match result {
                Ok(_) => {
                    self.stats.success(started, 0, bytes.len() as u64);
                    return Ok(id);
                }
                // A process-local sequence is only a collision avoidance
                // hint. The provider's conditional create is the authority
                // when multiple writers generate the same candidate ID.
                Err(object_store::Error::AlreadyExists { .. })
                | Err(object_store::Error::Precondition { .. }) => {
                    self.stats.conditional_conflict();
                    continue;
                }
                Err(error) => {
                    self.stats.error(started, &error);
                    return Err(backend_error(format!("put R2 block: {error}")));
                }
            }
        }
        increment(&self.stats.errors);
        let elapsed_ms = elapsed_ms(started);
        add(&self.stats.duration_ms_total, elapsed_ms);
        update_max(&self.stats.duration_ms_max, elapsed_ms);
        increment(&self.stats.id_collision_exhausted);
        self.stats.conditional_conflict();
        Err(FsError::new(ErrorCode::Eagain)
            .with_syscall("put block")
            .with_message("exhausted immutable block ID collision retries"))
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        let path = self.object_path(id)?;
        let started = self.stats.start(BlockOperation::Get);
        let result = match self.store.get(&path).await {
            Ok(result) => result,
            Err(error) => {
                self.stats.error(started, &error);
                return Err(map_get_error(error));
            }
        };
        match result.bytes().await {
            Ok(bytes) => {
                self.stats.success(started, bytes.len() as u64, 0);
                Ok(bytes.to_vec())
            }
            Err(error) => {
                self.stats.error(started, &error);
                Err(backend_error(format!("read R2 block: {error}")))
            }
        }
    }

    /// \`put_opts\` does not return until the object store has accepted the
    /// complete object. There are therefore no deferred uploads to drain;
    /// this is the confirmed-upload barrier required before metadata publish.
    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        let path = self.object_path(id)?;
        let started = self.stats.start(BlockOperation::Delete);
        if let Err(error) = self.store.head(&path).await {
            self.stats.error(started, &error);
            return Err(map_not_found(error));
        }
        match self.store.delete(&path).await {
            Ok(()) => {
                self.stats.success(started, 0, 0);
                Ok(())
            }
            Err(error) => {
                self.stats.error(started, &error);
                Err(map_not_found(error))
            }
        }
    }

    /// Reconcile only objects in this block-store prefix. Objects newer than
    /// the supplied grace period are retained because they may belong to an
    /// in-flight or ambiguous publication whose metadata result has not been
    /// reconciled.
    async fn reconcile(
        &self,
        live: &BTreeSet<BlockId>,
        grace: std::time::Duration,
    ) -> Result<BlockReconcileReport> {
        if grace.is_zero() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("reconcile blocks")
                .with_message("reconciliation grace period must be positive"));
        }
        let started = self.stats.start(BlockOperation::Reconcile);
        let mut listing = self.store.list(Some(&self.prefix));
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let cutoff_ms = now_ms.saturating_sub(grace.as_millis());
        let prefix = format!("{}/", self.prefix);
        let mut report = BlockReconcileReport::default();

        while let Some(object) = listing.next().await {
            let object = match object {
                Ok(object) => object,
                Err(error) => {
                    self.stats.error(started, &error);
                    return Err(backend_error(format!(
                        "list R2 blocks for reconciliation: {error}"
                    )));
                }
            };
            let Some(relative) = object.location.as_ref().strip_prefix(&prefix) else {
                continue;
            };
            if relative.is_empty() || relative.contains('/') {
                continue;
            }
            let id = BlockId(relative.to_owned());
            if validate_block_id(&id).is_err() {
                continue;
            }
            report.scanned = report.scanned.saturating_add(1);
            if live.contains(&id) {
                report.protected = report.protected.saturating_add(1);
                continue;
            }
            let is_recent = u128::try_from(object.last_modified.timestamp_millis())
                .is_ok_and(|timestamp_ms| timestamp_ms >= cutoff_ms);
            if is_recent {
                report.recent = report.recent.saturating_add(1);
                continue;
            }
            match self.store.delete(&object.location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {
                    report.deleted = report.deleted.saturating_add(1);
                }
                Err(error) => {
                    self.stats.error(started, &error);
                    return Err(backend_error(format!(
                        "delete unreferenced R2 block during reconciliation: {error}"
                    )));
                }
            }
        }
        self.stats.success(started, 0, 0);
        Ok(report)
    }
}

fn validate_prefix(prefix: &str) -> Result<ObjectPath> {
    let normalized = prefix.trim_matches('/');
    if normalized.is_empty() || normalized.contains('\0') {
        return Err(invalid_scope("block prefix must be non-empty"));
    }
    if normalized
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid_scope(
            "block prefix contains an unsafe path component",
        ));
    }
    Ok(ObjectPath::from(normalized.to_owned()))
}

fn validate_block_id(id: &BlockId) -> Result<()> {
    let mut chars = id.0.chars();
    if chars.next() != Some(BLOCK_ID_PREFIX)
        || id.0.len() != 1 + BLOCK_ID_HEX_BYTES
        || !chars.all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err(invalid_scope("invalid R2 block ID"));
    }
    Ok(())
}

fn invalid_scope(message: &'static str) -> FsError {
    FsError::new(ErrorCode::Einval)
        .with_syscall("block path")
        .with_message(message)
}

fn map_not_found(error: object_store::Error) -> FsError {
    match error {
        object_store::Error::NotFound { .. } => {
            FsError::new(ErrorCode::Enoent).with_syscall("block object")
        }
        error => backend_error(format!("R2 block object: {error}")),
    }
}

fn map_get_error(error: object_store::Error) -> FsError {
    map_not_found(error)
}

fn next_block_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or_default();
    let sequence = NEXT_BLOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{BLOCK_ID_PREFIX}{timestamp:016x}{sequence:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::storage::BlockStore;
    use object_store::memory::InMemory;

    async fn stores() -> (R2BlockStore, R2BlockStore, Arc<InMemory>) {
        let object_store = Arc::new(InMemory::new());
        let first = R2BlockStore::new(object_store.clone(), "vol-a/blocks", false).unwrap();
        let second = R2BlockStore::new(object_store.clone(), "vol-b/blocks", false).unwrap();
        (first, second, object_store)
    }

    #[tokio::test]
    async fn puts_are_confirmed_immutable_and_scoped() {
        let (first, second, object_store) = stores().await;
        assert!(!first.durable());
        assert_eq!(first.prefix(), "vol-a/blocks");

        let first_id = first.put(b"first bytes").await.unwrap();
        let second_id = first.put(b"first bytes").await.unwrap();
        assert_ne!(first_id, second_id, "each upload receives a unique ID");
        assert_eq!(first.get(&first_id).await.unwrap(), b"first bytes");
        assert_eq!(first.get(&second_id).await.unwrap(), b"first bytes");
        first.flush().await.unwrap();

        assert!(
            second
                .get(&first_id)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        assert!(
            second
                .delete(&first_id)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        assert_eq!(first.get(&first_id).await.unwrap(), b"first bytes");

        let listing = object_store
            .list_with_delimiter(Some(&ObjectPath::from("vol-a/blocks")))
            .await
            .unwrap();
        assert_eq!(listing.objects.len(), 2);
        assert!(
            listing
                .objects
                .iter()
                .all(|object| object.location.as_ref().starts_with("vol-a/blocks/b"))
        );
    }

    #[tokio::test]
    async fn volatility_is_explicit_and_missing_or_invalid_blocks_fail_closed() {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let volatile = R2BlockStore::new(object_store.clone(), "blocks", false).unwrap();
        let declared_durable = R2BlockStore::new(object_store, "blocks", true).unwrap();
        assert!(!volatile.durable());
        assert!(declared_durable.durable());

        let missing = BlockId("b00000000000000000000000000000000".to_owned());
        assert!(
            volatile
                .get(&missing)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        assert!(
            volatile
                .delete(&missing)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        for invalid in [
            BlockId("../other".to_owned()),
            BlockId("bUPPER00000000000000000000000000".to_owned()),
            BlockId("b0000/other".to_owned()),
        ] {
            assert!(
                volatile
                    .get(&invalid)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Einval)
            );
            assert!(
                volatile
                    .delete(&invalid)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Einval)
            );
        }
        assert!(R2BlockStore::new(Arc::new(InMemory::new()), "blocks/../other", false,).is_err());
    }

    #[tokio::test]
    async fn delete_is_confirmed_and_does_not_alias_another_prefix() {
        let (first, second, _) = stores().await;
        let id = first.put(&[0, 255, 3]).await.unwrap();
        first.delete(&id).await.unwrap();
        assert!(first.get(&id).await.unwrap_err().is(ErrorCode::Enoent));
        assert!(second.get(&id).await.unwrap_err().is(ErrorCode::Enoent));
        assert!(first.delete(&id).await.unwrap_err().is(ErrorCode::Enoent));
    }

    #[tokio::test]
    async fn stats_are_bounded_shared_and_classify_missing_objects() {
        let (first, _, _) = stores().await;
        let clone = first.clone();
        let id = clone.put(b"stats").await.unwrap();
        assert_eq!(first.get(&id).await.unwrap(), b"stats");

        let missing = BlockId("b00000000000000000000000000000000".to_owned());
        assert!(first.get(&missing).await.unwrap_err().is(ErrorCode::Enoent));
        assert!(
            first
                .delete(&missing)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );

        let stats = first.stats();
        assert_eq!(stats.puts, 1);
        assert_eq!(stats.gets, 2);
        assert_eq!(stats.deletes, 1);
        assert_eq!(stats.successes, 2);
        assert_eq!(stats.errors, 2);
        assert_eq!(stats.bytes_written, 5);
        assert_eq!(stats.bytes_read, 5);
        assert_eq!(stats.conditional_conflicts, 0);
        assert_eq!(stats.id_collision_exhausted, 0);
        assert_eq!(stats.retry_exhausted, 0);
        assert_eq!(
            stats.error_classes.get(&R2BlockStoreErrorClass::NotFound),
            Some(&2)
        );
        assert!(stats.duration_ms_total >= stats.duration_ms_max);
    }

    #[tokio::test]
    async fn reconciliation_protects_live_and_recent_blocks_then_deletes_old_blocks() {
        let (first, _, _) = stores().await;
        let id = first.put(b"reconcile me").await.unwrap();
        let live = BTreeSet::from([id.clone()]);

        let protected = first
            .reconcile(&live, std::time::Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(protected.scanned, 1);
        assert_eq!(protected.protected, 1);
        assert_eq!(protected.recent, 0);
        assert_eq!(protected.deleted, 0);

        let recent = first
            .reconcile(&BTreeSet::new(), std::time::Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(recent.recent, 1);
        assert_eq!(recent.deleted, 0);
        assert_eq!(first.get(&id).await.unwrap(), b"reconcile me");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let deleted = first
            .reconcile(&BTreeSet::new(), std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(deleted.deleted, 1);
        assert!(first.get(&id).await.unwrap_err().is(ErrorCode::Enoent));
    }
}
