//! Immutable byte blocks backed by an [\`object_store::ObjectStore\`].
//!
//! This adapter deliberately does not store namespace metadata or a snapshot
//! manifest. Each successful \`put\` is one provider-confirmed object upload;
//! metadata providers remain responsible for publishing references to it.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use mount_rs_core::storage::{BlockId, BlockReconcileReport, BlockStore};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, PutMode, PutOptions, PutPayload};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

const BLOCK_ID_PREFIX: char = 'b';
const LEGACY_BLOCK_ID_HEX_CHARS: usize = 32;
const CONTENT_BLOCK_ID_HEX_CHARS: usize = 64;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 4096;
const CONCURRENT_PROBE_NAME: &str = "_mount-rs-concurrent-probe-v1";
const CONCURRENT_PROBE_BYTES: &[u8] = b"mount-rs:object-store:concurrent-preflight:v1\n";
const CONCURRENT_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Check a configured signed object-store client before enabling concurrent
/// metadata publication over its blocks.
///
/// This probe performs a conditional create under the same block prefix and
/// reads the stored bytes directly from the supplied client. The reserved
/// object is stable across mounts and excluded from block reconciliation.
/// Callers must use a client built from a validated service configuration:
/// an arbitrary injected `ObjectStore` cannot establish a shared backing.
pub async fn probe_configured_concurrent_prefix(
    store: &dyn ObjectStore,
    prefix: &str,
) -> Result<()> {
    let prefix = validate_prefix(prefix)?;
    let path = ObjectPath::from(format!("{prefix}/{CONCURRENT_PROBE_NAME}"));
    tokio::time::timeout(CONCURRENT_PROBE_TIMEOUT, async {
        let create = store
            .put_opts(
                &path,
                PutPayload::from(CONCURRENT_PROBE_BYTES.to_vec()),
                PutOptions {
                    mode: PutMode::Create,
                    ..PutOptions::default()
                },
            )
            .await;
        match create {
            Ok(_)
            | Err(object_store::Error::AlreadyExists { .. })
            | Err(object_store::Error::Precondition { .. }) => {}
            Err(error) => return Err(probe_error("write", &error)),
        }
        let bytes = store
            .get(&path)
            .await
            .map_err(|error| probe_error("read", &error))?
            .bytes()
            .await
            .map_err(|error| probe_error("read", &error))?;
        if bytes.as_ref() != CONCURRENT_PROBE_BYTES {
            return Err(FsError::backend(
                "object-store concurrent preflight probe changed unexpectedly",
            ));
        }
        Ok(())
    })
    .await
    .map_err(|_| FsError::backend("object-store concurrent preflight probe timed out"))?
}

fn probe_error(operation: &str, error: &object_store::Error) -> FsError {
    // Provider error strings may contain service URLs or authorization data.
    // Report only a bounded error class in startup diagnostics.
    FsError::backend(format!(
        "object-store concurrent preflight {operation} failed ({:?})",
        classify_error(error)
    ))
}

/// Stable, bounded classes for object-store block errors.
///
/// The adapter deliberately does not expose provider messages, object paths,
/// or request identifiers as metric labels. `Authentication` includes the
/// invalid or expired workload-identity path; applications can alert on it
/// without parsing a backend error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ObjectStoreBlockStoreErrorClass {
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
pub struct ObjectStoreBlockStoreStats {
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
    pub cache_hits: u64,
    pub error_classes: BTreeMap<ObjectStoreBlockStoreErrorClass, u64>,
}

#[derive(Default)]
struct ObjectStoreBlockStoreStatsState {
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
    cache_hits: AtomicU64,
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

impl ObjectStoreBlockStoreStatsState {
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
            ObjectStoreBlockStoreErrorClass::NotFound => increment(&self.not_found_errors),
            ObjectStoreBlockStoreErrorClass::Authentication => {
                increment(&self.authentication_errors)
            }
            ObjectStoreBlockStoreErrorClass::Permission => increment(&self.permission_errors),
            ObjectStoreBlockStoreErrorClass::ConditionalConflict => {
                increment(&self.conditional_errors)
            }
            ObjectStoreBlockStoreErrorClass::Throttled => increment(&self.throttled_errors),
            ObjectStoreBlockStoreErrorClass::Client => increment(&self.client_errors),
            ObjectStoreBlockStoreErrorClass::Server => increment(&self.server_errors),
        }
    }

    fn logical_error(&self, started: Instant) {
        increment(&self.errors);
        let elapsed_ms = elapsed_ms(started);
        add(&self.duration_ms_total, elapsed_ms);
        update_max(&self.duration_ms_max, elapsed_ms);
    }

    fn conditional_conflict(&self) {
        increment(&self.conditional_conflicts);
    }

    fn cache_hit(&self) {
        increment(&self.cache_hits);
    }

    fn snapshot(&self) -> ObjectStoreBlockStoreStats {
        let mut error_classes = BTreeMap::new();
        insert_nonzero(
            &mut error_classes,
            ObjectStoreBlockStoreErrorClass::NotFound,
            load(&self.not_found_errors),
        );
        insert_nonzero(
            &mut error_classes,
            ObjectStoreBlockStoreErrorClass::Authentication,
            load(&self.authentication_errors),
        );
        insert_nonzero(
            &mut error_classes,
            ObjectStoreBlockStoreErrorClass::Permission,
            load(&self.permission_errors),
        );
        insert_nonzero(
            &mut error_classes,
            ObjectStoreBlockStoreErrorClass::ConditionalConflict,
            load(&self.conditional_errors),
        );
        insert_nonzero(
            &mut error_classes,
            ObjectStoreBlockStoreErrorClass::Throttled,
            load(&self.throttled_errors),
        );
        insert_nonzero(
            &mut error_classes,
            ObjectStoreBlockStoreErrorClass::Client,
            load(&self.client_errors),
        );
        insert_nonzero(
            &mut error_classes,
            ObjectStoreBlockStoreErrorClass::Server,
            load(&self.server_errors),
        );
        ObjectStoreBlockStoreStats {
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
            cache_hits: load(&self.cache_hits),
            error_classes,
        }
    }
}

#[derive(Default)]
struct ObjectStoreBlockCacheState {
    entries: HashMap<String, Vec<u8>>,
    order: VecDeque<String>,
    bytes: usize,
}

#[derive(Default)]
struct ObjectStoreBlockCache {
    state: Mutex<ObjectStoreBlockCacheState>,
}

impl ObjectStoreBlockCache {
    fn get(&self, id: &str) -> Option<Vec<u8>> {
        let mut state = self.state.lock().ok()?;
        let bytes = state.entries.get(id)?.clone();
        if let Some(position) = state.order.iter().position(|entry| entry == id) {
            state.order.remove(position);
        }
        state.order.push_back(id.to_owned());
        Some(bytes)
    }

    fn insert(&self, id: &str, bytes: &[u8]) {
        if bytes.len() > MAX_CACHE_BYTES {
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(previous) = state.entries.remove(id) {
            state.bytes = state.bytes.saturating_sub(previous.len());
            if let Some(position) = state.order.iter().position(|entry| entry == id) {
                state.order.remove(position);
            }
        }
        while (state.entries.len() >= MAX_CACHE_ENTRIES
            || state.bytes.saturating_add(bytes.len()) > MAX_CACHE_BYTES)
            && !state.order.is_empty()
        {
            let Some(evicted) = state.order.pop_front() else {
                break;
            };
            if let Some(previous) = state.entries.remove(&evicted) {
                state.bytes = state.bytes.saturating_sub(previous.len());
            }
        }
        state.bytes = state.bytes.saturating_add(bytes.len());
        state.entries.insert(id.to_owned(), bytes.to_vec());
        state.order.push_back(id.to_owned());
    }

    fn remove(&self, id: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(previous) = state.entries.remove(id) {
            state.bytes = state.bytes.saturating_sub(previous.len());
        }
        if let Some(position) = state.order.iter().position(|entry| entry == id) {
            state.order.remove(position);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconcileCandidate {
    Protected,
    Recent,
    Delete,
}

fn classify_reconcile_candidate(
    in_scope: bool,
    valid_id: bool,
    is_live: bool,
    last_modified_ms: i64,
    now_ms: u128,
    grace_ms: u128,
) -> Option<ReconcileCandidate> {
    if !in_scope || !valid_id {
        return None;
    }
    if is_live {
        return Some(ReconcileCandidate::Protected);
    }
    let cutoff_ms = now_ms.saturating_sub(grace_ms);
    if last_modified_ms < 0 || (last_modified_ms as u128) >= cutoff_ms {
        return Some(ReconcileCandidate::Recent);
    }
    Some(ReconcileCandidate::Delete)
}

#[cfg(kani)]
mod verification {
    use super::*;

    #[kani::proof]
    #[kani::unwind(2)]
    fn reconciliation_deletes_only_unreferenced_direct_old_blocks() {
        let in_scope: bool = kani::any();
        let valid_id: bool = kani::any();
        let is_live: bool = kani::any();
        let last_modified_ms: i64 = kani::any();
        let now_ms: u128 = kani::any();
        let grace_ms: u128 = kani::any();

        let actual = classify_reconcile_candidate(
            in_scope,
            valid_id,
            is_live,
            last_modified_ms,
            now_ms,
            grace_ms,
        );
        let expected = if !in_scope || !valid_id {
            None
        } else if is_live {
            Some(ReconcileCandidate::Protected)
        } else if last_modified_ms < 0 {
            Some(ReconcileCandidate::Recent)
        } else {
            let modified_ms = last_modified_ms as u128;
            if modified_ms < now_ms && now_ms - modified_ms > grace_ms {
                Some(ReconcileCandidate::Delete)
            } else {
                Some(ReconcileCandidate::Recent)
            }
        };
        assert_eq!(actual, expected);

        kani::cover!(!in_scope && actual.is_none());
        kani::cover!(in_scope && !valid_id && actual.is_none());
        kani::cover!(
            in_scope && valid_id && is_live && actual == Some(ReconcileCandidate::Protected)
        );
        kani::cover!(
            in_scope
                && valid_id
                && !is_live
                && last_modified_ms < 0
                && actual == Some(ReconcileCandidate::Recent)
        );
        kani::cover!(
            in_scope
                && valid_id
                && !is_live
                && last_modified_ms >= 0
                && actual == Some(ReconcileCandidate::Recent)
        );
        kani::cover!(actual == Some(ReconcileCandidate::Delete));
    }

    fn check_block_id_grammar<const N: usize>() {
        let bytes: [u8; N] = kani::any();
        let actual = valid_block_id_bytes(&bytes);
        let expected = bytes[0] == b'b'
            && bytes[1..]
                .iter()
                .all(|byte| (b'0'..=b'9').contains(byte) || (b'a'..=b'f').contains(byte));
        assert_eq!(actual, expected);
        kani::cover!(actual);
        kani::cover!(bytes[0] != b'b' && !actual);
        kani::cover!(bytes[0] == b'b' && bytes[1] == b'/' && !actual);
        kani::cover!(bytes[0] == b'b' && bytes[1] == b'A' && !actual);
    }

    #[kani::proof]
    #[kani::unwind(70)]
    fn legacy_block_id_grammar_is_lowercase_hex() {
        check_block_id_grammar::<{ 1 + LEGACY_BLOCK_ID_HEX_CHARS }>();
    }

    #[kani::proof]
    #[kani::unwind(70)]
    fn content_block_id_grammar_is_lowercase_hex() {
        check_block_id_grammar::<{ 1 + CONTENT_BLOCK_ID_HEX_CHARS }>();
    }
}

type InFlightPutResult = std::result::Result<(), FsError>;

struct InFlightPut {
    result: watch::Sender<Option<InFlightPutResult>>,
}

impl InFlightPut {
    fn new() -> Arc<Self> {
        let (result, _) = watch::channel(None);
        Arc::new(Self { result })
    }

    fn subscribe(&self) -> watch::Receiver<Option<InFlightPutResult>> {
        self.result.subscribe()
    }

    fn finish(&self, result: InFlightPutResult) {
        let _ = self.result.send(Some(result));
    }
}

enum InFlightPutClaim {
    Leader(InFlightPutGuard),
    Follower(watch::Receiver<Option<InFlightPutResult>>),
}

struct InFlightPutGuard {
    id: String,
    entry: Arc<InFlightPut>,
    entries: Arc<Mutex<HashMap<String, Arc<InFlightPut>>>>,
    completed: bool,
}

impl InFlightPutGuard {
    fn remove(&self) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries
            .get(&self.id)
            .is_some_and(|entry| Arc::ptr_eq(entry, &self.entry))
        {
            entries.remove(&self.id);
        }
    }

    fn finish(mut self, result: InFlightPutResult) {
        self.remove();
        self.entry.finish(result);
        self.completed = true;
    }
}

impl Drop for InFlightPutGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        self.remove();
        self.entry
            .finish(Err(backend_error("object-store block upload canceled")));
    }
}

async fn wait_for_inflight_put(
    mut receiver: watch::Receiver<Option<InFlightPutResult>>,
) -> Result<()> {
    loop {
        let result = receiver.borrow().clone();
        if let Some(result) = result {
            return result;
        }
        if receiver.changed().await.is_err() {
            return Err(backend_error("object-store block upload canceled"));
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
    classes: &mut BTreeMap<ObjectStoreBlockStoreErrorClass, u64>,
    class: ObjectStoreBlockStoreErrorClass,
    count: u64,
) {
    if count > 0 {
        classes.insert(class, count);
    }
}

fn classify_error(error: &object_store::Error) -> ObjectStoreBlockStoreErrorClass {
    if is_throttled_error(error) {
        return ObjectStoreBlockStoreErrorClass::Throttled;
    }
    match error {
        object_store::Error::NotFound { .. } => ObjectStoreBlockStoreErrorClass::NotFound,
        object_store::Error::Unauthenticated { .. } => {
            ObjectStoreBlockStoreErrorClass::Authentication
        }
        object_store::Error::PermissionDenied { .. } => ObjectStoreBlockStoreErrorClass::Permission,
        object_store::Error::AlreadyExists { .. }
        | object_store::Error::Precondition { .. }
        | object_store::Error::NotModified { .. } => {
            ObjectStoreBlockStoreErrorClass::ConditionalConflict
        }
        object_store::Error::InvalidPath { .. }
        | object_store::Error::NotSupported { .. }
        | object_store::Error::NotImplemented
        | object_store::Error::UnknownConfigurationKey { .. } => {
            ObjectStoreBlockStoreErrorClass::Client
        }
        object_store::Error::Generic { .. } | object_store::Error::JoinError { .. } => {
            ObjectStoreBlockStoreErrorClass::Server
        }
        _ => ObjectStoreBlockStoreErrorClass::Server,
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
pub struct ObjectStoreBlockStore {
    store: Arc<dyn ObjectStore>,
    prefix: ObjectPath,
    durable: bool,
    stats: Arc<ObjectStoreBlockStoreStatsState>,
    cache: Arc<ObjectStoreBlockCache>,
    inflight_puts: Arc<Mutex<HashMap<String, Arc<InFlightPut>>>>,
}

impl ObjectStoreBlockStore {
    /// Create a block store over an existing object store.
    ///
    /// \`prefix\` is normalized once and all block IDs are validated before a
    /// provider call. \`durable\` is intentionally caller-declared: an
    /// in-memory object store is volatile, while a configured remote object
    /// store is normally durable. The adapter never guesses from the object-store
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
            stats: Arc::new(ObjectStoreBlockStoreStatsState::default()),
            cache: Arc::new(ObjectStoreBlockCache::default()),
            inflight_puts: Arc::new(Mutex::new(HashMap::new())),
        })
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
    pub fn stats(&self) -> ObjectStoreBlockStoreStats {
        self.stats.snapshot()
    }

    fn object_path(&self, id: &BlockId) -> Result<ObjectPath> {
        validate_block_id(id)?;
        Ok(ObjectPath::from(format!("{}/{}", self.prefix, id.0)))
    }

    fn claim_put(&self, id: &str) -> InFlightPutClaim {
        let entry = InFlightPut::new();
        let Ok(mut entries) = self.inflight_puts.lock() else {
            return InFlightPutClaim::Leader(InFlightPutGuard {
                id: id.to_owned(),
                entry,
                entries: Arc::clone(&self.inflight_puts),
                completed: false,
            });
        };
        if let Some(existing) = entries.get(id) {
            return InFlightPutClaim::Follower(existing.subscribe());
        }
        entries.insert(id.to_owned(), Arc::clone(&entry));
        InFlightPutClaim::Leader(InFlightPutGuard {
            id: id.to_owned(),
            entry,
            entries: Arc::clone(&self.inflight_puts),
            completed: false,
        })
    }

    async fn put_remote(
        &self,
        id: &BlockId,
        bytes: &[u8],
        path: &ObjectPath,
        started: Instant,
    ) -> Result<()> {
        let result = self
            .store
            .put_opts(
                path,
                PutPayload::from(bytes.to_vec()),
                PutOptions {
                    mode: PutMode::Create,
                    ..Default::default()
                },
            )
            .await;
        match result {
            Ok(_) => {
                self.cache.insert(&id.0, bytes);
                self.stats.success(started, 0, bytes.len() as u64);
                Ok(())
            }
            // Content addressing makes a conditional-create conflict an
            // idempotent success only after the existing object is checked.
            // The digest is still validated by comparing the immutable bytes;
            // a collision or manually-corrupted object fails closed.
            Err(object_store::Error::AlreadyExists { .. })
            | Err(object_store::Error::Precondition { .. }) => {
                self.stats.conditional_conflict();
                let existing = match self.store.get(path).await {
                    Ok(result) => match result.bytes().await {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            self.cache.remove(&id.0);
                            self.stats.error(started, &error);
                            return Err(backend_error(format!(
                                "verify object-store block: {error}"
                            )));
                        }
                    },
                    Err(error) => {
                        self.cache.remove(&id.0);
                        self.stats.error(started, &error);
                        return Err(map_not_found(error));
                    }
                };
                if existing.as_ref() != bytes {
                    self.cache.remove(&id.0);
                    increment(&self.stats.errors);
                    return Err(backend_error(
                        "object-store content-addressed block collision",
                    ));
                }
                self.cache.insert(&id.0, bytes);
                self.stats.success(started, 0, bytes.len() as u64);
                Ok(())
            }
            Err(error) => {
                self.cache.remove(&id.0);
                self.stats.error(started, &error);
                Err(backend_error(format!("put object-store block: {error}")))
            }
        }
    }
}

#[async_trait]
impl BlockStore for ObjectStoreBlockStore {
    fn durable(&self) -> bool {
        self.durable
    }

    async fn prepare_concurrent_mode(&self) -> Result<()> {
        Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("prepare concurrent object-store blocks")
            .with_message("concurrent object-store blocks require a validated signed service"))
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let started = self.stats.start(BlockOperation::Put);
        let id = BlockId(block_id(bytes));
        let path = self.object_path(&id)?;
        match self.claim_put(&id.0) {
            InFlightPutClaim::Follower(receiver) => match wait_for_inflight_put(receiver).await {
                Ok(()) => {
                    self.stats.success(started, 0, bytes.len() as u64);
                    Ok(id)
                }
                Err(error) => {
                    self.stats.logical_error(started);
                    Err(error)
                }
            },
            InFlightPutClaim::Leader(guard) => {
                let result = self.put_remote(&id, bytes, &path, started).await;
                match result {
                    Ok(()) => {
                        guard.finish(Ok(()));
                        Ok(id)
                    }
                    Err(error) => {
                        guard.finish(Err(error.clone()));
                        Err(error)
                    }
                }
            }
        }
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        let path = self.object_path(id)?;
        let started = self.stats.start(BlockOperation::Get);
        if let Some(bytes) = self.cache.get(&id.0) {
            self.stats.cache_hit();
            self.stats.success(started, bytes.len() as u64, 0);
            return Ok(bytes);
        }
        let result = match self.store.get(&path).await {
            Ok(result) => result,
            Err(error) => {
                self.stats.error(started, &error);
                return Err(map_get_error(error));
            }
        };
        match result.bytes().await {
            Ok(bytes) => {
                if id.0.len() == 1 + CONTENT_BLOCK_ID_HEX_CHARS && block_id(&bytes) != id.0 {
                    self.cache.remove(&id.0);
                    self.stats.logical_error(started);
                    return Err(FsError::backend(
                        "object-store content-addressed block digest mismatch",
                    ));
                }
                self.cache.insert(&id.0, &bytes);
                self.stats.success(started, bytes.len() as u64, 0);
                Ok(bytes.to_vec())
            }
            Err(error) => {
                self.stats.error(started, &error);
                Err(backend_error(format!("read object-store block: {error}")))
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
                self.cache.remove(&id.0);
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
        let prefix = format!("{}/", self.prefix);
        let mut report = BlockReconcileReport::default();

        while let Some(object) = listing.next().await {
            let object = match object {
                Ok(object) => object,
                Err(error) => {
                    self.stats.error(started, &error);
                    return Err(backend_error(format!(
                        "list object-store blocks for reconciliation: {error}"
                    )));
                }
            };
            let relative = object.location.as_ref().strip_prefix(&prefix);
            let in_scope =
                relative.is_some_and(|relative| !relative.is_empty() && !relative.contains('/'));
            let id = relative
                .filter(|_| in_scope)
                .map(|relative| BlockId(relative.to_owned()));
            let valid_id = id.as_ref().is_some_and(|id| validate_block_id(id).is_ok());
            let is_live = id.as_ref().is_some_and(|id| live.contains(id));
            let Some(candidate) = classify_reconcile_candidate(
                in_scope,
                valid_id,
                is_live,
                object.last_modified.timestamp_millis(),
                now_ms,
                grace.as_millis(),
            ) else {
                continue;
            };
            report.scanned = report.scanned.saturating_add(1);
            match candidate {
                ReconcileCandidate::Protected => {
                    report.protected = report.protected.saturating_add(1);
                    continue;
                }
                ReconcileCandidate::Recent => {
                    report.recent = report.recent.saturating_add(1);
                    continue;
                }
                ReconcileCandidate::Delete => {}
            }
            match self.store.delete(&object.location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {
                    self.cache.remove(&id.expect("validated direct block ID").0);
                    report.deleted = report.deleted.saturating_add(1);
                }
                Err(error) => {
                    self.stats.error(started, &error);
                    return Err(backend_error(format!(
                        "delete unreferenced object-store block during reconciliation: {error}"
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
    if !valid_block_id_bytes(id.0.as_bytes()) {
        return Err(invalid_scope("invalid object-store block ID"));
    }
    Ok(())
}

fn valid_block_id_bytes(bytes: &[u8]) -> bool {
    bytes.first() == Some(&(BLOCK_ID_PREFIX as u8))
        && (bytes.len() == 1 + LEGACY_BLOCK_ID_HEX_CHARS
            || bytes.len() == 1 + CONTENT_BLOCK_ID_HEX_CHARS)
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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
        error => backend_error(format!("object-store block object: {error}")),
    }
}

fn map_get_error(error: object_store::Error) -> FsError {
    map_not_found(error)
}

fn block_id(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(1 + CONTENT_BLOCK_ID_HEX_CHARS);
    encoded.push(BLOCK_ID_PREFIX);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream::BoxStream;
    use mount_rs_core::storage::BlockStore;
    use object_store::memory::InMemory;
    use object_store::{
        GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, PutMultipartOptions,
        PutResult,
    };

    #[derive(Debug)]
    struct PreEpochListStore {
        inner: Arc<InMemory>,
    }

    impl std::fmt::Display for PreEpochListStore {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("PreEpochListStore")
        }
    }

    #[async_trait]
    impl ObjectStore for PreEpochListStore {
        async fn put_opts(
            &self,
            location: &ObjectPath,
            payload: PutPayload,
            opts: PutOptions,
        ) -> object_store::Result<PutResult> {
            self.inner.put_opts(location, payload, opts).await
        }

        async fn put_multipart_opts(
            &self,
            location: &ObjectPath,
            opts: PutMultipartOptions,
        ) -> object_store::Result<Box<dyn MultipartUpload>> {
            self.inner.put_multipart_opts(location, opts).await
        }

        async fn get_opts(
            &self,
            location: &ObjectPath,
            options: GetOptions,
        ) -> object_store::Result<GetResult> {
            self.inner.get_opts(location, options).await
        }

        async fn delete(&self, location: &ObjectPath) -> object_store::Result<()> {
            self.inner.delete(location).await
        }

        fn list(
            &self,
            prefix: Option<&ObjectPath>,
        ) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
            self.inner
                .list(prefix)
                .map(|object| {
                    object.map(|mut meta| {
                        meta.last_modified =
                            (UNIX_EPOCH - std::time::Duration::from_secs(1)).into();
                        meta
                    })
                })
                .boxed()
        }

        async fn list_with_delimiter(
            &self,
            prefix: Option<&ObjectPath>,
        ) -> object_store::Result<ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }

        async fn copy(&self, from: &ObjectPath, to: &ObjectPath) -> object_store::Result<()> {
            self.inner.copy(from, to).await
        }

        async fn copy_if_not_exists(
            &self,
            from: &ObjectPath,
            to: &ObjectPath,
        ) -> object_store::Result<()> {
            self.inner.copy_if_not_exists(from, to).await
        }
    }

    async fn stores() -> (ObjectStoreBlockStore, ObjectStoreBlockStore, Arc<InMemory>) {
        let object_store = Arc::new(InMemory::new());
        let first =
            ObjectStoreBlockStore::new(object_store.clone(), "vol-a/blocks", false).unwrap();
        let second =
            ObjectStoreBlockStore::new(object_store.clone(), "vol-b/blocks", false).unwrap();
        (first, second, object_store)
    }

    #[tokio::test]
    async fn concurrent_put_claims_share_one_completion() {
        let (store, _, _) = stores().await;
        let id = "bshared";
        let leader = match store.claim_put(id) {
            InFlightPutClaim::Leader(guard) => guard,
            InFlightPutClaim::Follower(_) => panic!("first claim must be the leader"),
        };
        let follower = match store.claim_put(id) {
            InFlightPutClaim::Follower(receiver) => receiver,
            InFlightPutClaim::Leader(_) => panic!("second claim must be a follower"),
        };

        leader.finish(Ok(()));
        wait_for_inflight_put(follower).await.unwrap();
        assert!(matches!(store.claim_put(id), InFlightPutClaim::Leader(_)));
    }

    #[tokio::test]
    async fn canceled_put_claim_releases_followers() {
        let (store, _, _) = stores().await;
        let id = "bcanceled";
        let leader = match store.claim_put(id) {
            InFlightPutClaim::Leader(guard) => guard,
            InFlightPutClaim::Follower(_) => panic!("first claim must be the leader"),
        };
        let follower = match store.claim_put(id) {
            InFlightPutClaim::Follower(receiver) => receiver,
            InFlightPutClaim::Leader(_) => panic!("second claim must be a follower"),
        };

        drop(leader);
        assert!(
            wait_for_inflight_put(follower)
                .await
                .unwrap_err()
                .is(ErrorCode::Eio)
        );
    }

    #[tokio::test]
    async fn put_reuses_a_completed_inflight_upload() {
        let (store, _, _) = stores().await;
        let bytes = b"coalesced bytes";
        let id = block_id(bytes);
        let leader = match store.claim_put(&id) {
            InFlightPutClaim::Leader(guard) => guard,
            InFlightPutClaim::Follower(_) => panic!("first claim must be the leader"),
        };
        let waiting_store = store.clone();
        let put = tokio::spawn(async move { waiting_store.put(bytes).await });
        tokio::task::yield_now().await;

        store.cache.insert(&id, bytes);
        leader.finish(Ok(()));
        assert_eq!(put.await.unwrap().unwrap(), BlockId(id));
        assert_eq!(store.stats().puts, 1);
        assert_eq!(store.stats().successes, 1);
    }

    #[tokio::test]
    async fn puts_are_confirmed_immutable_and_scoped() {
        let (first, second, object_store) = stores().await;
        assert!(!first.durable());
        assert_eq!(first.prefix(), "vol-a/blocks");

        let first_id = first.put(b"first bytes").await.unwrap();
        let second_id = first.put(b"first bytes").await.unwrap();
        assert_eq!(
            first_id, second_id,
            "object-store blocks are content-addressed"
        );
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
        assert_eq!(listing.objects.len(), 1);
        assert!(
            listing
                .objects
                .iter()
                .all(|object| object.location.as_ref().starts_with("vol-a/blocks/b"))
        );
    }

    #[tokio::test]
    async fn conditional_create_replay_checks_existing_bytes_before_success() {
        let object_store = Arc::new(InMemory::new());
        let matching =
            ObjectStoreBlockStore::new(object_store.clone(), "matching/blocks", false).unwrap();
        let conflicting =
            ObjectStoreBlockStore::new(object_store.clone(), "conflicting/blocks", false).unwrap();

        // Exercise distinct object IDs so every case begins with a fresh
        // conditional create against the provider.
        for value in 0..16u8 {
            let bytes = [value];
            let id = block_id(&bytes);
            let matching_path = ObjectPath::from(format!("matching/blocks/{id}"));
            object_store
                .put(&matching_path, PutPayload::from(bytes.to_vec()))
                .await
                .unwrap();
            assert_eq!(matching.put(&bytes).await.unwrap(), BlockId(id.clone()));
            assert_eq!(matching.get(&BlockId(id.clone())).await.unwrap(), bytes);

            let conflicting_path = ObjectPath::from(format!("conflicting/blocks/{id}"));
            let different_bytes = [value, 0];
            object_store
                .put(
                    &conflicting_path,
                    PutPayload::from(different_bytes.to_vec()),
                )
                .await
                .unwrap();
            assert!(
                conflicting
                    .put(&bytes)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Eio),
                "a different object at a content-addressed path must fail closed"
            );
            assert_eq!(
                object_store
                    .get(&conflicting_path)
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap(),
                different_bytes.as_slice(),
                "a failed replay must preserve the existing object"
            );
        }

        assert_eq!(matching.stats().conditional_conflicts, 16);
        assert_eq!(matching.stats().successes, 32);
        assert_eq!(conflicting.stats().conditional_conflicts, 16);
        assert_eq!(conflicting.stats().errors, 16);
    }

    #[tokio::test]
    async fn get_rejects_remote_bytes_that_do_not_match_content_block_id() {
        let backing = Arc::new(InMemory::new());
        let writer = ObjectStoreBlockStore::new(backing.clone(), "volume/blocks", true).unwrap();
        let id = writer.put(b"original block").await.unwrap();
        let path = ObjectPath::from(format!("volume/blocks/{}", id.0));
        backing
            .put(&path, PutPayload::from(b"corrupted block".to_vec()))
            .await
            .unwrap();

        let reader = ObjectStoreBlockStore::new(backing.clone(), "volume/blocks", true).unwrap();
        let error = reader
            .get(&id)
            .await
            .expect_err("content-addressed block bytes must be verified before returning");
        assert!(error.is(ErrorCode::Eio));

        backing
            .put(&path, PutPayload::from(b"original block".to_vec()))
            .await
            .unwrap();
        assert_eq!(reader.get(&id).await.unwrap(), b"original block");
    }

    #[tokio::test]
    async fn get_preserves_legacy_short_block_ids_without_a_full_digest() {
        let backing = Arc::new(InMemory::new());
        let id = BlockId("b0123456789abcdef0123456789abcdef".to_owned());
        backing
            .put(
                &ObjectPath::from(format!("volume/blocks/{}", id.0)),
                PutPayload::from(b"legacy block".to_vec()),
            )
            .await
            .unwrap();

        let reader = ObjectStoreBlockStore::new(backing, "volume/blocks", true).unwrap();
        assert_eq!(reader.get(&id).await.unwrap(), b"legacy block");
    }

    #[tokio::test]
    async fn cached_put_recreates_a_block_deleted_by_another_client() {
        let backing = Arc::new(InMemory::new());
        let writer = ObjectStoreBlockStore::new(backing.clone(), "volume/blocks", true).unwrap();
        let body = b"reused after remote deletion";
        let id = writer.put(body).await.unwrap();
        let path = ObjectPath::from(format!("volume/blocks/{}", id.0));
        backing.delete(&path).await.unwrap();

        assert_eq!(writer.put(body).await.unwrap(), id);
        let fresh_reader = ObjectStoreBlockStore::new(backing, "volume/blocks", true).unwrap();
        assert_eq!(fresh_reader.get(&id).await.unwrap(), body);
    }

    #[tokio::test]
    async fn cached_put_rejects_a_replaced_remote_block() {
        let backing = Arc::new(InMemory::new());
        let writer = ObjectStoreBlockStore::new(backing.clone(), "volume/blocks", true).unwrap();
        let body = b"original immutable block";
        let id = writer.put(body).await.unwrap();
        let path = ObjectPath::from(format!("volume/blocks/{}", id.0));
        backing
            .put(&path, PutPayload::from(b"different remote bytes".to_vec()))
            .await
            .unwrap();

        let error = writer
            .put(body)
            .await
            .expect_err("cached bytes do not prove the remote object still has that identity");
        assert!(error.is(ErrorCode::Eio));
        let read_error = writer
            .get(&id)
            .await
            .expect_err("a verified remote collision must invalidate the writer's cached block");
        assert!(read_error.is(ErrorCode::Eio));
    }

    #[tokio::test]
    async fn volatility_is_explicit_and_missing_or_invalid_blocks_fail_closed() {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let volatile = ObjectStoreBlockStore::new(object_store.clone(), "blocks", false).unwrap();
        let unrelated: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let declared_durable = ObjectStoreBlockStore::new(unrelated, "blocks", true).unwrap();
        assert!(!volatile.durable());
        assert!(declared_durable.durable());
        assert!(
            volatile
                .prepare_concurrent_mode()
                .await
                .expect_err("an injected object store does not prove a shared backing")
                .is(ErrorCode::Enotsup)
        );
        assert!(
            declared_durable
                .prepare_concurrent_mode()
                .await
                .expect_err("durability declarations do not prove a shared backing")
                .is(ErrorCode::Enotsup)
        );

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
        assert!(
            ObjectStoreBlockStore::new(Arc::new(InMemory::new()), "blocks/../other", false,)
                .is_err()
        );
    }

    #[tokio::test]
    async fn configured_prefix_probe_is_idempotent_and_detects_a_changed_object() {
        let backing = InMemory::new();
        probe_configured_concurrent_prefix(&backing, "volume/blocks")
            .await
            .unwrap();
        probe_configured_concurrent_prefix(&backing, "volume/blocks")
            .await
            .unwrap();
        let path = ObjectPath::from(format!("volume/blocks/{CONCURRENT_PROBE_NAME}"));
        backing
            .put(&path, PutPayload::from(b"changed".to_vec()))
            .await
            .unwrap();
        assert!(
            probe_configured_concurrent_prefix(&backing, "volume/blocks")
                .await
                .expect_err("a service changing the capability marker must fail closed")
                .is(ErrorCode::Eio)
        );
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
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.retry_exhausted, 0);
        assert_eq!(
            stats
                .error_classes
                .get(&ObjectStoreBlockStoreErrorClass::NotFound),
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

    #[tokio::test]
    async fn reconciliation_retains_a_block_with_an_invalid_pre_epoch_timestamp() {
        let backing = Arc::new(InMemory::new());
        let store = ObjectStoreBlockStore::new(
            Arc::new(PreEpochListStore {
                inner: backing.clone(),
            }),
            "blocks",
            false,
        )
        .unwrap();
        let id = store.put(b"cannot establish age").await.unwrap();
        let report = store
            .reconcile(&BTreeSet::new(), std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(report.recent, 1);
        assert_eq!(report.deleted, 0);
        backing
            .head(&store.object_path(&id).unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reconciliation_only_deletes_old_direct_valid_ids_in_its_scope() {
        let object_store = Arc::new(InMemory::new());
        let store =
            ObjectStoreBlockStore::new(object_store.clone(), "vol-a/blocks", false).unwrap();
        let live_id = store.put(b"live").await.unwrap();
        let dead_id = store.put(b"dead").await.unwrap();
        let nested = ObjectPath::from(format!("vol-a/blocks/nested/{}", block_id(b"nested")));
        let sibling = ObjectPath::from(format!("vol-a/blocks-other/{}", block_id(b"sibling")));
        let malformed = ObjectPath::from("vol-a/blocks/bINVALID");

        for path in [&nested, &sibling, &malformed] {
            object_store
                .put(path, PutPayload::from(b"unmanaged".to_vec()))
                .await
                .unwrap();
        }

        // The in-memory store timestamps objects at millisecond precision.
        // Let every object age past a 1ms grace so the path/live decisions are
        // exercised rather than the recent-object guard.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let report = store
            .reconcile(
                &BTreeSet::from([live_id.clone()]),
                std::time::Duration::from_millis(1),
            )
            .await
            .unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.protected, 1);
        assert_eq!(report.recent, 0);
        assert_eq!(report.deleted, 1);
        assert_eq!(store.get(&live_id).await.unwrap(), b"live");
        assert!(store.get(&dead_id).await.unwrap_err().is(ErrorCode::Enoent));
        for path in [&nested, &sibling, &malformed] {
            assert!(object_store.head(path).await.is_ok(), "{path} must survive");
        }
    }
}
