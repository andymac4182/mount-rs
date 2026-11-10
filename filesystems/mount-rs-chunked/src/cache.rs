//! Bounded process-local cache for immutable blocks.
use async_trait::async_trait;
use mount_rs_core::Result;
use mount_rs_core::drain_gate::DrainGate;
use mount_rs_core::storage::{BlockId, BlockStore};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLockReadGuard, RwLockWriteGuard};

/// Byte and entry bounds both apply. Zero for either bound disables caching.
/// Writes are acknowledged by the backing store before entering the cache;
/// flush always reaches that store. This cache does not add durability.
///
/// Each instance is scoped to exactly one backing provider/volume. All users
/// reclaiming its blocks must call this wrapper's delete, not bypass it.
/// Immutable blocks cannot be externally overwritten under an existing ID.
/// Concurrent misses for one block share a fill when the result fits the cache.
/// Cancellation releases fill ownership; failures are retried by waiting callers.
/// Oversized blocks are not retained and therefore cannot share completed data.
/// Cache misses can overlap immutable puts and flushes. Puts run
/// concurrently with each other, as they do against an uncached store; a
/// flush waits for the puts in flight and holds back new ones, so it covers
/// every put acknowledged before it. Deletion and clearing exclude all backing I/O
/// to prevent stale repopulation. Hits do not wait for backing I/O, and they
/// share the cache index: a hit records its recency in the entry itself, so
/// concurrent hits never exclude each other. The LRU order is re-linked lazily
/// by the writers that need it (insertion and eviction).
/// Use one wrapper per volume, shared by Arc.
/// This is RAM caching, not a persistent disk cache.
pub struct CachedBlockStore<B: ?Sized> {
    backing: Arc<B>,
    max_bytes: usize,
    max_entries: usize,
    state: Arc<std::sync::RwLock<State>>,
    io_gate: Arc<DrainGate>,
    fills: Arc<Fills>,
    front: Arc<Front>,
    write_gate: Arc<DrainGate>,
}

impl<B: ?Sized> Clone for CachedBlockStore<B> {
    fn clone(&self) -> Self {
        Self {
            backing: self.backing.clone(),
            max_bytes: self.max_bytes,
            max_entries: self.max_entries,
            state: self.state.clone(),
            io_gate: self.io_gate.clone(),
            fills: self.fills.clone(),
            front: self.front.clone(),
            write_gate: self.write_gate.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub resident_bytes: usize,
    pub resident_entries: usize,
    /// Completed backing gets, including failures; cancelled calls are excluded.
    pub backing_gets_completed: u64,
    pub backing_gets_failed: u64,
    pub backing_read_bytes: u64,
    /// Sum of completed backing-get durations, not wall time or a percentile.
    pub backing_get_nanoseconds: u64,
}

type Recency = BTreeMap<(u64, u64), BlockId>;

const FILL_SLOTS: usize = 256;
const FILL_PROBE: usize = 8;

/// One in-flight backing read that concurrent misses for a block share.
struct Fill {
    id: BlockId,
    /// Held exclusively by whichever miss is reading the backing store; the
    /// others queue here and find the block cached when their turn comes.
    turn: DrainGate,
    /// Misses currently sharing this fill. The one that takes it to zero
    /// frees the slot.
    sharers: std::sync::atomic::AtomicUsize,
}

/// In-flight fills, found without a lock: a fixed table of atomic slots
/// probed from the block's hash. A miss never touches the cache index to
/// register itself, so cold reads do not stall hits on other blocks.
///
/// Sharing a fill is an optimisation, not an invariant. A miss that finds
/// its probe window full reads the backing store unshared, and two misses
/// racing a completing fill may each read once.
struct Fills {
    slots: Box<[arc_swap::ArcSwapOption<Fill>]>,
}

impl Fills {
    fn new() -> Self {
        Self {
            slots: (0..FILL_SLOTS)
                .map(|_| arc_swap::ArcSwapOption::empty())
                .collect(),
        }
    }

    fn window(&self, id: &BlockId) -> impl Iterator<Item = &arc_swap::ArcSwapOption<Fill>> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        id.0.hash(&mut hasher);
        let start = hasher.finish() as usize;
        (0..FILL_PROBE).map(move |step| &self.slots[start.wrapping_add(step) % FILL_SLOTS])
    }

    fn join(&self, id: &BlockId) -> Option<FillRef<'_>> {
        for slot in self.window(id) {
            if let Some(fill) = slot.load_full()
                && fill.id == *id
            {
                return Some(FillRef::new(slot, fill));
            }
        }
        let mut mine: Option<Arc<Fill>> = None;
        for slot in self.window(id) {
            loop {
                let current = slot.load();
                match &*current {
                    Some(fill) if fill.id == *id => {
                        return Some(FillRef::new(slot, Arc::clone(fill)));
                    }
                    Some(_) => break,
                    None => {
                        let fill = Arc::clone(mine.get_or_insert_with(|| {
                            Arc::new(Fill {
                                id: id.clone(),
                                turn: DrainGate::new(),
                                sharers: std::sync::atomic::AtomicUsize::new(0),
                            })
                        }));
                        let previous = slot.compare_and_swap(&current, Some(Arc::clone(&fill)));
                        if previous.is_none() {
                            return Some(FillRef::new(slot, fill));
                        }
                    }
                }
            }
        }
        None
    }

    #[cfg(test)]
    fn sharers(&self, id: &BlockId) -> usize {
        self.window(id)
            .filter_map(|slot| slot.load_full())
            .find(|fill| fill.id == *id)
            .map_or(0, |fill| fill.sharers.load(Ordering::Acquire))
    }
}

/// One miss's share of a fill. The last sharer to leave frees the slot; a
/// cancelled leader leaves it in place for the misses still queued behind it.
struct FillRef<'a> {
    slot: &'a arc_swap::ArcSwapOption<Fill>,
    fill: Arc<Fill>,
}

impl<'a> FillRef<'a> {
    fn new(slot: &'a arc_swap::ArcSwapOption<Fill>, fill: Arc<Fill>) -> Self {
        fill.sharers.fetch_add(1, Ordering::AcqRel);
        Self { slot, fill }
    }
}

impl Drop for FillRef<'_> {
    fn drop(&mut self) {
        // A miss that joins after this count reaches zero keeps its own
        // reference and simply stops being shared with.
        if self.fill.sharers.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let current = self.slot.load();
        if current
            .as_ref()
            .is_some_and(|fill| Arc::ptr_eq(fill, &self.fill))
        {
            self.slot.compare_and_swap(&current, None);
        }
    }
}

/// What a hit needs from a cached block. Shared between the index and
/// the lock-free table in front of it.
struct Hot {
    id: BlockId,
    bytes: Arc<Vec<u8>>,
    /// Epoch of the latest hit. Written at most once per epoch, so repeated
    /// hits on a hot block share its cache line without bouncing it.
    used: AtomicU64,
    /// Hits since this entry was cached; folded into the totals on removal.
    hits: AtomicU64,
}

impl Hot {
    fn touch(&self, epoch: u64, count_hit: bool) {
        if self.used.load(Ordering::Relaxed) != epoch {
            self.used.store(epoch, Ordering::Relaxed);
        }
        if count_hit {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct Entry {
    hot: Arc<Hot>,
    /// Key under which `recency` currently links this entry. Writers only.
    linked: (u64, u64),
}

const FRONT_PROBE: usize = 4;

/// Cached blocks, findable without the index lock: a fixed table of atomic
/// slots probed from the block id's hash. A hit loads one slot, compares the
/// id and copies out of the block it guards; it takes no lock and no
/// reference count, so hits on different cores share nothing they write.
///
/// The index stays the authority. Its writers, who hold the index lock,
/// publish each entry here and retract it before releasing its payload. An
/// entry whose probe window is full is simply not published; its hits use
/// the index.
struct Front {
    slots: Box<[arc_swap::ArcSwapOption<Hot>]>,
    /// The index's current sequence, for hits to stamp recency with.
    epoch: AtomicU64,
}

impl Default for Front {
    fn default() -> Self {
        Self::new(16)
    }
}

impl Front {
    fn new(max_entries: usize) -> Self {
        // Four slots per entry keep nearly every probe window short of full.
        let slots = max_entries
            .saturating_mul(4)
            .clamp(64, 1 << 20)
            .next_power_of_two();
        Self {
            slots: (0..slots)
                .map(|_| arc_swap::ArcSwapOption::empty())
                .collect(),
            epoch: AtomicU64::new(0),
        }
    }

    fn window(&self, id: &BlockId) -> impl Iterator<Item = &arc_swap::ArcSwapOption<Hot>> {
        // Ids are digests or counters: their tail varies the most.
        let bytes = id.0.as_bytes();
        let tail = &bytes[bytes.len().saturating_sub(16)..];
        let hash = tail.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
        let start = (hash ^ (hash >> 29)) as usize;
        let mask = self.slots.len() - 1;
        (0..FRONT_PROBE).map(move |step| &self.slots[start.wrapping_add(step) & mask])
    }

    /// Runs `read` on the cached block, if it is published here.
    #[inline]
    fn hit<R>(&self, id: &BlockId, read: impl FnOnce(&[u8]) -> R) -> Option<R> {
        for slot in self.window(id) {
            let hot = slot.load();
            if let Some(hot) = hot.as_deref()
                && hot.id == *id
            {
                hot.touch(self.epoch.load(Ordering::Relaxed) | 1, true);
                return Some(read(&hot.bytes));
            }
        }
        None
    }

    /// Index writers only.
    fn publish(&self, hot: &Arc<Hot>) {
        if let Some(slot) = self.window(&hot.id).find(|slot| slot.load().is_none()) {
            slot.store(Some(Arc::clone(hot)));
        }
    }

    /// Index writers only.
    fn retract(&self, hot: &Arc<Hot>) {
        for slot in self.window(&hot.id) {
            if slot
                .load()
                .as_ref()
                .is_some_and(|held| Arc::ptr_eq(held, hot))
            {
                slot.store(None);
            }
        }
    }
}

#[derive(Default)]
struct State {
    entries: BTreeMap<BlockId, Entry>,
    /// Eviction order by (epoch of last known use, link sequence). A key may
    /// trail its entry's `used` epoch; eviction re-links such entries before
    /// choosing a victim, so hits never write to this map.
    recency: Recency,
    front: Arc<Front>,
    /// Advances by two on each insertion, so the odd value a hit records is
    /// always newer than every link made before it. Hits only read it:
    /// nothing every hit writes is shared between blocks.
    sequence: AtomicU64,
    misses: AtomicU64,
    stats: BlockCacheStats,
}
impl State {
    fn retire(&mut self, entry: Entry) -> Arc<Vec<u8>> {
        self.stats.hits = self
            .stats
            .hits
            .saturating_add(entry.hot.hits.load(Ordering::Relaxed));
        self.front.retract(&entry.hot);
        self.stats.resident_bytes -= entry.hot.bytes.len();
        self.stats.resident_entries -= 1;
        // The table's reference is gone; a hit that still guards the block
        // keeps it alive until it has copied its range.
        match Arc::try_unwrap(entry.hot) {
            Ok(hot) => hot.bytes,
            Err(hot) => Arc::clone(&hot.bytes),
        }
    }
    fn remove(&mut self, id: &BlockId) -> Option<Arc<Vec<u8>>> {
        let entry = self.entries.remove(id)?;
        self.recency.remove(&entry.linked);
        Some(self.retire(entry))
    }
    /// Empties the cache, returning what the caller should free once it
    /// has released the index lock.
    fn clear(&mut self) -> (Vec<Arc<Vec<u8>>>, Recency) {
        let payloads = std::mem::take(&mut self.entries)
            .into_values()
            .map(|entry| self.retire(entry))
            .collect();
        *self.sequence.get_mut() = 0;
        self.front.epoch.store(0, Ordering::Relaxed);
        (payloads, std::mem::take(&mut self.recency))
    }
    fn next_sequence(&mut self) -> u64 {
        let sequence = self.sequence.get_mut();
        *sequence += 2;
        self.front.epoch.store(*sequence, Ordering::Relaxed);
        *sequence
    }
    fn stats(&self) -> BlockCacheStats {
        let resident_hits: u64 = self
            .entries
            .values()
            .map(|entry| entry.hot.hits.load(Ordering::Relaxed))
            .sum();
        BlockCacheStats {
            hits: self.stats.hits.saturating_add(resident_hits),
            misses: self.misses.load(Ordering::Relaxed),
            ..self.stats
        }
    }
    /// Shared-lock lookup. Recency is recorded in the entry, not the index.
    fn get(&self, id: &BlockId, count_hit: bool) -> Option<Arc<Vec<u8>>> {
        let entry = self.entries.get(id)?;
        entry
            .hot
            .touch(self.sequence.load(Ordering::Relaxed) | 1, count_hit);
        Some(entry.hot.bytes.clone())
    }
    /// Removes the least recently used entry, first re-linking entries that
    /// were hit in a later epoch than the one they are filed under.
    fn evict_oldest(&mut self) -> Option<Arc<Vec<u8>>> {
        loop {
            let (key, id) = self.recency.pop_first()?;
            let used = self.entries.get(&id)?.hot.used.load(Ordering::Relaxed);
            if used > key.0 {
                let linked = (used, self.next_sequence());
                self.entries.get_mut(&id)?.linked = linked;
                self.recency.insert(linked, id);
                continue;
            }
            let entry = self.entries.remove(&id)?;
            return Some(self.retire(entry));
        }
    }
    fn insert(
        &mut self,
        id: BlockId,
        bytes: Arc<Vec<u8>>,
        max_bytes: usize,
        max_entries: usize,
    ) -> Vec<Arc<Vec<u8>>> {
        if max_entries == 0 || max_bytes == 0 || bytes.len() > max_bytes {
            return vec![bytes];
        }
        // Sequences only order entries, so restarting them needs an empty cache.
        let mut retired = Vec::new();
        if *self.sequence.get_mut() >= u64::MAX / 2 {
            retired = self.clear().0;
        }
        let sequence = self.next_sequence();
        retired.extend(self.remove(&id));
        while self.stats.resident_entries >= max_entries
            || self.stats.resident_bytes > max_bytes - bytes.len()
        {
            let Some(oldest) = self.evict_oldest() else {
                break;
            };
            retired.push(oldest);
            self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
        self.stats.resident_bytes += bytes.len();
        self.stats.resident_entries += 1;
        let linked = (sequence, sequence);
        self.recency.insert(linked, id.clone());
        let hot = Arc::new(Hot {
            id: id.clone(),
            bytes,
            used: AtomicU64::new(sequence),
            hits: AtomicU64::new(0),
        });
        self.front.publish(&hot);
        self.entries.insert(id, Entry { hot, linked });
        retired
    }
}
impl<B: BlockStore> CachedBlockStore<B> {
    pub fn new(backing: B, max_bytes: usize, max_entries: usize) -> Self {
        Self::from_shared(Arc::new(backing), max_bytes, max_entries)
    }
}
impl<B: BlockStore + ?Sized> CachedBlockStore<B> {
    pub fn from_shared(backing: Arc<B>, max_bytes: usize, max_entries: usize) -> Self {
        let front = Arc::new(Front::new(max_entries));
        Self {
            backing,
            max_bytes,
            max_entries,
            state: Arc::new(std::sync::RwLock::new(State {
                front: Arc::clone(&front),
                ..State::default()
            })),
            front,
            io_gate: Arc::new(DrainGate::new()),
            fills: Arc::new(Fills::new()),
            write_gate: Arc::new(DrainGate::new()),
        }
    }
    fn prepare_entry(&self, bytes: &[u8]) -> Option<Arc<Vec<u8>>> {
        (self.max_entries != 0 && self.max_bytes != 0 && bytes.len() <= self.max_bytes)
            .then(|| Arc::new(bytes.to_vec()))
    }
    pub async fn stats(&self) -> BlockCacheStats {
        self.read_state().stats()
    }
    // The index lock is never held across an await: every critical section
    // is a bounded in-memory update, and hits only share it.
    fn read_state(&self) -> RwLockReadGuard<'_, State> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn write_state(&self) -> RwLockWriteGuard<'_, State> {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Runs `read` on a cached block. A published block is read without
    /// the index lock; the rest, and every miss, go through the index.
    #[inline]
    fn lookup<R>(&self, id: &BlockId, read: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let mut read = Some(read);
        if let Some(result) = self.front.hit(id, |bytes| (read.take().unwrap())(bytes)) {
            return Some(result);
        }
        let cached = {
            let state = self.read_state();
            let cached = state.get(id, true);
            if cached.is_none() {
                state.misses.fetch_add(1, Ordering::Relaxed);
            }
            cached
        };
        cached.map(|bytes| (read.take().unwrap())(&bytes))
    }
    /// Miss path: fetch from the backing store, sharing one fill per block.
    async fn fill(&self, id: &BlockId) -> Result<Vec<u8>> {
        let _io = self.io_gate.read().await;
        // Share one fill per block. Cancellation releases the turn so a
        // waiter can retry the provider.
        let fill = if self.max_bytes != 0 && self.max_entries != 0 {
            self.fills.join(id)
        } else {
            None
        };
        let _turn = match &fill {
            Some(fill) => Some(fill.fill.turn.write().await),
            None => None,
        };
        // Another miss or put may have populated this entry while we waited.
        let cached = self.read_state().get(id, false);
        if let Some(bytes) = cached {
            return Ok(bytes.as_ref().clone());
        }
        let started = std::time::Instant::now();
        let result = self.backing.get(id).await;
        let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let cached = result
            .as_ref()
            .ok()
            .and_then(|bytes| self.prepare_entry(bytes));
        let mut state = self.write_state();
        state.stats.backing_gets_completed = state.stats.backing_gets_completed.saturating_add(1);
        state.stats.backing_get_nanoseconds =
            state.stats.backing_get_nanoseconds.saturating_add(elapsed);
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(error) => {
                state.stats.backing_gets_failed = state.stats.backing_gets_failed.saturating_add(1);
                return Err(error);
            }
        };
        state.stats.backing_read_bytes = state
            .stats
            .backing_read_bytes
            .saturating_add(bytes.len() as u64);
        let retired =
            cached.map(|cached| state.insert(id.clone(), cached, self.max_bytes, self.max_entries));
        drop(state);
        // Evicted payload destruction must not serialize unrelated hits.
        drop(retired);
        Ok(bytes)
    }
    /// Drops only cached copies; authoritative backing data is untouched.
    pub async fn clear(&self) {
        let _io = self.io_gate.write().await;
        let retired = self.write_state().clear();
        // Free potentially large buffers and tree nodes after releasing the
        // state mutex. Keep I/O exclusion until invalidation is complete.
        drop(retired);
    }
}
#[async_trait]
impl<B: BlockStore + ?Sized> BlockStore for CachedBlockStore<B> {
    fn durable(&self) -> bool {
        self.backing.durable()
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let _io = self.io_gate.read().await;
        let _write = self.write_gate.read().await;
        let id = self.backing.put(bytes).await?;
        // The durability-ordering gate protects backing put/flush, not cache
        // allocation or publication. Deletion remains excluded by the I/O gate.
        drop(_write);
        // Allocate/copy before taking the shared cache-state lock.
        if let Some(cached) = self.prepare_entry(bytes) {
            let retired =
                self.write_state()
                    .insert(id.clone(), cached, self.max_bytes, self.max_entries);
            drop(retired);
        }
        Ok(id)
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        // Clone only the shared pointer under the cache lock. Potentially large
        // caller-owned byte copies must not serialize other cache hits.
        if let Some(bytes) = self.lookup(id, <[u8]>::to_vec) {
            return Ok(bytes);
        }
        self.fill(id).await
    }
    async fn read_into(&self, id: &BlockId, offset: usize, buffer: &mut [u8]) -> Result<()> {
        // A hit copies only the requested range out of the shared block.
        if let Some(copied) = self.lookup(id, |bytes| {
            mount_rs_core::storage::copy_block_range(bytes, offset, buffer)
        }) {
            return copied;
        }
        let bytes = self.fill(id).await?;
        mount_rs_core::storage::copy_block_range(&bytes, offset, buffer)
    }
    async fn flush(&self) -> Result<()> {
        let _io = self.io_gate.read().await;
        let _write = self.write_gate.write().await;
        self.backing.flush().await
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        let _io = self.io_gate.write().await;
        // No miss/put can repopulate while this operation owns the I/O gate.
        // Evict before awaiting, including for cancellation/unknown outcomes.
        let retired = self.write_state().remove(id);
        drop(retired);
        self.backing.delete(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::{ErrorCode, FsError};
    use mount_rs_memory::MemoryBlockStore;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct StalledIo {
        inner: MemoryBlockStore,
        stall: Arc<AtomicBool>,
        entered: Arc<tokio::sync::Notify>,
        resume: Arc<tokio::sync::Notify>,
    }

    #[tokio::test]
    async fn ranged_reads_copy_only_the_requested_bytes_on_hit_and_miss() {
        let backing = MemoryBlockStore::new();
        let id = backing.put(b"0123456789").await.unwrap();
        let cache = CachedBlockStore::new(backing, 64, 4);
        let mut bytes = [0; 4];
        cache.read_into(&id, 3, &mut bytes).await.unwrap();
        assert_eq!(&bytes, b"3456");
        let stats = cache.stats().await;
        assert_eq!(
            (stats.misses, stats.hits, stats.resident_entries),
            (1, 0, 1)
        );
        cache.read_into(&id, 6, &mut bytes).await.unwrap();
        assert_eq!(&bytes, b"6789");
        let stats = cache.stats().await;
        assert_eq!((stats.misses, stats.hits), (1, 1));
        assert_eq!(stats.backing_gets_completed, 1);
        for offset in [7, usize::MAX] {
            assert_eq!(
                cache
                    .read_into(&id, offset, &mut bytes)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Eio
            );
        }
        cache.read_into(&id, 10, &mut []).await.unwrap();
    }

    #[tokio::test]
    async fn hits_bypass_the_index_until_the_block_is_evicted_or_deleted() {
        let cache = CachedBlockStore::new(MemoryBlockStore::new(), 1024, 2);
        let first = cache.put(b"first").await.unwrap();
        let second = cache.put(b"second").await.unwrap();
        // Published blocks are read from the table, with no index access.
        assert_eq!(cache.front.hit(&first, <[u8]>::len), Some(5));
        let mut bytes = [0; 3];
        cache.read_into(&second, 1, &mut bytes).await.unwrap();
        assert_eq!(&bytes, b"eco");
        assert_eq!(cache.stats().await.hits, 2);

        // Eviction and deletion retract a block before its payload is freed.
        let payload = Arc::downgrade(&cache.read_state().entries[&second].hot.bytes);
        let third = cache.put(b"third").await.unwrap();
        assert!(cache.front.hit(&first, <[u8]>::len).is_none());
        assert_eq!(cache.front.hit(&third, <[u8]>::len), Some(5));
        cache.delete(&second).await.unwrap();
        assert!(cache.front.hit(&second, <[u8]>::len).is_none());
        assert!(payload.upgrade().is_none());
        // The evicted block is still readable, through a miss.
        cache.read_into(&first, 0, &mut bytes).await.unwrap();
        assert_eq!(&bytes, b"fir");
        let stats = cache.stats().await;
        assert_eq!((stats.misses, stats.resident_entries), (1, 2));
    }

    #[tokio::test]
    async fn eviction_and_replacement_return_payloads_for_unlocked_reclamation() {
        let backing = mount_rs_memory::MemoryBlockStore::new();
        let first = backing.put(b"first").await.unwrap();
        let second = backing.put(b"second").await.unwrap();
        let mut state = State::default();
        let bytes = Arc::new(vec![1; 8]);
        let weak = Arc::downgrade(&bytes);
        assert!(state.insert(first.clone(), bytes, 16, 1).is_empty());
        let retired = state.insert(second.clone(), Arc::new(vec![2; 8]), 16, 1);
        assert_eq!(state.stats.resident_entries, 1);
        assert_eq!(state.stats.resident_bytes, 8);
        assert_eq!(state.stats.evictions, 1);
        assert!(weak.upgrade().is_some());
        drop(retired);
        assert!(weak.upgrade().is_none());
        let old = Arc::downgrade(&state.entries[&second].hot.bytes);
        let retired = state.insert(second.clone(), Arc::new(vec![3; 4]), 16, 1);
        assert_eq!(state.stats.resident_bytes, 4);
        assert_eq!(state.stats.evictions, 1);
        assert!(old.upgrade().is_some());
        drop(retired);
        assert!(old.upgrade().is_none());
        let old = Arc::downgrade(&state.entries[&second].hot.bytes);
        let retired = state.remove(&second);
        assert_eq!(state.stats.resident_entries, 0);
        assert_eq!(state.stats.resident_bytes, 0);
        assert!(old.upgrade().is_some());
        drop(retired);
        assert!(old.upgrade().is_none());
    }

    #[tokio::test]
    async fn backing_metrics_distinguish_hits_successes_and_failures() {
        let backing = Arc::new(MemoryBlockStore::new());
        let id = backing.put(b"cold bytes").await.unwrap();
        let missing = backing.put(b"deleted").await.unwrap();
        backing.delete(&missing).await.unwrap();
        let cache = CachedBlockStore::from_shared(backing, 1024, 10);
        assert_eq!(cache.get(&id).await.unwrap(), b"cold bytes");
        let cold = cache.stats().await;
        assert_eq!(cold.backing_gets_completed, 1);
        assert_eq!(cold.backing_read_bytes, 10);
        assert_eq!(cold.backing_gets_failed, 0);
        cache.get(&id).await.unwrap();
        let warm = cache.stats().await;
        assert_eq!(warm.backing_gets_completed, cold.backing_gets_completed);
        assert_eq!(warm.backing_get_nanoseconds, cold.backing_get_nanoseconds);
        assert!(cache.get(&missing).await.is_err());
        let failed = cache.stats().await;
        assert_eq!(failed.backing_gets_completed, 2);
        assert_eq!(failed.backing_gets_failed, 1);
        assert_eq!(failed.backing_read_bytes, 10);
        assert!(failed.backing_get_nanoseconds >= cold.backing_get_nanoseconds);
    }
    #[tokio::test]
    async fn concurrent_misses_share_fill_and_cancelled_leader_releases_waiters() {
        for cancel_leader in [false, true] {
            let backing = MemoryBlockStore::new();
            let id = backing.put(b"shared").await.unwrap();
            let stall = Arc::new(AtomicBool::new(true));
            let entered = Arc::new(tokio::sync::Notify::new());
            let resume = Arc::new(tokio::sync::Notify::new());
            let cache = CachedBlockStore::new(
                StalledIo {
                    inner: backing,
                    stall: stall.clone(),
                    entered: entered.clone(),
                    resume: resume.clone(),
                },
                1024,
                16,
            );
            let worker = cache.clone();
            let key = id.clone();
            let leader = tokio::spawn(async move { worker.get(&key).await });
            entered.notified().await;
            let mut waiters = Vec::new();
            for _ in 0..20 {
                let worker = cache.clone();
                let key = id.clone();
                waiters.push(tokio::spawn(async move { worker.get(&key).await }));
            }
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while cache.stats().await.misses != 21 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            stall.store(false, Ordering::SeqCst);
            if cancel_leader {
                leader.abort();
                assert!(leader.await.unwrap_err().is_cancelled());
            } else {
                resume.notify_one();
                assert_eq!(leader.await.unwrap().unwrap(), b"shared");
            }
            for waiter in waiters {
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap(),
                    b"shared"
                );
            }
            assert_eq!(cache.stats().await.backing_gets_completed, 1);
            assert_eq!(cache.stats().await.backing_read_bytes, 6);
        }
    }

    #[tokio::test]
    async fn different_blocks_fill_concurrently() {
        let backing = MemoryBlockStore::new();
        let a = backing.put(b"a").await.unwrap();
        let b = backing.put(b"b").await.unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let cache = CachedBlockStore::new(
            StalledIo {
                inner: backing,
                stall: Arc::new(AtomicBool::new(true)),
                entered: entered.clone(),
                resume: resume.clone(),
            },
            1024,
            16,
        );
        let worker = cache.clone();
        let first = tokio::spawn(async move { worker.get(&a).await });
        entered.notified().await;
        let worker = cache.clone();
        let second = tokio::spawn(async move { worker.get(&b).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        resume.notify_waiters();
        assert_eq!(first.await.unwrap().unwrap(), b"a");
        assert_eq!(second.await.unwrap().unwrap(), b"b");
        assert_eq!(cache.stats().await.backing_gets_completed, 2);
    }

    impl StalledIo {
        async fn wait(&self) {
            if self.stall.load(Ordering::SeqCst) {
                self.entered.notify_one();
                self.resume.notified().await;
            }
        }
    }
    #[async_trait]
    impl BlockStore for StalledIo {
        fn durable(&self) -> bool {
            false
        }
        async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
            self.wait().await;
            self.inner.put(bytes).await
        }
        async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
            self.wait().await;
            self.inner.get(id).await
        }
        async fn flush(&self) -> Result<()> {
            self.wait().await;
            Ok(())
        }
        async fn delete(&self, id: &BlockId) -> Result<()> {
            self.wait().await;
            self.inner.delete(id).await
        }
    }

    #[tokio::test]
    async fn noatime_volume_serves_cached_handle_reads_during_blocked_sibling_write() {
        use crate::{AtimePolicy, ChunkedFs, ChunkedOptions};
        use mount_rs_core::FsDriver;
        use mount_rs_memory::MemoryMetadataStore;
        use std::time::Duration;

        let stall = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let cache = CachedBlockStore::new(
            StalledIo {
                inner: MemoryBlockStore::new(),
                stall: stall.clone(),
                entered: entered.clone(),
                resume: resume.clone(),
            },
            8192,
            2,
        );
        let fs = ChunkedFs::open(
            MemoryMetadataStore::new(),
            cache.clone(),
            ChunkedOptions::fixed("blocked-put", 4096)
                .unwrap()
                .with_atime_policy(AtimePolicy::NoAtime),
        )
        .await
        .unwrap();
        let sample = fs.open("/sample", "w+", 0o600).await.unwrap();
        let sibling = fs.open("/writing", "w", 0o600).await.unwrap();
        let expected = [7; 4096];
        sample.write(&expected, Some(0)).await.unwrap();
        stall.store(true, Ordering::SeqCst);
        let task = tokio::spawn(async move {
            assert_eq!(sibling.write(&[9; 4096], Some(0)).await.unwrap(), 4096);
            sibling.close().await.unwrap();
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .expect("write must reach the backing store");
        tokio::time::timeout(Duration::from_secs(2), async {
            assert_eq!(fs.stat("/sample").await.unwrap().size, 4096);
            let mut data = [0; 4096];
            assert_eq!(sample.read(&mut data, Some(0)).await.unwrap(), 4096);
            assert_eq!(data, expected);
        })
        .await
        .expect("stat and cached handle read must complete while the write is stalled");
        assert_eq!(cache.stats().await.backing_gets_completed, 0);
        assert!(!task.is_finished());
        stall.store(false, Ordering::SeqCst);
        resume.notify_one();
        task.await.unwrap();
        sample.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cold_reads_do_not_wait_for_backing_put_or_flush() {
        use std::time::Duration;
        for flush in [false, true] {
            let backing = MemoryBlockStore::new();
            let cold = backing.put(b"cold").await.unwrap();
            let stall = Arc::new(AtomicBool::new(true));
            let entered = Arc::new(tokio::sync::Notify::new());
            let resume = Arc::new(tokio::sync::Notify::new());
            let cache = CachedBlockStore::new(
                StalledIo {
                    inner: backing,
                    stall: stall.clone(),
                    entered: entered.clone(),
                    resume: resume.clone(),
                },
                1024,
                16,
            );
            let worker = cache.clone();
            let task = tokio::spawn(async move {
                if flush {
                    worker.flush().await.unwrap();
                } else {
                    worker.put(b"new").await.unwrap();
                }
            });
            tokio::time::timeout(Duration::from_secs(1), entered.notified())
                .await
                .unwrap();
            // The existing operation remains blocked on resume; subsequent
            // backing gets can proceed, so only wrapper contention is tested.
            stall.store(false, Ordering::SeqCst);
            let bytes = tokio::time::timeout(Duration::from_millis(100), cache.get(&cold))
                .await
                .expect("cold read must not wait for unrelated write I/O")
                .unwrap();
            assert_eq!(bytes, b"cold");
            resume.notify_one();
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn cache_hits_do_not_wait_for_backing_put_flush_or_miss() {
        use std::time::Duration;
        let backing = MemoryBlockStore::new();
        let cold_id = backing.put(b"cold").await.unwrap();
        let stall = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let cache = CachedBlockStore::new(
            StalledIo {
                inner: backing,
                stall: stall.clone(),
                entered: entered.clone(),
                resume: resume.clone(),
            },
            1024,
            16,
        );
        let hot_id = cache.put(b"hot").await.unwrap();
        stall.store(true, Ordering::SeqCst);
        for operation in 0..3 {
            let worker = cache.clone();
            let cold = cold_id.clone();
            let task = tokio::spawn(async move {
                match operation {
                    0 => {
                        worker.put(b"new").await.unwrap();
                    }
                    1 => {
                        worker.flush().await.unwrap();
                    }
                    _ => {
                        assert_eq!(worker.get(&cold).await.unwrap(), b"cold");
                    }
                }
            });
            tokio::time::timeout(Duration::from_secs(1), entered.notified())
                .await
                .unwrap();
            let bytes = tokio::time::timeout(Duration::from_millis(100), cache.get(&hot_id))
                .await
                .expect("warm hit must not wait for backing I/O")
                .unwrap();
            assert_eq!(bytes, b"hot");
            resume.notify_one();
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn puts_overlap_each_other_and_flush_waits_for_them() {
        use std::time::Duration;
        let stall = Arc::new(AtomicBool::new(true));
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let cache = CachedBlockStore::new(
            StalledIo {
                inner: MemoryBlockStore::new(),
                stall: stall.clone(),
                entered: entered.clone(),
                resume: resume.clone(),
            },
            1024,
            16,
        );
        let mut puts = Vec::new();
        for bytes in [b"one", b"two"] {
            let worker = cache.clone();
            puts.push(tokio::spawn(async move { worker.put(bytes).await }));
            // Each put reaches the backing store while the other is in flight.
            tokio::time::timeout(Duration::from_secs(1), entered.notified())
                .await
                .expect("puts must not serialize behind each other");
        }
        let worker = cache.clone();
        let flush = tokio::spawn(async move { worker.flush().await });
        // A flush must cover the puts already in flight, so it waits for them.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), entered.notified())
                .await
                .is_err()
        );
        resume.notify_one();
        resume.notify_one();
        for put in puts {
            put.await.unwrap().unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("flush runs once the puts finish");
        resume.notify_one();
        flush.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn miss_then_delete_cannot_repopulate_a_deleted_entry() {
        use std::time::Duration;
        let backing = MemoryBlockStore::new();
        let id = backing.put(b"cold").await.unwrap();
        let stall = Arc::new(AtomicBool::new(true));
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let cache = CachedBlockStore::new(
            StalledIo {
                inner: backing,
                stall: stall.clone(),
                entered: entered.clone(),
                resume: resume.clone(),
            },
            1024,
            16,
        );
        let reader = cache.clone();
        let read_id = id.clone();
        let read = tokio::spawn(async move { reader.get(&read_id).await });
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        let deleter = cache.clone();
        let delete_id = id.clone();
        let delete = tokio::spawn(async move { deleter.delete(&delete_id).await });
        stall.store(false, Ordering::SeqCst);
        resume.notify_one();
        assert_eq!(read.await.unwrap().unwrap(), b"cold");
        delete.await.unwrap().unwrap();
        assert_eq!(cache.get(&id).await.unwrap_err().code, ErrorCode::Enoent);
        assert_eq!(cache.stats().await.resident_entries, 0);
    }

    #[tokio::test]
    async fn invalidation_drains_shared_fill_waiters_before_returning() {
        for delete in [false, true] {
            let backing = MemoryBlockStore::new();
            let id = backing.put(b"cold").await.unwrap();
            let stall = Arc::new(AtomicBool::new(true));
            let entered = Arc::new(tokio::sync::Notify::new());
            let resume = Arc::new(tokio::sync::Notify::new());
            let cache = CachedBlockStore::new(
                StalledIo {
                    inner: backing,
                    stall: stall.clone(),
                    entered: entered.clone(),
                    resume: resume.clone(),
                },
                1024,
                16,
            );
            let mut readers = Vec::new();
            for _ in 0..20 {
                let reader = cache.clone();
                let key = id.clone();
                readers.push(tokio::spawn(async move { reader.get(&key).await }));
            }
            entered.notified().await;
            // All readers must own the I/O read guard and the shared fill
            // reference before invalidation queues its exclusive guard.
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if cache.fills.sharers(&id) == 20 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let invalidator = cache.clone();
            let key = id.clone();
            let invalidation = tokio::spawn(async move {
                if delete {
                    invalidator.delete(&key).await.unwrap();
                } else {
                    invalidator.clear().await;
                }
            });
            tokio::task::yield_now().await;
            assert!(!invalidation.is_finished());
            stall.store(false, Ordering::SeqCst);
            resume.notify_one();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                for reader in readers {
                    assert_eq!(reader.await.unwrap().unwrap(), b"cold");
                }
                invalidation.await.unwrap();
            })
            .await
            .unwrap();
            assert_eq!(cache.stats().await.resident_entries, 0);
            assert_eq!(cache.stats().await.backing_gets_completed, 1);
            if delete {
                assert_eq!(cache.get(&id).await.unwrap_err().code, ErrorCode::Enoent);
            } else {
                assert_eq!(cache.get(&id).await.unwrap(), b"cold");
                assert_eq!(cache.stats().await.backing_gets_completed, 2);
            }
        }
    }

    struct Faults {
        inner: MemoryBlockStore,
        fail: Arc<AtomicBool>,
    }
    #[async_trait]
    impl BlockStore for Faults {
        fn durable(&self) -> bool {
            false
        }
        async fn put(&self, b: &[u8]) -> Result<BlockId> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(FsError::new(ErrorCode::Eio));
            }
            self.inner.put(b).await
        }
        async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
            self.inner.get(id).await
        }
        async fn flush(&self) -> Result<()> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(FsError::new(ErrorCode::Eio));
            }
            Ok(())
        }
        async fn delete(&self, id: &BlockId) -> Result<()> {
            self.inner.delete(id).await?;
            if self.fail.load(Ordering::SeqCst) {
                return Err(FsError::new(ErrorCode::Eio));
            }
            Ok(())
        }
    }
    #[tokio::test]
    async fn writes_reach_backing_before_hits_and_flush_failure_propagates() {
        let backing = MemoryBlockStore::new();
        let fail = Arc::new(AtomicBool::new(false));
        let cache = CachedBlockStore::new(
            Faults {
                inner: backing.clone(),
                fail: fail.clone(),
            },
            10,
            10,
        );
        let id = cache.put(b"abc").await.unwrap();
        assert_eq!(backing.get(&id).await.unwrap(), b"abc");
        assert_eq!(cache.get(&id).await.unwrap(), b"abc");
        assert_eq!(cache.stats().await.hits, 1);
        fail.store(true, Ordering::SeqCst);
        assert!(cache.put(b"def").await.is_err());
        assert_eq!(cache.stats().await.resident_entries, 1);
        assert!(cache.flush().await.is_err());
        assert!(cache.delete(&id).await.is_err());
        assert!(cache.get(&id).await.is_err());
        assert_eq!(cache.stats().await.resident_entries, 0);
    }
    #[tokio::test]
    async fn lru_bounds_oversize_empty_and_clear() {
        let backing = MemoryBlockStore::new();
        let cache = CachedBlockStore::new(backing.clone(), 6, 2);
        let a = cache.put(b"aaa").await.unwrap();
        let b = cache.put(b"bbb").await.unwrap();
        cache.get(&a).await.unwrap();
        let c = cache.put(b"ccc").await.unwrap();
        assert_eq!(cache.stats().await.evictions, 1);
        cache.get(&a).await.unwrap();
        cache.get(&c).await.unwrap();
        assert_eq!(cache.stats().await.misses, 0);
        cache.get(&b).await.unwrap();
        assert_eq!(cache.stats().await.misses, 1);
        let large = cache.put(b"oversized").await.unwrap();
        cache.get(&large).await.unwrap();
        assert_eq!(cache.stats().await.resident_bytes, 6);
        cache.put(b"").await.unwrap();
        assert_eq!(cache.stats().await.resident_entries, 2);
        cache.clear().await;
        assert_eq!(cache.stats().await.resident_bytes, 0);
        assert_eq!(backing.get(&a).await.unwrap(), b"aaa");
        let disabled = CachedBlockStore::new(backing, 0, 2);
        disabled.get(&a).await.unwrap();
        assert_eq!(disabled.stats().await.resident_entries, 0);
    }
    #[tokio::test]
    async fn clones_share_cache_but_distinct_instances_do_not() {
        let backing = MemoryBlockStore::new();
        let id = backing.put(b"read-through").await.unwrap();
        let cache = CachedBlockStore::new(backing.clone(), 100, 10);
        assert_eq!(cache.get(&id).await.unwrap(), b"read-through");
        assert_eq!(cache.clone().get(&id).await.unwrap(), b"read-through");
        assert_eq!(cache.stats().await.misses, 1);
        assert_eq!(cache.stats().await.hits, 1);
        let independent = CachedBlockStore::new(backing, 100, 10);
        independent.get(&id).await.unwrap();
        assert_eq!(independent.stats().await.misses, 1);
        cache.clone().clear().await;
        assert_eq!(cache.stats().await.resident_entries, 0);
        assert_eq!(independent.stats().await.resident_entries, 1);
    }

    struct PendingDelete {
        inner: MemoryBlockStore,
        deleted: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl BlockStore for PendingDelete {
        fn durable(&self) -> bool {
            false
        }
        async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
            self.inner.put(bytes).await
        }
        async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
            self.inner.get(id).await
        }
        async fn flush(&self) -> Result<()> {
            self.inner.flush().await
        }
        async fn delete(&self, id: &BlockId) -> Result<()> {
            self.inner.delete(id).await?;
            self.deleted.notify_one();
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn cancelled_delete_cannot_leave_stale_cache_hit() {
        let deleted = Arc::new(tokio::sync::Notify::new());
        let cache = CachedBlockStore::new(
            PendingDelete {
                inner: MemoryBlockStore::new(),
                deleted: deleted.clone(),
            },
            100,
            10,
        );
        let id = cache.put(b"cancel-delete").await.unwrap();
        let task_cache = cache.clone();
        let task_id = id.clone();
        let task = tokio::spawn(async move { task_cache.delete(&task_id).await });
        deleted.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(cache.get(&id).await.unwrap_err().code, ErrorCode::Enoent);
        assert_eq!(cache.stats().await.resident_entries, 0);
    }
}
