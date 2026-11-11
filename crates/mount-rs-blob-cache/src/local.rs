use crate::*;
use arc_swap::ArcSwap;
use mount_rs_core::park_cell::ParkCell;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
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
}
/// A registered scope: its policy and how many registrations hold it.
#[derive(Clone)]
struct ScopeEntry {
    policy: IntegrityPolicy,
    registrations: usize,
}
type Scopes = BTreeMap<CacheKey, ScopeEntry>;
pub struct PendingReservation {
    used: Arc<AtomicUsize>,
    bytes: usize,
}
impl Drop for PendingReservation {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
/// One bounded cache shared across drives. Disk operations serialize independently
/// of the RAM index; no filesystem operation holds the index mutex.
pub struct LocalCache {
    config: LocalCacheConfig,
    state: ParkCell<State>,
    /// Read for every inbound peer stream; replaced whole, by
    /// compare-and-swap, when a drive registers or unregisters.
    scopes: ArcSwap<Scopes>,
    /// Held across disk I/O by threads that may block, which park for it.
    disk_io: mount_rs_core::turn_gate::TurnLock<()>,
    root: File,
    _lock: File,
    pending: Arc<AtomicUsize>,
    pub(crate) io_permits: Arc<tokio::sync::Semaphore>,
}
impl LocalCache {
    /// Secure descriptor-relative disk operations currently require Unix.
    #[cfg(not(unix))]
    pub fn new(_config: LocalCacheConfig) -> Result<Arc<Self>> {
        Err(FsError::new(ErrorCode::Enotsup).with_syscall("blob cache"))
    }
    #[cfg(unix)]
    pub fn new(config: LocalCacheConfig) -> Result<Arc<Self>> {
        if config.max_entries == 0
            || config.max_entries > 1_000_000
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
            state: ParkCell::new(state),
            scopes: ArcSwap::from_pointee(Scopes::new()),
            disk_io: mount_rs_core::turn_gate::TurnLock::new(()),
            root,
            _lock: lock,
            pending: Arc::new(AtomicUsize::new(0)),
            io_permits: Arc::new(tokio::sync::Semaphore::new(8)),
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
    pub async fn io_permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        self.io_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| error())
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
    pub fn register_scope(&self, scope: CacheScope, policy: IntegrityPolicy) -> Result<()> {
        if [
            &scope.identity.cluster,
            &scope.identity.partition,
            &scope.identity.drive,
        ]
        .iter()
        .any(|s| s.is_empty() || s.len() > 1024)
        {
            return Err(error());
        }
        let key = Self::scope_hash(&scope);
        loop {
            let current = self.scopes.load();
            let mut next = Scopes::clone(&current);
            match next.get_mut(&key) {
                Some(entry) => {
                    if entry.policy != policy {
                        return Err(error());
                    }
                    entry.registrations = entry.registrations.checked_add(1).ok_or_else(error)?;
                }
                None => {
                    if next.len() >= self.config.max_entries {
                        return Err(error());
                    }
                    next.insert(
                        key,
                        ScopeEntry {
                            policy,
                            registrations: 1,
                        },
                    );
                }
            }
            let previous = self.scopes.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return Ok(());
            }
        }
    }
    pub fn unregister_scope(&self, scope: &CacheScope) {
        let key = Self::scope_hash(scope);
        loop {
            let current = self.scopes.load();
            let Some(entry) = current.get(&key) else {
                return;
            };
            let mut next = Scopes::clone(&current);
            if entry.registrations <= 1 {
                next.remove(&key);
            } else if let Some(entry) = next.get_mut(&key) {
                entry.registrations -= 1;
            }
            let previous = self.scopes.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return;
            }
        }
    }
    pub fn scope_policy(&self, scope: &CacheScope) -> Option<IntegrityPolicy> {
        self.scopes
            .load()
            .get(&Self::scope_hash(scope))
            .map(|entry| entry.policy)
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
    ) -> Option<Vec<u8>> {
        self.get_memory(scope, id, policy)
            .or_else(|| self.get_disk(scope, id, policy))
    }
    pub fn get_disk(
        &self,
        scope: &CacheScope,
        id: &BlockId,
        policy: IntegrityPolicy,
    ) -> Option<Vec<u8>> {
        let key = Self::key(scope, id);
        // Reads take no disk lock, so reads of different blocks (and of the
        // same block) overlap. A file is published by rename and checked
        // against its recorded size and digest, and the index generation is
        // compared again below, so a read that raced a replacement or an
        // eviction is discarded rather than believed.
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
            // The file is bad. Removing it is a disk change like any other,
            // so it waits its turn with the writers, and looks again: a
            // writer may have replaced the entry in the meantime.
            drop(state);
            let _disk = self.disk_io.lock_blocking();
            let mut state = self.state.lock().ok()?;
            if state
                .entries
                .get(&key)
                .is_none_or(|entry| entry.generation != generation)
            {
                return None;
            }
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
        if bytes.len() > self.config.max_blob_bytes {
            return Err(error());
        }
        self.insert_shared(scope, id, Arc::from(bytes), policy)
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
        if bytes.len() > self.config.max_blob_bytes {
            return Err(error());
        }
        policy.verify(id, &bytes)?;
        let key = Self::key(scope, id);
        let disk_bytes = bytes.len().checked_add(32).ok_or_else(error)?;
        let memory = bytes.len() <= self.config.memory_bytes;
        let _disk = self.disk_io.lock_blocking();
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
    pub fn invalidate(&self, scope: &CacheScope, id: &BlockId) {
        let key = Self::key(scope, id);
        {
            let _disk = self.disk_io.lock_blocking();
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
    pub fn invalidate_scope(&self, scope: &CacheScope) {
        {
            let _disk = self.disk_io.lock_blocking();
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
    pub async fn shutdown(&self) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.io_permits.clone().acquire_many_owned(8),
        )
        .await;
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
    #[test]
    fn concurrent_scope_registrations_are_all_counted() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(cfg(dir.path())).unwrap();
        let shared = scope("shared");
        // While a thread holds a registration the scope must stay visible: a
        // lost count would let another thread's unregister remove it.
        std::thread::scope(|threads| {
            for _ in 0..8 {
                threads.spawn(|| {
                    for _ in 0..2_000 {
                        cache
                            .register_scope(shared.clone(), IntegrityPolicy::Opaque)
                            .unwrap();
                        assert_eq!(cache.scope_policy(&shared), Some(IntegrityPolicy::Opaque));
                        assert!(
                            cache
                                .register_scope(shared.clone(), IntegrityPolicy::Sha256Prefixed)
                                .is_err()
                        );
                        cache.unregister_scope(&shared);
                    }
                });
            }
        });
        assert_eq!(cache.scope_policy(&shared), None);
        assert!(cache.scopes.load().is_empty());
        // Registrations taken together are each counted, and it takes that
        // many releases, however they interleave, to remove the scope.
        std::thread::scope(|threads| {
            for _ in 0..8 {
                threads.spawn(|| {
                    for _ in 0..500 {
                        cache
                            .register_scope(shared.clone(), IntegrityPolicy::Opaque)
                            .unwrap();
                    }
                });
            }
        });
        assert_eq!(
            cache.scopes.load()[&LocalCache::scope_hash(&shared)].registrations,
            4_000
        );
        std::thread::scope(|threads| {
            for thread in 0..8 {
                let (cache, shared) = (&cache, &shared);
                threads.spawn(move || {
                    for n in 0..500 {
                        if (thread, n) != (0, 0) {
                            cache.unregister_scope(shared);
                        }
                    }
                });
            }
        });
        assert_eq!(cache.scope_policy(&shared), Some(IntegrityPolicy::Opaque));
        cache.unregister_scope(&shared);
        assert_eq!(cache.scope_policy(&shared), None);
        cache.unregister_scope(&shared);
        // The bound on distinct scopes holds when they arrive together.
        let admitted = AtomicUsize::new(0);
        std::thread::scope(|threads| {
            for n in 0..8 {
                let (cache, admitted) = (&cache, &admitted);
                threads.spawn(move || {
                    if cache
                        .register_scope(scope(&format!("drive-{n}")), IntegrityPolicy::Opaque)
                        .is_ok()
                    {
                        admitted.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });
        assert_eq!(admitted.load(Ordering::Relaxed), 3);
        assert_eq!(cache.scopes.load().len(), 3);
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
            c.get_disk(&s, &id, IntegrityPolicy::Opaque)
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
    /// Not a test: disk reads of blocks too large for RAM, from 8 threads.
    /// `cargo test -p mount-rs-blob-cache --release profile_disk_reads -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn profile_disk_reads() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(LocalCacheConfig {
            directory: dir.path().to_path_buf(),
            memory_bytes: 4,
            disk_bytes: 64 * 1024 * 1024,
            max_entries: 4096,
            max_blob_bytes: 8192,
        })
        .unwrap();
        let s = scope("reads");
        let ids: Vec<BlockId> = (0..256_u32)
            .map(|n| BlockId(format!("block-{n}")))
            .collect();
        for id in &ids {
            cache
                .insert(&s, id, &[7_u8; 4096], IntegrityPolicy::Opaque)
                .unwrap();
        }
        let started = std::time::Instant::now();
        let mut waits: Vec<u64> = std::thread::scope(|threads| {
            let handles: Vec<_> = (0..8_usize)
                .map(|thread| {
                    let (cache, s, ids) = (&cache, &s, &ids);
                    threads.spawn(move || {
                        let mut waits = Vec::with_capacity(20_000);
                        for n in 0..20_000_usize {
                            let id = &ids[(n * 31 + thread * 17) % ids.len()];
                            let began = std::time::Instant::now();
                            assert!(cache.get_disk(s, id, IntegrityPolicy::Opaque).is_some());
                            waits.push(began.elapsed().as_nanos() as u64);
                        }
                        waits
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap())
                .collect()
        });
        let seconds = started.elapsed().as_secs_f64();
        waits.sort_unstable();
        let at = |q: f64| waits[((waits.len() - 1) as f64 * q) as usize] as f64 / 1e6;
        println!(
            "disk reads: {:.0}/s p50={:.4} p99={:.4} p99.9={:.4} ms",
            waits.len() as f64 / seconds,
            at(0.5),
            at(0.99),
            at(0.999)
        );
    }
    /// Not a test: disk-backed inserts of blocks too large for RAM, from one
    /// thread and then from 8. Every insert after a thread's first pass over
    /// its ids replaces a file (unlink, write, rename). This is the measure
    /// that keeps `disk_io` a lock: with one disk-writer thread in its place
    /// the single inserter's median was about a fifth slower in every run.
    /// `cargo test -p mount-rs-blob-cache --release profile_disk_inserts -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn profile_disk_inserts() {
        for threads in [1_usize, 8] {
            let dir = tempfile::tempdir().unwrap();
            let cache = LocalCache::new(LocalCacheConfig {
                directory: dir.path().to_path_buf(),
                memory_bytes: 4,
                disk_bytes: 256 * 1024 * 1024,
                max_entries: 8192,
                max_blob_bytes: 8192,
            })
            .unwrap();
            let s = scope("inserts");
            let per_thread = 24_000 / threads;
            let block: Arc<[u8]> = Arc::from(vec![7_u8; 4096]);
            let started = std::time::Instant::now();
            let mut waits: Vec<u64> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..threads)
                    .map(|thread| {
                        let (cache, s, block) = (&cache, &s, &block);
                        scope.spawn(move || {
                            let ids: Vec<BlockId> = (0..256_u32)
                                .map(|n| BlockId(format!("block-{thread}-{n}")))
                                .collect();
                            let mut waits = Vec::with_capacity(per_thread);
                            for n in 0..per_thread {
                                let id = &ids[n % ids.len()];
                                let began = std::time::Instant::now();
                                cache
                                    .insert_shared(s, id, block.clone(), IntegrityPolicy::Opaque)
                                    .unwrap();
                                waits.push(began.elapsed().as_nanos() as u64);
                            }
                            waits
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|handle| handle.join().unwrap())
                    .collect()
            });
            let seconds = started.elapsed().as_secs_f64();
            waits.sort_unstable();
            let at = |q: f64| waits[((waits.len() - 1) as f64 * q) as usize] as f64 / 1e6;
            println!(
                "disk inserts x{threads}: {:.0}/s p50={:.4} p99={:.4} p99.9={:.4} ms",
                waits.len() as f64 / seconds,
                at(0.5),
                at(0.99),
                at(0.999)
            );
        }
    }
    #[test]
    fn racing_inserts_and_invalidations_leave_no_file_the_index_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let config = LocalCacheConfig {
            directory: dir.path().to_path_buf(),
            memory_bytes: 64,
            // Room for 12 of the 16 blocks, so inserts also evict.
            disk_bytes: 12 * (48 + 32),
            max_entries: 14,
            max_blob_bytes: 64,
        };
        let cache = LocalCache::new(config.clone()).unwrap();
        let scopes = [scope("left"), scope("right")];
        let ids: Vec<BlockId> = (0..8).map(|n| BlockId(format!("block-{n}"))).collect();
        let check = |cache: &LocalCache| {
            let state = cache.state.lock().unwrap();
            assert!(state.quarantined.is_empty());
            let mut files = BTreeMap::new();
            for entry in fs::read_dir(dir.path()).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                assert!(!name.ends_with(".tmp"), "temporary file {name} left behind");
                if let Some(key) = parse_key(&name) {
                    files.insert(key, entry.metadata().unwrap().len() as usize);
                }
            }
            let indexed: BTreeMap<CacheKey, usize> = state
                .entries
                .iter()
                .filter(|(_, entry)| entry.disk)
                .map(|(key, entry)| (*key, entry.size + 32))
                .collect();
            assert_eq!(files, indexed, "the directory and the index disagree");
            assert_eq!(state.disk, indexed.values().sum::<usize>());
            assert!(state.disk <= config.disk_bytes);
            assert!(state.entries.len() <= config.max_entries);
        };
        for round in 0..4 {
            std::thread::scope(|threads| {
                for thread in 0..8_usize {
                    let (cache, scopes, ids) = (&cache, &scopes, &ids);
                    threads.spawn(move || {
                        for n in 0..400_usize {
                            let turn = n * 7 + thread * 13 + round;
                            let s = &scopes[turn % 2];
                            let id = &ids[(turn / 2) % ids.len()];
                            // The same bytes for a block every time: a
                            // different RAM copy would refuse the insert.
                            let block = [id.0.as_bytes()[6]; 48];
                            match (n + thread) % 8 {
                                0..=3 => cache
                                    .insert(s, id, &block, IntegrityPolicy::Opaque)
                                    .unwrap(),
                                4 | 5 => cache.invalidate(s, id),
                                6 if n % 64 == 6 => cache.invalidate_scope(s),
                                _ => {
                                    if let Some(read) = cache.get(s, id, IntegrityPolicy::Opaque) {
                                        assert_eq!(read, block);
                                    }
                                }
                            }
                        }
                    });
                }
            });
            // Nothing is in flight: every change was made before its caller
            // returned.
            check(&cache);
        }
        // A restart reads the directory and finds what the index held.
        let before = cache.usage();
        drop(cache);
        let cache = LocalCache::new(config.clone()).unwrap();
        assert_eq!((cache.usage().1, cache.usage().2), (before.1, before.2));
        check(&cache);
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
        let _disk = cache.disk_io.lock_blocking();
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
        assert!(cache.get_disk(&a, &id, IntegrityPolicy::Opaque).is_none());
        assert!(cache.get_disk(&b, &id, IntegrityPolicy::Opaque).is_none());
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
            ),
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
            c.get(&s, &id, IntegrityPolicy::Opaque),
            Some(b"data".to_vec())
        );
        fs::write(path, b"XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXoops").unwrap();
        assert_eq!(c.get(&s, &id, IntegrityPolicy::Opaque), None);
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
        assert!(c.get(&s, &id, IntegrityPolicy::Opaque).is_none());
        assert_eq!(fs::read(outside).unwrap(), b"secret");
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
