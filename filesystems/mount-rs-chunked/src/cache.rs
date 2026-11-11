//! Bounded process-local cache for immutable blocks.
use async_trait::async_trait;
use mount_rs_core::Result;
use mount_rs_core::drain_gate::DrainGate;
use mount_rs_core::storage::{BlockId, BlockStore};
use mount_rs_core::turn_gate::TurnGate;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
/// every put acknowledged before it. Deletion and clearing wait for no backing
/// I/O and stall none: a miss or a put that overlaps a deletion of a block in
/// the same stripe is simply not cached (see `Front::deletes`), which is what
/// prevents stale repopulation. Hits do not wait for backing I/O, and they
/// share the cache index: a hit records its recency in the entry itself, so
/// concurrent hits never exclude each other. The LRU order is re-linked lazily
/// by the writers that need it (insertion and eviction).
/// Use one wrapper per volume, shared by Arc.
/// This is RAM caching, not a persistent disk cache.
pub struct CachedBlockStore<B: ?Sized> {
    backing: Arc<B>,
    max_bytes: usize,
    max_entries: usize,
    index: Arc<Index>,
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
            index: self.index.clone(),
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

/// One slot of the fill table: an in-flight backing read that concurrent
/// misses for a block share.
#[derive(Default)]
struct Fill {
    /// Hash of the block the slot's current sharers are reading.
    key: AtomicU64,
    /// Held exclusively by whichever miss is reading the backing store; the
    /// others queue here and find the block cached when their turn comes.
    turn: TurnGate,
    /// Misses currently using this slot. Zero means the slot is free.
    sharers: std::sync::atomic::AtomicUsize,
}

/// In-flight fills, found without a lock: a fixed table of slots probed
/// from the block's hash. Joining or leaving a fill is one atomic update of
/// the slot's count and allocates nothing. A miss never touches the cache
/// index to register itself, so cold reads do not stall hits on other blocks.
///
/// Sharing a fill is an optimisation, not an invariant. A miss that finds
/// its probe window full reads the backing store unshared, and two misses
/// racing a completing fill may each read once. A slot is matched by hash
/// and can change hands between a miss's look and its join, so two misses
/// for different blocks can end up taking turns in one slot; each still
/// looks for, and reads, its own block.
struct Fills {
    slots: Box<[Fill]>,
}

impl Fills {
    fn new() -> Self {
        Self {
            slots: (0..FILL_SLOTS).map(|_| Fill::default()).collect(),
        }
    }

    fn key(id: &BlockId) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        id.0.hash(&mut hasher);
        hasher.finish()
    }

    fn window(&self, key: u64) -> impl Iterator<Item = &Fill> {
        let start = key as usize;
        (0..FILL_PROBE).map(move |step| &self.slots[start.wrapping_add(step) % FILL_SLOTS])
    }

    fn join(&self, id: &BlockId) -> Option<FillRef<'_>> {
        let key = Self::key(id);
        // A fill for this block that still has sharers.
        for fill in self.window(key) {
            if fill.key.load(Ordering::Acquire) != key {
                continue;
            }
            let mut sharers = fill.sharers.load(Ordering::Acquire);
            while sharers != 0 {
                match fill.sharers.compare_exchange_weak(
                    sharers,
                    sharers + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return Some(FillRef { fill }),
                    Err(current) => sharers = current,
                }
            }
        }
        // Otherwise a free slot.
        for fill in self.window(key) {
            if fill
                .sharers
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                fill.key.store(key, Ordering::Release);
                return Some(FillRef { fill });
            }
        }
        None
    }

    #[cfg(test)]
    fn sharers(&self, id: &BlockId) -> usize {
        let key = Self::key(id);
        self.window(key)
            .find(|fill| fill.key.load(Ordering::Acquire) == key)
            .map_or(0, |fill| fill.sharers.load(Ordering::Acquire))
    }
}

/// One miss's share of a fill. The last sharer to leave frees the slot; a
/// cancelled leader leaves it in use for the misses still queued behind it.
struct FillRef<'a> {
    fill: &'a Fill,
}

impl Drop for FillRef<'_> {
    fn drop(&mut self) {
        self.fill.sharers.fetch_sub(1, Ordering::AcqRel);
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

const FRONT_PROBE: usize = 8;

/// Stripes of the delete generation; see `Front::deletes`.
const DELETE_STRIPES: usize = 64;

/// A copy of every stripe's generation, taken by a put, which does not know
/// its block's id (and so its stripe) until the backing store has answered.
type Generations = [u64; DELETE_STRIPES];

fn tail_hash(id: &BlockId) -> u64 {
    // Ids are digests or counters: their tail varies the most.
    let bytes = id.0.as_bytes();
    let tail = &bytes[bytes.len().saturating_sub(16)..];
    let hash = tail.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    hash ^ (hash >> 29)
}

/// Cached blocks, findable without the index lock: a fixed table of atomic
/// slots probed from the block id's hash. A hit loads one slot, compares the
/// id and copies out of the block it guards; it takes no lock and no
/// reference count, so hits on different cores share nothing they write.
///
/// Every cached block is published here, so a lookup that finds nothing in
/// its probe window is a miss and never consults the index. The index keeps
/// the bounds and the eviction order; whoever has the index's turn publishes
/// each entry here and retracts it before releasing its payload. When a
/// block's probe window is full, the index evicts the least recently used
/// block in that window to make room.
///
/// The counters are here for the same reason: `stats` reads them without
/// taking the index's turn.
struct Front {
    slots: Box<[arc_swap::ArcSwapOption<Hot>]>,
    /// The index's current sequence, for hits to stamp recency with.
    epoch: AtomicU64,
    /// Delete generations, striped by block id. This is what keeps a deleted
    /// block out of the cache without excluding backing I/O.
    ///
    /// Written only by whoever has the index's turn: a delete advances its
    /// block's stripe once before it calls the backing store and once after
    /// the backing store has answered (or the delete was cancelled), removing
    /// the block from the index in the same turn each time. A miss or a put
    /// reads the generation before it calls the backing store, and the entry
    /// it queues carries that reading; the index files the entry only if the
    /// stripe still has that generation.
    ///
    /// Filing and both advances happen inside the turn, so they are totally
    /// ordered. An entry whose reading predates a delete's second advance is
    /// either filed before that advance, and removed by it, or filed after
    /// it, and dropped as stale. An entry whose reading follows the second
    /// advance comes from a backing call that began after the backing delete
    /// had answered. So once a delete has returned, the cache holds nothing
    /// that was read or written before the backing store deleted the block.
    deletes: Box<[AtomicU64]>,
    misses: AtomicU64,
    /// Written by the index after each turn.
    retired_hits: AtomicU64,
    evictions: AtomicU64,
    resident_bytes: AtomicU64,
    resident_entries: AtomicU64,
    backing_gets_completed: AtomicU64,
    backing_gets_failed: AtomicU64,
    backing_read_bytes: AtomicU64,
    backing_get_nanoseconds: AtomicU64,
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
            deletes: (0..DELETE_STRIPES).map(|_| AtomicU64::new(0)).collect(),
            misses: AtomicU64::new(0),
            retired_hits: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            resident_bytes: AtomicU64::new(0),
            resident_entries: AtomicU64::new(0),
            backing_gets_completed: AtomicU64::new(0),
            backing_gets_failed: AtomicU64::new(0),
            backing_read_bytes: AtomicU64::new(0),
            backing_get_nanoseconds: AtomicU64::new(0),
        }
    }

    fn stats(&self) -> BlockCacheStats {
        // Retired hits first: an entry retired during the scan is then left
        // out for this reading, not counted twice.
        let retired_hits = self.retired_hits.load(Ordering::Relaxed);
        let resident_hits: u64 = self
            .slots
            .iter()
            .filter_map(|slot| {
                slot.load()
                    .as_deref()
                    .map(|hot| hot.hits.load(Ordering::Relaxed))
            })
            .sum();
        BlockCacheStats {
            hits: retired_hits.saturating_add(resident_hits),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            resident_bytes: self.resident_bytes.load(Ordering::Relaxed) as usize,
            resident_entries: self.resident_entries.load(Ordering::Relaxed) as usize,
            backing_gets_completed: self.backing_gets_completed.load(Ordering::Relaxed),
            backing_gets_failed: self.backing_gets_failed.load(Ordering::Relaxed),
            backing_read_bytes: self.backing_read_bytes.load(Ordering::Relaxed),
            backing_get_nanoseconds: self.backing_get_nanoseconds.load(Ordering::Relaxed),
        }
    }

    fn window(&self, id: &BlockId) -> impl Iterator<Item = &arc_swap::ArcSwapOption<Hot>> {
        let start = tail_hash(id) as usize;
        let mask = self.slots.len() - 1;
        (0..FRONT_PROBE).map(move |step| &self.slots[start.wrapping_add(step) & mask])
    }

    fn stripe(id: &BlockId) -> usize {
        // The high bits: the low ones choose the probe window.
        (tail_hash(id) >> 32) as usize % DELETE_STRIPES
    }

    /// The block's delete generation, to be read before the backing call
    /// whose result will be queued for the index.
    fn generation(&self, id: &BlockId) -> u64 {
        self.deletes[Self::stripe(id)].load(Ordering::SeqCst)
    }

    /// Every stripe's generation, for a put: read before the backing call.
    fn generations(&self) -> Generations {
        std::array::from_fn(|stripe| self.deletes[stripe].load(Ordering::SeqCst))
    }

    /// Runs `read` on the cached block, if it is published here.
    #[inline]
    fn hit<R>(&self, id: &BlockId, count_hit: bool, read: impl FnOnce(&[u8]) -> R) -> Option<R> {
        for slot in self.window(id) {
            let hot = slot.load();
            if let Some(hot) = hot.as_deref()
                && hot.id == *id
            {
                hot.touch(self.epoch.load(Ordering::Relaxed) | 1, count_hit);
                return Some(read(&hot.bytes));
            }
        }
        None
    }

    /// Index only. When the block's probe window has no free slot, returns
    /// the least recently used block in it, which the index evicts.
    fn crowded(&self, id: &BlockId) -> Option<BlockId> {
        let mut oldest: Option<(u64, BlockId)> = None;
        for slot in self.window(id) {
            let hot = slot.load();
            let hot = hot.as_deref()?;
            let used = hot.used.load(Ordering::Relaxed);
            if oldest.as_ref().is_none_or(|(least, _)| used < *least) {
                oldest = Some((used, hot.id.clone()));
            }
        }
        oldest.map(|(_, id)| id)
    }

    /// Index only, after `crowded` returned `None` for this block.
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
    /// Hits of retired entries, evictions and residency; the rest of the
    /// statistics are counted in `front`.
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
    /// One phase of a delete: makes every entry for this block's stripe
    /// that is still on its way to the index stale, and removes the block.
    /// See `Front::deletes`.
    fn invalidate(&mut self, id: &BlockId) -> Option<Arc<Vec<u8>>> {
        self.front.deletes[Front::stripe(id)].fetch_add(1, Ordering::SeqCst);
        self.remove(id)
    }
    /// Empties the cache and makes every entry still on its way stale.
    fn invalidate_all(&mut self) -> (Vec<Arc<Vec<u8>>>, Recency) {
        for stripe in self.front.deletes.iter() {
            stripe.fetch_add(1, Ordering::SeqCst);
        }
        self.clear()
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
    /// Makes this turn's totals visible to `Front::stats`.
    fn publish_counts(&self) {
        let front = &self.front;
        front.retired_hits.store(self.stats.hits, Ordering::Relaxed);
        front
            .evictions
            .store(self.stats.evictions, Ordering::Relaxed);
        front
            .resident_bytes
            .store(self.stats.resident_bytes as u64, Ordering::Relaxed);
        front
            .resident_entries
            .store(self.stats.resident_entries as u64, Ordering::Relaxed);
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
        while let Some(crowding) = self.front.crowded(&id) {
            let Some(evicted) = self.remove(&crowding) else {
                break;
            };
            retired.push(evicted);
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
/// The index and the entries waiting to be filed in it.
///
/// Nothing waits for the index. One thread at a time has its turn, taken
/// with a single compare-and-swap. A put or a completed miss queues its
/// entry and tries for the turn; if another thread has it, that thread files
/// the entry before it leaves (it looks at the queue again after giving the
/// turn up, so an entry queued at the last moment is not stranded). Lookups
/// and statistics read `Front` and never take the turn. A delete or a clear
/// needs the index changed before it returns, so it alone waits for the
/// turn, yielding until the thread that has it (which files a bounded number
/// of entries) gives it up.
const SETTLE_LIMIT: usize = 4;

struct Index {
    busy: std::sync::atomic::AtomicBool,
    state: std::cell::UnsafeCell<State>,
    arriving: crossbeam_queue::SegQueue<(BlockId, Arc<Vec<u8>>, u64)>,
    max_bytes: usize,
    max_entries: usize,
}

// SAFETY: `state` is only reached through `Turn`, and `busy` admits one
// `Turn` at a time; `State` itself is `Send`.
unsafe impl Sync for Index {}

/// The index, for the one thread whose turn it is.
struct Turn<'a>(&'a Index);

impl std::ops::Deref for Turn<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        // SAFETY: this `Turn` is the only one; see `Index::try_turn`.
        unsafe { &*self.0.state.get() }
    }
}

impl std::ops::DerefMut for Turn<'_> {
    fn deref_mut(&mut self) -> &mut State {
        // SAFETY: as in `deref`, and `&mut self` excludes other borrows.
        unsafe { &mut *self.0.state.get() }
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.publish_counts();
        self.0.busy.store(false, Ordering::Release);
    }
}

impl Index {
    fn try_turn(&self) -> Option<Turn<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Turn(self))
    }

    /// Files up to `limit` queued entries. Returns how many it filed and
    /// what the caller should free once it has given the turn up.
    fn file_arrivals(&self, state: &mut State, limit: usize) -> (usize, Vec<Arc<Vec<u8>>>) {
        let mut retired = Vec::new();
        let mut filed = 0;
        while filed < limit
            && let Some((id, bytes, generation)) = self.arriving.pop()
        {
            // The stripe is only written inside the turn, which this is.
            if state.front.generation(&id) == generation {
                retired.extend(state.insert(id, bytes, self.max_bytes, self.max_entries));
            } else {
                // A delete began or ended since the backing call behind
                // this entry started: it may hold a deleted block.
                retired.push(bytes);
            }
            filed += 1;
        }
        (filed, retired)
    }

    /// Files queued entries unless another thread has the turn, in which
    /// case that thread does it.
    ///
    /// One call files at most `SETTLE_LIMIT` entries, so a thread is not
    /// kept filing for as long as others keep queueing. Entries it leaves
    /// behind are filed by the next arrival, which is why they are there.
    fn settle(&self) {
        while !self.arriving.is_empty() {
            let Some(mut state) = self.try_turn() else {
                return;
            };
            let (filed, retired) = self.file_arrivals(&mut state, SETTLE_LIMIT);
            drop(state);
            drop(retired);
            if filed == SETTLE_LIMIT {
                return;
            }
        }
    }

    /// `generation` is the block's delete generation as it was before the
    /// backing call that produced `bytes` began.
    fn insert(&self, id: BlockId, bytes: Arc<Vec<u8>>, generation: u64) {
        self.arriving.push((id, bytes, generation));
        self.settle();
    }

    /// Runs `work` on the index with every queued entry filed first. The
    /// turn may be taken by a put or a miss filing entries; that is a few
    /// insertions, so this yields until it is free.
    fn exclusive<R>(&self, work: impl FnOnce(&mut State) -> R) -> R {
        let mut state = loop {
            if let Some(state) = self.try_turn() {
                break state;
            }
            std::thread::yield_now();
        };
        let (_, retired) = self.file_arrivals(&mut state, usize::MAX);
        let result = work(&mut state);
        drop(state);
        drop(retired);
        self.settle();
        result
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
            index: Arc::new(Index {
                busy: std::sync::atomic::AtomicBool::new(false),
                state: std::cell::UnsafeCell::new(State {
                    front: Arc::clone(&front),
                    ..State::default()
                }),
                arriving: crossbeam_queue::SegQueue::new(),
                max_bytes,
                max_entries,
            }),
            front,
            fills: Arc::new(Fills::new()),
            write_gate: Arc::new(DrainGate::new()),
        }
    }
    fn prepare_entry(&self, bytes: &[u8]) -> Option<Arc<Vec<u8>>> {
        (self.max_entries != 0 && self.max_bytes != 0 && bytes.len() <= self.max_bytes)
            .then(|| Arc::new(bytes.to_vec()))
    }
    pub async fn stats(&self) -> BlockCacheStats {
        self.front.stats()
    }
    /// Runs `read` on a cached block. Neither a hit nor a miss touches the
    /// index.
    #[inline]
    fn lookup<R>(&self, id: &BlockId, read: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let result = self.front.hit(id, true, read);
        if result.is_none() {
            self.front.misses.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
    /// Miss path: fetch from the backing store, sharing one fill per block.
    async fn fill(&self, id: &BlockId) -> Result<Vec<u8>> {
        // Share one fill per block. Cancellation releases the turn so a
        // waiter can retry the provider.
        let fill = if self.max_bytes != 0 && self.max_entries != 0 {
            self.fills.join(id)
        } else {
            None
        };
        let _turn = match &fill {
            Some(share) => Some(share.fill.turn.lock().await),
            None => None,
        };
        // Another miss or put may have populated this entry while we waited.
        if let Some(bytes) = self.front.hit(id, false, <[u8]>::to_vec) {
            return Ok(bytes);
        }
        // Before the backing call: see `Front::deletes`.
        let generation = self.front.generation(id);
        let started = std::time::Instant::now();
        let result = self.backing.get(id).await;
        let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let cached = result
            .as_ref()
            .ok()
            .and_then(|bytes| self.prepare_entry(bytes));
        let read = result.as_ref().map(|bytes| bytes.len() as u64).ok();
        let front = &self.front;
        front.backing_gets_completed.fetch_add(1, Ordering::Relaxed);
        front
            .backing_get_nanoseconds
            .fetch_add(elapsed, Ordering::Relaxed);
        match read {
            Some(read) => front.backing_read_bytes.fetch_add(read, Ordering::Relaxed),
            None => front.backing_gets_failed.fetch_add(1, Ordering::Relaxed),
        };
        // Never waits, like a put. The fill's turn is still held, so the
        // misses sharing it look for the block only after it is queued.
        if let Some(cached) = cached {
            self.index.insert(id.clone(), cached, generation);
        }
        result
    }
    /// Drops only cached copies; authoritative backing data is untouched.
    pub async fn clear(&self) {
        // A miss or a put in flight is not cached when it completes.
        let retired = self.index.exclusive(State::invalidate_all);
        // Free potentially large buffers and tree nodes after giving the
        // index's turn up.
        drop(retired);
    }
}
#[async_trait]
impl<B: BlockStore + ?Sized> BlockStore for CachedBlockStore<B> {
    fn durable(&self) -> bool {
        self.backing.durable()
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let _write = self.write_gate.read().await;
        // Before the backing call: see `Front::deletes`. The id, and so the
        // stripe, is not known until the backing store answers.
        let generations = self.front.generations();
        let id = self.backing.put(bytes).await?;
        // The durability-ordering gate protects backing put/flush, not cache
        // allocation or publication.
        drop(_write);
        // Allocate/copy before taking the shared cache-state lock.
        if let Some(cached) = self.prepare_entry(bytes) {
            // Never waits: if another thread is in the index, it files this
            // entry on its way out.
            self.index
                .insert(id.clone(), cached, generations[Front::stripe(&id)]);
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
        let _write = self.write_gate.write().await;
        self.backing.flush().await
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        /// The second phase, which must run however the backing call ends,
        /// cancellation included.
        struct Finish<'a>(&'a Index, &'a BlockId);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                drop(self.0.exclusive(|state| state.invalidate(self.1)));
            }
        }
        // Evict before awaiting, so hits stop as the delete starts. Entries
        // already on their way to the index become stale.
        drop(self.index.exclusive(|state| state.invalidate(id)));
        let finish = Finish(&self.index, id);
        let result = self.backing.delete(id).await;
        // Whatever was read or written while the backing store still had
        // the block is removed or made stale here, before this returns.
        drop(finish);
        result
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
        assert_eq!(cache.front.hit(&first, true, <[u8]>::len), Some(5));
        let mut bytes = [0; 3];
        cache.read_into(&second, 1, &mut bytes).await.unwrap();
        assert_eq!(&bytes, b"eco");
        assert_eq!(cache.stats().await.hits, 2);

        // Eviction and deletion retract a block before its payload is freed.
        let payload = cache
            .index
            .exclusive(|state| Arc::downgrade(&state.entries[&second].hot.bytes));
        let third = cache.put(b"third").await.unwrap();
        assert!(cache.front.hit(&first, false, <[u8]>::len).is_none());
        assert_eq!(cache.front.hit(&third, false, <[u8]>::len), Some(5));
        cache.delete(&second).await.unwrap();
        assert!(cache.front.hit(&second, false, <[u8]>::len).is_none());
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

    /// A backing store that can hold a get or a put after the inner store
    /// has answered it, so the answer is in flight while a delete runs.
    struct HeldAnswer {
        inner: MemoryBlockStore,
        hold: Arc<AtomicBool>,
        answered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl HeldAnswer {
        async fn held(&self) {
            if self.hold.load(Ordering::SeqCst) {
                self.answered.notify_one();
                self.release.notified().await;
            }
        }
    }
    #[async_trait]
    impl BlockStore for HeldAnswer {
        fn durable(&self) -> bool {
            false
        }
        async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
            let id = self.inner.put(bytes).await;
            self.held().await;
            id
        }
        async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
            let bytes = self.inner.get(id).await;
            self.held().await;
            bytes
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn delete(&self, id: &BlockId) -> Result<()> {
            self.inner.delete(id).await
        }
    }

    #[tokio::test]
    async fn answer_in_flight_across_a_delete_cannot_repopulate_the_deleted_entry() {
        use std::time::Duration;
        for put in [false, true] {
            let backing = MemoryBlockStore::new();
            let id = backing.put(b"cold").await.unwrap();
            let hold = Arc::new(AtomicBool::new(true));
            let answered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let cache = CachedBlockStore::new(
                HeldAnswer {
                    inner: backing,
                    hold: hold.clone(),
                    answered: answered.clone(),
                    release: release.clone(),
                },
                1024,
                16,
            );
            let worker = cache.clone();
            let key = id.clone();
            // The backing store answers with the block, and the answer is
            // still on its way to the cache when the delete runs.
            let racer = tokio::spawn(async move {
                if put {
                    assert_eq!(worker.put(b"cold").await.unwrap(), key);
                } else {
                    assert_eq!(worker.get(&key).await.unwrap(), b"cold");
                }
            });
            tokio::time::timeout(Duration::from_secs(1), answered.notified())
                .await
                .unwrap();
            // The delete waits for neither the miss nor the put.
            tokio::time::timeout(Duration::from_secs(1), cache.delete(&id))
                .await
                .expect("delete must not wait for backing I/O in flight")
                .unwrap();
            hold.store(false, Ordering::SeqCst);
            release.notify_one();
            racer.await.unwrap();
            assert!(cache.front.hit(&id, false, <[u8]>::len).is_none());
            assert_eq!(cache.get(&id).await.unwrap_err().code, ErrorCode::Enoent);
            assert_eq!(cache.stats().await.resident_entries, 0);
        }
    }

    #[tokio::test]
    async fn invalidation_does_not_wait_for_shared_fills() {
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
            // The leader is stalled in the backing store with the others
            // queued behind it; invalidation completes regardless.
            stall.store(false, Ordering::SeqCst);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                if delete {
                    cache.delete(&id).await.unwrap();
                } else {
                    cache.clear().await;
                }
            })
            .await
            .expect("invalidation must not wait for a fill");
            resume.notify_one();
            for reader in readers {
                let read = tokio::time::timeout(std::time::Duration::from_secs(2), reader)
                    .await
                    .unwrap()
                    .unwrap();
                if delete {
                    assert_eq!(read.unwrap_err().code, ErrorCode::Enoent);
                } else {
                    assert_eq!(read.unwrap(), b"cold");
                }
            }
            if delete {
                assert_eq!(cache.stats().await.resident_entries, 0);
                assert_eq!(cache.get(&id).await.unwrap_err().code, ErrorCode::Enoent);
            } else {
                // The fill that overlapped the clear was not cached; the
                // next one in the queue read again and was.
                assert_eq!(cache.stats().await.backing_gets_completed, 2);
                assert_eq!(cache.stats().await.resident_entries, 1);
                assert_eq!(cache.get(&id).await.unwrap(), b"cold");
                assert_eq!(cache.stats().await.backing_gets_completed, 2);
            }
        }
    }

    /// A backing store whose calls yield a varying number of times before
    /// and after the inner store answers, so answers stay in flight while
    /// other calls run.
    struct Jitter {
        inner: MemoryBlockStore,
        step: std::sync::atomic::AtomicU64,
    }
    impl Jitter {
        async fn pause(&self) {
            let step = self
                .step
                .fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
            for _ in 0..(step >> 61) {
                tokio::task::yield_now().await;
            }
        }
    }
    #[async_trait]
    impl BlockStore for Jitter {
        fn durable(&self) -> bool {
            false
        }
        async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
            self.pause().await;
            let id = self.inner.put(bytes).await;
            self.pause().await;
            id
        }
        async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
            self.pause().await;
            let bytes = self.inner.get(id).await;
            self.pause().await;
            bytes
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn delete(&self, id: &BlockId) -> Result<()> {
            self.pause().await;
            let deleted = self.inner.delete(id).await;
            self.pause().await;
            deleted
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fills_and_puts_racing_deletes_never_leave_a_deleted_block_cached() {
        let inner = MemoryBlockStore::new();
        let cache = CachedBlockStore::new(
            Jitter {
                inner: inner.clone(),
                step: std::sync::atomic::AtomicU64::new(1),
            },
            1 << 20,
            64,
        );
        for round in 0..400_u32 {
            let bytes = Arc::new(format!("round {round}").into_bytes());
            let id = inner.put(&bytes).await.unwrap();
            // Unrelated traffic, some of it in the same stripe over the run.
            let other = cache
                .put(format!("other {round}").as_bytes())
                .await
                .unwrap();

            let mut early = Vec::new();
            for task in 0..6 {
                let (cache, id, bytes, other) =
                    (cache.clone(), id.clone(), bytes.clone(), other.clone());
                early.push(tokio::spawn(async move {
                    match task {
                        0 | 1 => assert_eq!(cache.put(&bytes).await.unwrap(), id),
                        2 => {
                            // Not found, or deleted before, are both fine.
                            let _ = cache.delete(&id).await;
                        }
                        3 => {
                            let _ = cache.delete(&other).await;
                        }
                        _ => {
                            if let Ok(read) = cache.get(&id).await {
                                assert_eq!(read, *bytes);
                            }
                        }
                    }
                }));
            }
            // Reads that are still in flight when the last delete runs.
            let mut readers = Vec::new();
            for _ in 0..6 {
                let (cache, id, bytes) = (cache.clone(), id.clone(), bytes.clone());
                readers.push(tokio::spawn(async move {
                    for _ in 0..3 {
                        if let Ok(read) = cache.get(&id).await {
                            assert_eq!(read, *bytes);
                        }
                    }
                }));
            }
            // Every put has returned before the last delete starts.
            for task in early {
                task.await.unwrap();
            }
            let _ = cache.delete(&id).await;
            assert!(
                cache.front.hit(&id, false, <[u8]>::len).is_none(),
                "round {round}: served after delete returned"
            );
            for reader in readers {
                reader.await.unwrap();
            }
            // With everything queued for the index filed.
            assert!(
                !cache
                    .index
                    .exclusive(|state| state.entries.contains_key(&id)),
                "round {round}: cached after delete returned"
            );
            assert!(cache.front.hit(&id, false, <[u8]>::len).is_none());
            assert_eq!(cache.get(&id).await.unwrap_err().code, ErrorCode::Enoent);
        }
        let stats = cache.stats().await;
        assert!(stats.resident_entries <= 64);
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

#[cfg(test)]
mod miss_profile {
    use super::*;
    use mount_rs_memory::MemoryBlockStore;

    /// Diagnostic: eight threads reading blocks that are almost never
    /// cached, each from its own random sequence.
    #[test]
    #[ignore = "diagnostic; prints timings"]
    fn profile_concurrent_misses() {
        let cache = Arc::new(CachedBlockStore::new(MemoryBlockStore::new(), 1 << 20, 64));
        let ids: Vec<BlockId> = (0..4096_u32)
            .map(|index| {
                futures_lite::future::block_on(cache.put(&index.to_le_bytes().repeat(256))).unwrap()
            })
            .collect();
        let ids = Arc::new(ids);
        let started = std::time::Instant::now();
        let threads: Vec<_> = (0..8_u64)
            .map(|thread| {
                let (cache, ids) = (cache.clone(), ids.clone());
                std::thread::spawn(move || {
                    let mut times = Vec::with_capacity(200_000);
                    let mut buffer = [0_u8; 64];
                    let mut random = 0x9e37_79b9_7f4a_7c15_u64.wrapping_mul(thread + 1);
                    for _ in 0..200_000 {
                        random ^= random << 13;
                        random ^= random >> 7;
                        random ^= random << 17;
                        let id = &ids[random as usize % ids.len()];
                        let began = std::time::Instant::now();
                        futures_lite::future::block_on(cache.read_into(id, 0, &mut buffer))
                            .unwrap();
                        times.push(began.elapsed());
                    }
                    times
                })
            })
            .collect();
        let mut times: Vec<_> = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect();
        let elapsed = started.elapsed();
        times.sort_unstable();
        let at = |fraction: f64| times[((times.len() - 1) as f64 * fraction) as usize];
        let stats = futures_lite::future::block_on(cache.stats());
        eprintln!(
            "MISSES {:.0} reads/s p50 {:?} p99 {:?} p99.9 {:?} max {:?}, hits {} misses {} resident {}",
            times.len() as f64 / elapsed.as_secs_f64(),
            at(0.5),
            at(0.99),
            at(0.999),
            at(1.0),
            stats.hits,
            stats.misses,
            stats.resident_entries
        );
        assert!(stats.resident_entries <= 64);
    }
}
