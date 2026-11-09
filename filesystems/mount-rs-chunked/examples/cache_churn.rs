//! RAM cache reads during bounded working-set churn, including memory backing.
//! This measures cache/provider contention, not network or metadata latency.
use mount_rs_chunked::CachedBlockStore;
use mount_rs_core::storage::BlockStore;
use mount_rs_memory::MemoryBlockStore;
use std::{sync::Arc, time::Instant};

const SAMPLES: usize = 200;
const CAPACITY: usize = 8;

fn percentile(samples: &[f64], percent: usize) -> f64 {
    samples[(samples.len() * percent).div_ceil(100) - 1]
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "mode,clients,block_bytes,reads,writes,read_p50_us,read_p95_us,read_p99_us,read_max_us,write_p99_us,elapsed_ms,hits,misses,evictions,backing_gets,resident_bytes,resident_entries"
    );
    for size in [4096, 65536, 1048576] {
        for clients in [1, 5, 10, 25, 50, 100] {
            for (mode, working_set) in [("resident", 4), ("churn", 16)] {
                let backing = MemoryBlockStore::new();
                let mut payloads = Vec::new();
                let mut ids = Vec::new();
                for index in 0..working_set {
                    let payload = vec![u8::try_from(index)?; size];
                    ids.push(backing.put(&payload).await?);
                    payloads.push(payload);
                }
                let cache = Arc::new(CachedBlockStore::new(backing, size * CAPACITY, CAPACITY));
                for id in &ids {
                    cache.get(id).await?;
                }
                let before = cache.stats().await;
                let ids = Arc::new(ids);
                let payloads = Arc::new(payloads);
                // One writer joins each burst; every reader has exactly one
                // operation per burst. Barrier waiting is outside latency.
                let barrier = Arc::new(tokio::sync::Barrier::new(clients + 1));
                let mut tasks = tokio::task::JoinSet::new();
                for client in 0..clients {
                    let (cache, ids, payloads, barrier) = (
                        cache.clone(),
                        ids.clone(),
                        payloads.clone(),
                        barrier.clone(),
                    );
                    tasks.spawn(async move {
                        let mut samples = Vec::with_capacity(SAMPLES);
                        for round in 0..SAMPLES {
                            let index = (client + round) % ids.len();
                            barrier.wait().await;
                            let start = Instant::now();
                            let bytes = cache.get(&ids[index]).await.unwrap();
                            samples.push(start.elapsed().as_secs_f64() * 1e6);
                            assert_eq!(bytes, payloads[index]);
                        }
                        samples
                    });
                }
                let start = Instant::now();
                let mut writes = Vec::with_capacity(SAMPLES);
                for round in 0..SAMPLES {
                    let index = (round + working_set / 2) % working_set;
                    barrier.wait().await;
                    let begin = Instant::now();
                    let id = cache.put(&payloads[index]).await?;
                    writes.push(begin.elapsed().as_secs_f64() * 1e6);
                    assert_eq!(id, ids[index]);
                }
                let mut reads = Vec::with_capacity(clients * SAMPLES);
                while let Some(task) = tasks.join_next().await {
                    reads.extend(task?);
                }
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                let after = cache.stats().await;
                let hits = after.hits - before.hits;
                let misses = after.misses - before.misses;
                let evictions = after.evictions - before.evictions;
                let gets = after.backing_gets_completed - before.backing_gets_completed;
                assert_eq!(hits + misses, reads.len() as u64);
                assert_eq!(after.backing_gets_failed, 0);
                assert!(after.resident_bytes <= size * CAPACITY);
                assert!(after.resident_entries <= CAPACITY);
                if mode == "resident" {
                    assert_eq!(misses, 0);
                    assert_eq!(gets, 0);
                    assert_eq!(evictions, 0);
                } else {
                    assert!(evictions > 0);
                    assert!(gets > 0);
                }
                reads.sort_by(f64::total_cmp);
                writes.sort_by(f64::total_cmp);
                println!(
                    "{mode},{clients},{size},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{elapsed:.3},{hits},{misses},{evictions},{gets},{},{}",
                    reads.len(),
                    writes.len(),
                    percentile(&reads, 50),
                    percentile(&reads, 95),
                    percentile(&reads, 99),
                    reads.last().unwrap(),
                    percentile(&writes, 99),
                    after.resident_bytes,
                    after.resident_entries
                );
            }
        }
    }
    Ok(())
}
