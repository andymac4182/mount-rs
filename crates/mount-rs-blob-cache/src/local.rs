use crate::*;
use arc_swap::ArcSwap;
use im::OrdMap;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
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
/// When an entry was last used and last advertised. Both only steer choices
/// (which entry to evict, whether to advertise again); neither is part of
/// what the index accounts for, so a hit records itself here and leaves the
/// index alone.
#[derive(Default)]
struct Stamp {
    /// The cache's `tick` at the last use.
    used: AtomicU64,
    /// Nanoseconds from the cache's `epoch` to the last advertisement, plus
    /// one; zero until the first.
    advertised: AtomicU64,
    /// While the slot is free: the free slot below it, plus one.
    next: AtomicU32,
}
/// Every entry's stamps, side by side. Choosing what to evict reads the
/// last use of every entry; with the stamps packed together, and the
/// index's `seats` beside them, that is a walk over two arrays and not a
/// pointer chase through the index to each entry's own allocation.
///
/// Slots are handed out and taken back without a lock: free slots form a
/// stack threaded through `next`, and the stack head carries a count of
/// changes so that a pop which raced a pop-and-push of the same slot fails
/// and looks again. Storage only grows, a chunk at a time, each chunk twice
/// the size of the one before, so a slot's address never changes.
struct Slab {
    chunks: [OnceLock<Box<[Stamp]>>; Self::CHUNKS],
    /// Slots ever created.
    created: AtomicU32,
    /// Change count in the high half; top free slot plus one in the low.
    free: AtomicU64,
}
impl Slab {
    const FIRST: usize = 1024;
    /// Enough doubling chunks to hold every `u32` slot number.
    const CHUNKS: usize = 23;
    // Only a cache that opens builds one, and it opens only on Unix.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn new() -> Arc<Self> {
        Arc::new(Self {
            chunks: [const { OnceLock::new() }; Self::CHUNKS],
            created: AtomicU32::new(0),
            free: AtomicU64::new(0),
        })
    }
    /// Chunk `c` holds `FIRST << c` slots, after the `FIRST * (2^c - 1)`
    /// slots of the chunks before it.
    fn place(slot: u32) -> (usize, usize) {
        let chunk = (slot as usize / Self::FIRST + 1).ilog2() as usize;
        (chunk, slot as usize - ((1 << chunk) - 1) * Self::FIRST)
    }
    fn stamp(&self, slot: u32) -> &Stamp {
        let (chunk, offset) = Self::place(slot);
        // A slot number is only ever held by an entry it was handed to,
        // which happens after its chunk exists.
        &self.chunks[chunk]
            .get()
            .expect("stamp slot without storage")[offset]
    }
    fn take(&self) -> Result<u32> {
        let mut head = self.free.load(Ordering::Acquire);
        while head as u32 != 0 {
            let slot = head as u32 - 1;
            // If the slot was popped since `head` was read this may be
            // anything, and the changed count fails the exchange.
            let below = self.stamp(slot).next.load(Ordering::Acquire);
            let next = ((head >> 32).wrapping_add(1) << 32) | u64::from(below);
            match self
                .free
                .compare_exchange_weak(head, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Ok(slot),
                Err(now) => head = now,
            }
        }
        let slot = self
            .created
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| error())?;
        let (chunk, _) = Self::place(slot);
        self.chunks[chunk].get_or_init(|| {
            (0..Self::FIRST << chunk)
                .map(|_| Stamp::default())
                .collect()
        });
        Ok(slot)
    }
    fn give_back(&self, slot: u32) {
        let stamp = self.stamp(slot);
        let mut head = self.free.load(Ordering::Acquire);
        loop {
            stamp.next.store(head as u32, Ordering::Release);
            let next = ((head >> 32).wrapping_add(1) << 32) | u64::from(slot + 1);
            match self
                .free
                .compare_exchange_weak(head, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(now) => head = now,
            }
        }
    }
}
/// One entry's slot in the slab. Every version of the entry shares it, and
/// it goes back to the slab when the last version, in the last snapshot of
/// the index that anyone still holds, is dropped: no reader can be looking
/// at a slot that has been handed to another entry.
struct Stamps {
    slab: Arc<Slab>,
    slot: u32,
}
impl Stamps {
    fn new(slab: &Arc<Slab>, used: u64) -> Result<Arc<Self>> {
        let slot = slab.take()?;
        let stamp = slab.stamp(slot);
        stamp.used.store(used, Ordering::Relaxed);
        stamp.advertised.store(0, Ordering::Release);
        Ok(Arc::new(Self {
            slab: slab.clone(),
            slot,
        }))
    }
}
impl Drop for Stamps {
    fn drop(&mut self) {
        self.slab.give_back(self.slot);
    }
}
#[derive(Clone)]
struct Entry {
    bytes: Option<Arc<[u8]>>,
    size: usize,
    disk: bool,
    scope: Option<CacheKey>,
    generation: u64,
    stamps: Arc<Stamps>,
}
/// An entry as the index holds it: behind its own pointer, so that the copy
/// of a path which a change makes copies pointers and not entries, with its
/// stamp slot beside the pointer so that a hit does not follow it.
#[derive(Clone)]
struct Indexed {
    entry: Arc<Entry>,
    slot: u32,
}
impl std::ops::Deref for Indexed {
    type Target = Entry;
    fn deref(&self) -> &Entry {
        &self.entry
    }
}
/// Hashes a cache key for the index. A key is already a SHA-256 digest, so
/// its first eight bytes are as good as any hash of all of it; they are
/// mixed with a seed drawn when the cache opens, so which keys share a
/// bucket cannot be arranged from outside.
#[derive(Clone)]
struct KeyHasher(u64);
impl std::hash::BuildHasher for KeyHasher {
    type Hasher = KeyHash;
    fn build_hasher(&self) -> KeyHash {
        KeyHash(self.0)
    }
}
struct KeyHash(u64);
impl std::hash::Hasher for KeyHash {
    fn write(&mut self, bytes: &[u8]) {
        if let Some(head) = bytes.first_chunk::<8>() {
            let mixed = (u64::from_le_bytes(*head) ^ self.0).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            self.0 = mixed ^ (mixed >> 32);
        }
    }
    // The length of a key is always the same and says nothing.
    fn write_usize(&mut self, _: usize) {}
    fn finish(&self) -> u64 {
        self.0
    }
}
/// What an eviction scan asks of an entry, at the entry's stamp slot.
#[derive(Clone, Copy)]
struct Seat {
    key: CacheKey,
    /// The entry's bytes are in RAM.
    resident: bool,
    /// The entry has a file.
    disk: bool,
}
/// The index and its byte counts, as one immutable value. A change builds
/// the next value from the current one and publishes it by compare-and-swap,
/// so the counts always describe exactly the entries beside them. The
/// collections share structure between versions: a change copies a path,
/// not the collection.
#[derive(Clone)]
struct State {
    /// Changed only through `put`, `take` and `edit`, which keep `seats`
    /// in step. Nothing needs the keys in order, and a hash trie finds one
    /// in fewer steps than an ordered tree compares 32-byte keys.
    entries: im::HashMap<CacheKey, Indexed, KeyHasher>,
    /// `seats[slot]` describes the entry whose stamps are in `slot`, and is
    /// empty for a slot none of this state's entries holds.
    seats: im::Vector<Option<Seat>>,
    memory: usize,
    /// Bytes of every file the cache may have on disk: those of indexed
    /// entries, of quarantined artifacts, of files unindexed but not yet
    /// unlinked, and the room held by inserts that are writing.
    disk: usize,
    /// The last generation handed out. A file is named for its key and the
    /// generation of the insert that wrote it, so no name is written twice.
    generation: u64,
    quarantined: OrdMap<String, usize>,
}
/// A file whose entry has left the index. Its bytes stay counted in
/// `State::disk` until `settle` has unlinked it.
struct Doomed {
    name: String,
    charge: usize,
}
impl State {
    // Only a cache that opens builds one, and it opens only on Unix.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn new() -> Self {
        use std::hash::BuildHasher;
        let seed = std::collections::hash_map::RandomState::new().hash_one(0_u8);
        Self {
            entries: im::HashMap::with_hasher(KeyHasher(seed)),
            seats: im::Vector::new(),
            memory: 0,
            disk: 0,
            generation: 0,
            quarantined: OrdMap::new(),
        }
    }
    /// Index an entry under a key that has none.
    fn put(&mut self, key: CacheKey, entry: Entry) {
        let slot = entry.stamps.slot;
        while self.seats.len() <= slot as usize {
            self.seats.push_back(None);
        }
        self.seats.set(
            slot as usize,
            Some(Seat {
                key,
                resident: entry.bytes.is_some(),
                disk: entry.disk,
            }),
        );
        let entry = Indexed {
            entry: Arc::new(entry),
            slot,
        };
        let replaced = self.entries.insert(key, entry);
        debug_assert!(replaced.is_none());
    }
    fn take(&mut self, key: &CacheKey) -> Option<Indexed> {
        let entry = self.entries.remove(key)?;
        self.seats.set(entry.slot as usize, None);
        Some(entry)
    }
    /// Change this state's version of an entry it holds, leaving the
    /// version any older snapshot holds alone.
    fn edit(&mut self, key: &CacheKey, change: impl FnOnce(&mut Entry)) {
        let Some(indexed) = self.entries.get_mut(key) else {
            return;
        };
        let entry = Arc::make_mut(&mut indexed.entry);
        change(entry);
        let seat = Seat {
            key: *key,
            resident: entry.bytes.is_some(),
            disk: entry.disk,
        };
        self.seats.set(indexed.slot as usize, Some(seat));
    }
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
#[cfg(test)]
type Written = Box<dyn Fn(&LocalCache) + Send + Sync>;
/// One bounded cache shared across drives. The index is replaced whole by
/// compare-and-swap and every file is written once under a name of its own
/// (`{key}.{generation}`), so lookups, admissions and disk writes never
/// wait for each other. Only invalidation waits, for the disk work in
/// flight: see `files`.
pub struct LocalCache {
    config: LocalCacheConfig,
    /// Read whole by every lookup; replaced whole by `step`.
    state: ArcSwap<State>,
    /// Held shared by whatever may have a cache file on disk that the index
    /// does not name: an insert, from before it decides to write until its
    /// file is indexed or removed, and anything unlinking a file it has just
    /// taken out of the index. Held exclusive by an invalidation across
    /// unindexing and unlinking. So when an invalidation returns, no file of
    /// a block it invalidated exists, under its final or its temporary name,
    /// and a process that stops at any moment after that cannot leave one
    /// for the next start to adopt. Inserts share the gate and still
    /// overlap each other.
    files: mount_rs_core::drain_gate::DrainGate,
    /// Orders uses for eviction; see `Stamp`.
    tick: AtomicU64,
    stamps: Arc<Slab>,
    epoch: std::time::Instant,
    /// Read for every inbound peer stream; replaced whole, by
    /// compare-and-swap, when a drive registers or unregisters.
    scopes: ArcSwap<Scopes>,
    root: File,
    _lock: File,
    pending: Arc<AtomicUsize>,
    pub(crate) io_permits: Arc<tokio::sync::Semaphore>,
    /// Called by a disk-backed insert between writing its file and indexing
    /// it, for tests that need something to happen exactly there.
    #[cfg(test)]
    written: OnceLock<Written>,
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
        let mut state = State::new();
        let stamps = Slab::new();
        let mut tick = 0;
        // The newest file of each block. An insert that replaced a file and
        // was stopped before unlinking the old one leaves both; a cache
        // written before files carried generations has bare key names,
        // which count as older than any generation.
        let mut newest = BTreeMap::<CacheKey, (u64, String, u64)>::new();
        for entry in fs::read_dir(&config.directory).map_err(|_| error())? {
            let entry = entry.map_err(|_| error())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name
                .strip_suffix(".tmp")
                .is_some_and(|stem| is_key(stem) || parse_name(stem).is_some())
            {
                unlink_relative(&root, &name)?;
                continue;
            }
            let (key, generation) = match parse_key(&name) {
                Some(key) => (key, 0),
                None => match parse_name(&name) {
                    Some(parsed) => parsed,
                    None => continue,
                },
            };
            let f = match open_relative(&root, &name, false, false) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let metadata = f.metadata().map_err(|_| error())?;
            if !metadata.is_file() {
                continue;
            }
            // Later inserts must not reuse the name of any file seen here.
            state.generation = state.generation.max(generation);
            match newest.get(&key) {
                Some((other, _, _)) if *other >= generation => unlink_relative(&root, &name)?,
                _ => {
                    if let Some((_, older, _)) =
                        newest.insert(key, (generation, name, metadata.len()))
                    {
                        unlink_relative(&root, &older)?;
                    }
                }
            }
        }
        for (key, (mut generation, mut name, len)) in newest {
            let size = usize::try_from(len.saturating_sub(32)).map_err(|_| error())?;
            let total = size.checked_add(32).ok_or_else(error)?;
            if len < 32
                || size > config.max_blob_bytes
                || total > config.disk_bytes.saturating_sub(state.disk)
                || state.entries.len() >= config.max_entries
            {
                unlink_relative(&root, &name)?;
                continue;
            }
            if generation == 0 {
                // Adopt a file from the older layout under a name of its own.
                state.generation = state.generation.checked_add(1).ok_or_else(error)?;
                generation = state.generation;
                let adopted = file_name(&key, generation);
                rename_relative(&root, &name, &adopted)?;
                name = adopted;
            }
            debug_assert_eq!(name, file_name(&key, generation));
            state.disk += total;
            tick += 1;
            state.put(
                key,
                Entry {
                    bytes: None,
                    size,
                    disk: true,
                    scope: None,
                    generation,
                    stamps: Stamps::new(&stamps, tick)?,
                },
            );
        }
        Ok(Arc::new(Self {
            config,
            state: ArcSwap::from_pointee(state),
            files: mount_rs_core::drain_gate::DrainGate::new(),
            tick: AtomicU64::new(tick),
            stamps,
            epoch: std::time::Instant::now(),
            scopes: ArcSwap::from_pointee(Scopes::new()),
            root,
            _lock: lock,
            pending: Arc::new(AtomicUsize::new(0)),
            io_permits: Arc::new(tokio::sync::Semaphore::new(8)),
            #[cfg(test)]
            written: OnceLock::new(),
        }))
    }
    /// Make one change to the index. `section` is given a private copy of
    /// the current state to edit; the copy is published only if the state is
    /// still the one it was copied from, and otherwise the section runs
    /// again on the newer state. A section may therefore run several times
    /// and must do nothing but edit the copy and compute its result: only
    /// the result of the published attempt is returned, so a caller acting
    /// on it (unlinking the files of evicted entries, say) acts once.
    fn step<R>(&self, mut section: impl FnMut(&mut State) -> R) -> R {
        match self.try_step(|state| Ok::<_, std::convert::Infallible>(section(state))) {
            Ok(result) => result,
            Err(never) => match never {},
        }
    }
    /// `step` for a section that can refuse: an `Err` publishes nothing.
    fn try_step<R, E>(
        &self,
        mut section: impl FnMut(&mut State) -> std::result::Result<R, E>,
    ) -> std::result::Result<R, E> {
        let mut current = self.state.load_full();
        loop {
            let mut next = State::clone(&current);
            let result = section(&mut next)?;
            let previous = self.state.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return Ok(result);
            }
            current = arc_swap::Guard::into_inner(previous);
        }
    }
    fn next_tick(&self) -> u64 {
        self.tick.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    }
    fn touch(&self, entry: &Indexed) {
        self.stamps
            .stamp(entry.slot)
            .used
            .fetch_max(self.next_tick(), Ordering::Relaxed);
    }
    /// The least recently used of the entries `wanted` accepts.
    fn coldest(&self, state: &State, wanted: impl Fn(&Seat) -> bool) -> Option<CacheKey> {
        let mut coldest: Option<(u64, &Seat)> = None;
        let mut slot = 0_u32;
        for seats in state.seats.leaves() {
            for seat in seats {
                if let Some(seat) = seat
                    && wanted(seat)
                {
                    let used = self.stamps.stamp(slot).used.load(Ordering::Relaxed);
                    if coldest.is_none_or(|(least, _)| used < least) {
                        coldest = Some((used, seat));
                    }
                }
                slot += 1;
            }
        }
        coldest.map(|(_, seat)| seat.key)
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
    /// The file that holds this block now. A block with no file gets a
    /// path that names none: files are named for the insert that wrote
    /// them, so where the next one will go is not known in advance.
    pub fn path_for(&self, scope: &CacheScope, id: &BlockId) -> PathBuf {
        let key = Self::key(scope, id);
        let name = match self.state.load().entries.get(&key) {
            Some(entry) if entry.disk => file_name(&key, entry.generation),
            _ => hex_key(&key),
        };
        self.config.directory.join(name)
    }
    /// RAM bytes are immutable and verified at admission; only the returned Vec allocates.
    pub fn get_memory_shared_hashed(&self, key: &CacheKey) -> Option<Arc<[u8]>> {
        let state = self.state.load();
        let entry = state.entries.get(key)?;
        self.touch(entry);
        entry.bytes.clone()
    }
    pub fn get_memory_hashed(&self, key: &CacheKey) -> Option<Vec<u8>> {
        self.get_memory_shared_hashed(key)
            .map(|bytes| bytes.to_vec())
    }
    pub fn should_advertise_key(&self, key: &CacheKey, interval: std::time::Duration) -> bool {
        let state = self.state.load();
        let Some(entry) = state.entries.get(key) else {
            return false;
        };
        let now = u64::try_from(self.epoch.elapsed().as_nanos())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1);
        let interval = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX);
        let stamp = self.stamps.stamp(entry.slot);
        let last = stamp.advertised.load(Ordering::Acquire);
        if last != 0 && now.saturating_sub(last) < interval {
            return false;
        }
        // Of the callers that find the interval over, one moves the stamp.
        stamp
            .advertised
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
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
        // Reads wait for nothing, so reads of different blocks (and of the
        // same block) overlap each other and every writer. The file read is
        // the one the index named when the read began: it was complete
        // before it was indexed, nothing writes that name again, and its
        // size and digest are checked. The index generation is compared
        // again below, so a read that raced a replacement or an eviction is
        // discarded rather than believed.
        let (size, generation) = {
            let state = self.state.load();
            let entry = state.entries.get(&key)?;
            self.touch(entry);
            if !entry.disk {
                return None;
            }
            (entry.size, entry.generation)
        };
        let bytes = read_checked(
            &self.root,
            &file_name(&key, generation),
            &key,
            size,
            self.config.max_blob_bytes,
        )
        .ok()
        .filter(|b| policy.verify(id, b).is_ok());
        let replaced = |state: &State| {
            state
                .entries
                .get(&key)
                .is_none_or(|entry| entry.generation != generation)
        };
        if let Some(bytes) = &bytes {
            if bytes.len() > self.config.memory_bytes {
                // Nothing to admit, so nothing to change: only look.
                if replaced(&self.state.load()) {
                    return None;
                }
            } else {
                let shared: Arc<[u8]> = Arc::from(bytes.as_slice());
                let current = self.step(|state| {
                    if replaced(state) {
                        return false;
                    }
                    self.admit_shared(state, &key, shared.clone());
                    true
                });
                if !current {
                    return None;
                }
            }
        } else {
            // The file is bad or gone. If the index still names it, the
            // entry goes and so does the file; if the entry was replaced or
            // its file evicted in the meantime, this read just missed.
            let _files = park_on(self.files.read());
            let doomed = self.step(|state| {
                if state
                    .entries
                    .get(&key)
                    .is_some_and(|entry| entry.generation == generation && entry.disk)
                {
                    unindex(state, &key)
                } else {
                    None
                }
            });
            let _ = self.settle(doomed.into_iter().collect());
        }
        bytes
    }
    fn admit_shared(&self, state: &mut State, key: &CacheKey, bytes: Arc<[u8]>) {
        if bytes.len() > self.config.memory_bytes {
            return;
        }
        while bytes.len() > self.config.memory_bytes.saturating_sub(state.memory) {
            let Some(old) = self.coldest(state, |seat| seat.resident) else {
                break;
            };
            let entry = &state.entries[&old];
            state.memory = state.memory.saturating_sub(entry.size);
            if entry.disk {
                state.edit(&old, |entry| entry.bytes = None);
            } else {
                state.take(&old);
            }
        }
        if state
            .entries
            .get(key)
            .is_some_and(|entry| entry.bytes.is_none())
        {
            state.memory += bytes.len();
            state.edit(key, |entry| entry.bytes = Some(bytes));
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
        let scope = Self::scope_hash(scope);
        let mut stamps = None;
        self.try_step(|state| {
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
                    let Some(old) = self.coldest(state, |seat| !seat.disk) else {
                        return Ok(());
                    };
                    unindex(state, &old);
                }
                state.generation = state.generation.checked_add(1).ok_or_else(error)?;
                let generation = state.generation;
                // One slot for the new entry, however often the step reruns.
                let stamps = match &stamps {
                    Some(stamps) => Arc::clone(stamps),
                    None => Arc::clone(stamps.insert(Stamps::new(&self.stamps, self.next_tick())?)),
                };
                state.put(
                    key,
                    Entry {
                        bytes: None,
                        size: bytes.len(),
                        disk: false,
                        scope: Some(scope),
                        generation,
                        stamps,
                    },
                );
            }
            self.admit_shared(state, &key, bytes.clone());
            Ok(())
        })
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
        // Shared from here to the end: until this insert's file is indexed
        // or removed, and every file it unindexes is unlinked, no
        // invalidation runs, and none that has returned can be undone by it.
        let _files = park_on(self.files.read());
        self.retry_quarantine();
        let scope = Self::scope_hash(scope);
        let stamps = Stamps::new(&self.stamps, self.next_tick())?;
        // First step: decide whether the block goes to disk, take out of the
        // index whatever must make way for it, and hold its room. All of it
        // is one step, so the decision and the room it rests on cannot come
        // apart: no new disk write starts while an artifact is quarantined,
        // and two inserts cannot both be given the last of the disk budget.
        let Some(room) = self.try_step(|state| {
            let mut disk = disk_bytes <= self.config.disk_bytes && state.quarantined.is_empty();
            if !disk && !memory {
                return Ok(None);
            }
            // Files this insert takes out of the index. Their bytes stay
            // counted until they are unlinked, which happens before this
            // insert writes anything.
            let mut doomed = Vec::<Doomed>::new();
            let freed = |doomed: &[Doomed]| doomed.iter().map(|file| file.charge).sum::<usize>();
            if let Some(entry) = state.entries.get(&key)
                && let Some(old) = &entry.bytes
            {
                if !Arc::ptr_eq(old, &bytes) && old.as_ref() != bytes.as_ref() {
                    return Err(error());
                }
                if entry.disk {
                    let file = Doomed {
                        name: file_name(&key, entry.generation),
                        charge: entry.size + 32,
                    };
                    state.edit(&key, |entry| entry.disk = false);
                    doomed.push(file);
                }
            } else if let Some(file) = unindex(state, &key) {
                doomed.push(file);
            }
            let mut index = true;
            while !state.entries.contains_key(&key)
                && state.entries.len() + state.quarantined.len() >= self.config.max_entries
            {
                let Some(old) = self.coldest(state, |_| true) else {
                    // Nothing left to evict. What was taken out stays out
                    // and its files still go.
                    index = false;
                    break;
                };
                if let Some(file) = unindex(state, &old) {
                    doomed.push(file);
                }
            }
            disk &= index;
            while disk
                && disk_bytes
                    > self
                        .config
                        .disk_bytes
                        .saturating_sub(state.disk - freed(&doomed))
            {
                let Some(old) = self.coldest(state, |seat| seat.disk) else {
                    // The rest of the budget is held by other inserts that
                    // are writing: this block stays off the disk.
                    disk = false;
                    break;
                };
                let entry = &state.entries[&old];
                let file = Doomed {
                    name: file_name(&old, entry.generation),
                    charge: entry.size + 32,
                };
                if entry.bytes.is_some() {
                    state.edit(&old, |entry| entry.disk = false);
                } else {
                    state.take(&old);
                }
                doomed.push(file);
            }
            // The room this insert holds: enough for the files it must
            // still unlink and for the file it will then write, which are
            // never on disk together.
            let freed = freed(&doomed);
            let hold = if disk { freed.max(disk_bytes) } else { freed };
            state.disk = state.disk - freed + hold;
            state.generation = state.generation.checked_add(1).ok_or_else(error)?;
            Ok(Some(Room {
                index: index && (disk || memory),
                disk,
                generation: state.generation,
                doomed,
                hold,
            }))
        })?
        else {
            return Ok(());
        };
        let Room {
            index,
            disk,
            generation,
            doomed,
            hold,
        } = room;
        let name = file_name(&key, generation);
        let stuck: Vec<&Doomed> = doomed
            .iter()
            .filter(|file| unlink_relative(&self.root, &file.name).is_err())
            .collect();
        // What is still held once the unlinks are done: the room for the
        // file about to be written, unless this insert stops here.
        let keep = if disk && index && stuck.is_empty() {
            disk_bytes
        } else {
            0
        };
        if !stuck.is_empty() || hold != keep {
            self.step(|state| {
                let mut held = hold;
                for file in &stuck {
                    held -= file.charge;
                    quarantine(state, &file.name, file.charge);
                }
                state.disk = state.disk.saturating_sub(held - keep);
            });
        }
        if !stuck.is_empty() {
            return Err(error());
        }
        if !index {
            return Ok(());
        }
        if disk && write_checked(&self.root, &name, &key, &bytes).is_err() {
            let temp = format!("{name}.tmp");
            let left = open_relative(&self.root, &temp, false, false).is_ok();
            self.step(|state| {
                if left {
                    quarantine(state, &temp, disk_bytes);
                } else {
                    state.disk = state.disk.saturating_sub(disk_bytes);
                }
            });
            return Err(error());
        }
        #[cfg(test)]
        if disk && let Some(written) = self.written.get() {
            written(self);
        }
        // Second step: index the block, whose file is now complete.
        let (indexed, displaced) = self.step(|state| {
            // Another insert of this block may have indexed it since the
            // first step. One that brings a file replaces it, and the other
            // file goes; one that brings none leaves the file it found.
            if !disk && state.entries.get(&key).is_some_and(|entry| entry.disk) {
                if memory {
                    self.admit_shared(state, &key, bytes.clone());
                }
                return (true, None);
            }
            let displaced = unindex(state, &key);
            while state.entries.len() + state.quarantined.len() >= self.config.max_entries {
                let Some(old) = self.coldest(state, |seat| !seat.disk) else {
                    return (false, displaced);
                };
                unindex(state, &old);
            }
            state.put(
                key,
                Entry {
                    bytes: None,
                    size: bytes.len(),
                    disk,
                    scope: Some(scope),
                    generation,
                    stamps: stamps.clone(),
                },
            );
            if memory {
                self.admit_shared(state, &key, bytes.clone());
            }
            (true, displaced)
        });
        let mut doomed: Vec<Doomed> = displaced.into_iter().collect();
        if !indexed && disk {
            // Not indexed, so this insert's own file goes; the room held
            // for it is released when it has.
            doomed.push(Doomed {
                name,
                charge: disk_bytes,
            });
        }
        let _ = self.settle(doomed);
        Ok(())
    }
    /// Unlink files whose entries have left the index, then stop counting
    /// the bytes of those that went. One that will not go is quarantined
    /// with its bytes still counted.
    fn settle(&self, doomed: Vec<Doomed>) -> Result<()> {
        if doomed.is_empty() {
            return Ok(());
        }
        let gone: Vec<bool> = doomed
            .iter()
            .map(|file| unlink_relative(&self.root, &file.name).is_ok())
            .collect();
        self.step(|state| {
            for (file, gone) in doomed.iter().zip(&gone) {
                if *gone {
                    state.disk = state.disk.saturating_sub(file.charge);
                } else {
                    quarantine(state, &file.name, file.charge);
                }
            }
        });
        if gone.iter().all(|gone| *gone) {
            Ok(())
        } else {
            Err(error())
        }
    }
    fn retry_quarantine(&self) {
        let state = self.state.load_full();
        for (name, charge) in state.quarantined.iter() {
            if unlink_relative(&self.root, name).is_ok() {
                self.step(|state| {
                    if state.quarantined.remove(name).is_some() {
                        state.disk = state.disk.saturating_sub(*charge);
                    }
                });
            }
        }
    }
    pub fn invalidate(&self, scope: &CacheScope, id: &BlockId) {
        let key = Self::key(scope, id);
        // Exclusive: every insert that began before this has indexed its
        // file or removed it, and none begins until the file is gone.
        let _files = park_on(self.files.write());
        let doomed = self.step(|state| unindex(state, &key));
        let _ = self.settle(doomed.into_iter().collect());
    }
    pub fn invalidate_scope(&self, scope: &CacheScope) {
        let hash = Self::scope_hash(scope);
        let _files = park_on(self.files.write());
        let doomed = self.step(|state| {
            let keys = state
                .entries
                .iter()
                .filter(|(_, e)| e.scope.is_none_or(|s| s == hash))
                .map(|(k, _)| *k)
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| unindex(state, &key))
                .collect::<Vec<_>>()
        });
        let _ = self.settle(doomed);
    }
    pub fn usage(&self) -> (usize, usize, usize) {
        let state = self.state.load();
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
/// Wait for `waiting` on a thread that may block, such as a blocking worker:
/// the thread parks until it is woken.
fn park_on<F: std::future::Future>(waiting: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut waiting = std::pin::pin!(waiting);
    loop {
        if let std::task::Poll::Ready(output) = waiting.as_mut().poll(&mut context) {
            return output;
        }
        // A wake that arrived before this makes `park` return at once.
        std::thread::park();
    }
}
/// What the first step of a disk-backed insert decided.
struct Room {
    /// Whether the block is to be indexed at all.
    index: bool,
    /// Whether it gets a file.
    disk: bool,
    generation: u64,
    /// Files to unlink before writing.
    doomed: Vec<Doomed>,
    /// Bytes of `State::disk` this insert accounts for.
    hold: usize,
}
/// Take `key` out of the index. Its RAM bytes stop being counted at once;
/// if it had a file, the file is returned for the caller to `settle`, and
/// its disk bytes stay counted until then.
fn unindex(state: &mut State, key: &CacheKey) -> Option<Doomed> {
    let entry = state.take(key)?;
    if entry.bytes.is_some() {
        state.memory = state.memory.saturating_sub(entry.size);
    }
    entry.disk.then(|| Doomed {
        name: file_name(key, entry.generation),
        charge: entry.size + 32,
    })
}
/// Record a file that could not be removed. Its bytes are already counted
/// in `State::disk` and stay counted, and it takes up an entry, until
/// `retry_quarantine` manages to unlink it. No new disk write starts while
/// anything is quarantined.
///
/// An artifact and a RAM copy of the same block are one entry between
/// them, so the RAM copy goes. An entry with a file goes only if the file
/// is this very one: another generation's is a different file, which
/// removing the entry would orphan.
fn quarantine(state: &mut State, name: &str, charge: usize) {
    if state.quarantined.contains_key(name) {
        state.disk = state.disk.saturating_sub(charge);
        return;
    }
    if let Some((key, generation)) = parse_name(name.strip_suffix(".tmp").unwrap_or(name))
        && let Some(entry) = state.entries.get(&key)
        && (!entry.disk || (entry.generation == generation && !name.ends_with(".tmp")))
        && let Some(entry) = state.take(&key)
    {
        if entry.bytes.is_some() {
            state.memory = state.memory.saturating_sub(entry.size);
        }
        if entry.disk {
            state.disk = state.disk.saturating_sub(entry.size + 32);
        }
    }
    state.quarantined.insert(name.to_owned(), charge);
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
/// The name of the file an insert of `key` with this generation writes.
fn file_name(key: &CacheKey, generation: u64) -> String {
    format!("{}.{generation}", hex_key(key))
}
/// The key and generation in a file name; only the exact form `file_name`
/// writes is accepted.
fn parse_name(name: &str) -> Option<(CacheKey, u64)> {
    let (key, generation) = name.split_once('.')?;
    let key = parse_key(key)?;
    if generation.is_empty()
        || generation.starts_with('0')
        || !generation.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((key, generation.parse().ok()?))
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
fn read_checked(
    root: &File,
    name: &str,
    key: &CacheKey,
    size: usize,
    max: usize,
) -> Result<Vec<u8>> {
    let mut f = open_relative(root, name, false, false)?;
    let m = f.metadata().map_err(|_| error())?;
    if !m.is_file() || m.len() != size as u64 + 32 || size > max {
        return Err(error());
    }
    let mut sum = [0; 32];
    f.read_exact(&mut sum).map_err(|_| error())?;
    let mut bytes = vec![0; size];
    f.read_exact(&mut bytes).map_err(|_| error())?;
    if checksum(key, &bytes)[..] != sum {
        return Err(error());
    }
    Ok(bytes)
}
fn checksum(key: &CacheKey, bytes: &[u8]) -> CacheKey {
    let mut hash = Sha256::new();
    hash.update(key);
    hash.update(bytes);
    hash.finalize().into()
}
#[cfg(unix)]
fn rename_relative(root: &File, from: &str, to: &str) -> Result<()> {
    use std::os::fd::AsRawFd;
    let source = std::ffi::CString::new(from).map_err(|_| error())?;
    let target = std::ffi::CString::new(to).map_err(|_| error())?;
    if unsafe {
        libc::renameat(
            root.as_raw_fd(),
            source.as_ptr(),
            root.as_raw_fd(),
            target.as_ptr(),
        )
    } < 0
    {
        Err(error())
    } else {
        Ok(())
    }
}
/// Write `name` once: its bytes go to a temporary file of its own, which
/// must not exist, and are published by rename. Nothing else ever writes
/// either name, so there is nothing to clear away first and nobody to wait
/// for.
#[cfg(unix)]
fn write_checked(root: &File, name: &str, key: &CacheKey, bytes: &[u8]) -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let temp = format!("{name}.tmp");
    let path = std::ffi::CString::new(temp.as_str()).map_err(|_| error())?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = unsafe { libc::openat(root.as_raw_fd(), path.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(error());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let outcome = file
        .write_all(&checksum(key, bytes))
        .and_then(|_| file.write_all(bytes))
        .map_err(|_| error())
        .and_then(|_| rename_relative(root, &temp, name));
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
    /// The cache files in `dir`, by key and generation, with their lengths.
    /// A temporary file is a failure: none outlives the insert that made it.
    fn files(dir: &std::path::Path) -> BTreeMap<(CacheKey, u64), usize> {
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!name.ends_with(".tmp"), "temporary file {name} left behind");
            assert!(parse_key(&name).is_none(), "file {name} has no generation");
            if let Some(file) = parse_name(&name) {
                files.insert(file, entry.metadata().unwrap().len() as usize);
            }
        }
        files
    }
    /// Everything the index promises about itself and the directory, for a
    /// cache with nothing in flight.
    fn audit(cache: &LocalCache, dir: &std::path::Path) {
        let state = cache.state.load();
        assert!(state.quarantined.is_empty());
        let indexed: BTreeMap<(CacheKey, u64), usize> = state
            .entries
            .iter()
            .filter(|(_, entry)| entry.disk)
            .map(|(key, entry)| ((*key, entry.generation), entry.size + 32))
            .collect();
        assert_eq!(files(dir), indexed, "the directory and the index disagree");
        assert_eq!(state.disk, indexed.values().sum::<usize>());
        assert!(state.disk <= cache.config.disk_bytes);
        let resident: usize = state
            .entries
            .values()
            .filter(|entry| entry.bytes.is_some())
            .map(|entry| entry.size)
            .sum();
        assert_eq!(state.memory, resident);
        assert!(state.memory <= cache.config.memory_bytes);
        assert!(state.entries.len() <= cache.config.max_entries);
        // Every entry has its seat, and no seat is anyone else's.
        assert_eq!(
            state.seats.iter().flatten().count(),
            state.entries.len(),
            "a seat outlived its entry"
        );
        for (key, entry) in state.entries.iter() {
            let seat = state.seats[entry.slot as usize].expect("an entry without a seat");
            assert_eq!(
                (seat.key, seat.resident, seat.disk),
                (*key, entry.bytes.is_some(), entry.disk)
            );
        }
        // Every stamp slot is held by one live entry or is free.
        let mut free = 0;
        let mut top = cache.stamps.free.load(Ordering::Acquire) as u32;
        while top != 0 {
            free += 1;
            top = cache.stamps.stamp(top - 1).next.load(Ordering::Acquire);
        }
        assert_eq!(
            cache.stamps.created.load(Ordering::Acquire) as usize - free,
            state.entries.len(),
            "a stamp slot was lost or handed out twice"
        );
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
    /// Not a test: the index alone, with no disk I/O, from one thread and
    /// then from 8. A hit is a RAM lookup of one of 1024 resident 4 KiB
    /// blocks. An insert admits a new block to a full RAM budget, so each
    /// one evicts the least recently used block. Every block is its own
    /// allocation, as blocks read from a store are. Times are per operation
    /// as each caller saw it.
    /// `cargo test -p mount-rs-blob-cache --release profile_index -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn profile_index() {
        const RESIDENT: usize = 1024;
        for threads in [1_usize, 8] {
            let dir = tempfile::tempdir().unwrap();
            let cache = LocalCache::new(LocalCacheConfig {
                directory: dir.path().to_path_buf(),
                memory_bytes: RESIDENT * 4096,
                disk_bytes: 0,
                max_entries: 4 * RESIDENT,
                max_blob_bytes: 4096,
            })
            .unwrap();
            let s = scope("index");
            let fingerprint = LocalCache::scope_hash(&s);
            let block = || -> Arc<[u8]> { Arc::from(vec![7_u8; 4096]) };
            let keys: Vec<CacheKey> = (0..RESIDENT)
                .map(|n| {
                    let id = BlockId(format!("resident-{n}"));
                    cache
                        .insert_memory_shared(&s, &id, block(), IntegrityPolicy::Opaque)
                        .unwrap();
                    LocalCache::key_hashed(&fingerprint, &id)
                })
                .collect();
            assert_eq!(cache.usage(), (RESIDENT * 4096, 0, RESIDENT));
            let run = |per_thread: usize, work: &(dyn Fn(usize, usize) + Sync)| {
                let started = std::time::Instant::now();
                std::thread::scope(|scope| {
                    for thread in 0..threads {
                        scope.spawn(move || {
                            for n in 0..per_thread {
                                work(thread, n);
                            }
                        });
                    }
                });
                started.elapsed().as_nanos() as f64 / per_thread as f64
            };
            let hits = 4_000_000 / threads;
            let hit = run(hits, &|thread, n| {
                let key = &keys[(n * 31 + thread * 17) % keys.len()];
                assert!(cache.get_memory_shared_hashed(key).is_some());
            });
            let inserts = 80_000 / threads;
            let new: Vec<Vec<(BlockId, Arc<[u8]>)>> = (0..threads)
                .map(|thread| {
                    (0..inserts)
                        .map(|n| (BlockId(format!("new-{thread}-{n}")), block()))
                        .collect()
                })
                .collect();
            let insert = run(inserts, &|thread, n| {
                let (id, block) = &new[thread][n];
                cache
                    .insert_memory_shared(&s, id, block.clone(), IntegrityPolicy::Opaque)
                    .unwrap();
            });
            assert_eq!(cache.usage(), (RESIDENT * 4096, 0, RESIDENT));
            println!(
                "index x{threads}: hit {hit:.0} ns ({:.0}/s)  insert+evict {insert:.0} ns ({:.0}/s)",
                threads as f64 * 1e9 / hit,
                threads as f64 * 1e9 / insert
            );
        }
    }
    /// Not a test: disk-backed inserts of blocks too large for RAM, from one
    /// thread and then from 8. Every insert after a thread's first pass over
    /// its ids replaces a file (unlink, write, rename). This is the measure
    /// disk inserts are held to: a lock around every disk operation gave way
    /// to write-once file names because one inserter was no slower without
    /// it and eight got about half as much again done.
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
        let check = |cache: &LocalCache| audit(cache, dir.path());
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
    fn racing_memory_admissions_and_invalidations_keep_the_counts_exact() {
        let dir = tempfile::tempdir().unwrap();
        let config = LocalCacheConfig {
            directory: dir.path().to_path_buf(),
            // Room for 5 of the 16 blocks and 9 entries: admissions evict
            // for bytes and for entries, all the time.
            memory_bytes: 5 * 48,
            disk_bytes: 0,
            max_entries: 9,
            max_blob_bytes: 64,
        };
        let cache = LocalCache::new(config).unwrap();
        let scopes = [scope("left"), scope("right")];
        let ids: Vec<BlockId> = (0..8).map(|n| BlockId(format!("block-{n}"))).collect();
        for round in 0..4 {
            std::thread::scope(|threads| {
                for thread in 0..8_usize {
                    let (cache, scopes, ids) = (&cache, &scopes, &ids);
                    threads.spawn(move || {
                        for n in 0..3_000_usize {
                            let turn = n * 7 + thread * 13 + round;
                            let s = &scopes[turn % 2];
                            let id = &ids[(turn / 2) % ids.len()];
                            let block: Arc<[u8]> = Arc::from([id.0.as_bytes()[6]; 48]);
                            match (n + thread) % 8 {
                                0..=3 => cache
                                    .insert_memory_shared(s, id, block, IntegrityPolicy::Opaque)
                                    .unwrap(),
                                4 => cache.invalidate(s, id),
                                5 if n % 64 == 5 => cache.invalidate_scope(s),
                                _ => {
                                    if let Some(read) =
                                        cache.get_memory(s, id, IntegrityPolicy::Opaque)
                                    {
                                        assert_eq!(read[..], block[..]);
                                    }
                                    let (memory, _, entries) = cache.usage();
                                    assert!(memory <= 5 * 48 && entries <= 9);
                                }
                            }
                        }
                    });
                }
            });
            // Two admissions that chose the same block to evict, and both
            // subtracted it, would leave the count below what is resident.
            audit(&cache, dir.path());
        }
    }
    #[test]
    fn concurrent_admissions_and_invalidations_all_take_effect() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(LocalCacheConfig {
            directory: dir.path().to_path_buf(),
            memory_bytes: 8 * 500 * 16,
            disk_bytes: 0,
            max_entries: 8 * 500,
            max_blob_bytes: 16,
        })
        .unwrap();
        let s = scope("all");
        let id = |thread: usize, n: usize| BlockId(format!("{thread}-{n}"));
        // Room for everything, so nothing is evicted: a change published
        // over another, instead of on top of it, shows as a block missing
        // or a block that should have gone.
        std::thread::scope(|threads| {
            for thread in 0..8_usize {
                let (cache, s) = (&cache, &s);
                threads.spawn(move || {
                    for n in 0..500 {
                        cache
                            .insert_memory_shared(
                                s,
                                &id(thread, n),
                                Arc::from([thread as u8; 16]),
                                IntegrityPolicy::Opaque,
                            )
                            .unwrap();
                        if n % 2 == 1 {
                            cache.invalidate(s, &id(thread, n - 1));
                        }
                    }
                });
            }
        });
        for thread in 0..8 {
            for n in 0..500 {
                assert_eq!(
                    cache.get_memory(&s, &id(thread, n), IntegrityPolicy::Opaque),
                    (n % 2 == 1).then(|| vec![thread as u8; 16]),
                    "block {thread}-{n}"
                );
            }
        }
        assert_eq!(cache.usage(), (8 * 250 * 16, 0, 8 * 250));
        audit(&cache, dir.path());
    }
    #[test]
    fn stamp_slots_are_never_held_twice() {
        let slab = Slab::new();
        std::thread::scope(|threads| {
            for thread in 0..8_u64 {
                let slab = &slab;
                threads.spawn(move || {
                    let mut held = Vec::new();
                    for n in 0..20_000_u64 {
                        let slot = slab.take().unwrap();
                        let mine = (thread << 32) | n;
                        slab.stamp(slot).used.store(mine, Ordering::Release);
                        held.push((slot, mine));
                        // Hold a few at a time, so that slots are taken
                        // from, and given back to, a stack others are using.
                        if held.len() > (n % 5) as usize {
                            for (slot, mine) in held.drain(..) {
                                assert_eq!(
                                    slab.stamp(slot).used.load(Ordering::Acquire),
                                    mine,
                                    "a slot was handed to two holders"
                                );
                                slab.give_back(slot);
                            }
                        }
                    }
                    for (slot, _) in held {
                        slab.give_back(slot);
                    }
                });
            }
        });
        // Eight holders of at most five slots each never needed more.
        assert!(slab.created.load(Ordering::Acquire) <= 40);
        // Storage grows by whole chunks and slots keep their place.
        let slots: Vec<u32> = (0..3 * Slab::FIRST as u32 + 7)
            .map(|_| slab.take().unwrap())
            .collect();
        for slot in &slots {
            slab.stamp(*slot)
                .used
                .store(u64::from(*slot), Ordering::Release);
        }
        for slot in &slots {
            assert_eq!(
                slab.stamp(*slot).used.load(Ordering::Acquire),
                u64::from(*slot)
            );
        }
        let mut distinct = slots.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), slots.len());
    }
    #[test]
    fn one_of_racing_advertisers_wins_each_interval() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(cfg(dir.path())).unwrap();
        let s = scope("advert");
        let id = BlockId("id".into());
        cache
            .insert(&s, &id, b"data", IntegrityPolicy::Opaque)
            .unwrap();
        let key = LocalCache::key(&s, &id);
        let hour = std::time::Duration::from_secs(3600);
        let barrier = std::sync::Barrier::new(8);
        let winners = AtomicUsize::new(0);
        std::thread::scope(|threads| {
            for _ in 0..8 {
                threads.spawn(|| {
                    barrier.wait();
                    for _ in 0..1_000 {
                        if cache.should_advertise_key(&key, hour) {
                            winners.fetch_add(1, Ordering::Relaxed);
                        }
                        // Hits in between must not disturb the stamp.
                        assert!(cache.get_memory_hashed(&key).is_some());
                    }
                });
            }
        });
        assert_eq!(winners.load(Ordering::Relaxed), 1);
        assert!(cache.should_advertise_key(&key, std::time::Duration::ZERO));
        assert!(!cache.should_advertise_key(&[0; 32], hour));
    }
    #[test]
    fn an_invalidation_waits_for_an_insert_whose_file_is_not_yet_indexed() {
        use std::sync::atomic::AtomicBool;
        for whole_scope in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let config = LocalCacheConfig {
                directory: dir.path().to_path_buf(),
                memory_bytes: 256,
                disk_bytes: 4096,
                max_entries: 16,
                max_blob_bytes: 64,
            };
            let cache = LocalCache::new(config.clone()).unwrap();
            let s = scope("deleted");
            let id = BlockId("racing".into());
            let key = LocalCache::key(&s, &id);
            let named_for_key = || -> Vec<String> {
                fs::read_dir(dir.path())
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with(&hex_key(&key)))
                    .collect()
            };
            // The insert stops between renaming its file into place and
            // indexing it: the file is on disk and the index does not know.
            let parked = Arc::new(AtomicBool::new(false));
            let release = Arc::new(AtomicBool::new(false));
            let invalidated = AtomicBool::new(false);
            cache
                .written
                .set(Box::new({
                    let (parked, release) = (parked.clone(), release.clone());
                    move |_| {
                        parked.store(true, Ordering::SeqCst);
                        while !release.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                    }
                }))
                .ok()
                .unwrap();
            std::thread::scope(|threads| {
                threads.spawn(|| {
                    cache
                        .insert(&s, &id, &[7; 48], IntegrityPolicy::Opaque)
                        .unwrap()
                });
                while !parked.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                assert_eq!(named_for_key().len(), 1, "the insert's file is in place");
                assert_eq!(cache.usage().2, 0, "and not yet indexed");
                threads.spawn(|| {
                    if whole_scope {
                        cache.invalidate_scope(&s);
                    } else {
                        cache.invalidate(&s, &id);
                    }
                    invalidated.store(true, Ordering::SeqCst);
                });
                // Were the invalidation to return now, a process that
                // stopped here would leave the file for the next start.
                std::thread::sleep(std::time::Duration::from_millis(150));
                let returned = invalidated.load(Ordering::SeqCst);
                // Lookups do not wait behind the waiting invalidation.
                let hit = cache.get_memory(&s, &id, IntegrityPolicy::Opaque);
                release.store(true, Ordering::SeqCst);
                assert!(
                    !returned,
                    "an invalidation returned while the block's file was on disk"
                );
                assert_eq!(hit, None);
            });
            assert!(invalidated.load(Ordering::SeqCst));
            assert_eq!(named_for_key(), Vec::<String>::new());
            assert_eq!(cache.get(&s, &id, IntegrityPolicy::Opaque), None);
            assert_eq!(cache.usage(), (0, 0, 0));
            audit(&cache, dir.path());
            drop(cache);
            let cache = LocalCache::new(config).unwrap();
            assert_eq!(cache.get(&s, &id, IntegrityPolicy::Opaque), None);
            assert_eq!(cache.usage(), (0, 0, 0));
        }
    }
    #[test]
    fn quarantine_leaves_an_entry_whose_file_is_another_generation() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.memory_bytes = 0;
        let cache = LocalCache::new(config).unwrap();
        let s = scope("generations");
        let id = BlockId("id".into());
        cache
            .insert(&s, &id, b"data", IntegrityPolicy::Opaque)
            .unwrap();
        let key = LocalCache::key(&s, &id);
        let generation = cache.state.load().entries[&key].generation;
        // An artifact of an earlier insert of the same block turns up stuck.
        for stale in [
            file_name(&key, generation + 7),
            format!("{}.tmp", file_name(&key, generation)),
        ] {
            cache.step(|state| {
                state.disk += 36;
                quarantine(state, &stale, 36);
            });
        }
        assert_eq!(cache.usage(), (0, 108, 3));
        assert_eq!(
            cache.get_disk(&s, &id, IntegrityPolicy::Opaque),
            Some(b"data".to_vec()),
            "quarantine of another generation's file took the entry with it"
        );
        // The artifacts are not on disk, so the next insert clears them.
        cache
            .insert(
                &s,
                &BlockId("other".into()),
                b"more",
                IntegrityPolicy::Opaque,
            )
            .unwrap();
        assert_eq!(cache.usage(), (0, 72, 2));
        audit(&cache, dir.path());
    }
    #[test]
    fn restart_keeps_the_newest_file_of_a_block_and_adopts_the_old_layout() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.memory_bytes = 0;
        config.disk_bytes = 1024;
        let cache = LocalCache::new(config.clone()).unwrap();
        let s = scope("restart");
        let [kept, old_layout, lost] = ["kept", "old-layout", "lost"].map(|id| BlockId(id.into()));
        for id in [&kept, &old_layout, &lost] {
            cache
                .insert(&s, id, id.0.as_bytes(), IntegrityPolicy::Opaque)
                .unwrap();
        }
        let key = |id: &BlockId| LocalCache::key(&s, id);
        let generation = |id: &BlockId| cache.state.load().entries[&key(id)].generation;
        let name = |id: &BlockId| file_name(&key(id), generation(id));
        let at = |name: &str| dir.path().join(name);
        // An insert that replaced a file and stopped before unlinking the
        // one it replaced: two files of one block, and a temporary file.
        let newest = file_name(&key(&kept), 40);
        fs::copy(at(&name(&kept)), at(&newest)).unwrap();
        fs::write(
            at(&format!("{}.tmp", file_name(&key(&kept), 41))),
            b"partial",
        )
        .unwrap();
        // The older of two is a truncated file: were it the one kept, the
        // block would be unreadable.
        fs::write(at(&name(&kept)), b"short").unwrap();
        // A cache written before files carried generations.
        fs::rename(at(&name(&old_layout)), at(&hex_key(&key(&old_layout)))).unwrap();
        fs::write(at(&format!("{}.tmp", hex_key(&key(&lost)))), b"partial").unwrap();
        // Names that are not the cache's are left alone.
        fs::write(at(&format!("{}.07", hex_key(&key(&lost)))), b"foreign").unwrap();
        fs::remove_file(at(&name(&lost))).unwrap();
        drop(cache);
        let cache = LocalCache::new(config).unwrap();
        for (id, found) in [(&kept, true), (&old_layout, true), (&lost, false)] {
            assert_eq!(
                cache.get(&s, id, IntegrityPolicy::Opaque),
                found.then(|| id.0.as_bytes().to_vec()),
                "{}",
                id.0
            );
        }
        assert_eq!(cache.usage().2, 2);
        audit(&cache, dir.path());
        {
            let state = cache.state.load();
            assert_eq!(state.entries[&key(&kept)].generation, 40);
            // Adopted under a generation above every one seen on disk, and
            // the counter carries on from there.
            assert_eq!(state.entries[&key(&old_layout)].generation, 41);
            assert_eq!(state.generation, 41);
        }
        assert!(at(&format!("{}.07", hex_key(&key(&lost)))).exists());
        cache
            .insert(&s, &lost, b"lost", IntegrityPolicy::Opaque)
            .unwrap();
        assert_eq!(cache.state.load().entries[&key(&lost)].generation, 42);
        audit(&cache, dir.path());
    }
    #[test]
    fn rejects_overflow_prone_public_configurations() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = cfg(dir.path());
        config.max_blob_bytes = usize::MAX;
        assert!(LocalCache::new(config).is_err());
    }
    #[test]
    fn memory_only_admission_writes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(cfg(dir.path())).unwrap();
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
        assert!(files(dir.path()).is_empty());
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
        // The temporary file of a failed write of `a`, which held 36 bytes.
        let temp = format!("{}.tmp", file_name(&LocalCache::key(&s, &a), 9));
        cache.step(|state| {
            state.disk += 36;
            quarantine(state, &temp, 36);
        });
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
        let key = LocalCache::key(&s, &c);
        assert!(
            fs::read_dir(dir.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&hex_key(&key))),
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
        // In the way of the name the next insert will publish under.
        let name = file_name(&LocalCache::key(&s, &id), cache.state.load().generation + 1);
        let path = dir.path().join(&name);
        fs::create_dir(&path).unwrap();
        assert!(
            cache
                .insert(&s, &id, b"data", IntegrityPolicy::Opaque)
                .is_err()
        );
        assert!(
            !dir.path().join(format!("{name}.tmp")).exists(),
            "failed rename must clean owned temp bytes"
        );
        assert!(path.is_dir(), "preexisting directory is preserved");
        assert_eq!(cache.usage(), (0, 0, 0), "the failed write kept its room");
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
fn write_checked(_root: &File, _name: &str, _key: &CacheKey, _bytes: &[u8]) -> Result<()> {
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
