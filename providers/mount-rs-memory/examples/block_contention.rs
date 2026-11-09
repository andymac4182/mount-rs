//! Same-binary ABBA comparison with the recorded pre-change block-store code.
#[path = "support/mutex_baseline.rs"]
mod baseline;

use mount_rs_core::storage::BlockStore;
use std::future::Future;
use std::sync::{Arc, Barrier};
use std::task::{Context, Poll, Waker};
use std::time::Instant;

// Both in-memory implementations complete without suspension. Fail if that
// assumption changes; do not busy-poll an asynchronous provider.
fn ready<T>(future: impl Future<Output = T>) -> T {
    match std::pin::pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("in-memory benchmark operation suspended"),
    }
}

fn main() {
    println!("operation,clients,implementation,repeat,count,p50_ms,p99_ms,elapsed_ms");
    for operation in ["get", "duplicate-put", "new-put"] {
        for clients in [1, 5, 10, 25, 50, 100] {
            for (repeat, candidate) in [false, true, true, false].into_iter().enumerate() {
                let store: Arc<dyn BlockStore> = if candidate {
                    Arc::new(mount_rs_memory::MemoryBlockStore::new())
                } else {
                    Arc::new(baseline::MemoryBlockStore::new())
                };
                let payload = vec![0xa5; 4096];
                let id = ready(store.put(&payload)).unwrap();
                assert_eq!(ready(store.get(&id)).unwrap(), payload);
                let barrier = Arc::new(Barrier::new(clients + 1));
                let mut tasks = Vec::new();
                for client in 0..clients {
                    let store = store.clone();
                    let id = id.clone();
                    let mut payload = payload.clone();
                    let barrier = barrier.clone();
                    tasks.push(std::thread::spawn(move || {
                        let mut times = Vec::with_capacity(100);
                        let mut published = Vec::new();
                        for iteration in 0..100_u64 {
                            if operation == "new-put" {
                                payload[..8].copy_from_slice(&(client as u64).to_le_bytes());
                                payload[8..16].copy_from_slice(&iteration.to_le_bytes());
                            }
                            barrier.wait();
                            let start = Instant::now();
                            if operation == "get" {
                                let bytes = ready(store.get(&id)).unwrap();
                                times.push(start.elapsed().as_secs_f64() * 1000.0);
                                assert_eq!(bytes, payload);
                            } else {
                                let actual = ready(store.put(&payload)).unwrap();
                                times.push(start.elapsed().as_secs_f64() * 1000.0);
                                if operation == "duplicate-put" {
                                    assert_eq!(actual, id);
                                } else {
                                    published.push((actual, iteration));
                                }
                            }
                        }
                        barrier.wait();
                        // Verify new publications outside the timed phase.
                        for (id, iteration) in published {
                            payload[8..16].copy_from_slice(&iteration.to_le_bytes());
                            assert_eq!(ready(store.get(&id)).unwrap(), payload);
                        }
                        times
                    }));
                }
                let start = Instant::now();
                for _ in 0..=100 {
                    barrier.wait();
                }
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                let mut times = Vec::new();
                for task in tasks {
                    times.extend(task.join().unwrap());
                }
                assert_eq!(times.len(), clients * 100);
                times.sort_by(f64::total_cmp);
                let count = times.len();
                println!(
                    "{operation},{clients},{},{repeat},{count},{:.6},{:.6},{elapsed:.6}",
                    if candidate { "shared" } else { "mutex" },
                    times[(count * 50).div_ceil(100) - 1],
                    times[(count * 99).div_ceil(100) - 1],
                );
            }
        }
    }
}
