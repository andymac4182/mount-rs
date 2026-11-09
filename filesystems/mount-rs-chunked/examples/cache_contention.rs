//! Shared-cache hit contention; excludes providers, network and filesystem work.
use mount_rs_chunked::CachedBlockStore;
use mount_rs_core::storage::BlockStore;
use mount_rs_memory::MemoryBlockStore;
use std::{sync::Arc, time::Instant};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("clients,block_bytes,reads,p50_us,p95_us,p99_us,max_us,elapsed_ms,backing_gets");
    for size in [4096, 65536, 1048576] {
        for clients in [1, 5, 10, 25, 50, 100] {
            let cache = Arc::new(CachedBlockStore::new(MemoryBlockStore::new(), size * 2, 2));
            let expected = Arc::new(vec![73; size]);
            let id = cache.put(&expected).await?;
            let barrier = Arc::new(tokio::sync::Barrier::new(clients));
            let mut tasks = tokio::task::JoinSet::new();
            let started = Instant::now();
            for _ in 0..clients {
                let (cache, expected, id, barrier) =
                    (cache.clone(), expected.clone(), id.clone(), barrier.clone());
                tasks.spawn(async move {
                    let mut samples = Vec::with_capacity(1000);
                    for _ in 0..1000 {
                        barrier.wait().await;
                        let begin = Instant::now();
                        let actual = cache.get(&id).await.unwrap();
                        let elapsed = begin.elapsed().as_secs_f64() * 1e6;
                        assert_eq!(actual.as_slice(), expected.as_slice());
                        samples.push(elapsed);
                    }
                    samples
                });
            }
            let mut samples = Vec::new();
            while let Some(result) = tasks.join_next().await {
                samples.extend(result?);
            }
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            let stats = cache.stats().await;
            assert_eq!(stats.hits, (clients * 1000) as u64);
            assert_eq!(stats.backing_gets_completed, 0);
            samples.sort_by(f64::total_cmp);
            let n = samples.len();
            println!(
                "{clients},{size},{n},{:.3},{:.3},{:.3},{:.3},{elapsed:.3},{}",
                samples[(n * 50).div_ceil(100) - 1],
                samples[(n * 95).div_ceil(100) - 1],
                samples[(n * 99).div_ceil(100) - 1],
                samples[n - 1],
                stats.backing_gets_completed
            );
        }
    }
    Ok(())
}
