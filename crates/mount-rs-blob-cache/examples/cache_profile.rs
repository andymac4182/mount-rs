//! Isolated 4KiB RAM-hit microbenchmark; excludes RPC, metadata and real datastore costs.
use async_trait::async_trait;
use mount_rs_blob_cache::*;
use mount_rs_core::{
    Result,
    storage::{BlockId, BlockStore, ConcurrentBackingId},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
struct Counter;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(n as u64, Ordering::Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOCATOR: Counter = Counter;
struct Backing {
    bytes: Vec<u8>,
    gets: AtomicU64,
}
#[async_trait]
impl BlockStore for Backing {
    fn durable(&self) -> bool {
        true
    }
    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        ConcurrentBackingId::from_bytes([1; 16])
    }
    async fn verify_concurrent_backing(&self, _: ConcurrentBackingId) -> Result<()> {
        Ok(())
    }
    async fn put(&self, _: &[u8]) -> Result<BlockId> {
        Ok(BlockId("opaque".into()))
    }
    async fn get(&self, _: &BlockId) -> Result<Vec<u8>> {
        self.gets.fetch_add(1, Ordering::Relaxed);
        Ok(self.bytes.clone())
    }
    async fn flush(&self) -> Result<()> {
        Ok(())
    }
    async fn delete(&self, _: &BlockId) -> Result<()> {
        Ok(())
    }
}
async fn measure(name: &str, store: &dyn BlockStore, id: &BlockId, n: u64) {
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    let started = Instant::now();
    for _ in 0..n {
        let b = store.get(id).await.unwrap();
        std::hint::black_box(b);
    }
    let elapsed = started.elapsed();
    ENABLED.store(false, Ordering::Relaxed);
    println!(
        "CACHE_PROFILE name={name} reads={n} elapsed_us={} reads_per_second={:.0} allocs_per_read={:.3} allocated_bytes_per_read={:.3}",
        elapsed.as_micros(),
        n as f64 / elapsed.as_secs_f64(),
        ALLOCS.load(Ordering::Relaxed) as f64 / n as f64,
        BYTES.load(Ordering::Relaxed) as f64 / n as f64
    );
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let dir = tempfile::tempdir().unwrap();
    let backing = Arc::new(Backing {
        bytes: vec![42; 4096],
        gets: AtomicU64::new(0),
    });
    let local = LocalCache::new(LocalCacheConfig {
        directory: dir.path().join("cache"),
        memory_bytes: 4 * 1024 * 1024,
        disk_bytes: 16 * 1024 * 1024,
        max_entries: 1000,
        max_blob_bytes: 4096,
    })
    .unwrap();
    let cache = CachedBlockStore::new(
        backing.clone(),
        local.clone(),
        ScopeIdentity {
            cluster: "bench".into(),
            partition: "p".into(),
            drive: "d".into(),
        },
        IntegrityPolicy::Opaque,
    );
    cache.prepare_concurrent_backing().await.unwrap();
    let id = BlockId("opaque".into());
    cache.get(&id).await.unwrap();
    local.shutdown().await;
    measure("uncached_memory_provider", backing.as_ref(), &id, 100_000).await;
    let before = backing.gets.load(Ordering::Relaxed);
    measure("warm_ram_cache", &cache, &id, 100_000).await;
    println!(
        "CACHE_PROFILE warm_backing_gets={} cache_hits={}",
        backing.gets.load(Ordering::Relaxed) - before,
        cache.metrics().snapshot().local_hits
    );
}
