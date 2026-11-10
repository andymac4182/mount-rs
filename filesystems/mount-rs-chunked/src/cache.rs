//! Bounded process-local cache for immutable blocks.
use async_trait::async_trait;
use mount_rs_core::Result;
use mount_rs_core::storage::{BlockId, BlockStore};
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use tokio::sync::{Mutex, RwLock};

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
/// Cache misses can overlap immutable puts and flushes. Puts and flushes
/// serialize with each other; deletion and clearing exclude all backing I/O
/// to prevent stale repopulation. Hits do not wait for backing I/O.
/// Use one wrapper per volume, shared by Arc.
/// This is RAM caching, not a persistent disk cache.
pub struct CachedBlockStore<B: ?Sized> {
    backing: Arc<B>,
    max_bytes: usize,
    max_entries: usize,
    state: Arc<Mutex<State>>,
    io_gate: Arc<RwLock<()>>,
    write_gate: Arc<Mutex<()>>,
}

impl<B: ?Sized> Clone for CachedBlockStore<B> {
    fn clone(&self) -> Self {
        Self {
            backing: self.backing.clone(),
            max_bytes: self.max_bytes,
            max_entries: self.max_entries,
            state: self.state.clone(),
            io_gate: self.io_gate.clone(),
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

#[derive(Default)]
struct State {
    fills: BTreeMap<BlockId, Weak<Mutex<()>>>,
    entries: BTreeMap<BlockId, (Arc<Vec<u8>>, u64)>,
    recency: BTreeMap<u64, BlockId>,
    sequence: u64,
    stats: BlockCacheStats,
}
impl State {
    fn remove(&mut self, id: &BlockId) -> Option<Arc<Vec<u8>>> {
        if let Some((bytes, sequence)) = self.entries.remove(id) {
            self.recency.remove(&sequence);
            self.stats.resident_bytes -= bytes.len();
            self.stats.resident_entries -= 1;
            return Some(bytes);
        }
        None
    }
    fn clear(&mut self) {
        self.entries.clear();
        self.recency.clear();
        self.stats.resident_bytes = 0;
        self.stats.resident_entries = 0;
        self.sequence = 0;
    }
    fn next_sequence(&mut self) -> u64 {
        if self.sequence == u64::MAX {
            self.clear();
        }
        self.sequence += 1;
        self.sequence
    }
    fn get(&mut self, id: &BlockId, count_hit: bool) -> Option<Arc<Vec<u8>>> {
        // Allocate the timestamp first, so rollover cannot invalidate a borrow.
        let sequence = self.next_sequence();
        let (bytes, old) = self.entries.get_mut(id)?;
        self.recency.remove(old);
        *old = sequence;
        self.recency.insert(sequence, id.clone());
        if count_hit {
            self.stats.hits = self.stats.hits.saturating_add(1);
        }
        Some(bytes.clone())
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
        let sequence = self.next_sequence();
        let mut retired = Vec::new();
        retired.extend(self.remove(&id));
        while self.stats.resident_entries >= max_entries
            || self.stats.resident_bytes > max_bytes - bytes.len()
        {
            let Some((_, oldest)) = self.recency.first_key_value() else {
                break;
            };
            let oldest = oldest.clone();
            retired.extend(self.remove(&oldest));
            self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
        self.stats.resident_bytes += bytes.len();
        self.stats.resident_entries += 1;
        self.recency.insert(sequence, id.clone());
        self.entries.insert(id, (bytes, sequence));
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
        Self {
            backing,
            max_bytes,
            max_entries,
            state: Arc::new(Mutex::new(State::default())),
            io_gate: Arc::new(RwLock::new(())),
            write_gate: Arc::new(Mutex::new(())),
        }
    }
    fn prepare_entry(&self, bytes: &[u8]) -> Option<Arc<Vec<u8>>> {
        (self.max_entries != 0 && self.max_bytes != 0 && bytes.len() <= self.max_bytes)
            .then(|| Arc::new(bytes.to_vec()))
    }
    pub async fn stats(&self) -> BlockCacheStats {
        self.state.lock().await.stats
    }
    async fn lookup(&self, id: &BlockId) -> Option<Arc<Vec<u8>>> {
        let mut state = self.state.lock().await;
        let cached = state.get(id, true);
        if cached.is_none() {
            state.stats.misses = state.stats.misses.saturating_add(1);
        }
        cached
    }
    /// Miss path: fetch from the backing store, sharing one fill per block.
    async fn fill(&self, id: &BlockId) -> Result<Vec<u8>> {
        let _io = self.io_gate.read().await;
        // Share one fill per block. Weak entries cannot keep abandoned tasks
        // alive; each miss prunes keys whose fills have completed.
        // Cancellation releases the mutex so a waiter can retry the provider.
        let fill = if self.max_bytes != 0 && self.max_entries != 0 {
            let mut state = self.state.lock().await;
            state.fills.retain(|_, fill| fill.strong_count() != 0);
            if let Some(fill) = state.fills.get(id).and_then(Weak::upgrade) {
                Some(fill)
            } else {
                let fill = Arc::new(Mutex::new(()));
                state.fills.insert(id.clone(), Arc::downgrade(&fill));
                Some(fill)
            }
        } else {
            None
        };
        let _fill = match &fill {
            Some(fill) => Some(fill.lock().await),
            None => None,
        };
        // Another miss or put may have populated this entry while we waited.
        let cached = { self.state.lock().await.get(id, false) };
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
        let mut state = self.state.lock().await;
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
        let retired = {
            let mut state = self.state.lock().await;
            let retired = (
                std::mem::take(&mut state.entries),
                std::mem::take(&mut state.recency),
            );
            state.clear();
            retired
        };
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
        let _write = self.write_gate.lock().await;
        let id = self.backing.put(bytes).await?;
        // The durability-ordering gate protects backing put/flush, not cache
        // allocation or publication. Deletion remains excluded by the I/O gate.
        drop(_write);
        // Allocate/copy before taking the shared cache-state lock.
        if let Some(cached) = self.prepare_entry(bytes) {
            let retired = self.state.lock().await.insert(
                id.clone(),
                cached,
                self.max_bytes,
                self.max_entries,
            );
            drop(retired);
        }
        Ok(id)
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        // Clone only the shared pointer under the cache lock. Potentially large
        // caller-owned byte copies must not serialize other cache hits.
        if let Some(bytes) = self.lookup(id).await {
            return Ok(bytes.as_ref().clone());
        }
        self.fill(id).await
    }
    async fn read_into(&self, id: &BlockId, offset: usize, buffer: &mut [u8]) -> Result<()> {
        // A hit copies only the requested range out of the shared block.
        if let Some(bytes) = self.lookup(id).await {
            return mount_rs_core::storage::copy_block_range(&bytes, offset, buffer);
        }
        let bytes = self.fill(id).await?;
        mount_rs_core::storage::copy_block_range(&bytes, offset, buffer)
    }
    async fn flush(&self) -> Result<()> {
        let _io = self.io_gate.read().await;
        let _write = self.write_gate.lock().await;
        self.backing.flush().await
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        let _io = self.io_gate.write().await;
        // No miss/put can repopulate while this operation owns the I/O gate.
        // Evict before awaiting, including for cancellation/unknown outcomes.
        let retired = self.state.lock().await.remove(id);
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
        let old = Arc::downgrade(&state.entries[&second].0);
        let retired = state.insert(second.clone(), Arc::new(vec![3; 4]), 16, 1);
        assert_eq!(state.stats.resident_bytes, 4);
        assert_eq!(state.stats.evictions, 1);
        assert!(old.upgrade().is_some());
        drop(retired);
        assert!(old.upgrade().is_none());
        let old = Arc::downgrade(&state.entries[&second].0);
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
                    if cache
                        .state
                        .lock()
                        .await
                        .fills
                        .get(&id)
                        .is_some_and(|fill| fill.strong_count() == 20)
                    {
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
