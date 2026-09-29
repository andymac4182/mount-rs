use crate::owned::{OwnedTask, notify_waker};
use crate::*;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Poll, Waker},
};
#[cfg(unix)]
use std::{
    fs::{self, OpenOptions},
    io::Write,
};
pub type CacheKey = [u8; 32];
#[derive(Clone, Debug)]
pub struct LocalCacheConfig {
    pub directory: PathBuf,
    pub memory_bytes: usize,
    pub disk_bytes: usize,
    pub max_entries: usize,
    pub max_blob_bytes: usize,
}
struct Entry {
    bytes: Option<Arc<[u8]>>,
    size: usize,
    disk: bool,
    scope: Option<CacheKey>,
    tick: u64,
    generation: u64,
    last_advertised: Option<std::time::Instant>,
}
#[derive(Default)]
struct State {
    entries: BTreeMap<CacheKey, Entry>,
    memory: usize,
    disk: usize,
    tick: u64,
    generation: u64,
    quarantined: BTreeMap<String, usize>,
    scopes: BTreeMap<CacheKey, ScopeRegistration>,
    identities: BTreeMap<ScopeIdentity, IdentityRecord>,
    admission_failed: bool,
}

struct AdmissionOwner;
struct IdentityRecord {
    identity: Arc<ScopeIdentity>,
    epoch: u64,
    backing: Option<ConcurrentBackingId>,
}
struct ScopeRegistration {
    scope: Arc<CacheScope>,
    policy: IntegrityPolicy,
    groups: usize,
    epoch: u64,
}
/// A trusted local identity reservation captured before provider verification.
/// The cache-owner marker prevents a token from authorizing another cache.
/// Reservations survive an empty scope registry and are never recycled.
#[derive(Clone)]
pub struct IdentityEpoch {
    owner: Arc<AdmissionOwner>,
    identity: Arc<ScopeIdentity>,
    epoch: u64,
}
/// One independently owned registration group. Cloning shares its final Drop;
/// it does not add a registration, allocate a new group, or change authority.
#[derive(Clone)]
pub struct ScopeLease {
    inner: Arc<ScopeLeaseInner>,
}
struct ScopeLeaseInner {
    cache: Arc<LocalCache>,
    scope: Arc<CacheScope>,
    key: CacheKey,
    policy: IntegrityPolicy,
    epoch: u64,
}
impl ScopeLease {
    #[cfg(all(test, unix))]
    pub(crate) fn same_group(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub fn policy(&self) -> IntegrityPolicy {
        self.inner.policy
    }
    pub(crate) fn accepts_verified_epoch(&self, verified_from: &IdentityEpoch) -> Result<bool> {
        if !Arc::ptr_eq(&verified_from.owner, &self.inner.cache.admission_owner)
            || *verified_from.identity != self.inner.scope.identity
            || verified_from.epoch != self.inner.epoch
        {
            return Ok(false);
        }
        self.is_current()
    }
    pub fn is_current(&self) -> Result<bool> {
        self.inner.cache.with_admission_state(|state| {
            Ok(state
                .identities
                .get(&self.inner.scope.identity)
                .is_some_and(|identity| {
                    identity.epoch == self.inner.epoch
                        && identity.backing == Some(self.inner.scope.backing)
                        && state.scopes.get(&self.inner.key).is_some_and(|entry| {
                            *entry.scope == *self.inner.scope
                                && entry.policy == self.inner.policy
                                && entry.epoch == self.inner.epoch
                                && entry.groups != 0
                        })
                }))
        })
    }
}
impl Drop for ScopeLeaseInner {
    fn drop(&mut self) {
        // Drop is administrative metadata release, never a disk operation. An
        // old generation cannot decrement a replacement with the same digest.
        if let Ok(mut state) = self.cache.state.lock()
            && let Some(entry) = state.scopes.get_mut(&self.key)
            && *entry.scope == *self.scope
            && entry.policy == self.policy
            && entry.epoch == self.epoch
        {
            if let Some(remaining) = entry.groups.checked_sub(1) {
                entry.groups = remaining;
                if remaining == 0 {
                    state.scopes.remove(&self.key);
                }
            } else {
                state.admission_failed = true;
            }
        }
    }
}

pub struct PendingReservation {
    used: Arc<AtomicUsize>,
    bytes: usize,
}
impl Drop for PendingReservation {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
#[cfg(all(test, unix))]
pub(crate) struct TestDiskInsertGate {
    pub(crate) entered: tokio::sync::oneshot::Sender<()>,
    pub(crate) release: std::sync::mpsc::Receiver<()>,
}

struct IoState {
    sealed: bool,
    permits: usize,
    workers: Vec<Arc<OwnedTask>>,
    failure: Option<FsError>,
}
struct IoRegistry {
    state: Mutex<IoState>,
    changed: Arc<tokio::sync::Notify>,
    wake: Waker,
}
impl IoRegistry {
    fn new() -> Arc<Self> {
        let changed = Arc::new(tokio::sync::Notify::new());
        let wake = notify_waker(&changed);
        Arc::new(Self {
            state: Mutex::new(IoState {
                sealed: false,
                permits: 0,
                workers: Vec::with_capacity(8),
                failure: None,
            }),
            changed,
            wake,
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, IoState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.sealed = true;
                state.failure.get_or_insert_with(error);
                state
            }
        }
    }
    fn reap(&self, state: &mut IoState) {
        let IoState {
            workers,
            sealed,
            failure,
            ..
        } = state;
        workers.retain(|worker| match worker.poll_result() {
            Poll::Pending => true,
            Poll::Ready(result) => {
                if let Err(error) = result {
                    *sealed = true;
                    failure.get_or_insert(error);
                }
                false
            }
        });
    }
}

/// An admitted IO slot. Dropping it wakes an owned shutdown even when the
/// request that acquired it was canceled before submitting a blocking worker.
pub struct CacheIoPermit {
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    registry: Arc<IoRegistry>,
}
impl Drop for CacheIoPermit {
    fn drop(&mut self) {
        let mut state = self.registry.lock();
        if let Some(remaining) = state.permits.checked_sub(1) {
            state.permits = remaining;
        } else {
            state.sealed = true;
            state.failure.get_or_insert_with(error);
        }
        drop(state);
        // Publish the accounting change before making its physical slot
        // available to another admission; the registry count stays <=8.
        drop(self.permit.take());
        self.registry.changed.notify_waiters();
    }
}

/// Only the blocking launcher can create this view. Its exact cache owner and
/// admitted slot remain alive until the callback returns, including after the
/// cache seals admission. Callbacks borrow it and cannot take or clone its slot.
pub(crate) struct AdmittedCache {
    cache: Arc<LocalCache>,
    _permit: CacheIoPermit,
}
impl AdmittedCache {
    pub(crate) fn get_disk(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        policy: IntegrityPolicy,
    ) -> Option<Vec<u8>> {
        self.cache.get_disk_admitted(scope, id, policy)
    }
    #[cfg(test)]
    fn insert(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        bytes: &[u8],
        policy: IntegrityPolicy,
    ) -> Result<()> {
        self.cache.insert_admitted(scope, id, bytes, policy)
    }
    pub(crate) fn insert_shared(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        bytes: Arc<[u8]>,
        policy: IntegrityPolicy,
    ) -> Result<()> {
        self.cache.insert_shared_admitted(scope, id, bytes, policy)
    }
    pub(crate) fn invalidate(&self, scope: &CacheScope, id: &BlockId) {
        self.cache.invalidate_admitted(scope, id)
    }
    pub(crate) fn invalidate_scope(&self, scope: &CacheScope) {
        self.cache.invalidate_scope_admitted(scope)
    }
}

/// One bounded cache shared across drives. Disk operations serialize independently
/// of the RAM index; no filesystem operation holds the index mutex.
pub struct LocalCache {
    config: LocalCacheConfig,
    max_scopes: usize,
    admission_owner: Arc<AdmissionOwner>,
    state: Mutex<State>,
    disk_io: Mutex<()>,
    root: File,
    _lock: File,
    pending: Arc<AtomicUsize>,
    pub(crate) io_permits: Arc<tokio::sync::Semaphore>,
    io: Arc<IoRegistry>,
    #[cfg(all(test, unix))]
    pub(crate) test_disk_insert_gate: Mutex<Option<TestDiskInsertGate>>,
    #[cfg(all(test, unix))]
    pub(crate) test_fill_worker: Mutex<Option<tokio::sync::oneshot::Sender<Arc<OwnedTask>>>>,
}
impl LocalCache {
    /// The original constructor keeps the historical distinct-scope bound.
    pub fn new(config: LocalCacheConfig) -> Result<Arc<Self>> {
        let max_scopes = config.max_entries;
        Self::new_with_scope_capacity(config, max_scopes)
    }
    /// Scope/identity metadata has its own bound; this never changes blob entry,
    /// RAM, disk, payload, pending-buffer or eight-worker IO budgets.
    #[cfg(not(unix))]
    pub fn new_with_scope_capacity(
        _config: LocalCacheConfig,
        _max_scopes: usize,
    ) -> Result<Arc<Self>> {
        Err(FsError::new(ErrorCode::Enotsup).with_syscall("blob cache"))
    }
    #[cfg(unix)]
    pub fn new_with_scope_capacity(
        config: LocalCacheConfig,
        max_scopes: usize,
    ) -> Result<Arc<Self>> {
        if config.max_entries == 0
            || config.max_entries > 1_000_000
            || max_scopes == 0
            || max_scopes > 1_000_000
            || config.max_blob_bytes == 0
            || config.max_blob_bytes > 256 * 1024 * 1024
            || config.memory_bytes > isize::MAX as usize
            || config.disk_bytes > isize::MAX as usize
        {
            return Err(error());
        }
        if let Ok(m) = fs::symlink_metadata(&config.directory)
            && (m.file_type().is_symlink() || !m.is_dir())
        {
            return Err(error());
        }
        fs::create_dir_all(&config.directory).map_err(|_| error())?;
        let root = options()
            .read(true)
            .open(&config.directory)
            .map_err(|_| error())?;
        let marker = config.directory.join(".mount-rs-blob-cache-v1");
        if !marker.exists() {
            if fs::read_dir(&config.directory)
                .map_err(|_| error())?
                .next()
                .is_some()
            {
                return Err(error());
            }
            let mut f = options()
                .write(true)
                .create_new(true)
                .open(&marker)
                .map_err(|_| error())?;
            f.write_all(b"mount-rs-blob-cache-v1\n")
                .map_err(|_| error())?;
        }
        let mut marker_text = String::new();
        open_relative(&root, ".mount-rs-blob-cache-v1", false, false)?
            .take(128)
            .read_to_string(&mut marker_text)
            .map_err(|_| error())?;
        if marker_text != "mount-rs-blob-cache-v1\n" {
            return Err(error());
        }
        let lock = open_relative(&root, ".lock", true, true)?;
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::PermissionsExt;
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(error());
            }
            root.set_permissions(fs::Permissions::from_mode(0o700))
                .map_err(|_| error())?;
        }
        let mut state = State::default();
        for entry in fs::read_dir(&config.directory).map_err(|_| error())? {
            let entry = entry.map_err(|_| error())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.strip_suffix(".tmp").is_some_and(is_key) {
                unlink_relative(&root, &name)?;
                continue;
            }
            if !is_key(&name) {
                continue;
            }
            let f = match open_relative(&root, &name, false, false) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let metadata = f.metadata().map_err(|_| error())?;
            if !metadata.is_file() {
                continue;
            }
            let size = usize::try_from(metadata.len().saturating_sub(32)).map_err(|_| error())?;
            let total = size.checked_add(32).ok_or_else(error)?;
            if metadata.len() < 32
                || size > config.max_blob_bytes
                || total > config.disk_bytes.saturating_sub(state.disk)
                || state.entries.len() >= config.max_entries
            {
                unlink_relative(&root, &name)?;
                continue;
            }
            let key = parse_key(&name).ok_or_else(error)?;
            state.disk += total;
            state.tick += 1;
            state.generation += 1;
            state.entries.insert(
                key,
                Entry {
                    bytes: None,
                    size,
                    disk: true,
                    scope: None,
                    tick: state.tick,
                    generation: state.generation,
                    last_advertised: None,
                },
            );
        }
        Ok(Arc::new(Self {
            config,
            max_scopes,
            admission_owner: Arc::new(AdmissionOwner),
            state: Mutex::new(state),
            disk_io: Mutex::new(()),
            root,
            _lock: lock,
            pending: Arc::new(AtomicUsize::new(0)),
            io_permits: Arc::new(tokio::sync::Semaphore::new(8)),
            io: IoRegistry::new(),
            #[cfg(all(test, unix))]
            test_disk_insert_gate: Mutex::new(None),
            #[cfg(all(test, unix))]
            test_fill_worker: Mutex::new(None),
        }))
    }
    pub fn reserve_pending(&self, payload: usize) -> Option<PendingReservation> {
        if payload > self.config.max_blob_bytes {
            return None;
        }
        let bytes = payload.max(1).checked_add(128)?;
        let limit = self.config.memory_bytes.max(self.config.max_blob_bytes);
        self.pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                mount_rs_core::storage::checked_buffer_reservation(used, payload, 128, limit)
            })
            .ok()?;
        Some(PendingReservation {
            used: self.pending.clone(),
            bytes,
        })
    }
    pub fn max_blob_bytes(&self) -> usize {
        self.config.max_blob_bytes
    }
    pub async fn io_permit(&self) -> Result<CacheIoPermit> {
        let permit = self
            .io_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| FsError::new(ErrorCode::Ebusy).with_syscall("closed cache IO"))?;
        self.admit_permit(permit)
    }
    fn try_io_permit(&self) -> Result<CacheIoPermit> {
        let permit = self
            .io_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| FsError::new(ErrorCode::Ebusy).with_syscall("cache IO admission"))?;
        self.admit_permit(permit)
    }
    fn admit_permit(&self, permit: tokio::sync::OwnedSemaphorePermit) -> Result<CacheIoPermit> {
        let mut state = self.io.lock();
        self.io.reap(&mut state);
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.sealed {
            return Err(FsError::new(ErrorCode::Ebusy).with_syscall("closed cache IO"));
        }
        state.permits = state
            .permits
            .checked_add(1)
            .filter(|count| *count <= 8)
            .ok_or_else(error)?;
        Ok(CacheIoPermit {
            permit: Some(permit),
            registry: self.io.clone(),
        })
    }
    fn start_blocking<R: Send + 'static>(
        self: &Arc<Self>,
        state: &mut IoState,
        permit: CacheIoPermit,
        work: impl FnOnce(&AdmittedCache) -> R + Send + 'static,
    ) -> (Arc<OwnedTask>, tokio::sync::oneshot::Receiver<R>) {
        let cache = AdmittedCache {
            cache: self.clone(),
            _permit: permit,
        };
        let (send, receive) = tokio::sync::oneshot::channel();
        let handle = tokio::task::spawn_blocking(move || {
            let result = work(&cache);
            let _ = send.send(result);
            Ok(())
        });
        let worker = OwnedTask::with_signal(handle, self.io.changed.clone(), self.io.wake.clone());
        state.workers.push(worker.clone());
        #[cfg(all(test, unix))]
        if let Some(retained) = self
            .test_fill_worker
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            let _ = retained.send(worker.clone());
        }
        (worker, receive)
    }
    /// Submit optional work without waiting for a slot or disk completion.
    /// Actual joins remain bounded by the same eight IO slots, even if callers
    /// abandon their result or a blocking closure panics.
    pub(crate) fn try_spawn_blocking<R: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(&AdmittedCache) -> R + Send + 'static,
    ) -> Result<()> {
        let raw = self
            .io_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| FsError::new(ErrorCode::Ebusy))?;
        let permit = self.admit_permit(raw)?;
        let mut state = self.io.lock();
        self.io.reap(&mut state);
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.sealed || state.workers.len() == 8 {
            return Err(FsError::new(ErrorCode::Ebusy).with_syscall("cache worker admission"));
        }
        let (_, receive) = self.start_blocking(&mut state, permit, work);
        drop(receive);
        Ok(())
    }
    pub(crate) async fn run_blocking<R: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(&AdmittedCache) -> R + Send + 'static,
    ) -> Result<R> {
        let permit = self.io_permit().await?;
        let (worker, receive) = loop {
            let changed = self.io.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.io.lock();
                self.io.reap(&mut state);
                if let Some(error) = &state.failure {
                    return Err(error.clone());
                }
                if state.sealed {
                    return Err(
                        FsError::new(ErrorCode::Ebusy).with_syscall("cache worker admission")
                    );
                }
                if state.workers.len() < 8 {
                    break self.start_blocking(&mut state, permit, work);
                }
            }
            changed.await;
        };
        let result = receive.await;
        worker.join().await?;
        result.map_err(|_| error())
    }
    pub fn scope_hash(scope: &CacheScope) -> CacheKey {
        hash_parts(&[
            scope.identity.cluster.as_bytes(),
            scope.identity.partition.as_bytes(),
            scope.identity.drive.as_bytes(),
            &scope.backing.as_bytes(),
        ])
    }
    /// Preserves the original digest filename contract without heap allocations.
    pub fn key_hashed(scope: &CacheKey, id: &BlockId) -> CacheKey {
        let mut hex = [0u8; 64];
        const DIGITS: &[u8] = b"0123456789abcdef";
        for (i, b) in scope.iter().enumerate() {
            hex[2 * i] = DIGITS[usize::from(*b >> 4)];
            hex[2 * i + 1] = DIGITS[usize::from(*b & 15)];
        }
        hash_parts(&[&hex, id.0.as_bytes()])
    }
    fn key(scope: &CacheScope, id: &BlockId) -> CacheKey {
        Self::key_hashed(&Self::scope_hash(scope), id)
    }
    fn valid_identity(identity: &ScopeIdentity) -> bool {
        [&identity.cluster, &identity.partition, &identity.drive]
            .iter()
            .all(|part| !part.is_empty() && part.len() <= 1024)
    }
    fn with_admission_state<T>(&self, work: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        // This short metadata operation shares the seal linearization point but
        // acquires no disk permit. Lock order is always IO registry -> RAM index.
        let mut io = self.io.lock();
        self.io.reap(&mut io);
        if let Some(failure) = &io.failure {
            return Err(failure.clone());
        }
        if io.sealed {
            return Err(FsError::new(ErrorCode::Ebusy).with_syscall("closed cache admission"));
        }
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                io.sealed = true;
                let failure = io.failure.get_or_insert_with(error).clone();
                return Err(failure);
            }
        };
        if state.admission_failed {
            io.sealed = true;
            let failure = io.failure.get_or_insert_with(error).clone();
            return Err(failure);
        }
        let result = work(&mut state);
        if state.admission_failed {
            io.sealed = true;
            let failure = io.failure.get_or_insert_with(error).clone();
            return Err(failure);
        }
        result
    }
    /// Only trusted configured/runtime identities may reserve an epoch. Peer
    /// request fields are not authority and must be checked by their route map.
    pub fn identity_epoch(&self, identity: &ScopeIdentity) -> Result<IdentityEpoch> {
        if !Self::valid_identity(identity) {
            return Err(error());
        }
        self.with_admission_state(|state| {
            // Existing reservations need only borrowed lookup and Arc clones.
            if let Some(record) = state.identities.get(identity) {
                return Ok(IdentityEpoch {
                    owner: self.admission_owner.clone(),
                    identity: record.identity.clone(),
                    epoch: record.epoch,
                });
            }
            if state.identities.len() >= self.max_scopes {
                return Err(FsError::new(ErrorCode::Ebusy).with_syscall("cache identity capacity"));
            }
            let record =
                state
                    .identities
                    .entry(identity.clone())
                    .or_insert_with(|| IdentityRecord {
                        identity: Arc::new(identity.clone()),
                        epoch: 0,
                        backing: None,
                    });
            Ok(IdentityEpoch {
                owner: self.admission_owner.clone(),
                identity: record.identity.clone(),
                epoch: record.epoch,
            })
        })
    }
    /// Publish an acknowledged local proof only if its pre-await identity epoch
    /// is still current. A different backing advances authority before capacity
    /// admission and removes all older backing scopes of this exact identity.
    pub fn register_scope_lease(
        self: &Arc<Self>,
        scope: CacheScope,
        policy: IntegrityPolicy,
        verified_from: IdentityEpoch,
    ) -> Result<ScopeLease> {
        if !Arc::ptr_eq(&verified_from.owner, &self.admission_owner)
            || *verified_from.identity != scope.identity
            || !Self::valid_identity(&scope.identity)
        {
            return Err(error());
        }
        self.with_admission_state(|state| {
            let record = state.identities.get(&scope.identity).ok_or_else(error)?;
            if record.epoch != verified_from.epoch {
                return Err(FsError::new(ErrorCode::Estale).with_syscall("cache authority epoch"));
            }
            if record.backing != Some(scope.backing) {
                let had_previous_backing = record.backing.is_some();
                let Some(next) = record.epoch.checked_add(1) else {
                    state.admission_failed = true;
                    return Err(error());
                };
                let record = state
                    .identities
                    .get_mut(&scope.identity)
                    .ok_or_else(error)?;
                record.epoch = next;
                record.backing = Some(scope.backing);
                // None can arise only at initial reservation or after an
                // exact identity revoke already removed every scope. Avoid
                // quadratic first-admission scans across distinct cold Drives.
                if had_previous_backing {
                    state
                        .scopes
                        .retain(|_, entry| entry.scope.identity != scope.identity);
                }
            }
            let epoch = state
                .identities
                .get(&scope.identity)
                .ok_or_else(error)?
                .epoch;
            let key = Self::scope_hash(&scope);
            let shared_scope = if let Some(entry) = state.scopes.get_mut(&key) {
                // Digest equality alone is never an authority match.
                if *entry.scope != scope || entry.policy != policy || entry.epoch != epoch {
                    return Err(error());
                }
                let Some(groups) = entry.groups.checked_add(1) else {
                    state.admission_failed = true;
                    return Err(error());
                };
                entry.groups = groups;
                entry.scope.clone()
            } else {
                if state.scopes.len() >= self.max_scopes {
                    return Err(FsError::new(ErrorCode::Ebusy).with_syscall("cache scope capacity"));
                }
                let shared_scope = Arc::new(scope);
                state.scopes.insert(
                    key,
                    ScopeRegistration {
                        scope: shared_scope.clone(),
                        policy,
                        groups: 1,
                        epoch,
                    },
                );
                shared_scope
            };
            Ok(ScopeLease {
                inner: Arc::new(ScopeLeaseInner {
                    cache: self.clone(),
                    scope: shared_scope,
                    key,
                    policy,
                    epoch,
                }),
            })
        })
    }
    /// Acquire an independent group only from an already proven current scope.
    /// This never verifies a backing, reserves an unknown identity or advances
    /// authority. Normal holder reuse clones its retained group instead.
    pub fn current_scope_lease(
        self: &Arc<Self>,
        scope: &CacheScope,
        policy: IntegrityPolicy,
    ) -> Result<Option<ScopeLease>> {
        self.with_admission_state(|state| {
            let Some(record) = state.identities.get(&scope.identity) else {
                return Ok(None);
            };
            if record.backing != Some(scope.backing) {
                return Ok(None);
            }
            let epoch = record.epoch;
            let key = Self::scope_hash(scope);
            let Some(entry) = state.scopes.get_mut(&key) else {
                return Ok(None);
            };
            if *entry.scope != *scope || entry.policy != policy || entry.epoch != epoch {
                return Ok(None);
            }
            let Some(groups) = entry.groups.checked_add(1) else {
                state.admission_failed = true;
                return Err(error());
            };
            entry.groups = groups;
            Ok(Some(ScopeLease {
                inner: Arc::new(ScopeLeaseInner {
                    cache: self.clone(),
                    scope: entry.scope.clone(),
                    key,
                    policy,
                    epoch,
                }),
            }))
        })
    }
    /// An observed local authority refusal revokes this exact identity, even
    /// when no scope is registered. This is metadata-only and remains available
    /// while every disk permit is held. Terminal admission still returns error.
    pub fn revoke_identity_admission(&self, identity: &ScopeIdentity) -> Result<()> {
        if !Self::valid_identity(identity) {
            return Err(error());
        }
        self.with_admission_state(|state| {
            let identity_count = state.identities.len();
            let record = match state.identities.entry(identity.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    if identity_count >= self.max_scopes {
                        return Err(
                            FsError::new(ErrorCode::Ebusy).with_syscall("cache identity capacity")
                        );
                    }
                    entry.insert(IdentityRecord {
                        identity: Arc::new(identity.clone()),
                        epoch: 0,
                        backing: None,
                    })
                }
            };
            let Some(next) = record.epoch.checked_add(1) else {
                state.admission_failed = true;
                return Err(error());
            };
            record.epoch = next;
            record.backing = None;
            state
                .scopes
                .retain(|_, entry| entry.scope.identity != *identity);
            Ok(())
        })
    }
    pub fn scope_policy(&self, scope: &CacheScope) -> Option<IntegrityPolicy> {
        self.with_admission_state(|state| {
            let Some(record) = state.identities.get(&scope.identity) else {
                return Ok(None);
            };
            let policy = state
                .scopes
                .get(&Self::scope_hash(scope))
                .filter(|entry| {
                    *entry.scope == *scope
                        && entry.epoch == record.epoch
                        && record.backing == Some(scope.backing)
                        && entry.groups != 0
                })
                .map(|entry| entry.policy);
            Ok(policy)
        })
        .ok()
        .flatten()
    }
    pub fn path_for(&self, scope: &CacheScope, id: &BlockId) -> PathBuf {
        self.config.directory.join(hex_key(&Self::key(scope, id)))
    }
    /// RAM bytes are immutable and verified at admission; only the returned Vec allocates.
    pub fn get_memory_shared_hashed(&self, key: &CacheKey) -> Option<Arc<[u8]>> {
        let bytes = {
            let mut state = self.state.lock().ok()?;
            state.tick = state.tick.wrapping_add(1);
            let tick = state.tick;
            let entry = state.entries.get_mut(key)?;
            entry.tick = tick;
            entry.bytes.clone()?
        };
        Some(bytes)
    }
    pub fn get_memory_hashed(&self, key: &CacheKey) -> Option<Vec<u8>> {
        self.get_memory_shared_hashed(key)
            .map(|bytes| bytes.to_vec())
    }
    pub fn should_advertise_key(&self, key: &CacheKey, interval: std::time::Duration) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let Some(entry) = state.entries.get_mut(key) else {
            return false;
        };
        let now = std::time::Instant::now();
        if entry
            .last_advertised
            .is_some_and(|last| now.duration_since(last) < interval)
        {
            return false;
        }
        entry.last_advertised = Some(now);
        true
    }
    pub fn get_memory(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        _policy: IntegrityPolicy,
    ) -> Option<Vec<u8>> {
        self.get_memory_hashed(&Self::key(scope, id))
    }
    pub fn get(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        policy: IntegrityPolicy,
    ) -> Result<Option<Vec<u8>>> {
        let _permit = self.try_io_permit()?;
        Ok(self
            .get_memory(scope, id, policy)
            .or_else(|| self.get_disk_admitted(scope, id, policy)))
    }
    pub fn get_disk(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        policy: IntegrityPolicy,
    ) -> Result<Option<Vec<u8>>> {
        let _permit = self.try_io_permit()?;
        Ok(self.get_disk_admitted(scope, id, policy))
    }
    fn get_disk_admitted(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        policy: IntegrityPolicy,
    ) -> Option<Vec<u8>> {
        let key = Self::key(scope, id);
        let _disk = self.disk_io.lock().ok()?;
        let (size, generation) = {
            let mut state = self.state.lock().ok()?;
            state.tick = state.tick.wrapping_add(1);
            let tick = state.tick;
            let entry = state.entries.get_mut(&key)?;
            entry.tick = tick;
            if !entry.disk {
                return None;
            }
            (entry.size, entry.generation)
        };
        let bytes = read_checked(&self.root, &hex_key(&key), size, self.config.max_blob_bytes)
            .ok()
            .filter(|b| policy.verify(id, b).is_ok());
        let mut state = self.state.lock().ok()?;
        if state
            .entries
            .get(&key)
            .is_none_or(|entry| entry.generation != generation)
        {
            return None;
        }
        if let Some(bytes) = &bytes {
            self.admit_shared(&mut state, &key, Arc::from(bytes.as_slice()));
        } else {
            let remove = self.remove_index(&mut state, &key);
            drop(state);
            if let Some(charge) = remove {
                let _ = self.delete_charged(hex_key(&key), charge);
            }
        }
        bytes
    }
    fn admit_shared(&self, state: &mut State, key: &CacheKey, bytes: Arc<[u8]>) {
        if bytes.len() > self.config.memory_bytes {
            return;
        }
        while bytes.len() > self.config.memory_bytes.saturating_sub(state.memory) {
            let old = state
                .entries
                .iter()
                .filter(|(_, e)| e.bytes.is_some())
                .min_by_key(|(_, e)| e.tick)
                .map(|(k, _)| *k);
            let Some(old) = old else { break };
            let entry = state.entries.get_mut(&old).unwrap();
            entry.bytes = None;
            state.memory = state.memory.saturating_sub(entry.size);
            if !entry.disk {
                state.entries.remove(&old);
            }
        }
        if let Some(entry) = state.entries.get_mut(key)
            && entry.bytes.is_none()
        {
            state.memory += bytes.len();
            entry.bytes = Some(bytes);
        }
    }
    pub fn insert(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        bytes: &[u8],
        policy: IntegrityPolicy,
    ) -> Result<()> {
        let _permit = self.try_io_permit()?;
        self.insert_admitted(scope, id, bytes, policy)
    }
    fn insert_admitted(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        bytes: &[u8],
        policy: IntegrityPolicy,
    ) -> Result<()> {
        if bytes.len() > self.config.max_blob_bytes {
            return Err(error());
        }
        self.insert_shared_admitted(scope, id, Arc::from(bytes), policy)
    }
    /// Admit immutable verified bytes to RAM without touching the filesystem.
    /// New entries are skipped when making room would require a disk eviction.
    pub fn insert_memory_shared(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        bytes: Arc<[u8]>,
        policy: IntegrityPolicy,
    ) -> Result<()> {
        if bytes.len() > self.config.max_blob_bytes {
            return Err(error());
        }
        policy.verify(id, &bytes)?;
        if bytes.len() > self.config.memory_bytes {
            return Ok(());
        }
        let key = Self::key(scope, id);
        let mut state = self.state.lock().map_err(|_| error())?;
        if state
            .entries
            .get(&key)
            .is_some_and(|entry| entry.size != bytes.len())
        {
            return Err(error());
        }
        if let Some(entry) = state.entries.get(&key)
            && entry
                .bytes
                .as_ref()
                .is_some_and(|old| !Arc::ptr_eq(old, &bytes) && old.as_ref() != bytes.as_ref())
        {
            return Err(error());
        }
        if !state.entries.contains_key(&key) {
            while state.entries.len() + state.quarantined.len() >= self.config.max_entries {
                let Some(old) = state
                    .entries
                    .iter()
                    .filter(|(_, entry)| !entry.disk)
                    .min_by_key(|(_, entry)| entry.tick)
                    .map(|(key, _)| *key)
                else {
                    return Ok(());
                };
                self.remove_index(&mut state, &old);
            }
            state.generation = state.generation.checked_add(1).ok_or_else(error)?;
            let generation = state.generation;
            state.tick = state.tick.wrapping_add(1);
            let tick = state.tick;
            state.entries.insert(
                key,
                Entry {
                    bytes: None,
                    size: bytes.len(),
                    disk: false,
                    scope: Some(Self::scope_hash(scope)),
                    tick,
                    generation,
                    last_advertised: None,
                },
            );
        }
        self.admit_shared(&mut state, &key, bytes);
        Ok(())
    }
    pub fn insert_shared(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        bytes: Arc<[u8]>,
        policy: IntegrityPolicy,
    ) -> Result<()> {
        let _permit = self.try_io_permit()?;
        self.insert_shared_admitted(scope, id, bytes, policy)
    }
    fn insert_shared_admitted(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        bytes: Arc<[u8]>,
        policy: IntegrityPolicy,
    ) -> Result<()> {
        if bytes.len() > self.config.max_blob_bytes {
            return Err(error());
        }
        policy.verify(id, &bytes)?;
        let key = Self::key(scope, id);
        let disk_bytes = bytes.len().checked_add(32).ok_or_else(error)?;
        let memory = bytes.len() <= self.config.memory_bytes;
        let _disk = self.disk_io.lock().map_err(|_| error())?;
        #[cfg(all(test, unix))]
        {
            let gate = self
                .test_disk_insert_gate
                .lock()
                .map_err(|_| error())?
                .take();
            if let Some(gate) = gate {
                let _ = gate.entered.send(());
                // Dropping the test's release sender also releases the real worker.
                let _ = gate.release.recv();
            }
        }
        self.retry_quarantine();
        let disk = disk_bytes <= self.config.disk_bytes
            && self
                .state
                .lock()
                .map_err(|_| error())?
                .quarantined
                .is_empty();
        if !disk && !memory {
            return Ok(());
        }
        let mut unlink = Vec::new();
        let generation =
            {
                let mut state = self.state.lock().map_err(|_| error())?;
                if state
                    .entries
                    .get(&key)
                    .is_some_and(|entry| entry.bytes.is_some())
                {
                    let entry = state.entries.get_mut(&key).unwrap();
                    if entry.bytes.as_ref().is_some_and(|old| {
                        !Arc::ptr_eq(old, &bytes) && old.as_ref() != bytes.as_ref()
                    }) {
                        return Err(error());
                    }
                    if entry.disk {
                        entry.disk = false;
                        let charge = entry.size + 32;
                        state.disk = state.disk.saturating_sub(charge);
                        unlink.push((key, charge));
                    }
                } else if let Some(charge) = self.remove_index(&mut state, &key) {
                    unlink.push((key, charge));
                }
                while !state.entries.contains_key(&key)
                    && state.entries.len() + state.quarantined.len() >= self.config.max_entries
                {
                    let Some(old) = state
                        .entries
                        .iter()
                        .min_by_key(|(_, e)| e.tick)
                        .map(|(k, _)| *k)
                    else {
                        return Ok(());
                    };
                    if let Some(charge) = self.remove_index(&mut state, &old) {
                        unlink.push((old, charge));
                    }
                }
                if disk {
                    while disk_bytes > self.config.disk_bytes.saturating_sub(state.disk) {
                        let old = *state
                            .entries
                            .iter()
                            .filter(|(_, e)| e.disk)
                            .min_by_key(|(_, e)| e.tick)
                            .ok_or_else(error)?
                            .0;
                        let entry = state.entries.get_mut(&old).unwrap();
                        entry.disk = false;
                        let charge = entry.size + 32;
                        let empty = entry.bytes.is_none();
                        state.disk = state.disk.saturating_sub(charge);
                        if empty {
                            state.entries.remove(&old);
                        }
                        unlink.push((old, charge));
                    }
                }
                state.generation = state.generation.checked_add(1).ok_or_else(error)?;
                state.generation
            };
        let mut deletion_failed = false;
        for (old, charge) in unlink {
            if self.delete_charged(hex_key(&old), charge).is_err() {
                deletion_failed = true;
            }
        }
        if deletion_failed {
            return Err(error());
        }
        if disk && write_checked(&self.root, &hex_key(&key), &bytes).is_err() {
            let temp = format!("{}.tmp", hex_key(&key));
            if open_relative(&self.root, &temp, false, false).is_ok() {
                self.quarantine(temp, disk_bytes);
            }
            return Err(error());
        }
        let mut state = self.state.lock().map_err(|_| error())?;
        state.tick = state.tick.wrapping_add(1);
        let tick = state.tick;
        if disk {
            state.disk = state.disk.checked_add(disk_bytes).ok_or_else(error)?;
        }
        if let Some(old) = state.entries.remove(&key) {
            if old.bytes.is_some() {
                state.memory = state.memory.saturating_sub(old.size);
            }
            if old.disk {
                state.disk = state.disk.saturating_sub(old.size + 32);
            }
        }
        while state.entries.len() + state.quarantined.len() >= self.config.max_entries {
            let Some(old) = state
                .entries
                .iter()
                .filter(|(_, entry)| !entry.disk)
                .min_by_key(|(_, entry)| entry.tick)
                .map(|(key, _)| *key)
            else {
                if disk {
                    state.disk = state.disk.saturating_sub(disk_bytes);
                }
                drop(state);
                if disk {
                    self.quarantine(hex_key(&key), disk_bytes);
                }
                return Ok(());
            };
            self.remove_index(&mut state, &old);
        }
        state.entries.insert(
            key,
            Entry {
                bytes: None,
                size: bytes.len(),
                disk,
                scope: Some(Self::scope_hash(scope)),
                tick,
                generation,
                last_advertised: None,
            },
        );
        if memory {
            self.admit_shared(&mut state, &key, bytes);
        }
        Ok(())
    }
    // Disk errors retain both byte and entry charges until an owned artifact can
    // actually be removed. No new disk write occurs while quarantine is nonempty.
    fn quarantine(&self, name: String, charge: usize) {
        if let Ok(mut state) = self.state.lock()
            && !state.quarantined.contains_key(&name)
        {
            // A preserved RAM hint and its failed disk replacement represent
            // one entry. Quarantine owns that entry charge until deletion works.
            if let Some(key) = parse_key(name.strip_suffix(".tmp").unwrap_or(&name)) {
                self.remove_index(&mut state, &key);
            }
            state.disk = state.disk.saturating_add(charge);
            state.quarantined.insert(name, charge);
        }
    }
    fn delete_charged(&self, name: String, charge: usize) -> Result<()> {
        if unlink_relative(&self.root, &name).is_err() {
            self.quarantine(name, charge);
            Err(error())
        } else {
            Ok(())
        }
    }
    fn retry_quarantine(&self) {
        let artifacts = self
            .state
            .lock()
            .map(|state| {
                state
                    .quarantined
                    .iter()
                    .map(|(name, charge)| (name.clone(), *charge))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (name, charge) in artifacts {
            if unlink_relative(&self.root, &name).is_ok()
                && let Ok(mut state) = self.state.lock()
                && state.quarantined.remove(&name).is_some()
            {
                state.disk = state.disk.saturating_sub(charge);
            }
        }
    }
    pub fn invalidate(&self, scope: &CacheScope, id: &BlockId) -> Result<()> {
        let _permit = self.try_io_permit()?;
        self.invalidate_admitted(scope, id);
        Ok(())
    }
    /// Invalidate within an already admitted slot, including a batch that holds
    /// every IO slot. The exclusive borrow prevents concurrent slot reuse, and
    /// a permit from another cache cannot authorize this cache's disk work.
    pub fn invalidate_with_permit(
        &self,
        permit: &mut CacheIoPermit,
        scope: &CacheScope,
        id: &BlockId,
    ) -> Result<()> {
        if permit.permit.is_none() || !Arc::ptr_eq(&permit.registry, &self.io) {
            return Err(error().with_syscall("cache IO permit identity"));
        }
        self.invalidate_admitted(scope, id);
        Ok(())
    }
    fn invalidate_admitted(&self, scope: &CacheScope, id: &BlockId) {
        let key = Self::key(scope, id);
        if let Ok(_disk) = self.disk_io.lock() {
            let remove = self
                .state
                .lock()
                .ok()
                .and_then(|mut state| self.remove_index(&mut state, &key));
            if let Some(charge) = remove {
                let _ = self.delete_charged(hex_key(&key), charge);
            }
        }
    }
    pub fn invalidate_scope(&self, scope: &CacheScope) -> Result<()> {
        let _permit = self.try_io_permit()?;
        self.invalidate_scope_admitted(scope);
        Ok(())
    }
    fn invalidate_scope_admitted(&self, scope: &CacheScope) {
        if let Ok(_disk) = self.disk_io.lock() {
            let hash = Self::scope_hash(scope);
            let unlink = if let Ok(mut state) = self.state.lock() {
                let keys = state
                    .entries
                    .iter()
                    .filter(|(_, e)| e.scope.is_none_or(|s| s == hash))
                    .map(|(k, _)| *k)
                    .collect::<Vec<_>>();
                keys.into_iter()
                    .filter_map(|key| {
                        self.remove_index(&mut state, &key)
                            .map(|charge| (key, charge))
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            for (key, charge) in unlink {
                let _ = self.delete_charged(hex_key(&key), charge);
            }
        }
    }
    fn remove_index(&self, state: &mut State, key: &CacheKey) -> Option<usize> {
        let entry = state.entries.remove(key)?;
        if entry.bytes.is_some() {
            state.memory = state.memory.saturating_sub(entry.size);
        }
        if entry.disk {
            let charge = entry.size + 32;
            state.disk = state.disk.saturating_sub(charge);
            Some(charge)
        } else {
            None
        }
    }
    pub fn usage(&self) -> (usize, usize, usize) {
        let state = self.state.lock().unwrap();
        (
            state.memory,
            state.disk,
            state.entries.len() + state.quarantined.len(),
        )
    }
    pub async fn shutdown(&self) -> Result<()> {
        {
            let mut state = self.io.lock();
            state.sealed = true;
            self.io_permits.close();
        }
        let drain = async {
            loop {
                let changed = self.io.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                {
                    let mut state = self.io.lock();
                    self.io.reap(&mut state);
                    if let Some(error) = &state.failure {
                        return Err(error.clone());
                    }
                    if state.workers.is_empty() && state.permits == 0 {
                        return Ok(());
                    }
                }
                changed.await;
            }
        };
        match tokio::time::timeout(std::time::Duration::from_secs(2), drain).await {
            Ok(result) => result,
            Err(_) => {
                let mut state = self.io.lock();
                let failure = FsError::new(ErrorCode::Ebusy).with_syscall("cache IO drain timeout");
                let failure = state.failure.get_or_insert(failure).clone();
                drop(state);
                self.io.changed.notify_waiters();
                Err(failure)
            }
        }
    }
}
fn hash_parts(parts: &[&[u8]]) -> CacheKey {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p);
    }
    h.finalize().into()
}
fn hex_key(key: &CacheKey) -> String {
    let mut out = String::with_capacity(64);
    for b in key {
        use std::fmt::Write;
        write!(&mut out, "{b:02x}").unwrap();
    }
    out
}
fn parse_key(key: &str) -> Option<CacheKey> {
    if !is_key(key) {
        return None;
    }
    let mut out = [0; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&key[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}
fn is_key(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
#[cfg(unix)]
fn options() -> OpenOptions {
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        o.mode(0o600);
    }
    o
}
#[cfg(unix)]
fn open_relative(root: &File, name: &str, write: bool, create: bool) -> Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = std::ffi::CString::new(name).map_err(|_| error())?;
    let flags = libc::O_NONBLOCK
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if write { libc::O_RDWR } else { libc::O_RDONLY }
        | if create { libc::O_CREAT } else { 0 };
    let fd = unsafe { libc::openat(root.as_raw_fd(), name.as_ptr(), flags, 0o600) };
    if fd < 0 {
        Err(error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
#[cfg(unix)]
fn unlink_relative(root: &File, name: &str) -> Result<()> {
    use std::os::fd::AsRawFd;
    let n = std::ffi::CString::new(name).map_err(|_| error())?;
    if unsafe { libc::unlinkat(root.as_raw_fd(), n.as_ptr(), 0) } < 0
        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound
    {
        Err(error())
    } else {
        Ok(())
    }
}
fn read_checked(root: &File, key: &str, size: usize, max: usize) -> Result<Vec<u8>> {
    let mut f = open_relative(root, key, false, false)?;
    let m = f.metadata().map_err(|_| error())?;
    if !m.is_file() || m.len() != size as u64 + 32 || size > max {
        return Err(error());
    }
    let mut sum = [0; 32];
    f.read_exact(&mut sum).map_err(|_| error())?;
    let mut bytes = vec![0; size];
    f.read_exact(&mut bytes).map_err(|_| error())?;
    if checksum(key, &bytes)?[..] != sum {
        return Err(error());
    }
    Ok(bytes)
}
fn checksum(key: &str, bytes: &[u8]) -> Result<CacheKey> {
    let key = parse_key(key).ok_or_else(error)?;
    let mut hash = Sha256::new();
    hash.update(key);
    hash.update(bytes);
    Ok(hash.finalize().into())
}
#[cfg(unix)]
fn write_checked(root: &File, key: &str, bytes: &[u8]) -> Result<()> {
    use std::os::fd::AsRawFd;
    let temp = format!("{key}.tmp");
    unlink_relative(root, &temp)?;
    let outcome = (|| {
        let mut file = open_relative(root, &temp, true, true)?;
        file.set_len(0).map_err(|_| error())?;
        file.write_all(&checksum(key, bytes)?)
            .and_then(|_| file.write_all(bytes))
            .map_err(|_| error())?;
        let source = std::ffi::CString::new(temp.clone()).unwrap();
        let target = std::ffi::CString::new(key).unwrap();
        if unsafe {
            libc::renameat(
                root.as_raw_fd(),
                source.as_ptr(),
                root.as_raw_fd(),
                target.as_ptr(),
            )
        } < 0
        {
            return Err(error());
        }
        Ok(())
    })();
    if outcome.is_err() {
        let _ = unlink_relative(root, &temp);
    }
    outcome
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn scope(drive: &str) -> CacheScope {
        CacheScope {
            identity: ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: drive.into(),
            },
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
        }
    }
    fn cfg(p: &std::path::Path) -> LocalCacheConfig {
        LocalCacheConfig {
            directory: p.to_path_buf(),
            memory_bytes: 4,
            disk_bytes: 108,
            max_entries: 3,
            max_blob_bytes: 16,
        }
    }
    #[tokio::test]
    async fn public_sync_disk_work_is_drained_before_shutdown_acknowledges() {
        let dir = tempfile::tempdir().unwrap();
        let config = cfg(dir.path());
        let cache = LocalCache::new(config.clone()).unwrap();
        let s = scope("sync-drain");
        let id = BlockId("finished".into());
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        *cache.test_disk_insert_gate.lock().unwrap() = Some(TestDiskInsertGate {
            entered,
            release: blocked,
        });
        let c = cache.clone();
        let write_scope = s.clone();
        let write_id = id.clone();
        let worker = std::thread::spawn(move || {
            c.insert(&write_scope, &write_id, b"done", IntegrityPolicy::Opaque)
        });
        ready.await.unwrap();
        let c = cache.clone();
        let mut close = tokio::spawn(async move { c.shutdown().await });
        let observed = tokio::time::timeout(std::time::Duration::from_millis(50), &mut close).await;
        let retained = LocalCache::new(config.clone()).is_err();
        // Release and positively join the actual disk worker before any final
        // assertion can unwind its fixture or hide a false acknowledgment.
        drop(release);
        let written = worker.join().unwrap();
        let (premature, closed) = match observed {
            Ok(result) => (true, result.unwrap()),
            Err(_) => (false, close.await.unwrap()),
        };
        drop(cache);
        let fresh = LocalCache::new(config).unwrap();
        let bytes = fresh.get_disk(&s, &id, IntegrityPolicy::Opaque).unwrap();
        assert!(written.is_ok());
        assert!(closed.is_ok());
        assert!(
            retained,
            "real synchronous worker must retain the disk owner"
        );
        assert_eq!(bytes.as_deref(), Some(b"done".as_slice()));
        assert!(
            !premature,
            "shutdown acknowledged real public disk work before it drained"
        );
    }
    #[tokio::test]
    async fn public_sync_disk_work_is_rejected_after_shutdown_acknowledges() {
        let dir = tempfile::tempdir().unwrap();
        let config = cfg(dir.path());
        let cache = LocalCache::new(config.clone()).unwrap();
        let s = scope("sealed");
        let kept = BlockId("kept".into());
        let late = BlockId("late".into());
        cache
            .insert(&s, &kept, b"done", IntegrityPolicy::Opaque)
            .unwrap();
        cache.shutdown().await.unwrap();
        let attempts = [
            cache.get(&s, &kept, IntegrityPolicy::Opaque).map(|_| ()),
            cache
                .get_disk(&s, &kept, IntegrityPolicy::Opaque)
                .map(|_| ()),
            cache.insert(&s, &late, b"late", IntegrityPolicy::Opaque),
            cache.insert_shared(
                &s,
                &late,
                Arc::from(b"late".as_slice()),
                IntegrityPolicy::Opaque,
            ),
            cache.invalidate(&s, &kept),
            cache.invalidate_scope(&s),
        ];
        drop(cache);
        let fresh = LocalCache::new(config).unwrap();
        let bytes = fresh.get_disk(&s, &kept, IntegrityPolicy::Opaque).unwrap();
        let absent = fresh.get_disk(&s, &late, IntegrityPolicy::Opaque).unwrap();
        assert_eq!(bytes.as_deref(), Some(b"done".as_slice()));
        assert!(absent.is_none(), "sealed insertion changed the disk");
        for result in attempts {
            assert!(result.is_err_and(|e| e.code == ErrorCode::Ebusy));
        }
    }
    #[tokio::test]
    async fn public_sync_disk_admission_shares_all_eight_slots() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.memory_bytes = 0;
        config.disk_bytes = 8 * 36;
        config.max_entries = 8;
        let cache = LocalCache::new(config.clone()).unwrap();
        let s = scope("sync-capacity");
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        *cache.test_disk_insert_gate.lock().unwrap() = Some(TestDiskInsertGate {
            entered,
            release: blocked,
        });
        let mut workers = Vec::new();
        for index in 0..8 {
            let cache = cache.clone();
            let scope = s.clone();
            workers.push(std::thread::spawn(move || {
                cache.insert(
                    &scope,
                    &BlockId(index.to_string()),
                    b"done",
                    IntegrityPolicy::Opaque,
                )
            }));
        }
        ready.await.unwrap();
        let full = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while cache.io.lock().permits != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        let ninth = full.then(|| {
            cache.insert(
                &s,
                &BlockId("ninth".into()),
                b"late",
                IntegrityPolicy::Opaque,
            )
        });
        let c = cache.clone();
        let mut close = tokio::spawn(async move { c.shutdown().await });
        let observed = tokio::time::timeout(std::time::Duration::from_millis(50), &mut close).await;
        drop(release);
        let written = workers
            .into_iter()
            .map(|worker| worker.join())
            .collect::<Vec<_>>();
        let (premature, closed) = match observed {
            Ok(result) => (true, result.unwrap()),
            Err(_) => (false, close.await.unwrap()),
        };
        let empty = cache.io.lock().permits == 0;
        drop(cache);
        let fresh = LocalCache::new(config).unwrap();
        let bytes = (0..8)
            .map(|index| {
                fresh
                    .get_disk(&s, &BlockId(index.to_string()), IntegrityPolicy::Opaque)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            full,
            "all eight actual synchronous workers must be admitted"
        );
        assert!(
            ninth.is_some_and(|result| result.is_err_and(|error| error.code == ErrorCode::Ebusy))
        );
        assert!(!premature);
        assert!(closed.is_ok());
        assert!(empty);
        assert!(
            written
                .into_iter()
                .all(|result| result.is_ok_and(|result| result.is_ok()))
        );
        assert!(
            bytes
                .into_iter()
                .all(|bytes| bytes.as_deref() == Some(b"done".as_slice()))
        );
    }
    #[tokio::test]
    async fn public_sync_disk_admission_rejects_another_caches_permit() {
        let dir = tempfile::tempdir().unwrap();
        let a = LocalCache::new(cfg(&dir.path().join("a"))).unwrap();
        let b = LocalCache::new(cfg(&dir.path().join("b"))).unwrap();
        let s = scope("permit-identity");
        let id = BlockId("kept".into());
        a.insert(&s, &id, b"done", IntegrityPolicy::Opaque).unwrap();
        let mut wrong = b.io_permit().await.unwrap();
        let result = a.invalidate_with_permit(&mut wrong, &s, &id);
        let bytes = a.get_disk(&s, &id, IntegrityPolicy::Opaque).unwrap();
        drop(wrong);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
        assert!(result.is_err_and(|error| error.code == ErrorCode::Eio));
        assert_eq!(bytes.as_deref(), Some(b"done".as_slice()));
    }
    #[tokio::test]
    async fn cancelled_close_rejoins_actual_worker_and_seals_new_io() {
        let dir = tempfile::tempdir().unwrap();
        let config = cfg(dir.path());
        let cache = LocalCache::new(config.clone()).unwrap();
        let s = scope("drain");
        let id = BlockId("finished".into());
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let c = cache.clone();
        let write_scope = s.clone();
        let write_id = id.clone();
        let request = tokio::spawn(async move {
            c.run_blocking(move |cache| {
                entered.send(()).unwrap();
                let _ = blocked.recv();
                cache.insert(&write_scope, &write_id, b"done", IntegrityPolicy::Opaque)
            })
            .await
        });
        ready.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        let c = cache.clone();
        let first = tokio::spawn(async move { c.shutdown().await });
        while !cache.io.lock().sealed {
            tokio::task::yield_now().await;
        }
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let admission = cache
            .run_blocking(|_| panic!("sealed work must not start"))
            .await;
        let c = cache.clone();
        let mut second = tokio::spawn(async move { c.shutdown().await });
        let premature =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut second).await;
        // Unblock the actual worker before assertions that can unwind the fixture.
        drop(release);
        let joined = second.await.unwrap();
        let drained = {
            let state = cache.io.lock();
            state.workers.is_empty() && state.permits == 0
        };
        drop(cache);
        let fresh = LocalCache::new(config).unwrap();
        let bytes = fresh.get_disk(&s, &id, IntegrityPolicy::Opaque).unwrap();
        assert!(admission.is_err_and(|e| e.code == ErrorCode::Ebusy));
        assert!(
            premature.is_err(),
            "second close acknowledged a live real worker"
        );
        assert!(
            joined.is_ok(),
            "second close did not positively join the worker"
        );
        assert!(drained);
        assert_eq!(bytes.as_deref(), Some(b"done".as_slice()));
    }
    #[tokio::test]
    async fn actual_worker_panic_is_sticky_and_keeps_directory_owner() {
        let dir = tempfile::tempdir().unwrap();
        let config = cfg(dir.path());
        let cache = LocalCache::new(config.clone()).unwrap();
        let result = cache
            .run_blocking(|_| panic!("actual cache worker failure"))
            .await;
        let closed = cache.shutdown().await;
        let retry = cache.shutdown().await;
        let retained = LocalCache::new(config.clone()).is_err();
        let reaped = {
            let state = cache.io.lock();
            state.workers.is_empty() && state.permits == 0
        };
        drop(cache);
        let fresh = LocalCache::new(config).unwrap();
        drop(fresh);
        assert!(result.is_err_and(|e| e.code == ErrorCode::Eio));
        assert!(closed.is_err_and(|e| e.code == ErrorCode::Eio));
        assert!(retry.is_err_and(|e| e.code == ErrorCode::Eio));
        assert!(retained, "failed close must leave keeper ownership intact");
        assert!(reaped, "panic must be an observed real task join");
    }
    #[tokio::test]
    async fn actual_worker_registry_is_bounded_by_eight_io_slots() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(cfg(dir.path())).unwrap();
        let mut releases = Vec::new();
        for _ in 0..8 {
            let (entered, ready) = tokio::sync::oneshot::channel();
            let (release, blocked) = std::sync::mpsc::channel::<()>();
            cache
                .try_spawn_blocking(move |_| {
                    entered.send(()).unwrap();
                    let _ = blocked.recv();
                })
                .unwrap();
            ready.await.unwrap();
            releases.push(release);
        }
        let count = cache.io.lock().workers.len();
        let ninth = cache.try_spawn_blocking(|_| panic!("ninth worker must not start"));
        drop(releases);
        let joined = cache.shutdown().await;
        let state = cache.io.lock();
        assert_eq!(count, 8);
        assert!(ninth.is_err_and(|e| e.code == ErrorCode::Ebusy));
        assert!(joined.is_ok());
        assert!(state.workers.is_empty());
        assert_eq!(
            state.workers.capacity(),
            8,
            "completed joins must not accumulate"
        );
    }
    #[test]
    fn ram_hit_does_not_wait_for_a_blocked_disk_open() {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.memory_bytes = 4;
        let cache = LocalCache::new(config).unwrap();
        let s = scope("slow");
        let id = BlockId("slow".into());
        cache
            .insert(&s, &id, b"slow", IntegrityPolicy::Opaque)
            .unwrap();
        let hot = scope("hot");
        let hot_id = BlockId("hot".into());
        cache
            .insert(&hot, &hot_id, b"fast", IntegrityPolicy::Opaque)
            .unwrap();
        let path = cache.path_for(&s, &id);
        fs::remove_file(&path).unwrap();
        let raw = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        let c = cache.clone();
        let (started, ready) = std::sync::mpsc::channel();
        let disk = std::thread::spawn(move || {
            started.send(()).unwrap();
            c.get_disk(&s, &id, IntegrityPolicy::Opaque).unwrap()
        });
        ready.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let c = cache.clone();
        let (send, recv) = std::sync::mpsc::channel();
        let memory = std::thread::spawn(move || {
            send.send(c.get_memory(&hot, &hot_id, IntegrityPolicy::Opaque))
                .unwrap()
        });
        let result = recv.recv_timeout(std::time::Duration::from_millis(100));
        let writer = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path);
        drop(writer);
        disk.join().unwrap();
        memory.join().unwrap();
        assert_eq!(
            result.expect("RAM lookup blocked behind disk IO"),
            Some(b"fast".to_vec())
        );
    }
    #[test]
    fn rejects_overflow_prone_public_configurations() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.max_blob_bytes = usize::MAX;
        assert!(LocalCache::new(config).is_err());
    }
    #[test]
    fn memory_only_admission_does_not_wait_for_disk_mutex() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(cfg(dir.path())).unwrap();
        let _disk = cache.disk_io.lock().unwrap();
        let s = scope("ram");
        let id = BlockId("id".into());
        cache
            .insert_memory_shared(
                &s,
                &id,
                Arc::from(b"data".as_slice()),
                IntegrityPolicy::Opaque,
            )
            .unwrap();
        assert_eq!(
            cache.get_memory(&s, &id, IntegrityPolicy::Opaque),
            Some(b"data".to_vec())
        );
        assert!(!cache.path_for(&s, &id).exists());
    }
    #[test]
    fn opaque_disk_files_cannot_be_swapped_between_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.memory_bytes = 0;
        let cache = LocalCache::new(config).unwrap();
        let id = BlockId("opaque".into());
        let a = scope("a");
        let b = scope("b");
        cache
            .insert(&a, &id, b"aaaa", IntegrityPolicy::Opaque)
            .unwrap();
        cache
            .insert(&b, &id, b"bbbb", IntegrityPolicy::Opaque)
            .unwrap();
        let pa = cache.path_for(&a, &id);
        let pb = cache.path_for(&b, &id);
        let a_bytes = fs::read(&pa).unwrap();
        let b_bytes = fs::read(&pb).unwrap();
        fs::write(&pa, b_bytes).unwrap();
        fs::write(&pb, a_bytes).unwrap();
        assert!(
            cache
                .get_disk(&a, &id, IntegrityPolicy::Opaque)
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .get_disk(&b, &id, IntegrityPolicy::Opaque)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn owned_temporary_quarantine_replaces_the_matching_ram_entry_charge() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.memory_bytes = 8;
        config.max_entries = 2;
        let cache = LocalCache::new(config).unwrap();
        let s = scope("temporary");
        let a = BlockId("a".into());
        let b = BlockId("b".into());
        cache
            .insert_memory_shared(
                &s,
                &a,
                Arc::from(b"aaaa".as_slice()),
                IntegrityPolicy::Opaque,
            )
            .unwrap();
        cache
            .insert_memory_shared(
                &s,
                &b,
                Arc::from(b"bbbb".as_slice()),
                IntegrityPolicy::Opaque,
            )
            .unwrap();
        let temp = format!("{}.tmp", hex_key(&LocalCache::key(&s, &a)));
        cache.quarantine(temp, 36);
        assert_eq!(
            cache.usage(),
            (4, 36, 2),
            "owned temp quarantine must replace matching RAM entry charge"
        );
        assert!(cache.get_memory(&s, &a, IntegrityPolicy::Opaque).is_none());
        assert_eq!(
            cache.get_memory(&s, &b, IntegrityPolicy::Opaque),
            Some(b"bbbb".to_vec())
        );
    }
    #[test]
    fn same_key_failed_eviction_retains_only_one_entry_charge() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.memory_bytes = 8;
        config.disk_bytes = 72;
        config.max_entries = 2;
        let cache = LocalCache::new(config).unwrap();
        let s = scope("same-key");
        let a = BlockId("a".into());
        let b = BlockId("b".into());
        cache
            .insert(&s, &a, b"aaaa", IntegrityPolicy::Opaque)
            .unwrap();
        cache
            .insert(&s, &b, b"bbbb", IntegrityPolicy::Opaque)
            .unwrap();
        let path = cache.path_for(&s, &a);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            cache
                .insert(&s, &a, b"aaaa", IntegrityPolicy::Opaque)
                .is_err()
        );
        assert_eq!(
            cache.usage(),
            (4, 72, 2),
            "RAM and quarantine must not double-count one failed eviction"
        );
        assert!(cache.get_memory(&s, &a, IntegrityPolicy::Opaque).is_none());
        assert_eq!(
            cache.get_memory(&s, &b, IntegrityPolicy::Opaque),
            Some(b"bbbb".to_vec())
        );
    }
    #[test]
    fn eviction_unlink_failure_stops_disk_admission_and_keeps_charge() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.memory_bytes = 0;
        config.disk_bytes = 72;
        config.max_entries = 2;
        let cache = LocalCache::new(config).unwrap();
        let s = scope("failure");
        let a = BlockId("a".into());
        let b = BlockId("b".into());
        let c = BlockId("c".into());
        cache
            .insert(&s, &a, b"aaaa", IntegrityPolicy::Opaque)
            .unwrap();
        cache
            .insert(&s, &b, b"bbbb", IntegrityPolicy::Opaque)
            .unwrap();
        let path = cache.path_for(&s, &a);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let _ = cache.insert(&s, &c, b"cccc", IntegrityPolicy::Opaque);
        assert!(
            !cache.path_for(&s, &c).exists(),
            "failed eviction must not create another disk blob"
        );
        assert_eq!(
            cache.usage().1,
            72,
            "failed deletion must retain disk budget charge"
        );
        fs::remove_dir(path).unwrap();
        cache
            .insert(&s, &c, b"cccc", IntegrityPolicy::Opaque)
            .unwrap();
        assert!(cache.path_for(&s, &c).exists());
        assert!(cache.usage().1 <= 72);
    }
    #[test]
    fn failed_atomic_write_cleans_owned_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(cfg(dir.path())).unwrap();
        let s = scope("temp");
        let id = BlockId("id".into());
        let path = cache.path_for(&s, &id);
        fs::create_dir(&path).unwrap();
        assert!(
            cache
                .insert(&s, &id, b"data", IntegrityPolicy::Opaque)
                .is_err()
        );
        assert!(
            !path.with_extension("tmp").exists(),
            "failed rename must clean owned temp bytes"
        );
        assert!(path.is_dir(), "preexisting directory is preserved");
    }
    #[test]
    fn global_limits_keep_disk_entries_after_ram_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let c = LocalCache::new(cfg(dir.path())).unwrap();
        for d in ["one", "two", "three"] {
            c.insert(
                &scope(d),
                &BlockId("opaque".into()),
                b"abcd",
                IntegrityPolicy::Opaque,
            )
            .unwrap();
        }
        assert_eq!(c.usage(), (4, 108, 3));
        assert_eq!(
            c.get(
                &scope("one"),
                &BlockId("opaque".into()),
                IntegrityPolicy::Opaque
            )
            .unwrap(),
            Some(b"abcd".to_vec())
        );
    }
    #[test]
    fn disk_corruption_is_a_miss_and_warm_restart_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.memory_bytes = 0;
        let c = LocalCache::new(config.clone()).unwrap();
        let s = scope("x");
        let id = BlockId("../../escape".into());
        c.insert(&s, &id, b"data", IntegrityPolicy::Opaque).unwrap();
        let path = c.path_for(&s, &id);
        drop(c);
        let c = LocalCache::new(config).unwrap();
        assert_eq!(
            c.get(&s, &id, IntegrityPolicy::Opaque).unwrap(),
            Some(b"data".to_vec())
        );
        fs::write(path, b"XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXoops").unwrap();
        assert_eq!(c.get(&s, &id, IntegrityPolicy::Opaque).unwrap(), None);
    }
    #[test]
    fn rejects_foreign_directory_and_duplicate_owner() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("important"), b"keep").unwrap();
        assert!(LocalCache::new(cfg(d.path())).is_err());
        assert_eq!(fs::read(d.path().join("important")).unwrap(), b"keep");
        let d = tempfile::tempdir().unwrap();
        let _c = LocalCache::new(cfg(d.path())).unwrap();
        assert!(LocalCache::new(cfg(d.path())).is_err());
    }
    #[test]
    fn final_symlink_never_reads_external_bytes() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let mut config = cfg(d.path());
        config.memory_bytes = 0;
        let c = LocalCache::new(config).unwrap();
        let s = scope("s");
        let id = BlockId("id".into());
        c.insert(&s, &id, b"safe", IntegrityPolicy::Opaque).unwrap();
        let outside = d.path().join("external");
        fs::write(&outside, b"secret").unwrap();
        let path = c.path_for(&s, &id);
        fs::remove_file(&path).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(c.get(&s, &id, IntegrityPolicy::Opaque).unwrap().is_none());
        assert_eq!(fs::read(outside).unwrap(), b"secret");
    }

    fn proof_scope(drive: &str, backing: u8) -> CacheScope {
        CacheScope {
            identity: ScopeIdentity {
                cluster: "epochs".into(),
                partition: "p".into(),
                drive: drive.into(),
            },
            backing: ConcurrentBackingId::from_bytes([backing; 16]).unwrap(),
        }
    }
    fn proof_cache(dir: &tempfile::TempDir, name: &str, scope_capacity: usize) -> Arc<LocalCache> {
        LocalCache::new_with_scope_capacity(
            LocalCacheConfig {
                directory: dir.path().join(name),
                memory_bytes: 1024,
                disk_bytes: 4096,
                max_entries: 1,
                max_blob_bytes: 1024,
            },
            scope_capacity,
        )
        .unwrap()
    }
    fn lease(cache: &Arc<LocalCache>, scope: CacheScope) -> ScopeLease {
        let epoch = cache.identity_epoch(&scope.identity).unwrap();
        cache
            .register_scope_lease(scope, IntegrityPolicy::Opaque, epoch)
            .unwrap()
    }
    #[test]
    fn empty_identity_keeps_revocation_epoch_and_other_identities() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "empty", 4);
        let scope = proof_scope("revoked", 1);
        let held = cache.identity_epoch(&scope.identity).unwrap();
        let other_scope = proof_scope("unaffected", 1);
        let other = lease(&cache, other_scope.clone());
        cache.revoke_identity_admission(&scope.identity).unwrap();
        let stale = cache.register_scope_lease(scope.clone(), IntegrityPolicy::Opaque, held);
        assert!(stale.is_err_and(|e| e.code == ErrorCode::Estale));
        assert!(other.is_current().unwrap());
        assert_eq!(
            cache.scope_policy(&other_scope),
            Some(IntegrityPolicy::Opaque)
        );
        let fresh = lease(&cache, scope.clone());
        assert!(fresh.is_current().unwrap());
        assert_eq!(cache.scope_policy(&scope), Some(IntegrityPolicy::Opaque));
    }
    #[test]
    fn lease_clones_share_one_group_and_independent_groups_release_separately() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "groups", 2);
        let scope = proof_scope("d", 1);
        let first = lease(&cache, scope.clone());
        let cloned = first.clone();
        assert!(Arc::ptr_eq(&first.inner, &cloned.inner));
        assert_eq!(
            cache
                .state
                .lock()
                .unwrap()
                .scopes
                .values()
                .next()
                .unwrap()
                .groups,
            1
        );
        let independent = cache
            .current_scope_lease(&scope, IntegrityPolicy::Opaque)
            .unwrap()
            .unwrap();
        assert!(!Arc::ptr_eq(&first.inner, &independent.inner));
        assert_eq!(
            cache
                .state
                .lock()
                .unwrap()
                .scopes
                .values()
                .next()
                .unwrap()
                .groups,
            2
        );
        drop(first);
        drop(cloned);
        assert!(independent.is_current().unwrap());
        assert_eq!(cache.scope_policy(&scope), Some(IntegrityPolicy::Opaque));
        drop(independent);
        assert_eq!(cache.scope_policy(&scope), None);
        assert_eq!(cache.state.lock().unwrap().identities.len(), 1);
    }
    #[test]
    fn stale_drop_cannot_remove_a_fresh_same_backing_generation() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "drop", 2);
        let scope = proof_scope("d", 1);
        let stale = lease(&cache, scope.clone());
        cache.revoke_identity_admission(&scope.identity).unwrap();
        let fresh = lease(&cache, scope.clone());
        assert!(!stale.is_current().unwrap());
        assert!(fresh.is_current().unwrap());
        drop(stale);
        assert_eq!(cache.scope_policy(&scope), Some(IntegrityPolicy::Opaque));
        assert!(fresh.is_current().unwrap());
    }
    #[test]
    fn observed_backing_transition_invalidates_held_proofs_and_old_epochs() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "transition", 2);
        let old_scope = proof_scope("d", 1);
        let old = lease(&cache, old_scope.clone());
        let held_epoch = cache.identity_epoch(&old_scope.identity).unwrap();
        let next_scope = proof_scope("d", 2);
        let next = lease(&cache, next_scope.clone());
        assert!(!old.is_current().unwrap());
        assert!(next.is_current().unwrap());
        assert_eq!(cache.scope_policy(&old_scope), None);
        assert!(
            cache
                .register_scope_lease(old_scope, IntegrityPolicy::Opaque, held_epoch)
                .is_err()
        );
        drop(old);
        assert_eq!(
            cache.scope_policy(&next_scope),
            Some(IntegrityPolicy::Opaque)
        );
    }
    #[test]
    fn epoch_cannot_cross_cache_owner_or_exact_identity() {
        let dir = tempfile::tempdir().unwrap();
        let a = proof_cache(&dir, "a", 2);
        let b = proof_cache(&dir, "b", 2);
        let scope = proof_scope("d", 1);
        let token = a.identity_epoch(&scope.identity).unwrap();
        let _ = b.identity_epoch(&scope.identity).unwrap();
        assert!(
            b.register_scope_lease(scope.clone(), IntegrityPolicy::Opaque, token.clone())
                .is_err()
        );
        let mut wrong = scope.clone();
        wrong.identity.partition = "other".into();
        assert!(
            a.register_scope_lease(wrong, IntegrityPolicy::Opaque, token.clone())
                .is_err()
        );
        let good = a
            .register_scope_lease(scope, IntegrityPolicy::Opaque, token)
            .unwrap();
        assert!(good.is_current().unwrap());
    }
    #[test]
    fn scope_and_retained_identity_bounds_are_independent_of_blob_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "capacity", 3);
        let scopes: Vec<_> = (0..3).map(|i| proof_scope(&format!("d{i}"), 1)).collect();
        let leases: Vec<_> = scopes
            .iter()
            .map(|scope| lease(&cache, scope.clone()))
            .collect();
        assert_eq!(cache.state.lock().unwrap().scopes.len(), 3);
        for scope in &scopes {
            cache
                .insert_memory_shared(
                    scope,
                    &BlockId("opaque".into()),
                    Arc::from(b"bytes".as_slice()),
                    IntegrityPolicy::Opaque,
                )
                .unwrap();
        }
        assert_eq!(
            cache.usage().2,
            1,
            "blob entry capacity changed with scope capacity"
        );
        assert!(
            cache
                .identity_epoch(&proof_scope("one-too-many", 1).identity)
                .is_err()
        );
        drop(leases);
        assert!(cache.state.lock().unwrap().scopes.is_empty());
        assert_eq!(
            cache.state.lock().unwrap().identities.len(),
            3,
            "empty scopes forgot authority history"
        );
        assert!(
            cache
                .identity_epoch(&proof_scope("replacement", 1).identity)
                .is_err()
        );
        assert!(
            LocalCache::new_with_scope_capacity(
                LocalCacheConfig {
                    directory: dir.path().join("zero"),
                    memory_bytes: 1,
                    disk_bytes: 1,
                    max_entries: 1,
                    max_blob_bytes: 1
                },
                0
            )
            .is_err()
        );
        assert!(
            LocalCache::new_with_scope_capacity(
                LocalCacheConfig {
                    directory: dir.path().join("too-many"),
                    memory_bytes: 1,
                    disk_bytes: 1,
                    max_entries: 1,
                    max_blob_bytes: 1
                },
                1_000_001
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn administrative_revocation_works_with_eight_held_disk_permits_and_seals() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "saturated", 2);
        let scope = proof_scope("d", 1);
        let old = lease(&cache, scope.clone());
        let token = cache.identity_epoch(&scope.identity).unwrap();
        let mut permits = Vec::new();
        for _ in 0..8 {
            permits.push(cache.io_permit().await.unwrap());
        }
        cache.revoke_identity_admission(&scope.identity).unwrap();
        assert!(!old.is_current().unwrap());
        assert!(
            cache
                .register_scope_lease(scope.clone(), IntegrityPolicy::Opaque, token)
                .is_err()
        );
        let current = lease(&cache, scope.clone());
        assert!(current.is_current().unwrap());
        drop(permits);
        cache.shutdown().await.unwrap();
        assert!(current.is_current().is_err());
        assert!(cache.identity_epoch(&scope.identity).is_err());
        assert!(
            cache
                .current_scope_lease(&scope, IntegrityPolicy::Opaque)
                .is_err()
        );
        assert!(cache.revoke_identity_admission(&scope.identity).is_err());
        assert_eq!(cache.scope_policy(&scope), None);
    }
    #[test]
    fn authority_epoch_overflow_stays_failed_closed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "overflow", 2);
        let scope = proof_scope("d", 1);
        let retained = lease(&cache, scope.clone());
        cache
            .state
            .lock()
            .unwrap()
            .identities
            .get_mut(&scope.identity)
            .unwrap()
            .epoch = u64::MAX;
        assert!(cache.revoke_identity_admission(&scope.identity).is_err());
        assert!(retained.is_current().is_err());
        assert_eq!(cache.scope_policy(&scope), None);
        assert!(cache.identity_epoch(&scope.identity).is_err());
    }
    #[test]
    fn poisoned_authority_index_stays_failed_closed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "poison", 2);
        let scope = proof_scope("d", 1);
        let retained = lease(&cache, scope.clone());
        let held = cache.clone();
        let joined = std::thread::spawn(move || {
            let _index = held.state.lock().unwrap();
            panic!("controlled admission index poison");
        })
        .join();
        assert!(joined.is_err(), "actual poison thread must join");
        assert!(retained.is_current().is_err());
        assert_eq!(cache.scope_policy(&scope), None);
        assert!(cache.identity_epoch(&scope.identity).is_err());
    }

    #[test]
    fn twenty_thousand_scopes_fit_without_raising_the_single_blob_entry_budget() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "production-shape-capacity", 20_000);
        let leases: Vec<_> = (0..20_000)
            .map(|i| lease(&cache, proof_scope(&format!("sandbox-{i}"), 1)))
            .collect();
        assert_eq!(cache.state.lock().unwrap().scopes.len(), 20_000);
        assert_eq!(cache.state.lock().unwrap().identities.len(), 20_000);
        assert!(
            cache
                .identity_epoch(&proof_scope("sandbox-one-too-many", 1).identity)
                .is_err()
        );
        assert_eq!(cache.config.max_entries, 1);
        assert_eq!(cache.io_permits.available_permits(), 8);
        assert_eq!(cache.usage(), (0, 0, 0));
        drop(leases);
        assert!(cache.state.lock().unwrap().scopes.is_empty());
        assert_eq!(cache.state.lock().unwrap().identities.len(), 20_000);
    }

    #[test]
    fn adoption_refuses_unknown_scope_and_policy_mismatch_without_reserving_identity() {
        let dir = tempfile::tempdir().unwrap();
        let cache = proof_cache(&dir, "policy", 2);
        let scope = proof_scope("known", 1);
        let unknown = proof_scope("unknown", 1);
        assert!(
            cache
                .current_scope_lease(&unknown, IntegrityPolicy::Opaque)
                .unwrap()
                .is_none()
        );
        assert_eq!(cache.scope_policy(&unknown), None);
        assert!(
            cache.state.lock().unwrap().identities.is_empty(),
            "lookup reserved an incoming unknown identity"
        );
        let retained = lease(&cache, scope.clone());
        assert!(
            cache
                .current_scope_lease(&scope, IntegrityPolicy::Sha256Prefixed)
                .unwrap()
                .is_none()
        );
        let token = cache.identity_epoch(&scope.identity).unwrap();
        assert!(
            cache
                .register_scope_lease(scope.clone(), IntegrityPolicy::Sha256Prefixed, token)
                .is_err()
        );
        assert!(
            retained.is_current().unwrap(),
            "policy mismatch changed the already proven policy"
        );
        assert_eq!(cache.scope_policy(&scope), Some(IntegrityPolicy::Opaque));
    }
}

#[cfg(not(unix))]
fn open_relative(_root: &File, _name: &str, _write: bool, _create: bool) -> Result<File> {
    Err(FsError::new(ErrorCode::Enotsup))
}
#[cfg(not(unix))]
fn unlink_relative(_root: &File, _name: &str) -> Result<()> {
    Err(FsError::new(ErrorCode::Enotsup))
}
#[cfg(not(unix))]
fn write_checked(_root: &File, _key: &str, _bytes: &[u8]) -> Result<()> {
    Err(FsError::new(ErrorCode::Enotsup))
}
#[cfg(all(test, not(unix)))]
#[test]
fn unsupported_cache_platform_fails_before_creating_directory() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("uncreated-cache");
    let result = LocalCache::new(LocalCacheConfig {
        directory: directory.clone(),
        memory_bytes: 4096,
        disk_bytes: 4096,
        max_entries: 4,
        max_blob_bytes: 4096,
    });
    assert!(result.err().unwrap().is(ErrorCode::Enotsup));
    assert!(!directory.exists());
}
