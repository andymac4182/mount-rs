//! Warm, single-volume disk-cache diagnostic; emits CSV, not production acceptance.
use async_trait::async_trait;
use mount_rs_core::storage::{BlockId, BlockStore};
use mount_rs_core::{FsError, Result};
use mount_rs_host::{DiskCacheBudget, DiskCachedBlockStore};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct Backing {
    bytes: Vec<u8>,
    gets: AtomicU64,
    puts: AtomicU64,
}

#[async_trait]
impl BlockStore for Backing {
    fn durable(&self) -> bool {
        false
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        assert_eq!(bytes, self.bytes);
        self.puts.fetch_add(1, Ordering::Relaxed);
        Ok(BlockId("fixture".to_owned()))
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        assert_eq!(id.0, "fixture");
        self.gets.fetch_add(1, Ordering::Relaxed);
        Ok(self.bytes.clone())
    }
    async fn flush(&self) -> Result<()> {
        Ok(())
    }
    async fn delete(&self, _: &BlockId) -> Result<()> {
        Err(FsError::enotsup("read-only benchmark fixture"))
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let mixed = match std::env::var("DISK_AUDIT_MIXED").as_deref() {
        Ok("1") => true,
        Err(std::env::VarError::NotPresent) | Ok("0") => false,
        _ => panic!("DISK_AUDIT_MIXED must be 0 or 1"),
    };
    let sizes = match std::env::var("DISK_AUDIT_SIZE").as_deref() {
        Ok("4096") => vec![4096],
        Ok("1048576") => vec![1024 * 1024],
        Err(std::env::VarError::NotPresent) => vec![4096, 1024 * 1024],
        _ => panic!("DISK_AUDIT_SIZE must be 4096 or 1048576"),
    };
    println!(
        "bytes,clients,workers,repeat,count,p50_ms,p99_ms,elapsed_ms,lock_wait_mean_ms,lock_hold_mean_ms,probe_queue_mean_ms,backing_gets,mixed,write_count,write_p99_ms"
    );
    for size in sizes {
        for clients in [1, 5, 10, 25, 50, 100] {
            // ABBA worker admission diagnostic, not an old/new implementation comparison.
            for (repeat, workers) in [1, 4, 4, 1].into_iter().enumerate() {
                let root = tempfile::tempdir().unwrap();
                let backing = Arc::new(Backing {
                    bytes: vec![0xa5; size],
                    gets: AtomicU64::new(0),
                    puts: AtomicU64::new(0),
                });
                let cache = Arc::new(
                    DiskCachedBlockStore::open_with_budget(
                        backing.clone(),
                        root.path(),
                        "diagnostic",
                        (size * 2 + 160) as u64,
                        2,
                        DiskCacheBudget::new(workers, 4).unwrap(),
                    )
                    .await
                    .unwrap(),
                );
                let id = BlockId("fixture".to_owned());
                assert_eq!(cache.get(&id).await.unwrap(), backing.bytes);
                assert_eq!(cache.get(&id).await.unwrap(), backing.bytes);
                let before = cache.stats().await.unwrap();
                let barrier = Arc::new(tokio::sync::Barrier::new(clients));
                let mut tasks = tokio::task::JoinSet::new();
                let started = Instant::now();
                for _ in 0..clients {
                    let cache = cache.clone();
                    let barrier = barrier.clone();
                    let id = id.clone();
                    let payload = backing.bytes.clone();
                    tasks.spawn(async move {
                        let mut times = Vec::with_capacity(100);
                        let mut writes = Vec::with_capacity(100);
                        for _ in 0..100 {
                            barrier.wait().await;
                            let read = async {
                                let start = Instant::now();
                                let bytes = cache.get(&id).await.unwrap();
                                (bytes, start.elapsed().as_secs_f64() * 1000.0)
                            };
                            let (bytes, elapsed) = if mixed {
                                let write = async {
                                    let start = Instant::now();
                                    assert_eq!(cache.put(&payload).await.unwrap(), id);
                                    start.elapsed().as_secs_f64() * 1000.0
                                };
                                let (read, elapsed) = tokio::join!(read, write);
                                writes.push(elapsed);
                                read
                            } else {
                                read.await
                            };
                            times.push(elapsed);
                            assert_eq!(bytes.len(), size);
                            assert!(bytes.iter().all(|byte| *byte == 0xa5));
                        }
                        (times, writes)
                    });
                }
                let mut times = Vec::new();
                let mut writes = Vec::new();
                while let Some(result) = tasks.join_next().await {
                    let (read, write) = result.unwrap();
                    times.extend(read);
                    writes.extend(write);
                }
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                let after = cache.stats().await.unwrap();
                let count = times.len();
                assert_eq!(count, clients * 100);
                assert_eq!(after.hits - before.hits, count as u64);
                assert_eq!(after.misses, before.misses);
                assert_eq!(after.io_failures, 0);
                assert!(!after.disabled);
                let gets = backing.gets.load(Ordering::Relaxed);
                assert_eq!(gets, 1, "timed reads must never reach backing storage");
                let write_count = writes.len();
                assert_eq!(write_count, if mixed { count } else { 0 });
                assert_eq!(backing.puts.load(Ordering::Relaxed), write_count as u64);
                assert_eq!(
                    after.fill_attempts - before.fill_attempts,
                    write_count as u64
                );
                writes.sort_by(f64::total_cmp);
                let write_p99 = if writes.is_empty() {
                    0.0
                } else {
                    writes[(write_count * 99).div_ceil(100) - 1]
                };
                times.sort_by(f64::total_cmp);
                let mean = |value: u64| value as f64 / count as f64 / 1_000_000.0;
                println!(
                    "{size},{clients},{workers},{repeat},{count},{:.6},{:.6},{elapsed:.6},{:.6},{:.6},{:.6},{gets},{mixed},{write_count},{write_p99:.6}",
                    times[(count * 50).div_ceil(100) - 1],
                    times[(count * 99).div_ceil(100) - 1],
                    mean(after.lock_wait_nanoseconds - before.lock_wait_nanoseconds),
                    mean(after.lock_hold_nanoseconds - before.lock_hold_nanoseconds),
                    mean(after.read_queue_nanoseconds - before.read_queue_nanoseconds),
                );
            }
        }
    }
}
