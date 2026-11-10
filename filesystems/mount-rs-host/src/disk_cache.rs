//! Best-effort persistent cache for one immutable backing-store generation.
use async_trait::async_trait;
use mount_rs_core::drain_gate::DrainGate;
use mount_rs_core::storage::{BlockId, BlockStore};
use mount_rs_core::{ErrorCode, FsError, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const MAGIC: &[u8; 8] = b"MNTCACH1";
const HEADER: usize = 80;
const MAX_PAYLOAD: usize = super::LOCAL_MAX_BLOCK_BYTES;

#[derive(Clone, Copy, Debug, Default)]
pub struct DiskCacheStats {
    /// Completed disk critical sections, including stats/clear/delete calls.
    /// A stats snapshot excludes the critical section taking that snapshot.
    pub lock_sections: u64,
    /// Mutex waiting only, excluding worker admission and blocking dispatch.
    pub lock_wait_nanoseconds: u64,
    pub lock_wait_max_nanoseconds: u64,
    /// Operation time under the mutex, excluding this timing bookkeeping.
    pub lock_hold_nanoseconds: u64,
    pub lock_hold_max_nanoseconds: u64,
    /// Completed cache probes, including misses (not authoritative backing reads).
    pub read_probes: u64,
    /// Time awaiting a worker permit and blocking dispatch.
    /// Excludes the invalidation guard and authoritative backing I/O.
    pub read_queue_nanoseconds: u64,
    /// Time inside the cache probe, including file I/O, checksums and LRU updates.
    pub read_service_nanoseconds: u64,
    /// Completed fill attempts, including rejected or disabled-cache attempts.
    pub fill_attempts: u64,
    /// Admission, payload copy, worker dispatch and disk-mutex waiting.
    pub fill_wait_and_copy_nanoseconds: u64,
    /// Cache insertion, including checksum, eviction and file operations.
    pub fill_service_nanoseconds: u64,
    /// Time awaiting successful backing gets on disk misses that reached fill admission.
    pub backing_get_nanoseconds: u64,
    /// Read-miss fill admission through insertion, excluding fills after writes.
    pub read_fill_nanoseconds: u64,
    pub hits: u64,
    pub misses: u64,
    pub corruptions: u64,
    pub io_failures: u64,
    pub admission_rejections: u64,
    pub evictions: u64,
    pub resident_bytes: u64,
    pub resident_entries: usize,
    /// An I/O error disables cache access until the cache is reopened.
    pub disabled: bool,
}

/// Shared admission limits for disk caches belonging to one server.
/// Clone the same budget into every volume to bound workers and temporary fill
/// copies across those volumes. Resident cache bytes remain bounded per volume.
#[derive(Clone)]
pub struct DiskCacheBudget {
    workers: Arc<tokio::sync::Semaphore>,
    fills: Arc<tokio::sync::Semaphore>,
    background_read_fills: bool,
}
impl DiskCacheBudget {
    /// Allow a verified backing read to return after bounded fill admission.
    /// Clear and delete still wait for admitted fills before invalidating entries.
    pub fn with_background_read_fills(mut self, enabled: bool) -> Self {
        self.background_read_fills = enabled;
        self
    }
    pub fn new(workers: usize, fill_copies: usize) -> Result<Self> {
        if workers == 0
            || fill_copies == 0
            || workers > tokio::sync::Semaphore::MAX_PERMITS
            || fill_copies > tokio::sync::Semaphore::MAX_PERMITS
        {
            return Err(FsError::new(ErrorCode::Einval).with_message(
                "disk cache worker and fill limits must be positive valid semaphore sizes",
            ));
        }
        Ok(Self {
            workers: Arc::new(tokio::sync::Semaphore::new(workers)),
            fills: Arc::new(tokio::sync::Semaphore::new(fill_copies)),
            background_read_fills: false,
        })
    }
}
impl Default for DiskCacheBudget {
    fn default() -> Self {
        Self::new(4, 4).expect("constant valid disk cache budget")
    }
}

// Readers retain a generation-specific recency cell across file I/O. Only
// maintenance updates index membership, under the disk writer lock.
#[derive(Default)]
struct ReadIndex {
    entries: arc_swap::ArcSwap<im::OrdMap<String, Arc<AtomicU64>>>,
    sequence: AtomicU64,
    disabled: AtomicBool,
    hits: AtomicU64,
    misses: AtomicU64,
    queue_ns: AtomicU64,
    service_ns: AtomicU64,
}
impl ReadIndex {
    fn tick(&self) -> u64 {
        self.sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .expect("disk cache sequence exhausted")
            + 1
    }
    fn update(&self, key: &str, sequence: Option<u64>) {
        let mut entries = (**self.entries.load()).clone();
        if let Some(sequence) = sequence {
            entries.insert(key.to_owned(), Arc::new(AtomicU64::new(sequence)));
        } else {
            entries.remove(key);
        }
        self.entries.store(Arc::new(entries));
    }
}
struct Disk {
    root: PathBuf,
    _lock: File,
    max_bytes: u64,
    max_entries: usize,
    // Sequence ordering is process-local; reopening starts a fresh LRU order.
    entries: BTreeMap<String, (u64, u64)>,
    recency: BTreeMap<u64, String>,
    /// Space set aside for fills whose files are being written outside the
    /// mutex. It counts against both bounds until the fill commits or fails.
    reserved_bytes: u64,
    reserved_entries: usize,
    index: Arc<ReadIndex>,
    stats: DiskCacheStats,
}

fn error(message: impl ToString) -> FsError {
    FsError::new(ErrorCode::Eio).with_message(message.to_string())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn regular_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
}
fn with_disk<T>(disk: &Mutex<Disk>, operation: impl FnOnce(&mut Disk) -> T) -> Result<T> {
    let waiting = std::time::Instant::now();
    let mut disk = disk
        .lock()
        .map_err(|_| error("disk cache mutex poisoned"))?;
    let acquired = std::time::Instant::now();
    let result = operation(&mut disk);
    let nanos =
        |duration: std::time::Duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
    let wait = nanos(acquired.duration_since(waiting));
    let hold = nanos(acquired.elapsed());
    let stats = &mut disk.stats;
    stats.lock_sections = stats.lock_sections.saturating_add(1);
    stats.lock_wait_nanoseconds = stats.lock_wait_nanoseconds.saturating_add(wait);
    stats.lock_wait_max_nanoseconds = stats.lock_wait_max_nanoseconds.max(wait);
    stats.lock_hold_nanoseconds = stats.lock_hold_nanoseconds.saturating_add(hold);
    stats.lock_hold_max_nanoseconds = stats.lock_hold_max_nanoseconds.max(hold);
    Ok(result)
}
// Own the opened file while reading, independent of index eviction/unlink.
fn read_file(mut file: File, key: &str) -> std::io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() < HEADER as u64
        || metadata.len() > (HEADER + MAX_PAYLOAD) as u64
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid cache entry size/type",
        ));
    }
    let mut encoded = Vec::new();
    (&mut file)
        .take((HEADER + MAX_PAYLOAD + 1) as u64)
        .read_to_end(&mut encoded)?;
    if encoded.len() < HEADER
        || encoded.len() > HEADER + MAX_PAYLOAD
        || &encoded[..8] != MAGIC
        || encoded[8..40] != Sha256::digest(key.as_bytes())[..]
    {
        // The header stores the ASCII key's SHA-256, not an opaque provider path.
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid cache entry header",
        ));
    }
    let length = u64::from_le_bytes(encoded[40..48].try_into().unwrap());
    if length != (encoded.len() - HEADER) as u64
        || encoded[48..80] != Sha256::digest(&encoded[HEADER..])[..]
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid cache payload checksum",
        ));
    }
    Ok(encoded.split_off(HEADER))
}
impl Disk {
    fn open(root: &Path, scope: &str, max_bytes: u64, max_entries: usize) -> Result<Self> {
        if !cfg!(unix) {
            return Err(FsError::enotsup("disk cache directory synchronization"));
        }
        if scope.is_empty() || max_bytes < HEADER as u64 || max_entries == 0 {
            return Err(
                FsError::new(ErrorCode::Einval).with_message("invalid disk cache scope or bounds")
            );
        }
        let root = fs::canonicalize(root)
            .map_err(error)?
            .join(digest(scope.as_bytes()));
        match fs::create_dir(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(error(e)),
        }
        if !fs::symlink_metadata(&root)
            .map_err(error)?
            .file_type()
            .is_dir()
        {
            return Err(error("disk cache scope is not a directory"));
        }
        let lock = regular_options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("lock"))
            .map_err(error)?;
        if !lock.metadata().map_err(error)?.is_file() {
            return Err(error("invalid cache lock"));
        }
        lock.try_lock()
            .map_err(|_| error("disk cache directory is already owned or cannot be locked"))?;
        let mut disk = Self {
            root,
            _lock: lock,
            max_bytes,
            max_entries,
            entries: BTreeMap::new(),
            recency: BTreeMap::new(),
            reserved_bytes: 0,
            reserved_entries: 0,
            index: Arc::default(),
            stats: DiskCacheStats::default(),
        };
        for item in fs::read_dir(&disk.root).map_err(error)? {
            let item = item.map_err(error)?;
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| error("unexpected cache filename"))?;
            if name == "lock" {
                continue;
            }
            if name.starts_with(".cache-tmp-") {
                fs::remove_file(item.path()).map_err(error)?;
                continue;
            }
            if name.len() != 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(error("unexpected file in private cache directory"));
            }
            let metadata = fs::symlink_metadata(item.path()).map_err(error)?;
            let size = metadata.len();
            if !metadata.is_file()
                || size < HEADER as u64
                || size > (HEADER + MAX_PAYLOAD) as u64
                || size > max_bytes
            {
                fs::remove_file(item.path()).map_err(error)?;
                continue;
            }
            if disk.make_room(size).map_err(error)? {
                disk.record(name, size);
            }
        }
        Ok(disk)
    }
    fn record(&mut self, key: String, size: u64) {
        let generation = self.index.tick();
        self.index.update(&key, Some(generation));
        self.entries.insert(key.clone(), (size, generation));
        self.recency.insert(generation, key);
        self.stats.resident_bytes += size;
        self.stats.resident_entries = self.entries.len();
    }
    fn forget(&mut self, key: &str) {
        if let Some((size, sequence)) = self.entries.remove(key) {
            self.recency.remove(&sequence);
            self.index.update(key, None);
            self.stats.resident_bytes -= size;
        }
        self.stats.resident_entries = self.entries.len();
    }
    fn remove(&mut self, key: &str) -> std::io::Result<()> {
        match fs::remove_file(self.root.join(key)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.forget(key);
        Ok(())
    }
    /// Evicts until an entry of `size` bytes fits. `false` means it cannot:
    /// what is left of the budget is reserved by fills still being written.
    fn make_room(&mut self, size: u64) -> std::io::Result<bool> {
        let cutoff = self.index.sequence.load(Ordering::Relaxed);
        while self.entries.len() + self.reserved_entries >= self.max_entries
            || self.stats.resident_bytes + self.reserved_bytes > self.max_bytes - size
        {
            let Some((_, key)) = self.recency.first_key_value() else {
                return Ok(false);
            };
            let key = key.clone();
            // Hits publish recency without a writer. Repair stale recency order
            // lazily when this entry reaches the eviction frontier.
            let entry = self.entries.get_mut(&key).expect("indexed entry");
            let touched = self.index.entries.load()[&key].load(Ordering::Relaxed);
            if touched > entry.1 && entry.1 < cutoff {
                self.recency.remove(&entry.1);
                entry.1 = touched;
                self.recency.insert(touched, key);
                continue;
            }
            self.remove(&key)?;
            self.stats.evictions += 1;
        }
        Ok(true)
    }
    fn disable(&mut self) {
        self.stats.io_failures += 1;
        self.index.disabled.store(true, Ordering::Release);
        self.stats.disabled = true;
    }
    fn read_failed(&mut self, key: &str, entry: &Arc<AtomicU64>, error: std::io::Error) {
        // The worker can outlive eviction or replacement of the opened entry.
        if !self
            .index
            .entries
            .load()
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            return;
        }
        match error.kind() {
            std::io::ErrorKind::NotFound => self.forget(key),
            std::io::ErrorKind::InvalidData => {
                self.stats.corruptions += 1;
                if self.remove(key).is_err() {
                    self.disable();
                }
            }
            _ => self.disable(),
        }
    }
    /// First half of a fill: admit an entry of `size` bytes and set its space
    /// aside, so the file can be written without the mutex and still never
    /// take the cache over its bounds.
    fn reserve(&mut self, key: &str, size: u64) -> bool {
        if self.stats.disabled || size > (HEADER + MAX_PAYLOAD) as u64 || size > self.max_bytes {
            self.stats.admission_rejections += 1;
            return false;
        }
        // Every file in the directory is indexed (see `open`), so only an
        // indexed key has a file to remove; an unlink that finds nothing is
        // still a directory operation, and this is the serialized section.
        let replaced = if self.entries.contains_key(key) {
            self.remove(key)
        } else {
            Ok(())
        };
        match replaced.and_then(|()| self.make_room(size)) {
            Ok(true) => {
                self.reserved_bytes += size;
                self.reserved_entries += 1;
                true
            }
            Ok(false) => {
                self.stats.admission_rejections += 1;
                false
            }
            Err(_) => {
                self.disable();
                false
            }
        }
    }
    /// Both halves of a fill in one step.
    #[cfg(test)]
    fn insert(&mut self, key: String, bytes: &[u8]) {
        let size = (HEADER + bytes.len()) as u64;
        if self.reserve(&key, size) {
            let written = write_entry(&self.root.clone(), &key, bytes);
            self.commit(key, size, written);
        }
    }
    /// Second half of a fill: index the file `write_entry` put in place, or
    /// give the reserved space back.
    fn commit(&mut self, key: String, size: u64, written: std::io::Result<()>) {
        self.reserved_bytes -= size;
        self.reserved_entries -= 1;
        if self.stats.disabled {
            return;
        }
        match written {
            Ok(()) => {
                // Another fill of the same block may have committed
                // meanwhile; its file is the one just replaced.
                self.forget(&key);
                self.record(key, size);
            }
            Err(_) => self.disable(),
        }
    }
}
/// Writes one cache entry and renames it into place under `key`. Needs no
/// lock. The temporary name is unique, the rename is atomic, and nothing
/// looks for the file before `Disk::commit` indexes it. Blocks are named by
/// their content, so two fills of one key rename identical files over each
/// other. An eviction of the same key between this rename and the commit
/// removes the file first; the index then names a missing file, which the
/// first read reports and `read_failed` drops as an ordinary miss.
fn write_entry(root: &Path, key: &str, bytes: &[u8]) -> std::io::Result<()> {
    let mut temp = tempfile::Builder::new()
        .prefix(".cache-tmp-")
        .tempfile_in(root)?;
    let mut header = [0; HEADER];
    header[..8].copy_from_slice(MAGIC);
    header[8..40].copy_from_slice(&Sha256::digest(key.as_bytes()));
    header[40..48].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
    header[48..].copy_from_slice(&Sha256::digest(bytes));
    temp.write_all(&header)?;
    temp.write_all(bytes)?;
    temp.persist(root.join(key)).map_err(|e| e.error)?;
    Ok(())
}
/// Caches `bytes` under `key`, holding the mutex only to reserve space and to
/// publish the written file; hashing and writing happen between the two.
/// `finish` runs in the last critical section this takes.
fn fill(
    disk: &Mutex<Disk>,
    root: &Path,
    key: String,
    bytes: Option<&[u8]>,
    finish: impl FnOnce(&mut Disk),
) -> Result<()> {
    let mut finish = Some(finish);
    let size = bytes.map(|bytes| (HEADER + bytes.len()) as u64);
    let reserved = with_disk(disk, |disk| {
        let reserved = match size {
            Some(size) => disk.reserve(&key, size),
            None => {
                disk.stats.admission_rejections += 1;
                false
            }
        };
        if !reserved && let Some(finish) = finish.take() {
            finish(disk);
        }
        reserved
    })?;
    let (Some(bytes), Some(size), Some(finish), true) = (bytes, size, finish, reserved) else {
        return Ok(());
    };
    let written = write_entry(root, &key, bytes);
    with_disk(disk, |disk| {
        disk.commit(key, size, written);
        finish(disk);
    })
}

/// Persistent owner-local cache. `root` is pre-existing and application-owned;
/// `scope` must uniquely identify the immutable backing-store generation/volume.
/// Only one instance may open a scope; clones share ownership. Backing durability
/// and errors are preserved. Cache writes are atomic but intentionally not fsynced.
/// Corruption is treated as a miss; cache I/O failures disable it until reopen.
/// All authoritative reclamation must pass through this wrapper's `delete`.
/// Delete first syncs cache invalidation; failure prevents authoritative deletion.
/// Unix only currently; the directory is not a sandbox against hostile renames.
/// Four workers and four admission copies are allowed per cache. These are not
/// process-global limits; providers and concurrent requests own separate buffers.
pub struct DiskCachedBlockStore<B: ?Sized> {
    backing: Arc<B>,
    disk: Arc<Mutex<Disk>>,
    index: Arc<ReadIndex>,
    root: Arc<PathBuf>,
    io: Arc<DrainGate>,
    /// Puts share this gate and flush takes it exclusively, so puts to the
    /// backing store overlap while a flush still covers every put
    /// acknowledged before it.
    writes: Arc<DrainGate>,
    workers: Arc<tokio::sync::Semaphore>,
    fills: Arc<tokio::sync::Semaphore>,
    background_read_fills: bool,
    max_payload: usize,
}
impl<B: ?Sized> Clone for DiskCachedBlockStore<B> {
    fn clone(&self) -> Self {
        Self {
            backing: self.backing.clone(),
            disk: self.disk.clone(),
            root: self.root.clone(),
            index: self.index.clone(),
            io: self.io.clone(),
            writes: self.writes.clone(),
            workers: self.workers.clone(),
            fills: self.fills.clone(),
            background_read_fills: self.background_read_fills,
            max_payload: self.max_payload,
        }
    }
}
impl<B: BlockStore + ?Sized + 'static> DiskCachedBlockStore<B> {
    pub async fn open(
        backing: Arc<B>,
        root: impl AsRef<Path>,
        scope: &str,
        max_bytes: u64,
        max_entries: usize,
    ) -> Result<Self> {
        Self::open_with_budget(
            backing,
            root,
            scope,
            max_bytes,
            max_entries,
            DiskCacheBudget::default(),
        )
        .await
    }
    /// Open with worker/fill admission shared across a server's volumes.
    /// Each admitted fill copy is at most LOCAL_MAX_BLOCK_BYTES (16 MiB).
    pub async fn open_with_budget(
        backing: Arc<B>,
        root: impl AsRef<Path>,
        scope: &str,
        max_bytes: u64,
        max_entries: usize,
        budget: DiskCacheBudget,
    ) -> Result<Self> {
        let root = root.as_ref().to_owned();
        let scope = scope.to_owned();
        let permit = budget
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(error)?;
        let disk = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            Disk::open(&root, &scope, max_bytes, max_entries)
        })
        .await
        .map_err(error)??;
        Ok(Self {
            backing,
            root: Arc::new(disk.root.clone()),
            index: disk.index.clone(),
            disk: Arc::new(Mutex::new(disk)),
            io: Arc::new(DrainGate::new()),
            writes: Arc::new(DrainGate::new()),
            workers: budget.workers,
            fills: budget.fills,
            background_read_fills: budget.background_read_fills,
            max_payload: usize::try_from(max_bytes.saturating_sub(HEADER as u64))
                .unwrap_or(usize::MAX)
                .min(MAX_PAYLOAD),
        })
    }
    // Move exclusion into the blocking worker and return it with the result.
    // Cancellation cannot release it while that worker still touches cache files.
    async fn disk<G: Send + 'static, T: Send + 'static>(
        &self,
        guard: G,
        operation: impl FnOnce(&mut Disk) -> T + Send + 'static,
    ) -> Result<(G, T)> {
        let permit = self.workers.clone().acquire_owned().await.map_err(error)?;
        let disk = self.disk.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let result = with_disk(&disk, operation)?;
            Ok((guard, result))
        })
        .await
        .map_err(error)?
    }
    async fn read_cached<G: Send + 'static>(
        &self,
        guard: G,
        key: String,
        read: impl FnOnce(File, &str) -> std::io::Result<Vec<u8>> + Send + 'static,
    ) -> Result<(G, Option<Vec<u8>>)> {
        let queued = std::time::Instant::now();
        // A concurrent fill can turn this into a redundant authoritative read.
        // The caller retains the invalidation guard through the read and refill.
        if !self.index.entries.load().contains_key(&key) {
            self.index.misses.fetch_add(1, Ordering::Relaxed);
            self.index.service_ns.fetch_add(
                u64::try_from(queued.elapsed().as_nanos()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            return Ok((guard, None));
        }
        let permit = self.workers.clone().acquire_owned().await.map_err(error)?;
        let disk = self.disk.clone();
        let index = self.index.clone();
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let started = std::time::Instant::now();
            let entry = index.entries.load().get(&key).cloned();
            let mut hit = None;
            if let Some(entry) = entry
                && !index.disabled.load(Ordering::Acquire)
            {
                let path = root.join(&key);
                let result = (|| {
                    if !fs::symlink_metadata(&path)?.is_file() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "cache entry is not regular",
                        ));
                    }
                    read(regular_options().open(path)?, &key)
                })();
                match result {
                    Ok(bytes) if !index.disabled.load(Ordering::Acquire) => {
                        entry.fetch_max(index.tick(), Ordering::Relaxed);
                        hit = Some(bytes);
                    }
                    Err(error) => {
                        with_disk(&disk, |disk| disk.read_failed(&key, &entry, error))?;
                    }
                    Ok(_) => {}
                }
            }
            let nanos = |d: std::time::Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
            index
                .queue_ns
                .fetch_add(nanos(started.duration_since(queued)), Ordering::Relaxed);
            index
                .service_ns
                .fetch_add(nanos(started.elapsed()), Ordering::Relaxed);
            if hit.is_some() {
                index.hits.fetch_add(1, Ordering::Relaxed);
            } else {
                index.misses.fetch_add(1, Ordering::Relaxed);
            }
            Ok((guard, hit))
        })
        .await
        .map_err(error)?
    }
    async fn cache_bytes<G: Send + 'static>(
        &self,
        guard: G,
        key: String,
        bytes: &[u8],
        backing_get: Option<std::time::Duration>,
    ) -> Result<()> {
        // Bound cache-owned copies before allocating, including callers queued
        // for disk workers. Authoritative provider/request buffers are separate.
        let queued = std::time::Instant::now();
        let permit = self.fills.clone().acquire_owned().await.map_err(error)?;
        let bytes = (bytes.len() <= self.max_payload).then(|| bytes.to_vec());
        let started = std::sync::OnceLock::new();
        let finish = move |disk: &mut Disk, started: std::time::Instant| {
            let nanos = |duration: std::time::Duration| {
                u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
            };
            if let Some(elapsed) = backing_get {
                disk.stats.backing_get_nanoseconds = disk
                    .stats
                    .backing_get_nanoseconds
                    .saturating_add(nanos(elapsed));
                disk.stats.read_fill_nanoseconds = disk
                    .stats
                    .read_fill_nanoseconds
                    .saturating_add(nanos(queued.elapsed()));
            }
            disk.stats.fill_attempts = disk.stats.fill_attempts.saturating_add(1);
            disk.stats.fill_wait_and_copy_nanoseconds = disk
                .stats
                .fill_wait_and_copy_nanoseconds
                .saturating_add(nanos(started.duration_since(queued)));
            disk.stats.fill_service_nanoseconds = disk
                .stats
                .fill_service_nanoseconds
                .saturating_add(nanos(started.elapsed()));
        };
        let workers = self.workers.clone();
        let (disk, root) = (self.disk.clone(), self.root.clone());
        let work = async move {
            let worker = workers.acquire_owned().await.map_err(error)?;
            tokio::task::spawn_blocking(move || {
                // Held until the worker is done with the cache files.
                let _held = (guard, permit, worker);
                let started = *started.get_or_init(std::time::Instant::now);
                fill(&disk, &root, key, bytes.as_deref(), |disk| {
                    finish(disk, started)
                })
            })
            .await
            .map_err(error)?
        };
        if self.background_read_fills && backing_get.is_some() {
            tokio::spawn(work);
        } else {
            work.await?;
        }
        Ok(())
    }
    pub async fn stats(&self) -> Result<DiskCacheStats> {
        Ok(self
            .disk((), |disk| {
                let mut stats = disk.stats;
                stats.hits = disk.index.hits.load(Ordering::Relaxed);
                stats.misses = disk.index.misses.load(Ordering::Relaxed);
                stats.read_probes = stats.hits.saturating_add(stats.misses);
                stats.read_queue_nanoseconds = disk.index.queue_ns.load(Ordering::Relaxed);
                stats.read_service_nanoseconds = disk.index.service_ns.load(Ordering::Relaxed);
                stats
            })
            .await?
            .1)
    }
    /// Clear only cached copies. A failure disables cache reads/admission.
    pub async fn clear(&self) -> Result<()> {
        let guard = self.io.clone().write_owned().await;
        self.disk(guard, |disk| {
            for key in disk.entries.keys().cloned().collect::<Vec<_>>() {
                if let Err(e) = disk.remove(&key) {
                    disk.disable();
                    return Err(error(e));
                }
            }
            File::open(&disk.root)
                .and_then(|file| file.sync_all())
                .map_err(|e| {
                    disk.disable();
                    error(e)
                })
        })
        .await?
        .1
    }
}
#[async_trait]
impl<B: BlockStore + ?Sized + 'static> BlockStore for DiskCachedBlockStore<B> {
    fn durable(&self) -> bool {
        self.backing.durable()
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let guard = self.io.clone().read_owned().await;
        let _write = self.writes.read().await;
        let id = self.backing.put(bytes).await?;
        // Only authoritative put/flush ordering needs this gate. Keep the I/O
        // exclusion guard through cache work, but do not delay backing flush
        // behind non-authoritative fill admission or disk-cache I/O.
        drop(_write);
        let key = digest(id.0.as_bytes());
        self.cache_bytes(guard, key, bytes, None).await?;
        Ok(id)
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        let guard = self.io.clone().read_owned().await;
        let key = digest(id.0.as_bytes());
        let (guard, hit) = self.read_cached(guard, key.clone(), read_file).await?;
        if let Some(bytes) = hit {
            return Ok(bytes);
        }
        let started = std::time::Instant::now();
        let bytes = self.backing.get(id).await?;
        self.cache_bytes(guard, key, &bytes, Some(started.elapsed()))
            .await?;
        Ok(bytes)
    }
    async fn flush(&self) -> Result<()> {
        let _write = self.writes.write().await;
        self.backing.flush().await
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        let guard = self.io.clone().write_owned().await;
        let key = digest(id.0.as_bytes());
        let (_guard, invalidation) = self
            .disk(guard, move |disk| {
                let result = disk
                    .remove(&key)
                    .and_then(|()| File::open(&disk.root)?.sync_all());
                result.map_err(|e| {
                    disk.disable();
                    error(e)
                })
            })
            .await?;
        // A restart must not resurrect a cached copy of an authoritative delete.
        // If durable invalidation fails, do not start the backing deletion.
        invalidation?;
        self.backing.delete(id).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    #[derive(Default)]
    struct Backing {
        data: Mutex<BTreeMap<String, Vec<u8>>>,
        gets: AtomicUsize,
        fail_put: AtomicBool,
        fail_flush: AtomicBool,
        stall_delete: AtomicBool,
        deleted: tokio::sync::Notify,
        put_completed: tokio::sync::Notify,
    }
    #[async_trait]
    impl BlockStore for Backing {
        fn durable(&self) -> bool {
            true
        }
        async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
            if self.fail_put.load(Ordering::SeqCst) {
                return Err(error("put failed"));
            }
            // Deliberately opaque IDs, including path separators.
            let id = format!("../../opaque/{}", digest(bytes));
            self.data.lock().unwrap().insert(id.clone(), bytes.to_vec());
            self.put_completed.notify_one();
            Ok(BlockId(id))
        }
        async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            self.data
                .lock()
                .unwrap()
                .get(&id.0)
                .cloned()
                .ok_or_else(|| error("missing backing block"))
        }
        async fn flush(&self) -> Result<()> {
            if self.fail_flush.load(Ordering::SeqCst) {
                Err(error("flush failed"))
            } else {
                Ok(())
            }
        }
        async fn delete(&self, id: &BlockId) -> Result<()> {
            self.data.lock().unwrap().remove(&id.0);
            self.deleted.notify_one();
            if self.stall_delete.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(())
        }
    }
    #[tokio::test]
    async fn deferred_fill_returns_before_disk_work_and_delete_invalidates_it() {
        let root = tempfile::tempdir().unwrap();
        let backing = Arc::new(Backing::default());
        let id = backing.put(b"from provider").await.unwrap();
        let budget = DiskCacheBudget::new(1, 1)
            .unwrap()
            .with_background_read_fills(true);
        let cache = DiskCachedBlockStore::open_with_budget(
            backing,
            root.path(),
            "deferred",
            1024,
            8,
            budget,
        )
        .await
        .unwrap();
        let worker = cache.workers.clone().acquire_owned().await.unwrap();
        let read = tokio::time::timeout(std::time::Duration::from_secs(2), cache.get(&id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, b"from provider");
        assert_eq!(cache.fills.available_permits(), 0);
        assert!(cache.io.try_write().is_none());
        drop(worker);
        // Acquiring the sole fill permit waits for insertion to finish.
        let filled = cache.fills.clone().acquire_owned().await.unwrap();
        assert_eq!(cache.get(&id).await.unwrap(), b"from provider");
        assert_eq!(cache.stats().await.unwrap().hits, 1);
        drop(filled);
        cache.delete(&id).await.unwrap();
        assert!(cache.get(&id).await.is_err());
    }
    #[tokio::test]
    async fn backing_flush_does_not_wait_for_cache_fill_admission() {
        let root = tempfile::tempdir().unwrap();
        let backing = Arc::new(Backing::default());
        let cache = Arc::new(
            DiskCachedBlockStore::open(backing.clone(), root.path(), "flush-scope", 1024, 8)
                .await
                .unwrap(),
        );
        let permits = cache
            .fills
            .clone()
            .acquire_many_owned(cache.fills.available_permits() as u32)
            .await
            .unwrap();
        let writer = cache.clone();
        let task = tokio::spawn(async move { writer.put(b"committed").await });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            backing.put_completed.notified(),
        )
        .await
        .unwrap();
        assert!(!task.is_finished(), "cache fill should remain blocked");
        tokio::time::timeout(std::time::Duration::from_secs(2), cache.flush())
            .await
            .expect("backing flush waited on cache fill")
            .unwrap();
        drop(permits);
        let id = task.await.unwrap().unwrap();
        assert_eq!(cache.get(&id).await.unwrap(), b"committed");
    }

    #[tokio::test]
    async fn persists_verified_hits_and_excludes_another_owner() {
        let root = tempfile::tempdir().unwrap();
        let backing = Arc::new(Backing::default());
        let cache =
            DiskCachedBlockStore::open(backing.clone(), root.path(), "volume-generation", 1024, 8)
                .await
                .unwrap();
        let id = cache.put(b"persistent").await.unwrap();
        // The previous writer appended these fields individually. Keep its on-disk format.
        let mut expected = b"MNTCACH1".to_vec();
        expected.extend(Sha256::digest(digest(id.0.as_bytes()).as_bytes()));
        expected.extend(10_u64.to_le_bytes());
        expected.extend(Sha256::digest(b"persistent"));
        expected.extend(b"persistent");
        assert_eq!(
            fs::read(cache_path(root.path(), "volume-generation", &id)).unwrap(),
            expected
        );
        assert!(
            DiskCachedBlockStore::open(backing.clone(), root.path(), "volume-generation", 1024, 8)
                .await
                .is_err()
        );
        assert_eq!(cache.clone().get(&id).await.unwrap(), b"persistent");
        assert_eq!(cache.stats().await.unwrap().fill_attempts, 1);
        drop(cache);
        let reopened =
            DiskCachedBlockStore::open(backing.clone(), root.path(), "volume-generation", 1024, 8)
                .await
                .unwrap();
        assert_eq!(reopened.get(&id).await.unwrap(), b"persistent");
        assert_eq!(backing.gets.load(Ordering::SeqCst), 0);
        let other = DiskCachedBlockStore::open(
            backing.clone(),
            root.path(),
            "different-generation",
            1024,
            8,
        )
        .await
        .unwrap();
        assert_eq!(other.get(&id).await.unwrap(), b"persistent");
        assert_eq!(backing.gets.load(Ordering::SeqCst), 1);
        let hit_stats = reopened.stats().await.unwrap();
        let miss_stats = other.stats().await.unwrap();
        // Ordinary probes never acquire the maintenance mutex.
        // Stats snapshots exclude their own section; misses also fill, which
        // takes one section to reserve space and one to publish the file.
        assert_eq!(hit_stats.lock_sections, 0);
        assert_eq!(miss_stats.lock_sections, 2);
        for stats in [hit_stats, miss_stats] {
            assert!(stats.lock_wait_max_nanoseconds <= stats.lock_wait_nanoseconds);
            assert!(stats.lock_hold_max_nanoseconds <= stats.lock_hold_nanoseconds);
        }
        assert_eq!(hit_stats.fill_attempts, 0);
        assert_eq!(miss_stats.fill_attempts, 1);
        assert_eq!(other.stats().await.unwrap().fill_attempts, 1);
        assert_eq!((hit_stats.read_probes, hit_stats.hits), (1, 1));
        assert_eq!((miss_stats.read_probes, miss_stats.misses), (1, 1));
        assert_eq!(miss_stats.read_queue_nanoseconds, 0);
        // Stats snapshots and fills do not add read probes.
        assert_eq!(other.stats().await.unwrap().read_probes, 1);
        assert!(cache_path(root.path(), "volume-generation", &id).is_file());
    }
    fn cache_path(root: &Path, scope: &str, id: &BlockId) -> PathBuf {
        root.join(digest(scope.as_bytes()))
            .join(digest(id.0.as_bytes()))
    }
    #[tokio::test]
    async fn corruption_refetches_and_never_masks_missing_backing() {
        let root = tempfile::tempdir().unwrap();
        let backing = Arc::new(Backing::default());
        let cache = DiskCachedBlockStore::open(backing.clone(), root.path(), "scope", 1024, 8)
            .await
            .unwrap();
        let id = cache.put(b"correct").await.unwrap();
        let path = cache_path(root.path(), "scope", &id);
        fs::write(&path, b"truncated").unwrap();
        assert_eq!(cache.get(&id).await.unwrap(), b"correct");
        let mut bytes = fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(&path, bytes).unwrap();
        backing.data.lock().unwrap().clear();
        assert!(cache.get(&id).await.is_err());
        assert_eq!(cache.stats().await.unwrap().corruptions, 2);
    }
    #[tokio::test]
    async fn byte_entry_bounds_reopen_pruning_and_oversize_bypass() {
        let root = tempfile::tempdir().unwrap();
        let backing = Arc::new(Backing::default());
        let cache = DiskCachedBlockStore::open(backing.clone(), root.path(), "scope", 170, 2)
            .await
            .unwrap();
        let a = cache.put(b"aaaaa").await.unwrap();
        let b = cache.put(b"bbbbb").await.unwrap();
        cache.get(&a).await.unwrap();
        let c = cache.put(b"ccccc").await.unwrap();
        assert!(!cache_path(root.path(), "scope", &b).exists());
        assert!(cache_path(root.path(), "scope", &a).exists());
        assert!(cache_path(root.path(), "scope", &c).exists());
        assert_eq!(cache.stats().await.unwrap().resident_bytes, 170);
        let large = cache.put(&[7; 171]).await.unwrap();
        assert_eq!(cache.get(&large).await.unwrap(), [7; 171]);
        assert!(!cache_path(root.path(), "scope", &large).exists());
        drop(cache);
        let cache = DiskCachedBlockStore::open(backing, root.path(), "scope", 85, 1)
            .await
            .unwrap();
        assert_eq!(cache.stats().await.unwrap().resident_entries, 1);
        assert_eq!(cache.stats().await.unwrap().resident_bytes, 85);
        cache.clear().await.unwrap();
        assert_eq!(cache.stats().await.unwrap().resident_bytes, 0);
    }
    #[tokio::test]
    async fn backing_errors_and_cancelled_delete_do_not_leave_hits() {
        let root = tempfile::tempdir().unwrap();
        let backing = Arc::new(Backing::default());
        let cache = DiskCachedBlockStore::open(backing.clone(), root.path(), "scope", 1024, 8)
            .await
            .unwrap();
        backing.fail_put.store(true, Ordering::SeqCst);
        assert!(cache.put(b"failed").await.is_err());
        assert_eq!(cache.stats().await.unwrap().resident_entries, 0);
        backing.fail_put.store(false, Ordering::SeqCst);
        let id = cache.put(b"delete-me").await.unwrap();
        backing.fail_flush.store(true, Ordering::SeqCst);
        assert!(cache.flush().await.is_err());
        backing.stall_delete.store(true, Ordering::SeqCst);
        let child = cache.clone();
        let child_id = id.clone();
        let task = tokio::spawn(async move { child.delete(&child_id).await });
        backing.deleted.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(cache.get(&id).await.is_err());
        drop(cache);
        let reopened = DiskCachedBlockStore::open(backing, root.path(), "scope", 1024, 8)
            .await
            .unwrap();
        assert!(reopened.get(&id).await.is_err());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "release-mode disk cache latency diagnostic"]
    async fn profile_concurrent_warm_disk_reads() {
        let root = tempfile::tempdir().unwrap();
        let backing = Arc::new(Backing::default());
        let cache =
            DiskCachedBlockStore::open(backing.clone(), root.path(), "profile", 1 << 20, 128)
                .await
                .unwrap();
        let mut ids = Vec::new();
        for client in 0..100_u8 {
            ids.push(cache.put(&[client; 4096]).await.unwrap());
        }
        for clients in [1, 10, 50, 100] {
            let before = cache.stats().await.unwrap();
            let start = Arc::new(tokio::sync::Barrier::new(clients));
            let mut tasks = tokio::task::JoinSet::new();
            for (client, id) in ids.iter().take(clients).enumerate() {
                let (cache, id, start) = (cache.clone(), id.clone(), start.clone());
                tasks.spawn(async move {
                    start.wait().await;
                    let mut durations = Vec::with_capacity(100);
                    for _ in 0..100 {
                        let now = std::time::Instant::now();
                        let bytes = cache.get(&id).await.unwrap();
                        durations.push(now.elapsed());
                        assert_eq!(bytes.as_slice(), &[client as u8; 4096]);
                    }
                    durations
                });
            }
            let mut durations = Vec::new();
            while let Some(result) = tasks.join_next().await {
                durations.extend(result.unwrap());
            }
            durations.sort_unstable();
            let after = cache.stats().await.unwrap();
            assert_eq!(after.hits - before.hits, durations.len() as u64);
            assert_eq!(after.lock_sections - before.lock_sections, 1); // previous stats call
            assert_eq!(backing.gets.load(Ordering::Relaxed), 0);
            let percentile = |numerator: usize| {
                durations[(durations.len() * numerator).div_ceil(10000) - 1].as_secs_f64() * 1000.
            };
            let per_hit_us = |ns: u64| ns as f64 / durations.len() as f64 / 1000.;
            println!(
                "clients={clients} hits={} probe_mutex_acquisitions=0 p99_ms={:.3} p99_95_ms={:.3} mean_queue_us={:.1} mean_service_us={:.1}",
                durations.len(),
                percentile(9900),
                percentile(9995),
                per_hit_us(after.read_queue_nanoseconds - before.read_queue_nanoseconds),
                per_hit_us(after.read_service_nanoseconds - before.read_service_nanoseconds),
            );
        }
    }

    #[test]
    fn opened_reads_survive_eviction_without_resurrecting_or_removing_replacements() {
        let root = tempfile::tempdir().unwrap();
        let mut disk = Disk::open(root.path(), "scope", 1024, 1).unwrap();
        let key = digest(b"one");
        disk.insert(key.clone(), b"original");
        let original = disk.index.entries.load()[&key].clone();
        let opened = regular_options().open(disk.root.join(&key)).unwrap();
        disk.insert(digest(b"two"), b"evicts original");
        assert_eq!(read_file(opened, &key).unwrap(), b"original");
        original.fetch_max(disk.index.tick(), Ordering::Relaxed);
        assert!(!disk.entries.contains_key(&key));
        disk.insert(key.clone(), b"replacement");
        let replacement = disk.entries[&key];
        disk.read_failed(
            &key,
            &original,
            std::io::Error::new(std::io::ErrorKind::InvalidData, "old corrupt entry"),
        );
        assert_eq!(disk.entries[&key], replacement);
        assert_eq!(
            read_file(regular_options().open(disk.root.join(&key)).unwrap(), &key).unwrap(),
            b"replacement"
        );
        assert_eq!(disk.stats.resident_entries, 1);
        assert_eq!(
            disk.stats.resident_bytes,
            (HEADER + b"replacement".len()) as u64
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cached_reads_overlap_and_cancelled_workers_retain_exclusion() {
        let root = tempfile::tempdir().unwrap();
        let cache = DiskCachedBlockStore::open_with_budget(
            Arc::new(Backing::default()),
            root.path(),
            "scope",
            1024,
            8,
            DiskCacheBudget::new(2, 2).unwrap(),
        )
        .await
        .unwrap();
        let id = cache.put(b"parallel").await.unwrap();
        let (started, mut entered) = tokio::sync::mpsc::unbounded_channel();
        let mut tasks = Vec::new();
        let mut releases = Vec::new();
        for _ in 0..2 {
            let child = cache.clone();
            let started = started.clone();
            let key = digest(id.0.as_bytes());
            let (release, wait) = std::sync::mpsc::channel();
            releases.push(release);
            tasks.push(tokio::spawn(async move {
                let guard = child.io.clone().read_owned().await;
                child
                    .read_cached(guard, key, move |file, key| {
                        started.send(()).unwrap();
                        wait.recv().expect("test released worker");
                        read_file(file, key)
                    })
                    .await
            }));
        }
        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_secs(5), entered.recv())
                .await
                .expect("reads serialized during file I/O")
                .unwrap();
        }
        // Both workers are inside file reading; index access still completes.
        assert_eq!(cache.disk.try_lock().unwrap().stats.resident_entries, 1);
        let first = tasks.remove(0);
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert!(cache.io.try_write().is_none());
        assert!(cache.workers.try_acquire().is_err());
        for release in releases {
            release.send(()).unwrap();
        }
        let (guard, bytes) = tasks.remove(0).await.unwrap().unwrap();
        assert_eq!(bytes.unwrap(), b"parallel");
        drop(guard);
        tokio::time::timeout(std::time::Duration::from_secs(5), cache.clear())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cache.stats().await.unwrap().resident_entries, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_read_fill_clear_and_delete_preserve_bytes_and_bounds() {
        let root = tempfile::tempdir().unwrap();
        let backing = Arc::new(Backing::default());
        let max_bytes = (2 * (HEADER + 4096)) as u64;
        let cache = DiskCachedBlockStore::open_with_budget(
            backing.clone(),
            root.path(),
            "churn",
            max_bytes,
            2,
            DiskCacheBudget::new(4, 4).unwrap(),
        )
        .await
        .unwrap();
        let mut fixtures = Vec::new();
        for value in 0..4_u8 {
            let bytes = vec![value; 4096];
            fixtures.push((cache.put(&bytes).await.unwrap(), bytes));
        }
        let fixtures = Arc::new(fixtures);
        let mut tasks = tokio::task::JoinSet::new();
        for client in 0..8_u8 {
            let cache = cache.clone();
            let fixtures = fixtures.clone();
            tasks.spawn(async move {
                for iteration in 0..50_u8 {
                    let (id, expected) = &fixtures[usize::from(iteration) % fixtures.len()];
                    assert_eq!(cache.get(id).await.unwrap(), *expected);
                    let mut bytes = vec![0xa5; 4096];
                    bytes[0] = client;
                    bytes[1] = iteration;
                    let private = cache.put(&bytes).await.unwrap();
                    assert_eq!(cache.get(&private).await.unwrap(), bytes);
                    cache.delete(&private).await.unwrap();
                    assert!(
                        cache.get(&private).await.is_err(),
                        "deleted data resurfaced"
                    );
                    if client == 0 && iteration % 5 == 0 {
                        cache.clear().await.unwrap();
                    }
                    if iteration % 10 == 0 {
                        cache.flush().await.unwrap();
                    }
                    let stats = cache.stats().await.unwrap();
                    assert!(stats.resident_bytes <= max_bytes);
                    assert!(stats.resident_entries <= 2);
                    assert!(!stats.disabled);
                    assert_eq!(stats.corruptions, 0);
                }
            });
        }
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while let Some(task) = tasks.join_next().await {
                task.unwrap();
            }
        })
        .await
        .expect("cache churn deadlocked");
        let stats = cache.stats().await.unwrap();
        assert!(stats.evictions > 0);
        assert!(stats.read_probes >= 1200);
        assert_eq!(stats.io_failures, 0);
        drop(cache);
        let reopened = DiskCachedBlockStore::open(backing, root.path(), "churn", max_bytes, 2)
            .await
            .unwrap();
        for (id, bytes) in fixtures.iter() {
            assert_eq!(reopened.get(id).await.unwrap(), *bytes);
        }
    }

    #[tokio::test]
    async fn cancelled_blocking_worker_keeps_invalidation_excluded_until_finished() {
        let root = tempfile::tempdir().unwrap();
        let budget = DiskCacheBudget::new(1, 1).unwrap();
        let cache = DiskCachedBlockStore::open_with_budget(
            Arc::new(Backing::default()),
            root.path(),
            "scope",
            1024,
            8,
            budget.clone(),
        )
        .await
        .unwrap();
        let other = DiskCachedBlockStore::open_with_budget(
            Arc::new(Backing::default()),
            root.path(),
            "other-volume",
            1024,
            8,
            budget,
        )
        .await
        .unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(std::sync::Barrier::new(2));
        let child = cache.clone();
        let signal = started.clone();
        let barrier = release.clone();
        let task = tokio::spawn(async move {
            let guard = child.io.clone().read_owned().await;
            child
                .disk(guard, move |disk| {
                    signal.notify_one();
                    barrier.wait();
                    disk.insert(digest(b"late"), b"late fill");
                })
                .await
        });
        started.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let excluded = cache.io.try_write().is_none();
        let shared_worker_held = other.workers.try_acquire().is_err();
        release.wait();
        assert!(
            shared_worker_held,
            "cancelled worker released the shared server budget early"
        );
        let id = other.put(b"other volume").await.unwrap();
        assert_eq!(other.get(&id).await.unwrap(), b"other volume");
        cache.clear().await.unwrap();
        assert!(
            excluded,
            "caller cancellation released exclusion before its disk worker finished"
        );
        assert_eq!(cache.stats().await.unwrap().resident_entries, 0);
    }

    #[test]
    fn shared_budget_rejects_invalid_limits() {
        assert!(DiskCacheBudget::new(0, 1).is_err());
        assert!(DiskCacheBudget::new(1, 0).is_err());
        assert!(DiskCacheBudget::new(usize::MAX, 1).is_err());
        assert!(DiskCacheBudget::new(1, usize::MAX).is_err());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn symlink_entries_are_removed_without_reading_the_target() {
        let root = tempfile::tempdir().unwrap();
        let cache =
            DiskCachedBlockStore::open(Arc::new(Backing::default()), root.path(), "scope", 1024, 8)
                .await
                .unwrap();
        let id = cache.put(b"safe").await.unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, b"unchanged").unwrap();
        let path = cache_path(root.path(), "scope", &id);
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert_eq!(cache.get(&id).await.unwrap(), b"safe");
        assert_eq!(fs::read(outside).unwrap(), b"unchanged");
        assert_eq!(cache.stats().await.unwrap().corruptions, 1);
    }
    #[tokio::test]
    async fn failed_eviction_disables_cache_and_failed_invalidation_preserves_backing() {
        let root = tempfile::tempdir().unwrap();
        let backing = Arc::new(Backing::default());
        let cache = DiskCachedBlockStore::open(backing.clone(), root.path(), "scope", 85, 1)
            .await
            .unwrap();
        let id = cache.put(b"first").await.unwrap();
        let path = cache_path(root.path(), "scope", &id);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let newer = cache.put(b"newer").await.unwrap();
        assert!(cache.stats().await.unwrap().disabled);
        assert_eq!(cache.get(&newer).await.unwrap(), b"newer");
        assert!(cache.delete(&id).await.is_err());
        assert_eq!(backing.get(&id).await.unwrap(), b"first");
        fs::remove_dir(&path).unwrap();
        cache.delete(&id).await.unwrap();
        assert!(backing.get(&id).await.is_err());
    }

    #[test]
    fn a_fill_being_written_holds_its_space_against_both_bounds() {
        let root = tempfile::tempdir().unwrap();
        let size = (HEADER + 8) as u64;
        // Room for two entries by count and by bytes.
        let mut disk = Disk::open(root.path(), "reserve", size * 2, 2).unwrap();
        let (first, second, third) = (digest(b"1"), digest(b"2"), digest(b"3"));
        assert!(disk.reserve(&first, size));
        assert!(disk.reserve(&second, size));
        // Nothing is resident yet, so there is nothing to evict for a third.
        assert!(!disk.reserve(&third, size));
        assert_eq!(disk.stats.admission_rejections, 1);
        let root = disk.root.clone();
        disk.commit(first.clone(), size, write_entry(&root, &first, b"11111111"));
        assert_eq!((disk.stats.resident_entries, disk.reserved_entries), (1, 1));
        // The committed entry can be evicted; the reserved one cannot.
        assert!(disk.reserve(&third, size));
        assert_eq!(disk.stats.evictions, 1);
        assert!(!root.join(&first).exists());
        disk.commit(
            second.clone(),
            size,
            write_entry(&root, &second, b"22222222"),
        );
        disk.commit(third.clone(), size, write_entry(&root, &third, b"33333333"));
        assert_eq!((disk.reserved_bytes, disk.reserved_entries), (0, 0));
        assert_eq!(disk.stats.resident_bytes, size * 2);
        // A second fill of a resident block replaces it without counting twice.
        assert!(disk.reserve(&third, size));
        disk.commit(third.clone(), size, write_entry(&root, &third, b"33333333"));
        assert_eq!(
            (disk.stats.resident_entries, disk.stats.resident_bytes),
            (2, size * 2)
        );
        assert_eq!(disk.stats.evictions, 1);
        assert!(!disk.stats.disabled);
    }
}
